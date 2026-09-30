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
type CFDictionaryRef = *const c_void;
type CFRawStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFIndex = isize;
type OSStatus = i32;
type SecCodeRef = *const c_void;
type SecRequirementRef = *const c_void;
type SecCSFlags = u32;

/// `kSecCSDefaultFlags`。
const K_SEC_CS_DEFAULT_FLAGS: SecCSFlags = 0;

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
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    /// `const CFStringRef kSecGuestAttributeAudit`：用 audit token 定位对端进程。
    static kSecGuestAttributeAudit: CFRawStringRef;

    /// `const CFStringRef kSecCodeInfoUnique`：签名信息里的 **cdhash**（`CFData`）。
    /// ad-hoc 签名同样有它，且由二进制的**实际字节**决定 —— 这是无证书身份绑定的锚。
    static kSecCodeInfoUnique: CFRawStringRef;

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
    fn SecCodeCopySigningInformation(
        code: SecCodeRef,
        flags: SecCSFlags,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
        flags: SecCSFlags,
        code: *mut SecCodeRef,
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
/// **待真机**：最终路径要与 S3/S4 的 `.app` 打包结果对齐（见模块文档）。
pub const INSTALLED_APP_BINARY: &str = "/Applications/XrayTun.app/Contents/MacOS/xt-daemon";

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
fn peer_guest(fd: RawFd) -> Result<SecCodeRef, ErrorBody> {
    let token = crate::fdpass::peer_audit_token(fd)
        .map_err(|e| unauthorized(format!("取对端 audit token 失败：{e}")))?;

    // SAFETY: 全部是 CoreFoundation / Security 的标准对象生命周期操作。
    // `token` 是栈上 32 字节数组，在 `CFDataCreate` 返回前一直存活；
    // 创建出的 CFData / CFDictionary 在下面立即 Release；返回的 SecCode 由调用方 Release。
    unsafe {
        let token_data = CFDataCreate(
            std::ptr::null(),
            token.as_ptr() as *const u8,
            std::mem::size_of_val(&token) as CFIndex,
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
            return Err(unauthorized(format!(
                "SecCodeCopyGuestWithAttributes 失败（OSStatus {status}）"
            )));
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

/// 从一个 `SecCode` 取 **cdhash**（签名信息里的 `kSecCodeInfoUnique`）。
fn code_cdhash(code: SecCodeRef) -> Result<Vec<u8>, ErrorBody> {
    if code.is_null() {
        return Err(internal_error("SecCode 为空"));
    }
    // SAFETY: `SecCodeCopySigningInformation` 写一个我们提供的有效指针，返回的
    // 字典由我们 Release；`kSecCodeInfoUnique` 是 Security.framework 导出的常量。
    unsafe {
        let mut info: CFDictionaryRef = std::ptr::null();
        let status = SecCodeCopySigningInformation(code, K_SEC_CS_DEFAULT_FLAGS, &mut info);
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
        let mut code: SecCodeRef = std::ptr::null();
        let status = SecStaticCodeCreateWithPath(url, K_SEC_CS_DEFAULT_FLAGS, &mut code);
        CFRelease(url);
        if status != 0 || code.is_null() {
            return Err(unauthorized(format!(
                "读不到 {} 的代码签名（OSStatus {status}）",
                path.display()
            )));
        }
        let hash = code_cdhash(code);
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

/// 校验对端进程的 cdhash == 绑定二进制的 cdhash。
fn verify_installed_app_cdhash(fd: RawFd, app_binary: &Path) -> Result<(), ErrorBody> {
    // 先读「应该是谁」：读不到就拒绝（fail-closed），不许因为读不到而放行。
    let want = static_cdhash(app_binary)?;
    let guest = peer_guest(fd)?;
    let got = code_cdhash(guest);
    // SAFETY: `peer_guest` 返回的是需要 Release 的 Security 对象。
    unsafe { CFRelease(guest) };
    let got = got?;
    if !cdhashes_match(&got, &want) {
        return Err(unauthorized(format!(
            "cdhash 不一致：对端 {}，已安装组件 {}（{}）",
            hex(&got),
            hex(&want),
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
