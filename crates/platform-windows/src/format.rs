use serde::Serialize;
use xmic_platform_api::PlatformError;

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
const WAVE_FORMAT_BASE_BYTES: usize = 18;
const WAVE_FORMAT_EXTENSIBLE_BYTES: usize = 40;
const MAX_WAVE_FORMAT_EXTENSION_BYTES: usize = 256;
const PCM_SUBFORMAT: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
const FLOAT_SUBFORMAT: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleEncoding {
    Pcm,
    Float,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFormatDescriptor {
    pub format_tag: u16,
    pub encoding: SampleEncoding,
    pub channels: u16,
    pub sample_rate_hz: u32,
    pub average_bytes_per_second: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub valid_bits_per_sample: u16,
    pub channel_mask: u32,
    pub extension_bytes: u16,
    #[serde(skip)]
    wave_format_bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct DecodedInterleavedAudio {
    pub channels: usize,
    pub sample_rate_hz: u32,
    pub frames: usize,
    pub non_finite_samples_replaced: u64,
    pub samples: Vec<f32>,
}

impl AudioFormatDescriptor {
    pub fn from_wave_format_bytes(mut wave_format_bytes: Vec<u8>) -> Result<Self, PlatformError> {
        if wave_format_bytes.len() < 16 {
            return Err(format_error("WAVEFORMATEX is shorter than 16 bytes"));
        }

        let format_tag = read_u16(&wave_format_bytes, 0)?;
        let channels = read_u16(&wave_format_bytes, 2)?;
        let sample_rate_hz = read_u32(&wave_format_bytes, 4)?;
        let average_bytes_per_second = read_u32(&wave_format_bytes, 8)?;
        let block_align = read_u16(&wave_format_bytes, 12)?;
        let bits_per_sample = read_u16(&wave_format_bytes, 14)?;
        let extension_bytes = if wave_format_bytes.len() >= WAVE_FORMAT_BASE_BYTES {
            read_u16(&wave_format_bytes, 16)?
        } else {
            0
        };
        if extension_bytes as usize > MAX_WAVE_FORMAT_EXTENSION_BYTES {
            return Err(format_error(format!(
                "format extension is too large: {extension_bytes} bytes"
            )));
        }
        let expected_length = if extension_bytes == 0 && wave_format_bytes.len() == 16 {
            16
        } else {
            WAVE_FORMAT_BASE_BYTES + extension_bytes as usize
        };
        if wave_format_bytes.len() < expected_length {
            return Err(format_error(format!(
                "format declares {extension_bytes} extension bytes but only {} bytes are available",
                wave_format_bytes.len()
            )));
        }
        wave_format_bytes.truncate(expected_length);

        let (encoding, valid_bits_per_sample, channel_mask) = match format_tag {
            WAVE_FORMAT_PCM => (SampleEncoding::Pcm, bits_per_sample, 0),
            WAVE_FORMAT_IEEE_FLOAT => (SampleEncoding::Float, bits_per_sample, 0),
            WAVE_FORMAT_EXTENSIBLE => {
                if extension_bytes < 22 || wave_format_bytes.len() < WAVE_FORMAT_EXTENSIBLE_BYTES {
                    return Err(format_error(
                        "WAVE_FORMAT_EXTENSIBLE requires a 22-byte extension",
                    ));
                }
                let valid_bits = read_u16(&wave_format_bytes, 18)?;
                let channel_mask = read_u32(&wave_format_bytes, 20)?;
                let subformat: [u8; 16] = wave_format_bytes[24..40]
                    .try_into()
                    .map_err(|_| format_error("invalid extensible subformat GUID"))?;
                let encoding = if subformat == PCM_SUBFORMAT {
                    SampleEncoding::Pcm
                } else if subformat == FLOAT_SUBFORMAT {
                    SampleEncoding::Float
                } else {
                    return Err(format_error(format!(
                        "unsupported extensible subformat GUID: {}",
                        bytes_to_hex(&subformat)
                    )));
                };
                (encoding, valid_bits, channel_mask)
            }
            other => {
                return Err(format_error(format!(
                    "unsupported wave format tag: 0x{other:04x}"
                )));
            }
        };

        validate_format(
            encoding,
            channels,
            sample_rate_hz,
            block_align,
            bits_per_sample,
            valid_bits_per_sample,
        )?;

        Ok(Self {
            format_tag,
            encoding,
            channels,
            sample_rate_hz,
            average_bytes_per_second,
            block_align,
            bits_per_sample,
            valid_bits_per_sample,
            channel_mask,
            extension_bytes,
            wave_format_bytes,
        })
    }

    pub fn wave_format_bytes(&self) -> &[u8] {
        &self.wave_format_bytes
    }

    pub fn decode_interleaved_f32(
        &self,
        data: &[u8],
    ) -> Result<DecodedInterleavedAudio, PlatformError> {
        let block_align = self.block_align as usize;
        if !data.len().is_multiple_of(block_align) {
            return Err(format_error(format!(
                "audio byte count {} is not aligned to {} bytes per frame",
                data.len(),
                block_align
            )));
        }
        let frames = data.len() / block_align;
        let channels = self.channels as usize;
        let sample_count = frames
            .checked_mul(channels)
            .ok_or_else(|| format_error("decoded sample count overflow"))?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(sample_count)
            .map_err(|error| format_error(format!("reserve decoded sample buffer: {error}")))?;
        samples.resize(sample_count, 0.0);
        let non_finite_samples_replaced = self.decode_interleaved_f32_into(data, &mut samples)?;

        Ok(DecodedInterleavedAudio {
            channels,
            sample_rate_hz: self.sample_rate_hz,
            frames,
            non_finite_samples_replaced,
            samples,
        })
    }

    pub fn decode_interleaved_f32_into(
        &self,
        data: &[u8],
        output: &mut [f32],
    ) -> Result<u64, PlatformError> {
        let block_align = self.block_align as usize;
        if !data.len().is_multiple_of(block_align) {
            return Err(format_error(format!(
                "audio byte count {} is not aligned to {} bytes per frame",
                data.len(),
                block_align
            )));
        }
        let frames = data.len() / block_align;
        let channels = self.channels as usize;
        let required = frames
            .checked_mul(channels)
            .ok_or_else(|| format_error("decoded sample count overflow"))?;
        if output.len() < required {
            return Err(format_error(format!(
                "decoded output needs {required} samples but only {} are available",
                output.len()
            )));
        }

        let sample_bytes = (self.bits_per_sample / 8) as usize;
        let mut non_finite_samples_replaced = 0u64;
        for (frame, output_frame) in data
            .chunks_exact(block_align)
            .zip(output[..required].chunks_exact_mut(channels))
        {
            for (channel, sample) in output_frame.iter_mut().enumerate() {
                let offset = channel * sample_bytes;
                let bytes = &frame[offset..offset + sample_bytes];
                *sample = match self.encoding {
                    SampleEncoding::Pcm => {
                        decode_pcm(bytes, self.bits_per_sample, self.valid_bits_per_sample)?
                    }
                    SampleEncoding::Float => decode_float(bytes, self.bits_per_sample)?,
                };
                if !sample.is_finite() {
                    *sample = 0.0;
                    non_finite_samples_replaced += 1;
                }
            }
        }
        Ok(non_finite_samples_replaced)
    }
}

fn validate_format(
    encoding: SampleEncoding,
    channels: u16,
    sample_rate_hz: u32,
    block_align: u16,
    bits_per_sample: u16,
    valid_bits_per_sample: u16,
) -> Result<(), PlatformError> {
    if channels == 0 || sample_rate_hz == 0 || block_align == 0 {
        return Err(format_error(format!(
            "invalid format dimensions: channels={channels}, sampleRate={sample_rate_hz}, blockAlign={block_align}"
        )));
    }
    let supported_bits = match encoding {
        SampleEncoding::Pcm => matches!(bits_per_sample, 8 | 16 | 24 | 32),
        SampleEncoding::Float => matches!(bits_per_sample, 32 | 64),
    };
    if !supported_bits {
        return Err(format_error(format!(
            "unsupported {encoding:?} container width: {bits_per_sample} bits"
        )));
    }
    if valid_bits_per_sample == 0 || valid_bits_per_sample > bits_per_sample {
        return Err(format_error(format!(
            "invalid valid-bit count: {valid_bits_per_sample} in a {bits_per_sample}-bit container"
        )));
    }
    if encoding == SampleEncoding::Float && valid_bits_per_sample != bits_per_sample {
        return Err(format_error(
            "floating-point valid bits must equal the container width",
        ));
    }
    let minimum_block_align = channels as u32 * u32::from(bits_per_sample / 8);
    if u32::from(block_align) < minimum_block_align {
        return Err(format_error(format!(
            "block alignment {block_align} is smaller than {minimum_block_align} bytes"
        )));
    }
    Ok(())
}

fn decode_pcm(bytes: &[u8], container_bits: u16, valid_bits: u16) -> Result<f32, PlatformError> {
    if container_bits == 8 {
        let unsigned = bytes[0] as i32 - 128;
        return Ok(unsigned as f32 / 128.0);
    }

    let signed = match container_bits {
        16 => i16::from_le_bytes([bytes[0], bytes[1]]) as i64,
        24 => {
            let packed =
                i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
            ((packed << 8) >> 8) as i64
        }
        32 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64,
        _ => return Err(format_error("unsupported PCM container width")),
    };
    let shift = container_bits - valid_bits;
    let aligned = signed >> shift;
    let scale = (1u64 << (valid_bits - 1)) as f64;
    Ok((aligned as f64 / scale) as f32)
}

fn decode_float(bytes: &[u8], bits_per_sample: u16) -> Result<f32, PlatformError> {
    match bits_per_sample {
        32 => Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        64 => Ok(f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]) as f32),
        _ => Err(format_error("unsupported float container width")),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PlatformError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| format_error("truncated 16-bit wave format field"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PlatformError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format_error("truncated 32-bit wave format field"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

fn format_error(message: impl Into<String>) -> PlatformError {
    PlatformError::Operation(format!("decode audio format: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_format(bits: u16, channels: u16) -> AudioFormatDescriptor {
        let bytes_per_sample = bits / 8;
        let block_align = channels * bytes_per_sample;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * u32::from(block_align)).to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        AudioFormatDescriptor::from_wave_format_bytes(bytes).unwrap()
    }

    #[test]
    fn decodes_pcm16_extremes() {
        let format = pcm_format(16, 1);
        let decoded = format
            .decode_interleaved_f32(&[0x00, 0x80, 0x00, 0x00, 0xff, 0x7f])
            .unwrap();

        assert_eq!(decoded.frames, 3);
        assert_eq!(decoded.samples, [-1.0, 0.0, 32767.0 / 32768.0]);
    }

    #[test]
    fn decodes_signed_pcm24() {
        let format = pcm_format(24, 1);
        let decoded = format
            .decode_interleaved_f32(&[0x00, 0x00, 0x80, 0xff, 0xff, 0x7f])
            .unwrap();

        assert_eq!(decoded.samples, [-1.0, 8388607.0 / 8388608.0]);
    }

    #[test]
    fn parses_extensible_float_and_replaces_nan() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&384_000u32.to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&22u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&FLOAT_SUBFORMAT);
        let format = AudioFormatDescriptor::from_wave_format_bytes(bytes).unwrap();
        let mut data = Vec::new();
        data.extend_from_slice(&0.25f32.to_le_bytes());
        data.extend_from_slice(&f32::NAN.to_le_bytes());
        let decoded = format.decode_interleaved_f32(&data).unwrap();

        assert_eq!(format.encoding, SampleEncoding::Float);
        assert_eq!(format.channel_mask, 3);
        assert_eq!(decoded.samples, [0.25, 0.0]);
        assert_eq!(decoded.non_finite_samples_replaced, 1);
    }

    #[test]
    fn decodes_into_reused_buffer() {
        let format = pcm_format(16, 1);
        let mut output = [9.0; 4];
        let replaced = format
            .decode_interleaved_f32_into(&[0x00, 0x80, 0xff, 0x7f], &mut output)
            .unwrap();

        assert_eq!(replaced, 0);
        assert_eq!(output, [-1.0, 32767.0 / 32768.0, 9.0, 9.0]);
    }
}
