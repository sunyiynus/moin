//! Screen selection, annotation, pinned images and recording controls.
//! Every window owns its own GPUI state; capture work and GIF encoding run off the UI thread.
use super::{
    Moin, appearance, launcher_window,
    ui::{self, UiWindow},
};
use anyhow::{Context as _, Result, bail};
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
};
use moin::{capture as backend, config::CaptureConfig};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ACCENT: u32 = 0xdaf48a;
const MUTED: u32 = 0xbebfc5;
static ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptureKind {
    Screenshot,
    Gif,
}

#[derive(Default)]
struct CaptureWindows {
    recorder: Option<UiWindow<Recorder>>,
}
impl Global for CaptureWindows {}

fn stop_recording(cx: &mut App) -> bool {
    let handle = cx
        .try_global::<CaptureWindows>()
        .and_then(|state| state.recorder);
    handle.is_some_and(|handle| {
        handle
            .update(cx, |view, window, cx| {
                if view.result.is_some() {
                    return false;
                }
                view.stop.store(true, Ordering::Release);
                if view.countdown > 0 {
                    window.remove_window();
                } else {
                    view.finishing = true;
                    launcher_window::show(window);
                    cx.notify();
                }
                true
            })
            .unwrap_or(false)
    })
}

struct Lease;
impl Drop for Lease {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

struct Session {
    handles: RefCell<Vec<AnyWindowHandle>>,
    done: Cell<bool>,
    lease: RefCell<Option<Arc<Lease>>>,
    launcher: Option<UiWindow<Moin>>,
    config: CaptureConfig,
    kind: CaptureKind,
}

impl Session {
    fn close(&self, restore: bool, cx: &mut App) {
        if self.done.replace(true) {
            return;
        }
        self.lease.borrow_mut().take();
        let handles = self.handles.borrow_mut().drain(..).collect::<Vec<_>>();
        // A window root must not be borrowed again while its click handler is running.
        let launcher = self.launcher;
        cx.defer(move |cx| {
            for handle in handles {
                let _ = cx.update_window(handle, |_, window, _| window.remove_window());
            }
            if restore {
                restore_launcher(launcher, None, cx);
            }
        });
    }
}

fn restore_launcher(handle: Option<UiWindow<Moin>>, error: Option<String>, cx: &mut App) {
    if let Some(handle) = handle {
        let _ = handle.update(cx, |view, window, cx| {
            if let Some(error) = error {
                view.error = Some(error);
            }
            launcher_window::show(window);
            view.input.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        });
    }
}

pub(super) fn launch(
    kind: CaptureKind,
    config: CaptureConfig,
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    if kind == CaptureKind::Gif && stop_recording(cx) {
        launcher_window::hide(window);
        return Ok(());
    }
    if ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        bail!("已有截图或录制正在进行，请先完成或取消当前操作");
    }
    let lease = Arc::new(Lease);
    let launcher = UiWindow::from_window(window);
    let config = config.normalized();
    let include_cursor = config.include_cursor;
    launcher_window::hide(window);
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        // Allow the compositor to remove the launcher before freezing the desktop.
        executor.timer(Duration::from_millis(180)).await;
        let captured = executor
            .spawn(async move {
                backend::monitors()?
                    .into_iter()
                    .map(|rect| backend::capture_rect_with_cursor(rect, include_cursor))
                    .collect::<Result<Vec<_>>>()
            })
            .await;
        let _ = cx.update(|cx| match captured {
            Ok(frames) if !frames.is_empty() => {
                let session = Rc::new(Session {
                    handles: RefCell::new(Vec::new()),
                    done: Cell::new(false),
                    lease: RefCell::new(Some(lease)),
                    launcher,
                    config,
                    kind,
                });
                let displays = cx.displays();
                for (index, frame) in frames.into_iter().enumerate() {
                    let display = displays.get(index).or_else(|| displays.first());
                    let bounds = display.map_or_else(
                        || {
                            Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(frame.width as f32), px(frame.height as f32)),
                            )
                        },
                        |display| display.bounds(),
                    );
                    let rect = frame.rect;
                    let session_for_window = session.clone();
                    let opened = cx.open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(bounds)),
                            display_id: display.map(|d| d.id()),
                            kind: WindowKind::PopUp,
                            titlebar: None,
                            is_movable: false,
                            is_resizable: false,
                            is_minimizable: false,
                            window_background: WindowBackgroundAppearance::Opaque,
                            ..Default::default()
                        },
                        move |window, cx| {
                            configure_overlay(window, Some(rect));
                            replay_viewport_after_creation(window, cx);
                            let closing = session_for_window.clone();
                            window.on_window_should_close(cx, move |_, cx| {
                                closing.close(true, cx);
                                true
                            });
                            let selection =
                                cx.new(|cx| Selection::new(frame, session_for_window, window, cx));
                            ui::root(selection, window, cx)
                        },
                    );
                    match opened {
                        Ok(handle) => session.handles.borrow_mut().push(handle.into()),
                        Err(error) => {
                            session.close(false, cx);
                            restore_launcher(
                                launcher,
                                Some(format!("无法打开框选窗口 · {error:#}")),
                                cx,
                            );
                            break;
                        }
                    }
                }
            }
            Ok(_) => restore_launcher(launcher, Some("没有可用的显示器".into()), cx),
            Err(error) => restore_launcher(launcher, Some(format!("屏幕捕获失败 · {error:#}")), cx),
        });
    })
    .detach();
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Select,
    Rectangle,
    Pen,
}

#[derive(Clone, Debug)]
struct Mark {
    tool: Tool,
    points: Vec<Point<Pixels>>,
}

#[derive(Clone, Copy)]
enum Finish {
    Copy,
    Save,
    Pin,
}

struct Selection {
    frame: Arc<backend::CaptureFrame>,
    image: Arc<RenderImage>,
    session: Rc<Session>,
    focus: FocusHandle,
    anchor: Option<Point<Pixels>>,
    selection: Option<Bounds<Pixels>>,
    tool: Tool,
    marks: Vec<Mark>,
    drawing: bool,
    busy: bool,
    error: Option<String>,
}

impl Selection {
    fn new(
        frame: backend::CaptureFrame,
        session: Rc<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        ui::init(cx);
        let image = render_image(&frame);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            frame: Arc::new(frame),
            image,
            session,
            focus,
            anchor: None,
            selection: None,
            tool: Tool::Select,
            marks: Vec::new(),
            drawing: false,
            busy: false,
            error: None,
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.focus.focus(window, cx);
        let position = clamp_point(event.position, window.viewport_size());
        if self.selection.is_some() && event.click_count == 2 && self.tool == Tool::Select {
            self.confirm(Finish::Copy, window, cx);
            return;
        }
        if self.tool != Tool::Select && self.selection.is_some_and(|b| b.contains(&position)) {
            self.marks.push(Mark {
                tool: self.tool,
                points: vec![position],
            });
            self.drawing = true;
        } else {
            self.anchor = Some(position);
            self.selection = None;
            self.marks.clear();
            self.tool = Tool::Select;
        }
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let position = clamp_point(event.position, window.viewport_size());
        if self.drawing {
            if let (Some(mark), Some(bounds)) = (self.marks.last_mut(), self.selection) {
                let point = point(
                    position.x.clamp(bounds.left(), bounds.right()),
                    position.y.clamp(bounds.top(), bounds.bottom()),
                );
                if mark.tool == Tool::Rectangle && mark.points.len() > 1 {
                    mark.points[1] = point;
                } else {
                    mark.points.push(point);
                }
            }
        } else if let Some(anchor) = self.anchor {
            self.selection = Some(normalized_bounds(anchor, position));
        } else {
            return;
        }
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.mouse_move(
            &MouseMoveEvent {
                position: event.position,
                ..Default::default()
            },
            window,
            cx,
        );
        self.anchor = None;
        self.drawing = false;
        if self
            .selection
            .is_some_and(|b| b.size.width < px(3.) || b.size.height < px(3.))
        {
            self.selection = None;
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let primary = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        match key {
            "escape" if !self.busy => self.session.close(true, cx),
            "enter" => self.confirm(Finish::Copy, window, cx),
            "c" if primary => self.confirm(Finish::Copy, window, cx),
            "s" if primary => self.confirm(Finish::Save, window, cx),
            "p" if primary => self.confirm(Finish::Pin, window, cx),
            "z" if primary && !self.busy => {
                self.marks.pop();
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn confirm(&mut self, finish: Finish, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(bounds) = self.selection else {
            return;
        };
        let viewport = window.viewport_size();
        let region = region_for_selection(self.frame.rect, bounds, viewport);
        if self.session.kind == CaptureKind::Gif {
            let session = self.session.clone();
            session.close(false, cx);
            cx.defer(move |cx| {
                if let Err(error) =
                    Recorder::open(region, session.config.clone(), session.launcher, cx)
                {
                    restore_launcher(
                        session.launcher,
                        Some(format!("无法开始录制 · {error:#}")),
                        cx,
                    );
                }
            });
            return;
        }
        let pixel_bounds = pixel_selection(bounds, viewport, self.frame.width, self.frame.height);
        let mut frame = match self.frame.crop(
            pixel_bounds.0,
            pixel_bounds.1,
            pixel_bounds.2,
            pixel_bounds.3,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        annotate(
            &mut frame,
            &self.marks,
            bounds,
            viewport,
            self.frame.width,
            self.frame.height,
        );
        if matches!(finish, Finish::Pin) {
            match Pinned::open(frame, self.session.config.clone(), cx) {
                Ok(_) => self.session.close(false, cx),
                Err(error) => self.error = Some(format!("无法贴图 · {error:#}")),
            }
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        let config = self.session.config.clone();
        let work = cx.background_spawn(async move {
            match finish {
                Finish::Copy => backend::copy_image(&frame),
                Finish::Save => {
                    let path = output_path(&config, "png")?;
                    backend::save_png(&frame, &path)
                }
                Finish::Pin => unreachable!(),
            }
        });
        cx.spawn(async move |view, cx| {
            let result = work.await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(()) => view.session.close(false, cx),
                    Err(error) => view.error = Some(format!("截图处理失败 · {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toolbar(
        &self,
        bounds: Bounds<Pixels>,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let gif = self.session.kind == CaptureKind::Gif;
        let width = if gif { 410. } else { 660. };
        let height = if f32::from(viewport.width) < width + 24. {
            112.
        } else {
            62.
        };
        let left = bounds
            .left()
            .max(px(12.))
            .min((viewport.width - px(width + 12.)).max(px(12.)));
        let top = if bounds.bottom() + px(height + 12.) < viewport.height {
            bounds.bottom() + px(12.)
        } else {
            (bounds.top() - px(height)).max(px(12.))
        };
        let mut toolbar = div()
            .id("capture-toolbar")
            .absolute()
            .left(left)
            .top(top)
            .flex()
            .flex_wrap()
            .max_w((viewport.width - px(24.)).max(px(1.)))
            .items_center()
            .gap(px(5.))
            .p(px(7.))
            .rounded(px(10.))
            .bg(rgb(0x232326))
            .border_1()
            .border_color(rgba(0xffffff30))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if !gif {
            for (id, label, tool) in [
                ("select", "框选", Tool::Select),
                ("rectangle", "矩形", Tool::Rectangle),
                ("pen", "画笔", Tool::Pen),
            ] {
                toolbar = toolbar.child(
                    button(id, label, self.tool == tool)
                        .disabled(self.busy)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if !view.busy {
                                view.tool = tool;
                                cx.notify();
                            }
                        })),
                );
            }
            toolbar = toolbar.child(button("undo", "撤销", false).disabled(self.busy).on_click(
                cx.listener(|view, _, _, cx| {
                    view.marks.pop();
                    cx.notify();
                }),
            ));
            toolbar = toolbar.child(div().w(px(1.)).h(px(20.)).mx(px(4.)).bg(rgba(0xffffff20)));
            for (id, label, finish) in [
                ("pin", "贴图", Finish::Pin),
                ("save", "保存 PNG", Finish::Save),
            ] {
                toolbar = toolbar.child(button(id, label, false).disabled(self.busy).on_click(
                    cx.listener(move |view, _, window, cx| view.confirm(finish, window, cx)),
                ));
            }
        }
        toolbar
            .child(
                button(
                    "finish",
                    if self.busy {
                        "处理中…"
                    } else if gif {
                        "开始录制  ↵"
                    } else {
                        "复制  ↵"
                    },
                    true,
                )
                .disabled(self.busy)
                .on_click(
                    cx.listener(|view, _, window, cx| view.confirm(Finish::Copy, window, cx)),
                ),
            )
            .child(
                button("cancel", "取消 Esc", false)
                    .disabled(self.busy)
                    .on_click(cx.listener(|view, _, _, cx| {
                        if !view.busy {
                            view.session.close(true, cx);
                        }
                    })),
            )
    }
}

impl Render for Selection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.image.clone();
        let selected = self.selection;
        let marks = self.marks.clone();
        let viewport = window.viewport_size();
        let mut root = div()
            .id("capture-selection")
            .size_full()
            .relative()
            .overflow_hidden()
            .font_family(appearance::font_family())
            .text_size(px(13.))
            .text_color(rgb(0xffffff))
            .track_focus(&self.focus)
            .cursor(CursorStyle::Crosshair)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
                        if let Some(area) = selected {
                            let origin = bounds.origin;
                            let outer = bounds.size;
                            for mask in [
                                Bounds::new(origin, size(outer.width, area.top())),
                                Bounds::new(
                                    origin + point(px(0.), area.bottom()),
                                    size(outer.width, (outer.height - area.bottom()).max(px(0.))),
                                ),
                                Bounds::new(
                                    origin + point(px(0.), area.top()),
                                    size(area.left(), area.size.height),
                                ),
                                Bounds::new(
                                    origin + point(area.right(), area.top()),
                                    size(
                                        (outer.width - area.right()).max(px(0.)),
                                        area.size.height,
                                    ),
                                ),
                            ] {
                                window.paint_quad(fill(mask, rgba(0x00000077)));
                            }
                            window.paint_quad(outline(
                                Bounds::new(origin + area.origin, area.size),
                                rgb(ACCENT),
                                BorderStyle::default(),
                            ));
                            for mark in marks {
                                paint_mark(&mark, origin, window);
                            }
                        } else {
                            window.paint_quad(fill(bounds, rgba(0x00000044)));
                        }
                    },
                )
                .absolute()
                .size_full(),
            );
        if let Some(bounds) = selected.filter(|_| self.anchor.is_none()) {
            let (_, _, width, height) =
                pixel_selection(bounds, viewport, self.frame.width, self.frame.height);
            root = root.child(
                div()
                    .absolute()
                    .left(bounds.left())
                    .top((bounds.top() - px(30.)).max(px(4.)))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(5.))
                    .bg(rgb(0x232326))
                    .child(format!("{width} × {height}")),
            );
            root = root.child(self.toolbar(bounds, viewport, cx));
        } else {
            root = root.child(
                div()
                    .absolute()
                    .top(px(28.))
                    .left(px(24.))
                    .px(px(18.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .max_w((viewport.width - px(48.)).max(px(1.)))
                    .bg(rgb(0x232326))
                    .border_1()
                    .border_color(rgba(0xffffff25))
                    .child(if self.session.kind == CaptureKind::Gif {
                        "拖动选择 GIF 录制区域 · Esc 取消"
                    } else {
                        "拖动选择截图区域 · Enter 复制 · Ctrl+S 保存 · Ctrl+P 贴图 · Esc 取消"
                    }),
            );
        }
        if let Some(error) = &self.error {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(20.))
                    .left(px(24.))
                    .right(px(24.))
                    .p(px(12.))
                    .bg(rgb(0x462d2b))
                    .rounded(px(8.))
                    .child(error.clone()),
            );
        }
        ui::dark(root, cx)
    }
}

fn button(id: &'static str, label: impl Into<SharedString>, active: bool) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .tab_stop(false)
        .label(label)
        .debug_selector(move || id.into())
        .h(px(36.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(rgb(if active { ACCENT } else { 0xeeeeee }))
        .bg(rgba(if active { 0xdaf48a18 } else { 0xffffff08 }))
}

fn render_image(frame: &backend::CaptureFrame) -> Arc<RenderImage> {
    let mut bgra = frame.rgba.clone();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(frame.width, frame.height, bgra)
        .expect("validated screen frame");
    Arc::new(RenderImage::new([image::Frame::new(image)]))
}

fn normalized_bounds(a: Point<Pixels>, b: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(a.x.min(b.x), a.y.min(b.y)),
        size((a.x - b.x).abs(), (a.y - b.y).abs()),
    )
}

fn clamp_point(p: Point<Pixels>, viewport: Size<Pixels>) -> Point<Pixels> {
    point(
        p.x.clamp(px(0.), viewport.width),
        p.y.clamp(px(0.), viewport.height),
    )
}

fn pixel_selection(
    bounds: Bounds<Pixels>,
    viewport: Size<Pixels>,
    width: u32,
    height: u32,
) -> (u32, u32, u32, u32) {
    let sx = width as f32 / f32::from(viewport.width).max(1.);
    let sy = height as f32 / f32::from(viewport.height).max(1.);
    let x = (f32::from(bounds.left()) * sx)
        .floor()
        .clamp(0., width.saturating_sub(1) as f32) as u32;
    let y = (f32::from(bounds.top()) * sy)
        .floor()
        .clamp(0., height.saturating_sub(1) as f32) as u32;
    let right = (f32::from(bounds.right()) * sx)
        .ceil()
        .clamp((x + 1) as f32, width as f32) as u32;
    let bottom = (f32::from(bounds.bottom()) * sy)
        .ceil()
        .clamp((y + 1) as f32, height as f32) as u32;
    (x, y, right - x, bottom - y)
}

fn region_for_selection(
    rect: backend::ScreenRect,
    bounds: Bounds<Pixels>,
    viewport: Size<Pixels>,
) -> backend::ScreenRect {
    let (x, y, width, height) = pixel_selection(bounds, viewport, rect.width, rect.height);
    backend::ScreenRect {
        x: rect.x + x as i32,
        y: rect.y + y as i32,
        width,
        height,
    }
}

fn paint_mark(mark: &Mark, origin: Point<Pixels>, window: &mut Window) {
    if mark.points.len() < 2 {
        return;
    }
    let mut path = PathBuilder::stroke(px(2.));
    let first = mark.points[0] + origin;
    path.move_to(first);
    if mark.tool == Tool::Rectangle {
        let last = mark.points[1] + origin;
        path.line_to(point(last.x, first.y));
        path.line_to(last);
        path.line_to(point(first.x, last.y));
        path.line_to(first);
    } else {
        for p in mark.points.iter().skip(1) {
            path.line_to(*p + origin);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, rgb(0xff625e));
    }
}

fn annotate(
    frame: &mut backend::CaptureFrame,
    marks: &[Mark],
    selection: Bounds<Pixels>,
    viewport: Size<Pixels>,
    width: u32,
    height: u32,
) {
    let sx = width as f32 / f32::from(viewport.width).max(1.);
    let sy = height as f32 / f32::from(viewport.height).max(1.);
    let (offset_x, offset_y, _, _) = pixel_selection(selection, viewport, width, height);
    let mapped = |p: Point<Pixels>| {
        (
            (f32::from(p.x) * sx).round() as i32 - offset_x as i32,
            (f32::from(p.y) * sy).round() as i32 - offset_y as i32,
        )
    };
    let radius = sx.max(sy).round().clamp(1., 5.) as i32;
    for mark in marks {
        if mark.points.len() < 2 {
            continue;
        }
        if mark.tool == Tool::Rectangle {
            let a = mapped(mark.points[0]);
            let b = mapped(mark.points[1]);
            for (start, end) in [
                (a, (b.0, a.1)),
                ((b.0, a.1), b),
                (b, (a.0, b.1)),
                ((a.0, b.1), a),
            ] {
                raster_line(frame, start, end, radius);
            }
        } else {
            for pair in mark.points.windows(2) {
                raster_line(frame, mapped(pair[0]), mapped(pair[1]), radius);
            }
        }
    }
}

fn raster_line(frame: &mut backend::CaptureFrame, a: (i32, i32), b: (i32, i32), radius: i32) {
    let (mut x, mut y) = a;
    let dx = (b.0 - x).abs();
    let dy = -(b.1 - y).abs();
    let sx = if x < b.0 { 1 } else { -1 };
    let sy = if y < b.1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        for py in (y - radius)..=(y + radius) {
            for px in (x - radius)..=(x + radius) {
                if px >= 0 && py >= 0 && px < frame.width as i32 && py < frame.height as i32 {
                    let offset = (py as usize * frame.width as usize + px as usize) * 4;
                    frame.rgba[offset..offset + 4].copy_from_slice(&[255, 98, 94, 255]);
                }
            }
        }
        if x == b.0 && y == b.1 {
            break;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += sx;
        }
        if twice <= dx {
            error += dx;
            y += sy;
        }
    }
}

fn output_path(config: &CaptureConfig, extension: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(&config.output_dir)
        .with_context(|| format!("无法创建保存目录 {}", config.output_dir.display()))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(config.output_dir.join(format!("Moin-{stamp}.{extension}")))
}

struct Pinned {
    frame: Arc<backend::CaptureFrame>,
    image: Arc<RenderImage>,
    config: CaptureConfig,
    focus: FocusHandle,
    notice: Option<String>,
}

impl Pinned {
    fn open(frame: backend::CaptureFrame, config: CaptureConfig, cx: &mut App) -> Result<()> {
        let ratio = frame.width as f32 / frame.height.max(1) as f32;
        let width = (frame.width as f32).clamp(300., 800.);
        let height = (width / ratio).clamp(100., 650.);
        cx.open_window(
            WindowOptions {
                titlebar: None,
                kind: WindowKind::PopUp,
                window_background: WindowBackgroundAppearance::Opaque,
                window_bounds: Some(WindowBounds::centered(
                    size(px(width), px(height + 42.)),
                    cx,
                )),
                window_min_size: Some(size(px(300.), px(100.))),
                is_minimizable: false,
                ..Default::default()
            },
            move |window, cx| {
                configure_overlay(window, None);
                replay_viewport_after_creation(window, cx);
                window.set_window_title("Moin · 贴图");
                let pinned = cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window, cx);
                    Self {
                        image: render_image(&frame),
                        frame: Arc::new(frame),
                        config,
                        focus,
                        notice: None,
                    }
                });
                ui::root(pinned, window, cx)
            },
        )?;
        Ok(())
    }

    fn export(&mut self, save: bool, cx: &mut Context<Self>) {
        let frame = self.frame.clone();
        let config = self.config.clone();
        let work = cx.background_spawn(async move {
            if save {
                let path = output_path(&config, "png")?;
                backend::save_png(&frame, &path)?;
                Ok(format!("已保存 {}", path.display()))
            } else {
                backend::copy_image(&frame)?;
                Ok("已复制".to_owned())
            }
        });
        cx.spawn(async move |view, cx| {
            let result: Result<String> = work.await;
            let _ = view.update(cx, |view, cx| {
                view.notice = Some(result.unwrap_or_else(|e| format!("{e:#}")));
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for Pinned {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x202023))
            .text_color(rgb(0xffffff))
            .text_size(px(12.))
            .font_family(appearance::font_family())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                } else if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
                    match event.keystroke.key.as_str() {
                        "c" => view.export(false, cx),
                        "s" => view.export(true, cx),
                        _ => {}
                    }
                }
            }))
            .child(
                div()
                    .h(px(42.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .gap(px(4.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(self.notice.clone().unwrap_or_else(|| "Moin · 贴图".into())),
                    )
                    .child(
                        button("copy", "复制", false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|view, _, _, cx| view.export(false, cx))),
                    )
                    .child(
                        button("save", "保存", false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|view, _, _, cx| view.export(true, cx))),
                    )
                    .child(
                        button("close", "关闭", false)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
            .child(
                img(self.image.clone())
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .object_fit(ObjectFit::Contain),
            );
        ui::dark(content, cx)
    }
}

struct Recorder {
    config: CaptureConfig,
    region: backend::ScreenRect,
    launcher: Option<UiWindow<Moin>>,
    stop: Arc<AtomicBool>,
    elapsed: Arc<AtomicU32>,
    countdown: u8,
    recording: bool,
    finishing: bool,
    result: Option<Result<PathBuf, String>>,
    focus: FocusHandle,
    _lease: Option<Arc<Lease>>,
    _task: Option<Task<()>>,
}

impl Recorder {
    fn open(
        region: backend::ScreenRect,
        config: CaptureConfig,
        launcher: Option<UiWindow<Moin>>,
        cx: &mut App,
    ) -> Result<()> {
        // Session close releases the selection lease before this deferred callback.
        ACTIVE.store(true, Ordering::Release);
        let lease = Arc::new(Lease);
        let toolbar_size = size(px(448.), px(104.));
        let screens = backend::monitors()?;
        let display_index = screens.iter().position(|screen| {
            let center_x = region.x as i64 + region.width as i64 / 2;
            let center_y = region.y as i64 + region.height as i64 / 2;
            center_x >= screen.x as i64
                && center_x < screen.x as i64 + screen.width as i64
                && center_y >= screen.y as i64
                && center_y < screen.y as i64 + screen.height as i64
        });
        let displays = cx.displays();
        let display_id = display_index
            .and_then(|index| displays.get(index))
            .map(|display| display.id());
        let handle = cx.open_window(
            WindowOptions {
                titlebar: None,
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Blurred,
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    display_id,
                    toolbar_size,
                    cx,
                ))),
                display_id,
                ..Default::default()
            },
            move |window, cx| {
                configure_overlay(window, None);
                replay_viewport_after_creation(window, cx);
                position_recording_toolbar(window, region);
                window.set_background_appearance(WindowBackgroundAppearance::Blurred);
                let stop_on_close = Arc::new(AtomicBool::new(false));
                let stop = stop_on_close.clone();
                window.on_window_should_close(cx, move |_, _| {
                    stop_on_close.store(true, Ordering::Release);
                    true
                });
                let recorder = cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window, cx);
                    let mut this = Self {
                        config,
                        region,
                        launcher,
                        stop,
                        elapsed: Arc::new(AtomicU32::new(0)),
                        countdown: 3,
                        recording: false,
                        finishing: false,
                        result: None,
                        focus,
                        _lease: Some(lease),
                        _task: None,
                    };
                    this.start(cx);
                    this
                });
                ui::root(recorder, window, cx)
            },
        )?;
        cx.set_global(CaptureWindows {
            recorder: Some(UiWindow::from_handle(handle)),
        });
        Ok(())
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let config = self.config.clone();
        let region = self.region;
        let stop = self.stop.clone();
        let elapsed = self.elapsed.clone();
        let lease = self
            ._lease
            .as_ref()
            .expect("recording owns capture lease")
            .clone();
        let executor = cx.background_executor().clone();
        self._task = Some(cx.spawn(async move |view, cx| {
            for remaining in (1..=3).rev() {
                let _ = view.update(cx, |view, cx| {
                    view.countdown = remaining;
                    cx.notify();
                });
                executor.timer(Duration::from_secs(1)).await;
                if stop.load(Ordering::Acquire) {
                    return;
                }
            }
            let _ = view.update(cx, |view, cx| {
                view.countdown = 0;
                view.recording = true;
                cx.notify();
            });
            let elapsed_work = elapsed.clone();
            let stop_work = stop.clone();
            let (sender, receiver) = async_channel::bounded(1);
            executor
                .spawn(async move {
                    let _lease = lease;
                    let result = (|| {
                        let path = output_path(&config, "gif")?;
                        backend::record_gif(
                            region,
                            &path,
                            backend::RecordingOptions {
                                fps: config.gif_fps,
                                max_seconds: config.gif_max_seconds,
                                include_cursor: config.include_cursor,
                            },
                            stop_work,
                            elapsed_work,
                        )?;
                        Ok(path)
                    })();
                    let _ = sender.send(result).await;
                })
                .detach();
            loop {
                match receiver.try_recv() {
                    Ok(result) => {
                        let _ = view.update(cx, |view, cx| {
                            view.recording = false;
                            view.finishing = false;
                            view._lease.take();
                            view.result = Some(result.map_err(|e: anyhow::Error| format!("{e:#}")));
                            cx.notify();
                        });
                        return;
                    }
                    Err(async_channel::TryRecvError::Closed) => return,
                    Err(async_channel::TryRecvError::Empty) => {}
                }
                executor.timer(Duration::from_millis(150)).await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    stop.store(true, Ordering::Release);
                    return;
                }
            }
        }));
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Render for Recorder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let seconds = self.elapsed.load(Ordering::Acquire) / 1000;
        let label = if self.countdown > 0 {
            format!("{} 秒后开始录制", self.countdown)
        } else if self.finishing {
            "正在保存 GIF…".into()
        } else if matches!(self.result, Some(Err(_))) {
            "GIF 录制失败".into()
        } else if self.result.is_some() {
            "GIF 录制完成".into()
        } else {
            format!("录制中  {:02}:{:02}", seconds / 60, seconds % 60)
        };
        let detail = match &self.result {
            Some(Ok(path)) => path.display().to_string(),
            Some(Err(error)) => error.clone(),
            None => format!(
                "{} × {} · {} fps · 最长 {} 秒",
                self.region.width,
                self.region.height,
                self.config.gif_fps,
                self.config.gif_max_seconds
            ),
        };
        let mut root = div()
            .size_full()
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(18.))
            .rounded(px(10.))
            .bg(rgba(0x1e1e22ed))
            .border_1()
            .border_color(rgba(0xffffff25))
            .font_family(appearance::font_family())
            .text_color(rgb(0xffffff))
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if view.countdown > 0 || view.result.is_some() {
                        view.stop.store(true, Ordering::Release);
                        if view.countdown > 0 {
                            let launcher = view.launcher;
                            cx.defer(move |cx| restore_launcher(launcher, None, cx));
                        }
                        window.remove_window();
                    } else {
                        view.stop.store(true, Ordering::Release);
                        view.finishing = true;
                        cx.notify();
                    }
                }
            }))
            .child(
                div()
                    .size(px(9.))
                    .rounded_full()
                    .bg(rgb(if self.result.is_some() {
                        ACCENT
                    } else {
                        0xff625e
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(div().text_size(px(15.)).child(label))
                    .child(div().truncate().text_color(rgb(MUTED)).child(detail)),
            );
        if let Some(Ok(path)) = &self.result {
            let path = path.clone();
            root = root.child(
                button("show", "打开", false)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.open_url(
                            url::Url::from_file_path(&path)
                                .map(|u| u.to_string())
                                .unwrap_or_default()
                                .as_str(),
                        )
                    }),
            );
        }
        let content = root.child(
            button(
                "stop",
                if self.result.is_some() {
                    "关闭"
                } else if self.countdown > 0 {
                    "取消"
                } else if self.finishing {
                    "保存中"
                } else {
                    "停止 ■"
                },
                true,
            )
            .disabled(self.finishing)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|view, _, window, cx| {
                view.stop.store(true, Ordering::Release);
                if view.countdown > 0 || view.result.is_some() {
                    if view.countdown > 0 {
                        let launcher = view.launcher;
                        cx.defer(move |cx| restore_launcher(launcher, None, cx));
                    }
                    window.remove_window();
                } else {
                    view.finishing = true;
                    cx.notify();
                }
            })),
        );
        ui::dark(content, cx)
    }
}

/// Native popup framing is kept separate from launcher acrylic: the screenshot
/// must be an exact, opaque view of the frozen pixels, without rounded insets.
fn configure_overlay(window: &mut Window, rect: Option<backend::ScreenRect>) {
    launcher_window::configure(window);
    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{Foundation::HWND, Graphics::Dwm::*, UI::WindowsAndMessaging::*};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                unsafe {
                    let hwnd = HWND(handle.hwnd.get() as *mut _);
                    let corners = DWMWCP_DONOTROUND;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_WINDOW_CORNER_PREFERENCE,
                        (&raw const corners).cast(),
                        std::mem::size_of_val(&corners) as u32,
                    );
                    let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
                    let extended = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                    SetWindowLongW(hwnd, GWL_EXSTYLE, (extended | WS_EX_TOOLWINDOW.0) as i32);
                    if let Some(rect) = rect {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            rect.x,
                            rect.y,
                            rect.width as i32,
                            rect.height as i32,
                            SWP_NOACTIVATE | SWP_FRAMECHANGED,
                        );
                    } else {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                        );
                    }
                    // Native resize notifications during GPUI's window constructor cannot
                    // borrow its Window yet. Replay after construction so hit testing and
                    // selection coordinates use the actual client size and monitor DPI.
                    let mut client = windows::Win32::Foundation::RECT::default();
                    if GetClientRect(hwnd, &mut client).is_ok() {
                        let packed = ((client.right - client.left) as u32 & 0xffff)
                            | (((client.bottom - client.top) as u32 & 0xffff) << 16);
                        let _ = PostMessageW(
                            Some(hwnd),
                            WM_SIZE,
                            windows::Win32::Foundation::WPARAM(SIZE_RESTORED as usize),
                            windows::Win32::Foundation::LPARAM(packed as isize),
                        );
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(handle) = handle.as_raw() {
                unsafe {
                    let view = handle.ns_view.as_ptr().cast::<objc::runtime::Object>();
                    let native: cocoa::base::id = msg_send![view, window];
                    let _: () = msg_send![native,setSharingType:0usize];
                    let _: () = msg_send![native,setLevel:25isize];
                    let _: () = msg_send![native,setHasShadow:cocoa::base::NO];
                    let layer: cocoa::base::id = msg_send![view, layer];
                    let _: () = msg_send![layer,setCornerRadius:0.0f64];
                    if let Some(rect) = rect {
                        use cocoa::{
                            appkit::NSScreen,
                            foundation::{NSArray, NSPoint, NSRect, NSSize},
                        };
                        let screens = NSScreen::screens(cocoa::base::nil);
                        let primary = NSArray::objectAtIndex(screens, 0);
                        let primary_frame: NSRect = msg_send![primary, frame];
                        let frame = NSRect::new(
                            NSPoint::new(
                                rect.x as f64,
                                primary_frame.size.height - rect.y as f64 - rect.height as f64,
                            ),
                            NSSize::new(rect.width as f64, rect.height as f64),
                        );
                        let _: () = msg_send![native,setFrame:frame display:cocoa::base::YES];
                    }
                }
            }
        }
        let _ = rect;
    }
}

fn replay_viewport_after_creation(window: &Window, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        use objc::{msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = window.window_handle();
        cx.defer(move |cx| {
            // Resolve a live window at execution time. Invoke outside its update
            // closure so GPUI's resize callback can borrow the Window again.
            let raw = cx.update_window(handle, |_, window, _| {
                HasWindowHandle::window_handle(window)
                    .ok()
                    .map(|h| h.as_raw())
            });
            if let Ok(Some(RawWindowHandle::AppKit(raw))) = raw {
                unsafe {
                    let view = raw.ns_view.as_ptr().cast::<objc::runtime::Object>();
                    let _: () = msg_send![view, viewDidChangeBackingProperties];
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, cx);
}

fn position_recording_toolbar(window: &Window, region: backend::ScreenRect) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{Foundation::HWND, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                unsafe {
                    let hwnd = HWND(handle.hwnd.get() as *mut _);
                    let rect = windows::Win32::Foundation::RECT {
                        left: region.x,
                        top: region.y,
                        right: region.x + region.width as i32,
                        bottom: region.y + region.height as i32,
                    };
                    let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
                    let mut info = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    let _ = GetMonitorInfoW(monitor, &mut info);
                    let work = info.rcWork;
                    let scale = window.scale_factor();
                    let width = (448. * scale) as i32;
                    let height = (104. * scale) as i32;
                    let x = region
                        .x
                        .clamp(work.left, (work.right - width).max(work.left));
                    let preferred = if region.y - height - 12 >= work.top {
                        region.y - height - 12
                    } else {
                        region.y + region.height as i32 + 12
                    };
                    let y = preferred.clamp(work.top, (work.bottom - height).max(work.top));
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use cocoa::{
            appkit::NSScreen,
            foundation::{NSArray, NSPoint, NSRect},
        };
        use objc::{msg_send, sel, sel_impl};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let screen = backend::monitors().ok().and_then(|screens| {
            screens.into_iter().find(|screen| {
                region.x >= screen.x
                    && region.y >= screen.y
                    && region.x as i64 + region.width as i64
                        <= screen.x as i64 + screen.width as i64
                    && region.y as i64 + region.height as i64
                        <= screen.y as i64 + screen.height as i64
            })
        });
        if let (Some(screen), Ok(handle)) = (screen, HasWindowHandle::window_handle(window)) {
            if let RawWindowHandle::AppKit(handle) = handle.as_raw() {
                unsafe {
                    let view = handle.ns_view.as_ptr().cast::<objc::runtime::Object>();
                    let native: cocoa::base::id = msg_send![view, window];
                    let frame: NSRect = msg_send![native, frame];
                    let screens = NSScreen::screens(cocoa::base::nil);
                    let primary = NSArray::objectAtIndex(screens, 0);
                    let primary_frame: NSRect = msg_send![primary, frame];
                    let width = frame.size.width;
                    let height = frame.size.height;
                    let x = (region.x as f64).clamp(
                        screen.x as f64,
                        (screen.x as f64 + screen.width as f64 - width).max(screen.x as f64),
                    );
                    let above = region.y as f64 - height - 12.;
                    let preferred = if above >= screen.y as f64 + 28. {
                        above
                    } else {
                        region.y as f64 + region.height as f64 + 12.
                    };
                    let y = preferred.clamp(
                        screen.y as f64 + 28.,
                        (screen.y as f64 + screen.height as f64 - height - 12.)
                            .max(screen.y as f64 + 28.),
                    );
                    let origin = NSPoint::new(x, primary_frame.size.height - y - height);
                    let _: () = msg_send![native,setFrameOrigin:origin];
                }
            }
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (window, region);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn selection_maps_retina_and_negative_monitor_origins() {
        let area = normalized_bounds(point(px(210.), px(130.)), point(px(10.), px(30.)));
        assert_eq!(
            pixel_selection(area, size(px(400.), px(300.)), 800, 600),
            (20, 60, 400, 200)
        );
        let region = region_for_selection(
            backend::ScreenRect {
                x: -1920,
                y: -240,
                width: 1920,
                height: 1080,
            },
            area,
            size(px(960.), px(540.)),
        );
        assert_eq!(
            region,
            backend::ScreenRect {
                x: -1900,
                y: -180,
                width: 400,
                height: 200
            }
        );
    }

    #[::core::prelude::v1::test]
    fn annotations_are_clipped_and_do_not_change_unmarked_pixels() {
        let mut frame = backend::CaptureFrame {
            rect: backend::ScreenRect {
                x: 0,
                y: 0,
                width: 20,
                height: 20,
            },
            width: 20,
            height: 20,
            rgba: vec![255; 20 * 20 * 4],
        };
        raster_line(&mut frame, (-2, 1), (5, 1), 1);
        assert_eq!(&frame.rgba[0..4], &[255, 98, 94, 255]);
        assert_eq!(&frame.rgba[(19 * 20 + 19) * 4..], &[255, 255, 255, 255]);
        let image = render_image(&frame);
        assert_eq!(&image.as_bytes(0).unwrap()[0..4], &[94, 98, 255, 255]);
    }

    #[::core::prelude::v1::test]
    fn selector_drag_annotation_toolbar_and_cancel_work_without_desktop_access() {
        let mut app = TestAppContext::single();
        let session = Rc::new(Session {
            handles: RefCell::new(vec![]),
            done: Cell::new(false),
            lease: RefCell::new(None),
            launcher: None,
            config: CaptureConfig::default(),
            kind: CaptureKind::Screenshot,
        });
        let original_session = session.clone();
        let (view, cx) = app.add_window_view(move |window, cx| {
            Selection::new(
                backend::CaptureFrame {
                    rect: backend::ScreenRect {
                        x: -640,
                        y: 0,
                        width: 640,
                        height: 480,
                    },
                    width: 640,
                    height: 480,
                    rgba: vec![255; 640 * 480 * 4],
                },
                session,
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.simulate_mouse_down(
            point(px(40.), px(80.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(480.), px(360.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(480.), px(360.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        let selected = cx.read(|cx| view.read(cx).selection.unwrap());
        assert_eq!(
            selected,
            Bounds::new(point(px(40.), px(80.)), size(px(440.), px(280.)))
        );
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        let pen = cx
            .debug_bounds("pen")
            .expect("annotation button is visible");
        cx.simulate_click(pen.center(), Modifiers::default());
        cx.read(|cx| {
            assert_eq!(view.read(cx).tool, Tool::Pen);
            assert_eq!(view.read(cx).selection, Some(selected));
        });
        cx.simulate_mouse_down(
            point(px(100.), px(140.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(220.), px(200.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(220.), px(200.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.read(|cx| assert_eq!(view.read(cx).marks.len(), 1));
        cx.simulate_keystrokes("ctrl-z");
        cx.read(|cx| assert!(view.read(cx).marks.is_empty()));
        cx.simulate_keystrokes("escape");
        assert!(original_session.done.get());
    }
}

#[cfg(all(test, windows))]
#[path = "capture_native_qa.rs"]
mod native_qa_tests;
