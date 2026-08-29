#include "noland_latency_telemetry.h"
#include "noland_video_renderer.h"
#include "Limelight.h"

#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
typedef CRITICAL_SECTION nl_telemetry_mutex_t;
#else
#include <pthread.h>
typedef pthread_mutex_t nl_telemetry_mutex_t;
#endif

static void nl_telemetry_lock(nl_latency_telemetry_t* telemetry) {
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }
#if defined(_WIN32)
  EnterCriticalSection((nl_telemetry_mutex_t*)telemetry->lock);
#else
  pthread_mutex_lock((nl_telemetry_mutex_t*)telemetry->lock);
#endif
}

static void nl_telemetry_unlock(nl_latency_telemetry_t* telemetry) {
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }
#if defined(_WIN32)
  LeaveCriticalSection((nl_telemetry_mutex_t*)telemetry->lock);
#else
  pthread_mutex_unlock((nl_telemetry_mutex_t*)telemetry->lock);
#endif
}


static nl_frame_timing_t* nl_find_record_locked(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us) {
  size_t offset;
  if (telemetry == NULL || telemetry->record_count == 0U) {
    return NULL;
  }

  for (offset = 0U; offset < telemetry->record_count; ++offset) {
    size_t index = (telemetry->next_record + NL_FRAME_TIMING_CAPACITY - 1U - offset) % NL_FRAME_TIMING_CAPACITY;
    nl_frame_timing_t* record = &telemetry->records[index];
    if (record->presentation_time_us == presentation_time_us &&
        (record->validity & NL_FRAME_TIMING_VALID_PRESENTATION_TIME) != 0U) {
      return record;
    }
  }
  return NULL;
}

bool nl_latency_telemetry_init(nl_latency_telemetry_t* telemetry) {
  nl_telemetry_mutex_t* mutex;
  if (telemetry == NULL) {
    return false;
  }
  memset(telemetry, 0, sizeof(*telemetry));
  mutex = (nl_telemetry_mutex_t*)malloc(sizeof(*mutex));
  if (mutex == NULL) {
    return false;
  }
#if defined(_WIN32)
  InitializeCriticalSection(mutex);
#else
  if (pthread_mutex_init(mutex, NULL) != 0) {
    free(mutex);
    return false;
  }
#endif
  telemetry->lock = mutex;
  telemetry->pending_core_video_frames = -1;
  return true;
}

void nl_latency_telemetry_cleanup(nl_latency_telemetry_t* telemetry) {
  nl_telemetry_mutex_t* mutex;
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }
  mutex = (nl_telemetry_mutex_t*)telemetry->lock;
#if defined(_WIN32)
  DeleteCriticalSection(mutex);
#else
  pthread_mutex_destroy(mutex);
#endif
  free(mutex);
  memset(telemetry, 0, sizeof(*telemetry));
}

void nl_latency_telemetry_reset(nl_latency_telemetry_t* telemetry, bool enabled, uint32_t stream_fps, uint32_t late_tolerance_us, uint8_t smoothing_capacity) {
  void* lock;
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }
  nl_telemetry_lock(telemetry);
  lock = telemetry->lock;
  memset(telemetry, 0, sizeof(*telemetry));
  telemetry->lock = lock;
  telemetry->enabled = enabled ? 1U : 0U;
  telemetry->late_tolerance_us = late_tolerance_us != 0U
      ? late_tolerance_us
      : (stream_fps != 0U ? (500000U / stream_fps) : 0U);
  telemetry->smoothing_queue_capacity = smoothing_capacity > 3U ? 3U : smoothing_capacity;
  telemetry->smoothing_reserve_budget_us = stream_fps != 0U
      ? ((uint64_t)telemetry->smoothing_queue_capacity * 1000000ULL) / stream_fps
      : 0U;
  telemetry->pending_core_video_frames = -1;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_decode_submit(nl_latency_telemetry_t* telemetry, const nl_video_frame_metadata_t* frame, uint64_t now_us, uint16_t decoder_queue_depth) {
  nl_frame_timing_t* record;
  if (telemetry == NULL || frame == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  record = &telemetry->records[telemetry->next_record];
  memset(record, 0, sizeof(*record));
  record->frame_number = (uint32_t)frame->frame_number;
  record->host_processing_latency_tenth_ms = frame->host_processing_latency;
  record->decoder_queue_depth_at_submit = decoder_queue_depth;
  if (frame->receive_time_us != 0U) {
    record->receive_time_us = frame->receive_time_us;
    record->validity |= NL_FRAME_TIMING_VALID_RECEIVE_TIME;
  }
  if (frame->enqueue_time_us != 0U) {
    record->core_enqueue_time_us = frame->enqueue_time_us;
    record->validity |= NL_FRAME_TIMING_VALID_CORE_ENQUEUE_TIME;
  }
  if (now_us != 0U) {
    record->decoder_submit_time_us = now_us;
    record->validity |= NL_FRAME_TIMING_VALID_DECODER_SUBMIT_TIME;
  }
  if (frame->presentation_time_us != 0U || frame->rtp_timestamp != 0U) {
    record->presentation_time_us = frame->presentation_time_us;
    record->validity |= NL_FRAME_TIMING_VALID_PRESENTATION_TIME;
  }
  telemetry->decoder_queue_depth = decoder_queue_depth;
  telemetry->next_record = (telemetry->next_record + 1U) % NL_FRAME_TIMING_CAPACITY;
  if (telemetry->record_count < NL_FRAME_TIMING_CAPACITY) {
    telemetry->record_count += 1U;
  }
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_decoder_output(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t now_us, uint16_t render_queue_depth, bool backpressured) {
  nl_frame_timing_t* record;
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  record = nl_find_record_locked(telemetry, presentation_time_us);
  if (record != NULL) {
    record->decoder_output_time_us = now_us;
    record->render_queue_depth_at_output = render_queue_depth;
    record->decoder_back_pressured = backpressured ? 1U : 0U;
    if (now_us != 0U) {
      record->validity |= NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME;
    }
  }
  telemetry->render_queue_depth = render_queue_depth;
  telemetry->decoder_backpressured = backpressured ? 1U : 0U;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_render_submit(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t now_us) {
  nl_frame_timing_t* record;
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  record = nl_find_record_locked(telemetry, presentation_time_us);
  if (record != NULL) {
    record->render_submit_time_us = now_us;
    if (now_us != 0U) {
      record->validity |= NL_FRAME_TIMING_VALID_RENDER_SUBMIT_TIME;
    }
  }
  telemetry->rendered_frame_count += 1U;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_late(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t lateness_us, uint32_t consecutive_late_frames) {
  nl_frame_timing_t* record;
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  record = nl_find_record_locked(telemetry, presentation_time_us);
  if (record != NULL) {
    record->classified_late = 1U;
    record->lateness_us = lateness_us;
  }
  telemetry->late_frame_count += 1U;
  telemetry->consecutive_late_frames = consecutive_late_frames;
  if (lateness_us > telemetry->maximum_lateness_us) {
    telemetry->maximum_lateness_us = lateness_us;
  }
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_drop(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t lateness_us, nl_frame_drop_reason_t reason) {
  nl_frame_timing_t* record;
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  record = nl_find_record_locked(telemetry, presentation_time_us);
  if (record != NULL) {
    record->drop_reason = (uint8_t)reason;
    record->lateness_us = lateness_us;
    if (reason == NL_FRAME_DROP_LATE_SUPERSEDED) {
      record->dropped_as_superseded = 1U;
    }
  }
  switch (reason) {
    case NL_FRAME_DROP_LATE_SUPERSEDED:
      telemetry->adaptive_stale_drop_count += 1U;
      telemetry->last_drop_lateness_us = lateness_us;
      break;
    case NL_FRAME_DROP_PACER_BACKLOG:
      telemetry->pacer_backlog_drop_count += 1U;
      break;
    case NL_FRAME_DROP_RENDERER_ERROR:
    case NL_FRAME_DROP_DECODER_FAILURE:
      telemetry->renderer_error_drop_count += 1U;
      break;
    default:
      break;
  }
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_record_backpressure(nl_latency_telemetry_t* telemetry, uint64_t duration_us, bool active) {
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  telemetry->decoder_backpressure_time_us += duration_us;
  telemetry->decoder_backpressured = active ? 1U : 0U;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_set_queue_depths(nl_latency_telemetry_t* telemetry, uint16_t decoder_queue_depth, uint16_t render_queue_depth) {
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  telemetry->decoder_queue_depth = decoder_queue_depth;
  telemetry->render_queue_depth = render_queue_depth;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_set_smoothing(nl_latency_telemetry_t* telemetry, uint8_t depth, uint8_t capacity, uint64_t overflow_drops, uint64_t underflow_repeats, uint32_t stream_fps) {
  if (telemetry == NULL || telemetry->enabled == 0U) {
    return;
  }
  nl_telemetry_lock(telemetry);
  telemetry->smoothing_queue_capacity = capacity > 3U ? 3U : capacity;
  telemetry->smoothing_queue_depth = depth > telemetry->smoothing_queue_capacity ? telemetry->smoothing_queue_capacity : depth;
  if (telemetry->smoothing_queue_depth > telemetry->max_smoothing_queue_depth) {
    telemetry->max_smoothing_queue_depth = telemetry->smoothing_queue_depth;
  }
  telemetry->smoothing_overflow_drops = overflow_drops;
  telemetry->smoothing_underflow_repeats = underflow_repeats;
  telemetry->smoothing_reserve_budget_us = stream_fps != 0U
      ? ((uint64_t)telemetry->smoothing_queue_capacity * 1000000ULL) / stream_fps
      : 0U;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_set_pacing(nl_latency_telemetry_t* telemetry, nl_pacing_mode_t configured, nl_pacing_mode_t effective) {
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }
  nl_telemetry_lock(telemetry);
  telemetry->configured_pacing_mode = configured;
  telemetry->effective_pacing_mode = effective;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_sample_network(nl_latency_telemetry_t* telemetry, uint64_t now_us, int32_t pending_core_video_frames, const void* rtp_video_stats) {
  const RTP_VIDEO_STATS* stats = (const RTP_VIDEO_STATS*)rtp_video_stats;
  if (telemetry == NULL || telemetry->lock == NULL || stats == NULL) {
    return;
  }
  nl_telemetry_lock(telemetry);
  telemetry->pending_core_video_frames = pending_core_video_frames;
  if (telemetry->has_network_baseline != 0U) {
    telemetry->video_packets_interval = stats->packetCountVideo - telemetry->previous_packet_count_video;
    telemetry->fec_packets_interval = stats->packetCountFec - telemetry->previous_packet_count_fec;
    telemetry->fec_recoveries_interval = stats->packetCountFecRecovered - telemetry->previous_packet_count_fec_recovered;
    telemetry->fec_failures_interval = stats->packetCountFecFailed - telemetry->previous_packet_count_fec_failed;
    telemetry->out_of_sequence_interval = stats->packetCountOOS - telemetry->previous_packet_count_oos;
    telemetry->invalid_packets_interval = stats->packetCountInvalid - telemetry->previous_packet_count_invalid;
    telemetry->invalid_fec_packets_interval = stats->packetCountFecInvalid - telemetry->previous_packet_count_fec_invalid;
  } else {
    telemetry->has_network_baseline = 1U;
  }
  telemetry->previous_packet_count_video = stats->packetCountVideo;
  telemetry->previous_packet_count_fec = stats->packetCountFec;
  telemetry->previous_packet_count_fec_recovered = stats->packetCountFecRecovered;
  telemetry->previous_packet_count_fec_failed = stats->packetCountFecFailed;
  telemetry->previous_packet_count_oos = stats->packetCountOOS;
  telemetry->previous_packet_count_invalid = stats->packetCountInvalid;
  telemetry->previous_packet_count_fec_invalid = stats->packetCountFecInvalid;

  if (telemetry->last_sample_time_us != 0U && now_us > telemetry->last_sample_time_us) {
    uint64_t rendered_delta = telemetry->rendered_frame_count - telemetry->last_sample_rendered_frame_count;
    uint64_t elapsed_us = now_us - telemetry->last_sample_time_us;
    telemetry->rendered_fps_x100 = (uint32_t)((rendered_delta * 100000000ULL) / elapsed_us);
  }
  telemetry->last_sample_time_us = now_us;
  telemetry->last_sample_rendered_frame_count = telemetry->rendered_frame_count;
  nl_telemetry_unlock(telemetry);
}

void nl_latency_telemetry_snapshot(nl_latency_telemetry_t* telemetry, nl_latency_snapshot_t* output) {
  uint64_t decode_total_us = 0U;
  uint64_t dwell_total_us = 0U;
  uint64_t decode_count = 0U;
  uint64_t dwell_count = 0U;
  size_t offset;
  if (output == NULL) {
    return;
  }
  memset(output, 0, sizeof(*output));
  output->pending_core_video_frames = -1;
  if (telemetry == NULL || telemetry->lock == NULL) {
    return;
  }

  nl_telemetry_lock(telemetry);
  for (offset = 0U; offset < telemetry->record_count; ++offset) {
    const nl_frame_timing_t* record = &telemetry->records[offset];
    if ((record->validity & (NL_FRAME_TIMING_VALID_DECODER_SUBMIT_TIME | NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME)) ==
        (NL_FRAME_TIMING_VALID_DECODER_SUBMIT_TIME | NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME) &&
        record->decoder_output_time_us >= record->decoder_submit_time_us) {
      decode_total_us += record->decoder_output_time_us - record->decoder_submit_time_us;
      decode_count += 1U;
    }
    if ((record->validity & (NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME | NL_FRAME_TIMING_VALID_RENDER_SUBMIT_TIME)) ==
        (NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME | NL_FRAME_TIMING_VALID_RENDER_SUBMIT_TIME) &&
        record->render_submit_time_us >= record->decoder_output_time_us) {
      dwell_total_us += record->render_submit_time_us - record->decoder_output_time_us;
      dwell_count += 1U;
    }
  }

  output->video_packets_interval = telemetry->video_packets_interval;
  output->fec_packets_interval = telemetry->fec_packets_interval;
  output->fec_recoveries_interval = telemetry->fec_recoveries_interval;
  output->fec_failures_interval = telemetry->fec_failures_interval;
  output->out_of_sequence_interval = telemetry->out_of_sequence_interval;
  output->invalid_packets_interval = telemetry->invalid_packets_interval;
  output->invalid_fec_packets_interval = telemetry->invalid_fec_packets_interval;
  output->pending_core_video_frames = telemetry->pending_core_video_frames;
  output->decoder_queue_depth = telemetry->decoder_queue_depth;
  output->render_queue_depth = telemetry->render_queue_depth;
  output->average_decode_pipeline_us = decode_count != 0U ? decode_total_us / decode_count : 0U;
  output->average_render_queue_dwell_us = dwell_count != 0U ? dwell_total_us / dwell_count : 0U;
  output->late_frame_count = telemetry->late_frame_count;
  output->adaptive_stale_drop_count = telemetry->adaptive_stale_drop_count;
  output->pacer_backlog_drop_count = telemetry->pacer_backlog_drop_count;
  output->renderer_error_drop_count = telemetry->renderer_error_drop_count;
  output->maximum_lateness_us = telemetry->maximum_lateness_us;
  output->decoder_backpressure_time_us = telemetry->decoder_backpressure_time_us;
  output->last_drop_lateness_us = telemetry->last_drop_lateness_us;
  output->rendered_fps_x100 = telemetry->rendered_fps_x100;
  output->consecutive_late_frames = telemetry->consecutive_late_frames;
  output->late_tolerance_us = telemetry->late_tolerance_us;
  output->decoder_backpressured = telemetry->decoder_backpressured;
  output->smoothing_queue_depth = telemetry->smoothing_queue_depth;
  output->smoothing_queue_capacity = telemetry->smoothing_queue_capacity;
  output->max_smoothing_queue_depth = telemetry->max_smoothing_queue_depth;
  output->smoothing_overflow_drops = telemetry->smoothing_overflow_drops;
  output->smoothing_underflow_repeats = telemetry->smoothing_underflow_repeats;
  output->smoothing_reserve_budget_us = telemetry->smoothing_reserve_budget_us;
  output->configured_pacing_mode = telemetry->configured_pacing_mode;
  output->effective_pacing_mode = telemetry->effective_pacing_mode;
  output->ring_count = (uint32_t)telemetry->record_count;
  nl_telemetry_unlock(telemetry);
}

size_t nl_latency_telemetry_record_count(nl_latency_telemetry_t* telemetry) {
  size_t count = 0U;
  if (telemetry == NULL || telemetry->lock == NULL) {
    return 0U;
  }
  nl_telemetry_lock(telemetry);
  count = telemetry->record_count;
  nl_telemetry_unlock(telemetry);
  return count;
}
