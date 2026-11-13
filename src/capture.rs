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

/// 屏幕选择：None = 首屏；Some(i) = 第 i 块屏（多屏支持）。
pub static SCREEN_SEL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn screen_count() -> usize {
    xcap::Monitor::all().map(|m| m.len()).unwrap_or(1)
}

/// 枚举屏幕（id 从 0 起），前端下拉框用。
pub fn screen_list() -> Vec<(usize, u32, u32)> {
    xcap::Monitor::all()
        .map(|ms| {
            ms.iter()
                .enumerate()
                .map(|(i, m)| (i, m.width() as u32, m.height() as u32))
                .collect()
        })
        .unwrap_or_default()
}

/// 全屏帧：优先 xcap 按选定屏抓取，权限受限时回退平台 CLI。
fn grab_full() -> Result<RgbaImage> {
    let sel = SCREEN_SEL.load(std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut monitors) = xcap::Monitor::all() {
        if !monitors.is_empty() {
            let idx = sel.min(monitors.len() - 1);
            match monitors.swap_remove(idx).capture_image() {
                Ok(img) => return Ok(img),
                Err(e) => tracing::warn!("xcap capture_image failed ({e}), fallback to cli"),
            }
        } else {
            tracing::warn!("xcap: no monitors, fallback to cli");
        }
    } else {
        tracing::warn!("xcap Monitor::all failed, fallback to cli");
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

/// 画质档（0=极简 1=省流 2=标清 3=流畅 4=清晰 5=高清 6=超清）→ JPEG 质量。
/// 七档按 1080P 码率 32K/128K/512K/1M/2M/8M/16M 定义（VP8 路径见 vpx.rs::tier_kbps），
/// JPEG 路径映射为视觉可辨的压缩质量。
pub fn tier_jpeg_quality(tier: u8) -> u8 {
    // 七档：32K/128K/512K/1M/2M/8M/16M → JPEG 质量（30..95 夹紧在 encode_jpeg）
    match tier {
        0 => 35,
        1 => 42,
        2 => 46,
        3 => 50,
        4 => 65,
        5 => 78,
        _ => 90,
    }
}

/// 帧缩放百分比（50/70/90/100）→ 缩放 RGBA 帧。>=100 原样返回。
pub fn resize_rgba(img: &RgbaImage, pct: u8) -> RgbaImage {
    if pct >= 100 {
        return img.clone();
    }
    let nw = (img.width() as u32 * pct as u32 / 100).max(1);
    let nh = (img.height() as u32 * pct as u32 / 100).max(1);
    image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle)
}

/// 物理像素 / enigo 逻辑坐标 的比值。
/// mac Retina：xcap 抓物理像素（2x），CGEvent 用逻辑点 → 2.0；其它平台 1.0。
/// （Windows DPI 感知待实测，先按 1.0。）
pub fn pixel_per_point() -> f64 {
    #[cfg(target_os = "macos")]
    {
        xcap::Monitor::all()
            .ok()
            .and_then(|ms| ms.first().map(|m| m.scale_factor() as f64))
            .filter(|v| *v > 0.0)
            .unwrap_or(1.0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_tier_mapping() {
        assert_eq!(tier_jpeg_quality(0), 35); // 极简 32K
        assert_eq!(tier_jpeg_quality(1), 42); // 省流 128K
        assert_eq!(tier_jpeg_quality(2), 46); // 标清 512K
        assert_eq!(tier_jpeg_quality(3), 50); // 流畅 1M（默认）
        assert_eq!(tier_jpeg_quality(4), 65); // 清晰 2M
        assert_eq!(tier_jpeg_quality(5), 78); // 高清 8M
        assert_eq!(tier_jpeg_quality(6), 90); // 超清 16M
        assert_eq!(tier_jpeg_quality(255), 90); // 越界按最高档
    }

    #[test]
    fn resize_scales_dimensions() {
        let img = RgbaImage::new(1920, 1080);
        assert_eq!(resize_rgba(&img, 100).dimensions(), (1920, 1080));
        assert_eq!(resize_rgba(&img, 90).dimensions(), (1728, 972));
        assert_eq!(resize_rgba(&img, 70).dimensions(), (1344, 756));
        assert_eq!(resize_rgba(&img, 50).dimensions(), (960, 540));
        // 极小图不越界
        let tiny = RgbaImage::new(1, 1);
        assert_eq!(resize_rgba(&tiny, 50).dimensions(), (1, 1));
    }

    #[test]
    fn pixel_per_point_positive() {
        assert!(pixel_per_point() >= 1.0);
    }
}
