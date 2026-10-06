use xmic_engine_protocol::{
    ConfigurationIssue, EngineSettings, KnownVbCablePair, PrimaryReasonCode,
    TrustedVirtualAudioPair, VirtualDeviceIssueReason, VirtualDeviceMode, VirtualRouteConflictKind,
};
use xmic_platform_api::{DataFlow, DeviceRole, EndpointDescriptor};

#[derive(Debug, Clone, Copy)]
pub(crate) struct VirtualAudioPair<'a> {
    pub render: &'a EndpointDescriptor,
    pub capture: &'a EndpointDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteFingerprint {
    aec_enabled: bool,
    follow_default_render: bool,
    configured_render_id: Option<String>,
    configured_capture_id: Option<String>,
    configured_output_id: Option<String>,
    configured_virtual_capture_id: Option<String>,
    render: Option<EndpointIdentity>,
    capture: Option<EndpointIdentity>,
    output: Option<EndpointIdentity>,
    virtual_capture: Option<EndpointIdentity>,
    configuration_issue: Option<ConfigurationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EndpointIdentity {
    id: String,
    group_id: Option<String>,
    friendly_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteWaitReason {
    pub code: PrimaryReasonCode,
    pub endpoint_id: Option<String>,
}

impl From<&EndpointDescriptor> for EndpointIdentity {
    fn from(endpoint: &EndpointDescriptor) -> Self {
        Self {
            id: endpoint.id.clone(),
            group_id: endpoint.group_id.clone(),
            friendly_name: endpoint.friendly_name.clone(),
        }
    }
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

/// Applies the current first-run routing default only. Existing endpoint IDs are never interpreted
/// as an older schema and no route alias is migrated.
pub(crate) fn apply_default_virtual_device(
    settings: &mut EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    let Some(pair) = standard_vb_cable_pair(endpoints) else {
        return false;
    };
    let known_pair = KnownVbCablePair {
        render_endpoint_id: pair.render.id.clone().into(),
        capture_endpoint_id: pair.capture.id.clone().into(),
    };

    if settings.virtual_device_mode.is_none()
        && settings.output_endpoint_id.is_none()
        && settings.virtual_capture_endpoint_id.is_none()
    {
        apply_vb_cable(settings, pair);
        settings.known_vb_cable_pair = Some(known_pair);
        return true;
    }

    if settings.virtual_device_mode == Some(VirtualDeviceMode::VbCable)
        && configured_vb_cable_pair_matches(settings, pair)
        && settings.known_vb_cable_pair.as_ref() != Some(&known_pair)
    {
        settings.known_vb_cable_pair = Some(known_pair);
        return true;
    }

    false
}

/// Decides whether the audio route is usable, and if not, which of the three things the user has to
/// fix.
///
/// **This function's behaviour is defined by `fixtures/route-validation-cases.json`, and
/// `getConfigurationIssue` in `src/routeValidation.ts` must satisfy the same vectors.** The frontend keeps its own copy so it can price an unapplied draft without a round trip; the fixture is what stops the
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

pub(crate) fn route_fingerprint(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> RouteFingerprint {
    let render = selected_render(settings, endpoints).map(EndpointIdentity::from);
    let capture = selected_physical_capture(settings, endpoints).map(EndpointIdentity::from);
    let output = settings
        .output_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Render, id))
        .map(EndpointIdentity::from);
    let virtual_capture = settings
        .virtual_capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id))
        .map(EndpointIdentity::from);
    RouteFingerprint {
        aec_enabled: settings.dsp.aec.enabled,
        follow_default_render: settings.follow_default_render,
        configured_render_id: settings.render_endpoint_id.clone(),
        configured_capture_id: settings.capture_endpoint_id.clone(),
        configured_output_id: settings.output_endpoint_id.clone(),
        configured_virtual_capture_id: settings.virtual_capture_endpoint_id.clone(),
        render,
        capture,
        output,
        virtual_capture,
        configuration_issue: configuration_issue(settings, endpoints),
    }
}

pub(crate) fn processing_route_ready(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    configuration_issue(settings, endpoints).is_none()
        && notification_probe_route_safe(settings, endpoints)
        && selected_physical_capture(settings, endpoints).is_some()
        && configured_output(settings, endpoints).is_some()
        && configured_virtual_capture(settings, endpoints).is_some()
        && (!settings.dsp.aec.enabled || selected_render(settings, endpoints).is_some())
}

pub(crate) fn fallback_route_ready(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    configuration_issue(settings, endpoints).is_none()
        && configured_output(settings, endpoints).is_some()
}

/// Replaces any enumerated copy of the render endpoint with the identity WASAPI actually opened,
/// then reruns the complete safety classifier. This closes the preflight/open race where Windows
/// may reuse a saved virtual-output ID for a different physical endpoint.
pub(crate) fn opened_virtual_output_issue(
    settings: &EngineSettings,
    opened_output: &EndpointDescriptor,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    if opened_output.flow != DataFlow::Render
        || settings.output_endpoint_id.as_deref() != Some(opened_output.id.as_str())
    {
        return Some(virtual_output_identity_issue(settings));
    }
    let mut verified = endpoints
        .iter()
        .filter(|endpoint| !(endpoint.flow == DataFlow::Render && endpoint.id == opened_output.id))
        .cloned()
        .collect::<Vec<_>>();
    verified.push(opened_output.clone());
    configuration_issue(settings, &verified)
}

pub(crate) fn route_wait_reason(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> RouteWaitReason {
    if configured_output(settings, endpoints).is_none() {
        return RouteWaitReason {
            code: PrimaryReasonCode::WaitingForVirtualOutput,
            endpoint_id: settings.output_endpoint_id.clone(),
        };
    }
    if configured_virtual_capture(settings, endpoints).is_none() {
        return RouteWaitReason {
            code: PrimaryReasonCode::WaitingForVirtualOutput,
            endpoint_id: settings.virtual_capture_endpoint_id.clone(),
        };
    }
    if selected_physical_capture(settings, endpoints).is_none() {
        return RouteWaitReason {
            code: PrimaryReasonCode::WaitingForCapture,
            endpoint_id: settings.capture_endpoint_id.clone(),
        };
    }
    if settings.dsp.aec.enabled && selected_render(settings, endpoints).is_none() {
        return RouteWaitReason {
            code: PrimaryReasonCode::WaitingForPlayback,
            endpoint_id: settings.render_endpoint_id.clone(),
        };
    }
    RouteWaitReason {
        code: PrimaryReasonCode::AudioDeviceFailure,
        endpoint_id: None,
    }
}

/// Returns whether the current capture selection is safe for the optional Off-time speech probe.
/// This deliberately does not require a virtual output: the probe never writes one. It does require
/// enough endpoint information to prove that a default capture is not the configured virtual return.
pub(crate) fn notification_probe_route_safe(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    let Some(capture) = selected_physical_capture(settings, endpoints) else {
        return false;
    };
    if is_standard_vb_cable_capture(capture) {
        return false;
    }
    let Some(virtual_capture_id) = settings.virtual_capture_endpoint_id.as_deref() else {
        return true;
    };
    if capture.id == virtual_capture_id
        || trusted_virtual_capture_name_matches(settings, virtual_capture_id, capture)
    {
        return false;
    }
    let Some(virtual_capture) = find_endpoint(endpoints, DataFlow::Capture, virtual_capture_id)
    else {
        // A default capture with a configured but currently missing virtual return is ambiguous;
        // suppress the optional probe until Windows enumerates the route again.
        return settings.capture_endpoint_id.is_some();
    };
    !same_endpoint_or_group(capture, virtual_capture)
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

fn configured_vb_cable_pair_matches(settings: &EngineSettings, pair: VirtualAudioPair<'_>) -> bool {
    settings.output_endpoint_id.as_deref() == Some(pair.render.id.as_str())
        && settings.virtual_capture_endpoint_id.as_deref() == Some(pair.capture.id.as_str())
}

fn known_vb_cable_pair_is_offline(settings: &EngineSettings) -> bool {
    let Some(known_pair) = settings.known_vb_cable_pair.as_ref() else {
        return false;
    };
    settings.virtual_device_mode == Some(VirtualDeviceMode::VbCable)
        && settings.output_endpoint_id.as_deref() == Some(known_pair.render_endpoint_id.as_str())
        && settings.virtual_capture_endpoint_id.as_deref()
            == Some(known_pair.capture_endpoint_id.as_str())
}

fn vb_cable_configuration_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    let pair = match standard_vb_cable_pair(endpoints) {
        Some(pair) => pair,
        None if known_vb_cable_pair_is_offline(settings) => {
            // Only a pair previously recognised from standard VB-CABLE endpoint names may be
            // temporarily absent. Any endpoint that remains visible still participates in the
            // physical-capture and playback-reference safety checks.
            return known_offline_vb_cable_safety_issue(settings, endpoints);
        }
        None => {
            return Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            });
        }
    };
    if (settings.virtual_device_mode == Some(VirtualDeviceMode::VbCable)
        || settings.output_endpoint_id.is_some()
        || settings.virtual_capture_endpoint_id.is_some())
        && !configured_vb_cable_pair_matches(settings, pair)
    {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::VbCableUnavailable,
        });
    }
    let capture = selected_physical_capture(settings, endpoints);
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

fn selected_render<'a>(
    settings: &EngineSettings,
    endpoints: &'a [EndpointDescriptor],
) -> Option<&'a EndpointDescriptor> {
    if !settings.dsp.aec.enabled {
        return None;
    }
    if settings.follow_default_render {
        endpoints.iter().find(|endpoint| {
            endpoint.flow == DataFlow::Render
                && endpoint.default_roles.contains(&DeviceRole::Multimedia)
        })
    } else {
        settings
            .render_endpoint_id
            .as_deref()
            .and_then(|id| find_endpoint(endpoints, DataFlow::Render, id))
    }
}

fn configured_output<'a>(
    settings: &EngineSettings,
    endpoints: &'a [EndpointDescriptor],
) -> Option<&'a EndpointDescriptor> {
    settings
        .output_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Render, id))
}

fn configured_virtual_capture<'a>(
    settings: &EngineSettings,
    endpoints: &'a [EndpointDescriptor],
) -> Option<&'a EndpointDescriptor> {
    settings
        .virtual_capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id))
}

fn selected_physical_capture<'a>(
    settings: &EngineSettings,
    endpoints: &'a [EndpointDescriptor],
) -> Option<&'a EndpointDescriptor> {
    settings
        .capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id))
        .or_else(|| {
            settings
                .capture_endpoint_id
                .is_none()
                .then(|| default_multimedia_capture(endpoints))
                .flatten()
        })
}

fn known_offline_vb_cable_safety_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    if known_vb_cable_identity_was_reused(settings, endpoints) {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::VbCableUnavailable,
        });
    }
    let capture = selected_physical_capture(settings, endpoints);
    let virtual_render = settings
        .output_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Render, id));
    let virtual_capture = settings
        .virtual_capture_endpoint_id
        .as_deref()
        .and_then(|id| find_endpoint(endpoints, DataFlow::Capture, id));
    if capture.is_some_and(|physical| {
        is_standard_vb_cable_capture(physical)
            || trusted_virtual_capture_name_matches(
                settings,
                settings
                    .virtual_capture_endpoint_id
                    .as_deref()
                    .unwrap_or_default(),
                physical,
            )
            || virtual_render.is_some_and(|endpoint| same_endpoint_or_group(physical, endpoint))
            || virtual_capture.is_some_and(|endpoint| same_endpoint_or_group(physical, endpoint))
            || settings.virtual_capture_endpoint_id.as_deref() == Some(physical.id.as_str())
    }) {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: capture.map(|endpoint| endpoint.id.clone()),
        });
    }
    virtual_render.and_then(|output| render_conflict(settings, endpoints, output))
}

fn known_vb_cable_identity_was_reused(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> bool {
    let Some(known) = settings.known_vb_cable_pair.as_ref() else {
        return false;
    };
    endpoints
        .iter()
        .find(|endpoint| endpoint.id == known.render_endpoint_id.as_str())
        .is_some_and(|endpoint| !is_standard_vb_cable_render(endpoint))
        || endpoints
            .iter()
            .find(|endpoint| endpoint.id == known.capture_endpoint_id.as_str())
            .is_some_and(|endpoint| !is_standard_vb_cable_capture(endpoint))
}

fn custom_configuration_issue(
    settings: &EngineSettings,
    endpoints: &[EndpointDescriptor],
) -> Option<ConfigurationIssue> {
    let (Some(output_id), Some(virtual_capture_id)) = (
        settings.output_endpoint_id.as_deref(),
        settings.virtual_capture_endpoint_id.as_deref(),
    ) else {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairIncomplete,
        });
    };
    let Some(trusted_pair) = find_trusted_pair(
        output_id,
        virtual_capture_id,
        &settings.trusted_virtual_audio_pairs,
    ) else {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
        });
    };
    let virtual_render = find_endpoint(endpoints, DataFlow::Render, output_id);
    let virtual_capture = find_endpoint(endpoints, DataFlow::Capture, virtual_capture_id);
    if virtual_render.is_some_and(|endpoint| {
        !trusted_name_matches(&trusted_pair.render_name, &endpoint.friendly_name)
    }) || virtual_capture.is_some_and(|endpoint| {
        !trusted_name_matches(&trusted_pair.capture_name, &endpoint.friendly_name)
    }) {
        return Some(ConfigurationIssue::VirtualDeviceRequired {
            reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
        });
    }

    let physical_capture = selected_physical_capture(settings, endpoints);
    if physical_capture.is_some_and(is_standard_vb_cable_capture)
        || physical_capture.is_some_and(|physical| {
            trusted_virtual_capture_name_matches(settings, virtual_capture_id, physical)
        })
    {
        return Some(ConfigurationIssue::PhysicalCaptureRequired {
            virtual_capture_endpoint_id: physical_capture.map(|endpoint| endpoint.id.clone()),
        });
    }
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
    if !settings.dsp.aec.enabled {
        return None;
    }
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

fn trusted_virtual_capture_name_matches(
    settings: &EngineSettings,
    configured_capture_id: &str,
    capture: &EndpointDescriptor,
) -> bool {
    settings.trusted_virtual_audio_pairs.iter().any(|pair| {
        pair.capture_endpoint_id == configured_capture_id
            && trusted_name_matches(&pair.capture_name, &capture.friendly_name)
    })
}

fn find_trusted_pair<'a>(
    render_id: &str,
    capture_id: &str,
    trusted_pairs: &'a [TrustedVirtualAudioPair],
) -> Option<&'a TrustedVirtualAudioPair> {
    trusted_pairs
        .iter()
        .find(|pair| pair.render_endpoint_id == render_id && pair.capture_endpoint_id == capture_id)
}

fn trusted_name_matches(expected: &str, actual: &str) -> bool {
    !expected.trim().is_empty() && expected.trim().eq_ignore_ascii_case(actual.trim())
}

fn virtual_output_identity_issue(settings: &EngineSettings) -> ConfigurationIssue {
    ConfigurationIssue::VirtualDeviceRequired {
        reason: if settings.virtual_device_mode == Some(VirtualDeviceMode::Custom) {
            VirtualDeviceIssueReason::CustomPairUnconfirmed
        } else {
            VirtualDeviceIssueReason::VbCableUnavailable
        },
    }
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

pub(crate) fn is_standard_vb_cable_capture(endpoint: &EndpointDescriptor) -> bool {
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
    fn fresh_defaults_select_standard_vb_cable() {
        let mut settings = EngineSettings {
            output_endpoint_id: Some("cable-input".to_owned()),
            ..EngineSettings::default()
        };

        settings.output_endpoint_id = None;
        assert!(apply_default_virtual_device(&mut settings, &cable_pair()));
        assert_eq!(
            settings.virtual_device_mode,
            Some(VirtualDeviceMode::VbCable)
        );
        assert_eq!(
            settings.virtual_capture_endpoint_id.as_deref(),
            Some("cable-output")
        );
        assert_eq!(
            settings.known_vb_cable_pair,
            Some(KnownVbCablePair {
                render_endpoint_id: "cable-input".to_owned().into(),
                capture_endpoint_id: "cable-output".to_owned().into(),
            })
        );
    }

    #[test]
    fn existing_route_is_not_interpreted_or_migrated() {
        let mut settings = EngineSettings {
            output_endpoint_id: Some("custom-render".to_owned()),
            ..EngineSettings::default()
        };
        assert!(!apply_default_virtual_device(&mut settings, &cable_pair()));
        assert_eq!(settings.virtual_device_mode, None);
        assert_eq!(
            settings.output_endpoint_id.as_deref(),
            Some("custom-render")
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

        assert!(!apply_default_virtual_device(&mut settings, &cable_pair()));
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
    /// The vectors carry the current settings schema. First-run default selection is deliberately
    /// separate and has no compatibility or migration behavior.
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
    fn readiness_separates_processing_from_safe_silence_fallback() {
        let mut endpoints = cable_pair().to_vec();
        endpoints[1].default_roles.clear();
        endpoints.push(endpoint(
            "microphone",
            "Physical microphone",
            DataFlow::Capture,
            Some("microphone"),
            vec![DeviceRole::Multimedia],
        ));
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            capture_endpoint_id: Some("microphone".to_owned()),
            ..EngineSettings::default()
        };

        assert!(!processing_route_ready(&settings, &endpoints));
        assert!(fallback_route_ready(&settings, &endpoints));
        assert_eq!(
            route_wait_reason(&settings, &endpoints).code,
            PrimaryReasonCode::WaitingForPlayback,
        );
        endpoints.push(endpoint(
            "speakers",
            "Speakers",
            DataFlow::Render,
            Some("speakers"),
            vec![DeviceRole::Multimedia],
        ));
        assert!(processing_route_ready(&settings, &endpoints));
        assert_eq!(
            route_wait_reason(&settings, &endpoints).code,
            PrimaryReasonCode::AudioDeviceFailure,
        );
    }

    #[test]
    fn aec_off_ignores_playback_conflicts() {
        let endpoints = cable_pair();
        let mut settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            capture_endpoint_id: Some("physical-mic".to_owned()),
            ..EngineSettings::default()
        };
        settings.dsp.aec.enabled = false;
        assert_eq!(render_conflict(&settings, &endpoints, &endpoints[0]), None);
    }

    #[test]
    fn aec_off_still_rejects_virtual_capture_feedback() {
        let endpoints = cable_pair();
        let mut settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            ..EngineSettings::default()
        };
        settings.dsp.aec.enabled = false;
        assert_eq!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            })
        );
    }

    #[test]
    fn notification_probe_rejects_a_virtual_default_capture() {
        let endpoints = cable_pair();
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            ..EngineSettings::default()
        };
        assert!(!notification_probe_route_safe(&settings, &endpoints));
    }

    #[test]
    fn notification_probe_rejects_a_trusted_custom_virtual_name() {
        let endpoints = [endpoint(
            "custom-capture-reinstalled",
            "Custom virtual output",
            DataFlow::Capture,
            None,
            vec![DeviceRole::Multimedia],
        )];
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            output_endpoint_id: Some("custom-render".to_owned()),
            virtual_capture_endpoint_id: Some("custom-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![TrustedVirtualAudioPair {
                render_endpoint_id: "custom-render".to_owned(),
                capture_endpoint_id: "custom-capture".to_owned(),
                render_name: "Custom virtual input".to_owned(),
                capture_name: "Custom virtual output".to_owned(),
            }],
            ..EngineSettings::default()
        };
        assert!(!notification_probe_route_safe(&settings, &endpoints));
    }

    #[test]
    fn notification_probe_accepts_a_physical_default_capture() {
        let mut endpoints = cable_pair().to_vec();
        endpoints[1].default_roles.clear();
        endpoints.push(endpoint(
            "microphone",
            "Physical microphone",
            DataFlow::Capture,
            Some("microphone"),
            vec![DeviceRole::Multimedia],
        ));
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            ..EngineSettings::default()
        };
        assert!(notification_probe_route_safe(&settings, &endpoints));
    }

    #[test]
    fn custom_mode_with_no_capture_id_follows_the_default() {
        let endpoints = [
            endpoint(
                "physical-mic",
                "Physical microphone",
                DataFlow::Capture,
                Some("physical"),
                vec![DeviceRole::Multimedia],
            ),
            endpoint(
                "virtual-render",
                "Custom virtual input",
                DataFlow::Render,
                Some("virtual"),
                vec![],
            ),
            endpoint(
                "virtual-capture",
                "Custom virtual output",
                DataFlow::Capture,
                Some("virtual"),
                vec![],
            ),
        ];
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![TrustedVirtualAudioPair {
                render_endpoint_id: "virtual-render".to_owned(),
                capture_endpoint_id: "virtual-capture".to_owned(),
                render_name: "Custom virtual input".to_owned(),
                capture_name: "Custom virtual output".to_owned(),
            }],
            ..EngineSettings::default()
        };

        assert_eq!(configuration_issue(&settings, &endpoints), None);
        assert_eq!(configuration_issue(&settings, &[]), None);
    }

    #[test]
    fn known_vb_cable_absence_is_transient() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            known_vb_cable_pair: Some(KnownVbCablePair {
                render_endpoint_id: "cable-input".to_owned().into(),
                capture_endpoint_id: "cable-output".to_owned().into(),
            }),
            ..EngineSettings::default()
        };
        assert_eq!(configuration_issue(&settings, &[]), None);
    }

    #[test]
    fn opened_output_identity_replaces_a_stale_preflight_snapshot() {
        let settings = EngineSettings {
            capture_endpoint_id: Some("microphone".to_owned()),
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            known_vb_cable_pair: Some(KnownVbCablePair {
                render_endpoint_id: "cable-input".to_owned().into(),
                capture_endpoint_id: "cable-output".to_owned().into(),
            }),
            ..EngineSettings::default()
        };
        let endpoints = vec![
            endpoint(
                "speakers",
                "Speakers",
                DataFlow::Render,
                Some("speakers"),
                vec![DeviceRole::Multimedia],
            ),
            endpoint(
                "microphone",
                "Microphone",
                DataFlow::Capture,
                Some("microphone"),
                vec![DeviceRole::Multimedia],
            ),
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
                Vec::new(),
            ),
        ];
        let opened_output = endpoint(
            "cable-input",
            "HDMI Speakers",
            DataFlow::Render,
            Some("physical-output"),
            Vec::new(),
        );

        assert_eq!(
            opened_virtual_output_issue(&settings, &opened_output, &endpoints),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
        );
    }

    #[test]
    fn known_offline_pair_blocks_its_capture_id_under_a_nonstandard_identity() {
        let endpoints = [endpoint(
            "cable-output",
            "Renamed virtual return",
            DataFlow::Capture,
            None,
            vec![DeviceRole::Multimedia],
        )];
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            known_vb_cable_pair: Some(KnownVbCablePair {
                render_endpoint_id: "cable-input".to_owned().into(),
                capture_endpoint_id: "cable-output".to_owned().into(),
            }),
            ..EngineSettings::default()
        };

        assert_eq!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
        );
    }

    #[test]
    fn known_offline_pair_rejects_a_different_remaining_vb_capture() {
        let endpoints = [endpoint(
            "new-cable-output",
            "CABLE Output (VB-Audio Virtual Cable)",
            DataFlow::Capture,
            None,
            vec![DeviceRole::Multimedia],
        )];
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            known_vb_cable_pair: Some(KnownVbCablePair {
                render_endpoint_id: "cable-input".to_owned().into(),
                capture_endpoint_id: "cable-output".to_owned().into(),
            }),
            ..EngineSettings::default()
        };

        assert_eq!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id: Some("new-cable-output".to_owned()),
            })
        );
    }

    #[test]
    fn arbitrary_vb_cable_ids_are_blocked_when_the_pair_is_offline() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("speakers".to_owned()),
            virtual_capture_endpoint_id: Some("microphone".to_owned()),
            ..EngineSettings::default()
        };
        assert_eq!(
            configuration_issue(&settings, &[]),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
        );
    }

    #[test]
    fn mismatched_vb_cable_ids_are_blocked_while_the_pair_is_present() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("speakers".to_owned()),
            virtual_capture_endpoint_id: Some("microphone".to_owned()),
            ..EngineSettings::default()
        };
        assert_eq!(
            configuration_issue(&settings, &cable_pair()),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
        );
    }

    #[test]
    fn matching_vb_cable_ids_recognise_the_current_pair() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::VbCable),
            output_endpoint_id: Some("cable-input".to_owned()),
            virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            capture_endpoint_id: Some("microphone".to_owned()),
            ..EngineSettings::default()
        };
        assert_eq!(configuration_issue(&settings, &cable_pair()), None);
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
