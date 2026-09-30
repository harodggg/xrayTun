//! # xt-contract —— 唯一的跨边界词汇表
//!
//! 这个 crate 里**只有数据和错误**：没有 IO、没有进程、没有 tokio、没有 `unsafe`。
//!
//! 为什么把它单独拿出来（而不是让 UI 直接对齐 Rust 内部类型）：
//!
//! 1. 跨进程边界（UI ↔ daemon）只允许存在一份词汇表。谁想加字段，改这里，
//!    编译期就会把两个消费者一起拽过来 —— 不存在「忘了改前端」。
//! 2. 它依赖最少（serde / thiserror），所以可以被任何一侧依赖而不产生环。
//!    依赖方向由 Cargo.toml 强制，不靠约定。
//! 3. `Result<_, ErrorBody>` 里的 [`ErrorCode`] 是**封闭枚举**：里面没有
//!    `Retry` / `Fallback` / `Degraded` 这样的成员。「失败不允许悄悄换一条路」
//!    这条规则因此不是注释里的纪律，而是类型层面的不可能。
//!
//! 设计取舍：这里**不放 trait**。只有一个实现者的抽象接口是仪式，不是边界；
//! 本项目真正的边界是进程边界（AF_UNIX + 本文件定义的帧），它已经被类型表达。
#![forbid(unsafe_code)]

pub mod error;
pub mod model;
pub mod protocol;

/// 线上协议版本。改它是**破坏性**变更：老 UI 连新 daemon 必须被明确拒绝，
/// 而不是「尽量兼容」——那正是我们要消除的回落。
pub const PROTOCOL_VERSION: u32 = 1;

/// 单帧上限（1 MiB）。超过即 `invalid_request`，不允许分片拼接：
/// 帧大小失控通常是 bug，不是需求。
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

pub mod prelude {
    pub use crate::error::{ErrorBody, ErrorCode};
    pub use crate::model::{
        Capability, ConnectPhase, ConnectionView, DatapathView, LogLevel, LogLine, NodeId,
        NodeSource, NodeView, Notice, NoticeSeverity, ProbeResult, RunMode, SettingsPatch,
        SettingsView, Stage, StatsView, SubscriptionId, SubscriptionView, Topic,
    };
    pub use crate::protocol::{Event, Frame, Outcome, Request, RequestId, Response};
}
