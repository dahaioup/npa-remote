// 插件参数：插件自己声明有哪些参数、当前值是多少。
//
// core 侧对应实现（internal/plugin/settings.go）：
//   - plugin.settings.describe → 插件声明参数表（core 渲染「插件 → 设置」页）
//   - plugin.settings.get      → 插件自报当前值（core 落回设置库）
//   - plugin.settings.apply    → core 把用户改的值推给插件
//
// 值存在插件同目录的 npa-remote-settings.json：插件重启后仍在，
// core 推来的值会覆盖文件里的值。
use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use serde_json::{json, Value};

/// 参数声明。type 取 string / bool / number / enum。
fn specs() -> Vec<Value> {
    vec![
        json!({
            "key": "ocr_enabled",
            "label": "允许读屏（OCR）",
            "type": "bool",
            "default": "true",
            "description": "关闭后 desktop.read 不可用，插件也不再声明该能力",
            "writable": true,
        }),
        json!({
            "key": "jpeg_quality",
            "label": "截图 JPEG 质量",
            "type": "number",
            "default": "78",
            "description": "1-95，越高越清晰、文件越大；网页端的档位仍然优先",
            "writable": true,
        }),
        json!({
            "key": "frame_rate",
            "label": "默认帧率",
            "type": "number",
            "default": "15",
            "description": "客户端没有指定帧率时用这个值（1-30）",
            "writable": true,
        }),
        json!({
            "key": "capture_mode",
            "label": "编码方式",
            "type": "enum",
            "options": ["auto", "vp8", "jpeg"],
            "default": "auto",
            "description": "auto=能用 VP8 就用；jpeg=强制 JPEG（带宽大但兼容性好）",
            "writable": true,
        }),
    ]
}

type Store = RwLock<BTreeMap<String, String>>;

fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(load()))
}

/// 参数文件路径：插件可执行文件同目录。
fn settings_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("npa-remote-settings.json")
}

fn load() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(settings_path()) {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
            for (key, value) in map {
                if let Some(text) = value.as_str() {
                    out.insert(key, text.to_string());
                }
            }
        }
    }
    out
}

fn persist(values: &BTreeMap<String, String>) {
    let mut map = serde_json::Map::new();
    for (key, value) in values {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    if let Ok(text) = serde_json::to_string_pretty(&Value::Object(map)) {
        let _ = std::fs::write(settings_path(), text);
    }
}

/// describe 返回参数声明。
pub fn describe() -> Value {
    json!({"settings": specs()})
}

/// get 返回当前生效的值（声明里的默认值 + 已存的值）。
pub fn get() -> Value {
    let store = store().read().unwrap_or_else(|e| e.into_inner());
    let mut values = serde_json::Map::new();
    for spec in specs() {
        let key = spec.get("key").and_then(Value::as_str).unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let value = store
            .get(key)
            .cloned()
            .or_else(|| spec.get("default").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        values.insert(key.to_string(), Value::String(value));
    }
    json!({"values": Value::Object(values)})
}

/// apply 合并 core 推来的值并落盘。
pub fn apply(incoming: &Value) -> Value {
    if let Some(map) = incoming.get("values").and_then(Value::as_object) {
        let mut store = store().write().unwrap_or_else(|e| e.into_inner());
        for (key, value) in map {
            if let Some(text) = value.as_str() {
                store.insert(key.clone(), text.to_string());
            }
        }
        persist(&store);
    }
    get()
}

/// 读一个字符串参数。
pub fn get_str(key: &str) -> Option<String> {
    store().read().ok().and_then(|s| s.get(key).cloned())
}

/// 读一个布尔参数（默认 true）。
pub fn get_bool(key: &str, default: bool) -> bool {
    match get_str(key) {
        Some(value) => matches!(value.as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

/// 读一个数字参数（越界或非法时用默认值）。
pub fn get_num(key: &str, default: u64, min: u64, max: u64) -> u64 {
    match get_str(key).and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(value) => value.clamp(min, max),
        None => default,
    }
}

/// OCR 是否允许。
pub fn ocr_enabled() -> bool {
    get_bool("ocr_enabled", true)
}

/// 截图 JPEG 质量（None 表示用网页端的档位）。
pub fn jpeg_quality() -> Option<u8> {
    match get_str("jpeg_quality").and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(value) => Some(value.clamp(1, 95) as u8),
        None => None,
    }
}

/// 客户端没指定时的默认帧率。
pub fn default_fps() -> u64 {
    get_num("frame_rate", 15, 1, 30)
}

/// 编码方式：auto / vp8 / jpeg。
pub fn capture_mode() -> String {
    get_str("capture_mode")
        .unwrap_or_else(|| "auto".to_string())
        .trim()
        .to_lowercase()
}
