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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

/// 查**恰好这个前缀（网络号 + 前缀长度都相等）**上原本有没有路由。
///
/// # 判据来源（为什么不能再用 `route -n get -net <cidr>`）
///
/// 旧实现用 `route -n get -net`，注释声称它是「精确前缀查找」。**实测 + route(8)
/// 源码**都表明它不是，这个错误会直接变成回滚后的残留路由：
///
/// 1. **本机只读实测**（macOS，隧道在跑，`netstat -rn -f inet` 里有
///    `0/1 → utun6` 与 `128.0/1 → utun6`）：
///    ```text
///    $ route -n get -net 0.0.0.0/1
///    destination: default        ← 目标地址 0.0.0.0 被显示成 default，只能靠 mask 区分
///           mask: 128.0.0.0      ← /1，说明是 0.0.0.0/1 那条
///      interface: utun6
///    $ route -n get -net 192.0.0.0/1     ← 表里**没有** 192.0.0.0/1
///    destination: 128.0.0.0      ← 却返回了**另一张网**的 /1（128.0.0.0/1）
///           mask: 128.0.0.0
///      interface: utun6
///    $ route -n get -net 34.135.247.136/32   ← 表里有 `34.135.247.136 192.168.0.1 UGHS en0`
///    destination: default        ← 却返回了 `0/1` 捕获路由！
///           mask: 128.0.0.0
///      interface: utun6
///    $ route -n get -net 0.0.0.0/2       ← 表里没有 /2
///    route: writing to routing socket: not in table
///    ```
///    同一条 `/32` 查询在另一次采样里又回 `not in table` —— 结果随路由表状态漂移。
///    即：`route -n get -net` 匹配的是「**覆盖目标地址**的那条」，前缀长度/网络号都可以
///    不同，而且不稳定。它不是「网络号精确」，更不是「前缀长度精确」。
/// 2. **route(8) 源码**（Apple `network_cmds/route.tproj/route.c`）与上面一致：
///    `getaddr()`→`inet_makenetandmask()` 只负责填查询掩码，`rtmsg()` 把
///    DST + 该掩码发给内核；`RTM_GET` 落在内核的 `rtalloc1()`（最长前缀匹配）上。
///    所以 `-net 192.0.0.0/1` 会拿到覆盖它的 `128.0.0.0/1`，`-net X/32` 会拿到覆盖 X
///    的任意路由（哪怕 X 自己有一条更具体的 host 路由，见上）。
/// 3. 现场日志（`~/Library/Application Support/com.xraytun.desktop/logs/app.jsonl`
///    ts `1790476849`/`1790476863`）里确有 `0/1 → en0` 残留：回滚把
///    `InstalledRoute::replaced` 记成了**别的**路由（现场报告的结论是 `/0` 默认路由
///    那条），于是 `Restore` 执行 `route add -net 0.0.0.0/1 <物理网关>`，
///    把半个 IPv4 空间钉在物理网卡上（代理在用时会绕过隧道 = 流量泄漏）。
///    诚实边界：快照/日志不落 `replaced`，且我实测时机器的 `0/1` 正在（隧道在跑），
///    所以「旧路径在 `0/1` 缺失时到底退化成 `/0` 还是 `not in table`」没能在现场复现；
///    但上面的第 1 条已经证明「`route -n get -net` 不是网络号精确」，
///    新判据把这条路径整个换掉，任何退化形态都不会再产出错误的 `replaced`。
///
/// # 现在的判据
///
/// 解析 `netstat -rn -f inet|inet6`，把 Destination 列还原成 `(网络号, 前缀长度)`，
/// 与查询**两者都相等**才算查到（见 [`parse_netstat_destination`] 对键格式的说明）。
/// 因此 `0.0.0.0/1` 永远不会拿到 `/0` 默认路由，`128.0.0.0/1` 也不会拿到别的 /1。
///
/// # fail-open
///
/// `netstat` 跑不起来、或某一行的键解析不出来 ⇒ 这一行不算，匹配不到就返回 `None`
/// （=「原本这个前缀上什么都没有」）。这个方向是刻意选的：回滚只会**删**我们装的那条，
/// 不会凭空造出一条；失败代价是漏掉一次「恢复被顶掉的路由」（空洞），
/// 而不是留下一条劫持半个地址空间的残留（泄漏）。两者相比，泄漏更严重。
pub fn existing_route(destination: &Cidr) -> Option<ExistingRoute> {
    let family = if destination.addr.is_ipv6() { "inet6" } else { "inet" };
    match run(crate::tools::NETSTAT, &args(&["-rn", "-f", family])) {
        Ok(table) => existing_route_in_netstat(&table, destination),
        Err(e) => {
            tracing::warn!(%destination, error = %e, "读路由表失败，按「原本没有」处理（fail-open）");
            None
        }
    }
}

/// 从 `netstat -rn -f <family>` 的表文本里按**网络号 + 前缀长度精确**取一条路由。
///
/// 纯函数：真实 `netstat` 输出可以直接当测试输入，也方便用替身执行器单测。
/// 匹配的是**第一行**命中的（同一前缀出现多条时如此，与 `route add` 的替换语义
/// 之间存在无法从文本消除的歧义 —— 记下来的是我们能看到的第一条）。
fn existing_route_in_netstat(table: &str, destination: &Cidr) -> Option<ExistingRoute> {
    let want = destination.network();
    for line in table.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue; // 表头 / 空行 / 统计行
        }
        let (dest, gateway, flags, netif) = (f[0], f[1], f[2], f[3]);
        // `L`(RTF_LLINFO) 是 ARP/邻居缓存，不是可安装的路由：它的 Gateway 列是 MAC，
        // 混进来只会还原出一条假的接口路由。
        if flags.contains('L') {
            continue;
        }
        let Some((net, prefix)) = parse_netstat_destination(dest) else {
            continue;
        };
        if net == want.addr && prefix == want.prefix {
            return Some(ExistingRoute {
                interface: Some(netif.to_string()),
                // `link#14` / 裸 MAC / `fe80::%utun0` 都解析不成 IpAddr ⇒ `None`
                // ⇒ `to_via()` 还原成 `Interface{netif}`，与旧 `route get` 行为一致。
                gateway: gateway.parse::<IpAddr>().ok(),
            });
        }
    }
    None
}

/// 把 `netstat -rn` 的 Destination 列还原成 `(网络号, 前缀长度)`。
///
/// 键格式**不是猜的**：来自 macOS `netstat` 的格式化代码
/// （`network_cmds/netstat.tproj/route.c` 的 `netname()` / `netname6()` / `domask()`）：
///
/// * `default` = `0.0.0.0/0`；
/// * IPv4 网络路由会**省掉全零的后段**：`10` = 10.0.0.0/8、`169.254` = 169.254.0.0/16、
///   `192.168.0` = 192.168.0.0/24；
/// * 掩码不等于该地址的「自然类掩码」（A 类 /8、B 类 /16、其余 /24，即
///   `forgemask()`）时补 `/P`：`0/1`、`128.0/1`、`100.64/10`、`172.16/12`、
///   `192.168.0/16`、`192.168.0.1/32`；
/// * 主机路由（RTF_HOST）走 `routename()`，是**裸四段、不带 `/32`**：
///   `34.135.247.136`；而带 `/32` 后缀的（`192.168.0.1/32`）是掩码为 /32 的**网络**路由；
/// * IPv6 网络路由一定带 `/P`（`fe80::/10`、`fe80::%en0/64`），主机路由是裸地址、
///   可能带 `%en0` 作用域；`default` 同样表示 `::/0`。
///
/// 解析不出来（段数 > 4、前缀越界、非数字）⇒ `None`，调用方跳过这一行。
fn parse_netstat_destination(key: &str) -> Option<(IpAddr, u8)> {
    if key == "default" {
        return Some((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    }
    // 先切出 `/P`（`%en0` 出现在 `/P` **之前**：`fe80::%en0/64`），再从地址里
    // 去掉 `%en0` 作用域 —— `Cidr` 里没有作用域，不参与前缀比较。
    let (addr_raw, prefix_text) = match key.rsplit_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (key, None),
    };
    let addr_text = addr_raw.split('%').next().unwrap_or(addr_raw);
    match prefix_text {
        Some(p) => {
            let prefix: u8 = p.parse().ok()?;
            if let Ok(a) = addr_text.parse::<Ipv4Addr>() {
                return Some((IpAddr::V4(mask_v4(a, prefix)?), prefix));
            }
            if let Ok(a) = addr_text.parse::<Ipv6Addr>() {
                return Some((IpAddr::V6(mask_v6(a, prefix)?), prefix));
            }
            // `0/1`、`128.0/1` 这类省掉后段的 IPv4（netstat 的真实写法）。
            let a = parse_v4_shorthand(addr_text)?;
            Some((IpAddr::V4(mask_v4(a, prefix)?), prefix))
        }
        None => {
            if let Ok(a) = addr_text.parse::<Ipv4Addr>() {
                // 裸四段 = 主机路由，netstat 不给 /32 后缀。
                return Some((IpAddr::V4(a), 32));
            }
            if let Ok(a) = addr_text.parse::<Ipv6Addr>() {
                return Some((IpAddr::V6(a), 128));
            }
            let a = parse_v4_shorthand(addr_text)?;
            let prefix = natural_v4_prefix(a);
            Some((IpAddr::V4(mask_v4(a, prefix)?), prefix))
        }
    }
}

/// `netstat` 的 IPv4 目标可以只写前 1–3 段（后面的全零段省掉）：`10`、`169.254`、`192.168.0`。
fn parse_v4_shorthand(text: &str) -> Option<Ipv4Addr> {
    let mut octets = [0u8; 4];
    let mut n = 0usize;
    for part in text.split('.') {
        if n == 4 {
            return None;
        }
        octets[n] = part.parse().ok()?;
        n += 1;
    }
    (n > 0).then_some(Ipv4Addr::from(octets))
}

/// 不带 `/P` 时的「自然类掩码」：A 类 /8、B 类 /16、其余（C/D/E）/24。
/// 与 netstat 的 `forgemask()` 逐分支一致。
fn natural_v4_prefix(a: Ipv4Addr) -> u8 {
    match a.octets()[0] {
        0..=127 => 8,
        128..=191 => 16,
        _ => 24,
    }
}

/// 网络号 = 地址 & 掩码。前缀越界 ⇒ `None`（解析失败，走 fail-open）。
fn mask_v4(a: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    Some(Ipv4Addr::from(u32::from(a) & mask))
}

/// IPv6 版 [`mask_v4`]。
fn mask_v6(a: Ipv6Addr, prefix: u8) -> Option<Ipv6Addr> {
    if prefix > 128 {
        return None;
    }
    let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
    Some(Ipv6Addr::from(u128::from(a) & mask))
}

/// 解析 `route -n get` 的输出，取「默认路由」需要的两个字段。
///
/// ⚠️ 只给 [`default_route`] / [`default_route_v6`] 用（查 `/0` 时 `-net 0.0.0.0/0`
/// 与 `route get default` 等价）。**不要**拿它判断「某个前缀上有没有路由」——
/// 那件事由 [`existing_route`] 精确判断（见其文档里的实测与源码依据）。
fn parse_route_get(output: &str) -> Option<DefaultRoute> {
    let (interface, gateway) = route_get_fields(output);
    interface.map(|interface| DefaultRoute { interface, gateway })
}

/// `route -n get` 输出的字段抽取。
///
/// 返回 `(interface, gateway)`。
fn route_get_fields(output: &str) -> (Option<String>, Option<IpAddr>) {
    let mut interface = None;
    let mut gateway = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("gateway:") {
            gateway = rest.trim().parse::<IpAddr>().ok();
        } else if let Some(rest) = line.strip_prefix("interface:") {
            interface = Some(rest.trim().to_string());
        }
    }
    (interface, gateway)
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
/// * `0/1` / `128.0/1 ... <netif>`：捕获路由指向哪个接口。**键的实际写法**见
///   [`parse_netstat_destination`]：`128/1`、`128.0/1`、`128.0.0.0/1` 都认（真机是 `128.0/1`）；
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
        if dest == "default" {
            audit.default_rows += 1;
            if flags.contains('I') && netif == physical_interface {
                audit.scoped_default_gateway = Some(gateway.to_string());
            }
            continue;
        }
        // 捕获路由用与 `existing_route` **同一个**前缀解析器，避免两处各写一套判断
        // （曾经的 bug：审计只认 `128/1`/`128.0.0.0/1`，认不出真机的 `128.0/1`，
        //  于是一条 `128/1 → en0` 残留会被报成「缺失」）。
        match parse_netstat_destination(dest) {
            Some((IpAddr::V4(a), 1)) if a == Ipv4Addr::UNSPECIFIED => {
                audit.capture_0_1 = Some(netif.to_string());
                continue;
            }
            Some((IpAddr::V4(a), 1)) if a == Ipv4Addr::new(128, 0, 0, 0) => {
                audit.capture_128_1 = Some(netif.to_string());
                continue;
            }
            _ => {}
        }
        if dest.parse::<Ipv4Addr>().is_ok()
            && flags.contains('G')
            && flags.contains('H')
            && netif == physical_interface
        {
            audit.bypass_hosts.push((dest.to_string(), gateway.to_string()));
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
    use crate::macos::with_executor;
    use crate::plan::{rollback_plan, RollbackAction};
    use std::cell::RefCell;
    use std::rc::Rc;
    use xt_proto::InstalledRoute;

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
    // task-85 / P1：查「**这个前缀**上原本有没有路由」（回滚时要不只删除、还要恢复）
    //
    // 关键回归：`replaced` 必须是**同一网络号 + 同一前缀长度**的那条（或 None）。
    // 记成别的（例如 `/0` 默认路由）⇒ 回滚的 `Restore` 会在本前缀上造出
    // `0/1 → en0` 残留，把半个 IPv4 空间钉在物理网卡上（app.jsonl
    // ts 1790476849/1790476863 的现场）。
    // -----------------------------------------------------------------------

    /// netstat 的 Destination 键格式（真机实测 + netstat 源码 `netname()`/`domask()`）。
    #[test]
    fn netstat_destination_keys_parse_to_their_real_prefixes() {
        let v4 = |ip: &str, p: u8| (IpAddr::V4(ip.parse().unwrap()), p);
        let v6 = |ip: &str, p: u8| (IpAddr::V6(ip.parse().unwrap()), p);
        for (key, want) in [
            ("default", v4("0.0.0.0", 0)),
            ("0/1", v4("0.0.0.0", 1)),
            ("128.0/1", v4("128.0.0.0", 1)),
            ("128/1", v4("128.0.0.0", 1)),
            ("128.0.0.0/1", v4("128.0.0.0", 1)),
            ("10", v4("10.0.0.0", 8)),
            ("169.254", v4("169.254.0.0", 16)),
            ("192.168.0", v4("192.168.0.0", 24)),
            ("100.64/10", v4("100.64.0.0", 10)),
            ("172.16/12", v4("172.16.0.0", 12)),
            ("192.168.0/16", v4("192.168.0.0", 16)),
            ("224.0.0/4", v4("224.0.0.0", 4)),
            ("192.168.0.1/32", v4("192.168.0.1", 32)),
            // 裸四段 = RTF_HOST 主机路由（netstat 不给 `/32` 后缀）
            ("34.135.247.136", v4("34.135.247.136", 32)),
            ("fe80::/10", v6("fe80::", 10)),
            ("fe80::%en0/64", v6("fe80::", 64)),
            ("::1", v6("::1", 128)),
        ] {
            assert_eq!(parse_netstat_destination(key), Some(want), "键 {key}");
        }
        // 解析不出来 ⇒ None（fail-open 到「原本没有」）
        assert_eq!(parse_netstat_destination("0.0.0.0/33"), None);
        assert_eq!(parse_netstat_destination("not-a-key"), None);
    }

    /// 回滚要恢复的那条必须**同前缀**：`0.0.0.0/1` 该拿到 `0/1` 那条，
    /// `0.0.0.0/0` 该拿到 `default` 那条，两者不许串。
    #[test]
    fn existing_route_matches_only_the_exact_prefix() {
        let table = "\
Routing tables

Internet:
Destination        Gateway            Flags        Netif Expire
0/1                utun6              UScg                utun6
default            192.168.0.1        UGScg                 en0
128.0/1            utun6              USc                 utun6
192.168.0/16       192.168.0.1        UGSc                  en0
";
        let half: Cidr = "0.0.0.0/1".parse().unwrap();
        let other_half: Cidr = "128.0.0.0/1".parse().unwrap();
        let whole: Cidr = "0.0.0.0/0".parse().unwrap();

        assert_eq!(
            existing_route_in_netstat(table, &half).and_then(|r| r.to_via()),
            Some(RouteVia::Interface { name: "utun6".into() }),
            "0.0.0.0/1 必须拿到 0/1 那条（接口路由 ⇒ Interface）",
        );
        assert_eq!(
            existing_route_in_netstat(table, &other_half).and_then(|r| r.to_via()),
            Some(RouteVia::Interface { name: "utun6".into() }),
            "128.0.0.0/1 必须拿到真机写法 128.0/1 那条",
        );
        assert_eq!(
            existing_route_in_netstat(table, &whole).and_then(|r| r.to_via()),
            Some(RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() }),
            "0.0.0.0/0 拿到的才是 default 那条 —— 它不是 0.0.0.0/1 的 replaced",
        );
        // 表里根本没有的网络，即便同为 /1 也不许冒名顶替。
        // （注意 `64.0.0.0/1` 不是规范前缀：`network()` 会把它归一到 `0.0.0.0/1`，
        //  那本来就该命中；这里用 `64.0.0.0/2`，它确实是规范前缀且表里没有。）
        let absent: Cidr = "64.0.0.0/2".parse().unwrap();
        assert_eq!(existing_route_in_netstat(table, &absent), None);
    }

    /// 主机路由：旧实现走 `route -n get -net X/32`，真机上**查不到**表里明明存在的
    /// UGHS（实测 `route -n get -net 34.135.247.136/32` → `not in table`）；
    /// netstat 精确匹配能认，且 `link#`/MAC 不会解析成网关。
    #[test]
    fn existing_route_finds_host_routes_and_ignores_arp_gateways() {
        let table = "\
Routing tables

Internet:
Destination        Gateway            Flags        Netif Expire
default            192.168.0.1        UGScg                 en0
192.168.0.1/32     link#14            UCS                   en0
34.135.247.136     192.168.0.1        UGHS                  en0
192.168.0.1        f8:ce:21:e5:36:2a  UHLWI                 en0   1194
";
        assert_eq!(
            existing_route_in_netstat(table, &"192.168.0.1/32".parse().unwrap()).and_then(|r| r.to_via()),
            Some(RouteVia::Interface { name: "en0".into() }),
            "link# 不是网关 ⇒ 接口路由",
        );
        assert_eq!(
            existing_route_in_netstat(table, &"34.135.247.136/32".parse().unwrap())
                .and_then(|r| r.to_via()),
            Some(RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() }),
            "带网关的主机路由必须还原成 Gateway（旧实现整条漏掉）",
        );
    }

    /// ARP/邻居缓存（flags 含 `L`）不是可安装的路由：只有它时不许当成「原本有」。
    #[test]
    fn arp_neighbour_entries_are_not_existing_routes() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
192.168.0.77       f8:ce:21:e5:36:2a  UHLWI                 en0   1194
";
        assert_eq!(
            existing_route_in_netstat(table, &"192.168.0.77/32".parse().unwrap()),
            None,
        );
    }

    /// 前缀上真的没有路由 ⇒ `None`（回滚只删不造）。
    #[test]
    fn a_prefix_without_a_route_is_none() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
10                 192.168.0.1        UGSc                  en0
127                127.0.0.1          UCS                   lo0
";
        assert_eq!(
            existing_route_in_netstat(table, &"192.168.0.0/16".parse().unwrap()),
            None,
        );
        assert_eq!(existing_route_in_netstat("", &"0.0.0.0/1".parse().unwrap()), None);
    }

    // -----------------------------------------------------------------------
    // P1 判据：用可注入执行器/替身（**不碰真机路由**）
    // -----------------------------------------------------------------------

    /// 现场报告里 `route -n get -net 0.0.0.0/1` 的形态：**退回 `/0` 默认路由**
    /// （`mask: default`）。旧实现把它当成 `0.0.0.0/1` 的 `replaced`，
    /// 回滚 `Restore` 于是执行 `route add -net 0.0.0.0/1 <物理网关>`，
    /// 留下 `0/1 → en0`。
    const BUGGY_GET_FOR_SLASH1: &str = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.0.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>
";

    /// 一台**内存里的假路由表**：所有 `route`/`netstat` 调用只读它/改它。
    #[derive(Default)]
    struct FakeRouteTable {
        /// netstat 行：`(Destination, Gateway, Flags, Netif)`。
        rows: RefCell<Vec<(String, String, String, String)>>,
        /// 旧路径 `route -n get -net ...` 要读的输出（新路径根本不调用它）。
        get_output: RefCell<String>,
    }

    impl FakeRouteTable {
        fn new() -> Rc<Self> {
            let me = Rc::new(Self::default());
            me.rows.borrow_mut().push((
                "default".into(),
                "192.168.0.1".into(),
                "UGScg".into(),
                "en0".into(),
            ));
            me
        }

        /// netstat 的真实 Destination 键写法：`0.0.0.0/1` 打成 `0/1`、
        /// `128.0.0.0/1` 打成 `128.0/1`、`0.0.0.0/0` 打成 `default`。
        fn key(cidr: &str) -> String {
            match cidr {
                "0.0.0.0/1" => "0/1".into(),
                "128.0.0.0/1" => "128.0/1".into(),
                "0.0.0.0/0" => "default".into(),
                other => other.to_string(),
            }
        }

        fn netstat(&self) -> String {
            let mut s = String::from(
                "Routing tables\n\nInternet:\nDestination        Gateway            Flags               Netif Expire\n",
            );
            for (d, g, fl, i) in self.rows.borrow().iter() {
                s.push_str(&format!("{d:<18} {g:<18} {fl:<19} {i}\n"));
            }
            s
        }

        /// 只认识本测试用到的命令形态；其余命令直接报错（替身不该被意外调用）。
        fn executor(self: &Rc<Self>, tunnel: &str) -> crate::macos::TestExecutor {
            let me = Rc::clone(self);
            let tunnel = tunnel.to_string();
            Rc::new(move |program: &str, argv: &[String]| -> Result<String> {
                if program == crate::tools::NETSTAT {
                    return Ok(me.netstat());
                }
                if program != crate::tools::ROUTE {
                    return Err(Error::Invalid(format!("替身收到意外命令 {program}")));
                }
                let action = argv.get(1).map(String::as_str).unwrap_or("");
                // argv = ["-n", <action>, "-net"|"-host", <dest>, <via...>]
                let dest = argv.get(3).cloned().unwrap_or_default();
                match action {
                    "get" => Ok(me.get_output.borrow().clone()),
                    "add" => {
                        let via_args = &argv[4..];
                        if via_args.iter().any(|a| a == "-interface") {
                            // 接口路由：netstat 的 Gateway 列就是接口名
                            me.rows.borrow_mut().push((
                                Self::key(&dest),
                                tunnel.clone(),
                                "UScg".into(),
                                tunnel.clone(),
                            ));
                        } else {
                            let gw = via_args.first().cloned().unwrap_or_default();
                            me.rows.borrow_mut().push((
                                Self::key(&dest),
                                gw,
                                "UGSc".into(),
                                "en0".into(),
                            ));
                        }
                        Ok(String::new())
                    }
                    "delete" => {
                        let key = Self::key(&dest);
                        me.rows.borrow_mut().retain(|(d, ..)| *d != key);
                        Ok(String::new())
                    }
                    other => Err(Error::Invalid(format!("替身收到意外 action {other}"))),
                }
            })
        }
    }

    /// **判据 2（改前红）**：`replaced` 必须是查询前缀**自身**那条（这里是「没有」），
    /// 不是 `/0`。
    #[test]
    fn replaced_for_a_capture_half_is_never_the_default_route() {
        let me = FakeRouteTable::new();
        *me.get_output.borrow_mut() = BUGGY_GET_FOR_SLASH1.to_string();
        let (half, whole) = with_executor(me.executor("utun6"), || {
            (
                existing_route(&"0.0.0.0/1".parse().unwrap()),
                existing_route(&"0.0.0.0/0".parse().unwrap()),
            )
        });
        assert_eq!(
            half, None,
            "查 0.0.0.0/1 时把它记成 /0 默认路由 ⇒ 回滚会造出 0/1 → en0 残留",
        );
        // 正向对照：/0 自己那条仍然查得到（证明查询逻辑没被整个废掉）。
        assert_eq!(
            whole,
            Some(ExistingRoute {
                interface: Some("en0".into()),
                gateway: Some("192.168.0.1".parse().unwrap()),
            }),
        );
    }

    /// **判据 2 的实测评据（改前红）**：本机 `route -n get -net 34.135.247.136/32`
    /// 实测回的是 `0/1` 捕获路由（`destination: default` / `mask: 128.0.0.0` /
    /// `interface: utun6`），而表里明明有那条 `34.135.247.136 192.168.0.1 UGHS en0`。
    ///
    /// 旧实现会把 `0/1 → utun6` 记成这个 `/32` 的 `replaced` ⇒ 回滚执行
    /// `route add -host 34.135.247.136 -interface utun6`：把**代理服务器 IP 装进隧道**
    /// （旁路失效 / 路由环）。新判据只认同网络号 + 同前缀长度 ⇒ `None`。
    #[test]
    fn existing_route_never_mistakes_a_capture_half_for_a_host_route() {
        let me = FakeRouteTable::new();
        me.rows.borrow_mut().push((
            "0/1".into(),
            "utun6".into(),
            "UScg".into(),
            "utun6".into(),
        ));
        // 上面那条 live 样本：注意 destination 是 `default`（= 0.0.0.0）、mask 是 /1。
        *me.get_output.borrow_mut() = "\
   route to: 34.135.247.136
destination: default
       mask: 128.0.0.0
  interface: utun6
      flags: <UP,DONE,STATIC,PRCLONING,GLOBAL>
"
        .to_string();
        let got = with_executor(me.executor("utun6"), || {
            existing_route(&"34.135.247.136/32".parse().unwrap())
        });
        assert_eq!(
            got, None,
            "0/1 那条不是 34.135.247.136/32 的 replaced：装回去会把代理 IP 塞进隧道",
        );
    }

    /// **判据 1（改前红）**：装 capture 路由（`0/1` + `128/1`）再回滚之后，
    /// 两半都不得还挂在物理网卡（en0）上。
    #[test]
    fn rollback_of_capture_routes_leaves_no_half_on_the_physical_nic() {
        let me = FakeRouteTable::new();
        *me.get_output.borrow_mut() = BUGGY_GET_FOR_SLASH1.to_string();
        let tunnel = "utun6";

        let after = with_executor(me.executor(tunnel), || {
            // 与 controller.rs 的 install_route_or_skip / rollback 同形的两步。
            let mut installed: Vec<InstalledRoute> = Vec::new();
            for cidr in ["0.0.0.0/1", "128.0.0.0/1"] {
                let destination: Cidr = cidr.parse().unwrap();
                let via = RouteVia::Interface { name: tunnel.to_string() };
                let replaced = existing_route(&destination).and_then(|r| r.to_via());
                add(&destination, &via).unwrap();
                installed.push(InstalledRoute { destination, via, replaced });
            }
            for action in rollback_plan(&installed) {
                match action {
                    RollbackAction::Delete { destination, via } => {
                        delete(&destination, &via).unwrap();
                    }
                    RollbackAction::Restore { destination, via } => {
                        add(&destination, &via).unwrap();
                    }
                }
            }
            me.netstat()
        });

        let leftovers: Vec<String> = after
            .lines()
            .filter(|l| {
                l.starts_with("0/1") || l.starts_with("128.0/1") || l.starts_with("128/1")
            })
            .map(|l| l.trim_end().to_string())
            .collect();
        assert!(
            leftovers.is_empty(),
            "回滚后 0/1、128/1 都不许还在（更不许挂在 en0）：{leftovers:?}",
        );
    }

    /// 正向对照（改前改后都该绿）：同前缀那条真的在时，必须记下来。
    #[test]
    fn existing_route_reads_the_exact_prefix_through_the_executor() {
        let me = FakeRouteTable::new();
        me.rows.borrow_mut().push((
            "0/1".into(),
            "utun6".into(),
            "UScg".into(),
            "utun6".into(),
        ));
        let got = with_executor(me.executor("utun6"), || {
            existing_route(&"0.0.0.0/1".parse().unwrap())
        });
        assert_eq!(
            got,
            Some(ExistingRoute { interface: Some("utun6".into()), gateway: None }),
            "0/1 那条要被认出来（接口路由 ⇒ gateway None）",
        );
    }

    /// fail-open：`netstat` 跑不起来 ⇒ `None`，绝不凭空造一条 `replaced`。
    #[test]
    fn netstat_failure_fails_open_to_no_replaced_route() {
        let exec: crate::macos::TestExecutor = Rc::new(|_program: &str, _argv: &[String]| {
            Err(Error::Command {
                program: "netstat".into(),
                args: vec![],
                code: None,
                stderr: "boom".into(),
            })
        });
        let got = with_executor(exec, || existing_route(&"0.0.0.0/1".parse().unwrap()));
        assert_eq!(got, None);
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
        // **真机写法是 `128.0/1`**（不是 `128/1`、也不是 `128.0.0.0/1`）——
        // 旧解析器只认后两种，会把一条 `128/1 → en0` 残留误报成「缺失」。
        let table =
            format!("{INCIDENT_TABLE}128.0/1              utun6              UScg                utun6\n");
        let a = parse_netstat_inet(&table, "en0");
        assert_eq!(a.capture_0_1.as_deref(), Some("utun6"));
        assert_eq!(a.capture_128_1.as_deref(), Some("utun6"), "必须认得真机键 128.0/1");
        // ARP 邻居（UHLWI，没有 G）不许被算成节点旁路
        assert_eq!(a.bypass_hosts.len(), 2, "{:?}", a.bypass_hosts);
        assert!(a.bypass_hosts.iter().all(|(_, gw)| gw == "192.168.0.1"));
    }

    /// **判据 1 的观测面（改前红）**：残留 `128.0/1 → en0` 必须被审计**看见**
    /// —— 否则「回滚之后不许还挂在物理网卡上」这条判断在日志里根本没法验证。
    #[test]
    fn route_audit_sees_a_128_half_residue_on_the_physical_nic() {
        let table = "\
Routing tables

Internet:
Destination        Gateway            Flags        Netif Expire
0/1                en0                UScg                 en0
128.0/1            en0                USc                  en0
default            192.168.0.1        UGScg                 en0
";
        let a = parse_netstat_inet(table, "en0");
        assert_eq!(a.capture_0_1.as_deref(), Some("en0"), "0/1 残留必须看得见");
        assert_eq!(a.capture_128_1.as_deref(), Some("en0"), "128.0/1 残留必须看得见");
    }

    #[test]
    fn route_audit_rejects_a_bad_interface_name() {
        assert!(current_route_audit("-rf").is_err());
    }
}
