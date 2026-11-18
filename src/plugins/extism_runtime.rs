use super::{MAX_RESPONSE_BYTES, PLUGIN_TIMEOUT, PluginResult, read_file};
use anyhow::{Context, Result, ensure};
use extism::{CompiledPlugin, Manifest, Plugin, PluginBuilder, Wasm};
use std::{collections::BTreeMap, path::Path, sync::Mutex};

const MAX_WASM_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MEMORY_PAGES: u32 = 1024; // 64 MiB of WebAssembly linear memory.
const FUEL_LIMIT: u64 = 100_000_000;

pub(super) struct WasmPlugin {
    bytes: Vec<u8>,
    function: String,
    wasi: bool,
    config: BTreeMap<String, String>,
    compiled: Mutex<Option<CompiledPlugin>>,
}

impl WasmPlugin {
    pub(super) fn load(
        path: &Path,
        function: String,
        wasi: bool,
        config: BTreeMap<String, String>,
    ) -> Result<Self> {
        ensure!(
            !function.is_empty()
                && function.len() <= 128
                && !function.chars().any(char::is_control),
            "Extism function 须为 1 至 128 字节的导出函数名"
        );
        let bytes = read_file(path, MAX_WASM_BYTES)
            .with_context(|| format!("无法读取 WebAssembly 插件 {}", path.display()))?;
        ensure!(
            bytes.starts_with(b"\0asm\x01\0\0\0"),
            "插件须为 WebAssembly 1.0 二进制模块"
        );
        ensure!(
            serde_json::to_vec(&config)?.len() <= MAX_RESPONSE_BYTES as usize,
            "插件 config 超过大小限制"
        );
        Ok(Self {
            bytes,
            function,
            wasi,
            config,
            compiled: Mutex::new(None),
        })
    }

    fn compiled(&self) -> Result<CompiledPlugin> {
        let mut cached = self
            .compiled
            .lock()
            .map_err(|_| anyhow::anyhow!("插件编译缓存不可用"))?;
        if let Some(compiled) = cached.as_ref() {
            return Ok(compiled.clone());
        }

        // Read local Wasm bytes ourselves. Extism manifests, URLs, remote module
        // registration, host paths and host imports are never supplied by plugins.
        let mut manifest = Manifest::new([Wasm::data(self.bytes.clone())])
            .disallow_all_hosts()
            .with_memory_max(MAX_MEMORY_PAGES)
            .with_timeout(PLUGIN_TIMEOUT);
        manifest.allowed_paths = Some(BTreeMap::new());
        manifest.memory.max_var_bytes = Some(MAX_RESPONSE_BYTES);
        manifest.memory.max_http_response_bytes = Some(MAX_RESPONSE_BYTES);
        manifest.config = self.config.clone();
        let compiled = PluginBuilder::new(manifest)
            .with_wasi(self.wasi)
            .with_fuel_limit(FUEL_LIMIT)
            .with_cache_disabled()
            .compile()
            .context("WebAssembly 插件编译失败")?;
        *cached = Some(compiled.clone());
        Ok(compiled)
    }

    pub(super) fn run(&self, input: &str) -> Result<PluginResult> {
        // Retain compiled machine code, but use a new instance for each command.
        // A failed call cannot leave guest globals, variables or memory behind.
        let mut plugin =
            Plugin::new_from_compiled(&self.compiled()?).context("无法创建 Extism 插件实例")?;
        ensure!(
            plugin.function_exists(&self.function),
            "插件没有导出函数「{}」",
            self.function
        );
        let payload = serde_json::to_vec(&serde_json::json!({ "version": 1, "input": input }))?;
        let output: &[u8] = plugin
            .call(&self.function, payload.as_slice())
            .context("Extism 函数执行失败（执行上限 10 秒、1 亿 fuel）")?;
        ensure!(
            output.len() as u64 <= MAX_RESPONSE_BYTES,
            "插件输出超过 1 MiB 限制"
        );
        serde_json::from_slice(output)
            .context("插件输出须为 UTF-8 JSON，包含 title、detail 和 output")
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::fs;

    // Exercise the real Extism memory ABI without depending on a Wasm toolchain
    // being installed on the host that runs cargo test.
    pub(crate) fn response_wasm(response: &[u8]) -> Vec<u8> {
        checked_response_wasm(response, None)
    }

    fn checked_response_wasm(response: &[u8], expected_input: Option<&[u8]>) -> Vec<u8> {
        let stores = response
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                format!(
                    "(call $store (i64.add (local.get $offset) (i64.const {i})) (i32.const {byte}))"
                )
            })
            .collect::<String>();
        let checks = expected_input.map_or(String::new(), |input| {
            let mut checks = format!("(if (i64.ne (call $input_length) (i64.const {})) (then unreachable))", input.len());
            for (i, byte) in input.iter().enumerate() {
                checks.push_str(&format!("(if (i32.ne (call $input_byte (i64.const {i})) (i32.const {byte})) (then unreachable))"));
            }
            checks
        });
        wat::parse_str(format!(
            r#"(module
            (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
            (import "extism:host/env" "store_u8" (func $store (param i64 i32)))
            (import "extism:host/env" "output_set" (func $output (param i64 i64)))
            (import "extism:host/env" "input_length" (func $input_length (result i64)))
            (import "extism:host/env" "input_load_u8" (func $input_byte (param i64) (result i32)))
            (func (export "run") (result i32) (local $offset i64)
                {checks}
                (local.set $offset (call $alloc (i64.const {})))
                {stores}
                (call $output (local.get $offset) (i64.const {}))
                (i32.const 0)))"#,
            response.len(),
            response.len()
        ))
        .unwrap()
    }

    #[test]
    fn runs_bundled_rust_pdk_example_with_config_and_unicode() {
        let (_directory, mut plugin) = load(include_bytes!(
            "../../examples/extism/moin_extism_hello.wasm"
        ));
        plugin.config.insert("greeting".into(), "欢迎".into());
        assert_eq!(plugin.run("世界 🌏").unwrap().output, "欢迎，世界 🌏！");
        assert_eq!(plugin.run("").unwrap().output, "欢迎，世界！");
    }

    #[test]
    fn passes_versioned_unicode_json_to_guest() {
        let input = "你好 \"Extism\"\n🦀";
        let payload =
            serde_json::to_vec(&serde_json::json!({ "version": 1, "input": input })).unwrap();
        let bytes = checked_response_wasm(
            br#"{"title":"Protocol","detail":"Extism","output":"valid input"}"#,
            Some(&payload),
        );
        let (_directory, plugin) = load(&bytes);
        assert_eq!(plugin.run(input).unwrap().output, "valid input");
        assert!(plugin.run("different input").is_err());
    }

    fn load(bytes: &[u8]) -> (tempfile::TempDir, WasmPlugin) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.wasm");
        fs::write(&path, bytes).unwrap();
        let plugin = WasmPlugin::load(&path, "run".into(), false, BTreeMap::new()).unwrap();
        (directory, plugin)
    }

    #[test]
    fn runs_real_extism_wasm_and_rejects_missing_exports_and_invalid_output() {
        let bytes = response_wasm(br#"{"title":"Example","detail":"Extism","output":"portable"}"#);
        let (_directory, mut plugin) = load(&bytes);
        assert_eq!(plugin.run("你好").unwrap().output, "portable");
        assert_eq!(plugin.run("again").unwrap().output, "portable");
        assert!(plugin.compiled.lock().unwrap().is_some());
        plugin.function = "missing".into();
        assert!(
            plugin
                .run("input")
                .unwrap_err()
                .to_string()
                .contains("没有导出函数")
        );

        let (_directory, plugin) = load(&response_wasm(b"not JSON"));
        assert!(
            plugin
                .run("input")
                .unwrap_err()
                .to_string()
                .contains("UTF-8 JSON")
        );
    }

    #[test]
    fn bounds_guest_execution_and_memory() {
        let loop_wasm = wat::parse_str(
            r#"(module
            (func (export "run") (result i32) (loop $forever (br $forever)) (i32.const 0)))"#,
        )
        .unwrap();
        let (_directory, plugin) = load(&loop_wasm);
        let error = format!("{:#}", plugin.run("").unwrap_err());
        assert!(
            error.contains("fuel") || error.contains("timeout"),
            "{error}"
        );

        let constructor_wasm = wat::parse_str(
            r#"(module
            (func $initialize (loop $forever (br $forever)))
            (start $initialize)
            (func (export "run") (result i32) (i32.const 0)))"#,
        )
        .unwrap();
        let (_directory, plugin) = load(&constructor_wasm);
        let error = format!("{:#}", plugin.run("").unwrap_err());
        assert!(
            error.contains("fuel") || error.contains("timeout"),
            "{error}"
        );

        let memory_wasm = wat::parse_str(
            r#"(module
            (memory 2048)
            (func (export "run") (result i32) (i32.const 0)))"#,
        )
        .unwrap();
        let (_directory, plugin) = load(&memory_wasm);
        assert!(plugin.run("").is_err());
    }

    #[test]
    fn rejects_oversized_guest_output_before_deserializing() {
        let size = MAX_RESPONSE_BYTES + 1;
        let wasm = wat::parse_str(format!(
            r#"(module
            (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
            (import "extism:host/env" "output_set" (func $output (param i64 i64)))
            (func (export "run") (result i32)
                (call $output (call $alloc (i64.const {size})) (i64.const {size}))
                (i32.const 0)))"#
        ))
        .unwrap();
        let (_directory, plugin) = load(&wasm);
        assert!(plugin.run("").unwrap_err().to_string().contains("1 MiB"));
    }
}
