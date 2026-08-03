use std::{
    collections::HashMap,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    core::PWSTR,
    Win32::{
        Devices::FunctionDiscovery::{PKEY_Device_ContainerId, PKEY_Device_FriendlyName},
        Foundation::PROPERTYKEY,
        Media::Audio::{
            eCapture, eCommunications, eConsole, eMultimedia, eRender, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            StructuredStorage::PropVariantToStringAlloc, CLSCTX_ALL, COINIT_MULTITHREADED,
            STGM_READ,
        },
        System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    },
};
use xmic_platform_api::{AudioPlatform, DataFlow, DeviceRole, EndpointDescriptor, PlatformError};

mod capture;
mod format;
mod ipc;
mod monitor;
mod output;
mod stream;
mod system_lifecycle;

const WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const QPC_UNITS_PER_SECOND: i64 = 10_000_000;
const LATENCY_SMOOTHING_SHIFT: u32 = 3;
/// Windows hands out this well-known container ID for endpoints that belong to no device container,
/// and GUID_NULL for drivers that leave the property unset.
const CONTAINERLESS_DEVICE_IDS: [&str; 2] = [
    "{00000000-0000-0000-FFFF-FFFFFFFFFFFF}",
    "{00000000-0000-0000-0000-000000000000}",
];

/// WASAPI reports packet timestamps as performance-counter values already converted to 100 ns
/// units, so measuring how stale a packet is when the worker sees it needs "now" in the same unit.
pub(crate) struct QpcClock {
    frequency: i64,
}

impl QpcClock {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let mut frequency = 0i64;
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .map_err(|error| operation_error("read performance counter frequency", error))?;
        if frequency <= 0 {
            return Err(PlatformError::Operation(
                "performance counter frequency is not positive".to_owned(),
            ));
        }
        Ok(Self { frequency })
    }

    /// Returns 0 when the counter cannot be read so callers simply skip the sample.
    pub(crate) fn now_100ns(&self) -> u64 {
        let mut counter = 0i64;
        if unsafe { QueryPerformanceCounter(&mut counter) }.is_err() || counter < 0 {
            return 0;
        }
        u64::try_from(
            i128::from(counter) * i128::from(QPC_UNITS_PER_SECOND) / i128::from(self.frequency),
        )
        .unwrap_or(0)
    }
}

/// Exponential average with a 1/8 step. Latency samples jitter by a frame or more, and the workers
/// can only publish through a single atomic, so the smoothing happens where the sample is taken.
pub(crate) fn smoothed_sample(previous: u64, sample: u64) -> u64 {
    if previous == 0 {
        return sample;
    }
    previous - (previous >> LATENCY_SMOOTHING_SHIFT) + (sample >> LATENCY_SMOOTHING_SHIFT)
}

fn join_worker_with_timeout<T>(
    worker: JoinHandle<T>,
    description: &str,
) -> Result<T, PlatformError> {
    let deadline = Instant::now() + WORKER_STOP_TIMEOUT;
    while !worker.is_finished() {
        if Instant::now() >= deadline {
            drop(worker);
            return Err(PlatformError::Operation(format!(
                "{description} did not stop within 2 seconds"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
    worker.join().map_err(|_| PlatformError::WorkerStopped)
}

pub use capture::{
    capture_duplex, CaptureRequest, CapturedAudioStream, DuplexCapture, PacketTimestamp,
};
pub use format::{AudioFormatDescriptor, DecodedInterleavedAudio, SampleEncoding};
pub use ipc::{connect_local_ipc, wait_for_process_exit, LocalIpcServer};
pub use monitor::{start_device_change_monitor, DeviceChangeMonitor};
pub use output::{
    start_output_sink, OutputCountersSnapshot, OutputSinkRequest, OutputSinkStream,
    OutputStreamInfo,
};
pub use stream::{
    start_capture_source, start_duplex_source, CaptureSourceInfo, CaptureSourceRequest,
    CaptureSourceStream, DuplexSourceRequest, DuplexSourceStream, SourceCountersSnapshot,
    SourceStreamInfo,
};
pub use system_lifecycle::{start_system_lifecycle_monitor, SystemLifecycleMonitor};

pub struct RealtimeAudioThreadRegistration {
    _registration: capture::MmcssRegistration,
}

pub fn register_realtime_audio_thread() -> Result<RealtimeAudioThreadRegistration, PlatformError> {
    Ok(RealtimeAudioThreadRegistration {
        _registration: capture::MmcssRegistration::register()?,
    })
}

#[derive(Debug, Default)]
pub struct WasapiPlatform;

impl WasapiPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl AudioPlatform for WasapiPlatform {
    fn active_endpoints(&self) -> Result<Vec<EndpointDescriptor>, PlatformError> {
        enumerate_active_endpoints()
    }
}

pub fn enumerate_active_endpoints() -> Result<Vec<EndpointDescriptor>, PlatformError> {
    std::thread::spawn(enumerate_on_mta)
        .join()
        .map_err(|_| PlatformError::WorkerStopped)?
}

fn enumerate_on_mta() -> Result<Vec<EndpointDescriptor>, PlatformError> {
    let _com = ComApartment::initialize_mta()?;
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| operation_error("create MMDeviceEnumerator", error))?
    };

    let defaults = collect_defaults(&enumerator)?;
    let mut endpoints = Vec::new();
    enumerate_flow(
        &enumerator,
        eRender,
        DataFlow::Render,
        &defaults,
        &mut endpoints,
    )?;
    enumerate_flow(
        &enumerator,
        eCapture,
        DataFlow::Capture,
        &defaults,
        &mut endpoints,
    )?;
    endpoints.sort_by(|left, right| {
        (flow_order(left.flow), &left.friendly_name)
            .cmp(&(flow_order(right.flow), &right.friendly_name))
    });
    Ok(endpoints)
}

fn collect_defaults(
    enumerator: &IMMDeviceEnumerator,
) -> Result<HashMap<String, Vec<DeviceRole>>, PlatformError> {
    let mut defaults = HashMap::<String, Vec<DeviceRole>>::new();
    for flow in [eRender, eCapture] {
        for (role, api_role) in [
            (DeviceRole::Console, eConsole),
            (DeviceRole::Multimedia, eMultimedia),
            (DeviceRole::Communications, eCommunications),
        ] {
            if let Ok(device) = unsafe { enumerator.GetDefaultAudioEndpoint(flow, api_role) } {
                let id = device_id(&device)?;
                defaults.entry(id).or_default().push(role);
            }
        }
    }
    Ok(defaults)
}

fn enumerate_flow(
    enumerator: &IMMDeviceEnumerator,
    api_flow: windows::Win32::Media::Audio::EDataFlow,
    flow: DataFlow,
    defaults: &HashMap<String, Vec<DeviceRole>>,
    output: &mut Vec<EndpointDescriptor>,
) -> Result<(), PlatformError> {
    let collection = unsafe {
        enumerator
            .EnumAudioEndpoints(api_flow, DEVICE_STATE_ACTIVE)
            .map_err(|error| operation_error("enumerate active audio endpoints", error))?
    };
    let count = unsafe {
        collection
            .GetCount()
            .map_err(|error| operation_error("read audio endpoint count", error))?
    };

    for index in 0..count {
        let device = unsafe {
            collection
                .Item(index)
                .map_err(|error| operation_error("read audio endpoint", error))?
        };
        let id = device_id(&device)?;
        output.push(EndpointDescriptor {
            platform: "windows".to_owned(),
            friendly_name: friendly_name(&device)?,
            group_id: container_group_id(&device),
            default_roles: defaults.get(&id).cloned().unwrap_or_default(),
            id,
            flow,
        });
    }
    Ok(())
}

fn device_id(device: &IMMDevice) -> Result<String, PlatformError> {
    let value = unsafe {
        device
            .GetId()
            .map_err(|error| operation_error("read audio endpoint ID", error))?
    };
    let text = unsafe { value.to_string() }
        .map_err(|error| PlatformError::Operation(format!("decode audio endpoint ID: {error}")));
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    text
}

fn friendly_name(device: &IMMDevice) -> Result<String, PlatformError> {
    string_property(device, &PKEY_Device_FriendlyName, "friendly name")
}

fn device_group_id(device: &IMMDevice) -> Result<String, PlatformError> {
    string_property(device, &PKEY_Device_ContainerId, "container ID")
}

/// The container ID is only reported when it actually identifies a physical device.
///
/// Callers use a shared group to decide that two endpoints are the same piece of hardware — that is
/// how the virtual cable's write and read ends are recognised as one device, and how a playback
/// reference is rejected for being the virtual microphone. Windows returns a placeholder for every
/// endpoint without a container, so passing it on made unrelated devices look identical: a virtual
/// cable, a second virtual cable and the onboard S/PDIF output all carry the same placeholder, which
/// turned an ordinary default-device change into a phantom feedback-route conflict.
fn container_group_id(device: &IMMDevice) -> Option<String> {
    device_group_id(device)
        .ok()
        .filter(|id| !is_placeholder_container(id))
}

fn is_placeholder_container(id: &str) -> bool {
    let id = id.trim();
    id.is_empty()
        || CONTAINERLESS_DEVICE_IDS
            .iter()
            .any(|placeholder| id.eq_ignore_ascii_case(placeholder))
}

fn string_property(
    device: &IMMDevice,
    key: &PROPERTYKEY,
    description: &str,
) -> Result<String, PlatformError> {
    let store = unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .map_err(|error| operation_error("open audio endpoint properties", error))?
    };
    let value = unsafe {
        store.GetValue(key).map_err(|error| {
            operation_error(&format!("read audio endpoint {description}"), error)
        })?
    };
    let text: PWSTR = unsafe {
        PropVariantToStringAlloc(&value).map_err(|error| {
            operation_error(&format!("convert audio endpoint {description}"), error)
        })?
    };
    let result = unsafe { text.to_string() }.map_err(|error| {
        PlatformError::Operation(format!("decode audio endpoint {description}: {error}"))
    });
    unsafe { CoTaskMemFree(Some(text.0.cast())) };
    result
}

fn flow_order(flow: DataFlow) -> u8 {
    match flow {
        DataFlow::Render => 0,
        DataFlow::Capture => 1,
    }
}

fn operation_error(context: &str, error: windows::core::Error) -> PlatformError {
    PlatformError::Operation(format!("{context}: {error}"))
}

struct ComApartment;

impl ComApartment {
    fn initialize_mta() -> Result<Self, PlatformError> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|error| operation_error("initialize COM MTA", error))?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_starts_at_the_first_sample() {
        assert_eq!(smoothed_sample(0, 120_000), 120_000);
    }

    #[test]
    fn smoothing_moves_an_eighth_of_the_way() {
        assert_eq!(smoothed_sample(80_000, 160_000), 90_000);
        assert_eq!(smoothed_sample(80_000, 0), 70_000);
    }

    #[test]
    fn smoothing_converges_on_a_stable_sample() {
        let mut value = 0;
        for _ in 0..64 {
            value = smoothed_sample(value, 100_000);
        }

        assert_eq!(value, 100_000);
    }

    #[test]
    fn qpc_clock_advances() {
        let clock = QpcClock::new().unwrap();
        let first = clock.now_100ns();
        let second = clock.now_100ns();

        assert!(first > 0);
        assert!(second >= first);
    }

    #[test]
    fn containerless_endpoints_do_not_share_a_group() {
        // The placeholder Windows reports for endpoints without a device container, in both the casing
        // it uses and the one a driver might, plus GUID_NULL.
        assert!(is_placeholder_container(
            "{00000000-0000-0000-FFFF-FFFFFFFFFFFF}"
        ));
        assert!(is_placeholder_container(
            "{00000000-0000-0000-ffff-ffffffffffff}"
        ));
        assert!(is_placeholder_container(
            "{00000000-0000-0000-0000-000000000000}"
        ));
        assert!(is_placeholder_container("  "));

        // A real container has to keep grouping its endpoints, or a headset's speaker and microphone
        // stop being recognised as one device.
        assert!(!is_placeholder_container(
            "{D94D4918-B1FD-5811-B71B-231CD8315B15}"
        ));
    }
}
