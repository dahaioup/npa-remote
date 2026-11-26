// NPA 插件模式：stdin/stdout 上的 LSP 帧 JSON-RPC。
//
// 协议对端：internal/plugin（Content-Length 帧；plugin.initialize 握手；
// 方法名与 internal/desktop/router.go 的 Invoke 一致）。
//
// 实际的桌面能力实现在 src/desktop.rs——同一条实现也被 HTTPS 上的 MCP
// 端点（src/mcp.rs）复用，两条通道行为一致。
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
        let resp = match crate::desktop::dispatch(method, &params) {
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
