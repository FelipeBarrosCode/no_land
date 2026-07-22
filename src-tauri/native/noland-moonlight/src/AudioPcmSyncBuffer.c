#include "AudioPcmSyncBuffer.h"

#include <stdlib.h>
#include <string.h>

static uint32_t nl_min_u32(uint32_t a, uint32_t b) {
  return a < b ? a : b;
}

bool nl_audio_pcm_sync_buffer_init(nl_audio_pcm_sync_buffer_t* buffer,
                                   uint32_t capacity_frames,
                                   uint32_t channel_count) {
  if (buffer == NULL || capacity_frames == 0 || channel_count == 0) {
    return false;
  }

  memset(buffer, 0, sizeof(*buffer));
  buffer->samples = (float*)calloc((size_t)capacity_frames * (size_t)channel_count, sizeof(float));
  if (buffer->samples == NULL) {
    return false;
  }

  buffer->capacity_frames = capacity_frames;
  buffer->channel_count = channel_count;
  return true;
}

void nl_audio_pcm_sync_buffer_reset(nl_audio_pcm_sync_buffer_t* buffer) {
  if (buffer == NULL) {
    return;
  }

  buffer->read_index_frames = 0;
  buffer->write_index_frames = 0;
  buffer->size_frames = 0;
}

void nl_audio_pcm_sync_buffer_cleanup(nl_audio_pcm_sync_buffer_t* buffer) {
  if (buffer == NULL) {
    return;
  }

  free(buffer->samples);
  memset(buffer, 0, sizeof(*buffer));
}

uint32_t nl_audio_pcm_sync_buffer_available_frames(const nl_audio_pcm_sync_buffer_t* buffer) {
  return buffer != NULL ? buffer->size_frames : 0;
}

uint32_t nl_audio_pcm_sync_buffer_free_frames(const nl_audio_pcm_sync_buffer_t* buffer) {
  return buffer != NULL ? (buffer->capacity_frames - buffer->size_frames) : 0;
}

uint32_t nl_audio_pcm_sync_buffer_discard_frames(nl_audio_pcm_sync_buffer_t* buffer,
                                                 uint32_t frames) {
  uint32_t discarded;
  if (buffer == NULL || buffer->capacity_frames == 0 || frames == 0) {
    return 0;
  }

  discarded = nl_min_u32(frames, buffer->size_frames);
  buffer->read_index_frames = (buffer->read_index_frames + discarded) % buffer->capacity_frames;
  buffer->size_frames -= discarded;
  return discarded;
}

uint32_t nl_audio_pcm_sync_buffer_write(nl_audio_pcm_sync_buffer_t* buffer,
                                        const float* interleaved_samples,
                                        uint32_t frames) {
  uint32_t written = 0;
  if (buffer == NULL || buffer->samples == NULL || interleaved_samples == NULL || frames == 0) {
    return 0;
  }

  while (written < frames && buffer->size_frames < buffer->capacity_frames) {
    uint32_t contiguous_capacity = buffer->capacity_frames - buffer->write_index_frames;
    uint32_t writable = nl_min_u32(frames - written, contiguous_capacity);
    writable = nl_min_u32(writable, buffer->capacity_frames - buffer->size_frames);
    memcpy(&buffer->samples[(size_t)buffer->write_index_frames * buffer->channel_count],
           &interleaved_samples[(size_t)written * buffer->channel_count],
           (size_t)writable * buffer->channel_count * sizeof(float));
    buffer->write_index_frames = (buffer->write_index_frames + writable) % buffer->capacity_frames;
    buffer->size_frames += writable;
    written += writable;
  }

  return written;
}

uint32_t nl_audio_pcm_sync_buffer_read(nl_audio_pcm_sync_buffer_t* buffer,
                                       float* interleaved_samples,
                                       uint32_t frames) {
  uint32_t read = 0;
  if (buffer == NULL || buffer->samples == NULL || interleaved_samples == NULL || frames == 0) {
    return 0;
  }

  while (read < frames && buffer->size_frames > 0) {
    uint32_t contiguous_available = buffer->capacity_frames - buffer->read_index_frames;
    uint32_t readable = nl_min_u32(frames - read, contiguous_available);
    readable = nl_min_u32(readable, buffer->size_frames);
    memcpy(&interleaved_samples[(size_t)read * buffer->channel_count],
           &buffer->samples[(size_t)buffer->read_index_frames * buffer->channel_count],
           (size_t)readable * buffer->channel_count * sizeof(float));
    buffer->read_index_frames = (buffer->read_index_frames + readable) % buffer->capacity_frames;
    buffer->size_frames -= readable;
    read += readable;
  }

  return read;
}
