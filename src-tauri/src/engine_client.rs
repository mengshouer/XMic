use std::{
    collections::VecDeque,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant},
};

use std::os::windows::process::CommandExt;
use xmic_engine_protocol::{
    ConfigurationIssue, DspLibraryValidation, EngineOutput, EngineRequest, EngineResponse,
    EngineSettings, EngineState, EngineStatus, HelperStatus, NotificationProbePermit,
    PrimaryReason, PrimaryReasonCode, PROTOCOL_VERSION,
};
use xmic_platform_api::{AudioPlatform, EndpointDescriptor};
use xmic_platform_windows::WasapiPlatform;

use crate::{
    engine::EngineStartError,
    engine_ipc::send_request,
    route_validation::{
        apply_default_virtual_device, fallback_route_ready, notification_probe_route_safe,
        processing_route_ready, route_fingerprint, route_wait_reason, RouteFingerprint,
    },
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IPC_HELLO_TIMEOUT: Duration = Duration::from_secs(3);
const IPC_STATUS_TIMEOUT: Duration = Duration::from_secs(12);
const IPC_START_TIMEOUT: Duration = Duration::from_secs(28);
const IPC_APPLY_TIMEOUT: Duration = Duration::from_secs(40);
const HELPER_START_TIMEOUT: Duration = Duration::from_secs(8);
const HELPER_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const ROUTE_STABILITY_WINDOW: Duration = Duration::from_secs(1);
const PROCESSING_STABILITY_WINDOW: Duration = Duration::from_secs(10);
const CRASHES_BEFORE_BLOCK: usize = 3;
const RECOVERY_BACKOFFS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];

pub struct EngineController {
    executable: PathBuf,
    native_library: PathBuf,
    native_library_guard_error: Option<String>,
    config_dir: PathBuf,
    pipe_name: String,
    notification_probe_allowed: AtomicBool,
    session_available: AtomicBool,
    state: Mutex<SupervisorState>,
    #[cfg(test)]
    scripted_requests: Mutex<Option<VecDeque<(EngineRequest, EngineResponse)>>>,
}

pub(crate) struct TrayState {
    pub(crate) desired_enabled: bool,
    pub(crate) processed: bool,
    pub(crate) state: EngineState,
    pub(crate) output: EngineOutput,
    pub(crate) settings_reset: bool,
    pub(crate) retrying: bool,
}

impl TrayState {
    pub(crate) fn from_status(status: &EngineStatus) -> Self {
        Self {
            desired_enabled: status.desired_enabled,
            processed: status.output == EngineOutput::Processed,
            state: status.state,
            output: status.output,
            settings_reset: status
                .primary_reason
                .as_ref()
                .is_some_and(|reason| reason.code == PrimaryReasonCode::SettingsReset),
            retrying: status.desired_enabled
                && status.state == EngineState::Degraded
                && status.output == EngineOutput::Inactive,
        }
    }
}

struct SupervisorState {
    child: Option<Child>,
    desired_enabled: bool,
    crash_blocked: bool,
    shutting_down: bool,
    restart_not_before: Option<Instant>,
    recovery_attempt: usize,
    crashes: CrashTracker,
    cached_status: EngineStatus,
    supervisor_warning: Option<String>,
    speech_notice_shown: bool,
    observed_route_issue: Option<ConfigurationIssue>,
    route_fingerprint: Option<RouteFingerprint>,
    route_change_pending: bool,
    route_stable_since: Option<Instant>,
    processing_since: Option<Instant>,
    processing_route_ready: bool,
    fallback_route_ready: bool,
    route_wait_code: PrimaryReasonCode,
    route_wait_endpoint_id: Option<String>,
    helper_metrics_baseline: xmic_engine_protocol::EngineMetrics,
    processing_recovery_events: u64,
    pending_recovery_cause: Option<PrimaryReasonCode>,
    helper_started_once: bool,
    /// A new/resumed helper gets one Start attempt so an immutable DSP failure can establish the
    /// configured Raw/Silence fallback without turning that failure into a periodic DSP retry.
    helper_session_start_pending: bool,
}

#[derive(Default)]
struct CrashTracker {
    events: VecDeque<Instant>,
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
        native_library_guard_error: Option<String>,
        config_dir: PathBuf,
        desired_enabled: bool,
    ) -> Result<Self, String> {
        let mut bootstrap = EngineStatus::bootstrap(Vec::new());
        bootstrap.desired_enabled = desired_enabled;
        let controller = Self {
            executable,
            native_library,
            native_library_guard_error,
            config_dir,
            pipe_name: format!("xmic-engine-{}", std::process::id()),
            notification_probe_allowed: AtomicBool::new(false),
            session_available: AtomicBool::new(true),
            state: Mutex::new(SupervisorState {
                child: None,
                desired_enabled,
                crash_blocked: false,
                shutting_down: false,
                restart_not_before: None,
                recovery_attempt: 0,
                crashes: CrashTracker::default(),
                cached_status: bootstrap,
                supervisor_warning: None,
                speech_notice_shown: false,
                observed_route_issue: None,
                route_fingerprint: None,
                route_change_pending: false,
                route_stable_since: None,
                processing_since: None,
                processing_route_ready: false,
                fallback_route_ready: false,
                route_wait_code: PrimaryReasonCode::AudioDeviceFailure,
                route_wait_endpoint_id: None,
                helper_metrics_baseline: xmic_engine_protocol::EngineMetrics::default(),
                processing_recovery_events: 0,
                pending_recovery_cause: None,
                helper_started_once: false,
                helper_session_start_pending: false,
            }),
            #[cfg(test)]
            scripted_requests: Mutex::new(None),
        };
        // Helper launch is best-effort so a temporary executable/DLL failure cannot abort the UI.
        // The endpoint-aware status poll owns subsequent recovery attempts.
        let _ = controller.ensure_process();
        Ok(controller)
    }

    pub fn status(&self, endpoints: Vec<EndpointDescriptor>) -> EngineStatus {
        let mut state = lock_unpoisoned(&self.state);
        // Use the cached settings only to keep an already-running probe alive. The fresh helper
        // response is checked against the current endpoint snapshot below; if the answer changed,
        // one corrective status request starts or stops the probe without resetting it every poll.
        let now = Instant::now();
        let cached_route_issue =
            crate::route_validation::configuration_issue(&state.cached_status.settings, &endpoints);
        let cached_route_became_valid =
            state.observed_route_issue.is_some() && cached_route_issue.is_none();
        let cached_settings = state.cached_status.settings.clone();
        observe_route_environment(
            &mut state,
            &cached_settings,
            &endpoints,
            cached_route_issue.as_ref(),
            now,
        );
        state.observed_route_issue = cached_route_issue.clone();
        let cached_probe_allowed = self.session_available.load(Ordering::Acquire)
            && self.notification_probe_allowed.load(Ordering::Acquire)
            && !settings_reset_pending(&state.cached_status)
            && !state.desired_enabled
            && notification_probe_route_safe(&state.cached_status.settings, &endpoints);
        match self.get_status_locked(&mut state, cached_probe_allowed) {
            Ok(mut status) => {
                status.endpoints = endpoints.clone();
                let fresh_probe_allowed = self.session_available.load(Ordering::Acquire)
                    && self.notification_probe_allowed.load(Ordering::Acquire)
                    && !settings_reset_pending(&status)
                    && !status.desired_enabled
                    && notification_probe_route_safe(&status.settings, &endpoints);
                if fresh_probe_allowed != cached_probe_allowed {
                    match self.get_status_locked(&mut state, fresh_probe_allowed) {
                        Ok(mut corrected) => {
                            corrected.endpoints = endpoints.clone();
                            status = corrected;
                        }
                        Err(error) => {
                            state.supervisor_warning =
                                Some(format!("notification probe status refresh failed: {error}"));
                        }
                    }
                }
                if status.desired_enabled && settings_reset_pending(&status) {
                    let reason = status.primary_reason.take();
                    status.transition_to_inactive(EngineState::Blocked, reason);
                }
                if !status.primary_reason.as_ref().is_some_and(|reason| {
                    matches!(
                        reason.code,
                        PrimaryReasonCode::SettingsReset | PrimaryReasonCode::CrashLoop
                    )
                }) {
                    let mut settings = status.settings.clone();
                    if apply_default_virtual_device(&mut settings, &status.endpoints) {
                        if let Ok(response) = self.request_locked(
                            &mut state,
                            &EngineRequest::UpdateSettings {
                                settings: Box::new(settings.clone()),
                            },
                        ) {
                            if expect_ack(response).is_ok() {
                                status.settings = settings;
                            }
                        }
                    }
                }
                let route_issue = crate::route_validation::configuration_issue(
                    &status.settings,
                    &status.endpoints,
                );
                let previous_route_issue = state.observed_route_issue.clone();
                let route_became_valid = cached_route_became_valid
                    || previous_route_issue.is_some() && route_issue.is_none();
                observe_route_environment(
                    &mut state,
                    &status.settings,
                    &status.endpoints,
                    route_issue.as_ref(),
                    Instant::now(),
                );
                state.observed_route_issue = route_issue.clone();
                if status.desired_enabled
                    && !status.primary_reason.as_ref().is_some_and(|reason| {
                        matches!(
                            reason.code,
                            PrimaryReasonCode::SettingsReset | PrimaryReasonCode::CrashLoop
                        )
                    })
                {
                    if let Some(issue) = route_issue.clone() {
                        // The endpoint snapshot can race stream startup, so do not infer ownership
                        // from the public output flag. If a helper exists, stop it unconditionally;
                        // otherwise there cannot be an audio session behind this controller.
                        self.stop_helper_for_safety(&mut state);
                        if state.restart_not_before.is_none() {
                            defer_recovery(&mut state, "configuration issue remains");
                        }
                        status.transition_to_inactive(
                            EngineState::Blocked,
                            Some(PrimaryReason::configuration(issue)),
                        );
                    } else if is_recoverable_configuration_block(&status)
                        && self.session_available.load(Ordering::Acquire)
                    {
                        // A race between endpoint enumeration and stream opening can leave the
                        // helper blocked after the route has already become valid. Re-evaluate it
                        // immediately on the next endpoint snapshot rather than waiting for a user
                        // edit.
                        let now = Instant::now();
                        schedule_configuration_recovery(&mut state, route_became_valid, now);
                        if route_handoff_ready(&state, true, now) && should_retry(&state, now) {
                            state.route_change_pending = false;
                            state.route_stable_since = None;
                            state.restart_not_before = None;
                            match self.request_locked(&mut state, &EngineRequest::Start) {
                                Ok(EngineResponse::Ack) => {
                                    let endpoint_snapshot = status.endpoints.clone();
                                    match self.get_status_locked(&mut state, false) {
                                        Ok(mut next) => {
                                            next.endpoints = endpoint_snapshot;
                                            status = next;
                                        }
                                        Err(error) => {
                                            defer_recovery(&mut state, &error);
                                        }
                                    }
                                }
                                Ok(EngineResponse::ConfigurationRequired(issue)) => {
                                    status.transition_to_inactive(
                                        EngineState::Blocked,
                                        Some(PrimaryReason::configuration(issue.clone())),
                                    );
                                    defer_recovery(
                                        &mut state,
                                        "helper still reports configuration required",
                                    );
                                }
                                Ok(EngineResponse::Error(error)) => {
                                    defer_recovery(&mut state, &error);
                                }
                                Err(error) => {
                                    return self
                                        .unavailable_status_after_error(&mut state, status, error)
                                }
                                Ok(response) => {
                                    let error = format!(
                                        "unexpected helper recovery response: {response:?}"
                                    );
                                    self.terminate_failed_helper_locked(&mut state, &error);
                                    return self
                                        .unavailable_status_after_error(&mut state, status, error);
                                }
                            }
                        }
                    }
                }
                apply_supervisor_context(
                    &mut status,
                    state.supervisor_warning.as_deref(),
                    state.restart_not_before,
                    Instant::now(),
                );
                state.cached_status = status.clone();
                status
            }
            Err(error) => {
                let crash_latched = state.crash_blocked && state.desired_enabled;
                let reset_reason = if crash_latched {
                    None
                } else {
                    state
                        .cached_status
                        .primary_reason
                        .clone()
                        .filter(|reason| reason.code == PrimaryReasonCode::SettingsReset)
                };
                let dsp_failure = if crash_latched {
                    None
                } else {
                    startup_dsp_failure(&state.cached_status)
                };
                if state.desired_enabled
                    && reset_reason.is_none()
                    && !crash_latched
                    && state.restart_not_before.is_none()
                {
                    defer_recovery(&mut state, &error);
                }
                let mut status = state.cached_status.clone();
                status.endpoints = endpoints;
                status.desired_enabled = state.desired_enabled;
                let route_issue = crate::route_validation::configuration_issue(
                    &status.settings,
                    &status.endpoints,
                );
                let route_blocked = state.desired_enabled
                    && reset_reason.is_none()
                    && !crash_latched
                    && route_issue.is_some();
                state.observed_route_issue = route_issue.clone();
                let (inactive_state, primary_reason) = if route_blocked {
                    self.stop_helper_for_safety(&mut state);
                    (
                        EngineState::Blocked,
                        route_issue.map(PrimaryReason::configuration),
                    )
                } else if crash_latched {
                    (
                        EngineState::Blocked,
                        Some(PrimaryReason::with_detail(
                            PrimaryReasonCode::CrashLoop,
                            "audio engine recovery is blocked after repeated helper crashes; turn processing Off and then On to retry",
                        )),
                    )
                } else if !state.desired_enabled {
                    (
                        EngineState::Off,
                        reset_reason
                            .or(dsp_failure)
                            .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff))),
                    )
                } else if let Some(reason) = reset_reason {
                    (EngineState::Blocked, Some(reason))
                } else {
                    (
                        EngineState::Degraded,
                        Some(PrimaryReason::with_detail(
                            PrimaryReasonCode::HelperUnavailable,
                            error,
                        )),
                    )
                };
                status.transition_to_inactive(inactive_state, primary_reason);
                if state.desired_enabled {
                    apply_supervisor_context(
                        &mut status,
                        state.supervisor_warning.as_deref(),
                        state.restart_not_before,
                        Instant::now(),
                    );
                }
                state.cached_status = status.clone();
                status
            }
        }
    }

    fn unavailable_status_after_error(
        &self,
        state: &mut SupervisorState,
        mut status: EngineStatus,
        error: String,
    ) -> EngineStatus {
        if state.restart_not_before.is_none() {
            defer_recovery(state, &error);
        }
        status.desired_enabled = state.desired_enabled;
        let inactive_state = if state.desired_enabled {
            EngineState::Degraded
        } else {
            EngineState::Off
        };
        status.transition_to_inactive(
            inactive_state,
            Some(PrimaryReason::with_detail(
                PrimaryReasonCode::HelperUnavailable,
                error,
            )),
        );
        apply_supervisor_context(
            &mut status,
            state.supervisor_warning.as_deref(),
            state.restart_not_before,
            Instant::now(),
        );
        state.cached_status = status.clone();
        status
    }

    pub fn update_settings(&self, mut settings: EngineSettings) -> Result<(), String> {
        // First-run standard VB-CABLE selection is part of the explicit Apply transaction. Doing it
        // before the helper restarts avoids entering a degraded session with no output and then
        // trying to mutate settings while that fallback still owns the stream.
        if let Ok(endpoints) = WasapiPlatform::new().active_endpoints() {
            apply_default_virtual_device(&mut settings, &endpoints);
        }
        settings.validate().map_err(|error| error.to_string())?;
        let mut state = lock_unpoisoned(&self.state);
        let restart = state.desired_enabled && self.session_available.load(Ordering::Acquire);

        // The helper owns the Apply transaction. When processing is active it first moves the
        // current owner to Raw/Silence, saves the full settings, and then starts one fresh owner;
        // the UI must not send a separate Stop/Start pair that would destroy that fallback.
        state.observed_route_issue = None;
        state.restart_not_before = None;
        state.recovery_attempt = 0;
        if !restart && state.desired_enabled {
            // A paused Windows session cannot restart during Apply. Stop is sent only in this case;
            // normal Apply lets the helper perform its fallback handoff.
            self.request_handoff_ack_locked(&mut state, &EngineRequest::Stop)?;
            state.helper_session_start_pending = true;
        }
        self.request_handoff_ack_locked(
            &mut state,
            &EngineRequest::UpdateSettings {
                settings: Box::new(settings.clone()),
            },
        )?;
        state.cached_status.settings = settings;
        state.observed_route_issue = None;
        state.route_fingerprint = None;
        state.route_change_pending = false;
        state.route_stable_since = None;
        state.processing_since = None;
        state.pending_recovery_cause = None;
        state.restart_not_before = None;
        if restart {
            match self.get_status_locked(&mut state, false) {
                Ok(mut status) => {
                    status.settings = state.cached_status.settings.clone();
                    state.cached_status = status;
                }
                Err(error) => defer_recovery(&mut state, &error),
            }
        } else {
            let reason = startup_dsp_failure(&state.cached_status)
                .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff)));
            state
                .cached_status
                .transition_to_inactive(EngineState::Off, reason);
        }
        Ok(())
    }

    pub fn start_requested(&self) -> bool {
        lock_unpoisoned(&self.state).desired_enabled
    }

    pub fn set_notification_probe_allowed(&self, allowed: bool) {
        self.notification_probe_allowed
            .store(allowed, Ordering::Release);
    }

    pub fn set_session_available(&self, available: bool) {
        let was_available = self.session_available.swap(available, Ordering::AcqRel);
        if !was_available && available {
            // A Windows lock/disconnect pause is an external boundary, not another failed retry.
            // A route already waiting for recovery gets a fresh stability window; the next endpoint
            // fingerprint can restart it if devices are still re-enumerating.
            schedule_session_resume(&mut lock_unpoisoned(&self.state), Instant::now());
        }
    }

    /// Releases the normal audio session for a Windows lifecycle pause without discarding the user's
    /// desiredEnabled intent.
    pub fn pause(&self) -> Result<(), String> {
        self.session_available.store(false, Ordering::Release);
        let mut state = lock_unpoisoned(&self.state);
        // A lifecycle pause is an intentional ownership boundary. Suppress crash accounting while
        // checking/stopping the helper, then restore the preserved user intent for post-resume retry.
        let desired_enabled = state.desired_enabled;
        state.desired_enabled = false;
        if let Err(error) = self.reap_child_locked(&mut state) {
            self.terminate_child_locked(&mut state);
            state.desired_enabled = desired_enabled;
            return Err(error);
        }
        if state.child.is_none() {
            state.desired_enabled = desired_enabled;
            return Ok(());
        }
        let result = match self.request_existing_locked(&mut state, &EngineRequest::Stop) {
            Ok(response) => expect_ack(response),
            Err(error) => Err(error),
        };
        if result.is_err() {
            // A lifecycle pause must release every audio device. If the helper cannot confirm Stop,
            // terminate the process rather than reporting SessionUnavailable while it may still own
            // a capture or virtual-output stream.
            self.terminate_child_locked(&mut state);
        }
        state.desired_enabled = desired_enabled;
        if result.is_ok() {
            state.helper_session_start_pending = desired_enabled;
        }
        result
    }

    pub(crate) fn cached_tray_state(&self) -> TrayState {
        TrayState::from_status(&lock_unpoisoned(&self.state).cached_status)
    }

    pub(crate) fn cached_status(&self) -> EngineStatus {
        lock_unpoisoned(&self.state).cached_status.clone()
    }

    pub(crate) fn query_dsp_validation_for_self_test(&self) -> DspLibraryValidation {
        let mut state = lock_unpoisoned(&self.state);
        match self.request_locked(
            &mut state,
            &EngineRequest::GetStatus {
                probe_permit: NotificationProbePermit::default(),
            },
        ) {
            Ok(EngineResponse::Status(status)) => {
                let validation = status.dsp_validation.clone();
                state.cached_status.dsp_validation = validation.clone();
                validation
            }
            _ => state.cached_status.dsp_validation.clone(),
        }
    }

    pub(crate) fn take_notification_probe_notice(&self) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.desired_enabled
            || state.cached_status.state != EngineState::Off
            || state.cached_status.output != EngineOutput::Inactive
            || state.speech_notice_shown
            || !state.cached_status.notification_probe_speech_detected
        {
            return false;
        }
        state.speech_notice_shown = true;
        true
    }

    pub fn start(&self) -> Result<(), EngineStartError> {
        let mut state = lock_unpoisoned(&self.state);
        let already_desired = state.desired_enabled;
        // Record the user's On intent before any helper/device operation can fail. Recoverable start
        // errors must remain scheduled; only Stop cancels this intent.
        state.observed_route_issue = None;
        begin_explicit_start(&mut state);
        let current = match self.get_status_locked(&mut state, false) {
            Ok(current) => current,
            Err(error) => {
                defer_recovery(&mut state, &error);
                return Err(EngineStartError::Runtime(error));
            }
        };
        state.cached_status = current.clone();
        if settings_reset_pending(&current) {
            state.desired_enabled = true;
            state.cached_status.desired_enabled = true;
            let reason = state.cached_status.primary_reason.take();
            state
                .cached_status
                .transition_to_inactive(EngineState::Blocked, reason);
            return Err(EngineStartError::Runtime(
                "settings were reset; apply settings before starting".to_owned(),
            ));
        }
        if already_desired && current.session_active() {
            return Ok(());
        }
        let endpoints = WasapiPlatform::new().active_endpoints().map_err(|error| {
            let message = error.to_string();
            defer_recovery(&mut state, &message);
            EngineStartError::Runtime(message)
        })?;
        let mut settings = state.cached_status.settings.clone();
        if apply_default_virtual_device(&mut settings, &endpoints) {
            expect_ack(
                self.request_locked(
                    &mut state,
                    &EngineRequest::UpdateSettings {
                        settings: Box::new(settings.clone()),
                    },
                )
                .map_err(EngineStartError::Runtime)?,
            )
            .map_err(EngineStartError::Runtime)?;
            state.cached_status.settings = settings;
        }
        if let Some(issue) =
            crate::route_validation::configuration_issue(&state.cached_status.settings, &endpoints)
        {
            state.cached_status.desired_enabled = true;
            state.cached_status.transition_to_inactive(
                EngineState::Blocked,
                Some(PrimaryReason::configuration(issue.clone())),
            );
            return Err(EngineStartError::ConfigurationRequired(issue));
        }
        self.ensure_process_locked(&mut state)
            .map_err(EngineStartError::Runtime)?;
        match self.request_locked(&mut state, &EngineRequest::Start) {
            Ok(EngineResponse::Ack) => Ok(()),
            Ok(EngineResponse::ConfigurationRequired(issue)) => {
                state.cached_status.desired_enabled = true;
                state.cached_status.transition_to_inactive(
                    EngineState::Blocked,
                    Some(PrimaryReason::configuration(issue.clone())),
                );
                Err(EngineStartError::ConfigurationRequired(issue))
            }
            Ok(EngineResponse::Error(error)) => {
                defer_recovery(&mut state, &error);
                Err(EngineStartError::Runtime(error))
            }
            Ok(response) => {
                let error = format!("unexpected helper start response: {response:?}");
                defer_recovery(&mut state, &error);
                Err(EngineStartError::Runtime(error))
            }
            Err(error) => {
                defer_recovery(&mut state, &error);
                Err(EngineStartError::Runtime(error))
            }
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut state = lock_unpoisoned(&self.state);
        state.desired_enabled = false;
        state.helper_session_start_pending = false;
        state.restart_not_before = None;
        state.recovery_attempt = 0;
        state.route_change_pending = false;
        state.route_stable_since = None;
        state.processing_since = None;
        state.pending_recovery_cause = None;
        state.supervisor_warning = None;
        if let Err(error) = self.reap_child_locked(&mut state) {
            self.terminate_child_locked(&mut state);
            return Err(error);
        }
        if state.child.is_some() {
            let result = match self.request_existing_locked(&mut state, &EngineRequest::Stop) {
                Ok(response) => expect_ack(response),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                self.terminate_child_locked(&mut state);
                return Err(error);
            }
        }
        state.cached_status.desired_enabled = false;
        let reason = state
            .cached_status
            .primary_reason
            .take()
            .filter(|reason| reason.code == PrimaryReasonCode::SettingsReset)
            .or_else(|| startup_dsp_failure(&state.cached_status))
            .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff)));
        state
            .cached_status
            .transition_to_inactive(EngineState::Off, reason);
        Ok(())
    }

    pub fn shutdown(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.desired_enabled = false;
        state.helper_session_start_pending = false;
        state.shutting_down = true;
        if state.child.is_some() {
            let _ = self.request_existing_locked(&mut state, &EngineRequest::Shutdown);
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
        // RuntimeSession owns deletion of the helper and managed DLL after this method has
        // confirmed the helper is gone and its native module is unloaded.
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
        if state.desired_enabled && state.observed_route_issue.is_some() {
            return Err("audio route is blocked; waiting for endpoint recovery".to_owned());
        }
        if state.crash_blocked && state.desired_enabled {
            return Err(
                "audio engine recovery is blocked after repeated helper crashes; turn processing Off and then On to retry"
                    .to_owned(),
            );
        }
        if let Some(not_before) = state.restart_not_before {
            if let Some(remaining) = not_before.checked_duration_since(Instant::now()) {
                return Err(format!(
                    "audio engine recovery is waiting for {} ms",
                    remaining.as_millis()
                ));
            }
            state.restart_not_before = None;
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("--audio-engine")
            .arg("--pipe-name")
            .arg(&self.pipe_name)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .arg("--native-library")
            .arg(&self.native_library);
        if let Some(error) = &self.native_library_guard_error {
            command.arg("--native-library-guard-error").arg(error);
        }
        command
            .arg("--config-dir")
            .arg(&self.config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let child = command.spawn().map_err(|error| {
            format!(
                "start audio engine helper {}: {error}",
                self.executable.display()
            )
        })?;
        state.processing_recovery_events = state
            .processing_recovery_events
            .max(state.cached_status.metrics.processing_recovery_events);
        state.helper_metrics_baseline = metrics_for_new_helper(state);
        state.cached_status.metrics.device_restarts = state.helper_metrics_baseline.device_restarts;
        state.helper_started_once = true;
        state.child = Some(child);
        let deadline = Instant::now() + HELPER_START_TIMEOUT;
        loop {
            match self.request_with_deadline_locked(
                state,
                &EngineRequest::Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            ) {
                Ok(EngineResponse::Hello { protocol_version })
                    if protocol_version == PROTOCOL_VERSION =>
                {
                    break
                }
                Ok(response) => {
                    let error = format!("unexpected helper handshake response: {response:?}");
                    self.terminate_failed_helper_locked(state, &error);
                    return Err(error);
                }
                Err(error) => {
                    self.reap_child_locked(state)?;
                    if state.child.is_none() {
                        return Err(format!(
                            "audio engine helper exited during startup: {error}"
                        ));
                    }
                    if Instant::now() >= deadline {
                        let error = format!("audio engine helper startup timed out: {error}");
                        self.terminate_failed_helper_locked(state, &error);
                        return Err(error);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        state.helper_session_start_pending = state.desired_enabled;
        Ok(())
    }

    fn request_existing_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<EngineResponse, String> {
        if state.child.is_none() {
            return Err("audio engine helper is not running".to_owned());
        }
        match self.request_with_deadline_locked(state, request) {
            Ok(response) => Ok(response),
            Err(error) => {
                self.terminate_failed_helper_locked(state, &error);
                Err(error)
            }
        }
    }

    fn request_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<EngineResponse, String> {
        #[cfg(test)]
        if let Some(script) = lock_unpoisoned(&self.scripted_requests).as_mut() {
            let (expected, response) = script.pop_front().expect("unexpected helper request");
            assert_eq!(
                serde_json::to_value(request).unwrap(),
                serde_json::to_value(expected).unwrap(),
            );
            if matches!(request, EngineRequest::Start) {
                state.helper_session_start_pending = false;
            }
            return Ok(response);
        }
        self.ensure_process_locked(state)?;
        if matches!(request, EngineRequest::Start) {
            state.helper_session_start_pending = false;
        }
        match self.request_with_deadline_locked(state, request) {
            Ok(response) => Ok(response),
            Err(error) => {
                self.terminate_failed_helper_locked(state, &error);
                Err(error)
            }
        }
    }

    /// Executes a helper request off the UI supervisor lock and gives the complete exchange a hard
    /// deadline. If a connected helper stops replying, terminate it before exposing an inactive
    /// status; otherwise an audio worker could continue writing behind a false snapshot.
    fn request_with_deadline_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<EngineResponse, String> {
        let pipe_name = self.pipe_name.clone();
        let request = request.clone();
        let request_timeout = ipc_request_timeout(&request);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = match thread::Builder::new()
            .name("xmic-helper-ipc-request".to_owned())
            .spawn(move || {
                let result = send_request(&pipe_name, &request, IPC_CONNECT_TIMEOUT);
                let _ = result_tx.send(result);
            }) {
            Ok(worker) => worker,
            Err(error) => {
                let message = format!("start helper IPC request worker: {error}");
                self.terminate_failed_helper_locked(state, &message);
                return Err(message);
            }
        };

        match result_rx.recv_timeout(request_timeout) {
            Ok(result) => {
                let _ = worker.join();
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let message = format!(
                    "audio engine IPC request timed out after {} seconds; helper terminated",
                    request_timeout.as_secs()
                );
                self.terminate_failed_helper_locked(state, &message);
                let _ = worker.join();
                Err(message)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let message = "audio engine IPC request worker stopped".to_owned();
                self.terminate_failed_helper_locked(state, &message);
                let _ = worker.join();
                Err(message)
            }
        }
    }

    fn request_ack_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<bool, String> {
        match self.request_locked(state, request)? {
            EngineResponse::Ack => Ok(true),
            EngineResponse::Error(_) | EngineResponse::ConfigurationRequired(_) => Ok(false),
            response => {
                let error = format!("unexpected helper acknowledgement: {response:?}");
                self.terminate_failed_helper_locked(state, &error);
                Err(error)
            }
        }
    }

    fn request_handoff_ack_locked(
        &self,
        state: &mut SupervisorState,
        request: &EngineRequest,
    ) -> Result<(), String> {
        let response = match self.request_locked(state, request) {
            Ok(response) => response,
            Err(error) => {
                if state.desired_enabled {
                    defer_recovery(state, &error);
                }
                return Err(error);
            }
        };
        match expect_handoff_ack(response) {
            Ok(()) => Ok(()),
            Err(failure) => {
                self.terminate_failed_helper_locked(state, &failure);
                if state.desired_enabled {
                    defer_recovery(state, &failure);
                }
                Err(failure)
            }
        }
    }

    fn request_status_snapshot_locked(
        &self,
        state: &mut SupervisorState,
        probe_permit: NotificationProbePermit,
    ) -> Result<EngineStatus, String> {
        let helper = match self.request_locked(state, &EngineRequest::GetStatus { probe_permit })? {
            EngineResponse::Status(status) => *status,
            response => {
                let error = format!("unexpected helper status response: {response:?}");
                self.terminate_failed_helper_locked(state, &error);
                return Err(error);
            }
        };
        let mut status = public_status_from_helper(state, helper);
        apply_supervisor_metrics(state, &mut status);
        Ok(status)
    }

    fn get_status_locked(
        &self,
        state: &mut SupervisorState,
        probe_allowed: bool,
    ) -> Result<EngineStatus, String> {
        let probe_permit = notification_probe_permit(state, probe_allowed);
        let mut status = self.request_status_snapshot_locked(state, probe_permit)?;

        let available = self.session_available.load(Ordering::Acquire);
        if should_publish_session_unavailable(&status, state.desired_enabled, available) {
            // Pause has already awaited Helper::Stop. If it could not release ownership, pause()
            // returns an error rather than letting this status claim an inactive output.
            status.transition_to_inactive(
                EngineState::Degraded,
                Some(PrimaryReason::new(PrimaryReasonCode::SessionUnavailable)),
            );
        }

        let now = Instant::now();
        schedule_fallback_recovery(state, &status, available, now);
        let route_environment_stable = route_environment_stable(state, available, now);
        if route_environment_stable && !state.processing_route_ready && should_retry(state, now) {
            if state.fallback_route_ready {
                let route_wait_code = state.route_wait_code;
                let prepared = if fallback_needs_recovery(&status) {
                    if status.session_active() {
                        self.request_handoff_ack_locked(state, &EngineRequest::Stop)?;
                    }
                    self.request_ack_locked(state, &EngineRequest::Start)?
                } else {
                    self.request_handoff_ack_locked(
                        state,
                        &EngineRequest::PrepareFallback {
                            reason: route_wait_code,
                        },
                    )?;
                    true
                };
                if prepared {
                    status = self.request_status_snapshot_locked(state, probe_permit)?;
                    // Keep the handoff pending while the Processing route is unavailable. A later
                    // fingerprint change will restart the one-second stability window; clearing it
                    // here would strand a live fallback with no path back to Processing.
                    state.route_stable_since = None;
                    state.restart_not_before = None;
                } else {
                    defer_recovery(state, "the changed route could not enter fallback");
                    status = self.request_status_snapshot_locked(state, probe_permit)?;
                    status.desired_enabled = state.desired_enabled;
                    apply_supervisor_context(
                        &mut status,
                        state.supervisor_warning.as_deref(),
                        state.restart_not_before,
                        Instant::now(),
                    );
                    return Ok(status);
                }
            } else {
                if status.session_active() {
                    self.request_handoff_ack_locked(state, &EngineRequest::Stop)?;
                }
                state.route_stable_since = None;
                state.restart_not_before = None;
                status.transition_to_inactive(
                    EngineState::Degraded,
                    Some(primary_route_wait_reason(state)),
                );
            }
        }
        let route_handoff_due = route_handoff_ready(state, available, now);
        let ordinary_retry = !state.route_change_pending
            && state.desired_enabled
            && available
            && state.observed_route_issue.is_none()
            && should_start_degraded_session(&status, state.helper_session_start_pending)
            && (state.processing_route_ready
                || (fallback_needs_recovery(&status) && state.fallback_route_ready));
        if ordinary_retry && state.restart_not_before.is_none() {
            defer_recovery(state, "processing is degraded");
        }
        if (route_handoff_due || ordinary_retry) && should_retry(state, now) {
            let recovery_cause = status.primary_reason.as_ref().map(|reason| reason.code);
            state.pending_recovery_cause = recovery_cause;
            // The fingerprint/preflight above is non-invasive. Only after the route has remained
            // stable do we ask the helper to move an active Processing owner to fallback, then
            // release that sole owner for one bounded handoff.
            let fallback_reason =
                recovery_fallback_reason(route_handoff_due, state.route_wait_code);
            if !fallback_needs_recovery(&status) {
                self.request_handoff_ack_locked(
                    state,
                    &EngineRequest::PrepareFallback {
                        reason: fallback_reason,
                    },
                )?;
            }
            self.request_handoff_ack_locked(state, &EngineRequest::Stop)?;
            match self.request_locked(state, &EngineRequest::Start) {
                Ok(EngineResponse::Ack) => {
                    state.route_change_pending = false;
                    state.route_stable_since = None;
                    state.restart_not_before = None;
                    status = self.request_status_snapshot_locked(state, probe_permit)?;
                }
                Ok(EngineResponse::ConfigurationRequired(issue)) => {
                    status.transition_to_inactive(
                        EngineState::Blocked,
                        Some(PrimaryReason::configuration(issue)),
                    );
                    defer_recovery(
                        state,
                        "helper reported a configuration block during handoff",
                    );
                }
                Ok(EngineResponse::Error(error)) => {
                    defer_recovery(state, &error);
                    status = self.request_status_snapshot_locked(state, probe_permit)?;
                }
                Err(error) => return Err(error),
                Ok(response) => {
                    let error = format!("unexpected restart response: {response:?}");
                    self.terminate_failed_helper_locked(state, &error);
                    return Err(error);
                }
            }
        }
        if apply_crash_blocked_status(state, &mut status) {
            // A latched crash block takes precedence over the helper status from before the crash.
        } else if status.state == EngineState::Processing {
            let recovery_cause = state.pending_recovery_cause.take();
            record_successful_recovery(state, recovery_cause);
            status.metrics.processing_recovery_events = state.processing_recovery_events;
            observe_processing_stability(state, now);
            if !state.route_change_pending {
                state.restart_not_before = None;
            }
            state.supervisor_warning = None;
        } else {
            state.processing_since = None;
            if state.desired_enabled
                && available
                && !state.route_change_pending
                && should_start_degraded_session(&status, state.helper_session_start_pending)
                && (state.processing_route_ready
                    || (fallback_needs_recovery(&status) && state.fallback_route_ready))
                && state.restart_not_before.is_none()
            {
                defer_recovery(state, "processing remains degraded");
            }
        }
        if state.route_change_pending
            && !state.processing_route_ready
            && state.observed_route_issue.is_none()
            && state.desired_enabled
        {
            status.primary_reason = Some(primary_route_wait_reason(state));
        }
        status.desired_enabled = state.desired_enabled;
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
        state.helper_session_start_pending = false;
        if state.shutting_down || !state.desired_enabled {
            return Ok(());
        }
        record_unexpected_helper_crash(
            state,
            Instant::now(),
            &format!("audio engine helper exited with {exit_status}"),
        );
        Ok(())
    }

    fn stop_helper_for_safety(&self, state: &mut SupervisorState) {
        if state.child.is_none() {
            return;
        }
        // This is an intentional safety teardown, not a helper crash. Temporarily suppress crash
        // accounting while Stop is being acknowledged or the child is force-terminated.
        let desired_enabled = state.desired_enabled;
        state.desired_enabled = false;
        let stopped = match self.request_existing_locked(state, &EngineRequest::Stop) {
            Ok(response) => matches!(response, EngineResponse::Ack),
            Err(_) => false,
        };
        if !stopped {
            // A safety conflict must never be reported as inactive while the helper may still be
            // writing. Killing the helper is the last-resort ownership boundary; the next status
            // pass can start a clean helper once the route is valid again.
            self.terminate_child_locked(state);
        }
        state.desired_enabled = desired_enabled;
        if stopped {
            state.helper_session_start_pending = desired_enabled;
        }
    }

    fn terminate_failed_helper_locked(&self, state: &mut SupervisorState, error: &str) {
        let had_child = state.child.is_some();
        self.terminate_child_locked(state);
        record_terminated_helper_failure(state, had_child, Instant::now(), error);
    }

    fn terminate_child_locked(&self, state: &mut SupervisorState) {
        state.helper_session_start_pending = false;
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

fn ipc_request_timeout(request: &EngineRequest) -> Duration {
    match request {
        EngineRequest::UpdateSettings { .. } => IPC_APPLY_TIMEOUT,
        EngineRequest::Start => IPC_START_TIMEOUT,
        EngineRequest::Hello { .. } => IPC_HELLO_TIMEOUT,
        EngineRequest::GetStatus { .. }
        | EngineRequest::PrepareFallback { .. }
        | EngineRequest::Stop
        | EngineRequest::Shutdown => IPC_STATUS_TIMEOUT,
    }
}

fn expect_ack(response: EngineResponse) -> Result<(), String> {
    match response {
        EngineResponse::Ack => Ok(()),
        EngineResponse::Error(error) => Err(error),
        EngineResponse::ConfigurationRequired(_) => {
            Err("audio engine requires configuration".to_owned())
        }
        response => Err(format!("unexpected helper acknowledgement: {response:?}")),
    }
}

fn expect_handoff_ack(response: EngineResponse) -> Result<(), String> {
    match response {
        EngineResponse::Ack => Ok(()),
        EngineResponse::Error(error) => Err(format!("helper rejected audio handoff: {error}")),
        EngineResponse::ConfigurationRequired(issue) => {
            Err(format!("helper blocked audio handoff: {issue:?}"))
        }
        response => Err(format!("unexpected helper handoff response: {response:?}")),
    }
}

fn public_status_from_helper(state: &SupervisorState, helper: HelperStatus) -> EngineStatus {
    let HelperStatus {
        session_running,
        output,
        reason,
        dsp_validation,
        settings,
        active_route,
        metrics,
        notification_probe_speech_detected,
    } = helper;
    let mut status = EngineStatus {
        protocol_version: PROTOCOL_VERSION,
        desired_enabled: state.desired_enabled,
        state: EngineState::Degraded,
        output: if session_running {
            output
        } else {
            EngineOutput::Inactive
        },
        primary_reason: reason,
        dsp_validation,
        endpoints: Vec::new(),
        settings,
        active_route: if session_running { active_route } else { None },
        metrics,
        notification_probe_speech_detected,
    };

    if !state.desired_enabled {
        let helper_reason = status.primary_reason.take();
        let persistent_reason = helper_reason
            .as_ref()
            .filter(|reason| reason.code == PrimaryReasonCode::SettingsReset)
            .cloned()
            .or_else(|| helper_reason.filter(|reason| reason.code == PrimaryReasonCode::DspFailure))
            .or_else(|| startup_dsp_failure(&status));
        status.transition_to_inactive(
            EngineState::Off,
            persistent_reason.or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff))),
        );
    } else {
        if status
            .primary_reason
            .as_ref()
            .is_some_and(|reason| reason.code == PrimaryReasonCode::ManualOff)
        {
            status.primary_reason = None;
        }
        derive_enabled_status(&mut status, session_running);
    }
    status
}

fn derive_enabled_status(status: &mut EngineStatus, session_running: bool) {
    if status.primary_reason.as_ref().is_some_and(|reason| {
        reason.code == PrimaryReasonCode::SettingsReset || reason.configuration_issue.is_some()
    }) {
        let reason = status.primary_reason.take();
        status.transition_to_inactive(EngineState::Blocked, reason);
    } else if session_running
        && status.output == EngineOutput::Processed
        && status.primary_reason.is_none()
    {
        status.state = EngineState::Processing;
    } else {
        status.state = EngineState::Degraded;
        if !session_running || status.output == EngineOutput::Inactive {
            let reason = status
                .primary_reason
                .take()
                .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::HelperUnavailable)));
            status.transition_to_inactive(EngineState::Degraded, reason);
        } else if status.primary_reason.is_none() {
            status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::Starting));
        }
    }
}

fn metrics_for_new_helper(state: &SupervisorState) -> xmic_engine_protocol::EngineMetrics {
    let mut baseline = state.cached_status.metrics.inactive();
    if state.helper_started_once {
        baseline.device_restarts = baseline.device_restarts.saturating_add(1);
    }
    baseline
}

fn apply_supervisor_metrics(state: &SupervisorState, status: &mut EngineStatus) {
    status.metrics = status
        .metrics
        .with_counter_baseline(&state.helper_metrics_baseline);
    status.metrics.processing_recovery_events = state.processing_recovery_events;
}

fn notification_probe_permit(state: &SupervisorState, allowed: bool) -> NotificationProbePermit {
    let off = !state.desired_enabled;
    NotificationProbePermit {
        allowed: off && allowed,
        needed: off && !state.speech_notice_shown,
    }
}

fn apply_crash_blocked_status(state: &SupervisorState, status: &mut EngineStatus) -> bool {
    if !state.crash_blocked || !state.desired_enabled {
        return false;
    }
    status.transition_to_inactive(
        EngineState::Blocked,
        Some(PrimaryReason::with_detail(
            PrimaryReasonCode::CrashLoop,
            "audio engine recovery is blocked after repeated helper crashes; turn processing Off and then On to retry",
        )),
    );
    true
}

fn begin_explicit_start(state: &mut SupervisorState) {
    if !state.desired_enabled {
        // A crash block is latched. Only the explicit Off -> On transition gets a fresh crash
        // window and permits the helper to be launched again.
        state.crash_blocked = false;
        state.crashes = CrashTracker::default();
        state.recovery_attempt = 0;
        state.pending_recovery_cause = None;
        state.supervisor_warning = None;
    }
    state.desired_enabled = true;
    state.restart_not_before = None;
}

fn record_terminated_helper_failure(
    state: &mut SupervisorState,
    had_child: bool,
    now: Instant,
    error: &str,
) {
    if !had_child || state.shutting_down || !state.desired_enabled {
        return;
    }
    let crash_count = state.crashes.record(now);
    if crash_count >= CRASHES_BEFORE_BLOCK {
        state.crash_blocked = true;
        state.restart_not_before = None;
        state.supervisor_warning = Some(format!(
            "audio engine recovery is blocked after {crash_count} crashes in 60 seconds: {error}; turn processing Off and then On to retry"
        ));
    } else {
        state.supervisor_warning = Some(format!(
            "audio engine helper failed during IPC ({crash_count}/{CRASHES_BEFORE_BLOCK}): {error}"
        ));
    }
}

fn record_unexpected_helper_crash(state: &mut SupervisorState, now: Instant, error: &str) {
    let crash_count = state.crashes.record(now);
    if crash_count >= CRASHES_BEFORE_BLOCK {
        state.crash_blocked = true;
        state.restart_not_before = None;
        state.supervisor_warning = Some(format!(
            "audio engine recovery is blocked after {crash_count} crashes in 60 seconds: {error}; turn processing Off and then On to retry"
        ));
    } else {
        defer_recovery(state, error);
    }
}

fn primary_route_wait_reason(state: &SupervisorState) -> PrimaryReason {
    if matches!(
        state.route_wait_code,
        PrimaryReasonCode::WaitingForPlayback
            | PrimaryReasonCode::WaitingForCapture
            | PrimaryReasonCode::WaitingForVirtualOutput
    ) {
        PrimaryReason::waiting(state.route_wait_code, state.route_wait_endpoint_id.clone())
    } else {
        PrimaryReason::new(PrimaryReasonCode::AudioDeviceFailure)
    }
}

fn schedule_session_resume(state: &mut SupervisorState, now: Instant) {
    if !state.desired_enabled {
        return;
    }
    if state.route_change_pending {
        state.route_stable_since = Some(now);
        state.restart_not_before = Some(now + ROUTE_STABILITY_WINDOW);
    } else {
        state.restart_not_before = Some(now);
    }
    state.processing_since = None;
    state.recovery_attempt = 0;
    state.supervisor_warning = None;
}

fn observe_route_environment(
    state: &mut SupervisorState,
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
    route_issue: Option<&ConfigurationIssue>,
    now: Instant,
) {
    state.processing_route_ready = processing_route_ready(settings, endpoints);
    state.fallback_route_ready = fallback_route_ready(settings, endpoints);
    let wait_reason = route_wait_reason(settings, endpoints);
    state.route_wait_code = wait_reason.code;
    state.route_wait_endpoint_id = wait_reason.endpoint_id;
    let fingerprint = route_fingerprint(settings, endpoints);
    let changed = state
        .route_fingerprint
        .as_ref()
        .is_some_and(|previous| previous != &fingerprint);
    let first_observation = state.route_fingerprint.is_none();
    state.route_fingerprint = Some(fingerprint);
    if first_observation || !changed {
        return;
    }

    state.route_change_pending = state.desired_enabled;
    state.route_stable_since = Some(now);
    state.processing_since = None;
    state.recovery_attempt = 0;
    state.supervisor_warning = None;
    state.restart_not_before = if route_issue.is_some() {
        None
    } else if state.route_change_pending {
        Some(now + ROUTE_STABILITY_WINDOW)
    } else {
        Some(now)
    };
}

fn fallback_needs_recovery(status: &EngineStatus) -> bool {
    !status.session_active()
        || (status.settings.degraded_output == xmic_engine_protocol::DegradedOutput::Raw
            && status.output == EngineOutput::Silence
            && status
                .primary_reason
                .as_ref()
                .is_some_and(|reason| reason.code == PrimaryReasonCode::WaitingForCapture))
}

fn schedule_fallback_recovery(
    state: &mut SupervisorState,
    status: &EngineStatus,
    available: bool,
    now: Instant,
) {
    if state.route_change_pending
        && state.route_stable_since.is_none()
        && fallback_needs_recovery(status)
        && state.fallback_route_ready
        && state.desired_enabled
        && available
        && !state.crash_blocked
        && state.observed_route_issue.is_none()
        && should_start_degraded_session(status, state.helper_session_start_pending)
    {
        // Restore either a missing owner or the capture missing from a requested Raw fallback.
        state.route_stable_since = Some(now);
        if state.restart_not_before.is_none() {
            defer_recovery(state, "fallback output is incomplete");
        }
    }
}

fn schedule_configuration_recovery(
    state: &mut SupervisorState,
    route_became_valid: bool,
    now: Instant,
) {
    if !state.route_change_pending {
        state.route_change_pending = true;
        state.route_stable_since = Some(now);
        state.restart_not_before = Some(now + ROUTE_STABILITY_WINDOW);
    } else if route_became_valid {
        state.restart_not_before = state
            .route_stable_since
            .map(|since| since + ROUTE_STABILITY_WINDOW);
    }
}

fn recovery_fallback_reason(
    route_handoff_due: bool,
    route_wait_code: PrimaryReasonCode,
) -> PrimaryReasonCode {
    if route_handoff_due {
        route_wait_code
    } else {
        PrimaryReasonCode::Starting
    }
}

fn observe_processing_stability(state: &mut SupervisorState, now: Instant) {
    let stable_since = state.processing_since.get_or_insert(now);
    if now.duration_since(*stable_since) >= PROCESSING_STABILITY_WINDOW {
        state.recovery_attempt = 0;
    }
}

fn record_successful_recovery(state: &mut SupervisorState, cause: Option<PrimaryReasonCode>) {
    if cause == Some(PrimaryReasonCode::CpuOverload) {
        state.processing_recovery_events = state.processing_recovery_events.saturating_add(1);
    }
}

fn route_environment_stable(state: &SupervisorState, available: bool, now: Instant) -> bool {
    state.route_change_pending
        && available
        && state.desired_enabled
        && state.observed_route_issue.is_none()
        && state
            .route_stable_since
            .is_some_and(|since| now.duration_since(since) >= ROUTE_STABILITY_WINDOW)
}

fn route_handoff_ready(state: &SupervisorState, available: bool, now: Instant) -> bool {
    route_environment_stable(state, available, now) && state.processing_route_ready
}

fn defer_recovery(state: &mut SupervisorState, error: &str) {
    if state.crash_blocked {
        return;
    }
    let index = state.recovery_attempt.min(RECOVERY_BACKOFFS.len() - 1);
    state.restart_not_before = Some(Instant::now() + RECOVERY_BACKOFFS[index]);
    state.recovery_attempt = state.recovery_attempt.saturating_add(1);
    state.supervisor_warning = Some(format!("audio engine recovery is waiting: {error}"));
}

fn should_retry(state: &SupervisorState, now: Instant) -> bool {
    state
        .restart_not_before
        .is_none_or(|not_before| now >= not_before)
}

fn settings_reset_pending(status: &EngineStatus) -> bool {
    status
        .primary_reason
        .as_ref()
        .is_some_and(|reason| reason.code == PrimaryReasonCode::SettingsReset)
}

fn startup_dsp_failure(status: &EngineStatus) -> Option<PrimaryReason> {
    status
        .dsp_validation
        .error
        .as_ref()
        .map(|error| PrimaryReason::with_detail(PrimaryReasonCode::DspFailure, error.clone()))
}

fn should_publish_session_unavailable(
    status: &EngineStatus,
    desired_enabled: bool,
    session_available: bool,
) -> bool {
    desired_enabled && !session_available && !settings_reset_pending(status)
}

fn is_recoverable_configuration_block(status: &EngineStatus) -> bool {
    status.state == EngineState::Blocked && status.configuration_issue().is_some()
}

fn retryable_degraded_status(status: &EngineStatus) -> bool {
    if status.primary_reason.as_ref().is_some_and(|reason| {
        matches!(
            reason.code,
            PrimaryReasonCode::SettingsReset | PrimaryReasonCode::CrashLoop
        ) || (reason.code == PrimaryReasonCode::DspFailure && status.dsp_validation.error.is_some())
    }) {
        return false;
    }
    if status.state == EngineState::Off {
        return true;
    }
    status.state == EngineState::Degraded
        && status.primary_reason.as_ref().is_some_and(|reason| {
            matches!(
                reason.code,
                PrimaryReasonCode::Starting
                    | PrimaryReasonCode::WaitingForPlayback
                    | PrimaryReasonCode::WaitingForCapture
                    | PrimaryReasonCode::WaitingForVirtualOutput
                    | PrimaryReasonCode::AudioDeviceFailure
                    | PrimaryReasonCode::DspFailure
                    | PrimaryReasonCode::CpuOverload
                    | PrimaryReasonCode::HelperUnavailable
                    | PrimaryReasonCode::WorkerTimeout
            )
        })
}

fn should_start_degraded_session(status: &EngineStatus, helper_start_pending: bool) -> bool {
    retryable_degraded_status(status)
        || (helper_start_pending
            && status.dsp_validation.error.is_some()
            && status
                .primary_reason
                .as_ref()
                .is_some_and(|reason| reason.code == PrimaryReasonCode::DspFailure))
}

fn apply_supervisor_context(
    status: &mut EngineStatus,
    warning: Option<&str>,
    retry_not_before: Option<Instant>,
    now: Instant,
) {
    if let Some(warning) = warning {
        if let Some(reason) = &mut status.primary_reason {
            reason.detail = Some(match reason.detail.take() {
                Some(detail) => format!("{detail}; {warning}"),
                None => warning.to_owned(),
            });
        } else {
            status.primary_reason = Some(PrimaryReason::with_detail(
                PrimaryReasonCode::HelperUnavailable,
                warning,
            ));
        }
    }
    if let Some(reason) = &mut status.primary_reason {
        reason.retry_after_ms = retry_not_before
            .and_then(|deadline| deadline.checked_duration_since(now))
            .map(|remaining| u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX));
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
    use xmic_engine_protocol::{ActiveRoute, EngineMetrics};

    fn supervisor_state() -> SupervisorState {
        SupervisorState {
            child: None,
            desired_enabled: true,
            crash_blocked: false,
            shutting_down: false,
            restart_not_before: None,
            recovery_attempt: 0,
            crashes: CrashTracker::default(),
            cached_status: EngineStatus::bootstrap(Vec::new()),
            supervisor_warning: None,
            speech_notice_shown: false,
            observed_route_issue: None,
            route_fingerprint: None,
            route_change_pending: false,
            route_stable_since: None,
            processing_since: None,
            processing_route_ready: false,
            fallback_route_ready: false,
            route_wait_code: PrimaryReasonCode::AudioDeviceFailure,
            route_wait_endpoint_id: None,
            helper_metrics_baseline: EngineMetrics::default(),
            processing_recovery_events: 0,
            pending_recovery_cause: None,
            helper_started_once: false,
            helper_session_start_pending: false,
        }
    }

    fn scripted_controller(state: SupervisorState) -> EngineController {
        EngineController {
            executable: PathBuf::from("unused-test-helper.exe"),
            native_library: PathBuf::from("unused-test-dsp.dll"),
            native_library_guard_error: None,
            config_dir: PathBuf::new(),
            pipe_name: "unused-test-pipe".to_owned(),
            notification_probe_allowed: AtomicBool::new(true),
            session_available: AtomicBool::new(true),
            state: Mutex::new(state),
            scripted_requests: Mutex::new(Some(VecDeque::new())),
        }
    }

    fn expect_status(controller: &EngineController, state: &SupervisorState, helper: HelperStatus) {
        lock_unpoisoned(&controller.scripted_requests)
            .as_mut()
            .unwrap()
            .push_back((
                EngineRequest::GetStatus {
                    probe_permit: notification_probe_permit(state, false),
                },
                EngineResponse::Status(Box::new(helper)),
            ));
    }

    fn expect_fallback_restart(controller: &EngineController) {
        lock_unpoisoned(&controller.scripted_requests)
            .as_mut()
            .unwrap()
            .extend([
                (EngineRequest::Stop, EngineResponse::Ack),
                (EngineRequest::Start, EngineResponse::Ack),
            ]);
    }

    fn helper_status(
        session_running: bool,
        output: EngineOutput,
        reason: Option<PrimaryReason>,
    ) -> HelperStatus {
        HelperStatus {
            session_running,
            output,
            reason,
            dsp_validation: DspLibraryValidation::valid(xmic_engine_protocol::DSP_ABI_VERSION),
            settings: EngineSettings::default(),
            active_route: session_running.then(|| ActiveRoute {
                render_endpoint_id: None,
                capture_endpoint_id: Some("mic".to_owned()),
                output_endpoint_id: "virtual-output".to_owned(),
            }),
            metrics: EngineMetrics::default(),
            notification_probe_speech_detected: false,
        }
    }

    fn endpoint(
        id: &str,
        name: &str,
        flow: xmic_platform_api::DataFlow,
        multimedia: bool,
    ) -> EndpointDescriptor {
        EndpointDescriptor {
            platform: "windows".to_owned(),
            id: id.to_owned(),
            group_id: Some(id.to_owned()),
            friendly_name: name.to_owned(),
            flow,
            default_roles: if multimedia {
                vec![xmic_platform_api::DeviceRole::Multimedia]
            } else {
                Vec::new()
            },
        }
    }

    fn ready_route() -> (EngineSettings, Vec<EndpointDescriptor>) {
        let settings = EngineSettings {
            capture_endpoint_id: Some("mic".to_owned()),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_device_mode: Some(xmic_engine_protocol::VirtualDeviceMode::VbCable),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            ..EngineSettings::default()
        };
        let endpoints = vec![
            endpoint(
                "speakers",
                "Speakers",
                xmic_platform_api::DataFlow::Render,
                true,
            ),
            endpoint(
                "mic",
                "Physical microphone",
                xmic_platform_api::DataFlow::Capture,
                false,
            ),
            endpoint(
                "cable-input",
                "CABLE Input (VB-Audio Virtual Cable)",
                xmic_platform_api::DataFlow::Render,
                false,
            ),
            endpoint(
                "cable-output",
                "CABLE Output (VB-Audio Virtual Cable)",
                xmic_platform_api::DataFlow::Capture,
                false,
            ),
        ];
        (settings, endpoints)
    }

    #[test]
    fn missing_route_rearms_fallback_after_owner_loss_without_endpoint_changes() {
        for helper_restarted in [false, true] {
            let mut state = supervisor_state();
            let (settings, mut endpoints) = ready_route();
            let now = Instant::now();
            observe_route_environment(&mut state, &settings, &endpoints, None, now);
            endpoints.retain(|endpoint| endpoint.id != "speakers");
            observe_route_environment(&mut state, &settings, &endpoints, None, now);
            assert!(state.route_change_pending);
            assert!(!state.processing_route_ready);
            assert!(state.fallback_route_ready);

            // A successful fallback cleared this timestamp, then its owner exited.
            state.route_stable_since = None;
            state.restart_not_before = None;
            state.helper_session_start_pending = helper_restarted;
            let status = public_status_from_helper(
                &state,
                helper_status(
                    false,
                    EngineOutput::Inactive,
                    Some(PrimaryReason::new(if helper_restarted {
                        PrimaryReasonCode::HelperUnavailable
                    } else {
                        PrimaryReasonCode::AudioDeviceFailure
                    })),
                ),
            );
            observe_route_environment(&mut state, &settings, &endpoints, None, now);
            assert!(state.route_stable_since.is_none());

            schedule_fallback_recovery(&mut state, &status, true, now);

            assert_eq!(state.route_stable_since, Some(now));
            assert!(!route_environment_stable(&state, true, now));
            let due = state
                .restart_not_before
                .unwrap()
                .max(now + ROUTE_STABILITY_WINDOW);
            assert!(route_environment_stable(&state, true, due));
            assert!(should_retry(&state, due));
            assert!(!route_handoff_ready(&state, true, due));
        }
    }

    #[test]
    fn live_fallback_does_not_rearm_a_pending_route() {
        let mut state = supervisor_state();
        state.route_change_pending = true;
        state.fallback_route_ready = true;
        let status = public_status_from_helper(
            &state,
            helper_status(
                true,
                EngineOutput::Raw,
                Some(PrimaryReason::new(PrimaryReasonCode::WaitingForPlayback)),
            ),
        );

        schedule_fallback_recovery(&mut state, &status, true, Instant::now());

        assert!(state.route_stable_since.is_none());
        assert!(state.restart_not_before.is_none());
    }

    #[test]
    fn inactive_fallback_recovery_keeps_an_existing_backoff() {
        let mut state = supervisor_state();
        state.route_change_pending = true;
        state.fallback_route_ready = true;
        state.recovery_attempt = 3;
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        state.restart_not_before = Some(deadline);
        let status =
            public_status_from_helper(&state, helper_status(false, EngineOutput::Inactive, None));

        schedule_fallback_recovery(&mut state, &status, true, now);
        schedule_fallback_recovery(&mut state, &status, true, now + Duration::from_secs(1));

        assert_eq!(state.route_stable_since, Some(now));
        assert_eq!(state.restart_not_before, Some(deadline));
        assert_eq!(state.recovery_attempt, 3);
        assert!(!should_retry(&state, now + ROUTE_STABILITY_WINDOW));
    }

    #[test]
    fn inactive_fallback_recovery_respects_intent_lifecycle_and_blockers() {
        for blocked_by in [
            "off",
            "session",
            "crash",
            "configuration",
            "settings",
            "output",
        ] {
            let mut state = supervisor_state();
            state.route_change_pending = true;
            state.fallback_route_ready = blocked_by != "output";
            state.desired_enabled = blocked_by != "off";
            state.crash_blocked = blocked_by == "crash";
            if blocked_by == "configuration" {
                state.observed_route_issue = Some(ConfigurationIssue::PhysicalCaptureRequired {
                    virtual_capture_endpoint_id: Some("virtual".to_owned()),
                });
            }
            let status = public_status_from_helper(
                &state,
                helper_status(
                    false,
                    EngineOutput::Inactive,
                    (blocked_by == "settings")
                        .then(|| PrimaryReason::new(PrimaryReasonCode::SettingsReset)),
                ),
            );

            schedule_fallback_recovery(
                &mut state,
                &status,
                blocked_by != "session",
                Instant::now(),
            );

            assert!(state.route_stable_since.is_none(), "{blocked_by}");
            assert!(state.restart_not_before.is_none(), "{blocked_by}");
        }
    }

    #[test]
    fn raw_capture_recovery_restarts_active_silence_without_playback() {
        for route_pending in [true, false] {
            let mut state = supervisor_state();
            let (mut settings, mut endpoints) = ready_route();
            settings.degraded_output = xmic_engine_protocol::DegradedOutput::Raw;
            observe_route_environment(&mut state, &settings, &endpoints, None, Instant::now());
            endpoints.retain(|endpoint| endpoint.id != "speakers");
            observe_route_environment(&mut state, &settings, &endpoints, None, Instant::now());
            assert!(!state.processing_route_ready);
            assert!(state.fallback_route_ready);
            state.route_change_pending = route_pending;
            state.route_stable_since = None;
            state.restart_not_before = None;
            let mut waiting = helper_status(
                true,
                EngineOutput::Silence,
                Some(PrimaryReason::waiting(
                    PrimaryReasonCode::WaitingForCapture,
                    Some("mic".to_owned()),
                )),
            );
            waiting.settings = settings;
            waiting.dsp_validation = DspLibraryValidation::invalid(None, "invalid custom DLL");
            waiting.active_route = Some(ActiveRoute {
                render_endpoint_id: None,
                capture_endpoint_id: None,
                output_endpoint_id: "cable-input".to_owned(),
            });
            let controller = scripted_controller(state);
            let mut state = lock_unpoisoned(&controller.state);
            expect_status(&controller, &state, waiting.clone());

            let status = controller.get_status_locked(&mut state, false).unwrap();

            assert_eq!(status.output, EngineOutput::Silence);
            assert!(
                state.restart_not_before.is_some(),
                "missing capture must schedule recovery"
            );
            assert_eq!(state.recovery_attempt, 1);
            assert_eq!(state.route_stable_since.is_some(), route_pending);
            let initial_deadline = state.restart_not_before;
            expect_status(&controller, &state, waiting.clone());
            controller.get_status_locked(&mut state, false).unwrap();
            assert_eq!(state.restart_not_before, initial_deadline);

            // At the deadline, the still-active Silence owner must stop before capture is reopened.
            state.route_stable_since =
                route_pending.then(|| Instant::now() - ROUTE_STABILITY_WINDOW);
            state.restart_not_before = Some(Instant::now() - Duration::from_secs(1));
            expect_status(&controller, &state, waiting.clone());
            expect_fallback_restart(&controller);
            expect_status(&controller, &state, waiting.clone());
            let retry_started = Instant::now();
            controller.get_status_locked(&mut state, false).unwrap();
            expect_status(&controller, &state, waiting.clone());
            controller.get_status_locked(&mut state, false).unwrap();
            assert_eq!(state.recovery_attempt, 2);
            assert!(state.restart_not_before.unwrap() >= retry_started + RECOVERY_BACKOFFS[1]);

            // Capture becomes available with exactly the same endpoint fingerprint.
            let mut recovered = waiting.clone();
            recovered.output = EngineOutput::Raw;
            recovered.reason = Some(PrimaryReason::new(PrimaryReasonCode::DspFailure));
            recovered.active_route.as_mut().unwrap().capture_endpoint_id = Some("mic".to_owned());
            state.route_stable_since =
                route_pending.then(|| Instant::now() - ROUTE_STABILITY_WINDOW);
            state.restart_not_before = Some(Instant::now() - Duration::from_secs(1));
            expect_status(&controller, &state, waiting);
            expect_fallback_restart(&controller);
            expect_status(&controller, &state, recovered.clone());
            let status = controller.get_status_locked(&mut state, false).unwrap();
            assert_eq!(status.output, EngineOutput::Raw);
            assert!(status.dsp_validation.error.is_some());
            assert!(state.restart_not_before.is_none());
            assert!(state.route_stable_since.is_none());

            // An invalid DSP does not turn the now-working Raw owner into a restart loop.
            expect_status(&controller, &state, recovered);
            controller.get_status_locked(&mut state, false).unwrap();
            assert!(state.restart_not_before.is_none());
            assert!(lock_unpoisoned(&controller.scripted_requests)
                .as_ref()
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn raw_capture_recovery_keeps_healthy_fallbacks_running() {
        use xmic_engine_protocol::DegradedOutput;
        for route_pending in [true, false] {
            for (policy, output, reason) in [
                (
                    DegradedOutput::Raw,
                    EngineOutput::Raw,
                    PrimaryReasonCode::WaitingForPlayback,
                ),
                (
                    DegradedOutput::Silence,
                    EngineOutput::Silence,
                    PrimaryReasonCode::WaitingForCapture,
                ),
                (
                    DegradedOutput::Raw,
                    EngineOutput::Silence,
                    PrimaryReasonCode::DspFailure,
                ),
            ] {
                let mut state = supervisor_state();
                state.route_change_pending = route_pending;
                state.fallback_route_ready = true;
                let mut helper = helper_status(true, output, Some(PrimaryReason::new(reason)));
                helper.settings.degraded_output = policy;
                helper.dsp_validation = DspLibraryValidation::invalid(None, "invalid custom DLL");
                let controller = scripted_controller(state);
                let mut state = lock_unpoisoned(&controller.state);
                expect_status(&controller, &state, helper);

                controller.get_status_locked(&mut state, false).unwrap();

                assert!(state.route_stable_since.is_none());
                assert!(state.restart_not_before.is_none());
                assert!(lock_unpoisoned(&controller.scripted_requests)
                    .as_ref()
                    .unwrap()
                    .is_empty());
            }
        }
    }

    #[test]
    fn raw_capture_recovery_respects_lifecycle_and_safety_gates() {
        for blocked_by in [
            "off",
            "session",
            "crash",
            "configuration",
            "settings",
            "output",
        ] {
            let mut state = supervisor_state();
            state.route_change_pending = true;
            state.fallback_route_ready = blocked_by != "output";
            state.desired_enabled = blocked_by != "off";
            state.crash_blocked = blocked_by == "crash";
            if blocked_by == "configuration" {
                state.observed_route_issue = Some(ConfigurationIssue::PhysicalCaptureRequired {
                    virtual_capture_endpoint_id: Some("virtual".to_owned()),
                });
            }
            let mut helper = helper_status(
                true,
                EngineOutput::Silence,
                Some(PrimaryReason::new(if blocked_by == "settings" {
                    PrimaryReasonCode::SettingsReset
                } else {
                    PrimaryReasonCode::WaitingForCapture
                })),
            );
            helper.settings.degraded_output = xmic_engine_protocol::DegradedOutput::Raw;
            let controller = scripted_controller(state);
            controller
                .session_available
                .store(blocked_by != "session", Ordering::Release);
            let mut state = lock_unpoisoned(&controller.state);
            expect_status(&controller, &state, helper);

            controller.get_status_locked(&mut state, false).unwrap();

            assert!(state.route_stable_since.is_none(), "{blocked_by}");
            assert!(state.restart_not_before.is_none(), "{blocked_by}");
            assert!(lock_unpoisoned(&controller.scripted_requests)
                .as_ref()
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn raw_capture_recovery_remains_retryable_with_an_invalid_dsp() {
        let state = supervisor_state();
        let mut helper = helper_status(
            true,
            EngineOutput::Silence,
            Some(PrimaryReason::waiting(
                PrimaryReasonCode::WaitingForCapture,
                Some("mic".to_owned()),
            )),
        );
        helper.settings.degraded_output = xmic_engine_protocol::DegradedOutput::Raw;
        helper.dsp_validation = DspLibraryValidation::invalid(None, "invalid custom DLL");
        let status = public_status_from_helper(&state, helper.clone());
        assert!(should_start_degraded_session(&status, false));
        assert!(status.dsp_validation.error.is_some());

        helper.output = EngineOutput::Raw;
        helper.reason = Some(PrimaryReason::new(PrimaryReasonCode::DspFailure));
        let recovered = public_status_from_helper(&state, helper);
        assert!(!should_start_degraded_session(&recovered, false));
    }

    #[test]
    fn resume_reseeds_a_pending_route_stability_window() {
        let mut state = supervisor_state();
        state.route_change_pending = true;
        state.route_stable_since = None;
        state.recovery_attempt = 3;
        state.supervisor_warning = Some("waiting".to_owned());
        let now = Instant::now();

        schedule_session_resume(&mut state, now);

        assert_eq!(state.route_stable_since, Some(now));
        assert_eq!(state.restart_not_before, Some(now + ROUTE_STABILITY_WINDOW),);
        assert_eq!(state.recovery_attempt, 0);
        assert!(state.supervisor_warning.is_none());
    }

    #[test]
    fn resume_retries_immediately_when_no_route_handoff_is_pending() {
        let mut state = supervisor_state();
        let now = Instant::now();

        schedule_session_resume(&mut state, now);

        assert_eq!(state.route_stable_since, None);
        assert_eq!(state.restart_not_before, Some(now));
    }

    #[test]
    fn route_change_resets_backoff_and_waits_for_stability() {
        let (settings, mut endpoints) = ready_route();
        let mut state = supervisor_state();
        state.cached_status.settings = settings.clone();
        state.cached_status.state = EngineState::Degraded;
        state.cached_status.output = EngineOutput::Raw;
        let start = Instant::now();
        observe_route_environment(&mut state, &settings, &endpoints, None, start);
        assert!(!state.route_change_pending);

        endpoints[0].id = "headset".to_owned();
        endpoints[0].friendly_name = "Headset".to_owned();
        observe_route_environment(
            &mut state,
            &settings,
            &endpoints,
            None,
            start + Duration::from_millis(10),
        );

        assert!(state.route_change_pending);
        assert_eq!(state.recovery_attempt, 0);
        assert!(!route_handoff_ready(
            &state,
            true,
            start + Duration::from_millis(999),
        ));
        assert!(!should_retry(&state, start + Duration::from_millis(999),));
        assert!(route_handoff_ready(
            &state,
            true,
            start + Duration::from_millis(1_010),
        ));
        assert!(should_retry(&state, start + Duration::from_millis(1_010),));
        state.processing_route_ready = false;
        assert!(route_environment_stable(
            &state,
            true,
            start + Duration::from_millis(1_010),
        ));
        assert!(!route_handoff_ready(
            &state,
            true,
            start + Duration::from_millis(1_010),
        ));
    }

    #[test]
    fn unrelated_endpoint_churn_does_not_restart_the_selected_route() {
        let (settings, mut endpoints) = ready_route();
        let mut state = supervisor_state();
        let start = Instant::now();
        observe_route_environment(&mut state, &settings, &endpoints, None, start);
        endpoints.push(endpoint(
            "unused-headset",
            "Unused headset",
            xmic_platform_api::DataFlow::Render,
            false,
        ));

        observe_route_environment(
            &mut state,
            &settings,
            &endpoints,
            None,
            start + Duration::from_secs(1),
        );

        assert!(!state.route_change_pending);
        assert!(state.route_stable_since.is_none());
    }

    #[test]
    fn ipc_deadlines_cover_each_helper_operation_without_becoming_unbounded() {
        assert_eq!(
            ipc_request_timeout(&EngineRequest::Hello {
                protocol_version: PROTOCOL_VERSION,
            }),
            IPC_HELLO_TIMEOUT,
        );
        assert_eq!(
            ipc_request_timeout(&EngineRequest::Start),
            IPC_START_TIMEOUT
        );
        assert_eq!(
            ipc_request_timeout(&EngineRequest::UpdateSettings {
                settings: Box::new(EngineSettings::default()),
            }),
            IPC_APPLY_TIMEOUT,
        );
        assert!(IPC_APPLY_TIMEOUT > IPC_START_TIMEOUT);
        assert!(IPC_STATUS_TIMEOUT > HELPER_STOP_TIMEOUT);
    }

    #[test]
    fn an_initial_recoverable_configuration_block_gets_a_stability_window() {
        let mut state = supervisor_state();
        state.processing_route_ready = true;
        state.observed_route_issue = None;
        let start = Instant::now();

        schedule_configuration_recovery(&mut state, false, start);

        assert!(state.route_change_pending);
        assert_eq!(
            state.restart_not_before,
            Some(start + ROUTE_STABILITY_WINDOW),
        );
        assert!(!route_handoff_ready(
            &state,
            true,
            start + ROUTE_STABILITY_WINDOW - Duration::from_millis(1),
        ));
        assert!(route_handoff_ready(
            &state,
            true,
            start + ROUTE_STABILITY_WINDOW,
        ));
    }

    #[test]
    fn only_route_handoffs_use_the_structured_route_wait_reason() {
        assert_eq!(
            recovery_fallback_reason(true, PrimaryReasonCode::WaitingForPlayback),
            PrimaryReasonCode::WaitingForPlayback,
        );
        assert_eq!(
            recovery_fallback_reason(false, PrimaryReasonCode::WaitingForCapture),
            PrimaryReasonCode::Starting,
        );
    }

    #[test]
    fn processing_must_remain_stable_before_backoff_resets() {
        let mut state = supervisor_state();
        state.recovery_attempt = 3;
        let start = Instant::now();

        observe_processing_stability(&mut state, start);
        observe_processing_stability(
            &mut state,
            start + PROCESSING_STABILITY_WINDOW - Duration::from_millis(1),
        );
        assert_eq!(state.recovery_attempt, 3);
        observe_processing_stability(&mut state, start + PROCESSING_STABILITY_WINDOW);
        assert_eq!(state.recovery_attempt, 0);
    }

    #[test]
    fn only_a_successful_cpu_overload_recovery_increments_the_counter() {
        let mut state = supervisor_state();

        record_successful_recovery(&mut state, Some(PrimaryReasonCode::AudioDeviceFailure));
        assert_eq!(state.processing_recovery_events, 0);
        record_successful_recovery(&mut state, Some(PrimaryReasonCode::CpuOverload));
        assert_eq!(state.processing_recovery_events, 1);
    }

    #[test]
    fn spawning_another_helper_records_one_pipeline_restart() {
        let mut state = supervisor_state();
        state.cached_status.metrics.device_restarts = 2;
        assert_eq!(metrics_for_new_helper(&state).device_restarts, 2);
        state.helper_started_once = true;
        let second = metrics_for_new_helper(&state);
        assert_eq!(second.device_restarts, 3);
        state.cached_status.metrics.device_restarts = second.device_restarts;
        assert_eq!(metrics_for_new_helper(&state).device_restarts, 4);
    }

    #[test]
    fn supervisor_metrics_preserve_helper_restart_totals() {
        let mut state = supervisor_state();
        state.helper_metrics_baseline = EngineMetrics {
            processed_blocks: 7,
            dsp_failures: 2,
            ..EngineMetrics::default()
        };
        state.processing_recovery_events = 3;
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.metrics.processed_blocks = 5;

        apply_supervisor_metrics(&state, &mut status);

        assert_eq!(status.metrics.processed_blocks, 12);
        assert_eq!(status.metrics.dsp_failures, 2);
        assert_eq!(status.metrics.processing_recovery_events, 3);
    }

    #[test]
    fn inactive_status_preserves_counters_and_clears_session_gauges() {
        let current = EngineMetrics {
            processed_blocks: 42,
            device_restarts: 3,
            dsp_failures: 2,
            pipeline_latency_ms: 18.0,
            input_peak_dbfs: Some(-6.0),
            vad_gate_open: Some(true),
            ..EngineMetrics::default()
        };

        let inactive = current.inactive();

        assert_eq!(inactive.processed_blocks, 42);
        assert_eq!(inactive.device_restarts, 3);
        assert_eq!(inactive.dsp_failures, 2);
        assert_eq!(inactive.pipeline_latency_ms, 0.0);
        assert_eq!(inactive.input_peak_dbfs, None);
        assert_eq!(inactive.vad_gate_open, None);
    }

    #[test]
    fn failed_handoff_ack_is_not_treated_as_a_retryable_negative_ack() {
        assert!(expect_handoff_ack(EngineResponse::Ack).is_ok());
        assert!(expect_handoff_ack(EngineResponse::Error("worker stuck".to_owned())).is_err());
        assert!(
            expect_handoff_ack(EngineResponse::Status(Box::new(helper_status(
                false,
                EngineOutput::Inactive,
                None,
            ))))
            .is_err()
        );
    }

    #[test]
    fn ui_supervisor_alone_derives_public_processing_state() {
        let state = supervisor_state();
        let status =
            public_status_from_helper(&state, helper_status(true, EngineOutput::Processed, None));

        assert!(status.desired_enabled);
        assert_eq!(status.state, EngineState::Processing);
        assert_eq!(status.output, EngineOutput::Processed);
    }

    #[test]
    fn processed_output_with_a_runtime_failure_remains_degraded() {
        let state = supervisor_state();
        let status = public_status_from_helper(
            &state,
            helper_status(
                true,
                EngineOutput::Processed,
                Some(PrimaryReason::new(PrimaryReasonCode::WorkerTimeout)),
            ),
        );

        assert_eq!(status.state, EngineState::Degraded);
        assert_eq!(status.output, EngineOutput::Processed);
    }

    #[test]
    fn ui_supervisor_replaces_helper_manual_off_while_user_intent_is_on() {
        let state = supervisor_state();
        let status = public_status_from_helper(
            &state,
            helper_status(
                false,
                EngineOutput::Inactive,
                Some(PrimaryReason::new(PrimaryReasonCode::ManualOff)),
            ),
        );

        assert_eq!(status.state, EngineState::Degraded);
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::HelperUnavailable),
        );
        assert!(retryable_degraded_status(&status));
    }

    #[test]
    fn ui_supervisor_derives_degraded_fallback_from_helper_runtime_facts() {
        let state = supervisor_state();
        let status = public_status_from_helper(
            &state,
            helper_status(
                true,
                EngineOutput::Raw,
                Some(PrimaryReason::new(PrimaryReasonCode::DspFailure)),
            ),
        );

        assert_eq!(status.state, EngineState::Degraded);
        assert_eq!(status.output, EngineOutput::Raw);
        assert!(status.active_route.is_some());
        assert!(retryable_degraded_status(&status));
    }

    #[test]
    fn startup_dsp_failure_remains_visible_while_processing_is_off() {
        let mut state = supervisor_state();
        state.desired_enabled = false;
        let mut helper = helper_status(false, EngineOutput::Inactive, None);
        helper.dsp_validation = DspLibraryValidation::invalid(None, "missing export");

        let status = public_status_from_helper(&state, helper);

        assert_eq!(status.state, EngineState::Off);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::DspFailure)
        );
        assert!(!retryable_degraded_status(&status));
    }

    #[test]
    fn settings_reset_keeps_primary_priority_over_startup_dsp_failure() {
        let mut state = supervisor_state();
        state.desired_enabled = false;
        let mut helper = helper_status(
            false,
            EngineOutput::Inactive,
            Some(PrimaryReason::with_detail(
                PrimaryReasonCode::SettingsReset,
                "settings reset",
            )),
        );
        helper.dsp_validation = DspLibraryValidation::invalid(Some(1), "ABI mismatch");

        let status = public_status_from_helper(&state, helper);

        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::SettingsReset)
        );
        assert_eq!(status.dsp_validation.actual_abi, Some(1));
        assert!(status.dsp_validation.error.is_some());
    }

    #[test]
    fn immutable_startup_validation_failure_disables_ordinary_retry() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.desired_enabled = true;
        status.state = EngineState::Degraded;
        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::DspFailure));
        status.dsp_validation = DspLibraryValidation::invalid(None, "cannot pin library");

        assert!(!retryable_degraded_status(&status));
        assert!(!should_start_degraded_session(&status, false));
        assert!(should_start_degraded_session(&status, true));

        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::AudioDeviceFailure));
        assert!(
            retryable_degraded_status(&status),
            "fallback device failures must still recover without changing the pinned DLL"
        );
    }

    #[test]
    fn ui_supervisor_derives_off_even_while_an_outputless_probe_is_running() {
        let mut state = supervisor_state();
        state.desired_enabled = false;
        let status =
            public_status_from_helper(&state, helper_status(true, EngineOutput::Inactive, None));

        assert_eq!(status.state, EngineState::Off);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::ManualOff),
        );
    }

    #[test]
    fn ui_supervisor_derives_blocked_from_helper_runtime_facts() {
        let state = supervisor_state();
        let reason = PrimaryReason::configuration(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some("virtual".to_owned()),
        });
        let status = public_status_from_helper(
            &state,
            helper_status(false, EngineOutput::Inactive, Some(reason)),
        );

        assert_eq!(status.state, EngineState::Blocked);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert!(status.active_route.is_none());
    }

    #[test]
    fn recovery_uses_one_two_five_ten_second_backoff_slots() {
        let mut state = supervisor_state();
        for expected in RECOVERY_BACKOFFS {
            let before = Instant::now();
            defer_recovery(&mut state, "failure");
            let deadline = state.restart_not_before.unwrap();
            assert!(deadline.duration_since(before) >= expected);
        }
        defer_recovery(&mut state, "failure");
        assert!(state.restart_not_before.unwrap() > Instant::now());
    }

    #[test]
    fn third_supervisor_terminated_helper_failure_latches_a_crash_block() {
        let mut state = supervisor_state();
        let now = Instant::now();

        record_terminated_helper_failure(&mut state, false, now, "no child");
        assert!(state.crashes.events.is_empty());
        record_terminated_helper_failure(&mut state, true, now, "first timeout");
        record_terminated_helper_failure(
            &mut state,
            true,
            now + Duration::from_secs(1),
            "second timeout",
        );
        assert!(!state.crash_blocked);
        record_terminated_helper_failure(
            &mut state,
            true,
            now + Duration::from_secs(2),
            "third timeout",
        );

        assert!(state.crash_blocked);
        assert!(state.restart_not_before.is_none());
        assert!(state
            .supervisor_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("3 crashes")));
    }

    #[test]
    fn intentional_stop_does_not_count_as_an_ipc_crash() {
        let mut state = supervisor_state();
        state.desired_enabled = false;

        record_terminated_helper_failure(&mut state, true, Instant::now(), "stop timeout");

        assert!(state.crashes.events.is_empty());
        assert!(!state.crash_blocked);
    }

    #[test]
    fn third_helper_crash_in_the_window_latches_a_block() {
        let mut state = supervisor_state();
        let now = Instant::now();

        record_unexpected_helper_crash(&mut state, now, "first");
        assert!(!state.crash_blocked);
        assert!(state.restart_not_before.is_some());
        record_unexpected_helper_crash(&mut state, now + Duration::from_secs(1), "second");
        assert!(!state.crash_blocked);
        assert!(state.restart_not_before.is_some());
        record_unexpected_helper_crash(&mut state, now + Duration::from_secs(2), "third");
        assert!(state.crash_blocked);
        assert!(state.restart_not_before.is_none());

        let mut status = EngineStatus::bootstrap(Vec::new());
        status.metrics.processed_blocks = 42;
        status.metrics.pipeline_latency_ms = 12.0;
        assert!(apply_crash_blocked_status(&state, &mut status));
        assert_eq!(status.state, EngineState::Blocked);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert_eq!(status.metrics.processed_blocks, 42);
        assert_eq!(status.metrics.pipeline_latency_ms, 0.0);
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::CrashLoop)
        );
    }

    #[test]
    fn crashes_outside_the_sixty_second_window_keep_retrying() {
        let mut state = supervisor_state();
        let now = Instant::now();

        record_unexpected_helper_crash(&mut state, now, "first");
        record_unexpected_helper_crash(&mut state, now + CRASH_WINDOW, "second");
        record_unexpected_helper_crash(&mut state, now + CRASH_WINDOW * 2, "third");

        assert!(!state.crash_blocked);
        assert!(state.restart_not_before.is_some());
    }

    #[test]
    fn crash_block_requires_an_explicit_off_then_on_transition() {
        let mut state = supervisor_state();
        state.crash_blocked = true;
        state.recovery_attempt = 3;
        state.supervisor_warning = Some("blocked".to_owned());

        begin_explicit_start(&mut state);
        assert!(state.crash_blocked, "On -> On must not clear the latch");
        state.desired_enabled = false;
        begin_explicit_start(&mut state);
        assert!(!state.crash_blocked);
        assert!(state.desired_enabled);
        assert_eq!(state.recovery_attempt, 0);
        assert!(state.supervisor_warning.is_none());
    }

    #[test]
    fn tray_rendering_is_derived_from_the_public_status_contract() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.desired_enabled = true;
        status.state = EngineState::Degraded;
        status.output = EngineOutput::Raw;
        let tray = TrayState::from_status(&status);
        assert!(!tray.processed);
        assert!(tray.desired_enabled);
        assert_eq!(tray.state, EngineState::Degraded);
        assert_eq!(tray.output, EngineOutput::Raw);
    }

    #[test]
    fn speech_notice_is_consumed_once_per_lifecycle() {
        let mut state = supervisor_state();
        state.desired_enabled = false;
        state.cached_status.notification_probe_speech_detected = true;
        state.speech_notice_shown = true;
        assert!(state.speech_notice_shown);
    }

    #[test]
    fn settings_reset_is_never_auto_retried() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.desired_enabled = true;
        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::SettingsReset));

        assert!(settings_reset_pending(&status));
        assert!(!retryable_degraded_status(&status));
        status.state = EngineState::Blocked;
        assert!(!retryable_degraded_status(&status));
    }

    #[test]
    fn crash_loop_reason_has_priority_over_a_pending_settings_reset() {
        let mut state = supervisor_state();
        state.crash_blocked = true;
        state.cached_status.primary_reason =
            Some(PrimaryReason::new(PrimaryReasonCode::SettingsReset));

        let mut status = state.cached_status.clone();
        assert!(apply_crash_blocked_status(&state, &mut status));
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::CrashLoop)
        );
    }

    #[test]
    fn settings_reset_takes_precedence_over_session_unavailability() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::SettingsReset));
        assert!(!should_publish_session_unavailable(&status, true, false));

        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::AudioDeviceFailure));
        assert!(should_publish_session_unavailable(&status, true, false));
        assert!(!should_publish_session_unavailable(&status, false, false));
    }

    #[test]
    fn only_configuration_block_is_automatically_recoverable() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.desired_enabled = true;
        status.state = EngineState::Blocked;
        status.primary_reason = Some(PrimaryReason::configuration(
            ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id: Some("virtual".to_owned()),
            },
        ));
        assert!(is_recoverable_configuration_block(&status));

        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::CrashLoop));
        assert!(!is_recoverable_configuration_block(&status));
        status.primary_reason = Some(PrimaryReason::new(PrimaryReasonCode::SettingsReset));
        assert!(!is_recoverable_configuration_block(&status));
    }

    #[test]
    fn cached_diagnostics_leave_probe_permission_and_helper_lifecycle_unchanged() {
        let mut state = supervisor_state();
        state.desired_enabled = false;
        state.cached_status.dsp_validation =
            DspLibraryValidation::invalid(None, "invalid custom DLL");
        let validation = state.cached_status.dsp_validation.clone();
        let controller = scripted_controller(state);

        assert_eq!(controller.cached_status().dsp_validation, validation);

        let state = lock_unpoisoned(&controller.state);
        assert!(!state.speech_notice_shown);
        assert!(notification_probe_permit(&state, true).needed);
        assert!(notification_probe_permit(&state, true).allowed);
        assert!(state.child.is_none());
        assert!(!state.helper_started_once);
        assert!(state.crashes.events.is_empty());
    }

    #[test]
    fn notification_probe_permit_is_off_only() {
        let mut state = supervisor_state();

        let permit = notification_probe_permit(&state, true);
        assert!(!permit.allowed);
        assert!(!permit.needed);

        state.desired_enabled = false;
        let permit = notification_probe_permit(&state, true);
        assert!(permit.allowed);
        assert!(permit.needed);
    }
}
