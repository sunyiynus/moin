// Uses the native IME and custom paint pattern from GPUI's Apache-2.0 input example.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill,
    point, prelude::*, px, relative, rems, rgb, size,
};
use unicode_segmentation::UnicodeSegmentation;

const KEY_CONTEXT: &str = "MoinSearchInput";
const PLACEHOLDER: &str = "搜索应用、文件和命令…";
const INK: u32 = 0xffffff;
const MUTED: u32 = 0xd2d2d2;
const SELECTION: u32 = 0x335a72;

actions!(
    moin_search_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        ExtendLeft,
        ExtendRight,
        Home,
        End,
        ExtendHome,
        ExtendEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let input = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, input),
        KeyBinding::new("delete", Delete, input),
        KeyBinding::new("left", Left, input),
        KeyBinding::new("right", Right, input),
        KeyBinding::new("shift-left", ExtendLeft, input),
        KeyBinding::new("shift-right", ExtendRight, input),
        KeyBinding::new("home", Home, input),
        KeyBinding::new("end", End, input),
        KeyBinding::new("shift-home", ExtendHome, input),
        KeyBinding::new("shift-end", ExtendEnd, input),
        KeyBinding::new("ctrl-a", SelectAll, input),
        KeyBinding::new("ctrl-c", Copy, input),
        KeyBinding::new("ctrl-x", Cut, input),
        KeyBinding::new("ctrl-v", Paste, input),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-a", SelectAll, input),
        KeyBinding::new("cmd-c", Copy, input),
        KeyBinding::new("cmd-x", Cut, input),
        KeyBinding::new("cmd-v", Paste, input),
        KeyBinding::new("cmd-left", Home, input),
        KeyBinding::new("cmd-right", End, input),
        KeyBinding::new("cmd-shift-left", ExtendHome, input),
        KeyBinding::new("cmd-shift-right", ExtendEnd, input),
    ]);
}

pub struct Changed;

pub struct SearchInput {
    focus: FocusHandle,
    text: String,
    // Both endpoints are UTF-8 byte offsets. The head follows the cursor.
    anchor: usize,
    head: usize,
    marked: Option<Range<usize>>,
    dragging: bool,
    painted: Option<LineGeometry>,
    placeholder: SharedString,
    text_size_rem: f32,
    line_height_rem: f32,
    ink: u32,
    muted: u32,
    selection_color: u32,
}

impl EventEmitter<Changed> for SearchInput {}

impl Focusable for SearchInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SearchInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            text: String::new(),
            anchor: 0,
            head: 0,
            marked: None,
            dragging: false,
            painted: None,
            placeholder: PLACEHOLDER.into(),
            text_size_rem: 23. / 16.,
            line_height_rem: 32. / 16.,
            ink: INK,
            muted: MUTED,
            selection_color: SELECTION,
        }
    }

    pub fn field(value: &str, placeholder: &str, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.text = value.to_owned();
        input.head = input.text.len();
        input.anchor = input.head;
        input.placeholder = placeholder.to_owned().into();
        input.text_size_rem = 14. / 16.;
        input.line_height_rem = 24. / 16.;
        input.ink = 0x1b1b1b;
        input.muted = 0x666666;
        input.selection_color = 0xcce4f7;
        input
    }

    pub fn chat(cx: &mut Context<Self>) -> Self {
        let mut input = Self::new(cx);
        input.placeholder = "向 Moin 提问，Enter 发送…".into();
        input
    }

    /// Set the text metrics in rems. UI text uses a 16 px baseline; AI text
    /// converts its own pixel size into rems so the two scales stay independent.
    pub fn set_rem_size(&mut self, text_size_rem: f32, line_height_rem: f32) {
        self.text_size_rem = text_size_rem;
        self.line_height_rem = line_height_rem;
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    pub fn focus(&self, window: &mut Window, _: &App) {
        self.focus.focus(window);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_value(String::new(), cx);
    }

    pub fn set_value(&mut self, value: String, cx: &mut Context<Self>) {
        if self.text != value || self.marked.is_some() {
            self.text = value;
            self.anchor = self.text.len();
            self.head = self.anchor;
            self.marked = None;
            self.painted = None;
            cx.emit(Changed);
            cx.notify();
        }
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    fn set_cursor(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        if !extend {
            self.anchor = offset;
        }
        self.head = offset;
        cx.notify();
    }

    fn previous_grapheme(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < self.head).then_some(i))
            .unwrap_or(0)
    }

    fn next_grapheme(&self) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > self.head).then_some(i))
            .unwrap_or(self.text.len())
    }

    fn target_range(&self, utf16_range: Option<Range<usize>>) -> Range<usize> {
        utf16_range
            .map(|range| byte_range(&self.text, range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection())
    }

    fn replace_selection(&mut self, text: &str, cx: &mut Context<Self>) {
        let target = self.target_range(None);
        self.commit(target, text, cx);
    }

    fn commit(&mut self, target: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text_changed = &self.text[target.clone()] != text;
        self.text.replace_range(target.clone(), text);
        self.anchor = target.start + text.len();
        self.head = self.anchor;
        let was_composing = self.marked.take().is_some();
        if text_changed || was_composing {
            cx.emit(Changed);
        }
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection();
        let destination = if selection.is_empty() {
            self.previous_grapheme()
        } else {
            selection.start
        };
        self.set_cursor(destination, false, cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection();
        let destination = if selection.is_empty() {
            self.next_grapheme()
        } else {
            selection.end
        };
        self.set_cursor(destination, false, cx);
    }

    fn extend_left(&mut self, _: &ExtendLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(self.previous_grapheme(), true, cx);
    }

    fn extend_right(&mut self, _: &ExtendRight, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(self.next_grapheme(), true, cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(0, false, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(self.text.len(), false, cx);
    }

    fn extend_home(&mut self, _: &ExtendHome, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(0, true, cx);
    }

    fn extend_end(&mut self, _: &ExtendEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.set_cursor(self.text.len(), true, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.anchor = 0;
        self.set_cursor(self.text.len(), true, cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.text.is_empty() && self.marked.is_none() {
            cx.propagate();
            return;
        }
        if self.selection().is_empty() {
            self.anchor = self.previous_grapheme();
        }
        self.replace_selection("", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection().is_empty() {
            self.anchor = self.next_grapheme();
        }
        self.replace_selection("", cx);
    }

    fn copy_selection(&self, cx: &mut Context<Self>) -> bool {
        let selected = &self.text[self.selection()];
        if selected.is_empty() {
            return false;
        }

        cx.write_to_clipboard(ClipboardItem::new_string(selected.to_owned()));
        true
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.copy_selection(cx) {
            self.replace_selection("", cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let single_line = text.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ");
            self.replace_selection(&single_line, cx);
        }
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(geometry) = self.painted.as_ref() else {
            return 0;
        };
        if self.text.is_empty() {
            return 0;
        }

        let offset = geometry
            .line
            .closest_index_for_x(position.x - geometry.origin.x);
        self.text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(self.text.len()))
            .min_by_key(|boundary| boundary.abs_diff(offset))
            .unwrap_or(0)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        self.dragging = true;
        self.set_cursor(self.index_at(event.position), event.modifiers.shift, cx);
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.dragging = false;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging {
            self.set_cursor(self.index_at(event.position), true, cx);
        }
    }
}

fn byte_offset(text: &str, utf16_offset: usize) -> usize {
    let mut code_units = 0;
    for (byte, character) in text.char_indices() {
        if code_units >= utf16_offset {
            return byte;
        }
        code_units += character.len_utf16();
    }
    text.len()
}

fn byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = byte_offset(text, range.start);
    start..byte_offset(text, range.end).max(start)
}

fn utf16_range(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

impl EntityInputHandler for SearchInput {
    fn text_for_range(
        &mut self,
        requested: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let bytes = byte_range(&self.text, requested);
        *actual = Some(utf16_range(&self.text, bytes.clone()));
        Some(self.text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: utf16_range(&self.text, self.selection()),
            reversed: self.head < self.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .clone()
            .map(|range| utf16_range(&self.text, range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.emit(Changed);
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target_range(range);
        self.commit(target, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target_range(range);
        if text.is_empty() {
            self.commit(target, text, cx);
            return;
        }

        let start = target.start;
        self.text.replace_range(target, text);
        self.marked = Some(start..start + text.len());
        let selected = selected
            .map(|range| byte_range(text, range))
            .unwrap_or_else(|| text.len()..text.len());
        self.anchor = start + selected.start;
        self.head = start + selected.end;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.painted.as_ref()?;
        let bytes = byte_range(&self.text, range);
        let x = |index| {
            (geometry.origin.x + geometry.line.x_for_index(index))
                .max(bounds.left())
                .min(bounds.right())
        };
        Some(Bounds::from_corners(
            point(x(bytes.start), bounds.top()),
            point(x(bytes.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_at(position);
        Some(self.text[..index].encode_utf16().count())
    }
}

impl Render for SearchInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .min_w(px(0.))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .text_size(rems(self.text_size_rem))
            .line_height(rems(self.line_height_rem))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::extend_left))
            .on_action(cx.listener(Self::extend_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::extend_home))
            .on_action(cx.listener(Self::extend_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(InputLine(cx.entity()))
    }
}

struct InputLine(Entity<SearchInput>);

struct LineGeometry {
    line: ShapedLine,
    origin: Point<Pixels>,
    scroll: Pixels,
}

impl IntoElement for InputLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputLine {
    type RequestLayoutState = ();
    type PrepaintState = Option<LineGeometry>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.0.read(cx);
        let style = window.text_style();
        let (display, color) = if input.text.is_empty() {
            (input.placeholder.clone(), rgb(input.muted).into())
        } else {
            (input.text.clone().into(), rgb(input.ink).into())
        };
        let display: gpui::SharedString = display;
        let run = TextRun {
            len: display.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match &input.marked {
            Some(marked) => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: run.len - marked.end,
                    ..run
                },
            ],
            None => vec![run],
        };
        let runs: Vec<_> = runs.into_iter().filter(|run| run.len > 0).collect();
        let line = window.text_system().shape_line(
            display,
            style.font_size.to_pixels(window.rem_size()),
            &runs,
            None,
        );
        let old_scroll = input.painted.as_ref().map_or(px(0.), |old| old.scroll);
        let caret_x = line.x_for_index(input.head);
        let available = (bounds.size.width - px(3.)).max(px(0.));
        let scroll = old_scroll
            .max(caret_x - available)
            .min(caret_x)
            .min((line.width - available).max(px(0.)));
        Some(LineGeometry {
            line,
            origin: point(bounds.left() - scroll, bounds.top()),
            scroll,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(geometry) = prepaint.take() else {
            return;
        };
        let input = self.0.read(cx);
        let focus = input.focus.clone();
        let selection = input.selection();
        let caret = input.head;
        let ink = input.ink;
        let selection_color = input.selection_color;
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);

        let x = |index| geometry.origin.x + geometry.line.x_for_index(index);
        let top = bounds.top() + px(4.);
        let bottom = bounds.bottom() - px(4.);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if !selection.is_empty() {
                let start = x(selection.start);
                let end = x(selection.end);
                window.paint_quad(fill(
                    Bounds::from_corners(point(start.min(end), top), point(start.max(end), bottom)),
                    rgb(selection_color),
                ));
            }
            let _ = geometry
                .line
                .paint(geometry.origin, window.line_height(), window, cx);
            if focus.is_focused(window) && selection.is_empty() {
                window.paint_quad(fill(
                    Bounds::new(point(x(caret), top), size(px(1.5), bottom - top)),
                    rgb(ink),
                ));
            }
        });
        self.0.update(cx, |input, _| input.painted = Some(geometry));
    }
}
