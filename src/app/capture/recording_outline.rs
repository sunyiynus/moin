//! Non-activating, mouse-transparent recording boundaries. These native strips
//! have no controls or content surface over the captured area's interior.
use anyhow::{Result, ensure};
use moin::capture::ScreenRect;
use std::{marker::PhantomData, rc::Rc};

pub(super) struct RecordingOutline {
    windows: Vec<native::BorderWindow>,
    // All window creation, z-order changes and destruction stay on the UI thread.
    _ui_thread: PhantomData<Rc<()>>,
}

impl RecordingOutline {
    pub(super) fn open(region: ScreenRect, screen: ScreenRect, thickness: u32) -> Result<Self> {
        Self::create(region, screen, thickness, true)
    }

    fn create(region: ScreenRect, screen: ScreenRect, thickness: u32, show: bool) -> Result<Self> {
        let mut windows = Vec::with_capacity(4);
        for rect in outline_rects(region, screen, thickness)? {
            windows.push(native::BorderWindow::open(rect, show)?);
        }
        Ok(Self {
            windows,
            _ui_thread: PhantomData,
        })
    }

    pub(super) fn raise(&self) {
        for window in &self.windows {
            window.raise();
        }
    }
}

fn outline_rects(
    region: ScreenRect,
    screen: ScreenRect,
    thickness: u32,
) -> Result<[ScreenRect; 4]> {
    region.validate()?;
    screen.validate()?;
    let right = region.x as i64 + region.width as i64;
    let bottom = region.y as i64 + region.height as i64;
    let screen_right = screen.x as i64 + screen.width as i64;
    let screen_bottom = screen.y as i64 + screen.height as i64;
    ensure!(
        region.x >= screen.x
            && region.y >= screen.y
            && right <= screen_right
            && bottom <= screen_bottom,
        "显示器布局已改变，请重新选择录制区域"
    );
    let t = thickness.max(1).min(region.width).min(region.height) as i64;
    // Prefer the outside of the selection; move inward only at display edges.
    // Every strip is excluded from capture, including full-screen selections.
    let left = (region.x as i64 - t).max(screen.x as i64);
    let top = (region.y as i64 - t).max(screen.y as i64);
    let right = right.min(screen_right - t);
    let bottom = bottom.min(screen_bottom - t);
    Ok([
        ScreenRect {
            x: left as i32,
            y: top as i32,
            width: (right + t - left) as u32,
            height: t as u32,
        },
        ScreenRect {
            x: left as i32,
            y: bottom as i32,
            width: (right + t - left) as u32,
            height: t as u32,
        },
        ScreenRect {
            x: left as i32,
            y: top as i32,
            width: t as u32,
            height: (bottom + t - top) as u32,
        },
        ScreenRect {
            x: right as i32,
            y: top as i32,
            width: t as u32,
            height: (bottom + t - top) as u32,
        },
    ])
}

#[cfg(windows)]
mod native {
    use super::*;
    use anyhow::Context as _;
    use std::sync::OnceLock;
    use windows::{
        Win32::{
            Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM},
            Graphics::Gdi::{CreateSolidBrush, DeleteObject},
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::*,
        },
        core::w,
    };

    pub(super) struct BorderWindow(pub(super) HWND);
    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
    impl BorderWindow {
        pub(super) fn open(rect: ScreenRect, show: bool) -> Result<Self> {
            static CLASS: OnceLock<std::result::Result<(), String>> = OnceLock::new();
            let class = CLASS.get_or_init(|| unsafe {
                let brush = CreateSolidBrush(COLORREF(0x005e62ff));
                if brush.is_invalid() {
                    return Err("无法创建录制边框画刷".into());
                }
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: GetModuleHandleW(None).map_err(|e| e.to_string())?.into(),
                    lpszClassName: w!("MoinRecordingOutline"),
                    hbrBackground: brush,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    let error = windows::core::Error::from_win32().to_string();
                    let _ = DeleteObject(brush.into());
                    return Err(error);
                }
                Ok(())
            });
            class
                .as_ref()
                .map_err(|error| anyhow::anyhow!("无法注册录制边框 · {error}"))?;
            unsafe {
                let frame = Self(
                    CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                        w!("MoinRecordingOutline"),
                        w!("Moin GIF recording boundary"),
                        WS_POPUP | WS_DISABLED,
                        rect.x,
                        rect.y,
                        rect.width as i32,
                        rect.height as i32,
                        None,
                        None,
                        Some(GetModuleHandleW(None)?.into()),
                        None,
                    )
                    .context("无法创建录制区域边框")?,
                );
                SetLayeredWindowAttributes(frame.0, COLORREF(0), 255, LWA_ALPHA)?;
                SetWindowDisplayAffinity(frame.0, WDA_EXCLUDEFROMCAPTURE)
                    .context("无法将录制边框从屏幕捕获中排除")?;
                SetWindowPos(
                    frame.0,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE
                        | SWP_NOSIZE
                        | SWP_NOACTIVATE
                        | if show {
                            SWP_SHOWWINDOW
                        } else {
                            SET_WINDOW_POS_FLAGS(0)
                        },
                )?;
                Ok(frame)
            }
        }
        pub(super) fn raise(&self) {
            unsafe {
                let _ = SetWindowPos(
                    self.0,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
    impl Drop for BorderWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use anyhow::Context as _;
    use cocoa::{
        appkit::NSScreen,
        base::{NO, YES, id, nil},
        foundation::{NSArray, NSPoint, NSRect, NSSize},
    };
    use objc::{class, msg_send, sel, sel_impl};

    pub(super) struct BorderWindow(id);
    impl BorderWindow {
        pub(super) fn open(rect: ScreenRect, show: bool) -> Result<Self> {
            unsafe {
                let screens = NSScreen::screens(nil);
                ensure!(NSArray::count(screens) > 0, "没有可用的显示器");
                let primary = NSArray::objectAtIndex(screens, 0);
                let primary_frame: NSRect = msg_send![primary, frame];
                let bounds = NSRect::new(
                    NSPoint::new(
                        rect.x as f64,
                        primary_frame.size.height - rect.y as f64 - rect.height as f64,
                    ),
                    NSSize::new(rect.width as f64, rect.height as f64),
                );
                let native: id = msg_send![class!(NSWindow), alloc];
                let native: id = msg_send![native,initWithContentRect:bounds styleMask:0usize backing:2usize defer:NO];
                let native = (!native.is_null())
                    .then_some(native)
                    .context("无法创建录制区域边框")?;
                let frame = Self(native);
                let _: () = msg_send![native,setReleasedWhenClosed:NO];
                let _: () = msg_send![native,setOpaque:YES];
                let color: id = msg_send![class!(NSColor),colorWithSRGBRed:1.0f64 green:(98.0f64/255.0) blue:(94.0f64/255.0) alpha:1.0f64];
                let _: () = msg_send![native,setBackgroundColor:color];
                let _: () = msg_send![native,setIgnoresMouseEvents:YES];
                let _: () = msg_send![native,setHasShadow:NO];
                let _: () = msg_send![native,setSharingType:0usize];
                let _: () = msg_send![native,setLevel:25isize];
                let _: () =
                    msg_send![native,setCollectionBehavior:((1usize<<0)|(1usize<<4)|(1usize<<8))];
                if show {
                    let _: () = msg_send![native, orderFrontRegardless];
                }
                Ok(frame)
            }
        }
        pub(super) fn raise(&self) {
            unsafe {
                let _: () = msg_send![self.0, orderFrontRegardless];
            }
        }
    }
    impl Drop for BorderWindow {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0,orderOut:nil];
                let _: () = msg_send![self.0, close];
                let _: () = msg_send![self.0, release];
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod native {
    use super::*;
    pub(super) struct BorderWindow;
    impl BorderWindow {
        pub(super) fn open(_: ScreenRect, _: bool) -> Result<Self> {
            anyhow::bail!("当前平台暂不支持录制边框")
        }
        pub(super) fn raise(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outline_stays_outside_interior_and_visible_at_display_edges() {
        let screen = ScreenRect {
            x: -1920,
            y: -240,
            width: 1920,
            height: 1080,
        };
        let selection = ScreenRect {
            x: -1800,
            y: -100,
            width: 640,
            height: 480,
        };
        let strips = outline_rects(selection, screen, 5).unwrap();
        for strip in strips {
            assert!(
                strip.x as i64 + strip.width as i64 <= selection.x as i64
                    || strip.x >= selection.x + selection.width as i32
                    || strip.y as i64 + strip.height as i64 <= selection.y as i64
                    || strip.y >= selection.y + selection.height as i32
            );
        }
        let strips = outline_rects(screen, screen, 5).unwrap();
        for strip in strips {
            assert!(
                strip.x >= screen.x
                    && strip.y >= screen.y
                    && strip.x as i64 + strip.width as i64 <= screen.x as i64 + screen.width as i64
                    && strip.y as i64 + strip.height as i64
                        <= screen.y as i64 + screen.height as i64
            );
        }
        let tiny = ScreenRect {
            x: -1920,
            y: -240,
            width: 1,
            height: 1,
        };
        for strip in outline_rects(tiny, screen, 6).unwrap() {
            assert!(strip.width > 0 && strip.height > 0);
        }
        assert!(outline_rects(ScreenRect { x: 0, ..selection }, screen, 3).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn native_outline_is_nonactivating_excluded_and_destroyed_on_drop() {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let screen = ScreenRect {
            x: 0,
            y: 0,
            width: 1000,
            height: 800,
        };
        let selection = ScreenRect {
            x: 20,
            y: 20,
            width: 600,
            height: 400,
        };
        let foreground = unsafe { GetForegroundWindow() };
        let outline = RecordingOutline::create(selection, screen, 4, false).unwrap();
        let handles = outline
            .windows
            .iter()
            .map(|window| window.0)
            .collect::<Vec<_>>();
        let expected = outline_rects(selection, screen, 4).unwrap();
        outline.raise();
        for (handle, rect) in handles.iter().zip(expected) {
            unsafe {
                let styles = GetWindowLongW(*handle, GWL_EXSTYLE) as u32;
                assert_eq!(
                    styles
                        & (WS_EX_TRANSPARENT.0
                            | WS_EX_LAYERED.0
                            | WS_EX_NOACTIVATE.0
                            | WS_EX_TOOLWINDOW.0),
                    WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0
                );
                let mut affinity = 0;
                GetWindowDisplayAffinity(*handle, &mut affinity).unwrap();
                assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0);
                assert!(!IsWindowVisible(*handle).as_bool());
                let mut actual = windows::Win32::Foundation::RECT::default();
                GetWindowRect(*handle, &mut actual).unwrap();
                assert_eq!(
                    (
                        actual.left,
                        actual.top,
                        actual.right - actual.left,
                        actual.bottom - actual.top
                    ),
                    (rect.x, rect.y, rect.width as i32, rect.height as i32)
                );
            }
        }
        drop(outline);
        for handle in handles {
            assert!(!unsafe { IsWindow(Some(handle)) }.as_bool());
        }
        assert_eq!(foreground, unsafe { GetForegroundWindow() });
    }
}
