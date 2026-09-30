//! `SCM_RIGHTS` 传 fd + 读对端身份（凭据 / audit token）。
//!
//! # 时序约束：帧先、fd 后
//!
//! macOS 的 `AF_UNIX` **不支持 `SOCK_SEQPACKET`**（实测 `socketpair` 返回
//! `EPROTONOSUPPORT`），所以 fd 不能和它所属的消息天然同帧到达，只能作为
//! 「紧跟在响应帧之后的一条独立 1 字节 `SCM_RIGHTS` 消息」发送。正确性依赖
//! 一条明确的协议约束：
//!
//! > **连接严格一问一答，且同一时刻只有一条在途消息。**
//! > 客户端发出 `TakeTunFd` 后必须先读完 `Response::TakeTunFd` 帧，再读 fd。
//!
//! 调用点（[`crate::server`]）先 `write_all` 响应帧并 `flush`，再 `sendmsg`；
//! 两者是同一条 `SOCK_STREAM` 上的顺序发送，所以 fd 不会被别的帧插队。
//!
//! # fd 的**所有权**不转移
//!
//! `sendmsg(SCM_RIGHTS)` 由内核在接收方**复制**一个新的描述符，发送方原来的
//! fd 仍然有效。utun 接口的生命周期绑定在描述符上：TakeTunFd 之前 helper 是
//! 唯一持有者，此时提前关闭接口就消失；TakeTunFd 之后数据面也拿到一份副本，
//! 接口要等所有副本都关闭才销毁。所以 helper 侧这份不是多余的 —— 它是 helper
//! 对接口的引用，持有到 `TunDown` 再关闭。
//!
//! # 为什么用 audit token 而不是 pid 做授权
//!
//! pid 可被回收：在「取 pid」到「用 pid 校验签名」之间，该 pid 可能被分配给
//! 另一个进程（TOCTOU）。`audit_token` 是内核给连接打上的不可伪造标识，
//! 交给 `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` 使用。
//! [`peer_pid`] 只用于日志，**不参与**授权决策。

use std::os::fd::RawFd;

/// `audit_token_t` 是 8 个 `u32`，共 32 字节。
pub const AUDIT_TOKEN_BYTES: usize = 8 * std::mem::size_of::<u32>();

/// 发送一个 fd。**必须在对应的响应帧之后调用**（见模块文档的时序约束）。
///
/// 这是绕过 tokio 的**同步** `sendmsg`：数据体只有 1 字节，正常不会碰到发送缓冲
/// 满（`EAGAIN`）。若真发生，按错误处理并断开连接 —— 宁可让对端看到失败，
/// 也绝不静默地「帧说给了 fd、fd 却没到」。
pub fn send_fd(socket: RawFd, fd: RawFd) -> std::io::Result<()> {
    if socket < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket fd 为负",
        ));
    }
    if fd < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "要发送的 fd 为负",
        ));
    }

    // 只发送、不接收：一个 1 字节的数据体 + 一条 `SCM_RIGHTS` 控制消息。
    let byte = [0_u8];
    let mut iov = [libc::iovec {
        iov_base: byte.as_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    // `CMSG_SPACE(sizeof(int))` 的缓冲。固定 16 字节对单个 fd 绰绰有余。
    const CONTROL_SPACE: usize = 16;
    let mut control = [0_u8; CONTROL_SPACE];

    // SAFETY: 手工构造 `msghdr` / `cmsghdr`。`control` 缓冲长度不小于
    // `CMSG_SPACE(sizeof(RawFd))`；`iov` 指向的 `byte` 在本调用期间存活。
    // 控制消息里的 fd 是调用方提供的有效描述符。
    unsafe {
        let msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: iov.as_mut_ptr(),
            msg_iovlen: iov.len() as _,
            msg_control: control.as_mut_ptr() as *mut libc::c_void,
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

        let written = libc::sendmsg(socket, &msg as *const libc::msghdr, 0);
        if written < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 接收一个 fd。
///
/// **helperd 目前只发不收**：这是给客户端一侧（daemon）准备的同一段实现 ——
/// 两个方向放在一起，省得「一边改了另一边没改」的 drift。保留它也让
/// `SCM_RIGHTS` 的往返能在真机上被单测覆盖。
///
/// 若对端在这条消息上没有附带 fd 会**阻塞等待**，所以调用方必须只在确实
/// 期待 fd 的场合调用（刚发过 `TakeTunFd`）。
#[allow(dead_code)]
pub fn recv_fd(socket: RawFd) -> std::io::Result<RawFd> {
    let mut byte = [0_u8; 1];
    let mut iov = [libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    const CONTROL_SPACE: usize = 64;
    let mut control = [0_u8; CONTROL_SPACE];

    // SAFETY: 手工构造 `msghdr` 并解析控制消息。所有指针都指向本函数栈上
    // 存活到调用结束的缓冲；`recvmsg` 只会在 `msg_controllen` 之内写入。
    unsafe {
        let mut msg = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: iov.as_mut_ptr(),
            msg_iovlen: iov.len() as _,
            msg_control: control.as_mut_ptr() as *mut libc::c_void,
            msg_controllen: CONTROL_SPACE as _,
            msg_flags: 0,
        };

        let read = libc::recvmsg(socket, &mut msg as *mut libc::msghdr, 0);
        if read < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "对端已关闭连接",
            ));
        }
        // 控制消息被截断意味着 fd 没拿到 —— 必须显式报错，不能静默地「没有 fd」。
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
                    // macOS 没有 `MSG_CMSG_CLOEXEC`，必须手工设置，
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

/// 设置 `FD_CLOEXEC`。失败不返回错误：它只是防泄漏的加固，不是功能本身；
/// 调用方也不该因为一个 best-effort 的加固失败而丢掉 fd。
pub fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl 只读/写 fd 的标志位，不涉及指针。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// `getpeereid`：拿到连接对端的 uid/gid。
///
/// 这是访问控制的**第一层**（第二层是 [`crate::trust`] 的代码签名校验）。
/// 注意它不返回 pid。
pub fn peer_credentials(socket: RawFd) -> std::io::Result<(libc::uid_t, libc::gid_t)> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: getpeereid 只会写入这两个由我们提供的有效指针。
    let rc = unsafe { libc::getpeereid(socket, &mut uid, &mut gid) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((uid, gid))
}

/// 取对端 pid。**仅用于日志**，不用于授权（pid 复用 ⇒ TOCTOU）。
///
/// 取不到返回 `None`：这正是「没有这个信息」的语义，调用方照常工作。
pub fn peer_pid(socket: RawFd) -> Option<libc::pid_t> {
    let mut pid: libc::pid_t = -1;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERPID 到我们提供的有效缓冲；len 初值为其大小。
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

/// 取对端的 `audit_token_t`（8 个 `u32`，共 32 字节）。
///
/// **这是做授权该用的东西**：内核给连接打上的不可伪造标识，没有 pid 那样的
/// 复用问题。交给 `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` 使用。
///
/// 长度**必须**是 32 字节，否则拒绝：曾把选项号写成 `LOCAL_PEEREUUID`（`0x005`，
/// 只回 16 字节），结果是「合法对端也一律被拒」——这道门在生产上等于不存在。
/// 所以这里用 libc 的 `LOCAL_PEERTOKEN`（`0x006`）而不是手写字面量，并当场校验长度。
pub fn peer_audit_token(socket: RawFd) -> std::io::Result<[u32; 8]> {
    let mut token = [0_u32; 8];
    let mut len = AUDIT_TOKEN_BYTES as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERTOKEN 到 32 字节缓冲；缓冲正是 token 本身。
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
    if len as usize != AUDIT_TOKEN_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "对端 audit token 只有 {len} 字节（应为 {AUDIT_TOKEN_BYTES}）—— \
                 取错了 socket 选项或对端不支持 LOCAL_PEERTOKEN；拒绝授权"
            ),
        ));
    }
    Ok(token)
}
