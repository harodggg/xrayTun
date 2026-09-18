//! `xt-core` —— 与平台无关的代理业务核心。
//!
//! 职责边界（见 `docs/01-architecture.md`）：
//!
//! * **不碰系统网络配置**。建 TUN、改路由、改 DNS 全部由 `xt-tun` / `xt-helper` 负责。
//! * **不碰 UI**。这里只暴露纯函数与少量长生命周期对象，方便单测与复用。
//! * 订阅解析、节点规范化、Xray 配置生成、核心进程生命周期、延迟探针都在这里。
//!
//! 一个刻意的取舍：**配置变更走“重启核心”而不是 gRPC 热更新**。
//! 对桌面客户端来说，切换节点/规则的频率是分钟级，重启 Xray 大约 100~300ms，
//! 用户体验上等价；而省掉 gRPC 代码生成（需要 `protoc`）显著降低构建复杂度。
//! 需要 gRPC 时（Per-outbound 统计、Observatory 主动探测）的接入点见
//! `docs/03-xray-integration.md`。

pub mod error;
pub mod geo_lookup;
pub mod model;
pub mod net;
pub mod routing;
pub mod dns_probe;
pub mod store;
pub mod update;
pub mod subscription;
pub mod util;
pub mod xray;

pub use error::{Error, Result};
pub use model::{
    AppSettings, DatapathMode, DnsHandling, DnsSettings, FakeDnsSettings, FdOwnership, MuxSettings,
    Node, NodeId, NodeSource, Protocol, ProxyMode, RealitySettings, RoutingPreset, Subscription,
    TlsSettings, Transport, TunSettings, VmessSecurity,
};
pub use routing::{MatchCondition, Network, PortMatcher, RoutingRule, RuleAction};
pub use subscription::{parse_any, ParseOutcome, SubscriptionFormat};

/// 应用标识，用于派生数据目录、launchd 标签、keychain 服务名等。
pub const APP_IDENTIFIER: &str = "com.xraytun.desktop";
