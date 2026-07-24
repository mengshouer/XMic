use std::{fs::File, io::BufWriter, path::PathBuf, time::Instant};

use serde::Serialize;
use xmic_engine_core::{DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ};
use xmic_rnnoise_dsp::{RnnoiseConfig, RnnoiseDsp, LATENCY_SAMPLES};

use crate::wave::{create_parent_directory, read_device_wave, write_f32_wave};

#[derive(Debug)]
pub struct RnnoiseOptions {
    input: PathBuf,
    output: PathBuf,
    metrics: PathBuf,
    mix: f32,
}

pub fn parse_rnnoise_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<RnnoiseOptions, Box<dyn std::error::Error>> {
    let mut input = None;
    let mut output = None;
    let mut metrics = None;
    let mut mix = None;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--input" => set_once(&mut input, PathBuf::from(value), &flag)?,
            "--output" => set_once(&mut output, PathBuf::from(value), &flag)?,
            "--metrics" => set_once(&mut metrics, PathBuf::from(value), &flag)?,
            "--mix" => set_once(&mut mix, value.parse::<f32>()?, &flag)?,
            _ => return Err(format!("unknown rnnoise-capture option: {flag}").into()),
        }
    }

    let options = RnnoiseOptions {
        input: input.ok_or("--input is required")?,
        output: output.ok_or("--output is required")?,
        metrics: metrics.ok_or("--metrics is required")?,
        mix: mix.unwrap_or(1.0),
    };
    if !options.mix.is_finite() || !(0.0..=1.0).contains(&options.mix) {
        return Err("--mix must be a finite number between 0 and 1".into());
    }
    if options.input == options.output
        || options.input == options.metrics
        || options.output == options.metrics
    {
        return Err("rnnoise-capture paths must be distinct".into());
    }
    Ok(options)
}

pub fn run_rnnoise(options: RnnoiseOptions) -> Result<(), Box<dyn std::error::Error>> {
    let wave = read_device_wave(&options.input)?;
    let input = wave.decode_f32()?;
    if input.sample_rate_hz != DSP_SAMPLE_RATE_HZ as u32 || input.channels != 1 {
        return Err("RNNoise input WAV must be 48 kHz mono".into());
    }
    if input.non_finite_samples_replaced != 0 {
        return Err("RNNoise input WAV contains non-finite samples".into());
    }
    if input.frames % DSP_SAMPLES_PER_FRAME != 0 {
        return Err("RNNoise input WAV is not aligned to 10 ms blocks".into());
    }

    let blocks = input.frames / DSP_SAMPLES_PER_FRAME;
    let mut dsp = RnnoiseDsp::new(RnnoiseConfig { mix: options.mix })?;
    let mut output = vec![0.0; input.samples.len()];
    let mut processing_times_us = Vec::with_capacity(blocks);
    let mut vad_probabilities = Vec::with_capacity(blocks);

    for block in 0..blocks {
        let start = block * DSP_SAMPLES_PER_FRAME;
        let end = start + DSP_SAMPLES_PER_FRAME;
        let started = Instant::now();
        let frame_metrics =
            dsp.process_frame(&input.samples[start..end], &mut output[start..end])?;
        processing_times_us.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        vad_probabilities.push(frame_metrics.vad_probability);
    }

    write_f32_wave(
        &options.output,
        input.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &output,
    )?;
    let report = RnnoiseReport::new(
        &options,
        input.frames,
        &processing_times_us,
        &vad_probabilities,
    );
    create_parent_directory(&options.metrics)?;
    serde_json::to_writer_pretty(BufWriter::new(File::create(&options.metrics)?), &report)?;

    println!(
        "RNNoise: {} blocks, mix {:.2}, p99 {:.1} us -> {}",
        blocks,
        options.mix,
        report.processing_time_us.p99,
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

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RnnoiseReport {
    mix: f32,
    capture_frames: usize,
    blocks: usize,
    algorithmic_latency_frames: usize,
    algorithmic_latency_ms: f64,
    processing_time_us: ProcessingTimeReport,
    vad_probability: VadReport,
}

impl RnnoiseReport {
    fn new(
        options: &RnnoiseOptions,
        capture_frames: usize,
        processing_times_us: &[f64],
        vad_probabilities: &[f32],
    ) -> Self {
        let mut processing = processing_times_us.to_vec();
        processing.sort_by(f64::total_cmp);
        let mut vad = vad_probabilities
            .iter()
            .map(|value| f64::from(*value))
            .collect::<Vec<_>>();
        vad.sort_by(f64::total_cmp);
        Self {
            mix: options.mix,
            capture_frames,
            blocks: processing.len(),
            algorithmic_latency_frames: LATENCY_SAMPLES,
            algorithmic_latency_ms: LATENCY_SAMPLES as f64 * 1_000.0 / DSP_SAMPLE_RATE_HZ as f64,
            processing_time_us: ProcessingTimeReport {
                mean: processing.iter().sum::<f64>() / processing.len().max(1) as f64,
                p50: percentile(&processing, 0.50),
                p95: percentile(&processing, 0.95),
                p99: percentile(&processing, 0.99),
                maximum: processing.last().copied().unwrap_or(0.0),
            },
            vad_probability: VadReport {
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
struct ProcessingTimeReport {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    maximum: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VadReport {
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
    fn rejects_non_finite_mix() {
        let result = parse_rnnoise_options(
            [
                "--input",
                "input.wav",
                "--output",
                "output.wav",
                "--metrics",
                "metrics.json",
                "--mix",
                "NaN",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert!(result.is_err());
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.95), 4.0);
    }
}
