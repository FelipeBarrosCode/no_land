#include "AudioPlayoutController.h"

#include "AudioMasterClock.h"

#include <stdlib.h>
#include <string.h>

static uint32_t nl_ms_to_frames(uint32_t sample_rate, uint32_t milliseconds) {
  return (uint32_t)(((uint64_t)sample_rate * (uint64_t)milliseconds) / 1000ULL);
}

static uint32_t nl_packet_queue_capacity_for_controller(const nl_audio_playout_controller_t* controller) {
  uint32_t base_packets;

  if (controller == NULL || controller->samples_per_frame == 0) {
    return 0;
  }

  base_packets = controller->maximum_buffer_frames / controller->samples_per_frame;
  if ((controller->maximum_buffer_frames % controller->samples_per_frame) != 0) {
    base_packets += 1U;
  }

  base_packets += 16U;
  if (base_packets < 32U) {
    base_packets = 32U;
  }

  return base_packets;
}

static int nl_packet_compare(const nl_audio_jitter_packet_t* lhs,
                             const nl_audio_jitter_packet_t* rhs) {
  if (lhs->extended_rtp_timestamp < rhs->extended_rtp_timestamp) {
    return -1;
  }
  if (lhs->extended_rtp_timestamp > rhs->extended_rtp_timestamp) {
    return 1;
  }
  if (lhs->sequence_number < rhs->sequence_number) {
    return -1;
  }
  if (lhs->sequence_number > rhs->sequence_number) {
    return 1;
  }
  return 0;
}

static void nl_advance_expected_packet(nl_audio_playout_controller_t* controller,
                                       uint16_t sequence_number,
                                       uint64_t extended_rtp_timestamp) {
  if (controller == NULL) {
    return;
  }

  controller->expected_sequence_number = (uint16_t)(sequence_number + 1U);
  controller->expected_extended_rtp_timestamp = extended_rtp_timestamp + (uint64_t)controller->samples_per_frame;
  controller->has_expected_packet = true;
}

static bool nl_packet_is_obsolete(const nl_audio_playout_controller_t* controller,
                                  const nl_audio_jitter_packet_t* packet) {
  uint64_t audio_now_us;
  uint64_t packet_end_media_time_us;
  const uint64_t late_grace_us = 5000ULL;

  if (controller == NULL || packet == NULL || !AudioClockIsValid()) {
    return false;
  }

  audio_now_us = AudioClockGetMediaTimeUs();
  packet_end_media_time_us = AudioClockMediaTimeUsForExtendedTimestamp(
      packet->extended_rtp_timestamp + (uint64_t)controller->samples_per_frame);

  if (packet_end_media_time_us == 0) {
    return false;
  }

  return packet_end_media_time_us + late_grace_us <= audio_now_us;
}

static void nl_drop_head_packet(nl_audio_playout_controller_t* controller) {
  if (controller == NULL || controller->packet_queue_count == 0) {
    return;
  }

  if (controller->packet_queue_count > 1U) {
    memmove(&controller->packet_queue[0],
            &controller->packet_queue[1],
            (size_t)(controller->packet_queue_count - 1U) * sizeof(controller->packet_queue[0]));
  }
  controller->packet_queue_count -= 1U;
  controller->dropped_frames_count += controller->samples_per_frame;
}

static bool nl_pop_head_packet(nl_audio_playout_controller_t* controller,
                               nl_audio_jitter_packet_t* packet) {
  if (controller == NULL || packet == NULL || controller->packet_queue_count == 0) {
    return false;
  }

  *packet = controller->packet_queue[0];
  if (controller->packet_queue_count > 1U) {
    memmove(&controller->packet_queue[0],
            &controller->packet_queue[1],
            (size_t)(controller->packet_queue_count - 1U) * sizeof(controller->packet_queue[0]));
  }
  controller->packet_queue_count -= 1U;
  return true;
}

static void nl_write_decoded_samples(nl_audio_playout_controller_t* controller,
                                     uint32_t decoded_samples) {
  uint32_t free_frames;
  uint32_t frames_to_drop;

  if (controller == NULL || decoded_samples == 0) {
    return;
  }

  free_frames = nl_audio_pcm_sync_buffer_free_frames(&controller->pcm_buffer);
  if (free_frames < decoded_samples) {
    frames_to_drop = decoded_samples - free_frames;
    controller->dropped_frames_count +=
        nl_audio_pcm_sync_buffer_discard_frames(&controller->pcm_buffer, frames_to_drop);
  }

  nl_audio_pcm_sync_buffer_write(&controller->pcm_buffer,
                                 controller->decode_scratch,
                                 decoded_samples);
}

static int nl_decode_packet_into_pcm(nl_audio_playout_controller_t* controller,
                                     const nl_audio_jitter_packet_t* packet) {
  int decoded_samples;
  const unsigned char* payload = NULL;
  int payload_length = 0;

  if (controller == NULL || controller->decoder == NULL || controller->decode_scratch == NULL ||
      packet == NULL) {
    return -1;
  }

  AudioClockObservePacket(packet->sequence_number,
                          packet->rtp_timestamp,
                          packet->extended_rtp_timestamp,
                          packet->arrival_time_us);

  if (!packet->plc_placeholder) {
    payload = packet->payload;
    payload_length = (int)packet->payload_length;
  }

  decoded_samples = opus_multistream_decode_float(controller->decoder,
                                                  payload,
                                                  payload_length,
                                                  controller->decode_scratch,
                                                  (int)controller->samples_per_frame,
                                                  0);
  if (decoded_samples <= 0) {
    return decoded_samples;
  }

  controller->decoded_packet_count += 1U;
  if (packet->plc_placeholder) {
    controller->concealment_count += 1U;
  }

  nl_write_decoded_samples(controller, (uint32_t)decoded_samples);
  return decoded_samples;
}

static bool nl_generate_plc_frame(nl_audio_playout_controller_t* controller) {
  nl_audio_jitter_packet_t plc_packet;

  if (controller == NULL || controller->decoded_packet_count == 0 || !controller->has_expected_packet) {
    return false;
  }

  memset(&plc_packet, 0, sizeof(plc_packet));
  plc_packet.plc_placeholder = true;
  plc_packet.sequence_number = controller->expected_sequence_number;
  plc_packet.extended_rtp_timestamp = controller->expected_extended_rtp_timestamp;
  plc_packet.rtp_timestamp = (uint32_t)(plc_packet.extended_rtp_timestamp & 0xFFFFFFFFU);
  plc_packet.arrival_time_us = LiGetMicroseconds();

  if (nl_decode_packet_into_pcm(controller, &plc_packet) <= 0) {
    return false;
  }

  nl_advance_expected_packet(controller,
                             plc_packet.sequence_number,
                             plc_packet.extended_rtp_timestamp);
  return true;
}

bool nl_audio_playout_controller_init(nl_audio_playout_controller_t* controller,
                                      const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                                      uint32_t target_buffer_ms,
                                      uint32_t maximum_buffer_ms) {
  int error = OPUS_OK;
  uint32_t extra_capacity_frames;
  uint32_t pcm_capacity_frames;

  if (controller == NULL || opus_config == NULL || opus_config->sampleRate <= 0 ||
      opus_config->channelCount <= 0 || opus_config->samplesPerFrame <= 0) {
    return false;
  }

  memset(controller, 0, sizeof(*controller));
  controller->sample_rate = (uint32_t)opus_config->sampleRate;
  controller->channel_count = (uint32_t)opus_config->channelCount;
  controller->samples_per_frame = (uint32_t)opus_config->samplesPerFrame;
  controller->target_buffer_frames =
      nl_ms_to_frames(controller->sample_rate, target_buffer_ms > 0 ? target_buffer_ms : 20U);
  controller->maximum_buffer_frames =
      nl_ms_to_frames(controller->sample_rate, maximum_buffer_ms > 0 ? maximum_buffer_ms : 80U);
  if (controller->maximum_buffer_frames < controller->target_buffer_frames) {
    controller->maximum_buffer_frames = controller->target_buffer_frames;
  }
  controller->minimum_target_frames = controller->samples_per_frame;
  if (controller->minimum_target_frames > controller->target_buffer_frames) {
    controller->minimum_target_frames = controller->target_buffer_frames;
  }
  controller->current_target_frames = controller->target_buffer_frames;
  controller->startup_prefill_frames = controller->current_target_frames;
  if (controller->startup_prefill_frames < controller->samples_per_frame) {
    controller->startup_prefill_frames = controller->samples_per_frame;
  }

  controller->decoder = opus_multistream_decoder_create(opus_config->sampleRate,
                                                        opus_config->channelCount,
                                                        opus_config->streams,
                                                        opus_config->coupledStreams,
                                                        opus_config->mapping,
                                                        &error);
  if (controller->decoder == NULL || error != OPUS_OK) {
    nl_audio_playout_controller_cleanup(controller);
    return false;
  }

  controller->decode_scratch = (float*)calloc((size_t)controller->samples_per_frame * controller->channel_count,
                                              sizeof(float));
  if (controller->decode_scratch == NULL) {
    nl_audio_playout_controller_cleanup(controller);
    return false;
  }

  controller->packet_queue_capacity = nl_packet_queue_capacity_for_controller(controller);
  controller->packet_queue = (nl_audio_jitter_packet_t*)calloc(controller->packet_queue_capacity,
                                                               sizeof(nl_audio_jitter_packet_t));
  if (controller->packet_queue == NULL) {
    nl_audio_playout_controller_cleanup(controller);
    return false;
  }

  extra_capacity_frames = controller->samples_per_frame * 4U;
  pcm_capacity_frames = controller->maximum_buffer_frames + extra_capacity_frames;
  if (pcm_capacity_frames < controller->samples_per_frame * 2U) {
    pcm_capacity_frames = controller->samples_per_frame * 2U;
  }

  if (!nl_audio_pcm_sync_buffer_init(&controller->pcm_buffer,
                                     pcm_capacity_frames,
                                     controller->channel_count)) {
    nl_audio_playout_controller_cleanup(controller);
    return false;
  }

  return true;
}

void nl_audio_playout_controller_reset(nl_audio_playout_controller_t* controller) {
  if (controller == NULL) {
    return;
  }

  nl_audio_pcm_sync_buffer_reset(&controller->pcm_buffer);
  controller->packet_queue_count = 0;
  controller->has_expected_packet = false;
  controller->expected_sequence_number = 0;
  controller->expected_extended_rtp_timestamp = 0;
  controller->decoded_packet_count = 0;
  controller->concealment_count = 0;
  controller->dropped_frames_count = 0;
  controller->starvation_event_count = 0;
  controller->target_increase_count = 0;
  controller->target_decrease_count = 0;
  controller->stable_high_watermark_counter = 0;
  controller->current_target_frames = controller->target_buffer_frames;
  if (controller->decoder != NULL) {
    opus_multistream_decoder_ctl(controller->decoder, OPUS_RESET_STATE);
  }
}

void nl_audio_playout_controller_cleanup(nl_audio_playout_controller_t* controller) {
  if (controller == NULL) {
    return;
  }

  if (controller->decoder != NULL) {
    opus_multistream_decoder_destroy(controller->decoder);
    controller->decoder = NULL;
  }
  free(controller->decode_scratch);
  controller->decode_scratch = NULL;
  free(controller->packet_queue);
  controller->packet_queue = NULL;
  controller->packet_queue_capacity = 0;
  controller->packet_queue_count = 0;
  nl_audio_pcm_sync_buffer_cleanup(&controller->pcm_buffer);
  memset(controller, 0, sizeof(*controller));
}

int nl_audio_playout_controller_submit_packet(nl_audio_playout_controller_t* controller,
                                              const char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us) {
  nl_audio_jitter_packet_t packet;
  uint32_t insert_at;

  if (controller == NULL || controller->packet_queue == NULL) {
    return -1;
  }

  memset(&packet, 0, sizeof(packet));
  packet.sequence_number = sequence_number;
  packet.rtp_timestamp = rtp_timestamp;
  packet.extended_rtp_timestamp = extended_rtp_timestamp;
  packet.arrival_time_us = arrival_time_us;
  packet.plc_placeholder = (sample_data == NULL || sample_length <= 0);

  if (!packet.plc_placeholder) {
    if (sample_length > NL_AUDIO_MAX_COMPRESSED_PACKET_SIZE) {
      return -1;
    }
    packet.payload_length = (uint16_t)sample_length;
    memcpy(packet.payload, sample_data, (size_t)sample_length);
  }

  for (insert_at = 0; insert_at < controller->packet_queue_count; ++insert_at) {
    int order = nl_packet_compare(&packet, &controller->packet_queue[insert_at]);
    if (order == 0) {
      return 0;
    }
    if (order < 0) {
      break;
    }
  }

  if (controller->packet_queue_count >= controller->packet_queue_capacity) {
    if (insert_at == 0) {
      controller->dropped_frames_count += controller->samples_per_frame;
      return 0;
    }

    memmove(&controller->packet_queue[0],
            &controller->packet_queue[1],
            (size_t)(controller->packet_queue_count - 1U) * sizeof(controller->packet_queue[0]));
    controller->packet_queue_count -= 1U;
    insert_at -= 1U;
    controller->dropped_frames_count += controller->samples_per_frame;
  }

  if (insert_at < controller->packet_queue_count) {
    memmove(&controller->packet_queue[insert_at + 1U],
            &controller->packet_queue[insert_at],
            (size_t)(controller->packet_queue_count - insert_at) * sizeof(controller->packet_queue[0]));
  }

  controller->packet_queue[insert_at] = packet;
  controller->packet_queue_count += 1U;
  return controller->samples_per_frame;
}

uint32_t nl_audio_playout_controller_available_frames(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? nl_audio_pcm_sync_buffer_available_frames(&controller->pcm_buffer) : 0;
}

uint32_t nl_audio_playout_controller_target_frames(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->target_buffer_frames : 0;
}

uint32_t nl_audio_playout_controller_current_target_frames(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->current_target_frames : 0;
}

uint64_t nl_audio_playout_controller_concealment_count(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->concealment_count : 0;
}

uint64_t nl_audio_playout_controller_dropped_frames_count(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->dropped_frames_count : 0;
}

uint64_t nl_audio_playout_controller_starvation_event_count(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->starvation_event_count : 0;
}

uint64_t nl_audio_playout_controller_target_increase_count(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->target_increase_count : 0;
}

uint64_t nl_audio_playout_controller_target_decrease_count(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->target_decrease_count : 0;
}

void nl_audio_playout_controller_note_starvation(nl_audio_playout_controller_t* controller) {
  if (controller == NULL) {
    return;
  }

  controller->starvation_event_count += 1U;
  controller->stable_high_watermark_counter = 0;
  if (controller->current_target_frames < controller->maximum_buffer_frames) {
    uint32_t next_target = controller->current_target_frames + controller->samples_per_frame;
    if (next_target > controller->maximum_buffer_frames) {
      next_target = controller->maximum_buffer_frames;
    }
    if (next_target != controller->current_target_frames) {
      controller->current_target_frames = next_target;
      controller->target_increase_count += 1U;
    }
  }
}

void nl_audio_playout_controller_note_buffer_level(nl_audio_playout_controller_t* controller,
                                                   uint32_t effective_lead_frames) {
  if (controller == NULL) {
    return;
  }

  if (effective_lead_frames > controller->current_target_frames + controller->samples_per_frame) {
    controller->stable_high_watermark_counter += 1U;
    if (controller->stable_high_watermark_counter >= 32U &&
        controller->current_target_frames > controller->minimum_target_frames) {
      uint32_t next_target = controller->current_target_frames;
      if (next_target > controller->samples_per_frame) {
        next_target -= controller->samples_per_frame;
      }
      if (next_target < controller->minimum_target_frames) {
        next_target = controller->minimum_target_frames;
      }
      if (next_target != controller->current_target_frames) {
        controller->current_target_frames = next_target;
        controller->target_decrease_count += 1U;
      }
      controller->stable_high_watermark_counter = 0;
    }
  }
  else {
    controller->stable_high_watermark_counter = 0;
  }
}

uint32_t nl_audio_playout_controller_maximum_frames(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->maximum_buffer_frames : 0;
}

uint32_t nl_audio_playout_controller_startup_prefill_frames(const nl_audio_playout_controller_t* controller) {
  return controller != NULL ? controller->startup_prefill_frames : 0;
}

uint32_t nl_audio_playout_controller_queued_packet_frames(const nl_audio_playout_controller_t* controller) {
  if (controller == NULL) {
    return 0;
  }
  return controller->packet_queue_count * controller->samples_per_frame;
}

uint32_t nl_audio_playout_controller_fill_frames(nl_audio_playout_controller_t* controller,
                                                 uint32_t minimum_frames,
                                                 bool allow_concealment) {
  uint32_t available_frames;

  if (controller == NULL) {
    return 0;
  }

  available_frames = nl_audio_playout_controller_available_frames(controller);
  while (available_frames < minimum_frames) {
    while (controller->packet_queue_count > 0 &&
           nl_packet_is_obsolete(controller, &controller->packet_queue[0])) {
      nl_drop_head_packet(controller);
    }

    if (controller->packet_queue_count > 0) {
      nl_audio_jitter_packet_t packet = controller->packet_queue[0];
      int decoded_samples;

      if (!controller->has_expected_packet) {
        if (!nl_pop_head_packet(controller, &packet)) {
          break;
        }

        decoded_samples = nl_decode_packet_into_pcm(controller, &packet);
        if (decoded_samples <= 0) {
          break;
        }

        nl_advance_expected_packet(controller,
                                   packet.sequence_number,
                                   packet.extended_rtp_timestamp);
        available_frames += (uint32_t)decoded_samples;
        continue;
      }

      if (packet.extended_rtp_timestamp < controller->expected_extended_rtp_timestamp) {
        nl_drop_head_packet(controller);
        continue;
      }

      if (packet.extended_rtp_timestamp == controller->expected_extended_rtp_timestamp) {
        if (!nl_pop_head_packet(controller, &packet)) {
          break;
        }

        decoded_samples = nl_decode_packet_into_pcm(controller, &packet);
        if (decoded_samples <= 0) {
          break;
        }

        nl_advance_expected_packet(controller,
                                   packet.sequence_number,
                                   packet.extended_rtp_timestamp);
        available_frames += (uint32_t)decoded_samples;
        continue;
      }

      if (!allow_concealment) {
        if (!nl_pop_head_packet(controller, &packet)) {
          break;
        }

        decoded_samples = nl_decode_packet_into_pcm(controller, &packet);
        if (decoded_samples <= 0) {
          break;
        }

        nl_advance_expected_packet(controller,
                                   packet.sequence_number,
                                   packet.extended_rtp_timestamp);
        available_frames += (uint32_t)decoded_samples;
        continue;
      }

      if (!nl_generate_plc_frame(controller)) {
        break;
      }

      available_frames = nl_audio_playout_controller_available_frames(controller);
      continue;
    }

    if (!allow_concealment || !nl_generate_plc_frame(controller)) {
      break;
    }

    available_frames = nl_audio_playout_controller_available_frames(controller);
  }

  return nl_audio_playout_controller_available_frames(controller);
}

uint32_t nl_audio_playout_controller_pop_frames(nl_audio_playout_controller_t* controller,
                                                float* interleaved_samples,
                                                uint32_t requested_frames) {
  if (controller == NULL) {
    return 0;
  }
  return nl_audio_pcm_sync_buffer_read(&controller->pcm_buffer,
                                       interleaved_samples,
                                       requested_frames);
}
