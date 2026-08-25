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

#define NL_WAYLAND_DISPLAY_CONTEXT_TYPE "GstWaylandDisplayHandleContextType"
#define NL_INPUT_MAX_BYTES (8U * 1024U * 1024U)
#define NL_DECODER_NAME_MAX 64
#define NL_SINK_NAME_MAX 64
#define NL_SINK_INDEX_NONE (-1)
#define NL_X11_SINK_COUNT 3
#define NL_WAYLAND_SINK_COUNT 1
#define NL_SINK_READY_TIMEOUT (2 * GST_SECOND)

typedef enum nl_linux_codec {
  NL_LINUX_CODEC_UNKNOWN = 0,
  NL_LINUX_CODEC_H264,
  NL_LINUX_CODEC_H265,
  NL_LINUX_CODEC_AV1,
} nl_linux_codec_t;

typedef struct nl_linux_codec_description {
  const char* parser;
  const char* media_type;
  const char* stream_format;
  const char* alignment;
  const char* software_decoder;
  const char* const* hardware_decoders;
} nl_linux_codec_description_t;

typedef struct nl_linux_sink_description {
  const char* name;
  bool needs_converter;
} nl_linux_sink_description_t;

typedef struct nl_linux_video_context {
  GstElement* pipeline;
  GstAppSrc* appsrc;
  GstBus* bus;
  GstElement* decoder;
  GstElement* render_queue;
  GstElement* sink;
  nl_video_renderer_t* renderer;
  uintptr_t window_handle;
  uintptr_t display_handle;
  nl_surface_type_t surface_type;
  uint32_t surface_width;
  uint32_t surface_height;
  int video_format;
  int width;
  int height;
  int redraw_rate;
  pthread_t frame_thread;
  pthread_mutex_t mutex;
  bool mutex_initialized;
  bool frame_thread_started;
  bool running;
  bool using_software_decoder;
  bool fallback_attempted;
  bool awaiting_idr;
  bool fatal_error;
  int sink_index;
  uint32_t disabled_sinks;
  uint8_t smoothing_capacity;
  uint64_t smoothing_overflow_drops;
  uint64_t last_decoder_output_pts_us;
  uint64_t backpressure_started_us;
  bool backpressure_active;
  char decoder_name[NL_DECODER_NAME_MAX];
  char sink_name[NL_SINK_NAME_MAX];
} nl_linux_video_context_t;

static const char* const NL_H264_HARDWARE_DECODERS[] = {
    "nvh264dec",
    "nvh264sldec",
    "vah264dec",
    "vaapih264dec",
    "v4l2slh264dec",
    "v4l2h264dec",
    "omxh264dec",
    NULL,
};

static const char* const NL_H265_HARDWARE_DECODERS[] = {
    "nvh265dec",
    "nvh265sldec",
    "vah265dec",
    "vaapih265dec",
    "v4l2slh265dec",
    "v4l2h265dec",
    "omxh265dec",
    NULL,
};

static const char* const NL_AV1_HARDWARE_DECODERS[] = {
    "nvav1dec",
    "nvav1sldec",
    "vaav1dec",
    "v4l2slav1dec",
    "v4l2av1dec",
    NULL,
};

static const nl_linux_sink_description_t NL_X11_SINKS[NL_X11_SINK_COUNT] = {
    {"glimagesink", false},
    {"xvimagesink", false},
    {"ximagesink", true},
};

static const nl_linux_sink_description_t NL_WAYLAND_SINKS[NL_WAYLAND_SINK_COUNT] = {
    {"waylandsink", false},
};

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
  context->sink_index = NL_SINK_INDEX_NONE;
  context->renderer = renderer;
  renderer->platform_context = context;
  return context;
}

static uint8_t nl_linux_smoothing_capacity(const nl_video_renderer_t* renderer) {
  uint8_t capacity;
  if (renderer == NULL) return 0U;
  capacity = (uint8_t)renderer->latency_config.frame_buffer_mode;
  return capacity > 3U ? 3U : capacity;
}

static uint16_t nl_linux_u16_depth(guint depth) {
  return depth > (guint)UINT16_MAX ? UINT16_MAX : (uint16_t)depth;
}

static uint64_t nl_linux_buffer_pts_us(const GstBuffer* buffer) {
  GstClockTime pts;
  if (buffer == NULL) return 0U;
  pts = GST_BUFFER_PTS(buffer);
  return GST_CLOCK_TIME_IS_VALID(pts) ? (uint64_t)(pts / 1000U) : 0U;
}

static void nl_linux_query_render_queue(GstElement* render_queue,
                                        guint* depth,
                                        guint* capacity) {
  guint current_depth = 0U;
  guint maximum_depth = 0U;
  if (render_queue != NULL) {
    g_object_get(render_queue,
                 "current-level-buffers", &current_depth,
                 "max-size-buffers", &maximum_depth,
                 NULL);
  }
  if (depth != NULL) *depth = current_depth;
  if (capacity != NULL) *capacity = maximum_depth;
}

static void nl_linux_publish_smoothing(nl_linux_video_context_t* context,
                                       guint render_queue_depth) {
  nl_video_renderer_t* renderer;
  uint8_t smoothing_capacity;
  uint8_t smoothing_depth;
  uint64_t overflow_drops;
  uint32_t stream_fps;
  if (context == NULL) return;

  pthread_mutex_lock(&context->mutex);
  renderer = context->renderer;
  smoothing_capacity = context->smoothing_capacity;
  smoothing_depth = render_queue_depth > 0U
      ? (uint8_t)(render_queue_depth - 1U)
      : 0U;
  if (smoothing_depth > smoothing_capacity) {
    smoothing_depth = smoothing_capacity;
  }
  overflow_drops = context->smoothing_overflow_drops;
  stream_fps = context->redraw_rate > 0 ? (uint32_t)context->redraw_rate : 0U;
  pthread_mutex_unlock(&context->mutex);

  if (renderer != NULL) {
    nl_latency_telemetry_set_smoothing(&renderer->telemetry,
                                       smoothing_depth,
                                       smoothing_capacity,
                                       overflow_drops,
                                       0U,
                                       stream_fps);
  }
}

static void nl_linux_set_backpressure(nl_linux_video_context_t* context,
                                      bool active,
                                      uint64_t now_us) {
  nl_video_renderer_t* renderer;
  uint64_t duration_us = 0U;
  bool changed = false;
  if (context == NULL) return;

  pthread_mutex_lock(&context->mutex);
  renderer = context->renderer;
  if (active && !context->backpressure_active) {
    context->backpressure_active = true;
    context->backpressure_started_us = now_us;
    changed = true;
  } else if (!active && context->backpressure_active) {
    if (now_us >= context->backpressure_started_us) {
      duration_us = now_us - context->backpressure_started_us;
    }
    context->backpressure_active = false;
    context->backpressure_started_us = 0U;
    changed = true;
  }
  pthread_mutex_unlock(&context->mutex);

  if (changed && renderer != NULL) {
    nl_latency_telemetry_record_backpressure(&renderer->telemetry,
                                             duration_us,
                                             active);
  }
}

static void nl_linux_reset_latency_state(nl_linux_video_context_t* context) {
  nl_video_renderer_t* renderer;
  uint8_t smoothing_capacity;
  uint32_t stream_fps;
  if (context == NULL) return;

  pthread_mutex_lock(&context->mutex);
  renderer = context->renderer;
  smoothing_capacity = nl_linux_smoothing_capacity(renderer);
  context->smoothing_capacity = smoothing_capacity;
  context->smoothing_overflow_drops = 0U;
  context->last_decoder_output_pts_us = 0U;
  context->backpressure_started_us = 0U;
  context->backpressure_active = false;
  stream_fps = context->redraw_rate > 0 ? (uint32_t)context->redraw_rate : 0U;
  pthread_mutex_unlock(&context->mutex);

  if (renderer != NULL) {
    nl_latency_telemetry_reset(&renderer->telemetry,
                               renderer->latency_config.telemetry_enabled != 0U,
                               stream_fps,
                               renderer->latency_config.late_frame_tolerance_us,
                               smoothing_capacity);
    nl_latency_telemetry_set_pacing(&renderer->telemetry,
                                    renderer->latency_config.pacing_mode,
                                    NL_PACING_MODE_OFF);
  }
}

static GstPadProbeReturn nl_linux_post_decoder_probe(GstPad* pad,
                                                     GstPadProbeInfo* info,
                                                     gpointer data) {
  nl_linux_video_context_t* context = (nl_linux_video_context_t*)data;
  GstBuffer* buffer = GST_PAD_PROBE_INFO_BUFFER(info);
  guint render_queue_depth = 0U;
  guint render_queue_capacity = 0U;
  uint64_t presentation_time_us;
  uint64_t now_us;
  bool backpressured;
  (void)pad;
  if (context == NULL || buffer == NULL) return GST_PAD_PROBE_OK;

  presentation_time_us = nl_linux_buffer_pts_us(buffer);
  now_us = LiGetMicroseconds();
  nl_linux_query_render_queue(context->render_queue,
                              &render_queue_depth,
                              &render_queue_capacity);
  backpressured = render_queue_capacity != 0U &&
                  render_queue_depth >= render_queue_capacity;

  pthread_mutex_lock(&context->mutex);
  context->last_decoder_output_pts_us = presentation_time_us;
  pthread_mutex_unlock(&context->mutex);

  nl_latency_telemetry_record_decoder_output(
      &context->renderer->telemetry,
      presentation_time_us,
      now_us,
      nl_linux_u16_depth(render_queue_depth),
      backpressured);
  nl_linux_set_backpressure(context, backpressured, now_us);
  nl_linux_publish_smoothing(context, render_queue_depth);
  return GST_PAD_PROBE_OK;
}

static GstPadProbeReturn nl_linux_pre_sink_probe(GstPad* pad,
                                                 GstPadProbeInfo* info,
                                                 gpointer data) {
  nl_linux_video_context_t* context = (nl_linux_video_context_t*)data;
  GstBuffer* buffer = GST_PAD_PROBE_INFO_BUFFER(info);
  guint render_queue_depth = 0U;
  guint render_queue_capacity = 0U;
  uint64_t now_us;
  (void)pad;
  if (context == NULL || buffer == NULL) return GST_PAD_PROBE_OK;

  now_us = LiGetMicroseconds();
  nl_linux_query_render_queue(context->render_queue,
                              &render_queue_depth,
                              &render_queue_capacity);
  nl_latency_telemetry_record_render_submit(
      &context->renderer->telemetry,
      nl_linux_buffer_pts_us(buffer),
      now_us);
  nl_linux_set_backpressure(
      context,
      render_queue_capacity != 0U && render_queue_depth >= render_queue_capacity,
      now_us);
  nl_linux_publish_smoothing(context, render_queue_depth);
  return GST_PAD_PROBE_OK;
}

static void nl_linux_render_queue_overrun(GstElement* render_queue,
                                          gpointer data) {
  nl_linux_video_context_t* context = (nl_linux_video_context_t*)data;
  nl_video_renderer_t* renderer;
  guint render_queue_depth = 0U;
  uint8_t smoothing_capacity;
  uint64_t presentation_time_us;
  uint64_t now_us;
  if (context == NULL) return;

  now_us = LiGetMicroseconds();
  nl_linux_query_render_queue(render_queue, &render_queue_depth, NULL);
  pthread_mutex_lock(&context->mutex);
  renderer = context->renderer;
  smoothing_capacity = context->smoothing_capacity;
  presentation_time_us = context->last_decoder_output_pts_us;
  if (smoothing_capacity != 0U) {
    context->smoothing_overflow_drops += 1U;
  }
  pthread_mutex_unlock(&context->mutex);

  nl_linux_set_backpressure(context, true, now_us);
  if (renderer != NULL) {
    nl_latency_telemetry_record_drop(
        &renderer->telemetry,
        presentation_time_us,
        0U,
        smoothing_capacity == 0U
            ? NL_FRAME_DROP_PACER_BACKLOG
            : NL_FRAME_DROP_SMOOTHING_OVERFLOW);
  }
  nl_linux_publish_smoothing(context, render_queue_depth);
}

static bool nl_linux_add_buffer_probe(GstElement* element,
                                      const char* pad_name,
                                      GstPadProbeCallback callback,
                                      nl_linux_video_context_t* context) {
  GstPad* pad;
  gulong probe_id;
  if (element == NULL || pad_name == NULL || callback == NULL || context == NULL) {
    return false;
  }
  pad = gst_element_get_static_pad(element, pad_name);
  if (pad == NULL) return false;
  probe_id = gst_pad_add_probe(pad,
                               GST_PAD_PROBE_TYPE_BUFFER,
                               callback,
                               context,
                               NULL);
  gst_object_unref(pad);
  return probe_id != 0U;
}

static bool nl_linux_factory_exists(const char* name) {
  GstElementFactory* factory;
  if (name == NULL) return false;
  factory = gst_element_factory_find(name);
  if (factory == NULL) return false;
  gst_object_unref(factory);
  return true;
}

static nl_linux_codec_t nl_linux_codec_from_format(int video_format) {
  if ((video_format & VIDEO_FORMAT_MASK_H264) != 0) return NL_LINUX_CODEC_H264;
  if ((video_format & VIDEO_FORMAT_MASK_H265) != 0) return NL_LINUX_CODEC_H265;
  if ((video_format & VIDEO_FORMAT_MASK_AV1) != 0) return NL_LINUX_CODEC_AV1;
  return NL_LINUX_CODEC_UNKNOWN;
}

static bool nl_linux_describe_codec(int video_format,
                                    nl_linux_codec_description_t* description) {
  nl_linux_codec_t codec;
  if (description == NULL) return false;
  memset(description, 0, sizeof(*description));
  codec = nl_linux_codec_from_format(video_format);
  switch (codec) {
    case NL_LINUX_CODEC_H264:
      description->parser = "h264parse";
      description->media_type = "video/x-h264";
      description->stream_format = "byte-stream";
      description->alignment = "au";
      description->software_decoder = "avdec_h264";
      description->hardware_decoders = NL_H264_HARDWARE_DECODERS;
      return true;
    case NL_LINUX_CODEC_H265:
      description->parser = "h265parse";
      description->media_type = "video/x-h265";
      description->stream_format = "byte-stream";
      description->alignment = "au";
      description->software_decoder = "avdec_h265";
      description->hardware_decoders = NL_H265_HARDWARE_DECODERS;
      return true;
    case NL_LINUX_CODEC_AV1:
      description->parser = "av1parse";
      description->media_type = "video/x-av1";
      description->stream_format = "obu-stream";
      description->alignment = "tu";
      description->software_decoder = "avdec_av1";
      description->hardware_decoders = NL_AV1_HARDWARE_DECODERS;
      return true;
    default:
      return false;
  }
}

static GstContext* nl_linux_create_wayland_display_context(uintptr_t display_handle) {
  GstContext* context;
  GstStructure* structure;
  if (display_handle == 0) return NULL;
  context = gst_context_new(NL_WAYLAND_DISPLAY_CONTEXT_TYPE, TRUE);
  if (context == NULL) return NULL;
  structure = gst_context_writable_structure(context);
  gst_structure_set(structure,
                    "display",
                    G_TYPE_POINTER,
                    (gpointer)display_handle,
                    NULL);
  return context;
}

static void nl_linux_apply_wayland_display_context(GstElement* element,
                                                   uintptr_t display_handle) {
  GstContext* context;
  if (element == NULL || display_handle == 0) return;
  context = nl_linux_create_wayland_display_context(display_handle);
  if (context == NULL) return;
  gst_element_set_context(element, context);
  gst_context_unref(context);
}

static void nl_linux_set_overlay_handle(GstElement* element,
                                        uintptr_t handle,
                                        uint32_t width,
                                        uint32_t height) {
  if (element == NULL || !GST_IS_VIDEO_OVERLAY(element) || handle == 0) return;
  gst_video_overlay_set_window_handle(GST_VIDEO_OVERLAY(element), (guintptr)handle);
  gst_video_overlay_handle_events(GST_VIDEO_OVERLAY(element), FALSE);
  if (width > 0 && height > 0) {
    gst_video_overlay_set_render_rectangle(GST_VIDEO_OVERLAY(element),
                                           0,
                                           0,
                                           (gint)width,
                                           (gint)height);
  }
}

static GstBusSyncReply nl_linux_bus_sync(GstBus* bus,
                                         GstMessage* message,
                                         gpointer data) {
  nl_linux_video_context_t* context = (nl_linux_video_context_t*)data;
  (void)bus;
  if (context == NULL) return GST_BUS_PASS;

  if (GST_MESSAGE_TYPE(message) == GST_MESSAGE_NEED_CONTEXT) {
    const gchar* context_type = NULL;
    if (gst_message_parse_context_type(message, &context_type) &&
        context_type != NULL &&
        strcmp(context_type, NL_WAYLAND_DISPLAY_CONTEXT_TYPE) == 0) {
      uintptr_t display_handle;
      pthread_mutex_lock(&context->mutex);
      display_handle = context->display_handle;
      pthread_mutex_unlock(&context->mutex);
      if (display_handle != 0 && GST_IS_ELEMENT(GST_MESSAGE_SRC(message))) {
        nl_linux_apply_wayland_display_context(GST_ELEMENT(GST_MESSAGE_SRC(message)),
                                               display_handle);
        gst_message_unref(message);
        return GST_BUS_DROP;
      }
    }
  }

  if (gst_is_video_overlay_prepare_window_handle_message(message)) {
    uintptr_t window_handle;
    uint32_t width;
    uint32_t height;
    pthread_mutex_lock(&context->mutex);
    window_handle = context->window_handle;
    width = context->surface_width;
    height = context->surface_height;
    pthread_mutex_unlock(&context->mutex);
    nl_linux_set_overlay_handle(GST_ELEMENT(GST_MESSAGE_SRC(message)),
                                window_handle,
                                width,
                                height);
    gst_message_unref(message);
    return GST_BUS_DROP;
  }
  return GST_BUS_PASS;
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
  context->decoder = NULL;
  context->render_queue = NULL;
  context->sink = NULL;
  context->decoder_name[0] = '\0';
  context->sink_name[0] = '\0';
  nl_linux_reset_latency_state(context);
}

static int nl_linux_sink_count(const nl_linux_video_context_t* context) {
  if (context == NULL) return 0;
  if (context->surface_type == NL_SURFACE_X11_WINDOW) return NL_X11_SINK_COUNT;
  if (context->surface_type == NL_SURFACE_WAYLAND_SURFACE) return NL_WAYLAND_SINK_COUNT;
  return 0;
}

static bool nl_linux_get_sink_description(
    const nl_linux_video_context_t* context,
    int sink_index,
    nl_linux_sink_description_t* description) {
  if (context == NULL || description == NULL || sink_index < 0) return false;
  if (context->surface_type == NL_SURFACE_X11_WINDOW &&
      sink_index < NL_X11_SINK_COUNT) {
    *description = NL_X11_SINKS[sink_index];
    return true;
  }
  if (context->surface_type == NL_SURFACE_WAYLAND_SURFACE &&
      sink_index < NL_WAYLAND_SINK_COUNT) {
    *description = NL_WAYLAND_SINKS[sink_index];
    return true;
  }
  return false;
}

static bool nl_linux_message_from_sink(const nl_linux_video_context_t* context,
                                       const GstMessage* message) {
  GstObject* source;
  if (context == NULL || context->sink == NULL || message == NULL) return false;
  source = GST_MESSAGE_SRC(message);
  if (source == NULL) return false;
  return source == GST_OBJECT(context->sink) ||
         gst_object_has_as_ancestor(source, GST_OBJECT(context->sink));
}

static void nl_linux_unref_unowned_element(GstElement** element) {
  if (element != NULL && *element != NULL) {
    gst_object_unref(*element);
    *element = NULL;
  }
}

static int nl_linux_build_pipeline(nl_linux_video_context_t* context,
                                   const nl_linux_codec_description_t* codec,
                                   const char* decoder_name,
                                   bool software_decoder,
                                   int sink_index) {
  GstElement* pipeline = NULL;
  GstElement* appsrc = NULL;
  GstElement* parser = NULL;
  GstElement* decoder = NULL;
  GstElement* render_queue = NULL;
  GstElement* converter = NULL;
  GstElement* sink = NULL;
  GstBus* bus = NULL;
  GstCaps* caps = NULL;
  GstMessage* validation_error = NULL;
  GstStateChangeReturn state_result;
  nl_linux_sink_description_t sink_description;
  bool elements_owned_by_pipeline = false;
  uint8_t smoothing_capacity;

  if (context == NULL || codec == NULL || decoder_name == NULL ||
      !nl_linux_get_sink_description(context, sink_index, &sink_description)) {
    return -1;
  }
  if (!nl_linux_factory_exists(sink_description.name)) return -1;

  nl_linux_destroy_pipeline(context);
  smoothing_capacity = nl_linux_smoothing_capacity(context->renderer);
  pthread_mutex_lock(&context->mutex);
  context->smoothing_capacity = smoothing_capacity;
  pthread_mutex_unlock(&context->mutex);
  pipeline = gst_pipeline_new("noland-video-pipeline");
  appsrc = gst_element_factory_make("appsrc", "noland-source");
  parser = gst_element_factory_make(codec->parser, "noland-parser");
  decoder = gst_element_factory_make(decoder_name, "noland-decoder");
  render_queue = gst_element_factory_make("queue", "noland-render-queue");
  sink = gst_element_factory_make(sink_description.name, "noland-sink");
  if (sink_description.needs_converter) {
    converter = gst_element_factory_make("videoconvert", "noland-converter");
  }

  if (pipeline == NULL || appsrc == NULL || parser == NULL || decoder == NULL ||
      render_queue == NULL || sink == NULL ||
      (sink_description.needs_converter && converter == NULL)) {
    g_printerr("[noland-video] missing GStreamer element for decoder=%s sink=%s\n",
               decoder_name,
               sink_description.name);
    goto fail;
  }

  g_object_set(appsrc,
               "is-live", TRUE,
               "format", GST_FORMAT_TIME,
               "block", FALSE,
               NULL);
  gst_app_src_set_stream_type(GST_APP_SRC(appsrc), GST_APP_STREAM_TYPE_STREAM);
  gst_app_src_set_max_bytes(GST_APP_SRC(appsrc), NL_INPUT_MAX_BYTES);

  g_object_set(render_queue,
               "max-size-buffers", 1U + (guint)smoothing_capacity,
               "max-size-bytes", 0U,
               "max-size-time", (guint64)0,
               "leaky", 2,
               NULL);
  g_object_set(sink, "sync", FALSE, NULL);
  if (g_object_class_find_property(G_OBJECT_GET_CLASS(sink), "force-aspect-ratio") != NULL) {
    g_object_set(sink, "force-aspect-ratio", TRUE, NULL);
  }
  if (g_object_class_find_property(G_OBJECT_GET_CLASS(parser), "config-interval") != NULL) {
    g_object_set(parser, "config-interval", -1, NULL);
  }

  caps = gst_caps_new_simple(codec->media_type,
                             "stream-format", G_TYPE_STRING, codec->stream_format,
                             "alignment", G_TYPE_STRING, codec->alignment,
                             "width", G_TYPE_INT, context->width,
                             "height", G_TYPE_INT, context->height,
                             "framerate", GST_TYPE_FRACTION,
                             context->redraw_rate > 0 ? context->redraw_rate : 60,
                             1,
                             NULL);
  if (caps == NULL) goto fail;
  gst_app_src_set_caps(GST_APP_SRC(appsrc), caps);
  gst_caps_unref(caps);
  caps = NULL;

  if (converter != NULL) {
    gst_bin_add_many(GST_BIN(pipeline),
                     appsrc,
                     parser,
                     decoder,
                     render_queue,
                     converter,
                     sink,
                     NULL);
  } else {
    gst_bin_add_many(GST_BIN(pipeline),
                     appsrc,
                     parser,
                     decoder,
                     render_queue,
                     sink,
                     NULL);
  }
  elements_owned_by_pipeline = true;

  if (converter != NULL) {
    if (!gst_element_link_many(appsrc,
                               parser,
                               decoder,
                               render_queue,
                               converter,
                               sink,
                               NULL)) {
      g_printerr("[noland-video] failed to link decoder=%s to sink=%s with conversion\n",
                 decoder_name,
                 sink_description.name);
      goto fail;
    }
  } else if (!gst_element_link_many(appsrc,
                                    parser,
                                    decoder,
                                    render_queue,
                                    sink,
                                    NULL)) {
    g_printerr("[noland-video] failed to link decoder=%s directly to sink=%s\n",
               decoder_name,
               sink_description.name);
    goto fail;
  }

  if (!nl_linux_add_buffer_probe(decoder,
                                 "src",
                                 nl_linux_post_decoder_probe,
                                 context) ||
      !nl_linux_add_buffer_probe(sink,
                                 "sink",
                                 nl_linux_pre_sink_probe,
                                 context)) {
    g_printerr("[noland-video] failed to install latency probes for decoder=%s sink=%s\n",
               decoder_name,
               sink_description.name);
    goto fail;
  }
  g_signal_connect(render_queue,
                   "overrun",
                   G_CALLBACK(nl_linux_render_queue_overrun),
                   context);

  if (context->surface_type == NL_SURFACE_WAYLAND_SURFACE &&
      context->display_handle != 0) {
    nl_linux_apply_wayland_display_context(pipeline, context->display_handle);
    nl_linux_apply_wayland_display_context(sink, context->display_handle);
  }
  nl_linux_set_overlay_handle(sink,
                              context->window_handle,
                              context->surface_width,
                              context->surface_height);

  bus = gst_element_get_bus(pipeline);
  if (bus == NULL) goto fail;
  gst_bus_set_sync_handler(bus, nl_linux_bus_sync, context, NULL);

  context->pipeline = pipeline;
  context->appsrc = GST_APP_SRC(gst_object_ref(appsrc));
  context->bus = bus;
  context->decoder = decoder;
  context->render_queue = render_queue;
  context->sink = sink;
  context->using_software_decoder = software_decoder;
  context->sink_index = sink_index;
  snprintf(context->decoder_name, sizeof(context->decoder_name), "%s", decoder_name);
  snprintf(context->sink_name,
           sizeof(context->sink_name),
           "%s",
           sink_description.name);

  state_result = gst_element_set_state(context->pipeline, GST_STATE_PAUSED);
  if (state_result == GST_STATE_CHANGE_FAILURE) {
    g_printerr("[noland-video] decoder=%s sink=%s failed to enter PAUSED\n",
               decoder_name,
               sink_description.name);
    nl_linux_destroy_pipeline(context);
    return -1;
  }
  state_result = gst_element_get_state(context->pipeline,
                                       NULL,
                                       NULL,
                                       NL_SINK_READY_TIMEOUT);
  validation_error = gst_bus_pop_filtered(context->bus, GST_MESSAGE_ERROR);
  if (state_result == GST_STATE_CHANGE_FAILURE || validation_error != NULL) {
    if (validation_error != NULL) {
      GError* error = NULL;
      gchar* debug = NULL;
      gst_message_parse_error(validation_error, &error, &debug);
      g_printerr("[noland-video] decoder=%s sink=%s failed PAUSED validation: %s (%s)\n",
                 decoder_name,
                 sink_description.name,
                 error != NULL ? error->message : "unknown",
                 debug != NULL ? debug : "no details");
      if (error != NULL) g_error_free(error);
      g_free(debug);
      gst_message_unref(validation_error);
    }
    nl_linux_destroy_pipeline(context);
    return -1;
  }

  g_printerr("[noland-video] selected %s decoder=%s sink=%s\n",
             software_decoder ? "software" : "hardware",
             decoder_name,
             sink_description.name);
  return 0;

fail:
  if (caps != NULL) gst_caps_unref(caps);
  if (bus != NULL) {
    gst_bus_set_sync_handler(bus, NULL, NULL, NULL);
    gst_object_unref(bus);
  }
  if (pipeline != NULL) {
    gst_element_set_state(pipeline, GST_STATE_NULL);
    gst_object_unref(pipeline);
  }
  if (!elements_owned_by_pipeline) {
    nl_linux_unref_unowned_element(&appsrc);
    nl_linux_unref_unowned_element(&parser);
    nl_linux_unref_unowned_element(&decoder);
    nl_linux_unref_unowned_element(&render_queue);
    nl_linux_unref_unowned_element(&converter);
    nl_linux_unref_unowned_element(&sink);
  }
  return -1;
}

static int nl_linux_try_decoder_with_sinks(
    nl_linux_video_context_t* context,
    const nl_linux_codec_description_t* codec,
    const char* decoder_name,
    bool software_decoder,
    int first_sink_index) {
  int sink_index;
  int sink_count = nl_linux_sink_count(context);
  if (first_sink_index < 0) first_sink_index = 0;
  for (sink_index = first_sink_index; sink_index < sink_count; sink_index++) {
    if ((context->disabled_sinks & (1U << sink_index)) != 0) continue;
    if (nl_linux_build_pipeline(context,
                                codec,
                                decoder_name,
                                software_decoder,
                                sink_index) == 0) {
      return 0;
    }
  }
  return -1;
}

static int nl_linux_create_pipeline(nl_linux_video_context_t* context,
                                    bool force_software,
                                    int first_sink_index) {
  nl_linux_codec_description_t codec;
  const char* const* candidate;
  if (context == NULL) return -1;
  if (!nl_linux_describe_codec(context->video_format, &codec)) {
    g_printerr("[noland-video] unsupported Linux video format mask: 0x%x\n",
               context->video_format);
    return -1;
  }
  if (!nl_linux_factory_exists(codec.parser)) {
    g_printerr("[noland-video] required parser is unavailable: %s\n", codec.parser);
    return -1;
  }

  nl_linux_destroy_pipeline(context);
  if (!force_software) {
    for (candidate = codec.hardware_decoders; *candidate != NULL; candidate++) {
      if (!nl_linux_factory_exists(*candidate)) continue;
      if (nl_linux_try_decoder_with_sinks(context,
                                          &codec,
                                          *candidate,
                                          false,
                                          first_sink_index) == 0) {
        return 0;
      }
    }
  }

  if (!nl_linux_factory_exists(codec.software_decoder)) {
    g_printerr("[noland-video] deterministic software fallback is unavailable: %s\n",
               codec.software_decoder);
    return -1;
  }
  if (nl_linux_try_decoder_with_sinks(context,
                                      &codec,
                                      codec.software_decoder,
                                      true,
                                      first_sink_index) != 0) {
    return -1;
  }
  if (!force_software) context->fallback_attempted = true;
  return 0;
}

static bool nl_linux_is_running(nl_linux_video_context_t* context) {
  bool running;
  if (context == NULL) return false;
  pthread_mutex_lock(&context->mutex);
  running = context->running;
  pthread_mutex_unlock(&context->mutex);
  return running;
}

static void nl_linux_begin_idr_recovery(nl_linux_video_context_t* context) {
  if (context == NULL) return;
  pthread_mutex_lock(&context->mutex);
  context->awaiting_idr = true;
  pthread_mutex_unlock(&context->mutex);
  LiRequestIdrFrame();
}

static bool nl_linux_rebuild_with_next_sink(nl_linux_video_context_t* context) {
  nl_linux_codec_description_t codec;
  char decoder_name[NL_DECODER_NAME_MAX];
  bool software_decoder;
  bool should_rebuild;
  int failed_sink_index;

  if (context == NULL || context->surface_type != NL_SURFACE_X11_WINDOW) return false;
  pthread_mutex_lock(&context->mutex);
  failed_sink_index = context->sink_index;
  should_rebuild = context->running &&
                   failed_sink_index >= 0 &&
                   failed_sink_index + 1 < nl_linux_sink_count(context);
  software_decoder = context->using_software_decoder;
  snprintf(decoder_name, sizeof(decoder_name), "%s", context->decoder_name);
  if (failed_sink_index >= 0) {
    context->disabled_sinks |= 1U << failed_sink_index;
  }
  pthread_mutex_unlock(&context->mutex);
  if (!should_rebuild || decoder_name[0] == '\0') return false;
  if (!nl_linux_describe_codec(context->video_format, &codec)) return false;

  g_printerr("[noland-video] advancing X11 sink after failure of %s\n",
             context->sink_name[0] != '\0' ? context->sink_name : "unknown");
  if (nl_linux_try_decoder_with_sinks(context,
                                      &codec,
                                      decoder_name,
                                      software_decoder,
                                      failed_sink_index + 1) != 0) {
    return false;
  }
  if (gst_element_set_state(context->pipeline, GST_STATE_PLAYING) ==
      GST_STATE_CHANGE_FAILURE) {
    pthread_mutex_lock(&context->mutex);
    if (context->sink_index >= 0) {
      context->disabled_sinks |= 1U << context->sink_index;
    }
    pthread_mutex_unlock(&context->mutex);
    return false;
  }
  nl_linux_begin_idr_recovery(context);
  return true;
}

static bool nl_linux_rebuild_with_software(nl_linux_video_context_t* context) {
  bool should_rebuild;
  if (context == NULL) return false;

  pthread_mutex_lock(&context->mutex);
  should_rebuild = context->running &&
                   !context->using_software_decoder &&
                   !context->fallback_attempted;
  if (should_rebuild) context->fallback_attempted = true;
  pthread_mutex_unlock(&context->mutex);
  if (!should_rebuild) return false;

  g_printerr("[noland-video] rebuilding failed hardware pipeline with software decoder\n");
  if (nl_linux_create_pipeline(context, true, 0) != 0 ||
      gst_element_set_state(context->pipeline, GST_STATE_PLAYING) ==
          GST_STATE_CHANGE_FAILURE) {
    g_printerr("[noland-video] software decoder fallback failed\n");
    pthread_mutex_lock(&context->mutex);
    context->fatal_error = true;
    context->running = false;
    pthread_mutex_unlock(&context->mutex);
    LiWakeWaitForVideoFrame();
    return false;
  }

  nl_linux_begin_idr_recovery(context);
  return true;
}

static void nl_linux_process_bus_messages(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  GstMessage* message;
  bool fatal_error = false;
  bool sink_error = false;
  if (context == NULL || context->bus == NULL) return;

  while ((message = gst_bus_pop_filtered(
              context->bus,
              (GstMessageType)(GST_MESSAGE_ERROR | GST_MESSAGE_WARNING))) != NULL) {
    GError* error = NULL;
    gchar* debug = NULL;
    if (GST_MESSAGE_TYPE(message) == GST_MESSAGE_ERROR) {
      gst_message_parse_error(message, &error, &debug);
      g_printerr("[noland-video] GStreamer fatal error from %s: %s (%s)\n",
                 GST_OBJECT_NAME(GST_MESSAGE_SRC(message)),
                 error != NULL ? error->message : "unknown",
                 debug != NULL ? debug : "no details");
      sink_error = nl_linux_message_from_sink(context, message);
      fatal_error = true;
    } else {
      gst_message_parse_warning(message, &error, &debug);
      g_printerr("[noland-video] GStreamer warning from %s: %s (%s)\n",
                 GST_OBJECT_NAME(GST_MESSAGE_SRC(message)),
                 error != NULL ? error->message : "unknown",
                 debug != NULL ? debug : "no details");
    }
    if (error != NULL) g_error_free(error);
    g_free(debug);
    gst_message_unref(message);
    if (fatal_error) break;
  }

  if (!fatal_error || !nl_linux_is_running(context)) return;
  if (sink_error && nl_linux_rebuild_with_next_sink(context)) return;
  if (nl_linux_rebuild_with_software(context)) return;

  pthread_mutex_lock(&context->mutex);
  context->fatal_error = true;
  context->running = false;
  pthread_mutex_unlock(&context->mutex);
  LiWakeWaitForVideoFrame();
}

static void* nl_linux_frame_thread(void* data) {
  nl_video_renderer_t* renderer = (nl_video_renderer_t*)data;

  while (renderer != NULL) {
    nl_linux_video_context_t* context = nl_linux_context(renderer);
    VIDEO_FRAME_HANDLE handle = NULL;
    PDECODE_UNIT decode_unit = NULL;
    nl_video_frame_metadata_t metadata;
    int result;

    if (context == NULL || !nl_linux_is_running(context)) break;
    nl_linux_process_bus_messages(renderer);
    if (!nl_linux_is_running(context)) break;

    if (!LiWaitForNextVideoFrame(&handle, &decode_unit)) {
      if (!nl_linux_is_running(context)) break;
      continue;
    }
    if (decode_unit == NULL) {
      if (handle != NULL) LiCompleteVideoFrame(handle, DR_NEED_IDR);
      continue;
    }

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
    nl_linux_process_bus_messages(renderer);
  }
  return NULL;
}

void nl_video_renderer_platform_attach_surface(nl_video_renderer_t* renderer,
                                               const nl_surface_descriptor_t* surface) {
  nl_linux_video_context_t* context = nl_linux_ensure_context(renderer);
  GstElement* pipeline;
  GstElement* sink;
  if (context == NULL || surface == NULL || surface->window_handle == NULL) return;
  if (surface->surface_type != NL_SURFACE_X11_WINDOW &&
      surface->surface_type != NL_SURFACE_WAYLAND_SURFACE) {
    return;
  }

  pthread_mutex_lock(&context->mutex);
  context->window_handle = (uintptr_t)surface->window_handle;
  context->display_handle = (uintptr_t)surface->display_handle;
  context->surface_type = surface->surface_type;
  context->surface_width = surface->width;
  context->surface_height = surface->height;
  pipeline = context->pipeline;
  sink = context->sink;
  pthread_mutex_unlock(&context->mutex);

  if (surface->surface_type == NL_SURFACE_WAYLAND_SURFACE &&
      surface->display_handle != NULL) {
    nl_linux_apply_wayland_display_context(pipeline,
                                           (uintptr_t)surface->display_handle);
    nl_linux_apply_wayland_display_context(sink,
                                           (uintptr_t)surface->display_handle);
  }
  nl_linux_set_overlay_handle(sink,
                              (uintptr_t)surface->window_handle,
                              surface->width,
                              surface->height);
}

void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL) return;
  pthread_mutex_lock(&context->mutex);
  context->window_handle = 0;
  context->display_handle = 0;
  context->surface_type = NL_SURFACE_TYPE_UNKNOWN;
  context->surface_width = 0;
  context->surface_height = 0;
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
  context->fallback_attempted = false;
  context->awaiting_idr = false;
  context->fatal_error = false;
  context->sink_index = NL_SINK_INDEX_NONE;
  context->disabled_sinks = 0;
  pthread_mutex_unlock(&context->mutex);
  return nl_linux_create_pipeline(context, false, 0);
}

void nl_video_renderer_platform_start(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  if (context == NULL || context->pipeline == NULL || context->frame_thread_started) return;

  pthread_mutex_lock(&context->mutex);
  context->running = true;
  pthread_mutex_unlock(&context->mutex);

  if (gst_element_set_state(context->pipeline, GST_STATE_PLAYING) ==
      GST_STATE_CHANGE_FAILURE) {
    if (!nl_linux_rebuild_with_next_sink(context) &&
        !nl_linux_rebuild_with_software(context)) {
      g_printerr("[noland-video] failed to start GStreamer video pipeline\n");
      pthread_mutex_lock(&context->mutex);
      context->running = false;
      pthread_mutex_unlock(&context->mutex);
      return;
    }
  }

  if (pthread_create(&context->frame_thread, NULL, nl_linux_frame_thread, renderer) == 0) {
    context->frame_thread_started = true;
  } else {
    pthread_mutex_lock(&context->mutex);
    context->running = false;
    pthread_mutex_unlock(&context->mutex);
    LiWakeWaitForVideoFrame();
    gst_element_set_state(context->pipeline, GST_STATE_NULL);
  }
}

void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_linux_video_context_t* context = nl_linux_context(renderer);
  GstAppSrc* appsrc;
  GstElement* pipeline;
  if (context == NULL) return;

  pthread_mutex_lock(&context->mutex);
  context->running = false;
  appsrc = context->appsrc;
  pipeline = context->pipeline;
  pthread_mutex_unlock(&context->mutex);

  LiWakeWaitForVideoFrame();
  if (appsrc != NULL) {
    g_object_set(appsrc, "block", FALSE, NULL);
    gst_app_src_end_of_stream(appsrc);
  }
  if (pipeline != NULL) gst_element_set_state(pipeline, GST_STATE_NULL);
  if (context->frame_thread_started) {
    pthread_join(context->frame_thread, NULL);
    context->frame_thread_started = false;
  }
  nl_linux_reset_latency_state(context);
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
  guint64 queued_bytes;
  GstFlowReturn flow;
  bool awaiting_idr;
  bool fatal_error;

  if (context == NULL || context->appsrc == NULL || decode_unit == NULL) {
    return DR_NEED_IDR;
  }

  pthread_mutex_lock(&context->mutex);
  awaiting_idr = context->awaiting_idr;
  fatal_error = context->fatal_error;
  if (awaiting_idr && decode_unit->frameType == FRAME_TYPE_IDR) {
    context->awaiting_idr = false;
    awaiting_idr = false;
  }
  pthread_mutex_unlock(&context->mutex);
  if (fatal_error || awaiting_idr) return DR_NEED_IDR;

  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    if (entry->data != NULL && entry->length > 0) total += (size_t)entry->length;
  }
  if (total == 0) return DR_NEED_IDR;

  queued_bytes = gst_app_src_get_current_level_bytes(context->appsrc);
  if (total > NL_INPUT_MAX_BYTES ||
      queued_bytes > (guint64)NL_INPUT_MAX_BYTES - (guint64)total) {
    g_printerr("[noland-video] rejecting compressed AU of %zu bytes at queued level "
               "%" G_GUINT64_FORMAT "/%u; requesting IDR\n",
               total,
               queued_bytes,
               NL_INPUT_MAX_BYTES);
    nl_linux_begin_idr_recovery(context);
    return DR_NEED_IDR;
  }

  buffer = gst_buffer_new_allocate(NULL, total, NULL);
  if (buffer == NULL || !gst_buffer_map(buffer, &map, GST_MAP_WRITE)) {
    if (buffer != NULL) gst_buffer_unref(buffer);
    nl_linux_begin_idr_recovery(context);
    return DR_NEED_IDR;
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
    GST_BUFFER_DURATION(buffer) =
        gst_util_uint64_scale_int(1, GST_SECOND, context->redraw_rate);
  }
  if (decode_unit->frameType == FRAME_TYPE_IDR) {
    GST_BUFFER_FLAG_UNSET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
  } else {
    GST_BUFFER_FLAG_SET(buffer, GST_BUFFER_FLAG_DELTA_UNIT);
  }

  flow = gst_app_src_push_buffer(context->appsrc, buffer);
  (void)frame;
  if (flow != GST_FLOW_OK) {
    nl_linux_begin_idr_recovery(context);
    return DR_NEED_IDR;
  }
  return DR_OK;
}
