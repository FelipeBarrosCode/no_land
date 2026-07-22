#ifndef NOLAND_AUDIO_RENDERER_H
#define NOLAND_AUDIO_RENDERER_H

#include "Limelight.h"

#include <string.h>

typedef struct nl_audio_renderer {
  void* platform_context;
  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
} nl_audio_renderer_t;

typedef struct nl_audio_renderer_stats {
  uint32_t buffered_frames;
  uint32_t startup_prefill_frames;
  uint32_t target_frames;
  uint32_t current_target_frames;
  uint32_t maximum_frames;
  uint64_t concealment_count;
  uint64_t dropped_frames_count;
  uint64_t starvation_event_count;
  uint64_t target_increase_count;
  uint64_t target_decrease_count;
} nl_audio_renderer_stats_t;

#if defined(__APPLE__)
int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags);
void nl_audio_renderer_start(nl_audio_renderer_t* renderer);
void nl_audio_renderer_stop(nl_audio_renderer_t* renderer);
void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer);
void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us);
void nl_audio_renderer_get_stats(nl_audio_renderer_t* renderer,
                                 nl_audio_renderer_stats_t* stats);
#elif defined(__linux__)
int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags);
void nl_audio_renderer_start(nl_audio_renderer_t* renderer);
void nl_audio_renderer_stop(nl_audio_renderer_t* renderer);
void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer);
void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us);
void nl_audio_renderer_get_stats(nl_audio_renderer_t* renderer,
                                 nl_audio_renderer_stats_t* stats);
#elif defined(_WIN32)
int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags);
void nl_audio_renderer_start(nl_audio_renderer_t* renderer);
void nl_audio_renderer_stop(nl_audio_renderer_t* renderer);
void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer);
void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us);
void nl_audio_renderer_get_stats(nl_audio_renderer_t* renderer,
                                 nl_audio_renderer_stats_t* stats);
#else
static inline int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                                         int audio_configuration,
                                         const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                                         int ar_flags) {
  (void)audio_configuration;
  (void)opus_config;
  (void)ar_flags;
  if (renderer != NULL) {
    memset(renderer, 0, sizeof(*renderer));
  }
  return 0;
}

static inline void nl_audio_renderer_start(nl_audio_renderer_t* renderer) {
  (void)renderer;
}

static inline void nl_audio_renderer_stop(nl_audio_renderer_t* renderer) {
  (void)renderer;
}

static inline void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer) {
  if (renderer != NULL) {
    memset(renderer, 0, sizeof(*renderer));
  }
}

static inline void nl_audio_renderer_decode_and_play_sample(
    nl_audio_renderer_t* renderer,
    char* sample_data,
    int sample_length,
    uint16_t sequence_number,
    uint32_t rtp_timestamp,
    uint64_t extended_rtp_timestamp,
    uint64_t arrival_time_us) {
  (void)renderer;
  (void)sample_data;
  (void)sample_length;
  (void)sequence_number;
  (void)rtp_timestamp;
  (void)extended_rtp_timestamp;
  (void)arrival_time_us;
}

static inline void nl_audio_renderer_get_stats(nl_audio_renderer_t* renderer,
                                               nl_audio_renderer_stats_t* stats) {
  (void)renderer;
  if (stats != NULL) {
    memset(stats, 0, sizeof(*stats));
  }
}
#endif

#endif
