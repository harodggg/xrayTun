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
    /// 最近一次探测/更新的错误，用于 UI 顶部的提示条。
    pub last_notice: Option<String>,
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
            last_notice: None,
        }
    }

    pub fn push_log(&mut self, source: &str, level: &str, message: impl Into<String>) {
        if self.logs.len() >= LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(LogEntry {
            ts_unix: now_unix(),
            source: source.to_string(),
            level: level.to_string(),
            message: message.into(),
        });
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
        }
    }

    /// 加锁并执行。锁中毒时返回 `None`，调用方给出可读错误。
    pub fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
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

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
