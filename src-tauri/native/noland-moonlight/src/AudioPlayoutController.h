#ifndef NOLAND_AUDIO_PLAYOUT_CONTROLLER_H
#define NOLAND_AUDIO_PLAYOUT_CONTROLLER_H

#include "AudioPcmSyncBuffer.h"
#include "Limelight.h"

#include <stdbool.h>
#include <stdint.h>

#include <opus/opus_multistream.h>

#define NL_AUDIO_MAX_COMPRESSED_PACKET_SIZE 1500

typedef struct nl_audio_jitter_packet {
  uint16_t sequence_number;
  uint32_t rtp_timestamp;
  uint64_t extended_rtp_timestamp;
  uint64_t arrival_time_us;
  uint16_t payload_length;
  bool plc_placeholder;
  unsigned char payload[NL_AUDIO_MAX_COMPRESSED_PACKET_SIZE];
} nl_audio_jitter_packet_t;

typedef struct nl_audio_playout_controller {
  OpusMSDecoder* decoder;
  float* decode_scratch;
  nl_audio_pcm_sync_buffer_t pcm_buffer;
  nl_audio_jitter_packet_t* packet_queue;
  uint32_t packet_queue_capacity;
  uint32_t packet_queue_count;
  uint32_t sample_rate;
  uint32_t channel_count;
  uint32_t samples_per_frame;
  uint32_t minimum_target_frames;
  uint32_t target_buffer_frames;
  uint32_t current_target_frames;
  uint32_t maximum_buffer_frames;
  uint32_t startup_prefill_frames;
  uint32_t stable_high_watermark_counter;
  uint64_t decoded_packet_count;
  uint64_t concealment_count;
  uint64_t dropped_frames_count;
  uint64_t starvation_event_count;
  uint64_t target_increase_count;
  uint64_t target_decrease_count;
} nl_audio_playout_controller_t;

bool nl_audio_playout_controller_init(nl_audio_playout_controller_t* controller,
                                      const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                                      uint32_t target_buffer_ms,
                                      uint32_t maximum_buffer_ms);
void nl_audio_playout_controller_reset(nl_audio_playout_controller_t* controller);
void nl_audio_playout_controller_cleanup(nl_audio_playout_controller_t* controller);
int nl_audio_playout_controller_submit_packet(nl_audio_playout_controller_t* controller,
                                              const char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us);
uint32_t nl_audio_playout_controller_available_frames(const nl_audio_playout_controller_t* controller);
uint32_t nl_audio_playout_controller_target_frames(const nl_audio_playout_controller_t* controller);
uint32_t nl_audio_playout_controller_current_target_frames(const nl_audio_playout_controller_t* controller);
uint64_t nl_audio_playout_controller_concealment_count(const nl_audio_playout_controller_t* controller);
uint64_t nl_audio_playout_controller_dropped_frames_count(const nl_audio_playout_controller_t* controller);
uint64_t nl_audio_playout_controller_starvation_event_count(const nl_audio_playout_controller_t* controller);
uint64_t nl_audio_playout_controller_target_increase_count(const nl_audio_playout_controller_t* controller);
uint64_t nl_audio_playout_controller_target_decrease_count(const nl_audio_playout_controller_t* controller);
void nl_audio_playout_controller_note_starvation(nl_audio_playout_controller_t* controller);
void nl_audio_playout_controller_note_buffer_level(nl_audio_playout_controller_t* controller,
                                                   uint32_t effective_lead_frames);
uint32_t nl_audio_playout_controller_maximum_frames(const nl_audio_playout_controller_t* controller);
uint32_t nl_audio_playout_controller_startup_prefill_frames(const nl_audio_playout_controller_t* controller);
uint32_t nl_audio_playout_controller_queued_packet_frames(const nl_audio_playout_controller_t* controller);
uint32_t nl_audio_playout_controller_fill_frames(nl_audio_playout_controller_t* controller,
                                                 uint32_t minimum_frames,
                                                 bool allow_concealment);
uint32_t nl_audio_playout_controller_pop_frames(nl_audio_playout_controller_t* controller,
                                                float* interleaved_samples,
                                                uint32_t requested_frames);

#endif
