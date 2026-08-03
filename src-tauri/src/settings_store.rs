use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use xmic_engine_protocol::EngineSettings;

/// Bumped whenever a stored file needs a one-shot rewrite. The version lives in the file but not in
/// `EngineSettings`, so it never crosses the IPC or Tauri boundary.
/// - 1: rewrote the superseded VAD gate defaults and repaired a pass-through AGC2.
/// - 2: renamed `failurePolicy` to `unprocessedOutput`; `serde` aliases read the old key, and the
///   rewrite is what makes the file stop carrying it.
const SETTINGS_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSettings {
    #[serde(default)]
    schema_version: u16,
    #[serde(flatten)]
    settings: EngineSettings,
}

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
        if let Some(stored) = load_valid_settings(&self.path) {
            return self.migrate(stored);
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

    /// Applies the steps the stored version is missing and records the new one, so a value the user
    /// later tunes back to an old default is never rewritten again.
    fn migrate(&self, mut stored: StoredSettings) -> EngineSettings {
        if stored.schema_version >= SETTINGS_SCHEMA_VERSION {
            return stored.settings;
        }
        if stored.schema_version < 1 {
            stored.settings.dsp.migrate_legacy_defaults();
        }
        if stored.settings.validate().is_ok() {
            let _ = self.save(&stored.settings);
        }
        stored.settings
    }

    pub fn save(&self, settings: &EngineSettings) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "settings path has no parent directory".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create settings directory {}: {error}", parent.display()))?;
        let stored = StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            settings: settings.clone(),
        };
        let contents = serde_json::to_string_pretty(&stored)
            .map_err(|error| format!("serialize settings: {error}"))?;
        fs::write(&self.path, contents)
            .map_err(|error| format!("write settings {}: {error}", self.path.display()))
    }
}

fn load_valid_settings(path: &Path) -> Option<StoredSettings> {
    let contents = fs::read_to_string(path).ok()?;
    let stored = serde_json::from_str::<StoredSettings>(&contents).ok()?;
    stored.settings.validate().ok()?;
    Some(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmic_engine_protocol::{
        AecPreset, TrustedVirtualAudioPair, UnprocessedOutput, VirtualDeviceMode,
        LEGACY_VAD_CONFIRMATION_MS, LEGACY_VAD_FLOOR_DB,
    };

    fn temporary_store(name: &str) -> (PathBuf, SettingsStore) {
        let directory =
            std::env::temp_dir().join(format!("xmic-settings-{}-{name}", std::process::id()));
        let path = directory.join("settings.json");
        let legacy = directory.join("audio-route.json");
        (directory, SettingsStore::new(path, legacy))
    }

    #[test]
    fn stored_settings_round_trip_with_the_schema_version() {
        let mut settings = EngineSettings::default();
        settings.dsp.vad_gate.threshold = 0.85;
        let stored = StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            settings: settings.clone(),
        };

        let encoded = serde_json::to_string(&stored).unwrap();
        let decoded: StoredSettings = serde_json::from_str(&encoded).unwrap();

        assert!(encoded.contains("\"schemaVersion\""));
        assert_eq!(decoded.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(decoded.settings, settings);
    }

    #[test]
    fn versionless_settings_migrate_once_and_keep_every_other_field() {
        let (directory, store) = temporary_store("migrate");
        let mut legacy_defaults = EngineSettings {
            follow_default_render: true,
            capture_endpoint_id: Some("physical-capture".to_owned()),
            capture_channel: Some(2),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            trusted_virtual_audio_pairs: vec![TrustedVirtualAudioPair {
                render_endpoint_id: "custom-render".to_owned(),
                capture_endpoint_id: "custom-capture".to_owned(),
                render_name: "Custom render".to_owned(),
                capture_name: "Custom capture".to_owned(),
            }],
            unprocessed_output: UnprocessedOutput::Silence,
            ..EngineSettings::default()
        };
        legacy_defaults.dsp.vad_gate.confirmation_ms = LEGACY_VAD_CONFIRMATION_MS;
        legacy_defaults.dsp.vad_gate.floor_db = LEGACY_VAD_FLOOR_DB;
        legacy_defaults.dsp.agc2.adaptive_digital_enabled = false;
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            &store.path,
            serde_json::to_string_pretty(&legacy_defaults).unwrap(),
        )
        .unwrap();

        let migrated = store.load();

        assert_eq!(
            migrated.dsp.vad_gate.confirmation_ms,
            EngineSettings::default().dsp.vad_gate.confirmation_ms
        );
        assert_eq!(migrated.dsp.vad_gate.floor_db, -20.0);
        assert!(migrated.dsp.agc2.adaptive_digital_enabled);
        // The schema version envelope must not cost any routing field on the way through.
        assert_eq!(
            migrated.capture_endpoint_id.as_deref(),
            Some("physical-capture")
        );
        assert_eq!(migrated.capture_channel, Some(2));
        assert_eq!(migrated.output_endpoint_id.as_deref(), Some("cable-input"));
        assert_eq!(
            migrated.virtual_device_mode,
            Some(VirtualDeviceMode::VbCable)
        );
        assert_eq!(
            migrated.virtual_capture_endpoint_id.as_deref(),
            Some("cable-output")
        );
        assert_eq!(
            migrated.trusted_virtual_audio_pairs,
            legacy_defaults.trusted_virtual_audio_pairs
        );

        // A value tuned back to an old default must survive because the version is now recorded.
        let mut retuned = migrated.clone();
        retuned.dsp.vad_gate.confirmation_ms = LEGACY_VAD_CONFIRMATION_MS;
        store.save(&retuned).unwrap();

        assert_eq!(
            store.load().dsp.vad_gate.confirmation_ms,
            LEGACY_VAD_CONFIRMATION_MS
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn full_settings_round_trip_through_json() {
        let mut settings = EngineSettings {
            capture_channel: Some(3),
            unprocessed_output: UnprocessedOutput::RawMicrophone,
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
        assert_eq!(settings.unprocessed_output, UnprocessedOutput::Silence);
        assert_eq!(settings.capture_channel, Some(1));
        assert_eq!(settings.render_endpoint_id.as_deref(), Some("render-id"));
    }

    #[test]
    fn a_schema_one_file_converges_to_the_new_key_without_retuning_dsp_values() {
        let (directory, store) = temporary_store("rename");
        let mut settings = EngineSettings {
            unprocessed_output: UnprocessedOutput::RawMicrophone,
            ..EngineSettings::default()
        };
        // A value the user deliberately tuned back to a superseded default: schema 1 already ran the
        // DSP migration, so bumping to 2 must not touch it again.
        settings.dsp.vad_gate.confirmation_ms = LEGACY_VAD_CONFIRMATION_MS;
        let stored = serde_json::to_value(StoredSettings {
            schema_version: 1,
            settings,
        })
        .unwrap();
        let mut object = stored.as_object().unwrap().clone();
        object.remove("unprocessedOutput");
        object.insert("failurePolicy".to_owned(), "rawPassthrough".into());
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.path, serde_json::to_string_pretty(&object).unwrap()).unwrap();

        let migrated = store.load();

        assert_eq!(
            migrated.unprocessed_output,
            UnprocessedOutput::RawMicrophone
        );
        assert_eq!(
            migrated.dsp.vad_gate.confirmation_ms,
            LEGACY_VAD_CONFIRMATION_MS
        );
        let rewritten = fs::read_to_string(&store.path).unwrap();
        assert!(rewritten.contains("\"unprocessedOutput\": \"rawMicrophone\""));
        assert!(!rewritten.contains("failurePolicy"));
        assert!(rewritten.contains("\"schemaVersion\": 2"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_legacy_fields_use_route_defaults() {
        let route: LegacyAudioRoute = serde_json::from_str("{}").unwrap();

        assert_eq!(route, LegacyAudioRoute::default());
    }
}
