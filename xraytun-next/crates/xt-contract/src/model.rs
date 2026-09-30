//! 领域模型：线上（wire）形状。
//!
//! 一条硬规则：**没有数据就是 `None`，不是 0、不是空字符串、不是上一帧的旧值。**
//! 界面因此永远可以区分「知道」和「不知道」——
//! 把未知显示成 0 是本项目最不能接受的谎（用户会以为没有流量，其实只是没采样）。

use serde::{Deserialize, Serialize};

/// 节点的稳定标识。用字符串而不是自增整数：
/// 订阅刷新后索引会变，而 id 必须能跨刷新保持指向同一台服务器。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 运行模式。两种入站，一个控制面；TUN 需要特权 helper，proxy 不需要。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// 本机 SOCKS 入站（无特权，可在 CI 上端到端验证）。
    Proxy,
    /// utun 入站（macOS + 特权 helper）。
    Tun,
}

/// 连接生命周期的**阶段**。这是用户会看到的东西，所以它必须和内部状态机
/// 一一对应且可证明（见 xt-state 的转换表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

/// `Connecting` 的子阶段。存在的理由：用户需要知道「卡在哪一步」，
/// 而「一直转圈」是最容易引发不信任的界面。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectPhase {
    /// 生成配置并落盘。
    PreparingConfig,
    /// 拉起核心进程。
    StartingCore,
    /// 等核心真正可连（proxy：SOCKS 可连；tun：路由已提交）。
    AwaitingReady,
    /// 仅 TUN：接管默认路由的最后一步。
    CommittingRoutes,
}

impl ConnectPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            ConnectPhase::PreparingConfig => "preparing_config",
            ConnectPhase::StartingCore => "starting_core",
            ConnectPhase::AwaitingReady => "awaiting_ready",
            ConnectPhase::CommittingRoutes => "committing_routes",
        }
    }
}

/// 数据面进程的真实事实。全部为 `Option`：
/// 「进程还没起来」和「pid 是 0」是两件事。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatapathView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 来自 `xray version` 的真实输出，不是我们写死的常量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 核心确认可连的时刻（epoch ms）。它是「启动耗时」的唯一合法来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at_ms: Option<u64>,
}

/// 真实流量计数（来自 Xray StatsService，单位字节）。
/// `RuntimeSnapshot.stats` 为 `None` 表示**从未采样成功**：界面必须显示「未采样」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsView {
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    /// 本帧数据的采样时刻。0 不可能出现（构造时强制 > 0）。
    pub sampled_at_ms: u64,
}

/// 一次状态快照。事件 `Event::State` 携带的就是它。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionView {
    pub stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ConnectPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RunMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_since_ms: Option<u64>,
    #[serde(default)]
    pub datapath: DatapathView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsView>,
    /// 最近一次真实失败。它不会因为「后来起来了」而被替换成描述成功的话术。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<crate::error::ErrorBody>,
}

impl Default for ConnectionView {
    fn default() -> Self {
        Self {
            stage: Stage::Disconnected,
            phase: None,
            mode: None,
            node_id: None,
            connected_since_ms: None,
            datapath: DatapathView::default(),
            stats: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub ts_ms: u64,
    pub level: LogLevel,
    /// 发出日志的组件（crate/module），不是 UI 页签名。
    pub target: String,
    pub message: String,
}

/// 节点的来源。UI 需要如实展示「这条节点是订阅来的还是手填的」。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeSource {
    Subscription { id: SubscriptionId },
    Manual,
}

/// 节点视图。`endpoint` 是真实 host:port；解析不出来的节点**不会**出现在这里
/// （宁缺毋假）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeView {
    pub id: NodeId,
    pub name: String,
    /// 上游协议名（vless / vmess / trojan / ss / freedom …）。
    pub protocol: String,
    pub endpoint: String,
    pub source: NodeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeResult>,
}

/// 延迟探测结果。**恰好一个**字段为 `Some`：
/// 要么测到了真实 TTFB，要么拿到了真实失败原因；不存在「未知」这种含糊态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub node_id: NodeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::ErrorBody>,
    pub at_ms: u64,
}

impl ProbeResult {
    pub fn measured(node_id: NodeId, ttfb_ms: u32, at_ms: u64) -> Self {
        Self { node_id, ttfb_ms: Some(ttfb_ms), error: None, at_ms }
    }

    pub fn failed(node_id: NodeId, error: crate::error::ErrorBody, at_ms: u64) -> Self {
        Self { node_id, ttfb_ms: None, error: Some(error), at_ms }
    }

    /// 不变量自检：用于测试断言，不用在生产路径上代替构造。
    pub fn is_consistent(&self) -> bool {
        self.ttfb_ms.is_some() != self.error.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub severity: NoticeSeverity,
    pub code: crate::error::ErrorCode,
    pub message: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionView {
    pub id: SubscriptionId,
    /// 原样保存用户填的 URL（含 token），只在本地文件里，不进日志。
    pub url: String,
    pub node_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<crate::error::ErrorBody>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// SOCKS 监听地址（proxy 模式的入口）。
    pub socks_listen: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_node: Option<NodeId>,
    pub log_level: LogLevel,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            socks_listen: "127.0.0.1:1080".to_string(),
            selected_node: None,
            log_level: LogLevel::Info,
        }
    }
}

/// 局部更新：`None` = 不改这一项。用 patch 而不是整对象替换，
/// 是为了避免「UI 没读到的旧字段把新值覆盖回去」。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks_listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_node: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<LogLevel>,
}

/// daemon 的真实身份。UI 顶栏显示的版本、pid 都取自这里，不写死。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHello {
    pub daemon_version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<Capability>,
    pub pid: u32,
    pub started_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ProxyMode,
    TunMode,
    Stats,
    Probe,
    /// 能解析订阅原文（本地文件）并把节点列出来。**不含**远端拉取。
    Subscriptions,
    /// 能从 http/https URL 拉取订阅并刷新（本轮不宣告）。
    ///
    /// 为什么把它和 `Subscriptions` 分开：如果合成一个粗粒度能力，
    /// 界面就只能二选一——要么显示一个按不动的"刷新"入口（假控件），
    /// 要么把本地解析一起藏掉。能力粒度必须与**用户可做的动作**对齐。
    SubscriptionFetch,
}

/// 订阅主题。客户端只收到它订阅过的事件类型 —— 少发就是省电，不是省事。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    State,
    Log,
    Probe,
    Notice,
}

pub const ALL_TOPICS: [Topic; 4] = [Topic::State, Topic::Log, Topic::Probe, Topic::Notice];
