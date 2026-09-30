//! 传输层 / TLS 的 JSON 渲染（`streamSettings`）。
//!
//! 单独一个模块的理由：三种订阅格式（分享链接 / Clash YAML / Xray JSON）
//! 解析出来的传输字段是同一组，渲染成 Xray JSON 的字段名必须**只有一个来源**
//! —— 字段名写错不会报错，只会在连接时静默失败（或者被核心判为非法配置）。
//!
//! 支持范围就是这里列出的 8 种 network 与 3 种 security；遇到别的值**报错**，
//! 不做「当作 tcp 处理」这种猜测。

use serde_json::{json, Map, Value};
use xt_contract::error::{bad_request, ErrorBody};

use crate::snippet;

#[derive(Clone, Debug, Default)]
pub(crate) struct Stream {
    /// Xray 的 `streamSettings.network` 取值：tcp/ws/grpc/http/httpupgrade/xhttp/kcp/quic。
    pub network: String,
    pub host: String,
    pub path: String,
    pub service_name: String,
    pub mode: String,
    pub header_type: String,
    pub quic_key: String,
    pub quic_security: String,
    /// `none` / `tls` / `reality`。
    pub security: String,
    pub sni: String,
    pub alpn: Vec<String>,
    pub fingerprint: String,
    pub allow_insecure: bool,
    pub public_key: String,
    pub short_id: String,
    pub spider_x: String,
}

impl Stream {
    pub(crate) fn with_network(network: &str) -> Self {
        Self {
            network: normalize_network(network),
            path: "/".into(),
            security: "none".into(),
            spider_x: "/".into(),
            ..Default::default()
        }
    }
}

/// 从解析器给出的 network 名字规范化成 Xray 认的取值。
/// `h2` 是分享链接里的常见别名，Xray 只认 `http`。
fn normalize_network(network: &str) -> String {
    match network.to_ascii_lowercase().as_str() {
        "h2" => "http".into(),
        other => other.into(),
    }
}

/// 构造一份 streamSettings。`raw` 用于错误里带原始片段（知道是哪一条坏了）。
pub(crate) fn render(stream: &Stream, raw: &str) -> Result<Value, ErrorBody> {
    let network = normalize_network(&stream.network);
    let security = match stream.security.to_ascii_lowercase().as_str() {
        "" => "none".into(),
        "xtls" => "tls".into(),
        other => other.to_string(),
    };

    let mut out = Map::new();
    out.insert("network".into(), Value::String(network.clone()));
    out.insert("security".into(), Value::String(security.clone()));

    match security.as_str() {
        "none" => {}
        "tls" => {
            let mut tls = Map::new();
            tls.insert("serverName".into(), Value::String(server_name(stream)));
            tls.insert("allowInsecure".into(), Value::Bool(stream.allow_insecure));
            if !stream.alpn.is_empty() {
                tls.insert("alpn".into(), json!(stream.alpn));
            }
            if !stream.fingerprint.is_empty() {
                // uTLS 指纹是抗主动探测的手段，字段可选 ≠ 可以丢。
                tls.insert("fingerprint".into(), Value::String(stream.fingerprint.clone()));
            }
            out.insert("tlsSettings".into(), Value::Object(tls));
        }
        "reality" => {
            if stream.public_key.is_empty() {
                return Err(bad_request(format!(
                    "REALITY 缺少公钥（pbk / public-key）：{}",
                    snippet(raw, 120)
                )));
            }
            out.insert(
                "realitySettings".into(),
                json!({
                    "serverName": server_name(stream),
                    "fingerprint": if stream.fingerprint.is_empty() { "chrome".to_string() } else { stream.fingerprint.clone() },
                    "publicKey": stream.public_key,
                    "shortId": stream.short_id,
                    "spiderX": if stream.spider_x.is_empty() { "/".to_string() } else { stream.spider_x.clone() },
                    "show": false
                }),
            );
        }
        other => {
            return Err(bad_request(format!(
                "不支持的 security: {other}（支持 none/tls/reality）：{}",
                snippet(raw, 120)
            )))
        }
    }

    match network.as_str() {
        "tcp" => {}
        "ws" => {
            let mut ws = Map::new();
            ws.insert("path".into(), Value::String(if stream.path.is_empty() { "/".into() } else { stream.path.clone() }));
            if !stream.host.is_empty() {
                // Host 头伪装：留空时让 Xray 用 SNI，不写空头。
                ws.insert("headers".into(), json!({ "Host": stream.host }));
            }
            out.insert("wsSettings".into(), Value::Object(ws));
        }
        "grpc" => {
            out.insert("grpcSettings".into(), json!({ "serviceName": stream.service_name }));
        }
        "httpupgrade" => {
            out.insert("httpupgradeSettings".into(), json!({ "path": stream.path, "host": stream.host }));
        }
        "xhttp" => {
            out.insert(
                "xhttpSettings".into(),
                json!({ "path": stream.path, "host": stream.host, "mode": if stream.mode.is_empty() { "auto" } else { &stream.mode } }),
            );
        }
        "quic" => {
            out.insert("quicSettings".into(), json!({ "key": stream.quic_key, "security": stream.quic_security }));
        }
        "kcp" => {
            out.insert(
                "kcpSettings".into(),
                json!({ "header": { "type": if stream.header_type.is_empty() { "none" } else { &stream.header_type } } }),
            );
        }
        "http" => {
            let hosts = if stream.host.is_empty() { Vec::new() } else { vec![stream.host.clone()] };
            out.insert("httpSettings".into(), json!({ "host": hosts, "path": stream.path }));
        }
        other => {
            return Err(bad_request(format!(
                "不支持的传输 network: {other}（支持 tcp/ws/grpc/http/httpupgrade/xhttp/kcp/quic）：{}",
                snippet(raw, 120)
            )))
        }
    }

    Ok(Value::Object(out))
}

/// SNI 缺省时用主机名：绝大多数服务端就是这么配的；留空会让核心发一个空 SNI。
fn server_name(stream: &Stream) -> String {
    if stream.sni.is_empty() {
        stream.host.clone()
    } else {
        stream.sni.clone()
    }
}
