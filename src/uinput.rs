// Linux uinput 虚拟输入设备。
//
// 背景：Wayland 会话（KWin / Mutter）下，X11 XTEST 注入的假事件**不会**进入
// 合成器的输入管线 —— Xwayland 收到 XTEST 事件后只更新自己的指针状态，
// 合成器完全无视，于是表现为「画面能看到、鼠标点不动」。
//
// uinput 是内核级虚拟输入设备：合成器通过 libinput 把它当真实硬件读，
// Wayland 与 X11 会话下都能用（本机 157 跑的是 KWin Wayland，靠这条才点得动）。
//
// 限制：uinput 的鼠标只有相对量（REL_X/REL_Y），没有绝对定位，
// 所以这里自己维护「当前光标位置」，用增量方式把指针推到目标坐标。

use anyhow::{bail, Context, Result};
use std::os::unix::io::RawFd;
use std::sync::Mutex;

// ---- linux/uinput.h 的 ioctl 号 ----
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564; // _IOW('U', 100, int)
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565; // _IOW('U', 101, int)
const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566; // _IOW('U', 102, int)
const UI_SET_PHYS: libc::c_ulong = 0x4008_5508; // _IOW('U', 108, char*)
const UI_ABS_SETUP: libc::c_ulong = 0x4004_5540; // _IOW('U', 0x40 + abs_code, int) —— ABS_X=0
const UI_SET_ABSMIN: libc::c_ulong = 0x4004_5523; // _IOW('U', 0x23, int)
const UI_SET_ABSMAX: libc::c_ulong = 0x4004_5524; // _IOW('U', 0x24, int)
const UI_DEV_SETUP: libc::c_ulong = 0x405C_5503; // _IOW('U', 3, struct uinput_setup) size=92

// 事件类型
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

/// 绝对坐标轴的取值范围（与 ydotool 一致）。
const ABS_MAX: i32 = 32767;

const SYN_REPORT: i32 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

/// 键盘键码（linux/input-event-codes.h 常用子集）。
/// 注意 evdev 的键码不是 ASCII，字母/数字都是跳号的。
pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_F1: u16 = 59;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_MENU: u16 = 127;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;

/// 浏览器 KeyboardEvent.code → evdev 键码。
pub fn keycode_from_browser(code: &str) -> Option<u16> {
    let c = code.strip_prefix("Key").and_then(|s| s.chars().next());
    if let Some(ch) = c {
        if code.len() == 4 {
            let up = ch.to_ascii_uppercase();
            if up.is_ascii_uppercase() {
                // A..Z 在 evdev 里是跳号的，手写映射
                return Some(match up {
                    'A' => KEY_A,
                    'B' => KEY_B,
                    'C' => KEY_C,
                    'D' => KEY_D,
                    'E' => KEY_E,
                    'F' => KEY_F,
                    'G' => KEY_G,
                    'H' => KEY_H,
                    'I' => KEY_I,
                    'J' => KEY_J,
                    'K' => KEY_K,
                    'L' => KEY_L,
                    'M' => KEY_M,
                    'N' => KEY_N,
                    'O' => KEY_O,
                    'P' => KEY_P,
                    'Q' => KEY_Q,
                    'R' => KEY_R,
                    'S' => KEY_S,
                    'T' => KEY_T,
                    'U' => KEY_U,
                    'V' => KEY_V,
                    'W' => KEY_W,
                    'X' => KEY_X,
                    'Y' => KEY_Y,
                    'Z' => KEY_Z,
                    _ => 0,
                });
            }
        }
    }
    Some(match code {
        "Digit0" => KEY_0,
        "Digit1" => KEY_1,
        "Digit2" => KEY_2,
        "Digit3" => KEY_3,
        "Digit4" => KEY_4,
        "Digit5" => KEY_5,
        "Digit6" => KEY_6,
        "Digit7" => KEY_7,
        "Digit8" => KEY_8,
        "Digit9" => KEY_9,
        "Minus" => KEY_MINUS,
        "Equal" => KEY_EQUAL,
        "Backspace" => KEY_BACKSPACE,
        "Tab" => KEY_TAB,
        "Enter" => KEY_ENTER,
        "Escape" => KEY_ESC,
        "Space" => KEY_SPACE,
        "ArrowLeft" => KEY_LEFT,
        "ArrowRight" => KEY_RIGHT,
        "ArrowUp" => KEY_UP,
        "ArrowDown" => KEY_DOWN,
        "Home" => KEY_HOME,
        "End" => KEY_END,
        "PageUp" => KEY_PAGEUP,
        "PageDown" => KEY_PAGEDOWN,
        "Insert" => KEY_INSERT,
        "Delete" => KEY_DELETE,
        "ControlLeft" => KEY_LEFTCTRL,
        "ControlRight" => KEY_RIGHTCTRL,
        "ShiftLeft" => KEY_LEFTSHIFT,
        "ShiftRight" => KEY_RIGHTSHIFT,
        "AltLeft" => KEY_LEFTALT,
        "AltRight" => KEY_RIGHTALT,
        "MetaLeft" => KEY_LEFTMETA,
        "MetaRight" => KEY_RIGHTMETA,
        "ContextMenu" => KEY_MENU,
        "F1" => KEY_F1,
        "F2" => KEY_F2,
        "F3" => KEY_F3,
        "F4" => KEY_F4,
        "F5" => KEY_F5,
        "F6" => KEY_F6,
        "F7" => KEY_F7,
        "F8" => KEY_F8,
        "F9" => KEY_F9,
        "F10" => KEY_F10,
        "F11" => KEY_F11,
        "F12" => KEY_F12,
        "BracketLeft" => KEY_LEFTBRACE,
        "BracketRight" => KEY_RIGHTBRACE,
        "Semicolon" => KEY_SEMICOLON,
        "Quote" => KEY_APOSTROPHE,
        "Backquote" => KEY_GRAVE,
        "Backslash" => KEY_BACKSLASH,
        "Comma" => KEY_COMMA,
        "Period" => KEY_DOT,
        "Slash" => KEY_SLASH,
        _ => return None,
    })
}

/// 组合键里的修饰键名 → evdev 键码。
pub fn modifier_code(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "controlleft" => KEY_LEFTCTRL,
        "shift" | "shiftleft" => KEY_LEFTSHIFT,
        "alt" | "option" | "altleft" => KEY_LEFTALT,
        "cmd" | "meta" | "win" | "super" | "metaleft" => KEY_LEFTMETA,
        _ => return None,
    })
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UInputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UInputSetup {
    id: UInputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

impl Default for UInputSetup {
    fn default() -> Self {
        Self {
            id: UInputId::default(),
            name: [0; 80],
            ff_effects_max: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

struct Device {
    fd: RawFd,
    /// 我们自己维护的光标位置（uinput 只有相对量）
    pos: Option<(i64, i64)>,
}

impl Drop for Device {
    fn drop(&mut self) {
        // 退出时销毁虚拟设备，别在 /dev/input 里留个幽灵设备
        unsafe {
            libc::ioctl(self.fd, UI_DEV_DESTROY, 0);
            libc::close(self.fd);
        }
    }
}

static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

fn ioctl_set(fd: RawFd, req: libc::c_ulong, arg: libc::c_ulong) -> Result<()> {
    let rc = unsafe { libc::ioctl(fd, req, arg) };
    if rc < 0 {
        bail!(
            "uinput ioctl {req:#x} failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// 创建一个带鼠标 + 键盘能力的虚拟输入设备。
fn create_device() -> Result<Device> {
    let path = b"/dev/uinput\0";
    let fd = unsafe {
        libc::open(
            path.as_ptr() as *const libc::c_char,
            libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        bail!(
            "open /dev/uinput failed: {}",
            std::io::Error::last_os_error()
        );
    }

    // 事件类型
    for ev in [EV_SYN, EV_KEY, EV_REL, EV_ABS] {
        ioctl_set(fd, UI_SET_EVBIT, ev as libc::c_ulong)?;
    }
    // 绝对坐标轴：绝对定位才能绕开 Wayland 合成器的指针加速（相对量会被缩放，
    // 远程光标就会飘）。只注册 ABS_X/ABS_Y 而不注册 REL_X/REL_Y，
    // libinput 才会把它当「绝对指针设备」而不是触摸板。
    for (code, name) in [(ABS_X, "ABS_X"), (ABS_Y, "ABS_Y")] {
        ioctl_set(fd, UI_ABS_SETUP + code as libc::c_ulong, code as libc::c_ulong)?;
        ioctl_set(fd, UI_SET_ABSMIN, 0)?;
        ioctl_set(fd, UI_SET_ABSMAX, ABS_MAX as libc::c_ulong)?;
        let _ = name;
    }
    // 鼠标键
    for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
        ioctl_set(fd, UI_SET_KEYBIT, b as libc::c_ulong)?;
    }
    // 相对轴
    for r in [REL_WHEEL] {
        ioctl_set(fd, UI_SET_RELBIT, r as libc::c_ulong)?;
    }
    // 键盘键码（常用子集即可，用到哪个注册哪个）
    for k in [
        KEY_ESC,
        KEY_1,
        KEY_2,
        KEY_3,
        KEY_4,
        KEY_5,
        KEY_6,
        KEY_7,
        KEY_8,
        KEY_9,
        KEY_0,
        KEY_MINUS,
        KEY_EQUAL,
        KEY_BACKSPACE,
        KEY_TAB,
        KEY_Q,
        KEY_W,
        KEY_E,
        KEY_R,
        KEY_T,
        KEY_Y,
        KEY_U,
        KEY_I,
        KEY_O,
        KEY_P,
        KEY_LEFTBRACE,
        KEY_RIGHTBRACE,
        KEY_ENTER,
        KEY_LEFTCTRL,
        KEY_A,
        KEY_S,
        KEY_D,
        KEY_F,
        KEY_G,
        KEY_H,
        KEY_J,
        KEY_K,
        KEY_L,
        KEY_SEMICOLON,
        KEY_APOSTROPHE,
        KEY_GRAVE,
        KEY_LEFTSHIFT,
        KEY_BACKSLASH,
        KEY_Z,
        KEY_X,
        KEY_C,
        KEY_V,
        KEY_B,
        KEY_N,
        KEY_M,
        KEY_COMMA,
        KEY_DOT,
        KEY_SLASH,
        KEY_RIGHTSHIFT,
        KEY_LEFTALT,
        KEY_SPACE,
        KEY_F1,
        KEY_F2,
        KEY_F3,
        KEY_F4,
        KEY_F5,
        KEY_F6,
        KEY_F7,
        KEY_F8,
        KEY_F9,
        KEY_F10,
        KEY_F11,
        KEY_F12,
        KEY_HOME,
        KEY_UP,
        KEY_PAGEUP,
        KEY_LEFT,
        KEY_RIGHT,
        KEY_END,
        KEY_DOWN,
        KEY_PAGEDOWN,
        KEY_INSERT,
        KEY_DELETE,
        KEY_RIGHTCTRL,
        KEY_RIGHTALT,
        KEY_LEFTMETA,
        KEY_RIGHTMETA,
        KEY_MENU,
    ] {
        ioctl_set(fd, UI_SET_KEYBIT, k as libc::c_ulong)?;
    }

    // 设备名（合成器里显示的名字）
    let mut setup = UInputSetup::default();
    let name = b"npa-remote";
    setup.name[..name.len()].copy_from_slice(name);
    setup.id.bustype = 0x03; // BUS_USB
    setup.id.vendor = 0x70; // 随便填个 vendor id
    setup.id.product = 0x0001;
    setup.id.version = 0x0001;
    let rc = unsafe {
        libc::ioctl(
            fd,
            UI_DEV_SETUP,
            &setup as *const UInputSetup as *const libc::c_void,
        )
    };
    if rc < 0 {
        // 老内核不支持 UI_DEV_SETUP，退回旧接口（名字用 UI_SET_PHYS）
        let phys = b"npa-remote\0";
        unsafe {
            libc::ioctl(
                fd,
                UI_SET_PHYS,
                phys.as_ptr() as *const libc::c_char,
            );
        }
    }

    let rc = unsafe { libc::ioctl(fd, UI_DEV_CREATE, 0) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        bail!("UI_DEV_CREATE failed: {err}");
    }

    Ok(Device { fd, pos: None })
}

/// 确保虚拟设备已创建（幂等）。失败时返回错误，调用方退回 enigo。
pub fn ensure() -> Result<()> {
    let mut guard = DEVICE.lock().map_err(|_| anyhow::anyhow!("uinput lock poisoned"))?;
    if guard.is_some() {
        return Ok(());
    }
    let dev = create_device()?;
    *guard = Some(dev);
    Ok(())
}

/// uinput 是否可用（能创建虚拟设备）。
pub fn available() -> bool {
    ensure().is_ok()
}

/// 同步一次（EV_SYN/SYN_REPORT）。
fn sync(fd: RawFd) -> Result<()> {
    write_event(
        fd,
        InputEvent {
            type_: EV_SYN,
            code: 0,
            value: SYN_REPORT,
            ..Default::default()
        },
    )
}

fn write_event(fd: RawFd, ev: InputEvent) -> Result<()> {
    let bytes = unsafe {
        std::slice::from_raw_parts(
            &ev as *const InputEvent as *const u8,
            std::mem::size_of::<InputEvent>(),
        )
    };
    let n = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
    if n != bytes.len() as isize {
        bail!("uinput write failed: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// 把光标移动到 (x, y)（屏幕绝对坐标）。uinput 只有相对量，这里自己记位置。
pub fn move_to(x: i64, y: i64, sw: u32, sh: u32) -> Result<()> {
    ensure()?;
    let mut guard = DEVICE.lock().map_err(|_| anyhow::anyhow!("uinput lock poisoned"))?;
    let dev = guard.as_mut().context("uinput device missing")?;
    // 绝对定位：坐标按屏幕像素换算到 0..ABS_MAX。
    // 用绝对量而不是相对量，是为了绕开 Wayland 合成器的指针加速——
    // 相对量会被加速曲线缩放，远程光标会越点越偏。
    let w = sw.max(2) as f64;
    let h = sh.max(2) as f64;
    let ax = (((x as f64) * (ABS_MAX as f64)) / (w - 1.0)).round() as i32;
    let ay = (((y as f64) * (ABS_MAX as f64)) / (h - 1.0)).round() as i32;
    let ax = ax.clamp(0, ABS_MAX);
    let ay = ay.clamp(0, ABS_MAX);
    write_event(
        dev.fd,
        InputEvent {
            type_: EV_ABS,
            code: ABS_X,
            value: ax,
            ..Default::default()
        },
    )?;
    write_event(
        dev.fd,
        InputEvent {
            type_: EV_ABS,
            code: ABS_Y,
            value: ay,
            ..Default::default()
        },
    )?;
    sync(dev.fd)?;
    dev.pos = Some((x, y));
    Ok(())
}

/// 用外部读到的真实光标位置校准内部记录（首次使用时对齐）。
pub fn sync_position(x: i64, y: i64) {
    if let Ok(mut guard) = DEVICE.lock() {
        if let Some(dev) = guard.as_mut() {
            dev.pos = Some((x, y));
        }
    }
}

/// 鼠标键：click / press / release。
#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    Click,
    Press,
    Release,
}

pub fn button(name: &str, action: Action) -> Result<()> {
    ensure()?;
    let code = match name.to_ascii_lowercase().as_str() {
        "left" | "" => BTN_LEFT,
        "right" => BTN_RIGHT,
        "middle" => BTN_MIDDLE,
        other => bail!("unknown button: {other}"),
    };
    let mut guard = DEVICE.lock().map_err(|_| anyhow::anyhow!("uinput lock poisoned"))?;
    let dev = guard.as_mut().context("uinput device missing")?;
    match action {
        Action::Click => {
            write_event(
                dev.fd,
                InputEvent {
                    type_: EV_KEY,
                    code,
                    value: 1,
                    ..Default::default()
                },
            )?;
            sync(dev.fd)?;
            write_event(
                dev.fd,
                InputEvent {
                    type_: EV_KEY,
                    code,
                    value: 0,
                    ..Default::default()
                },
            )?;
            sync(dev.fd)?;
        }
        Action::Press => {
            write_event(
                dev.fd,
                InputEvent {
                    type_: EV_KEY,
                    code,
                    value: 1,
                    ..Default::default()
                },
            )?;
            sync(dev.fd)?;
        }
        Action::Release => {
            write_event(
                dev.fd,
                InputEvent {
                    type_: EV_KEY,
                    code,
                    value: 0,
                    ..Default::default()
                },
            )?;
            sync(dev.fd)?;
        }
    }
    Ok(())
}

/// 滚轮（正值向上，与现有约定一致）。
pub fn scroll(dy: i64) -> Result<()> {
    ensure()?;
    let mut guard = DEVICE.lock().map_err(|_| anyhow::anyhow!("uinput lock poisoned"))?;
    let dev = guard.as_mut().context("uinput device missing")?;
    write_event(
        dev.fd,
        InputEvent {
            type_: EV_REL,
            code: REL_WHEEL,
            value: dy as i32,
            ..Default::default()
        },
    )?;
    sync(dev.fd)?;
    Ok(())
}

/// 键盘按键（evdev 键码）。
pub fn key(code: u16, press: bool) -> Result<()> {
    ensure()?;
    let mut guard = DEVICE.lock().map_err(|_| anyhow::anyhow!("uinput lock poisoned"))?;
    let dev = guard.as_mut().context("uinput device missing")?;
    write_event(
        dev.fd,
        InputEvent {
            type_: EV_KEY,
            code,
            value: if press { 1 } else { 0 },
            ..Default::default()
        },
    )?;
    sync(dev.fd)?;
    Ok(())
}

/// 组合键：按下顺序 → 逆序抬起。
pub fn combo(codes: &[u16]) -> Result<()> {
    for c in codes {
        key(*c, true)?;
    }
    for c in codes.iter().rev() {
        key(*c, false)?;
    }
    Ok(())
}

/// 按名字组合（"ctrl+v" 这种）。
pub fn combo_names(parts: &[String]) -> Result<()> {
    let mut codes = Vec::new();
    for p in parts {
        let Some(c) = keycode_from_name(p) else {
            bail!("unknown key: {p}");
        };
        codes.push(c);
    }
    combo(&codes)
}

/// 逐键输入 ASCII 文本。
pub fn type_ascii(text: &str) -> Result<()> {
    for ch in text.chars() {
        let Some((code, shift)) = ascii_key(ch) else {
            bail!("uinput cannot type non-ascii character: {ch}");
        };
        if shift {
            key(KEY_LEFTSHIFT, true)?;
        }
        key(code, true)?;
        key(code, false)?;
        if shift {
            key(KEY_LEFTSHIFT, false)?;
        }
    }
    Ok(())
}

/// ASCII 字符 → (evdev 键码, 是否需要 Shift)。
pub fn ascii_key(ch: char) -> Option<(u16, bool)> {
    let lower = ch.to_ascii_lowercase();
    let code = match lower {
        ' ' => KEY_SPACE,
        '\n' | '\r' => KEY_ENTER,
        '\t' => KEY_TAB,
        'a' => KEY_A,
        'b' => KEY_B,
        'c' => KEY_C,
        'd' => KEY_D,
        'e' => KEY_E,
        'f' => KEY_F,
        'g' => KEY_G,
        'h' => KEY_H,
        'i' => KEY_I,
        'j' => KEY_J,
        'k' => KEY_K,
        'l' => KEY_L,
        'm' => KEY_M,
        'n' => KEY_N,
        'o' => KEY_O,
        'p' => KEY_P,
        'q' => KEY_Q,
        'r' => KEY_R,
        's' => KEY_S,
        't' => KEY_T,
        'u' => KEY_U,
        'v' => KEY_V,
        'w' => KEY_W,
        'x' => KEY_X,
        'y' => KEY_Y,
        'z' => KEY_Z,
        '0' => KEY_0,
        '1' => KEY_1,
        '2' => KEY_2,
        '3' => KEY_3,
        '4' => KEY_4,
        '5' => KEY_5,
        '6' => KEY_6,
        '7' => KEY_7,
        '8' => KEY_8,
        '9' => KEY_9,
        '-' => KEY_MINUS,
        '=' => KEY_EQUAL,
        '[' => KEY_LEFTBRACE,
        ']' => KEY_RIGHTBRACE,
        '\\' => KEY_BACKSLASH,
        ';' => KEY_SEMICOLON,
        '\'' => KEY_APOSTROPHE,
        '`' => KEY_GRAVE,
        ',' => KEY_COMMA,
        '.' => KEY_DOT,
        '/' => KEY_SLASH,
        _ => return None,
    };
    let shift = ch.is_ascii_uppercase()
        || matches!(
            ch,
            '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_' | '+' | '{' | '}' | '|' | ':' | '"' | '<' | '>' | '?' | '~'
        );
    Some((code, shift))
}

/// 键名（组合键用）→ evdev 键码。
pub fn keycode_from_name(name: &str) -> Option<u16> {
    let low = name.trim().to_ascii_lowercase();
    if low.is_empty() {
        return None;
    }
    if let Some(c) = modifier_code(&low) {
        return Some(c);
    }
    let named = match low.as_str() {
        "enter" | "return" => KEY_ENTER,
        "tab" => KEY_TAB,
        "esc" | "escape" => KEY_ESC,
        "space" => KEY_SPACE,
        "backspace" => KEY_BACKSPACE,
        "delete" | "del" => KEY_DELETE,
        "insert" | "ins" => KEY_INSERT,
        "home" => KEY_HOME,
        "end" => KEY_END,
        "pageup" | "pgup" => KEY_PAGEUP,
        "pagedown" | "pgdn" => KEY_PAGEDOWN,
        "up" | "uparrow" => KEY_UP,
        "down" | "downarrow" => KEY_DOWN,
        "left" | "leftarrow" => KEY_LEFT,
        "right" | "rightarrow" => KEY_RIGHT,
        "f1" => KEY_F1,
        "f2" => KEY_F2,
        "f3" => KEY_F3,
        "f4" => KEY_F4,
        "f5" => KEY_F5,
        "f6" => KEY_F6,
        "f7" => KEY_F7,
        "f8" => KEY_F8,
        "f9" => KEY_F9,
        "f10" => KEY_F10,
        "f11" => KEY_F11,
        "f12" => KEY_F12,
        _ => {
            let mut it = low.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            ascii_key(c).map(|(code, _)| code)?
        }
    };
    Some(named)
}
