use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture");
    let artifact_directory = match target_arch.as_str() {
        "x86_64" => "win-x64",
        unsupported => panic!(
            "XMic has no bundled WebRTC DSP artifact for target architecture {unsupported}; only x64 releases are currently supported"
        ),
    };
    let dsp_library = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("..")
        .join("artifacts")
        .join("webrtc")
        .join(artifact_directory)
        .join("xmic_webrtc_dsp.dll");
    if !dsp_library.is_file() {
        panic!(
            "bundled WebRTC DSP artifact is missing: {}",
            dsp_library.display()
        );
    }
    println!("cargo:rerun-if-changed={}", dsp_library.display());
    println!(
        "cargo:rustc-env=XMIC_EMBEDDED_DSP_PATH={}",
        dsp_library.display()
    );

    tauri_build::build()
}
