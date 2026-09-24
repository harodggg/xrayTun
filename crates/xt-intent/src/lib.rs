//! 意图过滤判定引擎。
//!
//! # 它在整个功能里的位置
//!
//! Xray 的 TUN 数据面看得见**端点**（域名 / IP / 端口 / 协议），看不见 TLS 里的
//! URL 与正文。所以广告拦截在这里只有一件事可做：**判定端点是不是投放/追踪基础设施，
//! 然后把它路由到 `blackhole`**。判定由 Jev（TypeSafe System One）的类型化问题给出。
//!
//! 完整设计见 `docs/design/INTENT-FILTER.md`；本 crate 只负责**判定**，
//! 不碰系统网络配置、不自己起核心、不做 HTTP 之外的任何 IO。
//!
//! # 分层（每一层加一次钱、加一次风险）
//!
//! ```text
//! 连接记录 ──▶ Observer   去重、过滤掉不该问的（IP 字面量 / 内网 / 单标签名）
//!          ──▶ Cache      命中即 0 成本；指纹里含模型与网关，换模型即整体失效
//!          ──▶ Budget     滑动窗口 + 每日上限；超额 fail-open（放行）
//!          ──▶ Gateway    一次 HTTPS 调用（`POST /v1/systemone`）
//!          ──▶ Gate       三条件 AND 才定罪，任一条件不足都放行
//!          ──▶ Audit      append-only JSONL：每一次判决都能被复查、被申诉
//!          ──▶ rules     判决 → `Vec<RoutingRule>`（复用 xt-core 的既有 IR）
//! ```
//!
//! # 五条硬约束（写在类型里，不靠口头约定）
//!
//! 1. **fail-open**：网关超时 / 预算耗尽 / 解析失败 / 答案缺字段 —— 一律放行 + 审计。
//!    判定失败绝不能让用户断网。
//! 2. **首访放行**：新域名在判决回来之前必须放行。数据面永不等模型。
//! 3. **只有模型能定罪**：流量形状（[`shape`]）只调阈值、只排优先级，
//!    单独出现时永远不产生 block（有测试钉住）。
//! 4. **缓存 key 含模型与网关**：换模型 / 换网关 / 改问题措辞 ⇒ 旧判决整体作废。
//! 5. **allow 永远优先于 block**：用户纠正过的域名不可能再被拦（同一个域名
//!    同时出现在两带时，物化阶段直接消解，并有测试）。

pub mod answer;
pub mod audit;
pub mod budget;
pub mod cache;
pub mod engine;
pub mod eval;
pub mod gateway;
pub mod jev;
pub mod observer;
pub mod question;
pub mod rules;
pub mod settings;
pub mod shape;
pub mod transport;
pub mod verdict;

pub use answer::Answers;
pub use audit::{AuditRecord, AuditLog};
pub use budget::{Budget, BudgetExhausted};
pub use cache::{CacheEntry, VerdictCache};
pub use engine::{ClassifyReport, EngineStats, IntentEngine};
pub use eval::{CorpusObserver, Dataset, DomainLabels, Gate, Judgement, Label, Metrics};
pub use gateway::{Gateway, GatewayError, GatewayResponse, ScriptedGateway};
pub use jev::{JevConfig, JevGateway};
pub use transport::{HttpRequest, HttpResponse, TlsTransport, Transport, TransportError};
pub use observer::{Candidate, Observer, ObserverStats};
pub use question::{domain_request, FlowContext, IntentRequest, Question};
pub use rules::{materialize, AllowAction, AllowOverride, IntentRules, RuleOptions};
pub use settings::{config_from_settings, readiness_note};
pub use shape::FlowShape;
pub use verdict::{AllowReason, BlockVerdict, Category, DeferReason, Thresholds, Verdict};

/// 判决缓存的格式版本。语义变化（例如判决结构改了）必须 +1 ——
/// 旧缓存会在加载时被整体丢弃，而不是被误读成新语义。
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// 把任意浮点夹到 `[0,1]`；非有限值一律 0（**不猜**）。
///
/// `is_finite` 的守门是**必要**的，不是多余的：`f32::clamp` 对 NaN 返回 NaN，
/// 而我们的语义是"读不出来就是 0"。
pub(crate) fn clamp01(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(0.0, 1.0)
}
