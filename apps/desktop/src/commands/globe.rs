//! 地球仪数据：本机与出口节点的地理位置。
//!
//! # 一个必须说清的边界
//!
//! **位置来自 ip-api.com**，不是本地算出来的 —— 项目自带的 `geoip.dat` 只有
//! 国别与网段，没有经纬度（实测确认）。把节点 IP 发给第三方是个真实的代价，
//! 所以：结果按 IP 缓存、界面上标注来源、查询失败时如实报错而不是画一个
//! 坐标 (0,0) 的假点。
//!
//! **查询必须绕过隧道**：隧道开着时直接发请求会从节点出去，查到的会是
//! 「节点自己的位置」—— 错得很像对的。详见 `xt_core::geo_lookup`。

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;

use xt_core::geo_lookup::{merge_sources, parse_api, parse_who, GeoLocation};
use xt_core::xray::stats::{monotonic_traffic_by_tag, MonotonicCounters, StatEntry};

use super::*;

/// 地球仪上的一条航线。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeRoute {
    /// 起点（本机公网出口）。
    pub from: GeoLocation,
    /// 终点（出口节点）。
    pub to: GeoLocation,
    /// 这条航线当前承载的实测字节（上行+下行），用于决定飞机密度。
    ///
    /// **跨核心重启保持单调**；`traffic_ok == false` 时固定为 0，
    /// **不是**真实读数（此前正是这里把「没查到」显示成了 0）。
    pub bytes: u64,
    /// 这次到底查没查到流量。
    pub traffic_ok: bool,
    /// 累计值跨重启续接时补偿掉的归零次数（`>0` = 核心重启过）。
    pub counter_resets: u32,
    /// 出口节点的名字。
    pub node_name: String,
    /// 这条航线的流量**归属**（task-179 / A20）。
    pub traffic: TrafficProvenance,
}

/// 出口累计流量的**归属**（task-179 / A20）。
///
/// 旧实现取「所有出站里 up+down 最大的那个」当出口，注释自己写着
/// 「最大的那个就是节点出站」—— **那是假设，不是验证**：真凶可能是 `direct`，
/// 而用户会得出「我的节点扛了 8 GiB」这种数据结论。
///
/// 现在只认**具体 tag**：拿不到就如实 `unattributed`（`verified=false` + `reason` 必填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrafficProvenance {
    /// 这个数值**真正来自哪个** outbound tag（未归属 = `None`）。
    pub tag: Option<String>,
    /// 该 tag 是不是**节点出站**（`node-*`）——不是的话界面不许说「我的节点」。
    pub is_node_outbound: bool,
    /// 归属是否**已验证**（拿到了具体 tag 且统计可用）。
    pub verified: bool,
    /// `verified = false` 时必填且具体。
    pub reason: Option<String>,
}

impl TrafficProvenance {
    /// 有具体 tag ⇒ 已验证。
    fn verified_for(tag: &str) -> Self {
        Self {
            tag: Some(tag.to_string()),
            is_node_outbound: tag.starts_with("node-"),
            verified: true,
            reason: None,
        }
    }

    /// 归不到任何 tag ⇒ 如实说「未归属」，并给出**具体**原因。
    fn unattributed(reason: impl Into<String>) -> Self {
        Self {
            tag: None,
            is_node_outbound: false,
            verified: false,
            reason: Some(reason.into()),
        }
    }
}

/// 「本机 · <IP>」这条陈述的**可验证来源**（task-179 / A21）。
///
/// 旧实现只在**读到**物理网卡时才加 `curl --interface`；读不到就走系统默认路由
/// —— **隧道开着时那就是从节点出去**，服务看到的是节点出口，而界面仍写「本机」。
/// 那是「陈述比事实强」，方向上还是隐私判断。
///
/// 现在把「来不来自本机」做成字段：只有**绑了物理网卡且拿到了位置**才 `trusted = true`；
/// 否则 `trusted = false` **且 `reason` 必填**（界面据此降级文案）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfCheck {
    /// 服务看到的那一个出口 IP（没问到 = `None`）。
    pub ip: Option<String>,
    /// **实际**绑定的物理网卡 —— 只有可信的那次查询才有值。
    pub bound_interface: Option<String>,
    /// 只有「绑了物理网卡 + 拿到了位置」才为 true。
    pub trusted: bool,
    /// `trusted = false` 时必填且具体。
    pub reason: Option<String>,
}

impl SelfCheck {
    /// 判据（**纯函数**：`字段 = X ⇒ 呈现 = Y` 的后端侧断言就钉在这里）。
    fn judge(iface: Option<&str>, origin: Option<&GeoLocation>) -> Self {
        match (iface, origin) {
            (Some(iface), Some(loc)) => Self {
                ip: Some(loc.ip.clone()),
                bound_interface: Some(iface.to_string()),
                trusted: true,
                reason: None,
            },
            (Some(iface), None) => Self {
                ip: None,
                bound_interface: None,
                trusted: false,
                reason: Some(format!(
                    "绑定 {iface} 的查询没有成功（两个数据源都没返回或绑卡失败）⇒ 本机位置未验证"
                )),
            },
            (None, Some(loc)) => Self {
                ip: Some(loc.ip.clone()),
                bound_interface: None,
                trusted: false,
                reason: Some(
                    "读不到物理默认路由 ⇒ 查询走的是系统默认路由；隧道开着时那就是节点出口，\
                     查到的不是本机"
                        .to_string(),
                ),
            },
            (None, None) => Self {
                ip: None,
                bound_interface: None,
                trusted: false,
                reason: Some(
                    "读不到物理默认路由，且两个数据源都没返回 ⇒ 本机位置未知".to_string(),
                ),
            },
        }
    }
}

/// 地球仪数据。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeData {
    pub route: Option<GlobeRoute>,
    /// 本机位置（即使没有出口也会尽量给出）。
    pub origin: Option<GeoLocation>,
    /// 拿不到位置时的原因，界面如实展示。
    pub error: Option<String>,
    /// 「本机 · IP」这条陈述的**可验证来源**（task-179 / A21）。
    pub self_check: SelfCheck,
}

/// 取物理网卡名 —— 绑它才能绕过隧道，否则查询会从节点出去、查到节点的位置。
///
/// 隧道开着时默认路由就是 utun，所以这里必须读**物理**默认路由；
/// `default_route` 返回的是系统当前的默认路由，正是我们要绑的那个接口。
fn physical_interface() -> Option<String> {
    xt_tun::macos::route::default_route()
        .ok()
        .map(|d| d.interface)
}

/// 节点地址是否是私网/保留地址（这类地址没有公网位置，不该去查）。
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// 地球仪：本机 → 出口节点。
#[tauri::command]
pub async fn globe_data(state: State<'_, AppState>) -> Result<GlobeData, String> {
    let iface = physical_interface();

    // 当前选中的节点
    let node = state.with(|i| {
        let selected = i.settings.selected_node.clone();
        selected.and_then(|id| {
            i.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| (n.name.clone(), n.address.clone(), n.outbound_tag()))
        })
    });
    let node = node.flatten();

    // 节点地址解析成 IP（域名取第一个）
    let exit_ip: Option<IpAddr> = node.as_ref().and_then(|(_, addr, _)| {
        xt_core::net::resolve_host(addr)
            .into_iter()
            .find(|ip| !is_private(*ip))
    });

    // 两个数据源**并发**查询、互为校验。用系统的 curl（项目既有做法：
    // 零依赖、走系统信任链、支持 `--interface` 绑网卡），不引 HTTP 库。
    //
    // 绑网卡是必须的：隧道开着时直接请求会从节点出去，查到的是**节点自己的
    // 位置**（实测过：不绑查到香港、绑了查到本机所在的大理）。
    let exit_ip_str = exit_ip.map(|ip| ip.to_string());

    // **统一走 `tauri::async_runtime::spawn`**：与 `tokio::spawn` 同样可 `.await`
    // （`tauri::async_runtime::JoinHandle` 实现了 `Future`），但同步/异步上下文都能用。
    // 裸 `tokio::spawn` 在同步上下文（`setup` 回调）会 panic ⇒ release 下 SIGABRT（0.8.39 事故）。
    let self_task = tauri::async_runtime::spawn(query_self(iface.clone()));
    let exit_task = {
        let ip = exit_ip_str.clone();
        let iface = iface.clone();
        tauri::async_runtime::spawn(async move {
            match ip {
                Some(ip) => query_ip(&ip, iface.as_deref()).await,
                None => None,
            }
        })
    };

    let origin = self_task.await.ok().flatten();
    let exit = exit_task.await.ok().flatten();
    // task-179 / A21：把「来不来自本机」判出来（纯函数，见 `SelfCheck::judge`）——
    // 读不到物理网卡时查到的是**节点出口**，界面据此降级文案，不许再说「本机」。
    let self_check = SelfCheck::judge(iface.as_deref(), origin.as_ref());
    let error = if origin.is_none() {
        Some("查本机位置失败（两个数据源都没返回）".to_string())
    } else if exit_ip_str.is_some() && exit.is_none() {
        Some("查节点位置失败（两个数据源都没返回）".to_string())
    } else {
        None
    };
    let route = match (origin.clone(), exit) {
        (Some(from), Some(to)) => {
            // 实测流量：**只认该节点的 outbound tag**（task-179 / A20 —— 不再取最大）
            let node_tag = node.as_ref().map(|(_, _, tag)| tag.clone());
            let traffic = current_exit_traffic(node_tag.as_deref()).await;
            Some(GlobeRoute {
                from,
                to,
                bytes: traffic.bytes,
                traffic_ok: traffic.ok,
                counter_resets: traffic.resets,
                node_name: node.map(|(n, _, _)| n).unwrap_or_else(|| "节点".into()),
                traffic: traffic.provenance.clone(),
            })
        }
        _ => None,
    };

    Ok(GlobeData {
        route,
        origin,
        error,
        self_check,
    })
}

/// 用系统 curl 发一次 GET。`interface` 为 `Some` 时绑到该网卡（绕过隧道）。
///
/// 用 curl 而不是自写 socket：https 需要 TLS，而项目已有既定做法
/// （见 `commands/nodes.rs` 的订阅拉取）—— 零依赖、走系统信任链。
async fn curl_get(url: &str, interface: Option<&str>) -> Option<String> {
    let mut cmd = tokio::process::Command::new("/usr/bin/curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "8",
        "--user-agent",
        "XrayTun/location",
    ]);
    if let Some(iface) = interface {
        cmd.args(["--interface", iface]);
    }
    let out = cmd.arg(url).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 查一个指定 IP 的位置（双源）。
async fn query_ip(ip: &str, interface: Option<&str>) -> Option<GeoLocation> {
    let (who_url, api_url) = (
        format!("https://ipwho.is/{ip}"),
        format!("http://ip-api.com/json/{ip}?fields=status,message,country,city,lat,lon,isp&lang=zh-CN"),
    );
    let (who, api) = tokio::join!(
        curl_get(&who_url, interface),
        curl_get(&api_url, interface),
    );
    merge_sources(
        ip,
        who.and_then(|b| parse_who(&b, ip)),
        api.and_then(|b| parse_api(&b, ip)),
    )
}

/// 查**本机**的公网位置（双源）。服务自己看到的是发起请求的出口地址，
/// 所以 url 里不带 IP。
async fn query_self(interface: Option<String>) -> Option<GeoLocation> {
    let (who, api) = tokio::join!(
        curl_get("https://ipwho.is/", interface.as_deref()),
        curl_get(
            "http://ip-api.com/json/?fields=status,message,country,city,lat,lon,isp,query&lang=zh-CN",
            interface.as_deref(),
        ),
    );
    merge_sources(
        "",
        who.and_then(|b| parse_who(&b, "")),
        api.and_then(|b| parse_api(&b, "")),
    )
}

/// 出口累计字节的读取结果：把「值」与「这次是否查到」分开表达。
///
/// **不用 0 表示「没查到」**：0 是合法读数（真的没有流量），混在一起会让
/// 地球仪在查询失败时显示「出口累计 0 B」——那是假读数。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitTraffic {
    bytes: u64,
    ok: bool,
    /// 观察到的计数器归零次数（核心重启）。
    resets: u32,
    /// 这个数字的**归属**（task-179 / A20）：来自哪个 tag、可不可信。
    provenance: TrafficProvenance,
}

impl ExitTraffic {
    /// 这次没查到 / 归不到：`bytes` 是占位 0、`provenance` 明确「未归属 + 原因」。
    fn unattributed(reason: impl Into<String>) -> Self {
        Self {
            bytes: 0,
            ok: false,
            resets: 0,
            provenance: TrafficProvenance::unattributed(reason),
        }
    }
}

/// 从一次采样里算**该节点出站**的累计字节 —— 纯函数，便于测试
/// （这一段的错法都是「数字悄悄不对」）。
///
/// # task-179 / A20：**不再用「取最大」当归属**
///
/// 旧实现取「所有出站里 up+down 最大的那个」，注释写着「最大的那个就是节点出站」
/// —— 那是**假设**：真凶可能是 `direct`，用户却会得出「我的节点扛了 8 GiB」。
/// 现在只认**具体 tag**（`node-<id>`）；拿不到就给「未归属」+ 具体原因。
/// 累计值跨核心重启保持单调，见 [`MonotonicCounters`]。
fn read_exit_traffic(
    stats: Option<&[StatEntry]>,
    counters: &mut MonotonicCounters,
    node_tag: Option<&str>,
) -> ExitTraffic {
    let Some(node_tag) = node_tag else {
        return ExitTraffic::unattributed("没有选中的节点 ⇒ 归不到任何 outbound（不挑「最大的」顶上）");
    };
    let Some(stats) = stats else {
        return ExitTraffic::unattributed("查统计失败（核心没在跑或 API 不可达）⇒ 归属未验证");
    };
    let (by_tag, resets) = monotonic_traffic_by_tag(counters, stats, "outbound");
    // **只认这个 tag**：别的出站（例如 direct）流量再大也不算它的
    let (up, down) = by_tag.get(node_tag).copied().unwrap_or((0, 0));
    ExitTraffic {
        bytes: up.saturating_add(down),
        ok: true,
        resets,
        provenance: TrafficProvenance::verified_for(node_tag),
    }
}

/// 进程内的出口累计读数表：把「核心重启后计数器归零」补偿掉，见 topology.rs。
static EXIT_COUNTERS: OnceLock<Mutex<MonotonicCounters>> = OnceLock::new();

/// 当前出口节点的累计字节。查不到时 `ok=false`（而不是把 0 当读数）。
///
/// `node_tag` 来自选中节点的 [`xt_core::model::Node::outbound_tag`]；拿不到就如实「未归属」。
///
/// 锁包住查询：单调化依赖「采样顺序 = 观察顺序」，理由同 topology.rs。
async fn current_exit_traffic(node_tag: Option<&str>) -> ExitTraffic {
    let addr: std::net::SocketAddr =
        ([127, 0, 0, 1], xt_core::xray::config::API_PORT).into();
    let mut counters = EXIT_COUNTERS
        .get_or_init(|| Mutex::new(MonotonicCounters::new()))
        .lock()
        .await;
    let stats = xt_core::xray::query_stats(addr, Duration::from_millis(900))
        .await
        .ok();
    read_exit_traffic(stats.as_deref(), &mut counters, node_tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **真实网络**的双源查询验证。
    ///
    /// 默认不跑（CI 不该依赖外网），需要时手动：
    ///
    /// ```bash
    /// cargo test -p xraytun-desktop --lib globe -- --ignored --nocapture
    /// ```
    ///
    /// 它存在的价值：parse/merge 的单测证明不了「curl 那条路真的通」——
    /// 绑网卡的参数、url 形状、两个源的实际响应字段，只有真发一次才知道。
    #[tokio::test]
    #[ignore = "需要网络"]
    async fn real_lookup_returns_a_plausible_location() {
        let iface = physical_interface();
        println!("网卡 = {iface:?}");

        let origin = query_self(iface.clone()).await.expect("本机位置应当查得到");
        println!(
            "本机: {} {} ({:.4}, {:.4}) 来源={} 一致={} {:?}",
            origin.city, origin.country, origin.lat, origin.lon,
            origin.source, origin.consistent, origin.sources
        );
        assert!(!origin.ip.is_empty(), "应当拿到公网 IP");
        assert!(origin.lat.abs() <= 90.0 && origin.lon.abs() <= 180.0);
        assert_eq!(origin.sources.len(), 2, "两个源都应当有结果");

        let node = query_ip("45.207.197.185", iface.as_deref()).await.expect("节点位置应当查得到");
        println!(
            "节点: {} {} ({:.4}, {:.4}) 来源={} 一致={}",
            node.city, node.country, node.lat, node.lon, node.source, node.consistent
        );
        // 节点在香港（实测），纬度应当在 22 附近
        assert!((node.lat - 22.3).abs() < 2.0, "节点纬度应当在香港附近，实际 {}", node.lat);
    }

    /// 查不到流量时必须标记为不可用：把 0 当实测值会让地球仪显示
    /// 「出口累计 0 B」——那是一次查询失败，不是真的没有流量。
    #[test]
    fn unavailable_stats_are_flagged_rather_than_zero() {
        let t = read_exit_traffic(None, &mut MonotonicCounters::new(), Some("node-a"));
        assert!(!t.ok, "查不到必须标记为不可用");
        assert_eq!(t.bytes, 0);
        assert_eq!(t.resets, 0);
        assert!(!t.provenance.verified, "查不到时归属也不许标成「已验证」");
        assert!(
            t.provenance.reason.as_deref().is_some_and(|r| !r.is_empty()),
            "未验证必须给具体原因：{:?}",
            t.provenance
        );
    }

    /// **primary 回归**：核心重启让计数器归零，地球仪的累计字节不得回退。
    #[test]
    fn exit_bytes_do_not_regress_across_a_core_restart() {
        let mut counters = MonotonicCounters::new();
        let big = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 8_600_000_000,
        }];
        let first = read_exit_traffic(Some(&big), &mut counters, Some("node-a"));
        assert!(first.ok);
        assert_eq!(first.bytes, 8_600_000_000);
        assert_eq!(first.resets, 0);

        // 核心重启：计数器从 0 重新计
        let reset = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 0,
        }];
        let second = read_exit_traffic(Some(&reset), &mut counters, Some("node-a"));
        assert!(second.ok);
        assert_eq!(second.bytes, 8_600_000_000, "重启不得让累计值归零");
        assert_eq!(second.resets, 1);

        // 重启后的新流量继续累加
        let grown = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 1_024,
        }];
        assert_eq!(
            read_exit_traffic(Some(&grown), &mut counters, Some("node-a")).bytes,
            8_600_001_024
        );
    }

    /// **task-179 / A20 主回归**：归属**只认具体 tag**，不是「上下行合计最大的那个」。
    ///
    /// 构造一个「`direct` 明显比节点出站大」的世界：旧实现（取最大）会报 direct 的
    /// 18,000，新实现必须报**节点 tag 的** 1,800（并说明 tag 与是否节点出站）。
    #[test]
    fn attribution_follows_the_node_tag_not_the_busiest_outbound() {
        let stats = vec![
            // direct 更大 —— 真凶可能是它，但**不许**因此算到节点头上
            StatEntry { name: "outbound>>>direct>>>traffic>>>downlink".into(), value: 9_000 },
            StatEntry { name: "outbound>>>direct>>>traffic>>>uplink".into(), value: 9_000 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 900 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>uplink".into(), value: 900 },
            // 畸形：不参与
            StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 },
            // 入站计数不能混进出口
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 10_000 },
        ];
        let t = read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), Some("node-a"));
        assert!(t.ok);
        assert_eq!(t.bytes, 1_800, "必须报**节点 tag** 的合计，而不是最大的 direct");
        assert_eq!(t.provenance.tag.as_deref(), Some("node-a"));
        assert!(t.provenance.verified, "拿到了具体 tag ⇒ 归属已验证");
        assert!(t.provenance.is_node_outbound, "`node-*` 是节点出站");
        assert!(t.provenance.reason.is_none(), "已验证时不该有原因");

        // 查到统计、但该 tag 没有计数 = 真的 0（不是「没查到」），且**归属仍是该 tag**
        let junk = vec![StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 }];
        let none = read_exit_traffic(Some(&junk), &mut MonotonicCounters::new(), Some("node-a"));
        assert!(none.ok, "查到统计时 ok 必须为真");
        assert_eq!(none.bytes, 0);
        assert_eq!(none.provenance.tag.as_deref(), Some("node-a"));
        assert!(none.provenance.verified);

        // 如果归属真的是 `direct`（例如直连规则在跑），**不许**把 `direct` 说成节点出站
        let direct =
            read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), Some("direct"));
        assert_eq!(direct.bytes, 18_000, "tag=direct 时报的就是 direct 的数字");
        assert_eq!(direct.provenance.tag.as_deref(), Some("direct"));
        assert!(!direct.provenance.is_node_outbound, "`direct` 不是节点出站");
        assert!(direct.provenance.verified, "拿到了具体 tag 仍算已验证归属");
    }

    /// **归不到 tag ⇒ 明确 unattributed**（不许挑一个顶上、也不许报数字）。
    #[test]
    fn without_a_node_tag_traffic_is_explicitly_unattributed() {
        let stats = vec![StatEntry {
            name: "outbound>>>direct>>>traffic>>>downlink".into(),
            value: 9_000,
        }];
        let t = read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), None);
        assert!(!t.ok);
        assert_eq!(t.bytes, 0, "未归属时不许报任何数字（哪怕是最大的那个）");
        assert!(!t.provenance.verified);
        assert_eq!(t.provenance.tag, None);
        assert!(!t.provenance.is_node_outbound);
        assert!(t
            .provenance
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("节点")));
    }

    // -----------------------------------------------------------------------
    // task-179 / A21：`SelfCheck` 的「字段 = X ⇒ 呈现 = Y」后端侧断言
    // -----------------------------------------------------------------------

    fn loc(ip: &str) -> GeoLocation {
        GeoLocation {
            ip: ip.into(),
            country: "中国".into(),
            city: "大理".into(),
            lat: 25.6,
            lon: 100.2,
            isp: "电信".into(),
            source: "ipwho.is".into(),
            consistent: true,
            sources: vec![],
        }
    }

    /// 只有「**绑了物理网卡 + 拿到了位置**」才可信；任何一边缺 ⇒ `trusted=false` 且原因具体。
    #[test]
    fn self_check_is_trusted_only_when_the_query_was_bound_and_answered() {
        let trusted = SelfCheck::judge(Some("en0"), Some(&loc("203.0.113.10")));
        assert!(trusted.trusted);
        assert_eq!(trusted.bound_interface.as_deref(), Some("en0"));
        assert_eq!(trusted.ip.as_deref(), Some("203.0.113.10"));
        assert!(trusted.reason.is_none());

        // 读不到物理网卡 ⇒ 走系统默认路由；隧道开着时查到的是**节点出口** ⇒ 不许说「本机」
        let unbound = SelfCheck::judge(None, Some(&loc("203.0.113.10")));
        assert!(!unbound.trusted, "没绑卡时查到的可能是节点出口，不许说「本机」");
        assert_eq!(unbound.bound_interface, None);
        assert!(unbound
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("节点出口")));

        // 绑了却没答 ⇒ 未验证（原因里点名是哪张网卡）
        let no_answer = SelfCheck::judge(Some("en0"), None);
        assert!(!no_answer.trusted);
        assert!(no_answer
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("en0")));

        let nothing = SelfCheck::judge(None, None);
        assert!(!nothing.trusted && nothing.ip.is_none());
        assert!(nothing.reason.as_deref().is_some_and(|r| !r.is_empty()));
    }

    /// **不变量**（界面据此判「能不能说『本机』」）：
    /// `trusted = true` ⇔ 「绑了网卡 && 有 IP && 没写原因」；`trusted = false` ⇒ 原因非空。
    /// 任何一边破掉，这条测试就红 —— 双向敏感性就钉在这里。
    #[test]
    fn trusted_iff_bound_interface_and_ip_and_no_reason() {
        for (iface, origin) in [
            (Some("en0"), Some(loc("203.0.113.10"))),
            (None, Some(loc("203.0.113.10"))),
            (Some("en0"), None),
            (None, None),
        ] {
            let sc = SelfCheck::judge(iface, origin.as_ref());
            assert_eq!(
                sc.trusted,
                sc.bound_interface.is_some() && sc.ip.is_some() && sc.reason.is_none(),
                "trusted 必须等价于「绑了网卡 + 有 IP + 没原因」：{sc:?}"
            );
            if !sc.trusted {
                assert!(
                    sc.reason.as_deref().is_some_and(|r| !r.is_empty()),
                    "`trusted=false` 必须给具体原因（不许留空）：{sc:?}"
                );
            }
        }
    }
}
