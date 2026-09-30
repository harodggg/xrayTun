//! 路由表操作：**默认上行发现 + 精确前缀查询 + 可增删的一对路由**。
//!
//! 这个文件里的每条策略都是踩坑之后固化的，读之前先看结论：
//!
//! 1. **覆盖默认路由用 `0.0.0.0/1` + `128.0.0.0/1`，绝不删默认路由。**
//!    这两条比 `/0` 更具体，因此对所有目的地址都先于默认路由命中；而原默认路由
//!    完好无损。好处是回滚只要删这两条 —— 即使 helper 被 `kill -9`，内核也会在
//!    utun 消失时自动清理挂在该接口上的路由，不会留下「网断了但不知道为什么」。
//!    本模块不负责这条策略本身（那是 `imp::mod` 的编排），只保证 `add`/`delete`
//!    能把任意前缀（含 `/1`）正确地表达成 `route(8)` 的 argv。
//!
//! 2. **查「恰好这个前缀（网络号 + 前缀长度都相等）上原本有什么」必须用
//!    `netstat -rn -f inet|inet6` 解析，不能用 `route -n get -net <cidr>`。**
//!    `route -n get -net` 走的是内核 `rtalloc1()`（**最长前缀匹配**），
//!    `-net 192.0.0.0/1` 会返回覆盖它的 `128.0.0.0/1`，`-net X/32` 会返回覆盖 X 的
//!    任意路由（哪怕表里有更具体的 host 路由也照错）。旧实现把它当「精确前缀查找」，
//!    于是回滚把 `/1` 的 `replaced` 记成了 `/0` 默认路由，执行
//!    `route add -net 0.0.0.0/1 <物理网关>`，把半个 IPv4 空间钉在物理网卡上
//!    （代理在用时会绕过隧道 = 流量泄漏）。判据细节见 [`existing_route`]。
//!
//! 3. **`delete` 把「路由本就不存在」当成功。** 回滚路径上这个错误必须被吃掉，
//!    否则一次部分失败会让整个回滚中断，留下更糟的半残状态。
//!
//! 4. **目标一律先 [`Cidr::network`] 归一化**：`Cidr` 刻意保留主机位（接口地址需要），
//!    但把 `10.1.2.3/8` 直接喂给 `route(8)` 时它按哪个网络号处理是依赖实现细节的。
//!    `/32`、`/128` 用 `-host` + **裸地址**（带 `/32` 会被 `route(8)` 判 `bad address`）。
//!
//! 所有外部命令都是**绝对路径 + argv 数组**（见 [`tools`]），永不经过 shell；
//! 唯一从外部进来的字符串是接口名，且先过 [`validate_interface_name`]。
//!
//! 本模块只在 macOS 上编译（`imp` 是 `#[cfg(target_os = "macos")]`）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use xt_contract::error::{bad_request, not_found, ErrorBody};

use crate::imp::{args, run, run_ok, tools};
use crate::model::{Cidr, PhysicalUplink, RouteVia};
use crate::validate::validate_interface_name;

// ---------------------------------------------------------------------------
// 默认上行发现
// ---------------------------------------------------------------------------

/// 发现当前的物理上行（默认路由指向的网卡 + 网关）。
///
/// 用 `route -n get default` 而不是解析 `netstat -rn`：前者输出稳定、字段带标签，
/// 且不受路由表规模影响。`-n` 保证不触发反向 DNS 解析（既快又不会被 DNS 改动影响）。
///
/// **网关可能缺失**：接口路由（例如默认路由直接挂在 p2p/utun 上）没有 `gateway:` 行，
/// 此时 `gateway` 是 `None`。调用方（`imp::tun_up`）需要网关时会自己把它当失败处理。
pub(crate) fn discover_physical() -> Result<crate::model::PhysicalUplink, ErrorBody> {
    let out = run(tools::ROUTE, &args(&["-n", "get", "default"]))?;
    parse_physical_uplink(&out)
        .ok_or_else(|| not_found("找不到默认路由：`route -n get default` 没有 interface 行"))
}

/// 解析 `route -n get default` 的输出（**纯函数**，真机样例可直接当测试输入）。
///
/// 只认带标签的两行：`gateway:` 与 `interface:`。没有 `interface:` 就等于没查到
/// 默认路由（返回 `None`）；没有 `gateway:` 是正常的接口路由（`gateway: None`）。
fn parse_physical_uplink(output: &str) -> Option<PhysicalUplink> {
    let (interface, gateway) = route_get_fields(output);
    interface.map(|interface| PhysicalUplink { interface, gateway })
}

/// `route -n get ...` 输出的字段抽取：返回 `(interface, gateway)`。
fn route_get_fields(output: &str) -> (Option<String>, Option<IpAddr>) {
    let mut interface = None;
    let mut gateway = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("gateway:") {
            // 解析不出来（例如 `link#14`）⇒ None，不瞎猜一个网关出来。
            gateway = rest.trim().parse::<IpAddr>().ok();
        } else if let Some(rest) = line.strip_prefix("interface:") {
            interface = Some(rest.trim().to_string());
        }
    }
    (interface, gateway)
}

// ---------------------------------------------------------------------------
// 增 / 删
// ---------------------------------------------------------------------------

/// 安装一条路由。目标已由 [`Cidr`] 表达，argv 构造见 [`build_args`]。
pub(crate) fn add(
    destination: &crate::model::Cidr,
    via: &crate::model::RouteVia,
) -> Result<(), ErrorBody> {
    run_ok(tools::ROUTE, &build_args("add", destination, via)?)
}

/// 删除一条路由。
///
/// 「路由不存在」**不算失败**：回滚路径上这个错误必须被吃掉，否则一次部分失败会让
/// 整个回滚中断，留下更糟的半残状态。判据是 `route(8)` 的 stderr（见
/// [`is_missing_route`]）；其余错误原样返回，绝不吞。
pub(crate) fn delete(
    destination: &crate::model::Cidr,
    via: &crate::model::RouteVia,
) -> Result<(), ErrorBody> {
    let argv = build_args("delete", destination, via)?;
    match run_ok(tools::ROUTE, &argv) {
        Ok(()) => Ok(()),
        Err(e) if stderr_of(&e).is_some_and(is_missing_route) => Ok(()),
        Err(e) => Err(e),
    }
}

/// 从 `run` 的 `ErrorBody.detail` 里取出 `stderr`（`imp::run` 失败时一定填它）。
fn stderr_of(error: &ErrorBody) -> Option<&str> {
    error.detail.as_ref()?.get("stderr")?.as_str()
}

/// 「这条路由本来就不在表里」的三种 `route(8)` 文案（大小写不敏感）。
///
/// * `not in table`：删除一条不存在的路由；
/// * `no such process`：内核在某些路径上对不存在的路由返回的等价文案；
/// * `bad address`：目标地址形态不被接受（例如已经被前一步清掉后重试）。
fn is_missing_route(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("not in table") || s.contains("no such process") || s.contains("bad address")
}

/// 构造 `route(8)` 的 argv（**纯函数**，可单测）。
///
/// ```text
/// -n <add|delete> [-inet6] -host <裸地址>        -interface <name>
/// -n <add|delete> [-inet6] -net  <网络号/前缀>   <gateway>
/// ```
///
/// 关键点：
/// * `/32`、`/128` 用 `-host` + **裸地址**。实测 `route -n add -host
///   255.255.255.255/32 ...` 直接 `bad address`（退出码 68）；某些地址带 `/32`
///   恰好能过是 `getaddr()` 的解析细节，不是文档承诺的行为 —— 依赖它等于埋雷。
/// * 其余用 `-net` + `网络号/前缀`（标准写法）。
/// * 网关地址族必须与目标一致，否则 `route(8)` 拒绝；这里提前判并给明确错误。
fn build_args(action: &str, destination: &Cidr, via: &RouteVia) -> Result<Vec<String>, ErrorBody> {
    // 路由目标必须是网络地址：`Cidr` 保留主机位是接口地址的需要，归一只在这里做。
    let dest = destination.network();
    let mut v: Vec<String> = vec!["-n".into(), action.into()];

    if dest.is_ipv6() {
        v.push("-inet6".into());
    }

    let is_host = destination.prefix == if dest.is_ipv4() { 32 } else { 128 };
    if is_host {
        // `-host` 只接受裸地址（不带 `/32`、`/128`）。
        v.push("-host".into());
        v.push(dest.to_string());
    } else {
        v.push("-net".into());
        v.push(format!("{dest}/{}", destination.prefix));
    }

    match via {
        RouteVia::Interface { name } => {
            // 唯一来自外部的字符串：注入 `-rf` / `utun0; rm` 这类值必须在进 argv 前拦下。
            validate_interface_name(name)?;
            v.push("-interface".into());
            v.push(name.clone());
        }
        RouteVia::Gateway { addr } => {
            if addr.is_ipv4() != dest.is_ipv4() {
                return Err(bad_request(format!(
                    "目标 {destination} 与网关 {addr} 地址族不匹配"
                )));
            }
            v.push(addr.to_string());
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// 精确前缀查询（老仓库最重要的教训）
// ---------------------------------------------------------------------------

/// 查**恰好这个前缀（网络号 + 前缀长度都相等）**上原本有没有路由。
///
/// # 为什么不用 `route -n get -net <cidr>`
///
/// 那**不是**精确前缀查找：`route(8)` 的 `rtmsg()` 把 DST + 掩码发给内核，
/// `RTM_GET` 落在 `rtalloc1()`（最长前缀匹配）上。实测（隧道在跑，
/// 表里有 `0/1 → utun6` 与 `128.0/1 → utun6`）：
///
/// ```text
/// $ route -n get -net 0.0.0.0/1      → destination: default / mask: 128.0.0.0 / utun6
/// $ route -n get -net 192.0.0.0/1    → 表里没有，却回 128.0.0.0/1 那条
/// $ route -n get -net 34.135.247.136/32 → 表里有 UGHS，却回 0/1 捕获路由
/// $ route -n get -net 0.0.0.0/2      → route: writing to routing socket: not in table
/// ```
///
/// 同一条 `/32` 查询在不同采样里结果还会漂移。把它当 `replaced` 的来源，
/// 回滚就会在别的网段上造出残留（`0/1 → en0` 的泄漏现场就是这么来的）。
///
/// # 现在的判据
///
/// 解析 `netstat -rn -f inet|inet6`，把 Destination 列还原成 `(网络号, 前缀长度)`，
/// 与查询**两者都相等**才算查到（键格式见 [`parse_netstat_destination`]）。
/// 因此 `0.0.0.0/1` 永远不会拿到 `/0` 默认路由，`128.0.0.0/1` 也不会拿到别的 `/1`。
///
/// # fail-open
///
/// `netstat` 跑不起来、或某一行键解析不出来 ⇒ 跳过该行；匹配不到就返回 `None`
/// （=「原本这个前缀上什么都没有」）。方向是刻意选的：回滚只会**删**我们装的那条，
/// 不会凭空造出一条。失败的代价是漏掉一次「恢复被顶掉的路由」（空洞），
/// 而不是留下一条劫持半个地址空间的残留（泄漏）—— 两者相比，泄漏更严重。
/// 也正因为是 fail-open，这里失败不写日志、不返回错误（本 crate 也不带日志依赖）。
pub(crate) fn existing_route(destination: &crate::model::Cidr) -> Option<crate::model::RouteVia> {
    let family = if destination.addr.is_ipv6() { "inet6" } else { "inet" };
    let table = run(tools::NETSTAT, &args(&["-rn", "-f", family])).ok()?;
    existing_route_in_netstat(&table, destination)
}

/// 从 `netstat -rn -f <family>` 的表文本里按**网络号 + 前缀长度精确**取一条路由。
///
/// 纯函数：真实 `netstat` 输出可以直接当测试输入。
/// 同一前缀出现多条时取**第一行**（文本里已无法消除与 `route add` 替换语义之间的
/// 歧义 —— 记下来的是我们能看到的第一条）。
fn existing_route_in_netstat(table: &str, destination: &Cidr) -> Option<RouteVia> {
    let want_addr = destination.network();
    let want_prefix = destination.prefix;

    for line in table.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue; // 表头 / 空行 / "Internet:" / 统计行
        }
        let (dest, gateway, flags, netif) = (f[0], f[1], f[2], f[3]);

        // `L`(RTF_LLINFO) 是 ARP/邻居缓存，不是可安装的路由：它的 Gateway 列是 MAC，
        // 混进来只会还原出一条假的接口路由。
        if flags.contains('L') {
            continue;
        }

        // `netstat -f inet6` 里的 `default` 是 `::/0`，而 `parse_netstat_destination`
        // 按 IPv4 语境把 `default` 归成 `0.0.0.0/0`（同一个词两族共用）。这里按查询
        // 的地址族补上 IPv6 的解释，否则 `::/0` 永远查不到自己那条。
        let parsed = if dest == "default" && destination.addr.is_ipv6() {
            Some((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
        } else {
            parse_netstat_destination(dest)
        };
        let Some((net, prefix)) = parsed else {
            continue; // 解析不出来 ⇒ 跳过这一行（fail-open）
        };
        if net == want_addr && prefix == want_prefix {
            // gateway 列能当 IP 读 ⇒ 网关路由；`link#14` / MAC / 带作用域的
            // link-local 读不成就落到 netif ⇒ 接口路由。
            return Some(match gateway.parse::<IpAddr>() {
                Ok(addr) => RouteVia::Gateway { addr },
                Err(_) => RouteVia::Interface { name: netif.to_string() },
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
/// * `default` = `0.0.0.0/0`（IPv6 表的 `default` 由调用方按地址族补成 `::/0`）；
/// * IPv4 网络路由会**省掉全零的后段**：`10` = 10.0.0.0/8、`169.254` = 169.254.0.0/16、
///   `192.168.0` = 192.168.0.0/24；
/// * 掩码不等于该地址的「自然类掩码」（A 类 /8、B 类 /16、其余 /24，即
///   `forgemask()`）时补 `/P`：`0/1`、`128.0/1`、`100.64/10`、`172.16/12`、
///   `192.168.0/16`、`192.168.0.1/32`；
/// * 主机路由（RTF_HOST）走 `routename()`，是**裸四段、不带 `/32`**：
///   `34.135.247.136`；而带 `/32` 后缀的（`192.168.0.1/32`）是掩码为 /32 的**网络**路由；
/// * IPv6 网络路由一定带 `/P`（`fe80::/10`、`fe80::%en0/64`），主机路由是裸地址、
///   可能带 `%en0` 作用域。
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
                // 裸四段 = 主机路由，netstat 不给 `/32` 后缀。
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
            return None; // 段数 > 4
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(text: &str) -> Cidr {
        Cidr::parse(text).expect("测试用的 CIDR 必须合法")
    }

    // -----------------------------------------------------------------------
    // 默认上行发现
    // -----------------------------------------------------------------------

    #[test]
    fn parses_default_route_with_gateway() {
        let sample = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>
";
        let up = parse_physical_uplink(sample).expect("应解析出物理上行");
        assert_eq!(up.interface, "en0");
        assert_eq!(up.gateway, Some("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn parses_default_route_without_gateway() {
        // 接口路由：没有 `gateway:` 行，gateway 必须是 None（不是瞎猜一个）。
        let sample = "   route to: default\n  interface: utun4\n";
        let up = parse_physical_uplink(sample).expect("应解析出物理上行");
        assert_eq!(up.interface, "utun4");
        assert_eq!(up.gateway, None);
    }

    #[test]
    fn default_route_without_interface_is_not_a_uplink() {
        assert!(parse_physical_uplink("   route to: default\n    gateway: 192.168.1.1\n").is_none());
    }

    #[test]
    fn unparsable_gateway_line_is_none() {
        let sample = "  gateway: link#14\n  interface: en0\n";
        let up = parse_physical_uplink(sample).unwrap();
        assert_eq!(up.gateway, None);
    }

    // -----------------------------------------------------------------------
    // argv 构造
    // -----------------------------------------------------------------------

    #[test]
    fn split_default_uses_net_flag_with_cidr() {
        let argv = build_args("add", &cidr("0.0.0.0/1"), &RouteVia::Interface { name: "utun4".into() })
            .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-net", "0.0.0.0/1", "-interface", "utun4"]);
    }

    #[test]
    fn other_half_of_default_keeps_its_network_number() {
        let argv = build_args("add", &cidr("128.0.0.0/1"), &RouteVia::Interface { name: "utun4".into() })
            .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-net", "128.0.0.0/1", "-interface", "utun4"]);
    }

    /// `-host` 必须带**裸地址**，不能带 `/32`（`route(8)` 会报 `bad address`）。
    #[test]
    fn host_route_uses_bare_address_not_cidr() {
        let argv = build_args(
            "add",
            &cidr("203.0.113.7/32"),
            &RouteVia::Gateway { addr: "192.168.1.1".parse().unwrap() },
        )
        .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-host", "203.0.113.7", "192.168.1.1"]);
        assert!(
            !argv.iter().any(|a| a.contains('/')),
            "`-host` 的参数里不能出现 '/'：{argv:?}"
        );
    }

    #[test]
    fn ipv6_host_route_is_inet6_and_bare() {
        let argv =
            build_args("add", &cidr("2001:db8::1/128"), &RouteVia::Interface { name: "utun4".into() })
                .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-inet6", "-host", "2001:db8::1", "-interface", "utun4"]);
    }

    #[test]
    fn ipv6_network_route_carries_inet6_flag() {
        let argv = build_args("add", &cidr("fe80::/10"), &RouteVia::Interface { name: "utun4".into() })
            .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-inet6", "-net", "fe80::/10", "-interface", "utun4"]);
    }

    /// 目标带主机位时必须归一化成网络号：`10.1.2.3/8` → `10.0.0.0/8`。
    #[test]
    fn destination_is_normalized_to_the_network_address() {
        let argv =
            build_args("delete", &cidr("10.1.2.3/8"), &RouteVia::Interface { name: "utun4".into() })
                .unwrap();
        assert_eq!(argv, vec!["-n", "delete", "-net", "10.0.0.0/8", "-interface", "utun4"]);
    }

    #[test]
    fn gateway_route_appends_the_bare_gateway() {
        let argv = build_args(
            "add",
            &cidr("192.168.0.0/16"),
            &RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() },
        )
        .unwrap();
        assert_eq!(argv, vec!["-n", "add", "-net", "192.168.0.0/16", "192.168.0.1"]);
    }

    #[test]
    fn rejects_cross_family_gateway() {
        assert!(build_args(
            "add",
            &cidr("0.0.0.0/1"),
            &RouteVia::Gateway { addr: "fe80::1".parse().unwrap() }
        )
        .is_err());
        assert!(build_args(
            "add",
            &cidr("::/1"),
            &RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() }
        )
        .is_err());
    }

    #[test]
    fn rejects_interface_name_injection() {
        for bad in ["-rf", "utun0; rm", "utun0/../../x", ""] {
            assert!(
                build_args("add", &cidr("0.0.0.0/1"), &RouteVia::Interface { name: bad.into() })
                    .is_err(),
                "非法接口名 {bad:?} 必须在进 argv 前被拒"
            );
        }
    }

    #[test]
    fn missing_route_messages_are_recognized() {
        assert!(is_missing_route("route: writing to routing socket: not in table"));
        assert!(is_missing_route("delete net 0.0.0.0: not in table"));
        assert!(is_missing_route("route: bad address: 255.255.255.255/32"));
        assert!(is_missing_route("route: No Such Process"));
        assert!(is_missing_route("NOT IN TABLE"));
        assert!(!is_missing_route("route: permission denied"));
        assert!(!is_missing_route(""));
    }

    #[test]
    fn stderr_is_read_from_the_error_detail() {
        let e = ErrorBody::new(xt_contract::error::ErrorCode::Io, "route 执行失败").with_detail(
            serde_json::json!({ "program": tools::ROUTE, "stderr": "not in table" }),
        );
        assert_eq!(stderr_of(&e), Some("not in table"));
        let e = ErrorBody::new(xt_contract::error::ErrorCode::Io, "无 detail");
        assert_eq!(stderr_of(&e), None);
    }

    // -----------------------------------------------------------------------
    // netstat 键解析（真机实测 + netstat 源码 netname()/domask()）
    // -----------------------------------------------------------------------

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
        assert_eq!(parse_netstat_destination("fe80::/129"), None);
        assert_eq!(parse_netstat_destination("1.2.3.4.5"), None);
        assert_eq!(parse_netstat_destination("not-a-key"), None);
        assert_eq!(parse_netstat_destination(""), None);
    }

    // -----------------------------------------------------------------------
    // 精确前缀匹配（task-85 的核心回归）
    // -----------------------------------------------------------------------

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
        assert_eq!(
            existing_route_in_netstat(table, &cidr("0.0.0.0/1")),
            Some(RouteVia::Interface { name: "utun6".into() }),
            "0.0.0.0/1 必须拿到 0/1 那条（接口路由 ⇒ Interface）",
        );
        assert_eq!(
            existing_route_in_netstat(table, &cidr("128.0.0.0/1")),
            Some(RouteVia::Interface { name: "utun6".into() }),
            "128.0.0.0/1 必须拿到真机写法 128.0/1 那条",
        );
        assert_eq!(
            existing_route_in_netstat(table, &cidr("0.0.0.0/0")),
            Some(RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() }),
            "0.0.0.0/0 拿到的才是 default 那条 —— 它不是 0.0.0.0/1 的 replaced",
        );
        // 表里根本没有的网络，即便同为 /1 也不许冒名顶替。
        assert_eq!(existing_route_in_netstat(table, &cidr("64.0.0.0/2")), None);
    }

    /// 主机路由：带网关的还原成 `Gateway`，`link#`/MAC 还原成 `Interface`。
    #[test]
    fn existing_route_finds_host_routes_and_ignores_arp_gateways() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
default            192.168.0.1        UGScg                 en0
192.168.0.1/32     link#14            UCS                   en0
34.135.247.136     192.168.0.1        UGHS                  en0
192.168.0.1        f8:ce:21:e5:36:2a  UHLWI                 en0   1194
";
        assert_eq!(
            existing_route_in_netstat(table, &cidr("192.168.0.1/32")),
            Some(RouteVia::Interface { name: "en0".into() }),
            "link# 不是网关 ⇒ 接口路由",
        );
        assert_eq!(
            existing_route_in_netstat(table, &cidr("34.135.247.136/32")),
            Some(RouteVia::Gateway { addr: "192.168.0.1".parse().unwrap() }),
            "带网关的主机路由必须还原成 Gateway",
        );
    }

    /// ARP/邻居缓存（flags 含 `L`）不是可安装的路由：只有它时不许当成「原本有」。
    #[test]
    fn arp_neighbour_entries_are_not_existing_routes() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
192.168.0.77       f8:ce:21:e5:36:2a  UHLWI                 en0   1194
";
        assert_eq!(existing_route_in_netstat(table, &cidr("192.168.0.77/32")), None);
    }

    /// 前缀上真的没有路由 ⇒ `None`（回滚只删不造）。
    #[test]
    fn a_prefix_without_a_route_is_none() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
10                 192.168.0.1        UGSc                  en0
127                127.0.0.1          UCS                   lo0
";
        assert_eq!(existing_route_in_netstat(table, &cidr("192.168.0.0/16")), None);
        assert_eq!(existing_route_in_netstat("", &cidr("0.0.0.0/1")), None);
        assert_eq!(existing_route_in_netstat("Routing tables\n\nInternet:\n", &cidr("0.0.0.0/1")), None);
    }

    /// IPv6 表里的 `default` 是 `::/0`，不能被当成 IPv4 的 `0.0.0.0/0` 而漏掉。
    ///
    /// 注意：纯解析器是**按文本**工作的，`default` 这个词两个地址族共用、文本里没有
    /// 族信息；调用方必须传入与查询地址族一致的那张表（[`existing_route`] 用
    /// `-f inet`/`-f inet6` 保证这点），所以这里不构造「拿 v4 查询读 inet6 表」
    /// 这种现实中不存在的输入。
    #[test]
    fn ipv6_default_row_matches_the_v6_zero_prefix() {
        let table = "\
Routing tables

Internet6:
Destination                             Gateway                         Flags         Netif Expire
default                                 fe80::1                         UGc               en0
fe80::%en0/64                           link#4                          UCS               en0
";
        assert_eq!(
            existing_route_in_netstat(table, &cidr("::/0")),
            Some(RouteVia::Gateway { addr: "fe80::1".parse().unwrap() }),
        );
        // 该表里那条 `fe80::%en0/64` 是前缀 /64 的接口路由，不许被 `::/0` 顶替。
        assert_eq!(
            existing_route_in_netstat(table, &cidr("fe80::/10")),
            None,
            "同一张表里只有 /64 那条，查 /10 必须落空",
        );
    }

    /// 接口路由的 Gateway 列是接口名（真机 utun 的形态）⇒ Interface，不是 Gateway。
    #[test]
    fn interface_name_in_gateway_column_becomes_interface_via() {
        let table = "\
Destination        Gateway            Flags        Netif Expire
198.18.0.0/15      utun6              USc                 utun6
";
        assert_eq!(
            existing_route_in_netstat(table, &cidr("198.18.0.0/15")),
            Some(RouteVia::Interface { name: "utun6".into() }),
        );
    }
}
