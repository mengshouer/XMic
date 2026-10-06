//! Realtime load measurement and the single overload transition.
//!
//! The previous three-step ladder temporarily bypassed AGC2 before falling back. A degraded path is
//! now all-or-nothing: either the configured DSP chain runs in order, or the session hands off to the
//! configured raw/silence fallback with every DSP stage bypassed.

use std::time::Duration;

const PROCESSING_DEADLINE_US: u64 = 10_000;
pub(super) const PROCESSING_BUDGET_US: u64 = 4_000;
const PROCESSING_HISTOGRAM_BIN_US: u64 = 50;
const PROCESSING_HISTOGRAM_BINS: usize = 4_096;
const OVERLOAD_WINDOW_FRAMES: u32 = 200;
const OVERLOAD_BUDGET_BLOCKS_PER_WINDOW: u32 = 10;
const OVERLOAD_DEADLINES_PER_WINDOW: u32 = 2;
const OVERLOAD_BACKLOG_EVENTS_PER_WINDOW: u32 = 2;
const OVERLOAD_WINDOWS_TO_FALLBACK: u32 = 2;

pub(super) struct ProcessingOverloadController {
    window_frames: u32,
    budget_exceeded: u32,
    deadline_misses: u32,
    backlog_events: u32,
    overloaded_windows: u32,
}

impl ProcessingOverloadController {
    pub(super) fn new() -> Self {
        Self {
            window_frames: 0,
            budget_exceeded: 0,
            deadline_misses: 0,
            backlog_events: 0,
            overloaded_windows: 0,
        }
    }

    /// Returns true once, when sustained overload requires a complete fallback handoff.
    pub(super) fn observe(&mut self, processing_time: Duration, backlog_event: bool) -> bool {
        let processing_us = u64::try_from(processing_time.as_micros()).unwrap_or(u64::MAX);
        self.window_frames = self.window_frames.saturating_add(1);
        if processing_us > PROCESSING_BUDGET_US {
            self.budget_exceeded = self.budget_exceeded.saturating_add(1);
        }
        if processing_us > PROCESSING_DEADLINE_US {
            self.deadline_misses = self.deadline_misses.saturating_add(1);
        }
        if backlog_event {
            self.backlog_events = self.backlog_events.saturating_add(1);
        }
        if self.window_frames < OVERLOAD_WINDOW_FRAMES {
            return false;
        }

        let overloaded = self.budget_exceeded >= OVERLOAD_BUDGET_BLOCKS_PER_WINDOW
            || self.deadline_misses >= OVERLOAD_DEADLINES_PER_WINDOW
            || self.backlog_events >= OVERLOAD_BACKLOG_EVENTS_PER_WINDOW;
        self.window_frames = 0;
        self.budget_exceeded = 0;
        self.deadline_misses = 0;
        self.backlog_events = 0;
        self.overloaded_windows = if overloaded {
            self.overloaded_windows.saturating_add(1)
        } else {
            0
        };
        self.overloaded_windows >= OVERLOAD_WINDOWS_TO_FALLBACK
    }
}

pub(super) struct ProcessingHistogram {
    bins: [u64; PROCESSING_HISTOGRAM_BINS],
    total: u64,
    max_us: u64,
    deadline_misses: u64,
}

impl Default for ProcessingHistogram {
    fn default() -> Self {
        Self {
            bins: [0; PROCESSING_HISTOGRAM_BINS],
            total: 0,
            max_us: 0,
            deadline_misses: 0,
        }
    }
}

impl ProcessingHistogram {
    pub(super) fn observe(&mut self, duration: Duration) {
        let microseconds = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let bin = (microseconds.saturating_sub(1) / PROCESSING_HISTOGRAM_BIN_US)
            .min((PROCESSING_HISTOGRAM_BINS - 1) as u64) as usize;
        self.bins[bin] = self.bins[bin].saturating_add(1);
        self.total = self.total.saturating_add(1);
        self.max_us = self.max_us.max(microseconds);
        if microseconds > PROCESSING_DEADLINE_US {
            self.deadline_misses = self.deadline_misses.saturating_add(1);
        }
    }

    pub(super) fn p99_us(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let target = self.total.saturating_mul(99).div_ceil(100);
        let mut cumulative = 0u64;
        for (index, count) in self.bins.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= target {
                return ((index as u64 + 1) * PROCESSING_HISTOGRAM_BIN_US) as f64;
            }
        }
        (PROCESSING_HISTOGRAM_BINS as u64 * PROCESSING_HISTOGRAM_BIN_US) as f64
    }

    pub(super) fn max_us(&self) -> u64 {
        self.max_us
    }

    pub(super) fn deadline_misses(&self) -> u64 {
        self.deadline_misses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sustained_overload_requests_one_complete_fallback() {
        let mut controller = ProcessingOverloadController::new();
        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_FALLBACK - 1) {
            assert!(!controller.observe(Duration::from_millis(5), false));
        }
        assert!(controller.observe(Duration::from_millis(5), false));
    }

    #[test]
    fn isolated_slow_frames_do_not_trigger_fallback() {
        let mut controller = ProcessingOverloadController::new();
        for frame in 0..(OVERLOAD_WINDOW_FRAMES * 4) {
            let duration = if frame % OVERLOAD_WINDOW_FRAMES == 0 {
                Duration::from_millis(5)
            } else {
                Duration::from_micros(200)
            };
            assert!(!controller.observe(duration, false));
        }
    }

    #[test]
    fn processing_histogram_reports_tail_and_deadlines() {
        let mut histogram = ProcessingHistogram::default();
        for _ in 0..99 {
            histogram.observe(Duration::from_micros(100));
        }
        histogram.observe(Duration::from_micros(18_750));
        assert_eq!(histogram.p99_us(), 100.0);
        assert_eq!(histogram.max_us(), 18_750);
        assert_eq!(histogram.deadline_misses(), 1);
    }
}
