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
   兜底造一个显示器（抓根窗口）。无人值守机的会话 X 上所有输出口都是
   disconnected，`xcb_randr_get_monitors` 返回空 → scrap 枚举不到任何显示器 →
   采集起不来（黑屏）；但会话根窗口里有桌面内容。上游的兜底只是「换下一个
   screen」，单屏时直接返回 None，没有根窗口兜底。
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

# --- 3. iter.rs: RandR 枚举不到 monitor 时，用 X screen 尺寸抓根窗口兜底 ---
# 无人值守机的会话 X（lightdm/Xorg）上所有输出口都是 disconnected，
# RandR 1.5 的 `xcb_randr_get_monitors` 返回空列表 → scrap 一个显示器都枚举不到
# → 采集起不来（黑屏）。但会话根窗口里其实有桌面内容。
# 上游的兜底只是「换下一个 screen」，单屏时直接返回 None，没有根窗口兜底。
# 这里在「本 screen 没有 active monitor」时，用该 X screen 的像素尺寸 + 根窗口
# 合成一个 Display，让 scrap 能抓到真实桌面。
it = open(iter_path, encoding="utf-8").read()
if "NPA: RandR fallback" in it:
    print("iter.rs 已有 RandR 兜底，跳过")
else:
    patches = [
        # 3.1 结构体字段带上 X screen 尺寸
        (
            """    inner: Option<(
        xcb_randr_monitor_info_iterator_t,
        xcb_window_t,
        *mut xcb_randr_get_monitors_reply_t,
    )>,""",
            """    inner: Option<(
        xcb_randr_monitor_info_iterator_t,
        xcb_window_t,
        *mut xcb_randr_get_monitors_reply_t,
        u16, // NPA: X screen width in pixels (RandR fallback)
        u16, // NPA: X screen height in pixels (RandR fallback)
    )>,""",
        ),
        # 3.2 next_screen 返回值类型
        (
            """    ) -> Option<(
        xcb_randr_monitor_info_iterator_t,
        xcb_window_t,
        *mut xcb_randr_get_monitors_reply_t,
    )> {""",
            """    ) -> Option<(
        xcb_randr_monitor_info_iterator_t,
        xcb_window_t,
        *mut xcb_randr_get_monitors_reply_t,
        u16, // NPA
        u16, // NPA
    )> {""",
        ),
        # 3.3 取 X screen 像素尺寸
        (
            """            let root = (*outer.data).root;
""",
            """            let root = (*outer.data).root;
            // NPA: RandR fallback — remember the X screen pixel size so a screen
            // without any active monitor can still be captured (root window).
            let sw = (*outer.data).width_in_pixels;
            let sh = (*outer.data).height_in_pixels;
""",
        ),
        # 3.4 返回值带上尺寸
        (
            """            Some((inner, root, response))
""",
            """            Some((inner, root, response, sw, sh))
""",
        ),
        # 3.5 next() 的模式
        (
            """            if let Some((ref mut inner, root, _)) = self.inner {
""",
            """            if let Some((ref mut inner, root, _, _, _)) = self.inner {
""",
        ),
        # 3.6 free_reply 的模式
        (
            """        if let Some((_, _, response)) = self.inner.take() {
""",
            """        if let Some((_, _, response, _, _)) = self.inner.take() {
""",
        ),
        # 3.7 根窗口兜底
        (
            """            // The current screen was empty, so try the next screen.
            self.free_reply();
            self.inner = Self::next_screen(&mut self.outer, &self.server);
""",
            """            // NPA: RandR fallback — this screen reports no active monitor. On a
            // headless session X server every output is "disconnected", so
            // RandR 1.5 returns an empty monitor list even though the session
            // root window still holds the desktop. Capture the root window
            // using the X screen's pixel size instead of giving up.
            let fallback = match &self.inner {
                Some((_, root, _, sw, sh)) => Some((*root, *sw, *sh)),
                None => None,
            };
            if let Some((root, sw, sh)) = fallback {
                unsafe {
                    let pixfmt = get_pixfmt(self.server.raw(), root).unwrap_or(Pixfmt::BGRA);
                    let display = Display::new(
                        self.server.clone(),
                        true,
                        Rect {
                            x: 0,
                            y: 0,
                            w: sw,
                            h: sh,
                        },
                        root,
                        "screen0".to_owned(),
                        pixfmt,
                    );
                    self.free_reply();
                    self.inner = Self::next_screen(&mut self.outer, &self.server);
                    return Some(display);
                }
            }

            // The current screen was empty, so try the next screen.
            self.free_reply();
            self.inner = Self::next_screen(&mut self.outer, &self.server);
""",
        ),
    ]
    for old, new in patches:
        if old not in it:
            sys.exit("ERROR: %s 里找不到锚点（上游 scrap 结构可能变了）:\n%s" % (iter_path, old[:160]))
        it = it.replace(old, new, 1)
    open(iter_path, "w", encoding="utf-8").write(it)
    print("patched %s（RandR 根窗口兜底）" % iter_path)

# ============================================================================
# 4. 非 shm 读回兜底（无显示输出的机器上，shm 读回恒为陈旧帧）
# ============================================================================
# 背景：机器没接任何显示输出时（GPU 从不 present），X 服务器把画面复制到共享
# 内存的动作不会发生 —— 实测根窗口底色红→蓝，shm 抓图 md5 完全相同，只有非
# shm 的按需读回（xwd）能看到新内容。受影响的不只是 scrap：依赖 present/损伤
# 事件的 x11vnc 同样饿死（画面冻结）。上游 scrap 的 frame() 只比较「已返回帧」
# 与「本次 shm 读回」，在 shm 恒定时会把陈旧帧当成新帧推出去。
#
# 这里改成：
#   * 记录上一次 shm 读回的内容，区分「屏幕真没变」与「shm 陈旧」；
#   * shm 没变化时，用非 shm 的 XGetImage（xcb_get_image）读一次确认；
#   * 若非 shm 内容变了而 shm 没变 → 判定该机器 shm 陈旧，之后一直走非 shm。
# 对有显示器的桌面，快路命中，无任何额外开销。

NON_SHM_MARKER = "NPA: non-shm fallback"

# --- 4.1 ffi.rs：xcb_get_image / xcb_get_image_reply 声明与结构 ---
if "xcb_get_image" not in ffi:
    decl_anchor = "    pub fn xcb_randr_get_monitors_unchecked(\n"
    if decl_anchor not in ffi:
        sys.exit("ERROR: %s 里找不到 xcb_randr_get_monitors_unchecked 锚点" % ffi_path)
    decl = """    // NPA: non-shm fallback — XGetImage 直取像素（不经过共享内存）。
    pub fn xcb_get_image(
        c: *mut xcb_connection_t,
        drawable: xcb_drawable_t,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        plane_mask: u32,
        format: u8,
    ) -> xcb_get_image_cookie_t;

    pub fn xcb_get_image_reply(
        c: *mut xcb_connection_t,
        cookie: xcb_get_image_cookie_t,
        e: *mut *mut xcb_generic_error_t,
    ) -> *mut xcb_image_t;

"""
    ffi = ffi.replace(decl_anchor, decl + decl_anchor, 1)
    open(ffi_path, "w", encoding="utf-8").write(ffi)
    print("patched %s（xcb_get_image 声明）" % ffi_path)
else:
    print("ffi.rs 已有 xcb_get_image 声明，跳过")

if "pub struct xcb_image_t" not in ffi:
    struct_anchor = "#[repr(C)]\npub struct xcb_generic_error_t {"
    if struct_anchor not in ffi:
        sys.exit("ERROR: %s 里找不到 xcb_generic_error_t 锚点" % ffi_path)
    structs = """// NPA: non-shm fallback — xcb_get_image 的 cookie 与图像结构（xcb/xproto.h）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct xcb_get_image_cookie_t {
    pub sequence: u32,
}

#[repr(C)]
pub struct xcb_image_t {
    pub width: u16,
    pub height: u16,
    pub x: i16,
    pub y: i16,
    pub depth: u8,
    pub bpp: u8,
    pub pad: u8,
    pub data: *mut c_void,
    pub size: u32,
}

"""
    ffi = ffi.replace(struct_anchor, structs + struct_anchor, 1)
    open(ffi_path, "w", encoding="utf-8").write(ffi)
    print("patched %s（xcb_image_t 结构）" % ffi_path)
else:
    print("ffi.rs 已有 xcb_image_t 结构，跳过")

# --- 4.2 capturer.rs：字段 + 常量 + 新的 frame() ---
# 幂等标记放在 frame() 内部（字段注释在别处，先打上字段标记会让整段被跳过）。
FRAME_MARKER = "NPA: non-shm fallback — shm 内容是否变化"

NON_SHM_FRAME = """    pub fn frame<'b>(&'b mut self) -> std::io::Result<&'b [u8]> {
        // NPA: 给 shm 读回计时。没有连接任何显示输出的机器上，X server 的 shm
        // GetImage 回复要等到下一次 present 才送达 → 这里会阻塞数秒（实测 ~4s），
        // 表现为远控只有 0.2 fps、鼠标一动画面不动。读回一旦超时，就永久切到
        // 非 shm 的 XGetImage 读回（它不依赖 present，能立刻拿到当前画面）。
        let mut had_prev = false;
        let mut shm_changed = false;
        if !self.non_shm {
            let __npa_t0 = std::time::Instant::now();
            self.get_image();
            let __npa_shm_ms = __npa_t0.elapsed().as_millis() as u64;
            if __npa_shm_ms > 150 {
                // 已经切到非 shm：之后每次 frame() 都不再走 shm 读回（否则每帧
                // 都要阻塞数秒）。
                self.non_shm = true;
                eprintln!(
                    "NPA: shm 读回耗时 {}ms（疑似无 present）→ 切换非 shm 读回",
                    __npa_shm_ms
                );
            } else {
                // NPA: non-shm fallback — shm 内容是否变化，要跟「上一次 shm 读回」比，
                // 而不是跟「已返回的帧」比：无显示输出时 shm 恒为陈旧帧，跟已返回帧比
                // 会把同一张陈旧帧当成新帧推出去。
                let shm = unsafe { slice::from_raw_parts(self.buffer, self.size) };
                had_prev = !self.last_shm.is_empty();
                shm_changed = had_prev && !Self::same_bytes(shm, &self.last_shm);
                self.last_shm.clear();
                self.last_shm.extend_from_slice(shm);
            }
        }

        // 快路：shm 有新内容 → 有显示器的桌面走这里（无额外开销）。
        if shm_changed && !self.non_shm {
            let shm = unsafe { slice::from_raw_parts(self.buffer, self.size) };
            if !Self::same_bytes(shm, &self.saved_raw_data) {
                self.saved_raw_data.clear();
                self.saved_raw_data.extend_from_slice(shm);
            }
            return Ok(shm);
        }

        // shm 没变化 → 可能是「屏幕真没变」，也可能是 shm 陈旧（无显示输出）。
        // 用非 shm 的 XGetImage 再读一次确认。
        let probe_due = match self.last_probe {
            None => true,
            Some(t) => t.elapsed().as_millis() as u64 >= Self::NON_SHM_PROBE_INTERVAL_MS,
        };
        if self.non_shm || probe_due {
            let rect = self.display.rect();
            let server = self.display.server().raw();
            let root = self.display.root();
            let size = self.size;
            if Self::fetch_image_non_shm(
                server,
                root,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                size,
                &mut self.non_shm_buf,
            ) {
                let fresh_changed = !Self::same_bytes(&self.non_shm_buf, &self.saved_raw_data);
                if fresh_changed {
                    // 屏幕变了，但 shm 一直没变 → 这台机器的 shm 读回是陈旧帧。
                    if had_prev && !shm_changed {
                        self.stale_hits += 1;
                        if self.stale_hits >= 2 {
                            self.non_shm = true;
                        }
                        eprintln!(
                            "NPA: shm 陈旧确认（第 {} 次），之后固定走非 shm 读回",
                            self.stale_hits
                        );
                    } else {
                        eprintln!("NPA: 非 shm 读回拿到新画面（shm 未变）");
                    }
                    self.last_probe = Some(std::time::Instant::now());
                    self.saved_raw_data.clear();
                    self.saved_raw_data.extend_from_slice(&self.non_shm_buf);
                    return Ok(&self.non_shm_buf);
                }
                self.last_probe = Some(std::time::Instant::now());
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
        }

        // 非 shm 读回不可用 → 退回原有 shm 语义。
        let shm = unsafe { slice::from_raw_parts(self.buffer, self.size) };
        if !Self::same_bytes(shm, &self.saved_raw_data) {
            self.saved_raw_data.clear();
            self.saved_raw_data.extend_from_slice(shm);
            return Ok(shm);
        }
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
"""

NON_SHM_HELPERS = """    fn same_bytes(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && (a.is_empty() || a == b)
    }

    /// NPA: non-shm fallback — 通过 X 协议读回整屏像素，写入 out。
    ///
    /// 做成关联函数（不借用 self）：frame() 里同时持有 self 的字段借用
    /// （non_shm_buf / saved_raw_data），整块 &mut self 借用会与它们冲突。
    #[allow(clippy::too_many_arguments)]
    fn fetch_image_non_shm(
        server: *mut xcb_connection_t,
        root: xcb_drawable_t,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        size: usize,
        out: &mut Vec<u8>,
    ) -> bool {
        unsafe {
            let cookie = xcb_get_image(server, root, x, y, w, h, !0, Self::FORMAT_Z_PIXMAP);
            let mut err: *mut xcb_generic_error_t = ptr::null_mut();
            let img = xcb_get_image_reply(server, cookie, &mut err);
            if img.is_null() {
                eprintln!(
                    "NPA: non-shm XGetImage 无回复 (xcb error code {})",
                    if err.is_null() { 0 } else { (*err).error_code as i32 }
                );
                if !err.is_null() {
                    libc::free(err as *mut libc::c_void);
                }
                return false;
            }
            let image = &*img;
            // xcb_image_t::bpp 是「每像素位数」（24 位深通常返回 32），
            // 而 size 是字节数 → 期望 bpp = size * 8 / (w * h)。
            let expect_bpp = if w as usize * h as usize > 0 {
                ((size * 8) / (w as usize * h as usize)) as u8
            } else {
                0
            };
            let ok = !image.data.is_null()
                && image.width as usize == w as usize
                && image.height as usize == h as usize
                && image.bpp == expect_bpp
                && image.size as usize >= size;
            if !ok {
                eprintln!(
                    "NPA: non-shm XGetImage 参数不符：reply w={} h={} bpp={} size={}，期望 w={} h={} bpp={} size={}",
                    image.width, image.height, image.bpp, image.size, w, h, expect_bpp, size
                );
            }
            if ok {
                // shm 段是只读挂载的，不能往里写 → 独立缓冲区存非 shm 帧。
                out.clear();
                out.reserve(size);
                out.extend_from_slice(slice::from_raw_parts(image.data as *const u8, size));
            }
            if !image.data.is_null() {
                libc::free(image.data as *mut libc::c_void);
            }
            libc::free(img as *mut libc::c_void);
            if !err.is_null() {
                libc::free(err as *mut libc::c_void);
            }
            ok
        }

    }"""


cap = open(cap_path, encoding="utf-8").read()
if "last_shm: Vec<u8>," not in cap:
    field_anchor = "    saved_raw_data: Vec<u8>, // for faster compare and copy\n"
    if field_anchor not in cap:
        sys.exit("ERROR: %s 里找不到 saved_raw_data 锚点" % cap_path)
    fields = field_anchor + """
    // NPA: non-shm fallback —— 记录上一次 shm 读回的内容，用来区分
    // 「屏幕真的没变」和「shm 陈旧（无显示输出）」两种情况。
    last_shm: Vec<u8>,
    stale_hits: u32,   // 连续多少次「shm 没变但非 shm 变了」
    non_shm: bool,     // 判定该机器 shm 陈旧 → 之后一直走非 shm 读回
    last_probe: Option<std::time::Instant>,
    non_shm_buf: Vec<u8>, // 非 shm 读回的帧（shm 段只读挂载，不能往里写）
"""
    cap = cap.replace(field_anchor, fields, 1)

if "last_shm: Vec::new()," not in cap:
    init_anchor = "            saved_raw_data: Vec::new(),\n"
    if init_anchor not in cap:
        sys.exit("ERROR: %s 里找不到 saved_raw_data 初始化锚点" % cap_path)
    cap = cap.replace(
        init_anchor,
        init_anchor
        + "            last_shm: Vec::new(),\n"
        + "            stale_hits: 0,\n"
        + "            non_shm: false,\n"
        + "            last_probe: None,\n"
        + "            non_shm_buf: Vec::new(),\n",
        1,
    )
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("patched %s（非 shm 字段初始化）" % cap_path)

if "const FORMAT_Z_PIXMAP: u8 = 2;" not in cap:
    const_anchor = "impl Capturer {\n"
    if const_anchor not in cap:
        sys.exit("ERROR: %s 里找不到 impl Capturer 锚点" % cap_path)
    consts = const_anchor + """    // NPA: non-shm fallback — xcb_get_image 的标准 ZPixmap 格式值
    // （xcb/xproto.h: XCB_IMAGE_FORMAT_Z_PIXMAP = 2；XYBitmap=0、XYPixmap=1、ZPixmap=2）。
    const FORMAT_Z_PIXMAP: u8 = 2;
    // NPA: 非 shm 探测的最小间隔（ms）——静态桌面时不必每帧都做一次昂贵的
    // 协议读回；1000ms 一次 ≈ 每秒 40ms 额外开销。
    const NON_SHM_PROBE_INTERVAL_MS: u64 = 1000;

"""
    cap = cap.replace(const_anchor, consts, 1)
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("patched %s（非 shm 常量）" % cap_path)

if FRAME_MARKER not in cap:
    # 替换整个 frame()（按大括号配平定位函数末尾）
    frame_header = "    pub fn frame<'b>(&'b mut self) -> std::io::Result<&'b [u8]> {"
    if frame_header not in cap:
        sys.exit("ERROR: %s 里找不到 frame() 锚点" % cap_path)
    start = cap.index(frame_header)
    depth = 0
    end = None
    for j in range(start, len(cap)):
        if cap[j] == "{":
            depth += 1
        elif cap[j] == "}":
            depth -= 1
            if depth == 0:
                end = j
                break
    if end is None:
        sys.exit("ERROR: %s 里 frame() 的大括号没配平" % cap_path)
    cap = cap[:start] + NON_SHM_FRAME + NON_SHM_HELPERS + cap[end + 1:]
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("patched %s（非 shm 读回兜底）" % cap_path)
else:
    print("capturer.rs 已有非 shm 兜底，跳过")

# 兜底：frame 已打过补丁但辅助函数缺失时，单独补上辅助函数。
if "fn same_bytes" not in cap:
    # frame 已打过补丁但辅助函数缺失 → 插到 frame() 之后。
    frame_header = "    pub fn frame<'b>(&'b mut self) -> std::io::Result<&'b [u8]> {"
    if frame_header not in cap:
        sys.exit("ERROR: %s 里找不到 frame() 锚点（无法插入辅助函数）" % cap_path)
    fstart = cap.index(frame_header)
    fdepth = 0
    fend = None
    for j in range(fstart, len(cap)):
        if cap[j] == "{":
            fdepth += 1
        elif cap[j] == "}":
            fdepth -= 1
            if fdepth == 0:
                fend = j
                break
    if fend is None:
        sys.exit("ERROR: %s 里 frame() 的大括号没配平（无法插入辅助函数）" % cap_path)
    cap = cap[:fend + 1] + "\n" + NON_SHM_HELPERS + cap[fend + 1:]
    open(cap_path, "w", encoding="utf-8").write(cap)
    print("patched %s（非 shm 辅助函数）" % cap_path)
else:
    print("capturer.rs 已有非 shm 辅助函数，跳过")