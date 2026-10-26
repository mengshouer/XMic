const QPC_TICKS_PER_MILLISECOND: f64 = 10_000.0;
const MAX_STREAM_DELAY_MS: f64 = 500.0;
const LARGE_SKEW_JUMP_MS: f64 = 120.0;
const WARMUP_SAMPLES: u16 = 20;
const SMOOTHING_FACTOR: f64 = 0.1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DelayCalibrationState {
    #[default]
    Uncalibrated,
    WarmingUp,
    Calibrated,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostDelayObservation {
    pub stream_delay_ms: i32,
    pub raw_skew_ms: Option<f32>,
    pub jitter_ms: f32,
    pub state: DelayCalibrationState,
    pub reset_required: bool,
}

#[derive(Debug, Default)]
pub struct HostDelayEstimator {
    previous_raw_skew_ms: Option<f64>,
    smoothed_delay_ms: Option<f64>,
    jitter_ms: f64,
    valid_samples: u16,
    warmup_delays_ms: [f64; WARMUP_SAMPLES as usize],
}

impl HostDelayEstimator {
    pub fn observe(
        &mut self,
        capture_qpc_position_100ns: u64,
        render_qpc_position_100ns: u64,
    ) -> HostDelayObservation {
        if capture_qpc_position_100ns == 0 || render_qpc_position_100ns == 0 {
            let reset_required = self.has_history();
            self.reset();
            return self.observation(None, reset_required);
        }

        let signed_ticks =
            i128::from(capture_qpc_position_100ns) - i128::from(render_qpc_position_100ns);
        let raw_skew_ms = signed_ticks as f64 / QPC_TICKS_PER_MILLISECOND;
        let reset_required = self.valid_samples >= WARMUP_SAMPLES
            && self
                .previous_raw_skew_ms
                .is_some_and(|previous| (raw_skew_ms - previous).abs() > LARGE_SKEW_JUMP_MS);
        if reset_required {
            self.reset();
        }

        self.update(raw_skew_ms);
        self.observation(Some(raw_skew_ms), reset_required)
    }

    pub fn current(&self) -> HostDelayObservation {
        self.observation(self.previous_raw_skew_ms, false)
    }

    pub fn reset(&mut self) {
        self.previous_raw_skew_ms = None;
        self.smoothed_delay_ms = None;
        self.jitter_ms = 0.0;
        self.valid_samples = 0;
        self.warmup_delays_ms.fill(0.0);
    }

    fn has_history(&self) -> bool {
        self.valid_samples != 0
    }

    fn update(&mut self, raw_skew_ms: f64) {
        let delay_ms = raw_skew_ms.clamp(0.0, MAX_STREAM_DELAY_MS);
        if self.valid_samples < WARMUP_SAMPLES {
            self.warmup_delays_ms[usize::from(self.valid_samples)] = delay_ms;
            let sample_count = usize::from(self.valid_samples) + 1;
            let samples = &self.warmup_delays_ms[..sample_count];
            let median_delay_ms = median(samples);
            self.smoothed_delay_ms = Some(median_delay_ms);
            self.jitter_ms = median_absolute_deviation(samples, median_delay_ms);
        } else if let Some(previous) = self.smoothed_delay_ms {
            let deviation = (delay_ms - previous).abs();
            self.jitter_ms += SMOOTHING_FACTOR * (deviation - self.jitter_ms);
            self.smoothed_delay_ms = Some(previous + SMOOTHING_FACTOR * (delay_ms - previous));
        } else {
            self.smoothed_delay_ms = Some(delay_ms);
        }
        self.previous_raw_skew_ms = Some(raw_skew_ms);
        self.valid_samples = self.valid_samples.saturating_add(1);
    }

    fn observation(&self, raw_skew_ms: Option<f64>, reset_required: bool) -> HostDelayObservation {
        HostDelayObservation {
            stream_delay_ms: self
                .smoothed_delay_ms
                .unwrap_or(0.0)
                .round()
                .clamp(0.0, MAX_STREAM_DELAY_MS) as i32,
            raw_skew_ms: raw_skew_ms.map(|value| value as f32),
            jitter_ms: self.jitter_ms as f32,
            state: match self.valid_samples {
                0 => DelayCalibrationState::Uncalibrated,
                WARMUP_SAMPLES.. => DelayCalibrationState::Calibrated,
                _ => DelayCalibrationState::WarmingUp,
            },
            reset_required,
        }
    }
}

fn median(values: &[f64]) -> f64 {
    debug_assert!(!values.is_empty());
    debug_assert!(values.len() <= WARMUP_SAMPLES as usize);
    let mut sorted = [0.0; WARMUP_SAMPLES as usize];
    sorted[..values.len()].copy_from_slice(values);
    sorted[..values.len()].sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) * 0.5
    } else {
        sorted[middle]
    }
}

fn median_absolute_deviation(values: &[f64], median_value: f64) -> f64 {
    let mut deviations = [0.0; WARMUP_SAMPLES as usize];
    for (deviation, value) in deviations.iter_mut().zip(values) {
        *deviation = (*value - median_value).abs();
    }
    median(&deviations[..values.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_QPC: u64 = 50_000_000;

    fn qpc_after(milliseconds: u64) -> u64 {
        BASE_QPC + milliseconds * QPC_TICKS_PER_MILLISECOND as u64
    }

    #[test]
    fn equal_timestamps_produce_zero_delay() {
        let observation = HostDelayEstimator::default().observe(BASE_QPC, BASE_QPC);

        assert_eq!(observation.stream_delay_ms, 0);
        assert_eq!(observation.raw_skew_ms, Some(0.0));
        assert_eq!(observation.state, DelayCalibrationState::WarmingUp);
        assert!(!observation.reset_required);
    }

    #[test]
    fn capture_after_render_produces_positive_delay() {
        let observation = HostDelayEstimator::default().observe(qpc_after(20), BASE_QPC);

        assert_eq!(observation.stream_delay_ms, 20);
        assert_eq!(observation.raw_skew_ms, Some(20.0));
    }

    #[test]
    fn negative_skew_is_reported_but_clamped_for_webrtc() {
        let observation = HostDelayEstimator::default().observe(BASE_QPC, qpc_after(20));

        assert_eq!(observation.stream_delay_ms, 0);
        assert_eq!(observation.raw_skew_ms, Some(-20.0));
    }

    #[test]
    fn warmup_reaches_calibrated_state() {
        let mut estimator = HostDelayEstimator::default();
        for offset in 0..WARMUP_SAMPLES {
            estimator.observe(
                qpc_after(20 + u64::from(offset)),
                qpc_after(u64::from(offset)),
            );
        }

        assert_eq!(estimator.current().state, DelayCalibrationState::Calibrated);
        assert_eq!(estimator.current().stream_delay_ms, 20);
    }

    #[test]
    fn warmup_median_rejects_an_initial_outlier() {
        let mut estimator = HostDelayEstimator::default();
        estimator.observe(qpc_after(100), BASE_QPC);
        for _ in 1..WARMUP_SAMPLES {
            estimator.observe(qpc_after(20), BASE_QPC);
        }

        assert_eq!(estimator.current().state, DelayCalibrationState::Calibrated);
        assert_eq!(estimator.current().stream_delay_ms, 20);
        assert_eq!(estimator.current().jitter_ms, 0.0);
    }

    #[test]
    fn ordinary_jitter_is_smoothed() {
        let mut estimator = HostDelayEstimator::default();
        for _ in 0..WARMUP_SAMPLES {
            estimator.observe(qpc_after(20), BASE_QPC);
        }
        let observation = estimator.observe(qpc_after(30), BASE_QPC);

        assert_eq!(observation.stream_delay_ms, 21);
        assert!(observation.jitter_ms > 0.0);
        assert!(!observation.reset_required);
    }

    #[test]
    fn large_jump_restarts_warmup_and_requests_echo_path_reset() {
        let mut estimator = HostDelayEstimator::default();
        for _ in 0..WARMUP_SAMPLES {
            estimator.observe(qpc_after(20), BASE_QPC);
        }
        let observation = estimator.observe(qpc_after(200), BASE_QPC);

        assert!(observation.reset_required);
        assert_eq!(observation.state, DelayCalibrationState::WarmingUp);
        assert_eq!(observation.stream_delay_ms, 200);
        assert_eq!(observation.jitter_ms, 0.0);
    }

    #[test]
    fn invalid_timestamp_clears_existing_history_once() {
        let mut estimator = HostDelayEstimator::default();
        estimator.observe(qpc_after(20), BASE_QPC);

        let first = estimator.observe(0, BASE_QPC);
        let second = estimator.observe(0, BASE_QPC);

        assert!(first.reset_required);
        assert!(!second.reset_required);
        assert_eq!(second.state, DelayCalibrationState::Uncalibrated);
    }

    #[test]
    fn explicit_reset_clears_history() {
        let mut estimator = HostDelayEstimator::default();
        estimator.observe(qpc_after(20), BASE_QPC);
        estimator.reset();

        assert_eq!(
            estimator.current().state,
            DelayCalibrationState::Uncalibrated
        );
        assert_eq!(estimator.current().raw_skew_ms, None);
        assert_eq!(estimator.current().stream_delay_ms, 0);
    }

    #[test]
    fn stream_delay_stays_within_native_range() {
        let observation = HostDelayEstimator::default().observe(qpc_after(800), BASE_QPC);

        assert_eq!(observation.stream_delay_ms, 500);
    }

    #[test]
    fn warmup_outlier_above_jump_threshold_does_not_trigger_reset() {
        let mut estimator = HostDelayEstimator::default();
        let first = estimator.observe(qpc_after(200), BASE_QPC);
        assert!(!first.reset_required);

        for _ in 1..WARMUP_SAMPLES {
            let next = estimator.observe(qpc_after(20), BASE_QPC);
            assert!(!next.reset_required);
        }

        assert_eq!(estimator.current().state, DelayCalibrationState::Calibrated);
        assert_eq!(estimator.current().stream_delay_ms, 20);
    }
}
