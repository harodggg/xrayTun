//! 拉起 xt-daemon。
//!
//! # 这里到底做了什么，没做什么
//!
//! 做了：
//! * 解析 socket 路径（`XT_SOCKET` → 默认 `/tmp/xraytun-daemon.sock`）；
//! * 解析 daemon 二进制（`XT_DAEMON_BIN` → bundle 的 `Contents/Resources/xt-daemon`
//!   → 与主程序同级，后者是 `cargo build` 的开发形态）；
//! * **一次**非阻塞连接探测：已经有人在监听就复用，不再拉起第二个（`Server::bind`
//!   会删掉别人残留的 socket 文件，盲目 spawn 等于把正在服务的 daemon 踢掉）；
//! * 用**绝对路径 + argv** spawn（`--socket <path>`，可选 `--state-dir`），
//!   **永不经过 shell**；stdin 接 null，stderr 落日志文件；
//! * **等 daemon 自己报「已在监听」**才算拉起来（见 [`READY_PREFIX`]）。
//!
//! # 为什么壳必须等就绪（真机 S6 的第一个 bug）
//!
//! UI 的引导链 `hello → subscribe → status → …` **只跑一次**，失败不重发
//! （那是刻意的：本项目禁自动重试）。而冷启动时主窗口是先建的，页面脚本可能在
//! daemon `bind()` 完成**之前**就发出第一次 `hello` —— 那一次必然以 `io` 失败，
//! 界面就永久停在「连不上」。
//!
//! 所以壳必须等到「socket 已经存在」这个**事件**再建窗口。`xt-daemon` 为此把
//! `bind()` 与 `serve()` 分开，并在 `bind()` 成功后往 stdout 写一行就绪信号；
//! 这里读那一行。**这是事件驱动的等待，不是轮询**：`read_line` 挂在那里等对端写，
//! 没有 sleep，也没有「隔一会儿再看一眼」。
//!
//! 没做（都是占位，真机 S6 必须补）：
//! * **生命周期**：不 `kill_on_drop`、不持有 `Child` —— daemon 是独立进程，壳退出后
//!   它继续活着，下次启动会被探测复用。是否应该在退出时停掉它，是产品决定
//!   （代理工具退出通常应停），但那要接 `RunEvent::ExitRequested` 与信号，
//!   本轮不猜。
//! * **单实例竞态**：探测与 spawn 之间有 TOCTOU 窗口。两个壳同时启动可能各拉一个，
//!   后到的那个 daemon 会 bind 失败并退出（`xt-ipc` 的 `Server::bind` 拒绝覆盖非
//!   socket 文件 / 报错），但期间可能已经删掉前一个的 socket 文件。真机对齐时
//!   要么让 daemon 自己实现单实例（锁文件），要么由壳持有一个显式的启动锁。
//! * **state-dir / xray 路径**：只透传可选的 `XT_DAEMON_STATE_DIR`；daemon 的
//!   `--xray` / `XT_XRAY_BIN` 由它自己从环境读，壳不替它决定。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// 壳与 UI 共享的 socket 默认路径。**唯一的约定值**：壳用它拉起 daemon，并在建窗口时
/// 通过 `initialization_script` 写给 UI 的 `window.__XT_SOCKET__`。
///
/// 注意：`apps/ui/src/main.tsx` 自己的兜底是 `/run/xraytun/daemon.sock`（那是 Linux
/// 约定，macOS 上没有 `/run`）。壳不依赖那个兜底 —— 注入一定先于页面脚本执行，
/// 所以打包后 UI 用的是**这里**的值。
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/xraytun-daemon.sock";

/// 就绪信号前缀。与 `crates/xt-daemon/src/main.rs` 里 `println!` 的那一行**逐字对应**，
/// 改任何一边都是破坏协议。
const READY_PREFIX: &str = "xt-daemon: listening ";

/// 等就绪信号的上限。
///
/// 超时**不等于**失败：它只说明「我们没能确认它在监听」，调用方必须照实说，
/// 不能替 daemon 打包票。给 20 秒是因为首次启动时 macOS 会对新下载的可执行文件
/// 做一次 Gatekeeper 校验，那一下可能要好几秒。
const READY_DEADLINE: Duration = Duration::from_secs(20);

/// 覆盖 socket 路径（也是活体验收用的那个变量名）。
const SOCKET_ENV: &str = "XT_SOCKET";
/// 覆盖 daemon 二进制路径。
const BIN_ENV: &str = "XT_DAEMON_BIN";
/// 传给 daemon 的状态目录（可选）。
const STATE_DIR_ENV: &str = "XT_DAEMON_STATE_DIR";
/// daemon 日志文件路径（可选）。
const LOG_ENV: &str = "XT_DAEMON_LOG";

/// 一次「确保 daemon 在线」的结果。调用方只负责如实记日志，不据此改变别的行为。
#[derive(Debug)]
pub enum Launch {
    /// 探测到已有 daemon 在监听，复用它。
    AlreadyListening,
    /// 新拉起的 daemon，**并且已经确认它在监听**。
    Spawned { pid: Option<u32>, bin: PathBuf },
    /// 进程拉起来了，但**没能确认它在监听**（超时、或它在报就绪前就退了）。
    ///
    /// 单独一个变体而不是并进 `Spawned`：这两件事的把握不同，日志与界面提示也不该
    /// 用同一句话糊过去。注意这里的 `reason` 是「为什么没确认」，**不是**「它失败了」。
    NotListening { pid: Option<u32>, bin: PathBuf, reason: String },
    /// 没找到二进制或 spawn 失败。壳继续跑，界面会在第一条请求上如实报「连不上」。
    Unavailable(String),
}

/// socket 路径：`XT_SOCKET` → [`DEFAULT_SOCKET_PATH`]。
pub fn socket_path() -> PathBuf {
    match std::env::var_os(SOCKET_ENV) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(DEFAULT_SOCKET_PATH),
    }
}

/// 确保 daemon 在 `socket` 上在线，并且**已经确认可连**。
///
/// **异步**：探测、spawn 与等就绪都需要 tokio runtime context。
pub async fn ensure_daemon(socket: &Path) -> Launch {
    // 一次非阻塞尝试就够：能连上说明对面已经在服务（不 sleep、不轮询 —— 见不变量 I1）。
    if UnixStream::connect(socket).await.is_ok() {
        return Launch::AlreadyListening;
    }

    let Some(bin) = resolve_binary() else {
        return Launch::Unavailable(format!(
            "没找到 xt-daemon：设 {BIN_ENV} 指向它，或把可执行文件放到 bundle 的 \
             Contents/Resources/xt-daemon"
        ));
    };

    let mut command = Command::new(&bin);
    // 绝对路径 + argv，永不经过 shell。
    command.arg("--socket").arg(socket);
    if let Some(state_dir) = std::env::var_os(STATE_DIR_ENV) {
        if !state_dir.is_empty() {
            command.arg("--state-dir").arg(state_dir);
        }
    }
    command.stdin(Stdio::null());
    attach_stderr_log(&mut command);

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            // 先读就绪信号再返回：调用方（`lib.rs` 的 setup）等到这里返回才建主窗口，
            // 于是「窗口里的第一次 hello」必然落在已在监听的 daemon 上。
            let readiness = await_listening(&mut child).await;
            // 不 `wait`、不 `kill_on_drop`：Child 在这里被 drop，tokio 会在后台回收它，
            // 而 daemon 进程继续运行（这是**刻意的**独立进程语义，见模块文档）。
            match readiness {
                Ok(()) => Launch::Spawned { pid, bin },
                Err(reason) => Launch::NotListening { pid, bin, reason },
            }
        }
        Err(error) => Launch::Unavailable(format!("spawn {} 失败：{error}", bin.display())),
    }
}

/// 读子进程 stdout，直到那一行就绪信号出现（或超时、或它在报就绪之前就退了）。
///
/// 返回 `Err(原因)` 的语义见 [`Launch::NotListening`]：是「没确认」，不是「它失败了」。
async fn await_listening(child: &mut Child) -> Result<(), String> {
    let Some(stdout) = child.stdout.take() else {
        return Err("子进程没有可读的 stdout，无法确认它是否在监听".to_string());
    };
    let mut lines = BufReader::new(stdout).lines();

    let outcome = match tokio::time::timeout(READY_DEADLINE, lines.next_line()).await {
        Ok(Ok(Some(line))) => {
            if line.starts_with(READY_PREFIX) {
                Ok(())
            } else {
                Err(format!("daemon 的第一行 stdout 不是就绪信号（收到 {line:?}）"))
            }
        }
        // stdout 关了却没报就绪：它大概率启动失败并退出了（原因在 stderr，即日志文件）。
        Ok(Ok(None)) => {
            Err("daemon 在报出就绪信号之前就关闭了 stdout（很可能启动失败，见日志文件）"
                .to_string())
        }
        Ok(Err(error)) => Err(format!("读 daemon stdout 失败：{error}")),
        Err(_elapsed) => {
            Err(format!("等了 {} 秒仍未收到就绪信号", READY_DEADLINE.as_secs()))
        }
    };

    drain_stdout(lines);
    outcome
}

/// 把 stdout 剩下的行读走并追加到日志文件。
///
/// 为什么必须有人继续读：管道缓冲（macOS 上约 64 KiB）填满后，daemon 再往 stdout 写
/// 就会**阻塞**。daemon 正常不会往 stdout 写第二行，但「正常」不是不读的理由；
/// 真写了也不能丢，所以顺手落进同一个日志文件。
fn drain_stdout<R>(mut lines: tokio::io::Lines<R>)
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut log = open_log_for_append();
        while let Ok(Some(line)) = lines.next_line().await {
            let Some(file) = log.as_mut() else { continue };
            // 追加写是小块、低频的。这里用同步 IO 换「不必再引入一套句柄共享」，
            // 代价是偶尔占住 runtime 的一个 worker —— 对一个壳进程可以接受。
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
        }
    });
}

/// 打开日志文件（追加）。任何一步失败都返回 `None`。
fn open_log_for_append() -> Option<std::fs::File> {
    let path = log_path()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()
}

/// 把 daemon 的 stderr 落到日志文件；**stdout 必须留给壳读就绪信号**。
///
/// 为什么值得把 stderr 存下来：GUI 进程没有终端，daemon 的 tracing 会直接消失 ——
/// 出问题时手上将**没有任何证据**。宁可多一个日志文件。
fn attach_stderr_log(command: &mut Command) {
    // 就绪信号的通道，不能重定向走。
    command.stdout(Stdio::piped());

    let Some(path) = log_path() else {
        command.stderr(Stdio::null());
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(error) => {
            eprintln!(
                "xt shell: 打不开 daemon 日志 {}：{error}；改为丢弃 daemon 输出",
                path.display()
            );
            command.stderr(Stdio::null());
        }
    }
}

/// 解析 daemon 二进制。顺序固定，第一个存在的胜出：
/// 1. `XT_DAEMON_BIN`（显式覆盖；指了但不存在只警告，继续往下找，而不是直接失败）；
/// 2. `<exe>/../Resources/xt-daemon`（macOS bundle 形态）；
/// 3. `<exe>/xt-daemon`（`cargo build` 开发形态：两个二进制同在 `target/debug/`）。
pub fn resolve_binary() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(BIN_ENV) {
        if !raw.is_empty() {
            let candidate = PathBuf::from(raw);
            if candidate.is_file() {
                return Some(absolute(candidate));
            }
            eprintln!(
                "xt shell: {BIN_ENV}={} 不是文件，继续按 bundle 布局查找",
                candidate.display()
            );
        }
    }

    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("..").join("Resources").join("xt-daemon"), dir.join("xt-daemon")]
        .into_iter()
        .find(|candidate| candidate.is_file())
        .map(absolute)
}

/// 尽量把路径变成规范化的绝对路径（解析 `..` 与符号链接）。
/// `canonicalize` 失败（例如权限）就退回原路径 —— 它已经是绝对路径，spawn 仍能工作。
fn absolute(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// 日志文件：`XT_DAEMON_LOG` → `$HOME/Library/Logs/XrayTun/xt-daemon.log`。
fn log_path() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(LOG_ENV) {
        if !raw.is_empty() {
            return Some(PathBuf::from(raw));
        }
    }
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join("Library/Logs/XrayTun/xt-daemon.log"))
}
