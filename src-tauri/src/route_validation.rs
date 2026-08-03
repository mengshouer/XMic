use xmic_engine_protocol::{
    ConfigurationIssue, EngineSettings, TrustedVirtualAudioPair, VirtualDeviceIssueReason,
    VirtualDeviceMode, VirtualRouteConflictKind,
};
use xmic_platform_api::{DataFlow, DeviceRole, EndpointDescriptor};

#[derive(Debug, Clone, Copy)]
pub(crate) struct VirtualAudioPair<'a> {
    pub render: &'a EndpointDescriptor,
    pub capture: &'a EndpointDescriptor,
}

pub(crate) fn standard_vb_cable_pair(
    endpoints: &[EndpointDescriptor],
) -> Option<VirtualAudioPair<'_>> {
    let renders = endpoints
        .iter()
        .filter(|endpoint| is_standard_vb_cable_render(endpoint))
        .collect::<Vec<_>>();
    let captures = endpoints
        .iter()
        .filter(|endpoint| is_standard_vb_cable_capture(endpoint))
        .collect::<Vec<_>>();

    let grouped = renders.iter().find_map(|render| {
        captures
            .iter()
            .find(|capture| same_known_group(render, capture))
            .map(|capture| VirtualAudioPair { render, capture })
    });
    grouped.or(match (renders.as_slice(), captures.as_slice()) {
        ([render], [capture]) => Some(VirtualAudioPair { render, capture }),
        _ => None,
    })
}

pub(crate) fn reconcile_virtual_device_settings(
    settings: &mut EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    let before = settings.clone();
    let vb_cable = standard_vb_cable_pair(endpoints);

    match settings.virtual_device_mode {
        None => match settings.output_endpoint_id.as_deref() {
            Some(output_id) if vb_cable.is_some_and(|pair| pair.render.id == output_id) => {
                apply_vb_cable(settings, vb_cable.expect("checked VB-CABLE pair"));
            }
            Some(output_id)
                if find_endpoint(endpoints, DataFlow::Render, output_id)
                    .is_some_and(|endpoint| !is_standard_vb_cable_render(endpoint)) =>
            {
                settings.virtual_device_mode = Some(VirtualDeviceMode::Custom);
                settings.virtual_capture_endpoint_id = None;
            }
            Some(_) => {}
            None => {
                if let Some(pair) = vb_cable {
                    apply_vb_cable(settings, pair);
                }
            }
        },
        Some(VirtualDeviceMode::VbCable) => {
            if let Some(pair) = vb_cable {
                apply_vb_cable(settings, pair);
            }
        }
        Some(VirtualDeviceMode::Custom) => {}
    }

    *settings != before
}

/// Decides whether the audio route is usable, and if not, which of the three things the user has to
/// fix.
///
/// **This function's behaviour is defined by `fixtures/route-validation-cases.json`, and
/// `getConfigurationIssue` in `src/routeValidation.ts` must satisfy the same vectors.** The frontend
/// keeps its
/// own copy so it can price an unapplied draft without a round trip; the fixture is what stops the
/// two from drifting apart. Add a vector for a new rule — do not add an inline test here.
pub(crate) fn configuration_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    match settings.virtual_device_mode {
        Some(VirtualDeviceMode::Custom) => custom_configuration_issue(settings, endpoints),
        Some(VirtualDeviceMode::VbCable) | None => {
            vb_cable_configuration_issue(settings, endpoints)
        }
    }
}

pub(crate) fn default_multimedia_capture(
    endpoints: &[EndpointDescriptor],
) -> Option<&EndpointDescriptor> {
    endpoints.iter().find(|endpoint| {
        endpoint.flow == DataFlow::Capture
            && endpoint.default_roles.contains(&DeviceRole::Multimedia)
    })
}

fn apply_vb_cable(settings: &mut EngineSettings, pair: VirtualAudioPair<'_>) {
    settings.virtual_device_mode = Some(VirtualDeviceMode::VbCable);
    settings.output_endpoint_id = Some(pair.render.id.clone());
    settings.virtual_capture_endpoint_id = Some(pair.capture.id.clone());
}

fn vb_cable_configuration_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    let pair = standard_vb_cable_pair(endpoints).ok_or(ConfigurationIssue::VirtualDeviceRequired {
        reason: VirtualDeviceIssueReason::VbCableUnavailable,
    });
    let pair = match pair {
        Ok(pair) => pair,
        Err(issue) => return Some(issue),
    };
    let capture = settings
        .capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id))
        .or_else(|| {
            settings
                .capture_endpoint_id
                .is_none()
                .then(|| default_multimedia_capture(endpoints))
                .flatten()
        });
    if capture.is_some_and(|capture| {
        same_endpoint_or_group(capture, pair.capture)
            || same_endpoint_or_group(capture, pair.render)
            || is_standard_vb_cable_capture(capture)
    }) {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: capture.map(|endpoint| endpoint.id.clone()),
        });
    }

    render_conflict(settings, endpoints, pair.render)
}

fn custom_configuration_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    if settings.capture_endpoint_id.is_none() {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: settings.virtual_capture_endpoint_id.clone(),
        });
    }
    let (Some(output_id), Some(virtual_capture_id)) = (
        settings.output_endpoint_id.as_deref(),
        settings.virtual_capture_endpoint_id.as_deref(),
    ) else {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairIncomplete,
        });
    };
    if !pair_is_trusted(
        output_id,
        virtual_capture_id,
        &settings.trusted_virtual_audio_pairs,
    ) {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
        });
    }

    let physical_capture = settings
        .capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id));
    let virtual_render = find_endpoint(endpoints, DataFlow::Render, output_id);
    let virtual_capture = find_endpoint(endpoints, DataFlow::Capture, virtual_capture_id);
    if physical_capture.is_some_and(|physical| {
        virtual_render
            .is_some_and(|virtual_render| same_endpoint_or_group(physical, virtual_render))
            || virtual_capture
                .is_some_and(|virtual_capture| same_endpoint_or_group(physical, virtual_capture))
    }) {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: physical_capture.map(|endpoint| endpoint.id.clone()),
        });
    }

    virtual_render.and_then(|output| render_conflict(settings, endpoints, output))
}

fn render_conflict(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
    output: &EndpointDescriptor,
) -> Option<ConfigurationIssue> {
    let render = if settings.follow_default_render {
        endpoints.iter().find(|endpoint| {
            endpoint.flow == DataFlow::Render
                && endpoint.default_roles.contains(&DeviceRole::Multimedia)
        })
    } else {
        settings
            .render_endpoint_id
            .as_deref()
            .and_then(|id| find_endpoint(endpoints, DataFlow::Render, id))
    };
    let render = render?;
    if !same_endpoint_or_group(render, output) {
        return None;
    }
    Some(ConfigurationIssue::VirtualRouteConflict {
        kind: if settings.follow_default_render {
            VirtualRouteConflictKind::DefaultRender
        } else {
            VirtualRouteConflictKind::FixedRender
        },
        endpoint_id: render.id.clone(),
    })
}

fn pair_is_trusted(
    render_id: &str,
    capture_id: &str,
    trusted_pairs: &[TrustedVirtualAudioPair],
) -> bool {
    trusted_pairs
        .iter()
        .any(|pair| pair.render_endpoint_id == render_id && pair.capture_endpoint_id == capture_id)
}

fn find_endpoint<'a>(
    endpoints: &'a [EndpointDescriptor],
    flow: DataFlow,
    id: &str,
) -> Option<&'a EndpointDescriptor> {
    endpoints
        .iter()
        .find(|endpoint| endpoint.flow == flow && endpoint.id == id)
}

fn is_standard_vb_cable_render(endpoint: &EndpointDescriptor) -> bool {
    endpoint.flow == DataFlow::Render && standard_vb_name(&endpoint.friendly_name, "cable input")
}

fn is_standard_vb_cable_capture(endpoint: &EndpointDescriptor) -> bool {
    endpoint.flow == DataFlow::Capture && standard_vb_name(&endpoint.friendly_name, "cable output")
}

fn standard_vb_name(name: &str, endpoint_name: &str) -> bool {
    let name = name.trim().to_lowercase();
    name == endpoint_name
        || (name
            .strip_prefix(endpoint_name)
            .is_some_and(|rest| rest.starts_with(" ("))
            && name.contains("vb-audio virtual cable"))
}

fn same_known_group(left: &EndpointDescriptor, right: &EndpointDescriptor) -> bool {
    left.group_id
        .as_deref()
        .zip(right.group_id.as_deref())
        .is_some_and(|(left, right)| !left.is_empty() && left == right)
}

fn same_endpoint_or_group(left: &EndpointDescriptor, right: &EndpointDescriptor) -> bool {
    left.id == right.id || same_known_group(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(
        id: &str,
        name: &str,
        flow: DataFlow,
        group_id: Option<&str>,
        default_roles: Vec<DeviceRole>,
    ) -> EndpointDescriptor {
        EndpointDescriptor {
            platform: "windows".to_owned(),
            id: id.to_owned(),
            group_id: group_id.map(str::to_owned),
            friendly_name: name.to_owned(),
            flow,
            default_roles,
        }
    }

    fn cable_pair() -> [EndpointDescriptor; 2] {
        [
            endpoint(
                "cable-input",
                "CABLE Input (VB-Audio Virtual Cable)",
                DataFlow::Render,
                Some("cable"),
                Vec::new(),
            ),
            endpoint(
                "cable-output",
                "CABLE Output (VB-Audio Virtual Cable)",
                DataFlow::Capture,
                Some("cable"),
                vec![DeviceRole::Multimedia],
            ),
        ]
    }

    #[test]
    fn legacy_standard_output_migrates_to_vb_cable() {
        let mut settings = EngineSettings {
            output_endpoint_id: Some("cable-input".to_owned()),
            ..EngineSettings::default()
        };

        assert!(reconcile_virtual_device_settings(
            &mut settings,
            &cable_pair()
        ));
        assert_eq!(
            settings.virtual_device_mode,
            Some(VirtualDeviceMode::VbCable)
        );
        assert_eq!(
            settings.virtual_capture_endpoint_id.as_deref(),
            Some("cable-output")
        );
    }

    #[test]
    fn legacy_custom_output_requires_confirmation() {
        let mut settings = EngineSettings {
            output_endpoint_id: Some("custom-render".to_owned()),
            ..EngineSettings::default()
        };
        let endpoints = [endpoint(
            "custom-render",
            "Custom virtual input",
            DataFlow::Render,
            Some("custom"),
            vec![],
        )];

        assert!(reconcile_virtual_device_settings(&mut settings, &endpoints));
        assert_eq!(
            settings.virtual_device_mode,
            Some(VirtualDeviceMode::Custom)
        );
        assert_eq!(settings.virtual_capture_endpoint_id, None);
    }

    #[test]
    fn offline_legacy_output_waits_for_identification() {
        let mut settings = EngineSettings {
            output_endpoint_id: Some("offline-render".to_owned()),
            ..EngineSettings::default()
        };

        assert!(!reconcile_virtual_device_settings(&mut settings, &[]));
        assert_eq!(settings.virtual_device_mode, None);
        assert_eq!(
            settings.output_endpoint_id.as_deref(),
            Some("offline-render")
        );
    }

    #[test]
    fn explicit_custom_mode_is_not_replaced_by_vb_cable() {
        let mut settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("microphone".to_owned()),
            output_endpoint_id: Some("custom-render".to_owned()),
            virtual_capture_endpoint_id: Some("custom-capture".to_owned()),
            ..EngineSettings::default()
        };

        assert!(!reconcile_virtual_device_settings(
            &mut settings,
            &cable_pair()
        ));
        assert_eq!(
            settings.virtual_device_mode,
            Some(VirtualDeviceMode::Custom)
        );
        assert_eq!(
            settings.output_endpoint_id.as_deref(),
            Some("custom-render")
        );
    }

    /// The shared specification for `configuration_issue`. Every vector also runs against
    /// `getConfigurationIssue` in `src/routeValidation.ts` (see `src/routeValidation.test.ts`) —
    /// that is the
    /// only thing keeping two implementations of the same safety rules from drifting apart.
    ///
    /// The vectors carry settings as they exist *after* `reconcile_virtual_device_settings`, because
    /// that is what both sides see at runtime: the frontend validates a draft that came from the
    /// backend, which had already reconciled it. Reconciliation has no frontend counterpart and
    /// keeps its own tests below.
    #[derive(serde::Deserialize)]
    struct RouteValidationCase {
        name: String,
        why: String,
        settings: EngineSettings,
        endpoints: Vec<EndpointDescriptor>,
        expected: Option<ConfigurationIssue>,
    }

    fn shared_cases() -> Vec<RouteValidationCase> {
        serde_json::from_str(include_str!("../../fixtures/route-validation-cases.json"))
            .expect("route validation fixture parses")
    }

    #[test]
    fn shared_cases_hold_in_rust() {
        let cases = shared_cases();
        assert!(!cases.is_empty(), "the fixture must not be empty");
        for case in cases {
            assert_eq!(
                configuration_issue(&case.settings, &case.endpoints),
                case.expected,
                "{}\n  {}",
                case.name,
                case.why
            );
        }
    }
}
