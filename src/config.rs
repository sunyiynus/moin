use crate::ai::AiConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CONFIG_FILE: &str = "settings.json";

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "crate::search::index::default_roots")]
    pub roots: Vec<PathBuf>,
    #[serde(default = "crate::hotkey::default_shortcut")]
    pub activation_hotkey: String,
    #[serde(default)]
    pub feature_shortcuts: crate::hotkey::FeatureShortcuts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_apps: Vec<RecentApp>,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub everything: EverythingConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: crate::search::index::default_roots(),
            activation_hotkey: crate::hotkey::default_shortcut(),
            feature_shortcuts: crate::hotkey::FeatureShortcuts::default(),
            recent_apps: Vec::new(),
            appearance: AppearanceConfig::default(),
            everything: EverythingConfig::default(),
            capture: CaptureConfig::default(),
            ai: AiConfig::default(),
            embedding: None,
        }
    }
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

/// Capture files stay outside the application database and can be used directly.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct CaptureConfig {
    pub output_dir: PathBuf,
    pub gif_fps: u16,
    pub gif_max_seconds: u32,
    pub include_cursor: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        let output_dir = directories::UserDirs::new()
            .map(|dirs| dirs.picture_dir().unwrap_or(dirs.home_dir()).join("Moin"))
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join("Moin")))
            .unwrap_or_else(|| std::env::temp_dir().join("Moin Captures"));
        Self {
            output_dir,
            gif_fps: 10,
            gif_max_seconds: 60,
            include_cursor: true,
        }
    }
}

impl CaptureConfig {
    pub fn normalized(mut self) -> Self {
        if self.output_dir.as_os_str().is_empty() {
            self.output_dir = Self::default().output_dir;
        }
        self.gif_fps = self.gif_fps.clamp(1, 30);
        self.gif_max_seconds = self.gif_max_seconds.clamp(1, 120);
        self
    }

    /// Do not create folders until the first actual capture is saved.
    pub fn validated(self) -> Result<Self> {
        let config = self.normalized();
        anyhow::ensure!(config.output_dir.is_absolute(), "保存目录需要使用完整路径");
        anyhow::ensure!(
            !config.output_dir.is_file(),
            "保存目录指向了一个文件，请选择文件夹"
        );
        Ok(config)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingConfig {
    pub model: String,
    #[serde(default = "default_embedding_endpoint")]
    pub endpoint: String,
    pub dimensions: Option<usize>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            endpoint: default_embedding_endpoint(),
            dimensions: None,
        }
    }
}

fn default_embedding_endpoint() -> String {
    "http://127.0.0.1:11434/api/embed".into()
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
        let path = home.join(CONFIG_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let mut config: Self = serde_json::from_str(text)
                    .with_context(|| format!("无法读取 {}", path.display()))?;
                config.appearance = config.appearance.normalized();
                config.capture = config.capture.normalized();
                Ok(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                // Best effort: create the file so the user has a concrete config to edit.
                let _ = config.save(home);
                Ok(config)
            }
            Err(error) => Err(error).with_context(|| format!("无法读取 {}", path.display())),
        }
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        fs::create_dir_all(home).context("无法创建 Moin 数据目录")?;
        let path = home.join(CONFIG_FILE);
        let temporary = home.join("settings.json.tmp");
        let text = serde_json::to_string_pretty(self).context("无法序列化 Moin 设置")?;
        fs::write(&temporary, text).context("无法写入 Moin 设置")?;
        fs::rename(temporary, path).context("无法保存 Moin 设置")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_settings_through_json() -> Result<()> {
        let folder = tempfile::tempdir()?;
        let mut config = Config::load(folder.path())?;
        assert_eq!(config.activation_hotkey, "Alt+Space");
        assert!(config.feature_shortcuts.iter().next().is_none());
        assert_eq!(config.appearance, AppearanceConfig::default());
        assert!(config.everything.enabled);
        assert_eq!(config.capture, CaptureConfig::default());
        config.activation_hotkey = "Ctrl+Shift+K".into();
        config
            .feature_shortcuts
            .set(crate::hotkey::FeatureAction::Ai, "Ctrl+Alt+A".into());
        config.appearance.ui_font_size = 18.0;
        config.appearance.ai_font_family = "Microsoft YaHei UI".into();
        config.ai.model = "qwen2.5:3b".into();
        config.capture.gif_fps = 15;
        config.capture.gif_max_seconds = 30;
        config.capture.include_cursor = false;
        config.embedding = Some(EmbeddingConfig {
            model: "bge-m3".into(),
            endpoint: default_embedding_endpoint(),
            dimensions: Some(1024),
        });
        config.recent_apps.push(RecentApp {
            title: "Moin".into(),
            path: PathBuf::from("C:\\Tools\\Moin.exe"),
        });
        config.save(folder.path())?;

        let restored = Config::load(folder.path())?;
        assert_eq!(restored.activation_hotkey, "Ctrl+Shift+K");
        assert_eq!(
            restored
                .feature_shortcuts
                .get(crate::hotkey::FeatureAction::Ai),
            "Ctrl+Alt+A"
        );
        assert_eq!(restored.appearance.ui_font_size, 18.0);
        assert_eq!(restored.appearance.ai_font_family, "Microsoft YaHei UI");
        assert_eq!(restored.ai.model, "qwen2.5:3b");
        assert_eq!(restored.capture.gif_fps, 15);
        assert_eq!(restored.capture.gif_max_seconds, 30);
        assert!(!restored.capture.include_cursor);
        assert_eq!(
            restored
                .embedding
                .as_ref()
                .map(|value| value.model.as_str()),
            Some("bge-m3")
        );
        assert!(restored.everything.enabled);
        assert_eq!(restored.recent_apps.len(), 1);
        assert_eq!(restored.recent_apps[0].title, "Moin");
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

    #[test]
    fn capture_settings_preserve_older_configs_and_bound_recording() {
        let legacy: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.capture, CaptureConfig::default());
        let normalized = CaptureConfig {
            output_dir: PathBuf::new(),
            gif_fps: 0,
            gif_max_seconds: u32::MAX,
            include_cursor: false,
        }
        .normalized();
        assert!(normalized.output_dir.is_absolute());
        assert_eq!(normalized.gif_fps, 1);
        assert_eq!(normalized.gif_max_seconds, 120);
        assert!(!normalized.include_cursor);
        assert!(
            CaptureConfig {
                output_dir: PathBuf::from("relative"),
                ..normalized.clone()
            }
            .validated()
            .is_err()
        );
        assert!(normalized.validated().is_ok());
    }
}
