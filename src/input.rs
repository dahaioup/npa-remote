// 输入注入与剪贴板（enigo + arboard）。
use anyhow::{bail, Context, Result};
use enigo::{Enigo, Keyboard, Mouse, Settings};

const MAX_TEXT: usize = 4000;
const MAX_COMBO: usize = 8;

fn enigo() -> Result<Enigo> {
    Ok(Enigo::new(&Settings::default())?)
}

/// uinput 虚拟输入设备是否可用。
///
/// 失败会退回 enigo（XTEST），但在 Wayland 会话里 XTEST 不起作用 —— 所以把
/// 失败原因记一条日志，否则远程表现为「能看不能点」却毫无线索。
#[cfg(target_os = "linux")]
fn uinput_ready() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0); // 0=未探测 1=可用 2=不可用
    match STATE.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    match crate::uinput::ensure() {
        Ok(()) => {
            STATE.store(1, Ordering::Relaxed);
            tracing::info!("uinput virtual input device ready");
            true
        }
        Err(e) => {
            if STATE.swap(2, Ordering::Relaxed) == 0 {
                tracing::warn!("uinput unavailable, falling back to enigo: {e:#}");
            }
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn uinput_ready() -> bool {
    false
}

/// 打字（Unicode 文本路径，中文/英文都行）。
/// 特殊控制字符映射：\r→Enter、\x08→Backspace、\t→Tab、\x1b→Escape、
/// \x1b[A/B/C/D→方向键、\x1b[H/F→Home/End、\x1b[3~→Delete、\x1b[5~/6~→PgUp/PgDn、\x1b[2~→Insert。
pub fn type_text(text: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return type_text_uinput(text);
    }
    let chars = text.chars().count();
    if chars == 0 || chars > MAX_TEXT {
        bail!("text length must be 1..={MAX_TEXT}");
    }
    let mut e = enigo()?;
    let mut iter = text.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\r' | '\n' => { e.key(enigo::Key::Return, enigo::Direction::Click)?; }
            '\x08' => { e.key(enigo::Key::Backspace, enigo::Direction::Click)?; }
            '\t' => { e.key(enigo::Key::Tab, enigo::Direction::Click)?; }
            '\x1b' => {
                // ESC 序列：\x1b[X 或 \x1b[NN~
                match iter.next() {
                    Some('[') => {
                        match iter.next() {
                            Some('A') => e.key(enigo::Key::UpArrow, enigo::Direction::Click)?,
                            Some('B') => e.key(enigo::Key::DownArrow, enigo::Direction::Click)?,
                            Some('C') => e.key(enigo::Key::RightArrow, enigo::Direction::Click)?,
                            Some('D') => e.key(enigo::Key::LeftArrow, enigo::Direction::Click)?,
                            Some('H') => e.key(enigo::Key::Home, enigo::Direction::Click)?,
                            Some('F') => e.key(enigo::Key::End, enigo::Direction::Click)?,
                            Some('3') => { e.key(enigo::Key::Delete, enigo::Direction::Click)?; iter.next(); iter.next(); }
                            Some('5') => { e.key(enigo::Key::PageUp, enigo::Direction::Click)?; iter.next(); iter.next(); }
                            Some('6') => { e.key(enigo::Key::PageDown, enigo::Direction::Click)?; iter.next(); iter.next(); }
                            Some('2') => { /* Insert 无 enigo 键，跳过 */ iter.next(); iter.next(); }
                            _ => {} // 未知序列忽略
                        }
                    }
                    Some('O') => {
                        // \x1bOA 等 SS3 序列（F1-F4、Home/End）
                        match iter.next() {
                            Some('A') => e.key(enigo::Key::UpArrow, enigo::Direction::Click)?,
                            Some('B') => e.key(enigo::Key::DownArrow, enigo::Direction::Click)?,
                            Some('C') => e.key(enigo::Key::RightArrow, enigo::Direction::Click)?,
                            Some('D') => e.key(enigo::Key::LeftArrow, enigo::Direction::Click)?,
                            Some('H') => e.key(enigo::Key::Home, enigo::Direction::Click)?,
                            Some('F') => e.key(enigo::Key::End, enigo::Direction::Click)?,
                            Some('P') => e.key(enigo::Key::F1, enigo::Direction::Click)?,
                            Some('Q') => e.key(enigo::Key::F2, enigo::Direction::Click)?,
                            Some('R') => e.key(enigo::Key::F3, enigo::Direction::Click)?,
                            Some('S') => e.key(enigo::Key::F4, enigo::Direction::Click)?,
                            _ => {}
                        }
                    }
                    _ => { e.key(enigo::Key::Escape, enigo::Direction::Click)?; }
                }
            }
            _ => {
                // 普通字符：ASCII 走物理键（输入法能组词），非 ASCII 走 Unicode 注入
                #[cfg(any(target_os = "windows", target_os = "macos"))]
                {
                    if let Some(code) = code_for_char(c) {
                        press_code(code, c.is_ascii_uppercase(), Some(c))?;
                        continue;
                    }
                }
                e.text(&c.to_string())?;
            }
        }
    }
    Ok(())
}

/// Wayland 会话下的文本输入（uinput 逐键）。
///
/// uinput 只能发物理键码，没有「输入任意 Unicode」的能力，
/// 所以非 ASCII（中文）请走剪贴板 + 粘贴。
#[cfg(target_os = "linux")]
fn type_text_uinput(text: &str) -> Result<()> {
    use crate::uinput;
    let chars = text.chars().count();
    if chars == 0 || chars > MAX_TEXT {
        bail!("text length must be 1..={MAX_TEXT}");
    }
    for c in text.chars() {
        match c {
            '\r' | '\n' => {
                uinput::key(uinput::KEY_ENTER, true)?;
                uinput::key(uinput::KEY_ENTER, false)?;
            }
            '\t' => {
                uinput::key(uinput::KEY_TAB, true)?;
                uinput::key(uinput::KEY_TAB, false)?;
            }
            '\x08' => {
                uinput::key(uinput::KEY_BACKSPACE, true)?;
                uinput::key(uinput::KEY_BACKSPACE, false)?;
            }
            '\x1b' => {
                uinput::key(uinput::KEY_ESC, true)?;
                uinput::key(uinput::KEY_ESC, false)?;
            }
            _ => {
                let Some((code, shift)) = uinput::ascii_key(c) else {
                    bail!("Wayland 会话只能输入 ASCII；中文请用「复制选中→剪贴板」再 ctrl+v 粘贴");
                };
                if shift {
                    uinput::key(uinput::KEY_LEFTSHIFT, true)?;
                }
                uinput::key(code, true)?;
                uinput::key(code, false)?;
                if shift {
                    uinput::key(uinput::KEY_LEFTSHIFT, false)?;
                }
            }
        }
    }
    Ok(())
}

/// 组合键："ctrl+v"、"cmd+shift+3"、"enter"。
/// 修饰键名：ctrl/cmd/alt/shift/meta/win/super（大小写不敏感）。
/// 单独按 Shift：切换目标设备输入法（Win+Space / mac 切换输入法快捷键）
pub fn toggle_ime() -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return crate::uinput::combo_names(&["super".to_string(), "space".to_string()]);
    }
    let mut e = enigo()?;
    #[cfg(target_os = "windows")]
    {
        e.key(enigo::Key::Meta, enigo::Direction::Press)?;
        e.key(enigo::Key::Space, enigo::Direction::Click)?;
        e.key(enigo::Key::Meta, enigo::Direction::Release)?;
    }
    #[cfg(target_os = "macos")]
    {
        // macOS 默认输入法切换：Control+Space 或 Ctrl+Option+Space
        e.key(enigo::Key::Control, enigo::Direction::Press)?;
        e.key(enigo::Key::Space, enigo::Direction::Click)?;
        e.key(enigo::Key::Control, enigo::Direction::Release)?;
    }
    #[cfg(target_os = "linux")]
    {
        // Linux 常见：Super+Space
        e.key(enigo::Key::Meta, enigo::Direction::Press)?;
        e.key(enigo::Key::Space, enigo::Direction::Click)?;
        e.key(enigo::Key::Meta, enigo::Direction::Release)?;
    }
    Ok(())
}

pub fn hotkey(combo: &str) -> Result<()> {
    let parts: Vec<&str> = combo
        .split(['+', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > MAX_COMBO {
        bail!("combo must have 1..{MAX_COMBO} parts");
    }
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        let names: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        return crate::uinput::combo_names(&names);
    }
    #[cfg(target_os = "windows")]
    {
        let mut vks = Vec::new();
        for p in &parts {
            let Some(vk) = crate::wininput::vk_from_name(p) else {
                bail!("unknown key: {p}");
            };
            vks.push(vk);
        }
        return crate::wininput::send_combo(&vks);
    }
    #[cfg(target_os = "macos")]
    {
        let mut codes = Vec::new();
        for p in &parts {
            let Some(code) = crate::macinput::keycode_from_name(p) else {
                bail!("unknown key: {p}");
            };
            codes.push((code, crate::macinput::flag_for(p)));
        }
        return crate::macinput::send_combo(&codes);
    }
    #[allow(unreachable_code)]
    {
    let mut keys = Vec::new();
    for p in &parts {
        match parse_key(p)? {
            KeyOrMod::Key(k) => keys.push(k),
            KeyOrMod::Modifier(m) => keys.push(enigo::Key::Unicode(m)),
        }
    }
    let mut e = enigo()?;
    // 按顺序按下，逆序抬起
    for k in &keys {
        e.key(*k, enigo::Direction::Press)?;
    }
    for k in keys.iter().rev() {
        e.key(*k, enigo::Direction::Release)?;
    }
    Ok(())
    }
}

enum KeyOrMod {
    Key(enigo::Key),
    Modifier(char), // 用占位字符，实际用 Modifier 映射
}

fn parse_key(s: &str) -> Result<KeyOrMod> {
    let low = s.to_ascii_lowercase();
    use enigo::Key as K;
    let named = match low.as_str() {
        "ctrl" | "control" | "controlleft" => K::Control,
        "cmd" | "meta" | "win" | "super" => K::Meta,
        "alt" | "option" => K::Alt,
        "shift" => K::Shift,
        "enter" | "return" => K::Return,
        "tab" => K::Tab,
        "esc" | "escape" => K::Escape,
        "space" => K::Space,
        "backspace" => K::Backspace,
        "delete" | "del" => K::Delete,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "up" => K::UpArrow,
        "down" => K::DownArrow,
        "left" => K::LeftArrow,
        "right" => K::RightArrow,
        "f1" => K::F1,
        "f2" => K::F2,
        "f3" => K::F3,
        "f4" => K::F4,
        "f5" => K::F5,
        "f6" => K::F6,
        "f7" => K::F7,
        "f8" => K::F8,
        "f9" => K::F9,
        "f10" => K::F10,
        "f11" => K::F11,
        "f12" => K::F12,
        _ => {
            // 单字符键：字母/数字/常用符号
            let mut chars = s.chars();
            let c = chars.next().context("empty key")?;
            if chars.next().is_some() {
                bail!("unknown key: {s}");
            }
            return Ok(KeyOrMod::Key(K::Unicode(c)));
        }
    };
    Ok(KeyOrMod::Key(named))
}

/// 鼠标：move/click/scroll。
///
/// Wayland 会话（KWin/Mutter）下 X11 XTEST 注入的事件不会进入合成器的输入
/// 管线 —— enigo 在 Xwayland 上注入的鼠标事件被丢弃，表现为「画面能看到、
/// 点不动」。所以 Linux 上优先走 uinput 虚拟输入设备（合成器当真实硬件读），
/// 不可用时（例如非 root）再退回 enigo/XTEST。
pub fn mouse_move(x: i64, y: i64) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return crate::uinput::move_to(x, y);
    }
    let mut e = enigo()?;
    e.move_mouse(x as i32, y as i32, enigo::Coordinate::Abs)?;
    Ok(())
}

/// 鼠标按下（按住不放，配合 mouse_up 实现拖选/拖窗口）。
pub fn mouse_down(button: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return crate::uinput::button(button, crate::uinput::Action::Press);
    }
    let btn = parse_button(button)?;
    let mut e = enigo()?;
    e.button(btn, enigo::Direction::Press)?;
    Ok(())
}

/// 鼠标抬起（结束按住）。
pub fn mouse_up(button: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return crate::uinput::button(button, crate::uinput::Action::Release);
    }
    let btn = parse_button(button)?;
    let mut e = enigo()?;
    e.button(btn, enigo::Direction::Release)?;
    Ok(())
}

fn parse_button(button: &str) -> Result<enigo::Button> {
    match button.to_ascii_lowercase().as_str() {
        "left" | "" => Ok(enigo::Button::Left),
        "right" => Ok(enigo::Button::Right),
        "middle" => Ok(enigo::Button::Middle),
        _ => bail!("unknown button: {button}"),
    }
}

pub fn mouse_click(button: &str, count: u32) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        let n = count.clamp(1, 3);
        for _ in 0..n {
            crate::uinput::button(button, crate::uinput::Action::Click)?;
        }
        return Ok(());
    }
    let btn = parse_button(button)?;
    let n = count.clamp(1, 3);
    let mut e = enigo()?;
    for _ in 0..n {
        e.button(btn, enigo::Direction::Click)?;
    }
    Ok(())
}

pub fn mouse_scroll(dy: i64) -> Result<()> {
    #[cfg(target_os = "linux")]
    if uinput_ready() {
        return crate::uinput::scroll(dy);
    }
    let mut e = enigo()?;
    // 正值向上
    e.scroll(dy as i32, enigo::Axis::Vertical)?;
    Ok(())
}

/// 剪贴板读取（文本）。
pub fn clipboard_get() -> Result<String> {
    let mut cb = arboard::Clipboard::new()?;
    match cb.get_text() {
        Ok(t) => Ok(t),
        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// 剪贴板写入（文本）。
pub fn clipboard_set(text: &str) -> Result<()> {
    if text.len() > 1 << 20 {
        bail!("clipboard text too large (max 1MiB)");
    }
    let mut cb = arboard::Clipboard::new()?;
    cb.set_text(text.to_string())?;
    Ok(())
}

/// 鼠标当前位置（经 enigo 内部 API 不可用，这里用 xcap 不可得；
/// 各平台实现见 info.rs —— 骨架期 macOS/Win 用 enigo 的 cursor 轮询接口）。
pub fn mouse_pos() -> Result<(i64, i64)> {
    let mut e = enigo()?;
    match e.location() {
        Ok((x, y)) => Ok((x as i64, y as i64)),
        Err(e) => Err(e.into()),
    }
}

// ================= 物理按键（输入法友好） =================
//
// 目标机上的中文输入法（微软拼音/搜狗/ibus/fcitx）只处理**物理按键**：
// 直接注入 Unicode 文本不会进候选窗，表现是「输入法开着，打出来还是字母」。
// 所以浏览器把 KeyboardEvent.code 传过来，各平台注入真实按键：
//   Windows → SendInput(VK + 扫描码)
//   Linux   → uinput 虚拟键盘（Wayland 合成器经 libinput 读取）
//   macOS   → CGEvent keycode

/// 注入一个物理按键（浏览器 KeyboardEvent.code）。
pub fn press_code(code: &str, shift: bool, fallback_char: Option<char>) -> Result<()> {
    let _ = &fallback_char;
    #[cfg(target_os = "windows")]
    {
        return win_press_code(code, shift);
    }
    #[cfg(target_os = "linux")]
    {
        return linux_press_code(code, shift, fallback_char);
    }
    #[cfg(target_os = "macos")]
    {
        return mac_press_code(code, shift);
    }
    #[allow(unreachable_code)]
    {
        let _ = (code, shift, fallback_char);
        bail!("unsupported platform");
    }
}

#[cfg(target_os = "windows")]
fn win_press_code(code: &str, shift: bool) -> Result<()> {
    let Some(vk) = crate::wininput::vk_from_browser_code(code) else {
        bail!("unsupported key code: {code}");
    };
    if shift {
        crate::wininput::send_vk(0x10, true)?;
    }
    crate::wininput::send_vk(vk, true)?;
    crate::wininput::send_vk(vk, false)?;
    if shift {
        crate::wininput::send_vk(0x10, false)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_press_code(code: &str, shift: bool, fallback_char: Option<char>) -> Result<()> {
    if uinput_ready() {
        let Some(kc) = crate::uinput::keycode_from_browser(code) else {
            bail!("unsupported key code: {code}");
        };
        if shift {
            crate::uinput::key(crate::uinput::KEY_LEFTSHIFT, true)?;
        }
        crate::uinput::key(kc, true)?;
        crate::uinput::key(kc, false)?;
        if shift {
            crate::uinput::key(crate::uinput::KEY_LEFTSHIFT, false)?;
        }
        return Ok(());
    }
    // 非 root / 无 uinput：退回 enigo（X11 下 XTEST，尽量用按键名）
    let mut e = enigo()?;
    if shift {
        e.key(enigo::Key::Shift, enigo::Direction::Press)?;
    }
    match parse_key(code)? {
        KeyOrMod::Key(k) => e.key(k, enigo::Direction::Click)?,
        KeyOrMod::Modifier(_) => {
            if let Some(ch) = fallback_char {
                e.text(&ch.to_string())?;
            }
        }
    }
    if shift {
        e.key(enigo::Key::Shift, enigo::Direction::Release)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_press_code(code: &str, shift: bool) -> Result<()> {
    let Some(kc) = crate::macinput::keycode_from_browser(code) else {
        bail!("unsupported key code: {code}");
    };
    let flags = if shift {
        crate::macinput::flag_for("shift")
    } else {
        0
    };
    crate::macinput::send_key(kc, true, flags)?;
    crate::macinput::send_key(kc, false, flags)?;
    Ok(())
}

/// 字符 → 浏览器 KeyboardEvent.code（字母/数字/空格，用于物理键注入）。
fn code_for_char(c: char) -> Option<&'static str> {
    let up = c.to_ascii_uppercase();
    if c.is_ascii_alphabetic() {
        return Some(match up {
            'A' => "KeyA",
            'B' => "KeyB",
            'C' => "KeyC",
            'D' => "KeyD",
            'E' => "KeyE",
            'F' => "KeyF",
            'G' => "KeyG",
            'H' => "KeyH",
            'I' => "KeyI",
            'J' => "KeyJ",
            'K' => "KeyK",
            'L' => "KeyL",
            'M' => "KeyM",
            'N' => "KeyN",
            'O' => "KeyO",
            'P' => "KeyP",
            'Q' => "KeyQ",
            'R' => "KeyR",
            'S' => "KeyS",
            'T' => "KeyT",
            'U' => "KeyU",
            'V' => "KeyV",
            'W' => "KeyW",
            'X' => "KeyX",
            'Y' => "KeyY",
            'Z' => "KeyZ",
            _ => return None,
        });
    }
    Some(match c {
        '0' => "Digit0",
        '1' => "Digit1",
        '2' => "Digit2",
        '3' => "Digit3",
        '4' => "Digit4",
        '5' => "Digit5",
        '6' => "Digit6",
        '7' => "Digit7",
        '8' => "Digit8",
        '9' => "Digit9",
        ' ' => "Space",
        _ => return None,
    })
}

/// 按住修饰键（配合点击做 ctrl+click / shift+click）。
pub fn hold_modifiers(mods: &[String]) -> Result<()> {
    for m in mods {
        hold_modifier(m)?;
    }
    Ok(())
}

/// 释放修饰键。
pub fn release_modifiers(mods: &[String]) -> Result<()> {
    for m in mods.iter().rev() {
        release_modifier(m)?;
    }
    Ok(())
}

fn hold_modifier(name: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        if let Some(vk) = crate::wininput::vk_from_name(name) {
            return crate::wininput::send_vk(vk, true);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if uinput_ready() {
            if let Some(code) = crate::uinput::modifier_code(name) {
                return crate::uinput::key(code, true);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(code) = crate::macinput::keycode_from_name(name) {
            return crate::macinput::send_key(code, true, crate::macinput::flag_for(name));
        }
    }
    let mut e = enigo()?;
    let key = match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => enigo::Key::Control,
        "shift" => enigo::Key::Shift,
        "alt" | "option" => enigo::Key::Alt,
        "cmd" | "command" | "meta" | "win" | "super" => enigo::Key::Meta,
        other => bail!("unknown modifier: {other}"),
    };
    e.key(key, enigo::Direction::Press)?;
    Ok(())
}

fn release_modifier(name: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        if let Some(vk) = crate::wininput::vk_from_name(name) {
            return crate::wininput::send_vk(vk, false);
        }
    }
    #[cfg(target_os = "linux")]
    {
        if uinput_ready() {
            if let Some(code) = crate::uinput::modifier_code(name) {
                return crate::uinput::key(code, false);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(code) = crate::macinput::keycode_from_name(name) {
            return crate::macinput::send_key(code, false, 0);
        }
    }
    let mut e = enigo()?;
    let key = match name.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => enigo::Key::Control,
        "shift" => enigo::Key::Shift,
        "alt" | "option" => enigo::Key::Alt,
        "cmd" | "command" | "meta" | "win" | "super" => enigo::Key::Meta,
        other => bail!("unknown modifier: {other}"),
    };
    e.key(key, enigo::Direction::Release)?;
    Ok(())
}

#[allow(unused)]
fn unused_silence() {}
