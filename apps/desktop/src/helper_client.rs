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
            availability.error = Some("helper 尚未安装".into());
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
                availability.state = classify(&msg, socket_present);
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
fn classify(message: &str, socket_present: bool) -> HelperState {
    // ENOENT = 2：socket 文件不存在 → 从没装过
    if message.contains("os error 2") || message.contains("No such file or directory") {
        return if socket_present { HelperState::NotRunning } else { HelperState::NotInstalled };
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
        HelperState::NotRunning => format!(
            "助手进程没有在运行（socket 文件存在，但没有进程监听）。\n\
             常见于：刚卸载重装、或 launchd 启动的瞬间。\n\
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

/// socket 文件是否存在。注意**存在 ≠ 可用**：helper 可能没在跑，
/// 或者用户还没批准后台项。真正的判定在 [`HelperClient::availability`]。
pub fn socket_present(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_without_socket_is_explicit() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = client.availability(false);
        assert!(!a.reachable);
        assert!(a.error.as_deref().unwrap().contains("尚未安装"));
    }

    #[test]
    fn availability_with_missing_socket_file_reports_connection_error() {
        let mut client = HelperClient::new(Some(PathBuf::from("/tmp/definitely-not-here.sock")));
        let a = client.availability(true);
        assert!(!a.reachable);
        assert!(a.error.is_some(), "应给出可读的连接错误");
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
