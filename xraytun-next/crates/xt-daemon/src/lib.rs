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

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use xt_bus::{Bus, EventStream};
use xt_contract::error::{bad_request, internal, unsupported, ErrorBody, ErrorCode};
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
    subscription: Mutex<Option<SubscriptionView>>,
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
            // 出现按不动的按钮（本轮 tun 与远端拉取不宣告）。
            capabilities: vec![
                Capability::ProxyMode,
                Capability::Stats,
                Capability::Probe,
                Capability::Subscriptions,
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

    // -------------------------------------------------------------- 意图受理

    async fn begin_connect_intent(self: &Arc<Self>, node_id: NodeId, mode: RunMode) -> Outcome {
        if mode != RunMode::Proxy {
            return Outcome::error(unsupported(
                "tun 模式需真机验收通过后才宣告 TunMode 能力（S6）；当前只开放 proxy",
            ));
        }
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
        let mut subscription = None;
        load_subscription(&config, &mut catalog, &mut subscription).await;

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
            subscription: Mutex::new(subscription),
            started_at_ms: now_ms(),
        });

        let node_count = shared.list_nodes().await.len();
        shared
            .log_daemon(LogLevel::Info, format!("daemon 装配完成：{node_count} 个节点"))
            .await;
        if let Some(sub) = shared.subscription.lock().await.as_ref() {
            if let Some(error) = &sub.last_error {
                shared
                    .log_daemon(LogLevel::Error, format!("本地订阅解析失败：{}", error.message))
                    .await;
            }
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

/// 读本地订阅文件并灌进目录。
///
/// 远端拉取本轮不做（能力表里也没有 `SubscriptionFetch`）；解析失败**不阻止
/// daemon 启动**，但必须如实记在 `SubscriptionView.last_error` 与日志里 ——
/// 悄悄当作「没有节点」会让用户以为订阅是空的。
async fn load_subscription(
    config: &DaemonConfig,
    catalog: &mut Catalog,
    subscription: &mut Option<SubscriptionView>,
) {
    let id = SubscriptionId::new("local");
    let url = config.subscription_file.display().to_string();
    let body = match tokio::fs::read_to_string(&config.subscription_file).await {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            *subscription = Some(SubscriptionView {
                id,
                url,
                node_count: 0,
                fetched_at_ms: None,
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
            let node_count = parsed.nodes.len() as u32;
            catalog.replace_source(NodeSource::Subscription { id: id.clone() }, parsed.nodes);
            *subscription = Some(SubscriptionView {
                id,
                url,
                node_count,
                fetched_at_ms: file_mtime_ms(&config.subscription_file),
                last_error: None,
            });
        }
        Err(error) => {
            *subscription = Some(SubscriptionView {
                id,
                url,
                node_count: 0,
                fetched_at_ms: file_mtime_ms(&config.subscription_file),
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
            let list = shared.subscription.lock().await.clone();
            (
                Outcome::ok(Response::Subscriptions {
                    subscriptions: list.into_iter().collect(),
                }),
                false,
            )
        }
        // 远端拉取本轮不做：它是「本版本不提供的能力」，不是「试了但失败」。
        Request::AddSubscription { .. } | Request::RefreshSubscription { .. } => (
            Outcome::error(unsupported(
                "本版本不提供远端订阅拉取；节点来自本地订阅文件（--subscription-file）",
            )),
            false,
        ),
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
