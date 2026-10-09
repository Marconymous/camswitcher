//! "Debug info" window: live throughput and internals for nerds (F12).

use std::fmt::Write as _;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use shared::shm::{CONSUMER_FORMAT_NV12, CONSUMER_FORMAT_RGB32};
use shared::stats::{History, Snapshot};

use crate::capture::CamStatus;
use crate::compositor::{CompositorStats, OutputSource};
use crate::config::SwitchMode;
use crate::ui::App;
use crate::vcam::VcamState;

const REFRESH: Duration = Duration::from_millis(250);

/// One titled block of label/value rows; rendered as a grid and as plain text.
struct Section {
    title: String,
    rows: Vec<(&'static str, String)>,
    /// fps history drawn below the rows.
    sparkline: Option<History<60>>,
}

impl Section {
    fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), rows: Vec::new(), sparkline: None }
    }

    fn row(&mut self, label: &'static str, value: impl Into<String>) -> &mut Self {
        self.rows.push((label, value.into()));
        self
    }
}

fn mb_per_sec(bytes: f64) -> String {
    format!("{:.1} MB/s", bytes / 1e6)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn timing(s: &Snapshot) -> String {
    format!("avg {:.2} ms · max {:.2} ms", s.avg, s.max)
}

fn uptime(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

impl App {
    /// Shows the debug window while [`App::show_debug`] is set.
    pub fn debug_window(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.show_debug = !self.show_debug;
        }
        if !self.show_debug {
            return;
        }
        self.sysinfo.update();
        let sections = self.collect_sections();

        let builder = egui::ViewportBuilder::default()
            .with_title("CamSwitch – Debug info")
            .with_inner_size([560.0, 680.0])
            .with_icon(crate::window_icon());
        let mut close = false;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("debug"), builder, |ui, _class| {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::F12)) {
                close = true;
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Debug info");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("📋 Copy as text").clicked() {
                            ctx.copy_text(as_text(&sections));
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, section) in sections.iter().enumerate() {
                        section_ui(ui, i, section);
                    }
                });
            });
            ctx.request_repaint_after(REFRESH);
        });
        if close {
            self.show_debug = false;
        }
    }

    fn collect_sections(&self) -> Vec<Section> {
        let mut out = Vec::new();
        let stats = self.preview.lock().stats.clone();
        let (output_size, shm_connected, consumer_active) = {
            let p = self.preview.lock();
            (p.output_size, p.shm_connected, p.consumer_active)
        };

        let core = self.core.lock();
        let mut total_bytes = 0.0;
        let mut streaming = 0;
        for (i, cam) in core.config.selected.iter().enumerate() {
            let Some(slot) = core.slot(&cam.link) else { continue };
            let active = core.active.as_deref() == Some(cam.link.as_str());
            let mut s = Section::new(format!("Camera {} · {}{}", i + 1, cam.name, if active { " (active)" } else { "" }));
            let status = slot.status.lock().clone();
            let st = slot.stats.lock().clone();
            match &status {
                CamStatus::Starting => s.row("status", "starting"),
                CamStatus::Streaming { width, height, .. } => s.row("status", format!("streaming · decoded {width}x{height} BGRA")),
                CamStatus::Failed(e) => s.row("status", format!("failed: {e}")),
            };
            if matches!(status, CamStatus::Streaming { .. }) {
                streaming += 1;
                total_bytes += st.bytes_per_sec;
                let age = slot.frame.lock().as_ref().map(|f| f.captured.elapsed());
                s.row("native format", st.native.clone())
                    .row("frame rate", format!("{:.1} fps", st.fps))
                    .row("throughput", mb_per_sec(st.bytes_per_sec))
                    .row("frame interval", format!("avg {:.1} ms · max {:.1} ms", st.interval_avg_ms, st.interval_max_ms))
                    .row("ReadSample", format!("avg {:.2} ms", st.read_avg_ms))
                    .row("copy", format!("avg {:.2} ms", st.copy_avg_ms))
                    .row("buffer", format!("{}{}", if st.buffer_2d { "IMF2DBuffer" } else { "IMFMediaBuffer" }, if st.bottom_up { " · bottom-up" } else { "" }))
                    .row("frames", slot.frame_count.load(std::sync::atomic::Ordering::Relaxed).to_string())
                    .row("last frame", age.map_or("–".into(), |a| format!("{} ms ago", a.as_millis())));
                s.sparkline = Some(st.fps_history);
            }
            out.push(s);
        }

        let mut s = Section::new("Totals");
        s.row("capturing", format!("{streaming} of {} selected", core.config.selected.len()))
            .row("camera throughput", mb_per_sec(total_bytes))
            .row("mode", match core.config.mode {
                SwitchMode::Hot => "instant (all cameras on)",
                SwitchMode::Cold => "on demand (active camera only)",
            })
            .row("paused (idle)", stats.paused.to_string());
        drop(core);
        out.push(s);

        out.push(compositor_section(&stats, output_size));
        out.push(self.shm_section(&stats, shm_connected, consumer_active));

        let p = self.sysinfo.stats;
        let mut s = Section::new("Process");
        s.row("CPU", format!("{:.1} % of all cores", p.cpu_percent))
            .row("working set", mib(p.working_set))
            .row("private bytes", mib(p.private_bytes))
            .row("uptime", uptime(p.uptime))
            .row("build", format!("v{} {}", env!("CARGO_PKG_VERSION"), if cfg!(debug_assertions) { "debug" } else { "release" }));
        out.push(s);
        out
    }

    fn shm_section(&self, stats: &CompositorStats, connected: bool, consumer_active: bool) -> Section {
        let mut s = Section::new("Shared memory / virtual camera");
        let vcam = match &*self.vcam.state.lock() {
            VcamState::Starting => "starting".to_string(),
            VcamState::Running => "registered".to_string(),
            VcamState::Failed(e) => format!("failed: {e}"),
        };
        s.row("virtual camera", vcam);
        s.row("shared memory", if connected { "connected" } else { "not created yet (no app opened the camera)" });
        let Some(shm) = &stats.shm else { return s };
        s.row("consumer", if consumer_active { "active" } else { "none" })
            .row("requested size", format!("{}x{}", shm.req_size.0, shm.req_size.1))
            .row("frames written", shm.frame_counter.to_string())
            .row("seqlock", shm.seq.to_string())
            .row("writer heartbeat", format!("{} ms ago", shm.writer_age_ms))
            .row("consumer heartbeat", format!("{} ms ago", shm.consumer_age_ms));
        match &shm.dll {
            None => {
                s.row("DLL stats", "unavailable (old DLL loaded: rebuild and re-run scripts\\register.ps1)");
            }
            Some(d) => {
                let format = match d.format {
                    CONSUMER_FORMAT_NV12 => "NV12",
                    CONSUMER_FORMAT_RGB32 => "RGB32",
                    _ => "–",
                };
                s.row("consumer format", format)
                    .row("delivered", format!("{:.1} fps ({} total)", d.delivered_per_sec, d.frames_total))
                    .row("repeated frames", format!("{:.1} /s", d.repeats_per_sec))
                    .row("fallback frames", format!("{:.1} /s (color bars / black)", d.fallbacks_per_sec))
                    .row("errors", d.errors_total.to_string())
                    .row("frame age at read", format!("{} ms", d.frame_age_ms))
                    .row("pending requests", d.queue_depth.to_string());
            }
        }
        s
    }
}

fn compositor_section(stats: &CompositorStats, output_size: (u32, u32)) -> Section {
    let mut s = Section::new("Compositor");
    let source = match stats.source {
        OutputSource::Active => "active camera",
        OutputSource::Fallback => "previous camera (switching)",
        OutputSource::Fade => "crossfade to active camera",
        OutputSource::Placeholder => "placeholder picture",
        OutputSource::Muted => "muted picture",
        OutputSource::Unwatched => "nothing (nobody watching)",
    };
    s.row("output", format!("{}x{} NV12 · target {} fps", output_size.0, output_size.1, shared::FPS))
        .row("showing", source)
        .row("loop", format!("{:.1} /s · {:.1} late /s", stats.loop_rate, stats.late_per_sec))
        .row("loop work", timing(&stats.work))
        .row("letterbox", timing(&stats.render))
        .row("BGRA → NV12", timing(&stats.nv12))
        .row("shm write", timing(&stats.shm_write))
        .row("shm frames", format!("{:.1} written /s · {:.1} unchanged /s", stats.shm_write.rate, stats.skipped_per_sec))
        .row("preview updates", format!("{:.1} /s", stats.preview_per_sec))
        .row("capture → shm latency", timing(&stats.latency));
    s
}

fn section_ui(ui: &mut egui::Ui, id: usize, section: &Section) {
    ui.add_space(4.0);
    ui.label(RichText::new(&section.title).strong());
    egui::Grid::new(("debug", id)).num_columns(2).striped(true).min_col_width(150.0).show(ui, |ui| {
        for (label, value) in &section.rows {
            ui.label(RichText::new(*label).color(Color32::GRAY));
            ui.label(RichText::new(value).monospace());
            ui.end_row();
        }
    });
    if let Some(history) = &section.sparkline {
        sparkline(ui, history);
    }
    ui.add_space(4.0);
    ui.separator();
}

/// fps over the last minute, scaled to its maximum (at least the output rate).
fn sparkline(ui: &mut egui::Ui, history: &History<60>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(400.0), 36.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if history.len() < 2 {
        return;
    }
    let max = history.iter().fold(shared::FPS as f32, f32::max);
    let step = rect.width() / 59.0;
    let x0 = rect.right() - step * (history.len() - 1) as f32;
    let points: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, v)| egui::pos2(x0 + i as f32 * step, rect.bottom() - 2.0 - (v / max) * (rect.height() - 4.0)))
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, Color32::from_rgb(80, 200, 120))));
    painter.text(rect.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, format!("fps · last 60 s · max {max:.0}"), egui::FontId::proportional(10.0), Color32::GRAY);
}

fn as_text(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections {
        let _ = writeln!(out, "## {}", s.title);
        for (label, value) in &s.rows {
            let _ = writeln!(out, "{label:<22} {value}");
        }
        out.push('\n');
    }
    out
}
