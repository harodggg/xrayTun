//! `xt-tun` —— 系统网络配置层（当前实现仅覆盖 macOS）。
//!
//! 这一层只做四件事，且**只做这四件事**：
//!
//! 1. 创建 / 销毁 `utun` 虚拟网卡；
//! 2. 增删路由（用 `0.0.0.0/1` + `128.0.0.0/1` 覆盖默认路由，而不是删默认路由）；
//! 3. 备份 / 修改 / 还原系统 DNS；
//! 4. 把上述副作用记进**会话快照**，保证任何时刻都能完整回滚。
//!
//! 它不解析代理协议、不碰 Xray、不认识订阅 —— 这样 helper 才能以 root 运行
//! 而攻击面仍然很小（见 `docs/02-tun-and-privileges.md`）。
//!
//! # 安全约定
//!
//! 所有外部命令都通过**绝对路径 + argv 数组**调用，绝不经过 shell。
//! 这既避免 PATH 劫持（helper 以 root 运行），也避免参数注入。
//! 所有来自上层的字符串（接口名、CIDR、服务名）都必须先过 [`validate`] 的
//! 白名单校验。

pub mod error;
pub mod macos;
pub mod plan;
pub mod validate;

pub use error::{Error, Result};
pub use macos::snapshot::SessionSnapshot;
pub use plan::{build_plan, TunPlan};
pub use validate::{validate_interface_name, validate_service_name};

/// macOS 上系统工具的绝对路径。绝不依赖 `PATH`。
pub mod tools {
    pub const IFCONFIG: &str = "/sbin/ifconfig";
    pub const ROUTE: &str = "/sbin/route";
    pub const NETSTAT: &str = "/usr/sbin/netstat";
    pub const NETWORKSETUP: &str = "/usr/sbin/networksetup";
    pub const DSCACHEUTIL: &str = "/usr/bin/dscacheutil";
    pub const KILLALL: &str = "/usr/bin/killall";
    pub const SYSCTL: &str = "/usr/sbin/sysctl";
}
