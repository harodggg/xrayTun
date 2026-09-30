//! xt-helperd —— 特权 helper 守护进程（root，macOS 专用）。
//!
//! # 它是谁的、边界在哪
//!
//! 这是 XrayTun.app 的**特权面**：以 root 运行、唯一有权限改系统网络配置的进程。
//! 数据面（xray）以普通用户运行，helper 只做「建/拆 utun、装/撤路由、改/还原
//! DNS」这四件事，其余一概不做。
//!
//! 权限边界由三层共同保证（见 `docs/architecture/ARCHITECTURE.md` §1.2）：
//!
//! 1. **封闭指令集**：只认识 `xt-helperproto` 里的 6 条指令。未知 `cmd` / `result`
//!    tag 在反序列化时直接报错，不存在「收到不认识的指令就忽略」的路径。
//! 2. **对端校验**：`accept` 之后、处理任何指令之前，先读对端的 audit token
//!    并用 `SecCode` 校验它是不是预期的 daemon（见 [`trust`]）。不匹配 →
//!    回一帧 `Response::Error` 后关闭连接。
//! 3. **永不经过 shell**：helperd 自己不 spawn 任何东西；`xt-macosnet` 内部对外部
//!    命令一律用**绝对路径 + argv 数组**调用，任何来自对端的字符串都不可能被
//!    解释成 shell 语法。
//!
//! # 平台门控
//!
//! helperd 是 macOS 专用。四个真实模块（[`server`] / [`dispatch`] / [`fdpass`] /
//! [`trust`]）在 `#[cfg(target_os = "macos")]` 下声明，Linux 上**整个不编译**；
//! 非 macOS 只留一个显式失败的 `main`。这样 `cargo check --workspace` 在 Linux
//! 上仍然绿，且不会假装「helper 在 Linux 上能用」。
//!
//! # 文件职责
//!
//! * `server.rs` —— AF_UNIX 监听 + 连接循环 + 帧读写（取消安全）。
//! * `dispatch.rs` —— `helperproto::Request` → `xt_macosnet` 的封闭分发 + 会话状态。
//! * `fdpass.rs` —— `SCM_RIGHTS` 传 fd + 读对端凭据 / audit token。
//! * `trust.rs` —— 对端 `SecCode` 校验（签名或 cdhash 绑定，fail-closed）。

#[cfg(target_os = "macos")]
mod dispatch;
#[cfg(target_os = "macos")]
mod fdpass;
#[cfg(target_os = "macos")]
mod server;
#[cfg(target_os = "macos")]
mod trust;

/// macOS 入口：参数解析 → 日志 → 崩溃残留回滚 → 监听 → 服务。
///
/// 放在子模块里，是为了让下面两个平台各自只有一个 `main`，且真实代码全部落在
/// `#[cfg(target_os = "macos")]` 内。
#[cfg(target_os = "macos")]
mod app {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::Arc;

    use clap::Parser;

    #[derive(Parser, Debug)]
    #[command(name = "xt-helperd", about = "xraytun-next 特权 helper 守护进程（root，macOS）")]
    struct Cli {
        /// AF_UNIX socket 路径（root 所有，权限 root:admin 0660）
        #[arg(long, default_value = "/var/run/xraytun-helper.sock")]
        socket: PathBuf,

        /// helper 的 root 状态目录（快照落盘位置；只允许白名单前缀）
        #[arg(long, default_value = "/Library/Application Support/XrayTun")]
        state_dir: PathBuf,

        /// 日志级别
        #[arg(long, default_value = "info", value_parser = ["error", "warn", "info", "debug"])]
        log_level: String,
    }

    #[tokio::main]
    pub async fn run() -> ExitCode {
        let cli = Cli::parse();

        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(cli.log_level.clone()))
            .try_init();

        // helper 是 root 守护进程：非 root 运行时建卡/改路由一定会失败，而且
        // 「以错误身份运行的特权进程」本身就是安全问题。所以入口 fail-closed。
        // SAFETY: geteuid 只读进程的 effective uid，无副作用。
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("xt-helperd 必须以 root 运行（当前 euid != 0）：特权 helper 拒绝降级启动");
            return ExitCode::FAILURE;
        }

        // 启动前先回滚上一次崩溃留下的半残状态。判据与回滚逻辑在 xt-macosnet，
        // 这里只负责「用哪个目录」。目录必须先过白名单：它是 helper 以 root
        // 去读快照、并据此改系统网络的依据，绝不接受任意路径。
        if let Err(error) = crate::dispatch::validate_state_dir(&cli.state_dir) {
            eprintln!("--state-dir {} 不合法：{error}", cli.state_dir.display());
            return ExitCode::FAILURE;
        }
        match xt_macosnet::restore_stale(&cli.state_dir) {
            Ok(Some(interface)) => {
                tracing::warn!(interface = %interface, "已回滚上次崩溃遗留的 TUN 会话");
            }
            Ok(None) => tracing::info!("没有需要回滚的遗留会话"),
            Err(error) => {
                // 回滚失败意味着网络可能仍处于异常状态（例如 DNS 停在隧道值）。
                // 不隐瞒：这是用户会真实感知到的故障。
                tracing::error!(error = %error, "回滚遗留会话失败 —— 网络可能仍处于异常状态");
            }
        }

        // 对端授权策略在**构建期**决定，并在启动日志里可见（排障第一眼要看它）。
        let policy = Arc::new(crate::trust::TrustPolicy::from_build_env());
        tracing::info!(policy = %policy.describe(), "对端授权策略");

        let server = match crate::server::Server::bind(&cli.socket, policy).await {
            Ok(server) => server,
            Err(error) => {
                eprintln!("xt-helperd 绑定 socket 失败：{error}");
                return ExitCode::FAILURE;
            }
        };
        let state = Arc::new(crate::dispatch::State::new());

        let mut code = ExitCode::SUCCESS;
        // Ctrl-C 是关闭事件，不是轮询：`select!` 等到其中一个分支就收工。
        tokio::select! {
            result = server.serve(state) => {
                if let Err(error) = result {
                    tracing::error!(error = %error, "服务循环退出");
                    code = ExitCode::FAILURE;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("收到中断信号，正在退出");
            }
        }

        // 退出前摘掉 socket 文件：残留的 socket 会让客户端看到 ECONNREFUSED
        // （文件在、没人监听），是最难解释的一种状态。
        server.cleanup();
        code
    }
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    app::run()
}

/// 非 macOS 平台：helperd 没有实现，也**不假装**能跑。
///
/// 用 `eprintln!` + `exit(2)` 而不是「编译成空壳正常退出」：脚本/打包流水线
/// 若误在 Linux 上启动它，必须立刻拿到一个非零退出码与一句人话。
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "xt-helperd 是 macOS 专用的特权 helper（utun / route / DNS / SecCode 校验），\
         在本平台没有实现，也不会提供降级行为"
    );
    std::process::exit(2);
}
