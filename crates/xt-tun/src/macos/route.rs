//! 路由表操作。
//!
//! 这里实现两个关键策略：
//!
//! * **用 `0.0.0.0/1` + `128.0.0.0/1` 覆盖默认路由，而不是删掉默认路由。**
//!   这两条比 `/0` 更具体，因此对所有目的地址都胜出；而原默认路由完好无损。
//!   好处是回滚只要删这两条，即使进程被 `kill -9`，内核也会在 utun 消失时
//!   自动清理挂在该接口上的路由 —— 不会留下「网断了但不知道为什么」的状态。
//!
//! * **给代理服务器 IP 单独加一条更具体的 host 路由指向物理网关。**
//!   否则 Xray 自己连服务器的包也会被 `0.0.0.0/1` 送进隧道，形成路由环。
//!
//! macOS 没有 Linux 的 policy routing（没有 `ip rule`、没有 fwmark），
//! 所以「哪些流量走隧道」只能靠路由表本身表达 —— 这也是为什么
//! 分流规则要在 Xray 内部做，而不是在网络层做。
//! （`pfctl` 理论上可以，但 macOS 的 pf 版本较老、对本地生成流量的 `rdr`
//! 支持很脆，业界主流 TUN 客户端都不用。见 `docs/02-tun-and-privileges.md`。）

use std::net::IpAddr;

use xt_proto::{Cidr, RouteVia};

use crate::error::{Error, Result};
use crate::macos::{args, run, run_ok};
use crate::tools::ROUTE;
use crate::validate::validate_interface_name;

/// 默认路由信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultRoute {
    pub interface: String,
    pub gateway: Option<IpAddr>,
}

/// 查询 IPv4 默认路由。
///
/// 用 `route -n get default` 而不是解析 `netstat -rn`：前者输出稳定、
/// 字段带标签，且不受路由表规模影响。
pub fn default_route() -> Result<DefaultRoute> {
    let out = run(ROUTE, &args(&["-n", "get", "default"]))?;
    parse_route_get(&out).ok_or(Error::NoDefaultRoute)
}

/// 查询 IPv6 默认路由（可能不存在，返回 `None` 是正常的）。
pub fn default_route_v6() -> Option<DefaultRoute> {
    let out = run(ROUTE, &args(&["-n", "get", "-inet6", "default"])).ok()?;
    parse_route_get(&out)
}

/// 一条**已经存在**的路由（用于「装之前先记下原本是什么」，task-85）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingRoute {
    pub interface: Option<String>,
    pub gateway: Option<IpAddr>,
}

impl ExistingRoute {
    /// 还原它该用的 `RouteVia`。
    ///
    /// 实测样本（`route -n get -net 192.168.0.0/24`）：**接口路由没有 `gateway:` 行**，
    /// 只有 `interface: en0` ⇒ 用 `Interface` 还原；有 `gateway:` 的用 `Gateway` 还原。
    /// 两者都读不到 ⇒ `None`（不还原，避免瞎猜一条路由出来）。
    pub fn to_via(&self) -> Option<RouteVia> {
        match (&self.interface, self.gateway) {
            (Some(name), None) => Some(RouteVia::Interface { name: name.clone() }),
            (_, Some(addr)) => Some(RouteVia::Gateway { addr }),
            (None, None) => None,
        }
    }
}

/// 查**恰好这个前缀**上是否已经有路由（`route -n get -net <cidr>`）。
///
/// 为什么用这个命令（**实测**，macOS 26.6.2）：`route -n get -net` 是**精确前缀查找**，
/// 不是最长前缀匹配 ——
/// * `route -n get -net 127.0.0.0/8` 在这台被留下空洞的机器上回 `not in table`
///   （**不会**退回默认路由，所以它答的正是「这个前缀上有没有东西」）；
/// * `route -n get -net 192.168.0.0/24` 直接返回那条 on-link 接口路由
///   （`interface: en0`、**没有** `gateway:` 行）。
///
/// 查不到 / 出错 ⇒ `None`（= 原本这个前缀上什么都没有）。
pub fn existing_route(destination: &Cidr) -> Option<ExistingRoute> {
    let dest = destination.network().to_string();
    let mut argv: Vec<&str> = vec!["-n", "get"];
    if destination.addr.is_ipv6() {
        argv.push("-inet6");
    }
    argv.push("-net");
    argv.push(&dest);
    let out = run(ROUTE, &args(&argv)).ok()?;
    parse_route_get_info(&out)
}

/// 解析 `route -n get` 的输出，取「默认路由」需要的两个字段。
///
/// 它**不要求** `destination:` 行（历史行为：只有 `interface:` 也算解析成功，
/// 有测试钉着）。要判断「**这个前缀上到底有没有路由**」用下面的
/// [`parse_route_get_info`] —— 那个更严（必须有 `destination:`）。
fn parse_route_get(output: &str) -> Option<DefaultRoute> {
    let (interface, gateway, _) = route_get_fields(output);
    interface.map(|interface| DefaultRoute { interface, gateway })
}

/// 解析 `route -n get` 的输出。**只有出现 `destination:` 才算「查到了」** ——
/// 空洞的前缀上 `route` 什么都不输出（或只有一行 `not in table`）。
fn parse_route_get_info(output: &str) -> Option<ExistingRoute> {
    let (interface, gateway, has_destination) = route_get_fields(output);
    has_destination.then_some(ExistingRoute { interface, gateway })
}

/// `route -n get` 输出的字段抽取（两个解析函数共用，避免各写一份循环）。
///
/// 返回 `(interface, gateway, 是否有 destination 行)`。
fn route_get_fields(output: &str) -> (Option<String>, Option<IpAddr>, bool) {
    let mut interface = None;
    let mut gateway = None;
    let mut has_destination = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("destination:") {
            has_destination = true;
        } else if let Some(rest) = line.strip_prefix("gateway:") {
            gateway = rest.trim().parse::<IpAddr>().ok();
        } else if let Some(rest) = line.strip_prefix("interface:") {
            interface = Some(rest.trim().to_string());
        }
    }
    (interface, gateway, has_destination)
}

/// 安装一条路由。目标网段已由 [`Cidr`] 归一化。
pub fn add(destination: &Cidr, via: &RouteVia) -> Result<()> {
    run_ok(ROUTE, &build_args("add", destination, via)?)
}

/// 删除一条路由。
///
/// 「路由不存在」不算失败 —— 回滚路径上这个错误必须被吃掉，
/// 否则一次部分失败会让整个回滚中断，留下更糟的半残状态。
pub fn delete(destination: &Cidr, via: &RouteVia) -> Result<()> {
    let argv = build_args("delete", destination, via)?;
    match run_ok(ROUTE, &argv) {
        Ok(()) => Ok(()),
        Err(Error::Command { stderr, .. }) if is_missing_route(&stderr) => {
            tracing::debug!(%destination, "路由本就不存在，跳过删除");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn is_missing_route(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("not in table") || s.contains("no such process") || s.contains("bad address")
}

fn build_args(action: &str, destination: &Cidr, via: &RouteVia) -> Result<Vec<String>> {
    // 路由目标必须是网络地址：把 `10.1.2.3/8` 直接喂给 route(8) 时，
    // 它到底按 10.0.0.0/8 还是 10.1.0.0/8 处理是依赖实现细节的。
    // `Cidr` 刻意保留主机位（接口地址需要），所以归一化在这里显式做。
    let dest = destination.network();
    let mut v: Vec<String> = vec!["-n".into(), action.into()];

    // /32 与 /128 用 `-host`，其余用 `-net`。
    let is_host = dest.prefix == if dest.addr.is_ipv4() { 32 } else { 128 };
    if dest.addr.is_ipv6() {
        v.push("-inet6".into());
    }
    if is_host {
        // `-host` 只接受**裸地址**，不带 `/32`。
        //
        // 实测：`route -n add -host 255.255.255.255/32 ...` 直接
        // `route: bad address: 255.255.255.255/32`（退出码 68）。
        // 虽然某些地址带上 /32 恰好也能过，但那是 `getaddr()` 的解析细节，
        // 不是文档承诺的行为 —— 依赖它等于给自己埋一颗随机地址触发的雷。
        //
        // `-net` 则相反：`route -n add -net 10.0.0.0/8 ...` 是标准写法。
        v.push("-host".into());
        v.push(dest.addr.to_string());
    } else {
        v.push("-net".into());
        v.push(dest.to_string());
    }

    match via {
        RouteVia::Interface { name } => {
            validate_interface_name(name)?;
            v.push("-interface".into());
            v.push(name.clone());
        }
        RouteVia::ScopedInterface { name, gateway } => {
            validate_interface_name(name)?;
            if gateway.is_ipv4() != dest.addr.is_ipv4() {
                return Err(Error::Invalid(format!("目标 {dest} 与网关 {gateway} 地址族不匹配")));
            }
            // **网关必须给**。只写 `-interface` 会得到一条「目标在本地链路」
            // 的纯接口路由，内核会去对目标 IP 发 ARP，包根本出不去。
            v.push(gateway.to_string());
            // `-ifscope` 标记为「只对绑定该接口的 socket 生效」，
            // 从而允许它与 TUN 的默认接管路由共存。
            v.push("-ifscope".into());
            v.push(name.clone());
        }
        RouteVia::Gateway { addr } => {
            // 网关地址的地址族必须与目标一致，否则 route(8) 会拒绝。
            if addr.is_ipv4() != dest.addr.is_ipv4() {
                return Err(Error::Invalid(format!("目标 {dest} 与网关 {addr} 地址族不匹配")));
            }
            v.push(addr.to_string());
        }
    }
    Ok(v)
}

/// 列出挂载在某个接口上的路由（用于诊断与强制清理）。
pub fn routes_on_interface(interface: &str) -> Result<Vec<String>> {
    validate_interface_name(interface)?;
    let out = run(crate::tools::NETSTAT, &args(&["-rn"]))?;
    Ok(out
        .lines()
        .filter(|l| l.split_whitespace().any(|f| f == interface))
        .map(|l| l.trim().to_string())
        .collect())
}

// ---------------------------------------------------------------------------
// 路由表审计（task-176）：**为「现场包能直接回答『那条作用域默认路由在不在』」而存在**
// ---------------------------------------------------------------------------

/// 一次路由审计的**采样时点**。
///
/// ⚠️ 必须写进日志本身：否则事后分不清「这条审计是哪个阶段采的」——
/// 那正是本项目反复踩的「同一件事两个口径」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAuditPhase {
    /// helper 已建 utun、旁路路由已装，**默认接管还没发生**。
    AfterTunUp,
    /// `CommitRoutes` 之后 —— **关键时点**：此时绑卡直连必须能走通。
    AfterCommitRoutes,
    /// 回滚 / `TunDown` 之后。
    AfterRollback,
    /// 看门狗发现状态变化（含首轮基线）。
    WatchdogChanged,
}

impl RouteAuditPhase {
    pub fn label(self) -> &'static str {
        match self {
            RouteAuditPhase::AfterTunUp => "TunUp 之后（接管前）",
            RouteAuditPhase::AfterCommitRoutes => "CommitRoutes 之后",
            RouteAuditPhase::AfterRollback => "回滚之后",
            RouteAuditPhase::WatchdogChanged => "看门狗状态变化",
        }
    }
}

/// 只读的路由表审计结果（**只看结果，不看过程**）。
///
/// # 它能回答什么、不能回答什么
///
/// * 能：`default … I … <物理网卡>`（作用域默认路由）**在不在**、`0/1`/`128/1`
///   捕获路由指向哪个 utun、物理网卡上有几条带网关的 host 旁路（节点 `/32`）；
/// * **不能**：这条路由是**谁**装的 —— 别的 VPN 工具也在写同一张表（诚实清单第 4 条）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteAudit {
    /// 被审计的物理网卡名（写进日志，便于事后判读）。
    pub physical_interface: String,
    /// `default` 行的总数（健康会话应当是 2：系统的 + 我们那条作用域默认的）。
    pub default_rows: usize,
    /// `default` + flags 含 `I`(IFSCOPE) + 落在物理网卡上的那条的网关。
    pub scoped_default_gateway: Option<String>,
    /// `0/1` 捕获路由指向的接口（通常是 utunN）。
    pub capture_0_1: Option<String>,
    /// `128/1` 捕获路由指向的接口。
    pub capture_128_1: Option<String>,
    /// 物理网卡上带网关的 host 路由 `(目标, 网关)`：节点旁路的 `/32` 就是它们。
    pub bypass_hosts: Vec<(String, String)>,
}

impl RouteAudit {
    /// 作用域默认路由**缺失** —— 今天的关键盲区。
    pub fn scoped_default_missing(&self) -> bool {
        self.scoped_default_gateway.is_none()
    }

    /// 该采样时点下，缺这条路由是否构成**异常**。
    ///
    /// * `AfterTunUp`（接管前）与 `AfterRollback` 本来就不该有 ⇒ 不算异常；
    /// * `AfterCommitRoutes` 与 `WatchdogChanged` 缺了就异常 ——
    ///   绑了 en0 的 `direct` 出站会 `ENETUNREACH`（`task-172` 的形态）。
    pub fn is_anomaly(&self, phase: RouteAuditPhase) -> bool {
        self.scoped_default_missing()
            && matches!(
                phase,
                RouteAuditPhase::AfterCommitRoutes | RouteAuditPhase::WatchdogChanged
            )
    }

    /// **自包含的一行**（要求：一条日志就能判读，不必去翻别的上下文）。
    pub fn summary(&self, phase: RouteAuditPhase) -> String {
        let capture = format!(
            "0/1 → {}、128/1 → {}",
            self.capture_0_1.as_deref().unwrap_or("缺失"),
            self.capture_128_1.as_deref().unwrap_or("缺失"),
        );
        let tail = format!(
            "；旁路 host 路由 {} 条；default 行 {} 条",
            self.bypass_hosts.len(),
            self.default_rows
        );
        if let Some(gw) = &self.scoped_default_gateway {
            format!(
                "路由审计[{}]：{} 的作用域默认路由**在**（gateway {gw}）；{capture}{tail}",
                phase.label(),
                self.physical_interface,
            )
        } else {
            format!(
                "路由审计[{}]：{} 的作用域默认路由**缺失** ⇒ 绑定该网卡的直连（direct 出站）\
                 会 ENETUNREACH（task-172 的形态）；{capture}{tail}",
                phase.label(),
                self.physical_interface,
            )
        }
    }
}

/// 解析 `netstat -rn -f inet` 的文本（**纯函数**：真实样例可直接当测试输入）。
///
/// 认这三类行（其余忽略）：
/// * `default <gw> <flags> <netif>`：数 `default` 行数；flags 含 `I` 且 netif 是物理网卡 ⇒ scoped 默认路由；
/// * `0/1` / `128/1 ... <netif>`：捕获路由指向哪个接口；
/// * 裸 IPv4 + flags 含 `G` 与 `H` + netif 是物理网卡：带网关的 host 路由（节点 `/32` 旁路）。
///   （ARP 邻居那些 `UHLWI` 没有 `G`，因此不会被算成旁路。）
pub fn parse_netstat_inet(table: &str, physical_interface: &str) -> RouteAudit {
    let mut audit = RouteAudit {
        physical_interface: physical_interface.to_string(),
        ..Default::default()
    };
    for line in table.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue; // 表头 / 空行 / 统计行
        }
        let (dest, gateway, flags, netif) = (f[0], f[1], f[2], f[3]);
        match dest {
            "default" => {
                audit.default_rows += 1;
                if flags.contains('I') && netif == physical_interface {
                    audit.scoped_default_gateway = Some(gateway.to_string());
                }
            }
            "0/1" | "0.0.0.0/1" => audit.capture_0_1 = Some(netif.to_string()),
            "128/1" | "128.0.0.0/1" => audit.capture_128_1 = Some(netif.to_string()),
            _ => {
                if dest.parse::<std::net::Ipv4Addr>().is_ok()
                    && flags.contains('G')
                    && flags.contains('H')
                    && netif == physical_interface
                {
                    audit.bypass_hosts.push((dest.to_string(), gateway.to_string()));
                }
            }
        }
    }
    audit
}

/// 读**当前**路由表并审计（只读：一次 `netstat -rn -f inet`）。
pub fn current_route_audit(physical_interface: &str) -> Result<RouteAudit> {
    validate_interface_name(physical_interface)?;
    let out = run(crate::tools::NETSTAT, &args(&["-rn", "-f", "inet"]))?;
    Ok(parse_netstat_inet(&out, physical_interface))
}

/// 一次 TUN 生命周期里累积的路由审计记录（按发生顺序）。
///
/// `None` = 这次采样**读不到**路由表（`netstat` 失败）—— 必须如实记「不可判读」，
/// 不许静默跳过（否则现场包里会「少一段」而没人知道）。
#[derive(Debug, Clone, Default)]
pub struct RouteAuditLog {
    records: Vec<(RouteAuditPhase, Option<RouteAudit>)>,
}

impl RouteAuditLog {
    pub fn push(&mut self, phase: RouteAuditPhase, audit: Option<RouteAudit>) {
        self.records.push((phase, audit));
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 取走全部记录（调用方负责落盘；App 侧在 `start()` 返回后统一记）。
    pub fn take(&mut self) -> Vec<(RouteAuditPhase, Option<RouteAudit>)> {
        std::mem::take(&mut self.records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_route_get_output() {
        let sample = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>
";
        let r = parse_route_get(sample).unwrap();
        assert_eq!(r.interface, "en0");
        assert_eq!(r.gateway, Some("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn parses_route_get_without_gateway() {
        let sample = "   route to: default\n  interface: utun4\n";
        let r = parse_route_get(sample).unwrap();
        assert_eq!(r.interface, "utun4");
        assert_eq!(r.gateway, None);
    }

    #[test]
    fn route_get_without_interface_returns_none() {
        assert!(parse_route_get("   route to: default\n").is_none());
    }

    #[test]
    fn split_default_uses_net_flag_with_cidr() {
        let d: Cidr = "0.0.0.0/1".parse().unwrap();
        let argv = build_args("add", &d, &RouteVia::Interface { name: "utun4".into() }).unwrap();
        assert_eq!(argv, vec!["-n", "add", "-net", "0.0.0.0/1", "-interface", "utun4"]);
    }

    /// `-host` 必须带**裸地址**，不能带 `/32`。
    ///
    /// 回归测试：曾经这里传的是 `203.0.113.7/32`，而 `route(8)` 对
    /// `-host 255.255.255.255/32` 会直接拒绝（`bad address`，退出码 68），
    /// 导致整个 TUN 建立失败。
    #[test]
    fn host_route_uses_bare_address_not_cidr() {
        let d: Cidr = "203.0.113.7/32".parse().unwrap();
        let argv = build_args("add", &d, &RouteVia::Gateway { addr: "192.168.1.1".parse().unwrap() }).unwrap();
        assert_eq!(argv, vec!["-n", "add", "-host", "203.0.113.7", "192.168.1.1"]);
        assert!(
            !argv.iter().any(|a| a.contains('/')),
            "`-host` 的参数里不能出现 '/'：{argv:?}"
        );
    }

    #[test]
    fn ipv6_host_route_also_uses_bare_address() {
        let d: Cidr = "2001:db8::1/128".parse().unwrap();
        let argv = build_args("add", &d, &RouteVia::Interface { name: "utun4".into() }).unwrap();
        assert_eq!(argv, vec!["-n", "add", "-inet6", "-host", "2001:db8::1", "-interface", "utun4"]);
    }

    /// 作用域默认路由**必须带网关**。
    ///
    /// 回归：曾经写成 `-interface en0 -ifscope en0`（无网关），内核把它当成
    /// 「目标在本地链路上」的纯接口路由，于是对每个目标发 ARP —— 包全部发不出去，
    /// 表现为 TUN 模式下所有直连静默卡死。
    #[test]
    fn scoped_interface_route_carries_gateway_and_ifscope() {
        let d: Cidr = "0.0.0.0/0".parse().unwrap();
        let argv = build_args(
            "add",
            &d,
            &RouteVia::ScopedInterface { name: "en0".into(), gateway: "192.168.0.1".parse().unwrap() },
        )
        .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-net", "0.0.0.0/0", "192.168.0.1", "-ifscope", "en0"]);
        assert!(argv.contains(&"192.168.0.1".to_string()), "必须带网关：{argv:?}");
    }

    #[test]
    fn scoped_interface_still_validates_the_name() {
        let d: Cidr = "0.0.0.0/0".parse().unwrap();
        let e = build_args(
            "add",
            &d,
            &RouteVia::ScopedInterface { name: "-rf".into(), gateway: "192.168.0.1".parse().unwrap() },
        );
        assert!(e.is_err());
    }

    #[test]
    fn scoped_interface_rejects_cross_family_gateway() {
        let d: Cidr = "0.0.0.0/0".parse().unwrap();
        let e = build_args(
            "add",
            &d,
            &RouteVia::ScopedInterface { name: "en0".into(), gateway: "fe80::1".parse().unwrap() },
        );
        assert!(e.is_err());
    }

    #[test]
    fn ipv6_route_carries_inet6_flag() {
        let d: Cidr = "::/1".parse().unwrap();
        let argv = build_args("add", &d, &RouteVia::Interface { name: "utun4".into() }).unwrap();
        assert!(argv.contains(&"-inet6".to_string()));
    }

    #[test]
    fn rejects_cross_family_gateway() {
        let d: Cidr = "0.0.0.0/1".parse().unwrap();
        let err = build_args("add", &d, &RouteVia::Gateway { addr: "fe80::1".parse().unwrap() });
        assert!(err.is_err());
    }

    #[test]
    fn rejects_interface_name_injection() {
        let d: Cidr = "0.0.0.0/1".parse().unwrap();
        let err = build_args("add", &d, &RouteVia::Interface { name: "-rf /".into() });
        assert!(err.is_err());
    }

    #[test]
    fn foreign_route_errors_are_treated_as_already_gone() {
        assert!(is_missing_route("route: writing to routing socket: not in table"));
        assert!(is_missing_route("delete net 0.0.0.0: not in table"));
        assert!(!is_missing_route("route: permission denied"));
    }

    // -----------------------------------------------------------------------
    // task-85：查「这个前缀上原本有没有路由」（回滚时要不只删除、还要恢复）
    // -----------------------------------------------------------------------

    /// **实测样本**（本机 `route -n get -net 192.168.0.0/24`）：
    /// 接口路由**没有 `gateway:` 行**，只有 `interface:`。
    #[test]
    fn parses_an_existing_interface_route() {
        let sample = "\
   route to: 192.168.0.0
destination: 192.168.0.0
       mask: 255.255.255.0
  interface: en0
      flags: <UP,DONE,CLONING,STATIC>
 recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
       0         0         0         0         0         0      1500         0 \n";
        let info = parse_route_get_info(sample).expect("有 destination 行 ⇒ 查到了");
        assert_eq!(info.interface.as_deref(), Some("en0"));
        assert_eq!(info.gateway, None, "接口路由没有 gateway 行（实测）");
        assert_eq!(
            info.to_via(),
            Some(RouteVia::Interface { name: "en0".into() }),
            "接口路由必须还原成 Interface（不是 Gateway）——回滚要靠它把 lo0 那条装回去",
        );
    }

    /// 网关路由按网关还原。
    #[test]
    fn parses_an_existing_gateway_route() {
        let sample = "\
destination: 10.0.0.0
       mask: 255.0.0.0
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC>\n";
        let info = parse_route_get_info(sample).expect("查到了");
        assert_eq!(
            info.to_via(),
            Some(RouteVia::Gateway {
                addr: "192.168.1.1".parse().unwrap()
            }),
        );
    }

    /// **空洞的前缀**：`route` 没有输出（或只有一行 `not in table`）⇒ 什么都没查到。
    ///
    /// **实测**：这台被留下空洞的机器上 `route -n get -net 127.0.0.0/8` 就是空输出。
    /// 这一条决定「回滚时不许凭空造路由」。
    #[test]
    fn empty_output_means_the_prefix_has_no_route() {
        assert_eq!(parse_route_get_info(""), None);
        assert_eq!(
            parse_route_get_info("route: writing to routing socket: not in table\n"),
            None,
        );
        // 只有 interface 行、没有 destination 行 —— 不算查到（宁可当没有）
        assert_eq!(parse_route_get_info("  interface: en0\n"), None);
    }

    /// 默认路由的解析没被这次重构改坏（`destination: default` 同样算 destination 行）。
    #[test]
    fn default_route_still_parses_after_the_refactor() {
        let sample = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.0.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC>\n";
        let r = parse_route_get(sample).expect("默认路由仍应解析");
        assert_eq!(r.interface, "en0");
        assert_eq!(r.gateway, Some("192.168.0.1".parse().unwrap()));
    }

    // -----------------------------------------------------------------------
    // task-176：路由表审计（真实样例；地址用文档网段，**结构**照抄现场）
    // -----------------------------------------------------------------------

    /// 健康会话的 `netstat -rn -f inet` 形态（结构照抄
    /// `docs/09-network-drop/LOOPBACK-ROUTE-BASELINE.md:27-32` 的实测样例）：
    /// **两条 default**，其中一条带 `I`(IFSCOPE) 落在物理网卡上。
    const HEALTHY_TABLE: &str = "\
Routing tables

Internet:
Destination        Gateway            Flags        Netif Expire
0/1                utun6              UScg                utun6
default            192.168.0.1        UGScg                 en0
default            192.168.0.1        UGScIg                en0
127                192.168.0.1        UGSc                  en0
127.0.0.1          127.0.0.1          UH                    lo0
198.18.0.1         198.18.0.1         UH                  utun6
";

    /// 事故现场包的形态（**结构**照抄，地址换成文档网段 203.0.113.0/24）：
    /// **只有一条 default**（系统的，没有 `I`）；`0/1` 捕获在；两条节点 `/32` 旁路仍在。
    const INCIDENT_TABLE: &str = "\
Routing tables

Internet:
Destination        Gateway            Flags        Netif Expire
0/1                utun6              UScg                utun6
default            192.168.0.1        UGScg                 en0
203.0.113.7        192.168.0.1        UGHS                  en0
203.0.113.9        192.168.0.1        UGHS                  en0
127.0.0.1          127.0.0.1          UH                    lo0
198.18.0.1         198.18.0.1         UH                  utun6
192.168.0.11       0:12:42:69:f:ca    UHLWI                 en0   1199
192.168.0.1        f8:ce:21:e5:36:2a  UHLWIi                en0   1196
";

    #[test]
    fn route_audit_reads_the_scoped_default_from_a_healthy_table() {
        let a = parse_netstat_inet(HEALTHY_TABLE, "en0");
        assert_eq!(a.default_rows, 2, "健康形态有两条 default（系统 + scoped）");
        assert_eq!(a.scoped_default_gateway.as_deref(), Some("192.168.0.1"));
        assert_eq!(a.capture_0_1.as_deref(), Some("utun6"));
        assert!(!a.scoped_default_missing());
        assert!(
            !a.is_anomaly(RouteAuditPhase::AfterCommitRoutes),
            "接管后这条路由在 ⇒ 不是异常"
        );
        let s = a.summary(RouteAuditPhase::AfterCommitRoutes);
        assert!(s.contains("的作用域默认路由**在**"), "{s}");
        assert!(s.contains("192.168.0.1") && s.contains("0/1 → utun6"), "{s}");
        assert!(s.contains("CommitRoutes 之后"), "采样时点必须写进日志：{s}");
    }

    #[test]
    fn route_audit_flags_the_missing_scoped_default_after_commit() {
        let a = parse_netstat_inet(INCIDENT_TABLE, "en0");
        assert_eq!(a.default_rows, 1, "事故形态只有系统的 default");
        assert!(
            a.scoped_default_missing(),
            "没有带 I 标志的 default ⇒ scoped 默认路由缺失"
        );
        assert!(a.is_anomaly(RouteAuditPhase::AfterCommitRoutes));
        assert!(a.is_anomaly(RouteAuditPhase::WatchdogChanged));
        assert!(
            !a.is_anomaly(RouteAuditPhase::AfterTunUp)
                && !a.is_anomaly(RouteAuditPhase::AfterRollback),
            "接管前 / 回滚后本来就不该有这条路由 ⇒ 不许报警（否则是噪声）"
        );
        let s = a.summary(RouteAuditPhase::AfterCommitRoutes);
        // **自包含**：一条日志就能判读「缺了 + 后果 + 证据」
        assert!(s.contains("的作用域默认路由**缺失**"), "{s}");
        assert!(s.contains("ENETUNREACH"), "{s}");
        assert!(s.contains("task-172"), "{s}");
        assert!(s.contains("0/1 → utun6"), "{s}");
        assert!(s.contains("旁路 host 路由 2 条"), "{s}");
        assert!(s.contains("default 行 1 条"), "{s}");
        assert!(s.contains("CommitRoutes 之后"), "采样时点必须写进日志：{s}");
    }

    #[test]
    fn route_audit_reads_both_capture_routes_and_ignores_arp_neighbours() {
        let table =
            format!("{INCIDENT_TABLE}128/1              utun6              UScg                utun6\n");
        let a = parse_netstat_inet(&table, "en0");
        assert_eq!(a.capture_128_1.as_deref(), Some("utun6"));
        // ARP 邻居（UHLWI，没有 G）不许被算成节点旁路
        assert_eq!(a.bypass_hosts.len(), 2, "{:?}", a.bypass_hosts);
        assert!(a.bypass_hosts.iter().all(|(_, gw)| gw == "192.168.0.1"));
    }

    #[test]
    fn route_audit_rejects_a_bad_interface_name() {
        assert!(current_route_audit("-rf").is_err());
    }
}
