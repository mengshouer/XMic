#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use xmic_platform_api::EndpointDescriptor;

pub const PROTOCOL_VERSION: u16 = 9;
pub const MAX_CAPTURE_CHANNEL_INDEX: usize = 31;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DspFailurePolicy {
    RawPassthrough,
    #[default]
    Mute,
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
            floor_db: -90.0,
            attack_ms: 10.0,
            release_ms: 150.0,
        }
    }
}

const fn default_vad_confirmation_ms() -> u32 {
    120
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineSettings {
    pub follow_default_render: bool,
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
    pub capture_channel: Option<usize>,
    pub output_endpoint_id: Option<String>,
    pub failure_policy: DspFailurePolicy,
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
            failure_policy: DspFailurePolicy::Mute,
            dsp: DspSettings::default(),
        }
    }
}

impl EngineSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        validate_optional_endpoint(&self.render_endpoint_id)?;
        validate_optional_endpoint(&self.capture_endpoint_id)?;
        validate_optional_endpoint(&self.output_endpoint_id)?;
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineMetrics {
    pub processed_blocks: u64,
    pub render_underruns: u64,
    pub render_overflows: u64,
    pub capture_overflows: u64,
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
    pub endpoints: Vec<EndpointDescriptor>,
    pub settings: EngineSettings,
    pub active_route: Option<ActiveRoute>,
    pub active_device_profile: Option<ActiveDeviceProfile>,
    pub metrics: EngineMetrics,
    pub warning: Option<String>,
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
            metrics: EngineMetrics::default(),
            warning: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EngineRequest {
    Hello { protocol_version: u16 },
    GetStatus { include_endpoints: bool },
    UpdateSettings { settings: EngineSettings },
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
        assert!(settings.dsp.vad_gate.enabled);
        assert_eq!(settings.dsp.vad_gate.threshold, 0.70);
        assert_eq!(settings.dsp.vad_gate.confirmation_ms, 120);
        assert_eq!(settings.dsp.vad_gate.floor_db, -90.0);
        assert_eq!(settings.dsp.limiter.ceiling_dbfs, -1.0);
        assert_eq!(settings.failure_policy, DspFailurePolicy::Mute);
        assert_eq!(settings.capture_channel, None);
    }

    #[test]
    fn old_settings_default_vad_confirmation() {
        let mut value = serde_json::to_value(EngineSettings::default()).unwrap();
        value["dsp"]["vadGate"]
            .as_object_mut()
            .unwrap()
            .remove("confirmationMs");

        let settings: EngineSettings = serde_json::from_value(value).unwrap();

        assert_eq!(settings.dsp.vad_gate.confirmation_ms, 120);
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
    }
}
