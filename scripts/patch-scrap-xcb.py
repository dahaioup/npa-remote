#!/usr/bin/env python3
"""给 scrap 打补丁，适配无头 Linux（Xvfb）环境。

三处修改：

1. src/x11/ffi.rs：补 xcb_flush 的 FFI 声明（上游没绑）。

2. src/x11/capturer.rs：get_image() 之后 flush + 短暂等待。
   scrap 的 X11 抓图用 `xcb_shm_get_image_unchecked`（不等待回包），请求发出后
   必须 flush 才会真正送到 X server；共享内存要等 X server 处理完请求才更新。
   RustDesk 在 base 层定期 flush，独立使用 scrap 时没有这一层 → 画面永远停在
   首帧（实测 20 秒只有 2 帧）。

3. src/x11/iter.rs：RandR 1.5 的 monitor 枚举为空时，用 X screen 的像素尺寸
   兜底造一个显示器。Xvfb 等无头虚拟屏只实现 RANDR 1.2（没有 monitor 概念），
   `xcb_randr_get_monitors` 返回空 → scrap 枚举不到任何显示器 → 采集起不来。
   RustDesk 桌面环境有真实显示器，所以上游没做这个兜底。
"""
import os
import sys

root = sys.argv[1] if len(sys.argv) > 1 else "third_party/rustdesk/libs/scrap"

ffi_path = os.path.join(root, "src/x11/ffi.rs")
cap_path = os.path.join(root, "src/x11/capturer.rs")
iter_path = os.path.join(root, "src/x11/iter.rs")

for p in (ffi_path, cap_path, iter_path):
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
else:
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

# --- 3. iter.rs: RandR 枚举为空时用 screen 尺寸兜底 ---
it = open(iter_path, encoding="utf-8").read()
if "NPA: RandR 1.5" in it:
    print("iter.rs 已打补丁，跳过")
    sys.exit(0)

anchor = """            // The current screen was empty, so try the next screen.
            self.free_reply();
"""
inject = """            // NPA: RandR 1.5 monitor 枚举为空时（Xvfb 等无头虚拟屏只实现
            // RANDR 1.2，没有 monitor 概念），用 X screen 的像素尺寸兜底造一个
            // 显示器，否则 scrap 枚举不到任何屏幕 → 采集永远起不来。
            unsafe {
                let mut roots = xcb_setup_roots_iterator(self.server.setup());
                if roots.rem != 0 {
                    let screen = &*roots.data;
                    let root = screen.root;
                    let w = screen.width_in_pixels as u16;
                    let h = screen.height_in_pixels as u16;
                    let pixfmt = get_pixfmt(self.server.raw(), root).unwrap_or(Pixfmt::BGRA);
                    let display = Display::new(
                        self.server.clone(),
                        true,
                        Rect {
                            x: 0,
                            y: 0,
                            w,
                            h,
                        },
                        root,
                        "screen0".to_owned(),
                        pixfmt,
                    );
                    return Some(display);
                }
            }

            // The current screen was empty, so try the next screen.
            self.free_reply();
"""
if anchor not in it:
    sys.exit("ERROR: %s 里找不到 iter 锚点（上游 scrap 结构可能变了）" % iter_path)
open(iter_path, "w", encoding="utf-8").write(it.replace(anchor, inject, 1))
print("patched %s" % iter_path)
