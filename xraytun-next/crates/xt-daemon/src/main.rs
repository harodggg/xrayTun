//! xt-daemon 可执行入口：参数解析 → 日志初始化 → 装配 → 监听。
//!
//! 这里刻意不做任何业务判断：daemon 的全部行为都在 lib 里，入口只负责把
//! 「用户给的路径」变成 `DaemonConfig`，并把 Ctrl-C 变成一个关闭事件
//! （等信号是事件，不是轮询，也没有 sleep）。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use xt_contract::model::LogLevel;
use xt_daemon::{Daemon, DaemonConfig};

#[derive(Parser, Debug)]
#[command(name = "xt-daemon", about = "xraytun-next 控制面 daemon（组合根）")]
struct Cli {
    /// AF_UNIX socket 路径
    #[arg(long, default_value = "/tmp/xraytun.sock")]
    socket: PathBuf,

    /// 状态目录（settings.json / subscription.txt / 生成的 xray 配置）
    #[arg(long, default_value_os_t = default_state_dir())]
    state_dir: PathBuf,

    /// xray 可执行文件（也可用环境变量 XT_XRAY_BIN）
    #[arg(long)]
    xray: Option<PathBuf>,

    /// 日志级别
    #[arg(long, default_value = "info", value_parser = ["error", "warn", "info", "debug"])]
    log_level: String,

    /// 本地订阅原文；缺省 = <state-dir>/subscription.txt
    #[arg(long)]
    subscription_file: Option<PathBuf>,

    /// 探测靶点 URL（默认联网靶点；离线/测试时指向环回地址）
    #[arg(long, default_value = xt_probe::DEFAULT_PROBE_URL)]
    probe_url: String,
}

/// 状态目录默认值：优先 XDG 数据目录，其次 HOME，最后临时目录。
/// 不用「当前目录」——那会让配置随启动位置漂移。
fn default_state_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("xraytun");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local/share/xraytun");
        }
    }
    std::env::temp_dir().join("xraytun")
}

fn parse_level(raw: &str) -> LogLevel {
    match raw {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "debug" => LogLevel::Debug,
        _ => LogLevel::Info,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = parse_level(&cli.log_level);

    // 初始化失败（例如调用方已经装过 subscriber）不是致命问题：继续跑，
    // 核心日志仍然会进总线与 TailLogs。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(cli.log_level.clone()))
        .try_init();

    let subscription_file =
        cli.subscription_file.unwrap_or_else(|| cli.state_dir.join("subscription.txt"));
    // 查找顺序：--xray > XT_XRAY_BIN > PATH 里的 `xray`。刻意不写死仓库内的
    // 开发路径：那对打包后的产品是错的，而错得很隐蔽（看起来有值但永远找不到）。
    let xray_bin = cli
        .xray
        .or_else(|| std::env::var_os("XT_XRAY_BIN").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("xray"));
    let config = DaemonConfig {
        socket_path: cli.socket,
        state_dir: cli.state_dir,
        xray_bin,
        log_level: level,
        subscription_file,
        probe_url: cli.probe_url,
    };

    let daemon = match Daemon::bootstrap(config).await {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!("xt-daemon 启动失败：{error}");
            return ExitCode::FAILURE;
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(());
        }
    });

    match daemon.serve(shutdown_rx).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xt-daemon 退出：{error}");
            ExitCode::FAILURE
        }
    }
}
