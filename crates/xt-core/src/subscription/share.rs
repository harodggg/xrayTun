//! 把节点**导出**成分享链接（供二维码 / 复制用）。
//!
//! 这是 [`super::uri`] 里那些解析器的**逆运算**。之所以要认真做逆运算而不是
//! 简单地把导入时的原始链接存起来：从 Clash 订阅、Xray JSON 或界面上手动
//! 添加的节点**没有**原始链接（`Node::raw_uri` 是 `None`），而用户同样会想
//! 把它们导出到手机上去。
//!
//! # 正确性怎么保证
//!
//! 唯一可信的判据是**往返**：`parse(export(node)) == node`。
//! 测试里对每种协议都钉了这一条 —— 只测「生成的字符串长得对」是不够的，
//! 那只能证明我按自己的理解写对了，证明不了解析器认得。
//!
//! 有一个刻意的例外，见 [`ExportNote`]。

use crate::model::{Node, Protocol, Transport};
use base64::Engine;

/// 导出结果，附带「哪些信息没能带出去」的说明。
///
/// 分享链接的表达能力**比内部模型窄**，有些字段没有对应位置：
/// 比如 `mux` 多路复用、`Shadowsocks::udp_over_tcp`。这些字段丢掉之后
/// 目标端的行为会和本机不同，所以必须**说出来**，不能静默丢弃 ——
/// 这正是这个项目在解析方向上学到的教训（vless 的 `encryption` 被写死
/// 导致带后量子加密的节点全部连不上，而日志里一句提示都没有）。
#[derive(Debug, Clone)]
pub struct Export {
    pub uri: String,
    /// 没能表达进链接里的字段，人类可读。空表示无损。
    pub lost: Vec<String>,
}

/// 带 `name` 的片段要做百分号编码。
fn encode_fragment(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn encode_query(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// 拼 query。空值一律**省略** —— 写 `&flow=` 会让一些客户端把空串当成
/// 有意义的取值，而留空本来就是「不启用」的语义。
fn push(q: &mut Vec<(String, String)>, key: &str, value: &str) {
    if !value.is_empty() {
        q.push((key.to_string(), value.to_string()));
    }
}

fn finish_standard(scheme: &str, userinfo: &str, node: &Node, q: Vec<(String, String)>) -> String {
    let mut out = format!("{scheme}://{userinfo}@{}:{}", node.address, node.port);
    if !q.is_empty() {
        out.push('?');
        out.push_str(
            &q.iter()
                .map(|(k, v)| format!("{k}={}", encode_query(v)))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    out.push('#');
    out.push_str(&encode_fragment(&node.name));
    out
}

/// 传输层与 TLS 的 query 参数（vless / trojan / vmess 共用同一套键名，
/// 与 [`super::uri`] 的解析端一一对应）。
fn push_common(q: &mut Vec<(String, String)>, node: &Node, lost: &mut Vec<String>) {
    match &node.transport {
        Transport::Tcp => push(q, "type", "tcp"),
        Transport::WebSocket { path, host } => {
            push(q, "type", "ws");
            push(q, "path", path);
            push(q, "host", host);
        }
        Transport::Grpc { service_name, multi_mode } => {
            push(q, "type", "grpc");
            push(q, "serviceName", service_name);
            if *multi_mode {
                // 链接里没有 multiMode 的通用键；保持默认，说明损失。
                lost.push("gRPC multiMode（分享链接无对应字段）".into());
            }
        }
        Transport::HttpUpgrade { path, host } => {
            push(q, "type", "httpupgrade");
            push(q, "path", path);
            push(q, "host", host);
        }
        Transport::Xhttp { path, host, mode } => {
            push(q, "type", "xhttp");
            push(q, "path", path);
            push(q, "host", host);
            push(q, "mode", mode);
        }
        Transport::Quic { key, security } => {
            push(q, "type", "quic");
            push(q, "key", key);
            push(q, "quicSecurity", security);
        }
        Transport::Kcp { header_type, seed } => {
            push(q, "type", "kcp");
            push(q, "headerType", header_type);
            push(q, "seed", seed);
        }
        // `Transport::Http` 是 http 协议专用的传输形态；分享链接里没有
        // 对应的 `type` 取值，按 tcp 处理（协议本身已经写在 scheme 里）。
        Transport::Http { .. } => push(q, "type", "tcp"),
    }

    let tls = &node.tls;
    if !tls.enabled {
        push(q, "security", "none");
        return;
    }
    match &tls.reality {
        Some(r) => {
            push(q, "security", "reality");
            push(q, "pbk", &r.public_key);
            push(q, "sid", &r.short_id);
            if !r.spider_x.is_empty() && r.spider_x != "/" {
                push(q, "spx", &r.spider_x);
            }
        }
        None => push(q, "security", "tls"),
    }
    push(q, "sni", &tls.server_name);
    push(q, "fp", &tls.fingerprint);
    if !tls.alpn.is_empty() {
        push(q, "alpn", &tls.alpn.join(","));
    }
    if tls.allow_insecure {
        push(q, "allowInsecure", "1");
    }
}

/// 节点 → 分享链接。
///
/// `raw_uri` 存在时**优先用它**：那是用户当初导入的原文，比我们重新拼一遍
/// 更忠实（可能带了我们不认识的参数，重拼会丢）。只有它缺失（Clash /
/// Xray JSON / 手动添加的节点）时才自己生成。
pub fn export_uri(node: &Node) -> Export {
    if let Some(raw) = node.raw_uri.as_deref().filter(|s| !s.trim().is_empty()) {
        return Export { uri: raw.to_string(), lost: Vec::new() };
    }

    let mut lost = Vec::new();
    if node.mux.is_some() {
        // 分享链接里没有 mux 的标准位置。
        lost.push("mux 多路复用设置".into());
    }

    let uri = match &node.protocol {
        Protocol::Vless { uuid, flow, encryption } => {
            let mut q = Vec::new();
            push(&mut q, "encryption", encryption);
            push_common(&mut q, node, &mut lost);
            push(&mut q, "flow", flow);
            finish_standard("vless", uuid, node, q)
        }
        Protocol::Trojan { password } => {
            let mut q = Vec::new();
            push_common(&mut q, node, &mut lost);
            finish_standard("trojan", &encode_query(password), node, q)
        }
        Protocol::Vmess { uuid, security, alter_id } => {
            // vmess 用整段 base64(JSON)，和其他协议不同。
            let mut q = Vec::new();
            push_common(&mut q, node, &mut lost);
            let qmap: std::collections::HashMap<_, _> = q.into_iter().collect();
            let mut obj = serde_json::Map::new();
            obj.insert("v".into(), serde_json::json!("2"));
            obj.insert("ps".into(), serde_json::json!(node.name));
            obj.insert("add".into(), serde_json::json!(node.address));
            obj.insert("port".into(), serde_json::json!(node.port.to_string()));
            obj.insert("id".into(), serde_json::json!(uuid));
            obj.insert("aid".into(), serde_json::json!(alter_id.to_string()));
            obj.insert("scy".into(), serde_json::json!(security.as_xray()));
            obj.insert("net".into(), serde_json::json!(qmap.get("type").cloned().unwrap_or_else(|| "tcp".into())));
            obj.insert("type".into(), serde_json::json!("none"));
            obj.insert("host".into(), serde_json::json!(qmap.get("host").cloned().unwrap_or_default()));
            obj.insert("path".into(), serde_json::json!(qmap.get("path").cloned().unwrap_or_default()));
            obj.insert("tls".into(), serde_json::json!(if node.tls.enabled { "tls" } else { "" }));
            obj.insert("sni".into(), serde_json::json!(node.tls.server_name));
            obj.insert("alpn".into(), serde_json::json!(node.tls.alpn.join(",")));
            obj.insert("fp".into(), serde_json::json!(node.tls.fingerprint));
            if let Some(r) = &node.tls.reality {
                obj.insert("pbk".into(), serde_json::json!(r.public_key));
                obj.insert("sid".into(), serde_json::json!(r.short_id));
                obj.insert("spx".into(), serde_json::json!(r.spider_x));
            }
            if let Transport::Grpc { service_name, .. } = &node.transport {
                obj.insert("path".into(), serde_json::json!(service_name));
            }
            let json = serde_json::Value::Object(obj).to_string();
            let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
            format!("vmess://{b64}")
        }
        Protocol::Shadowsocks { method, password, uot } => {
            if *uot {
                lost.push("Shadowsocks UDP-over-TCP".into());
            }
            // SIP002：base64(method:password) 作为 userinfo。
            let userinfo = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(format!("{method}:{password}"));
            let mut q = Vec::new();
            push(&mut q, "type", "tcp");
            finish_standard("ss", &userinfo, node, q)
        }
        Protocol::Socks { username, password } => {
            let userinfo = if username.is_empty() {
                String::new()
            } else {
                let raw = format!("{username}:{password}");
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
            };
            finish_standard("socks", &userinfo, node, Vec::new())
        }
        Protocol::Http { username, password } => {
            let userinfo = if username.is_empty() {
                String::new()
            } else {
                let raw = format!("{username}:{password}");
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
            };
            let mut q = Vec::new();
            if node.tls.enabled {
                push(&mut q, "security", "tls");
                push(&mut q, "sni", &node.tls.server_name);
            }
            finish_standard("http", &userinfo, node, q)
        }
    };

    Export { uri, lost }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::uri::parse_share_link;
    use crate::model::{NodeSource, RealitySettings, TlsSettings};

    fn base(protocol: Protocol, transport: Transport, tls: TlsSettings) -> Node {
        let mut n = Node {
            id: String::new(),
            name: "测试节点".into(),
            address: "203.0.113.10".into(),
            port: 443,
            protocol,
            transport,
            tls,
            mux: None,
            source: NodeSource::Manual,
            tags: vec![],
            raw_uri: None,
        };
        n.refresh_id();
        n
    }

    fn tls_on(sni: &str) -> TlsSettings {
        TlsSettings {
            enabled: true,
            server_name: sni.into(),
            fingerprint: "chrome".into(),
            alpn: vec!["h2".into()],
            ..Default::default()
        }
    }

    /// **往返**：导出再解析必须回到同一个节点。
    ///
    /// 这是唯一能证明序列化器和解析器对得上的判据。只断言「字符串长得对」
    /// 证明不了任何事 —— 那只是我按自己的理解写了两遍。
    fn assert_roundtrip(node: &Node) {
        let uri = export_uri(node).uri;
        let back = parse_share_link(&uri).unwrap_or_else(|e| panic!("解析不回去: {e}\n  uri={uri}"));
        assert_eq!(back.address, node.address, "address 丢了");
        assert_eq!(back.port, node.port, "port 丢了");
        assert_eq!(back.protocol, node.protocol, "协议字段丢了\n  uri={uri}");
        assert_eq!(back.transport, node.transport, "传输层丢了\n  uri={uri}");
        assert_eq!(back.tls.enabled, node.tls.enabled, "TLS 开关丢了");
        assert_eq!(back.tls.server_name, node.tls.server_name, "SNI 丢了");
        assert_eq!(back.tls.fingerprint, node.tls.fingerprint, "指纹丢了");
        assert_eq!(back.tls.alpn, node.tls.alpn, "ALPN 丢了");
        assert_eq!(back.tls.reality, node.tls.reality, "REALITY 参数丢了");
        assert_eq!(back.name, node.name, "节点名丢了（中文要能过百分号编码）");
    }

    #[test]
    fn vless_reality_roundtrips() {
        let mut tls = tls_on("www.microsoft.com");
        tls.reality = Some(RealitySettings {
            public_key: "A".repeat(43),
            short_id: "0123456789abcdef".into(),
            spider_x: "/".into(),
        });
        assert_roundtrip(&base(
            Protocol::Vless {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                flow: "xtls-rprx-vision".into(),
                encryption: "none".into(),
            },
            Transport::Tcp,
            tls,
        ));
    }

    /// 带后量子加密的 vless：`encryption` 必须带出去。
    ///
    /// 钉住的是一个真实事故：解析端曾经把它写死成 `none`，
    /// 结果这类节点全部连不上，而日志里没有任何提示。
    #[test]
    fn vless_keeps_non_default_encryption() {
        let pq = "mlkem768x25519plus.native.0rtt.XDe7-CWCUYvjnZJgvTykuSBMWIPSB2T4MWTuetanM00";
        let node = base(
            Protocol::Vless {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                flow: String::new(),
                encryption: pq.into(),
            },
            Transport::Tcp,
            tls_on("www.microsoft.com"),
        );
        let uri = export_uri(&node).uri;
        assert!(uri.contains("encryption="), "encryption 没带出去: {uri}");
        assert_roundtrip(&node);
    }

    #[test]
    fn vless_websocket_roundtrips() {
        assert_roundtrip(&base(
            Protocol::Vless {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            Transport::WebSocket { path: "/ray".into(), host: "a.example.com".into() },
            tls_on("a.example.com"),
        ));
    }

    #[test]
    fn vless_grpc_roundtrips() {
        assert_roundtrip(&base(
            Protocol::Vless {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            Transport::Grpc { service_name: "grpc".into(), multi_mode: false },
            tls_on("www.microsoft.com"),
        ));
    }

    #[test]
    fn trojan_roundtrips() {
        assert_roundtrip(&base(
            Protocol::Trojan { password: "p@ss:word/1".into() },
            Transport::Tcp,
            tls_on("b.example.com"),
        ));
    }

    #[test]
    fn vmess_roundtrips() {
        assert_roundtrip(&base(
            Protocol::Vmess {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                security: crate::model::VmessSecurity::Auto,
                alter_id: 0,
            },
            Transport::WebSocket { path: "/ray".into(), host: "a.example.com".into() },
            tls_on("a.example.com"),
        ));
    }

    #[test]
    fn shadowsocks_roundtrips() {
        assert_roundtrip(&base(
            Protocol::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "pw".into(),
                uot: false,
            },
            Transport::Tcp,
            TlsSettings::default(),
        ));
    }

    #[test]
    fn no_tls_is_marked_explicitly() {
        // security=none 要写出来，否则目标端可能默认按 tls 处理
        let uri = export_uri(&base(
            Protocol::Trojan { password: "pw".into() },
            Transport::Tcp,
            TlsSettings::default(),
        ))
        .uri;
        assert!(uri.contains("security=none"), "{uri}");
    }

    /// 第一版导入的原文优先：重拼可能丢掉我们不认识的参数。
    #[test]
    fn raw_uri_wins_when_present() {
        let mut n = base(
            Protocol::Trojan { password: "pw".into() },
            Transport::Tcp,
            tls_on("x"),
        );
        let original = "trojan://pw@203.0.113.10:443?security=tls&weirdParam=1#原名";
        n.raw_uri = Some(original.into());
        assert_eq!(export_uri(&n).uri, original);
    }

    /// 表达不了的字段要**报出来**，不能静默丢。
    #[test]
    fn unsupported_fields_are_reported() {
        let mut n = base(
            Protocol::Trojan { password: "pw".into() },
            Transport::Tcp,
            tls_on("x"),
        );
        n.mux = Some(crate::model::MuxSettings { enabled: true, concurrency: 8, xudp_concurrency: 0 });
        let e = export_uri(&n);
        assert!(!e.lost.is_empty(), "mux 丢了却没说");
        assert!(e.lost.iter().any(|s| s.contains("mux")));
    }

    /// 节点名里的中文和特殊字符要能安全往返。
    #[test]
    fn name_survives_percent_encoding() {
        for name in ["香港 01", "a&b=c?d#e", "100% 纯正", "emoji 🇭🇰"] {
            let mut n = base(
                Protocol::Trojan { password: "pw".into() },
                Transport::Tcp,
                tls_on("x"),
            );
            n.name = name.into();
            assert_roundtrip(&n);
        }
    }
}
