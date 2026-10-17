//! 对端身份识别与代码签名校验。
//!
//! helper 是 root 守护进程，它的 AF_UNIX socket 就是一条**提权通道**。如果任何
//! 本地进程都能连接并下发「把默认路由指向 utunX」，那就等于把 root 的一部分
//! 能力开放给了整台机器。所以在 `accept` 之后、处理任何指令之前，helperd 必须
//! 先回答一个问题：**这个对端是不是我们预期的 daemon？**
//!
//! # 三道门（按「能拿到什么」选，见 [`TrustPolicy`]）
//!
//! | 层次 | 机制 | 适用 | 挡不住什么 |
//! |---|---|---|---|
//! | 文件系统 | socket 权限位 `root:admin 0660`（见 `server`） | 所有场景的**前置** | 同 admin 组里的任何进程 |
//! | 证书链 + Team ID | `anchor apple generic` + bundle id + OU | 有 Developer ID 证书的正式发行 | 没有证书时用不了 |
//! | **cdhash 绑定** | 对端 cdhash == 已安装组件的 cdhash（逐字节） | ad-hoc 签名（当前仓库形态） | 有能力替换 `/Applications` 下 App 的攻击者（那已需要 root） |
//! | 拒绝服务 | 什么都不放行（fail-closed） | 既没证书也没 App（判据缺失） | 代价是功能全不可用 —— 但绝不 fail-open |
//!
//! **没有**「信任任何对端」的运行时开关：那不是降级，那是在 root 进程里把门拆掉。
//!
//! # 为什么用 audit token 而不是 pid
//!
//! pid 会被回收：`SecCodeCopyGuestWithAttributes(kSecGuestAttributePid)` 按 pid
//! 反查进程，而在「取 pid」到「校验签名」之间该 pid 可能被分配给另一个进程
//! （TOCTOU）。audit token 是内核给连接打上的不可伪造标识，不存在这个问题；
//! 它由 [`crate::fdpass::peer_audit_token`] 从 socket 上取。
//!
//! # 诚实清单：哪些部分**待真机**
//!
//! 下面是本模块里「类型层面正确、但尚未在真机运行时验证」的条目。写在这里而
//! 不是假装已覆盖：
//!
//! 1. **`Security.framework` / `CoreFoundation` 的 FFI 只保证类型正确。**
//!    函数语义与常量（`SecCodeCopyGuestWithAttributes`、`SecCodeCheckValidity`、
//!    `SecCodeCopySigningInformation`、`kSecCodeInfoUnique`）必须在真机验证。
//! 2. **正路径未覆盖**：「真实签名的 App/daemon 连上来并通过」需要把 App 装好
//!    跑起来并让它连 helper —— 本机（开发机）没有这个 App，所以测不到。
//!    负路径（不受信对端被拒）可以在真机上用 `SecCodeCopySelf` 钉住。
//! 3. **[`INSTALLED_APP_BINARY`] 与 bundle id 是占位**：它们的最终值由 S3/S4 的
//!    `.app` 打包结果决定。打包后必须让二者与实际产物逐字对齐，否则要么把合法
//!    对端拒之门外（cdhash 不一致），要么要求串根本匹配不上任何东西。
//! 4. **`LOCAL_PEERTOKEN` 的长度校验**只在真机有意义（`getsockopt` 返回多少字节
//!    由内核决定）；这里做了运行时长度的显式校验，真机应复算一次「32 字节」。

use std::ffi::c_void;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use xt_contract::error::{ErrorBody, ErrorCode};

// ---------------------------------------------------------------------------
// 基础类型别名（CoreFoundation / Security 用不透明指针）
// ---------------------------------------------------------------------------

type CFAllocatorRef = *const c_void;
type CFDataRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFRawStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFIndex = isize;
type OSStatus = i32;
type SecCodeRef = *const c_void;
type SecStaticCodeRef = *const c_void;
type SecRequirementRef = *const c_void;
type SecCSFlags = u32;

/// `audit_token_t`：8 个 `u32`，共 32 字节。必须 `#[repr(C)]` 且无填充 ——
/// Security.framework 按 C 结构体原样读取这块内存。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuditToken {
    val: [u32; 8],
}

/// `kSecCSDefaultFlags`。
const K_SEC_CS_DEFAULT_FLAGS: SecCSFlags = 0;
/// `kSecCSSigningInformation`：`SecCodeCopySigningInformation` 取签名信息（含 cdhash）用。
const K_SEC_CS_SIGNING_INFORMATION: SecCSFlags = 1 << 1;
/// `kCFNumberSInt32Type`：`kSecGuestAttributePid` 的 value 类型。
const K_CF_NUMBER_SINT32_TYPE: i32 = 3;

/// `kCFTypeDictionaryKeyCallBacks` / `...ValueCallBacks` 是 CoreFoundation 导出的
/// **结构体变量**；我们只需要它的地址（把 `&...` 交给 `CFDictionaryCreate`），
/// 所以用零大小类型占位即可。
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
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const OpaqueCallbacks,
        value_callbacks: *const OpaqueCallbacks,
    ) -> CFDictionaryRef;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    /// 用文件系统路径造 `CFURL`（读**磁盘上**那个二进制的 cdhash 用）。
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: bool,
    ) -> CFURLRef;
    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external: bool,
    ) -> CFRawStringRef;
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        number_type: i32,
        value_ptr: *const c_void,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    /// `const CFStringRef kSecGuestAttributeAudit`：用 audit token 定位对端进程。
    static kSecGuestAttributeAudit: CFRawStringRef;
    /// `const CFStringRef kSecGuestAttributePid`：按 pid 定位对端进程（仅作
    /// ``LOCAL_PEERTOKEN`` 路径在系统上不可用时的最后回退）。
    static kSecGuestAttributePid: CFRawStringRef;

    /// `const CFStringRef kSecCodeInfoUnique`：签名信息里的 **cdhash**（`CFData`）。
    /// ad-hoc 签名同样有它，且由二进制的**实际字节**决定 —— 这是无证书身份绑定的锚。
    static kSecCodeInfoUnique: CFRawStringRef;
    /// `const CFStringRef kSecCodeInfoCdHashes`：**每个架构各一个** cdhash 的 CFArray。
    /// universal 二进制不能只比 `kSecCodeInfoUnique`（它挑的切片未必是进程正在跑的切片）。
    static kSecCodeInfoCdHashes: CFRawStringRef;

    fn SecRequirementCreateWithString(
        text: CFRawStringRef,
        flags: SecCSFlags,
        requirement: *mut SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopyGuestWithAttributes(
        guest: SecCodeRef,
        attributes: CFDictionaryRef,
        flags: SecCSFlags,
        code: *mut SecCodeRef,
    ) -> OSStatus;
    /// 用内核给的 audit token 直接建 `SecCode`。这是 `securityd` 自己用的入口，
    /// 比「自己拼 CFDictionary 再走 generic guest 查找」少一层对象组装。
    fn SecCodeCreateWithAuditToken(
        audit: *const AuditToken,
        flags: SecCSFlags,
        code: *mut SecCodeRef,
    ) -> OSStatus;
    fn SecCodeCopyStaticCode(
        code: SecCodeRef,
        flags: SecCSFlags,
        static_code: *mut SecStaticCodeRef,
    ) -> OSStatus;
    fn SecCodeCopySigningInformation(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
        flags: SecCSFlags,
        code: *mut SecStaticCodeRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(
        code: SecCodeRef,
        flags: SecCSFlags,
        requirement: SecRequirementRef,
    ) -> OSStatus;
}

// ---------------------------------------------------------------------------
// 身份与策略
// ---------------------------------------------------------------------------

/// 对端的内核身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    /// 对端 pid，**仅用于日志**，不用于授权决策（pid 复用 ⇒ TOCTOU）。
    pub pid: libc::pid_t,
}

/// 已安装的 XrayTun 组件二进制的**固定位置**。
///
/// helper 只从这里读身份，**绝不接受请求参数给的路径** —— 否则攻击者只要让
/// helper 去比对他自己那个二进制就行。
///
/// **2026-10-01 修正**：这里原来写的是 `Contents/MacOS/xt-daemon`，但打包布局把
/// `xt-daemon` 放在 **`Contents/Resources/`**（`apps/desktop/tauri.conf.json` 的
/// `bundle.resources`，发布工作流也按 `Contents/Resources/*` 验签）⇒ 文件永远不存在，
/// `policy_for` 必然落到 `RefuseService`：**helper 装了也不干活**。现按实际布局对齐。
///
/// 它仍然是**固定位置**：helper 不接受请求参数给的路径，否则攻击者只要让 helper 去比对他
/// 自己那个二进制就行。
pub const INSTALLED_APP_BINARY: &str = "/Applications/XrayTun.app/Contents/Resources/xt-daemon";

/// daemon 的 bundle id（代码签名要求串用）。**待真机**：打包时对齐。
pub const FALLBACK_DAEMON_BUNDLE_ID: &str = "com.xraytun.xt-daemon";

/// 编译期可覆盖的 bundle id：`XRAYTUN_DAEMON_ID=... cargo build`。
pub fn daemon_bundle_id() -> &'static str {
    option_env!("XRAYTUN_DAEMON_ID").unwrap_or(FALLBACK_DAEMON_BUNDLE_ID)
}

/// 对端授权策略。**没有** `AllowAny` 变体（见模块文档）。
#[derive(Debug, Clone)]
pub enum TrustPolicy {
    /// 要求对端满足给定的代码签名要求串（Apple 签发的链 + bundle id + Team ID）。
    RequireSignature { requirement: String },
    /// **不依赖证书的身份绑定**：对端进程的 cdhash 必须与 [`INSTALLED_APP_BINARY`]
    /// 的 cdhash **逐字节相同**。ad-hoc 签名同样有 cdhash，所以不需要 Team ID。
    RequireInstalledAppCdHash { app_binary: PathBuf },
    /// 没有可用的身份判据 ⇒ **拒绝一切特权操作**（fail-closed）。
    RefuseService { reason: String },
}

impl TrustPolicy {
    /// 从编译期注入的 Team ID + 磁盘上是否存在已安装组件决定策略。
    ///
    /// `None` 与 `Some("")` 一律视为「未配置」：ops 实测 `XRAYTUN_TEAM_ID=""` 会让
    /// `option_env!` 返回 `Some("")` 并「胜出」。未配置 ⇒ 走 cdhash 绑定或拒绝服务，
    /// 绝不变成「信任任何人」。
    pub fn policy_for(team: Option<&str>, app_binary: &Path) -> Self {
        match team.map(str::trim).filter(|t| !t.is_empty()) {
            Some(team) => TrustPolicy::RequireSignature { requirement: requirement_for(team) },
            None if app_binary.is_file() => {
                TrustPolicy::RequireInstalledAppCdHash { app_binary: app_binary.to_path_buf() }
            }
            None => TrustPolicy::RefuseService {
                reason: format!(
                    "既没有注入 XRAYTUN_TEAM_ID（None 或空串），也找不到已安装的组件：{}",
                    app_binary.display()
                ),
            },
        }
    }

    /// 默认策略：从构建期注入的 Team ID 出发。
    pub fn from_build_env() -> Self {
        let team = option_env!("XRAYTUN_TEAM_ID");
        TrustPolicy::policy_for(team, Path::new(INSTALLED_APP_BINARY))
    }

    /// 人读 + **可 grep 的策略标识**（进启动日志，供排障与产物断言）。
    pub fn describe(&self) -> String {
        match self {
            TrustPolicy::RequireSignature { requirement } => {
                format!("require-signature ({requirement})")
            }
            TrustPolicy::RequireInstalledAppCdHash { app_binary } => {
                format!("cdhash-binding ({})", app_binary.display())
            }
            TrustPolicy::RefuseService { reason } => format!("refuse-service ({reason})"),
        }
    }
}

/// 生成生产形状的签名要求串。
fn requirement_for(team: &str) -> String {
    format!(
        "anchor apple generic and identifier \"{id}\" and certificate leaf[subject.OU] = \"{team}\"",
        id = daemon_bundle_id(),
    )
}

// ---------------------------------------------------------------------------
// 授权入口
// ---------------------------------------------------------------------------

/// 执行授权。成功返回对端身份，失败返回 `PermissionDenied`。
///
/// 调用点在 `accept` 之后、处理任何指令之前（见 [`crate::server`]）。
pub fn authorize(fd: RawFd, policy: &TrustPolicy) -> Result<PeerIdentity, ErrorBody> {
    let identity = identify(fd)?;

    // root 对端直接放行：能当 root 的进程本来就能做任何事，校验它不是安全收益。
    // 正常部署里这条不会走到（helper 的对端是以普通用户运行的 daemon）。
    if identity.uid == 0 {
        tracing::debug!(pid = identity.pid, "对端是 root，跳过代码签名校验");
        return Ok(identity);
    }

    match policy {
        TrustPolicy::RequireSignature { requirement } => {
            verify_signature(fd, requirement).map_err(|error| {
                unauthorized(format!(
                    "对端代码签名校验未通过（uid={} pid={}）：{error}",
                    identity.uid, identity.pid
                ))
            })?;
            tracing::debug!(uid = identity.uid, pid = identity.pid, "对端代码签名校验通过");
        }
        TrustPolicy::RequireInstalledAppCdHash { app_binary } => {
            verify_installed_app_cdhash(fd, app_binary).map_err(|error| {
                unauthorized(format!(
                    "对端不是已安装的 XrayTun 组件（cdhash 绑定，uid={} pid={}）：{error}",
                    identity.uid, identity.pid
                ))
            })?;
            tracing::debug!(uid = identity.uid, pid = identity.pid, "对端 cdhash 与已安装组件相同");
        }
        TrustPolicy::RefuseService { reason } => {
            return Err(unauthorized(format!(
                "helper 没有可用的对端身份判据，拒绝服务：{reason}"
            )));
        }
    }

    Ok(identity)
}

/// 读 `getpeereid`（uid/gid）与 `LOCAL_PEERPID`（仅日志）。
fn identify(fd: RawFd) -> Result<PeerIdentity, ErrorBody> {
    if fd < 0 {
        return Err(unauthorized("对端 socket fd 非法（< 0）"));
    }
    let (uid, gid) = crate::fdpass::peer_credentials(fd)
        .map_err(|e| unauthorized(format!("getpeereid 失败：{e}")))?;
    let pid = crate::fdpass::peer_pid(fd).unwrap_or(-1);
    Ok(PeerIdentity { uid, gid, pid })
}

// ---------------------------------------------------------------------------
// 签名校验
// ---------------------------------------------------------------------------

/// 取对端进程的 `SecCode`（**调用方负责 `CFRelease`**）。
///
/// 2026-10-17 真机第一份日志：`SecCodeCopyGuestWithAttributes` 返回
/// `errSecCSBadObjectFormat (-67049)`，合法 App 也被拒。这里的策略是**先走
/// `SecCodeCreateWithAuditToken`**（securityd 自己的入口，不做 generic host 查找）
/// 再回退到旧的 CFDictionary 路径；两条都失败才按 pid 查一次。每一次失败都
/// 把 OSStatus 留在错误串里，不再把「框架调用失败」与「签名不匹配」混成一句话。
fn peer_guest(fd: RawFd) -> Result<SecCodeRef, ErrorBody> {
    let token = crate::fdpass::peer_audit_token(fd)
        .map_err(|e| unauthorized(format!("取对端 audit token 失败：{e}")))?;

    let mut attempts = Vec::new();

    match peer_guest_with_audit_token(&token) {
        Ok(guest) => return Ok(guest),
        Err(error) => attempts.push(format!("SecCodeCreateWithAuditToken：{error}")),
    }
    match peer_guest_with_audit_attributes(&token) {
        Ok(guest) => return Ok(guest),
        Err(error) => attempts.push(format!("SecCodeCopyGuestWithAttributes(audit)：{error}")),
    }

    // 最后回退：按 pid 查。只有上面两条都失败才会走到这里。
    if let Some(pid) = crate::fdpass::peer_pid(fd) {
        match peer_guest_with_pid(pid) {
            Ok(guest) => return Ok(guest),
            Err(error) => attempts.push(format!("SecCodeCopyGuestWithAttributes(pid={pid})：{error}")),
        }
    } else {
        attempts.push("LOCAL_PEERPID 也不可读".to_string());
    }

    Err(unauthorized(format!(
        "取对端 SecCode 失败（audit token 路径与 pid 回退都失败）：{}",
        attempts.join("；")
    )))
}

/// 首选路径：`SecCodeCreateWithAuditToken`。
fn peer_guest_with_audit_token(token: &[u32; 8]) -> Result<SecCodeRef, ErrorBody> {
    let audit = AuditToken { val: *token };
    let mut guest: SecCodeRef = std::ptr::null();
    // SAFETY: `audit` 在本调用期间存活；Security.framework 只读它，并写我们提供的 `guest`。
    let status = unsafe { SecCodeCreateWithAuditToken(&audit, K_SEC_CS_DEFAULT_FLAGS, &mut guest) };
    if status != 0 || guest.is_null() {
        return Err(unauthorized(format!("OSStatus {status}")));
    }
    Ok(guest)
}

/// 兼容路径：自己构造 `{kSecGuestAttributeAudit: CFData(token)}` 再调 generic API。
fn peer_guest_with_audit_attributes(token: &[u32; 8]) -> Result<SecCodeRef, ErrorBody> {
    // SAFETY: 全部是 CoreFoundation / Security 的标准对象生命周期操作。
    // `token` 在 `CFDataCreate` 返回前一直存活；创建出的 CFData / CFDictionary
    // 在下面立即 Release；返回的 SecCode 由调用方 Release。
    unsafe {
        let token_data = CFDataCreate(
            std::ptr::null(),
            token.as_ptr() as *const u8,
            std::mem::size_of_val(token) as CFIndex,
        );
        if token_data.is_null() {
            return Err(internal_error("CFDataCreate 失败"));
        }

        let keys: [*const c_void; 1] = [kSecGuestAttributeAudit];
        let values: [*const c_void; 1] = [token_data];
        let attributes = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if attributes.is_null() {
            CFRelease(token_data);
            return Err(internal_error("CFDictionaryCreate 失败"));
        }

        let mut guest: SecCodeRef = std::ptr::null();
        let status = SecCodeCopyGuestWithAttributes(
            std::ptr::null(),
            attributes,
            K_SEC_CS_DEFAULT_FLAGS,
            &mut guest,
        );
        CFRelease(attributes);
        CFRelease(token_data);
        if status != 0 || guest.is_null() {
            return Err(unauthorized(format!("OSStatus {status}")));
        }
        Ok(guest)
    }
}

/// 最后回退：按 pid 定位。只在上面的 audit token 路径失败时使用。
///
/// 这是**弱回退**：pid 有复用竞态。调用方仍会拿这个 SecCode 的 cdhash 与安装
/// 文件逐字节比较，且授权只在连接建立时发生；但没有 audit token 的不可伪造性。
/// 因此它必须留下 `warn`，不能悄悄降级。
fn peer_guest_with_pid(pid: libc::pid_t) -> Result<SecCodeRef, ErrorBody> {
    tracing::warn!(
        pid,
        "audit token 路径取不到对端 SecCode，回退到 pid 查找（弱身份绑定，仅作兼容）"
    );
    // SAFETY: 标准 CoreFoundation / Security 生命周期；数字与字典都在作用域内存活。
    unsafe {
        let number = CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_SINT32_TYPE,
            &pid as *const libc::pid_t as *const c_void,
        );
        if number.is_null() {
            return Err(internal_error("CFNumberCreate 失败"));
        }
        let keys: [*const c_void; 1] = [kSecGuestAttributePid];
        let values: [*const c_void; 1] = [number];
        let attributes = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if attributes.is_null() {
            CFRelease(number);
            return Err(internal_error("CFDictionaryCreate 失败"));
        }
        let mut guest: SecCodeRef = std::ptr::null();
        let status = SecCodeCopyGuestWithAttributes(
            std::ptr::null(),
            attributes,
            K_SEC_CS_DEFAULT_FLAGS,
            &mut guest,
        );
        CFRelease(attributes);
        CFRelease(number);
        if status != 0 || guest.is_null() {
            return Err(unauthorized(format!("OSStatus {status}")));
        }
        Ok(guest)
    }
}

fn verify_signature(fd: RawFd, requirement: &str) -> Result<(), ErrorBody> {
    let guest = peer_guest(fd)?;
    let verdict = sec_code_satisfies(guest, requirement);
    // SAFETY: `peer_guest` 返回的是需要 Release 的 Security 对象。
    unsafe { CFRelease(guest) };
    verdict
}

/// 一个已取得的 `SecCode` 是否满足要求串。失败返回**可读**原因。
fn sec_code_satisfies(code: SecCodeRef, requirement: &str) -> Result<(), ErrorBody> {
    if code.is_null() {
        return Err(internal_error("SecCode 为空（内核没有给出对端代码对象）"));
    }
    // SAFETY: 标准 CoreFoundation / Security 生命周期；创建出的 CFString /
    // SecRequirement 都在本函数作用域内 Release。
    unsafe {
        let text = cfstring(requirement);
        if text.is_null() {
            return Err(internal_error("构造 CFString 失败"));
        }
        let mut req: SecRequirementRef = std::ptr::null();
        let status = SecRequirementCreateWithString(text, K_SEC_CS_DEFAULT_FLAGS, &mut req);
        CFRelease(text);
        if status != 0 || req.is_null() {
            return Err(unauthorized(format!(
                "代码签名要求串本身非法（OSStatus {status}）：{requirement}"
            )));
        }

        let status = SecCodeCheckValidity(code, K_SEC_CS_DEFAULT_FLAGS, req);
        CFRelease(req);
        if status != 0 {
            return Err(unauthorized(format!(
                "SecCodeCheckValidity 返回 {status}（对端不是受信任的 XrayTun 构建）"
            )));
        }
    }
    Ok(())
}

/// 用 UTF-8 字节构造一个 `CFString`。失败返回 null。
fn cfstring(s: &str) -> CFRawStringRef {
    // kCFStringEncodingUTF8 = 0x08000100
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    // SAFETY: `s` 的字节在调用期间存活；`false` = 不复制外部表示。
    unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
            false,
        )
    }
}

// ---------------------------------------------------------------------------
// cdhash 身份绑定（不依赖 Developer ID 证书）
// ---------------------------------------------------------------------------

/// 取一段 `CFData` 的字节。null / 空 ⇒ `None`。
fn cfdata_bytes(data: CFDataRef) -> Option<Vec<u8>> {
    if data.is_null() {
        return None;
    }
    // SAFETY: 只读一个 CFData 的字节区间；指针在 CFData 的生命周期内有效，
    // 我们立刻 `to_vec()` 复制出来，不把裸指针带出函数。
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

/// 从一个动态 `SecCode` 取**所有架构**的 cdhash 集合。
///
/// 先把动态 `SecCode` 转成 `SecStaticCode`，再取签名信息：这是 Security.framework
/// 文档给出的对象类型（`SecCodeCopySigningInformation` 的形参是 `SecStaticCodeRef`），
/// 避免把动态对象硬塞给静态 API 的隐式转换。
fn code_cdhashes(code: SecCodeRef) -> Result<Vec<Vec<u8>>, ErrorBody> {
    if code.is_null() {
        return Err(internal_error("SecCode 为空"));
    }
    // SAFETY: `SecCodeCopyStaticCode` 只读 `code`、写我们提供的 `static_code`；
    // 返回的静态对象由我们 Release。
    let mut static_code: SecStaticCodeRef = std::ptr::null();
    let status = unsafe {
        SecCodeCopyStaticCode(code, K_SEC_CS_DEFAULT_FLAGS, &mut static_code)
    };
    if status != 0 || static_code.is_null() {
        return Err(unauthorized(format!(
            "SecCodeCopyStaticCode 失败（OSStatus {status}）"
        )));
    }
    let result = code_cdhashes_from_static(static_code);
    // SAFETY: `static_code` 是上面返回的、需要 Release 的 Security 对象。
    unsafe { CFRelease(static_code) };
    result
}

/// 从一个静态 `SecStaticCode` 取 cdhash。
fn code_cdhash_from_static(code: SecStaticCodeRef) -> Result<Vec<u8>, ErrorBody> {
    if code.is_null() {
        return Err(internal_error("SecStaticCode 为空"));
    }
    // SAFETY: `SecCodeCopySigningInformation` 写一个我们提供的有效指针，返回的
    // 字典由我们 Release；`kSecCodeInfoUnique` 是 Security.framework 导出的常量。
    unsafe {
        let mut info: CFDictionaryRef = std::ptr::null();
        let status = SecCodeCopySigningInformation(
            code,
            K_SEC_CS_SIGNING_INFORMATION,
            &mut info,
        );
        if status != 0 || info.is_null() {
            return Err(unauthorized(format!(
                "取签名信息失败（OSStatus {status}）——对端可能完全没有签名"
            )));
        }
        let unique = CFDictionaryGetValue(info, kSecCodeInfoUnique) as CFDataRef;
        let bytes = cfdata_bytes(unique);
        CFRelease(info);
        bytes.ok_or_else(|| {
            unauthorized("签名信息里没有 cdhash（kSecCodeInfoUnique）——这个二进制没有可用签名")
        })
    }
}

/// 从一个静态 `SecStaticCode` 取**所有架构**的 cdhash。
///
/// `kSecCodeInfoUnique` 只给一个 cdhash；universal 二进制在 arm64 上运行时，
/// 它未必是正在运行的那一切片。`kSecCodeInfoCdHashes` 给出每个架构的 cdhash，
/// 授权时只要**交集非空**，就说明对端运行的切片确实属于这个已签名的文件。
fn code_cdhashes_from_static(code: SecStaticCodeRef) -> Result<Vec<Vec<u8>>, ErrorBody> {
    if code.is_null() {
        return Err(internal_error("SecStaticCode 为空"));
    }
    // SAFETY: 标准 CoreFoundation / Security 生命周期；`info` 与 `unique` 都在
    // 本函数作用域内被读取，复制出字节后 Release。
    unsafe {
        let mut info: CFDictionaryRef = std::ptr::null();
        let status = SecCodeCopySigningInformation(
            code,
            K_SEC_CS_SIGNING_INFORMATION,
            &mut info,
        );
        if status != 0 || info.is_null() {
            return Err(unauthorized(format!(
                "取签名信息失败（OSStatus {status}）——对端可能完全没有签名"
            )));
        }

        let mut hashes = Vec::new();
        let unique = CFDictionaryGetValue(info, kSecCodeInfoUnique) as CFDataRef;
        if let Some(bytes) = cfdata_bytes(unique) {
            hashes.push(bytes);
        }
        let array = CFDictionaryGetValue(info, kSecCodeInfoCdHashes) as CFArrayRef;
        if !array.is_null() {
            let count = CFArrayGetCount(array);
            for index in 0..count {
                let value = CFArrayGetValueAtIndex(array, index) as CFDataRef;
                if let Some(bytes) = cfdata_bytes(value) {
                    if !hashes.contains(&bytes) {
                        hashes.push(bytes);
                    }
                }
            }
        }
        CFRelease(info);
        if hashes.is_empty() {
            return Err(unauthorized(
                "签名信息里没有 cdhash（kSecCodeInfoUnique / kSecCodeInfoCdHashes）",
            ));
        }
        Ok(hashes)
    }
}

/// 取磁盘文件的**全部架构** cdhash。
fn static_cdhashes(path: &Path) -> Result<Vec<Vec<u8>>, ErrorBody> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    // SAFETY: 标准 CoreFoundation / Security 生命周期。
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            false,
        );
        if url.is_null() {
            return Err(internal_error("CFURLCreateFromFileSystemRepresentation 失败"));
        }
        let mut code: SecStaticCodeRef = std::ptr::null();
        let status = SecStaticCodeCreateWithPath(url, K_SEC_CS_DEFAULT_FLAGS, &mut code);
        CFRelease(url);
        if status != 0 || code.is_null() {
            return Err(unauthorized(format!(
                "读不到 {} 的代码签名（OSStatus {status}）",
                path.display()
            )));
        }
        let hashes = code_cdhashes_from_static(code);
        CFRelease(code);
        hashes
    }
}

/// 取**磁盘上**某个二进制的 cdhash（单值，测试与诊断用）。
/// 取**磁盘上**某个二进制的 cdhash。
fn static_cdhash(path: &Path) -> Result<Vec<u8>, ErrorBody> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    // SAFETY: 标准 CoreFoundation / Security 生命周期；CFURL 与 SecCode 都在
    // 作用域内 Release，`bytes` 在 CFURL 创建前一直存活。
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as CFIndex,
            false,
        );
        if url.is_null() {
            return Err(internal_error("CFURLCreateFromFileSystemRepresentation 失败"));
        }
        let mut code: SecStaticCodeRef = std::ptr::null();
        let status = SecStaticCodeCreateWithPath(url, K_SEC_CS_DEFAULT_FLAGS, &mut code);
        CFRelease(url);
        if status != 0 || code.is_null() {
            return Err(unauthorized(format!(
                "读不到 {} 的代码签名（OSStatus {status}）",
                path.display()
            )));
        }
        let hash = code_cdhash_from_static(code);
        CFRelease(code);
        hash
    }
}

/// **逐字节**比较两个 cdhash。任一为空 / 长度不同 ⇒ `false`。
fn cdhashes_match(a: &[u8], b: &[u8]) -> bool {
    !a.is_empty() && a == b
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_all(hashes: &[Vec<u8>]) -> String {
    hashes.iter().map(|hash| hex(hash)).collect::<Vec<_>>().join(",")
}

/// 校验对端进程的 cdhash 与绑定二进制**至少有一个架构切片相同**。
///
/// 为什么不是只比 `kSecCodeInfoUnique`：安装文件与运行进程都可能是 universal；
/// 前者返回的 Unique 未必就是当前进程正在跑的切片。逐架构取集合求交集，
/// 既覆盖多架构，又保持「对端运行的确实是这个已签名文件里的一个切片」这个判据。
fn verify_installed_app_cdhash(fd: RawFd, app_binary: &Path) -> Result<(), ErrorBody> {
    // 先读「应该是谁」：读不到就拒绝（fail-closed），不许因为读不到而放行。
    let want = static_cdhashes(app_binary)?;
    let guest = peer_guest(fd)?;
    let got = code_cdhashes(guest);
    // SAFETY: `peer_guest` 返回的是需要 Release 的 Security 对象。
    unsafe { CFRelease(guest) };
    let got = got?;
    if !got.iter().any(|hash| want.iter().any(|want_hash| cdhashes_match(hash, want_hash))) {
        return Err(unauthorized(format!(
            "cdhash 不一致：对端 {}，已安装组件 {}（{}）",
            hex_all(&got),
            hex_all(&want),
            app_binary.display()
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 错误构造
// ---------------------------------------------------------------------------

fn unauthorized(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::PermissionDenied, message)
}

fn internal_error(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::process::Command;

    /// 真机路径的**最小复现**：一个 ad-hoc 签名的裸 Mach-O 进程，
    /// 通过真实 AF_UNIX socket 连接，另一侧用 `LOCAL_PEERTOKEN` 取 audit token，
    /// 再用 Security.framework 定位它的 `SecCode` 并把 cdhash 与磁盘文件逐字节比较。
    ///
    /// 为什么必须用 `fork + execl`：`std::process::Command` 在 macOS 上可能走
    /// `posix_spawn` 的 close-fds 语义，控制不了我们要继承的 socket fd；这里要的
    /// 正是「进程 exec 后 socket fd 仍在」这个现场。
    #[test]
    fn ad_hoc_signed_peer_is_located_by_audit_token_and_cdhash_matches() {
        let dir = std::env::temp_dir().join(format!(
            "xt-trust-peer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("创建测试目录");

        let peer = dir.join("xt-adhoc-peer");
        std::fs::copy("/bin/sleep", &peer).expect("复制 /bin/sleep");
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&peer)
            .output()
            .expect("执行 codesign");
        assert!(
            signed.status.success(),
            "ad-hoc 签名失败：stdout={} stderr={}",
            String::from_utf8_lossy(&signed.stdout),
            String::from_utf8_lossy(&signed.stderr)
        );
        let want = static_cdhashes(&peer).expect("磁盘上的 ad-hoc 签名必须能读出 cdhash");

        // socketpair：parent 端留在测试进程，child 端随 exec 继承。
        let mut sockets = [0_i32; 2];
        let rc = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr())
        };
        assert_eq!(rc, 0, "socketpair 失败：{}", std::io::Error::last_os_error());

        // 一根 CLOEXEC 的 pipe：exec 成功时内核会自动关掉写端，父进程读到 EOF，
        // 于是测试不需要 sleep/轮询就能确定「child 已经 exec 成 ad-hoc 二进制」。
        let mut notify = [0_i32; 2];
        let rc = unsafe { libc::pipe(notify.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe 失败：{}", std::io::Error::last_os_error());
        let flags = unsafe { libc::fcntl(notify[1], libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(notify[1], libc::F_SETFD, flags | libc::FD_CLOEXEC) },
            0
        );

        let c_path = CString::new(peer.as_os_str().as_bytes()).expect("peer path 不含 NUL");
        let c_arg0 = CString::new("xt-adhoc-peer").expect("argv0 不含 NUL");
        let c_arg1 = CString::new("30").expect("argv1 不含 NUL");
        let parent_socket = sockets[0];
        let child_socket = sockets[1];
        let notify_read = notify[0];
        let notify_write = notify[1];

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork 失败：{}", std::io::Error::last_os_error());
        if pid == 0 {
            // 子进程：只做 async-signal-safe 的系统调用，然后 exec。
            unsafe {
                libc::close(parent_socket);
                libc::close(notify_read);
                let _ = notify_write; // 保持打开，exec 时由 CLOEXEC 关闭
                libc::execl(
                    c_path.as_ptr(),
                    c_arg0.as_ptr(),
                    c_arg1.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                libc::_exit(127);
            }
        }

        // 父进程：关掉 child 端与 pipe 写端，等 EOF 表示 exec 已经完成。
        unsafe {
            libc::close(child_socket);
            libc::close(notify_write);
        }
        let mut byte = [0_u8; 1];
        let read = unsafe { libc::read(notify_read, byte.as_mut_ptr() as *mut std::ffi::c_void, 1) };
        unsafe { libc::close(notify_read) };
        assert_eq!(read, 0, "child 未按预期 exec（pipe read={read}）");

        let guest = peer_guest(parent_socket).expect("合法 ad-hoc 对端必须能被定位");
        let got = code_cdhashes(guest).expect("对端 cdhash 必须可读");
        unsafe { CFRelease(guest) };
        unsafe { libc::close(parent_socket) };
        assert!(
            got.iter().any(|hash| want.iter().any(|want_hash| cdhashes_match(hash, want_hash))),
            "对端运行的切片必须属于已签名文件：got={:?} want={:?}",
            got,
            want
        );

        // 清理：送 SIGKILL 并 reap；sleep 不会响应 SIGTERM 以外的信号？KILL 一定生效。
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cdhash_reader_rejects_unsigned_file() {
        let dir = std::env::temp_dir().join(format!("xt-unsigned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建测试目录");
        let path = dir.join("plain.txt");
        std::fs::write(&path, b"not a mach-o").expect("写测试文件");
        assert!(static_cdhash(&path).is_err(), "未签名文件不许读出 cdhash");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
