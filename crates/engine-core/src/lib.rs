#![forbid(unsafe_code)]

mod channel;
mod delay;
mod normalize;
mod post_process;
mod speech_presence;

pub use channel::{ChannelMapError, ChannelMapper};
pub use delay::{DelayCalibrationState, HostDelayEstimator, HostDelayObservation};
pub use normalize::{
    AdaptiveNormalizer, ClockObservation, ClockUpdate, NormalizedBlock, NormalizerError,
    NormalizerMetrics, DEFAULT_QUEUE_CAPACITY_FRAMES,
};
pub use post_process::{
    EchoGuard, EchoGuardConfig, EchoGuardFrameMetrics, LimiterFrameMetrics, PostProcessConfigError,
    SafetyLimiter, SafetyLimiterConfig, VadGate, VadGateConfig, VadGateFrameMetrics,
};
pub use speech_presence::SpeechPresenceDetector;

pub const DSP_SAMPLE_RATE_HZ: usize = 48_000;
pub const DSP_FRAME_DURATION_MS: usize = 10;
pub const DSP_SAMPLES_PER_FRAME: usize = DSP_SAMPLE_RATE_HZ * DSP_FRAME_DURATION_MS / 1_000;
pub const MAX_RENDER_CHANNELS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimestamp {
    pub device_position_frames: u64,
    pub qpc_position_100ns: u64,
}

#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub timestamp: FrameTimestamp,
    pub samples: [f32; DSP_SAMPLES_PER_FRAME],
}

#[derive(Debug, Clone)]
pub struct RenderFrame {
    pub timestamp: FrameTimestamp,
    pub channels: usize,
    pub samples: [[f32; DSP_SAMPLES_PER_FRAME]; MAX_RENDER_CHANNELS],
}

impl RenderFrame {
    pub fn validate(&self) -> Result<(), FrameError> {
        if (1..=MAX_RENDER_CHANNELS).contains(&self.channels) {
            Ok(())
        } else {
            Err(FrameError::UnsupportedRenderChannelCount(self.channels))
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("render channel count {0} is unsupported")]
    UnsupportedRenderChannelCount(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_milliseconds_at_48khz_is_480_samples() {
        assert_eq!(DSP_SAMPLES_PER_FRAME, 480);
    }

    #[test]
    fn render_frame_rejects_more_than_stereo() {
        let frame = RenderFrame {
            timestamp: FrameTimestamp {
                device_position_frames: 0,
                qpc_position_100ns: 0,
            },
            channels: 3,
            samples: [[0.0; DSP_SAMPLES_PER_FRAME]; MAX_RENDER_CHANNELS],
        };

        assert_eq!(
            frame.validate(),
            Err(FrameError::UnsupportedRenderChannelCount(3))
        );
    }
}
