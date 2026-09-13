//! 分享链接（`vmess://` / `vless://` / `trojan://` / `ss://` / `socks://` / `http://`）解析。
//!
//! 这一层的核心目标是**容错**。同一个协议在现实世界里有 5 种写法：
//!
//! * `vmess://` 的 JSON 里 `port` / `aid` 有时是字符串有时是数字；
//! * `ss://` 有 SIP002（userinfo 是 base64url）、旧版（整体 base64）两种布局；
//! * 查询参数大小写混用（`serviceName` / `servicename` / `Servicename`）；
//! * 名称放在 fragment 里且需要百分号解码；
//! * `vless://` 的 REALITY 参数用 `pbk` / `sid` / `spx` 这种短名。
//!
//! 所以这里刻意**不**用强类型 `serde` 结构体，而是拿 `serde_json::Value`
//! 逐个字段带默认值地取，避免一个可选字段的类型抖动就让整个节点解析失败。

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use percent_encoding::percent_decode_str;
use serde_json::Value;
use url::Url;

use crate::error::{Error, Result};
use crate::model::{
    MuxSettings, Node, NodeSource, Protocol, RealitySettings, TlsSettings, Transport, VmessSecurity,
};

/// 宽容的 base64 解码：依次尝试标准/无填充/URL-safe 变体。
///
/// 机场生成的订阅经常把 URL-safe 与标准字母表混用，严格解码会大面积失败。
pub fn b64_decode_lenient(s: &str) -> Result<Vec<u8>> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return Err(Error::Base64("空字符串".into()));
    }
    for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
        if let Ok(v) = engine.decode(&cleaned) {
            return Ok(v);
        }
    }
    let head: String = cleaned.chars().take(32).collect();
    Err(Error::Base64(format!("四种字母表都无法解码（前缀: {head}）")))
}

fn percent_decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn split_fragment(s: &str) -> (&str, Option<String>) {
    match s.split_once('#') {
        Some((body, frag)) => (body, Some(percent_decode(frag))),
        None => (s, None),
    }
}

/// 拆分 `host:port`，支持 `[::1]:443` 形式。
fn split_host_port(s: &str) -> Result<(String, u16)> {
    let s = s.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or(Error::MissingField("IPv6 缺少 ']'"))?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or(Error::MissingField("port"))?;
        return Ok((host.to_string(), port));
    }
    let (host, port) = s.rsplit_once(':').ok_or(Error::MissingField("host:port"))?;
    let port: u16 = port.trim().parse().map_err(|_| Error::MissingField("port"))?;
    if host.is_empty() {
        return Err(Error::MissingField("host"));
    }
    Ok((host.to_string(), port))
}

fn query_map(url: &Url) -> std::collections::HashMap<String, String> {
    url.query_pairs()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.into_owned()))
        .collect()
}

fn qget<'a>(q: &'a std::collections::HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| q.get(*k).map(|s| s.as_str())).filter(|s| !s.is_empty())
}

fn truthy(s: Option<&str>) -> bool {
    matches!(s.map(|v| v.to_ascii_lowercase()), Some(ref v) if v == "1" || v == "true" || v == "yes")
}

fn json_str(v: &Value, keys: &[&str]) -> Option<String> {
    let obj = v.as_object()?;
    for k in keys {
        match obj.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            Some(Value::Bool(b)) => return Some(b.to_string()),
            _ => {}
        }
    }
    None
}

fn json_u32(v: &Value, keys: &[&str]) -> Option<u32> {
    let obj = v.as_object()?;
    for k in keys {
        match obj.get(*k) {
            Some(Value::Number(n)) => return n.as_u64().map(|x| x as u32),
            Some(Value::String(s)) => {
                if let Ok(x) = s.trim().parse::<u32>() {
                    return Some(x);
                }
            }
            _ => {}
        }
    }
    None
}

// ===========================================================================
// 入口
// ===========================================================================

/// 解析一行分享链接。
pub fn parse_share_link(line: &str) -> Result<Node> {
    let line = line.trim();
    let lower = line.to_ascii_lowercase();
    let node = if lower.starts_with("vmess://") {
        parse_vmess(line)?
    } else if lower.starts_with("ssr://") {
        // ShadowsocksR 不在 Xray 原生支持范围内，明确报错而不是静默忽略。
        return Err(Error::BadShareLink {
            line: 0,
            line_text: "ShadowsocksR 不被 Xray 支持，请改用 ss/vless/trojan".into(),
        });
    } else if lower.starts_with("ss://") {
        parse_ss(line)?
    } else if lower.starts_with("socks://") || lower.starts_with("socks5://") {
        parse_socks_like(line, "socks")?
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        parse_socks_like(line, "http")?
    } else if lower.starts_with("vless://") || lower.starts_with("trojan://") {
        parse_standard(line)?
    } else {
        return Err(Error::BadShareLink { line: 0, line_text: line.chars().take(48).collect() });
    };
    Ok(node)
}

// ===========================================================================
// vmess://
// ===========================================================================

fn parse_vmess(line: &str) -> Result<Node> {
    let payload = &line["vmess://".len()..];
    let (payload, frag_name) = split_fragment(payload);
    let payload = payload.split('?').next().unwrap_or(payload);

    let decoded = b64_decode_lenient(payload)?;
    let text = String::from_utf8(decoded)
        .map_err(|e| Error::Base64(format!("vmess 负载不是 UTF-8: {e}")))?;
    let v: Value = serde_json::from_str(text.trim())?;

    let address = json_str(&v, &["add", "address"]).ok_or(Error::MissingField("add"))?;
    let port = json_u32(&v, &["port"]).ok_or(Error::MissingField("port"))? as u16;
    let uuid = json_str(&v, &["id", "uuid"]).ok_or(Error::MissingField("id"))?;
    let alter_id = json_u32(&v, &["aid", "alterId"]).unwrap_or(0);
    let security = match json_str(&v, &["scy", "security"]).unwrap_or_default().as_str() {
        "none" => VmessSecurity::None,
        "zero" => VmessSecurity::Zero,
        "aes-128-gcm" => VmessSecurity::Aes128Gcm,
        "chacha20-poly1305" => VmessSecurity::Chacha20Poly1305,
        _ => VmessSecurity::Auto,
    };

    let net = json_str(&v, &["net", "network"]).unwrap_or_else(|| "tcp".into());
    let host = json_str(&v, &["host"]).unwrap_or_default();
    let path = json_str(&v, &["path"]).unwrap_or_else(|| "/".into());
    let header_type = json_str(&v, &["type", "headerType"]).unwrap_or_default();
    let sni = json_str(&v, &["sni", "peer", "host"]).unwrap_or_default();
    let alpn = json_str(&v, &["alpn"]).unwrap_or_default();
    let fp = json_str(&v, &["fp"]).unwrap_or_default();
    let tls_on = matches!(json_str(&v, &["tls"]).unwrap_or_default().as_str(), "tls" | "reality" | "1" | "true");

    let transport = build_transport(&net, &host, &path, &header_type, "", "", &Default::default());
    let tls = build_tls(
        if tls_on { "tls" } else { "none" },
        &sni,
        &alpn,
        &fp,
        false,
        &Default::default(),
    );

    let name = frag_name
        .filter(|s| !s.is_empty())
        .or_else(|| json_str(&v, &["ps", "remarks"]))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{address}:{port}"));

    let mut node = Node {
        id: String::new(),
        name,
        address,
        port,
        protocol: Protocol::Vmess { uuid, alter_id, security },
        transport,
        tls,
        mux: None,
        source: NodeSource::Manual,
        tags: vec![],
        raw_uri: Some(line.to_string()),
    };
    node.refresh_id();
    Ok(node)
}

// ===========================================================================
// ss://
// ===========================================================================

fn parse_ss(line: &str) -> Result<Node> {
    let rest = &line["ss://".len()..];
    let (rest, frag_name) = split_fragment(rest);
    // 丢弃 `?plugin=...`：Xray 不支持 SIP003 插件，强行带上会导致连接失败。
    let rest = rest.split('?').next().unwrap_or(rest);

    let (userinfo_raw, hostport) = match rest.rsplit_once('@') {
        Some((u, hp)) => (u.to_string(), hp.to_string()),
        None => {
            // 旧版布局：整体 base64，内部是 `method:password@host:port`。
            let decoded = b64_decode_lenient(rest)?;
            let s = String::from_utf8(decoded)
                .map_err(|e| Error::Base64(format!("ss 负载不是 UTF-8: {e}")))?;
            let (u, hp) = s.rsplit_once('@').ok_or(Error::MissingField("host@ 分隔符"))?;
            (u.to_string(), hp.to_string())
        }
    };

    let userinfo = percent_decode(&userinfo_raw);
    // SIP002 规定 userinfo 是 base64url(method:password) 且不带填充；
    // 但也有人直接明文写 `method:password`，所以两种都试。
    let userinfo = if userinfo.contains(':') {
        userinfo
    } else {
        let decoded = b64_decode_lenient(&userinfo)?;
        String::from_utf8(decoded).map_err(|e| Error::Base64(format!("ss 凭据不是 UTF-8: {e}")))?
    };

    let (method, password) = userinfo
        .split_once(':')
        .ok_or(Error::MissingField("ss 凭据缺少 ':'（应为 method:password）"))?;
    let (address, port) = split_host_port(&hostport)?;

    let mut node = Node {
        id: String::new(),
        name: frag_name.filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{address}:{port}")),
        address,
        port,
        protocol: Protocol::Shadowsocks {
            method: method.trim().to_ascii_lowercase(),
            password: password.to_string(),
            // SS2022 的 UDP-over-TCP 需要显式开启，默认按兼容模式处理。
            uot: false,
        },
        transport: Transport::Tcp,
        tls: TlsSettings::default(),
        mux: None,
        source: NodeSource::Manual,
        tags: vec![],
        raw_uri: Some(line.to_string()),
    };
    node.refresh_id();
    Ok(node)
}

// ===========================================================================
// socks:// http://
// ===========================================================================

fn parse_socks_like(line: &str, kind: &str) -> Result<Node> {
    // 两种写法：`socks://base64(user:pass)@host:port` 与标准 URL。
    let scheme_end = line.find("://").unwrap() + 3;
    let scheme = &line[..scheme_end - 3];
    let rest = &line[scheme_end..];
    let (rest, frag_name) = split_fragment(rest);

    let base = format!("{scheme}://{rest}");
    if let Ok(url) = Url::parse(&base) {
        if let Some(host) = url.host_str() {
            if let Some(port) = url.port() {
                let (username, password) = if url.username().is_empty() {
                    (String::new(), String::new())
                } else {
                    let raw = percent_decode(url.username());
                    // v2rayN 风格：userinfo 整体是 base64(user:pass)
                    if raw.contains(':') {
                        let (u, p) = raw.split_once(':').unwrap();
                        (u.to_string(), p.to_string())
                    } else if let Ok(d) = b64_decode_lenient(&raw) {
                        match String::from_utf8(d) {
                            Ok(s) => match s.split_once(':') {
                                Some((u, p)) => (u.to_string(), p.to_string()),
                                None => (s, String::new()),
                            },
                            Err(_) => (raw.clone(), url.password().map(percent_decode).unwrap_or_default()),
                        }
                    } else {
                        (raw, url.password().map(percent_decode).unwrap_or_default())
                    }
                };
                let protocol = if kind == "socks" {
                    Protocol::Socks { username, password }
                } else {
                    Protocol::Http { username, password }
                };
                let mut node = Node {
                    id: String::new(),
                    name: frag_name
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("{host}:{port}")),
                    address: host.to_string(),
                    port,
                    protocol,
                    transport: Transport::Tcp,
                    tls: if scheme.eq_ignore_ascii_case("https") {
                        TlsSettings { enabled: true, server_name: host.to_string(), ..Default::default() }
                    } else {
                        TlsSettings::default()
                    },
                    mux: None,
                    source: NodeSource::Manual,
                    tags: vec![],
                    raw_uri: Some(line.to_string()),
                };
                node.refresh_id();
                return Ok(node);
            }
        }
    }
    Err(Error::MissingField("socks/http 链接缺少 host:port"))
}

// ===========================================================================
// vless:// trojan://
// ===========================================================================

fn parse_standard(line: &str) -> Result<Node> {
    let url = Url::parse(line)?;
    let scheme = url.scheme().to_ascii_lowercase();
    let address = url.host_str().ok_or(Error::MissingField("host"))?.to_string();
    let port = url.port().ok_or(Error::MissingField("port"))?;
    let q = query_map(&url);

    let user = percent_decode(url.username());
    let protocol = match scheme.as_str() {
        "vless" => {
            if user.is_empty() {
                return Err(Error::MissingField("vless uuid"));
            }
            Protocol::Vless {
                uuid: user,
                flow: qget(&q, &["flow"]).unwrap_or("").to_string(),
                encryption: qget(&q, &["encryption"]).unwrap_or("none").to_string(),
            }
        }
        "trojan" => {
            if user.is_empty() {
                return Err(Error::MissingField("trojan password"));
            }
            Protocol::Trojan { password: user }
        }
        _ => unreachable!("parse_standard 只处理 vless/trojan"),
    };

    let net = qget(&q, &["type", "net"]).unwrap_or("tcp").to_string();
    let host = qget(&q, &["host"]).unwrap_or("").to_string();
    let path = qget(&q, &["path"]).unwrap_or("/").to_string();
    let service_name = qget(&q, &["servicename"]).unwrap_or("").to_string();
    let mode = qget(&q, &["mode"]).unwrap_or("auto").to_string();
    let header_type = qget(&q, &["headertype"]).unwrap_or("").to_string();
    let quic_key = qget(&q, &["key"]).unwrap_or("").to_string();
    let quic_security = qget(&q, &["quicsecurity"]).unwrap_or("").to_string();

    let transport = build_transport(
        &net,
        &host,
        &path,
        &header_type,
        &service_name,
        &mode,
        &(quic_key.as_str(), quic_security.as_str()),
    );

    let security = qget(&q, &["security"]).unwrap_or_else(|| {
        if truthy(qget(&q, &["tls"])) {
            "tls"
        } else {
            "none"
        }
    });
    let sni = qget(&q, &["sni", "peer", "host"]).unwrap_or("").to_string();
    let alpn = qget(&q, &["alpn"]).unwrap_or("").to_string();
    let fp = qget(&q, &["fp"]).unwrap_or("").to_string();
    let insecure = truthy(qget(&q, &["allowinsecure", "insecure"]));
    let pbk = qget(&q, &["pbk"]).unwrap_or("").to_string();
    let sid = qget(&q, &["sid"]).unwrap_or("").to_string();
    let spx = qget(&q, &["spx"]).unwrap_or("/").to_string();

    let tls = build_tls(
        security,
        &sni,
        &alpn,
        &fp,
        insecure,
        &(pbk.as_str(), sid.as_str(), spx.as_str()),
    );

    let name = url
        .fragment()
        .map(percent_decode)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{address}:{port}"));

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
        raw_uri: Some(line.to_string()),
    };
    node.refresh_id();
    Ok(node)
}

// ===========================================================================
// 共享的传输 / TLS 构造
// ===========================================================================

type QuicParts<'a> = (&'a str, &'a str);
type RealityParts<'a> = (&'a str, &'a str, &'a str);

fn build_transport(
    net: &str,
    host: &str,
    path: &str,
    header_type: &str,
    service_name: &str,
    mode: &str,
    quic: &QuicParts<'_>,
) -> Transport {
    let path = if path.is_empty() { "/" } else { path };
    match net.to_ascii_lowercase().as_str() {
        "ws" | "websocket" => {
            Transport::WebSocket { path: path.to_string(), host: host.to_string() }
        }
        "grpc" | "gun" => {
            Transport::Grpc { service_name: service_name.to_string(), multi_mode: false }
        }
        "httpupgrade" => Transport::HttpUpgrade { path: path.to_string(), host: host.to_string() },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: path.to_string(),
            host: host.to_string(),
            mode: if mode.is_empty() { "auto".into() } else { mode.to_string() },
        },
        "quic" => Transport::Quic { key: quic.0.to_string(), security: quic.1.to_string() },
        "kcp" | "mkcp" => Transport::Kcp {
            header_type: if header_type.is_empty() { "none".into() } else { header_type.to_string() },
            seed: String::new(),
        },
        "h2" | "http" => Transport::Http { host: host.to_string(), path: path.to_string() },
        _ => Transport::Tcp,
    }
}

fn build_tls(
    security: &str,
    sni: &str,
    alpn: &str,
    fp: &str,
    insecure: bool,
    reality: &RealityParts<'_>,
) -> TlsSettings {
    let sec = security.to_ascii_lowercase();
    let alpn: Vec<String> = alpn
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let fingerprint = if fp.is_empty() { String::new() } else { fp.to_string() };

    match sec.as_str() {
        "reality" => TlsSettings {
            enabled: true,
            server_name: sni.to_string(),
            allow_insecure: insecure,
            alpn,
            fingerprint,
            reality: Some(RealitySettings {
                public_key: reality.0.to_string(),
                short_id: reality.1.to_string(),
                spider_x: if reality.2.is_empty() { "/".into() } else { reality.2.to_string() },
            }),
        },
        "tls" | "xtls" | "1" | "true" => TlsSettings {
            enabled: true,
            server_name: sni.to_string(),
            allow_insecure: insecure,
            alpn,
            fingerprint,
            reality: None,
        },
        _ => TlsSettings::default(),
    }
}

/// 便捷构造：手工添加节点时用。
pub fn manual_node(
    name: String,
    address: String,
    port: u16,
    protocol: Protocol,
    transport: Transport,
    tls: TlsSettings,
    mux: Option<MuxSettings>,
) -> Node {
    let mut node = Node {
        id: String::new(),
        name,
        address,
        port,
        protocol,
        transport,
        tls,
        mux,
        source: NodeSource::Manual,
        tags: vec![],
        raw_uri: None,
    };
    node.refresh_id();
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_lenient_handles_all_alphabets() {
        assert_eq!(b64_decode_lenient("aGVsbG8=").unwrap(), b"hello");
        // 无填充的标准字母表
        assert_eq!(b64_decode_lenient("aGVsbG8").unwrap(), b"hello");
        // URL-safe 字母表（`_` 在标准表里是非法字符）
        assert_eq!(b64_decode_lenient("aGVsbG8_").unwrap().len(), 6);
        assert!(b64_decode_lenient("!!!").is_err());
        assert!(b64_decode_lenient("").is_err());
    }

    #[test]
    fn split_host_port_variants() {
        assert_eq!(split_host_port("a.com:443").unwrap(), ("a.com".into(), 443));
        assert_eq!(split_host_port("[2001:db8::1]:8443").unwrap(), ("2001:db8::1".into(), 8443));
        assert!(split_host_port("a.com").is_err());
    }

    #[test]
    fn vmess_with_string_port_and_numeric_aid() {
        use base64::engine::general_purpose::STANDARD;
        let json = r#"{"v":"2","ps":"节点A","add":"a.example.com","port":"443","id":"11111111-1111-1111-1111-111111111111","aid":0,"scy":"auto","net":"ws","path":"/ray","host":"a.example.com","tls":"tls","sni":"a.example.com"}"#;
        let line = format!("vmess://{}#ignored", STANDARD.encode(json));
        let n = parse_share_link(&line).unwrap();
        assert_eq!(n.address, "a.example.com");
        assert_eq!(n.port, 443);
        // fragment 优先于 ps
        assert_eq!(n.name, "ignored");
        assert!(n.tls.enabled);
        assert_eq!(n.transport.network(), "ws");
    }

    #[test]
    fn vmess_falls_back_to_ps_when_no_fragment() {
        use base64::engine::general_purpose::STANDARD;
        let json = r#"{"ps":"备用名","add":"a.com","port":80,"id":"x","net":"tcp"}"#;
        let line = format!("vmess://{}", STANDARD.encode(json));
        assert_eq!(parse_share_link(&line).unwrap().name, "备用名");
    }

    #[test]
    fn ss_sip002_userinfo_is_base64url() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let creds = URL_SAFE_NO_PAD.encode("aes-256-gcm:hunter2");
        let line = format!("ss://{creds}@s.example.com:8388#%E6%97%A5%E6%9C%AC");
        let n = parse_share_link(&line).unwrap();
        assert_eq!(n.name, "日本");
        assert_eq!(n.address, "s.example.com");
        assert_eq!(n.port, 8388);
        match n.protocol {
            Protocol::Shadowsocks { method, password, .. } => {
                assert_eq!(method, "aes-256-gcm");
                assert_eq!(password, "hunter2");
            }
            other => panic!("协议不对: {other:?}"),
        }
    }

    #[test]
    fn ss_legacy_whole_payload_base64() {
        use base64::engine::general_purpose::STANDARD;
        let line = format!(
            "ss://{}#legacy",
            STANDARD.encode("chacha20-ietf-poly1305:pw@s.example.com:443")
        );
        let n = parse_share_link(&line).unwrap();
        assert_eq!(n.address, "s.example.com");
        assert_eq!(n.port, 443);
    }

    #[test]
    fn ssr_is_rejected_explicitly() {
        assert!(parse_share_link("ssr://c29tZXRoaW5n").is_err());
    }

    #[test]
    fn vless_reality_grpc() {
        let line = "vless://uuid-1@h.example.com:443?encryption=none&security=reality&sni=www.apple.com&fp=chrome&pbk=PUBKEY&sid=abcd&spx=%2F&type=grpc&serviceName=mysvc#R1";
        let n = parse_share_link(line).unwrap();
        assert_eq!(n.name, "R1");
        assert_eq!(n.tls.effective_security(), "reality");
        let r = n.tls.reality.as_ref().unwrap();
        assert_eq!(r.public_key, "PUBKEY");
        assert_eq!(r.short_id, "abcd");
        assert_eq!(r.spider_x, "/");
        match n.transport {
            Transport::Grpc { service_name, .. } => assert_eq!(service_name, "mysvc"),
            other => panic!("传输不对: {other:?}"),
        }
    }

    #[test]
    fn trojan_ws_with_alpn() {
        let line = "trojan://pass%40word@t.example.com:443?security=tls&type=ws&path=%2Fws&host=t.example.com&alpn=h2,http%2F1.1#T1";
        let n = parse_share_link(line).unwrap();
        match n.protocol {
            Protocol::Trojan { password } => assert_eq!(password, "pass@word"),
            other => panic!("协议不对: {other:?}"),
        }
        assert_eq!(n.tls.alpn, vec!["h2", "http/1.1"]);
        match n.transport {
            Transport::WebSocket { path, host } => {
                assert_eq!(path, "/ws");
                assert_eq!(host, "t.example.com");
            }
            other => panic!("传输不对: {other:?}"),
        }
    }

    #[test]
    fn socks_with_base64_userinfo() {
        use base64::engine::general_purpose::STANDARD;
        let creds = STANDARD.encode("user:pw");
        let line = format!("socks://{creds}@127.0.0.1:1080#local");
        let n = parse_share_link(&line).unwrap();
        match n.protocol {
            Protocol::Socks { username, password } => {
                assert_eq!(username, "user");
                assert_eq!(password, "pw");
            }
            other => panic!("协议不对: {other:?}"),
        }
    }

    #[test]
    fn https_link_enables_tls() {
        let n = parse_share_link("https://u:p@h.example.com:8443#H").unwrap();
        assert!(n.tls.enabled);
        assert_eq!(n.tls.server_name, "h.example.com");
        assert_eq!(n.protocol.name(), "http");
    }
}
