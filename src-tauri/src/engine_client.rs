use std::{
    collections::VecDeque,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use std::os::windows::process::CommandExt;
use xmic_engine_protocol::{
    AecPreset, EngineMode, EngineRequest, EngineResponse, EngineSettings, EngineStatus,
    PROTOCOL_VERSION,
};
use xmic_platform_api::EndpointDescriptor;

use crate::{engine::DspFeature, engine_ipc::send_request};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HELPER_START_TIMEOUT: Duration = Duration::from_secs(8);
const HELPER_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const HELPER_RESTART_BACKOFF: Duration = Duration::from_secs(1);
const ENGINE_RESTART_BACKOFF: Duration = Duration::from_secs(5);
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASHES_BEFORE_SAFE_MODE: usize = 3;

pub struct EngineController {
    executable: PathBuf,
    native_library: PathBuf,
    config_dir: PathBuf,
    pipe_name: String,
    state: Mutex<SupervisorState>,
}

struct SupervisorState {
    child: Option<Child>,
    requested_running: bool,
    safe_mode: bool,
    shutting_down: bool,
    restart_not_before: Option<Instant>,
    engine_restart_not_before: Option<Instant>,
    crashes: CrashTracker,
    cached_status: EngineStatus,
    supervisor_warning: Option<String>,
}

#[derive(Default)]
struct CrashTracker {
    events: VecDeque<Instant>,
}

impl SupervisorState {
    fn request_engine_start(&mut self) {
        self.requested_running = true;
        self.engine_restart_not_before = None;
    }

    fn defer_engine_restart(&mut self, error: &str) {
        self.engine_restart_not_before = Some(Instant::now() + ENGINE_RESTART_BACKOFF);
        self.supervisor_warning = Some(format!(
            "audio engine restart is waiting for the audio route: {error}"
        ));
    }

    fn mark_engine_started(&mut self) {
        self.engine_restart_not_before = None;
        if !self.safe_mode {
            self.supervisor_warning = None;
        }
    }

    fn cancel_engine_start(&mut self) {
        self.requested_running = false;
        self.engine_restart_not_before = None;
        self.supervisor_warning = None;
    }
}

impl CrashTracker {
    fn record(&mut self, now: Instant) -> usize {
        while self
            .events
            .front()
            .is_some_and(|event| now.duration_since(*event) >= CRASH_WINDOW)
        {
            self.events.pop_front();
        }
        self.events.push_back(now);
        self.events.len()
    }
}

impl EngineController {
    pub fn new(
        executable: PathBuf,
        native_library: PathBuf,
        config_dir: PathBuf,
    ) -> Result<Self, String> {
        let controller = Self {
            executable,
            native_library,
            config_dir,
            pipe_name: format!("xmic-engine-{}", std::process::id()),
            state: Mutex::new(SupervisorState {
                child: None,
                requested_running: false,
                safe_mode: false,
                shutting_down: false,
                restart_not_before: None,
                engine_restart_not_before: None,
                crashes: CrashTracker::default(),
                cached_status: EngineStatus::bootstrap(Vec::new()),
                supervisor_warning: None,
            }),
        };
        controller.ensure_process()?;
        Ok(controller)
    }

    pub fn status(&self, endpoints: Vec<EndpointDescriptor>) -> EngineStatus {
        let mut state = lock_unpoisoned(&self.state);
        match self.get_status_locked(&mut state) {
            Ok(mut status) => {
                status.endpoints = endpoints;
                apply_supervisor_warning(&mut status, state.supervisor_warning.as_deref());
                state.cached_status = status.clone();
                status
            }
            Err(error) => {
                let mut status = state.cached_status.clone();
                status.endpoints = endpoints;
                status.running = false;
                status.mode = if state.safe_mode {
                    EngineMode::SafeMode
                } else {
                    EngineMode::Error
                };
                status.detail = "audio engine helper is unavailable".to_owned();
                status.warning = Some(error);
                apply_supervisor_warning(&mut status, state.supervisor_warning.as_deref());
                status
            }
        }
    }

    pub fn update_settings(&self, settings: EngineSettings) -> Result<(), String> {
        settings.validate().map_err(|error| error.to_string())?;
        let mut state = lock_unpoisoned(&self.state);
        expect_ack(self.request_locked(
            &mut state,
            &EngineRequest::UpdateSettings {
                settings: settings.clone(),
            },
        )?)?;
        state.cached_status.settings = settings;
        Ok(())
    }

    pub(crate) fn settings(&self) -> EngineSettings {
        let mut state = lock_unpoisoned(&self.state);
        if let Ok(status) = self.get_status_locked(&mut state) {
            state.cached_status = status;
        }
        state.cached_status.settings.clone()
    }

    pub(crate) fn toggle_dsp_feature(&self, feature: DspFeature) -> Result<bool, String> {
        let mut settings = self.settings();
        let enabled = feature.toggle(&mut settings);
        self.update_settings(settings)?;
        Ok(enabled)
    }

    pub(crate) fn set_aec_preset(&self, preset: AecPreset) -> Result<(), String> {
        let mut settings = self.settings();
        settings.dsp.aec.preset = preset;
        self.update_settings(settings)
    }

    pub fn is_running(&self) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        match self.get_status_locked(&mut state) {
            Ok(status) => {
                let running = status.running;
                state.cached_status = status;
                running
            }
            Err(_) => false,
        }
    }

    pub fn start_requested(&self) -> bool {
        lock_unpoisoned(&self.state).requested_running
    }

    pub fn start(&self) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        state.request_engine_start();
        self.ensure_process_locked(&mut state)?;
        match send_request(&self.pipe_name, &EngineRequest::Start, IPC_CONNECT_TIMEOUT) {
            Ok(EngineResponse::Ack) => {
                state.mark_engine_started();
                Ok(())
            }
            Ok(EngineResponse::Error(error)) => {
                state.defer_engine_restart(&error);
                Err(error)
            }
            Ok(response) => {
                state.cancel_engine_start();
                Err(format!("unexpected helper acknowledgement: {response:?}"))
            }
            Err(error) => {
                self.reap_child_locked(&mut state)?;
                Err(error)
            }
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        state.cancel_engine_start();
        expect_ack(self.request_locked(&mut state, &EngineRequest::Stop)?)?;
        Ok(())
    }

    pub fn shutdown(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.cancel_engine_start();
        state.shutting_down = true;
        if state.child.is_some() {
            let _ = send_request(
                &self.pipe_name,
                &EngineRequest::Shutdown,
                IPC_CONNECT_TIMEOUT,
            );
            let deadline = Instant::now() + HELPER_STOP_TIMEOUT;
            while Instant::now() < deadline {
                let exited = state
                    .child
                    .as_mut()
                    .and_then(|child| child.try_wait().ok())
                    .flatten()
                    .is_some();
                if exited {
                    state.child = None;
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            if let Some(mut child) = state.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        drop(state);
        let _ = std::fs::remove_file(&self.executable);
        if let Some(runtime_dir) = self.executable.parent() {
            let _ = std::fs::remove_dir(runtime_dir);
        }
    }

    fn ensure_process(&self) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        self.ensure_process_locked(&mut state)
    }

    fn ensure_process_locked(&self, state: &mut SupervisorState) -> Result<(), String> {
        if state.shutting_down {
            return Err("audio engine supervisor is shutting down".to_owned());
        }
        self.reap_child_locked(state)?;
        if state.child.is_some() {
            return Ok(());
        }
        if let Some(not_before) = state.restart_not_before.take() {
            if let Some(remaining) = not_before.checked_duration_since(Instant::now()) {
                thread::sleep(remaining);
            }
        }

        let mut command = Command::new(&self.executable);
        command
            .arg("--audio-engine")
            .arg("--pipe-name")
            .arg(&self.pipe_name)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .arg("--native-library")
            .arg(&self.native_library)
            .arg("--config-dir")
            .arg(&self.config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        if state.safe_mode {
            command.arg("--safe-mode");
        }
        state.child = Some(command.spawn().map_err(|error| {
            format!(
                "start audio engine helper {}: {error}",
                self.executable.display()
            )
        })?);

        let deadline = Instant::now() + HELPER_START_TIMEOUT;
        loop {
            match send_request(
                &self.pipe_name,
                &EngineRequest::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
                Duration::from_millis(200),
            ) {
                Ok(EngineResponse::Hello { protocol_version })
                    if protocol_version == PROTOCOL_VERSION =>
                {
                    break;
                }
                Ok(EngineResponse::Error(error)) => {
                    self.terminate_child_locked(state);
                    return Err(error);
                }
                Ok(response) => {
                    self.terminate_child_locked(state);
                    return Err(format!(
                        "unexpected helper handshake response: {response:?}"
                    ));
                }
                Err(error) => {
                    self.reap_child_locked(state)?;
                    if state.child.is_none() {
                        return Err(format!(
                            "audio engine helper exited during startup: {error}"
                        ));
                    }
                    if Instant::now() >= deadline {
                        self.terminate_child_locked(state);
                        return Err(format!("audio engine helper startup timed out: {error}"));
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }

        Ok(())
    }

    fn request_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<EngineResponse, String> {
        self.ensure_process_locked(state)?;
        match send_request(&self.pipe_name, request, IPC_CONNECT_TIMEOUT) {
            Ok(response) => match response {
                EngineResponse::Error(error) => Err(error),
                response => Ok(response),
            },
            Err(error) => {
                self.reap_child_locked(state)?;
                Err(error)
            }
        }
    }

    fn get_status_locked(&self, state: &mut SupervisorState) -> Result<EngineStatus, String> {
        let mut status = match self.request_locked(
            state,
            &EngineRequest::GetStatus {
                include_endpoints: false,
            },
        )? {
            EngineResponse::Status(status) => *status,
            response => return Err(format!("unexpected helper status response: {response:?}")),
        };
        if status.running {
            state.mark_engine_started();
        } else if !state.requested_running && !state.safe_mode {
            state.supervisor_warning = None;
        }
        let retry_due = state
            .engine_restart_not_before
            .is_none_or(|not_before| Instant::now() >= not_before);
        if state.requested_running && !status.running && retry_due {
            match send_request(&self.pipe_name, &EngineRequest::Start, IPC_CONNECT_TIMEOUT) {
                Ok(EngineResponse::Ack) => {
                    state.mark_engine_started();
                    status = match self.request_locked(
                        state,
                        &EngineRequest::GetStatus {
                            include_endpoints: false,
                        },
                    )? {
                        EngineResponse::Status(status) => *status,
                        response => {
                            return Err(format!(
                                "unexpected helper status response after restart: {response:?}"
                            ));
                        }
                    };
                }
                Ok(EngineResponse::Error(error)) => {
                    state.defer_engine_restart(&error);
                }
                Ok(response) => {
                    return Err(format!("unexpected helper restart response: {response:?}"));
                }
                Err(error) => {
                    self.reap_child_locked(state)?;
                    return Err(error);
                }
            }
        }
        Ok(status)
    }

    fn reap_child_locked(&self, state: &mut SupervisorState) -> Result<(), String> {
        let Some(child) = state.child.as_mut() else {
            return Ok(());
        };
        let Some(exit_status) = child
            .try_wait()
            .map_err(|error| format!("query audio engine helper: {error}"))?
        else {
            return Ok(());
        };
        state.child = None;
        if state.shutting_down {
            return Ok(());
        }

        let now = Instant::now();
        let crash_count = state.crashes.record(now);
        if should_use_safe_mode(state.safe_mode, state.requested_running, crash_count) {
            state.safe_mode = true;
            state.cached_status.mode = EngineMode::SafeMode;
            state.supervisor_warning = Some(format!(
                "audio engine crashed {crash_count} times within 60 seconds; restarting in muted safe mode"
            ));
        } else {
            state.cached_status.mode = EngineMode::Starting;
            state.supervisor_warning = Some(format!(
                "audio engine exited with {exit_status}; restarting automatically"
            ));
        }
        state.cached_status.running = false;
        state.restart_not_before = Some(now + HELPER_RESTART_BACKOFF);
        Ok(())
    }

    fn terminate_child_locked(&self, state: &mut SupervisorState) {
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for EngineController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn expect_ack(response: EngineResponse) -> Result<(), String> {
    match response {
        EngineResponse::Ack => Ok(()),
        EngineResponse::Error(error) => Err(error),
        response => Err(format!("unexpected helper acknowledgement: {response:?}")),
    }
}

fn should_use_safe_mode(already_safe: bool, start_requested: bool, crash_count: usize) -> bool {
    already_safe || (start_requested && crash_count >= CRASHES_BEFORE_SAFE_MODE)
}

fn apply_supervisor_warning(status: &mut EngineStatus, warning: Option<&str>) {
    let Some(warning) = warning else {
        return;
    };
    status.warning = Some(match status.warning.take() {
        Some(engine_warning) => format!("{warning}; {engine_warning}"),
        None => warning.to_owned(),
    });
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_tracker_expires_events_outside_the_window() {
        let start = Instant::now();
        let mut tracker = CrashTracker::default();
        assert_eq!(tracker.record(start), 1);
        assert_eq!(tracker.record(start + Duration::from_secs(1)), 2);
        assert_eq!(
            tracker.record(start + CRASH_WINDOW + Duration::from_secs(1)),
            1
        );
    }

    #[test]
    fn supervisor_warning_preserves_engine_warning() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.warning = Some("engine warning".to_owned());

        apply_supervisor_warning(&mut status, Some("supervisor warning"));

        assert_eq!(
            status.warning.as_deref(),
            Some("supervisor warning; engine warning")
        );
    }

    #[test]
    fn recoverable_start_error_keeps_the_request_until_manual_cancellation() {
        let mut state = supervisor_state();
        state.request_engine_start();
        state.defer_engine_restart("route unavailable");

        assert!(state.requested_running);
        assert!(state.engine_restart_not_before.is_some());
        assert!(state
            .supervisor_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("route unavailable")));

        state.cancel_engine_start();

        assert!(!state.requested_running);
        assert!(state.engine_restart_not_before.is_none());
        assert!(state.supervisor_warning.is_none());
    }

    #[test]
    fn successful_restart_clears_transient_warning_but_preserves_safe_mode() {
        let mut state = supervisor_state();
        state.request_engine_start();
        state.defer_engine_restart("route unavailable");
        state.mark_engine_started();
        assert!(state.supervisor_warning.is_none());

        state.safe_mode = true;
        state.supervisor_warning = Some("safe mode warning".to_owned());
        state.mark_engine_started();
        assert_eq!(
            state.supervisor_warning.as_deref(),
            Some("safe mode warning")
        );
    }

    #[test]
    fn safe_mode_remains_active_after_its_crash_window_expires() {
        assert!(should_use_safe_mode(true, true, 1));
        assert!(should_use_safe_mode(false, true, CRASHES_BEFORE_SAFE_MODE));
        assert!(!should_use_safe_mode(
            false,
            false,
            CRASHES_BEFORE_SAFE_MODE
        ));
    }

    fn supervisor_state() -> SupervisorState {
        SupervisorState {
            child: None,
            requested_running: false,
            safe_mode: false,
            shutting_down: false,
            restart_not_before: None,
            engine_restart_not_before: None,
            crashes: CrashTracker::default(),
            cached_status: EngineStatus::bootstrap(Vec::new()),
            supervisor_warning: None,
        }
    }
}
