//! xt-subs —— 订阅解析（4 种格式）→ 节点列表，纯函数
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! **纯函数**：输入是订阅正文（`&str`），输出是节点列表。这里**不抓 URL** ——
//! 抓取是 IO，属于 daemon；把 IO 混进来就没法在单元测试里脱离网络验证解析。
//!
//! 四种格式：
//!
//! | 格式 | 特征 |
//! |---|---|
//! | [`SubscriptionFormat::UriList`] | 每行一个 `vmess/ss/trojan/vless://` 链接 |
//! | [`SubscriptionFormat::Base64UriList`] | 整体 base64，解码后是上面的链接列表 |
//! | [`SubscriptionFormat::ClashYaml`] | `proxies:` 列表（Clash / Mihomo） |
//! | [`SubscriptionFormat::XrayJson`] | 顶层 JSON 且含 `outbounds` |
//!
//! ## 为什么坏条目会让整个解析失败（I2 无回落）
//!
//! 旧实现是「尽力而为」：坏行记 warning 后跳过。那等于替用户决定了「这条不重要」，
//! 而用户看到的节点数变少时**无从知道少了什么、少了几个**。这里的规则是：任何一条
//! 解析不出来的条目都返回 `InvalidRequest`，错误里带**行号/索引 + 原始片段**，
//! 由调用方决定是整份拒绝还是让用户改订阅。宁缺毋假。
//!
//! ## NodeId 的稳定派生
//!
//! `base64url(protocol|host|port|name)`（无填充）。刻意**不用哈希**：
//! 哈希不可逆、不可读，排查时只能对着数据库找到底是哪个节点；base64 解码后
//! 就是 `vless|a.example.com|443|东京 01`，人一眼能认。它也不含凭据 ——
//! 参与派生的只有协议名、主机、端口、显示名，UUID/密码**不进去**。
//! 因此它同时满足：跨订阅刷新稳定（同一台服务器 → 同一个 id）、可读、无泄漏。

mod clash;
mod stream;
mod uri;
mod xray_json;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use xt_contract::error::{bad_request, ErrorBody};
use xt_contract::model::NodeId;

/// 订阅正文的格式。嗅探只用于**选择解析器**，不改变解析结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionFormat {
    UriList,
    Base64UriList,
    ClashYaml,
    XrayJson,
}

/// 解析出来的节点。
///
/// * `id`：见模块文档的稳定派生规则；
/// * `protocol`：上游协议名（`vmess`/`vless`/`trojan`/`shadowsocks`/`socks`/`http`）；
/// * `endpoint`：真实 `host:port`（IPv6 带方括号），给界面显示用；
/// * `outbound`：Xray outbound 的**内容**（`protocol` + `settings` + `streamSettings`），
///   **不含 `tag`** —— tag 由 xt-xrayconf 按 `node-<id>` 规则生成，
///   这样「一个节点一个 tag」这件事只有一个地方说了算。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParsedNode {
    pub id: NodeId,
    pub name: String,
    pub protocol: String,
    pub endpoint: String,
    pub outbound: serde_json::Value,
}

/// 一次解析的结果。`format` 是**实际**用到的解析器，不是猜的意图。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Subscription {
    pub format: SubscriptionFormat,
    pub nodes: Vec<ParsedNode>,
}

/// 解析订阅正文，自动嗅探格式。
pub fn parse(body: &str) -> Result<Subscription, ErrorBody> {
    let trimmed = body.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(bad_request("订阅正文为空，没有可解析的节点"));
    }

    match detect(trimmed)? {
        SubscriptionFormat::UriList => Ok(Subscription {
            format: SubscriptionFormat::UriList,
            nodes: parse_links(trimmed)?,
        }),
        SubscriptionFormat::Base64UriList => {
            let decoded = uri::decode_base64(trimmed).map_err(|reason| {
                bad_request("订阅不是合法的 base64")
                    .with_detail(serde_json::json!({ "reason": reason }))
            })?;
            let text = String::from_utf8(decoded).map_err(|err| {
                bad_request("base64 订阅解码后不是 UTF-8 文本")
                    .with_detail(serde_json::json!({ "reason": err.to_string() }))
            })?;
            Ok(Subscription {
                format: SubscriptionFormat::Base64UriList,
                nodes: parse_links(&text)?,
            })
        }
        SubscriptionFormat::ClashYaml => Ok(Subscription {
            format: SubscriptionFormat::ClashYaml,
            nodes: clash::parse(trimmed)?,
        }),
        SubscriptionFormat::XrayJson => Ok(Subscription {
            format: SubscriptionFormat::XrayJson,
            nodes: xray_json::parse(trimmed)?,
        }),
    }
}

/// 只解析分享链接列表（`vmess://` / `ss://` / `trojan://` / `vless://`）。
///
/// 独立暴露的理由：daemon 手工导入、E2E 里构造单条节点、以及 base64 解码后的
/// 二次解析都要用它，而它们都不需要重新做格式嗅探。
pub fn parse_links(text: &str) -> Result<Vec<ParsedNode>, ErrorBody> {
    let mut nodes = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        // 空行不是「坏条目」：它不承载节点，跳过不会隐瞒任何东西。
        if line.is_empty() {
            continue;
        }
        nodes.push(uri::parse_share_link(line, idx + 1)?);
    }
    if nodes.is_empty() {
        return Err(bad_request("订阅里没有任何节点（没有 vmess/ss/trojan/vless 链接）"));
    }
    Ok(nodes)
}

/// 嗅探格式。失败返回可读错误，不猜、不试第二种解析器。
fn detect(body: &str) -> Result<SubscriptionFormat, ErrorBody> {
    if body.starts_with('{') {
        // 是 JSON 就按 JSON 处理：形状不对就是错误，不再拿它去试 base64。
        return Ok(SubscriptionFormat::XrayJson);
    }
    if body.contains("proxies:") || body.contains("proxy-groups:") {
        return Ok(SubscriptionFormat::ClashYaml);
    }
    if uri::looks_like_link_list(body) {
        return Ok(SubscriptionFormat::UriList);
    }
    if let Ok(decoded) = uri::decode_base64(body) {
        if let Ok(text) = String::from_utf8(decoded) {
            let text_trimmed = text.trim();
            if uri::looks_like_link_list(text_trimmed) || text_trimmed.starts_with('{') {
                return Ok(SubscriptionFormat::Base64UriList);
            }
        }
    }
    Err(bad_request(
        "无法识别订阅格式（既不是链接列表 / base64 订阅，也不是 Clash YAML 或 Xray JSON）",
    )
    .with_detail(serde_json::json!({ "head": snippet(body, 120) })))
}

/// NodeId 的唯一生成入口。任何解析路径都必须走这里，
/// 否则「同一个节点在不同格式下得到不同 id」会让选择记忆失效。
pub(crate) fn node_id(protocol: &str, host: &str, port: u16, name: &str) -> NodeId {
    // host 统一小写：DNS 名大小写不敏感，同一台服务器不该因大小写产生两个 id。
    let canonical = format!("{protocol}|{}|{port}|{name}", host.to_ascii_lowercase());
    NodeId::new(URL_SAFE_NO_PAD.encode(canonical))
}

/// 组出一个节点。所有解析器共用，保证 id / endpoint / outbound 三者的口径一致。
pub(crate) fn assemble(
    protocol: &str,
    host: &str,
    port: u16,
    name: String,
    settings: serde_json::Value,
    stream: serde_json::Value,
) -> ParsedNode {
    let name = if name.trim().is_empty() { format!("{host}:{port}") } else { name };
    ParsedNode {
        id: node_id(protocol, host, port, &name),
        name,
        protocol: protocol.to_string(),
        endpoint: endpoint_of(host, port),
        outbound: serde_json::json!({
            "protocol": protocol,
            "settings": settings,
            "streamSettings": stream,
        }),
    }
}

/// `host:port`，IPv6 加方括号（`[::1]:443`），与 `SocketAddr` 的写法一致。
pub(crate) fn endpoint_of(host: &str, port: u16) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 一条条目解析失败的统一形状：**必须**带行号/索引与原始片段。
pub(crate) fn entry_error(where_: Where, reason: impl Into<String>) -> ErrorBody {
    let reason = reason.into();
    match where_ {
        Where::LineRaw(line, raw) => bad_request(format!("第 {line} 行解析失败: {reason}")).with_detail(
            serde_json::json!({ "line": line, "raw": raw, "reason": reason }),
        ),
        Where::Index(index, raw) => bad_request(format!("第 {index} 条解析失败: {reason}")).with_detail(
            serde_json::json!({ "index": index, "raw": raw, "reason": reason }),
        ),
    }
}

/// 解析失败发生在哪一条上。行号与索引都带上原始片段，用户才能自己定位。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Where<'a> {
    LineRaw(usize, &'a str),
    Index(usize, &'a str),
}

/// 截断到 `max` 个**字符**（不是字节）并加省略号。
/// 用字符边界而不是字节切片：订阅里出现中文名时按字节切会 panic。
pub(crate) fn snippet(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_stable_readable_and_credential_free() {
        let a = node_id("vless", "A.Example.com", 443, "东京 01");
        let b = node_id("vless", "a.example.com", 443, "东京 01");
        assert_eq!(a, b, "主机大小写不应产生两个 id");

        let decoded = URL_SAFE_NO_PAD.decode(a.as_str()).expect("id 必须可解码");
        assert_eq!(String::from_utf8(decoded).expect("utf8"), "vless|a.example.com|443|东京 01");

        let other = node_id("vless", "a.example.com", 8443, "东京 01");
        assert_ne!(a, other, "端口不同必须换 id");

        // 凭据不参与派生：同一 tuple 无论凭据如何都是同一个 id。
        let with_secret = node_id("vless", "a.example.com", 443, "东京 01");
        assert_eq!(a, with_secret);
    }

    #[test]
    fn base64_subscription_of_vless_links_parses() {
        let links = "vless://11111111-1111-1111-1111-111111111111@a.example.com:443?encryption=none&security=tls&sni=a.example.com#A\n\
                     vless://22222222-2222-2222-2222-222222222222@b.example.com:8443?encryption=none&security=reality&pbk=PUB&sid=ab&sni=www.apple.com#B";
        let packed = base64::engine::general_purpose::STANDARD.encode(links);
        let sub = parse(&packed).expect("base64 订阅应能解析");
        assert_eq!(sub.format, SubscriptionFormat::Base64UriList);
        assert_eq!(sub.nodes.len(), 2);
        assert_eq!(sub.nodes[0].name, "A");
        assert_eq!(sub.nodes[1].endpoint, "b.example.com:8443");
        assert_eq!(sub.nodes[1].protocol, "vless");
    }

    #[test]
    fn empty_subscription_is_an_error_not_an_empty_list() {
        let err = parse("   \n").expect_err("空订阅必须报错");
        assert!(err.message.contains("为空"), "{err:?}");
    }

    #[test]
    fn unrecognizable_body_is_an_error_with_head() {
        let err = parse("hello, this is not a subscription").expect_err("必须报错");
        let detail = err.detail.expect("带原始片段");
        assert!(detail["head"].as_str().is_some_and(|h| h.contains("hello")));
    }
}
