//! 非 macOS 平台的桩：**能编译、但一律返回 `Unsupported`**。
//!
//! 这样 Linux 上的 `cargo test --workspace` 仍是一条全绿的快速门，
//! 而 macOS 的真实原语在 `imp` 里，只有 `--target aarch64-apple-darwin`
//! 才能类型检查到。

use std::path::Path;

use xt_contract::error::{unsupported, ErrorBody};

use crate::model::{TunRequest, TunSession};

pub fn tun_up(_req: &TunRequest) -> Result<TunSession, ErrorBody> {
    Err(unsupported("TUN 模式只在 macOS 上可用"))
}

pub fn take_fd(_session: &TunSession) -> Result<std::os::fd::RawFd, ErrorBody> {
    Err(unsupported("TUN 模式只在 macOS 上可用"))
}

pub fn commit_routes(_session: &TunSession) -> Result<(), ErrorBody> {
    Err(unsupported("TUN 模式只在 macOS 上可用"))
}

pub fn tun_down(_session: &TunSession) -> Result<(), ErrorBody> {
    Err(unsupported("TUN 模式只在 macOS 上可用"))
}

pub fn restore_stale(_state_dir: &Path) -> Result<Option<String>, ErrorBody> {
    Err(unsupported("TUN 模式只在 macOS 上可用"))
}
