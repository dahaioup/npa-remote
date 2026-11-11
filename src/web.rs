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
const MAX_FPS: u64 = 15;

pub async fn serve(bind: &str, port: u16) -> Result<()> {
    let addr = format!("{bind}:{port}");
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("remote plugin listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn ws_upgrade(ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    // 帧发送任务：capture → jpeg → binary message；quality/fps 由控制消息热调
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);
    let mut quality: u8 = 70;
    let mut fps: u64 = DEFAULT_FPS;
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            #[cfg(target_os = "windows")]
            {
                let _ = (&quality, &fps);
                stream_vpx(tx).await;
            }
            #[cfg(not(target_os = "windows"))]
            loop {
                // 捕获是阻塞调用，丢到 blocking 线程
                let q = quality;
                let frame = tokio::task::spawn_blocking(move || {
                    crate::capture::grab(&Default::default())
                        .and_then(|f| crate::capture::encode_jpeg(&f, q))
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
                        let resp = handle_control(&v, &mut quality, &mut fps).await;
                        let _ = socket.send(Message::Text(resp.into())).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

async fn handle_control(v: &Value, quality: &mut u8, fps: &mut u64) -> String {
    let t = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
    let r: Result<Value, String> = async {
        match t {
            "move" => {
                let (x, y) = xy(v)?;
                crate::input::mouse_move(x, y).map_err(|e| e.to_string())?;
                Ok(json_ok("moved"))
            }
            "click" => {
                let button = v.get("button").and_then(|x| x.as_str()).unwrap_or("left");
                let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
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
                crate::input::mouse_scroll(-dy).map_err(|e| e.to_string())?; // 页面滚轮正=向下
                Ok(json_ok("scrolled"))
            }
            "pos" => {
                let (x, y) = crate::input::mouse_pos().unwrap_or((0, 0));
                let (w, h) = crate::capture::screen_size().unwrap_or((0, 0));
                Ok(serde_json::json!({"ok":true,"x":x,"y":y,"screen_w":w,"screen_h":h}))
            }
            "clip_get" => Ok(serde_json::json!({"ok":true,"text":crate::input::clipboard_get().map_err(|e| e.to_string())?})),
            "clip_set" => {
                let text = v.get("text").and_then(|x| x.as_str()).ok_or("text required")?;
                crate::input::clipboard_set(text).map_err(|e| e.to_string())?;
                Ok(json_ok("clip"))
            }
            "quality" => {
                *quality = v.get("v").and_then(|x| x.as_u64()).unwrap_or(70).clamp(30, 95) as u8;
                Ok(serde_json::json!({"ok":true,"quality":*quality}))
            }
            "fps" => {
                *fps = v.get("v").and_then(|x| x.as_u64()).unwrap_or(DEFAULT_FPS).clamp(1, MAX_FPS);
                Ok(serde_json::json!({"ok":true,"fps":*fps}))
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
#[cfg(target_os = "windows")]
async fn stream_vpx(tx: mpsc::Sender<Vec<u8>>) {
    let handle = tokio::task::spawn_blocking(move || {
        let mut stream = match crate::vpx::VpxStream::new() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("vpx stream init failed: {e:#}");
                return;
            }
        };
        loop {
            match stream.encode_one() {
                Ok(Some(f)) => {
                    let mut msg = vec![2u8, f.key as u8, 0, 0, 0, 0];
                    let n = f.data.len() as u32;
                    msg[2..6].copy_from_slice(&n.to_le_bytes());
                    msg.extend_from_slice(&f.data);
                    if tx.blocking_send(msg).is_err() {
                        return;
                    }
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(15)),
                Err(e) => {
                    tracing::error!("vpx encode failed: {e:#}");
                    return;
                }
            }
        }
    });
    let _ = handle.await;
}
