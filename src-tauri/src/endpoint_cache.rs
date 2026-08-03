use std::sync::{Arc, Mutex, MutexGuard};

use xmic_platform_api::{AudioPlatform, EndpointDescriptor};
use xmic_platform_windows::{start_device_change_monitor, DeviceChangeMonitor, WasapiPlatform};

/// The active audio endpoints, re-enumerated only when Windows says the device set changed.
///
/// Enumerating is not cheap. It spawns a thread to reach an MTA apartment, creates an
/// `IMMDeviceEnumerator`, asks for six default endpoints, and then opens two property stores for
/// every endpoint it finds. The UI wants the list on every status poll — about once a second while
/// the window is open — and the answer is identical until a device is added, removed, or made the
/// default for a role.
///
/// The invalidation signal is the endpoint notification client the audio engine already uses to
/// rebuild its session on a device change. Here it only has to move a counter.
pub(crate) struct EndpointCache {
    monitor: Option<DeviceChangeMonitor>,
    cached: Mutex<Option<Snapshot>>,
}

struct Snapshot {
    generation: u64,
    endpoints: Arc<Vec<EndpointDescriptor>>,
}

impl EndpointCache {
    /// A monitor that refuses to start is not fatal: with no invalidation signal the cache
    /// enumerates every time, which is exactly what it replaced.
    pub(crate) fn new() -> Self {
        Self {
            monitor: start_device_change_monitor().ok(),
            cached: Mutex::new(None),
        }
    }

    pub(crate) fn endpoints(&self) -> Result<Arc<Vec<EndpointDescriptor>>, String> {
        let Some(generation) = self.generation() else {
            return enumerate().map(Arc::new);
        };
        // Held across the enumeration on purpose: a second caller that arrives meanwhile waits, then
        // finds the snapshot this one stored and skips its own enumeration.
        let mut cached = lock_unpoisoned(&self.cached);
        if let Some(snapshot) = cached
            .as_ref()
            .filter(|snapshot| snapshot.generation == generation)
        {
            return Ok(Arc::clone(&snapshot.endpoints));
        }
        // The generation was read *before* enumerating. A device change that lands while the
        // enumeration runs therefore leaves the stored generation behind the monitor's, and the next
        // call enumerates again — one wasted pass is the right price for never serving a list that
        // is already out of date as if it were current.
        let endpoints = Arc::new(enumerate()?);
        *cached = Some(Snapshot {
            generation,
            endpoints: Arc::clone(&endpoints),
        });
        Ok(endpoints)
    }

    /// `None` when there is nothing left to invalidate the cache with, so the caller has to
    /// enumerate.
    fn generation(&self) -> Option<u64> {
        self.monitor
            .as_ref()
            .filter(|monitor| !monitor.worker_finished())
            .map(DeviceChangeMonitor::generation)
    }
}

fn enumerate() -> Result<Vec<EndpointDescriptor>, String> {
    WasapiPlatform::new()
        .active_endpoints()
        .map_err(|error| error.to_string())
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
