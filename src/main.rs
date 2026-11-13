// npa-remote — NPA remote 插件
//
// 双模式：
//   1. 独立模式（默认）：`npa-remote serve [--bind 0.0.0.0] [--port 56750]`
//      起 HTTP/WS 服务：内嵌 Web 控制页 + JPEG 帧流 + 输入注入 + 剪贴板。
//   2. NPA 插件模式：`npa-remote plugin`
//      在 stdin/stdout 上说 LSP 帧 JSON-RPC（internal/plugin 协议），
//      响应 plugin.initialize 与 desktop.* 方法，供 NPA Router 调用。
//
// 许可证：AGPL-3.0（见 LICENSE）。
mod capture;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod vpx;
mod input;
mod pluginrpc;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "npa-remote", version, about = "NPA remote 插件")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 独立模式：启动 Web 远程控制服务
    Serve {
        /// 监听地址（默认全网卡，提供局域网访问）
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// 监听端口（官方插件段 56750）
        #[arg(long, default_value_t = 56750)]
        port: u16,
    },
    /// NPA 插件模式：stdin/stdout JSON-RPC
    Plugin,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr) // stdout 属于插件协议，日志只能走 stderr
        .init();
    match Cli::parse().cmd {
        Cmd::Serve { bind, port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(web::serve(&bind, port))
        }
        Cmd::Plugin => pluginrpc::run(),
    }
}
