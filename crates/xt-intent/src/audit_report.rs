//! 离线审计：把 `intent-audit.jsonl` 变成一份**能拿来做决定**的报告。
//!
//! # 为什么这是离线的（这是设计，不是偷懒）
//!
//! 在线路径只该做**最少**的判断 —— 每加一个在线启发式，就多一个会误拦用户的现场决策。
//! 而"规则该怎么改 / 阈值该不该动 / 钱花在哪"是**应该有数据支撑的决策**，不是在线即兴。
//! 所以：在线只负责判定与落审计，**结论一律由这里离线产出**。
//!
//! # 这份报告回答什么
//!
//! 1. **L0 规则归纳**：哪些域反复被判为同一类 ⇒ 该固化成静态规则（以后不再问模型 ⇒ 省 Neuron、少外发）；
//! 2. **阈值校准**：哪些 block 的 `risk_of_breakage` 偏高 ⇒ 阈值可能偏保守；
//! 3. **成本账**：每个模型的调用数 / 输入输出 token（CF Workers AI 的模型还给出 Neuron 估算）；
//! 4. **演练比例**：`applied=false` 的占比（演练模式下判决不生效）；
//! 5. **新域/天分布**：均值与 p95 —— 这是"要不要上共享判决"唯一能立项的数。
//!
//! # 诚实边界（渲染进报告里，不是只写在注释里）
//!
//! - 这里的每一项都是**候选**，不是结论。真要下"准不准"的判断，必须有标注
//!   （见 `crates/xt-intent/examples/eval_domains.rs` 的 precision/recall）。
//! - Neuron 估算只对 `@cf/...` 的 Workers AI 模型 id 成立，且是**官方价目表的快照**
//!   （见 [`neurons_for`]）。表里没有的模型一律**不给估算** —— 编一个数比不给更糟。
//! - 报告里有**域名**（= 浏览记录）。写到文件时请导到仓库之外。

use std::collections::BTreeMap;

use crate::audit::AuditRecord;

// ---------------------------------------------------------------------------
// 日期（不引 chrono，就为了这一个函数）
// ---------------------------------------------------------------------------

/// 秒 → UTC 日 `YYYY-MM-DD`。
///
/// 算法：Howard Hinnant 的 `civil_from_days`
/// （<http://howardhinnant.github.io/date_algorithms.html>，公有领域）。
/// 自己实现而不是引 `chrono`：整个仓库只需要"秒 → 日"这一个变换，
/// 而它可以在 20 行内被已知日期钉死（见下方测试）。
///
/// 为什么用 **UTC** 而不是本地时区：分天必须**确定性**，否则同一天可能被传两次
/// 或者被跳过（本地时区会变、用户在旅行）。代价是"日界不在当地午夜"，可接受。
pub fn utc_day(ts_unix: u64) -> String {
    let days = (ts_unix / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------------------
// Neuron：官方价目快照
// ---------------------------------------------------------------------------

/// 价目表快照的抓取日（**换表必须改这个日期**，否则报告在撒谎）。
pub const NEURON_TABLE_FETCHED: &str = "2026-09-24";

/// 官方来源（Workers AI Pricing，`Price in Neurons` 列）。
pub const NEURON_TABLE_SOURCE: &str =
    "https://developers.cloudflare.com/workers-ai/platform/pricing/";

/// `(模型 id, 每 M 输入 token 的 Neuron, 每 M 输出 token 的 Neuron)`。
///
/// **只收 2026-09-24 官方表里逐字抄下来的行**，故意只收够用的几个 —— 少而准 > 多而假。
/// 表里没有的模型：`neurons_for` 返回 `None`，报告写"未知模型，不给估算"。
const NEURON_TABLE: &[(&str, f64, f64)] = &[
    ("@cf/ibm-granite/granite-4.0-h-micro", 1_542.0, 10_158.0),
    ("@cf/meta/llama-3.2-1b-instruct", 2_457.0, 18_252.0),
    ("@cf/meta/llama-3.2-3b-instruct", 4_625.0, 30_475.0),
    ("@cf/meta/llama-3.1-8b-instruct-fp8-fast", 4_119.0, 34_868.0),
    ("@cf/meta/llama-3.1-8b-instruct-fp8", 13_778.0, 26_128.0),
    ("@cf/meta/llama-guard-3-8b", 44_003.0, 2_730.0),
    ("@cf/qwen/qwen3-30b-a3b-fp8", 4_625.0, 30_475.0),
    ("@cf/google/gemma-4-26b-a4b-it", 9_091.0, 27_273.0),
    ("@cf/openai/gpt-oss-20b", 18_182.0, 27_273.0),
    ("@cf/meta/llama-3.1-8b-instruct", 25_608.0, 75_147.0),
];

/// 这次调用的 Neuron 估算。**表里没有就是 `None`**（不猜、不外推）。
pub fn neurons_for(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    NEURON_TABLE
        .iter()
        .find(|(m, _, _)| *m == model)
        .map(|(_, i, o)| {
            (input_tokens as f64 * i + output_tokens as f64 * o) / 1_000_000.0
        })
}

/// 当前 Neuron 单价（$ / 1000 Neurons，官方：$0.011）。
pub const USD_PER_1K_NEURONS: f64 = 0.011;

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

/// L0 规则归纳的门槛：至少出现这么多次。
pub const L0_MIN_ROWS: usize = 3;
/// L0 规则归纳的门槛：被 block 的比例至少这么高。
pub const L0_MIN_BLOCK_RATIO: f32 = 0.8;
/// 阈值校准的判据：被 block 但 `risk_of_breakage` 至少这么高 ⇒ 可能是错的拦截。
pub const BREAKAGE_REVIEW_MIN: f32 = 0.2;

/// 一天。
#[derive(Debug, Clone, PartialEq)]
pub struct DayStat {
    pub day: String,
    pub rows: usize,
    pub block: usize,
    pub allow: usize,
    pub deferred: usize,
    pub cache_hits: usize,
    pub applied: usize,
    /// 当天**首次出现**的域数（整个语料范围内的首次）。
    pub new_hosts: usize,
}

/// 一个域。
#[derive(Debug, Clone, PartialEq)]
pub struct HostStat {
    pub host: String,
    pub rows: usize,
    pub block: usize,
    pub allow: usize,
    pub deferred: usize,
    /// 其中判决**真的生效**（被写进规则）的行数。
    pub applied: usize,
    /// 出现过的类别数（>1 说明这个域被判过不同类 ⇒ 不适合固化）。
    pub categories: usize,
    pub category: Option<String>,
    pub first_day: String,
    pub last_day: String,
    pub max_risk_of_breakage: Option<f32>,
}

impl HostStat {
    /// 被 block 的比例（无行时 0）。
    pub fn block_ratio(&self) -> f32 {
        if self.rows == 0 {
            0.0
        } else {
            self.block as f32 / self.rows as f32
        }
    }
}

/// 一个模型。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelStat {
    pub model: String,
    pub calls: usize,
    /// 有 `usage` 的调用数（其余调用上游没回 token 数 ⇒ 成本账是不完整的）。
    pub calls_with_usage: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 只有表里认识的 `@cf/...` 模型才有值。
    pub neurons: Option<f64>,
}

/// 一个"该看看阈值"的样本。
#[derive(Debug, Clone, PartialEq)]
pub struct BreakageCandidate {
    pub host: String,
    pub day: String,
    pub ads_intent: Option<f32>,
    pub risk_of_breakage: f32,
    pub effective_min: Option<f32>,
}

/// 报告。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AuditReport {
    pub total_rows: usize,
    pub first_ts_unix: Option<u64>,
    pub last_ts_unix: Option<u64>,
    pub days: Vec<DayStat>,
    pub hosts: Vec<HostStat>,
    pub models: Vec<ModelStat>,
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub applied_rows: usize,
    pub unapplied_rows: usize,
    /// `context_sent` 非空的行数（说明用户显式开了"记录外发内容"）。
    pub rows_with_context: usize,
    /// 统计口径里被判"未知模型"的调用数。
    pub unknown_model_calls: usize,
    pub neurons_total: Option<f64>,
    /// 报告里所有行里能算出的 Neuron 总和对应的钱（按官方单价）。
    pub usd_estimate: Option<f64>,
    /// 新的域/天计数（升序）。
    pub new_hosts_per_day: Vec<(String, usize)>,
    pub l0_candidates: Vec<HostStat>,
    pub breakage_candidates: Vec<BreakageCandidate>,
}

impl AuditReport {
    /// 从审计记录构建。`min_block_ratio` 之类门槛用模块常量。
    pub fn build(records: &[AuditRecord]) -> Self {
        let mut r = AuditReport {
            total_rows: records.len(),
            ..Default::default()
        };
        if records.is_empty() {
            return r;
        }

        // 时间范围
        let mut ts: Vec<u64> = records.iter().map(|x| x.ts_unix).collect();
        ts.sort_unstable();
        r.first_ts_unix = ts.first().copied();
        r.last_ts_unix = ts.last().copied();

        // 天（并入 hosts 的 first_day）
        let mut days: BTreeMap<String, DayStat> = BTreeMap::new();
        let mut hosts: BTreeMap<String, HostStat> = BTreeMap::new();
        let mut host_categories: BTreeMap<String, Vec<Option<String>>> = BTreeMap::new();
        let mut models: BTreeMap<String, ModelStat> = BTreeMap::new();
        let mut seen_hosts: BTreeMap<String, ()> = BTreeMap::new();
        let mut new_per_day: BTreeMap<String, usize> = BTreeMap::new();
        let mut breakage: Vec<BreakageCandidate> = Vec::new();

        for rec in records {
            let day = utc_day(rec.ts_unix);
            if rec.context_sent.is_some() {
                r.rows_with_context += 1;
            }
            if rec.applied {
                r.applied_rows += 1;
            } else {
                r.unapplied_rows += 1;
            }
            if rec.cache_hit {
                r.cache_hits += 1;
            } else {
                r.cache_misses += 1;
            }

            // 天
            let d = days.entry(day.clone()).or_insert_with(|| DayStat {
                day: day.clone(),
                rows: 0,
                block: 0,
                allow: 0,
                deferred: 0,
                cache_hits: 0,
                applied: 0,
                new_hosts: 0,
            });
            d.rows += 1;
            match rec.outcome.as_str() {
                "block" => d.block += 1,
                "allow" => d.allow += 1,
                "deferred" => d.deferred += 1,
                _ => {}
            }
            if rec.cache_hit {
                d.cache_hits += 1;
            }
            if rec.applied {
                d.applied += 1;
            }

            // 域
            let h = hosts.entry(rec.host.clone()).or_insert_with(|| HostStat {
                host: rec.host.clone(),
                rows: 0,
                block: 0,
                allow: 0,
                deferred: 0,
                applied: 0,
                categories: 0,
                category: None,
                first_day: day.clone(),
                last_day: day.clone(),
                max_risk_of_breakage: None,
            });
            h.rows += 1;
            match rec.outcome.as_str() {
                "block" => h.block += 1,
                "allow" => h.allow += 1,
                "deferred" => h.deferred += 1,
                _ => {}
            }
            if rec.applied {
                h.applied += 1;
            }
            if day < h.first_day {
                h.first_day = day.clone();
            }
            if day > h.last_day {
                h.last_day = day.clone();
            }
            if let Some(risk) = rec.risk_of_breakage {
                h.max_risk_of_breakage = Some(match h.max_risk_of_breakage {
                    Some(prev) if prev >= risk => prev,
                    _ => risk,
                });
            }
            host_categories
                .entry(rec.host.clone())
                .or_default()
                .push(rec.category.clone());

            // 首次出现
            if !seen_hosts.contains_key(&rec.host) {
                seen_hosts.insert(rec.host.clone(), ());
                *new_per_day.entry(day.clone()).or_insert(0) += 1;
            }

            // 模型与成本
            if let Some(model) = &rec.model {
                let m = models.entry(model.clone()).or_insert_with(|| ModelStat {
                    model: model.clone(),
                    calls: 0,
                    calls_with_usage: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                    neurons: None,
                });
                m.calls += 1;
                if let Some(u) = rec.usage {
                    m.calls_with_usage += 1;
                    m.input_tokens += u.input_tokens as u64;
                    m.output_tokens += u.output_tokens as u64;
                }
            }

            // 阈值校准候选：block 了但"会不会弄坏了"的风险并不低
            if rec.outcome == "block" {
                if let Some(risk) = rec.risk_of_breakage {
                    if risk >= BREAKAGE_REVIEW_MIN {
                        breakage.push(BreakageCandidate {
                            host: rec.host.clone(),
                            day,
                            ads_intent: rec.ads_intent,
                            risk_of_breakage: risk,
                            effective_min: rec.effective_min,
                        });
                    }
                }
            }
        }

        // 域的类别数
        for (host, cats) in host_categories.iter() {
            let set: std::collections::BTreeSet<Option<String>> = cats.iter().cloned().collect();
            if let Some(h) = hosts.get_mut(host) {
                h.categories = set.len();
                h.category = cats.iter().find_map(|c| c.clone());
            }
        }

        // 模型的 Neuron（只在认得这个模型 id 时给）
        for m in models.values_mut() {
            let n = neurons_for(&m.model, m.input_tokens, m.output_tokens);
            m.neurons = n;
        }
        let known: Vec<&ModelStat> = models.values().filter(|m| m.neurons.is_some()).collect();
        r.unknown_model_calls = models
            .values()
            .filter(|m| m.neurons.is_none())
            .map(|m| m.calls)
            .sum();
        if !known.is_empty() {
            let total: f64 = known.iter().filter_map(|m| m.neurons).sum();
            r.neurons_total = Some(total);
            r.usd_estimate = Some(total / 1000.0 * USD_PER_1K_NEURONS);
        }

        // 排序
        r.days = days.into_values().collect();
        for d in r.days.iter_mut() {
            d.new_hosts = new_per_day.get(&d.day).copied().unwrap_or(0);
        }
        let mut hosts_vec: Vec<HostStat> = hosts.into_values().collect();
        hosts_vec.sort_by(|a, b| b.rows.cmp(&a.rows).then(a.host.cmp(&b.host)));

        // L0 候选：反复出现、几乎总是被 block、只判过一类、且判决真的生效过
        r.l0_candidates = hosts_vec
            .iter()
            .filter(|h| {
                h.rows >= L0_MIN_ROWS
                    && h.block_ratio() >= L0_MIN_BLOCK_RATIO
                    && h.categories <= 1
                    && h.applied > 0
            })
            .cloned()
            .collect();

        r.hosts = hosts_vec;
        let mut models_vec: Vec<ModelStat> = models.into_values().collect();
        models_vec.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.model.cmp(&b.model)));
        r.models = models_vec;
        r.new_hosts_per_day = new_per_day.into_iter().collect();
        breakage.sort_by(|a, b| {
            b.risk_of_breakage
                .partial_cmp(&a.risk_of_breakage)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.host.cmp(&b.host))
        });
        r.breakage_candidates = breakage;
        r
    }

    /// 平均每天多少个新域。
    pub fn mean_new_hosts_per_day(&self) -> f64 {
        if self.new_hosts_per_day.is_empty() {
            return 0.0;
        }
        let sum: usize = self.new_hosts_per_day.iter().map(|(_, n)| *n).sum();
        sum as f64 / self.new_hosts_per_day.len() as f64
    }

    /// 每天新域的 p95（nearest-rank：`ceil(0.95 * n)`，至少第 1 个）。
    pub fn p95_new_hosts_per_day(&self) -> usize {
        if self.new_hosts_per_day.is_empty() {
            return 0;
        }
        let mut v: Vec<usize> = self.new_hosts_per_day.iter().map(|(_, n)| *n).collect();
        v.sort_unstable();
        let n = v.len();
        let rank = ((n as f64) * 0.95).ceil() as usize;
        let idx = rank.saturating_sub(1).min(n - 1);
        v[idx]
    }

    /// 缓存命中率 = hits / (hits + misses)。**`cache_inherited` 是 hits 的子集，不再加一次。**
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            return None;
        }
        Some(self.cache_hits as f64 / total as f64)
    }

    /// 判决真正生效的比例（其余是演练模式或已被回滚）。
    pub fn applied_rate(&self) -> Option<f64> {
        if self.total_rows == 0 {
            return None;
        }
        Some(self.applied_rows as f64 / self.total_rows as f64)
    }

    /// 渲染成 Markdown。**报告里有域名** ⇒ 调用方负责写到仓库之外。
    pub fn render_markdown(&self) -> String {
        let mut s = String::new();
        s.push_str("# 意图判定：离线审计报告\n\n");

        if self.total_rows == 0 {
            s.push_str("审计文件里**一条记录都没有**。\n\n");
            s.push_str("> 这不代表「没有问题」，只代表「这里没有证据」：\n");
            s.push_str("> 要么意图过滤从未开启，要么审计被关了，要么文件已被轮转掉。\n");
            return s;
        }

        let range = match (self.first_ts_unix, self.last_ts_unix) {
            (Some(a), Some(b)) => format!("{} → {}", utc_day(a), utc_day(b)),
            _ => "未知".to_string(),
        };
        s.push_str(&format!(
            "- 记录数：**{}**\n- 覆盖：**{}**（{} 天）\n",
            self.total_rows,
            range,
            self.days.len()
        ));
        match self.cache_hit_rate() {
            Some(rate) => s.push_str(&format!("- 缓存命中率：**{:.1}%**（{} 命中 / {} 未命中）\n",
                rate * 100.0, self.cache_hits, self.cache_misses)),
            None => s.push_str("- 缓存命中率：**无法计算**（没有记录）\n"),
        }
        if let Some(rate) = self.applied_rate() {
            s.push_str(&format!(
                "- 判决真正生效：**{:.1}%**（未生效 {} 行 —— 演练模式或已被回滚）\n",
                rate * 100.0, self.unapplied_rows
            ));
        }
        if self.rows_with_context > 0 {
            s.push_str(&format!(
                "- ⚠️ 有 **{}** 行带着 `context_sent`（你显式开了「记录外发内容」）—— 同步时这些字段会被剥掉\n",
                self.rows_with_context
            ));
        }
        s.push('\n');

        // 每天
        s.push_str("## 每天\n\n| 天 | 行数 | block | allow | deferred | 新域 | 缓存命中 | 生效 |\n");
        s.push_str("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
        for d in &self.days {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                d.day, d.rows, d.block, d.allow, d.deferred, d.new_hosts, d.cache_hits, d.applied
            ));
        }

        // 新域分布
        s.push_str("\n## 新域 / 天（决定「要不要共享判决」的那个数）\n\n");
        s.push_str(&format!(
            "- 平均：**{:.1}** 个/天\n- p95：**{}** 个/天\n- 天数：{}\n",
            self.mean_new_hosts_per_day(),
            self.p95_new_hosts_per_day(),
            self.new_hosts_per_day.len()
        ));
        s.push_str("\n> 这个数直接决定前提：**每天新增多少个「从没见过的域」**。\n");
        s.push_str("> 它乘以单次判定的 Neuron 才是你每天的账。\n");

        // 成本
        s.push_str("\n## 成本（Token 与 Neuron）\n\n");
        s.push_str("| 模型 | 调用 | 有 usage 的 | 输入 token | 输出 token | Neuron |\n");
        s.push_str("| --- | ---: | ---: | ---: | ---: | ---: |\n");
        for m in &self.models {
            let n = match m.neurons {
                Some(v) => format!("{v:.1}"),
                None => "未知（不给估算）".to_string(),
            };
            s.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                m.model, m.calls, m.calls_with_usage, m.input_tokens, m.output_tokens, n
            ));
        }
        if let (Some(neurons), Some(usd)) = (self.neurons_total, self.usd_estimate) {
            s.push_str(&format!(
                "\n- 可估算的 Neuron 合计：**{neurons:.0}** ≈ **${usd:.4}**（按官方 ${USD_PER_1K_NEURONS}/1k Neuron）\n"
            ));
        }
        if self.unknown_model_calls > 0 {
            s.push_str(&format!(
                "- ⚠️ **{} 次调用的模型不在价目表里**，上面那行合计**不包含**它们 ⇒ 真实成本只会更高。\n",
                self.unknown_model_calls
            ));
        }
        s.push_str(&format!(
            "\n> Neuron 表快照：{}，来源 <{}>。表里没有的模型**一律不估算** —— 编一个数比不给更糟。\n",
            NEURON_TABLE_FETCHED, NEURON_TABLE_SOURCE
        ));

        // L0
        s.push_str("\n## L0 规则归纳候选（把反复出现的域固化成静态规则）\n\n");
        if self.l0_candidates.is_empty() {
            s.push_str("没有满足门槛的域（门槛：至少 3 次、被 block ≥ 80%、只判过一类、且生效过）。\n");
        } else {
            s.push_str(&format!(
                "**{}** 个域满足门槛。固化它们 = 以后这些域**不再问模型**（省 Neuron、少一次外发）：\n\n",
                self.l0_candidates.len()
            ));
            s.push_str("| 域 | 行数 | block 比例 | 类别 | 生效 | 首见 |\n");
            s.push_str("| --- | ---: | ---: | --- | ---: | --- |\n");
            for h in self.l0_candidates.iter().take(50) {
                s.push_str(&format!(
                    "| `{}` | {} | {:.0}% | {} | {} | {} |\n",
                    h.host,
                    h.rows,
                    h.block_ratio() * 100.0,
                    h.category.clone().unwrap_or_else(|| "—".into()),
                    h.applied,
                    h.first_day
                ));
            }
            let saved: usize = self.l0_candidates.iter().map(|h| h.rows).sum();
            s.push_str(&format!(
                "\n> 这些域一共产生了 **{}** 次判定 ⇒ 全部固化就是省下这么多调用。\n",
                saved
            ));
        }

        // 阈值
        s.push_str("\n## 阈值校准候选（被 block 但「可能弄坏」的风险不低）\n\n");
        if self.breakage_candidates.is_empty() {
            s.push_str("没有 `risk_of_breakage` ≥ 0.2 却仍被 block 的样本。\n");
        } else {
            s.push_str(&format!(
                "**{}** 条样本。它们不一定错，但**是唯一值得逐条看的证据**：\n\n",
                self.breakage_candidates.len()
            ));
            s.push_str("| 域 | 天 | ads_intent | risk_of_breakage | effective_min |\n");
            s.push_str("| --- | --- | ---: | ---: | ---: |\n");
            for b in self.breakage_candidates.iter().take(50) {
                s.push_str(&format!(
                    "| `{}` | {} | {} | {:.2} | {} |\n",
                    b.host,
                    b.day,
                    fmt_prob(b.ads_intent),
                    b.risk_of_breakage,
                    fmt_prob(b.effective_min)
                ));
            }
        }

        // Top hosts
        s.push_str("\n## 判定次数最多的域（前 30）\n\n| 域 | 行数 | block | allow | deferred | 类别数 |\n");
        s.push_str("| --- | ---: | ---: | ---: | ---: | ---: |\n");
        for h in self.hosts.iter().take(30) {
            s.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                h.host, h.rows, h.block, h.allow, h.deferred, h.categories
            ));
        }

        // 边界
        s.push_str("\n---\n\n## 这份报告**不能**说明什么（先说清楚）\n\n");
        s.push_str("1. 全是**候选**，不是结论。没有标注就算不出 precision/recall ——\n");
        s.push_str("   要那个数请用 `cargo run -p xt-intent --example eval_domains`。\n");
        s.push_str("2. 「DNS 里出现了某个域」**不等于**用户看到了广告。\n");
        s.push_str("3. `usage` 缺失的调用会让成本账**偏低**，不会偏高。\n");
        s.push_str("4. 报告里有**域名**（= 浏览记录）⇒ 不要提交进仓库。\n");
        s
    }
}

fn fmt_prob(v: Option<f32>) -> String {
    match v {
        Some(x) => format!("{x:.2}"),
        None => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::Usage;

    fn rec(ts: u64, host: &str, outcome: &str, applied: bool) -> AuditRecord {
        AuditRecord {
            ts_unix: ts,
            host: host.into(),
            outcome: outcome.into(),
            reason: None,
            category: Some("ad_or_monetization".into()),
            ads_intent: Some(0.97),
            risk_of_breakage: Some(0.05),
            choice_confidence: Some(0.93),
            effective_min: Some(0.85),
            applied,
            cache_hit: false,
            model: Some("jev-1.13-free".into()),
            usage: Some(Usage { input_tokens: 90, output_tokens: 12 }),
            context_sent: None,
        }
    }

    /// 已知日期必须对得上 —— 这是"不引 chrono"的代价换来的唯一保险。
    #[test]
    fn utc_day_matches_known_dates() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1), "1970-01-01");
        assert_eq!(utc_day(86_399), "1970-01-01");
        assert_eq!(utc_day(86_400), "1970-01-02");
        // 闰年 2024-02-29（2024-01-01 = 19723 天）
        assert_eq!(utc_day(19_723 * 86_400), "2024-01-01");
        assert_eq!(utc_day(19_782 * 86_400), "2024-02-29");
        assert_eq!(utc_day(19_783 * 86_400), "2024-03-01");
        // 世纪边界：2100 / 2200 都不是闰年（1970→2270 共 109573 天）
        assert_eq!(utc_day(109_573 * 86_400 - 86_400), "2269-12-31");
        assert_eq!(utc_day(109_573 * 86_400), "2270-01-01");
    }

    #[test]
    fn utc_day_is_monotonic_across_a_leap_year_boundary() {
        let mut prev = String::new();
        // 2024-02-27 → 2024-03-02
        let start = 19_780 * 86_400;
        for i in 0..5 {
            let d = utc_day(start + i * 86_400);
            if !prev.is_empty() {
                assert!(d > prev, "{d} 不大于 {prev}");
            }
            prev = d;
        }
        assert_eq!(prev, "2024-03-02");
    }

    #[test]
    fn neuron_rates_come_only_from_the_table_and_never_get_extrapolated() {
        // 官方表里的 llama-3.1-8b-instruct：25,608 / 75,147 per M
        let n = neurons_for("@cf/meta/llama-3.1-8b-instruct", 1_000_000, 0).unwrap();
        assert!((n - 25_608.0).abs() < 0.01, "{n}");
        let n = neurons_for("@cf/meta/llama-3.1-8b-instruct", 0, 1_000_000).unwrap();
        assert!((n - 75_147.0).abs() < 0.01, "{n}");
        // 1000 输入 + 80 输出 ≈ 31.6 neurons（这就是给用户算过的那个数）
        let n = neurons_for("@cf/meta/llama-3.1-8b-instruct", 1_000, 80).unwrap();
        assert!((n - 31.62).abs() < 0.05, "{n}");
        // 不认识就是 None —— 绝不外推
        assert!(neurons_for("jev-1.13-free", 1000, 80).is_none());
        assert!(neurons_for("@cf/meta/some-future-model", 1000, 80).is_none());
    }

    #[test]
    fn an_empty_corpus_says_there_is_no_evidence_instead_of_looking_clean() {
        let r = AuditReport::build(&[]);
        assert_eq!(r.total_rows, 0);
        assert_eq!(r.cache_hit_rate(), None);
        assert_eq!(r.applied_rate(), None);
        let md = r.render_markdown();
        assert!(md.contains("一条记录都没有"), "{md}");
        assert!(md.contains("不代表"), "{md}");
    }

    #[test]
    fn days_and_counters_are_aggregated_per_utc_day() {
        // 2026-09-23T23:59:59Z 与 2026-09-24T00:00:00Z 分属两天
        let day23 = 20_719 * 86_400 + 86_399;
        let day24 = 20_720 * 86_400;
        let recs = vec![
            rec(day23, "ads.example", "block", true),
            rec(day23, "ads.example", "block", true),
            rec(day24, "ads.example", "allow", false),
            rec(day24, "other.example", "deferred", false),
        ];
        let r = AuditReport::build(&recs);
        assert_eq!(r.days.len(), 2);
        assert_eq!(r.days[0].day, "2026-09-23");
        assert_eq!(r.days[0].block, 2);
        assert_eq!(r.days[1].allow, 1);
        assert_eq!(r.days[1].deferred, 1);
        // 新域：23 号 1 个（ads.example），24 号 1 个（other.example）
        assert_eq!(r.new_hosts_per_day, vec![("2026-09-23".to_string(), 1), ("2026-09-24".to_string(), 1)]);
    }

    #[test]
    fn cache_hit_rate_does_not_double_count_inherited_hits() {
        let mut recs = Vec::new();
        for _ in 0..44 {
            let mut r = rec(20_720 * 86_400, "a.example", "allow", false);
            r.cache_hit = true;
            recs.push(r);
        }
        for _ in 0..56 {
            recs.push(rec(20_720 * 86_400, "b.example", "allow", false));
        }
        let r = AuditReport::build(&recs);
        // 44 / (44 + 56) = 44%，不是 56%
        let rate = r.cache_hit_rate().unwrap();
        assert!((rate - 0.44).abs() < 1e-9, "{rate}");
    }

    #[test]
    fn l0_candidates_need_repetition_consistency_and_a_real_effect() {
        let t = 20_720 * 86_400;
        // a：3 次全 block、生效 ⇒ 候选
        // b：3 次但只有 2 次 block（66%） ⇒ 不是候选
        // c：3 次全 block 但从未生效 ⇒ 不是候选
        // d：只出现 2 次 ⇒ 不是候选
        let mut recs = Vec::new();
        for _ in 0..3 {
            recs.push(rec(t, "a.example", "block", true));
        }
        for i in 0..3 {
            recs.push(rec(t, "b.example", if i < 2 { "block" } else { "allow" }, true));
        }
        for _ in 0..3 {
            recs.push(rec(t, "c.example", "block", false));
        }
        for _ in 0..2 {
            recs.push(rec(t, "d.example", "block", true));
        }
        let r = AuditReport::build(&recs);
        let hosts: Vec<&str> = r.l0_candidates.iter().map(|h| h.host.as_str()).collect();
        assert_eq!(hosts, vec!["a.example"], "{hosts:?}");
        let a = &r.l0_candidates[0];
        assert!((a.block_ratio() - 1.0).abs() < 1e-6);
        assert_eq!(a.applied, 3);
        // 报告里要给出"固化能省多少"
        let md = r.render_markdown();
        assert!(md.contains("省下这么多调用"), "{md}");
    }

    #[test]
    fn breakage_candidates_only_include_blocks_with_real_risk() {
        let t = 20_720 * 86_400;
        let mut low = rec(t, "low.example", "block", true);
        low.risk_of_breakage = Some(0.05);
        let mut high = rec(t, "high.example", "block", true);
        high.risk_of_breakage = Some(0.41);
        let mut allowed = rec(t, "allowed.example", "allow", true);
        allowed.risk_of_breakage = Some(0.9); // allow 的不算候选
        let r = AuditReport::build(&[low, high, allowed]);
        let hosts: Vec<&str> = r.breakage_candidates.iter().map(|b| b.host.as_str()).collect();
        assert_eq!(hosts, vec!["high.example"], "{hosts:?}");
    }

    #[test]
    fn p95_uses_nearest_rank_and_never_panics_on_tiny_samples() {
        let t = 20_720 * 86_400;
        let mut recs = Vec::new();
        // 1 天 1 个新域
        recs.push(rec(t, "a.example", "allow", false));
        let r = AuditReport::build(&recs);
        assert_eq!(r.p95_new_hosts_per_day(), 1);
        assert!((r.mean_new_hosts_per_day() - 1.0).abs() < 1e-9);

        // 20 天，每天 1..20 个新域 ⇒ p95 应该是第 19 个（nearest-rank）
        let mut recs = Vec::new();
        for d in 0..20u64 {
            for i in 0..=d {
                recs.push(rec((20_720 + d) * 86_400, &format!("h{i}.example"), "allow", false));
            }
        }
        let r = AuditReport::build(&recs);
        assert_eq!(r.new_hosts_per_day.len(), 20);
        assert_eq!(r.p95_new_hosts_per_day(), 19);
    }

    #[test]
    fn unknown_models_are_reported_as_not_estimated_instead_of_zero() {
        let t = 20_720 * 86_400;
        let r = AuditReport::build(&[rec(t, "a.example", "block", true)]);
        assert_eq!(r.unknown_model_calls, 1);
        assert_eq!(r.neurons_total, None);
        assert_eq!(r.usd_estimate, None);
        let md = r.render_markdown();
        assert!(md.contains("不给估算"), "{md}");
        assert!(md.contains("不包含"), "必须说明合计不含未知模型：{md}");
    }

    #[test]
    fn a_known_model_produces_a_neuron_total_and_a_dollar_estimate() {
        let t = 20_720 * 86_400;
        let mut r0 = rec(t, "a.example", "block", true);
        r0.model = Some("@cf/meta/llama-3.1-8b-instruct".into());
        r0.usage = Some(Usage { input_tokens: 1_000, output_tokens: 80 });
        let r = AuditReport::build(&[r0]);
        let n = r.neurons_total.unwrap();
        assert!((n - 31.62).abs() < 0.05, "{n}");
        assert_eq!(r.unknown_model_calls, 0);
        // $0.011 / 1000 neurons
        let usd = r.usd_estimate.unwrap();
        assert!((usd - n / 1000.0 * USD_PER_1K_NEURONS).abs() < 1e-12);
        let md = r.render_markdown();
        assert!(md.contains(NEURON_TABLE_FETCHED), "{md}");
        assert!(md.contains(NEURON_TABLE_SOURCE), "{md}");
    }

    #[test]
    fn rows_with_context_are_counted_and_flagged() {
        let t = 20_720 * 86_400;
        let mut with = rec(t, "a.example", "allow", false);
        with.context_sent = Some("{\"state\":\"...\"}".into());
        let r = AuditReport::build(&[with, rec(t, "b.example", "allow", false)]);
        assert_eq!(r.rows_with_context, 1);
        assert!(r.render_markdown().contains("记录外发内容"));
    }

    #[test]
    fn applied_rate_counts_drill_mode_rows_as_not_applied() {
        let t = 20_720 * 86_400;
        let r = AuditReport::build(&[
            rec(t, "a.example", "block", true),
            rec(t, "b.example", "block", false),
            rec(t, "c.example", "block", false),
            rec(t, "d.example", "block", false),
        ]);
        let rate = r.applied_rate().unwrap();
        assert!((rate - 0.25).abs() < 1e-9, "{rate}");
        assert_eq!(r.unapplied_rows, 3);
        assert!(r.render_markdown().contains("演练模式"));
    }

    #[test]
    fn a_host_judged_into_two_categories_is_not_an_l0_candidate() {
        let t = 20_720 * 86_400;
        let mut a = rec(t, "flaky.example", "block", true);
        a.category = Some("ad_or_monetization".into());
        let mut b = rec(t, "flaky.example", "block", true);
        b.category = Some("cdn_or_infra".into());
        let mut c = rec(t, "flaky.example", "block", true);
        c.category = Some("api_or_service".into());
        let r = AuditReport::build(&[a, b, c]);
        let h = r.hosts.iter().find(|h| h.host == "flaky.example").unwrap();
        assert_eq!(h.categories, 3);
        assert!(r.l0_candidates.is_empty(), "类别不一致就不该固化");
    }

    #[test]
    fn report_warns_that_it_is_candidates_not_conclusions() {
        let t = 20_720 * 86_400;
        let r = AuditReport::build(&[rec(t, "a.example", "block", true)]);
        let md = r.render_markdown();
        assert!(md.contains("候选"), "{md}");
        assert!(md.contains("不等于"), "必须说明 DNS 出现 ≠ 用户看到广告：{md}");
        assert!(md.contains("不要提交进仓库"), "必须提示域名是浏览记录：{md}");
    }
}
