//! 网络流动拓扑：**真实**的入口、规则链与出口，以及各自的实际流量。
//!
//! # 数据来源，以及一处必须说清的边界
//!
//! * **拓扑**（入口 / 规则链 / 出口）来自运行中的真实配置 —— 就是 Xray 正在
//!   加载的那份 `runtime/config.json`。
//! * **流量**（每个入口、每个出口的字节数）来自 Xray 的 `StatsService`。
//!   `policy` 里打开了 `statsInbound*/statsOutbound*`，所以这是实测量。
//! * **每条连接走了哪条规则 —— 拿不到。** Xray 的统计只有
//!   `inbound>>>` / `outbound>>>` 两类计数器，**没有 per-rule 计数器**。
//!   所以「每辆车实际走了哪条匝道」无法从真实数据得出。
//!
//! 界面据此必须如实表达：车流画在**入口↔出口**之间（有实测依据），
//! 规则链作为拓扑与判定依据展示；而「某个目的地走哪条规则」由
//! [`explain_dest`] 用真实数据算出来 —— 那一条是有依据的。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::State;

use xt_core::routing::explain::{explain, DestQuery, Rule, RouteExplanation};
use xt_core::routing::geo::GeoData;
use xt_core::xray::{query_stats, API_PORT};

use super::*;

/// 一个入口（入站）。
#[derive(Debug, Clone, Serialize)]
pub struct TopoInbound {
    pub tag: String,
    pub protocol: String,
    pub port: Option<u16>,
    /// 实测：该入口的上行/下行字节（累计）。
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}

/// 一个出口（出站）。
#[derive(Debug, Clone, Serialize)]
pub struct TopoOutbound {
    pub tag: String,
    pub protocol: String,
    /// 这个出口是「节点」还是「直连/拦截」这类功能性出口。
    pub kind: String,
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
}

/// 规则链上的一条规则。
#[derive(Debug, Clone, Serialize)]
pub struct TopoRule {
    pub index: usize,
    pub tag: String,
    pub outbound: String,
    /// 人类可读的条件摘要，例如 `域名 geosite:cn`。
    pub conditions: Vec<String>,
}

/// 拓扑全貌。
#[derive(Debug, Clone, Serialize)]
pub struct Topology {
    pub inbound: Vec<TopoInbound>,
    pub rule: Vec<TopoRule>,
    pub outbound: Vec<TopoOutbound>,
    /// 取流量失败时的原因（例如核心没在跑）。界面据此如实说明，
    /// 而不是画一条 0 字节的假流量。
    pub traffic_error: Option<String>,
    /// 数据目录里是否有 geosite/geoip —— 没有的话域名规则无法判定。
    pub geo_available: bool,
}

/// 把规则的条件字段翻成一句人话。
fn describe_conditions(rule: &Rule) -> Vec<String> {
    let mut out = Vec::new();
    if !rule.conds.inbound_tag.is_empty() {
        out.push(format!("入站 {}", rule.conds.inbound_tag.join("、")));
    }
    for d in &rule.conds.domain {
        // `geosite:cn` 这类保留原样：那正是配置里的写法，用户能对上
        out.push(format!("域名 {d}"));
    }
    for i in &rule.conds.ip {
        out.push(format!("IP {i}"));
    }
    if let Some(p) = &rule.conds.port {
        out.push(format!("端口 {p}"));
    }
    if let Some(n) = &rule.conds.network {
        out.push(format!("网络 {n}"));
    }
    out
}

/// 从运行中的配置里读规则链。
fn load_rules(store: &xt_core::store::Store) -> Result<Vec<Rule>, String> {
    let cfg = read_runtime_config(store)?;
    rules_from_config(&cfg)
}

/// 读运行中的配置。读不到时给出**能读懂的原因**（最常见就是核心没在跑）。
fn read_runtime_config(store: &xt_core::store::Store) -> Result<serde_json::Value, String> {
    let path = store.core_config_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("读运行配置失败（核心没在跑？）: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("解析运行配置失败: {e}"))
}

/// 从配置里取规则链。**与读文件分开是为了能测**：这段是纯转换。
fn rules_from_config(cfg: &serde_json::Value) -> Result<Vec<Rule>, String> {
    let rules = cfg
        .get("routing")
        .and_then(|r| r.get("rules"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(vec![]));
    serde_json::from_value(rules).map_err(|e| format!("解析规则失败: {e}"))
}

/// 入口的静态信息：`(tag, protocol, port)`。
type InboundInfo = (String, String, Option<u16>);
/// 出口的静态信息：`(tag, protocol)`。
type OutboundInfo = (String, String);

/// 从配置里读入口与出口的静态信息。
fn load_endpoints(store: &xt_core::store::Store) -> (Vec<InboundInfo>, Vec<OutboundInfo>) {
    read_runtime_config(store)
        .map(|cfg| endpoints_from_config(&cfg))
        .unwrap_or_default()
}

/// 从配置里取入口与出口。同样与读文件分开，便于测试。
fn endpoints_from_config(cfg: &serde_json::Value) -> (Vec<InboundInfo>, Vec<OutboundInfo>) {
    let inbound = cfg
        .get("inbounds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|i| {
                    (
                        i.get("tag").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        i.get("protocol").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        i.get("port").and_then(|v| v.as_u64()).map(|p| p as u16),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let outbound = cfg
        .get("outbounds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| {
                    (
                        o.get("tag").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        o.get("protocol").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    (inbound, outbound)
}

/// 出口的类别：节点 / 直连 / 拦截 / 内部。
fn outbound_kind(tag: &str, protocol: &str) -> String {
    match protocol {
        "blackhole" => "block",
        "dns" => "dns",
        "freedom" => {
            if tag == "direct" {
                "direct"
            } else {
                "internal"
            }
        }
        _ => "node",
    }
    .to_string()
}

/// 取拓扑：真实入口 / 规则链 / 出口 + 实测流量。
#[tauri::command]
pub async fn routing_topology(
    state: State<'_, AppState>,
) -> Result<Topology, String> {
    let store = &state.store;
    let rules = load_rules(store)?;
    let (inbounds, outbounds) = load_endpoints(store);

    // 流量：核心没在跑时拿不到，如实记录原因而不是画 0
    let addr: SocketAddr = ([127, 0, 0, 1], API_PORT).into();
    let (traffic, traffic_error) = match query_stats(addr, Duration::from_millis(1200)).await {
        Ok(stats) => {
            (
                Some((
                    xt_core::xray::traffic_by_tag(&stats, "inbound"),
                    xt_core::xray::traffic_by_tag(&stats, "outbound"),
                )),
                None,
            )
        }
        Err(e) => (None, Some(format!("取流量失败：{e}"))),
    };
    let (in_up, out_up) = traffic.unwrap_or_default();

    let inbound = inbounds
        .into_iter()
        .map(|(tag, protocol, port)| {
            let (up, down) = in_up.get(&tag).copied().unwrap_or((0, 0));
            TopoInbound {
                tag,
                protocol,
                port,
                uplink_bytes: up,
                downlink_bytes: down,
            }
        })
        .collect();

    let outbound = outbounds
        .into_iter()
        .map(|(tag, protocol)| {
            let (up, down) = out_up.get(&tag).copied().unwrap_or((0, 0));
            TopoOutbound {
                tag: tag.clone(),
                kind: outbound_kind(&tag, &protocol),
                protocol,
                uplink_bytes: up,
                downlink_bytes: down,
            }
        })
        .collect();

    let rule = rules
        .iter()
        .enumerate()
        .map(|(index, r)| TopoRule {
            index,
            tag: r.tag.clone().unwrap_or_else(|| format!("规则 #{index}")),
            outbound: r.outbound.clone(),
            conditions: describe_conditions(r),
        })
        .collect();

    Ok(Topology {
        inbound,
        rule,
        outbound,
        traffic_error,
        geo_available: crate::supervisor::geo_dir(state.store.root()).is_some(),
    })
}

/// 判定一个目的地会走哪条规则（用真实规则 + 真实 geosite/geoip 数据）。
///
/// 首次调用会解析 geosite/geoip（约 350ms、只保留规则引用的类别），
/// 之后走缓存。
#[tauri::command]
pub async fn explain_dest(
    state: State<'_, AppState>,
    dest: String,
) -> Result<RouteExplanation, String> {
    let dest = dest.trim().to_string();
    if dest.is_empty() {
        return Err("请输入域名或 IP".into());
    }
    let rules = load_rules(&state.store)?;

    // geo 数据的加载成本不低（解析 28MB 的 protobuf），缓存起来。
    let geo: Arc<GeoData> = {
        let mut slot = state.geo.lock().await;
        if let Some(g) = slot.as_ref() {
            Arc::clone(g)
        } else {
            let Some(dir) = crate::supervisor::geo_dir(state.store.root()) else {
                return Err("找不到 geosite.dat / geoip.dat，无法判定域名规则".into());
            };
            let mut sites = Vec::new();
            let mut ips = Vec::new();
            for r in &rules {
                for d in &r.conds.domain {
                    if let Some(c) = d.strip_prefix("geosite:") {
                        sites.push(c.to_string());
                    }
                }
                for i in &r.conds.ip {
                    if let Some(c) = i.strip_prefix("geoip:") {
                        ips.push(c.to_string());
                    }
                }
            }
            let loaded = xt_core::routing::geo::GeoData::load(&dir, &sites, &ips)
                .map_err(|e| format!("加载 geo 数据失败：{e}"))?;
            let arc = Arc::new(loaded);
            *slot = Some(Arc::clone(&arc));
            arc
        }
    };

    let query = match dest.parse::<std::net::IpAddr>() {
        Ok(ip) => DestQuery {
            ip: Some(ip),
            port: 443,
            network: "tcp".into(),
            ..Default::default()
        },
        Err(_) => DestQuery {
            host: Some(dest.clone()),
            port: 443,
            network: "tcp".into(),
            ..Default::default()
        },
    };
    Ok(explain(&rules, &geo, &query))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(rules: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "inbounds": [
                { "tag": "tun", "protocol": "tun" },
                { "tag": "socks", "protocol": "socks", "port": 10808 }
            ],
            "outbounds": [
                { "tag": "node-abc", "protocol": "vless" },
                { "tag": "direct", "protocol": "freedom" },
                { "tag": "block", "protocol": "blackhole" }
            ],
            "routing": { "rules": rules }
        })
    }

    #[test]
    fn parses_rules_in_order() {
        let cfg = cfg_with(serde_json::json!([
            { "ruleTag": "ads", "outboundTag": "block", "domain": ["geosite:category-ads-all"] },
            { "ruleTag": "fallback", "outboundTag": "node-abc", "network": "tcp,udp" }
        ]));
        let rules = rules_from_config(&cfg).expect("应当解析成功");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].tag.as_deref(), Some("ads"));
        assert_eq!(rules[0].outbound, "block");
        assert_eq!(rules[1].conds.network.as_deref(), Some("tcp,udp"));
    }

    /// 配置里没有 routing 段不是错误：那是「没有规则」的合法状态。
    #[test]
    fn missing_routing_section_is_empty_not_error() {
        let cfg = serde_json::json!({ "inbounds": [], "outbounds": [] });
        assert_eq!(rules_from_config(&cfg).unwrap().len(), 0);
    }

    #[test]
    fn reads_inbound_and_outbound_endpoints() {
        let cfg = cfg_with(serde_json::json!([]));
        let (inbound, outbound) = endpoints_from_config(&cfg);
        assert_eq!(inbound.len(), 2);
        assert_eq!(inbound[0], ("tun".to_string(), "tun".to_string(), None));
        assert_eq!(inbound[1], ("socks".to_string(), "socks".to_string(), Some(10808)));
        assert_eq!(outbound.len(), 3);
        assert_eq!(outbound[0], ("node-abc".to_string(), "vless".to_string()));
    }

    /// 条件要翻成能读的一句话；顺序与配置里的字段顺序一致。
    #[test]
    fn describes_conditions_in_readable_form() {
        let cfg = cfg_with(serde_json::json!([
            {
                "ruleTag": "r", "outboundTag": "direct",
                "domain": ["geosite:cn"], "ip": ["geoip:cn"], "port": "443"
            }
        ]));
        let rules = rules_from_config(&cfg).unwrap();
        let got = describe_conditions(&rules[0]);
        assert_eq!(got, vec!["域名 geosite:cn", "IP geoip:cn", "端口 443"]);
    }

    /// 出口类别决定车道颜色：别把 freedom 都当成直连
    /// （`api` 出站也是 freedom，但它属于内部）。
    #[test]
    fn classifies_outbound_kinds() {
        assert_eq!(outbound_kind("direct", "freedom"), "direct");
        assert_eq!(outbound_kind("api", "freedom"), "internal");
        assert_eq!(outbound_kind("block", "blackhole"), "block");
        assert_eq!(outbound_kind("dns-out", "dns"), "dns");
        assert_eq!(outbound_kind("node-abc", "vless"), "node");
    }
}
