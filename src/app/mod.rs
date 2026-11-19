use self::{
    appearance::rem,
    input::{Changed, SearchInput},
};
use crate::plugins::{PluginResult, Plugins};
use moin::search::everything;

pub(crate) mod appearance;
mod chat;
#[cfg(windows)]
pub(crate) mod icons;
pub(crate) mod input;
pub(crate) mod launcher_window;
pub(crate) mod platform;
mod preview;
pub(crate) mod settings;
use anyhow::Result;
use gpui::{prelude::*, *};
use moin::{
    config::{AppearanceConfig, Config, RecentApp},
    search::index::{self, Hit, HitKind, IndexSummary, SearchMode},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const INK: u32 = 0xffffff;
const MUTED: u32 = 0xd2d2d2;
const LINE: u32 = 0xffffff18;
const LIME: u32 = 0xdaf48a;
const HOME_APPS: usize = 5;

#[derive(Clone)]
enum Command {
    Scope(SearchMode),
    Ai,
    Plugin(String, String),
}

#[derive(Clone, Copy)]
enum ResultAction {
    Run,
    Preview,
    Copy,
    Reveal,
    Settings,
}

actions!(
    moin,
    [
        Next,
        Previous,
        Confirm,
        Dismiss,
        Copy,
        FocusSearch,
        All,
        Apps,
        Files,
        Text,
        OpenSettings,
        Preview,
        MoreActions,
        Ai,
        NewChat,
    ]
);

pub fn bind_keys(cx: &mut App) {
    chat::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("down", Next, Some("Moin")),
        KeyBinding::new("up", Previous, Some("Moin")),
        KeyBinding::new("enter", Confirm, Some("Moin")),
        KeyBinding::new("escape", Dismiss, Some("Moin")),
        KeyBinding::new("ctrl-shift-c", Copy, Some("Moin")),
        KeyBinding::new("ctrl-l", FocusSearch, Some("Moin")),
        KeyBinding::new("ctrl-k", MoreActions, Some("Moin")),
        KeyBinding::new("ctrl-1", All, Some("Moin")),
        KeyBinding::new("ctrl-2", Apps, Some("Moin")),
        KeyBinding::new("ctrl-3", Files, Some("Moin")),
        KeyBinding::new("ctrl-4", Text, Some("Moin")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("Moin")),
        KeyBinding::new("ctrl-p", Preview, Some("Moin")),
        KeyBinding::new("ctrl-5", Ai, Some("Moin")),
        KeyBinding::new("ctrl-n", NewChat, Some("Moin")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-shift-c", Copy, Some("Moin")),
        KeyBinding::new("cmd-l", FocusSearch, Some("Moin")),
        KeyBinding::new("cmd-k", MoreActions, Some("Moin")),
        KeyBinding::new("cmd-1", All, Some("Moin")),
        KeyBinding::new("cmd-2", Apps, Some("Moin")),
        KeyBinding::new("cmd-3", Files, Some("Moin")),
        KeyBinding::new("cmd-4", Text, Some("Moin")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Moin")),
        KeyBinding::new("cmd-p", Preview, Some("Moin")),
        KeyBinding::new("cmd-5", Ai, Some("Moin")),
        KeyBinding::new("cmd-n", NewChat, Some("Moin")),
    ]);
}

pub struct Moin {
    home: PathBuf,
    db: PathBuf,
    config: Config,
    plugins: Arc<Plugins>,
    ai_config: moin::ai::AiConfig,
    ai_error: Option<String>,
    ai_notice: Option<String>,
    ai: chat::AiChat,
    input: Entity<SearchInput>,
    mode: SearchMode,
    hits: Vec<Hit>,
    selected: usize,
    summary: Option<IndexSummary>,
    indexing: bool,
    settings_window: Option<WindowHandle<settings::Settings>>,
    preview_open: bool,
    file_preview: Option<preview::FilePreview>,
    preview_key: Option<(PathBuf, Option<usize>)>,
    preview_error: Option<String>,
    preview_loading: bool,
    preview_generation: u64,
    preview_task: Option<Task<()>>,
    actions_open: bool,
    selected_action: usize,
    commands: Vec<Command>,
    result: Option<PluginResult>,
    notice: Option<String>,
    error: Option<String>,
    settings_error: Option<String>,
    everything_status: everything::Status,
    everything_error: Option<String>,
    searching: bool,
    plugin_busy: bool,
    folder_picker_open: bool,
    hotkey_hint: String,
    active_hotkey: Option<String>,
    hotkey_busy: bool,
    hotkey_error: Option<String>,
    feature_shortcuts_busy: bool,
    feature_shortcuts_error: Option<String>,
    #[cfg(any(windows, target_os = "macos"))]
    tray_controller: platform::tray::TrayController,
    scroll: ScrollHandle,
    search_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl Moin {
    pub fn new(
        home: PathBuf,
        config: Config,
        plugins: Plugins,
        #[cfg(any(windows, target_os = "macos"))] tray_controller: platform::tray::TrayController,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(SearchInput::new);
        input.read(cx).focus(window, cx);
        let subscription = cx.subscribe(&input, |view: &mut Self, _, _: &Changed, cx| {
            view.refresh(cx);
        });
        let commands = [SearchMode::Apps, SearchMode::Files, SearchMode::Text]
            .into_iter()
            .map(Command::Scope)
            .chain(std::iter::once(Command::Ai))
            .chain(
                plugins
                    .commands()
                    .into_iter()
                    .map(|(prefix, description)| Command::Plugin(prefix, description)),
            )
            .collect();
        let ai_config = config.ai.clone();
        let ai_error = None;
        let ai = chat::AiChat::new(
            cx,
            &config.appearance,
            ai_config.clone(),
            ai_error.clone(),
            home.join("index.sqlite3"),
        );
        let mut this = Self {
            db: home.join("index.sqlite3"),
            home,
            config,
            plugins: Arc::new(plugins),
            ai_config,
            ai_error,
            ai_notice: None,
            ai,
            input,
            mode: SearchMode::All,
            hits: Vec::new(),
            selected: 0,
            summary: None,
            indexing: false,
            settings_window: None,
            preview_open: false,
            file_preview: None,
            preview_key: None,
            preview_error: None,
            preview_loading: false,
            preview_generation: 0,
            preview_task: None,
            actions_open: false,
            selected_action: 0,
            commands,
            result: None,
            notice: None,
            error: None,
            settings_error: None,
            everything_status: everything::Status::Checking,
            everything_error: None,
            searching: false,
            plugin_busy: false,
            folder_picker_open: false,
            hotkey_hint: String::new(),
            active_hotkey: None,
            hotkey_busy: false,
            hotkey_error: None,
            feature_shortcuts_busy: false,
            feature_shortcuts_error: None,
            #[cfg(any(windows, target_os = "macos"))]
            tray_controller,
            scroll: ScrollHandle::new(),
            search_task: None,
            _subscription: subscription,
        };
        this.update_index(false, cx);
        this.refresh_everything_status(cx);
        this.configure_feature_shortcuts(this.config.feature_shortcuts.clone(), false, cx);
        this
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    fn composing(&self, cx: &App) -> bool {
        if self.ai.active {
            self.ai.pane.read(cx).composing(cx)
        } else {
            self.input.read(cx).is_composing()
        }
    }

    pub fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.update_index(true, cx);
    }

    fn update_index(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.indexing {
            return;
        }
        self.indexing = true;
        self.settings_error = None;
        let db = self.db.clone();
        let roots = self.config.roots.clone();
        let update = cx.background_spawn(async move {
            if force {
                index::rebuild(&db, &roots)
            } else {
                index::prepare(&db, &roots)
            }
        });
        cx.spawn(async move |view, cx| {
            let result = update.await;
            let _ = view.update(cx, |view, cx| {
                view.indexing = false;
                match result {
                    Ok(summary) => {
                        view.summary = Some(summary);
                        if !view.plugin_busy && view.result.is_none() {
                            view.refresh(cx);
                        }
                    }
                    Err(error) => {
                        let message = format!("索引更新失败 · {error:#}");
                        view.error = Some(message.clone());
                        view.settings_error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.mode == SearchMode::All {
            if let Some((mode, query)) = parse_scope_prefix(self.input.read(cx).value()) {
                self.mode = mode;
                self.input
                    .update(cx, |input, cx| input.set_value(query, cx));
                return;
            }
        }
        self.search_task.take();
        self.searching = false;
        self.plugin_busy = false;
        self.result = None;
        self.preview_open = false;
        self.clear_file_preview();
        self.actions_open = false;
        self.error = None;
        self.notice = None;
        self.selected = 0;
        self.hits.clear();
        self.scroll.set_offset(point(px(0.), px(0.)));
        let query = self.query(cx);
        if self.plugins.recognizes(&query) || self.indexing && !self.db.exists() {
            cx.notify();
            return;
        }
        self.searching = true;
        let db = self.db.clone();
        let data_dir = self.home.clone();
        let everything_enabled = cfg!(windows) && self.config.everything.enabled;
        let mode = self.mode;
        let home = query.is_empty() && mode == SearchMode::All;
        let recent = self.config.recent_apps.clone();
        let executor = cx.background_executor().clone();
        self.search_task = Some(cx.spawn(async move |view, cx| {
            executor.timer(Duration::from_millis(220)).await;
            let result = executor
                .spawn(async move {
                    if home {
                        let hits = recent
                            .into_iter()
                            .filter(|app| {
                                app.path.is_file()
                                    || cfg!(target_os = "macos")
                                        && app.path.is_dir()
                                        && app
                                            .path
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
                            })
                            .take(HOME_APPS)
                            .map(|app| Hit {
                                title: app.title,
                                path: app.path,
                                kind: HitKind::App,
                                detail: String::new(),
                                excerpt: String::new(),
                                chunk: None,
                            })
                            .collect::<Vec<_>>();
                        if !hits.is_empty() {
                            return Ok(hits);
                        }
                    }
                    let limit = if home { HOME_APPS } else { 60 };
                    if everything_enabled {
                        everything::search_with_index(&db, &data_dir, &query, mode, limit)
                    } else {
                        index::search(&db, &query, mode, limit)
                    }
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.searching = false;
                match result {
                    Ok(hits) => view.hits = hits,
                    Err(_) if view.indexing => {}
                    Err(error) => view.error = Some(format!("搜索失败 · {error:#}")),
                }
                view.update_file_preview(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn set_mode(&mut self, mode: SearchMode, window: &mut Window, cx: &mut Context<Self>) {
        self.ai.active = false;
        self.mode = mode;
        let plugin_query = self.plugins.recognizes(&self.query(cx));
        if plugin_query {
            self.input.update(cx, |input, cx| input.clear(cx));
        }
        self.input.read(cx).focus(window, cx);
        self.refresh(cx);
    }

    pub fn choose_folder(&mut self, cx: &mut Context<Self>) {
        if self.indexing || self.folder_picker_open {
            return;
        }
        self.folder_picker_open = true;
        self.settings_error = None;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("选择索引目录".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |view, cx| {
                view.folder_picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        let mut config = view.config.clone();
                        for path in paths {
                            if !config.roots.contains(&path) {
                                config.roots.push(path);
                            }
                        }
                        view.update_config(config, cx);
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        view.settings_error = Some(format!("无法选择目录 · {error:#}"));
                    }
                    Err(_) => view.settings_error = Some("目录选择器已关闭".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn update_config(&mut self, config: Config, cx: &mut Context<Self>) {
        match config.save(&self.home) {
            Ok(()) => {
                self.config = config;
                self.rebuild(cx);
            }
            Err(error) => self.settings_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    pub fn save_appearance(
        &mut self,
        appearance: AppearanceConfig,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let appearance = appearance.normalized();
        let mut config = self.config.clone();
        config.appearance = appearance.clone();
        config.save(&self.home)?;
        self.config = config;
        self.ai.sync_appearance(&appearance, cx);
        cx.notify();
        Ok(())
    }

    pub fn remove_root(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.indexing && !self.folder_picker_open && index < self.config.roots.len() {
            let mut config = self.config.clone();
            config.roots.remove(index);
            self.update_config(config, cx);
        }
    }

    pub fn settings_snapshot(&self) -> settings::Snapshot {
        settings::Snapshot {
            roots: self.config.roots.clone(),
            indexing: self.indexing,
            folder_picker_open: self.folder_picker_open,
            counts: self.summary.as_ref().map(|s| (s.apps, s.files, s.chunks)),
            vector_status: self.summary.as_ref().map_or_else(
                || "等待创建 LanceDB 向量库".into(),
                |s| {
                    format!(
                        "LanceDB · {} 个向量 · {}",
                        s.vectors,
                        s.embedding.as_deref().unwrap_or("等待更新索引")
                    )
                },
            ),
            error: self.settings_error.clone(),
            plugin_commands: self.plugins.commands(),
            plugin_warnings: self.plugins.warnings().to_vec(),
            data_dir: self.home.clone(),
            hotkey_hint: self.hotkey_hint.clone(),
            activation_hotkey: self.config.activation_hotkey.clone(),
            hotkey_busy: self.hotkey_busy,
            hotkey_error: self.hotkey_error.clone(),
            feature_shortcuts: self.config.feature_shortcuts.clone(),
            feature_shortcuts_busy: self.feature_shortcuts_busy,
            feature_shortcuts_error: self.feature_shortcuts_error.clone(),
            ai_config: self.ai_config.clone(),
            ai_error: self.ai_error.clone(),
            ai_notice: self.ai_notice.clone(),
            appearance: self.config.appearance.clone(),
            everything_enabled: self.config.everything.enabled,
            everything_status: self.everything_status.clone(),
            everything_error: self.everything_error.clone(),
        }
    }

    pub fn set_everything_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.config.everything.enabled == enabled {
            return;
        }
        let mut config = self.config.clone();
        config.everything.enabled = enabled;
        match config.save(&self.home) {
            Ok(()) => {
                self.config = config;
                self.everything_error = None;
                if enabled {
                    self.refresh_everything_status(cx);
                }
            }
            Err(error) => {
                self.everything_error = Some(format!("Everything 设置未保存 · {error:#}"));
            }
        }
        cx.notify();
    }

    pub fn refresh_everything_status(&mut self, cx: &mut Context<Self>) {
        if !cfg!(windows) {
            self.everything_status = everything::Status::Unsupported;
            cx.notify();
            return;
        }
        self.everything_status = everything::Status::Checking;
        let home = self.home.clone();
        let detect = cx.background_spawn(async move { everything::status(&home) });
        cx.spawn(async move |view, cx| {
            let status = detect.await;
            let _ = view.update(cx, |view, cx| {
                view.everything_status = status;
                view.everything_error = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn start_everything(&mut self, cx: &mut Context<Self>) {
        match everything::start() {
            Ok(_) => {
                self.everything_error = None;
                self.everything_status = everything::Status::Checking;
                let home = self.home.clone();
                let executor = cx.background_executor().clone();
                let detect = cx.background_spawn(async move {
                    executor.timer(Duration::from_millis(900)).await;
                    everything::status(&home)
                });
                cx.spawn(async move |view, cx| {
                    let status = detect.await;
                    let _ = view.update(cx, |view, cx| {
                        view.everything_status = status;
                        cx.notify();
                    });
                })
                .detach();
                cx.notify();
            }
            Err(error) => {
                self.everything_error = Some(format!("无法启动 Everything · {error:#}"));
                cx.notify();
            }
        }
    }

    pub fn save_feature_shortcuts(
        &mut self,
        shortcuts: moin::hotkey::FeatureShortcuts,
        cx: &mut Context<Self>,
    ) {
        self.configure_feature_shortcuts(shortcuts, true, cx);
    }

    fn configure_feature_shortcuts(
        &mut self,
        shortcuts: moin::hotkey::FeatureShortcuts,
        persist: bool,
        cx: &mut Context<Self>,
    ) {
        if self.hotkey_busy || self.feature_shortcuts_busy {
            return;
        }
        let shortcuts = match shortcuts.normalized(&self.config.activation_hotkey) {
            Ok(shortcuts) => shortcuts,
            Err(error) => {
                self.feature_shortcuts_error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        #[cfg(any(windows, target_os = "macos"))]
        {
            let receiver = match self.tray_controller.change_feature_hotkeys(&shortcuts) {
                Ok(receiver) => receiver,
                Err(error) => {
                    self.feature_shortcuts_error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
            };
            let previous = self.config.feature_shortcuts.clone();
            self.feature_shortcuts_busy = true;
            self.feature_shortcuts_error = None;
            cx.spawn(async move |view, cx| {
                let result = receiver.recv().await;
                let rollback = view
                    .update(cx, |view, cx| {
                        match result {
                            Ok(Ok(())) => {
                                if persist {
                                    let mut config = view.config.clone();
                                    config.feature_shortcuts = shortcuts;
                                    match config.save(&view.home) {
                                        Ok(()) => view.config = config,
                                        Err(error) => {
                                            view.feature_shortcuts_error =
                                                Some(format!("功能快捷键未保存 · {error:#}"));
                                            cx.notify();
                                            return Some(
                                                view.tray_controller
                                                    .change_feature_hotkeys(&previous),
                                            );
                                        }
                                    }
                                }
                            }
                            Ok(Err(error)) => {
                                view.feature_shortcuts_error =
                                    Some(format!("功能快捷键未启用 · {error:#}"))
                            }
                            Err(_) => {
                                view.feature_shortcuts_error = Some("功能快捷键服务已停止".into())
                            }
                        }
                        view.feature_shortcuts_busy = false;
                        cx.notify();
                        None
                    })
                    .ok()
                    .flatten();
                if let Some(rollback) = rollback {
                    let error = match rollback {
                        Ok(receiver) => match receiver.recv().await {
                            Ok(Ok(())) => None,
                            Ok(Err(error)) => Some(error.to_string()),
                            Err(error) => Some(error.to_string()),
                        },
                        Err(error) => Some(error.to_string()),
                    };
                    let _ = view.update(cx, |view, cx| {
                        if let Some(error) = error {
                            let original = view.feature_shortcuts_error.take().unwrap_or_default();
                            view.feature_shortcuts_error =
                                Some(format!("{original}。恢复旧功能快捷键失败 · {error}"));
                        }
                        view.feature_shortcuts_busy = false;
                        cx.notify();
                    });
                }
            })
            .detach();
            cx.notify();
        }
    }

    fn activate_feature(
        &mut self,
        feature: moin::hotkey::FeatureAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use moin::hotkey::FeatureAction;
        self.actions_open = false;
        match feature {
            FeatureAction::All
            | FeatureAction::Apps
            | FeatureAction::Files
            | FeatureAction::Text => {
                let mode = match feature {
                    FeatureAction::Apps => SearchMode::Apps,
                    FeatureAction::Files => SearchMode::Files,
                    FeatureAction::Text => SearchMode::Text,
                    _ => SearchMode::All,
                };
                self.input.update(cx, |input, cx| input.clear(cx));
                self.set_mode(mode, window, cx);
            }
            FeatureAction::Ai => self.set_ai(true, window, cx),
            FeatureAction::Settings => {
                self.open_settings(window, cx);
                return;
            }
            _ => {
                self.set_mode(SearchMode::All, window, cx);
                if let Some(command) = feature.command() {
                    self.tool(command, window, cx);
                }
            }
        }
        launcher_window::show(window);
        self.focus_input(window, cx);
        cx.notify();
    }

    pub fn save_activation_hotkey(&mut self, value: String, cx: &mut Context<Self>) {
        if self.feature_shortcuts_busy {
            return;
        }
        if let Err(error) = self.config.feature_shortcuts.normalized(&value) {
            self.hotkey_error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            if self.hotkey_busy {
                return;
            }
            let receiver = match self.tray_controller.change_hotkey(&value) {
                Ok(receiver) => receiver,
                Err(error) => {
                    self.hotkey_error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
            };
            let previous = self.active_hotkey.clone();
            self.hotkey_busy = true;
            self.hotkey_error = None;
            cx.spawn(async move |view, cx| {
                let result = receiver.recv().await;
                let rollback = view
                    .update(cx, |view, cx| match result {
                        Ok(Ok(label)) => {
                            let mut config = view.config.clone();
                            config.activation_hotkey = label;
                            match config.save(&view.home) {
                                Ok(()) => {
                                    view.config = config;
                                    view.hotkey_busy = false;
                                    cx.notify();
                                    return None;
                                }
                                Err(error) => {
                                    view.hotkey_error = Some(format!("快捷键未保存 · {error:#}"))
                                }
                            }
                            let rollback = match previous {
                                Some(label) => view.tray_controller.change_hotkey(&label),
                                None => view.tray_controller.disable_hotkey(),
                            };
                            cx.notify();
                            Some(rollback)
                        }
                        Ok(Err(error)) => {
                            view.hotkey_error = Some(format!("{error:#}"));
                            view.hotkey_busy = false;
                            cx.notify();
                            None
                        }
                        Err(_) => {
                            view.hotkey_error = Some("快捷键服务已停止".into());
                            view.hotkey_busy = false;
                            cx.notify();
                            None
                        }
                    })
                    .ok()
                    .flatten();
                if let Some(rollback) = rollback {
                    let error = match rollback {
                        Ok(receiver) => match receiver.recv().await {
                            Ok(Ok(_)) => None,
                            Ok(Err(error)) => Some(error.to_string()),
                            Err(error) => Some(error.to_string()),
                        },
                        Err(error) => Some(error.to_string()),
                    };
                    let _ = view.update(cx, |view, cx| {
                        if let Some(error) = error {
                            let previous = view.hotkey_error.take().unwrap_or_default();
                            view.hotkey_error =
                                Some(format!("{previous}。恢复旧快捷键失败 · {error}"));
                        }
                        view.hotkey_busy = false;
                        cx.notify();
                    });
                }
            })
            .detach();
            cx.notify();
        }
    }

    fn showing_commands(&self, cx: &App) -> bool {
        self.mode == SearchMode::All && self.input.read(cx).value().trim().is_empty()
    }

    fn clear_file_preview(&mut self) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.preview_task.take();
        self.preview_key = None;
        self.file_preview = None;
        self.preview_error = None;
        self.preview_loading = false;
    }

    fn update_file_preview(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self
            .hits
            .get(self.selected)
            .filter(|hit| hit.kind != HitKind::App)
            .cloned()
        else {
            self.clear_file_preview();
            return;
        };
        let key = (hit.path.clone(), hit.chunk);
        if self.preview_key.as_ref() == Some(&key) {
            return;
        }
        self.clear_file_preview();
        self.preview_key = Some(key);
        self.preview_loading = true;
        let generation = self.preview_generation;
        let executor = cx.background_executor().clone();
        self.preview_task = Some(cx.spawn(async move |view, cx| {
            // Fast arrow navigation need not open each file along the way.
            executor.timer(Duration::from_millis(90)).await;
            let result = executor.spawn(async move { preview::load(&hit) }).await;
            let _ = view.update(cx, |view, cx| {
                if view.preview_generation != generation {
                    return;
                }
                view.preview_loading = false;
                match result {
                    Ok(preview) => view.file_preview = Some(preview),
                    Err(error) => view.preview_error = Some(format!("无法预览 · {error:#}")),
                }
                cx.notify();
            });
        }));
    }

    fn result_row(&self, selected: usize, cx: &App) -> usize {
        selected
            + 1
            + usize::from(
                self.showing_commands(cx) && !self.hits.is_empty() && selected >= self.hits.len(),
            )
    }

    fn select_result(&mut self, index: usize, cx: &mut Context<Self>) {
        let len = self.hits.len()
            + if self.showing_commands(cx) {
                self.commands.len()
            } else {
                0
            };
        if len == 0 {
            return;
        }
        self.selected = index.min(len - 1);
        self.scroll
            .scroll_to_item(self.result_row(self.selected, cx));
        self.notice = None;
        self.update_file_preview(cx);
        if self.preview_key.is_none() {
            self.preview_open = false;
        }
        cx.notify();
    }

    fn select_visible_result(
        &mut self,
        offset: usize,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.hits.len()
            + if self.showing_commands(cx) {
                self.commands.len()
            } else {
                0
            };
        let top = self.scroll.top_item();
        let bottom = self.scroll.bottom_item();
        if let Some(index) = (0..len)
            .filter(|index| {
                let row = self.result_row(*index, cx);
                row >= top && row <= bottom
            })
            .nth(offset)
        {
            self.select_result(index, cx);
            if run {
                self.confirm(window, cx);
            }
        }
    }

    fn return_to_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.active || self.composing(cx) {
            return;
        }
        if self.mode != SearchMode::All {
            self.set_mode(SearchMode::All, window, cx);
        } else if self.plugins.recognizes(&self.query(cx)) {
            self.input.update(cx, |input, cx| input.clear(cx));
        }
    }

    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.active || self.composing(cx) {
            return;
        }
        let stroke = &event.keystroke;
        if stroke.modifiers.alt && !stroke.modifiers.control && !stroke.modifiers.platform {
            if self.actions_open {
                return;
            }
            if let Some(number) = result_number(&stroke.key) {
                self.select_visible_result(number - 1, !stroke.modifiers.shift, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if stroke.modifiers.modified()
            && !(stroke.key == "tab"
                && stroke.modifiers.shift
                && !stroke.modifiers.control
                && !stroke.modifiers.alt
                && !stroke.modifiers.platform
                && !stroke.modifiers.function)
        {
            return;
        }
        match stroke.key.as_str() {
            "pageup" => {
                self.move_selection(-7, cx);
                cx.stop_propagation();
            }
            "pagedown" => {
                self.move_selection(7, cx);
                cx.stop_propagation();
            }
            "tab" => {
                self.move_selection(if stroke.modifiers.shift { -1 } else { 1 }, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.ai.active {
            return;
        }
        if self.actions_open {
            self.selected_action = self
                .selected_action
                .saturating_add_signed(delta)
                .min(self.result_actions(cx).len().saturating_sub(1));
            cx.notify();
            return;
        }
        let home = self.showing_commands(cx);
        let len = self.hits.len() + if home { self.commands.len() } else { 0 };
        if len == 0 || self.composing(cx) {
            return;
        }
        self.select_result(self.selected.saturating_add_signed(delta), cx);
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.active {
            // Enter on a Markdown control must not submit the composer as well.
            self.ai.pane.update(cx, |pane, cx| {
                if pane.input_focused(window, cx) {
                    pane.send_ai(window, cx);
                }
            });
            return;
        }
        if self.actions_open {
            if let Some((action, _, _, _)) =
                self.result_actions(cx).get(self.selected_action).copied()
            {
                self.perform_action(action, window, cx);
            }
            return;
        }
        if self.plugin_busy || self.composing(cx) {
            return;
        }
        let query = self.query(cx);
        if self.plugins.recognizes(&query) {
            self.result = None;
            self.error = None;
            self.notice = None;
            self.plugin_busy = true;
            let plugins = self.plugins.clone();
            let running = cx.background_spawn(async move { plugins.run(&query) });
            self.search_task = Some(cx.spawn(async move |view, cx| {
                let result = running.await;
                let _ = view.update(cx, |view, cx| {
                    view.plugin_busy = false;
                    match result {
                        Ok(result) => view.result = result,
                        Err(error) => view.error = Some(format!("{error:#}")),
                    }
                    cx.notify();
                });
            }));
            cx.notify();
        } else if self.showing_commands(cx) && self.selected >= self.hits.len() {
            self.execute_command(self.selected - self.hits.len(), window, cx);
        } else if !self.searching {
            self.open_selected(window, cx);
        }
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hit) = self.hits.get(self.selected) {
            match open_path(&hit.path) {
                Ok(()) => {
                    if hit.kind == HitKind::App {
                        self.config.recent_apps.retain(|app| app.path != hit.path);
                        self.config.recent_apps.insert(
                            0,
                            RecentApp {
                                title: hit.title.clone(),
                                path: hit.path.clone(),
                            },
                        );
                        self.config.recent_apps.truncate(12);
                        if let Err(error) = self.config.save(&self.home) {
                            self.settings_error = Some(format!("{error:#}"));
                        }
                    }
                    launcher_window::hide(window);
                    self.input.update(cx, |input, cx| input.clear(cx));
                    self.refresh(cx);
                }
                Err(error) => self.error = Some(format!("无法打开 · {error:#}")),
            }
            cx.notify();
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.ai.active {
            self.copy_ai(cx);
            return;
        }
        if self.composing(cx) {
            return;
        }
        let value = self
            .result
            .as_ref()
            .map(|result| result.output.clone())
            .or_else(|| {
                self.hits.get(self.selected).map(|hit| match hit.chunk {
                    Some(chunk) => format!(
                        "{}\n{} · 片段 {}\n\n{}",
                        hit.title,
                        hit.path.display(),
                        chunk,
                        hit.excerpt
                    ),
                    None => hit.path.to_string_lossy().into_owned(),
                })
            });
        if let Some(value) = value {
            self.actions_open = false;
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.notice = Some("已复制到剪贴板".into());
            cx.notify();
        }
    }

    fn tool(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(format!("{command} "), cx);
        });
        self.input.read(cx).focus(window, cx);
        self.refresh(cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.active {
            if self.composing(cx) {
                return;
            }
            if self.ai.pane.read(cx).busy() {
                self.stop_ai(cx);
            } else {
                self.set_ai(false, window, cx);
            }
            return;
        }
        if self.actions_open {
            self.actions_open = false;
            cx.notify();
            return;
        }
        if self.composing(cx) {
            return;
        }
        if self.preview_open {
            self.preview_open = false;
            cx.notify();
            return;
        }
        let query = self.query(cx);
        if !query.is_empty() {
            let replacement = if self.plugins.recognizes(&query) {
                let prefix = if query.starts_with('=') {
                    "="
                } else {
                    query.split_whitespace().next().unwrap_or_default()
                };
                if query == prefix {
                    String::new()
                } else {
                    format!("{prefix} ")
                }
            } else {
                String::new()
            };
            self.input
                .update(cx, |input, cx| input.set_value(replacement, cx));
            self.input.read(cx).focus(window, cx);
            return;
        }
        if self.mode != SearchMode::All {
            self.set_mode(SearchMode::All, window, cx);
            return;
        }
        if !self.composing(cx) && !self.folder_picker_open {
            launcher_window::hide(window);
        }
    }

    fn result_actions(
        &self,
        cx: &App,
    ) -> Vec<(ResultAction, &'static str, &'static str, &'static str)> {
        let mut actions = Vec::new();
        if self.plugins.recognizes(&self.query(cx)) {
            actions.push((ResultAction::Run, "运行命令", "\u{e768}", "Enter"));
            if self.result.is_some() {
                actions.push((ResultAction::Copy, "复制结果", "\u{e8c8}", "Ctrl+Shift+C"));
            }
        } else if let Some(hit) = self.hits.get(self.selected) {
            actions.push((ResultAction::Run, "打开", "\u{e8a7}", "Enter"));
            if hit.kind != HitKind::App {
                actions.push((ResultAction::Preview, "展开预览", "\u{e8a5}", "Ctrl+P"));
            }
            actions.push((
                ResultAction::Copy,
                if hit.chunk.is_some() {
                    "复制片段"
                } else {
                    "复制路径"
                },
                "\u{e8c8}",
                "Ctrl+Shift+C",
            ));
            actions.push((ResultAction::Reveal, "打开所在文件夹", "\u{e838}", ""));
        } else if self.showing_commands(cx) {
            actions.push((ResultAction::Run, "运行命令", "\u{e768}", "Enter"));
        }
        actions.push((ResultAction::Settings, "打开设置", "\u{e713}", "Ctrl+,"));
        actions
    }

    fn toggle_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai.active {
            return;
        }
        self.actions_open = !self.actions_open;
        self.selected_action = 0;
        self.input.read(cx).focus(window, cx);
        cx.notify();
    }

    fn perform_action(
        &mut self,
        action: ResultAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.actions_open = false;
        match action {
            ResultAction::Run => self.confirm(window, cx),
            ResultAction::Preview => self.toggle_preview(cx),
            ResultAction::Copy => self.copy(cx),
            ResultAction::Reveal => {
                if let Some(parent) = self
                    .hits
                    .get(self.selected)
                    .and_then(|hit| hit.path.parent())
                {
                    match open_path(parent) {
                        Ok(()) => self.notice = Some("已打开所在文件夹".into()),
                        Err(error) => self.error = Some(format!("{error:#}")),
                    }
                }
            }
            ResultAction::Settings => self.open_settings(window, cx),
        }
        cx.notify();
    }

    fn actions_menu(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("result-actions")
            .absolute()
            .right(px(12.))
            .bottom(px(58.))
            .w(px(310.))
            .p_1()
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(0xffffff24))
            .bg(rgb(0x292929))
            .shadow_lg()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(rem(12.))
                    .text_color(rgb(MUTED))
                    .child("操作"),
            )
            .children(self.result_actions(cx).into_iter().enumerate().map(
                |(i, (action, label, icon, key))| {
                    row()
                        .id(("result-action", i))
                        .h(rem(40.))
                        .px_3()
                        .gap_3()
                        .rounded(px(4.))
                        .when(i == self.selected_action, |row| row.bg(rgba(0xffffff12)))
                        .hover(|style| style.bg(rgba(0xffffff1a)))
                        .cursor_pointer()
                        .child(glyph(icon, 14., INK))
                        .child(div().flex_1().text_size(rem(14.)).child(label))
                        .when(!key.is_empty(), |row| row.child(keycap(key)))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.perform_action(action, window, cx)
                        }))
                },
            ))
    }

    fn execute_command(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.commands.get(index).cloned() {
            Some(Command::Scope(mode)) => self.set_mode(mode, window, cx),
            Some(Command::Ai) => self.set_ai(true, window, cx),
            Some(Command::Plugin(prefix, _)) => self.tool(&prefix, window, cx),
            None => {}
        }
    }

    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        if self.ai.active {
            return;
        }
        self.actions_open = false;
        if self
            .hits
            .get(self.selected)
            .is_some_and(|hit| hit.kind != HitKind::App)
        {
            self.preview_open = !self.preview_open;
            cx.notify();
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.actions_open = false;
        let Some(launcher) = window.window_handle().downcast::<Self>() else {
            return;
        };
        let settings_window = self.settings_window;
        cx.defer(move |cx| {
            if let Some(handle) = settings_window
                && handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                let _ = launcher.update(cx, |_, window, _| launcher_window::hide(window));
                return;
            }
            let result = settings::open(launcher, cx);
            let _ = launcher.update(cx, |view, window, cx| {
                match result {
                    Ok(handle) => {
                        view.settings_window = Some(handle);
                        launcher_window::hide(window);
                    }
                    Err(error) => view.error = Some(format!("无法打开设置 · {error:#}")),
                }
                cx.notify();
            });
        });
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let has_query = !self.input.read(cx).value().is_empty();
        row()
            .h(rem(72.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgba(LINE))
            .px(px(24.))
            .gap(px(20.))
            .child(mark())
            .child(
                div()
                    .text_size(rem(20.))
                    .line_height(rem(34.))
                    .flex_1()
                    .min_w_0()
                    .child(self.input.clone()),
            )
            .when(self.mode != SearchMode::All, |header| {
                header.child(
                    control("scope", mode_label(self.mode))
                        .child(keycap("×"))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.set_mode(SearchMode::All, window, cx);
                        })),
                )
            })
            .when(has_query, |header| {
                header.child(
                    control("clear", "×").on_click(cx.listener(|view, _, window, cx| {
                        view.input.update(cx, |input, cx| input.clear(cx));
                        view.input.read(cx).focus(window, cx);
                    })),
                )
            })
            .child(
                control("ai-mode", "AI 对话")
                    .child(keycap(
                        self.configured_command_key(&Command::Ai)
                            .unwrap_or("Ctrl+5".into()),
                    ))
                    .on_click(cx.listener(|view, _, window, cx| view.set_ai(true, window, cx))),
            )
    }

    fn configured_command_key(&self, command: &Command) -> Option<String> {
        use moin::hotkey::FeatureAction;
        let feature = match command {
            Command::Scope(SearchMode::Apps) => FeatureAction::Apps,
            Command::Scope(SearchMode::Files) => FeatureAction::Files,
            Command::Scope(SearchMode::Text) => FeatureAction::Text,
            Command::Scope(SearchMode::All) => FeatureAction::All,
            Command::Ai => FeatureAction::Ai,
            Command::Plugin(prefix, _) => match prefix.as_str() {
                "=" => FeatureAction::Calculator,
                "翻译" => FeatureAction::Translation,
                "词典" => FeatureAction::Dictionary,
                "换算" => FeatureAction::UnitConverter,
                _ => return None,
            },
        };
        let key = self.config.feature_shortcuts.get(feature);
        (!key.is_empty()).then(|| {
            // A configured global Control chord must not be rendered as Command.
            // The legacy keycap conversion still maps in-app Ctrl fallbacks to Cmd.
            if cfg!(target_os = "macos") {
                key.replace("Ctrl", "⌃")
                    .replace("Cmd", "⌘")
                    .replace("Alt", "⌥")
            } else {
                key.to_owned()
            }
        })
    }

    fn listing(&self, query: &str, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let home = self.showing_commands(cx);
        let mut list = div()
            .id("entries")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(8.))
            .px(px(8.))
            .track_scroll(&self.scroll);
        if !self.hits.is_empty() {
            let label = if home {
                if self.hits.first().is_some_and(|hit| {
                    self.config
                        .recent_apps
                        .iter()
                        .any(|app| app.path == hit.path)
                }) {
                    "最近使用"
                } else {
                    "应用程序"
                }
            } else if query.is_empty() {
                mode_label(self.mode)
            } else {
                "搜索结果"
            };
            list = list.child(section_label(label));
            for (i, hit) in self.hits.iter().enumerate() {
                let (glyph, label, color) = hit_style(hit.kind);
                let detail = if self.preview_key.is_some() {
                    String::new()
                } else if hit.kind == HitKind::Text {
                    collapse(&hit.excerpt)
                } else if matches!(hit.kind, HitKind::File | HitKind::Folder) {
                    hit.path
                        .parent()
                        .map(|parent| parent.display().to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let symbol = hit_icon(hit, window, cx)
                    .unwrap_or_else(|| glyph_tile(glyph, color).into_any_element());
                let active = self.selected == i;
                list = list.child(
                    entry(("hit", i), symbol, hit.title.clone(), detail, active)
                        .when(!home, |entry| entry.child(kind_label(label)))
                        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                            view.select_result(i, cx);
                            if view.hits.get(i).is_some_and(|hit| hit.kind == HitKind::App)
                                || event.click_count() >= 2
                            {
                                view.open_selected(window, cx);
                            }
                            view.input.read(cx).focus(window, cx);
                        })),
                );
            }
        }
        if home {
            list = list.child(section_label("命令"));
            for (i, command) in self.commands.iter().enumerate() {
                let (title, detail, glyph, color, key) = match command {
                    Command::Scope(SearchMode::Apps) => (
                        "搜索应用".into(),
                        String::new(),
                        "\u{e71d}",
                        0xa8cff5,
                        Some("Ctrl+2"),
                    ),
                    Command::Scope(SearchMode::Files) => (
                        "搜索文件".into(),
                        String::new(),
                        "\u{e8b7}",
                        0xffcd8a,
                        Some("Ctrl+3"),
                    ),
                    Command::Scope(_) => (
                        "搜索正文".into(),
                        String::new(),
                        "\u{e8a5}",
                        0xc8dffc,
                        Some("Ctrl+4"),
                    ),
                    Command::Ai => (
                        "与 Moin 对话".into(),
                        "AI 助手".into(),
                        "\u{e8f2}",
                        LIME,
                        Some("Ctrl+5"),
                    ),
                    Command::Plugin(prefix, description) => {
                        let (title, explanation) =
                            description.split_once(" · ").unwrap_or((description, ""));
                        let (glyph, color) = match prefix.as_str() {
                            "=" => ("\u{e8ef}", 0xdaf48a),
                            "翻译" => ("\u{e8c1}", 0x9bd4ff),
                            "词典" => ("\u{e82d}", 0xb8efdb),
                            "换算" => ("\u{ed5e}", 0xffd18f),
                            _ => ("\u{e74c}", 0xafe0ec),
                        };
                        (title.to_owned(), explanation.to_owned(), glyph, color, None)
                    }
                };
                let key = self
                    .configured_command_key(command)
                    .or_else(|| key.map(str::to_owned));
                list = list.child(
                    entry(
                        ("command", i),
                        glyph_tile(glyph, color).into_any_element(),
                        title,
                        detail,
                        self.selected == self.hits.len() + i,
                    )
                    .when_some(key, |entry, key| entry.child(keycap(key)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.execute_command(i, window, cx);
                    })),
                );
            }
        } else if self.hits.is_empty() {
            let (title, detail) = if self.searching || self.indexing {
                ("正在搜索", "正在读取本地索引")
            } else if query.is_empty() && self.mode == SearchMode::Text {
                ("搜索文件正文", "输入关键词，查找并预览原文片段")
            } else {
                ("没有匹配的结果", "试试更短的关键词，或到设置中检查索引目录")
            };
            list = list.child(message(title, detail));
        }
        list
    }

    fn search_results(&self, query: &str, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(self.listing(query, window, cx))
            .when(self.preview_key.is_some(), |body| {
                body.child(
                    div()
                        .w(relative(0.42))
                        .min_w(px(280.))
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .border_l_1()
                        .border_color(rgba(LINE))
                        .bg(rgba(0x00000012))
                        .child(self.file_preview_panel(cx)),
                )
            })
    }

    fn file_preview_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut pane = div()
            .id("file-preview")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(18.))
            .flex()
            .flex_col()
            .gap(px(12.));
        pane = pane.child(
            row()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(rem(12.))
                        .text_color(rgb(MUTED))
                        .child("文件预览"),
                )
                .child(
                    control(
                        "expand-file-preview",
                        if self.preview_open {
                            "返回结果"
                        } else {
                            "展开"
                        },
                    )
                    .text_size(rem(12.))
                    .child(keycap("Ctrl+P"))
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_preview(cx))),
                ),
        );
        if self.preview_loading {
            return pane.child(
                div()
                    .text_size(rem(14.))
                    .text_color(rgb(MUTED))
                    .child("正在读取预览…"),
            );
        }
        if let Some(error) = &self.preview_error {
            return pane.child(
                div()
                    .text_size(rem(14.))
                    .line_height(rem(22.))
                    .text_color(rgb(0xffb4a5))
                    .child(error.clone()),
            );
        }
        let Some(preview) = &self.file_preview else {
            return pane;
        };
        pane = pane
            .child(
                div()
                    .text_size(rem(18.))
                    .line_height(rem(27.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(preview.title.clone()),
            )
            .child(
                div()
                    .text_size(rem(12.))
                    .line_height(rem(19.))
                    .text_color(rgb(MUTED))
                    .child(preview.path.display().to_string()),
            );
        let metadata = [
            Some(preview.kind.clone()),
            preview.size.map(preview::size_label),
            preview.modified.map(preview::modified_label),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        pane = pane.child(
            div()
                .text_size(rem(12.))
                .line_height(rem(19.))
                .text_color(rgb(MUTED))
                .child(metadata),
        );
        match &preview.content {
            preview::Content::Text {
                text,
                truncated,
                chunk,
            } => {
                if let Some(chunk) = chunk {
                    pane = pane.child(
                        div()
                            .text_size(rem(12.))
                            .text_color(rgb(LIME))
                            .child(format!("命中片段 {chunk}")),
                    );
                }
                pane = pane.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .p(px(12.))
                        .bg(rgba(0x00000024))
                        .text_size(rem(13.))
                        .line_height(rem(22.))
                        .child(if text.is_empty() {
                            "文件为空".to_owned()
                        } else {
                            text.clone()
                        }),
                );
                if *truncated {
                    pane = pane.child(
                        div()
                            .text_size(rem(12.))
                            .text_color(rgb(MUTED))
                            .child("仅显示文件开头，按 Enter 打开完整内容。"),
                    );
                }
            }
            preview::Content::Image { width, height } => {
                pane = pane
                    .child(
                        div()
                            .text_size(rem(12.))
                            .text_color(rgb(MUTED))
                            .child(format!("{width} × {height}")),
                    )
                    .child(
                        img(preview.path.clone())
                            .w_full()
                            .h(px(230.))
                            .object_fit(ObjectFit::Contain)
                            .with_loading(|| {
                                div()
                                    .text_size(rem(13.))
                                    .child("正在加载图片…")
                                    .into_any_element()
                            })
                            .with_fallback(|| {
                                div()
                                    .text_size(rem(13.))
                                    .child("图片无法加载，按 Enter 用默认应用打开。")
                                    .into_any_element()
                            }),
                    );
            }
            preview::Content::Directory { entries, truncated } => {
                if entries.is_empty() {
                    pane = pane.child(
                        div()
                            .text_size(rem(13.))
                            .text_color(rgb(MUTED))
                            .child("文件夹为空"),
                    );
                }
                for (index, (name, directory)) in entries.iter().enumerate() {
                    pane = pane.child(
                        row()
                            .gap(px(8.))
                            .min_h(rem(24.))
                            .min_w_0()
                            .child(glyph(
                                if *directory { "\u{e8b7}" } else { "\u{e8a5}" },
                                14.,
                                MUTED,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(rem(13.))
                                    .child(name.clone()),
                            )
                            .id(("folder-preview-entry", index)),
                    );
                }
                if *truncated {
                    pane = pane.child(
                        div()
                            .text_size(rem(12.))
                            .text_color(rgb(MUTED))
                            .child("最多显示前 80 项，按 Enter 打开文件夹。"),
                    );
                }
            }
            preview::Content::Metadata(message) => {
                pane = pane.child(
                    div()
                        .text_size(rem(13.))
                        .line_height(rem(22.))
                        .text_color(rgb(MUTED))
                        .child(message.clone()),
                );
            }
        }
        pane
    }

    fn plugin_view(&self, query: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        let example = match query.split_whitespace().next() {
            Some("=") => Some("= sqrt(144) + 12 * 3"),
            Some("dict" | "词典") => Some("词典 rust"),
            Some("tr" | "翻译") => Some("翻译 hello"),
            Some("convert" | "换算") => Some("换算 12 km mi"),
            _ => None,
        };
        let pane = div()
            .id("plugin-result")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(26.))
            .py(px(18.));
        if let Some(result) = &self.result {
            return pane
                .child(
                    div()
                        .text_size(rem(15.))
                        .text_color(rgb(MUTED))
                        .child(result.title.clone()),
                )
                .child(
                    div()
                        .mt_3()
                        .text_size(rem(26.))
                        .line_height(rem(37.))
                        .child(result.output.clone()),
                )
                .child(
                    div()
                        .mt_3()
                        .text_size(rem(14.))
                        .text_color(rgb(MUTED))
                        .child(result.detail.clone()),
                );
        }
        let detail = self.error.as_deref().unwrap_or(if self.plugin_busy {
            "正在执行命令…"
        } else {
            "在搜索框填写参数，按 Enter 运行"
        });
        pane.child(message("插件命令", detail))
            .when_some(example, |pane, example| {
                pane.child(
                    div()
                        .px_4()
                        .child(control("plugin-example", example).on_click(cx.listener(
                            move |view, _, window, cx| {
                                view.input
                                    .update(cx, |input, cx| input.set_value(example.into(), cx));
                                view.input.read(cx).focus(window, cx);
                            },
                        ))),
                )
            })
    }

    fn footer(&self, plugin: bool, cx: &mut Context<Self>) -> Div {
        let has_result = self.hits.get(self.selected).is_some();
        let can_open = has_result || self.showing_commands(cx) || plugin;
        let copy_output = plugin && self.result.is_some();
        let feedback = self
            .error
            .as_ref()
            .or(self.notice.as_ref())
            .cloned()
            .unwrap_or_default();
        row()
            .h(rem(48.))
            .flex_shrink_0()
            .px(px(14.))
            .gap(px(16.))
            .border_t_1()
            .border_color(rgba(LINE))
            .bg(rgba(0x00000022))
            .child(
                control("settings", "设置")
                    .child(glyph("\u{e713}", 18., INK))
                    .flex_row_reverse()
                    .on_click(cx.listener(|view, _, window, cx| view.open_settings(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(rem(12.))
                    .text_color(rgb(if self.error.is_some() {
                        0xffb4a5
                    } else {
                        MUTED
                    }))
                    .child(feedback),
            )
            .when(copy_output, |bar| {
                bar.child(
                    control("copy-output", "复制")
                        .child(keycap("Ctrl+Shift+C"))
                        .on_click(cx.listener(|view, _, _, cx| view.copy(cx))),
                )
            })
            .when(can_open, |bar| {
                bar.child(
                    control(
                        "run-button",
                        if has_result && !plugin {
                            "打开"
                        } else {
                            "运行"
                        },
                    )
                    .child(keycap("↵"))
                    .on_click(cx.listener(|view, _, window, cx| view.confirm(window, cx))),
                )
            })
            .child(
                control("more-actions", "更多")
                    .child(keycap("Ctrl"))
                    .child(keycap("K"))
                    .when(self.actions_open, |button| button.bg(rgba(0xffffff14)))
                    .on_click(cx.listener(|view, _, window, cx| view.toggle_actions(window, cx))),
            )
    }
}

impl Render for Moin {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.config.appearance.ui_font_size));
        let query = self.query(cx);
        let plugin = self.plugins.recognizes(&query);
        let body = if self.ai.active {
            self.ai.pane.clone().into_any_element()
        } else if plugin {
            self.plugin_view(&query, cx).into_any_element()
        } else if self.preview_open && self.preview_key.is_some() {
            self.file_preview_panel(cx).into_any_element()
        } else {
            self.search_results(&query, window, cx).into_any_element()
        };
        div()
            .key_context("Moin")
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.palette_key(event, window, cx)
            }))
            .on_action(cx.listener(|view, _: &input::Backspace, window, cx| {
                if !view.ai.active && view.query(cx).is_empty() {
                    view.return_to_parent(window, cx);
                }
            }))
            .on_action(cx.listener(|view, _: &Next, _, cx| view.move_selection(1, cx)))
            .on_action(cx.listener(|view, _: &Previous, _, cx| view.move_selection(-1, cx)))
            .on_action(cx.listener(|view, _: &Confirm, window, cx| view.confirm(window, cx)))
            .on_action(cx.listener(|view, _: &Dismiss, window, cx| view.dismiss(window, cx)))
            .on_action(cx.listener(|view, _: &Copy, _, cx| view.copy(cx)))
            .on_action(cx.listener(|view, _: &Preview, _, cx| view.toggle_preview(cx)))
            .on_action(
                cx.listener(|view, _: &MoreActions, window, cx| view.toggle_actions(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &OpenSettings, window, cx| view.open_settings(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &FocusSearch, window, cx| view.focus_input(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &Ai, window, cx| view.set_ai(!view.ai.active, window, cx)),
            )
            .on_action(cx.listener(|view, _: &NewChat, window, cx| {
                if view.ai.active {
                    view.new_ai_chat(window, cx);
                }
            }))
            .on_action(
                cx.listener(|view, _: &All, window, cx| view.set_mode(SearchMode::All, window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &Apps, window, cx| {
                    view.set_mode(SearchMode::Apps, window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &Files, window, cx| {
                view.set_mode(SearchMode::Files, window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &Text, window, cx| {
                    view.set_mode(SearchMode::Text, window, cx)
                }),
            )
            .size_full()
            .relative()
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(0xffffff2e))
            .bg(rgba(0x202020a8))
            .text_color(rgb(INK))
            .font_family(appearance::resolve_family(
                &self.config.appearance.ui_font_family,
            ))
            .text_size(rem(16.))
            .flex()
            .flex_col()
            .when(!self.ai.active, |root| root.child(self.header(cx)))
            .child(body)
            .when(!self.ai.active, |root| root.child(self.footer(plugin, cx)))
            .when(self.actions_open, |root| {
                root.child(
                    div()
                        .id("actions-dismiss")
                        .absolute()
                        .inset_0()
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.actions_open = false;
                            cx.notify();
                        })),
                )
                .child(self.actions_menu(cx))
            })
    }
}

fn row() -> Div {
    div().flex().items_center()
}

/// Command Palette aliases enter a search page only once the delimiter is typed.
fn parse_scope_prefix(value: &str) -> Option<(SearchMode, String)> {
    let value = value.trim_start();
    let end = value.find(char::is_whitespace)?;
    let mode = match value[..end].to_ascii_lowercase().as_str() {
        "file" | "files" | "文件" => SearchMode::Files,
        "app" | "apps" | "应用" => SearchMode::Apps,
        "text" | "正文" => SearchMode::Text,
        _ => return None,
    };
    Some((mode, value[end..].trim_start().to_owned()))
}

fn result_number(key: &str) -> Option<usize> {
    if let Ok(number @ 1..=9) = key.parse::<usize>() {
        return Some(number);
    }
    // GPUI reports the printable character for shifted number-row keys.
    key.chars()
        .next()
        .filter(|_| key.chars().count() == 1)
        .and_then(|key| "!@#$%^&*(".chars().position(|shifted| shifted == key))
        .map(|index| index + 1)
}

fn glyph(icon: &'static str, size: f32, color: u32) -> Div {
    appearance::icon(icon, size, color)
}

fn mark() -> Div {
    let bar = |height| {
        div()
            .w(px(3.))
            .h(px(height))
            .rounded(px(1.5))
            .bg(rgb(0x132e2a))
    };
    row()
        .size(px(24.))
        .flex_shrink_0()
        .rounded(px(5.))
        .bg(rgb(LIME))
        .justify_center()
        .gap(px(2.5))
        .child(bar(10.))
        .child(bar(17.))
        .child(bar(13.))
}

fn glyph_tile(icon: &'static str, color: u32) -> Div {
    row()
        .size(px(20.))
        .flex_shrink_0()
        .rounded(px(6.))
        .bg(rgba(0xffffff08))
        .justify_center()
        .child(glyph(icon, 18., color))
}

fn mode_label(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::All => "全部",
        SearchMode::Apps => "应用程序",
        SearchMode::Files => "文件名称",
        SearchMode::Text => "文件正文",
    }
}

fn hit_style(kind: HitKind) -> (&'static str, &'static str, u32) {
    match kind {
        HitKind::App => ("\u{e71d}", "应用", 0x9ecafa),
        HitKind::File => ("\u{e8b7}", "文件", 0xf5cc90),
        HitKind::Folder => ("\u{e8b7}", "文件夹", 0xf5cc90),
        HitKind::Text => ("\u{e8a5}", "正文", 0xb5d8fa),
    }
}

fn hit_icon(hit: &Hit, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    #[cfg(windows)]
    {
        let image = match hit.kind {
            HitKind::App => icons::app_icon(&hit.path, window, cx),
            HitKind::File | HitKind::Text => icons::file_icon(&hit.path, window, cx),
            HitKind::Folder => None,
        }?;
        Some(img(image).size(px(20.)).flex_shrink_0().into_any_element())
    }
    #[cfg(not(windows))]
    {
        let _ = (hit, window, cx);
        None
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn section_label(label: &'static str) -> Div {
    div()
        .px(px(16.))
        .h(rem(28.))
        .flex()
        .items_center()
        .text_size(rem(12.))
        .text_color(rgb(MUTED))
        .child(label)
}

fn kind_label(label: &'static str) -> Div {
    div()
        .px_2()
        .text_size(rem(12.))
        .text_color(rgb(MUTED))
        .child(label)
}

fn entry(
    id: impl Into<ElementId>,
    icon: AnyElement,
    title: String,
    detail: String,
    active: bool,
) -> Stateful<Div> {
    row()
        .id(id)
        .h(rem(44.))
        .w_full()
        .px(px(16.))
        .gap(px(16.))
        .rounded(px(4.))
        .when(active, |row| row.bg(rgba(0xffffff17)))
        .hover(move |style| style.bg(rgba(if active { 0xffffff1e } else { 0xffffff0e })))
        .active(|style| style.bg(rgba(0xffffff1e)))
        .cursor_pointer()
        .child(icon)
        .child(
            row()
                .flex_1()
                .min_w_0()
                .gap(px(15.))
                .child(
                    div()
                        .when(!detail.is_empty(), |title| title.max_w(relative(0.56)))
                        .truncate()
                        .text_size(rem(16.))
                        .child(title),
                )
                .when(!detail.is_empty(), |line| {
                    line.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(rem(14.))
                            .text_color(rgb(MUTED))
                            .child(detail),
                    )
                }),
        )
}

fn message(title: &str, detail: &str) -> Div {
    div()
        .p_4()
        .child(
            div()
                .text_size(rem(18.))
                .font_weight(FontWeight::MEDIUM)
                .child(title.to_owned()),
        )
        .child(
            div()
                .mt_2()
                .text_size(rem(14.))
                .text_color(rgb(MUTED))
                .child(detail.to_owned()),
        )
}

fn control(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    row()
        .id(id)
        .h(rem(36.))
        .flex_shrink_0()
        .gap(px(8.))
        .px_2()
        .rounded(px(4.))
        .text_size(rem(14.))
        .hover(|style| style.bg(rgba(0xffffff1c)))
        .active(|style| style.bg(rgba(0xffffff32)))
        .cursor_pointer()
        .child(label.into())
}

fn keycap(key: impl AsRef<str>) -> Div {
    div()
        .flex_shrink_0()
        .px(px(7.))
        .py(px(3.))
        .min_w(px(24.))
        .text_align(TextAlign::Center)
        .rounded(px(6.))
        .bg(rgba(0xffffff0e))
        .text_size(rem(12.))
        .text_color(rgb(MUTED))
        .child(appearance::shortcut_label(key.as_ref()))
}

#[cfg(windows)]
fn open_path(path: &Path) -> Result<()> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::{HSTRING, w},
    };
    anyhow::ensure!(path.exists(), "文件已移动或删除，请更新索引");
    let value = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(path.as_os_str()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    anyhow::ensure!(
        value.0 as isize > 32,
        "Windows 打开失败 ({})",
        value.0 as isize
    );
    Ok(())
}

#[cfg(not(windows))]
fn open_path(path: &Path) -> Result<()> {
    anyhow::ensure!(path.exists(), "文件已移动或删除，请更新索引");
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(command).arg(path).status()?;
    anyhow::ensure!(status.success(), "系统打开失败 ({status})");
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
pub fn listen_to_tray(
    handle: WindowHandle<Moin>,
    receiver: async_channel::Receiver<platform::tray::TrayEvent>,
    cx: &mut App,
) {
    use platform::tray::TrayEvent;

    cx.spawn(async move |cx| {
        while let Ok(event) = receiver.recv().await {
            let result = match event {
                TrayEvent::Quit => {
                    let _ = cx.update(|app| app.quit());
                    return;
                }
                TrayEvent::HotkeyChanged(hotkey) => handle.update(cx, |view, _, cx| {
                    view.active_hotkey = hotkey.clone();
                    view.hotkey_hint = hotkey.map_or_else(
                        || "快捷键占用 · 从托盘打开".into(),
                        |key| format!("{key} · 唤起 / 收起"),
                    );
                    cx.notify();
                }),
                TrayEvent::ActivateFeature(feature) => handle.update(cx, |view, window, cx| {
                    if view.folder_picker_open {
                        return;
                    }
                    let key = view.config.feature_shortcuts.get(feature).to_owned();
                    if let Some(settings) = view.settings_window {
                        if settings
                            .update(cx, |settings, window, cx| {
                                settings.capture_registered_hotkey(&key, window, cx)
                            })
                            .unwrap_or(false)
                        {
                            return;
                        }
                    }
                    view.activate_feature(feature, window, cx);
                }),
                TrayEvent::Show | TrayEvent::Toggle => handle.update(cx, |view, window, cx| {
                    if view.folder_picker_open {
                        return;
                    }
                    if matches!(event, TrayEvent::Toggle)
                        && let (Some(settings), Some(key)) =
                            (view.settings_window, view.active_hotkey.as_deref())
                        && settings
                            .update(cx, |view, window, cx| {
                                view.capture_registered_hotkey(key, window, cx)
                            })
                            .unwrap_or(false)
                    {
                        return;
                    }
                    if matches!(event, TrayEvent::Toggle)
                        && launcher_window::is_visible(window)
                        && window.is_window_active()
                    {
                        launcher_window::hide(window);
                    } else {
                        launcher_window::show(window);
                        view.focus_input(window, cx);
                    }
                }),
            };
            if result.is_err() {
                break;
            }
        }
        let _ = cx.update(|app| app.quit());
    })
    .detach();
}

#[cfg(test)]
mod palette_tests {
    use super::{parse_scope_prefix, result_number};
    use moin::search::index::SearchMode;
    #[::core::prelude::v1::test]
    fn scope_aliases_require_space_and_preserve_query() {
        assert_eq!(parse_scope_prefix("file"), None);
        assert_eq!(parse_scope_prefix("file.txt"), None);
        assert_eq!(
            parse_scope_prefix(" File  C:\\资料\\交接.md"),
            Some((SearchMode::Files, "C:\\资料\\交接.md".into()))
        );
        assert_eq!(
            parse_scope_prefix("apps "),
            Some((SearchMode::Apps, String::new()))
        );
        assert_eq!(
            parse_scope_prefix("正文\t检索内容"),
            Some((SearchMode::Text, "检索内容".into()))
        );
        assert_eq!(parse_scope_prefix("搜索 file 内容"), None);
    }
    #[::core::prelude::v1::test]
    fn shifted_number_row_keys_still_select_the_corresponding_result() {
        for (index, key) in "!@#$%^&*(".chars().enumerate() {
            assert_eq!(result_number(&key.to_string()), Some(index + 1));
        }
        assert_eq!(result_number("9"), Some(9));
        assert_eq!(result_number("0"), None);
        assert_eq!(result_number("pageup"), None);
    }
}
