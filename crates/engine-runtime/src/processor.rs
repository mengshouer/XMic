use std::{path::Path, sync::Arc};

use xmic_engine_core::{
    EchoGuard, EchoGuardConfig, SafetyLimiter, SafetyLimiterConfig, VadGate, VadGateConfig,
    DSP_FRAME_DURATION_MS, DSP_SAMPLES_PER_FRAME, MAX_RENDER_CHANNELS,
};
use xmic_engine_protocol::{AecPreset, DspSettings};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp};
use xmic_webrtc_dsp::{
    AecPreset as NativeAecPreset, Agc2Config, DspConfig, DspMetrics, WebRtcAgc2, WebRtcDsp,
    WebRtcLibrary,
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
    /// RNNoise emits audio/VAD for the previous 10 ms frame. Delay the raw/AEC power pair by the
    /// same frame so EchoGuard never compares a VAD decision with the following frame's attenuation.
    echo_guard_input: [f32; DSP_SAMPLES_PER_FRAME],
    echo_guard_aec_output: [f32; DSP_SAMPLES_PER_FRAME],
    echo_guard_residual_likelihood: Option<f32>,
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
        let library = if settings.aec.enabled || settings.agc2.enabled {
            Some(WebRtcLibrary::load(native_library).map_err(|failure| failure.error)?)
        } else {
            None
        };
        Self::build(library, render_channels, settings)
    }

    pub fn from_library(
        native_library: Arc<WebRtcLibrary>,
        render_channels: usize,
        settings: &DspSettings,
    ) -> Result<Self, DspProcessorError> {
        Self::build(Some(native_library), render_channels, settings)
    }

    fn build(
        native_library: Option<Arc<WebRtcLibrary>>,
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
                WebRtcDsp::from_library(
                    Arc::clone(native_library.as_ref().expect("validated native library")),
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
                WebRtcAgc2::from_library(
                    Arc::clone(native_library.as_ref().expect("validated native library")),
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
            gain_db: settings.limiter.gain_db,
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
            echo_guard_input: [0.0; DSP_SAMPLES_PER_FRAME],
            echo_guard_aec_output: [0.0; DSP_SAMPLES_PER_FRAME],
            echo_guard_residual_likelihood: None,
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

        if let Some(agc2) = &mut self.agc2 {
            agc2.process_capture(&self.rnnoise_output, &mut self.post_agc_output)?;
        } else {
            self.post_agc_output.copy_from_slice(&self.rnnoise_output);
        }
        let echo_guard_vad_probability =
            echo_guard_vad_probability(vad_probability, speech_vad_active);
        let echo_guard_metrics = self.echo_guard.process_frame(
            echo_guard_vad_probability,
            self.echo_guard_residual_likelihood,
            &self.echo_guard_input,
            &self.echo_guard_aec_output,
        )?;
        self.echo_guard_input.copy_from_slice(capture);
        self.echo_guard_aec_output.copy_from_slice(&self.aec_output);
        self.echo_guard_residual_likelihood = residual_echo_likelihood;
        let gate_vad_probability = if echo_guard_metrics.active {
            0.0
        } else {
            vad_probability
        };
        let gate_metrics = self.gate.process_frame_with_secondary_vad_and_veto(
            gate_vad_probability,
            speech_vad_active,
            echo_guard_metrics.active,
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

    pub fn aec_output(&self) -> &[f32; DSP_SAMPLES_PER_FRAME] {
        &self.aec_output
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
        // Every decision state that was aligned with the old echo path must reset together. Keeping
        // old attenuation smoothing, VAD hold or delayed power frames after the native AEC rebuild
        // can open the gate on evidence from before the reset.
        self.echo_guard.reset();
        self.gate.reset();
        self.echo_guard_input.fill(0.0);
        self.echo_guard_aec_output.fill(0.0);
        self.echo_guard_residual_likelihood = None;
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
    fn processing_latency_matches_each_preset() {
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
        let library = WebRtcLibrary::load(&native_library).unwrap();
        let mut processor =
            DspProcessor::from_library(library, 2, &DspSettings::default()).unwrap();
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

    #[test]
    fn continuous_render_frames_allow_aec3_to_cancel_delayed_echo_across_pauses() {
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
        settings.rnnoise.enabled = false;
        settings.agc2.enabled = false;
        settings.vad_gate.enabled = false;
        let mut processor = DspProcessor::load(&native_library, 1, &settings).unwrap();

        let echo_delay_frames = 5usize; // 50 ms acoustic delay
        let total_frames = 300usize; // 3 seconds
        let silence_frame = [0.0f32; DSP_SAMPLES_PER_FRAME];
        let mut render_history = Vec::with_capacity(total_frames);
        let mut raw_tail_energy = 0.0f64;
        let mut processed_tail_energy = 0.0f64;

        for frame_index in 0..total_frames {
            // Far-end speaks in bursts (frames 0..150 and 180..280) and is silent in between/after.
            let speaking = (0..150).contains(&frame_index) || (180..280).contains(&frame_index);
            let mut render_frame = silence_frame;
            if speaking {
                for (sample_idx, sample) in render_frame.iter_mut().enumerate() {
                    let global_sample = (frame_index * DSP_SAMPLES_PER_FRAME + sample_idx) as u64;
                    // Deterministic broadband speech-like excitation so AEC3's matched-filter delay
                    // estimator and subband adaptive filter have full-band spectral content.
                    let noise = ((global_sample
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407)
                        >> 33) as i32 as f32)
                        / (i32::MAX as f32);
                    let envelope = 0.5
                        + 0.4
                            * ((global_sample as f32 / 48_000.0) * 6.0 * std::f32::consts::TAU)
                                .sin();
                    *sample = noise * envelope * 0.35;
                }
            }
            render_history.push(render_frame);

            let capture_frame = if frame_index >= echo_delay_frames {
                let mut delayed = render_history[frame_index - echo_delay_frames];
                for sample in &mut delayed {
                    *sample *= 0.5;
                }
                delayed
            } else {
                silence_frame
            };

            let mut output = [0.0f32; DSP_SAMPLES_PER_FRAME];
            // Even when speaking == false (WASAPI loopback idle), feeding a zero render frame
            // advances WebRTC's RenderDelayBuffer so the 50 ms tail echo in frames 250..255 is cancelled.
            processor.process_render(&render_frame).unwrap();
            processor
                .process_capture(&capture_frame, &mut output, 0)
                .unwrap();

            if (220..285).contains(&frame_index) {
                raw_tail_energy += capture_frame
                    .iter()
                    .map(|&s| f64::from(s) * f64::from(s))
                    .sum::<f64>();
                processed_tail_energy += output
                    .iter()
                    .map(|&s| f64::from(s) * f64::from(s))
                    .sum::<f64>();
            }
        }

        assert!(
            processed_tail_energy < raw_tail_energy * 0.05,
            "expected >13 dB echo attenuation across active speech and pause tail (raw={raw_tail_energy}, processed={processed_tail_energy})"
        );
    }
}
