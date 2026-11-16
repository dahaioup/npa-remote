// 输入注入与剪贴板（enigo + arboard）。
use anyhow::{bail, Context, Result};
use enigo::{Enigo, Keyboard, Mouse, Settings};

const MAX_TEXT: usize = 4000;
const MAX_COMBO: usize = 8;

fn enigo() -> Result<Enigo> {
    Ok(Enigo::new(&Settings::default())?)
}

/// 打字（Unicode 文本路径，中文/英文都行）。
/// 特殊控制字符映射：\r→Enter、\x08→Backspace、\t→Tab、\x1b→Escape、
/// \x1b[A/B/C/D→方向键、\x1b[H/F→Home/End、\x1b[3~→Delete、\x1b[5~/6~→PgUp/PgDn、\x1b[2~→Insert。
pub fn type_text(text: &str) -> Result<()> {
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
            _ => { e.text(&c.to_string())?; }
        }
    }
    Ok(())
}

/// 组合键："ctrl+v"、"cmd+shift+3"、"enter"。
/// 修饰键名：ctrl/cmd/alt/shift/meta/win/super（大小写不敏感）。
/// 单独按 Shift：切换目标设备输入法（Win+Space / mac 切换输入法快捷键）
pub fn toggle_ime() -> Result<()> {
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
pub fn mouse_move(x: i64, y: i64) -> Result<()> {
    let mut e = enigo()?;
    e.move_mouse(x as i32, y as i32, enigo::Coordinate::Abs)?;
    Ok(())
}

/// 鼠标按下（按住不放，配合 mouse_up 实现拖选/拖窗口）。
pub fn mouse_down(button: &str) -> Result<()> {
    let btn = parse_button(button)?;
    let mut e = enigo()?;
    e.button(btn, enigo::Direction::Press)?;
    Ok(())
}

/// 鼠标抬起（结束按住）。
pub fn mouse_up(button: &str) -> Result<()> {
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
    let btn = parse_button(button)?;
    let n = count.clamp(1, 3);
    let mut e = enigo()?;
    for _ in 0..n {
        e.button(btn, enigo::Direction::Click)?;
    }
    Ok(())
}

pub fn mouse_scroll(dy: i64) -> Result<()> {
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

#[allow(unused)]
fn unused_silence() {}
