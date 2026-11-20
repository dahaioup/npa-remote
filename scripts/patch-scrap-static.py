#!/usr/bin/env python3
"""给 scrap 的 build.rs 注入静态链接声明（仅 Linux 交叉编译用）。

背景：交叉编译时 pkg-config crate 只会 emit 动态的 -lnpa_vpx，而 rustc 把这些
-l 参数放在 -Wl,-Bdynamic 之后，链接器（zig lld）就只会去找同名 .so，报
"unable to find dynamic system library"。这里改成显式的
cargo:rustc-link-lib=static=npa_vpx / npa_aom / npa_yuv。
"""
import os
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "third_party/rustdesk/libs/scrap/build.rs"

if not os.path.exists(path):
    sys.exit("ERROR: %s 不存在" % path)

src = open(path, encoding="utf-8").read()
marker = "fn main() {\n"
inject = """
    // NPA: 静态链接 libvpx/libyuv/libaom（交叉编译下 pkg-config crate 只会 emit 动态 -l）
    if let Ok(root) = std::env::var("NPA_STATIC_LIB") {
        println!("cargo:rustc-link-search=native={}", root);
        println!("cargo:rustc-link-lib=static=npa_vpx");
        println!("cargo:rustc-link-lib=static=npa_aom");
        println!("cargo:rustc-link-lib=static=npa_yuv");
    }
"""
if "NPA_STATIC_LIB" in src:
    print("scrap build.rs 已打过补丁，跳过")
    sys.exit(0)
if marker not in src:
    sys.exit("ERROR: %s 里找不到 'fn main() {'" % path)

src = src.replace(marker, marker + inject, 1)
open(path, "w", encoding="utf-8").write(src)
print("patched %s" % path)
