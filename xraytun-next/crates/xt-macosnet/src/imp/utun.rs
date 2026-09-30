//! `utun` 设备创建（Darwin 内核控制路径）。
//!
//! macOS 没有 Linux 的 `/dev/net/tun`。要拿到一张 utun，只能走内核控制
//! （kernel control）这条路：
//!
//! ```text
//! fd = socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL)
//! fcntl(fd, F_SETFD, FD_CLOEXEC)                                  // 别泄漏给 exec 的子进程
//! ioctl(fd, CTLIOCGINFO, &ctl_info{ "com.apple.net.utun_control" })  // 名字 → 控制 id
//! connect(fd, &sockaddr_ctl{ sc_id, sc_unit })                    // 绑定到某个 utunN
//! getsockopt(fd, SYSPROTO_CONTROL, UTUN_OPT_IFNAME, &buf)         // 问内核要真实接口名
//! ```
//!
//! 三个必须知道的坑：
//!
//! 1. **需要 root**。utun 控制带 `CTL_FLAG_PRIVILEGED`，非 root 的 `connect()`
//!    直接 `EPERM`。这正是本项目必须有特权 helper 的原因，也是本模块把
//!    `EPERM` 单独映射成 [`ErrorCode::PermissionDenied`] 而不是笼统 `Io` 的原因 ——
//!    UI 要靠它区分「你没权限」和「系统出错了」。
//! 2. **读写都带 4 字节地址族头**。每次 `read()` 拿到的前 4 字节是大端 `u32` 的
//!    `AF_INET`(2) / `AF_INET6`(30)，真正的 IP 包从第 5 字节开始；`write()` 时
//!    也必须自己补上这 4 字节。本模块只负责建卡并把 fd 交出去，但调用方忘了这
//!    件事的典型症状是「隧道建起来了却完全不通」，所以在这里写明。
//! 3. **`sc_unit` 的语义是「编号 + 1」**。`sc_unit = 0` 表示让内核挑一个空闲编号，
//!    `sc_unit = N + 1` 才表示要 `utunN`。本模块的 [`create`] 固定用 `0`（由内核
//!    挑号，避免和目标机既有接口撞名），因此调用方拿到的名字必须来自
//!    `UTUN_OPT_IFNAME`，**不能**自己拼 `utun0`。
//!
//! 另外两个容易把人带偏的细节，代码里都钉死了：
//!
//! * `CTLIOCGINFO` 是 `0xc0644e03`（`IOC_INOUT`，不是 BSD 惯例推出来的 `IOC_IN`）。
//!   用错的表现是 `ioctl` 返回 `ENOTSUP(45)` 而不是预期的 `EPERM`。
//! * `struct sockaddr_ctl` 是 32 字节、`struct ctl_info` 是 100 字节。布局错了
//!   `ioctl`/`connect` 会以看似无关的 errno 失败，所以下面的 `tests` 把它们断言住。

use std::io;
use std::os::fd::RawFd;

use xt_contract::error::{ErrorBody, ErrorCode};

/// `PF_SYSTEM` / `AF_SYSTEM`：Darwin 上两者都是 32。
///
/// 这里自己定义而不依赖 `libc` 是否导出，避免换 libc 版本就编译不过。
const PF_SYSTEM: libc::c_int = 32;
const AF_SYSTEM: u8 = 32;

/// `SYSPROTO_CONTROL`：`socket()` 的第三个参数，也是 `getsockopt` 的 level。
const SYSPROTO_CONTROL: libc::c_int = 2;

/// `struct sockaddr_ctl.ss_sysaddr` 的取值。
const AF_SYS_CONTROL: u16 = 2;

/// `getsockopt` 的 option：取内核分配的真实接口名。
const UTUN_OPT_IFNAME: libc::c_int = 2;

/// utun 控制的名字，必须以 `\0` 结尾（`ctl_name` 是 C 字符串，不是长度前缀）。
const UTUN_CONTROL_NAME: &[u8] = b"com.apple.net.utun_control\0";

/// `_IOW('N', 3, struct ctl_info)` —— 由控制名换到控制 id。
///
/// 这个值**用系统头文件核实过**，不要凭 `_IOW` 的直觉去推：
///
/// ```text
/// $ cc -E - <<< '#include <sys/kern_control.h>'   →  CTLIOCGINFO = 0xc0644e03
///   sizeof(struct ctl_info) = 100, MAX_KCTL_NAME = 96
/// ```
///
/// 按 BSD 惯例 `_IOW` 应该是 `IOC_IN (0x80000000)`，推出来是 `0x80644e03`；
/// **但 Darwin 实际用的是 `IOC_INOUT (0xc0000000)`**。用错时 `ioctl` 返回
/// `ENOTSUP(45)`，一个完全不指向真正原因的错误码。
const CTLIOCGINFO: libc::c_ulong = 0xc064_4e03;

/// `struct ctl_info`：`{ u_int32_t ctl_id; char ctl_name[MAX_KCTL_NAME]; }`，
/// `MAX_KCTL_NAME = 96`，所以 sizeof = 100。
#[repr(C)]
#[derive(Clone, Copy)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [libc::c_char; 96],
}

/// `struct sockaddr_ctl`：u8 + u8 + u16 + u32 + u32 + u32[5]，sizeof = 32。
#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    /// `0` = 内核挑号；`N + 1` = 指定 `utunN`。
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

/// 创建一张 utun，由内核挑选编号（`sc_unit = 0`）。
///
/// 返回 `(fd, 接口名)`。**fd 的所有权交给调用方**：成功时调用方负责 `close`
/// （关掉 fd 即销毁接口），失败路径本函数自己 `close`，不会泄漏。
///
/// 接口名形如 `utun4`，**必须**用内核返回的这个值去配地址/装路由，
/// 不要自己拼名字。
///
/// # 错误
///
/// * `EPERM` 来自 `connect()` 时 → [`ErrorCode::PermissionDenied`]（非 root 的常见结果）；
/// * 其余 syscall 失败 → [`ErrorCode::Io`]，`detail` 里带 `errno` 与出错的操作名。
pub(crate) fn create() -> Result<(std::os::fd::RawFd, String), ErrorBody> {
    // SAFETY: 下面全部是裸系统调用，参数都按 Darwin 的 ABI 布局构造，缓冲区都由
    // 本函数拥有且在调用期间保持存活；失败一律在 `FdGuard` 的 Drop 里 close。
    unsafe {
        let fd = libc::socket(PF_SYSTEM, libc::SOCK_DGRAM, SYSPROTO_CONTROL);
        if fd < 0 {
            return Err(syscall_err("socket(PF_SYSTEM)", io::Error::last_os_error()));
        }
        // 从这里开始 fd 有主：任何提前 return 都会 close，不必在每条失败路径上手写。
        let guard = FdGuard(fd);

        set_cloexec(guard.0)?;

        // 控制名填进定长数组；`ctl_name` 整体零初始化，末尾的 NUL 由常量自带。
        let mut info = CtlInfo { ctl_id: 0, ctl_name: [0; 96] };
        for (slot, byte) in info.ctl_name.iter_mut().zip(UTUN_CONTROL_NAME) {
            *slot = *byte as libc::c_char;
        }

        // SAFETY: info 是有效的 CtlInfo，指针与 CTLIOCGINFO 声明的类型匹配。
        if libc::ioctl(guard.0, CTLIOCGINFO, &mut info as *mut CtlInfo as *mut libc::c_void) < 0 {
            return Err(syscall_err("ioctl(CTLIOCGINFO)", io::Error::last_os_error()));
        }

        let addr = SockaddrCtl {
            sc_len: std::mem::size_of::<SockaddrCtl>() as u8,
            sc_family: AF_SYSTEM,
            ss_sysaddr: AF_SYS_CONTROL,
            sc_id: info.ctl_id,
            // 0 = 内核分配编号（见模块文档「坑 3」）。
            sc_unit: 0,
            sc_reserved: [0; 5],
        };

        // SAFETY: addr 布局与 Darwin 的 struct sockaddr_ctl 一致，长度用 sizeof 传入。
        if libc::connect(
            guard.0,
            &addr as *const SockaddrCtl as *const libc::sockaddr,
            std::mem::size_of::<SockaddrCtl>() as libc::socklen_t,
        ) < 0
        {
            let e = io::Error::last_os_error();
            // EPERM 是最常见的失败：给一条能行动的提示，而不是裸 errno。
            return Err(if e.raw_os_error() == Some(libc::EPERM) {
                ErrorBody::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "connect(utun) 失败：创建 utun 需要 root 权限（EPERM）；\
                         若已以 root 运行仍失败，请检查 helper 的沙箱/签名配置（{e}）"
                    ),
                )
                .with_detail(serde_json::json!({
                    "syscall": "connect(utun)",
                    "errno": e.raw_os_error(),
                }))
            } else {
                syscall_err("connect(utun)", e)
            });
        }

        // 问内核要真实的接口名（形如 utun4）。
        let mut name_buf = [0 as libc::c_char; 64];
        let mut len = name_buf.len() as libc::socklen_t;
        // SAFETY: name_buf 是可写缓冲区，len 正确描述其容量；内核最多写入 len 字节。
        if libc::getsockopt(
            guard.0,
            SYSPROTO_CONTROL,
            UTUN_OPT_IFNAME,
            name_buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        ) < 0
        {
            return Err(syscall_err("getsockopt(UTUN_OPT_IFNAME)", io::Error::last_os_error()));
        }

        let name = interface_name(&name_buf, len)?;
        Ok((guard.into_raw(), name))
    }
}

/// 从 `UTUN_OPT_IFNAME` 的缓冲区里取出接口名。
///
/// 不直接用 `CStr::from_ptr`：内核理论上会写 NUL 结尾的字符串，但这里的长度来自
/// 内核，越界或缺失 NUL 都会变成未定义行为/panic。用 `len` 限界、自己找 NUL，
/// 是零成本的防御。
fn interface_name(buf: &[libc::c_char], len: libc::socklen_t) -> Result<String, ErrorBody> {
    // SAFETY: c_char 在 Darwin 上是 i8，转 u8 只是重新解释位模式，无对齐要求。
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
    let n = (len as usize).min(bytes.len());
    let end = bytes[..n].iter().position(|&b| b == 0).unwrap_or(n);
    let name = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if name.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::Io,
            "getsockopt(UTUN_OPT_IFNAME) 成功但返回了空接口名".to_string(),
        ));
    }
    Ok(name)
}

/// 给 fd 打上 `FD_CLOEXEC`。
///
/// utun fd 之后会通过 `SCM_RIGHTS` 交给 daemon，但绝不能顺着 `exec` 泄漏给
/// `route` / `ifconfig` / xray 这些子进程 —— 那等于把一张 TUN 白送给不受控的进程。
fn set_cloexec(fd: RawFd) -> Result<(), ErrorBody> {
    // SAFETY: fcntl 只读写该 fd 的标志位，不涉及内存。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(syscall_err("fcntl(F_GETFD)", io::Error::last_os_error()));
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(syscall_err("fcntl(F_SETFD)", io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// syscall 失败 → [`ErrorBody`]：`EPERM` 单独归类，其余算 `Io`。
///
/// 保留 `errno` 与出错的操作名放进 `detail`，因为向用户展示的中文 message 不该
/// 是排查的唯一线索。
fn syscall_err(op: &str, e: io::Error) -> ErrorBody {
    let code = if e.raw_os_error() == Some(libc::EPERM) {
        ErrorCode::PermissionDenied
    } else {
        ErrorCode::Io
    };
    ErrorBody::new(code, format!("{op} 失败：{e}")).with_detail(serde_json::json!({
        "syscall": op,
        "errno": e.raw_os_error(),
        "kind": format!("{:?}", e.kind()),
    }))
}

/// 持有 fd 所有权的小守卫：中途 return 自动 `close`，成功时用 `into_raw` 解除。
struct FdGuard(RawFd);

impl FdGuard {
    /// 交出所有权，Drop 不再 close。
    fn into_raw(mut self) -> RawFd {
        let fd = self.0;
        self.0 = -1;
        fd
    }
}

impl Drop for FdGuard {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: fd 由本守卫独占，且只在 Drop 里 close 一次。
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctl_info_layout_matches_darwin() {
        // 4 + MAX_KCTL_NAME(96) = 100 —— CTLIOCGINFO 的编码前提。
        assert_eq!(std::mem::size_of::<CtlInfo>(), 100, "struct ctl_info 必须是 4+96 字节");
    }

    #[test]
    fn sockaddr_ctl_layout_matches_darwin() {
        // u_char + u_char + u_int16_t + u_int32_t + u_int32_t + u_int32_t[5]
        assert_eq!(std::mem::size_of::<SockaddrCtl>(), 32);
    }

    #[test]
    fn ctliocginfo_matches_system_header() {
        assert_eq!(CTLIOCGINFO, 0xc064_4e03u64 as libc::c_ulong);
        // 高两位是 IOC_INOUT，不是直觉上的 IOC_IN。
        assert_eq!((CTLIOCGINFO >> 30) as u32, 0b11);
    }

    #[test]
    fn control_name_is_nul_terminated() {
        assert_eq!(UTUN_CONTROL_NAME.last().copied(), Some(0));
        assert!(UTUN_CONTROL_NAME.len() <= 96, "控制名必须放得进 ctl_name[96]");
    }

    /// 非 root 时创建必然失败，且应当失败在 `connect()`（`ioctl` 拿 ctl_id 不需要特权）。
    /// 以 root 跑时这个测试自动退化为「能创建」，不做断言。
    #[test]
    fn create_without_root_is_permission_denied() {
        match create() {
            Ok((fd, name)) => {
                // SAFETY: fd 是本测试刚创建并独占的。
                unsafe { libc::close(fd) };
                eprintln!("以 root 运行，跳过权限断言（已创建 {name}）");
            }
            Err(e) => {
                assert!(
                    e.message.contains("connect(utun)"),
                    "非 root 时应在 connect(utun) 处失败，实际错误: {e}"
                );
                assert_eq!(e.code, ErrorCode::PermissionDenied, "EPERM 必须映射成 PermissionDenied");
            }
        }
    }
}
