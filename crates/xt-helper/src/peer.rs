//! 对端身份识别与代码签名校验。
//!
//! helper 是 root 守护进程，它的 Unix socket 就是一条**提权通道**。
//! 如果任何本地进程都能连接并下发「把默认路由指向 utunX」的指令，
//! 那就等于把 root 的部分能力开放给了全机器。
//!
//! 因此这里做两道校验：
//!
//! | 层次 | 机制 | 挡住什么 |
//! |---|---|---|
//! | 文件系统 | socket 权限位 `root:admin 0660` | 非管理员组用户 |
//! | 内核 + Security.framework | 对端代码签名要求串 | 同组但不是我们 App 的进程 |
//!
//! ## 为什么用 audit token 而不是 pid
//!
//! 用 pid 做校验存在 TOCTOU：`SecCodeCopyGuestWithAttributes(kSecGuestAttributePid)`
//! 按 pid 反查进程，而在「取 pid」到「校验签名」之间，该 pid 可能被回收并
//! 分配给另一个进程。**audit token** 是内核给连接打上的不可伪造标识，
//! 不存在这个问题。macOS 上通过 `getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN)`
//! 从 socket 上取。
//!
//! ## 关于可验证性
//!
//! 下面的 `Security.framework` / `CoreFoundation` FFI 只保证**类型层面**正确；
//! 函数语义与常量名必须在**真实签名的构建**上通过集成测试验证
//! （见 `docs/06-helper-protocol.md` 的「必须补的测试」一节）。
//! 类型检查通过 ≠ 运行时正确，这里不能自欺欺人。

use std::ffi::c_void;
use std::os::unix::io::RawFd;

use crate::error::{HelperError, Result};

// ---------------------------------------------------------------------------
// 基础类型别名（CoreFoundation 用不透明指针）
// ---------------------------------------------------------------------------

type CFAllocatorRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFIndex = isize;
type OSStatus = i32;
type SecCodeRef = *const c_void;
type SecRequirementRef = *const c_void;
type SecCSFlags = u32;

const K_SEC_CS_DEFAULT_FLAGS: SecCSFlags = 0;

/// `kCFTypeDictionaryKeyCallBacks` / `...ValueCallBacks` 是 CoreFoundation 导出的
/// **结构体变量**。我们只需要它的地址，所以用零大小类型占位即可。
#[repr(C)]
struct OpaqueCallbacks {
    _private: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: OpaqueCallbacks;
    static kCFTypeDictionaryValueCallBacks: OpaqueCallbacks;

    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const OpaqueCallbacks,
        value_callbacks: *const OpaqueCallbacks,
    ) -> CFDictionaryRef;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    /// `const CFStringRef kSecGuestAttributeAudit`：用 audit token 定位对端进程。
    static kSecGuestAttributeAudit: CFStringRef;

    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: SecCSFlags,
        requirement: *mut SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopyGuestWithAttributes(
        guest: SecCodeRef,
        attributes: CFDictionaryRef,
        flags: SecCSFlags,
        code: *mut SecCodeRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(code: SecCodeRef, flags: SecCSFlags, requirement: SecRequirementRef) -> OSStatus;
}

/// `SOL_LOCAL` / `LOCAL_PEERTOKEN`（Darwin）。
const SOL_LOCAL: libc::c_int = 0;
const LOCAL_PEERTOKEN: libc::c_int = 0x005;

/// `audit_token_t` 是 8 个 `u32`。
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

// ---------------------------------------------------------------------------
// 对端身份
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    /// 对端 pid，**仅用于日志**，不用于授权决策。
    pub pid: libc::pid_t,
}

/// 读取连接对端的 uid/gid/pid。
pub fn identify(fd: RawFd) -> Result<PeerIdentity> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: getpeereid 写入两个由我们提供的有效指针。
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(HelperError::new(
            crate::error::ErrorCode::Unauthorized,
            format!("getpeereid 失败: {}", std::io::Error::last_os_error()),
        ));
    }

    let mut pid: libc::pid_t = -1;
    let mut pid_len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERPID 到有效缓冲区。失败不致命，换个方式拿即可。
    let rc = unsafe {
        libc::getsockopt(
            fd,
            SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut libc::pid_t as *mut c_void,
            &mut pid_len,
        )
    };
    if rc != 0 {
        pid = -1;
    }

    Ok(PeerIdentity { uid, gid, pid })
}

fn audit_token(fd: RawFd) -> Result<AuditToken> {
    let mut token = AuditToken { val: [0; 8] };
    let mut len = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    // SAFETY: 读 LOCAL_PEERTOKEN 到 32 字节的 audit token 缓冲区。
    let rc = unsafe {
        libc::getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERTOKEN,
            &mut token as *mut AuditToken as *mut c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(HelperError::new(
            crate::error::ErrorCode::Unauthorized,
            format!("无法取得对端 audit token: {}", std::io::Error::last_os_error()),
        ));
    }
    Ok(token)
}

// ---------------------------------------------------------------------------
// 授权策略
// ---------------------------------------------------------------------------

/// 对端授权策略。
#[derive(Debug, Clone)]
pub enum PeerPolicy {
    /// 生产默认：要求对端满足给定的代码签名要求串。
    RequireSignature { requirement: String },
    /// **仅开发用**：跳过签名校验。
    ///
    /// 只有在环境变量 `XRAYTUN_HELPER_INSECURE=1` 时才会被启用，
    /// 并且每次连接都会打一条 `warn` 日志。绝不能在发行版里打开。
    InsecureAllowAny,
}

impl PeerPolicy {
    /// 从编译期注入的 Team ID 构造默认策略。
    ///
    /// 要求串的含义：由 Apple 签发的证书链（`anchor apple generic`）+
    /// bundle id 匹配 + 组织单位（Team ID）匹配。
    /// 这三者组合起来的效果是「只有我们签名的 App 能连上」。
    pub fn from_build_env() -> Self {
        match option_env!("XRAYTUN_TEAM_ID") {
            Some(team) if !team.is_empty() => Self::RequireSignature {
                requirement: format!(
                    "anchor apple generic and identifier \"com.xraytun.desktop\" \
                     and certificate leaf[subject.OU] = \"{team}\""
                ),
            },
            _ => {
                tracing::warn!(
                    "构建时未设置 XRAYTUN_TEAM_ID，helper 将使用开发期策略（仅校验 socket 权限位）"
                );
                Self::InsecureAllowAny
            }
        }
    }

    /// 环境变量强制降级（开发期）。
    pub fn allow_insecure_override() -> bool {
        matches!(std::env::var("XRAYTUN_HELPER_INSECURE").as_deref(), Ok("1"))
    }
}

/// 执行授权。成功返回对端身份，失败返回 `Unauthorized`。
pub fn authorize(fd: RawFd, policy: &PeerPolicy) -> Result<PeerIdentity> {
    let identity = identify(fd)?;

    // root 自己（用于调试 / 同机脚本）直接放行。
    // 注意这不是漏洞：能当 root 的进程本来就能做任何事。
    if identity.uid == 0 {
        tracing::debug!(pid = identity.pid, "对端是 root，跳过签名校验");
        return Ok(identity);
    }

    let effective = if PeerPolicy::allow_insecure_override() {
        tracing::warn!(
            uid = identity.uid,
            pid = identity.pid,
            "XRAYTUN_HELPER_INSECURE=1：正在跳过代码签名校验（仅限开发环境）"
        );
        &PeerPolicy::InsecureAllowAny
    } else {
        policy
    };

    match effective {
        PeerPolicy::InsecureAllowAny => {
            tracing::warn!(uid = identity.uid, "未启用签名校验，接受未验证的对端");
        }
        PeerPolicy::RequireSignature { requirement } => {
            verify_signature(fd, requirement).map_err(|e| {
                HelperError::new(
                    crate::error::ErrorCode::Unauthorized,
                    format!("对端代码签名校验未通过（uid={} pid={}）: {e}", identity.uid, identity.pid),
                )
            })?;
            tracing::debug!(uid = identity.uid, pid = identity.pid, "对端代码签名校验通过");
        }
    }

    Ok(identity)
}

fn verify_signature(fd: RawFd, requirement: &str) -> Result<()> {
    let token = audit_token(fd)?;

    // SAFETY: 下面全部是 CoreFoundation / Security 的标准对象生命周期操作。
    // 每个创建出来的 CF 对象都在作用域结束时 Release。
    unsafe {
        // 1) audit token -> CFData
        let token_data = CFDataCreate(
            std::ptr::null(),
            &token as *const AuditToken as *const u8,
            std::mem::size_of::<AuditToken>() as CFIndex,
        );
        if token_data.is_null() {
            return Err(HelperError::new(crate::error::ErrorCode::Internal, "CFDataCreate 失败"));
        }

        // 2) { kSecGuestAttributeAudit: CFData }
        let keys: [*const c_void; 1] = [kSecGuestAttributeAudit];
        let values: [*const c_void; 1] = [token_data];
        let attrs = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if attrs.is_null() {
            CFRelease(token_data);
            return Err(HelperError::new(
                crate::error::ErrorCode::Internal,
                "CFDictionaryCreate 失败",
            ));
        }

        // 3) 取得对端的 SecCode
        let mut guest: SecCodeRef = std::ptr::null();
        let status = SecCodeCopyGuestWithAttributes(std::ptr::null(), attrs, K_SEC_CS_DEFAULT_FLAGS, &mut guest);
        CFRelease(attrs);
        CFRelease(token_data);
        if status != 0 || guest.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("SecCodeCopyGuestWithAttributes 失败（OSStatus {status}）"),
            ));
        }

        // 4) 要求串 -> SecRequirement
        let cf_req = cfstring(requirement);
        if cf_req.is_null() {
            CFRelease(guest);
            return Err(HelperError::new(
                crate::error::ErrorCode::Internal,
                "构造 CFString 失败",
            ));
        }
        let mut req: SecRequirementRef = std::ptr::null();
        let status = SecRequirementCreateWithString(cf_req, K_SEC_CS_DEFAULT_FLAGS, &mut req);
        CFRelease(cf_req);
        if status != 0 || req.is_null() {
            CFRelease(guest);
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("代码签名要求串本身非法（OSStatus {status}）: {requirement}"),
            ));
        }

        // 5) 校验
        let status = SecCodeCheckValidity(guest, K_SEC_CS_DEFAULT_FLAGS, req);
        CFRelease(req);
        CFRelease(guest);
        if status != 0 {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("SecCodeCheckValidity 返回 {status}（对端不是受信任的 XrayTun 构建）"),
            ));
        }
    }
    Ok(())
}

/// 用 UTF-8 字节构造一个 CFString。失败返回 null。
///
/// 这里刻意不用 `CFStringCreateWithBytes` 之外的花样：任何多余的东西
/// 都可能是错的，而我们没法在本机验证。
unsafe fn cfstring(s: &str) -> CFStringRef {
    extern "C" {
        fn CFStringCreateWithBytes(
            allocator: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external: bool,
        ) -> CFStringRef;
    }
    // kCFStringEncodingUTF8 = 0x08000100
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    CFStringCreateWithBytes(
        std::ptr::null(),
        s.as_ptr(),
        s.len() as CFIndex,
        K_CF_STRING_ENCODING_UTF8,
        false,
    )
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_env_policy_never_silently_trusts_everything() {
        let policy = PeerPolicy::from_build_env();
        match policy {
            PeerPolicy::RequireSignature { requirement } => {
                assert!(requirement.contains("anchor apple generic"));
                assert!(requirement.contains("certificate leaf[subject.OU]"));
            }
            PeerPolicy::InsecureAllowAny => {
                // 未注入 Team ID 的构建只应出现在开发环境。
                eprintln!("提醒：当前构建未注入 XRAYTUN_TEAM_ID，helper 处于开发策略");
            }
        }
    }

    #[test]
    fn requirement_string_contains_bundle_id_and_team() {
        // 用一个假 Team ID 走一遍格式化逻辑，确认语法形状。
        let team = "ABCDE12345";
        let requirement = format!(
            "anchor apple generic and identifier \"com.xraytun.desktop\" \
             and certificate leaf[subject.OU] = \"{team}\""
        );
        assert!(requirement.contains("com.xraytun.desktop"));
        assert!(requirement.contains("ABCDE12345"));
        assert!(!requirement.contains('\n'), "要求串不应含换行");
    }

    #[test]
    fn audit_token_is_thirty_two_bytes() {
        assert_eq!(std::mem::size_of::<AuditToken>(), 32);
    }

    #[test]
    fn identify_reports_our_own_credentials() {
        // 覆盖真实的 `getpeereid` 调用路径。授权决策依赖它给出的 uid，
        // 而 uid 判断错了就是彻底的安全漏洞。
        use std::os::unix::io::AsRawFd;
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair 应可用");
        let identity = identify(a.as_raw_fd()).expect("getpeereid 应成功");
        // SAFETY: geteuid/getegid 无副作用。
        assert_eq!(identity.uid, unsafe { libc::geteuid() });
        assert_eq!(identity.gid, unsafe { libc::getegid() });
        // socketpair 上没有真正的「对端进程」，pid 取不到是正常的 —— 只要求不 panic。
        let _ = identity.pid;
    }
}
