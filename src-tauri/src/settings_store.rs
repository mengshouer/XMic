use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use xmic_engine_protocol::EngineSettings;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LegacyAudioRoute {
    follow_default_render: bool,
    render_endpoint_id: Option<String>,
    capture_endpoint_id: Option<String>,
    capture_channel: Option<usize>,
    output_endpoint_id: Option<String>,
}

impl Default for LegacyAudioRoute {
    fn default() -> Self {
        Self {
            follow_default_render: true,
            render_endpoint_id: None,
            capture_endpoint_id: None,
            capture_channel: None,
            output_endpoint_id: None,
        }
    }
}

impl LegacyAudioRoute {
    fn apply_to(self, settings: &mut EngineSettings) {
        settings.follow_default_render = self.follow_default_render;
        settings.render_endpoint_id = self.render_endpoint_id;
        settings.capture_endpoint_id = self.capture_endpoint_id;
        settings.capture_channel = self.capture_channel;
        settings.output_endpoint_id = self.output_endpoint_id;
    }
}

pub struct SettingsStore {
    path: PathBuf,
    legacy_route_path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf, legacy_route_path: PathBuf) -> Self {
        Self {
            path,
            legacy_route_path,
        }
    }

    pub fn load(&self) -> EngineSettings {
        if let Some(settings) = load_valid_settings(&self.path) {
            return settings;
        }
        let mut settings = EngineSettings::default();
        let Ok(contents) = fs::read_to_string(&self.legacy_route_path) else {
            return settings;
        };
        let Ok(route) = serde_json::from_str::<LegacyAudioRoute>(&contents) else {
            return settings;
        };
        route.apply_to(&mut settings);
        if settings.validate().is_ok() {
            settings
        } else {
            EngineSettings::default()
        }
    }

    pub fn save(&self, settings: &EngineSettings) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "settings path has no parent directory".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create settings directory {}: {error}", parent.display()))?;
        let contents = serde_json::to_string_pretty(settings)
            .map_err(|error| format!("serialize settings: {error}"))?;
        fs::write(&self.path, contents)
            .map_err(|error| format!("write settings {}: {error}", self.path.display()))
    }
}

fn load_valid_settings(path: &Path) -> Option<EngineSettings> {
    let contents = fs::read_to_string(path).ok()?;
    let settings = serde_json::from_str::<EngineSettings>(&contents).ok()?;
    settings.validate().ok()?;
    Some(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmic_engine_protocol::{AecPreset, DspFailurePolicy};

    #[test]
    fn full_settings_round_trip_through_json() {
        let mut settings = EngineSettings {
            capture_channel: Some(3),
            failure_policy: DspFailurePolicy::RawPassthrough,
            ..EngineSettings::default()
        };
        settings.dsp.aec.preset = AecPreset::Strong;
        settings.dsp.vad_gate.threshold = 0.85;

        let encoded = serde_json::to_string(&settings).unwrap();
        let decoded: EngineSettings = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, settings);
    }

    #[test]
    fn legacy_route_only_replaces_routing_fields() {
        let mut settings = EngineSettings::default();
        settings.dsp.aec.preset = AecPreset::Strong;
        let route = LegacyAudioRoute {
            follow_default_render: false,
            render_endpoint_id: Some("render-id".to_owned()),
            capture_endpoint_id: Some("capture-id".to_owned()),
            capture_channel: Some(1),
            output_endpoint_id: Some("output-id".to_owned()),
        };

        route.apply_to(&mut settings);

        assert_eq!(settings.dsp.aec.preset, AecPreset::Strong);
        assert_eq!(settings.failure_policy, DspFailurePolicy::Mute);
        assert_eq!(settings.capture_channel, Some(1));
        assert_eq!(settings.render_endpoint_id.as_deref(), Some("render-id"));
    }

    #[test]
    fn missing_legacy_fields_use_route_defaults() {
        let route: LegacyAudioRoute = serde_json::from_str("{}").unwrap();

        assert_eq!(route, LegacyAudioRoute::default());
    }
}
