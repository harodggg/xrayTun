//! xt-state —— 连接生命周期状态机（纯函数，无 IO、无 tokio）
//!
//! 所有者：backend-1。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 这个 crate 只允许一个方向：`State --to_view--> ConnectionView`。
//! 理由：UI 上「已连接」三个字必须能被机器证明。如果视图可以由别处拼出来，
//! 迟早会出现「进程早退了但界面还写着已连接」这种假话。
//!
//! 纯函数带来的直接收益：整张转换矩阵可以穷举测试，不需要任何进程/socket/时钟。

use xt_contract::error::{conflict, internal, ErrorBody};
use xt_contract::model::{
    ConnectPhase, ConnectionView, DatapathView, NodeId, RunMode, Stage, StatsView,
};

/// 状态机状态。字段公开是因为调用方（xt-daemon）要读它来做真实动作；
/// 但迁移**只能**经过 [`begin_connect`] / [`begin_switch`] / [`apply`]：
/// 没有 `set_stage` 之类的入口，因此「已连接」不可能被谁手写出来，
/// 只能被 CoreReady 事件推出来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// 空闲。`last_error` 记录导致落回这里的**真实**失败；正常断开时为 `None`。
    Disconnected { last_error: Option<ErrorBody> },
    /// 正在连。`pid` / `ready_at_ms` 只有真实观测到才有值 —— 先填值就是假数据。
    Connecting {
        phase: ConnectPhase,
        mode: RunMode,
        node_id: NodeId,
        pid: Option<u32>,
        ready_at_ms: Option<u64>,
    },
    /// 已证明可连（proxy：SOCKS 接受连接；tun：默认路由已提交）。
    Connected {
        mode: RunMode,
        node_id: NodeId,
        pid: u32,
        /// 核心确认可连的真实时刻（CoreReady.at_ms）。它是「启动耗时」的唯一来源。
        ready_at_ms: u64,
        /// 进入 Connected 的时刻。proxy 时与 `ready_at_ms` 相同；tun 时是路由提交时刻。
        connected_since_ms: u64,
    },
    /// 正在拆。`pid` 为 `None` 表示核心其实没起来过（例如连接在 spawn 前就被取消）。
    Disconnecting {
        mode: RunMode,
        node_id: NodeId,
        pid: Option<u32>,
    },
}

impl State {
    /// 初始状态：没有任何失败记录。
    pub fn disconnected() -> Self {
        State::Disconnected { last_error: None }
    }

    pub fn stage(&self) -> Stage {
        match self {
            State::Disconnected { .. } => Stage::Disconnected,
            State::Connecting { .. } => Stage::Connecting,
            State::Connected { .. } => Stage::Connected,
            State::Disconnecting { .. } => Stage::Disconnecting,
        }
    }
}

/// 观测信号。注意这里**没有连接意图**：意图需要 mode/node_id，而信号不携带它们
/// （见 [`begin_connect`] / [`begin_switch`]）。信号描述的都是「外面真实发生了什么」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// 配置阶段推进：在 `Connecting{PreparingConfig}` 上表示配置已落盘、可以拉起核心。
    /// 在 `Disconnected` 上无意义（意图未绑定），返回 conflict。
    ConfigReady,
    /// 核心进程真实存在，pid 是操作系统给的。
    CoreStarted { pid: u32 },
    /// 数据面确认可连。proxy：SOCKS 已接受连接；tun：核心已就绪（路由尚未提交）。
    CoreReady { at_ms: u64 },
    /// 仅 tun：默认路由已提交。
    RoutesCommitted,
    /// 数据面失败/退出。`error` 必须是真实原因。它是唯一携带错误的信号，
    /// 因此配置生成失败、spawn 失败也走这条 —— 失败路径只有一条出口。
    CoreExited { error: ErrorBody },
    /// 断开事件。`Connected`/`Connecting` 上表示断开意图（进入 Disconnecting）；
    /// `Disconnecting` 上表示拆解完成（落回 Disconnected，且不带失败）。
    Disconnected,
}

/// 调用方现在必须执行的动作。状态机不做事，只把「下一步是什么」说清楚。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PrepareConfig,
    SpawnCore,
    AwaitCoreReady,
    /// 仅 tun：提交默认路由。
    CommitRoutes,
    /// 停止数据面。必须是幂等的：进程可能已经自己退出了。
    StopCore,
    /// 把新状态发出去。每次成功迁移都带它，否则界面会停在旧状态。
    PublishState,
}

/// 一次迁移的结果：新状态 + 必须执行的动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: State,
    pub actions: Vec<Action>,
}

/// 连接意图的**唯一**入口。
///
/// 为什么需要它：`Signal` 不携带 `mode`/`node_id`。若让裸 `ConfigReady` 触发
/// `Disconnected -> Connecting`，状态机就只能猜节点 —— 那等于凭空多出一条
/// 「不知道连哪」的路径，正是本项目要消除的东西。意图由调用方在收到用户
/// 连接请求时绑定一次，之后所有推进都是观测信号。
pub fn begin_connect(
    state: &State,
    mode: RunMode,
    node_id: NodeId,
) -> Result<Transition, ErrorBody> {
    match state {
        State::Disconnected { .. } => Ok(Transition {
            state: State::Connecting {
                phase: ConnectPhase::PreparingConfig,
                mode,
                node_id,
                pid: None,
                ready_at_ms: None,
            },
            actions: vec![Action::PrepareConfig, Action::PublishState],
        }),
        other => Err(conflict(format!(
            "当前阶段 {} 不接受连接意图：请先断开",
            stage_str(other.stage())
        ))),
    }
}

/// 切节点意图入口。切换 = 「停掉旧的，按新节点重新走一遍连接」。
///
/// 为什么不是「保留旧连接直到新的成功」：那等于给切节点准备了两条成功路径，
/// 失败时还能退回旧节点 —— 本项目不要这种回落。因此这里只返回一条路径的动作，
/// 且没有任何「回到旧 node_id」的动作；切过去失败就停在 Disconnected + last_error。
pub fn begin_switch(state: &State, node_id: NodeId) -> Result<Transition, ErrorBody> {
    match state {
        State::Connected { mode, .. } => Ok(Transition {
            state: State::Connecting {
                phase: ConnectPhase::PreparingConfig,
                mode: *mode,
                node_id,
                // 旧核心正在被停掉，新核心还没起来：pid 此刻必须为空。
                pid: None,
                ready_at_ms: None,
            },
            actions: vec![Action::StopCore, Action::PrepareConfig, Action::PublishState],
        }),
        State::Disconnected { .. } => {
            Err(conflict("尚未连接，请用 connect 指定节点与模式"))
        }
        other => Err(conflict(format!(
            "当前阶段 {} 不接受切节点意图",
            stage_str(other.stage())
        ))),
    }
}

/// 状态迁移。返回 `Err` 时**不产生任何新状态**（调用方原状态继续有效），
/// 且 `code` 一定是 `Conflict` —— 非法迁移是调用方的 bug，不是需要兜住的运行时状况。
pub fn apply(state: &State, signal: Signal, now_ms: u64) -> Result<Transition, ErrorBody> {
    use Action::{AwaitCoreReady, CommitRoutes, PublishState, SpawnCore, StopCore};
    use ConnectPhase::{AwaitingReady, CommittingRoutes, PreparingConfig, StartingCore};

    // 用 `&signal` 匹配：失败的兜底分支里还要拿 signal 生成错误消息。
    match (state, &signal) {
        // ---- 连接推进（每条边都只有一个前驱，不存在第二路线）----------------
        (
            State::Connecting { phase: PreparingConfig, mode, node_id, pid, ready_at_ms },
            Signal::ConfigReady,
        ) => Ok(Transition {
            state: State::Connecting {
                phase: StartingCore,
                mode: *mode,
                node_id: node_id.clone(),
                pid: *pid,
                ready_at_ms: *ready_at_ms,
            },
            actions: vec![SpawnCore, PublishState],
        }),
        (
            State::Connecting { phase: StartingCore, mode, node_id, ready_at_ms, .. },
            Signal::CoreStarted { pid },
        ) => Ok(Transition {
            state: State::Connecting {
                phase: AwaitingReady,
                mode: *mode,
                node_id: node_id.clone(),
                pid: Some(*pid),
                ready_at_ms: *ready_at_ms,
            },
            actions: vec![AwaitCoreReady, PublishState],
        }),
        (
            State::Connecting { phase: AwaitingReady, mode: RunMode::Proxy, node_id, pid, .. },
            Signal::CoreReady { at_ms },
        ) => {
            let pid = *pid;
            Ok(Transition {
                state: State::Connected {
                    mode: RunMode::Proxy,
                    node_id: node_id.clone(),
                    pid: pid.ok_or_else(pid_missing)?,
                    ready_at_ms: *at_ms,
                    connected_since_ms: *at_ms,
                },
                actions: vec![PublishState],
            })
        }
        (
            State::Connecting { phase: AwaitingReady, mode: RunMode::Tun, node_id, pid, .. },
            Signal::CoreReady { at_ms },
        ) => Ok(Transition {
            state: State::Connecting {
                phase: CommittingRoutes,
                mode: RunMode::Tun,
                node_id: node_id.clone(),
                pid: *pid,
                ready_at_ms: Some(*at_ms),
            },
            actions: vec![CommitRoutes, PublishState],
        }),
        (
            State::Connecting { phase: CommittingRoutes, mode, node_id, pid, ready_at_ms },
            Signal::RoutesCommitted,
        ) => {
            let pid = *pid;
            let ready_at_ms = *ready_at_ms;
            Ok(Transition {
                state: State::Connected {
                    mode: *mode,
                    node_id: node_id.clone(),
                    pid: pid.ok_or_else(pid_missing)?,
                    ready_at_ms: ready_at_ms.ok_or_else(ready_missing)?,
                    connected_since_ms: now_ms,
                },
                actions: vec![PublishState],
            })
        }

        // ---- 失败：所有失败都落回 Disconnected 并带上真实原因 ----------------
        (State::Connecting { phase: PreparingConfig, .. }, Signal::CoreExited { error }) => {
            // 核心从未起来，没有东西可停。
            Ok(Transition {
                state: State::Disconnected { last_error: Some(error.clone()) },
                actions: vec![PublishState],
            })
        }
        (State::Connecting { .. }, Signal::CoreExited { error })
        | (State::Disconnecting { .. }, Signal::CoreExited { error })
        | (State::Connected { .. }, Signal::CoreExited { error }) => Ok(Transition {
            state: State::Disconnected { last_error: Some(error.clone()) },
            actions: vec![StopCore, PublishState],
        }),

        // ---- 断开：先 Disconnecting，再 Disconnected -------------------------
        (State::Connected { mode, node_id, pid, .. }, Signal::Disconnected) => {
            Ok(Transition {
                state: State::Disconnecting {
                    mode: *mode,
                    node_id: node_id.clone(),
                    pid: Some(*pid),
                },
                actions: vec![StopCore, PublishState],
            })
        }
        (State::Connecting { mode, node_id, pid, .. }, Signal::Disconnected) => {
            Ok(Transition {
                state: State::Disconnecting { mode: *mode, node_id: node_id.clone(), pid: *pid },
                actions: vec![StopCore, PublishState],
            })
        }
        (State::Disconnecting { .. }, Signal::Disconnected) => Ok(Transition {
            state: State::Disconnected { last_error: None },
            actions: vec![PublishState],
        }),

        // ---- 其余组合一律非法 -------------------------------------------------
        _ => Err(conflict(format!(
            "非法转换：{} 状态不接受 {} 信号",
            stage_str(state.stage()),
            signal_str(&signal)
        ))),
    }
}

/// `State -> ConnectionView` 的唯一方向。
///
/// `stats` 直接透传：`None` 表示**从未采样成功**，界面必须显示「未采样」而不是 0。
/// `last_error` 是调用方记住的最近一次真实失败；为 `None` 时回落到状态机自己
/// 记录的那次失败（正常断开记录的是 `None`），所以失败不会被一次成功抹掉。
pub fn to_view(
    state: &State,
    stats: Option<StatsView>,
    last_error: Option<ErrorBody>,
) -> ConnectionView {
    match state {
        State::Disconnected { last_error: recorded } => ConnectionView {
            stage: Stage::Disconnected,
            phase: None,
            mode: None,
            node_id: None,
            connected_since_ms: None,
            datapath: DatapathView::default(),
            stats,
            last_error: last_error.or_else(|| recorded.clone()),
        },
        State::Connecting { phase, mode, node_id, pid, ready_at_ms } => ConnectionView {
            stage: Stage::Connecting,
            phase: Some(*phase),
            mode: Some(*mode),
            node_id: Some(node_id.clone()),
            connected_since_ms: None,
            datapath: DatapathView { pid: *pid, version: None, ready_at_ms: *ready_at_ms },
            stats,
            last_error,
        },
        State::Connected { mode, node_id, pid, ready_at_ms, connected_since_ms } => {
            ConnectionView {
                stage: Stage::Connected,
                phase: None,
                mode: Some(*mode),
                node_id: Some(node_id.clone()),
                connected_since_ms: Some(*connected_since_ms),
                datapath: DatapathView {
                    pid: Some(*pid),
                    version: None,
                    ready_at_ms: Some(*ready_at_ms),
                },
                stats,
                last_error,
            }
        }
        State::Disconnecting { mode, node_id, pid } => ConnectionView {
            stage: Stage::Disconnecting,
            phase: None,
            mode: Some(*mode),
            node_id: Some(node_id.clone()),
            connected_since_ms: None,
            datapath: DatapathView { pid: *pid, version: None, ready_at_ms: None },
            stats,
            last_error,
        },
    }
}

/// 不变量被破坏（例如没观测到 pid 就收到 CoreReady）。这种错误绝不能静默吞掉。
fn pid_missing() -> ErrorBody {
    internal("状态机不变量被破坏：核心 pid 未知却收到 CoreReady")
}

fn ready_missing() -> ErrorBody {
    internal("状态机不变量被破坏：没有 CoreReady 时刻却收到 RoutesCommitted")
}

fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::Disconnected => "disconnected",
        Stage::Connecting => "connecting",
        Stage::Connected => "connected",
        Stage::Disconnecting => "disconnecting",
    }
}

fn signal_str(signal: &Signal) -> &'static str {
    match signal {
        Signal::ConfigReady => "config_ready",
        Signal::CoreStarted { .. } => "core_started",
        Signal::CoreReady { .. } => "core_ready",
        Signal::RoutesCommitted => "routes_committed",
        Signal::CoreExited { .. } => "core_exited",
        Signal::Disconnected => "disconnected",
    }
}
