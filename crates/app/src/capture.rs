//! Per-camera capture thread: MF source reader converting to RGB32 (BGRA).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use shared::stats::{History, Window};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{GUID, HSTRING, Interface, Result};

/// Source of [`Frame::id`]; unique across all captures.
static NEXT_FRAME_ID: AtomicU64 = AtomicU64::new(1);

/// One BGRA frame, top-down, tightly packed.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Unique per captured frame (never 0), lets the compositor skip repeats.
    pub id: u64,
    /// When the frame arrived from the camera.
    pub captured: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CamStatus {
    Starting,
    Streaming { width: u32, height: u32, fps: f32 },
    Failed(String),
}

/// Per-camera numbers for the debug view, refreshed once per second.
#[derive(Clone, Debug, Default)]
pub struct CamStats {
    /// Native format the camera delivers, e.g. `MJPG 1920x1080 @ 30 fps`.
    pub native: String,
    pub fps: f32,
    /// Decoded BGRA bytes per second.
    pub bytes_per_sec: f64,
    pub interval_avg_ms: f32,
    pub interval_max_ms: f32,
    /// Time blocked in `ReadSample` (includes waiting for the camera).
    pub read_avg_ms: f32,
    /// Time copying the decoded buffer out.
    pub copy_avg_ms: f32,
    pub buffer_2d: bool,
    pub bottom_up: bool,
    pub fps_history: History<60>,
}

pub struct Slot {
    pub frame: Mutex<Option<Arc<Frame>>>,
    pub status: Mutex<CamStatus>,
    pub frame_count: AtomicU64,
    pub stats: Mutex<CamStats>,
}

impl Slot {
    pub fn has_frame(&self) -> bool {
        self.frame.lock().is_some()
    }
}

/// Running capture; stops when dropped (the thread exits after its current read).
pub struct Capture {
    pub slot: Arc<Slot>,
    stop: Arc<AtomicBool>,
}

impl Capture {
    pub fn start(link: String, name: String) -> Self {
        let slot = Arc::new(Slot {
            frame: Mutex::new(None),
            status: Mutex::new(CamStatus::Starting),
            frame_count: AtomicU64::new(0),
            stats: Mutex::new(CamStats::default()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (slot, stop) = (slot.clone(), stop.clone());
            std::thread::Builder::new()
                .name(format!("capture {name}"))
                .spawn(move || {
                    unsafe {
                        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    }
                    log::info!("capture start: {name}");
                    if let Err(e) = run(&link, &slot, &stop) {
                        log::warn!("capture {name} failed: {e}");
                        *slot.status.lock() = CamStatus::Failed(e.message().to_string());
                    }
                    log::info!("capture end: {name}");
                    unsafe { CoUninitialize() };
                })
                .expect("spawn capture thread");
        }
        Self { slot, stop }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// Readable name of a video subtype; MF video subtypes carry a FourCC in `data1`.
fn subtype_name(subtype: &GUID) -> String {
    match subtype.data1 {
        21 => "RGB24".into(),
        22 => "RGB32".into(),
        v => {
            let bytes = v.to_le_bytes();
            if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                String::from_utf8_lossy(&bytes).trim_end().to_string()
            } else {
                format!("{subtype:?}")
            }
        }
    }
}

/// e.g. `MJPG 1920x1080 @ 30 fps`.
fn describe_type(t: &IMFMediaType) -> String {
    unsafe {
        let subtype = t.GetGUID(&MF_MT_SUBTYPE).map(|g| subtype_name(&g)).unwrap_or_else(|_| "?".into());
        let (w, h) = unpack(t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0));
        let (num, den) = unpack(t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0));
        let fps = if den == 0 { 0.0 } else { num as f32 / den as f32 };
        format!("{subtype} {w}x{h} @ {fps:.4} fps").replace(".0000 fps", " fps")
    }
}

/// Picks the native type to capture in: >= 29 fps first, then the largest
/// frame up to 1920x1080, then uncompressed over MJPG.
fn choose_native_type(reader: &IMFSourceReader) -> Result<IMFMediaType> {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let mut best: Option<((bool, bool, u64, bool), IMFMediaType)> = None;
    let mut i = 0;
    while let Ok(t) = unsafe { reader.GetNativeMediaType(stream, i) } {
        i += 1;
        let Ok(size) = (unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }) else { continue };
        let (w, h) = unpack(size);
        let (num, den) = unpack(unsafe { t.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0));
        let fps = if den == 0 { 0.0 } else { num as f32 / den as f32 };
        let subtype = unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or(GUID::zeroed());
        let fits = w <= 1920 && h <= 1080;
        let score = (fits, fps >= 29.0, if fits { w as u64 * h as u64 } else { 0 }, subtype != MFVideoFormat_MJPG);
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, t));
        }
    }
    best.map(|(_, t)| t).ok_or_else(|| windows::core::Error::new(MF_E_INVALIDMEDIATYPE, "no usable video format"))
}

fn run(link: &str, slot: &Slot, stop: &AtomicBool) -> Result<()> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.unwrap();
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        attrs.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &HSTRING::from(link))?;
        let source = MFCreateDeviceSource(&attrs)?;
        let result = read_loop(&source, slot, stop);
        let _ = source.Shutdown();
        result
    }
}

fn read_loop(source: &IMFMediaSource, slot: &Slot, stop: &AtomicBool) -> Result<()> {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    unsafe {
        let mut rattrs = None;
        MFCreateAttributes(&mut rattrs, 1)?;
        let rattrs = rattrs.unwrap();
        rattrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromMediaSource(source, &rattrs)?;

        let native = choose_native_type(&reader)?;
        reader.SetCurrentMediaType(stream, None, &native)?;
        let (w, h) = unpack(native.GetUINT64(&MF_MT_FRAME_SIZE)?);
        slot.stats.lock().native = describe_type(&native);

        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, native.GetUINT64(&MF_MT_FRAME_SIZE)?)?;
        out.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
        reader.SetCurrentMediaType(stream, None, &out)?;
        let current = reader.GetCurrentMediaType(stream)?;
        // Stride can come back negative (bottom-up) if the processor ignored ours.
        let default_stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or((w * 4) as i32);

        let mut intervals = Window::default();
        let mut reads = Window::default();
        let mut copies = Window::default();
        let mut last_frame: Option<Instant> = None;
        let mut spare: Option<Vec<u8>> = None;
        while !stop.load(Ordering::Relaxed) {
            let mut flags = 0u32;
            let mut sample = None;
            let read_start = Instant::now();
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            reads.add(ms_since(read_start));
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(windows::core::Error::new(MF_E_HW_MFT_FAILED_START_STREAMING, "stream error"));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Err(windows::core::Error::new(MF_E_END_OF_STREAM, "device stopped"));
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.GetBufferByIndex(0)?;
            let copy_start = Instant::now();

            let mut data = spare.take().unwrap_or_default();
            data.resize((w * h * 4) as usize, 0);
            let row = (w * 4) as usize;

            let b2 = buffer.cast::<IMF2DBuffer>();
            let buffer_2d = b2.is_ok();
            if let Ok(b2) = b2 {
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                b2.Lock2D(&mut scan0, &mut pitch)?;
                for y in 0..h as usize {
                    let src = scan0.offset(y as isize * pitch as isize);
                    std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(y * row), row);
                }
                b2.Unlock2D()?;
            } else {
                let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut ptr, None, Some(&mut len))?;
                if len as usize >= row * h as usize {
                    for y in 0..h as usize {
                        let src_row = if default_stride < 0 { h as usize - 1 - y } else { y };
                        std::ptr::copy_nonoverlapping(ptr.add(src_row * row), data.as_mut_ptr().add(y * row), row);
                    }
                }
                buffer.Unlock()?;
            }

            copies.add(ms_since(copy_start));

            let now = Instant::now();
            let frame = Frame { width: w, height: h, data, id: NEXT_FRAME_ID.fetch_add(1, Ordering::Relaxed), captured: now };
            let old = slot.frame.lock().replace(Arc::new(frame));
            // Reuse the replaced frame's allocation when nobody else holds it.
            spare = old.and_then(|f| Arc::try_unwrap(f).ok()).map(|f| f.data);
            slot.frame_count.fetch_add(1, Ordering::Relaxed);

            if let Some(prev) = last_frame.replace(now) {
                intervals.add((now - prev).as_secs_f32() * 1000.0);
            } else {
                intervals.tick();
            }
            if let Some(iv) = intervals.take_if_elapsed(now) {
                let (rd, cp) = (reads.take_if_elapsed(now).unwrap_or_default(), copies.take_if_elapsed(now).unwrap_or_default());
                *slot.status.lock() = CamStatus::Streaming { width: w, height: h, fps: iv.rate };
                let mut st = slot.stats.lock();
                st.fps = iv.rate;
                st.bytes_per_sec = iv.rate as f64 * (w * h * 4) as f64;
                st.interval_avg_ms = if iv.rate > 0.0 { 1000.0 / iv.rate } else { 0.0 };
                st.interval_max_ms = iv.max;
                st.read_avg_ms = rd.avg;
                st.copy_avg_ms = cp.avg;
                st.buffer_2d = buffer_2d;
                st.bottom_up = default_stride < 0;
                st.fps_history.push(iv.rate);
            } else if matches!(*slot.status.lock(), CamStatus::Starting) {
                *slot.status.lock() = CamStatus::Streaming { width: w, height: h, fps: 0.0 };
            }
        }
        Ok(())
    }
}

fn ms_since(t: Instant) -> f32 {
    t.elapsed().as_secs_f32() * 1000.0
}
