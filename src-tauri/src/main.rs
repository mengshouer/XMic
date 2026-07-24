// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--audio-engine")) {
        if xmic_lib::run_audio_engine_process().is_err() {
            std::process::exit(1);
        }
    } else {
        xmic_lib::run();
    }
}
