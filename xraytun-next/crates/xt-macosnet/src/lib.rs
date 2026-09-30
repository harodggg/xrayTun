//! xt-macosnet —— macOS 系统网络原语，只做四件事 + 快照：
//! 建/拆 utun、配地址、加/删路由、设置/还原 DNS。
//!
//! 只有特权 helper 会调用它（权限边界见 ARCHITECTURE.md §1.2）。
//! **非 macOS 平台编译成"能编译但一律返回 `Unsupported`"的桩**，
//! 这样 Linux 上的 `cargo test --workspace` 仍然是全绿的快速门。
//!
//! 接口冻结在 docs/design/MACOS-APP-PLAN.md §5。所有者：macosnet（S1）。
