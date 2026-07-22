#ifndef NOLAND_AUDIO_PCM_SYNC_BUFFER_H
#define NOLAND_AUDIO_PCM_SYNC_BUFFER_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct nl_audio_pcm_sync_buffer {
  float* samples;
  uint32_t capacity_frames;
  uint32_t channel_count;
  uint32_t read_index_frames;
  uint32_t write_index_frames;
  uint32_t size_frames;
} nl_audio_pcm_sync_buffer_t;

bool nl_audio_pcm_sync_buffer_init(nl_audio_pcm_sync_buffer_t* buffer,
                                   uint32_t capacity_frames,
                                   uint32_t channel_count);
void nl_audio_pcm_sync_buffer_reset(nl_audio_pcm_sync_buffer_t* buffer);
void nl_audio_pcm_sync_buffer_cleanup(nl_audio_pcm_sync_buffer_t* buffer);
uint32_t nl_audio_pcm_sync_buffer_available_frames(const nl_audio_pcm_sync_buffer_t* buffer);
uint32_t nl_audio_pcm_sync_buffer_free_frames(const nl_audio_pcm_sync_buffer_t* buffer);
uint32_t nl_audio_pcm_sync_buffer_discard_frames(nl_audio_pcm_sync_buffer_t* buffer,
                                                 uint32_t frames);
uint32_t nl_audio_pcm_sync_buffer_write(nl_audio_pcm_sync_buffer_t* buffer,
                                        const float* interleaved_samples,
                                        uint32_t frames);
uint32_t nl_audio_pcm_sync_buffer_read(nl_audio_pcm_sync_buffer_t* buffer,
                                       float* interleaved_samples,
                                       uint32_t frames);

#endif
