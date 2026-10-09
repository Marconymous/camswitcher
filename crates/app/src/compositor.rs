//! 30 fps output loop: picks the active camera's latest frame, letterboxes it
//! to the output size, writes NV12 to shared memory and a preview for the UI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use shared::image::{bgra_to_nv12, blend_bgra, cover_rect, fill_black_bgra, fit_rect, rotate_bgra, scale_bgra_letterbox, scale_bgra_rect, swap_rb, zoom_rect};
use shared::shm::{FrameShm, MAX_HEIGHT, MAX_WIDTH, ShmHeader};
use shared::stats::{Snapshot, Window};

use crate::capture::Frame;
use crate::config::{Fit, Rotation, Transform};
use crate::core::Core;
use crate::stills::Still;

pub const PREVIEW_WIDTH: u32 = 640;
pub const PREVIEW_HEIGHT: u32 = 360;

/// Latest preview image (RGBA) plus output status, read by the UI.
pub struct Preview {
    pub rgba: Vec<u8>,
    pub seq: u64,
    pub output_size: (u32, u32),
    pub shm_connected: bool,
    pub consumer_active: bool,
    /// Refreshed once per second.
    pub stats: CompositorStats,
}

/// What the output currently shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputSource {
    #[default]
    Placeholder,
    Active,
    /// Cold mode: previous camera while the new one starts.
    Fallback,
    /// Crossfade from the previous camera to the active one.
    Fade,
    Muted,
    /// Nobody watches; no camera frame is used.
    Unwatched,
}

/// Compositor loop numbers for the debug view. Timings in ms, rates per second.
#[derive(Clone, Debug, Default)]
pub struct CompositorStats {
    pub loop_rate: f32,
    /// Iterations that started behind schedule.
    pub late_per_sec: f32,
    pub work: Snapshot,
    /// Letterboxing into the output size.
    pub render: Snapshot,
    pub nv12: Snapshot,
    pub shm_write: Snapshot,
    /// Unchanged frames not rewritten to shared memory.
    pub skipped_per_sec: f32,
    pub preview_per_sec: f32,
    /// Camera capture to shared-memory write.
    pub latency: Snapshot,
    pub source: OutputSource,
    pub paused: bool,
    pub shm: Option<ShmSnapshot>,
}

/// Shared-memory header contents.
#[derive(Clone, Debug, Default)]
pub struct ShmSnapshot {
    pub seq: u64,
    pub frame_counter: u64,
    pub req_size: (u32, u32),
    pub writer_age_ms: u64,
    pub consumer_age_ms: u64,
    /// `None` when the loaded DLL predates the statistics fields.
    pub dll: Option<DllStats>,
}

/// Virtual camera DLL counters, rates derived per second.
#[derive(Clone, Debug, Default)]
pub struct DllStats {
    /// 0 = none, 1 = NV12, 2 = RGB32.
    pub format: u32,
    pub delivered_per_sec: f32,
    pub repeats_per_sec: f32,
    pub fallbacks_per_sec: f32,
    pub frames_total: u64,
    pub errors_total: u64,
    pub frame_age_ms: u32,
    pub queue_depth: u32,
}

/// Cumulative DLL counters at the previous snapshot.
#[derive(Clone, Copy, Default)]
struct DllCounters {
    frames: u64,
    repeats: u64,
    fallbacks: u64,
}

fn shm_snapshot(hdr: &ShmHeader, prev: &mut Option<(Instant, DllCounters)>, now: Instant) -> ShmSnapshot {
    let ms = shared::shm::now_ms();
    let dll = (hdr.dll_stats_version.load(Ordering::Relaxed) != 0).then(|| {
        let cur = DllCounters {
            frames: hdr.consumer_frames.load(Ordering::Relaxed),
            repeats: hdr.consumer_repeats.load(Ordering::Relaxed),
            fallbacks: hdr.consumer_fallbacks.load(Ordering::Relaxed),
        };
        let (per_sec, old) = match prev.replace((now, cur)) {
            Some((t, old)) => (1.0 / now.saturating_duration_since(t).as_secs_f32().max(0.001), old),
            None => (0.0, cur),
        };
        DllStats {
            format: hdr.consumer_format.load(Ordering::Relaxed),
            delivered_per_sec: cur.frames.saturating_sub(old.frames) as f32 * per_sec,
            repeats_per_sec: cur.repeats.saturating_sub(old.repeats) as f32 * per_sec,
            fallbacks_per_sec: cur.fallbacks.saturating_sub(old.fallbacks) as f32 * per_sec,
            frames_total: cur.frames,
            errors_total: hdr.consumer_errors.load(Ordering::Relaxed),
            frame_age_ms: hdr.consumer_frame_age_ms.load(Ordering::Relaxed),
            queue_depth: hdr.consumer_queue_depth.load(Ordering::Relaxed),
        }
    });
    ShmSnapshot {
        seq: hdr.seq.load(Ordering::Relaxed),
        frame_counter: hdr.frame_counter.load(Ordering::Relaxed),
        req_size: (hdr.req_width.load(Ordering::Relaxed), hdr.req_height.load(Ordering::Relaxed)),
        writer_age_ms: ms.saturating_sub(hdr.writer_tick.load(Ordering::Relaxed)),
        consumer_age_ms: ms.saturating_sub(hdr.consumer_tick.load(Ordering::Relaxed)),
        dll,
    }
}

/// Per-second accumulators of the compositor loop.
struct Meters {
    loops: Window,
    late: Window,
    work: Window,
    render: Window,
    nv12: Window,
    shm_write: Window,
    skipped: Window,
    preview: Window,
    latency: Window,
}

impl Meters {
    fn new() -> Self {
        let t = Instant::now();
        Self {
            loops: Window::new(t),
            late: Window::new(t),
            work: Window::new(t),
            render: Window::new(t),
            nv12: Window::new(t),
            shm_write: Window::new(t),
            skipped: Window::new(t),
            preview: Window::new(t),
            latency: Window::new(t),
        }
    }

    /// Fills the timing fields of `stats` once a window has passed.
    fn take(&mut self, now: Instant, stats: &mut CompositorStats) -> bool {
        let Some(loops) = self.loops.take_if_elapsed(now) else { return false };
        let take = |w: &mut Window| w.take_if_elapsed(now).unwrap_or_default();
        stats.loop_rate = loops.rate;
        stats.late_per_sec = take(&mut self.late).rate;
        stats.work = take(&mut self.work);
        stats.render = take(&mut self.render);
        stats.nv12 = take(&mut self.nv12);
        stats.shm_write = take(&mut self.shm_write);
        stats.skipped_per_sec = take(&mut self.skipped).rate;
        stats.preview_per_sec = take(&mut self.preview).rate;
        stats.latency = take(&mut self.latency);
        true
    }
}

fn ms_since(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}

/// The preview counts as hidden when the UI hasn't drawn for this long
/// (it repaints at least every 500 ms while visible).
const PREVIEW_TIMEOUT_MS: u64 = 1000;
/// Nobody must watch for this long before cameras are paused (avoids flapping).
const IDLE_DELAY: Duration = Duration::from_secs(3);

pub fn spawn(
    core: Arc<Mutex<Core>>,
    preview: Arc<Mutex<Preview>>,
    ui_heartbeat: Arc<AtomicU64>,
    repaint: impl Fn() + Send + 'static,
    stop: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || run(core, preview, ui_heartbeat, repaint, stop))
        .expect("spawn compositor");
}

/// Identity of what was last rendered.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RenderKey {
    /// Frame id, 0 = none.
    frame: u64,
    /// Fading-out frame id and progress in 1/255.
    fade: Option<(u64, u8)>,
    muted: bool,
    size: (u32, u32),
    /// Stills / transforms version.
    version: u64,
}

/// What to draw: a camera frame with its adjustments, optionally crossfading
/// from the previous camera, or a still picture.
struct Picture<'a> {
    frame: Option<(&'a Frame, &'a Transform)>,
    /// Previous camera and the new one's weight (0..1).
    fade_from: Option<(&'a Frame, &'a Transform, f32)>,
    still: &'a Still,
}

/// Reusable buffers for [`render`].
#[derive(Default)]
struct Scratch {
    rotated: Vec<u8>,
    blend: Vec<u8>,
}

/// Draws `pic` into `dst` (`w` x `h` BGRA).
fn render(pic: &Picture, scratch: &mut Scratch, dst: &mut Vec<u8>, w: u32, h: u32) {
    match (pic.frame, pic.fade_from) {
        (Some((f, t)), Some((from, from_t, a))) => {
            render_frame(from, from_t, &mut scratch.rotated, dst, w, h);
            render_frame(f, t, &mut scratch.rotated, &mut scratch.blend, w, h);
            blend_bgra(dst, &scratch.blend, a);
        }
        (Some((f, t)), None) => render_frame(f, t, &mut scratch.rotated, dst, w, h),
        (None, _) => pic.still.draw(dst, w, h),
    }
}

/// Rotate, zoom/pan, fit or fill, mirror.
fn render_frame(f: &Frame, t: &Transform, scratch: &mut Vec<u8>, dst: &mut Vec<u8>, w: u32, h: u32) {
    dst.resize((w * h * 4) as usize, 0);
    let (src, sw, sh) = if t.rotate == Rotation::R0 {
        (&f.data[..], f.width, f.height)
    } else {
        let (rw, rh) = rotate_bgra(&f.data, f.width, f.height, (f.width * 4) as usize, t.rotate.quarter_turns(), scratch);
        (&scratch[..], rw, rh)
    };
    let mut src_rect = zoom_rect(sw, sh, t.zoom, t.center);
    let dst_rect = match t.fit {
        Fit::Letterbox => fit_rect(src_rect.2, src_rect.3, w, h),
        Fit::Fill => {
            let (x, y, cw, ch) = cover_rect(src_rect.2, src_rect.3, w, h);
            src_rect = (src_rect.0 + x, src_rect.1 + y, cw, ch);
            (0, 0, w, h)
        }
    };
    if dst_rect.2 != w || dst_rect.3 != h {
        fill_black_bgra(dst);
    }
    scale_bgra_rect(src, (sw * 4) as usize, src_rect, dst, w, dst_rect, t.mirror);
}

fn run(core: Arc<Mutex<Core>>, preview: Arc<Mutex<Preview>>, ui_heartbeat: Arc<AtomicU64>, repaint: impl Fn(), stop: Arc<AtomicBool>) {
    let period = Duration::from_nanos(1_000_000_000 / shared::FPS as u64);
    let mut next = Instant::now();
    let mut shm: Option<FrameShm> = None;
    let mut last_open_attempt: Option<Instant> = None;
    let mut out_bgra = Vec::new();
    let mut scratch = Scratch::default();
    let mut nv12 = Vec::new();
    let mut prev_bgra = vec![0u8; (PREVIEW_WIDTH * PREVIEW_HEIGHT * 4) as usize];
    let mut seq = 0u64;
    // Last frame written to shared memory / to the preview.
    let mut shm_key: Option<RenderKey> = None;
    let mut preview_key: Option<RenderKey> = None;
    let mut watched_at = Instant::now();
    let mut meters = Meters::new();
    let mut stats = CompositorStats::default();
    let mut dll_prev = None;

    while !stop.load(Ordering::Relaxed) {
        let loop_start = Instant::now();
        meters.loops.tick();
        if shm.is_none() && last_open_attempt.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            last_open_attempt = Some(Instant::now());
            if let Ok(s) = FrameShm::open() {
                log::info!("connected to virtual camera shared memory");
                shm = Some(s);
            }
        }

        let (w, h) = shm
            .as_ref()
            .filter(|s| s.is_initialized())
            .map(|s| {
                let hdr = s.header();
                (hdr.req_width.load(Ordering::Relaxed), hdr.req_height.load(Ordering::Relaxed))
            })
            .filter(|&(w, h)| w > 0 && h > 0 && w <= MAX_WIDTH && h <= MAX_HEIGHT && w % 2 == 0 && h % 2 == 0)
            .unwrap_or((shared::DEFAULT_WIDTH, shared::DEFAULT_HEIGHT));

        let consumer_active = shm.as_ref().is_some_and(|s| s.consumer_alive());
        let preview_visible =
            shared::shm::now_ms().saturating_sub(ui_heartbeat.load(Ordering::Relaxed)) < PREVIEW_TIMEOUT_MS;
        if consumer_active || preview_visible {
            watched_at = Instant::now();
        }

        let (sources, paused) = {
            let mut c = core.lock();
            c.set_idle(watched_at.elapsed() >= IDLE_DELAY);
            c.tick();
            (c.output_sources(), c.paused())
        };
        let pick = |s: &Option<(Arc<crate::capture::Slot>, Transform)>| {
            s.as_ref().and_then(|(s, t)| Some((s.frame.lock().clone()?, *t)))
        };
        let (frame, fade_from, source) = if sources.muted {
            (None, None, OutputSource::Muted)
        } else if !(consumer_active || preview_visible) {
            (None, None, OutputSource::Unwatched)
        } else {
            match pick(&sources.active) {
                Some(f) => {
                    let fade = sources.fade_from.as_ref().and_then(|(s, a)| Some((pick(&Some(s.clone()))?, *a)));
                    match fade {
                        Some(fade) => (Some(f), Some(fade), OutputSource::Fade),
                        None => (Some(f), None, OutputSource::Active),
                    }
                }
                // No picture from the new camera yet: keep the previous one.
                None => match pick(&sources.fallback).or_else(|| pick(&sources.fade_from.as_ref().map(|(s, _)| s.clone()))) {
                    Some(f) => (Some(f), None, OutputSource::Fallback),
                    None => (None, None, OutputSource::Placeholder),
                },
            }
        };
        let still = if sources.muted { &sources.muted_still } else { &sources.placeholder };
        let key = RenderKey {
            frame: frame.as_ref().map_or(0, |(f, _)| f.id),
            fade: fade_from.as_ref().map(|((f, _), a)| (f.id, (a * 255.0) as u8)),
            muted: sources.muted,
            size: (w, h),
            version: sources.render_version,
        };
        let pic = Picture {
            frame: frame.as_ref().map(|(f, t)| (&**f, t)),
            fade_from: fade_from.as_ref().map(|((f, t), a)| (&**f, t, *a)),
            still,
        };

        let mut out_ready = false;
        if consumer_active && shm_key != Some(key) {
            let t = Instant::now();
            render(&pic, &mut scratch, &mut out_bgra, w, h);
            meters.render.add(ms_since(t));
            out_ready = true;
            if let Some(s) = &shm {
                let t = Instant::now();
                bgra_to_nv12(&out_bgra, w, h, &mut nv12);
                meters.nv12.add(ms_since(t));
                let t = Instant::now();
                s.write_frame(w, h, &nv12);
                meters.shm_write.add(ms_since(t));
                if let Some((f, _)) = &frame {
                    meters.latency.add(ms_since(f.captured));
                }
            }
            shm_key = Some(key);
        } else if let Some(s) = &shm {
            if consumer_active {
                meters.skipped.tick();
            }
            // Unchanged frame or nobody consuming: the buffer still holds the
            // latest picture, just keep the heartbeat alive.
            s.header().writer_tick.store(shared::shm::now_ms(), Ordering::Relaxed);
        }
        if !consumer_active {
            shm_key = None;
        }

        let mut repaint_needed = false;
        if preview_visible && preview_key != Some(key) {
            if out_ready {
                scale_bgra_letterbox(&out_bgra, w, h, (w * 4) as usize, &mut prev_bgra, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            } else if consumer_active {
                // Shm is up to date but the preview isn't: show the output framing.
                render(&pic, &mut scratch, &mut out_bgra, w, h);
                scale_bgra_letterbox(&out_bgra, w, h, (w * 4) as usize, &mut prev_bgra, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            } else {
                // No consumer: output is the default 16:9 size, same as the preview,
                // so scale the camera frame straight to preview size.
                render(&pic, &mut scratch, &mut prev_bgra, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            }
            swap_rb(&mut prev_bgra);
            seq += 1;
            {
                let mut p = preview.lock();
                std::mem::swap(&mut p.rgba, &mut prev_bgra);
                p.seq = seq;
            }
            prev_bgra.resize((PREVIEW_WIDTH * PREVIEW_HEIGHT * 4) as usize, 0);
            preview_key = Some(key);
            meters.preview.tick();
            repaint_needed = true;
        } else if !preview_visible {
            preview_key = None;
        }

        meters.work.add(ms_since(loop_start));
        let now = Instant::now();
        let stats_ready = meters.take(now, &mut stats);
        if stats_ready {
            stats.source = source;
            stats.paused = paused;
            stats.shm = shm.as_ref().filter(|s| s.is_initialized()).map(|s| shm_snapshot(s.header(), &mut dll_prev, now));
        }

        {
            let mut p = preview.lock();
            if stats_ready {
                p.stats = stats.clone();
            }
            let status = ((w, h), shm.is_some(), consumer_active);
            repaint_needed |= (p.output_size, p.shm_connected, p.consumer_active) != status;
            (p.output_size, p.shm_connected, p.consumer_active) = status;
        }
        if preview_visible && repaint_needed {
            repaint();
        }

        let now = Instant::now();
        next += period;
        if next < now {
            meters.late.tick();
            next = now;
        } else {
            std::thread::sleep(next - now);
        }
    }
}
