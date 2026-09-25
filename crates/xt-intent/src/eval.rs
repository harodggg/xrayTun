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

use crate::answer::Answers;
use crate::question::{KIND_LABELS, Q_ADS_INTENT, Q_ENDPOINT_KIND, Q_RISK_OF_BREAKAGE};
use crate::verdict::{AllowReason, Category, DeferReason, Thresholds, Verdict};

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

// ---------------------------------------------------------------------------
// 原始答案诊断（task-13）
// ---------------------------------------------------------------------------
//
// 背景：两次实测都是「无标注桶 0 拦」，但我们只统计了**最终 verdict**，
// 看不到模型实际答了什么。于是两种解释分不开：
//
//   (a) 模型高置信地说"不是广告"      ⇒ 能力/问题本身的问题；
//   (b) 模型给了广告类别但被闸门挡下  ⇒ **是我们的阈值/白名单**；
//   (c) 低置信 / 字段被解析丢         ⇒ **是我们的问法/解析**。
//
// 这一节把网关返回的**原始 answers**（每个 id 的原样 JSON）做成聚合统计。
//
// 隐私：输入记录含域名（只能落在仓库外）；本节的渲染函数**只输出聚合数字**，
// 有一条测试直接断言渲染结果里不出现任何样例域名。

/// `eval_domains --raw-answers` 写的一行：一次网关调用的原始答案。
///
/// **含域名**，所以文件只能落在仓库外（`/tmp`）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawAnswerRecord {
    pub host: String,
    #[serde(default)]
    pub ts_unix: u64,
    /// `false` = 网关失败（超时/非 2xx/连接错误），这一条没拿到答案。
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub expected_ids: Vec<String>,
    #[serde(default)]
    pub answer_ids: Vec<String>,
    /// `id → 原样 JSON`（服务器给什么形状就是什么形状）。
    #[serde(default)]
    pub answers: serde_json::Value,
    #[serde(default)]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 一条记录的归类。**(a)/(b)/(c) 的判定规则写在这里**，渲染时逐条给条数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerClass {
    /// (b) 类别本身可拦，但被**数值闸门**挡下（阈值 或 风险刹车）。
    AdCategoryStoppedByNumericGate,
    /// (c) 类别可拦，但模型**自报置信度**低于阈值 —— 它自己在犹豫。
    AdCategoryLowConfidence,
    /// (c) 类别说"不是广告"，但模型自报置信度低于阈值 —— 在犹豫，不是明确否定。
    ///
    /// 单独一个变体而不是并进 [`Self::NotAdCategory`]：**(a) 的判据是"高置信"**，
    /// 把犹豫的样本混进 (a) 会把"模型明确说不是"说重。
    NonAdLowConfidence,
    /// (a) 答案可解析、类别**不在**可拦集合、自报置信度也够 ⇒ 明确说"不是广告"。
    NotAdCategory,
    /// (c) 少字段 / `noul` 形状不对 / choice 标签不在白名单 ⇒ 解析这层丢了答案。
    Unparsable,
    /// 没拿到答案（网关错误 / 超时）——**不计入 a/b/c**，单独列。
    NoAnswer,
    /// 拿到了答案且真的被判成 block（无标注桶里的意外）——单独列。
    Blocked,
}

impl AnswerClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdCategoryStoppedByNumericGate => "ad_category_stopped_by_numeric_gate",
            Self::AdCategoryLowConfidence => "ad_category_low_confidence",
            Self::NonAdLowConfidence => "non_ad_low_confidence",
            Self::NotAdCategory => "not_ad_category",
            Self::Unparsable => "unparsable",
            Self::NoAnswer => "no_answer",
            Self::Blocked => "blocked",
        }
    }

    /// 归到 (a)/(b)/(c) 哪一桶；`None` = 不参与该三分（网关失败 / 真的拦了）。
    pub fn bucket(self) -> Option<&'static str> {
        match self {
            Self::NotAdCategory => Some("a"),
            Self::AdCategoryStoppedByNumericGate => Some("b"),
            Self::AdCategoryLowConfidence | Self::NonAdLowConfidence | Self::Unparsable => Some("c"),
            Self::NoAnswer | Self::Blocked => None,
        }
    }
}

/// 一条原始答案的逐条分析。`host` **绝不进渲染**（只在测试与 join 时用）。
#[derive(Debug, Clone)]
pub struct RecordAnalysis {
    pub host: String,
    pub class: AnswerClass,
    /// 引擎实际挡下它的原因（`allow`/`deferred` 的原因名；`block`/`no_answer`）。
    pub gate: String,
    /// 解析出的类别标签（不在白名单时为 `None`）。
    pub category: Option<String>,
    /// 原始 `choice` 字段 —— **即使不在白名单也记**：词表太窄要看得见。
    pub raw_choice: Option<String>,
    pub raw_kind_type: Option<String>,
    pub raw_ads_type: Option<String>,
    pub raw_risk_type: Option<String>,
    pub ads_intent: Option<f32>,
    pub risk_of_breakage: Option<f32>,
    pub choice_confidence: Option<f32>,
    /// 期望但服务端没给的 id。
    pub missing_ids: Vec<String>,
    /// **反事实**：把风险刹车关掉（`risk_of_breakage_max = 1.0`）之后再走一遍闸门，
    /// 下一条挡它的是什么。`None` = 参数不全（读不出来）或这一步会 block。
    ///
    /// 为什么需要它：刹车在闸门链里**最先**（`decide` 的第一条），所以
    /// "被 risk 挡下"会把后面所有闸门都遮住。只看最终 verdict 会误以为
    /// "把风险阈值放开就有 block" —— 这个字段直接给出答案。
    pub next_gate_without_brake: Option<String>,
}

fn gate_str(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Block(_) => "block".to_string(),
        Verdict::Allow(AllowReason::BreakageRiskTooHigh { .. }) => {
            "breakage_risk_too_high".to_string()
        }
        Verdict::Allow(AllowReason::CategoryNotBlockable { .. }) => {
            "category_not_blockable".to_string()
        }
        Verdict::Allow(AllowReason::LowConfidence { .. }) => "low_confidence".to_string(),
        Verdict::Allow(AllowReason::BelowThreshold { .. }) => "below_threshold".to_string(),
        Verdict::Deferred(DeferReason::MissingAnswer { id }) => {
            format!("missing_answer:{id}")
        }
        Verdict::Deferred(DeferReason::SchemaInvalid { id }) => {
            format!("schema_invalid:{id}")
        }
        Verdict::Deferred(other) => other.as_str().to_string(),
    }
}

fn raw_type(answers: &Answers, id: &str) -> Option<String> {
    answers
        .raw_of(id)
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// 分析一条原始答案。
///
/// `engine_verdict` 优先用**引擎实际给的判决**（含形状加分）；`None` 时用
/// `thresholds` 重算一遍 `decide`（`bonus = 0`），结果一样可复算。
pub fn analyze_record(
    rec: &RawAnswerRecord,
    engine_verdict: Option<&Verdict>,
    thresholds: &Thresholds,
) -> RecordAnalysis {
    let mut a = RecordAnalysis {
        host: rec.host.clone(),
        class: AnswerClass::NoAnswer,
        gate: rec
            .error_kind
            .clone()
            .unwrap_or_else(|| "no_answer".to_string()),
        category: None,
        raw_choice: None,
        raw_kind_type: None,
        raw_ads_type: None,
        raw_risk_type: None,
        ads_intent: None,
        risk_of_breakage: None,
        choice_confidence: None,
        missing_ids: Vec::new(),
        next_gate_without_brake: None,
    };
    if !rec.ok {
        return a;
    }

    let Some(answers) = Answers::from_wire(&serde_json::json!({ "answers": rec.answers }))
    else {
        a.class = AnswerClass::Unparsable;
        a.gate = "answers_not_object".into();
        return a;
    };

    a.raw_choice = answers
        .raw_of(Q_ENDPOINT_KIND)
        .and_then(|v| v.get("choice"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    a.raw_kind_type = raw_type(&answers, Q_ENDPOINT_KIND);
    a.raw_ads_type = raw_type(&answers, Q_ADS_INTENT);
    a.raw_risk_type = raw_type(&answers, Q_RISK_OF_BREAKAGE);
    a.ads_intent = answers.noul(Q_ADS_INTENT);
    a.risk_of_breakage = answers.noul(Q_RISK_OF_BREAKAGE);
    a.missing_ids = rec
        .expected_ids
        .iter()
        .filter(|id| !answers.has(id))
        .cloned()
        .collect();

    let kind = answers.choice(Q_ENDPOINT_KIND, KIND_LABELS);
    if let Some((label, conf)) = &kind {
        a.category = Some(label.clone());
        a.choice_confidence = Some(*conf);
    }

    let verdict = match engine_verdict {
        Some(v) => v.clone(),
        None => crate::verdict::decide(&answers, thresholds, 0.0),
    };
    a.gate = gate_str(&verdict);

    // 反事实：刹车关掉之后再走一遍（只在这个字段能读出三个数时才做）。
    if kind.is_some() && a.ads_intent.is_some() && a.risk_of_breakage.is_some() {
        let mut without_brake = thresholds.clone();
        without_brake.risk_of_breakage_max = 1.0;
        a.next_gate_without_brake =
            Some(gate_str(&crate::verdict::decide(&answers, &without_brake, 0.0)));
    }

    if matches!(verdict, Verdict::Block(_)) {
        a.class = AnswerClass::Blocked;
        return a;
    }

    // 只要有任何一个期望字段读不出来，就归 (c)：**先把"我们丢了答案"和
    // "模型说不是"分开**，这是这份诊断的全部意义。
    if kind.is_none() || a.ads_intent.is_none() || a.risk_of_breakage.is_none() {
        a.class = AnswerClass::Unparsable;
        return a;
    }

    let blockable = a
        .category
        .as_deref()
        .and_then(Category::from_label)
        .map(|c| thresholds.block_categories.contains(&c))
        .unwrap_or(false);
    // 自报置信度低于阈值 ⇒ 模型在犹豫。**即使类别说"不是广告"也归 (c)**：
    // (a) 的判据是"高置信"，把犹豫的样本算进 (a) 会把结论说重。
    let low_conf = a
        .choice_confidence
        .map(|c| c < thresholds.choice_confidence_min)
        .unwrap_or(true);

    a.class = match &verdict {
        Verdict::Deferred(_) => AnswerClass::NoAnswer,
        Verdict::Block(_) => AnswerClass::Blocked,
        Verdict::Allow(AllowReason::LowConfidence { .. }) => {
            if blockable {
                AnswerClass::AdCategoryLowConfidence
            } else {
                AnswerClass::NonAdLowConfidence
            }
        }
        Verdict::Allow(_) => {
            if blockable {
                AnswerClass::AdCategoryStoppedByNumericGate
            } else if low_conf {
                AnswerClass::NonAdLowConfidence
            } else {
                AnswerClass::NotAdCategory
            }
        }
    };
    a
}

fn bucket_of(v: Option<f32>, edges: &[f32], labels: &[&str]) -> usize {
    match v {
        None => labels.len() - 1,
        Some(x) => edges.iter().position(|e| x < *e).unwrap_or(edges.len()),
    }
}

fn hist_lines(name: &str, labels: &[&str], counts: &[usize], total: usize) -> String {
    let mut out = format!("### {name}\n\n| 桶 | 条数 | 占比 |\n|---|---:|---:|\n");
    for (l, c) in labels.iter().zip(counts.iter()) {
        out.push_str(&format!("| {l} | {c} | {:.1}% |\n", pct(*c as u64, total as u64)));
    }
    out.push('\n');
    out
}

/// 把一批分析渲染成**只有聚合数字**的 Markdown。
///
/// ⚠️ 这里绝不能出现 `RecordAnalysis.host`（有一条测试盯着）。
pub fn render_diagnosis(analyses: &[RecordAnalysis], buckets_asked: usize) -> String {
    let total = analyses.len();
    let mut out = String::new();
    out.push_str("## 无标注桶：原始答案分布（task-13）\n\n");
    out.push_str(&format!(
        "样本 {total} 条（排队问出的无标注域名 {buckets_asked} 条）。\
         **本节的每个数字都来自 `--raw-answers` 里模型的实际回答，不是最终 verdict 的转述。**\n\n"
    ));

    // 三分桶
    let mut a_n = 0usize;
    let mut b_n = 0usize;
    let mut c_n = 0usize;
    let mut no_answer = 0usize;
    let mut blocked = 0usize;
    for x in analyses {
        match x.class.bucket() {
            Some("a") => a_n += 1,
            Some("b") => b_n += 1,
            Some("c") => c_n += 1,
            _ => match x.class {
                AnswerClass::NoAnswer => no_answer += 1,
                _ => blocked += 1,
            },
        }
    }
    out.push_str("### 判定（三分）\n\n");
    out.push_str("| 桶 | 含义 | 条数 | 占比 |\n|---|---|---:|---:|\n");
    out.push_str(&format!(
        "| **(a)** 模型高置信说\"不是广告\" | 答案可解析、类别不在可拦集合 | {a_n} | {:.1}% |\n",
        pct(a_n as u64, total as u64)
    ));
    out.push_str(&format!(
        "| **(b)** 给了广告类别、被数值/风险闸门挡下 | 阈值或风险刹车 | {b_n} | {:.1}% |\n",
        pct(b_n as u64, total as u64)
    ));
    out.push_str(&format!(
        "| **(c)** 低置信 / 字段被解析丢 | 置信度闸门 / 缺字段 / 形状不符 | {c_n} | {:.1}% |\n",
        pct(c_n as u64, total as u64)
    ));
    out.push_str(&format!(
        "| 未拿到答案（不计入三分） | 网关失败 / 超时 | {no_answer} | {:.1}% |\n",
        pct(no_answer as u64, total as u64)
    ));
    out.push_str(&format!(
        "| 真的被判 block（不计入三分） | 无标注桶里的意外收获 | {blocked} | {:.1}% |\n\n",
        pct(blocked as u64, total as u64)
    ));

    // 细分归类
    let mut classes: BTreeMap<&'static str, usize> = BTreeMap::new();
    for x in analyses {
        *classes.entry(x.class.as_str()).or_insert(0) += 1;
    }
    out.push_str("### 每条记录的具体归类\n\n| 归类 | 条数 |\n|---|---:|\n");
    for (k, v) in &classes {
        out.push_str(&format!("| `{k}` | {v} |\n"));
    }
    out.push('\n');

    // 闸门分布
    let mut gates: BTreeMap<&str, usize> = BTreeMap::new();
    for x in analyses {
        *gates.entry(x.gate.as_str()).or_insert(0) += 1;
    }
    out.push_str("### 引擎实际挡下它的原因\n\n| 闸门 / 原因 | 条数 |\n|---|---:|\n");
    for (k, v) in &gates {
        out.push_str(&format!("| `{k}` | {v} |\n"));
    }
    out.push('\n');

    // 反事实：刹车在闸门链里最先，会把后面的闸门全遮住。这里把刹车关掉再走一遍，
    // 直接回答"把风险阈值放开是不是就有 block 了"。
    let mut next_gates: BTreeMap<&str, usize> = BTreeMap::new();
    let mut next_unknown = 0usize;
    for x in analyses {
        match x.next_gate_without_brake.as_deref() {
            Some(g) => *next_gates.entry(g).or_insert(0) += 1,
            None => next_unknown += 1,
        }
    }
    out.push_str(
        "### 反事实：先关掉风险刹车（`risk_of_breakage_max = 1.0`），下一条挡它的是什么\n\n\
         刹车是 `decide()` 的第一条，会遮住后面的闸门。这张表回答\"只把风险阈值放开\
         会不会就有 block\"。\n\n| 下一条闸门 | 条数 |\n|---|---:|\n",
    );
    for (k, v) in &next_gates {
        out.push_str(&format!("| `{k}` | {v} |\n"));
    }
    out.push_str(&format!("| （读不出三个数，无法判定） | {next_unknown} |\n\n"));

    // 类别
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    for x in analyses {
        let key = match (&x.category, &x.raw_choice) {
            (Some(c), _) => c.clone(),
            (None, Some(raw)) => format!("<不在白名单: {raw}>"),
            (None, None) => "<没有 choice 字段>".to_string(),
        };
        *cats.entry(key).or_insert(0) += 1;
    }
    out.push_str("### 类别计数（模型实际选的 `endpoint_kind`）\n\n| 类别 | 条数 |\n|---|---:|\n");
    for (k, v) in &cats {
        out.push_str(&format!("| {k} | {v} |\n"));
    }
    out.push('\n');

    // 三个数
    let ads_labels = ["[0,0.1)", "[0.1,0.3)", "[0.3,0.5)", "[0.5,0.7)", "[0.7,0.85)", "[0.85,1.0]", "缺"];
    let ads_edges = [0.1f32, 0.3, 0.5, 0.7, 0.85, 1.01];
    let mut ads_counts = vec![0usize; ads_labels.len()];
    let risk_labels = ["[0,0.3] 不触刹车", "(0.3,0.5]", "(0.5,0.8]", "(0.8,1.0]", "缺"];
    let risk_edges = [0.3001f32, 0.5, 0.8, 1.01];
    let mut risk_counts = vec![0usize; risk_labels.len()];
    let conf_labels = ["[0,0.5) 过不了置信度闸门", "[0.5,0.7)", "[0.7,0.9)", "[0.9,1.0]", "缺"];
    let conf_edges = [0.5f32, 0.7, 0.9, 1.01];
    let mut conf_counts = vec![0usize; conf_labels.len()];
    for x in analyses {
        ads_counts[bucket_of(x.ads_intent, &ads_edges, &ads_labels)] += 1;
        risk_counts[bucket_of(x.risk_of_breakage, &risk_edges, &risk_labels)] += 1;
        conf_counts[bucket_of(x.choice_confidence, &conf_edges, &conf_labels)] += 1;
    }
    out.push_str(&hist_lines("`ads_intent` 分桶", &ads_labels, &ads_counts, total));
    out.push_str(&hist_lines("`risk_of_breakage` 分桶", &risk_labels, &risk_counts, total));
    out.push_str(&hist_lines("`choice_confidence` 分桶", &conf_labels, &conf_counts, total));

    // 解析失败细分
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    let mut kind_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut ads_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut risk_types: BTreeMap<String, usize> = BTreeMap::new();
    for x in analyses {
        for id in &x.missing_ids {
            *missing.entry(id.clone()).or_insert(0) += 1;
        }
        *kind_types
            .entry(x.raw_kind_type.clone().unwrap_or_else(|| "<无>".into()))
            .or_insert(0) += 1;
        *ads_types
            .entry(x.raw_ads_type.clone().unwrap_or_else(|| "<无>".into()))
            .or_insert(0) += 1;
        *risk_types
            .entry(x.raw_risk_type.clone().unwrap_or_else(|| "<无>".into()))
            .or_insert(0) += 1;
    }
    out.push_str("### 解析细查（为什么读不出答案）\n\n");
    out.push_str("**期望但服务端没给的 id**：\n\n| id | 条数 |\n|---|---:|\n");
    if missing.is_empty() {
        out.push_str("| （没有缺失） | 0 |\n");
    }
    for (k, v) in &missing {
        out.push_str(&format!("| `{k}` | {v} |\n"));
    }
    out.push('\n');
    for (name, m) in [
        ("`endpoint_kind` 的 `type` 字段", &kind_types),
        ("`ads_intent` 的 `type` 字段", &ads_types),
        ("`risk_of_breakage` 的 `type` 字段", &risk_types),
    ] {
        out.push_str(&format!("**{name}**：\n\n| 取值 | 条数 |\n|---|---:|\n"));
        for (k, v) in m {
            out.push_str(&format!("| `{k}` | {v} |\n"));
        }
        out.push('\n');
    }
    out
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

    // -----------------------------------------------------------------------
    // task-13：原始答案诊断
    //
    // 这一组测试钉住"模型判不动"与"我们的闸门/解析把答案丢了"**能被分开**。
    // 每条都用真实的三个答案形状。
    // -----------------------------------------------------------------------

    fn answers_json(kind: &str, ads: f64, risk: f64, conf: f64) -> serde_json::Value {
        serde_json::json!({
            Q_ENDPOINT_KIND: { "type": "choice", "choice": kind, "confidence": conf },
            Q_ADS_INTENT: { "type": "noul", "noul": ads },
            Q_RISK_OF_BREAKAGE: { "type": "noul", "noul": risk }
        })
    }

    /// 夹具域名刻意用 `secret-…` 前缀：渲染结果里**不许**出现它。
    fn raw_record(answers: serde_json::Value) -> RawAnswerRecord {
        let ids: Vec<String> = answers
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        serde_json::from_value(serde_json::json!({
            "host": "secret-probe.example",
            "ts_unix": 1,
            "ok": true,
            "expected_ids": [Q_ENDPOINT_KIND, Q_ADS_INTENT, Q_RISK_OF_BREAKAGE],
            "answer_ids": ids,
            "answers": answers,
        }))
        .expect("夹具必须是合法 RawAnswerRecord")
    }

    fn analyze(answers: serde_json::Value) -> RecordAnalysis {
        analyze_record(&raw_record(answers), None, &Thresholds::default())
    }

    /// (a)：模型高置信说"这不是广告"（类别 human_site）。
    #[test]
    fn a_confident_non_ad_category_is_bucket_a() {
        let a = analyze(answers_json("human_site", 0.02, 0.02, 0.95));
        assert_eq!(a.class, AnswerClass::NotAdCategory);
        assert_eq!(a.class.bucket(), Some("a"));
        assert_eq!(a.gate, "category_not_blockable");
        assert_eq!(a.category.as_deref(), Some("human_site"));
        // 反事实：关掉刹车也一样 —— 挡住它的是类别本身。
        assert_eq!(a.next_gate_without_brake.as_deref(), Some("category_not_blockable"));
    }

    /// (b)：模型给了**广告类别**，被阈值挡下 —— 这是"我们的闸门"。
    #[test]
    fn an_ad_category_under_the_threshold_is_bucket_b() {
        let a = analyze(answers_json("ad_or_monetization", 0.80, 0.05, 0.95));
        assert_eq!(a.class, AnswerClass::AdCategoryStoppedByNumericGate);
        assert_eq!(a.class.bucket(), Some("b"));
        assert_eq!(a.gate, "below_threshold");
        assert_eq!(a.ads_intent, Some(0.80));
        assert_eq!(a.next_gate_without_brake.as_deref(), Some("below_threshold"));
    }

    /// (b)：风险刹车也是"我们的闸门"（数值闸门），不是模型不判。
    #[test]
    fn the_risk_brake_also_counts_as_bucket_b() {
        let a = analyze(answers_json("ad_or_monetization", 0.97, 0.90, 0.97));
        assert_eq!(a.class, AnswerClass::AdCategoryStoppedByNumericGate);
        assert_eq!(a.gate, "breakage_risk_too_high");
        // **判别性**：这条如果关掉刹车就会 block —— 说明"打开阈值就有 block"
        // 对这条成立，而对 (a) 那种（类别不对）永远不成立。
        assert_eq!(a.next_gate_without_brake.as_deref(), Some("block"));
    }

    /// (c)：类别对、分数高，但模型**自报置信度低** —— 它自己在犹豫。
    #[test]
    fn an_ad_category_with_low_self_reported_confidence_is_bucket_c() {
        let a = analyze(answers_json("ad_or_monetization", 0.97, 0.05, 0.20));
        assert_eq!(a.class, AnswerClass::AdCategoryLowConfidence);
        assert_eq!(a.class.bucket(), Some("c"));
        assert_eq!(a.gate, "low_confidence");
    }

    /// (c)：类别说"不是广告"，但置信度低于阈值 ⇒ 犹豫，不算 (a) 的"明确否定"。
    ///
    /// 判别性：`decide()` 会先报 `category_not_blockable`（类别闸门在置信度之前），
    /// 只看 verdict 会把它算进 (a) —— 这条测试钉住按**置信度**补判。
    #[test]
    fn a_non_ad_category_with_low_confidence_is_also_bucket_c() {
        // 风险放低，让闸门链走到**类别**那一步（`decide` 先判刹车，再判类别）。
        let a = analyze(answers_json("api_or_service", 0.28, 0.10, 0.30));
        assert_eq!(a.gate, "category_not_blockable", "闸门链先报的是类别");
        assert_eq!(a.class, AnswerClass::NonAdLowConfidence);
        assert_eq!(a.class.bucket(), Some("c"));
    }

    /// (c) 的核心：`ads_intent` 被当成 `choice` 回给了我们 ⇒ 是**我们读不出**，
    /// 不是模型说"不是"。
    #[test]
    fn a_noul_answer_delivered_as_choice_is_bucket_c_and_names_the_field() {
        let answers = serde_json::json!({
            Q_ENDPOINT_KIND: { "type": "choice", "choice": "human_site", "confidence": 0.99 },
            Q_ADS_INTENT: { "type": "choice", "choice": "no", "confidence": 0.9 },
            Q_RISK_OF_BREAKAGE: { "type": "noul", "noul": 0.01 }
        });
        let a = analyze(answers);
        assert_eq!(a.class, AnswerClass::Unparsable);
        assert_eq!(a.class.bucket(), Some("c"));
        assert_eq!(a.gate, "schema_invalid:ads_intent");
        assert_eq!(a.raw_ads_type.as_deref(), Some("choice"));
        // 渲染要能说清是哪个字段、什么形状。
        let md = render_diagnosis(std::slice::from_ref(&a), 1);
        assert!(md.contains("ads_intent"), "{md}");
        assert!(md.contains("`choice`"), "{md}");
    }

    /// (c)：字段直接缺失。
    #[test]
    fn a_missing_field_is_bucket_c_and_names_the_id() {
        let answers = serde_json::json!({
            Q_ENDPOINT_KIND: { "type": "choice", "choice": "ad_or_monetization", "confidence": 0.99 },
            Q_ADS_INTENT: { "type": "noul", "noul": 0.99 }
        });
        let a = analyze(answers);
        assert_eq!(a.class, AnswerClass::Unparsable);
        assert_eq!(a.missing_ids, vec![Q_RISK_OF_BREAKAGE.to_string()]);
        assert_eq!(a.gate, "missing_answer:risk_of_breakage");
    }

    /// (c)：choice 标签不在白名单 —— 词表太窄要看得见原始词。
    #[test]
    fn an_unknown_choice_label_is_bucket_c_and_the_raw_word_is_kept() {
        let a = analyze(answers_json("totally_an_ad", 0.99, 0.0, 0.99));
        assert_eq!(a.class, AnswerClass::Unparsable);
        assert_eq!(a.raw_choice.as_deref(), Some("totally_an_ad"));
        let md = render_diagnosis(std::slice::from_ref(&a), 1);
        assert!(md.contains("totally_an_ad"), "越白名单的原始词必须出现在报告里：{md}");
        assert!(md.contains("<不在白名单"), "{md}");
    }

    /// 网关失败**不许**被算进 (a)/(b)/(c)：那会把"没问到"读成"模型判不动"。
    #[test]
    fn a_gateway_error_is_not_silently_counted_in_any_bucket() {
        let rec: RawAnswerRecord = serde_json::from_value(serde_json::json!({
            "host": "secret-probe.example",
            "ok": false,
            "error_kind": "timeout",
            "expected_ids": [Q_ENDPOINT_KIND, Q_ADS_INTENT, Q_RISK_OF_BREAKAGE],
        }))
        .unwrap();
        let a = analyze_record(&rec, None, &Thresholds::default());
        assert_eq!(a.class, AnswerClass::NoAnswer);
        assert_eq!(a.class.bucket(), None);
        assert_eq!(a.gate, "timeout");
        let md = render_diagnosis(std::slice::from_ref(&a), 1);
        assert!(md.contains("未拿到答案"), "{md}");
        assert!(md.contains("| **(a)**") && md.contains("| **(c)**"), "{md}");
    }

    /// 引擎实际判决优先于重算（含形状加分时两者会不同）。
    #[test]
    fn the_engine_verdict_wins_over_recomputing() {
        let rec = raw_record(answers_json("ad_or_monetization", 0.95, 0.05, 0.99));
        let engine = Verdict::Allow(AllowReason::BelowThreshold {
            ads_intent: 0.95,
            effective_min: 0.99,
        });
        let a = analyze_record(&rec, Some(&engine), &Thresholds::default());
        assert_eq!(a.gate, "below_threshold");
        assert_eq!(a.class, AnswerClass::AdCategoryStoppedByNumericGate);
    }

    /// **隐私**：诊断渲染里一个域名都不许出现。
    #[test]
    fn the_diagnosis_contains_no_hostnames() {
        let mut analyses = Vec::new();
        for (kind, ads, risk, conf) in [
            ("human_site", 0.02, 0.02, 0.95),
            ("ad_or_monetization", 0.80, 0.05, 0.95),
            ("ad_or_monetization", 0.97, 0.05, 0.20),
        ] {
            analyses.push(analyze(answers_json(kind, ads, risk, conf)));
        }
        let md = render_diagnosis(&analyses, analyses.len());
        assert!(!md.contains("secret"), "诊断里不许出现域名：\n{md}");
        assert!(md.contains("| **(a)**"), "{md}");
        assert!(md.contains("| **(b)**"), "{md}");
        assert!(md.contains("`ads_intent` 分桶"), "{md}");
        assert!(md.contains("下一条挡它的是什么"), "反事实表必须在：{md}");
    }
}
