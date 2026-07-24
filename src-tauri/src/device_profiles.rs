use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use xmic_engine_protocol::{ActiveDeviceProfile, DspSettings};
use xmic_platform_api::EndpointDescriptor;

const PROFILE_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone)]
pub struct ResolvedDeviceProfile {
    pub dsp: DspSettings,
    pub status: ActiveDeviceProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct StoredDeviceProfiles {
    schema_version: u16,
    profiles: Vec<StoredDeviceProfile>,
}

impl Default for StoredDeviceProfiles {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            profiles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDeviceProfile {
    render_endpoint_id: String,
    render_group_id: Option<String>,
    capture_endpoint_id: String,
    capture_group_id: Option<String>,
    customized: bool,
    dsp: DspSettings,
}

pub struct DeviceProfileStore {
    path: PathBuf,
    profiles: Mutex<StoredDeviceProfiles>,
}

impl DeviceProfileStore {
    pub fn new(path: PathBuf) -> Self {
        let profiles = load_profiles(&path).unwrap_or_default();
        Self {
            path,
            profiles: Mutex::new(profiles),
        }
    }

    pub fn resolve(
        &self,
        render: &EndpointDescriptor,
        capture: &EndpointDescriptor,
        initial_dsp: &DspSettings,
    ) -> Result<ResolvedDeviceProfile, String> {
        let mut profiles = lock_unpoisoned(&self.profiles);
        let (index, changed) = resolve_profile_index(&mut profiles, render, capture, initial_dsp);
        if changed {
            save_profiles(&self.path, &profiles)?;
        }
        Ok(resolved_profile(&profiles.profiles[index]))
    }

    pub fn update(&self, active: &ActiveDeviceProfile, dsp: &DspSettings) -> Result<(), String> {
        let mut profiles = lock_unpoisoned(&self.profiles);
        let profile = profiles
            .profiles
            .iter_mut()
            .find(|profile| {
                profile.render_endpoint_id == active.render_endpoint_id
                    && profile.capture_endpoint_id == active.capture_endpoint_id
            })
            .ok_or_else(|| "active device profile no longer exists".to_owned())?;
        if profile.dsp == *dsp {
            return Ok(());
        }
        profile.dsp = dsp.clone();
        profile.customized = true;
        save_profiles(&self.path, &profiles)
    }
}

fn resolve_profile_index(
    stored: &mut StoredDeviceProfiles,
    render: &EndpointDescriptor,
    capture: &EndpointDescriptor,
    initial_dsp: &DspSettings,
) -> (usize, bool) {
    if let Some(index) = stored.profiles.iter().position(|profile| {
        profile.render_endpoint_id == render.id && profile.capture_endpoint_id == capture.id
    }) {
        return (index, false);
    }

    let group_matches = stored
        .profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            same_known_group(
                profile.render_group_id.as_deref(),
                render.group_id.as_deref(),
            ) && same_known_group(
                profile.capture_group_id.as_deref(),
                capture.group_id.as_deref(),
            )
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if let [index] = group_matches.as_slice() {
        let profile = &mut stored.profiles[*index];
        profile.render_endpoint_id.clone_from(&render.id);
        profile.render_group_id.clone_from(&render.group_id);
        profile.capture_endpoint_id.clone_from(&capture.id);
        profile.capture_group_id.clone_from(&capture.group_id);
        return (*index, true);
    }

    let first_profile = stored.profiles.is_empty();
    stored.profiles.push(StoredDeviceProfile {
        render_endpoint_id: render.id.clone(),
        render_group_id: render.group_id.clone(),
        capture_endpoint_id: capture.id.clone(),
        capture_group_id: capture.group_id.clone(),
        customized: first_profile,
        dsp: if first_profile {
            initial_dsp.clone()
        } else {
            DspSettings::default()
        },
    });
    (stored.profiles.len() - 1, true)
}

fn same_known_group(stored: Option<&str>, current: Option<&str>) -> bool {
    matches!((stored, current), (Some(left), Some(right)) if left == right)
}

fn resolved_profile(profile: &StoredDeviceProfile) -> ResolvedDeviceProfile {
    ResolvedDeviceProfile {
        dsp: profile.dsp.clone(),
        status: ActiveDeviceProfile {
            render_endpoint_id: profile.render_endpoint_id.clone(),
            capture_endpoint_id: profile.capture_endpoint_id.clone(),
            customized: profile.customized,
        },
    }
}

fn load_profiles(path: &Path) -> Option<StoredDeviceProfiles> {
    let contents = fs::read_to_string(path).ok()?;
    let profiles = serde_json::from_str::<StoredDeviceProfiles>(&contents).ok()?;
    (profiles.schema_version == PROFILE_SCHEMA_VERSION).then_some(profiles)
}

fn save_profiles(path: &Path, profiles: &StoredDeviceProfiles) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "device profile path has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create device profile directory {}: {error}",
            parent.display()
        )
    })?;
    let contents = serde_json::to_string_pretty(profiles)
        .map_err(|error| format!("serialize device profiles: {error}"))?;
    fs::write(path, contents)
        .map_err(|error| format!("write device profiles {}: {error}", path.display()))
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmic_platform_api::{DataFlow, DeviceRole};

    fn endpoint(id: &str, group_id: Option<&str>) -> EndpointDescriptor {
        EndpointDescriptor {
            platform: "windows".to_owned(),
            id: id.to_owned(),
            group_id: group_id.map(str::to_owned),
            friendly_name: id.to_owned(),
            flow: DataFlow::Render,
            default_roles: vec![DeviceRole::Multimedia],
        }
    }

    #[test]
    fn first_pair_migrates_the_existing_dsp_settings() {
        let mut stored = StoredDeviceProfiles::default();
        let mut initial = DspSettings::default();
        initial.aec.enabled = false;

        let (index, changed) = resolve_profile_index(
            &mut stored,
            &endpoint("render", Some("render-group")),
            &endpoint("capture", Some("capture-group")),
            &initial,
        );

        assert!(changed);
        assert!(stored.profiles[index].customized);
        assert!(!stored.profiles[index].dsp.aec.enabled);
    }

    #[test]
    fn later_pairs_start_from_accepted_defaults() {
        let mut stored = StoredDeviceProfiles::default();
        let mut initial = DspSettings::default();
        initial.aec.enabled = false;
        resolve_profile_index(
            &mut stored,
            &endpoint("speaker", Some("speaker-group")),
            &endpoint("desk-mic", Some("desk-mic-group")),
            &initial,
        );

        let (index, _) = resolve_profile_index(
            &mut stored,
            &endpoint("headset", Some("headset-group")),
            &endpoint("headset-mic", Some("headset-mic-group")),
            &initial,
        );

        assert!(!stored.profiles[index].customized);
        assert_eq!(stored.profiles[index].dsp, DspSettings::default());
    }

    #[test]
    fn unique_container_pair_migrates_changed_endpoint_ids() {
        let mut stored = StoredDeviceProfiles::default();
        resolve_profile_index(
            &mut stored,
            &endpoint("old-render", Some("render-group")),
            &endpoint("old-capture", Some("capture-group")),
            &DspSettings::default(),
        );

        let (index, changed) = resolve_profile_index(
            &mut stored,
            &endpoint("new-render", Some("render-group")),
            &endpoint("new-capture", Some("capture-group")),
            &DspSettings::default(),
        );

        assert!(changed);
        assert_eq!(stored.profiles.len(), 1);
        assert_eq!(stored.profiles[index].render_endpoint_id, "new-render");
        assert_eq!(stored.profiles[index].capture_endpoint_id, "new-capture");
    }

    #[test]
    fn unchanged_defaults_do_not_mark_a_profile_as_customized() {
        let directory = std::env::temp_dir().join(format!(
            "xmic-device-profile-{}-unchanged",
            std::process::id()
        ));
        let path = directory.join("device-profiles.json");
        let store = DeviceProfileStore::new(path);
        let initial = DspSettings::default();
        store
            .resolve(
                &endpoint("first-render", Some("first-render-group")),
                &endpoint("first-capture", Some("first-capture-group")),
                &initial,
            )
            .unwrap();
        let profile = store
            .resolve(
                &endpoint("second-render", Some("second-render-group")),
                &endpoint("second-capture", Some("second-capture-group")),
                &initial,
            )
            .unwrap();

        assert!(!profile.status.customized);
        store.update(&profile.status, &profile.dsp).unwrap();
        let resolved = store
            .resolve(
                &endpoint("second-render", Some("second-render-group")),
                &endpoint("second-capture", Some("second-capture-group")),
                &initial,
            )
            .unwrap();

        assert!(!resolved.status.customized);
        let _ = fs::remove_dir_all(directory);
    }
}
