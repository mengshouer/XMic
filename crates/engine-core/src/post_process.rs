use std::collections::VecDeque;

use crate::{DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ};

const MAX_VAD_PRE_ROLL_FRAMES: usize = 1_000;

#[derive(Debug, Clone, Copy)]
pub struct EchoGuardConfig {
    pub enabled: bool,
    pub minimum_input_dbfs: f32,
    pub likelihood_threshold: f32,
    pub attenuation_threshold_db: f32,
    pub instantaneous_attenuation_threshold_db: f32,
    pub strong_attenuation_threshold_db: f32,
    pub strong_instantaneous_attenuation_threshold_db: f32,
    pub nearend_vad_threshold: f32,
    pub nearend_instantaneous_attenuation_threshold_db: f32,
    pub nearend_smoothed_attenuation_threshold_db: f32,
    pub nearend_echo_likelihood_veto_threshold: f32,
    pub dominant_nearend_vad_threshold: f32,
    pub dominant_nearend_instantaneous_attenuation_threshold_db: f32,
    pub dominant_nearend_smoothed_attenuation_threshold_db: f32,
    pub nearend_confirmation_frames: usize,
    pub hold_frames: usize,
    pub smoothing_factor: f32,
}

impl Default for EchoGuardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            minimum_input_dbfs: -70.0,
            likelihood_threshold: 0.05,
            attenuation_threshold_db: -5.5,
            instantaneous_attenuation_threshold_db: -4.0,
            strong_attenuation_threshold_db: -10.0,
            strong_instantaneous_attenuation_threshold_db: -6.0,
            nearend_vad_threshold: 0.85,
            nearend_instantaneous_attenuation_threshold_db: -2.0,
            nearend_smoothed_attenuation_threshold_db: -2.5,
            nearend_echo_likelihood_veto_threshold: 0.25,
            dominant_nearend_vad_threshold: 0.98,
            dominant_nearend_instantaneous_attenuation_threshold_db: -0.75,
            dominant_nearend_smoothed_attenuation_threshold_db: -1.5,
            nearend_confirmation_frames: 6,
            hold_frames: 50,
            smoothing_factor: 0.8,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EchoGuardFrameMetrics {
    pub active: bool,
    pub attenuation_db: Option<f32>,
    pub effective_vad_probability: f32,
    pub nearend_candidate: bool,
    pub released: bool,
}

pub struct EchoGuard {
    config: EchoGuardConfig,
    smoothed_input_power: f32,
    smoothed_output_power: f32,
    initialized: bool,
    nearend_candidate_frames: usize,
    hold_remaining: usize,
}

impl EchoGuard {
    pub fn new(config: EchoGuardConfig) -> Result<Self, PostProcessConfigError> {
        let valid = config.minimum_input_dbfs.is_finite()
            && (-120.0..=0.0).contains(&config.minimum_input_dbfs)
            && config.likelihood_threshold.is_finite()
            && (0.0..=1.0).contains(&config.likelihood_threshold)
            && config.attenuation_threshold_db.is_finite()
            && (-60.0..=0.0).contains(&config.attenuation_threshold_db)
            && config.instantaneous_attenuation_threshold_db.is_finite()
            && (-60.0..=0.0).contains(&config.instantaneous_attenuation_threshold_db)
            && config.strong_attenuation_threshold_db.is_finite()
            && (-60.0..=0.0).contains(&config.strong_attenuation_threshold_db)
            && config
                .strong_instantaneous_attenuation_threshold_db
                .is_finite()
            && (-60.0..=0.0).contains(&config.strong_instantaneous_attenuation_threshold_db)
            && config.nearend_vad_threshold.is_finite()
            && (0.0..=1.0).contains(&config.nearend_vad_threshold)
            && config
                .nearend_instantaneous_attenuation_threshold_db
                .is_finite()
            && (-60.0..=0.0).contains(&config.nearend_instantaneous_attenuation_threshold_db)
            && config.nearend_smoothed_attenuation_threshold_db.is_finite()
            && (-60.0..=0.0).contains(&config.nearend_smoothed_attenuation_threshold_db)
            && config.nearend_echo_likelihood_veto_threshold.is_finite()
            && (0.0..=1.0).contains(&config.nearend_echo_likelihood_veto_threshold)
            && config.dominant_nearend_vad_threshold.is_finite()
            && (0.0..=1.0).contains(&config.dominant_nearend_vad_threshold)
            && config
                .dominant_nearend_instantaneous_attenuation_threshold_db
                .is_finite()
            && (-60.0..=0.0)
                .contains(&config.dominant_nearend_instantaneous_attenuation_threshold_db)
            && config
                .dominant_nearend_smoothed_attenuation_threshold_db
                .is_finite()
            && (-60.0..=0.0).contains(&config.dominant_nearend_smoothed_attenuation_threshold_db)
            && (1..=100).contains(&config.nearend_confirmation_frames)
            && config.hold_frames <= 1_000
            && config.smoothing_factor.is_finite()
            && (0.0..1.0).contains(&config.smoothing_factor);
        if !valid {
            return Err(PostProcessConfigError::InvalidEchoGuard);
        }
        Ok(Self {
            config,
            smoothed_input_power: 0.0,
            smoothed_output_power: 0.0,
            initialized: false,
            nearend_candidate_frames: 0,
            hold_remaining: 0,
        })
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn reset(&mut self) {
        self.smoothed_input_power = 0.0;
        self.smoothed_output_power = 0.0;
        self.initialized = false;
        self.nearend_candidate_frames = 0;
        self.hold_remaining = 0;
    }

    pub fn process_frame(
        &mut self,
        vad_probability: f32,
        residual_echo_likelihood: Option<f32>,
        input: &[f32],
        aec_output: &[f32],
    ) -> Result<EchoGuardFrameMetrics, PostProcessConfigError> {
        if input.len() != DSP_SAMPLES_PER_FRAME || aec_output.len() != DSP_SAMPLES_PER_FRAME {
            return Err(PostProcessConfigError::InvalidEchoGuardFrameLength {
                input: input.len(),
                output: aec_output.len(),
            });
        }
        if !self.config.enabled {
            return Ok(EchoGuardFrameMetrics {
                active: false,
                attenuation_db: None,
                effective_vad_probability: vad_probability,
                nearend_candidate: false,
                released: false,
            });
        }

        let input_power = frame_power(input);
        let output_power = frame_power(aec_output);
        if !self.initialized {
            self.smoothed_input_power = input_power;
            self.smoothed_output_power = output_power;
            self.initialized = true;
        } else {
            let retained = self.config.smoothing_factor;
            let incoming = 1.0 - retained;
            self.smoothed_input_power =
                retained * self.smoothed_input_power + incoming * input_power;
            self.smoothed_output_power =
                retained * self.smoothed_output_power + incoming * output_power;
        }

        let input_dbfs = power_dbfs(input_power);
        let instantaneous_attenuation_db = power_ratio_db(output_power, input_power);
        let attenuation_db = power_ratio_db(self.smoothed_output_power, self.smoothed_input_power);
        let likelihood_supports_echo = residual_echo_likelihood.is_some_and(|likelihood| {
            likelihood.is_finite() && likelihood >= self.config.likelihood_threshold
        });
        let correlated_echo = likelihood_supports_echo
            && attenuation_db <= self.config.attenuation_threshold_db
            && instantaneous_attenuation_db <= self.config.instantaneous_attenuation_threshold_db;
        let strongly_attenuated_echo = attenuation_db
            <= self.config.strong_attenuation_threshold_db
            && instantaneous_attenuation_db
                <= self.config.strong_instantaneous_attenuation_threshold_db;
        let echo_evidence = input_dbfs >= self.config.minimum_input_dbfs
            && (correlated_echo || strongly_attenuated_echo);
        let echo_likelihood_vetoes_release = match residual_echo_likelihood {
            Some(likelihood) if likelihood.is_finite() => {
                likelihood >= self.config.nearend_echo_likelihood_veto_threshold
            }
            Some(_) => true,
            None => false,
        };
        let normal_nearend_evidence = vad_probability.is_finite()
            && vad_probability >= self.config.nearend_vad_threshold
            && instantaneous_attenuation_db
                >= self.config.nearend_instantaneous_attenuation_threshold_db
            && attenuation_db >= self.config.nearend_smoothed_attenuation_threshold_db
            && !echo_likelihood_vetoes_release;
        // Strong, unattenuated speech may override a stale AEC likelihood during double-talk.
        let dominant_nearend_evidence = vad_probability.is_finite()
            && vad_probability >= self.config.dominant_nearend_vad_threshold
            && instantaneous_attenuation_db
                >= self
                    .config
                    .dominant_nearend_instantaneous_attenuation_threshold_db
            && attenuation_db
                >= self
                    .config
                    .dominant_nearend_smoothed_attenuation_threshold_db;
        let nearend_evidence = normal_nearend_evidence || dominant_nearend_evidence;
        let suspected_residual_echo =
            input_dbfs >= self.config.minimum_input_dbfs && echo_likelihood_vetoes_release;
        let mut released = false;
        let active = if echo_evidence {
            self.nearend_candidate_frames = 0;
            self.hold_remaining = self.config.hold_frames;
            true
        } else if nearend_evidence && self.hold_remaining > 0 {
            self.nearend_candidate_frames = self
                .nearend_candidate_frames
                .saturating_add(1)
                .min(self.config.nearend_confirmation_frames);
            if self.nearend_candidate_frames >= self.config.nearend_confirmation_frames {
                self.hold_remaining = 0;
                released = true;
                false
            } else {
                true
            }
        } else if self.hold_remaining > 0 {
            self.nearend_candidate_frames = 0;
            if suspected_residual_echo {
                self.hold_remaining = self.config.hold_frames;
            } else {
                self.hold_remaining -= 1;
            }
            true
        } else {
            self.nearend_candidate_frames = 0;
            false
        };

        Ok(EchoGuardFrameMetrics {
            active,
            attenuation_db: Some(attenuation_db),
            effective_vad_probability: if active { 0.0 } else { vad_probability },
            nearend_candidate: nearend_evidence && active,
            released,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VadGateConfig {
    pub enabled: bool,
    pub threshold: f32,
    pub confirmation_frames: usize,
    pub pre_roll_frames: usize,
    pub hold_frames: usize,
    pub floor_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Default for VadGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 0.70,
            confirmation_frames: 12,
            pre_roll_frames: 2,
            hold_frames: 30,
            floor_db: -90.0,
            attack_ms: 10.0,
            release_ms: 150.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VadGateFrameMetrics {
    pub open: bool,
    pub final_gain: f32,
    pub minimum_gain: f32,
}

pub struct VadGate {
    config: VadGateConfig,
    floor_gain: f32,
    attack_coefficient: f32,
    release_coefficient: f32,
    voice_candidate_frames: usize,
    primary_voice_seen: bool,
    secondary_voice_seen: bool,
    hold_remaining: usize,
    delayed_frames: VecDeque<[f32; DSP_SAMPLES_PER_FRAME]>,
    gain: f32,
}

impl VadGate {
    pub fn new(config: VadGateConfig) -> Result<Self, PostProcessConfigError> {
        if !config.threshold.is_finite() || !(0.0..=1.0).contains(&config.threshold) {
            return Err(PostProcessConfigError::InvalidVadThreshold);
        }
        if !(1..=100).contains(&config.confirmation_frames) {
            return Err(PostProcessConfigError::InvalidVadConfirmation);
        }
        if config.pre_roll_frames > MAX_VAD_PRE_ROLL_FRAMES {
            return Err(PostProcessConfigError::InvalidVadPreRoll);
        }
        if config.hold_frames > 1_000 {
            return Err(PostProcessConfigError::InvalidVadHold);
        }
        if !config.floor_db.is_finite() || !(-90.0..=0.0).contains(&config.floor_db) {
            return Err(PostProcessConfigError::InvalidVadFloor);
        }
        if !valid_time_ms(config.attack_ms) || !valid_time_ms(config.release_ms) {
            return Err(PostProcessConfigError::InvalidVadTiming);
        }
        let pre_roll_frames = if config.enabled {
            config.pre_roll_frames
        } else {
            0
        };
        let mut delayed_frames = VecDeque::with_capacity(pre_roll_frames);
        delayed_frames.resize(pre_roll_frames, [0.0; DSP_SAMPLES_PER_FRAME]);
        let floor_gain = db_to_gain(config.floor_db);
        Ok(Self {
            config,
            floor_gain,
            attack_coefficient: smoothing_coefficient(config.attack_ms),
            release_coefficient: smoothing_coefficient(config.release_ms),
            voice_candidate_frames: 0,
            primary_voice_seen: false,
            secondary_voice_seen: false,
            hold_remaining: 0,
            delayed_frames,
            gain: if config.enabled { floor_gain } else { 1.0 },
        })
    }

    pub fn process_frame(
        &mut self,
        vad_probability: f32,
        samples: &mut [f32],
    ) -> Result<VadGateFrameMetrics, PostProcessConfigError> {
        self.process_frame_with_secondary_vad(vad_probability, None, samples)
    }

    pub fn process_frame_with_secondary_vad(
        &mut self,
        vad_probability: f32,
        secondary_voice_active: Option<bool>,
        samples: &mut [f32],
    ) -> Result<VadGateFrameMetrics, PostProcessConfigError> {
        self.process_frame_with_secondary_vad_and_veto(
            vad_probability,
            secondary_voice_active,
            false,
            samples,
        )
    }

    /// Processes a frame while allowing a sidechain to veto the gate. A veto clears both pending
    /// confirmation and an existing hold, so a stale candidate cannot reopen the gate on echo.
    pub fn process_frame_with_secondary_vad_and_veto(
        &mut self,
        vad_probability: f32,
        secondary_voice_active: Option<bool>,
        veto: bool,
        samples: &mut [f32],
    ) -> Result<VadGateFrameMetrics, PostProcessConfigError> {
        if samples.len() != DSP_SAMPLES_PER_FRAME {
            return Err(PostProcessConfigError::InvalidFrameLength {
                actual: samples.len(),
            });
        }
        if !self.config.enabled {
            return Ok(VadGateFrameMetrics {
                open: true,
                final_gain: 1.0,
                minimum_gain: 1.0,
            });
        }

        let open = if veto {
            self.voice_candidate_frames = 0;
            self.primary_voice_seen = false;
            self.secondary_voice_seen = false;
            self.hold_remaining = 0;
            false
        } else {
            let primary_voice_candidate =
                vad_probability.is_finite() && vad_probability >= self.config.threshold;
            let secondary_voice_candidate = secondary_voice_active == Some(true);
            let voice_candidate = primary_voice_candidate || secondary_voice_candidate;
            if voice_candidate {
                self.voice_candidate_frames = self
                    .voice_candidate_frames
                    .saturating_add(1)
                    .min(self.config.confirmation_frames);
                self.primary_voice_seen |= primary_voice_candidate;
                self.secondary_voice_seen |=
                    secondary_voice_candidate || secondary_voice_active.is_none();
            } else {
                self.voice_candidate_frames = 0;
                self.primary_voice_seen = false;
                self.secondary_voice_seen = false;
            }
            let voice_active = self.voice_candidate_frames >= self.config.confirmation_frames
                && self.primary_voice_seen
                && self.secondary_voice_seen;
            if voice_active {
                self.hold_remaining = self.config.hold_frames;
                true
            } else if self.hold_remaining > 0 {
                // Publish this frame as held before consuming it. Otherwise a configured N-frame
                // hold only kept N-1 complete frames open after speech ended.
                self.hold_remaining -= 1;
                true
            } else {
                false
            }
        };
        let target = if open { 1.0 } else { self.floor_gain };
        let coefficient = if target > self.gain {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        let mut delayed = [0.0; DSP_SAMPLES_PER_FRAME];
        delayed.copy_from_slice(samples);
        if let Some(previous) = self.delayed_frames.pop_front() {
            self.delayed_frames.push_back(delayed);
            delayed = previous;
        }
        let mut minimum_gain = self.gain;
        for (sample, delayed_sample) in samples.iter_mut().zip(delayed) {
            self.gain = target + coefficient * (self.gain - target);
            minimum_gain = minimum_gain.min(self.gain);
            *sample = sanitize_sample(delayed_sample) * self.gain;
        }
        Ok(VadGateFrameMetrics {
            open,
            final_gain: self.gain,
            minimum_gain,
        })
    }

    pub fn reset(&mut self) {
        self.voice_candidate_frames = 0;
        self.primary_voice_seen = false;
        self.secondary_voice_seen = false;
        self.hold_remaining = 0;
        for frame in &mut self.delayed_frames {
            frame.fill(0.0);
        }
        self.gain = if self.config.enabled {
            self.floor_gain
        } else {
            1.0
        };
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SafetyLimiterConfig {
    pub ceiling_dbfs: f32,
    pub release_ms: f32,
}

impl Default for SafetyLimiterConfig {
    fn default() -> Self {
        Self {
            ceiling_dbfs: -1.0,
            release_ms: 50.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LimiterFrameMetrics {
    pub limited_samples: usize,
    pub minimum_gain: f32,
}

pub struct SafetyLimiter {
    ceiling: f32,
    release_coefficient: f32,
    gain: f32,
}

impl SafetyLimiter {
    pub fn new(config: SafetyLimiterConfig) -> Result<Self, PostProcessConfigError> {
        if !config.ceiling_dbfs.is_finite() || !(-30.0..=0.0).contains(&config.ceiling_dbfs) {
            return Err(PostProcessConfigError::InvalidLimiterCeiling);
        }
        if !valid_time_ms(config.release_ms) {
            return Err(PostProcessConfigError::InvalidLimiterRelease);
        }
        Ok(Self {
            ceiling: db_to_gain(config.ceiling_dbfs),
            release_coefficient: smoothing_coefficient(config.release_ms),
            gain: 1.0,
        })
    }

    pub fn process_frame(
        &mut self,
        samples: &mut [f32],
    ) -> Result<LimiterFrameMetrics, PostProcessConfigError> {
        if samples.len() != DSP_SAMPLES_PER_FRAME {
            return Err(PostProcessConfigError::InvalidFrameLength {
                actual: samples.len(),
            });
        }
        let mut limited_samples = 0usize;
        let mut minimum_gain = self.gain;
        for sample in samples {
            let sample_value = sanitize_sample(*sample);
            let peak = sample_value.abs();
            let required_gain = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            if required_gain < self.gain {
                self.gain = required_gain;
            } else {
                self.gain = 1.0 + self.release_coefficient * (self.gain - 1.0);
            }
            minimum_gain = minimum_gain.min(self.gain);
            if self.gain < 0.999_999 {
                limited_samples += 1;
            }
            *sample = (sample_value * self.gain).clamp(-self.ceiling, self.ceiling);
        }
        Ok(LimiterFrameMetrics {
            limited_samples,
            minimum_gain,
        })
    }
}

fn valid_time_ms(value: f32) -> bool {
    value.is_finite() && (0.1..=10_000.0).contains(&value)
}

fn smoothing_coefficient(time_ms: f32) -> f32 {
    (-1.0 / (time_ms * 0.001 * DSP_SAMPLE_RATE_HZ as f32)).exp()
}

fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

fn frame_power(samples: &[f32]) -> f32 {
    samples
        .iter()
        .map(|sample| sanitize_sample(*sample).powi(2))
        .sum::<f32>()
        / samples.len() as f32
}

fn power_dbfs(power: f32) -> f32 {
    10.0 * power.max(f32::MIN_POSITIVE).log10()
}

fn power_ratio_db(numerator: f32, denominator: f32) -> f32 {
    const POWER_FLOOR: f32 = 1.0e-12;
    10.0 * ((numerator + POWER_FLOOR) / (denominator + POWER_FLOOR)).log10()
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PostProcessConfigError {
    #[error("invalid echo guard configuration")]
    InvalidEchoGuard,
    #[error("echo guard frame has input/output lengths {input}/{output}; expected 480")]
    InvalidEchoGuardFrameLength { input: usize, output: usize },
    #[error("invalid VAD threshold")]
    InvalidVadThreshold,
    #[error("invalid VAD confirmation duration")]
    InvalidVadConfirmation,
    #[error("invalid VAD pre-roll duration")]
    InvalidVadPreRoll,
    #[error("invalid VAD hold duration")]
    InvalidVadHold,
    #[error("invalid VAD floor")]
    InvalidVadFloor,
    #[error("invalid VAD attack or release")]
    InvalidVadTiming,
    #[error("invalid limiter ceiling")]
    InvalidLimiterCeiling,
    #[error("invalid limiter release")]
    InvalidLimiterRelease,
    #[error("post-processing frame has {actual} samples; expected 480")]
    InvalidFrameLength { actual: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut limiter = SafetyLimiter::new(SafetyLimiterConfig::default()).unwrap();
        let mut samples = [1.0; DSP_SAMPLES_PER_FRAME];
        let metrics = limiter.process_frame(&mut samples).unwrap();
        let ceiling = db_to_gain(-1.0);

        assert!(metrics.limited_samples > 0);
        assert!(samples.iter().all(|sample| sample.abs() <= ceiling));
    }

    #[test]
    fn disabled_gate_is_bit_transparent_for_finite_samples() {
        let mut gate = VadGate::new(VadGateConfig::default()).unwrap();
        let mut samples = [0.25; DSP_SAMPLES_PER_FRAME];
        let metrics = gate.process_frame(0.0, &mut samples).unwrap();

        assert!(metrics.open);
        assert_eq!(samples, [0.25; DSP_SAMPLES_PER_FRAME]);
    }

    #[test]
    fn gate_holds_after_voice_ends() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 1,
            hold_frames: 2,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.25; DSP_SAMPLES_PER_FRAME];

        assert!(gate.process_frame(1.0, &mut samples).unwrap().open);
        assert!(gate.process_frame(0.0, &mut samples).unwrap().open);
        assert!(gate.process_frame(0.0, &mut samples).unwrap().open);
        assert!(!gate.process_frame(0.0, &mut samples).unwrap().open);
    }

    #[test]
    fn sidechain_veto_clears_pending_confirmation_and_hold() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 2,
            hold_frames: 3,
            pre_roll_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.25; DSP_SAMPLES_PER_FRAME];

        assert!(
            !gate
                .process_frame_with_secondary_vad_and_veto(1.0, Some(true), false, &mut samples)
                .unwrap()
                .open
        );
        assert!(
            !gate
                .process_frame_with_secondary_vad_and_veto(0.0, Some(false), true, &mut samples)
                .unwrap()
                .open
        );
        assert!(
            !gate
                .process_frame_with_secondary_vad_and_veto(1.0, Some(true), false, &mut samples)
                .unwrap()
                .open
        );
        assert!(
            gate.process_frame_with_secondary_vad_and_veto(1.0, Some(true), false, &mut samples)
                .unwrap()
                .open
        );
    }

    #[test]
    fn gate_rejects_short_voice_candidates() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            floor_db: -90.0,
            attack_ms: 0.1,
            release_ms: 0.1,
            ..VadGateConfig::default()
        })
        .unwrap();

        for probability in [1.0, 0.0, 0.0] {
            let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];
            assert!(!gate.process_frame(probability, &mut samples).unwrap().open);
        }
    }

    #[test]
    fn secondary_vad_only_needs_to_confirm_once_during_a_candidate() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            pre_roll_frames: 0,
            hold_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            !gate
                .process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                .unwrap()
                .open
        );
        assert!(
            !gate
                .process_frame_with_secondary_vad(1.0, Some(true), &mut samples)
                .unwrap()
                .open
        );
        assert!(
            gate.process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                .unwrap()
                .open
        );
    }

    #[test]
    fn primary_and_secondary_vad_may_confirm_on_different_frames() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            pre_roll_frames: 0,
            hold_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            !gate
                .process_frame_with_secondary_vad(0.0, Some(true), &mut samples)
                .unwrap()
                .open
        );
        assert!(
            !gate
                .process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                .unwrap()
                .open
        );
        assert!(
            gate.process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                .unwrap()
                .open
        );
    }

    #[test]
    fn secondary_vad_rejects_an_unconfirmed_candidate() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            pre_roll_frames: 0,
            hold_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];

        for _ in 0..4 {
            assert!(
                !gate
                    .process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                    .unwrap()
                    .open
            );
        }
    }

    #[test]
    fn a_candidate_gap_clears_secondary_vad_confirmation() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            pre_roll_frames: 0,
            hold_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            !gate
                .process_frame_with_secondary_vad(1.0, Some(true), &mut samples)
                .unwrap()
                .open
        );
        assert!(
            !gate
                .process_frame_with_secondary_vad(0.0, Some(false), &mut samples)
                .unwrap()
                .open
        );
        for _ in 0..3 {
            assert!(
                !gate
                    .process_frame_with_secondary_vad(1.0, Some(false), &mut samples)
                    .unwrap()
                    .open
            );
        }
    }

    #[test]
    fn default_gate_rejects_candidates_shorter_than_120_ms() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            pre_roll_frames: 0,
            hold_frames: 0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [0.5; DSP_SAMPLES_PER_FRAME];

        for _ in 0..11 {
            assert!(!gate.process_frame(1.0, &mut samples).unwrap().open);
        }
        assert!(gate.process_frame(1.0, &mut samples).unwrap().open);
    }

    #[test]
    fn enabled_gate_starts_at_its_closed_floor() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            pre_roll_frames: 0,
            floor_db: -90.0,
            ..VadGateConfig::default()
        })
        .unwrap();
        let mut samples = [1.0; DSP_SAMPLES_PER_FRAME];

        let metrics = gate.process_frame(0.0, &mut samples).unwrap();

        assert!(!metrics.open);
        assert!(metrics.final_gain <= db_to_gain(-89.9));
        assert!(samples.iter().all(|sample| sample.abs() < 0.000_1));
    }

    #[test]
    fn gate_preserves_preroll_after_confirmation() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            floor_db: -90.0,
            attack_ms: 0.1,
            release_ms: 0.1,
            ..VadGateConfig::default()
        })
        .unwrap();

        for expected in [0.1, 0.2] {
            let mut samples = [expected; DSP_SAMPLES_PER_FRAME];
            assert!(!gate.process_frame(1.0, &mut samples).unwrap().open);
        }
        let mut samples = [0.3; DSP_SAMPLES_PER_FRAME];
        assert!(gate.process_frame(1.0, &mut samples).unwrap().open);
        assert!((samples[DSP_SAMPLES_PER_FRAME - 1] - 0.1).abs() < 0.001);
    }

    #[test]
    fn gate_can_preserve_preroll_longer_than_confirmation() {
        let mut gate = VadGate::new(VadGateConfig {
            enabled: true,
            confirmation_frames: 3,
            pre_roll_frames: 6,
            floor_db: -90.0,
            attack_ms: 0.1,
            release_ms: 0.1,
            ..VadGateConfig::default()
        })
        .unwrap();

        for frame in 1..=6 {
            let mut samples = [frame as f32 / 10.0; DSP_SAMPLES_PER_FRAME];
            let probability = if frame >= 5 { 1.0 } else { 0.0 };
            assert!(!gate.process_frame(probability, &mut samples).unwrap().open);
        }
        let mut samples = [0.7; DSP_SAMPLES_PER_FRAME];
        assert!(gate.process_frame(1.0, &mut samples).unwrap().open);
        assert!((samples[DSP_SAMPLES_PER_FRAME - 1] - 0.1).abs() < 0.001);
    }

    #[test]
    fn echo_guard_blocks_vad_when_aec_attenuates_correlated_audio() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let output = [0.2; DSP_SAMPLES_PER_FRAME];

        let metrics = guard
            .process_frame(0.9, Some(0.8), &input, &output)
            .unwrap();

        assert!(metrics.active);
        assert_eq!(metrics.effective_vad_probability, 0.0);
        assert!(metrics.attenuation_db.unwrap() < -5.5);
    }

    #[test]
    fn echo_guard_preserves_nearend_when_aec_output_remains_strong() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let output = [0.45; DSP_SAMPLES_PER_FRAME];

        let metrics = guard
            .process_frame(0.9, Some(0.8), &input, &output)
            .unwrap();

        assert!(!metrics.active);
        assert_eq!(metrics.effective_vad_probability, 0.9);
    }

    #[test]
    fn echo_guard_requires_residual_echo_evidence() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let output = [0.2; DSP_SAMPLES_PER_FRAME];

        let metrics = guard.process_frame(0.9, None, &input, &output).unwrap();

        assert!(!metrics.active);
        assert_eq!(metrics.effective_vad_probability, 0.9);
    }

    #[test]
    fn echo_guard_uses_strong_attenuation_without_likelihood() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let output = [0.1; DSP_SAMPLES_PER_FRAME];

        let metrics = guard.process_frame(0.9, None, &input, &output).unwrap();

        assert!(metrics.active);
        assert_eq!(metrics.effective_vad_probability, 0.0);
    }

    #[test]
    fn echo_guard_holds_through_short_evidence_gaps() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            hold_frames: 2,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let echo_output = [0.2; DSP_SAMPLES_PER_FRAME];
        let gap_output = [0.45; DSP_SAMPLES_PER_FRAME];

        assert!(
            guard
                .process_frame(0.1, Some(0.8), &input, &echo_output)
                .unwrap()
                .active
        );
        assert!(
            guard
                .process_frame(0.1, None, &input, &gap_output)
                .unwrap()
                .active
        );
        assert!(
            guard
                .process_frame(0.1, None, &input, &gap_output)
                .unwrap()
                .active
        );
        assert!(
            !guard
                .process_frame(0.1, None, &input, &gap_output)
                .unwrap()
                .active
        );
    }

    #[test]
    fn echo_guard_requires_sustained_nearend_evidence_to_release() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            nearend_confirmation_frames: 3,
            smoothing_factor: 0.0,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let echo_output = [0.2; DSP_SAMPLES_PER_FRAME];
        let nearend_output = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            guard
                .process_frame(0.1, Some(0.8), &input, &echo_output)
                .unwrap()
                .active
        );
        for _ in 0..2 {
            let metrics = guard
                .process_frame(0.9, Some(0.1), &input, &nearend_output)
                .unwrap();
            assert!(metrics.active);
            assert!(metrics.nearend_candidate);
            assert!(!metrics.released);
        }
        let metrics = guard
            .process_frame(0.9, Some(0.1), &input, &nearend_output)
            .unwrap();

        assert!(!metrics.active);
        assert_eq!(metrics.effective_vad_probability, 0.9);
        assert!(metrics.released);
    }

    #[test]
    fn echo_guard_rejects_interrupted_keyboard_like_candidates() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            nearend_confirmation_frames: 6,
            smoothing_factor: 0.0,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let echo_output = [0.2; DSP_SAMPLES_PER_FRAME];
        let nearend_output = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            guard
                .process_frame(0.1, Some(0.8), &input, &echo_output)
                .unwrap()
                .active
        );
        for _ in 0..5 {
            let metrics = guard
                .process_frame(0.9, Some(0.1), &input, &nearend_output)
                .unwrap();
            assert!(metrics.active);
            assert!(metrics.nearend_candidate);
            assert!(!metrics.released);
        }
        assert!(
            guard
                .process_frame(0.1, None, &input, &nearend_output)
                .unwrap()
                .active
        );
        for _ in 0..5 {
            let metrics = guard
                .process_frame(0.9, Some(0.1), &input, &nearend_output)
                .unwrap();
            assert!(metrics.active);
            assert!(metrics.nearend_candidate);
            assert!(!metrics.released);
        }
    }

    #[test]
    fn echo_guard_rejects_sustained_high_vad_residual_echo() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            hold_frames: 2,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let echo_output = [0.2; DSP_SAMPLES_PER_FRAME];
        let weakly_attenuated_echo = [0.43; DSP_SAMPLES_PER_FRAME];

        assert!(
            guard
                .process_frame(0.1, Some(0.8), &input, &echo_output)
                .unwrap()
                .active
        );
        for _ in 0..100 {
            let metrics = guard
                .process_frame(0.99, Some(0.4), &input, &weakly_attenuated_echo)
                .unwrap();
            assert!(metrics.active);
            assert!(!metrics.nearend_candidate);
            assert!(!metrics.released);
            assert_eq!(metrics.effective_vad_probability, 0.0);
        }
    }

    #[test]
    fn echo_guard_releases_dominant_nearend_despite_stale_echo_likelihood() {
        let mut guard = EchoGuard::new(EchoGuardConfig {
            enabled: true,
            nearend_confirmation_frames: 3,
            smoothing_factor: 0.0,
            ..EchoGuardConfig::default()
        })
        .unwrap();
        let input = [0.5; DSP_SAMPLES_PER_FRAME];
        let echo_output = [0.2; DSP_SAMPLES_PER_FRAME];
        let nearend_output = [0.5; DSP_SAMPLES_PER_FRAME];

        assert!(
            guard
                .process_frame(0.1, Some(0.8), &input, &echo_output)
                .unwrap()
                .active
        );
        for _ in 0..2 {
            let metrics = guard
                .process_frame(0.99, Some(0.8), &input, &nearend_output)
                .unwrap();
            assert!(metrics.active);
            assert!(metrics.nearend_candidate);
            assert!(!metrics.released);
        }
        let metrics = guard
            .process_frame(0.99, Some(0.8), &input, &nearend_output)
            .unwrap();

        assert!(!metrics.active);
        assert!(metrics.released);
        assert_eq!(metrics.effective_vad_probability, 0.99);
    }
}
