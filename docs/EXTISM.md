# Extism 插件

Moin 内置 Extism Rust 运行时。Windows 和 macOS 使用同一个 `.wasm` 文件。现有可执行插件继续使用原来的 JSON stdin/stdout 协议。

## 安装

把 `.wasm` 与 JSON 清单复制到应用数据目录下的 `plugins` 文件夹，然后重启 Moin。数据目录可以在设置中打开，也可以用 `MOIN_DATA_DIR` 指定。

仓库已提供编译好的 `examples/extism/hello.json` 与 `examples/extism/moin_extism_hello.wasm`，可以直接复制安装。清单示例：

```json
{
  "command": "hello",
  "description": "问候 · hello 世界",
  "runtime": "extism",
  "wasm": "moin_extism_hello.wasm",
  "function": "run",
  "wasi": false,
  "config": { "greeting": "你好" }
}
```

`wasm` 为相对于清单目录的本地路径，也可使用绝对路径。`function` 默认为 `run`。`wasi` 默认为 `false`；使用依赖 WASI Preview 1 的 PDK 时可设为 `true`。`config` 中的字符串通过 Extism PDK 的 config API 读取。`runtime` 也接受 `wasm`；省略 `runtime` 时仍按可执行插件处理。

插件出现在浮窗的命令列表中。输入 `hello 世界` 并按 Enter，Moin 向导出函数传入 UTF-8 JSON：

```json
{ "version": 1, "input": "世界" }
```

函数通过 Extism 的输出 API 返回以下 JSON。输入和输出都是函数调用的数据缓冲区，不使用 stdin/stdout。

```json
{ "title": "问候", "detail": "Extism · Rust / WebAssembly", "output": "你好，世界！" }
```

命令名为 1 至 32 个字母、数字、连字符或下划线。英文命令名不区分大小写，不能与内置命令、其他 Wasm 插件或可执行插件重名。无效清单会出现在插件加载提示中，其他插件仍可使用。清单字段有误会显示错误，避免拼写错误被忽略。

## 编写 Rust 插件

仓库提供完整示例 [examples/extism](../examples/extism)，包括可直接安装的 `moin_extism_hello.wasm` 和 `hello.json`。示例导出 `run` 函数，检查协议版本，读取 `greeting` 配置，并返回问候。附带的 Wasm 由该目录中的 Rust 源码与 Cargo.lock 构建，运行时测试也使用它验证 PDK 兼容性。

在正常的 Rustup 工具链中安装 Wasm 目标并构建：

```sh
rustup target add wasm32-unknown-unknown
cargo build --manifest-path examples/extism/Cargo.toml --target wasm32-unknown-unknown --release
```

构建产物为 `examples/extism/target/wasm32-unknown-unknown/release/moin_extism_hello.wasm`。将它和 `examples/extism/hello.json` 一起复制到数据目录的 `plugins` 文件夹。

其他 Extism PDK 也可使用，只需导出对应函数并遵守相同的 JSON 协议。此接口接收普通文本输入并返回文本结果；当前不提供 Moin 的文件索引、窗口或系统操作的宿主函数。

## 执行与资源限制

插件只在提交命令时执行。首次调用编译 Wasm，随后复用编译结果；编译在后台线程中进行。每次调用创建新的插件实例，Guest 全局变量、Extism vars 和内存不会跨调用保留。替换清单或 Wasm 后重启 Moin 生效。

- 本地 Wasm 文件最多 32 MiB；不加载远程模块或插件提供的 Extism 清单。
- 每次执行最多 10 秒，并限制为 1 亿 fuel；初始化与构造函数也受 fuel 限制。首次编译时间不包含在执行超时内。
- WebAssembly 线性内存最多 1024 页，即 64 MiB；Extism vars 最多 1 MiB。
- 输入命令最多 64 KiB，JSON 输出最多 1 MiB。超限、缺少导出函数、Guest trap 和不合法 JSON 都会显示为命令错误。
- 未开放网络请求、宿主文件系统路径或自定义宿主函数。启用 WASI 不会挂载本机文件夹；清单不接受 `allowed_hosts` 或 `allowed_paths`。需要系统访问的现有可执行插件仍以当前用户权限运行。

官方参考：[Extism Rust SDK](https://docs.rs/extism/1.41.0/extism/)、[Rust PDK](https://github.com/extism/rust-pdk)、[Extism Manifest](https://extism.org/docs/concepts/manifest/)。
