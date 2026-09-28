#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>

int main(int argc, const char * argv[]) {
    @autoreleasepool {
        NSScreen *screen = [NSScreen mainScreen];
        NSValue *sizeValue = screen.deviceDescription[@"NSDeviceSize"];
        NSSize size = [sizeValue sizeValue];
        NSLog(@"NSDeviceSize: %f x %f", size.width, size.height);
        
        NSNumber *screenNumber = screen.deviceDescription[@"NSScreenNumber"];
        CGDirectDisplayID displayID = (CGDirectDisplayID)screenNumber.unsignedIntValue;
        CGDisplayModeRef mode = CGDisplayCopyDisplayMode(displayID);
        size_t pixelWidth = CGDisplayModeGetPixelWidth(mode);
        size_t pixelHeight = CGDisplayModeGetPixelHeight(mode);
        NSLog(@"CGDisplayModeGetPixelSize: %zu x %zu", pixelWidth, pixelHeight);
    }
    return 0;
}
