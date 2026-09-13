//! 开机自启动（macOS 登录项）。
//!
//! # 为什么用 `SMAppService` 而不是写 LaunchAgent plist
//!
//! 两条路都能实现「登录后自动启动」，但落点完全不同：
//!
//! * 写 `~/Library/LaunchAgents/*.plist`：纯文件操作，好测。但在 macOS 13+
//!   它会被归到「系统设置 → 通用 → 登录项与扩展 → **允许在后台**」里，
//!   而不是用户会去找的「**登录时打开**」。用户去「登录时打开」列表里
//!   找不到这个 App，自然认为功能没生效。
//! * `SMAppService`：系统认可的方式，出现在「登录时打开」列表里，
//!   和用户在系统设置里手动加的那一项是同一个位置。
//!
//! 项目最低支持 macOS 13.0，正好是 `SMAppService` 的引入版本
//! （`API_AVAILABLE(macos(13.0))`），所以不需要为老系统留退路。
//!
//! # 两个容易写错的细节
//!
//! 1. **ObjC 选择子是 `mainAppService`，不是 `mainApp`。**
//!    头文件里是
//!    `@property (class, readonly) SMAppService *mainAppService NS_SWIFT_NAME(mainApp)`。
//!    `mainApp` 只是 Swift 侧的名字；从 ObjC 发消息必须用 `mainAppService`，
//!    写错的结果是 unrecognized selector（抛异常或拿到 nil），
//!    而且不会有编译错误。
//! 2. **`status` 才是事实来源，不是我们的设置字段。**
//!    用户可以在系统设置里删掉这一项。所以界面上的开关要读 `status()`，
//!    而不是回显 `settings.launch_at_login` —— 否则会出现
//!    「开关是开的，但根本不会自启」。

use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;

// ServiceManagement 只提供 ObjC 接口，没有 Rust 绑定，所以直接链框架。
#[link(name = "ServiceManagement", kind = "framework")]
extern "C" {}

/// `SMAppServiceStatus`（见 `SMAppService.h` 的 `NS_ENUM`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItem {
    /// 没注册过，或者注册后被移除了。
    NotRegistered,
    /// 已注册且会在登录时启动。
    Enabled,
    /// 已注册，但**需要用户去系统设置里同意**才会生效。
    RequiresApproval,
    /// 系统找不到这个服务（通常意味着 App 不在一个稳定的路径上）。
    NotFound,
}

impl LoginItem {
    /// 界面上的开关该显示成什么。
    pub fn is_on(self) -> bool {
        matches!(self, Self::Enabled | Self::RequiresApproval)
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::NotRegistered => "未开启",
            Self::Enabled => "已开启",
            Self::RequiresApproval => "已注册，但需要在「系统设置 → 通用 → 登录项」里允许",
            // 实测：App 不在 /Applications 时 status 就是 NotFound，但
            // registerAndReturnError: 依然能成功（注册后 status 变成 Enabled）。
            // 所以这里只提醒、不断言失败 —— 写成「找不到，请先移动」会让用户
            // 以为功能坏了，而他其实什么都没做错。
            Self::NotFound => "尚未注册（把 App 放进「应用程序」目录，系统才会稳定保留这项设置）",
        }
    }

    /// 给 UI 的稳定标识。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRegistered => "not_registered",
            Self::Enabled => "enabled",
            Self::RequiresApproval => "requires_approval",
            Self::NotFound => "not_found",
        }
    }
}

/// 取 `SMAppService.mainAppService`。
fn main_app_service() -> Result<*mut AnyObject, String> {
    let Some(cls) = AnyClass::get(c"SMAppService") else {
        return Err("系统里没有 SMAppService（需要 macOS 13 及以上）".into());
    };
    // SAFETY: 选择子 `mainAppService` 已对照 SDK 头文件确认；
    // 它返回一个 autoreleased 对象，我们在函数内立即使用，不做持有。
    let svc: *mut AnyObject = unsafe { msg_send![cls, mainAppService] };
    if svc.is_null() {
        return Err("SMAppService.mainAppService 返回了 nil".into());
    }
    Ok(svc)
}

/// 读当前状态。
pub fn status() -> Result<LoginItem, String> {
    let svc = main_app_service()?;
    // SAFETY: `status` 是 readonly 的 NSInteger 属性，返回枚举值。
    let raw: isize = unsafe { msg_send![svc, status] };
    Ok(match raw {
        0 => LoginItem::NotRegistered,
        1 => LoginItem::Enabled,
        2 => LoginItem::RequiresApproval,
        3 => LoginItem::NotFound,
        // 出现没见过的值时宁可报错，也不要猜 —— 上层会把它显示给用户。
        other => return Err(format!("SMAppService.status 返回了未知值 {other}")),
    })
}

/// 注册为登录项。
pub fn enable() -> Result<LoginItem, String> {
    let svc = main_app_service()?;
    let mut err: *mut AnyObject = std::ptr::null_mut();
    // SAFETY: 选择子 `registerAndReturnError:` 已对照 SDK 头文件确认；
    // `err` 是合法的出参位置，失败时由被调用方向它写入 NSError。
    // 这里刻意不用「动态选择子 + 出参」的写法 —— objc2 的宏不接受
    // `sel: xxx, &mut err` 这种组合，只能写死选择子。
    let ok: bool = unsafe { msg_send![svc, registerAndReturnError: &mut err] };
    if !ok {
        return Err(format!("注册登录项失败：{}", describe_error(err)));
    }
    // 注册成功后**重新读一次状态**：macOS 可能要求用户去系统设置里批准，
    // 那种情况下 status 是 RequiresApproval 而不是 Enabled。
    // 界面必须能区分这两者，否则用户以为开好了、实际不会自启。
    status()
}

/// 取消登录项。
pub fn disable() -> Result<LoginItem, String> {
    let svc = main_app_service()?;
    // 本来就没注册时 unregister 会报 kSMErrorInvalidSignature / 类似错误，
    // 但那对调用方来说不是失败 —— 目标状态已经达成。
    if matches!(status()?, LoginItem::NotRegistered) {
        return Ok(LoginItem::NotRegistered);
    }
    let mut err: *mut AnyObject = std::ptr::null_mut();
    // SAFETY: 同上，选择子已对照头文件确认。
    let ok: bool = unsafe { msg_send![svc, unregisterAndReturnError: &mut err] };
    if !ok {
        return Err(format!("取消登录项失败：{}", describe_error(err)));
    }
    status()
}

/// 把登录项调成期望的状态。已经是目标状态时**什么都不做**。
///
/// 这一步不能省：`registerAndReturnError:` 对已注册的服务会返回
/// `kSMErrorAlreadyRegistered` 失败。保存设置是个高频动作，
/// 不加判断会让用户每改一次别的设置就看到一条「注册登录项失败」。
pub fn apply(desired: bool) -> Result<LoginItem, String> {
    let current = status()?;
    if desired == current.is_on() {
        return Ok(current);
    }
    if desired { enable() } else { disable() }
}

/// 打开「系统设置 → 通用 → 登录项与扩展」。
///
/// `RequiresApproval` 时用户必须去那里点一下，把我们直接送过去能省掉
/// 「在设置里翻半天找不到」。
pub fn open_system_settings() -> Result<(), String> {
    let Some(cls) = AnyClass::get(c"SMAppService") else {
        return Err("系统里没有 SMAppService".into());
    };
    // SAFETY: 类方法，无参数无返回值。返回类型必须显式标注 ——
    // 宏无法从上下文推断出 void。
    let _: () = unsafe { msg_send![cls, openSystemSettingsLoginItems] };
    Ok(())
}

/// 把 NSError 变成一句能读的话。
fn describe_error(err: *mut AnyObject) -> String {
    if err.is_null() {
        return "系统没有给出原因".into();
    }
    // SAFETY: err 非空，是 NSError；两个属性都是 readonly 的 NSString / NSInteger。
    unsafe {
        let desc: *mut AnyObject = msg_send![err, localizedDescription];
        let code: isize = msg_send![err, code];
        let text = if desc.is_null() {
            "未知错误".to_string()
        } else {
            let utf8: *const std::os::raw::c_char = msg_send![desc, UTF8String];
            if utf8.is_null() {
                "未知错误".to_string()
            } else {
                std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
            }
        };
        format!("{text}（NSError {code}）")
    }
}
