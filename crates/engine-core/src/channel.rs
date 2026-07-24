const SPEAKER_FRONT_LEFT: u32 = 0x0000_0001;
const SPEAKER_FRONT_RIGHT: u32 = 0x0000_0002;
const SPEAKER_FRONT_CENTER: u32 = 0x0000_0004;
const SPEAKER_LOW_FREQUENCY: u32 = 0x0000_0008;
const SPEAKER_BACK_LEFT: u32 = 0x0000_0010;
const SPEAKER_BACK_RIGHT: u32 = 0x0000_0020;
const SPEAKER_FRONT_LEFT_OF_CENTER: u32 = 0x0000_0040;
const SPEAKER_FRONT_RIGHT_OF_CENTER: u32 = 0x0000_0080;
const SPEAKER_BACK_CENTER: u32 = 0x0000_0100;
const SPEAKER_SIDE_LEFT: u32 = 0x0000_0200;
const SPEAKER_SIDE_RIGHT: u32 = 0x0000_0400;
const LEFT_SPEAKERS: u32 = SPEAKER_FRONT_LEFT
    | SPEAKER_BACK_LEFT
    | SPEAKER_FRONT_LEFT_OF_CENTER
    | SPEAKER_SIDE_LEFT
    | 0x0000_1000
    | 0x0000_8000;
const RIGHT_SPEAKERS: u32 = SPEAKER_FRONT_RIGHT
    | SPEAKER_BACK_RIGHT
    | SPEAKER_FRONT_RIGHT_OF_CENTER
    | SPEAKER_SIDE_RIGHT
    | 0x0000_4000
    | 0x0002_0000;
const CENTER_SPEAKERS: u32 =
    SPEAKER_FRONT_CENTER | SPEAKER_BACK_CENTER | 0x0000_0800 | 0x0000_2000 | 0x0001_0000;
const CAPTURE_AUTO_SMOOTHING: f32 = 0.25;
const CAPTURE_AUTO_SWITCH_RATIO: f32 = 4.0;
const CAPTURE_AUTO_SILENCE_POWER: f32 = 1.0e-8;
const CAPTURE_AUTO_CONFIRMATION_BLOCKS: usize = 3;

#[derive(Debug, Clone)]
pub struct ChannelMapper {
    input_channels: usize,
    output_channels: usize,
    matrix: Vec<[f32; 2]>,
    auto_capture: Option<AutoCaptureSelector>,
}

impl ChannelMapper {
    pub fn render(input_channels: usize, channel_mask: u32) -> Result<Self, ChannelMapError> {
        match input_channels {
            0 => Err(ChannelMapError::InvalidChannelCount(0)),
            1 => Ok(Self::identity(1)),
            2 => Ok(Self::identity(2)),
            channels => Self::render_downmix(channels, channel_mask),
        }
    }

    pub fn capture(
        input_channels: usize,
        selected_channel: usize,
    ) -> Result<Self, ChannelMapError> {
        if input_channels == 0 {
            return Err(ChannelMapError::InvalidChannelCount(0));
        }
        if selected_channel >= input_channels {
            return Err(ChannelMapError::SelectedChannelOutOfRange {
                selected: selected_channel,
                channels: input_channels,
            });
        }

        let mut matrix = vec![[0.0, 0.0]; input_channels];
        matrix[selected_channel][0] = 1.0;
        Ok(Self {
            input_channels,
            output_channels: 1,
            matrix,
            auto_capture: None,
        })
    }

    pub fn capture_auto(input_channels: usize) -> Result<Self, ChannelMapError> {
        if input_channels == 0 {
            return Err(ChannelMapError::InvalidChannelCount(0));
        }
        Ok(Self {
            input_channels,
            output_channels: 1,
            matrix: vec![[0.0, 0.0]; input_channels],
            auto_capture: Some(AutoCaptureSelector::new(input_channels)),
        })
    }

    pub fn input_channels(&self) -> usize {
        self.input_channels
    }

    pub fn output_channels(&self) -> usize {
        self.output_channels
    }

    pub fn map_interleaved(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<usize, ChannelMapError> {
        if !input.len().is_multiple_of(self.input_channels) {
            return Err(ChannelMapError::InputNotFrameAligned {
                samples: input.len(),
                channels: self.input_channels,
            });
        }
        let frames = input.len() / self.input_channels;
        let required_output = frames
            .checked_mul(self.output_channels)
            .ok_or(ChannelMapError::OutputSizeOverflow)?;
        if output.len() < required_output {
            return Err(ChannelMapError::OutputTooSmall {
                required: required_output,
                actual: output.len(),
            });
        }

        if let Some(selector) = &mut self.auto_capture {
            let selected_channel = selector.select(input, self.input_channels);
            for (input_frame, output_frame) in input
                .chunks_exact(self.input_channels)
                .zip(output.chunks_exact_mut(1))
            {
                output_frame[0] = input_frame[selected_channel];
            }
            return Ok(frames);
        }

        for (input_frame, output_frame) in input
            .chunks_exact(self.input_channels)
            .zip(output.chunks_exact_mut(self.output_channels))
        {
            let mut left = 0.0;
            let mut right = 0.0;
            for (sample, coefficients) in input_frame.iter().zip(&self.matrix) {
                left += sample * coefficients[0];
                right += sample * coefficients[1];
            }
            output_frame[0] = left;
            if self.output_channels == 2 {
                output_frame[1] = right;
            }
        }
        Ok(frames)
    }

    fn identity(channels: usize) -> Self {
        let mut matrix = vec![[0.0, 0.0]; channels];
        matrix[0][0] = 1.0;
        if channels == 2 {
            matrix[1][1] = 1.0;
        }
        Self {
            input_channels: channels,
            output_channels: channels,
            matrix,
            auto_capture: None,
        }
    }

    fn render_downmix(input_channels: usize, channel_mask: u32) -> Result<Self, ChannelMapError> {
        if channel_mask == 0 || channel_mask.count_ones() as usize != input_channels {
            return Err(ChannelMapError::InvalidChannelMask {
                mask: channel_mask,
                channels: input_channels,
            });
        }

        let mut matrix = Vec::with_capacity(input_channels);
        for speaker in speaker_bits(channel_mask) {
            let coefficients = if speaker == SPEAKER_LOW_FREQUENCY {
                [0.5, 0.5]
            } else if LEFT_SPEAKERS & speaker != 0 {
                [1.0, 0.0]
            } else if RIGHT_SPEAKERS & speaker != 0 {
                [0.0, 1.0]
            } else if CENTER_SPEAKERS & speaker != 0 {
                [std::f32::consts::FRAC_1_SQRT_2; 2]
            } else {
                [0.5, 0.5]
            };
            matrix.push(coefficients);
        }
        normalize_downmix_gain(&mut matrix);

        Ok(Self {
            input_channels,
            output_channels: 2,
            matrix,
            auto_capture: None,
        })
    }
}

#[derive(Debug, Clone)]
struct AutoCaptureSelector {
    selected_channel: usize,
    candidate_channel: usize,
    candidate_blocks: usize,
    smoothed_power: Vec<f32>,
}

impl AutoCaptureSelector {
    fn new(channels: usize) -> Self {
        Self {
            selected_channel: 0,
            candidate_channel: 0,
            candidate_blocks: 0,
            smoothed_power: vec![0.0; channels],
        }
    }

    fn select(&mut self, input: &[f32], channels: usize) -> usize {
        let frames = input.len() / channels;
        if frames == 0 {
            return self.selected_channel;
        }
        for channel in 0..channels {
            let power = input
                .chunks_exact(channels)
                .map(|frame| {
                    let sample = frame[channel];
                    if sample.is_finite() {
                        sample * sample
                    } else {
                        0.0
                    }
                })
                .sum::<f32>()
                / frames as f32;
            self.smoothed_power[channel] +=
                (power - self.smoothed_power[channel]) * CAPTURE_AUTO_SMOOTHING;
        }
        let best_channel = self
            .smoothed_power
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map_or(self.selected_channel, |(channel, _)| channel);
        let best_power = self.smoothed_power[best_channel];
        let selected_power = self.smoothed_power[self.selected_channel];
        let should_consider_switch = best_channel != self.selected_channel
            && best_power > CAPTURE_AUTO_SILENCE_POWER
            && (selected_power <= CAPTURE_AUTO_SILENCE_POWER
                || best_power >= selected_power * CAPTURE_AUTO_SWITCH_RATIO);
        if !should_consider_switch {
            self.candidate_channel = self.selected_channel;
            self.candidate_blocks = 0;
            return self.selected_channel;
        }
        if selected_power <= CAPTURE_AUTO_SILENCE_POWER {
            self.selected_channel = best_channel;
            self.candidate_channel = best_channel;
            self.candidate_blocks = 0;
            return self.selected_channel;
        }
        if self.candidate_channel == best_channel {
            self.candidate_blocks = self.candidate_blocks.saturating_add(1);
        } else {
            self.candidate_channel = best_channel;
            self.candidate_blocks = 1;
        }
        if self.candidate_blocks >= CAPTURE_AUTO_CONFIRMATION_BLOCKS {
            self.selected_channel = best_channel;
            self.candidate_blocks = 0;
        }
        self.selected_channel
    }
}

fn speaker_bits(mask: u32) -> impl Iterator<Item = u32> {
    (0..32).filter_map(move |index| {
        let bit = 1u32 << index;
        (mask & bit != 0).then_some(bit)
    })
}

fn normalize_downmix_gain(matrix: &mut [[f32; 2]]) {
    let left_gain = matrix
        .iter()
        .map(|coefficients| coefficients[0])
        .sum::<f32>();
    let right_gain = matrix
        .iter()
        .map(|coefficients| coefficients[1])
        .sum::<f32>();
    let normalization = left_gain.max(right_gain).max(1.0);
    for coefficients in matrix {
        coefficients[0] /= normalization;
        coefficients[1] /= normalization;
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChannelMapError {
    #[error("channel count {0} is invalid")]
    InvalidChannelCount(usize),
    #[error("capture channel {selected} is outside the {channels}-channel input")]
    SelectedChannelOutOfRange { selected: usize, channels: usize },
    #[error("channel mask 0x{mask:08x} does not describe {channels} channels")]
    InvalidChannelMask { mask: u32, channels: usize },
    #[error("{samples} interleaved samples are not aligned to {channels} channels")]
    InputNotFrameAligned { samples: usize, channels: usize },
    #[error("mapped output sample count overflow")]
    OutputSizeOverflow,
    #[error("mapped output needs {required} samples but only {actual} are available")]
    OutputTooSmall { required: usize, actual: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_selects_one_channel() {
        let mut mapper = ChannelMapper::capture(2, 1).unwrap();
        let input = [0.1, 0.2, 0.3, 0.4];
        let mut output = [0.0; 2];

        assert_eq!(mapper.map_interleaved(&input, &mut output).unwrap(), 2);
        assert_eq!(output, [0.2, 0.4]);
    }

    #[test]
    fn stereo_render_is_preserved() {
        let mut mapper = ChannelMapper::render(2, 3).unwrap();
        let input = [0.1, 0.2, 0.3, 0.4];
        let mut output = [0.0; 4];

        mapper.map_interleaved(&input, &mut output).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn surround_downmix_stays_bounded() {
        let mut mapper = ChannelMapper::render(6, 0x3f).unwrap();
        let mut output = [0.0; 2];

        mapper
            .map_interleaved(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0], &mut output)
            .unwrap();
        assert!(output[0] <= 1.0);
        assert!(output[1] <= 1.0);
    }

    #[test]
    fn automatic_capture_uses_the_active_channel() {
        let mut mapper = ChannelMapper::capture_auto(2).unwrap();
        let input = [0.0, 0.4, 0.0, -0.3];
        let mut output = [0.0; 2];

        mapper.map_interleaved(&input, &mut output).unwrap();

        assert_eq!(output, [0.4, -0.3]);
    }

    #[test]
    fn automatic_capture_ignores_a_single_competing_transient() {
        let mut mapper = ChannelMapper::capture_auto(2).unwrap();
        let mut output = [0.0; 2];
        mapper
            .map_interleaved(&[0.2, 0.0, 0.2, 0.0], &mut output)
            .unwrap();

        mapper
            .map_interleaved(&[0.0, 1.0, 0.0, 1.0], &mut output)
            .unwrap();

        assert_eq!(output, [0.0, 0.0]);
    }
}
