//! Persistent user configuration (`~/.config/synapse-linux/config.toml`).
//!
//! Device settings themselves live in the headset; this file only keeps
//! application preferences and the user's saved EQ curves.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::eq::{self, Bands};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// Follow the system locale.
    #[default]
    Auto,
    Tr,
    En,
}

impl Language {
    /// Resolve `Auto` from the environment (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`).
    pub fn resolve(self) -> Language {
        match self {
            Language::Auto => {
                let vars = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"];
                let locale = vars
                    .iter()
                    .filter_map(|v| std::env::var(v).ok())
                    .find(|v| !v.is_empty())
                    .unwrap_or_default();
                if locale.to_ascii_lowercase().starts_with("tr") {
                    Language::Tr
                } else {
                    Language::En
                }
            }
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub enabled: bool,
    /// Warn once when the battery drops to this level (percent, 0 = never).
    pub low_battery_percent: u8,
    pub full_charge: bool,
    /// Notify when the headset connects / disconnects.
    pub connection: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            low_battery_percent: 20,
            full_charge: true,
            connection: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreset {
    pub name: String,
    pub bands: Bands,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Language,
    /// Seconds between status polls in the tray.
    pub tray_poll_seconds: u64,
    /// Last curve written to the custom slot (restored in the editor).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_custom_curve: Option<Bands>,
    pub notifications: Notifications,
    pub user_presets: Vec<UserPreset>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Language::Auto,
            tray_poll_seconds: 30,
            last_custom_curve: None,
            notifications: Notifications::default(),
            user_presets: Vec::new(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("synapse-linux").join("config.toml"))
    }

    /// Load from the default location; missing or broken files give defaults.
    pub fn load() -> Config {
        match Self::default_path() {
            Some(path) => Self::load_from(&path),
            None => Config::default(),
        }
    }

    pub fn load_from(path: &Path) -> Config {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Config::default(),
            Err(err) => {
                log::warn!("cannot read {}: {err}", path.display());
                return Config::default();
            }
        };
        match toml::from_str::<Config>(&text) {
            Ok(mut config) => {
                config.sanitize();
                config
            }
            Err(err) => {
                let backup = path.with_extension("toml.broken");
                log::warn!(
                    "{} is invalid ({err}); moving it to {}",
                    path.display(),
                    backup.display()
                );
                let _ = fs::rename(path, &backup);
                Config::default()
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path =
            Self::default_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
        self.save_to(&path)
    }

    /// Write atomically (temp file + rename).
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    fn sanitize(&mut self) {
        self.tray_poll_seconds = self.tray_poll_seconds.clamp(5, 3600);
        self.notifications.low_battery_percent = self.notifications.low_battery_percent.min(100);
        self.last_custom_curve = self.last_custom_curve.map(eq::clamp_bands);
        for preset in &mut self.user_presets {
            preset.bands = eq::clamp_bands(preset.bands);
            preset.name = preset.name.trim().to_string();
        }
        self.user_presets.retain(|p| !p.name.is_empty());
    }

    pub fn find_preset(&self, name: &str) -> Option<&UserPreset> {
        self.user_presets
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Add or replace a saved curve. Returns false for an empty name.
    pub fn upsert_preset(&mut self, name: &str, bands: Bands) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let bands = eq::clamp_bands(bands);
        match self.user_presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(name)) {
            Some(existing) => existing.bands = bands,
            None => self.user_presets.push(UserPreset {
                name: name.to_string(),
                bands,
            }),
        }
        true
    }

    pub fn remove_preset(&mut self, name: &str) -> bool {
        let before = self.user_presets.len();
        self.user_presets.retain(|p| !p.name.eq_ignore_ascii_case(name.trim()));
        before != self.user_presets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("synapse-config-{tag}-{}", std::process::id()))
            .join("config.toml")
    }

    #[test]
    fn roundtrip_through_toml() {
        let path = temp_path("roundtrip");
        let mut config = Config {
            language: Language::Tr,
            last_custom_curve: Some([1; 10]),
            ..Config::default()
        };
        assert!(config.upsert_preset("Gece", [-1, 0, 1, 2, 3, 4, 5, 6, 6, 6]));
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn missing_file_gives_defaults_and_partial_file_fills_in() {
        assert_eq!(
            Config::load_from(Path::new("/nonexistent/synapse/config.toml")),
            Config::default()
        );
        let path = temp_path("partial");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "language = \"en\"\n[notifications]\nlow_battery_percent = 150\n").unwrap();
        let loaded = Config::load_from(&path);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert_eq!(loaded.language, Language::En);
        assert_eq!(loaded.notifications.low_battery_percent, 100);
        assert!(loaded.notifications.enabled);
        assert_eq!(loaded.tray_poll_seconds, 30);
    }

    #[test]
    fn broken_file_is_moved_aside() {
        let path = temp_path("broken");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
        assert!(!path.exists());
        assert!(path.with_extension("toml.broken").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn preset_management() {
        let mut config = Config::default();
        assert!(!config.upsert_preset("   ", [0; 10]));
        assert!(config.upsert_preset("Bass", [9; 10]));
        assert_eq!(config.find_preset("bass").unwrap().bands, [6; 10]); // clamped
        assert!(config.upsert_preset("BASS", [1; 10]));
        assert_eq!(config.user_presets.len(), 1);
        assert!(config.remove_preset("Bass"));
        assert!(!config.remove_preset("Bass"));
    }

    #[test]
    fn language_resolution() {
        assert_eq!(Language::Tr.resolve(), Language::Tr);
        assert_eq!(Language::En.resolve(), Language::En);
        assert!(matches!(Language::Auto.resolve(), Language::Tr | Language::En));
    }
}
