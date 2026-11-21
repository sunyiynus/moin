# Desktop application code

这个目录是 Moin 的桌面 UI 和原生窗口外壳。布局、样式、交互都在这里修改。

| 文件 | 内容 |
| --- | --- |
| `mod.rs` | 主浮窗：`Moin` 视图、搜索框、结果列表、预览、操作菜单、底部状态栏、整体布局 |
| `settings.rs` | 设置窗口：搜索、索引、AI、mihomo 代理、外观、插件、常规页面 |
| `input.rs` | GPUI Kit `Input` / `Textarea` 状态适配：同步业务文本与功能入口按键 |
| `ui.rs` | GPUI Kit 初始化、窗口主题作用域、独立 Kit Root 和业务窗口句柄 |
| `appearance.rs` | 字体解析、rem 缩放、SVG 图标、资源加载 |
| `icons.rs` | Windows 应用/文件系统图标提取（仅 Windows 编译） |
| `launcher_window.rs` | 浮窗原生窗口：显示/隐藏、圆角、亚克力/毛玻璃 |
| `chat/mod.rs` | AI 会话状态、任务控制、`AiPane` 的 `Render` 入口 |
| `chat/view.rs` | AI 对话布局：会话侧栏、消息正文、输入区、底部栏、命令执行详情 |
| `chat/markdown.rs` | AI 回复和设置预览使用的 Markdown 渲染（GPUI Kit `TextView`） |
| `platform/tray.rs` | Windows 托盘图标、托盘菜单和快捷键宿主 |
| `platform/tray_macos.rs` | macOS 菜单栏图标和原生菜单 |

通用 UI 控件使用 [longbridge/gpui-kit 0.10.0](https://github.com/longbridge/gpui-kit) 发布的能力；GPUI 核心由 Kit 固定的 `gpui-pre` 0.3.12 快照提供，并通过 `gpui` 别名保持现有导入路径。按钮、输入框、开关、单选项、列表、侧栏、分组、提示、快捷键标签、头像和折叠区域均由 Kit 组件提供。`div` 仅组织布局；截图选区/画笔使用专用渲染，Markdown 与长文本由 Kit 的 `TextView` 渲染。

每个原生窗口都有独立的 Kit Root。业务层通过 `UiWindow<T>` 访问 Root 下面的视图，避免直接假定窗口根是 `Moin` 或 `AiPane`。浅色设置页与深色浮窗使用覆盖布局、预绘制、绘制三个阶段的主题作用域；退出作用域后恢复主题，避免 Kit 的全局主题影响另一个窗口。AI 会话仍只同步数据。

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
| `src/mihomo.rs` | mihomo 核心进程监督与控制器状态 |
| `src/config.rs` | 配置文件读写 |
| `src/hotkey.rs` | 快捷键解析与注册 |
| `src/lib.rs` | 后端库模块入口 |
| `src/main.rs` | 应用入口和窗口创建 |
