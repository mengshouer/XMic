use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use serde::Serialize;

use crate::{DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ, MAX_RENDER_CHANNELS};

const MAX_RELATIVE_RATIO: f64 = 1.01;
const CLOCK_ESTIMATE_INTERVAL_100NS: u64 = 5_000_000;
const SINC_LENGTH: usize = 64;
const SINC_OVERSAMPLING: usize = 128;
pub const DEFAULT_QUEUE_CAPACITY_FRAMES: usize = DSP_SAMPLES_PER_FRAME * 16;

#[derive(Debug, Clone)]
pub struct NormalizedBlock {
    pub channels: usize,
    pub samples: [[f32; DSP_SAMPLES_PER_FRAME]; MAX_RENDER_CHANNELS],
}

impl NormalizedBlock {
    pub fn silence(channels: usize) -> Result<Self, NormalizerError> {
        validate_channels(channels)?;
        Ok(Self {
            channels,
            samples: [[0.0; DSP_SAMPLES_PER_FRAME]; MAX_RENDER_CHANNELS],
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClockObservation {
    pub device_position_frames: u64,
    pub qpc_position_100ns: u64,
    pub discontinuity: bool,
    pub timestamp_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockUpdate {
    Stable,
    Reanchored,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizerMetrics {
    pub input_rate_hz: u32,
    pub output_rate_hz: u32,
    pub channels: usize,
    pub queue_capacity_frames: usize,
    pub queue_fill_frames: usize,
    pub minimum_queue_fill_frames: usize,
    pub maximum_queue_fill_frames: usize,
    pub input_frames_received: u64,
    pub input_frames_consumed: u64,
    pub output_frames: u64,
    pub output_blocks: u64,
    pub underrun_events: u64,
    pub overflow_events: u64,
    pub dropped_input_frames: u64,
    pub non_finite_samples_replaced: u64,
    pub clock_discontinuities: u64,
    pub clock_updates: u64,
    pub nominal_resample_ratio: f64,
    pub current_resample_ratio: f64,
    pub measured_input_rate_hz: Option<f64>,
    pub measured_drift_ppm: Option<f64>,
}

pub struct AdaptiveNormalizer {
    channels: usize,
    nominal_input_rate_hz: u32,
    resampler: Async<f32>,
    queue: InterleavedRing,
    input_scratch: Vec<f32>,
    output_scratch: Vec<f32>,
    clock_anchor: Option<(u64, u64)>,
    smoothed_input_rate_hz: Option<f64>,
    metrics: NormalizerMetrics,
}

impl AdaptiveNormalizer {
    pub fn new(
        input_rate_hz: u32,
        channels: usize,
        queue_capacity_frames: usize,
    ) -> Result<Self, NormalizerError> {
        validate_channels(channels)?;
        if !(8_000..=384_000).contains(&input_rate_hz) {
            return Err(NormalizerError::UnsupportedInputRate(input_rate_hz));
        }
        if queue_capacity_frames < DSP_SAMPLES_PER_FRAME * 2 {
            return Err(NormalizerError::QueueTooSmall(queue_capacity_frames));
        }

        let nominal_ratio = DSP_SAMPLE_RATE_HZ as f64 / input_rate_hz as f64;
        let parameters = SincInterpolationParameters::new(SINC_LENGTH, WindowFunction::Blackman2)
            .oversampling_factor(SINC_OVERSAMPLING)
            .interpolation(SincInterpolationType::Cubic);
        let resampler = Async::<f32>::new_sinc(
            nominal_ratio,
            MAX_RELATIVE_RATIO,
            &parameters,
            DSP_SAMPLES_PER_FRAME,
            channels,
            FixedAsync::Output,
        )
        .map_err(|error| NormalizerError::CreateResampler(error.to_string()))?;
        let maximum_input_frames = resampler.input_frames_max();
        if queue_capacity_frames < maximum_input_frames {
            return Err(NormalizerError::QueueBelowResamplerInput {
                capacity: queue_capacity_frames,
                required: maximum_input_frames,
            });
        }
        let input_scratch = vec![0.0; maximum_input_frames * channels];
        let output_scratch = vec![0.0; resampler.output_frames_max() * channels];

        Ok(Self {
            channels,
            nominal_input_rate_hz: input_rate_hz,
            resampler,
            queue: InterleavedRing::new(queue_capacity_frames, channels)?,
            input_scratch,
            output_scratch,
            clock_anchor: None,
            smoothed_input_rate_hz: None,
            metrics: NormalizerMetrics {
                input_rate_hz,
                output_rate_hz: DSP_SAMPLE_RATE_HZ as u32,
                channels,
                queue_capacity_frames,
                queue_fill_frames: 0,
                minimum_queue_fill_frames: queue_capacity_frames,
                maximum_queue_fill_frames: 0,
                input_frames_received: 0,
                input_frames_consumed: 0,
                output_frames: 0,
                output_blocks: 0,
                underrun_events: 0,
                overflow_events: 0,
                dropped_input_frames: 0,
                non_finite_samples_replaced: 0,
                clock_discontinuities: 0,
                clock_updates: 0,
                nominal_resample_ratio: nominal_ratio,
                current_resample_ratio: nominal_ratio,
                measured_input_rate_hz: None,
                measured_drift_ppm: None,
            },
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn input_frames_needed(&self) -> usize {
        self.resampler.input_frames_next()
    }

    pub fn queue_fill_frames(&self) -> usize {
        self.queue.len_frames()
    }

    pub fn push_interleaved(&mut self, input: &[f32]) -> Result<(), NormalizerError> {
        if !input.len().is_multiple_of(self.channels) {
            return Err(NormalizerError::InputNotFrameAligned {
                samples: input.len(),
                channels: self.channels,
            });
        }
        let input_frames = input.len() / self.channels;
        let result = self.queue.push_overwrite(input);
        self.metrics.input_frames_received += input_frames as u64;
        self.metrics.non_finite_samples_replaced += result.non_finite_samples_replaced;
        if result.dropped_frames > 0 {
            self.metrics.overflow_events += 1;
            self.metrics.dropped_input_frames += result.dropped_frames as u64;
        }
        self.update_fill_metrics();
        Ok(())
    }

    pub fn observe_clock(
        &mut self,
        observation: ClockObservation,
    ) -> Result<ClockUpdate, NormalizerError> {
        if observation.discontinuity || observation.timestamp_error {
            self.reset_stream_timing();
            if !observation.timestamp_error && observation.qpc_position_100ns != 0 {
                self.clock_anchor = Some((
                    observation.device_position_frames,
                    observation.qpc_position_100ns,
                ));
            }
            self.metrics.clock_discontinuities += 1;
            return Ok(ClockUpdate::Reanchored);
        }

        let Some((anchor_device, anchor_qpc)) = self.clock_anchor else {
            self.clock_anchor = Some((
                observation.device_position_frames,
                observation.qpc_position_100ns,
            ));
            return Ok(ClockUpdate::Stable);
        };
        if observation.device_position_frames <= anchor_device
            || observation.qpc_position_100ns <= anchor_qpc
        {
            self.reset_stream_timing();
            self.clock_anchor = Some((
                observation.device_position_frames,
                observation.qpc_position_100ns,
            ));
            self.metrics.clock_discontinuities += 1;
            return Ok(ClockUpdate::Reanchored);
        }

        let qpc_delta = observation.qpc_position_100ns - anchor_qpc;
        if qpc_delta < CLOCK_ESTIMATE_INTERVAL_100NS {
            return Ok(ClockUpdate::Stable);
        }
        let device_delta = observation.device_position_frames - anchor_device;
        let measured_rate = device_delta as f64 * 10_000_000.0 / qpc_delta as f64;
        if !measured_rate.is_finite() || measured_rate < 1_000.0 {
            self.metrics.clock_discontinuities += 1;
            self.reset_stream_timing();
            self.clock_anchor = Some((
                observation.device_position_frames,
                observation.qpc_position_100ns,
            ));
            return Ok(ClockUpdate::Reanchored);
        }
        let smoothed_rate = match self.smoothed_input_rate_hz {
            Some(current) => current * 0.8 + measured_rate * 0.2,
            None => measured_rate,
        };
        self.smoothed_input_rate_hz = Some(smoothed_rate);
        let minimum_ratio = self.metrics.nominal_resample_ratio / MAX_RELATIVE_RATIO;
        let maximum_ratio = self.metrics.nominal_resample_ratio * MAX_RELATIVE_RATIO;
        let ratio = (DSP_SAMPLE_RATE_HZ as f64 / smoothed_rate).clamp(minimum_ratio, maximum_ratio);
        self.resampler
            .set_resample_ratio(ratio, true)
            .map_err(|error| NormalizerError::AdjustRatio(error.to_string()))?;

        self.metrics.clock_updates += 1;
        self.metrics.current_resample_ratio = ratio;
        self.metrics.measured_input_rate_hz = Some(smoothed_rate);
        self.metrics.measured_drift_ppm =
            Some((smoothed_rate / self.nominal_input_rate_hz as f64 - 1.0) * 1_000_000.0);
        self.clock_anchor = Some((
            observation.device_position_frames,
            observation.qpc_position_100ns,
        ));
        Ok(ClockUpdate::Stable)
    }

    pub fn process_next(&mut self, output: &mut NormalizedBlock) -> Result<bool, NormalizerError> {
        if output.channels != self.channels {
            return Err(NormalizerError::OutputChannelMismatch {
                expected: self.channels,
                actual: output.channels,
            });
        }
        let input_frames = self.resampler.input_frames_next();
        let output_frames = self.resampler.output_frames_next();
        if output_frames != DSP_SAMPLES_PER_FRAME {
            return Err(NormalizerError::UnexpectedOutputFrames(output_frames));
        }
        if self.queue.len_frames() < input_frames {
            self.metrics.underrun_events += 1;
            for channel in &mut output.samples {
                channel.fill(0.0);
            }
            self.update_fill_metrics();
            return Ok(false);
        }

        self.queue.copy_front(
            input_frames,
            &mut self.input_scratch[..input_frames * self.channels],
        );
        self.output_scratch[..output_frames * self.channels].fill(0.0);
        let input_adapter = InterleavedSlice::new(
            &self.input_scratch[..input_frames * self.channels],
            self.channels,
            input_frames,
        )
        .map_err(|error| NormalizerError::Adapter(error.to_string()))?;
        let mut output_adapter = InterleavedSlice::new_mut(
            &mut self.output_scratch[..output_frames * self.channels],
            self.channels,
            output_frames,
        )
        .map_err(|error| NormalizerError::Adapter(error.to_string()))?;
        let (consumed, written) = self
            .resampler
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
            .map_err(|error| NormalizerError::Process(error.to_string()))?;
        if written != DSP_SAMPLES_PER_FRAME {
            return Err(NormalizerError::UnexpectedOutputFrames(written));
        }
        self.queue.discard(consumed);

        for frame in 0..DSP_SAMPLES_PER_FRAME {
            for channel in 0..self.channels {
                output.samples[channel][frame] =
                    self.output_scratch[frame * self.channels + channel];
            }
        }
        for channel in self.channels..MAX_RENDER_CHANNELS {
            output.samples[channel].fill(0.0);
        }

        self.metrics.input_frames_consumed += consumed as u64;
        self.metrics.output_frames += written as u64;
        self.metrics.output_blocks += 1;
        self.update_fill_metrics();
        Ok(true)
    }

    pub fn metrics(&self) -> &NormalizerMetrics {
        &self.metrics
    }

    fn update_fill_metrics(&mut self) {
        let fill = self.queue.len_frames();
        self.metrics.queue_fill_frames = fill;
        self.metrics.minimum_queue_fill_frames = self.metrics.minimum_queue_fill_frames.min(fill);
        self.metrics.maximum_queue_fill_frames = self.metrics.maximum_queue_fill_frames.max(fill);
    }

    fn reset_stream_timing(&mut self) {
        let discarded_frames = self.queue.len_frames();
        self.queue.clear();
        self.resampler.reset();
        self.clock_anchor = None;
        self.smoothed_input_rate_hz = None;
        self.metrics.dropped_input_frames = self
            .metrics
            .dropped_input_frames
            .saturating_add(discarded_frames as u64);
        self.metrics.current_resample_ratio = self.metrics.nominal_resample_ratio;
        self.metrics.measured_input_rate_hz = None;
        self.metrics.measured_drift_ppm = None;
        self.update_fill_metrics();
    }
}

fn validate_channels(channels: usize) -> Result<(), NormalizerError> {
    if (1..=MAX_RENDER_CHANNELS).contains(&channels) {
        Ok(())
    } else {
        Err(NormalizerError::UnsupportedChannelCount(channels))
    }
}

struct InterleavedRing {
    data: Vec<f32>,
    capacity_frames: usize,
    channels: usize,
    head_frame: usize,
    len_frames: usize,
}

impl InterleavedRing {
    fn new(capacity_frames: usize, channels: usize) -> Result<Self, NormalizerError> {
        let sample_capacity = capacity_frames
            .checked_mul(channels)
            .ok_or(NormalizerError::QueueSizeOverflow)?;
        Ok(Self {
            data: vec![0.0; sample_capacity],
            capacity_frames,
            channels,
            head_frame: 0,
            len_frames: 0,
        })
    }

    fn len_frames(&self) -> usize {
        self.len_frames
    }

    fn push_overwrite(&mut self, input: &[f32]) -> PushResult {
        let mut input = input;
        let mut input_frames = input.len() / self.channels;
        let mut dropped_frames = 0usize;
        if input_frames > self.capacity_frames {
            let skip_frames = input_frames - self.capacity_frames;
            input = &input[skip_frames * self.channels..];
            input_frames = self.capacity_frames;
            dropped_frames += skip_frames;
        }
        let overflow = self
            .len_frames
            .saturating_add(input_frames)
            .saturating_sub(self.capacity_frames);
        if overflow > 0 {
            self.discard(overflow);
            dropped_frames += overflow;
        }

        let mut non_finite_samples_replaced = 0u64;
        for frame in 0..input_frames {
            let destination_frame =
                (self.head_frame + self.len_frames + frame) % self.capacity_frames;
            let destination_offset = destination_frame * self.channels;
            let source_offset = frame * self.channels;
            for channel in 0..self.channels {
                let sample = input[source_offset + channel];
                if sample.is_finite() {
                    self.data[destination_offset + channel] = sample;
                } else {
                    self.data[destination_offset + channel] = 0.0;
                    non_finite_samples_replaced += 1;
                }
            }
        }
        self.len_frames += input_frames;
        PushResult {
            dropped_frames,
            non_finite_samples_replaced,
        }
    }

    fn copy_front(&self, frames: usize, output: &mut [f32]) {
        debug_assert!(frames <= self.len_frames);
        debug_assert!(output.len() >= frames * self.channels);
        for frame in 0..frames {
            let source_frame = (self.head_frame + frame) % self.capacity_frames;
            let source_offset = source_frame * self.channels;
            let destination_offset = frame * self.channels;
            output[destination_offset..destination_offset + self.channels]
                .copy_from_slice(&self.data[source_offset..source_offset + self.channels]);
        }
    }

    fn discard(&mut self, frames: usize) {
        debug_assert!(frames <= self.len_frames);
        self.head_frame = (self.head_frame + frames) % self.capacity_frames;
        self.len_frames -= frames;
    }

    fn clear(&mut self) {
        self.head_frame = 0;
        self.len_frames = 0;
    }
}

struct PushResult {
    dropped_frames: usize,
    non_finite_samples_replaced: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizerError {
    #[error("normalizer channel count {0} is unsupported")]
    UnsupportedChannelCount(usize),
    #[error("normalizer input rate {0} Hz is unsupported")]
    UnsupportedInputRate(u32),
    #[error("normalizer queue capacity {0} frames is too small")]
    QueueTooSmall(usize),
    #[error("normalizer queue has {capacity} frames but resampler may require {required}")]
    QueueBelowResamplerInput { capacity: usize, required: usize },
    #[error("normalizer queue size overflow")]
    QueueSizeOverflow,
    #[error("{samples} samples are not aligned to {channels} channels")]
    InputNotFrameAligned { samples: usize, channels: usize },
    #[error("create asynchronous resampler: {0}")]
    CreateResampler(String),
    #[error("adjust asynchronous resampler ratio: {0}")]
    AdjustRatio(String),
    #[error("audio adapter error: {0}")]
    Adapter(String),
    #[error("resample audio: {0}")]
    Process(String),
    #[error("normalizer produced {0} frames instead of 480")]
    UnexpectedOutputFrames(usize),
    #[error("output block has {actual} channels; expected {expected}")]
    OutputChannelMismatch { expected: usize, actual: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_48khz_to_fixed_blocks() {
        let mut normalizer =
            AdaptiveNormalizer::new(48_000, 1, DEFAULT_QUEUE_CAPACITY_FRAMES).unwrap();
        let mut output = NormalizedBlock::silence(1).unwrap();
        let mut blocks = 0;

        for packet in 0..100 {
            let input = (0..DSP_SAMPLES_PER_FRAME)
                .map(|frame| {
                    let sample = packet * DSP_SAMPLES_PER_FRAME + frame;
                    (sample as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.25
                })
                .collect::<Vec<_>>();
            normalizer.push_interleaved(&input).unwrap();
            while normalizer.queue_fill_frames() >= normalizer.input_frames_needed() {
                assert!(normalizer.process_next(&mut output).unwrap());
                blocks += 1;
            }
        }

        assert!(blocks >= 98);
        assert_eq!(normalizer.metrics().overflow_events, 0);
        assert_eq!(normalizer.metrics().output_frames, blocks * 480);
    }

    #[test]
    fn reports_output_underrun() {
        let mut normalizer =
            AdaptiveNormalizer::new(48_000, 1, DEFAULT_QUEUE_CAPACITY_FRAMES).unwrap();
        let mut output = NormalizedBlock::silence(1).unwrap();

        assert!(!normalizer.process_next(&mut output).unwrap());
        assert_eq!(normalizer.metrics().underrun_events, 1);
    }

    #[test]
    fn estimates_input_clock_rate() {
        let mut normalizer =
            AdaptiveNormalizer::new(48_000, 1, DEFAULT_QUEUE_CAPACITY_FRAMES).unwrap();
        normalizer
            .observe_clock(ClockObservation {
                device_position_frames: 0,
                qpc_position_100ns: 1_000_000,
                discontinuity: false,
                timestamp_error: false,
            })
            .unwrap();
        normalizer
            .observe_clock(ClockObservation {
                device_position_frames: 48_000,
                qpc_position_100ns: 11_000_000,
                discontinuity: false,
                timestamp_error: false,
            })
            .unwrap();

        assert_eq!(normalizer.metrics().clock_updates, 1);
        assert_eq!(normalizer.metrics().measured_input_rate_hz, Some(48_000.0));
        assert_eq!(normalizer.metrics().measured_drift_ppm, Some(0.0));
    }

    #[test]
    fn discontinuity_discards_queued_audio_and_requests_reanchor() {
        let mut normalizer =
            AdaptiveNormalizer::new(48_000, 1, DEFAULT_QUEUE_CAPACITY_FRAMES).unwrap();
        normalizer
            .push_interleaved(&[0.25; DSP_SAMPLES_PER_FRAME])
            .unwrap();

        let update = normalizer
            .observe_clock(ClockObservation {
                device_position_frames: 48_000,
                qpc_position_100ns: 10_000_000,
                discontinuity: true,
                timestamp_error: false,
            })
            .unwrap();

        assert_eq!(update, ClockUpdate::Reanchored);
        assert_eq!(normalizer.queue_fill_frames(), 0);
        assert_eq!(
            normalizer.metrics().dropped_input_frames,
            DSP_SAMPLES_PER_FRAME as u64
        );
    }

    #[test]
    fn default_queue_supports_maximum_input_rate() {
        let normalizer =
            AdaptiveNormalizer::new(384_000, 2, DEFAULT_QUEUE_CAPACITY_FRAMES).unwrap();

        assert!(normalizer.input_frames_needed() <= DEFAULT_QUEUE_CAPACITY_FRAMES);
    }
}
