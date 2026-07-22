#include "noland_audio_renderer.h"

#include <opus/opus_multistream.h>

#include <windows.h>
#include <mmsystem.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#define WAVEOUT_BUFFER_COUNT 8
#define WAVEOUT_BUFFER_MS    30

#pragma comment(lib, "winmm.lib")

typedef struct nl_audio_windows_context {
  HWAVEOUT waveout;
  OpusMSDecoder* decoder;
  float* decode_scratch;
  int16_t* interleaved_pcm;
  WAVEHDR headers[WAVEOUT_BUFFER_COUNT];
  int16_t* buffers[WAVEOUT_BUFFER_COUNT];
  int buffer_index;
  int sample_rate;
  int channel_count;
  int samples_per_frame;
  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
  volatile int initialized;
} nl_audio_windows_context_t;

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

  nl_audio_windows_context_t* ctx = calloc(1, sizeof(nl_audio_windows_context_t));
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

  size_t scratch_samples = (size_t)ctx->samples_per_frame * (size_t)ctx->channel_count;
  ctx->decode_scratch = calloc(scratch_samples, sizeof(float));
  if (ctx->decode_scratch == NULL) {
    free(ctx);
    return -1;
  }

  ctx->interleaved_pcm = calloc(scratch_samples, sizeof(int16_t));
  if (ctx->interleaved_pcm == NULL) {
    free(ctx->decode_scratch);
    free(ctx);
    return -1;
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
    free(ctx->decode_scratch);
    free(ctx->interleaved_pcm);
    free(ctx);
    return -1;
  }

  WAVEFORMATEX wfx;
  memset(&wfx, 0, sizeof(wfx));
  wfx.wFormatTag = WAVE_FORMAT_PCM;
  wfx.nChannels = (WORD)ctx->channel_count;
  wfx.nSamplesPerSec = (DWORD)ctx->sample_rate;
  wfx.wBitsPerSample = 16;
  wfx.nBlockAlign = (WORD)((wfx.nChannels * wfx.wBitsPerSample) / 8);
  wfx.nAvgBytesPerSec = wfx.nSamplesPerSec * wfx.nBlockAlign;
  wfx.cbSize = 0;

  MMRESULT result = waveOutOpen(&ctx->waveout, WAVE_MAPPER, &wfx, 0, 0, CALLBACK_NULL);
  if (result != MMSYSERR_NOERROR) {
    fprintf(stderr, "[noland-audio] waveOutOpen failed: %u\n", (unsigned int)result);
    opus_multistream_decoder_destroy(ctx->decoder);
    free(ctx->decode_scratch);
    free(ctx->interleaved_pcm);
    free(ctx);
    return -1;
  }

  DWORD buffer_frames = (DWORD)(((uint64_t)ctx->sample_rate * (uint64_t)WAVEOUT_BUFFER_MS) / 1000ULL);
  DWORD buffer_bytes = buffer_frames * wfx.nBlockAlign;

  for (int i = 0; i < WAVEOUT_BUFFER_COUNT; i++) {
    ctx->buffers[i] = calloc(buffer_frames, wfx.nBlockAlign);
    if (ctx->buffers[i] == NULL) {
      goto cleanup_buffers;
    }
    memset(&ctx->headers[i], 0, sizeof(WAVEHDR));
    ctx->headers[i].lpData = (LPSTR)ctx->buffers[i];
    ctx->headers[i].dwBufferLength = buffer_bytes;
    ctx->headers[i].dwFlags = 0;
    result = waveOutPrepareHeader(ctx->waveout, &ctx->headers[i], sizeof(WAVEHDR));
    if (result != MMSYSERR_NOERROR) {
      fprintf(stderr, "[noland-audio] waveOutPrepareHeader[%d] failed: %u\n", i, (unsigned int)result);
      free(ctx->buffers[i]);
      ctx->buffers[i] = NULL;
      goto cleanup_buffers;
    }
  }

  ctx->buffer_index = 0;
  ctx->initialized = 1;

  fprintf(stderr, "[noland-audio] init sampleRate=%d channels=%d samplesPerFrame=%d bufferMs=%u target=%u max=%u\n",
          ctx->sample_rate, ctx->channel_count, ctx->samples_per_frame,
          (unsigned int)WAVEOUT_BUFFER_MS, (unsigned int)ctx->target_buffer_ms,
          (unsigned int)ctx->maximum_buffer_ms);

  renderer->platform_context = ctx;
  return 0;

cleanup_buffers:
  for (int j = 0; j < i; j++) {
    if (ctx->buffers[j] != NULL) {
      waveOutUnprepareHeader(ctx->waveout, &ctx->headers[j], sizeof(WAVEHDR));
      free(ctx->buffers[j]);
      ctx->buffers[j] = NULL;
    }
  }
  waveOutClose(ctx->waveout);
  opus_multistream_decoder_destroy(ctx->decoder);
  free(ctx->decode_scratch);
  free(ctx->interleaved_pcm);
  free(ctx);
  return -1;
}

void nl_audio_renderer_start(nl_audio_renderer_t* renderer) {
  (void)renderer;
  // waveOut begins playback as soon as buffers are written
}

void nl_audio_renderer_stop(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  nl_audio_windows_context_t* ctx = (nl_audio_windows_context_t*)renderer->platform_context;
  if (ctx->waveout != NULL) {
    waveOutReset(ctx->waveout);
  }
}

void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    if (renderer != NULL) {
      memset(renderer, 0, sizeof(*renderer));
    }
    return;
  }

  nl_audio_windows_context_t* ctx = (nl_audio_windows_context_t*)renderer->platform_context;

  if (ctx->waveout != NULL) {
    waveOutReset(ctx->waveout);
    for (int i = 0; i < WAVEOUT_BUFFER_COUNT; i++) {
      if (ctx->buffers[i] != NULL) {
        waveOutUnprepareHeader(ctx->waveout, &ctx->headers[i], sizeof(WAVEHDR));
        free(ctx->buffers[i]);
        ctx->buffers[i] = NULL;
      }
    }
    waveOutClose(ctx->waveout);
    ctx->waveout = NULL;
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

static int write_audio_buffer(nl_audio_windows_context_t* ctx) {
  if (!ctx->initialized || ctx->waveout == NULL) {
    return -1;
  }

  WAVEHDR* hdr = &ctx->headers[ctx->buffer_index];

  // If this buffer is still queued, skip it to avoid glitching
  if (hdr->dwFlags & WHDR_INQUEUE) {
    return 0;
  }

  // Re-prepare if it was unprepared after playing
  if (hdr->dwFlags & WHDR_DONE) {
    MMRESULT r = waveOutUnprepareHeader(ctx->waveout, hdr, sizeof(WAVEHDR));
    if (r != MMSYSERR_NOERROR) {
      return -1;
    }
    hdr->dwFlags = 0;
    r = waveOutPrepareHeader(ctx->waveout, hdr, sizeof(WAVEHDR));
    if (r != MMSYSERR_NOERROR) {
      return -1;
    }
  }

  MMRESULT result = waveOutWrite(ctx->waveout, hdr, sizeof(WAVEHDR));
  if (result != MMSYSERR_NOERROR) {
    fprintf(stderr, "[noland-audio] waveOutWrite failed: %u\n", (unsigned int)result);
    return -1;
  }

  ctx->buffer_index = (ctx->buffer_index + 1) % WAVEOUT_BUFFER_COUNT;
  return 0;
}

void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length) {
  if (renderer == NULL || renderer->platform_context == NULL || sample_data == NULL || sample_length <= 0) {
    return;
  }

  nl_audio_windows_context_t* ctx = (nl_audio_windows_context_t*)renderer->platform_context;
  if (!ctx->initialized || ctx->decoder == NULL || ctx->decode_scratch == NULL) {
    return;
  }

  int decoded_samples = opus_multistream_decode_float(
      ctx->decoder,
      (const unsigned char*)sample_data,
      sample_length,
      ctx->decode_scratch,
      ctx->samples_per_frame,
      0);

  if (decoded_samples <= 0) {
    fprintf(stderr, "[noland-audio] Opus decode failed: %s\n", opus_strerror(decoded_samples));
    return;
  }

  size_t sample_count = (size_t)decoded_samples * (size_t)ctx->channel_count;
  for (size_t i = 0; i < sample_count; i++) {
    float sample = ctx->decode_scratch[i];
    if (sample > 1.0f) sample = 1.0f;
    if (sample < -1.0f) sample = -1.0f;
    ctx->interleaved_pcm[i] = (int16_t)(sample * 32767.0f);
  }

  // Copy into the current waveOut buffer at the next write position
  WAVEHDR* hdr = &ctx->headers[ctx->buffer_index];
  DWORD available = hdr->dwBufferLength;
  DWORD needed = (DWORD)(sample_count * sizeof(int16_t));
  DWORD bytes_to_copy = needed < available ? needed : available;

  memcpy(hdr->lpData, ctx->interleaved_pcm, bytes_to_copy);
  hdr->dwBufferLength = bytes_to_copy;

  // If we filled enough or this is a new buffer, write it out
  write_audio_buffer(ctx);
}
