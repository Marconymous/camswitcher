//! The single video stream of the virtual camera. `RequestSample` tokens are
//! queued and served by a worker thread paced at [`shared::FPS`].

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_SET_NOT_FOUND, S_OK};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

use crate::frames::FrameProvider;

pub const STREAM_ID: u32 = 0;
const FRAME_DURATION_100NS: i64 = 10_000_000 / shared::FPS as i64;

#[derive(Clone)]
struct Format {
    subtype: GUID,
    width: u32,
    height: u32,
}

struct State {
    running: bool,
    shutdown: bool,
    stream_state: MF_STREAM_STATE,
    tokens: VecDeque<Option<IUnknown>>,
    format: Option<Format>,
    /// Allocator handed to us by Frame Server via `IMFSampleAllocatorControl`.
    provided_allocator: Option<IMFVideoSampleAllocatorEx>,
    /// Allocator in use while running (provided one, or our own).
    allocator: Option<IMFVideoSampleAllocatorEx>,
}

struct Shared {
    queue: IMFMediaEventQueue,
    state: Mutex<State>,
    cv: Condvar,
}

// MF objects used here (event queue, allocators, samples) are free-threaded.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

#[implement(IMFMediaStream2, IKsControl)]
pub struct MediaStream {
    shared: Arc<Shared>,
    attributes: IMFAttributes,
    descriptor: IMFStreamDescriptor,
    source: Mutex<Option<IMFMediaSource>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn video_type(subtype: &GUID, w: u32, h: u32) -> Result<IMFMediaType> {
    let nv12 = *subtype == MFVideoFormat_NV12;
    let stride = if nv12 { w } else { w * 4 };
    let size = if nv12 { w * h * 3 / 2 } else { w * h * 4 };
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(shared::FPS, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, size)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, size * 8 * shared::FPS)?;
        Ok(t)
    }
}

fn format_of(t: &IMFMediaType) -> Result<Format> {
    unsafe {
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
        Ok(Format { subtype: t.GetGUID(&MF_MT_SUBTYPE)?, width: (size >> 32) as u32, height: size as u32 })
    }
}

fn ks_not_found() -> windows::core::Error {
    HRESULT::from_win32(ERROR_SET_NOT_FOUND.0).into()
}

impl MediaStream {
    pub fn create() -> Result<windows::core::ComObject<MediaStream>> {
        unsafe {
            let mut types = Vec::new();
            for subtype in [MFVideoFormat_NV12, MFVideoFormat_RGB32] {
                for (w, h) in shared::OUTPUT_SIZES {
                    types.push(Some(video_type(&subtype, w, h)?));
                }
            }

            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.unwrap();
            attributes.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, STREAM_ID)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;

            let descriptor = MFCreateStreamDescriptor(STREAM_ID, &types)?;
            descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(types[0].as_ref().unwrap())?;
            attributes.CopyAllItems(&descriptor)?;

            let shared = Arc::new(Shared {
                queue: MFCreateEventQueue()?,
                state: Mutex::new(State {
                    running: false,
                    shutdown: false,
                    stream_state: MF_STREAM_STATE_STOPPED,
                    tokens: VecDeque::new(),
                    format: None,
                    provided_allocator: None,
                    allocator: None,
                }),
                cv: Condvar::new(),
            });
            let worker = {
                let shared = shared.clone();
                std::thread::Builder::new().name("vcam-stream".into()).spawn(move || worker(shared)).ok()
            };

            Ok(windows::core::ComObject::new(MediaStream {
                shared,
                attributes,
                descriptor,
                source: Mutex::new(None),
                worker: Mutex::new(worker),
            }))
        }
    }

    pub fn set_source(&self, source: IMFMediaSource) {
        *self.source.lock().unwrap() = Some(source);
    }

    pub fn attributes(&self) -> IMFAttributes {
        self.attributes.clone()
    }

    pub fn descriptor(&self) -> IMFStreamDescriptor {
        self.descriptor.clone()
    }

    pub fn is_running(&self) -> bool {
        self.shared.state.lock().unwrap().running
    }

    pub fn set_allocator(&self, allocator: Option<IMFVideoSampleAllocatorEx>) {
        self.shared.state.lock().unwrap().provided_allocator = allocator;
    }

    /// Starts streaming with `media_type` and queues `MEStreamStarted`.
    pub fn start(&self, media_type: &IMFMediaType, time: i64) -> Result<()> {
        let format = format_of(media_type)?;
        let mut st = self.shared.state.lock().unwrap();
        let allocator = match &st.provided_allocator {
            Some(a) => a.clone(),
            None => unsafe {
                let mut raw = std::ptr::null_mut();
                MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)?;
                IMFVideoSampleAllocatorEx::from_raw(raw)
            },
        };
        unsafe { allocator.InitializeSampleAllocator(8, media_type)? };
        log::info!(
            "stream start {}x{} {} (provided allocator: {})",
            format.width,
            format.height,
            if format.subtype == MFVideoFormat_NV12 { "NV12" } else { "RGB32" },
            st.provided_allocator.is_some()
        );
        st.allocator = Some(allocator);
        st.format = Some(format);
        st.running = true;
        st.stream_state = MF_STREAM_STATE_RUNNING;
        drop(st);
        self.shared.cv.notify_all();
        unsafe {
            self.shared.queue.QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, &PROPVARIANT::from(time))
        }
    }

    /// Stops streaming and queues `MEStreamStopped`.
    pub fn stop(&self) -> Result<()> {
        let mut st = self.shared.state.lock().unwrap();
        let was_running = st.running;
        st.running = false;
        st.stream_state = MF_STREAM_STATE_STOPPED;
        st.tokens.clear();
        if let Some(a) = st.allocator.take() {
            unsafe {
                let _ = a.UninitializeSampleAllocator();
            }
        }
        drop(st);
        if was_running {
            log::info!("stream stop");
            unsafe {
                self.shared.queue.QueueEventParamVar(MEStreamStopped.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())?;
            }
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        let _ = self.stop();
        {
            let mut st = self.shared.state.lock().unwrap();
            st.shutdown = true;
            st.provided_allocator = None;
        }
        self.shared.cv.notify_all();
        if let Some(h) = self.worker.lock().unwrap().take() {
            let _ = h.join();
        }
        unsafe {
            let _ = self.shared.queue.Shutdown();
        }
        self.source.lock().unwrap().take();
    }

    fn check_shutdown(&self) -> Result<()> {
        if self.shared.state.lock().unwrap().shutdown { Err(MF_E_SHUTDOWN.into()) } else { Ok(()) }
    }
}

fn worker(shared: Arc<Shared>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let period = Duration::from_nanos(1_000_000_000 / shared::FPS as u64);
    let mut frames = FrameProvider::new();
    let mut next = Instant::now();
    let mut errors = 0u32;
    loop {
        let (token, format, allocator) = {
            let mut st = shared.state.lock().unwrap();
            loop {
                if st.shutdown {
                    unsafe { CoUninitialize() };
                    return;
                }
                if st.running && !st.tokens.is_empty() {
                    break;
                }
                st = shared.cv.wait(st).unwrap();
            }
            let token = st.tokens.pop_front().unwrap();
            (token, st.format.clone().unwrap(), st.allocator.clone().unwrap())
        };

        let now = Instant::now();
        if next + period < now {
            next = now;
        }
        if next > now {
            std::thread::sleep(next - now);
        }
        next += period;

        let result = produce(&mut frames, &format, &allocator, token.as_ref());
        let st = shared.state.lock().unwrap();
        if !st.running || st.shutdown {
            continue;
        }
        match result {
            Ok(sample) => unsafe {
                let unk: IUnknown = sample.cast().unwrap();
                let _ = shared.queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &unk);
            },
            Err(e) => {
                errors += 1;
                if errors <= 10 {
                    log::error!("sample production failed: {e}");
                }
            }
        }
    }
}

fn produce(
    frames: &mut FrameProvider,
    format: &Format,
    allocator: &IMFVideoSampleAllocatorEx,
    token: Option<&IUnknown>,
) -> Result<IMFSample> {
    let (w, h) = (format.width, format.height);
    let nv12_out = format.subtype == MFVideoFormat_NV12;
    let frame = frames.next_frame(w, h);
    unsafe {
        let sample = allocator.AllocateSample()?;
        let buffer = sample.GetBufferByIndex(0)?;
        let frame_len = if nv12_out { w * h * 3 / 2 } else { w * h * 4 };

        if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
            let (mut scan0, mut pitch, mut start, mut len) = (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
            b2.Lock2DSize(MF2DBuffer_LockFlags_Write, &mut scan0, &mut pitch, &mut start, &mut len)?;
            let rows = if nv12_out { h * 3 / 2 } else { h };
            let needed = pitch.unsigned_abs() as usize * rows as usize;
            if (len as usize) >= needed {
                write_frame(frame, w, h, nv12_out, scan0, pitch as isize);
            }
            b2.Unlock2D()?;
        } else {
            let (mut ptr, mut max) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, Some(&mut max), None)?;
            if max >= frame_len {
                let stride = if nv12_out { w } else { w * 4 } as isize;
                write_frame(frame, w, h, nv12_out, ptr, stride);
            }
            buffer.Unlock()?;
        }
        let _ = buffer.SetCurrentLength(frame_len);

        sample.SetSampleTime(MFGetSystemTime())?;
        sample.SetSampleDuration(FRAME_DURATION_100NS)?;
        if let Some(token) = token {
            sample.SetUnknown(&MFSampleExtension_Token, token)?;
        }
        Ok(sample)
    }
}

/// Writes an NV12 `frame` into a locked buffer whose first row is `scan0` and
/// rows are `pitch` bytes apart (negative for bottom-up RGB).
unsafe fn write_frame(frame: &[u8], w: u32, h: u32, nv12_out: bool, scan0: *mut u8, pitch: isize) {
    let (wu, hu) = (w as usize, h as usize);
    unsafe {
        if nv12_out {
            for y in 0..hu {
                std::ptr::copy_nonoverlapping(frame.as_ptr().add(y * wu), scan0.offset(y as isize * pitch), wu);
            }
            let uv = scan0.offset(hu as isize * pitch);
            for y in 0..hu / 2 {
                std::ptr::copy_nonoverlapping(frame.as_ptr().add(wu * hu + y * wu), uv.offset(y as isize * pitch), wu);
            }
        } else {
            for y in 0..h {
                let row = std::slice::from_raw_parts_mut(scan0.offset(y as isize * pitch), wu * 4);
                shared::image::nv12_to_bgra_row(frame, w, h, y, row);
            }
        }
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        // Not under the state lock: a blocking GetEvent must not stall the stream.
        unsafe { self.shared.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        self.check_shutdown()?;
        unsafe { self.shared.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.shared.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(&self, met: u32, ext: *const GUID, hr: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        self.check_shutdown()?;
        unsafe { self.shared.queue.QueueEventParamVar(met, ext, hr, value) }
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.source.lock().unwrap().clone().ok_or_else(|| MF_E_SHUTDOWN.into())
    }
    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        self.check_shutdown()?;
        Ok(self.descriptor.clone())
    }
    fn RequestSample(&self, token: Ref<IUnknown>) -> Result<()> {
        let mut st = self.shared.state.lock().unwrap();
        if st.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        if !st.running {
            return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
        }
        st.tokens.push_back(token.cloned());
        drop(st);
        self.shared.cv.notify_all();
        Ok(())
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        self.check_shutdown()?;
        log::info!("SetStreamState({})", value.0);
        self.shared.state.lock().unwrap().stream_state = value;
        Ok(())
    }
    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        self.check_shutdown()?;
        Ok(self.shared.state.lock().unwrap().stream_state)
    }
}

impl IKsControl_Impl for MediaStream_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_not_found())
    }
    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_not_found())
    }
    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut core::ffi::c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(ks_not_found())
    }
}

pub(crate) fn ks_error() -> windows::core::Error {
    ks_not_found()
}
