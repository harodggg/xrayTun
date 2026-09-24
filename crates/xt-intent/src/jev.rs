//! Jev 网关：把 [`IntentRequest`] 走一次 HTTPS POST，带重试与退避。
//!
//! # 这里只有**协议**，没有策略
//!
//! 判不判、拦不拦、花不花钱都不在本模块：那是 [`crate::verdict`] 与
//! [`crate::engine`] 的事。本模块只做四件确定的事：
//!
//! 1. 组请求（头、正文、URL）；
//! 2. 发出去（通过 [`Transport`]，所以测试里一次网络都不发）；
//! 3. 按**已被验证的**策略重试（最多 3 次尝试、退避 500ms→5000ms、25% 抖动、
//!    尊重 `Retry-After` 但上限 60s、可重试状态码 `{408,429,500,502,503,504,529}`）；
//! 4. 把状态码与响应体映射成 [`GatewayError`]。
//!
//! # 与 `jev-x-filter` 的差异（有意为之）
//!
//! * **不做 HTTP-date 形式的 `Retry-After`**：只认整数秒。日期形式落回指数退避，
//!   并记一条 `tracing::debug`。少写一个日期解析器，换来的是"不会因为日期解析
//!   出错而把退避算成天文数字"。
//! * **`User-Agent` 由我们自己设**：浏览器那边这个头是被 Chrome 丢掉的，
//!   桌面端没有这个限制，报自己的名字更利于上游排障。
//! * **密钥在构造时校验**：含 CR/LF 的密钥会被 `build_request` 静默丢掉，
//!   那种失败表现为"怎么都 401"，所以在构造时就直接拒绝。

use std::sync::Arc;
use std::time::Duration;

use tracing::debug;

use crate::audit::Usage;
use crate::gateway::{Gateway, GatewayError, GatewayResponse};
use crate::question::IntentRequest;
use crate::transport::{join_url, HttpRequest, HttpResponse, Transport, TransportError};

/// Jev 网关的路径（上游客户端固定这个值）。
pub const SYSTEMONE_PATH: &str = "/v1/systemone";

/// 启动时**只**打印一次的默认超时（每次尝试都吃这个预算）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(12);

/// 与上游一致的可重试状态码集合。
pub const RETRYABLE_STATUS: &[u16] = &[408, 429, 500, 502, 503, 504, 529];

/// 退避上限与初始值（上游客户端的默认值，实测过，不自己重编一套）。
const BACKOFF_INITIAL_MS: u64 = 500;
const BACKOFF_MAX_MS: u64 = 5_000;
/// `Retry-After` 的上限：超过这个值就不等了（宁可少问几次，也不让一个候选占住队列）。
const MAX_RETRY_AFTER_MS: u64 = 60_000;

/// 网关配置。
#[derive(Debug, Clone, PartialEq)]
pub struct JevConfig {
    /// base URL（**必须** `https://`，由 [`crate::transport::parse_https_url`] 兜底）。
    pub base_url: String,
    pub model: String,
    /// API Key。`None` 表示无密钥（只有 Zen 档允许）。
    pub api_key: Option<String>,
    pub timeout: Duration,
    /// 重试次数（不含首次）。2 ⇒ 最多 3 次尝试。
    pub max_retries: u32,
}

impl JevConfig {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            timeout: DEFAULT_TIMEOUT,
            max_retries: 2,
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// 校验（构造时调用）。返回**全部**问题。
    pub fn validate(&self) -> Result<(), String> {
        if !self.base_url.starts_with("https://") {
            return Err(format!("网关地址必须是 https://，现在是 {}", self.base_url));
        }
        if self.model.trim().is_empty() {
            return Err("模型 id 不能为空".into());
        }
        if let Some(k) = &self.api_key {
            if k.contains('\r') || k.contains('\n') {
                return Err("API Key 里不能有换行".into());
            }
            if k.trim().is_empty() {
                return Err("API Key 是空的（要么别配，要么配一个真的）".into());
            }
        }
        if self.timeout.is_zero() {
            return Err("超时不能是 0".into());
        }
        Ok(())
    }

    pub fn endpoint(&self) -> String {
        join_url(&self.base_url, SYSTEMONE_PATH)
    }
}

/// 记录退避时长（测试注入；生产是 `thread::sleep`）。
pub type Sleeper = Arc<dyn Fn(Duration) + Send + Sync>;
/// 抖动比例，返回 `[0,1)`。
pub type Jitter = Arc<dyn Fn() -> f64 + Send + Sync>;

/// 走 [`Transport`] 的 Jev 客户端。
pub struct JevGateway<T: Transport> {
    config: JevConfig,
    transport: T,
    sleeper: Sleeper,
    jitter: Jitter,
}

impl<T: Transport> JevGateway<T> {
    pub fn new(config: JevConfig, transport: T) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            transport,
            sleeper: Arc::new(|d: Duration| std::thread::sleep(d)),
            // 与上游的 0.25 抖动一致：不引入 rand 依赖，用时间低位当噪声源。
            jitter: Arc::new(|| {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                (nanos % 1000) as f64 / 1000.0
            }),
        })
    }

    /// 注入"睡眠"与"抖动"（测试用：把退避序列变成可断言的数据）。
    pub fn with_timing(mut self, sleeper: Sleeper, jitter: Jitter) -> Self {
        self.sleeper = sleeper;
        self.jitter = jitter;
        self
    }

    pub fn config(&self) -> &JevConfig {
        &self.config
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// 组一次请求（不含重试）。公开出来是为了能**直接断言请求字节**。
    pub fn build_http_request(&self, request: &IntentRequest) -> Result<HttpRequest, GatewayError> {
        let body = serde_json::to_vec(&request.to_wire())
            .map_err(|e| GatewayError::Schema(format!("请求体序列化失败：{e}")))?;
        let mut headers = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
            ("user-agent".to_string(), format!("xraytun/{}", env!("CARGO_PKG_VERSION"))),
        ];
        if let Some(key) = &self.config.api_key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }
        Ok(HttpRequest {
            url: self.config.endpoint(),
            headers,
            body,
            timeout: self.config.timeout,
        })
    }

    /// 一次尝试 → 错误分类（不改状态、不睡眠）。
    fn attempt(&self, request: &IntentRequest) -> Result<GatewayResponse, Attempt> {
        let http = self.build_http_request(request).map_err(Attempt::Fatal)?;
        let response = match self.transport.post(&http) {
            Ok(r) => r,
            Err(e) => {
                let transient = !matches!(e, TransportError::BadUrl(_) | TransportError::Malformed(_));
                return Err(Attempt::Transport { error: e, transient });
            }
        };
        if (200..300).contains(&response.status) {
            return GatewayResponse::from_wire(
                &serde_json::from_str::<serde_json::Value>(&response.body).map_err(|e| {
                    Attempt::Fatal(GatewayError::Schema(format!("响应不是 JSON：{e}")))
                })?,
            )
            .map_err(Attempt::Fatal);
        }
        let message = extract_error_message(&response.body);
        let code = response.status;
        Err(Attempt::Status {
            code,
            message,
            retry_after: retry_after(&response),
            transient: RETRYABLE_STATUS.contains(&code),
        })
    }

    /// 退避：`min(500 * 2^attempt, 5000)`，减去最多 25% 的抖动。
    fn backoff(&self, attempt: u32, retry_after_ms: Option<u64>) -> Duration {
        if let Some(ms) = retry_after_ms {
            if ms <= MAX_RETRY_AFTER_MS {
                return Duration::from_millis(ms);
            }
            debug!(ms, "Retry-After 超过上限，改用指数退避");
        }
        let exp = BACKOFF_INITIAL_MS.saturating_mul(1u64 << attempt.min(16));
        let capped = exp.min(BACKOFF_MAX_MS);
        let jitter_fraction = (self.jitter)().clamp(0.0, 0.999);
        let jitter = (capped as f64 * 0.25 * jitter_fraction) as u64;
        Duration::from_millis(capped.saturating_sub(jitter))
    }
}

/// 一次尝试的失败，携带"要不要重试"。
enum Attempt {
    /// 重试也没用（URL 不对、响应不是 JSON、请求体都组不出来）。
    Fatal(GatewayError),
    /// 传输层失败；`transient` 决定要不要重试。
    Transport { error: TransportError, transient: bool },
    /// 非 2xx。
    Status { code: u16, message: String, retry_after: Option<u64>, transient: bool },
}

impl Attempt {
    fn retryable(&self) -> bool {
        match self {
            Self::Fatal(_) => false,
            Self::Transport { transient, .. } => *transient,
            Self::Status { transient, .. } => *transient,
        }
    }

    fn into_error(self) -> GatewayError {
        match self {
            Self::Fatal(e) => e,
            Self::Transport { error, .. } => match error {
                TransportError::Timeout { .. } => GatewayError::Timeout,
                other => GatewayError::Transport(other.to_string()),
            },
            Self::Status { code, message, .. } => GatewayError::Status { code, message },
        }
    }

    fn backoff_hint(&self) -> Option<u64> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl<T: Transport> Gateway for JevGateway<T> {
    fn ask(&self, request: &IntentRequest) -> Result<GatewayResponse, GatewayError> {
        let mut last: Option<Attempt> = None;
        for attempt in 0..=self.config.max_retries {
            match self.attempt(request) {
                Ok(response) => return Ok(response),
                Err(failure) => {
                    if !failure.retryable() || attempt == self.config.max_retries {
                        return Err(failure.into_error());
                    }
                    let delay = self.backoff(attempt, failure.backoff_hint());
                    if matches!(&failure, Attempt::Status { code, .. } if *code == 429) {
                        debug!(?delay, "Jev 网关限流，退避后重试");
                    }
                    (self.sleeper)(delay);
                    last = Some(failure);
                }
            }
        }
        Err(last.map(Attempt::into_error).unwrap_or(GatewayError::Transport("重试循环没有结果".into())))
    }

    fn describe(&self) -> String {
        let key = if self.config.api_key.is_some() { "有密钥" } else { "无密钥" };
        format!("{} · {} · {key}", self.transport.describe(), self.config.model)
    }
}

/// 从错误响应体里尽力取出人话。
///
/// 顺序与上游客户端一致：`body.error`（字符串）→ `body.error.message` → `body.message`。
/// 实测 Zen 免密钥档返回的是 `{"type":"error","error":{"type":"FreeUsageLimitError","message":"…"}}`，
/// 命中的是第二条。
pub fn extract_error_message(body: &str) -> String {
    const LIMIT: usize = 500;
    let text = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let from_error = v.get("error").and_then(|e| {
                e.as_str().map(str::to_string).or_else(|| {
                    e.get("message").and_then(|m| m.as_str()).map(str::to_string)
                })
            });
            from_error.or_else(|| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                "(空响应体)".to_string()
            } else {
                body.trim().to_string()
            }
        });
    text.chars().take(LIMIT).collect()
}

/// `Retry-After`：只认整数秒（见模块文档）。
fn retry_after(response: &HttpResponse) -> Option<u64> {
    let raw = response.header("retry-after")?;
    raw.trim().parse::<u64>().ok().map(|secs| secs.saturating_mul(1000))
}

/// 把 `usage` / `cost` 从响应里带出来（审计用）。
pub fn usage_of(response: &GatewayResponse) -> Option<Usage> {
    response.usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{domain_request, FlowContext};
    use crate::transport::{HttpResponse, TransportError};
    use std::sync::Mutex;

    /// 脚本化传输：按顺序返回预设结果，并记录收到的请求。
    struct MockTransport {
        script: Mutex<Vec<Result<HttpResponse, TransportError>>>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl MockTransport {
        fn new(script: Vec<Result<HttpResponse, TransportError>>) -> Self {
            Self { script: Mutex::new(script), seen: Mutex::new(Vec::new()) }
        }
        fn requests(&self) -> Vec<HttpRequest> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Transport for MockTransport {
        fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.seen.lock().unwrap().push(request.clone());
            self.script.lock().unwrap().remove(0)
        }
        fn describe(&self) -> String {
            "mock".into()
        }
    }

    fn ok_body(ads: f32) -> String {
        format!(
            r#"{{"model":"jev-latest","answers":{{"endpoint_kind":{{"type":"choice","choice":"ad_or_monetization","confidence":0.9}},"ads_intent":{{"type":"noul","noul":{ads}}},"risk_of_breakage":{{"type":"noul","noul":0.05}}}},"usage":{{"input_tokens":90,"output_tokens":12}},"cost":"0"}}"#
        )
    }

    fn resp(status: u16, body: &str, headers: &[(&str, &str)]) -> HttpResponse {
        HttpResponse {
            status,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.to_string(),
        }
    }

    fn request() -> IntentRequest {
        domain_request(
            &FlowContext { host: "adsrv-7f3.example".into(), port: Some(443), network: "tcp".into(), inbound: "tun".into(), ..Default::default() },
            "jev-latest",
        )
    }

    struct Timing {
        slept: Arc<Mutex<Vec<u64>>>,
    }

    fn gateway(script: Vec<Result<HttpResponse, TransportError>>) -> (JevGateway<MockTransport>, Timing) {
        let slept = Arc::new(Mutex::new(Vec::new()));
        let rec = slept.clone();
        let g = JevGateway::new(
            JevConfig::new("https://opencode.ai/zen", "jev-1.13-free").with_key("test-key"),
            MockTransport::new(script),
        )
        .unwrap()
        .with_timing(
            Arc::new(move |d: Duration| rec.lock().unwrap().push(d.as_millis() as u64)),
            Arc::new(|| 0.0), // 抖动关掉 ⇒ 退避时长可断言
        );
        (g, Timing { slept })
    }

    #[test]
    fn a_successful_call_sends_exactly_one_request_with_the_right_headers() {
        let (g, _) = gateway(vec![Ok(resp(200, &ok_body(0.97), &[]))]);
        let r = g.ask(&request()).unwrap();
        assert_eq!(r.answers.noul(crate::question::Q_ADS_INTENT), Some(0.97));
        assert_eq!(r.usage.unwrap().input_tokens, 90);
        assert_eq!(r.cost.as_deref(), Some("0"));

        let reqs = g.transport().requests();
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        assert_eq!(req.url, "https://opencode.ai/zen/v1/systemone");
        let auth = req.headers.iter().find(|(k, _)| k == "authorization").unwrap();
        assert_eq!(auth.1, "Bearer test-key");
        assert!(req.headers.iter().any(|(k, v)| k == "accept" && v == "application/json"));
        assert!(req.headers.iter().any(|(k, _)| k == "content-type"));
        assert!(req.headers.iter().any(|(k, v)| k == "user-agent" && v.starts_with("xraytun/")));
        // 正文必须是上游认的三个键。
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        let keys: Vec<&str> = body.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["model", "questions", "state"]);
    }

    #[test]
    fn a_keyless_gateway_omits_the_authorization_header() {
        let g = JevGateway::new(
            JevConfig::new("https://opencode.ai/zen", "jev-1.13-free"),
            MockTransport::new(vec![Ok(resp(200, &ok_body(0.5), &[]))]),
        )
        .unwrap();
        g.ask(&request()).unwrap();
        let reqs = g.transport().requests();
        assert!(!reqs[0].headers.iter().any(|(k, _)| k == "authorization"), "无密钥时不许带空的 Authorization");
        assert!(g.describe().contains("无密钥"));
    }

    #[test]
    fn a_transient_status_is_retried_and_then_succeeds() {
        let (g, t) = gateway(vec![
            Ok(resp(503, "upstream busy", &[])),
            Ok(resp(200, &ok_body(0.9), &[])),
        ]);
        assert!(g.ask(&request()).is_ok());
        assert_eq!(g.transport().requests().len(), 2);
        assert_eq!(*t.slept.lock().unwrap(), vec![500], "抖动注入 0 ⇒ 退避就是名义值 500ms");
    }

    #[test]
    fn a_client_error_is_not_retried() {
        let (g, t) = gateway(vec![Ok(resp(401, r#"{"error":"bad key"}"#, &[]))]);
        let err = g.ask(&request()).unwrap_err();
        assert_eq!(err, GatewayError::Status { code: 401, message: "bad key".into() });
        assert_eq!(g.transport().requests().len(), 1, "401 重试没有意义");
        assert!(t.slept.lock().unwrap().is_empty());
        assert!(!err.is_transient());
    }

    #[test]
    fn rate_limit_is_retried_and_its_body_message_survives() {
        // 这是 Zen 档实测返回的形状。
        let body = r#"{"type":"error","error":{"type":"FreeUsageLimitError","message":"Rate limit exceeded. Please try again later."},"metadata":{}}"#;
        let (g, _) = gateway(vec![
            Ok(resp(429, body, &[])),
            Ok(resp(429, body, &[])),
            Ok(resp(429, body, &[])),
        ]);
        let err = g.ask(&request()).unwrap_err();
        assert_eq!(g.transport().requests().len(), 3, "429 必须重试到上限");
        match err {
            GatewayError::Status { code, message } => {
                assert_eq!(code, 429);
                assert!(message.contains("Rate limit exceeded"), "{message}");
            }
            other => panic!("应当是 429：{other:?}"),
        }
    }

    #[test]
    fn retry_after_seconds_wins_over_exponential_backoff() {
        let (g, t) = gateway(vec![
            Ok(resp(429, "slow down", &[("Retry-After", "2")])),
            Ok(resp(200, &ok_body(0.9), &[])),
        ]);
        assert!(g.ask(&request()).is_ok());
        assert_eq!(*t.slept.lock().unwrap(), vec![2000]);
    }

    #[test]
    fn an_absurd_retry_after_falls_back_to_backoff_instead_of_waiting_forever() {
        let (g, t) = gateway(vec![
            Ok(resp(429, "slow down", &[("Retry-After", "86400")])),
            Ok(resp(200, &ok_body(0.9), &[])),
        ]);
        assert!(g.ask(&request()).is_ok());
        let slept = t.slept.lock().unwrap().clone();
        assert_eq!(slept, vec![500], "上限 60s 之外一律落回指数退避");
    }

    #[test]
    fn an_http_date_retry_after_falls_back_to_backoff() {
        let (g, t) = gateway(vec![
            Ok(resp(429, "slow", &[("Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT")])),
            Ok(resp(200, &ok_body(0.9), &[])),
        ]);
        assert!(g.ask(&request()).is_ok());
        assert_eq!(*t.slept.lock().unwrap(), vec![500], "日期形式不解析，直接落回退避");
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let g = JevGateway::new(
            JevConfig::new("https://gw.example", "m"),
            MockTransport::new(vec![]),
        )
        .unwrap()
        .with_timing(Arc::new(|_| {}), Arc::new(|| 0.0));
        assert_eq!(g.backoff(0, None).as_millis(), 500);
        assert_eq!(g.backoff(1, None).as_millis(), 1000);
        assert_eq!(g.backoff(2, None).as_millis(), 2000);
        assert_eq!(g.backoff(3, None).as_millis(), 4000);
        assert_eq!(g.backoff(4, None).as_millis(), 5000, "封顶 5s");
        assert_eq!(g.backoff(20, None).as_millis(), 5000);
    }

    #[test]
    fn jitter_only_shortens_and_never_produces_zero() {
        let g = JevGateway::new(JevConfig::new("https://gw.example", "m"), MockTransport::new(vec![]))
            .unwrap()
            .with_timing(Arc::new(|_| {}), Arc::new(|| 0.999));
        // 500 * 0.25 * 0.999 = 124.875 ⇒ 取整 124 ⇒ 500 - 124 = 376（不低于 75% 的名义值）。
        assert_eq!(g.backoff(0, None).as_millis(), 376);
    }

    #[test]
    fn a_timeout_is_retried_but_a_bad_url_is_not() {
        // 超时可重试。
        let mut script: Vec<Result<HttpResponse, TransportError>> = vec![
            Err(TransportError::Timeout { phase: "read", budget_ms: 12_000 }),
            Ok(resp(200, &ok_body(0.9), &[])),
        ];
        let (g, _) = gateway(std::mem::take(&mut script));
        assert!(g.ask(&request()).is_ok());
        assert_eq!(g.transport().requests().len(), 2);

        // URL 不合法不可重试。
        let g2 = JevGateway::new(JevConfig::new("https://gw.example", "m"), MockTransport::new(vec![]))
            .unwrap();
        // 直接把一个坏 URL 塞进去（模拟调用方拿到的坏配置）。
        let mut bad = request();
        bad.model = "m".into();
        let cfg = JevConfig { base_url: "https://".into(), ..g2.config().clone() };
        assert!(cfg.validate().is_ok(), "https:// 本身能过 config 校验，坏在 host 空");
        let g3 = JevGateway::new(cfg, MockTransport::new(vec![Err(TransportError::BadUrl("没有主机名".into()))])).unwrap();
        let err = g3.ask(&bad).unwrap_err();
        assert!(matches!(err, GatewayError::Transport(_)), "{err:?}");
        assert_eq!(g3.transport().requests().len(), 1, "坏 URL 重试也是白搭");
    }

    #[test]
    fn a_non_json_2xx_is_a_schema_error_and_is_not_retried() {
        let (g, _) = gateway(vec![Ok(resp(200, "<html>oops</html>", &[]))]);
        let err = g.ask(&request()).unwrap_err();
        assert!(matches!(err, GatewayError::Schema(_)), "{err:?}");
        assert_eq!(g.transport().requests().len(), 1);
    }

    #[test]
    fn a_2xx_without_answers_is_a_schema_error() {
        let (g, _) = gateway(vec![Ok(resp(200, r#"{"model":"m","foo":1}"#, &[]))]);
        assert!(matches!(g.ask(&request()).unwrap_err(), GatewayError::Schema(_)));
    }

    #[test]
    fn config_validation_rejects_what_would_fail_silently() {
        assert!(JevConfig::new("http://gw.example", "m").validate().is_err(), "明文网关");
        assert!(JevConfig::new("https://gw.example", "  ").validate().is_err(), "空模型");
        assert!(JevConfig::new("https://gw.example", "m").with_key("a\r\nX: 1").validate().is_err(), "密钥里换行");
        assert!(JevConfig::new("https://gw.example", "m").with_key("   ").validate().is_err(), "空密钥");
        let mut c = JevConfig::new("https://gw.example", "m");
        c.timeout = Duration::ZERO;
        assert!(c.validate().is_err());
        assert!(JevConfig::new("https://gw.example", "m").validate().is_ok());
    }

    #[test]
    fn endpoint_keeps_the_base_subpath() {
        assert_eq!(
            JevConfig::new("https://opencode.ai/zen", "m").endpoint(),
            "https://opencode.ai/zen/v1/systemone"
        );
        assert_eq!(
            JevConfig::new("https://api.typesafe.ai", "m").endpoint(),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            JevConfig::new("https://api.typesafe.ai/", "m").endpoint(),
            "https://api.typesafe.ai/v1/systemone"
        );
    }

    #[test]
    fn error_message_extraction_covers_the_three_shapes() {
        assert_eq!(extract_error_message(r#"{"error":"boom"}"#), "boom");
        assert_eq!(
            extract_error_message(r#"{"error":{"type":"X","message":"nested boom"}}"#),
            "nested boom"
        );
        assert_eq!(extract_error_message(r#"{"message":"top level"}"#), "top level");
        // 不是 JSON 时也别编，原样给（截断）。
        assert_eq!(extract_error_message("plain text failure"), "plain text failure");
        assert_eq!(extract_error_message("   "), "(空响应体)");
        // 超长截断到 500 字符。
        assert_eq!(extract_error_message(&"x".repeat(900)).chars().count(), 500);
    }

    #[test]
    fn describe_never_leaks_the_key() {
        let (g, _) = gateway(vec![]);
        let d = g.describe();
        assert!(!d.contains("test-key"), "{d}");
        assert!(d.contains("有密钥"));
        assert!(d.contains("jev-1.13-free"));
    }

    #[test]
    fn only_the_documented_statuses_are_retryable() {
        for code in RETRYABLE_STATUS {
            assert!(crate::gateway::GatewayError::Status { code: *code, message: String::new() }.is_transient(), "{code}");
        }
        for code in [400u16, 401, 403, 404, 409, 422] {
            assert!(!crate::gateway::GatewayError::Status { code, message: String::new() }.is_transient(), "{code}");
        }
    }

    /// **真实网络**：这条只证明"TLS + HTTP + 请求形状真的能用"。
    ///
    /// 默认 `#[ignore]`（CI 不该依赖外网）。手动跑：
    /// ```bash
    /// cargo test -p xt-intent --lib jev -- --ignored --nocapture
    /// ```
    ///
    /// （**不是** `--test live`：这条用例在 lib 的测试模块里，没有名为 `live` 的
    /// 测试目标 —— 照着旧写法跑会得到"0 tests"，看起来像"没跑"，实际是"没找到"。）
    ///
    /// 断言刻意宽松：免密钥档随时可能 429（本机实测就是 429），
    /// 但**绝不允许**出现 TLS / 连接 / 格式层面的失败 —— 那才是我们自己的 bug。
    #[test]
    #[ignore = "需要外网；手动跑，见本测试的文档"]
    fn live_zen_endpoint_speaks_the_protocol() {
        let g = JevGateway::new(
            JevConfig::new("https://opencode.ai/zen", "jev-1.13-free"),
            crate::transport::TlsTransport::new(),
        )
        .unwrap();
        // ⚠️ 请求体里的 `model` 必须与网关配置一致。本机第一次跑这条用例时写死了
        // `jev-latest`，Zen 档直接回 `401 Model jev-latest is not supported` ——
        // 那不是 TLS 的问题，是"两个地方各写了一个模型 id"。
        let req = domain_request(
            &FlowContext { host: "adsrv-7f3.example".into(), port: Some(443), ..Default::default() },
            &g.config().model,
        );
        match g.ask(&req) {
            Ok(r) => {
                println!("在线成功：model={:?} usage={:?}", r.model, r.usage);
                assert!(!r.answers.is_empty());
            }
            Err(GatewayError::Status { code, message }) => {
                println!("在线返回 HTTP {code}：{message}");
                assert!(
                    matches!(code, 401 | 403 | 404 | 429 | 503 | 529),
                    "非预期的状态码：{code} {message}"
                );
            }
            Err(other) => panic!("传输层失败说明我们自己的实现有问题：{other:?}"),
        }
    }
}
