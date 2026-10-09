//! 30 fps output loop: picks the active camera's latest frame, letterboxes it
//! to the output size, writes NV12 to shared memory and a preview for the UI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use shared::image::{bgra_to_nv12, scale_bgra_letterbox, swap_rb};
use shared::shm::{FrameShm, MAX_HEIGHT, MAX_WIDTH};

use crate::capture::Frame;
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

/// Identity of what was last rendered: frame id (0 = none), mute, output size,
/// stills version.
type RenderKey = (u64, bool, u32, u32, u64);

/// Letterboxes `frame` into `dst` (`w` x `h` BGRA), or draws `still` without one.
fn render(frame: Option<&Frame>, still: &Still, dst: &mut Vec<u8>, w: u32, h: u32) {
    match frame {
        Some(f) => {
            dst.resize((w * h * 4) as usize, 0);
            scale_bgra_letterbox(&f.data, f.width, f.height, (f.width * 4) as usize, dst, w, h);
        }
        None => still.draw(dst, w, h),
    }
}

fn run(core: Arc<Mutex<Core>>, preview: Arc<Mutex<Preview>>, ui_heartbeat: Arc<AtomicU64>, repaint: impl Fn(), stop: Arc<AtomicBool>) {
    let period = Duration::from_nanos(1_000_000_000 / shared::FPS as u64);
    let mut next = Instant::now();
    let mut shm: Option<FrameShm> = None;
    let mut last_open_attempt: Option<Instant> = None;
    let mut out_bgra = Vec::new();
    let mut nv12 = Vec::new();
    let mut prev_bgra = vec![0u8; (PREVIEW_WIDTH * PREVIEW_HEIGHT * 4) as usize];
    let mut seq = 0u64;
    // Last frame written to shared memory / to the preview.
    let mut shm_key: Option<RenderKey> = None;
    let mut preview_key: Option<RenderKey> = None;
    let mut watched_at = Instant::now();

    while !stop.load(Ordering::Relaxed) {
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

        let sources = {
            let mut c = core.lock();
            c.set_idle(watched_at.elapsed() >= IDLE_DELAY);
            c.tick();
            c.output_sources()
        };
        let frame = if sources.muted || !(consumer_active || preview_visible) {
            None
        } else {
            let pick = |s: &Option<Arc<crate::capture::Slot>>| s.as_ref().and_then(|s| s.frame.lock().clone());
            pick(&sources.active).or_else(|| pick(&sources.fallback))
        };
        let still = if sources.muted { &sources.muted_still } else { &sources.placeholder };
        let key: RenderKey = (frame.as_ref().map_or(0, |f| f.id), sources.muted, w, h, sources.stills_version);

        let mut out_ready = false;
        if consumer_active && shm_key != Some(key) {
            render(frame.as_deref(), still, &mut out_bgra, w, h);
            out_ready = true;
            if let Some(s) = &shm {
                bgra_to_nv12(&out_bgra, w, h, &mut nv12);
                s.write_frame(w, h, &nv12);
            }
            shm_key = Some(key);
        } else if let Some(s) = &shm {
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
                render(frame.as_deref(), still, &mut out_bgra, w, h);
                scale_bgra_letterbox(&out_bgra, w, h, (w * 4) as usize, &mut prev_bgra, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            } else {
                // No consumer: output is the default 16:9 size, same as the preview,
                // so scale the camera frame straight to preview size.
                render(frame.as_deref(), still, &mut prev_bgra, PREVIEW_WIDTH, PREVIEW_HEIGHT);
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
            repaint_needed = true;
        } else if !preview_visible {
            preview_key = None;
        }

        {
            let mut p = preview.lock();
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
            next = now;
        } else {
            std::thread::sleep(next - now);
        }
    }
}
