#ifndef NOLAND_AUDIO_RENDERER_H
#define NOLAND_AUDIO_RENDERER_H

#include "Limelight.h"

#include <string.h>

typedef struct nl_audio_renderer {
  void* platform_context;
  uint32_t target_buffer_ms;
  uint32_t maximum_buffer_ms;
} nl_audio_renderer_t;

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
                                              int sample_length);
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
                                              int sample_length);
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
                                              int sample_length);
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
    int sample_length) {
  (void)renderer;
  (void)sample_data;
  (void)sample_length;
}
#endif

#endif
