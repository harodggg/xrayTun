//! xt-xrayconf —— Xray 配置生成：节点 + 设置 → 上游 JSON，纯函数
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! **纯函数**：不读文件、不碰进程、不查时钟。同样的输入一定得到同样的输出，
//! 所以「生成的配置是否合法」可以在单元测试里断言，也可以在真核心上跑
//! `xray run -test -c`（那是另一层验证，不是这一层的替代）。
//!
//! 这个 crate 只认识 xt-contract（不认识 `ParsedNode`）：配置生成只关心
//! 「一个 tag + 一段 outbound JSON」，节点是怎么解析来的与它无关。
//!
//! 本轮只有 proxy 模式：一个 socks 入站 + 一个 API 入站，出站是「选中的节点」
//! 与 `direct`。**不做热更新**：切节点 = 重新生成整份配置 + 重启核心
//! （配置是纯函数的产物，重启是最不容易出错的落地方式；热更新会引入一份
//! 「配置里说的」与「核心正在跑的」不一致的中间状态）。

use std::net::SocketAddr;

use serde_json::{json, Map, Value};
use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{LogLevel, NodeId};

/// outbound 的 tag 前缀。`node-<id>` 里的 id 是稳定派生的 NodeId，
/// 所以同一个节点在任何一次生成里都得到同一个 tag —— 统计（StatsService）
/// 与路由规则因此可以长期对齐。
pub const NODE_TAG_PREFIX: &str = "node-";
/// socks 入站的 tag。backend-3 按它过滤统计，不能随手改。
pub const SOCKS_TAG: &str = "socks";
/// API 入站的 tag。
pub const API_TAG: &str = "api";
/// 直连出站的 tag。
pub const DIRECT_TAG: &str = "direct";

/// 节点出站。`outbound` 是**内容**（protocol / settings / streamSettings），
/// tag 由本 crate 统一写入 —— 「一个节点一个 tag」只有一个地方说了算。
#[derive(Clone, Debug, PartialEq)]
pub struct OutboundSpec {
    pub node_id: NodeId,
    pub outbound: Value,
}

/// `generate` 的全部输入。`api_listen` 由 daemon 计算（socks 端口 + 1），
/// 不暴露成用户设置：用户改一个端口却忘了另一个，就会得到一个自己连不上的核心。
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigInputs {
    pub listen_socks: SocketAddr,
    pub api_listen: SocketAddr,
    pub selected: OutboundSpec,
    pub log_level: LogLevel,
}

/// 生成主实例配置（proxy 模式）。
pub fn generate(input: &ConfigInputs) -> Result<String, ErrorBody> {
    let socks = validate_socks(input.listen_socks)?;
    let api = validate_api(input.listen_socks, input.api_listen)?;
    let node_tag = node_tag(&input.selected.node_id);

    let mut root = Map::new();
    root.insert("log".into(), log_section(input.log_level));
    // API 只开 StatsService：其他服务（Handler/Logger/Routing）本轮没有消费者，
    // 开了就是一条没用的攻击面。
    root.insert("api".into(), json!({ "tag": API_TAG, "services": ["StatsService"] }));
    root.insert("stats".into(), json!({}));
    root.insert("policy".into(), json!({
        "levels": { "0": { "statsUserUplink": true, "statsUserDownlink": true } },
        // 四个开关全开：少了任何一个，StatsService 不会返回对应方向的计数器，
        // 界面就只能显示「未采样」。
        "system": {
            "statsInboundUplink": true,
            "statsInboundDownlink": true,
            "statsOutboundUplink": true,
            "statsOutboundDownlink": true
        }
    }));
    root.insert(
        "inbounds".into(),
        json!([
            {
                "tag": SOCKS_TAG,
                "listen": socks.ip().to_string(),
                "port": socks.port(),
                "protocol": "socks",
                // udp: 保留 UDP，数据面（将来 tun2socks / QUIC）要用；
                // noauth 是因为它只监听回环。
                "settings": { "auth": "noauth", "udp": true, "userLevel": 0 }
            },
            {
                "tag": API_TAG,
                "listen": api.ip().to_string(),
                "port": api.port(),
                "protocol": "dokodemo-door",
                "settings": { "address": api.ip().to_string() }
            }
        ]),
    );
    root.insert(
        "outbounds".into(),
        json!([
            outbound_with_tag(&input.selected)?,
            { "tag": DIRECT_TAG, "protocol": "freedom", "settings": {} }
        ]),
    );
    root.insert(
        "routing".into(),
        json!({
            "domainStrategy": "IPIfNonMatch",
            "rules": [
                // API 流量自己不能被代理，否则查询统计会先经过隧道（还会把
                // 「查询本身」算进字节数）。
                { "type": "field", "inboundTag": [API_TAG], "outboundTag": API_TAG },
                { "type": "field", "inboundTag": [SOCKS_TAG], "outboundTag": node_tag }
            ]
        }),
    );
    serialize(root)
}

/// 生成探测用的临时实例配置：**每个节点一个独立的 socks 入站端口**，
/// 这样多个节点可以并行探测，而每条探测走哪个节点由入站 tag 决定，
/// 不需要在探测过程中改配置或重启（那会引入等待与竞态）。
///
/// 返回 `(配置, 端口列表)`，端口列表与 `outbounds` 一一对应。
pub fn generate_probe(outbounds: &[OutboundSpec], base_port: u16) -> Result<(String, Vec<u16>), ErrorBody> {
    if outbounds.is_empty() {
        return Err(bad_request("探测配置没有任何出站，生成它没有意义"));
    }
    if base_port == 0 {
        // 端口 0 表示「让内核随机挑」，那我们就无法把真实端口告诉调用方，
        // 返回一个猜的端口就是假数据。
        return Err(bad_request("探测基准端口不能是 0（0 由内核随机分配，调用方无法得知真实端口）"));
    }
    let count = u16::try_from(outbounds.len())
        .map_err(|_| bad_request(format!("节点数 {} 超出探测端口可用范围", outbounds.len())))?;
    // 先把整个端口区间验证完再生成：生成到一半才发现溢出，会留下一份
    // 只覆盖了部分节点的配置。
    base_port
        .checked_add(count - 1)
        .ok_or_else(|| bad_request(format!("探测端口区间超出 u16：base_port={base_port}，节点数={count}")))?;

    let mut inbounds = Vec::with_capacity(outbounds.len());
    let mut rules = Vec::with_capacity(outbounds.len());
    let mut ports = Vec::with_capacity(outbounds.len());
    let mut rendered = Vec::with_capacity(outbounds.len() + 1);

    for (index, spec) in outbounds.iter().enumerate() {
        let port = base_port + index as u16;
        let inbound_tag = format!("socks-{index}");
        ports.push(port);
        inbounds.push(json!({
            "tag": inbound_tag,
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false, "userLevel": 0 }
        }));
        rules.push(json!({
            "type": "field",
            "inboundTag": [inbound_tag],
            "outboundTag": node_tag(&spec.node_id)
        }));
        rendered.push(outbound_with_tag(spec)?);
    }
    rendered.push(json!({ "tag": DIRECT_TAG, "protocol": "freedom", "settings": {} }));

    let mut root = Map::new();
    // 探测只需要真实 TTFB，日志噪音没有价值。
    root.insert("log".into(), json!({ "loglevel": "warning", "access": "", "error": "" }));
    root.insert("inbounds".into(), Value::Array(inbounds));
    root.insert("outbounds".into(), Value::Array(rendered));
    root.insert(
        "routing".into(),
        json!({ "domainStrategy": "IPIfNonMatch", "rules": Value::Array(rules) }),
    );
    Ok((serialize(root)?, ports))
}

/// `node-<id>`。id 本身是 base64url，不含 `/` 等会在别处被当作路径的字符。
pub fn node_tag(id: &NodeId) -> String {
    format!("{NODE_TAG_PREFIX}{id}")
}

fn outbound_with_tag(spec: &OutboundSpec) -> Result<Value, ErrorBody> {
    let mut object = match &spec.outbound {
        Value::Object(map) => map.clone(),
        _ => {
            return Err(ErrorBody::new(
                ErrorCode::Internal,
                format!("节点 {} 的 outbound 不是 JSON 对象", spec.node_id),
            ))
        }
    };
    if !object.get("protocol").is_some_and(Value::is_string) {
        return Err(ErrorBody::new(
            ErrorCode::Internal,
            format!("节点 {} 的 outbound 缺少 protocol 字段", spec.node_id),
        ));
    }
    // 覆盖而不是信任输入里的 tag：tag 的唯一来源是这里，避免出现
    // 「生成器以为叫 A，调用方以为叫 B」的错位。
    object.insert("tag".into(), Value::String(node_tag(&spec.node_id)));
    Ok(Value::Object(object))
}

fn log_section(level: LogLevel) -> Value {
    // access/error 为空字符串 = 输出到 stdout/stderr，由 daemon 逐行捕获成事件。
    json!({ "loglevel": xray_log_level(level), "access": "", "error": "" })
}

/// 契约里的 `LogLevel::Warn` 线上字符串是 `warn`，而 Xray 只认 `warning`。
/// 这个映射是**格式翻译**，不是回落：写 `warn` 会被核心判为非法配置。
fn xray_log_level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warning",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

fn validate_socks(addr: SocketAddr) -> Result<SocketAddr, ErrorBody> {
    if !addr.ip().is_loopback() {
        // 本轮的入口只服务本机；监听非回环地址等于把用户的出口开放给整个局域网，
        // 而配置里没有任何鉴权。
        return Err(bad_request(format!("socks 入站必须监听回环地址，收到 {addr}"))
            .with_detail(json!({ "listen_socks": addr.to_string() })));
    }
    Ok(addr)
}

fn validate_api(socks: SocketAddr, api: SocketAddr) -> Result<SocketAddr, ErrorBody> {
    if !api.ip().is_loopback() {
        return Err(bad_request(format!("API 入站必须监听回环地址，收到 {api}"))
            .with_detail(json!({ "api_listen": api.to_string() })));
    }
    let expected = socks
        .port()
        .checked_add(1)
        .ok_or_else(|| bad_request("socks 端口已经是 65535，无法再留出 API 端口"))?;
    if api.port() != expected {
        return Err(bad_request(format!(
            "API 端口必须是 socks 端口 + 1（期望 {expected}，收到 {}）",
            api.port()
        ))
        .with_detail(json!({
            "listen_socks": socks.to_string(),
            "api_listen": api.to_string(),
        })));
    }
    Ok(api)
}

/// 美化 JSON：落盘的配置是给人排查的，缩进值这个字节。
fn serialize(root: Map<String, Value>) -> Result<String, ErrorBody> {
    serde_json::to_string_pretty(&Value::Object(root)).map_err(|err| {
        ErrorBody::new(ErrorCode::Internal, format!("生成的配置无法序列化: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn spec(id: &str, protocol: &str) -> OutboundSpec {
        OutboundSpec {
            node_id: NodeId::new(id),
            outbound: json!({
                "protocol": protocol,
                "settings": { "vnext": [{ "address": "a.example.com", "port": 443, "users": [{ "id": "u", "encryption": "none" }] }] },
                "streamSettings": { "network": "tcp", "security": "none" }
            }),
        }
    }

    fn inputs(selected: OutboundSpec) -> ConfigInputs {
        ConfigInputs {
            listen_socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1080),
            api_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1081),
            selected,
            log_level: LogLevel::Info,
        }
    }

    /// 核心断言：socks 路由、outbound tag、NodeId 三者一一对应。
    #[test]
    fn socks_route_points_at_the_only_node_outbound_with_matching_id() {
        let node = spec("bm9kZS0x", "vless");
        let config: Value = serde_json::from_str(&generate(&inputs(node.clone())).expect("生成")).expect("合法 JSON");

        let outbounds = config["outbounds"].as_array().expect("outbounds 数组");
        assert_eq!(outbounds.len(), 2);
        let node_tag_value = node_tag(&node.node_id);
        assert_eq!(outbounds[0]["tag"], json!(node_tag_value));
        assert_eq!(outbounds[0]["protocol"], json!("vless"));
        assert_eq!(outbounds[1]["tag"], json!(DIRECT_TAG));

        let rules = config["routing"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        let socks_rule = rules.iter().find(|r| r["inboundTag"] == json!([SOCKS_TAG])).expect("socks 规则");
        assert_eq!(socks_rule["outboundTag"], json!(node_tag_value));

        // 一一对应：配置里只有一个 node-<id> tag，且它正是被选中的那个。
        let node_tags: Vec<&str> = outbounds
            .iter()
            .filter_map(|o| o["tag"].as_str())
            .filter(|t| t.starts_with(NODE_TAG_PREFIX))
            .collect();
        assert_eq!(node_tags, vec![node_tag_value.as_str()]);
        assert!(config["inbounds"].as_array().expect("inbounds").iter().any(|i| i["tag"] == json!(SOCKS_TAG)));
    }

    #[test]
    fn stats_and_api_are_wired_for_real_counters() {
        let config: Value = serde_json::from_str(&generate(&inputs(spec("a", "vless"))).expect("生成")).expect("JSON");
        assert_eq!(config["stats"], json!({}));
        assert_eq!(config["api"]["services"], json!(["StatsService"]));
        let api_inbound = config["inbounds"]
            .as_array()
            .expect("inbounds")
            .iter()
            .find(|i| i["tag"] == json!(API_TAG))
            .expect("api 入站");
        assert_eq!(api_inbound["protocol"], json!("dokodemo-door"));
        assert_eq!(api_inbound["port"], json!(1081));
        for key in ["statsInboundUplink", "statsInboundDownlink", "statsOutboundUplink", "statsOutboundDownlink"] {
            assert_eq!(config["policy"]["system"][key], json!(true), "{key} 必须为 true");
        }
    }

    #[test]
    fn wrong_api_port_is_refused_instead_of_guessed() {
        let mut input = inputs(spec("a", "vless"));
        input.api_listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999);
        let err = generate(&input).expect_err("API 端口错必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
        assert!(err.message.contains("socks 端口 + 1"), "{err:?}");
    }

    #[test]
    fn non_loopback_listen_is_refused() {
        let mut input = inputs(spec("a", "vless"));
        input.listen_socks = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1080);
        let err = generate(&input).expect_err("非回环必须拒绝");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
    }

    #[test]
    fn probe_gives_one_port_per_node_routed_to_its_own_tag() {
        let specs = vec![spec("a", "vless"), spec("b", "trojan")];
        let (text, ports) = generate_probe(&specs, 21000).expect("生成探测配置");
        assert_eq!(ports, vec![21000, 21001]);
        let config: Value = serde_json::from_str(&text).expect("JSON");

        let inbounds = config["inbounds"].as_array().expect("inbounds");
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[0]["port"], json!(21000));
        assert_eq!(inbounds[0]["tag"], json!("socks-0"));
        assert_eq!(inbounds[1]["tag"], json!("socks-1"));

        let rules = config["routing"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["inboundTag"], json!(["socks-0"]));
        assert_eq!(rules[0]["outboundTag"], json!(node_tag(&specs[0].node_id)));
        assert_eq!(rules[1]["outboundTag"], json!(node_tag(&specs[1].node_id)));
    }

    #[test]
    fn probe_refuses_overflow_and_empty_input() {
        assert!(generate_probe(&[], 20000).is_err());
        let specs = vec![spec("a", "vless"), spec("b", "trojan")];
        let err = generate_probe(&specs, 65535).expect_err("端口溢出必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
    }

    #[test]
    fn log_level_is_translated_to_what_xray_accepts() {
        let mut input = inputs(spec("a", "vless"));
        input.log_level = LogLevel::Warn;
        let config: Value = serde_json::from_str(&generate(&input).expect("生成")).expect("JSON");
        assert_eq!(config["log"]["loglevel"], json!("warning"));
    }
}
