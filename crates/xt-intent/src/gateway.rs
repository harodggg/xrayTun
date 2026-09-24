//! Jev 网关：一次 HTTPS 调用，只问一个域名。
//!
//! # 为什么是 trait 而不是直接写网络代码
//!
//! 判定逻辑的正确性（闸门、缓存、预算、规则物化）与传输无关，而**测试里绝不能发网络请求**。
//! [`Gateway`] 是一道接缝：生产实现是一个手写的 HTTP/1.1 over rustls 客户端
//! （见设计文档 §5.1 的依赖取舍），测试与离线评测用 [`ScriptedGateway`]。
//!
//! # 只读答案，不解读
//!
//! 网关在这里只负责"把答案取回来并解析成 [`Answers`]"。判不判、拦不拦，
//! 是 [`crate::verdict::decide`] 的事。这条分工让「模型准不准」与「策略对不对」
//! 可以分开测量。

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;

use crate::answer::Answers;
use crate::audit::Usage;
use crate::question::IntentRequest;

/// 网关失败。**每一种都要能被翻译成 `Deferred`（放行）**，
/// 所以这里不允许出现"未知错误"这种兜不住的东西 —— 传输层一律归 [`GatewayError::Transport`]。
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayError {
    /// 连不上 / TLS 失败 / 读超时之外的 IO。
    Transport(String),
    /// 超时（已经重试过）。
    Timeout,
    /// HTTP 非 2xx。401 / 422 / 429 / 529 分别有名字，便于审计区分。
    Status { code: u16, message: String },
    /// 2xx 但 body 不是我们认识的形状。
    Schema(String),
}

impl GatewayError {
    /// 稳定的短名字（审计与统计）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport",
            Self::Timeout => "timeout",
            Self::Status { .. } => "status",
            Self::Schema(_) => "schema",
        }
    }

    /// 这个错误值不值得立刻重试（交给调用方的调度，不在这里 sleep）。
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) | Self::Timeout => true,
            Self::Status { code, .. } => matches!(code, 408 | 429 | 500 | 502 | 503 | 504 | 529),
            Self::Schema(_) => false,
        }
    }
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(m) => write!(f, "传输失败：{m}"),
            Self::Timeout => write!(f, "超时"),
            Self::Status { code, message } => write!(f, "HTTP {code}：{message}"),
            Self::Schema(m) => write!(f, "响应形状不认识：{m}"),
        }
    }
}

/// 网关的返回。
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayResponse {
    pub model: Option<String>,
    pub answers: Answers,
    pub usage: Option<Usage>,
    pub cost: Option<String>,
}

impl GatewayResponse {
    /// 从完整的响应体解析。**这是唯一一处解析响应的代码** ——
    /// 真客户端与测试夹具共用它，就不会出现"测试通过、线上解析不同"。
    pub fn from_wire(body: &Value) -> Result<Self, GatewayError> {
        let answers = Answers::from_wire(body)
            .ok_or_else(|| GatewayError::Schema("响应里没有 answers 对象".into()))?;
        let model = body.get("model").and_then(Value::as_str).map(str::to_string);
        let usage = body.get("usage").map(|u| Usage {
            input_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        });
        let cost = body.get("cost").and_then(Value::as_str).map(str::to_string);
        Ok(Self { model, answers, usage, cost })
    }
}

/// 一次分类调用。
///
/// `&self` 而不是 `&mut self`：引擎持有它是为了发请求，不是为了改它；
/// 需要可变状态的实现（连接池、计数）自己内部同步。
pub trait Gateway: Send + Sync {
    fn ask(&self, request: &IntentRequest) -> Result<GatewayResponse, GatewayError>;

    /// 人类可读的描述（界面"当前网关"那一行）。**不要**包含密钥。
    fn describe(&self) -> String;
}

/// 脚本化网关：按顺序吐出预设结果，并记录每次请求。
///
/// 用途有三个：单测、离线评测夹具、以及桌面端的"演练模式先跑一遍假数据"。
/// 脚本跑完后默认返回 `Transport("script exhausted")` —— **不是 panic**，
/// 因为"测试里少写了一条夹具"这种失败，应该表现为一次可审计的判决失败，
/// 而不是把整个进程炸掉。
pub struct ScriptedGateway {
    script: Mutex<VecDeque<Result<GatewayResponse, GatewayError>>>,
    calls: Mutex<Vec<IntentRequest>>,
    fallback: Mutex<Option<Result<GatewayResponse, GatewayError>>>,
    label: String,
}

impl ScriptedGateway {
    pub fn new(script: Vec<Result<GatewayResponse, GatewayError>>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            calls: Mutex::new(Vec::new()),
            fallback: Mutex::new(None),
            label: "scripted".into(),
        }
    }

    /// 每次都答同一份（用于"这个域名肯定被拦 / 肯定放行"的场景）。
    pub fn always(response: GatewayResponse) -> Self {
        let g = Self::new(Vec::new());
        *g.fallback.lock().unwrap() = Some(Ok(response));
        g
    }

    /// 每次都失败（用于 fail-open 测试）。
    pub fn always_failing(error: GatewayError) -> Self {
        let g = Self::new(Vec::new());
        *g.fallback.lock().unwrap() = Some(Err(error));
        g
    }

    pub fn push(&self, item: Result<GatewayResponse, GatewayError>) {
        self.script.lock().unwrap().push_back(item);
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// 已经收到的请求（按顺序）。
    pub fn calls(&self) -> Vec<IntentRequest> {
        self.calls.lock().unwrap().clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Gateway for ScriptedGateway {
    fn ask(&self, request: &IntentRequest) -> Result<GatewayResponse, GatewayError> {
        self.calls.lock().unwrap().push(request.clone());
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(item) => item,
            None => self
                .fallback
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err(GatewayError::Transport("script exhausted".into()))),
        }
    }

    fn describe(&self) -> String {
        format!("scripted({})", self.label)
    }
}

/// 测试与评测夹具：按类别/概率造一份"正常"的响应体。
pub fn scripted_response(
    model: &str,
    kind: &str,
    ads_intent: f32,
    risk_of_breakage: f32,
    confidence: f32,
) -> GatewayResponse {
    GatewayResponse::from_wire(&serde_json::json!({
        "model": model,
        "answers": {
            crate::question::Q_ENDPOINT_KIND: {
                "type": "choice", "choice": kind, "confidence": confidence,
                "probabilities": { kind: confidence }
            },
            crate::question::Q_ADS_INTENT: { "type": "noul", "noul": ads_intent },
            crate::question::Q_RISK_OF_BREAKAGE: { "type": "noul", "noul": risk_of_breakage }
        },
        "usage": { "input_tokens": 90, "output_tokens": 12 },
        "cost": "0"
    }))
    .expect("夹具必须是合法响应")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{KIND_AD_OR_MONETIZATION, KIND_HUMAN_SITE, Q_ADS_INTENT};
    use serde_json::json;

    #[test]
    fn from_wire_reads_model_usage_and_cost() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": { "ads_intent": { "type": "noul", "noul": 0.9 } },
            "usage": { "input_tokens": 120, "output_tokens": 40 },
            "cost": "0"
        });
        let r = GatewayResponse::from_wire(&body).unwrap();
        assert_eq!(r.model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(r.usage.unwrap().input_tokens, 120);
        assert_eq!(r.cost.as_deref(), Some("0"));
        assert_eq!(r.answers.noul(Q_ADS_INTENT), Some(0.9));
    }

    #[test]
    fn from_wire_requires_an_answers_object() {
        assert!(matches!(
            GatewayResponse::from_wire(&json!({ "model": "m" })),
            Err(GatewayError::Schema(_))
        ));
        assert!(matches!(
            GatewayResponse::from_wire(&json!({ "answers": 3 })),
            Err(GatewayError::Schema(_))
        ));
    }

    #[test]
    fn usage_is_optional_and_never_invents_numbers() {
        let r = GatewayResponse::from_wire(&json!({ "answers": {} })).unwrap();
        assert!(r.usage.is_none());
        assert!(r.cost.is_none());
        assert!(r.model.is_none());
    }

    #[test]
    fn scripted_gateway_records_every_call_in_order() {
        let g = ScriptedGateway::new(vec![
            Ok(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93)),
            Ok(scripted_response("m", KIND_HUMAN_SITE, 0.10, 0.10, 0.90)),
        ])
        .with_label("unit");
        assert_eq!(g.describe(), "scripted(unit)");

        let req = crate::question::domain_request(&crate::question::FlowContext { host: "a.example".into(), ..Default::default() }, "m");
        assert!(g.ask(&req).is_ok());
        assert!(g.ask(&req).is_ok());
        assert_eq!(g.call_count(), 2);
        assert_eq!(g.calls()[0].state, req.state);

        // 脚本用完之后是**可审计的失败**，不是 panic。
        let err = g.ask(&req).unwrap_err();
        assert_eq!(err.as_str(), "transport");
        assert!(err.is_transient());
    }

    #[test]
    fn always_helpers_do_what_they_say() {
        let ok = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 1.0, 0.0, 1.0));
        let req = crate::question::domain_request(&crate::question::FlowContext::default(), "m");
        assert!(ok.ask(&req).is_ok());
        assert!(ok.ask(&req).is_ok());

        let bad = ScriptedGateway::always_failing(GatewayError::Timeout);
        assert_eq!(bad.ask(&req).unwrap_err(), GatewayError::Timeout);
    }

    #[test]
    fn transient_classification_matches_the_upstream_retry_set() {
        // 与 vendor 客户端一致：{408,429,500,502,503,504,529} + 连接/超时错误可重试。
        for code in [408, 429, 500, 502, 503, 504, 529] {
            assert!(GatewayError::Status { code, message: String::new() }.is_transient(), "{code}");
        }
        for code in [400, 401, 403, 404, 422] {
            assert!(!GatewayError::Status { code, message: String::new() }.is_transient(), "{code}");
        }
        assert!(GatewayError::Transport("x".into()).is_transient());
        assert!(!GatewayError::Schema("x".into()).is_transient());
    }

    #[test]
    fn fixture_is_a_legal_response() {
        let r = scripted_response("m", KIND_AD_OR_MONETIZATION, 0.9, 0.1, 0.8);
        assert_eq!(r.answers.noul(Q_ADS_INTENT), Some(0.9));
        assert!(r.answers.has(crate::question::Q_ENDPOINT_KIND));
    }
}
