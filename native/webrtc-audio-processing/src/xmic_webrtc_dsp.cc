#include "xmic_webrtc_dsp/include/xmic_webrtc_dsp.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <memory>
#include <new>
#include <optional>

#include "api/audio/audio_processing.h"
#include "api/audio/builtin_audio_processing_builder.h"
#include "api/audio/echo_canceller3_config.h"
#include "api/audio/echo_detector_creator.h"
#include "api/environment/environment_factory.h"
#include "api/scoped_refptr.h"
#include "common_audio/vad/include/webrtc_vad.h"

namespace {

constexpr int kSampleRateHz = XMIC_WEBRTC_DSP_SAMPLE_RATE_HZ;
constexpr size_t kFrameSamples = XMIC_WEBRTC_DSP_FRAME_SAMPLES;
constexpr size_t kMaximumRenderChannels = 2;
constexpr int kSpeechVadMode = 3;

float SanitizeSample(float sample) {
  return std::isfinite(sample) ? std::clamp(sample, -1.0f, 1.0f) : 0.0f;
}

bool IsValidPreset(XmicAecPreset preset) {
  return preset == XMIC_AEC_NATURAL || preset == XMIC_AEC_BALANCED ||
         preset == XMIC_AEC_STRONG;
}

bool IsValidAgc2Config(const XmicAgc2Config &config) {
  return config.adaptive_digital_enabled <= 1 &&
         std::isfinite(config.fixed_gain_db) && config.fixed_gain_db >= 0.0f &&
         config.fixed_gain_db <= 30.0f && std::isfinite(config.headroom_db) &&
         config.headroom_db >= 0.0f && config.headroom_db <= 30.0f &&
         std::isfinite(config.max_gain_db) && config.max_gain_db > 0.0f &&
         config.max_gain_db <= 50.0f && std::isfinite(config.initial_gain_db) &&
         config.initial_gain_db >= 0.0f &&
         config.initial_gain_db <= config.max_gain_db &&
         std::isfinite(config.max_gain_change_db_per_second) &&
         config.max_gain_change_db_per_second > 0.0f &&
         config.max_gain_change_db_per_second <= 20.0f &&
         std::isfinite(config.max_output_noise_level_dbfs) &&
         config.max_output_noise_level_dbfs >= -90.0f &&
         config.max_output_noise_level_dbfs <= 0.0f;
}

webrtc::EchoCanceller3Config CreateAec3Config(XmicAecPreset preset) {
  webrtc::EchoCanceller3Config config;
  if (preset == XMIC_AEC_NATURAL) {
    config.suppressor.normal_tuning.mask_lf.enr_transparent = 0.4f;
    config.suppressor.normal_tuning.mask_lf.enr_suppress = 0.5f;
    config.suppressor.normal_tuning.mask_hf.enr_transparent = 0.3f;
    config.suppressor.normal_tuning.mask_hf.enr_suppress = 0.4f;
    config.suppressor.nearend_tuning.mask_lf.enr_transparent = 1.29f;
    config.suppressor.nearend_tuning.mask_lf.enr_suppress = 1.3f;
    config.suppressor.nearend_tuning.mask_hf.enr_transparent = 1.09f;
    config.suppressor.nearend_tuning.mask_hf.enr_suppress = 1.1f;
  } else if (preset == XMIC_AEC_STRONG) {
    // Keep the near-end tuning at the upstream defaults so double-talk can
    // still recover naturally. These normal-state values make the suppressor
    // react earlier and retain more attenuation while only far-end audio is
    // present.
    config.suppressor.normal_tuning.mask_lf.enr_transparent = 0.15f;
    config.suppressor.normal_tuning.mask_lf.enr_suppress = 0.25f;
    config.suppressor.normal_tuning.mask_hf.enr_transparent = 0.03f;
    config.suppressor.normal_tuning.mask_hf.enr_suppress = 0.08f;
    config.suppressor.normal_tuning.max_inc_factor = 1.5f;
    config.suppressor.normal_tuning.max_dec_factor_lf = 0.15f;

    // The default minimum audible-echo floor limits how far suppression can
    // reduce low-level residuals. Strong permits another 6 dB of headroom.
    config.echo_audibility.low_render_limit = 128.0f;
    config.echo_audibility.normal_render_limit = 32.0f;
    config.echo_audibility.audibility_threshold_lf = 4.0f;
    config.echo_audibility.audibility_threshold_mf = 4.0f;
    config.echo_audibility.audibility_threshold_hf = 4.0f;

    // Global Windows routing commonly combines render and capture endpoints
    // backed by independent device clocks.
    config.echo_removal_control.has_clock_drift = true;
    config.ep_strength.default_gain = 1.5f;
    config.suppressor.conservative_hf_suppression = true;
    config.render_levels.active_render_limit = 30.0f;
  }
  return config;
}

XmicDspResult ToResult(int result) {
  return result == webrtc::AudioProcessing::kNoError ? XMIC_DSP_OK
                                                     : XMIC_DSP_WEBRTC_ERROR;
}

} // namespace

struct XmicWebRtcDsp {
  webrtc::scoped_refptr<webrtc::AudioProcessing> audio_processing;
  VadInst *speech_vad = nullptr;
  size_t render_channels = 0;
  std::array<std::array<float, kFrameSamples>, kMaximumRenderChannels>
      render_buffers{};
  std::array<float, kFrameSamples> capture_buffer{};
  std::array<int16_t, kFrameSamples> vad_buffer{};
  std::atomic<int32_t> stream_delay_ms{0};

  ~XmicWebRtcDsp() {
    if (speech_vad != nullptr) {
      WebRtcVad_Free(speech_vad);
    }
  }
};

struct XmicWebRtcAgc2 {
  webrtc::scoped_refptr<webrtc::AudioProcessing> audio_processing;
  std::array<float, kFrameSamples> capture_buffer{};
};

uint32_t xmic_webrtc_dsp_abi_version(void) {
  return XMIC_WEBRTC_DSP_ABI_VERSION;
}

XmicDspResult xmic_webrtc_dsp_create(const XmicDspConfig *config,
                                     XmicWebRtcDsp **out_dsp) {
  if (out_dsp == nullptr) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  *out_dsp = nullptr;
  if (config == nullptr || config->struct_size != sizeof(XmicDspConfig)) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  if (config->abi_version != XMIC_WEBRTC_DSP_ABI_VERSION) {
    return XMIC_DSP_UNSUPPORTED_ABI;
  }
  if (config->render_channels < 1 ||
      config->render_channels > kMaximumRenderChannels ||
      !IsValidPreset(config->aec_preset) ||
      config->high_pass_filter_enabled > 1) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  std::unique_ptr<XmicWebRtcDsp> dsp(new (std::nothrow) XmicWebRtcDsp());
  if (!dsp) {
    return XMIC_DSP_OUT_OF_MEMORY;
  }

  webrtc::AudioProcessing::Config apm_config;
  apm_config.echo_canceller.enabled = true;
  apm_config.echo_canceller.enforce_high_pass_filtering =
      config->high_pass_filter_enabled != 0;
  apm_config.high_pass_filter.enabled = config->high_pass_filter_enabled != 0;
  apm_config.noise_suppression.enabled = false;
  apm_config.gain_controller1.enabled = false;
  apm_config.gain_controller2.enabled = false;

  webrtc::EchoCanceller3Config aec3_config =
      CreateAec3Config(config->aec_preset);
  if (!webrtc::EchoCanceller3Config::Validate(&aec3_config)) {
    return XMIC_DSP_WEBRTC_ERROR;
  }

  webrtc::BuiltinAudioProcessingBuilder builder(apm_config);
  builder.SetEchoCancellerConfig(aec3_config, aec3_config);
  if (config->aec_preset == XMIC_AEC_STRONG) {
    builder.SetEchoDetector(webrtc::CreateEchoDetector());
  }
  dsp->audio_processing = builder.Build(webrtc::CreateEnvironment());
  if (!dsp->audio_processing) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  dsp->speech_vad = WebRtcVad_Create();
  if (dsp->speech_vad == nullptr) {
    return XMIC_DSP_OUT_OF_MEMORY;
  }
  if (WebRtcVad_Init(dsp->speech_vad) != 0 ||
      WebRtcVad_set_mode(dsp->speech_vad, kSpeechVadMode) != 0) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  dsp->render_channels = config->render_channels;
  *out_dsp = dsp.release();
  return XMIC_DSP_OK;
}

void xmic_webrtc_dsp_destroy(XmicWebRtcDsp *dsp) { delete dsp; }

XmicDspResult xmic_webrtc_dsp_process_render(XmicWebRtcDsp *dsp,
                                             const float *interleaved_samples,
                                             size_t frames, size_t channels) {
  if (dsp == nullptr || interleaved_samples == nullptr ||
      frames != kFrameSamples || channels != dsp->render_channels) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  std::array<const float *, kMaximumRenderChannels> source_channels{};
  std::array<float *, kMaximumRenderChannels> destination_channels{};
  for (size_t channel = 0; channel < channels; ++channel) {
    for (size_t frame = 0; frame < frames; ++frame) {
      dsp->render_buffers[channel][frame] =
          SanitizeSample(interleaved_samples[frame * channels + channel]);
    }
    source_channels[channel] = dsp->render_buffers[channel].data();
    destination_channels[channel] = dsp->render_buffers[channel].data();
  }

  const webrtc::StreamConfig stream_config(kSampleRateHz, channels);
  return ToResult(dsp->audio_processing->ProcessReverseStream(
      source_channels.data(), stream_config, stream_config,
      destination_channels.data()));
}

XmicDspResult xmic_webrtc_dsp_process_aec_capture(XmicWebRtcDsp *dsp,
                                                  const float *input,
                                                  float *output, size_t frames,
                                                  int32_t stream_delay_ms) {
  if (dsp == nullptr || input == nullptr || output == nullptr ||
      frames != kFrameSamples || stream_delay_ms < 0 || stream_delay_ms > 500) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  std::transform(input, input + frames, dsp->capture_buffer.begin(),
                 SanitizeSample);
  const float *source_channels[] = {dsp->capture_buffer.data()};
  float *destination_channels[] = {output};
  const webrtc::StreamConfig stream_config(kSampleRateHz, 1);

  int result = dsp->audio_processing->set_stream_delay_ms(stream_delay_ms);
  if (result != webrtc::AudioProcessing::kNoError) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  result = dsp->audio_processing->ProcessStream(
      source_channels, stream_config, stream_config, destination_channels);
  if (result != webrtc::AudioProcessing::kNoError) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  dsp->stream_delay_ms.store(stream_delay_ms, std::memory_order_relaxed);
  std::transform(output, output + frames, output, SanitizeSample);
  return XMIC_DSP_OK;
}

XmicDspResult xmic_webrtc_dsp_detect_speech(XmicWebRtcDsp *dsp,
                                            const float *input, size_t frames,
                                            uint8_t *speech_detected) {
  if (dsp == nullptr || input == nullptr || speech_detected == nullptr ||
      frames != kFrameSamples) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  for (size_t frame = 0; frame < frames; ++frame) {
    dsp->vad_buffer[frame] = static_cast<int16_t>(
        std::lrint(SanitizeSample(input[frame]) * 32767.0f));
  }
  const int result = WebRtcVad_Process(dsp->speech_vad, kSampleRateHz,
                                       dsp->vad_buffer.data(), frames);
  if (result < 0) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  *speech_detected = result == 0 ? 0 : 1;
  return XMIC_DSP_OK;
}

XmicDspResult xmic_webrtc_dsp_reset_echo_path(XmicWebRtcDsp *dsp) {
  if (dsp == nullptr) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  dsp->stream_delay_ms.store(0, std::memory_order_relaxed);
  if (WebRtcVad_Init(dsp->speech_vad) != 0 ||
      WebRtcVad_set_mode(dsp->speech_vad, kSpeechVadMode) != 0) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  return ToResult(dsp->audio_processing->Initialize());
}

XmicDspResult xmic_webrtc_dsp_get_metrics(const XmicWebRtcDsp *dsp,
                                          XmicDspMetrics *metrics) {
  if (dsp == nullptr || metrics == nullptr ||
      metrics->struct_size != sizeof(XmicDspMetrics)) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  if (metrics->abi_version != XMIC_WEBRTC_DSP_ABI_VERSION) {
    return XMIC_DSP_UNSUPPORTED_ABI;
  }

  const auto statistics = dsp->audio_processing->GetStatistics();
  const float unavailable = std::numeric_limits<float>::quiet_NaN();
  metrics->stream_delay_ms =
      dsp->stream_delay_ms.load(std::memory_order_relaxed);
  metrics->estimated_delay_ms = statistics.delay_ms.value_or(0);
  metrics->echo_return_loss_db =
      statistics.echo_return_loss.has_value()
          ? static_cast<float>(*statistics.echo_return_loss)
          : unavailable;
  metrics->echo_return_loss_enhancement_db =
      statistics.echo_return_loss_enhancement.has_value()
          ? static_cast<float>(*statistics.echo_return_loss_enhancement)
          : unavailable;
  metrics->delay_estimate_available = statistics.delay_ms.has_value() ? 1 : 0;
  metrics->echo_return_loss_available =
      statistics.echo_return_loss.has_value() ? 1 : 0;
  metrics->echo_return_loss_enhancement_available =
      statistics.echo_return_loss_enhancement.has_value() ? 1 : 0;
  metrics->residual_echo_likelihood =
      statistics.residual_echo_likelihood.has_value()
          ? static_cast<float>(*statistics.residual_echo_likelihood)
          : unavailable;
  metrics->residual_echo_likelihood_available =
      statistics.residual_echo_likelihood.has_value() ? 1 : 0;
  std::fill(std::begin(metrics->reserved), std::end(metrics->reserved), 0);
  return XMIC_DSP_OK;
}

XmicDspResult xmic_webrtc_agc2_create(const XmicAgc2Config *config,
                                      XmicWebRtcAgc2 **out_agc2) {
  if (out_agc2 == nullptr) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  *out_agc2 = nullptr;
  if (config == nullptr || config->struct_size != sizeof(XmicAgc2Config)) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  if (config->abi_version != XMIC_WEBRTC_DSP_ABI_VERSION) {
    return XMIC_DSP_UNSUPPORTED_ABI;
  }
  if (!IsValidAgc2Config(*config)) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  std::unique_ptr<XmicWebRtcAgc2> agc2(new (std::nothrow) XmicWebRtcAgc2());
  if (!agc2) {
    return XMIC_DSP_OUT_OF_MEMORY;
  }

  webrtc::AudioProcessing::Config apm_config;
  apm_config.echo_canceller.enabled = false;
  apm_config.high_pass_filter.enabled = false;
  apm_config.noise_suppression.enabled = false;
  apm_config.gain_controller1.enabled = false;
  apm_config.gain_controller2.enabled = true;
  apm_config.gain_controller2.input_volume_controller.enabled = false;
  apm_config.gain_controller2.adaptive_digital.enabled =
      config->adaptive_digital_enabled != 0;
  apm_config.gain_controller2.adaptive_digital.headroom_db =
      config->headroom_db;
  apm_config.gain_controller2.adaptive_digital.max_gain_db =
      config->max_gain_db;
  apm_config.gain_controller2.adaptive_digital.initial_gain_db =
      config->initial_gain_db;
  apm_config.gain_controller2.adaptive_digital.max_gain_change_db_per_second =
      config->max_gain_change_db_per_second;
  apm_config.gain_controller2.adaptive_digital.max_output_noise_level_dbfs =
      config->max_output_noise_level_dbfs;
  apm_config.gain_controller2.fixed_digital.gain_db = config->fixed_gain_db;

  webrtc::BuiltinAudioProcessingBuilder builder(apm_config);
  agc2->audio_processing = builder.Build(webrtc::CreateEnvironment());
  if (!agc2->audio_processing) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  *out_agc2 = agc2.release();
  return XMIC_DSP_OK;
}

void xmic_webrtc_agc2_destroy(XmicWebRtcAgc2 *agc2) { delete agc2; }

XmicDspResult xmic_webrtc_agc2_process_capture(XmicWebRtcAgc2 *agc2,
                                               const float *input,
                                               float *output, size_t frames) {
  if (agc2 == nullptr || input == nullptr || output == nullptr ||
      frames != kFrameSamples) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }

  std::transform(input, input + frames, agc2->capture_buffer.begin(),
                 SanitizeSample);
  const float *source_channels[] = {agc2->capture_buffer.data()};
  float *destination_channels[] = {output};
  const webrtc::StreamConfig stream_config(kSampleRateHz, 1);
  const int result = agc2->audio_processing->ProcessStream(
      source_channels, stream_config, stream_config, destination_channels);
  if (result != webrtc::AudioProcessing::kNoError) {
    return XMIC_DSP_WEBRTC_ERROR;
  }
  std::transform(output, output + frames, output, SanitizeSample);
  return XMIC_DSP_OK;
}

XmicDspResult xmic_webrtc_agc2_reset(XmicWebRtcAgc2 *agc2) {
  if (agc2 == nullptr) {
    return XMIC_DSP_INVALID_ARGUMENT;
  }
  return ToResult(agc2->audio_processing->Initialize());
}
