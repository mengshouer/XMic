use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use xmic_engine_protocol::AecPreset;
use xmic_platform_api::SystemLifecycleEvent;
use xmic_platform_windows::SystemLifecycleMonitor;

use crate::{emit_application_state, engine::DspFeature, engine_client::EngineController};

const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PENDING_START_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LifecyclePreferences {
    pub launch_at_login: bool,
    pub start_engine_on_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStatus {
    pub launch_at_login: bool,
    pub start_engine_on_launch: bool,
    pub session_available: bool,
    pub resume_pending: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct StoredPreferences {
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
    resume_pending: bool,
    last_error: Option<String>,
    exiting: bool,
}

impl LifecycleState {
    fn new(preferences: StoredPreferences) -> Self {
        Self {
            preferences,
            pause_reasons: 0,
            resume_pending: false,
            last_error: None,
            exiting: false,
        }
    }

    fn pause(&mut self, reason: PauseReason, engine_running: bool) -> bool {
        let was_available = self.pause_reasons == 0;
        self.pause_reasons |= reason.mask();
        if was_available && engine_running {
            self.resume_pending = true;
        }
        engine_running
    }

    fn resume(&mut self, reason: PauseReason) -> bool {
        self.pause_reasons &= !reason.mask();
        self.pause_reasons == 0 && self.resume_pending && !self.exiting
    }

    fn session_available(&self) -> bool {
        self.pause_reasons == 0 && !self.exiting
    }

    fn synchronize_engine_state(&mut self, start_requested: bool, running: bool) -> bool {
        if !self.session_available() {
            return false;
        }
        let mut changed = false;
        if running {
            if self.resume_pending {
                self.resume_pending = false;
                changed = true;
            }
            if self.last_error.take().is_some() {
                changed = true;
            }
        } else if start_requested && !self.resume_pending {
            self.resume_pending = true;
            changed = true;
        }
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
            start_engine_on_launch: state.preferences.start_engine_on_launch,
            session_available: state.session_available(),
            resume_pending: state.resume_pending,
            last_error: state.last_error.clone(),
        }
    }

    pub fn start_engine_on_launch(&self) -> bool {
        lock_unpoisoned(&self.state)
            .preferences
            .start_engine_on_launch
    }

    pub fn set_start_engine_on_launch(&self, enabled: bool) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        if state.preferences.start_engine_on_launch == enabled {
            return Ok(());
        }
        let preferences = StoredPreferences {
            start_engine_on_launch: enabled,
        };
        self.store.save(preferences)?;
        state.preferences = preferences;
        Ok(())
    }

    pub fn is_session_available(&self) -> bool {
        lock_unpoisoned(&self.state).session_available()
    }

    pub fn has_pending_start(&self) -> bool {
        lock_unpoisoned(&self.state).resume_pending
    }

    pub fn start_manually(&self, engine: &EngineController) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        {
            let mut state = lock_unpoisoned(&self.state);
            if state.exiting {
                return Err("XMic is exiting".to_owned());
            }
            if !state.session_available() {
                return Err(
                    "Windows session is unavailable; processing will remain stopped".to_owned(),
                );
            }
            state.resume_pending = false;
        }
        if engine.is_running() {
            self.clear_error();
            return Ok(());
        }
        match engine.start() {
            Ok(()) => {
                self.clear_error();
                Ok(())
            }
            Err(error) => {
                let start_requested = engine.start_requested();
                let mut state = lock_unpoisoned(&self.state);
                if start_requested && state.session_available() {
                    state.resume_pending = true;
                }
                state.last_error = Some(format!("启动处理失败：{error}"));
                Err(error)
            }
        }
    }

    pub fn stop_manually(&self, engine: &EngineController) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        {
            let mut state = lock_unpoisoned(&self.state);
            state.resume_pending = false;
        }
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

    pub(crate) fn toggle_dsp_feature(
        &self,
        engine: &EngineController,
        feature: DspFeature,
    ) -> Result<bool, String> {
        let _operation = lock_unpoisoned(&self.operation);
        self.reconfigure_engine(engine, |engine| engine.toggle_dsp_feature(feature))
    }

    pub(crate) fn set_aec_preset(
        &self,
        engine: &EngineController,
        preset: AecPreset,
    ) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        if engine.settings().dsp.aec.preset == preset {
            return Ok(());
        }
        self.reconfigure_engine(engine, |engine| engine.set_aec_preset(preset))
    }

    fn reconfigure_engine<T>(
        &self,
        engine: &EngineController,
        update: impl FnOnce(&EngineController) -> Result<T, String>,
    ) -> Result<T, String> {
        if lock_unpoisoned(&self.state).exiting {
            return Err("XMic is exiting".to_owned());
        }

        let was_running = engine.is_running();
        if was_running {
            if let Err(error) = engine.stop() {
                let message = format!("更新 DSP 设置前停止处理失败：{error}");
                self.set_error(message.clone());
                return Err(message);
            }
        }

        let result = match update(engine) {
            Ok(result) => result,
            Err(error) => {
                let message = if was_running {
                    match engine.start() {
                        Ok(()) => format!("更新 DSP 设置失败：{error}"),
                        Err(restart_error) => {
                            lock_unpoisoned(&self.state).resume_pending = true;
                            format!(
                                "更新 DSP 设置失败：{error}；恢复原处理配置也失败：{restart_error}"
                            )
                        }
                    }
                } else {
                    format!("更新 DSP 设置失败：{error}")
                };
                self.set_error(message.clone());
                return Err(message);
            }
        };

        if was_running {
            if let Err(error) = engine.start() {
                lock_unpoisoned(&self.state).resume_pending = true;
                let message = format!("DSP 设置已保存，但恢复处理失败：{error}");
                self.set_error(message.clone());
                return Err(message);
            }
        }

        if self.is_session_available() {
            self.clear_error();
        }
        Ok(result)
    }

    pub fn start_automatically(&self, engine: &EngineController) -> Result<(), String> {
        let _operation = lock_unpoisoned(&self.operation);
        {
            let mut state = lock_unpoisoned(&self.state);
            if state.exiting {
                return Ok(());
            }
            state.resume_pending = true;
        }
        self.try_pending_start(engine, "自动启动处理失败")
    }

    pub fn retry_pending_start(&self, engine: &EngineController) -> bool {
        let _operation = lock_unpoisoned(&self.operation);
        let should_retry = {
            let state = lock_unpoisoned(&self.state);
            state.session_available() && state.resume_pending
        };
        if !should_retry {
            return false;
        }
        let _ = self.try_pending_start(engine, "恢复处理失败");
        true
    }

    pub fn synchronize_engine_state(&self, start_requested: bool, running: bool) -> bool {
        lock_unpoisoned(&self.state).synchronize_engine_state(start_requested, running)
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
        {
            let mut state = lock_unpoisoned(&self.state);
            state.exiting = true;
            state.resume_pending = false;
        }
        let _ = engine.stop();
    }

    fn pause(&self, reason: PauseReason, engine: &EngineController) {
        let running = engine.is_running();
        let should_stop = lock_unpoisoned(&self.state).pause(reason, running);
        if should_stop {
            if let Err(error) = engine.stop() {
                lock_unpoisoned(&self.state).resume_pending = false;
                self.set_error(format!("暂停处理失败：{error}"));
            }
        }
    }

    fn resume(&self, reason: PauseReason, engine: &EngineController) {
        let should_resume = lock_unpoisoned(&self.state).resume(reason);
        if should_resume {
            let _ = self.try_pending_start(engine, "恢复处理失败");
        }
    }

    fn try_pending_start(
        &self,
        engine: &EngineController,
        error_context: &str,
    ) -> Result<(), String> {
        let should_start = {
            let state = lock_unpoisoned(&self.state);
            state.session_available() && state.resume_pending
        };
        if !should_start {
            return Ok(());
        }
        if engine.is_running() {
            let mut state = lock_unpoisoned(&self.state);
            state.resume_pending = false;
            state.last_error = None;
            return Ok(());
        }
        match engine.start() {
            Ok(()) => {
                let mut state = lock_unpoisoned(&self.state);
                state.resume_pending = false;
                state.last_error = None;
                Ok(())
            }
            Err(error) => {
                self.set_error(format!("{error_context}：{error}"));
                Err(error)
            }
        }
    }

    fn clear_error(&self) {
        lock_unpoisoned(&self.state).last_error = None;
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
    let mut next_retry = Instant::now() + PENDING_START_RETRY_INTERVAL;
    let mut last_running = None;
    while !stop.load(Ordering::Acquire) {
        match monitor.recv_timeout(LIFECYCLE_POLL_INTERVAL) {
            Ok(Some(event)) => {
                let lifecycle = app.state::<LifecycleController>();
                let engine = app.state::<EngineController>();
                lifecycle.handle_system_event(event, &engine);
                emit_application_state(&app);
                next_retry = Instant::now() + PENDING_START_RETRY_INTERVAL;
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

        if Instant::now() >= next_retry {
            let lifecycle = app.state::<LifecycleController>();
            let engine = app.state::<EngineController>();
            if lifecycle.retry_pending_start(&engine) {
                emit_application_state(&app);
            }
            next_retry = Instant::now() + PENDING_START_RETRY_INTERVAL;
        }

        let (running, start_requested) = {
            let engine = app.state::<EngineController>();
            (engine.is_running(), engine.start_requested())
        };
        let lifecycle_changed = app
            .state::<LifecycleController>()
            .synchronize_engine_state(start_requested, running);
        let running_changed = last_running
            .replace(running)
            .is_some_and(|last| last != running);
        if running_changed || lifecycle_changed {
            emit_application_state(&app);
        }
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
    fn overlapping_pause_reasons_resume_only_after_all_clear() {
        let mut state = LifecycleState::new(StoredPreferences::default());

        assert!(state.pause(PauseReason::SessionLocked, true));
        assert!(!state.pause(PauseReason::SystemSuspended, false));
        assert!(!state.resume(PauseReason::SessionLocked));
        assert!(state.resume(PauseReason::SystemSuspended));
    }

    #[test]
    fn manual_cancellation_prevents_resume() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.pause(PauseReason::SessionLocked, true);
        state.resume_pending = false;

        assert!(!state.resume(PauseReason::SessionLocked));
    }

    #[test]
    fn requested_restart_is_visible_until_the_engine_recovers() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.last_error = Some("start failed".to_owned());

        assert!(state.synchronize_engine_state(true, false));
        assert!(state.resume_pending);
        assert!(state.last_error.is_some());

        assert!(state.synchronize_engine_state(true, true));
        assert!(!state.resume_pending);
        assert!(state.last_error.is_none());
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
            start_engine_on_launch: true,
        };

        store.save(preferences).unwrap();

        assert_eq!(store.load(), preferences);
        let _ = fs::remove_file(path);
    }
}
