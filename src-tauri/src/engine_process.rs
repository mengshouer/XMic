use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process, thread,
};

use xmic_engine_protocol::{EngineRequest, EngineResponse, PROTOCOL_VERSION};
use xmic_platform_windows::{wait_for_process_exit, LocalIpcServer};

use crate::{
    engine::{EngineStartError, RealtimeEngineController},
    engine_ipc::{read_message, write_message},
    settings_store::SettingsStore,
};

struct AudioEngineProcessConfig {
    pipe_name: String,
    parent_process_id: u32,
    native_library: PathBuf,
    native_library_guard_error: Option<String>,
    config_dir: PathBuf,
}

pub fn run_from_environment() -> Result<(), String> {
    let config = parse_args(std::env::args_os().skip(1))?;
    run(config)
}

fn run(config: AudioEngineProcessConfig) -> Result<(), String> {
    let server = LocalIpcServer::bind(&config.pipe_name).map_err(|error| error.to_string())?;
    monitor_parent(config.parent_process_id)?;
    remove_legacy_state_files(&config.config_dir);
    let controller = RealtimeEngineController::new_with_library_guard_error(
        config.native_library,
        config.native_library_guard_error,
        SettingsStore::new(config.config_dir.join("settings.json")),
    );

    loop {
        let mut stream = server.accept().map_err(|error| error.to_string())?;
        let request = match read_message::<EngineRequest>(&mut stream) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_message(&mut stream, &EngineResponse::Error(error));
                continue;
            }
        };
        let (response, shutdown) = handle_request(&controller, request);
        write_message(&mut stream, &response)?;
        if shutdown {
            break;
        }
    }
    Ok(())
}

fn handle_request(
    controller: &RealtimeEngineController,
    request: EngineRequest,
) -> (EngineResponse, bool) {
    match request {
        EngineRequest::Hello { protocol_version } => {
            if protocol_version == PROTOCOL_VERSION {
                (
                    EngineResponse::Hello {
                        protocol_version: PROTOCOL_VERSION,
                    },
                    false,
                )
            } else {
                (
                    EngineResponse::Error(format!(
                        "engine protocol mismatch: UI {protocol_version}, helper {PROTOCOL_VERSION}"
                    )),
                    false,
                )
            }
        }
        EngineRequest::GetStatus { probe_permit } => (
            EngineResponse::Status(Box::new(controller.status(probe_permit))),
            false,
        ),
        EngineRequest::UpdateSettings { settings } => match controller.update_settings(*settings) {
            Ok(()) => (EngineResponse::Ack, false),
            Err(error) => (EngineResponse::Error(error), false),
        },
        EngineRequest::PrepareFallback { reason } => match controller.prepare_fallback(reason) {
            Ok(()) => (EngineResponse::Ack, false),
            Err(error) => (EngineResponse::Error(error), false),
        },
        EngineRequest::Start => match controller.start() {
            Ok(()) => (EngineResponse::Ack, false),
            Err(EngineStartError::ConfigurationRequired(issue)) => {
                (EngineResponse::ConfigurationRequired(issue), false)
            }
            Err(EngineStartError::Runtime(error)) => (EngineResponse::Error(error), false),
        },
        EngineRequest::Stop => match controller.stop() {
            Ok(()) => (EngineResponse::Ack, false),
            Err(error) => (EngineResponse::Error(error), false),
        },
        EngineRequest::Shutdown => {
            let response = match controller.shutdown() {
                Ok(()) => EngineResponse::Ack,
                Err(error) => EngineResponse::Error(error),
            };
            (response, true)
        }
    }
}

fn remove_legacy_state_files(config_dir: &Path) {
    // Clean break: old route/profile files are neither read nor migrated. Best-effort deletion keeps
    // a stale profile from being mistaken for active state while preserving startup if cleanup fails.
    for name in ["device-profiles.json", "audio-route.json"] {
        let _ = std::fs::remove_file(config_dir.join(name));
    }
}

fn monitor_parent(parent_process_id: u32) -> Result<(), String> {
    thread::Builder::new()
        .name("xmic-parent-monitor".to_owned())
        .spawn(move || {
            let _ = wait_for_process_exit(parent_process_id);
            process::exit(0);
        })
        .map(|_| ())
        .map_err(|error| format!("start parent process monitor: {error}"))
}

fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<AudioEngineProcessConfig, String> {
    let mut pipe_name = None;
    let mut parent_process_id = None;
    let mut native_library = None;
    let mut native_library_guard_error = None;
    let mut config_dir = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--audio-engine" => {}
            "--pipe-name" => pipe_name = Some(required_value(&mut args, "--pipe-name")?),
            "--parent-pid" => {
                let value = required_value(&mut args, "--parent-pid")?;
                parent_process_id = Some(
                    value
                        .to_string_lossy()
                        .parse::<u32>()
                        .map_err(|error| format!("invalid --parent-pid: {error}"))?,
                );
            }
            "--native-library" => {
                native_library = Some(PathBuf::from(required_value(
                    &mut args,
                    "--native-library",
                )?));
            }
            "--native-library-guard-error" => {
                native_library_guard_error = Some(
                    required_value(&mut args, "--native-library-guard-error")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "--config-dir" => {
                config_dir = Some(PathBuf::from(required_value(&mut args, "--config-dir")?));
            }
            unknown => return Err(format!("unknown audio engine argument: {unknown}")),
        }
    }

    Ok(AudioEngineProcessConfig {
        pipe_name: pipe_name
            .ok_or_else(|| "missing --pipe-name".to_owned())?
            .to_string_lossy()
            .into_owned(),
        parent_process_id: parent_process_id.ok_or_else(|| "missing --parent-pid".to_owned())?,
        native_library: native_library.ok_or_else(|| "missing --native-library".to_owned())?,
        native_library_guard_error,
        config_dir: config_dir.ok_or_else(|| "missing --config-dir".to_owned())?,
    })
}

fn required_value(
    args: &mut impl Iterator<Item = OsString>,
    argument: &str,
) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {argument}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_legacy_state_files_without_affecting_current_settings() {
        let directory = std::env::temp_dir().join(format!(
            "xmic-engine-process-cleanup-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let legacy_profile = directory.join("device-profiles.json");
        let legacy_route = directory.join("audio-route.json");
        let current = directory.join("settings.json");
        std::fs::write(&legacy_profile, "old").unwrap();
        std::fs::write(&legacy_route, "old").unwrap();
        std::fs::write(&current, "current").unwrap();

        remove_legacy_state_files(&directory);

        assert!(!legacy_profile.exists());
        assert!(!legacy_route.exists());
        assert!(current.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn parses_required_process_arguments() {
        let config = parse_args(
            [
                "--pipe-name",
                "xmic-engine-test",
                "--parent-pid",
                "42",
                "--native-library",
                "dsp.dll",
                "--config-dir",
                "config",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(config.pipe_name, "xmic-engine-test");
        assert_eq!(config.parent_process_id, 42);
        assert_eq!(config.native_library, PathBuf::from("dsp.dll"));
        assert!(config.native_library_guard_error.is_none());
        assert_eq!(config.config_dir, PathBuf::from("config"));
    }

    #[test]
    fn parses_an_immutable_library_guard_failure() {
        let config = parse_args(
            [
                "--audio-engine",
                "--pipe-name",
                "xmic-engine-test",
                "--parent-pid",
                "42",
                "--native-library",
                "missing.dll",
                "--native-library-guard-error",
                "selected library could not be pinned",
                "--config-dir",
                "config",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(
            config.native_library_guard_error.as_deref(),
            Some("selected library could not be pinned")
        );
    }
}
