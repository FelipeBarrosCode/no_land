#ifndef NOLAND_FRAME_DEADLINE_POLICY_H
#define NOLAND_FRAME_DEADLINE_POLICY_H

#include <stdbool.h>
#include <stdint.h>

#include "noland_moonlight.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum nl_frame_deadline_reason {
  NL_FRAME_DEADLINE_ON_TIME = 0,
  NL_FRAME_DEADLINE_LATE_ONLY = 1,
  NL_FRAME_DEADLINE_NOT_BACKPRESSURED = 2,
  NL_FRAME_DEADLINE_NO_NEWER_FRAME = 3,
  NL_FRAME_DEADLINE_COOLDOWN = 4,
  NL_FRAME_DEADLINE_SMOOTHNESS_MODE = 5,
  NL_FRAME_DEADLINE_LATE_SUPERSEDED = 6,
  NL_FRAME_DEADLINE_FEATURE_DISABLED = 7
} nl_frame_deadline_reason_t;

typedef struct nl_frame_deadline_input {
  bool feature_enabled;
  bool latency_priority_mode;
  uint64_t now_ns;
  uint64_t render_deadline_ns;
  uint64_t jitter_tolerance_ns;
  uint64_t estimated_frame_time_ns;
  uint32_t consecutive_late_frames;
  uint64_t latest_decoder_full_buffer_ms;
  bool newer_frame_queued;
  uint64_t last_adaptive_drop_ns;
} nl_frame_deadline_input_t;

typedef struct nl_frame_deadline_decision {
  bool is_late;
  bool severe_lateness;
  bool late_streak;
  bool backpressured;
  bool cooldown_expired;
  bool drop;
  uint64_t lateness_ns;
  uint64_t drop_threshold_ns;
  nl_frame_deadline_reason_t reason;
} nl_frame_deadline_decision_t;

uint64_t nl_estimated_frame_time_ns(uint32_t stream_fps);
uint64_t nl_jitter_tolerance_ns(uint32_t stream_fps, uint32_t configured_tolerance_us);
nl_frame_deadline_decision_t nl_decide_frame_deadline(const nl_frame_deadline_input_t* input);

typedef struct nl_pacing_resolution {
  nl_pacing_mode_t effective_mode;
  uint32_t sync_interval;
} nl_pacing_resolution_t;

nl_pacing_resolution_t nl_resolve_pacing_mode(
    nl_pacing_mode_t configured_mode,
    bool vsync_enabled,
    uint32_t stream_fps,
    uint32_t display_refresh_x100);

#ifdef __cplusplus
}
#endif

#endif
