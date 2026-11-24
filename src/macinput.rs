// macOS 物理按键注入（CGEvent）。
//
// 和 Windows 同理：中文输入法（拼音/仓颉）只处理**物理按键**，
// 直接注入 Unicode 文本不会进候选窗。RustDesk 也是用 CGEvent 的
// keycode 注入，这样 IME 能正常组词。

use anyhow::{bail, Result};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(source: *const core::ffi::c_void, virtual_key: u16, key_down: bool)
        -> *mut core::ffi::c_void;
    fn CGEventPost(tap: u32, event: *const core::ffi::c_void);
    fn CGEventSetFlags(event: *mut core::ffi::c_void, flags: u64);
}

const CG_HID_EVENT_TAP: u32 = 0;
const CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
const CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
const CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
const CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;

fn post(code: u16, down: bool, flags: u64) -> Result<()> {
    unsafe {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
        if ev.is_null() {
            bail!("CGEventCreateKeyboardEvent failed");
        }
        if flags != 0 {
            CGEventSetFlags(ev, flags);
        }
        CGEventPost(CG_HID_EVENT_TAP, ev);
    }
    Ok(())
}

/// 浏览器 KeyboardEvent.code → macOS 虚拟键码（ANSI 布局）。
pub fn keycode_from_browser(code: &str) -> Option<u16> {
    if let Some(rest) = code.strip_prefix("Key") {
        if rest.len() == 1 {
            let c = rest.chars().next().unwrap().to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(match c {
                    'A' => 0x00,
                    'S' => 0x01,
                    'D' => 0x02,
                    'F' => 0x03,
                    'H' => 0x04,
                    'G' => 0x05,
                    'Z' => 0x06,
                    'X' => 0x07,
                    'C' => 0x08,
                    'V' => 0x09,
                    'B' => 0x0B,
                    'Q' => 0x0C,
                    'W' => 0x0D,
                    'E' => 0x0E,
                    'R' => 0x0F,
                    'Y' => 0x10,
                    'T' => 0x11,
                    'I' => 0x22,
                    'O' => 0x1F,
                    'P' => 0x23,
                    'L' => 0x25,
                    'J' => 0x26,
                    'K' => 0x28,
                    'N' => 0x2D,
                    'M' => 0x2E,
                    _ => return None,
                });
            }
        }
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if rest.len() == 1 {
            if let Some(d) = rest.chars().next().and_then(|c| c.to_digit(10)) {
                return Some(match d {
                    1 => 0x12,
                    2 => 0x13,
                    3 => 0x14,
                    4 => 0x15,
                    5 => 0x17,
                    6 => 0x16,
                    7 => 0x1A,
                    8 => 0x1C,
                    9 => 0x19,
                    0 => 0x1D,
                    _ => return None,
                });
            }
        }
    }
    Some(match code {
        "Space" => 0x31,
        "Enter" | "NumpadEnter" => 0x24,
        "Tab" => 0x30,
        "Escape" => 0x35,
        "Backspace" => 0x33,
        "Delete" => 0x75,
        "Home" => 0x73,
        "End" => 0x77,
        "PageUp" => 0x74,
        "PageDown" => 0x79,
        "ArrowLeft" => 0x7B,
        "ArrowRight" => 0x7C,
        "ArrowDown" => 0x7D,
        "ArrowUp" => 0x7E,
        "Minus" => 0x1B,
        "Equal" => 0x18,
        "BracketLeft" => 0x21,
        "BracketRight" => 0x1E,
        "Backslash" => 0x2A,
        "Semicolon" => 0x29,
        "Quote" => 0x27,
        "Backquote" => 0x32,
        "Comma" => 0x2B,
        "Period" => 0x2F,
        "Slash" => 0x2C,
        "ControlLeft" => 0x3B,
        "ControlRight" => 0x3E,
        "ShiftLeft" => 0x38,
        "ShiftRight" => 0x3C,
        "AltLeft" => 0x3A,
        "AltRight" => 0x3D,
        "MetaLeft" => 0x37,
        "MetaRight" => 0x36,
        "F1" => 0x7A,
        "F2" => 0x78,
        "F3" => 0x63,
        "F4" => 0x76,
        "F5" => 0x60,
        "F6" => 0x61,
        "F7" => 0x62,
        "F8" => 0x64,
        "F9" => 0x65,
        "F10" => 0x6D,
        "F11" => 0x67,
        "F12" => 0x6F,
        _ => return None,
    })
}

/// 组合键名 → macOS 虚拟键码。
pub fn keycode_from_name(name: &str) -> Option<u16> {
    let low = name.trim().to_ascii_lowercase();
    match low.as_str() {
        "ctrl" | "control" => return Some(0x3B),
        "shift" => return Some(0x38),
        "alt" | "option" => return Some(0x3A),
        "cmd" | "command" | "meta" | "win" | "super" => return Some(0x37),
        "space" => return Some(0x31),
        "enter" | "return" => return Some(0x24),
        "tab" => return Some(0x30),
        "esc" | "escape" => return Some(0x35),
        "backspace" => return Some(0x33),
        "delete" | "del" => return Some(0x75),
        "home" => return Some(0x73),
        "end" => return Some(0x77),
        "pageup" | "pgup" => return Some(0x74),
        "pagedown" | "pgdn" => return Some(0x79),
        "up" | "arrowup" => return Some(0x7E),
        "down" | "arrowdown" => return Some(0x7D),
        "left" | "arrowleft" => return Some(0x7B),
        "right" | "arrowright" => return Some(0x7C),
        _ => {}
    }
    if low.len() == 1 {
        let c = low.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return keycode_from_browser(&format!("Key{}", c.to_ascii_uppercase()));
        }
        if c.is_ascii_digit() {
            return keycode_from_browser(&format!("Digit{c}"));
        }
    }
    None
}

/// 按下/抬起一个物理键。
pub fn send_key(code: u16, down: bool, flags: u64) -> Result<()> {
    post(code, down, flags)
}

/// 组合键：按下顺序 → 逆序抬起（修饰键带 flag）。
pub fn send_combo(parts: &[(u16, u64)]) -> Result<()> {
    for (code, flags) in parts {
        post(*code, true, *flags)?;
    }
    for (code, flags) in parts.iter().rev() {
        post(*code, false, *flags)?;
    }
    Ok(())
}

/// 修饰键名 → CGEvent flag。
pub fn flag_for(name: &str) -> u64 {
    match name.trim().to_ascii_lowercase().as_str() {
        "shift" => CG_EVENT_FLAG_MASK_SHIFT,
        "ctrl" | "control" => CG_EVENT_FLAG_MASK_CONTROL,
        "alt" | "option" => CG_EVENT_FLAG_MASK_ALTERNATE,
        "cmd" | "command" | "meta" | "win" | "super" => CG_EVENT_FLAG_MASK_COMMAND,
        _ => 0,
    }
}
