//! Switcher state: selected cameras, active camera, mute, Hot/Cold capture policy.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::{CamStatus, Capture, Slot};
use crate::config::{Anchor, Config, Overlay, OverlayKind, Pip, Scene, SelectedCam, StillSource, SwitchMode, Transform, TransitionKind};
use crate::devices::{DeviceInfo, DeviceList};
use crate::stills::{Still, StillImage};

/// Which still picture: shown while muted, or while the active camera has no picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StillKind {
    Muted,
    Placeholder,
}

pub struct Core {
    pub config: Config,
    /// Cameras usable as inputs.
    pub devices: Vec<DeviceInfo>,
    /// Our own virtual camera(s); listed for information, never captured.
    pub own_devices: Vec<DeviceInfo>,
    /// Symbolic link of the active camera (always one of `config.selected`).
    pub active: Option<String>,
    pub muted: bool,
    captures: HashMap<String, Capture>,
    /// Cold mode: previous camera, kept streaming until the new one delivers.
    fallback: Option<String>,
    /// Crossfade in progress from the previous camera.
    fade: Option<Fade>,
    /// Nobody watches (no consumer, window hidden); set by the compositor.
    idle: bool,
    /// Indexed by [`StillKind`].
    stills: [Still; 2],
    /// Load errors of custom images, indexed by [`StillKind`].
    pub still_errors: [Option<String>; 2],
    /// Bumped whenever a still, a camera transform, picture-in-picture or an
    /// overlay changes, so the compositor redraws.
    render_version: u64,
    /// Decoded overlay images by path (or the load error).
    overlay_images: HashMap<PathBuf, Result<Arc<StillImage>, String>>,
}

/// A crossfade from `from` to the active camera.
struct Fade {
    from: String,
    /// Set once the active camera has a picture.
    started: Option<Instant>,
}

/// Picture-in-picture inset for the compositor.
pub struct PipDraw {
    pub source: (Arc<Slot>, Transform),
    pub corner: Anchor,
    pub size_pct: u8,
}

pub enum OverlayContent {
    Text { text: String, background: bool },
    Image(Arc<StillImage>),
}

/// An overlay for the compositor; `index` into `Config::overlays`.
pub struct OverlayDraw {
    pub index: usize,
    pub content: OverlayContent,
    pub anchor: Anchor,
    pub size_pct: f32,
}

/// What the compositor should show.
pub struct OutputSources {
    pub active: Option<(Arc<Slot>, Transform)>,
    pub fallback: Option<(Arc<Slot>, Transform)>,
    /// Camera being faded out and the fade progress (0 = only it, 1 = done).
    pub fade_from: Option<((Arc<Slot>, Transform), f32)>,
    pub muted: bool,
    pub muted_still: Still,
    pub pip: Option<PipDraw>,
    pub overlays: Vec<OverlayDraw>,
    pub placeholder: Still,
    pub render_version: u64,
}

impl Core {
    pub fn new(config: Config, devices: DeviceList) -> Self {
        let mut core = Self {
            config,
            devices: devices.cameras,
            own_devices: devices.own,
            active: None,
            muted: false,
            captures: HashMap::new(),
            fallback: None,
            fade: None,
            idle: false,
            stills: [Still::ColorBars, Still::ColorBars],
            still_errors: [None, None],
            render_version: 0,
            overlay_images: HashMap::new(),
        };
        for kind in [StillKind::Muted, StillKind::Placeholder] {
            core.load_still(kind);
        }
        core.load_overlay_images();
        // Never feed our own output back in (older configs may contain it).
        core.config.selected.retain(|s| !s.name.starts_with(shared::VCAM_FRIENDLY_NAME));
        core.active = core
            .config
            .last_active
            .clone()
            .filter(|l| core.is_selected(l) && core.is_present(l))
            .or_else(|| core.first_present());
        core.reconcile();
        core
    }

    pub fn is_present(&self, link: &str) -> bool {
        self.devices.iter().any(|d| d.link == link)
    }

    pub fn is_selected(&self, link: &str) -> bool {
        self.config.selected.iter().any(|s| s.link == link)
    }

    fn present_selected(&self) -> impl Iterator<Item = &SelectedCam> {
        self.config.selected.iter().filter(|s| self.is_present(&s.link))
    }

    fn first_present(&self) -> Option<String> {
        self.present_selected().next().map(|s| s.link.clone())
    }

    pub fn slot(&self, link: &str) -> Option<&Arc<Slot>> {
        self.captures.get(link).map(|c| &c.slot)
    }

    pub fn active_name(&self) -> Option<&str> {
        let link = self.active.as_deref()?;
        self.config.selected.iter().find(|s| s.link == link).map(|s| s.name.as_str())
    }

    /// Short description of what the output shows, for the on-screen toast.
    pub fn status_text(&self) -> String {
        if self.muted {
            return "Camera muted".into();
        }
        let Some(link) = self.active.as_deref() else { return "No camera".into() };
        match self.config.selected.iter().position(|s| s.link == link) {
            Some(i) => format!("{} · {}", i + 1, self.config.selected[i].name),
            None => "No camera".into(),
        }
    }

    /// Activates the `index`-th selected camera (hotkey slot order).
    pub fn activate_index(&mut self, index: usize) {
        if let Some(link) = self.config.selected.get(index).map(|s| s.link.clone()) {
            self.activate(&link);
        }
    }

    pub fn activate(&mut self, link: &str) {
        if self.active.as_deref() == Some(link) || !self.is_selected(link) || !self.is_present(link) {
            return;
        }
        // Keep showing the old camera only if it actually has a picture.
        let old = self.active.take().filter(|old| self.slot(old).is_some_and(|s| s.has_frame()));
        if self.config.mode == SwitchMode::Cold {
            self.fallback = old.clone();
        }
        self.fade = old
            .filter(|_| self.config.transition.kind == TransitionKind::Fade)
            .map(|from| Fade { from, started: None });
        self.active = Some(link.to_string());
        self.config.last_active = self.active.clone();
        self.config.save();
        self.reconcile();
    }

    /// Moves to the next (`+1`) or previous (`-1`) present selected camera.
    pub fn cycle(&mut self, delta: isize) {
        let links: Vec<String> = self.present_selected().map(|s| s.link.clone()).collect();
        if links.is_empty() {
            return;
        }
        let cur = self.active.as_ref().and_then(|a| links.iter().position(|l| l == a));
        let next = match cur {
            Some(i) => (i as isize + delta).rem_euclid(links.len() as isize) as usize,
            None => 0,
        };
        self.activate(&links[next].clone());
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }

    pub fn set_mode(&mut self, mode: SwitchMode) {
        if self.config.mode != mode {
            self.config.mode = mode;
            self.fallback = None;
            self.fade = None;
            self.config.save();
            self.reconcile();
        }
    }

    pub fn set_selected(&mut self, dev: &DeviceInfo, on: bool) {
        if on && !self.is_selected(&dev.link) {
            self.config.selected.push(SelectedCam {
                link: dev.link.clone(),
                name: dev.name.clone(),
                transform: Transform::default(),
            });
            if self.active.is_none() {
                self.active = Some(dev.link.clone());
            }
        } else if !on {
            self.config.selected.retain(|s| s.link != dev.link);
            if self.active.as_deref() == Some(dev.link.as_str()) {
                self.active = self.first_present();
            }
            if self.fallback.as_deref() == Some(dev.link.as_str()) {
                self.fallback = None;
            }
        }
        self.config.last_active = self.active.clone();
        self.config.save();
        self.reconcile();
    }

    /// Removes a selected camera that is no longer connected.
    pub fn forget(&mut self, link: &str) {
        self.config.selected.retain(|s| s.link != link);
        if self.active.as_deref() == Some(link) {
            self.active = self.first_present();
        }
        self.config.save();
        self.reconcile();
    }

    pub fn move_selected(&mut self, index: usize, up: bool) {
        let len = self.config.selected.len();
        let other = if up { index.checked_sub(1) } else { (index + 1 < len).then_some(index + 1) };
        if let Some(other) = other {
            self.config.selected.swap(index, other);
            self.config.save();
        }
    }

    /// New device list (refresh / hot-plug). Failed captures are retried.
    pub fn set_devices(&mut self, devices: DeviceList) {
        self.devices = devices.cameras;
        self.own_devices = devices.own;
        self.captures.retain(|_, c| !matches!(*c.slot.status.lock(), CamStatus::Failed(_)));
        if self.active.as_ref().is_none_or(|a| !self.is_present(a)) {
            self.active = self.first_present();
        }
        self.reconcile();
    }

    pub fn transform(&self, link: &str) -> Transform {
        self.config.selected.iter().find(|s| s.link == link).map(|s| s.transform).unwrap_or_default()
    }

    /// Changes a camera's picture adjustments; `save` writes the config (skip
    /// it while a slider is dragged).
    pub fn set_transform(&mut self, link: &str, t: Transform, save: bool) {
        if let Some(cam) = self.config.selected.iter_mut().find(|s| s.link == link)
            && cam.transform != t
        {
            cam.transform = t;
            self.render_version += 1;
        }
        if save {
            self.config.save();
        }
    }

    pub fn still_source(&self, kind: StillKind) -> &StillSource {
        match kind {
            StillKind::Muted => &self.config.muted_image,
            StillKind::Placeholder => &self.config.placeholder_image,
        }
    }

    pub fn set_still_source(&mut self, kind: StillKind, source: StillSource) {
        if *self.still_source(kind) == source {
            return;
        }
        match kind {
            StillKind::Muted => self.config.muted_image = source,
            StillKind::Placeholder => self.config.placeholder_image = source,
        }
        self.config.save();
        self.load_still(kind);
    }

    fn load_still(&mut self, kind: StillKind) {
        let (still, error) = Still::resolve(self.still_source(kind));
        self.stills[kind as usize] = still;
        self.still_errors[kind as usize] = error;
        self.render_version += 1;
    }

    /// `save` writes the config (skip it while a slider is dragged).
    pub fn set_transition(&mut self, t: crate::config::Transition, save: bool) {
        self.config.transition = t;
        if t.kind == TransitionKind::Cut {
            self.fade = None;
        }
        if save {
            self.config.save();
        }
    }

    fn pip_draw(&self) -> Option<PipDraw> {
        let pip = &self.config.pip;
        let link = pip.link.as_deref().filter(|l| pip.enabled && self.active.as_deref() != Some(*l))?;
        Some(PipDraw { source: (self.slot(link)?.clone(), self.transform(link)), corner: pip.corner, size_pct: pip.size_pct })
    }

    fn overlay_draws(&self) -> Vec<OverlayDraw> {
        if !self.config.overlays_visible {
            return Vec::new();
        }
        self.config
            .overlays
            .iter()
            .enumerate()
            .filter(|(_, o)| o.enabled)
            .filter_map(|(index, o)| {
                let content = match &o.kind {
                    OverlayKind::Text { text, background } => {
                        OverlayContent::Text { text: text.clone(), background: *background }
                    }
                    OverlayKind::Image { path } => OverlayContent::Image(self.overlay_images.get(path)?.as_ref().ok()?.clone()),
                };
                Some(OverlayDraw { index, content, anchor: o.anchor, size_pct: o.size_pct })
            })
            .collect()
    }

    /// Load error of an image overlay, if any.
    pub fn overlay_error(&self, index: usize) -> Option<&str> {
        match &self.config.overlays.get(index)?.kind {
            OverlayKind::Image { path } => self.overlay_images.get(path)?.as_ref().err().map(String::as_str),
            OverlayKind::Text { .. } => None,
        }
    }

    /// Decodes images of new overlay paths, forgets unused ones.
    fn load_overlay_images(&mut self) {
        let paths: HashSet<PathBuf> = self
            .config
            .overlays
            .iter()
            .filter_map(|o| match &o.kind {
                OverlayKind::Image { path } => Some(path.clone()),
                OverlayKind::Text { .. } => None,
            })
            .collect();
        self.overlay_images.retain(|p, _| paths.contains(p));
        for path in paths {
            self.overlay_images.entry(path).or_insert_with_key(|path| {
                crate::stills::load_premul(path).map(Arc::new).inspect_err(|e| log::warn!("overlay {}: {e}", path.display()))
            });
        }
    }

    /// `save` writes the config (skip it while a slider is dragged).
    pub fn set_pip(&mut self, pip: Pip, save: bool) {
        if self.config.pip != pip {
            self.config.pip = pip;
            self.render_version += 1;
            self.reconcile();
        }
        if save {
            self.config.save();
        }
    }

    pub fn toggle_pip(&mut self) {
        let mut pip = self.config.pip.clone();
        pip.enabled = !pip.enabled;
        // First use: pick another camera than the active one.
        if pip.enabled && pip.link.is_none() {
            pip.link = self.present_selected().map(|s| s.link.clone()).find(|l| Some(l) != self.active.as_ref());
        }
        self.set_pip(pip, true);
    }

    /// `save` writes the config (skip it while typing or dragging).
    pub fn set_overlays(&mut self, overlays: Vec<Overlay>, save: bool) {
        if self.config.overlays != overlays {
            self.config.overlays = overlays;
            self.load_overlay_images();
            self.render_version += 1;
        }
        if save {
            self.config.save();
        }
    }

    pub fn set_overlays_visible(&mut self, on: bool) {
        if self.config.overlays_visible != on {
            self.config.overlays_visible = on;
            self.render_version += 1;
            self.config.save();
        }
    }

    pub fn toggle_overlays(&mut self) {
        self.set_overlays_visible(!self.config.overlays_visible);
    }

    /// The current camera, picture-in-picture and overlay switches as a scene.
    pub fn current_scene(&self, name: String) -> Scene {
        Scene {
            name,
            active: self.active.clone(),
            pip: self.config.pip.clone(),
            overlays_visible: self.config.overlays_visible,
            overlays_enabled: self.config.overlays.iter().map(|o| o.enabled).collect(),
        }
    }

    pub fn add_scene(&mut self) {
        let name = format!("Scene {}", self.config.scenes.len() + 1);
        self.config.scenes.push(self.current_scene(name));
        self.config.hotkeys.scenes.resize(self.config.scenes.len(), String::new());
        self.config.save();
    }

    pub fn remove_scene(&mut self, index: usize) {
        if index < self.config.scenes.len() {
            self.config.scenes.remove(index);
            if index < self.config.hotkeys.scenes.len() {
                self.config.hotkeys.scenes.remove(index);
            }
            self.config.save();
        }
    }

    /// Recalls scene `index`: camera (with the usual transition),
    /// picture-in-picture and overlay switches.
    pub fn apply_scene(&mut self, index: usize) {
        let Some(scene) = self.config.scenes.get(index).cloned() else { return };
        if let Some(link) = &scene.active {
            self.activate(link);
        }
        let mut overlays = self.config.overlays.clone();
        for (o, on) in overlays.iter_mut().zip(&scene.overlays_enabled) {
            o.enabled = *on;
        }
        self.set_overlays(overlays, false);
        self.config.overlays_visible = scene.overlays_visible;
        self.render_version += 1;
        self.set_pip(scene.pip, true);
    }

    pub fn set_pause_when_idle(&mut self, on: bool) {
        if self.config.pause_when_idle != on {
            self.config.pause_when_idle = on;
            self.config.save();
            self.reconcile();
        }
    }

    /// Called by the compositor when watchers come and go.
    pub fn set_idle(&mut self, idle: bool) {
        if self.idle != idle {
            self.idle = idle;
            if self.config.pause_when_idle {
                log::info!("{}", if idle { "idle: pausing cameras" } else { "watched: resuming cameras" });
                self.reconcile();
            }
        }
    }

    pub fn paused(&self) -> bool {
        self.idle && self.config.pause_when_idle
    }

    /// Called every compositor frame: completes Cold-mode switches.
    pub fn tick(&mut self) {
        if let Some(fade) = &self.fade {
            let active = self.active.as_deref().and_then(|a| self.slot(a));
            let has_frame = active.is_some_and(|s| s.has_frame());
            let failed = active.is_none_or(|s| matches!(*s.status.lock(), CamStatus::Failed(_)));
            let duration = Duration::from_millis(self.config.transition.duration_ms as u64);
            let done = failed || !self.is_present(&fade.from) || fade.started.is_some_and(|t| t.elapsed() >= duration);
            if done {
                self.fade = None;
                self.reconcile();
            } else if fade.started.is_none() && has_frame {
                self.fade.as_mut().unwrap().started = Some(Instant::now());
            }
        }
        if self.fallback.is_some() {
            let done = match self.active.as_deref().and_then(|a| self.slot(a)) {
                Some(slot) => slot.has_frame() || matches!(*slot.status.lock(), CamStatus::Failed(_)),
                None => true,
            };
            if done {
                self.fallback = None;
                self.reconcile();
            }
        }
    }

    pub fn output_sources(&self) -> OutputSources {
        OutputSources {
            active: self.active.as_deref().and_then(|a| Some((self.slot(a)?.clone(), self.transform(a)))),
            fallback: self.fallback.as_deref().and_then(|f| Some((self.slot(f)?.clone(), self.transform(f)))),
            fade_from: self.fade.as_ref().and_then(|f| {
                let duration = self.config.transition.duration_ms.max(1) as f32 / 1000.0;
                let t = f.started.map_or(0.0, |t| (t.elapsed().as_secs_f32() / duration).min(1.0));
                Some(((self.slot(&f.from)?.clone(), self.transform(&f.from)), t))
            }),
            muted: self.muted,
            muted_still: self.stills[StillKind::Muted as usize].clone(),
            placeholder: self.stills[StillKind::Placeholder as usize].clone(),
            render_version: self.render_version,
            pip: self.pip_draw(),
            overlays: self.overlay_draws(),
        }
    }

    fn wanted(&self) -> HashSet<String> {
        if self.paused() {
            return HashSet::new();
        }
        let mut wanted: HashSet<String> = match self.config.mode {
            SwitchMode::Hot => self.present_selected().map(|s| s.link.clone()).collect(),
            SwitchMode::Cold => self.active.iter().cloned().collect(),
        };
        wanted.extend(self.fallback.iter().cloned());
        wanted.extend(self.fade.iter().map(|f| f.from.clone()));
        if self.config.pip.enabled {
            wanted.extend(self.config.pip.link.iter().filter(|l| self.is_selected(l)).cloned());
        }
        wanted.retain(|l| self.is_present(l));
        wanted
    }

    fn reconcile(&mut self) {
        let wanted = self.wanted();
        self.captures.retain(|link, _| wanted.contains(link));
        for link in wanted {
            if !self.captures.contains_key(&link) {
                let name = self.devices.iter().find(|d| d.link == link).map(|d| d.name.clone()).unwrap_or_default();
                self.captures.insert(link.clone(), Capture::start(link, name));
            }
        }
    }
}
