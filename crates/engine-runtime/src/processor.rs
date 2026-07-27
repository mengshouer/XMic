use std::{collections::VecDeque, path::Path};

use xmic_engine_core::{
    EchoGuard, EchoGuardConfig, SafetyLimiter, SafetyLimiterConfig, VadGate, VadGateConfig,
    DSP_FRAME_DURATION_MS, DSP_SAMPLES_PER_FRAME, MAX_RENDER_CHANNELS,
};
use xmic_engine_protocol::{AecPreset, DspSettings};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp};
use xmic_webrtc_dsp::{
    AecPreset as NativeAecPreset, Agc2Config, DspConfig, DspMetrics, WebRtcAgc2, WebRtcDsp,
};

const ECHO_GUARD_SMOOTHING_PRE_ROLL_FRAMES: usize = 2;
const SPEECH_ONSET_PRE_ROLL_FRAMES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DspFrameMetrics {
    pub vad_probability: f32,
    pub speech_vad_active: Option<bool>,
    pub gate_open: bool,
    pub residual_echo_likelihood: Option<f32>,
    pub aec_attenuation_db: Option<f32>,
    pub echo_guard_active: bool,
    pub echo_guard_nearend_candidate: bool,
    pub echo_guard_released: bool,
    pub limited_samples: usize,
}

pub struct DspProcessor {
    render_channels: usize,
    aec: Option<WebRtcDsp>,
    rnnoise: Option<RnnoiseDsp>,
    agc2: Option<WebRtcAgc2>,
    speech_vad_enabled: bool,
    echo_guard: EchoGuard,
    gate: VadGate,
    limiter: SafetyLimiter,
    aec_output: [f32; DSP_SAMPLES_PER_FRAME],
    rnnoise_output: [f32; DSP_SAMPLES_PER_FRAME],
    post_agc_output: [f32; DSP_SAMPLES_PER_FRAME],
}

impl DspProcessor {
    pub fn load(
        native_library: &Path,
        render_channels: usize,
        settings: &DspSettings,
    ) -> Result<Self, DspProcessorError> {
        if !(1..=MAX_RENDER_CHANNELS).contains(&render_channels) {
            return Err(DspProcessorError::UnsupportedRenderChannels(
                render_channels,
            ));
        }
        if settings.vad_gate.enabled && !settings.rnnoise.enabled {
            return Err(DspProcessorError::VadRequiresRnnoise);
        }

        let aec = settings
            .aec
            .enabled
            .then(|| {
                WebRtcDsp::load(
                    native_library,
                    DspConfig {
                        render_channels,
                        aec_preset: map_aec_preset(settings.aec.preset),
                        high_pass_filter_enabled: settings.aec.high_pass_filter_enabled,
                    },
                )
            })
            .transpose()?;
        let rnnoise = settings
            .rnnoise
            .enabled
            .then(|| {
                RnnoiseDsp::new(RnnoiseConfig {
                    mix: settings.rnnoise.mix,
                })
            })
            .transpose()?;
        let agc2 = settings
            .agc2
            .enabled
            .then(|| {
                WebRtcAgc2::load(
                    native_library,
                    Agc2Config {
                        adaptive_digital_enabled: settings.agc2.adaptive_digital_enabled,
                        fixed_gain_db: settings.agc2.fixed_gain_db,
                        headroom_db: settings.agc2.headroom_db,
                        max_gain_db: settings.agc2.max_gain_db,
                        initial_gain_db: settings.agc2.initial_gain_db,
                        max_gain_change_db_per_second: settings.agc2.max_gain_change_db_per_second,
                        max_output_noise_level_dbfs: settings.agc2.max_output_noise_level_dbfs,
                    },
                )
            })
            .transpose()?;
        let hold_frames = usize::try_from(settings.vad_gate.hold_ms)
            .map_err(|_| DspProcessorError::VadHoldOverflow)?
            .div_ceil(DSP_FRAME_DURATION_MS);
        let confirmation_frames = usize::try_from(settings.vad_gate.confirmation_ms)
            .map_err(|_| DspProcessorError::VadConfirmationOverflow)?
            .div_ceil(DSP_FRAME_DURATION_MS);
        let echo_guard_config = EchoGuardConfig {
            enabled: settings.aec.enabled
                && settings.vad_gate.enabled
                && settings.aec.preset == AecPreset::Strong,
            ..EchoGuardConfig::default()
        };
        let pre_roll_frames = gate_pre_roll_frames(
            settings.vad_gate.enabled,
            confirmation_frames,
            echo_guard_config,
        );
        let speech_vad_enabled = settings.aec.enabled && settings.vad_gate.enabled;
        let echo_guard = EchoGuard::new(echo_guard_config)?;
        let gate = VadGate::new(VadGateConfig {
            enabled: settings.vad_gate.enabled,
            threshold: settings.vad_gate.threshold,
            confirmation_frames,
            pre_roll_frames,
            hold_frames,
            floor_db: settings.vad_gate.floor_db,
            attack_ms: settings.vad_gate.attack_ms,
            release_ms: settings.vad_gate.release_ms,
        })?;
        let limiter = SafetyLimiter::new(SafetyLimiterConfig {
            ceiling_dbfs: settings.limiter.ceiling_dbfs,
            release_ms: settings.limiter.release_ms,
        })?;

        Ok(Self {
            render_channels,
            aec,
            rnnoise,
            agc2,
            speech_vad_enabled,
            echo_guard,
            gate,
            limiter,
            aec_output: [0.0; DSP_SAMPLES_PER_FRAME],
            rnnoise_output: [0.0; DSP_SAMPLES_PER_FRAME],
            post_agc_output: [0.0; DSP_SAMPLES_PER_FRAME],
        })
    }

    pub fn process_frame(
        &mut self,
        render_interleaved: &[f32],
        capture: &[f32],
        output: &mut [f32],
        stream_delay_ms: i32,
    ) -> Result<DspFrameMetrics, DspProcessorError> {
        self.process_render(render_interleaved)?;
        self.process_capture(capture, output, stream_delay_ms)
    }

    pub fn process_render(&mut self, render_interleaved: &[f32]) -> Result<(), DspProcessorError> {
        let expected_render = DSP_SAMPLES_PER_FRAME * self.render_channels;
        if render_interleaved.len() != expected_render {
            return Err(DspProcessorError::InvalidRenderFrameLength {
                expected: expected_render,
                actual: render_interleaved.len(),
            });
        }
        if let Some(aec) = &mut self.aec {
            aec.process_render(render_interleaved)?;
        }
        Ok(())
    }

    pub fn process_capture(
        &mut self,
        capture: &[f32],
        output: &mut [f32],
        stream_delay_ms: i32,
    ) -> Result<DspFrameMetrics, DspProcessorError> {
        self.process_capture_with_options(capture, output, stream_delay_ms, false)
    }

    pub fn process_capture_with_options(
        &mut self,
        capture: &[f32],
        output: &mut [f32],
        stream_delay_ms: i32,
        bypass_agc2: bool,
    ) -> Result<DspFrameMetrics, DspProcessorError> {
        validate_capture_lengths(capture, output)?;

        let residual_echo_likelihood = if let Some(aec) = &mut self.aec {
            aec.process_capture(capture, &mut self.aec_output, stream_delay_ms)?;
            if self.echo_guard.enabled() {
                aec.metrics()?.residual_echo_likelihood
            } else {
                None
            }
        } else {
            self.aec_output.copy_from_slice(capture);
            None
        };

        let vad_probability = if let Some(rnnoise) = &mut self.rnnoise {
            rnnoise
                .process_frame(&self.aec_output, &mut self.rnnoise_output)?
                .vad_probability
        } else {
            self.rnnoise_output.copy_from_slice(&self.aec_output);
            1.0
        };
        let speech_vad_active = if self.speech_vad_enabled {
            self.aec
                .as_mut()
                .map(|aec| aec.detect_speech(&self.rnnoise_output))
                .transpose()?
        } else {
            None
        };

        if bypass_agc2 {
            self.post_agc_output.copy_from_slice(&self.rnnoise_output);
        } else if let Some(agc2) = &mut self.agc2 {
            agc2.process_capture(&self.rnnoise_output, &mut self.post_agc_output)?;
        } else {
            self.post_agc_output.copy_from_slice(&self.rnnoise_output);
        }
        let echo_guard_vad_probability =
            echo_guard_vad_probability(vad_probability, speech_vad_active);
        let echo_guard_metrics = self.echo_guard.process_frame(
            echo_guard_vad_probability,
            residual_echo_likelihood,
            capture,
            &self.aec_output,
        )?;
        let gate_vad_probability = if echo_guard_metrics.active {
            0.0
        } else {
            vad_probability
        };
        let gate_metrics = self.gate.process_frame_with_secondary_vad(
            gate_vad_probability,
            speech_vad_active,
            &mut self.post_agc_output,
        )?;
        let limiter_metrics = self.limiter.process_frame(&mut self.post_agc_output)?;
        output.copy_from_slice(&self.post_agc_output);

        Ok(DspFrameMetrics {
            vad_probability,
            speech_vad_active,
            gate_open: gate_metrics.open,
            residual_echo_likelihood,
            aec_attenuation_db: echo_guard_metrics.attenuation_db,
            echo_guard_active: echo_guard_metrics.active,
            echo_guard_nearend_candidate: echo_guard_metrics.nearend_candidate,
            echo_guard_released: echo_guard_metrics.released,
            limited_samples: limiter_metrics.limited_samples,
        })
    }

    pub fn render_channels(&self) -> usize {
        self.render_channels
    }

    pub fn aec_metrics(&self) -> Result<Option<DspMetrics>, DspProcessorError> {
        self.aec
            .as_ref()
            .map(WebRtcDsp::metrics)
            .transpose()
            .map_err(Into::into)
    }

    pub fn reset_echo_path(&mut self) -> Result<(), DspProcessorError> {
        if let Some(aec) = &mut self.aec {
            aec.reset_echo_path()?;
        }
        Ok(())
    }

    pub fn reset_agc2(&mut self) -> Result<(), DspProcessorError> {
        if let Some(agc2) = &mut self.agc2 {
            agc2.reset()?;
        }
        Ok(())
    }
}

pub fn processing_latency_frames(settings: &DspSettings) -> Result<usize, DspProcessorError> {
    let confirmation_frames = usize::try_from(settings.vad_gate.confirmation_ms)
        .map_err(|_| DspProcessorError::VadConfirmationOverflow)?
        .div_ceil(DSP_FRAME_DURATION_MS);
    let echo_guard_config = EchoGuardConfig {
        enabled: settings.aec.enabled
            && settings.vad_gate.enabled
            && settings.aec.preset == AecPreset::Strong,
        ..EchoGuardConfig::default()
    };
    let gate_frames = gate_pre_roll_frames(
        settings.vad_gate.enabled,
        confirmation_frames,
        echo_guard_config,
    );
    Ok(usize::from(settings.rnnoise.enabled).saturating_add(gate_frames))
}

fn gate_pre_roll_frames(
    enabled: bool,
    confirmation_frames: usize,
    echo_guard_config: EchoGuardConfig,
) -> usize {
    if !enabled {
        return 0;
    }
    let confirmation_pre_roll = confirmation_frames.saturating_sub(1);
    let echo_guard_pre_roll = if echo_guard_config.enabled {
        echo_guard_config
            .nearend_confirmation_frames
            .saturating_sub(1)
            .saturating_add(ECHO_GUARD_SMOOTHING_PRE_ROLL_FRAMES)
    } else {
        0
    };
    confirmation_pre_roll
        .saturating_add(SPEECH_ONSET_PRE_ROLL_FRAMES)
        .saturating_add(echo_guard_pre_roll)
}

fn validate_capture_lengths(capture: &[f32], output: &[f32]) -> Result<(), DspProcessorError> {
    if capture.len() == DSP_SAMPLES_PER_FRAME && output.len() == DSP_SAMPLES_PER_FRAME {
        Ok(())
    } else {
        Err(DspProcessorError::InvalidCaptureFrameLength {
            expected: DSP_SAMPLES_PER_FRAME,
            capture: capture.len(),
            output: output.len(),
        })
    }
}

fn echo_guard_vad_probability(vad_probability: f32, speech_vad_active: Option<bool>) -> f32 {
    if speech_vad_active == Some(false) {
        0.0
    } else {
        vad_probability
    }
}

pub struct AlignedRawPath {
    delayed: VecDeque<[f32; DSP_SAMPLES_PER_FRAME]>,
}

impl AlignedRawPath {
    pub fn new(delay_frames: usize) -> Self {
        let mut delayed = VecDeque::with_capacity(delay_frames);
        delayed.resize(delay_frames, [0.0; DSP_SAMPLES_PER_FRAME]);
        Self { delayed }
    }

    pub fn process_frame(&mut self, input: &[f32], output: &mut [f32]) -> bool {
        if input.len() != DSP_SAMPLES_PER_FRAME || output.len() != DSP_SAMPLES_PER_FRAME {
            return false;
        }
        if let Some(delayed) = self.delayed.pop_front() {
            output.copy_from_slice(&delayed);
            let mut current = [0.0; DSP_SAMPLES_PER_FRAME];
            current.copy_from_slice(input);
            self.delayed.push_back(current);
        } else {
            output.copy_from_slice(input);
        }
        true
    }
}

fn map_aec_preset(preset: AecPreset) -> NativeAecPreset {
    match preset {
        AecPreset::Natural => NativeAecPreset::Natural,
        AecPreset::Balanced => NativeAecPreset::Balanced,
        AecPreset::Strong => NativeAecPreset::Strong,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DspProcessorError {
    #[error("render channel count {0} is unsupported")]
    UnsupportedRenderChannels(usize),
    #[error("VAD gate requires RNNoise because RNNoise supplies its VAD probability")]
    VadRequiresRnnoise,
    #[error("VAD hold duration cannot be represented on this platform")]
    VadHoldOverflow,
    #[error("VAD confirmation duration cannot be represented on this platform")]
    VadConfirmationOverflow,
    #[error("render frame has {actual} samples; expected {expected}")]
    InvalidRenderFrameLength { expected: usize, actual: usize },
    #[error("capture/output frame lengths are {capture}/{output}; expected {expected}")]
    InvalidCaptureFrameLength {
        expected: usize,
        capture: usize,
        output: usize,
    },
    #[error(transparent)]
    Native(#[from] xmic_webrtc_dsp::DspError),
    #[error(transparent)]
    Rnnoise(#[from] xmic_rnnoise_dsp::RnnoiseError),
    #[error(transparent)]
    PostProcess(#[from] xmic_engine_core::PostProcessConfigError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_frame_lengths_before_loading_native_code() {
        let settings = DspSettings::default();
        let result = DspProcessor::load(Path::new("missing.dll"), 3, &settings);
        assert!(matches!(
            result,
            Err(DspProcessorError::UnsupportedRenderChannels(3))
        ));
    }

    #[test]
    fn secondary_vad_vetoes_echo_guard_false_positive() {
        assert_eq!(echo_guard_vad_probability(0.99, Some(false)), 0.0);
        assert_eq!(echo_guard_vad_probability(0.99, Some(true)), 0.99);
        assert_eq!(echo_guard_vad_probability(0.99, None), 0.99);
    }

    #[test]
    fn rejects_gate_without_vad_source_before_loading_native_code() {
        let mut settings = DspSettings::default();
        settings.aec.enabled = false;
        settings.rnnoise.enabled = false;
        settings.agc2.enabled = false;
        let result = DspProcessor::load(Path::new("missing.dll"), 1, &settings);
        assert!(matches!(result, Err(DspProcessorError::VadRequiresRnnoise)));
    }

    #[test]
    fn maximum_vad_confirmation_is_supported() {
        let mut settings = DspSettings::default();
        settings.aec.enabled = false;
        settings.agc2.enabled = false;
        settings.vad_gate.confirmation_ms = 1_000;

        let processor = DspProcessor::load(Path::new("missing.dll"), 1, &settings);

        assert!(processor.is_ok());
    }

    #[test]
    fn bypass_chain_is_sample_transparent() {
        let mut settings = DspSettings::default();
        settings.aec.enabled = false;
        settings.rnnoise.enabled = false;
        settings.agc2.enabled = false;
        settings.vad_gate.enabled = false;
        settings.limiter.ceiling_dbfs = 0.0;
        let mut processor = DspProcessor::load(Path::new("missing.dll"), 1, &settings).unwrap();
        let render = [0.0; DSP_SAMPLES_PER_FRAME];
        let capture = [0.25; DSP_SAMPLES_PER_FRAME];
        let mut output = [0.0; DSP_SAMPLES_PER_FRAME];

        processor
            .process_frame(&render, &capture, &mut output, 0)
            .unwrap();

        assert_eq!(output, capture);
    }

    #[test]
    fn reset_echo_path_is_safe_when_aec_is_disabled() {
        let mut settings = DspSettings::default();
        settings.aec.enabled = false;
        settings.rnnoise.enabled = false;
        settings.agc2.enabled = false;
        settings.vad_gate.enabled = false;
        let mut processor = DspProcessor::load(Path::new("missing.dll"), 1, &settings).unwrap();

        processor.reset_echo_path().unwrap();
    }

    #[test]
    fn aligned_raw_path_delays_one_frame_when_requested() {
        let mut path = AlignedRawPath::new(1);
        let first = [0.25; DSP_SAMPLES_PER_FRAME];
        let second = [-0.5; DSP_SAMPLES_PER_FRAME];
        let mut output = [1.0; DSP_SAMPLES_PER_FRAME];

        assert!(path.process_frame(&first, &mut output));
        assert_eq!(output, [0.0; DSP_SAMPLES_PER_FRAME]);
        assert!(path.process_frame(&second, &mut output));
        assert_eq!(output, first);
    }

    #[test]
    fn raw_fallback_latency_matches_each_preset() {
        let natural = DspSettings::default();
        let mut strong = natural.clone();
        strong.aec.preset = AecPreset::Strong;

        assert_eq!(processing_latency_frames(&natural).unwrap(), 8);
        assert_eq!(processing_latency_frames(&strong).unwrap(), 15);
    }

    #[test]
    fn presets_without_echo_guard_do_not_pay_guard_latency() {
        let natural = DspSettings::default();
        let mut balanced = natural.clone();
        balanced.aec.preset = AecPreset::Balanced;
        let mut strong = natural.clone();
        strong.aec.preset = AecPreset::Strong;

        assert_eq!(processing_latency_frames(&natural).unwrap(), 8);
        assert_eq!(processing_latency_frames(&balanced).unwrap(), 8);
        assert_eq!(processing_latency_frames(&strong).unwrap(), 15);
    }

    #[test]
    fn gate_latency_keeps_audio_before_the_first_speech_candidate() {
        let echo_guard = EchoGuardConfig::default();

        assert_eq!(gate_pre_roll_frames(true, 12, echo_guard), 15);
        assert_eq!(gate_pre_roll_frames(false, 12, echo_guard), 0);
    }

    #[test]
    fn accepted_chain_processes_silence_with_bundled_native_library() {
        let native_library = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("artifacts")
            .join("webrtc")
            .join("win-x64")
            .join("xmic_webrtc_dsp.dll");
        if !native_library.is_file() {
            return;
        }
        let mut processor =
            DspProcessor::load(&native_library, 2, &DspSettings::default()).unwrap();
        let render = [0.0; DSP_SAMPLES_PER_FRAME * 2];
        let capture = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut output = [1.0; DSP_SAMPLES_PER_FRAME];

        processor
            .process_frame(&render, &capture, &mut output, 0)
            .unwrap();
        processor
            .process_frame(&render, &capture, &mut output, 0)
            .unwrap();

        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn natural_chain_uses_secondary_speech_vad() {
        let native_library = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("artifacts")
            .join("webrtc")
            .join("win-x64")
            .join("xmic_webrtc_dsp.dll");
        if !native_library.is_file() {
            return;
        }
        let mut settings = DspSettings::default();
        settings.aec.preset = AecPreset::Natural;
        let mut processor = DspProcessor::load(&native_library, 2, &settings).unwrap();
        let render = [0.0; DSP_SAMPLES_PER_FRAME * 2];
        let capture = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut output = [1.0; DSP_SAMPLES_PER_FRAME];

        let metrics = processor
            .process_frame(&render, &capture, &mut output, 0)
            .unwrap();

        assert_eq!(metrics.speech_vad_active, Some(false));
        assert!(!metrics.gate_open);
        assert!(!metrics.echo_guard_active);
    }

    #[test]
    fn strong_chain_exposes_residual_echo_likelihood() {
        let native_library = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("artifacts")
            .join("webrtc")
            .join("win-x64")
            .join("xmic_webrtc_dsp.dll");
        if !native_library.is_file() {
            return;
        }
        let mut settings = DspSettings::default();
        settings.aec.preset = AecPreset::Strong;
        let mut processor = DspProcessor::load(&native_library, 2, &settings).unwrap();
        let render = [0.0; DSP_SAMPLES_PER_FRAME * 2];
        let capture = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut output = [1.0; DSP_SAMPLES_PER_FRAME];

        let mut frame_metrics = None;
        for _ in 0..2 {
            frame_metrics = Some(
                processor
                    .process_frame(&render, &capture, &mut output, 0)
                    .unwrap(),
            );
        }

        assert!(processor
            .aec_metrics()
            .unwrap()
            .unwrap()
            .residual_echo_likelihood
            .is_some());
        assert_eq!(frame_metrics.unwrap().speech_vad_active, Some(false));
    }
}
