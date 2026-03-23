#import <Cocoa/Cocoa.h>

@class SPPermissionManager;

@protocol SPStatusBarDelegate <NSObject>
@optional
- (void)statusBarDidSelectReloadConfig;
- (void)statusBarDidSelectReviewUncertainPhrases;
- (void)statusBarDidSelectQuit;
- (void)statusBarMenuDidOpen;
- (void)statusBarMenuDidClose;
@end

@interface SPStatusBarManager : NSObject <NSMenuDelegate>

- (instancetype)initWithDelegate:(id<SPStatusBarDelegate>)delegate
               permissionManager:(SPPermissionManager *)permissionManager;

/// Update the status bar icon and status text.
/// state: "idle", "recording_hold", "recording_toggle", "connecting_asr",
///        "finalizing_asr", "correcting", "preparing_paste", "pasting", "error", "completed"
- (void)updateState:(NSString *)state;

@end
