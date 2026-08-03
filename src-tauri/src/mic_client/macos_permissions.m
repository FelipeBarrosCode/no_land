#import <AVFoundation/AVFoundation.h>
#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>

int noland_macos_ensure_microphone_access(void) {
    @autoreleasepool {
        if (@available(macOS 10.14, *)) {
            AVAuthorizationStatus status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
            switch (status) {
                case AVAuthorizationStatusAuthorized:
                    return 0;
                case AVAuthorizationStatusDenied:
                case AVAuthorizationStatusRestricted:
                    return 1;
                case AVAuthorizationStatusNotDetermined: {
                    __block BOOL granted = NO;
                    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
                    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL accessGranted) {
                        granted = accessGranted;
                        dispatch_semaphore_signal(semaphore);
                    }];
                    long waitResult = dispatch_semaphore_wait(
                        semaphore,
                        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(30 * NSEC_PER_SEC))
                    );
                    if (waitResult != 0) {
                        return 2;
                    }
                    return granted ? 0 : 1;
                }
            }
        }

        return 0;
    }
}
