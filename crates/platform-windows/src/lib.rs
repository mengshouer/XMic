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
    start_duplex_source, DuplexSourceRequest, DuplexSourceStream, SourceCountersSnapshot,
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
            group_id: device_group_id(&device).ok(),
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
