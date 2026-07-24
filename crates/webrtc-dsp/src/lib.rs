use std::{
    ffi::c_void,
    path::Path,
    ptr::{self, NonNull},
};

use libloading::Library;

pub const ABI_VERSION: u32 = 2;
pub const SAMPLE_RATE_HZ: usize = 48_000;
pub const FRAME_SAMPLES: usize = 480;

const RESULT_OK: i32 = 0;

type AbiVersionFn = unsafe extern "C" fn() -> u32;
type CreateFn = unsafe extern "C" fn(*const RawConfig, *mut *mut c_void) -> i32;
type DestroyFn = unsafe extern "C" fn(*mut c_void);
type ProcessRenderFn = unsafe extern "C" fn(*mut c_void, *const f32, usize, usize) -> i32;
type ProcessCaptureFn = unsafe extern "C" fn(*mut c_void, *const f32, *mut f32, usize, i32) -> i32;
type DetectSpeechFn = unsafe extern "C" fn(*mut c_void, *const f32, usize, *mut u8) -> i32;
type ResetFn = unsafe extern "C" fn(*mut c_void) -> i32;
type GetMetricsFn = unsafe extern "C" fn(*const c_void, *mut RawMetrics) -> i32;
type Agc2CreateFn = unsafe extern "C" fn(*const RawAgc2Config, *mut *mut c_void) -> i32;
type Agc2DestroyFn = unsafe extern "C" fn(*mut c_void);
type Agc2ProcessFn = unsafe extern "C" fn(*mut c_void, *const f32, *mut f32, usize) -> i32;
type Agc2ResetFn = unsafe extern "C" fn(*mut c_void) -> i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AecPreset {
    Natural,
    Balanced,
    Strong,
}

impl AecPreset {
    fn as_raw(self) -> i32 {
        match self {
            Self::Natural => 0,
            Self::Balanced => 1,
            Self::Strong => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DspConfig {
    pub render_channels: usize,
    pub aec_preset: AecPreset,
    pub high_pass_filter_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DspMetrics {
    pub stream_delay_ms: i32,
    pub estimated_delay_ms: Option<i32>,
    pub echo_return_loss_db: Option<f32>,
    pub echo_return_loss_enhancement_db: Option<f32>,
    pub residual_echo_likelihood: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
pub struct Agc2Config {
    pub adaptive_digital_enabled: bool,
    pub fixed_gain_db: f32,
    pub headroom_db: f32,
    pub max_gain_db: f32,
    pub initial_gain_db: f32,
    pub max_gain_change_db_per_second: f32,
    pub max_output_noise_level_dbfs: f32,
}

impl Default for Agc2Config {
    fn default() -> Self {
        Self {
            adaptive_digital_enabled: true,
            fixed_gain_db: 0.0,
            headroom_db: 5.0,
            max_gain_db: 6.0,
            initial_gain_db: 0.0,
            max_gain_change_db_per_second: 3.0,
            max_output_noise_level_dbfs: -55.0,
        }
    }
}

pub struct WebRtcDsp {
    handle: NonNull<c_void>,
    render_channels: usize,
    functions: Functions,
    _library: Library,
}

impl WebRtcDsp {
    pub fn load(path: &Path, config: DspConfig) -> Result<Self, DspError> {
        if !(1..=2).contains(&config.render_channels) {
            return Err(DspError::UnsupportedRenderChannels(config.render_channels));
        }

        // Loading a native library can run its initialization code. The caller
        // supplies the pinned xmic DLL path rather than an ambient search name.
        let library = unsafe { Library::new(path) }?;
        let functions = unsafe { Functions::load(&library)? };
        let actual_abi = unsafe { (functions.abi_version)() };
        if actual_abi != ABI_VERSION {
            return Err(DspError::AbiMismatch {
                expected: ABI_VERSION,
                actual: actual_abi,
            });
        }

        let raw_config = RawConfig {
            struct_size: size_of_u32::<RawConfig>()?,
            abi_version: ABI_VERSION,
            render_channels: u32::try_from(config.render_channels)
                .map_err(|_| DspError::SizeOverflow)?,
            aec_preset: config.aec_preset.as_raw(),
            high_pass_filter_enabled: u8::from(config.high_pass_filter_enabled),
            reserved: [0; 11],
        };
        let mut handle = ptr::null_mut();
        let result = unsafe { (functions.create)(&raw_config, &mut handle) };
        check_result("create", result)?;
        let handle = NonNull::new(handle).ok_or(DspError::NullHandle)?;

        Ok(Self {
            handle,
            render_channels: config.render_channels,
            functions,
            _library: library,
        })
    }

    pub fn process_render(&mut self, interleaved: &[f32]) -> Result<(), DspError> {
        let expected = FRAME_SAMPLES
            .checked_mul(self.render_channels)
            .ok_or(DspError::SizeOverflow)?;
        if interleaved.len() != expected {
            return Err(DspError::InvalidRenderSamples {
                expected,
                actual: interleaved.len(),
            });
        }
        let result = unsafe {
            (self.functions.process_render)(
                self.handle.as_ptr(),
                interleaved.as_ptr(),
                FRAME_SAMPLES,
                self.render_channels,
            )
        };
        check_result("process_render", result)
    }

    pub fn process_capture(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stream_delay_ms: i32,
    ) -> Result<(), DspError> {
        if input.len() != FRAME_SAMPLES || output.len() != FRAME_SAMPLES {
            return Err(DspError::InvalidCaptureSamples {
                expected: FRAME_SAMPLES,
                input: input.len(),
                output: output.len(),
            });
        }
        if !(0..=500).contains(&stream_delay_ms) {
            return Err(DspError::InvalidStreamDelay(stream_delay_ms));
        }
        let result = unsafe {
            (self.functions.process_capture)(
                self.handle.as_ptr(),
                input.as_ptr(),
                output.as_mut_ptr(),
                FRAME_SAMPLES,
                stream_delay_ms,
            )
        };
        check_result("process_capture", result)
    }

    pub fn detect_speech(&mut self, input: &[f32]) -> Result<bool, DspError> {
        if input.len() != FRAME_SAMPLES {
            return Err(DspError::InvalidVadSamples {
                expected: FRAME_SAMPLES,
                actual: input.len(),
            });
        }
        let mut speech_detected = 0u8;
        let result = unsafe {
            (self.functions.detect_speech)(
                self.handle.as_ptr(),
                input.as_ptr(),
                FRAME_SAMPLES,
                &mut speech_detected,
            )
        };
        check_result("detect_speech", result)?;
        Ok(speech_detected != 0)
    }

    pub fn reset_echo_path(&mut self) -> Result<(), DspError> {
        let result = unsafe { (self.functions.reset)(self.handle.as_ptr()) };
        check_result("reset_echo_path", result)
    }

    pub fn metrics(&self) -> Result<DspMetrics, DspError> {
        let mut raw = RawMetrics {
            struct_size: size_of_u32::<RawMetrics>()?,
            abi_version: ABI_VERSION,
            stream_delay_ms: 0,
            estimated_delay_ms: 0,
            echo_return_loss_db: f32::NAN,
            echo_return_loss_enhancement_db: f32::NAN,
            delay_estimate_available: 0,
            echo_return_loss_available: 0,
            echo_return_loss_enhancement_available: 0,
            residual_echo_likelihood_available: 0,
            residual_echo_likelihood: f32::NAN,
            reserved: [0; 8],
        };
        let result = unsafe { (self.functions.get_metrics)(self.handle.as_ptr(), &mut raw) };
        check_result("get_metrics", result)?;
        Ok(DspMetrics {
            stream_delay_ms: raw.stream_delay_ms,
            estimated_delay_ms: (raw.delay_estimate_available != 0)
                .then_some(raw.estimated_delay_ms),
            echo_return_loss_db: (raw.echo_return_loss_available != 0)
                .then_some(raw.echo_return_loss_db),
            echo_return_loss_enhancement_db: (raw.echo_return_loss_enhancement_available != 0)
                .then_some(raw.echo_return_loss_enhancement_db),
            residual_echo_likelihood: (raw.residual_echo_likelihood_available != 0)
                .then_some(raw.residual_echo_likelihood),
        })
    }
}

impl Drop for WebRtcDsp {
    fn drop(&mut self) {
        unsafe { (self.functions.destroy)(self.handle.as_ptr()) };
    }
}

pub struct WebRtcAgc2 {
    handle: NonNull<c_void>,
    functions: Agc2Functions,
    _library: Library,
}

impl WebRtcAgc2 {
    pub fn load(path: &Path, config: Agc2Config) -> Result<Self, DspError> {
        validate_agc2_config(config)?;
        let library = unsafe { Library::new(path) }?;
        let functions = unsafe { Agc2Functions::load(&library)? };
        let actual_abi = unsafe { (functions.abi_version)() };
        if actual_abi != ABI_VERSION {
            return Err(DspError::AbiMismatch {
                expected: ABI_VERSION,
                actual: actual_abi,
            });
        }

        let raw_config = RawAgc2Config {
            struct_size: size_of_u32::<RawAgc2Config>()?,
            abi_version: ABI_VERSION,
            adaptive_digital_enabled: u8::from(config.adaptive_digital_enabled),
            reserved_flags: [0; 3],
            fixed_gain_db: config.fixed_gain_db,
            headroom_db: config.headroom_db,
            max_gain_db: config.max_gain_db,
            initial_gain_db: config.initial_gain_db,
            max_gain_change_db_per_second: config.max_gain_change_db_per_second,
            max_output_noise_level_dbfs: config.max_output_noise_level_dbfs,
            reserved: [0; 8],
        };
        let mut handle = ptr::null_mut();
        let result = unsafe { (functions.create)(&raw_config, &mut handle) };
        check_result("agc2_create", result)?;
        let handle = NonNull::new(handle).ok_or(DspError::NullHandle)?;
        Ok(Self {
            handle,
            functions,
            _library: library,
        })
    }

    pub fn process_capture(&mut self, input: &[f32], output: &mut [f32]) -> Result<(), DspError> {
        if input.len() != FRAME_SAMPLES || output.len() != FRAME_SAMPLES {
            return Err(DspError::InvalidCaptureSamples {
                expected: FRAME_SAMPLES,
                input: input.len(),
                output: output.len(),
            });
        }
        let result = unsafe {
            (self.functions.process)(
                self.handle.as_ptr(),
                input.as_ptr(),
                output.as_mut_ptr(),
                FRAME_SAMPLES,
            )
        };
        check_result("agc2_process_capture", result)
    }

    pub fn reset(&mut self) -> Result<(), DspError> {
        let result = unsafe { (self.functions.reset)(self.handle.as_ptr()) };
        check_result("agc2_reset", result)
    }
}

impl Drop for WebRtcAgc2 {
    fn drop(&mut self) {
        unsafe { (self.functions.destroy)(self.handle.as_ptr()) };
    }
}

#[derive(Clone, Copy)]
struct Functions {
    abi_version: AbiVersionFn,
    create: CreateFn,
    destroy: DestroyFn,
    process_render: ProcessRenderFn,
    process_capture: ProcessCaptureFn,
    detect_speech: DetectSpeechFn,
    reset: ResetFn,
    get_metrics: GetMetricsFn,
}

impl Functions {
    unsafe fn load(library: &Library) -> Result<Self, libloading::Error> {
        Ok(Self {
            abi_version: *library.get(b"xmic_webrtc_dsp_abi_version\0")?,
            create: *library.get(b"xmic_webrtc_dsp_create\0")?,
            destroy: *library.get(b"xmic_webrtc_dsp_destroy\0")?,
            process_render: *library.get(b"xmic_webrtc_dsp_process_render\0")?,
            process_capture: *library.get(b"xmic_webrtc_dsp_process_aec_capture\0")?,
            detect_speech: *library.get(b"xmic_webrtc_dsp_detect_speech\0")?,
            reset: *library.get(b"xmic_webrtc_dsp_reset_echo_path\0")?,
            get_metrics: *library.get(b"xmic_webrtc_dsp_get_metrics\0")?,
        })
    }
}

#[derive(Clone, Copy)]
struct Agc2Functions {
    abi_version: AbiVersionFn,
    create: Agc2CreateFn,
    destroy: Agc2DestroyFn,
    process: Agc2ProcessFn,
    reset: Agc2ResetFn,
}

impl Agc2Functions {
    unsafe fn load(library: &Library) -> Result<Self, libloading::Error> {
        Ok(Self {
            abi_version: *library.get(b"xmic_webrtc_dsp_abi_version\0")?,
            create: *library.get(b"xmic_webrtc_agc2_create\0")?,
            destroy: *library.get(b"xmic_webrtc_agc2_destroy\0")?,
            process: *library.get(b"xmic_webrtc_agc2_process_capture\0")?,
            reset: *library.get(b"xmic_webrtc_agc2_reset\0")?,
        })
    }
}

#[repr(C)]
struct RawConfig {
    struct_size: u32,
    abi_version: u32,
    render_channels: u32,
    aec_preset: i32,
    high_pass_filter_enabled: u8,
    reserved: [u8; 11],
}

#[repr(C)]
struct RawMetrics {
    struct_size: u32,
    abi_version: u32,
    stream_delay_ms: i32,
    estimated_delay_ms: i32,
    echo_return_loss_db: f32,
    echo_return_loss_enhancement_db: f32,
    delay_estimate_available: u8,
    echo_return_loss_available: u8,
    echo_return_loss_enhancement_available: u8,
    residual_echo_likelihood_available: u8,
    residual_echo_likelihood: f32,
    reserved: [u8; 8],
}

#[repr(C)]
struct RawAgc2Config {
    struct_size: u32,
    abi_version: u32,
    adaptive_digital_enabled: u8,
    reserved_flags: [u8; 3],
    fixed_gain_db: f32,
    headroom_db: f32,
    max_gain_db: f32,
    initial_gain_db: f32,
    max_gain_change_db_per_second: f32,
    max_output_noise_level_dbfs: f32,
    reserved: [u8; 8],
}

fn validate_agc2_config(config: Agc2Config) -> Result<(), DspError> {
    let valid = config.fixed_gain_db.is_finite()
        && (0.0..=30.0).contains(&config.fixed_gain_db)
        && config.headroom_db.is_finite()
        && (0.0..=30.0).contains(&config.headroom_db)
        && config.max_gain_db.is_finite()
        && (0.0..=50.0).contains(&config.max_gain_db)
        && config.max_gain_db > 0.0
        && config.initial_gain_db.is_finite()
        && (0.0..=config.max_gain_db).contains(&config.initial_gain_db)
        && config.max_gain_change_db_per_second.is_finite()
        && (0.0..=20.0).contains(&config.max_gain_change_db_per_second)
        && config.max_gain_change_db_per_second > 0.0
        && config.max_output_noise_level_dbfs.is_finite()
        && (-90.0..=0.0).contains(&config.max_output_noise_level_dbfs);
    if valid {
        Ok(())
    } else {
        Err(DspError::InvalidAgc2Config)
    }
}

fn size_of_u32<T>() -> Result<u32, DspError> {
    u32::try_from(std::mem::size_of::<T>()).map_err(|_| DspError::SizeOverflow)
}

fn check_result(operation: &'static str, result: i32) -> Result<(), DspError> {
    if result == RESULT_OK {
        Ok(())
    } else {
        Err(DspError::Native { operation, result })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("load WebRTC DSP library: {0}")]
    Library(#[from] libloading::Error),
    #[error("WebRTC DSP ABI mismatch: expected {expected}, got {actual}")]
    AbiMismatch { expected: u32, actual: u32 },
    #[error("WebRTC DSP returned a null handle")]
    NullHandle,
    #[error("render channel count {0} is unsupported")]
    UnsupportedRenderChannels(usize),
    #[error("render block has {actual} samples; expected {expected}")]
    InvalidRenderSamples { expected: usize, actual: usize },
    #[error("capture block has input/output lengths {input}/{output}; expected {expected}")]
    InvalidCaptureSamples {
        expected: usize,
        input: usize,
        output: usize,
    },
    #[error("VAD block has {actual} samples; expected {expected}")]
    InvalidVadSamples { expected: usize, actual: usize },
    #[error("stream delay {0} ms is outside 0..=500")]
    InvalidStreamDelay(i32),
    #[error("invalid AGC2 configuration")]
    InvalidAgc2Config,
    #[error("WebRTC DSP {operation} failed with native result {result}")]
    Native {
        operation: &'static str,
        result: i32,
    },
    #[error("size conversion overflow")]
    SizeOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_struct_sizes_match_c_abi() {
        assert_eq!(std::mem::size_of::<RawConfig>(), 28);
        assert_eq!(std::mem::size_of::<RawMetrics>(), 40);
        assert_eq!(std::mem::size_of::<RawAgc2Config>(), 44);
    }

    #[test]
    fn rejects_invalid_render_channel_count_before_loading() {
        let result = WebRtcDsp::load(
            Path::new("missing.dll"),
            DspConfig {
                render_channels: 3,
                aec_preset: AecPreset::Natural,
                high_pass_filter_enabled: true,
            },
        );

        assert!(matches!(
            result,
            Err(DspError::UnsupportedRenderChannels(3))
        ));
    }

    #[test]
    fn rejects_invalid_agc2_config_before_loading() {
        let result = WebRtcAgc2::load(
            Path::new("missing.dll"),
            Agc2Config {
                max_gain_db: 0.0,
                ..Agc2Config::default()
            },
        );
        assert!(matches!(result, Err(DspError::InvalidAgc2Config)));
    }

    #[test]
    fn bundled_vad_rejects_silence() {
        let native_library = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("artifacts")
            .join("webrtc")
            .join("win-x64")
            .join("xmic_webrtc_dsp.dll");
        if !native_library.is_file() {
            return;
        }
        let mut dsp = WebRtcDsp::load(
            &native_library,
            DspConfig {
                render_channels: 2,
                aec_preset: AecPreset::Strong,
                high_pass_filter_enabled: true,
            },
        )
        .unwrap();

        assert!(!dsp.detect_speech(&[0.0; FRAME_SAMPLES]).unwrap());
    }
}
