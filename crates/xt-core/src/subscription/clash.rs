//! Clash / Mihomo 订阅（YAML）解析。
//!
//! 只关心 `proxies:` 这一个列表 —— 订阅里的 `proxy-groups` / `rules` 属于
//! Clash 自己的路由模型，导入到本应用时会被忽略（我们有自己的规则系统，
//! 且语义并不一一对应，强行翻译只会让用户困惑）。
//!
//! 用 `serde_yaml::Value` 而不是强类型结构体，理由同 [`super::uri`]：
//! 各家机场的 YAML 字段差异极大，容错比类型安全更重要。

use serde_yaml::Value;

use crate::error::{Error, Result};
use crate::model::{
    Node, NodeSource, Protocol, RealitySettings, TlsSettings, Transport, VmessSecurity,
};

use super::{ParseOutcome, SubscriptionFormat};

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key)
}

fn s(v: &Value, key: &str) -> Option<String> {
    match get(v, key)? {
        Value::String(x) if !x.is_empty() => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn u16_of(v: &Value, key: &str) -> Option<u16> {
    match get(v, key)? {
        Value::Number(n) => n.as_u64().map(|x| x as u16),
        Value::String(x) => x.trim().parse().ok(),
        _ => None,
    }
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    match get(v, key)? {
        Value::Number(n) => n.as_u64().map(|x| x as u32),
        Value::String(x) => x.trim().parse().ok(),
        _ => None,
    }
}

fn bool_of(v: &Value, key: &str) -> bool {
    match get(v, key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(x)) => matches!(x.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    match get(v, key) {
        Some(Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Some(Value::String(x)) => x.split(',').map(|p| p.trim().to_string()).collect(),
        _ => vec![],
    }
}

/// 解析 Clash 订阅正文。
pub fn parse(body: &str) -> Result<ParseOutcome> {
    let doc: Value = serde_yaml::from_str(body).map_err(|e| Error::Yaml(e.to_string()))?;
    let mut outcome = ParseOutcome::new(SubscriptionFormat::ClashYaml);

    let Some(proxies) = get(&doc, "proxies") else {
        return Err(Error::Yaml("订阅中没有 proxies 字段".into()));
    };
    let Some(list) = proxies.as_sequence() else {
        return Err(Error::Yaml("proxies 不是数组".into()));
    };

    for (idx, item) in list.iter().enumerate() {
        match convert(item) {
            Ok(node) => outcome.nodes.push(node),
            Err(e) => outcome.warnings.push(format!("proxies[{idx}] 已跳过: {e}")),
        }
    }
    Ok(outcome)
}

/// 解析**单条** Clash proxy 定义。
///
/// 用户经常直接从机场文档 / Clash 配置片段里复制一条 proxy 贴进来，
/// 而它没有 `proxies:` 外壳。这里容忍三种形态：
///
/// 1. 完整外壳 `proxies: [...]` → 直接交给 [`parse`]
/// 2. 一个映射 `{name: a, type: vless, ...}`
/// 3. 一个映射数组 `- {name: a, ...}`（用户可能连前面的 `proxies:` 一起漏掉）
///
/// 之所以不要求用户自己包一层外壳：他们要粘贴的是**内容**，
/// 而「这段内容属于哪种格式」应该由程序判断，不该是使用者的负担。
pub fn parse_single(body: &str) -> Result<ParseOutcome> {
    let doc: Value = serde_yaml::from_str(body).map_err(|e| Error::Yaml(e.to_string()))?;

    if get(&doc, "proxies").is_some() {
        return parse(body);
    }

    let items: Vec<&Value> = match &doc {
        Value::Sequence(seq) => seq.iter().collect(),
        Value::Mapping(_) => vec![&doc],
        _ => return Err(Error::Yaml("既不是 proxy 映射也不是数组".into())),
    };

    let mut outcome = ParseOutcome::new(SubscriptionFormat::ClashYaml);
    for (i, item) in items.iter().enumerate() {
        match convert(item) {
            Ok(node) => outcome.nodes.push(node),
            Err(e) => outcome.warnings.push(format!("第 {} 条已跳过: {e}", i + 1)),
        }
    }
    if outcome.nodes.is_empty() {
        return Err(Error::Yaml(
            "没能解析出节点。需要至少有 `type` / `server` / `port` 三个字段".into(),
        ));
    }
    Ok(outcome)
}

fn convert(v: &Value) -> Result<Node> {
    let kind = s(v, "type").ok_or(Error::MissingField("type"))?.to_ascii_lowercase();
    let address = s(v, "server").ok_or(Error::MissingField("server"))?;
    let port = u16_of(v, "port").ok_or(Error::MissingField("port"))?;
    let name = s(v, "name").unwrap_or_else(|| format!("{address}:{port}"));

    let protocol = match kind.as_str() {
        "ss" => Protocol::Shadowsocks {
            method: s(v, "cipher").ok_or(Error::MissingField("cipher"))?.to_ascii_lowercase(),
            password: s(v, "password").ok_or(Error::MissingField("password"))?,
            uot: bool_of(v, "udp-over-tcp"),
        },
        "vmess" => Protocol::Vmess {
            uuid: s(v, "uuid").ok_or(Error::MissingField("uuid"))?,
            alter_id: u32_of(v, "alterId").unwrap_or(0),
            security: match s(v, "cipher").unwrap_or_default().to_ascii_lowercase().as_str() {
                "none" => VmessSecurity::None,
                "zero" => VmessSecurity::Zero,
                "aes-128-gcm" => VmessSecurity::Aes128Gcm,
                "chacha20-poly1305" => VmessSecurity::Chacha20Poly1305,
                _ => VmessSecurity::Auto,
            },
        },
        "vless" => Protocol::Vless {
            uuid: s(v, "uuid").ok_or(Error::MissingField("uuid"))?,
            flow: s(v, "flow").unwrap_or_default(),
            // `encryption` 必须读出来，不能写死 "none"。
            //
            // Xray 25.x 起 VLESS 支持 **后量子加密**（`mlkem768x25519plus.*`），
            // 服务端启用后，客户端用 "none" 握手会被直接拒绝。
            // 这个字段在 Clash 订阅里是明文出现的，写死等于静默丢弃一个必需参数。
            encryption: s(v, "encryption").unwrap_or_else(|| "none".into()),
        },
        "trojan" => Protocol::Trojan {
            password: s(v, "password").ok_or(Error::MissingField("password"))?,
        },
        "socks5" | "socks" => Protocol::Socks {
            username: s(v, "username").unwrap_or_default(),
            password: s(v, "password").unwrap_or_default(),
        },
        "http" => Protocol::Http {
            username: s(v, "username").unwrap_or_default(),
            password: s(v, "password").unwrap_or_default(),
        },
        // 把实际收到的 type 打进错误里 —— 订阅里出现新类型时，
        // 用户看到的是「哪个值不认识」，而不是一句笼统的解析失败。
        other => {
            return Err(Error::UnsupportedValue {
                field: "type",
                value: other.to_string(),
                supported: "ss/vmess/vless/trojan/socks5/http",
            })
        }
    };

    // ---- 传输层 ----
    let network = s(v, "network").unwrap_or_else(|| "tcp".into()).to_ascii_lowercase();
    let ws_path = s(v, "ws-path").or_else(|| {
        get(v, "ws-opts").and_then(|o| s(o, "path"))
    });
    let ws_host = get(v, "ws-opts")
        .and_then(|o| get(o, "headers"))
        .and_then(|h| s(h, "Host").or_else(|| s(h, "host")))
        .or_else(|| s(v, "ws-headers"));

    let transport = match network.as_str() {
        "ws" => Transport::WebSocket {
            path: ws_path.unwrap_or_else(|| "/".into()),
            host: ws_host.clone().unwrap_or_default(),
        },
        "grpc" => Transport::Grpc {
            service_name: get(v, "grpc-opts")
                .and_then(|o| s(o, "grpc-service-name"))
                .unwrap_or_default(),
            multi_mode: false,
        },
        "h2" | "http" => Transport::Http {
            host: ws_host.clone().unwrap_or_default(),
            path: ws_path.unwrap_or_else(|| "/".into()),
        },
        _ => Transport::Tcp,
    };

    // ---- TLS ----
    let reality = get(v, "reality-opts").map(|o| RealitySettings {
        public_key: s(o, "public-key").unwrap_or_default(),
        short_id: s(o, "short-id").unwrap_or_default(),
        spider_x: "/".into(),
    });
    let tls_enabled = bool_of(v, "tls") || reality.is_some();
    let tls = TlsSettings {
        enabled: tls_enabled,
        server_name: s(v, "servername")
            .or_else(|| s(v, "sni"))
            .unwrap_or_else(|| ws_host.clone().unwrap_or_default()),
        allow_insecure: bool_of(v, "skip-cert-verify"),
        alpn: str_list(v, "alpn"),
        fingerprint: s(v, "client-fingerprint").unwrap_or_default(),
        reality,
    };

    let mut node = Node {
        id: String::new(),
        name,
        address,
        port,
        protocol,
        transport,
        tls,
        mux: None,
        source: NodeSource::Manual,
        tags: vec![],
        raw_uri: None,
    };
    node.refresh_id();
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
port: 7890
proxies:
  - name: "香港 01"
    type: ss
    server: hk1.example.com
    port: 443
    cipher: chacha20-ietf-poly1305
    password: "pw1"
    udp: true
  - name: "日本 WS"
    type: vmess
    server: jp1.example.com
    port: 443
    uuid: 11111111-2222-3333-4444-555555555555
    alterId: 0
    cipher: auto
    network: ws
    tls: true
    servername: jp1.example.com
    skip-cert-verify: false
    ws-opts:
      path: /ray
      headers:
        Host: jp1.example.com
  - name: "REALITY"
    type: vless
    server: r.example.com
    port: 8443
    uuid: abc
    flow: xtls-rprx-vision
    tls: true
    servername: www.microsoft.com
    client-fingerprint: chrome
    reality-opts:
      public-key: PUBKEY
      short-id: "00"
  - name: "不支持的协议"
    type: hysteria2
    server: x.example.com
    port: 443
proxy-groups:
  - name: auto
    type: url-test
    proxies: ["香港 01"]
"#;

    #[test]
    fn parses_three_supported_proxies_and_warns_on_one() {
        let out = parse(SAMPLE).unwrap();
        assert_eq!(out.format, SubscriptionFormat::ClashYaml);
        assert_eq!(out.nodes.len(), 3);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("proxies[3]"));
    }

    /// 跳过的节点必须在告警里**说清楚是哪个 type 不认识**。
    ///
    /// 这条钉住的是一个真实踩过的坑：订阅导入后节点数比订阅里少，
    /// 而界面上只有一句「已跳过」，用户完全不知道是哪种协议没被支持。
    #[test]
    fn unsupported_type_is_named_in_the_warning() {
        let out = parse(SAMPLE).unwrap();
        assert!(
            out.warnings[0].contains("hysteria2"),
            "告警里应出现实际收到的 type，实际是：{}",
            out.warnings[0]
        );
        assert!(
            out.warnings[0].contains("ss/vmess/vless/trojan/socks5/http"),
            "告警里应列出支持的取值，实际是：{}",
            out.warnings[0]
        );
    }

    #[test]
    fn ss_fields_survive() {
        let out = parse(SAMPLE).unwrap();
        let n = out.nodes.iter().find(|n| n.name == "香港 01").unwrap();
        match &n.protocol {
            Protocol::Shadowsocks { method, password, .. } => {
                assert_eq!(method, "chacha20-ietf-poly1305");
                assert_eq!(password, "pw1");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ws_opts_and_host_header() {
        let out = parse(SAMPLE).unwrap();
        let n = out.nodes.iter().find(|n| n.name == "日本 WS").unwrap();
        match &n.transport {
            Transport::WebSocket { path, host } => {
                assert_eq!(path, "/ray");
                assert_eq!(host, "jp1.example.com");
            }
            other => panic!("{other:?}"),
        }
        assert!(n.tls.enabled);
    }

    #[test]
    fn reality_opts_imply_tls() {
        let out = parse(SAMPLE).unwrap();
        let n = out.nodes.iter().find(|n| n.name == "REALITY").unwrap();
        assert_eq!(n.tls.effective_security(), "reality");
        assert_eq!(n.tls.reality.as_ref().unwrap().public_key, "PUBKEY");
        assert_eq!(n.tls.fingerprint, "chrome");
    }

    #[test]
    fn vless_encryption_is_not_hardcoded() {
        // 回归测试：曾经这里写死 "none"，导致启用后量子加密的节点全部连不上。
        let yaml = r#"
proxies:
  - name: "pq"
    type: vless
    server: 1.2.3.4
    port: 443
    uuid: b831381d-6324-4d53-ad4f-8cda48b30811
    encryption: mlkem768x25519plus.native.0rtt.XDe7-CWCUYvjnZJgvTykuSBMWIPSB2T4MWTuetanM00
    tls: true
    servername: www.microsoft.com
"#;
        let out = parse(yaml).unwrap();
        match &out.nodes[0].protocol {
            Protocol::Vless { encryption, .. } => {
                assert!(
                    encryption.starts_with("mlkem768x25519plus"),
                    "encryption 被丢弃了：{encryption}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn vless_encryption_defaults_to_none_when_absent() {
        let yaml = "proxies:\n  - {name: a, type: vless, server: 1.2.3.4, port: 443, uuid: u}\n";
        let out = parse(yaml).unwrap();
        match &out.nodes[0].protocol {
            Protocol::Vless { encryption, .. } => assert_eq!(encryption, "none"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn single_proxy_without_wrapper_parses() {
        // 用户从 Clash 片段里复制一条贴进来 —— 没有 proxies: 外壳。
        let single = "{name: hk-example-01, server: 203.0.113.10, port: 8443, \
client-fingerprint: chrome, \
encryption: mlkem768x25519plus.native.0rtt.XDe7-CWCUYvjnZJgvTykuSBMWIPSB2T4MWTuetanM00, \
network: tcp, reality-opts: {public-key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA, \
short-id: 0123456789abcdef}, servername: www.microsoft.com, tls: true, type: vless, \
udp: true, uuid: b831381d-6324-4d53-ad4f-8cda48b30811, xudp: true, \
skip-cert-verify: true, tfo: false}";
        let out = parse_single(single).unwrap();
        assert_eq!(out.nodes.len(), 1);
        let n = &out.nodes[0];
        assert_eq!(n.name, "hk-example-01");
        assert_eq!(n.address, "203.0.113.10");
        assert_eq!(n.port, 8443);
        assert_eq!(n.tls.effective_security(), "reality");
        assert_eq!(n.tls.fingerprint, "chrome");
        let r = n.tls.reality.as_ref().unwrap();
        assert_eq!(r.public_key, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(r.short_id, "0123456789abcdef");
        match &n.protocol {
            Protocol::Vless { encryption, .. } => {
                assert!(encryption.starts_with("mlkem768x25519plus"), "{encryption}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn single_proxy_accepts_sequence_without_wrapper() {
        let seq = "- {name: a, type: ss, server: 1.2.3.4, port: 443, cipher: aes-256-gcm, password: p}\n";
        let out = parse_single(seq).unwrap();
        assert_eq!(out.nodes.len(), 1);
    }

    #[test]
    fn single_proxy_with_wrapper_delegates_to_full_parse() {
        let out = parse_single(SAMPLE).unwrap();
        assert_eq!(out.nodes.len(), 3, "带 proxies: 外壳时应走完整解析");
    }

    #[test]
    fn single_proxy_garbage_is_an_error() {
        assert!(parse_single("just a string").is_err());
        assert!(parse_single("{foo: bar}").is_err());
    }

    #[test]
    fn missing_proxies_key_is_an_error() {
        assert!(parse("rules:\n  - MATCH,DIRECT\n").is_err());
    }
}
