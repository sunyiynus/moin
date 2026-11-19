//! Application bindings around GPUI Kit's Input and InputState.
//!
//! The adapter keeps a plain value available before the owning window is drawn.
//! Selection, editing, undo, IME, clipboard and painting belong to GPUI Kit.
use gpui::{prelude::*, *};
use gpui_kit::component::input::{
    self as kit, Input, InputEvent, InputState, Textarea, TextareaState,
};

const PLACEHOLDER: &str = "搜索应用、文件和命令…";

actions!(moin_search_input, [Backspace]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "shift-enter",
        kit::Enter {
            secondary: false,
            shift: true,
        },
        Some("Input"),
    )]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("ctrl-a", kit::SelectAll, Some("Input")),
        KeyBinding::new("ctrl-c", kit::Copy, Some("Input")),
        KeyBinding::new("ctrl-x", kit::Cut, Some("Input")),
        KeyBinding::new("ctrl-v", kit::Paste, Some("Input")),
    ]);
}

pub struct Changed;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Search,
    Field,
    Chat,
}

#[derive(Clone)]
enum TextState {
    Input(Entity<InputState>),
    Textarea(Entity<TextareaState>),
}

impl TextState {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Input(state) => state.read(cx).focus_handle(cx).clone(),
            Self::Textarea(state) => state.read(cx).focus_handle(cx).clone(),
        }
    }

    fn value(&self, cx: &App) -> SharedString {
        match self {
            Self::Input(state) => state.read(cx).value(),
            Self::Textarea(state) => state.read(cx).value(),
        }
    }

    fn set_value(&self, value: String, window: &mut Window, cx: &mut App) {
        match self {
            Self::Input(state) => {
                state.update(cx, |state, cx| state.set_value(value, window, cx))
            }
            Self::Textarea(state) => {
                state.update(cx, |state, cx| state.set_value(value, window, cx))
            }
        }
    }

    fn set_placeholder(&self, placeholder: SharedString, window: &mut Window, cx: &mut App) {
        match self {
            Self::Input(state) => {
                state.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx))
            }
            Self::Textarea(state) => {
                state.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx))
            }
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Input(state) => state.update(cx, |state, cx| state.focus(window, cx)),
            Self::Textarea(state) => state.update(cx, |state, cx| state.focus(window, cx)),
        }
    }

    fn marked(&self, window: &mut Window, cx: &mut App) -> bool {
        match self {
            Self::Input(state) => state.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            }),
            Self::Textarea(state) => state.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            }),
        }
    }

    #[cfg(test)]
    fn replace_and_mark_text_in_range(
        &self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        selected: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        match self {
            Self::Input(state) => state.update(cx, |state, cx| {
                state.replace_and_mark_text_in_range(range, text, selected, window, cx)
            }),
            Self::Textarea(state) => state.update(cx, |state, cx| {
                state.replace_and_mark_text_in_range(range, text, selected, window, cx)
            }),
        }
    }

    #[cfg(test)]
    fn replace_text_in_range(
        &self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        match self {
            Self::Input(state) => state.update(cx, |state, cx| {
                state.replace_text_in_range(range, text, window, cx)
            }),
            Self::Textarea(state) => state.update(cx, |state, cx| {
                state.replace_text_in_range(range, text, window, cx)
            }),
        }
    }
}

pub struct SearchInput {
    initial_focus: FocusHandle,
    state: Option<TextState>,
    text: String,
    emitted_text: String,
    placeholder: SharedString,
    kind: Kind,
    composing: bool,
    pending_value: bool,
    pending_placeholder: bool,
    text_size_rem: f32,
    line_height_rem: f32,
    ink: u32,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<Changed> for SearchInput {}

impl Focusable for SearchInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.as_ref().map_or_else(
            || self.initial_focus.clone(),
            |state| state.focus_handle(cx),
        )
    }
}

impl SearchInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            initial_focus: cx.focus_handle(),
            state: None,
            text: String::new(),
            emitted_text: String::new(),
            placeholder: PLACEHOLDER.into(),
            kind: Kind::Search,
            composing: false,
            pending_value: false,
            pending_placeholder: false,
            text_size_rem: 23. / 16.,
            line_height_rem: 32. / 16.,
            ink: 0xffffff,
            subscriptions: Vec::new(),
        }
    }

    pub fn field(value: &str, placeholder: &str, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.text = value.to_owned();
        input.emitted_text = input.text.clone();
        input.placeholder = placeholder.to_owned().into();
        input.kind = Kind::Field;
        input.text_size_rem = 14. / 16.;
        input.line_height_rem = 24. / 16.;
        input.ink = 0x1b1b1b;
        input
    }

    pub fn chat(cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.kind = Kind::Chat;
        input.placeholder = "向 Moin 提问，Enter 发送 · Shift+Enter 换行…".into();
        input
    }

    pub fn set_rem_size(&mut self, text_size_rem: f32, line_height_rem: f32) {
        self.text_size_rem = text_size_rem;
        self.line_height_rem = line_height_rem;
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    pub fn is_composing(&self) -> bool {
        self.composing
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != placeholder {
            self.placeholder = placeholder.to_owned().into();
            self.pending_placeholder = true;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(state) = &self.state {
            state.focus(window, cx);
        } else {
            self.initial_focus.focus(window, cx);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_value(String::new(), cx);
    }

    pub fn set_value(&mut self, value: String, cx: &mut Context<Self>) {
        if self.text != value || self.composing {
            self.text = value;
            self.emitted_text = self.text.clone();
            self.composing = false;
            self.pending_value = true;
            cx.emit(Changed);
            cx.notify();
        }
    }

    fn ensure_state(&mut self, window: &mut Window, cx: &mut Context<Self>) -> TextState {
        if let Some(state) = &self.state {
            let state = state.clone();
            if std::mem::take(&mut self.pending_value) {
                state.set_value(self.text.clone(), window, cx);
            }
            if std::mem::take(&mut self.pending_placeholder) {
                state.set_placeholder(self.placeholder.clone(), window, cx);
            }
            return state;
        }
        let value = self.text.clone();
        let placeholder = self.placeholder.clone();
        let chat = self.kind == Kind::Chat;
        let state = if chat {
            TextState::Textarea(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .default_value(value.clone())
                    .placeholder(placeholder.clone())
                    .auto_grow(1, 5)
            }))
        } else {
            TextState::Input(cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value.clone())
                    .placeholder(placeholder.clone())
            }))
        };
        match &state {
            TextState::Input(state) => {
                let wrapped = TextState::Input(state.clone());
                self.subscriptions.push(cx.subscribe_in(
                    state,
                    window,
                    move |view, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            view.read_state(&wrapped, true, window, cx);
                        }
                    },
                ));
                let wrapped = TextState::Input(state.clone());
                self.subscriptions
                    .push(cx.observe_in(state, window, move |view, _, window, cx| {
                        view.read_state(&wrapped, false, window, cx)
                    }));
            }
            TextState::Textarea(state) => {
                let wrapped = TextState::Textarea(state.clone());
                self.subscriptions.push(cx.subscribe_in(
                    state,
                    window,
                    move |view, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            view.read_state(&wrapped, true, window, cx);
                        }
                    },
                ));
                let wrapped = TextState::Textarea(state.clone());
                self.subscriptions
                    .push(cx.observe_in(state, window, move |view, _, window, cx| {
                        view.read_state(&wrapped, false, window, cx)
                    }));
            }
        }
        if self.initial_focus.is_focused(window) {
            state.focus(window, cx);
        }
        self.pending_value = false;
        self.pending_placeholder = false;
        self.state = Some(state.clone());
        state
    }

    fn read_state(
        &mut self,
        state: &TextState,
        committed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A pending application update wins over an older Kit notification.
        if self.pending_value {
            return;
        }
        let text = state.value(cx).to_string();
        let composing = state.marked(window, cx);
        let changed = self.text != text || self.composing != composing;
        self.text = text;
        self.composing = composing;
        if committed && !composing && self.text != self.emitted_text {
            self.emitted_text = self.text.clone();
            cx.emit(Changed);
        }
        if changed {
            cx.notify();
        }
    }

    fn refresh_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.pending_value {
            if let Some(state) = &self.state {
                self.composing = state.marked(window, cx);
            }
        }
        self.composing
    }

    fn enter(&mut self, action: &kit::Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.refresh_composition(window, cx) {
            // The native IME owns confirmation until its preedit is committed.
            // Kit's multiline handler would otherwise replace it with a newline.
            cx.stop_propagation();
            return;
        }
        if self.kind == Kind::Field {
            cx.propagate();
            return;
        }
        if self.kind == Kind::Chat && action.shift {
            cx.propagate();
            return;
        }
        window.dispatch_action(Box::new(super::Confirm), cx);
        cx.stop_propagation();
    }

    fn escape(&mut self, _: &kit::Escape, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == Kind::Field || self.refresh_composition(window, cx) {
            cx.propagate();
            return;
        }
        window.dispatch_action(Box::new(super::Dismiss), cx);
        cx.stop_propagation();
    }

    fn up(&mut self, _: &kit::MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != Kind::Search || self.refresh_composition(window, cx) {
            cx.propagate();
            return;
        }
        window.dispatch_action(Box::new(super::Previous), cx);
        cx.stop_propagation();
    }

    fn down(&mut self, _: &kit::MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != Kind::Search || self.refresh_composition(window, cx) {
            cx.propagate();
            return;
        }
        window.dispatch_action(Box::new(super::Next), cx);
        cx.stop_propagation();
    }

    fn backspace(&mut self, _: &kit::Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == Kind::Search
            && self.text.is_empty()
            && !self.refresh_composition(window, cx)
        {
            window.dispatch_action(Box::new(Backspace), cx);
            cx.stop_propagation();
        } else {
            cx.propagate();
        }
    }
}

impl Render for SearchInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.ensure_state(window, cx);
        let input: AnyElement = match &state {
            TextState::Input(state) => {
                let mut input = Input::new(state)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .text_size(rems(self.text_size_rem))
                    .line_height(rems(self.line_height_rem))
                    .text_color(rgb(self.ink))
                    .px_0()
                    .py_0();
                if self.kind != Kind::Chat {
                    input.style().size.height = Some(rems(self.line_height_rem).into());
                }
                input.into_any_element()
            }
            TextState::Textarea(state) => Textarea::new(state)
                .appearance(false)
                .bordered(false)
                .text_size(rems(self.text_size_rem))
                .line_height(rems(self.line_height_rem))
                .text_color(rgb(self.ink))
                .px_0()
                .py_0()
                .into_any_element(),
        };
        let editor = div()
            .w_full()
            .min_w_0()
            .capture_action(cx.listener(Self::enter))
            .capture_action(cx.listener(Self::escape))
            .capture_action(cx.listener(Self::up))
            .capture_action(cx.listener(Self::down))
            .capture_action(cx.listener(Self::backspace))
            .child(input);
        if self.kind == Kind::Field {
            super::ui::light(editor, cx)
        } else {
            super::ui::dark(editor, cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Events {
        changes: Vec<String>,
        submitted: usize,
        dismissed: usize,
        moves: isize,
        backs: usize,
    }

    struct Harness {
        input: Entity<SearchInput>,
        events: Rc<RefCell<Events>>,
        _changed: Subscription,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .p_4()
                .on_action(cx.listener(|view, _: &super::super::Confirm, _, _| {
                    view.events.borrow_mut().submitted += 1
                }))
                .on_action(cx.listener(|view, _: &super::super::Dismiss, _, _| {
                    view.events.borrow_mut().dismissed += 1
                }))
                .on_action(cx.listener(|view, _: &super::super::Next, _, _| {
                    view.events.borrow_mut().moves += 1
                }))
                .on_action(cx.listener(|view, _: &super::super::Previous, _, _| {
                    view.events.borrow_mut().moves -= 1
                }))
                .on_action(
                    cx.listener(|view, _: &Backspace, _, _| view.events.borrow_mut().backs += 1),
                )
                .child(self.input.clone())
        }
    }

    fn fixture(
        app: &mut TestAppContext,
        chat: bool,
    ) -> (
        Entity<SearchInput>,
        &mut VisualTestContext,
        Rc<RefCell<Events>>,
    ) {
        app.update(|cx| {
            super::super::ui::init(cx);
            bind_keys(cx);
        });
        let events = Rc::new(RefCell::new(Events::default()));
        let mut input = None;
        let (_, cx) = app.add_window_view(|window, cx| {
            let editor = cx.new(if chat {
                SearchInput::chat
            } else {
                SearchInput::new
            });
            editor.update(cx, |editor, cx| editor.focus(window, cx));
            input = Some(editor.clone());
            let view = cx.new(|cx| {
                let changed_events = events.clone();
                let changed = cx.subscribe(&editor, move |_, editor, _: &Changed, cx| {
                    changed_events
                        .borrow_mut()
                        .changes
                        .push(editor.read(cx).value().to_owned());
                });
                Harness {
                    input: editor,
                    events: events.clone(),
                    _changed: changed,
                }
            });
            Root::new(view, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (input.unwrap(), cx, events)
    }

    #[::core::prelude::v1::test]
    fn kit_input_keeps_ime_preedit_out_of_committed_search_events() {
        let mut app = TestAppContext::single();
        let (editor, cx, events) = fixture(&mut app, false);
        let state = cx.read(|cx| editor.read(cx).state.clone().unwrap());
        cx.update(|window, cx| {
            state.replace_and_mark_text_in_range(None, "f", None, window, cx);
        });
        cx.read(|cx| {
            assert!(editor.read(cx).is_composing());
            assert_eq!(editor.read(cx).value(), "f");
        });
        assert!(events.borrow().changes.is_empty());
        cx.update(|window, cx| {
            state.replace_text_in_range(None, "文件😀", window, cx);
        });
        cx.read(|cx| {
            assert!(!editor.read(cx).is_composing());
            assert_eq!(editor.read(cx).value(), "文件😀");
        });
        assert_eq!(events.borrow().changes, ["文件😀"]);
    }

    #[::core::prelude::v1::test]
    fn kit_input_preserves_application_navigation_and_programmatic_updates() {
        let mut app = TestAppContext::single();
        let (editor, cx, events) = fixture(&mut app, false);
        cx.simulate_input("中文😀");
        cx.read(|cx| assert_eq!(editor.read(cx).value(), "中文😀"));
        cx.simulate_keystrokes("down down up enter escape");
        assert_eq!(events.borrow().moves, 1);
        assert_eq!(events.borrow().submitted, 1);
        assert_eq!(events.borrow().dismissed, 1);
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.clear(cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("backspace");
        assert_eq!(events.borrow().backs, 1);
        assert_eq!(events.borrow().changes, ["中", "中文", "中文😀", ""]);
        let state = cx.read(|cx| editor.read(cx).state.clone().unwrap());
        cx.read(|cx| assert_eq!(state.value(cx).as_ref(), ""));
    }

    #[::core::prelude::v1::test]
    fn kit_chat_uses_shift_enter_for_newlines_and_plain_enter_for_send() {
        let mut app = TestAppContext::single();
        let (editor, cx, events) = fixture(&mut app, true);
        cx.simulate_input("第一行");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("第二行");
        cx.read(|cx| assert_eq!(editor.read(cx).value(), "第一行\n第二行"));
        assert_eq!(events.borrow().submitted, 0);
        cx.simulate_keystrokes("enter");
        assert_eq!(events.borrow().submitted, 1);
        cx.read(|cx| assert_eq!(editor.read(cx).value(), "第一行\n第二行"));
    }

    #[::core::prelude::v1::test]
    fn kit_chat_enter_during_ime_preedit_does_not_send_or_insert_a_newline() {
        let mut app = TestAppContext::single();
        let (editor, cx, events) = fixture(&mut app, true);
        let state = cx.read(|cx| editor.read(cx).state.clone().unwrap());
        cx.update(|window, cx| {
            state.replace_and_mark_text_in_range(None, "f", None, window, cx);
        });
        cx.simulate_keystrokes("enter");
        cx.read(|cx| {
            assert_eq!(editor.read(cx).value(), "f");
            assert!(editor.read(cx).is_composing());
            assert_eq!(state.value(cx).as_ref(), "f");
        });
        assert_eq!(events.borrow().submitted, 0);
        assert!(events.borrow().changes.is_empty());
    }
}
