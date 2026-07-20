#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <objc/runtime.h>

extern void noland_macos_input_on_relative_mouse(double delta_x, double delta_y);
extern void noland_macos_input_on_mouse_button(unsigned char button, bool pressed);

@interface NolandMacosStreamInputBridge : NSObject
@property (nonatomic, assign) NSView *view;
@property (nonatomic, weak) NSWindow *window;
@property (nonatomic, assign) BOOL captureActive;
@property (nonatomic, strong) id localMouseMonitor;
@end

@implementation NolandMacosStreamInputBridge
@end

static const void *kNolandMacosStreamInputBridgeKey = &kNolandMacosStreamInputBridgeKey;

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

static void noland_set_capture_state(NolandMacosStreamInputBridge *bridge, BOOL active) {
    if (bridge == nil || bridge.captureActive == active) {
        return;
    }

    bridge.captureActive = active;
    if (active) {
        [bridge.window makeFirstResponder:bridge.view];
        [bridge.window makeKeyAndOrderFront:nil];
        [bridge.window setAcceptsMouseMovedEvents:YES];
        [NSCursor hide];
        CGAssociateMouseAndMouseCursorPosition(false);
    } else {
        CGAssociateMouseAndMouseCursorPosition(true);
        [NSCursor unhide];
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
            existing.window = window;
            return 0;
        }

        NolandMacosStreamInputBridge *bridge = [NolandMacosStreamInputBridge new];
        bridge.view = view;
        bridge.window = window;
        __weak NolandMacosStreamInputBridge *weakBridge = bridge;

        bridge.localMouseMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:
            NSEventMaskMouseMoved |
            NSEventMaskLeftMouseDragged |
            NSEventMaskRightMouseDragged |
            NSEventMaskOtherMouseDragged |
            NSEventMaskLeftMouseDown |
            NSEventMaskLeftMouseUp |
            NSEventMaskRightMouseDown |
            NSEventMaskRightMouseUp |
            NSEventMaskOtherMouseDown |
            NSEventMaskOtherMouseUp
            handler:^NSEvent * _Nullable(NSEvent * _Nonnull event) {
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
                        noland_macos_input_on_relative_mouse(event.deltaX, event.deltaY);
                        return nil;
                    case NSEventTypeLeftMouseDown:
                    case NSEventTypeRightMouseDown:
                    case NSEventTypeOtherMouseDown:
                        noland_macos_input_on_mouse_button(noland_map_mouse_button(event), true);
                        return nil;
                    case NSEventTypeLeftMouseUp:
                    case NSEventTypeRightMouseUp:
                    case NSEventTypeOtherMouseUp:
                        noland_macos_input_on_mouse_button(noland_map_mouse_button(event), false);
                        return nil;
                    default:
                        return event;
                }
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

        noland_set_capture_state(bridge, NO);
        if (bridge.localMouseMonitor != nil) {
            [NSEvent removeMonitor:bridge.localMouseMonitor];
            bridge.localMouseMonitor = nil;
        }
        objc_setAssociatedObject(view, kNolandMacosStreamInputBridgeKey, nil, OBJC_ASSOCIATION_ASSIGN);
    }
}

int noland_macos_input_set_capture_active(void *ns_view, bool active) {
    @autoreleasepool {
        if (ns_view == NULL) {
            return -1;
        }

        NSView *view = (__bridge NSView *)ns_view;
        NolandMacosStreamInputBridge *bridge = objc_getAssociatedObject(view, kNolandMacosStreamInputBridgeKey);
        if (bridge == nil) {
            return 1;
        }

        bridge.window = view.window;
        if (bridge.window == nil) {
            return -2;
        }

        noland_set_capture_state(bridge, active ? YES : NO);
        return 0;
    }
}
