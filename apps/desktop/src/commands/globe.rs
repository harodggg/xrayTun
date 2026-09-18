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

use xt_core::geo_lookup::{GeoCache, GeoLocation};

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

    let iface_for_blocking = iface.clone();
    let exit_ip_str = exit_ip.map(|ip| ip.to_string());

    // 查询是阻塞 I/O（raw socket + read），放到阻塞线程上，别卡住 async 运行时。
    let outcome = tokio::task::spawn_blocking(move || {
        let mut cache = GeoCache::default();
        let mut err: Option<String> = None;

        let origin = match xt_core::geo_lookup::lookup_self(iface_for_blocking.as_deref()) {
            Ok(loc) => Some(loc),
            Err(e) => {
                err = Some(format!("查本机位置失败：{e}"));
                None
            }
        };

        let exit = match &exit_ip_str {
            Some(ip) => match cache.get_or_lookup(ip, ip, iface_for_blocking.as_deref()) {
                Ok(loc) => Some(loc),
                Err(e) => {
                    err = Some(format!("查节点位置失败：{e}"));
                    None
                }
            },
            None => None,
        };
        (origin, exit, err)
    })
    .await
    .map_err(|e| format!("查询任务失败：{e}"))?;

    let (origin, exit, error) = outcome;
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

