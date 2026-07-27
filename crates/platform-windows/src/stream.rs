use std::{
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle, Thread},
    time::Instant,
};

use rtrb::{Consumer, Producer, RingBuffer};
use windows::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::Audio::{
        IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_NOPERSIST,
    },
    System::{
        Com::{CoCreateInstance, CLSCTX_ALL},
        Threading::WaitForSingleObject,
    },
};
use xmic_engine_core::{
    AdaptiveNormalizer, CaptureFrame, ChannelMapper, ClockObservation, ClockUpdate, FrameTimestamp,
    NormalizedBlock, RenderFrame, DEFAULT_QUEUE_CAPACITY_FRAMES, DSP_SAMPLES_PER_FRAME,
    DSP_SAMPLE_RATE_HZ,
};
use xmic_platform_api::{DataFlow, EndpointDescriptor, PlatformError};

use crate::{
    capture::{
        copy_wave_format, flag_is_set, select_endpoint, CoTaskMemWaveFormat, EventHandle,
        MmcssRegistration,
    },
    device_group_id, device_id, friendly_name, join_worker_with_timeout, operation_error,
    smoothed_sample, AudioFormatDescriptor, ComApartment, QpcClock, WORKER_START_TIMEOUT,
};

const SOURCE_RING_BLOCKS: usize = 32;
const EVENT_WAIT_MS: u32 = 100;
/// Packet timestamps beyond this age are treated as driver noise rather than real latency.
const MAX_CAPTURE_PACKET_LAG_100NS: u64 = 5_000_000;

#[derive(Debug, Clone)]
pub struct DuplexSourceRequest {
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
    pub capture_channel: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SourceStreamInfo {
    pub render_endpoint: EndpointDescriptor,
    pub capture_endpoint: EndpointDescriptor,
    pub render_format: AudioFormatDescriptor,
    pub capture_format: AudioFormatDescriptor,
    pub render_channels: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceCountersSnapshot {
    pub render_blocks: u64,
    pub capture_blocks: u64,
    pub render_overflows: u64,
    pub capture_overflows: u64,
    pub wait_timeouts: u64,
    pub data_discontinuities: u64,
    pub non_finite_samples_replaced: u64,
    /// Smoothed age of a capture packet when the WASAPI worker receives it, in 100 ns units. This is
    /// the measured device-to-process latency; 0 means no usable packet timestamp yet.
    pub capture_packet_lag_100ns: u64,
}

#[derive(Default)]
struct SourceCounters {
    render_blocks: AtomicU64,
    capture_blocks: AtomicU64,
    render_overflows: AtomicU64,
    capture_overflows: AtomicU64,
    wait_timeouts: AtomicU64,
    data_discontinuities: AtomicU64,
    non_finite_samples_replaced: AtomicU64,
    capture_packet_lag_100ns: AtomicU64,
}

impl SourceCounters {
    fn snapshot(&self) -> SourceCountersSnapshot {
        SourceCountersSnapshot {
            render_blocks: self.render_blocks.load(Ordering::Relaxed),
            capture_blocks: self.capture_blocks.load(Ordering::Relaxed),
            render_overflows: self.render_overflows.load(Ordering::Relaxed),
            capture_overflows: self.capture_overflows.load(Ordering::Relaxed),
            wait_timeouts: self.wait_timeouts.load(Ordering::Relaxed),
            data_discontinuities: self.data_discontinuities.load(Ordering::Relaxed),
            non_finite_samples_replaced: self.non_finite_samples_replaced.load(Ordering::Relaxed),
            capture_packet_lag_100ns: self.capture_packet_lag_100ns.load(Ordering::Relaxed),
        }
    }

    /// Only the capture worker writes this, so a plain load/store pair is enough.
    fn observe_capture_packet_lag(&self, sample_100ns: u64) {
        let previous = self.capture_packet_lag_100ns.load(Ordering::Relaxed);
        self.capture_packet_lag_100ns
            .store(smoothed_sample(previous, sample_100ns), Ordering::Relaxed);
    }
}

pub struct DuplexSourceStream {
    info: SourceStreamInfo,
    render_consumer: Consumer<RenderFrame>,
    capture_consumer: Consumer<CaptureFrame>,
    stop: Arc<AtomicBool>,
    counters: Arc<SourceCounters>,
    workers: Vec<JoinHandle<Result<(), PlatformError>>>,
}

impl DuplexSourceStream {
    pub fn info(&self) -> &SourceStreamInfo {
        &self.info
    }

    pub fn pop_render(&mut self) -> Option<RenderFrame> {
        self.render_consumer.pop().ok()
    }

    pub fn pop_capture(&mut self) -> Option<CaptureFrame> {
        self.capture_consumer.pop().ok()
    }

    /// Capture frames already normalized and waiting for the DSP thread, including the next one.
    pub fn capture_queue_blocks(&self) -> usize {
        self.capture_consumer.slots()
    }

    pub fn counters(&self) -> SourceCountersSnapshot {
        self.counters.snapshot()
    }

    pub fn worker_finished(&self) -> bool {
        self.workers.iter().any(JoinHandle::is_finished)
    }

    pub fn stop(mut self) -> Result<(), PlatformError> {
        self.request_stop();
        self.join_workers()
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn join_workers(&mut self) -> Result<(), PlatformError> {
        let mut first_error = None;
        for worker in self.workers.drain(..) {
            let result = join_worker_with_timeout(worker, "WASAPI source worker")?;
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for DuplexSourceStream {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_workers();
    }
}

pub fn start_duplex_source(
    request: DuplexSourceRequest,
) -> Result<DuplexSourceStream, PlatformError> {
    let (render_producer, render_consumer) = RingBuffer::new(SOURCE_RING_BLOCKS);
    let (capture_producer, capture_consumer) = RingBuffer::new(SOURCE_RING_BLOCKS);
    let stop = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(SourceCounters::default());
    let wake_thread = thread::current();

    let render_worker = spawn_source_worker(
        SourceWorkerRequest {
            flow: DataFlow::Render,
            endpoint_id: request.render_endpoint_id,
            capture_channel: None,
        },
        FrameProducer::Render(render_producer),
        Arc::clone(&stop),
        Arc::clone(&counters),
        wake_thread.clone(),
    );
    let capture_worker = spawn_source_worker(
        SourceWorkerRequest {
            flow: DataFlow::Capture,
            endpoint_id: request.capture_endpoint_id,
            capture_channel: request.capture_channel,
        },
        FrameProducer::Capture(capture_producer),
        Arc::clone(&stop),
        Arc::clone(&counters),
        wake_thread,
    );

    let startup_deadline = Instant::now() + WORKER_START_TIMEOUT;
    let render_ready = match receive_source_ready(&render_worker.ready, startup_deadline, "render")
    {
        Ok(ready) => ready,
        Err(error) => {
            stop.store(true, Ordering::Release);
            drop(render_worker.handle);
            drop(capture_worker.handle);
            return Err(error);
        }
    };
    let capture_ready =
        match receive_source_ready(&capture_worker.ready, startup_deadline, "capture") {
            Ok(ready) => ready,
            Err(error) => {
                stop.store(true, Ordering::Release);
                drop(render_worker.handle);
                drop(capture_worker.handle);
                return Err(error);
            }
        };
    let workers = vec![render_worker.handle, capture_worker.handle];
    let info = match (render_ready, capture_ready) {
        (Ok(render), Ok(capture)) => SourceStreamInfo {
            render_endpoint: render.endpoint,
            capture_endpoint: capture.endpoint,
            render_format: render.format,
            capture_format: capture.format,
            render_channels: render.output_channels,
        },
        (render, capture) => {
            stop.store(true, Ordering::Release);
            for worker in workers {
                drop(worker);
            }
            return Err(PlatformError::Operation(
                render.err().or_else(|| capture.err()).unwrap_or_else(|| {
                    "WASAPI source preparation failed without an error".to_owned()
                }),
            ));
        }
    };

    Ok(DuplexSourceStream {
        info,
        render_consumer,
        capture_consumer,
        stop,
        counters,
        workers,
    })
}

fn receive_source_ready(
    receiver: &mpsc::Receiver<Result<PreparedSourceInfo, String>>,
    deadline: Instant,
    flow: &str,
) -> Result<Result<PreparedSourceInfo, String>, PlatformError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(ready) => Ok(ready),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::Operation(format!(
            "WASAPI {flow} preparation timed out after 5 seconds"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(PlatformError::WorkerStopped),
    }
}

struct SpawnedSourceWorker {
    ready: mpsc::Receiver<Result<PreparedSourceInfo, String>>,
    handle: JoinHandle<Result<(), PlatformError>>,
}

fn spawn_source_worker(
    request: SourceWorkerRequest,
    producer: FrameProducer,
    stop: Arc<AtomicBool>,
    counters: Arc<SourceCounters>,
    wake_thread: Thread,
) -> SpawnedSourceWorker {
    let (ready_tx, ready) = mpsc::sync_channel(1);
    let handle = thread::spawn(move || {
        let mut com_guard = None;
        let prepared = match ComApartment::initialize_mta() {
            Ok(com) => {
                com_guard = Some(com);
                prepare_source(request, producer, counters, wake_thread)
            }
            Err(error) => Err(error),
        };
        let ready_message = prepared
            .as_ref()
            .map(PreparedSource::info)
            .map_err(ToString::to_string);
        let _ = ready_tx.send(ready_message);
        let _com_guard = com_guard;
        run_source(prepared?, &stop)
    });
    SpawnedSourceWorker { ready, handle }
}

#[derive(Debug)]
struct SourceWorkerRequest {
    flow: DataFlow,
    endpoint_id: Option<String>,
    capture_channel: Option<usize>,
}

#[derive(Debug, Clone)]
struct PreparedSourceInfo {
    endpoint: EndpointDescriptor,
    format: AudioFormatDescriptor,
    output_channels: usize,
}

enum FrameProducer {
    Render(Producer<RenderFrame>),
    Capture(Producer<CaptureFrame>),
}

struct PreparedSource {
    client: IAudioClient,
    capture_client: IAudioCaptureClient,
    event: EventHandle,
    endpoint: EndpointDescriptor,
    format: AudioFormatDescriptor,
    endpoint_buffer_frames: usize,
    mapper: ChannelMapper,
    normalizer: AdaptiveNormalizer,
    decoded: Vec<f32>,
    mapped: Vec<f32>,
    normalized: NormalizedBlock,
    timeline: OutputTimeline,
    clock: QpcClock,
    producer: FrameProducer,
    counters: Arc<SourceCounters>,
    wake_thread: Thread,
}

impl PreparedSource {
    fn info(&self) -> PreparedSourceInfo {
        PreparedSourceInfo {
            endpoint: self.endpoint.clone(),
            format: self.format.clone(),
            output_channels: self.mapper.output_channels(),
        }
    }
}

fn prepare_source(
    request: SourceWorkerRequest,
    producer: FrameProducer,
    counters: Arc<SourceCounters>,
    wake_thread: Thread,
) -> Result<PreparedSource, PlatformError> {
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| operation_error("create MMDeviceEnumerator", error))?
    };
    let device = select_endpoint(&enumerator, request.flow, request.endpoint_id.as_deref())?;
    let endpoint = EndpointDescriptor {
        platform: "windows".to_owned(),
        id: device_id(&device)?,
        group_id: device_group_id(&device).ok(),
        friendly_name: friendly_name(&device)?,
        flow: request.flow,
        default_roles: Vec::new(),
    };
    let client: IAudioClient = unsafe {
        device
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| operation_error("activate WASAPI source client", error))?
    };
    let mix_format = CoTaskMemWaveFormat::new(unsafe {
        client
            .GetMixFormat()
            .map_err(|error| operation_error("read source mix format", error))?
    })?;
    let format = unsafe { copy_wave_format(mix_format.as_ptr())? };
    let event = EventHandle::new()?;
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_NOPERSIST
        | if request.flow == DataFlow::Render {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                0,
                0,
                mix_format.as_ptr(),
                None,
            )
            .map_err(|error| operation_error("initialize shared WASAPI source", error))?;
        client
            .SetEventHandle(event.raw())
            .map_err(|error| operation_error("set WASAPI source event", error))?;
    }
    let endpoint_buffer_frames = unsafe {
        client
            .GetBufferSize()
            .map_err(|error| operation_error("read source buffer size", error))?
    } as usize;
    let capture_client: IAudioCaptureClient = unsafe {
        client
            .GetService()
            .map_err(|error| operation_error("get IAudioCaptureClient", error))?
    };
    let input_channels = format.channels as usize;
    let mapper = match request.flow {
        DataFlow::Render => ChannelMapper::render(input_channels, format.channel_mask),
        DataFlow::Capture => match request.capture_channel {
            Some(channel) => ChannelMapper::capture(input_channels, channel),
            None => ChannelMapper::capture_auto(input_channels),
        },
    }
    .map_err(|error| PlatformError::Operation(format!("configure channel mapping: {error}")))?;
    let output_channels = mapper.output_channels();
    let normalizer = AdaptiveNormalizer::new(
        format.sample_rate_hz,
        output_channels,
        DEFAULT_QUEUE_CAPACITY_FRAMES,
    )
    .map_err(|error| PlatformError::Operation(format!("create source normalizer: {error}")))?;
    let decoded_samples = endpoint_buffer_frames
        .checked_mul(input_channels)
        .ok_or_else(|| PlatformError::Operation("source decode buffer overflow".to_owned()))?;
    let mapped_samples = endpoint_buffer_frames
        .checked_mul(output_channels)
        .ok_or_else(|| PlatformError::Operation("source map buffer overflow".to_owned()))?;

    Ok(PreparedSource {
        client,
        capture_client,
        event,
        endpoint,
        format,
        endpoint_buffer_frames,
        mapper,
        normalizer,
        decoded: vec![0.0; decoded_samples],
        mapped: vec![0.0; mapped_samples],
        normalized: NormalizedBlock::silence(output_channels)
            .map_err(|error| PlatformError::Operation(error.to_string()))?,
        timeline: OutputTimeline::default(),
        clock: QpcClock::new()?,
        producer,
        counters,
        wake_thread,
    })
}

fn run_source(mut prepared: PreparedSource, stop: &AtomicBool) -> Result<(), PlatformError> {
    let _mmcss = MmcssRegistration::register()?;
    unsafe {
        prepared
            .client
            .Start()
            .map_err(|error| operation_error("start WASAPI source", error))?;
    }
    let run_result = (|| {
        while !stop.load(Ordering::Acquire) {
            let wait_result = unsafe { WaitForSingleObject(prepared.event.raw(), EVENT_WAIT_MS) };
            if wait_result == WAIT_OBJECT_0 {
                drain_source_packets(&mut prepared)?;
            } else if wait_result == WAIT_TIMEOUT {
                prepared
                    .counters
                    .wait_timeouts
                    .fetch_add(1, Ordering::Relaxed);
            } else if wait_result == WAIT_FAILED {
                return Err(operation_error(
                    "wait for WASAPI source event",
                    windows::core::Error::from_win32(),
                ));
            } else {
                return Err(PlatformError::Operation(format!(
                    "unexpected WASAPI source wait result: {}",
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
            .map_err(|error| operation_error("stop WASAPI source", error))
    };
    run_result?;
    stop_result
}

fn drain_source_packets(prepared: &mut PreparedSource) -> Result<(), PlatformError> {
    loop {
        let next_frames = unsafe {
            prepared
                .capture_client
                .GetNextPacketSize()
                .map_err(|error| operation_error("read streaming WASAPI packet size", error))?
        };
        if next_frames == 0 {
            return Ok(());
        }

        let mut source = null_mut();
        let mut frame_count = 0u32;
        let mut flags = 0u32;
        let mut device_position = 0u64;
        let mut qpc_position = 0u64;
        unsafe {
            prepared
                .capture_client
                .GetBuffer(
                    &mut source,
                    &mut frame_count,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                )
                .map_err(|error| operation_error("acquire streaming WASAPI packet", error))?;
        }
        let packet_result = process_source_packet(
            prepared,
            source,
            frame_count as usize,
            flags,
            device_position,
            qpc_position,
        );
        let release_result = unsafe {
            prepared
                .capture_client
                .ReleaseBuffer(frame_count)
                .map_err(|error| operation_error("release streaming WASAPI packet", error))
        };
        packet_result?;
        release_result?;
    }
}

fn process_source_packet(
    prepared: &mut PreparedSource,
    source: *mut u8,
    frame_count: usize,
    flags: u32,
    device_position: u64,
    qpc_position: u64,
) -> Result<(), PlatformError> {
    if frame_count > prepared.endpoint_buffer_frames {
        return Err(PlatformError::Operation(format!(
            "WASAPI source packet has {frame_count} frames; buffer supports {}",
            prepared.endpoint_buffer_frames
        )));
    }
    let input_samples = frame_count * prepared.format.channels as usize;
    let decoded = &mut prepared.decoded[..input_samples];
    let silent = flag_is_set(flags, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
    if silent {
        decoded.fill(0.0);
    } else {
        if source.is_null() {
            return Err(PlatformError::Operation(
                "WASAPI returned a null streaming buffer without the silent flag".to_owned(),
            ));
        }
        let byte_count = frame_count * prepared.format.block_align as usize;
        let bytes = unsafe { std::slice::from_raw_parts(source, byte_count) };
        let replaced = prepared
            .format
            .decode_interleaved_f32_into(bytes, decoded)?;
        prepared
            .counters
            .non_finite_samples_replaced
            .fetch_add(replaced, Ordering::Relaxed);
    }

    let mapped_samples = frame_count * prepared.mapper.output_channels();
    prepared
        .mapper
        .map_interleaved(decoded, &mut prepared.mapped[..mapped_samples])
        .map_err(|error| PlatformError::Operation(format!("map source packet: {error}")))?;
    let discontinuity = flag_is_set(flags, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32);
    let timestamp_error = flag_is_set(flags, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32);
    if discontinuity || timestamp_error {
        prepared
            .counters
            .data_discontinuities
            .fetch_add(1, Ordering::Relaxed);
    }
    observe_capture_packet_lag(prepared, qpc_position, timestamp_error);
    let clock_update = prepared
        .normalizer
        .observe_clock(ClockObservation {
            device_position_frames: device_position,
            qpc_position_100ns: qpc_position,
            discontinuity,
            timestamp_error,
        })
        .map_err(|error| PlatformError::Operation(format!("observe source clock: {error}")))?;
    if clock_update == ClockUpdate::Reanchored {
        prepared.timeline.reset();
    }
    if !timestamp_error {
        prepared.timeline.observe_packet(
            device_position,
            qpc_position,
            prepared.format.sample_rate_hz,
        );
    }
    prepared
        .normalizer
        .push_interleaved(&prepared.mapped[..mapped_samples])
        .map_err(|error| PlatformError::Operation(format!("queue source packet: {error}")))?;

    while prepared.normalizer.queue_fill_frames() >= prepared.normalizer.input_frames_needed() {
        if !prepared
            .normalizer
            .process_next(&mut prepared.normalized)
            .map_err(|error| {
                PlatformError::Operation(format!("normalize source packet: {error}"))
            })?
        {
            break;
        }
        let timestamp = prepared.timeline.next_timestamp();
        push_normalized(prepared, timestamp);
    }
    Ok(())
}

/// Measures how stale the packet already is when the capture worker receives it. The render loopback
/// stream shares these counters but only the microphone path feeds the DSP, so it is skipped.
fn observe_capture_packet_lag(
    prepared: &PreparedSource,
    qpc_position_100ns: u64,
    timestamp_error: bool,
) {
    if !matches!(prepared.producer, FrameProducer::Capture(_))
        || timestamp_error
        || qpc_position_100ns == 0
    {
        return;
    }
    let now_100ns = prepared.clock.now_100ns();
    if now_100ns <= qpc_position_100ns {
        return;
    }
    let lag_100ns = now_100ns - qpc_position_100ns;
    if lag_100ns <= MAX_CAPTURE_PACKET_LAG_100NS {
        prepared.counters.observe_capture_packet_lag(lag_100ns);
    }
}

fn push_normalized(prepared: &mut PreparedSource, timestamp: FrameTimestamp) {
    let pushed = match &mut prepared.producer {
        FrameProducer::Render(producer) => {
            let frame = RenderFrame {
                timestamp,
                channels: prepared.normalized.channels,
                samples: prepared.normalized.samples,
            };
            let pushed = producer.push(frame).is_ok();
            if pushed {
                prepared
                    .counters
                    .render_blocks
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                prepared
                    .counters
                    .render_overflows
                    .fetch_add(1, Ordering::Relaxed);
            }
            pushed
        }
        FrameProducer::Capture(producer) => {
            let frame = CaptureFrame {
                timestamp,
                samples: prepared.normalized.samples[0],
            };
            let pushed = producer.push(frame).is_ok();
            if pushed {
                prepared
                    .counters
                    .capture_blocks
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                prepared
                    .counters
                    .capture_overflows
                    .fetch_add(1, Ordering::Relaxed);
            }
            pushed
        }
    };
    if pushed {
        prepared.wake_thread.unpark();
    }
}

#[derive(Default)]
struct OutputTimeline {
    anchor_device_position: Option<u64>,
    anchor_qpc_100ns: Option<u64>,
    output_frames: u64,
}

impl OutputTimeline {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn observe_packet(
        &mut self,
        device_position: u64,
        qpc_position_100ns: u64,
        input_rate_hz: u32,
    ) {
        if self.anchor_qpc_100ns.is_none() && qpc_position_100ns != 0 {
            self.anchor_qpc_100ns = Some(qpc_position_100ns);
            self.anchor_device_position = Some(
                device_position.saturating_mul(DSP_SAMPLE_RATE_HZ as u64)
                    / u64::from(input_rate_hz),
            );
        }
    }

    fn next_timestamp(&mut self) -> FrameTimestamp {
        let output_offset = self.output_frames;
        self.output_frames = self
            .output_frames
            .saturating_add(DSP_SAMPLES_PER_FRAME as u64);
        FrameTimestamp {
            device_position_frames: self
                .anchor_device_position
                .unwrap_or(0)
                .saturating_add(output_offset),
            qpc_position_100ns: self.anchor_qpc_100ns.unwrap_or(0).saturating_add(
                output_offset.saturating_mul(10_000_000) / DSP_SAMPLE_RATE_HZ as u64,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_timeline_advances_by_ten_milliseconds() {
        let mut timeline = OutputTimeline::default();
        timeline.observe_packet(48_000, 50_000_000, 48_000);

        let first = timeline.next_timestamp();
        let second = timeline.next_timestamp();

        assert_eq!(first.device_position_frames, 48_000);
        assert_eq!(second.device_position_frames, 48_480);
        assert_eq!(
            second.qpc_position_100ns - first.qpc_position_100ns,
            100_000
        );
    }

    #[test]
    fn output_timeline_reanchors_after_reset() {
        let mut timeline = OutputTimeline::default();
        timeline.observe_packet(48_000, 50_000_000, 48_000);
        let _ = timeline.next_timestamp();

        timeline.reset();
        timeline.observe_packet(96_000, 100_000_000, 48_000);

        let timestamp = timeline.next_timestamp();
        assert_eq!(timestamp.device_position_frames, 96_000);
        assert_eq!(timestamp.qpc_position_100ns, 100_000_000);
    }
}
