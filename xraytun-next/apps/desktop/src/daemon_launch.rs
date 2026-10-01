//! 拉起 xt-daemon —— **占位实现，真机 S6 再对齐**。
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
//!   **永不经过 shell**；stdin 接 null，stdout/stderr 落到日志文件。
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
//! * **打包**：`tauri.conf.json` 的 `bundle.resources` 还没有把 `xt-daemon` 映射进
//!   `Contents/Resources/`，见 `apps/desktop/binaries/README.md`。
//! * **state-dir / xray 路径**：只透传可选的 `XT_DAEMON_STATE_DIR`；daemon 的
//!   `--xray` / `XT_XRAY_BIN` 由它自己从环境读，壳不替它决定。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::net::UnixStream;
use tokio::process::Command;

/// 壳与 UI 共享的 socket 默认路径。**唯一的约定值**：壳用它拉起 daemon，并在建窗口时
/// 通过 `initialization_script` 写给 UI 的 `window.__XT_SOCKET__`。
///
/// 注意：`apps/ui/src/main.tsx` 自己的兜底是 `/run/xraytun/daemon.sock`（那是 Linux
/// 约定，macOS 上没有 `/run`）。壳不依赖那个兜底 —— 注入一定先于页面脚本执行，
/// 所以打包后 UI 用的是**这里**的值。
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/xraytun-daemon.sock";

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
    /// 新拉起的 daemon。
    Spawned { pid: Option<u32>, bin: PathBuf },
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

/// 确保 daemon 在 `socket` 上在线。**异步**：探测与 spawn 都需要 tokio runtime context。
pub async fn ensure_daemon(socket: &Path) -> Launch {
    // 一次非阻塞尝试就够：能连上说明对面已经在服务（不 sleep、不轮询 —— 见不变量 I1）。
    if UnixStream::connect(socket).await.is_ok() {
        return Launch::AlreadyListening;
    }

    let Some(bin) = resolve_binary() else {
        return Launch::Unavailable(format!(
            "没找到 xt-daemon：设 {BIN_ENV} 指向它，或把可执行文件放到 bundle 的 \
             Contents/Resources/xt-daemon（打包尚未接线，见 binaries/README.md）"
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
    attach_log(&mut command);

    match command.spawn() {
        Ok(child) => {
            // 不 `wait`、不 `kill_on_drop`：Child 在这里被 drop，tokio 会在后台回收它，
            // 而 daemon 进程继续运行（这是**刻意的**独立进程语义，见模块文档）。
            Launch::Spawned { pid: child.id(), bin }
        }
        Err(error) => Launch::Unavailable(format!("spawn {} 失败：{error}", bin.display())),
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

/// 把 daemon 的 stdout/stderr 落到日志文件；任何一步失败就退化成丢弃输出。
///
/// 为什么值得做：GUI 进程没有终端，daemon 的 tracing 写 stderr 会直接消失 ——
/// 出问题时手上将**没有任何证据**。宁可多一个日志文件。
fn attach_log(command: &mut Command) {
    let Some(path) = log_path() else {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "xt shell: 打不开 daemon 日志 {}：{error}；改为丢弃 daemon 输出",
                path.display()
            );
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
            return;
        }
    };
    // 两个半都要拿文件句柄（stdout/stderr 各一个）。`try_clone` 失败就只接 stdout。
    match file.try_clone() {
        Ok(stderr) => {
            command.stdout(Stdio::from(file));
            command.stderr(Stdio::from(stderr));
        }
        Err(error) => {
            eprintln!("xt shell: 复制日志句柄失败：{error}；daemon stderr 将被丢弃");
            command.stdout(Stdio::from(file));
            command.stderr(Stdio::null());
        }
    }
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
