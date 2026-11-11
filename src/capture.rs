// 屏幕捕获 + JPEG 编码。
// 骨架期用 xcap（跨平台纯 Rust 路径，mac/win/linux 通用）；
// 后续 Windows 上换 rustdesk libs/scrap 的 DXGI+硬编码，接口不变。
use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::RgbaImage;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Region {
    #[serde(default)]
    pub x: i64,
    #[serde(default)]
    pub y: i64,
    #[serde(default)]
    pub w: i64,
    #[serde(default)]
    pub h: i64,
}

impl Region {
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

/// 抓全屏（主显示器）并裁剪 region，返回 RGBA 帧。
pub fn grab(region: &Region) -> Result<RgbaImage> {
    let rgba = grab_full().context("capture failed")?;
    if region.is_empty() {
        return Ok(rgba);
    }
    let (mw, mh) = (rgba.width(), rgba.height());
    let x = (region.x.max(0) as u32).min(mw.saturating_sub(1));
    let y = (region.y.max(0) as u32).min(mh.saturating_sub(1));
    let w = (region.w as u32).min(mw.saturating_sub(x));
    let h = (region.h as u32).min(mh.saturating_sub(y));
    if w == 0 || h == 0 {
        anyhow::bail!("region out of bounds");
    }
    Ok(image::imageops::crop_imm(&rgba, x, y, w, h).to_image())
}

/// RGBA 帧编码为 JPEG（quality 1-100）。
pub fn encode_jpeg(frame: &RgbaImage, quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(128 * 1024);
    let rgb = image::DynamicImage::ImageRgba8(frame.clone()).to_rgb8();
    let enc = JpegEncoder::new_with_quality(&mut buf, quality.clamp(30, 95));
    rgb.write_with_encoder(enc)?;
    Ok(buf)
}

/// 全屏帧：优先 xcap，权限受限时回退平台 CLI。
fn grab_full() -> Result<RgbaImage> {
    if let Ok(monitors) = xcap::Monitor::all() {
        if let Some(m) = monitors.first() {
            if let Ok(img) = m.capture_image() {
                return Ok(img);
            }
        }
    }
    grab_cli()
}

#[cfg(target_os = "macos")]
fn grab_cli() -> Result<RgbaImage> {
    let tmp = std::env::temp_dir().join("npa-remote-shot.png");
    let st = std::process::Command::new("screencapture")
        .args(["-x", "-t", "png"])
        .arg(&tmp)
        .output()?;
    if !st.status.success() {
        anyhow::bail!("screencapture failed: {}", st.status);
    }
    let img = image::open(&tmp)?.to_rgba8();
    let _ = std::fs::remove_file(&tmp);
    Ok(img)
}

#[cfg(target_os = "windows")]
fn grab_cli() -> Result<RgbaImage> {
    anyhow::bail!("xcap unavailable")
}

#[cfg(target_os = "linux")]
fn grab_cli() -> Result<RgbaImage> {
    let tmp = std::env::temp_dir().join("npa-remote-shot.png");
    let st = std::process::Command::new("import")
        .arg("-window")
        .arg("root")
        .arg(&tmp)
        .output()?;
    if !st.status.success() {
        anyhow::bail!("import (imagemagick) failed: {}", st.status);
    }
    let img = image::open(&tmp)?.to_rgba8();
    let _ = std::fs::remove_file(&tmp);
    Ok(img)
}

/// 屏幕信息：分辨率 + 鼠标位置由 input 模块补充，这里给分辨率。
pub fn screen_size() -> Result<(u32, u32)> {
    let img = grab_full()?;
    Ok((img.width(), img.height()))
}
