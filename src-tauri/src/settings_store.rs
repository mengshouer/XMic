use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};
use xmic_engine_protocol::EngineSettings;

/// This schema is intentionally a clean break. Files from earlier schemas are never guessed at or
/// migrated; invalid files are rewritten to defaults with a reset latch that only explicit Apply clears.
const SETTINGS_SCHEMA_VERSION: u16 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredSettings {
    schema_version: u16,
    settings: EngineSettings,
}

#[derive(Debug, Clone)]
pub struct SettingsLoad {
    pub settings: EngineSettings,
    /// Present when an existing file was invalid and defaults remain pending explicit Apply.
    pub reset_reason: Option<String>,
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> SettingsLoad {
        if !self.path.exists() {
            return SettingsLoad {
                settings: EngineSettings::default(),
                reset_reason: self.read_reset_reason(),
            };
        }

        let loaded = fs::read_to_string(&self.path)
            .map_err(|error| format!("read settings: {error}"))
            .and_then(|contents| {
                serde_json::from_str::<StoredSettings>(&contents)
                    .map_err(|error| format!("parse settings: {error}"))
            })
            .and_then(|stored| {
                if stored.schema_version != SETTINGS_SCHEMA_VERSION {
                    return Err(format!(
                        "settings schema {} is not supported by schema {}",
                        stored.schema_version, SETTINGS_SCHEMA_VERSION
                    ));
                }
                stored
                    .settings
                    .validate()
                    .map_err(|error| format!("validate settings: {error}"))?;
                Ok(stored.settings)
            });

        match loaded {
            Ok(settings) => {
                let reset_reason = self.read_reset_reason();
                SettingsLoad {
                    settings: if reset_reason.is_some() {
                        EngineSettings::default()
                    } else {
                        settings
                    },
                    reset_reason,
                }
            }
            Err(error) => {
                let settings = EngineSettings::default();
                let reason = format!("{error}; using accepted defaults until Apply");
                // Replace the invalid file with the current defaults, but persist a sidecar latch so
                // a helper restart cannot mistake those defaults for an acknowledged Apply. Only a
                // successful `save()` removes the latch.
                let marker_result = self.write_reset_marker(&reason);
                if marker_result.is_ok() {
                    let _ = self.write_settings_file(&settings);
                }
                SettingsLoad {
                    settings,
                    reset_reason: Some(match marker_result {
                        Ok(()) => reason,
                        Err(marker_error) => {
                            format!("{reason}; could not persist reset latch: {marker_error}")
                        }
                    }),
                }
            }
        }
    }

    pub fn save(&self, settings: &EngineSettings) -> Result<(), String> {
        settings.validate().map_err(|error| error.to_string())?;
        self.write_settings_file(settings)?;
        let marker = self.reset_marker_path();
        if marker.exists() {
            fs::remove_file(&marker).map_err(|error| {
                format!("clear settings reset latch {}: {error}", marker.display())
            })?;
        }
        Ok(())
    }

    fn write_settings_file(&self, settings: &EngineSettings) -> Result<(), String> {
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

    fn reset_marker_path(&self) -> PathBuf {
        let mut marker = self.path.as_os_str().to_os_string();
        marker.push(".reset");
        PathBuf::from(marker)
    }

    fn read_reset_reason(&self) -> Option<String> {
        match fs::read_to_string(self.reset_marker_path()) {
            Ok(reason) => {
                let reason = reason.trim();
                Some(if reason.contains("until Apply") {
                    reason.to_owned()
                } else {
                    format!("{reason}; using accepted defaults until Apply")
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(format!(
                "settings reset latch could not be read ({error}); using accepted defaults until Apply"
            )),
        }
    }

    fn write_reset_marker(&self, reason: &str) -> Result<(), String> {
        let marker = self.reset_marker_path();
        let parent = marker
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| "settings reset marker has no parent directory".to_owned())?;
        fs::create_dir_all(&parent).map_err(|error| {
            format!(
                "create settings reset marker directory {}: {error}",
                parent.display()
            )
        })?;
        fs::write(&marker, reason)
            .map_err(|error| format!("write settings reset marker {}: {error}", marker.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use xmic_engine_protocol::DegradedOutput;

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temporary_store(name: &str) -> (PathBuf, SettingsStore) {
        let directory = std::env::temp_dir().join(format!(
            "xmic-settings-{}-{name}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let path = directory.join("settings.json");
        (directory, SettingsStore::new(path))
    }

    #[test]
    fn current_settings_round_trip() {
        let (directory, store) = temporary_store("round-trip");
        let settings = EngineSettings {
            capture_channel: Some(3),
            degraded_output: DegradedOutput::Raw,
            ..EngineSettings::default()
        };

        store.save(&settings).unwrap();
        let loaded = store.load();

        assert_eq!(loaded.settings, settings);
        assert_eq!(loaded.reset_reason, None);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn old_schema_is_reset_and_the_reset_is_visible() {
        let (directory, store) = temporary_store("old-schema");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            &store.path,
            serde_json::to_string_pretty(&StoredSettings {
                schema_version: 2,
                settings: EngineSettings::default(),
            })
            .unwrap(),
        )
        .unwrap();

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert!(loaded
            .reset_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not supported")));
        let rewritten = fs::read_to_string(&store.path).unwrap();
        assert!(rewritten.contains("\"schemaVersion\": 3"));
        assert!(store.reset_marker_path().is_file());
        let again = store.load();
        assert!(again.reset_reason.is_some());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn reset_latch_keeps_defaults_pending_across_a_valid_file_reload() {
        let (directory, store) = temporary_store("reset-latch");
        fs::create_dir_all(&directory).unwrap();
        store.save(&EngineSettings::default()).unwrap();
        fs::write(store.reset_marker_path(), "manual reset").unwrap();

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert!(loaded.reset_reason.is_some());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_settings_file_does_not_clear_an_existing_reset_latch() {
        let (directory, store) = temporary_store("missing-with-reset-latch");
        fs::create_dir_all(&directory).unwrap();
        fs::write(store.reset_marker_path(), "invalid settings were reset").unwrap();

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert!(loaded.reset_reason.is_some());
        assert!(store.reset_marker_path().is_file());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn applying_settings_acknowledges_and_clears_a_reset() {
        let (directory, store) = temporary_store("reset-ack");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.path, r#"{"schemaVersion":2}"#).unwrap();

        assert!(store.load().reset_reason.is_some());
        store.save(&EngineSettings::default()).unwrap();
        assert_eq!(store.load().reset_reason, None);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn old_aliases_are_reset_instead_of_migrated() {
        let (directory, store) = temporary_store("aliases");
        fs::create_dir_all(&directory).unwrap();
        let mut value = serde_json::to_value(StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            settings: EngineSettings::default(),
        })
        .unwrap();
        value["settings"]["failurePolicy"] = "mute".into();
        fs::write(&store.path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert!(loaded.reset_reason.is_some());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_current_schema_field_exposes_a_reset_and_uses_defaults() {
        let (directory, store) = temporary_store("missing-field");
        fs::create_dir_all(&directory).unwrap();
        let mut value = serde_json::to_value(StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            settings: EngineSettings::default(),
        })
        .unwrap();
        value["settings"]["dsp"]["vadGate"]
            .as_object_mut()
            .unwrap()
            .remove("enabled");
        fs::write(&store.path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert!(loaded.reset_reason.is_some());
        assert!(loaded.settings.dsp.vad_gate.enabled);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_nullable_fields_reset_settings_until_apply() {
        let defaults = serde_json::to_value(EngineSettings::default()).unwrap();
        let settings = EngineSettings {
            capture_endpoint_id: Some("selected-microphone".to_owned()),
            degraded_output: DegradedOutput::Raw,
            ..EngineSettings::default()
        };
        for (field, value) in defaults.as_object().unwrap() {
            if !value.is_null() {
                continue;
            }
            let (directory, store) = temporary_store(field);
            store.save(&settings).unwrap();
            let mut stored = serde_json::to_value(StoredSettings {
                schema_version: SETTINGS_SCHEMA_VERSION,
                settings: settings.clone(),
            })
            .unwrap();
            stored["settings"].as_object_mut().unwrap().remove(field);
            fs::write(&store.path, serde_json::to_string_pretty(&stored).unwrap()).unwrap();

            let loaded = store.load();

            assert_eq!(loaded.settings, EngineSettings::default());
            assert!(loaded
                .reset_reason
                .as_deref()
                .is_some_and(|reason| reason.contains(&format!("missing field `{field}`"))));
            assert!(store.reset_marker_path().is_file());
            let again = store.load();
            assert_eq!(again.settings, EngineSettings::default());
            assert_eq!(again.reset_reason, loaded.reset_reason);
            store.save(&settings).unwrap();
            let applied = store.load();
            assert_eq!(applied.settings, settings);
            assert_eq!(applied.reset_reason, None);
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn missing_file_uses_defaults_without_claiming_a_reset() {
        let (directory, store) = temporary_store("missing");

        let loaded = store.load();

        assert_eq!(loaded.settings, EngineSettings::default());
        assert_eq!(loaded.reset_reason, None);
        let _ = fs::remove_dir_all(directory);
    }
}
