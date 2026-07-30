#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <QuartzCore/QuartzCore.h>
#import <objc/runtime.h>

extern void noland_macos_input_on_relative_mouse(double delta_x, double delta_y);
extern void noland_macos_input_on_absolute_mouse(double x, double y, double content_width, double content_height);
extern void noland_macos_input_on_mouse_button(unsigned char button, bool pressed);
extern void noland_macos_input_on_keyboard(unsigned short virtual_key, bool pressed, unsigned char modifiers);
extern void noland_macos_input_on_vertical_scroll(double amount, bool high_resolution);
extern void noland_macos_input_on_horizontal_scroll(double amount, bool high_resolution);
extern void noland_macos_input_on_focus_changed(bool focused);
extern void noland_macos_input_on_capture_changed(bool active, int mode);
extern int noland_macos_input_request_capture(void);
extern void noland_macos_input_debug_native_event(int kind);
extern bool noland_macos_input_debug_capture_active(void);
extern int noland_macos_input_debug_capture_mode(void);
extern unsigned long long noland_macos_input_debug_capture_requests(void);
extern unsigned long long noland_macos_input_debug_native_mouse_moves(void);
extern unsigned long long noland_macos_input_debug_native_mouse_downs(void);
extern unsigned long long noland_macos_input_debug_native_mouse_ups(void);
extern unsigned long long noland_macos_input_debug_native_keys(void);
extern unsigned long long noland_macos_input_debug_rust_relative_callbacks(void);
extern unsigned long long noland_macos_input_debug_rust_absolute_callbacks(void);
extern unsigned long long noland_macos_input_debug_rust_button_callbacks(void);
extern unsigned long long noland_macos_input_debug_rust_key_callbacks(void);
extern unsigned long long noland_input_debug_relative_send_attempts(void);
extern unsigned long long noland_input_debug_absolute_send_attempts(void);
extern unsigned long long noland_input_debug_button_send_attempts(void);
extern unsigned long long noland_input_debug_key_send_attempts(void);
extern unsigned long long noland_input_debug_scroll_send_attempts(void);
extern unsigned long long noland_input_debug_send_errors(void);

static const NSInteger kNolandCaptureModeNone = 0;
static const NSInteger kNolandCaptureModeRelative = 1;
static const NSInteger kNolandCaptureModeAbsolute = 2;
static const unsigned char kNolandModifierShift = 0x01;
static const unsigned char kNolandModifierCtrl = 0x02;
static const unsigned char kNolandModifierAlt = 0x04;
static const unsigned char kNolandModifierMeta = 0x08;

@class NolandMacosStreamInputBridge;

@interface NolandMacosStreamContainerView : NSView
@end

@interface NolandMacosCaptureView : NSView
@property (nonatomic, weak) NolandMacosStreamInputBridge *bridge;
@property (nonatomic, strong) NSTrackingArea *trackingArea;
@end

@interface NolandMacosStreamInputBridge : NSObject
@property (nonatomic, assign) NSView *view;
@property (nonatomic, strong) NolandMacosCaptureView *captureView;
@property (nonatomic, weak) NSWindow *window;
@property (nonatomic, assign) BOOL captureActive;
@property (nonatomic, assign) NSInteger captureMode;
@property (nonatomic, assign) BOOL cursorHidden;
@property (nonatomic, assign) BOOL suppressNextLeftMouseUp;
@property (nonatomic, strong) CATextLayer *debugTextLayer;
@property (nonatomic, strong) NSView *debugBadgeView;
@property (nonatomic, strong) NSTextField *debugBadgeLabel;
@property (nonatomic, strong) NSTimer *debugTimer;
@property (nonatomic, strong) id didBecomeKeyObserver;
@property (nonatomic, strong) id didResignKeyObserver;
@property (nonatomic, strong) id localEventMonitor;
@end

@implementation NolandMacosStreamContainerView

- (instancetype)initWithFrame:(NSRect)frameRect {
    self = [super initWithFrame:frameRect];
    if (self != nil) {
        self.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        self.wantsLayer = YES;
        self.layer = [CALayer layer];
        self.layer.backgroundColor = NSColor.blackColor.CGColor;
    }
    return self;
}

- (BOOL)isOpaque {
    return YES;
}

@end

@implementation NolandMacosStreamInputBridge
@end

static const void *kNolandMacosStreamInputBridgeKey = &kNolandMacosStreamInputBridgeKey;
static const void *kNolandMacosStreamContainerViewKey = &kNolandMacosStreamContainerViewKey;
static BOOL kNolandDebugOverlayEnabled = NO;
static NSHashTable<NolandMacosStreamInputBridge *> *kNolandStreamInputBridges = nil;

static NSHashTable<NolandMacosStreamInputBridge *> *noland_stream_input_bridges(void) {
    if (kNolandStreamInputBridges == nil) {
        kNolandStreamInputBridges = [NSHashTable weakObjectsHashTable];
    }
    return kNolandStreamInputBridges;
}

static void noland_update_debug_overlay(NolandMacosStreamInputBridge *bridge);

static void noland_update_debug_overlay_visibility(NolandMacosStreamInputBridge *bridge) {
    if (bridge == nil) {
        return;
    }

    BOOL hidden = !kNolandDebugOverlayEnabled;
    if (bridge.debugBadgeView != nil) {
        bridge.debugBadgeView.hidden = hidden;
    }
    if (bridge.debugTextLayer != nil) {
        bridge.debugTextLayer.hidden = hidden;
    }

    if (hidden) {
        if (bridge.debugTimer != nil) {
            [bridge.debugTimer invalidate];
            bridge.debugTimer = nil;
        }
        return;
    }

    if (bridge.debugTimer == nil || ![bridge.debugTimer isValid]) {
        bridge.debugTimer = [NSTimer scheduledTimerWithTimeInterval:0.25
                                                             repeats:YES
                                                               block:^(__unused NSTimer *timer) {
            noland_update_debug_overlay(bridge);
        }];
    }

    noland_update_debug_overlay(bridge);
}

void noland_macos_input_set_debug_overlay_enabled(bool enabled) {
    kNolandDebugOverlayEnabled = enabled ? YES : NO;
    for (NolandMacosStreamInputBridge *bridge in noland_stream_input_bridges()) {
        noland_update_debug_overlay_visibility(bridge);
    }
}

int noland_macos_detect_main_display(unsigned int *width,
                                     unsigned int *height,
                                     unsigned int *refresh_hz) {
    NSScreen *screen = [NSScreen mainScreen];
    if (screen == nil) {
        NSArray<NSScreen *> *screens = [NSScreen screens];
        if (screens.count > 0) {
            screen = screens.firstObject;
        }
    }
    if (screen == nil) {
        return 0;
    }

    NSNumber *screenNumber = screen.deviceDescription[@"NSScreenNumber"];
    if (screenNumber == nil) {
        return 0;
    }

    CGDirectDisplayID displayID = (CGDirectDisplayID)screenNumber.unsignedIntValue;
    CGDisplayModeRef mode = CGDisplayCopyDisplayMode(displayID);
    if (mode == NULL) {
        return 0;
    }

    size_t detectedWidth = CGDisplayModeGetPixelWidth(mode);
    size_t detectedHeight = CGDisplayModeGetPixelHeight(mode);
    double refresh = CGDisplayModeGetRefreshRate(mode);
    CGDisplayModeRelease(mode);

    if (detectedWidth == 0 || detectedHeight == 0) {
        return 0;
    }

    if (width != NULL) {
        *width = (unsigned int)detectedWidth;
    }
    if (height != NULL) {
        *height = (unsigned int)detectedHeight;
    }
    if (refresh_hz != NULL) {
        *refresh_hz = (unsigned int)((refresh > 1.0) ? llround(refresh) : 60.0);
    }

    return 1;
}

static unsigned char noland_modifier_bits(NSEventModifierFlags flags) {
    unsigned char modifiers = 0;
    if ((flags & NSEventModifierFlagShift) != 0) {
        modifiers |= kNolandModifierShift;
    }
    if ((flags & NSEventModifierFlagControl) != 0) {
        modifiers |= kNolandModifierCtrl;
    }
    if ((flags & NSEventModifierFlagOption) != 0) {
        modifiers |= kNolandModifierAlt;
    }
    if ((flags & NSEventModifierFlagCommand) != 0) {
        modifiers |= kNolandModifierMeta;
    }
    return modifiers;
}

static unsigned char noland_map_mouse_button(NSEvent *event) {
    switch (event.type) {
        case NSEventTypeLeftMouseDown:
        case NSEventTypeLeftMouseUp:
            return 0x01;
        case NSEventTypeOtherMouseDown:
        case NSEventTypeOtherMouseUp:
            switch (event.buttonNumber) {
                case 2:
                    return 0x02;
                case 3:
                    return 0x04;
                case 4:
                    return 0x05;
                default:
                    return 0x02;
            }
        case NSEventTypeRightMouseDown:
        case NSEventTypeRightMouseUp:
        default:
            return 0x03;
    }
}

static unsigned short noland_vk_for_key_code(unsigned short keyCode) {
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
        case kVK_Escape: return 0x1B;
        case kVK_Command: return 0x5B;
        case kVK_RightCommand: return 0x5C;
        case kVK_Shift: return 0x10;
        case kVK_RightShift: return 0x10;
        case kVK_CapsLock: return 0x14;
        case kVK_Option: return 0x12;
        case kVK_RightOption: return 0x12;
        case kVK_Control: return 0x11;
        case kVK_RightControl: return 0x11;
        case kVK_Function: return 0x00;
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
        case kVK_Help: return 0x2D;
        case kVK_Home: return 0x24;
        case kVK_PageUp: return 0x21;
        case kVK_ForwardDelete: return 0x2E;
        case kVK_End: return 0x23;
        case kVK_PageDown: return 0x22;
        case kVK_LeftArrow: return 0x25;
        case kVK_RightArrow: return 0x27;
        case kVK_DownArrow: return 0x28;
        case kVK_UpArrow: return 0x26;
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
        case kVK_ANSI_KeypadDecimal: return 0x6E;
        case kVK_ANSI_KeypadMultiply: return 0x6A;
        case kVK_ANSI_KeypadPlus: return 0x6B;
        case kVK_ANSI_KeypadClear: return 0x90;
        case kVK_ANSI_KeypadDivide: return 0x6F;
        case kVK_ANSI_KeypadEnter: return 0x0D;
        case kVK_ANSI_KeypadMinus: return 0x6D;
        case kVK_ANSI_KeypadEquals: return 0xBB;
        default: return 0x00;
    }
}

static BOOL noland_is_release_shortcut(NSEvent *event) {
    if (event == nil || event.type != NSEventTypeKeyDown) {
        return NO;
    }

    NSEventModifierFlags required = NSEventModifierFlagControl | NSEventModifierFlagOption | NSEventModifierFlagShift;
    NSEventModifierFlags current = event.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask;
    if ((current & required) != required) {
        return NO;
    }

    return event.keyCode == kVK_ANSI_Z || event.keyCode == kVK_ANSI_Q;
}

static BOOL noland_modifier_pressed_for_event(NSEvent *event) {
    if (event == nil) {
        return NO;
    }

    switch (event.keyCode) {
        case kVK_Shift:
        case kVK_RightShift:
            return (event.modifierFlags & NSEventModifierFlagShift) != 0;
        case kVK_Control:
        case kVK_RightControl:
            return (event.modifierFlags & NSEventModifierFlagControl) != 0;
        case kVK_Option:
        case kVK_RightOption:
            return (event.modifierFlags & NSEventModifierFlagOption) != 0;
        case kVK_Command:
        case kVK_RightCommand:
            return (event.modifierFlags & NSEventModifierFlagCommand) != 0;
        case kVK_CapsLock:
            return (event.modifierFlags & NSEventModifierFlagCapsLock) != 0;
        default:
            return NO;
    }
}

static void noland_emit_absolute_mouse(NolandMacosStreamInputBridge *bridge, NSEvent *event) {
    if (bridge == nil || event == nil || bridge.view == nil) {
        return;
    }

    NSPoint point = [bridge.view convertPoint:event.locationInWindow fromView:nil];
    NSRect bounds = bridge.view.bounds;
    CGFloat width = NSWidth(bounds);
    CGFloat height = NSHeight(bounds);
    if (width <= 0.0 || height <= 0.0) {
        return;
    }

    CGFloat x = point.x;
    CGFloat y = height - point.y;
    if (x < 0.0) {
        x = 0.0;
    } else if (x > width) {
        x = width;
    }
    if (y < 0.0) {
        y = 0.0;
    } else if (y > height) {
        y = height;
    }

    noland_macos_input_on_absolute_mouse((double)x, (double)y, (double)width, (double)height);
}

static void noland_handle_mouse_motion(NolandMacosStreamInputBridge *bridge, NSEvent *event) {
    if (bridge == nil || event == nil || !bridge.captureActive) {
        return;
    }

    if (bridge.captureMode == kNolandCaptureModeRelative) {
        noland_macos_input_on_relative_mouse(event.deltaX, event.deltaY);
    } else if (bridge.captureMode == kNolandCaptureModeAbsolute) {
        noland_emit_absolute_mouse(bridge, event);
    }
}

static void noland_set_capture_state(NolandMacosStreamInputBridge *bridge, BOOL active, NSInteger mode);
static BOOL noland_bridge_contains_window_point(NolandMacosStreamInputBridge *bridge, NSPoint windowPoint);
static BOOL noland_route_local_event(NolandMacosStreamInputBridge *bridge, NSEvent *event);

static void noland_handle_mouse_button(NolandMacosStreamInputBridge *bridge, NSEvent *event, bool pressed) {
    if (bridge == nil || event == nil || !bridge.captureActive) {
        return;
    }

    if (bridge.captureMode == kNolandCaptureModeAbsolute) {
        noland_emit_absolute_mouse(bridge, event);
    }

    noland_macos_input_on_mouse_button(noland_map_mouse_button(event), pressed);
}

static void noland_handle_scroll(NolandMacosStreamInputBridge *bridge, NSEvent *event) {
    if (bridge == nil || event == nil || !bridge.captureActive) {
        return;
    }

    BOOL highResolution = event.hasPreciseScrollingDeltas;
    CGFloat deltaY = highResolution ? event.scrollingDeltaY : event.deltaY;
    CGFloat deltaX = highResolution ? event.scrollingDeltaX : event.deltaX;

    if (highResolution) {
        deltaY = MAX(-1.0, MIN(1.0, deltaY));
        deltaX = MAX(-1.0, MIN(1.0, deltaX));
        deltaY *= 120.0;
        deltaX *= 120.0;
    }

    if (deltaY != 0.0) {
        noland_macos_input_on_vertical_scroll((double)deltaY, highResolution);
    }
    if (deltaX != 0.0) {
        noland_macos_input_on_horizontal_scroll((double)deltaX, highResolution);
    }
}

static void noland_handle_key(NolandMacosStreamInputBridge *bridge, NSEvent *event, bool pressed) {
    if (bridge == nil || event == nil || !bridge.captureActive) {
        return;
    }

    if (pressed && noland_is_release_shortcut(event)) {
        noland_set_capture_state(bridge, NO, kNolandCaptureModeNone);
        return;
    }

    unsigned short virtualKey = noland_vk_for_key_code(event.keyCode);
    if (virtualKey != 0x00) {
        noland_macos_input_on_keyboard(virtualKey, pressed, noland_modifier_bits(event.modifierFlags));
    }
}

static void noland_handle_flags_changed(NolandMacosStreamInputBridge *bridge, NSEvent *event) {
    if (bridge == nil || event == nil || !bridge.captureActive) {
        return;
    }

    unsigned short virtualKey = noland_vk_for_key_code(event.keyCode);
    if (virtualKey != 0x00) {
        noland_macos_input_on_keyboard(virtualKey, noland_modifier_pressed_for_event(event), noland_modifier_bits(event.modifierFlags));
    }
}

@implementation NolandMacosCaptureView



- (instancetype)initWithFrame:(NSRect)frameRect {
    self = [super initWithFrame:frameRect];
    if (self != nil) {
        self.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
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

- (BOOL)becomeFirstResponder {
    return YES;
}

- (BOOL)resignFirstResponder {
    return YES;
}

- (BOOL)acceptsFirstMouse:(NSEvent *)event {
    (void)event;
    return YES;
}

- (BOOL)canBecomeKeyView {
    return YES;
}

- (NSView *)hitTest:(NSPoint)point {
    if (self.bridge == nil || self.hidden || self.alphaValue <= 0.0) {
        return nil;
    }
    return NSPointInRect(point, self.bounds) ? self : nil;
}

- (void)updateTrackingAreas {
    [super updateTrackingAreas];

    if (self.trackingArea != nil) {
        [self removeTrackingArea:self.trackingArea];
        self.trackingArea = nil;
    }

    NSTrackingAreaOptions options = NSTrackingMouseMoved |
        NSTrackingMouseEnteredAndExited |
        NSTrackingActiveAlways |
        NSTrackingInVisibleRect |
        NSTrackingEnabledDuringMouseDrag;
    self.trackingArea = [[NSTrackingArea alloc] initWithRect:NSZeroRect options:options owner:self userInfo:nil];
    [self addTrackingArea:self.trackingArea];
}

- (void)mouseMoved:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)mouseDragged:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)rightMouseDragged:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)otherMouseDragged:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)leftMouseDown:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)leftMouseUp:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)rightMouseDown:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)rightMouseUp:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)otherMouseDown:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)otherMouseUp:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)scrollWheel:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)keyDown:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)keyUp:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

- (void)flagsChanged:(NSEvent *)event {
    (void)noland_route_local_event(self.bridge, event);
}

@end

static BOOL noland_bridge_contains_window_point(NolandMacosStreamInputBridge *bridge, NSPoint windowPoint) {
    if (bridge == nil || bridge.captureView == nil || bridge.window == nil) {
        return NO;
    }

    NSPoint localPoint = [bridge.captureView convertPoint:windowPoint fromView:nil];
    return NSPointInRect(localPoint, bridge.captureView.bounds);
}

static BOOL noland_route_local_event(NolandMacosStreamInputBridge *bridge, NSEvent *event) {
    if (bridge == nil || event == nil || bridge.captureView == nil) {
        return NO;
    }

    switch (event.type) {
        case NSEventTypeMouseMoved:
        case NSEventTypeLeftMouseDragged:
        case NSEventTypeRightMouseDragged:
        case NSEventTypeOtherMouseDragged:
            if (!bridge.captureActive && !noland_bridge_contains_window_point(bridge, event.locationInWindow)) {
                return NO;
            }
            noland_macos_input_debug_native_event(1);
            noland_handle_mouse_motion(bridge, event);
            NSLog(@"[noland-stream-input] mouse motion active=%d mode=%ld point=(%.1f, %.1f) dx=%.2f dy=%.2f",
                  bridge.captureActive,
                  (long)bridge.captureMode,
                  event.locationInWindow.x,
                  event.locationInWindow.y,
                  event.deltaX,
                  event.deltaY);
            return YES;
        case NSEventTypeLeftMouseDown:
            if (!noland_bridge_contains_window_point(bridge, event.locationInWindow)) {
                return NO;
            }
            noland_macos_input_debug_native_event(2);
            [bridge.window makeFirstResponder:bridge.captureView];
            NSLog(@"[noland-stream-input] left down active=%d mode=%ld point=(%.1f, %.1f)",
                  bridge.captureActive,
                  (long)bridge.captureMode,
                  event.locationInWindow.x,
                  event.locationInWindow.y);
            if (!bridge.captureActive) {
                int mode = noland_macos_input_request_capture();
                NSLog(@"[noland-stream-input] request capture -> mode=%d", mode);
                if (mode != 0) {
                    bridge.suppressNextLeftMouseUp = YES;
                    noland_set_capture_state(bridge, YES, mode);
                    if (mode == kNolandCaptureModeAbsolute) {
                        noland_emit_absolute_mouse(bridge, event);
                    }
                }
                return YES;
            }
            noland_handle_mouse_button(bridge, event, true);
            return YES;
        case NSEventTypeLeftMouseUp:
            if (!bridge.captureActive && !bridge.suppressNextLeftMouseUp) {
                return NO;
            }
            noland_macos_input_debug_native_event(3);
            if (bridge.suppressNextLeftMouseUp) {
                bridge.suppressNextLeftMouseUp = NO;
                NSLog(@"[noland-stream-input] suppress left up after capture activation");
                return YES;
            }
            NSLog(@"[noland-stream-input] left up active=%d", bridge.captureActive);
            if (!bridge.captureActive) {
                return YES;
            }
            noland_handle_mouse_button(bridge, event, false);
            return YES;
        case NSEventTypeRightMouseDown:
        case NSEventTypeOtherMouseDown:
            if (!bridge.captureActive && !noland_bridge_contains_window_point(bridge, event.locationInWindow)) {
                return NO;
            }
            noland_macos_input_debug_native_event(2);
            [bridge.window makeFirstResponder:bridge.captureView];
            NSLog(@"[noland-stream-input] mouse down type=%ld active=%d button=%ld",
                  (long)event.type,
                  bridge.captureActive,
                  (long)event.buttonNumber);
            if (!bridge.captureActive) {
                return YES;
            }
            noland_handle_mouse_button(bridge, event, true);
            return YES;
        case NSEventTypeRightMouseUp:
        case NSEventTypeOtherMouseUp:
            if (!bridge.captureActive) {
                return NO;
            }
            noland_macos_input_debug_native_event(3);
            NSLog(@"[noland-stream-input] mouse up type=%ld button=%ld", (long)event.type, (long)event.buttonNumber);
            noland_handle_mouse_button(bridge, event, false);
            return YES;
        case NSEventTypeScrollWheel:
            if (!bridge.captureActive && !noland_bridge_contains_window_point(bridge, event.locationInWindow)) {
                return NO;
            }
            noland_handle_scroll(bridge, event);
            return bridge.captureActive;
        case NSEventTypeKeyDown:
            if (!bridge.captureActive) {
                return NO;
            }
            noland_macos_input_debug_native_event(4);
            NSLog(@"[noland-stream-input] key down code=%hu mods=%llu", event.keyCode, (unsigned long long)event.modifierFlags);
            noland_handle_key(bridge, event, true);
            return YES;
        case NSEventTypeKeyUp:
            if (!bridge.captureActive) {
                return NO;
            }
            noland_macos_input_debug_native_event(4);
            NSLog(@"[noland-stream-input] key up code=%hu mods=%llu", event.keyCode, (unsigned long long)event.modifierFlags);
            noland_handle_key(bridge, event, false);
            return YES;
        case NSEventTypeFlagsChanged:
            if (!bridge.captureActive) {
                return NO;
            }
            noland_handle_flags_changed(bridge, event);
            return YES;
        default:
            return NO;
    }
}

static void noland_set_capture_state(NolandMacosStreamInputBridge *bridge, BOOL active, NSInteger mode) {
    if (bridge == nil || bridge.captureView == nil) {
        return;
    }

    NSInteger targetMode = active ? mode : kNolandCaptureModeNone;
    if (bridge.captureActive == active && bridge.captureMode == targetMode) {
        return;
    }

    BOOL wasRelative = bridge.captureMode == kNolandCaptureModeRelative;
    BOOL shouldBeRelative = active && mode == kNolandCaptureModeRelative;

    bridge.captureActive = active;
    bridge.captureMode = targetMode;
    bridge.captureView.hidden = NO;

    noland_macos_input_on_capture_changed(active, (int)targetMode);

    if (active) {
        [bridge.window makeKeyAndOrderFront:nil];
        [bridge.window setIgnoresMouseEvents:NO];
        [bridge.window setAcceptsMouseMovedEvents:YES];
        [bridge.window makeFirstResponder:bridge.captureView];
        if (!bridge.cursorHidden) {
            [NSCursor hide];
            bridge.cursorHidden = YES;
        }
        if (shouldBeRelative) {
            CGAssociateMouseAndMouseCursorPosition(false);
        } else if (wasRelative) {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
    } else {
        if (wasRelative) {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
        if (bridge.cursorHidden) {
            [NSCursor unhide];
            bridge.cursorHidden = NO;
        }
    }
}

static void noland_update_debug_overlay(NolandMacosStreamInputBridge *bridge) {
    if (bridge == nil || bridge.captureView == nil) {
        return;
    }

    NSRect bounds = bridge.captureView.bounds;
    CGFloat width = MIN(460.0, MAX(320.0, NSWidth(bounds) - 24.0));
    CGFloat height = 124.0;

    NSString *text = [NSString stringWithFormat:
        @"capture: %@ (%d) req:%llu\n"
         "native move:%llu down:%llu up:%llu key:%llu\n"
         "rust rel:%llu abs:%llu btn:%llu key:%llu\n"
         "send rel:%llu abs:%llu btn:%llu key:%llu scr:%llu err:%llu",
        noland_macos_input_debug_capture_active() ? @"active" : @"inactive",
        noland_macos_input_debug_capture_mode(),
        noland_macos_input_debug_capture_requests(),
        noland_macos_input_debug_native_mouse_moves(),
        noland_macos_input_debug_native_mouse_downs(),
        noland_macos_input_debug_native_mouse_ups(),
        noland_macos_input_debug_native_keys(),
        noland_macos_input_debug_rust_relative_callbacks(),
        noland_macos_input_debug_rust_absolute_callbacks(),
        noland_macos_input_debug_rust_button_callbacks(),
        noland_macos_input_debug_rust_key_callbacks(),
        noland_input_debug_relative_send_attempts(),
        noland_input_debug_absolute_send_attempts(),
        noland_input_debug_button_send_attempts(),
        noland_input_debug_key_send_attempts(),
        noland_input_debug_scroll_send_attempts(),
        noland_input_debug_send_errors()];

    if (bridge.debugTextLayer != nil) {
        bridge.debugTextLayer.frame = CGRectMake(NSWidth(bounds) - width - 12.0, 12.0, width, height);
        bridge.debugTextLayer.string = text;
    }

    if (bridge.debugBadgeView != nil && bridge.debugBadgeLabel != nil) {
        bridge.debugBadgeView.frame = NSMakeRect(NSWidth(bounds) - width - 12.0, NSHeight(bounds) - height - 12.0, width, height);
        bridge.debugBadgeLabel.frame = NSInsetRect(bridge.debugBadgeView.bounds, 10.0, 8.0);
        bridge.debugBadgeLabel.stringValue = text;
    }
}

static NSView *noland_macos_ensure_stream_target_view(NSView *view) {
    if (view == nil) {
        return nil;
    }

    if ([view isKindOfClass:[NolandMacosStreamContainerView class]]) {
        return view;
    }

    NSWindow *window = view.window;
    NSView *contentView = window != nil ? window.contentView : view;
    if (contentView == nil) {
        return view;
    }

    NolandMacosStreamContainerView *container = objc_getAssociatedObject(contentView, kNolandMacosStreamContainerViewKey);
    if (container == nil) {
        container = [[NolandMacosStreamContainerView alloc] initWithFrame:contentView.bounds];
        [contentView addSubview:container positioned:NSWindowAbove relativeTo:nil];
        objc_setAssociatedObject(contentView, kNolandMacosStreamContainerViewKey, container, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    } else if (container.superview != contentView) {
        container.frame = contentView.bounds;
        [contentView addSubview:container positioned:NSWindowAbove relativeTo:nil];
    }

    return container;
}

void *noland_macos_resolve_stream_target_view(void *ns_view) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return NULL;
        }

        NSView *view = (__bridge NSView *)ns_view;
        NSView *target = noland_macos_ensure_stream_target_view(view);
        return (__bridge void *)(target != nil ? target : view);
    }
}

int noland_macos_input_install(void *ns_view) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return -1;
        }

        NSView *view = noland_macos_ensure_stream_target_view((__bridge NSView *)ns_view);
        NSWindow *window = view.window;
        if (window == nil) {
            return -2;
        }

        NolandMacosStreamInputBridge *existing = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (existing != nil) {
            existing.view = view;
            existing.window = window;
            if (existing.captureView.superview != view) {
                [existing.captureView removeFromSuperview];
                existing.captureView.frame = view.bounds;
                [view addSubview:existing.captureView positioned:NSWindowAbove relativeTo:nil];
            }
            [noland_stream_input_bridges() addObject:existing];
            noland_update_debug_overlay_visibility(existing);
            return 0;
        }

        NolandMacosStreamInputBridge *bridge = [NolandMacosStreamInputBridge new];
        bridge.view = view;
        bridge.window = window;
        bridge.captureActive = NO;
        bridge.captureMode = kNolandCaptureModeNone;
        bridge.cursorHidden = NO;
        bridge.suppressNextLeftMouseUp = NO;

        NolandMacosCaptureView *captureView = [[NolandMacosCaptureView alloc] initWithFrame:view.bounds];
        captureView.bridge = bridge;
        captureView.hidden = NO;
        bridge.captureView = captureView;
        [view addSubview:captureView positioned:NSWindowAbove relativeTo:nil];

        NSView *debugBadgeView = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 360, 124)];
        debugBadgeView.wantsLayer = YES;
        debugBadgeView.layer.backgroundColor = [NSColor colorWithWhite:0.02 alpha:0.88].CGColor;
        debugBadgeView.layer.cornerRadius = 10.0;
        debugBadgeView.layer.borderWidth = 2.0;
        debugBadgeView.layer.borderColor = NSColor.systemYellowColor.CGColor;
        debugBadgeView.hidden = !kNolandDebugOverlayEnabled;
        debugBadgeView.autoresizingMask = NSViewMinXMargin | NSViewMinYMargin;
        NSTextField *debugBadgeLabel = [[NSTextField alloc] initWithFrame:NSInsetRect(debugBadgeView.bounds, 10.0, 8.0)];
        debugBadgeLabel.bezeled = NO;
        debugBadgeLabel.drawsBackground = NO;
        debugBadgeLabel.editable = NO;
        debugBadgeLabel.selectable = NO;
        debugBadgeLabel.textColor = NSColor.whiteColor;
        debugBadgeLabel.font = [NSFont monospacedSystemFontOfSize:12.0 weight:NSFontWeightSemibold];
        debugBadgeLabel.alignment = NSTextAlignmentLeft;
        debugBadgeLabel.lineBreakMode = NSLineBreakByWordWrapping;
        debugBadgeLabel.maximumNumberOfLines = 4;
        [debugBadgeView addSubview:debugBadgeLabel];
        [captureView addSubview:debugBadgeView positioned:NSWindowAbove relativeTo:nil];
        bridge.debugBadgeView = debugBadgeView;
        bridge.debugBadgeLabel = debugBadgeLabel;

        CATextLayer *debugTextLayer = [CATextLayer layer];
        debugTextLayer.contentsScale = NSScreen.mainScreen.backingScaleFactor > 0 ? NSScreen.mainScreen.backingScaleFactor : 2.0;
        debugTextLayer.wrapped = YES;
        debugTextLayer.fontSize = 11.0;
        debugTextLayer.foregroundColor = NSColor.whiteColor.CGColor;
        debugTextLayer.backgroundColor = [NSColor colorWithWhite:0.06 alpha:0.72].CGColor;
        debugTextLayer.cornerRadius = 8.0;
        debugTextLayer.borderWidth = 1.0;
        debugTextLayer.borderColor = [NSColor colorWithRed:0.2 green:0.85 blue:0.95 alpha:0.7].CGColor;
        debugTextLayer.zPosition = 1000.0;
        bridge.debugTextLayer = debugTextLayer;
        debugTextLayer.hidden = !kNolandDebugOverlayEnabled;
        [captureView.layer addSublayer:debugTextLayer];
        bridge.localEventMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:(NSEventMaskMouseMoved |
                                                                                  NSEventMaskLeftMouseDown |
                                                                                  NSEventMaskLeftMouseUp |
                                                                                  NSEventMaskRightMouseDown |
                                                                                  NSEventMaskRightMouseUp |
                                                                                  NSEventMaskOtherMouseDown |
                                                                                  NSEventMaskOtherMouseUp |
                                                                                  NSEventMaskLeftMouseDragged |
                                                                                  NSEventMaskRightMouseDragged |
                                                                                  NSEventMaskOtherMouseDragged |
                                                                                  NSEventMaskScrollWheel |
                                                                                  NSEventMaskKeyDown |
                                                                                  NSEventMaskKeyUp |
                                                                                  NSEventMaskFlagsChanged)
                                                                       handler:^NSEvent * _Nullable(NSEvent * _Nonnull event) {
            if (event.window != bridge.window) {
                return event;
            }
            BOOL handled = noland_route_local_event(bridge, event);
            return handled ? nil : event;
        }];

        NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
        bridge.didBecomeKeyObserver = [center addObserverForName:NSWindowDidBecomeKeyNotification object:window queue:nil usingBlock:^(NSNotification * _Nonnull note) {
            (void)note;
            noland_macos_input_on_focus_changed(true);
        }];
        bridge.didResignKeyObserver = [center addObserverForName:NSWindowDidResignKeyNotification object:window queue:nil usingBlock:^(NSNotification * _Nonnull note) {
            (void)note;
            noland_set_capture_state(bridge, NO, kNolandCaptureModeNone);
            noland_macos_input_on_focus_changed(false);
        }];

        objc_setAssociatedObject(view, kNolandMacosStreamInputBridgeKey, bridge, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        [noland_stream_input_bridges() addObject:bridge];
        noland_update_debug_overlay_visibility(bridge);
        return 0;
    }
}

void noland_macos_input_uninstall(void *ns_view) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return;
        }

        NSView *view = noland_macos_ensure_stream_target_view((__bridge NSView *)ns_view);
        NolandMacosStreamInputBridge *bridge = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (bridge == nil) {
            return;
        }

        noland_set_capture_state(bridge, NO, kNolandCaptureModeNone);
        [bridge.captureView removeFromSuperview];
        bridge.captureView = nil;
        [bridge.debugTextLayer removeFromSuperlayer];
        bridge.debugTextLayer = nil;
        [bridge.debugBadgeView removeFromSuperview];
        bridge.debugBadgeView = nil;
        bridge.debugBadgeLabel = nil;
        [bridge.debugTimer invalidate];
        bridge.debugTimer = nil;
        [noland_stream_input_bridges() removeObject:bridge];
        if (bridge.localEventMonitor != nil) {
            [NSEvent removeMonitor:bridge.localEventMonitor];
            bridge.localEventMonitor = nil;
        }

        NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
        if (bridge.didBecomeKeyObserver != nil) {
            [center removeObserver:bridge.didBecomeKeyObserver];
            bridge.didBecomeKeyObserver = nil;
        }
        if (bridge.didResignKeyObserver != nil) {
            [center removeObserver:bridge.didResignKeyObserver];
            bridge.didResignKeyObserver = nil;
        }

        objc_setAssociatedObject(view, kNolandMacosStreamInputBridgeKey, nil, OBJC_ASSOCIATION_ASSIGN);
    }
}

int noland_macos_input_set_capture_active(void *ns_view, bool active, int mode) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return -1;
        }

        NSView *view = noland_macos_ensure_stream_target_view((__bridge NSView *)ns_view);
        NolandMacosStreamInputBridge *bridge = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (bridge == nil) {
            return 1;
        }

        bridge.view = view;
        bridge.window = view.window;
        if (bridge.window == nil) {
            return -2;
        }
        if (!active) {
            bridge.suppressNextLeftMouseUp = NO;
        }
        if (bridge.captureView.superview != view) {
            [bridge.captureView removeFromSuperview];
            bridge.captureView.frame = view.bounds;
            [view addSubview:bridge.captureView positioned:NSWindowAbove relativeTo:nil];
        }
        if (bridge.debugBadgeView != nil && bridge.debugBadgeView.superview != bridge.captureView) {
            [bridge.debugBadgeView removeFromSuperview];
            [bridge.captureView addSubview:bridge.debugBadgeView positioned:NSWindowAbove relativeTo:nil];
        }
        if (bridge.debugTextLayer != nil && bridge.debugTextLayer.superlayer != bridge.captureView.layer) {
            [bridge.debugTextLayer removeFromSuperlayer];
            [bridge.captureView.layer addSublayer:bridge.debugTextLayer];
            bridge.debugTextLayer.zPosition = 1000.0;
        }

        noland_set_capture_state(bridge, active ? YES : NO, mode);
        noland_update_debug_overlay_visibility(bridge);
        return 0;
    }
}
