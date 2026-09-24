//! 离线评测：Jev 判域名到底准不准。
//!
//! # 为什么这一层必须先于"默认开启"存在
//!
//! 用户的原话是「这应该能够过滤所有的广告。**取决于 Jev 的准确程度**」。
//! 这句话只能靠数字回答，不能靠感觉。而且**漏拦**与**误杀**的代价不对称：
//! 漏掉一个广告用户几乎无感，误杀一个正常站点用户会立刻关掉功能。
//! 所以主指标是**误杀率（每 1000 条连接的 FP）**，不是召回率。
//!
//! # 标注从哪来
//!
//! 用本机真实的连接日志 + 随包分发的 `geosite.dat`：
//!
//! | 集合 | 判据 | 强度 |
//! |---|---|---|
//! | **正样本** | 实际出现过 **且** 命中 `category-ads-all` | 强（社区维护的广告名单） |
//! | **负样本** | 实际出现过 **且** 命中 `apple`/`microsoft`/`google` 这类明确的产品域名表 | 强 |
//! | **未知** | 两边都不命中 | —— **这就是模型要判的那一批**，也是本次评测真正要回答的问题 |
//!
//! `cn`（大陆域名）**刻意不用作负样本**：它里面既有正常站点也有投放域名，
//! 拿它当"正常"会系统性高估精确率。
//!
//! # 隐私
//!
//! 报告里**只有聚合数字**，不含任何具体域名。要导出域名列表必须显式给出路径
//! （`--dump-unknown <path>`），而且默认关着 —— 语料本身是用户的浏览记录。
//!
//! # 判据（写死在这里，改它要说明理由）
//!
//! * holdout 精确率 **≥ 0.95**；
//! * 误杀率 **≤ 1 / 1000 连接**。
//!
//! 达不到就不允许把功能默认打开 —— 只能停在演练模式。

use std::collections::BTreeMap;

use xt_core::xray::access_log::ObservedLine;

use crate::verdict::Verdict;

/// 一条观测：某个主机名被连了多少次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainObservation {
    pub host: String,
    pub connections: u64,
}

/// 标注来源。抽象出来是为了**测试不需要真实的 `geosite.dat`**。
pub trait DomainLabels {
    /// 命中广告/追踪名单（强正样本）。
    fn is_positive(&self, host: &str) -> bool;
    /// 命中明确的产品域名表（强负样本）。
    fn is_negative(&self, host: &str) -> bool;
}

/// 用 `geosite.dat` 做标注。
pub struct GeoSiteLabels {
    geo: std::sync::Arc<xt_core::routing::geo::GeoData>,
}

impl GeoSiteLabels {
    /// 需要哪些类别由调用方给（`GeoData` 是**流式按需加载**的，
    /// 只加载用到的类别是它省内存的关键设计）。
    pub fn load(dir: &std::path::Path) -> Result<Self, String> {
        let wanted: Vec<String> = POSITIVE_CATEGORIES
            .iter()
            .chain(NEGATIVE_CATEGORIES.iter())
            .map(|s| s.to_string())
            .collect();
        let geo = xt_core::routing::geo::GeoData::load(dir, &wanted, &[])
            .map_err(|e| format!("加载 geosite 失败：{e}"))?;
        Ok(Self { geo: std::sync::Arc::new(geo) })
    }

    pub fn from_geo(geo: std::sync::Arc<xt_core::routing::geo::GeoData>) -> Self {
        Self { geo }
    }
}

/// 当"广告/追踪"用的类别。
pub const POSITIVE_CATEGORIES: &[&str] = &["category-ads-all"];
/// 当"明确正常"用的类别。刻意不含 `cn`（见模块文档）。
pub const NEGATIVE_CATEGORIES: &[&str] = &["apple", "microsoft", "google", "category-gov-us"];

impl DomainLabels for GeoSiteLabels {
    fn is_positive(&self, host: &str) -> bool {
        POSITIVE_CATEGORIES.iter().any(|c| self.geo.site_matches(c, host))
    }

    fn is_negative(&self, host: &str) -> bool {
        NEGATIVE_CATEGORIES.iter().any(|c| self.geo.site_matches(c, host))
    }
}

/// 评测用的数据集（**只有聚合计数会进报告**）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dataset {
    /// 出现过且命中广告名单。
    pub positives: Vec<DomainObservation>,
    /// 出现过且命中明确产品表。
    pub negatives: Vec<DomainObservation>,
    /// 两边都不命中 —— 模型要判的那一批。
    pub unknown: Vec<DomainObservation>,
    /// 全部观测到的连接行数（分母；FP/1000 连接用它）。
    pub connections_total: u64,
    /// 配不到域名的连接行数（实测约占一半，见 `access_log` 模块文档）。
    pub without_domain: u64,
    /// 被观察器过滤掉的次数（IP 字面量 / 内网 / 单标签 / 白名单…）。
    pub filtered: BTreeMap<&'static str, u64>,
}

impl Dataset {
    pub fn observed_domains(&self) -> usize {
        self.positives.len() + self.negatives.len() + self.unknown.len()
    }

    pub fn all(&self) -> impl Iterator<Item = &DomainObservation> {
        self.positives.iter().chain(self.negatives.iter()).chain(self.unknown.iter())
    }
}

/// 观察器对语料的过滤规则。**离线评测必须与线上用同一套规则**，
/// 否则量的是一个不存在的系统。
pub struct CorpusObserver {
    counts: BTreeMap<String, u64>,
    pub dataset: Dataset,
}

impl Default for CorpusObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl CorpusObserver {
    pub fn new() -> Self {
        Self {
            counts: BTreeMap::new(),
            dataset: Dataset::default(),
        }
    }

    /// 吃一行观察结果（来自 `ConnectionLog::observe_with_record`）。
    ///
    /// **不重新实现过滤规则**：`ObservedLine.record` 已经是"配对之后"的状态，
    /// 而"该不该问"这一层由 [`crate::observer::Observer`] 负责 —— 线上与离线
    /// 都走它，两条路径才不会漂移。这里只统计。
    pub fn observe(&mut self, line: &ObservedLine) {
        self.dataset.connections_total += 1;
        match line.record.as_ref().and_then(|r| r.domain.as_deref()) {
            Some(d) => *self.counts.entry(d.to_ascii_lowercase()).or_insert(0) += 1,
            None => self.dataset.without_domain += 1,
        }
    }

    /// 用标注把计数分成三桶。
    ///
    /// `is_candidate` 由调用方给（线上是 [`crate::observer::Observer`] 的过滤），
    /// 返回 `Err(reason)` 表示"不值得问"，会被计进 `filtered`。
    pub fn finish<L, F>(mut self, labels: &L, is_candidate: F) -> Dataset
    where
        L: DomainLabels,
        F: Fn(&str) -> Result<(), &'static str>,
    {
        for (host, connections) in std::mem::take(&mut self.counts) {
            if let Err(why) = is_candidate(&host) {
                *self.dataset.filtered.entry(why).or_insert(0) += 1;
                continue;
            }
            let o = DomainObservation { host, connections };
            if labels.is_positive(&o.host) {
                self.dataset.positives.push(o);
            } else if labels.is_negative(&o.host) {
                self.dataset.negatives.push(o);
            } else {
                self.dataset.unknown.push(o);
            }
        }
        // 确定性输出（报告与日志才可比）。
        for v in [
            &mut self.dataset.positives,
            &mut self.dataset.negatives,
            &mut self.dataset.unknown,
        ] {
            v.sort_by(|a, b| b.connections.cmp(&a.connections).then_with(|| a.host.cmp(&b.host)));
        }
        self.dataset
    }
}

/// 一条被判过的样本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Positive,
    Negative,
    Unknown,
}

/// 判定结果（把 [`Verdict`] 压成评测关心的三态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judgement {
    Block,
    NotBlock,
    /// 拿不到答案（网关失败 / 预算耗尽 / 缺字段）—— **放行**。
    Deferred,
}

impl From<&Verdict> for Judgement {
    fn from(v: &Verdict) -> Self {
        match v {
            Verdict::Block(_) => Self::Block,
            Verdict::Allow(_) => Self::NotBlock,
            Verdict::Deferred(_) => Self::Deferred,
        }
    }
}

/// 混淆矩阵 + 两个主指标。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    /// 命中广告名单、模型也拦了。
    pub true_positive: u64,
    /// 命中广告名单、模型放行了（漏拦）。
    pub false_negative: u64,
    /// 明确正常、模型却拦了（**误杀**）。
    pub false_positive: u64,
    /// 明确正常、模型放行了。
    pub true_negative: u64,
    /// 无标注的样本里，模型拦了多少（"模型自己发现的新广告域"，无法验证）。
    pub unknown_blocked: u64,
    pub unknown_allowed: u64,
    pub deferred: u64,
    /// 分母：全部连接数。
    pub connections_total: u64,
    /// 被误杀的域名**按连接数**加权。
    pub false_positive_connections: u64,
}

impl Metrics {
    /// 精确率：模型判定为 block 的里面，真正的广告占比。
    ///
    /// 分母只算**有强标注**的样本 —— 未知桶里的 block 无法验证，
    /// 把它们算进分子会自欺欺人（那正是我们要量的东西）。
    pub fn precision(&self) -> Option<f64> {
        let denom = self.true_positive + self.false_positive;
        (denom > 0).then(|| self.true_positive as f64 / denom as f64)
    }

    /// 召回率（漏拦有多少）。
    pub fn recall(&self) -> Option<f64> {
        let denom = self.true_positive + self.false_negative;
        (denom > 0).then(|| self.true_positive as f64 / denom as f64)
    }

    /// **主指标**：每 1000 条连接的误杀条数。
    ///
    /// 用连接数加权而不是域名数：用户感受到的是"网坏了多少个瞬间"，
    /// 而不是"坏了几个域名"。一个被误杀的高频域名比十个低频域名更伤。
    pub fn false_positives_per_1000_connections(&self) -> Option<f64> {
        // **没有一条负样本被真的判过 => 这个数不可测**，而不是 0。
        // 打印 0.000 会让人以为"测过了，很干净"，实际是"根本没测"。
        if self.true_negative + self.false_positive == 0 {
            return None;
        }
        (self.connections_total > 0)
            .then(|| self.false_positive_connections as f64 * 1000.0 / self.connections_total as f64)
    }

    /// 按写死的判据给结论（见模块文档）。
    pub fn verdict(&self) -> Gate {
        let Some(precision) = self.precision() else {
            return Gate::NotEnoughEvidence("没有一条有强标注的 block 判定，精确率无从计算".into());
        };
        let Some(fp) = self.false_positives_per_1000_connections() else {
            return Gate::NotEnoughEvidence("一条负样本都没判过，误杀率无从计算".into());
        };
        if precision < 0.95 {
            return Gate::Fail(format!("精确率 {precision:.3} < 0.95"));
        }
        if fp > 1.0 {
            return Gate::Fail(format!("误杀率 {fp:.3}/1000 连接 > 1"));
        }
        Gate::Pass
    }
}

/// 判据结论。
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Pass,
    Fail(String),
    NotEnoughEvidence(String),
}

impl Gate {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "通过",
            Self::Fail(_) => "未通过",
            Self::NotEnoughEvidence(_) => "证据不足",
        }
    }
}

/// 把逐样本判定汇总成指标。
pub fn metrics_from(samples: &[(Label, Judgement, u64)]) -> Metrics {
    let mut m = Metrics::default();
    for (label, judgement, connections) in samples {
        match (label, judgement) {
            (Label::Positive, Judgement::Block) => m.true_positive += 1,
            (Label::Positive, Judgement::NotBlock) => m.false_negative += 1,
            (Label::Negative, Judgement::Block) => {
                m.false_positive += 1;
                m.false_positive_connections += *connections;
            }
            (Label::Negative, Judgement::NotBlock) => m.true_negative += 1,
            (Label::Unknown, Judgement::Block) => m.unknown_blocked += 1,
            (Label::Unknown, Judgement::NotBlock) => m.unknown_allowed += 1,
            (_, Judgement::Deferred) => m.deferred += 1,
        }
    }
    m
}

/// 渲染报告。
///
/// **只有数字，没有域名。** 这条是硬约束：语料是用户的浏览记录。
pub fn render_report(dataset: &Dataset, metrics: Option<&Metrics>, notes: &[String]) -> String {
    let mut out = String::new();
    out.push_str("# 意图过滤 · 离线评测报告\n\n");
    out.push_str("> 本报告只含聚合数字，不含任何具体域名（语料是浏览记录）。\n\n");

    out.push_str("## 数据集\n\n");
    out.push_str("| 项 | 数量 |\n|---|---|\n");
    out.push_str(&format!("| 连接行（分母） | {} |\n", dataset.connections_total));
    out.push_str(&format!(
        "| 其中配不到域名 | {}（{:.1}%） |\n",
        dataset.without_domain,
        pct(dataset.without_domain, dataset.connections_total)
    ));
    out.push_str(&format!("| 观测到的域名 | {} |\n", dataset.observed_domains()));
    out.push_str(&format!("| ├ 命中广告名单（强正样本） | {} |\n", dataset.positives.len()));
    out.push_str(&format!("| ├ 命中明确产品表（强负样本） | {} |\n", dataset.negatives.len()));
    out.push_str(&format!("| └ **无标注（模型要判的）** | {} |\n", dataset.unknown.len()));
    let filtered: u64 = dataset.filtered.values().sum();
    if filtered > 0 {
        out.push_str(&format!("| 被观察器过滤掉（不值得问） | {filtered} |\n"));
    }
    out.push('\n');

    // L0 基线：**不需要模型**就能算，所以永远有数字。
    out.push_str("## L0 静态名单的覆盖（不需要模型）\n\n");
    let pos_conn: u64 = dataset.positives.iter().map(|o| o.connections).sum();
    out.push_str(&format!(
        "命中 `category-ads-all` 的域名占全部观测域名 {:.1}%，占全部连接 {:.2}%。\n\n",
        pct(dataset.positives.len() as u64, dataset.observed_domains() as u64),
        pct(pos_conn, dataset.connections_total)
    ));

    match metrics {
        None => {
            out.push_str("## 模型指标\n\n**未运行**：需要能问一次 Jev 网关（API Key，或免密钥档有额度）。\n\n");
        }
        Some(m) => {
            out.push_str("## 模型指标\n\n");
            out.push_str("| 指标 | 值 |\n|---|---|\n");
            out.push_str(&format!("| 真阳性（名单命中且拦） | {} |\n", m.true_positive));
            out.push_str(&format!("| 假阳性（**误杀**） | {} |\n", m.false_positive));
            out.push_str(&format!("| 假阴性（漏拦） | {} |\n", m.false_negative));
            out.push_str(&format!("| 真阴性 | {} |\n", m.true_negative));
            out.push_str(&format!("| 无标注桶里被拦 | {} |\n", m.unknown_blocked));
            out.push_str(&format!("| 拿不到答案（放行） | {} |\n", m.deferred));
            out.push('\n');
            // **分母为 0 时不许打印 0.000** —— 那看起来像"测出来的零"，
            // 实际是"没有样本"。这个区别决定了要不要相信这份报告。
            out.push_str(&format!(
                "**精确率** {}（判据 ≥ 0.95）\n\n",
                opt_ratio(m.precision(), "没有一条可验证的 block 判定")
            ));
            out.push_str(&format!(
                "**误杀率** {} / 1000 连接（判据 ≤ 1）\n\n",
                match m.false_positives_per_1000_connections() {
                    Some(v) => format!("{v:.3}"),
                    None => "—（一条负样本都没判过）".to_string(),
                }
            ));
            out.push_str(&format!(
                "**召回率** {} —— **非判据**（产出量指标：漏拦一个广告用户几乎无感，\
                 误杀一个正常站点用户会立刻关掉功能）\n\n",
                opt_ratio(m.recall(), "名单里一条都没判过")
            ));
            let gate = m.verdict();
            out.push_str(&format!("**结论：{}**", gate.as_str()));
            match gate {
                Gate::Fail(why) => out.push_str(&format!(" —— {why}")),
                Gate::NotEnoughEvidence(why) => out.push_str(&format!(" —— {why}")),
                Gate::Pass => {}
            }
            out.push_str("\n\n");

            // **读法**：判据只回答一半的问题。没有这一段，"结论：通过"会被读成
            // "打开就能过滤广告了" —— 而它其实只说"打开不太会误杀"。
            out.push_str("### 怎么读这份报告（判据只回答一半）\n\n");
            out.push_str(
                "通过判据回答的是**「开它会不会把上网搞坏」**（精确率 + 误杀率）。\
                 它**不回答「能不能拦到广告」**，那是下面两件事：\n\n",
            );
            out.push_str(&format!(
                "* **召回率 {}**：已知广告域名里有多少被拦到。判据不管这一项，\
                 但用户感受得到 —— 它决定「开了之后到底少看了多少广告」。\n",
                opt_ratio(m.recall(), "名单里一条都没判过")
            ));
            out.push_str(&format!(
                "* **无标注桶里拦了 {} 条**：这才是本功能的**全部增量**（静态名单拦不到的那批\
                 新域名），也是网关账单的去处。\n",
                m.unknown_blocked
            ));
            if m.unknown_blocked == 0 {
                out.push_str(
                    "\n> ⚠️ **无标注桶一条都没拦 ⇒ 本次评测对「模型能发现新广告域」这件事\
                     **没有任何证据**。`通过` 只说明「开它不太会误杀」，**不支持「打开就能过滤广告」**。\
                     要支持后者，未知桶里必须至少有几条真的 block（那是唯一可看的正例）。\n",
                );
            }
            out.push('\n');
        }
    }

    if !notes.is_empty() {
        out.push_str("## 说明\n\n");
        for n in notes {
            out.push_str(&format!("* {n}\n"));
        }
        out.push('\n');
    }
    out
}

/// 有值就打三位小数，没值就打出**为什么没有** —— 不打 0.000。
fn opt_ratio(v: Option<f64>, why: &str) -> String {
    match v {
        Some(v) => format!("{v:.3}"),
        None => format!("—（{why}）"),
    }
}

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::xray::access_log::ConnectionRecord;

    struct FakeLabels {
        ad: &'static [&'static str],
        benign: &'static [&'static str],
    }

    impl DomainLabels for FakeLabels {
        fn is_positive(&self, host: &str) -> bool {
            self.ad.contains(&host)
        }
        fn is_negative(&self, host: &str) -> bool {
            self.benign.contains(&host)
        }
    }

    fn line(domain: Option<&str>) -> ObservedLine {
        ObservedLine {
            outbound: Some("node-a".into()),
            record: Some(ConnectionRecord {
                ts_ms: 0,
                ts_text: String::new(),
                from: String::new(),
                network: "tcp".into(),
                target_host: "203.0.113.1".into(),
                target_port: Some(443),
                inbound_tag: "tun".into(),
                outbound_tag: "node-a".into(),
                domain: domain.map(str::to_string),
                domain_paired: domain.is_some(),
                domain_pair_delta_us: None,
                sniff_id: None,
            }),
        }
    }

    fn observer_with(lines: Vec<ObservedLine>) -> CorpusObserver {
        let mut o = CorpusObserver::new();
        for l in &lines {
            o.observe(l);
        }
        o
    }

    #[test]
    fn the_dataset_buckets_by_label_and_counts_connections() {
        let lines = vec![
            line(Some("ad.example")),
            line(Some("ad.example")),
            line(Some("ad.example")),
            line(Some("benign.example")),
            line(Some("mystery.example")),
            line(None), // 配不到域名
        ];
        let labels = FakeLabels { ad: &["ad.example"], benign: &["benign.example"] };
        let ds = observer_with(lines).finish(&labels, |_| Ok(()));

        assert_eq!(ds.connections_total, 6);
        assert_eq!(ds.without_domain, 1);
        assert_eq!(ds.positives.len(), 1);
        assert_eq!(ds.positives[0].connections, 3, "同一域名要合并计数");
        assert_eq!(ds.negatives.len(), 1);
        assert_eq!(ds.unknown.len(), 1);
        assert_eq!(ds.observed_domains(), 3);
    }

    #[test]
    fn filtered_hosts_are_counted_with_their_reason_not_silently_dropped() {
        let ds = observer_with(vec![line(Some("ip.example")), line(Some("ok.example"))])
            .finish(&FakeLabels { ad: &[], benign: &[] }, |h| {
                if h == "ip.example" {
                    Err("ip_literal")
                } else {
                    Ok(())
                }
            });
        assert_eq!(ds.filtered.get("ip_literal"), Some(&1));
        assert_eq!(ds.observed_domains(), 1);
    }

    #[test]
    fn metrics_use_connections_as_the_denominator_for_false_positives() {
        // 3 条广告域名连接（拦了），1 个正常域名但被拦了 10 次连接。
        let samples = vec![
            (Label::Positive, Judgement::Block, 3),
            (Label::Negative, Judgement::Block, 10),
            (Label::Negative, Judgement::NotBlock, 990),
            (Label::Unknown, Judgement::Block, 5),
        ];
        let mut m = metrics_from(&samples);
        m.connections_total = 1008;
        assert_eq!(m.true_positive, 1);
        assert_eq!(m.false_positive, 1);
        assert_eq!(m.false_positive_connections, 10);
        assert_eq!(m.precision(), Some(0.5));
        let fp = m.false_positives_per_1000_connections().unwrap();
        assert!((fp - 9.920634).abs() < 1e-4, "{fp}");
        assert!(matches!(m.verdict(), Gate::Fail(_)), "0.5 的精确率必须判不通过");
    }

    #[test]
    fn the_gate_passes_only_when_both_criteria_hold() {
        let m = Metrics {
            true_positive: 100,
            false_positive: 1,
            false_negative: 5,
            true_negative: 200,
            connections_total: 20_000,
            false_positive_connections: 2,
            ..Default::default()
        };
        assert_eq!(m.verdict(), Gate::Pass);
        assert!(m.precision().unwrap() >= 0.99);

        // 精确率够但误杀率超了（一个高频域名被误杀）。
        let m2 = Metrics { false_positive_connections: 50, ..m.clone() };
        assert!(matches!(m2.verdict(), Gate::Fail(_)));
    }

    #[test]
    fn a_deferred_judgement_is_not_a_block_it_is_a_pass() {
        let m = metrics_from(&[
            (Label::Negative, Judgement::Deferred, 100),
            (Label::Positive, Judgement::Deferred, 100),
        ]);
        assert_eq!(m.deferred, 2);
        assert_eq!(m.false_positive, 0, "拿不到答案绝不许算成误杀");
        assert_eq!(m.true_positive, 0);
    }

    #[test]
    fn no_negative_evidence_means_the_false_positive_rate_is_unmeasurable() {
        // 只判过正样本：精确率算得出来，误杀率算不出来 —— 两者不是一回事。
        // **连接数故意给非 0**：这样断言的就真的是"没有负样本"，而不是"分母为 0"。
        let mut m = metrics_from(&[(Label::Positive, Judgement::Block, 5)]);
        m.connections_total = 1000;
        assert_eq!(m.precision(), Some(1.0));
        assert_eq!(m.false_positives_per_1000_connections(), None);
        assert!(matches!(m.verdict(), Gate::NotEnoughEvidence(_)));

        // 判过负样本（哪怕一条）之后才有数。
        let mut m2 = metrics_from(&[
            (Label::Positive, Judgement::Block, 5),
            (Label::Negative, Judgement::NotBlock, 5),
        ]);
        m2.connections_total = 1000;
        assert_eq!(m2.false_positives_per_1000_connections(), Some(0.0));
    }

    #[test]
    fn no_strong_labels_means_not_enough_evidence_not_a_pass() {
        let m = metrics_from(&[(Label::Unknown, Judgement::Block, 1)]);
        assert!(matches!(m.verdict(), Gate::NotEnoughEvidence(_)), "{:?}", m.verdict());
    }

    /// 报告里**一个域名都不许出现** —— 语料是浏览记录。
    #[test]
    fn the_report_contains_no_hostnames() {
        let ds = observer_with(vec![
            line(Some("secret-ad-domain.example")),
            line(Some("secret-benign-domain.example")),
        ])
        .finish(
            &FakeLabels { ad: &["secret-ad-domain.example"], benign: &["secret-benign-domain.example"] },
            |_| Ok(()),
        );
        let report = render_report(&ds, None, &["说明".to_string()]);
        assert!(!report.contains("secret"), "报告里不许出现域名：\n{report}");
        assert!(report.contains("L0 静态名单"));
        assert!(report.contains("未运行"), "没跑模型时必须明说，不许留空");
    }

    /// **分母为 0 时不许把 0.000 当成结论打出来。**
    #[test]
    fn a_zero_denominator_is_reported_as_missing_not_as_zero() {
        let ds = Dataset { connections_total: 1000, ..Default::default() };
        // 全部 deferred：一条有标注的判定都没有。
        let m = metrics_from(&[
            (Label::Positive, Judgement::Deferred, 1),
            (Label::Negative, Judgement::Deferred, 1),
        ]);
        let report = render_report(&ds, Some(&m), &[]);
        assert!(!report.contains("精确率** 0.000"), "分母为 0 却打了 0.000：\n{report}");
        assert!(!report.contains("召回率** 0.000"), "分母为 0 却打了 0.000：\n{report}");
        assert!(report.contains("没有一条可验证的 block 判定"), "{report}");
        assert!(report.contains("证据不足"), "{report}");
    }

    #[test]
    fn the_report_states_the_model_numbers_when_there_are_any() {
        let ds = Dataset { connections_total: 1000, ..Default::default() };
        let m = metrics_from(&[
            (Label::Positive, Judgement::Block, 1),
            (Label::Negative, Judgement::NotBlock, 1),
        ]);
        let report = render_report(&ds, Some(&m), &[]);
        assert!(report.contains("精确率"));
        assert!(report.contains("误杀率"));
        assert!(report.contains("结论："));
    }

    /// **读法那一节是判别性的**：未知桶一条都没拦时，报告必须**显式**说
    /// "对'模型能发现新广告域'这件事没有任何证据"。
    ///
    /// 没有这条，`结论：通过` 会被读成"打开就能过滤广告" —— 而通过判据只覆盖
    /// 精确率与误杀率（设计里刻意不管召回）。真实语料上第一次跑出来就是
    /// 精确率 1.000 / 召回 0.417 / 未知桶 0 拦，正是最容易被误读的那种形态。
    #[test]
    fn a_pass_with_no_unknown_blocks_says_the_increment_is_unproven() {
        let ds = Dataset { connections_total: 1000, ..Default::default() };
        // `metrics_from` **不填 `connections_total`**（那是调用方从数据集带过来的），
        // 漏了它会让误杀率算不出来 ⇒ 结论变成"证据不足"而不是"通过"。
        let mut m = metrics_from(&[
            (Label::Positive, Judgement::Block, 1),
            (Label::Negative, Judgement::NotBlock, 1),
            (Label::Unknown, Judgement::NotBlock, 1),
        ]);
        m.connections_total = ds.connections_total;
        assert_eq!(m.verdict(), Gate::Pass, "前置条件：这份指标本身是「通过」的");
        let report = render_report(&ds, Some(&m), &[]);
        assert!(report.contains("结论：通过"), "{report}");
        assert!(
            report.contains("没有任何证据"),
            "未知桶 0 拦时报告必须点明增量没有证据：\n{report}"
        );
        assert!(
            report.contains("非判据"),
            "召回率必须被标成非判据（它是产出量指标，不是准入判据）：\n{report}"
        );
    }

    /// 负对照：未知桶**有** block 时不该再打那句警告（否则警告会变成噪音，
    /// 每次都被忽略）。
    #[test]
    fn an_unknown_block_removes_the_unproven_warning() {
        let ds = Dataset { connections_total: 1000, ..Default::default() };
        let mut m = metrics_from(&[
            (Label::Positive, Judgement::Block, 1),
            (Label::Negative, Judgement::NotBlock, 1),
            (Label::Unknown, Judgement::Block, 1),
        ]);
        m.connections_total = ds.connections_total;
        let report = render_report(&ds, Some(&m), &[]);
        assert!(!report.contains("没有任何证据"), "{report}");
        assert!(report.contains("无标注桶里拦了 1 条"), "{report}");
    }
}
