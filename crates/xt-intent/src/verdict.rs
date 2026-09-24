//! 判决：三道问题的答案怎么变成一个「拦 / 不拦」的结论。
//!
//! # 闸门是**三条件 AND**
//!
//! 1. 类别是投放/追踪（`choice` 白名单）；
//! 2. `ads_intent ≥ 阈值`（`noul`）；
//! 3. **`risk_of_breakage ≤ 阈值`**（`noul`）—— 这一条是误杀的刹车。
//!
//! 第 3 条不是装饰：它让模型有机会说"我知道它像广告，但拦了会坏"。少了它，
//! 一个把 CDN 判成广告的模型错误会直接变成"用户网页白屏"。
//!
//! # 顺序是有意义的
//!
//! 四个"不拦"的理由（破解风险高 / 类别不可拦 / 置信度低 / 分数不够）都返回
//! [`Verdict::Allow`]，但记录**第一个命中**的理由。顺序按"信息量"排：
//! 刹车最先，因为它最容易被忽略，而它在审计里最该被看见。

use serde::{Deserialize, Serialize};

use crate::answer::Answers;
use crate::question::{KIND_LABELS, Q_ADS_INTENT, Q_ENDPOINT_KIND, Q_RISK_OF_BREAKAGE};

/// 端点类别。字符串形态是**持久化格式**（写进缓存与审计），不要改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    AdOrMonetization,
    TrackerOrAnalytics,
    CdnOrInfra,
    ApiOrService,
    HumanSite,
    Unknown,
}

impl Category {
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "ad_or_monetization" => Self::AdOrMonetization,
            "tracker_or_analytics" => Self::TrackerOrAnalytics,
            "cdn_or_infra" => Self::CdnOrInfra,
            "api_or_service" => Self::ApiOrService,
            "human_site" => Self::HumanSite,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdOrMonetization => "ad_or_monetization",
            Self::TrackerOrAnalytics => "tracker_or_analytics",
            Self::CdnOrInfra => "cdn_or_infra",
            Self::ApiOrService => "api_or_service",
            Self::HumanSite => "human_site",
            Self::Unknown => "unknown",
        }
    }

    /// 中文标签（只用于界面，不参与任何判断）。
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::AdOrMonetization => "广告/投放",
            Self::TrackerOrAnalytics => "追踪/统计",
            Self::CdnOrInfra => "CDN/基础设施",
            Self::ApiOrService => "接口/服务",
            Self::HumanSite => "人类站点",
            Self::Unknown => "未知",
        }
    }
}

/// 定罪的证据。**每一条都要能展示给用户** —— 判决必须可申诉。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockVerdict {
    pub category: Category,
    pub ads_intent: f32,
    pub risk_of_breakage: f32,
    pub choice_confidence: f32,
    /// 本次生效的 `ads_intent` 阈值（已含形状加分）。
    pub effective_min: f32,
}

/// 模型给了答案，但闸门说不拦。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AllowReason {
    /// 拦了会坏 —— 优先于其它一切理由。
    BreakageRiskTooHigh { risk: f32, max: f32 },
    /// 类别不是投放/追踪。
    CategoryNotBlockable { category: Category },
    /// 方向对但模型自己不确定。
    LowConfidence { confidence: f32, min: f32 },
    /// 分数没到。
    BelowThreshold { ads_intent: f32, effective_min: f32 },
}

/// 拿不到可用的答案（= 放行 + 审计）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DeferReason {
    /// 期望的答案键不存在（服务端少给了一个）。
    MissingAnswer { id: String },
    /// 答案存在但不是合法形状（空对象 / 未知 choice 标签 / 非有限数）。
    SchemaInvalid { id: String },
    /// 本地预算用尽。
    BudgetExhausted { scope: String },
    /// 网关不可达 / 超时 / 非 2xx。
    GatewayUnavailable { message: String },
    /// 这个主机名压根不该问（IP 字面量 / 内网 / 单标签名 / 白名单）。
    NotCandidate { why: String },
    /// 功能被关掉，或处于演练模式（演练模式仍会问，但 `applied=false`）。
    Disabled,
}

impl DeferReason {
    /// 稳定的短名字，写进审计与统计。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingAnswer { .. } => "missing_answer",
            Self::SchemaInvalid { .. } => "schema_invalid",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::GatewayUnavailable { .. } => "gateway_unavailable",
            Self::NotCandidate { .. } => "not_candidate",
            Self::Disabled => "disabled",
        }
    }
}

/// 一次判定的结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Block(BlockVerdict),
    Allow(AllowReason),
    Deferred(DeferReason),
}

impl Verdict {
    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block(_))
    }

    /// 审计与统计用的稳定名字。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Block(_) => "block",
            Self::Allow(_) => "allow",
            Self::Deferred(_) => "deferred",
        }
    }

    /// 更细的原因（`allow`/`deferred` 用，`block` 返回 `None`）。
    pub fn reason_str(&self) -> Option<&'static str> {
        match self {
            Self::Block(_) => None,
            Self::Allow(r) => Some(match r {
                AllowReason::BreakageRiskTooHigh { .. } => "breakage_risk_too_high",
                AllowReason::CategoryNotBlockable { .. } => "category_not_blockable",
                AllowReason::LowConfidence { .. } => "low_confidence",
                AllowReason::BelowThreshold { .. } => "below_threshold",
            }),
            Self::Deferred(r) => Some(r.as_str()),
        }
    }

    pub fn block_evidence(&self) -> Option<&BlockVerdict> {
        match self {
            Self::Block(b) => Some(b),
            _ => None,
        }
    }
}

/// 闸门阈值。全部可调，且**必须**能用离线夹具校准（见设计文档 §10-P1.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// `ads_intent`（「是」的概率）下限。
    pub ads_intent_min: f32,
    /// `choice` 的 confidence 下限。
    pub choice_confidence_min: f32,
    /// `risk_of_breakage` 的上限：**超过就永远不拦**。
    pub risk_of_breakage_max: f32,
    /// 哪些类别可以定罪。
    pub block_categories: Vec<Category>,
    /// 形状加分最多能把阈值压低多少。默认 0.10 —— 有测试断言
    /// 这个上限**不可能**把 `ads_intent=0` 抬过闸门。
    pub shape_bonus_max: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            ads_intent_min: 0.85,
            choice_confidence_min: 0.5,
            risk_of_breakage_max: 0.3,
            block_categories: vec![Category::AdOrMonetization, Category::TrackerOrAnalytics],
            shape_bonus_max: 0.10,
        }
    }
}

impl Thresholds {
    /// 设置页保存前调它 —— 越界的阈值比错误的判定更难排查。
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        let unit = |v: f32, name: &str, errs: &mut Vec<String>| {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                errs.push(format!("{name} 必须在 0..=1，现在是 {v}"));
            }
        };
        unit(self.ads_intent_min, "ads_intent_min", &mut errs);
        unit(self.choice_confidence_min, "choice_confidence_min", &mut errs);
        unit(self.risk_of_breakage_max, "risk_of_breakage_max", &mut errs);
        unit(self.shape_bonus_max, "shape_bonus_max", &mut errs);
        if self.block_categories.is_empty() {
            errs.push("block_categories 不能为空（空就等于永不拦截）".into());
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    /// 形状加分之后的实际阈值。`bonus` 会被夹到 `[0, shape_bonus_max]`。
    pub fn effective_ads_min(&self, bonus: f32) -> f32 {
        let bonus = if bonus.is_finite() { bonus.max(0.0).min(self.shape_bonus_max) } else { 0.0 };
        (self.ads_intent_min - bonus).max(0.0)
    }
}

/// 走一遍闸门。`shape_bonus` 来自 [`crate::shape`]（只调阈值，永不定罪）。
pub fn decide(answers: &Answers, t: &Thresholds, shape_bonus: f32) -> Verdict {
    let (kind_label, choice_confidence) = match answers.choice(Q_ENDPOINT_KIND, KIND_LABELS) {
        Some(v) => v,
        // 健忘：分不清"缺答案"与"答案非法"会让审计失去意义，所以分开报。
        None => {
            return Verdict::Deferred(if answers.has(Q_ENDPOINT_KIND) {
                DeferReason::SchemaInvalid { id: Q_ENDPOINT_KIND.into() }
            } else {
                DeferReason::MissingAnswer { id: Q_ENDPOINT_KIND.into() }
            })
        }
    };
    let category = match Category::from_label(&kind_label) {
        Some(c) => c,
        None => return Verdict::Deferred(DeferReason::SchemaInvalid { id: Q_ENDPOINT_KIND.into() }),
    };

    let ads_intent = match answers.noul(Q_ADS_INTENT) {
        Some(v) => v,
        None => {
            return Verdict::Deferred(if answers.has(Q_ADS_INTENT) {
                DeferReason::SchemaInvalid { id: Q_ADS_INTENT.into() }
            } else {
                DeferReason::MissingAnswer { id: Q_ADS_INTENT.into() }
            })
        }
    };
    let risk = match answers.noul(Q_RISK_OF_BREAKAGE) {
        Some(v) => v,
        None => {
            return Verdict::Deferred(if answers.has(Q_RISK_OF_BREAKAGE) {
                DeferReason::SchemaInvalid { id: Q_RISK_OF_BREAKAGE.into() }
            } else {
                DeferReason::MissingAnswer { id: Q_RISK_OF_BREAKAGE.into() }
            })
        }
    };

    // 1) 刹车最先：拦了会坏，别的都不用看了。
    if risk > t.risk_of_breakage_max {
        return Verdict::Allow(AllowReason::BreakageRiskTooHigh {
            risk,
            max: t.risk_of_breakage_max,
        });
    }
    // 2) 类别不在可拦集合里。
    if !t.block_categories.contains(&category) {
        return Verdict::Allow(AllowReason::CategoryNotBlockable { category });
    }
    // 3) 模型自己不确定。
    if choice_confidence < t.choice_confidence_min {
        return Verdict::Allow(AllowReason::LowConfidence {
            confidence: choice_confidence,
            min: t.choice_confidence_min,
        });
    }
    // 4) 分数不够（含形状加分后的实际阈值）。
    let effective_min = t.effective_ads_min(shape_bonus);
    if ads_intent < effective_min {
        return Verdict::Allow(AllowReason::BelowThreshold { ads_intent, effective_min });
    }

    Verdict::Block(BlockVerdict {
        category,
        ads_intent,
        risk_of_breakage: risk,
        choice_confidence,
        effective_min,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{KIND_AD_OR_MONETIZATION, KIND_CDN_OR_INFRA, KIND_HUMAN_SITE};
    use serde_json::json;

    /// 构造一份答案：`(category, ads, risk, confidence)`。
    fn answers(category: &str, ads: f32, risk: f32, confidence: f32) -> Answers {
        Answers::from_wire(&json!({
            "answers": {
                Q_ENDPOINT_KIND: { "type": "choice", "choice": category, "confidence": confidence },
                Q_ADS_INTENT: { "type": "noul", "noul": ads },
                Q_RISK_OF_BREAKAGE: { "type": "noul", "noul": risk }
            }
        }))
        .unwrap()
    }

    #[test]
    fn the_three_conditions_must_all_hold() {
        let t = Thresholds::default();

        // 典型广告：类别对、分数高、风险低 → 拦。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93), &t, 0.0);
        assert!(v.is_block(), "{v:?}");
        let b = v.block_evidence().unwrap();
        assert_eq!(b.category, Category::AdOrMonetization);
        assert_eq!(b.effective_min, 0.85);

        // 分数够但类别是 CDN → 不拦。
        let v = decide(&answers(KIND_CDN_OR_INFRA, 0.99, 0.05, 0.99), &t, 0.0);
        assert_eq!(
            v,
            Verdict::Allow(AllowReason::CategoryNotBlockable { category: Category::CdnOrInfra })
        );

        // 类别对、分数高，但拦了会坏 → 不拦（这就是刹车）。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 1.0, 0.55, 0.99), &t, 0.0);
        assert_eq!(
            v,
            Verdict::Allow(AllowReason::BreakageRiskTooHigh { risk: 0.55, max: 0.3 })
        );

        // 分数差一点 → 不拦。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 0.80, 0.05, 0.99), &t, 0.0);
        assert_eq!(v, Verdict::Allow(AllowReason::BelowThreshold { ads_intent: 0.80, effective_min: 0.85 }));

        // 方向对但模型不确定 → 不拦。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 0.99, 0.05, 0.31), &t, 0.0);
        assert_eq!(v, Verdict::Allow(AllowReason::LowConfidence { confidence: 0.31, min: 0.5 }));
    }

    #[test]
    fn the_brake_outranks_every_other_reason() {
        let t = Thresholds::default();
        // 类别不对 + 分数不够 + 风险高 ⇒ 报出来的理由是「拦了会坏」。
        let v = decide(&answers(KIND_HUMAN_SITE, 0.10, 0.9, 0.1), &t, 0.0);
        assert!(matches!(
            v,
            Verdict::Allow(AllowReason::BreakageRiskTooHigh { .. })
        ));
    }

    #[test]
    fn a_missing_or_broken_answer_never_blocks() {
        let t = Thresholds::default();

        // 少一个答案。
        let partial = Answers::from_wire(&json!({
            "answers": {
                Q_ENDPOINT_KIND: { "type": "choice", "choice": KIND_AD_OR_MONETIZATION, "confidence": 0.99 },
                Q_ADS_INTENT: { "type": "noul", "noul": 0.99 }
            }
        }))
        .unwrap();
        assert_eq!(
            decide(&partial, &t, 0.0),
            Verdict::Deferred(DeferReason::MissingAnswer { id: Q_RISK_OF_BREAKAGE.into() })
        );

        // 答案是空对象（ext 那边会静默放过）。
        let empty = Answers::from_wire(&json!({
            "answers": {
                Q_ENDPOINT_KIND: { "type": "choice", "choice": KIND_AD_OR_MONETIZATION, "confidence": 0.99 },
                Q_ADS_INTENT: {},
                Q_RISK_OF_BREAKAGE: { "type": "noul", "noul": 0.0 }
            }
        }))
        .unwrap();
        assert_eq!(
            decide(&empty, &t, 0.0),
            Verdict::Deferred(DeferReason::SchemaInvalid { id: Q_ADS_INTENT.into() })
        );

        // 未知 choice 标签。
        let weird = answers("totally_an_ad", 0.99, 0.0, 0.99);
        assert_eq!(
            decide(&weird, &t, 0.0),
            Verdict::Deferred(DeferReason::SchemaInvalid { id: Q_ENDPOINT_KIND.into() })
        );
    }

    #[test]
    fn shape_bonus_can_lower_the_bar_but_can_never_create_a_block() {
        let t = Thresholds::default();
        // 0.80 差一点点：形状强（吃满 0.10 加分）时阈值降到 0.75 → 拦。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 0.80, 0.05, 0.99), &t, 0.10);
        assert!(v.is_block(), "{v:?}");

        // 但**零信号**永远拦不住，无论形状多强。
        let v = decide(&answers(KIND_AD_OR_MONETIZATION, 0.0, 0.05, 0.99), &t, 1.0);
        assert!(!v.is_block(), "形状加分绝不能单独定罪：{v:?}");
        assert_eq!(v, Verdict::Allow(AllowReason::BelowThreshold { ads_intent: 0.0, effective_min: 0.75 }));

        // 加分被夹在上限内。
        assert_eq!(t.effective_ads_min(99.0), 0.75);
        assert_eq!(t.effective_ads_min(-5.0), 0.85);
        assert_eq!(t.effective_ads_min(f32::NAN), 0.85);
    }

    #[test]
    fn thresholds_validate() {
        assert!(Thresholds::default().validate().is_ok());

        let t = Thresholds {
            ads_intent_min: 1.5,
            block_categories: Vec::new(),
            ..Default::default()
        };
        let errs = t.validate().unwrap_err();
        assert_eq!(errs.len(), 2, "{errs:?}");
    }

    #[test]
    fn category_labels_round_trip() {
        for label in KIND_LABELS {
            let c = Category::from_label(label).expect(label);
            assert_eq!(c.as_str(), *label);
        }
        assert_eq!(Category::from_label("nope"), None);
    }

    #[test]
    fn reason_names_are_stable() {
        // 这些字符串会写进审计与统计 —— 改名等于让历史数据失去意义。
        assert_eq!(DeferReason::BudgetExhausted { scope: "day".into() }.as_str(), "budget_exhausted");
        assert_eq!(Verdict::Block(BlockVerdict {
            category: Category::AdOrMonetization,
            ads_intent: 0.9,
            risk_of_breakage: 0.0,
            choice_confidence: 0.9,
            effective_min: 0.85,
        })
        .kind_str(), "block");
    }
}
