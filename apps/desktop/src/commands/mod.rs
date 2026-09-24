//! Tauri 命令层：UI 能调用的全部入口。
//!
//! # 约定
//!
//! * 所有命令返回 `Result<T, String>`。`String` 是**给用户看的**中文消息，
//!   不是调试信息 —— 上游错误在 `map_err` 里已经被翻译成人话。
//! * 每个会改变状态或系统的命令，成功后都返回最新的 [`AppSnapshot`]，
//!   让 UI 一次拿全，避免「调完再查一次」产生的竞态和额外 IPC。
//! * **绝不跨 `.await` 持有 `inner` 锁**。耗时动作（起进程、改网络、跑探针）
//!   都在锁外做，做完再短暂加锁写回结果。
//!
//! # 文件划分
//!
//! 这里原来是一个 2700 行的单文件。按**职责**拆开之后，每块都能单独读懂：
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`snapshot`] | 给 UI 的完整快照，以及快照里各项（核心版本、登录项、更新、DNS）的求值 |
//! | [`settings`] | 设置读写与登录项开关 |
//! | [`core`] | 核心启停，以及看门狗（隧道存活、网络迁移、睡眠恢复、连通性检查） |
//! | [`nodes`] | 节点与订阅的增删改查、切换与失败回退 |
//! | [`latency`] | 延迟探测入口 |
//! | [`helper`] | 特权 helper 的探测、安装、重启、卸载 |
//! | [`diagnostics`] | 日志读取与脱敏、诊断信息、数据目录 |
//! | [`util`] | 跨模块共用的小工具 |
//!
//! 拆分是纯机械的：命令名、参数、错误文案、日志文案、持久化时机一律未改。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use xt_core::model::{AppSettings, Node, ProxyMode, Subscription};
use xt_core::xray;
use xt_proto::{Request, DEFAULT_SOCKET_PATH};

use crate::events;
use crate::state::{persist_settings, AppSnapshot, CoreAvailability, CoreRuntime};
use crate::AppState;

mod core;
mod diagnostics;
mod helper;
mod incident;
mod intent;
mod latency;
mod mitm;
mod nodes;
mod settings;
mod snapshot;
mod globe;
mod topology;
mod util;

pub use core::*;
pub use diagnostics::*;
pub use helper::*;
pub use incident::*;
pub use intent::*;
pub use latency::*;
pub use mitm::*;
pub use nodes::*;
pub use settings::*;
pub use snapshot::*;
pub use globe::*;
pub use topology::*;
