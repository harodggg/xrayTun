//! 分享链接解析：`vmess://` / `ss://` / `vless://` / `trojan://`。
//!
//! 现实世界里同一个协议有四五种写法（vmess 的 `port` 有时是字符串；
//! `ss://` 有 SIP002 与整体 base64 两种布局；查询参数大小写混用；
//! 名称放在 fragment 里需要百分号解码），所以这里刻意**不用强类型结构体**：
//! 一个可选字段的类型抖动不该让整个节点解析失败。
//!
//! 「容错」的边界：**写法多样性**容忍，**缺失必需字段**不容忍 ——
//! 缺 uuid/密码/端口一律报错并带行号与原文，不产出半个节点。

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use percent_encoding::percent_decode_str;
use serde_json::{json, Value};
use url::Url;

use xt_contract::error::ErrorBody;

use crate::stream::{self, Stream};
use crate::{assemble, entry_error, snippet, ParsedNode, Where};

/// 宽容的 base64 解码：依次试标准/无填充/URL-safe 变体。
/// 机场生成的订阅经常把两种字母表混用，严格解码会大面积失败。
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("空字符串".into());
    }
    for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&cleaned) {
            return Ok(bytes);
        }
    }
    Err(format!("四种字母表都无法解码（前缀: {}）", snippet(&cleaned, 32)))
}

/// 判断正文是不是「每行一个分享链接」的列表。
pub(crate) fn looks_like_link_list(body: &str) -> bool {
    const SCHEMES: [&str; 5] = ["vmess://", "vless://", "trojan://", "ss://", "ssr://"];
    body.lines().any(|line| {
        let line = line.trim();
        SCHEMES.iter().any(|scheme| {
            // 必须用 `str::get`：按字节切片落在多字节字符中间会 panic，
            // 而「用户粘了一句中文」太容易触发了。
            line.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        })
    })
}

/// 解析一行分享链接。`line_no` 是**原文里的行号（从 1 开始）**，错误必须带上它。
pub(crate) fn parse_share_link(line: &str, line_no: usize) -> Result<ParsedNode, ErrorBody> {
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("vmess://") {
        parse_vmess(line, line_no)
    } else if lower.starts_with("ss://") {
        parse_ss(line, line_no)
    } else if lower.starts_with("ssr://") {
        // SSR 不在 Xray 原生支持范围。明确报错，而不是让用户以为它被支持。
        Err(entry_error(Where::LineRaw(line_no, &snippet(line, 120)), "ShadowsocksR 不被 Xray 支持，请改用 ss/vless/trojan"))
    } else if lower.starts_with("vless://") || lower.starts_with("trojan://") {
        parse_standard(line, line_no)
    } else {
        Err(entry_error(
            Where::LineRaw(line_no, &snippet(line, 120)),
            "不认识的分享链接（支持 vmess://、ss://、vless://、trojan://）",
        ))
    }
}

fn parse_vmess(line: &str, line_no: usize) -> Result<ParsedNode, ErrorBody> {
    let bad = |reason: String| entry_error(Where::LineRaw(line_no, &snippet(line, 120)), reason);

    let payload = &line["vmess://".len()..];
    let (payload, fragment) = split_fragment(payload);
    let payload = payload.split('?').next().unwrap_or(payload);

    let decoded = decode_base64(payload).map_err(bad)?;
    let text = String::from_utf8(decoded).map_err(|err| bad(format!("vmess 负载不是 UTF-8: {err}")))?;
    let value: Value = serde_json::from_str(text.trim()).map_err(|err| bad(format!("vmess 负载不是合法 JSON: {err}")))?;

    let address = json_str(&value, &["add", "address"]).ok_or_else(|| bad("缺少 add（服务器地址）".into()))?;
    let port_raw = json_u32(&value, &["port"]).ok_or_else(|| bad("缺少 port".into()))?;
    let port = u16::try_from(port_raw).map_err(|_| bad(format!("port 超出范围: {port_raw}")))?;
    let uuid = json_str(&value, &["id", "uuid"]).ok_or_else(|| bad("缺少 id（uuid）".into()))?;
    let alter_id = json_u32(&value, &["aid", "alterId"]).unwrap_or(0);
    let security = match json_str(&value, &["scy", "security"]).unwrap_or_default().to_ascii_lowercase().as_str() {
        "none" => "none",
        "zero" => "zero",
        "aes-128-gcm" => "aes-128-gcm",
        "chacha20-poly1305" => "chacha20-poly1305",
        // 空值/未知一律 `auto`：这是 vmess 的协商默认值，不是我们编的假值。
        _ => "auto",
    };

    let mut s = Stream::with_network(&json_str(&value, &["net", "network"]).unwrap_or_else(|| "tcp".into()));
    s.host = json_str(&value, &["host"]).unwrap_or_default();
    s.path = json_str(&value, &["path"]).unwrap_or_else(|| "/".into());
    s.header_type = json_str(&value, &["type", "headerType"]).unwrap_or_default();
    s.sni = json_str(&value, &["sni", "peer", "host"]).unwrap_or_default();
    s.alpn = split_list(&json_str(&value, &["alpn"]).unwrap_or_default());
    s.fingerprint = json_str(&value, &["fp"]).unwrap_or_default();
    s.security = match json_str(&value, &["tls"]).unwrap_or_default().to_ascii_lowercase().as_str() {
        "tls" | "1" | "true" => "tls".into(),
        _ => "none".into(),
    };
    let stream_json = stream::render(&s, line)?;

    let name = fragment
        .filter(|n| !n.trim().is_empty())
        .or_else(|| json_str(&value, &["ps", "remarks"]))
        .unwrap_or_default();

    let settings = json!({
        "vnext": [{
            "address": address,
            "port": port,
            "users": [{ "id": uuid, "alterId": alter_id, "security": security, "level": 0 }]
        }]
    });
    Ok(assemble("vmess", &address, port, name, settings, stream_json))
}

fn parse_ss(line: &str, line_no: usize) -> Result<ParsedNode, ErrorBody> {
    let bad = |reason: String| entry_error(Where::LineRaw(line_no, &snippet(line, 120)), reason);

    let rest = &line["ss://".len()..];
    let (rest, fragment) = split_fragment(rest);
    // SIP003 插件（`?plugin=...`）Xray 客户端不支持；带上去只会得到一个连不上的节点。
    let rest = rest.split('?').next().unwrap_or(rest);

    let (userinfo_raw, hostport) = match rest.rsplit_once('@') {
        Some((userinfo, hostport)) => (userinfo.to_string(), hostport.to_string()),
        None => {
            // 旧版布局：整体 base64，内部是 `method:password@host:port`。
            let decoded = decode_base64(rest).map_err(|reason| bad(format!("ss 旧版布局不是合法 base64: {reason}")))?;
            let text = String::from_utf8(decoded).map_err(|err| bad(format!("ss 负载不是 UTF-8: {err}")))?;
            let (userinfo, hostport) = text
                .rsplit_once('@')
                .ok_or_else(|| bad("ss 旧版布局缺少 '@' 分隔符".into()))?;
            (userinfo.to_string(), hostport.to_string())
        }
    };

    let userinfo = percent_decode(&userinfo_raw);
    // SIP002 规定 userinfo 是 base64url(method:password)，但也有人直接写明文的
    // `method:password`，两种都得认。
    let userinfo = if userinfo.contains(':') {
        userinfo
    } else {
        let decoded = decode_base64(&userinfo).map_err(|reason| bad(format!("ss 凭据不是合法 base64: {reason}")))?;
        String::from_utf8(decoded).map_err(|err| bad(format!("ss 凭据不是 UTF-8: {err}")))?
    };
    let (method, password) = userinfo
        .split_once(':')
        .ok_or_else(|| bad("ss 凭据缺少 ':'（应为 method:password）".into()))?;

    let (address, port) = split_host_port(&hostport).map_err(bad)?;
    let name = fragment.unwrap_or_default();

    let settings = json!({
        "servers": [{
            "address": address,
            "port": port,
            "method": method.trim().to_ascii_lowercase(),
            "password": password,
            "uot": false,
            "level": 0
        }]
    });
    let stream_json = stream::render(&Stream::with_network("tcp"), line)?;
    Ok(assemble("shadowsocks", &address, port, name, settings, stream_json))
}

fn parse_standard(line: &str, line_no: usize) -> Result<ParsedNode, ErrorBody> {
    let bad = |reason: String| entry_error(Where::LineRaw(line_no, &snippet(line, 120)), reason);

    let url = Url::parse(line).map_err(|err| bad(format!("链接不是合法 URL: {err}")))?;
    let scheme = url.scheme().to_ascii_lowercase();
    let address = url
        .host_str()
        .ok_or_else(|| bad("缺少主机名".into()))?
        // `url` 对 IPv6 会带方括号返回（`[::1]`），而 NodeId 派生与 Clash 路径
        // 都用裸主机名：这里统一去括号，避免同一个节点在两种格式下得到两个 id。
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = url.port().ok_or_else(|| bad("缺少端口".into()))?;
    let query = query_map(&url);
    let user = percent_decode(url.username());

    let mut s = Stream::with_network(qget(&query, &["type", "net"]).unwrap_or("tcp"));
    s.host = qget(&query, &["host"]).unwrap_or("").to_string();
    s.path = qget(&query, &["path"]).unwrap_or("/").to_string();
    s.service_name = qget(&query, &["servicename"]).unwrap_or("").to_string();
    s.mode = qget(&query, &["mode"]).unwrap_or("").to_string();
    s.header_type = qget(&query, &["headertype"]).unwrap_or("").to_string();
    s.quic_key = qget(&query, &["key"]).unwrap_or("").to_string();
    s.quic_security = qget(&query, &["quicsecurity"]).unwrap_or("").to_string();
    s.security = match qget(&query, &["security"]) {
        Some(value) => value.to_ascii_lowercase(),
        None if truthy(qget(&query, &["tls"])) => "tls".into(),
        None => "none".into(),
    };
    s.sni = qget(&query, &["sni", "peer", "host"]).unwrap_or("").to_string();
    s.alpn = split_list(qget(&query, &["alpn"]).unwrap_or(""));
    s.fingerprint = qget(&query, &["fp"]).unwrap_or("").to_string();
    s.allow_insecure = truthy(qget(&query, &["allowinsecure", "insecure"]));
    s.public_key = qget(&query, &["pbk"]).unwrap_or("").to_string();
    s.short_id = qget(&query, &["sid"]).unwrap_or("").to_string();
    s.spider_x = qget(&query, &["spx"]).unwrap_or("/").to_string();

    let (protocol, settings) = match scheme.as_str() {
        "vless" => {
            if user.is_empty() {
                return Err(bad("vless 缺少 uuid".into()));
            }
            let mut account = json!({
                "id": user,
                // encryption 必须读出来：Xray 25.x 起服务端可能启用后量子加密，
                // 写死 "none" 会让那些节点全部握手失败。
                "encryption": qget(&query, &["encryption"]).unwrap_or("none"),
                "level": 0
            });
            let flow = qget(&query, &["flow"]).unwrap_or("");
            if !flow.is_empty() {
                account["flow"] = Value::String(flow.to_string());
            }
            (
                "vless",
                json!({ "vnext": [{ "address": address, "port": port, "users": [account] }] }),
            )
        }
        "trojan" => {
            if user.is_empty() {
                return Err(bad("trojan 缺少密码".into()));
            }
            (
                "trojan",
                json!({ "servers": [{ "address": address, "port": port, "password": user, "level": 0 }] }),
            )
        }
        other => return Err(bad(format!("不支持的 scheme: {other}（支持 vless/trojan）"))),
    };

    let stream_json = stream::render(&s, line)?;
    let name = url.fragment().map(percent_decode).unwrap_or_default();
    Ok(assemble(protocol, &address, port, name, settings, stream_json))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn percent_decode(text: &str) -> String {
    percent_decode_str(text).decode_utf8_lossy().into_owned()
}

fn split_fragment(text: &str) -> (&str, Option<String>) {
    match text.split_once('#') {
        Some((body, fragment)) => (body, Some(percent_decode(fragment))),
        None => (text, None),
    }
}

/// 拆 `host:port`，支持 `[::1]:443`。
fn split_host_port(text: &str) -> Result<(String, u16), String> {
    let text = text.trim().trim_end_matches('/');
    if let Some(rest) = text.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(|| "IPv6 地址缺少 ']'".to_string())?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| "缺少端口".to_string())?;
        return Ok((host.to_string(), port));
    }
    let (host, port) = text.rsplit_once(':').ok_or_else(|| "缺少 host:port".to_string())?;
    let port = port.trim().parse::<u16>().map_err(|err| format!("端口非法: {err}"))?;
    if host.is_empty() {
        return Err("缺少主机名".into());
    }
    Ok((host.to_string(), port))
}

fn query_map(url: &Url) -> std::collections::HashMap<String, String> {
    url.query_pairs().map(|(k, v)| (k.to_ascii_lowercase(), v.into_owned())).collect()
}

fn qget<'a>(query: &'a std::collections::HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| query.get(*key).map(String::as_str))
        .filter(|value| !value.is_empty())
}

fn truthy(value: Option<&str>) -> bool {
    matches!(value.map(|v| v.to_ascii_lowercase()).as_deref(), Some("1" | "true" | "yes"))
}

fn json_str(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        match object.get(*key) {
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            Some(Value::Number(number)) => return Some(number.to_string()),
            Some(Value::Bool(flag)) => return Some(flag.to_string()),
            _ => {}
        }
    }
    None
}

fn json_u32(value: &Value, keys: &[&str]) -> Option<u32> {
    let object = value.as_object()?;
    for key in keys {
        match object.get(*key) {
            Some(Value::Number(number)) => {
                if let Some(parsed) = number.as_u64() {
                    return u32::try_from(parsed).ok();
                }
            }
            Some(Value::String(text)) => {
                if let Ok(parsed) = text.trim().parse::<u32>() {
                    return Some(parsed);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',').map(|part| part.trim().to_string()).filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vless_reality_grpc_keeps_every_parameter() {
        let line = "vless://uuid-1@h.example.com:443?encryption=none&security=reality&sni=www.apple.com&fp=chrome&pbk=PUBKEY&sid=abcd&spx=%2F&type=grpc&serviceName=mysvc#R1";
        let node = parse_share_link(line, 1).expect("解析");
        assert_eq!(node.protocol, "vless");
        assert_eq!(node.endpoint, "h.example.com:443");
        assert_eq!(node.name, "R1");
        let outbound = &node.outbound;
        assert_eq!(outbound["streamSettings"]["network"], json!("grpc"));
        assert_eq!(outbound["streamSettings"]["security"], json!("reality"));
        assert_eq!(outbound["streamSettings"]["grpcSettings"]["serviceName"], json!("mysvc"));
        assert_eq!(outbound["streamSettings"]["realitySettings"]["publicKey"], json!("PUBKEY"));
        assert_eq!(outbound["streamSettings"]["realitySettings"]["shortId"], json!("abcd"));
        assert_eq!(outbound["settings"]["vnext"][0]["users"][0]["encryption"], json!("none"));
    }

    #[test]
    fn vmess_json_accepts_string_port_and_names_the_fragment() {
        let payload = json!({
            "v": "2", "ps": "节点A", "add": "a.example.com", "port": "443",
            "id": "11111111-1111-1111-1111-111111111111", "aid": 0, "scy": "auto",
            "net": "ws", "path": "/ray", "host": "a.example.com", "tls": "tls", "sni": "a.example.com"
        });
        let line = format!("vmess://{}#ignored", STANDARD.encode(payload.to_string()));
        let node = parse_share_link(&line, 7).expect("解析");
        assert_eq!(node.name, "ignored");
        assert_eq!(node.endpoint, "a.example.com:443");
        assert_eq!(node.outbound["settings"]["vnext"][0]["users"][0]["id"], json!("11111111-1111-1111-1111-111111111111"));
        assert_eq!(node.outbound["streamSettings"]["wsSettings"]["path"], json!("/ray"));
        assert_eq!(node.outbound["streamSettings"]["security"], json!("tls"));
    }

    #[test]
    fn ss_sip002_decodes_base64url_credentials() {
        let creds = URL_SAFE_NO_PAD.encode("aes-256-gcm:hunter2");
        let line = format!("ss://{creds}@s.example.com:8388#%E6%97%A5%E6%9C%AC");
        let node = parse_share_link(&line, 2).expect("解析");
        assert_eq!(node.name, "日本");
        assert_eq!(node.protocol, "shadowsocks");
        assert_eq!(node.outbound["settings"]["servers"][0]["method"], json!("aes-256-gcm"));
        assert_eq!(node.outbound["settings"]["servers"][0]["password"], json!("hunter2"));
    }

    #[test]
    fn ss_legacy_layout_is_whole_payload_base64() {
        let line = format!("ss://{}#legacy", STANDARD.encode("chacha20-ietf-poly1305:pw@s.example.com:443"));
        let node = parse_share_link(&line, 1).expect("解析");
        assert_eq!(node.endpoint, "s.example.com:443");
    }

    /// 核心断言之一：失败必须带行号与原文片段，不能静默跳过。
    #[test]
    fn bad_line_reports_line_number_and_raw_fragment() {
        let err = parse_share_link("vmess://not-base64!!!", 42).expect_err("必须报错");
        assert_eq!(err.code, xt_contract::error::ErrorCode::InvalidRequest, "{err:?}");
        let detail = err.detail.expect("带 detail");
        assert_eq!(detail["line"], json!(42));
        assert!(detail["raw"].as_str().is_some_and(|raw| raw.contains("not-base64")));
    }

    #[test]
    fn ssr_is_rejected_by_name() {
        let err = parse_share_link("ssr://c29tZXRoaW5n", 3).expect_err("必须报错");
        assert!(err.message.contains("ShadowsocksR"), "{err:?}");
    }

    #[test]
    fn trojan_percent_encoded_password_and_alpn() {
        let line = "trojan://pass%40word@t.example.com:443?security=tls&type=ws&path=%2Fws&host=t.example.com&alpn=h2,http%2F1.1#T1";
        let node = parse_share_link(line, 1).expect("解析");
        assert_eq!(node.outbound["settings"]["servers"][0]["password"], json!("pass@word"));
        assert_eq!(node.outbound["streamSettings"]["tlsSettings"]["alpn"], json!(["h2", "http/1.1"]));
        assert_eq!(node.outbound["streamSettings"]["wsSettings"]["headers"]["Host"], json!("t.example.com"));
    }

    #[test]
    fn ipv6_endpoint_gets_brackets() {
        let line = "vless://uuid@[2001:db8::1]:8443?encryption=none#v6";
        let node = parse_share_link(line, 1).expect("解析");
        assert_eq!(node.endpoint, "[2001:db8::1]:8443");
    }

    /// 同一个服务器在分享链接与 Clash YAML 两种格式下必须得到同一个 NodeId，
    /// 否则用户换订阅格式就会丢掉选择记忆。
    #[test]
    fn ipv6_id_matches_across_formats() {
        let link = parse_share_link("vless://uuid@[2001:db8::1]:8443?encryption=none#v6", 1).expect("解析");
        let yaml = "proxies:\n  - {name: v6, type: vless, server: \"2001:db8::1\", port: 8443, uuid: uuid, encryption: none}\n";
        let nodes = crate::clash::parse(yaml).expect("解析");
        assert_eq!(link.id, nodes[0].id);
    }

    #[test]
    fn unknown_network_is_an_error_not_silently_tcp() {
        let line = "vless://uuid@h.example.com:443?encryption=none&type=quic-unknown#x";
        let err = parse_share_link(line, 9).expect_err("未知传输必须报错");
        assert!(err.message.contains("不支持的传输"), "{err:?}");
    }
}
