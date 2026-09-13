//! `xraytun-helper` —— 以 root 运行的特权守护进程。
//!
//! 它的存在只为了解决一件事：**macOS 上创建 `utun` 必须 root**。
//! 除此之外的一切（订阅、协议、代理逻辑、UI）都不在这里。
//!
//! ```text
//! launchd (system)
//!   └── xraytun-helper run            ← root，常驻
//!         ├── /var/run/com.xraytun.helper.sock   AF_UNIX + SOCK_SEQPACKET, 0660 root:admin
//!         ├── xt-tun::controller                 建 utun / 配路由 / 改 DNS / 快照回滚
//!         └── (可选) 以 root 拉起数据面子进程
//!
//! XrayTun.app                        ← 普通用户
//!         └── 通过 socket 下发有限指令 + 取回 utun fd（SCM_RIGHTS）
//! ```
//!
//! 子命令：
//!
//! * `run`      守护进程模式（launchd 调用，默认）
//! * `status`   查询状态
//! * `restore`  回滚遗留会话（排障用）
//! * `version`  打印版本

mod error;
mod peer;
mod protocol;
mod server;

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use xt_proto::{Request, Response, DEFAULT_SOCKET_PATH, PROTOCOL_VERSION};

use error::{HelperError, Result};
use server::Helper;

#[derive(Parser, Debug)]
#[command(
    name = "xraytun-helper",
    version,
    about = "XrayTun 特权 helper：只负责建 utun、配置路由/DNS，并按快照回滚",
    disable_help_subcommand = true
)]
struct Cli {
    /// 监听/连接的 socket 路径。
    #[arg(long, global = true, default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 以守护进程方式运行（launchd 调用）。
    Run,
    /// 打印 helper 与 TUN 会话状态。
    Status,
    /// 回滚上次遗留的会话（网络排障用）。
    Restore,
    /// 打印版本与协议版本。
    Version,
}

fn main() {
    init_tracing();
    let cli = Cli::parse();

    let code = match cli.command.unwrap_or(Command::Run) {
        Command::Run => run_daemon(cli.socket),
        Command::Status => client_call(cli.socket, Request::Status),
        Command::Restore => client_call(cli.socket, Request::Restore),
        Command::Version => {
            println!(
                "xraytun-helper {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            0
        }
    };
    std::process::exit(code);
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    // launchd 会把 stderr 收进 plist 里配置的 StandardErrorPath，
    // 所以这里统一写 stderr 就够了，不需要自己实现日志轮转。
    let filter = EnvFilter::try_from_env("XRAYTUN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

// ---------------------------------------------------------------------------
// 守护进程模式
// ---------------------------------------------------------------------------

fn run_daemon(socket: PathBuf) -> i32 {
    if !is_root() {
        eprintln!(
            "错误：helper 必须以 root 运行（创建 utun 需要 root）。\n\
             开发调试请用：sudo {} run --socket /tmp/xraytun-helper.sock",
            std::env::args().next().unwrap_or_else(|| "xraytun-helper".into())
        );
        // 不直接退出：允许开发者在非 root 下只测协议层（TunUp 会失败，
        // 但 Hello/Status/Shutdown 都能正常走通）。
        if std::env::var("XRAYTUN_ALLOW_NONROOT").as_deref() != Ok("1") {
            return 77; // EX_NOPERM
        }
        eprintln!("警告：XRAYTUN_ALLOW_NONROOT=1，正在非 root 模式下运行（TUN 功能不可用）");
    }

    let helper = Helper::new(socket);

    // **关键**：启动即回滚上次遗留的会话。这是「helper 被 kill -9 之后
    // 用户不会永久断网」的唯一保障。
    helper.recover_from_crash();

    install_signal_handlers();

    match helper.serve() {
        Ok(()) => 0,
        Err(e) => {
            // helper 起不来，或者中途 accept 失败。
            // launchd 的 KeepAlive 会重启我们，所以先尽力把网络还原干净。
            tracing::error!(error = %e, "helper 服务循环退出，尝试回滚后退出");
            graceful_shutdown();
            1
        }
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid 无副作用。
    unsafe { libc::geteuid() == 0 }
}

/// 屏蔽 SIGTERM/SIGINT，并起一个线程用 `sigwait` **同步**等待。
///
/// 为什么不用 `signal()` 注册回调？因为回调运行在信号上下文里，
/// 不能安全地调用 `route`/`networksetup`（会 malloc、会阻塞）。
/// `sigwait` 把信号变成普通的同步等待，回调里就能随便做事了。
fn install_signal_handlers() {
    // 必须在创建任何其它线程之前屏蔽信号，这样新线程会继承该掩码。
    // SAFETY: sigset 操作与 pthread_sigmask 都是标准调用。
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }

    std::thread::spawn(|| {
        // SAFETY: 同上；在线程里重建一份集合。
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut sig: libc::c_int = 0;
        unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::sigwait(&set, &mut sig);
        }
        tracing::warn!(signal = sig, "收到终止信号，开始回滚网络配置");
        graceful_shutdown();
        std::process::exit(0);
    });
}

/// 尽力把系统恢复到「没有 TUN」的状态。
///
/// 一定不能失败就 panic：这是关机路径，任何 panic 都会让用户卡在断网状态。
fn graceful_shutdown() {
    // 先摘 socket：让 GUI 立刻看到「helper 不在」，而不是连上一个没人监听的
    // 陈旧文件拿到 ECONNREFUSED（那会被误读成「已安装但坏了」）。
    let sock = std::path::Path::new(xt_proto::DEFAULT_SOCKET_PATH);
    match std::fs::remove_file(sock) {
        Ok(()) => tracing::info!("已清理 socket 文件"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "清理 socket 文件失败"),
    }

    match xt_tun::macos::snapshot::SessionSnapshot::load() {
        Ok(Some(snap)) => {
            if let Some(pid) = snap.datapath_pid {
                tracing::info!(pid, "终止数据面进程");
                // SAFETY: kill 只读 pid。pid 可能已经不存在，失败无害。
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            match xt_tun::macos::controller::rollback(&snap) {
                Ok(()) => tracing::info!("网络配置已回滚"),
                Err(e) => tracing::error!(error = %e, "回滚失败，快照已保留，下次启动会重试"),
            }
        }
        Ok(None) => {}
        Err(e) => tracing::error!(error = %e, "读取快照失败"),
    }
}

// ---------------------------------------------------------------------------
// 客户端模式
// ---------------------------------------------------------------------------

fn client_call(socket: PathBuf, request: Request) -> i32 {
    match client_call_inner(&socket, request) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("失败: {e}");
            eprintln!(
                "提示：helper 可能没有运行。检查 `sudo launchctl print system/{}`，\n\
                 或查看 /Library/Logs/XrayTun/helper.log。",
                xt_proto::HELPER_LABEL
            );
            2
        }
    }
}

fn client_call_inner(socket: &std::path::Path, request: Request) -> Result<()> {
    let stream: UnixStream = protocol::connect(socket)
        .map_err(|e| error::internal(format!("连接 {} 失败: {e}", socket.display())))?;

    // 必须先握手。
    protocol::send(
        &stream,
        &Request::Hello {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL_VERSION,
            client_name: "xraytun-helper-cli".into(),
        },
    )?;
    let (hello, _): (Response, _) = protocol::recv(&stream)?;
    match hello {
        Response::Hello(info) => {
            println!("helper {} (协议 {})", info.helper_version, info.protocol);
            if let Some(stale) = &info.stale_session {
                println!("⚠︎ 发现未清理的会话: {stale}");
            }
        }
        Response::Error(e) => return Err(e),
        other => return Err(error::internal(format!("握手返回了意外响应: {other:?}"))),
    }

    protocol::send(&stream, &request)?;
    let (response, _): (Response, _) = protocol::recv(&stream)?;
    print_response(&response);
    Ok(())
}

fn print_response(response: &Response) {
    match response {
        Response::Hello(info) => println!("{info:#?}"),
        Response::Ok { message } => {
            if let Some(m) = message {
                println!("{m}");
            }
        }
        Response::Status(status) => {
            println!("helper pid: {}", status.pid);
            println!("数据面可用: {}", status.datapath_available);
            if status.sessions.is_empty() {
                println!("当前没有活跃的 TUN 会话");
            }
            for s in &status.sessions {
                println!(
                    "会话 {} | 接口 {} | {} 条路由 | DNS 已改: {} | 数据面 pid: {:?}",
                    s.session_id,
                    s.interface,
                    s.installed_routes.len(),
                    s.dns_modified,
                    s.datapath_pid
                );
            }
        }
        Response::Stats(stats) => {
            println!(
                "会话 {} | 运行 {}s | 下行 {} | 上行 {}",
                stats.session_id, stats.uptime_secs, human_bytes(stats.rx_bytes), human_bytes(stats.tx_bytes)
            );
        }
        Response::TunFd(info) => {
            println!("utun fd: {} (mtu {}, header {} 字节)", info.interface, info.mtu, info.header_len);
        }
        Response::Error(e) => {
            eprintln!("错误 [{:?}]: {}", e.code, e.message);
            std::process::exit(3);
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// 让 `HelperError` 在 main 里可打印。
#[allow(dead_code)]
fn describe(e: &HelperError) -> String {
    format!("{:?}: {}", e.code, e.message)
}
