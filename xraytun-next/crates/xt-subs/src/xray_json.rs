//! Xray JSON 订阅（自建面板导出）解析。
//!
//! 输入是完整的 Xray 配置片段（顶层含 `outbounds`）。与其它三种格式的区别：
//! 这里拿到的是**已经渲染好的 outbound**，所以不做字段翻译，直接把它当作节点内容
//! 保留（只去掉 `tag` —— tag 的唯一来源是 xt-xrayconf）。
//!
//! `freedom` / `blackhole` / `dns` 是路由基础设施，不是节点：跳过它们不算隐瞒，
//! 因为它们本来就不代表一台可选的服务器。其余未知 protocol **报错**，
//! 避免用户拿到一份「少了几个节点却不知道少了什么」的列表。

use serde_json::{json, Value};

use xt_contract::error::{bad_request, ErrorBody};

use crate::{endpoint_of, node_id, snippet, ParsedNode, Where};

pub(crate) fn parse(body: &str) -> Result<Vec<ParsedNode>, ErrorBody> {
    let document: Value = serde_json::from_str(body).map_err(|err| {
        bad_request(format!("Xray JSON 订阅不是合法 JSON: {err}"))
            .with_detail(json!({ "reason": err.to_string() }))
    })?;

    let outbounds = document
        .get("outbounds")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("Xray JSON 订阅里没有 outbounds 数组"))?;
    if outbounds.is_empty() {
        return Err(bad_request("Xray JSON 订阅的 outbounds 是空数组"));
    }

    let mut nodes = Vec::new();
    for (index, outbound) in outbounds.iter().enumerate() {
        let protocol = outbound
            .get("protocol")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                crate::entry_error(Where::Index(index + 1, &snippet(&outbound.to_string(), 160)), "outbound 缺少 protocol 字段")
            })?
            .to_ascii_lowercase();

        if matches!(protocol.as_str(), "freedom" | "blackhole" | "dns") {
            continue;
        }
        nodes.push(convert(outbound, &protocol).map_err(|reason| {
            crate::entry_error(Where::Index(index + 1, &snippet(&outbound.to_string(), 160)), reason)
        })?);
    }

    if nodes.is_empty() {
        return Err(bad_request("Xray JSON 订阅里没有任何节点出站（只有 freedom/blackhole/dns）"));
    }
    Ok(nodes)
}

fn convert(outbound: &Value, protocol: &str) -> Result<ParsedNode, String> {
    let (address, port) = endpoint_from_settings(outbound, protocol)?;
    let port = u16::try_from(port).map_err(|_| format!("port 超出范围: {port}"))?;
    let name = outbound.get("tag").and_then(Value::as_str).map(str::to_string).unwrap_or_default();
    let name = if name.trim().is_empty() { format!("{address}:{port}") } else { name };

    // 保留整段 outbound（只去掉 tag）：mux / proxySettings 这类字段是用户
    // 真实配置的一部分，不是我们有权丢弃的东西。
    let mut content = outbound.clone();
    if let Some(object) = content.as_object_mut() {
        object.remove("tag");
    }

    Ok(ParsedNode {
        id: node_id(protocol, &address, port, &name),
        name,
        protocol: protocol.to_string(),
        endpoint: endpoint_of(&address, port),
        outbound: content,
    })
}

/// 从 `settings` 里取第一个服务端的地址与端口。
/// vmess/vless 用 `vnext`，trojan/shadowsocks/socks/http 用 `servers`。
fn endpoint_from_settings(outbound: &Value, protocol: &str) -> Result<(String, u32), String> {
    let settings = outbound.get("settings").ok_or("outbound 缺少 settings")?;
    let list_key = match protocol {
        "vmess" | "vless" => "vnext",
        "trojan" | "shadowsocks" | "ss" | "socks" | "http" => "servers",
        other => {
            return Err(format!(
                "不支持的 protocol: {other}（支持 vmess/vless/trojan/shadowsocks/socks/http）"
            ))
        }
    };
    let first = settings
        .get(list_key)
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .ok_or_else(|| format!("settings.{list_key} 里没有服务端"))?;
    let address = first
        .get("address")
        .and_then(Value::as_str)
        .ok_or("服务端缺少 address")?
        .to_string();
    let port = first
        .get("port")
        .and_then(Value::as_u64)
        .ok_or("服务端缺少 port")?;
    Ok((address, port as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_outbound_content_and_drops_routing_infrastructure() {
        let body = json!({
            "outbounds": [
                { "tag": "proxy-a", "protocol": "vless", "settings": { "vnext": [{ "address": "a.example.com", "port": 443, "users": [{ "id": "u", "encryption": "none" }] }] }, "streamSettings": { "network": "tcp", "security": "none" }, "mux": { "enabled": true } },
                { "tag": "direct", "protocol": "freedom", "settings": {} },
                { "tag": "block", "protocol": "blackhole", "settings": {} }
            ]
        })
        .to_string();
        let nodes = parse(&body).expect("解析");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "proxy-a");
        assert_eq!(nodes[0].endpoint, "a.example.com:443");
        // tag 不保留：由 xt-xrayconf 统一写入。
        assert!(nodes[0].outbound.get("tag").is_none());
        // mux 是用户配置的一部分，必须保留。
        assert_eq!(nodes[0].outbound["mux"]["enabled"], json!(true));
    }

    #[test]
    fn unsupported_protocol_reports_index_and_raw() {
        let body = json!({
            "outbounds": [
                { "tag": "ok", "protocol": "vless", "settings": { "vnext": [{ "address": "a.com", "port": 443 }] } },
                { "tag": "weird", "protocol": "wireguard", "settings": {} }
            ]
        })
        .to_string();
        let err = parse(&body).expect_err("必须报错");
        let detail = err.detail.expect("带 detail");
        assert_eq!(detail["index"], json!(2));
        assert!(detail["raw"].as_str().is_some_and(|raw| raw.contains("wireguard")));
    }

    #[test]
    fn no_outbounds_is_an_error() {
        assert!(parse("{\"inbounds\":[]}").is_err());
        assert!(parse("not json").is_err());
    }
}
