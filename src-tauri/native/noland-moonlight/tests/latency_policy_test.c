#include "noland_frame_deadline_policy.h"
#include "noland_latency_telemetry.h"
#include "noland_video_renderer.h"
#include "Limelight.h"

#include <assert.h>
#include <stdint.h>
#include <string.h>

static nl_frame_deadline_input_t eligible_input(uint32_t fps) {
  nl_frame_deadline_input_t input;
  memset(&input, 0, sizeof(input));
  input.feature_enabled = true;
  input.latency_priority_mode = true;
  input.estimated_frame_time_ns = nl_estimated_frame_time_ns(fps);
  input.jitter_tolerance_ns = nl_jitter_tolerance_ns(fps, 0);
  input.render_deadline_ns = 1000000000ULL;
  input.now_ns = input.render_deadline_ns + input.estimated_frame_time_ns * 2U;
  input.latest_decoder_full_buffer_ms = (input.estimated_frame_time_ns + 999999ULL) / 1000000ULL;
  input.newer_frame_queued = true;
  return input;
}

static void test_deadline_policy(void) {
  nl_frame_deadline_input_t input = eligible_input(60);
  nl_frame_deadline_decision_t decision;

  input.latest_decoder_full_buffer_ms = 0;
  decision = nl_decide_frame_deadline(&input);
  assert(decision.is_late);
  assert(!decision.drop);
  assert(decision.reason == NL_FRAME_DEADLINE_NOT_BACKPRESSURED);

  input = eligible_input(60);
  input.now_ns += 1U;
  decision = nl_decide_frame_deadline(&input);
  assert(decision.severe_lateness);
  assert(decision.drop);

  input = eligible_input(60);
  input.now_ns = input.render_deadline_ns + input.jitter_tolerance_ns + 1U;
  input.consecutive_late_frames = 3U;
  decision = nl_decide_frame_deadline(&input);
  assert(decision.late_streak);
  assert(decision.drop);

  input = eligible_input(60);
  input.newer_frame_queued = false;
  decision = nl_decide_frame_deadline(&input);
  assert(!decision.drop);
  assert(decision.reason == NL_FRAME_DEADLINE_NO_NEWER_FRAME);

  input = eligible_input(60);
  input.latency_priority_mode = false;
  decision = nl_decide_frame_deadline(&input);
  assert(!decision.drop);
  assert(decision.reason == NL_FRAME_DEADLINE_SMOOTHNESS_MODE);

  input = eligible_input(120);
  input.last_adaptive_drop_ns = input.now_ns - 1000000ULL;
  decision = nl_decide_frame_deadline(&input);
  assert(!decision.drop);
  assert(decision.reason == NL_FRAME_DEADLINE_COOLDOWN);

  assert(nl_estimated_frame_time_ns(60) == 16666666ULL);
  assert(nl_estimated_frame_time_ns(120) == 8333333ULL);
  assert(nl_estimated_frame_time_ns(144) == 6944444ULL);
  assert(nl_estimated_frame_time_ns(240) == 4166666ULL);
  assert(nl_estimated_frame_time_ns(0) == 0U);
  assert(nl_jitter_tolerance_ns(240, 0) == 2083333ULL);
  assert(nl_jitter_tolerance_ns(60, 7000) == 7000000ULL);
}

static void test_pacing_resolution(void) {
  nl_pacing_resolution_t resolution;

  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_AUTOMATIC, true, 60, 6000);
  assert(resolution.effective_mode == NL_PACING_MODE_HARDWARE_MULTIPLE);
  assert(resolution.sync_interval == 1U);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_AUTOMATIC, true, 60, 12000);
  assert(resolution.effective_mode == NL_PACING_MODE_HARDWARE_MULTIPLE);
  assert(resolution.sync_interval == 2U);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_AUTOMATIC, true, 60, 24000);
  assert(resolution.effective_mode == NL_PACING_MODE_HARDWARE_MULTIPLE);
  assert(resolution.sync_interval == 4U);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_AUTOMATIC, true, 120, 24000);
  assert(resolution.effective_mode == NL_PACING_MODE_HARDWARE_MULTIPLE);
  assert(resolution.sync_interval == 2U);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_AUTOMATIC, true, 60, 16500);
  assert(resolution.effective_mode == NL_PACING_MODE_SOFTWARE);
  assert(resolution.sync_interval == 0U);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_HARDWARE_MULTIPLE, true, 60, 16500);
  assert(resolution.effective_mode == NL_PACING_MODE_OFF);
  resolution = nl_resolve_pacing_mode(NL_PACING_MODE_SOFTWARE, false, 60, 6000);
  assert(resolution.effective_mode == NL_PACING_MODE_OFF);
}

static void test_bounded_telemetry_and_wrap(void) {
  nl_latency_telemetry_t telemetry;
  nl_latency_snapshot_t snapshot;
  nl_video_frame_metadata_t frame;
  RTP_VIDEO_STATS stats;
  uint32_t index;
  bool initialized;

  initialized = nl_latency_telemetry_init(&telemetry);
  assert(initialized);
  if (!initialized) {
    return;
  }
  nl_latency_telemetry_reset(&telemetry, true, 240, 0, 3);
  memset(&frame, 0, sizeof(frame));

  for (index = 0U; index < NL_FRAME_TIMING_CAPACITY + 500U; ++index) {
    frame.frame_number = (int32_t)index;
    frame.receive_time_us = 1000U + index;
    frame.enqueue_time_us = 900U + index;
    frame.presentation_time_us = 5000U + index;
    frame.rtp_timestamp = index + 1U;
    nl_latency_telemetry_record_decode_submit(&telemetry, &frame, 2000U + index, 2U);
  }
  assert(nl_latency_telemetry_record_count(&telemetry) == NL_FRAME_TIMING_CAPACITY);

  memset(&stats, 0, sizeof(stats));
  stats.packetCountVideo = UINT32_MAX - 2U;
  stats.packetCountFec = UINT32_MAX - 1U;
  stats.packetCountFecRecovered = UINT32_MAX;
  stats.packetCountFecFailed = UINT32_MAX - 3U;
  stats.packetCountOOS = UINT32_MAX - 4U;
  stats.packetCountInvalid = UINT32_MAX - 5U;
  stats.packetCountFecInvalid = UINT32_MAX - 6U;
  nl_latency_telemetry_sample_network(&telemetry, 1000000U, 4, &stats);
  stats.packetCountVideo = 3U;
  stats.packetCountFec = 2U;
  stats.packetCountFecRecovered = 1U;
  stats.packetCountFecFailed = 4U;
  stats.packetCountOOS = 5U;
  stats.packetCountInvalid = 6U;
  stats.packetCountFecInvalid = 7U;
  nl_latency_telemetry_sample_network(&telemetry, 1250000U, 1, &stats);
  nl_latency_telemetry_snapshot(&telemetry, &snapshot);
  assert(snapshot.video_packets_interval == 6U);
  assert(snapshot.fec_packets_interval == 4U);
  assert(snapshot.fec_recoveries_interval == 2U);
  assert(snapshot.fec_failures_interval == 8U);
  assert(snapshot.out_of_sequence_interval == 10U);
  assert(snapshot.invalid_packets_interval == 12U);
  assert(snapshot.invalid_fec_packets_interval == 14U);
  assert(snapshot.pending_core_video_frames == 1);
  assert(snapshot.ring_count == NL_FRAME_TIMING_CAPACITY);
  assert(snapshot.smoothing_queue_capacity == 3U);
  assert(snapshot.smoothing_reserve_budget_us == 12500U);

  nl_latency_telemetry_reset(&telemetry, true, 60, 0, 0);
  nl_latency_telemetry_snapshot(&telemetry, &snapshot);
  assert(snapshot.ring_count == 0U);
  assert(snapshot.video_packets_interval == 0U);
  assert(snapshot.fec_packets_interval == 0U);
  assert(snapshot.fec_recoveries_interval == 0U);
  assert(snapshot.fec_failures_interval == 0U);
  assert(snapshot.out_of_sequence_interval == 0U);
  assert(snapshot.invalid_packets_interval == 0U);
  assert(snapshot.invalid_fec_packets_interval == 0U);
  assert(snapshot.pending_core_video_frames == -1);

  frame.frame_number = 1;
  frame.receive_time_us = 2000U;
  frame.enqueue_time_us = 1000U;
  frame.presentation_time_us = 0U;
  frame.rtp_timestamp = 0U;
  nl_latency_telemetry_record_decode_submit(&telemetry, &frame, 500U, 0U);
  nl_latency_telemetry_record_decoder_output(&telemetry, 0U, 400U, 0U, false);
  nl_latency_telemetry_snapshot(&telemetry, &snapshot);
  assert(snapshot.average_decode_pipeline_us == 0U);

  nl_latency_telemetry_reset(&telemetry, false, 60, 0, 0);
  nl_latency_telemetry_record_decode_submit(&telemetry, &frame, 1000U, 0U);
  assert(nl_latency_telemetry_record_count(&telemetry) == 0U);

  memset(&stats, 0, sizeof(stats));
  stats.packetCountVideo = UINT32_MAX - 2U;
  stats.packetCountFec = UINT32_MAX - 1U;
  stats.packetCountFecRecovered = UINT32_MAX;
  stats.packetCountFecFailed = UINT32_MAX - 3U;
  stats.packetCountOOS = UINT32_MAX - 4U;
  stats.packetCountInvalid = UINT32_MAX - 5U;
  stats.packetCountFecInvalid = UINT32_MAX - 6U;
  nl_latency_telemetry_sample_network(&telemetry, 2000000U, 7, &stats);
  stats.packetCountVideo = 3U;
  stats.packetCountFec = 2U;
  stats.packetCountFecRecovered = 1U;
  stats.packetCountFecFailed = 4U;
  stats.packetCountOOS = 5U;
  stats.packetCountInvalid = 6U;
  stats.packetCountFecInvalid = 7U;
  nl_latency_telemetry_sample_network(&telemetry, 2250000U, 2, &stats);
  nl_latency_telemetry_snapshot(&telemetry, &snapshot);
  assert(snapshot.video_packets_interval == 6U);
  assert(snapshot.fec_packets_interval == 4U);
  assert(snapshot.fec_recoveries_interval == 2U);
  assert(snapshot.fec_failures_interval == 8U);
  assert(snapshot.out_of_sequence_interval == 10U);
  assert(snapshot.invalid_packets_interval == 12U);
  assert(snapshot.invalid_fec_packets_interval == 14U);
  assert(snapshot.pending_core_video_frames == 2);
  assert(snapshot.ring_count == 0U);
  assert(snapshot.late_frame_count == 0U);
  assert(snapshot.rendered_fps_x100 == 0U);
  nl_latency_telemetry_cleanup(&telemetry);
}

int main(void) {
  test_deadline_policy();
  test_pacing_resolution();
  test_bounded_telemetry_and_wrap();
  return 0;
}
