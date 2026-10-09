//! Small rolling-statistics helpers for the debug view.

use std::time::{Duration, Instant};

/// Length of one statistics window.
pub const WINDOW: Duration = Duration::from_secs(1);

/// Summary of one finished window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Samples per second.
    pub rate: f32,
    pub avg: f32,
    pub max: f32,
}

/// Accumulates samples (e.g. durations in ms) over a window of [`WINDOW`].
pub struct Window {
    start: Instant,
    count: u32,
    sum: f64,
    max: f32,
}

impl Default for Window {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl Window {
    pub fn new(start: Instant) -> Self {
        Self { start, count: 0, sum: 0.0, max: 0.0 }
    }

    pub fn add(&mut self, value: f32) {
        self.count += 1;
        self.sum += value as f64;
        self.max = self.max.max(value);
    }

    /// Counts an event without a value.
    pub fn tick(&mut self) {
        self.add(0.0);
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    /// Finishes the window if [`WINDOW`] has passed at `now`, then starts a new one.
    pub fn take_if_elapsed(&mut self, now: Instant) -> Option<Snapshot> {
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed < WINDOW {
            return None;
        }
        let snap = Snapshot {
            rate: self.count as f32 / elapsed.as_secs_f32(),
            avg: if self.count == 0 { 0.0 } else { (self.sum / self.count as f64) as f32 },
            max: self.max,
        };
        *self = Self::new(now);
        Some(snap)
    }
}

/// Fixed-size ring of the most recent values, oldest first when iterated.
#[derive(Clone, Debug)]
pub struct History<const N: usize> {
    values: [f32; N],
    len: usize,
    next: usize,
}

impl<const N: usize> Default for History<N> {
    fn default() -> Self {
        Self { values: [0.0; N], len: 0, next: 0 }
    }
}

impl<const N: usize> History<N> {
    pub fn push(&mut self, v: f32) {
        self.values[self.next] = v;
        self.next = (self.next + 1) % N;
        self.len = (self.len + 1).min(N);
    }

    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        let start = (self.next + N - self.len) % N;
        (0..self.len).map(move |i| self.values[(start + i) % N])
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_rate_and_reset() {
        let t0 = Instant::now();
        let mut w = Window::new(t0);
        for v in [1.0, 2.0, 3.0, 6.0] {
            w.add(v);
        }
        assert_eq!(w.take_if_elapsed(t0 + Duration::from_millis(500)), None);
        let s = w.take_if_elapsed(t0 + Duration::from_secs(2)).unwrap();
        assert_eq!(s, Snapshot { rate: 2.0, avg: 3.0, max: 6.0 });
        assert_eq!(w.count(), 0);
        let s = w.take_if_elapsed(t0 + Duration::from_secs(3)).unwrap();
        assert_eq!(s, Snapshot::default());
    }

    #[test]
    fn history_wraps_oldest_first() {
        let mut h = History::<3>::default();
        assert!(h.is_empty());
        for v in 1..=5 {
            h.push(v as f32);
        }
        assert_eq!(h.iter().collect::<Vec<_>>(), vec![3.0, 4.0, 5.0]);
    }
}
