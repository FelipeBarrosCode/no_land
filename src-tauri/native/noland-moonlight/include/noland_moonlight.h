#ifndef NOLAND_MOONLIGHT_H
#define NOLAND_MOONLIGHT_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nl_runtime nl_runtime_t;

typedef enum nl_result {
  NL_RESULT_OK = 0,
  NL_RESULT_INVALID_ARGUMENT = 1,
  NL_RESULT_OUT_OF_MEMORY = 2,
  NL_RESULT_NOT_READY = 3,
  NL_RESULT_INVALID_STATE = 4,
  NL_RESULT_QUEUE_EMPTY = 5
} nl_result_t;

typedef enum nl_stream_state {
  NL_STREAM_STATE_IDLE = 0,
  NL_STREAM_STATE_STARTING = 1,
  NL_STREAM_STATE_STREAMING = 2,
  NL_STREAM_STATE_STOPPING = 3
} nl_stream_state_t;

typedef enum nl_event_kind {
  NL_EVENT_NONE = 0,
  NL_EVENT_STATE_CHANGED = 1,
  NL_EVENT_CONNECTED = 2,
  NL_EVENT_STOPPED = 3,
  NL_EVENT_SURFACE_ATTACHED = 4,
  NL_EVENT_SURFACE_DETACHED = 5,
  NL_EVENT_ERROR = 6,
  NL_EVENT_STAGE_STARTING = 7,
  NL_EVENT_STAGE_COMPLETE = 8,
  NL_EVENT_STAGE_FAILED = 9,
  NL_EVENT_TERMINATED = 10,
  NL_EVENT_VIDEO_FRAME = 11
} nl_event_kind_t;

typedef enum nl_surface_type {
  NL_SURFACE_TYPE_UNKNOWN = 0,
  NL_SURFACE_WINDOWS_HWND = 1,
  NL_SURFACE_MACOS_NSVIEW = 2,
  NL_SURFACE_X11_WINDOW = 3,
  NL_SURFACE_WAYLAND_SURFACE = 4
} nl_surface_type_t;

typedef struct nl_start_request {
  const char* host_id;
  uint32_t app_id;
  const char* session_url;
  const char* host_address;
  const char* server_app_version;
  const char* server_gfe_version;
  int32_t server_codec_mode_support;
  int32_t width;
  int32_t height;
  int32_t fps;
  int32_t bitrate_kbps;
  int32_t packet_size;
  int32_t streaming_remotely;
  int32_t audio_configuration;
  int32_t supported_video_formats;
  int32_t client_refresh_rate_x100;
  int32_t color_space;
  int32_t color_range;
  int32_t encryption_flags;
  int8_t remote_input_aes_key[16];
  int8_t remote_input_aes_iv[16];
} nl_start_request_t;

typedef struct nl_surface_descriptor {
  nl_surface_type_t surface_type;
  void* window_handle;
  void* display_handle;
  uint32_t width;
  uint32_t height;
  float scale_factor;
} nl_surface_descriptor_t;

typedef struct nl_event {
  nl_event_kind_t kind;
  nl_stream_state_t state;
  int32_t code;
  char message[256];
} nl_event_t;

typedef struct nl_stats {
  nl_stream_state_t state;
  uint64_t start_count;
  uint64_t stop_count;
  uint64_t surface_attach_count;
  uint64_t surface_detach_count;
  uint64_t dropped_event_count;
  uint32_t last_width;
  uint32_t last_height;
  uint8_t has_surface;
  uint32_t estimated_rtt_ms;
  uint32_t estimated_rtt_variance_ms;
  uint8_t has_estimated_rtt;
  uint64_t video_setup_count;
  uint64_t video_frame_count;
  uint64_t video_frame_event_count;
  uint64_t coalesced_video_frame_event_count;
  uint8_t renderer_ready;
  uint8_t video_session_active;
  uint64_t renderer_submitted_frame_count;
  uint64_t renderer_dropped_frame_count;
  uint64_t audio_init_count;
  uint64_t audio_sample_count;
  uint64_t mouse_move_count;
  uint64_t mouse_position_count;
  uint64_t mouse_button_count;
  uint64_t keyboard_event_count;
  uint64_t controller_arrival_count;
  uint64_t controller_state_count;
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
} nl_stats_t;

nl_result_t nl_runtime_create(nl_runtime_t** output);
void nl_runtime_destroy(nl_runtime_t* runtime);
const char* nl_runtime_version_string(void);
const char* nl_get_launch_query_parameters(void);
int32_t nl_runtime_smoke_test(void);

nl_result_t nl_runtime_start(nl_runtime_t* runtime, const nl_start_request_t* request);
nl_result_t nl_runtime_request_stop(nl_runtime_t* runtime);
nl_result_t nl_runtime_attach_surface(nl_runtime_t* runtime, const nl_surface_descriptor_t* surface);
nl_result_t nl_runtime_detach_surface(nl_runtime_t* runtime);
nl_result_t nl_runtime_poll_event(nl_runtime_t* runtime, nl_event_t* output);
nl_result_t nl_runtime_read_stats(nl_runtime_t* runtime, nl_stats_t* output);
nl_result_t nl_send_relative_mouse(nl_runtime_t* runtime, int16_t delta_x, int16_t delta_y);
nl_result_t nl_send_absolute_mouse(nl_runtime_t* runtime, int16_t x, int16_t y, int16_t reference_width, int16_t reference_height);
nl_result_t nl_send_mouse_button(nl_runtime_t* runtime, uint8_t button, bool pressed);
nl_result_t nl_send_keyboard(nl_runtime_t* runtime, uint16_t virtual_key, bool pressed, uint8_t modifiers);
nl_result_t nl_send_controller_arrival(nl_runtime_t* runtime, uint8_t controller_number, uint16_t active_gamepad_mask, uint8_t controller_type, uint32_t supported_button_flags, uint16_t capabilities);
nl_result_t nl_send_controller_state(nl_runtime_t* runtime, int16_t controller_number, int16_t active_gamepad_mask, int32_t button_flags, uint8_t left_trigger, uint8_t right_trigger, int16_t left_stick_x, int16_t left_stick_y, int16_t right_stick_x, int16_t right_stick_y);

#ifdef __cplusplus
}
#endif

#endif
