#!/usr/bin/env python3
"""给 scrap 打补丁，适配无头 Linux（Xvfb）环境。

三处修改：

1. src/x11/ffi.rs：补 xcb_flush 的 FFI 声明（上游没绑）。

2. src/x11/capturer.rs：get_image() 之后 flush + 同步等待。
   scrap 的 X11 抓图用 `xcb_shm_get_image_unchecked`（不等待回包），请求发出后
   必须 flush 才会真正送到 X server；共享内存要等 X server 处理完请求才更新。
   RustDesk 在 base 层定期 flush，独立使用 scrap 时没有这一层 → 画面永远停在
   首帧（实测 20 秒只有 2 帧）。
   只 flush 不够：flush 只保证字节写进 socket，服务器处理是异步的；因此 flush
   之后补发一个带 reply 的请求（shm query_version）并等它回包——按 X 协议顺序
   处理，等它回包就说明前面的 unchecked 抓图请求已经处理完、shm 已更新。

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

# --- 1b. ffi.rs: 同步版 xcb_shm_get_image 声明（带 cookie，能等回复） ---
if "pub fn xcb_shm_get_image(" not in ffi:
    anchor2 = "    pub fn xcb_shm_get_image_unchecked(\n"
    if anchor2 not in ffi:
        sys.exit("ERROR: %s 里找不到 xcb_shm_get_image_unchecked 锚点" % ffi_path)
    decl = """    pub fn xcb_shm_get_image(
        c: *mut xcb_connection_t,
        drawable: xcb_drawable_t,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        plane_mask: u32,
        format: u8,
        shmseg: xcb_shm_seg_t,
        offset: u32,
    ) -> xcb_shm_get_image_cookie_t;
"""
    ffi = ffi.replace(anchor2, decl + anchor2, 1)
    open(ffi_path, "w", encoding="utf-8").write(ffi)
    print("patched ffi.rs（同步 get_image 声明）")
else:
    print("ffi.rs 已有同步 get_image 声明，跳过")

# --- 2. capturer.rs: get_image 之后 flush ---
cap = open(cap_path, encoding="utf-8").read()
old_block = """            // NPA: flush XCB — unchecked 请求必须推出才会被 X server 处理，
            // 否则 shm 内容不更新、画面永远停在首帧（RustDesk 在 base 层做这件事）。
            xcb_flush(self.display.server().raw());
            std::thread::sleep(std::time::Duration::from_millis(8));
"""
new_block = """            // NPA: flush + 同步 — unchecked 请求必须 flush 才会真的送到 X server；
            // 再发一个带 reply 的请求（shm query_version）等服务器把前一个请求处理完，
            // 共享内存内容才会更新（否则画面永远停在首帧，只推第一帧）。
            xcb_flush(self.display.server().raw());
            let sync_cookie = xcb_shm_query_version(self.display.server().raw());
            let _ = xcb_shm_query_version_reply(self.display.server().raw(), sync_cookie, ptr::null_mut());
"""
if old_block in cap:
    cap = cap.replace(old_block, new_block, 1)
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("upgraded %s（flush → flush+同步）" % cap_path)
elif "NPA: flush + 同步" in cap:
    print("capturer.rs 已是新版（flush + 同步），跳过")
else:
    anchor = """            libc::free(response as *mut _);
        }
    }
"""
    inject = """            libc::free(response as *mut _);
""" + new_block + """        }
    }
"""
    if anchor not in cap:
        sys.exit("ERROR: %s 里找不到 get_image 锚点（上游 scrap 结构可能变了）" % cap_path)
    open(cap_path, "w", encoding="utf-8").write(cap.replace(anchor, inject, 1))
    print("patched %s" % cap_path)

# --- 2b. capturer.rs: 改用同步版 get_image（带 cookie，等服务器回包） ---
# unchecked 版本在部分 X server（Xvfb）上不会真正填充共享内存，
# 改用带 cookie 的同步版本：请求发出后等回复，保证 shm 内容已更新。
if "NPA: sync get_image" in cap:
    print("capturer.rs 已用同步 get_image，跳过")
else:
    call_anchor = "let request = xcb_shm_get_image_unchecked("
    if call_anchor not in cap:
        sys.exit("ERROR: %s 里找不到 get_image 调用点（上游 scrap 结构可能变了）" % cap_path)
    cap = cap.replace(
        call_anchor,
        "let request = xcb_shm_get_image( // NPA: sync get_image\n                ",
        1,
    )
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("patched %s（同步 get_image）" % cap_path)

# --- 3. iter.rs: 撤销之前的 RandR 兜底（上游自带 screen fallback，无需补丁） ---
# 之前的版本在「RandR 1.5 monitor 枚举为空」时自己造一个 Display，
# 结果抓到 0 帧（pixfmt/rect 与真实 screen 不一致，采集直接失败）。
# 上游 next_screen 本来就会退回到 X screen，所以这里保持原样、只做撤销。
it = open(iter_path, encoding="utf-8").read()
if "NPA: RandR 1.5" in it:
    start = it.index("            // NPA: RandR 1.5")
    end_marker = "            // The current screen was empty, so try the next screen.\n"
    end = it.index(end_marker, start)
    it = it[:start] + it[end:]
    open(iter_path, "w", encoding="utf-8").write(it)
    print("reverted %s（撤销 RandR 兜底）" % iter_path)
else:
    print("iter.rs 无 RandR 兜底，保持上游原样")
