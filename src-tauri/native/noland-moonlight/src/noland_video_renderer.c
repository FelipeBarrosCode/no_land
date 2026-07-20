#include "noland_video_renderer.h"
#include "Limelight.h"

#include <string.h>

static void nl_video_renderer_platform_attach_surface_noop(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface) {
  (void)renderer;
  (void)surface;
}

static void nl_video_renderer_platform_detach_surface_noop(nl_video_renderer_t* renderer) {
  (void)renderer;
}

static int nl_video_renderer_platform_setup_noop(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate) {
  (void)renderer;
  (void)video_format;
  (void)width;
  (void)height;
  (void)redraw_rate;
  return 0;
}

static void nl_video_renderer_platform_start_noop(nl_video_renderer_t* renderer) {
  (void)renderer;
}

static void nl_video_renderer_platform_stop_noop(nl_video_renderer_t* renderer) {
  (void)renderer;
}

static void nl_video_renderer_platform_cleanup_noop(nl_video_renderer_t* renderer) {
  (void)renderer;
}

static int nl_video_renderer_platform_submit_frame_noop(nl_video_renderer_t* renderer, const void* decode_unit, const nl_video_frame_metadata_t* frame) {
  (void)renderer;
  (void)decode_unit;
  (void)frame;
  return DR_OK;
}

#if !defined(__APPLE__)
void nl_video_renderer_platform_attach_surface(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface) {
  nl_video_renderer_platform_attach_surface_noop(renderer, surface);
}

void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer) {
  nl_video_renderer_platform_detach_surface_noop(renderer);
}

int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate) {
  return nl_video_renderer_platform_setup_noop(renderer, video_format, width, height, redraw_rate);
}

void nl_video_renderer_platform_start(nl_video_renderer_t* renderer) {
  nl_video_renderer_platform_start_noop(renderer);
}

void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_video_renderer_platform_stop_noop(renderer);
}

void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_video_renderer_platform_cleanup_noop(renderer);
}

int nl_video_renderer_platform_submit_frame(nl_video_renderer_t* renderer, const void* decode_unit, const nl_video_frame_metadata_t* frame) {
  return nl_video_renderer_platform_submit_frame_noop(renderer, decode_unit, frame);
}
#endif

void nl_video_renderer_init(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  memset(renderer, 0, sizeof(*renderer));
}

void nl_video_renderer_attach_surface(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface) {
  if (renderer == NULL) {
    return;
  }
  if (surface != NULL) {
    renderer->surface = *surface;
    renderer->surface_attached = true;
    nl_video_renderer_platform_attach_surface(renderer, surface);
  }
}

void nl_video_renderer_detach_surface(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  nl_video_renderer_platform_detach_surface(renderer);
  memset(&renderer->surface, 0, sizeof(renderer->surface));
  renderer->surface_attached = false;
}

int nl_video_renderer_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate) {
  if (renderer == NULL) {
    return -1;
  }
  renderer->configured = true;
  renderer->video_format = video_format;
  renderer->width = width;
  renderer->height = height;
  renderer->redraw_rate = redraw_rate;
  return nl_video_renderer_platform_setup(renderer, video_format, width, height, redraw_rate);
}

void nl_video_renderer_start(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  renderer->started = true;
  nl_video_renderer_platform_start(renderer);
}

void nl_video_renderer_stop(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  renderer->started = false;
  nl_video_renderer_platform_stop(renderer);
}

void nl_video_renderer_cleanup(nl_video_renderer_t* renderer) {
  bool preserve_surface_attached = false;
  nl_surface_descriptor_t preserved_surface;

  if (renderer == NULL) {
    return;
  }

  preserved_surface = renderer->surface;
  preserve_surface_attached = renderer->surface_attached;
  nl_video_renderer_platform_cleanup(renderer);
  memset(renderer, 0, sizeof(*renderer));
  if (preserve_surface_attached) {
    renderer->surface = preserved_surface;
    renderer->surface_attached = true;
  }
}

int nl_video_renderer_submit_frame(nl_video_renderer_t* renderer, const void* decode_unit, const nl_video_frame_metadata_t* frame) {
  if (renderer == NULL || frame == NULL) {
    return -1;
  }

  if (!nl_video_renderer_is_ready(renderer)) {
    renderer->dropped_frame_count += 1U;
    return DR_OK;
  }

  renderer->last_frame = *frame;
  if (nl_video_renderer_platform_submit_frame(renderer, decode_unit, frame) != DR_OK) {
    renderer->dropped_frame_count += 1U;
    return DR_OK;
  }
  renderer->submitted_frame_count += 1U;
  return DR_OK;
}

bool nl_video_renderer_is_ready(const nl_video_renderer_t* renderer) {
  return renderer != NULL && renderer->configured && renderer->started && renderer->surface_attached;
}

bool nl_video_renderer_is_session_active(const nl_video_renderer_t* renderer) {
  return renderer != NULL && renderer->configured && renderer->started;
}
