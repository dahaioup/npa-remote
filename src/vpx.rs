// Windows：RustDesk scrap 采集（DXGI）+ VP8 编码（libvpx）。
// 仅在 windows 目标编译；其它平台走 capture.rs 的 JPEG 路径。
#![cfg(target_os = "windows")]

use anyhow::{Context, Result};
use scrap::codec::EncoderApi;
use scrap::{EncodeInput, EncodeYuvFormat, Pixfmt, TraitCapturer, VpxEncoderConfig, VpxVideoCodecId};

pub struct VpxStream {
    screen_id: usize,
    capturer: scrap::Capturer,
    encoder: scrap::codec::Encoder,
    width: usize,
    height: usize,
    yuv: Vec<u8>,
    mid: Vec<u8>,
    ms: i64,
}

/// 一帧编码结果：(key, vp8 data)
pub struct EncodedFrame {
    pub key: bool,
    pub data: Vec<u8>,
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
    pub fn new(screen: &'static std::sync::atomic::AtomicUsize) -> Result<Self> {
        // scrap master 的 Display::primary() 在 Windows 上是未实现的占位（恒 Err(NotFound)），
        // 必须用 all() 取第一块屏（与 rustdesk 本体一致）。
        let mut displays = scrap::Display::all().context("no display")?;
        if displays.is_empty() {
            anyhow::bail!("no display found");
        }
        let idx = screen.load(std::sync::atomic::Ordering::Relaxed).min(displays.len() - 1);
        let display = displays.swap_remove(idx);
        let width = display.width();
        let height = display.height();
        let capturer = scrap::Capturer::new(display).context("capturer init")?;
        let cfg = scrap::codec::EncoderCfg::VPX(VpxEncoderConfig {
            width: width as _,
            height: height as _,
            quality: 60.0,
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
        use scrap::EncodeInput;
        let frame = match self.capturer.frame(std::time::Duration::from_millis(50)) {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(None);
                }
                return Err(e).context("capture frame");
            }
        };
        let input = frame
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
