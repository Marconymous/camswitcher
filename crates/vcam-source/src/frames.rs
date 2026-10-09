//! Frame provider for the stream: latest app frame from shared memory, or a
//! fallback (color bars when the app isn't writing, black on size mismatch).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use shared::image::{color_bars_nv12, fill_black_nv12};
use shared::shm::{FrameShm, now_ms};

pub struct FrameProvider {
    shm: Option<FrameShm>,
    last_attempt: Option<Instant>,
    frame: Vec<u8>,
    bars_offset: u32,
}

impl FrameProvider {
    pub fn new() -> Self {
        Self { shm: None, last_attempt: None, frame: Vec::new(), bars_offset: 0 }
    }

    fn ensure_shm(&mut self) {
        if self.shm.is_some() || self.last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.last_attempt = Some(Instant::now());
        match FrameShm::create() {
            Ok(shm) => {
                log::info!("shared memory ready");
                self.shm = Some(shm);
            }
            Err(e) => log::error!("shared memory create failed: {e}"),
        }
    }

    /// Returns an NV12 frame of exactly `w` x `h`.
    pub fn next_frame(&mut self, w: u32, h: u32) -> &[u8] {
        self.ensure_shm();
        if let Some(shm) = &self.shm {
            let hdr = shm.header();
            hdr.req_width.store(w, Ordering::Relaxed);
            hdr.req_height.store(h, Ordering::Relaxed);
            hdr.consumer_tick.store(now_ms(), Ordering::Relaxed);
            if shm.writer_alive() {
                match shm.read_frame(&mut self.frame) {
                    Some((fw, fh, _)) if fw == w && fh == h => return &self.frame,
                    // Torn read: keep the previous frame if it has the right size.
                    None if self.frame.len() == (w * h * 3 / 2) as usize => return &self.frame,
                    // App still producing the old size; it follows req_* shortly.
                    _ => {
                        fill_black_nv12(&mut self.frame, w, h);
                        return &self.frame;
                    }
                }
            }
        }
        self.bars_offset = (self.bars_offset + 4) % w.max(1);
        color_bars_nv12(&mut self.frame, w, h, self.bars_offset);
        &self.frame
    }
}
