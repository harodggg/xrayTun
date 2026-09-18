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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
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
    let path = store.core_config_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("读运行配置失败（核心没在跑？）: {e}"))?;
    let cfg: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析运行配置失败: {e}"))?;
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
    let path = store.core_config_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(cfg) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (Vec::new(), Vec::new());
    };
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

/// 把 `inbound>>>tun>>>traffic>>>uplink` 这类计数器整理成 `tag -> (up, down)`。
fn traffic_by_tag(stats: &[xt_core::xray::StatEntry], dir: &str) -> HashMap<String, (u64, u64)> {
    let mut out: HashMap<String, (u64, u64)> = HashMap::new();
    let prefix = format!("{dir}>>>");
    for s in stats {
        let Some(rest) = s.name.strip_prefix(&prefix) else {
            continue;
        };
        let parts: Vec<&str> = rest.split(">>>").collect();
        if parts.len() != 3 || parts[1] != "traffic" {
            continue;
        }
        let entry = out.entry(parts[0].to_string()).or_insert((0, 0));
        match parts[2] {
            "uplink" => entry.0 = entry.0.saturating_add(s.value.max(0) as u64),
            "downlink" => entry.1 = entry.1.saturating_add(s.value.max(0) as u64),
            _ => {}
        }
    }
    out
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
            let up = traffic_by_tag(&stats, "inbound");
            let down = traffic_by_tag(&stats, "outbound");
            (Some((up, down)), None)
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

/// geo 数据文件所在目录是否可用（给界面一个明确的可用性信号）。
pub fn geo_unavailable_reason(root: &Path) -> Option<String> {
    if crate::supervisor::geo_dir(root).is_some() {
        None
    } else {
        Some("数据目录里没有 geosite.dat / geoip.dat".into())
    }
}

/// 供测试与诊断：把规则条件拼成一行。
pub fn rule_summary(rule: &Rule) -> String {
    describe_conditions(rule).join(" · ")
}
