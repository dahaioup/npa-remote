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

/// 采集前的显示环境准备（进程启动早期调用，必须早于任何 X 连接建立）。
///
/// scrap 的 X11 后端会在首次调用时建立 XCB 连接并可能缓存，之后再改 DISPLAY
/// 不一定生效（连接已经指向旧 display）。所以这里必须在进程启动阶段就把
/// DISPLAY 定下来：
///   1. DISPLAY 缺失 → 探测 /tmp/.X11-unix 补上（优先真实会话屏，再退 Xvfb）；
///   2. 当前 DISPLAY 指向的 X server 还活着 → 就用它。
///      无人值守机的会话 X 往往所有输出口都 disconnected（RandR 枚举不到
///      monitor），但会话根窗口里有桌面内容 —— scrap 的根窗口兜底能抓到它；
///   3. 当前 display 连不上 → 才退到 Xvfb 虚拟屏。
///
/// 注意：早期版本这里会优先复用「运行中的 Xvfb」，结果无人值守机上抓到的是
/// 一个空虚拟屏（纯黑），而真正的桌面在会话 X 的根窗口里。
#[cfg(target_os = "linux")]
pub fn prepare_display_for_capture() {
    ensure_display();
    if current_display_reachable() {
        tracing::info!(
            "capture display ready: {} (session root window)",
            std::env::var("DISPLAY").unwrap_or_default()
        );
        return;
    }
    if ensure_virtual_display() {
        tracing::info!(
            "capture display ready: {}",
            std::env::var("DISPLAY").unwrap_or_default()
        );
    }
}

#[cfg(not(target_os = "linux"))]
pub fn prepare_display_for_capture() {}

/// 当前 DISPLAY 指向的 X server 是否还活着。
///
/// 注意：光看 /tmp/.X11-unix/X<n> 这个文件存在是不够的——机器上可能留着
/// 早已退出、没人监听的陈旧 socket（用户换过、容器重启过都会留下）。
/// 那种 socket 连上去会直接 ECONNREFUSED，xlib 只能报 "No protocol specified"，
/// 于是整条采集链路静默失败。所以这里直接连一下 socket 探活。
#[cfg(target_os = "linux")]
pub fn current_display_reachable() -> bool {
    let Some(num) = current_display_number() else {
        return false;
    };
    x_socket_alive(num)
}

/// X socket 是否真的有人在监听（陈旧 socket 文件不算）。
#[cfg(target_os = "linux")]
fn x_socket_alive(num: u32) -> bool {
    use std::os::unix::net::UnixStream;
    UnixStream::connect(format!("/tmp/.X11-unix/X{num}")).is_ok()
}

/// 解析 DISPLAY 里的 display 号（":0" / ":0.0" / "host:0.0" → 0）。
#[cfg(target_os = "linux")]
fn current_display_number() -> Option<u32> {
    let disp = std::env::var("DISPLAY").ok()?;
    let tail = disp.rsplit(':').next()?;
    let num = tail.split('.').next()?;
    num.parse::<u32>().ok()
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
        // DISPLAY 已经指向一个活着的 X server：就用它（无人值守机的会话 X
        // 常常没有活动显示器，但根窗口里有桌面内容，scrap 的根窗口兜底能抓到）。
        if let Ok(cur) = std::env::var("DISPLAY") {
            if current_display_reachable() {
                return;
            }
            tracing::warn!("DISPLAY={cur} is not reachable, probing other X servers");
            std::env::remove_var("DISPLAY");
        }
        // DISPLAY 没设时：先找正在跑的 Xvfb 虚拟屏（无人值守机/容器里的
        // 常规配置，往往这就是唯一能连上的 X server），再退到真实会话屏。
        // 两种候选都要先探活——机器上常留着没人监听的陈旧 socket。
        let socks = list_x_sockets();
        let alive: Vec<u32> = socks.into_iter().filter(|n| x_socket_alive(*n)).collect();
        if let Some(d) = find_running_xvfb_display() {
            std::env::set_var("DISPLAY", &d);
            tracing::info!("DISPLAY was unset, reusing running Xvfb display {d}");
            return;
        }
        if let Some(num) = alive.iter().copied().find(|n| !display_is_xvfb(*n)) {
            let d = format!(":{num}");
            std::env::set_var("DISPLAY", &d);
            tracing::info!("DISPLAY was unset, detected live session display {d}");
            return;
        }
        if let Some(num) = alive.first().copied() {
            let d = format!(":{num}");
            std::env::set_var("DISPLAY", &d);
            tracing::info!("DISPLAY was unset, detected {d} from /tmp/.X11-unix");
            return;
        }
        tracing::warn!("DISPLAY unset and no live X11 socket found in /tmp/.X11-unix");
    });
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_display() {}

/// /tmp/.X11-unix 里的 display 号（升序）。
#[cfg(target_os = "linux")]
fn list_x_sockets() -> Vec<u32> {
    let mut nums: Vec<u32> = match std::fs::read_dir("/tmp/.X11-unix") {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let num = name.strip_prefix('X')?;
                if num.chars().all(|c| c.is_ascii_digit()) {
                    num.parse::<u32>().ok()
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    nums.sort_unstable();
    nums
}

/// 某个 display 号背后是不是 Xvfb（扫 /proc 找 Xvfb 进程的 display 参数）。
#[cfg(target_os = "linux")]
fn display_is_xvfb(num: u32) -> bool {
    let want = format!(":{num}");
    let mut entries: Vec<_> = match std::fs::read_dir("/proc") {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return false,
    };
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let raw = match std::fs::read(format!("/proc/{name}/cmdline")) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !text.contains("Xvfb") {
            continue;
        }
        for tok in text.split_whitespace() {
            if tok == want {
                return true;
            }
        }
    }
    false
}

/// 无人值守机器常见「X server 在跑、但一个活动显示器都没有」的情况：
/// 所有输出口都是 disconnected → xcap/scrap 枚举不到 monitor。
/// 现在 scrap 有根窗口兜底，能抓到会话根窗口里的桌面；
/// 但如果当前 display 根本连不上，还是得用 Xvfb 起一个虚拟屏。
/// 返回 true 表示当前 DISPLAY 可用（会话屏或虚拟屏）。
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
    // 当前 display 还活着 → 就用它。无人值守机的会话 X 没有活动显示器，
    // 但根窗口里有桌面内容（scrap 根窗口兜底），比切到空虚拟屏强。
    if current_display_reachable() {
        let d = std::env::var("DISPLAY").unwrap_or_default();
        tracing::info!("reusing current display {d}");
        *guard = Some(d);
        return true;
    }
    // 已经在跑 Xvfb 虚拟屏：直接用它（当前 DISPLAY 不是它就切过去）。
    if let Some(d) = find_running_xvfb_display() {
        if std::env::var("DISPLAY").ok().as_deref() != Some(d.as_str()) {
            std::env::set_var("DISPLAY", &d);
            tracing::info!("switched to running virtual display {d}");
        } else {
            tracing::info!("reusing running virtual display {d}");
        }
        *guard = Some(d);
        return true;
    }
    // 没有虚拟屏，但有真实的活动显示器 → 用当前 DISPLAY。
    // xcap 依赖 RandR 1.5 的 monitor 枚举：真机上非空；会话 X 上所有输出口
    // disconnected 时为空，正好用来区分「有画面」和「只有黑屏」。
    match xcap::Monitor::all() {
        Ok(mons) if !mons.is_empty() => {
            let d = std::env::var("DISPLAY").unwrap_or_default();
            tracing::info!("reusing active display {d} ({} monitors)", mons.len());
            *guard = Some(d);
            return true;
        }
        _ => {}
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
    tracing::info!("no reachable display — started virtual screen via {xvfb} on {disp}");
    *guard = Some(disp);
    true
}

/// 扫描 /proc，找出正在运行的 Xvfb 及其 display 号（如 :90）。
/// 无人值守机上会话 X（:0）没有活动显示器，真正能出画面的往往是我们自己起的虚拟屏。
#[cfg(target_os = "linux")]
fn find_running_xvfb_display() -> Option<String> {
    let mut entries: Vec<_> = match std::fs::read_dir("/proc") {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return None,
    };
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let raw = match std::fs::read(format!("/proc/{name}/cmdline")) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !text.contains("Xvfb") {
            continue;
        }
        for tok in text.split_whitespace() {
            if let Some(num) = tok.strip_prefix(':') {
                if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                    return Some(format!(":{num}"));
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn find_xvfb() -> Option<String> {    let mut dirs: Vec<String> = std::env::var("PATH")
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
