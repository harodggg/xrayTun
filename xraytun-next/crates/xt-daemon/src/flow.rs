//! 数据面流程：连接 / 断开 / 切节点 / 探测。
//!
//! 这里的每一段都是「状态机信号 + 真实事件」的翻译，不自己发明状态：
//! 配置落盘 → `ConfigReady`；进程真实存在 → `CoreStarted{pid}`；
//! SOCKS 真的可连 → `CoreReady{at_ms}`；进程退出/任一步失败 → `CoreExited{error}`。
//!
//! 失败只有一条出口：落回 `Disconnected`，带上真实原因。没有重试，也没有
//! 「换下一个节点试试」—— 那正是本项目要消除的回落。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use xt_contract::error::{internal, ErrorBody, ErrorCode};
use xt_contract::model::{
    LogLevel, LogLine, NodeId, NoticeSeverity, ProbeResult, RunMode, Stage,
};
use xt_contract::protocol::Event;
use xt_datapath::{DatapathSpec, RunningDatapath};
use xt_probe::{ProbePlan, ProbeTarget, Prober};
use xt_state::{apply, begin_connect, begin_switch, Signal};
use xt_stats::StatsClient;
use xt_helperproto::{SessionRef, TunUpArgs};

use crate::helper::HelperClient;

use crate::{
    api_addr_for, now_ms, parse_loopback_addr, Shared, CORE_STOP_DEADLINE, PROBE_BASE_PORT,
};

/// 用户意图的形状。`Connect` 携带 mode，`Switch` 沿用当前 mode（proxy）。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Intent {
    Connect(RunMode),
    Switch,
}

/// 统计链路预热的失败上限。
///
/// 它**不是**采样周期，也不是"等一会儿再看看"：TCP 已经就绪之后，gRPC 服务要么
/// 立刻能应答，要么就是真的异常。设一个秒级上限，是为了在真异常时还能给用户
/// 一个可用的 SOCKS 通道（此时如实显示"未采样"），而不是把连接一起失败掉。
const STATS_WARMUP_DEADLINE: Duration = Duration::from_secs(5);

/// 一次核心会话：daemon 需要「请它停」与「等它死」两条通道。
///
/// 为什么要两条：`stop_tx` 是**请求**（fire and forget），`exit_rx` 是**事实**
/// （监督任务一定会发一次，无论进程是自己死的还是被我们停的）。断开流程只等
/// `exit_rx`，因此不存在「停不下来又没有任何事件」的中间态。
pub(crate) struct CoreSession {
    stop_tx: tokio::sync::mpsc::Sender<()>,
    exit_rx: Option<tokio::sync::oneshot::Receiver<CoreExit>>,
}

/// 核心会话的结束方式。区分二者是为了不把「我们请它停」报成「核心崩了」。
#[derive(Clone, Copy, Debug)]
pub(crate) enum CoreExit {
    Stopped,
    Died(Option<i32>),
}

/// TUN 会话持有的 helper 连接 + 会话引用：断开时用它调 helper.tun_down。
pub(crate) struct TunSession {
    pub helper: HelperClient,
    pub session: SessionRef,
}

/// TUN 网段参数。设置里还没有 TUN 项（S4 接 UI 后再改成设置驱动），先用常量。
const TUN_ADDRESS: &str = "198.18.0.1/15";
const TUN_GATEWAY: &str = "198.18.0.1";
const TUN_MTU: u16 = 1420;
const TUN_DNS_SERVER: &str = "198.18.0.2";
/// direct 出站绑定的物理网卡。**待真机/待 helper 上报**：daemon 目前不知道
/// 物理网卡名（helper 的 discover_physical 结果还没回传），先用占位 `en0`。
const TUN_PHYSICAL_IFACE: &str = "en0";
const TUN_DEFAULT_ROUTES: &[&str] = &["0.0.0.0/1", "128.0.0.0/1"];
/// helperd 的 AF_UNIX socket 路径（与 helperd 的默认 `--socket` 一致）。
const HELPER_SOCKET: &str = "/var/run/xraytun-helper.sock";

struct PreparedConfig {
    socks_addr: SocketAddr,
    api_addr: SocketAddr,
    config_path: PathBuf,
}

impl Shared {
    /// 连接（或切节点后重连）的完整流程。
    pub(crate) async fn connect_flow(self: &Arc<Self>, node_id: NodeId, intent: Intent) {
        if matches!(intent, Intent::Connect(RunMode::Tun)) {
            self.connect_tun_flow(node_id).await;
            return;
        }
        let _op = self.op_lock.lock().await;
        // 「就绪耗时」的起点：文档里说的启动耗时必须能追到这一个真实时刻。
        let connect_started_ms = now_ms();

        // ---- 意图绑定：状态机是唯一入口 ------------------------------------
        let state = self.snapshot_state().await;
        let transition = match intent {
            Intent::Connect(mode) => begin_connect(&state, mode, node_id.clone()),
            // 「选择就用」一条路径：未连接时切节点 = 连接。
            Intent::Switch => match state.stage() {
                Stage::Disconnected => begin_connect(&state, RunMode::Proxy, node_id.clone()),
                _ => begin_switch(&state, node_id.clone()),
            },
        };
        let transition = match transition {
            Ok(transition) => transition,
            Err(error) => {
                self.emit_intent_error(error).await;
                return;
            }
        };
        self.log_daemon(LogLevel::Info, format!("开始连接节点 {node_id}")).await;
        // 上一个会话的统计与版本属于上一个核心：新会话没有采样之前必须是
        // 「未采样」，而不是沿用旧数字。
        self.clear_stats().await;
        self.set_datapath_version(None).await;
        self.commit(transition).await; // Connecting{PreparingConfig}

        // 切节点时旧核心还占着同一个 socks 端口，必须先停干净。
        self.stop_core_session().await;

        // ---- PreparingConfig：生成 → 落盘 → 预检 ---------------------------
        let prepared = match self.prepare_config(&node_id).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_connect(error).await;
                return;
            }
        };

        let state = self.snapshot_state().await;
        let transition = match apply(&state, Signal::ConfigReady, now_ms()) {
            Ok(transition) => transition,
            Err(error) => {
                self.fail_connect(error).await;
                return;
            }
        };
        self.commit(transition).await; // Connecting{StartingCore}

        // ---- StartingCore：spawn 真进程 ------------------------------------
        let spec = DatapathSpec {
            xray_bin: self.config.xray_bin.clone(),
            config_path: prepared.config_path.clone(),
            socks_addr: prepared.socks_addr,
            // api 入站与 socks 不是同一个就绪事件：把它一起纳入就绪判定，
            // 后面的 StatsClient::connect 才不会撞上 refused。
            required_addrs: vec![prepared.api_addr],
            log_level: self.settings_view().await.log_level,
            tun_fd: None,
        };
        let mut datapath = match xt_datapath::start(&spec).await {
            Ok(datapath) => datapath,
            Err(error) => {
                self.fail_connect(error).await;
                return;
            }
        };
        let pid = match datapath.pid() {
            Some(pid) => pid,
            None => {
                // 真实地读不到 pid：如实失败，绝不用一个 0 顶上。
                let _ = datapath.stop().await;
                self.fail_connect(internal("核心已启动但读不到 pid")).await;
                return;
            }
        };
        self.set_datapath_version(datapath.version()).await;
        self.log_daemon(LogLevel::Info, format!("核心已启动 pid={pid}")).await;

        let state = self.snapshot_state().await;
        match apply(&state, Signal::CoreStarted { pid }, now_ms()) {
            Ok(transition) => self.commit(transition).await, // AwaitingReady
            Err(error) => {
                let _ = datapath.stop().await;
                self.fail_connect(error).await;
                return;
            }
        }

        // ---- AwaitingReady：事件驱动（每读到一行输出试一次 SOCKS 可连）------
        let ready = match datapath.wait_ready().await {
            Ok(info) => info,
            Err(error) => {
                // 核心还在跑但不监听 / 已经退出：先收尸，再如实落失败终态。
                let _ = datapath.stop().await;
                self.fail_connect(error).await;
                return;
            }
        };

        // ---- 先装好统计链路，**再**让 Connected 变成可观测状态 ----------------
        //
        // 顺序在这里是语义，不是风格：`Connected` 一旦发布出去，消费者（UI / E2E）
        // 就有权立刻发 `Status`。而 UI 的采样是"消费者驱动"的 —— 它看到 Connected
        // 就会去问。如果这时候 `stats` 客户端还没装好，`sample_stats` 只能返回 `None`，
        // 界面就显示"未采样"，可实际数据是存在的。
        //
        // 「装好」的判据是**真的查到过一次真实计数**，不是"h2 握手成功"：
        // 握手成功只证明 api 入站 accept 了 TCP，不证明 gRPC 服务已经能应答
        // （同一份二进制里我们见过两种结果）。所以把这一步并进就绪判定：
        // 没成功就等核心的**下一条日志**再试一次（事件驱动，不 sleep、不轮询；
        // 每次探测本身也会让核心打一行日志，循环因此由真实事件推进），
        // 超过窗口才如实降级为"未采样 + Notice"——降级时界面看到的仍是实话。
        let mut ticks = datapath.logs();
        let warmup_deadline = tokio::time::Instant::now() + STATS_WARMUP_DEADLINE;
        let mut stats_ready = false;
        loop {
            match StatsClient::connect(prepared.api_addr).await {
                Ok(client) => match client.sample().await {
                    Ok(_) => {
                        // 注意：这里**不**把这次预热样本写进缓存闸门。
                        // 预热发生在流量之前，它的数字几乎必然是 0；把 0 当成"最近样本"
                        // 会让 Connected 之后的第一次 Status 返回 0 —— 那就是把
                        // "现在还没流量" 冒充成 "刚刚的采样"，属于 I3 禁止的假话。
                        self.set_stats_client(Some(Arc::new(client))).await;
                        stats_ready = true;
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "StatsService 尚未应答，等核心下一条日志再试");
                    }
                },
                Err(error) => {
                    tracing::debug!(error = %error, "api 入站尚未建立 h2 会话，等核心下一条日志再试");
                }
            }
            if stats_ready || tokio::time::Instant::now() >= warmup_deadline {
                break;
            }
            // 等一条真实事件（核心的输出），或者等到失败上限。
            // 用 timeout_at 而不是 sleep：这里的等待由事件唤醒，时间只用来兜底
            // 「核心一声不吭」的情况 —— 与 xt-datapath 的 wait_ready 同一套口径。
            match tokio::time::timeout_at(warmup_deadline, ticks.recv()).await {
                Ok(Ok(_)) => {}
                // 慢订阅者丢了中间几行不影响判断：继续等下一行。
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                // 两个输出流都关了 ⇒ 核心不会再变好；到点也一样。
                Ok(Err(_)) | Err(_) => break,
            }
        }
        if !stats_ready {
            self.emit_notice(
                NoticeSeverity::Warning,
                ErrorCode::DatapathUnavailable,
                "统计服务在就绪窗口内没有应答：界面将显示未采样（不会用 0 顶替）".to_string(),
            )
            .await;
        }

        let state = self.snapshot_state().await;
        match apply(&state, Signal::CoreReady { at_ms: ready.ready_at_ms }, now_ms()) {
            Ok(transition) => self.commit(transition).await, // Connected
            Err(error) => {
                let _ = datapath.stop().await;
                self.fail_connect(error).await;
                return;
            }
        }

        // ---- 监督：核心自己死了必须变成一条真实的状态迁移 -------------------
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(supervise(Arc::clone(self), datapath, stop_rx, exit_tx));
        self.install_core(CoreSession { stop_tx, exit_rx: Some(exit_rx) }).await;
        self.publish_view().await;
        self.log_daemon(
            LogLevel::Info,
            format!(
                "已连接 {node_id}（pid={pid}，就绪耗时 {}ms）",
                ready.ready_at_ms.saturating_sub(connect_started_ms)
            ),
        )
        .await;
    }

    /// 让核心失败 → `Disconnected + last_error`，成功路径不会被写进失败。
    async fn fail_connect(&self, error: ErrorBody) {
        let state = self.snapshot_state().await;
        match apply(&state, Signal::CoreExited { error: error.clone() }, now_ms()) {
            Ok(transition) => self.commit(transition).await,
            // 状态已经不接受失败信号（例如并发断开已把它推到 Disconnected）：
            // 发当前真实视图，而不是硬塞一个旧状态。
            Err(_) => self.publish_view().await,
        }
        self.emit_notice(NoticeSeverity::Error, error.code, error.message).await;
    }

    /// 生成配置 → 落盘 → `xray run -test` 预检。
    async fn prepare_config(&self, node_id: &NodeId) -> Result<PreparedConfig, ErrorBody> {
        let settings = self.settings_view().await;
        let socks_addr = parse_loopback_addr(&settings.socks_listen, "socks 监听地址")?;
        let api_addr = api_addr_for(socks_addr)?;
        let selected = self.outbound_spec(node_id).await?;

        let inputs = xt_xrayconf::ConfigInputs {
            listen_socks: socks_addr,
            api_listen: api_addr,
            selected,
            log_level: settings.log_level,
        };
        // 配置内容由 xt-xrayconf 生成；daemon 只负责落盘与预检。
        let json = xt_xrayconf::generate(&inputs)?;
        let config_path = self.config.active_config_path();
        if let Some(dir) = config_path.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                ErrorBody::new(ErrorCode::Io, format!("创建配置目录失败: {e}"))
            })?;
        }
        tokio::fs::write(&config_path, json.as_bytes()).await.map_err(|e| {
            ErrorBody::new(
                ErrorCode::Io,
                format!("写入配置 {} 失败: {e}", config_path.display()),
            )
        })?;

        // 预检必须在 spawn 之前：非法配置的错误是 `config_invalid`，
        // 而不是「进程起来又退了」这种指向错误方向的现场。
        xt_datapath::validate_config(&self.config.xray_bin, &config_path).await?;
        Ok(PreparedConfig { socks_addr, api_addr, config_path })
    }

    /// TUN 连接：helper.TunUp → TakeTunFd → spawn xray(XRAY_TUN_FD) → 就绪 → CommitRoutes。
    ///
    /// 当前**不可达**：`begin_connect_intent` 在真机验收（S6）通过前拒绝 tun 模式。
    /// 这里把两阶段流程写完并编译，真机验收后去掉入口拒绝即可启用。
    async fn connect_tun_flow(self: &Arc<Self>, node_id: NodeId) {
        let _op = self.op_lock.lock().await;
        let connect_started_ms = now_ms();

        let state = self.snapshot_state().await;
        let transition = match begin_connect(&state, RunMode::Tun, node_id.clone()) {
            Ok(t) => t,
            Err(e) => return self.emit_intent_error(e).await,
        };
        self.log_daemon(LogLevel::Info, format!("开始连接节点 {node_id}（tun）")).await;
        self.clear_stats().await;
        self.set_datapath_version(None).await;
        self.commit(transition).await; // Connecting{PreparingConfig}
        self.stop_core_session().await;

        // ---- PreparingConfig：生成 TUN 配置 + 预检 ---------------------------
        let settings = self.settings_view().await;
        let api_addr = match parse_loopback_addr(&settings.socks_listen, "api 地址") {
            Ok(socks) => match api_addr_for(socks) {
                Ok(a) => a,
                Err(e) => return self.fail_connect(e).await,
            },
            Err(e) => return self.fail_connect(e).await,
        };
        let selected = match self.outbound_spec(&node_id).await {
            Ok(s) => s,
            Err(e) => return self.fail_connect(e).await,
        };
        let json = match xt_xrayconf::generate_tun(&xt_xrayconf::TunConfigInputs {
            api_listen: api_addr,
            selected,
            log_level: settings.log_level,
            addresses: vec![TUN_ADDRESS.to_string()],
            gateway: TUN_GATEWAY.to_string(),
            mtu: TUN_MTU,
            dns_server_addr: TUN_DNS_SERVER.to_string(),
            physical_interface: TUN_PHYSICAL_IFACE.to_string(),
        }) {
            Ok(j) => j,
            Err(e) => return self.fail_connect(e).await,
        };
        let config_path = self.config.active_config_path();
        if let Some(dir) = config_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(dir).await {
                return self
                    .fail_connect(ErrorBody::new(ErrorCode::Io, format!("创建配置目录失败: {e}")))
                    .await;
            }
        }
        if let Err(e) = tokio::fs::write(&config_path, json.as_bytes()).await {
            return self
                .fail_connect(ErrorBody::new(ErrorCode::Io, format!("写入配置失败: {e}")))
                .await;
        }
        if let Err(e) = xt_datapath::validate_config(&self.config.xray_bin, &config_path).await {
            return self.fail_connect(e).await;
        }
        let state = self.snapshot_state().await;
        match apply(&state, Signal::ConfigReady, now_ms()) {
            Ok(t) => self.commit(t).await, // StartingCore
            Err(e) => return self.fail_connect(e).await,
        }

        // ---- helper：TunUp → TakeTunFd（拿 utun fd）--------------------------
        let helper = match HelperClient::connect(std::path::Path::new(HELPER_SOCKET)).await {
            Ok(h) => h,
            Err(e) => return self.fail_connect(e).await,
        };
        let session = match helper
            .tun_up(TunUpArgs {
                addresses: vec![TUN_ADDRESS.to_string()],
                mtu: TUN_MTU,
                bypass_routes: vec![],
                default_routes: TUN_DEFAULT_ROUTES.iter().map(|s| s.to_string()).collect(),
                dns_servers: vec![TUN_DNS_SERVER.to_string()],
            })
            .await
        {
            Ok(s) => s,
            Err(e) => return self.fail_connect(e).await,
        };
        let tun_fd = match helper.take_tun_fd(&session).await {
            Ok(fd) => fd,
            Err(e) => {
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        };

        // ---- StartingCore：spawn xray 带 XRAY_TUN_FD -------------------------
        let spec = DatapathSpec {
            xray_bin: self.config.xray_bin.clone(),
            config_path: config_path.clone(),
            // TUN 无 socks 入站：就绪判定只看 api（StatsService）。
            socks_addr: api_addr,
            required_addrs: vec![],
            log_level: settings.log_level,
            tun_fd: Some(tun_fd),
        };
        let mut datapath = match xt_datapath::start(&spec).await {
            Ok(d) => d,
            Err(e) => {
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        };
        let pid = match datapath.pid() {
            Some(pid) => pid,
            None => {
                let _ = datapath.stop().await;
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(internal("核心已启动但读不到 pid")).await;
            }
        };
        self.set_datapath_version(datapath.version()).await;
        let state = self.snapshot_state().await;
        match apply(&state, Signal::CoreStarted { pid }, now_ms()) {
            Ok(t) => self.commit(t).await, // AwaitingReady
            Err(e) => {
                let _ = datapath.stop().await;
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        }

        // ---- AwaitingReady：等 api 入站可连 ----------------------------------
        let ready = match datapath.wait_ready().await {
            Ok(info) => info,
            Err(e) => {
                let _ = datapath.stop().await;
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        };
        let state = self.snapshot_state().await;
        match apply(&state, Signal::CoreReady { at_ms: ready.ready_at_ms }, now_ms()) {
            Ok(t) => self.commit(t).await, // CommittingRoutes
            Err(e) => {
                let _ = datapath.stop().await;
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        }

        // ---- CommittingRoutes：helper.CommitRoutes → RoutesCommitted ---------
        if let Err(e) = helper.commit_routes(&session).await {
            let _ = datapath.stop().await;
            let _ = helper.tun_down(&session).await;
            return self.fail_connect(e).await;
        }
        let state = self.snapshot_state().await;
        match apply(&state, Signal::RoutesCommitted, now_ms()) {
            Ok(t) => self.commit(t).await, // Connected
            Err(e) => {
                let _ = datapath.stop().await;
                let _ = helper.tun_down(&session).await;
                return self.fail_connect(e).await;
            }
        }

        // ---- 监督 + 记住 tun 会话（断开时 tun_down）--------------------------
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(supervise(Arc::clone(self), datapath, stop_rx, exit_tx));
        self.install_core(CoreSession { stop_tx, exit_rx: Some(exit_rx) }).await;
        *self.tun.lock().await = Some(TunSession { helper, session });
        self.publish_view().await;
        self.log_daemon(
            LogLevel::Info,
            format!(
                "已连接 {node_id}（tun，就绪耗时 {}ms）",
                ready.ready_at_ms.saturating_sub(connect_started_ms)
            ),
        )
        .await;
    }

    /// 断开：StopCore → `Disconnected`（`last_error` 清空，历史日志保留）。
    pub(crate) async fn disconnect_flow(self: &Arc<Self>) {
        let _op = self.op_lock.lock().await;
        let state = self.snapshot_state().await;
        match state.stage() {
            // 已经是终态：意图被满足，发一条当前真实视图作为终态事件，
            // 让客户端不必自己去猜「到底断没断」。
            Stage::Disconnected | Stage::Disconnecting => {
                self.publish_view().await;
                return;
            }
            _ => {}
        }

        let transition = match apply(&state, Signal::Disconnected, now_ms()) {
            Ok(transition) => transition,
            Err(error) => {
                self.emit_intent_error(error).await;
                return;
            }
        };
        self.log_daemon(LogLevel::Info, "正在断开").await;
        self.commit(transition).await; // Disconnecting
        self.stop_core_session().await;
        // TUN：先停数据面（上面），再让 helper 回滚路由/DNS（顺序见架构 §4.2）。
        if let Some(tun) = self.tun.lock().await.take() {
            let _ = tun.helper.tun_down(&tun.session).await;
        }

        let state = self.snapshot_state().await;
        if state.stage() == Stage::Disconnecting {
            match apply(&state, Signal::Disconnected, now_ms()) {
                // 正常停止：last_error 清空。
                Ok(transition) => self.commit(transition).await,
                Err(error) => self.emit_intent_error(error).await,
            }
        } else {
            // 监督任务已经把核心的意外退出报成了失败终态：不覆盖它。
            self.publish_view().await;
        }
        self.log_daemon(LogLevel::Info, "已断开").await;
    }

    /// 请当前核心会话停止并**等到确定的事实**。幂等：没有会话时什么都不做。
    pub(crate) async fn stop_core_session(&self) {
        let session = self.take_core().await;
        if let Some(mut session) = session {
            let _ = session.stop_tx.send(()).await;
            if let Some(exit_rx) = session.exit_rx.take() {
                match tokio::time::timeout(CORE_STOP_DEADLINE, exit_rx).await {
                    Ok(_) => {}
                    Err(_) => {
                        // 监督任务没在期限内回报：进程可能还在。如实留日志
                        //（`kill_on_drop` 会在会话被丢弃时兜底）。
                        tracing::warn!("核心停止未在期限内确认，可能有残留进程");
                    }
                }
            }
        }
        self.clear_stats().await;
        self.set_datapath_version(None).await;
    }

    /// 核心**自己**退出（不是我们请它停的）：如实落成失败终态，不重试。
    pub(crate) async fn on_core_died(self: &Arc<Self>, code: Option<i32>) {
        let _op = self.op_lock.lock().await;
        let state = self.snapshot_state().await;
        match state.stage() {
            // 断开流程自己收尾（它会把 Disconnecting 推到 Disconnected，且不带错误）。
            // 这里再迁一次就会产生第二条收尾路径。
            Stage::Disconnected | Stage::Disconnecting => return,
            _ => {}
        }

        let code_text = code.map(|c| c.to_string()).unwrap_or_else(|| "未知".to_string());
        let error = ErrorBody::new(
            ErrorCode::CoreExitedEarly,
            format!("数据面进程在运行中退出（退出码 {code_text}）"),
        )
        .with_detail(serde_json::json!({ "exit_code": code }));
        match apply(&state, Signal::CoreExited { error: error.clone() }, now_ms()) {
            Ok(transition) => self.commit(transition).await,
            Err(_) => self.publish_view().await,
        }
        // 会话已经结束：清掉句柄，别让后续 stop 去等一个不会再发生的事件。
        let _ = self.take_core().await;
        self.clear_stats().await;
        self.set_datapath_version(None).await;
        self.emit_notice(NoticeSeverity::Error, error.code, error.message).await;
    }

    /// 探测：临时实例 + 每节点一个独立 socks 端口，逐节点串行测真实 TTFB。
    pub(crate) async fn probe_flow(self: &Arc<Self>, node_ids: Vec<NodeId>) {
        let _probe = self.probe_lock.lock().await;

        let mut specs = Vec::new();
        for id in &node_ids {
            match self.outbound_spec(id).await {
                Ok(spec) => specs.push(spec),
                // 单个节点不在目录里不该拖垮整批：它自己报一条真实失败。
                Err(error) => self.publish_probe_failure(id.clone(), error).await,
            }
        }
        if specs.is_empty() {
            return;
        }

        let (config_json, ports) = match xt_xrayconf::generate_probe(&specs, PROBE_BASE_PORT) {
            Ok(built) => built,
            Err(error) => {
                for spec in &specs {
                    self.publish_probe_failure(spec.node_id.clone(), error.clone()).await;
                }
                return;
            }
        };
        let prober = match Prober::new(&self.config.probe_url) {
            Ok(prober) => prober,
            Err(error) => {
                for spec in &specs {
                    self.publish_probe_failure(spec.node_id.clone(), error.clone()).await;
                }
                return;
            }
        };
        let targets = specs
            .iter()
            .zip(ports.iter())
            .map(|(spec, port)| ProbeTarget {
                node_id: spec.node_id.clone(),
                socks_addr: SocketAddr::from(([127, 0, 0, 1], *port)),
            })
            .collect();
        let plan = ProbePlan {
            xray_bin: self.config.xray_bin.clone(),
            config_json,
            config_path: self.config.probe_config_path(),
            targets,
        };

        match prober.run(&plan).await {
            Ok(results) => {
                for result in results {
                    self.publish_event(Event::Probe { result });
                }
            }
            Err(error) => {
                // 临时实例起不来 / 配置被拒：每个节点各报一条真实失败。
                for spec in &specs {
                    self.publish_probe_failure(spec.node_id.clone(), error.clone()).await;
                }
            }
        }
    }

    async fn publish_probe_failure(&self, node_id: NodeId, error: ErrorBody) {
        let result = ProbeResult::failed(node_id, error, now_ms());
        self.publish_event(Event::Probe { result });
        // 探测失败不是连接失败，不该弹 Notice；但要在日志里留一条可排查的线。
        self.log_daemon(LogLevel::Warn, "一个节点探测失败（原因已随 Probe 事件发出）").await;
    }
}

/// 监督一个核心会话：等它退出，或等我们请它停。
///
/// `biased` 先看「进程退出」：如果它在我们发停指令的同时死了，算它自己死的 ——
/// 这是一种更保守的事实（宁可报崩溃，也不把崩溃说成正常停止）。
async fn supervise(
    shared: Arc<Shared>,
    mut datapath: RunningDatapath,
    mut stop_rx: tokio::sync::mpsc::Receiver<()>,
    exit_tx: tokio::sync::oneshot::Sender<CoreExit>,
) {
    let logs = tokio::spawn(pump_logs(Arc::clone(&shared), datapath.logs()));
    let outcome = tokio::select! {
        biased;
        code = datapath.wait_exit() => CoreExit::Died(code),
        _ = stop_rx.recv() => {
            let _ = datapath.stop().await;
            CoreExit::Stopped
        }
    };
    logs.abort();
    // 先把事实发出去，再决定要不要动状态：断开流程正等着这个事实。
    let _ = exit_tx.send(outcome);
    if let CoreExit::Died(code) = outcome {
        shared.on_core_died(code).await;
    }
}

/// 核心日志 → 环形缓冲 + 总线。核心重定向的 stdout/stderr 每一行都走这里。
async fn pump_logs(
    shared: Arc<Shared>,
    mut rx: tokio::sync::broadcast::Receiver<LogLine>,
) {
    loop {
        match rx.recv().await {
            Ok(line) => shared.push_log(line).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                // 订阅落后只说明我们的环形缓冲慢，不是核心没输出；如实记下跳过的行数。
                tracing::warn!(skipped, "核心日志订阅落后，部分行未进入缓冲");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
