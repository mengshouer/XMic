#![cfg(target_os = "windows")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "xmic-runtime-smoke-{name}-{}-{}",
        std::process::id(),
        TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn copy_standalone_executable(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let executable = root.join("XMic.exe");
    fs::copy(env!("CARGO_BIN_EXE_xmic"), &executable).unwrap();
    executable
}

fn run_self_test(executable: &Path, root: &Path, report: &Path) -> Output {
    Command::new(executable)
        .arg("--runtime-self-test")
        .arg(report)
        .current_dir(root)
        .env("APPDATA", root.join("roaming"))
        .env("LOCALAPPDATA", root.join("local"))
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "runtime self-test failed with {:?}: stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn standalone_copy_uses_the_embedded_dsp_and_cleans_its_run_directory() {
    let root = test_root("standard");
    let executable = copy_standalone_executable(&root);
    let report_path = root.join("report.json");

    let output = run_self_test(&executable, &root, &report_path);

    assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let diagnostics = &report["diagnostics"];
    assert_eq!(diagnostics["mode"], "standard");
    assert_eq!(
        PathBuf::from(diagnostics["dataDirectory"].as_str().unwrap()),
        root.join("roaming/com.mengshouer.xmic")
    );
    assert_eq!(
        PathBuf::from(diagnostics["runtimeDirectory"].as_str().unwrap()),
        root.join("local/com.mengshouer.xmic/runtime")
    );
    assert_eq!(
        PathBuf::from(diagnostics["webviewDataDirectory"].as_str().unwrap()),
        root.join("local/com.mengshouer.xmic")
    );
    assert_eq!(diagnostics["dspSource"], "embedded");
    assert_eq!(diagnostics["dspCustom"], false);
    assert_eq!(diagnostics["dspSelectionPinned"], true);
    assert!(diagnostics["dspSelectionGuardError"].is_null());
    assert_eq!(diagnostics["expectedDspAbi"], 2);
    assert_eq!(diagnostics["actualDspAbi"], 2);
    assert!(diagnostics["dspValidationError"].is_null());
    assert_eq!(report["embeddedLibraryMatches"], true);
    assert_eq!(report["currentRuntimeCleaned"], true);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn portable_copy_keeps_all_app_owned_paths_under_data() {
    let root = test_root("portable");
    let executable = copy_standalone_executable(&root);
    fs::write(root.join("portable.flag"), []).unwrap();
    fs::create_dir(root.join("data")).unwrap();
    let report_path = root.join("report.json");

    let output = run_self_test(&executable, &root, &report_path);

    assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let diagnostics = &report["diagnostics"];
    assert_eq!(diagnostics["mode"], "portable");
    assert_eq!(
        PathBuf::from(diagnostics["dataDirectory"].as_str().unwrap()),
        root.join("data")
    );
    assert_eq!(
        PathBuf::from(diagnostics["runtimeDirectory"].as_str().unwrap()),
        root.join("data/runtime")
    );
    assert_eq!(
        PathBuf::from(diagnostics["webviewDataDirectory"].as_str().unwrap()),
        root.join("data/webview")
    );
    assert!(!root.join("roaming/com.mengshouer.xmic").exists());
    assert!(!root.join("local/com.mengshouer.xmic").exists());
    assert_eq!(report["currentRuntimeCleaned"], true);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unwritable_portable_layout_fails_without_standard_mode_fallback() {
    let root = test_root("portable-unwritable");
    let executable = copy_standalone_executable(&root);
    fs::write(root.join("portable.flag"), []).unwrap();
    fs::write(root.join("data"), b"not a directory").unwrap();
    let report_path = root.join("report.json");

    let output = run_self_test(&executable, &root, &report_path);

    assert!(!output.status.success());
    assert!(!report_path.exists());
    assert!(!root.join("roaming/com.mengshouer.xmic").exists());
    assert!(!root.join("local/com.mengshouer.xmic").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_executable_sidecar_fails_closed_instead_of_using_embedded_dsp() {
    let root = test_root("invalid-sidecar");
    let executable = copy_standalone_executable(&root);
    fs::write(root.join("xmic_webrtc_dsp.dll"), b"not a PE library").unwrap();
    let report_path = root.join("report.json");

    let output = run_self_test(&executable, &root, &report_path);

    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["diagnostics"]["dspSource"], "executableSidecar");
    assert_eq!(report["diagnostics"]["dspCustom"], true);
    assert_eq!(report["diagnostics"]["dspSelectionPinned"], true);
    assert!(report["diagnostics"]["dspValidationError"].is_string());
    assert_eq!(report["embeddedLibraryMatches"], true);
    assert_eq!(report["currentRuntimeCleaned"], true);
    let _ = fs::remove_dir_all(root);
}
