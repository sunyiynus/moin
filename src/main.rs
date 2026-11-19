#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod plugins;

use anyhow::Result;
use gpui::{
    App, AppContext, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, px, size,
};
use moin::config;

fn main() {
    install_panic_hook();
    if let Err(error) = run() {
        report_error(&error);
    }
}

/// Windows GUI binaries have no console, so panics would otherwise abort silently.
/// Keep an append-only report next to the app data for the next reproduction.
fn install_panic_hook() {
    let log_path = config::data_dir()
        .map(|dir| dir.join("panic.log"))
        .unwrap_or_else(|_| std::env::temp_dir().join("moin-panic.log"));
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write as _;

        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "==== panic ====\nthread: {:?}\nmessage: {info}\nlocation: {:?}\nbacktrace:\n{backtrace}\n",
            std::thread::current().name(),
            info.location(),
        );
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Repeated crashes must not grow one file without bound.
        const LIMIT: u64 = 1 << 20;
        if std::fs::metadata(&log_path).is_ok_and(|meta| meta.len() > LIMIT) {
            let _ = std::fs::rename(&log_path, log_path.with_extension("old.log"));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(file, "{report}");
        }
        default_hook(info);
    }));
}

fn report_error(error: &anyhow::Error) {
    eprintln!("{error:#}");
    #[cfg(windows)]
    {
        use windows::{Win32::UI::WindowsAndMessaging::*, core::HSTRING};
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(format!("{error:#}")),
                &HSTRING::from("Moin"),
                MB_ICONERROR,
            );
        }
    }
}

fn run() -> Result<()> {
    let home = config::data_dir()?;
    let config = config::Config::load(&home)?;
    #[cfg(windows)]
    let (_tray, events) = match app::platform::tray::start(&config.activation_hotkey)? {
        app::platform::tray::TrayStart::Ready(tray, events) => (tray, events),
        app::platform::tray::TrayStart::AlreadyRunning => return Ok(()),
    };

    #[cfg(windows)]
    let tray_controller = _tray.controller();
    let plugins = plugins::Plugins::load(&home)?;
    let application = gpui_kit::platform::application().with_assets(app::appearance::Assets);
    #[cfg(target_os = "macos")]
    let reopened = {
        let window = std::rc::Rc::new(std::cell::Cell::new(None::<app::ui::UiWindow<app::Moin>>));
        let handle = window.clone();
        application.on_reopen(move |cx| {
            if let Some(handle) = handle.get() {
                let _ = handle.update(cx, |_, window, _| app::launcher_window::show(window));
            }
        });
        window
    };
    application.run(move |cx: &mut App| {
        app::ui::init(cx);
        // AppKit's menu bar and global shortcut APIs must be created on the main
        // thread after applicationDidFinishLaunching, which invokes this callback.
        #[cfg(target_os = "macos")]
        let (_tray, events) = match app::platform::tray::start(&config.activation_hotkey, &home) {
            Ok(app::platform::tray::TrayStart::Ready(tray, events)) => (tray, events),
            Ok(app::platform::tray::TrayStart::AlreadyRunning) => {
                cx.quit();
                return;
            }
            Err(error) => {
                report_error(&error);
                cx.quit();
                return;
            }
        };
        #[cfg(target_os = "macos")]
        let tray_controller = _tray.controller();
        app::input::bind_keys(cx);
        app::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(560.)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                kind: WindowKind::PopUp,
                titlebar: None,
                window_background: WindowBackgroundAppearance::Blurred,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            move |window, cx| {
                #[cfg(any(windows, target_os = "macos"))]
                {
                    app::launcher_window::configure(window);
                    window.on_window_should_close(cx, |window, _| {
                        app::launcher_window::hide(window);
                        false
                    });
                }
                let view = cx.new(|cx| {
                    app::Moin::new(
                        home,
                        config,
                        plugins,
                        #[cfg(any(windows, target_os = "macos"))]
                        tray_controller,
                        window,
                        cx,
                    )
                });
                app::ui::root(view, window, cx)
            },
        );
        match window {
            Ok(window) => {
                let window = app::ui::UiWindow::<app::Moin>::from_handle(window);
                #[cfg(target_os = "macos")]
                reopened.set(Some(window));
                cx.activate(true);
                #[cfg(any(windows, target_os = "macos"))]
                app::listen_to_tray(window, events, cx);
            }
            Err(error) => {
                report_error(&error);
                cx.quit();
            }
        }
    });
    Ok(())
}
