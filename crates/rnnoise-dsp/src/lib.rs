#![forbid(unsafe_code)]

use nnnoiseless::DenoiseState;

pub const SAMPLE_RATE_HZ: usize = 48_000;
pub const FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;
pub const LATENCY_SAMPLES: usize = FRAME_SAMPLES;

const PCM_16_SCALE: f32 = 32_768.0;
const PCM_16_MAX: f32 = 32_767.0;

#[derive(Debug, Clone, Copy)]
pub struct RnnoiseConfig {
    pub mix: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RnnoiseFrameMetrics {
    pub vad_probability: f32,
}

pub struct RnnoiseDsp {
    state: Box<DenoiseState<'static>>,
    mix: f32,
    first_frame: bool,
    delayed_vad_probability: f32,
    delayed_dry: [f32; FRAME_SAMPLES],
    scaled_input: [f32; FRAME_SAMPLES],
    denoised_output: [f32; FRAME_SAMPLES],
}

impl RnnoiseDsp {
    pub fn new(config: RnnoiseConfig) -> Result<Self, RnnoiseError> {
        if !config.mix.is_finite() || !(0.0..=1.0).contains(&config.mix) {
            return Err(RnnoiseError::InvalidMix(config.mix));
        }
        Ok(Self {
            state: DenoiseState::new(),
            mix: config.mix,
            first_frame: true,
            delayed_vad_probability: 0.0,
            delayed_dry: [0.0; FRAME_SAMPLES],
            scaled_input: [0.0; FRAME_SAMPLES],
            denoised_output: [0.0; FRAME_SAMPLES],
        })
    }

    pub fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<RnnoiseFrameMetrics, RnnoiseError> {
        if input.len() != FRAME_SAMPLES || output.len() != FRAME_SAMPLES {
            return Err(RnnoiseError::InvalidFrameLength {
                expected: FRAME_SAMPLES,
                input: input.len(),
                output: output.len(),
            });
        }

        for (scaled, sample) in self.scaled_input.iter_mut().zip(input.iter().copied()) {
            let sample = sanitize_sample(sample);
            *scaled = (sample * PCM_16_SCALE).clamp(-PCM_16_SCALE, PCM_16_MAX);
        }
        let current_vad_probability = self
            .state
            .process_frame(&mut self.denoised_output, &self.scaled_input)
            .clamp(0.0, 1.0);
        let vad_probability = self.delayed_vad_probability;

        if self.first_frame {
            output.fill(0.0);
            self.first_frame = false;
        } else {
            for ((destination, dry), wet) in output
                .iter_mut()
                .zip(self.delayed_dry.iter().copied())
                .zip(self.denoised_output.iter().copied())
            {
                let wet = sanitize_sample(wet / PCM_16_SCALE);
                *destination = sanitize_sample(dry + (wet - dry) * self.mix);
            }
        }

        for (delayed, sample) in self.delayed_dry.iter_mut().zip(input.iter().copied()) {
            *delayed = sanitize_sample(sample);
        }
        self.delayed_vad_probability = current_vad_probability;

        Ok(RnnoiseFrameMetrics { vad_probability })
    }

    pub fn reset(&mut self) {
        self.state = DenoiseState::new();
        self.first_frame = true;
        self.delayed_vad_probability = 0.0;
        self.delayed_dry.fill(0.0);
        self.scaled_input.fill(0.0);
        self.denoised_output.fill(0.0);
    }

    pub fn mix(&self) -> f32 {
        self.mix
    }
}

fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RnnoiseError {
    #[error("RNNoise mix {0} is outside 0..=1")]
    InvalidMix(f32),
    #[error("RNNoise frame has input/output lengths {input}/{output}; expected {expected}")]
    InvalidFrameLength {
        expected: usize,
        input: usize,
        output: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_contract_matches_engine_contract() {
        assert_eq!(SAMPLE_RATE_HZ, 48_000);
        assert_eq!(FRAME_SAMPLES, 480);
        assert_eq!(LATENCY_SAMPLES, FRAME_SAMPLES);
    }

    #[test]
    fn rejects_invalid_mix() {
        assert!(matches!(
            RnnoiseDsp::new(RnnoiseConfig { mix: f32::NAN }),
            Err(RnnoiseError::InvalidMix(value)) if value.is_nan()
        ));
        assert!(matches!(
            RnnoiseDsp::new(RnnoiseConfig { mix: 1.1 }),
            Err(RnnoiseError::InvalidMix(1.1))
        ));
    }

    #[test]
    fn dry_mix_has_one_frame_latency() {
        let mut dsp = RnnoiseDsp::new(RnnoiseConfig { mix: 0.0 }).unwrap();
        let first = [0.25; FRAME_SAMPLES];
        let second = [-0.5; FRAME_SAMPLES];
        let mut output = [1.0; FRAME_SAMPLES];

        dsp.process_frame(&first, &mut output).unwrap();
        assert_eq!(output, [0.0; FRAME_SAMPLES]);

        dsp.process_frame(&second, &mut output).unwrap();
        assert_eq!(output, first);
    }

    #[test]
    fn rejects_incorrect_frame_length() {
        let mut dsp = RnnoiseDsp::new(RnnoiseConfig { mix: 1.0 }).unwrap();
        let mut output = [0.0; FRAME_SAMPLES];
        assert_eq!(
            dsp.process_frame(&[0.0; 1], &mut output),
            Err(RnnoiseError::InvalidFrameLength {
                expected: FRAME_SAMPLES,
                input: 1,
                output: FRAME_SAMPLES,
            })
        );
    }
}
