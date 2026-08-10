#include "noland_audio_renderer.h"

#include <Limelight.h>
#include <opus/opus_multistream.h>
#include <SDL2/SDL.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

typedef struct nl_audio_macos_context {
  SDL_AudioDeviceID audio_device;
  OpusMSDecoder* decoder;
  float* audio_buffer;
  int sample_rate;
  int channel_count;
  int samples_per_frame;
  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
  uint32_t frame_size_bytes;
  uint32_t frame_duration_ms;
  int sdl_audio_initialized_here;
} nl_audio_macos_context_t;

int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags) {
  (void)audio_configuration;
  (void)ar_flags;

  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
  if (renderer == NULL || opus_config == NULL) {
    return -1;
  }

  target_buffer_ms = renderer->target_buffer_ms;
  maximum_buffer_ms = renderer->maximum_buffer_ms;
  nl_audio_renderer_cleanup(renderer);
  renderer->target_buffer_ms = target_buffer_ms;
  renderer->maximum_buffer_ms = maximum_buffer_ms;

  nl_audio_macos_context_t* ctx = calloc(1, sizeof(nl_audio_macos_context_t));
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

  if ((SDL_WasInit(SDL_INIT_AUDIO) & SDL_INIT_AUDIO) == 0) {
    if (SDL_InitSubSystem(SDL_INIT_AUDIO) != 0) {
      fprintf(stderr, "[noland-audio] SDL_InitSubSystem(SDL_INIT_AUDIO) failed: %s\n", SDL_GetError());
      free(ctx);
      return -1;
    }
    ctx->sdl_audio_initialized_here = 1;
  }

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
    if (ctx->sdl_audio_initialized_here) {
      SDL_QuitSubSystem(SDL_INIT_AUDIO);
    }
    free(ctx);
    return -1;
  }

  SDL_AudioSpec want, have;
  SDL_zero(want);
  want.freq = opus_config->sampleRate;
  want.format = AUDIO_F32SYS;
  want.channels = (Uint8)opus_config->channelCount;
  want.samples = SDL_max(480, opus_config->samplesPerFrame * 3);

  ctx->frame_duration_ms = opus_config->sampleRate > 0
      ? (uint32_t)(opus_config->samplesPerFrame / (opus_config->sampleRate / 1000))
      : 5U;
  if (ctx->frame_duration_ms == 0) {
    ctx->frame_duration_ms = 5U;
  }

  ctx->frame_size_bytes = (uint32_t)(opus_config->samplesPerFrame *
                                     opus_config->channelCount *
                                     (int)sizeof(float));

  ctx->audio_device = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
  if (ctx->audio_device == 0) {
    fprintf(stderr, "[noland-audio] Failed to open SDL audio device: %s\n", SDL_GetError());
    opus_multistream_decoder_destroy(ctx->decoder);
    if (ctx->sdl_audio_initialized_here) {
      SDL_QuitSubSystem(SDL_INIT_AUDIO);
    }
    free(ctx);
    return -1;
  }

  ctx->audio_buffer = (float*)SDL_malloc(ctx->frame_size_bytes);
  if (ctx->audio_buffer == NULL) {
    fprintf(stderr, "[noland-audio] Failed to allocate SDL audio buffer\n");
    SDL_CloseAudioDevice(ctx->audio_device);
    opus_multistream_decoder_destroy(ctx->decoder);
    if (ctx->sdl_audio_initialized_here) {
      SDL_QuitSubSystem(SDL_INIT_AUDIO);
    }
    free(ctx);
    return -1;
  }

  fprintf(stderr, "[noland-audio] SDL desired buffer: %u samples (%u bytes)\n",
          want.samples,
          (unsigned int)(want.samples * want.channels * sizeof(float)));
  fprintf(stderr, "[noland-audio] SDL obtained buffer: %u samples (%u bytes)\n",
          have.samples,
          have.size);
  fprintf(stderr, "[noland-audio] SDL audio driver: %s\n", SDL_GetCurrentAudioDriver());

  renderer->platform_context = ctx;
  return 0;
}

void nl_audio_renderer_start(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  nl_audio_macos_context_t* ctx = (nl_audio_macos_context_t*)renderer->platform_context;
  SDL_PauseAudioDevice(ctx->audio_device, 0);
}

void nl_audio_renderer_stop(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  nl_audio_macos_context_t* ctx = (nl_audio_macos_context_t*)renderer->platform_context;
  if (ctx->audio_device != 0) {
    SDL_PauseAudioDevice(ctx->audio_device, 1);
    SDL_ClearQueuedAudio(ctx->audio_device);
  }
}

void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    if (renderer != NULL) {
      memset(renderer, 0, sizeof(*renderer));
    }
    return;
  }

  nl_audio_macos_context_t* ctx = (nl_audio_macos_context_t*)renderer->platform_context;

  if (ctx->audio_device != 0) {
    SDL_PauseAudioDevice(ctx->audio_device, 1);
    SDL_ClearQueuedAudio(ctx->audio_device);
    SDL_CloseAudioDevice(ctx->audio_device);
    ctx->audio_device = 0;
  }
  if (ctx->audio_buffer != NULL) {
    SDL_free(ctx->audio_buffer);
    ctx->audio_buffer = NULL;
  }
  if (ctx->decoder != NULL) {
    opus_multistream_decoder_destroy(ctx->decoder);
    ctx->decoder = NULL;
  }
  if (ctx->sdl_audio_initialized_here) {
    SDL_QuitSubSystem(SDL_INIT_AUDIO);
    ctx->sdl_audio_initialized_here = 0;
  }

  free(ctx);
  renderer->platform_context = NULL;
}

void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length) {
  if (renderer == NULL || renderer->platform_context == NULL || sample_length < 0) {
    return;
  }

  nl_audio_macos_context_t* ctx = (nl_audio_macos_context_t*)renderer->platform_context;
  if (ctx->decoder == NULL || ctx->audio_buffer == NULL || ctx->audio_device == 0) {
    return;
  }

  if (sample_length > 0 && LiGetPendingAudioDuration() > 30) {
    return;
  }

  for (int i = 0; i < 100; i++) {
    if (SDL_GetAudioDeviceStatus(ctx->audio_device) == SDL_AUDIO_STOPPED) {
      return;
    }

    if ((SDL_GetQueuedAudioSize(ctx->audio_device) / ctx->frame_size_bytes) * ctx->frame_duration_ms <= 50U) {
      break;
    }

    SDL_Delay(1);
  }

  const unsigned char* opus_data = sample_data != NULL ? (const unsigned char*)sample_data : NULL;
  int decoded_samples = opus_multistream_decode_float(
      ctx->decoder,
      opus_data,
      sample_length,
      ctx->audio_buffer,
      ctx->samples_per_frame,
      0);

  if (decoded_samples <= 0) {
    fprintf(stderr, "[noland-audio] Opus decode failed: %s\n", opus_strerror(decoded_samples));
    return;
  }

  uint32_t bytes_written = (uint32_t)(decoded_samples * ctx->channel_count * (int)sizeof(float));
  if (SDL_QueueAudio(ctx->audio_device, ctx->audio_buffer, bytes_written) < 0) {
    fprintf(stderr, "[noland-audio] SDL_QueueAudio failed: %s\n", SDL_GetError());
  }
}
