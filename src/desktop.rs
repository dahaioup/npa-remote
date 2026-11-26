// 桌面能力层：截图 / 键鼠 / 剪贴板 / 屏幕信息 / OCR。
//
// 这一层与「怎么被调用」无关：
//   - NPA 插件模式（stdio LSP 帧 JSON-RPC，方法名 desktop.*）→ src/pluginrpc.rs
//   - 独立模式（HTTPS 上的 MCP Streamable HTTP）→ src/mcp.rs
// 两条通道共用同一套实现与同一份能力清单，行为完全一致。
use anyhow::Result;
use serde_json::{json, Value};

/// 插件 id（对 NPA 报的名字）。
pub const PLUGIN_ID: &str = "npa-remote";

/// 插件名（对 NPA 报的名字）。
pub const PLUGIN_NAME: &str = "NPA Remote 插件";

/// 基础能力清单。
const BASE_CAPABILITIES: &[&str] = &[
    "desktop.screenshot",
    "desktop.screens",
    "desktop.mouse_move",
    "desktop.mouse_click",
    "desktop.mouse_scroll",
    "desktop.type",
    "desktop.hotkey",
    "desktop.info",
    "desktop.clipboard_get",
    "desktop.clipboard_set",
];

/// 能力清单：OCR 只有 Windows / macOS 有系统引擎，Linux 不声明
/// （调用方会明确收到「插件不支持 desktop.read」而不是一个空结果）。
pub fn capabilities() -> Vec<String> {
    let mut caps: Vec<String> = BASE_CAPABILITIES.iter().map(|c| c.to_string()).collect();
    if crate::ocr::supported() {
        caps.push("desktop.read".to_string());
    }
    caps
}

/// plugin.initialize 的返回体。
pub fn initialize_result() -> Value {
    json!({
        "id": PLUGIN_ID,
        "name": PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1",
        "capabilities": capabilities(),
    })
}

/// 按插件协议的方法名分发一次调用。
pub fn dispatch(method: &str, params: &Value) -> Result<Value> {
    match method {
        "plugin.initialize" => Ok(initialize_result()),
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
            Ok(json!({"resource": {
                "path": path,
                "mime_type": "image/jpeg",
                "mime": "image/jpeg",
                "size": jpeg.len(),
            }}))
        }
        "desktop.mouse_move" => {
            let x = params.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = params.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let relative = params
                .get("relative")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if relative {
                let (cx, cy) = crate::input::mouse_pos().unwrap_or((0, 0));
                crate::input::mouse_move(cx + x, cy + y)?;
            } else {
                crate::input::mouse_move(x, y)?;
            }
            Ok(json!({}))
        }
        "desktop.mouse_click" => {
            let button = params.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("click");
            match action {
                "click" => crate::input::mouse_click(button, 1)?,
                "down" => crate::input::mouse_down(button)?,
                "up" => crate::input::mouse_up(button)?,
                other => anyhow::bail!("action {other} not supported (click/down/up)"),
            }
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
        "desktop.read" => {
            // 屏幕文字识别：先截图（无论 OCR 成败都带上截图路径），再识别。
            let region = params.get("region").cloned().unwrap_or(json!({}));
            let region: crate::capture::Region = serde_json::from_value(region)?;
            let lang = params.get("lang").and_then(|v| v.as_str()).unwrap_or("");
            let frame = crate::capture::grab(&region)?;
            let jpeg = crate::capture::encode_jpeg(&frame, 70)?;
            let path = write_temp(&jpeg)?;
            let mut out = json!({
                "status": "ok",
                "resource": {
                    "path": path,
                    "mime_type": "image/jpeg",
                    "mime": "image/jpeg",
                    "size": jpeg.len(),
                },
                "text": "",
                "lines": [],
            });
            match crate::ocr::recognize(&path, lang) {
                Ok(o) => {
                    out["engine"] = json!(o.engine);
                    out["text"] = json!(o.text);
                    out["lines"] = json!(o.lines);
                }
                Err(e) => {
                    out["status"] = json!("ocr_failed");
                    out["error"] = json!(e.to_string());
                }
            }
            Ok(out)
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

/// 把截图写到临时目录，返回文件路径。
pub fn write_temp(data: &[u8]) -> Result<String> {
    let dir = std::env::temp_dir().join("npa-remote");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "shot-{}.jpg",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    ));
    std::fs::write(&path, data)?;
    Ok(path.to_string_lossy().into_owned())
}
