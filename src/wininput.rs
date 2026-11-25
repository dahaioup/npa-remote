// Windows 物理键盘注入（SendInput + 扫描码）。
//
// 为什么不用 enigo：enigo 的 `Key::Unicode(c)` 走的是 KEYEVENTF_UNICODE，
// Windows 的 TSF 输入法框架（微软拼音/搜狗等）只处理**物理按键**，
// unicode 注入不会进候选窗 —— 表现就是「中文输入法开着，打出来还是字母」。
//
// RustDesk 的做法：用 SendInput 注入虚拟键码（带 KEYEVENTF_SCANCODE），
// 让 IME 看到真实的按键序列，从而正常组词。

use anyhow::{bail, Result};

// ---- user32.dll ----
#[link(name = "user32")]
extern "system" {
    fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
    fn keybd_event(b_vk: u8, b_scan: u8, dw_flags: u32, dw_extra_info: usize);
    #[allow(dead_code)]
    fn MapVirtualKeyW(u_code: u32, u_map_type: u32) -> u16;
    fn GetSystemMetrics(n_index: i32) -> i32;
    fn GetCursorPos(lp_point: *mut CursorPoint) -> i32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn GetWindowTextW(hwnd: isize, lp_string: *mut u16, n_max_count: i32) -> i32;
    // 桌面切换：登录界面（Winlogon 桌面）不属于当前用户桌面，SendInput 打不到。
    fn GetForegroundWindow() -> isize;
    fn GetWindowThreadProcessId(hwnd: isize, lpdwprocessid: *mut u32) -> u32;
    fn GetThreadDesktop(thread_id: u32) -> isize;
    fn SetThreadDesktop(h_desktop: isize) -> i32;
    fn OpenDesktopW(lpszdesktop: *const u16, dwflags: u32, dwdesiredaccess: u32) -> isize;
    fn CloseDesktop(h_desktop: isize) -> i32;
    // 输入队列：PeekMessage(PM_NOREMOVE) 会强制当前线程创建消息队列。
    fn PeekMessageW(lp_msg: *mut Msg, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
}

/// Windows MSG（只用 PeekMessage 的缓冲区，字段顺序与 x64 布局一致）。
#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
    l_private: u32,
}

const PM_NOREMOVE: u32 = 0x0000;
const PM_REMOVE: u32 = 0x0001;

/// Windows POINT（GetCursorPos 用）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CursorPoint {
    pub x: i32,
    pub y: i32,
}

/// 当前鼠标位置（屏幕像素）。
pub fn cursor_pos() -> Result<(i32, i32)> {
    let mut p = CursorPoint::default();
    let ok = unsafe { GetCursorPos(&mut p) };
    if ok == 0 {
        bail!("GetCursorPos failed: {}", std::io::Error::last_os_error());
    }
    Ok((p.x, p.y))
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcessId() -> u32;
    fn ProcessIdToSessionId(process_id: u32, session_id: *mut u32) -> i32;
    fn GetProcessWindowStation() -> isize;
    fn GetUserObjectInformationW(
        h_obj: isize,
        index: u32,
        pv_info: *mut u16,
        n_length: u32,
        lpdw_length_needed: *mut u32,
    ) -> i32;
}

const DESKTOP_READWRITE_OBJECTS: u32 = 0x0001;
const DESKTOP_SWITCHDESKTOP: u32 = 0x0100;

const INPUT_MOUSE: u32 = 0;
const INPUT_KEYBOARD: u32 = 1;

#[allow(dead_code)]
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_UNICODE: u32 = 0x0004;

const MOUSEEVENTF_MOVE: u32 = 0x0001;
const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;

#[allow(dead_code)]
const MAPVK_VK_TO_VSC: u32 = 0;
const UOI_NAME: u32 = 2;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

/// 需要 EXTENDEDKEY 标记的键（方向键、Home/End、小键盘右侧等）。
#[allow(dead_code)]
fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x2E // Insert/Delete/Home/End/PageUp/PageDown/方向键
            | 0x5B | 0x5C // Win
            | 0x5D // Menu
            | 0xA3 | 0xA5 // 右 Ctrl / 右 Alt
            | 0x6F..=0x87 // 小键盘
    )
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardInput {
    w_vk: u16,
    w_scan: u16,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    dw_flags: u32,
    time: u32,
    dw_extra_info: usize,
}

// 注意：必须是真正的 union，不能用 struct 模拟。
// C 的 INPUT（x64）= DWORD type(4) + padding(4) + union(32) = 40 字节；
// 若用 struct{ki,mi} 并排会变成 64 字节，SendInput 校验 cbSize != sizeof(INPUT)
// 直接返回 ERROR_INVALID_PARAMETER(87)，一个事件都注不进去。
#[repr(C)]
#[derive(Clone, Copy)]
union InputUnion {
    ki: KeyboardInput,
    mi: MouseInput,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    r#type: u32,
    u: InputUnion,
}

// 编译期断言：INPUT 必须是 40 字节（x64）/ 28 字节（x86）。
const _: () = assert!(
    std::mem::size_of::<Input>() == if cfg!(target_pointer_width = "64") { 40 } else { 28 },
    "INPUT struct layout is wrong: SendInput will reject it with ERROR_INVALID_PARAMETER"
);

impl Input {
    fn keyboard(vk: u16, scan: u16, flags: u32) -> Self {
        Self {
            r#type: INPUT_KEYBOARD,
            u: InputUnion {
                ki: KeyboardInput {
                    w_vk: vk,
                    w_scan: scan,
                    dw_flags: flags,
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        }
    }

    fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> Self {
        Self {
            r#type: INPUT_MOUSE,
            u: InputUnion {
                mi: MouseInput {
                    dx,
                    dy,
                    mouse_data: data,
                    dw_flags: flags,
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        }
    }
}

/// 是否允许切换到前台窗口所在桌面（默认关闭）。
///
/// SetThreadDesktop 不会重挂线程输入队列，切换后 SendInput 会失败；
/// 只有在明确知道目标桌面（登录界面 / Winlogon）时才需要切。
fn desktop_switch_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("NPA_ATTACH_FOREGROUND_DESKTOP")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn send(inputs: &[Input]) -> Result<()> {
    // 登录界面（Winlogon 桌面）在别的 desktop 上，SendInput 只作用于当前线程的桌面，
    // 所以先切到前台窗口所在的桌面，注入完再切回来。
    //
    // 但 SetThreadDesktop 不会重挂线程的输入队列，切过去之后 SendInput 反而会失败
    // （SendInput sent 0/1 events）。所以默认**不切**，只有显式设置
    // NPA_ATTACH_FOREGROUND_DESKTOP=1 时才启用（需要进程有 DESKTOP_SWITCHDESKTOP 权限）。
    let _guard = if desktop_switch_enabled() {
        DesktopGuard::enter_foreground_desktop()
    } else {
        DesktopGuard::none()
    };
    let n = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<Input>() as i32,
        )
    };
    if n as usize != inputs.len() {
        bail!(
            "SendInput sent {n}/{} events (winerr={} {})",
            inputs.len(),
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            desktop_context()
        );
    }
    Ok(())
}

/// 桌面切换守卫：注入期间临时切到前台窗口所在桌面（登录界面用），离开时切回。
pub struct DesktopGuard {
    prev: Option<isize>,
    opened: Option<isize>,
}

impl DesktopGuard {
    /// 不做任何桌面切换（默认行为）。
    pub fn none() -> Self {
        DesktopGuard {
            prev: None,
            opened: None,
        }
    }

    /// 若前台窗口位于别的桌面则切过去；失败（无 DESKTOP_SWITCHDESKTOP 权限）则保持原桌面。
    pub fn enter_foreground_desktop() -> Self {
        let mut guard = DesktopGuard {
            prev: None,
            opened: None,
        };
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd == 0 {
                return guard;
            }
            let tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
            let target = GetThreadDesktop(tid);
            let current = GetThreadDesktop(GetCurrentThreadId());
            if target == 0 || current == 0 || target == current {
                return guard;
            }
            if SetThreadDesktop(target) == 0 {
                return guard;
            }
            guard.prev = Some(current);
            guard
        }
    }
}

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev {
            unsafe {
                SetThreadDesktop(prev);
            }
        }
        if let Some(h) = self.opened {
            unsafe {
                CloseDesktop(h);
            }
        }
    }
}

/// 打开 Winlogon 桌面（Windows 登录/锁屏界面所在的桌面）。
///
/// 只有以 SYSTEM 身份运行的服务进程才有权限切换过去 —— 因此操作开机登录界面
/// 要求 NPA 以 Windows 服务方式运行（session 0），这也是 Synergy/RustDesk 的做法。
pub fn open_logon_desktop() -> Result<isize> {
    let name: Vec<u16> = "Winlogon".encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe {
        OpenDesktopW(
            name.as_ptr(),
            0,
            DESKTOP_READWRITE_OBJECTS | DESKTOP_SWITCHDESKTOP,
        )
    };
    if h == 0 {
        bail!(
            "OpenDesktopW(Winlogon) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(h)
}

// ---------- 键盘注入线程 ----------
//
// 实测（158 / Windows 11，焦点在 WinForms 文本框）：
//   - 主线程（WinForms 线程，自带输入队列）里 keybd_event → 字符正常落盘；
//   - 派生线程（没有输入队列）里 keybd_event → 调用挂起（探针 PHASE_B 3 秒未返回）。
// 插件的 WS 处理跑在 tokio worker 线程上（没有输入队列），键盘注入因此全部失效
// （鼠标走 SendInput 不受此限，所以症状是「鼠标能动、键盘不能」）。
//
// 修法：键盘注入搬到专用线程——线程启动时用 PeekMessage(PM_NOREMOVE) 强制附加输入
// 队列，并周期泵消息保持队列存活；按键请求经通道发送，保证按下/抬起顺序不乱。

enum KeyCmd {
    Vk { vk: u16, down: bool },
    Unicode(char),
}

struct Injector {
    tx: std::sync::mpsc::Sender<(KeyCmd, std::sync::mpsc::Sender<Result<()>>)>,
}

fn injector() -> &'static Injector {
    static I: std::sync::OnceLock<Injector> = std::sync::OnceLock::new();
    I.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(KeyCmd, std::sync::mpsc::Sender<Result<()>>)>();
        std::thread::Builder::new()
            .name("npa-key-injector".into())
            .spawn(move || injector_loop(rx))
            .expect("spawn key injector thread");
        Injector { tx }
    })
}

fn injector_loop(rx: std::sync::mpsc::Receiver<(KeyCmd, std::sync::mpsc::Sender<Result<()>>)>) {
    attach_input_queue();
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok((cmd, reply)) => {
                let r = match cmd {
                    KeyCmd::Vk { vk, down } => raw_keybd_event(vk, down),
                    KeyCmd::Unicode(ch) => raw_send_unicode(ch),
                };
                let _ = reply.send(r);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => drain_messages(),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// 强制当前线程创建消息队列（没有输入队列时 keybd_event 会挂起）。
fn attach_input_queue() {
    unsafe {
        let mut msg = Msg::default();
        PeekMessageW(&mut msg as *mut Msg, 0, 0, 0, PM_NOREMOVE);
    }
}

/// 泵掉队列里的消息，保持输入队列存活。
fn drain_messages() {
    unsafe {
        let mut msg = Msg::default();
        while PeekMessageW(&mut msg as *mut Msg, 0, 0, 0, PM_REMOVE) != 0 {
            msg = Msg::default();
        }
    }
}

fn injector_send(cmd: KeyCmd) -> Result<()> {
    let (rtx, rrx) = std::sync::mpsc::channel();
    injector()
        .tx
        .send((cmd, rtx))
        .map_err(|_| anyhow::anyhow!("key injector thread is not running"))?;
    rrx.recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| anyhow::anyhow!("key injector timed out"))?
}

/// 按下/抬起一个虚拟键（走 keybd_event 传统驱动路径，IME 能正确组词）。
pub fn send_vk(vk: u16, down: bool) -> Result<()> {
    injector_send(KeyCmd::Vk { vk, down })
}

fn raw_keybd_event(vk: u16, down: bool) -> Result<()> {
    // 实测（158 / Windows 11，焦点在 WinForms 文本框）：
    //   - SendInput(虚拟键) 返回成功，但目标窗口只收到 WM_KEYDOWN、没有 WM_CHAR
    //     → 字符不落盘，输入法也组不出词；
    //   - keybd_event 走传统键盘驱动路径，字符与输入法都正常。
    // 所以键盘统一用 keybd_event；SendInput 只保留给鼠标（MOUSEEVENTF_*）。
    //
    // 注意 bScan 必须传 0：探针对照实验（158，焦点在 WinForms 文本框）
    //   keybd_event(vk, 0,    0) → text=[c]   字符正常落盘
    //   keybd_event(vk, 0x23, 0) → 只有 WM_KEYDOWN，无 WM_CHAR，字符不落盘
    // 传非零扫描码会让 keybd_event 走扫描码注入路径，TranslateMessage 不再生成字符。
    let scan = 0u8;
    let flags = if down { 0 } else { KEYEVENTF_KEYUP };
    unsafe { keybd_event(vk as u8, scan, flags, 0) };
    Ok(())
}

/// 可能接管键盘/鼠标输入的进程（共享键鼠类软件）。
///
/// 158 上 Synergy 服务端在跑时会把本机输入全部转发给客户端（Synergy 的低级钩子），
/// 此时 SendInput/keybd_event 注入的事件被吞掉——远程控制表现为「画面正常、点不动」。
/// 这类进程只能由用户侧退出或停服务，插件只能识别并提示。
const INPUT_GRABBER_PATTERNS: &[&str] = &[
    "synergy",
    "barrier",
    "input_leap",
    "awesun",
    "sunlogin",
    "todesk",
    "parsec",
    "anydesk",
    "rustdesk",
];

/// 列出正在运行的可能接管输入的进程（名称 + PID）。
pub fn running_input_grabbers() -> Vec<String> {
    let out = match std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let mut hits: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut cols = line.split(',');
        let name = cols.next().unwrap_or("").trim().trim_matches('"');
        let pid = cols.next().unwrap_or("").trim().trim_matches('"');
        if name.is_empty() {
            continue;
        }
        if INPUT_GRABBER_PATTERNS.iter().any(|p| name.contains(p)) {
            hits.push(format!("{name} (pid {pid})"));
        }
    }
    hits
}

/// 组合键：按下顺序 → 逆序抬起。
pub fn send_combo(vks: &[u16]) -> Result<()> {
    for vk in vks {
        send_vk(*vk, true)?;
    }
    for vk in vks.iter().rev() {
        send_vk(*vk, false)?;
    }
    Ok(())
}

/// 直接注入 Unicode（不经过 IME，用于输入中文等非 ASCII 文本）。
pub fn send_unicode(ch: char) -> Result<()> {
    injector_send(KeyCmd::Unicode(ch))
}

fn raw_send_unicode(ch: char) -> Result<()> {
    let mut buf = [0u16; 2];
    for unit in ch.encode_utf16(&mut buf) {
        send(&[
            Input::keyboard(0, *unit, KEYEVENTF_UNICODE),
            Input::keyboard(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ])?;
    }
    Ok(())
}

/// 浏览器 KeyboardEvent.code → Windows 虚拟键码。
pub fn vk_from_browser_code(code: &str) -> Option<u16> {
    if let Some(rest) = code.strip_prefix("Key") {
        if rest.len() == 1 {
            let c = rest.chars().next().unwrap().to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(c as u16);
            }
        }
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if rest.len() == 1 {
            if let Some(d) = rest.chars().next().and_then(|c| c.to_digit(10)) {
                return Some(0x30 + d as u16);
            }
        }
    }
    Some(match code {
        "Space" => 0x20,
        "Enter" | "NumpadEnter" => 0x0D,
        "Tab" => 0x09,
        "Escape" => 0x1B,
        "Backspace" => 0x08,
        "Delete" => 0x2E,
        "Insert" => 0x2D,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "Backquote" => 0xC0,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "ContextMenu" => 0x5D,
        "ControlLeft" => 0xA2,
        "ControlRight" => 0xA3,
        "ShiftLeft" => 0xA0,
        "ShiftRight" => 0xA1,
        "AltLeft" => 0xA4,
        "AltRight" => 0xA5,
        "MetaLeft" => 0x5B,
        "MetaRight" => 0x5C,
        "CapsLock" => 0x14,
        "NumLock" => 0x90,
        "ScrollLock" => 0x91,
        "PrintScreen" => 0x2C,
        "Pause" => 0x13,
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        _ => return None,
    })
}

/// 组合键名 → 虚拟键码（"ctrl" / "shift" / "alt" / "win" / 单字符）。
pub fn vk_from_name(name: &str) -> Option<u16> {
    let low = name.trim().to_ascii_lowercase();
    match low.as_str() {
        "ctrl" | "control" => return Some(0x11),
        "shift" => return Some(0x10),
        "alt" | "option" => return Some(0x12),
        "win" | "cmd" | "meta" | "super" | "command" => return Some(0x5B),
        "space" => return Some(0x20),
        "enter" | "return" => return Some(0x0D),
        "tab" => return Some(0x09),
        "esc" | "escape" => return Some(0x1B),
        "backspace" => return Some(0x08),
        "delete" | "del" => return Some(0x2E),
        "insert" | "ins" => return Some(0x2D),
        "home" => return Some(0x24),
        "end" => return Some(0x23),
        "pageup" | "pgup" => return Some(0x21),
        "pagedown" | "pgdn" => return Some(0x22),
        "up" | "arrowup" => return Some(0x26),
        "down" | "arrowdown" => return Some(0x28),
        "left" | "arrowleft" => return Some(0x25),
        "right" | "arrowright" => return Some(0x27),
        _ => {}
    }
    if low.len() == 1 {
        let c = low.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    None
}

/// 鼠标绝对移动（屏幕像素）。
pub fn mouse_move_abs(x: i32, y: i32) -> Result<()> {
    let w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1);
    let h = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1);
    let nx = (x.clamp(0, w - 1) as i64 * 65535 / (w - 1).max(1) as i64) as i32;
    let ny = (y.clamp(0, h - 1) as i64 * 65535 / (h - 1).max(1) as i64) as i32;
    send(&[Input::mouse(
        nx,
        ny,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )])
}

/// 鼠标键：down / up。
pub fn mouse_button(button: &str, down: bool) -> Result<()> {
    let flags = match button.to_ascii_lowercase().as_str() {
        "left" | "" => {
            if down {
                MOUSEEVENTF_LEFTDOWN
            } else {
                MOUSEEVENTF_LEFTUP
            }
        }
        "right" => {
            if down {
                MOUSEEVENTF_RIGHTDOWN
            } else {
                MOUSEEVENTF_RIGHTUP
            }
        }
        "middle" => {
            if down {
                MOUSEEVENTF_MIDDLEDOWN
            } else {
                MOUSEEVENTF_MIDDLEUP
            }
        }
        other => bail!("unknown button: {other}"),
    };
    send(&[Input::mouse(0, 0, 0, flags)])
}

/// 滚轮（正值向上，与现有约定一致）。
pub fn mouse_wheel(delta: i32) -> Result<()> {
    send(&[Input::mouse(0, 0, delta as u32, MOUSEEVENTF_WHEEL)])
}


/// 当前进程的会话与桌面信息（诊断用）：窗口站名 / 桌面名 / 会话 ID。
pub fn desktop_context() -> String {
    let mut session: u32 = 0;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
    }
    let mut winsta = String::new();
    let mut desktop = String::new();
    unsafe {
        let ws = GetProcessWindowStation();
        if ws != 0 {
            let mut need: u32 = 0;
            let mut buf = [0u16; 256];
            if GetUserObjectInformationW(ws, UOI_NAME, buf.as_mut_ptr(), 512, &mut need) != 0 {
                winsta = wide_to_string(&buf);
            }
            let cur = GetThreadDesktop(GetCurrentThreadId());
            if cur != 0 {
                let mut need2: u32 = 0;
                let mut buf2 = [0u16; 256];
                if GetUserObjectInformationW(cur, UOI_NAME, buf2.as_mut_ptr(), 512, &mut need2) != 0 {
                    desktop = wide_to_string(&buf2);
                }
            }
        }
    }
    format!("session={session} winsta={winsta} desktop={desktop}")
}

fn wide_to_string(buf: &[u16]) -> String {
    let s: String = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| char::from_u32(*c as u32).unwrap_or('?'))
        .collect();
    s
}
