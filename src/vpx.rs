// Windows：RustDesk scrap 采集（DXGI）+ VP8 编码（libvpx）。
// 仅在 windows 目标编译；其它平台走 capture.rs 的 JPEG 路径。
#![cfg(target_os = "windows")]

use anyhow::{Context, Result};
use scrap::codec::EncoderApi;
use scrap::{
    EncodeInput, EncodeYuvFormat, Frame, PixelBuffer, Pixfmt, TraitCapturer, TraitPixelBuffer,
    VpxEncoderConfig, VpxVideoCodecId,
};

pub struct VpxStream {
    screen_id: usize,
    capturer: scrap::Capturer,
    encoder: scrap::codec::Encoder,
    /// 编码输出尺寸（已按 scale 缩放，恒为偶数）
    width: usize,
    height: usize,
    /// 分辨率档百分比（50/70/90/100）；<100 时逐帧重采样
    scale: u8,
    yuv: Vec<u8>,
    mid: Vec<u8>,
    ms: i64,
}

/// 一帧编码结果：(key, vp8 data)
pub struct EncodedFrame {
    pub key: bool,
    pub data: Vec<u8>,
}

/// 画质四档 → VP8 码率比（quality 字段 = bitrate ratio）。
/// 按 1080P 基准 2073 kbps：256K/512K/1M/2M。
pub fn tier_ratio(tier: u8) -> f32 {
    let kbps = match tier {
        0 => 256.0,
        1 => 512.0,
        2 => 1024.0,
        _ => 2048.0,
    };
    kbps / 2073.0
}

/// 缩放尺寸：向下取偶（I420 需要 w/2、h/2 整除），最小 2。
fn scaled_dims(w: usize, h: usize, pct: u8) -> (usize, usize) {
    if pct >= 100 {
        return (w, h);
    }
    let sw = (w * pct as usize / 100) & !1;
    let sh = (h * pct as usize / 100) & !1;
    (sw.max(2), sh.max(2))
}

/// 按 stride 重排为连续像素后重采样到 (dw,dh)（通道顺序不变，插值逐通道独立）。
fn repack_resize(data: &[u8], sw: usize, sh: usize, stride0: usize, dw: usize, dh: usize) -> Result<Vec<u8>> {
    let row = sw * 4;
    let mut v = Vec::with_capacity(row * sh);
    for y in 0..sh {
        let s = y * stride0;
        if s + row > data.len() {
            anyhow::bail!("frame stride out of range");
        }
        v.extend_from_slice(&data[s..s + row]);
    }
    let img = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(sw as u32, sh as u32, v)
        .context("wrap frame for resize")?;
    let out = image::imageops::resize(&img, dw as u32, dh as u32, image::imageops::FilterType::Triangle);
    Ok(out.into_raw())
}

fn yuv_format(w: usize, h: usize) -> scrap::EncodeYuvFormat {
    scrap::EncodeYuvFormat {
        pixfmt: scrap::Pixfmt::I420,
        w,
        h,
        stride: vec![w, w / 2, w / 2],
        u: w * h,
        v: w * h + w * h / 4,
    }
}

impl VpxStream {
    pub fn new(
        screen: &'static std::sync::atomic::AtomicUsize,
        ratio: f32,
        scale: u8,
    ) -> Result<Self> {
        // scrap master 的 Display::primary() 在 Windows 上是未实现的占位（恒 Err(NotFound)），
        // 必须用 all() 取第一块屏（与 rustdesk 本体一致）。
        let mut displays = scrap::Display::all().context("no display")?;
        if displays.is_empty() {
            anyhow::bail!("no display found");
        }
        let idx = screen.load(std::sync::atomic::Ordering::Relaxed).min(displays.len() - 1);
        let display = displays.swap_remove(idx);
        // 采集恒为全分辨率，编码输出按 scale 缩放（encode_one 内重采样）
        let (width, height) = scaled_dims(display.width(), display.height(), scale);
        let capturer = scrap::Capturer::new(display).context("capturer init")?;
        let cfg = scrap::codec::EncoderCfg::VPX(VpxEncoderConfig {
            width: width as _,
            height: height as _,
            quality: ratio,
            codec: VpxVideoCodecId::VP8,
            keyframe_interval: Some(300),
        });
        let encoder = scrap::codec::Encoder::new(cfg, false).context("vpx encoder init")?;
        Ok(Self {
            screen_id: screen.load(std::sync::atomic::Ordering::Relaxed),
            capturer,
            encoder,
            width,
            height,
            scale,
            yuv: Vec::new(),
            mid: Vec::new(),
            ms: 0,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// 抓取并编码一帧。无新帧时返回 Ok(None)。
    pub fn encode_one(&mut self) -> Result<Option<EncodedFrame>> {
        let frame = match self.capturer.frame(std::time::Duration::from_millis(50)) {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(e).context("capture frame");
            }
        };
        // 分辨率缩放：scale<100 时取原始像素重采样后重新包装成帧
        let mut resized: Vec<u8> = Vec::new();
        let scaled_frame;
        let active: &Frame;
        if self.scale < 100 {
            let (sw, sh, stride0, pixfmt, src) = match &frame {
                Frame::PixelBuffer(pb) => {
                    let w = pb.width();
                    let stride = pb.stride().first().copied().unwrap_or(w * 4);
                    (w, pb.height(), stride, pb.pixfmt(), pb.data().to_vec())
                }
                _ => anyhow::bail!("unsupported frame type for scaling (need PixelBuffer)"),
            };
            resized = repack_resize(&src, sw, sh, stride0, self.width, self.height)?;
            scaled_frame =
                Frame::PixelBuffer(PixelBuffer::new(&resized, pixfmt, self.width, self.height));
            active = &scaled_frame;
        } else {
            active = &frame;
        }
        let input = active
            .to(
                yuv_format(self.width, self.height),
                &mut self.yuv,
                &mut self.mid,
            )
            .context("yuv convert")?;
        let yuv = input.yuv().context("yuv input")?;
        self.ms = (self.ms + 1000 / 15) % (1 << 30);
        let video_frame = self
            .encoder
            .encode_to_message(EncodeInput::YUV(yuv), self.ms)
            .context("vpx encode")?;
        // 从 protobuf VideoFrame 提取 vp8s 帧列表
        if let Some(base::message_proto::video_frame::Union::Vp8s(frames)) = video_frame.union {
            // 返回最后一个（最完整）帧；VP8 key/delta 都是独立可解码单元
            if let Some(last) = frames.frames.last() {
                return Ok(Some(EncodedFrame {
                    key: last.key,
                    data: last.data.to_vec(),
                }));
            }
        }
        Ok(None)
    }
}
