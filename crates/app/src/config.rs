//! Persistent settings at `%APPDATA%\CamSwitch\config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SwitchMode {
    /// All selected cameras stream; switching is instant.
    #[default]
    Hot,
    /// Only the active camera streams; switching waits for the new camera.
    Cold,
}

/// Picture shown instead of a camera (muted, or while a camera starts).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StillSource {
    #[default]
    ColorBars,
    Black,
    Image(PathBuf),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SelectedCam {
    /// MF symbolic link; stable identity of the device.
    pub link: String,
    pub name: String,
}

/// Hotkeys in `global-hotkey` string form, e.g. `ctrl+alt+shift+KeyM`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub mute: String,
    pub next: String,
    pub prev: String,
    /// `cameras[i]` activates the i-th selected camera.
    pub cameras: Vec<String>,
}

pub const CAMERA_HOTKEY_SLOTS: usize = 9;

impl Default for HotkeyConfig {
    // Ctrl+Alt alone is AltGr on many keyboard layouts (e.g. AltGr+7 = "{" on
    // German), so defaults add Shift to avoid swallowing typed characters.
    fn default() -> Self {
        Self {
            mute: "ctrl+alt+shift+KeyM".into(),
            next: "ctrl+alt+shift+ArrowRight".into(),
            prev: "ctrl+alt+shift+ArrowLeft".into(),
            cameras: (1..=CAMERA_HOTKEY_SLOTS).map(|i| format!("ctrl+alt+shift+Digit{i}")).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub selected: Vec<SelectedCam>,
    pub mode: SwitchMode,
    pub last_active: Option<String>,
    pub hotkeys: HotkeyConfig,
    /// Stop all captures while no app uses the virtual camera and the window is hidden.
    pub pause_when_idle: bool,
    /// Shown while muted.
    pub muted_image: StillSource,
    /// Shown while the active camera has no picture yet (starting, switching, failed).
    pub placeholder_image: StillSource,
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "CamSwitch").map(|d| d.config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!("invalid config {}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    log::warn!("saving config failed: {e}");
                }
            }
            Err(e) => log::warn!("serializing config failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial() {
        let mut c = Config::default();
        c.selected.push(SelectedCam { link: "\\\\?\\usb#a".into(), name: "Cam A".into() });
        c.mode = SwitchMode::Cold;
        c.pause_when_idle = true;
        c.muted_image = StillSource::Image(PathBuf::from(r"C:\pics\away.png"));
        c.placeholder_image = StillSource::Black;
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.selected[0].name, "Cam A");
        assert_eq!(back.mode, SwitchMode::Cold);
        assert!(back.pause_when_idle);
        assert_eq!(back.muted_image, c.muted_image);
        assert_eq!(back.placeholder_image, StillSource::Black);

        let partial: Config = toml::from_str("mode = \"Cold\"").unwrap();
        assert_eq!(partial.hotkeys.cameras.len(), CAMERA_HOTKEY_SLOTS);
        assert!(!partial.pause_when_idle);
        assert_eq!(partial.muted_image, StillSource::ColorBars);
    }

    #[test]
    fn default_hotkeys_parse() {
        use std::str::FromStr;
        let h = HotkeyConfig::default();
        for s in [&h.mute, &h.next, &h.prev].into_iter().chain(h.cameras.iter()) {
            global_hotkey::hotkey::HotKey::from_str(s).unwrap();
        }
    }
}
