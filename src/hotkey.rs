use anyhow::{Result, bail, ensure};

/// A global shortcut, represented independently of the desktop UI.
///
/// Key values use stable logical (Windows virtual-key) codes on every platform.
/// Native backends translate these codes when registering the shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub modifiers: u32,
    pub key: u32,
}

pub const ALT: u32 = 1;
pub const CONTROL: u32 = 2;
pub const SHIFT: u32 = 4;
pub const WIN: u32 = 8;
/// The platform modifier: Command on macOS, Windows/Super on Windows.
pub const PLATFORM: u32 = WIN;

pub fn default_shortcut() -> String {
    "Alt+Space".into()
}

impl Shortcut {
    pub fn from_key(key: &str, mut modifiers: u32) -> Result<Self> {
        // GPUI converts shifted digit and punctuation keys to their printable character.
        let shifted = "!@#$%^&*()_+{}|:\"<>?~";
        let unshifted = "1234567890-=[]\\;',./`";
        let normalized = if key.chars().count() == 1 {
            key.chars()
                .next()
                .and_then(|ch| shifted.chars().position(|item| item == ch))
                .and_then(|position| unshifted.chars().nth(position))
                .map(|ch| {
                    modifiers |= SHIFT;
                    ch.to_string()
                })
                .unwrap_or_else(|| key.to_owned())
        } else {
            key.to_owned()
        };
        let shortcut = Self {
            modifiers,
            key: key_code(&normalized.to_ascii_lowercase())?,
        };
        shortcut.validate()?;
        Ok(shortcut)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut modifiers = 0;
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            ensure!(!part.is_empty(), "快捷键格式无效");
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "ctrl" | "control" => Some(CONTROL),
                "alt" | "option" | "opt" | "⌥" => Some(ALT),
                "shift" | "⇧" => Some(SHIFT),
                "win" | "windows" | "super" | "platform" | "cmd" | "command" | "meta" | "⌘" => {
                    Some(PLATFORM)
                }
                _ => None,
            };
            if let Some(modifier) = modifier {
                ensure!(modifiers & modifier == 0, "修饰键重复");
                modifiers |= modifier;
                continue;
            }
            ensure!(key.is_none(), "请使用一个主键搭配修饰键");
            key = Some(key_code(&lower)?);
        }
        let key = key.ok_or_else(|| anyhow::anyhow!("请再按一个字母、数字或功能键"))?;
        let shortcut = Self { modifiers, key };
        shortcut.validate()?;
        Ok(shortcut)
    }

    fn validate(self) -> Result<()> {
        #[cfg(windows)]
        ensure!(self.key != 0x7b, "Windows 保留了 F12，请选择其他按键");
        ensure!(
            self.modifiers & !(ALT | CONTROL | SHIFT | PLATFORM) == 0,
            "快捷键包含无效修饰键"
        );
        ensure!(
            self.modifiers != 0 || (0x70..=0x87).contains(&self.key),
            "请搭配 Ctrl、Alt/Option、Shift 或 Win/Cmd，或使用功能键"
        );
        Ok(())
    }

    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (bit, label) in [
            (CONTROL, "Ctrl"),
            (
                ALT,
                if cfg!(target_os = "macos") {
                    "Option"
                } else {
                    "Alt"
                },
            ),
            (SHIFT, "Shift"),
            (
                PLATFORM,
                if cfg!(target_os = "macos") {
                    "Cmd"
                } else {
                    "Win"
                },
            ),
        ] {
            if self.modifiers & bit != 0 {
                parts.push(label.to_owned());
            }
        }
        parts.push(key_label(self.key));
        parts.join("+")
    }
}

fn key_code(key: &str) -> Result<u32> {
    if key.len() == 1 {
        let byte = key.as_bytes()[0];
        if byte.is_ascii_alphanumeric() {
            return Ok(u32::from(byte.to_ascii_uppercase()));
        }
    }
    if let Some(number) = key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u32>().ok())
        && (1..=24).contains(&number)
    {
        return Ok(0x6f + number);
    }
    let code = match key {
        "space" | "空格" => 0x20,
        "enter" | "return" => 0x0d,
        "tab" => 0x09,
        "escape" | "esc" => 0x1b,
        "backspace" => 0x08,
        "delete" => 0x2e,
        "insert" => 0x2d,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "page_up" | "page-up" => 0x21,
        "pagedown" | "page_down" | "page-down" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "," | "comma" => 0xbc,
        "." | "period" => 0xbe,
        "/" | "slash" => 0xbf,
        ";" | "semicolon" => 0xba,
        "-" | "minus" => 0xbd,
        "=" | "equal" => 0xbb,
        "[" | "bracketleft" => 0xdb,
        "]" | "bracketright" => 0xdd,
        "\\" | "backslash" => 0xdc,
        "'" | "apostrophe" => 0xde,
        "`" | "grave" => 0xc0,
        _ => bail!("暂不支持这个按键，请选择字母、数字、空格或功能键"),
    };
    Ok(code)
}

fn key_label(key: u32) -> String {
    if (0x30..=0x39).contains(&key) || (0x41..=0x5a).contains(&key) {
        return char::from_u32(key).unwrap_or_default().to_string();
    }
    if (0x70..=0x87).contains(&key) {
        return format!("F{}", key - 0x6f);
    }
    match key {
        0x20 => "Space",
        0x0d => "Enter",
        0x09 => "Tab",
        0x1b => "Escape",
        0x08 => "Backspace",
        0x2e => "Delete",
        0x2d => "Insert",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0xbc => ",",
        0xbe => ".",
        0xbf => "/",
        0xba => ";",
        0xbd => "-",
        0xbb => "=",
        0xdb => "[",
        0xdd => "]",
        0xdc => "\\",
        0xde => "'",
        0xc0 => "`",
        _ => "Unknown",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_shortcuts_and_rejects_invalid_combinations() {
        assert_eq!(
            Shortcut::parse("shift + CONTROL + k").unwrap().label(),
            "Ctrl+Shift+K"
        );
        let option = if cfg!(target_os = "macos") {
            "Option"
        } else {
            "Alt"
        };
        assert_eq!(
            Shortcut::parse("alt+空格").unwrap().label(),
            format!("{option}+Space")
        );
        assert_eq!(Shortcut::parse("F6").unwrap().label(), "F6");
        assert_eq!(
            Shortcut::from_key("!", CONTROL).unwrap().label(),
            "Ctrl+Shift+1"
        );
        assert_eq!(
            Shortcut::from_key("+", ALT).unwrap().label(),
            format!("{option}+Shift+=")
        );
        for key in ["Ctrl", "a", "Ctrl++a", "Alt+Alt+A", "Alt+A+B"] {
            assert!(Shortcut::parse(key).is_err(), "{key}");
        }
    }

    #[test]
    fn understands_command_and_option_aliases_and_round_trips_labels() {
        let expected = Shortcut {
            modifiers: CONTROL | ALT | PLATFORM,
            key: u32::from(b'K'),
        };
        for text in [
            "Control+Option+Command+K",
            "Ctrl+Opt+Cmd+K",
            "Ctrl+Alt+Meta+K",
            "Ctrl+⌥+⌘+K",
        ] {
            assert_eq!(Shortcut::parse(text).unwrap(), expected);
        }
        assert_eq!(Shortcut::parse(&expected.label()).unwrap(), expected);
        assert_eq!(
            Shortcut::from_key("k", CONTROL | ALT | PLATFORM).unwrap(),
            expected
        );
        assert!(Shortcut::parse("Cmd+Win+K").is_err());
        assert!(Shortcut::parse("Option+Alt+K").is_err());
        assert!(Shortcut::from_key("k", 16).is_err());
    }

    #[test]
    fn f12_reservation_only_applies_on_windows() {
        assert_eq!(Shortcut::parse("Ctrl+F12").is_err(), cfg!(windows));
    }
}
