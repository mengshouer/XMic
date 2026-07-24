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
    RenderFrame, DSP_SAMPLES_PER_FRAME, MAX_RENDER_CHANNELS,
};
use xmic_engine_protocol::{
    ActiveDeviceProfile, ActiveRoute, DelayCalibrationState as ProtocolDelayCalibrationState,
    DspFailurePolicy, DspSettings, EngineMetrics, EngineMode, EngineSettings, EngineStatus,
    ProcessingLoadMode, PROTOCOL_VERSION,
};
use xmic_engine_runtime::{processing_latency_frames, AlignedRawPath, DspProcessor};
use xmic_platform_api::{EndpointDescriptor, PlatformError};
use xmic_platform_windows::{
    register_realtime_audio_thread, start_device_change_monitor, start_duplex_source,
    start_output_sink, DeviceChangeMonitor, DuplexSourceRequest, DuplexSourceStream,
    OutputSinkRequest, OutputSinkStream,
};

use crate::{device_profiles::DeviceProfileStore, settings_store::SettingsStore};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const RESTART_BACKOFF: Duration = Duration::from_secs(1);
const CONVERGENCE_TIME: Duration = Duration::from_secs(2);
const STATUS_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const PROCESSING_DEADLINE_US: u64 = 10_000;
const PROCESSING_BUDGET_US: u64 = 4_000;
const PROCESSING_HISTOGRAM_BIN_US: u64 = 50;
const PROCESSING_HISTOGRAM_BINS: usize = 4_096;
const OVERLOAD_WINDOW_FRAMES: u32 = 200;
const OVERLOAD_BUDGET_BLOCKS_PER_WINDOW: u32 = 10;
const OVERLOAD_DEADLINES_PER_WINDOW: u32 = 2;
const OVERLOAD_TIMING_FAULTS_PER_WINDOW: u32 = 2;
const OVERLOAD_WINDOWS_TO_ESCALATE: u32 = 2;
const STABLE_WINDOWS_TO_RECOVER: u32 = 5;

pub struct RealtimeEngineController {
    control: Mutex<ControlState>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    native_library: PathBuf,
    settings_store: SettingsStore,
    profile_store: Arc<DeviceProfileStore>,
    safe_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DspFeature {
    Aec,
    Agc2,
    VadGate,
}

impl DspFeature {
    pub(crate) fn toggle(self, settings: &mut EngineSettings) -> bool {
        let enabled = !match self {
            Self::Aec => settings.dsp.aec.enabled,
            Self::Agc2 => settings.dsp.agc2.enabled,
            Self::VadGate => settings.dsp.vad_gate.enabled,
        };
        match self {
            Self::Aec => settings.dsp.aec.enabled = enabled,
            Self::Agc2 => settings.dsp.agc2.enabled = enabled,
            Self::VadGate => settings.dsp.vad_gate.enabled = enabled,
        }
        enabled
    }
}

struct ControlState {
    settings: EngineSettings,
    worker: Option<EngineWorker>,
}

struct EngineWorker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

#[derive(Clone)]
struct RuntimeSnapshot {
    mode: EngineMode,
    detail: String,
    active_route: Option<ActiveRoute>,
    metrics: EngineMetrics,
    warning: Option<String>,
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
            device_profile: None,
            effective_dsp: None,
        }
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
            }),
            runtime: Arc::new(Mutex::new(RuntimeSnapshot::default())),
            native_library,
            settings_store,
            profile_store: Arc::new(profile_store),
            safe_mode,
        }
    }

    pub fn status(&self, endpoints: Vec<EndpointDescriptor>) -> EngineStatus {
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
            endpoints,
            settings,
            active_route: runtime.active_route,
            active_device_profile,
            metrics: runtime.metrics,
            warning: runtime.warning,
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
        control.settings = settings;
        Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        reap_finished_worker(&mut control);
        if control.worker.is_some() {
            return Err("audio engine is already running".to_owned());
        }
        control
            .settings
            .validate()
            .map_err(|error| error.to_string())?;
        if control.settings.output_endpoint_id.is_none() {
            return Err("select a virtual microphone output endpoint first".to_owned());
        }

        replace_runtime(
            &self.runtime,
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
                    ready_tx,
                )
            })
            .map_err(|error| {
                let message = format!("start audio engine worker: {error}");
                set_worker_error(&self.runtime, &message);
                message
            })?;
        control.worker = Some(EngineWorker { stop, handle });

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                request_stop_without_join(control.worker.take());
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                request_stop_without_join(control.worker.take());
                replace_runtime(
                    &self.runtime,
                    EngineMode::Error,
                    "audio engine startup timed out",
                    None,
                    EngineMetrics::default(),
                    Some("WASAPI did not complete startup within 15 seconds".to_owned()),
                );
                Err("audio engine startup timed out".to_owned())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                request_stop_without_join(control.worker.take());
                Err("audio engine worker stopped during startup".to_owned())
            }
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut control = lock_unpoisoned(&self.control);
        let Some(worker) = control.worker.take() else {
            replace_runtime(
                &self.runtime,
                EngineMode::Sleeping,
                "audio engine is stopped",
                None,
                EngineMetrics::default(),
                None,
            );
            return Ok(());
        };
        if let Err(error) = stop_worker_with_timeout(worker) {
            replace_runtime(
                &self.runtime,
                EngineMode::Error,
                "audio engine stop timed out",
                None,
                EngineMetrics::default(),
                Some(error.clone()),
            );
            return Err(error);
        }
        replace_runtime(
            &self.runtime,
            EngineMode::Sleeping,
            "audio engine is stopped",
            None,
            EngineMetrics::default(),
            None,
        );
        Ok(())
    }
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

fn run_worker(
    settings: EngineSettings,
    native_library: PathBuf,
    profile_store: Arc<DeviceProfileStore>,
    safe_mode: bool,
    stop: Arc<AtomicBool>,
    runtime: Arc<Mutex<RuntimeSnapshot>>,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let _realtime_thread = match register_realtime_audio_thread() {
        Ok(registration) => registration,
        Err(error) => {
            let message = format!("register DSP thread with MMCSS: {error}");
            set_worker_error(&runtime, &message);
            let _ = ready.send(Err(message));
            return;
        }
    };
    let monitor = match start_device_change_monitor() {
        Ok(monitor) => monitor,
        Err(error) => {
            let message = format!("start device change monitor: {error}");
            set_worker_error(&runtime, &message);
            let _ = ready.send(Err(message));
            return;
        }
    };
    let mut first_session = true;
    let mut metrics = EngineMetrics::default();

    while !stop.load(Ordering::Acquire) {
        replace_runtime(
            &runtime,
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
                set_worker_error(&runtime, &message);
                if first_session {
                    let _ = ready.send(Err(message));
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

        set_runtime_profile(
            &runtime,
            session.device_profile.clone(),
            session.effective_settings.dsp.clone(),
        );
        let (initial_mode, warning) = session.initial_state();
        replace_runtime(
            &runtime,
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
        );
        let stop_result = session.stop();
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = stop_result {
            set_worker_error(&runtime, &format!("stop realtime session: {error}"));
        }
        metrics.device_restarts = metrics.device_restarts.saturating_add(1);
        match exit {
            SessionExit::DeviceChanged => {
                replace_runtime(
                    &runtime,
                    EngineMode::Starting,
                    "audio endpoint changed; rebuilding the session",
                    None,
                    metrics.clone(),
                    None,
                );
            }
            SessionExit::Failed(error) => {
                set_worker_error(&runtime, &error);
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
                match self.effective_settings.failure_policy {
                    DspFailurePolicy::RawPassthrough => EngineMode::RawFallback,
                    DspFailurePolicy::Mute => EngineMode::Muted,
                },
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
    ) -> SessionExit {
        let started = Instant::now();
        let mut last_status_update = Instant::now();
        let mut interleaved_render = [0.0; DSP_SAMPLES_PER_FRAME * MAX_RENDER_CHANNELS];
        let mut processed = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut aligned_raw = [0.0; DSP_SAMPLES_PER_FRAME];
        let mut dsp_warning = self.initial_warning.clone();
        let mut dsp_failed = self.processor.is_none();
        let mut gate_was_open = false;
        let mut echo_guard_was_active = false;
        let mut nearend_candidate_was_active = false;
        let mut host_delay_estimator = HostDelayEstimator::default();
        let mut last_data_discontinuities = self.source.counters().data_discontinuities;
        let mut overload = ProcessingOverloadController::new(settings.dsp.agc2.enabled);
        metrics.processing_load_mode = ProcessingLoadMode::Full;
        let mut recovery_mute_until = None;
        let mut last_timing_faults = realtime_timing_faults(&self.source, &self.output);

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

            let Some(capture) = self.source.pop_capture() else {
                thread::park_timeout(Duration::from_millis(20));
                continue;
            };
            let processing_started = Instant::now();
            let source_counters = self.source.counters();
            if counter_changed(
                &mut last_data_discontinuities,
                source_counters.data_discontinuities,
            ) {
                host_delay_estimator.reset();
                metrics.host_delay_reset_events = metrics.host_delay_reset_events.saturating_add(1);
                if !dsp_failed && overload.mode() != ProcessingLoadMode::Protection {
                    if let Err(error) = reset_processor_echo_path(&mut self.processor) {
                        dsp_warning = Some(format!(
                            "DSP reset after a WASAPI discontinuity failed; using failure policy: {error}"
                        ));
                        dsp_failed = true;
                        metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                    }
                }
            }
            metrics.input_peak_dbfs =
                max_level(metrics.input_peak_dbfs, peak_dbfs(&capture.samples));
            metrics.input_clipped_samples = metrics
                .input_clipped_samples
                .saturating_add(clipped_sample_count(&capture.samples));
            let mut render_blocks = 0u64;
            let mut latest_render_qpc_position_100ns = None;
            let mut latest_render_sample_count = 0usize;
            while let Some(render) = self.source.pop_render() {
                render_blocks += 1;
                if !dsp_failed && overload.mode() != ProcessingLoadMode::Protection {
                    interleave_render(&render, &mut interleaved_render);
                    let sample_count = DSP_SAMPLES_PER_FRAME * render.channels;
                    if let Some(processor) = &mut self.processor {
                        match processor.process_render(&interleaved_render[..sample_count]) {
                            Ok(()) => {
                                latest_render_qpc_position_100ns =
                                    Some(render.timestamp.qpc_position_100ns);
                                latest_render_sample_count = sample_count;
                            }
                            Err(error) => {
                                dsp_warning = Some(format!(
                                    "DSP render processing failed; using failure policy: {error}"
                                ));
                                dsp_failed = true;
                                metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                            }
                        }
                    }
                }
            }
            if render_blocks == 0 {
                metrics.render_underruns = metrics.render_underruns.saturating_add(1);
            }

            let mut host_delay_observation = host_delay_estimator.current();
            if !dsp_failed && overload.mode() != ProcessingLoadMode::Protection {
                if let Some(render_qpc_position_100ns) = latest_render_qpc_position_100ns {
                    host_delay_observation = host_delay_estimator.observe(
                        capture.timestamp.qpc_position_100ns,
                        render_qpc_position_100ns,
                    );
                    if host_delay_observation.reset_required {
                        metrics.host_delay_reset_events =
                            metrics.host_delay_reset_events.saturating_add(1);
                        match reset_processor_echo_path(&mut self.processor) {
                            Ok(()) => {
                                if let Some(processor) = &mut self.processor {
                                    if let Err(error) = processor.process_render(
                                        &interleaved_render[..latest_render_sample_count],
                                    ) {
                                        dsp_warning = Some(format!(
                                            "DSP render processing after a timing reset failed; using failure policy: {error}"
                                        ));
                                        dsp_failed = true;
                                        metrics.dsp_failures =
                                            metrics.dsp_failures.saturating_add(1);
                                    }
                                }
                            }
                            Err(error) => {
                                dsp_warning = Some(format!(
                                    "DSP timing reset failed; using failure policy: {error}"
                                ));
                                dsp_failed = true;
                                metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                            }
                        }
                    }
                }
            }
            update_host_delay_metrics(metrics, host_delay_observation);

            self.raw_path
                .process_frame(&capture.samples, &mut aligned_raw);
            let mut dsp_frame_metrics = None;
            if !dsp_failed && overload.mode() != ProcessingLoadMode::Protection {
                if let Some(processor) = &mut self.processor {
                    match processor.process_capture_with_options(
                        &capture.samples,
                        &mut processed,
                        host_delay_observation.stream_delay_ms,
                        overload.mode() == ProcessingLoadMode::Reduced,
                    ) {
                        Ok(frame_metrics) => dsp_frame_metrics = Some(frame_metrics),
                        Err(error) => {
                            dsp_warning = Some(format!(
                                "DSP capture processing failed; using failure policy: {error}"
                            ));
                            dsp_failed = true;
                            metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                        }
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
            let recovery_muted = recovery_mute_until.is_some_and(|until| Instant::now() < until);
            let output = if self.safe_mode
                || overload.mode() == ProcessingLoadMode::Protection
                || recovery_muted
            {
                [0.0; DSP_SAMPLES_PER_FRAME]
            } else if dsp_failed {
                match settings.failure_policy {
                    DspFailurePolicy::RawPassthrough => aligned_raw,
                    DspFailurePolicy::Mute => [0.0; DSP_SAMPLES_PER_FRAME],
                }
            } else {
                processed
            };
            metrics.output_peak_dbfs = max_level(metrics.output_peak_dbfs, peak_dbfs(&output));
            if !self.output.push_block(output) {
                metrics.output_queue_overflows = self.output.counters().queue_overflows;
                dsp_warning = Some(
                    "virtual output queue overflowed; clock controller is recovering".to_owned(),
                );
            }
            metrics.processed_blocks = metrics.processed_blocks.saturating_add(1);
            let processing_elapsed = processing_started.elapsed();
            self.processing_histogram.observe(processing_elapsed);
            let current_timing_faults = realtime_timing_faults(&self.source, &self.output);
            let timing_fault =
                current_timing_faults > last_timing_faults && started.elapsed() >= CONVERGENCE_TIME;
            last_timing_faults = current_timing_faults;
            if processing_elapsed.as_micros() > u128::from(PROCESSING_BUDGET_US) {
                metrics.processing_budget_exceeded_blocks =
                    metrics.processing_budget_exceeded_blocks.saturating_add(1);
            }
            let overload_transition = (!self.safe_mode && !dsp_failed)
                .then(|| overload.observe(processing_elapsed, timing_fault))
                .flatten();
            if let Some(transition) = overload_transition {
                metrics.processing_load_mode = transition.current;
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
                            dsp_failed = false;
                            host_delay_estimator.reset();
                            recovery_mute_until = Some(Instant::now() + CONVERGENCE_TIME);
                        }
                        Err(error) => {
                            dsp_warning = Some(format!(
                                "DSP recovery after CPU overload failed; using failure policy: {error}"
                            ));
                            dsp_failed = true;
                            metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                        }
                    }
                } else if transition.current == ProcessingLoadMode::Protection {
                    clear_dsp_metrics(metrics);
                } else if transition.previous == ProcessingLoadMode::Reduced
                    && transition.current == ProcessingLoadMode::Full
                {
                    if let Some(processor) = &mut self.processor {
                        if let Err(error) = processor.reset_agc2() {
                            dsp_warning = Some(format!(
                                "reset AGC2 after CPU recovery failed; using failure policy: {error}"
                            ));
                            dsp_failed = true;
                            metrics.dsp_failures = metrics.dsp_failures.saturating_add(1);
                        }
                    }
                }
            }
            metrics.processing_load_mode = overload.mode();

            if last_status_update.elapsed() >= STATUS_UPDATE_INTERVAL {
                let source_counters = self.source.counters();
                let output_counters = self.output.counters();
                metrics.render_overflows = source_counters.render_overflows;
                metrics.capture_overflows = source_counters.capture_overflows;
                metrics.output_underruns = output_counters.underrun_events;
                metrics.output_queue_overflows = output_counters.queue_overflows;
                metrics.output_queue_depth_ms = output_counters.queue_depth_frames as f32 * 1_000.0
                    / xmic_engine_core::DSP_SAMPLE_RATE_HZ as f32;
                metrics.output_rate_adjustment_ppm = output_counters.rate_adjustment_ppm as f32;
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
                        Err(error) => {
                            dsp_warning = Some(format!("read AEC metrics failed: {error}"));
                        }
                    }
                }
                let mode = if self.safe_mode {
                    EngineMode::SafeMode
                } else if dsp_failed {
                    match settings.failure_policy {
                        DspFailurePolicy::RawPassthrough => EngineMode::RawFallback,
                        DspFailurePolicy::Mute => EngineMode::Muted,
                    }
                } else if started.elapsed() < CONVERGENCE_TIME
                    || recovery_mute_until.is_some_and(|until| Instant::now() < until)
                {
                    EngineMode::Converging
                } else {
                    EngineMode::Active
                };
                replace_runtime(
                    runtime,
                    mode,
                    initial_detail(mode),
                    Some(self.active_route()),
                    metrics.clone(),
                    combine_runtime_warnings(dsp_warning.clone(), overload.mode()),
                );
                last_status_update = Instant::now();
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

fn validate_topology(
    source: &DuplexSourceStream,
    output_endpoint: &EndpointDescriptor,
    follow_default_render: bool,
) -> Result<(), String> {
    if let Some(message) = render_output_conflict_message(
        &source.info().render_endpoint,
        output_endpoint,
        follow_default_render,
    ) {
        return Err(message);
    }
    if same_device_group(&source.info().capture_endpoint, output_endpoint) {
        return Err(
            "the virtual output belongs to the same device group as the physical microphone"
                .to_owned(),
        );
    }
    let capture_name = source.info().capture_endpoint.friendly_name.to_lowercase();
    if capture_name.contains("cable output") || capture_name.contains("vb-audio virtual cable") {
        return Err("virtual cable capture cannot be used as the physical microphone".to_owned());
    }
    Ok(())
}

fn render_output_conflict_message(
    render_endpoint: &EndpointDescriptor,
    output_endpoint: &EndpointDescriptor,
    follow_default_render: bool,
) -> Option<String> {
    if render_endpoint.id != output_endpoint.id
        && !same_device_group(render_endpoint, output_endpoint)
    {
        return None;
    }
    if follow_default_render {
        return Some(format!(
            "Windows 默认播放设备“{}”同时被选为虚拟麦克风输出。请先将音响或耳机设为 Windows 默认播放设备，然后重试。",
            render_endpoint.friendly_name
        ));
    }
    Some(format!(
        "播放参考“{}”不能同时作为虚拟麦克风输出。请为播放参考选择实际发声的音响或耳机。",
        render_endpoint.friendly_name
    ))
}

fn same_device_group(left: &EndpointDescriptor, right: &EndpointDescriptor) -> bool {
    left.group_id
        .as_ref()
        .zip(right.group_id.as_ref())
        .is_some_and(|(left, right)| !left.is_empty() && left == right)
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

fn initial_detail(mode: EngineMode) -> &'static str {
    match mode {
        EngineMode::Converging => "AEC3 is converging",
        EngineMode::Active => "realtime microphone processing is active",
        EngineMode::RawFallback => "raw microphone fallback is active",
        EngineMode::SafeMode => "engine safe mode is active; microphone output is muted",
        EngineMode::Muted => "microphone output is muted after a DSP failure",
        _ => "audio engine state changed",
    }
}

fn set_worker_error(runtime: &Mutex<RuntimeSnapshot>, message: &str) {
    let mut state = lock_unpoisoned(runtime);
    state.mode = EngineMode::Error;
    state.detail = "audio engine is waiting to reconnect".to_owned();
    state.active_route = None;
    state.warning = Some(message.to_owned());
}

fn replace_runtime(
    runtime: &Mutex<RuntimeSnapshot>,
    mode: EngineMode,
    detail: &str,
    active_route: Option<ActiveRoute>,
    metrics: EngineMetrics,
    warning: Option<String>,
) {
    let mut state = lock_unpoisoned(runtime);
    state.mode = mode;
    state.detail = detail.to_owned();
    state.active_route = active_route;
    state.metrics = metrics;
    state.warning = warning;
}

fn set_runtime_profile(
    runtime: &Mutex<RuntimeSnapshot>,
    device_profile: ActiveDeviceProfile,
    effective_dsp: DspSettings,
) {
    let mut state = lock_unpoisoned(runtime);
    state.device_profile = Some(device_profile);
    state.effective_dsp = Some(effective_dsp);
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

fn realtime_timing_faults(source: &DuplexSourceStream, output: &OutputSinkStream) -> u64 {
    let source = source.counters();
    let output = output.counters();
    source
        .render_overflows
        .saturating_add(source.capture_overflows)
        .saturating_add(source.data_discontinuities)
        .saturating_add(output.underrun_events)
        .saturating_add(output.queue_overflows)
}

fn combine_runtime_warnings(
    dsp_warning: Option<String>,
    load_mode: ProcessingLoadMode,
) -> Option<String> {
    let overload_warning = match load_mode {
        ProcessingLoadMode::Full => None,
        ProcessingLoadMode::Reduced => {
            Some("CPU overload: AGC2 is temporarily bypassed while AEC and noise removal continue")
        }
        ProcessingLoadMode::Protection => Some(
            "CPU overload protection: microphone output is muted until realtime scheduling stabilizes",
        ),
    };
    match (dsp_warning, overload_warning) {
        (Some(dsp), Some(overload)) => Some(format!("{dsp}; {overload}")),
        (Some(dsp), None) => Some(dsp),
        (None, Some(overload)) => Some(overload.to_owned()),
        (None, None) => None,
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessingLoadTransition {
    previous: ProcessingLoadMode,
    current: ProcessingLoadMode,
}

struct ProcessingOverloadController {
    mode: ProcessingLoadMode,
    reduced_stage_available: bool,
    window_frames: u32,
    budget_exceeded: u32,
    deadline_misses: u32,
    timing_faults: u32,
    overloaded_windows: u32,
    stable_windows: u32,
}

impl ProcessingOverloadController {
    fn new(reduced_stage_available: bool) -> Self {
        Self {
            mode: ProcessingLoadMode::Full,
            reduced_stage_available,
            window_frames: 0,
            budget_exceeded: 0,
            deadline_misses: 0,
            timing_faults: 0,
            overloaded_windows: 0,
            stable_windows: 0,
        }
    }

    fn mode(&self) -> ProcessingLoadMode {
        self.mode
    }

    fn observe(
        &mut self,
        processing_time: Duration,
        timing_fault: bool,
    ) -> Option<ProcessingLoadTransition> {
        let processing_us = u64::try_from(processing_time.as_micros()).unwrap_or(u64::MAX);
        self.window_frames = self.window_frames.saturating_add(1);
        if processing_us > PROCESSING_BUDGET_US {
            self.budget_exceeded = self.budget_exceeded.saturating_add(1);
        }
        if processing_us > PROCESSING_DEADLINE_US {
            self.deadline_misses = self.deadline_misses.saturating_add(1);
        }
        if timing_fault {
            self.timing_faults = self.timing_faults.saturating_add(1);
        }
        if self.window_frames < OVERLOAD_WINDOW_FRAMES {
            return None;
        }

        let overloaded = self.budget_exceeded >= OVERLOAD_BUDGET_BLOCKS_PER_WINDOW
            || self.deadline_misses >= OVERLOAD_DEADLINES_PER_WINDOW
            || self.timing_faults >= OVERLOAD_TIMING_FAULTS_PER_WINDOW;
        let stable =
            self.budget_exceeded == 0 && self.deadline_misses == 0 && self.timing_faults == 0;
        self.window_frames = 0;
        self.budget_exceeded = 0;
        self.deadline_misses = 0;
        self.timing_faults = 0;

        if overloaded {
            self.overloaded_windows = self.overloaded_windows.saturating_add(1);
            self.stable_windows = 0;
        } else if stable {
            self.stable_windows = self.stable_windows.saturating_add(1);
            self.overloaded_windows = 0;
        } else {
            self.overloaded_windows = 0;
            self.stable_windows = 0;
        }

        let previous = self.mode;
        if self.overloaded_windows >= OVERLOAD_WINDOWS_TO_ESCALATE {
            self.mode = match self.mode {
                ProcessingLoadMode::Full if self.reduced_stage_available => {
                    ProcessingLoadMode::Reduced
                }
                ProcessingLoadMode::Full => ProcessingLoadMode::Protection,
                ProcessingLoadMode::Reduced | ProcessingLoadMode::Protection => {
                    ProcessingLoadMode::Protection
                }
            };
            self.overloaded_windows = 0;
        } else if self.stable_windows >= STABLE_WINDOWS_TO_RECOVER {
            self.mode = match self.mode {
                ProcessingLoadMode::Protection if self.reduced_stage_available => {
                    ProcessingLoadMode::Reduced
                }
                ProcessingLoadMode::Protection => ProcessingLoadMode::Full,
                ProcessingLoadMode::Reduced | ProcessingLoadMode::Full => ProcessingLoadMode::Full,
            };
            self.stable_windows = 0;
        }

        (self.mode != previous).then_some(ProcessingLoadTransition {
            previous,
            current: self.mode,
        })
    }
}

struct ProcessingHistogram {
    bins: [u64; PROCESSING_HISTOGRAM_BINS],
    total: u64,
    max_us: u64,
    deadline_misses: u64,
}

impl Default for ProcessingHistogram {
    fn default() -> Self {
        Self {
            bins: [0; PROCESSING_HISTOGRAM_BINS],
            total: 0,
            max_us: 0,
            deadline_misses: 0,
        }
    }
}

impl ProcessingHistogram {
    fn observe(&mut self, duration: Duration) {
        let microseconds = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let bin = (microseconds.saturating_sub(1) / PROCESSING_HISTOGRAM_BIN_US)
            .min((PROCESSING_HISTOGRAM_BINS - 1) as u64) as usize;
        self.bins[bin] = self.bins[bin].saturating_add(1);
        self.total = self.total.saturating_add(1);
        self.max_us = self.max_us.max(microseconds);
        if microseconds > PROCESSING_DEADLINE_US {
            self.deadline_misses = self.deadline_misses.saturating_add(1);
        }
    }

    fn p99_us(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let target = self.total.saturating_mul(99).div_ceil(100);
        let mut cumulative = 0u64;
        for (index, count) in self.bins.iter().copied().enumerate() {
            cumulative = cumulative.saturating_add(count);
            if cumulative >= target {
                return ((index as u64 + 1) * PROCESSING_HISTOGRAM_BIN_US) as f64;
            }
        }
        (PROCESSING_HISTOGRAM_BINS as u64 * PROCESSING_HISTOGRAM_BIN_US) as f64
    }

    fn max_us(&self) -> u64 {
        self.max_us
    }

    fn deadline_misses(&self) -> u64 {
        self.deadline_misses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmic_platform_api::DataFlow;

    fn endpoint(id: &str, group_id: Option<&str>) -> EndpointDescriptor {
        EndpointDescriptor {
            platform: "windows".to_owned(),
            id: id.to_owned(),
            group_id: group_id.map(str::to_owned),
            friendly_name: "CABLE Input (VB-Audio Virtual Cable)".to_owned(),
            flow: DataFlow::Render,
            default_roles: Vec::new(),
        }
    }

    #[test]
    fn default_render_conflict_explains_how_to_fix_windows_routing() {
        let render = endpoint("cable-input", Some("cable-group"));
        let output = endpoint("cable-input", Some("cable-group"));
        let message = render_output_conflict_message(&render, &output, true)
            .expect("matching endpoint should be rejected");

        assert!(message.contains("Windows 默认播放设备"));
        assert!(message.contains("CABLE Input"));
        assert!(message.contains("音响或耳机"));
    }

    #[test]
    fn distinct_render_and_output_endpoints_do_not_conflict() {
        let render = endpoint("speakers", Some("speaker-group"));
        let output = endpoint("cable-input", Some("cable-group"));
        assert_eq!(render_output_conflict_message(&render, &output, true), None);
    }

    #[test]
    fn endpoints_in_the_same_device_group_conflict_even_with_different_ids() {
        let render = endpoint("speaker-output-a", Some("physical-device"));
        let output = endpoint("speaker-output-b", Some("physical-device"));

        assert!(render_output_conflict_message(&render, &output, false).is_some());
    }

    #[test]
    fn processing_histogram_reports_tail_without_allocations() {
        let mut histogram = ProcessingHistogram::default();
        for _ in 0..99 {
            histogram.observe(Duration::from_micros(100));
        }
        histogram.observe(Duration::from_micros(500));

        assert_eq!(histogram.p99_us(), 100.0);
        assert_eq!(histogram.max_us(), 500);
        assert_eq!(histogram.deadline_misses(), 0);
    }

    #[test]
    fn processing_histogram_reports_deadline_misses_beyond_ten_ms() {
        let mut histogram = ProcessingHistogram::default();
        histogram.observe(Duration::from_micros(18_750));

        assert_eq!(histogram.p99_us(), 18_750.0);
        assert_eq!(histogram.max_us(), 18_750);
        assert_eq!(histogram.deadline_misses(), 1);
    }

    #[test]
    fn sustained_overload_escalates_and_stable_windows_recover() {
        let mut controller = ProcessingOverloadController::new(true);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Reduced);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Protection);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Reduced);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

    #[test]
    fn isolated_slow_frames_do_not_trigger_degradation() {
        let mut controller = ProcessingOverloadController::new(true);
        for frame in 0..(OVERLOAD_WINDOW_FRAMES * 4) {
            let duration = if frame % OVERLOAD_WINDOW_FRAMES == 0 {
                Duration::from_millis(5)
            } else {
                Duration::from_micros(200)
            };
            controller.observe(duration, false);
        }

        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

    #[test]
    fn overload_skips_the_reduced_stage_when_agc2_is_disabled() {
        let mut controller = ProcessingOverloadController::new(false);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * OVERLOAD_WINDOWS_TO_ESCALATE) {
            controller.observe(Duration::from_millis(5), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Protection);

        for _ in 0..(OVERLOAD_WINDOW_FRAMES * STABLE_WINDOWS_TO_RECOVER) {
            controller.observe(Duration::from_micros(200), false);
        }
        assert_eq!(controller.mode(), ProcessingLoadMode::Full);
    }

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
    fn clipping_counter_only_counts_near_full_scale_samples() {
        assert_eq!(clipped_sample_count(&[-1.0, -0.999, 0.998, 0.999, 1.0]), 4);
    }

    #[test]
    fn dsp_feature_toggle_changes_only_the_selected_stage() {
        let mut settings = EngineSettings::default();

        assert!(!DspFeature::Aec.toggle(&mut settings));
        assert!(!settings.dsp.agc2.enabled);
        assert!(settings.dsp.vad_gate.enabled);

        assert!(DspFeature::Agc2.toggle(&mut settings));
        assert!(!settings.dsp.aec.enabled);
        assert!(settings.dsp.vad_gate.enabled);

        assert!(!DspFeature::VadGate.toggle(&mut settings));
        assert!(!settings.dsp.aec.enabled);
        assert!(settings.dsp.agc2.enabled);
    }
}
