#![forbid(unsafe_code)]

mod processor;

pub use processor::{
    processing_latency_frames, AlignedRawPath, DspFrameMetrics, DspProcessor, DspProcessorError,
};
