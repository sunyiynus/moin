//! Opt-in Win32 framing and native-font toolbar checks. All windows stay hidden.
//! No desktop pixels, clipboard contents, or input are read or changed.
use super::*;
use futures::FutureExt;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetClientRect, GetWindowLongW, GetWindowRect, IsWindowVisible, WS_EX_TOPMOST,
    },
};

struct NativeFixture {
    selection: Entity<Selection>,
    toolbar: bool,
    observed: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

impl Render for NativeFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let observations = self.observed.clone();
        let mut root = div().size_full().relative();
        if self.toolbar {
            let area = Bounds::new(
                point(px(30.), (viewport.height - px(110.)).max(px(30.))),
                size(
                    (viewport.width - px(60.)).max(px(1.)),
                    px(75.).min(viewport.height),
                ),
            );
            let toolbar = self.selection.update(cx, |view, cx| {
                view.toolbar(area, viewport, cx).into_any_element()
            });
            root = root.child(
                div()
                    .size_full()
                    .relative()
                    .on_children_prepainted(move |bounds, _, _| {
                        *observations.borrow_mut() = bounds;
                    })
                    .child(toolbar),
            );
        }
        root
    }
}

fn raw_hwnd(window: &Window) -> Result<HWND, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(HWND(handle.hwnd.get() as *mut _)),
        _ => Err("native capture QA did not create a Win32 window".into()),
    }
}

fn hidden_topmost_supported() -> Result<bool, String> {
    use windows::{Win32::UI::WindowsAndMessaging::*, core::w};
    struct HiddenWindow(HWND);
    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
    unsafe {
        // An independent Win32 baseline distinguishes an application regression
        // from desktop environments which ignore z-order changes on hidden HWNDs.
        let window = HiddenWindow(
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("Moin hidden z-order baseline"),
                WS_POPUP,
                0,
                0,
                96,
                64,
                None,
                None,
                None,
                None,
            )
            .map_err(|error| format!("create hidden z-order baseline: {error}"))?,
        );
        SetWindowPos(
            window.0,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .map_err(|error| format!("set hidden baseline topmost: {error}"))?;
        if IsWindowVisible(window.0).as_bool() || GetForegroundWindow() == window.0 {
            return Err("z-order baseline unexpectedly became visible or active".into());
        }
        Ok(GetWindowLongW(window.0, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0)
    }
}

fn synthetic_frame() -> backend::CaptureFrame {
    backend::CaptureFrame {
        rect: backend::ScreenRect {
            x: 0,
            y: 0,
            width: 96,
            height: 64,
        },
        width: 96,
        height: 64,
        rgba: vec![255; 96 * 64 * 4],
    }
}

fn open_fixture(
    cx: &mut App,
    kind: CaptureKind,
    logical_size: Size<Pixels>,
    toolbar: bool,
) -> Result<
    (
        WindowHandle<NativeFixture>,
        Rc<RefCell<Vec<Bounds<Pixels>>>>,
    ),
    String,
> {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let captured = observed.clone();
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(40.), px(40.)),
                    logical_size,
                ))),
                titlebar: None,
                kind: WindowKind::PopUp,
                is_resizable: false,
                show: false,
                focus: false,
                ..Default::default()
            },
            move |window, cx| {
                let session = Rc::new(Session {
                    handles: RefCell::new(Vec::new()),
                    done: Cell::new(false),
                    lease: RefCell::new(None),
                    launcher: None,
                    config: CaptureConfig::default(),
                    kind,
                });
                let selection = cx.new(|cx| Selection::new(synthetic_frame(), session, window, cx));
                cx.new(|_| NativeFixture {
                    selection,
                    toolbar,
                    observed: captured,
                })
            },
        )
        .map_err(|error| format!("open hidden capture fixture: {error:#}"))?;
    Ok((handle, observed))
}

async fn native_cases(cx: &mut AsyncApp) -> Result<Vec<String>, String> {
    let mut reports = Vec::new();
    let topmost_supported = hidden_topmost_supported()?;
    if !topmost_supported {
        reports.push("environment: an independent hidden Win32 STATIC popup also ignores HWND_TOPMOST despite SetWindowPos succeeding; skipping only hidden WS_EX_TOPMOST property assertions, retaining geometry/DPI/toolbar/visibility checks".into());
    }
    // Read display geometry only; do not capture any screen pixels.
    let monitors =
        backend::monitors().map_err(|error| format!("read monitor bounds: {error:#}"))?;
    for (index, monitor) in monitors.into_iter().take(4).enumerate() {
        let rect = backend::ScreenRect {
            x: monitor.x + 17,
            y: monitor.y + 19,
            width: monitor.width.saturating_sub(34).min(640).max(1),
            height: monitor.height.saturating_sub(38).min(480).max(1),
        };
        let (handle, _) = cx
            .update(|cx| open_fixture(cx, CaptureKind::Screenshot, size(px(480.), px(320.)), false))
            .map_err(|error| error.to_string()) ?;
        cx.update_window(handle.into(), |_, window, _| -> Result<(), String> {
            let hwnd = raw_hwnd(window)?;
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                return Err(
                    "capture fixture unexpectedly became visible before configuration".into(),
                );
            }
            configure_overlay(window, Some(rect));
            Ok(())
        })
        .map_err(|error| error.to_string())??;
        // Native geometry changes dispatch GPUI resize/move updates on the UI loop.
        cx.background_executor()
            .timer(Duration::from_millis(80))
            .await;
        cx.update_window(handle.into(), |_, window, cx| -> Result<(), String> {
            let hwnd = raw_hwnd(window)?;
            window.draw(cx).clear(cx);
            let mut outer = RECT::default();
            let mut client = RECT::default();
            unsafe {
                GetWindowRect(hwnd, &mut outer).map_err(|error| error.to_string())?;
                GetClientRect(hwnd, &mut client).map_err(|error| error.to_string())?;
            }
            let topmost = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
            if topmost_supported && !topmost {
                return Err(format!("selection window is not topmost on monitor {index}"));
            }
            if (outer.left, outer.top, outer.right - outer.left, outer.bottom - outer.top)
                != (rect.x, rect.y, rect.width as i32, rect.height as i32)
            {
                return Err(format!("selection window rectangle mismatch on monitor {index}: {outer:?}, expected {rect:?}"));
            }
            if (client.right - client.left, client.bottom - client.top) != (rect.width as i32, rect.height as i32) {
                return Err(format!("selection frame has nonclient insets on monitor {index}: {client:?}, expected {rect:?}"));
            }
            let viewport = window.viewport_size();
            let scale = window.scale_factor();
            if (f32::from(viewport.width) * scale - rect.width as f32).abs() > 2.
                || (f32::from(viewport.height) * scale - rect.height as f32).abs() > 2.
            {
                return Err(format!("GPUI viewport does not track physical monitor rectangle {index}: {viewport:?} × {scale}, expected {rect:?}"));
            }
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                return Err("configuring/drawing made hidden capture fixture visible".into());
            }
            reports.push(format!("monitor {index}: {rect:?}, logical {viewport:?}, scale {scale}"));
            window.remove_window();
            Ok(())
        })
        .map_err(|error| format!("update hidden selection fixture: {error:#}"))??;
    }
    for kind in [CaptureKind::Screenshot, CaptureKind::Gif] {
        for width in [430., 640., 960.] {
            let (handle, observations) = cx
                .update(|cx| open_fixture(cx, kind, size(px(width), px(360.)), true))
                .map_err(|error| error.to_string()) ?;
            cx.update_window(handle.into(), |_, window, _| -> Result<(), String> {
                let hwnd = raw_hwnd(window)?;
                configure_overlay(window, None);
                if topmost_supported
                    && unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 == 0
                {
                    return Err(
                        "pinned/tool window is not topmost when no screen rectangle is supplied"
                            .into(),
                    );
                }
                // Hidden GPUI windows defer their initial placement until first show.
                // Set explicit physical bounds, while preserving the hidden state.
                let scale = window.scale_factor();
                configure_overlay(
                    window,
                    Some(backend::ScreenRect {
                        x: 40,
                        y: 40,
                        width: (width * scale).round() as u32,
                        height: (360. * scale).round() as u32,
                    }),
                );
                Ok(())
            })
            .map_err(|error| error.to_string())??;
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let viewport = cx.update_window(handle.into(), |_, window, cx| -> Result<Size<Pixels>, String> {
                let hwnd = raw_hwnd(window)?;
                window.draw(cx).clear(cx);
                let topmost = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
                if topmost_supported && !topmost {
                    return Err("pinned/tool window is not topmost when no screen rectangle is supplied".into());
                }
                if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    return Err("toolbar fixture unexpectedly became visible".into());
                }
                let viewport = window.viewport_size();
                window.remove_window();
                Ok(viewport)
            }).map_err(|error| format!("draw hidden toolbar fixture: {error:#}"))??;
            let bounds = observations.borrow();
            if bounds.len() != 1 {
                return Err(format!(
                    "missing native toolbar bounds for {kind:?} at {width}px: {bounds:?}"
                ));
            }
            let bounds = bounds[0];
            if bounds.left() < px(0.)
                || bounds.top() < px(0.)
                || bounds.right() > viewport.width + px(1.)
                || bounds.bottom() > viewport.height + px(1.)
            {
                return Err(format!(
                    "native-font toolbar clips outside {kind:?} viewport at {width}px: {bounds:?}, viewport {viewport:?}"
                ));
            }
            reports.push(format!("{kind:?} toolbar at {width}px: {bounds:?}"));
        }
    }
    Ok(reports)
}

#[::core::prelude::v1::test]
#[ignore = "opt-in native capture geometry QA; only hidden, unfocused windows; run by itself"]
fn native_windows_capture_geometry_and_toolbars_stay_in_bounds() {
    let result = Rc::new(RefCell::new(None));
    let captured = result.clone();
    gpui_kit::platform::application()
        // Cases close their fixtures between awaits; keep the test application
        // alive until the suite explicitly quits instead of exiting after case 1.
        .with_quit_mode(QuitMode::Explicit)
        .with_assets(appearance::Assets)
        .run(move |cx| {
            ui::init(cx);
            cx.spawn(async move |cx| {
                let outcome = std::panic::AssertUnwindSafe(native_cases(cx))
                    .catch_unwind()
                    .await;
                *captured.borrow_mut() = Some(match outcome {
                    Ok(result) => result,
                    Err(panic) => Err(panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| {
                            panic
                                .downcast_ref::<&str>()
                                .map(|message| (*message).to_owned())
                        })
                        .unwrap_or_else(|| "panic in hidden capture QA".into())),
                });
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let reports = result
        .borrow_mut()
        .take()
        .expect("native application returned no capture result")
        .expect("native capture geometry QA failed");
    assert!(reports.len() >= 7);
    for report in reports {
        eprintln!("native capture: {report}");
    }
}
