//! Shared UI assets. macOS must not depend on Windows-only symbol fonts.

#[cfg(not(windows))]
use gpui::svg;
use gpui::{AssetSource, Div, Rems, SharedString, div, prelude::*, px, rems, rgb};
use std::borrow::Cow;

pub fn font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        ".AppleSystemUIFont"
    } else {
        "Segoe UI"
    }
}

pub fn mono_font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "Consolas"
    }
}

/// Resolve a user-configured font family, falling back to the platform UI font.
pub fn resolve_family(configured: &str) -> SharedString {
    let family = configured.trim();
    if family.is_empty() {
        SharedString::from(font_family())
    } else {
        SharedString::from(family.to_owned())
    }
}

pub fn resolve_ai_family(appearance: &crate::config::AppearanceConfig) -> SharedString {
    if appearance.ai_font_family.trim().is_empty() {
        resolve_family(&appearance.ui_font_family)
    } else {
        SharedString::from(appearance.ai_font_family.trim().to_owned())
    }
}

/// A text size expressed in rems, relative to the configured 16 px UI baseline.
pub fn rem(size: f32) -> Rems {
    rems(size / 16.0)
}

pub fn shortcut_label(value: &str) -> String {
    if cfg!(target_os = "macos") {
        value.replace("Ctrl", "⌘").replace("Alt", "⌥")
    } else {
        value.to_owned()
    }
}

pub fn icon(symbol: &'static str, size: f32, color: u32) -> Div {
    #[cfg(windows)]
    {
        div()
            .flex_shrink_0()
            .font_family("Segoe MDL2 Assets")
            .text_size(px(size))
            .text_color(rgb(color))
            .child(symbol)
    }
    #[cfg(not(windows))]
    {
        div().flex_shrink_0().flex().items_center().child(
            svg()
                .path(icon_path(symbol))
                .size(px(size))
                .text_color(rgb(color)),
        )
    }
}

#[cfg(not(windows))]
fn icon_path(symbol: &str) -> &'static str {
    match symbol {
        "\u{e8f2}" => "icons/chat",
        "\u{e721}" => "icons/search",
        "\u{e713}" => "icons/settings",
        "\u{e8d2}" => "icons/type",
        "\u{e8b7}" | "\u{e838}" => "icons/folder",
        "\u{e8a5}" => "icons/document",
        "\u{e8c8}" => "icons/copy",
        "\u{e768}" | "\u{e8a7}" => "icons/open",
        "\u{e8ef}" => "icons/calculator",
        "\u{e8c1}" => "icons/translate",
        "\u{e82d}" => "icons/book",
        "\u{ed5e}" => "icons/convert",
        "\u{e74c}" | "\u{ea86}" => "icons/plugin",
        _ => "icons/app",
    }
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let paths = match path {
            "icons/chat" => "<path d='M4 3h16v13H9l-5 5z'/><path d='M8 7h8M8 11h6'/>",
            "icons/search" => "<circle cx='11' cy='11' r='7'/><path d='m16 16 5 5'/>",
            "icons/settings" => {
                "<circle cx='12' cy='12' r='3'/><path d='m10 3 4 0 .5 3 2 1.2 3-.8 2 3.5-2.5 2 0 2.2 2.5 2-2 3.5-3-.8-2 1.2-.5 3h-4l-.5-3-2-1.2-3 .8-2-3.5 2.5-2v-2.2l-2.5-2 2-3.5 3 .8 2-1.2z'/>"
            }
            "icons/type" => "<path d='M5 6V4h14v2M12 4v16M8 20h8'/>",
            "icons/folder" => "<path d='M3 7V5h7l2 2h9v13H3z'/>",
            "icons/document" => "<path d='M5 3h9l5 5v13H5zM14 3v5h5M8 12h8M8 16h8'/>",
            "icons/copy" => {
                "<rect x='8' y='8' width='12' height='13' rx='1'/><path d='M16 8V3H3v13h5'/>"
            }
            "icons/open" => "<path d='M14 4h6v6M20 4 10 14M10 4H4v16h16v-6'/>",
            "icons/calculator" => {
                "<rect x='5' y='2' width='14' height='20' rx='2'/><path d='M8 6h8M8 11h1m3 0h1m3 0h.1M8 15h1m3 0h1m3 0h.1M8 19h1m3 0h1m3 0h.1'/>"
            }
            "icons/translate" => {
                "<path d='M3 5h12M9 2v3M12 5c0 6-4 10-9 11M5 8c1 3 4 6 7 8m2 4 4-11 4 11m-7-3h6'/>"
            }
            "icons/book" => {
                "<path d='M12 5C9 3 5 3 2 4v15c4-1 7-1 10 1 3-2 6-2 10-1V4c-3-1-7-1-10 1zm0 0v15'/>"
            }
            "icons/convert" => "<path d='M3 7h17m-4-4 4 4-4 4M21 17H4m4-4-4 4 4 4'/>",
            "icons/plugin" => "<path d='M8 3v5H3v8h5v5h8v-5h5V8h-5V3z'/>",
            "icons/app" => {
                "<rect x='3' y='3' width='7' height='7' rx='1'/><rect x='14' y='3' width='7' height='7' rx='1'/><rect x='3' y='14' width='7' height='7' rx='1'/><rect x='14' y='14' width='7' height='7' rx='1'/>"
            }
            _ => return Ok(None),
        };
        Ok(Some(Cow::Owned(format!(
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'>{paths}</svg>"
        ).into_bytes())))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
