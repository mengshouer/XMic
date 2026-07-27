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
        || (name.starts_with(&format!("{endpoint_name} ("))
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

    fn physical_capture(id: &str, roles: Vec<DeviceRole>) -> EndpointDescriptor {
        endpoint(
            id,
            "Physical microphone",
            DataFlow::Capture,
            Some(id),
            roles,
        )
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

    fn trusted_pair(render_id: &str, capture_id: &str) -> TrustedVirtualAudioPair {
        TrustedVirtualAudioPair {
            render_endpoint_id: render_id.to_owned(),
            capture_endpoint_id: capture_id.to_owned(),
            render_name: render_id.to_owned(),
            capture_name: capture_id.to_owned(),
        }
    }

    #[test]
    fn complete_standard_vb_cable_is_detected() {
        let endpoints = cable_pair();
        let pair = standard_vb_cable_pair(&endpoints).unwrap();

        assert_eq!(pair.render.id, "cable-input");
        assert_eq!(pair.capture.id, "cable-output");
    }

    #[test]
    fn renamed_vb_cable_is_not_guessed() {
        let endpoints = [
            endpoint(
                "custom-in",
                "Stream Input",
                DataFlow::Render,
                Some("cable"),
                vec![],
            ),
            endpoint(
                "custom-out",
                "Stream Output",
                DataFlow::Capture,
                Some("cable"),
                vec![],
            ),
        ];

        assert!(standard_vb_cable_pair(&endpoints).is_none());
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

    #[test]
    fn missing_vb_cable_blocks_configuration() {
        assert_eq!(
            configuration_issue(&EngineSettings::default(), &[]),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::VbCableUnavailable,
            })
        );
    }

    #[test]
    fn default_vb_capture_requires_explicit_physical_microphone() {
        let mut settings = EngineSettings::default();
        let endpoints = cable_pair();
        reconcile_virtual_device_settings(&mut settings, &endpoints);

        assert_eq!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::PhysicalCaptureRequired {
                virtual_capture_endpoint_id: Some("cable-output".to_owned()),
            })
        );
    }

    #[test]
    fn explicit_physical_capture_allows_virtual_system_default() {
        let mut settings = EngineSettings {
            capture_endpoint_id: Some("microphone".to_owned()),
            ..EngineSettings::default()
        };
        let mut endpoints = cable_pair().to_vec();
        endpoints.push(physical_capture("microphone", Vec::new()));
        reconcile_virtual_device_settings(&mut settings, &endpoints);

        assert_eq!(configuration_issue(&settings, &endpoints), None);
    }

    #[test]
    fn custom_mode_requires_explicit_physical_capture() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![trusted_pair("virtual-render", "virtual-capture")],
            ..EngineSettings::default()
        };

        assert!(matches!(
            configuration_issue(&settings, &[]),
            Some(ConfigurationIssue::PhysicalCaptureRequired { .. })
        ));
    }

    #[test]
    fn custom_pair_requires_confirmation() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("microphone".to_owned()),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            ..EngineSettings::default()
        };

        assert_eq!(
            configuration_issue(&settings, &[]),
            Some(ConfigurationIssue::VirtualDeviceRequired {
                reason: VirtualDeviceIssueReason::CustomPairUnconfirmed,
            })
        );
    }

    #[test]
    fn trusted_custom_pair_may_be_temporarily_offline() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("microphone".to_owned()),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![trusted_pair("virtual-render", "virtual-capture")],
            ..EngineSettings::default()
        };

        assert_eq!(configuration_issue(&settings, &[]), None);
    }

    #[test]
    fn custom_output_cannot_be_the_default_render() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("microphone".to_owned()),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![trusted_pair("virtual-render", "virtual-capture")],
            ..EngineSettings::default()
        };
        let endpoints = [
            endpoint(
                "virtual-render",
                "Custom virtual input",
                DataFlow::Render,
                Some("virtual-render"),
                vec![DeviceRole::Multimedia],
            ),
            physical_capture("microphone", Vec::new()),
        ];

        assert!(matches!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::VirtualRouteConflict {
                kind: VirtualRouteConflictKind::DefaultRender,
                ..
            })
        ));
    }

    #[test]
    fn custom_capture_may_be_default_and_use_a_different_group() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("microphone".to_owned()),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![trusted_pair("virtual-render", "virtual-capture")],
            ..EngineSettings::default()
        };
        let endpoints = [
            endpoint(
                "speakers",
                "Physical speakers",
                DataFlow::Render,
                Some("speakers"),
                vec![DeviceRole::Multimedia],
            ),
            endpoint(
                "virtual-render",
                "Custom virtual input",
                DataFlow::Render,
                Some("write-side"),
                vec![],
            ),
            endpoint(
                "virtual-capture",
                "Custom virtual output",
                DataFlow::Capture,
                Some("read-side"),
                vec![DeviceRole::Multimedia],
            ),
            physical_capture("microphone", Vec::new()),
        ];

        assert_eq!(configuration_issue(&settings, &endpoints), None);
    }

    #[test]
    fn custom_virtual_capture_cannot_be_the_physical_input() {
        let settings = EngineSettings {
            virtual_device_mode: Some(VirtualDeviceMode::Custom),
            capture_endpoint_id: Some("virtual-capture".to_owned()),
            output_endpoint_id: Some("virtual-render".to_owned()),
            virtual_capture_endpoint_id: Some("virtual-capture".to_owned()),
            trusted_virtual_audio_pairs: vec![trusted_pair("virtual-render", "virtual-capture")],
            ..EngineSettings::default()
        };
        let endpoints = [
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

        assert!(matches!(
            configuration_issue(&settings, &endpoints),
            Some(ConfigurationIssue::PhysicalCaptureRequired { .. })
        ));
    }
}
