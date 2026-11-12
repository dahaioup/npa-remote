// 独立模式：HTTP 静态页 + WS 控制通道。
// 页面（内嵌 index.html）连 /ws：
//   - 服务端每 interval 推一帧二进制 JPEG
//   - 客户端发 JSON 文本：{t:"move"|"click"|"type"|"hotkey"|"scroll"|"clip_get"|"clip_set"|"quality"|"fps", ...}
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

/// 画质四档（0=流畅 1=清晰 2=高清 3=超清），默认流畅（256K@1080P）。
const MAX_TIER: u8 = 3;
/// 分辨率四档百分比（50/70/90/100），默认 100。
const SCALE_STEPS: [u8; 4] = [50, 70, 90, 100];

/// 会话共享配置：控制消息热改，帧任务热读。
struct StreamCfg {
    tier: std::sync::atomic::AtomicU8,
    scale: std::sync::atomic::AtomicU8, // 百分比 50/70/90/100
    fps: std::sync::atomic::AtomicU64,
}

impl StreamCfg {
    fn new() -> Self {
        Self {
            tier: std::sync::atomic::AtomicU8::new(0),
            scale: std::sync::atomic::AtomicU8::new(100),
            fps: std::sync::atomic::AtomicU64::new(DEFAULT_FPS),
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
            #[cfg(target_os = "windows")]
            {
                stream_vpx(tx, cfg).await;
            }
            #[cfg(not(target_os = "windows"))]
            loop {
                let tier = cfg.tier();
                let scale = cfg.scale();
                let fps = cfg.fps();
                // 捕获是阻塞调用，丢到 blocking 线程
                let frame = tokio::task::spawn_blocking(move || {
                    let img = crate::capture::grab(&Default::default())?;
                    let img = crate::capture::resize_rgba(&img, scale);
                    crate::capture::encode_jpeg(&img, crate::capture::tier_jpeg_quality(tier))
                })
                .await;
                match frame {
                    Ok(Ok(jpeg)) => {
                        // 帧 framing：[type=1 jpeg][key][len u32 LE][data]
                        let mut msg = vec![1u8, 0, 0, 0, 0, 0];
                        let n = jpeg.len() as u32;
                        msg[2..6].copy_from_slice(&n.to_le_bytes());
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
                Ok(serde_json::json!({"ok":true,"x":x,"y":y,"screen_w":w,"screen_h":h,
                    "screen": crate::capture::SCREEN_SEL.load(std::sync::atomic::Ordering::Relaxed),
                    "screens": screens}))
            }
            "screen" => {
                let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                crate::capture::SCREEN_SEL.store(id, std::sync::atomic::Ordering::Relaxed);
                Ok(serde_json::json!({"ok":true,"screen":id}))
            }
            "clip_get" => Ok(serde_json::json!({"ok":true,"text":crate::input::clipboard_get().map_err(|e| e.to_string())?})),
            "clip_set" => {
                let text = v.get("text").and_then(|x| x.as_str()).ok_or("text required")?;
                crate::input::clipboard_set(text).map_err(|e| e.to_string())?;
                Ok(json_ok("clip"))
            }
            "quality" => {
                let tier = v.get("v").and_then(|x| x.as_u64()).unwrap_or(0).min(MAX_TIER as u64) as u8;
                cfg.tier.store(tier, std::sync::atomic::Ordering::Relaxed);
                let labels = ["流畅", "清晰", "高清", "超清"];
                Ok(serde_json::json!({"ok":true,"tier":tier,"label":labels[tier as usize],
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
            _ => Err(format!("unknown type: {t}")),
        }
    }
    .await;
    match r {
        Ok(v) => v.to_string(),
        Err(e) => serde_json::json!({"ok":false,"error":e}).to_string(),
    }
}

fn xy(v: &Value) -> Result<(i64, i64), String> {
    let x = v.get("x").and_then(|x| x.as_i64()).ok_or("x required")?;
    let y = v.get("y").and_then(|x| x.as_i64()).ok_or("y required")?;
    Ok((x, y))
}

fn json_ok(status: &str) -> Value {
    serde_json::json!({"ok":true,"status":status})
}

/// Windows：DXGI 采集 + VP8（RustDesk scrap），帧 framing [type=2][key][len LE][data]。
/// VP8 初始化或连续编码失败时回退 JPEG（xcap），保证浏览器始终有画面。
#[cfg(target_os = "windows")]
async fn stream_vpx(tx: mpsc::Sender<Vec<u8>>, cfg: std::sync::Arc<StreamCfg>) {
    let handle = tokio::task::spawn_blocking(move || {
        let sel = &crate::capture::SCREEN_SEL;
        let mut last_screen = sel.load(std::sync::atomic::Ordering::Relaxed);
        let mut last_tier = cfg.tier();
        let mut last_scale = cfg.scale();
        let mut stream = match crate::vpx::VpxStream::new(sel, crate::vpx::tier_ratio(last_tier), last_scale) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("vpx init failed, falling back to jpeg: {e:#}");
                None
            }
        };
        let mut vpx_errs: u32 = 0;
        loop {
            // 多屏/画质/分辨率任一变化 → 重建采集+编码器
            let cur_screen = sel.load(std::sync::atomic::Ordering::Relaxed);
            let cur_tier = cfg.tier();
            let cur_scale = cfg.scale();
            if cur_screen != last_screen || cur_tier != last_tier || cur_scale != last_scale {
                last_screen = cur_screen;
                last_tier = cur_tier;
                last_scale = cur_scale;
                stream = crate::vpx::VpxStream::new(sel, crate::vpx::tier_ratio(last_tier), last_scale)
                    .map(Some)
                    .unwrap_or_else(|e| {
                        tracing::error!("vpx rescreen/reconfig failed: {e:#}");
                        None
                    });
            }
            if let Some(st) = stream.as_mut() {
                match st.encode_one() {
                    Ok(Some(f)) => {
                        vpx_errs = 0;
                        let mut msg = vec![2u8, f.key as u8, 0, 0, 0, 0];
                        let n = f.data.len() as u32;
                        msg[2..6].copy_from_slice(&n.to_le_bytes());
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
                        if vpx_errs >= 5 {
                            tracing::error!("vpx disabled, falling back to jpeg");
                            stream = None;
                        } else {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            }
            // JPEG 回退路径（跟随 tier/scale 热调）
            let tier = cfg.tier();
            let scale = cfg.scale();
            let fps = cfg.fps();
            match crate::capture::grab(&Default::default())
                .map(|f| crate::capture::resize_rgba(&f, scale))
                .and_then(|f| crate::capture::encode_jpeg(&f, crate::capture::tier_jpeg_quality(tier)))
            {
                Ok(jpeg) => {
                    let mut msg = vec![1u8, 0, 0, 0, 0, 0];
                    let n = jpeg.len() as u32;
                    msg[2..6].copy_from_slice(&n.to_le_bytes());
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
