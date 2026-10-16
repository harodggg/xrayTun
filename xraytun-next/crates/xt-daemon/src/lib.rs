//! xt-daemon —— 控制面组合根：把领域 crate 装配成一个能对外提供 IPC 的进程。
//!
//! # 为什么 daemon 是独立进程
//!
//! UI 崩溃/重载不牵连隧道；业务逻辑可以脱离 WebView 在 CI 里跑真实验证；
//! CLI 与 UI 走同一份契约；崩溃被限制在一个进程里。代价是多一次 IPC 往返 ——
//! 只有当收益大于代价才引入进程边界，本轮只有这一处。
//!
//! # 组合根的职责边界
//!
//! 这个 crate **不产生业务判断**：状态迁移只经过 `xt-state`，节点目录只经过
//! `xt-nodes`，配置只经过 `xt-xrayconf`，进程生命周期只经过 `xt-datapath`，
//! 字节数只来自 `xt-stats`。daemon 只做三件事：**装配、把真实事件转成迁移、
//! 把迁移结果发布给订阅者**。
//!
//! # 无等待在这里的落点
//!
//! * 连接推进 = 状态机信号 + 真实事件（配置落盘、spawn、SOCKS 可连、进程退出）；
//! * `tokio::time::timeout` 只出现在「失败上限」的位置（就绪、停止、单次采样），
//!   没有一处用它当轮询周期；
//! * 统计采样由消费者驱动：只有 `Status` 请求才采样，100ms 内复用上一次样本
//!   （那是缓存闸门，不是定时器）。

pub mod flow;
mod helper;
mod subs;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use xt_bus::{Bus, EventStream};
use xt_contract::error::{bad_request, internal, not_found, ErrorBody, ErrorCode};
use xt_contract::model::{
    Capability, ConnectionView, DaemonHello, LogLevel, LogLine, NodeId, NodeSource, NodeView,
    Notice, NoticeSeverity, RunMode, SettingsView, StatsView, SubscriptionId, SubscriptionView,
};
use xt_contract::protocol::{Event, Frame, Outcome, Request, Response};
use xt_ipc::{Connection, Server};
use xt_nodes::Catalog;
use xt_settings::Settings;
use xt_state::State;
use xt_stats::StatsClient;

/// 最近日志环形缓冲容量。`TailLogs` 从它取历史，取不到历史就报空 —— 不编。
const LOG_RING_CAPACITY: usize = 500;

/// 统计采样的缓存闸门。100ms 内的重复请求直接回上一样本：
/// 它是**闸门**，不是节拍 —— 没有定时器，没人看就不采样。
const STATS_GATE: Duration = Duration::from_millis(100);

/// 停止核心的失败上限（转发给 xt-datapath 的 stop）。
pub(crate) const CORE_STOP_DEADLINE: Duration = Duration::from_secs(8);

/// 探测用的 socks 入站端口起点（每个节点 +1）。
pub(crate) const PROBE_BASE_PORT: u16 = 21000;

/// 启动一个 daemon 实例所需的全部真实输入。
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
    pub xray_bin: PathBuf,
    /// 只在 `settings.json` 不存在时作为初始值；存在时以文件为准。
    pub log_level: LogLevel,
    /// 本地订阅原文（base64 / URI 列表 / clash yaml 之一）。
    pub subscription_file: PathBuf,
    /// 探测靶点。默认联网靶点；E2E/离线时改成环回地址。
    pub probe_url: String,
}

impl DaemonConfig {
    pub fn settings_path(&self) -> PathBuf {
        self.state_dir.join("settings.json")
    }
    pub fn active_config_path(&self) -> PathBuf {
        self.state_dir.join("xray.json")
    }
    pub fn probe_config_path(&self) -> PathBuf {
        self.state_dir.join("probe.json")
    }
    /// 手工导入的节点（`Request::ImportNodes` 的落盘位置）。
    ///
    /// 与订阅文件分开：订阅是「机场给的正文」，手工导入是「用户自己粘的」，
    /// 两者在目录里是**两个来源**（`NodeSource::Subscription` / `NodeSource::Manual`），
    /// 一个坏了不该牵连另一个。
    pub fn imported_nodes_path(&self) -> PathBuf {
        self.state_dir.join("imported-nodes.json")
    }
    /// 订阅状态（**含完整 URL 的令牌**，落盘权限 0600，见 `subs::save`）。
    pub fn subscriptions_path(&self) -> PathBuf {
        self.state_dir.join("subscriptions.json")
    }
}

/// daemon 句柄。
pub struct Daemon {
    shared: Arc<Shared>,
}

/// 进程级共享状态。所有字段都是「真实观测」的容器，没有任何派生缓存
/// （唯一例外是 stats 的 100ms 闸门与日志环形缓冲，它们都带真实时间戳）。
pub(crate) struct Shared {
    pub(crate) config: DaemonConfig,
    bus: Bus,
    state: Mutex<State>,
    catalog: Mutex<Catalog>,
    settings: Mutex<Settings>,
    logs: Mutex<VecDeque<LogLine>>,
    core: Mutex<Option<flow::CoreSession>>,
    /// TUN 会话的 helper 客户端 + 会话引用（断开时调 helper.tun_down）。
    tun: Mutex<Option<flow::TunSession>>,
    stats: Mutex<Option<Arc<StatsClient>>>,
    /// 上一次成功采样 + 采样时刻。未成功采样过就是 `None`。
    stats_gate: Mutex<Option<(StatsView, Instant)>>,
    datapath_version: Mutex<Option<String>>,
    /// 连接/断开/切节点串行化：同一时刻只允许一个数据面意图在跑。
    op_lock: Mutex<()>,
    /// 探测独立串行化：它用自己临时实例，不该被主连接状态挡住。
    probe_lock: Mutex<()>,
    subscriptions: Mutex<Vec<subs::SubscriptionRecord>>,
    started_at_ms: u64,
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .max(1)
}

fn notice_level(severity: NoticeSeverity) -> LogLevel {
    match severity {
        NoticeSeverity::Info => LogLevel::Info,
        NoticeSeverity::Warning => LogLevel::Warn,
        NoticeSeverity::Error => LogLevel::Error,
    }
}

impl Shared {
    // ---------------------------------------------------------------- 基础读取

    pub(crate) async fn snapshot_state(&self) -> State {
        self.state.lock().await.clone()
    }

    pub(crate) async fn settings_view(&self) -> SettingsView {
        self.settings.lock().await.view()
    }

    pub(crate) async fn list_nodes(&self) -> Vec<NodeView> {
        self.catalog.lock().await.list()
    }

    pub(crate) async fn outbound_spec(
        &self,
        id: &NodeId,
    ) -> Result<xt_xrayconf::OutboundSpec, ErrorBody> {
        self.catalog.lock().await.outbound_spec(id)
    }

    /// 选择节点：只记录 + 落盘，不预判可用性（可用性是连接时的事实）。
    pub(crate) async fn select_node(&self, id: &NodeId) -> Result<NodeView, ErrorBody> {
        let view = {
            let mut catalog = self.catalog.lock().await;
            catalog.select(id)?;
            catalog
                .list()
                .into_iter()
                .find(|view| view.id == *id)
                .ok_or_else(|| internal("刚选择成功的节点却不在目录里"))?
        };
        let mut settings = self.settings.lock().await;
        settings.selected_node = Some(id.clone());
        xt_settings::save(&self.config.settings_path(), &settings)?;
        Ok(view)
    }

    // ------------------------------------------------------------------ 日志

    pub(crate) async fn push_log(&self, line: LogLine) {
        {
            let mut logs = self.logs.lock().await;
            if logs.len() == LOG_RING_CAPACITY {
                logs.pop_front();
            }
            logs.push_back(line.clone());
        }
        self.bus.publish(Event::Log { line });
    }

    pub(crate) async fn log_daemon(&self, level: LogLevel, message: impl Into<String>) {
        self.push_log(LogLine {
            ts_ms: now_ms(),
            level,
            target: "xt-daemon".to_string(),
            message: message.into(),
        })
        .await;
    }

    pub(crate) async fn emit_notice(
        &self,
        severity: NoticeSeverity,
        code: ErrorCode,
        message: impl Into<String>,
    ) {
        let message = message.into();
        let at_ms = now_ms();
        // Notice 与日志是两件事（前者给用户，后者给排查），但一条通知也应该
        // 能在 TailLogs 里找到，否则「刚才发生了什么」只能靠猜。
        self.push_log(LogLine {
            ts_ms: at_ms,
            level: notice_level(severity),
            target: "xt-daemon".to_string(),
            message: message.clone(),
        })
        .await;
        self.bus.publish(Event::Notice { notice: Notice { severity, code, message, at_ms } });
    }

    // ------------------------------------------------------------------ 视图

    async fn datapath_version(&self) -> Option<String> {
        self.datapath_version.lock().await.clone()
    }

    /// 用**最近一次成功样本**构造视图（不触发采样）。
    ///
    /// `StatsView` 自带 `sampled_at_ms`，所以「这是什么时候的数字」是明确的；
    /// 从未采样成功则为 `None`，界面显示「未采样」而不是 0（I3）。
    pub(crate) async fn publish_view(&self) {
        let stats = self.stats_gate.lock().await.as_ref().map(|(v, _)| *v);
        self.publish_view_with(stats).await;
    }

    pub(crate) async fn publish_view_with(&self, stats: Option<StatsView>) {
        let state = self.snapshot_state().await;
        let mut view = xt_state::to_view(&state, stats, None);
        // 状态机不认识版本号（它不该认识）；版本是「谁看见了核心才能填」的观测，
        // 因此由 daemon 在这里补上，而不是凭空在状态机里编一个。
        view.datapath.version = self.datapath_version().await;
        self.bus.set_connection(view.clone());
        self.bus.publish(Event::State { view });
    }

    /// 状态迁移的唯一提交点：更新状态 → 立即发布。
    ///
    /// `Action::StopCore` 不在这里执行：只有流程自己知道当前核心会话在哪儿、
    /// 以及停它是否需要等待；在这里「顺手」停会让两个地方都能停同一个进程。
    pub(crate) async fn commit(&self, transition: xt_state::Transition) {
        *self.state.lock().await = transition.state;
        self.publish_view().await;
    }

    /// 意图在执行过程中才发现不合法（例如并发意图把状态改了）。
    /// 已经回过 `Accepted` 的意图**必须**有一个终态事件，否则就是石沉大海。
    pub(crate) async fn emit_intent_error(&self, error: ErrorBody) {
        self.emit_notice(NoticeSeverity::Error, error.code, error.message.clone()).await;
        self.publish_view().await;
    }

    // ------------------------------------------------------------------ 统计

    /// 丢弃当前会话的统计状态（连接被停掉、或新会话还没开始时调用）。
    pub(crate) async fn clear_stats(&self) {
        *self.stats.lock().await = None;
        *self.stats_gate.lock().await = None;
    }

    // ------------------------------------------------------- 流程用的写入口
    //
    // 这些是「真实观测」的写入点：只有拿到真值的地方（spawn 之后、采样成功之后）
    // 才会调用它们。没有「先填个默认值再覆盖」的路径。

    pub(crate) async fn set_datapath_version(&self, version: Option<String>) {
        *self.datapath_version.lock().await = version;
    }

    pub(crate) async fn set_stats_client(&self, client: Option<Arc<StatsClient>>) {
        *self.stats.lock().await = client;
    }

    pub(crate) async fn install_core(&self, session: flow::CoreSession) {
        *self.core.lock().await = Some(session);
    }

    pub(crate) async fn take_core(&self) -> Option<flow::CoreSession> {
        self.core.lock().await.take()
    }

    pub(crate) fn publish_event(&self, event: Event) {
        self.bus.publish(event);
    }

    /// 消费者驱动的采样：`Status` 请求调它；100ms 内重复请求回上一样本。
    ///
    /// 传输层只在这里被用、不在别处被建：`StatsClient` 在 `Connected` 之后建立，
    /// 而 `Connected` 的前提是 **socks 与 api 两个端口都已接受连接**
    /// （见 xt-datapath 的 `required_addrs`），所以这里不会撞上「api 还没监听」。
    /// 采样失败就是如实 `None`，不重试、不回落。
    pub(crate) async fn sample_stats(&self) -> Option<StatsView> {
        let client = self.stats.lock().await.clone();
        let client = client?;
        {
            let gate = self.stats_gate.lock().await;
            if let Some((view, at)) = gate.as_ref() {
                if at.elapsed() < STATS_GATE {
                    return Some(*view);
                }
            }
        }
        match client.sample().await {
            Ok(view) => {
                *self.stats_gate.lock().await = Some((view, Instant::now()));
                Some(view)
            }
            Err(error) => {
                // 采样失败就是「未采样」—— 绝不把上一次的数字当作现在。
                tracing::warn!(error = %error, "统计采样失败，如实上报未采样");
                None
            }
        }
    }

    pub(crate) fn hello(&self) -> DaemonHello {
        DaemonHello {
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: xt_contract::PROTOCOL_VERSION,
            // 能力表是**唯一**事实来源：没实现的能力不宣告，界面因此不会
            // 出现按不动的按钮。
            //
            // TunMode 为什么现在宣告：用户 2026-10-01 裁决（T2）—— 本版就打开 TUN 通路，
            // 不再等 S6 真机验收。**宣告的是「本版本提供这条通路」（代码事实），
            // 不是「TUN 已经能接管流量」**：真机验收（helper 安装/回滚、utun 接管、
            // 路由与 DNS 提交）仍然没有证据，所以界面与 release notes 一律不许写成「已可用」。
            // 订阅拉取（SubscriptionFetch）本轮宣告：实现是 `/usr/bin/curl`（零新增依赖），
            // 端到端行为由 `tests/e2e_real_xray.rs` 对着本地 HTTP 服务验证 —— 不是「宣称」。
            capabilities: vec![
                // `ProxyMode` 仍然宣告，但**产品面不提供它**（用户 2026-10-16：只做 TUN）：
                // daemon 留着这条 SOCKS 数据面，是因为 CI 只能用它证明「真有字节穿过隧道」
                // （TUN 要 root/helper）。界面没有模式选择器，CLI 的 connect 也固定发 tun。
                Capability::ProxyMode,
                Capability::TunMode,
                Capability::Stats,
                Capability::Probe,
                Capability::Subscriptions,
                // 手工导入（只做本地解析）。**为什么必须宣告**：界面靠能力表决定要不要
                // 渲染「导入」入口 —— 没有它，真人打开 App 只能看到「暂无节点」而无处可贴。
                Capability::NodeImport,
                // 远端订阅拉取（`/usr/bin/curl`，零新增依赖）。与 `NodeImport` 分开宣告：
                // 前者联网、后者只本地解析 —— 界面上的两个入口与门控也随之不同。
                Capability::SubscriptionFetch,
            ],
            pid: std::process::id(),
            started_at_ms: self.started_at_ms,
        }
    }

    async fn status_view(&self) -> ConnectionView {
        let stats = self.sample_stats().await;
        let state = self.snapshot_state().await;
        let mut view = xt_state::to_view(&state, stats, None);
        view.datapath.version = self.datapath_version().await;
        self.bus.set_connection(view.clone());
        self.bus.publish(Event::State { view: view.clone() });
        view
    }

    async fn tail_logs(&self, lines: u32) -> Vec<LogLine> {
        let logs = self.logs.lock().await;
        let want = (lines as usize).min(logs.len());
        logs.iter().skip(logs.len() - want).cloned().collect()
    }

    /// 导入节点：解析用户粘贴的订阅原文 / 分享链接 → 与已导入的合并（按 id 去重）→
    /// 原子落盘 → 替换目录里的 `Manual` 来源。
    ///
    /// 只做**本地解析**，不联网（远端拉取是 `SubscriptionFetch`，见 `add_subscription`）。
    /// 失败原样上报（`InvalidRequest` / `Io`），**绝不静默导入 0 个**。
    async fn import_nodes(&self, payload: String) -> Outcome {
        let parsed = match xt_subs::parse(&payload) {
            Ok(subscription) => subscription,
            Err(error) => return Outcome::error(error),
        };
        let path = self.config.imported_nodes_path();
        let (merged, added) = merge_imported(read_imported_nodes(&path).await, parsed.nodes);
        if let Err(error) = save_imported_nodes(&path, &merged).await {
            return Outcome::error(error);
        }
        let total = merged.len();
        let nodes: Vec<xt_subs::ParsedNode> =
            merged.into_iter().map(xt_subs::ParsedNode::from).collect();
        self.catalog.lock().await.replace_source(NodeSource::Manual, nodes);
        self.log_daemon(
            LogLevel::Info,
            format!("导入节点：本次新增 {added} 个，手工来源共 {total} 个"),
        )
        .await;
        Outcome::ok(Response::Ok)
    }

    /// 新增订阅：校验 URL → 拉取 → 解析 → 落盘 → 进目录。
    ///
    /// 任一步失败都**原样上报**（`InvalidRequest` / `Io` / 解析错误），
    /// 绝不静默存一条空订阅 —— 那会让用户以为「加上了」。
    async fn add_subscription(&self, raw_url: String) -> Outcome {
        let url = match subs::validate_url(&raw_url) {
            Ok(url) => url,
            Err(error) => return Outcome::error(error),
        };
        let body = match subs::fetch_body(&url).await {
            Ok(body) => body,
            Err(error) => return Outcome::error(error),
        };
        let parsed = match xt_subs::parse(&body) {
            Ok(parsed) => parsed,
            Err(error) => return Outcome::error(error),
        };
        let node_count = parsed.nodes.len();
        let path = self.config.subscriptions_path();
        {
            let mut records = self.subscriptions.lock().await;
            let id = subs::next_id(records.as_slice());
            let mut next = records.clone();
            next.push(subs::SubscriptionRecord {
                id: id.clone(),
                url: url.clone(),
                fetched_at_ms: Some(now_ms()),
                nodes: parsed.nodes.iter().map(StoredNode::from).collect(),
                last_error: None,
            });
            if let Err(error) = subs::save(&path, &next) {
                return Outcome::error(error);
            }
            *records = next;
            drop(records);
            self.catalog.lock().await.replace_source(
                NodeSource::Subscription { id: SubscriptionId::new(id) },
                parsed.nodes,
            );
        }
        self.log_daemon(
            LogLevel::Info,
            // 只记 host：订阅 URL 里通常带账号令牌。
            format!("新增订阅 {}：{node_count} 个节点", subs::redact_url(&url)),
        )
        .await;
        Outcome::ok(Response::Ok)
    }

    /// 刷新一条订阅：重新拉取 → 替换它的节点。
    ///
    /// 失败时**既存 `last_error` 也原样上报**：存下来是为了下次打开界面还能看到原因，
    /// 上报是为了这次点击立刻有反馈。
    async fn refresh_subscription(&self, id: SubscriptionId) -> Outcome {
        if id.as_str() == "local" {
            return Outcome::error(bad_request(
                "本地文件订阅不受「刷新」管理：它每次启动都由 --subscription-file 重新读取",
            ));
        }
        // 先取出 URL 就放开锁：拉取最长 20 秒，不能把 `ListSubscriptions` 一起堵住。
        let url = {
            let records = self.subscriptions.lock().await;
            match records.iter().find(|record| record.id == id.as_str()) {
                Some(record) => record.url.clone(),
                None => return Outcome::error(not_found(format!("订阅 {id} 不存在"))),
            }
        };
        let path = self.config.subscriptions_path();
        let body = match subs::fetch_body(&url).await {
            Ok(body) => body,
            Err(error) => return self.record_subscription_error(&path, &id, error).await,
        };
        let parsed = match xt_subs::parse(&body) {
            Ok(parsed) => parsed,
            Err(error) => return self.record_subscription_error(&path, &id, error).await,
        };
        let node_count = parsed.nodes.len();
        {
            let mut records = self.subscriptions.lock().await;
            let Some(index) = records.iter().position(|record| record.id == id.as_str()) else {
                return Outcome::error(not_found(format!("订阅 {id} 不存在")));
            };
            // clone-swap：**先落盘、写成功后**才改内存。反过来（先改内存）一旦写盘失败，
            // 内存说「刷新成功、有 N 个节点」、磁盘还是旧值，而下面的目录更新被跳过 ——
            // 界面与节点列表立刻互相矛盾，重启后磁盘那份又赢。`add_subscription`
            // 与 `record_subscription_error` 用的都是这个写法。
            let mut next = records.clone();
            next[index].nodes = parsed.nodes.iter().map(StoredNode::from).collect();
            next[index].fetched_at_ms = Some(now_ms());
            next[index].last_error = None;
            // `save` 是同步的、几 KiB，拿锁期间不跨 await。
            if let Err(error) = subs::save(&path, next.as_slice()) {
                return Outcome::error(error);
            }
            *records = next;
        }
        self.catalog.lock().await.replace_source(
            NodeSource::Subscription { id: id.clone() },
            parsed.nodes,
        );
        self.log_daemon(
            LogLevel::Info,
            format!("刷新订阅 {}：{node_count} 个节点", subs::redact_url(&url)),
        )
        .await;
        Outcome::ok(Response::Ok)
    }

    /// 把一条订阅标记成失败（存 `last_error`），并**原样**把错误交给调用方。
    async fn record_subscription_error(
        &self,
        path: &Path,
        id: &SubscriptionId,
        error: ErrorBody,
    ) -> Outcome {
        {
            let mut records = self.subscriptions.lock().await;
            if let Some(index) = records.iter().position(|record| record.id == id.as_str()) {
                records[index].last_error = Some(error.clone());
                let next = records.clone();
                if let Err(save_error) = subs::save(path, &next) {
                    return Outcome::error(save_error);
                }
                *records = next;
            }
        }
        Outcome::error(error)
    }

    // -------------------------------------------------------------- 意图受理

    async fn begin_connect_intent(self: &Arc<Self>, node_id: NodeId, mode: RunMode) -> Outcome {
        // 2026-10-01（T2）：这里原来在真机验收（S6）前拒绝一切非 proxy 的意图。
        // 用户裁决本版就打开 TUN，所以那道早退已删除 —— 受理后由 flow::connect_tun_flow
        // 走 helper.TunUp → TakeTunFd → spawn xray → 就绪 → CommitRoutes；失败一律以真实
        // ErrorBody 作为终态上报（helper 没装 / socket 连不上 / 路由提交失败各有自己的错误），
        // 不做任何替代路径。
        let exists = { self.catalog.lock().await.get(&node_id).is_some() };
        if !exists {
            return Outcome::error(xt_contract::error::not_found(format!(
                "节点 {node_id} 不在当前目录里"
            )));
        }
        // 状态机先判一次：不合法的意图立刻回错误，而不是回 Accepted 之后再报。
        let state = self.snapshot_state().await;
        if let Err(error) = xt_state::begin_connect(&state, mode, node_id.clone()) {
            return Outcome::error(error);
        }
        if let Err(error) = self.select_node(&node_id).await {
            return Outcome::error(error);
        }
        let shared = Arc::clone(self);
        tokio::spawn(async move { shared.connect_flow(node_id, flow::Intent::Connect(mode)).await });
        Outcome::ok(Response::Accepted)
    }

    async fn begin_switch_intent(self: &Arc<Self>, node_id: NodeId) -> Outcome {
        let exists = { self.catalog.lock().await.get(&node_id).is_some() };
        if !exists {
            return Outcome::error(xt_contract::error::not_found(format!(
                "节点 {node_id} 不在当前目录里"
            )));
        }
        let state = self.snapshot_state().await;
        // 「选择就用」一条路径：未连接时切节点 = 连接；已连接时 = 停旧起新。
        let check = match state.stage() {
            xt_contract::model::Stage::Disconnected => {
                xt_state::begin_connect(&state, RunMode::Proxy, node_id.clone())
            }
            _ => xt_state::begin_switch(&state, node_id.clone()),
        };
        if let Err(error) = check {
            return Outcome::error(error);
        }
        if let Err(error) = self.select_node(&node_id).await {
            return Outcome::error(error);
        }
        let shared = Arc::clone(self);
        tokio::spawn(async move { shared.connect_flow(node_id, flow::Intent::Switch).await });
        Outcome::ok(Response::Accepted)
    }

    fn begin_disconnect_intent(self: &Arc<Self>) -> Outcome {
        let shared = Arc::clone(self);
        tokio::spawn(async move { shared.disconnect_flow().await });
        Outcome::ok(Response::Accepted)
    }

    fn begin_probe_intent(self: &Arc<Self>, node_ids: Vec<NodeId>) -> Outcome {
        let shared = Arc::clone(self);
        tokio::spawn(async move { shared.probe_flow(node_ids).await });
        Outcome::ok(Response::Accepted)
    }

    async fn patch_settings(
        &self,
        patch: &xt_contract::model::SettingsPatch,
    ) -> Result<(), ErrorBody> {
        let was_connected =
            self.snapshot_state().await.stage() == xt_contract::model::Stage::Connected;
        {
            let mut settings = self.settings.lock().await;
            settings.apply_patch(patch);
            xt_settings::save(&self.config.settings_path(), &settings)?;
        }
        if let Some(id) = &patch.selected_node {
            self.catalog.lock().await.restore_selection(Some(id.clone()));
        }
        // 已经跑着的核心不会因为文件改了而改变监听地址/日志级别 ——
        // 说清楚「下次连接生效」比悄悄不生效诚实。
        if was_connected && (patch.socks_listen.is_some() || patch.log_level.is_some()) {
            self.emit_notice(
                NoticeSeverity::Info,
                ErrorCode::Conflict,
                "设置已保存；监听地址与日志级别的变更会在下次连接时生效（当前核心仍按旧配置运行）",
            )
            .await;
        }
        Ok(())
    }
}

impl Daemon {
    /// 装配：读设置、读本地订阅、建目录与总线。**不做任何网络动作**。
    pub async fn bootstrap(config: DaemonConfig) -> Result<Daemon, ErrorBody> {
        tokio::fs::create_dir_all(&config.state_dir).await.map_err(|e| {
            ErrorBody::new(
                ErrorCode::Io,
                format!("创建状态目录 {} 失败: {e}", config.state_dir.display()),
            )
        })?;

        let settings_path = config.settings_path();
        let mut settings = xt_settings::load(&settings_path)?;
        if !settings_path.exists() {
            // 文件不存在 = 首次运行：此时才用命令行给的日志级别。
            // 文件存在时一律以文件为准，命令行不覆盖用户的持久化选择。
            settings.log_level = config.log_level;
        }

        let mut catalog = Catalog::new();
        let mut subscriptions = subs::load(&config.subscriptions_path()).await;
        // ① 先用 `--subscription-file` 的**当前内容**校正列表：`load_subscription` 会摘掉
        //    上次留下的 `local` 记录，再按文件现状重新登记（文件被删了就不该有这条）。
        load_subscription(&config, &mut catalog, &mut subscriptions).await;
        // ② 再把列表里**每条订阅已保存的节点**灌进目录：重启不该依赖网络才能有节点。
        //
        //    顺序不能反。反过来的话，`local` 的旧节点会先被灌进目录，而
        //    `load_subscription` 在文件缺失/解析失败时不会清目录 —— 界面就会出现
        //    「列表里没有这条订阅，目录里却一直是它的节点」，且每次重启都复现。
        for record in &subscriptions {
            if record.nodes.is_empty() {
                continue;
            }
            let source =
                NodeSource::Subscription { id: SubscriptionId::new(record.id.clone()) };
            let nodes = record.nodes.iter().cloned().map(xt_subs::ParsedNode::from).collect();
            catalog.replace_source(source, nodes);
        }
        load_imported_nodes_into(&config, &mut catalog).await;

        let shared = Arc::new(Shared {
            config,
            bus: Bus::new(256),
            state: Mutex::new(State::disconnected()),
            catalog: Mutex::new(catalog),
            settings: Mutex::new(settings),
            logs: Mutex::new(VecDeque::new()),
            core: Mutex::new(None),
            tun: Mutex::new(None),
            stats: Mutex::new(None),
            stats_gate: Mutex::new(None),
            datapath_version: Mutex::new(None),
            op_lock: Mutex::new(()),
            probe_lock: Mutex::new(()),
            subscriptions: Mutex::new(subscriptions),
            started_at_ms: now_ms(),
        });

        let node_count = shared.list_nodes().await.len();
        shared
            .log_daemon(LogLevel::Info, format!("daemon 装配完成：{node_count} 个节点"))
            .await;
        // 先把要记的日志收齐再放锁：持锁 await 是死锁的温床（`log_daemon` 自己也要拿锁）。
        let broken: Vec<String> = {
            let records = shared.subscriptions.lock().await;
            records
                .iter()
                .filter_map(|record| {
                    record.last_error.as_ref().map(|error| {
                        format!(
                            "订阅 {} 有问题：{}",
                            subs::redact_url(&record.url),
                            error.message
                        )
                    })
                })
                .collect()
        };
        for line in broken {
            shared.log_daemon(LogLevel::Error, line).await;
        }
        Ok(Daemon { shared })
    }

    pub fn socket_path(&self) -> &Path {
        &self.shared.config.socket_path
    }

    /// 只绑定监听，不开始服务。
    ///
    /// 绑定与循环分开是为了让「socket 已经存在」成为一个**可观测事件**：
    /// 调用方（E2E、包装脚本）在 `bind()` 返回后直接连接即可，不需要
    /// 「等一会儿再看文件在不在」这种轮询。
    pub async fn bind(self) -> Result<BoundDaemon, ErrorBody> {
        let server = Server::bind(&self.shared.config.socket_path).await?;
        self.shared
            .log_daemon(LogLevel::Info, format!("监听 {}", server.path().display()))
            .await;
        Ok(BoundDaemon { shared: self.shared, server })
    }

    /// 绑定并服务连接，直到 `shutdown` 触发。
    pub async fn serve(
        self,
        shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), ErrorBody> {
        self.bind().await?.serve(shutdown).await
    }
}

/// 已绑定但还没开始 accept 的 daemon。
pub struct BoundDaemon {
    shared: Arc<Shared>,
    server: Server,
}

impl BoundDaemon {
    pub fn socket_path(&self) -> &Path {
        self.server.path()
    }

    /// 服务连接，直到 `shutdown` 触发。退出时停掉还在跑的数据面 —— 那是清理，
    /// 不是「换一条路成功」。
    pub async fn serve(
        self,
        mut shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), ErrorBody> {
        let BoundDaemon { shared, server } = self;
        loop {
            tokio::select! {
                accepted = server.accept() => {
                    match accepted {
                        Ok(conn) => {
                            let shared = Arc::clone(&shared);
                            tokio::spawn(async move { handle_connection(shared, conn).await });
                        }
                        Err(error) => {
                            // 单条连接 accept 失败不该把 daemon 带走。
                            tracing::warn!(error = %error, "accept 失败");
                        }
                    }
                }
                _ = &mut shutdown => break,
            }
        }
        shared.stop_core_session().await;
        shared.log_daemon(LogLevel::Info, "daemon 已停止").await;
        Ok(())
    }
}

/// 手工导入的节点在磁盘上的形状。
///
/// 为什么不直接序列化 `xt_subs::ParsedNode`：那是解析器的输出、只实现了 `Serialize`
/// （它不是持久化格式）。这里显式列出我们真正要存的东西，让磁盘格式与解析器内部形状解耦。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredNode {
    id: String,
    name: String,
    protocol: String,
    endpoint: String,
    outbound: serde_json::Value,
}

impl From<&xt_subs::ParsedNode> for StoredNode {
    fn from(node: &xt_subs::ParsedNode) -> Self {
        Self {
            id: node.id.as_str().to_string(),
            name: node.name.clone(),
            protocol: node.protocol.clone(),
            endpoint: node.endpoint.clone(),
            outbound: node.outbound.clone(),
        }
    }
}

impl From<StoredNode> for xt_subs::ParsedNode {
    fn from(node: StoredNode) -> Self {
        Self {
            id: NodeId::new(node.id),
            name: node.name,
            protocol: node.protocol,
            endpoint: node.endpoint,
            outbound: node.outbound,
        }
    }
}

/// 合并「已经导入的」与「这次解析出来的」：按节点 id 去重（先到的保留），
/// 返回 `(合并结果, 本次新增数)`。纯函数，便于单测。
fn merge_imported(
    existing: Vec<StoredNode>,
    parsed: Vec<xt_subs::ParsedNode>,
) -> (Vec<StoredNode>, usize) {
    let mut seen: std::collections::HashSet<String> =
        existing.iter().map(|node| node.id.clone()).collect();
    let mut merged = existing;
    let mut added = 0usize;
    for node in parsed {
        if seen.insert(node.id.as_str().to_string()) {
            merged.push(StoredNode::from(&node));
            added += 1;
        }
    }
    (merged, added)
}

/// 读手工导入的节点文件。**读不到 = 空**（首次导入是正常状态）；**损坏 = 空 + 一条
/// error 日志**：不让 daemon 起不来，也不把「读坏了」悄悄说成「你没导入过」。
async fn read_imported_nodes(path: &Path) -> Vec<StoredNode> {
    match tokio::fs::read(path).await {
        Ok(bytes) => match serde_json::from_slice::<Vec<StoredNode>>(&bytes) {
            Ok(nodes) => nodes,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    path = %path.display(),
                    "手工导入的节点文件损坏，本次按空处理；重新导入会覆盖它"
                );
                Vec::new()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::error!(error = %error, path = %path.display(), "读取手工导入的节点文件失败");
            Vec::new()
        }
    }
}

/// 落盘：`tmp` + `rename`（与设置同一个原子替换约定：崩在中间不会留下半个 JSON）。
async fn save_imported_nodes(path: &Path, nodes: &[StoredNode]) -> Result<(), ErrorBody> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|error| {
            ErrorBody::new(
                ErrorCode::Io,
                format!("创建状态目录 {} 失败: {error}", dir.display()),
            )
        })?;
    }
    let body = serde_json::to_vec_pretty(nodes).map_err(|error| {
        ErrorBody::new(ErrorCode::Internal, format!("序列化导入节点失败: {error}"))
    })?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &body)
        .await
        .map_err(|error| ErrorBody::new(ErrorCode::Io, format!("写入 {} 失败: {error}", tmp.display())))?;
    tokio::fs::rename(&tmp, path).await.map_err(|error| {
        ErrorBody::new(ErrorCode::Io, format!("原子替换 {} 失败: {error}", path.display()))
    })?;
    Ok(())
}

/// 启动时把上次导入的节点装回目录。与订阅文件是**两个独立来源**。
async fn load_imported_nodes_into(config: &DaemonConfig, catalog: &mut Catalog) {
    let nodes: Vec<xt_subs::ParsedNode> = read_imported_nodes(&config.imported_nodes_path())
        .await
        .into_iter()
        .map(xt_subs::ParsedNode::from)
        .collect();
    if nodes.is_empty() {
        return;
    }
    tracing::info!(count = nodes.len(), "装载手工导入的节点");
    catalog.replace_source(NodeSource::Manual, nodes);
}

/// 读本地订阅文件并灌进目录。
///
/// 它是**文件驱动**的：每次启动都以文件为准（先摘掉上次的 `local` 记录再重读），
/// 因为「用户改了那个文件」和「文件被删了」都必须立刻反映出来。
/// 解析失败**不阻止 daemon 启动**，但必须如实记在 `last_error` 与日志里 ——
/// 悄悄当作「没有节点」会让用户以为订阅是空的。
async fn load_subscription(
    config: &DaemonConfig,
    catalog: &mut Catalog,
    subscriptions: &mut Vec<subs::SubscriptionRecord>,
) {
    let id = "local".to_string();
    let url = config.subscription_file.display().to_string();
    subscriptions.retain(|record| record.id != id);

    let body = match tokio::fs::read_to_string(&config.subscription_file).await {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            subscriptions.push(subs::SubscriptionRecord {
                id,
                url,
                fetched_at_ms: None,
                nodes: Vec::new(),
                last_error: Some(ErrorBody::new(
                    ErrorCode::Io,
                    format!("读取订阅文件失败: {err}"),
                )),
            });
            return;
        }
    };

    match xt_subs::parse(&body) {
        Ok(parsed) => {
            let fetched_at_ms = file_mtime_ms(&config.subscription_file);
            catalog.replace_source(
                NodeSource::Subscription { id: SubscriptionId::new(id.clone()) },
                parsed.nodes.clone(),
            );
            subscriptions.push(subs::SubscriptionRecord {
                id,
                url,
                fetched_at_ms,
                nodes: parsed.nodes.iter().map(StoredNode::from).collect(),
                last_error: None,
            });
        }
        Err(error) => {
            subscriptions.push(subs::SubscriptionRecord {
                id,
                url,
                fetched_at_ms: file_mtime_ms(&config.subscription_file),
                nodes: Vec::new(),
                last_error: Some(error),
            });
        }
    }
}

fn file_mtime_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1)
            .max(1),
    )
}

/// 一条连接的处理循环：一边收请求，一边把订阅到的事件推出去。
///
/// `biased` 让请求优先于事件：客户端在等应答时不该被事件流饿死。
async fn handle_connection(shared: Arc<Shared>, mut conn: Connection) {
    let mut hellod = false;
    let mut events: Option<EventStream> = None;
    let mut seq: u64 = 0;

    loop {
        tokio::select! {
            biased;
            frame = conn.next_frame() => {
                match frame {
                    Ok(Frame::Request { id, request }) => {
                        let (outcome, close) = dispatch(&shared, &mut hellod, &mut events, request).await;
                        if conn.send(Frame::response(id, outcome)).await.is_err() {
                            break;
                        }
                        if close {
                            break;
                        }
                    }
                    // 客户端发 response/event 帧 = 协议违规：帧序已经不可信，断开。
                    Ok(other) => {
                        tracing::debug!(?other, "客户端发了非请求帧，关闭连接");
                        break;
                    }
                    Err(error) => {
                        // EOF 是正常结束；其它 IO 错误也只是这条连接的事。
                        tracing::debug!(error = %error, "连接读取结束");
                        break;
                    }
                }
            }
            event = recv_event(&mut events) => {
                match event {
                    Some(event) => {
                        seq += 1;
                        if conn.send(Frame::event(seq, event)).await.is_err() {
                            break;
                        }
                    }
                    // 总线关闭：继续读请求已经没有意义。
                    None => break,
                }
            }
        }
    }
}

async fn recv_event(events: &mut Option<EventStream>) -> Option<Event> {
    match events {
        Some(stream) => stream.recv().await,
        // 还没 Subscribe：这个分支永远不产生值，select 只会走请求分支。
        None => std::future::pending().await,
    }
}

/// 派发一个请求。返回 `(应答, 是否关闭连接)`。
async fn dispatch(
    shared: &Arc<Shared>,
    hellod: &mut bool,
    events: &mut Option<EventStream>,
    request: Request,
) -> (Outcome, bool) {
    if !*hellod {
        // 契约：第一条必须是 hello，版本不符一律拒绝并关闭（不做兼容）。
        return match request {
            Request::Hello { protocol_version, .. } => {
                if protocol_version != xt_contract::PROTOCOL_VERSION {
                    (
                        Outcome::error(bad_request(format!(
                            "协议版本 {protocol_version} 与 daemon 的 {} 不一致",
                            xt_contract::PROTOCOL_VERSION
                        ))),
                        true,
                    )
                } else {
                    *hellod = true;
                    (Outcome::ok(Response::Hello(shared.hello())), false)
                }
            }
            _ => (Outcome::error(bad_request("第一条请求必须是 hello")), true),
        };
    }

    match request {
        Request::Hello { .. } => (Outcome::error(bad_request("hello 只能发一次")), false),
        Request::Subscribe { topics } => {
            *events = Some(shared.bus.subscribe(&topics));
            (Outcome::ok(Response::Subscribed { topics }), false)
        }
        Request::Status => (Outcome::ok(Response::Status(shared.status_view().await)), false),
        Request::ListNodes => (
            Outcome::ok(Response::Nodes { nodes: shared.list_nodes().await }),
            false,
        ),
        Request::Connect { node_id, mode } => {
            (shared.begin_connect_intent(node_id, mode).await, false)
        }
        Request::Disconnect => (shared.begin_disconnect_intent(), false),
        Request::SwitchNode { node_id } => (shared.begin_switch_intent(node_id).await, false),
        Request::ProbeNodes { node_ids } => (shared.begin_probe_intent(node_ids), false),
        Request::GetSettings => {
            (Outcome::ok(Response::Settings(shared.settings_view().await)), false)
        }
        Request::PatchSettings { patch } => match shared.patch_settings(&patch).await {
            Ok(()) => (Outcome::ok(Response::Ok), false),
            Err(error) => (Outcome::error(error), false),
        },
        Request::ListSubscriptions => {
            let list: Vec<SubscriptionView> =
                shared.subscriptions.lock().await.iter().map(subs::view).collect();
            (Outcome::ok(Response::Subscriptions { subscriptions: list }), false)
        }
        // 手工导入：只做**本地解析**（不联网），解析结果落盘并替换 `Manual` 来源。
        Request::ImportNodes { payload } => (shared.import_nodes(payload).await, false),
        // 订阅远端拉取：走 `/usr/bin/curl`（零新增依赖；理由见 subs 模块文档）。
        Request::AddSubscription { url } => (shared.add_subscription(url).await, false),
        Request::RefreshSubscription { id } => (shared.refresh_subscription(id).await, false),
        Request::TailLogs { lines } => (
            Outcome::ok(Response::Logs { logs: shared.tail_logs(lines).await }),
            false,
        ),
    }
}

/// 解析 `host:port`；socks 监听地址来自设置，非回环地址一律拒绝
/// （daemon 只服务本机，开放到外网是一个我们没有能力保护的攻击面）。
pub(crate) fn parse_loopback_addr(raw: &str, what: &str) -> Result<SocketAddr, ErrorBody> {
    let addr: SocketAddr = raw
        .trim()
        .parse()
        .map_err(|e| bad_request(format!("{what} {raw} 不是合法的 host:port: {e}")))?;
    if !addr.ip().is_loopback() {
        return Err(bad_request(format!("{what} 必须是回环地址（当前 {addr}）")));
    }
    Ok(addr)
}

/// api 监听 = socks 端口 + 1。对用户不暴露成设置项：
/// 用户改一个端口却忘了另一个，就会得到一个自己连不上的核心。
pub(crate) fn api_addr_for(socks: SocketAddr) -> Result<SocketAddr, ErrorBody> {
    let port = socks
        .port()
        .checked_add(1)
        .ok_or_else(|| bad_request("socks 端口 +1 溢出，请换一个更低端口"))?;
    Ok(SocketAddr::new(socks.ip(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(id: &str) -> StoredNode {
        StoredNode {
            id: id.to_string(),
            name: format!("node-{id}"),
            protocol: "vless".to_string(),
            endpoint: "a.example.com:443".to_string(),
            outbound: serde_json::json!({ "protocol": "vless" }),
        }
    }

    /// 用真实解析器造节点（避免夹具与生产形状漂移）。
    fn parsed(host: &str) -> xt_subs::ParsedNode {
        let line = format!("vless://uuid-{host}@{host}:443?encryption=none&security=none#{host}");
        xt_subs::parse_links(&line).expect("构造测试节点").remove(0)
    }

    #[test]
    fn merge_imported_keeps_existing_and_dedupes_by_id() {
        let node = parsed("new.example.com");
        let (merged, added) = merge_imported(vec![stored("keep")], vec![node.clone()]);
        assert_eq!(added, 1, "新节点必须计入新增");
        assert_eq!(merged.len(), 2, "已导入的不能被冲掉");

        // 同一个 id 再导入一次：不重复、不计新增。
        let (again, added_again) = merge_imported(merged, vec![node.clone()]);
        assert_eq!(added_again, 0, "重复导入同一条不该算新增");
        assert_eq!(again.len(), 2);
        assert!(again.iter().any(|n| n.id == node.id.as_str()));
    }

    #[test]
    fn stored_node_round_trips_through_parsed_node() {
        let node = parsed("roundtrip.example.com");
        let back = xt_subs::ParsedNode::from(StoredNode::from(&node));
        assert_eq!(back.id, node.id);
        assert_eq!(back.name, node.name);
        assert_eq!(back.protocol, node.protocol);
        assert_eq!(back.endpoint, node.endpoint);
        assert_eq!(back.outbound, node.outbound);
    }
}
