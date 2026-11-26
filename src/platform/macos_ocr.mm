// macOS OCR：Vision 框架识别屏幕文字。
// Rust 侧不直接绑 Vision（ObjC 接口过于庞杂），这里用 Objective-C++ 写一个
// 最小实现，返回 JSON 字符串（调用方用 free 释放）。
#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <Vision/Vision.h>
#include <stdlib.h>
#include <string.h>

extern "C" const char *npa_ocr_recognize(const char *path, const char *lang) {
    @autoreleasepool {
        if (path == NULL) {
            return strdup("{\"error\":\"path is null\"}");
        }
        NSString *p = [NSString stringWithUTF8String:path];
        NSString *l = (lang && *lang) ? [NSString stringWithUTF8String:lang] : nil;

        NSImage *img = [[NSImage alloc] initWithContentsOfFile:p];
        if (img == nil) {
            return strdup("{\"engine\":\"vision\",\"error\":\"cannot open image\",\"lines\":[]}");
        }
        CGImageRef cg = [img CGImageForProposedRect:NULL context:nil hints:nil];
        if (cg == NULL) {
            return strdup("{\"engine\":\"vision\",\"error\":\"cannot decode image\",\"lines\":[]}");
        }
        size_t w = CGImageGetWidth(cg);
        size_t h = CGImageGetHeight(cg);

        VNRecognizeTextRequest *req = [[VNRecognizeTextRequest alloc] init];
        req.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
        req.usesLanguageCorrection = YES;
        if (l != nil) {
            req.recognitionLanguages = @[l];
        }

        VNImageRequestHandler *handler =
            [[VNImageRequestHandler alloc] initWithCGImage:cg options:@{}];
        NSError *err = nil;
        if (![handler performRequests:@[req] error:&err]) {
            NSString *msg = err ? err.localizedDescription : @"vision request failed";
            NSDictionary *d = @{ @"engine": @"vision", @"error": msg, @"lines": @[] };
            NSData *data = [NSJSONSerialization dataWithJSONObject:d options:0 error:nil];
            NSString *s = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
            return strdup(s.UTF8String);
        }

        NSMutableArray *arr = [NSMutableArray array];
        for (VNRecognizedTextObservation *obs in req.results) {
            VNRecognizedText *top = [[obs topCandidates:1] firstObject];
            if (top == nil) {
                continue;
            }
            // Vision 用归一化坐标（原点左下），这里换算成像素、原点左上。
            CGRect bb = obs.boundingBox;
            double x = bb.origin.x * (double)w;
            double bw = bb.size.width * (double)w;
            double bh = bb.size.height * (double)h;
            double y = (1.0 - bb.origin.y - bb.size.height) * (double)h;
            [arr addObject:@{
                @"text": top.string,
                @"x": @((int)lround(x)),
                @"y": @((int)lround(y)),
                @"w": @((int)lround(bw)),
                @"h": @((int)lround(bh))
            }];
        }
        NSDictionary *payload = @{ @"engine": @"vision", @"lines": arr };
        NSData *data = [NSJSONSerialization dataWithJSONObject:payload options:0 error:nil];
        NSString *s = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
        return strdup(s.UTF8String);
    }
}
