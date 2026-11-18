//! Commands run only after the user submits them.
//!
//! To install an executable plugin, put a JSON manifest in the app data folder
//! under `plugins/`, alongside the executable or with an absolute executable path.
//!
//! ```json
//! {"command":"hello","description":"问候","executable":"hello.exe","args":[]}
//! ```
//!
//! On each invocation, the executable receives one UTF-8 JSON line on stdin:
//! `{"version":1,"input":"世界"}`. It must exit successfully and write a UTF-8
//! JSON object to stdout: `{"title":"问候","detail":"Hello 插件","output":"你好，世界"}`.
//! On Windows, the executable must be an .exe. Plugins run with the app user's permissions.
//! The executable's directory is its working directory. Plugin output is limited to 1 MiB
//! and the executable has 10 seconds to finish.
//! Restart Moin to reload its manifests, dictionary, and translation settings.
//! Extism WebAssembly plugins use `runtime: "extism"` and `wasm: "hello.wasm"`.
//! See `docs/EXTISM.md` for the portable plugin protocol and development example.

mod extism_runtime;

use extism_runtime::WasmPlugin;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginResult {
    pub title: String,
    pub detail: String,
    pub output: String,
}

#[derive(Default)]
pub struct Plugins {
    dictionary: BTreeMap<String, String>,
    translation: Option<Translation>,
    external: BTreeMap<String, ExternalPlugin>,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct TranslationConfig {
    model: String,
    #[serde(default = "default_endpoint")]
    endpoint: String,
}

fn default_endpoint() -> String {
    "http://127.0.0.1:11434/api/generate".into()
}

struct Translation {
    config: TranslationConfig,
    agent: ureq::Agent,
}

struct ExternalPlugin {
    command: String,
    description: String,
    backend: PluginBackend,
}

enum PluginBackend {
    Executable {
        executable: PathBuf,
        args: Vec<String>,
    },
    Extism(WasmPlugin),
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PluginRuntime {
    #[default]
    Executable,
    #[serde(alias = "wasm")]
    Extism,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginManifest {
    command: String,
    description: String,
    #[serde(default)]
    runtime: PluginRuntime,
    executable: Option<PathBuf>,
    #[serde(default)]
    args: Vec<String>,
    wasm: Option<PathBuf>,
    function: Option<String>,
    #[serde(default)]
    wasi: bool,
    #[serde(default)]
    config: BTreeMap<String, String>,
}

enum Invocation<'a> {
    Calculate(&'a str),
    Dictionary(&'a str),
    Translate(&'a str),
    Convert(&'a str),
    External(&'a ExternalPlugin, &'a str),
}

impl Plugins {
    pub fn load(folder: &Path) -> Result<Self> {
        let mut plugins = Self::default();

        let dictionary = folder.join("dictionary.tsv");
        if let Some(content) = plugins.optional_file(&dictionary, |path| {
            String::from_utf8(read_file(path, 128 * 1024 * 1024)?).context("词典须使用 UTF-8 编码")
        }) {
            for line in content.trim_start_matches('\u{feff}').lines() {
                if let Some((word, definition)) = line.split_once('\t') {
                    let word = word.trim();
                    let definition = definition.trim();
                    if !word.is_empty() && !definition.is_empty() {
                        plugins
                            .dictionary
                            .insert(normalize(word), definition.replace(r"\n", "\n"));
                    }
                }
            }
        }

        plugins.translation = plugins.optional_file(&folder.join("translation.json"), |path| {
            Translation::new(read_json(path)?)
        });

        let directory = folder.join("plugins");
        let mut files = match fs::read_dir(&directory) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                })
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                plugins
                    .warnings
                    .push(format!("{}  ·  {error}", directory.display()));
                Vec::new()
            }
        };
        files.sort();
        for manifest in files {
            if let Err(error) = plugins.add_external(&directory, &manifest) {
                plugins
                    .warnings
                    .push(format!("{}  ·  {error:#}", manifest.display()));
            }
        }
        Ok(plugins)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn recognizes(&self, query: &str) -> bool {
        self.invocation(query).is_some()
    }

    pub fn run(&self, query: &str) -> Result<Option<PluginResult>> {
        let Some(invocation) = self.invocation(query) else {
            return Ok(None);
        };
        ensure!(query.len() <= MAX_COMMAND_BYTES, "命令内容过长");
        let result = match invocation {
            Invocation::Calculate(input) => calculate(input)?,
            Invocation::Dictionary(input) => self.lookup(input)?,
            Invocation::Translate(input) => self.translate(input)?,
            Invocation::Convert(input) => convert(input)?,
            Invocation::External(plugin, input) => plugin.run(input)?,
        };
        Ok(Some(result))
    }

    pub fn commands(&self) -> Vec<(String, String)> {
        let mut commands = vec![
            ("=".into(), "计算器 · 例如 = (18 + 6) / 3".into()),
            ("翻译".into(), "翻译 · 例如 翻译 hello".into()),
            ("词典".into(), "英汉词典 · 例如 词典 index".into()),
            ("换算".into(), "单位换算 · 例如 换算 12 km mi".into()),
        ];
        commands.extend(
            self.external
                .values()
                .map(|plugin| (plugin.command.clone(), plugin.description.clone())),
        );
        commands
    }

    fn invocation<'a>(&'a self, query: &'a str) -> Option<Invocation<'a>> {
        let query = query.trim();
        if let Some(expression) = query.strip_prefix('=') {
            return Some(Invocation::Calculate(expression.trim()));
        }

        let (command, input) = split_command(query);
        match command.to_ascii_lowercase().as_str() {
            "词典" | "dict" => Some(Invocation::Dictionary(input)),
            "翻译" | "tr" => Some(Invocation::Translate(input)),
            "换算" | "convert" => Some(Invocation::Convert(input)),
            command => self
                .external
                .get(command)
                .map(|plugin| Invocation::External(plugin, input)),
        }
    }

    fn optional_file<T>(
        &mut self,
        path: &Path,
        read: impl FnOnce(&Path) -> Result<T>,
    ) -> Option<T> {
        match path.try_exists() {
            Ok(false) => return None,
            Ok(true) => {}
            Err(error) => {
                self.warnings
                    .push(format!("{}  ·  {error}", path.display()));
                return None;
            }
        }
        match read(path) {
            Ok(value) => Some(value),
            Err(error) => {
                self.warnings
                    .push(format!("{}  ·  {error:#}", path.display()));
                None
            }
        }
    }

    fn add_external(&mut self, directory: &Path, manifest: &Path) -> Result<()> {
        let mut plugin: PluginManifest = read_json(manifest)?;
        plugin.command = plugin.command.trim().into();
        ensure!(
            !plugin.command.is_empty()
                && plugin.command.chars().count() <= 32
                && plugin
                    .command
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-'),
            "命令名须为 1 至 32 个字母、数字、连字符或下划线"
        );
        let key = plugin.command.to_ascii_lowercase();
        ensure!(
            self.invocation(&key).is_none(),
            "命令名重复：{}",
            plugin.command
        );
        ensure!(!plugin.description.trim().is_empty(), "缺少插件说明");
        let backend = match plugin.runtime {
            PluginRuntime::Executable => {
                ensure!(
                    plugin.wasm.is_none()
                        && plugin.function.is_none()
                        && !plugin.wasi
                        && plugin.config.is_empty(),
                    "可执行插件不能包含 wasm、function、wasi 或 config 设置；WebAssembly 插件请设置 runtime 为 extism"
                );
                let executable = directory.join(plugin.executable.context("缺少 executable 路径")?);
                ensure!(
                    executable.is_file(),
                    "找不到可执行文件 {}",
                    executable.display()
                );
                // Resolve before changing the child's working directory.
                let executable = executable.canonicalize()?;
                #[cfg(windows)]
                ensure!(
                    executable
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
                    "Windows 插件须指向 .exe 文件"
                );
                PluginBackend::Executable {
                    executable,
                    args: plugin.args,
                }
            }
            PluginRuntime::Extism => {
                ensure!(
                    plugin.executable.is_none() && plugin.args.is_empty(),
                    "Extism 插件不能包含 executable 或 args"
                );
                let wasm = directory.join(plugin.wasm.context("缺少 wasm 路径")?);
                PluginBackend::Extism(WasmPlugin::load(
                    &wasm,
                    plugin.function.unwrap_or_else(|| "run".into()),
                    plugin.wasi,
                    plugin.config,
                )?)
            }
        };
        self.external.insert(
            key,
            ExternalPlugin {
                command: plugin.command,
                description: plugin.description,
                backend,
            },
        );
        Ok(())
    }

    fn lookup(&self, input: &str) -> Result<PluginResult> {
        ensure!(!input.is_empty(), "请输入要查询的单词");
        let term = normalize(input);
        if let Some(definition) = self.dictionary.get(&term) {
            return Ok(result(input, "本地词典", definition));
        }
        if let Some((english, chinese, definition)) =
            vocabulary().find(|(english, chinese, _)| *english == term || *chinese == term)
        {
            return Ok(result(
                english,
                "内置英汉词典",
                format!("{chinese}\n{definition}"),
            ));
        }
        bail!("词典中没有「{input}」。可在应用数据目录添加 dictionary.tsv 扩充词条。")
    }

    fn translate(&self, input: &str) -> Result<PluginResult> {
        let (code, rest) = split_command(input);
        let (target, text) = match code.to_ascii_lowercase().as_str() {
            "zh" => ("简体中文", rest),
            "en" => ("英语", rest),
            "ja" => ("日语", rest),
            "ko" => ("韩语", rest),
            "fr" => ("法语", rest),
            "de" => ("德语", rest),
            "es" => ("西班牙语", rest),
            "ru" => ("俄语", rest),
            _ if input.chars().any(is_han) => ("英语", input),
            _ => ("简体中文", input),
        };
        ensure!(!text.is_empty(), "请输入要翻译的内容，例如「翻译 en 你好」");

        let source = normalize(text.trim_end_matches(['.', '!', '?', '。', '！', '？']));
        let translated = vocabulary().find_map(|(english, chinese, _)| match target {
            "简体中文" if english == source => Some(chinese),
            "英语" if chinese == source => Some(english),
            _ => None,
        });
        if let Some(output) = translated {
            return Ok(result(target, "离线常用短语", output));
        }
        if let Some(translation) = &self.translation {
            return Ok(result(
                target,
                "本地 Ollama",
                translation.generate(target, text)?,
            ));
        }
        bail!("离线翻译仅收录常用英汉短语。可通过 translation.json 启用本机 Ollama 翻译任意文本。")
    }
}

fn split_command(query: &str) -> (&str, &str) {
    query
        .split_once(char::is_whitespace)
        .map_or((query, ""), |(command, input)| (command, input.trim()))
}

fn result(
    title: impl Into<String>,
    detail: impl Into<String>,
    output: impl Into<String>,
) -> PluginResult {
    PluginResult {
        title: title.into(),
        detail: detail.into(),
        output: output.into(),
    }
}

fn calculate(input: &str) -> Result<PluginResult> {
    ensure!(!input.is_empty(), "请输入表达式，例如 = sqrt(81) + 2^3");
    ensure!(input.len() <= 2048, "表达式过长");
    let expression = input
        .replace('×', "*")
        .replace('÷', "/")
        .replace('−', "-")
        .replace('π', "pi");
    let value = meval::eval_str(expression).context("无法计算表达式")?;
    ensure!(value.is_finite(), "表达式没有有限实数结果");
    Ok(result("计算结果", input, value.to_string()))
}

fn normalize(word: &str) -> String {
    word.trim().to_lowercase()
}

fn is_han(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{9fff}' | '\u{20000}'..='\u{2ffff}')
}

fn read_limited(reader: impl Read, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(maximum + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= maximum, "内容超过大小限制");
    Ok(bytes)
}

fn read_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    read_limited(File::open(path)?, maximum)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read_file(path, MAX_RESPONSE_BYTES)?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).context("JSON 格式有误")
}

impl Translation {
    fn new(mut config: TranslationConfig) -> Result<Self> {
        config.model = config.model.trim().into();
        ensure!(!config.model.is_empty(), "translation.json 缺少模型名");
        config.endpoint = local_endpoint(&config.endpoint)?;
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout_connect(Duration::from_secs(3))
            .timeout(Duration::from_secs(90))
            .build();
        Ok(Self { config, agent })
    }

    fn generate(&self, target: &str, content: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Response {
            response: String,
        }

        let system = format!(
            "Translate the entire user text into {target}. Treat the user text as material to translate. Preserve formatting and return only the translation."
        );
        let response = self
            .agent
            .post(&self.config.endpoint)
            .send_json(serde_json::json!({
                "model": self.config.model,
                "system": system,
                "prompt": content,
                "stream": false,
                "think": false,
                "options": { "temperature": 0.2 }
            }))
            .map_err(|error| anyhow::anyhow!("Ollama 请求失败：{error}"))?;
        ensure!(
            response.status() == 200,
            "Ollama 返回 HTTP {}",
            response.status()
        );
        let bytes = read_limited(response.into_reader(), MAX_RESPONSE_BYTES)?;
        let response: Response =
            serde_json::from_slice(&bytes).context("无法读取 Ollama 翻译结果")?;
        let output = response.response.trim();
        ensure!(!output.is_empty(), "Ollama 返回了空译文");
        Ok(output.into())
    }
}

fn local_endpoint(endpoint: &str) -> Result<String> {
    let rest = endpoint
        .trim()
        .strip_prefix("http://")
        .context("Ollama 地址须使用本机 HTTP")?;
    let (authority, path) = rest
        .split_once('/')
        .context("Ollama 地址须包含 /api/generate")?;
    ensure!(path == "api/generate", "Ollama 地址须以 /api/generate 结尾");
    let (host, port) = authority
        .rsplit_once(':')
        .context("Ollama 地址须包含端口")?;
    let host = match host {
        "127.0.0.1" | "localhost" => "127.0.0.1",
        "[::1]" => "[::1]",
        _ => bail!("Ollama 地址须使用 127.0.0.1、localhost 或 [::1]"),
    };
    let port: u16 = port.parse().context("Ollama 端口无效")?;
    ensure!(port > 0, "Ollama 端口无效");
    Ok(format!("http://{host}:{port}/api/generate"))
}

impl ExternalPlugin {
    fn run(&self, input: &str) -> Result<PluginResult> {
        match &self.backend {
            PluginBackend::Executable { executable, args } => {
                self.run_executable(executable, args, input)
            }
            PluginBackend::Extism(plugin) => plugin
                .run(input)
                .with_context(|| format!("Extism 插件「{}」运行失败", self.command)),
        }
    }

    fn run_executable(
        &self,
        executable: &Path,
        args: &[String],
        input: &str,
    ) -> Result<PluginResult> {
        let mut payload = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "input": input
        }))?;
        payload.push(b'\n');

        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir(executable.parent().context("插件无工作目录")?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("无法启动插件「{}」", self.command))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");

        let (write_result, wrote) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let written = stdin.write_all(&payload);
            drop(stdin);
            let _ = write_result.send(written);
        });
        let (read_result, read) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = read_result.send(read_limited(stdout, MAX_RESPONSE_BYTES));
        });

        let deadline = Instant::now() + PLUGIN_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    ensure!(
                        status.success(),
                        "插件「{}」退出，状态 {status}",
                        self.command
                    );
                    break;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                running => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Err(error) = running {
                        return Err(error).context("无法获取插件运行状态");
                    }
                    bail!("插件「{}」运行超时", self.command);
                }
            }
        }
        let remaining = || deadline.saturating_duration_since(Instant::now());
        wrote
            .recv_timeout(remaining())
            .context("插件输入超时")?
            .context("无法写入插件输入")?;
        let output = read.recv_timeout(remaining()).context("插件输出超时")??;
        serde_json::from_slice(&output)
            .context("插件输出须为 UTF-8 JSON，包含 title、detail 和 output")
    }
}

#[derive(PartialEq)]
enum Dimension {
    Length,
    Mass,
    Time,
    Temperature,
    Volume,
    Speed,
    Data,
}

struct Unit {
    symbol: &'static str,
    dimension: Dimension,
    factor: f64,
    offset: f64,
}

fn unit(name: &str) -> Option<Unit> {
    use Dimension::*;
    let (symbol, dimension, factor, offset) = match name.to_lowercase().as_str() {
        "mm" | "毫米" => ("mm", Length, 0.001, 0.0),
        "cm" | "厘米" => ("cm", Length, 0.01, 0.0),
        "m" | "米" => ("m", Length, 1.0, 0.0),
        "km" | "千米" | "公里" => ("km", Length, 1000.0, 0.0),
        "in" | "英寸" => ("in", Length, 0.0254, 0.0),
        "ft" | "英尺" => ("ft", Length, 0.3048, 0.0),
        "yd" | "码" => ("yd", Length, 0.9144, 0.0),
        "mi" | "英里" => ("mi", Length, 1609.344, 0.0),
        "mg" | "毫克" => ("mg", Mass, 0.001, 0.0),
        "g" | "克" => ("g", Mass, 1.0, 0.0),
        "kg" | "千克" | "公斤" => ("kg", Mass, 1000.0, 0.0),
        "oz" | "盎司" => ("oz", Mass, 28.349523125, 0.0),
        "lb" | "磅" => ("lb", Mass, 453.59237, 0.0),
        "ms" | "毫秒" => ("ms", Time, 0.001, 0.0),
        "s" | "秒" => ("s", Time, 1.0, 0.0),
        "min" | "分钟" => ("min", Time, 60.0, 0.0),
        "h" | "小时" => ("h", Time, 3600.0, 0.0),
        "d" | "天" => ("d", Time, 86400.0, 0.0),
        "c" | "°c" | "℃" | "摄氏度" => ("°C", Temperature, 1.0, 0.0),
        "f" | "°f" | "℉" | "华氏度" => ("°F", Temperature, 5.0 / 9.0, -32.0),
        "k" | "开尔文" => ("K", Temperature, 1.0, -273.15),
        "ml" | "毫升" => ("mL", Volume, 0.001, 0.0),
        "l" | "升" => ("L", Volume, 1.0, 0.0),
        "gal" | "美制加仑" => ("US gal", Volume, 3.785411784, 0.0),
        "m/s" => ("m/s", Speed, 1.0, 0.0),
        "km/h" => ("km/h", Speed, 1.0 / 3.6, 0.0),
        "mph" => ("mph", Speed, 0.44704, 0.0),
        "b" | "字节" => ("B", Data, 1.0, 0.0),
        "kb" => ("kB", Data, 1000.0, 0.0),
        "mb" => ("MB", Data, 1000000.0, 0.0),
        "gb" => ("GB", Data, 1000000000.0, 0.0),
        "tb" => ("TB", Data, 1000000000000.0, 0.0),
        "kib" => ("KiB", Data, 1024.0, 0.0),
        "mib" => ("MiB", Data, 1048576.0, 0.0),
        "gib" => ("GiB", Data, 1073741824.0, 0.0),
        "tib" => ("TiB", Data, 1099511627776.0, 0.0),
        _ => return None,
    };
    Some(Unit {
        symbol,
        dimension,
        factor,
        offset,
    })
}

fn convert(input: &str) -> Result<PluginResult> {
    let parts = input.split_whitespace().collect::<Vec<_>>();
    let (value, from, to) = match parts.as_slice() {
        [value, from, to] | [value, from, "to" | "到", to] => (value, from, to),
        _ => bail!("请输入数值、原单位和目标单位，例如「换算 12 km mi」"),
    };
    let value = value.parse::<f64>().context("换算数值无效")?;
    ensure!(value.is_finite(), "请输入有限数值");
    let from = unit(from).with_context(|| format!("未识别的单位「{from}」"))?;
    let to = unit(to).with_context(|| format!("未识别的单位「{to}」"))?;
    ensure!(from.dimension == to.dimension, "两个单位须属于同一类别");

    let value = ((value + from.offset) * from.factor) / to.factor - to.offset;
    ensure!(value.is_finite(), "换算结果超出范围");
    let number = if value.abs() < 0.000001 && value != 0.0 || value.abs() >= 1e12 {
        format!("{value:.6e}")
    } else {
        format!("{value:.8}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .into()
    };
    Ok(result("换算结果", input, format!("{number} {}", to.symbol)))
}

fn vocabulary() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    VOCABULARY.lines().filter_map(|line| {
        let (english, rest) = line.split_once('\t')?;
        let (chinese, definition) = rest.split_once('\t')?;
        Some((english, chinese, definition))
    })
}

const VOCABULARY: &str = "hello\t你好\tA greeting used when meeting or speaking to someone.
hi\t嗨\tAn informal greeting.
good morning\t早上好\tA greeting used in the morning.
good afternoon\t下午好\tA greeting used in the afternoon.
good evening\t晚上好\tA greeting used in the evening.
good night\t晚安\tA farewell used before bed or late at night.
goodbye\t再见\tA farewell used when departing.
see you later\t回头见\tAn informal farewell.
thank you\t谢谢\tAn expression of gratitude.
thank you very much\t非常感谢\tAn emphatic expression of gratitude.
you're welcome\t不客气\tA polite response to thanks.
excuse me\t不好意思\tA polite way to interrupt or get attention.
i'm sorry\t对不起\tAn expression of apology.
no problem\t没问题\tAn expression of willingness or reassurance.
how are you\t你好吗\tA customary question about someone's well-being.
i don't understand\t我不明白\tA phrase used when something is unclear.
please speak slowly\t请说慢一点\tA request to speak at a slower pace.
how much is it\t多少钱\tA question about the price of something.
where is the station\t车站在哪里\tA question asking for the location of a station.
welcome\t欢迎\tA greeting for someone who has arrived.
help\t帮助\tTo make something easier for someone.
yes\t是的\tAn affirmative response.
no\t不\tA negative response.
today\t今天\tThe present day.
tomorrow\t明天\tThe day after today.
yesterday\t昨天\tThe day before today.
time\t时间\tThe ongoing sequence of events and moments.
water\t水\tThe liquid essential to life.
coffee\t咖啡\tA drink made from roasted coffee beans.
tea\t茶\tA drink made by infusing tea leaves in water.
food\t食物\tSomething eaten for nourishment.
train\t火车\tLinked railway vehicles that carry passengers or goods.
airport\t机场\tA place where aircraft take off and land.
hotel\t酒店\tA business providing rooms to travelers.
restaurant\t餐厅\tA place where customers buy and eat meals.
library\t图书馆\tA place or collection used to borrow or consult materials.
book\t书\tA written work published in printed or digital form.
work\t工作\tActivity involving effort, often as part of a job.
home\t家\tThe place where someone lives.
weather\t天气\tThe conditions of the atmosphere at a place and time.
computer\t计算机\tAn electronic device that stores and processes information.
application\t应用程序\tSoftware intended to carry out a task for the user.
launcher\t启动器\tSoftware that opens programs or other resources.
file\t文件\tA named collection of data stored on a device.
folder\t文件夹\tA container that organizes files and other folders.
document\t文档\tA written or electronic record.
search\t搜索\tTo look for a particular item or information.
index\t索引\tAn organized structure used to find information efficiently.
database\t数据库\tAn organized collection of data intended for structured access.
query\t查询\tA request to retrieve information from a source.
result\t结果\tAn outcome produced by an operation or action.
text\t文本\tWritten material, or its representation on a computer.
content\t内容\tThe information inside a document, page, or other medium.
source\t来源\tThe place or object from which something comes.
reference\t参考资料\tMaterial consulted for information.
local\t本地\tPresent on the current device or within the immediate system.
offline\t离线\tAvailable or operating without a network connection.
translate\t翻译\tTo express the meaning of text or speech in another language.
translation\t译文\tA rendering of speech or writing in another language.
dictionary\t词典\tA resource defining words or giving their equivalents in other languages.
calculator\t计算器\tA tool that performs mathematical operations.
plugin\t插件\tA component installed to extend a program's capabilities.
command\t命令\tAn instruction that directs a program or system to perform an action.
keyboard\t键盘\tAn input device with keys for text and commands.
shortcut\t快捷键\tA key or key combination that invokes an operation.
settings\t设置\tOptions used to configure the behavior of a program.
language\t语言\tA system used to communicate meaning.
english\t英语\tA West Germanic language widely spoken internationally.
chinese\t中文\tThe written language of Chinese, or the Chinese language family.
model\t模型\tAn abstract representation, or a system trained from data.
embedding\t嵌入向量\tA numerical representation of an item in a vector space.
semantic search\t语义搜索\tInformation retrieval based on meaning beyond exact word matches.
full-text search\t全文搜索\tRetrieval that searches through the text of documents.
retrieval\t检索\tThe process of locating and returning stored information.
retrieval-augmented generation\t检索增强生成\tAn approach that supplies retrieved material to a generative model.
terminal\t终端\tAn interface for entering commands and reading their output.
software\t软件\tPrograms and related instructions used by a computer.
rust\t锈\tA reddish-brown iron oxide. Capitalized Rust also names a systems programming language.
open\t打开\tTo make a file, application, or resource available for use.
save\t保存\tTo retain information by writing it to storage.
copy\t复制\tTo create a duplicate or put selected material on the clipboard.
paste\t粘贴\tTo insert material from the clipboard.
delete\t删除\tTo remove an item or piece of data.
error\t错误\tA condition in which an operation fails or produces an invalid result.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_whole_plugin_commands() {
        let plugins = Plugins::default();
        assert!(plugins.recognizes("  DICT\tindex"));
        assert!(plugins.recognizes("=2^3"));
        assert!(!plugins.recognizes("translate.rs"));
        assert!(!plugins.recognizes("词典.tsv"));
        assert!(plugins.run("word").unwrap().is_none());
    }

    #[test]
    fn calculates_and_rejects_non_finite_results() {
        assert_eq!(calculate("sqrt(81) + 2^3").unwrap().output, "17");
        assert_eq!(calculate("9 ÷ 3 × 4").unwrap().output, "12");
        assert!(calculate("1 / 0").is_err());
    }

    #[test]
    fn supports_offline_dictionary_and_bidirectional_phrases() {
        let plugins = Plugins::default();
        assert!(
            plugins
                .lookup("INDEX")
                .unwrap()
                .output
                .starts_with("索引\n")
        );
        assert_eq!(plugins.translate("hello!").unwrap().output, "你好");
        assert_eq!(plugins.translate("en 你好").unwrap().output, "hello");
        assert!(plugins.translate("fr hello").is_err());
    }

    #[test]
    fn converts_temperature_and_distinguishes_data_units() {
        assert_eq!(convert("100 °C to F").unwrap().output, "212 °F");
        assert_eq!(convert("1 GiB GB").unwrap().output, "1.07374182 GB");
        assert!(convert("1 kg m").is_err());
    }

    #[test]
    fn restricts_ollama_to_loopback_generate_address() {
        assert_eq!(
            local_endpoint("http://localhost:11434/api/generate").unwrap(),
            default_endpoint()
        );
        assert!(local_endpoint("http://[::1]:10240/api/generate").is_ok());
        assert!(local_endpoint("http://127.0.0.1:11434@example.org:80/api/generate").is_err());
        assert!(local_endpoint("http://192.0.2.42:11434/api/generate").is_err());
    }

    #[test]
    fn loads_dictionary_and_manifests_independently() {
        let folder = tempfile::tempdir().unwrap();
        fs::write(
            folder.path().join("dictionary.tsv"),
            "\u{feff}Rust\t一种编程语言\n",
        )
        .unwrap();
        let manifests = folder.path().join("plugins");
        fs::create_dir(&manifests).unwrap();
        fs::write(manifests.join("broken.json"), "{invalid JSON").unwrap();
        let manifest = serde_json::json!({
            "command": "Example",
            "description": "本地插件",
            "executable": std::env::current_exe().unwrap(),
        });
        fs::write(manifests.join("example.json"), manifest.to_string()).unwrap();

        let plugins = Plugins::load(folder.path()).unwrap();
        assert_eq!(
            plugins.run("词典 rust").unwrap().unwrap().output,
            "一种编程语言"
        );
        assert!(plugins.recognizes("example data"));
        assert_eq!(plugins.warnings().len(), 1);
    }

    #[test]
    fn discovers_extism_and_executable_plugins_together_with_unique_commands() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("plugins");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("hello.wasm"),
            extism_runtime::tests::response_wasm(
                br#"{"title":"Hello","detail":"Extism","output":"Hello from Wasm"}"#,
            ),
        )
        .unwrap();
        let wasm = serde_json::json!({
            "command": "Hello", "description": "跨平台插件", "runtime": "extism", "wasm": "hello.wasm"
        });
        fs::write(directory.join("a-hello.json"), wasm.to_string()).unwrap();
        fs::write(directory.join("b-executable.json"), serde_json::json!({
            "command": "local", "description": "可执行插件", "executable": std::env::current_exe().unwrap()
        }).to_string()).unwrap();
        fs::write(directory.join("c-duplicate.json"), serde_json::json!({
            "command": "HELLO", "description": "重复插件", "runtime": "extism", "wasm": "hello.wasm"
        }).to_string()).unwrap();
        fs::write(directory.join("d-reserved.json"), serde_json::json!({
            "command": "DICT", "description": "内置命令冲突", "runtime": "wasm", "wasm": "hello.wasm"
        }).to_string()).unwrap();

        let plugins = Plugins::load(folder.path()).unwrap();
        assert!(plugins.recognizes("HELLO 世界"));
        assert!(plugins.recognizes("local input"));
        assert_eq!(plugins.commands().len(), 6);
        assert_eq!(
            plugins.run("hello 世界").unwrap().unwrap().output,
            "Hello from Wasm"
        );
        assert_eq!(plugins.warnings().len(), 2);
        assert!(
            plugins
                .warnings()
                .iter()
                .all(|warning| warning.contains("命令名重复"))
        );
    }

    #[test]
    fn rejects_ambiguous_missing_and_invalid_wasm_manifests_independently() {
        let folder = tempfile::tempdir().unwrap();
        let directory = folder.path().join("plugins");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("invalid.wasm"), b"this is not wasm").unwrap();
        let invalid = [
            serde_json::json!({ "runtime": "extism" }),
            serde_json::json!({ "runtime": "extism", "wasm": "missing.wasm" }),
            serde_json::json!({ "runtime": "extism", "wasm": "invalid.wasm" }),
            serde_json::json!({ "runtime": "extism", "wasm": "invalid.wasm", "function": "" }),
            serde_json::json!({ "runtime": "extism", "wasm": "invalid.wasm", "executable": "also.exe" }),
            serde_json::json!({ "runtime": "unknown", "wasm": "invalid.wasm" }),
            serde_json::json!({ "wasm": "invalid.wasm" }),
            serde_json::json!({ "runtime": "extism", "wasm": "invalid.wasm", "allowed_hosts": ["*"] }),
        ];
        for (i, mut manifest) in invalid.into_iter().enumerate() {
            manifest["command"] = format!("plugin{i}").into();
            manifest["description"] = "无效插件".into();
            fs::write(directory.join(format!("{i}.json")), manifest.to_string()).unwrap();
        }
        let plugins = Plugins::load(folder.path()).unwrap();
        assert_eq!(plugins.commands().len(), 4);
        assert_eq!(plugins.warnings().len(), 8);
    }
}
