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
}

/// 地球仪数据。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeData {
    pub route: Option<GlobeRoute>,
    /// 本机位置（即使没有出口也会尽量给出）。
    pub origin: Option<GeoLocation>,
    /// 拿不到位置时的原因，界面如实展示。
    pub error: Option<String>,
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
                .map(|n| (n.name.clone(), n.address.clone()))
        })
    });
    let node = node.flatten();

    // 节点地址解析成 IP（域名取第一个）
    let exit_ip: Option<IpAddr> = node.as_ref().and_then(|(_, addr)| {
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

    let self_task = tokio::spawn(query_self(iface.clone()));
    let exit_task = {
        let ip = exit_ip_str.clone();
        let iface = iface.clone();
        tokio::spawn(async move {
            match ip {
                Some(ip) => query_ip(&ip, iface.as_deref()).await,
                None => None,
            }
        })
    };

    let origin = self_task.await.ok().flatten();
    let exit = exit_task.await.ok().flatten();
    let error = if origin.is_none() {
        Some("查本机位置失败（两个数据源都没返回）".to_string())
    } else if exit_ip_str.is_some() && exit.is_none() {
        Some("查节点位置失败（两个数据源都没返回）".to_string())
    } else {
        None
    };
    let route = match (origin.clone(), exit) {
        (Some(from), Some(to)) => {
            // 实测流量：取当前节点的累计字节，让飞机密度有依据
            let traffic = current_exit_traffic().await;
            Some(GlobeRoute {
                from,
                to,
                bytes: traffic.bytes,
                traffic_ok: traffic.ok,
                counter_resets: traffic.resets,
                node_name: node.map(|(n, _)| n).unwrap_or_else(|| "节点".into()),
            })
        }
        _ => None,
    };

    Ok(GlobeData {
        route,
        origin,
        error,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExitTraffic {
    bytes: u64,
    ok: bool,
    /// 观察到的计数器归零次数（核心重启）。
    resets: u32,
}

impl ExitTraffic {
    /// 这次没查到：`bytes` 是占位 0，调用方必须看 `ok`。
    fn unavailable() -> Self {
        Self { bytes: 0, ok: false, resets: 0 }
    }
}

/// 从一次采样里算出口累计字节 —— 纯函数，便于测试（这一段的错法都是
/// 「数字悄悄不对」）。
///
/// 出口里流量最大的那个就是节点出站。累计值跨核心重启保持单调，
/// 见 [`MonotonicCounters`]。
fn read_exit_traffic(
    stats: Option<&[StatEntry]>,
    counters: &mut MonotonicCounters,
) -> ExitTraffic {
    let Some(stats) = stats else {
        return ExitTraffic::unavailable();
    };
    let (by_tag, resets) = monotonic_traffic_by_tag(counters, stats, "outbound");
    let bytes = by_tag
        .values()
        .map(|(up, down)| up.saturating_add(*down))
        .max()
        .unwrap_or(0);
    ExitTraffic { bytes, ok: true, resets }
}

/// 进程内的出口累计读数表：把「核心重启后计数器归零」补偿掉，见 topology.rs。
static EXIT_COUNTERS: OnceLock<Mutex<MonotonicCounters>> = OnceLock::new();

/// 当前出口节点的累计字节。查不到时 `ok=false`（而不是把 0 当读数）。
///
/// 锁包住查询：单调化依赖「采样顺序 = 观察顺序」，理由同 topology.rs。
async fn current_exit_traffic() -> ExitTraffic {
    let addr: std::net::SocketAddr =
        ([127, 0, 0, 1], xt_core::xray::config::API_PORT).into();
    let mut counters = EXIT_COUNTERS
        .get_or_init(|| Mutex::new(MonotonicCounters::new()))
        .lock()
        .await;
    let stats = xt_core::xray::query_stats(addr, Duration::from_millis(900))
        .await
        .ok();
    read_exit_traffic(stats.as_deref(), &mut counters)
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
        let t = read_exit_traffic(None, &mut MonotonicCounters::new());
        assert!(!t.ok, "查不到必须标记为不可用");
        assert_eq!(t.bytes, 0);
        assert_eq!(t.resets, 0);
    }

    /// **primary 回归**：核心重启让计数器归零，地球仪的累计字节不得回退。
    #[test]
    fn exit_bytes_do_not_regress_across_a_core_restart() {
        let mut counters = MonotonicCounters::new();
        let big = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 8_600_000_000,
        }];
        let first = read_exit_traffic(Some(&big), &mut counters);
        assert!(first.ok);
        assert_eq!(first.bytes, 8_600_000_000);
        assert_eq!(first.resets, 0);

        // 核心重启：计数器从 0 重新计
        let reset = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 0,
        }];
        let second = read_exit_traffic(Some(&reset), &mut counters);
        assert!(second.ok);
        assert_eq!(second.bytes, 8_600_000_000, "重启不得让累计值归零");
        assert_eq!(second.resets, 1);

        // 重启后的新流量继续累加
        let grown = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 1_024,
        }];
        assert_eq!(read_exit_traffic(Some(&grown), &mut counters).bytes, 8_600_001_024);
    }

    /// 出口取「上下行合计最大的那个」；畸形名字不产生假值。
    #[test]
    fn busiest_outbound_wins_and_junk_names_produce_no_bytes() {
        let stats = vec![
            StatEntry { name: "outbound>>>direct>>>traffic>>>downlink".into(), value: 100 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 900 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>uplink".into(), value: 900 },
            // 畸形：不参与
            StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 },
            // 入站计数不能混进出口
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 10_000 },
        ];
        let t = read_exit_traffic(Some(&stats), &mut MonotonicCounters::new());
        assert!(t.ok);
        assert_eq!(t.bytes, 1_800, "取上下行合计最大的出口");

        // 查到了统计但没有任何合法出口计数 = 真的 0（而不是「没查到」）
        let junk = vec![StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 }];
        let none = read_exit_traffic(Some(&junk), &mut MonotonicCounters::new());
        assert!(none.ok, "查到统计时 ok 必须为真");
        assert_eq!(none.bytes, 0);
    }
}
