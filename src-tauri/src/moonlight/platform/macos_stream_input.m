#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <objc/runtime.h>

extern void noland_macos_input_on_relative_mouse(double delta_x, double delta_y);
extern void noland_macos_input_on_absolute_mouse(double x, double y);
extern void noland_macos_input_on_mouse_button(unsigned char button, bool pressed);
extern void noland_macos_input_on_keyboard(unsigned short virtual_key, bool pressed, unsigned char modifiers);
extern void noland_macos_input_on_vertical_scroll(double amount, bool high_resolution);
extern void noland_macos_input_on_horizontal_scroll(double amount, bool high_resolution);
extern void noland_macos_input_on_focus_changed(bool focused);

@interface NolandMacosStreamInputBridge : NSObject
@property (nonatomic, assign) NSView *view;
@property (nonatomic, weak) NSWindow *window;
@property (nonatomic, assign) BOOL captureActive;
@property (nonatomic, assign) NSInteger captureMode;
@property (nonatomic, assign) BOOL cursorHidden;
@property (nonatomic, assign) NSEventModifierFlags lastModifierFlags;
@property (nonatomic, strong) id localMonitor;
@property (nonatomic, strong) id didBecomeKeyObserver;
@property (nonatomic, strong) id didResignKeyObserver;
@end

@implementation NolandMacosStreamInputBridge
@end

static const void *kNolandMacosStreamInputBridgeKey = &kNolandMacosStreamInputBridgeKey;
static const NSInteger kNolandCaptureModeNone = 0;
static const NSInteger kNolandCaptureModeRelative = 1;
static const NSInteger kNolandCaptureModeAbsolute = 2;
static const unsigned char kNolandModifierShift = 0x01;
static const unsigned char kNolandModifierCtrl = 0x02;
static const unsigned char kNolandModifierAlt = 0x04;
static const unsigned char kNolandModifierMeta = 0x08;

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

    noland_macos_input_on_absolute_mouse((double)x, (double)y);
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

static void noland_set_capture_state(NolandMacosStreamInputBridge *bridge, BOOL active, NSInteger mode) {
    if (bridge == nil) {
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

    if (active) {
        [bridge.window makeFirstResponder:bridge.view];
        [bridge.window makeKeyAndOrderFront:nil];
        [bridge.window setAcceptsMouseMovedEvents:YES];
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

int noland_macos_input_install(void *ns_view) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return -1;
        }

        NSView *view = (__bridge NSView *)ns_view;
        NSWindow *window = view.window;
        if (window == nil) {
            return -2;
        }

        NolandMacosStreamInputBridge *existing = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (existing != nil) {
            existing.view = view;
            existing.window = window;
            return 0;
        }

        NolandMacosStreamInputBridge *bridge = [NolandMacosStreamInputBridge new];
        bridge.view = view;
        bridge.window = window;
        bridge.captureActive = NO;
        bridge.captureMode = kNolandCaptureModeNone;
        bridge.cursorHidden = NO;
        bridge.lastModifierFlags = 0;
        __weak NolandMacosStreamInputBridge *weakBridge = bridge;

        NSEventMask mask = NSEventMaskMouseMoved |
            NSEventMaskLeftMouseDragged |
            NSEventMaskRightMouseDragged |
            NSEventMaskOtherMouseDragged |
            NSEventMaskLeftMouseDown |
            NSEventMaskLeftMouseUp |
            NSEventMaskRightMouseDown |
            NSEventMaskRightMouseUp |
            NSEventMaskOtherMouseDown |
            NSEventMaskOtherMouseUp |
            NSEventMaskScrollWheel |
            NSEventMaskKeyDown |
            NSEventMaskKeyUp |
            NSEventMaskFlagsChanged;

        bridge.localMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:mask handler:^NSEvent * _Nullable(NSEvent * _Nonnull event) {
            NolandMacosStreamInputBridge *strongBridge = weakBridge;
            if (strongBridge == nil || !strongBridge.captureActive) {
                return event;
            }
            if (event.window != strongBridge.window) {
                return event;
            }

            switch (event.type) {
                case NSEventTypeMouseMoved:
                case NSEventTypeLeftMouseDragged:
                case NSEventTypeRightMouseDragged:
                case NSEventTypeOtherMouseDragged:
                    if (strongBridge.captureMode == kNolandCaptureModeRelative) {
                        noland_macos_input_on_relative_mouse(event.deltaX, event.deltaY);
                    } else if (strongBridge.captureMode == kNolandCaptureModeAbsolute) {
                        noland_emit_absolute_mouse(strongBridge, event);
                    }
                    return nil;
                case NSEventTypeLeftMouseDown:
                case NSEventTypeRightMouseDown:
                case NSEventTypeOtherMouseDown:
                    if (strongBridge.captureMode == kNolandCaptureModeAbsolute) {
                        noland_emit_absolute_mouse(strongBridge, event);
                    }
                    noland_macos_input_on_mouse_button(noland_map_mouse_button(event), true);
                    return nil;
                case NSEventTypeLeftMouseUp:
                case NSEventTypeRightMouseUp:
                case NSEventTypeOtherMouseUp:
                    if (strongBridge.captureMode == kNolandCaptureModeAbsolute) {
                        noland_emit_absolute_mouse(strongBridge, event);
                    }
                    noland_macos_input_on_mouse_button(noland_map_mouse_button(event), false);
                    return nil;
                case NSEventTypeScrollWheel: {
                    BOOL highResolution = event.hasPreciseScrollingDeltas;
                    CGFloat deltaY = highResolution ? event.scrollingDeltaY : event.deltaY;
                    CGFloat deltaX = highResolution ? event.scrollingDeltaX : event.deltaX;
                    if (deltaY != 0.0) {
                        noland_macos_input_on_vertical_scroll((double)deltaY, highResolution);
                    }
                    if (deltaX != 0.0) {
                        noland_macos_input_on_horizontal_scroll((double)deltaX, highResolution);
                    }
                    return nil;
                }
                case NSEventTypeKeyDown: {
                    if (noland_is_release_shortcut(event)) {
                        return event;
                    }
                    unsigned short virtualKey = noland_vk_for_key_code(event.keyCode);
                    if (virtualKey != 0x00) {
                        noland_macos_input_on_keyboard(virtualKey, true, noland_modifier_bits(event.modifierFlags));
                    }
                    return nil;
                }
                case NSEventTypeKeyUp: {
                    unsigned short virtualKey = noland_vk_for_key_code(event.keyCode);
                    if (virtualKey != 0x00) {
                        noland_macos_input_on_keyboard(virtualKey, false, noland_modifier_bits(event.modifierFlags));
                    }
                    return nil;
                }
                case NSEventTypeFlagsChanged: {
                    unsigned short virtualKey = noland_vk_for_key_code(event.keyCode);
                    if (virtualKey != 0x00) {
                        noland_macos_input_on_keyboard(virtualKey, noland_modifier_pressed_for_event(event), noland_modifier_bits(event.modifierFlags));
                    }
                    strongBridge.lastModifierFlags = event.modifierFlags;
                    return nil;
                }
                default:
                    return event;
            }
        }];

        NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
        bridge.didBecomeKeyObserver = [center addObserverForName:NSWindowDidBecomeKeyNotification object:window queue:nil usingBlock:^(NSNotification * _Nonnull note) {
            (void)note;
            noland_macos_input_on_focus_changed(true);
        }];
        bridge.didResignKeyObserver = [center addObserverForName:NSWindowDidResignKeyNotification object:window queue:nil usingBlock:^(NSNotification * _Nonnull note) {
            (void)note;
            noland_macos_input_on_focus_changed(false);
        }];

        objc_setAssociatedObject(view, kNolandMacosStreamInputBridgeKey, bridge, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        return 0;
    }
}

void noland_macos_input_uninstall(void *ns_view) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return;
        }

        NSView *view = (__bridge NSView *)ns_view;
        NolandMacosStreamInputBridge *bridge = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (bridge == nil) {
            return;
        }

        noland_set_capture_state(bridge, NO, kNolandCaptureModeNone);
        if (bridge.localMonitor != nil) {
            [NSEvent removeMonitor:bridge.localMonitor];
            bridge.localMonitor = nil;
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

        NSView *view = (__bridge NSView *)ns_view;
        NolandMacosStreamInputBridge *bridge = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (bridge == nil) {
            return 1;
        }

        bridge.view = view;
        bridge.window = view.window;
        if (bridge.window == nil) {
            return -2;
        }

        noland_set_capture_state(bridge, active ? YES : NO, mode);
        return 0;
    }
}
