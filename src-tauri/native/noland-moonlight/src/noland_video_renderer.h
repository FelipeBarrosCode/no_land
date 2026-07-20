#ifndef NOLAND_VIDEO_RENDERER_H
#define NOLAND_VIDEO_RENDERER_H

#include "noland_moonlight.h"

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nl_video_frame_metadata {
  int32_t frame_number;
  int32_t frame_type;
  int32_t full_length;
  uint16_t host_processing_latency;
  uint64_t receive_time_us;
  uint64_t enqueue_time_us;
  uint64_t presentation_time_us;
  uint32_t rtp_timestamp;
  uint8_t hdr_active;
  uint8_t colorspace;
} nl_video_frame_metadata_t;

typedef struct nl_video_renderer {
  bool configured;
  bool started;
  bool surface_attached;
  int32_t video_format;
  int32_t width;
  int32_t height;
  int32_t redraw_rate;
  nl_surface_descriptor_t surface;
  nl_video_frame_metadata_t last_frame;
  uint64_t submitted_frame_count;
  uint64_t dropped_frame_count;
  void* platform_context;
} nl_video_renderer_t;

void nl_video_renderer_init(nl_video_renderer_t* renderer);
void nl_video_renderer_attach_surface(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface);
void nl_video_renderer_detach_surface(nl_video_renderer_t* renderer);
int nl_video_renderer_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate);
void nl_video_renderer_start(nl_video_renderer_t* renderer);
void nl_video_renderer_stop(nl_video_renderer_t* renderer);
void nl_video_renderer_cleanup(nl_video_renderer_t* renderer);
int nl_video_renderer_submit_frame(nl_video_renderer_t* renderer, const void* decode_unit, const nl_video_frame_metadata_t* frame);
bool nl_video_renderer_is_ready(const nl_video_renderer_t* renderer);
bool nl_video_renderer_is_session_active(const nl_video_renderer_t* renderer);

void nl_video_renderer_platform_attach_surface(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface);
void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer);
int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate);
void nl_video_renderer_platform_start(nl_video_renderer_t* renderer);
void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer);
void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer);
int nl_video_renderer_platform_submit_frame(nl_video_renderer_t* renderer, const void* decode_unit, const nl_video_frame_metadata_t* frame);

#ifdef __cplusplus
}
#endif

#endif
