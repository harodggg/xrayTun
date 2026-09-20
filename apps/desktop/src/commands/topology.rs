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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;

use xt_core::routing::explain::{explain, DestQuery, Rule, RouteExplanation};
use xt_core::routing::geo::GeoData;
use xt_core::xray::stats::{monotonic_traffic_by_tag, MonotonicCounters};
use xt_core::xray::{query_stats, StatEntry, API_PORT};

use super::*;

/// 一个入口（入站）。
#[derive(Debug, Clone, Serialize)]
pub struct TopoInbound {
    pub tag: String,
    pub protocol: String,
    pub port: Option<u16>,
    /// 实测：该入口的上行/下行字节（累计，跨核心重启保持单调）。
    /// `Topology.traffic_ok == false` 时固定为 0，**不是**真实读数。
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
    /// 累计字节，跨核心重启保持单调。`Topology.traffic_ok == false` 时
    /// 固定为 0，**不是**真实读数。
    pub uplink_bytes: u64,
    pub downlink_bytes: u64,
    /// 从核心访问日志累计到的那条出口的**连接数**。
    ///
    /// # 什么时候需要看它
    ///
    /// 有两类出口的**字节计数器恒为 0**，那是测量盲区而非事实：
    ///
    /// * `dns-out`（协议 `dns`）—— UDP 出站流量不计入 `StatsService`；
    /// * `api`（本机回环）—— 回环流量不计入统计。
    ///
    /// 本机实测这两者分别有 4769 / 5374 条连接，而字节一直是 0。界面如果
    /// 只显示 `0 B`，会让人以为「这两个出口没在用」。连接数是它们唯一可得
    /// 的活跃度指标。
    ///
    /// `None` 表示**没有观察到**（核心没跑、日志里还没有连接行），
    /// 与 `Some(0)`（观察到 0 条）不同 —— 界面应显示「—」而不是 `0`。
    #[serde(default)]
    pub connections: Option<u64>,
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
    /// **本次流量是否可信。**
    ///
    /// `false` = 这次没查到（原因见 `traffic_error`）。此时所有 `*_bytes`
    /// 都是占位 0，**不是**真实读数 —— 界面必须显示「—」，不能显示 0 B。
    /// 恒有 `traffic_ok == traffic_error.is_none()`。
    pub traffic_ok: bool,
    /// 累计字节跨核心重启续接时，被补偿掉的归零次数。
    ///
    /// `>0` 表示核心重启过（换网 / 熄屏唤醒 / 节点抖动的看门狗重建），
    /// 累计值已被续接而不是归零。界面可以据此如实说明，不用平滑掩盖。
    pub counter_resets: u32,
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

/// 进程内的累计流量读数表：用来把「核心重启后计数器归零」补偿掉。
///
/// 为什么是模块级 `static` 而不是 `AppState` 的字段：本任务的写入范围
/// 不含 `state.rs`，而这份状态只服务于本命令自己的采样历史，没有跨模块
/// 语义。用 `tokio::sync::Mutex` 是因为锁要跨 `.await`（见
/// [`routing_topology`]）。
static TRAFFIC_COUNTERS: OnceLock<Mutex<MonotonicCounters>> = OnceLock::new();

fn traffic_counters() -> &'static Mutex<MonotonicCounters> {
    TRAFFIC_COUNTERS.get_or_init(|| Mutex::new(MonotonicCounters::new()))
}

/// 一次流量采样的结果：要么是实测值，要么是「没查到」的原因。
///
/// **这里刻意不用 0 表示「没查到」**：0 是合法读数（真的没有流量），两者
/// 混在一起，界面就会在「查不到」时把累计值画成 0 —— 用户看到的是
/// 「8 GiB → 0 → 8 GiB」。
#[derive(Debug, Default)]
struct TrafficRead {
    inbound: HashMap<String, (u64, u64)>,
    outbound: HashMap<String, (u64, u64)>,
    error: Option<String>,
    /// 观察到的计数器归零次数（核心重启 / 换网 / 唤醒会 stop_core+start_core）。
    resets: u32,
}

/// 把一次查询结果读成流量。与 `query_stats` 分开：这段是纯的、能测。
///
/// 累计值跨核心重启保持单调，见 [`MonotonicCounters`]。
fn read_traffic(
    stats: Result<Vec<StatEntry>, String>,
    counters: &mut MonotonicCounters,
) -> TrafficRead {
    match stats {
        Ok(stats) => {
            let (inbound, inbound_resets) =
                monotonic_traffic_by_tag(counters, &stats, "inbound");
            let (outbound, outbound_resets) =
                monotonic_traffic_by_tag(counters, &stats, "outbound");
            TrafficRead {
                inbound,
                outbound,
                error: None,
                resets: inbound_resets.max(outbound_resets),
            }
        }
        Err(e) => TrafficRead {
            error: Some(format!("取流量失败：{e}")),
            ..TrafficRead::default()
        },
    }
}

/// 把静态拓扑（入口 / 规则链 / 出口）与实测流量拼成给界面的 [`Topology`]。
///
/// 纯函数，便于测试：这里的错法几乎都是「数字悄悄不对」，只能靠断言钉住。
fn assemble_topology(
    inbounds: Vec<InboundInfo>,
    outbounds: Vec<OutboundInfo>,
    rules: &[Rule],
    traffic: &TrafficRead,
    geo_available: bool,
    connections: &HashMap<String, u64>,
) -> Topology {
    let inbound = inbounds
        .into_iter()
        .map(|(tag, protocol, port)| {
            let (up, down) = traffic.inbound.get(&tag).copied().unwrap_or((0, 0));
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
            let (up, down) = traffic.outbound.get(&tag).copied().unwrap_or((0, 0));
            TopoOutbound {
                tag: tag.clone(),
                kind: outbound_kind(&tag, &protocol),
                protocol,
                uplink_bytes: up,
                downlink_bytes: down,
                // 没观察到就是 `None`（界面显示「—」），不是 `Some(0)`。
            connections: connections.get(&tag).copied(),
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

    Topology {
        inbound,
        rule,
        outbound,
        traffic_error: traffic.error.clone(),
        traffic_ok: traffic.error.is_none(),
        counter_resets: traffic.resets,
        geo_available,
    }
}

/// 取拓扑：真实入口 / 规则链 / 出口 + 实测流量。
#[tauri::command]
pub async fn routing_topology(
    state: State<'_, AppState>,
) -> Result<Topology, String> {
    let store = &state.store;
    let rules = load_rules(store)?;
    let (inbounds, outbounds) = load_endpoints(store);

    // 流量：核心没在跑时拿不到，如实记录原因而不是画 0。
    //
    // 锁**包住查询**：累计值的单调化依赖「采样顺序 = 观察顺序」。两个并发的
    // `routing_topology`（例如 React StrictMode 的双次挂载）若乱序观察，后采到
    // 的值会成为基准，先采到的旧值就被误判成「核心重启」，凭空抬高读数。
    // 查询最长 1.2s 超时，每 2 秒一次的轮询下串行化没有代价。
    let addr: SocketAddr = ([127, 0, 0, 1], API_PORT).into();
    let traffic = {
        let mut counters = traffic_counters().lock().await;
        let stats = query_stats(addr, Duration::from_millis(1200))
            .await
            .map_err(|e| e.to_string());
        read_traffic(stats, &mut counters)
    };

    // 连接数来自访问日志解析（`dns-out` / `api` 的字节计数器恒为 0，
    // 只能靠它体现活跃度）。取一份快照，避免在锁内做别的事。
    let connections = state
        .with(|i| i.connections.snapshot())
        .unwrap_or_default();

    Ok(assemble_topology(
        inbounds,
        outbounds,
        &rules,
        &traffic,
        crate::supervisor::geo_dir(state.store.root()).is_some(),
        &connections,
    ))
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

    /// 用标准夹具（入口 tun/socks，出口 node-abc/direct/block）拼一份拓扑。
    fn topo_from(read: &TrafficRead) -> Topology {
        let cfg = cfg_with(serde_json::json!([]));
        let (inbounds, outbounds) = endpoints_from_config(&cfg);
        let rules = rules_from_config(&cfg).unwrap();
        assemble_topology(inbounds, outbounds, &rules, read, true, &Default::default())
    }

    /// 某个出口的 (上行, 下行)。
    fn out_bytes(topo: &Topology, tag: &str) -> (u64, u64) {
        let o = topo
            .outbound
            .iter()
            .find(|o| o.tag == tag)
            .expect("夹具里有这个出口");
        (o.uplink_bytes, o.downlink_bytes)
    }

    fn exit_stat(name: &str, value: i64) -> StatEntry {
        StatEntry { name: name.to_string(), value }
    }

    /// 带连接数的组装（默认空 = 没有观察到任何连接行）。
    ///
    /// 配置里**必须带上 `dns-out` / `api`** —— 这两条出口正是本测试要覆盖的
    /// 对象（它们的字节计数器恒为 0，只能靠连接数体现活跃度）。
    fn topo_with_connections(read: &TrafficRead, conns: &HashMap<String, u64>) -> Topology {
        let cfg = serde_json::json!({
            "inbounds": [
                { "tag": "tun", "protocol": "tun" },
                { "tag": "api", "protocol": "dokodemo-door", "port": 10085 }
            ],
            "outbounds": [
                { "tag": "node-abc", "protocol": "vless" },
                { "tag": "direct", "protocol": "freedom" },
                { "tag": "block", "protocol": "blackhole" },
                { "tag": "dns-out", "protocol": "dns" },
                { "tag": "api", "protocol": "freedom" }
            ],
            "routing": { "rules": [] }
        });
        let (inbounds, outbounds) = endpoints_from_config(&cfg);
        let rules = rules_from_config(&cfg).unwrap();
        assemble_topology(inbounds, outbounds, &rules, read, true, conns)
    }

    /// **本次修复的核心语义**：`dns-out` / `api` 的字节计数器恒为 0
    /// （`StatsService` 不统计 UDP 出站与本机回环），界面只能靠连接数
    /// 体现它们的活跃度。所以连接数必须能传到接口层。
    #[test]
    fn connection_counts_reach_the_outbound_payload() {
        let read = TrafficRead::default();
        let mut conns: HashMap<String, u64> = HashMap::new();
        conns.insert("dns-out".into(), 4769);
        conns.insert("api".into(), 5374);
        let topo = topo_with_connections(&read, &conns);
        let get = |tag: &str| {
            topo.outbound
                .iter()
                .find(|o| o.tag == tag)
                .and_then(|o| o.connections)
        };
        assert_eq!(get("dns-out"), Some(4769));
        assert_eq!(get("api"), Some(5374));
    }

    /// 没观察到（核心没跑 / 日志里还没有连接行）必须是 `None`，**不能是
    /// `Some(0)`** —— 界面据此显示「—」而不是 `0`。显示成 0 会让人以为
    /// 「一个连接都没有」，而事实是「不知道」。
    #[test]
    fn unobserved_connection_count_is_none_not_zero() {
        let topo = topo_with_connections(&TrafficRead::default(), &HashMap::new());
        let dns = topo.outbound.iter().find(|o| o.tag == "dns-out").unwrap();
        assert_eq!(dns.connections, None);
    }

    /// **本次要修的核心语义**：查统计失败时接口必须能表达「不可用」，
    /// 而不是只给一个 0 —— 界面拿到的 0 会被画成「流量归零」，正是
    /// 用户说的「数据乱跳」。
    #[test]
    fn missing_stats_are_marked_unavailable_instead_of_silent_zero() {
        let mut counters = MonotonicCounters::new();
        let read = read_traffic(Err("连接 api 入站失败".into()), &mut counters);
        let topo = topo_from(&read);

        assert!(!topo.traffic_ok, "没查到时必须标记为不可信");
        assert_eq!(
            topo.traffic_error.as_deref(),
            Some("取流量失败：连接 api 入站失败")
        );
        // 两个字段表达同一件事，不允许漂移
        assert_eq!(topo.traffic_ok, topo.traffic_error.is_none());
        assert_eq!(topo.counter_resets, 0);
        // 字节字段形状未变，仍是占位 0：正因为如此，界面**必须**看 traffic_ok。
        assert!(topo
            .outbound
            .iter()
            .all(|o| o.uplink_bytes == 0 && o.downlink_bytes == 0));
    }

    /// 查到统计但某个 tag 没有计数器 = **真的是 0**。它与「没查到」是两回事，
    /// 不能因为两者都是 0 就混成一个。
    #[test]
    fn successful_stats_keep_real_zeros_distinct_from_unavailable() {
        let stats = vec![
            exit_stat("inbound>>>tun>>>traffic>>>downlink", 1_000),
            exit_stat("inbound>>>tun>>>traffic>>>uplink", 10),
            exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 2_048),
        ];
        let mut counters = MonotonicCounters::new();
        let topo = topo_from(&read_traffic(Ok(stats), &mut counters));

        assert!(topo.traffic_ok);
        assert_eq!(topo.traffic_error, None);
        let tun = topo.inbound.iter().find(|i| i.tag == "tun").unwrap();
        assert_eq!((tun.uplink_bytes, tun.downlink_bytes), (10, 1_000));
        // 配置里有 socks，计数器里没有：查到了统计，所以这是真的 0
        let socks = topo.inbound.iter().find(|i| i.tag == "socks").unwrap();
        assert_eq!((socks.uplink_bytes, socks.downlink_bytes), (0, 0));
        assert_eq!(out_bytes(&topo, "node-abc"), (0, 2_048));
    }

    /// **lead 指出的 primary 回归**：核心重启（看门狗 stop_core+start_core）
    /// 让计数器归零，拓扑给界面的累计字节不得回退，且必须如实上报「重启过」。
    #[test]
    fn cumulative_bytes_survive_a_core_restart_without_regressing() {
        let mut counters = MonotonicCounters::new();

        let before = vec![exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 8_600_000_000)];
        let first = topo_from(&read_traffic(Ok(before), &mut counters));
        assert_eq!(out_bytes(&first, "node-abc"), (0, 8_600_000_000));
        assert_eq!(first.counter_resets, 0);

        // 重启：计数器从 0 重新计
        let after_restart = vec![exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 0)];
        let second = topo_from(&read_traffic(Ok(after_restart), &mut counters));
        assert!(second.traffic_ok);
        assert_eq!(
            out_bytes(&second, "node-abc"),
            (0, 8_600_000_000),
            "重启不得让累计值归零（那正是用户看到的乱跳）"
        );
        assert_eq!(second.counter_resets, 1, "重启事件要如实上报，不能平滑掩盖");

        // 重启后的新流量继续累加在续接值上
        let grown = vec![exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 4_096)];
        let third = topo_from(&read_traffic(Ok(grown), &mut counters));
        assert_eq!(out_bytes(&third, "node-abc"), (0, 8_600_004_096));
    }

    /// 连续两次重启：每一段重启前的量都要被续接。少加一份就丢真实流量，
    /// 多加一份就凭空上涨 —— 两者都是「数字悄悄错」。
    #[test]
    fn consecutive_restarts_accumulate_each_session() {
        let mut counters = MonotonicCounters::new();
        let mut step = |value: i64| {
            let stats = vec![exit_stat("outbound>>>node-abc>>>traffic>>>uplink", value)];
            topo_from(&read_traffic(Ok(stats), &mut counters))
        };

        assert_eq!(out_bytes(&step(1_000), "node-abc"), (1_000, 0));
        assert_eq!(out_bytes(&step(0), "node-abc"), (1_000, 0));
        assert_eq!(out_bytes(&step(500), "node-abc"), (1_500, 0));
        assert_eq!(out_bytes(&step(3), "node-abc"), (1_503, 0));
        let last = step(10);
        assert_eq!(out_bytes(&last, "node-abc"), (1_510, 0));
        assert_eq!(last.counter_resets, 2);
    }

    /// 一次查询失败不能污染单调化基准：下次查到的值要和**上一次成功采样**
    /// 比，否则失败期间的重启会被漏掉、把归零当成新流量。
    #[test]
    fn a_failed_sample_does_not_reset_the_monotonic_baseline() {
        let mut counters = MonotonicCounters::new();
        let big = vec![exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 9_000)];
        let first = topo_from(&read_traffic(Ok(big), &mut counters));
        assert_eq!(out_bytes(&first, "node-abc"), (0, 9_000));

        // 这一拍没查到：不得更新基准，也不得把读数当 0
        let failed = topo_from(&read_traffic(Err("超时".into()), &mut counters));
        assert!(!failed.traffic_ok);

        // 下一拍查到：核心在这期间重启过，计数器只有 7
        let after = vec![exit_stat("outbound>>>node-abc>>>traffic>>>downlink", 7)];
        let third = topo_from(&read_traffic(Ok(after), &mut counters));
        assert_eq!(out_bytes(&third, "node-abc"), (0, 9_007), "基准仍是 9000");
        assert_eq!(third.counter_resets, 1);
    }
}
