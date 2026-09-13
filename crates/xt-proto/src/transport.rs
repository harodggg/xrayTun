//! GUI ↔ helper 的传输层。
//!
//! # 一个被实测推翻的设计（值得记下来）
//!
//! 最初的方案是用 `AF_UNIX` + **`SOCK_SEQPACKET`**：报文边界天然保留，
//! 既不需要自己做长度前缀分帧，`SCM_RIGHTS` 附带的 fd 也一定和它所属的
//! 那条消息一起到达，看起来是「两个问题一起解决」的漂亮方案。
//!
//! **但 macOS 不支持它。** 实测 `socketpair(AF_UNIX, SOCK_SEQPACKET, 0)` 与
//! `socket(AF_UNIX, SOCK_SEQPACKET, 0)` 都返回 `EPROTONOSUPPORT (errno 43)`。
//! `SOCK_SEQPACKET` 对 `AF_UNIX` 的支持是 Linux 特有的，Darwin 没有实现。
//! （`tests::seqpacket_is_not_supported_on_macos` 把这个事实钉在测试里，
//! 以免将来有人又想「优化」回去。）
//!
//! # 于是最终方案
//!
//! `SOCK_STREAM` + `u32` 大端长度前缀分帧，fd 走「紧跟在帧后面的独立 1 字节
//! `SCM_RIGHTS` 消息」。这样做的正确性依赖一条明确的协议约束：
//!
//! > **连接是严格的一问一答，且同一时刻只有一条在途消息。**
//! > 客户端发出 `TakeTunFd` 后必须先读完 `Response::TunFd` 帧，再读 fd。
//!
//! 因为双方严格同步，fd 消息不可能与任何其它帧交错；这不是「碰巧能跑」，
//! 而是协议规定的时序。helper 每条连接一个线程也保证了这一点。

use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{ErrorCode, HelperError, MAX_MESSAGE};

pub type Result<T, E = HelperError> = std::result::Result<T, E>;

fn internal(msg: impl Into<String>) -> HelperError {
    HelperError::new(ErrorCode::Internal, msg)
}

// ---------------------------------------------------------------------------
// 分帧
// ---------------------------------------------------------------------------

/// 发送一条消息。
pub fn send<T: Serialize>(stream: &UnixStream, msg: &T) -> Result<()> {
    let bytes = encode(msg)?;
    let mut sock = stream;
    sock.write_all(&bytes)
        .and_then(|_| sock.flush())
        .map_err(|e| internal(format!("发送失败: {e}")))
}

fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(msg).map_err(|e| internal(format!("序列化失败: {e}")))?;
    if payload.len() > MAX_MESSAGE {
        return Err(internal(format!("消息过大: {} > {MAX_MESSAGE}", payload.len())));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// 接收一条消息，并取出在这条消息**之前**由 [`send_fd`] 送来的 fd（如果有）。
///
/// 绝大多数消息不带 fd，此时第二项是 `None`。
pub fn recv<T: DeserializeOwned>(stream: &UnixStream) -> Result<(T, Option<RawFd>)> {
    let mut sock = stream;

    let mut len_buf = [0u8; 4];
    sock.read_exact(&mut len_buf)
        .map_err(|e| internal(format!("读取帧长度失败（连接可能已断开）: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MESSAGE {
        return Err(internal(format!("声明的帧长度过大: {len} > {MAX_MESSAGE}")));
    }

    let mut payload = vec![0u8; len];
    sock.read_exact(&mut payload)
        .map_err(|e| internal(format!("读取帧内容失败: {e}")))?;

    let msg = serde_json::from_slice(&payload)
        .map_err(|e| internal(format!("反序列化失败（前 120 字节: {}）: {e}", preview(&payload))))?;
    Ok((msg, None))
}

fn preview(bytes: &[u8]) -> String {
    let end = bytes.len().min(120);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// ---------------------------------------------------------------------------
// fd 传递
// ---------------------------------------------------------------------------

/// 发送一个 fd。
///
/// 必须在对应的帧**之后**调用（见模块文档里的时序约束）。
/// fd 的所有权不转移：内核在接收方复制一个新的描述符，发送方原来的 fd 仍然有效。
pub fn send_fd(stream: &UnixStream, fd: RawFd) -> Result<()> {
    send_fd_raw(stream.as_raw_fd(), fd).map_err(|e| internal(format!("发送 fd 失败: {e}")))
}

/// 接收一个 fd。
///
/// 如果对端在这条消息上没有附带 fd，会**阻塞等待**。调用方必须只在确实
/// 期待 fd 的场合（例如刚发过 `TakeTunFd`）调用它。
pub fn recv_fd(stream: &UnixStream) -> Result<RawFd> {
    recv_fd_raw(stream.as_raw_fd()).map_err(|e| internal(format!("接收 fd 失败: {e}")))
}

/// 原始版本：直接对一个 socket fd 操作，不要求 `UnixStream` 包装。
pub fn send_fd_raw(socket: RawFd, fd: RawFd) -> std::io::Result<()> {
    use std::io::IoSlice;
    if fd < 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "fd 为负"));
    }

    let byte = [0u8];
    // 只发送、不接收，所以用 IoSlice（只读 iovec）语义上更准确。
    let mut iov = [IoSlice::new(&byte)];
    const SPACE: usize = 16;
    let mut cmsg_buf = [0u8; SPACE];

    // SAFETY: 手工构造 cmsghdr/msghdr；缓冲区大小与 CMSG_SPACE 一致。
    unsafe {
        let msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: iov.as_mut_ptr() as *mut libc::iovec,
            msg_iovlen: iov.len() as _,
            msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
            msg_controllen: libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _,
            msg_flags: 0,
        };

        let cmsg = libc::CMSG_FIRSTHDR(&msg as *const libc::msghdr);
        if cmsg.is_null() {
            return Err(std::io::Error::other("构造 SCM_RIGHTS 控制消息失败"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(cmsg),
            std::mem::size_of::<RawFd>(),
        );

        let n = libc::sendmsg(socket, &msg as *const libc::msghdr, 0);
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 原始版本：直接对一个 socket fd 操作。
pub fn recv_fd_raw(socket: RawFd) -> std::io::Result<RawFd> {
    use std::io::IoSliceMut;

    let mut byte = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut byte)];
    const SPACE: usize = 64;
    let mut cmsg_buf = [0u8; SPACE];

    // SAFETY: 手工构造 msghdr 并解析控制消息。
    unsafe {
        let mut msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: iov.as_mut_ptr() as *mut libc::iovec,
            msg_iovlen: iov.len() as _,
            msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
            msg_controllen: SPACE as _,
            msg_flags: 0,
        };

        let n = libc::recvmsg(socket, &mut msg as *mut libc::msghdr, 0);
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "对端已关闭连接"));
        }
        // 控制消息被截断意味着 fd 没拿到 —— 必须显式报错，
        // 否则会变成「静默地拿不到 fd」这种极难排查的问题。
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "控制消息被截断，fd 传递失败",
            ));
        }

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
                    // macOS 没有 MSG_CMSG_CLOEXEC，必须手工设置，
                    // 否则 fd 会泄漏给之后 exec 出去的子进程。
                    set_cloexec(fd);
                    return Ok(fd);
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg as *const libc::msghdr, cmsg);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "对端没有在这条消息上附带 fd",
        ))
    }
}

pub fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl 只操作标志位。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

// ---------------------------------------------------------------------------
// 对端身份
// ---------------------------------------------------------------------------

/// `getpeereid`：拿到连接对端的 uid/gid。
///
/// 访问控制的第一层（第二层是代码签名校验）。注意它**不返回 pid**。
pub fn peer_credentials(socket: RawFd) -> std::io::Result<(libc::uid_t, libc::gid_t)> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: getpeereid 写入两个有效指针。
    let rc = unsafe { libc::getpeereid(socket, &mut uid, &mut gid) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((uid, gid))
}

/// 取对端 pid。**仅用于日志**，不要拿它做授权判断（存在 pid 复用导致的 TOCTOU）。
pub fn peer_pid(socket: RawFd) -> Option<libc::pid_t> {
    let mut pid: libc::pid_t = -1;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERPID 到有效缓冲区。
    let rc = unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut libc::pid_t as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 && pid > 0 {
        Some(pid)
    } else {
        None
    }
}

/// 取对端的 `audit_token_t`（8 个 u32，共 32 字节）。
///
/// **这是做授权该用的东西**：内核给连接打上的不可伪造标识，没有 pid 那样的复用问题。
/// 交给 `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` 使用。
pub fn peer_audit_token(socket: RawFd) -> std::io::Result<[u32; 8]> {
    let mut token = [0u32; 8];
    let mut len = std::mem::size_of::<[u32; 8]>() as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERTOKEN 到 32 字节缓冲区。
    let rc = unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(token)
}

/// 以标准 `SOCK_STREAM` 连接到 helper。
///
/// 直接用标准库：`AF_UNIX` 的 `SOCK_STREAM` 正是 `UnixStream` 的默认行为。
pub fn connect(path: &std::path::Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// 监听 helper 的 socket。
pub fn bind(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    std::os::unix::net::UnixListener::bind(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Response, TunFdInfo};

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().expect("UnixStream::pair 应可用")
    }

    /// **把「macOS 不支持 AF_UNIX SEQPACKET」钉死在测试里。**
    ///
    /// 曾经的设计用了 `SOCK_SEQPACKET`（报文边界 + fd 一定同帧到达，非常诱人），
    /// 但在 macOS 上直接 `EPROTONOSUPPORT`。谁要是又想改回去，
    /// 这个测试会立刻拦住他。
    #[test]
    fn seqpacket_is_not_supported_on_macos() {
        const SOCK_SEQPACKET: libc::c_int = 5;
        let mut fds = [0 as RawFd; 2];
        // SAFETY: socketpair 写入两个 fd；失败时不会写入。
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, SOCK_SEQPACKET, 0, fds.as_mut_ptr()) };
        if rc == 0 {
            // 万一将来某个 macOS 版本支持了，也要记得关掉 fd。
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            panic!("这台机器竟然支持 AF_UNIX SOCK_SEQPACKET —— 可以重新评估传输层设计！");
        }
        let err = std::io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EPROTONOSUPPORT),
            "期望 EPROTONOSUPPORT，实际 {err}"
        );
    }

    #[test]
    fn json_message_roundtrips() {
        let (a, b) = pair();
        send(&a, &Request::Status).unwrap();
        let (back, fd): (Request, Option<RawFd>) = recv(&b).unwrap();
        assert_eq!(back, Request::Status);
        assert!(fd.is_none());
    }

    #[test]
    fn framing_survives_back_to_back_messages() {
        // 流式 socket 上连发两条，必须靠长度前缀正确切分。
        let (a, b) = pair();
        send(&a, &Response::Ok { message: Some("one".into()) }).unwrap();
        send(&a, &Response::Ok { message: Some("two".into()) }).unwrap();
        let (first, _): (Response, _) = recv(&b).unwrap();
        let (second, _): (Response, _) = recv(&b).unwrap();
        assert_eq!(first, Response::Ok { message: Some("one".into()) });
        assert_eq!(second, Response::Ok { message: Some("two".into()) });
    }

    #[test]
    fn fd_follows_its_frame_in_order() {
        let (a, b) = pair();
        let dir = std::env::temp_dir().join(format!("xt-transport-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, b"payload").unwrap();
        let file = std::fs::File::open(&path).unwrap();

        let info = TunFdInfo {
            session_id: "s".into(),
            interface: "utun4".into(),
            mtu: 1500,
            header_len: 4,
        };

        // 协议规定的时序：帧先，fd 后。
        send(&a, &Response::TunFd(info.clone())).unwrap();
        send_fd(&a, file.as_raw_fd()).unwrap();
        // 再发一条普通消息，验证它不会被 fd 污染。
        send(&a, &Response::Ok { message: None }).unwrap();

        let (msg, _): (Response, _) = recv(&b).unwrap();
        assert_eq!(msg, Response::TunFd(info));
        let fd = recv_fd(&b).unwrap();

        let (next, _): (Response, _) = recv(&b).unwrap();
        assert_eq!(next, Response::Ok { message: None });

        let mut buf = [0u8; 16];
        // SAFETY: fd 是刚收到的有效描述符。
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert!(n > 0);
        assert_eq!(&buf[..n as usize], b"payload");

        // SAFETY: 关闭测试资源。
        unsafe { libc::close(fd) };
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_message_is_rejected_before_sending() {
        let (a, _b) = pair();
        let huge = Response::Ok { message: Some("x".repeat(MAX_MESSAGE)) };
        let err = send(&a, &huge).unwrap_err();
        assert!(err.to_string().contains("过大"), "应拒绝超大消息: {err}");
    }

    #[test]
    fn garbage_payload_reports_a_useful_error() {
        let (a, b) = pair();
        // 手工写一个「长度合法但内容非法」的帧。
        {
            let mut sock = &a;
            let payload = b"not json at all";
            sock.write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
            sock.write_all(payload).unwrap();
            sock.flush().unwrap();
        }
        let err = recv::<Request>(&b).unwrap_err();
        assert!(err.to_string().contains("反序列化失败"), "{err}");
        assert!(err.to_string().contains("not json"), "错误里应包含原文片段");
    }

    #[test]
    fn disconnected_peer_is_reported() {
        let (a, b) = pair();
        drop(a);
        let err = recv::<Request>(&b).unwrap_err();
        assert!(err.to_string().contains("断开") || err.to_string().contains("失败"), "{err}");
    }

    #[test]
    fn absurd_frame_length_is_rejected() {
        let (a, b) = pair();
        {
            let mut sock = &a;
            sock.write_all(&(u32::MAX).to_be_bytes()).unwrap();
            sock.flush().unwrap();
        }
        let err = recv::<Request>(&b).unwrap_err();
        assert!(err.to_string().contains("过大"), "{err}");
    }

    #[test]
    fn peer_credentials_match_our_own() {
        let (a, _b) = pair();
        let (uid, _gid) = peer_credentials(a.as_raw_fd()).unwrap();
        // SAFETY: geteuid 无副作用。
        assert_eq!(uid, unsafe { libc::geteuid() });
    }

    #[test]
    fn peer_pid_is_available_on_socketpair() {
        let (a, _b) = pair();
        // socketpair 没有真正的「对端进程」，这里只要求不 panic。
        let _ = peer_pid(a.as_raw_fd());
    }

    #[test]
    fn listener_and_connect_round_trip() {
        let dir = std::env::temp_dir().join(format!("xt-listen-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("s.sock");
        let _ = std::fs::remove_file(&sock);

        let listener = bind(&sock).unwrap();
        let client = connect(&sock).unwrap();
        let (server, _) = listener.accept().unwrap();

        send(&client, &Request::Status).unwrap();
        let (req, _): (Request, _) = recv(&server).unwrap();
        assert_eq!(req, Request::Status);

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
