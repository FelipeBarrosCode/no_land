#include "noland_audio_renderer.h"

#include <opus/opus_multistream.h>
#include <pulse/simple.h>
#include <pulse/error.h>
#include <pulse/gccmacro.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

typedef struct nl_audio_linux_context {
  pa_simple* pulse;
  OpusMSDecoder* decoder;
  float* decode_scratch;
  int16_t* interleaved_pcm;
  int sample_rate;
  int channel_count;
  int samples_per_frame;
  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
} nl_audio_linux_context_t;

int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags) {
  (void)audio_configuration;
  (void)ar_flags;

  if (renderer == NULL || opus_config == NULL) {
    return -1;
  }

  nl_audio_renderer_cleanup(renderer);

  nl_audio_linux_context_t* ctx = calloc(1, sizeof(nl_audio_linux_context_t));
  if (ctx == NULL) {
    return -1;
  }

  ctx->sample_rate = opus_config->sampleRate;
  ctx->channel_count = opus_config->channelCount;
  ctx->samples_per_frame = opus_config->samplesPerFrame;
  ctx->target_buffer_ms = renderer->target_buffer_ms > 0 ? renderer->target_buffer_ms : 20U;
  ctx->maximum_buffer_ms = renderer->maximum_buffer_ms > 0 ? renderer->maximum_buffer_ms : 80U;

  if (ctx->maximum_buffer_ms < ctx->target_buffer_ms) {
    ctx->maximum_buffer_ms = ctx->target_buffer_ms;
  }

  // Allocate decode scratch buffer (float planar/interleaved from Opus)
  size_t scratch_samples = (size_t)ctx->samples_per_frame * (size_t)ctx->channel_count;
  ctx->decode_scratch = calloc(scratch_samples, sizeof(float));
  if (ctx->decode_scratch == NULL) {
    free(ctx);
    return -1;
  }

  // Allocate interleaved PCM buffer for PulseAudio (int16_t interleaved)
  ctx->interleaved_pcm = calloc(scratch_samples, sizeof(int16_t));
  if (ctx->interleaved_pcm == NULL) {
    free(ctx->decode_scratch);
    free(ctx);
    return -1;
  }

  // Create Opus decoder
  int error = 0;
  ctx->decoder = opus_multistream_decoder_create(
      opus_config->sampleRate,
      opus_config->channelCount,
      opus_config->streams,
      opus_config->coupledStreams,
      opus_config->mapping,
      &error);
  if (ctx->decoder == NULL || error != OPUS_OK) {
    fprintf(stderr, "[noland-audio] Failed to create Opus decoder: %s\n", opus_strerror(error));
    free(ctx->decode_scratch);
    free(ctx->interleaved_pcm);
    free(ctx);
    return -1;
  }

  // Use PulseAudio simple API — works with both PulseAudio and PipeWire-Pulse
  pa_sample_spec ss;
  ss.format = PA_SAMPLE_S16LE;
  ss.rate = (uint32_t)ctx->sample_rate;
  ss.channels = (uint8_t)ctx->channel_count;

  int pa_error = 0;
  ctx->pulse = pa_simple_new(
      NULL,                           // default server
      "Noland Connect",               // application name
      PA_STREAM_PLAYBACK,             // direction
      NULL,                           // default sink
      "Moonlight Audio",              // stream description
      &ss,                            // sample format
      NULL,                           // default channel map
      NULL,                           // buffering attributes (use defaults)
      &pa_error);

  if (ctx->pulse == NULL) {
    fprintf(stderr, "[noland-audio] pa_simple_new failed: %s\n", pa_strerror(pa_error));
    opus_multistream_decoder_destroy(ctx->decoder);
    free(ctx->decode_scratch);
    free(ctx->interleaved_pcm);
    free(ctx);
    return -1;
  }

  fprintf(stderr, "[noland-audio] init sampleRate=%d channels=%d samplesPerFrame=%d target=%u max=%u\n",
          ctx->sample_rate, ctx->channel_count, ctx->samples_per_frame,
          (unsigned int)ctx->target_buffer_ms, (unsigned int)ctx->maximum_buffer_ms);

  renderer->platform_context = ctx;
  return 0;
}

void nl_audio_renderer_start(nl_audio_renderer_t* renderer) {
  (void)renderer;
  // PulseAudio simple API starts immediately on first write
}

void nl_audio_renderer_stop(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  nl_audio_linux_context_t* ctx = (nl_audio_linux_context_t*)renderer->platform_context;
  if (ctx->pulse != NULL) {
    pa_simple_drain(ctx->pulse, NULL);
  }
}

void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    if (renderer != NULL) {
      memset(renderer, 0, sizeof(*renderer));
    }
    return;
  }

  nl_audio_linux_context_t* ctx = (nl_audio_linux_context_t*)renderer->platform_context;

  if (ctx->pulse != NULL) {
    pa_simple_drain(ctx->pulse, NULL);
    pa_simple_free(ctx->pulse);
    ctx->pulse = NULL;
  }
  if (ctx->decoder != NULL) {
    opus_multistream_decoder_destroy(ctx->decoder);
    ctx->decoder = NULL;
  }
  free(ctx->decode_scratch);
  ctx->decode_scratch = NULL;
  free(ctx->interleaved_pcm);
  ctx->interleaved_pcm = NULL;
  free(ctx);
  renderer->platform_context = NULL;
}

void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length) {
  if (renderer == NULL || renderer->platform_context == NULL || sample_length < 0) {
    return;
  }

  nl_audio_linux_context_t* ctx = (nl_audio_linux_context_t*)renderer->platform_context;
  if (ctx->decoder == NULL || ctx->pulse == NULL || ctx->decode_scratch == NULL) {
    return;
  }

  // Moonlight passes NULL/0 to request Opus packet loss concealment (PLC).
  // Do not early-return in that case; feed it through to the decoder.
  const unsigned char* opus_data = sample_data != NULL ? (const unsigned char*)sample_data : NULL;

  // Decode Opus to float PCM
  int decoded_samples = opus_multistream_decode_float(
      ctx->decoder,
      opus_data,
      sample_length,
      ctx->decode_scratch,
      ctx->samples_per_frame,
      0);  // no FEC

  if (decoded_samples <= 0) {
    fprintf(stderr, "[noland-audio] Opus decode failed: %s\n", opus_strerror(decoded_samples));
    return;
  }

  // Convert float to int16_t interleaved
  size_t sample_count = (size_t)decoded_samples * (size_t)ctx->channel_count;
  for (size_t i = 0; i < sample_count; i++) {
    float sample = ctx->decode_scratch[i];
    // Clamp to [-1.0, 1.0]
    if (sample > 1.0f) sample = 1.0f;
    if (sample < -1.0f) sample = -1.0f;
    ctx->interleaved_pcm[i] = (int16_t)(sample * 32767.0f);
  }

  // Write to PulseAudio (blocking, but fast for small buffers)
  int pa_error = 0;
  if (pa_simple_write(ctx->pulse, ctx->interleaved_pcm, sample_count * sizeof(int16_t), &pa_error) < 0) {
    fprintf(stderr, "[noland-audio] pa_simple_write failed: %s\n", pa_strerror(pa_error));
  }
}
