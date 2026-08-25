#include "noland_frame_deadline_policy.h"

#include <string.h>

uint64_t nl_estimated_frame_time_ns(uint32_t stream_fps) {
  return stream_fps == 0U ? 0U : 1000000000ULL / stream_fps;
}

uint64_t nl_jitter_tolerance_ns(uint32_t stream_fps, uint32_t configured_tolerance_us) {
  uint64_t frame_time_ns = nl_estimated_frame_time_ns(stream_fps);
  if (configured_tolerance_us != 0U) {
    return (uint64_t)configured_tolerance_us * 1000ULL;
  }
  return frame_time_ns / 2U;
}

nl_pacing_resolution_t nl_resolve_pacing_mode(
    nl_pacing_mode_t configured_mode,
    bool vsync_enabled,
    uint32_t stream_fps,
    uint32_t display_refresh_x100) {
  nl_pacing_resolution_t resolution;
  uint64_t stream_fps_x100;
  uint64_t ratio;
  memset(&resolution, 0, sizeof(resolution));
  resolution.effective_mode = NL_PACING_MODE_OFF;
  if (!vsync_enabled || configured_mode == NL_PACING_MODE_OFF || stream_fps == 0U) {
    return resolution;
  }
  if (configured_mode == NL_PACING_MODE_SOFTWARE) {
    resolution.effective_mode = NL_PACING_MODE_SOFTWARE;
    return resolution;
  }

  stream_fps_x100 = (uint64_t)stream_fps * 100ULL;
  ratio = stream_fps_x100 != 0U && display_refresh_x100 != 0U &&
          display_refresh_x100 % stream_fps_x100 == 0U
      ? display_refresh_x100 / stream_fps_x100
      : 0U;
  if (ratio >= 1U && ratio <= 4U) {
    resolution.effective_mode = NL_PACING_MODE_HARDWARE_MULTIPLE;
    resolution.sync_interval = (uint32_t)ratio;
  } else if (configured_mode == NL_PACING_MODE_AUTOMATIC) {
    resolution.effective_mode = NL_PACING_MODE_SOFTWARE;
  }
  return resolution;
}

nl_frame_deadline_decision_t nl_decide_frame_deadline(const nl_frame_deadline_input_t* input) {
  nl_frame_deadline_decision_t decision;
  uint64_t half_frame_ns;
  uint64_t late_threshold_ns;
  uint64_t cooldown_ns;
  uint64_t one_frame_ms;

  memset(&decision, 0, sizeof(decision));
  decision.reason = NL_FRAME_DEADLINE_ON_TIME;
  if (input == NULL || input->estimated_frame_time_ns == 0U) {
    decision.reason = NL_FRAME_DEADLINE_FEATURE_DISABLED;
    return decision;
  }

  half_frame_ns = input->estimated_frame_time_ns / 2U;
  late_threshold_ns = input->jitter_tolerance_ns > half_frame_ns
      ? input->jitter_tolerance_ns
      : half_frame_ns;
  decision.drop_threshold_ns = late_threshold_ns;

  if (input->now_ns > input->render_deadline_ns) {
    decision.lateness_ns = input->now_ns - input->render_deadline_ns;
  }
  decision.is_late = decision.lateness_ns > input->jitter_tolerance_ns;
  decision.severe_lateness = decision.lateness_ns > late_threshold_ns * 2U;
  decision.late_streak = input->consecutive_late_frames >= 3U;

  one_frame_ms = (input->estimated_frame_time_ns + 999999ULL) / 1000000ULL;
  decision.backpressured = input->latest_decoder_full_buffer_ms >= one_frame_ms;

  cooldown_ns = input->estimated_frame_time_ns > 8000000ULL
      ? input->estimated_frame_time_ns
      : 8000000ULL;
  decision.cooldown_expired = input->last_adaptive_drop_ns == 0U ||
      (input->now_ns >= input->last_adaptive_drop_ns &&
       input->now_ns - input->last_adaptive_drop_ns >= cooldown_ns);

  if (!input->feature_enabled) {
    decision.reason = NL_FRAME_DEADLINE_FEATURE_DISABLED;
  } else if (!input->latency_priority_mode) {
    decision.reason = NL_FRAME_DEADLINE_SMOOTHNESS_MODE;
  } else if (!decision.is_late) {
    decision.reason = NL_FRAME_DEADLINE_ON_TIME;
  } else if (!decision.backpressured) {
    decision.reason = NL_FRAME_DEADLINE_NOT_BACKPRESSURED;
  } else if (!input->newer_frame_queued) {
    decision.reason = NL_FRAME_DEADLINE_NO_NEWER_FRAME;
  } else if (!decision.cooldown_expired) {
    decision.reason = NL_FRAME_DEADLINE_COOLDOWN;
  } else if (!decision.severe_lateness && !decision.late_streak) {
    decision.reason = NL_FRAME_DEADLINE_LATE_ONLY;
  } else {
    decision.drop = true;
    decision.reason = NL_FRAME_DEADLINE_LATE_SUPERSEDED;
  }

  return decision;
}
