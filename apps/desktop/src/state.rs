//! 应用状态。
//!
//! 所有可变状态集中在一个 `Mutex<Inner>` 里，而不是拆成多个锁。
//! 原因：这些状态之间有强一致要求（例如「core 在跑」与「tun_session 存在」
//! 必须同时成立），拆锁只会把一致性责任推给调用方，而调用方是几十个命令函数，
//! 迟早会漏。
//!
//! 锁的临界区都很短（纯内存操作），命令里真正的耗时动作（启动进程、改网络）
//! 都在锁外做，避免把 UI 卡住。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use xt_core::model::{AppSettings, Node, ProxyMode, Subscription};
use xt_core::store::Store;

/// 运行中的核心/隧道运行时信息，直接给 UI 用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreRuntime {
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at_unix: Option<u64>,
    pub config_path: Option<PathBuf>,
    /// TUN 会话 id。`Some` 表示 helper 侧有一个活跃的 utun。
    pub tun_session: Option<String>,
    pub tun_interface: Option<String>,
    /// 两阶段启动里「默认路由还没接管」的状态。
    ///
    /// UI 必须显示这个状态：此时隧道已经建好但**流量还没被接管**，
    /// 用户看到「已连接」但其实没生效会非常困惑。
    pub routes_committed: bool,
    pub last_error: Option<String>,
    /// **上一次真正验证过能用的节点 id。**
    ///
    /// 为什么需要它：切换节点是整个隧道拆掉重建，而「新节点是坏的」完全可能
    /// （实测有节点 TCP 可达却转发不了流量）。没有这个记录，切换失败就只能
    /// 把用户丢在断网状态 —— 有了它就能自动退回上一个可用的节点。
    ///
    /// 只在**连通性检查通过之后**才写，所以它是「验证过的」而不是「选过的」。
    #[serde(default)]
    pub last_good_node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts_unix: u64,
    /// `core` / `app` / `helper`
    pub source: String,
    pub level: String,
    pub message: String,
}

/// 一条延迟探测结果，UI 直接渲染。
pub type LatencyTable = HashMap<String, xt_core::xray::ProbeResult>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrafficSample {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// 字节/秒，由两次采样的差值算出。
    pub rx_rate: u64,
    pub tx_rate: u64,
}

pub struct Inner {
    pub settings: AppSettings,
    pub subscriptions: Vec<Subscription>,
    pub nodes: Vec<Node>,
    pub runtime: CoreRuntime,
    pub latencies: LatencyTable,
    pub logs: VecDeque<LogEntry>,
    pub traffic: TrafficSample,
    /// 流量采样任务。跟核心一起生灭，见 `traffic.rs`。
    pub traffic_task: Option<crate::traffic::TrafficMonitor>,
    /// 更新状态（检查结果缓存）。
    pub update: UpdateStatus,
    /// DNS 探测状态。
    pub dns: DnsStatus,
    /// 最近一次探测/更新的错误，用于 UI 顶部的提示条。
    pub last_notice: Option<String>,
    /// 日志落盘目录。为什么需要落盘见 [`Inner::push_log`]。
    pub logs_dir: PathBuf,
}

/// DNS 探测状态。
#[derive(Debug, Clone, Default, Serialize)]
pub struct DnsStatus {
    /// 探测结果。国内组在前、国外组在后，各自已按「可用 + 快」排序；
    /// 界面按 `kind` 分成两块显示。
    pub probes: Vec<xt_core::dns_probe::DnsProbe>,
    /// 国内组自动选中的那台（写进 `direct_servers[0]`）。
    pub chosen: Option<String>,
    /// 国外组自动选中的那台（写进 `remote_servers[0]`）。
    pub chosen_foreign: Option<String>,
    pub probed_at: Option<u64>,
    /// 国内组探测失败的原因。
    pub error: Option<String>,
    /// 国外组探测失败/跳过的原因。**单独一条**：国外组要经节点才测得了，
    /// 它的失败和「国内解析器都不通」是两码事，混在一起会误导。
    pub foreign_error: Option<String>,
}

/// 更新相关的状态。
///
/// `latest_*` 是**上次检查的结果**，缓存起来是因为检查要联网（几秒），
/// 而界面每次快照都读它不该触发网络请求。
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateStatus {
    /// 当前生效的核心版本（可能是包内的，也可能是更新来的）。
    pub core_version: Option<String>,
    /// 是否正在使用更新下来的核心。
    pub core_managed: bool,
    pub core_managed_version: Option<String>,
    /// geo 数据来自哪次发布。包内自带的没有记录，为 `None`。
    pub geo_tag: Option<String>,
    pub geo_installed_at: Option<u64>,
    pub latest_core: Option<xt_core::update::Available>,
    pub latest_geo: Option<xt_core::update::Available>,
    /// 客户端**自己**的最新版。仓库是私有的，所以这一步需要 token。
    pub latest_app: Option<xt_core::update::Available>,
    pub checked_at: Option<u64>,
    /// 检查更新时的错误（核心 / geo / 客户端共用一条）。
    pub check_error: Option<String>,
    /// 正在进行的下载进度。`None` 表示没有在下载。
    pub progress: Option<UpdateProgress>,
}

/// 一次更新下载的进度，用于界面上的进度条。
///
/// `total` 来自 GitHub API 报的字节数；上游没报时为 `None`，界面就退化成
/// **不确定进度**（只显示已下载多少），而不是画一个假的百分比。
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateProgress {
    pub label: String,
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
}

impl Inner {
    fn new(store: &Store) -> Self {
        Self {
            settings: store.load_settings(),
            subscriptions: store.load_subscriptions(),
            nodes: store.load_nodes(),
            runtime: CoreRuntime::default(),
            latencies: HashMap::new(),
            logs: VecDeque::with_capacity(LOG_CAPACITY),
            traffic: TrafficSample::default(),
            traffic_task: None,
            update: UpdateStatus::default(),
            dns: DnsStatus::default(),
            last_notice: None,
            logs_dir: store.logs_dir(),
        }
    }

    /// 记一条日志到**内存**环形缓冲（纯内存操作，可以安全地在锁内调用）。
    ///
    /// **落盘不在这里**：写文件是阻塞 I/O，而本函数的调用点在
    /// `Mutex<Inner>` 的临界区内（本文件开头与 `AppState::inner` 都写明
    /// 「锁只覆盖纯内存操作」）。核心日志转发循环会为核心输出的每一行调用它，
    /// 一旦在锁内写盘，就会拖住所有状态读者（快照、托盘）。
    ///
    /// 需要落盘的调用方用 [`AppState::log`]，它在**锁外**写文件。
    pub fn push_log(&mut self, source: &str, level: &str, message: impl Into<String>) {
        let entry = LogEntry {
            ts_unix: now_unix(),
            source: source.to_string(),
            level: level.to_string(),
            message: message.into(),
        };
        if self.logs.len() >= LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(entry);
    }

    /// 找出当前选中的节点。
    pub fn selected_node(&self) -> Option<&Node> {
        let id = self.settings.selected_node.as_deref()?;
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// 内存里保留的日志条数。UI 只显示最近这些，更早的看日志文件。
const LOG_CAPACITY: usize = 2000;

pub struct AppState {
    pub store: Store,
    /// 领域状态。锁只覆盖纯内存操作，绝不跨 `.await` 持有。
    pub inner: Mutex<Inner>,
    /// 核心/隧道的生命周期。用 `tokio::sync::Mutex` 是因为启动流程本身是异步的，
    /// 而 `std::sync::MutexGuard` 不是 `Send`，没法安全地跨 await。
    pub supervisor: tokio::sync::Mutex<crate::supervisor::Supervisor>,
    /// 日志目录已就绪 —— 避免每条日志都做一次 `create_dir_all`。
    pub logs_dir_ready: std::sync::atomic::AtomicBool,
    /// 路由判定用的 geosite/geoip 数据。解析一次约 350ms，缓存在这里。
    pub geo: tokio::sync::Mutex<Option<std::sync::Arc<xt_core::routing::geo::GeoData>>>,
    /// helper 连接。单独的锁，避免 helper 的 IPC 拖慢 UI 状态读取。
    ///
    /// 用 `tokio::sync::Mutex` 而不是 `std::sync::Mutex`：启动流程需要在
    /// **持有 helper 的同时** await 进程启动与端口等待，而
    /// `std::sync::MutexGuard` 不是 `Send`，把它跨 await 持有会让整个
    /// Tauri 命令的 future 失去 `Send`，编译期直接报错。
    pub helper: tokio::sync::Mutex<crate::helper_client::HelperClient>,
}

impl AppState {
    pub fn new(store: Store) -> Self {
        if let Err(e) = store.ensure_dirs() {
            tracing::warn!(error = %e, "创建数据目录失败（首次写入时会重试）");
        }
        let inner = Inner::new(&store);
        Self {
            store,
            inner: Mutex::new(inner),
            supervisor: tokio::sync::Mutex::new(crate::supervisor::Supervisor::default()),
            helper: tokio::sync::Mutex::new(crate::helper_client::HelperClient::new(None)),
            logs_dir_ready: std::sync::atomic::AtomicBool::new(false),
            geo: tokio::sync::Mutex::new(None),
        }
    }

    /// 记一条日志并落盘 —— **写文件在锁外**。
    ///
    /// 这是需要持久化的调用方的入口（见 [`Inner::push_log`] 关于为什么
    /// 不能把写盘放进锁内的说明）。
    ///
    /// 消息里的换行会被转义成字面 `\n`：日志文件是 JSONL，一行必须是一条
    /// 完整记录 —— 含换行的消息会被拆成多行，每一行都解析失败、被
    /// `tail_logs` 静默丢掉（核心的多行报错正好是这种情况）。
    pub fn log(&self, source: &str, level: &str, message: impl Into<String>) {
        let message = message.into().replace('\r', "").replace('\n', "\\n");
        let logged = self.with(|i| {
            i.push_log(source, level, message.clone());
            i.logs.back().cloned()
        });
        let Some(entry) = logged.flatten() else { return };
        let Ok(line) = serde_json::to_string(&entry) else { return };
        // 已有日志文件时不再每次 create_dir_all（那是两次额外系统调用）。
        let dir_exists = self.logs_dir_ready.load(std::sync::atomic::Ordering::Relaxed);
        let written = if dir_exists {
            xt_core::store::append_log_line_existing(&self.store.logs_dir(), &line)
        } else {
            let r = xt_core::store::append_log_line(&self.store.logs_dir(), &line);
            if r.is_ok() {
                self.logs_dir_ready.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            r
        };
        if let Err(e) = written {
            tracing::warn!(error = %e, "写日志文件失败");
        }
    }

    /// 加锁并执行。锁中毒时返回 `None`，调用方给出可读错误。
    ///
    /// **不允许重入。** `std::sync::Mutex` 是非递归的：在已经持有锁的闭包里
    /// 再调用 `self.with(...)` 会永久等待自己。这条规则真的被违反过 ——
    /// `build_snapshot` 把一个内部要拿锁的 `update_status` 写进了快照闭包，
    /// 结果进程活着、连得上 helper、日志一句错都没有，界面却什么都加载不出来。
    ///
    /// 所以这里显式检测重入并 **panic**：把「静默挂起」变成一句能读的报错。
    /// 宁可炸响，也不要再让这种 bug 以「界面空白」的形式出现。
    pub fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
        thread_local! {
            static IN_WITH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        /// 保证任何退出路径（含 panic）都复位标志。
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                IN_WITH.with(|c| c.set(false));
            }
        }

        if IN_WITH.with(|c| c.replace(true)) {
            // 先复位再抛，避免后续调用被这个标志连坐。
            IN_WITH.with(|c| c.set(false));
            panic!(
                "AppState::with 重入：不能在 with 闭包内部再调用 state.with —— \
                 非递归互斥量会自死锁。请先在外面把值算好再传进闭包。"
            );
        }
        let _reset = ResetGuard;

        match self.inner.lock() {
            Ok(mut guard) => Some(f(&mut guard)),
            Err(poisoned) => {
                // 中毒说明某个命令 panic 过。数据本身没坏（都是普通结构体），
                // 所以恢复而不是直接放弃，否则用户必须重启 App。
                tracing::error!("状态锁中毒，正在恢复");
                let mut guard = poisoned.into_inner();
                Some(f(&mut guard))
            }
        }
    }
}

/// 给 UI 的完整快照。一次 IPC 拿全，避免 UI 侧拼装竞态。
#[derive(Debug, Clone, Serialize)]
pub struct AppSnapshot {
    pub settings: AppSettings,
    pub subscriptions: Vec<Subscription>,
    pub nodes: Vec<Node>,
    pub runtime: CoreRuntime,
    pub latency: LatencyTable,
    pub traffic: TrafficSample,
    pub notice: Option<String>,
    pub helper: HelperAvailability,
    pub core: CoreAvailability,
    /// 开机自启动的**真实**状态（来自系统，不是回显设置字段）。
    pub login_item: LoginItemState,
    pub update: UpdateStatus,
    pub dns: DnsStatus,
    pub app_version: String,
}

/// 登录项状态。
///
/// 之所以要从系统读而不是直接回显 `settings.launch_at_login`：
/// 用户可以在「系统设置 → 通用 → 登录项」里把这一项删掉。回显设置字段的话，
/// 界面会显示「已开启」，而实际根本不会自启 —— 一个没人会怀疑的谎。
#[derive(Debug, Clone, Default, Serialize)]
pub struct LoginItemState {
    /// `not_registered` / `enabled` / `requires_approval` / `not_found` / `error`
    pub status: String,
    /// 给用户看的一句话。
    pub detail: String,
    /// 是否需要用户去系统设置里点一下。
    pub needs_approval: bool,
}

/// helper 的可用性。
///
/// `state` 这个枚举是刻意加的。最初只有 `socket_present` + `reachable`
/// 两个布尔量，结果 UI 把「socket 文件在但没人监听」（`ECONNREFUSED`）
/// 和「根本没装」混成了同一句话 —— 而这两者需要用户做的事完全不同：
/// 一个是「去装」，另一个是「让 launchd 把它拉起来」。
#[derive(Debug, Clone, Default, Serialize)]
pub struct HelperAvailability {
    pub socket_present: bool,
    pub reachable: bool,
    pub version: Option<String>,
    pub protocol: Option<u32>,
    pub tun_active: bool,
    pub stale_session: Option<String>,
    /// 需要用户去「系统设置 → 通用 → 登录项与扩展 → 后台允许」里批准。
    pub needs_approval: bool,
    pub error: Option<String>,
    /// 精确状态，UI 据此给出**不同的**操作指引。
    pub state: HelperState,
}

/// helper 的精确状态。每种状态对应一个不同的用户动作。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// 一切正常。
    Ready,
    /// socket 文件不存在 —— 从没装过。
    NotInstalled,
    /// socket 文件在，但连接被拒 —— 守护进程没在跑（陈旧 socket，或 launchd 没拉起来）。
    /// **这个状态最常见，而且最容易修好。**
    NotRunning,
    /// 连接被拒于权限 —— 当前用户不在 admin 组。
    NotPermitted,
    /// 已安装、能连上，但被 launchd 或用户策略挡住（少见）。
    NeedsApproval,
    /// 其它错误，看 `error`。
    #[default]
    Unknown,
}

/// 内核可执行文件的可用性状态。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CoreAvailability {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub error: Option<String>,
    /// 版本是否满足原生 TUN 的最低要求（>= 26.1.31）。
    pub supports_native_tun: bool,
    pub min_native_tun_version: String,
}

/// 当前 Unix 时间戳（秒）。唯一实现在 `xt_core::util`。
///
/// 这里只做再导出：命令层与 supervisor 都按 `crate::state::now_unix` 引用它，
/// 保留这个名字可以让调用点一行都不用改，同时消除重复实现。
pub use xt_core::util::now_unix;

/// 校验设置并持久化。返回校验错误而不是静默修正 —— 用户需要知道为什么。
pub fn persist_settings(state: &AppState, settings: &AppSettings) -> Result<(), String> {
    settings.validate().map_err(|e| e.to_string())?;
    state.store.save_settings(settings).map_err(|e| e.to_string())?;
    state.with(|inner| inner.settings = settings.clone());
    Ok(())
}

/// 根据模式与内核能力推导出配置档位。
///
/// `native_tun_available` 由命令层通过 `core_version` 判定
/// （见 `xt_core::xray::MIN_CORE_VERSION_NATIVE_TUN`）。核心太老时必须走
/// 外部数据面，否则 `protocol: "tun"` 会让核心直接启动失败。
pub fn profile_for(
    settings: &AppSettings,
    physical_interface: Option<&str>,
    native_tun_available: bool,
) -> xt_core::xray::InboundProfile {
    match settings.mode {
        ProxyMode::Tun if native_tun_available => {
            xt_core::xray::InboundProfile::Tun(xt_core::xray::tun_inbound_spec(
                settings,
                physical_interface,
                // 路由由 helper 负责（它持有快照与回滚能力），所以不让 Xray 自己也装一遍：
                // 两边都装会产生「谁负责删」的歧义，而删漏的后果是永久断网。
                false,
            ))
        }
        ProxyMode::Tun => xt_core::xray::InboundProfile::TunExternalDatapath,
        _ => xt_core::xray::InboundProfile::LocalProxy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **重入 `state.with` 必须炸响，而不是静静地死锁。**
    ///
    /// 这条钉住的是一个真实事故：`build_snapshot` 把一个内部要拿锁的
    /// `update_status` 写进了快照闭包，于是 `std::sync::Mutex` 自死锁。
    /// 症状是「进程活着、连得上 helper、日志一句错都没有，界面什么都
    /// 加载不出来」—— 我在发布前完全没发现，因为它不报任何错。
    ///
    /// 现在重入会 panic。这条测试保证那个 panic 一直存在：
    /// 万一有人把守卫删了，这里会从「panic 被捕获」变成「测试挂死」，
    /// 而挂死的测试在 CI 里是超时失败，同样能被发现。
    #[test]
    fn nested_with_panics_instead_of_deadlocking() {
        let state = AppState::new(temp_store("nested-with"));
        let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.with(|_| {
                // 在持有锁的闭包里再拿一次锁 —— 真实事故就是长这样的
                let _ = state.with(|_| ());
            });
        }));
        assert!(hit.is_err(), "重入必须在 panic 里被发现，而不是死锁");
        let _ = std::fs::remove_dir_all(state.store.root());
    }

    /// 守卫用完必须复位：一次 panic 不能把后续所有调用都连坐。
    #[test]
    fn with_still_works_after_a_reentrancy_panic() {
        let state = AppState::new(temp_store("with-reset"));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.with(|_| {
                let _ = state.with(|_| ());
            });
        }));
        // 若标志没复位，这一次会直接 panic
        let v = state.with(|i| i.settings.mode);
        assert!(v.is_some(), "panic 之后 with 应当照常可用");
        let _ = std::fs::remove_dir_all(state.store.root());
    }

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("xt-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn snapshot_defaults_are_sane() {
        let store = temp_store("defaults");
        let state = AppState::new(store.clone());
        let settings = state.with(|i| i.settings.clone()).unwrap();
        assert_eq!(settings.socks_port, 10808);
        assert_eq!(settings.mode, ProxyMode::SystemProxy);
        assert!(state.with(|i| i.nodes.is_empty()).unwrap());
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// **日志必须在重启后仍然拿得到。**
    ///
    /// 这条钉住的是 0.8.3 补的能力：在此之前日志只在内存环形缓冲里，
    /// App 一重启就清空 —— 而「开机后没自动连上」要看的恰恰是启动那一刻的
    /// 日志。用户重启完打开日志页，看到的永远是启动之后的内容。
    ///
    /// 做法：写几条 → **丢掉整个 AppState**（模拟退出）→ 新建一个指向同一
    /// 数据目录的实例 → 从文件里读回来。用内存缓冲是过不了这个测试的。
    #[test]
    fn logs_survive_a_restart() {
        let store = temp_store("logs-persist");
        {
            let state = AppState::new(store.clone());
            // 用 `log()`（锁外落盘）而不是 `push_log()`（只进内存）——
            // 这条测试要验的正是**落盘**。
            state.log("app", "info", "上次退出时是连接状态，正在自动重连…");
            state.log("app", "error", "自动重连试了 24 次仍失败");
        } // state 在这里 drop —— 等价于 App 退出

        let reopened = AppState::new(store.clone());
        let from_file: Vec<LogEntry> = reopened.store.tail_logs(200);
        assert!(
            from_file.iter().any(|e| e.message.contains("正在自动重连")),
            "重启后必须还能读到上次启动的日志，实际拿到 {} 条",
            from_file.len()
        );
        assert!(from_file.iter().any(|e| e.level == "error"));
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 含换行的消息必须落成**一行**。
    ///
    /// 日志文件是 JSONL：一行必须是一条完整记录。核心的多行报错（配置片段、
    /// 栈）如果原样写进去，会被拆成多行，每一行都解析失败、被 `tail_logs`
    /// 静默丢掉 —— 恰好把最需要看的那类日志弄没了。
    #[test]
    fn multiline_messages_are_written_as_one_line() {
        let store = temp_store("logs-newline");
        let state = AppState::new(store.clone());
        state.log("core", "error", "启动失败:\n    \"port\": 10808\n    已被占用");

        let back: Vec<LogEntry> = store.tail_logs(10);
        assert_eq!(back.len(), 1, "一条消息应当只落成一行");
        assert!(back[0].message.contains("启动失败"), "内容不该丢");
        // 文件里也必须只有一行
        let text = std::fs::read_to_string(store.logs_dir().join("app.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1, "含换行的消息被拆成了多行");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn log_ring_buffer_is_bounded() {
        let store = temp_store("logs");
        let state = AppState::new(store.clone());
        state.with(|i| {
            for n in 0..(LOG_CAPACITY + 50) {
                i.push_log("test", "info", format!("line {n}"));
            }
            assert_eq!(i.logs.len(), LOG_CAPACITY);
            // 最早的那些应该被挤出去
            assert!(i.logs.front().unwrap().message.contains(&format!("line {}", 50)));
        });
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn poisoned_lock_is_recovered() {
        let store = temp_store("poison");
        let state = AppState::new(store.clone());
        // 故意在持锁时 panic，把锁搞中毒。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.with(|_| panic!("boom"));
        }));
        // 之后仍然必须能正常读写。
        assert!(state.with(|i| i.nodes.len()).is_some());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn persist_rejects_invalid_settings() {
        let store = temp_store("invalid");
        let state = AppState::new(store.clone());
        let bad = AppSettings {
            socks_port: 80, // 特权端口
            ..Default::default()
        };
        assert!(persist_settings(&state, &bad).is_err());
        assert!(persist_settings(&state, &AppSettings::default()).is_ok());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn profile_is_tun_only_in_tun_mode() {
        let mut s = AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        };
        assert!(profile_for(&s, Some("en0"), true).is_tun());
        // 核心太老时仍然进入 TUN 档位，但走外部数据面
        assert!(matches!(
            profile_for(&s, Some("en0"), false),
            xt_core::xray::InboundProfile::TunExternalDatapath
        ));
        s.mode = ProxyMode::SystemProxy;
        assert!(!profile_for(&s, Some("en0"), true).is_tun());
    }
}
