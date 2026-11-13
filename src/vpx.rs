// Windows：RustDesk scrap 采集（DXGI）+ VP8 编码（libvpx）。
// macOS：scrap quartz（CoreGraphics）采集 + 同一 VP8 编码路径。
// 仅在 windows/macos 目标编译；linux 走 capture.rs 的 JPEG 路径。
#![cfg(any(target_os = "windows", target_os = "macos"))]

use anyhow::{Context, Result};
use scrap::{
    EncodeInput, Frame, TraitCapturer, TraitPixelBuffer, VpxEncoderConfig, VpxVideoCodecId,
};

/// 持续无编码输出多久才判「编码器卡死」（此前一律按故障处理，导致低码率丢帧误判回退 JPEG）。
const NO_OUTPUT_STALL_SECS: u64 = 5;

pub struct VpxStream {
    screen_id: usize,
    capturer: scrap::Capturer,
    encoder: Option<scrap::codec::Encoder>,
    /// 档位码率 kbps（按 1080P 标定；首帧按实际编码面积折算，见 quality_for）
    kbps: f32,
    /// 当前帧率（关键帧间隔固定 10 秒 ≈ 10×fps 帧）
    fps: u64,
    /// 编码输出尺寸（首帧按实际帧尺寸×scale 确定，恒为偶数）
    width: usize,
    height: usize,
    /// 分辨率档百分比（50/70/90/100）；<100 时逐帧重采样
    scale: u8,
    yuv: Vec<u8>,
    mid: Vec<u8>,
    ms: i64,
    /// 最近一次编码有输出的时刻（区分 libvpx 正常丢帧 vs 编码器卡死）
    no_output_since: std::time::Instant,
}

/// 一帧编码结果：(key, vp8 data)
pub struct EncodedFrame {
    pub key: bool,
    pub data: Vec<u8>,
}

/// 画质七档 → 档位码率（kbps，按 1080P 标定）：
/// 32K/128K/512K/1M/2M/8M/16M。实际送入编码器的码率再按
/// 实际分辨率与 1080P 的面积比折算（quality_for）。
pub fn tier_kbps(tier: u8) -> f32 {
    match tier {
        0 => 32.0,
        1 => 128.0,
        2 => 512.0,
        3 => 1024.0,
        4 => 2048.0,
        5 => 8192.0,
        _ => 16384.0,
    }
}

/// 实际码率 = 档位预设 × 实际编码面积 / 1080P 面积；
/// scrap 的 quality 字段是 ratio（rc_target_bitrate = ratio × base_bitrate(实际分辨率)），
/// 故 ratio = 目标 kbps ÷ base_bitrate(w,h)。
fn quality_for(kbps: f32, w: usize, h: usize) -> f32 {
    if w == 0 || h == 0 {
        return 1.0;
    }
    let target = kbps * (w * h) as f32 / (1920.0 * 1080.0);
    let base = scrap::codec::base_bitrate(w as u32, h as u32) as f32;
    if base > 0.0 {
        target / base
    } else {
        1.0
    }
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

/// I420 平面缩放：Y 全尺寸→(dw,dh)，U/V 各 (sw/2,sh/2)→(dw/2,dh/2)。
/// 先转后缩：转换走 libyuv SIMD，缩放只碰单通道平面（比 RGBA 4 通道缩放省约 3 倍数据量）。
fn resize_i420(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Result<Vec<u8>> {
    let y_len = sw * sh;
    let c_len = y_len / 4;
    if src.len() < y_len + 2 * c_len {
        anyhow::bail!("i420 buffer too small: {} < {}", src.len(), y_len + 2 * c_len);
    }
    let (cw, ch) = (sw / 2, sh / 2);
    let (dcw, dch) = (dw / 2, dh / 2);
    fn plane(bytes: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Result<Vec<u8>> {
        let img = image::GrayImage::from_raw(w as u32, h as u32, bytes.to_vec())
            .context("wrap i420 plane")?;
        let out =
            image::imageops::resize(&img, dw as u32, dh as u32, image::imageops::FilterType::Triangle);
        Ok(out.into_raw())
    }
    let y = plane(&src[..y_len], sw, sh, dw, dh)?;
    let u = plane(&src[y_len..y_len + c_len], cw, ch, dcw, dch)?;
    let v = plane(&src[y_len + c_len..y_len + 2 * c_len], cw, ch, dcw, dch)?;
    let mut out = Vec::with_capacity(dw * dh + dw * dh / 2);
    out.extend_from_slice(&y);
    out.extend_from_slice(&u);
    out.extend_from_slice(&v);
    Ok(out)
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
        kbps: f32,
        scale: u8,
        fps: u64,
    ) -> Result<Self> {
        // scrap master 的 Display::primary() 在 Windows 上是未实现的占位（恒 Err(NotFound)），
        // 必须用 all() 取第一块屏（与 rustdesk 本体一致）。
        let mut displays = scrap::Display::all().context("no display")?;
        if displays.is_empty() {
            anyhow::bail!("no display found");
        }
        let idx = screen.load(std::sync::atomic::Ordering::Relaxed).min(displays.len() - 1);
        let display = displays.swap_remove(idx);
        // 编码器延到首帧创建，输出尺寸以实际采集帧为准（见 encode_one）：
        // 既不依赖 display.width()/height()（其内部走 BackingScaleFactor），
        // 又保证编码尺寸与真实帧严格一致。
        let capturer = scrap::Capturer::new(display).context("capturer init")?;
        Ok(Self {
            screen_id: screen.load(std::sync::atomic::Ordering::Relaxed),
            capturer,
            encoder: None,
            kbps,
            fps,
            width: 0,
            height: 0,
            scale,
            yuv: Vec::new(),
            mid: Vec::new(),
            ms: 0,
            no_output_since: std::time::Instant::now(),
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// 热调画质档（档位切换）：按当前编码分辨率折算 ratio 后走 vpx_codec_enc_config_set，
    /// 不重建采集与编码器，画面不中断。
    pub fn set_tier(&mut self, kbps: f32) -> Result<()> {
        self.kbps = kbps;
        if let Some(enc) = self.encoder.as_mut() {
            let ratio = quality_for(self.kbps, self.width, self.height);
            enc.set_quality(ratio).context("vpx set_quality")?;
        }
        Ok(())
    }

    /// 抓取并编码一帧。无新帧时返回 Ok(None)。
    pub fn encode_one(&mut self) -> Result<Option<EncodedFrame>> {
        let frame = match self.capturer.frame(std::time::Duration::from_millis(50)) {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    // 暂无新帧：不计入「编码无输出」时长
                    self.no_output_since = std::time::Instant::now();
                    return Ok(None);
                }
                return Err(e).context("capture frame");
            }
        };
        // 源尺寸以实际帧为准（不依赖 display 元数据）
        let (sw, sh) = match &frame {
            Frame::PixelBuffer(pb) => (pb.width(), pb.height()),
            _ => anyhow::bail!("unsupported frame type (need PixelBuffer)"),
        };
        // 首帧创建编码器：输出尺寸 = 实际帧按 scale 缩放
        if self.encoder.is_none() {
            let (w, h) = scaled_dims(sw, sh, self.scale);
            self.width = w;
            self.height = h;
            let cfg = scrap::codec::EncoderCfg::VPX(VpxEncoderConfig {
                width: w as _,
                height: h as _,
                quality: quality_for(self.kbps, self.width, self.height),
                codec: VpxVideoCodecId::VP8,
                keyframe_interval: Some(((10 * self.fps).max(1)) as usize), // 10 秒一个关键帧（按帧率折算）
            });
            self.encoder =
                Some(scrap::codec::Encoder::new(cfg, false).context("vpx encoder init")?);
            let ratio = quality_for(self.kbps, w, h);
            tracing::info!(
                "vpx encoder {w}x{h} tier={}kbps → target≈{:.0}kbps (ratio {ratio:.4})",
                self.kbps,
                ratio * scrap::codec::base_bitrate(w as u32, h as u32) as f32
            );
        }
        // 先转 I420（libyuv SIMD），尺寸不一致（scale<100）再对 Y/U/V 平面缩放
        let input = frame
            .to(yuv_format(sw, sh), &mut self.yuv, &mut self.mid)
            .context("yuv convert")?;
        let full = input.yuv().context("yuv input")?;
        let resized = if (sw, sh) != (self.width, self.height) {
            Some(resize_i420(full, sw, sh, self.width, self.height)?)
        } else {
            None
        };
        let yuv: &[u8] = resized.as_deref().unwrap_or(full);
        self.ms = (self.ms + 1000 / 15) % (1 << 30);
        let video_frame = match self
            .encoder
            .as_mut()
            .expect("encoder initialized")
            .encode_to_message(EncodeInput::YUV(yuv), self.ms)
        {
            Ok(m) => {
                self.no_output_since = std::time::Instant::now();
                m
            }
            Err(e) => {
                // libvpx 在低码率/关键帧欠账时按 rc_dropframe_thresh 丢帧，
                // 单次无输出（"no valid frame"）是正常现象；仅持续 NO_OUTPUT_STALL_SECS
                // 秒拿不到输出才视为编码器卡死，交给上层 5 次错误回退 JPEG。
                let no_output = e.to_string().contains("no valid frame");
                if no_output
                    && self.no_output_since.elapsed()
                        <= std::time::Duration::from_secs(NO_OUTPUT_STALL_SECS)
                {
                    return Ok(None);
                }
                return Err(e).context("vpx encode");
            }
        };
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
