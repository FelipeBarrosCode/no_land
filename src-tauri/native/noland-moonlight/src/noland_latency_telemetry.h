#ifndef NOLAND_LATENCY_TELEMETRY_H
#define NOLAND_LATENCY_TELEMETRY_H

#include "noland_moonlight.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nl_video_frame_metadata nl_video_frame_metadata_t;

#define NL_FRAME_TIMING_CAPACITY 1200U

#define NL_FRAME_TIMING_VALID_RECEIVE_TIME (1U << 0)
#define NL_FRAME_TIMING_VALID_CORE_ENQUEUE_TIME (1U << 1)
#define NL_FRAME_TIMING_VALID_DECODER_SUBMIT_TIME (1U << 2)
#define NL_FRAME_TIMING_VALID_DECODER_OUTPUT_TIME (1U << 3)
#define NL_FRAME_TIMING_VALID_PRESENTATION_TIME (1U << 4)
#define NL_FRAME_TIMING_VALID_RENDER_SUBMIT_TIME (1U << 5)

typedef enum nl_frame_drop_reason {
  NL_FRAME_DROP_NONE = 0,
  NL_FRAME_DROP_CORE_NETWORK_LOSS = 1,
  NL_FRAME_DROP_CORE_JITTER = 2,
  NL_FRAME_DROP_DECODER_FAILURE = 3,
  NL_FRAME_DROP_LATE_SUPERSEDED = 4,
  NL_FRAME_DROP_PACER_BACKLOG = 5,
  NL_FRAME_DROP_RENDERER_ERROR = 6,
  NL_FRAME_DROP_SMOOTHING_OVERFLOW = 7
} nl_frame_drop_reason_t;

typedef struct nl_frame_timing {
  uint32_t frame_number;
  uint32_t validity;
  uint64_t receive_time_us;
  uint64_t core_enqueue_time_us;
  uint64_t decoder_submit_time_us;
  uint64_t decoder_output_time_us;
  uint64_t presentation_time_us;
  uint64_t render_submit_time_us;
  uint64_t lateness_us;
  uint16_t host_processing_latency_tenth_ms;
  uint16_t decoder_queue_depth_at_submit;
  uint16_t render_queue_depth_at_output;
  uint8_t decoder_back_pressured;
  uint8_t classified_late;
  uint8_t dropped_as_superseded;
  uint8_t drop_reason;
} nl_frame_timing_t;

typedef struct nl_latency_snapshot {
  uint32_t video_packets_interval;
  uint32_t fec_packets_interval;
  uint32_t fec_recoveries_interval;
  uint32_t fec_failures_interval;
  uint32_t out_of_sequence_interval;
  uint32_t invalid_packets_interval;
  uint32_t invalid_fec_packets_interval;
  int32_t pending_core_video_frames;
  uint16_t decoder_queue_depth;
  uint16_t render_queue_depth;
  uint64_t average_decode_pipeline_us;
  uint64_t average_render_queue_dwell_us;
  uint64_t late_frame_count;
  uint64_t adaptive_stale_drop_count;
  uint64_t pacer_backlog_drop_count;
  uint64_t renderer_error_drop_count;
  uint64_t maximum_lateness_us;
  uint64_t decoder_backpressure_time_us;
  uint64_t last_drop_lateness_us;
  uint32_t rendered_fps_x100;
  uint32_t consecutive_late_frames;
  uint32_t late_tolerance_us;
  uint8_t decoder_backpressured;
  uint8_t smoothing_queue_depth;
  uint8_t smoothing_queue_capacity;
  uint8_t max_smoothing_queue_depth;
  uint64_t smoothing_overflow_drops;
  uint64_t smoothing_underflow_repeats;
  uint64_t smoothing_reserve_budget_us;
  nl_pacing_mode_t configured_pacing_mode;
  nl_pacing_mode_t effective_pacing_mode;
  uint32_t ring_count;
} nl_latency_snapshot_t;

typedef struct nl_latency_telemetry {
  nl_frame_timing_t records[NL_FRAME_TIMING_CAPACITY];
  size_t next_record;
  size_t record_count;
  uint64_t late_frame_count;
  uint64_t adaptive_stale_drop_count;
  uint64_t pacer_backlog_drop_count;
  uint64_t renderer_error_drop_count;
  uint64_t maximum_lateness_us;
  uint64_t last_drop_lateness_us;
  uint64_t decoder_backpressure_time_us;
  uint64_t rendered_frame_count;
  uint64_t last_sample_rendered_frame_count;
  uint64_t last_sample_time_us;
  uint32_t rendered_fps_x100;
  uint32_t consecutive_late_frames;
  uint32_t late_tolerance_us;
  uint16_t decoder_queue_depth;
  uint16_t render_queue_depth;
  uint8_t decoder_backpressured;
  uint8_t smoothing_queue_depth;
  uint8_t smoothing_queue_capacity;
  uint8_t max_smoothing_queue_depth;
  uint64_t smoothing_overflow_drops;
  uint64_t smoothing_underflow_repeats;
  uint64_t smoothing_reserve_budget_us;
  nl_pacing_mode_t configured_pacing_mode;
  nl_pacing_mode_t effective_pacing_mode;
  uint32_t previous_packet_count_video;
  uint32_t previous_packet_count_fec;
  uint32_t previous_packet_count_fec_recovered;
  uint32_t previous_packet_count_fec_failed;
  uint32_t previous_packet_count_oos;
  uint32_t previous_packet_count_invalid;
  uint32_t previous_packet_count_fec_invalid;
  uint32_t video_packets_interval;
  uint32_t fec_packets_interval;
  uint32_t fec_recoveries_interval;
  uint32_t fec_failures_interval;
  uint32_t out_of_sequence_interval;
  uint32_t invalid_packets_interval;
  uint32_t invalid_fec_packets_interval;
  int32_t pending_core_video_frames;
  uint8_t has_network_baseline;
  uint8_t enabled;
  void* lock;
} nl_latency_telemetry_t;

bool nl_latency_telemetry_init(nl_latency_telemetry_t* telemetry);
void nl_latency_telemetry_cleanup(nl_latency_telemetry_t* telemetry);
void nl_latency_telemetry_reset(nl_latency_telemetry_t* telemetry, bool enabled, uint32_t stream_fps, uint32_t late_tolerance_us, uint8_t smoothing_capacity);
void nl_latency_telemetry_record_decode_submit(nl_latency_telemetry_t* telemetry, const nl_video_frame_metadata_t* frame, uint64_t now_us, uint16_t decoder_queue_depth);
void nl_latency_telemetry_record_decoder_output(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t now_us, uint16_t render_queue_depth, bool backpressured);
void nl_latency_telemetry_record_render_submit(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t now_us);
void nl_latency_telemetry_record_late(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t lateness_us, uint32_t consecutive_late_frames);
void nl_latency_telemetry_record_drop(nl_latency_telemetry_t* telemetry, uint64_t presentation_time_us, uint64_t lateness_us, nl_frame_drop_reason_t reason);
void nl_latency_telemetry_record_backpressure(nl_latency_telemetry_t* telemetry, uint64_t duration_us, bool active);
void nl_latency_telemetry_set_queue_depths(nl_latency_telemetry_t* telemetry, uint16_t decoder_queue_depth, uint16_t render_queue_depth);
void nl_latency_telemetry_set_smoothing(nl_latency_telemetry_t* telemetry, uint8_t depth, uint8_t capacity, uint64_t overflow_drops, uint64_t underflow_repeats, uint32_t stream_fps);
void nl_latency_telemetry_set_pacing(nl_latency_telemetry_t* telemetry, nl_pacing_mode_t configured, nl_pacing_mode_t effective);
void nl_latency_telemetry_sample_network(nl_latency_telemetry_t* telemetry, uint64_t now_us, int32_t pending_core_video_frames, const void* rtp_video_stats);
void nl_latency_telemetry_snapshot(nl_latency_telemetry_t* telemetry, nl_latency_snapshot_t* output);
size_t nl_latency_telemetry_record_count(nl_latency_telemetry_t* telemetry);

#ifdef __cplusplus
}
#endif

#endif
