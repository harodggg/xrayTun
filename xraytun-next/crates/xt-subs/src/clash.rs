//! Clash / Mihomo 订阅（YAML）解析。
//!
//! 只关心 `proxies:` 列表 —— `proxy-groups` / `rules` 是 Clash 自己的路由模型，
//! 本应用的语义与之不一一对应，强行翻译只会让用户以为自己的分流规则被继承了。
//!
//! 用 `serde_yaml::Value` 而不是强类型结构体：各家机场的字段差异极大，
//! 而类型安全在这里的收益远小于「多认一种写法」。
//!
//! 坏条目**不跳过**：一条 `type` 不认识的 proxy 会让整份订阅解析失败，
//! 错误里带索引与原始 YAML 片段（I2 无回落）。

use serde_json::json;
use serde_yaml::Value;

use xt_contract::error::ErrorBody;

use crate::stream::{self, Stream};
use crate::{assemble, entry_error, snippet, ParsedNode, Where};

pub(crate) fn parse(body: &str) -> Result<Vec<ParsedNode>, ErrorBody> {
    let doc: Value = serde_yaml::from_str(body).map_err(|err| {
        xt_contract::error::bad_request(format!("Clash YAML 解析失败: {err}"))
            .with_detail(json!({ "reason": err.to_string() }))
    })?;

    let proxies = doc.get("proxies").ok_or_else(|| {
        xt_contract::error::bad_request("Clash 订阅里没有 proxies 字段（只有 rules / proxy-groups 不构成节点列表）")
    })?;
    let list = proxies
        .as_sequence()
        .ok_or_else(|| xt_contract::error::bad_request("Clash 的 proxies 不是数组"))?;
    if list.is_empty() {
        return Err(xt_contract::error::bad_request("Clash 订阅的 proxies 是空数组"));
    }

    let mut nodes = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        // 索引从 1 开始：错误信息是给人看的，`proxies[1]` 比 `proxies[0]` 好对。
        let raw = serde_yaml::to_string(item).unwrap_or_else(|_| "<无法回显>".into());
        let raw = snippet(raw.trim(), 160);
        nodes.push(convert(item).map_err(|reason| entry_error(Where::Index(index + 1, &raw), reason))?);
    }
    Ok(nodes)
}

fn convert(item: &Value) -> Result<ParsedNode, String> {
    let kind = text(item, "type").ok_or("缺少 type")?.to_ascii_lowercase();
    let address = text(item, "server").ok_or("缺少 server")?;
    let port = number(item, "port").ok_or("缺少 port（或不是数字）")?;
    let port = u16::try_from(port).map_err(|_| format!("port 超出范围: {port}"))?;
    let name = text(item, "name").unwrap_or_default();

    let mut s = Stream::with_network(&text(item, "network").unwrap_or_else(|| "tcp".into()));
    s.path = nested(item, &["ws-opts"], "path")
        .or_else(|| text(item, "ws-path"))
        .unwrap_or_else(|| "/".into());
    s.host = nested(item, &["ws-opts", "headers"], "Host")
        .or_else(|| nested(item, &["ws-opts", "headers"], "host"))
        .or_else(|| text(item, "ws-headers"))
        .unwrap_or_default();
    s.service_name = nested(item, &["grpc-opts"], "grpc-service-name").unwrap_or_default();
    s.sni = text(item, "servername").or_else(|| text(item, "sni")).unwrap_or_else(|| s.host.clone());
    s.alpn = list(item, "alpn");
    s.fingerprint = text(item, "client-fingerprint").unwrap_or_default();
    s.allow_insecure = flag(item, "skip-cert-verify");
    s.public_key = nested(item, &["reality-opts"], "public-key").unwrap_or_default();
    s.short_id = nested(item, &["reality-opts"], "short-id").unwrap_or_default();
    // reality-opts 存在 ⇒ REALITY；否则看 tls 开关。两者都不是 ⇒ 明文。
    s.security = if !s.public_key.is_empty() || item.get("reality-opts").is_some() {
        "reality".into()
    } else if flag(item, "tls") {
        "tls".into()
    } else {
        "none".into()
    };

    let settings = match kind.as_str() {
        "ss" => {
            let method = text(item, "cipher").ok_or("ss 缺少 cipher")?;
            let password = text(item, "password").ok_or("ss 缺少 password")?;
            json!({
                "servers": [{
                    "address": address, "port": port,
                    "method": method.to_ascii_lowercase(), "password": password,
                    "uot": flag(item, "udp-over-tcp"), "level": 0
                }]
            })
        }
        "vmess" => {
            let uuid = text(item, "uuid").ok_or("vmess 缺少 uuid")?;
            let alter_id = number(item, "alterId").unwrap_or(0);
            let security = match text(item, "cipher").unwrap_or_default().to_ascii_lowercase().as_str() {
                "none" => "none",
                "zero" => "zero",
                "aes-128-gcm" => "aes-128-gcm",
                "chacha20-poly1305" => "chacha20-poly1305",
                _ => "auto",
            };
            json!({
                "vnext": [{
                    "address": address, "port": port,
                    "users": [{ "id": uuid, "alterId": alter_id, "security": security, "level": 0 }]
                }]
            })
        }
        "vless" => {
            let uuid = text(item, "uuid").ok_or("vless 缺少 uuid")?;
            let flow = text(item, "flow").unwrap_or_default();
            let mut account = json!({
                "id": uuid,
                // encryption 不能写死 none：服务端启用后量子加密时会被直接拒绝。
                "encryption": text(item, "encryption").unwrap_or_else(|| "none".into()),
                "level": 0
            });
            if !flow.is_empty() {
                account["flow"] = json!(flow);
            }
            json!({ "vnext": [{ "address": address, "port": port, "users": [account] }] })
        }
        "trojan" => {
            let password = text(item, "password").ok_or("trojan 缺少 password")?;
            json!({ "servers": [{ "address": address, "port": port, "password": password, "level": 0 }] })
        }
        "socks5" | "socks" => {
            let mut server = json!({ "address": address, "port": port, "level": 0 });
            let username = text(item, "username").unwrap_or_default();
            if !username.is_empty() {
                server["users"] = json!([{ "user": username, "pass": text(item, "password").unwrap_or_default(), "level": 0 }]);
            }
            json!({ "servers": [server] })
        }
        "http" => {
            let mut server = json!({ "address": address, "port": port, "level": 0 });
            let username = text(item, "username").unwrap_or_default();
            if !username.is_empty() {
                server["users"] = json!([{ "user": username, "pass": text(item, "password").unwrap_or_default(), "level": 0 }]);
            }
            json!({ "servers": [server] })
        }
        other => {
            return Err(format!(
                "不支持的 type: {other}（支持 ss/vmess/vless/trojan/socks5/http）"
            ))
        }
    };

    let protocol = match kind.as_str() {
        "ss" => "shadowsocks",
        "socks5" | "socks" => "socks",
        other => other,
    };
    let stream_json = stream::render(&s, &format!("proxies 中的 {}:{}", address, port))
        .map_err(|err| err.message)?;
    Ok(assemble(protocol, &address, port, name, settings, stream_json))
}

// ---------------------------------------------------------------------------
// YAML 取值（缺字段一律返回 None，由调用方决定是「可选」还是「必需」）
// ---------------------------------------------------------------------------

fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

fn nested(value: &Value, path: &[&str], key: &str) -> Option<String> {
    let mut current = value;
    for step in path {
        current = get(current, step)?;
    }
    text(current, key)
}

fn text(value: &Value, key: &str) -> Option<String> {
    as_text(get(value, key)?)
}

fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn number(value: &Value, key: &str) -> Option<u32> {
    match get(value, key)? {
        Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(text) => text.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn flag(value: &Value, key: &str) -> bool {
    match get(value, key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => matches!(text.to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Some(Value::Number(number)) => number.as_u64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn list(value: &Value, key: &str) -> Vec<String> {
    match get(value, key) {
        Some(Value::Sequence(items)) => items.iter().filter_map(as_text).collect(),
        Some(Value::String(text)) => text.split(',').map(|part| part.trim().to_string()).filter(|part| !part.is_empty()).collect(),
        _ => Vec::new(),
    }
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
  - name: "直连 HTTP"
    type: trojan
    server: t.example.com
    port: 443
    password: pw
    tls: true
proxy-groups:
  - name: auto
    type: url-test
    proxies: ["香港 01"]
"#;

    #[test]
    fn four_proxies_parse_with_expected_fields() {
        let nodes = parse(SAMPLE).expect("解析");
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].protocol, "shadowsocks");
        assert_eq!(nodes[0].outbound["settings"]["servers"][0]["method"], json!("chacha20-ietf-poly1305"));
        assert_eq!(nodes[1].outbound["streamSettings"]["wsSettings"]["path"], json!("/ray"));
        assert_eq!(nodes[1].outbound["streamSettings"]["security"], json!("tls"));
        assert_eq!(nodes[2].outbound["streamSettings"]["security"], json!("reality"));
        assert_eq!(nodes[2].outbound["streamSettings"]["realitySettings"]["publicKey"], json!("PUBKEY"));
        assert_eq!(nodes[3].outbound["settings"]["servers"][0]["password"], json!("pw"));
    }

    /// 坏条目导致整份失败，错误带索引与原始片段 —— 不静默少几个节点。
    #[test]
    fn unsupported_type_fails_the_whole_parse_with_index_and_raw() {
        let yaml = "proxies:\n  - {name: ok, type: ss, server: a.com, port: 443, cipher: aes-256-gcm, password: p}\n  - {name: bad, type: hysteria2, server: b.com, port: 443}\n";
        let err = parse(yaml).expect_err("必须报错");
        let detail = err.detail.as_ref().expect("带 detail");
        assert_eq!(detail["index"], json!(2));
        assert!(detail["raw"].as_str().is_some_and(|raw| raw.contains("hysteria2")));
        assert!(err.message.contains("hysteria2"), "{err:?}");
    }

    #[test]
    fn missing_proxies_key_is_an_error() {
        assert!(parse("rules:\n  - MATCH,DIRECT\n").is_err());
    }

    #[test]
    fn vless_encryption_is_not_hardcoded() {
        let yaml = "proxies:\n  - {name: pq, type: vless, server: 1.2.3.4, port: 443, uuid: u, encryption: mlkem768x25519plus.native.0rtt.KEY}\n";
        let nodes = parse(yaml).expect("解析");
        assert_eq!(nodes[0].outbound["settings"]["vnext"][0]["users"][0]["encryption"], json!("mlkem768x25519plus.native.0rtt.KEY"));
    }
}
