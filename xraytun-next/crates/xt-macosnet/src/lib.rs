//! xt-macosnet —— macOS 系统网络原语，只做四件事 + 快照回滚：
//! 建/拆 utun、配地址、加/删路由、设置/还原 DNS。
//!
//! 只有特权 helper 会调用它（权限边界见 `docs/architecture/ARCHITECTURE.md` §1.2）。
//! **非 macOS 平台编译成"能编译但一律返回 `Unsupported`"的桩**，
//! 这样 Linux 上的 `cargo test --workspace` 仍是全绿的快速门。
//!
//! 接口冻结在 `docs/design/MACOS-APP-PLAN.md` §5。所有者：macosnet（S1）。

pub mod model;
#[cfg(any(target_os = "macos", test))]
mod validate;

#[cfg(target_os = "macos")]
mod imp;
#[cfg(not(target_os = "macos"))]
mod stub;

pub use model::{Cidr, InstalledRoute, RouteVia, Snapshot, TunRequest, TunSession};

use std::os::fd::RawFd;
use std::path::Path;

use xt_contract::error::ErrorBody;

/// 建卡 + 配地址 + 装 bypass 路由 + 落快照；此时**不碰默认路由、不改 DNS**。
pub fn tun_up(req: &TunRequest) -> Result<TunSession, ErrorBody> {
    #[cfg(target_os = "macos")]
    {
        imp::tun_up(req)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::tun_up(req)
    }
}

/// 交出 utun 的 fd（helper 把它通过 SCM_RIGHTS 发给 daemon）。
pub fn take_fd(session: &TunSession) -> Result<RawFd, ErrorBody> {
    #[cfg(target_os = "macos")]
    {
        imp::take_fd(session)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::take_fd(session)
    }
}

/// 接管：装默认路由 + 改 DNS。失败必须能全量回滚。
pub fn commit_routes(session: &TunSession) -> Result<(), ErrorBody> {
    #[cfg(target_os = "macos")]
    {
        imp::commit_routes(session)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::commit_routes(session)
    }
}

/// 还原：按快照倒序撤销（DNS → 路由 → 接口）。
pub fn tun_down(session: &TunSession) -> Result<(), ErrorBody> {
    #[cfg(target_os = "macos")]
    {
        imp::tun_down(session)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::tun_down(session)
    }
}

/// 启动时处理上一次崩溃留下的半残状态。
pub fn restore_stale(state_dir: &Path) -> Result<Option<String>, ErrorBody> {
    #[cfg(target_os = "macos")]
    {
        imp::restore_stale(state_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        stub::restore_stale(state_dir)
    }
}
