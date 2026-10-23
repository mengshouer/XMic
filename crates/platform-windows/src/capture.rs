use std::{
    ptr::null_mut,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

use serde::Serialize;
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::{
            eCapture, eMultimedia, eRender, IAudioCaptureClient, IAudioClient, IAudioClock,
            IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_NOPERSIST, WAVEFORMATEX,
        },
        System::{
            Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                WaitForSingleObject,
            },
        },
    },
};
use xmic_platform_api::{DataFlow, EndpointDescriptor, PlatformError};

use super::{
    container_group_id, device_id, friendly_name, operation_error, AudioFormatDescriptor,
    ComApartment,
};

const MAX_CAPTURE_DURATION: Duration = Duration::from_secs(120);
const CAPTURE_CAPACITY_MARGIN: Duration = Duration::from_secs(2);
const EVENT_WAIT_SLICE: Duration = Duration::from_millis(100);
const WAVE_FORMAT_BASE_BYTES: usize = 18;
const MAX_WAVE_FORMAT_EXTENSION_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub duration: Duration,
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockSnapshot {
    pub device_position: u64,
    pub qpc_position_100ns: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PacketTimestamp {
    pub output_frame_offset: u64,
    pub frame_count: u32,
    pub device_position: u64,
    pub qpc_position_100ns: u64,
    pub clock: ClockSnapshot,
    pub flags: u32,
    pub silent: bool,
    pub data_discontinuity: bool,
    pub timestamp_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedAudioStream {
    pub endpoint: EndpointDescriptor,
    pub format: AudioFormatDescriptor,
    pub endpoint_buffer_frames: u32,
    pub clock_frequency: u64,
    pub start_clock: ClockSnapshot,
    pub end_clock: ClockSnapshot,
    pub captured_frames: u64,
    pub silent_frames: u64,
    pub non_silent_frames: u64,
    pub wait_timeouts: u64,
    pub metadata_overflow: u64,
    pub packets: Vec<PacketTimestamp>,
    #[serde(skip)]
    pub data: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplexCapture {
    pub requested_duration_ms: u64,
    pub render: CapturedAudioStream,
    pub capture: CapturedAudioStream,
}

pub fn capture_duplex(request: CaptureRequest) -> Result<DuplexCapture, PlatformError> {
    validate_request(&request)?;

    let duration = request.duration;
    let barrier = Arc::new(Barrier::new(3));
    let render_barrier = Arc::clone(&barrier);
    let capture_barrier = Arc::clone(&barrier);
    let render_id = request.render_endpoint_id;
    let capture_id = request.capture_endpoint_id;

    let render_worker = std::thread::spawn(move || {
        capture_worker(DataFlow::Render, render_id, duration, render_barrier)
    });
    let capture_worker_handle = std::thread::spawn(move || {
        capture_worker(DataFlow::Capture, capture_id, duration, capture_barrier)
    });

    barrier.wait();

    let render = render_worker
        .join()
        .map_err(|_| PlatformError::WorkerStopped)??;
    let capture = capture_worker_handle
        .join()
        .map_err(|_| PlatformError::WorkerStopped)??;

    Ok(DuplexCapture {
        requested_duration_ms: duration.as_millis() as u64,
        render,
        capture,
    })
}

fn validate_request(request: &CaptureRequest) -> Result<(), PlatformError> {
    if request.duration.is_zero() || request.duration > MAX_CAPTURE_DURATION {
        return Err(PlatformError::Operation(format!(
            "capture duration must be between 1 ms and {} seconds",
            MAX_CAPTURE_DURATION.as_secs()
        )));
    }
    Ok(())
}

fn capture_worker(
    flow: DataFlow,
    endpoint_id: Option<String>,
    duration: Duration,
    barrier: Arc<Barrier>,
) -> Result<CapturedAudioStream, PlatformError> {
    let _com = match ComApartment::initialize_mta() {
        Ok(com) => com,
        Err(error) => {
            barrier.wait();
            return Err(error);
        }
    };

    let prepared = prepare_capture(flow, endpoint_id.as_deref(), duration);
    barrier.wait();
    record_prepared(prepared?, duration)
}

struct PreparedCapture {
    client: IAudioClient,
    capture_client: IAudioCaptureClient,
    clock: IAudioClock,
    event: EventHandle,
    endpoint: EndpointDescriptor,
    format: AudioFormatDescriptor,
    endpoint_buffer_frames: u32,
    data: Vec<u8>,
    packets: Vec<PacketTimestamp>,
}

fn prepare_capture(
    flow: DataFlow,
    endpoint_id: Option<&str>,
    duration: Duration,
) -> Result<PreparedCapture, PlatformError> {
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| operation_error("create MMDeviceEnumerator", error))?
    };
    let device = select_endpoint(&enumerator, flow, endpoint_id)?;
    let endpoint = EndpointDescriptor {
        platform: "windows".to_owned(),
        id: device_id(&device)?,
        group_id: container_group_id(&device),
        friendly_name: friendly_name(&device)?,
        flow,
        default_roles: Vec::new(),
    };
    let client: IAudioClient = unsafe {
        device
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| operation_error("activate WASAPI audio client", error))?
    };
    let mix_format = CoTaskMemWaveFormat::new(unsafe {
        client
            .GetMixFormat()
            .map_err(|error| operation_error("read endpoint mix format", error))?
    })?;
    let format = unsafe { copy_wave_format(mix_format.as_ptr())? };
    let event = EventHandle::new()?;
    let stream_flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_NOPERSIST
        | if flow == DataFlow::Render {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };

    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                0,
                0,
                mix_format.as_ptr(),
                None,
            )
            .map_err(|error| operation_error("initialize shared WASAPI capture", error))?;
        client
            .SetEventHandle(event.raw())
            .map_err(|error| operation_error("set WASAPI capture event", error))?;
    }
    let endpoint_buffer_frames = unsafe {
        client
            .GetBufferSize()
            .map_err(|error| operation_error("read WASAPI buffer size", error))?
    };
    let capture_client: IAudioCaptureClient = unsafe {
        client
            .GetService()
            .map_err(|error| operation_error("get IAudioCaptureClient", error))?
    };
    let clock: IAudioClock = unsafe {
        client
            .GetService()
            .map_err(|error| operation_error("get IAudioClock", error))?
    };
    let data_capacity = capture_data_capacity(&format, duration)?;
    let mut data = Vec::new();
    data.try_reserve_exact(data_capacity).map_err(|error| {
        PlatformError::Operation(format!("reserve audio capture buffer: {error}"))
    })?;
    data.resize(data_capacity, 0);

    let packet_capacity = packet_metadata_capacity(duration);
    let mut packets = Vec::new();
    packets
        .try_reserve_exact(packet_capacity)
        .map_err(|error| {
            PlatformError::Operation(format!("reserve packet metadata buffer: {error}"))
        })?;

    Ok(PreparedCapture {
        client,
        capture_client,
        clock,
        event,
        endpoint,
        format,
        endpoint_buffer_frames,
        data,
        packets,
    })
}

pub(super) fn select_endpoint(
    enumerator: &IMMDeviceEnumerator,
    flow: DataFlow,
    endpoint_id: Option<&str>,
) -> Result<IMMDevice, PlatformError> {
    if let Some(endpoint_id) = endpoint_id {
        let endpoint_id = HSTRING::from(endpoint_id);
        return unsafe {
            enumerator
                .GetDevice(&endpoint_id)
                .map_err(|error| operation_error("select explicit audio endpoint", error))
        };
    }

    let api_flow = match flow {
        DataFlow::Render => eRender,
        DataFlow::Capture => eCapture,
    };
    unsafe {
        enumerator
            .GetDefaultAudioEndpoint(api_flow, eMultimedia)
            .map_err(|error| operation_error("select default multimedia endpoint", error))
    }
}

fn record_prepared(
    mut prepared: PreparedCapture,
    duration: Duration,
) -> Result<CapturedAudioStream, PlatformError> {
    let _mmcss = MmcssRegistration::register()?;
    let clock_frequency = unsafe {
        prepared
            .clock
            .GetFrequency()
            .map_err(|error| operation_error("read IAudioClock frequency", error))?
    };

    unsafe {
        prepared
            .client
            .Start()
            .map_err(|error| operation_error("start WASAPI capture", error))?;
    }
    let start_clock = clock_snapshot(&prepared.clock)?;
    let deadline = Instant::now() + duration;
    let mut bytes_used = 0usize;
    let mut captured_frames = 0u64;
    let mut silent_frames = 0u64;
    let mut non_silent_frames = 0u64;
    let mut wait_timeouts = 0u64;
    let mut metadata_overflow = 0u64;
    let capture_result = (|| {
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait_duration = remaining.min(EVENT_WAIT_SLICE);
            let wait_ms = wait_duration.as_millis().max(1) as u32;
            let wait_result = unsafe { WaitForSingleObject(prepared.event.raw(), wait_ms) };

            if wait_result == WAIT_OBJECT_0 {
                drain_packets(
                    &mut prepared,
                    &mut bytes_used,
                    &mut captured_frames,
                    &mut silent_frames,
                    &mut non_silent_frames,
                    &mut metadata_overflow,
                )?;
            } else if wait_result == WAIT_TIMEOUT {
                wait_timeouts += 1;
            } else if wait_result == WAIT_FAILED {
                return Err(operation_error(
                    "wait for WASAPI capture event",
                    windows::core::Error::from_thread(),
                ));
            } else {
                return Err(PlatformError::Operation(format!(
                    "unexpected WASAPI wait result: {}",
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
            .map_err(|error| operation_error("stop WASAPI capture", error))
    };
    capture_result?;
    stop_result?;

    let end_clock = clock_snapshot(&prepared.clock)?;
    prepared.data.truncate(bytes_used);

    Ok(CapturedAudioStream {
        endpoint: prepared.endpoint,
        format: prepared.format,
        endpoint_buffer_frames: prepared.endpoint_buffer_frames,
        clock_frequency,
        start_clock,
        end_clock,
        captured_frames,
        silent_frames,
        non_silent_frames,
        wait_timeouts,
        metadata_overflow,
        packets: prepared.packets,
        data: prepared.data,
    })
}

#[allow(clippy::too_many_arguments)]
fn drain_packets(
    prepared: &mut PreparedCapture,
    bytes_used: &mut usize,
    captured_frames: &mut u64,
    silent_frames: &mut u64,
    non_silent_frames: &mut u64,
    metadata_overflow: &mut u64,
) -> Result<(), PlatformError> {
    loop {
        let next_frames = unsafe {
            prepared
                .capture_client
                .GetNextPacketSize()
                .map_err(|error| operation_error("read WASAPI packet size", error))?
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
                .map_err(|error| operation_error("acquire WASAPI packet", error))?;
        }

        let packet_result = copy_packet(
            prepared,
            source,
            frame_count,
            flags,
            device_position,
            qpc_position,
            bytes_used,
            captured_frames,
            silent_frames,
            non_silent_frames,
            metadata_overflow,
        );
        let release_result = unsafe {
            prepared
                .capture_client
                .ReleaseBuffer(frame_count)
                .map_err(|error| operation_error("release WASAPI packet", error))
        };
        packet_result?;
        release_result?;
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_packet(
    prepared: &mut PreparedCapture,
    source: *mut u8,
    frame_count: u32,
    flags: u32,
    device_position: u64,
    qpc_position: u64,
    bytes_used: &mut usize,
    captured_frames: &mut u64,
    silent_frames: &mut u64,
    non_silent_frames: &mut u64,
    metadata_overflow: &mut u64,
) -> Result<(), PlatformError> {
    let packet_bytes = (frame_count as usize)
        .checked_mul(prepared.format.block_align as usize)
        .ok_or_else(|| PlatformError::Operation("WASAPI packet byte count overflow".to_owned()))?;
    let next_bytes_used = bytes_used
        .checked_add(packet_bytes)
        .ok_or_else(|| PlatformError::Operation("capture buffer offset overflow".to_owned()))?;
    if next_bytes_used > prepared.data.len() {
        return Err(PlatformError::Operation(
            "preallocated WASAPI capture buffer exhausted".to_owned(),
        ));
    }

    let silent = flag_is_set(flags, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
    if silent {
        *silent_frames += frame_count as u64;
    } else {
        if source.is_null() {
            return Err(PlatformError::Operation(
                "WASAPI returned a null buffer without the silent flag".to_owned(),
            ));
        }
        let source = unsafe { std::slice::from_raw_parts(source, packet_bytes) };
        prepared.data[*bytes_used..next_bytes_used].copy_from_slice(source);
        *non_silent_frames += frame_count as u64;
    }

    let packet = PacketTimestamp {
        output_frame_offset: *captured_frames,
        frame_count,
        device_position,
        qpc_position_100ns: qpc_position,
        clock: clock_snapshot(&prepared.clock)?,
        flags,
        silent,
        data_discontinuity: flag_is_set(flags, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32),
        timestamp_error: flag_is_set(flags, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32),
    };
    if prepared.packets.len() < prepared.packets.capacity() {
        prepared.packets.push(packet);
    } else {
        *metadata_overflow += 1;
    }

    *bytes_used = next_bytes_used;
    *captured_frames += frame_count as u64;
    Ok(())
}

fn clock_snapshot(clock: &IAudioClock) -> Result<ClockSnapshot, PlatformError> {
    let mut device_position = 0u64;
    let mut qpc_position = 0u64;
    unsafe {
        clock
            .GetPosition(&mut device_position, Some(&mut qpc_position))
            .map_err(|error| operation_error("read IAudioClock position", error))?;
    }
    Ok(ClockSnapshot {
        device_position,
        qpc_position_100ns: qpc_position,
    })
}

pub(super) fn flag_is_set(flags: u32, flag: u32) -> bool {
    flags & flag != 0
}

fn capture_data_capacity(
    format: &AudioFormatDescriptor,
    duration: Duration,
) -> Result<usize, PlatformError> {
    let duration = duration
        .checked_add(CAPTURE_CAPACITY_MARGIN)
        .ok_or_else(|| PlatformError::Operation("capture duration overflow".to_owned()))?;
    let frames = (format.sample_rate_hz as u128)
        .checked_mul(duration.as_nanos())
        .and_then(|value| value.checked_add(999_999_999))
        .map(|value| value / 1_000_000_000)
        .ok_or_else(|| PlatformError::Operation("capture frame capacity overflow".to_owned()))?;
    let bytes = frames
        .checked_mul(format.block_align as u128)
        .ok_or_else(|| PlatformError::Operation("capture byte capacity overflow".to_owned()))?;
    usize::try_from(bytes)
        .map_err(|_| PlatformError::Operation("capture buffer is too large".to_owned()))
}

fn packet_metadata_capacity(duration: Duration) -> usize {
    let milliseconds = duration.as_millis().min(120_000) as usize;
    milliseconds.saturating_mul(2).saturating_add(2_048)
}

pub(super) unsafe fn copy_wave_format(
    format: *const WAVEFORMATEX,
) -> Result<AudioFormatDescriptor, PlatformError> {
    if format.is_null() {
        return Err(PlatformError::Operation(
            "WASAPI returned a null mix format".to_owned(),
        ));
    }
    let extension_bytes = unsafe { (*format).cbSize };
    if extension_bytes as usize > MAX_WAVE_FORMAT_EXTENSION_BYTES {
        return Err(PlatformError::Operation(format!(
            "WASAPI mix format extension is too large: {extension_bytes} bytes"
        )));
    }
    let byte_count = WAVE_FORMAT_BASE_BYTES + extension_bytes as usize;
    let bytes = unsafe { std::slice::from_raw_parts(format.cast::<u8>(), byte_count) }.to_vec();
    AudioFormatDescriptor::from_wave_format_bytes(bytes)
}

pub(super) struct CoTaskMemWaveFormat(*mut WAVEFORMATEX);

impl CoTaskMemWaveFormat {
    pub(super) fn new(format: *mut WAVEFORMATEX) -> Result<Self, PlatformError> {
        if format.is_null() {
            Err(PlatformError::Operation(
                "WASAPI returned a null mix format".to_owned(),
            ))
        } else {
            Ok(Self(format))
        }
    }

    pub(super) fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0
    }
}

impl Drop for CoTaskMemWaveFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

pub(super) struct EventHandle(HANDLE);

impl EventHandle {
    pub(super) fn new() -> Result<Self, PlatformError> {
        let handle = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map_err(|error| operation_error("create WASAPI event", error))?;
        Ok(Self(handle))
    }

    pub(super) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe { _ = CloseHandle(self.0) };
    }
}

pub(super) struct MmcssRegistration(HANDLE);

impl MmcssRegistration {
    pub(super) fn register() -> Result<Self, PlatformError> {
        let mut task_index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index) }
            .map_err(|error| operation_error("register MMCSS Audio task", error))?;
        Ok(Self(handle))
    }
}

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        unsafe { _ = AvRevertMmThreadCharacteristics(self.0) };
    }
}
