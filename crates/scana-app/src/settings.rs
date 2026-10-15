//! The user's settings, in `~/.config/scana/config.toml` (or `$XDG_CONFIG_HOME/scana/`).
//! A missing or broken file gives the defaults, with a message, never a crash.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeChoice {
    #[default]
    Dark,
    /// Black and white with strong colours, for the cab in daylight.
    HighContrast,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Text and controls size: 1.0 is normal. Ctrl + and Ctrl - change it too.
    pub zoom: f32,
    /// Gap between requests when Scana is alone on the bus, ms.
    pub gap_alone_ms: u64,
    /// Gap when another tester (such as a dash display) is polling, ms.
    pub gap_shared_ms: u64,
    /// Record every connection to a session folder.
    pub auto_record: bool,
    pub show_learn: bool,
    /// Shows raw bytes and other detail.
    pub advanced: bool,
    pub first_run: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeChoice::Dark,
            zoom: 1.0,
            gap_alone_ms: 50,
            gap_shared_ms: 100,
            auto_record: false,
            show_learn: true,
            advanced: false,
            first_run: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("scana").join("config.toml")
}

impl Settings {
    /// Loads the settings, or the defaults and a message saying what went wrong.
    pub fn load() -> (Settings, Option<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    s.clamp();
                    (s, None)
                }
                Err(e) => (
                    Settings::default(),
                    Some(format!(
                        "Settings in {} couldn't be read ({}); using the defaults.",
                        path.display(),
                        first_line(&e.to_string())
                    )),
                ),
            },
            Err(_) => (Settings::default(), None),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("can't save settings in {}: {e}", path.display()))
    }

    pub fn clamp(&mut self) {
        self.zoom = self.zoom.clamp(0.7, 2.5);
        self.gap_alone_ms = self.gap_alone_ms.clamp(20, 1000);
        self.gap_shared_ms = self.gap_shared_ms.clamp(20, 1000);
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bad_values() {
        let s = Settings { zoom: 1.4, theme: ThemeChoice::HighContrast, ..Settings::default() };
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
        // Missing fields take defaults; silly values are clamped.
        let mut partial: Settings = toml::from_str("zoom = 9.0").unwrap();
        partial.clamp();
        assert_eq!(partial.zoom, 2.5);
        assert_eq!(partial.gap_alone_ms, 50);
    }
}
