//! 判定接缝：一个已经终结了 TLS 的请求 → 「阻断」还是「放行」。
//!
//! # 为什么是一个 trait
//!
//! 真正的判定会问 Jev（或查 `xt-intent` 的判决缓存/域名名单），而那需要网络与状态。
//! 把判定抽成 [`Decider`]，代理循环就能在没有网络、没有模型的条件下被完整测试；
//! 生产的 decider 由桌面端注入（复用 `xt-intent` 的缓存）。
//!
//! # 两条必须遵守的语义
//!
//! 1. **fail-open**：拿不到判定（没有 Host、decider 内部出错、域名不认识）一律**放行**。
//!    MITM 拦错的代价是"页面坏了"，而漏拦只是"这条广告还在" —— 两者不对称。
//! 2. **阻断要给出可读的原因**：调用方会把它写进审计。没有原因的阻断等于无法申诉。

use std::collections::BTreeSet;

use crate::http1::RequestHead;

/// 判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 阻断这个请求，并给出**可读的原因**（进审计）。
    Block { reason: String },
    /// 放行（原样转发）。
    Pass,
}

impl Decision {
    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block { .. })
    }
}

/// 判定接缝。实现必须是 `Send + Sync`（代理是多连接的）。
pub trait Decider: Send + Sync {
    fn decide(&self, head: &RequestHead) -> Decision;
}

/// 按主机名（含子域）匹配的黑名单 —— 用于测试与"本地裁决"，
/// 生产里由 `xt-intent` 的缓存/判决驱动。
#[derive(Debug, Clone, Default)]
pub struct BlocklistDecider {
    /// 归一化后的主机名（小写）。`a.example` 会同时命中 `x.a.example`。
    hosts: BTreeSet<String>,
}

impl BlocklistDecider {
    pub fn new<I: IntoIterator<Item = String>>(hosts: I) -> Self {
        Self {
            hosts: hosts
                .into_iter()
                .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        }
    }

    /// 是否命中（**子域也算命中**）：域名名单里写 `ads.example` 时，
    /// `cdn.ads.example` 也该被拦 —— 投放端换子域是常见做法。
    pub fn is_blocked(&self, host: &str) -> bool {
        let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if h.is_empty() {
            return false;
        }
        self.hosts.iter().any(|blocked| h == *blocked || h.ends_with(&format!(".{blocked}")))
    }
}

impl Decider for BlocklistDecider {
    fn decide(&self, head: &RequestHead) -> Decision {
        // **没有 Host 一律放行**：判定不出来就不要动它（fail-open）。
        let Some(host) = head.host() else {
            return Decision::Pass;
        };
        if self.is_blocked(host) {
            return Decision::Block { reason: format!("命中黑名单：{host}") };
        }
        Decision::Pass
    }
}

/// 阻断时需要回给客户端的那一段**完整响应字节**。
///
/// 用 `204 No Content` + `Content-Length: 0`：
///
/// * 调用方（浏览器/SDK）拿到的是"成功但没内容"，绝大多数场景下**不会**因此报错，
///   而返回 403/500 反而会让页面进入错误分支、比广告本身更显眼；
/// * 绝不返回"看起来像成功响应体"的东西 —— 我们不知道调用方期望 JSON 还是图片。
pub fn blocked_response(reason: &str) -> Vec<u8> {
    // 原因只进**响应头**（`X-XrayTun-Blocked`），不进 body：body 要让调用方猜不出内容类型。
    //
    // 原因要**按控制字符净化**：只滤 `\r\n` 不够 —— `\x0b`（VT）、`\x0c` 这些在某些
    // 解析器里同样能起分隔作用。所以直接丢掉**所有 ASCII 控制字符**。
    let safe: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    format!(
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nX-XrayTun-Blocked: {safe}\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http1::RequestHead;

    fn head(target: &str, host: Option<&str>) -> RequestHead {
        let mut h = RequestHead::parse(
            format!("GET {target} HTTP/1.1\r\n{}\r\n", host.map(|x| format!("Host: {x}\r\n")).unwrap_or_default())
                .as_bytes(),
        )
        .unwrap();
        h.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("host") || host.is_some());
        h
    }

    #[test]
    fn exact_host_and_subdomains_both_match() {
        let d = BlocklistDecider::new(vec!["ads.example".into()]);
        assert!(d.is_blocked("ads.example"));
        assert!(d.is_blocked("ADS.EXAMPLE."));
        assert!(d.is_blocked("cdn.ads.example"));
        assert!(!d.is_blocked("notads.example"), "后缀匹配必须按标签边界");
        assert!(!d.is_blocked("example"));
        assert!(!d.is_blocked(""));
    }

    #[test]
    fn a_blocked_host_produces_a_decision_with_a_readable_reason() {
        let d = BlocklistDecider::new(vec!["ads.example".into()]);
        let dv = d.decide(&head("/x", Some("ads.example")));
        match dv {
            Decision::Block { reason } => assert!(reason.contains("ads.example"), "{reason}"),
            other => panic!("应当阻断：{other:?}"),
        }
        assert!(d.decide(&head("/x", Some("news.example"))).eq(&Decision::Pass));
    }

    /// **fail-open**：没有 Host、或 Host 空 ⇒ 放行。判定不出来就别动它。
    #[test]
    fn a_request_with_no_host_is_passed() {
        let d = BlocklistDecider::new(vec!["ads.example".into()]);
        assert_eq!(d.decide(&head("/x", None)), Decision::Pass);
        assert_eq!(d.decide(&head("/x", Some(""))), Decision::Pass);
    }

    /// 子域名单为空 ⇒ 什么都没配 ⇒ 一律放行（"开了但没配"等于没开）。
    #[test]
    fn an_empty_blocklist_passes_everything() {
        let d = BlocklistDecider::default();
        assert_eq!(d.decide(&head("/x", Some("ads.example"))), Decision::Pass);
    }

    #[test]
    fn the_blocked_response_is_204_with_zero_length() {
        let r = blocked_response("命中黑名单：ads.example");
        let text = String::from_utf8(r).unwrap();
        assert!(text.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.contains("X-XrayTun-Blocked: 命中黑名单：ads.example"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    /// 原因里的控制字符必须被净化 —— 否则就是**头注入**。
    ///
    /// 判据不是"文本里有没有 X-Injected"（净化之后它只是头值里的一段普通文字），
    /// 而是**有没有多出一行头**。
    #[test]
    fn a_reason_with_newlines_cannot_inject_headers() {
        for evil in ["evil\r\nX-Injected: 1", "evil\nX-Injected: 1", "evil\x0bX: 1"] {
            let r = blocked_response(evil);
            let text = String::from_utf8(r).unwrap();
            let lines: Vec<&str> = text.split("\r\n").collect();
            // 起始行 + 3 个头 + 空行 + 结束空串 = 6 段
            assert_eq!(lines.len(), 6, "{evil:?} 多出了头行：\n{text}");
            assert!(
                !lines.iter().any(|l| l.starts_with("X-Injected") || l.starts_with("X: ")),
                "{evil:?} 造成了头注入：\n{text}"
            );
            assert!(lines.iter().any(|l| l.starts_with("X-XrayTun-Blocked: ")));
        }
    }
}
