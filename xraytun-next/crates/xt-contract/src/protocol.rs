//! 协议：请求 / 响应 / 事件 / 帧。
//!
//! 形状选择的理由（这些理由会原样出现在架构文档里）：
//!
//! * **长度前缀 + JSON**，不用 gRPC/protobuf：唯一的对端是本机 UI 与本机 CLI，
//!   没有跨语言 schema 演进需求；而 JSON 能被 `nc`、日志、用户直接看懂 ——
//!   可排查性 > 编解码性能。代价是每帧多几百字节，量级无关紧要。
//! * **命令受理与结果分离**：`connect` 只是「受理」（返回 [`Response::Accepted`]），
//!   终态通过 [`Event::State`] 到达。这样客户端**不需要轮询**，
//!   也就没有「等一会儿再看看」这类代码存在的理由。
//! * **事件带自增 `seq`**：客户端能发现丢帧，而不是默默漂移。

use serde::{Deserialize, Serialize};

use crate::error::ErrorBody;
use crate::model::{
    ConnectionView, DaemonHello, LogLine, NodeId, NodeView, Notice, ProbeResult, RunMode,
    SettingsPatch, SettingsView, SubscriptionId, SubscriptionView, Topic,
};

pub type RequestId = u64;
pub type EventSeq = u64;

/// 客户端 → daemon。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 必须是一台连接上的第一个请求；版本不符即 [`crate::error::ErrorCode::InvalidRequest`]。
    Hello { client_version: String, protocol_version: u32 },
    /// 订阅事件。之后本连接只推送所列主题。
    Subscribe { topics: Vec<Topic> },
    Status,
    /// 受理连接意图；结果见 `Event::State`。
    Connect { node_id: NodeId, mode: RunMode },
    Disconnect,
    /// 切换节点 = 新意图；**不回落**：切过去失败就停在失败，并如实报错。
    SwitchNode { node_id: NodeId },
    ListNodes,
    /// 发起探测；每条结果以 `Event::Probe` 回来。
    ProbeNodes { node_ids: Vec<NodeId> },
    GetSettings,
    PatchSettings { patch: SettingsPatch },
    ListSubscriptions,
    AddSubscription { url: String },
    RefreshSubscription { id: SubscriptionId },
    TailLogs { lines: u32 },
}

/// daemon → 客户端（对请求的应答）。
///
/// **形状约束（踩过的坑）**：这里用 `#[serde(tag = "result")]`（内部 tag）。
/// serde 的内部 tag 只支持 *结构体变体* 与 *单字段 struct/map 变体*；
/// 写成 `Nodes(Vec<NodeView>)` 这种「单字段包序列」时，序列化会**在运行时**报
/// `cannot serialize tagged newtype variant ... containing a sequence` —— 编译期看不出来，
/// 只有真的发一次 `list_nodes` 才会炸。
/// 所以凡是携带集合的应答一律写成结构体变体。回归守卫：`tests/wire.rs` 对每个变体做 round-trip。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Hello(DaemonHello),
    Subscribed { topics: Vec<Topic> },
    Status(ConnectionView),
    Nodes { nodes: Vec<NodeView> },
    Settings(SettingsView),
    Subscriptions { subscriptions: Vec<SubscriptionView> },
    Logs { logs: Vec<LogLine> },
    /// 意图已受理：终态一定会在事件流里出现（成功或失败都出现，不会石沉大海）。
    Accepted,
    /// 同步完成的成功（没有需要等待的结果）。
    Ok,
}

/// 应答的两种结局。用显式 tag 而不是 `Result` 的默认表示，
/// 是为了让 TypeScript 侧的类型窄化不需要任何技巧。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { response: Response },
    Error { error: ErrorBody },
}

impl Outcome {
    pub fn ok(response: Response) -> Self {
        Outcome::Ok { response }
    }

    pub fn error(error: ErrorBody) -> Self {
        Outcome::Error { error }
    }
}

/// daemon → 客户端（主动推送）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    State { view: ConnectionView },
    Log { line: LogLine },
    Probe { result: ProbeResult },
    Notice { notice: Notice },
}

impl Event {
    pub const fn topic(&self) -> Topic {
        match self {
            Event::State { .. } => Topic::State,
            Event::Log { .. } => Topic::Log,
            Event::Probe { .. } => Topic::Probe,
            Event::Notice { .. } => Topic::Notice,
        }
    }
}

/// 一根连接上跑的三种帧。`kind` 是判别式，长度前缀由 xt-ipc 负责。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Request { id: RequestId, request: Request },
    Response { id: RequestId, outcome: Outcome },
    Event { seq: EventSeq, event: Event },
}

impl Frame {
    pub fn request(id: RequestId, request: Request) -> Self {
        Frame::Request { id, request }
    }

    pub fn response(id: RequestId, outcome: Outcome) -> Self {
        Frame::Response { id, outcome }
    }

    pub fn event(seq: EventSeq, event: Event) -> Self {
        Frame::Event { seq, event }
    }
}
