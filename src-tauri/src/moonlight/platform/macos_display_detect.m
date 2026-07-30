#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <math.h>

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
