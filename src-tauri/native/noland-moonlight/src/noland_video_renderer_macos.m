#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <QuartzCore/QuartzCore.h>
#import <dispatch/dispatch.h>
#import <CoreVideo/CoreVideo.h>

#include "noland_video_renderer.h"
#include "Limelight.h"

#include <stdlib.h>
#include <string.h>
#include <stdint.h>

@interface NolandSampleDisplayLayer : AVSampleBufferDisplayLayer
@end

@implementation NolandSampleDisplayLayer
- (void)layoutSublayers {
  [super layoutSublayers];
}
@end

typedef struct nl_macos_video_context {
  __unsafe_unretained NSView* view;
  __strong AVSampleBufferDisplayLayer* layer;
  CMVideoFormatDescriptionRef format_description;
  uint8_t* sps;
  size_t sps_len;
  uint8_t* pps;
  size_t pps_len;
  uint8_t* vps;
  size_t vps_len;
  int video_format;
  int width;
  int height;
  int redraw_rate;
  CVDisplayLinkRef display_link;
  CFTimeInterval layer_not_ready_since;
  uint64_t consecutive_layer_not_ready_frames;
} nl_macos_video_context_t;

static void nl_run_on_main_sync(dispatch_block_t block) {
  if ([NSThread isMainThread]) {
    block();
  } else {
    dispatch_sync(dispatch_get_main_queue(), block);
  }
}

static nl_macos_video_context_t* nl_macos_context(nl_video_renderer_t* renderer) {
  if (renderer == NULL) {
    return NULL;
  }
  return (nl_macos_video_context_t*)renderer->platform_context;
}

static nl_macos_video_context_t* nl_macos_ensure_context(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context != NULL) {
    return context;
  }
  context = calloc(1, sizeof(*context));
  if (context == NULL) {
    return NULL;
  }
  renderer->platform_context = context;
  return context;
}

static CFTimeInterval nl_macos_now(void) {
  return CACurrentMediaTime();
}

static void nl_macos_reset_layer_backpressure(nl_macos_video_context_t* context) {
  if (context == NULL) {
    return;
  }
  context->layer_not_ready_since = 0;
  context->consecutive_layer_not_ready_frames = 0;
}

static bool nl_macos_should_recover_for_backpressure(nl_macos_video_context_t* context) {
  CFTimeInterval now;
  if (context == NULL) {
    return false;
  }
  now = nl_macos_now();
  if (context->layer_not_ready_since <= 0) {
    context->layer_not_ready_since = now;
  }
  context->consecutive_layer_not_ready_frames += 1U;
  return context->consecutive_layer_not_ready_frames >= 60U &&
         now - context->layer_not_ready_since >= 2.0;
}

static void nl_macos_free_parameter_set(uint8_t** bytes, size_t* length) {
  if (bytes != NULL && *bytes != NULL) {
    free(*bytes);
    *bytes = NULL;
  }
  if (length != NULL) {
    *length = 0;
  }
}

static void nl_macos_reset_format_description(nl_macos_video_context_t* context) {
  if (context == NULL) {
    return;
  }
  if (context->format_description != NULL) {
    CFRelease(context->format_description);
    context->format_description = NULL;
  }
}

static void nl_macos_store_parameter_set(uint8_t** dst, size_t* dst_len, const uint8_t* src, size_t src_len) {
  uint8_t* next = NULL;
  if (src == NULL || src_len == 0 || dst == NULL || dst_len == NULL) {
    return;
  }
  next = malloc(src_len);
  if (next == NULL) {
    return;
  }
  memcpy(next, src, src_len);
  nl_macos_free_parameter_set(dst, dst_len);
  *dst = next;
  *dst_len = src_len;
}

static bool nl_strip_annexb_start_code(const uint8_t* data, size_t length, const uint8_t** payload, size_t* payload_len) {
  size_t offset = 0;
  if (data == NULL || payload == NULL || payload_len == NULL || length == 0) {
    return false;
  }
  if (length >= 4 && data[0] == 0x00 && data[1] == 0x00 && data[2] == 0x00 && data[3] == 0x01) {
    offset = 4;
  } else if (length >= 3 && data[0] == 0x00 && data[1] == 0x00 && data[2] == 0x01) {
    offset = 3;
  }
  if (offset >= length) {
    return false;
  }
  *payload = data + offset;
  *payload_len = length - offset;
  return true;
}

static bool nl_find_next_annexb_nal(const uint8_t* data, size_t length, size_t* cursor, const uint8_t** nal, size_t* nal_len) {
  size_t i;
  size_t start = SIZE_MAX;
  size_t prefix = 0;
  if (data == NULL || cursor == NULL || nal == NULL || nal_len == NULL) {
    return false;
  }

  for (i = *cursor; i + 3 < length; ++i) {
    if (i + 4 <= length && data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x00 && data[i + 3] == 0x01) {
      start = i + 4;
      prefix = 4;
      break;
    }
    if (data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x01) {
      start = i + 3;
      prefix = 3;
      break;
    }
  }

  if (start == SIZE_MAX || start >= length) {
    return false;
  }

  for (i = start; i + 3 < length; ++i) {
    if (i + 4 <= length && data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x00 && data[i + 3] == 0x01) {
      break;
    }
    if (data[i] == 0x00 && data[i + 1] == 0x00 && data[i + 2] == 0x01) {
      break;
    }
  }

  *nal = data + start;
  *nal_len = i - start;
  *cursor = i;
  (void)prefix;
  return *nal_len > 0;
}

static bool nl_macos_update_format_description(nl_macos_video_context_t* context) {
  CMVideoFormatDescriptionRef format_description = NULL;
  OSStatus status;

  if (context == NULL) {
    return false;
  }

  if (context->video_format & VIDEO_FORMAT_MASK_H264) {
    const uint8_t* parameter_sets[2];
    size_t parameter_set_sizes[2];

    if (context->sps == NULL || context->pps == NULL || context->sps_len == 0 || context->pps_len == 0) {
      return false;
    }

    parameter_sets[0] = context->sps;
    parameter_sets[1] = context->pps;
    parameter_set_sizes[0] = context->sps_len;
    parameter_set_sizes[1] = context->pps_len;

    status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
        kCFAllocatorDefault,
        2,
        parameter_sets,
        parameter_set_sizes,
        4,
        &format_description);
  } else if (context->video_format & VIDEO_FORMAT_MASK_H265) {
    size_t parameter_set_count = 0;
    const uint8_t* parameter_sets[3];
    size_t parameter_set_sizes[3];

    if (context->vps != NULL && context->vps_len > 0) {
      parameter_sets[parameter_set_count] = context->vps;
      parameter_set_sizes[parameter_set_count] = context->vps_len;
      parameter_set_count++;
    }
    if (context->sps != NULL && context->sps_len > 0) {
      parameter_sets[parameter_set_count] = context->sps;
      parameter_set_sizes[parameter_set_count] = context->sps_len;
      parameter_set_count++;
    }
    if (context->pps != NULL && context->pps_len > 0) {
      parameter_sets[parameter_set_count] = context->pps;
      parameter_set_sizes[parameter_set_count] = context->pps_len;
      parameter_set_count++;
    }
    if (parameter_set_count == 0) {
      return false;
    }

    status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        kCFAllocatorDefault,
        parameter_set_count,
        parameter_sets,
        parameter_set_sizes,
        4,
        NULL,
        &format_description);
  } else {
    return false;
  }

  if (status != noErr || format_description == NULL) {
    return false;
  }

  nl_macos_reset_format_description(context);
  context->format_description = format_description;
  return true;
}

static void nl_macos_collect_parameter_sets(nl_macos_video_context_t* context, const DECODE_UNIT* decode_unit) {
  const LENTRY* entry;
  if (context == NULL || decode_unit == NULL) {
    return;
  }
  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    const uint8_t* payload = NULL;
    size_t payload_len = 0;
    if (!nl_strip_annexb_start_code((const uint8_t*)entry->data, (size_t)entry->length, &payload, &payload_len)) {
      continue;
    }
    if (entry->bufferType == BUFFER_TYPE_SPS) {
      nl_macos_store_parameter_set(&context->sps, &context->sps_len, payload, payload_len);
    } else if (entry->bufferType == BUFFER_TYPE_PPS) {
      nl_macos_store_parameter_set(&context->pps, &context->pps_len, payload, payload_len);
    } else if (entry->bufferType == BUFFER_TYPE_VPS) {
      nl_macos_store_parameter_set(&context->vps, &context->vps_len, payload, payload_len);
    }
  }
  nl_macos_update_format_description(context);
}

static uint8_t* nl_macos_build_avcc_sample(const DECODE_UNIT* decode_unit, size_t* output_length) {
  size_t annexb_length = 0;
  uint8_t* annexb = NULL;
  uint8_t* cursor_ptr = NULL;
  size_t cursor = 0;
  size_t avcc_length = 0;
  uint8_t* avcc = NULL;
  const LENTRY* entry;

  if (output_length != NULL) {
    *output_length = 0;
  }
  if (decode_unit == NULL || output_length == NULL) {
    return NULL;
  }

  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    if (entry->bufferType == BUFFER_TYPE_PICDATA) {
      annexb_length += (size_t)entry->length;
    }
  }
  if (annexb_length == 0) {
    return NULL;
  }

  annexb = malloc(annexb_length);
  if (annexb == NULL) {
    return NULL;
  }

  cursor_ptr = annexb;
  for (entry = decode_unit->bufferList; entry != NULL; entry = entry->next) {
    if (entry->bufferType == BUFFER_TYPE_PICDATA) {
      memcpy(cursor_ptr, entry->data, (size_t)entry->length);
      cursor_ptr += entry->length;
    }
  }

  cursor = 0;
  while (cursor < annexb_length) {
    const uint8_t* nal = NULL;
    size_t nal_len = 0;
    if (!nl_find_next_annexb_nal(annexb, annexb_length, &cursor, &nal, &nal_len)) {
      break;
    }
    avcc_length += 4 + nal_len;
  }

  if (avcc_length == 0) {
    free(annexb);
    return NULL;
  }

  avcc = malloc(avcc_length);
  if (avcc == NULL) {
    free(annexb);
    return NULL;
  }

  cursor = 0;
  size_t write_offset = 0;
  while (cursor < annexb_length) {
    const uint8_t* nal = NULL;
    size_t nal_len = 0;
    uint32_t be_len;
    if (!nl_find_next_annexb_nal(annexb, annexb_length, &cursor, &nal, &nal_len)) {
      break;
    }
    be_len = CFSwapInt32HostToBig((uint32_t)nal_len);
    memcpy(avcc + write_offset, &be_len, 4);
    write_offset += 4;
    memcpy(avcc + write_offset, nal, nal_len);
    write_offset += nal_len;
  }

  free(annexb);
  *output_length = avcc_length;
  return avcc;
}

static CMSampleBufferRef nl_macos_create_sample_buffer(nl_macos_video_context_t* context, const DECODE_UNIT* decode_unit) {
  uint8_t* avcc = NULL;
  size_t avcc_length = 0;
  CMBlockBufferRef block_buffer = NULL;
  CMSampleBufferRef sample_buffer = NULL;
  CMSampleTimingInfo timing = {0};
  size_t sample_size;
  OSStatus status;
  CFArrayRef attachments;

  if (context == NULL || decode_unit == NULL || context->format_description == NULL) {
    return NULL;
  }

  avcc = nl_macos_build_avcc_sample(decode_unit, &avcc_length);
  if (avcc == NULL || avcc_length == 0) {
    free(avcc);
    return NULL;
  }

  status = CMBlockBufferCreateWithMemoryBlock(
      kCFAllocatorDefault,
      NULL,
      avcc_length,
      kCFAllocatorDefault,
      NULL,
      0,
      avcc_length,
      0,
      &block_buffer);
  if (status != noErr || block_buffer == NULL) {
    free(avcc);
    return NULL;
  }
  status = CMBlockBufferReplaceDataBytes(avcc, block_buffer, 0, avcc_length);
  free(avcc);
  if (status != noErr) {
    CFRelease(block_buffer);
    return NULL;
  }

  timing.duration = context->redraw_rate > 0 ? CMTimeMake(1, context->redraw_rate) : kCMTimeInvalid;
  timing.presentationTimeStamp = CMTimeMake((int64_t)decode_unit->presentationTimeUs, 1000000);
  timing.decodeTimeStamp = kCMTimeInvalid;
  sample_size = avcc_length;

  status = CMSampleBufferCreateReady(
      kCFAllocatorDefault,
      block_buffer,
      context->format_description,
      1,
      1,
      &timing,
      1,
      &sample_size,
      &sample_buffer);
  CFRelease(block_buffer);

  if (status != noErr || sample_buffer == NULL) {
    return NULL;
  }

  attachments = CMSampleBufferGetSampleAttachmentsArray(sample_buffer, YES);
  if (attachments != NULL && CFArrayGetCount(attachments) > 0) {
    CFMutableDictionaryRef attachment = (CFMutableDictionaryRef)CFArrayGetValueAtIndex(attachments, 0);
    if (attachment != NULL) {
      CFDictionarySetValue(attachment, kCMSampleAttachmentKey_DisplayImmediately, kCFBooleanTrue);
      if (decode_unit->frameType == FRAME_TYPE_IDR) {
        CFDictionarySetValue(attachment, kCMSampleAttachmentKey_NotSync, kCFBooleanFalse);
      }
    }
  }

  return sample_buffer;
}

void nl_video_renderer_platform_attach_surface(nl_video_renderer_t* renderer, const nl_surface_descriptor_t* surface) {
  nl_macos_video_context_t* context = nl_macos_ensure_context(renderer);
  if (context == NULL || surface == NULL || surface->surface_type != NL_SURFACE_MACOS_NSVIEW || surface->window_handle == NULL) {
    return;
  }
  if ((uintptr_t)surface->window_handle < 4096) {
    return;
  }
  context->view = (__bridge NSView*)surface->window_handle;
  nl_run_on_main_sync(^{
    NSView* view = context->view;
    if (view == nil) {
      return;
    }
    [view setWantsLayer:YES];
    if (view.layer == nil) {
      view.layer = [CALayer layer];
    }
    if (context->layer == nil) {
      context->layer = [NolandSampleDisplayLayer layer];
      context->layer.videoGravity = AVLayerVideoGravityResizeAspect;
      context->layer.backgroundColor = NSColor.blackColor.CGColor;
      context->layer.opaque = YES;
      context->layer.needsDisplayOnBoundsChange = YES;
      context->layer.frame = view.bounds;
      context->layer.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
      [view.layer addSublayer:context->layer];
    } else if (context->layer.superlayer != view.layer) {
      context->layer.frame = view.bounds;
      [view.layer addSublayer:context->layer];
    }
  });
}

void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  nl_run_on_main_sync(^{
    if (context->layer != nil) {
      [context->layer flushAndRemoveImage];
      [context->layer removeFromSuperlayer];
      context->layer = nil;
    }
  });
  nl_macos_reset_layer_backpressure(context);
  context->view = nil;
}

int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate) {
  nl_macos_video_context_t* context = nl_macos_ensure_context(renderer);
  if (context == NULL) {
    return -1;
  }
  if (context->video_format != video_format) {
    nl_macos_free_parameter_set(&context->sps, &context->sps_len);
    nl_macos_free_parameter_set(&context->pps, &context->pps_len);
    nl_macos_free_parameter_set(&context->vps, &context->vps_len);
  }
  context->video_format = video_format;
  context->width = width;
  context->height = height;
  context->redraw_rate = redraw_rate;
  nl_macos_reset_format_description(context);
  return 0;
}

static void nl_macos_recover_display_layer(nl_macos_video_context_t* context) {
  if (context == NULL || context->view == nil) {
    return;
  }
  if (context->layer != nil) {
    [context->layer flushAndRemoveImage];
    [context->layer removeFromSuperlayer];
    context->layer = nil;
  }
  context->layer = [NolandSampleDisplayLayer layer];
  context->layer.videoGravity = AVLayerVideoGravityResizeAspect;
  context->layer.backgroundColor = NSColor.blackColor.CGColor;
  context->layer.opaque = YES;
  context->layer.needsDisplayOnBoundsChange = YES;
  context->layer.frame = context->view.bounds;
  context->layer.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
  [context->view.layer addSublayer:context->layer];
  nl_macos_reset_format_description(context);
  nl_macos_reset_layer_backpressure(context);
}

static CVReturn nl_macos_display_link_callback(CVDisplayLinkRef displayLink,
                                                const CVTimeStamp* inNow,
                                                const CVTimeStamp* inOutputTime,
                                                CVOptionFlags flagsIn,
                                                CVOptionFlags* flagsOut,
                                                void* displayLinkContext) {
  nl_video_renderer_t* renderer = (nl_video_renderer_t*)displayLinkContext;
  nl_macos_video_context_t* context;
  VIDEO_FRAME_HANDLE handle;
  PDECODE_UNIT du;
  int result;
  nl_video_frame_metadata_t frame_metadata;

  (void)displayLink;
  (void)inNow;
  (void)inOutputTime;
  (void)flagsIn;
  (void)flagsOut;

  if (renderer == NULL || renderer->platform_context == NULL) {
    return kCVReturnSuccess;
  }

  context = (nl_macos_video_context_t*)renderer->platform_context;
  if (context->layer == nil) {
    return kCVReturnSuccess;
  }

  while (LiPollNextVideoFrame(&handle, &du)) {
    memset(&frame_metadata, 0, sizeof(frame_metadata));
    frame_metadata.frame_number = du->frameNumber;
    frame_metadata.frame_type = du->frameType;
    frame_metadata.full_length = du->fullLength;
    frame_metadata.host_processing_latency = du->frameHostProcessingLatency;
    frame_metadata.receive_time_us = du->receiveTimeUs;
    frame_metadata.enqueue_time_us = du->enqueueTimeUs;
    frame_metadata.presentation_time_us = du->presentationTimeUs;
    frame_metadata.rtp_timestamp = du->rtpTimestamp;
    frame_metadata.hdr_active = du->hdrActive ? 1U : 0U;
    frame_metadata.colorspace = du->colorspace;

    if (renderer->frame_processor != NULL) {
      result = renderer->frame_processor(renderer->frame_processor_user_data, du, &frame_metadata);
    } else {
      result = nl_video_renderer_submit_frame(renderer, du, &frame_metadata);
    }
    LiCompleteVideoFrame(handle, result);

    if (context->redraw_rate > 0) {
      if (LiGetPendingVideoFrames() == 1) {
        break;
      }
    }
  }

  return kCVReturnSuccess;
}

void nl_video_renderer_platform_start(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  nl_run_on_main_sync(^{
    if (context->layer != nil) {
      [context->layer flushAndRemoveImage];
    }
  });
  nl_macos_reset_layer_backpressure(context);

  if (context->display_link != NULL) {
    CVDisplayLinkStop(context->display_link);
    CVDisplayLinkRelease(context->display_link);
    context->display_link = NULL;
  }

  CVReturn err = CVDisplayLinkCreateWithActiveCGDisplays(&context->display_link);
  if (err != kCVReturnSuccess || context->display_link == NULL) {
    return;
  }
  CVDisplayLinkSetOutputCallback(context->display_link, nl_macos_display_link_callback, renderer);
  CVDisplayLinkStart(context->display_link);
}

void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  if (context->display_link != NULL) {
    CVDisplayLinkStop(context->display_link);
    CVDisplayLinkRelease(context->display_link);
    context->display_link = NULL;
  }
  nl_run_on_main_sync(^{
    if (context->layer != nil) {
      [context->layer flushAndRemoveImage];
    }
  });
  nl_macos_reset_layer_backpressure(context);
}

void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  if (context->display_link != NULL) {
    CVDisplayLinkStop(context->display_link);
    CVDisplayLinkRelease(context->display_link);
    context->display_link = NULL;
  }
  nl_video_renderer_platform_detach_surface(renderer);
  nl_macos_reset_format_description(context);
  nl_macos_free_parameter_set(&context->sps, &context->sps_len);
  nl_macos_free_parameter_set(&context->pps, &context->pps_len);
  nl_macos_free_parameter_set(&context->vps, &context->vps_len);
  free(context);
  renderer->platform_context = NULL;
}

int nl_video_renderer_platform_submit_frame(nl_video_renderer_t* renderer, const void* raw_decode_unit, const nl_video_frame_metadata_t* frame) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  const DECODE_UNIT* decode_unit = (const DECODE_UNIT*)raw_decode_unit;
  CMSampleBufferRef sample_buffer = NULL;
  __block bool request_idr = false;
  (void)frame;

  if (context == NULL || decode_unit == NULL || context->layer == nil) {
    return DR_OK;
  }

  if (context->layer.status == AVQueuedSampleBufferRenderingStatusFailed) {
    nl_run_on_main_sync(^{
      nl_macos_recover_display_layer(context);
    });
    return DR_NEED_IDR;
  }

  if (decode_unit->frameType == FRAME_TYPE_IDR) {
    nl_macos_collect_parameter_sets(context, decode_unit);
  }
  if (context->format_description == NULL) {
    return DR_NEED_IDR;
  }

  sample_buffer = nl_macos_create_sample_buffer(context, decode_unit);
  if (sample_buffer == NULL) {
    return DR_OK;
  }

  CFRetain(sample_buffer);
  nl_run_on_main_sync(^{
    if (context->layer == nil) {
      nl_macos_reset_layer_backpressure(context);
      CFRelease(sample_buffer);
      return;
    }
    if (context->layer.status == AVQueuedSampleBufferRenderingStatusFailed) {
      nl_macos_recover_display_layer(context);
      request_idr = true;
      CFRelease(sample_buffer);
      return;
    }
    if (!context->layer.readyForMoreMediaData && nl_macos_should_recover_for_backpressure(context)) {
      nl_macos_recover_display_layer(context);
      request_idr = true;
      CFRelease(sample_buffer);
      return;
    }
    [context->layer enqueueSampleBuffer:sample_buffer];
    if (context->layer.readyForMoreMediaData) {
      nl_macos_reset_layer_backpressure(context);
    }
    CFRelease(sample_buffer);
  });
  CFRelease(sample_buffer);
  return request_idr ? DR_NEED_IDR : DR_OK;
}
