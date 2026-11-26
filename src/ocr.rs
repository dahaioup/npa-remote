// OCR：屏幕文字识别。
// Windows 走系统自带 WinRT OCR（Windows.Media.Ocr），macOS 走 Vision 框架，
// 其它平台暂不支持——能力表里不会声明 desktop.read，NPA 的 screen.read 会
// 明确报「插件不支持」。
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// 一行识别结果及其在截图内的像素框（与 NPA desktop.TextLine 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 识别结果（与 NPA desktop.ReadResult 对齐）。
#[derive(Debug, Clone)]
pub struct Outcome {
    pub engine: String,
    pub text: String,
    pub lines: Vec<Line>,
}

/// 当前平台是否支持 OCR（决定 capabilities 里是否声明 desktop.read）。
pub fn supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

/// 识别图片文件里的文字。path 为图片文件路径；lang 形如 "zh-Hans"（可空表示用系统默认）。
pub fn recognize(path: &str, lang: &str) -> Result<Outcome> {
    #[cfg(target_os = "windows")]
    {
        crate::ocr_windows::recognize(path, lang)
    }
    #[cfg(target_os = "macos")]
    {
        crate::ocr_macos::recognize(path, lang)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (path, lang);
        anyhow::bail!("OCR is not supported on this platform")
    }
}

/// 从命令输出里截出 JSON 片段（脚本可能在 JSON 前后混入其它输出）。
pub fn extract_json(raw: &str) -> &str {
    let start = raw.find('{');
    let end = raw.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => &raw[s..=e],
        _ => "",
    }
}

/// 拼接整页文本。
pub fn join_lines(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 各平台 OCR 后端统一的输出结构：{"engine":..,"error":..,"lines":[..]}。
#[derive(Debug, Clone, Deserialize)]
pub struct Payload {
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub lines: Vec<RawLine>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLine {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub w: i32,
    #[serde(default)]
    pub h: i32,
}

/// 解析后端输出：先截 JSON，再按统一结构解析；后端报 error 时直接失败。
pub fn parse_payload(json: &str) -> Result<Outcome> {
    let json = extract_json(json);
    if json.is_empty() {
        anyhow::bail!("no JSON in OCR output");
    }
    let payload: Payload = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("invalid OCR JSON: {} ({})", &json[..json.len().min(200)], e))?;
    if let Some(err) = payload.error {
        anyhow::bail!("{}", err);
    }
    let lines: Vec<Line> = payload
        .lines
        .into_iter()
        .map(|l| Line { text: l.text, x: l.x, y: l.y, w: l.w, h: l.h })
        .collect();
    let text = join_lines(&lines);
    Ok(Outcome { engine: payload.engine, text, lines })
}
