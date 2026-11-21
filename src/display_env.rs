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

/// 无人值守机器常见「X server 在跑、但一个活动显示器都没有」的情况：
/// 所有输出口都是 disconnected → xcap/scrap 枚举不到 monitor → 抓不到画面。
/// 这时用 Xvfb 起一个虚拟屏并把 DISPLAY 指过去（需要系统装有 Xvfb）。
/// 返回 true 表示已经（或已经能）用上虚拟屏。
#[cfg(target_os = "linux")]
pub fn ensure_virtual_display() -> bool {
    use std::sync::{Mutex, OnceLock};
    static STATE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(None));
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if let Some(d) = guard.as_ref() {
        if std::env::var("DISPLAY").ok().as_deref() == Some(d.as_str()) {
            return true;
        }
    }
    // 允许显式关闭（排查用）
    if std::env::var_os("NPA_NO_XVFB").is_some() {
        return false;
    }
    // 已经有可用的显示器就直接复用（插件重启时不要每次都新起一个 :90/:91…）
    // 注意：不能用 xcap::Monitor::all() 判断——它同样依赖 RandR 1.5 的 monitor 枚举，
    // 在只有 RANDR 1.2 的无头 X server 上恒为空；scrap 打了补丁后能按 screen 尺寸兜底。
    if let Ok(displays) = scrap::Display::all() {
        if !displays.is_empty() {
            let d = std::env::var("DISPLAY").unwrap_or_default();
            tracing::info!("reusing existing display {d} ({} displays)", displays.len());
            *guard = Some(d);
            return true;
        }
    }
    let Some(xvfb) = find_xvfb() else {
        warn_throttled(
            "xvfb",
            "Xvfb not found — install it (sudo apt install xvfb) to get a virtual screen on headless machines",
        );
        return false;
    };
    let Some(num) = pick_free_display() else {
        warn_throttled("xvfb", "no free X display number found");
        return false;
    };
    let disp = format!(":{num}");
    let mut cmd = std::process::Command::new(&xvfb);
    cmd.arg(&disp)
        .args(["-screen", "0", "1280x800x24", "-nolisten", "tcp"]);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Err(e) = cmd.spawn() {
        warn_throttled("xvfb", &format!("spawn {xvfb} failed: {e}"));
        return false;
    }
    // 等 X socket 出现（最多 ~5s）
    let sock = format!("/tmp/.X11-unix/X{num}");
    for _ in 0..50 {
        if std::path::Path::new(&sock).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::env::set_var("DISPLAY", &disp);
    tracing::info!("no active display — started virtual screen via {xvfb} on {disp}");
    *guard = Some(disp);
    true
}

#[cfg(target_os = "linux")]
fn find_xvfb() -> Option<String> {
    let mut dirs: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| d.to_string())
        .collect();
    for extra in ["/usr/bin", "/usr/local/bin", "/bin"] {
        if !dirs.iter().any(|d| d == extra) {
            dirs.push(extra.to_string());
        }
    }
    for d in dirs {
        let p = format!("{d}/Xvfb");
        if std::path::Path::new(&p).is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn pick_free_display() -> Option<u32> {
    for n in 90..=120u32 {
        let lock = format!("/tmp/.X{n}-lock");
        let sock = format!("/tmp/.X11-unix/X{n}");
        if !std::path::Path::new(&lock).exists() && !std::path::Path::new(&sock).exists() {
            return Some(n);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_virtual_display() -> bool {
    false
}
