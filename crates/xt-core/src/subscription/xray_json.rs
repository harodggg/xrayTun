//! Xray / V2Ray JSON 配置解析：把 `outbounds` 里的服务器定义提取成节点。
//!
//! 用途：用户从面板（3x-ui、X-UI、Marzban 等）拿到的是完整配置 JSON，
//! 直接导入比手动复制链接方便得多。
//!
//! 只提取「服务器型」outbound（vmess/vless/trojan/shadowsocks/socks/http），
//! `freedom` / `blackhole` / `dns` 这类功能性 outbound 会被忽略。

use serde_json::Value;

use super::{ParseOutcome, SubscriptionFormat};
use crate::error::{Error, Result};
use crate::model::{
    MuxSettings, Node, NodeSource, Protocol, RealitySettings, TlsSettings, Transport, VmessSecurity,
};

pub fn parse(body: &str) -> Result<ParseOutcome> {
    let doc: Value = serde_json::from_str(body).map_err(Error::Json)?;
    let mut outcome = ParseOutcome::new(SubscriptionFormat::XrayJson);
    let Some(outbounds) = doc.get("outbounds").and_then(|v| v.as_array()) else {
        return Err(Error::InvalidConfig("配置里没有 outbounds 数组".into()));
    };

    for (idx, ob) in outbounds.iter().enumerate() {
        match convert(ob) {
            Ok(Some(node)) => outcome.nodes.push(node),
            Ok(None) => {} // 功能性 outbound，正常跳过
            Err(e) => outcome.warnings.push(format!("outbounds[{idx}] 已跳过: {e}")),
        }
    }

    if outcome.nodes.is_empty() {
        return Err(Error::InvalidConfig("outbounds 里没有可导入的服务器".into()));
    }
    Ok(outcome)
}

fn convert(ob: &Value) -> Result<Option<Node>> {
    let protocol = ob.get("protocol").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    let tag = ob.get("tag").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let stream = ob.get("streamSettings");

    let (address, port, proto) = match protocol.as_str() {
        "vmess" | "vless" => {
            let vnext = ob
                .pointer("/settings/vnext")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .ok_or(Error::MissingField("settings.vnext[0]"))?;
            let address = vnext.get("address").and_then(|v| v.as_str()).ok_or(Error::MissingField("address"))?;
            let port = vnext.get("port").and_then(json_u16).ok_or(Error::MissingField("port"))?;
            let user = vnext
                .get("users")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .ok_or(Error::MissingField("users[0]"))?;
            let uuid = user.get("id").and_then(|v| v.as_str()).ok_or(Error::MissingField("id"))?;
            let proto = if protocol == "vmess" {
                Protocol::Vmess {
                    uuid: uuid.to_string(),
                    alter_id: user.get("alterId").and_then(json_u32).unwrap_or(0),
                    security: match user.get("security").and_then(|v| v.as_str()).unwrap_or("auto") {
                        "none" => VmessSecurity::None,
                        "zero" => VmessSecurity::Zero,
                        "aes-128-gcm" => VmessSecurity::Aes128Gcm,
                        "chacha20-poly1305" => VmessSecurity::Chacha20Poly1305,
                        _ => VmessSecurity::Auto,
                    },
                }
            } else {
                Protocol::Vless {
                    uuid: uuid.to_string(),
                    flow: user.get("flow").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    encryption: user
                        .get("encryption")
                        .and_then(|v| v.as_str())
                        .unwrap_or("none")
                        .to_string(),
                }
            };
            (address.to_string(), port, proto)
        }
        "trojan" => {
            let server = first_server(ob)?;
            let password = server
                .get("password")
                .and_then(|v| v.as_str())
                .ok_or(Error::MissingField("password"))?;
            (
                server.get("address").and_then(|v| v.as_str()).ok_or(Error::MissingField("address"))?.to_string(),
                server.get("port").and_then(json_u16).ok_or(Error::MissingField("port"))?,
                Protocol::Trojan { password: password.to_string() },
            )
        }
        "shadowsocks" => {
            let server = first_server(ob)?;
            (
                server.get("address").and_then(|v| v.as_str()).ok_or(Error::MissingField("address"))?.to_string(),
                server.get("port").and_then(json_u16).ok_or(Error::MissingField("port"))?,
                Protocol::Shadowsocks {
                    method: server
                        .get("method")
                        .and_then(|v| v.as_str())
                        .ok_or(Error::MissingField("method"))?
                        .to_string(),
                    password: server
                        .get("password")
                        .and_then(|v| v.as_str())
                        .ok_or(Error::MissingField("password"))?
                        .to_string(),
                    uot: server.get("uot").and_then(|v| v.as_bool()).unwrap_or(false),
                },
            )
        }
        "socks" | "http" => {
            let server = first_server(ob)?;
            let user = server.get("users").and_then(|v| v.as_array()).and_then(|a| a.first());
            let username = user.and_then(|u| u.get("user")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let password = user.and_then(|u| u.get("pass")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let proto = if protocol == "socks" {
                Protocol::Socks { username, password }
            } else {
                Protocol::Http { username, password }
            };
            (
                server.get("address").and_then(|v| v.as_str()).ok_or(Error::MissingField("address"))?.to_string(),
                server.get("port").and_then(json_u16).ok_or(Error::MissingField("port"))?,
                proto,
            )
        }
        // freedom / blackhole / dns / wireguard 等不导入
        _ => return Ok(None),
    };

    let transport = stream.map(build_transport).unwrap_or_default();
    let tls = stream.map(build_tls).unwrap_or_default();
    let mux = ob.get("mux").and_then(|m| {
        m.get("enabled").and_then(|v| v.as_bool()).filter(|b| *b).map(|_| MuxSettings {
            enabled: true,
            concurrency: m.get("concurrency").and_then(json_u32).unwrap_or(8),
            xudp_concurrency: 0,
        })
    });

    let name = if tag.is_empty() { format!("{address}:{port}") } else { tag };

    let mut node = Node {
        id: String::new(),
        name,
        address,
        port,
        protocol: proto,
        transport,
        tls,
        mux,
        source: NodeSource::Manual,
        tags: vec![],
        raw_uri: None,
    };
    node.refresh_id();
    Ok(Some(node))
}

fn first_server(ob: &Value) -> Result<&Value> {
    ob.pointer("/settings/servers")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or(Error::MissingField("settings.servers[0]"))
}

fn json_u16(v: &Value) -> Option<u16> {
    match v {
        Value::Number(n) => n.as_u64().map(|x| x as u16),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn json_u32(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().map(|x| x as u32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn build_transport(stream: &Value) -> Transport {
    let network = stream.get("network").and_then(|v| v.as_str()).unwrap_or("tcp").to_ascii_lowercase();
    match network.as_str() {
        "ws" => Transport::WebSocket {
            path: stream
                .pointer("/wsSettings/path")
                .and_then(|v| v.as_str())
                .unwrap_or("/")
                .to_string(),
            host: stream
                .pointer("/wsSettings/headers/Host")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "grpc" => Transport::Grpc {
            service_name: stream
                .pointer("/grpcSettings/serviceName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            multi_mode: stream
                .pointer("/grpcSettings/multiMode")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: stream
                .pointer("/httpupgradeSettings/path")
                .and_then(|v| v.as_str())
                .unwrap_or("/")
                .to_string(),
            host: stream
                .pointer("/httpupgradeSettings/host")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: stream
                .pointer("/xhttpSettings/path")
                .or_else(|| stream.pointer("/splithttpSettings/path"))
                .and_then(|v| v.as_str())
                .unwrap_or("/")
                .to_string(),
            host: stream
                .pointer("/xhttpSettings/host")
                .or_else(|| stream.pointer("/splithttpSettings/host"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            mode: stream
                .pointer("/xhttpSettings/mode")
                .and_then(|v| v.as_str())
                .unwrap_or("auto")
                .to_string(),
        },
        "quic" => Transport::Quic {
            key: stream.pointer("/quicSettings/key").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            security: stream
                .pointer("/quicSettings/security")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "kcp" | "mkcp" => Transport::Kcp {
            header_type: stream
                .pointer("/kcpSettings/header/type")
                .and_then(|v| v.as_str())
                .unwrap_or("none")
                .to_string(),
            seed: stream.pointer("/kcpSettings/seed").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        "http" | "h2" => Transport::Http {
            host: stream.pointer("/httpSettings/host").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            path: stream
                .pointer("/httpSettings/path")
                .and_then(|v| v.as_str())
                .unwrap_or("/")
                .to_string(),
        },
        _ => Transport::Tcp,
    }
}

fn build_tls(stream: &Value) -> TlsSettings {
    let security = stream.get("security").and_then(|v| v.as_str()).unwrap_or("none");
    if security == "none" {
        return TlsSettings::default();
    }
    let section = if security == "reality" { "/realitySettings" } else { "/tlsSettings" };
    let p = |k: &str| stream.pointer(&format!("{section}{k}")).cloned();

    let alpn = p("/alpn")
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_else(Vec::new);

    TlsSettings {
        enabled: true,
        server_name: p("/serverName").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(),
        allow_insecure: stream
            .pointer("/tlsSettings/allowInsecure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        alpn,
        fingerprint: p("/fingerprint").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(),
        reality: if security == "reality" {
            Some(RealitySettings {
                public_key: p("/publicKey").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(),
                short_id: p("/shortId").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(),
                spider_x: p("/spiderX").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_else(|| "/".into()),
            })
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "outbounds": [
        {
          "tag": "proxy",
          "protocol": "vless",
          "settings": {
            "vnext": [{
              "address": "jp.example.com",
              "port": 443,
              "users": [{"id": "uuid-1", "encryption": "none", "flow": "xtls-rprx-vision"}]
            }]
          },
          "streamSettings": {
            "network": "tcp",
            "security": "reality",
            "realitySettings": {
              "serverName": "www.apple.com",
              "publicKey": "PBK",
              "shortId": "ab",
              "fingerprint": "chrome"
            }
          }
        },
        { "tag": "direct", "protocol": "freedom" },
        {
          "tag": "ss-node",
          "protocol": "shadowsocks",
          "settings": {
            "servers": [{"address": "s.example.com", "port": 8388, "method": "aes-256-gcm", "password": "pw"}]
          }
        }
      ]
    }"#;

    #[test]
    fn imports_server_outbounds_and_skips_freedom() {
        let out = parse(SAMPLE).unwrap();
        assert_eq!(out.nodes.len(), 2);
        assert!(out.nodes.iter().any(|n| n.name == "proxy"));
        assert!(out.nodes.iter().any(|n| n.name == "ss-node"));
    }

    #[test]
    fn reality_settings_map_correctly() {
        let out = parse(SAMPLE).unwrap();
        let n = out.nodes.iter().find(|n| n.name == "proxy").unwrap();
        assert_eq!(n.tls.effective_security(), "reality");
        let r = n.tls.reality.as_ref().unwrap();
        assert_eq!(r.public_key, "PBK");
        assert_eq!(r.short_id, "ab");
        assert_eq!(n.tls.fingerprint, "chrome");
        match &n.protocol {
            Protocol::Vless { uuid, flow, .. } => {
                assert_eq!(uuid, "uuid-1");
                assert_eq!(flow, "xtls-rprx-vision");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_outbounds_is_error() {
        assert!(parse(r#"{"outbounds":[{"protocol":"freedom"}]}"#).is_err());
    }
}
