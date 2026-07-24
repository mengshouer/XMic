#ifndef XMIC_WEBRTC_DSP_H_
#define XMIC_WEBRTC_DSP_H_

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#if defined(XMIC_WEBRTC_DSP_IMPLEMENTATION)
#define XMIC_WEBRTC_DSP_API __declspec(dllexport)
#else
#define XMIC_WEBRTC_DSP_API __declspec(dllimport)
#endif
#else
#define XMIC_WEBRTC_DSP_API __attribute__((visibility("default")))
#endif

#if defined(__cplusplus)
extern "C" {
#endif

#define XMIC_WEBRTC_DSP_ABI_VERSION 2u
#define XMIC_WEBRTC_DSP_SAMPLE_RATE_HZ 48000u
#define XMIC_WEBRTC_DSP_FRAME_SAMPLES 480u

typedef struct XmicWebRtcDsp XmicWebRtcDsp;
typedef struct XmicWebRtcAgc2 XmicWebRtcAgc2;

typedef enum XmicDspResult {
  XMIC_DSP_OK = 0,
  XMIC_DSP_INVALID_ARGUMENT = 1,
  XMIC_DSP_UNSUPPORTED_ABI = 2,
  XMIC_DSP_WEBRTC_ERROR = 3,
  XMIC_DSP_OUT_OF_MEMORY = 4,
} XmicDspResult;

typedef enum XmicAecPreset {
  XMIC_AEC_NATURAL = 0,
  XMIC_AEC_BALANCED = 1,
  XMIC_AEC_STRONG = 2,
} XmicAecPreset;

typedef struct XmicDspConfig {
  uint32_t struct_size;
  uint32_t abi_version;
  uint32_t render_channels;
  XmicAecPreset aec_preset;
  uint8_t high_pass_filter_enabled;
  uint8_t reserved[11];
} XmicDspConfig;

typedef struct XmicDspMetrics {
  uint32_t struct_size;
  uint32_t abi_version;
  int32_t stream_delay_ms;
  int32_t estimated_delay_ms;
  float echo_return_loss_db;
  float echo_return_loss_enhancement_db;
  uint8_t delay_estimate_available;
  uint8_t echo_return_loss_available;
  uint8_t echo_return_loss_enhancement_available;
  uint8_t residual_echo_likelihood_available;
  float residual_echo_likelihood;
  uint8_t reserved[8];
} XmicDspMetrics;

typedef struct XmicAgc2Config {
  uint32_t struct_size;
  uint32_t abi_version;
  uint8_t adaptive_digital_enabled;
  uint8_t reserved_flags[3];
  float fixed_gain_db;
  float headroom_db;
  float max_gain_db;
  float initial_gain_db;
  float max_gain_change_db_per_second;
  float max_output_noise_level_dbfs;
  uint8_t reserved[8];
} XmicAgc2Config;

XMIC_WEBRTC_DSP_API uint32_t xmic_webrtc_dsp_abi_version(void);
XMIC_WEBRTC_DSP_API XmicDspResult
xmic_webrtc_dsp_create(const XmicDspConfig *config, XmicWebRtcDsp **out_dsp);
XMIC_WEBRTC_DSP_API void xmic_webrtc_dsp_destroy(XmicWebRtcDsp *dsp);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_dsp_process_render(
    XmicWebRtcDsp *dsp, const float *interleaved_samples, size_t frames,
    size_t channels);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_dsp_process_aec_capture(
    XmicWebRtcDsp *dsp, const float *input, float *output, size_t frames,
    int32_t stream_delay_ms);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_dsp_detect_speech(
    XmicWebRtcDsp *dsp, const float *input, size_t frames,
    uint8_t *speech_detected);
XMIC_WEBRTC_DSP_API XmicDspResult
xmic_webrtc_dsp_reset_echo_path(XmicWebRtcDsp *dsp);
XMIC_WEBRTC_DSP_API XmicDspResult
xmic_webrtc_dsp_get_metrics(const XmicWebRtcDsp *dsp, XmicDspMetrics *metrics);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_agc2_create(
    const XmicAgc2Config *config, XmicWebRtcAgc2 **out_agc2);
XMIC_WEBRTC_DSP_API void xmic_webrtc_agc2_destroy(XmicWebRtcAgc2 *agc2);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_agc2_process_capture(
    XmicWebRtcAgc2 *agc2, const float *input, float *output, size_t frames);
XMIC_WEBRTC_DSP_API XmicDspResult xmic_webrtc_agc2_reset(XmicWebRtcAgc2 *agc2);

#if defined(__cplusplus)
}
#endif

#undef XMIC_WEBRTC_DSP_API

#endif // XMIC_WEBRTC_DSP_H_
