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

/// 自动重建（看门狗自愈）的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOutcome {
    /// 重建成功，隧道已恢复。
    Recovered,
    /// 重建失败，已**尝试**退回直连（不再走代理）。
    ///
    /// ⚠️ **不等于「网络可用」**：这一步的结局有两种 ——
    /// 回滚成功（路由/DNS 已还原）或回滚失败（网络恢复**未经验证**，
    /// 见 `commands::FallbackOutcome`）。界面必须按结局区分文案，
    /// **不许**一律说「能上网」。
    ///
    /// 刻意不叫 `failed`：退回直连是一个**动作**，与「隧道失败」不是同一件事。
    DirectFallback,
}

/// 看门狗自动重建隧道的状态。
///
/// # 为什么需要它
///
/// 看门狗（`commands/core.rs::spawn_tunnel_watchdog`）在换网 / 熄屏唤醒 /
/// 节点抖动时**确实会自动重建**，但此前这件事**只写进了一个自由文本 notice**，
/// 而 notice 只在 `snapshot` 命令里下发 —— 界面因此看不到「正在自愈」，
/// 反而把按钮变回「连接」，用户去点就和看门狗抢。
///
/// 这个结构给出**机器可读**的状态，并且随 `CoreRuntime` 一起在
/// `runtime://changed` 事件与快照里下发（两者都传整个 `CoreRuntime`）。
///
/// # 刻意没有「预计下次重试时间」
///
/// 看门狗的探测是固定 10 秒一跳；一旦判定需要重建就**立刻**做，
/// 失败后直接退回直连并**退出**（不再重试）。也就是说后端并不知道
/// 「下次重试在什么时候」—— 编一个数字（或一个恒为 `null` 的字段）
/// 都是无意义的语义，所以它不存在。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryState {
    /// 看门狗正在重建隧道。**后端真实状态**，不是前端按时间猜的。
    pub recovering: bool,
    /// 自 App 启动以来发起自动重建的序号（含进行中的这次）；`0` = 从未发起。
    ///
    /// **刻意不持久化**：App 重启后从 `0` 重新计。这是语义的一部分、不是 bug ——
    /// 「第 N 次」指的是**这一次运行**里发生过几次自愈。
    pub attempt: u32,
    /// 触发这次重建的连续探测失败次数（看门狗每 10 秒探测一次）。
    pub probe_failures: u32,
    /// 本次自动重建的开始时刻（Unix 秒）；没发起过就是 `None`。
    pub started_unix: Option<u64>,
    /// 最近一次自动重建的结局；`None` = 还没结束过任何一次。
    pub last_outcome: Option<RecoveryOutcome>,
    /// 最近一次自动重建的结束时刻（Unix 秒）。
    pub finished_unix: Option<u64>,
}

impl RecoveryState {
    /// 同步「连续探测失败次数」。返回 `true` 表示状态变了
    /// （调用方据此决定要不要发事件，避免每 10 秒无谓地推一次）。
    pub fn set_probe_failures(&mut self, failures: u32) -> bool {
        if self.probe_failures == failures {
            return false;
        }
        self.probe_failures = failures;
        true
    }

    /// 开始一次自动重建。返回本次的序号（第 N 次，从 1 开始）。
    pub fn begin(&mut self, now_unix: u64) -> u32 {
        self.attempt = self.attempt.saturating_add(1);
        self.recovering = true;
        self.started_unix = Some(now_unix);
        self.attempt
    }

    /// 重建成功：隧道已恢复。
    pub fn succeeded(&mut self, now_unix: u64) {
        self.recovering = false;
        self.probe_failures = 0;
        self.last_outcome = Some(RecoveryOutcome::Recovered);
        self.finished_unix = Some(now_unix);
    }

    /// 重建失败：已退回直连。
    pub fn fell_back_to_direct(&mut self, now_unix: u64) {
        self.recovering = false;
        self.probe_failures = 0;
        self.last_outcome = Some(RecoveryOutcome::DirectFallback);
        self.finished_unix = Some(now_unix);
    }
}

/// 自动恢复期间写进 `last_notice` 的文案（快照顶部的提示条）。
pub const RECOVERING_NOTICE: &str = "网络中断，正在自动恢复…";

/// 自动恢复**成功**后清掉恢复中的提示条。
///
/// # 为什么必须清
///
/// `last_notice` 会随快照下发。成功后不清，下一次刷新就会把过期的
/// 「正在自动恢复…」带回来（还带一个无事可做的按钮）—— 那就从
/// 「该显示恢复时看不见」变成「恢复完了还一直显示恢复中」。
///
/// # 为什么只清那一条
///
/// 同一个字段还被别的流程使用（例如「检测到上次异常退出，正在修复网络配置…」）。
/// 无差别清空会把别人的提示一起抹掉，所以这里**按内容比对**，只清我们自己写的。
pub fn clear_recovering_notice(notice: &mut Option<String>) {
    if notice.as_deref() == Some(RECOVERING_NOTICE) {
        *notice = None;
    }
}

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
    /// 自动恢复（看门狗重建）的状态。界面据此显示「正在自动恢复（第 N 次）」，
    /// 并在恢复期间禁用/改写连接按钮，避免和看门狗抢。
    ///
    /// 放在 `CoreRuntime` 里而不是事件载荷顶层：`CoreRuntime` 是**整体**
    /// 在事件与快照两条路上传的，这样刷新快照时恢复状态不会丢。
    #[serde(default)]
    pub recovery: RecoveryState,
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
    /// 连接日志：按出口 tag 的连接数 + 最近若干条**结构化连接**（环形缓冲）。
    ///
    /// # 为什么需要它
    ///
    /// `dns-out`（UDP）与 `api`（本机回环）的**字节**计数器恒为 0 —— 那是
    /// 测量盲区，不是事实：本机实测它们各有 4769 / 5374 条连接。界面若只显示
    /// `0 B`，用户会以为「这两个出口没在用」。
    ///
    /// 连接数是这两类出口**唯一可得**的活跃度指标；结构化记录则支撑
    /// 「单连接可视化」（点一条连接 → 在拓扑上高亮它的入口/出口）。
    ///
    /// # 拿不到的字段（硬约束）
    ///
    /// 访问日志只记连接**建立**（`accepted`），没有每连接字节数、没有结束时间、
    /// 没有连接 ID；域名靠 `sniffed` 时序配对，是**近似**。
    /// 详见 [`xt_core::xray::access_log`] 模块头注释。
    pub connections: xt_core::xray::access_log::ConnectionLog,
    /// 意图过滤的运行态（判定引擎 + 缓存 + 审计）。
    ///
    /// **它不下发路由规则、不碰系统网络配置**：本版只观察与判定，见
    /// [`crate::intent`] 的模块文档。所以把它挂在 `Inner` 上是安全的 ——
    /// 数据面路径（核心日志转发）只做一次 `observe`，判定在后台节拍里跑。
    pub intent: crate::intent::IntentRuntime,
    /// MITM 通道的运行态（P4 第四步）：代理句柄 + 本会话 CA。
    ///
    /// **只活在内存里**，不落盘：CA 是每次启动新生成的（见 `crate::mitm` 的文档）。
    pub mitm: crate::mitm::MitmRuntime,
    /// **本次连接实际使用的节点 id**（自动回落之后可能与
    /// `settings.selected_node` 不同）。
    ///
    /// # 为什么单独存，而不是改 `settings.selected_node`
    ///
    /// 回落是「这一会话先用别人顶上」，**不是**替用户改选择：悄悄改掉
    /// `selected_node` 会让用户下次点连接时不知道自己在用哪个节点。
    /// 但「现在跑的到底是哪个」必须如实可查 —— 否则连通性检查会拿着
    /// 用户选中的（而不是实际跑着的）节点去核对，把验证结果记到错的节点头上。
    ///
    /// 不放进 `CoreRuntime`：那边是**序列化给前端**的契约，
    /// 加字段会破坏 `tests/type_contract.rs`（Rust 不许提供前端未声明的字段）。
    pub active_node: Option<String>,
    /// 每个节点**连续失败**的次数（成功即清零）。
    ///
    /// 用途：同一节点连续失败达到
    /// [`crate::node_health::CONSECUTIVE_FAILURES_BEFORE_SUB_REFRESH`] 且它来自订阅时，
    /// 提示「重拉订阅」（见 `node_health::subscription_refresh_hint`）。
    pub node_fail_streak: HashMap<String, u32>,
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
    /// 客户端**自己**的最新版。
    ///
    /// **有这个字段不等于有新版可装** —— 它是「GitHub 上的最新版」，
    /// 查到了就一定有值。界面要判断是否该显示「更新」按钮，请看
    /// [`UpdateStatus::app_update_available`]。
    pub latest_app: Option<xt_core::update::Available>,

    /// 是否**确实**有比当前版本更新的客户端版本。
    ///
    /// 由后端用 `xt_core::update::compare_versions` 算出（逐段数值比较，
    /// `0.9.0 > 0.10.0` 这类字符串比较会判错的情况由它负责）。
    ///
    /// 为什么不让前端自己比：版本比较是**一处实现、一处测试**的逻辑，
    /// 而且 `latest_app` 一定有值是常态（你装的就是最新版时也是），
    /// 前端只看它的存在性就会永远显示「更新」按钮 —— 实测踩过：
    /// 客户端与 GitHub 都是 0.8.23 时，按钮仍然出现、点了白跑一趟。
    #[serde(default)]
    pub app_update_available: bool,
    pub checked_at: Option<u64>,
    /// 检查更新时的错误（**派生字段**）：按 客户端 → 核心 → geo 取**第一条非空**的子系统错误。
    ///
    /// **为什么是派生而不是「谁最后写谁赢」**：它以前是三路共写的一条 ——
    /// 于是 ① 客户端复查成功（`check_error.take()`）会把**核心**的失败一起清掉；
    /// ② 客户端与核心**同时**失败时只留最后一次写入。两次都是**静默少报**。
    /// 改成派生后「任一路成功都不清除别人的失败」由构造保证，不需要每个写入点
    /// 各自记住一条非本地的不变量（`version_check::refresh_merged_error` 是唯一写点）。
    ///
    /// 界面上它只用于「更新检查有没有问题」的汇总；判断**客户端**有没有新版请看
    /// [`UpdateStatus::check_error_app`] —— 核心/geo 的失败会让本字段非空，
    /// 但**不改变**客户端结论。
    pub check_error: Option<String>,
    /// **客户端专属**的检查错误：只由 `check_app_update` 与自动检测写入
    /// （两条路共用 `version_check::apply_app_check_result`）。
    ///
    /// 核心 / geo 的失败**绝不**写这里（`apply_core_geo_check_result` 只写
    /// `check_error_core` / `check_error_geo` / `checked_at` / `latest_core` / `latest_geo`），
    /// 由 version_check.rs 的单测钉住。
    pub check_error_app: Option<String>,
    /// **核心专属**的检查错误：只由 `check_updates` 的核心那一支写入（geo 的失败不写这里）。
    pub check_error_core: Option<String>,
    /// **geo 专属**的检查错误：只由 `check_updates` 的 geo 那一支写入。
    ///
    /// 这一格是补出来的缺口：此前 geo 的 `Err` **没有任何字段可承载** ⇒
    /// geo 检查失败在界面上**永远看不到**（`apply_core_geo_check_result` 只在 `Ok` 分支处理 geo）。
    pub check_error_geo: Option<String>,
    /// **客户端专属**的上次检查时刻：只由客户端检查写入。
    ///
    /// `checked_at` 仍是三路共用的旧字段（核心/geo 也会写，语义未变）；
    /// 它**不能**代表「客户端上次检查时刻」。
    pub checked_at_app: Option<u64>,
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

impl UpdateStatus {
    /// **下载终态**：清掉进度。
    ///
    /// # 为什么必须由每条更新路径的**每个出口**调用（A17，task-153）
    ///
    /// 界面判断「正在下载」的判据就是 **`progress !== null`**
    /// （`Settings.tsx:307` 的 `downloading`）—— 它同时驱动
    /// 「下载中，请勿关闭…」那条提示与**升级按钮的禁用**。
    /// 所以只要有**一个出口**忘了收尾，用户就会永久看到那句假指令、并且
    /// **再也点不到升级**（只有重启 App 才恢复）。
    ///
    /// geo 路径原来一个出口都没收尾，正是那条死路。
    pub(crate) fn finish_download(&mut self) {
        self.progress = None;
    }
}

/// **进度收尾守卫**：`Drop` 时清掉下载进度。
///
/// # 为什么是守卫，而不是在每个出口手写一句（task-158 的机制）
///
/// 界面判据是 `progress !== null`（同时驱动「下载中，请勿关闭…」与**升级按钮禁用**）。
/// 三条更新路径各有 3~5 个出口（成功 / join 失败 / `user_msg` 错误 / 写脚本失败 /
/// 启动脚本失败 / 提前 `return`），**逐个记住去清迟早会漏**：
/// A17 是「geo 整条路径一个出口都没清」，`task-158` 是「core 的失败出口漏了」。
/// 守卫靠 `Drop` —— `?`、`return`、甚至 panic unwind 都绕不过它。
///
/// 用法：建了 `progress_reporter` 之后立刻 `let progress = ProgressGuard::new(&state);`，
/// 并在 `build_snapshot(...)` **之前** `drop(progress);`（否则返回给界面的那份快照里
/// 还带着进度；后面那次轮询才会消失 —— 那就又变成「一会儿假下载中」了）。
pub(crate) struct ProgressGuard<'a> {
    state: &'a AppState,
}

impl<'a> ProgressGuard<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        self.state.with(|i| i.update.finish_download());
    }
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
            connections: xt_core::xray::access_log::ConnectionLog::new(),
            intent: crate::intent::IntentRuntime::new(store.root().to_path_buf()),
            mitm: crate::mitm::MitmRuntime::default(),
            active_node: None,
            node_fail_streak: HashMap::new(),
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

    /// 意图过滤的后台节拍：判定积压的候选、周期性落盘。
    ///
    /// 由 `lib.rs` 的一个 10 秒定时任务调用。**没有引擎时是空操作**，
    /// 所以"功能没开"与"引擎建不起来"这两种状态都不需要额外分支。
    ///
    /// 返回这一轮判定的一轮账（日志用）；没引擎时 `None`。
    pub fn tick_intent(&self, now: u64) -> Option<xt_intent::engine::ClassifyReport> {
        let report = self.with(|i| i.intent.tick(now))?;
        if let Some(r) = &report {
            // 只在这轮真的做了事的时候记日志 —— 每 10 秒一条空账会把日志刷成噪音。
            if r.asked > 0 || r.blocked > 0 || r.gateway_errors > 0 || r.budget_denied > 0 {
                self.log(
                    "intent",
                    "info",
                    format!(
                        "意图判定：问 {} 次（缓存命中 {}）⇒ 拦 {} / 放行 {} / 延后 {}，网关错误 {}，预算拒绝 {}",
                        r.asked, r.cache_hits, r.blocked, r.allowed, r.deferred, r.gateway_errors, r.budget_denied
                    ),
                );
            }
        }
        report
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
    /// 所以这里显式检测重入，把「静默挂起」变成一句能读的 `error` 日志并
    /// **返回 `None`**。**不能 panic**：本 App 的 release profile 是
    /// `panic = "abort"`，一个编程错误不该以 SIGABRT / 界面闪退的形式落到
    /// 用户头上（0.8.38 事故就是这个形态）。`None` 与「锁中毒」同义，
    /// 调用方都按「拿不到状态」走已有的可读错误路径。
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
            // 先复位再返回，避免后续调用被这个标志连坐。
            IN_WITH.with(|c| c.set(false));
            tracing::error!(
                "AppState::with 重入：不能在 with 闭包内部再调用 state.with —— \
                 非递归互斥量会自死锁。请先在外面把值算好再传进闭包。\
                 （已降级为返回 None，调用方按「拿不到状态」处理）"
            );
            return None;
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
    /// 已安装助手 vs App 包内助手的**版本对照**（task-84）。
    ///
    /// 与 `version` 的区别：`version` 是**运行中**助手在 HELLO 里自报的版本
    /// （需要守护进程在跑）；这里是**磁盘上那两个工件**各自自报的版本，
    /// 磁盘上没有也能读（守护进程没跑也照样能判断「装了的是不是随包的那份」）。
    pub version_check: HelperVersionCheck,
}

/// 已安装 helper 与 App 包内 helper 的**版本对照**（task-84）。
///
/// # 为什么需要它
///
/// **App 更新不会刷新特权 helper**：`restart_helper` 只是 `kickstart` 已经装在
/// 磁盘上的那份二进制，只有 `install_helper` 才会把**包内**那份拷过去。
/// 而路由/DNS 的安装与回滚都发生在 helper 里 —— 于是用户「更新到最新版」之后，
/// helper 侧那一部分修复**一点都没生效，而且完全无声**。
///
/// # 三态必须分开
///
/// 「读不到」**不许猜成「不一致」**：那会让用户去重装一个本来没问题的助手，
/// 属于本项目最忌讳的「用没验证的事吓人」；反过来，「一致」时**不许提示任何东西**，
/// 否则就是狼来了。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HelperVersionCheck {
    /// 两边都读到了，且**协议号相等** → **界面不该提示**。
    ///
    /// 判据是**协议号**（`commands/helper.rs::helper_versions_are_compatible`），
    /// **包版本不同不算不一致**：App 0.8.34 + 已装 helper 0.8.33、协议同为 1 ⇒ 就是这里。
    /// `version` 报的是**已安装**（实际在跑）那份的**包版本**。
    Match { version: String },
    /// 兼容性判据不满足 → 提示 + 「重新安装助手」入口。
    ///
    /// 两种情形都落到这里，**都不能再由包版本推出**：
    /// * 两边**协议号都读到了、但不相等**（此时包版本可以相同）；
    /// * **至少一边协议号读不到**（老二进制没有 `(protocol N)`）**且**包版本也不同
    ///   —— 读不到时保守退回「包版本相等」，只有包版本也不同才判不一致。
    Mismatch {
        /// 磁盘上安装的那份（`/Library/PrivilegedHelperTools/…`）自报的版本。
        installed: String,
        /// App 包内那份自报的版本。
        bundled: String,
    },
    /// **至少一边读不到** → 如实降级：不提示重装，但把读到的部分带着。
    Unreadable {
        installed: Option<String>,
        bundled: Option<String>,
        /// 为什么读不到（给人看的）。
        reason: String,
    },
}

impl Default for HelperVersionCheck {
    fn default() -> Self {
        // 默认是「还没查过」。**不能默认成 `Match`** —— 那等于在没有证据时宣布一致。
        Self::Unreadable {
            installed: None,
            bundled: None,
            reason: "尚未检查".to_string(),
        }
    }
}

/// helper 的精确状态。每种状态对应一个不同的用户动作。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// 一切正常。
    Ready,
    /// **安装产物**（`/Library/LaunchDaemons/com.xraytun.helper.plist` 或
    /// `/Library/PrivilegedHelperTools/com.xraytun.helper`）不存在 —— 从没装过。
    ///
    /// ⚠️ **不是**「socket 文件不存在」：socket 由守护进程 `serve()` 启动时 bind、
    /// 退出时删除（`crates/xt-helper/src/server.rs:114`、`:120-125`），
    /// 「装了但没跑」时它同样是 false（task-183 修正的正是这个假设）。
    NotInstalled,
    /// **已安装但守护进程没在跑**：socket 不在（每次退出都会被删）或连接被拒。
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

    /// **重入 `state.with` 必须被抓住，而不是静静地死锁。**
    ///
    /// 这条钉住的是一个真实事故：`build_snapshot` 把一个内部要拿锁的
    /// `update_status` 写进了快照闭包，于是 `std::sync::Mutex` 自死锁。
    /// 症状是「进程活着、连得上 helper、日志一句错都没有，界面什么都
    /// 加载不出来」—— 我在发布前完全没发现，因为它不报任何错。
    ///
    /// task-1 之后判据变成：重入**立即返回 `None`**（外层因此是 `Some(None)`），
    /// 既不死锁、也不 panic —— release profile 是 `panic = "abort"`，
    /// 在这里 panic 等于让用户看到 SIGABRT 而不是这句能读的报错。
    ///
    /// 判别性：把守卫删掉 ⇒ 内层 `with` 会永久等自己 ⇒ `recv_timeout` 超时 ⇒ 红；
    /// 把降级改回 `panic!` ⇒ 线程结束但没有值发回 ⇒ 同样超时/断言红。
    #[test]
    fn nested_with_reports_error_without_panicking_or_deadlocking() {
        let state = std::sync::Arc::new(AppState::new(temp_store("nested-with")));
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let state = state.clone();
            std::thread::spawn(move || {
                // 在持有锁的闭包里再拿一次锁 —— 真实事故就是长这样的
                let out = state.with(|_| state.with(|_| ()));
                let _ = tx.send(out);
            });
        }
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(out) => assert_eq!(
                out,
                Some(None),
                "重入必须降级成 `None`（可读错误），不许 panic、也不许死锁"
            ),
            Err(_) => panic!("重入既没返回值也没报错：守卫可能被删了（现在会自死锁）"),
        }
        let _ = std::fs::remove_dir_all(state.store.root());
    }

    /// 守卫用完必须复位：一次重入不能把后续所有调用都连坐。
    #[test]
    fn with_still_works_after_a_reentrancy() {
        let state = AppState::new(temp_store("with-reset"));
        let nested = state.with(|_| state.with(|_| ()));
        assert_eq!(nested, Some(None), "重入这一跳必须被识别出来");
        // 若标志没复位，这一次会直接返回 None
        let v = state.with(|i| i.settings.mode);
        assert!(v.is_some(), "重入之后 with 应当照常可用");
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

    // -----------------------------------------------------------------------
    // 自动恢复状态机（task-22）
    //
    // 界面依赖这三个状态做文案与按钮语义，所以状态转换必须是**纯函数、
    // 可单测**的；看门狗里那部分异步装配由这些方法驱动。
    // -----------------------------------------------------------------------

    /// 初始状态：没在恢复、没发起过、没有结局。
    #[test]
    fn recovery_starts_idle() {
        let r = RecoveryState::default();
        assert!(!r.recovering);
        assert_eq!(r.attempt, 0);
        assert_eq!(r.probe_failures, 0);
        assert_eq!(r.started_unix, None);
        assert_eq!(r.last_outcome, None);
        assert_eq!(r.finished_unix, None);
    }

    /// 探测失败次数只在**变化**时报告变化 —— 否则看门狗每 10 秒都要推一次事件。
    #[test]
    fn probe_failure_count_only_reports_changes() {
        let mut r = RecoveryState::default();
        assert!(r.set_probe_failures(1), "0 → 1 是变化");
        assert!(!r.set_probe_failures(1), "1 → 1 不是变化");
        assert!(r.set_probe_failures(2), "1 → 2 是变化");
        assert!(r.set_probe_failures(0), "恢复到 0 也是变化（界面要收回告警）");
        assert!(!r.set_probe_failures(0));
    }

    /// **看门狗触发 → 状态变 recovering → 成功** 这条主路径。
    #[test]
    fn watchdog_recovery_is_visible_then_finishes_as_recovered() {
        let mut r = RecoveryState::default();
        r.set_probe_failures(1);
        r.set_probe_failures(2);

        let attempt = r.begin(1_000);
        assert_eq!(attempt, 1, "第一次自动重建的序号是 1");
        assert!(r.recovering, "重建期间必须是 recovering —— 界面据此禁用连接按钮");
        assert_eq!(r.started_unix, Some(1_000));
        assert_eq!(r.probe_failures, 2, "保留触发这次重建的失败次数");
        assert_eq!(r.last_outcome, None, "还没结束，不能提前写结局");

        r.succeeded(1_030);
        assert!(!r.recovering);
        assert_eq!(r.last_outcome, Some(RecoveryOutcome::Recovered));
        assert_eq!(r.finished_unix, Some(1_030));
        assert_eq!(r.probe_failures, 0, "恢复后失败计数归零");
    }

    /// 失败路径：退回直连，结局与成功**可区分**（界面文案不同）。
    #[test]
    fn watchdog_recovery_failure_is_recorded_as_direct_fallback() {
        let mut r = RecoveryState::default();
        r.set_probe_failures(2);
        assert_eq!(r.begin(2_000), 1);
        assert!(r.recovering);

        r.fell_back_to_direct(2_040);
        assert!(!r.recovering);
        assert_eq!(r.last_outcome, Some(RecoveryOutcome::DirectFallback));
        assert_eq!(r.finished_unix, Some(2_040));
        assert_ne!(
            r.last_outcome,
            Some(RecoveryOutcome::Recovered),
            "失败与成功必须可区分，不能都只报「结束了」"
        );
    }

    /// **重建成功必须清掉恢复中的提示条**（product-manager 实测的缺陷：
    /// 不清的话下一次快照刷新会把「正在自动恢复…」带回来，
    /// 于是变成「恢复完了还一直显示恢复中」）。
    ///
    /// 同时锁住「只清我们写的那条」：别的 notice 不能被误清。
    #[test]
    fn recovery_success_clears_only_our_own_notice() {
        let mut ours = Some(RECOVERING_NOTICE.to_string());
        clear_recovering_notice(&mut ours);
        assert_eq!(ours, None, "恢复成功后不得留下过期的「正在自动恢复」");

        let mut unrelated = Some("检测到上次异常退出，正在修复网络配置…".to_string());
        clear_recovering_notice(&mut unrelated);
        assert_eq!(
            unrelated.as_deref(),
            Some("检测到上次异常退出，正在修复网络配置…"),
            "别的模块写的 notice 不能被顺手清掉"
        );

        // 夹具用**现在的生产文案**（回滚失败那一种）：它与本测试的目的无关，
        // 但别引用已经删掉的旧文案 —— 那会让后来人以为旧文案还在用。
        let mut unverified = Some(
            "自动恢复失败，回退直连未完成：**未能确认网络已恢复**（helper 回滚失败）。\
             请点「修复网络」重试回滚"
                .to_string(),
        );
        clear_recovering_notice(&mut unverified);
        assert!(unverified.is_some(), "失败时 notice 要保留并说清下一步");

        let mut empty: Option<String> = None;
        clear_recovering_notice(&mut empty);
        assert_eq!(empty, None);
    }

    /// 第二次自动重建拿到序号 2（界面显示「第 N 次」的依据）。
    #[test]
    fn second_recovery_gets_the_next_ordinal() {
        let mut r = RecoveryState::default();
        assert_eq!(r.begin(100), 1);
        r.succeeded(110);
        assert_eq!(r.begin(200), 2);
        assert!(r.recovering);
        assert_eq!(r.attempt, 2, "第 N 次是累计序号，不是每次都从 1 开始");
        assert_eq!(r.started_unix, Some(200), "开始时刻更新为本次");
        assert_eq!(r.last_outcome, Some(RecoveryOutcome::Recovered), "上一次的结局保留");
    }

    /// 序列化给前端的**字段名**就是契约（`apps/ui/src/types.ts` 手写）。
    /// 顺带把「刻意没有 next_retry」钉进契约：谁想加一个恒为 null 的字段，
    /// 就得先改这条测试并想清楚它到底有没有真实语义。
    #[test]
    fn recovery_state_wire_shape_is_deliberate() {
        let json = serde_json::to_value(RecoveryState::default()).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("应当是 JSON 对象")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "attempt",
                "finished_unix",
                "last_outcome",
                "probe_failures",
                "recovering",
                "started_unix",
            ],
            "字段名/数量变了就要同步 apps/ui/src/types.ts 的 RecoveryState；\
             并且想清楚新字段是不是后端真的知道"
        );
        // 结局是字符串枚举，不是自由文本 —— 前端不用猜文案。
        assert_eq!(
            serde_json::to_value(RecoveryOutcome::DirectFallback).unwrap(),
            serde_json::json!("direct_fallback")
        );
        assert_eq!(
            serde_json::to_value(RecoveryOutcome::Recovered).unwrap(),
            serde_json::json!("recovered")
        );
    }
}
