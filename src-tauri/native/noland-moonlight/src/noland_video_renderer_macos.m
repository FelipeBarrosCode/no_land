#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <Carbon/Carbon.h>
#import <CoreMedia/CoreMedia.h>
#import <QuartzCore/QuartzCore.h>
#import <dispatch/dispatch.h>

#include "noland_video_renderer.h"
#include "Limelight.h"

#include <stdlib.h>
#include <string.h>
#include <stdint.h>

@class NolandInputCaptureView;

@interface NolandSampleDisplayLayer : AVSampleBufferDisplayLayer
@end

@implementation NolandSampleDisplayLayer
- (void)layoutSublayers {
  [super layoutSublayers];
}
@end

typedef struct nl_macos_video_context {
  nl_video_renderer_t* renderer;
  __unsafe_unretained NSView* view;
  __strong AVSampleBufferDisplayLayer* layer;
  __strong NolandInputCaptureView* input_view;
  CMVideoFormatDescriptionRef format_description;
  uint8_t* sps;
  size_t sps_len;
  uint8_t* pps;
  size_t pps_len;
  int video_format;
  int width;
  int height;
  int redraw_rate;
} nl_macos_video_context_t;

static int16_t nl_clamp_i16(double value) {
  if (value > 32767.0) {
    return 32767;
  }
  if (value < -32768.0) {
    return -32768;
  }
  return (int16_t)llround(value);
}

static uint8_t nl_macos_modifiers_from_flags(NSEventModifierFlags flags) {
  uint8_t modifiers = 0;
  NSEventModifierFlags deviceFlags = flags & NSEventModifierFlagDeviceIndependentFlagsMask;
  if ((deviceFlags & NSEventModifierFlagShift) != 0) {
    modifiers |= 0x01;
  }
  if ((deviceFlags & NSEventModifierFlagControl) != 0) {
    modifiers |= 0x02;
  }
  if ((deviceFlags & NSEventModifierFlagOption) != 0) {
    modifiers |= 0x04;
  }
  if ((deviceFlags & NSEventModifierFlagCommand) != 0) {
    modifiers |= 0x08;
  }
  return modifiers;
}

static uint16_t nl_macos_virtual_key_for_key_code(unsigned short keyCode) {
  switch (keyCode) {
    case kVK_ANSI_A: return 0x41;
    case kVK_ANSI_B: return 0x42;
    case kVK_ANSI_C: return 0x43;
    case kVK_ANSI_D: return 0x44;
    case kVK_ANSI_E: return 0x45;
    case kVK_ANSI_F: return 0x46;
    case kVK_ANSI_G: return 0x47;
    case kVK_ANSI_H: return 0x48;
    case kVK_ANSI_I: return 0x49;
    case kVK_ANSI_J: return 0x4A;
    case kVK_ANSI_K: return 0x4B;
    case kVK_ANSI_L: return 0x4C;
    case kVK_ANSI_M: return 0x4D;
    case kVK_ANSI_N: return 0x4E;
    case kVK_ANSI_O: return 0x4F;
    case kVK_ANSI_P: return 0x50;
    case kVK_ANSI_Q: return 0x51;
    case kVK_ANSI_R: return 0x52;
    case kVK_ANSI_S: return 0x53;
    case kVK_ANSI_T: return 0x54;
    case kVK_ANSI_U: return 0x55;
    case kVK_ANSI_V: return 0x56;
    case kVK_ANSI_W: return 0x57;
    case kVK_ANSI_X: return 0x58;
    case kVK_ANSI_Y: return 0x59;
    case kVK_ANSI_Z: return 0x5A;
    case kVK_ANSI_0: return 0x30;
    case kVK_ANSI_1: return 0x31;
    case kVK_ANSI_2: return 0x32;
    case kVK_ANSI_3: return 0x33;
    case kVK_ANSI_4: return 0x34;
    case kVK_ANSI_5: return 0x35;
    case kVK_ANSI_6: return 0x36;
    case kVK_ANSI_7: return 0x37;
    case kVK_ANSI_8: return 0x38;
    case kVK_ANSI_9: return 0x39;
    case kVK_Return: return 0x0D;
    case kVK_Tab: return 0x09;
    case kVK_Space: return 0x20;
    case kVK_Delete: return 0x08;
    case kVK_ForwardDelete: return 0x2E;
    case kVK_Escape: return 0x1B;
    case kVK_Home: return 0x24;
    case kVK_End: return 0x23;
    case kVK_PageUp: return 0x21;
    case kVK_PageDown: return 0x22;
    case kVK_LeftArrow: return 0x25;
    case kVK_RightArrow: return 0x27;
    case kVK_DownArrow: return 0x28;
    case kVK_UpArrow: return 0x26;
    case kVK_Shift: return 0x10;
    case kVK_RightShift: return 0x10;
    case kVK_Control: return 0x11;
    case kVK_RightControl: return 0x11;
    case kVK_Option: return 0x12;
    case kVK_RightOption: return 0x12;
    case kVK_Command: return 0x5B;
    case kVK_RightCommand: return 0x5C;
    case kVK_CapsLock: return 0x14;
    case kVK_F1: return 0x70;
    case kVK_F2: return 0x71;
    case kVK_F3: return 0x72;
    case kVK_F4: return 0x73;
    case kVK_F5: return 0x74;
    case kVK_F6: return 0x75;
    case kVK_F7: return 0x76;
    case kVK_F8: return 0x77;
    case kVK_F9: return 0x78;
    case kVK_F10: return 0x79;
    case kVK_F11: return 0x7A;
    case kVK_F12: return 0x7B;
    case kVK_ANSI_Minus: return 0xBD;
    case kVK_ANSI_Equal: return 0xBB;
    case kVK_ANSI_LeftBracket: return 0xDB;
    case kVK_ANSI_RightBracket: return 0xDD;
    case kVK_ANSI_Backslash: return 0xDC;
    case kVK_ANSI_Semicolon: return 0xBA;
    case kVK_ANSI_Quote: return 0xDE;
    case kVK_ANSI_Grave: return 0xC0;
    case kVK_ANSI_Comma: return 0xBC;
    case kVK_ANSI_Period: return 0xBE;
    case kVK_ANSI_Slash: return 0xBF;
    case kVK_ANSI_Keypad0: return 0x60;
    case kVK_ANSI_Keypad1: return 0x61;
    case kVK_ANSI_Keypad2: return 0x62;
    case kVK_ANSI_Keypad3: return 0x63;
    case kVK_ANSI_Keypad4: return 0x64;
    case kVK_ANSI_Keypad5: return 0x65;
    case kVK_ANSI_Keypad6: return 0x66;
    case kVK_ANSI_Keypad7: return 0x67;
    case kVK_ANSI_Keypad8: return 0x68;
    case kVK_ANSI_Keypad9: return 0x69;
    case kVK_ANSI_KeypadPlus: return 0x6B;
    case kVK_ANSI_KeypadMinus: return 0x6D;
    case kVK_ANSI_KeypadMultiply: return 0x6A;
    case kVK_ANSI_KeypadDivide: return 0x6F;
    case kVK_ANSI_KeypadDecimal: return 0x6E;
    case kVK_ANSI_KeypadEnter: return 0x0D;
    default: return 0;
  }
}

static uint8_t nl_macos_modifier_mask_for_key_code(unsigned short keyCode) {
  switch (keyCode) {
    case kVK_Shift:
    case kVK_RightShift:
      return 0x01;
    case kVK_Control:
    case kVK_RightControl:
      return 0x02;
    case kVK_Option:
    case kVK_RightOption:
      return 0x04;
    case kVK_Command:
    case kVK_RightCommand:
      return 0x08;
    default:
      return 0;
  }
}

static nl_runtime_t* nl_macos_runtime(nl_macos_video_context_t* context) {
  if (context == NULL || context->renderer == NULL) {
    return NULL;
  }
  return (nl_runtime_t*)context->renderer->owner_runtime;
}

static void nl_macos_send_relative_mouse(nl_macos_video_context_t* context, CGFloat deltaX, CGFloat deltaY) {
  nl_runtime_t* runtime = nl_macos_runtime(context);
  if (runtime == NULL) {
    return;
  }
  nl_send_relative_mouse(runtime, nl_clamp_i16(deltaX), nl_clamp_i16(deltaY));
}

static void nl_macos_send_mouse_button(nl_macos_video_context_t* context, uint8_t button, BOOL pressed) {
  nl_runtime_t* runtime = nl_macos_runtime(context);
  if (runtime == NULL) {
    return;
  }
  nl_send_mouse_button(runtime, button, pressed ? true : false);
}

static void nl_macos_send_keyboard(nl_macos_video_context_t* context, uint16_t virtualKey, BOOL pressed, uint8_t modifiers) {
  nl_runtime_t* runtime = nl_macos_runtime(context);
  if (runtime == NULL || virtualKey == 0) {
    return;
  }
  nl_send_keyboard(runtime, virtualKey, pressed ? true : false, modifiers);
}

@interface NolandInputCaptureView : NSView
@property(nonatomic, assign) nl_macos_video_context_t* nlContext;
@property(nonatomic, strong) NSMutableSet<NSNumber*>* pressedKeys;
@property(nonatomic, strong) NSMutableSet<NSNumber*>* pressedMouseButtons;
@property(nonatomic, assign) BOOL captureActive;
@property(nonatomic, assign) BOOL cursorHidden;
@property(nonatomic, strong) id windowResignObserver;
- (void)releaseAllInputs;
- (void)releaseCapture;
@end

@implementation NolandInputCaptureView

- (instancetype)initWithFrame:(NSRect)frameRect {
  self = [super initWithFrame:frameRect];
  if (self != nil) {
    _pressedKeys = [NSMutableSet set];
    _pressedMouseButtons = [NSMutableSet set];
    self.wantsLayer = YES;
    self.layer.backgroundColor = NSColor.clearColor.CGColor;
  }
  return self;
}

- (BOOL)isOpaque {
  return NO;
}

- (BOOL)acceptsFirstResponder {
  return YES;
}

- (BOOL)acceptsFirstMouse:(NSEvent*)event {
  (void)event;
  return YES;
}

- (void)updateTrackingAreas {
  [super updateTrackingAreas];
  for (NSTrackingArea* area in [self trackingAreas]) {
    [self removeTrackingArea:area];
  }
  NSTrackingAreaOptions options = NSTrackingActiveAlways | NSTrackingInVisibleRect | NSTrackingMouseMoved | NSTrackingEnabledDuringMouseDrag;
  NSTrackingArea* trackingArea = [[NSTrackingArea alloc] initWithRect:NSZeroRect options:options owner:self userInfo:nil];
  [self addTrackingArea:trackingArea];
}

- (void)viewDidMoveToWindow {
  [super viewDidMoveToWindow];
  if (self.windowResignObserver != nil) {
    [NSNotificationCenter.defaultCenter removeObserver:self.windowResignObserver];
    self.windowResignObserver = nil;
  }
  if (self.window != nil) {
    __weak typeof(self) weakSelf = self;
    self.window.acceptsMouseMovedEvents = YES;
    self.windowResignObserver = [NSNotificationCenter.defaultCenter addObserverForName:NSWindowDidResignKeyNotification object:self.window queue:nil usingBlock:^(NSNotification* note) {
      (void)note;
      [weakSelf releaseAllInputs];
    }];
  }
}

- (void)dealloc {
  if (self.windowResignObserver != nil) {
    [NSNotificationCenter.defaultCenter removeObserver:self.windowResignObserver];
  }
}

- (void)activateCapture {
  self.captureActive = YES;
  [self.window makeFirstResponder:self];
  if (!self.cursorHidden) {
    [NSCursor hide];
    self.cursorHidden = YES;
  }
}

- (void)releaseAllInputs {
  nl_macos_video_context_t* context = self.nlContext;
  uint8_t modifiers = 0;
  for (NSNumber* keyNumber in [self.pressedKeys allObjects]) {
    nl_macos_send_keyboard(context, keyNumber.unsignedShortValue, NO, modifiers);
  }
  [self.pressedKeys removeAllObjects];
  for (NSNumber* buttonNumber in [self.pressedMouseButtons allObjects]) {
    nl_macos_send_mouse_button(context, buttonNumber.unsignedCharValue, NO);
  }
  [self.pressedMouseButtons removeAllObjects];
}

- (void)releaseCapture {
  [self releaseAllInputs];
  self.captureActive = NO;
  if (self.cursorHidden) {
    [NSCursor unhide];
    self.cursorHidden = NO;
  }
}



- (void)handleMouseButton:(NSEvent*)event button:(uint8_t)button pressed:(BOOL)pressed {
  if (!self.captureActive) {
    [self activateCapture];
  }
  NSNumber* buttonNumber = @(button);
  if (pressed) {
    [self.pressedMouseButtons addObject:buttonNumber];
  } else {
    [self.pressedMouseButtons removeObject:buttonNumber];
  }
  nl_macos_send_mouse_button(self.nlContext, button, pressed);
}

- (void)mouseDown:(NSEvent*)event {
  [self handleMouseButton:event button:0x01 pressed:YES];
}

- (void)mouseUp:(NSEvent*)event {
  [self handleMouseButton:event button:0x01 pressed:NO];
}

- (void)rightMouseDown:(NSEvent*)event {
  [self handleMouseButton:event button:0x03 pressed:YES];
}

- (void)rightMouseUp:(NSEvent*)event {
  [self handleMouseButton:event button:0x03 pressed:NO];
}

- (void)otherMouseDown:(NSEvent*)event {
  uint8_t button = event.buttonNumber == 2 ? 0x02 : (event.buttonNumber == 3 ? 0x04 : 0x05);
  [self handleMouseButton:event button:button pressed:YES];
}

- (void)otherMouseUp:(NSEvent*)event {
  uint8_t button = event.buttonNumber == 2 ? 0x02 : (event.buttonNumber == 3 ? 0x04 : 0x05);
  [self handleMouseButton:event button:button pressed:NO];
}

- (void)sendMouseMove:(NSEvent*)event {
  if (!self.captureActive) {
    return;
  }
  if (event.deltaX != 0.0 || event.deltaY != 0.0) {
    nl_macos_send_relative_mouse(self.nlContext, event.deltaX, -event.deltaY);
  }
}

- (void)mouseMoved:(NSEvent*)event {
  [self sendMouseMove:event];
}

- (void)mouseDragged:(NSEvent*)event {
  [self sendMouseMove:event];
}

- (void)rightMouseDragged:(NSEvent*)event {
  [self sendMouseMove:event];
}

- (void)otherMouseDragged:(NSEvent*)event {
  [self sendMouseMove:event];
}

- (void)flagsChanged:(NSEvent*)event {
  uint16_t virtualKey = nl_macos_virtual_key_for_key_code(event.keyCode);
  uint8_t modifierMask = nl_macos_modifier_mask_for_key_code(event.keyCode);
  uint8_t modifiers = nl_macos_modifiers_from_flags(event.modifierFlags);
  BOOL pressed = modifierMask != 0 && (modifiers & modifierMask) != 0;
  NSNumber* keyNumber = @(virtualKey);
  if (virtualKey == 0 || modifierMask == 0) {
    return;
  }
  if (pressed) {
    if (![self.pressedKeys containsObject:keyNumber]) {
      [self.pressedKeys addObject:keyNumber];
      nl_macos_send_keyboard(self.nlContext, virtualKey, YES, modifiers);
    }
  } else if ([self.pressedKeys containsObject:keyNumber]) {
    [self.pressedKeys removeObject:keyNumber];
    nl_macos_send_keyboard(self.nlContext, virtualKey, NO, modifiers);
  }
}

- (void)keyDown:(NSEvent*)event {
  uint16_t virtualKey = nl_macos_virtual_key_for_key_code(event.keyCode);
  uint8_t modifiers = nl_macos_modifiers_from_flags(event.modifierFlags);
  NSNumber* keyNumber = @(virtualKey);
  BOOL releaseShortcut = (modifiers & 0x07) == 0x07 && (event.keyCode == kVK_ANSI_Z || event.keyCode == kVK_ANSI_Q);
  if (releaseShortcut) {
    [self releaseCapture];
    return;
  }
  if (virtualKey == 0 || event.isARepeat) {
    return;
  }
  if (!self.captureActive) {
    [self activateCapture];
  }
  if (![self.pressedKeys containsObject:keyNumber]) {
    [self.pressedKeys addObject:keyNumber];
    nl_macos_send_keyboard(self.nlContext, virtualKey, YES, modifiers);
  }
}

- (void)keyUp:(NSEvent*)event {
  uint16_t virtualKey = nl_macos_virtual_key_for_key_code(event.keyCode);
  uint8_t modifiers = nl_macos_modifiers_from_flags(event.modifierFlags);
  NSNumber* keyNumber = @(virtualKey);
  if (virtualKey == 0) {
    return;
  }
  [self.pressedKeys removeObject:keyNumber];
  nl_macos_send_keyboard(self.nlContext, virtualKey, NO, modifiers);
}

- (void)cancelOperation:(id)sender {
  (void)sender;
  [self releaseCapture];
}

- (BOOL)resignFirstResponder {
  [self releaseCapture];
  return [super resignFirstResponder];
}

@end

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
    context->renderer = renderer;
    return context;
  }
  context = calloc(1, sizeof(*context));
  if (context == NULL) {
    return NULL;
  }
  context->renderer = renderer;
  renderer->platform_context = context;
  return context;
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
  const uint8_t* parameter_sets[2];
  size_t parameter_set_sizes[2];
  CMVideoFormatDescriptionRef format_description = NULL;
  OSStatus status;

  if (context == NULL || context->video_format != VIDEO_FORMAT_H264) {
    return false;
  }
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
      context->layer.needsDisplayOnBoundsChange = YES;
      context->layer.frame = view.bounds;
      context->layer.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
      [view.layer addSublayer:context->layer];
    } else if (context->layer.superlayer != view.layer) {
      context->layer.frame = view.bounds;
      [view.layer addSublayer:context->layer];
    }
    if (context->input_view == nil) {
      context->input_view = [[NolandInputCaptureView alloc] initWithFrame:view.bounds];
      context->input_view.nlContext = context;
      context->input_view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
      context->input_view.hidden = NO;
      [view addSubview:context->input_view positioned:NSWindowAbove relativeTo:nil];
    } else if (context->input_view.superview != view) {
      context->input_view.frame = view.bounds;
      context->input_view.nlContext = context;
      [view addSubview:context->input_view positioned:NSWindowAbove relativeTo:nil];
    }
    [view.window makeFirstResponder:context->input_view];
  });
}

void nl_video_renderer_platform_detach_surface(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  nl_run_on_main_sync(^{
    if (context->input_view != nil) {
      [context->input_view releaseCapture];
      [context->input_view removeFromSuperview];
      context->input_view = nil;
    }
    if (context->layer != nil) {
      [context->layer flushAndRemoveImage];
      [context->layer removeFromSuperlayer];
    }
  });
  context->view = nil;
}

int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer, int video_format, int width, int height, int redraw_rate) {
  nl_macos_video_context_t* context = nl_macos_ensure_context(renderer);
  if (context == NULL) {
    return -1;
  }
  context->video_format = video_format;
  context->width = width;
  context->height = height;
  context->redraw_rate = redraw_rate;
  nl_macos_reset_format_description(context);
  return 0;
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
}

void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  nl_run_on_main_sync(^{
    if (context->layer != nil) {
      [context->layer flushAndRemoveImage];
    }
  });
}

void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  if (context == NULL) {
    return;
  }
  nl_video_renderer_platform_detach_surface(renderer);
  nl_macos_reset_format_description(context);
  nl_macos_free_parameter_set(&context->sps, &context->sps_len);
  nl_macos_free_parameter_set(&context->pps, &context->pps_len);
  free(context);
  renderer->platform_context = NULL;
}

int nl_video_renderer_platform_submit_frame(nl_video_renderer_t* renderer, const void* raw_decode_unit, const nl_video_frame_metadata_t* frame) {
  nl_macos_video_context_t* context = nl_macos_context(renderer);
  const DECODE_UNIT* decode_unit = (const DECODE_UNIT*)raw_decode_unit;
  CMSampleBufferRef sample_buffer = NULL;
  (void)frame;

  if (context == NULL || decode_unit == NULL || context->layer == nil) {
    return DR_OK;
  }
  if (context->video_format != VIDEO_FORMAT_H264) {
    return DR_OK;
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
      CFRelease(sample_buffer);
      return;
    }
    if (context->layer.status == AVQueuedSampleBufferRenderingStatusFailed) {
      [context->layer flush];
    }
    [context->layer enqueueSampleBuffer:sample_buffer];
    CFRelease(sample_buffer);
  });
  CFRelease(sample_buffer);
  return DR_OK;
}
