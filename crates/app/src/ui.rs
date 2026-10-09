//! egui window: camera selection, switch mode, mute, hotkeys, live preview.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use parking_lot::Mutex;

use crate::capture::CamStatus;
use crate::compositor::{PREVIEW_HEIGHT, PREVIEW_WIDTH, Preview};
use crate::config::{StillSource, SwitchMode};
use crate::core::{Core, StillKind};
use crate::devices::{DeviceInfo, DeviceList};
use crate::hotkeys::{self, Action, Hotkeys};
use crate::sysinfo::SysInfo;
use crate::vcam::{VcamState, VirtualCamera};

const DEVICE_REFRESH: Duration = Duration::from_secs(5);

pub struct App {
    pub core: Arc<Mutex<Core>>,
    pub preview: Arc<Mutex<Preview>>,
    pub hotkeys: Result<Hotkeys, String>,
    pub vcam: VirtualCamera,
    pub stop: Arc<AtomicBool>,
    /// `now_ms()` of the last frame drawn while the window was visible.
    pub ui_heartbeat: Arc<AtomicU64>,
    pub texture: Option<egui::TextureHandle>,
    pub last_seq: u64,
    pub recording: Option<Action>,
    pub device_scan: Option<Receiver<DeviceList>>,
    pub last_scan: Instant,
    /// Debug info window open (F12).
    pub show_debug: bool,
    pub sysinfo: SysInfo,
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl App {
    fn poll_devices(&mut self) {
        if let Some(rx) = &self.device_scan {
            match rx.try_recv() {
                Ok(devices) => {
                    let mut core = self.core.lock();
                    if core.devices != devices.cameras || core.own_devices != devices.own {
                        log::info!("devices changed: {} present", devices.cameras.len());
                    }
                    core.set_devices(devices);
                    self.device_scan = None;
                }
                Err(TryRecvError::Disconnected) => self.device_scan = None,
                Err(TryRecvError::Empty) => {}
            }
        } else if self.last_scan.elapsed() > DEVICE_REFRESH {
            self.start_scan();
        }
    }

    fn start_scan(&mut self) {
        self.last_scan = Instant::now();
        self.device_scan = Some(crate::scan_devices_async());
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let p = self.preview.lock();
        if p.seq == self.last_seq && self.texture.is_some() {
            return;
        }
        self.last_seq = p.seq;
        if p.rgba.len() != (PREVIEW_WIDTH * PREVIEW_HEIGHT * 4) as usize {
            return;
        }
        let image = egui::ColorImage::from_rgba_premultiplied([PREVIEW_WIDTH as usize, PREVIEW_HEIGHT as usize], &p.rgba);
        match &mut self.texture {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)),
        }
    }

    fn cameras_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Cameras");
            if ui.button("⟳ Refresh").clicked() {
                self.start_scan();
            }
        });
        let mut core = self.core.lock();
        if core.devices.is_empty() && core.config.selected.is_empty() {
            ui.label("No cameras found.");
        }

        // Selected cameras first, in hotkey order.
        let selected = core.config.selected.clone();
        let count = selected.len();
        for (i, cam) in selected.iter().enumerate() {
            let present = core.is_present(&cam.link);
            let is_active = core.active.as_deref() == Some(cam.link.as_str());
            ui.horizontal(|ui| {
                let mut on = true;
                if ui.checkbox(&mut on, "").changed() {
                    let dev = DeviceInfo { name: cam.name.clone(), link: cam.link.clone() };
                    if present { core.set_selected(&dev, false) } else { core.forget(&cam.link) }
                }
                ui.label(RichText::new(format!("{}", i + 1)).monospace().strong());
                let label = RichText::new(&cam.name).strong();
                let label = if is_active { label.color(Color32::from_rgb(80, 200, 120)) } else { label };
                if ui.add_enabled(present, egui::Button::selectable(is_active, label)).clicked() {
                    core.activate(&cam.link);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(i + 1 < count, egui::Button::new("⏷").small()).clicked() {
                        core.move_selected(i, false);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("⏶").small()).clicked() {
                        core.move_selected(i, true);
                    }
                });
            });
            let status = if !present {
                RichText::new("disconnected").color(Color32::GRAY)
            } else {
                match core.slot(&cam.link).map(|s| s.status.lock().clone()) {
                    None => RichText::new("idle").color(Color32::GRAY),
                    Some(CamStatus::Starting) => RichText::new("starting…").color(Color32::YELLOW),
                    Some(CamStatus::Streaming { width, height, fps }) => {
                        RichText::new(format!("{width}x{height} @ {fps:.0} fps")).color(Color32::GRAY)
                    }
                    Some(CamStatus::Failed(e)) => RichText::new(format!("error: {e}")).color(Color32::LIGHT_RED),
                }
            };
            ui.indent(("status", i), |ui| ui.label(status.small()));
        }

        let unselected: Vec<DeviceInfo> =
            core.devices.iter().filter(|d| !core.is_selected(&d.link)).cloned().collect();
        if !unselected.is_empty() || !core.own_devices.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Available").small().color(Color32::GRAY));
            for dev in unselected {
                let mut on = false;
                if ui.checkbox(&mut on, &dev.name).changed() {
                    core.set_selected(&dev, true);
                }
            }
            for dev in &core.own_devices {
                ui.add_enabled(false, egui::Checkbox::new(&mut false, &dev.name))
                    .on_disabled_hover_text("CamSwitch's own output — can't be used as an input");
            }
        }
    }

    fn controls_ui(&mut self, ui: &mut egui::Ui) {
        let mut core = self.core.lock();
        ui.heading("Switching");
        let mut mode = core.config.mode;
        ui.radio_value(&mut mode, SwitchMode::Hot, "Instant — all selected cameras stay on");
        ui.radio_value(&mut mode, SwitchMode::Cold, "On demand — only the active camera is on");
        core.set_mode(mode);
        let mut pause = core.config.pause_when_idle;
        if ui
            .checkbox(&mut pause, "Pause cameras when nobody is watching")
            .on_hover_text(
                "Turns cameras off while no app uses the virtual camera and this window is minimized or hidden. \
                 They take a moment to start again.",
            )
            .changed()
        {
            core.set_pause_when_idle(pause);
        }

        ui.add_space(8.0);
        let (text, fill) = if core.muted {
            ("🚫 Camera muted — click to unmute", Color32::from_rgb(170, 40, 40))
        } else {
            ("Mute camera (black screen)", ui.visuals().widgets.inactive.weak_bg_fill)
        };
        let hint = hotkeys::display(&core.config.hotkeys.mute);
        if ui
            .add_sized([ui.available_width(), 32.0], egui::Button::new(text).fill(fill))
            .on_hover_text(hint)
            .clicked()
        {
            core.toggle_mute();
        }
        drop(core);

        ui.add_space(8.0);
        ui.heading("Virtual camera");
        let state = self.vcam.state.lock().clone();
        match state {
            VcamState::Starting => ui.label("starting…"),
            VcamState::Running => ui.label(RichText::new(format!("“{}” available", shared::VCAM_FRIENDLY_NAME)).color(Color32::from_rgb(80, 200, 120))),
            VcamState::Failed(e) => ui.label(RichText::new(e).color(Color32::LIGHT_RED)),
        };
        let p = self.preview.lock();
        let usage = if p.consumer_active {
            format!("in use by an app · {}x{}", p.output_size.0, p.output_size.1)
        } else {
            "not in use".to_string()
        };
        drop(p);
        ui.label(RichText::new(usage).small().color(Color32::GRAY));

        ui.add_space(8.0);
        if ui.selectable_label(self.show_debug, "🐞 Debug info").on_hover_text("Live throughput and internals (F12)").clicked() {
            self.show_debug = !self.show_debug;
        }
    }

    fn stills_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Pictures").default_open(false).show(ui, |ui| {
            let mut pick = None;
            {
                let mut core = self.core.lock();
                egui::Grid::new("stills").num_columns(2).show(ui, |ui| {
                    for (kind, label) in [(StillKind::Muted, "Muted"), (StillKind::Placeholder, "Starting / switching")] {
                        ui.label(label);
                        let current = core.still_source(kind).clone();
                        let text = match &current {
                            StillSource::ColorBars => "Color bars".to_string(),
                            StillSource::Black => "Black".to_string(),
                            StillSource::Image(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned()),
                        };
                        let mut choice = None;
                        let combo = egui::ComboBox::from_id_salt(("still", kind as u8)).selected_text(text).show_ui(ui, |ui| {
                            if ui.selectable_label(current == StillSource::ColorBars, "Color bars").clicked() {
                                choice = Some(StillSource::ColorBars);
                            }
                            if ui.selectable_label(current == StillSource::Black, "Black").clicked() {
                                choice = Some(StillSource::Black);
                            }
                            if ui.selectable_label(matches!(current, StillSource::Image(_)), "Image…").clicked() {
                                pick = Some(kind);
                            }
                        });
                        if let StillSource::Image(p) = &current {
                            combo.response.on_hover_text(p.display().to_string());
                        }
                        if let Some(source) = choice {
                            core.set_still_source(kind, source);
                        }
                        ui.end_row();
                        if let Some(err) = &core.still_errors[kind as usize] {
                            ui.label("");
                            ui.label(RichText::new(format!("can't load image: {err}")).small().color(Color32::LIGHT_RED));
                            ui.end_row();
                        }
                    }
                });
            }
            // The dialog is modal and blocking: never hold the core lock across it,
            // or the compositor and hotkeys would stall.
            if let Some(kind) = pick {
                let start_dir = match self.core.lock().still_source(kind) {
                    StillSource::Image(p) => p.parent().map(|d| d.to_path_buf()),
                    _ => None,
                };
                let mut dialog = rfd::FileDialog::new().add_filter("Images", crate::stills::EXTENSIONS);
                if let Some(dir) = start_dir {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    self.core.lock().set_still_source(kind, StillSource::Image(path));
                }
            }
        });
    }

    fn hotkeys_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Hotkeys").default_open(false).show(ui, |ui| {
            let hk = match &self.hotkeys {
                Ok(h) => h,
                Err(e) => {
                    ui.label(RichText::new(format!("Global hotkeys unavailable: {e}")).color(Color32::LIGHT_RED));
                    return;
                }
            };
            let cfg = self.core.lock().config.hotkeys.clone();
            let mut clear = None;
            egui::Grid::new("hotkeys").num_columns(3).striped(true).show(ui, |ui| {
                for action in Action::all(&cfg) {
                    let name = match action {
                        Action::Mute => "Mute".to_string(),
                        Action::Next => "Next camera".to_string(),
                        Action::Prev => "Previous camera".to_string(),
                        Action::Camera(i) => format!("Camera {}", i + 1),
                    };
                    ui.label(name);
                    let text = if self.recording == Some(action) {
                        "press keys… (Esc cancels)".to_string()
                    } else {
                        hotkeys::display(action.get(&cfg))
                    };
                    if ui.button(text).clicked() {
                        self.recording = Some(action);
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").on_hover_text("Clear").clicked() {
                            clear = Some(action);
                        }
                        if let Some(err) = hk.errors.get(&action) {
                            ui.label(RichText::new(err).small().color(Color32::LIGHT_RED));
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some(action) = clear {
                self.set_hotkey(action, String::new());
            }
            ui.label(RichText::new("Hotkeys work system-wide, also while other apps are focused.").small().color(Color32::GRAY));
        });
    }

    fn set_hotkey(&mut self, action: Action, value: String) {
        let mut core = self.core.lock();
        action.set(&mut core.config.hotkeys, value);
        core.config.save();
        let cfg = core.config.hotkeys.clone();
        drop(core);
        if let Ok(h) = &mut self.hotkeys {
            h.apply(&cfg);
        }
    }

    fn handle_recording(&mut self, ctx: &egui::Context) {
        let Some(action) = self.recording else { return };
        let pressed = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        if let Some((key, mods)) = pressed {
            if key == egui::Key::Escape {
                self.recording = None;
            } else if let Some(s) = hotkeys::from_egui(key, mods) {
                self.recording = None;
                self.set_hotkey(action, s);
            }
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tex) = &self.texture else {
            ui.centered_and_justified(|ui| ui.label("waiting for frames…"));
            return;
        };
        let avail = ui.available_size();
        let aspect = PREVIEW_WIDTH as f32 / PREVIEW_HEIGHT as f32;
        let size = if avail.x / avail.y > aspect { egui::vec2(avail.y * aspect, avail.y) } else { egui::vec2(avail.x, avail.x / aspect) };
        let rect = egui::Rect::from_center_size(ui.max_rect().center(), size);
        ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);

        let core = self.core.lock();
        let painter = ui.painter_at(rect);
        let name = core.active_name().unwrap_or("no camera selected");
        let label_pos = rect.left_top() + egui::vec2(10.0, 10.0);
        let galley = painter.layout_no_wrap(name.to_string(), egui::FontId::proportional(16.0), Color32::WHITE);
        painter.rect_filled(egui::Rect::from_min_size(label_pos - egui::vec2(6.0, 4.0), galley.size() + egui::vec2(12.0, 8.0)), 4.0, Color32::from_black_alpha(160));
        painter.galley(label_pos, galley, Color32::WHITE);
        if core.muted {
            let galley = painter.layout_no_wrap("MUTED".into(), egui::FontId::proportional(28.0), Color32::WHITE);
            let r = egui::Rect::from_center_size(rect.center(), galley.size() + egui::vec2(28.0, 14.0));
            painter.rect_filled(r, 6.0, Color32::from_rgb(190, 30, 30));
            painter.galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // eframe stops calling us while minimized/occluded; the compositor
        // treats a stale heartbeat as "preview hidden".
        if ctx.input(|i| i.viewport().visible().unwrap_or(true)) {
            self.ui_heartbeat.store(shared::shm::now_ms(), Ordering::Relaxed);
        }
        self.poll_devices();
        self.handle_recording(&ctx);
        self.update_texture(&ctx);

        egui::Panel::left("controls").resizable(true).default_size(340.0).min_size(280.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                self.cameras_ui(ui);
                ui.separator();
                self.controls_ui(ui);
                ui.separator();
                self.stills_ui(ui);
                ui.separator();
                self.hotkeys_ui(ui);
            });
        });
        egui::CentralPanel::default_margins().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            self.preview_ui(ui);
        });
        self.debug_window(&ctx);
        // Keep polling device changes even without frames.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}
