//! CPU and memory usage of this process, for the debug view.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessStats {
    /// Share of all cores, 0–100.
    pub cpu_percent: f32,
    pub working_set: u64,
    pub private_bytes: u64,
    pub uptime: Duration,
}

pub struct SysInfo {
    started: Instant,
    last: Option<(Instant, u64)>,
    cores: u32,
    pub stats: ProcessStats,
}

impl SysInfo {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        Self { started: Instant::now(), last: None, cores, stats: ProcessStats::default() }
    }

    /// Resamples at most once per [`SAMPLE_INTERVAL`].
    pub fn update(&mut self) {
        let now = Instant::now();
        if self.last.is_some_and(|(t, _)| now - t < SAMPLE_INTERVAL) {
            return;
        }
        let cpu = cpu_time_100ns();
        if let (Some((t, prev)), Some(cpu)) = (self.last, cpu) {
            let wall = (now - t).as_secs_f64() * 1e7 * self.cores as f64;
            self.stats.cpu_percent = (cpu.saturating_sub(prev) as f64 / wall * 100.0) as f32;
        }
        self.last = cpu.map(|c| (now, c));
        if let Some((ws, private)) = memory() {
            self.stats.working_set = ws;
            self.stats.private_bytes = private;
        }
        self.stats.uptime = now - self.started;
    }
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// Kernel + user time of this process.
fn cpu_time_100ns() -> Option<u64> {
    let (mut create, mut exit, mut kernel, mut user) = Default::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut create, &mut exit, &mut kernel, &mut user) }.ok()?;
    Some(ft(kernel) + ft(user))
}

/// (working set, private bytes).
fn memory() -> Option<(u64, u64)> {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb)
    };
    ok.as_bool().then_some((c.WorkingSetSize as u64, c.PrivateUsage as u64))
}
