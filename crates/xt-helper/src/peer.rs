//! 对端身份识别与代码签名校验。
//!
//! helper 是 root 守护进程，它的 Unix socket 就是一条**提权通道**。
//! 如果任何本地进程都能连接并下发「把默认路由指向 utunX」的指令，
//! 那就等于把 root 的部分能力开放给了全机器。
//!
//! 因此这里做三道校验（按"能拿到什么"选，见 [`PeerPolicy`]）：
//!
//! | 层次 | 机制 | 适用场景 | **挡不住什么** |
//! |---|---|---|---|
//! | 文件系统 | socket 权限位 `root:admin 0660` | 所有场景的**前置** | 同用户/同管理员组里的任何进程（`admin` 是 macOS 首用户默认组） |
//! | 证书链 + Team ID（[`PeerPolicy::RequireSignature`]） | `anchor apple generic` + bundle id + OU | 有 Developer ID 证书的正式发行 | 没有证书时用不了；只挡"不是这个 Team 签的"，挡不住"同一个 Team 被冒用/签名密钥被偷" |
//! | **cdhash 绑定**（[`PeerPolicy::RequireInstalledAppCdHash`]） | 对端 cdhash == 已安装 App 二进制的 cdhash（逐字节） | ad-hoc 签名（本仓库当前形态：`Signature=adhoc`、`TeamIdentifier=not set`） | **有能力替换 `/Applications` 下 App 的攻击者**（那已经需要管理员/root）；App 每次被重新签名（升级/改动）后 cdhash 会变，必须重装配套 |
//! | 拒绝服务（[`PeerPolicy::RefuseService`]） | 什么都不放行 | 既没证书也没 App（判据缺失） | 不"挡不住"，代价是功能全不可用 |
//!
//! 宽松策略（`InsecureAllowAny`）**只在 debug 构建存在**（`cfg(debug_assertions)`），
//! release 里连变体都不编译。
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
//!   一个不受信任的二进制必须被拒（`the_signature_check_rejects_...`）；
//! * cdhash 绑定由三条真机测试钉住：两个不同二进制的 cdhash 必须不同、
//!   授权层必须拒绝"不是那个二进制"的对端，以及（`--ignored`，需要装了 App）
//!   **我们读到的 cdhash == `codesign -dvvv` 报的 `CDHash=`**。
//!
//! 仍未覆盖的正路径是"真实签名的 App 进程连上来并通过"——那需要把 App 跑起来
//! 并让它连 helper（见 `docs/06-helper-protocol.md` 的「必须补的测试」一节）。

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
type CFURLRef = *const c_void;
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
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const OpaqueCallbacks,
        value_callbacks: *const OpaqueCallbacks,
    ) -> CFDictionaryRef;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    /// 用文件系统路径造 `CFURL`（取**磁盘上**那个二进制的 cdhash 用）。
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: bool,
    ) -> CFURLRef;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    /// `const CFStringRef kSecGuestAttributeAudit`：用 audit token 定位对端进程。
    static kSecGuestAttributeAudit: CFStringRef;

    /// `const CFStringRef kSecCodeInfoUnique`：签名信息里的 **cdhash**（`CFData`）。
    ///
    /// 这是本模块"无证书身份绑定"的锚：ad-hoc 签名同样有 cdhash，
    /// 而它由二进制的**实际字节**决定，不需要 Team ID 也能逐字节比对。
    static kSecCodeInfoUnique: CFStringRef;

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
    /// 取一个 `SecCode` 的签名信息字典（cdhash 在 `kSecCodeInfoUnique` 里）。
    fn SecCodeCopySigningInformation(
        code: SecCodeRef,
        flags: SecCSFlags,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    /// 从**磁盘路径**建立静态 `SecCode`（用来读已安装 App 的 cdhash）。
    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
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

/// 已安装 App 可执行文件的**固定位置**。
///
/// helper 只从这里读身份，**绝不接受请求参数给的路径** —— 否则攻击者只要
/// 让 helper 去比对他自己那个二进制就行。
pub const INSTALLED_APP_BINARY: &str = "/Applications/XrayTun.app/Contents/MacOS/xraytun-desktop";

/// 产物断言用的策略标识。
///
/// `verify-team-id-injection.sh --assert-helper` 这类检查要在**产物**上区分
/// 三种状态；[`PeerPolicy::describe`] 引用这些常量，所以只有真的编进产物的
/// 那条策略才会在 `strings` 里出现。
pub const POLICY_MARK_REQUIRE_SIGNATURE: &str = "XRAYTUN_HELPER_POLICY=require-signature";
pub const POLICY_MARK_CDHASH_BINDING: &str = "XRAYTUN_HELPER_POLICY=cdhash-binding";
pub const POLICY_MARK_REFUSE_SERVICE: &str = "XRAYTUN_HELPER_POLICY=refuse-service";
/// **只在 debug 构建存在**。release 产物里 grep 到它就说明 cfg 门失效了。
#[cfg(debug_assertions)]
pub const POLICY_MARK_INSECURE_DEBUG: &str = "XRAYTUN_HELPER_POLICY=insecure-allow-any-debug";

/// 对端授权策略。
///
/// # 三道门，按"能拿到什么"选
///
/// | 策略 | 前提 | 适用 |
/// |---|---|---|
/// | [`Self::RequireSignature`] | 有 Developer ID 证书（真 Team ID） | 有证书的正式发行 |
/// | [`Self::RequireInstalledAppCdHash`] | 只有 ad-hoc 签名（**没有 Team ID**） | 本仓库当前的发行形态 |
/// | [`Self::RefuseService`] | 既没 Team ID 也找不到已安装 App | 什么都不放行（fail-closed） |
/// | `InsecureAllowAny` | **debug 构建** | 开发机 |
#[derive(Debug, Clone)]
pub enum PeerPolicy {
    /// 要求对端满足给定的代码签名要求串（Apple 签发的链 + bundle id + Team ID）。
    RequireSignature { requirement: String },
    /// **不依赖证书的身份绑定**：对端进程的 **cdhash** 必须与
    /// [`INSTALLED_APP_BINARY`] 那个二进制的 cdhash **逐字节相同**。
    ///
    /// ad-hoc 签名同样有 cdhash，所以它不需要 Team ID 也能用。
    /// 代价与边界见模块文档与任务报告：它挡的是"同用户的其他进程"，
    /// **挡不住**有能力替换 `/Applications` 下 App 的攻击者（那已经需要管理员权限）。
    RequireInstalledAppCdHash { app_binary: std::path::PathBuf },
    /// 没有可用的身份判据 ⇒ **拒绝一切特权操作**（fail-closed，不是 fail-open）。
    RefuseService { reason: String },
    /// **仅 debug 构建**：跳过校验。
    ///
    /// `#[cfg(debug_assertions)]` —— release 里这个变体、`allow_insecure_override()`
    /// 以及 `authorize` 里的降级分支**都不编译**。这是编译期保证，
    /// 不是"运行时看到 env 就放行"。
    #[cfg(debug_assertions)]
    InsecureAllowAny,
}

impl PeerPolicy {
    /// 从编译期注入的 Team ID + 磁盘上是否存在已安装 App 决定策略。
    ///
    /// **`None` 与 `Some("")` 一律视为"未配置"**（ops 实测 `XRAYTUN_TEAM_ID=""`
    /// 会让 `option_env!` 返回 `Some("")` 并"胜出"；旧代码只查 `!is_empty()`
    /// 就变成了"未配置 ⇒ 信任任何人"）。这里改成：未配置 ⇒ 走 cdhash 绑定，
    /// 而不是全开。
    pub fn policy_for(team: Option<&str>, app_binary: &std::path::Path) -> Self {
        match team.map(str::trim).filter(|t| !t.is_empty()) {
            Some(team) => Self::RequireSignature { requirement: requirement_for(team) },
            None => {
                if app_binary.is_file() {
                    Self::RequireInstalledAppCdHash { app_binary: app_binary.to_path_buf() }
                } else {
                    Self::RefuseService {
                        reason: format!(
                            "既没有注入 XRAYTUN_TEAM_ID（None 或空串），也找不到已安装 App：{}",
                            app_binary.display()
                        ),
                    }
                }
            }
        }
    }

    /// 从编译期注入的 Team ID 构造默认策略。
    pub fn from_build_env() -> Self {
        let team = option_env!("XRAYTUN_TEAM_ID");
        #[cfg(debug_assertions)]
        {
            // ⚠️ 下面整块只在 debug 存在（`cfg(debug_assertions)`）。
            // release 构建里既没有 `InsecureAllowAny` 变体，也没有这个分支。
            if Self::allow_insecure_override() {
                tracing::warn!(
                    "XRAYTUN_HELPER_INSECURE=1（仅 debug 构建有效）：跳过对端校验"
                );
                return Self::InsecureAllowAny;
            }
            // 开发机常常还没装 App：debug + 没有 Team ID + 没有已安装 App 时
            // 才退回"信任任何对端"，并且每次都打 warn。
            if team.map(str::trim).filter(|t| !t.is_empty()).is_none()
                && !std::path::Path::new(INSTALLED_APP_BINARY).is_file()
            {
                tracing::warn!(
                    "debug 构建 + 未注入 XRAYTUN_TEAM_ID + 未安装 App：使用开发策略（信任任何对端）"
                );
                return Self::InsecureAllowAny;
            }
        }
        Self::policy_for(team, std::path::Path::new(INSTALLED_APP_BINARY))
    }

    /// 环境变量强制降级（**仅 debug 构建**；release 里这个函数不存在）。
    #[cfg(debug_assertions)]
    pub fn allow_insecure_override() -> bool {
        matches!(std::env::var("XRAYTUN_HELPER_INSECURE").as_deref(), Ok("1"))
    }

    /// 人读 + **可 grep 的策略标识**（进启动日志与产物断言）。
    pub fn describe(&self) -> String {
        match self {
            Self::RequireSignature { requirement } => {
                format!("{POLICY_MARK_REQUIRE_SIGNATURE} ({requirement})")
            }
            Self::RequireInstalledAppCdHash { app_binary } => {
                format!("{POLICY_MARK_CDHASH_BINDING} ({})", app_binary.display())
            }
            Self::RefuseService { reason } => format!("{POLICY_MARK_REFUSE_SERVICE} ({reason})"),
            #[cfg(debug_assertions)]
            Self::InsecureAllowAny => POLICY_MARK_INSECURE_DEBUG.to_string(),
        }
    }
}

fn requirement_for(team: &str) -> String {
    format!(
        "anchor apple generic and identifier \"com.xraytun.desktop\" \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// 执行授权。成功返回对端身份，失败返回 `Unauthorized`。
pub fn authorize(fd: RawFd, policy: &PeerPolicy) -> Result<PeerIdentity> {
    let identity = identify(fd)?;

    // root 自己（用于调试 / 同机脚本）直接放行。
    // 注意这不是漏洞：能当 root 的进程本来就能做任何事。
    if identity.uid == 0 {
        tracing::debug!(pid = identity.pid, "对端是 root，跳过身份校验");
        return Ok(identity);
    }

    #[cfg(debug_assertions)]
    let effective: &PeerPolicy = {
        if PeerPolicy::allow_insecure_override() {
            tracing::warn!(
                uid = identity.uid,
                pid = identity.pid,
                "XRAYTUN_HELPER_INSECURE=1（仅 debug 构建）：正在跳过身份校验"
            );
            &PeerPolicy::InsecureAllowAny
        } else {
            policy
        }
    };
    #[cfg(not(debug_assertions))]
    let effective: &PeerPolicy = policy;

    match effective {
        #[cfg(debug_assertions)]
        PeerPolicy::InsecureAllowAny => {
            tracing::warn!(uid = identity.uid, "未启用身份校验，接受未验证的对端（仅 debug）");
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
        PeerPolicy::RequireInstalledAppCdHash { app_binary } => {
            verify_installed_app_cdhash(fd, app_binary).map_err(|e| {
                HelperError::new(
                    crate::error::ErrorCode::Unauthorized,
                    format!(
                        "对端不是已安装的 App（cdhash 绑定，uid={} pid={}）: {e}",
                        identity.uid, identity.pid
                    ),
                )
            })?;
            tracing::debug!(uid = identity.uid, pid = identity.pid, "对端 cdhash 与已安装 App 相同");
        }
        PeerPolicy::RefuseService { reason } => {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("helper 没有可用的对端身份判据，拒绝服务：{reason}"),
            ));
        }
    }

    Ok(identity)
}

/// 取对端进程的 `SecCode`（**调用方负责 `CFRelease`**）。
fn peer_guest(fd: RawFd) -> Result<SecCodeRef> {
    let token = audit_token(fd)?;
    // SAFETY: 下面全部是 CoreFoundation / Security 的标准对象生命周期操作。
    // 创建出来的 CFData/CFDictionary 用完即 Release；返回的 `SecCode` 由调用方 Release。
    unsafe {
        let token_data = CFDataCreate(
            std::ptr::null(),
            &token as *const AuditToken as *const u8,
            std::mem::size_of::<AuditToken>() as CFIndex,
        );
        if token_data.is_null() {
            return Err(HelperError::new(crate::error::ErrorCode::Internal, "CFDataCreate 失败"));
        }

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

        let mut guest: SecCodeRef = std::ptr::null();
        let status =
            SecCodeCopyGuestWithAttributes(std::ptr::null(), attrs, K_SEC_CS_DEFAULT_FLAGS, &mut guest);
        CFRelease(attrs);
        CFRelease(token_data);
        if status != 0 || guest.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("SecCodeCopyGuestWithAttributes 失败（OSStatus {status}）"),
            ));
        }
        Ok(guest)
    }
}

fn verify_signature(fd: RawFd, requirement: &str) -> Result<()> {
    let guest = peer_guest(fd)?;
    // 要求串 -> SecRequirement -> 校验。
    //
    // 抽成 [`sec_code_satisfies`] 是为了能在测试里拿 `SecCodeCopySelf()` 的结果
    // 走**同一段**校验代码：一个不受信任的二进制必须在这里被拒。
    let verdict = sec_code_satisfies(guest, requirement);
    // SAFETY: `peer_guest` 返回的是一个需要 Release 的 Security 对象。
    unsafe { CFRelease(guest) };
    verdict
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

// ---------------------------------------------------------------------------
// cdhash 身份绑定（不依赖 Developer ID 证书）
// ---------------------------------------------------------------------------

/// 取一段 `CFData` 的字节。`null` / 空 ⇒ `None`。
fn cfdata_bytes(data: CFDataRef) -> Option<Vec<u8>> {
    if data.is_null() {
        return None;
    }
    // SAFETY: 只读一个 CFData 的字节区间，指针在 CFData 的生命周期内有效。
    unsafe {
        let len = CFDataGetLength(data);
        if len <= 0 {
            return None;
        }
        let ptr = CFDataGetBytePtr(data);
        if ptr.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr, len as usize).to_vec())
    }
}

/// 从一个 `SecCode` 取 **cdhash**（签名信息里的 `kSecCodeInfoUnique`）。
fn code_cdhash(code: SecCodeRef) -> Result<Vec<u8>> {
    if code.is_null() {
        return Err(HelperError::new(crate::error::ErrorCode::Internal, "SecCode 为空"));
    }
    // SAFETY: `SecCodeCopySigningInformation` 写一个我们提供的有效指针，返回的字典由我们
    // Release；`kSecCodeInfoUnique` 是 Security.framework 导出的 CFString 常量。
    unsafe {
        let mut info: CFDictionaryRef = std::ptr::null();
        let status = SecCodeCopySigningInformation(code, K_SEC_CS_DEFAULT_FLAGS, &mut info);
        if status != 0 || info.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("取签名信息失败（OSStatus {status}）——对端可能完全没有签名"),
            ));
        }
        let unique = CFDictionaryGetValue(info, kSecCodeInfoUnique) as CFDataRef;
        let bytes = cfdata_bytes(unique);
        CFRelease(info);
        bytes.ok_or_else(|| {
            HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                "签名信息里没有 cdhash（kSecCodeInfoUnique）——这个二进制没有可用签名"
                    .to_string(),
            )
        })
    }
}

/// 取**磁盘上**某个二进制（已安装 App）的 cdhash。
fn static_cdhash(path: &std::path::Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: 标准 CoreFoundation / Security 生命周期；CFURL 与 SecCode 都在作用域内 Release。
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            false,
        );
        if url.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Internal,
                "CFURLCreateFromFileSystemRepresentation 失败",
            ));
        }
        let mut code: SecCodeRef = std::ptr::null();
        let status = SecStaticCodeCreateWithPath(url, K_SEC_CS_DEFAULT_FLAGS, &mut code);
        CFRelease(url);
        if status != 0 || code.is_null() {
            return Err(HelperError::new(
                crate::error::ErrorCode::Unauthorized,
                format!("读不到 {} 的代码签名（OSStatus {status}）", path.display()),
            ));
        }
        let hash = code_cdhash(code);
        CFRelease(code);
        hash
    }
}

/// **逐字节**比较两个 cdhash。长度不同 / 任一为空 ⇒ `false`。
pub fn cdhashes_match(a: &[u8], b: &[u8]) -> bool {
    !a.is_empty() && a == b
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 校验对端进程的 cdhash == [`INSTALLED_APP_BINARY`] 的 cdhash。
fn verify_installed_app_cdhash(fd: RawFd, app_binary: &std::path::Path) -> Result<()> {
    // 先读"应该是谁"：读不到就拒绝（fail-closed），不许因为读不到而放行。
    let want = static_cdhash(app_binary)?;
    let guest = peer_guest(fd)?;
    let got = code_cdhash(guest);
    // SAFETY: `peer_guest` 返回的是需要 Release 的 Security 对象。
    unsafe { CFRelease(guest) };
    let got = got?;
    if !cdhashes_match(&got, &want) {
        return Err(HelperError::new(
            crate::error::ErrorCode::Unauthorized,
            format!(
                "cdhash 不一致：对端 {}，已安装 App {}（{}）",
                hex(&got),
                hex(&want),
                app_binary.display()
            ),
        ));
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
            PeerPolicy::RequireInstalledAppCdHash { app_binary } => {
                assert_eq!(
                    app_binary,
                    std::path::Path::new(INSTALLED_APP_BINARY),
                    "cdhash 绑定只认固定位置，绝不接受请求参数给的路径"
                );
            }
            PeerPolicy::RefuseService { reason } => {
                assert!(!reason.is_empty(), "拒绝服务必须留下可读原因");
            }
            // 宽松策略**只可能在 debug 构建**出现（release 里连变体都不编译）。
            #[cfg(debug_assertions)]
            PeerPolicy::InsecureAllowAny => {
                eprintln!(
                    "提醒：debug 构建 + 当前环境未走到真策略（显式 XRAYTUN_HELPER_INSECURE=1，\
                     或未注入 Team ID 且本机没有已安装 App）"
                );
            }
        }
    }

    #[test]
    fn requirement_string_contains_bundle_id_and_team() {
        // 用一个假 Team ID 走一遍**生产用的**格式化函数，确认语法形状。
        let requirement = requirement_for("ABCDE12345");
        assert!(requirement.contains("com.xraytun.desktop"));
        assert!(requirement.contains("ABCDE12345"));
        assert!(!requirement.contains('\n'), "要求串不应含换行");
    }

    // -----------------------------------------------------------------------
    // task-17：不依赖 Developer ID 的身份绑定
    // -----------------------------------------------------------------------

    /// 判别性：**空串必须当"未配置"**（ops 实测 `XRAYTUN_TEAM_ID=""` 会让
    /// `option_env!` 返回 `Some("")` 并"胜出"）。
    ///
    /// 旧代码的判据是 `Some(team) if !team.is_empty()`，空串会落进 `_` 分支 ⇒
    /// 旧行为是"信任任何人"；新行为是"走 cdhash 绑定"（有 App）或"拒绝服务"（没有）。
    #[test]
    fn an_empty_or_blank_team_id_is_treated_as_unconfigured() {
        let app = std::env::current_exe().expect("current_exe");
        for team in [None, Some(""), Some("   "), Some("\t")] {
            let p = PeerPolicy::policy_for(team, &app);
            assert!(
                matches!(p, PeerPolicy::RequireInstalledAppCdHash { .. }),
                "{team:?} 必须当未配置 ⇒ cdhash 绑定，实际 {p:?}"
            );
            assert!(!p.describe().contains(POLICY_MARK_REQUIRE_SIGNATURE));
        }
    }

    /// 真 Team ID ⇒ 签名要求串（三条策略里的第一条）。
    #[test]
    fn a_real_team_id_selects_the_signature_requirement() {
        let p = PeerPolicy::policy_for(Some("ABCDE12345"), std::path::Path::new("/nonexistent"));
        match &p {
            PeerPolicy::RequireSignature { requirement } => {
                assert!(requirement.contains("certificate leaf[subject.OU] = \"ABCDE12345\""));
            }
            other => panic!("应当是 RequireSignature，实际 {other:?}"),
        }
        assert!(p.describe().contains(POLICY_MARK_REQUIRE_SIGNATURE));
    }

    /// 判别性：既没 Team ID 也找不到已安装 App ⇒ **拒绝服务**（不是全开）。
    #[test]
    fn no_team_id_and_no_installed_app_refuses_service() {
        let p = PeerPolicy::policy_for(None, std::path::Path::new("/nonexistent/XrayTun.app"));
        assert!(matches!(p, PeerPolicy::RefuseService { .. }), "{p:?}");
        assert!(p.describe().contains(POLICY_MARK_REFUSE_SERVICE));
        assert!(p.describe().contains("XrayTun.app"), "原因要指出找的是哪个路径");
    }

    /// 三种策略的产物标识各不相同（`verify-team-id-injection.sh` 那类断言要用）。
    #[test]
    fn each_policy_has_its_own_product_assertion_marker() {
        let sig = PeerPolicy::policy_for(Some("ABCDE12345"), std::path::Path::new("/x"));
        let cd = PeerPolicy::RequireInstalledAppCdHash {
            app_binary: std::path::PathBuf::from(INSTALLED_APP_BINARY),
        };
        let rf = PeerPolicy::RefuseService { reason: "x".into() };
        assert!(sig.describe().contains(POLICY_MARK_REQUIRE_SIGNATURE));
        assert!(cd.describe().contains(POLICY_MARK_CDHASH_BINDING));
        assert!(rf.describe().contains(POLICY_MARK_REFUSE_SERVICE));
        #[cfg(debug_assertions)]
        assert!(PeerPolicy::InsecureAllowAny
            .describe()
            .contains(POLICY_MARK_INSECURE_DEBUG));
    }

    /// 判别性：**不同 cdhash 一律不匹配**；空的也不匹配。
    #[test]
    fn different_or_empty_cdhashes_never_match() {
        assert!(cdhashes_match(&[1, 2, 3], &[1, 2, 3]));
        assert!(!cdhashes_match(&[1, 2, 3], &[1, 2, 4]), "改一个字节就不许匹配");
        assert!(!cdhashes_match(&[1, 2, 3], &[1, 2]), "长度不同不许匹配");
        assert!(!cdhashes_match(&[], &[]), "空 cdhash 不许匹配");
        assert!(!cdhashes_match(&[], &[1]), "空 cdhash 不许匹配");
    }

    /// 真机上用**真的** Security.framework 取 cdhash：`/bin/ls` 有，
    /// 未签名的普通文件没有。
    #[test]
    fn cdhash_can_be_read_from_a_signed_binary_and_not_from_an_unsigned_file() {
        let ls = static_cdhash(std::path::Path::new("/bin/ls")).expect("/bin/ls 应当有 cdhash");
        assert_eq!(ls.len(), 20, "cdhash 是 20 字节");
        // 同一个文件两次结果必须一致（不能是随机/时间相关的）。
        let again = static_cdhash(std::path::Path::new("/bin/ls")).unwrap();
        assert_eq!(ls, again);

        let plain = std::env::temp_dir().join(format!("xt-unsigned-{}.bin", std::process::id()));
        std::fs::write(&plain, b"not a mach-o").unwrap();
        let err = static_cdhash(&plain).expect_err("未签名文件必须读不到 cdhash");
        assert!(
            err.to_string().contains("代码签名") || err.to_string().contains("cdhash"),
            "错误要可读：{err}"
        );
        let _ = std::fs::remove_file(&plain);

        let missing = static_cdhash(std::path::Path::new("/nonexistent/app"));
        assert!(missing.is_err(), "不存在的路径必须报错，不许当成'没有签名就算了'");
    }

    /// **判别性（核心）**：两个不同二进制的 cdhash 必须不同 ⇒ 拿 A 的 cdhash 去比 B 必被拒。
    #[test]
    fn a_different_binarys_cdhash_is_refused() {
        let want = static_cdhash(std::path::Path::new("/bin/ls")).unwrap();
        let other = static_cdhash(std::path::Path::new("/bin/cat")).unwrap();
        assert!(!cdhashes_match(&other, &want), "ls 与 cat 的 cdhash 不许相等");
        assert!(cdhashes_match(&want, &want));
    }

    /// 端到端（授权层）：cdhash 绑定的策略下，一个 cdhash 与"已安装 App"不同的对端
    /// 必须返回 `Unauthorized`。用 socketpair 作对端、`/bin/ls` 当"已安装 App"。
    #[test]
    fn the_cdhash_policy_refuses_a_peer_that_is_not_the_bound_binary() {
        use std::os::unix::io::AsRawFd;
        // SAFETY: geteuid 无副作用。
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("提醒：以 root 运行，authorize 会直接放行 —— 跳过");
            return;
        }
        // 显式降级（仅 debug 有效）会让 authorize 放行；那属于另一条测试的语义。
        if PeerPolicy::allow_insecure_override() {
            eprintln!("提醒：XRAYTUN_HELPER_INSECURE=1（仅 debug）—— 跳过");
            return;
        }
        let (a, _b) = socket_pair();
        let policy = PeerPolicy::RequireInstalledAppCdHash {
            app_binary: std::path::PathBuf::from("/bin/ls"),
        };
        let err = authorize(a.as_raw_fd(), &policy).expect_err("对端不是 /bin/ls，必须被拒");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        let msg = err.to_string();
        assert!(
            msg.contains("cdhash 绑定") || msg.contains("cdhash 不一致"),
            "原因要能读懂：{msg}"
        );
    }

    /// 已安装 App 指到不存在的路径 ⇒ 直接拒绝（fail-closed，不许因为"读不到"而放行）。
    #[test]
    fn a_missing_bound_binary_is_refused_without_touching_the_peer() {
        use std::os::unix::io::AsRawFd;
        let (a, _b) = socket_pair();
        let policy = PeerPolicy::RequireInstalledAppCdHash {
            app_binary: std::path::PathBuf::from("/nonexistent/XrayTun.app/xraytun-desktop"),
        };
        let err = authorize(a.as_raw_fd(), &policy).expect_err("绑定路径不存在必须拒绝");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        assert!(err.to_string().contains("读不到"), "{err}");
    }

    /// `RefuseService` ⇒ 对端（非 root）一律 `Unauthorized`，并带可读原因。
    #[test]
    fn refuse_service_policy_denies_every_peer() {
        use std::os::unix::io::AsRawFd;
        // SAFETY: geteuid 无副作用。
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("提醒：以 root 运行，authorize 会直接放行 —— 跳过");
            return;
        }
        if PeerPolicy::allow_insecure_override() {
            eprintln!("提醒：XRAYTUN_HELPER_INSECURE=1（仅 debug）—— 跳过");
            return;
        }
        let (a, _b) = socket_pair();
        let policy = PeerPolicy::RefuseService { reason: "没证书也没装 App".into() };
        let err = authorize(a.as_raw_fd(), &policy).expect_err("必须拒绝");
        assert_eq!(err.code, crate::error::ErrorCode::Unauthorized);
        assert!(err.to_string().contains("拒绝服务"), "{err}");
        assert!(err.to_string().contains("没证书也没装 App"), "原因要带出来：{err}");
    }

    /// **正路径的真实用例**：我们取到的"已安装 App" cdhash 必须与
    /// `/usr/bin/codesign -dvvv` 报告的 `CDHash=` **逐字符相同**。
    ///
    /// 这条证明 cdhash 绑定赖以成立的那一步：`SecStaticCodeCreateWithPath` +
    /// `SecCodeCopySigningInformation(kSecCodeInfoUnique)` 读出来的就是 Apple 说的那个
    /// cdhash（**ad-hoc 签名也有**）。默认 `#[ignore]` —— CI 上没有这个 App。
    ///
    /// ```bash
    /// cargo test -p xt-helper -- --ignored real_installed_app_cdhash
    /// ```
    #[test]
    #[ignore = "需要本机安装 /Applications/XrayTun.app；手动跑"]
    fn real_installed_app_cdhash_matches_what_codesign_reports() {
        let app = std::path::Path::new(INSTALLED_APP_BINARY);
        if !app.is_file() {
            eprintln!("跳过：本机没有 {}", app.display());
            return;
        }
        let ours = static_cdhash(app).expect("已安装 App 必须有 cdhash");
        assert_eq!(ours.len(), 20, "cdhash 是 20 字节");

        let out = std::process::Command::new("/usr/bin/codesign")
            .arg("-dvvv")
            .arg(app)
            .output()
            .expect("跑 codesign");
        // codesign 把描述写到 stderr。
        let text = String::from_utf8_lossy(&out.stderr);
        let reported = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("CDHash="))
            .expect("codesign 应当报出 CDHash")
            .trim()
            .to_ascii_lowercase();
        assert_eq!(hex(&ours), reported, "我们读到的 cdhash 必须就是 codesign 报的那个");
    }

    /// **文本守卫（判别性）**：`InsecureAllowAny` 只允许在 `#[cfg(debug_assertions)]`
    /// 下声明 ⇒ **release 里整段不编译**（不是运行时判断）。
    ///
    /// 这条在"有人把 cfg 删掉"时会红；同时配 `cargo build --release` + `strings`
    /// 的产物证据（见提交说明）。
    #[test]
    fn the_loose_policy_is_cfg_gated_out_of_release() {
        // ⚠️ **不能**用 `split("#[cfg(test)]")` 截断：本文件里 `SecCodeCopySelf` 的
        // 声明也带着 `#[cfg(test)]`，那样会把枚举之前的内容全切掉（这条守卫第一次
        // 就是这么假红的）。生产声明的缩进是 4 空格，测试里引用它的缩进更深，
        // 所以按"4 空格 + 属性 + 声明"整段匹配是可靠的。
        let prod = include_str!("peer.rs");
        assert!(
            prod.contains("#[cfg(debug_assertions)]\n    InsecureAllowAny,"),
            "InsecureAllowAny 变体必须由 cfg(debug_assertions) gate 住"
        );
        assert!(
            prod.contains("#[cfg(debug_assertions)]\n    pub fn allow_insecure_override"),
            "allow_insecure_override() 也必须只在 debug 存在"
        );
        // 降级选择本身也整段在 cfg 里：release 里连"判断要不要降级"这一步都不存在。
        assert!(
            prod.contains("#[cfg(debug_assertions)]\n    let effective: &PeerPolicy"),
            "authorize 的降级分支必须整段 cfg(debug_assertions) gate 住"
        );
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
