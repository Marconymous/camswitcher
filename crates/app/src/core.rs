//! Switcher state: selected cameras, active camera, mute, Hot/Cold capture policy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::{CamStatus, Capture, Slot};
use crate::config::{Config, SelectedCam, StillSource, SwitchMode, Transform, TransitionKind};
use crate::devices::{DeviceInfo, DeviceList};
use crate::stills::Still;

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
    /// Bumped whenever a still or a camera transform changes, so the
    /// compositor redraws.
    render_version: u64,
}

/// A crossfade from `from` to the active camera.
struct Fade {
    from: String,
    /// Set once the active camera has a picture.
    started: Option<Instant>,
}

/// What the compositor should show.
pub struct OutputSources {
    pub active: Option<(Arc<Slot>, Transform)>,
    pub fallback: Option<(Arc<Slot>, Transform)>,
    /// Camera being faded out and the fade progress (0 = only it, 1 = done).
    pub fade_from: Option<((Arc<Slot>, Transform), f32)>,
    pub muted: bool,
    pub muted_still: Still,
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
        };
        for kind in [StillKind::Muted, StillKind::Placeholder] {
            core.load_still(kind);
        }
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
