use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
};

use rtrb::{Consumer, Producer, RingBuffer};
use windows::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::Audio::{
        IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_NOPERSIST,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    },
    System::{
        Com::{CoCreateInstance, CLSCTX_ALL},
        Threading::WaitForSingleObject,
    },
};
use xmic_engine_core::{DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ};
use xmic_platform_api::{DataFlow, EndpointDescriptor, PlatformError};

use crate::{
    capture::{select_endpoint, EventHandle, MmcssRegistration},
    device_group_id, device_id, friendly_name, join_worker_with_timeout, operation_error,
    smoothed_sample, ComApartment, WORKER_START_TIMEOUT,
};

const OUTPUT_RING_BLOCKS: usize = 32;
const OUTPUT_TARGET_QUEUE_FRAMES: u64 = (DSP_SAMPLES_PER_FRAME * 3) as u64;
const OUTPUT_START_QUEUE_FRAMES: u64 = OUTPUT_TARGET_QUEUE_FRAMES;
const QUEUE_FILL_SMOOTHING: f64 = 0.1;
const RATE_CORRECTION_PER_FRAME: f64 = 0.000_005;
const MAX_RATE_CORRECTION: f64 = 0.005;
const EVENT_WAIT_MS: u32 = 100;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;

#[derive(Debug, Clone)]
pub struct OutputSinkRequest {
    pub endpoint_id: String,
}

#[derive(Debug, Clone)]
pub struct OutputStreamInfo {
    pub endpoint: EndpointDescriptor,
    pub endpoint_buffer_frames: u32,
    /// Latency the driver reports for this stream, in 100 ns units.
    pub stream_latency_100ns: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputCountersSnapshot {
    pub queued_blocks: u64,
    pub queue_overflows: u64,
    pub queue_depth_frames: u64,
    pub rate_adjustment_ppm: i64,
    pub rendered_frames: u64,
    pub underrun_events: u64,
    pub underrun_frames: u64,
    pub wait_timeouts: u64,
    /// Smoothed number of frames still sitting in the endpoint buffer when the worker wakes up.
    pub endpoint_padding_frames: u64,
}

#[derive(Default)]
struct OutputCounters {
    queued_blocks: AtomicU64,
    queue_overflows: AtomicU64,
    accepted_input_frames: AtomicU64,
    consumed_input_frames: AtomicU64,
    rate_adjustment_ppm: AtomicI64,
    rendered_frames: AtomicU64,
    underrun_events: AtomicU64,
    underrun_frames: AtomicU64,
    wait_timeouts: AtomicU64,
    endpoint_padding_frames: AtomicU64,
}

impl OutputCounters {
    fn snapshot(&self) -> OutputCountersSnapshot {
        let accepted_input_frames = self.accepted_input_frames.load(Ordering::Acquire);
        let consumed_input_frames = self.consumed_input_frames.load(Ordering::Acquire);
        OutputCountersSnapshot {
            queued_blocks: self.queued_blocks.load(Ordering::Relaxed),
            queue_overflows: self.queue_overflows.load(Ordering::Relaxed),
            queue_depth_frames: accepted_input_frames.saturating_sub(consumed_input_frames),
            rate_adjustment_ppm: self.rate_adjustment_ppm.load(Ordering::Relaxed),
            rendered_frames: self.rendered_frames.load(Ordering::Relaxed),
            underrun_events: self.underrun_events.load(Ordering::Relaxed),
            underrun_frames: self.underrun_frames.load(Ordering::Relaxed),
            wait_timeouts: self.wait_timeouts.load(Ordering::Relaxed),
            endpoint_padding_frames: self.endpoint_padding_frames.load(Ordering::Relaxed),
        }
    }

    /// Only the output worker writes this, so a plain load/store pair is enough.
    fn observe_endpoint_padding(&self, frames: u64) {
        let previous = self.endpoint_padding_frames.load(Ordering::Relaxed);
        self.endpoint_padding_frames
            .store(smoothed_sample(previous, frames), Ordering::Relaxed);
    }
}

pub struct OutputSinkStream {
    info: OutputStreamInfo,
    producer: Producer<[f32; DSP_SAMPLES_PER_FRAME]>,
    stop: Arc<AtomicBool>,
    counters: Arc<OutputCounters>,
    worker: Option<JoinHandle<Result<(), PlatformError>>>,
}

impl OutputSinkStream {
    pub fn info(&self) -> &OutputStreamInfo {
        &self.info
    }

    pub fn push_block(&mut self, samples: [f32; DSP_SAMPLES_PER_FRAME]) -> bool {
        if self.producer.push(samples).is_ok() {
            self.counters.queued_blocks.fetch_add(1, Ordering::Relaxed);
            self.counters
                .accepted_input_frames
                .fetch_add(DSP_SAMPLES_PER_FRAME as u64, Ordering::Release);
            true
        } else {
            self.counters
                .queue_overflows
                .fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    pub fn counters(&self) -> OutputCountersSnapshot {
        self.counters.snapshot()
    }

    pub fn worker_finished(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub fn stop(mut self) -> Result<(), PlatformError> {
        self.request_stop();
        self.join_worker()
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn join_worker(&mut self) -> Result<(), PlatformError> {
        self.worker.take().map_or(Ok(()), |worker| {
            join_worker_with_timeout(worker, "WASAPI output worker")?
        })
    }
}

impl Drop for OutputSinkStream {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_worker();
    }
}

pub fn start_output_sink(request: OutputSinkRequest) -> Result<OutputSinkStream, PlatformError> {
    if request.endpoint_id.trim().is_empty() {
        return Err(PlatformError::Operation(
            "output endpoint ID cannot be empty".to_owned(),
        ));
    }
    let (producer, consumer) = RingBuffer::new(OUTPUT_RING_BLOCKS);
    let stop = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(OutputCounters::default());
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker_stop = Arc::clone(&stop);
    let worker_counters = Arc::clone(&counters);
    let worker = thread::spawn(move || {
        let _com = ComApartment::initialize_mta()?;
        let prepared = prepare_output(&request.endpoint_id, consumer, worker_counters);
        let ready = prepared
            .as_ref()
            .map(PreparedOutput::info)
            .map_err(ToString::to_string);
        let _ = ready_tx.send(ready);
        run_output(prepared?, &worker_stop)
    });

    let info = match ready_rx.recv_timeout(WORKER_START_TIMEOUT) {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            return Err(PlatformError::Operation(error));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            return Err(PlatformError::Operation(
                "WASAPI output preparation timed out after 5 seconds".to_owned(),
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            return Err(PlatformError::WorkerStopped);
        }
    };

    Ok(OutputSinkStream {
        info,
        producer,
        stop,
        counters,
        worker: Some(worker),
    })
}

struct PreparedOutput {
    client: IAudioClient,
    render_client: IAudioRenderClient,
    event: EventHandle,
    endpoint: EndpointDescriptor,
    endpoint_buffer_frames: u32,
    stream_latency_100ns: u64,
    consumer: Consumer<[f32; DSP_SAMPLES_PER_FRAME]>,
    current_block: [f32; DSP_SAMPLES_PER_FRAME],
    current_offset: usize,
    current_sample: Option<f32>,
    next_sample: Option<f32>,
    resample_phase: f64,
    rate_controller: OutputRateController,
    prebuffering: bool,
    has_started_audio: bool,
    underrun_active: bool,
    consumed_input_frames: u64,
    counters: Arc<OutputCounters>,
}

impl PreparedOutput {
    fn info(&self) -> OutputStreamInfo {
        OutputStreamInfo {
            endpoint: self.endpoint.clone(),
            endpoint_buffer_frames: self.endpoint_buffer_frames,
            stream_latency_100ns: self.stream_latency_100ns,
        }
    }

    fn queue_depth_frames(&self) -> u64 {
        self.counters
            .accepted_input_frames
            .load(Ordering::Acquire)
            .saturating_sub(self.consumed_input_frames)
    }

    fn pop_input_sample(&mut self) -> Option<f32> {
        if self.current_offset == DSP_SAMPLES_PER_FRAME {
            self.current_block = self.consumer.pop().ok()?;
            self.current_offset = 0;
        }
        let sample = self.current_block[self.current_offset];
        self.current_offset += 1;
        self.consumed_input_frames = self.consumed_input_frames.saturating_add(1);
        Some(sample)
    }

    fn next_resampled_sample(&mut self) -> Option<f32> {
        if self.current_sample.is_none() {
            self.current_sample = self.pop_input_sample();
        }
        if self.next_sample.is_none() {
            self.next_sample = self.pop_input_sample();
        }
        let current = self.current_sample?;
        let next = self.next_sample?;
        let sample = current + (next - current) * self.resample_phase as f32;
        self.resample_phase += self.rate_controller.input_step();

        while self.resample_phase >= 1.0 {
            self.resample_phase -= 1.0;
            self.current_sample = self.next_sample.take();
            if self.current_sample.is_none() {
                break;
            }
            self.next_sample = self.pop_input_sample();
        }
        Some(sample)
    }

    fn reset_resampler(&mut self) {
        self.current_sample = None;
        self.next_sample = None;
        self.resample_phase = 0.0;
    }
}

struct OutputRateController {
    smoothed_queue_frames: f64,
    input_step: f64,
}

impl Default for OutputRateController {
    fn default() -> Self {
        Self {
            smoothed_queue_frames: OUTPUT_TARGET_QUEUE_FRAMES as f64,
            input_step: 1.0,
        }
    }
}

impl OutputRateController {
    fn reset(&mut self, queue_depth_frames: u64) {
        self.smoothed_queue_frames = queue_depth_frames as f64;
        self.input_step = 1.0;
    }

    fn update(&mut self, queue_depth_frames: u64) -> i64 {
        self.smoothed_queue_frames = self.smoothed_queue_frames * (1.0 - QUEUE_FILL_SMOOTHING)
            + queue_depth_frames as f64 * QUEUE_FILL_SMOOTHING;
        let queue_error = self.smoothed_queue_frames - OUTPUT_TARGET_QUEUE_FRAMES as f64;
        let correction = (queue_error * RATE_CORRECTION_PER_FRAME)
            .clamp(-MAX_RATE_CORRECTION, MAX_RATE_CORRECTION);
        self.input_step = 1.0 + correction;
        (correction * 1_000_000.0).round() as i64
    }

    fn input_step(&self) -> f64 {
        self.input_step
    }
}

fn prepare_output(
    endpoint_id: &str,
    consumer: Consumer<[f32; DSP_SAMPLES_PER_FRAME]>,
    counters: Arc<OutputCounters>,
) -> Result<PreparedOutput, PlatformError> {
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| operation_error("create MMDeviceEnumerator", error))?
    };
    let device = select_endpoint(&enumerator, DataFlow::Render, Some(endpoint_id))?;
    let endpoint = EndpointDescriptor {
        platform: "windows".to_owned(),
        id: device_id(&device)?,
        group_id: device_group_id(&device).ok(),
        friendly_name: friendly_name(&device)?,
        flow: DataFlow::Render,
        default_roles: Vec::new(),
    };
    let client: IAudioClient = unsafe {
        device
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| operation_error("activate WASAPI output client", error))?
    };
    let format = float_mono_format();
    let event = EventHandle::new()?;
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_NOPERSIST
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe {
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 0, 0, &format, None)
            .map_err(|error| operation_error("initialize shared WASAPI output", error))?;
        client
            .SetEventHandle(event.raw())
            .map_err(|error| operation_error("set WASAPI output event", error))?;
    }
    let endpoint_buffer_frames = unsafe {
        client
            .GetBufferSize()
            .map_err(|error| operation_error("read output buffer size", error))?
    };
    // The driver's own latency figure completes the measured output path; a driver that refuses the
    // call simply contributes nothing instead of failing the stream.
    let stream_latency_100ns = unsafe { client.GetStreamLatency() }
        .map(|latency| u64::try_from(latency).unwrap_or(0))
        .unwrap_or(0);
    let render_client: IAudioRenderClient = unsafe {
        client
            .GetService()
            .map_err(|error| operation_error("get IAudioRenderClient", error))?
    };

    Ok(PreparedOutput {
        client,
        render_client,
        event,
        endpoint,
        endpoint_buffer_frames,
        stream_latency_100ns,
        consumer,
        current_block: [0.0; DSP_SAMPLES_PER_FRAME],
        current_offset: DSP_SAMPLES_PER_FRAME,
        current_sample: None,
        next_sample: None,
        resample_phase: 0.0,
        rate_controller: OutputRateController::default(),
        prebuffering: true,
        has_started_audio: false,
        underrun_active: false,
        consumed_input_frames: 0,
        counters,
    })
}

fn run_output(mut prepared: PreparedOutput, stop: &AtomicBool) -> Result<(), PlatformError> {
    let _mmcss = MmcssRegistration::register()?;
    prefill_output_silence(&prepared)?;
    unsafe {
        prepared
            .client
            .Start()
            .map_err(|error| operation_error("start WASAPI output", error))?;
    }
    let run_result = (|| {
        while !stop.load(Ordering::Acquire) {
            let wait_result = unsafe { WaitForSingleObject(prepared.event.raw(), EVENT_WAIT_MS) };
            if wait_result == WAIT_OBJECT_0 {
                write_available_output(&mut prepared)?;
            } else if wait_result == WAIT_TIMEOUT {
                prepared
                    .counters
                    .wait_timeouts
                    .fetch_add(1, Ordering::Relaxed);
            } else if wait_result == WAIT_FAILED {
                return Err(operation_error(
                    "wait for WASAPI output event",
                    windows::core::Error::from_win32(),
                ));
            } else {
                return Err(PlatformError::Operation(format!(
                    "unexpected WASAPI output wait result: {}",
                    wait_result.0
                )));
            }
        }
        Ok(())
    })();
    let stop_result = unsafe {
        prepared
            .client
            .Stop()
            .map_err(|error| operation_error("stop WASAPI output", error))
    };
    run_result?;
    stop_result
}

fn prefill_output_silence(prepared: &PreparedOutput) -> Result<(), PlatformError> {
    unsafe {
        prepared
            .render_client
            .GetBuffer(prepared.endpoint_buffer_frames)
            .map_err(|error| operation_error("acquire initial output buffer", error))?;
        prepared
            .render_client
            .ReleaseBuffer(
                prepared.endpoint_buffer_frames,
                AUDCLNT_BUFFERFLAGS_SILENT.0 as u32,
            )
            .map_err(|error| operation_error("release initial output buffer", error))?;
    }
    Ok(())
}

fn write_available_output(prepared: &mut PreparedOutput) -> Result<(), PlatformError> {
    let padding = unsafe {
        prepared
            .client
            .GetCurrentPadding()
            .map_err(|error| operation_error("read WASAPI output padding", error))?
    };
    let available = prepared.endpoint_buffer_frames.saturating_sub(padding);
    prepared
        .counters
        .observe_endpoint_padding(u64::from(padding));
    if available == 0 {
        return Ok(());
    }
    let destination = unsafe {
        prepared
            .render_client
            .GetBuffer(available)
            .map_err(|error| operation_error("acquire WASAPI output buffer", error))?
    };
    if destination.is_null() {
        return Err(PlatformError::Operation(
            "WASAPI returned a null output buffer".to_owned(),
        ));
    }
    let samples =
        unsafe { std::slice::from_raw_parts_mut(destination.cast::<f32>(), available as usize) };
    let mut underrun_frames = 0u64;
    let mut underrun_started = false;
    let queue_depth_frames = prepared.queue_depth_frames();
    if prepared.prebuffering && queue_depth_frames >= OUTPUT_START_QUEUE_FRAMES {
        prepared.prebuffering = false;
        prepared.has_started_audio = true;
        prepared.underrun_active = false;
        prepared.rate_controller.reset(queue_depth_frames);
        prepared.reset_resampler();
    }
    let rate_adjustment_ppm = if prepared.prebuffering {
        0
    } else {
        prepared.rate_controller.update(queue_depth_frames)
    };
    prepared
        .counters
        .rate_adjustment_ppm
        .store(rate_adjustment_ppm, Ordering::Relaxed);

    for sample in samples {
        if prepared.prebuffering {
            *sample = 0.0;
            if prepared.has_started_audio {
                underrun_frames = underrun_frames.saturating_add(1);
            }
        } else if let Some(output_sample) = prepared.next_resampled_sample() {
            *sample = output_sample;
        } else {
            *sample = 0.0;
            underrun_frames = underrun_frames.saturating_add(1);
            underrun_started = !prepared.underrun_active;
            prepared.underrun_active = true;
            prepared.prebuffering = true;
            prepared.rate_controller.reset(0);
            prepared.reset_resampler();
            prepared
                .counters
                .rate_adjustment_ppm
                .store(0, Ordering::Relaxed);
        }
    }
    prepared
        .counters
        .consumed_input_frames
        .store(prepared.consumed_input_frames, Ordering::Release);
    unsafe {
        prepared
            .render_client
            .ReleaseBuffer(available, 0)
            .map_err(|error| operation_error("release WASAPI output buffer", error))?;
    }
    prepared
        .counters
        .rendered_frames
        .fetch_add(u64::from(available), Ordering::Relaxed);
    if underrun_started {
        prepared
            .counters
            .underrun_events
            .fetch_add(1, Ordering::Relaxed);
    }
    if underrun_frames > 0 {
        prepared
            .counters
            .underrun_frames
            .fetch_add(underrun_frames, Ordering::Relaxed);
    }
    Ok(())
}

fn float_mono_format() -> WAVEFORMATEX {
    let block_align = std::mem::size_of::<f32>() as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: 1,
        nSamplesPerSec: DSP_SAMPLE_RATE_HZ as u32,
        nAvgBytesPerSec: DSP_SAMPLE_RATE_HZ as u32 * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_contract_is_48khz_float_mono() {
        let format = float_mono_format();
        let format_tag = format.wFormatTag;
        let channels = format.nChannels;
        let sample_rate = format.nSamplesPerSec;
        let block_align = format.nBlockAlign;
        assert_eq!(format_tag, WAVE_FORMAT_IEEE_FLOAT);
        assert_eq!(channels, 1);
        assert_eq!(sample_rate, 48_000);
        assert_eq!(block_align, 4);
    }

    #[test]
    fn output_rate_controller_tracks_queue_fill() {
        let mut controller = OutputRateController::default();
        controller.reset(OUTPUT_TARGET_QUEUE_FRAMES);

        assert_eq!(controller.update(OUTPUT_TARGET_QUEUE_FRAMES), 0);
        assert_eq!(controller.input_step(), 1.0);

        let high_adjustment = controller.update(OUTPUT_TARGET_QUEUE_FRAMES + 480);
        assert!(high_adjustment > 0);
        assert!(controller.input_step() > 1.0);

        controller.reset(OUTPUT_TARGET_QUEUE_FRAMES);
        let low_adjustment = controller.update(OUTPUT_TARGET_QUEUE_FRAMES - 480);
        assert!(low_adjustment < 0);
        assert!(controller.input_step() < 1.0);
    }

    #[test]
    fn output_rate_controller_is_bounded() {
        let mut controller = OutputRateController::default();
        controller.reset(OUTPUT_TARGET_QUEUE_FRAMES * 10);

        let adjustment = controller.update(OUTPUT_TARGET_QUEUE_FRAMES * 10);

        assert_eq!(adjustment, 5_000);
        assert_eq!(controller.input_step(), 1.005);
    }
}
