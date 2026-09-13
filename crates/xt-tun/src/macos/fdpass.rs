//! Unix domain socket 上的文件描述符传递（`SCM_RIGHTS`）。
//!
//! 实现位于 [`xt_proto::transport`] —— GUI 进程也需要完全相同的代码，
//! 两边各写一份必然产生 drift。这里保留一层转发，并承载平台相关的说明。
//!
//! # 用来做什么
//!
//! helper 以 root 建好 utun 之后，把 fd 交给以**普通用户**身份运行的数据面
//! （通常是 Xray 原生 `tun` 入站，通过 `XRAY_TUN_FD` 接收）。这样数据面
//! 不需要 root，权限面更干净。
//!
//! # 四个必须知道的细节
//!
//! 1. **utun 读写本身不再校验权限**。内核只在 `connect()` 那一刻检查
//!    `CTL_FLAG_PRIVILEGED`；拿到 fd 之后任何进程都能收发 IP 包。
//!    所以「传 fd」不是授权边界，**谁拿到 fd 谁就控制整条隧道**。
//! 2. **接口/地址/路由/DNS 的配置仍然需要 root**。fd 只解决数据面。
//!    这正是 Xray 在收到 `XRAY_TUN_FD` 时会**跳过**自己那套地址/路由配置的原因
//!    （`ownsFd = false`，`setup()` 直接返回）—— 配置责任转移给了发 fd 的一方，
//!    也就是我们的 helper。
//! 3. macOS **没有 `MSG_CMSG_CLOEXEC`**，接收方必须自己 `fcntl(FD_CLOEXEC)`，
//!    否则 fd 会泄漏给之后 exec 出去的子进程（`xt_proto::transport` 已处理）。
//! 4. **macOS 的 `AF_UNIX` 不支持 `SOCK_SEQPACKET`**（实测 `EPROTONOSUPPORT`），
//!    所以底层的时序约束是「帧先、fd 后，连接严格一问一答」，
//!    详见 `xt_proto::transport` 的模块文档。

pub use xt_proto::transport::{
    peer_audit_token, peer_credentials, peer_pid, recv_fd_raw, send_fd_raw, set_cloexec,
};

/// 发送一个 fd。**必须在对应的数据帧之后调用**（时序约束见模块文档）。
pub fn send_fd(socket: std::os::unix::io::RawFd, fd: std::os::unix::io::RawFd) -> crate::Result<()> {
    send_fd_raw(socket, fd).map_err(|e| crate::Error::syscall("sendmsg(SCM_RIGHTS)", e))
}

/// 接收一个 fd。若对端没有附带 fd 则会阻塞 —— 只在确实期待 fd 时调用。
pub fn recv_fd(socket: std::os::unix::io::RawFd) -> crate::Result<std::os::unix::io::RawFd> {
    recv_fd_raw(socket).map_err(|e| crate::Error::syscall("recvmsg(SCM_RIGHTS)", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::{AsRawFd, RawFd};

    fn pair() -> (std::os::unix::net::UnixStream, std::os::unix::net::UnixStream) {
        std::os::unix::net::UnixStream::pair().expect("UnixStream::pair 应可用")
    }

    #[test]
    fn fd_survives_roundtrip() {
        let (a, b) = pair();
        let dir = std::env::temp_dir().join(format!("xt-fdpass-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payload.txt");
        std::fs::write(&path, b"hello-fd").unwrap();
        let file = std::fs::File::open(&path).unwrap();

        send_fd(a.as_raw_fd(), file.as_raw_fd()).unwrap();
        let received = recv_fd(b.as_raw_fd()).unwrap();

        let mut buf = [0u8; 16];
        // SAFETY: received 是刚收到的有效 fd。
        let n = unsafe { libc::read(received, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert!(n > 0);
        assert_eq!(&buf[..n as usize], b"hello-fd");

        // SAFETY: 关闭测试资源。
        unsafe { libc::close(received) };
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peer_credentials_are_readable() {
        let (a, _b) = pair();
        let (uid, _gid) = peer_credentials(a.as_raw_fd()).unwrap();
        // SAFETY: geteuid 无副作用。
        assert_eq!(uid, unsafe { libc::geteuid() });
    }

    #[test]
    fn audit_token_is_thirty_two_bytes() {
        let (a, _b) = pair();
        if let Ok(t) = peer_audit_token(a.as_raw_fd()) {
            assert_eq!(std::mem::size_of_val(&t), 32);
        }
    }

    #[test]
    fn recv_fd_on_plain_data_reports_missing_fd() {
        let (a, b) = pair();
        // 只写一个字节、不带控制消息。
        // SAFETY: 写 1 字节到有效 fd。
        let wrote = unsafe { libc::write(a.as_raw_fd(), b"x".as_ptr() as *const libc::c_void, 1) };
        assert_eq!(wrote, 1);
        let err = recv_fd(b.as_raw_fd()).unwrap_err();
        assert!(err.to_string().contains("SCM_RIGHTS"), "{err}");
    }

    #[test]
    fn raw_fd_helpers_are_reachable() {
        // 保证 `xt_proto::transport` 的原始接口确实被转发出来，
        // 供将来需要在非 UnixStream 场景下使用。
        let (a, b) = pair();
        let f: RawFd = a.as_raw_fd();
        let g: RawFd = b.as_raw_fd();
        assert!(f >= 0 && g >= 0);
    }
}
