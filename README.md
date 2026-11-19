# Moin

Moin 是用 Rust 和 GPUI 构建的 Windows / macOS 桌面搜索器。它从一个窗口启动应用、查找文件和检索正文，并提供本地文档检索工具供大模型调用。扩展支持 Extism WebAssembly 插件和本机可执行插件。

界面使用 [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit) 的官方组件。当前固定到 `gpui-kit` 0.10.0，核心 GPUI 快照由 `gpui-pre` 0.3.12 提供；输入框、按钮、开关、单选、列表、侧栏、分组、提示、快捷键标签、头像和折叠区域使用 `gpui-component`。Markdown 与长文本使用 Kit 的 `TextView`。图标来自官方资源包，窗口保留亚克力/毛玻璃材质。设置和聊天的主题、输入实体、布局仍按窗口独立。

## 运行

### Windows

构建需要 MSVC Rust 工具链、Visual Studio 的 C++ 工具、带 `fxc.exe` 的 Windows SDK，以及 Protobuf 编译器。设置 `PROTOC` 指向 `protoc.exe`、`PROTOC_INCLUDE` 指向包含 `google/protobuf` 的 include 目录。项目使用 Rust 2024 edition。

```powershell
cargo build --release --bins
.\target\release\moin.exe
```

首次启动会扫描 Windows 开始菜单以及当前用户的桌面、文档和下载目录。从浮窗底部打开「设置」，在「索引」里添加或移除正文索引目录，以及更新索引。移除只影响索引。

搜索使用 Windows 桌面亚克力浮窗，首页显示最近打开的应用和搜索命令。设置使用可调整大小的独立窗口。浮窗失去焦点时会自动收起，切到其他窗口不会留下一层遮挡；再次按激活快捷键会原样恢复上次的查询、视图和 AI 对话。浮窗收起后，Moin 继续在 Windows 右下角通知区域运行，图标也可能出现在「^」展开的区域里。点击托盘中的图标可重新打开，右键菜单可打开或退出。再次启动 `moin.exe` 会唤起已运行的 Moin。

在「设置 → 常规 → 激活快捷键」点击快捷键框，按下组合键，再点击「保存快捷键」。保存后即时生效，并在下次启动时恢复。组合键被其他程序占用时，设置会显示错误，原快捷键继续生效。

「设置 → 快捷键」可以分别录制首页、应用搜索、文件搜索、正文搜索、AI、计算器、翻译、词典、单位换算、截图、GIF 录制和设置的全局激活键。即使 Moin 收起在托盘，也能直接进入对应功能。默认未分配，留空即可停用某项；保存时会检查重复和系统占用，失败会保留原绑定。设置统一保存在 `settings.json` 的 `feature_shortcuts` 字段。

### macOS

在 Mac 上安装 Rust、完整 Xcode（包含 Metal 编译器）和构建依赖，然后构建。原生构建会匹配当前 Mac 的 Apple Silicon 或 Intel 架构。

```sh
brew install protobuf cmake
export PROTOC="$(brew --prefix protobuf)/bin/protoc"
export PROTOC_INCLUDE="$(brew --prefix protobuf)/include"
cargo build --release --locked --bins
./target/release/moin
```

macOS 使用原生毛玻璃浮窗，收起后常驻屏幕右上角的菜单栏。菜单可打开浮窗或退出，Dock 中不保留图标。默认用 `Option+Space` 唤起，在设置中可录制并保存 `Cmd`、`Option`、`Ctrl`、`Shift` 组合。搜索 `/Applications`、`~/Applications`、`/System/Applications` 和系统实用工具中的 `.app`，应用包作为一个结果，不会索引包内资源和辅助程序。打开应用使用系统 `open`。

macOS 的应用内快捷键使用 `Cmd` 代替下表中的 `Ctrl`，包括复制、粘贴、全选、聚焦搜索、更多、切换范围和打开设置。原有 `Ctrl` 绑定也可用。

生成可从 Finder 打开的应用包和压缩包：

```sh
python3 scripts/package_macos.py
```

输出为 `dist/<架构>/Moin.app`，其中也包含 `Contents/MacOS/moin-cli`。复制 `.app` 到「应用程序」即可。脚本默认进行本地 ad-hoc 签名；发行时可通过 `MOIN_SIGN_IDENTITY` 指定开发者证书，仍需另外完成 Apple 公证。代码库包含 Apple Silicon、Intel 两个平台的 macOS 构建与测试工作流。

### 快捷键

| 快捷键 | 操作 |
| --- | --- |
| `Alt+Space` | 唤起浮窗，浮窗在前台时再次按下会收起 |
| `Ctrl+Alt+Space` | `Alt+Space` 已被占用时的备用快捷键 |
| `Ctrl+L` | 聚焦搜索框 |
| `Ctrl+K` | 打开或关闭所选结果的操作菜单 |
| `Ctrl+1` 至 `Ctrl+4` | 切换全部、应用、文件名、正文 |
| `↑` / `↓`、`Tab` / `Shift+Tab` | 选择结果 |
| `PageUp` / `PageDown` | 分页移动选择 |
| `Alt+1` 至 `Alt+9` | 运行对应的可见结果 |
| `Alt+Shift+1` 至 `Alt+Shift+9` | 只选择对应的可见结果 |
| `Enter` | 打开结果或运行插件 |
| `Ctrl+Shift+C` | 复制路径、带出处的正文片段或插件结果 |
| `Ctrl+P` | 展开文件预览或返回结果 |
| `Ctrl+,` | 打开设置 |
| `Esc` | 关闭菜单或展开预览；功能内直接返回全局搜索，首页清空查询后再次按下收起 |
| `Backspace`（搜索框为空） | 返回上级搜索范围 |

浮窗按 Command Palette 的键盘交互组织输入、选择和返回。应用及功能命令可单击运行；文件、文件夹和正文结果单击选中并自动显示右侧预览，双击或按 Enter 打开。方向键移动时预览随选择更新；支持文本、命中片段、常见图片、文件夹列表和文件信息，大文件及不支持的格式显示基本信息。预览在后台读取并限制大小，快速切换时会丢弃过期结果。

在首页直接键入下列入口键即可进入功能，无需空格或 Enter。进入后输入框只接收该功能的查询或参数，入口显示在右侧，按 `Esc` 清除本次查询并直接返回全局搜索；空输入框按 `Backspace` 也可返回。输入法组词期间不会触发入口。

| 入口键 | 功能 |
| --- | --- |
| `a` | 应用搜索 |
| `f` | 文件搜索 |
| `t` | 文件正文搜索 |
| `i` | AI 对话 |
| `c` 或 `=` | 计算器 |
| `r` | 翻译 |
| `d` | 词典 |
| `u` | 单位换算 |
| `s` | 截图 |
| `g` | GIF 录制 |
| `o` | 设置 |

例如键入 `f` 后直接输入文件名，按一次 `Esc` 回到首页，再键入 `c` 输入表达式。也可一次粘贴 `f C:\资料\交接.md`，或使用完整粘贴的 `file `、`apps `、`text ` 及对应中文前缀。内置及第三方插件通过相同的参数页运行，点选插件后填写参数并按 Enter。直接搜索以入口字母开头的应用时，先键入 `a` 进入应用搜索。

`Ctrl+K` 的「更多」菜单可复制结果和打开所在文件夹；方向键选择、Enter 执行，Esc 返回结果列表。功能的全局唤起快捷键仍可在「设置 → 快捷键」中分别配置。

## 截图与 GIF

首页输入 `s` 进入截图，输入 `g` 进入 GIF 录制，也可以点击命令列表或配置各自的全局快捷键。Moin 会先收起浮窗，再显示每个屏幕的框选层；在一个屏幕内拖动选择区域。截图保留原始像素，支持高 DPI 与副屏负坐标。

截图选区下方提供矩形、画笔、撤销、复制、保存 PNG 和贴图。`Enter` 或 `Ctrl+C` 复制，`Ctrl+S` 保存到配置目录，`Ctrl+P` 打开可拖动的置顶贴图，`Ctrl+Z` 撤销标注；macOS 同时支持 `Cmd`。贴图可再次复制、保存，按 `Esc` 关闭。框选状态按 `Esc` 取消并回到全局搜索。

GIF 框选后按 Enter，倒计时三秒后开始录制。控制条显示时长，点击停止或在控制条中按 Esc 会完成编码并保存；再次按配置的 GIF 全局快捷键也会停止录制。录制在后台逐帧编码，不随时长积压图像。输出最长边不超过 1920 像素、总面积不超过 207 万像素，文件接近 256 MiB 时自动结束；实际帧率受截图和编码速度影响。

「设置 → 截图与录制」可配置保存目录、是否包含鼠标指针、GIF 帧率（1–30）和最长时长（1–120 秒），默认保存到系统图片目录中的 `Moin` 文件夹。截图使用 PNG，录制使用循环 GIF，均不包含声音。只有实际保存时才会创建目录，写入失败不会留下不完整的输出文件。

Windows 使用原生 GDI 截图，录制控制条排除在画面之外。macOS 14 及以上使用 [ScreenCaptureKit 内容过滤](https://developer.apple.com/documentation/screencapturekit/sccontentfilter)，排除 Moin 自己的窗口；首次使用需在系统设置中授予屏幕录制权限。较旧 macOS 可使用系统截图服务；GIF 录制需要 macOS 14 或更新版本。macOS 的原生运行行为需要在 Mac 上验证。

## AI 对话

浮窗内置基于 [Rig](https://github.com/0xPlaygrounds/rig) 的 Agent。点击「AI 对话」或按 `Ctrl+5`（macOS 为 `Cmd+5`）进入对话，在「设置 → AI 助手」配置本机 Ollama 或 OpenAI 兼容模型服务。支持流式回答、多轮追问、停止生成、新对话和复制回答；Markdown 与长文本由 GPUI Kit 的 `TextView` 解析和渲染，支持标题、嵌套列表、任务列表、代码块、引用、表格、链接和文本选择，代码块可一键复制。生成中按 `Esc` 停止，空闲时按 `Esc` 返回搜索。模型需由你自行安装或配置。

在「设置 → 外观」可分别指定 AI 对话和其余界面的字体与字号。两项互不影响，保存后立即生效；字体名称留空时使用系统字体，AI 对话则跟随界面字体。

设置窗口采用 Windows Command Palette 风格：左侧分类导航，右侧按功能分组的设置项。激活快捷键在「常规」中配置；文件名搜索、正文索引、AI 模型、字体和插件各有独立页面。

所有界面设置统一保存在数据目录的 `settings.json`，包括索引目录、快捷键、外观、Everything、AI、mihomo 代理和可选的嵌入模型参数。

AI 界面左侧支持多个会话，可新建、切换和删除；各会话有独立上下文和任务。点击设置右侧的「单独窗口」可把对话打开为普通窗口，与浮窗同步会话记录和回复。两边使用独立的 GPUI 视图、输入框和布局，草稿、会话选择、展开状态和滚动位置互不影响。两个窗口均使用透明亚克力背景；会话只在本次运行中保留。

在「设置 → AI 助手 → 任务执行」启用「允许执行本机命令」后，Agent 可以执行 PowerShell 命令和脚本，读取退出码与输出并继续任务。macOS 默认使用 sh。支持配置工作目录，对话显示执行记录，停止生成会结束命令及其子进程。需要支持工具调用的模型，详细行为见 [AI 对话说明](docs/AI.md)。

可以选择允许 Agent 调用本地文档检索工具，复用现有 LanceDB 索引并显示来源。此选项默认关闭；启用后，检索片段会发送给所选模型服务。聊天只保存在当前进程内存中，API Key 从配置的环境变量读取。具体配置及使用方法见 [AI 对话说明](docs/AI.md)。

## mihomo 代理核心

Moin 可以托管本机 [mihomo](https://github.com/MetaCubeX/mihomo) (Clash.Meta) 核心。在「设置 → mihomo 代理」中选择可执行文件与 YAML 配置、填写 loopback 控制器地址并保存，即可启动、停止和重新加载核心；状态卡片显示核心版本、PID 和日志位置。

核心由 [`mihomo-driver`](https://crates.io/crates/mihomo-driver) 启动，命令行为 `mihomo -d <配置目录> -f <配置文件>`，PID 记录保存在数据目录的 `mihomo/mihomo.pid`，输出写入 `mihomo/logs/mihomo.log`。打开「随 Moin 启动」后，下次启动 Moin 会按已保存的设置自动拉起核心；退出 Moin 时会尝试停止核心。

控制器只接受 `http://` + `127.0.0.0/8` 或 `::1` 字面量和显式端口，例如 `http://127.0.0.1:9090`。设置统一保存在 `settings.json` 的 `mihomo` 字段：

```json
"mihomo": {
  "auto_start": false,
  "binary_path": "C:\\Tools\\mihomo\\mihomo.exe",
  "config_path": "C:\\Tools\\mihomo\\config.yaml",
  "controller": "http://127.0.0.1:9090",
  "secret": ""
}
```

`secret` 为空时不发送 bearer 鉴权；它会和其余设置一样以明文保存在本机 `settings.json`。

## 文件名搜索与 Everything

Windows 上的文件名搜索会优先使用本机 Everything 的索引。Moin 已把 Everything SDK 的 x64 DLL 嵌入 `moin.exe`，首次使用时释放到 Moin 数据目录的 `bin` 文件夹（默认 `%LOCALAPPDATA%\Moin\bin`）并动态加载；本机需要安装并运行 [Everything](https://www.voidtools.com/)。启用 Everything 后，文件搜索完全由 Everything 完成；Everything 未安装、未运行或 IPC 不可用时，文件搜索会直接提示失败，可在「设置 → 搜索」中停用 Everything 以改用 Moin 本地索引。

在「设置 → 搜索」可以查看 Everything 连接状态、重新检测或启动 Everything。文件名搜索直接使用 Everything 的索引，不需要设置搜索目录；查询会直接交给 Everything（仅把 `/` 统一为 Windows 的 `\`），Moin 不做其他查询改写、结果过滤或重排，并保持 Everything 的结果顺序。在「全部」范围中，Moin 只负责把应用和正文结果与 Everything 的文件结果拼接，文件部分本身不做处理。

结果同时包含文件和文件夹，文件夹显示为「文件夹」并可用 `Enter` 直接在资源管理器打开。Everything 的路径匹配始终开启，因此文件名、文件夹、完整路径以及 `ext:`、`path:`、`folder:`、`|` 等 Everything 搜索语法都可以直接使用，例如输入 `C:\Users\me\Documents` 会直接按路径匹配。路径分隔符可以用 `\` 或 `/`：`/` 会在发送给 Everything 前转换为 `\`，`regex:` 查询保持原样。

「设置 → 索引」中配置的目录只用于建立正文和向量索引。Everything 只提供文件名和路径结果，正文和向量检索仍由 Moin 完成。SDK 来源和许可见 `assets/everything/NOTICE.md`。

## 本地正文检索

正文片段向量保存在本地 LanceDB 数据目录 `%LOCALAPPDATA%\Moin\index.lancedb`。文件元数据、原文片段和关键词索引保存在 `index.sqlite3`，并记录对应的 LanceDB 表版本。索引支持中英文，可检索不超过 2 MiB 的常见纯文本、Markdown、源代码和配置文件。支持 UTF-8 及带 BOM 的 UTF-16。PDF、Office 等文件当前可按文件名查找。应用与 CLI 可通过环境变量 `MOIN_DATA_DIR` 使用同一替代数据目录。

macOS 默认数据目录为 `~/Library/Application Support/Moin`，向量库、设置和插件均保存在这个目录下。macOS CLI 命令名为 `moin-cli`，不带 `.exe`；MCP 客户端的 `command` 使用它的绝对路径，参数仍为 `["mcp"]`。

正文检索结合关键词和 LanceDB 余弦相似度返回片段。每条返回都带标题、来源路径和片段编号。UI 可预览和复制带出处的片段。重建完成后才发布新版本，已有查询继续读取完整的旧版本。正常启动复用现有索引，文件变化后可在设置中更新。

默认生成 384 维本地词法向量，适合词语重叠检索。要启用神经网络语义检索，在本机 Ollama 安装嵌入模型，在数据目录的 `settings.json` 中添加 `embedding` 对象，然后点击「更新索引」。例如已安装 `bge-m3` 时可使用以下配置。

```json
{
  "embedding": {
    "model": "bge-m3",
    "endpoint": "http://127.0.0.1:11434/api/embed"
  }
}
```

设置的索引页和 CLI `status` 会显示模型及向量数量。配置中的模型必须已安装，模型加载或嵌入失败会显示错误并保留已提交的索引。查询使用该索引记录的模型与维度。删除 `embedding` 对象后更新索引即可切换回默认词法向量。

```powershell
.\target\release\moin-cli.exe status
.\target\release\moin-cli.exe search "项目交接" --limit 8
```

也可以在支持 stdio 的 MCP 客户端中注册 `moin-cli.exe mcp`，调用 `search_local_documents`。例如在支持 `mcpServers` 配置的客户端中使用以下配置，并把路径换成实际位置。

```json
{
  "mcpServers": {
    "moin": {
      "command": "D:\\3_codes_projects\\moin\\target\\release\\moin-cli.exe",
      "args": ["mcp"]
    }
  }
}
```

## 插件

点击浮窗命令列表中的插件或直接输入命令，按 Enter 运行。

| 插件 | 命令示例 |
| --- | --- |
| 计算器 | `= sqrt(81) + 2^3` |
| 词典 | `词典 rust` 或 `dict index` |
| 翻译 | `翻译 hello` 或 `翻译 en 你好` |
| 单位换算 | `换算 12 km mi` |

词典内置小型词表。可在数据目录放置 UTF-8 的 `dictionary.tsv` 扩展词条，每行写单词、Tab 和释义。翻译内置常用中英短语。要翻译任意句子，运行本地 Ollama 并安装支持目标语言的模型，在数据目录保存 `translation.json` 后重启 Moin。模型名使用已安装的名称。

```json
{ "model": "qwen2.5:3b" }
```

Extism 插件使用同一个 `.wasm` 文件即可在 Windows 和 macOS 运行。把清单与模块放入数据目录的 `plugins` 子目录并重启，例如：

```json
{
  "command": "hello",
  "description": "Extism 问候",
  "runtime": "extism",
  "wasm": "hello.wasm",
  "function": "run"
}
```

输入 `hello 世界` 后按 Enter 调用。插件函数接收下述 JSON 请求，返回相同的 `title`、`detail`、`output` JSON 协议。运行时限制执行时间与内存，不映射主机文件目录或开放网络访问。开发示例、清单字段和构建命令见 [Extism 插件开发](docs/EXTISM.md)。

本机可执行插件也可用任何语言编写。把可执行文件与清单放在 `plugins` 子目录并重启。Windows 使用 `.exe`；macOS 使用对应架构的可执行文件，并设置执行权限。下面是 Windows 的 `hello.json` 示例。

```json
{ "command": "hello", "description": "问候", "executable": "hello.exe", "args": [] }
```

运行 `hello 世界` 时，Moin 向插件进程写入一行 UTF-8 JSON，然后关闭 stdin。

```json
{ "version": 1, "input": "世界" }
```

插件从 stdout 返回一个 JSON 对象。工作目录为可执行文件所在目录，超时为 10 秒，stdout 上限为 1 MiB。插件以当前用户权限运行。

```json
{ "title": "问候", "detail": "Hello 插件", "output": "你好，世界" }
```
