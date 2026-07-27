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
use xmic_engine_protocol::{AecPreset, ConfigurationIssue, EngineSettings};
use xmic_platform_api::SystemLifecycleEvent;
use xmic_platform_windows::SystemLifecycleMonitor;

use crate::{
    emit_application_state,
    engine::{DspFeature, EngineStartError},
    engine_client::EngineController,
};

const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const PENDING_START_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
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
    pub resume_pending: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
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
    resume_pending: bool,
    last_error: Option<String>,
    configuration_issue: Option<ConfigurationIssue>,
    configuration_notice_pending: bool,
    exiting: bool,
}

impl LifecycleState {
    fn new(preferences: StoredPreferences) -> Self {
        Self {
            preferences,
            pause_reasons: 0,
            resume_pending: false,
            last_error: None,
            configuration_issue: None,
            configuration_notice_pending: false,
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

    fn synchronize_engine_state(
        &mut self,
        start_requested: bool,
        running: bool,
        configuration_issue: Option<ConfigurationIssue>,
    ) -> bool {
        if let Some(issue) = configuration_issue {
            return self.block_for_configuration(issue, true);
        }
        let mut changed = false;
        if self.configuration_issue.take().is_some() {
            self.configuration_notice_pending = false;
            changed = true;
        }
        if !self.session_available() {
            return changed;
        }
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

    fn block_for_configuration(&mut self, issue: ConfigurationIssue, notify: bool) -> bool {
        let changed = self.configuration_issue.as_ref() != Some(&issue)
            || self.resume_pending
            || self.last_error.is_some();
        let new_issue = self.configuration_issue.as_ref() != Some(&issue);
        self.configuration_issue = Some(issue);
        self.resume_pending = false;
        self.last_error = None;
        if notify && new_issue {
            self.configuration_notice_pending = true;
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
            start_in_tray: state.preferences.start_in_tray,
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

    pub fn has_pending_start(&self) -> bool {
        lock_unpoisoned(&self.state).resume_pending
    }

    pub fn configuration_issue(&self) -> Option<ConfigurationIssue> {
        lock_unpoisoned(&self.state).configuration_issue.clone()
    }

    pub fn synchronize_configuration_issue(
        &self,
        issue: Option<ConfigurationIssue>,
        notify: bool,
    ) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        match issue {
            Some(issue) => state.block_for_configuration(issue, notify),
            None => {
                let changed = state.configuration_issue.take().is_some();
                state.configuration_notice_pending = false;
                changed
            }
        }
    }

    pub fn take_configuration_notice(&self) -> Option<ConfigurationIssue> {
        let mut state = lock_unpoisoned(&self.state);
        if !state.configuration_notice_pending {
            return None;
        }
        state.configuration_notice_pending = false;
        state.configuration_issue.clone()
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
                self.clear_start_state();
                Ok(())
            }
            Err(EngineStartError::ConfigurationRequired(issue)) => {
                lock_unpoisoned(&self.state).block_for_configuration(issue, true);
                Err("audio device configuration is required".to_owned())
            }
            Err(EngineStartError::Runtime(error)) => {
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

    pub(crate) fn set_dsp_feature_enabled(
        &self,
        engine: &EngineController,
        feature: DspFeature,
        enabled: bool,
    ) -> Result<bool, String> {
        let _operation = lock_unpoisoned(&self.operation);
        let Some(settings) = updated_dsp_feature_settings(engine.settings(), feature, enabled)?
        else {
            return Ok(false);
        };
        self.reconfigure_engine(engine, move |engine| {
            engine.update_settings(settings)?;
            Ok(true)
        })
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
                            record_restart_failure(
                                &mut lock_unpoisoned(&self.state),
                                &restart_error,
                            );
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
                record_restart_failure(&mut lock_unpoisoned(&self.state), &error);
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

    pub fn synchronize_engine_state(
        &self,
        start_requested: bool,
        running: bool,
        configuration_issue: Option<ConfigurationIssue>,
    ) -> bool {
        lock_unpoisoned(&self.state).synchronize_engine_state(
            start_requested,
            running,
            configuration_issue,
        )
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
                state.configuration_issue = None;
                state.configuration_notice_pending = false;
                Ok(())
            }
            Err(EngineStartError::ConfigurationRequired(issue)) => {
                lock_unpoisoned(&self.state).block_for_configuration(issue, true);
                Err("audio device configuration is required".to_owned())
            }
            Err(EngineStartError::Runtime(error)) => {
                self.set_error(format!("{error_context}：{error}"));
                Err(error)
            }
        }
    }

    fn clear_error(&self) {
        lock_unpoisoned(&self.state).last_error = None;
    }

    fn clear_start_state(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.last_error = None;
        state.configuration_issue = None;
        state.configuration_notice_pending = false;
    }

    fn set_error(&self, error: String) {
        lock_unpoisoned(&self.state).last_error = Some(error);
    }
}

fn updated_dsp_feature_settings(
    mut settings: EngineSettings,
    feature: DspFeature,
    enabled: bool,
) -> Result<Option<EngineSettings>, String> {
    if feature.enabled(&settings) == enabled {
        return Ok(None);
    }
    feature.set_enabled(&mut settings, enabled);
    settings.validate().map_err(|error| error.to_string())?;
    Ok(Some(settings))
}

fn record_restart_failure(state: &mut LifecycleState, error: &EngineStartError) {
    match error {
        EngineStartError::ConfigurationRequired(issue) => {
            state.block_for_configuration(issue.clone(), true);
        }
        EngineStartError::Runtime(_) => state.resume_pending = true,
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

        let (running, start_requested, configuration_issue) = {
            let engine = app.state::<EngineController>();
            let running = engine.is_running();
            (
                running,
                engine.start_requested(),
                engine.configuration_issue(),
            )
        };
        let lifecycle_changed = app.state::<LifecycleController>().synchronize_engine_state(
            start_requested,
            running,
            configuration_issue,
        );
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
    fn unchanged_dsp_feature_does_not_require_reconfiguration() {
        let settings = EngineSettings::default();

        assert!(
            updated_dsp_feature_settings(settings, DspFeature::Aec, true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_dsp_dependency_is_rejected_before_reconfiguration() {
        let settings = EngineSettings::default();

        let error = updated_dsp_feature_settings(settings, DspFeature::Rnnoise, false).unwrap_err();

        assert_eq!(error, "VAD gate requires RNNoise");
    }

    #[test]
    fn valid_dsp_feature_update_changes_only_the_requested_state() {
        let settings = EngineSettings::default();

        let updated = updated_dsp_feature_settings(settings, DspFeature::Agc2, true)
            .unwrap()
            .unwrap();

        assert!(updated.dsp.aec.enabled);
        assert!(updated.dsp.rnnoise.enabled);
        assert!(updated.dsp.agc2.enabled);
        assert!(updated.dsp.vad_gate.enabled);
    }

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

        assert!(state.synchronize_engine_state(true, false, None));
        assert!(state.resume_pending);
        assert!(state.last_error.is_some());

        assert!(state.synchronize_engine_state(true, true, None));
        assert!(!state.resume_pending);
        assert!(state.last_error.is_none());
    }

    #[test]
    fn repeated_configuration_blocker_only_requests_one_notice() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.resume_pending = true;
        state.last_error = Some("route unavailable".to_owned());
        let issue = physical_capture_required();

        assert!(state.block_for_configuration(issue.clone(), true));
        assert!(!state.resume_pending);
        assert!(state.last_error.is_none());
        assert!(state.configuration_notice_pending);

        state.configuration_notice_pending = false;
        assert!(!state.block_for_configuration(issue, true));
        assert!(!state.configuration_notice_pending);
    }

    #[test]
    fn clearing_configuration_blocker_restores_normal_start_tracking() {
        let mut state = LifecycleState::new(StoredPreferences::default());
        state.block_for_configuration(physical_capture_required(), true);

        assert!(state.synchronize_engine_state(false, false, None));
        assert!(state.configuration_issue.is_none());
        assert!(!state.configuration_notice_pending);

        assert!(state.synchronize_engine_state(true, false, None));
        assert!(state.resume_pending);
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
    fn legacy_preferences_show_the_main_window_by_default() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "xmic-lifecycle-legacy-{}-{}.json",
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
