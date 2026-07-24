use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    core::{implement, PCWSTR},
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::{
            EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
            IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
        },
        System::Com::{CoCreateInstance, CLSCTX_ALL},
    },
};
use xmic_platform_api::PlatformError;

use crate::{join_worker_with_timeout, operation_error, ComApartment, WORKER_START_TIMEOUT};

pub struct DeviceChangeMonitor {
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), PlatformError>>>,
}

impl DeviceChangeMonitor {
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
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
            join_worker_with_timeout(worker, "device change monitor")?
        })
    }
}

impl Drop for DeviceChangeMonitor {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_worker();
    }
}

pub fn start_device_change_monitor() -> Result<DeviceChangeMonitor, PlatformError> {
    let generation = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker_generation = Arc::clone(&generation);
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        let result = run_monitor(worker_generation, worker_stop, &ready_tx);
        if result.is_err() {
            let _ = ready_tx.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        }
        result
    });

    match ready_rx.recv_timeout(WORKER_START_TIMEOUT) {
        Ok(Ok(())) => Ok(DeviceChangeMonitor {
            generation,
            stop,
            worker: Some(worker),
        }),
        Ok(Err(error)) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            Err(PlatformError::Operation(error))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            Err(PlatformError::Operation(
                "device change monitor preparation timed out after 5 seconds".to_owned(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop.store(true, Ordering::Release);
            drop(worker);
            Err(PlatformError::WorkerStopped)
        }
    }
}

fn run_monitor(
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), PlatformError> {
    let _com = ComApartment::initialize_mta()?;
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|error| operation_error("create notification MMDeviceEnumerator", error))?
    };
    let callback: IMMNotificationClient = DeviceNotificationClient { generation }.into();
    unsafe {
        enumerator
            .RegisterEndpointNotificationCallback(&callback)
            .map_err(|error| operation_error("register endpoint notification", error))?;
    }
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        enumerator
            .UnregisterEndpointNotificationCallback(&callback)
            .map_err(|error| operation_error("unregister endpoint notification", error))?;
    }
    Ok(())
}

#[implement(IMMNotificationClient)]
struct DeviceNotificationClient {
    generation: Arc<AtomicU64>,
}

impl DeviceNotificationClient {
    fn mark_changed(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }
}

impl IMMNotificationClient_Impl for DeviceNotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        self.mark_changed();
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        self.mark_changed();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        self.mark_changed();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.mark_changed();
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_callback_updates_generation_without_allocating() {
        let generation = Arc::new(AtomicU64::new(0));
        let callback = DeviceNotificationClient {
            generation: Arc::clone(&generation),
        };

        callback.mark_changed();

        assert_eq!(generation.load(Ordering::Acquire), 1);
    }
}
