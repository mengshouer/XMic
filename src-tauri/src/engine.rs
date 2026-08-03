use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
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
    ActiveDeviceProfile, ActiveRoute, ConfigurationIssue,
    DelayCalibrationState as ProtocolDelayCalibrationState, DspSettings, EngineMetrics, EngineMode,
    EngineSettings, EngineStatus, IdleSessionPermit, ProcessingLoadMode, UnprocessedOutput,
    PROTOCOL_VERSION,
};
use xmic_engine_runtime::{processing_latency_frames, AlignedRawPath, DspProcessor};
use xmic_platform_api::{AudioPlatform, PlatformError};
use xmic_platform_windows::{
    register_realtime_audio_thread, start_capture_source, start_device_change_monitor,
    start_duplex_source, start_output_sink, CaptureSourceRequest, CaptureSourceStream,
    DeviceChangeMonitor, DuplexSourceRequest, DuplexSourceStream, OutputCountersSnapshot,
    OutputSinkRequest, OutputSinkStream, SourceCountersSnapshot, WasapiPlatform,
};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp};

use crate::{
    device_profiles::DeviceProfileStore,
    route_validation::{configuration_issue, reconcile_virtual_device_settings},
    settings_store::SettingsStore,
};

mod load;
mod topology;

use load::{ProcessingHistogram, ProcessingOverloadController, PROCESSING_BUDGET_US};
use topology::{validate_capture_output_topology, validate_physical_capture, validate_topology};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_BACKOFF: Duration = Duration::from_secs(1);
const CONVERGENCE_TIME: Duration = Duration::from_secs(2);
const STATUS_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
/// A discontinuity means the render/capture alignment jumped, so AEC3 has to be rebuilt — but the
/// rebuild is a full `AudioProcessing::Initialize()` on the realtime thread, and it costs the echo
/// canceller several seconds of reconvergence. A device that drops packets continuously would spend
/// all of its time rebuilding, so requests are coalesced into at most one rebuild per interval.
const ECHO_PATH_RESET_MIN_INTERVAL: Duration = Duration::from_millis(500);
const QPC_UNITS_PER_MILLISECOND: f32 = 10_000.0;
/// The frontend stops, applies and restarts the engine around every settings write, and a device
/// switch rebuilds the session, so an idle session must not chase those gaps: two streams writing to
/// the virtual device at once would mix processed and unprocessed audio together.
const IDLE_SESSION_GRACE: Duration = Duration::from_secs(3);
/// A route XMic cannot open is not worth retrying forever; the count resets when the settings change
/// or a processing session starts, so a fixed device really can recover.
const IDLE_SESSION_FAILURES_BEFORE_GIVING_UP: u32 = 3;
/// Shorter than the processing session's stop timeout: this one is awaited from the IPC loop.
const IDLE_STOP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RealtimeEngineController {
    control: Mutex<ControlState>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    native_library: PathBuf,
    settings_store: SettingsStore,
    profile_store: Arc<DeviceProfileStore>,
    safe_mode: bool,
    /// Set by the UI permit: speech detection is only worth running until the notice has been shown.
    speech_notice_needed: Arc<AtomicBool>,
    /// Latched by whichever session hears speech while the microphone is not being processed.
    unprocessed_speech: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DspFeature {
    Aec,
    Rnnoise,
    Agc2,
    VadGate,
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

impl DspFeature {
    pub(crate) fn enabled(self, settings: &EngineSettings) -> bool {
        match self {
            Self::Aec => settings.dsp.aec.enabled,
            Self::Rnnoise => settings.dsp.rnnoise.enabled,
            Self::Agc2 => settings.dsp.agc2.enabled,
            Self::VadGate => settings.dsp.vad_gate.enabled,
        }
    }

    pub(crate) fn set_enabled(self, settings: &mut EngineSettings, enabled: bool) {
        match self {
            Self::Aec => settings.dsp.aec.enabled = enabled,
            Self::Rnnoise => settings.dsp.rnnoise.enabled = enabled,
            Self::Agc2 => {
                settings.dsp.agc2.enabled = enabled;
                if enabled {
                    // A stored pass-through combination would make this switch a no-op and is
                    // rejected by validation, so arm the adaptive default alongside it.
                    settings.dsp.agc2.repair_pass_through();
                }
            }
            Self::VadGate => settings.dsp.vad_gate.enabled = enabled,
        }
    }

    pub(crate) fn toggle(self, settings: &mut EngineSettings) -> bool {
        let enabled = !self.enabled(settings);
        self.set_enabled(settings, enabled);
        enabled
    }
}

struct ControlState {
    settings: EngineSettings,
    worker: Option<EngineWorker>,
    idle: Option<IdleWorker>,
    permit: IdleSessionPermit,
    /// When the engine became idle; the grace period is measured from here.
    idle_since: Option<Instant>,
    idle_failures: u32,
}

struct EngineWorker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// Runs whenever XMic is not processing: it either forwards the unprocessed microphone to the virtual
/// device or just listens long enough to warn that nothing is being processed.
struct IdleWorker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    mode: IdleSessionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleSessionMode {
    /// `UnprocessedOutput::RawMicrophone`: capture plus the virtual output, no DSP.
    Forward,
    /// `UnprocessedOutput::Silence`: capture only, purely to detect speech nobody is processing.
    Listen,
}

#[derive(Clone)]
struct RuntimeSnapshot {
    mode: EngineMode,
    detail: String,
    active_route: Option<ActiveRoute>,
    metrics: EngineMetrics,
    warning: Option<String>,
    configuration_issue: Option<ConfigurationIssue>,
    device_profile: Option<ActiveDeviceProfile>,
    effective_dsp: Option<DspSettings>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            mode: EngineMode::Sleeping,
            detail: "audio engine is stopped".to_owned(),
            active_route: None,
            metrics: EngineMetrics::default(),
            warning: None,
            configuration_issue: None,
            device_profile: None,
            effective_dsp: None,
        }
    }
}

impl RuntimeSnapshot {
    /// A processing session changed state.
    ///
    /// **Deliberately leaves `device_profile` and `effective_dsp` alone.** Editing the DSP settings of
    /// the device pair that was last active has to keep working after the engine stops: `status()`
    /// reports `effective_dsp` as the settings, and `update_settings` writes back into the profile
    /// `device_profile` names. That feature is nothing more than these two fields surviving `stop()`,
    /// so a tidy-up that reset everything here would break it silently.
    fn publish_session(
        &mut self,
        mode: EngineMode,
        detail: &str,
        active_route: Option<ActiveRoute>,
        metrics: EngineMetrics,
        warning: Option<String>,
    ) {
        self.mode = mode;
        self.detail = detail.to_owned();
        self.active_route = active_route;
        self.metrics = metrics;
        self.warning = warning;
        self.configuration_issue = None;
    }

    /// Nothing is being processed. Keeps the warning: *why* processing is not running is more useful to
    /// the user than the fact that the microphone is being forwarded.
    fn publish_idle(&mut self, mode: IdleSessionMode, metrics: EngineMetrics) {
        self.mode = idle_engine_mode(mode);
        self.detail = idle_detail(mode).to_owned();
        self.active_route = None;
        self.metrics = metrics;
        self.configuration_issue = None;
    }

    /// The session died and the worker will retry. Keeps `metrics`, so the counters accumulated so far
    /// do not reset to zero in the UI on every reconnect.
    fn fail(&mut self, message: &str) {
        self.mode = EngineMode::Error;
        self.detail = "audio engine is waiting to reconnect".to_owned();
        self.active_route = None;
        self.warning = Some(message.to_owned());
        self.configuration_issue = None;
    }

    fn warn(&mut self, message: &str) {
        self.warning = Some(message.to_owned());
    }

    /// The route cannot be used until the user changes something.
    ///
    /// The only place that drops the device profile, and the reason is not tidiness: if the route is
    /// unusable then the profile resolved for the previous pair no longer describes anything.
    fn require_configuration(&mut self, issue: ConfigurationIssue) {
        self.mode = EngineMode::Sleeping;
        self.detail = "physical microphone selection is required".to_owned();
        self.active_route = None;
        self.warning = None;
        self.configuration_issue = Some(issue);
        self.device_profile = None;
        self.effective_dsp = None;
    }

    fn set_profile(&mut self, device_profile: ActiveDeviceProfile, effective_dsp: DspSettings) {
        self.device_profile = Some(device_profile);
        self.effective_dsp = Some(effective_dsp);
    }
}

impl RealtimeEngineController {
    pub fn new(
        native_library: PathBuf,
        settings_store: SettingsStore,
        profile_store: DeviceProfileStore,
        safe_mode: bool,
    ) -> Self {
        let settings = settings_store.load();
        Self {
            control: Mutex::new(ControlState {
                settings,
                worker: None,
                idle: None,
                permit: IdleSessionPermit::default(),
                idle_since: Some(Instant::now()),
                idle_failures: 0,
            }),
            runtime: Arc::new(Mutex::new(RuntimeSnapshot::default())),
            native_library,
            settings_store,
            profile_store: Arc::new(profile_store),
            safe_mode,
            speech_notice_needed: Arc::new(AtomicBool::new(false)),
            unprocessed_speech: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn status(&self, permit: IdleSessionPermit) -> EngineStatus {
        let mut control = lock_unpoisoned(&self.control);
        let worker_stopped = reap_finished_worker(&mut control);
        if worker_stopped {
            let mut runtime = lock_unpoisoned(&self.runtime);
            if !matches!(runtime.mode, EngineMode::Sleeping | EngineMode::Error) {
                runtime.mode = EngineMode::Error;
                runtime.detail = "audio engine worker stopped".to_owned();
                runtime.warning = Some("the realtime worker exited unexpectedly".to_owned());
            }
        }
        self.synchronize_idle_session(&mut control, permit);
        let runtime = lock_unpoisoned(&self.runtime).clone();
        let mut settings = control.settings.clone();
        if let Some(effective_dsp) = runtime.effective_dsp.clone() {
            settings.dsp = effective_dsp;
        }
        let active_device_profile = runtime
            .active_route
            .as_ref()
            .and(runtime.device_profile.clone());
        EngineStatus {
            protocol_version: PROTOCOL_VERSION,
            running: control.worker.is_some(),
            mode: runtime.mode,
            detail: runtime.detail,
            // The UI process owns endpoint enumeration and fills this in; enumerating here just to
            // answer a poll would duplicate a cache that already exists on the other side.
            endpoints: Vec::new(),
            settings,
            active_route: runtime.active_route,
            active_device_profile,
            configuration_issue: runtime.configuration_issue,
            metrics: runtime.metrics,
            warning: runtime.warning,
            unprocessed_speech_detected: self.unprocessed_speech.load(Ordering::Acquire),
        }
    }

    pub fn update_settings(&self, settings: EngineSettings) -> Result<(), String> {
        settings.validate().map_err(|error| error.to_string())?;
        let mut control = lock_unpoisoned(&self.control);
        reap_finished_worker(&mut control);
        if control.worker.is_some() {
            return Err("stop the audio engine before changing settings".to_owned());
        }
        let runtime = lock_unpoisoned(&self.runtime);
        if let Some(profile) = &runtime.device_profile {
            self.profile_store.update(profile, &settings.dsp)?;
        }
        drop(runtime);
        self.settings_store.save(&settings)?;
        // The idle session was built from the previous settings, and the route it holds may be the one
        // that just changed; a fixed route also deserves a fresh attempt.
        let _ = stop_idle_worker(&mut control);
        control.idle_failures = 0;
        control.settings = settings;
        {
            let mut runtime = lock_unpoisoned(&self.runtime);
            runtime.configuration_issue = None;
            // Whatever the previous route failed with no longer describes the one about to be tried.
            runtime.warning = None;
        }
        Ok(())
    }

    pub fn start(&self) -> Result<(), EngineStartError> {
        let mut control = lock_unpoisoned(&self.control);
        reap_finished_worker(&mut control);
        if control.worker.is_some() {
            return Err(EngineStartError::Runtime(
                "audio engine is already running".to_owned(),
            ));
        }
        // Two streams writing to the virtual device would mix unprocessed audio into the processed
        // signal, so the idle session has to be gone before the processing session opens the output.
        if !stop_idle_worker(&mut control) {
            return Err(EngineStartError::Runtime(
                "the unprocessed microphone path is still releasing the virtual device".to_owned(),
            ));
        }
        let endpoints = WasapiPlatform::new()
            .active_endpoints()
            .map_err(|error| EngineStartError::Runtime(error.to_string()))?;
        if reconcile_virtual_device_settings(&mut control.settings, &endpoints) {
            self.settings_store
                .save(&control.settings)
                .map_err(EngineStartError::Runtime)?;
        }
        control
            .settings
            .validate()
            .map_err(|error| EngineStartError::Runtime(error.to_string()))?;
        if let Some(issue) = configuration_issue(&control.settings, &endpoints) {
            lock_unpoisoned(&self.runtime).require_configuration(issue.clone());
            return Err(EngineStartError::ConfigurationRequired(issue));
        }
        if control.settings.output_endpoint_id.is_none() {
            return Err(EngineStartError::Runtime(
                "select a virtual microphone output endpoint first".to_owned(),
            ));
        }

        lock_unpoisoned(&self.runtime).publish_session(
            EngineMode::Starting,
            "starting realtime audio session",
            None,
            EngineMetrics::default(),
            None,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let settings = control.settings.clone();
        let native_library = self.native_library.clone();
        let profile_store = Arc::clone(&self.profile_store);
        let safe_mode = self.safe_mode;
        let runtime = Arc::clone(&self.runtime);
        let speech_notice_needed = Arc::clone(&self.speech_notice_needed);
        let unprocessed_speech = Arc::clone(&self.unprocessed_speech);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("xmic-dsp".to_owned())
            .spawn(move || {
                run_worker(
                    settings,
                    native_library,
                    profile_store,
                    safe_mode,
                    worker_stop,
                    runtime,
                    SpeechPresenceReport {
                        needed: speech_notice_needed,
                        detected: unprocessed_speech,
                    },
                    ready_tx,
                )
            })
            .map_err(|error| {
                let message = format!("start audio engine worker: {error}");
                lock_unpoisoned(&self.runtime).fail(&message);
                EngineStartError::Runtime(message)
            })?;
        control.worker = Some(EngineWorker { stop, handle });

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => {
                control.idle_failures = 0;
                Ok(())
            }
            Ok(Err(error)) => {
                request_stop_without_join(control.worker.take());
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                request_stop_without_join(control.worker.take());
                lock_unpoisoned(&self.runtime).publish_session(
                    EngineMode::Error,
                    "audio engine startup timed out",
                    None,
                    EngineMetrics::default(),
                    Some("WASAPI did not complete startup within 15 seconds".to_owned()),
                );
                Err(EngineStartError::Runtime(
                    "audio engine startup timed out".to_owned(),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                request_stop_without_join(control.worker.take());
                Err(EngineStartError::Runtime(
                    "audio engine worker stopped during startup".to_owned(),
                ))
            }
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        control.idle_since = Some(Instant::now());
        let Some(worker) = control.worker.take() else {
            lock_unpoisoned(&self.runtime).publish_session(
                EngineMode::Sleeping,
                "audio engine is stopped",
                None,
                EngineMetrics::default(),
                None,
            );
            return Ok(());
        };
        if let Err(error) = stop_worker_with_timeout(worker) {
            lock_unpoisoned(&self.runtime).publish_session(
                EngineMode::Error,
                "audio engine stop timed out",
                None,
                EngineMetrics::default(),
                Some(error.clone()),
            );
            return Err(error);
        }
        lock_unpoisoned(&self.runtime).publish_session(
            EngineMode::Sleeping,
            "audio engine is stopped",
            None,
            EngineMetrics::default(),
            None,
        );
        Ok(())
    }

    /// Stops both sessions so the microphone and the virtual device are released before the helper
    /// process exits.
    pub fn shutdown(&self) -> Result<(), String> {
        {
            let mut control = lock_unpoisoned(&self.control);
            let _ = stop_idle_worker(&mut control);
        }
        self.stop()
    }

    /// Evaluated on every status poll, which is the one call the UI makes about once a second. Only
    /// the decision happens here; opening WASAPI streams is left to the worker thread so a slow device
    /// cannot stall the IPC loop.
    fn synchronize_idle_session(&self, control: &mut ControlState, permit: IdleSessionPermit) {
        control.permit = permit;
        self.speech_notice_needed
            .store(permit.speech_notice_needed, Ordering::Release);
        if !permit.speech_notice_needed {
            self.unprocessed_speech.store(false, Ordering::Release);
        }
        self.reap_idle_worker(control);

        let desired = desired_idle_mode(control);
        match (control.idle.as_ref().map(|worker| worker.mode), desired) {
            (Some(current), Some(wanted)) if current == wanted => {}
            (Some(_), _) => {
                let _ = stop_idle_worker(control);
            }
            (None, Some(wanted)) if idle_grace_elapsed(control) => {
                self.start_idle_worker(control, wanted);
            }
            (None, _) => {}
        }
    }

    fn reap_idle_worker(&self, control: &mut ControlState) {
        let finished = control
            .idle
            .as_ref()
            .is_some_and(|worker| worker.handle.is_finished());
        if !finished {
            return;
        }
        if let Some(worker) = control.idle.take() {
            let _ = worker.handle.join();
        }
        control.idle_failures = control.idle_failures.saturating_add(1);
        control.idle_since = Some(Instant::now());
        if control.idle_failures >= IDLE_SESSION_FAILURES_BEFORE_GIVING_UP {
            lock_unpoisoned(&self.runtime).warn(
                "gave up on the unprocessed microphone path after repeated failures; change the audio settings or start processing to try again",
            );
        }
    }

    fn start_idle_worker(&self, control: &mut ControlState, mode: IdleSessionMode) {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let settings = control.settings.clone();
        let runtime = Arc::clone(&self.runtime);
        let report = SpeechPresenceReport {
            needed: Arc::clone(&self.speech_notice_needed),
            detected: Arc::clone(&self.unprocessed_speech),
        };
        match thread::Builder::new()
            .name("xmic-idle".to_owned())
            .spawn(move || run_idle_worker(settings, mode, worker_stop, runtime, report))
        {
            Ok(handle) => {
                control.idle = Some(IdleWorker { stop, handle, mode });
                control.idle_since = None;
            }
            Err(error) => {
                control.idle_failures = control.idle_failures.saturating_add(1);
                control.idle_since = Some(Instant::now());
                lock_unpoisoned(&self.runtime)
                    .warn(&format!("start the unprocessed microphone path: {error}"));
            }
        }
    }
}

fn desired_idle_mode(control: &ControlState) -> Option<IdleSessionMode> {
    if control.worker.is_some()
        || !control.permit.bypass_allowed
        || control.idle_failures >= IDLE_SESSION_FAILURES_BEFORE_GIVING_UP
    {
        return None;
    }
    match control.settings.unprocessed_output {
        UnprocessedOutput::RawMicrophone => Some(IdleSessionMode::Forward),
        // Silence needs no session of its own. The only reason to open the microphone is the one-shot
        // notice, so it closes again as soon as that notice has been shown.
        UnprocessedOutput::Silence if control.permit.speech_notice_needed => {
            Some(IdleSessionMode::Listen)
        }
        UnprocessedOutput::Silence => None,
    }
}

fn idle_grace_elapsed(control: &ControlState) -> bool {
    control
        .idle_since
        .is_none_or(|since| since.elapsed() >= IDLE_SESSION_GRACE)
}

/// Returns false when the worker is still alive after the timeout. It stays tracked in that case: an
/// untracked thread would keep holding the virtual device and mix its audio into whatever opens the
/// output next, which is the one thing the idle session must never do.
fn stop_idle_worker(control: &mut ControlState) -> bool {
    control.idle_since = Some(Instant::now());
    let Some(worker) = control.idle.take() else {
        return true;
    };
    worker.stop.store(true, Ordering::Release);
    let deadline = Instant::now() + IDLE_STOP_TIMEOUT;
    while !worker.handle.is_finished() {
        if Instant::now() >= deadline {
            control.idle = Some(worker);
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = worker.handle.join();
    true
}

fn reap_finished_worker(control: &mut ControlState) -> bool {
    let finished = control
        .worker
        .as_ref()
        .is_some_and(|worker| worker.handle.is_finished());
    if finished {
        if let Some(worker) = control.worker.take() {
            let _ = worker.handle.join();
        }
        // The supervisor restarts a processing session that died on its own, so the idle session waits
        // out the same grace period it does after a deliberate stop.
        control.idle_since = Some(Instant::now());
    }
    finished
}

fn request_stop_without_join(worker: Option<EngineWorker>) {
    if let Some(worker) = worker {
        worker.stop.store(true, Ordering::Release);
        drop(worker.handle);
    }
}

fn stop_worker_with_timeout(worker: EngineWorker) -> Result<(), String> {
    worker.stop.store(true, Ordering::Release);
    let deadline = Instant::now() + STOP_TIMEOUT;
    while !worker.handle.is_finished() {
        if Instant::now() >= deadline {
            drop(worker.handle);
            return Err("audio engine worker did not stop within 10 seconds".to_owned());
        }
        thread::sleep(Duration::from_millis(10));
    }
    worker
        .handle
        .join()
        .map_err(|_| "audio engine worker panicked".to_owned())
}

/// Shared with whichever session is running: the UI decides how long detection is worth doing, the
/// session reports what it heard.
struct SpeechPresenceReport {
    needed: Arc<AtomicBool>,
    detected: Arc<AtomicBool>,
}

impl SpeechPresenceReport {
    fn wanted(&self) -> bool {
        self.needed.load(Ordering::Acquire)
    }

    fn report(&self) {
        self.detected.store(true, Ordering::Release);
    }
}

/// RNNoise runs here purely as a side chain: its denoised output is thrown away, only the speech
/// probability is used. It is safe Rust with no native dependency, so it still works when the missing
/// `xmic_webrtc_dsp.dll` is the very reason nothing is being processed.
struct SpeechProbe {
    rnnoise: Option<RnnoiseDsp>,
    presence: SpeechPresenceDetector,
    discarded: [f32; DSP_SAMPLES_PER_FRAME],
    unavailable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SpeechProbeFrame {
    probability: Option<f32>,
    confirmed: bool,
}

impl Default for SpeechProbe {
    fn default() -> Self {
        Self {
            rnnoise: None,
            presence: SpeechPresenceDetector::default(),
            discarded: [0.0; DSP_SAMPLES_PER_FRAME],
            unavailable: false,
        }
    }
}

impl SpeechProbe {
    fn observe(
        &mut self,
        samples: &[f32; DSP_SAMPLES_PER_FRAME],
        peak_dbfs: f32,
    ) -> SpeechProbeFrame {
        if self.unavailable || self.presence.fired() {
            return SpeechProbeFrame::default();
        }
        if self.rnnoise.is_none() {
            match RnnoiseDsp::new(RnnoiseConfig { mix: 1.0 }) {
                Ok(rnnoise) => self.rnnoise = Some(rnnoise),
                Err(_) => {
                    self.unavailable = true;
                    return SpeechProbeFrame::default();
                }
            }
        }
        let Some(rnnoise) = self.rnnoise.as_mut() else {
            return SpeechProbeFrame::default();
        };
        match rnnoise.process_frame(samples, &mut self.discarded) {
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

fn run_idle_worker(
    settings: EngineSettings,
    mode: IdleSessionMode,
    stop: Arc<AtomicBool>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    report: SpeechPresenceReport,
) {
    // Forwarding audio is realtime work even without DSP; losing MMCSS is not worth refusing over.
    let _realtime_thread = register_realtime_audio_thread().ok();
    let monitor = match start_device_change_monitor() {
        Ok(monitor) => monitor,
        Err(error) => {
            lock_unpoisoned(&runtime).warn(&format!(
                "start device change monitor while not processing: {error}"
            ));
            return;
        }
    };
    let mut probe = SpeechProbe::default();

    while !stop.load(Ordering::Acquire) {
        let generation = monitor.generation();
        let mut session = match IdleSession::start(&settings, mode) {
            Ok(session) => session,
            Err(error) => {
                lock_unpoisoned(&runtime).warn(&format!("{}：{error}", idle_failure_context(mode)));
                break;
            }
        };
        lock_unpoisoned(&runtime).publish_idle(mode, EngineMetrics::default());
        let exit = session.run(&stop, &monitor, generation, &runtime, &mut probe, &report);
        let _ = session.stop();
        match exit {
            IdleExit::DeviceChanged => continue,
            IdleExit::Failed(error) => {
                if !stop.load(Ordering::Acquire) {
                    lock_unpoisoned(&runtime)
                        .warn(&format!("{}：{error}", idle_failure_context(mode)));
                }
                break;
            }
            IdleExit::Stopped => break,
        }
    }
    let _ = monitor.stop();
}

enum IdleExit {
    DeviceChanged,
    Failed(String),
    Stopped,
}

struct IdleSession {
    source: CaptureSourceStream,
    output: Option<OutputSinkStream>,
    mode: IdleSessionMode,
}

impl IdleSession {
    fn start(settings: &EngineSettings, mode: IdleSessionMode) -> Result<Self, String> {
        let source = start_capture_source(CaptureSourceRequest {
            capture_endpoint_id: settings.capture_endpoint_id.clone(),
            capture_channel: settings.capture_channel,
        })
        .map_err(|error| error.to_string())?;
        let output = match mode {
            IdleSessionMode::Listen => {
                validate_physical_capture(&source.info().capture_endpoint)?;
                None
            }
            IdleSessionMode::Forward => {
                let endpoint_id = settings
                    .output_endpoint_id
                    .clone()
                    .ok_or_else(|| "virtual microphone output endpoint is missing".to_owned())?;
                let output = start_output_sink(OutputSinkRequest { endpoint_id })
                    .map_err(|error| error.to_string())?;
                validate_capture_output_topology(
                    &source.info().capture_endpoint,
                    &output.info().endpoint,
                )?;
                Some(output)
            }
        };
        Ok(Self {
            source,
            output,
            mode,
        })
    }

    fn run(
        &mut self,
        stop: &AtomicBool,
        monitor: &DeviceChangeMonitor,
        generation: u64,
        runtime: &Mutex<RuntimeSnapshot>,
        probe: &mut SpeechProbe,
        report: &SpeechPresenceReport,
    ) -> IdleExit {
        let mut metrics = EngineMetrics::default();
        let mut window = StatusWindow::new(Instant::now());
        let mut vad_probability = None;

        loop {
            if stop.load(Ordering::Acquire) {
                return IdleExit::Stopped;
            }
            if monitor.worker_finished() {
                return IdleExit::Failed("device change monitor stopped".to_owned());
            }
            if monitor.generation() != generation {
                return IdleExit::DeviceChanged;
            }
            if self.source.worker_finished()
                || self
                    .output
                    .as_ref()
                    .is_some_and(OutputSinkStream::worker_finished)
            {
                return IdleExit::Failed("a WASAPI worker stopped unexpectedly".to_owned());
            }

            window.observe_backlog(self.source.capture_queue_blocks());
            let Some(capture) = self.source.pop_capture() else {
                thread::park_timeout(Duration::from_millis(20));
                continue;
            };
            let peak_dbfs = peak_dbfs(&capture.samples);
            window.observe_input_peak(peak_dbfs);
            metrics.input_clipped_samples = metrics
                .input_clipped_samples
                .saturating_add(clipped_sample_count(&capture.samples));
            if report.wanted() {
                let frame = probe.observe(&capture.samples, peak_dbfs);
                if let Some(probability) = frame.probability {
                    vad_probability = Some(probability);
                }
                if frame.confirmed {
                    report.report();
                }
            }
            if let Some(output) = &mut self.output {
                // Forwarding copies the capture frame verbatim, so the output's peak is the input's.
                window.observe_output_peak(peak_dbfs);
                if !output.push_block(capture.samples) {
                    metrics.output_queue_overflows = output.counters().queue_overflows;
                }
            }
            metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);

            if window.due(Instant::now()) {
                let output_counters = self
                    .output
                    .as_ref()
                    .map(OutputSinkStream::counters)
                    .unwrap_or_default();
                let output_stream_latency_100ns = self
                    .output
                    .as_ref()
                    .map_or(0, |output| output.info().stream_latency_100ns);
                // No DSP means no look-ahead: the only latency left is what the devices impose.
                apply_device_metrics(
                    &mut metrics,
                    &self.source.counters(),
                    &output_counters,
                    output_stream_latency_100ns,
                    0,
                    &mut window,
                );
                metrics.input_peak_dbfs = window.take_input_peak();
                metrics.output_peak_dbfs = window.take_output_peak();
                metrics.vad_probability = vad_probability.take();
                lock_unpoisoned(runtime).publish_idle(self.mode, metrics.clone());
            }
        }
    }

    fn stop(self) -> Result<(), PlatformError> {
        let source_result = self.source.stop();
        let output_result = self.output.map_or(Ok(()), OutputSinkStream::stop);
        source_result.and(output_result)
    }
}

fn idle_engine_mode(mode: IdleSessionMode) -> EngineMode {
    match mode {
        IdleSessionMode::Forward => EngineMode::RawFallback,
        IdleSessionMode::Listen => EngineMode::Sleeping,
    }
}

fn idle_detail(mode: IdleSessionMode) -> &'static str {
    match mode {
        IdleSessionMode::Forward => "not processing; sending the unprocessed microphone",
        IdleSessionMode::Listen => "not processing; the virtual microphone is silent",
    }
}

fn idle_failure_context(mode: IdleSessionMode) -> &'static str {
    match mode {
        IdleSessionMode::Forward => "无法输出未处理的麦克风声音",
        IdleSessionMode::Listen => "无法检测未处理的语音",
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    settings: EngineSettings,
    native_library: PathBuf,
    profile_store: Arc<DeviceProfileStore>,
    safe_mode: bool,
    stop: Arc<AtomicBool>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    speech: SpeechPresenceReport,
    ready: mpsc::SyncSender<Result<(), EngineStartError>>,
) {
    let _realtime_thread = match register_realtime_audio_thread() {
        Ok(registration) => registration,
        Err(error) => {
            let message = format!("register DSP thread with MMCSS: {error}");
            lock_unpoisoned(&runtime).fail(&message);
            let _ = ready.send(Err(EngineStartError::Runtime(message)));
            return;
        }
    };
    let monitor = match start_device_change_monitor() {
        Ok(monitor) => monitor,
        Err(error) => {
            let message = format!("start device change monitor: {error}");
            lock_unpoisoned(&runtime).fail(&message);
            let _ = ready.send(Err(EngineStartError::Runtime(message)));
            return;
        }
    };
    let mut first_session = true;
    let mut metrics = EngineMetrics::default();

    while !stop.load(Ordering::Acquire) {
        match WasapiPlatform::new().active_endpoints() {
            Ok(endpoints) => {
                if let Some(issue) = configuration_issue(&settings, &endpoints) {
                    lock_unpoisoned(&runtime).require_configuration(issue.clone());
                    if first_session {
                        let _ = ready.send(Err(EngineStartError::ConfigurationRequired(issue)));
                    }
                    break;
                }
            }
            Err(error) => {
                let message = format!("read active audio endpoints: {error}");
                lock_unpoisoned(&runtime).fail(&message);
                if first_session {
                    let _ = ready.send(Err(EngineStartError::Runtime(message)));
                    break;
                }
                if wait_for_stop(&stop, RESTART_BACKOFF) {
                    break;
                }
                continue;
            }
        }
        lock_unpoisoned(&runtime).publish_session(
            EngineMode::Starting,
            if first_session {
                "starting realtime audio session"
            } else {
                "reconnecting audio devices"
            },
            None,
            metrics.clone(),
            None,
        );
        let generation = monitor.generation();
        let session = RealtimeSession::start(
            &settings,
            &native_library,
            profile_store.as_ref(),
            safe_mode,
        );
        let mut session = match session {
            Ok(session) => session,
            Err(error) => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let message = format!("启动实时音频会话失败：{error}");
                lock_unpoisoned(&runtime).fail(&message);
                if first_session {
                    let _ = ready.send(Err(EngineStartError::Runtime(message)));
                    return;
                }
                if wait_for_stop(&stop, RESTART_BACKOFF) {
                    break;
                }
                continue;
            }
        };
        if stop.load(Ordering::Acquire) {
            let _ = session.stop();
            break;
        }

        lock_unpoisoned(&runtime).set_profile(
            session.device_profile.clone(),
            session.effective_settings.dsp.clone(),
        );
        let (initial_mode, warning) = session.initial_state();
        lock_unpoisoned(&runtime).publish_session(
            initial_mode,
            initial_detail(initial_mode),
            Some(session.active_route()),
            metrics.clone(),
            warning,
        );
        if first_session {
            let _ = ready.send(Ok(()));
            first_session = false;
        }

        let effective_settings = session.effective_settings.clone();
        let exit = session.run(
            &effective_settings,
            &stop,
            &monitor,
            generation,
            &runtime,
            &mut metrics,
            &speech,
        );
        let stop_result = session.stop();
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = stop_result {
            lock_unpoisoned(&runtime).fail(&format!("stop realtime session: {error}"));
        }
        metrics.device_restarts = metrics.device_restarts.saturating_add(1);
        match exit {
            SessionExit::DeviceChanged => {
                lock_unpoisoned(&runtime).publish_session(
                    EngineMode::Starting,
                    "audio endpoint changed; rebuilding the session",
                    None,
                    metrics.clone(),
                    None,
                );
            }
            SessionExit::Failed(error) => {
                lock_unpoisoned(&runtime).fail(&error);
                if wait_for_stop(&stop, RESTART_BACKOFF) {
                    break;
                }
            }
            SessionExit::Stopped => break,
        }
    }
    let _ = monitor.stop();
}

struct RealtimeSession {
    source: DuplexSourceStream,
    output: OutputSinkStream,
    processor: Option<DspProcessor>,
    raw_path: AlignedRawPath,
    processing_latency_frames: usize,
    route: ActiveRoute,
    device_profile: ActiveDeviceProfile,
    effective_settings: EngineSettings,
    safe_mode: bool,
    native_library: PathBuf,
    initial_warning: Option<String>,
    processing_histogram: ProcessingHistogram,
}

impl RealtimeSession {
    fn start(
        settings: &EngineSettings,
        native_library: &Path,
        profile_store: &DeviceProfileStore,
        safe_mode: bool,
    ) -> Result<Self, String> {
        let render_endpoint_id = if settings.follow_default_render {
            None
        } else {
            settings.render_endpoint_id.clone()
        };
        let source = start_duplex_source(DuplexSourceRequest {
            render_endpoint_id,
            capture_endpoint_id: settings.capture_endpoint_id.clone(),
            capture_channel: settings.capture_channel,
        })
        .map_err(|error| error.to_string())?;
        let output_endpoint_id = settings
            .output_endpoint_id
            .clone()
            .ok_or_else(|| "virtual microphone output endpoint is missing".to_owned())?;
        let output = start_output_sink(OutputSinkRequest {
            endpoint_id: output_endpoint_id,
        })
        .map_err(|error| error.to_string())?;
        validate_topology(
            &source,
            &output.info().endpoint,
            settings.follow_default_render,
        )?;
        let route = ActiveRoute {
            render_endpoint_id: source.info().render_endpoint.id.clone(),
            capture_endpoint_id: source.info().capture_endpoint.id.clone(),
            output_endpoint_id: output.info().endpoint.id.clone(),
        };
        let resolved_profile = profile_store.resolve(
            &source.info().render_endpoint,
            &source.info().capture_endpoint,
            &settings.dsp,
        )?;
        let mut effective_settings = settings.clone();
        effective_settings.dsp = resolved_profile.dsp;
        let raw_delay_frames = processing_latency_frames(&effective_settings.dsp)
            .map_err(|error| error.to_string())?;
        let (processor, initial_warning) = if safe_mode {
            (
                None,
                Some("engine safe mode is active after repeated process crashes".to_owned()),
            )
        } else {
            match DspProcessor::load(
                native_library,
                source.info().render_channels,
                &effective_settings.dsp,
            ) {
                Ok(processor) => (Some(processor), None),
                Err(error) => (
                    None,
                    Some(format!("DSP unavailable; using failure policy: {error}")),
                ),
            }
        };
        Ok(Self {
            source,
            output,
            processor,
            raw_path: AlignedRawPath::new(raw_delay_frames),
            processing_latency_frames: raw_delay_frames,
            route,
            device_profile: resolved_profile.status,
            effective_settings,
            safe_mode,
            native_library: native_library.to_path_buf(),
            initial_warning,
            processing_histogram: ProcessingHistogram::default(),
        })
    }

    fn active_route(&self) -> ActiveRoute {
        self.route.clone()
    }

    fn initial_state(&self) -> (EngineMode, Option<String>) {
        if self.safe_mode {
            return (EngineMode::SafeMode, self.initial_warning.clone());
        }
        match (&self.processor, &self.initial_warning) {
            (Some(_), _) => (EngineMode::Converging, None),
            (None, warning) => (
                unprocessed_engine_mode(self.effective_settings.unprocessed_output),
                warning
                    .clone()
                    .or_else(|| Some("DSP is unavailable".to_owned())),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        settings: &EngineSettings,
        stop: &AtomicBool,
        monitor: &DeviceChangeMonitor,
        generation: u64,
        runtime: &Mutex<RuntimeSnapshot>,
        metrics: &mut EngineMetrics,
        speech: &SpeechPresenceReport,
    ) -> SessionExit {
        let started = Instant::now();
        let mut interleaved_render = [0.0; DSP_SAMPLES_PER_FRAME * MAX_RENDER_CHANNELS];
        let mut processed = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut aligned_raw = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut health = DspHealth {
            failed: self.processor.is_none(),
            warning: self.initial_warning.clone(),
            recovery_mute_until: None,
        };
        let mut gate_was_open = false;
        let mut echo_guard_was_active = false;
        let mut nearend_candidate_was_active = false;
        let mut host_delay_estimator = HostDelayEstimator::default();
        let mut last_data_discontinuities = self.source.counters().data_discontinuities;
        let mut window = StatusWindow::new(Instant::now());
        let mut probe = SpeechProbe::default();
        let mut overload = ProcessingOverloadController::new(settings.dsp.agc2.enabled);
        metrics.processing_load_mode = ProcessingLoadMode::Full;
        let mut echo_path_reset = EchoPathResetLimiter::default();
        let mut last_backlog_events = dsp_backlog_events(&self.source);
        let mut reported_discontinuities = self.source.counters().data_discontinuities;
        let mut reported_output_underruns = self.output.counters().underrun_events;

        loop {
            if stop.load(Ordering::Acquire) {
                return SessionExit::Stopped;
            }
            if monitor.worker_finished() {
                return SessionExit::Failed("device change monitor stopped".to_owned());
            }
            if monitor.generation() != generation {
                return SessionExit::DeviceChanged;
            }
            if self.source.worker_finished() || self.output.worker_finished() {
                return SessionExit::Failed("a WASAPI worker stopped unexpectedly".to_owned());
            }

            window.observe_backlog(self.source.capture_queue_blocks());
            let Some(capture) = self.source.pop_capture() else {
                thread::park_timeout(Duration::from_millis(20));
                continue;
            };
            let processing_started = Instant::now();
            // This frame's load mode. `overload.observe` at the end of the iteration may move it, and
            // the new value must only reach the next frame: a decision about audio that has already
            // been sent cannot be revised by a mode that arrived after it.
            let frame_load = overload.mode();
            let source_counters = self.source.counters();
            if counter_changed(
                &mut last_data_discontinuities,
                source_counters.data_discontinuities,
            ) {
                host_delay_estimator.reset();
                metrics.host_delay_reset_events = metrics.host_delay_reset_events.saturating_add(1);
                echo_path_reset.request();
            }
            let input_peak_dbfs = peak_dbfs(&capture.samples);
            window.observe_input_peak(input_peak_dbfs);
            metrics.input_clipped_samples = metrics
                .input_clipped_samples
                .saturating_add(clipped_sample_count(&capture.samples));
            let mut render_blocks = 0u64;
            let mut latest_render_qpc_position_100ns = None;
            let mut latest_render_sample_count = 0usize;
            while let Some(render) = self.source.pop_render() {
                render_blocks += 1;
                if !health.failed && frame_load != ProcessingLoadMode::Protection {
                    interleave_render(&render, &mut interleaved_render);
                    let sample_count = DSP_SAMPLES_PER_FRAME * render.channels;
                    if let Some(processor) = &mut self.processor {
                        match processor.process_render(&interleaved_render[..sample_count]) {
                            Ok(()) => {
                                latest_render_qpc_position_100ns =
                                    Some(render.timestamp.qpc_position_100ns);
                                latest_render_sample_count = sample_count;
                            }
                            Err(error) => health.fail(
                                metrics,
                                format!(
                                    "DSP render processing failed; using failure policy: {error}"
                                ),
                            ),
                        }
                    }
                }
            }
            if render_blocks == 0 {
                metrics.render_underruns = metrics.render_underruns.saturating_add(1);
            }

            let mut host_delay_observation = host_delay_estimator.current();
            if !health.failed && frame_load != ProcessingLoadMode::Protection {
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
                // Both reset reasons rebuild the same echo canceller, so they share one rate limit.
                if echo_path_reset.take_due(Instant::now()) {
                    match reset_processor_echo_path(&mut self.processor) {
                        Ok(()) => {
                            if latest_render_sample_count > 0 {
                                if let Some(processor) = &mut self.processor {
                                    if let Err(error) = processor.process_render(
                                        &interleaved_render[..latest_render_sample_count],
                                    ) {
                                        health.fail(
                                            metrics,
                                            format!(
                                                "DSP render processing after an echo path rebuild failed; sending the configured unprocessed output: {error}"
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => health.fail(
                            metrics,
                            format!(
                                "DSP echo path rebuild failed; sending the configured unprocessed output: {error}"
                            ),
                        ),
                    }
                }
            }
            update_host_delay_metrics(metrics, host_delay_observation);

            self.raw_path
                .process_frame(&capture.samples, &mut aligned_raw);
            let mut dsp_frame_metrics = None;
            if !health.failed && frame_load != ProcessingLoadMode::Protection {
                if let Some(processor) = &mut self.processor {
                    match processor.process_capture_with_options(
                        &capture.samples,
                        &mut processed,
                        host_delay_observation.stream_delay_ms,
                        frame_load == ProcessingLoadMode::Reduced,
                    ) {
                        Ok(frame_metrics) => dsp_frame_metrics = Some(frame_metrics),
                        Err(error) => health.fail(
                            metrics,
                            format!("DSP capture processing failed; using failure policy: {error}"),
                        ),
                    }
                }
            }
            if let Some(frame_metrics) = dsp_frame_metrics {
                metrics.limiter_limited_samples = metrics.limiter_limited_samples.saturating_add(
                    u64::try_from(frame_metrics.limited_samples).unwrap_or(u64::MAX),
                );
                metrics.vad_probability = Some(frame_metrics.vad_probability);
                metrics.speech_vad_active = frame_metrics.speech_vad_active;
                metrics.vad_gate_open = Some(frame_metrics.gate_open);
                metrics.aec_residual_echo_likelihood = frame_metrics.residual_echo_likelihood;
                metrics.aec_attenuation_db = frame_metrics.aec_attenuation_db;
                if settings.dsp.vad_gate.enabled && frame_metrics.gate_open && !gate_was_open {
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
            }
            let decision = decide(
                self.safe_mode,
                &health,
                frame_load,
                started.elapsed(),
                Instant::now(),
                settings.unprocessed_output,
            );
            if decision.unprocessed && speech.wanted() {
                let frame = probe.observe(&capture.samples, input_peak_dbfs);
                if let Some(probability) = frame.probability {
                    metrics.vad_probability = Some(probability);
                }
                if frame.confirmed {
                    speech.report();
                }
            }
            let output = match decision.source {
                OutputSource::Processed => processed,
                OutputSource::AlignedRaw => aligned_raw,
                OutputSource::Silence => [0.0; DSP_SAMPLES_PER_FRAME],
            };
            window.observe_output_peak(peak_dbfs(&output));
            if !self.output.push_block(output) {
                metrics.output_queue_overflows = self.output.counters().queue_overflows;
                health.warn(
                    "virtual output queue overflowed; clock controller is recovering".to_owned(),
                );
            }
            metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);
            let processing_elapsed = processing_started.elapsed();
            self.processing_histogram.observe(processing_elapsed);
            let current_backlog_events = dsp_backlog_events(&self.source);
            let backlog_fault = current_backlog_events > last_backlog_events
                && started.elapsed() >= CONVERGENCE_TIME;
            last_backlog_events = current_backlog_events;
            if processing_elapsed.as_micros() > u128::from(PROCESSING_BUDGET_US) {
                metrics.processing_budget_exceeded_blocks =
                    metrics.processing_budget_exceeded_blocks.saturating_add(1);
            }
            let overload_transition = (!self.safe_mode && !health.failed)
                .then(|| overload.observe(processing_elapsed, backlog_fault))
                .flatten();
            if let Some(transition) = overload_transition {
                if transition.current > transition.previous {
                    metrics.processing_overload_events =
                        metrics.processing_overload_events.saturating_add(1);
                } else {
                    metrics.processing_recovery_events =
                        metrics.processing_recovery_events.saturating_add(1);
                }
                if transition.previous == ProcessingLoadMode::Protection
                    && transition.current != ProcessingLoadMode::Protection
                {
                    match self.reload_processor() {
                        Ok(()) => {
                            health.failed = false;
                            host_delay_estimator.reset();
                            health.recovery_mute_until = Some(Instant::now() + CONVERGENCE_TIME);
                        }
                        Err(error) => health.fail(
                            metrics,
                            format!(
                                "DSP recovery after CPU overload failed; using failure policy: {error}"
                            ),
                        ),
                    }
                } else if transition.current == ProcessingLoadMode::Protection {
                    clear_dsp_metrics(metrics);
                } else if transition.previous == ProcessingLoadMode::Reduced
                    && transition.current == ProcessingLoadMode::Full
                {
                    if let Some(processor) = &mut self.processor {
                        if let Err(error) = processor.reset_agc2() {
                            health.fail(
                                metrics,
                                format!(
                                    "reset AGC2 after CPU recovery failed; using failure policy: {error}"
                                ),
                            );
                        }
                    }
                }
            }
            metrics.processing_load_mode = overload.mode();

            if window.due(Instant::now()) {
                let source_counters = self.source.counters();
                let output_counters = self.output.counters();
                apply_device_metrics(
                    metrics,
                    &source_counters,
                    &output_counters,
                    self.output.info().stream_latency_100ns,
                    self.processing_latency_frames,
                    &mut window,
                );
                metrics.input_peak_dbfs = window.take_input_peak();
                metrics.output_peak_dbfs = window.take_output_peak();
                metrics.processing_time_p99_us = self.processing_histogram.p99_us();
                metrics.processing_time_max_us = self.processing_histogram.max_us();
                metrics.processing_deadline_misses = self.processing_histogram.deadline_misses();
                if overload.mode() == ProcessingLoadMode::Protection {
                    clear_aec_metrics(metrics);
                } else if let Some(processor) = &self.processor {
                    match processor.aec_metrics() {
                        Ok(Some(aec)) => {
                            metrics.aec_estimated_delay_ms = aec.estimated_delay_ms;
                            metrics.aec_echo_return_loss_db = aec.echo_return_loss_db;
                            metrics.aec_echo_return_loss_enhancement_db =
                                aec.echo_return_loss_enhancement_db;
                            metrics.aec_residual_echo_likelihood = aec.residual_echo_likelihood;
                        }
                        Ok(None) => {
                            metrics.aec_estimated_delay_ms = None;
                            metrics.aec_echo_return_loss_db = None;
                            metrics.aec_echo_return_loss_enhancement_db = None;
                            metrics.aec_residual_echo_likelihood = None;
                        }
                        Err(error) => health.warn(format!("read AEC metrics failed: {error}")),
                    }
                }
                lock_unpoisoned(runtime).publish_session(
                    decision.mode,
                    initial_detail(decision.mode),
                    Some(self.active_route()),
                    metrics.clone(),
                    combine_runtime_warnings(
                        health.warning.clone(),
                        overload.mode(),
                        settings.unprocessed_output,
                        DeviceInstability::since(
                            &mut reported_discontinuities,
                            source_counters.data_discontinuities,
                            &mut reported_output_underruns,
                            output_counters.underrun_events,
                        ),
                    ),
                );
            }
        }
    }

    fn reload_processor(&mut self) -> Result<(), String> {
        self.processor = Some(
            DspProcessor::load(
                &self.native_library,
                self.source.info().render_channels,
                &self.effective_settings.dsp,
            )
            .map_err(|error| error.to_string())?,
        );
        Ok(())
    }

    fn stop(self) -> Result<(), PlatformError> {
        let source_result = self.source.stop();
        let output_result = self.output.stop();
        source_result.and(output_result)
    }
}

enum SessionExit {
    DeviceChanged,
    Failed(String),
    Stopped,
}

/// The DSP's health as one value, because reporting a failure is three coupled writes: the flag that
/// diverts the output, the message the UI shows, and the counter. Six call sites used to perform all
/// three by hand, and any one of them could have performed two.
struct DspHealth {
    failed: bool,
    warning: Option<String>,
    /// Set while a rebuilt echo canceller has not reconverged. Distinct from `failed`: the DSP works,
    /// its output just cannot be trusted yet.
    recovery_mute_until: Option<Instant>,
}

impl DspHealth {
    fn fail(&mut self, metrics: &mut EngineMetrics, message: String) {
        self.warning = Some(message);
        self.failed = true;
        metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
    }

    /// Something worth telling the user about that has not stopped the DSP.
    fn warn(&mut self, message: String) {
        self.warning = Some(message);
    }

    fn recovering(&self, now: Instant) -> bool {
        self.recovery_mute_until.is_some_and(|until| now < until)
    }
}

/// What the virtual microphone carries for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSource {
    Processed,
    AlignedRaw,
    Silence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputDecision {
    source: OutputSource,
    mode: EngineMode,
    /// Nothing is being processed at all — safe mode, a DSP failure, or overload protection.
    ///
    /// Deliberately not the same question as `source != Processed`, which the recovery window also
    /// answers yes to. The speech probe listens only when there is nothing to process; it must stay
    /// quiet while a rebuilt canceller is warming up, because that is processing, just not yet
    /// trustworthy.
    unprocessed: bool,
}

/// The one place that answers "what does the other side hear, and what do we call it".
///
/// Both used to be derived independently, 130 lines apart, from the same conditions. That
/// arrangement allowed the tray to report processing while a DSP failure kept
/// the output muted. Deriving them together makes disagreement unrepresentable.
///
/// The asymmetry between the two convergence windows is deliberate. The initial one reports
/// `Converging` while still sending processed audio, because AEC3 converges *on* real audio. The
/// post-recovery one reports `Converging` and sends silence, because that canceller was just rebuilt
/// and unprocessed audio really would leak echo.
///
/// `load` must be the frame's snapshot, taken before `ProcessingOverloadController::observe` may move
/// it — a decision about audio that has already been emitted cannot use a mode that arrived after it.
fn decide(
    safe_mode: bool,
    health: &DspHealth,
    load: ProcessingLoadMode,
    session_elapsed: Duration,
    now: Instant,
    unprocessed_output: UnprocessedOutput,
) -> OutputDecision {
    let unprocessed = safe_mode || health.failed || load == ProcessingLoadMode::Protection;
    let recovering = health.recovering(now);
    let source = if recovering {
        OutputSource::Silence
    } else if unprocessed {
        match unprocessed_output {
            UnprocessedOutput::RawMicrophone => OutputSource::AlignedRaw,
            UnprocessedOutput::Silence => OutputSource::Silence,
        }
    } else {
        OutputSource::Processed
    };
    let mode = if safe_mode {
        EngineMode::SafeMode
    } else if unprocessed {
        unprocessed_engine_mode(unprocessed_output)
    } else if session_elapsed < CONVERGENCE_TIME || recovering {
        EngineMode::Converging
    } else {
        EngineMode::Active
    };
    OutputDecision {
        source,
        mode,
        unprocessed,
    }
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

fn reset_processor_echo_path(processor: &mut Option<DspProcessor>) -> Result<(), String> {
    processor
        .as_mut()
        .map(DspProcessor::reset_echo_path)
        .transpose()
        .map(|_| ())
        .map_err(|error| error.to_string())
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

fn unprocessed_engine_mode(unprocessed_output: UnprocessedOutput) -> EngineMode {
    match unprocessed_output {
        UnprocessedOutput::RawMicrophone => EngineMode::RawFallback,
        UnprocessedOutput::Silence => EngineMode::Muted,
    }
}

fn initial_detail(mode: EngineMode) -> &'static str {
    match mode {
        EngineMode::Converging => "AEC3 is converging",
        EngineMode::Active => "realtime microphone processing is active",
        EngineMode::RawFallback => "not processing; sending the unprocessed microphone",
        EngineMode::SafeMode => "engine safe mode is active; nothing is being processed",
        EngineMode::Muted => "not processing; the virtual microphone is silent",
        _ => "audio engine state changed",
    }
}

fn wait_for_stop(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    stop.load(Ordering::Acquire)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Evidence that the DSP thread itself is falling behind: the source rings only overflow when nothing
/// drains them in time.
///
/// Device-side events are deliberately excluded. Dropped driver packets, output interruptions and
/// output queue overflows say something about the device or its wireless link, not about how fast this
/// process is — a Bluetooth headset that stalls its microphone starves the output no matter how much
/// DSP is bypassed, so feeding those events into the degradation ladder both misdiagnoses the cause
/// and makes the result worse (it drops echo cancellation on exactly the link that needs it). They are
/// reported as device instability instead.
fn dsp_backlog_events(source: &DuplexSourceStream) -> u64 {
    let source = source.counters();
    source
        .render_overflows
        .saturating_add(source.capture_overflows)
}

/// Device-side events counted over one status window. They are reported instead of being treated as
/// CPU overload, because no amount of bypassed DSP makes a stalling device deliver audio on time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DeviceInstability {
    dropped_packets: u64,
    output_interruptions: u64,
}

impl DeviceInstability {
    fn since(
        reported_discontinuities: &mut u64,
        discontinuities: u64,
        reported_output_underruns: &mut u64,
        output_underruns: u64,
    ) -> Self {
        let instability = Self {
            dropped_packets: discontinuities.saturating_sub(*reported_discontinuities),
            output_interruptions: output_underruns.saturating_sub(*reported_output_underruns),
        };
        *reported_discontinuities = discontinuities;
        *reported_output_underruns = output_underruns;
        instability
    }

    fn detected(&self) -> bool {
        self.dropped_packets > 0 || self.output_interruptions > 0
    }
}

fn combine_runtime_warnings(
    dsp_warning: Option<String>,
    load_mode: ProcessingLoadMode,
    unprocessed_output: UnprocessedOutput,
    instability: DeviceInstability,
) -> Option<String> {
    let overload_warning = match (load_mode, unprocessed_output) {
        (ProcessingLoadMode::Full, _) => None,
        (ProcessingLoadMode::Reduced, _) => {
            Some("CPU overload: AGC2 is temporarily bypassed while AEC and noise removal continue".to_owned())
        }
        (ProcessingLoadMode::Protection, UnprocessedOutput::RawMicrophone) => Some(
            "CPU overload protection: sending the unprocessed microphone until realtime scheduling stabilizes".to_owned(),
        ),
        (ProcessingLoadMode::Protection, UnprocessedOutput::Silence) => Some(
            "CPU overload protection: microphone output is silent until realtime scheduling stabilizes".to_owned(),
        ),
    };
    let device_warning = instability.detected().then(|| {
        format!(
            "audio device instability: the driver dropped {} packet(s) and the virtual microphone was interrupted {} time(s) in the last second; this is the device or its wireless link, not CPU load",
            instability.dropped_packets, instability.output_interruptions
        )
    });
    let mut warnings = [dsp_warning, overload_warning, device_warning]
        .into_iter()
        .flatten();
    let first = warnings.next()?;
    Some(warnings.fold(first, |combined, warning| format!("{combined}; {warning}")))
}

/// Coalesces echo path rebuild requests. Every request is honoured within one interval, and a device
/// that drops packets continuously cannot make AEC3 spend the session reinitializing.
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

/// The one-second publishing window both session loops keep: when to publish next, plus the peaks and
/// the backlog high-water mark accumulated since the last publish.
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

    /// True once per interval, and starts the next window. The timestamp comes from the caller so this
    /// is testable without sleeping; starting the next window here rather than after the publish keeps
    /// the cadence from drifting by however long publishing took.
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

    /// `None` when nothing was ever observed, which is how a session with no output at all reports
    /// that it has no output peak rather than echoing the input's.
    fn take_output_peak(&mut self) -> Option<f32> {
        self.output_peak_dbfs.take()
    }

    fn take_backlog_blocks(&mut self) -> usize {
        std::mem::take(&mut self.backlog_blocks)
    }
}

/// The part of the one-second publish that is identical in both session loops: device counters and the
/// latency breakdown. Peaks stay with the callers — what an output peak means differs between them.
fn apply_device_metrics(
    metrics: &mut EngineMetrics,
    source: &SourceCountersSnapshot,
    output: &OutputCountersSnapshot,
    output_stream_latency_100ns: u64,
    processing_latency_frames: usize,
    window: &mut StatusWindow,
) {
    // A capture-only source never increments the render counters, so this stays 0 for an idle session.
    metrics.render_overflows = source.render_overflows;
    metrics.capture_overflows = source.capture_overflows;
    metrics.device_discontinuities = source.data_discontinuities;
    metrics.output_underruns = output.underrun_events;
    metrics.output_queue_overflows = output.queue_overflows;
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

/// Added microphone latency the user can act on, split into the segment that is measured on the way
/// in, the deterministic DSP look-ahead, and the segment measured on the way out. The virtual cable's
/// internal buffering and the voice app's jitter buffer are not observable from here.
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
    // The frame the worker is about to process does not count as backlog.
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
    metrics.vad_probability = None;
    metrics.speech_vad_active = None;
    metrics.vad_gate_open = None;
    metrics.echo_guard_active = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_interleave_preserves_stereo_order() {
        let mut frame = RenderFrame {
            timestamp: xmic_engine_core::FrameTimestamp {
                device_position_frames: 0,
                qpc_position_100ns: 0,
            },
            channels: 2,
            samples: [[0.0; DSP_SAMPLES_PER_FRAME]; MAX_RENDER_CHANNELS],
        };
        frame.samples[0][0] = 0.25;
        frame.samples[1][0] = -0.5;
        let mut output = [0.0; DSP_SAMPLES_PER_FRAME * MAX_RENDER_CHANNELS];

        interleave_render(&frame, &mut output);

        assert_eq!(&output[..2], &[0.25, -0.5]);
    }

    #[test]
    fn discontinuity_counter_change_is_reported_once() {
        let mut previous = 3;

        assert!(counter_changed(&mut previous, 4));
        assert!(!counter_changed(&mut previous, 4));
        assert!(counter_changed(&mut previous, 6));
    }

    #[test]
    fn peak_level_reports_silence_and_full_scale() {
        assert_eq!(peak_dbfs(&[0.0, 0.0]), -120.0);
        assert_eq!(peak_dbfs(&[-1.0, 0.25]), 0.0);
    }

    #[test]
    fn peak_level_windows_reset_after_publishing() {
        let mut window = None;

        window = max_level(window, peak_dbfs(&[0.5]));
        window = max_level(window, peak_dbfs(&[0.1]));
        let published = window.take();

        assert_eq!(published, Some(peak_dbfs(&[0.5])));
        assert_eq!(window, None);
        assert_eq!(
            max_level(window, peak_dbfs(&[0.1])),
            Some(peak_dbfs(&[0.1]))
        );
    }

    #[test]
    fn pipeline_latency_counts_look_ahead_capture_and_output_queue() {
        let natural = processing_latency_frames(&DspSettings::default()).unwrap();
        let output = OutputCountersSnapshot {
            queue_depth_frames: (DSP_SAMPLES_PER_FRAME * 3) as u64,
            endpoint_padding_frames: 480,
            ..OutputCountersSnapshot::default()
        };

        // 12 ms measured capture lag, one backlog block beyond the frame in flight.
        let breakdown = latency_breakdown(natural, 120_000, 2, &output, 30_000);

        assert_eq!(natural, 8);
        assert_eq!(breakdown.capture_ms, 22.0);
        assert_eq!(breakdown.dsp_ms, 80.0);
        assert_eq!(breakdown.output_ms, 43.0);
        assert_eq!(breakdown.total_ms(), 145.0);
    }

    #[test]
    fn pipeline_latency_ignores_the_frame_in_flight_and_missing_measurements() {
        let output = OutputCountersSnapshot::default();

        let idle = latency_breakdown(8, 0, 1, &output, 0);

        assert_eq!(idle.capture_ms, 0.0);
        assert_eq!(idle.output_ms, 0.0);
        assert_eq!(idle.total_ms(), 80.0);
    }

    #[test]
    fn latency_breakdown_publishes_every_segment() {
        let mut metrics = EngineMetrics::default();
        let breakdown = LatencyBreakdown {
            capture_ms: 11.0,
            dsp_ms: 80.0,
            output_ms: 32.0,
        };

        breakdown.apply(&mut metrics);

        assert_eq!(metrics.capture_latency_ms, 11.0);
        assert_eq!(metrics.dsp_latency_ms, 80.0);
        assert_eq!(metrics.output_latency_ms, 32.0);
        assert_eq!(metrics.pipeline_latency_ms, 123.0);
    }

    #[test]
    fn overload_protection_warning_follows_the_configured_unprocessed_output() {
        let silent = combine_runtime_warnings(
            None,
            ProcessingLoadMode::Protection,
            UnprocessedOutput::Silence,
            DeviceInstability::default(),
        )
        .unwrap();
        let raw = combine_runtime_warnings(
            None,
            ProcessingLoadMode::Protection,
            UnprocessedOutput::RawMicrophone,
            DeviceInstability::default(),
        )
        .unwrap();

        assert!(silent.contains("silent"));
        assert!(raw.contains("unprocessed"));
        assert_eq!(
            combine_runtime_warnings(
                None,
                ProcessingLoadMode::Full,
                UnprocessedOutput::Silence,
                DeviceInstability::default()
            ),
            None
        );
    }

    #[test]
    fn device_instability_is_reported_as_the_device_rather_than_cpu_load() {
        let mut discontinuities = 40;
        let mut output_underruns = 7;

        let instability =
            DeviceInstability::since(&mut discontinuities, 43, &mut output_underruns, 9);

        assert_eq!(instability.dropped_packets, 3);
        assert_eq!(instability.output_interruptions, 2);
        assert_eq!(discontinuities, 43);
        assert_eq!(output_underruns, 9);

        let warning = combine_runtime_warnings(
            None,
            ProcessingLoadMode::Full,
            UnprocessedOutput::RawMicrophone,
            instability,
        )
        .unwrap();
        assert!(warning.contains("audio device instability"));
        assert!(warning.contains("not CPU load"));

        // A quiet window says nothing at all.
        assert!(
            !DeviceInstability::since(&mut discontinuities, 43, &mut output_underruns, 9)
                .detected()
        );
    }

    #[test]
    fn echo_path_rebuilds_are_coalesced_but_never_dropped() {
        let start = Instant::now();
        let mut limiter = EchoPathResetLimiter::default();

        assert!(!limiter.take_due(start));

        // The first request is honoured immediately.
        limiter.request();
        assert!(limiter.take_due(start));
        assert!(!limiter.take_due(start));

        // A storm of requests inside the interval collapses into one rebuild, and that rebuild still
        // happens — a discontinuity is never silently ignored.
        for _ in 0..50 {
            limiter.request();
            assert!(!limiter.take_due(start + Duration::from_millis(100)));
        }
        assert!(limiter.take_due(start + ECHO_PATH_RESET_MIN_INTERVAL));
        assert!(!limiter.take_due(start + ECHO_PATH_RESET_MIN_INTERVAL * 4));
    }

    #[test]
    fn an_idle_session_only_runs_while_the_permit_allows_it() {
        let mut control = control_state(UnprocessedOutput::RawMicrophone);

        assert_eq!(desired_idle_mode(&control), None);

        control.permit.bypass_allowed = true;
        assert_eq!(desired_idle_mode(&control), Some(IdleSessionMode::Forward));

        control.settings.unprocessed_output = UnprocessedOutput::Silence;
        assert_eq!(desired_idle_mode(&control), None);

        // Silence opens the microphone for one reason only: the notice that has not been shown yet.
        control.permit.speech_notice_needed = true;
        assert_eq!(desired_idle_mode(&control), Some(IdleSessionMode::Listen));
    }

    #[test]
    fn a_route_that_keeps_failing_stops_being_retried() {
        let mut control = control_state(UnprocessedOutput::RawMicrophone);
        control.permit.bypass_allowed = true;

        control.idle_failures = IDLE_SESSION_FAILURES_BEFORE_GIVING_UP - 1;
        assert_eq!(desired_idle_mode(&control), Some(IdleSessionMode::Forward));

        control.idle_failures = IDLE_SESSION_FAILURES_BEFORE_GIVING_UP;
        assert_eq!(desired_idle_mode(&control), None);
    }

    #[test]
    fn the_grace_period_covers_the_stop_apply_restart_cycle() {
        let mut control = control_state(UnprocessedOutput::RawMicrophone);

        control.idle_since = Some(Instant::now());
        assert!(!idle_grace_elapsed(&control));

        control.idle_since = Some(Instant::now() - IDLE_SESSION_GRACE);
        assert!(idle_grace_elapsed(&control));
    }

    #[test]
    fn safe_mode_and_protection_report_the_configured_unprocessed_output() {
        assert_eq!(
            unprocessed_engine_mode(UnprocessedOutput::RawMicrophone),
            EngineMode::RawFallback
        );
        assert_eq!(
            unprocessed_engine_mode(UnprocessedOutput::Silence),
            EngineMode::Muted
        );
    }

    #[test]
    fn a_session_with_no_output_reports_no_output_peak() {
        // Listen mode opens the microphone only to hear speech nobody is processing. There is no
        // output, so reporting the input's peak as the output's would be a lie — and the idle loop used
        // to keep that distinction in an `Option::and` at the publish site rather than in the window.
        let now = Instant::now();
        let mut listening = StatusWindow::new(now);
        listening.observe_input_peak(-12.0);

        assert_eq!(listening.take_input_peak(), Some(-12.0));
        assert_eq!(listening.take_output_peak(), None);

        let mut forwarding = StatusWindow::new(now);
        forwarding.observe_input_peak(-12.0);
        forwarding.observe_output_peak(-12.0);

        assert_eq!(forwarding.take_output_peak(), Some(-12.0));
    }

    #[test]
    fn the_window_keeps_high_water_marks_and_empties_on_publish() {
        let mut window = StatusWindow::new(Instant::now());
        for peak in [-30.0, -6.0, -20.0] {
            window.observe_input_peak(peak);
        }
        for blocks in [2, 5, 1] {
            window.observe_backlog(blocks);
        }

        assert_eq!(window.take_input_peak(), Some(-6.0));
        assert_eq!(window.take_backlog_blocks(), 5);

        // The next window starts empty, or a quiet second would report the previous one's peak.
        assert_eq!(window.take_input_peak(), None);
        assert_eq!(window.take_backlog_blocks(), 0);
    }

    #[test]
    fn the_window_publishes_once_per_interval() {
        let start = Instant::now();
        let mut window = StatusWindow::new(start);

        assert!(!window.due(start));
        assert!(!window.due(start + STATUS_UPDATE_INTERVAL - Duration::from_millis(1)));
        assert!(window.due(start + STATUS_UPDATE_INTERVAL));
        // Publishing restarts the window instead of letting the next one fall due immediately.
        assert!(!window.due(start + STATUS_UPDATE_INTERVAL));
        assert!(window.due(start + STATUS_UPDATE_INTERVAL * 2));
    }

    fn snapshot_with_profile() -> RuntimeSnapshot {
        let mut snapshot = RuntimeSnapshot::default();
        snapshot.set_profile(
            ActiveDeviceProfile {
                render_endpoint_id: "cable-input".to_owned(),
                capture_endpoint_id: "microphone".to_owned(),
                customized: true,
            },
            DspSettings::default(),
        );
        snapshot
    }

    /// Editing the DSP settings of the pair that was last active has to keep working after the engine
    /// stops: `status()` reports `effective_dsp` as the settings and `update_settings` writes back into
    /// the profile `device_profile` names. That feature is nothing more than these two fields surviving
    /// the publish calls below, which is not obvious from reading them — it is the absence of two
    /// assignments.
    #[test]
    fn stopping_a_session_keeps_the_device_profile_editable() {
        let mut snapshot = snapshot_with_profile();

        snapshot.publish_session(
            EngineMode::Sleeping,
            "audio engine is stopped",
            None,
            EngineMetrics::default(),
            None,
        );

        assert!(snapshot.device_profile.is_some());
        assert!(snapshot.effective_dsp.is_some());

        // And while the worker is between retries.
        snapshot.fail("the realtime worker exited unexpectedly");

        assert!(snapshot.device_profile.is_some());
        assert!(snapshot.effective_dsp.is_some());
    }

    #[test]
    fn an_unusable_route_drops_the_device_profile() {
        let mut snapshot = snapshot_with_profile();

        snapshot.require_configuration(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
        });

        // The profile was resolved for a pair this route can no longer use.
        assert!(snapshot.device_profile.is_none());
        assert!(snapshot.effective_dsp.is_none());
    }

    /// A reconnect must not reset the counters the UI has been showing.
    #[test]
    fn a_failed_session_keeps_the_metrics_it_had() {
        let mut snapshot = RuntimeSnapshot::default();
        snapshot.publish_session(
            EngineMode::Active,
            "realtime microphone processing is active",
            None,
            EngineMetrics {
                processed_blocks: 4_242,
                ..EngineMetrics::default()
            },
            None,
        );

        snapshot.fail("a WASAPI worker stopped unexpectedly");

        assert_eq!(snapshot.metrics.processed_blocks, 4_242);
    }

    fn health(failed: bool, recovery_mute_until: Option<Instant>) -> DspHealth {
        DspHealth {
            failed,
            warning: None,
            recovery_mute_until,
        }
    }

    /// Exhausts every state `decide` can be asked about and asserts the relationships that have to
    /// hold, rather than a table of expected answers.
    ///
    /// The first invariant is the one that matters: earlier builds could leave the tray
    /// reporting processing while a DSP failure kept the output muted. Nothing at the time could have
    /// caught that, because the two answers were derived independently 130 lines apart.
    #[test]
    fn the_reported_mode_never_lies_about_what_goes_out() {
        let now = Instant::now();
        let mut combinations = 0;
        for safe_mode in [false, true] {
            for failed in [false, true] {
                for load in [
                    ProcessingLoadMode::Full,
                    ProcessingLoadMode::Reduced,
                    ProcessingLoadMode::Protection,
                ] {
                    for recovery_mute_until in [None, Some(now + Duration::from_secs(1))] {
                        for session_elapsed in [Duration::from_millis(500), CONVERGENCE_TIME * 3] {
                            for unprocessed_output in
                                [UnprocessedOutput::RawMicrophone, UnprocessedOutput::Silence]
                            {
                                combinations += 1;
                                let decision = decide(
                                    safe_mode,
                                    &health(failed, recovery_mute_until),
                                    load,
                                    session_elapsed,
                                    now,
                                    unprocessed_output,
                                );
                                let context = format!(
                                    "safe_mode={safe_mode} failed={failed} load={load:?} recovering={} elapsed={session_elapsed:?} unprocessed_output={unprocessed_output:?}",
                                    recovery_mute_until.is_some()
                                );

                                // Reporting "active" has to mean the processed signal is what left.
                                if decision.mode == EngineMode::Active {
                                    assert_eq!(
                                        decision.source,
                                        OutputSource::Processed,
                                        "{context}"
                                    );
                                }
                                // A rebuilt canceller has not reconverged, so nothing goes out at all.
                                if recovery_mute_until.is_some() {
                                    assert_eq!(decision.source, OutputSource::Silence, "{context}");
                                }
                                // Not processing and not warming up: the configured output decides.
                                if decision.unprocessed && recovery_mute_until.is_none() {
                                    let expected = match unprocessed_output {
                                        UnprocessedOutput::RawMicrophone => {
                                            OutputSource::AlignedRaw
                                        }
                                        UnprocessedOutput::Silence => OutputSource::Silence,
                                    };
                                    assert_eq!(decision.source, expected, "{context}");
                                }
                                // Safe mode outranks every other explanation.
                                if safe_mode {
                                    assert_eq!(decision.mode, EngineMode::SafeMode, "{context}");
                                }
                                // The flag the speech probe reads and the mode the UI shows have to
                                // tell the same story about whether anything is being processed.
                                assert_eq!(
                                    decision.unprocessed,
                                    matches!(
                                        decision.mode,
                                        EngineMode::SafeMode
                                            | EngineMode::RawFallback
                                            | EngineMode::Muted
                                    ),
                                    "{context}"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(combinations, 96);
    }

    /// The one place the two convergence windows deliberately disagree. Both report `Converging`, but
    /// the initial one keeps sending processed audio because AEC3 converges *on* real audio, while the
    /// post-recovery one mutes because the canceller it just rebuilt would leak echo.
    #[test]
    fn only_the_post_recovery_window_mutes_while_converging() {
        let now = Instant::now();

        let initial = decide(
            false,
            &health(false, None),
            ProcessingLoadMode::Full,
            Duration::from_millis(500),
            now,
            UnprocessedOutput::Silence,
        );
        let after_recovery = decide(
            false,
            &health(false, Some(now + Duration::from_secs(1))),
            ProcessingLoadMode::Full,
            CONVERGENCE_TIME * 3,
            now,
            UnprocessedOutput::Silence,
        );

        assert_eq!(
            (initial.source, initial.mode),
            (OutputSource::Processed, EngineMode::Converging)
        );
        assert_eq!(
            (after_recovery.source, after_recovery.mode),
            (OutputSource::Silence, EngineMode::Converging)
        );
    }

    #[test]
    fn a_dsp_failure_is_always_reported_three_ways() {
        let mut metrics = EngineMetrics::default();
        let mut state = health(false, None);

        state.fail(&mut metrics, "boom".to_owned());

        assert!(state.failed);
        assert_eq!(state.warning.as_deref(), Some("boom"));
        assert_eq!(metrics.dsp_failures, 1);

        // A warning is not a failure: it replaces the message and touches nothing else.
        state.warn("still noisy".to_owned());

        assert_eq!(state.warning.as_deref(), Some("still noisy"));
        assert!(state.failed);
        assert_eq!(metrics.dsp_failures, 1);
    }

    fn control_state(unprocessed_output: UnprocessedOutput) -> ControlState {
        ControlState {
            settings: EngineSettings {
                unprocessed_output,
                output_endpoint_id: Some("cable-input".to_owned()),
                ..EngineSettings::default()
            },
            worker: None,
            idle: None,
            permit: IdleSessionPermit::default(),
            idle_since: Some(Instant::now() - IDLE_SESSION_GRACE),
            idle_failures: 0,
        }
    }

    #[test]
    fn clipping_counter_only_counts_near_full_scale_samples() {
        assert_eq!(clipped_sample_count(&[-1.0, -0.999, 0.998, 0.999, 1.0]), 4);
    }

    #[test]
    fn dsp_feature_toggle_changes_only_the_selected_stage() {
        let mut settings = EngineSettings::default();

        assert!(!DspFeature::Aec.toggle(&mut settings));
        assert!(!settings.dsp.agc2.enabled);
        assert!(settings.dsp.vad_gate.enabled);

        assert!(!DspFeature::VadGate.toggle(&mut settings));
        assert!(!DspFeature::Rnnoise.toggle(&mut settings));
        assert!(!settings.dsp.aec.enabled);
        assert!(!settings.dsp.vad_gate.enabled);

        assert!(DspFeature::Agc2.toggle(&mut settings));
        assert!(!settings.dsp.aec.enabled);
        assert!(!settings.dsp.rnnoise.enabled);

        DspFeature::Rnnoise.set_enabled(&mut settings, true);
        assert!(DspFeature::Rnnoise.enabled(&settings));
        assert!(settings.dsp.agc2.enabled);
        assert!(!settings.dsp.vad_gate.enabled);
    }

    #[test]
    fn enabling_agc2_arms_the_adaptive_gain() {
        let mut settings = EngineSettings::default();
        settings.dsp.agc2.adaptive_digital_enabled = false;

        DspFeature::Agc2.set_enabled(&mut settings, true);

        assert!(settings.dsp.agc2.adaptive_digital_enabled);
        settings.validate().unwrap();
    }

    #[test]
    fn dsp_feature_deserializes_frontend_identifiers() {
        assert_eq!(
            serde_json::from_str::<DspFeature>(r#""rnnoise""#).unwrap(),
            DspFeature::Rnnoise
        );
        assert_eq!(
            serde_json::from_str::<DspFeature>(r#""vadGate""#).unwrap(),
            DspFeature::VadGate
        );
    }
}
