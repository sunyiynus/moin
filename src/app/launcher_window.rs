use gpui::Window;

#[cfg(any(windows, target_os = "macos"))]
use gpui::WindowBackgroundAppearance;
#[cfg(any(windows, target_os = "macos"))]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(windows)]
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
        DwmSetWindowAttribute,
    },
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, IsWindowVisible, SW_HIDE, SW_SHOW,
            SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongW,
            SetWindowPos, ShowWindowAsync, WM_NCCALCSIZE, WM_NCDESTROY, WS_CAPTION,
            WS_EX_NOREDIRECTIONBITMAP, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
            WS_THICKFRAME,
        },
    },
};

#[cfg(windows)]
const POPUP_FRAME_SUBCLASS: usize = 0x4d4f494e;

#[cfg(windows)]
unsafe extern "system" fn popup_frame(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _: usize,
) -> LRESULT {
    // GPUI reserves system resize-frame insets even for a non-resizable popup.
    // The launcher draws its own single border over the full window client area.
    if message == WM_NCCALCSIZE {
        return LRESULT(0);
    }
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(popup_frame), subclass);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

#[cfg(windows)]
fn hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?.as_raw();
    match handle {
        RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut _)),
        _ => None,
    }
}

#[cfg(windows)]
pub fn configure(window: &mut Window) {
    window.set_window_title("Moin");
    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    if let Some(hwnd) = hwnd(window) {
        let preference = DWMWCP_ROUND;
        unsafe {
            if !SetWindowSubclass(hwnd, Some(popup_frame), POPUP_FRAME_SUBCLASS, 0).as_bool() {
                eprintln!("无法对齐浮窗边框 · {}", windows::core::Error::from_win32());
            }
            // Windows can add caption bits to GPUI's zero-style overlapped tool window.
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let chrome = WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
            SetWindowLongW(hwnd, GWL_STYLE, ((style & !chrome.0) | WS_POPUP.0) as i32);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const preference).cast(),
                std::mem::size_of_val(&preference) as u32,
            );
            let border = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&raw const border).cast(),
                std::mem::size_of_val(&border) as u32,
            );
        }

        // The DirectComposition surface retains the alpha of the launcher tint. Ask GPUI for
        // blur-behind on the final popup style so acrylic samples the desktop below the window.
        let extended_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        if extended_style & WS_EX_NOREDIRECTIONBITMAP.0 != 0 {
            window.set_background_appearance(WindowBackgroundAppearance::Blurred);
        }
    }
}

#[cfg(windows)]
pub fn is_visible(window: &Window) -> bool {
    hwnd(window).is_some_and(|handle| unsafe { IsWindowVisible(handle).as_bool() })
}

#[cfg(windows)]
pub fn show(window: &Window) {
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_SHOW);
        }
    }
    window.activate_window();
}

#[cfg(target_os = "macos")]
fn ns_window(window: &Window) -> Option<cocoa::base::id> {
    use objc::{msg_send, sel, sel_impl};
    let handle = HasWindowHandle::window_handle(window).ok()?.as_raw();
    match handle {
        RawWindowHandle::AppKit(appkit) => unsafe {
            let view = appkit.ns_view.as_ptr().cast::<objc::runtime::Object>();
            let native: cocoa::base::id = msg_send![view, window];
            (!native.is_null()).then_some(native)
        },
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub fn configure(window: &mut Window) {
    use cocoa::base::{YES, id};
    use objc::{msg_send, sel, sel_impl};

    window.set_window_title("Moin");
    if let Some(native) = ns_window(window) {
        unsafe {
            // Keep GPUI's non-activating NSPanel, removing its native titlebar.
            // GPUIPanel implements canBecomeKeyWindow for keyboard/IME input.
            let _: () = msg_send![native, setStyleMask: 1usize << 7];
            let _: () = msg_send![native, setFloatingPanel: YES];
            let _: () = msg_send![native, setWorksWhenModal: YES];
            let _: () = msg_send![native, setExcludedFromWindowsMenu: YES];
            let _: () = msg_send![native, setHasShadow: YES];
            let content: id = msg_send![native, contentView];
            let _: () = msg_send![content, setWantsLayer: YES];
            let layer: id = msg_send![content, layer];
            let _: () = msg_send![layer, setCornerRadius: 8.0f64];
            let _: () = msg_send![layer, setMasksToBounds: YES];
        }
    }
    // GPUI uses NSVisualEffectView for the macOS blurred backdrop.
    window.set_background_appearance(WindowBackgroundAppearance::Blurred);
}

#[cfg(target_os = "macos")]
pub fn is_visible(window: &Window) -> bool {
    use cocoa::base::{BOOL, YES};
    use objc::{msg_send, sel, sel_impl};
    ns_window(window).is_some_and(|native| unsafe {
        let visible: BOOL = msg_send![native, isVisible];
        visible == YES
    })
}

#[cfg(target_os = "macos")]
pub fn show(window: &Window) {
    use cocoa::{
        appkit::NSApp,
        base::{YES, nil},
    };
    use objc::{msg_send, sel, sel_impl};
    if let Some(native) = ns_window(window) {
        unsafe {
            let _: () = msg_send![NSApp(), activateIgnoringOtherApps: YES];
            let _: () = msg_send![native, makeKeyAndOrderFront: nil];
        }
    }
}

pub fn hide(window: &Window) {
    #[cfg(windows)]
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_HIDE);
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(native) = ns_window(window) {
        use cocoa::base::nil;
        use objc::{msg_send, sel, sel_impl};
        unsafe {
            let _: () = msg_send![native, orderOut: nil];
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    window.minimize_window();
}
