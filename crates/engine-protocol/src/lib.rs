#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use xmic_platform_api::EndpointDescriptor;

pub const PROTOCOL_VERSION: u16 = 22;
pub const DSP_ABI_VERSION: u32 = 2;
pub const MAX_CAPTURE_CHANNEL_INDEX: usize = 31;
pub const MAX_TRUSTED_VIRTUAL_AUDIO_PAIRS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineState {
    Off,
    Processing,
    Degraded,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineOutput {
    Inactive,
    Processed,
    Raw,
    Silence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DspLibraryValidation {
    pub expected_abi: u32,
    pub actual_abi: Option<u32>,
    pub error: Option<String>,
}

impl DspLibraryValidation {
    pub fn pending() -> Self {
        Self {
            expected_abi: DSP_ABI_VERSION,
            actual_abi: None,
            error: None,
        }
    }

    pub fn valid(actual_abi: u32) -> Self {
        Self {
            expected_abi: DSP_ABI_VERSION,
            actual_abi: Some(actual_abi),
            error: None,
        }
    }

    pub fn invalid(actual_abi: Option<u32>, error: impl Into<String>) -> Self {
        Self {
            expected_abi: DSP_ABI_VERSION,
            actual_abi,
            error: Some(error.into()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.error.is_none() && self.actual_abi == Some(self.expected_abi)
    }
}

impl Default for DspLibraryValidation {
    fn default() -> Self {
        Self::pending()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrimaryReasonCode {
    ManualOff,
    Starting,
    AecWarmup,
    AecRebuild,
    WaitingForPlayback,
    WaitingForCapture,
    WaitingForVirtualOutput,
    AudioDeviceFailure,
    DspFailure,
    CpuOverload,
    SafetyConflict,
    PairUnconfirmed,
    SessionUnavailable,
    HelperUnavailable,
    CrashLoop,
    SettingsReset,
    WorkerTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AecPreset {
    Natural,
    Balanced,
    Strong,
}

/// What the virtual microphone carries while processing is degraded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DegradedOutput {
    Raw,
    #[default]
    Silence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DelayCalibrationState {
    #[default]
    Uncalibrated,
    WarmingUp,
    Calibrated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessingLoadMode {
    #[default]
    Full,
    Protection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AecSettings {
    pub enabled: bool,
    pub preset: AecPreset,
    pub high_pass_filter_enabled: bool,
}

impl Default for AecSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            preset: AecPreset::Natural,
            high_pass_filter_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RnnoiseSettings {
    pub enabled: bool,
    pub mix: f32,
}

impl Default for RnnoiseSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mix: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Agc2Settings {
    pub enabled: bool,
    pub adaptive_digital_enabled: bool,
    pub fixed_gain_db: f32,
    pub headroom_db: f32,
    pub max_gain_db: f32,
    pub initial_gain_db: f32,
    pub max_gain_change_db_per_second: f32,
    pub max_output_noise_level_dbfs: f32,
}

impl Default for Agc2Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            adaptive_digital_enabled: true,
            fixed_gain_db: 0.0,
            headroom_db: 5.0,
            max_gain_db: 6.0,
            initial_gain_db: 0.0,
            max_gain_change_db_per_second: 3.0,
            max_output_noise_level_dbfs: -55.0,
        }
    }
}

impl Agc2Settings {
    /// AGC2 without adaptive digital and without fixed gain only runs its internal limiter, so the
    /// switch reads as enabled while the level stays untouched.
    pub fn is_pass_through(&self) -> bool {
        !self.adaptive_digital_enabled && self.fixed_gain_db <= 0.0
    }

    /// Restores the adaptive default so enabling AGC2 actually changes the level.
    pub fn repair_pass_through(&mut self) -> bool {
        let repaired = self.is_pass_through();
        if repaired {
            self.adaptive_digital_enabled = true;
        }
        repaired
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VadGateSettings {
    pub enabled: bool,
    pub threshold: f32,
    pub confirmation_ms: u32,
    pub hold_ms: u32,
    pub floor_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Default for VadGateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.70,
            confirmation_ms: default_vad_confirmation_ms(),
            hold_ms: 300,
            floor_db: -20.0,
            attack_ms: 10.0,
            release_ms: 150.0,
        }
    }
}

/// Every 10 ms of confirmation costs 10 ms of gate look-ahead, so this value dominates the added
/// microphone latency and decides how short an utterance can be before the gate drops it entirely.
const fn default_vad_confirmation_ms() -> u32 {
    40
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LimiterSettings {
    #[serde(default)]
    pub gain_db: f32,
    pub ceiling_dbfs: f32,
    pub release_ms: f32,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            ceiling_dbfs: -1.0,
            release_ms: 50.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DspSettings {
    pub aec: AecSettings,
    pub rnnoise: RnnoiseSettings,
    pub agc2: Agc2Settings,
    pub vad_gate: VadGateSettings,
    pub limiter: LimiterSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VirtualDeviceMode {
    VbCable,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TrustedVirtualAudioPair {
    pub render_endpoint_id: String,
    pub capture_endpoint_id: String,
    pub render_name: String,
    pub capture_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VirtualRenderEndpointId(String);

impl VirtualRenderEndpointId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VirtualRenderEndpointId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VirtualCaptureEndpointId(String);

impl VirtualCaptureEndpointId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VirtualCaptureEndpointId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Endpoint IDs previously observed as the complete, name-recognised standard VB-CABLE pair.
/// Unlike a custom pair, this marker is written by route discovery rather than user confirmation.
/// Distinct transparent ID types prevent accidentally swapping the render and capture sides while
/// retaining the existing JSON string representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KnownVbCablePair {
    pub render_endpoint_id: VirtualRenderEndpointId,
    pub capture_endpoint_id: VirtualCaptureEndpointId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EngineSettings {
    pub follow_default_render: bool,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub render_endpoint_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub capture_endpoint_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub capture_channel: Option<usize>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub output_endpoint_id: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub virtual_device_mode: Option<VirtualDeviceMode>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub virtual_capture_endpoint_id: Option<String>,
    pub trusted_virtual_audio_pairs: Vec<TrustedVirtualAudioPair>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub known_vb_cable_pair: Option<KnownVbCablePair>,
    pub degraded_output: DegradedOutput,
    pub dsp: DspSettings,
}

// A custom deserializer makes the key required without rejecting an explicit null. Deriving Option
// alone silently accepts a missing key as None, bypassing the strict settings reset contract.
fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            follow_default_render: true,
            render_endpoint_id: None,
            capture_endpoint_id: None,
            capture_channel: None,
            output_endpoint_id: None,
            virtual_device_mode: None,
            virtual_capture_endpoint_id: None,
            trusted_virtual_audio_pairs: Vec::new(),
            known_vb_cable_pair: None,
            degraded_output: DegradedOutput::Silence,
            dsp: DspSettings::default(),
        }
    }
}

impl EngineSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        validate_optional_endpoint(&self.render_endpoint_id)?;
        validate_optional_endpoint(&self.capture_endpoint_id)?;
        validate_optional_endpoint(&self.output_endpoint_id)?;
        validate_optional_endpoint(&self.virtual_capture_endpoint_id)?;
        if self.trusted_virtual_audio_pairs.len() > MAX_TRUSTED_VIRTUAL_AUDIO_PAIRS {
            return Err(SettingsError::TooManyTrustedVirtualAudioPairs);
        }
        for pair in &self.trusted_virtual_audio_pairs {
            validate_endpoint(&pair.render_endpoint_id)?;
            validate_endpoint(&pair.capture_endpoint_id)?;
        }
        if let Some(pair) = &self.known_vb_cable_pair {
            validate_endpoint(pair.render_endpoint_id.as_str())?;
            validate_endpoint(pair.capture_endpoint_id.as_str())?;
        }
        if self
            .capture_channel
            .is_some_and(|channel| channel > MAX_CAPTURE_CHANNEL_INDEX)
        {
            return Err(SettingsError::OutOfRange("capture channel"));
        }
        if self.dsp.aec.enabled && !self.follow_default_render && self.render_endpoint_id.is_none()
        {
            return Err(SettingsError::FixedRenderEndpointRequired);
        }
        validate_range("RNNoise mix", self.dsp.rnnoise.mix, 0.0, 1.0)?;
        if self.dsp.vad_gate.enabled && !self.dsp.rnnoise.enabled {
            return Err(SettingsError::VadRequiresRnnoise);
        }
        if self.dsp.agc2.enabled && self.dsp.agc2.is_pass_through() {
            return Err(SettingsError::Agc2WouldPassThrough);
        }
        validate_range("AGC2 fixed gain", self.dsp.agc2.fixed_gain_db, 0.0, 30.0)?;
        validate_range("AGC2 headroom", self.dsp.agc2.headroom_db, 0.0, 30.0)?;
        validate_range("AGC2 maximum gain", self.dsp.agc2.max_gain_db, 0.001, 50.0)?;
        validate_range(
            "AGC2 initial gain",
            self.dsp.agc2.initial_gain_db,
            0.0,
            self.dsp.agc2.max_gain_db,
        )?;
        validate_range(
            "AGC2 gain change speed",
            self.dsp.agc2.max_gain_change_db_per_second,
            0.001,
            20.0,
        )?;
        validate_range(
            "AGC2 output noise level",
            self.dsp.agc2.max_output_noise_level_dbfs,
            -90.0,
            0.0,
        )?;
        validate_range("VAD threshold", self.dsp.vad_gate.threshold, 0.0, 1.0)?;
        if !(10..=1_000).contains(&self.dsp.vad_gate.confirmation_ms) {
            return Err(SettingsError::OutOfRange("VAD confirmation"));
        }
        if self.dsp.vad_gate.hold_ms > 10_000 {
            return Err(SettingsError::OutOfRange("VAD hold"));
        }
        validate_range("VAD floor", self.dsp.vad_gate.floor_db, -90.0, 0.0)?;
        validate_range("VAD attack", self.dsp.vad_gate.attack_ms, 0.1, 10_000.0)?;
        validate_range("VAD release", self.dsp.vad_gate.release_ms, 0.1, 10_000.0)?;
        validate_range("limiter gain", self.dsp.limiter.gain_db, -12.0, 30.0)?;
        validate_range("limiter ceiling", self.dsp.limiter.ceiling_dbfs, -30.0, 0.0)?;
        validate_range(
            "limiter release",
            self.dsp.limiter.release_ms,
            0.1,
            10_000.0,
        )?;
        Ok(())
    }
}

fn validate_optional_endpoint(endpoint_id: &Option<String>) -> Result<(), SettingsError> {
    if endpoint_id
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        Err(SettingsError::EmptyEndpointId)
    } else {
        Ok(())
    }
}

fn validate_endpoint(endpoint_id: &str) -> Result<(), SettingsError> {
    if endpoint_id.trim().is_empty() {
        Err(SettingsError::EmptyEndpointId)
    } else {
        Ok(())
    }
}

fn validate_range(
    name: &'static str,
    value: f32,
    minimum: f32,
    maximum: f32,
) -> Result<(), SettingsError> {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(SettingsError::OutOfRange(name))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("endpoint ID cannot be empty")]
    EmptyEndpointId,
    #[error("a fixed render endpoint is required when default following is disabled")]
    FixedRenderEndpointRequired,
    #[error("VAD gate requires RNNoise")]
    VadRequiresRnnoise,
    #[error("AGC2 needs adaptive digital gain or a fixed gain above 0 dB to change the level")]
    Agc2WouldPassThrough,
    #[error("too many trusted virtual audio device pairs")]
    TooManyTrustedVirtualAudioPairs,
    #[error("{0} is outside its supported range")]
    OutOfRange(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActiveRoute {
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
    pub output_endpoint_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VirtualDeviceIssueReason {
    VbCableUnavailable,
    CustomPairIncomplete,
    CustomPairUnconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VirtualRouteConflictKind {
    DefaultRender,
    FixedRender,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConfigurationIssue {
    PhysicalCaptureRequired {
        virtual_capture_endpoint_id: Option<String>,
    },
    VirtualDeviceRequired {
        reason: VirtualDeviceIssueReason,
    },
    VirtualRouteConflict {
        kind: VirtualRouteConflictKind,
        endpoint_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PrimaryReason {
    pub code: PrimaryReasonCode,
    pub detail: Option<String>,
    pub endpoint_id: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub configuration_issue: Option<ConfigurationIssue>,
}

impl PrimaryReason {
    pub fn new(code: PrimaryReasonCode) -> Self {
        Self {
            code,
            detail: None,
            endpoint_id: None,
            retry_after_ms: None,
            configuration_issue: None,
        }
    }

    pub fn with_detail(code: PrimaryReasonCode, detail: impl Into<String>) -> Self {
        Self {
            detail: Some(detail.into()),
            ..Self::new(code)
        }
    }

    pub fn waiting(code: PrimaryReasonCode, endpoint_id: Option<String>) -> Self {
        debug_assert!(matches!(
            code,
            PrimaryReasonCode::WaitingForPlayback
                | PrimaryReasonCode::WaitingForCapture
                | PrimaryReasonCode::WaitingForVirtualOutput
        ));
        Self {
            endpoint_id,
            ..Self::new(code)
        }
    }

    pub fn configuration(issue: ConfigurationIssue) -> Self {
        let (code, endpoint_id) = match &issue {
            ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
            } => (PrimaryReasonCode::PairUnconfirmed, None),
            ConfigurationIssue::VirtualDeviceRequired { .. } => {
                (PrimaryReasonCode::WaitingForVirtualOutput, None)
            }
            ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id,
            } => (
                PrimaryReasonCode::SafetyConflict,
                virtual_capture_endpoint_id.clone(),
            ),
            ConfigurationIssue::VirtualRouteConflict { endpoint_id, .. } => {
                (PrimaryReasonCode::SafetyConflict, Some(endpoint_id.clone()))
            }
        };
        Self {
            code,
            endpoint_id,
            configuration_issue: Some(issue),
            ..Self::new(code)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EngineMetrics {
    pub processed_blocks: u64,
    pub render_underruns: u64,
    pub render_overflows: u64,
    pub capture_overflows: u64,
    /// Packets the audio driver reported as discontinuous — data it dropped before XMic ever saw it.
    /// A wireless link that keeps dropping packets shows up here and nowhere else, which is why this
    /// is published rather than folded into the overload signal.
    pub device_discontinuities: u64,
    pub output_underruns: u64,
    pub output_queue_overflows: u64,
    pub output_queue_depth_ms: f32,
    pub output_rate_adjustment_ppm: f32,
    pub device_restarts: u64,
    pub dsp_failures: u64,
    pub processing_time_p99_us: f64,
    pub processing_time_max_us: u64,
    pub processing_deadline_misses: u64,
    pub processing_budget_exceeded_blocks: u64,
    pub processing_overload_events: u64,
    pub processing_recovery_events: u64,
    pub processing_load_mode: ProcessingLoadMode,
    /// Measured microphone-to-worker latency: the age of a capture packet when the WASAPI worker
    /// receives it plus any backlog waiting for the DSP thread.
    pub capture_latency_ms: f32,
    /// Deterministic DSP look-ahead: gate pre-roll plus RNNoise.
    pub dsp_latency_ms: f32,
    /// Measured write-to-endpoint latency: software queue, endpoint buffer fill, and the latency the
    /// output driver reports. The virtual cable's internal buffering is not observable from here.
    pub output_latency_ms: f32,
    /// Sum of the three segments above — the latency XMic adds before the voice app sees the audio.
    pub pipeline_latency_ms: f32,
    pub host_delay_ms: Option<i32>,
    pub host_delay_raw_skew_ms: Option<f32>,
    pub host_delay_jitter_ms: Option<f32>,
    pub host_delay_calibration_state: DelayCalibrationState,
    pub host_delay_reset_events: u64,
    pub aec_estimated_delay_ms: Option<i32>,
    pub aec_echo_return_loss_db: Option<f32>,
    pub aec_echo_return_loss_enhancement_db: Option<f32>,
    pub aec_residual_echo_likelihood: Option<f32>,
    pub aec_attenuation_db: Option<f32>,
    pub input_peak_dbfs: Option<f32>,
    pub output_peak_dbfs: Option<f32>,
    pub input_clipped_samples: u64,
    pub limiter_limited_samples: u64,
    pub vad_probability: Option<f32>,
    pub speech_vad_active: Option<bool>,
    pub vad_gate_open: Option<bool>,
    pub vad_gate_open_events: u64,
    pub echo_guard_active: Option<bool>,
    pub echo_guard_active_events: u64,
    pub echo_guard_nearend_candidate_events: u64,
    pub echo_guard_release_events: u64,
}

impl EngineMetrics {
    /// Clears values that only describe a currently owned audio session while retaining cumulative
    /// counters. Both the helper and UI supervisor use this transition so inactive snapshots cannot
    /// drift as metrics evolve.
    pub fn clear_session_gauges(&mut self) {
        self.input_peak_dbfs = None;
        self.output_peak_dbfs = None;
        self.output_queue_depth_ms = 0.0;
        self.output_rate_adjustment_ppm = 0.0;
        self.processing_time_p99_us = 0.0;
        self.processing_time_max_us = 0;
        self.processing_load_mode = ProcessingLoadMode::Full;
        self.capture_latency_ms = 0.0;
        self.dsp_latency_ms = 0.0;
        self.output_latency_ms = 0.0;
        self.pipeline_latency_ms = 0.0;
        self.host_delay_ms = None;
        self.host_delay_raw_skew_ms = None;
        self.host_delay_jitter_ms = None;
        self.host_delay_calibration_state = DelayCalibrationState::Uncalibrated;
        self.aec_estimated_delay_ms = None;
        self.aec_echo_return_loss_db = None;
        self.aec_echo_return_loss_enhancement_db = None;
        self.aec_residual_echo_likelihood = None;
        self.aec_attenuation_db = None;
        self.vad_probability = None;
        self.speech_vad_active = None;
        self.vad_gate_open = None;
        self.echo_guard_active = None;
    }

    pub fn inactive(self) -> Self {
        let mut metrics = self;
        metrics.clear_session_gauges();
        metrics
    }

    /// Adds counters accumulated before a helper process restart while leaving current-session
    /// gauges untouched. The UI supervisor applies this to every helper status so crash recovery
    /// cannot make app-lifetime diagnostics move backwards.
    pub fn with_counter_baseline(mut self, baseline: &Self) -> Self {
        self.processed_blocks = self
            .processed_blocks
            .saturating_add(baseline.processed_blocks);
        self.render_underruns = self
            .render_underruns
            .saturating_add(baseline.render_underruns);
        self.render_overflows = self
            .render_overflows
            .saturating_add(baseline.render_overflows);
        self.capture_overflows = self
            .capture_overflows
            .saturating_add(baseline.capture_overflows);
        self.device_discontinuities = self
            .device_discontinuities
            .saturating_add(baseline.device_discontinuities);
        self.output_underruns = self
            .output_underruns
            .saturating_add(baseline.output_underruns);
        self.output_queue_overflows = self
            .output_queue_overflows
            .saturating_add(baseline.output_queue_overflows);
        self.device_restarts = self
            .device_restarts
            .saturating_add(baseline.device_restarts);
        self.dsp_failures = self.dsp_failures.saturating_add(baseline.dsp_failures);
        self.processing_deadline_misses = self
            .processing_deadline_misses
            .saturating_add(baseline.processing_deadline_misses);
        self.processing_budget_exceeded_blocks = self
            .processing_budget_exceeded_blocks
            .saturating_add(baseline.processing_budget_exceeded_blocks);
        self.processing_overload_events = self
            .processing_overload_events
            .saturating_add(baseline.processing_overload_events);
        self.processing_recovery_events = self
            .processing_recovery_events
            .saturating_add(baseline.processing_recovery_events);
        self.host_delay_reset_events = self
            .host_delay_reset_events
            .saturating_add(baseline.host_delay_reset_events);
        self.input_clipped_samples = self
            .input_clipped_samples
            .saturating_add(baseline.input_clipped_samples);
        self.limiter_limited_samples = self
            .limiter_limited_samples
            .saturating_add(baseline.limiter_limited_samples);
        self.vad_gate_open_events = self
            .vad_gate_open_events
            .saturating_add(baseline.vad_gate_open_events);
        self.echo_guard_active_events = self
            .echo_guard_active_events
            .saturating_add(baseline.echo_guard_active_events);
        self.echo_guard_nearend_candidate_events = self
            .echo_guard_nearend_candidate_events
            .saturating_add(baseline.echo_guard_nearend_candidate_events);
        self.echo_guard_release_events = self
            .echo_guard_release_events
            .saturating_add(baseline.echo_guard_release_events);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EngineStatus {
    pub protocol_version: u16,
    pub desired_enabled: bool,
    pub state: EngineState,
    pub output: EngineOutput,
    pub primary_reason: Option<PrimaryReason>,
    pub dsp_validation: DspLibraryValidation,
    /// Filled in by the UI process, which owns endpoint enumeration and its cache.
    pub endpoints: Vec<EndpointDescriptor>,
    pub settings: EngineSettings,
    pub active_route: Option<ActiveRoute>,
    pub metrics: EngineMetrics,
    /// Latched when the Off-time capture-only probe hears sustained speech.
    pub notification_probe_speech_detected: bool,
}

impl EngineStatus {
    pub fn bootstrap(endpoints: Vec<EndpointDescriptor>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            desired_enabled: false,
            state: EngineState::Off,
            output: EngineOutput::Inactive,
            primary_reason: Some(PrimaryReason::new(PrimaryReasonCode::ManualOff)),
            dsp_validation: DspLibraryValidation::pending(),
            endpoints,
            settings: EngineSettings::default(),
            active_route: None,
            metrics: EngineMetrics::default(),
            notification_probe_speech_detected: false,
        }
    }

    pub fn session_active(&self) -> bool {
        matches!(self.state, EngineState::Processing | EngineState::Degraded)
            && self.output != EngineOutput::Inactive
    }

    pub fn configuration_issue(&self) -> Option<&ConfigurationIssue> {
        self.primary_reason
            .as_ref()
            .and_then(|reason| reason.configuration_issue.as_ref())
    }

    pub fn transition_to_inactive(
        &mut self,
        state: EngineState,
        primary_reason: Option<PrimaryReason>,
    ) {
        self.state = state;
        self.output = EngineOutput::Inactive;
        self.primary_reason = primary_reason;
        self.active_route = None;
        self.metrics.clear_session_gauges();
    }
}

/// Session/runtime facts reported by the helper process. The UI-side `EngineController` owns the
/// public desired intent and is the only layer that derives `EngineState`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelperStatus {
    pub session_running: bool,
    pub output: EngineOutput,
    pub reason: Option<PrimaryReason>,
    pub dsp_validation: DspLibraryValidation,
    pub settings: EngineSettings,
    pub active_route: Option<ActiveRoute>,
    pub metrics: EngineMetrics,
    pub notification_probe_speech_detected: bool,
}

/// Renewed by the UI on every status poll. The only session allowed while desired processing is Off
/// is a temporary capture-only notification probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NotificationProbePermit {
    pub allowed: bool,
    pub needed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineRequest {
    Hello {
        protocol_version: u16,
    },
    GetStatus {
        probe_permit: NotificationProbePermit,
    },
    UpdateSettings {
        settings: Box<EngineSettings>,
    },
    PrepareFallback {
        reason: PrimaryReasonCode,
    },
    Start,
    Stop,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum EngineResponse {
    Hello { protocol_version: u16 },
    Status(Box<HelperStatus>),
    Ack,
    ConfigurationRequired(ConfigurationIssue),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_status_exposes_runtime_facts_without_public_state() {
        let helper = HelperStatus {
            session_running: true,
            output: EngineOutput::Processed,
            reason: None,
            dsp_validation: DspLibraryValidation::valid(DSP_ABI_VERSION),
            settings: EngineSettings::default(),
            active_route: None,
            metrics: EngineMetrics::default(),
            notification_probe_speech_detected: false,
        };

        let value = serde_json::to_value(helper).unwrap();

        assert_eq!(value["sessionRunning"], true);
        assert_eq!(value["output"], "processed");
        assert!(value.get("desiredEnabled").is_none());
        assert!(value.get("state").is_none());
    }

    #[test]
    fn dsp_validation_round_trips_with_explicit_abi_metadata() {
        let validation = DspLibraryValidation::invalid(Some(1), "ABI mismatch");

        let value = serde_json::to_value(&validation).unwrap();
        let round_trip: DspLibraryValidation = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(value["expectedAbi"], DSP_ABI_VERSION);
        assert_eq!(value["actualAbi"], 1);
        assert_eq!(value["error"], "ABI mismatch");
        assert_eq!(round_trip, validation);
        assert!(!round_trip.is_valid());
        assert!(DspLibraryValidation::valid(DSP_ABI_VERSION).is_valid());
        assert!(!DspLibraryValidation::pending().is_valid());
    }

    #[test]
    fn primary_reason_metadata_reaches_typescript_in_camel_case() {
        let reason = PrimaryReason {
            code: PrimaryReasonCode::WaitingForPlayback,
            detail: Some("waiting".to_owned()),
            endpoint_id: Some("render-id".to_owned()),
            retry_after_ms: Some(750),
            configuration_issue: None,
        };

        let value = serde_json::to_value(reason).unwrap();

        assert_eq!(value["code"], "waitingForPlayback");
        assert_eq!(value["detail"], "waiting");
        assert_eq!(value["endpointId"], "render-id");
        assert_eq!(value["retryAfterMs"], 750);
        assert!(value.get("message").is_none());
        assert!(value.get("endpointName").is_none());
    }

    #[test]
    fn configuration_issues_map_to_structured_primary_reason_codes() {
        assert_eq!(
            PrimaryReason::configuration(ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id: Some("virtual".to_owned()),
            })
            .code,
            PrimaryReasonCode::SafetyConflict,
        );
        assert_eq!(
            PrimaryReason::configuration(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
            })
            .code,
            PrimaryReasonCode::PairUnconfirmed,
        );
        assert_eq!(
            PrimaryReason::configuration(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
            .code,
            PrimaryReasonCode::WaitingForVirtualOutput,
        );
    }

    #[test]
    fn prepare_fallback_carries_a_structured_reason() {
        let request = EngineRequest::PrepareFallback {
            reason: PrimaryReasonCode::AudioDeviceFailure,
        };

        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"type":"prepareFallback","reason":"audioDeviceFailure"}"#,
        );
    }

    #[test]
    fn known_vb_cable_identity_types_keep_string_wire_shape() {
        let pair = KnownVbCablePair {
            render_endpoint_id: "render".to_owned().into(),
            capture_endpoint_id: "capture".to_owned().into(),
        };

        let value = serde_json::to_value(pair).unwrap();

        assert_eq!(value["renderEndpointId"], "render");
        assert_eq!(value["captureEndpointId"], "capture");
    }

    #[test]
    fn helper_counter_baseline_preserves_app_lifetime_totals() {
        let current = EngineMetrics {
            processed_blocks: 5,
            pipeline_latency_ms: 12.0,
            ..EngineMetrics::default()
        };
        let baseline = EngineMetrics {
            processed_blocks: 7,
            dsp_failures: 2,
            pipeline_latency_ms: 99.0,
            ..EngineMetrics::default()
        };

        let merged = current.with_counter_baseline(&baseline);

        assert_eq!(merged.processed_blocks, 12);
        assert_eq!(merged.dsp_failures, 2);
        assert_eq!(merged.pipeline_latency_ms, 12.0);
    }

    #[test]
    fn accepted_defaults_are_preserved_and_valid() {
        let settings = EngineSettings::default();
        settings.validate().unwrap();

        assert!(settings.dsp.aec.enabled);
        assert_eq!(settings.dsp.aec.preset, AecPreset::Natural);
        assert!(settings.dsp.aec.high_pass_filter_enabled);
        assert!(settings.dsp.rnnoise.enabled);
        assert_eq!(settings.dsp.rnnoise.mix, 1.0);
        assert!(!settings.dsp.agc2.enabled);
        assert!(settings.dsp.vad_gate.enabled);
        assert_eq!(settings.dsp.vad_gate.confirmation_ms, 40);
        assert_eq!(settings.dsp.vad_gate.hold_ms, 300);
        assert_eq!(settings.dsp.vad_gate.floor_db, -20.0);
        assert_eq!(settings.dsp.limiter.gain_db, 0.0);
        assert_eq!(settings.dsp.limiter.ceiling_dbfs, -1.0);
        assert_eq!(settings.dsp.limiter.release_ms, 50.0);
        assert_eq!(settings.degraded_output, DegradedOutput::Silence);
        assert_eq!(settings.known_vb_cable_pair, None);
    }

    #[test]
    fn old_setting_names_and_values_are_a_clean_break() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value["failurePolicy"] = "rawPassthrough".into();
        assert!(serde_json::from_value::<EngineSettings>(value).is_err());
        assert!(serde_json::from_str::<DegradedOutput>(r#""mute""#).is_err());
        assert_eq!(
            serde_json::to_string(&DegradedOutput::Raw).unwrap(),
            r#""raw""#
        );
    }

    #[test]
    fn unknown_nested_setting_fields_are_rejected() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value["dsp"]["aec"]["legacyMode"] = true.into();
        assert!(serde_json::from_value::<EngineSettings>(value).is_err());
    }

    #[test]
    fn missing_current_schema_fields_are_rejected() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("degradedOutput");
        assert!(serde_json::from_value::<EngineSettings>(value).is_err());

        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value["dsp"]["vadGate"]
            .as_object_mut()
            .unwrap()
            .remove("confirmationMs");
        assert!(serde_json::from_value::<EngineSettings>(value).is_err());
    }

    #[test]
    fn nullable_setting_fields_require_explicit_keys() {
        let value = serde_json::to_value(EngineSettings::default()).unwrap();
        for field in [
            "renderEndpointId",
            "captureEndpointId",
            "captureChannel",
            "outputEndpointId",
            "virtualDeviceMode",
            "virtualCaptureEndpointId",
            "knownVbCablePair",
        ] {
            assert_eq!(value.get(field), Some(&serde_json::Value::Null));
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);

            let error = serde_json::from_value::<EngineSettings>(missing).unwrap_err();

            assert!(error
                .to_string()
                .contains(&format!("missing field `{field}`")));
        }
    }

    #[test]
    fn nullable_setting_values_round_trip_with_nulls_and_values() {
        let populated = EngineSettings {
            render_endpoint_id: Some("playback".to_owned()),
            capture_endpoint_id: Some("microphone".to_owned()),
            capture_channel: Some(3),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            known_vb_cable_pair: Some(KnownVbCablePair {
                render_endpoint_id: "virtual-render".to_owned().into(),
                capture_endpoint_id: "virtual-capture".to_owned().into(),
            }),
            ..EngineSettings::default()
        };
        for settings in [EngineSettings::default(), populated] {
            let json = serde_json::to_string(&settings).unwrap();
            let decoded: EngineSettings = serde_json::from_str(&json).unwrap();

            decoded.validate().unwrap();
            assert_eq!(decoded, settings);
        }
    }

    #[test]
    fn configuration_issue_fields_reach_typescript_in_camel_case() {
        let capture = serde_json::to_string(&ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
        })
        .unwrap();
        assert_eq!(
            capture,
            r#"{"type":"physicalCaptureRequired","virtualCaptureEndpointId":"cable-output"}"#
        );
    }

    #[test]
    fn aec_off_does_not_require_a_playback_endpoint() {
        let mut settings = EngineSettings {
            follow_default_render: false,
            ..EngineSettings::default()
        };
        assert_eq!(
            settings.validate(),
            Err(SettingsError::FixedRenderEndpointRequired)
        );

        settings.dsp.aec.enabled = false;
        settings.validate().unwrap();
    }

    #[test]
    fn output_only_active_route_serializes_a_null_capture() {
        let route = ActiveRoute {
            render_endpoint_id: None,
            capture_endpoint_id: None,
            output_endpoint_id: "virtual-output".to_owned(),
        };
        let json = serde_json::to_string(&route).unwrap();
        assert!(json.contains(r#""captureEndpointId":null"#));
    }

    #[test]
    fn session_active_requires_a_non_blocked_processing_state() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.state = EngineState::Blocked;
        status.output = EngineOutput::Silence;
        assert!(!status.session_active());

        status.state = EngineState::Degraded;
        assert!(status.session_active());
    }

    #[test]
    fn inactive_transition_clears_route_and_session_gauges_but_keeps_counters() {
        let mut status = EngineStatus::bootstrap(Vec::new());
        status.active_route = Some(ActiveRoute {
            render_endpoint_id: None,
            capture_endpoint_id: Some("mic".to_owned()),
            output_endpoint_id: "virtual-output".to_owned(),
        });
        status.metrics.processed_blocks = 9;
        status.metrics.input_peak_dbfs = Some(-3.0);

        status.transition_to_inactive(
            EngineState::Degraded,
            Some(PrimaryReason::new(PrimaryReasonCode::AudioDeviceFailure)),
        );

        assert_eq!(status.output, EngineOutput::Inactive);
        assert!(status.active_route.is_none());
        assert_eq!(status.metrics.processed_blocks, 9);
        assert_eq!(status.metrics.input_peak_dbfs, None);
    }

    #[test]
    fn bootstrap_uses_the_public_state_contract() {
        let status = EngineStatus::bootstrap(Vec::new());
        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
        assert!(!status.desired_enabled);
        assert_eq!(status.state, EngineState::Off);
        assert_eq!(status.output, EngineOutput::Inactive);
        assert_eq!(
            status.primary_reason.as_ref().map(|reason| reason.code),
            Some(PrimaryReasonCode::ManualOff)
        );
        assert!(!status.notification_probe_speech_detected);
    }
}
