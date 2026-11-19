use std::path::PathBuf;

use super::ui::UiWindow;
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable, Icon, IconName, Root, Sizable,
    alert::Alert,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    kbd::Kbd,
    label::Label,
    radio::Radio,
    sidebar::{Sidebar, SidebarMenu, SidebarMenuItem},
    switch::Switch,
    tag::Tag,
};

use super::{
    Moin,
    appearance::rem,
    input::{Changed, SearchInput},
};
use moin::search::everything;
use moin::{
    ai::{AiConfig, AiProvider},
    config::{
        AI_FONT_SIZE_MAX, AI_FONT_SIZE_MIN, AppearanceConfig, CaptureConfig, MihomoConfig,
        UI_FONT_SIZE_MAX, UI_FONT_SIZE_MIN,
    },
    hotkey::{self, FeatureAction, FeatureShortcuts, Shortcut},
    mihomo::{MihomoState, MihomoStatus},
};

// Fluent settings: a quiet navigation surface, white setting rows and one accent.
const INK: u32 = 0x1b1b1b;
const MUTED: u32 = 0x5f5f5f;
const LINE: u32 = 0xe5e5e5;
const PAPER: u32 = 0xf9f9f9;
const RAIL: u32 = 0xf3f3f3;
const WHITE: u32 = 0xffffff;
const ACCENT: u32 = 0x0067c0;

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
    pub capture: CaptureConfig,
    pub capture_error: Option<String>,
    pub mihomo: MihomoConfig,
    pub mihomo_state: MihomoState,
    pub mihomo_error: Option<String>,
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
    Capture,
    Mihomo,
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
            Self::Capture => "截图与录制",
            Self::Mihomo => "mihomo 代理",
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
            Self::Capture => "框选屏幕截图、贴图，或将选定区域录制为 GIF 动图。",
            Self::Mihomo => "启动本机 mihomo (Clash.Meta) 核心，并读取控制器运行状态。",
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
            Self::Capture => "\u{e722}",
            Self::Mihomo => "\u{e774}",
        }
    }
}

#[derive(Clone, Copy)]
enum FontTarget {
    Ui,
    Ai,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MihomoTarget {
    Binary,
    Config,
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
    capture_directory: Entity<SearchInput>,
    capture_fps: u16,
    capture_seconds: u32,
    capture_cursor: bool,
    capture_picker_open: bool,
    capture_error: Option<String>,
    mihomo_binary: Entity<SearchInput>,
    mihomo_config: Entity<SearchInput>,
    mihomo_controller: Entity<SearchInput>,
    mihomo_secret: Entity<SearchInput>,
    mihomo_auto_start: bool,
    mihomo_picker: Option<MihomoTarget>,
    mihomo_error: Option<String>,
    _field_subscriptions: Vec<Subscription>,
    _subscription: Subscription,
}

pub fn open(launcher: UiWindow<Moin>, cx: &mut App) -> anyhow::Result<UiWindow<Settings>> {
    open_page(launcher, Page::Mihomo, cx)
}

pub fn open_ai(launcher: UiWindow<Moin>, cx: &mut App) -> anyhow::Result<UiWindow<Settings>> {
    open_page(launcher, Page::Ai, cx)
}

fn open_page(
    launcher: UiWindow<Moin>,
    page: Page,
    cx: &mut App,
) -> anyhow::Result<UiWindow<Settings>> {
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
        move |window, cx| {
            let view = cx.new(|cx: &mut Context<Settings>| {
                let snapshot = launcher.read(cx).settings_snapshot();
                let config = snapshot.ai_config;
                let appearance = snapshot.appearance;
                let capture = snapshot.capture;
                let mihomo = snapshot.mihomo;
                let mut mihomo_states = launcher.read(cx).subscribe_mihomo();
                cx.spawn(async move |view, cx| {
                    while mihomo_states.changed().await.is_ok() {
                        if view.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                })
                .detach();
                let ai_base_url = cx.new(|cx| SearchInput::field(&config.base_url, "服务地址", cx));
                let ai_model = cx.new(|cx| {
                    SearchInput::field(&config.model, "填写已安装或服务支持的模型名称", cx)
                });
                let capture_directory = cx.new(|cx| {
                    SearchInput::field(
                        &capture.output_dir.to_string_lossy(),
                        "截图与 GIF 的保存目录",
                        cx,
                    )
                });
                let ai_key_env =
                    cx.new(|cx| SearchInput::field(&config.api_key_env, "OPENAI_API_KEY", cx));
                let ai_command_directory = cx.new(|cx| {
                    SearchInput::field(&config.command_working_directory, "留空使用用户主目录", cx)
                });
                let ai_system_prompt = cx.new(|cx| {
                    SearchInput::field(&config.system_prompt, "可选，使用 Moin 默认角色", cx)
                });
                let mihomo_binary = cx.new(|cx| {
                    SearchInput::field(
                        mihomo.binary_path.to_string_lossy().as_ref(),
                        "选择 mihomo 可执行文件",
                        cx,
                    )
                });
                let mihomo_config = cx.new(|cx| {
                    SearchInput::field(
                        mihomo.config_path.to_string_lossy().as_ref(),
                        "选择 mihomo 配置（YAML）",
                        cx,
                    )
                });
                let mihomo_controller = cx
                    .new(|cx| SearchInput::field(&mihomo.controller, "http://127.0.0.1:9090", cx));
                let mihomo_secret = cx.new(|cx| {
                    SearchInput::field(&mihomo.secret, "留空表示控制器未设置 secret", cx)
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
                subscriptions.extend(
                    [
                        &ui_font_family,
                        &ai_font_family,
                        &capture_directory,
                        &mihomo_binary,
                        &mihomo_config,
                        &mihomo_controller,
                        &mihomo_secret,
                    ]
                    .into_iter()
                    .map(|field| {
                        cx.subscribe(field, |_: &mut Settings, _, _: &Changed, cx| cx.notify())
                    }),
                );
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
                    capture_directory,
                    capture_fps: capture.gif_fps,
                    capture_seconds: capture.gif_max_seconds,
                    capture_cursor: capture.include_cursor,
                    capture_picker_open: false,
                    capture_error: None,
                    mihomo_binary,
                    mihomo_config,
                    mihomo_controller,
                    mihomo_secret,
                    mihomo_auto_start: mihomo.auto_start,
                    mihomo_picker: None,
                    mihomo_error: None,
                    _field_subscriptions: subscriptions,
                    _subscription: cx.observe(&launcher, |_, _, cx| cx.notify()),
                    launcher,
                    page,
                }
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .map(UiWindow::from_handle)
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
            Page::Capture => self.capture_page(snapshot, cx),
            Page::Mihomo => self.mihomo_page(snapshot, cx),
        };
        let tree = div()
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
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
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
            );
        super::ui::light(tree, cx)
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

    fn sidebar(&self, cx: &mut Context<Self>) -> Sidebar<SidebarMenu> {
        Sidebar::new("settings-sidebar")
            .collapsible(false)
            .w(px(224.))
            .min_h_0()
            .flex_shrink_0()
            .bg(rgb(RAIL))
            .header(
                column()
                    .px(px(12.))
                    .py(px(16.))
                    .gap_1()
                    .child(title("Moin", 17.))
                    .child(description("设置")),
            )
            .child(
                SidebarMenu::new().children(
                    [
                        Page::App,
                        Page::Shortcuts,
                        Page::Appearance,
                        Page::Search,
                        Page::Index,
                        Page::Ai,
                        Page::Mihomo,
                        Page::Capture,
                        Page::Plugins,
                    ]
                    .map(|page| {
                        SidebarMenuItem::new(page.label())
                            .icon(kit_icon_name(page.icon()))
                            .active(self.page == page)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.page = page;
                                view.hotkey_recording = false;
                                cx.notify();
                            }))
                    }),
                ),
            )
            .footer(description(format!("Moin {}", env!("CARGO_PKG_VERSION"))))
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
    ) -> GroupBox {
        let (label, color) = everything_status_label(enabled, status);
        let executable = status.executable().map(PathBuf::from);
        section("文件名搜索")
            .child(
                setting_row(
                    "\u{e8b7}",
                    "Everything 文件名搜索",
                    "直接使用 Everything 搜索文件名、文件夹和路径，支持 `\\` 或 `/` 分隔符，无需设置搜索目录。未连接时文件搜索不可用。",
                )
                .child(Switch::new("everything-toggle").checked(enabled)
                    .label(if enabled {"开启"} else {"关闭"})
                    .on_click(cx.listener(move|view,checked:&bool,_,cx| {
                        view.launcher.update(cx,|launcher,cx|launcher.set_everything_enabled(*checked,cx));
                        cx.notify();
                    }))),
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
                                Radio::new(id).label(label).checked(selected).on_click(cx.listener(move |view, _, _, cx| {
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
                        .child(Switch::new("ai-allow-commands").checked(self.ai_allow_commands)
                            .on_click(cx.listener(|view,checked:&bool,_,cx| {
                                view.ai_allow_commands=*checked;view.update_ai_dirty(cx);cx.notify();
                            }))))
                    .child(input_field("ai-command-directory", "命令工作目录", &self.ai_command_directory, cx))
                    .child(description("填写已存在的绝对路径。Windows 使用 PowerShell，macOS 默认使用 sh；调用之间不保留会话。")),
            )
            .child(
                section("对话与检索")
                    .child(input_field("ai-system-prompt", "角色指令", &self.ai_system_prompt, cx))
                    .child(description("角色指令可留空，使用 Moin 默认角色。"))
                    .child(
                        setting_row("\u{e8a5}", "检索本地文档", "将检索到的相关正文发送给所选模型。需要模型支持工具调用。")
                            .child(Switch::new("ai-local-documents").checked(self.ai_use_documents)
                                .on_click(cx.listener(|view,checked:&bool,_,cx| {
                                    view.ai_use_documents=*checked;view.update_ai_dirty(cx);cx.notify();
                                }))),
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

    fn capture_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let dirty = self.capture_draft(cx) != snapshot.capture.normalized();
        let can_save = dirty && !self.capture_picker_open;
        column()
            .gap(px(24.))
            .child(
                section("保存位置")
                    .child(input_field(
                        "capture-output-directory",
                        "保存目录",
                        &self.capture_directory,
                        cx,
                    ))
                    .child(
                        row().gap(px(8.)).child(
                            button(
                                "choose-capture-directory",
                                if self.capture_picker_open {
                                    "正在选择…"
                                } else {
                                    "选择文件夹"
                                },
                                false,
                                !self.capture_picker_open,
                            )
                            .when(
                                !self.capture_picker_open,
                                |control| {
                                    control.on_click(
                                        cx.listener(|view, _, _, cx| {
                                            view.choose_capture_folder(cx)
                                        }),
                                    )
                                },
                            ),
                        ),
                    )
                    .child(description(
                        "截图保存为 PNG，录制保存为 GIF。首次保存时会自动创建目录。",
                    )),
            )
            .child(
                section("截图与录制")
                    .child(
                        setting_row(
                            "\u{e722}",
                            "包含鼠标指针",
                            "在截图和录制画面中显示鼠标指针。",
                        )
                        .child(
                            Switch::new("capture-include-cursor")
                                .checked(self.capture_cursor)
                                .on_click(cx.listener(|view, checked: &bool, _, cx| {
                                    view.capture_cursor = *checked;
                                    view.capture_error = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        setting_row(
                            "\u{e722}",
                            "GIF 帧率",
                            format!(
                                "每秒 {} 帧。帧率越高，画面越流畅，文件也越大。",
                                self.capture_fps
                            ),
                        )
                        .child(
                            row().flex_wrap().gap(px(4.)).children(
                                [(5u16, "5"), (10, "10"), (15, "15"), (20, "20"), (30, "30")]
                                    .into_iter()
                                    .map(|(fps, label)| {
                                        button(
                                            ("capture-gif-fps", fps as usize),
                                            label,
                                            self.capture_fps == fps,
                                            true,
                                        )
                                        .on_click(
                                            cx.listener(move |view, _, _, cx| {
                                                view.capture_fps = fps;
                                                view.capture_error = None;
                                                cx.notify();
                                            }),
                                        )
                                    }),
                            ),
                        ),
                    )
                    .child(
                        setting_row(
                            "\u{e722}",
                            "最长录制时长",
                            format!(
                                "{} 秒后自动停止，也可以提前手动结束。",
                                self.capture_seconds
                            ),
                        )
                        .child(
                            row().flex_wrap().gap(px(4.)).children(
                                [
                                    (15u32, "15 秒"),
                                    (30, "30 秒"),
                                    (60, "60 秒"),
                                    (120, "120 秒"),
                                ]
                                .into_iter()
                                .map(|(seconds, label)| {
                                    button(
                                        ("capture-gif-seconds", seconds as usize),
                                        label,
                                        self.capture_seconds == seconds,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            view.capture_seconds = seconds;
                                            view.capture_error = None;
                                            cx.notify();
                                        },
                                    ))
                                }),
                            ),
                        ),
                    ),
            )
            .child(
                section("快捷入口").child(
                    setting_row(
                        "\u{e765}",
                        "截图与录制快捷键",
                        "在「快捷键」页面为截图和 GIF 录制分别分配全局快捷键。",
                    )
                    .child(
                        button("capture-open-shortcuts", "配置快捷键", false, true).on_click(
                            cx.listener(|view, _, _, cx| {
                                view.page = Page::Shortcuts;
                                cx.notify();
                            }),
                        ),
                    ),
                ),
            )
            .child(
                column()
                    .gap(px(10.))
                    .child(
                        row()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(
                                button("save-capture-config", "保存截图与录制设置", true, can_save)
                                    .when(can_save, |control| {
                                        control.on_click(
                                            cx.listener(|view, _, _, cx| {
                                                view.save_capture_draft(cx)
                                            }),
                                        )
                                    }),
                            )
                            .child(
                                button("restore-capture-config", "还原已保存设置", false, can_save)
                                    .when(can_save, |control| {
                                        control.on_click(cx.listener(|view, _, _, cx| {
                                            view.restore_capture_draft(cx)
                                        }))
                                    }),
                            ),
                    )
                    .when(dirty, |section| {
                        section.child(description("有未保存的修改。"))
                    })
                    .children(
                        self.capture_error
                            .clone()
                            .or(snapshot.capture_error)
                            .into_iter()
                            .map(error_message),
                    ),
            )
    }

    fn capture_draft(&self, cx: &App) -> CaptureConfig {
        CaptureConfig {
            output_dir: PathBuf::from(self.capture_directory.read(cx).value().trim()),
            gif_fps: self.capture_fps,
            gif_max_seconds: self.capture_seconds,
            include_cursor: self.capture_cursor,
        }
        .normalized()
    }

    fn choose_capture_folder(&mut self, cx: &mut Context<Self>) {
        if self.capture_picker_open {
            return;
        }
        self.capture_picker_open = true;
        self.capture_error = None;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择截图与 GIF 保存目录".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |view, cx| {
                view.capture_picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            view.capture_directory.update(cx, |input, cx| {
                                input.set_value(path.to_string_lossy().into_owned(), cx);
                            });
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        view.capture_error = Some(format!("无法选择保存目录 · {error:#}"))
                    }
                    Err(_) => view.capture_error = Some("目录选择器已关闭".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_capture_draft(&mut self, cx: &mut Context<Self>) {
        let draft = self.capture_draft(cx);
        let result = draft.clone().validated().and_then(|config| {
            self.launcher
                .update(cx, |launcher, cx| launcher.save_capture_config(config, cx))
        });
        match result {
            Ok(()) => {
                self.capture_directory.update(cx, |input, cx| {
                    input.set_value(draft.output_dir.to_string_lossy().into_owned(), cx);
                });
                self.capture_error = None;
            }
            Err(error) => self.capture_error = Some(format!("截图与录制设置未保存 · {error:#}")),
        }
        cx.notify();
    }

    fn restore_capture_draft(&mut self, cx: &mut Context<Self>) {
        let capture = self.launcher.read(cx).settings_snapshot().capture;
        self.capture_directory.update(cx, |input, cx| {
            input.set_value(capture.output_dir.to_string_lossy().into_owned(), cx);
        });
        self.capture_fps = capture.gif_fps;
        self.capture_seconds = capture.gif_max_seconds;
        self.capture_cursor = capture.include_cursor;
        self.capture_error = None;
        cx.notify();
    }

    fn mihomo_page(&self, snapshot: Snapshot, cx: &mut Context<Self>) -> Div {
        let draft = self.mihomo_draft(cx);
        let dirty = draft != snapshot.mihomo;
        let busy = snapshot.mihomo_state.status.is_busy();
        let running = snapshot.mihomo_state.status.is_running();
        let picking = self.mihomo_picker.is_some();
        let can_start = !busy && !dirty && !picking && snapshot.mihomo.configured();
        let can_save = !busy && !picking && dirty;
        let (status, status_color) = mihomo_status_label(&snapshot.mihomo_state);
        let log_dir = snapshot.data_dir.join("mihomo").join("logs");
        let log_hint = log_dir.to_string_lossy().into_owned();
        let error = snapshot
            .mihomo_error
            .clone()
            .or(snapshot.mihomo_state.error.clone());
        let notice = snapshot.mihomo_state.notice.clone();

        column()
            .gap(px(24.))
            .child(
                section("核心状态")
                    .child(
                        card()
                            .gap(px(12.))
                            .px(px(20.))
                            .py(px(16.))
                            .child(
                                row()
                                    .flex_wrap()
                                    .justify_between()
                                    .gap(px(16.))
                                    .child(
                                        column().flex_1().min_w(px(220.)).gap(px(4.)).child(
                                            Label::new(status.clone())
                                                .id((ElementId::from("mihomo-status"), status))
                                                .text_size(rem(14.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(status_color)),
                                        )
                                        .child(description(mihomo_status_detail(&snapshot))),
                                    )
                                    .child(
                                        row()
                                            .flex_wrap()
                                            .gap(px(8.))
                                            .child(
                                                button(
                                                    "start-mihomo",
                                                    if running { "重启核心" } else { "启动核心" },
                                                    true,
                                                    can_start,
                                                )
                                                    .when(can_start, |control| {
                                                        control.on_click(cx.listener(
                                                            |view, _, _, cx| {
                                                                view.mihomo_error = None;
                                                                view.launcher.update(
                                                                    cx,
                                                                    |launcher, cx| {
                                                                        launcher
                                                                            .start_mihomo(cx)
                                                                    },
                                                                );
                                                                cx.notify();
                                                            },
                                                        ))
                                                    }),
                                            )
                                            .child(
                                                button(
                                                    "stop-mihomo",
                                                    "停止核心",
                                                    false,
                                                    running && !busy,
                                                )
                                                .when(running && !busy, |control| {
                                                    control.on_click(cx.listener(
                                                        |view, _, _, cx| {
                                                            view.mihomo_error = None;
                                                            view.launcher.update(
                                                                cx,
                                                                |launcher, cx| {
                                                                    launcher.stop_mihomo(cx)
                                                                },
                                                            );
                                                            cx.notify();
                                                        },
                                                    ))
                                                }),
                                            )
                                            .child(
                                                button(
                                                    "reload-mihomo",
                                                    "重新加载配置",
                                                    false,
                                                    running && !busy,
                                                )
                                                .when(running && !busy, |control| {
                                                    control.on_click(cx.listener(
                                                        |view, _, _, cx| {
                                                            view.mihomo_error = None;
                                                            view.launcher.update(
                                                                cx,
                                                                |launcher, cx| {
                                                                    launcher.reload_mihomo(cx)
                                                                },
                                                            );
                                                            cx.notify();
                                                        },
                                                    ))
                                                }),
                                            )
                                            .child(
                                                button("refresh-mihomo", "刷新状态", false, !busy)
                                                    .when(!busy, |control| {
                                                        control.on_click(cx.listener(
                                                            |view, _, _, cx| {
                                                                view.mihomo_error = None;
                                                                view.launcher.update(
                                                                    cx,
                                                                    |launcher, cx| {
                                                                        launcher
                                                                            .refresh_mihomo(cx)
                                                                    },
                                                                );
                                                                cx.notify();
                                                            },
                                                        ))
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                row()
                                    .flex_wrap()
                                    .gap(px(8.))
                                    .child(
                                        button("open-mihomo-logs", "打开日志目录", false, true)
                                            .on_click(move |_, _, cx| {
                                                let _ = std::fs::create_dir_all(&log_dir);
                                                cx.open_with_system(&log_dir);
                                            }),
                                    )
                                    .child(description(format!("日志：{log_hint}"))),
                            )
                            .when_some(error, |card, error| card.child(error_message(error)))
                            .when_some(notice, |card, notice| card.child(description(notice))),
                    )
                    .when(dirty, |section| {
                        section.child(description("有未保存的修改；启动核心使用已保存的设置。"))
                    }),
            )
            .child(
                section("核心文件")
                    .child(input_field(
                        "mihomo-binary-path",
                        "可执行文件",
                        &self.mihomo_binary,
                        cx,
                    ))
                    .child(
                        row().gap(px(8.)).child(
                            button(
                                "choose-mihomo-binary",
                                if self.mihomo_picker == Some(MihomoTarget::Binary) {
                                    "正在选择…"
                                } else {
                                    "选择 mihomo 可执行文件"
                                },
                                false,
                                !picking,
                            )
                            .when(!picking, |control| {
                                control.on_click(cx.listener(|view, _, _, cx| {
                                    view.choose_mihomo_file(MihomoTarget::Binary, cx)
                                }))
                            }),
                        ),
                    )
                    .child(input_field(
                        "mihomo-config-path",
                        "配置文件",
                        &self.mihomo_config,
                        cx,
                    ))
                    .child(
                        row().gap(px(8.)).child(
                            button(
                                "choose-mihomo-config",
                                if self.mihomo_picker == Some(MihomoTarget::Config) {
                                    "正在选择…"
                                } else {
                                    "选择 mihomo 配置文件"
                                },
                                false,
                                !picking,
                            )
                            .when(!picking, |control| {
                                control.on_click(cx.listener(|view, _, _, cx| {
                                    view.choose_mihomo_file(MihomoTarget::Config, cx)
                                }))
                            }),
                        ),
                    )
                    .child(description(
                        "Moin 使用 mihomo -d <配置目录> -f <配置文件> 启动核心；PID 与日志保存在数据目录的 mihomo 文件夹。",
                    )),
            )
            .child(
                section("控制器")
                    .child(input_field(
                        "mihomo-controller",
                        "控制器",
                        &self.mihomo_controller,
                        cx,
                    ))
                    .child(input_field(
                        "mihomo-secret",
                        "Secret",
                        &self.mihomo_secret,
                        cx,
                    ))
                    .child(description(
                        "只接受 http:// 的 loopback IP 与显式端口，例如 http://127.0.0.1:9090。Secret 与其余设置一起明文保存在 settings.json。",
                    )),
            )
            .child(
                section("启动行为").child(
                    setting_row(
                        "\u{e713}",
                        "随 Moin 启动",
                        "下次启动 Moin 时自动拉起核心；路径为空时不会尝试。",
                    )
                    .child(
                        Switch::new("mihomo-auto-start")
                            .checked(self.mihomo_auto_start)
                            .on_click(cx.listener(|view, checked: &bool, _, cx| {
                                view.mihomo_auto_start = *checked;
                                view.mihomo_error = None;
                                cx.notify();
                            })),
                    ),
                ),
            )
            .child(
                column()
                    .gap(px(10.))
                    .child(
                        row()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(
                                button("save-mihomo-config", "保存 mihomo 设置", true, can_save)
                                    .when(can_save, |control| {
                                        control.on_click(cx.listener(|view, _, _, cx| {
                                            view.save_mihomo_draft(cx)
                                        }))
                                    }),
                            )
                            .child(
                                button("restore-mihomo-config", "还原已保存设置", false, can_save)
                                    .when(can_save, |control| {
                                        control.on_click(cx.listener(|view, _, _, cx| {
                                            view.restore_mihomo_draft(cx)
                                        }))
                                    }),
                            ),
                    )
                    .when(dirty, |section| {
                        section.child(description("有未保存的修改。"))
                    })
                    .children(self.mihomo_error.clone().into_iter().map(error_message)),
            )
    }

    fn mihomo_draft(&self, cx: &App) -> MihomoConfig {
        MihomoConfig {
            auto_start: self.mihomo_auto_start,
            binary_path: PathBuf::from(self.mihomo_binary.read(cx).value().trim()),
            config_path: PathBuf::from(self.mihomo_config.read(cx).value().trim()),
            controller: self.mihomo_controller.read(cx).value().trim().to_owned(),
            secret: self.mihomo_secret.read(cx).value().trim().to_owned(),
        }
        .normalized()
    }

    fn choose_mihomo_file(&mut self, target: MihomoTarget, cx: &mut Context<Self>) {
        if self.mihomo_picker.is_some() {
            return;
        }
        self.mihomo_picker = Some(target);
        self.mihomo_error = None;
        let prompt = match target {
            MihomoTarget::Binary => "选择 mihomo 可执行文件",
            MihomoTarget::Config => "选择 mihomo 配置文件",
        };
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |view, cx| {
                view.mihomo_picker = None;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            let input = match target {
                                MihomoTarget::Binary => &view.mihomo_binary,
                                MihomoTarget::Config => &view.mihomo_config,
                            };
                            input.update(cx, |input, cx| {
                                input.set_value(path.to_string_lossy().into_owned(), cx);
                            });
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        view.mihomo_error = Some(format!("无法选择 mihomo 文件 · {error:#}"))
                    }
                    Err(_) => view.mihomo_error = Some("文件选择器已关闭".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_mihomo_draft(&mut self, cx: &mut Context<Self>) {
        let draft = self.mihomo_draft(cx);
        let result = self.launcher.update(cx, |launcher, cx| {
            launcher.save_mihomo_config(draft.clone(), cx)
        });
        match result {
            Ok(()) => {
                self.mihomo_binary.update(cx, |input, cx| {
                    input.set_value(draft.binary_path.to_string_lossy().into_owned(), cx);
                });
                self.mihomo_config.update(cx, |input, cx| {
                    input.set_value(draft.config_path.to_string_lossy().into_owned(), cx);
                });
                self.mihomo_controller.update(cx, |input, cx| {
                    input.set_value(draft.controller.clone(), cx);
                });
                self.mihomo_secret.update(cx, |input, cx| {
                    input.set_value(draft.secret.clone(), cx);
                });
                self.mihomo_error = None;
            }
            Err(error) => self.mihomo_error = Some(format!("mihomo 设置未保存 · {error:#}")),
        }
        cx.notify();
    }

    fn restore_mihomo_draft(&mut self, cx: &mut Context<Self>) {
        let mihomo = self.launcher.read(cx).settings_snapshot().mihomo;
        self.mihomo_auto_start = mihomo.auto_start;
        self.mihomo_binary.update(cx, |input, cx| {
            input.set_value(mihomo.binary_path.to_string_lossy().into_owned(), cx);
        });
        self.mihomo_config.update(cx, |input, cx| {
            input.set_value(mihomo.config_path.to_string_lossy().into_owned(), cx);
        });
        self.mihomo_controller.update(cx, |input, cx| {
            input.set_value(mihomo.controller, cx);
        });
        self.mihomo_secret.update(cx, |input, cx| {
            input.set_value(mihomo.secret, cx);
        });
        self.mihomo_error = None;
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
                            .child(row().id(("feature-hotkey-focus",feature as usize)).track_focus(focus)
                                .child(shortcut_button(("feature-hotkey",feature as usize),value,recording,!busy)
                                    .on_click(cx.listener(move|view,_,window,cx| {
                                        view.feature_recording=Some(feature);
                                        view.hotkey_recording=true;view.hotkey_capture_error=None;
                                        view.recording_focus().focus(window, cx);cx.notify();
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
                                .id("activation-hotkey-focus")
                                .track_focus(&self.hotkey_focus)
                                .child(
                                    shortcut_button(
                                        "activation-hotkey",
                                        &self.hotkey_draft,
                                        self.hotkey_recording,
                                        !busy,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, window, cx| {
                                            view.feature_recording = None;
                                            view.hotkey_recording = true;
                                            view.hotkey_capture_error = None;
                                            view.hotkey_focus.focus(window, cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
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

fn mihomo_status_label(state: &MihomoState) -> (String, u32) {
    match &state.status {
        MihomoStatus::Stopped => ("核心未运行".into(), MUTED),
        MihomoStatus::Starting => ("正在启动 mihomo…".into(), MUTED),
        MihomoStatus::Running { version, .. } => (
            version.as_ref().map_or_else(
                || "mihomo 运行中".into(),
                |version| format!("mihomo {version} 运行中"),
            ),
            ACCENT,
        ),
        MihomoStatus::Stopping => ("正在停止 mihomo…".into(), MUTED),
    }
}

fn mihomo_status_detail(snapshot: &Snapshot) -> String {
    let controller = if snapshot.mihomo.controller.is_empty() {
        "控制器未填写".to_owned()
    } else {
        snapshot.mihomo.controller.clone()
    };
    match &snapshot.mihomo_state.status {
        MihomoStatus::Stopped => {
            if snapshot.mihomo.configured() {
                format!("控制器 {controller} · 等待启动")
            } else {
                "先选择 mihomo 可执行文件和配置文件，再启动核心。".into()
            }
        }
        MihomoStatus::Starting => format!("控制器 {controller} · 正在等待核心就绪"),
        MihomoStatus::Running { pid, .. } => match pid {
            Some(pid) => format!("控制器 {controller} · PID {pid}"),
            None => format!("控制器 {controller} · 进程已运行"),
        },
        MihomoStatus::Stopping => format!("控制器 {controller} · 正在停止进程"),
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

fn section(label: &'static str) -> GroupBox {
    GroupBox::new()
        .id(SharedString::from(format!("settings-section-{label}")))
        .title(title(label, 14.))
        .min_w_0()
        .flex_shrink_0()
        .gap(px(8.))
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

fn title(text: impl Into<SharedString>, size: f32) -> Label {
    let text = text.into();
    // GPUI keys a component's accessibility node by its whole id path, and
    // `Label::new` only sees this helper as the call site. Every label built here
    // would therefore share one element id and trip the duplicate-node assertion
    // once accessibility is active, so name each label after what it shows.
    Label::new(text.clone())
        .id((ElementId::from("settings-title"), text))
        .text_size(rem(size))
        .font_weight(FontWeight::SEMIBOLD)
}
fn description(text: impl Into<SharedString>) -> Label {
    let text = text.into();
    Label::new(text.clone())
        .id((ElementId::from("settings-description"), text))
        .text_size(rem(13.))
        .line_height(rem(21.))
        .text_color(rgb(MUTED))
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
                .on_click(move |_, window, cx| target.update(cx, |input, cx| input.focus(window, cx)))
                .child(input.clone()),
        )
}

fn kit_icon_name(glyph: &str) -> IconName {
    match glyph {
        "\u{e721}" => IconName::Search,
        "\u{e8a5}" => IconName::File,
        "\u{e8b7}" => IconName::Folder,
        "\u{e8f2}" => IconName::Bot,
        "\u{e8d2}" => IconName::ALargeSmall,
        "\u{e765}" => IconName::SquareTerminal,
        "\u{e722}" => IconName::Frame,
        "\u{e774}" => IconName::Network,
        "\u{ea86}" => IconName::LayoutDashboard,
        _ => IconName::Settings,
    }
}
fn icon(glyph: &'static str, size: f32, color: u32) -> Icon {
    Icon::new(kit_icon_name(glyph))
        .size(px(size))
        .text_color(rgb(color))
}
fn command_label(text: impl Into<SharedString>) -> Tag {
    Tag::new().child(text.into()).text_size(rem(13.))
}
fn error_message(message: String) -> Alert {
    Alert::error(
        SharedString::from(format!("settings-error-{message}")),
        message,
    )
    .small()
}
fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
) -> Button {
    Button::new(id)
        .label(label)
        .small()
        .disabled(!enabled)
        .when(primary, |button| button.primary())
}

fn shortcut_button(
    id: impl Into<ElementId>,
    value: &str,
    recording: bool,
    enabled: bool,
) -> Button {
    let mut control = Button::new(id)
        .small()
        .min_w(px(168.))
        .h(rem(34.))
        .disabled(!enabled);
    if recording {
        return control.label("请按组合键…").primary();
    }
    if value.is_empty() {
        return control.label("未分配");
    }
    let source = value
        .replace('+', "-")
        .to_ascii_lowercase()
        .replace("option-", "alt-");
    if let Ok(stroke) = super::parse_keystroke(&source) {
        control = control.label("").child(Kbd::new(stroke).appearance(false));
    } else {
        control = control.label(value.to_owned());
    }
    control.tooltip("点击录制快捷键")
}

#[cfg(test)]
mod kit_tests {
    use super::*;

    struct Controls {
        enabled: bool,
        toggles: usize,
        saved: usize,
        recording: bool,
        shortcut: String,
        capture_focus: FocusHandle,
    }

    impl Render for Controls {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            column()
                .size_full()
                .p_4()
                .bg(rgb(PAPER))
                .text_color(rgb(INK))
                .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                    if view.recording && view.capture_focus.is_focused(window) {
                        let modifiers = event.keystroke.modifiers;
                        let mask = (if modifiers.control {
                            hotkey::CONTROL
                        } else {
                            0
                        }) | (if modifiers.shift { hotkey::SHIFT } else { 0 });
                        if let Ok(shortcut) = Shortcut::from_key(&event.keystroke.key, mask) {
                            view.shortcut = shortcut.label();
                            view.recording = false;
                            cx.notify();
                        }
                    }
                }))
                .child(
                    section("组件交互")
                        .child(
                            setting_row(
                                "\u{e8f2}",
                                "允许执行本机命令",
                                "点击开关后只更新一次状态。",
                            )
                            .child(
                                row()
                                    .debug_selector(|| "settings-test-switch".into())
                                    .child(
                                        Switch::new("settings-test-switch-control")
                                            .checked(self.enabled)
                                            .on_click(cx.listener(
                                                |view, checked: &bool, _, cx| {
                                                    view.enabled = *checked;
                                                    view.toggles += 1;
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            ),
                        )
                        .child(
                            row()
                                .debug_selector(|| "settings-test-disabled-save".into())
                                .child(
                                    button(
                                        "settings-test-disabled-save-control",
                                        "保存设置",
                                        true,
                                        false,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, _, cx| {
                                            view.saved += 1;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .child(
                            row()
                                .id("settings-test-focus")
                                .track_focus(&self.capture_focus)
                                .debug_selector(|| "settings-test-shortcut".into())
                                .child(
                                    shortcut_button(
                                        "settings-test-shortcut-control",
                                        &self.shortcut,
                                        self.recording,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, window, cx| {
                                            view.recording = true;
                                            view.capture_focus.focus(window, cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                )
        }
    }

    #[::core::prelude::v1::test]
    fn kit_settings_controls_preserve_toggle_disabled_and_shortcut_interactions() {
        let mut app = TestAppContext::single();
        app.update(super::super::ui::init);
        let view = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured = view.clone();
        let (_, cx) = app.add_window_view(move |window, cx| {
            let controls = cx.new(|cx| Controls {
                enabled: false,
                toggles: 0,
                saved: 0,
                recording: false,
                shortcut: "Alt+Space".into(),
                capture_focus: cx.focus_handle(),
            });
            *captured.borrow_mut() = Some(controls.clone());
            Root::new(controls, window, cx)
        });
        let view = view.borrow_mut().take().unwrap();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let toggle = cx.debug_bounds("settings-test-switch").unwrap();
        cx.simulate_click(toggle.center(), Modifiers::default());
        cx.read(|cx| {
            let view = view.read(cx);
            assert!(view.enabled);
            assert_eq!(view.toggles, 1);
        });
        let disabled = cx.debug_bounds("settings-test-disabled-save").unwrap();
        cx.simulate_click(
            point(disabled.left() + px(32.), disabled.top() + px(10.)),
            Modifiers::default(),
        );
        cx.read(|cx| assert_eq!(view.read(cx).saved, 0));
        let shortcut = cx.debug_bounds("settings-test-shortcut").unwrap();
        cx.simulate_click(
            point(shortcut.left() + px(32.), shortcut.top() + px(10.)),
            Modifiers::default(),
        );
        cx.simulate_keystrokes("ctrl-shift-h");
        cx.read(|cx| {
            let view = view.read(cx);
            assert_eq!(view.shortcut, "Ctrl+Shift+H");
            assert!(!view.recording);
        });
    }
}
