use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    mem::size_of,
    path::Path,
};

use xmic_platform_windows::{AudioFormatDescriptor, CapturedAudioStream, DecodedInterleavedAudio};

pub struct DeviceWave {
    pub format: AudioFormatDescriptor,
    pub data: Vec<u8>,
}

impl DeviceWave {
    pub fn decode_f32(&self) -> Result<DecodedInterleavedAudio, Box<dyn std::error::Error>> {
        Ok(self.format.decode_interleaved_f32(&self.data)?)
    }
}

pub fn read_device_wave(path: &Path) -> Result<DeviceWave, Box<dyn std::error::Error>> {
    let file = fs::read(path)?;
    if file.len() < 12 || &file[0..4] != b"RIFF" || &file[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()).into());
    }
    let declared_size = read_u32(&file, 4)? as usize + 8;
    if declared_size > file.len() {
        return Err(format!("{} has a truncated RIFF payload", path.display()).into());
    }

    let mut format = None;
    let mut data = None;
    let mut offset = 12usize;
    while offset + 8 <= declared_size {
        let id = &file[offset..offset + 4];
        let chunk_size = read_u32(&file, offset + 4)? as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start
            .checked_add(chunk_size)
            .ok_or("WAV chunk offset overflow")?;
        if payload_end > declared_size {
            return Err(format!("{} contains a truncated WAV chunk", path.display()).into());
        }
        match id {
            b"fmt " if format.is_none() => {
                format = Some(AudioFormatDescriptor::from_wave_format_bytes(
                    file[payload_start..payload_end].to_vec(),
                )?);
            }
            b"data" if data.is_none() => {
                data = Some(file[payload_start..payload_end].to_vec());
            }
            _ => {}
        }
        offset = payload_end + chunk_size % 2;
    }

    Ok(DeviceWave {
        format: format.ok_or("WAV file has no fmt chunk")?,
        data: data.ok_or("WAV file has no data chunk")?,
    })
}

pub fn write_captured_wave(
    path: &Path,
    stream: &CapturedAudioStream,
) -> Result<(), Box<dyn std::error::Error>> {
    let format_bytes = stream.format.wave_format_bytes();
    let format_bytes = if stream.format.format_tag == 1 && stream.format.extension_bytes == 0 {
        &format_bytes[..16]
    } else {
        format_bytes
    };
    write_wave_bytes(
        path,
        format_bytes,
        stream.format.format_tag != 1,
        stream.captured_frames,
        &stream.data,
    )
}

pub fn write_f32_wave(
    path: &Path,
    channels: usize,
    sample_rate_hz: u32,
    samples: &[f32],
) -> Result<(), Box<dyn std::error::Error>> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        return Err("f32 WAV samples are not channel-aligned".into());
    }
    let channels = u16::try_from(channels)?;
    let block_align = channels
        .checked_mul(4)
        .ok_or("WAV block alignment overflow")?;
    let byte_rate = sample_rate_hz
        .checked_mul(u32::from(block_align))
        .ok_or("WAV byte rate overflow")?;
    let mut format = Vec::with_capacity(18);
    format.extend_from_slice(&3u16.to_le_bytes());
    format.extend_from_slice(&channels.to_le_bytes());
    format.extend_from_slice(&sample_rate_hz.to_le_bytes());
    format.extend_from_slice(&byte_rate.to_le_bytes());
    format.extend_from_slice(&block_align.to_le_bytes());
    format.extend_from_slice(&32u16.to_le_bytes());
    format.extend_from_slice(&0u16.to_le_bytes());
    let frames = samples.len() / channels as usize;
    let data_size = samples
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or("f32 WAV data size overflow")?;
    let data_size_u32 = u32::try_from(data_size).map_err(|_| "f32 WAV data exceeds 4 GiB")?;
    let data_chunk_size = riff_chunk_size(data_size)?;
    let riff_size = 4u64
        .checked_add(riff_chunk_size(format.len())?)
        .and_then(|value| value.checked_add(12))
        .and_then(|value| value.checked_add(data_chunk_size))
        .ok_or("WAV RIFF size overflow")?;
    let riff_size = u32::try_from(riff_size).map_err(|_| "WAV file exceeds RIFF size limit")?;
    let frame_count = u32::try_from(frames).map_err(|_| "WAV frame count exceeds 32-bit range")?;

    create_parent_directory(path)?;
    let mut writer = BufWriter::new(File::create(path)?);
    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    write_riff_chunk(&mut writer, b"fmt ", &format)?;
    write_riff_chunk(&mut writer, b"fact", &frame_count.to_le_bytes())?;
    writer.write_all(b"data")?;
    writer.write_all(&data_size_u32.to_le_bytes())?;
    for sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }
    writer.flush()?;
    Ok(())
}

fn write_wave_bytes(
    path: &Path,
    format: &[u8],
    include_fact: bool,
    frames: u64,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let format_chunk_bytes = riff_chunk_size(format.len())?;
    let fact_chunk_bytes = if include_fact { 12u64 } else { 0 };
    let data_chunk_bytes = riff_chunk_size(data.len())?;
    let riff_size = 4u64
        .checked_add(format_chunk_bytes)
        .and_then(|value| value.checked_add(fact_chunk_bytes))
        .and_then(|value| value.checked_add(data_chunk_bytes))
        .ok_or("WAV RIFF size overflow")?;
    let riff_size = u32::try_from(riff_size).map_err(|_| "WAV file exceeds RIFF size limit")?;
    let frame_count = u32::try_from(frames).map_err(|_| "WAV frame count exceeds 32-bit range")?;

    create_parent_directory(path)?;
    let mut writer = BufWriter::new(File::create(path)?);
    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    write_riff_chunk(&mut writer, b"fmt ", format)?;
    if include_fact {
        write_riff_chunk(&mut writer, b"fact", &frame_count.to_le_bytes())?;
    }
    write_riff_chunk(&mut writer, b"data", data)?;
    writer.flush()?;
    Ok(())
}

fn riff_chunk_size(data_length: usize) -> Result<u64, Box<dyn std::error::Error>> {
    let data_length = u64::try_from(data_length)?;
    Ok(8 + data_length + data_length % 2)
}

fn write_riff_chunk(
    writer: &mut impl Write,
    id: &[u8; 4],
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let data_length = u32::try_from(data.len()).map_err(|_| "RIFF chunk exceeds 4 GiB")?;
    writer.write_all(id)?;
    writer.write_all(&data_length.to_le_bytes())?;
    writer.write_all(data)?;
    if !data.len().is_multiple_of(2) {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Box<dyn std::error::Error>> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or("truncated 32-bit WAV field")?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

pub fn create_parent_directory(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riff_chunk_size_includes_padding() {
        assert_eq!(riff_chunk_size(4).unwrap(), 12);
        assert_eq!(riff_chunk_size(5).unwrap(), 14);
    }
}
