use std::{fs::File, io::BufWriter, path::PathBuf, process::ExitCode, time::Duration};

use xmic_platform_api::AudioPlatform;
use xmic_platform_windows::{capture_duplex, CaptureRequest, WasapiPlatform};

mod aec_capture;
mod final_capture;
mod normalize_capture;
mod rnnoise_capture;
mod wave;

use aec_capture::{parse_aec_options, run_aec};
use final_capture::{parse_final_options, run_final};
use normalize_capture::{parse_normalize_options, run_normalize};
use rnnoise_capture::{parse_rnnoise_options, run_rnnoise};
use wave::{create_parent_directory, write_captured_wave};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xmic audio spike failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments
        .next()
        .unwrap_or_else(|| "list-devices".to_owned());
    match command.as_str() {
        "list-devices" => {
            reject_extra_arguments(arguments)?;
            let endpoints = WasapiPlatform::new().active_endpoints()?;
            println!("{}", serde_json::to_string_pretty(&endpoints)?);
            Ok(())
        }
        "capture" => run_capture(parse_capture_options(arguments)?),
        "normalize-capture" => run_normalize(parse_normalize_options(arguments)?),
        "aec-capture" => run_aec(parse_aec_options(arguments)?),
        "final-capture" => run_final(parse_final_options(arguments)?),
        "rnnoise-capture" => run_rnnoise(parse_rnnoise_options(arguments)?),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => Err(format!("unknown command: {command}").into()),
    }
}

#[derive(Debug)]
struct CaptureOptions {
    seconds: u64,
    render_wav: PathBuf,
    mic_wav: PathBuf,
    metadata: PathBuf,
    render_id: Option<String>,
    mic_id: Option<String>,
}

fn parse_capture_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<CaptureOptions, Box<dyn std::error::Error>> {
    let mut seconds = None;
    let mut render_wav = None;
    let mut mic_wav = None;
    let mut metadata = None;
    let mut render_id = None;
    let mut mic_id = None;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--seconds" => set_once(&mut seconds, value.parse::<u64>()?, "--seconds")?,
            "--render-wav" => set_once(&mut render_wav, PathBuf::from(value), "--render-wav")?,
            "--mic-wav" => set_once(&mut mic_wav, PathBuf::from(value), "--mic-wav")?,
            "--metadata" => set_once(&mut metadata, PathBuf::from(value), "--metadata")?,
            "--render-id" => set_once(&mut render_id, value, "--render-id")?,
            "--mic-id" => set_once(&mut mic_id, value, "--mic-id")?,
            _ => return Err(format!("unknown capture option: {flag}").into()),
        }
    }

    let options = CaptureOptions {
        seconds: seconds.ok_or("--seconds is required")?,
        render_wav: render_wav.ok_or("--render-wav is required")?,
        mic_wav: mic_wav.ok_or("--mic-wav is required")?,
        metadata: metadata.ok_or("--metadata is required")?,
        render_id,
        mic_id,
    };
    if !(1..=120).contains(&options.seconds) {
        return Err("--seconds must be between 1 and 120".into());
    }
    if options.render_wav == options.mic_wav
        || options.render_wav == options.metadata
        || options.mic_wav == options.metadata
    {
        return Err("capture output paths must be distinct".into());
    }
    Ok(options)
}

fn set_once<T>(
    destination: &mut Option<T>,
    value: T,
    flag: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if destination.replace(value).is_some() {
        Err(format!("{flag} may only be specified once").into())
    } else {
        Ok(())
    }
}

fn reject_extra_arguments(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(argument) = arguments.next() {
        Err(format!("unexpected argument: {argument}").into())
    } else {
        Ok(())
    }
}

fn run_capture(options: CaptureOptions) -> Result<(), Box<dyn std::error::Error>> {
    let capture = capture_duplex(CaptureRequest {
        duration: Duration::from_secs(options.seconds),
        render_endpoint_id: options.render_id,
        capture_endpoint_id: options.mic_id,
    })?;

    write_captured_wave(&options.render_wav, &capture.render)?;
    write_captured_wave(&options.mic_wav, &capture.capture)?;
    create_parent_directory(&options.metadata)?;
    serde_json::to_writer_pretty(BufWriter::new(File::create(&options.metadata)?), &capture)?;

    println!(
        "render: {} frames, {} non-silent, {} packets -> {}",
        capture.render.captured_frames,
        capture.render.non_silent_frames,
        capture.render.packets.len(),
        options.render_wav.display()
    );
    println!(
        "capture: {} frames, {} non-silent, {} packets -> {}",
        capture.capture.captured_frames,
        capture.capture.non_silent_frames,
        capture.capture.packets.len(),
        options.mic_wav.display()
    );
    println!("metadata -> {}", options.metadata.display());
    Ok(())
}

fn print_help() {
    println!(
        r#"xmic-audio-spike

Commands:
  list-devices
  capture --seconds <1..120> --render-wav <path> --mic-wav <path>
    --metadata <path> [--render-id <endpoint-id>] [--mic-id <endpoint-id>]
  normalize-capture --render-wav <path> --mic-wav <path> --metadata <path>
    --render-output <path> --mic-output <path> --metrics <path>
    [--mic-channel <index>]
  aec-capture --render-wav <normalized-path> --mic-wav <normalized-path>
    --metadata <path> --dsp-library <path> --raw-output <path>
    --aec-output <path> --metrics <path> --stream-delay-ms <0..500>
    [--preset <natural|balanced|strong>] [--high-pass <true|false>]
  rnnoise-capture --input <aec-path> --output <final-path>
    --metrics <path> [--mix <0..1>]
  final-capture --input <aec-path> --output <final-path> --metrics <path>
    --dsp-library <path> [--rnnoise-mix <0..1>] [--agc2-enabled <true|false>]
    [--vad-gate <true|false>] [--limiter-ceiling-dbfs <-30..0>]"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_options_require_distinct_outputs() {
        let result = parse_capture_options(
            [
                "--seconds",
                "5",
                "--render-wav",
                "same.wav",
                "--mic-wav",
                "same.wav",
                "--metadata",
                "metadata.json",
            ]
            .into_iter()
            .map(str::to_owned),
        );

        assert!(result.is_err());
    }
}
