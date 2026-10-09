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
    #[serde(default)]
    pub transform: Transform,
}

/// Clockwise rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub const ALL: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];

    pub fn quarter_turns(self) -> u32 {
        self as u32
    }

    pub fn label(self) -> &'static str {
        match self {
            Rotation::R0 => "0°",
            Rotation::R90 => "90°",
            Rotation::R180 => "180°",
            Rotation::R270 => "270°",
        }
    }
}

/// How a camera picture with a different aspect ratio fills the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Fit {
    /// Whole picture, black bars.
    #[default]
    Letterbox,
    /// Crop to fill the output.
    Fill,
}

/// Per-camera picture adjustments, applied in the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub mirror: bool,
    pub rotate: Rotation,
    /// Digital zoom, 1.0 = whole picture.
    pub zoom: f32,
    /// Center of the zoomed part, 0..1 of the (rotated) picture.
    pub center: [f32; 2],
    pub fit: Fit,
}

impl Default for Transform {
    fn default() -> Self {
        Self { mirror: false, rotate: Rotation::R0, zoom: 1.0, center: [0.5, 0.5], fit: Fit::Letterbox }
    }
}

pub const MAX_ZOOM: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransitionKind {
    /// Switch immediately.
    #[default]
    Cut,
    /// Crossfade from the previous camera.
    Fade,
}

/// How the output changes from one camera to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration_ms: u32,
}

impl Default for Transition {
    fn default() -> Self {
        Self { kind: TransitionKind::Cut, duration_ms: 300 }
    }
}

pub const TRANSITION_MS: std::ops::RangeInclusive<u32> = 100..=1500;

/// Where something sits in the output picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Anchor {
    TopLeft,
    TopRight,
    #[default]
    BottomLeft,
    BottomRight,
    Center,
}

impl Anchor {
    pub const CORNERS: [Anchor; 4] = [Anchor::TopLeft, Anchor::TopRight, Anchor::BottomLeft, Anchor::BottomRight];
    pub const ALL: [Anchor; 5] = [Anchor::TopLeft, Anchor::TopRight, Anchor::BottomLeft, Anchor::BottomRight, Anchor::Center];

    pub fn label(self) -> &'static str {
        match self {
            Anchor::TopLeft => "Top left",
            Anchor::TopRight => "Top right",
            Anchor::BottomLeft => "Bottom left",
            Anchor::BottomRight => "Bottom right",
            Anchor::Center => "Center",
        }
    }
}

/// A second camera in a corner of the output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pip {
    pub enabled: bool,
    /// Symbolic link of the inset camera.
    pub link: Option<String>,
    pub corner: Anchor,
    /// Inset width in percent of the output width.
    pub size_pct: u8,
}

impl Default for Pip {
    fn default() -> Self {
        Self { enabled: false, link: None, corner: Anchor::BottomRight, size_pct: 28 }
    }
}

pub const PIP_SIZE_PCT: std::ops::RangeInclusive<u8> = 10..=50;

/// Text or picture drawn on top of the output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverlayKind {
    /// White text; `background` adds a dark box behind it.
    Text { text: String, background: bool },
    /// Image file; transparency is kept.
    Image { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub enabled: bool,
    pub kind: OverlayKind,
    pub anchor: Anchor,
    /// Text: line height, image: width; percent of the output height / width.
    pub size_pct: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            enabled: true,
            kind: OverlayKind::Text { text: "Your name".into(), background: true },
            anchor: Anchor::BottomLeft,
            size_pct: 6.0,
        }
    }
}

/// Saved output setup recalled at once: camera, picture-in-picture, overlays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    pub name: String,
    /// Symbolic link of the main camera.
    pub active: Option<String>,
    pub pip: Pip,
    pub overlays_visible: bool,
    /// Per-overlay `enabled`, by index.
    pub overlays_enabled: Vec<bool>,
}

impl Default for Scene {
    fn default() -> Self {
        Self { name: "Scene".into(), active: None, pip: Pip::default(), overlays_visible: true, overlays_enabled: Vec::new() }
    }
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
    pub pip: String,
    pub overlays: String,
    /// `scenes[i]` applies `Config::scenes[i]`.
    pub scenes: Vec<String>,
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
            pip: "ctrl+alt+shift+KeyP".into(),
            overlays: "ctrl+alt+shift+KeyO".into(),
            scenes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Start hidden in the tray (also via `--minimized`).
    pub start_minimized: bool,
    /// Briefly show the new state on screen after a hotkey or tray action.
    pub show_toast: bool,
    pub transition: Transition,
    pub pip: Pip,
    pub overlays: Vec<Overlay>,
    /// Master switch for all overlays (hotkey).
    pub overlays_visible: bool,
    pub scenes: Vec<Scene>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            selected: Vec::new(),
            mode: SwitchMode::default(),
            last_active: None,
            hotkeys: HotkeyConfig::default(),
            pause_when_idle: false,
            muted_image: StillSource::default(),
            placeholder_image: StillSource::default(),
            close_to_tray: true,
            start_minimized: false,
            show_toast: true,
            transition: Transition::default(),
            pip: Pip::default(),
            overlays: Vec::new(),
            overlays_visible: true,
            scenes: Vec::new(),
        }
    }
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
        c.selected.push(SelectedCam { link: "\\\\?\\usb#a".into(), name: "Cam A".into(), transform: Transform::default() });
        c.selected[0].transform.mirror = true;
        c.selected[0].transform.rotate = Rotation::R270;
        c.selected[0].transform.zoom = 2.5;
        c.selected[0].transform.fit = Fit::Fill;
        c.mode = SwitchMode::Cold;
        c.pause_when_idle = true;
        c.muted_image = StillSource::Image(PathBuf::from(r"C:\pics\away.png"));
        c.placeholder_image = StillSource::Black;
        c.close_to_tray = false;
        c.start_minimized = true;
        c.transition = Transition { kind: TransitionKind::Fade, duration_ms: 500 };
        c.pip = Pip { enabled: true, link: Some("cam-b".into()), corner: Anchor::TopLeft, size_pct: 30 };
        c.overlays.push(Overlay::default());
        c.overlays.push(Overlay {
            enabled: false,
            kind: OverlayKind::Image { path: PathBuf::from(r"C:\pics\logo.png") },
            anchor: Anchor::TopRight,
            size_pct: 12.5,
        });
        c.overlays_visible = false;
        c.scenes.push(Scene { name: "Talk".into(), active: Some("x".into()), overlays_enabled: vec![true, false], ..Scene::default() });
        c.hotkeys.scenes.push("ctrl+alt+shift+F1".into());
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.selected[0].name, "Cam A");
        assert_eq!(back.selected[0].transform, c.selected[0].transform);

        // Cameras saved before transforms existed.
        let old: Config = toml::from_str("[[selected]]\nlink = 'x'\nname = 'X'").unwrap();
        assert_eq!(old.selected[0].transform, Transform::default());
        assert_eq!(back.mode, SwitchMode::Cold);
        assert!(back.pause_when_idle);
        assert_eq!(back.muted_image, c.muted_image);
        assert_eq!(back.placeholder_image, StillSource::Black);
        assert!(!back.close_to_tray && back.start_minimized && back.show_toast);
        assert_eq!(back.transition, c.transition);
        assert_eq!(back.pip, c.pip);
        assert_eq!(back.overlays, c.overlays);
        assert!(!back.overlays_visible);
        assert_eq!(back.scenes, c.scenes);
        assert_eq!(back.hotkeys.scenes, c.hotkeys.scenes);

        let partial: Config = toml::from_str("mode = \"Cold\"").unwrap();
        assert_eq!(partial.hotkeys.cameras.len(), CAMERA_HOTKEY_SLOTS);
        assert!(!partial.pause_when_idle);
        assert_eq!(partial.muted_image, StillSource::ColorBars);
        assert!(partial.close_to_tray && !partial.start_minimized && partial.show_toast);
        assert_eq!(partial.transition, Transition::default());
        assert_eq!(partial.pip, Pip::default());
        assert!(partial.overlays.is_empty() && partial.overlays_visible && partial.scenes.is_empty());
    }

    #[test]
    fn default_hotkeys_parse() {
        use std::str::FromStr;
        let h = HotkeyConfig::default();
        for s in [&h.mute, &h.next, &h.prev, &h.pip, &h.overlays].into_iter().chain(h.cameras.iter()) {
            global_hotkey::hotkey::HotKey::from_str(s).unwrap();
        }
    }
}
