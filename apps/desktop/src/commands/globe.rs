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
use std::time::Duration;

use serde::Serialize;
use tauri::State;

use xt_core::geo_lookup::{merge_sources, parse_api, parse_who, GeoLocation};

use super::*;

/// 地球仪上的一条航线。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeRoute {
    /// 起点（本机公网出口）。
    pub from: GeoLocation,
    /// 终点（出口节点）。
    pub to: GeoLocation,
    /// 这条航线当前承载的实测字节（上行+下行），用于决定飞机密度。
    pub bytes: u64,
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
            let bytes = current_exit_bytes().await;
            Some(GlobeRoute {
                from,
                to,
                bytes,
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

/// 当前出口节点的累计字节（拿不到就返回 0 —— 界面据此显示「无实测流量」）。
async fn current_exit_bytes() -> u64 {
    let addr: std::net::SocketAddr =
        ([127, 0, 0, 1], xt_core::xray::config::API_PORT).into();
    let Ok(stats) = xt_core::xray::query_stats(addr, Duration::from_millis(900)).await else {
        return 0;
    };
    // 出口里流量最大的那个就是节点出站
    xt_core::xray::traffic_by_tag(&stats, "outbound")
        .values()
        .map(|(up, down)| up.saturating_add(*down))
        .max()
        .unwrap_or(0)
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
}
