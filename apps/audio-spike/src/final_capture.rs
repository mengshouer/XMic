use std::{fs::File, io::BufWriter, path::PathBuf, time::Instant};

use serde::Serialize;
use xmic_engine_core::{
    SafetyLimiter, SafetyLimiterConfig, VadGate, VadGateConfig, DSP_FRAME_DURATION_MS,
    DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ,
};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp, LATENCY_SAMPLES};
use xmic_webrtc_dsp::{Agc2Config, WebRtcAgc2};

use crate::wave::{create_parent_directory, read_device_wave, write_f32_wave};

#[derive(Debug)]
pub struct FinalOptions {
    input: PathBuf,
    output: PathBuf,
    metrics: PathBuf,
    dsp_library: Option<PathBuf>,
    rnnoise_mix: f32,
    agc2_enabled: bool,
    agc2: Agc2Config,
    vad_gate: VadGateConfig,
    limiter: SafetyLimiterConfig,
}

pub fn parse_final_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<FinalOptions, Box<dyn std::error::Error>> {
    let mut input = None;
    let mut output = None;
    let mut metrics = None;
    let mut dsp_library = None;
    let mut rnnoise_mix = None;
    let mut agc2_enabled = None;
    let mut agc2_adaptive = None;
    let mut agc2_fixed_gain_db = None;
    let mut agc2_headroom_db = None;
    let mut agc2_max_gain_db = None;
    let mut agc2_initial_gain_db = None;
    let mut agc2_max_gain_change_db_per_second = None;
    let mut agc2_max_output_noise_level_dbfs = None;
    let mut vad_gate_enabled = None;
    let mut vad_threshold = None;
    let mut vad_hold_ms = None;
    let mut vad_floor_db = None;
    let mut vad_attack_ms = None;
    let mut vad_release_ms = None;
    let mut limiter_ceiling_dbfs = None;
    let mut limiter_release_ms = None;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--input" => set_once(&mut input, PathBuf::from(value), &flag)?,
            "--output" => set_once(&mut output, PathBuf::from(value), &flag)?,
            "--metrics" => set_once(&mut metrics, PathBuf::from(value), &flag)?,
            "--dsp-library" => set_once(&mut dsp_library, PathBuf::from(value), &flag)?,
            "--rnnoise-mix" => set_once(&mut rnnoise_mix, value.parse::<f32>()?, &flag)?,
            "--agc2-enabled" => set_once(&mut agc2_enabled, parse_boolean(&value)?, &flag)?,
            "--agc2-adaptive" => set_once(&mut agc2_adaptive, parse_boolean(&value)?, &flag)?,
            "--agc2-fixed-gain-db" => {
                set_once(&mut agc2_fixed_gain_db, value.parse::<f32>()?, &flag)?
            }
            "--agc2-headroom-db" => set_once(&mut agc2_headroom_db, value.parse::<f32>()?, &flag)?,
            "--agc2-max-gain-db" => set_once(&mut agc2_max_gain_db, value.parse::<f32>()?, &flag)?,
            "--agc2-initial-gain-db" => {
                set_once(&mut agc2_initial_gain_db, value.parse::<f32>()?, &flag)?
            }
            "--agc2-max-gain-change-db-per-second" => set_once(
                &mut agc2_max_gain_change_db_per_second,
                value.parse::<f32>()?,
                &flag,
            )?,
            "--agc2-max-output-noise-level-dbfs" => set_once(
                &mut agc2_max_output_noise_level_dbfs,
                value.parse::<f32>()?,
                &flag,
            )?,
            "--vad-gate" => set_once(&mut vad_gate_enabled, parse_boolean(&value)?, &flag)?,
            "--vad-threshold" => set_once(&mut vad_threshold, value.parse::<f32>()?, &flag)?,
            "--vad-hold-ms" => set_once(&mut vad_hold_ms, value.parse::<u32>()?, &flag)?,
            "--vad-floor-db" => set_once(&mut vad_floor_db, value.parse::<f32>()?, &flag)?,
            "--vad-attack-ms" => set_once(&mut vad_attack_ms, value.parse::<f32>()?, &flag)?,
            "--vad-release-ms" => set_once(&mut vad_release_ms, value.parse::<f32>()?, &flag)?,
            "--limiter-ceiling-dbfs" => {
                set_once(&mut limiter_ceiling_dbfs, value.parse::<f32>()?, &flag)?
            }
            "--limiter-release-ms" => {
                set_once(&mut limiter_release_ms, value.parse::<f32>()?, &flag)?
            }
            _ => return Err(format!("unknown final-capture option: {flag}").into()),
        }
    }

    let default_agc2 = Agc2Config::default();
    let default_gate = VadGateConfig::default();
    let default_limiter = SafetyLimiterConfig::default();
    let agc2_enabled = agc2_enabled.unwrap_or(true);
    let dsp_library = dsp_library;
    if agc2_enabled && dsp_library.is_none() {
        return Err("--dsp-library is required when AGC2 is enabled".into());
    }
    let hold_ms = vad_hold_ms.unwrap_or(
        u32::try_from(default_gate.hold_frames * DSP_FRAME_DURATION_MS)
            .map_err(|_| "default VAD hold overflow")?,
    );
    let hold_frames = usize::try_from(hold_ms)?.div_ceil(DSP_FRAME_DURATION_MS);
    let options = FinalOptions {
        input: input.ok_or("--input is required")?,
        output: output.ok_or("--output is required")?,
        metrics: metrics.ok_or("--metrics is required")?,
        dsp_library,
        rnnoise_mix: rnnoise_mix.unwrap_or(1.0),
        agc2_enabled,
        agc2: Agc2Config {
            adaptive_digital_enabled: agc2_adaptive
                .unwrap_or(default_agc2.adaptive_digital_enabled),
            fixed_gain_db: agc2_fixed_gain_db.unwrap_or(default_agc2.fixed_gain_db),
            headroom_db: agc2_headroom_db.unwrap_or(default_agc2.headroom_db),
            max_gain_db: agc2_max_gain_db.unwrap_or(default_agc2.max_gain_db),
            initial_gain_db: agc2_initial_gain_db.unwrap_or(default_agc2.initial_gain_db),
            max_gain_change_db_per_second: agc2_max_gain_change_db_per_second
                .unwrap_or(default_agc2.max_gain_change_db_per_second),
            max_output_noise_level_dbfs: agc2_max_output_noise_level_dbfs
                .unwrap_or(default_agc2.max_output_noise_level_dbfs),
        },
        vad_gate: VadGateConfig {
            enabled: vad_gate_enabled.unwrap_or(default_gate.enabled),
            threshold: vad_threshold.unwrap_or(default_gate.threshold),
            confirmation_frames: 1,
            pre_roll_frames: 0,
            hold_frames,
            floor_db: vad_floor_db.unwrap_or(default_gate.floor_db),
            attack_ms: vad_attack_ms.unwrap_or(default_gate.attack_ms),
            release_ms: vad_release_ms.unwrap_or(default_gate.release_ms),
        },
        limiter: SafetyLimiterConfig {
            ceiling_dbfs: limiter_ceiling_dbfs.unwrap_or(default_limiter.ceiling_dbfs),
            release_ms: limiter_release_ms.unwrap_or(default_limiter.release_ms),
        },
    };
    if options.input == options.output
        || options.input == options.metrics
        || options.output == options.metrics
    {
        return Err("final-capture input/output/metrics paths must be distinct".into());
    }
    Ok(options)
}

pub fn run_final(options: FinalOptions) -> Result<(), Box<dyn std::error::Error>> {
    let wave = read_device_wave(&options.input)?;
    let input = wave.decode_f32()?;
    if input.sample_rate_hz != DSP_SAMPLE_RATE_HZ as u32 || input.channels != 1 {
        return Err("Final pipeline input WAV must be 48 kHz mono".into());
    }
    if input.non_finite_samples_replaced != 0 {
        return Err("Final pipeline input WAV contains non-finite samples".into());
    }
    if input.frames % DSP_SAMPLES_PER_FRAME != 0 {
        return Err("Final pipeline input WAV is not aligned to 10 ms blocks".into());
    }

    let blocks = input.frames / DSP_SAMPLES_PER_FRAME;
    let mut rnnoise = RnnoiseDsp::new(RnnoiseConfig {
        mix: options.rnnoise_mix,
    })?;
    let mut agc2 = if options.agc2_enabled {
        Some(WebRtcAgc2::load(
            options
                .dsp_library
                .as_ref()
                .ok_or("AGC2 library path is missing")?,
            options.agc2,
        )?)
    } else {
        None
    };
    let mut gate = VadGate::new(options.vad_gate)?;
    let mut limiter = SafetyLimiter::new(options.limiter)?;
    let mut rnnoise_buffer = [0.0; DSP_SAMPLES_PER_FRAME];
    let mut post_agc_buffer = [0.0; DSP_SAMPLES_PER_FRAME];
    let mut output = vec![0.0; input.samples.len()];
    let mut total_times_us = Vec::with_capacity(blocks);
    let mut rnnoise_times_us = Vec::with_capacity(blocks);
    let mut agc2_times_us = Vec::with_capacity(blocks);
    let mut post_times_us = Vec::with_capacity(blocks);
    let mut vad_probabilities = Vec::with_capacity(blocks);
    let mut gate_open_blocks = 0usize;
    let mut minimum_gate_gain = 1.0f32;
    let mut limited_samples = 0usize;
    let mut minimum_limiter_gain = 1.0f32;

    for block in 0..blocks {
        let start = block * DSP_SAMPLES_PER_FRAME;
        let end = start + DSP_SAMPLES_PER_FRAME;
        let total_started = Instant::now();
        let rnnoise_started = Instant::now();
        let rnnoise_metrics =
            rnnoise.process_frame(&input.samples[start..end], &mut rnnoise_buffer)?;
        rnnoise_times_us.push(rnnoise_started.elapsed().as_secs_f64() * 1_000_000.0);

        let agc2_started = Instant::now();
        if let Some(agc2) = &mut agc2 {
            agc2.process_capture(&rnnoise_buffer, &mut post_agc_buffer)?;
        } else {
            post_agc_buffer.copy_from_slice(&rnnoise_buffer);
        }
        agc2_times_us.push(agc2_started.elapsed().as_secs_f64() * 1_000_000.0);

        let post_started = Instant::now();
        let gate_metrics =
            gate.process_frame(rnnoise_metrics.vad_probability, &mut post_agc_buffer)?;
        let limiter_metrics = limiter.process_frame(&mut post_agc_buffer)?;
        post_times_us.push(post_started.elapsed().as_secs_f64() * 1_000_000.0);
        output[start..end].copy_from_slice(&post_agc_buffer);
        total_times_us.push(total_started.elapsed().as_secs_f64() * 1_000_000.0);

        vad_probabilities.push(rnnoise_metrics.vad_probability);
        gate_open_blocks += usize::from(gate_metrics.open);
        minimum_gate_gain = minimum_gate_gain.min(gate_metrics.minimum_gain);
        limited_samples += limiter_metrics.limited_samples;
        minimum_limiter_gain = minimum_limiter_gain.min(limiter_metrics.minimum_gain);
    }

    write_f32_wave(
        &options.output,
        input.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &output,
    )?;
    let report = FinalReport::new(
        &options,
        input.frames,
        &total_times_us,
        &rnnoise_times_us,
        &agc2_times_us,
        &post_times_us,
        &vad_probabilities,
        gate_open_blocks,
        minimum_gate_gain,
        limited_samples,
        minimum_limiter_gain,
        peak(&input.samples),
        peak(&output),
    );
    create_parent_directory(&options.metrics)?;
    serde_json::to_writer_pretty(BufWriter::new(File::create(&options.metrics)?), &report)?;
    println!(
        "Final: {} blocks, p99 {:.1} us, output peak {:.2} dBFS -> {}",
        blocks,
        report.processing_time_us.total.p99,
        report.output_peak_dbfs,
        options.output.display()
    );
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

fn parse_boolean(value: &str) -> Result<bool, Box<dyn std::error::Error>> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("boolean value must be true or false".into()),
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .map(f32::abs)
        .fold(0.0, f32::max)
}

fn amplitude_to_dbfs(amplitude: f32) -> f32 {
    if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        f32::NEG_INFINITY
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FinalReport {
    capture_frames: usize,
    blocks: usize,
    algorithmic_latency_frames: usize,
    algorithmic_latency_ms: f64,
    rnnoise_mix: f32,
    agc2: Agc2Report,
    vad_gate: VadGateReport,
    limiter: LimiterReport,
    input_peak_dbfs: f32,
    output_peak_dbfs: f32,
    processing_time_us: StageTimingReport,
    vad_probability: VadProbabilityReport,
}

impl FinalReport {
    #[allow(clippy::too_many_arguments)]
    fn new(
        options: &FinalOptions,
        capture_frames: usize,
        total_times_us: &[f64],
        rnnoise_times_us: &[f64],
        agc2_times_us: &[f64],
        post_times_us: &[f64],
        vad_probabilities: &[f32],
        gate_open_blocks: usize,
        minimum_gate_gain: f32,
        limited_samples: usize,
        minimum_limiter_gain: f32,
        input_peak: f32,
        output_peak: f32,
    ) -> Self {
        let mut vad = vad_probabilities
            .iter()
            .map(|value| f64::from(*value))
            .collect::<Vec<_>>();
        vad.sort_by(f64::total_cmp);
        Self {
            capture_frames,
            blocks: total_times_us.len(),
            algorithmic_latency_frames: LATENCY_SAMPLES,
            algorithmic_latency_ms: LATENCY_SAMPLES as f64 * 1_000.0 / DSP_SAMPLE_RATE_HZ as f64,
            rnnoise_mix: options.rnnoise_mix,
            agc2: Agc2Report::from_options(options),
            vad_gate: VadGateReport {
                enabled: options.vad_gate.enabled,
                threshold: options.vad_gate.threshold,
                hold_ms: options.vad_gate.hold_frames * DSP_FRAME_DURATION_MS,
                floor_db: options.vad_gate.floor_db,
                attack_ms: options.vad_gate.attack_ms,
                release_ms: options.vad_gate.release_ms,
                open_blocks: gate_open_blocks,
                minimum_gain: minimum_gate_gain,
            },
            limiter: LimiterReport {
                ceiling_dbfs: options.limiter.ceiling_dbfs,
                release_ms: options.limiter.release_ms,
                limited_samples,
                minimum_gain: minimum_limiter_gain,
            },
            input_peak_dbfs: amplitude_to_dbfs(input_peak),
            output_peak_dbfs: amplitude_to_dbfs(output_peak),
            processing_time_us: StageTimingReport {
                total: TimingSummary::new(total_times_us),
                rnnoise: TimingSummary::new(rnnoise_times_us),
                agc2: TimingSummary::new(agc2_times_us),
                gate_and_limiter: TimingSummary::new(post_times_us),
            },
            vad_probability: VadProbabilityReport {
                mean: vad.iter().sum::<f64>() / vad.len().max(1) as f64,
                p50: percentile(&vad, 0.50),
                p95: percentile(&vad, 0.95),
                maximum: vad.last().copied().unwrap_or(0.0),
                blocks_at_or_above_half: vad.iter().filter(|value| **value >= 0.5).count(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Agc2Report {
    enabled: bool,
    adaptive_digital_enabled: bool,
    fixed_gain_db: f32,
    headroom_db: f32,
    max_gain_db: f32,
    initial_gain_db: f32,
    max_gain_change_db_per_second: f32,
    max_output_noise_level_dbfs: f32,
}

impl Agc2Report {
    fn from_options(options: &FinalOptions) -> Self {
        Self {
            enabled: options.agc2_enabled,
            adaptive_digital_enabled: options.agc2.adaptive_digital_enabled,
            fixed_gain_db: options.agc2.fixed_gain_db,
            headroom_db: options.agc2.headroom_db,
            max_gain_db: options.agc2.max_gain_db,
            initial_gain_db: options.agc2.initial_gain_db,
            max_gain_change_db_per_second: options.agc2.max_gain_change_db_per_second,
            max_output_noise_level_dbfs: options.agc2.max_output_noise_level_dbfs,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VadGateReport {
    enabled: bool,
    threshold: f32,
    hold_ms: usize,
    floor_db: f32,
    attack_ms: f32,
    release_ms: f32,
    open_blocks: usize,
    minimum_gain: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LimiterReport {
    ceiling_dbfs: f32,
    release_ms: f32,
    limited_samples: usize,
    minimum_gain: f32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StageTimingReport {
    total: TimingSummary,
    rnnoise: TimingSummary,
    agc2: TimingSummary,
    gate_and_limiter: TimingSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimingSummary {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    maximum: f64,
}

impl TimingSummary {
    fn new(values: &[f64]) -> Self {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        Self {
            mean: sorted.iter().sum::<f64>() / sorted.len().max(1) as f64,
            p50: percentile(&sorted, 0.50),
            p95: percentile(&sorted, 0.95),
            p99: percentile(&sorted, 0.99),
            maximum: sorted.last().copied().unwrap_or(0.0),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VadProbabilityReport {
    mean: f64,
    p50: f64,
    p95: f64,
    maximum: f64,
    blocks_at_or_above_half: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc2_requires_library_path() {
        let result = parse_final_options(
            [
                "--input",
                "input.wav",
                "--output",
                "output.wav",
                "--metrics",
                "metrics.json",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert!(result.is_err());
    }

    #[test]
    fn allows_agc2_bypass_without_library() {
        let result = parse_final_options(
            [
                "--input",
                "input.wav",
                "--output",
                "output.wav",
                "--metrics",
                "metrics.json",
                "--agc2-enabled",
                "false",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert!(result.is_ok());
    }
}
