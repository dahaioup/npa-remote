#!/usr/bin/env python3
"""给 scrap 的 common/x11.rs 暴露底层 XCB 连接（Linux 采集需要主动 flush）。

背景：scrap 的 X11 抓图用 `xcb_shm_get_image_unchecked`（不等待回包），共享内存
要等 X server 处理完请求才更新。RustDesk 在 base 层定期 flush XCB 连接，
独立使用 scrap 时没有这一层 → 画面永远停在首帧（实测 20 秒只有 2 帧）。

这里在 Capturer 上加一个 raw_server()，让调用方在每次抓图前自己 xcb_flush。
"""
import os
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "third_party/rustdesk/libs/scrap/src/common/x11.rs"

if not os.path.exists(path):
    print("跳过：%s 不存在" % path)
    sys.exit(0)

src = open(path, encoding="utf-8").read()
if "raw_server" in src:
    print("scrap common/x11.rs 已打过补丁，跳过")
    sys.exit(0)

anchor = """    pub fn height(&self) -> usize {
        self.0.display().rect().h as usize
    }
"""
inject = anchor + """
    /// NPA: 暴露底层 XCB 连接指针（X11 抓图用 unchecked 请求，需要调用方 flush）
    pub fn raw_server(&self) -> *mut core::ffi::c_void {
        self.0.display().server().raw() as *mut core::ffi::c_void
    }
"""

if anchor not in src:
    sys.exit("ERROR: %s 里找不到锚点（上游 scrap 结构可能变了）" % path)

open(path, "w", encoding="utf-8").write(src.replace(anchor, inject, 1))
print("patched %s" % path)
