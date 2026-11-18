# Source layout

Moin 的 Rust 代码按业务域组织：

```text
src/
├── ai/                  # AI Agent 后端
│   ├── mod.rs           # Rig Agent、模型配置、对话流
│   ├── commands.rs      # 本机命令执行
│   └── tests.rs
├── app/                 # 桌面 UI 与原生窗口
│   ├── mod.rs           # 主浮窗视图
│   ├── settings.rs      # 设置窗口
│   ├── input.rs         # 文本输入框
│   ├── appearance.rs    # 字体、图标、资源
│   ├── icons.rs         # Windows 系统图标
│   ├── launcher_window.rs
│   ├── chat/            # AI 对话 UI
│   │   ├── mod.rs
│   │   ├── view.rs
│   │   └── markdown.rs
│   └── platform/        # 托盘/菜单栏外壳
│       ├── tray.rs
│       └── tray_macos.rs
├── plugins/             # 插件系统
│   ├── mod.rs
│   └── extism_runtime.rs
├── search/              # 文件名与正文搜索
│   ├── mod.rs
│   ├── index.rs         # SQLite / FTS5 / LanceDB
│   ├── everything.rs    # Everything SDK
│   ├── embedding.rs
│   └── vector_store.rs
├── bin/moin-cli.rs      # CLI / MCP
├── config.rs            # 配置读写：settings.json
├── hotkey.rs            # 快捷键解析与注册（跨领域）
├── lib.rs               # 后端库入口
└── main.rs              # 桌面应用入口
```

约定：

- 新增搜索能力放到 `src/search/`。
- 新增 Agent 能力放到 `src/ai/`。
- 新增界面放到 `src/app/`；AI 对话界面在 `src/app/chat/`。
- 新增插件能力放到 `src/plugins/`。
- `config.rs`、`hotkey.rs` 是被多个业务域共用的基础设施。
