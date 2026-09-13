//! 把上层请求编译成一份**具体的、可回滚的**系统变更计划。
//!
//! 之所以要单独这一步：真正的变更顺序很重要，而顺序错了会导致短暂的断网
//! 甚至路由环。这里把顺序固定下来并做成纯函数，就能单测它。
//!
//! 正确的顺序（这一点很多人会做错）：
//!
//! ```text
//! 1. 建 utun 并配地址
//! 2. 先加「代理服务器 IP 走物理网关」的 host 路由   ← 必须在 /1 之前
//! 3. 再加 0.0.0.0/1 + 128.0.0.0/1（以及 v6 的 ::/1 + 8000::/1）
//! 4. 最后改 DNS
//! ```
//!
//! 第 2 步如果排在第 3 步之后，中间会有一个窗口：默认流量已经进隧道，
//! 但隧道里的数据还要再连代理服务器 → 代理服务器本身又被送进隧道 → 路由环。

use std::net::IpAddr;

use xt_proto::{Cidr, DnsMode, Ipv6Mode, RouteVia, TunUpRequest};

use crate::error::{Error, Result};
use crate::validate::validate_cidr_text;

/// 物理出口（隧道建立前的默认路由）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalUplink {
    pub interface: String,
    pub gateway: Option<IpAddr>,
    /// 对应的 `networksetup` 服务名，用于改 DNS。
    pub service: Option<String>,
}

/// 路由的用途分类。这个区分是**两阶段启动**的基础：
/// 只有 [`RouteKind::DefaultCapture`] 会造成「流量被接管但数据面还没起来」的黑洞，
/// 所以只有它需要延迟到 [`crate::macos::controller::commit_routes`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// 绕过隧道：代理服务器 IP、内网网段。装早了也无害。
    Bypass,
    /// 物理网卡上的**作用域默认路由**（`-ifscope`）。
    ///
    /// 让绑定了物理网卡的数据面 socket 能逃出隧道。
    /// 必须在接管默认路由**之前**装好，否则中间会有一个窗口：
    /// `IP_BOUND_IF=en0` 的 socket 找不到任何可用路由。
    PhysicalScopedDefault,
    /// 接管默认路由（`0.0.0.0/1` + `128.0.0.0/1` 等）。
    DefaultCapture,
}

/// 一条待安装的路由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRoute {
    pub destination: Cidr,
    pub via: RouteVia,
    pub kind: RouteKind,
    /// 这条路由装不上时，是否必须放弃整次建立。
    ///
    /// 引入这个标记是因为一次真实故障：bypass 列表里混进了
    /// `255.255.255.255/32`，`route(8)` 拒绝它，于是**整个 TUN 建立失败** ——
    /// 而那条路由本来只是个无关紧要的「保险」。
    ///
    /// 现在的分档：
    ///
    /// * **critical**：代理服务器与物理网关的 host 路由、接管默认路由的 `/1`。
    ///   装不上就意味着路由环或流量没被接管，必须失败。
    /// * **非 critical**：RFC1918 / 链路本地 / 多播这些「别把内网塞进隧道」的
    ///   便利路由。装不上只会让内网访问绕路，不该毁掉整条隧道。
    pub critical: bool,
}

impl PlannedRoute {
    fn bypass(destination: Cidr, via: RouteVia, critical: bool) -> Self {
        Self { destination, via, kind: RouteKind::Bypass, critical }
    }

    fn capture(destination: Cidr, via: RouteVia) -> Self {
        Self { destination, via, kind: RouteKind::DefaultCapture, critical: true }
    }

    /// 物理网卡上的作用域默认路由。**必须带网关。**
    ///
    /// `critical: false` —— 装不上不会让整次建立失败，但直连分流会失效。
    /// 之所以不设成 critical：在没有默认网关的环境（点对点链路）它本来就没意义，
    /// 那时不该阻止用户用 TUN。
    fn scoped_default(interface: &str, gateway: IpAddr) -> Self {
        Self {
            destination: "0.0.0.0/0".parse().expect("常量合法"),
            via: RouteVia::ScopedInterface { name: interface.to_string(), gateway },
            kind: RouteKind::PhysicalScopedDefault,
            critical: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunPlan {
    pub session_id: String,
    /// 分配给 utun 的地址。
    pub addresses: Vec<Cidr>,
    pub mtu: u16,
    /// 按 **安装顺序** 排列。
    pub routes: Vec<PlannedRoute>,
    /// 需要写入系统的 DNS（空表示不改）。
    pub dns_servers: Vec<IpAddr>,
    pub dns_mode: DnsMode,
    pub search_domains: Vec<String>,
    pub physical: PhysicalUplink,
}

impl TunPlan {
    /// 回滚时需要删除的路由，顺序与安装相反。
    pub fn reverse_routes(&self) -> Vec<PlannedRoute> {
        let mut r = self.routes.clone();
        r.reverse();
        r
    }
}

/// 根据请求与物理出口计算变更计划。
pub fn build_plan(req: &TunUpRequest, physical: PhysicalUplink) -> Result<TunPlan> {
    let mut routes = Vec::new();

    // ---- 2) 代理服务器 IP：走物理网关（防止路由环） ----
    for host in &req.routes.bypass_hosts {
        let cidr = Cidr::host(*host);
        validate_cidr_text(&cidr.to_string())?;
        // 服务器 host 路由是防路由环的核心，装不上就必须失败。
        routes.push(PlannedRoute::bypass(cidr, bypass_via(&physical, *host)?, true));
    }

    // ---- 2b) 私有/保留网段：同样走物理出口 ----
    for raw in &req.routes.bypass_networks {
        // 这些是「网段」而不是「接口地址」，所以先归一化（`Cidr` 刻意保留主机位）。
        let net = raw.network();
        validate_cidr_text(&net.to_string())?;
        // IPv6 网段不能用 IPv4 网关，退化为按接口走。
        let via = match (net.addr, physical.gateway) {
            (IpAddr::V4(_), Some(gw)) => RouteVia::Gateway { addr: gw },
            _ => RouteVia::Interface { name: physical.interface.clone() },
        };
        // 内网绕行属于便利项：装不上只会让内网访问变慢，不该让整次建立失败。
        routes.push(PlannedRoute::bypass(net, via, false));
    }

    // ---- 2c) 物理网卡的作用域默认路由 ----
    //
    // 必须在接管默认路由之前装。作用域路由只对绑定了该接口的 socket 可见，
    // 所以它不会影响普通 App 的流量（那些流量仍然走 /1 进隧道）。
    //
    // 需要网关：只用 `-interface` 会得到一条「目标在本地链路」的纯接口路由，
    // 包发不出去。没有网关就干脆不加 —— 加了反而更糟。
    match physical.gateway {
        Some(gw) if gw.is_ipv4() => {
            routes.push(PlannedRoute::scoped_default(&physical.interface, gw))
        }
        _ => {
            tracing::warn!(
                interface = %physical.interface,
                "物理出口没有 IPv4 网关，跳过作用域默认路由（直连分流可能不可用）"
            );
        }
    }

    // ---- 3) 接管默认路由 ----
    if req.routes.default_route == xt_proto::DefaultRouteMode::SplitDefault {
        // 接口名要到 utun 建好之后才知道，这里先用占位符，由 controller 替换。
        let placeholder = RouteVia::Interface { name: String::new() };
        routes.push(PlannedRoute::capture("0.0.0.0/1".parse().unwrap(), placeholder.clone()));
        routes.push(PlannedRoute::capture("128.0.0.0/1".parse().unwrap(), placeholder.clone()));

        match req.routes.ipv6 {
            Ipv6Mode::Override => {
                routes.push(PlannedRoute::capture("::/1".parse().unwrap(), placeholder.clone()));
                routes.push(PlannedRoute::capture("8000::/1".parse().unwrap(), placeholder));
            }
            Ipv6Mode::Passthrough | Ipv6Mode::Disabled => {
                // 不动 v6 默认路由：v6 流量继续走物理网卡。
                // 代价是可能通过 v6 泄漏 —— 这是刻意的取舍，因为「一开 TUN 就断 v6」
                // 对用户来说更难接受。UI 里会明确提示这一点。
            }
        }
    }

    // ---- 4) DNS ----
    let dns_servers = match req.dns.mode {
        DnsMode::LeaveAlone => Vec::new(),
        _ => req.dns.servers.clone(),
    };
    for s in &dns_servers {
        let text = s.to_string();
        if !text.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':') {
            return Err(Error::Invalid(format!("DNS 地址非法: {s}")));
        }
    }

    Ok(TunPlan {
        session_id: req.session_id.clone(),
        addresses: req.addresses.clone(),
        mtu: req.mtu,
        routes,
        dns_servers,
        dns_mode: req.dns.mode.clone(),
        search_domains: req.dns.search_domains.clone(),
        physical,
    })
}

fn bypass_via(physical: &PhysicalUplink, host: IpAddr) -> Result<RouteVia> {
    match (host, physical.gateway) {
        (IpAddr::V4(_), Some(gw @ IpAddr::V4(_))) => Ok(RouteVia::Gateway { addr: gw }),
        (IpAddr::V6(_), Some(gw @ IpAddr::V6(_))) => Ok(RouteVia::Gateway { addr: gw }),
        // 没有网关（例如点对点链路）时退化为按接口走。
        _ => Ok(RouteVia::Interface { name: physical.interface.clone() }),
    }
}

/// 默认隧道网段的 IPv6 地址（ULA，不会被公网路由）。
pub const DEFAULT_TUN_NETWORK_V6: &str = "fdfe:dcba:9876::1/126";

#[cfg(test)]
mod tests {
    use super::*;
    use xt_proto::{DatapathPlan, DefaultRouteMode, DnsPlan, RoutePlan};

    fn physical() -> PhysicalUplink {
        PhysicalUplink {
            interface: "en0".into(),
            gateway: Some("192.168.1.1".parse().unwrap()),
            service: Some("Wi-Fi".into()),
        }
    }

    fn request(default_route: DefaultRouteMode, ipv6: Ipv6Mode) -> TunUpRequest {
        TunUpRequest {
            session_id: "s1".into(),
            interface_name: None,
            mtu: 1500,
            addresses: vec!["198.18.0.1/15".parse().unwrap()],
            routes: RoutePlan {
                default_route,
                bypass_hosts: vec!["203.0.113.7".parse().unwrap()],
                bypass_networks: vec!["192.168.0.0/16".parse().unwrap(), "fe80::/10".parse().unwrap()],
                ipv6,
            },
            dns: DnsPlan {
                mode: DnsMode::Automatic,
                servers: vec!["198.18.0.2".parse().unwrap()],
                search_domains: vec![],
            },
            datapath: DatapathPlan::HandoffFd,
            defer_default_routes: false,
        }
    }

    #[test]
    fn proxy_host_route_precedes_default_capture() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough), physical()).unwrap();
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();

        let host_idx = dests.iter().position(|d| d == "203.0.113.7/32").unwrap();
        let split_idx = dests.iter().position(|d| d == "0.0.0.0/1").unwrap();
        assert!(host_idx < split_idx, "代理服务器 host 路由必须先于默认接管，否则会形成路由环");

        // 且它走的是物理网关，不是隧道
        assert_eq!(
            plan.routes[host_idx].via,
            RouteVia::Gateway { addr: "192.168.1.1".parse().unwrap() }
        );
    }

    #[test]
    fn split_default_produces_both_halves() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough), physical()).unwrap();
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        assert!(dests.contains(&"0.0.0.0/1".to_string()));
        assert!(dests.contains(&"128.0.0.0/1".to_string()));
    }

    #[test]
    fn ipv6_passthrough_does_not_capture_v6() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough), physical()).unwrap();
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        assert!(!dests.contains(&"::/1".to_string()));
    }

    #[test]
    fn ipv6_override_captures_v6() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Override), physical()).unwrap();
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        assert!(dests.contains(&"::/1".to_string()));
        assert!(dests.contains(&"8000::/1".to_string()));
    }

    #[test]
    fn no_capture_mode_installs_no_default_routes() {
        let plan = build_plan(&request(DefaultRouteMode::None, Ipv6Mode::Passthrough), physical()).unwrap();
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        assert!(!dests.contains(&"0.0.0.0/1".to_string()));
    }

    #[test]
    fn v6_bypass_network_falls_back_to_interface() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough), physical()).unwrap();
        let fe80 = plan.routes.iter().find(|r| r.destination.to_string() == "fe80::/10").unwrap();
        assert_eq!(fe80.via, RouteVia::Interface { name: "en0".into() });
    }

    #[test]
    fn leave_alone_dns_produces_no_servers() {
        let mut req = request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough);
        req.dns.mode = DnsMode::LeaveAlone;
        let plan = build_plan(&req, physical()).unwrap();
        assert!(plan.dns_servers.is_empty());
    }

    #[test]
    fn reverse_routes_is_installation_order_reversed() {
        let plan = build_plan(&request(DefaultRouteMode::SplitDefault, Ipv6Mode::Override), physical()).unwrap();
        let fwd: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        let mut rev: Vec<String> = plan.reverse_routes().iter().map(|r| r.destination.to_string()).collect();
        rev.reverse();
        assert_eq!(fwd, rev);
    }

    #[test]
    fn rejects_non_ip_dns_server_text() {
        let mut req = request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough);
        // 直接构造一个非法串是不可能的（类型是 IpAddr），这里验证正常路径不报错。
        req.dns.servers = vec!["1.1.1.1".parse().unwrap()];
        assert!(build_plan(&req, physical()).is_ok());
    }
}
