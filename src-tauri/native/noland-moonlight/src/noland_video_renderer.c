#include "noland_video_renderer.h"
#include "Limelight.h"

#include <string.h>

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
  }
}

void nl_video_renderer_detach_surface(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
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
  return 0;
}

void nl_video_renderer_start(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  renderer->started = true;
}

void nl_video_renderer_stop(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return;
  }
  renderer->started = false;
}

void nl_video_renderer_cleanup(nl_video_renderer_t* renderer) {
  bool preserve_surface_attached = false;
  nl_surface_descriptor_t preserved_surface;

  if (renderer == NULL) {
    return;
  }

  preserved_surface = renderer->surface;
  preserve_surface_attached = renderer->surface_attached;
  memset(renderer, 0, sizeof(*renderer));
  if (preserve_surface_attached) {
    renderer->surface = preserved_surface;
    renderer->surface_attached = true;
  }
}

int nl_video_renderer_submit_frame(nl_video_renderer_t* renderer, const nl_video_frame_metadata_t* frame) {
  if (renderer == NULL || frame == NULL) {
    return -1;
  }

  if (!nl_video_renderer_is_ready(renderer)) {
    renderer->dropped_frame_count += 1U;
    return DR_OK;
  }

  renderer->last_frame = *frame;
  renderer->submitted_frame_count += 1U;
  return DR_OK;
}

bool nl_video_renderer_is_ready(const nl_video_renderer_t* renderer) {
  return renderer != NULL && renderer->configured && renderer->started && renderer->surface_attached;
}

bool nl_video_renderer_is_session_active(const nl_video_renderer_t* renderer) {
  return renderer != NULL && renderer->configured && renderer->started;
}
