// Windowless AppKit fixture for the macOS launch integration test only.
#import <AppKit/AppKit.h>
#include <unistd.h>

int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp finishLaunching];
        NSDictionary *environment = NSProcessInfo.processInfo.environment;
        NSString *root = environment[@"LITTLE_MUNGBEAN_LAUNCH_FIXTURE"];
        if (!root) return 1;
        NSDictionary *probe = @{
            @"pid": @(getpid()),
            @"arguments": NSProcessInfo.processInfo.arguments,
            @"CODEX_HOME": environment[@"CODEX_HOME"] ?: [NSNull null],
            @"OPENAI_BASE_URL": environment[@"OPENAI_BASE_URL"] ?: [NSNull null],
            @"OPENAI_API_KEY": environment[@"OPENAI_API_KEY"] ?: [NSNull null],
            @"CLEARED_ROUTE": environment[@"LITTLE_MUNGBEAN_TEST_CLEAR"] ?: [NSNull null],
        };
        NSData *data = [NSJSONSerialization dataWithJSONObject:probe options:0 error:NULL];
        if (![data writeToFile:[root stringByAppendingPathComponent:@"launched.json"] atomically:YES]) return 2;
        NSString *stop = [root stringByAppendingPathComponent:@"stop"];
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:45];
        while (![NSFileManager.defaultManager fileExistsAtPath:stop] && deadline.timeIntervalSinceNow > 0) {
            [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.05]];
        }
    }
    return 0;
}
