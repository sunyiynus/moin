
``` text
src/
├── ai/                     # AI Agent 业务
│   ├── mod.rs              # Rig Agent、模型配置、对话流
│   ├── commands.rs         # 本机命令执行
│   └── tests.rs
│
├── search/                 # 搜索业务
│   ├── mod.rs
│   ├── index.rs            # SQLite / FTS5 / LanceDB 索引
│   ├── everything.rs       # Everything SDK 文件名搜索
│   ├── embedding.rs        # 嵌入模型
│   └── vector_store.rs     # LanceDB 向量存储
│
├── app/                    # 桌面应用与界面
│   ├── mod.rs              # 主浮窗视图
│   ├── settings.rs         # 设置窗口
│   ├── input.rs            # 文本输入框
│   ├── appearance.rs       # 字体、图标、资源
│   ├── icons.rs            # Windows 系统图标
│   ├── launcher_window.rs  # 原生浮窗效果
│   ├── chat/               # AI 对话 UI
│   │   ├── mod.rs
│   │   ├── view.rs
│   │   └── markdown.rs
│   ├── platform/           # 平台外壳
│   │   ├── tray.rs
│   │   └── tray_macos.rs
│   └── README.md
│
├── plugins/                # 插件业务
│   ├── mod.rs
│   └── extism_runtime.rs
│
├── bin/
│   └── moin-cli.rs         # CLI / MCP
│
├── config.rs               # 跨业务配置
├── hotkey.rs               # 跨业务快捷键
├── lib.rs                  # 后端库入口
├── main.rs                 # 桌面应用入口
└── README.md               # 目录说明
```
