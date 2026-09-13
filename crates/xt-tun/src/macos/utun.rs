//! `utun` 设备创建。
//!
//! macOS 没有 Linux 那样的 `/dev/net/tun`。要拿到一个 utun，必须走
//! **内核控制（kernel control）** 这条路：
//!
//! ```text
//! fd = socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL)
//! ioctl(fd, CTLIOCGINFO, &ctl_info{ "com.apple.net.utun_control" })   // 换到控制 id
//! connect(fd, &sockaddr_ctl{ sc_id, sc_unit })                        // 绑定到某个 utunN
//! getsockopt(fd, SYSPROTO_CONTROL, UTUN_OPT_IFNAME, &buf)             // 问内核要接口名
//! ```
//!
//! 三个必须知道的坑：
//!
//! 1. **需要 root**。utun 控制是 `CTL_FLAG_PRIVILEGED`，非 root 的 `connect()`
//!    直接 `EPERM`。这正是必须有特权 helper 的原因。
//! 2. **读写都带 4 字节地址族头**。每次 `read()` 拿到的前 4 字节是大端 `u32`
//!    的 `AF_INET`(2) / `AF_INET6`(30)，真正的 IP 包从第 5 字节开始；`write()`
//!    时也必须自己补上这 4 字节。忘了这件事的典型症状是「隧道起来了但完全不通」。
//! 3. **`sc_unit = 0` 表示让内核挑**，`sc_unit = N+1` 表示要 `utunN`。

use std::ffi::CStr;
use std::io;
use std::os::unix::io::RawFd;

use crate::error::{Error, Result};

/// `PF_SYSTEM` / `AF_SYSTEM`。Darwin 上两者都是 32。
/// 这里自己定义而不依赖 `libc` 是否导出，避免跨 libc 版本编译不过。
const PF_SYSTEM: libc::c_int = 32;
const AF_SYSTEM: u8 = 32;
const SYSPROTO_CONTROL: libc::c_int = 2;
/// `struct sockaddr_ctl.ss_sysaddr` 的取值。
const AF_SYS_CONTROL: u16 = 2;

#[allow(dead_code)]
const UTUN_OPT_IFNAME: libc::c_int = 2;

const UTUN_CONTROL_NAME: &[u8] = b"com.apple.net.utun_control\0";

/// 每个 IP 包头部的地址族长度。
pub const UTUN_HEADER_LEN: usize = 4;

pub const AF_INET_U32: u32 = 2;
pub const AF_INET6_U32: u32 = 30;

#[repr(C)]
#[derive(Clone, Copy)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [libc::c_char; 96],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

/// `_IOW('N', 3, struct ctl_info)` —— 从 `com.apple.net.utun_control` 换到控制 id。
///
/// 这个值**用系统头文件核实过**，不要凭 `_IOW` 的直觉去推：
///
/// ```text
/// $ cc -E - <<< '#include <sys/kern_control.h>' ...   →  CTLIOCGINFO = 0xc0644e03
///   sizeof(struct ctl_info) = 100, MAX_KCTL_NAME = 96
/// ```
///
/// 按 BSD 惯例 `_IOW` 应该是 `IOC_IN (0x80000000)`，推出来是 `0x80644e03`；
/// **但 Darwin 实际用的是 `IOC_INOUT (0xc0000000)`**。用错的表现是
/// `ioctl` 返回 `ENOTSUP (45)` 而不是预期的 `EPERM` —— 一个完全不指向
/// 真正原因的错误码，非常容易把人带偏。
///
/// 下面的 `tests::ctliocginfo_matches_system_header` 把这个值钉住。
const CTLIOCGINFO: libc::c_ulong = 0xc064_4e03;

/// 一个打开着的 utun 设备。
pub struct UtunDevice {
    fd: RawFd,
    name: String,
}

impl UtunDevice {
    /// 创建 utun。`unit` 为 `None` 时由内核挑选编号。
    ///
    /// **需要 root。**
    pub fn create(unit: Option<u32>) -> Result<Self> {
        // SAFETY: 全部是裸系统调用，参数都已按 Darwin 的 ABI 布局构造。
        unsafe {
            let fd = libc::socket(PF_SYSTEM, libc::SOCK_DGRAM, SYSPROTO_CONTROL);
            if fd < 0 {
                return Err(Error::syscall("socket(PF_SYSTEM)", io::Error::last_os_error()));
            }
            // 失败路径统一走这个闭包，避免漏 close 导致 fd 泄漏。
            let cleanup = |fd: RawFd| {
                libc::close(fd);
            };

            if let Err(e) = set_cloexec(fd) {
                cleanup(fd);
                return Err(e);
            }

            let mut info = CtlInfo { ctl_id: 0, ctl_name: [0; 96] };
            for (i, b) in UTUN_CONTROL_NAME.iter().enumerate() {
                info.ctl_name[i] = *b as libc::c_char;
            }

            if libc::ioctl(fd, CTLIOCGINFO, &mut info as *mut CtlInfo as *mut libc::c_void) < 0 {
                let e = io::Error::last_os_error();
                cleanup(fd);
                return Err(Error::syscall("ioctl(CTLIOCGINFO)", e));
            }

            let addr = SockaddrCtl {
                sc_len: std::mem::size_of::<SockaddrCtl>() as u8,
                sc_family: AF_SYSTEM,
                ss_sysaddr: AF_SYS_CONTROL,
                sc_id: info.ctl_id,
                // 0 = 内核分配；N+1 = 指定 utunN。
                sc_unit: unit.map(|u| u + 1).unwrap_or(0),
                sc_reserved: [0; 5],
            };

            if libc::connect(
                fd,
                &addr as *const SockaddrCtl as *const libc::sockaddr,
                std::mem::size_of::<SockaddrCtl>() as libc::socklen_t,
            ) < 0
            {
                let e = io::Error::last_os_error();
                cleanup(fd);
                // EPERM 是最常见的失败原因，给出可行动的提示。
                return Err(Error::syscall(
                    "connect(utun)",
                    io::Error::new(
                        e.kind(),
                        format!("{e}（创建 utun 需要 root 权限；若以 root 运行仍失败，可能是接口名已被占用）"),
                    ),
                ));
            }

            // 问内核要真实的接口名（形如 utun4）。
            let mut name_buf = [0 as libc::c_char; 64];
            let mut len = name_buf.len() as libc::socklen_t;
            if libc::getsockopt(
                fd,
                SYSPROTO_CONTROL,
                UTUN_OPT_IFNAME,
                name_buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
            ) < 0
            {
                let e = io::Error::last_os_error();
                cleanup(fd);
                return Err(Error::syscall("getsockopt(UTUN_OPT_IFNAME)", e));
            }

            let name = CStr::from_ptr(name_buf.as_ptr()).to_string_lossy().into_owned();
            Ok(Self { fd, name })
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }

    /// 交出 fd 的所有权（Drop 不再关闭）。用于 `SCM_RIGHTS` 传递。
    pub fn into_raw_fd(self) -> RawFd {
        let fd = self.fd;
        std::mem::forget(self);
        fd
    }

    /// 从已经持有的 fd 重建对象（fd 传递的接收侧用）。
    ///
    /// # Safety
    /// `fd` 必须是有效的 utun fd，且此后由本对象独占。
    pub unsafe fn from_raw_fd(fd: RawFd, name: String) -> Self {
        Self { fd, name }
    }

    /// 读一个 IP 包，自动剥掉 4 字节地址族头。
    ///
    /// 返回 `(地址族, 包长度)`，地址族是 `AF_INET_U32` / `AF_INET6_U32`。
    pub fn read_packet(&self, buf: &mut [u8]) -> io::Result<(u32, usize)> {
        let mut raw = vec![0u8; buf.len() + UTUN_HEADER_LEN];
        let n = read_fd(self.fd, &mut raw)?;
        if n < UTUN_HEADER_LEN {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "utun 返回的包短于地址族头"));
        }
        let family = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let payload = n - UTUN_HEADER_LEN;
        buf[..payload].copy_from_slice(&raw[UTUN_HEADER_LEN..n]);
        Ok((family, payload))
    }

    /// 写一个 IP 包，自动补上 4 字节地址族头。
    pub fn write_packet(&self, family: u32, packet: &[u8]) -> io::Result<usize> {
        let mut raw = Vec::with_capacity(packet.len() + UTUN_HEADER_LEN);
        raw.extend_from_slice(&family.to_be_bytes());
        raw.extend_from_slice(packet);
        write_fd(self.fd, &raw)
    }
}

impl Drop for UtunDevice {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY: fd 由本对象独占，close 一次即可。
            unsafe {
                libc::close(self.fd);
            }
            self.fd = -1;
        }
    }
}

fn set_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: fcntl 只操作 fd 标志位。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(Error::syscall("fcntl(F_GETFD)", io::Error::last_os_error()));
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(Error::syscall("fcntl(F_SETFD)", io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: buf 是有效可写内存，长度由切片保证。
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: buf 是有效只读内存，长度由切片保证。
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// 内核是否允许非 root 创建 utun。用于给 UI 一个明确的「为什么需要 helper」提示。
pub fn probe_root_requirement() -> bool {
    match UtunDevice::create(None) {
        Ok(dev) => {
            tracing::warn!(interface = dev.name(), "非特权进程竟然成功创建了 utun —— 请检查沙箱/权限配置");
            false
        }
        Err(Error::Syscall { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied => true,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctliocginfo_matches_system_header() {
        // 与 `cc -E` 读出的 `CTLIOCGINFO` 一致（0xc0644e03）。
        // 结构体布局是推导这个值的前提，所以一并断言。
        assert_eq!(std::mem::size_of::<CtlInfo>(), 100, "struct ctl_info 必须是 4+96 字节");
        assert_eq!(CTLIOCGINFO, 0xc064_4e03u64 as libc::c_ulong);
        // 高两位是 IOC_INOUT，不是直觉上的 IOC_IN。
        assert_eq!((CTLIOCGINFO >> 30) as u32, 0b11);
    }

    #[test]
    fn sockaddr_ctl_layout_matches_darwin() {
        // u_char + u_char + u_int16_t + u_int32_t + u_int32_t + u_int32_t[5]
        assert_eq!(std::mem::size_of::<SockaddrCtl>(), 32);
    }

    #[test]
    fn header_constants_are_correct() {
        assert_eq!(AF_INET_U32, 2);
        assert_eq!(AF_INET6_U32, 30);
        assert_eq!(UTUN_HEADER_LEN, 4);
    }

    /// 非 root 环境下创建必然失败，且错误信息应该可读。
    /// 在 CI 里以 root 跑时这个测试会自动跳过（因为会成功）。
    #[test]
    fn create_without_root_fails_with_permission_error() {
        match UtunDevice::create(None) {
            Ok(dev) => {
                eprintln!("以 root 运行，跳过权限断言（已创建 {}）", dev.name());
            }
            Err(e) => {
                // 非 root 时应当在 connect() 处失败（ioctl 已经能成功拿到 ctl_id）。
                // 如果失败在 ioctl，说明 CTLIOCGINFO 又错了 —— 这个断言就是在防它。
                let msg = e.to_string();
                assert!(
                    msg.contains("connect(utun)"),
                    "非 root 时应在 connect(utun) 处失败，实际错误: {msg}"
                );
            }
        }
    }
}
