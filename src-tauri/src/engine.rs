use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use xmic_engine_core::{
    DelayCalibrationState as CoreDelayCalibrationState, HostDelayEstimator, HostDelayObservation,
    RenderFrame, SpeechPresenceDetector, DSP_FRAME_DURATION_MS, DSP_SAMPLES_PER_FRAME,
    MAX_RENDER_CHANNELS,
};
use xmic_engine_protocol::{
    ActiveRoute, ConfigurationIssue, DegradedOutput,
    DelayCalibrationState as ProtocolDelayCalibrationState, DspLibraryValidation, EngineMetrics,
    EngineOutput, EngineSettings, EngineState, HelperStatus, NotificationProbePermit,
    PrimaryReason, PrimaryReasonCode, ProcessingLoadMode, DSP_ABI_VERSION,
};
use xmic_engine_runtime::{processing_latency_frames, DspProcessor, DspProcessorError};
use xmic_platform_api::{AudioPlatform, DataFlow, EndpointDescriptor, PlatformError};
use xmic_platform_windows::{
    register_realtime_audio_thread, start_capture_source, start_duplex_source, start_output_sink,
    CaptureSourceRequest, CaptureSourceStream, DuplexSourceRequest, DuplexSourceStream,
    OutputCountersSnapshot, OutputSinkRequest, OutputSinkStream, SourceCountersSnapshot,
    WasapiPlatform,
};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp};
use xmic_webrtc_dsp::{WebRtcLibrary, ABI_VERSION as WEBRTC_ABI_VERSION};

use crate::{
    route_validation::{
        configuration_issue, default_multimedia_capture, is_standard_vb_cable_capture,
        notification_probe_route_safe, opened_virtual_output_issue, route_wait_reason,
    },
    settings_store::{SettingsLoad, SettingsStore},
};

mod load;
mod telemetry;
mod topology;

use load::{ProcessingHistogram, ProcessingOverloadController, PROCESSING_BUDGET_US};
use telemetry::{
    telemetry_channel, RealtimeTelemetry, ReasonUpdate, RuntimeTransition, TelemetryPublisher,
    TelemetryReceiver,
};
use topology::{validate_capture_output_topology, validate_physical_capture, validate_topology};

const _: () = assert!(DSP_ABI_VERSION == WEBRTC_ABI_VERSION);
const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const AEC_CONVERGENCE_TIME: Duration = Duration::from_secs(2);
const STATUS_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const ECHO_PATH_RESET_MIN_INTERVAL: Duration = Duration::from_millis(500);
const QPC_UNITS_PER_MILLISECOND: f32 = 10_000.0;

pub struct RealtimeEngineController {
    control: Mutex<ControlState>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    native_library: StartupDspLibrary,
    dsp_validation: DspLibraryValidation,
    settings_store: SettingsStore,
    probe_speech: Arc<AtomicBool>,
}

#[derive(Clone)]
enum StartupDspLibrary {
    Valid(Arc<WebRtcLibrary>),
    Invalid(String),
}

#[derive(Debug, Clone)]
pub(crate) enum EngineStartError {
    ConfigurationRequired(ConfigurationIssue),
    Runtime(String),
}

impl std::fmt::Display for EngineStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigurationRequired(_) => {
                formatter.write_str("audio device configuration is required")
            }
            Self::Runtime(message) => formatter.write_str(message),
        }
    }
}

struct ControlState {
    settings: EngineSettings,
    settings_reset_pending: bool,
    desired_enabled: bool,
    session: Option<TrackedSession>,
    probe_attempted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Processing,
    NotificationProbe,
}

struct TrackedSession {
    kind: SessionKind,
    stop: Arc<AtomicBool>,
    processing: Option<ProcessingSessionControl>,
    handle: JoinHandle<()>,
}

struct ProcessingSessionControl {
    fallback_requested: Arc<AtomicBool>,
    fallback_reason: Arc<AtomicU8>,
    fallback_ready: Arc<AtomicBool>,
    telemetry: TelemetryReceiver,
}

#[derive(Clone)]
struct RuntimeSnapshot {
    state: EngineState,
    output: EngineOutput,
    primary_reason: Option<PrimaryReason>,
    startup_dsp_failure: Option<PrimaryReason>,
    active_route: Option<ActiveRoute>,
    metrics: EngineMetrics,
}

impl RuntimeSnapshot {
    fn from_settings_load(load: &SettingsLoad, startup_dsp_failure: Option<PrimaryReason>) -> Self {
        let primary_reason = load.reset_reason.as_ref().map(|message| {
            PrimaryReason::with_detail(PrimaryReasonCode::SettingsReset, message.clone())
        });
        Self {
            state: EngineState::Off,
            output: EngineOutput::Inactive,
            primary_reason: primary_reason
                .or_else(|| startup_dsp_failure.clone())
                .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff))),
            startup_dsp_failure,
            active_route: None,
            metrics: EngineMetrics::default(),
        }
    }

    fn publish(&mut self, transition: RuntimeTransition) {
        self.state = transition.state;
        self.output = transition.output;
        self.primary_reason = transition.primary_reason;
        self.active_route = transition.active_route;
        self.metrics = transition.metrics;
    }

    fn apply_realtime_telemetry(&mut self, telemetry: RealtimeTelemetry) {
        self.state = telemetry.state;
        self.output = telemetry.output;
        self.primary_reason = match telemetry.reason {
            ReasonUpdate::Preserve => self.primary_reason.take(),
            ReasonUpdate::Clear => None,
            ReasonUpdate::Code(code) => Some(PrimaryReason::new(code)),
        };
        self.metrics = telemetry.metrics;
    }

    fn off(&mut self) {
        let reason = self
            .primary_reason
            .as_ref()
            .filter(|reason| reason.code == PrimaryReasonCode::SettingsReset)
            .cloned()
            .or_else(|| self.startup_dsp_failure.clone())
            .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff)));
        let mut metrics = self.metrics;
        metrics.clear_session_gauges();
        self.publish(RuntimeTransition {
            state: EngineState::Off,
            output: EngineOutput::Inactive,
            primary_reason: reason,
            active_route: None,
            metrics,
        });
    }

    fn reset_acknowledged(&mut self) {
        self.publish(RuntimeTransition {
            state: EngineState::Off,
            output: EngineOutput::Inactive,
            primary_reason: self
                .startup_dsp_failure
                .clone()
                .or_else(|| Some(PrimaryReason::new(PrimaryReasonCode::ManualOff))),
            active_route: None,
            metrics: self.metrics.inactive(),
        });
    }

    fn unavailable(&mut self, code: PrimaryReasonCode, detail: impl Into<String>) {
        self.unavailable_reason(PrimaryReason::with_detail(code, detail));
    }

    fn unavailable_reason(&mut self, reason: PrimaryReason) {
        if self
            .primary_reason
            .as_ref()
            .is_some_and(|reason| reason.code == PrimaryReasonCode::SettingsReset)
        {
            self.off();
            return;
        }
        let mut metrics = self.metrics;
        metrics.clear_session_gauges();
        self.publish(RuntimeTransition {
            state: EngineState::Degraded,
            output: EngineOutput::Inactive,
            primary_reason: Some(reason),
            active_route: None,
            metrics,
        });
    }

    fn worker_timeout(&mut self, message: impl Into<String>) {
        if self
            .primary_reason
            .as_ref()
            .is_some_and(|reason| reason.code == PrimaryReasonCode::SettingsReset)
        {
            self.off();
            return;
        }
        let keep_session = self.state != EngineState::Off && self.output != EngineOutput::Inactive;
        if keep_session {
            self.state = EngineState::Degraded;
            self.primary_reason = Some(PrimaryReason::with_detail(
                PrimaryReasonCode::WorkerTimeout,
                message,
            ));
        } else {
            self.unavailable(PrimaryReasonCode::WorkerTimeout, message);
        }
    }

    fn blocked(&mut self, issue: ConfigurationIssue) {
        let mut metrics = self.metrics;
        metrics.clear_session_gauges();
        self.publish(RuntimeTransition {
            state: EngineState::Blocked,
            output: EngineOutput::Inactive,
            primary_reason: Some(PrimaryReason::configuration(issue)),
            active_route: None,
            metrics,
        });
    }

    fn blocked_message(&mut self, message: impl Into<String>) {
        let mut metrics = self.metrics;
        metrics.clear_session_gauges();
        self.publish(RuntimeTransition {
            state: EngineState::Blocked,
            output: EngineOutput::Inactive,
            primary_reason: Some(PrimaryReason::with_detail(
                PrimaryReasonCode::SafetyConflict,
                message,
            )),
            active_route: None,
            metrics,
        });
    }

    fn starting(&mut self) {
        let mut metrics = self.metrics;
        metrics.clear_session_gauges();
        if self.state != EngineState::Off || metrics.processed_blocks > 0 {
            metrics.device_restarts = metrics.device_restarts.saturating_add(1);
        }
        metrics.processing_load_mode = ProcessingLoadMode::Full;
        self.publish(RuntimeTransition {
            state: EngineState::Degraded,
            output: EngineOutput::Inactive,
            primary_reason: Some(PrimaryReason::new(PrimaryReasonCode::Starting)),
            active_route: None,
            metrics,
        });
    }
}

impl RealtimeEngineController {
    #[cfg(test)]
    fn new(native_library: PathBuf, settings_store: SettingsStore) -> Self {
        Self::new_with_library_guard_error(native_library, None, settings_store)
    }

    pub(crate) fn new_with_library_guard_error(
        native_library: PathBuf,
        library_guard_error: Option<String>,
        settings_store: SettingsStore,
    ) -> Self {
        let (native_library, dsp_validation) = if let Some(message) = library_guard_error {
            (
                StartupDspLibrary::Invalid(message.clone()),
                DspLibraryValidation::invalid(None, message),
            )
        } else {
            match WebRtcLibrary::load(&native_library) {
                Ok(library) => {
                    let actual_abi = library.actual_abi();
                    (
                        StartupDspLibrary::Valid(library),
                        DspLibraryValidation::valid(actual_abi),
                    )
                }
                Err(failure) => {
                    let message = failure.to_string();
                    (
                        StartupDspLibrary::Invalid(message.clone()),
                        DspLibraryValidation::invalid(failure.actual_abi, message),
                    )
                }
            }
        };
        let loaded = settings_store.load();
        let startup_dsp_failure = dsp_validation.error.as_ref().map(|message| {
            PrimaryReason::with_detail(PrimaryReasonCode::DspFailure, message.clone())
        });
        let runtime = RuntimeSnapshot::from_settings_load(&loaded, startup_dsp_failure);
        Self {
            control: Mutex::new(ControlState {
                settings: loaded.settings,
                settings_reset_pending: loaded.reset_reason.is_some(),
                desired_enabled: false,
                session: None,
                probe_attempted: false,
            }),
            runtime: Arc::new(Mutex::new(runtime)),
            native_library,
            dsp_validation,
            settings_store,
            probe_speech: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn status(&self, probe_permit: NotificationProbePermit) -> HelperStatus {
        let mut control = lock_unpoisoned(&self.control);
        apply_pending_telemetry(&mut control.session, &self.runtime);
        let processing_finished = control.session.as_ref().is_some_and(|session| {
            session.kind == SessionKind::Processing && session.handle.is_finished()
        });
        let probe_finished = control.session.as_ref().is_some_and(|session| {
            session.kind == SessionKind::NotificationProbe && session.handle.is_finished()
        });
        reap_finished(&mut control.session);
        if probe_finished && !self.probe_speech.load(Ordering::Acquire) {
            // A temporary missing/ambiguous capture should not consume the whole app lifetime; the
            // next endpoint-aware poll may safely try the optional probe again.
            control.probe_attempted = false;
        }
        if processing_finished {
            if control.desired_enabled {
                let mut runtime = lock_unpoisoned(&self.runtime);
                if runtime.state != EngineState::Blocked && !control.settings_reset_pending {
                    runtime.unavailable(
                        PrimaryReasonCode::AudioDeviceFailure,
                        "audio session stopped unexpectedly; waiting for recovery",
                    );
                }
            } else {
                // A stop timeout may leave the worker tracked for a later poll. Once it finally
                // exits, publish the true Off state (while preserving an unacknowledged reset).
                lock_unpoisoned(&self.runtime).off();
            }
        }
        self.synchronize_probe(&mut control, probe_permit);
        if !control.desired_enabled
            && !control
                .session
                .as_ref()
                .is_some_and(|session| session.kind == SessionKind::Processing)
        {
            // A stale failure state must not survive after the normal session owner is gone. The
            // reset marker, if any, is intentionally retained by `off()` until Apply.
            lock_unpoisoned(&self.runtime).off();
        }
        if control.settings_reset_pending
            && !control
                .session
                .as_ref()
                .is_some_and(|session| session.kind == SessionKind::Processing)
        {
            let mut runtime = lock_unpoisoned(&self.runtime);
            let message = runtime
                .primary_reason
                .as_ref()
                .filter(|reason| reason.code == PrimaryReasonCode::SettingsReset)
                .and_then(|reason| reason.detail.clone())
                .unwrap_or_else(|| {
                    "settings were reset to current defaults; apply the settings before starting"
                        .to_owned()
                });
            runtime.publish(RuntimeTransition {
                state: if control.desired_enabled {
                    EngineState::Blocked
                } else {
                    EngineState::Off
                },
                output: EngineOutput::Inactive,
                primary_reason: Some(PrimaryReason::with_detail(
                    PrimaryReasonCode::SettingsReset,
                    message,
                )),
                active_route: None,
                metrics: EngineMetrics::default(),
            });
        }
        let runtime = lock_unpoisoned(&self.runtime).clone();
        HelperStatus {
            session_running: control.session.is_some(),
            output: runtime.output,
            reason: runtime.primary_reason,
            dsp_validation: self.dsp_validation.clone(),
            settings: control.settings.clone(),
            active_route: runtime.active_route,
            metrics: runtime.metrics,
            notification_probe_speech_detected: self.probe_speech.load(Ordering::Acquire),
        }
    }

    pub fn prepare_fallback(&self, reason: PrimaryReasonCode) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        self.prepare_fallback_locked(&mut control, reason)
    }

    fn prepare_fallback_locked(
        &self,
        control: &mut ControlState,
        reason: PrimaryReasonCode,
    ) -> Result<(), String> {
        apply_pending_telemetry(&mut control.session, &self.runtime);
        reap_finished(&mut control.session);
        let Some(processing) = control
            .session
            .as_ref()
            .and_then(|session| session.processing.as_ref())
        else {
            return Ok(());
        };
        if processing.fallback_ready.load(Ordering::Acquire) {
            if reason != PrimaryReasonCode::Starting {
                lock_unpoisoned(&self.runtime).primary_reason = Some(PrimaryReason::new(reason));
            }
            return Ok(());
        }
        processing
            .fallback_reason
            .store(encode_fallback_reason(reason), Ordering::Release);
        processing.fallback_requested.store(true, Ordering::Release);
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !processing.fallback_ready.load(Ordering::Acquire) {
            if control
                .session
                .as_ref()
                .is_none_or(|session| session.handle.is_finished())
            {
                return Err("audio session ended before fallback became ready".to_owned());
            }
            if Instant::now() >= deadline {
                return Err("audio session did not enter fallback in time".to_owned());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub fn update_settings(&self, settings: EngineSettings) -> Result<(), String> {
        settings.validate().map_err(|error| error.to_string())?;
        let mut control = lock_unpoisoned(&self.control);
        self.prepare_fallback_locked(&mut control, PrimaryReasonCode::Starting)?;
        if control
            .session
            .as_ref()
            .is_some_and(|session| session.kind == SessionKind::NotificationProbe)
            && !stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime))
        {
            return Err("notification probe did not release the microphone in time".to_owned());
        }

        self.settings_store.save(&settings)?;
        // Persistence is the acknowledgement point for SettingsReset. Update the in-memory copy
        // before waiting on the old fallback so a stop timeout cannot expose stale settings.
        control.settings = settings;
        control.settings_reset_pending = false;
        let desired_enabled = control.desired_enabled;
        if !stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime)) {
            return Err("fallback did not release the audio devices before Apply".to_owned());
        }
        // Publish the acknowledged inactive state only after the former owner is definitely gone.
        lock_unpoisoned(&self.runtime).reset_acknowledged();
        control.probe_attempted = false;
        self.probe_speech.store(false, Ordering::Release);
        if desired_enabled {
            // The settings write itself succeeded. A failed restart is represented by the runtime
            // Degraded/Blocked snapshot and the UI scheduler will retry; Apply must not pretend the
            // new settings were rejected merely because the first handoff attempt was unavailable.
            let _ = self.start_locked(&mut control);
        }
        Ok(())
    }

    pub fn start(&self) -> Result<(), EngineStartError> {
        let mut control = lock_unpoisoned(&self.control);
        self.start_locked(&mut control)
    }

    fn start_locked(&self, control: &mut ControlState) -> Result<(), EngineStartError> {
        control.desired_enabled = true;
        apply_pending_telemetry(&mut control.session, &self.runtime);
        reap_finished(&mut control.session);
        if control
            .session
            .as_ref()
            .is_some_and(|session| session.kind == SessionKind::Processing)
        {
            return Err(EngineStartError::Runtime(
                "an audio session is already active or still stopping".to_owned(),
            ));
        }
        if !stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime)) {
            return Err(EngineStartError::Runtime(
                "notification probe is still releasing the microphone".to_owned(),
            ));
        }
        self.probe_speech.store(false, Ordering::Release);

        control
            .settings
            .validate()
            .map_err(|error| EngineStartError::Runtime(error.to_string()))?;
        if control.settings_reset_pending {
            lock_unpoisoned(&self.runtime).publish(RuntimeTransition {
                state: EngineState::Blocked,
                output: EngineOutput::Inactive,
                primary_reason: Some(PrimaryReason::with_detail(
                    PrimaryReasonCode::SettingsReset,
                    "settings were reset to current defaults; apply the settings before starting",
                )),
                active_route: None,
                metrics: EngineMetrics::default(),
            });
            return Err(EngineStartError::Runtime(
                "settings were reset; apply settings before starting".to_owned(),
            ));
        }
        let endpoints = WasapiPlatform::new()
            .active_endpoints()
            .map_err(|error| EngineStartError::Runtime(error.to_string()))?;
        if let Some(issue) = configuration_issue(&control.settings, &endpoints) {
            lock_unpoisoned(&self.runtime).blocked(issue.clone());
            return Err(EngineStartError::ConfigurationRequired(issue));
        }

        lock_unpoisoned(&self.runtime).starting();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let fallback_requested = Arc::new(AtomicBool::new(false));
        let worker_fallback_requested = Arc::clone(&fallback_requested);
        let fallback_reason = Arc::new(AtomicU8::new(encode_fallback_reason(
            PrimaryReasonCode::Starting,
        )));
        let worker_fallback_reason = Arc::clone(&fallback_reason);
        let fallback_ready = Arc::new(AtomicBool::new(false));
        let worker_fallback_ready = Arc::clone(&fallback_ready);
        let (telemetry_publisher, telemetry_receiver) = telemetry_channel();
        let settings = control.settings.clone();
        let native_library = self.native_library.clone();
        let runtime = Arc::clone(&self.runtime);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("xmic-audio-session".to_owned())
            .spawn(move || {
                run_worker(
                    settings,
                    native_library,
                    WorkerContext {
                        stop: worker_stop,
                        fallback_requested: worker_fallback_requested,
                        fallback_reason: worker_fallback_reason,
                        fallback_ready: worker_fallback_ready,
                        telemetry: telemetry_publisher,
                        runtime,
                        ready: ready_tx,
                    },
                )
            })
            .map_err(|error| EngineStartError::Runtime(format!("start audio worker: {error}")))?;
        control.session = Some(TrackedSession {
            kind: SessionKind::Processing,
            stop,
            processing: Some(ProcessingSessionControl {
                fallback_requested,
                fallback_reason,
                fallback_ready,
                telemetry: telemetry_receiver,
            }),
            handle,
        });

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(session) = &control.session {
                    session.stop.store(true, Ordering::Release);
                }
                lock_unpoisoned(&self.runtime).worker_timeout(
                    "audio session startup timed out; the worker remains owned until it exits",
                );
                Err(EngineStartError::Runtime(
                    "audio session startup timed out".to_owned(),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngineStartError::Runtime(
                "audio worker stopped during startup".to_owned(),
            )),
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        control.desired_enabled = false;
        if !stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime)) {
            lock_unpoisoned(&self.runtime)
                .worker_timeout("audio worker did not stop within 10 seconds; it remains tracked");
            return Err("audio worker did not stop within 10 seconds".to_owned());
        }
        lock_unpoisoned(&self.runtime).off();
        control.probe_attempted = false;
        Ok(())
    }

    pub fn shutdown(&self) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        control.desired_enabled = false;
        if stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime)) {
            lock_unpoisoned(&self.runtime).off();
            Ok(())
        } else {
            Err("the audio session did not stop before shutdown".to_owned())
        }
    }

    fn synchronize_probe(&self, control: &mut ControlState, permit: NotificationProbePermit) {
        let runtime_off = lock_unpoisoned(&self.runtime).state == EngineState::Off;
        if !runtime_off
            || control.settings_reset_pending
            || !notification_probe_permitted(control.desired_enabled, permit)
        {
            if control
                .session
                .as_ref()
                .is_some_and(|session| session.kind == SessionKind::NotificationProbe)
            {
                let _ = stop_tracked(&mut control.session, STOP_TIMEOUT, Some(&self.runtime));
                if permit.needed && !control.desired_enabled && !control.settings_reset_pending {
                    control.probe_attempted = false;
                }
            }
            if !permit.needed {
                self.probe_speech.store(false, Ordering::Release);
            }
            return;
        }
        if control.session.is_some() || control.probe_attempted {
            return;
        }

        control.probe_attempted = true;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let settings = control.settings.clone();
        let detected = Arc::clone(&self.probe_speech);
        if let Ok(handle) = thread::Builder::new()
            .name("xmic-notification-probe".to_owned())
            .spawn(move || run_notification_probe(settings, worker_stop, detected))
        {
            control.session = Some(TrackedSession {
                kind: SessionKind::NotificationProbe,
                stop,
                processing: None,
                handle,
            });
        }
    }
}

fn notification_probe_permitted(desired_enabled: bool, permit: NotificationProbePermit) -> bool {
    !desired_enabled && permit.allowed && permit.needed
}

fn apply_pending_telemetry(session: &mut Option<TrackedSession>, runtime: &Mutex<RuntimeSnapshot>) {
    // Filter the epoch and apply its packet under the same lock as owner transitions.
    let mut runtime = lock_unpoisoned(runtime);
    let telemetry = session
        .as_mut()
        .and_then(|session| session.processing.as_mut())
        .and_then(|processing| processing.telemetry.take_latest());
    if let Some(telemetry) = telemetry {
        runtime.apply_realtime_telemetry(telemetry);
    }
}

fn reap_finished(session: &mut Option<TrackedSession>) {
    if session
        .as_ref()
        .is_some_and(|session| session.handle.is_finished())
    {
        if let Some(session) = session.take() {
            let _ = session.handle.join();
        }
    }
}

/// A timed-out session is deliberately left in the sole owner slot. Dropping its JoinHandle would
/// detach a thread that may still hold an audio endpoint and would make non-overlap impossible.
fn stop_tracked(
    session: &mut Option<TrackedSession>,
    timeout: Duration,
    runtime: Option<&Mutex<RuntimeSnapshot>>,
) -> bool {
    if let Some(runtime) = runtime {
        apply_pending_telemetry(session, runtime);
    }
    let Some(current) = session.as_ref() else {
        return true;
    };
    current.stop.store(true, Ordering::Release);
    let deadline = Instant::now() + timeout;
    while !current.handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(runtime) = runtime {
        apply_pending_telemetry(session, runtime);
    }
    if let Some(finished) = session.take() {
        let _ = finished.handle.join();
    }
    true
}

fn run_notification_probe(
    settings: EngineSettings,
    stop: Arc<AtomicBool>,
    detected: Arc<AtomicBool>,
) {
    // The optional reminder is not part of the audio-output path and deliberately runs without
    // MMCSS priority. Allocate RNNoise before opening the capture stream so its observer loop can
    // sleep normally and never performs lazy heap allocation while processing a frame.
    let Some(mut probe) = SpeechProbe::new() else {
        return;
    };
    let endpoint_snapshot = WasapiPlatform::new().active_endpoints().unwrap_or_default();
    if !notification_probe_route_safe(&settings, &endpoint_snapshot) {
        // Prove the selected/default capture is physical before opening any WASAPI capture stream.
        // The post-open checks below cover a device change racing this snapshot.
        return;
    }
    let mut source = match start_capture_source(CaptureSourceRequest {
        capture_endpoint_id: settings.capture_endpoint_id.clone(),
        capture_channel: settings.capture_channel,
    }) {
        Ok(source) => source,
        Err(_) => return,
    };
    let verification_snapshot = WasapiPlatform::new()
        .active_endpoints()
        .unwrap_or_else(|_| endpoint_snapshot.clone());
    if validate_physical_capture(&source.info().capture_endpoint).is_err()
        || !capture_matches_selected_physical(
            &settings,
            &source.info().capture_endpoint,
            &verification_snapshot,
        )
        || !notification_probe_route_safe(&settings, &verification_snapshot)
        || capture_is_configured_virtual(
            &settings,
            &source.info().capture_endpoint,
            &verification_snapshot,
        )
    {
        let _ = source.stop();
        return;
    }
    while !stop.load(Ordering::Acquire) && !detected.load(Ordering::Acquire) {
        if source.worker_finished() {
            break;
        }
        let Some(capture) = source.pop_capture() else {
            thread::park_timeout(Duration::from_millis(20));
            continue;
        };
        if probe
            .observe(&capture.samples, peak_dbfs(&capture.samples))
            .confirmed
        {
            detected.store(true, Ordering::Release);
        }
    }
    let _ = source.stop();
}

struct SpeechProbe {
    rnnoise: RnnoiseDsp,
    presence: SpeechPresenceDetector,
    discarded: [f32; DSP_SAMPLES_PER_FRAME],
    unavailable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SpeechProbeFrame {
    probability: Option<f32>,
    confirmed: bool,
}

impl SpeechProbe {
    fn new() -> Option<Self> {
        Some(Self {
            rnnoise: RnnoiseDsp::new(RnnoiseConfig { mix: 1.0 }).ok()?,
            presence: SpeechPresenceDetector::default(),
            discarded: [0.0; DSP_SAMPLES_PER_FRAME],
            unavailable: false,
        })
    }

    fn observe(
        &mut self,
        samples: &[f32; DSP_SAMPLES_PER_FRAME],
        peak_dbfs: f32,
    ) -> SpeechProbeFrame {
        if self.unavailable || self.presence.fired() {
            return SpeechProbeFrame::default();
        }
        match self.rnnoise.process_frame(samples, &mut self.discarded) {
            Ok(metrics) => SpeechProbeFrame {
                probability: Some(metrics.vad_probability),
                confirmed: self.presence.observe(metrics.vad_probability, peak_dbfs),
            },
            Err(_) => {
                self.unavailable = true;
                SpeechProbeFrame::default()
            }
        }
    }
}

struct WorkerContext {
    stop: Arc<AtomicBool>,
    fallback_requested: Arc<AtomicBool>,
    fallback_reason: Arc<AtomicU8>,
    fallback_ready: Arc<AtomicBool>,
    telemetry: TelemetryPublisher,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    ready: mpsc::SyncSender<Result<(), EngineStartError>>,
}

fn run_worker(settings: EngineSettings, native_library: StartupDspLibrary, context: WorkerContext) {
    let stop = context.stop.clone();
    let fallback_requested = context.fallback_requested.clone();
    let fallback_reason = context.fallback_reason.clone();
    let fallback_ready = context.fallback_ready.clone();
    let mut telemetry = context.telemetry;
    let runtime = context.runtime.clone();
    let ready = context.ready.clone();
    let _realtime = match register_realtime_audio_thread() {
        Ok(registration) => registration,
        Err(error) => {
            let message = format!("register audio worker with MMCSS: {error}");
            lock_unpoisoned(&runtime).unavailable(PrimaryReasonCode::AudioDeviceFailure, &message);
            let _ = ready.send(Err(EngineStartError::Runtime(message)));
            return;
        }
    };
    match RealtimeSession::start(&settings, &native_library) {
        Ok(mut session) => {
            session.publish_initial(&runtime);
            let _ = ready.send(Ok(()));
            let initial_metrics = lock_unpoisoned(&runtime).metrics;
            let exit = session.run(
                &stop,
                &fallback_requested,
                &fallback_reason,
                &mut telemetry,
                initial_metrics,
            );
            if stop.load(Ordering::Acquire) {
                let _ = session.stop();
                return;
            }
            match exit {
                SessionExit::FallbackRequired {
                    failure,
                    mut metrics,
                } => {
                    let reason = failure.into_start_failure();
                    note_failure_metric(&mut metrics, reason.code);
                    lock_unpoisoned(&runtime).metrics = metrics;
                    run_reused_fallback_after_processing(
                        &settings,
                        reason,
                        session,
                        FallbackContext {
                            stop: &stop,
                            fallback_ready: &fallback_ready,
                            telemetry: &mut telemetry,
                            runtime: &runtime,
                        },
                    );
                }
                SessionExit::RequestedFallback(reason, metrics) => {
                    lock_unpoisoned(&runtime).metrics = metrics;
                    let failure = SessionStartFailure::new(
                        reason,
                        if reason == PrimaryReasonCode::Starting {
                            "settings are being applied"
                        } else {
                            "configured audio route changed or became unavailable"
                        },
                    );
                    let context = FallbackContext {
                        stop: &stop,
                        fallback_ready: &fallback_ready,
                        telemetry: &mut telemetry,
                        runtime: &runtime,
                    };
                    if control_fallback_reuses_processing_resources(reason) {
                        run_reused_fallback_after_processing(&settings, failure, session, context);
                    } else {
                        // A route request means an endpoint identity changed; rebuild those
                        // resources rather than retaining the previous default/fixed endpoint.
                        let _ = session.stop();
                        run_fallback_after_processing(&settings, failure, context);
                    }
                }
                SessionExit::Failed(failure, metrics) => {
                    let _ = session.stop();
                    lock_unpoisoned(&runtime).metrics = metrics;
                    run_fallback_after_processing(
                        &settings,
                        SessionStartFailure::new(
                            PrimaryReasonCode::AudioDeviceFailure,
                            failure.message(),
                        ),
                        FallbackContext {
                            stop: &stop,
                            fallback_ready: &fallback_ready,
                            telemetry: &mut telemetry,
                            runtime: &runtime,
                        },
                    );
                }
                SessionExit::Stopped => {
                    let _ = session.stop();
                }
            }
        }
        Err(failure) if failure.configuration_issue.is_some() => {
            let message = failure.message.clone();
            if let Some(issue) = failure.configuration_issue.clone() {
                lock_unpoisoned(&runtime).blocked(issue.clone());
                let _ = ready.send(Err(EngineStartError::ConfigurationRequired(issue)));
            } else {
                lock_unpoisoned(&runtime).blocked_message(message.clone());
                let _ = ready.send(Err(EngineStartError::Runtime(message)));
            }
        }
        Err(failure) => {
            let mut failure_metrics = lock_unpoisoned(&runtime).metrics;
            note_failure_metric(&mut failure_metrics, failure.code);
            lock_unpoisoned(&runtime).metrics = failure_metrics;
            telemetry.advance_epoch();
            match FallbackSession::start(&settings, &failure) {
                Ok(mut fallback) => {
                    fallback.publish(&runtime);
                    fallback_ready.store(true, Ordering::Release);
                    let _ = ready.send(Ok(()));
                    let initial_metrics = lock_unpoisoned(&runtime).metrics;
                    let exit = fallback.run(&stop, &mut telemetry, initial_metrics);
                    let _ = fallback.stop();
                    if !stop.load(Ordering::Acquire) {
                        let Some(message) = retain_failed_session_metrics(exit, &runtime) else {
                            return;
                        };
                        telemetry.advance_epoch();
                        lock_unpoisoned(&runtime)
                            .unavailable(PrimaryReasonCode::AudioDeviceFailure, message);
                    }
                }
                Err(fallback_failure) => {
                    let message = format!("{}; {}", failure.message, fallback_failure.message);
                    if fallback_failure.configuration_issue.is_some() {
                        if let Some(issue) = fallback_failure.configuration_issue {
                            lock_unpoisoned(&runtime).blocked(issue.clone());
                            let _ = ready.send(Err(EngineStartError::ConfigurationRequired(issue)));
                        } else {
                            lock_unpoisoned(&runtime).blocked_message(message.clone());
                            let _ = ready.send(Err(EngineStartError::Runtime(message)));
                        }
                    } else {
                        let mut reason = fallback_failure.primary_reason();
                        reason.detail = Some(message.clone());
                        lock_unpoisoned(&runtime).unavailable_reason(reason);
                        let _ = ready.send(Err(EngineStartError::Runtime(message)));
                    }
                }
            }
        }
    }
}

struct FallbackContext<'a> {
    stop: &'a AtomicBool,
    fallback_ready: &'a AtomicBool,
    telemetry: &'a mut TelemetryPublisher,
    runtime: &'a Mutex<RuntimeSnapshot>,
}

fn run_reused_fallback_after_processing(
    settings: &EngineSettings,
    reason: SessionStartFailure,
    session: RealtimeSession,
    context: FallbackContext<'_>,
) {
    context.telemetry.advance_epoch();
    match FallbackSession::from_realtime(settings, &reason, session) {
        Ok(fallback) => run_active_fallback(fallback, context),
        Err(_) => run_fallback_after_processing(settings, reason, context),
    }
}

fn run_fallback_after_processing(
    settings: &EngineSettings,
    reason: SessionStartFailure,
    context: FallbackContext<'_>,
) {
    context.telemetry.advance_epoch();
    match FallbackSession::start(settings, &reason) {
        Ok(fallback) => run_active_fallback(fallback, context),
        Err(failure) => publish_fallback_start_failure(failure, context.runtime),
    }
}

fn publish_fallback_start_failure(failure: SessionStartFailure, runtime: &Mutex<RuntimeSnapshot>) {
    if let Some(issue) = failure.configuration_issue.clone() {
        lock_unpoisoned(runtime).blocked(issue);
    } else {
        lock_unpoisoned(runtime).unavailable_reason(failure.primary_reason());
    }
}

fn run_active_fallback(mut fallback: FallbackSession, context: FallbackContext<'_>) {
    let FallbackContext {
        stop,
        fallback_ready,
        telemetry,
        runtime,
    } = context;
    fallback.publish(runtime);
    fallback_ready.store(true, Ordering::Release);
    let initial_metrics = lock_unpoisoned(runtime).metrics;
    let exit = fallback.run(stop, telemetry, initial_metrics);
    let _ = fallback.stop();
    if !stop.load(Ordering::Acquire) {
        let Some(message) = retain_failed_session_metrics(exit, runtime) else {
            return;
        };
        telemetry.advance_epoch();
        lock_unpoisoned(runtime).unavailable(PrimaryReasonCode::AudioDeviceFailure, message);
    }
}

fn retain_failed_session_metrics(
    exit: SessionExit,
    runtime: &Mutex<RuntimeSnapshot>,
) -> Option<String> {
    let SessionExit::Failed(failure, final_metrics) = exit else {
        return None;
    };
    lock_unpoisoned(runtime).metrics = final_metrics;
    Some(failure.message().to_owned())
}

#[derive(Debug, Clone)]
struct SessionStartFailure {
    code: PrimaryReasonCode,
    message: String,
    endpoint_id: Option<String>,
    configuration_issue: Option<ConfigurationIssue>,
}

impl SessionStartFailure {
    fn new(code: PrimaryReasonCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            endpoint_id: None,
            configuration_issue: None,
        }
    }

    fn waiting(
        code: PrimaryReasonCode,
        endpoint_id: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            endpoint_id,
            configuration_issue: None,
        }
    }

    fn configuration(message: impl Into<String>, issue: ConfigurationIssue) -> Self {
        let code = PrimaryReason::configuration(issue.clone()).code;
        Self {
            code,
            message: message.into(),
            endpoint_id: None,
            configuration_issue: Some(issue),
        }
    }

    fn primary_reason(&self) -> PrimaryReason {
        let mut reason = if let Some(issue) = self.configuration_issue.clone() {
            PrimaryReason::configuration(issue)
        } else if matches!(
            self.code,
            PrimaryReasonCode::WaitingForPlayback
                | PrimaryReasonCode::WaitingForCapture
                | PrimaryReasonCode::WaitingForVirtualOutput
        ) {
            PrimaryReason::waiting(self.code, self.endpoint_id.clone())
        } else {
            PrimaryReason::new(self.code)
        };
        reason.detail = Some(self.message.clone());
        reason
    }

    fn capture_unavailable_reason(&self, settings: &EngineSettings, detail: &str) -> PrimaryReason {
        let mut reason = PrimaryReason::waiting(
            PrimaryReasonCode::WaitingForCapture,
            settings.capture_endpoint_id.clone(),
        );
        reason.detail = Some(format!("{}; {detail}", self.message));
        reason
    }
}

enum AudioSource {
    Duplex(Box<DuplexSourceStream>),
    Capture(Box<CaptureSourceStream>),
}

impl AudioSource {
    fn capture_endpoint(&self) -> &EndpointDescriptor {
        match self {
            Self::Duplex(source) => &source.info().capture_endpoint,
            Self::Capture(source) => &source.info().capture_endpoint,
        }
    }

    fn render_endpoint(&self) -> Option<&EndpointDescriptor> {
        match self {
            Self::Duplex(source) => Some(&source.info().render_endpoint),
            Self::Capture(_) => None,
        }
    }

    fn render_channels(&self) -> usize {
        match self {
            Self::Duplex(source) => source.info().render_channels,
            Self::Capture(_) => 1,
        }
    }

    fn pop_capture(&mut self) -> Option<xmic_engine_core::CaptureFrame> {
        match self {
            Self::Duplex(source) => source.pop_capture(),
            Self::Capture(source) => source.pop_capture(),
        }
    }

    fn pop_render(&mut self) -> Option<RenderFrame> {
        match self {
            Self::Duplex(source) => source.pop_render(),
            Self::Capture(_) => None,
        }
    }

    fn capture_queue_blocks(&self) -> usize {
        match self {
            Self::Duplex(source) => source.capture_queue_blocks(),
            Self::Capture(source) => source.capture_queue_blocks(),
        }
    }

    fn counters(&self) -> SourceCountersSnapshot {
        match self {
            Self::Duplex(source) => source.counters(),
            Self::Capture(source) => source.counters(),
        }
    }

    fn worker_finished(&self) -> bool {
        match self {
            Self::Duplex(source) => source.worker_finished(),
            Self::Capture(source) => source.worker_finished(),
        }
    }

    fn capture_worker_finished(&self) -> bool {
        match self {
            Self::Duplex(source) => source.capture_worker_finished(),
            Self::Capture(source) => source.worker_finished(),
        }
    }

    fn discard_render_backlog(&mut self) {
        if let Self::Duplex(source) = self {
            source.discard_render_backlog();
        }
    }

    fn stop(self) -> Result<(), PlatformError> {
        match self {
            Self::Duplex(source) => source.stop(),
            Self::Capture(source) => source.stop(),
        }
    }
}

struct RealtimeSession {
    source: AudioSource,
    output: OutputSinkStream,
    processor: DspProcessor,
    processing_latency_frames: usize,
    route: ActiveRoute,
    aec_enabled: bool,
    vad_gate_enabled: bool,
    processing_histogram: ProcessingHistogram,
}

impl RealtimeSession {
    fn start(
        settings: &EngineSettings,
        native_library: &StartupDspLibrary,
    ) -> Result<Self, SessionStartFailure> {
        let native_library = match native_library {
            StartupDspLibrary::Valid(library) => Arc::clone(library),
            StartupDspLibrary::Invalid(message) => {
                return Err(SessionStartFailure::new(
                    PrimaryReasonCode::DspFailure,
                    message.clone(),
                ))
            }
        };
        let endpoint_snapshot_result = WasapiPlatform::new().active_endpoints();
        let endpoint_snapshot_available = endpoint_snapshot_result.is_ok();
        let endpoint_snapshot = endpoint_snapshot_result.unwrap_or_default();
        let route_wait = route_wait_reason(settings, &endpoint_snapshot);
        let source = if settings.dsp.aec.enabled {
            let render_endpoint_id = if settings.follow_default_render {
                None
            } else {
                settings.render_endpoint_id.clone()
            };
            AudioSource::Duplex(Box::new(
                start_duplex_source(DuplexSourceRequest {
                    render_endpoint_id,
                    capture_endpoint_id: settings.capture_endpoint_id.clone(),
                    capture_channel: settings.capture_channel,
                })
                .map_err(|error| {
                    SessionStartFailure::waiting(
                        route_wait.code,
                        route_wait.endpoint_id.clone(),
                        error.to_string(),
                    )
                })?,
            ))
        } else {
            AudioSource::Capture(Box::new(
                start_capture_source(CaptureSourceRequest {
                    capture_endpoint_id: settings.capture_endpoint_id.clone(),
                    capture_channel: settings.capture_channel,
                })
                .map_err(|error| {
                    SessionStartFailure::waiting(
                        PrimaryReasonCode::WaitingForCapture,
                        settings.capture_endpoint_id.clone(),
                        error.to_string(),
                    )
                })?,
            ))
        };
        let output_endpoint_id = settings.output_endpoint_id.clone().ok_or_else(|| {
            SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForVirtualOutput,
                None,
                "virtual microphone output endpoint is missing",
            )
        })?;
        let output = start_output_sink(OutputSinkRequest {
            endpoint_id: output_endpoint_id.clone(),
        })
        .map_err(|error| {
            SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForVirtualOutput,
                Some(output_endpoint_id),
                error.to_string(),
            )
        })?;

        if (!endpoint_snapshot_available && settings.virtual_capture_endpoint_id.is_some())
            || default_capture_identity_is_unverifiable(settings, &endpoint_snapshot)
        {
            return Err(SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForCapture,
                settings.capture_endpoint_id.clone(),
                "the default capture or virtual route is not currently verifiable",
            ));
        }
        let verification_snapshot = WasapiPlatform::new()
            .active_endpoints()
            .unwrap_or_else(|_| endpoint_snapshot.clone());
        validate_opened_virtual_output(
            settings,
            &output.info().endpoint,
            &verification_snapshot,
            "the opened virtual output no longer matches the configured device identity",
        )?;
        if !capture_matches_selected_physical(
            settings,
            source.capture_endpoint(),
            &verification_snapshot,
        ) {
            return Err(SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForCapture,
                settings.capture_endpoint_id.clone(),
                "the selected physical capture changed while the audio session was opening",
            ));
        }
        if capture_is_configured_virtual(
            settings,
            source.capture_endpoint(),
            &verification_snapshot,
        ) {
            return Err(SessionStartFailure::configuration(
                "the configured virtual capture cannot be used as the physical microphone",
                ConfigurationIssue::PhysicalCaptureRequired {
                    virtual_capture_endpoint_id: Some(source.capture_endpoint().id.clone()),
                },
            ));
        }

        match &source {
            AudioSource::Duplex(duplex) => validate_topology(
                duplex,
                &output.info().endpoint,
                settings.follow_default_render,
            ),
            AudioSource::Capture(capture) => validate_capture_output_topology(
                &capture.info().capture_endpoint,
                &output.info().endpoint,
            ),
        }
        .map_err(|error| {
            SessionStartFailure::configuration(
                error,
                opened_route_configuration_issue(settings, &source, &output.info().endpoint),
            )
        })?;

        let processor =
            DspProcessor::from_library(native_library, source.render_channels(), &settings.dsp)
                .map_err(|error| {
                    SessionStartFailure::new(PrimaryReasonCode::DspFailure, error.to_string())
                })?;
        let processing_latency_frames =
            processing_latency_frames(&settings.dsp).map_err(|error| {
                SessionStartFailure::new(PrimaryReasonCode::DspFailure, error.to_string())
            })?;
        let route = ActiveRoute {
            render_endpoint_id: source.render_endpoint().map(|endpoint| endpoint.id.clone()),
            capture_endpoint_id: Some(source.capture_endpoint().id.clone()),
            output_endpoint_id: output.info().endpoint.id.clone(),
        };
        Ok(Self {
            source,
            output,
            processor,
            processing_latency_frames,
            route,
            aec_enabled: settings.dsp.aec.enabled,
            vad_gate_enabled: settings.dsp.vad_gate.enabled,
            processing_histogram: ProcessingHistogram::default(),
        })
    }

    fn publish_initial(&self, runtime: &Mutex<RuntimeSnapshot>) {
        let (state, output, reason) = if self.aec_enabled {
            (
                EngineState::Degraded,
                EngineOutput::Silence,
                Some(PrimaryReason::new(PrimaryReasonCode::AecWarmup)),
            )
        } else {
            (EngineState::Processing, EngineOutput::Processed, None)
        };
        let metrics = lock_unpoisoned(runtime).metrics;
        lock_unpoisoned(runtime).publish(RuntimeTransition {
            state,
            output,
            primary_reason: reason,
            active_route: Some(self.route.clone()),
            metrics,
        });
    }

    fn run(
        &mut self,
        stop: &AtomicBool,
        fallback_requested: &AtomicBool,
        fallback_reason: &AtomicU8,
        telemetry: &mut TelemetryPublisher,
        initial_metrics: EngineMetrics,
    ) -> SessionExit {
        let started = Instant::now();
        let mut interleaved_render = [0.0; DSP_SAMPLES_PER_FRAME * MAX_RENDER_CHANNELS];
        let mut processed = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut metrics = initial_metrics;
        metrics.clear_session_gauges();
        metrics.processing_load_mode = ProcessingLoadMode::Full;
        let counter_baseline =
            MetricBaseline::new(&metrics, &self.source.counters(), &self.output.counters());
        let mut gate_was_open = false;
        let mut echo_guard_was_active = false;
        let mut nearend_candidate_was_active = false;
        let mut host_delay_estimator = HostDelayEstimator::default();
        let mut last_data_discontinuities = self.source.counters().data_discontinuities;
        let mut window = StatusWindow::new(Instant::now());
        let mut overload = ProcessingOverloadController::new();
        let mut echo_path_reset = EchoPathResetLimiter::default();
        let mut last_backlog_events = dsp_backlog_events(&self.source);
        let mut recovery_mute_until = None;
        let mut pending_render = None;

        loop {
            if stop.load(Ordering::Acquire) {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                telemetry.publish(RealtimeTelemetry {
                    state: EngineState::Processing,
                    output: EngineOutput::Processed,
                    reason: ReasonUpdate::Preserve,
                    metrics,
                });
                return SessionExit::Stopped;
            }
            if fallback_requested.swap(false, Ordering::AcqRel) {
                let reason = fallback_request_reason(fallback_reason);
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                return SessionExit::RequestedFallback(reason, metrics);
            }
            if self.source.worker_finished() || self.output.worker_finished() {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                return SessionExit::Failed(SessionFailure::WasapiWorker, metrics);
            }

            window.observe_backlog(self.source.capture_queue_blocks());
            let Some(capture) = self.source.pop_capture() else {
                thread::park_timeout(Duration::from_millis(20));
                continue;
            };
            let processing_started = Instant::now();
            let source_counters = self.source.counters();
            if counter_changed(
                &mut last_data_discontinuities,
                source_counters.data_discontinuities,
            ) && self.aec_enabled
            {
                host_delay_estimator.reset();
                metrics.host_delay_reset_events = metrics.host_delay_reset_events.saturating_add(1);
                echo_path_reset.request();
            }
            let input_peak_dbfs = peak_dbfs(&capture.samples);
            window.observe_input_peak(input_peak_dbfs);
            metrics.input_clipped_samples = metrics
                .input_clipped_samples
                .saturating_add(clipped_sample_count(&capture.samples));

            let mut latest_render_qpc_position_100ns = None;
            let mut latest_render_sample_count = 0usize;
            loop {
                let render = pending_render.take().or_else(|| self.source.pop_render());
                let Some(render) = render else { break };
                if render_is_after_capture(
                    render.timestamp.qpc_position_100ns,
                    capture.timestamp.qpc_position_100ns,
                ) {
                    pending_render = Some(render);
                    break;
                }
                interleave_render(&render, &mut interleaved_render);
                let sample_count = DSP_SAMPLES_PER_FRAME * render.channels;
                if let Err(error) = self
                    .processor
                    .process_render(&interleaved_render[..sample_count])
                {
                    self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                    return SessionExit::FallbackRequired {
                        failure: RealtimeFailure::ProcessRender(error),
                        metrics,
                    };
                }
                latest_render_qpc_position_100ns = Some(render.timestamp.qpc_position_100ns);
                latest_render_sample_count = sample_count;
            }
            if self.aec_enabled && latest_render_sample_count == 0 {
                metrics.render_underruns = metrics.render_underruns.saturating_add(1);
            }

            let mut host_delay_observation = host_delay_estimator.current();
            if self.aec_enabled {
                if let Some(render_qpc_position_100ns) = latest_render_qpc_position_100ns {
                    host_delay_observation = host_delay_estimator.observe(
                        capture.timestamp.qpc_position_100ns,
                        render_qpc_position_100ns,
                    );
                    if host_delay_observation.reset_required {
                        metrics.host_delay_reset_events =
                            metrics.host_delay_reset_events.saturating_add(1);
                        echo_path_reset.request();
                    }
                }
                if echo_path_reset.take_due(Instant::now()) {
                    if let Err(error) = self.processor.reset_echo_path() {
                        self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                        return SessionExit::FallbackRequired {
                            failure: RealtimeFailure::ResetEchoPath(error),
                            metrics,
                        };
                    }
                    if latest_render_sample_count > 0 {
                        if let Err(error) = self
                            .processor
                            .process_render(&interleaved_render[..latest_render_sample_count])
                        {
                            self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                            return SessionExit::FallbackRequired {
                                failure: RealtimeFailure::RenderAfterReset(error),
                                metrics,
                            };
                        }
                    }
                    recovery_mute_until = Some(Instant::now() + AEC_CONVERGENCE_TIME);
                }
            }
            update_host_delay_metrics(&mut metrics, host_delay_observation);

            let frame_metrics = match self.processor.process_capture(
                &capture.samples,
                &mut processed,
                host_delay_observation.stream_delay_ms,
            ) {
                Ok(frame_metrics) => frame_metrics,
                Err(error) => {
                    self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                    return SessionExit::FallbackRequired {
                        failure: RealtimeFailure::ProcessCapture(error),
                        metrics,
                    };
                }
            };
            metrics.limiter_limited_samples = metrics
                .limiter_limited_samples
                .saturating_add(u64::try_from(frame_metrics.limited_samples).unwrap_or(u64::MAX));
            metrics.vad_probability = Some(frame_metrics.vad_probability);
            metrics.speech_vad_active = frame_metrics.speech_vad_active;
            metrics.vad_gate_open = Some(frame_metrics.gate_open);
            metrics.aec_residual_echo_likelihood = frame_metrics.residual_echo_likelihood;
            metrics.aec_attenuation_db = frame_metrics.aec_attenuation_db;
            if self.vad_gate_enabled && frame_metrics.gate_open && !gate_was_open {
                metrics.vad_gate_open_events = metrics.vad_gate_open_events.saturating_add(1);
            }
            gate_was_open = frame_metrics.gate_open;
            if frame_metrics.aec_attenuation_db.is_some() {
                metrics.echo_guard_active = Some(frame_metrics.echo_guard_active);
                if frame_metrics.echo_guard_active && !echo_guard_was_active {
                    metrics.echo_guard_active_events =
                        metrics.echo_guard_active_events.saturating_add(1);
                }
                if frame_metrics.echo_guard_nearend_candidate && !nearend_candidate_was_active {
                    metrics.echo_guard_nearend_candidate_events = metrics
                        .echo_guard_nearend_candidate_events
                        .saturating_add(1);
                }
                if frame_metrics.echo_guard_released {
                    metrics.echo_guard_release_events =
                        metrics.echo_guard_release_events.saturating_add(1);
                }
                echo_guard_was_active = frame_metrics.echo_guard_active;
                nearend_candidate_was_active = frame_metrics.echo_guard_nearend_candidate;
            } else {
                metrics.echo_guard_active = None;
                echo_guard_was_active = false;
                nearend_candidate_was_active = false;
            }

            let now = Instant::now();
            let initial_warmup = self.aec_enabled && started.elapsed() < AEC_CONVERGENCE_TIME;
            let rebuild_warmup = recovery_mute_until.is_some_and(|until| now < until);
            if !rebuild_warmup {
                recovery_mute_until = None;
            }
            let (state, output_kind, reason, output) = if rebuild_warmup {
                (
                    EngineState::Degraded,
                    EngineOutput::Silence,
                    Some(PrimaryReason::new(PrimaryReasonCode::AecRebuild)),
                    [0.0; DSP_SAMPLES_PER_FRAME],
                )
            } else if initial_warmup {
                (
                    EngineState::Degraded,
                    EngineOutput::Silence,
                    Some(PrimaryReason::new(PrimaryReasonCode::AecWarmup)),
                    [0.0; DSP_SAMPLES_PER_FRAME],
                )
            } else {
                (
                    EngineState::Processing,
                    EngineOutput::Processed,
                    None,
                    processed,
                )
            };
            window.observe_output_peak(peak_dbfs(&output));
            if !self.output.push_block(output) {
                metrics.output_queue_overflows = metrics
                    .output_queue_overflows
                    .max(self.output.counters().queue_overflows);
            }
            metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);

            let processing_elapsed = processing_started.elapsed();
            self.processing_histogram.observe(processing_elapsed);
            if processing_elapsed.as_micros() > u128::from(PROCESSING_BUDGET_US) {
                metrics.processing_budget_exceeded_blocks =
                    metrics.processing_budget_exceeded_blocks.saturating_add(1);
            }
            let current_backlog_events = dsp_backlog_events(&self.source);
            let backlog_fault = current_backlog_events > last_backlog_events
                && started.elapsed() >= AEC_CONVERGENCE_TIME;
            last_backlog_events = current_backlog_events;
            if overload.observe(processing_elapsed, backlog_fault) {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                return SessionExit::FallbackRequired {
                    failure: RealtimeFailure::CpuOverload,
                    metrics,
                };
            }

            if window.due(Instant::now()) {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                telemetry.publish(RealtimeTelemetry {
                    state,
                    output: output_kind,
                    reason: match reason {
                        Some(reason) => ReasonUpdate::Code(reason.code),
                        None => ReasonUpdate::Clear,
                    },
                    metrics,
                });
            }
        }
    }

    fn refresh_metrics(
        &mut self,
        metrics: &mut EngineMetrics,
        counter_baseline: &MetricBaseline,
        window: &mut StatusWindow,
    ) {
        let source_counters = self.source.counters();
        let output_counters = self.output.counters();
        apply_device_metrics(
            metrics,
            &source_counters,
            &output_counters,
            self.output.info().stream_latency_100ns,
            self.processing_latency_frames,
            counter_baseline,
            window,
        );
        metrics.input_peak_dbfs = window.take_input_peak();
        metrics.output_peak_dbfs = window.take_output_peak();
        apply_processing_time_metrics(metrics, &self.processing_histogram, counter_baseline);
        if self.aec_enabled {
            match self.processor.aec_metrics() {
                Ok(Some(aec)) => {
                    metrics.aec_estimated_delay_ms = aec.estimated_delay_ms;
                    metrics.aec_echo_return_loss_db = aec.echo_return_loss_db;
                    metrics.aec_echo_return_loss_enhancement_db =
                        aec.echo_return_loss_enhancement_db;
                    metrics.aec_residual_echo_likelihood = aec.residual_echo_likelihood;
                }
                Ok(None) | Err(_) => clear_aec_metrics(metrics),
            }
        } else {
            clear_aec_metrics(metrics);
        }
    }

    fn stop(self) -> Result<(), PlatformError> {
        let source_result = self.source.stop();
        let output_result = self.output.stop();
        source_result.and(output_result)
    }
}

fn validate_opened_virtual_output(
    settings: &EngineSettings,
    output: &EndpointDescriptor,
    endpoints: &[EndpointDescriptor],
    detail: &str,
) -> Result<(), SessionStartFailure> {
    match opened_virtual_output_issue(settings, output, endpoints) {
        Some(issue) => Err(SessionStartFailure::configuration(detail, issue)),
        None => Ok(()),
    }
}

fn capture_matches_selected_physical(
    settings: &EngineSettings,
    capture: &EndpointDescriptor,
    endpoints: &[EndpointDescriptor],
) -> bool {
    if let Some(configured_id) = settings.capture_endpoint_id.as_deref() {
        return capture.id == configured_id;
    }
    default_multimedia_capture(endpoints)
        .is_some_and(|selected| selected.id == capture.id || same_endpoint_group(selected, capture))
}

fn capture_is_configured_virtual(
    settings: &EngineSettings,
    capture: &EndpointDescriptor,
    endpoints: &[EndpointDescriptor],
) -> bool {
    if is_standard_vb_cable_capture(capture) {
        return true;
    }
    let Some(configured_id) = settings.virtual_capture_endpoint_id.as_deref() else {
        return false;
    };
    if configured_id == capture.id
        || settings.trusted_virtual_audio_pairs.iter().any(|pair| {
            pair.capture_endpoint_id == configured_id
                && pair
                    .capture_name
                    .trim()
                    .eq_ignore_ascii_case(capture.friendly_name.trim())
        })
    {
        return true;
    }
    endpoints
        .iter()
        .find(|endpoint| endpoint.flow == DataFlow::Capture && endpoint.id == configured_id)
        .is_some_and(|configured| same_endpoint_group(capture, configured))
}

fn same_endpoint_group(left: &EndpointDescriptor, right: &EndpointDescriptor) -> bool {
    left.group_id
        .as_deref()
        .zip(right.group_id.as_deref())
        .is_some_and(|(left, right)| !left.is_empty() && left == right)
}

fn default_capture_identity_is_unverifiable(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    settings.capture_endpoint_id.is_none()
        && settings.virtual_capture_endpoint_id.is_some()
        && !endpoints.iter().any(|endpoint| {
            endpoint.flow == DataFlow::Capture
                && Some(endpoint.id.as_str()) == settings.virtual_capture_endpoint_id.as_deref()
        })
}

fn capture_output_configuration_issue(
    settings: &EngineSettings,
    capture: &EndpointDescriptor,
    output: &EndpointDescriptor,
) -> Option<ConfigurationIssue> {
    if capture.id == output.id || same_endpoint_group(capture, output) {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some(capture.id.clone()),
        });
    }
    if capture_is_configured_virtual(settings, capture, &[]) {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some(capture.id.clone()),
        });
    }
    None
}

fn opened_route_configuration_issue(
    settings: &EngineSettings,
    source: &AudioSource,
    output: &EndpointDescriptor,
) -> ConfigurationIssue {
    let capture = source.capture_endpoint();
    if let Some(issue) = capture_output_configuration_issue(settings, capture, output) {
        return issue;
    }
    if let Some(render) = source.render_endpoint() {
        if render.id == output.id || same_endpoint_group(render, output) {
            return ConfigurationIssue::VirtualRouteConflict {
                kind: if settings.follow_default_render {
                    xmic_engine_protocol::VirtualRouteConflictKind::DefaultRender
                } else {
                    xmic_engine_protocol::VirtualRouteConflictKind::FixedRender
                },
                endpoint_id: render.id.clone(),
            };
        }
    }
    ConfigurationIssue::PhysicalCaptureRequired {
        virtual_capture_endpoint_id: settings.virtual_capture_endpoint_id.clone(),
    }
}

struct FallbackSession {
    source: Option<AudioSource>,
    output: OutputSinkStream,
    output_kind: EngineOutput,
    reason: PrimaryReason,
    route: ActiveRoute,
}

impl FallbackSession {
    fn from_realtime(
        settings: &EngineSettings,
        failure: &SessionStartFailure,
        session: RealtimeSession,
    ) -> Result<Self, SessionStartFailure> {
        let RealtimeSession { source, output, .. } = session;
        let output_endpoint_id = output.info().endpoint.id.clone();
        let endpoint_snapshot = WasapiPlatform::new().active_endpoints().map_err(|error| {
            SessionStartFailure::new(
                PrimaryReasonCode::AudioDeviceFailure,
                format!("verify reused fallback route: {error}"),
            )
        })?;
        validate_opened_virtual_output(
            settings,
            &output.info().endpoint,
            &endpoint_snapshot,
            "the reused virtual output no longer matches the configured device identity",
        )?;
        if settings.degraded_output == DegradedOutput::Raw {
            if !capture_matches_selected_physical(
                settings,
                source.capture_endpoint(),
                &endpoint_snapshot,
            ) || capture_is_configured_virtual(
                settings,
                source.capture_endpoint(),
                &endpoint_snapshot,
            ) {
                return Err(SessionStartFailure::configuration(
                    "the reused processing capture is no longer a safe physical microphone",
                    ConfigurationIssue::PhysicalCaptureRequired {
                        virtual_capture_endpoint_id: Some(source.capture_endpoint().id.clone()),
                    },
                ));
            }
            validate_capture_output_topology(source.capture_endpoint(), &output.info().endpoint)
                .map_err(|error| {
                    SessionStartFailure::configuration(
                        error,
                        capture_output_configuration_issue(
                            settings,
                            source.capture_endpoint(),
                            &output.info().endpoint,
                        )
                        .unwrap_or_else(|| {
                            ConfigurationIssue::PhysicalCaptureRequired {
                                virtual_capture_endpoint_id: Some(
                                    source.capture_endpoint().id.clone(),
                                ),
                            }
                        }),
                    )
                })?;
            let capture_endpoint_id = source.capture_endpoint().id.clone();
            return Ok(Self {
                source: Some(source),
                output,
                output_kind: EngineOutput::Raw,
                reason: failure.primary_reason(),
                route: ActiveRoute {
                    render_endpoint_id: None,
                    capture_endpoint_id: Some(capture_endpoint_id),
                    output_endpoint_id,
                },
            });
        }

        source.stop().map_err(|error| {
            SessionStartFailure::new(
                PrimaryReasonCode::AudioDeviceFailure,
                format!("stop processing source before silence fallback: {error}"),
            )
        })?;
        Ok(Self {
            source: None,
            output,
            output_kind: EngineOutput::Silence,
            reason: failure.primary_reason(),
            route: ActiveRoute {
                render_endpoint_id: None,
                capture_endpoint_id: None,
                output_endpoint_id,
            },
        })
    }

    fn start(
        settings: &EngineSettings,
        failure: &SessionStartFailure,
    ) -> Result<Self, SessionStartFailure> {
        let output_endpoint_id = settings.output_endpoint_id.clone().ok_or_else(|| {
            SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForVirtualOutput,
                None,
                "virtual output is unavailable, so no safe fallback exists",
            )
        })?;
        let output = start_output_sink(OutputSinkRequest {
            endpoint_id: output_endpoint_id.clone(),
        })
        .map_err(|error| {
            SessionStartFailure::waiting(
                PrimaryReasonCode::WaitingForVirtualOutput,
                Some(output_endpoint_id),
                format!("start degraded fallback output: {error}"),
            )
        })?;
        let endpoint_snapshot_result = WasapiPlatform::new().active_endpoints();
        let endpoint_snapshot_available = endpoint_snapshot_result.is_ok();
        let endpoint_snapshot = endpoint_snapshot_result.unwrap_or_default();
        validate_opened_virtual_output(
            settings,
            &output.info().endpoint,
            &endpoint_snapshot,
            "the opened fallback output no longer matches the configured device identity",
        )?;

        let mut reason = failure.primary_reason();
        if settings.degraded_output == DegradedOutput::Raw {
            if (!endpoint_snapshot_available && settings.virtual_capture_endpoint_id.is_some())
                || default_capture_identity_is_unverifiable(settings, &endpoint_snapshot)
            {
                return Ok(Self {
                    source: None,
                    route: ActiveRoute {
                        render_endpoint_id: None,
                        capture_endpoint_id: None,
                        output_endpoint_id: output.info().endpoint.id.clone(),
                    },
                    output,
                    output_kind: EngineOutput::Silence,
                    reason: failure.capture_unavailable_reason(
                        settings,
                        "capture identity could not be verified",
                    ),
                });
            }
            match start_capture_source(CaptureSourceRequest {
                capture_endpoint_id: settings.capture_endpoint_id.clone(),
                capture_channel: settings.capture_channel,
            }) {
                Ok(source) => {
                    let verification_snapshot = WasapiPlatform::new()
                        .active_endpoints()
                        .unwrap_or_else(|_| endpoint_snapshot.clone());
                    if !capture_matches_selected_physical(
                        settings,
                        &source.info().capture_endpoint,
                        &verification_snapshot,
                    ) {
                        let _ = source.stop();
                        reason = failure.capture_unavailable_reason(
                            settings,
                            "the opened capture no longer matches the selected physical microphone",
                        );
                    } else {
                        if capture_is_configured_virtual(
                            settings,
                            &source.info().capture_endpoint,
                            &verification_snapshot,
                        ) {
                            let capture_endpoint_id = source.info().capture_endpoint.id.clone();
                            let _ = source.stop();
                            return Err(SessionStartFailure::configuration(
                            "the configured virtual capture cannot be used as the physical microphone",
                            ConfigurationIssue::PhysicalCaptureRequired {
                                virtual_capture_endpoint_id: Some(capture_endpoint_id),
                            },
                        ));
                        }
                        if let Err(error) = validate_capture_output_topology(
                            &source.info().capture_endpoint,
                            &output.info().endpoint,
                        ) {
                            let issue = capture_output_configuration_issue(
                                settings,
                                &source.info().capture_endpoint,
                                &output.info().endpoint,
                            )
                            .unwrap_or_else(|| {
                                ConfigurationIssue::PhysicalCaptureRequired {
                                    virtual_capture_endpoint_id: settings
                                        .virtual_capture_endpoint_id
                                        .clone(),
                                }
                            });
                            let _ = source.stop();
                            return Err(SessionStartFailure::configuration(error, issue));
                        }
                        let route = ActiveRoute {
                            render_endpoint_id: None,
                            capture_endpoint_id: Some(source.info().capture_endpoint.id.clone()),
                            output_endpoint_id: output.info().endpoint.id.clone(),
                        };
                        return Ok(Self {
                            source: Some(AudioSource::Capture(Box::new(source))),
                            output,
                            output_kind: EngineOutput::Raw,
                            reason: failure.primary_reason(),
                            route,
                        });
                    }
                }
                Err(error) => {
                    reason = failure.capture_unavailable_reason(
                        settings,
                        &format!("start raw fallback capture: {error}"),
                    );
                }
            }
        }

        Ok(Self {
            source: None,
            route: ActiveRoute {
                render_endpoint_id: None,
                capture_endpoint_id: None,
                output_endpoint_id: output.info().endpoint.id.clone(),
            },
            output,
            output_kind: EngineOutput::Silence,
            reason,
        })
    }

    fn publish(&self, runtime: &Mutex<RuntimeSnapshot>) {
        let mut metrics = lock_unpoisoned(runtime).metrics;
        metrics.clear_session_gauges();
        metrics.processing_load_mode = if self.reason.code == PrimaryReasonCode::CpuOverload {
            ProcessingLoadMode::Protection
        } else {
            ProcessingLoadMode::Full
        };
        lock_unpoisoned(runtime).publish(RuntimeTransition {
            state: EngineState::Degraded,
            output: self.output_kind,
            primary_reason: Some(self.reason.clone()),
            active_route: Some(self.route.clone()),
            metrics,
        });
    }

    fn run(
        &mut self,
        stop: &AtomicBool,
        telemetry: &mut TelemetryPublisher,
        initial_metrics: EngineMetrics,
    ) -> SessionExit {
        let mut metrics = initial_metrics;
        metrics.clear_session_gauges();
        metrics.processing_load_mode = if self.reason.code == PrimaryReasonCode::CpuOverload {
            ProcessingLoadMode::Protection
        } else {
            ProcessingLoadMode::Full
        };
        let initial_source = self
            .source
            .as_ref()
            .map(AudioSource::counters)
            .unwrap_or_default();
        let initial_output = self.output.counters();
        let counter_baseline = MetricBaseline::new(&metrics, &initial_source, &initial_output);
        let mut window = StatusWindow::new(Instant::now());
        let mut next_silence = Instant::now();
        loop {
            if stop.load(Ordering::Acquire) {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                telemetry.publish(RealtimeTelemetry {
                    state: EngineState::Degraded,
                    output: self.output_kind,
                    reason: ReasonUpdate::Preserve,
                    metrics,
                });
                return SessionExit::Stopped;
            }
            if self.output.worker_finished()
                || self
                    .source
                    .as_ref()
                    .is_some_and(AudioSource::capture_worker_finished)
            {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                return SessionExit::Failed(SessionFailure::FallbackWorker, metrics);
            }

            if let Some(source) = &mut self.source {
                source.discard_render_backlog();
                window.observe_backlog(source.capture_queue_blocks());
                let Some(capture) = source.pop_capture() else {
                    thread::park_timeout(Duration::from_millis(20));
                    continue;
                };
                let input_peak = peak_dbfs(&capture.samples);
                window.observe_input_peak(input_peak);
                window.observe_output_peak(input_peak);
                metrics.input_clipped_samples = metrics
                    .input_clipped_samples
                    .saturating_add(clipped_sample_count(&capture.samples));
                // Raw fallback is sample-transparent and deliberately bypasses every DSP stage,
                // including the limiter and any latency-alignment path.
                let _ = self.output.push_block(capture.samples);
                metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);
            } else {
                let now = Instant::now();
                if now < next_silence {
                    thread::park_timeout((next_silence - now).min(Duration::from_millis(10)));
                    continue;
                }
                let silence = [0.0; DSP_SAMPLES_PER_FRAME];
                let _ = self.output.push_block(silence);
                window.observe_output_peak(-120.0);
                metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);
                next_silence = now + Duration::from_millis(DSP_FRAME_DURATION_MS as u64);
            }

            if window.due(Instant::now()) {
                self.refresh_metrics(&mut metrics, &counter_baseline, &mut window);
                telemetry.publish(RealtimeTelemetry {
                    state: EngineState::Degraded,
                    output: self.output_kind,
                    reason: ReasonUpdate::Preserve,
                    metrics,
                });
            }
        }
    }

    fn refresh_metrics(
        &mut self,
        metrics: &mut EngineMetrics,
        counter_baseline: &MetricBaseline,
        window: &mut StatusWindow,
    ) {
        let source_counters = self
            .source
            .as_ref()
            .map(AudioSource::counters)
            .unwrap_or_default();
        let output_counters = self.output.counters();
        apply_device_metrics(
            metrics,
            &source_counters,
            &output_counters,
            self.output.info().stream_latency_100ns,
            0,
            counter_baseline,
            window,
        );
        metrics.input_peak_dbfs = window.take_input_peak();
        metrics.output_peak_dbfs = window.take_output_peak();
        clear_dsp_metrics(metrics);
    }

    fn stop(self) -> Result<(), PlatformError> {
        let source_result = self.source.map_or(Ok(()), AudioSource::stop);
        let output_result = self.output.stop();
        source_result.and(output_result)
    }
}

enum SessionExit {
    RequestedFallback(PrimaryReasonCode, EngineMetrics),
    FallbackRequired {
        failure: RealtimeFailure,
        metrics: EngineMetrics,
    },
    Failed(SessionFailure, EngineMetrics),
    Stopped,
}

enum RealtimeFailure {
    ProcessRender(DspProcessorError),
    ResetEchoPath(DspProcessorError),
    RenderAfterReset(DspProcessorError),
    ProcessCapture(DspProcessorError),
    CpuOverload,
}

impl RealtimeFailure {
    fn into_start_failure(self) -> SessionStartFailure {
        match self {
            Self::ProcessRender(error) => SessionStartFailure::new(
                PrimaryReasonCode::DspFailure,
                format!("DSP render processing failed: {error}"),
            ),
            Self::ResetEchoPath(error) => SessionStartFailure::new(
                PrimaryReasonCode::DspFailure,
                format!("DSP echo path rebuild failed: {error}"),
            ),
            Self::RenderAfterReset(error) => SessionStartFailure::new(
                PrimaryReasonCode::DspFailure,
                format!("DSP render after echo rebuild failed: {error}"),
            ),
            Self::ProcessCapture(error) => SessionStartFailure::new(
                PrimaryReasonCode::DspFailure,
                format!("DSP capture processing failed: {error}"),
            ),
            Self::CpuOverload => SessionStartFailure::new(
                PrimaryReasonCode::CpuOverload,
                "realtime processing could not meet its deadline",
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SessionFailure {
    WasapiWorker,
    FallbackWorker,
}

impl SessionFailure {
    const fn message(self) -> &'static str {
        match self {
            Self::WasapiWorker => "a WASAPI worker stopped unexpectedly",
            Self::FallbackWorker => "a fallback WASAPI worker stopped",
        }
    }
}

fn note_failure_metric(metrics: &mut EngineMetrics, code: PrimaryReasonCode) {
    match code {
        PrimaryReasonCode::DspFailure => {
            metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
        }
        PrimaryReasonCode::CpuOverload => {
            metrics.processing_overload_events =
                metrics.processing_overload_events.saturating_add(1);
            metrics.processing_load_mode = ProcessingLoadMode::Protection;
        }
        _ => {}
    }
}

fn control_fallback_reuses_processing_resources(reason: PrimaryReasonCode) -> bool {
    reason == PrimaryReasonCode::Starting
}

const FALLBACK_REASON_STARTING: u8 = 0;
const FALLBACK_REASON_PLAYBACK: u8 = 1;
const FALLBACK_REASON_CAPTURE: u8 = 2;
const FALLBACK_REASON_VIRTUAL_OUTPUT: u8 = 3;
const FALLBACK_REASON_AUDIO_DEVICE: u8 = 4;

fn encode_fallback_reason(reason: PrimaryReasonCode) -> u8 {
    match reason {
        PrimaryReasonCode::Starting => FALLBACK_REASON_STARTING,
        PrimaryReasonCode::WaitingForPlayback => FALLBACK_REASON_PLAYBACK,
        PrimaryReasonCode::WaitingForCapture => FALLBACK_REASON_CAPTURE,
        PrimaryReasonCode::WaitingForVirtualOutput => FALLBACK_REASON_VIRTUAL_OUTPUT,
        _ => FALLBACK_REASON_AUDIO_DEVICE,
    }
}

fn fallback_request_reason(reason: &AtomicU8) -> PrimaryReasonCode {
    match reason.load(Ordering::Acquire) {
        FALLBACK_REASON_STARTING => PrimaryReasonCode::Starting,
        FALLBACK_REASON_PLAYBACK => PrimaryReasonCode::WaitingForPlayback,
        FALLBACK_REASON_CAPTURE => PrimaryReasonCode::WaitingForCapture,
        FALLBACK_REASON_VIRTUAL_OUTPUT => PrimaryReasonCode::WaitingForVirtualOutput,
        _ => PrimaryReasonCode::AudioDeviceFailure,
    }
}

fn render_is_after_capture(render_qpc_100ns: u64, capture_qpc_100ns: u64) -> bool {
    render_qpc_100ns != 0 && capture_qpc_100ns != 0 && render_qpc_100ns > capture_qpc_100ns
}

fn interleave_render(
    frame: &RenderFrame,
    output: &mut [f32; DSP_SAMPLES_PER_FRAME * MAX_RENDER_CHANNELS],
) {
    for sample_index in 0..DSP_SAMPLES_PER_FRAME {
        for channel in 0..frame.channels {
            output[sample_index * frame.channels + channel] = frame.samples[channel][sample_index];
        }
    }
}

fn update_host_delay_metrics(metrics: &mut EngineMetrics, observation: HostDelayObservation) {
    let has_estimate = observation.state != CoreDelayCalibrationState::Uncalibrated;
    metrics.host_delay_ms = has_estimate.then_some(observation.stream_delay_ms);
    metrics.host_delay_raw_skew_ms = observation.raw_skew_ms;
    metrics.host_delay_jitter_ms = has_estimate.then_some(observation.jitter_ms);
    metrics.host_delay_calibration_state = match observation.state {
        CoreDelayCalibrationState::Uncalibrated => ProtocolDelayCalibrationState::Uncalibrated,
        CoreDelayCalibrationState::WarmingUp => ProtocolDelayCalibrationState::WarmingUp,
        CoreDelayCalibrationState::Calibrated => ProtocolDelayCalibrationState::Calibrated,
    };
}

fn counter_changed(previous: &mut u64, current: u64) -> bool {
    if *previous == current {
        false
    } else {
        *previous = current;
        true
    }
}

fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .map(f32::abs)
        .fold(0.0_f32, f32::max);
    if peak > 0.0 {
        (20.0 * peak.log10()).clamp(-120.0, 0.0)
    } else {
        -120.0
    }
}

fn max_level(current: Option<f32>, next: f32) -> Option<f32> {
    Some(current.map_or(next, |current| current.max(next)))
}

fn clipped_sample_count(samples: &[f32]) -> u64 {
    samples
        .iter()
        .filter(|sample| sample.is_finite() && sample.abs() >= 0.999)
        .count()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn dsp_backlog_events(source: &AudioSource) -> u64 {
    let source = source.counters();
    source
        .render_overflows
        .saturating_add(source.capture_overflows)
}

#[derive(Debug, Default)]
struct EchoPathResetLimiter {
    pending: bool,
    last_reset: Option<Instant>,
}

impl EchoPathResetLimiter {
    fn request(&mut self) {
        self.pending = true;
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if !self.pending {
            return false;
        }
        if self
            .last_reset
            .is_some_and(|last| now.duration_since(last) < ECHO_PATH_RESET_MIN_INTERVAL)
        {
            return false;
        }
        self.pending = false;
        self.last_reset = Some(now);
        true
    }
}

#[derive(Debug)]
struct StatusWindow {
    last_update: Instant,
    input_peak_dbfs: Option<f32>,
    output_peak_dbfs: Option<f32>,
    backlog_blocks: usize,
}

impl StatusWindow {
    fn new(now: Instant) -> Self {
        Self {
            last_update: now,
            input_peak_dbfs: None,
            output_peak_dbfs: None,
            backlog_blocks: 0,
        }
    }

    fn observe_input_peak(&mut self, peak_dbfs: f32) {
        self.input_peak_dbfs = max_level(self.input_peak_dbfs, peak_dbfs);
    }

    fn observe_output_peak(&mut self, peak_dbfs: f32) {
        self.output_peak_dbfs = max_level(self.output_peak_dbfs, peak_dbfs);
    }

    fn observe_backlog(&mut self, blocks: usize) {
        self.backlog_blocks = self.backlog_blocks.max(blocks);
    }

    fn due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_update) < STATUS_UPDATE_INTERVAL {
            return false;
        }
        self.last_update = now;
        true
    }

    fn take_input_peak(&mut self) -> Option<f32> {
        self.input_peak_dbfs.take()
    }

    fn take_output_peak(&mut self) -> Option<f32> {
        self.output_peak_dbfs.take()
    }

    fn take_backlog_blocks(&mut self) -> usize {
        std::mem::take(&mut self.backlog_blocks)
    }
}

#[derive(Debug, Clone, Copy)]
struct MetricBaseline {
    prior_render_overflows: u64,
    prior_capture_overflows: u64,
    prior_device_discontinuities: u64,
    prior_output_underruns: u64,
    prior_output_queue_overflows: u64,
    prior_processing_deadline_misses: u64,
    initial_render_overflows: u64,
    initial_capture_overflows: u64,
    initial_device_discontinuities: u64,
    initial_output_underruns: u64,
    initial_output_queue_overflows: u64,
}

impl MetricBaseline {
    fn new(
        metrics: &EngineMetrics,
        source: &SourceCountersSnapshot,
        output: &OutputCountersSnapshot,
    ) -> Self {
        Self {
            prior_render_overflows: metrics.render_overflows,
            prior_capture_overflows: metrics.capture_overflows,
            prior_device_discontinuities: metrics.device_discontinuities,
            prior_output_underruns: metrics.output_underruns,
            prior_output_queue_overflows: metrics.output_queue_overflows,
            prior_processing_deadline_misses: metrics.processing_deadline_misses,
            initial_render_overflows: source.render_overflows,
            initial_capture_overflows: source.capture_overflows,
            initial_device_discontinuities: source.data_discontinuities,
            initial_output_underruns: output.underrun_events,
            initial_output_queue_overflows: output.queue_overflows,
        }
    }
}

fn apply_processing_time_metrics(
    metrics: &mut EngineMetrics,
    histogram: &ProcessingHistogram,
    baseline: &MetricBaseline,
) {
    metrics.processing_time_p99_us = histogram.p99_us();
    metrics.processing_time_max_us = histogram.max_us();
    metrics.processing_deadline_misses = baseline
        .prior_processing_deadline_misses
        .saturating_add(histogram.deadline_misses());
}

fn apply_device_metrics(
    metrics: &mut EngineMetrics,
    source: &SourceCountersSnapshot,
    output: &OutputCountersSnapshot,
    output_stream_latency_100ns: u64,
    processing_latency_frames: usize,
    baseline: &MetricBaseline,
    window: &mut StatusWindow,
) {
    metrics.render_overflows = baseline.prior_render_overflows.saturating_add(
        source
            .render_overflows
            .saturating_sub(baseline.initial_render_overflows),
    );
    metrics.capture_overflows = baseline.prior_capture_overflows.saturating_add(
        source
            .capture_overflows
            .saturating_sub(baseline.initial_capture_overflows),
    );
    metrics.device_discontinuities = baseline.prior_device_discontinuities.saturating_add(
        source
            .data_discontinuities
            .saturating_sub(baseline.initial_device_discontinuities),
    );
    metrics.output_underruns = baseline.prior_output_underruns.saturating_add(
        output
            .underrun_events
            .saturating_sub(baseline.initial_output_underruns),
    );
    metrics.output_queue_overflows = baseline.prior_output_queue_overflows.saturating_add(
        output
            .queue_overflows
            .saturating_sub(baseline.initial_output_queue_overflows),
    );
    metrics.output_queue_depth_ms =
        output.queue_depth_frames as f32 * 1_000.0 / xmic_engine_core::DSP_SAMPLE_RATE_HZ as f32;
    metrics.output_rate_adjustment_ppm = output.rate_adjustment_ppm as f32;
    latency_breakdown(
        processing_latency_frames,
        source.capture_packet_lag_100ns,
        window.take_backlog_blocks(),
        output,
        output_stream_latency_100ns,
    )
    .apply(metrics);
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LatencyBreakdown {
    capture_ms: f32,
    dsp_ms: f32,
    output_ms: f32,
}

impl LatencyBreakdown {
    fn total_ms(&self) -> f32 {
        self.capture_ms + self.dsp_ms + self.output_ms
    }

    fn apply(&self, metrics: &mut EngineMetrics) {
        metrics.capture_latency_ms = self.capture_ms;
        metrics.dsp_latency_ms = self.dsp_ms;
        metrics.output_latency_ms = self.output_ms;
        metrics.pipeline_latency_ms = self.total_ms();
    }
}

fn latency_breakdown(
    processing_latency_frames: usize,
    capture_packet_lag_100ns: u64,
    capture_backlog_blocks: usize,
    output: &OutputCountersSnapshot,
    output_stream_latency_100ns: u64,
) -> LatencyBreakdown {
    let backlog_ms = capture_backlog_blocks.saturating_sub(1) as f32 * DSP_FRAME_DURATION_MS as f32;
    LatencyBreakdown {
        capture_ms: capture_packet_lag_100ns as f32 / QPC_UNITS_PER_MILLISECOND + backlog_ms,
        dsp_ms: processing_latency_frames as f32 * DSP_FRAME_DURATION_MS as f32,
        output_ms: frames_to_ms(output.queue_depth_frames)
            + frames_to_ms(output.endpoint_padding_frames)
            + output_stream_latency_100ns as f32 / QPC_UNITS_PER_MILLISECOND,
    }
}

fn frames_to_ms(frames: u64) -> f32 {
    frames as f32 * 1_000.0 / xmic_engine_core::DSP_SAMPLE_RATE_HZ as f32
}

fn clear_aec_metrics(metrics: &mut EngineMetrics) {
    metrics.aec_estimated_delay_ms = None;
    metrics.aec_echo_return_loss_db = None;
    metrics.aec_echo_return_loss_enhancement_db = None;
    metrics.aec_residual_echo_likelihood = None;
    metrics.aec_attenuation_db = None;
}

fn clear_dsp_metrics(metrics: &mut EngineMetrics) {
    clear_aec_metrics(metrics);
    metrics.host_delay_ms = None;
    metrics.host_delay_raw_skew_ms = None;
    metrics.host_delay_jitter_ms = None;
    metrics.host_delay_calibration_state = ProtocolDelayCalibrationState::Uncalibrated;
    metrics.vad_probability = None;
    metrics.speech_vad_active = None;
    metrics.vad_gate_open = None;
    metrics.echo_guard_active = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_fake_processing_owner(controller: &RealtimeEngineController) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let fallback_requested = Arc::new(AtomicBool::new(false));
        let fallback_ready = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_requested = Arc::clone(&fallback_requested);
        let thread_ready = Arc::clone(&fallback_ready);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                if thread_requested.swap(false, Ordering::AcqRel) {
                    thread_ready.store(true, Ordering::Release);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        let (_, telemetry) = telemetry_channel();
        lock_unpoisoned(&controller.control).session = Some(TrackedSession {
            kind: SessionKind::Processing,
            stop,
            processing: Some(ProcessingSessionControl {
                fallback_requested,
                fallback_reason: Arc::new(AtomicU8::new(encode_fallback_reason(
                    PrimaryReasonCode::Starting,
                ))),
                fallback_ready: Arc::clone(&fallback_ready),
                telemetry,
            }),
            handle,
        });
        fallback_ready
    }

    #[test]
    fn raw_capture_failure_reports_a_retryable_resource_reason() {
        let settings = EngineSettings {
            capture_endpoint_id: Some("mic".to_owned()),
            degraded_output: DegradedOutput::Raw,
            ..EngineSettings::default()
        };
        let failure = SessionStartFailure::new(PrimaryReasonCode::DspFailure, "invalid custom DLL");
        for detail in ["capture is busy", "capture identity could not be verified"] {
            let reason = failure.capture_unavailable_reason(&settings, detail);
            assert_eq!(reason.code, PrimaryReasonCode::WaitingForCapture);
            assert_eq!(reason.endpoint_id.as_deref(), Some("mic"));
            assert_eq!(reason.detail, Some(format!("invalid custom DLL; {detail}")));
        }
        let reason =
            failure.capture_unavailable_reason(&EngineSettings::default(), "capture is busy");
        assert_eq!(reason.code, PrimaryReasonCode::WaitingForCapture);
        assert!(reason.endpoint_id.is_none());
        assert_eq!(failure.primary_reason().code, PrimaryReasonCode::DspFailure);
    }

    #[test]
    fn queued_processing_telemetry_cannot_overwrite_a_fallback_transition() {
        for apply_before_transition in [true, false] {
            let (mut publisher, mut receiver) = telemetry_channel();
            let runtime = Arc::new(Mutex::new(RuntimeSnapshot::from_settings_load(
                &SettingsLoad {
                    settings: EngineSettings::default(),
                    reset_reason: None,
                },
                None,
            )));
            let hook_ran = Arc::new(AtomicBool::new(false));
            let hook_runtime = Arc::clone(&runtime);
            let hook_observed = Arc::clone(&hook_ran);
            receiver.after_take = Some(Box::new(move || {
                assert!(
                    matches!(
                        hook_runtime.try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ),
                    "runtime lock must already be held after telemetry selection",
                );
                hook_observed.store(true, Ordering::Release);
            }));
            let mut session = Some(TrackedSession {
                kind: SessionKind::Processing,
                stop: Arc::new(AtomicBool::new(false)),
                processing: Some(ProcessingSessionControl {
                    fallback_requested: Arc::new(AtomicBool::new(false)),
                    fallback_reason: Arc::new(AtomicU8::new(encode_fallback_reason(
                        PrimaryReasonCode::Starting,
                    ))),
                    fallback_ready: Arc::new(AtomicBool::new(false)),
                    telemetry: receiver,
                }),
                handle: thread::spawn(|| {}),
            });
            publisher.publish(RealtimeTelemetry {
                state: EngineState::Processing,
                output: EngineOutput::Processed,
                reason: ReasonUpdate::Clear,
                metrics: EngineMetrics::default(),
            });
            if apply_before_transition {
                apply_pending_telemetry(&mut session, &runtime);
                assert_eq!(lock_unpoisoned(&runtime).output, EngineOutput::Processed);
            }
            publisher.advance_epoch();
            lock_unpoisoned(&runtime).publish(RuntimeTransition {
                state: EngineState::Degraded,
                output: EngineOutput::Raw,
                primary_reason: Some(PrimaryReason::new(PrimaryReasonCode::CpuOverload)),
                active_route: Some(ActiveRoute {
                    render_endpoint_id: None,
                    capture_endpoint_id: Some("mic".to_owned()),
                    output_endpoint_id: "virtual".to_owned(),
                }),
                metrics: EngineMetrics::default(),
            });

            apply_pending_telemetry(&mut session, &runtime);
            assert!(hook_ran.load(Ordering::Acquire));
            let snapshot = lock_unpoisoned(&runtime);
            assert_eq!(snapshot.state, EngineState::Degraded);
            assert_eq!(snapshot.output, EngineOutput::Raw);
            assert_eq!(
                snapshot.primary_reason.as_ref().unwrap().code,
                PrimaryReasonCode::CpuOverload
            );
            drop(snapshot);
            session.take().unwrap().handle.join().unwrap();
        }
    }

    #[test]
    fn apply_saves_settings_after_fallback_owns_the_handoff() {
        let directory =
            std::env::temp_dir().join(format!("xmic-apply-fallback-{}", std::process::id()));
        let path = directory.join("settings.json");
        let controller = RealtimeEngineController::new(
            PathBuf::from("missing-native.dll"),
            SettingsStore::new(path.clone()),
        );
        lock_unpoisoned(&controller.control).desired_enabled = true;
        install_fake_processing_owner(&controller);
        let settings = EngineSettings {
            virtual_device_mode: Some(xmic_engine_protocol::VirtualDeviceMode::Custom),
            ..EngineSettings::default()
        };

        controller.update_settings(settings.clone()).unwrap();

        assert_eq!(lock_unpoisoned(&controller.control).settings, settings);
        assert!(path.is_file());
        assert_eq!(
            lock_unpoisoned(&controller.runtime).state,
            EngineState::Blocked
        );
        let _ = controller.stop();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn prepare_fallback_waits_for_the_single_processing_owner() {
        let directory =
            std::env::temp_dir().join(format!("xmic-prepare-fallback-{}", std::process::id()));
        let controller = RealtimeEngineController::new(
            PathBuf::from("missing-native.dll"),
            SettingsStore::new(directory.join("settings.json")),
        );
        let fallback_ready = install_fake_processing_owner(&controller);

        controller
            .prepare_fallback(PrimaryReasonCode::Starting)
            .unwrap();

        assert!(fallback_ready.load(Ordering::Acquire));
        controller.stop().unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn timed_out_sessions_remain_owned() {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(Duration::from_millis(40));
        });
        let mut session = Some(TrackedSession {
            kind: SessionKind::Processing,
            stop,
            processing: None,
            handle,
        });

        assert!(!stop_tracked(&mut session, Duration::from_millis(1), None));
        assert!(session.is_some(), "timeout must not detach the session");
        thread::sleep(Duration::from_millis(50));
        assert!(stop_tracked(&mut session, Duration::from_secs(1), None));
        assert!(session.is_none());
    }

    #[test]
    fn settings_reset_survives_a_helper_controller_restart() {
        let directory =
            std::env::temp_dir().join(format!("xmic-engine-reset-{}", std::process::id()));
        let path = directory.join("settings.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&path, r#"{"schemaVersion":2}"#).unwrap();

        let first = RealtimeEngineController::new(
            PathBuf::from("missing-native.dll"),
            SettingsStore::new(path.clone()),
        );
        let permit = NotificationProbePermit::default();
        let first_status = first.status(permit);
        assert_eq!(
            first_status.reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::SettingsReset)
        );
        assert!(first_status.dsp_validation.error.is_some());
        drop(first);

        let second = RealtimeEngineController::new(
            PathBuf::from("missing-native.dll"),
            SettingsStore::new(path.clone()),
        );
        assert_eq!(
            second
                .status(permit)
                .reason
                .as_ref()
                .map(|reason| reason.code),
            Some(PrimaryReasonCode::SettingsReset)
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn selected_dsp_library_is_validated_before_any_session_starts() {
        let directory =
            std::env::temp_dir().join(format!("xmic-engine-dsp-validation-{}", std::process::id()));
        let controller = RealtimeEngineController::new(
            directory.join("missing-native.dll"),
            SettingsStore::new(directory.join("settings.json")),
        );

        let status = controller.status(NotificationProbePermit::default());

        assert!(!status.session_running);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert_eq!(
            status.reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::DspFailure)
        );
        assert_eq!(status.dsp_validation.expected_abi, DSP_ABI_VERSION);
        assert!(status.dsp_validation.actual_abi.is_none());
        assert!(status.dsp_validation.error.is_some());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn an_unpinned_selection_is_latched_without_loading_a_later_file() {
        let directory =
            std::env::temp_dir().join(format!("xmic-engine-dsp-guard-{}", std::process::id()));
        let native_library = directory.join("appeared-later.dll");
        let controller = RealtimeEngineController::new_with_library_guard_error(
            native_library.clone(),
            Some("selection could not be pinned".to_owned()),
            SettingsStore::new(directory.join("settings.json")),
        );
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(native_library, b"replacement").unwrap();

        let status = controller.status(NotificationProbePermit::default());

        assert_eq!(
            status.dsp_validation.error.as_deref(),
            Some("selection could not be pinned")
        );
        assert_eq!(
            status.reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::DspFailure)
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn invalid_settings_reset_is_exposed_in_the_public_reason() {
        let load = SettingsLoad {
            settings: EngineSettings::default(),
            reset_reason: Some("invalid current settings".to_owned()),
        };

        let runtime = RuntimeSnapshot::from_settings_load(&load, None);

        assert_eq!(runtime.state, EngineState::Off);
        assert_eq!(runtime.output, EngineOutput::Inactive);
        assert_eq!(
            runtime.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::SettingsReset)
        );
    }

    #[test]
    fn worker_timeout_keeps_a_possible_active_fallback_visible() {
        let mut runtime = RuntimeSnapshot {
            state: EngineState::Degraded,
            output: EngineOutput::Raw,
            primary_reason: None,
            startup_dsp_failure: None,
            active_route: Some(ActiveRoute {
                render_endpoint_id: None,
                capture_endpoint_id: Some("mic".to_owned()),
                output_endpoint_id: "virtual".to_owned(),
            }),
            metrics: EngineMetrics::default(),
        };

        runtime.worker_timeout("stop timed out");

        assert_eq!(runtime.state, EngineState::Degraded);
        assert_eq!(runtime.output, EngineOutput::Raw);
        assert!(runtime.active_route.is_some());
        assert_eq!(
            runtime.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::WorkerTimeout)
        );
    }

    #[test]
    fn manual_stop_does_not_acknowledge_a_settings_reset() {
        let load = SettingsLoad {
            settings: EngineSettings::default(),
            reset_reason: Some("invalid current settings".to_owned()),
        };
        let mut runtime = RuntimeSnapshot::from_settings_load(&load, None);

        runtime.off();

        assert_eq!(
            runtime.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::SettingsReset)
        );
        runtime.reset_acknowledged();
        assert_eq!(
            runtime.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::ManualOff)
        );
    }

    #[test]
    fn aec_off_route_has_no_render_endpoint() {
        let route = ActiveRoute {
            render_endpoint_id: None,
            capture_endpoint_id: Some("mic".to_owned()),
            output_endpoint_id: "virtual".to_owned(),
        };
        assert_eq!(route.render_endpoint_id, None);
    }

    #[test]
    fn metric_baseline_accumulates_device_counters_across_sessions() {
        let mut metrics = EngineMetrics {
            capture_overflows: 7,
            output_underruns: 3,
            ..EngineMetrics::default()
        };
        let source = SourceCountersSnapshot {
            capture_overflows: 2,
            data_discontinuities: 1,
            ..SourceCountersSnapshot::default()
        };
        let output = OutputCountersSnapshot {
            underrun_events: 4,
            ..OutputCountersSnapshot::default()
        };
        let baseline = MetricBaseline::new(&metrics, &source, &output);
        let later_source = SourceCountersSnapshot {
            capture_overflows: 5,
            data_discontinuities: 3,
            ..source
        };
        let later_output = OutputCountersSnapshot {
            underrun_events: 9,
            ..output
        };
        let mut window = StatusWindow::new(Instant::now());
        apply_device_metrics(
            &mut metrics,
            &later_source,
            &later_output,
            0,
            0,
            &baseline,
            &mut window,
        );

        assert_eq!(metrics.capture_overflows, 10);
        assert_eq!(metrics.device_discontinuities, 2);
        assert_eq!(metrics.output_underruns, 8);
    }

    #[test]
    fn sub_window_handoff_keeps_final_deadline_misses() {
        let mut metrics = EngineMetrics {
            processing_deadline_misses: 3,
            ..EngineMetrics::default()
        };
        let source = SourceCountersSnapshot::default();
        let output = OutputCountersSnapshot::default();
        let baseline = MetricBaseline::new(&metrics, &source, &output);
        let mut histogram = ProcessingHistogram::default();
        histogram.observe(Duration::from_millis(11));

        apply_processing_time_metrics(&mut metrics, &histogram, &baseline);

        assert_eq!(metrics.processing_deadline_misses, 4);
        assert!(metrics.processing_time_max_us >= 11_000);
    }

    #[test]
    fn requested_fallback_carries_the_control_thread_reason() {
        let reason = AtomicU8::new(encode_fallback_reason(
            PrimaryReasonCode::WaitingForPlayback,
        ));
        assert_eq!(
            fallback_request_reason(&reason),
            PrimaryReasonCode::WaitingForPlayback,
        );
        reason.store(
            encode_fallback_reason(PrimaryReasonCode::WaitingForVirtualOutput),
            Ordering::Release,
        );
        assert_eq!(
            fallback_request_reason(&reason),
            PrimaryReasonCode::WaitingForVirtualOutput,
        );
    }

    #[test]
    fn render_pairing_keeps_future_reference_for_a_later_capture() {
        assert!(render_is_after_capture(20_000, 10_000));
        assert!(!render_is_after_capture(10_000, 20_000));
        assert!(!render_is_after_capture(0, 20_000));
        assert!(!render_is_after_capture(20_000, 0));
    }

    #[test]
    fn pipeline_latency_counts_each_segment() {
        let output = OutputCountersSnapshot {
            queue_depth_frames: (DSP_SAMPLES_PER_FRAME * 3) as u64,
            endpoint_padding_frames: 480,
            ..OutputCountersSnapshot::default()
        };
        let breakdown = latency_breakdown(8, 120_000, 2, &output, 30_000);
        assert_eq!(breakdown.capture_ms, 22.0);
        assert_eq!(breakdown.dsp_ms, 80.0);
        assert_eq!(breakdown.output_ms, 43.0);
        assert_eq!(breakdown.total_ms(), 145.0);
    }

    #[test]
    fn echo_path_rebuild_requests_are_coalesced_not_dropped() {
        let start = Instant::now();
        let mut limiter = EchoPathResetLimiter::default();
        limiter.request();
        assert!(limiter.take_due(start));
        limiter.request();
        assert!(!limiter.take_due(start + Duration::from_millis(100)));
        assert!(limiter.take_due(start + ECHO_PATH_RESET_MIN_INTERVAL));
    }

    #[test]
    fn default_capture_identity_requires_a_visible_configured_virtual_endpoint() {
        let settings = EngineSettings {
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            ..EngineSettings::default()
        };
        assert!(default_capture_identity_is_unverifiable(&settings, &[]));
        let endpoint = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "virtual-capture".to_owned(),
            group_id: None,
            friendly_name: "Custom return".to_owned(),
            flow: DataFlow::Capture,
            default_roles: Vec::new(),
        };
        assert!(!default_capture_identity_is_unverifiable(
            &settings,
            &[endpoint]
        ));
    }

    #[test]
    fn default_capture_must_match_the_current_multimedia_endpoint() {
        let selected = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "physical-default".to_owned(),
            group_id: Some("physical-group".to_owned()),
            friendly_name: "Physical microphone".to_owned(),
            flow: DataFlow::Capture,
            default_roles: vec![xmic_platform_api::DeviceRole::Multimedia],
        };
        let unexpected = EndpointDescriptor {
            id: "unexpected-capture".to_owned(),
            group_id: Some("unexpected-group".to_owned()),
            ..selected.clone()
        };

        assert!(capture_matches_selected_physical(
            &EngineSettings::default(),
            &selected,
            std::slice::from_ref(&selected),
        ));
        assert!(!capture_matches_selected_physical(
            &EngineSettings::default(),
            &unexpected,
            &[selected],
        ));
    }

    #[test]
    fn standard_vb_cable_capture_is_never_a_physical_source() {
        let capture = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "re-enumerated-cable-output".to_owned(),
            group_id: None,
            friendly_name: "CABLE Output (VB-Audio Virtual Cable)".to_owned(),
            flow: DataFlow::Capture,
            default_roles: Vec::new(),
        };

        assert!(capture_is_configured_virtual(
            &EngineSettings::default(),
            &capture,
            &[],
        ));
    }

    #[test]
    fn configured_virtual_capture_is_never_a_raw_source() {
        let settings = EngineSettings {
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            ..EngineSettings::default()
        };
        let capture = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "virtual-capture".to_owned(),
            group_id: None,
            friendly_name: "Custom return".to_owned(),
            flow: xmic_platform_api::DataFlow::Capture,
            default_roles: Vec::new(),
        };
        assert!(capture_is_configured_virtual(&settings, &capture, &[]));
    }

    #[test]
    fn configured_virtual_capture_is_matched_by_a_trusted_name() {
        let settings = EngineSettings {
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![xmic_engine_protocol::TrustedVirtualAudioPair {
                render_endpoint_id: "virtual-render".to_owned(),
                capture_endpoint_id: "virtual-capture".to_owned(),
                render_name: "Custom input".to_owned(),
                capture_name: "Custom return".to_owned(),
            }],
            ..EngineSettings::default()
        };
        let capture = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "reinstalled-capture".to_owned(),
            group_id: None,
            friendly_name: "custom return".to_owned(),
            flow: DataFlow::Capture,
            default_roles: Vec::new(),
        };
        assert!(capture_is_configured_virtual(&settings, &capture, &[]));
    }

    #[test]
    fn configured_virtual_capture_is_matched_by_a_known_group() {
        let settings = EngineSettings {
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            ..EngineSettings::default()
        };
        let capture = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "virtual-capture-alias".to_owned(),
            group_id: Some("virtual-group".to_owned()),
            friendly_name: "Custom return".to_owned(),
            flow: DataFlow::Capture,
            default_roles: Vec::new(),
        };
        let configured = EndpointDescriptor {
            platform: "windows".to_owned(),
            id: "virtual-capture".to_owned(),
            group_id: Some("virtual-group".to_owned()),
            friendly_name: "Custom return".to_owned(),
            flow: DataFlow::Capture,
            default_roles: Vec::new(),
        };
        assert!(capture_is_configured_virtual(
            &settings,
            &capture,
            &[configured]
        ));
    }

    #[test]
    fn output_only_route_has_no_fake_capture_identity() {
        let route = ActiveRoute {
            render_endpoint_id: None,
            capture_endpoint_id: None,
            output_endpoint_id: "virtual-output".to_owned(),
        };
        assert_eq!(route.capture_endpoint_id, None);
    }

    #[test]
    fn starting_records_a_device_restart_without_inventing_recovery_metrics() {
        let mut runtime = RuntimeSnapshot {
            state: EngineState::Degraded,
            output: EngineOutput::Raw,
            primary_reason: Some(PrimaryReason::new(PrimaryReasonCode::CpuOverload)),
            startup_dsp_failure: None,
            active_route: None,
            metrics: EngineMetrics::default(),
        };

        runtime.starting();

        assert_eq!(runtime.metrics.device_restarts, 1);
        assert_eq!(runtime.metrics.processing_recovery_events, 0);
    }

    #[test]
    fn fallback_reuses_streams_only_when_endpoint_identity_is_unchanged() {
        fn assert_from_realtime_signature(
            converter: fn(
                &EngineSettings,
                &SessionStartFailure,
                RealtimeSession,
            ) -> Result<FallbackSession, SessionStartFailure>,
        ) {
            let _ = converter;
        }

        assert_from_realtime_signature(FallbackSession::from_realtime);
        assert!(control_fallback_reuses_processing_resources(
            PrimaryReasonCode::Starting,
        ));
        assert!(!control_fallback_reuses_processing_resources(
            PrimaryReasonCode::WaitingForCapture,
        ));
        assert!(!control_fallback_reuses_processing_resources(
            PrimaryReasonCode::WaitingForPlayback,
        ));
    }

    #[test]
    fn fallback_worker_failure_retains_its_final_metric_deltas() {
        let runtime = Mutex::new(RuntimeSnapshot {
            state: EngineState::Degraded,
            output: EngineOutput::Raw,
            primary_reason: None,
            startup_dsp_failure: None,
            active_route: None,
            metrics: EngineMetrics::default(),
        });
        let final_metrics = EngineMetrics {
            capture_overflows: 4,
            output_underruns: 2,
            ..EngineMetrics::default()
        };

        let message = retain_failed_session_metrics(
            SessionExit::Failed(SessionFailure::FallbackWorker, final_metrics),
            &runtime,
        );

        assert!(message.is_some());
        assert_eq!(lock_unpoisoned(&runtime).metrics.capture_overflows, 4);
        assert_eq!(lock_unpoisoned(&runtime).metrics.output_underruns, 2);
    }

    #[test]
    fn fallback_transition_records_the_triggering_failure() {
        let mut metrics = EngineMetrics::default();

        note_failure_metric(&mut metrics, PrimaryReasonCode::CpuOverload);

        assert_eq!(metrics.processing_overload_events, 1);
        assert_eq!(metrics.processing_load_mode, ProcessingLoadMode::Protection);
    }

    #[test]
    fn fallback_reason_is_structured() {
        let failure = SessionStartFailure::new(PrimaryReasonCode::DspFailure, "missing DLL");
        let reason = PrimaryReason::with_detail(failure.code, failure.message);
        assert_eq!(reason.code, PrimaryReasonCode::DspFailure);
        assert_eq!(reason.detail.as_deref(), Some("missing DLL"));
    }

    #[test]
    fn speech_probe_initializes_rnnoise_before_observing_frames() {
        let probe = SpeechProbe::new().expect("RNNoise probe initializes");
        assert!(!probe.presence.fired());
    }

    #[test]
    fn notification_probe_is_only_permitted_while_processing_is_off() {
        let permit = NotificationProbePermit {
            allowed: true,
            needed: true,
        };
        assert!(!notification_probe_permitted(true, permit));
        assert!(notification_probe_permitted(false, permit));
        assert!(!notification_probe_permitted(
            false,
            NotificationProbePermit {
                allowed: false,
                needed: true,
            }
        ));
    }
}
