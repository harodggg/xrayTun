//! 解析 Jev 的答案，**防御性地**。
//!
//! # 为什么每一处都要显式判空
//!
//! `jev-x-filter` 的两条实测教训，这里都不重犯：
//!
//! 1. 它的存在性检查是 `!answers[id]` —— 一个**空对象 `{}` 能通过**，
//!    然后静默取默认值 0（`pipeline.js:278`）。我们会要求答案对象里**真的有**
//!    对应类型的字段。
//! 2. 它的 `readAnswers` **不校验 `choice` 标签**：上游返回一个我们不认识的标签
//!    会被原样当成有效类别（它的测试里 `'nope'` 就是这么活下来的）。
//!    我们要求标签落在白名单里，否则判 `Unknown`。
//!
//! 原则：**读不出答案 ≠ 答案是"否"**。读不出就是 `None`，由上层判 `Deferred`（放行），
//! 而不是伪造一个 0 去参与判决。

use serde_json::{Map, Value};

use crate::clamp01;

/// 一份原始答案（`answers` 对象）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answers {
    raw: Map<String, Value>,
}

impl Answers {
    /// 从完整响应体里取 `answers`。顶层不是对象、或 `answers` 不是对象 → `None`。
    pub fn from_wire(body: &Value) -> Option<Self> {
        let answers = body.get("answers")?;
        let map = answers.as_object()?;
        Some(Self { raw: map.clone() })
    }

    /// 直接从一个 `answers` 对象构造（测试与离线夹具用）。
    pub fn from_map(map: Map<String, Value>) -> Self {
        Self { raw: map }
    }

    pub fn has(&self, id: &str) -> bool {
        self.raw.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.raw.keys().map(|k| k.as_str()).collect()
    }

    /// 取 `noul`（「是」的概率）。**不是有限数就不是答案。**
    ///
    /// `type` 字段若存在则必须与期望一致 —— 上游会回显 `type`，
    /// 把它当作一致性校验几乎零成本。
    pub fn noul(&self, id: &str) -> Option<f32> {
        let entry = self.raw.get(id)?.as_object()?;
        if let Some(t) = entry.get("type") {
            if t.as_str() != Some("noul") {
                return None;
            }
        }
        // 注意：`{}` 在这里返回 None（get 失败），这正是我们要的。
        let v = entry.get("noul")?.as_f64()?;
        if !v.is_finite() {
            return None;
        }
        Some(clamp01(v as f32))
    }

    /// 取 `choice`：标签必须在 `allowed` 里。返回 `(标签, confidence)`。
    ///
    /// `confidence` 缺失或非法时取 **0.0** —— 这不是"保守猜测"，
    /// 而是一个会**必然失败**于置信度闸门的值（fail-closed：不拦）。
    pub fn choice(&self, id: &str, allowed: &[&str]) -> Option<(String, f32)> {
        let entry = self.raw.get(id)?.as_object()?;
        if let Some(t) = entry.get("type") {
            if t.as_str() != Some("choice") {
                return None;
            }
        }
        let label = entry.get("choice")?.as_str()?;
        if !allowed.contains(&label) {
            return None;
        }
        let confidence = entry
            .get("confidence")
            .and_then(Value::as_f64)
            .filter(|c| c.is_finite())
            .map(|c| clamp01(c as f32))
            .unwrap_or(0.0);
        Some((label.to_string(), confidence))
    }

    /// 取某标签的概率（`choice` 的 `probabilities` 里）。用于二级闸门。
    pub fn choice_probability(&self, id: &str, label: &str) -> Option<f32> {
        let entry = self.raw.get(id)?.as_object()?;
        let v = entry.get("probabilities")?.as_object()?.get(label)?.as_f64()?;
        if !v.is_finite() {
            return None;
        }
        Some(clamp01(v as f32))
    }

    /// 取 `score`（可能是小数，上游给的是加权期望）。
    pub fn score(&self, id: &str) -> Option<f32> {
        let entry = self.raw.get(id)?.as_object()?;
        if let Some(t) = entry.get("type") {
            if t.as_str() != Some("score") {
                return None;
            }
        }
        let v = entry.get("score")?.as_f64()?;
        if !v.is_finite() {
            return None;
        }
        Some(v as f32)
    }

    /// 把答案里某个 id 的原始值拿出来（审计用：要能展示模型的原话）。
    pub fn raw_of(&self, id: &str) -> Option<&Value> {
        self.raw.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answers() -> Answers {
        Answers::from_wire(&json!({
            "model": "jev-latest",
            "answers": {
                "endpoint_kind": {
                    "type": "choice", "choice": "ad_or_monetization", "confidence": 0.93,
                    "probabilities": { "ad_or_monetization": 0.93, "unknown": 0.07 }
                },
                "ads_intent": { "type": "noul", "noul": 0.97 },
                "risk_of_breakage": { "type": "noul", "noul": 0.08 }
            }
        }))
        .unwrap()
    }

    #[test]
    fn reads_the_three_answer_kinds() {
        let a = answers();
        assert_eq!(a.noul("ads_intent"), Some(0.97));
        assert_eq!(a.noul("risk_of_breakage"), Some(0.08));
        let (label, conf) = a.choice("endpoint_kind", crate::question::KIND_LABELS).unwrap();
        assert_eq!(label, "ad_or_monetization");
        assert!((conf - 0.93).abs() < 1e-6);
        assert_eq!(a.choice_probability("endpoint_kind", "ad_or_monetization"), Some(0.93));
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn an_empty_object_is_not_an_answer() {
        // 这正是 `jev-x-filter` 会静默放过的形状（`!answers[id]` 为 false）。
        let a = Answers::from_wire(&json!({ "answers": { "ads_intent": {} } })).unwrap();
        assert!(a.has("ads_intent"));
        assert_eq!(a.noul("ads_intent"), None, "空对象必须读不出答案");
    }

    #[test]
    fn an_unknown_choice_label_is_refused() {
        let a = Answers::from_wire(&json!({
            "answers": { "endpoint_kind": { "type": "choice", "choice": "nope", "confidence": 0.99 } }
        }))
        .unwrap();
        // 上游返回了模型自己的词，不在白名单里 ⇒ 不认。
        assert!(a.choice("endpoint_kind", crate::question::KIND_LABELS).is_none());
    }

    #[test]
    fn type_mismatch_is_refused() {
        let a = Answers::from_wire(&json!({
            "answers": { "x": { "type": "score", "noul": 0.9 } }
        }))
        .unwrap();
        assert_eq!(a.noul("x"), None);
    }

    #[test]
    fn non_finite_numbers_are_refused_without_guessing() {
        // JSON 里没有 NaN 字面量，但字符串 / null / bool 都可能出现。
        let a = Answers::from_wire(&json!({
            "answers": {
                "s": { "type": "noul", "noul": "0.9" },
                "n": { "type": "noul", "noul": null },
                "b": { "type": "noul", "noul": true }
            }
        }))
        .unwrap();
        assert_eq!(a.noul("s"), None);
        assert_eq!(a.noul("n"), None);
        assert_eq!(a.noul("b"), None);
    }

    #[test]
    fn values_are_clamped_but_never_invented() {
        let a = Answers::from_wire(&json!({
            "answers": {
                "hi": { "type": "noul", "noul": 1.5 },
                "lo": { "type": "noul", "noul": -0.4 }
            }
        }))
        .unwrap();
        assert_eq!(a.noul("hi"), Some(1.0));
        assert_eq!(a.noul("lo"), Some(0.0));
    }

    #[test]
    fn missing_confidence_becomes_a_value_that_fails_the_gate() {
        let a = Answers::from_wire(&json!({
            "answers": { "endpoint_kind": { "type": "choice", "choice": "ad_or_monetization" } }
        }))
        .unwrap();
        let (_, conf) = a.choice("endpoint_kind", crate::question::KIND_LABELS).unwrap();
        assert_eq!(conf, 0.0, "缺 confidence 必须是 0（必然过不了闸门），不是 1");
    }

    #[test]
    fn from_wire_refuses_shapes_the_server_should_not_send() {
        assert!(Answers::from_wire(&json!({})).is_none());
        assert!(Answers::from_wire(&json!({ "answers": [] })).is_none());
        assert!(Answers::from_wire(&json!({ "answers": "x" })).is_none());
    }
}
