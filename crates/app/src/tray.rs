//! Notification-area icon with a menu: show the window, pick a camera, mute,
//! picture-in-picture, overlays, scenes, autostart, quit. The icon gets a red
//! slash while muted.
//!
//! Tray, menu and hotkey events are all delivered on the UI (main) thread by
//! the window procedures of `tray-icon`, `muda` and `global-hotkey`, so the
//! non-`Send` tray objects live in a thread-local and [`sync`] is a no-op on
//! any other thread.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, ViewportCommand};
use parking_lot::Mutex;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::core::Core;
use crate::hotkeys::Action;

/// Controls the main window from tray and hotkey handlers.
#[derive(Clone)]
pub struct WindowCtl {
    pub ctx: egui::Context,
    /// Set by "Quit" so the close request isn't turned into hide-to-tray.
    pub quitting: Arc<AtomicBool>,
}

impl WindowCtl {
    pub fn show(&self) {
        self.ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        self.ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        self.ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.ctx.request_repaint();
    }

    pub fn hide(&self) {
        self.ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    pub fn quit(&self) {
        self.quitting.store(true, Ordering::Relaxed);
        self.ctx.send_viewport_cmd(ViewportCommand::Close);
        self.ctx.request_repaint();
    }
}

/// What the tray shows; taken while holding the core lock, applied without it.
#[derive(Clone, PartialEq)]
pub struct TrayView {
    /// Selected cameras in hotkey order: name, connected.
    cams: Vec<(String, bool)>,
    active: Option<usize>,
    muted: bool,
    pip: bool,
    overlays: bool,
    scenes: Vec<String>,
}

impl TrayView {
    pub fn of(core: &Core) -> Self {
        Self {
            cams: core.config.selected.iter().map(|s| (s.name.clone(), core.is_present(&s.link))).collect(),
            active: core.active.as_deref().and_then(|a| core.config.selected.iter().position(|s| s.link == a)),
            muted: core.muted,
            pip: core.config.pip.enabled,
            overlays: core.config.overlays_visible,
            scenes: core.config.scenes.iter().map(|s| s.name.clone()).collect(),
        }
    }

    fn tooltip(&self) -> String {
        let name = self.active.and_then(|i| self.cams.get(i)).map_or("no camera", |(n, _)| n.as_str());
        if self.muted { format!("CamSwitch — muted ({name})") } else { format!("CamSwitch — {name}") }
    }
}

struct State {
    tray: TrayIcon,
    menu: Menu,
    mute: CheckMenuItem,
    pip: CheckMenuItem,
    overlays: CheckMenuItem,
    autostart: CheckMenuItem,
    cams: Vec<CheckMenuItem>,
    view: Option<TrayView>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

const ID_SHOW: &str = "show";
const ID_MUTE: &str = "mute";
const ID_PIP: &str = "pip";
const ID_OVERLAYS: &str = "overlays";
const SCENE_PREFIX: &str = "scene:";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const CAM_PREFIX: &str = "cam:";

/// Creates the tray icon on the current (UI) thread.
pub fn create(core: Arc<Mutex<Core>>, window: WindowCtl) -> Result<(), String> {
    let menu = Menu::new();
    let mute = CheckMenuItem::with_id(ID_MUTE, "Mute camera", true, false, None);
    let pip = CheckMenuItem::with_id(ID_PIP, "Picture-in-picture", true, false, None);
    let overlays = CheckMenuItem::with_id(ID_OVERLAYS, "Overlays", true, false, None);
    let autostart = CheckMenuItem::with_id(ID_AUTOSTART, "Start with Windows", true, false, None);
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_menu_on_left_click(false)
        .with_icon(icon(false))
        .with_tooltip("CamSwitch")
        .build()
        .map_err(|e| e.to_string())?;
    STATE.with_borrow_mut(|s| *s = Some(State { tray, menu, mute, pip, overlays, autostart, cams: Vec::new(), view: None }));
    sync(&TrayView::of(&core.lock()));

    {
        let core = core.clone();
        let window = window.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| match ev {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
            | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => window.show(),
            // Refresh before the menu opens on button-up (state may have
            // changed while nothing redrew, e.g. window hidden).
            TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Down, .. } => {
                let view = TrayView::of(&core.lock());
                sync(&view);
                STATE.with_borrow(|s| {
                    if let Some(s) = s {
                        s.autostart.set_checked(crate::autostart::is_enabled());
                    }
                });
            }
            _ => {}
        }));
    }

    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        let id = ev.id.0.as_str();
        let index = |prefix: &str| id.strip_prefix(prefix).and_then(|i| i.parse().ok());
        let action = match id {
            ID_SHOW => {
                window.show();
                None
            }
            ID_QUIT => {
                window.quit();
                None
            }
            ID_AUTOSTART => {
                if let Err(e) = crate::autostart::set(!crate::autostart::is_enabled()) {
                    log::warn!("autostart: {e}");
                }
                None
            }
            ID_MUTE => Some(Action::Mute),
            ID_PIP => Some(Action::TogglePip),
            ID_OVERLAYS => Some(Action::ToggleOverlays),
            _ => index(CAM_PREFIX).map(Action::Camera).or_else(|| index(SCENE_PREFIX).map(Action::Scene)),
        };
        let (view, text, show_toast) = {
            let mut c = core.lock();
            if let Some(a) = action {
                a.run(&mut c);
            }
            (TrayView::of(&c), action.map(|a| a.toast_text(&c)), c.config.show_toast)
        };
        // Check items toggle themselves on click; force them back to the truth.
        view_changed(&view);
        STATE.with_borrow(|s| {
            if let Some(s) = s {
                s.autostart.set_checked(crate::autostart::is_enabled());
            }
        });
        if let Some(text) = text.filter(|_| show_toast) {
            crate::toast::show(&text);
        }
        window.ctx.request_repaint();
    }));
    Ok(())
}

/// Brings the tray up to date; cheap when nothing changed.
pub fn sync(view: &TrayView) {
    STATE.with_borrow_mut(|s| {
        let Some(s) = s else { return };
        if s.view.as_ref() == Some(view) {
            return;
        }
        let old = s.view.replace(view.clone());
        if old.as_ref().map(|o| (&o.cams, &o.scenes)) != Some((&view.cams, &view.scenes)) {
            rebuild(s, view);
        }
        if old.as_ref().map(|o| o.muted) != Some(view.muted) {
            let _ = s.tray.set_icon(Some(icon(view.muted)));
        }
        apply_checks(s, view);
        let _ = s.tray.set_tooltip(Some(view.tooltip()));
    });
}

/// Like [`sync`] but always re-applies check marks (after a menu click).
fn view_changed(view: &TrayView) {
    sync(view);
    STATE.with_borrow(|s| {
        if let Some(s) = s {
            apply_checks(s, view);
        }
    });
}

fn apply_checks(s: &State, view: &TrayView) {
    s.mute.set_checked(view.muted);
    s.pip.set_checked(view.pip);
    s.overlays.set_checked(view.overlays);
    for (i, item) in s.cams.iter().enumerate() {
        item.set_checked(view.active == Some(i));
    }
}

/// Rebuilds the menu items in place; the native menu handle stays the same.
fn rebuild(s: &mut State, view: &TrayView) {
    while s.menu.remove_at(0).is_some() {}
    s.cams = view
        .cams
        .iter()
        .enumerate()
        .map(|(i, (name, present))| {
            let label = if i < 9 { format!("&{} {name}", i + 1) } else { name.clone() };
            CheckMenuItem::with_id(format!("{CAM_PREFIX}{i}"), label, *present, false, None)
        })
        .collect();
    let show = MenuItem::with_id(ID_SHOW, "Show CamSwitch", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
    let _ = s.menu.append(&show);
    let _ = s.menu.append(&PredefinedMenuItem::separator());
    if s.cams.is_empty() {
        let _ = s.menu.append(&MenuItem::new("No cameras selected", false, None));
    }
    for item in &s.cams {
        let _ = s.menu.append(item);
    }
    let _ = s.menu.append(&PredefinedMenuItem::separator());
    let _ = s.menu.append(&s.mute);
    let _ = s.menu.append(&s.pip);
    let _ = s.menu.append(&s.overlays);
    if !view.scenes.is_empty() {
        let scenes = Submenu::new("Scenes", true);
        for (i, name) in view.scenes.iter().enumerate() {
            let _ = scenes.append(&MenuItem::with_id(format!("{SCENE_PREFIX}{i}"), name, true, None));
        }
        let _ = s.menu.append(&scenes);
    }
    let _ = s.menu.append(&PredefinedMenuItem::separator());
    let _ = s.menu.append(&s.autostart);
    let _ = s.menu.append(&quit);
}

/// 32x32 camera icon (rendered by `scripts/make-icon.py`); slashed while muted.
fn icon(muted: bool) -> Icon {
    let png: &[u8] = if muted {
        include_bytes!("../assets/tray-muted.png")
    } else {
        include_bytes!("../assets/tray.png")
    };
    let img = image::load_from_memory(png).expect("valid icon").into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).expect("valid icon")
}
