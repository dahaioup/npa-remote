// NPA 插件模式：stdin/stdout 上的 LSP 帧 JSON-RPC。
// 协议对端：internal/plugin（Content-Length 帧；plugin.initialize 握手；
// 方法名与 internal/desktop/router.go 的 Invoke 一致）。
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        let line = match read_frame_line(&mut reader) {
            Some(l) => l,
            None => return Ok(()), // EOF：NPA 关闭了管道
        };
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let result = dispatch(method, &params);
        let resp = match result {
            Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e.to_string()}}),
        };
        write_frame(&mut out, &resp)?;
    }
}

/// 每帧头部形如 `Content-Length: N\r\n\r\n`（readLoop 逐行读，空行结束头部）。
fn read_frame_line<R: BufRead>(reader: &mut R) -> Option<String> {
    let mut length: Option<usize> = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim();
        if header.is_empty() {
            if let Some(n) = length {
                let mut body = vec![0u8; n];
                std::io::Read::read_exact(reader, &mut body).ok()?;
                return Some(String::from_utf8_lossy(&body).into_owned());
            }
            continue;
        }
        if let Some(v) = header
            .split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse().ok())
        {
            length = Some(v);
        }
    }
}

fn write_frame<W: Write>(out: &mut W, value: &Value) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    write!(out, "Content-Length: {}\r\n\r\n", data.len())?;
    out.write_all(&data)?;
    out.flush()?;
    Ok(())
}

fn dispatch(method: &str, params: &Value) -> Result<Value> {
    match method {
        "plugin.initialize" => Ok(json!({
            "id": "npa-remote",
            "name": "NPA Remote 插件",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": "1",
            "capabilities": [
                "desktop.screenshot", "desktop.screens",
                "desktop.mouse_move", "desktop.mouse_click", "desktop.mouse_scroll",
                "desktop.type", "desktop.hotkey",
                "desktop.info", "desktop.clipboard_get", "desktop.clipboard_set",
                "desktop.stream"
            ],
        })),
        "desktop.screens" => {
            let (w, h) = crate::capture::screen_size()?;
            Ok(json!([{"id": "0", "name": "primary", "width": w, "height": h}]))
        }
        "desktop.screenshot" => {
            let region = params.get("region").cloned().unwrap_or(json!({}));
            let region: crate::capture::Region = serde_json::from_value(region)?;
            let frame = crate::capture::grab(&region)?;
            let jpeg = crate::capture::encode_jpeg(&frame, 70)?;
            let path = write_temp(&jpeg)?;
            Ok(json!({"resource": {"path": path, "mime": "image/jpeg", "size": jpeg.len()}}))
        }
        "desktop.mouse_move" => {
            let x = params.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = params.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            crate::input::mouse_move(x, y)?;
            Ok(json!({}))
        }
        "desktop.mouse_click" => {
            let button = params.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("click");
            if action != "click" {
                anyhow::bail!("action {action} not supported (click only)");
            }
            crate::input::mouse_click(button, 1)?;
            Ok(json!({}))
        }
        "desktop.mouse_scroll" => {
            let dy = params.get("delta_y").and_then(|v| v.as_i64()).unwrap_or(0);
            crate::input::mouse_scroll(-dy)?; // NPA 约定 deltaY 正=向下，enigo 正=向上
            Ok(json!({}))
        }
        "desktop.type" => {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("text required"))?;
            crate::input::type_text(text)?;
            Ok(json!({}))
        }
        "desktop.hotkey" => {
            let combo = params
                .get("combo")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("combo required"))?;
            crate::input::hotkey(combo)?;
            Ok(json!({}))
        }
        "desktop.info" => {
            let (w, h) = crate::capture::screen_size()?;
            let (mx, my) = crate::input::mouse_pos().unwrap_or((0, 0));
            Ok(json!({"mouse": {"x": mx, "y": my}, "screens": [{"id":"0","width":w,"height":h}]}))
        }
        "desktop.clipboard_get" => Ok(json!({"text": crate::input::clipboard_get()?})),
        "desktop.clipboard_set" => {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("text required"))?;
            crate::input::clipboard_set(text)?;
            Ok(json!({}))
        }
        _ => anyhow::bail!("method not found: {method}"),
    }
}

fn write_temp(data: &[u8]) -> Result<String> {
    let dir = std::env::temp_dir().join("npa-remote");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("shot-{}.jpg", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()));
    std::fs::write(&path, data)?;
    Ok(path.to_string_lossy().into_owned())
}
