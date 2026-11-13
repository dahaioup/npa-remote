// 最小实现：只为提供 scrap Display::scale 所需的 BackingScaleFactor 符号。
// 函数体照抄 rustdesk src/platform/macos.mm（上游由主程序编译整个 macos.mm，
// 插件没有那个编译单元，这里只补这一个函数，避免拖进 AVFoundation/IOKit 等依赖）。
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#include <stdint.h>

// https://gist.github.com/briankc/025415e25900750f402235dbf1b74e42
extern "C" float BackingScaleFactor(uint32_t display) {
    @autoreleasepool {
        NSArray<NSScreen *> *screens = [NSScreen screens];
        for (NSScreen *screen in screens) {
            NSDictionary *deviceDescription = [screen deviceDescription];
            NSNumber *screenNumber = [deviceDescription objectForKey:@"NSScreenNumber"];
            CGDirectDisplayID screenDisplayID = [screenNumber unsignedIntValue];
            if (screenDisplayID == display) {
                return [screen backingScaleFactor];
            }
        }
        return 1;
    }
}
