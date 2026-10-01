//! daemon（普通用户）→ helperd（root 特权守护）的客户端。
//!
//! # 这一层负责什么
//!
//! helperd 以 root 运行，socket 是提权通道；daemon 只能用 [`xt_helperproto`]
//! 的**封闭指令集**跟它说话（六条指令，见该 crate 的模块文档）。本模块就是那套
//! 线上词汇的客户端：一条 [`HelperClient`] 对应一条 AF_UNIX 连接，每个方法
//! 发一帧 `Request`、读一帧 `Response`，不发明任何 helper 不认识的指令。
//!
//! # 时序契约：严格一问一答
//!
//! `TakeTunFd` 的 utun fd **不是**和响应帧同一条消息到达的。macOS 的 `AF_UNIX`
//! 不支持 `SOCK_SEQPACKET`（实测 `EPROTONOSUPPORT`），fd 只能作为「紧跟响应帧
//! 之后的一条 1 字节 `SCM_RIGHTS` 消息」发送。正确性依赖一条明确的协议约束
//! （见 helperd 侧 `fdpass` 的模块文档）：
//!
//! > 连接上同一时刻只有一条在途请求；发 `TakeTunFd` 的一方必须**先读完
//! > `Response::TakeTunFd` 帧，再收 fd**。
//!
//! 于是连接内部用一把 [`tokio::sync::Mutex`] 锁住：即使调用方从多个任务共享
//! 同一个 client，请求也不会交错，帧与 fd 的先后次序由结构本身保证，而不是靠
//! 每个调用点的自觉。
//!
//! # 取消安全：为什么这里可以用 `read_exact`
//!
//! `AsyncReadExt::read_exact` **不是取消安全的**：读到一半被 drop，已经读掉的
//! 字节留在 future 的局部变量里，连接就永久失步（下一轮会把帧体当长度头，得到
//! 一种「只在恰好被打断时复现」的随机 JSON 解析失败）。
//!
//! 本客户端成立的前提是**单任务顺序 await**：调用方（`flow.rs`）拿到 `Result`
//! 之前，不会把这些 future 放进 `tokio::select!`，也不会给它们套 `timeout`。
//! 这个前提是一份**契约**，不是巧合 —— 谁要在这个模块上加超时或并发 select，
//! 谁就得先把它改成带连接私有读缓冲的实现（helperd 侧的 `FrameReader` 就是那种
//! 写法）。写了注释是为了让下一个人能看见这条边界，而不是去踩它。
//!
//! # 错误映射
//!
//! * 连不上 / 握手被拒 / 协议版本不符 ⇒ [`ErrorCode::HelperUnavailable`]；
//! * 建连之后的读写 syscall 失败（含对端中途关闭）⇒ [`ErrorCode::Io`]；
//! * 本端序列化失败、响应变体不是期望的那一个、helper 回了别的会话 ⇒
//!   [`ErrorCode::Internal`]（**绝不静默**：宁可报错，也不把不对的东西当对的用）；
//! * helper 自己回的 `Response::Error { error }` ⇒ 原样把它的 `ErrorBody` 交出去，
//!   不重新包装 —— 对端的 `code` 是稳定字符串，UI 靠它做分支与本地化。
//!
//! # 平台
//!
//! `AF_UNIX` + `SCM_RIGHTS` 在 macOS 与 Linux 上都成立，本文件在两个平台都能
//! 编译；实际调用只发生在 macOS（helperd 的 socket 只在 macOS 上存在）。

use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt, Interest};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use xt_contract::error::{internal, ErrorBody, ErrorCode};
use xt_contract::MAX_FRAME_BYTES;
use xt_helperproto::{
    decode_response, encode_request, Request, Response, SessionRef, TunUpArgs,
    HELPER_PROTOCOL_VERSION,
};

/// 收 fd 时 `SCM_RIGHTS` 控制消息的缓冲大小。
///
/// `CMSG_SPACE(sizeof(RawFd))` 在 macOS / Linux 上都不超过 32 字节；64 是对齐之后
/// 的宽裕值。注意 `msg_controllen` 只声明**一个** fd 的空间 —— 多出来的控制数据
/// 会让内核置 `MSG_CTRUNC`，于是「对端多发了 fd」变成一条显式错误，而不是悄悄
/// 少收一个描述符。
const CONTROL_SPACE: usize = 64;

/// helperd 的客户端：一条 AF_UNIX 连接 + 严格一问一答。
///
/// 内部那把锁不是「防并发优化」，而是协议正确性的一部分：帧与紧随其后的
/// `SCM_RIGHTS` 消息必须成对消费，绝不能被第二个请求插队（见模块文档）。
#[derive(Debug)]
pub struct HelperClient {
    stream: Mutex<UnixStream>,
}

impl HelperClient {
    /// 连 helperd 的 socket，并做一次 `Status` 握手（协议版本不符即拒绝）。
    ///
    /// 握手不是可选步骤：它同时确认「socket 后面真的是 helperd」与「我们说的是
    /// 同一版线上词汇」。`Status` 是封闭指令集里唯一无副作用的查询指令，用它做
    /// 第一帧不会改变任何系统状态。
    pub async fn connect(socket_path: &Path) -> Result<Self, ErrorBody> {
        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            ErrorBody::new(
                ErrorCode::HelperUnavailable,
                format!("连接 helper socket {} 失败：{e}", socket_path.display()),
            )
        })?;
        let client = HelperClient { stream: Mutex::new(stream) };

        match client.exchange(Request::Status).await? {
            Response::Status { version, active_session } => {
                if version != HELPER_PROTOCOL_VERSION {
                    return Err(ErrorBody::new(
                        ErrorCode::HelperUnavailable,
                        format!(
                            "helper 协议版本不匹配：helper={version}，daemon={HELPER_PROTOCOL_VERSION}"
                        ),
                    ));
                }
                if let Some(session) = active_session {
                    // 不是错误：helper 上可能还留着一条会话（例如 daemon 刚重启）。
                    // 如实记一条日志，让「为什么 TunUp 会说已有会话」在现场可查。
                    tracing::info!(
                        session = %session.id,
                        interface = %session.interface,
                        "helper 上已有活跃 TUN 会话"
                    );
                }
                Ok(client)
            }
            other => Err(unexpected_response("Status", &other)),
        }
    }

    /// 建立 TUN 会话（两阶段启动的第一步）。
    pub async fn tun_up(&self, args: TunUpArgs) -> Result<SessionRef, ErrorBody> {
        match self.exchange(Request::TunUp { args }).await? {
            Response::TunUp { session } => Ok(session),
            other => Err(unexpected_response("TunUp", &other)),
        }
    }

    /// 取回 utun fd：**先读 `Response::TakeTunFd` 帧，再经 `SCM_RIGHTS` 收 fd**。
    ///
    /// 返回的 fd 由调用方持有并负责关闭（helper 侧仍持有一份，接口的生命周期
    /// 绑在所有副本上，所以交付 fd 不会让接口消失）。
    pub async fn take_tun_fd(&self, session: &SessionRef) -> Result<RawFd, ErrorBody> {
        // 整段（帧 + fd）都必须在一把锁里：中途放开锁就等于允许别的请求插到
        // 「帧已读、fd 未收」之间，那正是时序契约禁止的状态。
        let mut stream = self.stream.lock().await;
        write_request(&mut stream, &Request::TakeTunFd { session: session.clone() }).await?;
        let returned = match read_response(&mut stream).await? {
            Response::Error { error } => return Err(error),
            Response::TakeTunFd { session } => session,
            other => return Err(unexpected_response("TakeTunFd", &other)),
        };
        // 会话对不上说明 helper 回的是别的东西：绝不把「不知道是谁的 fd」交出去。
        // 放在收 fd **之前**判断，这样歧义状态下连 fd 都不会被消费。
        if returned != *session {
            return Err(internal(format!(
                "helper 返回的会话与请求不一致：请求 {}（{}），收到 {}（{}）",
                session.id, session.interface, returned.id, returned.interface
            )));
        }

        // 帧已经读完，fd 紧跟在后面（帧先、fd 后）。
        //
        // 这里用 `async_io` + `recvmsg(MSG_DONTWAIT)` 而不是裸 `recvmsg`：helperd
        // 是「write_all 响应帧 → sendmsg fd」两条 syscall，客户端完全可能在第二条
        // 之前就读完帧，此时直接 recvmsg 会 `EAGAIN`。`async_io` 会在 `WouldBlock`
        // 时清掉 readiness 并等下一次可读，既不忙等也不阻塞 tokio 的工作线程。
        let socket = stream.as_raw_fd();
        stream
            .async_io(Interest::READABLE, move || recv_fd_once(socket))
            .await
            .map_err(|e| io_error("接收 utun fd（SCM_RIGHTS）失败", e))
    }

    /// 两阶段启动的第二步：接管默认路由（helper 侧幂等）。
    pub async fn commit_routes(&self, session: &SessionRef) -> Result<(), ErrorBody> {
        match self.exchange(Request::CommitRoutes { session: session.clone() }).await? {
            Response::Ok => Ok(()),
            other => Err(unexpected_response("CommitRoutes", &other)),
        }
    }

    /// 拆掉会话并回滚路由 / DNS。
    pub async fn tun_down(&self, session: &SessionRef) -> Result<(), ErrorBody> {
        match self.exchange(Request::TunDown { session: session.clone() }).await? {
            Response::Ok => Ok(()),
            other => Err(unexpected_response("TunDown", &other)),
        }
    }

    /// 回滚上一次崩溃留下的半残状态；返回被回滚的接口名，`None` = 没有残留。
    ///
    /// daemon 暂不直接调它（helperd 自己启动时回滚自己的残留），保留这个方法是
    /// 协议封闭性的一部分（也为 S6 的「修复网络」留入口）。
    #[allow(dead_code)]
    pub async fn restore_stale(&self, state_dir: &str) -> Result<Option<String>, ErrorBody> {
        match self
            .exchange(Request::RestoreStale { state_dir: state_dir.to_string() })
            .await?
        {
            Response::RestoreStale { interface } => Ok(interface),
            other => Err(unexpected_response("RestoreStale", &other)),
        }
    }

    /// 发一帧请求，读一帧响应；helper 回的 `Response::Error` 在这里变成 `Err`。
    ///
    /// 需要「帧 + fd」两步的 [`HelperClient::take_tun_fd`] 不走这里 —— 它自己
    /// 持锁，把两步包在同一个临界区里。
    async fn exchange(&self, request: Request) -> Result<Response, ErrorBody> {
        let mut stream = self.stream.lock().await;
        write_request(&mut stream, &request).await?;
        match read_response(&mut stream).await? {
            Response::Error { error } => Err(error),
            other => Ok(other),
        }
    }
}

/// 写一帧请求（`长度前缀 || JSON`）并 flush。
async fn write_request(stream: &mut UnixStream, request: &Request) -> Result<(), ErrorBody> {
    let bytes = encode_request(request)?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|e| io_error("写 helper 请求帧失败", e))?;
    stream
        .flush()
        .await
        .map_err(|e| io_error("刷新 helper 请求帧失败", e))
}

/// 读一帧响应：先 4 字节长度头，再帧体，最后 `decode_response` 校验。
///
/// `read_exact` 的取消安全性前提见模块文档（单任务顺序 await + 连接被锁住）。
async fn read_response(stream: &mut UnixStream) -> Result<Response, ErrorBody> {
    let mut header = [0_u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| read_error("读 helper 响应帧长度前缀失败", e))?;
    let declared = u32::from_be_bytes(header);
    // 长度头**先**校验，别等帧体到齐：否则一条 0xFFFFFFFF 的长度头就能让我们
    // 分配 4 GiB。helperd 自己也做同样的上限检查（见它的 FrameReader）。
    if declared > MAX_FRAME_BYTES {
        return Err(internal(format!(
            "helper 响应帧体声明 {declared} 字节，超过上限 {MAX_FRAME_BYTES} 字节（连接已失步，必须重连）"
        )));
    }
    let mut frame = vec![0_u8; 4 + declared as usize];
    frame[..4].copy_from_slice(&header);
    stream
        .read_exact(&mut frame[4..])
        .await
        .map_err(|e| read_error("读 helper 响应帧体失败", e))?;
    decode_response(&frame).map_err(|e| internal(format!("helper 响应帧解码失败：{e}")))
}

/// 从 socket 上收一个 fd（`recvmsg` + `SCM_RIGHTS`）。
///
/// 返回 `WouldBlock` 表示「这条消息还没到」——**没有消费任何数据**，调用方
/// （`async_io`）会等可读再重试。只有确信对端刚发过 fd 时才可调用
/// （即刚读完 `Response::TakeTunFd` 帧）。
fn recv_fd_once(socket: RawFd) -> std::io::Result<RawFd> {
    let mut byte = [0_u8; 1];
    let mut iov = [libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    let mut control = [0_u8; CONTROL_SPACE];

    // SAFETY: 手工构造 `msghdr`。全零是合法初值（空指针 + 零长度），随后填入的
    // 指针都指向本函数栈上、活到 `recvmsg` 返回为止的缓冲。用 `zeroed` 而不是
    // 结构体字面量，是因为 `msghdr` 在部分目标上有私有填充字段，字面量构造不
    // 可移植，而 `CMSG_*` 只关心我们自己填的那几个字段。
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    // `CMSG_SPACE` 是 libc 的纯计算函数，无副作用。
    let control_space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) };
    msg.msg_controllen = control_space as _;

    // SAFETY: `msg` 的 iov / control 指向本函数存活到调用结束的缓冲；
    // `MSG_DONTWAIT` 保证即使 socket 意外处于阻塞模式也不会卡住工作线程
    // （正常路径上 tokio 已把 socket 设为非阻塞）。
    let read = unsafe { libc::recvmsg(socket, &mut msg as *mut libc::msghdr, libc::MSG_DONTWAIT) };
    if read < 0 {
        // EAGAIN / EWOULDBLOCK 会以 `ErrorKind::WouldBlock` 冒泡给 `try_io`，
        // 由它清掉 readiness 并等下一次可读。
        return Err(std::io::Error::last_os_error());
    }
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "对端在 fd 到达之前关闭了连接",
        ));
    }
    // 控制消息被截断 ⇒ fd 已经被内核丢掉了。必须显式报错，不能变成
    // 「帧说给了 fd、fd 却没到」这种最难排查的状态。
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "SCM_RIGHTS 控制消息被截断，utun fd 已丢失",
        ));
    }

    // SAFETY: `msg` 由 `recvmsg` 填好；`CMSG_*` 只在本函数的 control 缓冲
    // （`msg_controllen` 声明的范围）内游走，不会越界。
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg as *const libc::msghdr);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let mut fd: RawFd = -1;
                std::ptr::copy_nonoverlapping(
                    libc::CMSG_DATA(cmsg) as *const u8,
                    &mut fd as *mut RawFd as *mut u8,
                    std::mem::size_of::<RawFd>(),
                );
                if fd >= 0 {
                    // macOS **没有** `MSG_CMSG_CLOEXEC`，接收方必须自己设，否则
                    // fd 会随之后 exec 出去的子进程泄漏。失败只是少了加固。
                    set_cloexec(fd);
                    return Ok(fd);
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg as *const libc::msghdr, cmsg);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "响应帧之后没有附带 SCM_RIGHTS 控制消息",
    ))
}

/// 设置 `FD_CLOEXEC`（best-effort：失败不返回错误，也不丢 fd）。
fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl 只读写这个 fd 的标志位，不涉及指针。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// 读失败：EOF 单独给一句人话，其余原样带上原因。
fn read_error(what: &str, error: std::io::Error) -> ErrorBody {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        ErrorBody::new(ErrorCode::Io, format!("{what}：对端在帧读完之前关闭了连接"))
    } else {
        io_error(what, error)
    }
}

fn io_error(what: impl std::fmt::Display, error: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, format!("{what}：{error}"))
}

/// 响应变体不是期望的那一个 —— 协议被破坏了，绝不静默接受。
fn unexpected_response(expected: &str, actual: &Response) -> ErrorBody {
    internal(format!("helper 对 {expected} 的响应变体不对（期望 {expected}，实际 {actual:?}）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的发送侧：把 `fd` 以 `SCM_RIGHTS` 发给 `socket`（与 [`recv_fd_once`] 对称）。
    fn send_fd(socket: RawFd, fd: RawFd) -> std::io::Result<()> {
        let byte = [0_u8; 1];
        let mut iov = [libc::iovec {
            iov_base: byte.as_ptr() as *mut libc::c_void,
            iov_len: byte.len(),
        }];
        let mut control = [0_u8; CONTROL_SPACE];

        // SAFETY: 与 `recv_fd_once` 同一套手工构造；所有指针都指向本函数栈上、
        // 活到 `sendmsg` 返回为止的缓冲，控制消息里的 fd 由调用方保证有效。
        unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = iov.as_mut_ptr();
            msg.msg_iovlen = iov.len() as _;
            msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;

            let cmsg = libc::CMSG_FIRSTHDR(&msg as *const libc::msghdr);
            if cmsg.is_null() {
                return Err(std::io::Error::other(
                    "构造 SCM_RIGHTS 控制消息失败",
                ));
            }
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
            std::ptr::copy_nonoverlapping(
                &fd as *const RawFd as *const u8,
                libc::CMSG_DATA(cmsg),
                std::mem::size_of::<RawFd>(),
            );

            let sent = libc::sendmsg(socket, &msg as *const libc::msghdr, 0);
            if sent < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// 造一个「写进去 1 字节」的管道，返回 `(读端, 写端)`。
    fn one_byte_pipe() -> (RawFd, RawFd) {
        let mut pipe = [0 as RawFd; 2];
        // SAFETY: pipe 会把两个描述符写进本数组。
        let rc = unsafe { libc::pipe(pipe.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe 应可用");
        // SAFETY: pipe[1] 是刚创建的有效描述符，写 1 字节不会阻塞。
        let wrote = unsafe { libc::write(pipe[1], b"x".as_ptr() as *const libc::c_void, 1) };
        assert_eq!(wrote, 1, "应写进 1 字节");
        (pipe[0], pipe[1])
    }

    fn read_one_byte(fd: RawFd) -> u8 {
        let mut buf = [0_u8; 1];
        // SAFETY: fd 是调用方持有的有效读端；管道里已经有 1 字节。
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
        assert_eq!(n, 1, "应读出 1 字节");
        buf[0]
    }

    /// `SCM_RIGHTS` 的收端：拿回来的必须指向同一个「文件」，且带 `FD_CLOEXEC`
    /// （macOS 没有 `MSG_CMSG_CLOEXEC`，这一步只能由接收方补）。
    #[test]
    fn recv_fd_once_returns_the_sent_descriptor() {
        let (sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        let (read_end, write_end) = one_byte_pipe();

        send_fd(sender.as_raw_fd(), read_end).expect("发送 fd 应成功");
        let received = recv_fd_once(receiver.as_raw_fd()).expect("应收到 fd");
        assert_eq!(read_one_byte(received), b'x', "收到的 fd 应指向同一个管道");

        // SAFETY: fcntl(F_GETFD) 只读标志位。
        let flags = unsafe { libc::fcntl(received, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD 应成功");
        assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC, "收到的 fd 必须带 FD_CLOEXEC");

        // SAFETY: 关闭本测试创建的全部描述符。
        unsafe {
            libc::close(received);
            libc::close(read_end);
            libc::close(write_end);
        }
    }

    /// 读一帧请求（测试里当 helperd 的读侧）。
    async fn read_request_frame(stream: &mut UnixStream) -> Request {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).await.unwrap();
        let len = u32::from_be_bytes(header) as usize;
        let mut frame = vec![0_u8; 4 + len];
        frame[..4].copy_from_slice(&header);
        stream.read_exact(&mut frame[4..]).await.unwrap();
        xt_helperproto::decode_request(&frame).unwrap()
    }

    async fn write_response_frame(stream: &mut UnixStream, response: &Response) {
        let frame = xt_helperproto::encode_response(response).unwrap();
        stream.write_all(&frame).await.unwrap();
        stream.flush().await.unwrap();
    }

    /// `take_tun_fd` 必须**先读完帧、再收 fd**，并把 fd 原样交给调用方。
    #[tokio::test]
    async fn take_tun_fd_reads_the_frame_before_the_fd() {
        let (helper_side, daemon_side) = UnixStream::pair().unwrap();
        let session = SessionRef { id: "sess-1".into(), interface: "utun4".into() };
        let expected = session.clone();

        let helper = tokio::spawn(async move {
            let mut stream = helper_side;
            let request = read_request_frame(&mut stream).await;
            assert!(
                matches!(&request, Request::TakeTunFd { .. }),
                "helperd 应收到 TakeTunFd，实际 {request:?}"
            );
            // 协议时序：**先响应帧，再 fd**。
            write_response_frame(&mut stream, &Response::TakeTunFd { session }).await;

            let (read_end, write_end) = one_byte_pipe();
            send_fd(stream.as_raw_fd(), read_end).expect("发送 fd 应成功");
            // 本侧不再需要这两个描述符；管道由 daemon 收到的那份副本继续持有。
            // SAFETY: 关闭本任务创建的两个描述符。
            unsafe {
                libc::close(read_end);
                libc::close(write_end);
            }
        });

        let client = HelperClient { stream: Mutex::new(daemon_side) };
        let fd = client.take_tun_fd(&expected).await.expect("应收到 fd");
        assert_eq!(read_one_byte(fd), b'x', "收到的 fd 应指向 helper 写的那个管道");
        // SAFETY: fd 由本测试持有，用完关闭。
        unsafe { libc::close(fd) };
        helper.await.unwrap();
    }

    /// 变体不对时必须报 `Internal`，而不是「看起来成功了」。
    #[tokio::test]
    async fn a_wrong_response_variant_is_not_a_silent_success() {
        let (helper_side, daemon_side) = UnixStream::pair().unwrap();
        let helper = tokio::spawn(async move {
            let mut stream = helper_side;
            let _ = read_request_frame(&mut stream).await;
            // 对 TunUp 回一个 Ok：变体不对。
            write_response_frame(&mut stream, &Response::Ok).await;
        });

        let client = HelperClient { stream: Mutex::new(daemon_side) };
        let error = client
            .tun_up(TunUpArgs {
                addresses: vec!["198.18.0.1/15".into()],
                mtu: 1420,
                bypass_routes: Vec::new(),
                default_routes: Vec::new(),
                dns_servers: Vec::new(),
            })
            .await
            .expect_err("变体不对必须报错");
        assert_eq!(error.code, ErrorCode::Internal, "{error}");
        helper.await.unwrap();
    }

    /// helper 的 `Response::Error` 必须**原样**交出去（码与文案都不改写）。
    #[tokio::test]
    async fn helper_errors_are_propagated_verbatim() {
        let (helper_side, daemon_side) = UnixStream::pair().unwrap();
        let original = ErrorBody::new(ErrorCode::PermissionDenied, "对端签名校验未通过");
        let sent = original.clone();

        let helper = tokio::spawn(async move {
            let mut stream = helper_side;
            let _ = read_request_frame(&mut stream).await;
            write_response_frame(&mut stream, &Response::Error { error: sent }).await;
        });

        let client = HelperClient { stream: Mutex::new(daemon_side) };
        let error = client
            .exchange(Request::Status)
            .await
            .expect_err("Response::Error 必须变成 Err");
        assert_eq!(error, original, "helper 的错误必须原样透传");
        helper.await.unwrap();
    }
}
