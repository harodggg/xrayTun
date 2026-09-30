//! 系统 DNS 的备份 / 设置 / 还原。
//!
//! macOS 的 DNS 是**按「网络服务」(network service) 配置的**，不是按接口：
//! `en0` 对应的服务名是 `Wi-Fi`，`en1` 可能是 `USB 10/100/1000 LAN`。
//! 所以改 DNS 之前必须先跑一次 `networksetup -listnetworkserviceorder`，
//! 把「设备 → 服务名」的映射建出来，选中**第一个带 `Device:` 的服务**
//! （即服务顺序里的主网络服务）作为操作对象。
//!
//! 本模块只暴露三个入口（签名被 [`crate::imp`] 的编排层冻结调用）：
//!
//! | 函数 | 时机 | 语义 |
//! | --- | --- | --- |
//! | [`backup`] | `commit_routes` 改 DNS **之前** | 记下「用户原值」 |
//! | [`set`] | `commit_routes` | 写入隧道 DNS；空列表 = 清空交还 DHCP |
//! | [`restore`] | `tun_down` / `restore_stale` | 按备份还原 |
//!
//! # 最容易踩的坑
//!
//! `networksetup -setdnsservers` 写进去的值**会一直留着**，即使隧道已经拆了。
//! 用户会看到「代理关了但所有网站都打不开」—— 这是这类工具最经典的差评来源。
//! 所以纪律是：**改之前一定先备份，回滚时一定还原**（备份由上层落进
//! [`crate::model::Snapshot`] 并持久化，防 helper 被 `kill -9`）。
//!
//! # 哨兵（本模块的核心不变量）
//!
//! 隧道 DNS 哨兵是 `198.18.0.2`（落在 `198.18.0.0/15`，RFC 2544 保留、公网
//! 不可路由的隧道网段里）。`networksetup -getdnsservers` 读到的值**可能是上一轮
//! 残留的哨兵**。如果照抄进备份，之后每次「还原」都是把死地址写回系统 ——
//! 问题被永久化。因此：
//!
//! 1. [`backup`] **剔除**读到的所有哨兵；剔完为空 ⇒ 原值记为**未设置**
//!    （空列表 = 交还 DHCP），而不是把哨兵记成用户原值；
//! 2. [`restore`] 对备份**再剔一次**（磁盘上的历史快照可能已被旧版污染），
//!    全部剔除后按「清空 DNS」处理；
//! 3. [`set`] 拒绝任何落在隧道网段的地址 —— 调用方不该把哨兵当"用户 DNS"传进来。
//!
//! # 安全
//!
//! 只用绝对路径 + argv 数组（[`crate::imp::tools::NETWORKSETUP`]）+ [`crate::imp::run`]，
//! **永不经过 shell**；服务名先过 [`crate::validate::validate_service_name`]。

use std::net::IpAddr;

use xt_contract::error::{ErrorBody, ErrorCode};

use crate::imp::{args, run, run_ok, tools::NETWORKSETUP};
use crate::model::DnsBackup;
use crate::validate::validate_service_name;

/// 隧道 DNS 哨兵：解析器地址被指向隧道内的这个地址，由隧道数据面接管解析。
const SENTINEL_DNS: &str = "198.18.0.2";

/// 隧道网段（RFC 2544 保留，公网不可路由）。哨兵可配置，但一定落在这个网段里。
const TUN_NETWORK_PREFIX: &str = "198.18.0.0/15";

/// 隧道网段的网络号（`198.18.0.0`）与掩码（`/15` → `255.254.0.0`），以 `u32` 写死：
/// 哨兵判据只此一处，不必为常量再做一次 CIDR 解析。
const TUN_NETWORK_V4: u32 = 0xC6_12_00_00; // 198.18.0.0
const TUN_NETMASK_V4: u32 = 0xFF_FE_00_00; // 255.254.0.0（/15）

// ---------------------------------------------------------------------------
// 对外入口（签名被 imp/mod.rs 冻结）
// ---------------------------------------------------------------------------

/// 备份当前主网络服务的 DNS 配置。
///
/// 读到的值里若含隧道哨兵（说明上一轮没干净还原），**不把它当用户原值**：
/// 先剔除哨兵，剔完为空则记为「未设置」（空 `servers` = 还原时交还 DHCP）。
pub(crate) fn backup() -> Result<DnsBackup, ErrorBody> {
    let service = discover_primary_service()?;
    let current = get_dns(&service)?;

    let (poisoned, servers): (Vec<String>, Vec<String>) =
        current.into_iter().partition(|s| is_sentinel(s));
    if !poisoned.is_empty() {
        // helper 没有 tracing 依赖（见 Cargo.toml），用 stderr 留痕。
        eprintln!(
            "xt-macosnet: 服务 {service} 当前 DNS 含隧道哨兵 {poisoned:?}（上一轮残留）；\
             不记为用户原值，剩余原值 {servers:?}（空 = 交还 DHCP）"
        );
    }

    Ok(DnsBackup { service, servers })
}

/// 把主网络服务的 DNS 设置成 `servers`；**空列表 = 清空 DNS（交还 DHCP）**。
///
/// 每个元素必须是合法 IP（`std::net::IpAddr::parse`），否则
/// [`ErrorCode::InvalidRequest`]；落在隧道网段里的地址一律拒绝（见模块级哨兵说明）。
pub(crate) fn set(servers: &[String]) -> Result<(), ErrorBody> {
    let mut ips = Vec::with_capacity(servers.len());
    for raw in servers {
        let ip: IpAddr = raw.parse().map_err(|_| {
            ErrorBody::new(ErrorCode::InvalidRequest, format!("DNS 服务器不是合法 IP：{raw}"))
                .with_detail(serde_json::json!({ "dns_server": raw }))
        })?;
        if is_sentinel_ip(ip) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidRequest,
                format!("DNS 服务器落在隧道网段 {TUN_NETWORK_PREFIX} 内，拒绝写入：{raw}"),
            )
            .with_detail(serde_json::json!({
                "dns_server": raw,
                "reason": "tunnel_sentinel",
            })));
        }
        ips.push(ip);
    }

    let service = discover_primary_service()?;
    apply_servers(&service, &ips)
}

/// 按备份还原主网络服务的 DNS。
///
/// **对备份里的值再剔一次哨兵**：磁盘上的历史快照可能是旧版写下的
/// （`servers == ["198.18.0.2"]`），直接写回等于把用户永久钉在死地址上。
/// 剔完为空 ⇒ 按「清空 DNS」还原成 DHCP。
pub(crate) fn restore(backup: &DnsBackup) -> Result<(), ErrorBody> {
    validate_service_name(&backup.service)?;

    let ips: Vec<IpAddr> = backup
        .servers
        .iter()
        .filter_map(|raw| match raw.parse::<IpAddr>() {
            Ok(ip) if !is_sentinel_ip(ip) => Some(ip),
            _ => None,
        })
        .collect();

    if ips.len() != backup.servers.len() {
        eprintln!(
            "xt-macosnet: 服务 {} 的备份含隧道哨兵或非法值（历史快照被污染）{:?}；\
             按剔除后的 {ips:?} 还原（空 = 交还 DHCP）",
            backup.service, backup.servers
        );
    }

    // 还原用**备份里记下的服务名**，不重新探测：改动加在哪个服务上，就还原到哪个服务。
    apply_servers(&backup.service, &ips)
}

// ---------------------------------------------------------------------------
// networksetup 调用
// ---------------------------------------------------------------------------

/// 找到「主网络服务」：`-listnetworkserviceorder` 输出里**第一个带 `Device:` 的服务**。
fn discover_primary_service() -> Result<String, ErrorBody> {
    let out = run(NETWORKSETUP, &args(&["-listnetworkserviceorder"]))?;
    parse_first_service_with_device(&out).ok_or_else(|| {
        ErrorBody::new(
            ErrorCode::Conflict,
            "找不到任何带设备的网络服务，无法定位要改 DNS 的服务；请先连接 Wi-Fi 或有线网络",
        )
        .with_detail(serde_json::json!({ "output": out }))
    })
}

/// 读某个服务当前的 DNS 服务器列表（无 DNS 或全为非 IP 行 → 空）。
fn get_dns(service: &str) -> Result<Vec<String>, ErrorBody> {
    validate_service_name(service)?;
    let out = run(NETWORKSETUP, &args(&["-getdnsservers", service]))?;
    Ok(parse_dns_list(&out))
}

/// 把已经解析好的 IP 写到服务上；空 = `Empty`（交还 DHCP）。成功后尽力刷新缓存。
fn apply_servers(service: &str, ips: &[IpAddr]) -> Result<(), ErrorBody> {
    validate_service_name(service)?;

    let mut argv = vec!["-setdnsservers".to_string(), service.to_string()];
    if ips.is_empty() {
        argv.push("Empty".to_string());
    } else {
        argv.extend(ips.iter().map(|ip| ip.to_string()));
    }
    run_ok(NETWORKSETUP, &argv)?;
    flush_cache();
    Ok(())
}

/// 刷新 DNS 缓存。
///
/// 真正的关键是给 `mDNSResponder` 发 `SIGHUP`，`dscacheutil -flushcache` 是历史遗留的
/// 配套动作；两者都是**尽力而为**——失败不应让已经成功的 DNS 写入变成报错。
fn flush_cache() {
    let _ = run_ok("/usr/bin/dscacheutil", &args(&["-flushcache"]));
    let _ = run_ok("/usr/bin/killall", &args(&["-HUP", "mDNSResponder"]));
}

// ---------------------------------------------------------------------------
// 纯解析（可单测，不碰系统）
// ---------------------------------------------------------------------------

/// 解析 `-listnetworkserviceorder` 的输出，返回第一个带 `Device:` 的服务名。
///
/// 输出形如（空行分隔、编号服务名与硬件行成对出现）：
///
/// ```text
/// An asterisk (*) denotes that a network service is disabled.
/// (1) Wi-Fi
/// (Hardware Port: Wi-Fi, Device: en0)
///
/// (2) Thunderbolt Bridge
/// (Hardware Port: Thunderbolt Bridge, Device: bridge0)
/// ```
///
/// ⚠️ 判断顺序不能反：`(Hardware Port: ..., Device: en0)` 也以 `(` 开头，
/// 若先做 `(N)` 匹配会把它误认成服务名。
fn parse_first_service_with_device(output: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in output.lines() {
        let line = line.trim();

        if line.starts_with("(Hardware Port:") {
            if line.contains("Device:") {
                if let Some(name) = current {
                    return Some(name);
                }
            }
            continue;
        }

        // 形如 "(1) Wi-Fi"
        if let Some(rest) = line.strip_prefix('(') {
            if let Some((idx, name)) = rest.split_once(')') {
                // 只认 `(数字)` 这种编号行，避免把硬件行/说明行当服务名。
                if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) {
                    let name = name.trim();
                    if !name.is_empty() {
                        current = Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 解析 `-getdnsservers` 的输出。
///
/// 没有设置任何 DNS 时输出 `There aren't any DNS Servers set on Wi-Fi.`；
/// 否则一行一个 IP。非 IP 行（说明文字等）一律丢弃。
fn parse_dns_list(output: &str) -> Vec<String> {
    if output.contains("aren't any") {
        return Vec::new();
    }
    output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| l.parse::<IpAddr>().is_ok())
        .map(|l| l.to_string())
        .collect()
}

/// 判断一个 IP 是不是隧道哨兵：精确等于 [`SENTINEL_DNS`]，或落在隧道网段内。
fn is_sentinel_ip(ip: IpAddr) -> bool {
    if SENTINEL_DNS.parse::<IpAddr>().map(|s| s == ip).unwrap_or(false) {
        return true;
    }
    match ip {
        IpAddr::V4(a) => (u32::from(a) & TUN_NETMASK_V4) == TUN_NETWORK_V4,
        IpAddr::V6(_) => false,
    }
}

/// 判断字符串形式的地址是否是隧道哨兵。非 IP（含主机名）一律不算哨兵。
fn is_sentinel(server: &str) -> bool {
    match server.parse::<IpAddr>() {
        Ok(ip) => is_sentinel_ip(ip),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // 服务发现
    // -----------------------------------------------------------------------

    #[test]
    fn picks_first_service_with_a_device() {
        let sample = "\
An asterisk (*) denotes that a network service is disabled.
(1) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)

(2) Thunderbolt Bridge
(Hardware Port: Thunderbolt Bridge, Device: bridge0)

(3) USB 10/100/1000 LAN
(Hardware Port: USB 10/100/1000 LAN, Device: en7)
";
        assert_eq!(parse_first_service_with_device(sample).as_deref(), Some("Wi-Fi"));
    }

    #[test]
    fn skips_services_without_a_device() {
        // 有些服务（例如已拔掉的 VPN）没有 Device 行；不能选它，也不能把
        // 上一行的编号行错当成下一个服务。
        let sample = "\
(1) Stale VPN
(2) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)
";
        assert_eq!(parse_first_service_with_device(sample).as_deref(), Some("Wi-Fi"));
    }

    #[test]
    fn hardware_line_is_not_mistaken_for_a_service_name() {
        // `(Hardware Port: ..., Device: en0)` 也以 '(' 开头：若先做 (N) 匹配，
        // 服务名会被写成 "Hardware Port: ..."，于是永远匹配不到 Device。
        let sample = "\
(1) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)
";
        let picked = parse_first_service_with_device(sample).unwrap();
        assert!(!picked.contains("Hardware"), "硬件行被误认成服务名：{picked}");
        assert_eq!(picked, "Wi-Fi");
    }

    #[test]
    fn no_device_anywhere_yields_none() {
        assert!(parse_first_service_with_device("").is_none());
        assert!(parse_first_service_with_device("(1) Wi-Fi\n").is_none());
        assert!(
            parse_first_service_with_device("(1) Wi-Fi\n(Hardware Port: Wi-Fi)\n").is_none(),
            "没有 Device 行的服务不算主网络服务"
        );
    }

    #[test]
    fn service_name_keeps_spaces_and_slashes() {
        let sample = "\
(1) USB 10/100/1000 LAN
(Hardware Port: USB 10/100/1000 LAN, Device: en7)
";
        assert_eq!(
            parse_first_service_with_device(sample).as_deref(),
            Some("USB 10/100/1000 LAN")
        );
    }

    // -----------------------------------------------------------------------
    // DNS 列表解析
    // -----------------------------------------------------------------------

    #[test]
    fn parses_dns_list_and_empty_case() {
        assert_eq!(parse_dns_list("1.1.1.1\n8.8.8.8\n"), vec!["1.1.1.1", "8.8.8.8"]);
        assert!(parse_dns_list("There aren't any DNS Servers set on Wi-Fi.").is_empty());
        assert!(parse_dns_list("There aren't any DNS Servers set on USB LAN.").is_empty());
        // 混入说明文字/空行时只留 IP（避免把注释喂给 -setdnsservers）
        assert_eq!(parse_dns_list("1.1.1.1\nsome note\n\n"), vec!["1.1.1.1"]);
    }

    #[test]
    fn parses_v6_dns() {
        assert_eq!(
            parse_dns_list("2606:4700:4700::1111\n1.1.1.1\n"),
            vec!["2606:4700:4700::1111", "1.1.1.1"]
        );
    }

    // -----------------------------------------------------------------------
    // 哨兵识别（P0：残留哨兵绝不能被当成用户原值）
    // -----------------------------------------------------------------------

    #[test]
    fn recognizes_the_tunnel_sentinel() {
        assert!(is_sentinel("198.18.0.2"));
        // 隧道网段内其它地址同样算哨兵（哨兵本身可配置）
        assert!(is_sentinel("198.18.0.1"));
        assert!(is_sentinel("198.19.255.254"));
        // 真实解析器不是哨兵
        assert!(!is_sentinel("1.1.1.1"));
        assert!(!is_sentinel("8.8.8.8"));
        assert!(!is_sentinel("2606:4700:4700::1111"));
        // 非 IP（主机名/垃圾）不算哨兵
        assert!(!is_sentinel("dns.example.com"));
        assert!(!is_sentinel(""));
        // 紧邻隧道网段但不在其中的地址不算哨兵
        assert!(!is_sentinel("198.20.0.1"));
    }

    #[test]
    fn sentinel_is_inside_the_tunnel_prefix() {
        // 不变量：哨兵常量必须落在隧道网段里，否则识别逻辑有两套判据会漂移。
        let sentinel: IpAddr = SENTINEL_DNS.parse().unwrap();
        assert!(is_sentinel_ip(sentinel));
        assert!(is_sentinel_ip("198.18.0.0".parse().unwrap()));
        assert!(is_sentinel_ip("198.19.255.255".parse().unwrap()));
        assert!(!is_sentinel_ip("198.20.0.0".parse().unwrap()));
    }

    /// 备份里含哨兵（上一轮残留）时，`backup` 的剔除逻辑必须把哨兵丢掉，
    /// 只留用户自己的解析器；全被剔掉 ⇒ 空 = 交还 DHCP。
    #[test]
    fn backup_drops_only_the_sentinel_from_a_mixed_list() {
        let now = parse_dns_list("198.18.0.2\n1.1.1.1\n");
        let (poisoned, servers): (Vec<String>, Vec<String>) =
            now.into_iter().partition(|s| is_sentinel(s));
        assert_eq!(poisoned, vec!["198.18.0.2"]);
        assert_eq!(servers, vec!["1.1.1.1"], "只许剔哨兵，用户自己的 DNS 必须留下");

        let only_sentinel = parse_dns_list("198.18.0.2\n");
        let (_p, servers): (Vec<String>, Vec<String>) =
            only_sentinel.into_iter().partition(|s| is_sentinel(s));
        assert!(servers.is_empty(), "唯一值是哨兵 ⇒ 原值应记为「未设置」(DHCP)");
    }

    /// 历史快照可能已被旧版污染（`servers == ["198.18.0.2"]`）：`restore` 必须
    /// 把它剔成空（清空 DNS），绝不能把死地址写回系统。
    #[test]
    fn restore_strips_sentinel_from_a_legacy_snapshot() {
        let legacy = DnsBackup {
            service: "Wi-Fi".to_string(),
            servers: vec!["198.18.0.2".to_string()],
        };
        let ips: Vec<IpAddr> = legacy
            .servers
            .iter()
            .filter_map(|raw| match raw.parse::<IpAddr>() {
                Ok(ip) if !is_sentinel_ip(ip) => Some(ip),
                _ => None,
            })
            .collect();
        assert!(ips.is_empty(), "污染的旧备份必须按「清空」还原成 DHCP：{ips:?}");

        // 混合情形：只剔哨兵，保留用户原值。
        let mixed = DnsBackup {
            service: "Wi-Fi".to_string(),
            servers: vec!["198.18.0.2".to_string(), "1.1.1.1".to_string()],
        };
        let ips: Vec<IpAddr> = mixed
            .servers
            .iter()
            .filter_map(|raw| match raw.parse::<IpAddr>() {
                Ok(ip) if !is_sentinel_ip(ip) => Some(ip),
                _ => None,
            })
            .collect();
        assert_eq!(ips, vec!["1.1.1.1".parse::<IpAddr>().unwrap()]);
    }

    /// `set` 的入参校验：非法 IP / 隧道网段地址都必须被拒。
    #[test]
    fn set_rejects_non_ip_and_sentinel_servers() {
        let rejects_non_ip: Result<IpAddr, _> = "not-an-ip".parse::<IpAddr>();
        assert!(rejects_non_ip.is_err());
        assert!(is_sentinel_ip("198.18.0.2".parse().unwrap()));
        // 合法用户 DNS 全部通过
        for ok in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let ip: IpAddr = ok.parse().unwrap();
            assert!(!is_sentinel_ip(ip), "{ok} 不该被判为哨兵");
        }
    }

    #[test]
    fn service_name_validation_is_wired_in() {
        assert!(validate_service_name("Wi-Fi").is_ok());
        assert!(validate_service_name("Wi-Fi\n").is_err());
        assert!(validate_service_name("Wi-Fi; rm -rf /").is_ok(), "由 argv 边界兜住，不进 shell");
    }
}
