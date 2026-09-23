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

use xt_proto::{Cidr, DnsMode, InstalledRoute, Ipv6Mode, RouteVia, TunUpRequest};

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

/// 回滚时要执行的一个动作（task-85）。
///
/// # 为什么不能只有「删除」
///
/// `route add` 对**同一个前缀**是**替换**语义，不是新增：我们装
/// `127.0.0.0/8 → 物理网关` 时，会把内核那条 on-link 的 `127/8 → lo0` 顶掉。
/// 回滚若只按快照「反序删除自己装的那些」，**内核原来那条不会自己回来** ——
/// 于是机器从此缺一条本该有的接口路由（**实测**：`127.0.0.2` 永久 100% 丢包，
/// 而 `en0` 的同类 on-link 路由还在，形成不对称）。所以：
///
/// * **删除**我们装的那条；
/// * **恢复**装之前同前缀上原本存在的那条（如果当时记下了）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackAction {
    /// 删掉我们装的那条。
    Delete { destination: Cidr, via: RouteVia },
    /// 把我们装之前同前缀上原本存在的那条**装回去**。
    ///
    /// 只有 `InstalledRoute::replaced` 有值时才产生 —— **原本没有就不许凭空造**。
    Restore { destination: Cidr, via: RouteVia },
}

/// 按快照算回滚动作：**安装顺序反序**；每条先删自己、再恢复被顶掉的那条。
///
/// 纯函数（不碰系统），所以「回滚到底会不会恢复」这件事可以被断言 ——
/// 这是本卡的核心（以前的实现只删除，空洞就是这么留下的）。
pub fn rollback_plan(routes: &[InstalledRoute]) -> Vec<RollbackAction> {
    let mut actions = Vec::new();
    for installed in routes.iter().rev() {
        actions.push(RollbackAction::Delete {
            destination: installed.destination,
            via: installed.via.clone(),
        });
        if let Some(prior) = &installed.replaced {
            actions.push(RollbackAction::Restore {
                destination: installed.destination,
                via: prior.clone(),
            });
        }
    }
    actions
}

/// 根据请求与物理出口计算变更计划。
pub fn build_plan(req: &TunUpRequest, physical: PhysicalUplink) -> Result<TunPlan> {
    let mut routes = Vec::new();

    // ---- 2) 代理服务器 IP：走物理网关（防止路由环） ----
    for host in &req.routes.bypass_hosts {
        let cidr = Cidr::host(*host);
        validate_cidr_text(&cidr.to_string())?;
        // **回环不走物理网关。** 回环地址本来就只在 `lo0` 上，装一条经由网关的
        // `127.0.0.1/32` 反而会把它送出去（与下面 2b 是同一类错误）。
        if host.is_loopback() {
            continue;
        }
        // 服务器 host 路由是防路由环的核心，装不上就必须失败。
        routes.push(PlannedRoute::bypass(cidr, bypass_via(&physical, *host)?, true));
    }

    // ---- 2b) 私有/保留网段：同样走物理出口 ----
    for raw in &req.routes.bypass_networks {
        // 这些是「网段」而不是「接口地址」，所以先归一化（`Cidr` 刻意保留主机位）。
        let net = raw.network();
        validate_cidr_text(&net.to_string())?;
        // **回环网段不装路由。**
        //
        // 内核已经把 `127.0.0.0/8` 指向 `lo0`，而它比捕获用的 `0.0.0.0/1`
        // 更具体（`/8` > `/1`）⇒ TUN 本来就卷不走回环，这条旁路**没有必要**；
        // 装上反而有害：`127.0.0.2` 及以上会被送去局域网路由器，换网后还指向旧网关。
        // （实测：用户机器上出现过 `127 → 192.168.0.1 UGSc en0`。）
        // `::1/128` 同理：即便走 `::/1` 捕获也安全（`/128` 比 `/1` 更具体）。
        //
        // 防御放在这里、而不是只从 `default_bypass_networks()` 里删掉：列表可能被
        // 别的调用方带进来（IPC 请求是**数据**，不是常量），这一层对任何回环都成立。
        if net.addr.is_loopback() {
            continue;
        }
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

    // -----------------------------------------------------------------------
    // task-83：回环绝不经由物理网关
    //
    // 实测现场：连接状态下用户机器上出现过 `127 → 192.168.0.1 UGSc en0`
    // （整个回环网段指向局域网路由器），来源就是原来的 bypass 循环
    // 「所有 IPv4 旁路网段一律指向物理网关」。回环本来不需要旁路：
    // 内核已把 `127.0.0.0/8` 指向 `lo0`，且比捕获用的 `0.0.0.0/1` 更具体。
    // -----------------------------------------------------------------------

    fn request_with_bypass(
        default_route: DefaultRouteMode,
        ipv6: Ipv6Mode,
        bypass_hosts: Vec<IpAddr>,
        bypass_networks: Vec<Cidr>,
    ) -> TunUpRequest {
        let mut req = request(default_route, ipv6);
        req.routes.bypass_hosts = bypass_hosts;
        req.routes.bypass_networks = bypass_networks;
        req
    }

    fn route_for<'a>(plan: &'a TunPlan, dest: &str) -> Option<&'a PlannedRoute> {
        plan.routes.iter().find(|r| r.destination.to_string() == dest)
    }

    /// **核心断言**：规划结果里**没有**回环目的地会被装成路由。
    ///
    /// 两个方向都断言：
    /// ① 默认旁路列表（现在已不含回环）不产生任何回环路由；
    /// ② **防御**：请求里就算带了回环（网段 + 主机，v4 + v6），也不许装上。
    #[test]
    fn loopback_never_gets_a_route_even_if_the_request_asks_for_it() {
        // ① 默认列表
        let plan = build_plan(
            &request_with_bypass(
                DefaultRouteMode::SplitDefault,
                Ipv6Mode::Passthrough,
                vec!["203.0.113.7".parse().unwrap()],
                xt_proto::default_bypass_networks(),
            ),
            physical(),
        )
        .unwrap();
        let loopbacks: Vec<String> = plan
            .routes
            .iter()
            .filter(|r| r.destination.addr.is_loopback())
            .map(|r| r.destination.to_string())
            .collect();
        assert!(
            loopbacks.is_empty(),
            "默认旁路列表不该产生回环路由（内核自己就走 lo0）：{loopbacks:?}",
        );

        // ② 防御：请求里硬带回环
        let plan = build_plan(
            &request_with_bypass(
                DefaultRouteMode::SplitDefault,
                Ipv6Mode::Passthrough,
                vec![
                    "127.0.0.1".parse().unwrap(),
                    "203.0.113.7".parse().unwrap(),
                ],
                vec![
                    "127.0.0.0/8".parse().unwrap(),
                    "::1/128".parse().unwrap(),
                    "192.168.0.0/16".parse().unwrap(),
                ],
            ),
            physical(),
        )
        .unwrap();
        let loopbacks: Vec<String> = plan
            .routes
            .iter()
            .filter(|r| r.destination.addr.is_loopback())
            .map(|r| r.destination.to_string())
            .collect();
        assert!(
            loopbacks.is_empty(),
            "回环绝不该被装上路由（连 lo0 版本都不必装：内核已处理）：{loopbacks:?}",
        );
        // 同一次请求里，非回环的那条**必须**还在 —— 证明跳过是精准的、不是整段跳过。
        assert!(
            route_for(&plan, "192.168.0.0/16").is_some(),
            "只跳过回环，其它网段必须照常安装",
        );
        assert!(
            route_for(&plan, "203.0.113.7/32").is_some(),
            "公网服务器 host 路由必须照常安装（它是防路由环的核心）",
        );
    }

    /// **不变量（独立测试）**：其它旁路网段**必须仍然**经由物理网关
    /// —— 别为了修回环一条把整张表弄没。
    #[test]
    fn other_bypass_networks_still_go_via_the_physical_gateway() {
        let plan = build_plan(
            &request_with_bypass(
                DefaultRouteMode::SplitDefault,
                Ipv6Mode::Passthrough,
                vec!["203.0.113.7".parse().unwrap()],
                xt_proto::default_bypass_networks(),
            ),
            physical(),
        )
        .unwrap();
        let gw = RouteVia::Gateway {
            addr: "192.168.1.1".parse().unwrap(),
        };
        for dest in [
            "10.0.0.0/8",
            "100.64.0.0/10",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "224.0.0.0/4",
        ] {
            let route = route_for(&plan, dest).unwrap_or_else(|| panic!("{dest} 必须仍在计划里"));
            assert_eq!(route.via, gw, "{dest} 必须仍然经由物理网关");
            assert_eq!(route.kind, RouteKind::Bypass);
            assert!(
                !route.critical,
                "{dest} 是便利旁路：装不上不该毁掉整条隧道",
            );
        }
        // v6 链路本地照旧按接口走（没有 v6 网关）—— 既有行为，不许被这次修复改掉。
        let v6 = route_for(&plan, "fe80::/10").expect("fe80::/10 必须在");
        assert_eq!(v6.via, RouteVia::Interface { name: "en0".into() });
    }

    // -----------------------------------------------------------------------
    // task-143 A0：把「v6 节点的旁路退化成无网关 on-link」从 [推断] 变成可断言事实
    // （机制见 `docs/verification/V6-DATAPATH-AUDIT.md` §2；这里只钉住代码现状）
    // -----------------------------------------------------------------------

    /// **A0**：v6 服务器地址在当前实现下拿到什么 `via`。
    ///
    /// 机制：`PhysicalUplink.gateway` 只可能来自 IPv4 `default_route()`
    /// （App `supervisor.rs:507`、helper `controller.rs:48` 都只探 v4），
    /// 而 [`bypass_via`] 只认「同族网关」⇒ v6 主机落进 `_ => Interface{ en0 }`
    /// ⇒ `route -n add -inet6 -host <v6> -interface en0`（**没有网关**）。
    /// 本项目自己写过这种形状「包根本出不去」（`route.rs:196-197`、`plan.rs:212-213`），
    /// v4 侧还有回归测试 `scoped_interface_route_carries_gateway_and_ifscope`。
    ///
    /// ⚠️ 这条是**现状**的固化（修法是让 `physical` 拿得到 v6 网关，不是改这个 match）。
    #[test]
    fn v6_bypass_host_degrades_to_an_on_link_route_when_only_a_v4_gateway_exists() {
        let mut req = request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough);
        req.routes.bypass_hosts = vec!["2001:db8::1".parse().unwrap()];

        let plan = build_plan(&req, physical()).unwrap();
        let host = route_for(&plan, "2001:db8::1/128").expect("v6 host 旁路必须在计划里");
        assert_eq!(host.kind, RouteKind::Bypass);
        assert!(
            host.critical,
            "服务器 host 路由是防路由环的核心，必须 critical（装不上就该让建立失败）"
        );
        assert_eq!(
            host.via,
            RouteVia::Interface { name: "en0".into() },
            "v6 主机 + 只有 v4 网关 ⇒ 退化成**无网关**的 on-link /128（task-143 的 [推断]）"
        );
    }

    /// **A0 反向**：同族 v6 网关在场时 [`bypass_via`] 会给出 `Gateway{ v6 }`
    /// ⇒ 缺的不是 match 逻辑，而是「生产路径根本拿不到 v6 网关」
    /// （`default_route_v6()` 从初始提交起无人调用）。
    #[test]
    fn v6_bypass_host_uses_a_v6_gateway_when_one_is_supplied() {
        let mut req = request(DefaultRouteMode::SplitDefault, Ipv6Mode::Passthrough);
        req.routes.bypass_hosts = vec!["2001:db8::1".parse().unwrap()];
        let mut phys = physical();
        phys.gateway = Some("fe80::1".parse().unwrap());

        let plan = build_plan(&req, phys).unwrap();
        let host = route_for(&plan, "2001:db8::1/128").expect("v6 host 旁路必须在计划里");
        assert_eq!(
            host.via,
            RouteVia::Gateway { addr: "fe80::1".parse().unwrap() },
            "同族网关在场时必须给出带网关的路由（修法方向：让 physical 拿得到 v6 网关）"
        );
    }

    /// **不变量**：捕获路由不受回环跳过逻辑影响。
    #[test]
    fn default_capture_routes_are_unaffected_by_the_loopback_skip() {
        let plan = build_plan(
            &request_with_bypass(
                DefaultRouteMode::SplitDefault,
                Ipv6Mode::Override,
                vec!["203.0.113.7".parse().unwrap()],
                vec![
                    "127.0.0.0/8".parse().unwrap(),
                    "10.0.0.0/8".parse().unwrap(),
                ],
            ),
            physical(),
        )
        .unwrap();
        for dest in ["0.0.0.0/1", "128.0.0.0/1", "::/1", "8000::/1"] {
            let route = route_for(&plan, dest).unwrap_or_else(|| panic!("捕获路由 {dest} 必须仍在"));
            assert_eq!(route.kind, RouteKind::DefaultCapture);
            assert!(route.critical, "捕获路由装不上就意味着流量没被接管，必须 critical");
        }
        // 旁路里那条非回环的仍在，且仍经由物理网关
        let b = route_for(&plan, "10.0.0.0/8").expect("10.0.0.0/8 必须仍在");
        assert_eq!(b.kind, RouteKind::Bypass);
        assert_eq!(
            b.via,
            RouteVia::Gateway {
                addr: "192.168.1.1".parse().unwrap()
            },
        );
        // 顺序不变量：bypass 必须先于默认接管（否则会形成路由环）
        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        let host_idx = dests.iter().position(|d| d == "203.0.113.7/32").unwrap();
        let split_idx = dests.iter().position(|d| d == "0.0.0.0/1").unwrap();
        assert!(host_idx < split_idx);
    }

    // -----------------------------------------------------------------------
    // task-85：**删除 ≠ 恢复**
    //
    // `route add` 对**同前缀**是替换语义：装「127.0.0.0/8 → 物理网关」会把内核那条
    // on-link 的 `127/8 → lo0` 顶掉；回滚若只删自己那条，内核原来那条**不会回来**
    // ⇒ 留下空洞（**实测**：127.0.0.2 从此永久 100% 丢包，而 en0 的同类路由还在）。
    // -----------------------------------------------------------------------

    fn gw_via() -> RouteVia {
        RouteVia::Gateway {
            addr: "192.168.1.1".parse().unwrap(),
        }
    }

    fn lo0_via() -> RouteVia {
        RouteVia::Interface { name: "lo0".into() }
    }

    fn installed_route(dest: &str, via: RouteVia, replaced: Option<RouteVia>) -> InstalledRoute {
        InstalledRoute {
            destination: dest.parse().unwrap(),
            via,
            replaced,
        }
    }

    /// **核心断言**：原本该前缀上有一条接口路由 ⇒ 回滚动作**必须包含「把它装回去」**。
    #[test]
    fn rollback_restores_the_route_we_shadowed_instead_of_only_deleting() {
        let actions = rollback_plan(&[installed_route("127.0.0.0/8", gw_via(), Some(lo0_via()))]);
        assert!(
            actions.contains(&RollbackAction::Delete {
                destination: "127.0.0.0/8".parse().unwrap(),
                via: gw_via(),
            }),
            "我们装的那条仍然要删掉",
        );
        assert!(
            actions.contains(&RollbackAction::Restore {
                destination: "127.0.0.0/8".parse().unwrap(),
                via: lo0_via(),
            }),
            "**必须把内核原本那条 127/8 → lo0 装回来** —— 只删自己那条会留下空洞\
             （实测：127.0.0.2 永久 100% 丢包）",
        );
    }

    /// **反例**：原本同前缀上什么都没有 ⇒ **不得凭空造**一条（只删自己那条）。
    #[test]
    fn rollback_never_invents_a_route_that_was_not_there() {
        let actions = rollback_plan(&[installed_route("192.168.0.0/16", gw_via(), None)]);
        assert_eq!(actions.len(), 1, "只删自己那条：{actions:?}");
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, RollbackAction::Restore { .. })),
            "原本没有同前缀路由 ⇒ 不许恢复（不许凭空造）：{actions:?}",
        );
    }

    /// 顺序：**安装顺序反序**，且每条自己的 Delete 在它的 Restore 之前。
    #[test]
    fn rollback_actions_are_reverse_install_order_with_restore_after_delete() {
        // 安装顺序：先 a（顶掉了 lo0 那条）、后 b（原本没有）
        let actions = rollback_plan(&[
            installed_route("127.0.0.0/8", gw_via(), Some(lo0_via())),
            installed_route("10.0.0.0/8", gw_via(), None),
        ]);
        assert_eq!(
            actions,
            vec![
                RollbackAction::Delete {
                    destination: "10.0.0.0/8".parse().unwrap(),
                    via: gw_via(),
                },
                RollbackAction::Delete {
                    destination: "127.0.0.0/8".parse().unwrap(),
                    via: gw_via(),
                },
                RollbackAction::Restore {
                    destination: "127.0.0.0/8".parse().unwrap(),
                    via: lo0_via(),
                },
            ],
            "先处理后装的那条（b），再处理 a 的删除与恢复",
        );
    }

    /// **老快照兼容**：没有 `replaced` 字段的条目反序列化后是 `None` ⇒ 回滚只删不造。
    #[test]
    fn old_snapshots_without_the_replaced_field_still_parse() {
        let json = r#"{"destination":"127.0.0.0/8","via":{"kind":"gateway","addr":"192.168.1.1"}}"#;
        let r: InstalledRoute = serde_json::from_str(json).expect("老快照必须还能反序列化");
        assert_eq!(r.replaced, None, "缺字段 ⇒ None（向后兼容）");
        let actions = rollback_plan(&[r]);
        assert_eq!(actions.len(), 1, "老条目回滚时只删自己那条：{actions:?}");
    }
}
