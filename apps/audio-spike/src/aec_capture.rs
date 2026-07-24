use std::{fs::File, io::BufWriter, path::PathBuf, time::Instant};

use serde::{Deserialize, Serialize};
use xmic_engine_core::{DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ};
use xmic_webrtc_dsp::{AecPreset, DspConfig, DspMetrics, WebRtcDsp};

use crate::wave::{create_parent_directory, read_device_wave, write_f32_wave};

#[derive(Debug)]
pub struct AecOptions {
    render_wav: PathBuf,
    mic_wav: PathBuf,
    metadata: PathBuf,
    dsp_library: PathBuf,
    raw_output: PathBuf,
    aec_output: PathBuf,
    metrics: PathBuf,
    stream_delay_ms: i32,
    preset: AecPreset,
    high_pass_filter_enabled: bool,
}

pub fn parse_aec_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<AecOptions, Box<dyn std::error::Error>> {
    let mut render_wav = None;
    let mut mic_wav = None;
    let mut metadata = None;
    let mut dsp_library = None;
    let mut raw_output = None;
    let mut aec_output = None;
    let mut metrics = None;
    let mut stream_delay_ms = None;
    let mut preset = None;
    let mut high_pass_filter_enabled = None;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--render-wav" => set_once(&mut render_wav, PathBuf::from(value), &flag)?,
            "--mic-wav" => set_once(&mut mic_wav, PathBuf::from(value), &flag)?,
            "--metadata" => set_once(&mut metadata, PathBuf::from(value), &flag)?,
            "--dsp-library" => set_once(&mut dsp_library, PathBuf::from(value), &flag)?,
            "--raw-output" => set_once(&mut raw_output, PathBuf::from(value), &flag)?,
            "--aec-output" => set_once(&mut aec_output, PathBuf::from(value), &flag)?,
            "--metrics" => set_once(&mut metrics, PathBuf::from(value), &flag)?,
            "--stream-delay-ms" => set_once(&mut stream_delay_ms, value.parse::<i32>()?, &flag)?,
            "--preset" => set_once(&mut preset, parse_preset(&value)?, &flag)?,
            "--high-pass" => {
                set_once(&mut high_pass_filter_enabled, parse_boolean(&value)?, &flag)?
            }
            _ => return Err(format!("unknown aec-capture option: {flag}").into()),
        }
    }

    let options = AecOptions {
        render_wav: render_wav.ok_or("--render-wav is required")?,
        mic_wav: mic_wav.ok_or("--mic-wav is required")?,
        metadata: metadata.ok_or("--metadata is required")?,
        dsp_library: dsp_library.ok_or("--dsp-library is required")?,
        raw_output: raw_output.ok_or("--raw-output is required")?,
        aec_output: aec_output.ok_or("--aec-output is required")?,
        metrics: metrics.ok_or("--metrics is required")?,
        stream_delay_ms: stream_delay_ms.ok_or("--stream-delay-ms is required")?,
        preset: preset.unwrap_or(AecPreset::Natural),
        high_pass_filter_enabled: high_pass_filter_enabled.unwrap_or(true),
    };
    if !(0..=500).contains(&options.stream_delay_ms) {
        return Err("--stream-delay-ms must be between 0 and 500".into());
    }
    let paths = [
        &options.render_wav,
        &options.mic_wav,
        &options.metadata,
        &options.dsp_library,
        &options.raw_output,
        &options.aec_output,
        &options.metrics,
    ];
    for left in 0..paths.len() {
        for right in left + 1..paths.len() {
            if paths[left] == paths[right] {
                return Err("aec-capture paths must be distinct".into());
            }
        }
    }
    Ok(options)
}

pub fn run_aec(options: AecOptions) -> Result<(), Box<dyn std::error::Error>> {
    let timing: DuplexTiming = serde_json::from_reader(File::open(&options.metadata)?)?;
    let render_wave = read_device_wave(&options.render_wav)?;
    let mic_wave = read_device_wave(&options.mic_wav)?;
    let render = render_wave.decode_f32()?;
    let capture = mic_wave.decode_f32()?;
    validate_normalized_input(&render, "render", 1, 2)?;
    validate_normalized_input(&capture, "capture", 1, 1)?;
    if capture.frames % DSP_SAMPLES_PER_FRAME != 0 {
        return Err("capture WAV is not aligned to 10 ms blocks".into());
    }

    let render_start_qpc = first_qpc(&timing.render, "render")?;
    let capture_start_qpc = first_qpc(&timing.capture, "capture")?;
    let render_offset_frames = qpc_offset_frames(render_start_qpc, capture_start_qpc);
    let block_count = capture.frames / DSP_SAMPLES_PER_FRAME;
    let mut dsp = WebRtcDsp::load(
        &options.dsp_library,
        DspConfig {
            render_channels: render.channels,
            aec_preset: options.preset,
            high_pass_filter_enabled: options.high_pass_filter_enabled,
        },
    )?;
    let mut render_block = vec![0.0; DSP_SAMPLES_PER_FRAME * render.channels];
    let mut aec_samples = vec![0.0; capture.frames];
    let mut block_times_us = Vec::with_capacity(block_count);
    let mut render_padding_frames = 0u64;

    for block_index in 0..block_count {
        let capture_start = block_index * DSP_SAMPLES_PER_FRAME;
        render_padding_frames += fill_aligned_render_block(
            &render.samples,
            render.frames,
            render.channels,
            capture_start,
            render_offset_frames,
            &mut render_block,
        )?;
        let capture_end = capture_start + DSP_SAMPLES_PER_FRAME;
        let started = Instant::now();
        dsp.process_render(&render_block)?;
        dsp.process_capture(
            &capture.samples[capture_start..capture_end],
            &mut aec_samples[capture_start..capture_end],
            options.stream_delay_ms,
        )?;
        block_times_us.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }

    write_f32_wave(
        &options.raw_output,
        capture.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &capture.samples,
    )?;
    write_f32_wave(
        &options.aec_output,
        capture.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &aec_samples,
    )?;

    let report = AecReport::new(
        &options,
        render.channels,
        capture.frames,
        render_offset_frames,
        render_padding_frames,
        &block_times_us,
        dsp.metrics()?,
    );
    create_parent_directory(&options.metrics)?;
    serde_json::to_writer_pretty(BufWriter::new(File::create(&options.metrics)?), &report)?;

    println!(
        "AEC3: {} blocks, render offset {} frames, p99 {:.1} us -> {}",
        block_count,
        render_offset_frames,
        report.processing_time_us.p99,
        options.aec_output.display()
    );
    println!("raw -> {}", options.raw_output.display());
    println!("metrics -> {}", options.metrics.display());
    Ok(())
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

fn parse_preset(value: &str) -> Result<AecPreset, Box<dyn std::error::Error>> {
    match value {
        "natural" => Ok(AecPreset::Natural),
        "balanced" => Ok(AecPreset::Balanced),
        "strong" => Ok(AecPreset::Strong),
        _ => Err("--preset must be natural, balanced, or strong".into()),
    }
}

fn preset_name(preset: AecPreset) -> &'static str {
    match preset {
        AecPreset::Natural => "natural",
        AecPreset::Balanced => "balanced",
        AecPreset::Strong => "strong",
    }
}

fn parse_boolean(value: &str) -> Result<bool, Box<dyn std::error::Error>> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("boolean value must be true or false".into()),
    }
}

fn validate_normalized_input(
    input: &xmic_platform_windows::DecodedInterleavedAudio,
    label: &str,
    minimum_channels: usize,
    maximum_channels: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if input.sample_rate_hz != DSP_SAMPLE_RATE_HZ as u32 {
        return Err(format!("{label} WAV must be 48 kHz").into());
    }
    if !(minimum_channels..=maximum_channels).contains(&input.channels) {
        return Err(format!(
            "{label} WAV has {} channels; expected {minimum_channels}..={maximum_channels}",
            input.channels
        )
        .into());
    }
    if input.non_finite_samples_replaced != 0 {
        return Err(format!("{label} WAV contains non-finite samples").into());
    }
    Ok(())
}

fn first_qpc(timing: &StreamTiming, label: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let packet = timing
        .packets
        .first()
        .ok_or_else(|| format!("{label} packet timeline is empty"))?;
    if packet.timestamp_error {
        return Err(format!("{label} first packet has a timestamp error").into());
    }
    Ok(packet.qpc_position_100ns)
}

fn qpc_offset_frames(render_qpc: u64, capture_qpc: u64) -> i64 {
    let delta_100ns = capture_qpc as i128 - render_qpc as i128;
    (delta_100ns as f64 * DSP_SAMPLE_RATE_HZ as f64 / 10_000_000.0).round() as i64
}

fn fill_aligned_render_block(
    render: &[f32],
    render_frames: usize,
    channels: usize,
    capture_start: usize,
    render_offset_frames: i64,
    output: &mut [f32],
) -> Result<u64, Box<dyn std::error::Error>> {
    if output.len() != DSP_SAMPLES_PER_FRAME * channels {
        return Err("render block buffer has the wrong length".into());
    }
    let mut padding_frames = 0u64;
    for frame in 0..DSP_SAMPLES_PER_FRAME {
        let render_frame = capture_start as i128 + frame as i128 + render_offset_frames as i128;
        if (0..render_frames as i128).contains(&render_frame) {
            let render_frame = usize::try_from(render_frame)?;
            let input_start = render_frame * channels;
            let output_start = frame * channels;
            output[output_start..output_start + channels]
                .copy_from_slice(&render[input_start..input_start + channels]);
        } else {
            let output_start = frame * channels;
            output[output_start..output_start + channels].fill(0.0);
            padding_frames += 1;
        }
    }
    Ok(padding_frames)
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplexTiming {
    render: StreamTiming,
    capture: StreamTiming,
}

#[derive(Debug, Deserialize)]
struct StreamTiming {
    packets: Vec<PacketTiming>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PacketTiming {
    qpc_position_100ns: u64,
    timestamp_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AecReport {
    preset: &'static str,
    high_pass_filter_enabled: bool,
    render_channels: usize,
    capture_frames: usize,
    blocks: usize,
    stream_delay_ms: i32,
    render_qpc_offset_frames: i64,
    render_qpc_offset_ms: f64,
    render_padding_frames: u64,
    processing_time_us: ProcessingTimeReport,
    webrtc: WebRtcMetricsReport,
}

impl AecReport {
    #[allow(clippy::too_many_arguments)]
    fn new(
        options: &AecOptions,
        render_channels: usize,
        capture_frames: usize,
        render_qpc_offset_frames: i64,
        render_padding_frames: u64,
        processing_times_us: &[f64],
        metrics: DspMetrics,
    ) -> Self {
        let mut sorted = processing_times_us.to_vec();
        sorted.sort_by(f64::total_cmp);
        Self {
            preset: preset_name(options.preset),
            high_pass_filter_enabled: options.high_pass_filter_enabled,
            render_channels,
            capture_frames,
            blocks: processing_times_us.len(),
            stream_delay_ms: options.stream_delay_ms,
            render_qpc_offset_frames,
            render_qpc_offset_ms: render_qpc_offset_frames as f64 * 1_000.0
                / DSP_SAMPLE_RATE_HZ as f64,
            render_padding_frames,
            processing_time_us: ProcessingTimeReport {
                mean: sorted.iter().sum::<f64>() / sorted.len().max(1) as f64,
                p50: percentile(&sorted, 0.50),
                p95: percentile(&sorted, 0.95),
                p99: percentile(&sorted, 0.99),
                maximum: sorted.last().copied().unwrap_or(0.0),
            },
            webrtc: WebRtcMetricsReport {
                stream_delay_ms: metrics.stream_delay_ms,
                estimated_delay_ms: metrics.estimated_delay_ms,
                echo_return_loss_db: metrics.echo_return_loss_db,
                echo_return_loss_enhancement_db: metrics.echo_return_loss_enhancement_db,
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingTimeReport {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    maximum: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRtcMetricsReport {
    stream_delay_ms: i32,
    estimated_delay_ms: Option<i32>,
    echo_return_loss_db: Option<f32>,
    echo_return_loss_enhancement_db: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_alignment_is_negative_when_render_starts_later() {
        assert_eq!(qpc_offset_frames(20_000_000, 10_000_000), -48_000);
    }

    #[test]
    fn render_alignment_pads_before_render_start() {
        let render = [1.0, 2.0, 3.0, 4.0];
        let mut output = vec![9.0; DSP_SAMPLES_PER_FRAME];
        let padding =
            fill_aligned_render_block(&render, render.len(), 1, 0, -2, &mut output).unwrap();

        assert_eq!(padding, (DSP_SAMPLES_PER_FRAME - render.len()) as u64);
        assert_eq!(&output[..6], &[0.0, 0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.50), 2.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.99), 4.0);
    }
}
