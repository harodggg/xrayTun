//! 特权 helper 的客户端。
//!
//! 只做四件事：连接、握手、发一条请求、读回响应。
//! 所有网络配置的**决策**都在 helper 侧，这里只是把「我想要什么」翻译成
//! [`xt_proto::Request`]。这条边界是刻意的：GUI 被攻破也不能直接获得 root 能力。

use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use xt_proto::{
    HelloInfo, HelperError, Request, Response, TunFdInfo, DEFAULT_SOCKET_PATH, PROTOCOL_VERSION,
};

use crate::state::{HelperAvailability, HelperState};

/// 单条请求的超时。
///
/// 卡住比失败更糟：用户会看到「正在连接…」永远转下去，而且不会再试第二次。
/// 所以每个请求都有硬超时，超时按失败处理并允许重试。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// 启动 TUN 这种需要建卡 + 改路由 + 改 DNS 的操作给更长的预算。
const TUN_UP_TIMEOUT: Duration = Duration::from_secs(30);

pub type HelperResult<T> = Result<T, HelperError>;

pub struct HelperClient {
    socket: PathBuf,
    stream: Option<UnixStream>,
    pub hello: Option<HelloInfo>,
}

impl HelperClient {
    pub fn new(socket: Option<PathBuf>) -> Self {
        Self {
            socket: socket.unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH)),
            stream: None,
            hello: None,
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// 连接并握手。已经握手过就直接复用。
    ///
    /// **首次连接会重试**：安装/重启 helper 时存在一个真实的竞态窗口 ——
    /// launchd 已经 bootstrap 了，但进程还没 bind 到 socket。此时连上去
    /// 会拿到 `ECONNREFUSED`。
    ///
    /// 这个窗口只有几十毫秒，但如果不重试，用户会看到「安装成功」紧接着
    /// 「helper 连不上」——一个纯粹由时序造成的、看起来像 bug 的提示。
    pub fn ensure_connected(&mut self) -> HelperResult<()> {
        if self.stream.is_some() && self.hello.is_some() {
            return Ok(());
        }
        self.connect_fresh_with_retry()
    }

    fn connect_fresh_with_retry(&mut self) -> HelperResult<()> {
        const ATTEMPTS: u32 = 8;
        const INTERVAL: Duration = Duration::from_millis(200);

        let mut last: Option<HelperError> = None;
        for attempt in 0..ATTEMPTS {
            match self.connect_fresh() {
                Ok(()) => {
                    if attempt > 0 {
                        tracing::info!(attempt, "helper 在第 {attempt} 次重试后连接成功（启动竞态）");
                    }
                    return Ok(());
                }
                Err(e) => {
                    // 只有「连接被拒」和「文件不存在」值得重试。
                    // 权限不足、协议不匹配重试多少次都一样。
                    let retryable = e.message.contains("Connection refused")
                        || e.message.contains("No such file or directory")
                        || e.message.contains("os error 2")
                        || e.message.contains("os error 61");
                    last = Some(e);
                    if !retryable || attempt + 1 == ATTEMPTS {
                        break;
                    }
                    std::thread::sleep(INTERVAL);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            HelperError::new(xt_proto::ErrorCode::Internal, "连接 helper 失败（未知原因）")
        }))
    }

    fn connect_fresh(&mut self) -> HelperResult<()> {
        let stream = xt_proto::transport::connect(&self.socket).map_err(|e| {
            HelperError::new(
                xt_proto::ErrorCode::Internal,
                format!(
                    "无法连接 helper（{}）：{e}。helper 可能尚未安装或用户未在\
                     「系统设置 → 通用 → 登录项与扩展 → 后台允许」中启用它。",
                    self.socket.display()
                ),
            )
        })?;

        // 读超时保证不会无限等待；写超时通常用不到，但设上更安全。
        let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));

        xt_proto::transport::send(
            &stream,
            &Request::Hello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: PROTOCOL_VERSION,
                client_name: "XrayTun.app".to_string(),
            },
        )?;
        let (response, _): (Response, Option<RawFd>) = xt_proto::transport::recv(&stream)?;

        match response {
            Response::Hello(info) => {
                tracing::info!(
                    helper_version = %info.helper_version,
                    protocol = info.protocol,
                    "已连接 helper"
                );
                self.hello = Some(info);
                self.stream = Some(stream);
                Ok(())
            }
            Response::Error(e) => Err(e),
            other => Err(HelperError::new(
                xt_proto::ErrorCode::Internal,
                format!("握手返回了意外响应: {other:?}"),
            )),
        }
    }

    /// 发一条普通请求。
    pub fn call(&mut self, request: &Request) -> HelperResult<Response> {
        self.ensure_connected()?;
        let result = self.call_once(request);
        if result.is_err() {
            // 连接可能已经断了。丢掉连接，下次调用会重连。
            self.stream = None;
            self.hello = None;
        }
        result
    }

    fn call_once(&mut self, request: &Request) -> HelperResult<Response> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| HelperError::new(xt_proto::ErrorCode::Internal, "尚未连接 helper"))?;

        xt_proto::transport::send(stream, request)?;
        let (response, _): (Response, Option<RawFd>) = xt_proto::transport::recv(stream)?;

        // 超时设置是按连接生效的；TunUp 需要更宽松的预算，用完再恢复。
        match response {
            Response::Error(e) => Err(e),
            other => Ok(other),
        }
    }

    /// 启动 TUN 会话（只发 `TunUp`，不取 fd）。
    ///
    /// **单独把取 fd 拆出去**（见 [`Self::take_tun_fd`]）是刻意的：
    /// fd 传递有自己的时序约束（帧先、fd 后），把它混在一个函数里会让
    /// 「什么时候该调 recv_fd」变得隐式，而调错的后果是整条连接错位。
    /// 显式的两步调用让时序在调用点就看得见。
    pub fn tun_up(&mut self, request: xt_proto::TunUpRequest) -> HelperResult<()> {
        self.ensure_connected()?;
        if let Some(stream) = &self.stream {
            let _ = stream.set_read_timeout(Some(TUN_UP_TIMEOUT));
        }
        let result = self.call_once(&Request::TunUp(Box::new(request)));
        if let Some(stream) = &self.stream {
            let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
        }
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                self.stream = None;
                self.hello = None;
                Err(e)
            }
        }
    }

    /// 索取 utun fd。
    pub fn take_tun_fd(&mut self, session_id: &str) -> HelperResult<(TunFdInfo, RawFd)> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| HelperError::new(xt_proto::ErrorCode::Internal, "尚未连接 helper"))?;

        xt_proto::transport::send(
            stream,
            &Request::TakeTunFd { session_id: session_id.to_string() },
        )?;
        let (response, _): (Response, Option<RawFd>) = xt_proto::transport::recv(stream)?;
        match response {
            Response::TunFd(info) => {
                let fd = xt_proto::transport::recv_fd(stream)?;
                Ok((info, fd))
            }
            Response::Error(e) => Err(e),
            other => Err(HelperError::new(
                xt_proto::ErrorCode::Internal,
                format!("索取 fd 时收到意外响应: {other:?}"),
            )),
        }
    }

    /// 查询可用性，用于 UI 顶部的状态条。**失败不抛错**，而是编码进返回值。
    pub fn availability(&mut self, socket_present: bool) -> HelperAvailability {
        let mut availability = HelperAvailability {
            socket_present,
            ..Default::default()
        };
        if !socket_present {
            // **A10 修正版（task-183）**：socket 不在 ≠ 没装 ——
            // 它是守护进程 `serve()` 启动时才 bind、退出时删除的
            // （`crates/xt-helper/src/server.rs:114`、`:120-125`），
            // 所以「装了但没跑」（刚登录、launchd 还没拉起、刚退出）同样是 false。
            // 判据改成**安装产物**（plist / 二进制，路径与安装脚本同源）。
            match probe_install_artifacts() {
                InstallArtifacts::Present => {
                    availability.state = HelperState::NotRunning;
                    availability.error = Some(NOT_RUNNING_HINT.to_string());
                }
                InstallArtifacts::Absent => {
                    availability.state = HelperState::NotInstalled;
                    // 复用既有文案（`humanize` 里的 NotInstalled 那一支），不另写一份
                    availability.error = Some(humanize("", HelperState::NotInstalled));
                }
                InstallArtifacts::Unreadable(why) => {
                    availability.state = HelperState::Unknown;
                    availability.error = Some(format!(
                        "无法判断助手是否已安装（读取安装产物失败：{why}）。\
                         可在「设置 → 系统与助手」点「重新安装 helper」。"
                    ));
                }
            }
            return availability;
        }
        match self.ensure_connected() {
            Ok(()) => {
                availability.reachable = true;
                availability.state = HelperState::Ready;
                if let Some(hello) = &self.hello {
                    availability.version = Some(hello.helper_version.clone());
                    availability.protocol = Some(hello.protocol);
                    availability.tun_active = hello.tun_active;
                    availability.stale_session = hello.stale_session.clone();
                }
            }
            Err(e) => {
                let msg = e.message.clone();
                availability.state = classify(&msg);
                availability.needs_approval = availability.state == HelperState::NeedsApproval;
                availability.error = Some(humanize(&msg, availability.state));
            }
        }
        availability
    }

    /// 主动断开（切换设置里的 socket 路径时用）。
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.hello = None;
    }
}

/// 把连接错误归类成精确状态。
///
/// 判断依据是 `errno`，不是错误文本的模糊匹配 —— errno 是内核给的，
/// 文本是我们自己拼的。这里两种都用：errno 已经在文本里（`os error N`），
/// 因为 `xt_proto::transport` 把 io::Error 的 Display 拼进了消息。
///
/// ⚠️ **task-183（第四处同源假话）**：这里**不再**收 `socket_present`、也不再用它判
/// 「装没装」——「没看到 socket」只说明**守护进程此刻没在跑**（socket 是启动时 bind、
/// 退出时删的）。「装没装」由 [`HelperClient::availability`] 用**安装产物**判。
fn classify(message: &str) -> HelperState {
    // ENOENT = 2：socket 文件不存在 → **守护进程没在跑**（≠ 从没装过）
    if message.contains("os error 2") || message.contains("No such file or directory") {
        return HelperState::NotRunning;
    }
    // ECONNREFUSED = 61：文件在但没人监听 → 守护进程没跑
    if message.contains("os error 61") || message.contains("Connection refused") {
        return HelperState::NotRunning;
    }
    // EACCES = 13 / EPERM = 1：权限不足（不在 admin 组）
    if message.contains("os error 13")
        || message.contains("os error 1")
        || message.contains("Permission denied")
    {
        return HelperState::NotPermitted;
    }
    if message.contains("后台允许") || message.contains("not permitted") {
        return HelperState::NeedsApproval;
    }
    HelperState::Unknown
}

/// 把技术性错误翻译成「用户下一步该做什么」。
///
/// 这是这个函数存在的全部理由：`ECONNREFUSED` 本身不告诉用户任何事，
/// 而「守护进程没在运行，点『重启 helper』即可」是可执行的。
fn humanize(message: &str, state: HelperState) -> String {
    match state {
        HelperState::Ready => String::new(),
        HelperState::NotInstalled => "TUN 模式需要先安装特权助手。\
安装会写入 /Library/LaunchDaemons 与 /Library/PrivilegedHelperTools，需要一次管理员授权。\n\
系统代理模式不受影响。"
            .to_string(),
        // **task-183 预审修正**：这句必须对**两种成因**都为真 ——
        // ① socket 文件不在（守护进程退出时会删掉它，正是本卡修的主格）；
        // ② socket 文件在、但没有进程监听（`ECONNREFUSED`）。
        // 旧文案只写了 ② 的「socket 文件存在」，在 ① 那一格**与事实相反**。
        HelperState::NotRunning => format!(
            "助手进程没有在运行。socket 由守护进程启动时创建、退出时删除，\
             所以「它不存在」和「它在但没人监听」都只说明进程没跑 —— **都不代表没安装**。\n\
             常见于：刚登录还没被拉起、刚退出、或刚卸载重装。\n\
             点下面的「重启 helper」通常即可修复。\n\n\
             原始错误：{message}"
        ),
        HelperState::NotPermitted => format!(
            "当前用户不在 admin 组，无法连接特权助手。\n\
             请用管理员账号登录后重试。\n\n\
             原始错误：{message}"
        ),
        HelperState::NeedsApproval => format!(
            "助手已注册但被系统挡住。\n\
             请到「系统设置 → 通用 → 登录项与扩展 → 后台允许」中启用 XrayTun。\n\n\
             原始错误：{message}"
        ),
        HelperState::Unknown => message.to_string(),
    }
}

// ---------------------------------------------------------------------------
// A10 修正版（task-183）：判「装没装」要用**安装产物**，不是 socket 文件
// ---------------------------------------------------------------------------

/// 「装了但没在跑」的可执行文案 —— **socket 不在**那一格用的（那格我们没去 connect，
/// 所以没有「原始错误」可印，这是它与 [`humanize`] 唯一的差别）。
///
/// ⚠️ 口径必须与 `humanize(NotRunning)` **一致**（预审发现旧文案已改）：
/// 两处都不许出现「socket 文件存在」这种只对「文件在但没人监听」那一格成立的描述 ——
/// 本卡修的正是「socket 不在」这一格，写成那样就与事实相反。
pub(crate) const NOT_RUNNING_HINT: &str = "助手已安装，但进程没有在运行（socket 由守护进程启动时创建、退出时删除）。\
点「重启 helper」通常即可修复。";

/// 安装产物在不在 —— **这才是「装没装」的证据**（三分支，不许猜）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallArtifacts {
    /// plist 或二进制至少一个在 ⇒ **装过**。
    Present,
    /// 两者都确认不存在 ⇒ 从没装过。
    Absent,
    /// 读不出来（`EACCES` / `ENOTDIR` …）⇒ 如实说「无法判断」，**不许**吞成前两者。
    Unreadable(String),
}

/// 两个路径的探测结果 → 三分支（**纯函数**，可测）。
///
/// `metadata` 的语义：`Ok(())` 存在；`Err(NotFound)` 不存在；其它 `Err` ⇒ 读不出来。
fn classify_install_artifacts(
    plist: Result<(), std::io::Error>,
    binary: Result<(), std::io::Error>,
) -> InstallArtifacts {
    let mut present = false;
    let mut unreadable: Vec<String> = Vec::new();
    for (label, got) in [("plist", plist), ("binary", binary)] {
        match got {
            Ok(()) => present = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => unreadable.push(format!("{label}: {e}")),
        }
    }
    if present {
        // 有一个在就算装过（另一个读不出来也不影响这个结论）
        InstallArtifacts::Present
    } else if unreadable.is_empty() {
        InstallArtifacts::Absent
    } else {
        InstallArtifacts::Unreadable(unreadable.join("；"))
    }
}

/// 对**给定**两个路径做真实探测（测试用临时目录直接验这条）。
pub(crate) fn probe_install_artifacts_at(plist: &str, binary: &str) -> InstallArtifacts {
    classify_install_artifacts(
        std::fs::metadata(plist).map(|_| ()),
        std::fs::metadata(binary).map(|_| ()),
    )
}

/// 探测生产安装布局里的两个产物（路径来自 `xt_proto`，与安装脚本同源）。
///
/// `pub(crate)`：诊断文本（`commands/diagnostics.rs`）也要如实打印这个**磁盘事实**。
pub(crate) fn probe_install_artifacts() -> InstallArtifacts {
    #[cfg(test)]
    if let Some(fake) = TEST_INSTALL_ARTIFACTS.with(|c| c.borrow().clone()) {
        return fake;
    }
    probe_install_artifacts_at(xt_proto::HELPER_PLIST_PATH, xt_proto::HELPER_INSTALLED_PATH)
}

/// 诊断文本用的短名（与 `HelperState` 的 serde 表示一致）。
pub(crate) fn state_slug(state: HelperState) -> &'static str {
    match state {
        HelperState::Ready => "ready",
        HelperState::NotInstalled => "not_installed",
        HelperState::NotRunning => "not_running",
        HelperState::NotPermitted => "not_permitted",
        HelperState::NeedsApproval => "needs_approval",
        HelperState::Unknown => "unknown",
    }
}

/// 诊断文本用的安装产物短名。
pub(crate) fn artifact_slug(a: &InstallArtifacts) -> String {
    match a {
        InstallArtifacts::Present => "存在".to_string(),
        InstallArtifacts::Absent => "不存在".to_string(),
        InstallArtifacts::Unreadable(why) => format!("读不出来（{why}）"),
    }
}

#[cfg(test)]
thread_local! {
    /// 测试注入的三分支结果（真实 `/Library` 在 CI/开发机上不可造）。
    static TEST_INSTALL_ARTIFACTS: std::cell::RefCell<Option<InstallArtifacts>> =
        const { std::cell::RefCell::new(None) };
}

/// 在 `f()` 期间使用注入的安装产物结论；**退出（含 panic）时自动还原**。
///
/// `#[cfg(test)]` —— 生产二进制里没有这个入口（先例：`xt_core::store::with_log_limits`）。
#[cfg(test)]
pub(crate) fn with_install_artifacts<R>(fake: InstallArtifacts, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<InstallArtifacts>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_INSTALL_ARTIFACTS.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = TEST_INSTALL_ARTIFACTS.with(|c| c.borrow_mut().take());
    TEST_INSTALL_ARTIFACTS.with(|c| *c.borrow_mut() = Some(fake));
    let _guard = Restore(prev);
    f()
}

/// helper 状态 → **启动时那一行日志**（task-183 的第二处：与 UI 必须同一判据）。
///
/// 返回 `None` = 无需落日志。抽成纯函数是为了让「同一句错话进了日志」这件事**可被测试**。
pub(crate) fn helper_startup_log(a: &HelperAvailability) -> Option<(&'static str, String)> {
    if a.socket_present {
        if a.reachable {
            return Some((
                "info",
                format!("helper {} 已就绪", a.version.clone().unwrap_or_default()),
            ));
        }
        return Some((
            "warn",
            format!(
                "helper 不可连接：{}",
                a.error.clone().unwrap_or_else(|| "未知原因".into())
            ),
        ));
    }
    match a.state {
        // **装了但没跑**：不许说「尚未安装」（socket 每次退出都会被删）
        HelperState::NotRunning => Some((
            "warn",
            format!("{NOT_RUNNING_HINT}TUN 模式暂不可用；系统代理模式仍可正常使用。"),
        )),
        HelperState::NotInstalled => Some((
            "warn",
            "helper 尚未安装，TUN 模式不可用（系统代理模式仍可正常使用）".to_string(),
        )),
        // 读不出来 ⇒ 只说「无法判断」+ 原始原因
        _ => Some((
            "warn",
            format!(
                "无法判断 helper 是否已安装：{}",
                a.error.clone().unwrap_or_else(|| "未知原因".into())
            ),
        )),
    }
}

/// socket 文件是否存在。注意**存在 ≠ 可用**：helper 可能没在跑，
/// 或者用户还没批准后台项。真正的判定在 [`HelperClient::availability`]。
pub fn socket_present(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A10 修正版主回归**：socket 不在 + **安装产物在** ⇒ 「已安装但没运行」，
    /// **不许**说「尚未安装」（socket 每次退出都会被删 ⇒ 没 socket ≠ 没装）。
    #[test]
    fn availability_without_socket_but_installed_reports_not_running() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = with_install_artifacts(InstallArtifacts::Present, || client.availability(false));
        assert!(!a.reachable);
        assert_eq!(a.state, HelperState::NotRunning);
        let e = a.error.expect("必须给可执行文案");
        assert!(!e.contains("尚未安装"), "装了但没跑 ≠ 没装：{e}");
        assert!(e.contains("重启 helper"), "{e}");
    }

    #[test]
    fn availability_without_socket_and_artifacts_missing_reports_not_installed() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = with_install_artifacts(InstallArtifacts::Absent, || client.availability(false));
        assert_eq!(a.state, HelperState::NotInstalled);
        let e = a.error.expect("必须给可执行文案");
        assert!(e.contains("先安装特权助手"), "{e}");
        assert!(!e.contains("已安装，但进程没有在运行"), "{e}");
    }

    /// 读不出来（`EACCES` 等）⇒ **`Unknown` + 原始原因**，不许吞成前两者。
    #[test]
    fn availability_without_socket_and_unreadable_artifacts_reports_unknown() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = with_install_artifacts(
            InstallArtifacts::Unreadable("plist: Permission denied (os error 13)".into()),
            || client.availability(false),
        );
        assert_eq!(a.state, HelperState::Unknown);
        let e = a.error.expect("必须给原因");
        assert!(e.contains("无法判断"), "{e}");
        assert!(e.contains("Permission denied"), "原始错误要带上：{e}");
    }

    /// 三分支判据是**纯函数**：存在 / 都不存在 / 读不出来。
    #[test]
    fn classify_install_artifacts_is_three_way() {
        use std::io::{Error, ErrorKind};
        let missing = || Err(Error::new(ErrorKind::NotFound, "no such file"));
        assert_eq!(classify_install_artifacts(Ok(()), missing()), InstallArtifacts::Present);
        assert_eq!(classify_install_artifacts(missing(), Ok(())), InstallArtifacts::Present);
        assert_eq!(classify_install_artifacts(missing(), missing()), InstallArtifacts::Absent);
        let denied = Err(Error::new(ErrorKind::PermissionDenied, "denied"));
        assert!(matches!(
            classify_install_artifacts(denied, missing()),
            InstallArtifacts::Unreadable(_)
        ));
    }

    /// 真实文件系统的三分支（临时目录；**不碰 `/Library`**）。
    #[test]
    fn probe_install_artifacts_at_reads_the_filesystem_three_ways() {
        let dir = std::env::temp_dir().join(format!("xt-artifacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plist = dir.join("com.xraytun.helper.plist");
        let binary = dir.join("com.xraytun.helper");
        std::fs::write(&plist, b"plist").unwrap();

        assert_eq!(
            probe_install_artifacts_at(&plist.to_string_lossy(), &binary.to_string_lossy()),
            InstallArtifacts::Present,
            "有一个在就算装过"
        );
        let _ = std::fs::remove_file(&plist);
        assert_eq!(
            probe_install_artifacts_at(&plist.to_string_lossy(), &binary.to_string_lossy()),
            InstallArtifacts::Absent
        );
        // ENOTDIR：`/dev/null/child` 一定读不出来（不是 NotFound）
        assert!(
            matches!(
                probe_install_artifacts_at("/dev/null/child", &binary.to_string_lossy()),
                InstallArtifacts::Unreadable(_)
            ),
            "读不出来必须如实归类，不许当成「不存在」"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **第四处同源假话（`classify`）**：socket 不存在时的 ENOENT **不再**判成 NotInstalled。
    #[test]
    fn classify_no_longer_calls_a_missing_socket_not_installed() {
        assert_eq!(
            classify("connect: No such file or directory (os error 2)"),
            HelperState::NotRunning,
            "socket 被删只说明守护进程没在跑，不等于从没装过"
        );
        assert_eq!(
            classify("Connection refused (os error 61)"),
            HelperState::NotRunning
        );
        assert_eq!(classify("Permission denied (os error 13)"), HelperState::NotPermitted);
    }

    /// **第二处假话（`lib.rs` 那行日志）的同一判据**：装了没跑 ⇒ 说「已安装但没在运行」。
    #[test]
    fn startup_log_says_installed_but_not_running() {
        let a = HelperAvailability {
            socket_present: false,
            state: HelperState::NotRunning,
            error: Some(NOT_RUNNING_HINT.into()),
            ..Default::default()
        };
        let (level, msg) = helper_startup_log(&a).expect("必须落一行");
        assert_eq!(level, "warn");
        assert!(!msg.contains("尚未安装"), "同一句错话不许再进日志：{msg}");
        assert!(
            msg.contains("已安装，但进程没有在运行") && msg.contains("重启 helper"),
            "{msg}"
        );
    }

    #[test]
    fn startup_log_stays_honest_for_the_other_branches() {
        let not_installed = HelperAvailability {
            socket_present: false,
            state: HelperState::NotInstalled,
            error: Some("x".into()),
            ..Default::default()
        };
        let (_, msg) = helper_startup_log(&not_installed).unwrap();
        assert!(msg.contains("尚未安装"), "{msg}");

        let unknown = HelperAvailability {
            socket_present: false,
            state: HelperState::Unknown,
            error: Some("读不出来".into()),
            ..Default::default()
        };
        let (_, msg) = helper_startup_log(&unknown).unwrap();
        assert!(msg.contains("无法判断"), "{msg}");

        let unreachable = HelperAvailability {
            socket_present: true,
            reachable: false,
            error: Some("ECONNREFUSED".into()),
            ..Default::default()
        };
        let (_, msg) = helper_startup_log(&unreachable).unwrap();
        assert!(msg.contains("不可连接"), "{msg}");

        let ready = HelperAvailability {
            socket_present: true,
            reachable: true,
            version: Some("0.8.38".into()),
            state: HelperState::Ready,
            ..Default::default()
        };
        let (level, msg) = helper_startup_log(&ready).unwrap();
        assert_eq!(level, "info");
        assert!(msg.contains("已就绪"), "{msg}");
    }

    /// **反例（卡面要求）**：`socket_present == true` 但连不上 ⇒ **不得**判成 `NotInstalled`。
    ///
    /// 这里 `true` 是**注入的**（模拟「探测到 socket 之后、连接之前它被删掉」或
    /// 「文件在但没人监听」这两种真实竞态）—— 无论如何都说明**装过**。
    #[test]
    fn availability_with_socket_flag_but_connect_fails_is_not_not_installed() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = client.availability(true);
        assert!(!a.reachable);
        assert!(a.error.is_some(), "应给出可读的连接错误");
        assert_ne!(
            a.state,
            HelperState::NotInstalled,
            "socket 探到过 ⇒ 装过，不许说「尚未安装」：{:?}",
            a.state
        );
        assert_eq!(a.state, HelperState::NotRunning, "改判成「没在跑」");
    }

    /// **预审 1 的回归**：`humanize(NotRunning)` 的文案必须对**两种成因**都为真 ——
    /// 不许再出现「socket 文件存在」（那对「socket 不在」那一格与事实相反）。
    #[test]
    fn humanize_not_running_is_true_for_both_causes() {
        let refused = humanize("Connection refused (os error 61)", HelperState::NotRunning);
        let enoent = humanize("No such file or directory (os error 2)", HelperState::NotRunning);
        for m in [&refused, &enoent] {
            assert!(
                !m.contains("socket 文件存在"),
                "这句只对「文件在但没人监听」成立，不许当通用句：{m}"
            );
            assert!(m.contains("退出时删除"), "要给出真实成因：{m}");
            assert!(m.contains("重启 helper"), "{m}");
        }
    }

    #[test]
    fn call_without_helper_fails_fast() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let err = client.call(&Request::Status).unwrap_err();
        assert!(err.message.contains("helper"), "{}", err.message);
    }

    #[test]
    fn socket_present_reflects_filesystem() {
        let dir = std::env::temp_dir().join(format!("xt-client-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.sock");
        std::fs::write(&f, b"").unwrap();
        assert!(socket_present(&f));
        assert!(!socket_present(&dir.join("nope.sock")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
