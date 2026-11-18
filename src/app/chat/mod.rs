use super::*;
use super::{appearance::rem, launcher_window, settings};
use moin::ai::{self, AiConfig, CancelToken, ChatEvent, ChatMessage, ChatRole, Source};
use moin::config::AppearanceConfig;
use std::collections::HashMap;
pub(crate) mod markdown;
mod view;

// Only data and request lifetimes live here. No render elements, input entities,
// focus handles, Markdown layout caches or scroll handles cross window boundaries.
pub(super) struct ChatStore {
    config: AiConfig,
    config_error: Option<String>,
    appearance: AppearanceConfig,
    database: PathBuf,
    sessions: Vec<SessionData>,
    jobs: HashMap<u64, Running>,
    next_id: u64,
    next_turn: u64,
}
struct Running {
    cancel: CancelToken,
    _task: Task<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[derive(Clone)]
struct SessionData {
    id: u64,
    turns: Vec<TurnData>,
    history: Vec<ChatMessage>,
    busy: bool,
    generation: u64,
    status: String,
    error: Option<String>,
}
#[derive(Clone)]
struct TurnData {
    id: u64,
    question: String,
    answer: String,
    sources: Vec<Source>,
    complete: bool,
    stopped: bool,
    commands: Vec<(ai::CommandRequest, Option<ai::CommandReport>)>,
}
impl SessionData {
    fn new(id: u64) -> Self {
        Self {
            id,
            turns: vec![],
            history: vec![],
            busy: false,
            generation: 0,
            status: String::new(),
            error: None,
        }
    }
    fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Text(text) => {
                if let Some(turn) = self.turns.last_mut() {
                    turn.answer.push_str(&text);
                }
                self.status = "正在回复…".into();
            }
            ChatEvent::Status(status) => self.status = status,
            ChatEvent::Sources(sources) => {
                if let Some(turn) = self.turns.last_mut() {
                    turn.sources = sources;
                }
            }
            ChatEvent::CommandStarted(request) => {
                if let Some(turn) = self.turns.last_mut() {
                    turn.commands.push((request, None));
                }
                self.status = "正在执行本机命令…".into();
            }
            ChatEvent::CommandFinished(report) => {
                if let Some(turn) = self.turns.last_mut() {
                    if let Some((_, result)) =
                        turn.commands.iter_mut().rev().find(|(request, result)| {
                            result.is_none() && request.script == report.request.script
                        })
                    {
                        *result = Some(report);
                    }
                }
                self.status = "正在处理命令结果…".into();
            }
            ChatEvent::Done => {
                self.busy = false;
                self.status.clear();
                if let Some(turn) = self.turns.last_mut() {
                    if turn.answer.trim().is_empty() {
                        self.error = Some("模型没有返回文本，请检查模型配置后重试。".into());
                    } else {
                        turn.complete = true;
                        self.history.push(ChatMessage {
                            role: ChatRole::User,
                            content: turn.question.clone(),
                        });
                        self.history.push(ChatMessage {
                            role: ChatRole::Assistant,
                            content: turn.answer.clone(),
                        });
                        if self.history.len() > 24 {
                            self.history.drain(..self.history.len() - 24);
                        }
                    }
                }
            }
            ChatEvent::Error(error) => {
                self.busy = false;
                self.status.clear();
                self.error = Some(error);
            }
        }
    }
}
impl ChatStore {
    fn route_event(&mut self, id: u64, generation: u64, event: ChatEvent) -> bool {
        if let Some(chat) = self
            .sessions
            .iter_mut()
            .find(|chat| chat.id == id && chat.generation == generation)
        {
            chat.apply(event);
            true
        } else {
            false
        }
    }
    fn create(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.sessions.push(SessionData::new(id));
        id
    }
    fn stop(&mut self, id: u64) {
        self.jobs.remove(&id);
        if let Some(chat) = self.sessions.iter_mut().find(|chat| chat.id == id) {
            chat.generation = chat.generation.wrapping_add(1);
            if chat.busy {
                if let Some(turn) = chat.turns.last_mut() {
                    turn.stopped = true;
                }
                chat.status = "已停止生成".into();
            }
            chat.busy = false;
        }
    }
    fn start(&mut self, id: u64, prompt: String, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.sessions.iter().find(|chat| chat.id == id) else {
            return false;
        };
        if chat.busy {
            return false;
        }
        let session = match ai::start_chat(
            self.config.clone(),
            self.database.clone(),
            chat.history.clone(),
            prompt.clone(),
        ) {
            Ok(session) => session,
            Err(error) => {
                self.sessions
                    .iter_mut()
                    .find(|chat| chat.id == id)
                    .unwrap()
                    .error = Some(format!("{error:#}"));
                cx.notify();
                return false;
            }
        };
        self.stop(id);
        let turn_id = self.next_turn;
        self.next_turn += 1;
        let chat = self.sessions.iter_mut().find(|chat| chat.id == id).unwrap();
        let generation = chat.generation;
        chat.busy = true;
        chat.error = None;
        chat.status = "正在连接模型…".into();
        if chat.turns.len() >= 40 {
            chat.turns.remove(0);
        }
        chat.turns.push(TurnData {
            id: turn_id,
            question: prompt,
            answer: String::new(),
            sources: vec![],
            complete: false,
            stopped: false,
            commands: vec![],
        });
        let cancel = session.cancel;
        let task = cx.spawn(async move |store, cx| {
            let mut terminal = false;
            while let Ok(event) = session.receiver.recv().await {
                let finished = matches!(event, ChatEvent::Done | ChatEvent::Error(_));
                if store
                    .update(cx, |store, cx| {
                        if store.route_event(id, generation, event) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
                if finished {
                    terminal = true;
                    break;
                }
            }
            if !terminal {
                let _ = store.update(cx, |store, cx| {
                    if store.route_event(
                        id,
                        generation,
                        ChatEvent::Error("模型连接已断开，请重试。".into()),
                    ) {
                        cx.notify();
                    }
                });
            }
        });
        self.jobs.insert(
            id,
            Running {
                cancel,
                _task: task,
            },
        );
        cx.notify();
        true
    }
}

pub(super) struct AiChat {
    pub active: bool,
    pub pane: Entity<AiPane>,
    store: Entity<ChatStore>,
    detached: Option<WindowHandle<AiPane>>,
    launcher: Option<WindowHandle<Moin>>,
}
impl AiChat {
    pub fn new(
        cx: &mut Context<Moin>,
        appearance: &AppearanceConfig,
        config: AiConfig,
        error: Option<String>,
        database: PathBuf,
    ) -> Self {
        let appearance = appearance.clone();
        let store = cx.new(|_| ChatStore {
            config,
            config_error: error,
            appearance,
            database,
            sessions: vec![SessionData::new(1)],
            jobs: HashMap::new(),
            next_id: 2,
            next_turn: 1,
        });
        let owner = cx.entity().downgrade();
        let pane = cx.new(|cx| AiPane::new(store.clone(), owner, false, 1, cx));
        Self {
            active: false,
            pane,
            store,
            detached: None,
            launcher: None,
        }
    }
    pub fn sync_appearance(&mut self, appearance: &AppearanceConfig, cx: &mut Context<Moin>) {
        self.store.update(cx, |store, cx| {
            store.appearance = appearance.clone();
            cx.notify();
        });
    }
}

// A new AiPane is constructed for each window; only plain snapshots are copied.
pub(super) struct AiPane {
    store: Entity<ChatStore>,
    owner: WeakEntity<Moin>,
    detached: bool,
    sessions: Vec<UiSession>,
    selected: usize,
    model_config: AiConfig,
    config_error: Option<String>,
    appearance: AppearanceConfig,
    sidebar_width: f32,
    content_gutter: f32,
    _subscription: Subscription,
}
pub(super) struct UiSession {
    id: u64,
    input: Entity<SearchInput>,
    scroll: ScrollHandle,
    turns: Vec<UiTurn>,
    busy: bool,
    status: String,
    error: Option<String>,
    _subscription: Subscription,
}
struct UiTurn {
    data: TurnData,
    question_markdown: markdown::MarkdownDocument,
    answer_markdown: markdown::MarkdownDocument,
    commands: Vec<(ai::CommandRequest, Option<ai::CommandReport>, bool)>,
}
impl std::ops::Deref for UiTurn {
    type Target = TurnData;
    fn deref(&self) -> &TurnData {
        &self.data
    }
}
impl std::ops::Deref for AiPane {
    type Target = UiSession;
    fn deref(&self) -> &UiSession {
        &self.sessions[self.selected]
    }
}
impl std::ops::DerefMut for AiPane {
    fn deref_mut(&mut self) -> &mut UiSession {
        &mut self.sessions[self.selected]
    }
}
impl AiPane {
    fn new(
        store: Entity<ChatStore>,
        owner: WeakEntity<Moin>,
        detached: bool,
        selected_id: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = store.read(cx);
        let config = model.config.clone();
        let appearance = model.appearance.clone();
        let error = model.config_error.clone();
        let subscription = cx.observe(&store, |pane, _, cx| {
            pane.sync(cx);
            cx.notify();
        });
        let mut pane = Self {
            store,
            owner,
            detached,
            sessions: vec![],
            selected: 0,
            model_config: config,
            config_error: error,
            appearance,
            sidebar_width: 172.,
            content_gutter: 28.,
            _subscription: subscription,
        };
        pane.sync(cx);
        pane.selected = pane
            .sessions
            .iter()
            .position(|session| session.id == selected_id)
            .unwrap_or(0);
        pane
    }
    fn sync(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let snapshots = store.sessions.clone();
        self.model_config = store.config.clone();
        self.config_error = store.config_error.clone();
        self.appearance = store.appearance.clone();
        let selected_id = self.sessions.get(self.selected).map(|session| session.id);
        let mut previous = std::mem::take(&mut self.sessions)
            .into_iter()
            .map(|session| (session.id, session))
            .collect::<HashMap<_, _>>();
        for data in snapshots {
            let mut ui = previous.remove(&data.id).unwrap_or_else(|| {
                let input = cx.new(SearchInput::chat);
                let subscription = cx.subscribe(&input, |_, _, _: &Changed, cx| cx.notify());
                UiSession {
                    id: data.id,
                    input,
                    scroll: ScrollHandle::new(),
                    turns: vec![],
                    busy: false,
                    status: String::new(),
                    error: None,
                    _subscription: subscription,
                }
            });
            let base = self.appearance.ui_font_size.max(1.);
            let size = self.appearance.ai_font_size;
            ui.input.update(cx, |input, cx| {
                input.set_rem_size(size / base, size * 1.65 / base);
                cx.notify();
            });
            let follow = ui.scroll.max_offset().height + ui.scroll.offset().y < px(64.);
            let old_last = ui.turns.last().map(|turn| (turn.id, turn.answer.len()));
            let mut old_turns = std::mem::take(&mut ui.turns)
                .into_iter()
                .map(|turn| (turn.id, turn))
                .collect::<HashMap<_, _>>();
            for turn in data.turns {
                let mut rendered = old_turns.remove(&turn.id).unwrap_or_else(|| UiTurn {
                    question_markdown: markdown::MarkdownDocument::parse(&turn.question),
                    answer_markdown: markdown::MarkdownDocument::parse(""),
                    data: TurnData {
                        answer: String::new(),
                        ..turn.clone()
                    },
                    commands: vec![],
                });
                if rendered.data.answer != turn.answer {
                    rendered.answer_markdown = markdown::MarkdownDocument::parse(&turn.answer);
                }
                let old_commands = std::mem::take(&mut rendered.commands);
                rendered.commands = turn
                    .commands
                    .iter()
                    .enumerate()
                    .map(|(index, (request, report))| {
                        let previous = old_commands.get(index);
                        // Auto-expand a newly failed command; later user folding stays local.
                        let newly_failed = report.as_ref().is_some_and(|report| !report.success)
                            && previous.is_none_or(|(_, report, _)| report.is_none());
                        (
                            request.clone(),
                            report.clone(),
                            newly_failed || previous.is_some_and(|(_, _, expanded)| *expanded),
                        )
                    })
                    .collect();
                rendered.data = turn;
                ui.turns.push(rendered);
            }
            ui.busy = data.busy;
            ui.status = data.status;
            ui.error = data.error;
            if follow && old_last != ui.turns.last().map(|turn| (turn.id, turn.answer.len())) {
                ui.scroll.scroll_to_bottom();
            }
            self.sessions.push(ui);
        }
        self.selected = selected_id
            .and_then(|id| self.sessions.iter().position(|session| session.id == id))
            .unwrap_or(0);
    }
    pub(super) fn focus_input(&self, window: &mut Window, cx: &App) {
        self.input.read(cx).focus(window, cx);
    }
    pub(super) fn input_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub(super) fn composing(&self, cx: &App) -> bool {
        self.input.read(cx).is_composing()
    }
    pub(super) fn busy(&self) -> bool {
        self.busy
    }
    pub(super) fn send_ai(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.composing(cx) {
            return;
        }
        let prompt = self.input.read(cx).value().trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        let id = self.id;
        if self
            .store
            .update(cx, |store, cx| store.start(id, prompt, cx))
        {
            self.input.update(cx, |input, cx| input.clear(cx));
            self.focus_input(window, cx);
        }
    }
    pub(super) fn stop_ai(&mut self, cx: &mut Context<Self>) {
        let id = self.id;
        self.store.update(cx, |store, cx| {
            store.stop(id);
            cx.notify();
        });
    }
    pub(super) fn new_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.store.update(cx, |store, cx| {
            let id = store.create();
            cx.notify();
            id
        });
        self.sync(cx);
        self.selected = self
            .sessions
            .iter()
            .position(|session| session.id == id)
            .unwrap();
        self.focus_input(window, cx);
        cx.notify();
    }
    fn delete_session(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.stop(id);
            store.sessions.retain(|session| session.id != id);
            if store.sessions.is_empty() {
                store.create();
            }
            cx.notify();
        });
        self.sync(cx);
        self.focus_input(window, cx);
        cx.notify();
    }
    fn retry_ai(&mut self, cx: &mut Context<Self>) {
        let id = self.id;
        self.store.update(cx, |store, cx| {
            let Some(chat) = store.sessions.iter_mut().find(|session| session.id == id) else {
                return;
            };
            if chat.busy {
                return;
            }
            let Some(previous) = chat.turns.pop() else {
                return;
            };
            if previous.complete {
                chat.turns.push(previous);
                return;
            }
            let prompt = previous.question.clone();
            if !store.start(id, prompt, cx) {
                if let Some(chat) = store.sessions.iter_mut().find(|session| session.id == id) {
                    chat.turns.push(previous);
                }
            }
            cx.notify();
        });
    }
    pub(super) fn copy_ai(&mut self, cx: &mut Context<Self>) {
        if let Some(turn) = self.turns.iter().rev().find(|turn| !turn.answer.is_empty()) {
            let mut value = turn.answer.clone();
            for (index, source) in turn.sources.iter().enumerate() {
                value.push_str(&format!(
                    "\n[{}] {} · 片段 {}",
                    index + 1,
                    source.path.display(),
                    source.chunk
                ));
            }
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.status = "已复制回复".into();
            cx.notify();
        }
    }
    fn open_ai_settings(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |view, cx| view.show_ai_settings(cx));
        });
    }
    fn open_ai_window(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        let id = self.id;
        cx.defer(move |cx| {
            let _ = owner.update(cx, |view, cx| view.open_ai_window(id, cx));
        });
    }
    fn return_ai_popup(&mut self, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        let id = self.id;
        cx.defer(move |cx| {
            let _ = owner.update(cx, |view, cx| view.return_ai_popup(id, cx));
        });
    }
    fn set_ai(&mut self, _: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.owner.update(cx, |view, cx| {
            view.ai.active = false;
            view.input.read(cx).focus(window, cx);
            cx.notify();
        });
    }
}

impl Render for AiPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.appearance.ui_font_size));
        self.sidebar_width = if window.viewport_size().width < px(1000.) {
            160.
        } else {
            200.
        };
        self.content_gutter = if window.viewport_size().width < px(1000.) {
            20.
        } else {
            36.
        };
        div()
            .key_context("MoinAiPane")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(rgb(CHAT_INK))
            .font_family(super::appearance::resolve_family(
                &self.appearance.ui_font_family,
            ))
            .when(self.detached, |root| {
                root.rounded(px(8.))
                    .border_1()
                    .border_color(rgba(0xffffff2e))
                    .bg(rgba(0x202020a8))
            })
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" && !view.composing(cx) {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|view, _: &Confirm, window, cx| {
                if view.input.read(cx).focus_handle(cx).is_focused(window) {
                    view.send_ai(window, cx);
                }
            }))
            .on_action(cx.listener(|view, _: &Dismiss, window, cx| {
                if view.composing(cx) {
                    return;
                }
                if view.busy {
                    view.stop_ai(cx);
                } else if !view.detached {
                    launcher_window::hide(window);
                }
            }))
            .on_action(cx.listener(|view, _: &NewChat, window, cx| view.new_ai_chat(window, cx)))
            .on_action(cx.listener(|view, _: &Copy, _, cx| view.copy_ai(cx)))
            .on_action(
                cx.listener(|view, _: &FocusSearch, window, cx| view.focus_input(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &OpenSettings, window, cx| view.open_ai_settings(window, cx)),
            )
            .on_action(cx.listener(|view, _: &Ai, window, cx| {
                if view.detached {
                    view.return_ai_popup(cx);
                } else {
                    view.set_ai(false, window, cx);
                }
            }))
            .child(self.ai_header(window, cx))
            .child(self.ai_workspace(window, cx))
    }
}

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, Some("MoinAiPane")),
        KeyBinding::new("escape", Dismiss, Some("MoinAiPane")),
        KeyBinding::new("ctrl-n", NewChat, Some("MoinAiPane")),
        KeyBinding::new("ctrl-shift-c", Copy, Some("MoinAiPane")),
        KeyBinding::new("ctrl-l", FocusSearch, Some("MoinAiPane")),
        KeyBinding::new("ctrl-,", OpenSettings, Some("MoinAiPane")),
        KeyBinding::new("ctrl-5", Ai, Some("MoinAiPane")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewChat, Some("MoinAiPane")),
        KeyBinding::new("cmd-shift-c", Copy, Some("MoinAiPane")),
        KeyBinding::new("cmd-l", FocusSearch, Some("MoinAiPane")),
        KeyBinding::new("cmd-,", OpenSettings, Some("MoinAiPane")),
        KeyBinding::new("cmd-5", Ai, Some("MoinAiPane")),
    ]);
}

impl Moin {
    pub(super) fn focus_input(&self, window: &mut Window, cx: &App) {
        if self.ai.active {
            self.ai.pane.read(cx).focus_input(window, cx);
        } else {
            self.input.read(cx).focus(window, cx);
        }
    }
    pub(super) fn set_ai(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.ai.active = active;
        self.ai.launcher = window.window_handle().downcast::<Self>();
        self.actions_open = false;
        self.focus_input(window, cx);
        cx.notify();
    }
    pub(super) fn new_ai_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ai
            .pane
            .update(cx, |pane, cx| pane.new_ai_chat(window, cx));
    }
    pub(super) fn stop_ai(&mut self, cx: &mut Context<Self>) {
        self.ai.pane.update(cx, |pane, cx| pane.stop_ai(cx));
    }
    pub(super) fn copy_ai(&mut self, cx: &mut Context<Self>) {
        self.ai.pane.update(cx, |pane, cx| pane.copy_ai(cx));
    }
    pub fn save_ai_config(&mut self, config: AiConfig, cx: &mut Context<Self>) {
        match config.save(&self.home) {
            Ok(()) => {
                self.ai_config = config.clone();
                self.ai_error = None;
                self.ai_notice = Some("AI 设置已保存，后续提问使用新配置。".into());
                self.ai.store.update(cx, |store, cx| {
                    let ids = store
                        .sessions
                        .iter()
                        .map(|session| session.id)
                        .collect::<Vec<_>>();
                    for id in ids {
                        store.stop(id);
                    }
                    store.config = config;
                    store.config_error = None;
                    for session in &mut store.sessions {
                        session.history.clear();
                    }
                    cx.notify();
                });
            }
            Err(error) => {
                self.ai_error = Some(format!("AI 设置未保存 · {error:#}"));
                self.ai_notice = None;
            }
        }
        cx.notify();
    }
    fn show_ai_settings(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = self.ai.launcher else {
            return;
        };
        let existing = self.settings_window;
        cx.defer(move |cx| {
            if let Some(handle) = existing {
                if handle
                    .update(cx, |view, window, cx| {
                        view.show_ai(cx);
                        window.activate_window();
                    })
                    .is_ok()
                {
                    return;
                }
            }
            let result = settings::open_ai(launcher, cx);
            let _ = launcher.update(cx, |view, _, cx| {
                match result {
                    Ok(handle) => view.settings_window = Some(handle),
                    Err(error) => view.ai_error = Some(format!("无法打开 AI 设置 · {error:#}")),
                }
                cx.notify();
            });
        });
    }
    fn return_ai_popup(&mut self, id: u64, cx: &mut Context<Self>) {
        self.ai.active = true;
        self.ai.pane.update(cx, |pane, cx| {
            pane.sync(cx);
            if let Some(index) = pane.sessions.iter().position(|session| session.id == id) {
                pane.selected = index;
            }
            cx.notify();
        });
        if let Some(handle) = self.ai.launcher {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |view, window, cx| {
                    launcher_window::show(window);
                    view.focus_input(window, cx);
                });
            });
        }
        cx.notify();
    }
    fn open_ai_window(&mut self, selected_id: u64, cx: &mut Context<Self>) {
        let existing = self.ai.detached;
        let owner = cx.entity();
        let store = self.ai.store.clone();
        let launcher = self.ai.launcher;
        cx.defer(move |cx| {
            if let Some(handle) = existing {
                if handle
                    .update(cx, |pane, window, cx| {
                        window.activate_window();
                        pane.focus_input(window, cx);
                    })
                    .is_ok()
                {
                    return;
                }
            }

            let preferred = size(px(1120.), px(760.));
            let extent = cx.primary_display().map_or(preferred, |display| {
                let screen = display.bounds().size;
                size(
                    preferred.width.min((screen.width - px(48.)).max(px(1.))),
                    preferred.height.min((screen.height - px(96.)).max(px(1.))),
                )
            });
            let root_owner = owner.downgrade();
            let result = cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("Moin · AI 对话".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, extent, cx))),
                    window_min_size: Some(size(
                        px(760.).min(extent.width),
                        px(480.).min(extent.height),
                    )),
                    window_background: WindowBackgroundAppearance::Blurred,
                    ..Default::default()
                },
                move |window, cx| {
                    window.set_background_appearance(WindowBackgroundAppearance::Blurred);
                    let closing = root_owner.clone();
                    window.on_window_should_close(cx, move |_, cx| {
                        let _ = closing.update(cx, |view, cx| {
                            view.ai.detached = None;
                            cx.notify();
                        });
                        true
                    });
                    cx.new(|cx| AiPane::new(store, root_owner, true, selected_id, cx))
                },
            );
            let opened = result.is_ok();
            owner.update(cx, |view, cx| {
                match result {
                    Ok(handle) => view.ai.detached = Some(handle),
                    Err(error) => view.ai_error = Some(format!("无法打开对话窗口 · {error:#}")),
                }
                cx.notify();
            });
            if opened {
                if let Some(handle) = launcher {
                    let _ = handle.update(cx, |_, window, _| launcher_window::hide(window));
                }
            }
        });
    }
}

const CHAT_INK: u32 = 0xf1f1f3;
const CHAT_MUTED: u32 = 0xb0b2bb;
const CHAT_ACCENT: u32 = 0xa5b5ff;
const CHAT_LINE: u32 = 0xffffff1a;

fn chat_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    row()
        .id(id)
        .tab_index(0)
        .min_h(rem(28.))
        .px(px(8.))
        .gap(px(6.))
        .rounded(px(6.))
        .text_size(rem(12.))
        .text_color(rgb(CHAT_MUTED))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xffffff0c)))
        .active(|s| s.bg(rgba(0xffffff18)))
        .focus(|s| s.bg(rgba(0x4d6bfe30)))
        .child(label.into())
}

fn chat_avatar(size: f32) -> Div {
    row()
        .size(px(size))
        .justify_center()
        .flex_shrink_0()
        .rounded(px(size * 0.3))
        .bg(rgb(0x4d6bfe))
        .text_color(rgb(0xffffff))
        .text_size(px(size * 0.55))
        .font_weight(FontWeight::SEMIBOLD)
        .child("M")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turn(id: u64, question: &str) -> TurnData {
        TurnData {
            id,
            question: question.into(),
            answer: String::new(),
            sources: vec![],
            complete: false,
            stopped: false,
            commands: vec![],
        }
    }
    fn store() -> ChatStore {
        let mut first = SessionData::new(1);
        first.busy = true;
        first.turns.push(turn(1, "first"));
        let mut second = SessionData::new(2);
        second.busy = true;
        second.turns.push(turn(2, "second"));
        ChatStore {
            config: AiConfig::default(),
            config_error: None,
            appearance: AppearanceConfig::default(),
            database: PathBuf::new(),
            sessions: vec![first, second],
            jobs: HashMap::new(),
            next_id: 3,
            next_turn: 3,
        }
    }
    #[::core::prelude::v1::test]
    fn shared_snapshots_are_plain_independent_data() {
        fn plain_data<T: Send + Sync>() {}
        plain_data::<SessionData>();
        let mut model = store();
        model.route_event(1, 0, ChatEvent::Text("共享回复".into()));
        let mut left = model.sessions.clone();
        let right = model.sessions.clone();
        left[0].turns[0].answer.push_str(" · 本地副本");
        assert_eq!(right[0].turns[0].answer, "共享回复");
        assert_eq!(model.sessions[0].turns[0].answer, "共享回复");
    }
    #[::core::prelude::v1::test]
    fn switching_views_or_stopping_one_session_cannot_redirect_streams() {
        let mut model = store();
        assert!(model.route_event(1, 0, ChatEvent::Text("A".into())));
        assert!(model.route_event(2, 0, ChatEvent::Text("B".into())));
        model.stop(1);
        assert!(!model.route_event(1, 0, ChatEvent::Text("stale".into())));
        assert!(model.route_event(2, 0, ChatEvent::Done));
        assert_eq!(model.sessions[0].turns[0].answer, "A");
        assert!(model.sessions[0].turns[0].stopped);
        assert!(model.sessions[0].history.is_empty());
        assert_eq!(model.sessions[1].history[1].content, "B");
        assert!(!model.route_event(999, 0, ChatEvent::Text("deleted session".into())));
    }
}
