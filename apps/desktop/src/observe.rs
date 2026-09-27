//! App 侧的观察汇总：把 `xt-mitm` 的「只观察、不改写」摘要**按域名聚合成结论**。
//!
//! # 为什么不能只靠日志
//!
//! `xt_mitm::DomainObserver` 已经会按交换写一条 `tracing::info!` 摘要（那是"证据"），
//! 但日志是**逐条**的，用户问的是**按域名的结论**：「哪个站点有广告标记、哪个词命中几次」。
//! 本模块只做那一步聚合 —— 它**不碰网络、不改写任何字节**，也不复制一份日志通道：
//! 摘要仍然由 `DomainObserver` 走既有的 `tracing` 写出去（见 [`RecordingObserver`]）。
//!
//! # 隐私口径（与 `xt-mitm` 的模块文档一字不改地对齐）
//!
//! * 这里聚合的只有 [`xt_mitm::ExchangeRecord`] 里的字段：host / 方法 / 去 query 的
//!   path（截断）/ 状态码 / content-type / 体字节数 / 短哈希 / 标记词计数；
//! * **完整 URL、query、正文**都不进这份聚合，更不会发给界面；
//! * 完整 body 是否落盘由 `xt-mitm` 决定（默认不落盘，且只接受绝对路径）。
//!
//! # 诚实边界
//!
//! 「没有摘要」**不等于**「没有广告」：观察只覆盖 HTTP/1.1 明文可见的那部分流量
//! （盲点清单见 `xt_mitm::observe` 的模块文档与界面文案）。所以报告里
//! [`ObserveReport::hosts`] 为空只说明**没有采到证据**，界面必须如实转述这一点。
//!
//! # 留档与导出（只写摘要）
//!
//! 运行期的结论默认只活在内存里，重启即丢。本模块把**同一份结论**写成
//! 数据目录下的固定文件 [`OBSERVE_REPORT_FILE`]（[`ObserveArchive::maybe_write`]
//! 节流写 + 停止时强制写），并且让「导出」走**同一个** [`ObserveArchive::document`]
//! —— 导出的东西必须就是留档的东西。
//!
//! 文档里只有摘要：域名、条数、标记词命中、时间、body 短哈希、以及一个口径头
//! （schema / 版本 / 起止时间 / 观察了哪些域名 / 词表是什么）。**没有完整 URL、没有
//! query、没有正文**。默认关（零摘要）时**一个文件都不产生**。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use xt_core::model::MitmSettings;
use xt_mitm::{
    default_markers, DomainObserver, ExchangeRecord, MarkerHit, ObserveConfig, Observer,
};

/// 数据目录下的固定留档文件（**只存摘要，绝不存正文**）。
pub const OBSERVE_REPORT_FILE: &str = "observe-report.json";

/// 留档文件的 schema 版本（口径头的一部分：字段/口径变了它必须跟着升）。
pub const OBSERVE_REPORT_SCHEMA: &str = "xraytun.observe-report.v1";

/// 留档/导出文档里的隐私口径；**逐字写进文件**，这样留档被转手时仍带着口径。
pub const OBSERVE_PRIVACY_NOTE: &str =
    "只含摘要：域名、条数、标记词命中、时间、body 短哈希；不含完整 URL、query 或正文。";

/// 节流：运行期最多每这么多秒把摘要落一次盘（第一次是立刻写）。
const OBSERVE_PERSIST_INTERVAL_SECS: u64 = 5;

/// 每个域名最多留多少个去重后的 body 短哈希（有界：不能让它无界增长）。
const MAX_BODY_HASHES_PER_HOST: usize = 64;

/// 单个域名的观察汇总（界面上的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostObservation {
    /// 域名。**保留它**：不说域名，观察结果就没有可执行性。
    pub host: String,
    /// 这个域名上写过摘要的交换条数。
    pub exchanges: u64,
    /// 所有标记词在这个域名上的命中总数。
    pub marker_total: u64,
    /// **只列命中的词**（`promoted × 3` 这种）；一个都没命中就是空数组，
    /// 而不是替用户下"干净"的结论。
    pub markers: Vec<MarkerHit>,
    /// 去重后的 body **短哈希**（最多 [`MAX_BODY_HASHES_PER_HOST`] 个）。
    ///
    /// 它是内容指纹（FNV-1a 64 位，非密码学），用来判断"同一条响应被重复请求"；
    /// 它**不是**正文，也无法还原正文。留档里带它，是为了"看到了什么"能复核。
    #[serde(default)]
    pub body_hashes: Vec<String>,
    /// 最近一次观察到这个域名的时间（Unix 秒；`0` = 还没观察到）。
    pub last_seen_unix: u64,
}

/// 给界面看的观察报告（`MitmStatus::observe`）。
///
/// 它同时带上**配置**与**已经采到的数据**，因为两者是不同的事实：
/// `enabled=true, hosts=[]` 与 `enabled=true, hosts=["a"]` 都还没有数据，
/// 但原因完全不同，界面必须能分开说。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObserveReport {
    /// 配置里开着（≠ 采到过任何东西）。
    pub enabled: bool,
    /// 配置里的 opt-in 名单原样回传。**空 = 一条摘要都不会写。**
    pub configured_hosts: Vec<String>,
    /// 实际用于计数的词表（来自设置；缺省 = `xt_mitm::default_markers()`）。
    pub markers: Vec<String>,
    /// **这份结论有没有统计标记词**。
    ///
    /// `false` = 用户把词表清空了（`Some(vec![])`）：仍然按名单采条数与短哈希，
    /// 但命中数恒为 0。界面**必须**据此说"不统计任何标记词"，**不许**把它
    /// 渲染成"没有命中 / 干净"—— 那是把"没测量"讲成"测出来是 0"。
    pub marker_counting: bool,
    /// 已写摘要的交换总条数（**零就是零**）。
    pub exchanges: u64,
    /// 所有域名的标记词命中总数。
    pub marker_total: u64,
    /// 每个域名一条，按命中总数降序、再按域名升序。
    pub hosts: Vec<HostObservation>,
    /// body 落盘目录（配置值；`None` = 不落盘）。
    pub capture_body_dir: Option<String>,
    /// 配置里的问题（例如落盘目录是相对路径 ⇒ 已被拒绝并降级为不落盘）。
    pub note: Option<String>,
}

/// 观察汇总的累加器。内部是 `Mutex`：`Observer` 要求的 `Send + Sync` 靠它满足。
///
/// 用 `BTreeMap` 而不是 `HashMap`：报告要稳定有序（界面与测试都按域名读），
/// 顺便省掉一次排序的 key 复制。
#[derive(Debug, Default)]
pub struct ObserveLedger {
    inner: Mutex<LedgerInner>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    exchanges: u64,
    marker_total: u64,
    by_host: BTreeMap<String, HostObservation>,
    /// 这一轮观察的第一条摘要时间（`None` = 还没采到任何东西）。
    started_unix: Option<u64>,
    /// 这一轮观察的**结束**时间（`None` = 还在进行 / 还没停过）。
    ended_unix: Option<u64>,
}

/// 累加器的一份快照。落档、导出与界面报告都从这**同一份数据**渲染出来。
#[derive(Debug, Clone, Default)]
pub struct LedgerSnapshot {
    pub exchanges: u64,
    pub marker_total: u64,
    /// 每个域名一条，命中多的在前、同分按域名升序（顺序稳定）。
    pub hosts: Vec<HostObservation>,
    pub started_unix: Option<u64>,
    pub ended_unix: Option<u64>,
}

impl ObserveLedger {
    /// 记一条**已经在名单内**的交换（调用方负责过 [`Observer::observes`] 闸门）。
    pub fn record(&self, record: &ExchangeRecord) {
        let mut g = self.lock();
        let now = xt_core::util::now_unix();
        if g.started_unix.is_none() {
            g.started_unix = Some(now);
        }
        g.exchanges += 1;
        g.marker_total += record.marker_total as u64;
        let entry = g
            .by_host
            .entry(record.host.clone())
            .or_insert_with(|| HostObservation {
                host: record.host.clone(),
                exchanges: 0,
                marker_total: 0,
                markers: Vec::new(),
                body_hashes: Vec::new(),
                last_seen_unix: 0,
            });
        entry.exchanges += 1;
        entry.marker_total += record.marker_total as u64;
        for hit in record.marker_hits.iter().filter(|h| h.count > 0) {
            match entry.markers.iter_mut().find(|m| m.marker == hit.marker) {
                Some(existing) => existing.count += hit.count,
                None => entry.markers.push(hit.clone()),
            }
        }
        // 短哈希去重 + 有界：它只是"同一条响应被重复请求"的判据，不是正文。
        if !record.body_hash.is_empty()
            && !entry.body_hashes.iter().any(|h| h == &record.body_hash)
            && entry.body_hashes.len() < MAX_BODY_HASHES_PER_HOST
        {
            entry.body_hashes.push(record.body_hash.clone());
        }
        entry.last_seen_unix = now;
    }

    /// 累加器的快照（落档 / 导出 / 界面报告三处共用，避免各排一遍序）。
    pub fn snapshot(&self) -> LedgerSnapshot {
        let g = self.lock();
        let mut hosts: Vec<HostObservation> = g.by_host.values().cloned().collect();
        // 命中多的排前面；一样多时按域名，保证顺序稳定。
        hosts.sort_by(|a, b| {
            b.marker_total
                .cmp(&a.marker_total)
                .then_with(|| a.host.cmp(&b.host))
        });
        LedgerSnapshot {
            exchanges: g.exchanges,
            marker_total: g.marker_total,
            hosts,
            started_unix: g.started_unix,
            ended_unix: g.ended_unix,
        }
    }

    /// 停代理时调用：记下**结束时间**（只在真的采到过东西时才有意义）。
    pub fn mark_stopped(&self) {
        let mut g = self.lock();
        if g.started_unix.is_some() && g.ended_unix.is_none() {
            g.ended_unix = Some(xt_core::util::now_unix());
        }
    }

    /// **清空**内存结论（"清空"按钮）。
    ///
    /// 就地清空而不是换一个 `Arc`：跑着的观察者持有的是同一个 `Arc`，
    /// 换掉它会让"清空"在运行期看起来生效、实际旧账还在被继续写。
    pub fn clear(&self) {
        let mut g = self.lock();
        *g = LedgerInner::default();
    }

    /// 渲染成界面用的报告。`settings` 提供配置事实，累加器提供数据事实。
    pub fn report(&self, settings: &MitmSettings) -> ObserveReport {
        let snap = self.snapshot();
        let markers = effective_markers(settings);
        ObserveReport {
            enabled: settings.observe.enabled,
            configured_hosts: settings.observe.hosts.clone(),
            marker_counting: !markers.is_empty(),
            markers,
            exchanges: snap.exchanges,
            marker_total: snap.marker_total,
            hosts: snap.hosts,
            capture_body_dir: settings
                .observe
                .capture_body_dir
                .as_ref()
                .map(|p| p.display().to_string()),
            note: observe_note(settings),
        }
    }

    /// 有没有采到任何摘要（用于测试与诊断；界面按 `hosts` 判空）。
    pub fn is_empty(&self) -> bool {
        self.lock().exchanges == 0
    }

    /// 锁中毒**不 panic**：观察是"只观察"，没有资格因为一个 panic 把状态读取也弄挂。
    fn lock(&self) -> MutexGuard<'_, LedgerInner> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// 产品观察者：**先聚合**（给界面看的结论）→ **再委托** [`DomainObserver`]（走 tracing 日志）。
///
/// 顺序是刻意的：聚合是纯内存操作，不可能因为日志/落盘出问题而丢掉结论；
/// 而日志与落盘各自都有 fail-open 兜底（见 `xt_mitm::observe`）。
pub struct RecordingObserver {
    inner: DomainObserver,
    ledger: Arc<ObserveLedger>,
    /// 本次会话**实际生效**的词表（允许为空 = 显式不统计）。
    ///
    /// 必须自己存一份、而不是转发 `inner.markers()`：`DomainObserver::new` 会把
    /// **空词表当成"没配"替换成默认表**（那是库层的既有语义，本卡不动它）。
    /// 而产品口径是"空 = 用户明确不统计"，所以计数的词表由这一层给代理
    /// （`proxy.rs` 用的是 `Observer::markers()`），空表就真的一词不数。
    markers: Vec<String>,
    /// 留档（数据目录固定文件）。`None` = 不落盘（测试 / 未配置路径）。
    ///
    /// 放在观察者里而不是别处：写盘要发生在"刚采到一条"这一刻，而那一刻
    /// 只有观察者知道（`proxy.rs` 每条交换调一次 `observe`）。
    archive: Option<Arc<ObserveArchive>>,
}

impl RecordingObserver {
    pub fn new(config: &ObserveConfig, ledger: Arc<ObserveLedger>) -> Self {
        Self {
            inner: DomainObserver::new(config),
            ledger,
            markers: config.markers.clone(),
            archive: None,
        }
    }

    /// 装上留档。**加法式**扩展：不装就是不落盘（老调用点行为不变）。
    pub fn with_archive(mut self, archive: Arc<ObserveArchive>) -> Self {
        self.archive = Some(archive);
        self
    }
}

impl Observer for RecordingObserver {
    fn observes(&self, host: &str) -> bool {
        self.inner.observes(host)
    }

    fn markers(&self) -> &[String] {
        // **不是** `self.inner.markers()`：见字段注释，库层会把空表换成默认表。
        &self.markers
    }

    fn observe(&self, record: &ExchangeRecord, body: &[u8]) {
        // **名单外的域名一个字节都不汇总，也不写日志。**
        // 代理循环已经先问过 `observes`，但接缝本身不保证调用方一定问过；
        // 这里再兜一次，"不在名单 ⇒ 没有摘要"才是结构上成立的，而不是靠调用方自觉。
        if !self.inner.observes(&record.host) {
            return;
        }
        self.ledger.record(record);
        // 落档在聚合**之后**，而且是节流的（见 `ObserveArchive::maybe_write`）：
        // 写盘失败也绝不影响转发（这里只记状态 + 一条 warn）。
        if let Some(archive) = &self.archive {
            archive.maybe_write(&self.ledger);
        }
        self.inner.observe(record, body);
    }
}

/// 从 MITM 设置装配 `xt_mitm::ObserveConfig`。
///
/// 词表按 [`effective_markers`] 解析：`None` ⇒ 默认表；`Some(list)` ⇒ 归一化后的表
/// （**允许为空**，空表由 [`RecordingObserver`] 如实执行成"不统计"）。
/// 观察开不开、看哪些域名、落不落盘由用户分别决定。
pub fn observe_config_for(settings: &MitmSettings) -> ObserveConfig {
    ObserveConfig {
        enabled: settings.observe.enabled,
        hosts: settings.observe.hosts.clone(),
        markers: effective_markers(settings),
        capture_body_dir: settings.observe.capture_body_dir.clone(),
    }
}

/// 本次会话**生效**的标记词表。
///
/// * `None`（缺省 / 老 `settings.json`）= `xt_mitm::default_markers()`；
/// * `Some(list)` = 归一化后的表：逐项去首尾空白、丢掉空项、按首次出现去重。
///   **允许为空** —— 那是用户明确说了"不统计标记词"（`ObserveReport::marker_counting`
///   会如实带 `false`），不是"没配"。
pub fn effective_markers(settings: &MitmSettings) -> Vec<String> {
    match &settings.observe.markers {
        None => default_markers(),
        Some(list) => normalize_markers(list),
    }
}

/// 归一化用户词表：去空白、丢空项、去重（保序）。
fn normalize_markers(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let word = raw.trim();
        if word.is_empty() {
            continue;
        }
        if !out.iter().any(|w| w == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// 配置里的观察问题，翻成给用户看的一句话（没问题时 `None`）。
///
/// 目前只有"落盘目录是相对路径"这一条 —— `xt_mitm` 遇到它会**降级为不落盘**，
/// 用户必须能看见这个降级，而不是以为自己真的在留 body。
pub fn observe_note(settings: &MitmSettings) -> Option<String> {
    let errs = settings.observe.validate();
    if errs.is_empty() {
        None
    } else {
        Some(errs.join("；"))
    }
}

// ---------------------------------------------------------------------------
// 留档与导出
// ---------------------------------------------------------------------------

/// **落档与导出共用**的文档：口径头 + 摘要。
///
/// 口径头（schema / 版本 / 起止时间 / 观察了哪些域名 / 词表是什么）是刻意的：
/// 一份没有口径的计数在事后无法解释 —— 换了词表、换了名单，数字就不能直接比。
///
/// **这里没有、也不许有**任何字段承载正文、完整 URL 或 query。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveReportFile {
    /// schema 版本（[`OBSERVE_REPORT_SCHEMA`]）。
    pub schema: String,
    /// 写下这份留档的 App 版本（口径的一部分）。
    pub app_version: String,
    /// 这份文档的生成时间（Unix 秒）。
    pub generated_unix: u64,
    /// 这一轮观察的第一条摘要时间（`None` = 没采到过）。
    pub started_unix: Option<u64>,
    /// 这一轮观察的结束时间（`None` = 还在进行 / 没停过）。
    pub ended_unix: Option<u64>,
    /// 观察开关（配置事实）。
    pub enabled: bool,
    /// 配置里的 opt-in 名单（**我们绝不自动往里加域名**）。
    pub configured_hosts: Vec<String>,
    /// 真的采到过摘要的域名（数据事实）。
    pub observed_hosts: Vec<String>,
    /// 实际用于计数的词表。
    pub markers: Vec<String>,
    /// `false` = 词表为空 ⇒ **没有统计标记词**（不是"全部为 0"）。
    pub marker_counting: bool,
    pub exchanges: u64,
    pub marker_total: u64,
    /// 每个域名一条摘要（含 body 短哈希）。**不含正文**。
    pub hosts: Vec<HostObservation>,
    /// body 落盘目录（配置值；`None` = 不落盘）。
    pub capture_body_dir: Option<String>,
    /// 配置里的问题（例如落盘目录是相对路径 ⇒ 已降级为不落盘）。
    pub note: Option<String>,
    /// 隐私口径，逐字写进文件（[`OBSERVE_PRIVACY_NOTE`]）。
    pub privacy: String,
}

/// 一次会话的**配置口径**（在 `start` 那一刻冻结）。
///
/// 用冻结值而不是"读当前设置"：留档要回答的是"**当时**是拿哪份名单、哪份词表
/// 采的"，而不是"现在设置里写着什么"。
#[derive(Debug, Clone)]
pub struct ObserveSession {
    pub enabled: bool,
    pub configured_hosts: Vec<String>,
    pub markers: Vec<String>,
    pub capture_body_dir: Option<String>,
    pub note: Option<String>,
}

impl ObserveSession {
    pub fn from_settings(settings: &MitmSettings) -> Self {
        Self {
            enabled: settings.observe.enabled,
            configured_hosts: settings.observe.hosts.clone(),
            markers: effective_markers(settings),
            capture_body_dir: settings
                .observe
                .capture_body_dir
                .as_ref()
                .map(|p| p.display().to_string()),
            note: observe_note(settings),
        }
    }
}

/// 观察结论的留档 / 导出器。
///
/// 数据目录固定文件由 [`ObserveArchive::maybe_write`]（节流）与
/// [`ObserveArchive::write_now`]（停止时强制）维护；导出走同一个
/// [`ObserveArchive::document`]，因此**内容同源**。
#[derive(Debug)]
pub struct ObserveArchive {
    /// 数据目录固定文件；`None` = 不落盘（测试或未配置）。
    path: Option<PathBuf>,
    /// `start` 那一刻冻结的配置口径。
    session: ObserveSession,
    /// 上次节流写盘时间（Unix 秒）。
    last_write_unix: Mutex<u64>,
    /// 最近一次落盘/删除失败的可读原因（成功时清空）——失败**不许静默**。
    last_error: Mutex<Option<String>>,
}

impl ObserveArchive {
    pub fn new(settings: &MitmSettings, path: Option<PathBuf>) -> Self {
        Self {
            path,
            session: ObserveSession::from_settings(settings),
            last_write_unix: Mutex::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// 构造留档/导出文档 —— **唯一**的构造点（同源就靠这个）。
    pub fn document(&self, ledger: &ObserveLedger, generated_unix: u64) -> ObserveReportFile {
        let snap = ledger.snapshot();
        let observed_hosts = snap.hosts.iter().map(|h| h.host.clone()).collect();
        ObserveReportFile {
            schema: OBSERVE_REPORT_SCHEMA.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_unix,
            started_unix: snap.started_unix,
            ended_unix: snap.ended_unix,
            enabled: self.session.enabled,
            configured_hosts: self.session.configured_hosts.clone(),
            observed_hosts,
            markers: self.session.markers.clone(),
            marker_counting: !self.session.markers.is_empty(),
            exchanges: snap.exchanges,
            marker_total: snap.marker_total,
            hosts: snap.hosts,
            capture_body_dir: self.session.capture_body_dir.clone(),
            note: self.session.note.clone(),
            privacy: OBSERVE_PRIVACY_NOTE.to_string(),
        }
    }

    /// 最近一次落盘失败的可读原因（界面据此显示，不许静默）。
    pub fn last_error(&self) -> Option<String> {
        match self.last_error.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_error(&self, msg: Option<String>) {
        if let Ok(mut g) = self.last_error.lock() {
            *g = msg;
        }
    }

    fn lock_last_write(&self) -> MutexGuard<'_, u64> {
        match self.last_write_unix.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 记完一条摘要后调用：**节流**写一次（第一次立刻写）。
    ///
    /// * 没配路径 ⇒ 什么都不做；
    /// * **零摘要 ⇒ 绝不产生文件**（默认关时磁盘上不该多出任何东西）；
    /// * 写失败只记状态 + 一条 warn —— 观察没有资格弄坏转发。
    pub fn maybe_write(&self, ledger: &ObserveLedger) {
        if self.path.is_none() {
            return;
        }
        let now = xt_core::util::now_unix();
        {
            let mut last = self.lock_last_write();
            if now.saturating_sub(*last) < OBSERVE_PERSIST_INTERVAL_SECS {
                return;
            }
            // 先占位：并发的观察线程不该同时写同一个文件。
            *last = now;
        }
        if let Err(e) = self.write(ledger, now) {
            tracing::warn!(error = %e, "观察留档落盘失败（不影响转发）");
        }
    }

    /// 立刻写一次（停止 / 显式动作）。返回写入路径；失败给出可读原因。
    ///
    /// 零摘要时**不产生文件**（返回路径但不写），这是判据①的硬口径。
    pub fn write_now(&self, ledger: &ObserveLedger) -> Result<PathBuf, String> {
        let now = xt_core::util::now_unix();
        self.write(ledger, now)
    }

    fn write(&self, ledger: &ObserveLedger, now: u64) -> Result<PathBuf, String> {
        let Some(path) = self.path.clone() else {
            return Err("观察留档没有配置路径（不该发生）".to_string());
        };
        // **零摘要 ⇒ 零文件。** 默认关时磁盘上不得出现任何东西。
        if ledger.is_empty() {
            return Ok(path);
        }
        let doc = self.document(ledger, now);
        write_document(&path, &doc)?;
        self.set_error(None);
        *self.lock_last_write() = now;
        Ok(path)
    }
}

/// 数据目录下的固定留档文件路径。
pub fn report_path_in(root: &Path) -> PathBuf {
    root.join(OBSERVE_REPORT_FILE)
}

/// 把文档原子地写到目标路径（导出与留档共用）。
///
/// 路径由调用方保证是**绝对路径**（导出路径在命令层校验）。
pub fn write_document(target: &Path, doc: &ObserveReportFile) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(doc)
        .map_err(|e| format!("序列化观察结论失败：{e}"))?;
    write_json_atomic(target, &bytes)
}

/// 读回留档。
///
/// * 文件不存在 ⇒ `Ok(None)`："还没采到过"是**正常状态**，不是错误；
/// * 读/解析失败 ⇒ `Err(可读原因)`：**绝不**静默当成"没有留档"
///   （那会让一次故障长得和"确实没采到"一模一样）。
pub fn read_report_file(path: &Path) -> Result<Option<ObserveReportFile>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读取观察留档 {} 失败：{e}", path.display())),
    };
    serde_json::from_str::<ObserveReportFile>(&text)
        .map(Some)
        .map_err(|e| format!("解析观察留档 {} 失败（文件可能损坏）：{e}", path.display()))
}

/// 删掉留档文件（"清空"用）。没有文件时 `Ok(None)`；删不掉给出可读原因。
pub fn remove_report_file(path: &Path) -> Result<Option<String>, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Some(path.display().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("删除观察留档 {} 失败：{e}", path.display())),
    }
}

/// 原子写 JSON：同目录 `.tmp` → 设 `0600` → rename。
///
/// `0600` 是刻意的：摘要里有用户访问过的域名（隐私），同机其他用户不该读到。
/// 与 `xt_core::store::atomic_write` 同款（那个是私有的，这里不复用它以避免
/// 把 `xt-core` 的改动扩到"存储"层）。
fn write_json_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_mitm::{summarize, ExchangeMeta};

    fn meta(host: &str, body_len: usize) -> ExchangeMeta {
        ExchangeMeta {
            host: host.into(),
            method: "GET".into(),
            path: "/api/timeline".into(),
            status: 200,
            status_line: "HTTP/1.1 200 OK".into(),
            content_type: Some("application/json".into()),
            body_bytes: body_len,
            request_body_bytes: 0,
        }
    }

    fn record(host: &str, body: &[u8]) -> ExchangeRecord {
        summarize(&meta(host, body.len()), body, &default_markers())
    }

    /// 用**指定词表**摘要，忠实复刻 `proxy.rs` 的调用（它用的是 `Observer::markers()`）。
    fn record_with(host: &str, body: &[u8], markers: &[String]) -> ExchangeRecord {
        summarize(&meta(host, body.len()), body, markers)
    }

    fn settings(enabled: bool, hosts: &[&str]) -> MitmSettings {
        let mut s = MitmSettings { enabled: true, domains: vec!["news.example".into()], ..Default::default() };
        s.observe.enabled = enabled;
        s.observe.hosts = hosts.iter().map(|h| (*h).to_string()).collect();
        s
    }

    /// **判据①（负例）**：默认配置（观察关、名单空）⇒ 观察者说"不看"，
    /// 直接喂一条摘要也**一条都不汇总**（零摘要）。
    #[test]
    fn observation_off_writes_no_summary_even_if_a_record_is_fed_in() {
        let cfg = observe_config_for(&MitmSettings::default());
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());

        assert!(!ob.observes("news.example"), "默认关：连名单里的域名也不看");
        // 就算有人绕过 `observes` 直接调 `observe`，也不许留下摘要。
        ob.observe(&record("news.example", br#"{"promoted":true}"#), b"");
        assert!(ledger.is_empty(), "默认关必须是零摘要");

        let report = ledger.report(&MitmSettings::default());
        assert!(!report.enabled);
        assert!(report.hosts.is_empty());
        assert_eq!(report.exchanges, 0);
        assert_eq!(report.marker_total, 0);
    }

    /// **判据②（正例）**：开启 + 域名命中 ⇒ 有摘要，且标记词计数按**出现次数**聚合
    /// 到对应域名上（不是"命中就 1"）。
    #[test]
    fn observation_on_with_a_listed_host_aggregates_marker_counts() {
        let s = settings(true, &["news.example"]);
        let cfg = observe_config_for(&s);
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());

        assert!(ob.observes("news.example"));
        assert!(ob.observes("cdn.news.example"), "子域也要命中（与库层同一套规则）");

        let body = br#"{"a":{"promoted":true},"b":{"promoted":false},"c":{"is_ad":1}}"#;
        ob.observe(&record("news.example", body), body);
        ob.observe(&record("news.example", b"{\"promoted\":1}"), b"");
        // 另一个域名：不在名单里，喂进去也必须被拒。
        let other = record("other.example", br#"{"promoted":true}"#);
        ob.observe(&other, br#"{"promoted":true}"#);

        let report = ledger.report(&s);
        assert!(report.enabled);
        assert_eq!(report.exchanges, 2, "名单内两条摘要");
        assert_eq!(report.hosts.len(), 1, "名单外的域名不许出现在报告里");
        let h = &report.hosts[0];
        assert_eq!(h.host, "news.example");
        assert_eq!(h.exchanges, 2);
        // 第一条 promoted×2 + is_ad×1，第二条 promoted×1 ⇒ promoted×3、is_ad×1
        assert_eq!(h.marker_total, 4);
        assert_eq!(
            h.markers.iter().find(|m| m.marker == "promoted").map(|m| m.count),
            Some(3),
            "promoted × 3：按出现次数聚合，不是按条数：{:?}",
            h.markers
        );
        assert_eq!(h.markers.iter().find(|m| m.marker == "is_ad").map(|m| m.count), Some(1));
        assert!(
            !h.markers.iter().any(|m| m.count == 0),
            "报告只列命中的词（零命中项没有结论价值）：{:?}",
            h.markers
        );
        assert_eq!(report.marker_total, 4);
    }

    /// **判据③（负例）**：观察开着，但域名**不在**名单里 ⇒ 依然没有摘要。
    ///
    /// 判别性：把 `RecordingObserver::observe` 里的 `observes` 兜底删掉 ⇒ 这条红。
    #[test]
    fn observation_on_but_host_not_listed_still_writes_nothing() {
        let s = settings(true, &["news.example"]);
        let cfg = observe_config_for(&s);
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());

        assert!(!ob.observes("tracker.example"));
        let body = br#"{"promoted":true,"is_ad":true}"#;
        ob.observe(&record("tracker.example", body), body);

        assert!(ledger.is_empty(), "不在名单 ⇒ 一条摘要都不许写");
        let report = ledger.report(&s);
        assert_eq!(report.exchanges, 0);
        assert!(report.hosts.is_empty());
        assert_eq!(report.marker_total, 0);
        // 配置本身是"开着 + 有名单"，报告要如实反映，而不是谎称没开。
        assert!(report.enabled);
        assert_eq!(report.configured_hosts, vec!["news.example".to_string()]);
    }

    /// 开启但**名单为空** ⇒ 等于没开（"打开了但什么都没配"）。
    #[test]
    fn observation_on_with_an_empty_list_is_equivalent_to_off() {
        let s = settings(true, &[]);
        let cfg = observe_config_for(&s);
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());
        assert!(!ob.observes("news.example"));
        let body = br#"{"promoted":true}"#;
        ob.observe(&record("news.example", body), body);
        assert!(ledger.is_empty());
        let report = ledger.report(&s);
        assert!(report.enabled, "开关是开着的事实要如实带上");
        assert!(report.hosts.is_empty());
    }

    /// 相对落盘目录：报告里的 `note` 必须把**降级**说出来（不是静默不落盘）。
    #[test]
    fn a_relative_capture_dir_shows_up_as_a_readable_note() {
        let mut s = settings(true, &["news.example"]);
        s.observe.capture_body_dir = Some(std::path::PathBuf::from("crates/xt-mitm/leak"));
        let report = ObserveLedger::default().report(&s);
        let note = report.note.expect("相对路径必须给出可读原因");
        assert!(note.contains("绝对路径"), "{note}");
        assert!(note.contains("降级为不落盘"), "{note}");

        s.observe.capture_body_dir = Some(std::path::PathBuf::from("/tmp/xraytun-observe"));
        let report = ObserveLedger::default().report(&s);
        assert!(report.note.is_none(), "{:?}", report.note);
        assert_eq!(report.capture_body_dir.as_deref(), Some("/tmp/xraytun-observe"));
    }

    /// 报告字段是给界面的契约：名字漂移必须能被看见。
    #[test]
    fn the_report_serializes_the_fields_the_ui_reads() {
        let s = settings(true, &["news.example"]);
        let ledger = ObserveLedger::default();
        let body = br#"{"promoted":true}"#;
        ledger.record(&record("news.example", body));
        let json = serde_json::to_value(ledger.report(&s)).unwrap();
        for key in [
            "enabled",
            "configured_hosts",
            "markers",
            "marker_counting",
            "exchanges",
            "marker_total",
            "hosts",
            "capture_body_dir",
            "note",
        ] {
            assert!(json.get(key).is_some(), "报告缺字段 {key}: {json}");
        }
        let host = &json["hosts"][0];
        for key in [
            "host",
            "exchanges",
            "marker_total",
            "markers",
            "body_hashes",
            "last_seen_unix",
        ] {
            assert!(host.get(key).is_some(), "域名报告缺字段 {key}: {host}");
        }
    }

    // -----------------------------------------------------------------------
    // 「标记词表可配置」：None / 自定义 / 空表 三态
    // -----------------------------------------------------------------------

    /// 归一化：去空白、丢空项、去重（保序）；`None` ⇒ 默认表。
    #[test]
    fn marker_normalization_trims_drops_blank_and_dedupes() {
        let mut s = settings(true, &["news.example"]);
        s.observe.markers = Some(vec![
            "  promoted ".into(),
            "".into(),
            "   ".into(),
            "promoted".into(),
            " 广告".into(),
        ]);
        assert_eq!(
            effective_markers(&s),
            vec!["promoted".to_string(), "广告".to_string()],
            "去空白 / 丢空项 / 去重"
        );

        s.observe.markers = None;
        assert_eq!(
            effective_markers(&s),
            default_markers(),
            "None（缺省 / 老 settings.json）⇒ 默认词表"
        );
    }

    /// **判据⑤（自定义词表）**：用户给的词表是**唯一**的计数口径 ——
    /// 默认表里的词即使真实出现，也不再计入。
    #[test]
    fn a_custom_marker_list_replaces_the_default_vocabulary() {
        let mut s = settings(true, &["news.example"]);
        s.observe.markers = Some(vec!["sponsored".into()]);
        let cfg = observe_config_for(&s);
        assert_eq!(cfg.markers, vec!["sponsored".to_string()]);

        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());
        assert_eq!(
            ob.markers(),
            ["sponsored".to_string()].as_slice(),
            "代理计数用的必须是外层词表（proxy.rs 走 Observer::markers()）"
        );

        let body = br#"{"sponsored":true,"promoted":true,"is_ad":true}"#;
        let rec = record_with("news.example", body, ob.markers());
        assert_eq!(rec.marker_total, 1, "只数新词表里的 sponsored");
        ob.observe(&rec, body);

        let report = ledger.report(&s);
        assert!(report.marker_counting, "有词表 ⇒ 统计是开着的");
        assert_eq!(report.markers, vec!["sponsored".to_string()]);
        assert_eq!(report.exchanges, 1);
        assert_eq!(report.marker_total, 1, "promoted / is_ad 已不在词表里");
        assert_eq!(report.hosts[0].markers[0].marker, "sponsored");
    }

    /// **判据⑤（空词表）**：`Some(vec![])` = **明确不统计** ——
    /// 条目照常采（域名 / 条数 / 短哈希），命中恒 0，且报告带 `marker_counting=false`。
    ///
    /// 判别性：把 `RecordingObserver::markers()` 改回转发 `self.inner.markers()`
    /// ⇒ 库层会把空表换成默认表、`marker_total` 变成 3 ⇒ 这条红（那正是"静默全 0
    /// 之外更坏的一种"：用户以为没统计，其实在用默认词表统计）。
    #[test]
    fn an_empty_marker_list_is_explicit_no_counting_not_a_silent_all_zero() {
        let mut s = settings(true, &["news.example"]);
        s.observe.markers = Some(Vec::new());
        let cfg = observe_config_for(&s);
        assert!(
            cfg.markers.is_empty(),
            "空表要原样交给观察者（产品口径：用户明确不统计）"
        );

        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&cfg, ledger.clone());
        assert!(
            ob.markers().is_empty(),
            "外层 markers() 必须是空：proxy.rs 用它算命中，空表就真的一词不数"
        );

        let body = br#"{"promoted":true,"is_ad":true,"广告":true}"#;
        let rec = record_with("news.example", body, ob.markers());
        assert_eq!(rec.marker_total, 0, "空词表 ⇒ 一个词都不数");
        ob.observe(&rec, body);

        let report = ledger.report(&s);
        assert!(!report.marker_counting, "必须显式说「这份结论没有统计标记词」");
        assert!(report.markers.is_empty());
        assert_eq!(report.exchanges, 1, "仍然采条数（不统计词 ≠ 不观察）");
        assert_eq!(report.marker_total, 0);
        assert_eq!(report.hosts.len(), 1, "域名照常留下（结论才有可执行性）");
        // 空词表**不是**"干净"：报告里没有任何能读成"没有广告"的字段。
        assert!(
            !report.marker_counting,
            "`marker_total == 0` 在空词表下是「没测量」，不是「测出来是 0」"
        );
    }

    // -----------------------------------------------------------------------
    // 「观察结论可持久化 / 可导出」：留档只存摘要
    // -----------------------------------------------------------------------

    /// 只属于这条测试的临时目录（**观察不得往仓库或用户数据目录里写**）。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xt-observe-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn dir_entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    }

    /// **判据①（负例，落盘面）**：默认关 ⇒ 零摘要，而且**一个落盘文件都不产生**
    /// （连 `.tmp` 残留都不许有）。
    #[test]
    fn observation_off_writes_no_summary_and_no_report_file() {
        let dir = temp_dir("off");
        let report_path = dir.join(OBSERVE_REPORT_FILE);
        let s = MitmSettings::default();
        let archive = Arc::new(ObserveArchive::new(&s, Some(report_path.clone())));
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&observe_config_for(&s), ledger.clone())
            .with_archive(archive.clone());

        assert!(!ob.observes("news.example"), "默认关：连名单里的域名也不看");
        let body = br#"{"promoted":true}"#;
        ob.observe(&record("news.example", body), body);
        // 停代理那条路径（强制写）也要走一遍：它是唯一能产生文件的出口。
        ledger.mark_stopped();
        assert!(archive.write_now(&ledger).is_ok());
        archive.maybe_write(&ledger);

        assert!(ledger.is_empty(), "默认关必须是零摘要");
        assert!(
            !report_path.exists(),
            "默认关不得产生任何落盘文件：{}",
            report_path.display()
        );
        assert!(
            dir_entries(&dir).is_empty(),
            "临时目录必须干净（含 .tmp 残留）：{:?}",
            dir_entries(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **判据②（正例 + 隐私）**：开启 + 命中 ⇒ 计数正确；留档与导出里
    /// **只有摘要、没有正文**（夹具响应含一段典型"正文"，断言两种产物里都不出现它）。
    #[test]
    fn a_report_file_holds_only_summaries_and_never_the_body() {
        let dir = temp_dir("body");
        let report_path = dir.join(OBSERVE_REPORT_FILE);
        let export_path = dir.join("export.json");
        let s = settings(true, &["news.example"]);
        let archive = Arc::new(ObserveArchive::new(&s, Some(report_path.clone())));
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&observe_config_for(&s), ledger.clone())
            .with_archive(archive.clone());

        // 典型正文：既有标记词（promoted / is_ad），也有**绝不该进留档**的正文片段。
        let body = br#"{"items":[{"id":7,"title":"SUPER_SECRET_BODY_TOKEN_9f2","promoted":true,"is_ad":true}]}"#;
        assert!(
            String::from_utf8_lossy(body).contains("SUPER_SECRET_BODY_TOKEN_9f2"),
            "夹具必须真的含那段正文，否则下面的断言什么都没证明"
        );
        let rec = record("news.example", body);
        assert_eq!(rec.marker_total, 2, "promoted × 1 + is_ad × 1");
        ob.observe(&rec, body);

        // 强制落盘（不等节流）。
        let written = archive.write_now(&ledger).expect("留档落盘");
        assert_eq!(written, report_path);
        let on_disk = std::fs::read_to_string(&report_path).expect("留档文件必须存在");
        assert!(
            !on_disk.contains("SUPER_SECRET_BODY_TOKEN_9f2"),
            "**留档里绝不许出现正文片段**：{on_disk}"
        );

        // 导出：把同一个 document() 写到用户给的绝对路径。
        let doc = archive.document(&ledger, xt_core::util::now_unix());
        write_document(&export_path, &doc).expect("导出");
        let exported = std::fs::read_to_string(&export_path).expect("导出文件必须存在");
        assert!(
            !exported.contains("SUPER_SECRET_BODY_TOKEN_9f2"),
            "**导出的 JSON 里绝不能出现正文片段**：{exported}"
        );
        // 计数正确 + 口径头齐全 + 允许摘要字段（短哈希）在。
        assert!(exported.contains("news.example"), "{exported}");
        assert!(exported.contains("promoted"), "{exported}");
        assert!(exported.contains("\"exchanges\": 1"), "{exported}");
        assert!(exported.contains("\"marker_total\": 2"), "{exported}");
        assert!(exported.contains(OBSERVE_REPORT_SCHEMA), "缺 schema 口径头：{exported}");
        assert!(exported.contains("\"app_version\""), "缺版本口径头：{exported}");
        assert!(exported.contains("\"started_unix\""), "缺起止时间：{exported}");
        assert!(exported.contains("\"markers\""), "缺词表口径：{exported}");
        assert!(exported.contains("\"body_hashes\""), "短哈希是允许的摘要字段：{exported}");

        // 留档与导出的**摘要部分同源**（同一个 document 构造点）。
        let back = read_report_file(&report_path)
            .expect("读回留档")
            .expect("文件存在 ⇒ Some");
        let again = archive.document(&ledger, back.generated_unix);
        assert_eq!(back, again, "留档与导出必须同源");
        // 短哈希是**哈希**，不是正文。
        assert_eq!(back.hosts[0].body_hashes.len(), 1);
        assert_eq!(back.hosts[0].body_hashes[0], xt_mitm::short_hash(body));
        assert_eq!(back.observed_hosts, vec!["news.example".to_string()]);
        assert_eq!(back.exchanges, 1);
        assert!(back.ended_unix.is_none(), "还在跑（没停过）⇒ ended 必须是 None");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **判据③（负例，落盘面）**：观察开着、但域名**不在**名单里 ⇒ 零摘要且零文件。
    #[test]
    fn an_unlisted_host_leaves_no_summary_and_no_file() {
        let dir = temp_dir("unlisted");
        let report_path = dir.join(OBSERVE_REPORT_FILE);
        let s = settings(true, &["news.example"]);
        let archive = Arc::new(ObserveArchive::new(&s, Some(report_path.clone())));
        let ledger = Arc::new(ObserveLedger::default());
        let ob = RecordingObserver::new(&observe_config_for(&s), ledger.clone())
            .with_archive(archive.clone());

        assert!(!ob.observes("tracker.example"));
        let body = br#"{"promoted":true,"is_ad":true}"#;
        ob.observe(&record("tracker.example", body), body);
        assert!(ledger.is_empty(), "不在名单 ⇒ 一条摘要都不许写");
        assert!(archive.write_now(&ledger).is_ok());
        assert!(!report_path.exists(), "零摘要 ⇒ 零文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 留档文件坏掉时必须**报出可读原因**，不许静默当成"没有留档"。
    #[test]
    fn a_corrupt_report_file_is_reported_not_silently_treated_as_missing() {
        let dir = temp_dir("corrupt");
        let p = dir.join(OBSERVE_REPORT_FILE);
        assert_eq!(read_report_file(&p).unwrap(), None, "不存在 ⇒ Ok(None)（正常状态）");
        std::fs::write(&p, b"{ not json").unwrap();
        let err = read_report_file(&p).expect_err("坏文件必须报可读原因");
        assert!(err.contains("解析观察留档"), "{err}");
        assert!(remove_report_file(&p).unwrap().is_some(), "删掉坏文件");
        assert_eq!(remove_report_file(&p).unwrap(), None, "幂等");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「清空」必须**就地**清掉累加器：跑着的观察者持有同一个 `Arc`，
    /// 换一个实例会让清空看起来生效、旧账还在被继续写。
    #[test]
    fn clear_drops_the_ledger_in_place_so_a_running_observer_sees_it() {
        let s = settings(true, &["news.example"]);
        let ledger = Arc::new(ObserveLedger::default());
        let body = br#"{"promoted":true}"#;
        ledger.record(&record("news.example", body));
        assert_eq!(ledger.snapshot().exchanges, 1);

        let seen_by_observer = ledger.clone();
        ledger.clear();
        assert!(seen_by_observer.is_empty(), "清空必须就地生效（同一个 Arc）");
        assert!(ledger.snapshot().started_unix.is_none(), "起止时间也重新开始");
    }

    /// 未开启的会话，口径头必须如实说"关着 + 默认词表"（不能编出采过的痕迹）。
    #[test]
    fn a_default_session_header_says_off_with_the_default_vocabulary() {
        let session = ObserveSession::from_settings(&MitmSettings::default());
        assert!(!session.enabled);
        assert!(session.configured_hosts.is_empty());
        assert_eq!(session.markers, default_markers());
        assert_eq!(session.capture_body_dir, None);
    }
}
