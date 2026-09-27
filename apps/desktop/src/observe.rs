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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use xt_core::model::MitmSettings;
use xt_mitm::{
    default_markers, DomainObserver, ExchangeRecord, MarkerHit, ObserveConfig, Observer,
};

/// 单个域名的观察汇总（界面上的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

impl ObserveLedger {
    /// 记一条**已经在名单内**的交换（调用方负责过 [`Observer::observes`] 闸门）。
    pub fn record(&self, record: &ExchangeRecord) {
        let mut g = self.lock();
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
        entry.last_seen_unix = xt_core::util::now_unix();
    }

    /// 渲染成界面用的报告。`settings` 提供配置事实，累加器提供数据事实。
    pub fn report(&self, settings: &MitmSettings) -> ObserveReport {
        let g = self.lock();
        let mut hosts: Vec<HostObservation> = g.by_host.values().cloned().collect();
        // 命中多的排前面；一样多时按域名，保证顺序稳定。
        hosts.sort_by(|a, b| {
            b.marker_total
                .cmp(&a.marker_total)
                .then_with(|| a.host.cmp(&b.host))
        });
        let markers = effective_markers(settings);
        ObserveReport {
            enabled: settings.observe.enabled,
            configured_hosts: settings.observe.hosts.clone(),
            marker_counting: !markers.is_empty(),
            markers,
            exchanges: g.exchanges,
            marker_total: g.marker_total,
            hosts,
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
}

impl RecordingObserver {
    pub fn new(config: &ObserveConfig, ledger: Arc<ObserveLedger>) -> Self {
        Self {
            inner: DomainObserver::new(config),
            ledger,
            markers: config.markers.clone(),
        }
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
        for key in ["host", "exchanges", "marker_total", "markers", "last_seen_unix"] {
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
}
