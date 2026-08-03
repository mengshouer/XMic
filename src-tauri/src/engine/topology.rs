//! Route topology checks a session runs against the endpoints it actually opened.
//!
//! `route_validation` decides whether the *configured* route is usable. These run once the
//! WASAPI streams are open and confirm that what Windows handed back is still not a feedback
//! loop, because the default playback device can change between the two.

use xmic_platform_api::EndpointDescriptor;
use xmic_platform_windows::DuplexSourceStream;

pub(super) fn validate_topology(
    source: &DuplexSourceStream,
    output_endpoint: &EndpointDescriptor,
    follow_default_render: bool,
) -> Result<(), String> {
    if let Some(message) = render_output_conflict_message(
        &source.info().render_endpoint,
        output_endpoint,
        follow_default_render,
    ) {
        return Err(message);
    }
    validate_capture_output_topology(&source.info().capture_endpoint, output_endpoint)
}

/// The checks that do not involve the playback reference, so the idle session can run them too.
pub(super) fn validate_capture_output_topology(
    capture_endpoint: &EndpointDescriptor,
    output_endpoint: &EndpointDescriptor,
) -> Result<(), String> {
    if same_device_group(capture_endpoint, output_endpoint) {
        return Err(
            "the virtual output belongs to the same device group as the physical microphone"
                .to_owned(),
        );
    }
    validate_physical_capture(capture_endpoint)
}

pub(super) fn validate_physical_capture(
    capture_endpoint: &EndpointDescriptor,
) -> Result<(), String> {
    let capture_name = capture_endpoint.friendly_name.to_lowercase();
    if capture_name.contains("cable output") || capture_name.contains("vb-audio virtual cable") {
        return Err("virtual cable capture cannot be used as the physical microphone".to_owned());
    }
    Ok(())
}

fn render_output_conflict_message(
    render_endpoint: &EndpointDescriptor,
    output_endpoint: &EndpointDescriptor,
    follow_default_render: bool,
) -> Option<String> {
    if render_endpoint.id != output_endpoint.id
        && !same_device_group(render_endpoint, output_endpoint)
    {
        return None;
    }
    if follow_default_render {
        return Some(format!(
            "Windows 默认播放设备“{}”同时被选为虚拟麦克风输出。请先将音响或耳机设为 Windows 默认播放设备，然后重试。",
            render_endpoint.friendly_name
        ));
    }
    Some(format!(
        "播放参考“{}”不能同时作为虚拟麦克风输出。请为播放参考选择实际发声的音响或耳机。",
        render_endpoint.friendly_name
    ))
}

fn same_device_group(left: &EndpointDescriptor, right: &EndpointDescriptor) -> bool {
    left.group_id
        .as_ref()
        .zip(right.group_id.as_ref())
        .is_some_and(|(left, right)| !left.is_empty() && left == right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmic_platform_api::DataFlow;

    fn endpoint(id: &str, group_id: Option<&str>) -> EndpointDescriptor {
        EndpointDescriptor {
            platform: "windows".to_owned(),
            id: id.to_owned(),
            group_id: group_id.map(str::to_owned),
            friendly_name: "CABLE Input (VB-Audio Virtual Cable)".to_owned(),
            flow: DataFlow::Render,
            default_roles: Vec::new(),
        }
    }

    #[test]
    fn default_render_conflict_explains_how_to_fix_windows_routing() {
        let render = endpoint("cable-input", Some("cable-group"));
        let output = endpoint("cable-input", Some("cable-group"));
        let message = render_output_conflict_message(&render, &output, true)
            .expect("matching endpoint should be rejected");

        assert!(message.contains("Windows 默认播放设备"));
        assert!(message.contains("CABLE Input"));
        assert!(message.contains("音响或耳机"));
    }

    #[test]
    fn distinct_render_and_output_endpoints_do_not_conflict() {
        let render = endpoint("speakers", Some("speaker-group"));
        let output = endpoint("cable-input", Some("cable-group"));
        assert_eq!(render_output_conflict_message(&render, &output, true), None);
    }

    #[test]
    fn endpoints_in_the_same_device_group_conflict_even_with_different_ids() {
        let render = endpoint("speaker-output-a", Some("physical-device"));
        let output = endpoint("speaker-output-b", Some("physical-device"));

        assert!(render_output_conflict_message(&render, &output, false).is_some());
    }

    #[test]
    fn the_idle_session_refuses_to_feed_the_virtual_cable_back_into_itself() {
        let mut capture = endpoint("cable-output", Some("cable-group"));
        capture.friendly_name = "CABLE Output (VB-Audio Virtual Cable)".to_owned();
        let output = endpoint("cable-input", Some("other-group"));

        assert!(validate_capture_output_topology(&capture, &output).is_err());
        assert!(validate_physical_capture(&capture).is_err());

        let mut microphone = endpoint("mic", Some("mic-group"));
        microphone.friendly_name = "Microphone (Realtek)".to_owned();
        assert!(validate_capture_output_topology(&microphone, &output).is_ok());
    }
}
