// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut args = std::env::args_os();
    let _program = args.next();
    match args.next().as_deref() {
        Some(argument) if argument == std::ffi::OsStr::new("--audio-engine") => {
            if xmic_lib::run_audio_engine_process().is_err() {
                std::process::exit(1);
            }
        }
        Some(argument) if argument == std::ffi::OsStr::new("--runtime-self-test") => {
            let Some(report_path) = args.next() else {
                std::process::exit(2);
            };
            if xmic_lib::run_runtime_self_test(std::path::Path::new(&report_path)).is_err() {
                std::process::exit(1);
            }
        }
        _ => {
            if xmic_lib::run().is_err() {
                std::process::exit(1);
            }
        }
    }
}
