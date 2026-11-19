use std::path::PathBuf;

use gpui::{prelude::*, *};

use super::{
    Moin,
    appearance::rem,
    input::{Changed, SearchInput},
};
use moin::search::everything;
use moin::{
    ai::{AiConfig, AiProvider},
    config::{
        AI_FONT_SIZE_MAX, AI_FONT_SIZE_MIN, AppearanceConfig, UI_FONT_SIZE_MAX, UI_FONT_SIZE_MIN,
    },
    hotkey::{self, FeatureAction, FeatureShortcuts, Shortcut},
};

// Fluent settings: a quiet navigation surface, white setting rows and one accent.
const INK: u32 = 0x1b1b1b;
const MUTED: u32 = 0x5f5f5f;
const LINE: u32 = 0xe5e5e5;
const PAPER: u32 = 0xf9f9f9;
const RAIL: u32 = 0xf3f3f3;
const WHITE: u32 = 0xffffff;
const ACCENT: u32 = 0x0067c0;
const TINT: u32 = 0xeaf3fb;

const APPEARANCE_MARKDOWN_PREVIEW: &str = "**Moin** 支持 `Markdown` 渲染。\n\n- 列表项\n- **粗体**、*斜体* 与 [链接](https://example.com)\n\n> 引用文本\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```";

fn font_choices() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            "PingFang SC",
            "Hiragino Sans GB",
            "Songti SC",
            "Helvetica Neue",
            "Noto Sans SC",
        ]
    } else {
        &[
            "Microsoft YaHei UI",
            "Segoe UI",
            "SimSun",
            "KaiTi",
            "Noto Sans SC",
        ]
    }
}

#[derive(Default)]
pub struct Snapshot {
    pub roots: Vec<PathBuf>,
    pub indexing: bool,
    pub folder_picker_open: bool,
    pub counts: Option<(usize, usize, usize)>,
    pub error: Option<String>,
    pub plugin_commands: Vec<(String, String)>,
    pub plugin_warnings: Vec<String>,
    pub data_dir: PathBuf,
    pub hotkey_hint: String,
    pub activation_hotkey: String,
    pub hotkey_busy: bool,
    pub hotkey_error: Option<String>,
    pub feature_shortcuts: FeatureShortcuts,
    pub feature_shortcuts_busy: bool,
    pub feature_shortcuts_error: Option<String>,
    pub vector_status: String,
    pub ai_config: AiConfig,
    pub ai_error: Option<String>,
    pub ai_notice: Option<String>,
    pub appearance: AppearanceConfig,
    pub everything_enabled: bool,
    pub everything_status: everything::Status,
    pub everything_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Search,
    Index,
    Plugins,
    Ai,
    Appearance,
    App,
    Shortcuts,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Search => "搜索",
            Self::Index => "索引",
            Self::Plugins => "扩展插件",
            Self::Ai => "AI 助手",
            Self::Appearance => "外观",
            Self::App => "常规",
            Self::Shortcuts => "快捷键",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Search => {
                if cfg!(windows) {
                    "文件名搜索完全交给 Everything，支持文件名、文件夹、路径和 Everything 搜索语法。"
                } else {
                    "当前平台使用 Moin 本地索引检索文件名。"
                }
            }
            Self::Index => "选择需要建立正文索引的目录。",
            Self::Plugins => "查看可用命令，安装和管理本地扩展。",
            Self::Ai => "配置浮窗对话使用的模型，以及本地文档检索。",
            Self::Appearance => "分别调整 AI 对话与其余界面的字体和字号。",
            Self::App => "设置 Moin 的激活快捷键，查看应用信息。",
            Self::Shortcuts => "为各功能分配全局快捷键，直接唤起浮窗并进入对应功能。",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Search => "\u{e721}",
            Self::Index => "\u{e8a5}",
            Self::Plugins => "\u{ea86}",
            Self::Ai => "\u{e8f2}",
            Self::Appearance => "\u{e8d2}",
            Self::App => "\u{e713}",
            Self::Shortcuts => "\u{e765}",
        }
    }
}

#[derive(Clone, Copy)]
enum FontTarget {
    Ui,
    Ai,
}

pub struct Settings {
    launcher: Entity<Moin>,
    page: Page,
    hotkey_draft: String,
    hotkey_recording: bool,
    hotkey_focus: FocusHandle,
    hotkey_capture_error: Option<String>,
    feature_draft: FeatureShortcuts,
    feature_recording: Option<FeatureAction>,
    feature_focus: std::collections::BTreeMap<FeatureAction, FocusHandle>,
    ai_provider: AiProvider,
    ai_base_url: Entity<SearchInput>,
    ai_model: Entity<SearchInput>,
    ai_key_env: Entity<SearchInput>,
    ai_system_prompt: Entity<SearchInput>,
    ai_use_documents: bool,
    ai_allow_commands: bool,
    ai_command_directory: Entity<SearchInput>,
    ai_dirty: bool,
    ui_font_family: Entity<SearchInput>,
    ui_font_size: f32,
    ai_font_family: Entity<SearchInput>,
    ai_font_size: f32,
    appearance_error: Option<String>,
    _field_subscriptions: Vec<Subscription>,
    _subscription: Subscription,
}

pub fn open(launcher: WindowHandle<Moin>, cx: &mut App) -> anyhow::Result<WindowHandle<Settings>> {
    open_page(launcher, Page::App, cx)
}

pub fn open_ai(
    launcher: WindowHandle<Moin>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Settings>> {
    open_page(launcher, Page::Ai, cx)
}

fn open_page(
    launcher: WindowHandle<Moin>,
    page: Page,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Settings>> {
    let launcher = launcher.entity(cx)?;
    // Display bounds use logical pixels; leave room for the taskbar and title bar
    // on laptops with display scaling rather than opening below the screen edge.
    let preferred = size(px(1040.), px(720.));
    let window_size = cx.primary_display().map_or(preferred, |display| {
        let screen = display.bounds().size;
        size(
            preferred.width.min((screen.width - px(48.)).max(px(1.))),
            preferred.height.min((screen.height - px(96.)).max(px(1.))),
        )
    });
    let minimum = size(
        px(840.).min(window_size.width),
        px(560.).min(window_size.height),
    );
    let bounds = Bounds::centered(None, window_size, cx);
    cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Moin 设置".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(minimum),
            ..Default::default()
        },
        move |_, cx| {
            cx.new(|cx: &mut Context<Settings>| {
                let snapshot = launcher.read(cx).settings_snapshot();
                let config = snapshot.ai_config;
                let appearance = snapshot.appearance;
                let ai_base_url = cx.new(|cx| SearchInput::field(&config.base_url, "服务地址", cx));
                let ai_model = cx.new(|cx| {
                    SearchInput::field(&config.model, "填写已安装或服务支持的模型名称", cx)
                });
                let ai_key_env =
                    cx.new(|cx| SearchInput::field(&config.api_key_env, "OPENAI_API_KEY", cx));
                let ai_command_directory = cx.new(|cx| {
                    SearchInput::field(&config.command_working_directory, "留空使用用户主目录", cx)
                });
                let ai_system_prompt = cx.new(|cx| {
                    SearchInput::field(&config.system_prompt, "可选，使用 Moin 默认角色", cx)
                });
                let ui_font_family = cx.new(|cx| {
                    SearchInput::field(&appearance.ui_font_family, "留空使用系统字体", cx)
                });
                let ai_font_family = cx.new(|cx| {
                    SearchInput::field(&appearance.ai_font_family, "留空跟随界面字体", cx)
                });
                let subscriptions: Vec<Subscription> = [
                    &ai_base_url,
                    &ai_model,
                    &ai_key_env,
                    &ai_system_prompt,
                    &ai_command_directory,
                ]
                .into_iter()
                .map(|field| {
                    cx.subscribe(field, |view: &mut Settings, _, _: &Changed, cx| {
                        view.update_ai_dirty(cx);
                        cx.notify();
                    })
                })
                .collect();
                let mut subscriptions = subscriptions;
                subscriptions.extend([&ui_font_family, &ai_font_family].into_iter().map(|field| {
                    cx.subscribe(field, |_: &mut Settings, _, _: &Changed, cx| cx.notify())
                }));
                Settings {
                    hotkey_draft: snapshot.activation_hotkey,
                    hotkey_recording: false,
                    hotkey_focus: cx.focus_handle(),
                    hotkey_capture_error: None,
                    feature_draft: snapshot.feature_shortcuts,
                    feature_recording: None,
                    feature_focus: FeatureAction::ALL
                        .into_iter()
                        .map(|feature| (feature, cx.focus_handle()))
                        .collect(),
                    ai_provider: config.provider,
                    ai_base_url,
                    ai_model,
                    ai_key_env,
                    ai_system_prompt,
                    ai_use_documents: config.use_local_documents,
                    ai_allow_commands: config.allow_commands,
                    ai_command_directory,
                    ai_dirty: false,
                    ui_font_family,
                    ui_font_size: appearance.ui_font_size,
                    ai_font_family,
                    ai_font_size: appearance.ai_font_size,
                    appearance_error: None,
                    _field_subscriptions: subscriptions,
                    _subscription: cx.observe(&launcher, |_, _, cx| cx.notify()),
                    launcher,
                    page,
                }
            })
        },
    )
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.launcher.read(cx).settings_snapshot();
        window.set_rem_size(px(snapshot.appearance.ui_font_size));
        let ui_font_family = super::appearance::resolve_family(&snapshot.appearance.ui_font_family);
        let content = match self.page {
            Page::Search => self.search_page(snapshot, cx),
            Page::Index => self.index_page(snapshot, cx),
            Page::Plugins => self.plugins_page(snapshot),
            Page::Ai => self.ai_page(snapshot, cx),
            Page::Appearance => self.appearance_page(snapshot, cx),
            Page::App => self.app_page(snapshot, cx),
            Page::Shortcuts => self.shortcuts_page(snapshot, cx),
        };
        div()
            .size_full()
            .flex()
            .min_h_0()
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .font_family(ui_font_family)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if view.hotkey_recording && view.recording_focus().is_focused(window) {
                    view.capture_hotkey(event, cx);
                    cx.stop_propagation();
                } else if view.hotkey_recording {
                    view.hotkey_recording = false;
                    cx.notify();
                } else if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                }
            }))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        column()
                            .gap_2()
                            .px(px(32.))
                            .pt(px(28.))
                            .pb(px(24.))
                            .child(title(self.page.label(), 28.))
                            .child(description(self.page.description())),
                    )
                    .child(
                        column()
                            .id(("settings-content", self.page as usize))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(32.))
                            .pb(px(32.))
                            .child(content),
                    ),
            )
    }
}

impl Settings {
    pub fn show_ai(&mut self, cx: &mut Context<Self>) {
        self.page = Page::Ai;
        self.hotkey_recording = false;
        self.hotkey_capture_error = None;
        cx.notify();
    }

    /// The existing global registration consumes its key-down before the focused window.
    /// Route its activation event here while the shortcut field is recording.
    pub fn capture_registered_hotkey(
        &mut self,
        shortcut: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.hotkey_recording
            || !window.is_window_active()
            || !self.recording_focus().is_focused(window)
        {
            return false;
        }
        self.assign_captured_shortcut(shortcut.to_owned());
        cx.notify();
        true
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        column()
            .w(px(224.))
            .min_h_0()
            .flex_shrink_0()
            .bg(rgb(RAIL))
            .px(px(12.))
            .pt(px(24.))
            .pb(px(16.))
            .child(
                row()
                    .gap(px(12.))
                    .px(px(12.))
                    .mb(px(26.))
                    .child(
                        row()
                            .size(px(32.))
                            .justify_center()
                            .rounded(px(6.))
                            .bg(rgb(ACCENT))
                            .child(icon("\u{e713}", 18., WHITE)),
                    )
                    .child(
                        column()
                            .gap_1()
                            .child(title("Moin", 17.))
                            .child(description("设置")),
                    ),
            )
            .child(
                column()
                    .id("settings-navigation")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap(px(4.))
                    .children(
                        [
                            Page::App,
                            Page::Shortcuts,
                            Page::Appearance,
                            Page::Search,
                            Page::Index,
                            Page::Ai,
                            Page::Plugins,
                        ]
                        .map(|page| {
                            let selected = self.page == page;
                            row()
                                .id(("settings-page", page as usize))
                                .tab_index(0)
                                .min_h(rem(40.))
                                .py(px(8.))
                                .gap(px(12.))
                                .pr(px(10.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(rgba(0x00000000))
                                .when(selected, |item| item.bg(rgb(0xe8e8e8)))
                                .hover(|item| item.bg(rgb(0xeaeaea)))
                                .focus(|item| item.border_color(rgb(ACCENT)))
                                .cursor_pointer()
                                .text_size(rem(14.))
                                .child(
                                    div()
                                        .w(px(3.))
                                        .h(rem(16.))
                                        .flex_shrink_0()
                                        .rounded(px(2.))
                                        .bg(rgba(if selected { 0x0067c0ff } else { 0x00000000 })),
                                )
                                .child(icon(page.icon(), 17., if selected { ACCENT } else { INK }))
                                .child(page.label())
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.page = page;
                                    view.hotkey_recording = false;
                                    cx.notify();
                                }))
                        }),
                    ),
            )
            .child(
                column()
                    .px(px(12.))
                    .pt(px(16.))
                    .gap_1()
                    .child(description(format!("Moin {}", env!("CARGO_PKG_VERSION")))),
            )
    }

    fn search_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let enabled = snapshot.everything_enabled;
        let status = snapshot.everything_status.clone();
        let error = snapshot.everything_error.clone();
        if cfg!(windows) {
            column()
                .gap(px(24.))
                .child(self.everything_section(enabled, &status, error, cx))
        } else {
            column().gap(px(24.)).child(section("文件名搜索").child(
                card().px(px(20.)).py(px(16.)).child(description(
                    "当前平台使用 Moin 本地索引检索文件名；Everything 仅在 Windows 上可用。",
                )),
            ))
        }
    }

    fn index_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let idle = !snapshot.indexing && !snapshot.folder_picker_open;
        let status = if snapshot.indexing {
            "正在更新索引"
        } else if snapshot.error.is_some() {
            "索引需要处理"
        } else if snapshot.counts.is_some() {
            "索引已就绪"
        } else {
            "尚未建立索引"
        };
        let counts = snapshot
            .counts
            .map(|(apps, files, chunks)| [apps, files, chunks]);
        let has_roots = !snapshot.roots.is_empty();
        column()
            .gap(px(24.))
            .child(
                section("索引状态")
                    .child(
                        setting_row("\u{e8a5}", status, "应用、文件名和正文片段均保存在本机。")
                            .child(
                                button(
                                    "reindex",
                                    if snapshot.indexing {
                                        "正在更新"
                                    } else {
                                        "更新索引"
                                    },
                                    false,
                                    idle,
                                )
                                .when(idle, |control| {
                                    control.on_click(cx.listener(|view, _, _, cx| {
                                        view.launcher
                                            .update(cx, |launcher, cx| launcher.rebuild(cx));
                                    }))
                                }),
                            ),
                    )
                    .child(card().child(row().px(px(20.)).py(px(18.)).children(
                        ["应用", "文件", "正文片段"].into_iter().enumerate().map(
                            |(index, label)| {
                                column()
                                    .flex_1()
                                    .gap(px(4.))
                                    .when(index > 0, |item| {
                                        item.pl(px(20.)).border_l_1().border_color(rgb(LINE))
                                    })
                                    .child(title(
                                        counts.map_or_else(
                                            || "—".into(),
                                            |values| values[index].to_string(),
                                        ),
                                        22.,
                                    ))
                                    .child(description(label))
                            },
                        ),
                    )))
                    .when(!snapshot.vector_status.is_empty(), |section| {
                        section.child(description(snapshot.vector_status))
                    })
                    .children(snapshot.error.into_iter().map(error_message)),
            )
            .child(
                section("索引目录")
                    .child(
                        setting_row(
                            "\u{e8a5}",
                            "添加索引目录",
                            "将文件夹中的文件正文加入索引；正文检索需要这些目录。",
                        )
                        .child(
                            button("add-root", "添加目录", false, idle).when(idle, |control| {
                                control.on_click(cx.listener(|view, _, _, cx| {
                                    view.launcher
                                        .update(cx, |launcher, cx| launcher.choose_folder(cx));
                                }))
                            }),
                        ),
                    )
                    .when(!has_roots, |section| {
                        section.child(description(if cfg!(windows) {
                            "尚未添加索引目录。添加后，可检索文件正文和向量片段；文件名搜索仍可直接使用 Everything。"
                        } else {
                            "尚未添加索引目录。添加后，可检索文件正文和向量片段。"
                        }))
                    })
                    .children(snapshot.roots.into_iter().enumerate().map(|(index, root)| {
                        let name = root
                            .file_name()
                            .unwrap_or(root.as_os_str())
                            .to_string_lossy()
                            .into_owned();
                        setting_row("\u{e8a5}", name, root.to_string_lossy().into_owned()).child(
                            button(("remove-root", index), "移除目录", false, idle).when(
                                idle,
                                |control| {
                                    control.on_click(cx.listener(move |view, _, _, cx| {
                                        view.launcher.update(cx, |launcher, cx| {
                                            launcher.remove_root(index, cx)
                                        });
                                    }))
                                },
                            ),
                        )
                    })),
            )
    }

    fn everything_section(
        &self,
        enabled: bool,
        status: &everything::Status,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (label, color) = everything_status_label(enabled, status);
        let executable = status.executable().map(PathBuf::from);
        section("文件名搜索")
            .child(
                setting_row(
                    "\u{e8b7}",
                    "Everything 文件名搜索",
                    "直接使用 Everything 搜索文件名、文件夹和路径，支持 `\\` 或 `/` 分隔符，无需设置搜索目录。未连接时文件搜索不可用。",
                )
                .id("everything-toggle")
                .tab_index(0)
                .cursor_pointer()
                .focus(|row| row.border_color(rgb(ACCENT)))
                .child(toggle(enabled))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.launcher.update(cx, |launcher, cx| {
                        launcher.set_everything_enabled(!enabled, cx)
                    });
                    cx.notify();
                }))
                .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        view.launcher.update(cx, |launcher, cx| {
                            launcher.set_everything_enabled(!enabled, cx)
                        });
                        cx.stop_propagation();
                        cx.notify();
                    }
                })),
            )
            .child(
                card()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .min_h(rem(58.))
                    .px(px(20.))
                    .py(px(12.))
                    .child(
                        row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(10.))
                            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(rgb(color)))
                            .child(div().child(description(label))),
                    )
                    .child(
                        row()
                            .gap(px(8.))
                            .flex_wrap()
                            .child(
                                button("refresh-everything", "重新检测", false, true).on_click(
                                    cx.listener(|view, _, _, cx| {
                                        view.launcher.update(cx, |launcher, cx| {
                                            launcher.refresh_everything_status(cx)
                                        });
                                        cx.notify();
                                    }),
                                ),
                            )
                            .when_some(executable, |row, _| {
                                row.child(
                                    button("start-everything", "启动 Everything", false, true)
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.launcher.update(cx, |launcher, cx| {
                                                launcher.start_everything(cx)
                                            });
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(
                                button("get-everything", "获取 Everything", false, true).on_click(
                                    |_, _, cx| {
                                        cx.open_url("https://www.voidtools.com/downloads/")
                                    },
                                ),
                            ),
                    ),
            )
            .children(error.into_iter().map(error_message))
    }

    fn select_ai_provider(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        if self.ai_provider == provider {
            return;
        }
        let old_url = self.ai_base_url.read(cx).value().trim();
        if old_url.is_empty()
            || matches!(
                old_url,
                "http://127.0.0.1:11434" | "http://localhost:11434" | "https://api.openai.com/v1"
            )
        {
            let url = match provider {
                AiProvider::Ollama => "http://127.0.0.1:11434",
                AiProvider::OpenAiCompatible => "https://api.openai.com/v1",
            };
            self.ai_base_url
                .update(cx, |field, cx| field.set_value(url.into(), cx));
        }
        self.ai_provider = provider;
        self.update_ai_dirty(cx);
        cx.notify();
    }

    fn ai_draft(&self, cx: &App) -> AiConfig {
        AiConfig {
            provider: self.ai_provider,
            base_url: self.ai_base_url.read(cx).value().trim().to_owned(),
            model: self.ai_model.read(cx).value().trim().to_owned(),
            api_key_env: self.ai_key_env.read(cx).value().trim().to_owned(),
            system_prompt: self.ai_system_prompt.read(cx).value().to_owned(),
            use_local_documents: self.ai_use_documents,
            allow_commands: self.ai_allow_commands,
            command_working_directory: self.ai_command_directory.read(cx).value().trim().to_owned(),
        }
    }

    fn update_ai_dirty(&mut self, cx: &App) {
        self.ai_dirty = self.ai_draft(cx) != self.launcher.read(cx).settings_snapshot().ai_config;
    }

    fn save_ai_draft(&mut self, cx: &mut Context<Self>) {
        let config = self.ai_draft(cx);
        let saved = self.launcher.update(cx, |launcher, cx| {
            launcher.save_ai_config(config, cx);
            launcher.settings_snapshot().ai_error.is_none()
        });
        if saved {
            self.ai_dirty = false;
        }
        cx.notify();
    }

    fn restore_ai_draft(&mut self, cx: &mut Context<Self>) {
        let config = self.launcher.read(cx).settings_snapshot().ai_config;
        self.ai_provider = config.provider;
        self.ai_use_documents = config.use_local_documents;
        self.ai_allow_commands = config.allow_commands;
        for (field, value) in [
            (&self.ai_base_url, config.base_url),
            (&self.ai_model, config.model),
            (&self.ai_key_env, config.api_key_env),
            (&self.ai_system_prompt, config.system_prompt),
            (&self.ai_command_directory, config.command_working_directory),
        ] {
            field.update(cx, |input, cx| input.set_value(value, cx));
        }
        self.ai_dirty = false;
        cx.notify();
    }

    fn ai_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        column()
            .gap(px(24.))
            .child(
                section("模型连接")
                    .child(
                        setting_row("\u{e8f2}", "模型服务", "选择浮窗对话使用的服务。")
                            .child(row().flex_wrap().justify_end().gap(px(6.)).children([
                                (AiProvider::Ollama, "Ollama", "ai-provider-ollama"),
                                (AiProvider::OpenAiCompatible, "OpenAI 兼容", "ai-provider-openai"),
                            ].into_iter().map(|(provider, label, id)| {
                                let selected = self.ai_provider == provider;
                                button(id, label, selected, true).on_click(cx.listener(move |view, _, _, cx| {
                                    view.select_ai_provider(provider.clone(), cx);
                                }))
                            }))),
                    )
                    .child(input_field("ai-base-url", "服务地址", &self.ai_base_url, cx))
                    .child(input_field("ai-model", "模型名称", &self.ai_model, cx))
                    .child(description(match self.ai_provider {
                        AiProvider::Ollama => "连接正在运行的 Ollama。模型名称需与本机已安装的模型一致。",
                        AiProvider::OpenAiCompatible => "使用 Chat Completions 接口，填写服务商提供的模型名称。",
                    }))
                    .when(self.ai_provider == AiProvider::OpenAiCompatible, |section| {
                        section.child(input_field("ai-api-key-env", "密钥环境变量名", &self.ai_key_env, cx))
                            .child(description("填写保存 API Key 的环境变量名。修改环境变量后，需完全退出并重新启动 Moin。"))
                    }),
            )
            .child(
                section("任务执行")
                    .child(setting_row("\u{e713}", "允许执行本机命令", "以当前用户权限执行命令和脚本。执行输出会发送给所选模型，可在对话中停止任务。")
                        .id("ai-allow-commands").tab_index(0)
                        .focus(|style| style.border_color(rgb(ACCENT)))
                        .hover(|style| style.bg(rgb(0xfdfdfd))).cursor_pointer()
                        .child(toggle(self.ai_allow_commands))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.ai_allow_commands = !view.ai_allow_commands;
                            view.update_ai_dirty(cx); cx.notify();
                        })))
                    .child(input_field("ai-command-directory", "命令工作目录", &self.ai_command_directory, cx))
                    .child(description("填写已存在的绝对路径。Windows 使用 PowerShell，macOS 默认使用 sh；调用之间不保留会话。")),
            )
            .child(
                section("对话与检索")
                    .child(input_field("ai-system-prompt", "角色指令", &self.ai_system_prompt, cx))
                    .child(description("角色指令可留空，使用 Moin 默认角色。"))
                    .child(
                        setting_row("\u{e8a5}", "检索本地文档", "将检索到的相关正文发送给所选模型。需要模型支持工具调用。")
                            .id("ai-local-documents")
                            .tab_index(0)
                            .focus(|style| style.border_color(rgb(ACCENT)))
                            .hover(|style| style.bg(rgb(0xfdfdfd)))
                            .cursor_pointer()
                            .child(toggle(self.ai_use_documents))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.ai_use_documents = !view.ai_use_documents;
                                view.update_ai_dirty(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                column()
                    .gap(px(10.))
                    .child(row().flex_wrap().gap(px(8.))
                        .child(button("save-ai-config", "保存 AI 设置", true, true)
                            .on_click(cx.listener(|view, _, _, cx| view.save_ai_draft(cx))))
                        .child(button("restore-ai-config", "还原已保存设置", false, self.ai_dirty)
                            .when(self.ai_dirty, |control| control.on_click(cx.listener(|view, _, _, cx| {
                                view.restore_ai_draft(cx);
                            }))))
                    )
                    .when(self.ai_dirty, |section| section.child(description("有未保存的修改。")))
                    .when(!self.ai_dirty, |section| section.children(snapshot.ai_notice.into_iter().map(description)))
                    .children(snapshot.ai_error.into_iter().map(error_message)),
            )
    }

    fn appearance_page(&self, _snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let dirty = self.appearance_dirty(cx);
        let draft = self.appearance_draft(cx);
        let ai_style = super::chat::markdown::MarkdownStyle {
            font_family: super::appearance::resolve_ai_family(&draft),
            font_size: draft.ai_font_size,
            line_height: draft.ai_font_size * 1.62,
            text_color: INK,
            muted_color: MUTED,
            link_color: ACCENT,
            code_background: 0xf0f0f0ff,
            rule_color: (LINE << 8) | 0xff,
        };
        column()
            .gap(px(24.))
            .child(
                section("界面文字")
                    .child(input_field(
                        "ui-font-family",
                        "界面字体",
                        &self.ui_font_family,
                        cx,
                    ))
                    .child(self.font_suggestions("ui-font-suggestion", &self.ui_font_family, cx))
                    .child(
                        setting_row("\u{e8d2}", "界面字号", "应用于搜索框、结果列表与设置窗口。")
                            .child(self.font_size_stepper(
                                "ui-font-size",
                                FontTarget::Ui,
                                self.ui_font_size,
                                UI_FONT_SIZE_MIN,
                                UI_FONT_SIZE_MAX,
                                cx,
                            )),
                    )
                    .child(
                        card()
                            .p(px(20.))
                            .gap(px(12.))
                            .child(description("界面预览"))
                            .child(
                                div()
                                    .font_family(super::appearance::resolve_family(
                                        &draft.ui_font_family,
                                    ))
                                    .text_size(px(draft.ui_font_size))
                                    .line_height(px(draft.ui_font_size * 1.55))
                                    .child("搜索应用、文件和命令…"),
                            ),
                    ),
            )
            .child(
                section("AI 对话文字")
                    .child(input_field(
                        "ai-font-family",
                        "对话字体",
                        &self.ai_font_family,
                        cx,
                    ))
                    .child(self.font_suggestions("ai-font-suggestion", &self.ai_font_family, cx))
                    .child(
                        setting_row("\u{e8d2}", "对话字号", "应用于提问、回复和 Markdown 内容。")
                            .child(self.font_size_stepper(
                                "ai-font-size",
                                FontTarget::Ai,
                                self.ai_font_size,
                                AI_FONT_SIZE_MIN,
                                AI_FONT_SIZE_MAX,
                                cx,
                            )),
                    )
                    .child(
                        card()
                            .p(px(20.))
                            .gap(px(16.))
                            .child(description("Markdown 预览"))
                            .child(super::chat::markdown::render(
                                APPEARANCE_MARKDOWN_PREVIEW,
                                &ai_style,
                            )),
                    ),
            )
            .child(
                column()
                    .gap(px(10.))
                    .child(
                        row()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(button("save-appearance", "保存外观设置", true, dirty).when(
                                dirty,
                                |control| {
                                    control.on_click(
                                        cx.listener(|view, _, _, cx| {
                                            view.save_appearance_draft(cx)
                                        }),
                                    )
                                },
                            ))
                            .child(
                                button("restore-appearance", "还原已保存设置", false, dirty).when(
                                    dirty,
                                    |control| {
                                        control.on_click(cx.listener(|view, _, _, cx| {
                                            view.restore_appearance_draft(cx)
                                        }))
                                    },
                                ),
                            ),
                    )
                    .when(dirty, |section| {
                        section.child(description("有未保存的修改。"))
                    })
                    .children(self.appearance_error.clone().into_iter().map(error_message)),
            )
    }

    fn font_suggestions(
        &self,
        id: &'static str,
        field: &Entity<SearchInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut suggestions = row().flex_wrap().gap_2();
        let default_label = if id.starts_with("ai-") {
            "跟随界面字体"
        } else {
            "系统默认字体"
        };
        suggestions = suggestions.child(button((id, 0usize), default_label, false, true).on_click(
            cx.listener({
                let field = field.clone();
                move |_, _, _, cx| {
                    field.update(cx, |input, cx| input.set_value(String::new(), cx));
                    cx.notify();
                }
            }),
        ));
        for (index, family) in font_choices().iter().enumerate() {
            let field = field.clone();
            suggestions =
                suggestions.child(button((id, index + 1), *family, false, true).on_click(
                    cx.listener(move |_, _, _, cx| {
                        field.update(cx, |input, cx| input.set_value((*family).to_owned(), cx));
                        cx.notify();
                    }),
                ));
        }
        suggestions
    }

    fn font_size_stepper(
        &self,
        id: &'static str,
        target: FontTarget,
        value: f32,
        min: f32,
        max: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let can_shrink = value > min;
        let can_grow = value < max;
        row()
            .flex_shrink_0()
            .gap(px(4.))
            .child(
                button((id, 0usize), "−", false, can_shrink).when(can_shrink, |control| {
                    control.on_click(cx.listener(move |view, _, _, cx| {
                        view.change_font_size(target, -1.0, min, max);
                        cx.notify();
                    }))
                }),
            )
            .child(
                div()
                    .w(px(68.))
                    .text_center()
                    .text_size(rem(14.))
                    .child(format!("{} px", value as i32)),
            )
            .child(
                button((id, 1usize), "+", false, can_grow).when(can_grow, |control| {
                    control.on_click(cx.listener(move |view, _, _, cx| {
                        view.change_font_size(target, 1.0, min, max);
                        cx.notify();
                    }))
                }),
            )
    }

    fn change_font_size(&mut self, target: FontTarget, delta: f32, min: f32, max: f32) {
        let value = match target {
            FontTarget::Ui => &mut self.ui_font_size,
            FontTarget::Ai => &mut self.ai_font_size,
        };
        *value = (*value + delta).clamp(min, max);
        self.appearance_error = None;
    }

    fn appearance_draft(&self, cx: &App) -> AppearanceConfig {
        AppearanceConfig {
            ui_font_family: self.ui_font_family.read(cx).value().trim().to_owned(),
            ui_font_size: self.ui_font_size,
            ai_font_family: self.ai_font_family.read(cx).value().trim().to_owned(),
            ai_font_size: self.ai_font_size,
        }
        .normalized()
    }

    fn appearance_dirty(&self, cx: &App) -> bool {
        let saved = self
            .launcher
            .read(cx)
            .settings_snapshot()
            .appearance
            .normalized();
        self.appearance_draft(cx) != saved
    }

    fn save_appearance_draft(&mut self, cx: &mut Context<Self>) {
        let draft = self.appearance_draft(cx);
        let result = self.launcher.update(cx, |launcher, cx| {
            launcher.save_appearance(draft.clone(), cx)
        });
        match result {
            Ok(()) => {
                self.ui_font_size = draft.ui_font_size;
                self.ai_font_size = draft.ai_font_size;
                self.appearance_error = None;
            }
            Err(error) => {
                self.appearance_error = Some(format!("外观设置未保存 · {error:#}"));
            }
        }
        cx.notify();
    }

    fn restore_appearance_draft(&mut self, cx: &mut Context<Self>) {
        let appearance = self.launcher.read(cx).settings_snapshot().appearance;
        self.ui_font_family.update(cx, |input, cx| {
            input.set_value(appearance.ui_font_family, cx);
        });
        self.ai_font_family.update(cx, |input, cx| {
            input.set_value(appearance.ai_font_family, cx);
        });
        self.ui_font_size = appearance.ui_font_size;
        self.ai_font_size = appearance.ai_font_size;
        self.appearance_error = None;
        cx.notify();
    }

    fn plugins_page(&self, snapshot: Snapshot) -> Div {
        let directory = snapshot.data_dir.clone();
        column()
            .gap(px(24.))
            .child(
                section("可用命令")
                    .when(snapshot.plugin_commands.is_empty(), |section| {
                        section.child(description("暂无可用的插件命令。"))
                    })
                    .children(snapshot.plugin_commands.into_iter().map(|(command, help)| {
                        card().child(
                            row()
                                .flex_wrap()
                                .gap(px(16.))
                                .px(px(20.))
                                .py(px(18.))
                                .child(icon("\u{ea86}", 20., INK))
                                .child(div().min_w(px(108.)).child(command_label(command)))
                                .child(column().flex_1().min_w(px(160.)).child(description(help))),
                        )
                    })),
            )
            .child(
                section("安装扩展")
                    .child(
                        setting_row("\u{e8b7}", "本地插件目录", "将插件清单与 Extism .wasm 插件或可执行文件放入 plugins 文件夹，重新启动后加载。")
                            .child(button("open-plugin-data", "打开数据目录", false, true)
                                .on_click(move |_, _, cx| cx.open_with_system(&directory))),
                    )
                    .children(snapshot.plugin_warnings.into_iter().map(error_message)),
            )
    }

    fn recording_focus(&self) -> &FocusHandle {
        self.feature_recording
            .and_then(|feature| self.feature_focus.get(&feature))
            .unwrap_or(&self.hotkey_focus)
    }

    fn assign_captured_shortcut(&mut self, shortcut: String) {
        if let Some(feature) = self.feature_recording.take() {
            self.feature_draft.set(feature, shortcut);
        } else {
            self.hotkey_draft = shortcut;
        }
        self.hotkey_recording = false;
        self.hotkey_capture_error = None;
    }

    fn shortcuts_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let busy = snapshot.feature_shortcuts_busy || snapshot.hotkey_busy;
        let dirty = self.feature_draft != snapshot.feature_shortcuts;
        let can_save = !busy
            && !self.hotkey_recording
            && (dirty || snapshot.feature_shortcuts_error.is_some());
        column().gap(px(24.))
            .child(section("功能激活")
                .child(description(format!("主激活快捷键为 {}，可在「常规」中修改。以下快捷键在 Moin 常驻时全局生效，留空表示不分配。", snapshot.activation_hotkey)))
                .children(FeatureAction::ALL.into_iter().map(|feature| {
                    let recording = self.hotkey_recording && self.feature_recording == Some(feature);
                    let value = self.feature_draft.get(feature);
                    let focus = self.feature_focus.get(&feature).expect("feature focus");
                    setting_row("\u{e765}", feature.label(), feature.description())
                        .child(row().flex_wrap().gap(px(6.))
                            .child(row().id(("feature-hotkey", feature as usize))
                                .track_focus(focus).tab_index(0)
                                .min_w(px(168.)).min_h(rem(34.))
                                .px(px(12.)).py(px(4.)).justify_center()
                                .rounded(px(4.)).border_1()
                                .border_color(rgb(if recording { ACCENT } else { 0xd1d1d1 }))
                                .bg(rgb(if recording { TINT } else { WHITE }))
                                .focus(|style| style.border_color(rgb(ACCENT)))
                                .child(title(if recording { "请按组合键…".to_owned() }
                                    else if value.is_empty() { "未分配".to_owned() }
                                    else { value.to_owned() }, 14.))
                                .when(!busy, |control| control.cursor_pointer()
                                    .hover(|style| style.bg(rgb(TINT)))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.feature_recording = Some(feature);
                                        view.hotkey_recording = true;
                                        view.hotkey_capture_error = None;
                                        view.recording_focus().focus(window);
                                        cx.notify();
                                    }))))
                            .child(button(("clear-feature-hotkey", feature as usize), "清除", false, !busy && !value.is_empty())
                                .when(!busy && !value.is_empty(), |control| control.on_click(cx.listener(move |view, _, _, cx| {
                                    view.feature_draft.set(feature, String::new());
                                    view.hotkey_recording = false;
                                    view.feature_recording = None;
                                    view.hotkey_capture_error = None;
                                    cx.notify();
                                })))))
                })))
            .child(column().gap(px(10.))
                .child(row().flex_wrap().gap(px(8.))
                    .child(button("save-feature-hotkeys", if busy { "正在保存" } else { "保存功能快捷键" }, true, can_save)
                        .when(can_save, |control| control.on_click(cx.listener(|view, _, _, cx| {
                            let draft = view.feature_draft.clone();
                            view.hotkey_capture_error = None;
                            view.launcher.update(cx, |launcher, cx| launcher.save_feature_shortcuts(draft, cx));
                        }))))
                    .child(button("restore-feature-hotkeys", "还原已保存设置", false, !busy && dirty)
                        .when(!busy && dirty, |control| control.on_click(cx.listener(|view, _, _, cx| {
                            view.feature_draft = view.launcher.read(cx).settings_snapshot().feature_shortcuts;
                            view.hotkey_recording = false;
                            view.feature_recording = None;
                            view.hotkey_capture_error = None;
                            cx.notify();
                        })))))
                .child(description(if self.hotkey_recording {
                    "按下新的组合键；按 Esc 取消录制。".to_owned()
                } else if dirty {
                    "有未保存的修改。保存后立即生效，快捷键冲突时保留原配置。".to_owned()
                } else {
                    "点击按键框录制组合键，保存后立即生效。".to_owned()
                }))
                .children(self.hotkey_capture_error.clone().or(snapshot.feature_shortcuts_error).into_iter().map(error_message)))
    }

    fn capture_hotkey(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke.key;
        if key == "escape" && !event.keystroke.modifiers.modified() {
            self.hotkey_recording = false;
            self.feature_recording = None;
            self.hotkey_capture_error = None;
            cx.notify();
            return;
        }
        if [
            "alt", "option", "control", "ctrl", "shift", "platform", "super", "win", "cmd",
            "command", "meta",
        ]
        .contains(&key.as_str())
        {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let mut mask = 0;
        if modifiers.control {
            mask |= hotkey::CONTROL;
        }
        if modifiers.alt {
            mask |= hotkey::ALT;
        }
        if modifiers.shift {
            mask |= hotkey::SHIFT;
        }
        if modifiers.platform {
            mask |= hotkey::WIN;
        }
        match Shortcut::from_key(key, mask) {
            Ok(shortcut) => {
                self.assign_captured_shortcut(shortcut.label());
            }
            Err(error) => self.hotkey_capture_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn app_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let path = snapshot.data_dir.to_string_lossy().into_owned();
        let directory = snapshot.data_dir;
        let busy = snapshot.hotkey_busy || snapshot.feature_shortcuts_busy;
        let changed = self.hotkey_draft != snapshot.activation_hotkey;
        let can_save =
            !busy && !self.hotkey_recording && (changed || snapshot.hotkey_error.is_some());
        let cli_command = if cfg!(windows) {
            "moin-cli.exe mcp"
        } else {
            "moin-cli mcp"
        };
        column()
            .gap(px(24.))
            .child(
                section("激活")
                    .child(
                        setting_row(
                            "\u{e713}",
                            "激活快捷键",
                            "显示或隐藏 Moin 浮窗。点击右侧按键框录制组合键。",
                        )
                        .child(
                            row()
                                .id("activation-hotkey")
                                .track_focus(&self.hotkey_focus)
                                .tab_index(0)
                                .min_w(px(168.))
                                .min_h(rem(34.))
                                .px(px(12.))
                                .py(px(4.))
                                .justify_center()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(rgb(if self.hotkey_recording {
                                    ACCENT
                                } else {
                                    0xd1d1d1
                                }))
                                .bg(rgb(if self.hotkey_recording { TINT } else { WHITE }))
                                .focus(|style| style.border_color(rgb(ACCENT)))
                                .child(title(
                                    if self.hotkey_recording {
                                        "请按组合键…".to_owned()
                                    } else {
                                        self.hotkey_draft.clone()
                                    },
                                    14.,
                                ))
                                .when(!busy, |control| {
                                    control
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(TINT)))
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.feature_recording = None;
                                            view.hotkey_recording = true;
                                            view.hotkey_capture_error = None;
                                            view.hotkey_focus.focus(window);
                                            cx.notify();
                                        }))
                                }),
                        ),
                    )
                    .when(self.hotkey_recording, |section| {
                        section.child(description("按下新的组合键；按 Esc 取消录制。"))
                    })
                    .child(
                        row()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(
                                button(
                                    "save-activation-hotkey",
                                    if busy {
                                        "正在保存"
                                    } else {
                                        "保存快捷键"
                                    },
                                    true,
                                    can_save,
                                )
                                .when(can_save, |control| {
                                    control.on_click(cx.listener(|view, _, _, cx| {
                                        let shortcut = view.hotkey_draft.clone();
                                        view.hotkey_capture_error = None;
                                        view.launcher.update(cx, |launcher, cx| {
                                            launcher.save_activation_hotkey(shortcut, cx)
                                        });
                                    }))
                                }),
                            )
                            .child(
                                button("default-activation-hotkey", "恢复默认快捷键", false, !busy)
                                    .when(!busy, |control| {
                                        control.on_click(cx.listener(|view, _, _, cx| {
                                            view.hotkey_draft = hotkey::default_shortcut();
                                            view.hotkey_recording = false;
                                            view.hotkey_capture_error = None;
                                            cx.notify();
                                        }))
                                    }),
                            ),
                    )
                    .child(description(if changed {
                        format!(
                            "有未保存的修改。已保存的快捷键：{}",
                            snapshot.activation_hotkey
                        )
                    } else if snapshot.hotkey_hint.is_empty() {
                        "可从系统托盘打开浮窗。".to_owned()
                    } else {
                        format!("当前生效：{}", snapshot.hotkey_hint)
                    }))
                    .children(
                        self.hotkey_capture_error
                            .clone()
                            .or(snapshot.hotkey_error)
                            .into_iter()
                            .map(error_message),
                    ),
            )
            .child(
                section("数据与集成")
                    .child(
                        setting_row("\u{e8b7}", "本地数据", path).child(
                            button("open-app-data", "打开数据目录", false, true)
                                .on_click(move |_, _, cx| cx.open_with_system(&directory)),
                        ),
                    )
                    .child(description(
                        "所有界面设置统一保存在数据目录的 settings.json。",
                    ))
                    .child(
                        setting_row(
                            "\u{e8a5}",
                            "MCP 正文检索",
                            "在支持 stdio 的 AI 客户端中添加此启动命令。",
                        )
                        .child(command_label(cli_command)),
                    ),
            )
            .child(
                section("关于").child(
                    setting_row("\u{e713}", "Moin", "应用启动器、文件搜索与 AI 助手。")
                        .child(description(format!("版本 {}", env!("CARGO_PKG_VERSION")))),
                ),
            )
    }
}

fn everything_status_label(enabled: bool, status: &everything::Status) -> (String, u32) {
    if !enabled {
        return ("已停用 · 使用 Moin 本地索引".into(), MUTED);
    }
    match status {
        everything::Status::Checking => ("正在检测 Everything…".into(), MUTED),
        everything::Status::Ready { version } => (format!("已连接 Everything {version}"), ACCENT),
        everything::Status::Starting { version } => (
            version.as_ref().map_or_else(
                || "Everything 正在加载索引…".into(),
                |version| format!("Everything {version} 正在加载索引…"),
            ),
            MUTED,
        ),
        everything::Status::NotRunning { executable } => (
            if executable.is_some() {
                "已安装但未运行 · 可点击启动 Everything".into()
            } else {
                "未连接 · 需要安装并运行 Everything".into()
            },
            0x963e2b,
        ),
        everything::Status::Unavailable(reason) => (format!("不可用 · {reason}"), 0x963e2b),
        everything::Status::Unsupported => ("当前平台不支持 Everything".into(), MUTED),
    }
}

fn row() -> Div {
    div().flex().items_center()
}

fn column() -> Div {
    div().flex().flex_col()
}

fn card() -> Div {
    column()
        .min_w_0()
        .flex_shrink_0()
        .bg(rgb(WHITE))
        .border_1()
        .border_color(rgb(LINE))
        .rounded(px(5.))
}

fn section(label: &'static str) -> Div {
    column()
        .min_w_0()
        .flex_shrink_0()
        .gap(px(4.))
        .child(div().mb(px(8.)).child(title(label, 14.)))
}

fn setting_row(
    glyph: &'static str,
    label: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
    card()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .min_h(rem(76.))
        .px(px(20.))
        .py(px(16.))
        .gap(px(16.))
        .child(
            row()
                .flex_1()
                .min_w(px(200.))
                .gap(px(16.))
                .child(icon(glyph, 20., INK))
                .child(
                    column()
                        .flex_1()
                        .min_w_0()
                        .gap(px(4.))
                        .child(title(label, 14.))
                        .child(description(detail)),
                ),
        )
}

fn toggle(enabled: bool) -> Div {
    row()
        .flex_shrink_0()
        .gap(px(10.))
        .child(description(if enabled { "开启" } else { "关闭" }))
        .child(
            row()
                .w(px(40.))
                .h(px(20.))
                .px(px(3.))
                .rounded_full()
                .border_1()
                .border_color(rgb(if enabled { ACCENT } else { 0x8a8a8a }))
                .bg(rgb(if enabled { ACCENT } else { WHITE }))
                .when(enabled, |track| track.justify_end())
                .child(div().size(px(12.)).rounded_full().bg(rgb(if enabled {
                    WHITE
                } else {
                    0x626262
                }))),
        )
}

fn title(text: impl Into<SharedString>, size: f32) -> Div {
    div()
        .text_size(rem(size))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

fn description(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(rem(13.))
        .line_height(rem(21.))
        .text_color(rgb(MUTED))
        .child(text.into())
}

fn input_field(
    id: &'static str,
    label: &'static str,
    input: &Entity<SearchInput>,
    cx: &App,
) -> Div {
    let focus = input.read(cx).focus_handle(cx);
    let target = input.clone();
    card()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(16.))
        .min_h(rem(76.))
        .px(px(20.))
        .py(px(16.))
        .child(div().w(px(124.)).flex_shrink_0().child(title(label, 14.)))
        .child(
            row()
                .id(id)
                .track_focus(&focus)
                .tab_index(0)
                .flex_1()
                .min_w(px(200.))
                .h(rem(36.))
                .px(px(10.))
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(0xcfcfcf))
                .border_b_2()
                .bg(rgb(WHITE))
                .cursor(CursorStyle::IBeam)
                .hover(|style| style.bg(rgb(0xfafafa)))
                .focus(|style| style.border_color(rgb(ACCENT)))
                .on_click(move |_, window, cx| target.read(cx).focus(window, cx))
                .child(input.clone()),
        )
}

fn icon(glyph: &'static str, size: f32, color: u32) -> Div {
    super::appearance::icon(glyph, size, color)
}

fn command_label(text: impl Into<SharedString>) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded(px(5.))
        .bg(rgb(TINT))
        .text_color(rgb(ACCENT))
        .font_weight(FontWeight::MEDIUM)
        .text_size(rem(13.))
        .child(text.into())
}

fn error_message(message: String) -> Div {
    div()
        .px_3()
        .py_2()
        .rounded(px(6.))
        .text_size(rem(13.))
        .line_height(rem(21.))
        .text_color(rgb(0x963e2b))
        .bg(rgb(0xfaeee9))
        .child(message)
}

fn button(
    id: impl Into<ElementId>,
    label: &'static str,
    primary: bool,
    enabled: bool,
) -> Stateful<Div> {
    let background = if !enabled {
        RAIL
    } else if primary {
        ACCENT
    } else {
        WHITE
    };
    row()
        .id(id)
        .min_h(rem(34.))
        .py(px(4.))
        .flex_shrink_0()
        .px_3()
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(if background == ACCENT { ACCENT } else { LINE }))
        .bg(rgb(background))
        .text_size(rem(13.))
        .text_color(rgb(if background == ACCENT {
            WHITE
        } else if enabled {
            INK
        } else {
            MUTED
        }))
        .when(enabled, |button| {
            button
                .tab_index(0)
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(if primary { 0x1975c5 } else { 0xf3f3f3 })))
                .active(move |style| style.bg(rgb(if primary { 0x3183ca } else { 0xececec })))
                .focus(|style| style.border_2().border_color(rgb(ACCENT)))
        })
        .child(label)
}
