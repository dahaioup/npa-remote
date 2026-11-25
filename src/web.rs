// 独立模式：HTTP 静态页 + WS 控制通道。
// 页面（内嵌 index.html）连 /ws：
//   - 服务端每 interval 推一帧二进制 JPEG
//   - 客户端发 JSON 文本：{t:"move"|"click"|"type"|"hotkey"|"scroll"|"clip_get"|"clip_set"|"quality"|"scale"|"fps"|"screen"|"pos"|"ping", ...}
//   - 所有文本响应都回带 t=请求类型（前端据此分发），ping 回带 ts+server_ts 供估时钟偏移
//   - 二进制帧 framing：[type][key][len u32 LE][capture_ts u64 LE][data]，capture_ts=服务端采集时刻（画面延迟=上屏-采集）
use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use serde_json::Value;
use tokio::sync::mpsc;

const INDEX: &str = include_str!("index.html");
const DEFAULT_FPS: u64 = 4;
const MAX_FPS: u64 = 30;

/// 画质七档（0=极简 32K 1=省流 128K 2=标清 512K 3=流畅 1M 4=清晰 2M 5=高清 8M 6=超清 16M），
/// 默认流畅（1M@1080P）。
const MAX_TIER: u8 = 6;
/// 分辨率四档百分比（50/70/90/100），默认 100。
const SCALE_STEPS: [u8; 4] = [50, 70, 90, 100];
/// 屏幕下拉「无」：SCREEN_SEL 存 usize::MAX = 停止推流（连接与控制保持）。
const NO_SCREEN: usize = usize::MAX;
/// 画质档名（与 index.html 下拉一一对应）。
const TIER_LABELS: [&str; 7] = ["极简", "省流", "标清", "流畅", "清晰", "高清", "超清"];

/// 会话共享配置：控制消息热改，帧任务热读。
struct StreamCfg {
    tier: std::sync::atomic::AtomicU8,
    scale: std::sync::atomic::AtomicU8, // 百分比 50/70/90/100
    fps: std::sync::atomic::AtomicU64,
    codec: std::sync::atomic::AtomicU8, // 0=自动 1=VP8 2=JPEG
}

impl StreamCfg {
    fn new() -> Self {
        Self {
            tier: std::sync::atomic::AtomicU8::new(3), // 默认流畅 1M
            scale: std::sync::atomic::AtomicU8::new(100),
            fps: std::sync::atomic::AtomicU64::new(DEFAULT_FPS),
            codec: std::sync::atomic::AtomicU8::new(CODEC_AUTO),
        }
    }
    fn tier(&self) -> u8 {
        self.tier.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn scale(&self) -> u8 {
        self.scale.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn fps(&self) -> u64 {
        self.fps.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn codec(&self) -> u8 {
        self.codec.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// 把帧坐标（缩放后帧像素）映射到 enigo 逻辑坐标。
/// 帧→物理（乘 100/scale），物理→逻辑（mac Retina 除以 pixel_per_point）。
fn map_frame_to_enigo(fx: i64, fy: i64, scale_pct: u8) -> (i64, i64) {
    let inv = 100.0 / (scale_pct as f64).max(1.0);
    let d = crate::capture::pixel_per_point().max(1.0);
    (
        ((fx as f64) * inv / d).round() as i64,
        ((fy as f64) * inv / d).round() as i64,
    )
}

pub async fn serve(bind: &str, port: u16) -> Result<()> {
    let addr = format!("{bind}:{port}");
    let app = Router::new()
        .route("/", get(index))
        .route("/api/info", get(api_info))
        .route("/ws", get(ws_upgrade));
    // HTTPS（自签证书）：WebCodecs 需要 secure context，局域网裸 HTTP 会拿不到解码器。
    // 证书持久化在 exe 同目录，已有则复用（指纹稳定，浏览器信任一次即可）。
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let cert_path = exe_dir.join("npa-remote-cert.pem");
    let key_path = exe_dir.join("npa-remote-key.pem");
    if !cert_path.exists() || !key_path.exists() {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "npa-remote".into()])?;
        std::fs::write(&cert_path, ck.cert.pem())?;
        std::fs::write(&key_path, ck.key_pair.serialize_pem())?;
        tracing::info!("generated self-signed certificate at {}", cert_path.display());
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path)
        .await
        .map_err(|e| anyhow::anyhow!("load tls config: {e}"))?;
    tracing::info!("remote plugin listening on https://{addr} (self-signed)");
    axum_server::bind_rustls(addr.parse()?, rustls_cfg)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

/// GET /api/info — 返回本机 IP 列表 + 版本号（前端标题栏显示用）
async fn api_info() -> axum::response::Json<serde_json::Value> {
    let ips: Vec<String> = local_ip_address::list_afinet_netifas()
        .map(|v| {
            v.into_iter()
                .filter(|(_, ip)| ip.is_ipv4() && !ip.is_loopback())
                .map(|(_, ip)| ip.to_string())
                .collect()
        })
        .unwrap_or_default();
    axum::response::Json(serde_json::json!({
        "ip": ips.first().cloned().unwrap_or_default(),
        "ips": ips,
        "version": env!("CARGO_PKG_VERSION"),
        // 抓到的画面里是否已经带鼠标指针。前端据此决定要不要自己画一个光标：
        // X11 抓的是 framebuffer，光标是 X server 单独画的 sprite → 不在里面；
        // macOS CGDisplay 抓帧也不含光标；Windows DXGI 与 Wayland 合成器输出含光标。
        "cursor_in_frame": cursor_in_frame(),
    }))
}

/// 抓帧里是否已经包含鼠标指针（见 api_info 注释）。
#[cfg(target_os = "windows")]
fn cursor_in_frame() -> bool {
    // DXGI Desktop Duplication 默认把光标烧进帧里
    true
}

#[cfg(target_os = "macos")]
fn cursor_in_frame() -> bool {
    // CGDisplayCreateImage 抓的是显示缓冲区，光标不在里面
    false
}

#[cfg(target_os = "linux")]
fn cursor_in_frame() -> bool {
    // X11：framebuffer 里没有光标（X server 用独立 sprite 画）
    // Wayland：合成器输出的是最终画面，带光标
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn cursor_in_frame() -> bool {
    false
}

async fn ws_upgrade(ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    // 帧发送任务：capture → jpeg/vpx → binary message；tier/scale/fps 由控制消息热调
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
    let cfg = std::sync::Arc::new(StreamCfg::new());
    {
        let tx = tx.clone();
        let cfg = cfg.clone();
        tokio::spawn(async move {
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            {
                stream_vpx(tx, cfg).await;
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            loop {
                // 屏幕=「无」：停止推流（不抓不编码），连接与控制保持
                if crate::capture::SCREEN_SEL.load(std::sync::atomic::Ordering::Relaxed) == NO_SCREEN {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
                let tier = cfg.tier();
                let scale = cfg.scale();
                let fps = cfg.fps();
                let cap_ts = now_ms();
                // 捕获是阻塞调用，丢到 blocking 线程
                let frame = tokio::task::spawn_blocking(move || {
                    let t0 = std::time::Instant::now();
                    let img = crate::capture::grab(&Default::default())?;
                    let tg = t0.elapsed();
                    let t1 = std::time::Instant::now();
                    let img = crate::capture::resize_rgba(&img, scale);
                    let jpeg =
                        crate::capture::encode_jpeg(&img, crate::capture::tier_jpeg_quality(tier))?;
                    let te = t1.elapsed();
                    tracing::debug!(
                        "frame {}x{} grab={tg:?} encode={te:?} {}B",
                        img.width(),
                        img.height(),
                        jpeg.len()
                    );
                    Ok::<_, anyhow::Error>(jpeg)
                })
                .await;
                match frame {
                    Ok(Ok(jpeg)) => {
                        // 帧 framing：[type=1 jpeg][key][len u32 LE][capture_ts u64 LE][data]
                        let mut msg = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                        let n = jpeg.len() as u32;
                        msg[2..6].copy_from_slice(&n.to_le_bytes());
                        msg[6..14].copy_from_slice(&cap_ts.to_le_bytes());
                        msg.extend_from_slice(&jpeg);
                        if tx.send(msg).await.is_err() {
                            return;
                        }
                    }
                    _ => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                }
                let interval = std::time::Duration::from_millis(1000 / fps.max(1));
                tokio::time::sleep(interval).await;
            }
        });
    }

    loop {
        tokio::select! {
            frame = rx.recv() => {
                match frame {
                    Some(jpeg) => {
                        if socket.send(Message::Binary(jpeg.into())).await.is_err() { break; }
                    }
                    None => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let v: Value = match serde_json::from_str(&text) { Ok(v) => v, Err(_) => continue };
                        let resp = handle_control(&v, &cfg).await;
                        let _ = socket.send(Message::Text(resp.into())).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

async fn handle_control(v: &Value, cfg: &StreamCfg) -> String {
    let t = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
    let r: Result<Value, String> = async {
        match t {
            "move" => {
                let (x, y) = xy(v)?;
                let (ex, ey) = map_frame_to_enigo(x, y, cfg.scale());
                crate::input::mouse_move(ex, ey).map_err(|e| e.to_string())?;
                Ok(json_ok("moved"))
            }
            "click" => {
                let button = v.get("button").and_then(|x| x.as_str()).unwrap_or("left");
                let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                // 先把光标移到目标坐标再点击（否则落点=当前光标位置，通常在左上角）
                if let Ok((x, y)) = xy(v) {
                    let (ex, ey) = map_frame_to_enigo(x, y, cfg.scale());
                    crate::input::mouse_move(ex, ey).map_err(|e| e.to_string())?;
                }
                crate::input::mouse_click(button, count).map_err(|e| e.to_string())?;
                Ok(json_ok("clicked"))
            }
            "down" => {
                // 按下不放：拖选/拖窗口的前半段（先移坐标再按）
                let button = v.get("button").and_then(|x| x.as_str()).unwrap_or("left");
                if let Ok((x, y)) = xy(v) {
                    let (ex, ey) = map_frame_to_enigo(x, y, cfg.scale());
                    crate::input::mouse_move(ex, ey).map_err(|e| e.to_string())?;
                }
                // 带修饰键的点击（ctrl+click 多选、shift+click 连选）
                let mods = mods_of(v);
                if !mods.is_empty() {
                    crate::input::hold_modifiers(&mods).map_err(|e| e.to_string())?;
                }
                crate::input::mouse_down(button).map_err(|e| e.to_string())?;
                Ok(json_ok("down"))
            }
            "up" => {
                // 抬起：拖选/拖窗口的后半段（x/y 可能为 -1=mouseleave 兜底，只抬不移）
                let button = v.get("button").and_then(|x| x.as_str()).unwrap_or("left");
                if let Ok((x, y)) = xy(v) {
                    if x >= 0 && y >= 0 {
                        let (ex, ey) = map_frame_to_enigo(x, y, cfg.scale());
                        crate::input::mouse_move(ex, ey).map_err(|e| e.to_string())?;
                    }
                }
                crate::input::mouse_up(button).map_err(|e| e.to_string())?;
                let mods = mods_of(v);
                if !mods.is_empty() {
                    crate::input::release_modifiers(&mods).map_err(|e| e.to_string())?;
                }
                Ok(json_ok("up"))
            }
            "key" => {
                // 物理按键（输入法友好）：浏览器传 KeyboardEvent.code
                let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
                if code.is_empty() {
                    return Err("code required".to_string());
                }
                let shift = v.get("shift").and_then(|x| x.as_bool()).unwrap_or(false);
                let ch = v
                    .get("ch")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.chars().next());
                crate::input::press_code(code, shift, ch).map_err(|e| e.to_string())?;
                Ok(json_ok("key"))
            }
            "type" => {
                let text = v.get("text").and_then(|x| x.as_str()).ok_or("text required")?;
                crate::input::type_text(text).map_err(|e| e.to_string())?;
                Ok(json_ok("typed"))
            }
            "hotkey" => {
                let combo = v.get("combo").and_then(|x| x.as_str()).ok_or("combo required")?;
                crate::input::hotkey(combo).map_err(|e| e.to_string())?;
                Ok(json_ok("hotkey"))
            }
            "ime" => {
                crate::input::toggle_ime().map_err(|e| e.to_string())?;
                Ok(json_ok("ime"))
            }
            "scroll" => {
                let dy = v.get("dy").and_then(|x| x.as_i64()).unwrap_or(0);
                // 先移到坐标（滚轮落点跟随鼠标位置）
                if let Ok((x, y)) = xy(v) {
                    let (ex, ey) = map_frame_to_enigo(x, y, cfg.scale());
                    crate::input::mouse_move(ex, ey).map_err(|e| e.to_string())?;
                }
                crate::input::mouse_scroll(-dy).map_err(|e| e.to_string())?; // 页面滚轮正=向下
                Ok(json_ok("scrolled"))
            }
            "pos" => {
                let (x, y) = crate::input::mouse_pos().unwrap_or((0, 0));
                let (w, h) = crate::capture::screen_size().unwrap_or((0, 0));
                let screens: Vec<serde_json::Value> = crate::capture::screen_list()
                    .iter()
                    .map(|(id, sw, sh)| serde_json::json!({"id": id, "w": sw, "h": sh}))
                    .collect();
                let sel = crate::capture::SCREEN_SEL.load(std::sync::atomic::Ordering::Relaxed);
                // 输入被接管（Synergy/AweSun 等共享键鼠）时把嫌疑进程报给前端。
                #[cfg(target_os = "windows")]
                let grabbers = crate::wininput::running_input_grabbers();
                #[cfg(not(target_os = "windows"))]
                let grabbers: Vec<String> = Vec::new();
                Ok(serde_json::json!({"ok":true,"x":x,"y":y,"screen_w":w,"screen_h":h,
                    "screen": if sel == NO_SCREEN { -1 } else { sel as i64 },
                    "screens": screens,
                    "input_grabbers": grabbers}))
            }
            "screen" => {
                // id<0（下拉「无」）→ NO_SCREEN：停止推流，连接与控制保持
                let id = v.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
                let sel = if id < 0 { NO_SCREEN } else { id as usize };
                crate::capture::SCREEN_SEL.store(sel, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"screen": if sel == NO_SCREEN { -1 } else { sel as i64 }}))
            }
            "ping" => Ok(serde_json::json!({"ok":true,"ts": v.get("ts").cloned().unwrap_or(Value::Null), "server_ts": now_ms()})),
            "clip_get" => Ok(serde_json::json!({"ok":true,"text":crate::input::clipboard_get().map_err(|e| e.to_string())?})),
            "clip_set" => {
                let text = v.get("text").and_then(|x| x.as_str()).ok_or("text required")?;
                crate::input::clipboard_set(text).map_err(|e| e.to_string())?;
                Ok(json_ok("clip"))
            }
            "quality" => {
                let tier = v.get("v").and_then(|x| x.as_u64()).unwrap_or(3).min(MAX_TIER as u64) as u8;
                cfg.tier.store(tier, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"tier":tier,"label":TIER_LABELS[tier as usize],
                    "jpeg_quality": crate::capture::tier_jpeg_quality(tier)}))
            }
            "scale" => {
                let raw = v.get("v").and_then(|x| x.as_u64()).unwrap_or(100) as u8;
                // 吸附到最近的合法档
                let scale = *SCALE_STEPS
                    .iter()
                    .min_by_key(|s| (**s as i16 - raw as i16).abs())
                    .unwrap_or(&100);
                cfg.scale.store(scale, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"scale":scale}))
            }
            "fps" => {
                let fps = v.get("v").and_then(|x| x.as_u64()).unwrap_or(DEFAULT_FPS).clamp(1, MAX_FPS);
                cfg.fps.store(fps, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"fps":fps}))
            }
            "codec" => {
                // 编码切换：0=自动（VP8 优先，失败降级 JPEG）1=强制 VP8 2=强制 JPEG
                let codec = v.get("v").and_then(|x| x.as_u64()).unwrap_or(0).min(2) as u8;
                cfg.codec.store(codec, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"codec":codec,"label":CODEC_LABELS[codec as usize]}))
            }
            _ => Err(format!("unknown type: {t}")),
        }
    }
    .await;
    // 所有响应回带 t=请求类型，前端据此分发（pos/clip/ping 等）
    let mut val = match r {
        Ok(v) => v,
        Err(e) => serde_json::json!({"ok":false,"error":e}),
    };
    if let Value::Object(m) = &mut val {
        m.insert("t".into(), Value::String(t.into()));
    }
    val.to_string()
}

fn xy(v: &Value) -> Result<(i64, i64), String> {
    let x = v.get("x").and_then(|x| x.as_i64()).ok_or("x required")?;
    let y = v.get("y").and_then(|x| x.as_i64()).ok_or("y required")?;
    Ok((x, y))
}

fn json_ok(status: &str) -> Value {
    serde_json::json!({"ok":true,"status":status})
}

/// 鼠标消息里带的修饰键（ctrl/shift/alt/win），用于 ctrl+click 之类。
fn mods_of(v: &Value) -> Vec<String> {
    v.get("mods")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 服务端毫秒时间戳（帧头采集时刻 + ping 时钟同步用，画面延迟=上屏-采集）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 编码选择：0=自动（VP8 优先，失败降级 JPEG）、1=强制 VP8、2=强制 JPEG。
const CODEC_AUTO: u8 = 0;
const CODEC_VP8: u8 = 1;
const CODEC_JPEG: u8 = 2;
const CODEC_LABELS: [&str; 3] = ["自动", "VP8", "JPEG"];

/// Windows/macOS/Linux：DXGI/Quartz/X11 采集 + VP8（RustDesk scrap），帧 framing [type=2][key][len LE][data]。
/// VP8 初始化或连续编码失败时回退 JPEG，保证浏览器始终有画面。
/// 编码可在前端切换（自动 / 强制 VP8 / 强制 JPEG）；JPEG 与 VP8 共用同一个
/// scrap 采集源（无人值守机 xcap 抓不到画面时，scrap 的根窗口兜底能抓到）。
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
async fn stream_vpx(tx: mpsc::Sender<Vec<u8>>, cfg: std::sync::Arc<StreamCfg>) {
    let handle = tokio::task::spawn_blocking(move || {
        let sel = &crate::capture::SCREEN_SEL;
        let mut last_screen = sel.load(std::sync::atomic::Ordering::Relaxed);
        let mut last_tier = cfg.tier();
        let mut last_scale = cfg.scale();
        let mut last_fps = cfg.fps();
        let mut last_codec = cfg.codec();
        // JPEG 回退态下定期重试 VP8（自愈）
        let mut last_retry = std::time::Instant::now();
        let mut stream = match crate::vpx::VpxStream::new(
            sel,
            crate::vpx::tier_kbps(last_tier),
            last_scale,
            last_fps,
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("vpx init failed, falling back to jpeg: {e:#}");
                None
            }
        };
        let mut vpx_errs: u32 = 0;
        loop {
            // 多屏/画质/分辨率/帧率任一变化 → 重建采集+编码器
            let cur_screen = sel.load(std::sync::atomic::Ordering::Relaxed);
            if cur_screen == NO_SCREEN {
                // 屏幕=「无」：停止推流（不抓不编码），连接与控制保持
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
            let cur_tier = cfg.tier();
            let cur_scale = cfg.scale();
            let cur_fps = cfg.fps();
            let cur_codec = cfg.codec();
            if cur_codec != last_codec {
                last_codec = cur_codec;
                tracing::info!("codec switched to {}", CODEC_LABELS[cur_codec as usize]);
            }
            if cur_screen != last_screen || cur_scale != last_scale || cur_fps != last_fps {
                // 屏幕/分辨率/帧率变化 → 重建采集+编码器（帧率影响关键帧间隔 10×fps）
                last_screen = cur_screen;
                last_tier = cur_tier;
                last_scale = cur_scale;
                last_fps = cur_fps;
                stream = crate::vpx::VpxStream::new(
                    sel,
                    crate::vpx::tier_kbps(last_tier),
                    last_scale,
                    last_fps,
                )
                    .map(Some)
                    .unwrap_or_else(|e| {
                        tracing::error!("vpx rescreen/reconfig failed: {e:#}");
                        None
                    });
            } else if cur_tier != last_tier {
                // 仅画质档变化 → 热调码率（不重建采集，画面不中断）
                last_tier = cur_tier;
                if let Some(st) = stream.as_mut() {
                    if let Err(e) = st.set_tier(crate::vpx::tier_kbps(cur_tier)) {
                        tracing::error!("vpx set_quality failed: {e:#}, rebuilding");
                        stream = crate::vpx::VpxStream::new(
                            sel,
                            crate::vpx::tier_kbps(cur_tier),
                            last_scale,
                            last_fps,
                        )
                        .map(Some)
                        .unwrap_or_else(|e| {
                            tracing::error!("vpx rebuild failed: {e:#}");
                            None
                        });
                    }
                }
            }
            // 编码器选择：强制 JPEG 直走 JPEG；自动模式 VP8 连续失败 5 次后进
            // JPEG 冷却（15s 后自动重试 VP8）；强制 VP8 失败也不降级。
            let force_jpeg = cur_codec == CODEC_JPEG;
            let force_vp8 = cur_codec == CODEC_VP8;
            let vpx_cooling = cur_codec == CODEC_AUTO && vpx_errs >= 5;
            let want_vp8 = !force_jpeg && !vpx_cooling;
            if want_vp8 {
                if let Some(st) = stream.as_mut() {
                    let cap_ts = now_ms();
                    match st.encode_one() {
                        Ok(Some(f)) => {
                            vpx_errs = 0;
                            // 帧 framing：[type=2 vp8][key][len u32 LE][capture_ts u64 LE][data]
                            let mut msg = vec![2u8, f.key as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                            let n = f.data.len() as u32;
                            msg[2..6].copy_from_slice(&n.to_le_bytes());
                            msg[6..14].copy_from_slice(&cap_ts.to_le_bytes());
                            msg.extend_from_slice(&f.data);
                            if tx.blocking_send(msg).is_err() {
                                return;
                            }
                            let fps = cfg.fps();
                            let interval = if fps >= 30 { 1 } else { 1000 / fps.max(1) };
                            std::thread::sleep(std::time::Duration::from_millis(interval.max(1)));
                            continue;
                        }
                        Ok(None) => {
                            std::thread::sleep(std::time::Duration::from_millis(15));
                            continue;
                        }
                        Err(e) => {
                            vpx_errs += 1;
                            tracing::error!("vpx encode failed ({vpx_errs}): {e:#}");
                            if force_vp8 {
                                // 强制 VP8：不降级，短暂退避后重试
                                std::thread::sleep(std::time::Duration::from_millis(200));
                                continue;
                            }
                            if vpx_errs < 5 {
                                std::thread::sleep(std::time::Duration::from_millis(200));
                                continue;
                            }
                            // 自动模式：连续失败 → 进入 JPEG 冷却（下面走 JPEG 分支）
                            tracing::error!("vpx disabled, falling back to jpeg");
                            last_retry = std::time::Instant::now();
                            continue;
                        }
                    }
                }
            }
            // JPEG 路径：优先用与 VP8 同一个 scrap 采集源（与 VP8 同源同屏），
            // 拿不到再用 xcap 兜底。
            if let Some(st) = stream.as_mut() {
                match st.encode_one_jpeg(crate::capture::tier_jpeg_quality(cur_tier)) {
                    Ok(Some(jpeg)) => {
                        let cap_ts = now_ms();
                        let mut msg = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                        let n = jpeg.len() as u32;
                        msg[2..6].copy_from_slice(&n.to_le_bytes());
                        msg[6..14].copy_from_slice(&cap_ts.to_le_bytes());
                        msg.extend_from_slice(&jpeg);
                        if tx.blocking_send(msg).is_err() {
                            return;
                        }
                        let fps = cfg.fps();
                        let interval = if fps >= 30 { 33 } else { 1000 / fps.max(1) };
                        std::thread::sleep(std::time::Duration::from_millis(interval.max(1)));
                        continue;
                    }
                    Ok(None) => {
                        std::thread::sleep(std::time::Duration::from_millis(15));
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("jpeg encode failed: {e:#}");
                    }
                }
            }
            // 没有可用采集源 → 定期重建（VP8/JPEG 都能用上）
            if stream.is_none() && last_retry.elapsed() >= std::time::Duration::from_secs(15) {
                last_retry = std::time::Instant::now();
                match crate::vpx::VpxStream::new(
                    sel,
                    crate::vpx::tier_kbps(cfg.tier()),
                    cfg.scale(),
                    cfg.fps(),
                ) {
                    Ok(s) => {
                        tracing::info!("capture source recovered");
                        stream = Some(s);
                        vpx_errs = 0;
                        continue;
                    }
                    Err(e) => tracing::warn!("capture source retry failed: {e:#}"),
                }
            }
            // xcap JPEG 兜底（跟随 tier/scale 热调）
            let tier = cfg.tier();
            let scale = cfg.scale();
            let fps = cfg.fps();
            let cap_ts = now_ms();
            match crate::capture::grab(&Default::default())
                .map(|f| crate::capture::resize_rgba(&f, scale))
                .and_then(|f| crate::capture::encode_jpeg(&f, crate::capture::tier_jpeg_quality(tier)))
            {
                Ok(jpeg) => {
                    let mut msg = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                    let n = jpeg.len() as u32;
                    msg[2..6].copy_from_slice(&n.to_le_bytes());
                    msg[6..14].copy_from_slice(&cap_ts.to_le_bytes());
                    msg.extend_from_slice(&jpeg);
                    if tx.blocking_send(msg).is_err() {
                        return;
                    }
                    let interval = if fps >= 30 { 200 } else { (1000 / fps.max(1)).max(200) };
                    std::thread::sleep(std::time::Duration::from_millis(interval));
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
            }
        }
    });
    let _ = handle.await;
}
