//! 订阅解析。
//!
//! 支持四种主流格式，按“越具体越优先”的顺序嗅探：
//!
//! | 格式 | 典型来源 | 嗅探特征 |
//! |---|---|---|
//! | [`SubscriptionFormat::XrayJson`] | 自建面板导出 | 顶层 JSON 且含 `outbounds` |
//! | [`SubscriptionFormat::ClashYaml`] | Clash / Mihomo 订阅 | 含 `proxies:` 键 |
//! | [`SubscriptionFormat::Base64UriList`] | 绝大多数机场 | 整体 base64，解码后每行一个链接 |
//! | [`SubscriptionFormat::UriList`] | 手工粘贴 | 每行一个 `xxx://` 链接 |
//!
//! 解析策略是**尽力而为**：单行失败只记录 `warning`，不影响其它节点。
//! 机场订阅里混入一两条不支持的链接（例如 `hysteria2://`）是常态，
//! 整个订阅因此失败会让用户完全无法使用。

pub mod clash;
pub mod share;
pub mod uri;
pub mod xray_json;

use crate::model::Node;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionFormat {
    UriList,
    Base64UriList,
    ClashYaml,
    XrayJson,
}

impl SubscriptionFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UriList => "uri_list",
            Self::Base64UriList => "base64_uri_list",
            Self::ClashYaml => "clash_yaml",
            Self::XrayJson => "xray_json",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseOutcome {
    pub format: SubscriptionFormat,
    pub nodes: Vec<Node>,
    /// 被跳过的行及其原因，会在 UI 的「订阅详情」里展示。
    pub warnings: Vec<String>,
}

impl ParseOutcome {
    fn new(format: SubscriptionFormat) -> Self {
        Self { format, nodes: Vec::new(), warnings: Vec::new() }
    }

    /// 去重（同一订阅里重复链接很常见）并稳定排序：先按来源分组，再按名称。
    pub fn dedup(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.nodes.retain(|n| seen.insert(n.id.clone()));
    }
}

/// 嗅探格式。
pub fn detect(body: &str) -> Option<SubscriptionFormat> {
    let trimmed = body.trim_start_matches('\u{feff}').trim();

    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if v.get("outbounds").is_some() || v.get("inbounds").is_some() {
                return Some(SubscriptionFormat::XrayJson);
            }
        }
        // 也可能是 base64（有些机场把 JSON 又包了一层 base64），交给下面判断。
    }

    if looks_like_clash_yaml(trimmed) {
        return Some(SubscriptionFormat::ClashYaml);
    }

    if contains_share_link(trimmed) {
        return Some(SubscriptionFormat::UriList);
    }

    if let Ok(decoded) = uri::b64_decode_lenient(trimmed) {
        if let Ok(text) = String::from_utf8(decoded) {
            if contains_share_link(&text) {
                return Some(SubscriptionFormat::Base64UriList);
            }
        }
    }

    None
}

fn contains_share_link(s: &str) -> bool {
    const SCHEMES: &[&str] = &[
        "vmess://",
        "vless://",
        "trojan://",
        "ss://",
        "ssr://",
        "socks://",
        "socks5://",
        "http://",
        "https://",
    ];
    s.lines().any(|line| {
        let line = line.trim();
        SCHEMES.iter().any(|sc| {
            // 注意：**必须用 `str::get` 而不是 `line[..n]`**。
            //
            // `line[..n]` 按字节切片，n 落在多字节 UTF-8 字符中间时会 panic。
            // 触发条件非常容易满足：用户往「手动添加」里粘一句中文
            // （"这只是一句话" 的第 8 个字节就在「是」里面）。
            // `str::get` 在非字符边界上返回 None，正是这里想要的行为。
            line.get(..sc.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(sc))
        })
    })
}

fn looks_like_clash_yaml(s: &str) -> bool {
    if s.starts_with('{') {
        return false;
    }
    // 只需要看是否有一个顶层 `proxies:` 键，不必真的解析 YAML。
    s.lines().any(|line| {
        let t = line.trim_end();
        t == "proxies:" || t.starts_with("proxies:")
    })
}

/// 自动识别并解析订阅正文。
pub fn parse_any(body: &str) -> Result<ParseOutcome> {
    let format = detect(body).ok_or(crate::Error::EmptySubscription)?;
    let mut outcome = match format {
        SubscriptionFormat::ClashYaml => clash::parse(body)?,
        SubscriptionFormat::XrayJson => xray_json::parse(body)?,
        SubscriptionFormat::UriList => parse_uri_list(body),
        SubscriptionFormat::Base64UriList => {
            let decoded = uri::b64_decode_lenient(body.trim())?;
            let text = String::from_utf8(decoded)
                .map_err(|e| crate::Error::Base64(format!("解码结果不是 UTF-8: {e}")))?;
            parse_uri_list(&text)
        }
    };
    for node in &mut outcome.nodes {
        node.refresh_id();
    }
    outcome.dedup();
    Ok(outcome)
}

fn parse_uri_list(text: &str) -> ParseOutcome {
    let mut outcome = ParseOutcome::new(SubscriptionFormat::UriList);
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match uri::parse_share_link(line) {
            Ok(node) => outcome.nodes.push(node),
            Err(e) => outcome.warnings.push(format!("第 {} 行已跳过: {e}", idx + 1)),
        }
    }
    outcome
}

/// 便捷入口：只要节点列表。
pub fn parse_nodes(body: &str) -> Result<Vec<Node>> {
    Ok(parse_any(body)?.nodes)
}

/// 解析**用户手动粘贴**的内容。
///
/// 与 [`parse_any`] 的区别：订阅正文有固定的几种格式可以嗅探，
/// 而手动粘贴的内容是「人从别处复制来的一段东西」，形态更随意。
/// 这里按「越具体越优先」依次尝试：
///
/// 1. **单条分享链接** —— 以 `xxx://` 开头
/// 2. **可识别的订阅正文** —— 走 [`parse_any`]
/// 3. **单条 Clash proxy 映射** —— 没有 `proxies:` 外壳的那种
///
/// 第 3 条是刻意加的：用户从机场文档或 Clash 配置里复制的往往是
/// **一条 proxy 的内容**，而不是一份完整订阅。要求他们自己包一层
/// `proxies:` 外壳是刁难使用者 —— 判断格式是程序的责任。
pub fn parse_manual(input: &str) -> Result<ParseOutcome> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(crate::Error::EmptySubscription);
    }

    // 1) 单条分享链接。
    //
    // `!trimmed.contains('\n')` 这个条件不能少：一段**多行订阅正文**
    // 同样以 `vless://` 开头，只看前缀会把它误判成单条链接，
    // 结果只解析出第一个节点 —— 而且不报任何错，静默丢数据。
    let lower = trimmed.to_ascii_lowercase();
    let is_link = !trimmed.contains('\n')
        && [
            "vmess://", "vless://", "trojan://", "ss://", "ssr://", "socks://", "socks5://",
            "http://", "https://",
        ]
        .iter()
        .any(|scheme| lower.starts_with(scheme));

    let mut outcome = if is_link {
        let mut o = ParseOutcome::new(SubscriptionFormat::UriList);
        o.nodes.push(uri::parse_share_link(trimmed)?);
        o
    } else if detect(trimmed).is_some() {
        // 2) 是完整订阅正文
        parse_any(trimmed)?
    } else {
        // 3) 单条 Clash proxy
        clash::parse_single(trimmed)?
    };

    for node in &mut outcome.nodes {
        node.refresh_id();
    }
    outcome.dedup();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Protocol, Transport};

    #[test]
    fn detects_plain_uri_list() {
        let body = "vless://uuid@a.com:443#A\ntrojan://pw@b.com:443#B\n";
        assert_eq!(detect(body), Some(SubscriptionFormat::UriList));
        let out = parse_any(body).unwrap();
        assert_eq!(out.nodes.len(), 2);
    }

    #[test]
    fn detects_base64_uri_list() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let inner = "vless://uuid@a.com:443#A\nvmess://eyJ2IjoiMiIsInBzIjoiQiJ9\n";
        let body = STANDARD.encode(inner);
        assert_eq!(detect(&body), Some(SubscriptionFormat::Base64UriList));
        let out = parse_any(&body).unwrap();
        // 第二条 vmess 缺少必需字段，应只产生 warning 而不是整体失败
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn bad_lines_do_not_abort_the_whole_subscription() {
        let body = "vless://uuid@a.com:443#A\nhysteria2://nope@x.com:443#X\nvmess://@@@\n";
        let out = parse_any(body).unwrap();
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.warnings.len(), 2);
    }

    #[test]
    fn duplicate_nodes_are_removed() {
        let body = "vless://uuid@a.com:443#A\nvless://uuid@a.com:443#A-again\n";
        let out = parse_any(body).unwrap();
        assert_eq!(out.nodes.len(), 1);
    }

    #[test]
    fn detects_clash_yaml() {
        let body = "proxies:\n  - name: a\n    type: ss\n";
        assert_eq!(detect(body), Some(SubscriptionFormat::ClashYaml));
    }

    #[test]
    fn parse_manual_accepts_all_three_shapes() {
        // 1) 分享链接
        let a = parse_manual("vless://uuid@a.com:443#A").unwrap();
        assert_eq!(a.nodes.len(), 1);

        // 2) 完整订阅正文
        let b = parse_manual("vless://uuid@a.com:443#A\nvless://uuid@b.com:443#B\n").unwrap();
        assert_eq!(b.nodes.len(), 2);

        // 3) 单条 Clash proxy（没有 proxies: 外壳）
        let c = parse_manual("{name: x, type: ss, server: 1.2.3.4, port: 443, cipher: aes-256-gcm, password: p}")
            .unwrap();
        assert_eq!(c.nodes.len(), 1);
        assert_eq!(c.nodes[0].name, "x");
    }

    #[test]
    fn parse_manual_rejects_empty_and_garbage() {
        assert!(parse_manual("").is_err());
        assert!(parse_manual("   \n  ").is_err());
        assert!(parse_manual("这只是一句话").is_err());
    }

    /// 回归：`contains_share_link` 曾经用 `line[..n]` 按字节切片，
    /// 遇到多字节字符就 panic。往「手动添加」里粘中文即可触发崩溃。
    #[test]
    fn non_ascii_input_never_panics() {
        for s in [
            "这只是一句话",
            "节点名称：东京 01",
            "a",
            "节点",
            "abcdefg日",
            "🇯🇵 日本节点",
            "🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂",
            "一二三四五六七八九十一二三四五六七八九十",
        ] {
            // 只要求「不 panic + 能给出结论」，不关心具体是 Ok 还是 Err。
            let _ = parse_manual(s);
            let _ = detect(s);
        }
    }

    /// 回归：多行正文以链接开头时曾被误判为「单条链接」，只解析出第一个节点。
    #[test]
    fn multi_line_body_starting_with_link_is_not_treated_as_single() {
        let body = "vless://uuid@a.com:443#A\nvless://uuid@b.com:443#B\nvless://uuid@c.com:443#C\n";
        let out = parse_manual(body).unwrap();
        assert_eq!(out.nodes.len(), 3, "多行正文必须全部解析，不能只取第一行");
    }

    #[test]
    fn realm_of_a_realistic_vless_link() {
        let body = "vless://b831381d-6324-4d53-ad4f-8cda48b30811@jp1.example.com:443?encryption=none&security=reality&sni=www.microsoft.com&fp=chrome&pbk=abc123&sid=00&type=grpc&serviceName=grpc#%E4%B8%9C%E4%BA%AC%2001";
        let out = parse_any(body).unwrap();
        let n = &out.nodes[0];
        assert_eq!(n.name, "东京 01");
        assert_eq!(n.address, "jp1.example.com");
        assert_eq!(n.port, 443);
        assert!(matches!(n.transport, Transport::Grpc { .. }));
        assert_eq!(n.tls.effective_security(), "reality");
        assert!(matches!(n.protocol, Protocol::Vless { .. }));
    }
}
