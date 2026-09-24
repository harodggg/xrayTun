//! MITM 内容级判定通道。
//!
//! # 它在整条链路里的位置
//!
//! ```text
//! 客户端 ──TUN──▶ Xray ──(steer: domain∈opt-in 且 inboundTag∈{tun,socks,http})──▶ mitm-out
//!                                                                                  │ freedom.redirect
//!                                                                                  ▼
//!                                              【本 crate】监听 127.0.0.1:<listen_port>
//!                                                 终结 TLS（本地 CA）→ 判定 → 阻断或放行
//!                                                                                  │ 经 mitm-upstream
//!                                                                                  ▼
//!                                                         Xray（inboundTag=mitm-upstream，不命中 steer）→ 目标
//! ```
//!
//! # 四块职责，**前三块不碰网络**（所以能完整单测）
//!
//! | 模块 | 职责 | 只依赖 |
//! |---|---|---|
//! | [`http1`] | 解析/序列化 HTTP/1.1 头 | 无 |
//! | [`decide`] | 请求 → 阻断/放行（可注入的接缝） | 无 |
//! | [`rewrite`] | 响应体裁剪 + **`Content-Length` 一致性** | `serde_json` |
//! | [`decide`] | 请求 → 阻断/放行 的接缝（可注入） | 无 |
//! | [`tls`] / [`proxy`] | TLS 终结与转发（**唯一碰网络的部分**） | `rustls` / `rcgen` / 标准库线程 |
//!
//! # 三条写进类型的硬约束
//!
//! 1. **只广告 `http/1.1`**：ALPN 里不出现 `h2` ⇒ 不需要 h2 终止（帧/流/流控是 MITM 里最重的一块）。
//!    代价是 opt-in 域名失去 HTTP/2 多路复用 —— 这条要写进界面文案。
//! 2. **改body 必须同步改长度**：剪了内容却没改 `Content-Length` 会把客户端搞崩。
//!    [`rewrite`] 里没有"只改 body"的入口，只有"改 body 并重算头"这一个。
//! 3. **WebSocket 盲转发**：`Upgrade: websocket` 一律不解析、不裁剪。
//!
//! # 本模块**不做**的事
//!
//! 不做 HTML DOM 重写、不注入脚本、不把正文送到网关缓存。
//! 只做"删掉一个 JSON 数组元素"这种窄口径动作 —— 它已经能覆盖同域广告
//! （例如时间线接口里的推广条目），而 DOM 重写等于在里面再写一个 AdGuard。

pub mod decide;
pub mod http1;
pub mod proxy;
pub mod rewrite;
pub mod tls;

pub use decide::{blocked_response, BlocklistDecider, Decision, Decider};
pub use http1::RequestHead;
pub use proxy::{serve, ProxyConfig, ProxyHandle};
pub use rewrite::{apply_body_change, length_matches, strip_json_array_entries, RewriteError};
pub use tls::{CertResolver, LocalCa, TlsError, ALPN_HTTP1};

/// MITM 的状态码/原因短语用的常量（客户端看到的那一版响应）。
pub const BLOCKED_STATUS_LINE: &str = "HTTP/1.1 204 No Content";
