use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use filemux_core::{Config, default_config_path};
use filemux_engine::Engine;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "filemux", version, about = "TFTP、HTTP、NFSv3 文件共享服务")]
struct Cli {
    #[arg(
        short,
        long,
        global = true,
        help = "配置文件；相对路径以该文件所在目录为准"
    )]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "检查配置，不启动服务")]
    Check,
    #[command(about = "创建配置文件，不覆盖已有配置")]
    Init {
        #[arg(long, help = "已经存在的共享目录")]
        root: PathBuf,
        #[arg(long, help = "共享目录之外的持久状态目录")]
        state_dir: Option<PathBuf>,
    },
    #[command(about = "前台运行服务，按 Ctrl+C 停止")]
    Serve {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long, help = "启用 HTTP 并覆盖监听地址，例如 127.0.0.1:8080")]
        http_bind: Option<SocketAddr>,
        #[arg(long, help = "启用 TFTP 并覆盖监听地址")]
        tftp_bind: Option<SocketAddr>,
        #[arg(long, help = "启用 NFSv3 UDP/TCP 并覆盖监听地址")]
        nfs_bind: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "no_rpcbind",
            help = "NFS 端口映射的 UDP/TCP 地址"
        )]
        rpcbind: Option<SocketAddr>,
        #[arg(long, help = "关闭端口映射，客户端必须显式指定 NFS 与 MOUNT 端口")]
        no_rpcbind: bool,
        #[arg(long, help = "禁用 HTTP")]
        no_http: bool,
        #[arg(long, help = "允许 TFTP 上传，仍不覆盖已有文件")]
        allow_upload: bool,
        #[arg(long, help = "将就绪信息以一行 JSON 输出到 stdout，日志写入 stderr")]
        json: bool,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let explicit_config = cli.config.is_some();
    let path = cli.config.unwrap_or_else(default_config_path);
    if let Command::Init { root, state_dir } = cli.command {
        ensure!(!path.exists(), "配置文件已存在，未覆盖：{}", path.display());
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("无法访问共享目录 {}", root.display()))?;
        let mut config = Config {
            root,
            ..Config::default()
        };
        if let Some(state_dir) = state_dir {
            config.state_dir = absolute(state_dir)?;
        }
        config.save(&path)?;
        println!("配置已保存：{}", path.display());
        println!("TFTP 上传默认关闭；尚未启动任何服务。");
        return Ok(());
    }
    let mut config = if path.exists() {
        Config::load(&path)?
    } else if explicit_config || matches!(cli.command, Command::Check) {
        bail!(
            "配置文件不存在：{}；可使用 filemux init --root <共享目录> 创建",
            path.display()
        )
    } else {
        Config::default()
    };
    match cli.command {
        Command::Check => {
            config.validate()?;
            println!("配置有效：{}", path.display());
            println!("尚未启动服务，也未检查监听端口或持久状态是否可用。");
            Ok(())
        }
        Command::Serve {
            root,
            state_dir,
            http_bind,
            tftp_bind,
            nfs_bind,
            rpcbind,
            no_rpcbind,
            no_http,
            allow_upload,
            json,
        } => {
            if let Some(root) = root {
                config.root = absolute(root)?;
            }
            if let Some(state_dir) = state_dir {
                config.state_dir = absolute(state_dir)?;
            }
            if let Some(bind) = http_bind {
                config.http.bind = bind;
                config.http.enabled = true;
            }
            if let Some(bind) = tftp_bind {
                config.tftp.bind = bind;
                config.tftp.enabled = true;
            }
            if let Some(bind) = nfs_bind {
                config.nfs.bind = bind;
                config.nfs.enabled = true;
            }
            if let Some(bind) = rpcbind {
                config.nfs.rpcbind = Some(bind);
            }
            if no_rpcbind {
                config.nfs.rpcbind = None;
            }
            if no_http {
                config.http.enabled = false;
            }
            if allow_upload {
                config.tftp.allow_upload = true;
            }
            config.validate()?;
            let mut engine = Engine::new(config);
            let mut events = engine.subscribe();
            engine.start_enabled().await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"event":"ready", "services":engine.snapshot()})
                );
            } else {
                println!("共享目录：{}", engine.config().root.display());
                for service in engine.snapshot() {
                    for endpoint in service.endpoints {
                        println!(
                            "{} {} {}：{}",
                            service.protocol, endpoint.role, endpoint.transport, endpoint.address
                        );
                    }
                }
                println!("服务已启动，按 Ctrl+C 停止。");
            }
            std::io::stdout().flush().context("无法输出就绪信息")?;
            let mut health = tokio::time::interval(Duration::from_millis(500));
            let signal = shutdown_signal();
            tokio::pin!(signal);
            let outcome = loop {
                tokio::select! {
                    result = &mut signal => break result,
                    _ = health.tick() => {
                        if let Err(error) = engine.check_health().await { break Err(error); }
                    }
                    event = events.recv() => match event {
                        Ok(event) => eprintln!("[{}] {}{}", event.protocol, if event.is_error { "错误：" } else { "" }, event.message),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => eprintln!("日志更新过快，已省略 {count} 条旧记录。"),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break Err(anyhow::anyhow!("服务事件通道已关闭")),
                    }
                }
            };
            let stopped = engine.stop_all().await;
            outcome?;
            stopped?;
            if json {
                println!("{}", serde_json::json!({"event":"stopped"}));
            } else {
                println!("所有服务已停止。");
            }
            Ok(())
        }
        Command::Init { .. } => unreachable!("handled above"),
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("无法接收 Ctrl+C"),
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(windows)]
async fn shutdown_signal() -> Result<()> {
    let mut control_break = tokio::signal::windows::ctrl_break()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("无法接收 Ctrl+C"),
        _ = control_break.recv() => Ok(()),
    }
}
