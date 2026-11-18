# Desktop application code

这个目录是 Moin 的桌面 UI 和原生窗口外壳。布局、样式、交互都在这里修改。

| 文件 | 内容 |
| --- | --- |
| `mod.rs` | 主浮窗：`Moin` 视图、搜索框、结果列表、预览、操作菜单、底部状态栏、整体布局 |
| `settings.rs` | 设置窗口：搜索、索引、AI、外观、插件、常规页面 |
| `input.rs` | 自定义文本输入框：光标、选区、IME、绘制 |
| `appearance.rs` | 字体解析、rem 缩放、SVG 图标、资源加载 |
| `icons.rs` | Windows 应用/文件系统图标提取（仅 Windows 编译） |
| `launcher_window.rs` | 浮窗原生窗口：显示/隐藏、圆角、亚克力/毛玻璃 |
| `chat/mod.rs` | AI 会话状态、任务控制、`AiPane` 的 `Render` 入口 |
| `chat/view.rs` | AI 对话布局：会话侧栏、消息正文、输入区、底部栏、命令执行详情 |
| `chat/markdown.rs` | AI 回复和设置预览使用的 Markdown 渲染 |
| `platform/tray.rs` | Windows 托盘图标、托盘菜单和快捷键宿主 |
| `platform/tray_macos.rs` | macOS 菜单栏图标和原生菜单 |

## 后端代码

按业务域放在 `src/` 下：

| 路径 | 内容 |
| --- | --- |
| `src/search/index.rs` | SQLite/FTS5/LanceDB 索引与检索 |
| `src/search/everything.rs` | Everything SDK 文件名搜索 |
| `src/search/embedding.rs` | 嵌入模型 |
| `src/search/vector_store.rs` | LanceDB 向量存储 |
| `src/ai/mod.rs` | Rig Agent 后端 |
| `src/ai/commands.rs` | 本机命令执行 |
| `src/plugins/` | 插件运行时 |
| `src/config.rs` | 配置文件读写 |
| `src/hotkey.rs` | 快捷键解析与注册 |
| `src/lib.rs` | 后端库模块入口 |
| `src/main.rs` | 应用入口和窗口创建 |
