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
//! 函数语义与常量名必须在运行时验证。这里不再只靠注释：
//!
//! * audit token 的选项号由真实 `getsockopt` 钉住 —— `0x006` 回 32 字节，
//!   `0x005`（`LOCAL_PEEREUUID`）只回 16 字节，长度不对必须拒绝；
//! * 签名校验分支由 `SecCodeCopySelf()` + 生产形状要求串钉住：
//!   一个不受信任的二进制必须被拒（`the_signature_check_rejects_...`）。
//!
//! 仍未在 CI 覆盖的是"真签名的 App 能通过"这条正路径 —— 那需要发行构建
//! （见 `docs/06-helper-protocol.md` 的「必须补的测试」一节）。

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
    /// 取**本进程**的 `SecCode`。生产路径不用它；测试用它来钉住
    /// "要求串真的会拒绝一个不受信任的二进制"（见 `the_signature_check_...`）。
    /// 只编进测试构建，生产二进制里连符号都不需要。
    #[cfg(test)]
    fn SecCodeCopySelf(flags: SecCSFlags, code: *mut SecCodeRef) -> OSStatus;
    fn SecCodeCheckValidity(code: SecCodeRef, flags: SecCSFlags, requirement: SecRequirementRef) -> OSStatus;
}

/// `SOL_LOCAL`（Darwin）。**audit token 的选项号不手写**：见 [`LOCAL_PEERTOKEN`]。
const SOL_LOCAL: libc::c_int = 0;

/// audit token 的 socket 选项号。
///
/// **绝不要把它写成字面量**：这里曾经手工写着 `0x005`，而 `0x005` 是
/// `LOCAL_PEEREUUID`（只回 16 字节）；真正的 `LOCAL_PEERTOKEN` 是 `0x006`（32 字节）。
/// 后果不是"少一点信息"，而是签名校验拿一个形状不对的 token 去问
/// Security.framework ⇒ **合法 App 也会被一律拒掉**、这道门在生产上等于不存在。
/// `libc` 里两个常量都有定义，直接用它的值，语义由测试用真实 `getsockopt` 钉住。
const LOCAL_PEERTOKEN: libc::c_int = libc::LOCAL_PEERTOKEN;

/// `audit_token_t` 是 8 个 `u32`。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
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

/// 从 socket 上读一个 `SOL_LOCAL` 选项的原始字节，返回内核写入的字节数。
///
/// 抽出来是为了让测试能用**真的** `getsockopt` 对比 `LOCAL_PEERTOKEN`（0x006，
/// 32 字节）与 `LOCAL_PEEREUUID`（0x005，16 字节）的返回长度，
/// 而不是把"常量是对的"只写在注释里。
fn read_local_option(fd: RawFd, option: libc::c_int, buf: &mut [u8]) -> Result<usize> {
    let mut len = buf.len() as libc::socklen_t;
    // SAFETY: `buf` 是调用方提供的有效可写切片，初始长度就是 `len`；
    // getsockopt 只会写入不超过 `len` 字节，并在 `len` 里回写实际长度。
    let rc = unsafe {
        libc::getsockopt(
            fd,
            SOL_LOCAL,
            option,
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(HelperError::new(
            crate::error::ErrorCode::Unauthorized,
            format!(
                "getsockopt(SOL_LOCAL, {option:#06x}) 失败: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(len as usize)
}

/// audit token 必须是完整的 32 字节。
///
/// 长度不对就**拒绝**：半截 token 交给 `SecCodeCopyGuestWithAttributes`
/// 只会得到"不是合法 audit token"，既看不出根因，也谈不上安全。
fn check_token_len(got: usize) -> Result<()> {
    let want = std::mem::size_of::<AuditToken>();
    if got != want {
        return Err(HelperError::new(
            crate::error::ErrorCode::Unauthorized,
            format!(
                "对端 audit token 只有 {got} 字节（应为 {want}）—— \
                 取错了 socket 选项或对端不支持 LOCAL_PEERTOKEN；拒绝授权"
            ),
        ));
    }
    Ok(())
}

fn audit_token(fd: RawFd) -> Result<AuditToken> {
    let mut token = AuditToken { val: [0; 8] };
    // SAFETY: `AuditToken` 是 `#[repr(C)]` 的 8×u32、无填充；
    // 按字节视图写满 `size_of::<AuditToken>()` 字节是有效的。
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            token.val.as_mut_ptr() as *mut u8,
            std::mem::size_of::<AuditToken>(),
        )
    };
    let got = read_local_option(fd, LOCAL_PEERTOKEN, bytes)?;
    check_token_len(got)?;
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

        // 4) + 5) 要求串 -> SecRequirement -> 校验。
        //
        // 抽成 [`sec_code_satisfies`] 是为了能在测试里拿 `SecCodeCopySelf()` 的结果
        // 走**同一段**校验代码：一个不受信任的二进制必须在这里被拒。
        let verdict = sec_code_satisfies(guest, requirement);
        CFRelease(guest);
        verdict?;
    }
    Ok(())
}

/// 一个已取得的 `SecCode` 是否满足要求串。失败返回**可读**原因。
fn sec_code_satisfies(code: SecCodeRef, requirement: &str) -> Result<()> {
    if code.is_null() {
        return Err(HelperError::new(
            crate::error::ErrorCode::Internal,
            "SecCode 为空（内核没有给出对端代码对象）",
        ));
    }
    // SAFETY: 标准 CoreFoundation / Security 对象生命周期；创建出来的
    // CFString / SecRequirement 都在本函数作用域内 Release。
    unsafe {
        let cf_req = cfstring(requirement);
        if cf_req.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Internal,
                "构造 CFString 失败",
            ));
        }
        let mut req: SecRequirementRef = std::ptr::null();
        let status = SecRequirementCreateWithString(cf_req, K_SEC_CS_DEFAULT_FLAGS, &mut req);
        CFRelease(cf_req);
        if status != 0 || req.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("代码签名要求串本身非法（OSStatus {status}）: {requirement}"),
            ));
        }

        let status = SecCodeCheckValidity(code, K_SEC_CS_DEFAULT_FLAGS, req);
        CFRelease(req);
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

    // -----------------------------------------------------------------------
    // P0-1：audit token 的选项号必须是 0x006
    //
    // 这些测试用**真的** `getsockopt` 跑，而不是断言注释。修前常量是 0x005
    // （LOCAL_PEEREUUID）：内核只回 16 字节，而代码按 32 字节的 AuditToken
    // 交给 Security.framework ⇒ 合法 App 也会被一律拒掉。
    // -----------------------------------------------------------------------

    fn socket_pair() -> (std::os::unix::net::UnixStream, std::os::unix::net::UnixStream) {
        std::os::unix::net::UnixStream::pair().expect("socketpair 应可用")
    }

    /// 判别性：`0x006` 才是 audit token（32 字节）；`0x005` 是 16 字节的 UUID。
    /// 谁把常量改回 0x005，这条测试就会红。
    #[test]
    fn the_peer_token_option_is_the_32_byte_audit_token_not_the_euuid_one() {
        use std::os::unix::io::AsRawFd;
        assert_eq!(LOCAL_PEERTOKEN, 0x006, "LOCAL_PEEREUUID(0x005) 不是 audit token");
        assert_eq!(libc::LOCAL_PEEREUUID, 0x005, "libc 的定义也要对上（我们按它对照）");

        let (a, _b) = socket_pair();
        let fd = a.as_raw_fd();

        let mut uuid = [0u8; 32];
        let uuid_len = read_local_option(fd, libc::LOCAL_PEEREUUID, &mut uuid)
            .expect("LOCAL_PEEREUUID 应当可读");
        let mut token = [0u8; 32];
        let token_len =
            read_local_option(fd, LOCAL_PEERTOKEN, &mut token).expect("LOCAL_PEERTOKEN 应当可读");

        // **实测**：0x005 回 16 字节，0x006 回 32 字节（本机复算见提交说明）。
        assert_eq!(uuid_len, 16, "0x005 = LOCAL_PEEREUUID，只回 16 字节");
        assert_eq!(token_len, 32, "0x006 = LOCAL_PEERTOKEN，回满 32 字节 audit token");
        // 判别性：半截 token 必须被 `check_token_len` 拒掉，而 32 字节通过。
        assert!(check_token_len(uuid_len).is_err(), "16 字节必须被拒");
        assert!(check_token_len(token_len).is_ok(), "32 字节必须通过");
    }

    /// 正路径：真的 socketpair 上能取到完整 32 字节 token。
    #[test]
    fn audit_token_accepts_a_real_socket_pair_peer() {
        use std::os::unix::io::AsRawFd;
        let (a, _b) = socket_pair();
        let token = audit_token(a.as_raw_fd()).expect("32 字节 token 应当被接受");
        assert_eq!(token.val.len(), 8, "audit_token_t = 8×u32");
    }

    /// 判别性：长度不对时**拒绝**，且原因可读（指得出 socket 选项）。
    #[test]
    fn a_short_token_is_refused_with_a_readable_reason() {
        let err = check_token_len(16).expect_err("16 字节必须被拒");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        let msg = err.to_string();
        assert!(msg.contains("16") && msg.contains("32"), "要说清 16 vs 32：{msg}");
        assert!(msg.contains("LOCAL_PEERTOKEN"), "要指得出错的 socket 选项：{msg}");
        assert!(check_token_len(32).is_ok());
    }

    /// 判别性：拿不到 token 时返回 `Unauthorized`，**不许 fail-open**（不 panic）。
    #[test]
    fn a_bad_fd_is_refused_with_a_readable_reason() {
        let err = audit_token(-1).expect_err("坏 fd 必须被拒");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        assert!(err.to_string().contains("getsockopt"), "{err}");
    }

    /// 生产形状的要求串（用 `from_build_env` 的同一模板 + 一个不可能是本进程的 Team ID）。
    fn production_shaped_requirement(team: &str) -> String {
        format!(
            "anchor apple generic and identifier \"com.xraytun.desktop\" \
             and certificate leaf[subject.OU] = \"{team}\""
        )
    }

    /// 判别性：拿**本测试二进制**的 SecCode 去跑生产形状的要求串，必须被拒。
    ///
    /// 这条钉住签名分支不是"永远 Ok"：它走的是 `verify_signature` 里同一段
    /// `SecRequirementCreateWithString` + `SecCodeCheckValidity`，只是把
    /// "对端" 换成了"自己"。测试二进制没有 `com.xraytun.desktop` 这个标识符，
    /// 所以任何签名状态都必须失败。
    #[test]
    fn the_signature_check_rejects_an_untrusted_binary() {
        let mut code: SecCodeRef = std::ptr::null();
        // SAFETY: SecCodeCopySelf 写一个我们提供的有效指针。
        let status = unsafe { SecCodeCopySelf(K_SEC_CS_DEFAULT_FLAGS, &mut code) };
        assert_eq!(status, 0, "SecCodeCopySelf 应当成功（OSStatus {status}）");
        assert!(!code.is_null());

        let verdict = sec_code_satisfies(code, &production_shaped_requirement("0000000000"));
        // SAFETY: `code` 是 SecCodeCopySelf 返回的、需要 CFRelease 的对象。
        unsafe { CFRelease(code) };

        let err = verdict.expect_err("不受信任的二进制必须被拒");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        assert!(err.to_string().contains("SecCodeCheckValidity"), "{err}");
    }

    /// 判别性：真实 socket 上的签名校验必须拒绝一个不受信任的对端。
    /// 这条把 `getsockopt(0x006)` → CFData → `SecCodeCopyGuestWithAttributes`
    /// → `SecCodeCheckValidity` 整条链路真的跑一遍（旧常量 0x005 时会在
    /// token 长度那一步就拒绝，同样是 Err，但那是"取不到 token"而不是"校验不过"）。
    #[test]
    fn verify_signature_rejects_an_untrusted_peer_over_a_real_socket() {
        use std::os::unix::io::AsRawFd;
        let (a, _b) = socket_pair();
        let err = verify_signature(a.as_raw_fd(), &production_shaped_requirement("0000000000"))
            .expect_err("不受信任的对端不许通过");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
    }

    /// 判别性：`authorize` 在签名校验失败时必须返回 `Unauthorized` 并带上可读原因，
    /// **不许**把 `Err` 吞掉变成 `Ok`（那才是"门是空的"）。
    #[test]
    fn authorize_refuses_when_the_signature_check_fails() {
        // SAFETY: geteuid 无副作用。
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("提醒：以 root 运行，authorize 会直接放行（root 本来就能做任何事）—— 跳过");
            return;
        }
        if PeerPolicy::allow_insecure_override() {
            eprintln!("提醒：XRAYTUN_HELPER_INSECURE=1 已设，authorize 会降级 —— 跳过");
            return;
        }
        use std::os::unix::io::AsRawFd;
        let (a, _b) = socket_pair();
        let policy = PeerPolicy::RequireSignature {
            requirement: production_shaped_requirement("0000000000"),
        };
        let err = authorize(a.as_raw_fd(), &policy).expect_err("不受信任的对端必须被拒");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        let msg = err.to_string();
        assert!(msg.contains("签名校验未通过"), "原因要能读懂：{msg}");
    }
}
