//! The processing-load ladder: how a session decides it cannot keep up, and what it gives up.
//!
//! Only this process own lateness escalates the ladder. Device-side events are deliberately
//! excluded and reported as device instability instead, because no amount of bypassed DSP makes
//! a stalling device deliver audio on time. See `dsp_backlog_events` in the parent module.

use std::time::Duration;

use xmic_engine_protocol::ProcessingLoadMode;

const PROCESSING_DEADLINE_US: u64 = 10_000;
pub(super) const PROCESSING_BUDGET_US: u64 = 4_000;
const PROCESSING_HISTOGRAM_BIN_US: u64 = 50;
const PROCESSING_HISTOGRAM_BINS: usize = 4_096;
const OVERLOAD_WINDOW_FRAMES: u32 = 200;
const OVERLOAD_BUDGET_BLOCKS_PER_WINDOW: u32 = 10;
const OVERLOAD_DEADLINES_PER_WINDOW: u32 = 2;
const OVERLOAD_BACKLOG_EVENTS_PER_WINDOW: u32 = 2;
const OVERLOAD_WINDOWS_TO_ESCALATE: u32 = 2;
const STABLE_WINDOWS_TO_RECOVER: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessingLoadTransition {
    pub(super) previous: ProcessingLoadMode,
    pub(super) current: ProcessingLoadMode,
}

pub(super) struct ProcessingOverloadController {
    mode: ProcessingLoadMode,
    reduced_stage_available: bool,
    window_frames: u32,
    budget_exceeded: u32,
    deadline_misses: u32,
    backlog_events: u32,
    overloaded_windows: u32,
    stable_windows: u32,
}

impl ProcessingOverloadController {
    pub(super) fn new(reduced_stage_available: bool) -> Self {
        Self {
            mode: ProcessingLoadMode::Full,
            reduced_stage_available,
            window_frames: 0,
            budget_exceeded: 0,
            deadline_misses: 0,
            backlog_events: 0,
            overloaded_windows: 0,
            stable_windows: 0,
        }
    }

    pub(super) fn mode(&self) -> ProcessingLoadMode {
        self.mode
    }

    pub(super) fn observe(
        &mut self,
        processing_time: Duration,
        backlog_event: bool,
    ) -> Option<ProcessingLoadTransition> {
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
            return None;
        }

        let overloaded = self.budget_exceeded >= OVERLOAD_BUDGET_BLOCKS_PER_WINDOW
            || self.deadline_misses >= OVERLOAD_DEADLINES_PER_WINDOW
            || self.backlog_events >= OVERLOAD_BACKLOG_EVENTS_PER_WINDOW;
        let stable =
            self.budget_exceeded == 0 && self.deadline_misses == 0 && self.backlog_events == 0;
        self.window_frames = 0;
        self.budget_exceeded = 0;
        self.deadline_misses = 0;
        self.backlog_events = 0;

        if overloaded {
            self.overloaded_windows = self.overloaded_windows.saturating_add(1);
            self.stable_windows = 0;
        } else if stable {
            self.stable_windows = self.stable_windows.saturating_add(1);
            self.overloaded_windows = 0;
        } else {
            self.overloaded_windows = 0;
            self.stable_windows = 0;
        }

        let previous = self.mode;
        if self.overloaded_windows >= OVERLOAD_WINDOWS_TO_ESCALATE {
            self.mode = match self.mode {
                ProcessingLoadMode::Full if self.reduced_stage_available => {
                    ProcessingLoadMode::Reduced
                }
                ProcessingLoadMode::Full => ProcessingLoadMode::Protection,
                ProcessingLoadMode::Reduced | ProcessingLoadMode::Protection => {
                    ProcessingLoadMode::Protection
                }
            };
            self.overloaded_windows = 0;
        } else if self.stable_windows >= STABLE_WINDOWS_TO_RECOVER {
            self.mode = match self.mode {
                ProcessingLoadMode::Protection if self.reduced_stage_available => {
                    ProcessingLoadMode::Reduced
                }
                ProcessingLoadMode::Protection => ProcessingLoadMode::Full,
                ProcessingLoadMode::Reduced | ProcessingLoadMode::Full => ProcessingLoadMode::Full,
            };
            self.stable_windows = 0;
        }

        (self.mode != previous).then_some(ProcessingLoadTransition {
            previous,
            current: self.mode,
        })
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
    fn processing_histogram_reports_tail_without_allocations() {
        let mut histogram = ProcessingHistogram::default();
        for _ in 0..99 {
            histogram.observe(Duration::from_micros(100));
        }
        histogram.observe(Duration::from_micros(500));

        assert_eq!(histogram.p99_us(), 100.0);
        assert_eq!(histogram.max_us(), 500);
        assert_eq!(histogram.deadline_misses(), 0);
    }

    #[test]
    fn processing_histogram_reports_deadline_misses_beyond_ten_ms() {
        let mut histogram = ProcessingHistogram::default();
        histogram.observe(Duration::from_micros(18_750));

        assert_eq!(histogram.p99_us(), 18_750.0);
        assert_eq!(histogram.max_us(), 18_750);
        assert_eq!(histogram.deadline_misses(), 1);
    }

    #[test]
    fn sustained_overload_escalates_and_stable_windows_recover() {
        let mut controller = ProcessingOverloadController::new(true);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Reduced);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Protection);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Reduced);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

    #[test]
    fn isolated_slow_frames_do_not_trigger_degradation() {
        let mut controller = ProcessingOverloadController::new(true);
        for frame in 0..(OVERLOAD_WINDOW_FRAMES * 4) {
            let duration = if frame % OVERLOAD_WINDOW_FRAMES == 0 {
                Duration::from_millis(5)
            } else {
                Duration::from_micros(200)
            };
            controller.observe(duration, false);
        }

        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

    #[test]
    fn overload_skips_the_reduced_stage_when_agc2_is_disabled() {
        let mut controller = ProcessingOverloadController::new(false);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Protection);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

    #[test]
    fn a_dropping_device_does_not_degrade_the_dsp() {
        let mut controller = ProcessingOverloadController::new(true);

        // A device that drops packets and starves the output every single frame, while processing
        // itself stays far inside its budget: the ladder must not move, because bypassing AGC2 or the
        // whole chain cannot make the device deliver audio on time.
        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE * 4) {
            controller.observe(Duration::from_micros(200), false);
        }

        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }
}
