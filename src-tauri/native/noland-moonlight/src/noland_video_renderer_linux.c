#include "noland_video_renderer.h"
#include "Limelight.h"

#include <gst/app/gstappsrc.h>
#include <gst/gst.h>
#include <gst/video/videooverlay.h>

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef struct nl_linux_video_context {
  GstElement* pipeline;
  GstAppSrc* appsrc;
  GstBus* bus;
  uintptr_t window_handle;
  int video_format;
  int width;
  int height;
  int redraw_rate;
  pthread_t frame_thread;
  pthread_mutex_t mutex;
  bool mutex_initialized;
  bool frame_thread_started;
  bool running;
} nl_linux_video_context_t;

static nl_linux_video_context_t* nl_linux_context(nl_video_renderer_t* renderer) {
  return renderer == NULL ? NULL : (nl_linux_video_context_t*)renderer->platform_context;
}

static nl_linux_video_context_t* nl_linux_ensure_context(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context != NULL) return context;
  context = (nl_linux_video_context_t*)calloc(1, sizeof(*context));
  if (context == NULL) return NULL;
  if (pthread_mutex_init(&context->mutex, NULL) != 0) {
    free(context);
    return NULL;
  }
  context->mutex_initialized = true;
  renderer->platform_context = context;
  return context;
}

static void nl_linux_set_overlay_handle(GstElement* element, uintptr_t handle) {
  if (element != NULL && GST_IS_VIDEO_OVERLAY(element) && handle != 0) {
    gst_video_overlay_set_window_handle(GST_VIDEO_OVERLAY(element), (guintptr)handle);
    gst_video_overlay_handle_events(GST_VIDEO_OVERLAY(element), FALSE);
  }
}

static GstBusSyncReply nl_linux_bus_sync(GstBus* bus, GstMessage* message, gpointer data) {
  nl_linux_video_context_t* context = (nl_linux_video_context_t*)data;
  (void)bus;
  if (context == NULL) return GST_BUS_PASS;

  if (gst_is_video_overlay_prepare_window_handle_message(message)) {
    pthread_mutex_lock(&context->mutex);
    nl_linux_set_overlay_handle(GST_ELEMENT(GST_MESSAGE_SRC(message)), context->window_handle);
    pthread_mutex_unlock(&context->mutex);
    gst_message_unref(message);
    return GST_BUS_DROP;
  }
  return GST_BUS_PASS;
}

static void nl_linux_log_bus_messages(nl_linux_video_context_t* context) {
  GstMessage* message;
  if (context == NULL || context->bus == NULL) return;
  while ((message = gst_bus_pop_filtered(
              context->bus,
              (GstMessageType)(GST_MESSAGE_ERROR | GST_MESSAGE_WARNING))) != NULL) {
    GError* error = NULL;
    gchar* debug = NULL;
    if (GST_MESSAGE_TYPE(message) == GST_MESSAGE_ERROR) {
      gst_message_parse_error(message, &error, &debug);
      g_printerr("[noland-video] GStreamer error: %s (%s)\n",
                 error != NULL ? error->message : "unknown",
                 debug != NULL ? debug : "no details");
    } else {
      gst_message_parse_warning(message, &error, &debug);
      g_printerr("[noland-video] GStreamer warning: %s (%s)\n",
                 error != NULL ? error->message : "unknown",
                 debug != NULL ? debug : "no details");
    }
    if (error != NULL) g_error_free(error);
    g_free(debug);
    gst_message_unref(message);
  }
}

static void nl_linux_destroy_pipeline(nl_linux_video_context_t* context) {
  if (context == NULL) return;
  if (context->pipeline != NULL) {
    gst_element_set_state(context->pipeline, GST_STATE_NULL);
  }
  if (context->bus != NULL) {
    gst_bus_set_sync_handler(context->bus, NULL, NULL, NULL);
    gst_object_unref(context->bus);
    context->bus = NULL;
  }
  if (context->appsrc != NULL) {
    gst_object_unref(context->appsrc);
    context->appsrc = NULL;
  }
  if (context->pipeline != NULL) {
    gst_object_unref(context->pipeline);
    context->pipeline = NULL;
  }
}

static int nl_linux_create_pipeline(nl_linux_video_context_t* context) {
  GError* error = NULL;
  GstCaps* caps;
  const char* parser;
  const char* media_type;
  char pipeline_description[512];

  if (context == NULL) return -1;
  nl_linux_destroy_pipeline(context);

  if ((context->video_format & VIDEO_FORMAT_MASK_H264) != 0) {
    parser = "h264parse";
    media_type = "video/x-h264";
  } else if ((context->video_format & VIDEO_FORMAT_MASK_H265) != 0) {
    parser = "h265parse";
    media_type = "video/x-h265";
  } else {
    g_printerr("[noland-video] unsupported Linux video format mask: 0x%x\n",
               context->video_format);
    return -1;
  }

  snprintf(pipeline_description,
           sizeof(pipeline_description),
           "appsrc name=noland-source is-live=true format=time block=false "
           "max-bytes=8388608 ! queue max-size-buffers=3 leaky=downstream ! "
           "%s config-interval=-1 ! decodebin ! videoconvert ! videoscale ! "
           "autovideosink sync=false",
           parser);
  context->pipeline = gst_parse_launch(pipeline_description, &error);
  if (context->pipeline == NULL) {
    g_printerr("[noland-video] failed to create GStreamer pipeline: %s\n",
               error != NULL ? error->message : "unknown error");
    if (error != NULL) g_error_free(error);
    return -1;
  }
  if (error != NULL) g_error_free(error);

  context->appsrc = GST_APP_SRC(gst_bin_get_by_name(GST_BIN(context->pipeline), "noland-source"));
  if (context->appsrc == NULL) {
    nl_linux_destroy_pipeline(context);
    return -1;
  }

  caps = gst_caps_new_simple(media_type,
                             "stream-format", G_TYPE_STRING, "byte-stream",
                             "alignment", G_TYPE_STRING, "au",
                             "width", G_TYPE_INT, context->width,
                             "height", G_TYPE_INT, context->height,
                             "framerate", GST_TYPE_FRACTION,
                             context->redraw_rate > 0 ? context->redraw_rate : 60,
                             1,
                             NULL);
  gst_app_src_set_caps(context->appsrc, caps);
  gst_caps_unref(caps);
  gst_app_src_set_stream_type(context->appsrc, GST_APP_STREAM_TYPE_STREAM);
  gst_app_src_set_max_bytes(context->appsrc, 8U * 1024U * 1024U);
  gst_app_src_set_leaky_type(context->appsrc, GST_APP_LEAKY_TYPE_DOWNSTREAM);

  context->bus = gst_element_get_bus(context->pipeline);
  if (context->bus != NULL) {
    gst_bus_set_sync_handler(context->bus, nl_linux_bus_sync, context, NULL);
  }
  return 0;
}

static void* nl_linux_frame_thread(void* data) {
  nl_video_renderer_t* renderer = (nl_video_renderer_t*)data;
  nl_linux_video_context_t* context;

  while (renderer != NULL) {
    VIDEO_FRAME_HANDLE handle;
    PDECODE_UNIT decode_unit;
    nl_video_frame_metadata_t metadata;
    bool running;
    bool submitted = false;

    context = nl_linux_context(renderer);
    if (context == NULL) break;
    pthread_mutex_lock(&context->mutex);
    running = context->running;
    pthread_mutex_unlock(&context->mutex);
    if (!running) break;

    while (LiPollNextVideoFrame(&handle, &decode_unit)) {
      int result;
      memset(&metadata, 0, sizeof(metadata));
      metadata.frame_number = decode_unit->frameNumber;
      metadata.frame_type = decode_unit->frameType;
      metadata.full_length = decode_unit->fullLength;
      metadata.host_processing_latency = decode_unit->frameHostProcessingLatency;
      metadata.receive_time_us = decode_unit->receiveTimeUs;
      metadata.enqueue_time_us = decode_unit->enqueueTimeUs;
      metadata.presentation_time_us = decode_unit->presentationTimeUs;
      metadata.rtp_timestamp = decode_unit->rtpTimestamp;
      metadata.hdr_active = decode_unit->hdrActive ? 1U : 0U;
      metadata.colorspace = decode_unit->colorspace;

      result = renderer->frame_processor != NULL
                   ? renderer->frame_processor(renderer->frame_processor_user_data,
                                               decode_unit,
                                               &metadata)
                   : nl_video_renderer_submit_frame(renderer, decode_unit, &metadata);
      LiCompleteVideoFrame(handle, result);
      submitted = true;
      if (LiGetPendingVideoFrames() <= 1) break;
    }

    nl_linux_log_bus_messages(context);
    if (!submitted) usleep(1000);
  }
  return NULL;
}

void nl_video_renderer_platform_attach_surface(nl_video_renderer_t* renderer,
                                               const nl_surface_descriptor_t* surface) {
  nl_linux_video_context_t* context = nl_linux_ensure_context(renderer);
  if (context == NULL || surface == NULL || surface->window_handle == NULL) return;
  if (surface->surface_type != NL_SURFACE_X11_WINDOW &&
      surface->surface_type != NL_SURFACE_WAYLAND_SURFACE) {
    return;
  }
  pthread_mutex_lock(&context->mutex);
  context->window_handle = (uintptr_t)surface->window_handle;
  pthread_mutex_unlock(&context->mutex);
}

void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL) return;
  pthread_mutex_lock(&context->mutex);
  context->window_handle = 0;
  pthread_mutex_unlock(&context->mutex);
}

int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer,
                                     int video_format,
                                     int width,
                                     int height,
                                     int redraw_rate) {
  nl_linux_video_context_t* context = nl_linux_ensure_context(renderer);
  if (context == NULL) return -1;
  if (!gst_is_initialized()) gst_init(NULL, NULL);

  pthread_mutex_lock(&context->mutex);
  context->video_format = video_format;
  context->width = width;
  context->height = height;
  context->redraw_rate = redraw_rate;
  pthread_mutex_unlock(&context->mutex);
  return nl_linux_create_pipeline(context);
}

void nl_video_renderer_platform_start(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL || context->pipeline == NULL || context->frame_thread_started) return;
  if (gst_element_set_state(context->pipeline, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE) {
    g_printerr("[noland-video] failed to start GStreamer video pipeline\n");
    return;
  }

  pthread_mutex_lock(&context->mutex);
  context->running = true;
  pthread_mutex_unlock(&context->mutex);
  if (pthread_create(&context->frame_thread, NULL, nl_linux_frame_thread, renderer) == 0) {
    context->frame_thread_started = true;
  } else {
    pthread_mutex_lock(&context->mutex);
    context->running = false;
    pthread_mutex_unlock(&context->mutex);
    gst_element_set_state(context->pipeline, GST_STATE_NULL);
  }
}

void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL) return;
  pthread_mutex_lock(&context->mutex);
  context->running = false;
  pthread_mutex_unlock(&context->mutex);
  if (context->frame_thread_started) {
    pthread_join(context->frame_thread, NULL);
    context->frame_thread_started = false;
  }
  if (context->appsrc != NULL) gst_app_src_end_of_stream(context->appsrc);
  if (context->pipeline != NULL) gst_element_set_state(context->pipeline, GST_STATE_NULL);
}

void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL) return;
  nl_video_renderer_platform_stop(renderer);
  nl_linux_destroy_pipeline(context);
  if (context->mutex_initialized) pthread_mutex_destroy(&context->mutex);
  free(context);
  renderer->platform_context = NULL;
}

int nl_video_renderer_platform_submit_frame(nl_video_renderer_t* renderer,
                                            const void* raw_decode_unit,
                                            const nl_video_frame_metadata_t* frame) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  const DECODE_UNIT* decode_unit = (const DECODE_UNIT*)raw_decode_unit;
  const LENTRY* entry;
  GstBuffer* buffer;
  GstMapInfo map;
  size_t total = 0;
  size_t offset = 0;
  GstFlowReturn flow;

  if (context == NULL || context->appsrc == NULL || decode_unit == NULL) return DR_OK;
  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    if (entry->data != NULL && entry->length > 0) total += (size_t)entry->length;
  }
  if (total == 0) return DR_OK;

  buffer = gst_buffer_new_allocate(NULL, total, NULL);
  if (buffer == NULL || !gst_buffer_map(buffer, &map, GST_MAP_WRITE)) {
    if (buffer != NULL) gst_buffer_unref(buffer);
    return DR_OK;
  }
  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    if (entry->data == NULL || entry->length <= 0) continue;
    memcpy(map.data + offset, entry->data, (size_t)entry->length);
    offset += (size_t)entry->length;
  }
  gst_buffer_unmap(buffer, &map);
  GST_BUFFER_PTS(buffer) = (GstClockTime)decode_unit->presentationTimeUs * 1000U;
  GST_BUFFER_DTS(buffer) = GST_CLOCK_TIME_NONE;
  if (context->redraw_rate > 0) {
    GST_BUFFER_DURATION(buffer) = gst_util_uint64_scale_int(1, GST_SECOND, context->redraw_rate);
  }
  if (decode_unit->frameType == FRAME_TYPE_IDR) {
    GST_BUFFER_FLAG_UNSET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
  } else {
    GST_BUFFER_FLAG_SET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
  }

  flow = gst_app_src_push_buffer(context->appsrc, buffer);
  (void)frame;
  return flow == GST_FLOW_OK ? DR_OK : DR_NEED_IDR;
}
