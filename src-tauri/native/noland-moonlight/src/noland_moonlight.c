#include "noland_moonlight.h"
#include "noland_audio_renderer.h"
#include "noland_controller_manager.h"
#include "noland_video_renderer.h"
#include "Limelight.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <stdbool.h>

#if defined(_WIN32)
#include <windows.h>
#else
#include <pthread.h>
#endif

#define NL_EVENT_QUEUE_CAPACITY 64

typedef struct nl_owned_start_request {
  char* host_id;
  uint32_t app_id;
  char* session_url;
  char* host_address;
  char* server_app_version;
  char* server_gfe_version;
  int32_t server_codec_mode_support;
  int32_t width;
  int32_t height;
  int32_t fps;
  int32_t bitrate_kbps;
  int32_t packet_size;
  int32_t streaming_remotely;
  int32_t audio_configuration;
  uint32_t audio_target_buffer_ms;
  uint32_t audio_maximum_buffer_ms;
  int32_t supported_video_formats;
  int32_t client_refresh_rate_x100;
  int32_t color_space;
  int32_t color_range;
  int32_t encryption_flags;
  int8_t remote_input_aes_key[16];
  int8_t remote_input_aes_iv[16];
} nl_owned_start_request_t;

struct nl_runtime {
  nl_stream_state_t state;
  nl_owned_start_request_t request;
  nl_surface_descriptor_t surface;
  bool has_surface;
  bool worker_running;
  bool stop_requested;
  nl_event_t events[NL_EVENT_QUEUE_CAPACITY];
  uint32_t event_head;
  uint32_t event_len;
  uint64_t start_count;
  uint64_t stop_count;
  uint64_t surface_attach_count;
  uint64_t surface_detach_count;
  uint64_t dropped_event_count;
  uint64_t video_setup_count;
  uint64_t video_frame_count;
  uint64_t video_frame_event_count;
  uint64_t coalesced_video_frame_event_count;
  uint64_t audio_init_count;
  uint64_t audio_sample_count;
  uint64_t mouse_move_count;
  uint64_t mouse_position_count;
  uint64_t mouse_button_count;
  uint64_t keyboard_event_count;
  uint64_t controller_arrival_count;
  uint64_t controller_state_count;
  nl_video_renderer_t renderer;
  nl_audio_renderer_t audio_renderer;
  nl_controller_manager_t* controller_manager;
  int32_t last_video_frame_number;
  int32_t last_video_frame_type;
  int32_t last_video_frame_length;
  uint16_t last_video_host_processing_latency;
  uint64_t last_video_receive_time_us;
  uint64_t last_video_enqueue_time_us;
  uint64_t last_video_presentation_time_us;
  uint32_t last_video_rtp_timestamp;
  uint8_t last_video_hdr_active;
  uint8_t last_video_colorspace;
#if defined(_WIN32)
  CRITICAL_SECTION mutex;
  HANDLE worker_thread;
#else
  pthread_mutex_t mutex;
  pthread_t worker_thread;
#endif
};

static nl_runtime_t* g_active_runtime = NULL;
#if defined(_WIN32)
static CRITICAL_SECTION g_active_runtime_mutex;
static bool g_active_runtime_mutex_initialized = false;
#else
static pthread_mutex_t g_active_runtime_mutex = PTHREAD_MUTEX_INITIALIZER;
#endif

static void nl_global_lock(void) {
#if defined(_WIN32)
  if (!g_active_runtime_mutex_initialized) {
    InitializeCriticalSection(&g_active_runtime_mutex);
    g_active_runtime_mutex_initialized = true;
  }
  EnterCriticalSection(&g_active_runtime_mutex);
#else
  pthread_mutex_lock(&g_active_runtime_mutex);
#endif
}

static void nl_global_unlock(void) {
#if defined(_WIN32)
  LeaveCriticalSection(&g_active_runtime_mutex);
#else
  pthread_mutex_unlock(&g_active_runtime_mutex);
#endif
}

static void nl_runtime_lock(nl_runtime_t* runtime) {
  if (runtime == NULL) {
    return;
  }
#if defined(_WIN32)
  EnterCriticalSection(&runtime->mutex);
#else
  pthread_mutex_lock(&runtime->mutex);
#endif
}

static void nl_runtime_unlock(nl_runtime_t* runtime) {
  if (runtime == NULL) {
    return;
  }
#if defined(_WIN32)
  LeaveCriticalSection(&runtime->mutex);
#else
  pthread_mutex_unlock(&runtime->mutex);
#endif
}

static char* nl_strdup(const char* value) {
  size_t length;
  char* copy;
  if (value == NULL) {
    return NULL;
  }
  length = strlen(value);
  copy = (char*)malloc(length + 1);
  if (copy == NULL) {
    return NULL;
  }
  memcpy(copy, value, length + 1);
  return copy;
}

static void nl_owned_request_clear(nl_owned_start_request_t* request) {
  if (request == NULL) {
    return;
  }
  free(request->host_id);
  free(request->session_url);
  free(request->host_address);
  free(request->server_app_version);
  free(request->server_gfe_version);
  memset(request, 0, sizeof(*request));
}

static bool nl_owned_request_copy(nl_owned_start_request_t* output, const nl_start_request_t* input) {
  if (output == NULL || input == NULL || input->host_id == NULL || input->host_address == NULL || input->server_app_version == NULL) {
    return false;
  }

  memset(output, 0, sizeof(*output));
  output->host_id = nl_strdup(input->host_id);
  output->session_url = nl_strdup(input->session_url);
  output->host_address = nl_strdup(input->host_address);
  output->server_app_version = nl_strdup(input->server_app_version);
  output->server_gfe_version = nl_strdup(input->server_gfe_version);
  if (output->host_id == NULL || output->host_address == NULL || output->server_app_version == NULL ||
      (input->session_url != NULL && output->session_url == NULL) ||
      (input->server_gfe_version != NULL && output->server_gfe_version == NULL)) {
    nl_owned_request_clear(output);
    return false;
  }

  output->app_id = input->app_id;
  output->server_codec_mode_support = input->server_codec_mode_support;
  output->width = input->width;
  output->height = input->height;
  output->fps = input->fps;
  output->bitrate_kbps = input->bitrate_kbps;
  output->packet_size = input->packet_size;
  output->streaming_remotely = input->streaming_remotely;
  output->audio_configuration = input->audio_configuration;
  output->audio_target_buffer_ms = input->audio_target_buffer_ms;
  output->audio_maximum_buffer_ms = input->audio_maximum_buffer_ms;
  output->supported_video_formats = input->supported_video_formats;
  output->client_refresh_rate_x100 = input->client_refresh_rate_x100;
  output->color_space = input->color_space;
  output->color_range = input->color_range;
  output->encryption_flags = input->encryption_flags;
  memcpy(output->remote_input_aes_key, input->remote_input_aes_key, sizeof(output->remote_input_aes_key));
  memcpy(output->remote_input_aes_iv, input->remote_input_aes_iv, sizeof(output->remote_input_aes_iv));
  return true;
}

static void nl_event_init(nl_event_t* event, nl_event_kind_t kind, nl_stream_state_t state, int32_t code, const char* message) {
  if (event == NULL) {
    return;
  }
  memset(event, 0, sizeof(*event));
  event->kind = kind;
  event->state = state;
  event->code = code;
  if (message != NULL) {
    snprintf(event->message, sizeof(event->message), "%s", message);
  }
}

static void nl_runtime_push_event_locked(nl_runtime_t* runtime, nl_event_kind_t kind, int32_t code, const char* message) {
  nl_event_t event;
  uint32_t index;
  if (runtime == NULL) {
    return;
  }
  nl_event_init(&event, kind, runtime->state, code, message);

  if (kind == NL_EVENT_VIDEO_FRAME && runtime->event_len > 0U) {
    uint32_t tail_index = (runtime->event_head + runtime->event_len - 1U) % NL_EVENT_QUEUE_CAPACITY;
    if (runtime->events[tail_index].kind == NL_EVENT_VIDEO_FRAME) {
      runtime->events[tail_index] = event;
      runtime->coalesced_video_frame_event_count += 1U;
      return;
    }
  }

  if (runtime->event_len == NL_EVENT_QUEUE_CAPACITY) {
    runtime->event_head = (runtime->event_head + 1U) % NL_EVENT_QUEUE_CAPACITY;
    runtime->event_len -= 1U;
    runtime->dropped_event_count += 1U;
  }
  index = (runtime->event_head + runtime->event_len) % NL_EVENT_QUEUE_CAPACITY;
  runtime->events[index] = event;
  runtime->event_len += 1U;
  if (kind == NL_EVENT_VIDEO_FRAME) {
    runtime->video_frame_event_count += 1U;
  }
}

static void nl_runtime_push_event(nl_runtime_t* runtime, nl_event_kind_t kind, int32_t code, const char* message) {
  nl_runtime_lock(runtime);
  nl_runtime_push_event_locked(runtime, kind, code, message);
  nl_runtime_unlock(runtime);
}

static nl_runtime_t* nl_get_active_runtime(void) {
  nl_runtime_t* runtime;
  nl_global_lock();
  runtime = g_active_runtime;
  nl_global_unlock();
  return runtime;
}

static void nl_set_active_runtime(nl_runtime_t* runtime) {
  nl_global_lock();
  g_active_runtime = runtime;
  nl_global_unlock();
}

static void nl_clear_active_runtime(nl_runtime_t* runtime) {
  nl_global_lock();
  if (g_active_runtime == runtime) {
    g_active_runtime = NULL;
  }
  nl_global_unlock();
}

static void nl_set_state(nl_runtime_t* runtime, nl_stream_state_t state, nl_event_kind_t event_kind, int32_t code, const char* message) {
  nl_runtime_lock(runtime);
  runtime->state = state;
  if (event_kind != NL_EVENT_NONE) {
    nl_runtime_push_event_locked(runtime, event_kind, code, message);
  }
  nl_runtime_unlock(runtime);
}

static void nl_connection_stage_starting(int stage) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  const char* stage_name = LiGetStageName(stage);
  if (runtime == NULL) {
    return;
  }
  nl_runtime_push_event(runtime, NL_EVENT_STAGE_STARTING, stage, stage_name != NULL ? stage_name : "stage starting");
}

static void nl_connection_stage_complete(int stage) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  const char* stage_name = LiGetStageName(stage);
  if (runtime == NULL) {
    return;
  }
  nl_runtime_push_event(runtime, NL_EVENT_STAGE_COMPLETE, stage, stage_name != NULL ? stage_name : "stage complete");
}

static void nl_connection_stage_failed(int stage, int errorCode) {
  char message[256];
  nl_runtime_t* runtime = nl_get_active_runtime();
  const char* stage_name = LiGetStageName(stage);
  if (runtime == NULL) {
    return;
  }
  nl_runtime_lock(runtime);
  snprintf(
      message,
      sizeof(message),
      "%s failed (%d) [audio=0x%08X channels=%d remote=%d packet=%d sessionUrl=%s]",
      stage_name != NULL ? stage_name : "stage",
      errorCode,
      (unsigned int)runtime->request.audio_configuration,
      (runtime->request.audio_configuration >> 8) & 0xFF,
      runtime->request.streaming_remotely,
      runtime->request.packet_size,
      runtime->request.session_url != NULL ? "yes" : "no");
  nl_runtime_unlock(runtime);
  nl_runtime_push_event(runtime, NL_EVENT_STAGE_FAILED, errorCode, message);
}

static void nl_connection_started(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_set_state(runtime, NL_STREAM_STATE_STREAMING, NL_EVENT_CONNECTED, 0, "connected");
  if (runtime->controller_manager != NULL) {
    nl_controller_manager_start(runtime->controller_manager, runtime);
  }
}

static void nl_connection_terminated(int errorCode) {
  char message[256];
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  if (runtime->controller_manager != NULL) {
    nl_controller_manager_stop(runtime->controller_manager);
  }
  snprintf(message, sizeof(message), "terminated (%d)", errorCode);
  nl_set_state(runtime, NL_STREAM_STATE_IDLE, NL_EVENT_TERMINATED, errorCode, message);
  nl_clear_active_runtime(runtime);
  nl_runtime_lock(runtime);
  nl_owned_request_clear(&runtime->request);
  nl_runtime_unlock(runtime);
}

static void nl_connection_log_message(const char* format, ...) {
  char message[256];
  va_list args;
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || format == NULL) {
    return;
  }
  va_start(args, format);
  vsnprintf(message, sizeof(message), format, args);
  va_end(args);
  nl_runtime_push_event(runtime, NL_EVENT_STATE_CHANGED, 0, message);
}

static void nl_connection_rumble(unsigned short controllerNumber, unsigned short lowFreqMotor, unsigned short highFreqMotor) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || runtime->controller_manager == NULL) {
    return;
  }
  nl_controller_manager_rumble(runtime->controller_manager, controllerNumber, lowFreqMotor, highFreqMotor);
}

static void nl_connection_rumble_triggers(uint16_t controllerNumber, uint16_t leftTriggerMotor, uint16_t rightTriggerMotor) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || runtime->controller_manager == NULL) {
    return;
  }
  nl_controller_manager_rumble_triggers(runtime->controller_manager, controllerNumber, leftTriggerMotor, rightTriggerMotor);
}

static void nl_connection_set_motion_event_state(uint16_t controllerNumber, uint8_t motionType, uint16_t reportRateHz) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || runtime->controller_manager == NULL) {
    return;
  }
  nl_controller_manager_set_motion_event_state(runtime->controller_manager, controllerNumber, motionType, reportRateHz);
}

static void nl_connection_set_controller_led(uint16_t controllerNumber, uint8_t r, uint8_t g, uint8_t b) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || runtime->controller_manager == NULL) {
    return;
  }
  nl_controller_manager_set_led(runtime->controller_manager, controllerNumber, r, g, b);
}

static void nl_connection_set_adaptive_triggers(uint16_t controllerNumber, uint8_t eventFlags, uint8_t typeLeft, uint8_t typeRight, uint8_t* left, uint8_t* right) {
  nl_dualsense_output_report_t report;
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL || runtime->controller_manager == NULL) {
    return;
  }

  memset(&report, 0, sizeof(report));
  report.valid_flag0 = eventFlags;
  report.valid_flag1 = 0x04;
  report.right_trigger_effect_type = typeRight;
  report.left_trigger_effect_type = typeLeft;
  if (right != NULL) {
    memcpy(report.right_trigger_effect, right, sizeof(report.right_trigger_effect));
  }
  if (left != NULL) {
    memcpy(report.left_trigger_effect, left, sizeof(report.left_trigger_effect));
  }

  nl_controller_manager_set_adaptive_triggers(runtime->controller_manager, controllerNumber, &report);
}

static int nl_video_frame_processor(void* user_data, const void* raw_decode_unit, const nl_video_frame_metadata_t* frame);

static int nl_video_setup(int videoFormat, int width, int height, int redrawRate, void* context, int drFlags) {
  nl_runtime_t* runtime = (nl_runtime_t*)context;
  char message[256];
  int result;
  (void)drFlags;
  if (runtime == NULL) {
    return -1;
  }
  nl_runtime_lock(runtime);
  runtime->video_setup_count += 1U;
  result = nl_video_renderer_setup(&runtime->renderer, videoFormat, width, height, redrawRate);
  nl_video_renderer_set_frame_processor(&runtime->renderer, nl_video_frame_processor, runtime);
  snprintf(message, sizeof(message), "video setup %dx%d fmt=%d", width, height, videoFormat);
  nl_runtime_push_event_locked(runtime, NL_EVENT_STATE_CHANGED, result, message);
  nl_runtime_unlock(runtime);
  return result;
}

static void nl_video_start(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_video_renderer_start(&runtime->renderer);
  nl_runtime_push_event(runtime, NL_EVENT_STATE_CHANGED, 0, "video renderer started");
}

static void nl_video_stop(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_video_renderer_stop(&runtime->renderer);
  nl_runtime_push_event(runtime, NL_EVENT_STATE_CHANGED, 0, "video renderer stopped");
}

static void nl_video_cleanup(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_video_renderer_cleanup(&runtime->renderer);
  nl_runtime_push_event(runtime, NL_EVENT_STATE_CHANGED, 0, "video renderer cleaned up");
}

static int nl_video_frame_processor(void* user_data, const void* raw_decode_unit, const nl_video_frame_metadata_t* frame) {
  nl_runtime_t* runtime = (nl_runtime_t*)user_data;
  char message[256];
  if (runtime != NULL && frame != NULL) {
    int renderer_result;

    nl_runtime_lock(runtime);
    runtime->video_frame_count += 1U;
    runtime->last_video_frame_number = frame->frame_number;
    runtime->last_video_frame_type = frame->frame_type;
    runtime->last_video_frame_length = frame->full_length;
    runtime->last_video_host_processing_latency = frame->host_processing_latency;
    runtime->last_video_receive_time_us = frame->receive_time_us;
    runtime->last_video_enqueue_time_us = frame->enqueue_time_us;
    runtime->last_video_presentation_time_us = frame->presentation_time_us;
    runtime->last_video_rtp_timestamp = frame->rtp_timestamp;
    runtime->last_video_hdr_active = frame->hdr_active;
    runtime->last_video_colorspace = frame->colorspace;
    renderer_result = nl_video_renderer_submit_frame(&runtime->renderer, raw_decode_unit, frame);
    snprintf(message, sizeof(message), "video frame #%d len=%d", frame->frame_number, frame->full_length);
    nl_runtime_push_event_locked(runtime, NL_EVENT_VIDEO_FRAME, frame->frame_number, message);
    nl_runtime_unlock(runtime);
    return renderer_result;
  }
  return DR_OK;
}

static int nl_audio_init(int audioConfiguration, const POPUS_MULTISTREAM_CONFIGURATION opusConfig, void* context, int arFlags) {
  nl_runtime_t* runtime = (nl_runtime_t*)context;
  char message[256];
  int result;
  if (runtime == NULL || opusConfig == NULL) {
    return -1;
  }

  result = nl_audio_renderer_init(&runtime->audio_renderer, audioConfiguration, opusConfig, arFlags);
  if (result != 0) {
    return result;
  }

  nl_runtime_lock(runtime);
  runtime->audio_init_count += 1U;
  nl_runtime_unlock(runtime);
  snprintf(message, sizeof(message), "audio init configuration=%d channels=%d rate=%d frame=%d target=%u max=%u", audioConfiguration, opusConfig->channelCount, opusConfig->sampleRate, opusConfig->samplesPerFrame, (unsigned int)runtime->audio_renderer.target_buffer_ms, (unsigned int)runtime->audio_renderer.maximum_buffer_ms);
  nl_runtime_push_event(runtime, NL_EVENT_STATE_CHANGED, 0, message);
  return 0;
}

static void nl_audio_start(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_audio_renderer_start(&runtime->audio_renderer);
}

static void nl_audio_stop(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_audio_renderer_stop(&runtime->audio_renderer);
}

static void nl_audio_cleanup(void) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime == NULL) {
    return;
  }
  nl_audio_renderer_cleanup(&runtime->audio_renderer);
}

static void nl_audio_decode_and_play_sample(char* sampleData, int sampleLength) {
  nl_runtime_t* runtime = nl_get_active_runtime();
  if (runtime != NULL) {
    nl_runtime_lock(runtime);
    runtime->audio_sample_count += 1U;
    nl_runtime_unlock(runtime);
    nl_audio_renderer_decode_and_play_sample(&runtime->audio_renderer, sampleData, sampleLength);
  }
}

static bool nl_runtime_can_send_input(nl_runtime_t* runtime) {
  bool ready;
  if (runtime == NULL) {
    return false;
  }
  nl_runtime_lock(runtime);
  ready = runtime->state == NL_STREAM_STATE_STREAMING;
  nl_runtime_unlock(runtime);
  return ready;
}

static void nl_runtime_finish_worker(nl_runtime_t* runtime, int errorCode) {
  char message[256];
  if (runtime == NULL) {
    return;
  }
  nl_runtime_lock(runtime);
  runtime->worker_running = false;
  if (errorCode == 0) {
    nl_runtime_unlock(runtime);
    return;
  }
  nl_runtime_unlock(runtime);

  nl_clear_active_runtime(runtime);
  nl_runtime_lock(runtime);
  runtime->state = NL_STREAM_STATE_IDLE;
  snprintf(
      message,
      sizeof(message),
      "LiStartConnection returned %d [audio=0x%08X channels=%d remote=%d packet=%d sessionUrl=%s]",
      errorCode,
      (unsigned int)runtime->request.audio_configuration,
      (runtime->request.audio_configuration >> 8) & 0xFF,
      runtime->request.streaming_remotely,
      runtime->request.packet_size,
      runtime->request.session_url != NULL ? "yes" : "no");
  nl_runtime_push_event_locked(runtime, NL_EVENT_ERROR, errorCode, message);
  nl_owned_request_clear(&runtime->request);
  snprintf(message, sizeof(message), "stopped (%d)", errorCode);
  nl_runtime_push_event_locked(runtime, NL_EVENT_STOPPED, errorCode, message);
  nl_runtime_unlock(runtime);
}

static int nl_run_connection(nl_runtime_t* runtime) {
  SERVER_INFORMATION serverInfo;
  STREAM_CONFIGURATION streamConfig;
  CONNECTION_LISTENER_CALLBACKS connectionCallbacks;
  DECODER_RENDERER_CALLBACKS videoCallbacks;
  AUDIO_RENDERER_CALLBACKS audioCallbacks;

  if (runtime == NULL) {
    return -1;
  }

  LiInitializeServerInformation(&serverInfo);
  LiInitializeStreamConfiguration(&streamConfig);
  LiInitializeConnectionCallbacks(&connectionCallbacks);
  LiInitializeVideoCallbacks(&videoCallbacks);
  LiInitializeAudioCallbacks(&audioCallbacks);

  nl_runtime_lock(runtime);
  serverInfo.address = runtime->request.host_address;
  serverInfo.serverInfoAppVersion = runtime->request.server_app_version;
  serverInfo.serverInfoGfeVersion = runtime->request.server_gfe_version;
  serverInfo.rtspSessionUrl = runtime->request.session_url;
  serverInfo.serverCodecModeSupport = runtime->request.server_codec_mode_support;

  streamConfig.width = runtime->request.width;
  streamConfig.height = runtime->request.height;
  streamConfig.fps = runtime->request.fps;
  streamConfig.bitrate = runtime->request.bitrate_kbps;
  streamConfig.packetSize = runtime->request.packet_size;
  streamConfig.streamingRemotely = runtime->request.streaming_remotely;
  streamConfig.audioConfiguration = runtime->request.audio_configuration;
  runtime->audio_renderer.target_buffer_ms = runtime->request.audio_target_buffer_ms;
  runtime->audio_renderer.maximum_buffer_ms = runtime->request.audio_maximum_buffer_ms;
  streamConfig.supportedVideoFormats = runtime->request.supported_video_formats;
  streamConfig.clientRefreshRateX100 = runtime->request.client_refresh_rate_x100;
  streamConfig.colorSpace = runtime->request.color_space;
  streamConfig.colorRange = runtime->request.color_range;
  streamConfig.encryptionFlags = runtime->request.encryption_flags;
  memcpy(streamConfig.remoteInputAesKey, runtime->request.remote_input_aes_key, sizeof(streamConfig.remoteInputAesKey));
  memcpy(streamConfig.remoteInputAesIv, runtime->request.remote_input_aes_iv, sizeof(streamConfig.remoteInputAesIv));
  {
    char message[256];
    snprintf(
        message,
        sizeof(message),
        "starting host=%s appId=%u address=%s audio=0x%08X channels=%d target=%u max=%u remote=%d packet=%d sessionUrl=%s",
        runtime->request.host_id,
        (unsigned int)runtime->request.app_id,
        runtime->request.host_address,
        (unsigned int)runtime->request.audio_configuration,
        (runtime->request.audio_configuration >> 8) & 0xFF,
        (unsigned int)runtime->request.audio_target_buffer_ms,
        (unsigned int)runtime->request.audio_maximum_buffer_ms,
        runtime->request.streaming_remotely,
        runtime->request.packet_size,
        runtime->request.session_url != NULL ? "yes" : "no");
    nl_runtime_push_event_locked(runtime, NL_EVENT_STATE_CHANGED, 0, message);
  }
  nl_runtime_unlock(runtime);

  connectionCallbacks.stageStarting = nl_connection_stage_starting;
  connectionCallbacks.stageComplete = nl_connection_stage_complete;
  connectionCallbacks.stageFailed = nl_connection_stage_failed;
  connectionCallbacks.connectionStarted = nl_connection_started;
  connectionCallbacks.connectionTerminated = nl_connection_terminated;
  connectionCallbacks.logMessage = nl_connection_log_message;
  connectionCallbacks.rumble = nl_connection_rumble;
  connectionCallbacks.rumbleTriggers = nl_connection_rumble_triggers;
  connectionCallbacks.setMotionEventState = nl_connection_set_motion_event_state;
  connectionCallbacks.setControllerLED = nl_connection_set_controller_led;
  connectionCallbacks.setAdaptiveTriggers = nl_connection_set_adaptive_triggers;

  videoCallbacks.setup = nl_video_setup;
  videoCallbacks.start = nl_video_start;
  videoCallbacks.stop = nl_video_stop;
  videoCallbacks.cleanup = nl_video_cleanup;
  videoCallbacks.capabilities = CAPABILITY_PULL_RENDERER |
                                CAPABILITY_REFERENCE_FRAME_INVALIDATION_HEVC |
                                CAPABILITY_REFERENCE_FRAME_INVALIDATION_AV1;

  audioCallbacks.init = nl_audio_init;
  audioCallbacks.start = nl_audio_start;
  audioCallbacks.stop = nl_audio_stop;
  audioCallbacks.cleanup = nl_audio_cleanup;
  audioCallbacks.decodeAndPlaySample = nl_audio_decode_and_play_sample;
  audioCallbacks.capabilities = CAPABILITY_SUPPORTS_ARBITRARY_AUDIO_DURATION;

  nl_set_active_runtime(runtime);
  return LiStartConnection(&serverInfo, &streamConfig, &connectionCallbacks, &videoCallbacks, &audioCallbacks, runtime, 0, runtime, 0);
}

#if defined(_WIN32)
static DWORD WINAPI nl_worker_thread_proc(LPVOID parameter) {
  nl_runtime_t* runtime = (nl_runtime_t*)parameter;
  int result = nl_run_connection(runtime);
  nl_runtime_finish_worker(runtime, result);
  return 0;
}
#else
static void* nl_worker_thread_proc(void* parameter) {
  nl_runtime_t* runtime = (nl_runtime_t*)parameter;
  int result = nl_run_connection(runtime);
  nl_runtime_finish_worker(runtime, result);
  return NULL;
}
#endif

nl_result_t nl_runtime_create(nl_runtime_t** output) {
  nl_runtime_t* runtime;
  if (output == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }

  runtime = (nl_runtime_t*)calloc(1, sizeof(nl_runtime_t));
  if (runtime == NULL) {
    return NL_RESULT_OUT_OF_MEMORY;
  }
  runtime->state = NL_STREAM_STATE_IDLE;
  nl_video_renderer_init(&runtime->renderer);
  memset(&runtime->audio_renderer, 0, sizeof(runtime->audio_renderer));
  runtime->audio_renderer.target_buffer_ms = 20U;
  runtime->audio_renderer.maximum_buffer_ms = 80U;
  runtime->controller_manager = nl_controller_manager_create();
  if (runtime->controller_manager == NULL) {
    free(runtime);
    return NL_RESULT_OUT_OF_MEMORY;
  }
#if defined(_WIN32)
  InitializeCriticalSection(&runtime->mutex);
  runtime->worker_thread = NULL;
#else
  pthread_mutex_init(&runtime->mutex, NULL);
  runtime->worker_thread = 0;
#endif
  *output = runtime;
  return NL_RESULT_OK;
}

void nl_runtime_destroy(nl_runtime_t* runtime) {
  if (runtime == NULL) {
    return;
  }
  nl_runtime_request_stop(runtime);
  nl_runtime_lock(runtime);
  nl_runtime_unlock(runtime);
#if defined(_WIN32)
  if (runtime->worker_thread != NULL) {
    WaitForSingleObject(runtime->worker_thread, INFINITE);
    CloseHandle(runtime->worker_thread);
    runtime->worker_thread = NULL;
  }
  DeleteCriticalSection(&runtime->mutex);
#else
  if (runtime->worker_thread != 0) {
    pthread_join(runtime->worker_thread, NULL);
    runtime->worker_thread = 0;
  }
  pthread_mutex_destroy(&runtime->mutex);
#endif
  nl_audio_renderer_cleanup(&runtime->audio_renderer);
  nl_controller_manager_destroy(runtime->controller_manager);
  nl_owned_request_clear(&runtime->request);
  free(runtime);
}

const char* nl_runtime_version_string(void) {
  return "noland-moonlight/0.3.0";
}

const char* nl_get_launch_query_parameters(void) {
  return LiGetLaunchUrlQueryParameters();
}

int32_t nl_runtime_smoke_test(void) {
  return 7;
}

nl_result_t nl_runtime_start(nl_runtime_t* runtime, const nl_start_request_t* request) {
  if (runtime == NULL || request == NULL || request->host_id == NULL || request->host_address == NULL || request->server_app_version == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }

  nl_runtime_lock(runtime);
  if (runtime->state != NL_STREAM_STATE_IDLE || runtime->worker_running) {
    nl_runtime_unlock(runtime);
    return NL_RESULT_INVALID_STATE;
  }
  nl_owned_request_clear(&runtime->request);
  if (!nl_owned_request_copy(&runtime->request, request)) {
    nl_runtime_unlock(runtime);
    return NL_RESULT_OUT_OF_MEMORY;
  }
  runtime->state = NL_STREAM_STATE_STARTING;
  runtime->worker_running = true;
  runtime->stop_requested = false;
  runtime->start_count += 1U;
  nl_runtime_push_event_locked(runtime, NL_EVENT_STATE_CHANGED, 0, "starting");
  nl_runtime_unlock(runtime);

#if defined(_WIN32)
  runtime->worker_thread = CreateThread(NULL, 0, nl_worker_thread_proc, runtime, 0, NULL);
  if (runtime->worker_thread == NULL) {
    nl_runtime_lock(runtime);
    runtime->worker_running = false;
    runtime->state = NL_STREAM_STATE_IDLE;
    nl_owned_request_clear(&runtime->request);
    nl_runtime_unlock(runtime);
    return NL_RESULT_OUT_OF_MEMORY;
  }
#else
  if (pthread_create(&runtime->worker_thread, NULL, nl_worker_thread_proc, runtime) != 0) {
    nl_runtime_lock(runtime);
    runtime->worker_running = false;
    runtime->state = NL_STREAM_STATE_IDLE;
    nl_owned_request_clear(&runtime->request);
    nl_runtime_unlock(runtime);
    return NL_RESULT_OUT_OF_MEMORY;
  }
#endif

  return NL_RESULT_OK;
}

nl_result_t nl_runtime_request_stop(nl_runtime_t* runtime) {
  bool should_interrupt = false;
  bool should_stop = false;
  if (runtime == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }

  nl_runtime_lock(runtime);
  if (runtime->state == NL_STREAM_STATE_IDLE && !runtime->worker_running) {
    nl_runtime_unlock(runtime);
    return NL_RESULT_OK;
  }
  should_interrupt = runtime->worker_running || runtime->state == NL_STREAM_STATE_STARTING || runtime->state == NL_STREAM_STATE_STREAMING;
  should_stop = runtime->state == NL_STREAM_STATE_STARTING || runtime->state == NL_STREAM_STATE_STREAMING || runtime->state == NL_STREAM_STATE_STOPPING;
  runtime->state = NL_STREAM_STATE_STOPPING;
  runtime->stop_requested = true;
  runtime->stop_count += 1U;
  nl_runtime_push_event_locked(runtime, NL_EVENT_STATE_CHANGED, 0, "stopping");
  nl_runtime_unlock(runtime);

  if (should_interrupt) {
    LiInterruptConnection();
  }
  if (should_stop) {
    LiStopConnection();
  }
  if (runtime->controller_manager != NULL) {
    nl_controller_manager_stop(runtime->controller_manager);
  }

  nl_clear_active_runtime(runtime);
  nl_runtime_lock(runtime);
  runtime->state = NL_STREAM_STATE_IDLE;
  nl_owned_request_clear(&runtime->request);
  nl_runtime_push_event_locked(runtime, NL_EVENT_STOPPED, 0, "stopped (0)");
  nl_runtime_unlock(runtime);
  return NL_RESULT_OK;
}

nl_result_t nl_runtime_attach_surface(nl_runtime_t* runtime, const nl_surface_descriptor_t* surface) {
  if (runtime == NULL || surface == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }
  nl_runtime_lock(runtime);
  runtime->surface = *surface;
  runtime->has_surface = true;
  nl_video_renderer_attach_surface(&runtime->renderer, surface);
  runtime->surface_attach_count += 1U;
  nl_runtime_push_event_locked(runtime, NL_EVENT_SURFACE_ATTACHED, 0, "surface attached");
  nl_runtime_unlock(runtime);
  return NL_RESULT_OK;
}

nl_result_t nl_runtime_detach_surface(nl_runtime_t* runtime) {
  if (runtime == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }
  nl_runtime_lock(runtime);
  memset(&runtime->surface, 0, sizeof(runtime->surface));
  runtime->has_surface = false;
  nl_video_renderer_detach_surface(&runtime->renderer);
  runtime->surface_detach_count += 1U;
  nl_runtime_push_event_locked(runtime, NL_EVENT_SURFACE_DETACHED, 0, "surface detached");
  nl_runtime_unlock(runtime);
  return NL_RESULT_OK;
}

nl_result_t nl_runtime_poll_event(nl_runtime_t* runtime, nl_event_t* output) {
  if (runtime == NULL || output == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }
  nl_runtime_lock(runtime);
  if (runtime->event_len == 0U) {
    nl_runtime_unlock(runtime);
    return NL_RESULT_QUEUE_EMPTY;
  }
  *output = runtime->events[runtime->event_head];
  runtime->event_head = (runtime->event_head + 1U) % NL_EVENT_QUEUE_CAPACITY;
  runtime->event_len -= 1U;
  nl_runtime_unlock(runtime);
  return NL_RESULT_OK;
}

nl_result_t nl_runtime_read_stats(nl_runtime_t* runtime, nl_stats_t* output) {
  if (runtime == NULL || output == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }
  nl_runtime_lock(runtime);
  memset(output, 0, sizeof(*output));
  output->state = runtime->state;
  output->start_count = runtime->start_count;
  output->stop_count = runtime->stop_count;
  output->surface_attach_count = runtime->surface_attach_count;
  output->surface_detach_count = runtime->surface_detach_count;
  output->dropped_event_count = runtime->dropped_event_count;
  output->last_width = runtime->surface.width;
  output->last_height = runtime->surface.height;
  output->has_surface = runtime->has_surface ? 1U : 0U;
  output->has_estimated_rtt = 0U;
  nl_runtime_unlock(runtime);

  {
    uint32_t estimated_rtt = 0;
    uint32_t estimated_rtt_variance = 0;
    if (LiGetEstimatedRttInfo(&estimated_rtt, &estimated_rtt_variance)) {
          output->estimated_rtt_ms = estimated_rtt;
          output->estimated_rtt_variance_ms = estimated_rtt_variance;
          output->has_estimated_rtt = 1U;
        }
      }

      nl_runtime_lock(runtime);
      output->video_setup_count = runtime->video_setup_count;
      output->video_frame_count = runtime->video_frame_count;
      output->video_frame_event_count = runtime->video_frame_event_count;
      output->coalesced_video_frame_event_count = runtime->coalesced_video_frame_event_count;
      output->renderer_ready = nl_video_renderer_is_ready(&runtime->renderer) ? 1U : 0U;
      output->video_session_active = nl_video_renderer_is_session_active(&runtime->renderer) ? 1U : 0U;
      output->renderer_submitted_frame_count = runtime->renderer.submitted_frame_count;
      output->renderer_dropped_frame_count = runtime->renderer.dropped_frame_count;
      output->audio_init_count = runtime->audio_init_count;
      output->audio_sample_count = runtime->audio_sample_count;
      output->mouse_move_count = runtime->mouse_move_count;
      output->mouse_position_count = runtime->mouse_position_count;
      output->mouse_button_count = runtime->mouse_button_count;
      output->keyboard_event_count = runtime->keyboard_event_count;
      output->controller_arrival_count = runtime->controller_arrival_count;
      output->controller_state_count = runtime->controller_state_count;
      output->last_video_frame_number = runtime->last_video_frame_number;
      output->last_video_frame_type = runtime->last_video_frame_type;
      output->last_video_frame_length = runtime->last_video_frame_length;
      output->last_video_host_processing_latency = runtime->last_video_host_processing_latency;
      output->last_video_receive_time_us = runtime->last_video_receive_time_us;
      output->last_video_enqueue_time_us = runtime->last_video_enqueue_time_us;
      output->last_video_presentation_time_us = runtime->last_video_presentation_time_us;
      output->last_video_rtp_timestamp = runtime->last_video_rtp_timestamp;
      output->last_video_hdr_active = runtime->last_video_hdr_active;
      output->last_video_colorspace = runtime->last_video_colorspace;
      nl_runtime_unlock(runtime);

      return NL_RESULT_OK;
    }

    nl_result_t nl_send_relative_mouse(nl_runtime_t* runtime, int16_t delta_x, int16_t delta_y) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendMouseMoveEvent((short)delta_x, (short)delta_y);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->mouse_move_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_absolute_mouse(nl_runtime_t* runtime, int16_t x, int16_t y, int16_t reference_width, int16_t reference_height) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendMousePositionEvent((short)x, (short)y, (short)reference_width, (short)reference_height);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->mouse_position_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_mouse_button(nl_runtime_t* runtime, uint8_t button, bool pressed) {
      int result;
      char action = pressed ? BUTTON_ACTION_PRESS : BUTTON_ACTION_RELEASE;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendMouseButtonEvent(action, (int)button);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->mouse_button_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_vertical_scroll(nl_runtime_t* runtime, int16_t amount, bool high_resolution) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = high_resolution ? LiSendHighResScrollEvent((short)amount) : LiSendScrollEvent((signed char)amount);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_horizontal_scroll(nl_runtime_t* runtime, int16_t amount, bool high_resolution) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = high_resolution ? LiSendHighResHScrollEvent((short)amount) : LiSendHScrollEvent((signed char)amount);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_keyboard(nl_runtime_t* runtime, uint16_t virtual_key, bool pressed, uint8_t modifiers) {
      int result;
      char action = pressed ? KEY_ACTION_DOWN : KEY_ACTION_UP;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendKeyboardEvent((short)virtual_key, action, (char)modifiers);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->keyboard_event_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_controller_arrival(nl_runtime_t* runtime, uint8_t controller_number, uint16_t active_gamepad_mask, uint8_t controller_type, uint32_t supported_button_flags, uint16_t capabilities) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendControllerArrivalEvent(controller_number, active_gamepad_mask, controller_type, supported_button_flags, capabilities);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->controller_arrival_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }

    nl_result_t nl_send_controller_state(nl_runtime_t* runtime, int16_t controller_number, int16_t active_gamepad_mask, int32_t button_flags, uint8_t left_trigger, uint8_t right_trigger, int16_t left_stick_x, int16_t left_stick_y, int16_t right_stick_x, int16_t right_stick_y) {
      int result;
      if (!nl_runtime_can_send_input(runtime)) {
        return NL_RESULT_INVALID_STATE;
      }
      result = LiSendMultiControllerEvent((short)controller_number, (short)active_gamepad_mask, (int)button_flags, left_trigger, right_trigger, (short)left_stick_x, (short)left_stick_y, (short)right_stick_x, (short)right_stick_y);
      if (result != 0) {
        return NL_RESULT_NOT_READY;
      }
      nl_runtime_lock(runtime);
      runtime->controller_state_count += 1U;
      nl_runtime_unlock(runtime);
      return NL_RESULT_OK;
    }
