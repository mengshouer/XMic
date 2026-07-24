use std::{fs::File, io::BufWriter, path::PathBuf};

use serde::{Deserialize, Serialize};
use xmic_engine_core::{
    AdaptiveNormalizer, ChannelMapper, ClockObservation, NormalizedBlock, NormalizerMetrics,
    DEFAULT_QUEUE_CAPACITY_FRAMES, DSP_SAMPLES_PER_FRAME, DSP_SAMPLE_RATE_HZ,
};
use xmic_platform_windows::DecodedInterleavedAudio;

use crate::wave::{create_parent_directory, read_device_wave, write_f32_wave};

#[derive(Debug)]
pub struct NormalizeOptions {
    render_wav: PathBuf,
    mic_wav: PathBuf,
    metadata: PathBuf,
    render_output: PathBuf,
    mic_output: PathBuf,
    metrics: PathBuf,
    mic_channel: usize,
}

pub fn parse_normalize_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<NormalizeOptions, Box<dyn std::error::Error>> {
    let mut render_wav = None;
    let mut mic_wav = None;
    let mut metadata = None;
    let mut render_output = None;
    let mut mic_output = None;
    let mut metrics = None;
    let mut mic_channel = None;

    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--render-wav" => set_once(&mut render_wav, PathBuf::from(value), &flag)?,
            "--mic-wav" => set_once(&mut mic_wav, PathBuf::from(value), &flag)?,
            "--metadata" => set_once(&mut metadata, PathBuf::from(value), &flag)?,
            "--render-output" => set_once(&mut render_output, PathBuf::from(value), &flag)?,
            "--mic-output" => set_once(&mut mic_output, PathBuf::from(value), &flag)?,
            "--metrics" => set_once(&mut metrics, PathBuf::from(value), &flag)?,
            "--mic-channel" => set_once(&mut mic_channel, value.parse::<usize>()?, &flag)?,
            _ => return Err(format!("unknown normalize-capture option: {flag}").into()),
        }
    }

    let options = NormalizeOptions {
        render_wav: render_wav.ok_or("--render-wav is required")?,
        mic_wav: mic_wav.ok_or("--mic-wav is required")?,
        metadata: metadata.ok_or("--metadata is required")?,
        render_output: render_output.ok_or("--render-output is required")?,
        mic_output: mic_output.ok_or("--mic-output is required")?,
        metrics: metrics.ok_or("--metrics is required")?,
        mic_channel: mic_channel.unwrap_or(0),
    };
    let paths = [
        &options.render_wav,
        &options.mic_wav,
        &options.metadata,
        &options.render_output,
        &options.mic_output,
        &options.metrics,
    ];
    for left in 0..paths.len() {
        for right in left + 1..paths.len() {
            if paths[left] == paths[right] {
                return Err("normalize-capture paths must be distinct".into());
            }
        }
    }
    Ok(options)
}

pub fn run_normalize(options: NormalizeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let timing: DuplexTiming = serde_json::from_reader(File::open(&options.metadata)?)?;
    let render_wave = read_device_wave(&options.render_wav)?;
    let mic_wave = read_device_wave(&options.mic_wav)?;
    let render_decoded = render_wave.decode_f32()?;
    let mic_decoded = mic_wave.decode_f32()?;
    let mut render_mapper =
        ChannelMapper::render(render_decoded.channels, render_wave.format.channel_mask)?;
    let mut mic_mapper = ChannelMapper::capture(mic_decoded.channels, options.mic_channel)?;
    let render_mapped = map_audio(&render_decoded, &mut render_mapper)?;
    let mic_mapped = map_audio(&mic_decoded, &mut mic_mapper)?;

    let render = normalize_stream(render_mapped, &timing.render)?;
    let capture = normalize_stream(mic_mapped, &timing.capture)?;
    write_f32_wave(
        &options.render_output,
        render.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &render.samples,
    )?;
    write_f32_wave(
        &options.mic_output,
        capture.channels,
        DSP_SAMPLE_RATE_HZ as u32,
        &capture.samples,
    )?;

    let report = NormalizationReport {
        render: StreamReport::new(&timing.render, &render_decoded, &render),
        capture: StreamReport::new(&timing.capture, &mic_decoded, &capture),
    };
    create_parent_directory(&options.metrics)?;
    serde_json::to_writer_pretty(BufWriter::new(File::create(&options.metrics)?), &report)?;

    println!(
        "render: {}ch, {} blocks, drift {:.3} ppm -> {}",
        render.channels,
        render.metrics.output_blocks,
        render.metrics.measured_drift_ppm.unwrap_or(0.0),
        options.render_output.display()
    );
    println!(
        "capture: {}ch, {} blocks, drift {:.3} ppm -> {}",
        capture.channels,
        capture.metrics.output_blocks,
        capture.metrics.measured_drift_ppm.unwrap_or(0.0),
        options.mic_output.display()
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

struct MappedAudio {
    channels: usize,
    sample_rate_hz: u32,
    frames: usize,
    samples: Vec<f32>,
}

fn map_audio(
    decoded: &DecodedInterleavedAudio,
    mapper: &mut ChannelMapper,
) -> Result<MappedAudio, Box<dyn std::error::Error>> {
    let output_samples = decoded
        .frames
        .checked_mul(mapper.output_channels())
        .ok_or("mapped audio size overflow")?;
    let mut samples = vec![0.0; output_samples];
    let frames = mapper.map_interleaved(&decoded.samples, &mut samples)?;
    Ok(MappedAudio {
        channels: mapper.output_channels(),
        sample_rate_hz: decoded.sample_rate_hz,
        frames,
        samples,
    })
}

struct NormalizedAudio {
    channels: usize,
    samples: Vec<f32>,
    flush_padding_frames: u64,
    metrics: NormalizerMetrics,
}

fn normalize_stream(
    input: MappedAudio,
    timing: &StreamTiming,
) -> Result<NormalizedAudio, Box<dyn std::error::Error>> {
    if timing.metadata_overflow != 0 {
        return Err(format!(
            "{} metadata overflowed by {} packets",
            timing.endpoint.friendly_name, timing.metadata_overflow
        )
        .into());
    }
    validate_timing(&input, timing)?;
    let mut normalizer = AdaptiveNormalizer::new(
        input.sample_rate_hz,
        input.channels,
        DEFAULT_QUEUE_CAPACITY_FRAMES,
    )?;
    let target_blocks = target_output_blocks(input.sample_rate_hz, timing)?;
    let output_capacity = target_blocks
        .checked_mul(DSP_SAMPLES_PER_FRAME)
        .and_then(|frames| frames.checked_mul(input.channels))
        .ok_or("normalized output size overflow")?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(output_capacity)?;
    let mut block = NormalizedBlock::silence(input.channels)?;

    for packet in &timing.packets {
        let start_frame = usize::try_from(packet.output_frame_offset)?;
        let end_frame = start_frame
            .checked_add(packet.frame_count as usize)
            .ok_or("packet frame range overflow")?;
        let start_sample = start_frame * input.channels;
        let end_sample = end_frame * input.channels;
        normalizer.observe_clock(ClockObservation {
            device_position_frames: packet.device_position,
            qpc_position_100ns: packet.qpc_position_100ns,
            discontinuity: packet.data_discontinuity,
            timestamp_error: packet.timestamp_error,
        })?;
        normalizer.push_interleaved(&input.samples[start_sample..end_sample])?;
        process_available(&mut normalizer, &mut block, &mut samples, target_blocks)?;
    }

    let mut flush_padding_frames = 0u64;
    let silence = vec![0.0; DEFAULT_QUEUE_CAPACITY_FRAMES * input.channels];
    while completed_blocks(samples.len(), input.channels) < target_blocks {
        if normalizer.queue_fill_frames() < normalizer.input_frames_needed() {
            let missing_frames = normalizer.input_frames_needed() - normalizer.queue_fill_frames();
            normalizer.push_interleaved(&silence[..missing_frames * input.channels])?;
            flush_padding_frames += missing_frames as u64;
        }
        process_available(&mut normalizer, &mut block, &mut samples, target_blocks)?;
    }

    Ok(NormalizedAudio {
        channels: input.channels,
        samples,
        flush_padding_frames,
        metrics: normalizer.metrics().clone(),
    })
}

fn process_available(
    normalizer: &mut AdaptiveNormalizer,
    block: &mut NormalizedBlock,
    samples: &mut Vec<f32>,
    target_blocks: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    while normalizer.queue_fill_frames() >= normalizer.input_frames_needed()
        && completed_blocks(samples.len(), normalizer.channels()) < target_blocks
    {
        if !normalizer.process_next(block)? {
            break;
        }
        for frame in 0..DSP_SAMPLES_PER_FRAME {
            for channel in 0..normalizer.channels() {
                samples.push(block.samples[channel][frame]);
            }
        }
    }
    Ok(())
}

fn completed_blocks(sample_count: usize, channels: usize) -> usize {
    sample_count / channels / DSP_SAMPLES_PER_FRAME
}

fn validate_timing(
    input: &MappedAudio,
    timing: &StreamTiming,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut expected_offset = 0u64;
    for packet in &timing.packets {
        if packet.output_frame_offset != expected_offset {
            return Err(format!(
                "{} packet timeline jumps from frame {} to {}",
                timing.endpoint.friendly_name, expected_offset, packet.output_frame_offset
            )
            .into());
        }
        expected_offset = expected_offset
            .checked_add(packet.frame_count as u64)
            .ok_or("packet timeline overflow")?;
    }
    if expected_offset != input.frames as u64 {
        return Err(format!(
            "{} metadata describes {} frames but WAV contains {}",
            timing.endpoint.friendly_name, expected_offset, input.frames
        )
        .into());
    }
    Ok(())
}

fn target_output_blocks(
    input_rate_hz: u32,
    timing: &StreamTiming,
) -> Result<usize, Box<dyn std::error::Error>> {
    let first = timing.packets.first().ok_or("packet timeline is empty")?;
    let last = timing.packets.last().ok_or("packet timeline is empty")?;
    let packet_span = last
        .qpc_position_100ns
        .checked_sub(first.qpc_position_100ns)
        .ok_or("packet QPC timeline moved backward")?;
    let final_packet_duration = last.frame_count as f64 * 10_000_000.0 / input_rate_hz as f64;
    let duration_100ns = packet_span as f64 + final_packet_duration;
    let output_frames =
        (duration_100ns * DSP_SAMPLE_RATE_HZ as f64 / 10_000_000.0).round() as usize;
    Ok(output_frames / DSP_SAMPLES_PER_FRAME)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplexTiming {
    render: StreamTiming,
    capture: StreamTiming,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamTiming {
    endpoint: EndpointTiming,
    metadata_overflow: u64,
    packets: Vec<PacketTiming>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndpointTiming {
    friendly_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PacketTiming {
    output_frame_offset: u64,
    frame_count: u32,
    device_position: u64,
    qpc_position_100ns: u64,
    data_discontinuity: bool,
    timestamp_error: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizationReport {
    render: StreamReport,
    capture: StreamReport,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamReport {
    endpoint: String,
    source_frames: usize,
    source_channels: usize,
    source_rate_hz: u32,
    non_finite_samples_replaced_during_decode: u64,
    normalized_frames: usize,
    normalized_channels: usize,
    flush_padding_frames: u64,
    normalizer: NormalizerMetrics,
}

impl StreamReport {
    fn new(
        timing: &StreamTiming,
        source: &DecodedInterleavedAudio,
        normalized: &NormalizedAudio,
    ) -> Self {
        Self {
            endpoint: timing.endpoint.friendly_name.clone(),
            source_frames: source.frames,
            source_channels: source.channels,
            source_rate_hz: source.sample_rate_hz,
            non_finite_samples_replaced_during_decode: source.non_finite_samples_replaced,
            normalized_frames: normalized.samples.len() / normalized.channels,
            normalized_channels: normalized.channels,
            flush_padding_frames: normalized.flush_padding_frames,
            normalizer: normalized.metrics.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_options_default_to_first_mic_channel() {
        let options = parse_normalize_options(
            [
                "--render-wav",
                "render.wav",
                "--mic-wav",
                "mic.wav",
                "--metadata",
                "metadata.json",
                "--render-output",
                "render-normalized.wav",
                "--mic-output",
                "mic-normalized.wav",
                "--metrics",
                "metrics.json",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();

        assert_eq!(options.mic_channel, 0);
    }
}
