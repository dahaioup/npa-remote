#!/usr/bin/env python3
"""给 scrap 打补丁：X11 抓图后主动 flush XCB 连接（Linux 必需）。

背景：scrap 的 X11 抓图用 `xcb_shm_get_image_unchecked`（不等待回包），
请求发出后必须 flush 才会真正送到 X server；共享内存要等 X server 处理完
请求才更新。RustDesk 在 base 层定期 flush XCB 连接，独立使用 scrap 时
没有这一层 → 画面永远停在首帧（实测 20 秒只有 2 帧）。

补丁做两件事：
  1. src/x11/ffi.rs：补 xcb_flush 的 FFI 声明；
  2. src/x11/capturer.rs：get_image() 之后 flush + 短暂等待，
     让 shm 缓冲在下一次比较前已是新画面。
"""
import os
import sys

root = sys.argv[1] if len(sys.argv) > 1 else "third_party/rustdesk/libs/scrap"

ffi_path = os.path.join(root, "src/x11/ffi.rs")
cap_path = os.path.join(root, "src/x11/capturer.rs")

for p in (ffi_path, cap_path):
    if not os.path.exists(p):
        print("跳过：%s 不存在" % p)
        sys.exit(0)

# --- 1. ffi.rs: xcb_flush 声明 ---
ffi = open(ffi_path, encoding="utf-8").read()
if "xcb_flush" not in ffi:
    anchor = "    pub fn xcb_disconnect(c: *mut xcb_connection_t);\n"
    if anchor not in ffi:
        sys.exit("ERROR: %s 里找不到 xcb_disconnect 锚点" % ffi_path)
    ffi = ffi.replace(
        anchor,
        anchor + "\n    pub fn xcb_flush(c: *mut xcb_connection_t) -> i32;\n",
        1,
    )
    open(ffi_path, "w", encoding="utf-8").write(ffi)
    print("patched %s" % ffi_path)
else:
    print("ffi.rs 已打补丁，跳过")

# --- 2. capturer.rs: get_image 之后 flush ---
cap = open(cap_path, encoding="utf-8").read()
if "NPA: flush XCB" in cap:
    print("capturer.rs 已打补丁，跳过")
    sys.exit(0)
anchor = """            libc::free(response as *mut _);
        }
    }
"""
inject = """            libc::free(response as *mut _);
            // NPA: flush XCB — unchecked 请求必须推出才会被 X server 处理，
            // 否则 shm 内容不更新、画面永远停在首帧（RustDesk 在 base 层做这件事）。
            xcb_flush(self.display.server().raw());
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
"""
if anchor not in cap:
    sys.exit("ERROR: %s 里找不到 get_image 锚点（上游 scrap 结构可能变了）" % cap_path)
open(cap_path, "w", encoding="utf-8").write(cap.replace(anchor, inject, 1))
print("patched %s" % cap_path)
