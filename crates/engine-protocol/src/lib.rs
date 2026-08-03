#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use xmic_platform_api::EndpointDescriptor;

pub const PROTOCOL_VERSION: u16 = 17;
pub const MAX_CAPTURE_CHANNEL_INDEX: usize = 31;
pub const MAX_TRUSTED_VIRTUAL_AUDIO_PAIRS: usize = 32;

/// VAD gate values that earlier builds shipped as defaults. `DspSettings::migrate_legacy_defaults`
/// rewrites exactly these two values so stored settings pick up the lower-latency defaults, while
/// anything the user tuned by hand is left alone.
pub const LEGACY_VAD_CONFIRMATION_MS: u32 = 120;
pub const LEGACY_VAD_FLOOR_DB: f32 = -90.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineMode {
    Sleeping,
    Starting,
    Converging,
    Active,
    RawFallback,
    SafeMode,
    Muted,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AecPreset {
    Natural,
    Balanced,
    Strong,
}

/// What the virtual microphone carries whenever XMic is not processing: a DSP failure, CPU overload
/// protection, safe mode, or simply no processing session at all. The `serde` aliases keep settings
/// files written by builds that called this the "DSP failure policy" loadable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnprocessedOutput {
    #[serde(alias = "rawPassthrough")]
    RawMicrophone,
    #[default]
    #[serde(alias = "mute")]
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
    Reduced,
    Protection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
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
#[serde(default, rename_all = "camelCase")]
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
#[serde(default, rename_all = "camelCase")]
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
#[serde(default, rename_all = "camelCase")]
pub struct VadGateSettings {
    pub enabled: bool,
    pub threshold: f32,
    #[serde(default = "default_vad_confirmation_ms")]
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
#[serde(default, rename_all = "camelCase")]
pub struct LimiterSettings {
    pub ceiling_dbfs: f32,
    pub release_ms: f32,
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self {
            ceiling_dbfs: -1.0,
            release_ms: 50.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DspSettings {
    pub aec: AecSettings,
    pub rnnoise: RnnoiseSettings,
    pub agc2: Agc2Settings,
    pub vad_gate: VadGateSettings,
    pub limiter: LimiterSettings,
}

impl DspSettings {
    /// Rewrites values that still hold a superseded default and repairs the AGC2 pass-through
    /// combination. Callers run this once per stored file and persist the result; values the user
    /// tuned away from the old defaults are preserved.
    pub fn migrate_legacy_defaults(&mut self) -> bool {
        let defaults = VadGateSettings::default();
        let mut changed = false;
        if self.vad_gate.confirmation_ms == LEGACY_VAD_CONFIRMATION_MS {
            self.vad_gate.confirmation_ms = defaults.confirmation_ms;
            changed = true;
        }
        if holds_value(self.vad_gate.floor_db, LEGACY_VAD_FLOOR_DB) {
            self.vad_gate.floor_db = defaults.floor_db;
            changed = true;
        }
        changed | self.agc2.repair_pass_through()
    }
}

fn holds_value(value: f32, expected: f32) -> bool {
    (value - expected).abs() <= f32::EPSILON
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VirtualDeviceMode {
    VbCable,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedVirtualAudioPair {
    pub render_endpoint_id: String,
    pub capture_endpoint_id: String,
    pub render_name: String,
    pub capture_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineSettings {
    pub follow_default_render: bool,
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
    pub capture_channel: Option<usize>,
    pub output_endpoint_id: Option<String>,
    pub virtual_device_mode: Option<VirtualDeviceMode>,
    pub virtual_capture_endpoint_id: Option<String>,
    pub trusted_virtual_audio_pairs: Vec<TrustedVirtualAudioPair>,
    #[serde(alias = "failurePolicy")]
    pub unprocessed_output: UnprocessedOutput,
    pub dsp: DspSettings,
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
            unprocessed_output: UnprocessedOutput::Silence,
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
        if self
            .capture_channel
            .is_some_and(|channel| channel > MAX_CAPTURE_CHANNEL_INDEX)
        {
            return Err(SettingsError::OutOfRange("capture channel"));
        }
        if !self.follow_default_render && self.render_endpoint_id.is_none() {
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
#[serde(rename_all = "camelCase")]
pub struct ActiveRoute {
    pub render_endpoint_id: String,
    pub capture_endpoint_id: String,
    pub output_endpoint_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveDeviceProfile {
    pub render_endpoint_id: String,
    pub capture_endpoint_id: String,
    pub customized: bool,
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
    PhysicalCapture,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub protocol_version: u16,
    pub running: bool,
    pub mode: EngineMode,
    pub detail: String,
    /// Filled in by the UI process, which owns the endpoint enumeration and its cache. The helper
    /// leaves this empty — it has no reason to enumerate devices just to answer a status poll.
    pub endpoints: Vec<EndpointDescriptor>,
    pub settings: EngineSettings,
    pub active_route: Option<ActiveRoute>,
    pub active_device_profile: Option<ActiveDeviceProfile>,
    pub configuration_issue: Option<ConfigurationIssue>,
    pub metrics: EngineMetrics,
    pub warning: Option<String>,
    /// Latched once the engine hears speech while the virtual microphone is not carrying processed
    /// audio. The UI turns this into a single notification per application run.
    pub unprocessed_speech_detected: bool,
}

impl EngineStatus {
    pub fn bootstrap(endpoints: Vec<EndpointDescriptor>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            running: false,
            mode: EngineMode::Sleeping,
            detail: "audio engine is stopped".to_owned(),
            endpoints,
            settings: EngineSettings::default(),
            active_route: None,
            active_device_profile: None,
            configuration_issue: None,
            metrics: EngineMetrics::default(),
            warning: None,
            unprocessed_speech_detected: false,
        }
    }
}

/// Renewed by the UI on every status poll. Only the UI process knows whether the Windows session is
/// usable, whether the app is exiting, and whether the one-shot "not processing" notice has already
/// been shown, so the helper never starts an idle session on its own guesswork. A helper that has not
/// heard from the UI yet holds the default: nothing runs, the microphone stays closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdleSessionPermit {
    /// The idle session may open audio devices; false while Windows is locked, disconnected, asleep,
    /// or while XMic is exiting.
    pub bypass_allowed: bool,
    /// Speech detection is still worth running because the notice has not been shown yet.
    pub speech_notice_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineRequest {
    Hello {
        protocol_version: u16,
    },
    GetStatus {
        #[serde(default)]
        permit: IdleSessionPermit,
    },
    UpdateSettings {
        settings: Box<EngineSettings>,
    },
    Start,
    Stop,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum EngineResponse {
    Hello { protocol_version: u16 },
    Status(Box<EngineStatus>),
    Ack,
    ConfigurationRequired(ConfigurationIssue),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_defaults_are_valid() {
        let settings = EngineSettings::default();
        settings.validate().unwrap();

        assert_eq!(settings.dsp.aec.preset, AecPreset::Natural);
        assert_eq!(settings.dsp.rnnoise.mix, 1.0);
        assert_eq!(settings.dsp.agc2.max_gain_db, 6.0);
        assert!(!settings.dsp.agc2.enabled);
        assert!(settings.dsp.agc2.adaptive_digital_enabled);
        assert!(settings.dsp.vad_gate.enabled);
        assert_eq!(settings.dsp.vad_gate.threshold, 0.70);
        assert_eq!(settings.dsp.vad_gate.confirmation_ms, 40);
        assert_eq!(settings.dsp.vad_gate.floor_db, -20.0);
        assert_eq!(settings.dsp.limiter.ceiling_dbfs, -1.0);
        assert_eq!(settings.unprocessed_output, UnprocessedOutput::Silence);
        assert_eq!(settings.capture_channel, None);
    }

    #[test]
    fn settings_written_as_a_dsp_failure_policy_still_load() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("unprocessedOutput");
        object.insert("failurePolicy".to_owned(), "rawPassthrough".into());

        let settings: EngineSettings = serde_json::from_value(value).unwrap();

        assert_eq!(
            settings.unprocessed_output,
            UnprocessedOutput::RawMicrophone
        );
    }

    #[test]
    fn the_legacy_mute_value_maps_to_silence() {
        let settings: EngineSettings = serde_json::from_str(r#"{"failurePolicy":"mute"}"#).unwrap();

        assert_eq!(settings.unprocessed_output, UnprocessedOutput::Silence);
    }

    #[test]
    fn unprocessed_output_writes_the_current_names() {
        let settings = EngineSettings {
            unprocessed_output: UnprocessedOutput::RawMicrophone,
            ..EngineSettings::default()
        };

        let encoded = serde_json::to_string(&settings).unwrap();

        assert!(encoded.contains("\"unprocessedOutput\":\"rawMicrophone\""));
        assert!(!encoded.contains("failurePolicy"));
    }

    #[test]
    fn a_helper_without_a_permit_keeps_the_microphone_closed() {
        // `permit` carries a `serde` default, so a request that predates it still parses — and has to
        // parse as "nothing is allowed to run" rather than as the permissive all-false-is-false trap.
        let request: EngineRequest = serde_json::from_str(r#"{"type":"getStatus"}"#).unwrap();

        let EngineRequest::GetStatus { permit, .. } = request else {
            panic!("expected a status request");
        };
        assert!(!permit.bypass_allowed);
        assert!(!permit.speech_notice_needed);
    }

    /// `ConfigurationIssue` is the only tagged enum in this protocol whose struct-variant fields
    /// cross into TypeScript, and `rename_all` on an enum renames variants only — the fields need
    /// `rename_all_fields`. Without it the wire carried `virtual_capture_endpoint_id` while
    /// `App.tsx` read `virtualCaptureEndpointId`, so an issue produced by the helper rendered its
    /// device name as "-". The frontend recomputes the same issue from the draft and wins almost
    /// every time, which is why that stayed invisible; this test makes sure it cannot come back.
    #[test]
    fn configuration_issue_fields_reach_typescript_in_camel_case() {
        let capture = serde_json::to_string(&ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
        })
        .unwrap();
        let device = serde_json::to_string(&ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
        })
        .unwrap();
        let conflict = serde_json::to_string(&ConfigurationIssue::VirtualRouteConflict {
            kind: VirtualRouteConflictKind::DefaultRender,
            endpoint_id: "cable-input".to_owned(),
        })
        .unwrap();

        assert_eq!(
            capture,
            r#"{"type":"physicalCaptureRequired","virtualCaptureEndpointId":"cable-output"}"#
        );
        assert_eq!(
            device,
            r#"{"type":"virtualDeviceRequired","reason":"customPairUnconfirmed"}"#
        );
        assert_eq!(
            conflict,
            r#"{"type":"virtualRouteConflict","kind":"defaultRender","endpointId":"cable-input"}"#
        );
    }

    #[test]
    fn old_settings_default_vad_confirmation() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value["dsp"]["vadGate"]
            .as_object_mut()
            .unwrap()
            .remove("confirmationMs");

        let settings: EngineSettings = serde_json::from_value(value).unwrap();

        assert_eq!(settings.dsp.vad_gate.confirmation_ms, 40);
    }

    #[test]
    fn migration_replaces_only_the_superseded_gate_defaults() {
        let mut dsp = DspSettings::default();
        dsp.vad_gate.confirmation_ms = LEGACY_VAD_CONFIRMATION_MS;
        dsp.vad_gate.floor_db = LEGACY_VAD_FLOOR_DB;
        dsp.vad_gate.hold_ms = 500;

        assert!(dsp.migrate_legacy_defaults());

        assert_eq!(dsp.vad_gate.confirmation_ms, 40);
        assert_eq!(dsp.vad_gate.floor_db, -20.0);
        assert_eq!(dsp.vad_gate.hold_ms, 500);
    }

    #[test]
    fn migration_keeps_hand_tuned_gate_values() {
        let mut dsp = DspSettings::default();
        dsp.vad_gate.confirmation_ms = 200;
        dsp.vad_gate.floor_db = -45.0;

        assert!(!dsp.migrate_legacy_defaults());

        assert_eq!(dsp.vad_gate.confirmation_ms, 200);
        assert_eq!(dsp.vad_gate.floor_db, -45.0);
    }

    #[test]
    fn migration_arms_a_pass_through_agc2() {
        let mut dsp = DspSettings::default();
        dsp.agc2.adaptive_digital_enabled = false;
        dsp.agc2.fixed_gain_db = 0.0;

        assert!(dsp.migrate_legacy_defaults());

        assert!(dsp.agc2.adaptive_digital_enabled);
    }

    #[test]
    fn migration_keeps_a_fixed_gain_only_agc2() {
        let mut dsp = DspSettings::default();
        dsp.agc2.adaptive_digital_enabled = false;
        dsp.agc2.fixed_gain_db = 6.0;

        assert!(!dsp.migrate_legacy_defaults());

        assert!(!dsp.agc2.adaptive_digital_enabled);
    }

    #[test]
    fn rejects_an_enabled_agc2_that_would_pass_through() {
        let mut settings = EngineSettings::default();
        settings.dsp.agc2.enabled = true;
        settings.dsp.agc2.adaptive_digital_enabled = false;

        assert_eq!(
            settings.validate(),
            Err(SettingsError::Agc2WouldPassThrough)
        );

        settings.dsp.agc2.fixed_gain_db = 3.0;
        settings.validate().unwrap();
    }

    #[test]
    fn a_disabled_pass_through_agc2_stays_loadable() {
        let mut settings = EngineSettings::default();
        settings.dsp.agc2.adaptive_digital_enabled = false;

        settings.validate().unwrap();
    }

    #[test]
    fn missing_settings_fields_use_current_defaults() {
        let settings: EngineSettings = serde_json::from_str("{}").unwrap();

        assert_eq!(settings, EngineSettings::default());
    }

    #[test]
    fn rejects_non_finite_settings() {
        let mut settings = EngineSettings::default();
        settings.dsp.rnnoise.mix = f32::NAN;
        assert_eq!(
            settings.validate(),
            Err(SettingsError::OutOfRange("RNNoise mix"))
        );
    }

    #[test]
    fn rejects_fixed_render_mode_without_an_endpoint() {
        let settings = EngineSettings {
            follow_default_render: false,
            ..EngineSettings::default()
        };

        assert_eq!(
            settings.validate(),
            Err(SettingsError::FixedRenderEndpointRequired)
        );
    }

    #[test]
    fn rejects_vad_gate_without_rnnoise_vad() {
        let mut settings = EngineSettings::default();
        settings.dsp.rnnoise.enabled = false;

        assert_eq!(settings.validate(), Err(SettingsError::VadRequiresRnnoise));
    }

    #[test]
    fn bootstrap_uses_current_protocol() {
        let status = EngineStatus::bootstrap(Vec::new());
        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
        assert!(!status.running);
        assert_eq!(status.mode, EngineMode::Sleeping);
        assert_eq!(status.settings, EngineSettings::default());
        assert!(!status.unprocessed_speech_detected);
    }
}
