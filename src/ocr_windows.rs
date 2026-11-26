// Windows OCR：调用系统自带 WinRT OCR（Windows.Media.Ocr）。
// 走 PowerShell 脚本是因为 WinRT 的异步接口在纯 Rust 里没有稳定绑定，
// 而系统自带引擎不需要额外安装（与之前 NPA 内置脚本同一套能力）。
use anyhow::{Context, Result};
use std::process::Command;

use crate::ocr::{parse_payload, Outcome};

const SCRIPT: &str = include_str!("ocr_windows.ps1");

pub fn recognize(path: &str, lang: &str) -> Result<Outcome> {
    let script_path = write_script()?;
    let out = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .arg(path)
        .arg(lang)
        .output()
        .context("failed to launch powershell for OCR")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_file(&script_path);

    let raw = if stdout.trim().is_empty() { stderr } else { stdout };
    parse_payload(&raw)
}

fn write_script() -> Result<String> {
    let dir = std::env::temp_dir().join("npa-remote");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "ocr-{}.ps1",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::write(&path, SCRIPT)?;
    Ok(path.to_string_lossy().into_owned())
}
