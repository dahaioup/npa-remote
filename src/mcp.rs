// 独立模式的 MCP 服务：Streamable HTTP（POST /mcp）。
//
// 插件本身就是一个独立的远控程序，所以它的 HTTPS 服务除了网页与帧流，
// 还要能把桌面能力直接以 MCP 暴露出去——任何 MCP 客户端指向
// https://<host>:56750/mcp 就能截图、点鼠标、打字、读屏。
//
// 工具名与 NPA 主体的桌面工具保持一致（screen.* / clipboard.* /
// desktop.*），这样同一个客户端在「连 NPA」和「直连插件」之间切换时
// 不用改提示词。实现复用 src/desktop.rs，与 NPA 插件模式（stdio
// JSON-RPC）行为一致。
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

/// MCP 协议版本（与 NPA 主体协商的版本一致）。
const PROTOCOL_VERSION: &str = "2025-06-18";

/// 把 MCP 端点挂到已有 Router 上。
pub fn mount(app: Router) -> Router {
    app.route("/mcp", post(handle))
}

/// POST /mcp —— MCP Streamable HTTP 的唯一必需方法。
///
/// 请求体是 JSON-RPC 2.0 报文；initialize 的响应带 Mcp-Session-Id 头，
/// 之后的请求带上同一个头即可（这里不强制校验，方便 NPA 的代理透传）。
async fn handle(State(_): State<()>, body: Bytes) -> Response {
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"jsonrpc":"2.0","id":Value::Null,
                        "error":{"code":-32700,"message":format!("parse error: {e}")}}),
                None,
            )
        }
    };
    // 批量请求（MCP 允许一次发多个）→ 逐个处理。
    if let Some(items) = req.as_array() {
        let out: Vec<Value> = items
            .iter()
            .map(|item| handle_one(item).0)
            .collect();
        return json_response(StatusCode::OK, &Value::Array(out), None);
    }
    let (resp, session) = handle_one(&req);
    // 通知（没有 id）→ 202 Accepted，不带正文。
    if resp.get("id").is_none() {
        let mut accepted = StatusCode::ACCEPTED.into_response();
        if let Some(s) = session {
            accepted.headers_mut().insert("mcp-session-id", s);
        }
        return accepted;
    }
    json_response(StatusCode::OK, &resp, session)
}

/// 处理一个 JSON-RPC 报文，返回 (响应, 会话 id)。
fn handle_one(req: &Value) -> (Value, Option<HeaderValue>) {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    match method {
        "initialize" => {
            let session = new_session_id();
            let result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": crate::desktop::PLUGIN_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "远程桌面 MCP：截图、鼠标、键盘、剪贴板、屏幕文字识别。",
            });
            (
                json!({"jsonrpc":"2.0","id":id,"result":result}),
                HeaderValue::from_str(&session).ok(),
            )
        }
        // 客户端确认握手：没有 id 的通知，返回空即可。
        "notifications/initialized" | "initialized" => (json!({}), None),
        "ping" => (json!({"jsonrpc":"2.0","id":id,"result":{}}), None),
        "tools/list" => (
            json!({"jsonrpc":"2.0","id":id,"result":{"tools": tool_definitions()}}),
            None,
        ),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(name, &args) {
                Ok(result) => (json!({"jsonrpc":"2.0","id":id,"result":result}), None),
                Err(e) => (
                    json!({"jsonrpc":"2.0","id":id,
                           "error":{"code":-32000,"message":e.to_string()}}),
                    None,
                ),
            }
        }
        _ => (
            json!({"jsonrpc":"2.0","id":id,
                   "error":{"code":-32601,"message":format!("method not found: {method}")}}),
            None,
        ),
    }
}

/// 工具清单：名字与 NPA 主体的桌面工具对齐。
fn tool_definitions() -> Vec<Value> {
    let region = json!({
        "type": "object",
        "properties": {
            "x": {"type": "integer", "description": "区域左上角 X"},
            "y": {"type": "integer", "description": "区域左上角 Y"},
            "w": {"type": "integer", "description": "区域宽度"},
            "h": {"type": "integer", "description": "区域高度"},
        },
    });
    vec![
        json!({
            "name": "desktop.screens",
            "description": "列出本机屏幕与几何信息",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "desktop.screenshot",
            "description": "截屏（可指定区域），返回 JPEG",
            "inputSchema": {"type": "object", "properties": {"region": region}},
        }),
        json!({
            "name": "screen.read",
            "description": "屏幕文字识别（OCR）：先截图再识别，返回文本与每行坐标",
            "inputSchema": {"type": "object", "properties": {
                "region": region,
                "lang": {"type": "string", "description": "识别语言，如 zh-Hans / en-US"},
            }},
        }),
        json!({
            "name": "desktop.mouse_move",
            "description": "移动鼠标到坐标（absolute）",
            "inputSchema": {"type": "object", "properties": {
                "x": {"type": "integer"}, "y": {"type": "integer"},
            }, "required": ["x", "y"]},
        }),
        json!({
            "name": "screen.move",
            "description": "移动鼠标到坐标但不点击",
            "inputSchema": {"type": "object", "properties": {
                "x": {"type": "integer"}, "y": {"type": "integer"},
            }, "required": ["x", "y"]},
        }),
        json!({
            "name": "desktop.mouse_click",
            "description": "点击鼠标键（left/right/middle，click/down/up）",
            "inputSchema": {"type": "object", "properties": {
                "button": {"type": "string", "enum": ["left", "right", "middle"]},
                "action": {"type": "string", "enum": ["click", "down", "up"]},
            }},
        }),
        json!({
            "name": "screen.click",
            "description": "先移动到坐标再点击",
            "inputSchema": {"type": "object", "properties": {
                "x": {"type": "integer"}, "y": {"type": "integer"},
                "button": {"type": "string", "enum": ["left", "right", "middle"]},
            }, "required": ["x", "y"]},
        }),
        json!({
            "name": "desktop.mouse_scroll",
            "description": "滚动鼠标滚轮（delta_y 正=向下）",
            "inputSchema": {"type": "object", "properties": {
                "delta_x": {"type": "integer"}, "delta_y": {"type": "integer"},
            }},
        }),
        json!({
            "name": "screen.type",
            "description": "在键盘输入文本（物理按键，中文输入法友好）",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"},
            }, "required": ["text"]},
        }),
        json!({
            "name": "screen.hotkey",
            "description": "发送组合键，如 ctrl+v、ctrl+shift+esc、alt+f4",
            "inputSchema": {"type": "object", "properties": {
                "combo": {"type": "string"},
            }, "required": ["combo"]},
        }),
        json!({
            "name": "screen.info",
            "description": "查询鼠标位置与屏幕分辨率",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "clipboard.get",
            "description": "读取剪贴板文本",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "clipboard.set",
            "description": "写入剪贴板文本（配合 screen.hotkey ctrl+v 粘贴）",
            "inputSchema": {"type": "object", "properties": {
                "text": {"type": "string"},
            }, "required": ["text"]},
        }),
    ]
}

/// 调用一个工具：名字 → 插件协议方法 → 统一结果格式。
fn call_tool(name: &str, args: &Value) -> anyhow::Result<Value> {
    let (method, params) = match name {
        "desktop.screens" => ("desktop.screens", json!({})),
        "desktop.screenshot" => ("desktop.screenshot", args.clone()),
        "screen.read" => ("desktop.read", args.clone()),
        "desktop.mouse_move" | "screen.move" => ("desktop.mouse_move", args.clone()),
        "desktop.mouse_click" => ("desktop.mouse_click", args.clone()),
        "screen.click" => {
            // 先移到坐标再点：两步合成一个工具。
            let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            crate::desktop::dispatch(
                "desktop.mouse_move",
                &json!({"x": x, "y": y}),
            )?;
            let mut p = args.clone();
            p["action"] = json!("click");
            ("desktop.mouse_click", p)
        }
        "desktop.mouse_scroll" => ("desktop.mouse_scroll", args.clone()),
        "screen.type" => ("desktop.type", args.clone()),
        "screen.hotkey" => ("desktop.hotkey", args.clone()),
        "screen.info" => ("desktop.info", json!({})),
        "clipboard.get" => ("desktop.clipboard_get", json!({})),
        "clipboard.set" => ("desktop.clipboard_set", args.clone()),
        other => anyhow::bail!("unknown tool: {other}"),
    };

    let out = crate::desktop::dispatch(method, &params)?;

    // 带截图的工具：把 JPEG 直接内联进结果（base64），客户端不用再读文件。
    let mut content: Vec<Value> = Vec::new();
    if let Some(res) = out.get("resource") {
        let path = res.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let mime = res
            .get("mime_type")
            .and_then(|v| v.as_str())
            .unwrap_or("image/jpeg");
        if !path.is_empty() {
            content.push(json!({"type": "text", "text": format!("screenshot: {path}")}));
            if let Ok(bytes) = std::fs::read(path) {
                content.push(json!({
                    "type": "image",
                    "data": base64_encode(&bytes),
                    "mimeType": mime,
                }));
            }
        }
    }
    if content.is_empty() {
        // 没有图片：把结果本身转成文本块。
        let text = match &out {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        content.push(json!({"type": "text", "text": text}));
    }
    // OCR 的文本单独再给一块，方便模型直接读。
    if let Some(text) = out.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    Ok(json!({
        "content": content,
        "isError": false,
        "structuredContent": out,
    }))
}

/// 标准 base64 编码（无换行）。
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 生成会话 id（随机十六进制）。
fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mixed = nanos ^ (pid << 64) ^ (nanos / 7);
    format!("{:032x}", mixed)
}

/// 组装 JSON 响应（可选带会话头）。
fn json_response(status: StatusCode, body: &Value, session: Option<HeaderValue>) -> Response {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    let mut resp = (status, [(header::CONTENT_TYPE, "application/json")], text).into_response();
    if let Some(s) = session {
        resp.headers_mut().insert("mcp-session-id", s);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn initialize_advertises_server_info() {
        let (resp, session) = handle_one(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION}
        }));
        assert!(session.is_some());
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], crate::desktop::PLUGIN_NAME);
    }

    #[test]
    fn tool_list_matches_npa_desktop_tools() {
        let (resp, _) = handle_one(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        for want in [
            "desktop.screenshot",
            "screen.read",
            "screen.type",
            "screen.click",
            "screen.hotkey",
            "clipboard.get",
            "clipboard.set",
        ] {
            assert!(names.contains(&want.to_string()), "missing tool {want}");
        }
    }

    #[test]
    fn unknown_tool_is_rejected() {
        let err = call_tool("nope", &json!({})).unwrap_err();
        assert!(err.to_string().contains("unknown tool"));
    }

    #[test]
    fn unknown_method_returns_jsonrpc_error() {
        let (resp, _) = handle_one(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"}));
        assert_eq!(resp["error"]["code"], -32601);
    }
}
