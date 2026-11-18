use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    #[serde(default = "crate::hotkey::default_shortcut")]
    pub activation_hotkey: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_apps: Vec<RecentApp>,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub everything: EverythingConfig,
}

pub const UI_FONT_SIZE_MIN: f32 = 12.0;
pub const UI_FONT_SIZE_MAX: f32 = 22.0;
pub const AI_FONT_SIZE_MIN: f32 = 12.0;
pub const AI_FONT_SIZE_MAX: f32 = 28.0;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Empty means the platform's default UI font.
    pub ui_font_family: String,
    pub ui_font_size: f32,
    /// Empty means it follows the UI font family.
    pub ai_font_family: String,
    pub ai_font_size: f32,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            ui_font_family: String::new(),
            ui_font_size: 16.0,
            ai_font_family: String::new(),
            ai_font_size: 16.0,
        }
    }
}

impl AppearanceConfig {
    pub fn normalized(mut self) -> Self {
        self.ui_font_family = clean_family(self.ui_font_family);
        self.ai_font_family = clean_family(self.ai_font_family);
        self.ui_font_size = self.ui_font_size.clamp(UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX);
        self.ai_font_size = self.ai_font_size.clamp(AI_FONT_SIZE_MIN, AI_FONT_SIZE_MAX);
        self
    }
}

fn clean_family(family: String) -> String {
    let family = family.trim();
    if family.chars().count() > 120 || family.chars().any(char::is_control) {
        String::new()
    } else {
        family.to_owned()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct EverythingConfig {
    pub enabled: bool,
}

impl Default for EverythingConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct RecentApp {
    pub title: String,
    pub path: PathBuf,
}

pub fn data_dir() -> Result<PathBuf> {
    let path = std::env::var_os("MOIN_DATA_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join("Moin")))
        .context("找不到本地应用数据目录")?;
    fs::create_dir_all(&path).context("无法创建 Moin 数据目录")?;
    Ok(path)
}

impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        match fs::read(home.join("config.json")) {
            Ok(bytes) => {
                let mut config: Self =
                    serde_json::from_slice(&bytes).context("无法读取 config.json")?;
                config.appearance = config.appearance.normalized();
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                roots: crate::search::index::default_roots(),
                activation_hotkey: crate::hotkey::default_shortcut(),
                recent_apps: Vec::new(),
                appearance: AppearanceConfig::default(),
                everything: EverythingConfig::default(),
            }),
            Err(error) => Err(error).context("无法读取 config.json"),
        }
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        let path = home.join("config.json");
        let temporary = home.join("config.json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path).context("无法保存 Moin 设置")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_old_settings_and_round_trips_configured_hotkey() -> Result<()> {
        let mut config: Config = serde_json::from_str(r#"{"roots":[]}"#)?;
        assert_eq!(config.activation_hotkey, "Alt+Space");
        assert_eq!(config.appearance, AppearanceConfig::default());
        assert!(config.everything.enabled);
        config.activation_hotkey = "Ctrl+Shift+K".into();
        config.appearance.ui_font_size = 18.0;
        config.appearance.ai_font_family = "Microsoft YaHei UI".into();
        let restored: Config = serde_json::from_str(&serde_json::to_string(&config)?)?;
        assert_eq!(restored.activation_hotkey, "Ctrl+Shift+K");
        assert_eq!(restored.appearance.ui_font_size, 18.0);
        assert_eq!(restored.appearance.ai_font_family, "Microsoft YaHei UI");
        assert!(restored.everything.enabled);
        assert!(restored.recent_apps.is_empty());
        Ok(())
    }

    #[test]
    fn appearance_sizes_are_clamped_and_families_are_cleaned() {
        let appearance = AppearanceConfig {
            ui_font_family: "  ".into(),
            ui_font_size: 99.0,
            ai_font_family: "  PingFang SC ".into(),
            ai_font_size: 1.0,
        }
        .normalized();
        assert!(appearance.ui_font_family.is_empty());
        assert_eq!(appearance.ai_font_family, "PingFang SC");
        assert_eq!(appearance.ui_font_size, UI_FONT_SIZE_MAX);
        assert_eq!(appearance.ai_font_size, AI_FONT_SIZE_MIN);
    }
}
