//! Global hotkeys (RegisterHotKey via `global-hotkey`), dispatched straight
//! into [`Core`] so they work while the window is hidden or unfocused.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use parking_lot::Mutex;

use crate::config::HotkeyConfig;
use crate::core::Core;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Mute,
    Next,
    Prev,
    Camera(usize),
    TogglePip,
    ToggleOverlays,
    Scene(usize),
}

impl Action {
    pub fn get(self, cfg: &HotkeyConfig) -> &str {
        match self {
            Action::Mute => &cfg.mute,
            Action::Next => &cfg.next,
            Action::Prev => &cfg.prev,
            Action::Camera(i) => cfg.cameras.get(i).map(String::as_str).unwrap_or(""),
            Action::TogglePip => &cfg.pip,
            Action::ToggleOverlays => &cfg.overlays,
            Action::Scene(i) => cfg.scenes.get(i).map(String::as_str).unwrap_or(""),
        }
    }

    pub fn set(self, cfg: &mut HotkeyConfig, value: String) {
        match self {
            Action::Mute => cfg.mute = value,
            Action::Next => cfg.next = value,
            Action::Prev => cfg.prev = value,
            Action::Camera(i) => {
                if cfg.cameras.len() <= i {
                    cfg.cameras.resize(i + 1, String::new());
                }
                cfg.cameras[i] = value;
            }
            Action::TogglePip => cfg.pip = value,
            Action::ToggleOverlays => cfg.overlays = value,
            Action::Scene(i) => {
                if cfg.scenes.len() <= i {
                    cfg.scenes.resize(i + 1, String::new());
                }
                cfg.scenes[i] = value;
            }
        }
    }

    /// Every bindable action; `scenes` is the number of saved scenes.
    pub fn all(cfg: &HotkeyConfig, scenes: usize) -> Vec<Action> {
        let mut v = vec![Action::Mute, Action::Next, Action::Prev];
        v.extend((0..cfg.cameras.len()).map(Action::Camera));
        v.extend([Action::TogglePip, Action::ToggleOverlays]);
        v.extend((0..scenes).map(Action::Scene));
        v
    }

    /// Applies the action to `core`.
    pub fn run(self, c: &mut Core) {
        match self {
            Action::Mute => c.toggle_mute(),
            Action::Next => c.cycle(1),
            Action::Prev => c.cycle(-1),
            Action::Camera(i) => c.activate_index(i),
            Action::TogglePip => c.toggle_pip(),
            Action::ToggleOverlays => c.toggle_overlays(),
            Action::Scene(i) => c.apply_scene(i),
        }
    }

    /// On-screen notification after the action ran.
    pub fn toast_text(self, c: &Core) -> String {
        let on_off = |on: bool| if on { "on" } else { "off" };
        match self {
            Action::TogglePip => format!("Picture-in-picture {}", on_off(c.config.pip.enabled)),
            Action::ToggleOverlays => format!("Overlays {}", on_off(c.config.overlays_visible)),
            Action::Scene(i) => match c.config.scenes.get(i) {
                Some(scene) => format!("Scene · {}", scene.name),
                None => c.status_text(),
            },
            _ => c.status_text(),
        }
    }

    /// Label in the hotkey list.
    pub fn name(self, c: &Core) -> String {
        match self {
            Action::Mute => "Mute".to_string(),
            Action::Next => "Next camera".to_string(),
            Action::Prev => "Previous camera".to_string(),
            Action::Camera(i) => format!("Camera {}", i + 1),
            Action::TogglePip => "Picture-in-picture".to_string(),
            Action::ToggleOverlays => "Overlays".to_string(),
            Action::Scene(i) => format!("Scene: {}", c.config.scenes.get(i).map_or("?", |s| s.name.as_str())),
        }
    }
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    bindings: Arc<Mutex<HashMap<u32, Action>>>,
    /// Registration errors per action, shown next to the binding.
    pub errors: HashMap<Action, String>,
}

impl Hotkeys {
    /// `after` runs once an action was applied, without the core lock held.
    pub fn new(core: Arc<Mutex<Core>>, after: impl Fn(Action) + Send + Sync + 'static) -> global_hotkey::Result<Self> {
        let manager = GlobalHotKeyManager::new()?;
        let bindings: Arc<Mutex<HashMap<u32, Action>>> = Arc::default();
        {
            let bindings = bindings.clone();
            GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
                if ev.state() != HotKeyState::Pressed {
                    return;
                }
                let Some(action) = bindings.lock().get(&ev.id()).copied() else { return };
                log::info!("hotkey {action:?}");
                action.run(&mut core.lock());
                after(action);
            }));
        }
        Ok(Self { manager, registered: Vec::new(), bindings, errors: HashMap::new() })
    }

    /// (Re-)registers all hotkeys from `cfg`.
    pub fn apply(&mut self, cfg: &HotkeyConfig, scenes: usize) {
        let _ = self.manager.unregister_all(&self.registered);
        self.registered.clear();
        self.errors.clear();
        let mut bindings = HashMap::new();
        for action in Action::all(cfg, scenes) {
            let s = action.get(cfg);
            if s.is_empty() {
                continue;
            }
            match HotKey::from_str(s) {
                Ok(hk) => match self.manager.register(hk) {
                    Ok(()) => {
                        bindings.insert(hk.id(), action);
                        self.registered.push(hk);
                    }
                    Err(e) => {
                        self.errors.insert(action, format!("in use: {e}"));
                    }
                },
                Err(e) => {
                    self.errors.insert(action, format!("invalid: {e}"));
                }
            }
        }
        *self.bindings.lock() = bindings;
    }
}

/// Human-readable form, e.g. "Ctrl+Alt+Shift+M".
pub fn display(s: &str) -> String {
    if s.is_empty() {
        return "—".into();
    }
    s.split('+')
        .map(|t| match t.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" => "Win".to_string(),
            _ => t.strip_prefix("Key").or_else(|| t.strip_prefix("Digit")).unwrap_or(t).replace("Arrow", "").to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Builds a hotkey string from an egui key press, if the key is bindable.
pub fn from_egui(key: eframe::egui::Key, mods: eframe::egui::Modifiers) -> Option<String> {
    use eframe::egui::Key as K;
    let code = match key {
        K::A => Code::KeyA,
        K::B => Code::KeyB,
        K::C => Code::KeyC,
        K::D => Code::KeyD,
        K::E => Code::KeyE,
        K::F => Code::KeyF,
        K::G => Code::KeyG,
        K::H => Code::KeyH,
        K::I => Code::KeyI,
        K::J => Code::KeyJ,
        K::K => Code::KeyK,
        K::L => Code::KeyL,
        K::M => Code::KeyM,
        K::N => Code::KeyN,
        K::O => Code::KeyO,
        K::P => Code::KeyP,
        K::Q => Code::KeyQ,
        K::R => Code::KeyR,
        K::S => Code::KeyS,
        K::T => Code::KeyT,
        K::U => Code::KeyU,
        K::V => Code::KeyV,
        K::W => Code::KeyW,
        K::X => Code::KeyX,
        K::Y => Code::KeyY,
        K::Z => Code::KeyZ,
        K::Num0 => Code::Digit0,
        K::Num1 => Code::Digit1,
        K::Num2 => Code::Digit2,
        K::Num3 => Code::Digit3,
        K::Num4 => Code::Digit4,
        K::Num5 => Code::Digit5,
        K::Num6 => Code::Digit6,
        K::Num7 => Code::Digit7,
        K::Num8 => Code::Digit8,
        K::Num9 => Code::Digit9,
        K::F1 => Code::F1,
        K::F2 => Code::F2,
        K::F3 => Code::F3,
        K::F4 => Code::F4,
        K::F5 => Code::F5,
        K::F6 => Code::F6,
        K::F7 => Code::F7,
        K::F8 => Code::F8,
        K::F9 => Code::F9,
        K::F10 => Code::F10,
        K::F11 => Code::F11,
        K::F12 => Code::F12,
        K::F13 => Code::F13,
        K::F14 => Code::F14,
        K::F15 => Code::F15,
        K::F16 => Code::F16,
        K::F17 => Code::F17,
        K::F18 => Code::F18,
        K::F19 => Code::F19,
        K::F20 => Code::F20,
        K::ArrowLeft => Code::ArrowLeft,
        K::ArrowRight => Code::ArrowRight,
        K::ArrowUp => Code::ArrowUp,
        K::ArrowDown => Code::ArrowDown,
        K::Home => Code::Home,
        K::End => Code::End,
        K::PageUp => Code::PageUp,
        K::PageDown => Code::PageDown,
        K::Insert => Code::Insert,
        K::Delete => Code::Delete,
        K::Space => Code::Space,
        _ => return None,
    };
    let mut m = Modifiers::empty();
    if mods.ctrl {
        m |= Modifiers::CONTROL;
    }
    if mods.alt {
        m |= Modifiers::ALT;
    }
    if mods.shift {
        m |= Modifiers::SHIFT;
    }
    Some(HotKey::new(Some(m), code).into_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_readable() {
        assert_eq!(display("ctrl+alt+shift+KeyM"), "Ctrl+Alt+Shift+M");
        assert_eq!(display("ctrl+alt+shift+Digit3"), "Ctrl+Alt+Shift+3");
        assert_eq!(display("ctrl+alt+shift+ArrowRight"), "Ctrl+Alt+Shift+Right");
    }

    #[test]
    fn egui_key_roundtrips_through_parser() {
        let mods = eframe::egui::Modifiers { ctrl: true, alt: true, shift: true, ..Default::default() };
        let s = from_egui(eframe::egui::Key::M, mods).unwrap();
        assert_eq!(HotKey::from_str(&s).unwrap(), HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT), Code::KeyM));
    }
}
