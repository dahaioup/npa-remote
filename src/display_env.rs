// Linux 图形会话环境适配。
//
// 背景：插件通常由 NPA 的 sidecar 拉起，而 NPA 又是 systemd --user 服务启动，
// 这条链路上 **DISPLAY 不会自动继承**（systemd user session 未导入 X11 变量）。
// 结果：xcap / scrap 的 X11 后端连不上 X server →
//   - xcap 报 "no monitors, fallback to cli"
//   - scrap 的 Display::all() 报 "no display"（VP8 流直接起不来）
//
// 这里做两件事：
//   1. DISPLAY 缺失时，从 /tmp/.X11-unix/X<n> 探测出 :<n> 并补上；
//   2. 高频失败日志限频（原来每帧一行 warn → 一小时能刷 1GB 日志）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 同一类告警的最小间隔（秒）
const WARN_INTERVAL_SECS: u64 = 10;

static LAST_WARN: AtomicU64 = AtomicU64::new(0);

/// 限频告警：同类消息在 WARN_INTERVAL_SECS 内只打第一条。
pub fn warn_throttled(key: &str, msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST_WARN.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= WARN_INTERVAL_SECS
        && LAST_WARN
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::warn!("{key}: {msg}");
    }
}

/// 补齐 DISPLAY（仅 Linux 有意义；其他平台直接返回）。
///
/// 插件被 sidecar/systemd 拉起时环境里常常没有 DISPLAY，
/// 但 X11 socket 目录里能看出当前会话用的是几号 display。
#[cfg(target_os = "linux")]
pub fn ensure_display() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("DISPLAY").is_some() {
            return;
        }
        let mut found: Option<String> = None;
        if let Ok(rd) = std::fs::read_dir("/tmp/.X11-unix") {
            let mut socks: Vec<String> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with('X') && n.len() > 1)
                .collect();
            socks.sort();
            for s in socks {
                if let Some(num) = s.strip_prefix('X') {
                    if num.chars().all(|c| c.is_ascii_digit()) {
                        found = Some(format!(":{num}"));
                        break;
                    }
                }
            }
        }
        match found {
            Some(d) => {
                std::env::set_var("DISPLAY", &d);
                tracing::info!("DISPLAY was unset, detected {d} from /tmp/.X11-unix");
            }
            None => {
                tracing::warn!("DISPLAY unset and no X11 socket found in /tmp/.X11-unix");
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_display() {}
