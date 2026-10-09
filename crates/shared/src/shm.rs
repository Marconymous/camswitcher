//! Shared-memory frame transport between the app (writer) and the virtual
//! camera DLL (reader, hosted by the Frame Server service).
//!
//! Layout: [`ShmHeader`] padded to [`HEADER_SIZE`], followed by one NV12 frame
//! buffer of up to [`MAX_WIDTH`] x [`MAX_HEIGHT`]. Access is guarded by a
//! seqlock: the writer makes `seq` odd while copying, readers retry or skip
//! when they observe an odd or changed `seq`.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::core::HSTRING;

pub const SHM_NAME: &str = r"Global\CamSwitch_Frames_v1";
/// SYSTEM, LocalService (Frame Server) and Administrators: full access.
/// Interactive users: read + write (the app).
pub const SHM_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)";

pub const MAGIC: u32 = 0x4353_5743; // "CWSC"
pub const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 256;
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
pub const FRAME_CAPACITY: usize = (MAX_WIDTH * MAX_HEIGHT * 3 / 2) as usize;
pub const SHM_SIZE: usize = HEADER_SIZE + FRAME_CAPACITY;

/// Heartbeats older than this are considered stale.
pub const HEARTBEAT_TIMEOUT_MS: u64 = 1000;

#[repr(C)]
pub struct ShmHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    /// Output size negotiated by the consumer; written by the DLL, followed by the app.
    pub req_width: AtomicU32,
    pub req_height: AtomicU32,
    /// Seqlock counter; odd while a frame is being written.
    pub seq: AtomicU64,
    /// Size of the frame currently in the buffer.
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub frame_counter: AtomicU64,
    /// `GetTickCount64` of the last frame written by the app.
    pub writer_tick: AtomicU64,
    /// `GetTickCount64` of the last frame delivered by the DLL to a consumer.
    pub consumer_tick: AtomicU64,
    // DLL statistics for the app's debug view. Appended after the original
    // fields so older DLLs/apps keep working (they leave these at 0).
    /// Non-zero once a DLL that fills the fields below is running.
    pub dll_stats_version: AtomicU32,
    /// Format negotiated with the consumer: 0 = none, 1 = NV12, 2 = RGB32.
    pub consumer_format: AtomicU32,
    /// Samples delivered to the consumer.
    pub consumer_frames: AtomicU64,
    /// Deliveries that repeated the previous app frame.
    pub consumer_repeats: AtomicU64,
    /// Deliveries of color bars or black instead of an app frame.
    pub consumer_fallbacks: AtomicU64,
    /// Failed sample productions.
    pub consumer_errors: AtomicU64,
    /// Age of the app frame at the DLL's last read, in ms.
    pub consumer_frame_age_ms: AtomicU32,
    /// Pending `RequestSample` tokens.
    pub consumer_queue_depth: AtomicU32,
}

/// Value of [`ShmHeader::dll_stats_version`] written by this DLL version.
pub const DLL_STATS_VERSION: u32 = 1;
pub const CONSUMER_FORMAT_NV12: u32 = 1;
pub const CONSUMER_FORMAT_RGB32: u32 = 2;

const _: () = assert!(std::mem::size_of::<ShmHeader>() <= HEADER_SIZE);

pub fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

/// View over a mapped region (or any suitably sized memory, for tests).
pub struct FrameShm {
    base: NonNull<u8>,
    mapping: Option<(HANDLE, MEMORY_MAPPED_VIEW_ADDRESS)>,
}

// The region is only accessed through atomics and the seqlock protocol.
unsafe impl Send for FrameShm {}
unsafe impl Sync for FrameShm {}

impl FrameShm {
    /// Creates (or opens, if it exists) the global mapping. Used by the DLL,
    /// which runs inside a service holding `SeCreateGlobalPrivilege`.
    pub fn create() -> windows::core::Result<Self> {
        unsafe {
            let mut psd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(SHM_SDDL),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&sa),
                PAGE_READWRITE,
                0,
                SHM_SIZE as u32,
                &HSTRING::from(SHM_NAME),
            );
            let _ = LocalFree(Some(HLOCAL(psd.0)));
            let shm = Self::map(handle?)?;
            let h = shm.header();
            if h.magic.load(Ordering::Acquire) != MAGIC {
                h.version.store(VERSION, Ordering::Relaxed);
                h.req_width.store(crate::DEFAULT_WIDTH, Ordering::Relaxed);
                h.req_height.store(crate::DEFAULT_HEIGHT, Ordering::Relaxed);
                h.magic.store(MAGIC, Ordering::Release);
            }
            Ok(shm)
        }
    }

    /// Opens an existing mapping. Used by the app; fails until the DLL has
    /// created it (i.e. until some app opened the virtual camera at least once).
    pub fn open() -> windows::core::Result<Self> {
        unsafe {
            let handle = OpenFileMappingW(
                (FILE_MAP_READ | FILE_MAP_WRITE).0,
                false,
                &HSTRING::from(SHM_NAME),
            )?;
            Self::map(handle)
        }
    }

    unsafe fn map(handle: HANDLE) -> windows::core::Result<Self> {
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, SHM_SIZE) };
        let Some(base) = NonNull::new(view.Value as *mut u8) else {
            let err = windows::core::Error::from_thread();
            unsafe { CloseHandle(handle)? };
            return Err(err);
        };
        Ok(Self { base, mapping: Some((handle, view)) })
    }

    /// Wraps caller-owned memory of at least [`SHM_SIZE`] bytes, 8-byte aligned.
    ///
    /// # Safety
    /// `base` must stay valid for the lifetime of the returned value.
    pub unsafe fn from_raw(base: NonNull<u8>) -> Self {
        Self { base, mapping: None }
    }

    pub fn header(&self) -> &ShmHeader {
        unsafe { &*(self.base.as_ptr() as *const ShmHeader) }
    }

    pub fn is_initialized(&self) -> bool {
        self.header().magic.load(Ordering::Acquire) == MAGIC
    }

    fn data(&self) -> *mut u8 {
        unsafe { self.base.as_ptr().add(HEADER_SIZE) }
    }

    /// Writes one NV12 frame. Frames larger than the buffer are ignored.
    pub fn write_frame(&self, width: u32, height: u32, nv12: &[u8]) {
        let len = (width as usize) * (height as usize) * 3 / 2;
        if len > FRAME_CAPACITY || nv12.len() < len {
            return;
        }
        let h = self.header();
        let s = h.seq.load(Ordering::Relaxed);
        h.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        unsafe { std::ptr::copy_nonoverlapping(nv12.as_ptr(), self.data(), len) };
        h.width.store(width, Ordering::Relaxed);
        h.height.store(height, Ordering::Relaxed);
        h.frame_counter.fetch_add(1, Ordering::Relaxed);
        h.seq.store(s.wrapping_add(2), Ordering::Release);
        h.writer_tick.store(now_ms(), Ordering::Relaxed);
    }

    /// Copies the current frame into `out`. Returns `(width, height, frame_counter)`,
    /// or `None` if no consistent frame could be read (writer busy, nothing written yet).
    pub fn read_frame(&self, out: &mut Vec<u8>) -> Option<(u32, u32, u64)> {
        let h = self.header();
        for _ in 0..3 {
            let s1 = h.seq.load(Ordering::Acquire);
            if s1 == 0 {
                return None;
            }
            if s1 & 1 == 1 {
                std::thread::yield_now();
                continue;
            }
            let (w, hh) = (h.width.load(Ordering::Relaxed), h.height.load(Ordering::Relaxed));
            let counter = h.frame_counter.load(Ordering::Relaxed);
            let len = (w as usize) * (hh as usize) * 3 / 2;
            if len == 0 || len > FRAME_CAPACITY {
                return None;
            }
            out.resize(len, 0);
            unsafe { std::ptr::copy_nonoverlapping(self.data(), out.as_mut_ptr(), len) };
            fence(Ordering::Acquire);
            if h.seq.load(Ordering::Relaxed) == s1 {
                return Some((w, hh, counter));
            }
        }
        None
    }

    pub fn writer_alive(&self) -> bool {
        now_ms().saturating_sub(self.header().writer_tick.load(Ordering::Relaxed)) < HEARTBEAT_TIMEOUT_MS
    }

    pub fn consumer_alive(&self) -> bool {
        now_ms().saturating_sub(self.header().consumer_tick.load(Ordering::Relaxed)) < HEARTBEAT_TIMEOUT_MS
    }
}

impl Drop for FrameShm {
    fn drop(&mut self) {
        if let Some((handle, view)) = self.mapping.take() {
            unsafe {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heap_shm() -> (Vec<u64>, FrameShm) {
        let mut mem = vec![0u64; SHM_SIZE / 8 + 1];
        let shm = unsafe { FrameShm::from_raw(NonNull::new(mem.as_mut_ptr() as *mut u8).unwrap()) };
        (mem, shm)
    }

    #[test]
    fn header_layout_is_stable() {
        // Offsets of the original v1 fields must never move.
        assert_eq!(std::mem::offset_of!(ShmHeader, seq), 16);
        assert_eq!(std::mem::offset_of!(ShmHeader, frame_counter), 32);
        assert_eq!(std::mem::offset_of!(ShmHeader, consumer_tick), 48);
        assert!(std::mem::size_of::<ShmHeader>() <= HEADER_SIZE);
    }

    #[test]
    fn empty_reads_none() {
        let (_mem, shm) = heap_shm();
        let mut out = Vec::new();
        assert!(shm.read_frame(&mut out).is_none());
    }

    #[test]
    fn roundtrip() {
        let (_mem, shm) = heap_shm();
        let frame: Vec<u8> = (0..(64 * 32 * 3 / 2)).map(|i| i as u8).collect();
        shm.write_frame(64, 32, &frame);
        let mut out = Vec::new();
        assert_eq!(shm.read_frame(&mut out), Some((64, 32, 1)));
        assert_eq!(out, frame);
    }

    #[test]
    fn oversized_frame_ignored() {
        let (_mem, shm) = heap_shm();
        shm.write_frame(4096, 4096, &[]);
        assert_eq!(shm.header().seq.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_reads_are_consistent() {
        let (mem, shm) = heap_shm();
        let shm = std::sync::Arc::new(shm);
        let (w, h) = (320u32, 240u32);
        let len = (w * h * 3 / 2) as usize;
        let writer = {
            let shm = shm.clone();
            std::thread::spawn(move || {
                for i in 0..500u32 {
                    shm.write_frame(w, h, &vec![(i % 251) as u8; len]);
                }
            })
        };
        let mut out = Vec::new();
        while !writer.is_finished() {
            if shm.read_frame(&mut out).is_some() {
                assert!(out.iter().all(|&b| b == out[0]), "torn frame");
            }
        }
        writer.join().unwrap();
        assert!(shm.read_frame(&mut out).is_some());
        drop(mem);
    }
}
