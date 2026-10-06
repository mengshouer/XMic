use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use xmic_engine_protocol::ConfigurationIssue;
use xmic_platform_api::SystemLifecycleEvent;
use xmic_platform_windows::SystemLifecycleMonitor;

use crate::{emit_application_state, engine::EngineStartError, engine_client::EngineController};

const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct LifecyclePreferences {
    pub launch_at_login: bool,
    pub start_in_tray: bool,
    pub start_engine_on_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStatus {
    pub launch_at_login: bool,
    pub start_in_tray: bool,
    pub start_engine_on_launch: bool,
    pub session_available: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct StoredPreferences {
    start_in_tray: bool,
    start_engine_on_launch: bool,
}

pub struct LifecyclePreferencesStore {
    path: PathBuf,
}

impl LifecyclePreferencesStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> StoredPreferences {
        load_preferences(&self.path).unwrap_or_default()
    }

    fn save(&self, preferences: StoredPreferences) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "lifecycle preferences path has no parent directory".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create lifecycle preferences directory {}: {error}",
                parent.display()
            )
        })?;
        let contents = serde_json::to_string_pretty(&preferences)
            .map_err(|error| format!("serialize lifecycle preferences: {error}"))?;
        fs::write(&self.path, contents).map_err(|error| {
            format!(
                "write lifecycle preferences {}: {error}",
                self.path.display()
            )
        })
    }
}

fn load_preferences(path: &Path) -> Option<StoredPreferences> {
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

pub struct LifecycleController {
    operation: Mutex<()>,
    state: Mutex<LifecycleState>,
    store: LifecyclePreferencesStore,
}

#[derive(Debug)]
struct LifecycleState {
    preferences: StoredPreferences,
    pause_reasons: u8,
    last_error: Option<String>,
    configuration_issue: Option<ConfigurationIssue>,
    exiting: bool,
}

impl LifecycleState {
    fn new(preferences: StoredPreferences) -> Self {
        Self {
            preferences,
            pause_reasons: 0,
            last_error: None,
            configuration_issue: None,
            exiting: false,
        }
    }

    fn pause(&mut self, reason: PauseReason) -> bool {
        let was_available = self.session_available();
        self.pause_reasons |= reason.mask();
        was_available && !self.session_available()
    }

    fn resume(&mut self, reason: PauseReason) {
        self.pause_reasons &= !reason.mask();
    }

    fn session_available(&self) -> bool {
        self.pause_reasons == 0 && !self.exiting
    }

    fn synchronize_engine_state(
        &mut self,
        running: bool,
        configuration_issue: Option<ConfigurationIssue>,
    ) -> bool {
        if let Some(issue) = configuration_issue {
            return self.block_for_configuration(issue);
        }
        let mut changed = self.configuration_issue.take().is_some();
        if running && self.last_error.take().is_some() {
            changed = true;
        }
        changed
    }

    /// Records an audio route XMic cannot use right now while EngineController retains the user's
    /// desired intent and owns all recovery scheduling.
    fn block_for_configuration(&mut self, issue: ConfigurationIssue) -> bool {
        let changed =
            self.configuration_issue.as_ref() != Some(&issue) || self.last_error.is_some();
        self.configuration_issue = Some(issue);
        self.last_error = None;
        changed
    }
}

#[derive(Debug, Clone, Copy)]
enum PauseReason {
    SessionLocked,
    SessionDisconnected,
    SystemSuspended,
    SessionLoggedOff,
    MonitorFailed,
}

impl PauseReason {
    const fn mask(self) -> u8 {
        match self {
            Self::SessionLocked => 1 << 0,
            Self::SessionDisconnected => 1 << 1,
            Self::SystemSuspended => 1 << 2,
            Self::SessionLoggedOff => 1 << 3,
            Self::MonitorFailed => 1 << 4,
        }
    }
}

impl LifecycleController {
    pub fn new(store: LifecyclePreferencesStore) -> Self {
        let preferences = store.load();
        Self {
            operation: Mutex::new(()),
            state: Mutex::new(LifecycleState::new(preferences)),
            store,
        }
    }

    pub fn status(&self, launch_at_login: bool) -> LifecycleStatus {
        let state = lock_unpoisoned(&self.state);
        LifecycleStatus {
            launch_at_login,
            start_in_tray: state.preferences.start_in_tray,
            start_engine_on_launch: state.preferences.start_engine_on_launch,
            session_available: state.session_available(),
            last_error: state.last_error.clone(),
        }
    }

    pub fn start_engine_on_launch(&self) -> bool {
        lock_unpoisoned(&self.state)
            .preferences
            .start_engine_on_launch
    }

    pub fn start_in_tray(&self) -> bool {
        lock_unpoisoned(&self.state).preferences.start_in_tray
    }

    pub fn set_startup_preferences(
        &self,
        start_in_tray: bool,
        start_engine_on_launch: bool,
    ) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        let preferences = StoredPreferences {
            start_in_tray,
            start_engine_on_launch,
        };
        if state.preferences == preferences {
            return Ok(());
        }
        self.store.save(preferences)?;
        state.preferences = preferences;
        Ok(())
    }

    pub fn is_session_available(&self) -> bool {
        lock_unpoisoned(&self.state).session_available()
    }

    /// Publishes Windows session availability to the sole EngineController scheduler. The temporary
    /// Off-only notification probe additionally requires a route without a known safety conflict.
    pub fn refresh_engine_availability(&self, engine: &EngineController) {
        let (session_available, probe_allowed) = {
            let state = lock_unpoisoned(&self.state);
            (
                state.session_available(),
                state.session_available() && state.configuration_issue.is_none(),
            )
        };
        engine.set_session_available(session_available);
        engine.set_notification_probe_allowed(probe_allowed);
    }

    pub fn configuration_issue(&self) -> Option<ConfigurationIssue> {
        lock_unpoisoned(&self.state).configuration_issue.clone()
    }

    pub fn synchronize_configuration_issue(&self, issue: Option<ConfigurationIssue>) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        match issue {
            Some(issue) => state.block_for_configuration(issue),
            None => state.configuration_issue.take().is_some(),
        }
    }

    pub fn clear_last_error(&self) {
        self.clear_error();
    }

    pub fn start_manually(&self, engine: &EngineController) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        {
            let state = lock_unpoisoned(&self.state);
            if state.exiting {
                return Err("XMic is exiting".to_owned());
            }
            if !state.session_available() {
                return Err(
                    "Windows session is unavailable; processing will remain stopped".to_owned(),
                );
            }
        }
        match engine.start() {
            Ok(()) => {
                self.clear_start_state();
                Ok(())
            }
            Err(EngineStartError::ConfigurationRequired(issue)) => {
                lock_unpoisoned(&self.state).block_for_configuration(issue);
                Err("audio device configuration is required".to_owned())
            }
            Err(EngineStartError::Runtime(error)) => {
                lock_unpoisoned(&self.state).last_error = Some(format!("启动处理失败：{error}"));
                Err(error)
            }
        }
    }

    pub fn stop_manually(&self, engine: &EngineController) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        match engine.stop() {
            Ok(()) => {
                if self.is_session_available() {
                    self.clear_error();
                }
                Ok(())
            }
            Err(error) => {
                self.set_error(format!("停止处理失败：{error}"));
                Err(error)
            }
        }
    }

    pub fn synchronize_engine_state(
        &self,
        running: bool,
        configuration_issue: Option<ConfigurationIssue>,
    ) -> bool {
        lock_unpoisoned(&self.state).synchronize_engine_state(running, configuration_issue)
    }

    pub fn handle_system_event(&self, event: SystemLifecycleEvent, engine: &EngineController) {
        let _operation = lock_unpoisoned(&self.operation);
        match event {
            SystemLifecycleEvent::SessionLocked => self.pause(PauseReason::SessionLocked, engine),
            SystemLifecycleEvent::SessionDisconnected => {
                self.pause(PauseReason::SessionDisconnected, engine)
            }
            SystemLifecycleEvent::SessionLoggedOff => {
                self.pause(PauseReason::SessionLoggedOff, engine)
            }
            SystemLifecycleEvent::Suspending => self.pause(PauseReason::SystemSuspended, engine),
            SystemLifecycleEvent::SessionUnlocked => {
                self.resume(PauseReason::SessionLocked, engine)
            }
            SystemLifecycleEvent::SessionConnected => {
                self.resume(PauseReason::SessionDisconnected, engine)
            }
            SystemLifecycleEvent::Resumed => self.resume(PauseReason::SystemSuspended, engine),
        }
    }

    pub fn monitor_failed(&self, engine: &EngineController, error: String) {
        let _operation = lock_unpoisoned(&self.operation);
        self.pause(PauseReason::MonitorFailed, engine);
        self.set_error(format!("Windows lifecycle monitor 已停止：{error}"));
    }

    pub fn shutdown(&self, engine: &EngineController) {
        let _operation = lock_unpoisoned(&self.operation);
        lock_unpoisoned(&self.state).exiting = true;
        self.refresh_engine_availability(engine);
        let _ = engine.stop();
    }

    fn pause(&self, reason: PauseReason, engine: &EngineController) {
        let should_stop = lock_unpoisoned(&self.state).pause(reason);
        // Revoke availability before stopping, so the Off-only probe cannot take over the microphone
        // the Windows lifecycle event is meant to release.
        self.refresh_engine_availability(engine);
        if should_stop {
            if let Err(error) = engine.pause() {
                self.set_error(format!("暂停处理失败：{error}"));
            }
        }
    }

    fn resume(&self, reason: PauseReason, engine: &EngineController) {
        lock_unpoisoned(&self.state).resume(reason);
        // Availability is enough. EngineController owns retry timing and observes the preserved
        // desiredEnabled intent on the next lifecycle poll.
        self.refresh_engine_availability(engine);
    }

    fn clear_error(&self) {
        lock_unpoisoned(&self.state).last_error = None;
    }

    fn clear_start_state(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.last_error = None;
        state.configuration_issue = None;
    }

    fn set_error(&self, error: String) {
        lock_unpoisoned(&self.state).last_error = Some(error);
    }
}

pub struct BackgroundLifecycle {
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl BackgroundLifecycle {
    pub fn start(app: AppHandle, monitor: SystemLifecycleMonitor) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("xmic-lifecycle-coordinator".to_owned())
            .spawn(move || run_background_lifecycle(app, monitor, thread_stop.as_ref()))
            .map_err(|error| format!("start background lifecycle coordinator: {error}"))?;

        Ok(Self {
            stop,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = lock_unpoisoned(&self.worker).take() {
            let _ = worker.join();
        }
    }
}

impl Drop for BackgroundLifecycle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_background_lifecycle(app: AppHandle, monitor: SystemLifecycleMonitor, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        match monitor.recv_timeout(LIFECYCLE_POLL_INTERVAL) {
            Ok(Some(event)) => {
                let lifecycle = app.state::<LifecycleController>();
                let engine = app.state::<EngineController>();
                lifecycle.handle_system_event(event, &engine);
            }
            Ok(None) => {}
            Err(error) => {
                if !stop.load(Ordering::Acquire) {
                    let lifecycle = app.state::<LifecycleController>();
                    let engine = app.state::<EngineController>();
                    lifecycle.monitor_failed(&engine, error.to_string());
                    emit_application_state(&app);
                }
                break;
            }
        }

        // This is the only endpoint-aware status poll in the coordinator. In particular, do not use
        // a second helper status call here: it could renew an Off-time probe before the endpoint
        // snapshot has been checked for a safe physical capture.
        let status = emit_application_state(&app)
            .unwrap_or_else(|| app.state::<EngineController>().cached_status());
        app.state::<LifecycleController>().synchronize_engine_state(
            status.session_active(),
            status.configuration_issue().cloned(),
        );
        {
            let engine = app.state::<EngineController>();
            app.state::<LifecycleController>()
                .refresh_engine_availability(&engine);
        }
        // The engine can hear notification-probe speech without anything else changing, so this poll
        // is the one place guaranteed to notice it.
        crate::send_notification_probe_notice(&app);
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn overlapping_pause_reasons_restore_availability_only_after_all_clear() {
        let mut state = LifecycleState::new(StoredPreferences::default());

        assert!(state.pause(PauseReason::SessionLocked));
        assert!(!state.session_available());
        assert!(!state.pause(PauseReason::SystemSuspended));
        state.resume(PauseReason::SessionLocked);
        assert!(!state.session_available());
        state.resume(PauseReason::SystemSuspended);
        assert!(state.session_available());
    }

    #[test]
    fn engine_recovery_does_not_create_lifecycle_pending_state() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.last_error = Some("start failed".to_owned());

        assert!(!state.synchronize_engine_state(false, None));
        assert!(state.last_error.is_some());
        assert!(state.synchronize_engine_state(true, None));
        assert!(state.last_error.is_none());
    }

    #[test]
    fn repeated_configuration_blocker_is_idempotent() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.last_error = Some("route unavailable".to_owned());
        let issue = physical_capture_required();

        assert!(state.block_for_configuration(issue.clone()));
        assert!(state.last_error.is_none());
        assert!(!state.block_for_configuration(issue));
    }

    #[test]
    fn transient_configuration_issue_does_not_change_session_availability() {
        let mut state = LifecycleState::new(StoredPreferences::default());

        assert!(state.synchronize_engine_state(false, Some(physical_capture_required())));
        assert!(state.configuration_issue.is_some());
        assert!(state.session_available());

        assert!(state.synchronize_engine_state(false, None));
        assert!(state.configuration_issue.is_none());
        assert!(state.session_available());
    }

    #[test]
    fn preferences_round_trip() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "xmic-lifecycle-{}-{}.json",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let store = LifecyclePreferencesStore::new(path.clone());
        let preferences = StoredPreferences {
            start_in_tray: false,
            start_engine_on_launch: true,
        };

        store.save(preferences).unwrap();

        assert_eq!(store.load(), preferences);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn removed_lifecycle_state_fields_are_not_loaded() {
        let value: Result<StoredPreferences, _> =
            serde_json::from_str(r#"{"resumePending":true,"startEngineOnLaunch":true}"#);
        assert!(value.is_err());
    }

    #[test]
    fn invalid_preferences_show_the_main_window_by_default() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "xmic-lifecycle-invalid-{}-{}.json",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, r#"{"startEngineOnLaunch":true}"#).unwrap();

        let preferences = LifecyclePreferencesStore::new(path.clone()).load();

        assert!(!preferences.start_in_tray);
        assert!(preferences.start_engine_on_launch);
        let _ = fs::remove_file(path);
    }

    fn physical_capture_required() -> ConfigurationIssue {
        ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
        }
    }
}
