//! 系统 DNS 配置的读取、修改与还原。
//!
//! macOS 的 DNS 是**按网络服务（network service）**配置的，不是按接口。
//! `en0` 对应的是服务名 `Wi-Fi`，`en1` 可能对应 `USB 10/100/1000 LAN`。
//! 所以要改 DNS 必须先做一次「设备 → 服务名」的映射。
//!
//! # 最容易踩的坑
//!
//! `networksetup -setdnsservers` 写进去的值**会一直留着**，即使隧道已经拆了。
//! 用户会看到「代理关了但所有网站都打不开」—— 这是这类工具最经典的
//! 差评来源。所以这里的规则是：
//!
//! **改之前一定先备份，回滚时一定还原，且备份要落盘（防进程被 kill）。**
//!
//! 还原时用 `Empty` 而不是「写回原来的 IP」：`networksetup -getdnsservers`
//! 无法区分「用户手动设过 DNS」和「系统默认（DHCP 下发）」，而 `Empty`
//! 正是「交还给 DHCP」的语义。对于原本就有手动配置的情况，我们会把
//! 原值一起存进快照并在回滚时写回。

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use xt_proto::{Cidr, DEFAULT_SENTINEL_DNS_V4, DEFAULT_TUN_NETWORK_V4};

use crate::error::{Error, Result};
use crate::macos::{args, run, run_ok};
use crate::tools::{DSCACHEUTIL, KILLALL, NETWORKSETUP};
use crate::validate::validate_service_name;

/// DNS 配置的备份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsBackup {
    pub service: String,
    /// 原来的服务器列表。空表示原本是 DHCP 下发。
    ///
    /// ⚠️ 历史快照里**可能含隧道哨兵**（旧版 `backup()` 照抄当前值）。
    /// 判断"用户原值到底是什么"必须走 [`DnsBackup::effective_servers`]。
    pub servers: Vec<String>,
    #[serde(default)]
    pub search_domains: Vec<String>,
}

/// 这个 DNS 地址是不是**我们自己的隧道哨兵**（不是用户配的解析器）。
///
/// 判据两条，任一成立即算：
///
/// 1. 精确等于协议层写死的 [`DEFAULT_SENTINEL_DNS_V4`]（`198.18.0.2`，现场那个值）；
/// 2. 落在隧道网段 [`DEFAULT_TUN_NETWORK_V4`]（`198.18.0.0/15`，RFC 2544 保留、
///    公网不可路由）内 —— 用户设置里的哨兵**可配置**，但一定落在这个网段里。
///
/// # 为什么必须认得它
///
/// `networksetup -getdnsservers` 读到的值可能是**上一次会话残留的哨兵**。
/// 旧 `backup()` 无判据照抄：把哨兵当"用户原值"存进快照 ⇒ 之后每次"还原"
/// 都是还原成一个死地址，**问题永久化**（用户终端永远解析不了域名）。
pub fn is_sentinel(server: &str) -> bool {
    let Ok(ip) = server.parse::<IpAddr>() else {
        return false;
    };
    if DEFAULT_SENTINEL_DNS_V4.parse::<IpAddr>().map(|s| s == ip).unwrap_or(false) {
        return true;
    }
    let Ok(net) = DEFAULT_TUN_NETWORK_V4.parse::<Cidr>() else {
        return false;
    };
    net.network().contains(&Cidr::host(ip))
}

/// 一组 DNS 服务器里是否含哨兵。
pub fn has_sentinel(servers: &[String]) -> bool {
    servers.iter().any(|s| is_sentinel(s))
}

impl DnsBackup {
    /// 备份里**真正属于用户**的服务器：剔除隧道哨兵。
    ///
    /// 还原与回滚复检都必须走这里 —— 否则磁盘上被旧版污染的备份会把死地址写回系统。
    pub fn effective_servers(&self) -> Vec<String> {
        self.servers.iter().filter(|s| !is_sentinel(s)).cloned().collect()
    }
}

/// 列出所有启用的网络服务名。
pub fn list_services() -> Result<Vec<String>> {
    let out = run(NETWORKSETUP, &args(&["-listallnetworkservices"]))?;
    Ok(parse_service_list(&out))
}

fn parse_service_list(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1) // 第一行是 "An asterisk (*) denotes ..." 说明
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        // 被禁用的服务以 '*' 开头，改了也不会生效。
        .filter(|l| !l.starts_with('*'))
        .map(|l| l.to_string())
        .collect()
}

/// 由设备名反查网络服务名。
///
/// 实现方式：解析 `networksetup -listnetworkserviceorder` 的输出，
/// 它的结构稳定且有 `Device:` 标签可抓。
pub fn service_for_device(device: &str) -> Result<String> {
    let out = run(NETWORKSETUP, &args(&["-listnetworkserviceorder"]))?;
    parse_service_order(&out, device).ok_or_else(|| Error::NoNetworkService { device: device.to_string() })
}

fn parse_service_order(output: &str, device: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in output.lines() {
        let line = line.trim();
        // 顺序很重要：`(Hardware Port: ..., Device: en0)` 也以 '(' 开头，
        // 如果先做 `(N)` 的匹配，它会被误认成服务名，于是永远匹配不到设备。
        // 这正是 `tests::maps_device_to_service` 当初抓到的 bug。
        if line.starts_with("(Hardware Port:") {
            if let Some(dev) = extract_device(line) {
                if dev == device {
                    return current.clone();
                }
            }
            continue;
        }
        // 形如 "(1) Wi-Fi"
        if let Some(rest) = line.strip_prefix('(') {
            if let Some((_idx, name)) = rest.split_once(')') {
                current = Some(name.trim().to_string());
            }
        }
    }
    None
}

fn extract_device(line: &str) -> Option<String> {
    let idx = line.find("Device:")?;
    // `Device:` 后面跟着一个空格，必须先 trim，否则 take_while 会立刻停在空格上
    // 返回空串 —— 这是 `tests::maps_device_to_service` 抓到的第二个 bug。
    let rest = line[idx + "Device:".len()..].trim_start();
    let dev: String = rest
        .chars()
        .take_while(|c| !matches!(c, ')' | ',' | ' '))
        .collect();
    if dev.is_empty() {
        None
    } else {
        Some(dev)
    }
}

/// 读取当前 DNS 服务器列表。
pub fn get_dns(service: &str) -> Result<Vec<String>> {
    validate_service_name(service)?;
    let out = run(NETWORKSETUP, &args(&["-getdnsservers", service]))?;
    Ok(parse_dns_list(&out))
}

fn parse_dns_list(output: &str) -> Vec<String> {
    // 没有任何 DNS 时输出 "There aren't any DNS Servers set on Wi-Fi."
    if output.contains("aren't any") {
        return Vec::new();
    }
    output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && l.parse::<IpAddr>().is_ok())
        .map(|l| l.to_string())
        .collect()
}

/// 读取当前搜索域。
pub fn get_search_domains(service: &str) -> Result<Vec<String>> {
    validate_service_name(service)?;
    let out = run(NETWORKSETUP, &args(&["-getsearchdomains", service]))?;
    if out.contains("aren't any") {
        return Ok(Vec::new());
    }
    Ok(out.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).map(|l| l.to_string()).collect())
}

/// 备份某个服务的 DNS 配置。
///
/// **永远不许把隧道哨兵当用户原值**：若此刻读到的值里含哨兵（上一轮残留），
/// 就把它们剔掉；全被剔掉 ⇒ 原值记为**未设置**（空 = 交还 DHCP）。
/// 这样即使系统已处于残留状态，下一次"还原"也会还原成 DHCP 而不是死地址。
pub fn backup(service: &str) -> Result<DnsBackup> {
    let now = get_dns(service)?;
    let poisoned: Vec<String> = now.iter().filter(|s| is_sentinel(s)).cloned().collect();
    let servers: Vec<String> = now.iter().filter(|s| !is_sentinel(s)).cloned().collect();
    if !poisoned.is_empty() {
        tracing::warn!(
            service = %service,
            found = ?poisoned,
            kept = ?servers,
            "当前 DNS 里含隧道哨兵（上一轮残留）：不把它当用户原值；原值按「未设置」(DHCP) 记录"
        );
    }
    Ok(DnsBackup {
        service: service.to_string(),
        servers,
        search_domains: get_search_domains(service).unwrap_or_default(),
    })
}

/// 写入 DNS 服务器。
pub fn set_dns(service: &str, servers: &[IpAddr]) -> Result<()> {
    validate_service_name(service)?;
    if servers.is_empty() {
        return clear_dns(service);
    }
    let mut argv = vec!["-setdnsservers".to_string(), service.to_string()];
    argv.extend(servers.iter().map(|s| s.to_string()));
    run_ok(NETWORKSETUP, &argv)?;
    flush_cache();
    Ok(())
}

/// 清空 DNS 设置，交还给 DHCP。
pub fn clear_dns(service: &str) -> Result<()> {
    validate_service_name(service)?;
    run_ok(NETWORKSETUP, &args(&["-setdnsservers", service, "Empty"]))?;
    flush_cache();
    Ok(())
}

/// 还原备份。`servers` 为空时清空（等价于还原成 DHCP）。
///
/// **会剔除哨兵**：磁盘上的历史快照可能已被旧版污染（`servers = ["198.18.0.2"]`），
/// 直接写回等于把用户永久钉在死地址上。
pub fn restore(backup: &DnsBackup) -> Result<()> {
    let servers = backup.effective_servers();
    if servers.len() != backup.servers.len() {
        tracing::warn!(
            service = %backup.service,
            recorded = ?backup.servers,
            "备份里含隧道哨兵（历史快照被污染）：按「未设置」还原成 DHCP，不把死地址写回系统"
        );
    }
    restore_servers(&backup.service, &servers)
}

/// 把某个服务的 DNS 恢复成给定列表；空列表 = 交还 DHCP。
///
/// 列表里若混入哨兵会被就地剔除（防止调用方把哨兵当"原值"传进来）。
pub fn restore_servers(service: &str, servers: &[String]) -> Result<()> {
    let ips: Vec<IpAddr> = servers
        .iter()
        .filter(|s| !is_sentinel(s))
        .filter_map(|s| s.parse().ok())
        .collect();
    set_dns(service, &ips)
}

/// 刷新 DNS 缓存。
///
/// 真正的关键是给 `mDNSResponder` 发 `SIGHUP`；`dscacheutil -flushcache`
/// 是历史遗留的配套动作，一起做没有坏处。两者都是**尽力而为**，
/// 失败不应影响主流程。
pub fn flush_cache() {
    let _ = run_ok(DSCACHEUTIL, &args(&["-flushcache"]));
    let _ = run_ok(KILLALL, &args(&["-HUP", "mDNSResponder"]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_disabled_services() {
        let sample = "\
An asterisk (*) denotes that a network service is disabled.
Wi-Fi
Thunderbolt Bridge
*iPhone USB
USB 10/100/1000 LAN
";
        let services = parse_service_list(sample);
        assert_eq!(services, vec!["Wi-Fi", "Thunderbolt Bridge", "USB 10/100/1000 LAN"]);
    }

    #[test]
    fn maps_device_to_service() {
        let sample = "\
An asterisk (*) denotes that a network service is disabled.
(1) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)

(2) Thunderbolt Bridge
(Hardware Port: Thunderbolt Bridge, Device: bridge0)

(3) USB 10/100/1000 LAN
(Hardware Port: USB 10/100/1000 LAN, Device: en7)
";
        assert_eq!(parse_service_order(sample, "en0").unwrap(), "Wi-Fi");
        assert_eq!(parse_service_order(sample, "bridge0").unwrap(), "Thunderbolt Bridge");
        assert_eq!(parse_service_order(sample, "en7").unwrap(), "USB 10/100/1000 LAN");
        assert!(parse_service_order(sample, "utun4").is_none());
    }

    #[test]
    fn parses_dns_list_and_empty_case() {
        assert_eq!(parse_dns_list("1.1.1.1\n8.8.8.8\n"), vec!["1.1.1.1", "8.8.8.8"]);
        assert!(parse_dns_list("There aren't any DNS Servers set on Wi-Fi.").is_empty());
        // 非 IP 的行（例如说明文字）要被丢掉
        assert_eq!(parse_dns_list("1.1.1.1\nsome note\n"), vec!["1.1.1.1"]);
    }

    #[test]
    fn rejects_service_name_injection() {
        assert!(get_dns("Wi-Fi; rm -rf /").is_err());
        assert!(set_dns("-flag", &["1.1.1.1".parse().unwrap()]).is_err());
    }

    #[test]
    fn empty_server_list_means_clear() {
        // clear_dns 会真的去调 networksetup；这里只验证参数校验路径，
        // 实际调用在集成环境里跑。
        assert!(validate_service_name("Wi-Fi").is_ok());
    }

    // -----------------------------------------------------------------------
    // task-??? P0：哨兵识别
    //
    // 现场：App 不在时系统 DNS 残留成哨兵 `198.18.0.2`，整机解析不了域名。
    // 旧 `backup()` 是**无判据照抄当前值**：此刻读到的哨兵会被当成"用户原值"
    // 存进快照 ⇒ 之后每次"还原"都是还原成死地址（问题永久化）。
    // -----------------------------------------------------------------------

    /// 记录所有 `networksetup -setdnsservers` 的 argv。
    fn recording_exec(
        get_dns: &'static str,
    ) -> (
        crate::macos::TestExecutor,
        std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let exec: crate::macos::TestExecutor = Rc::new(move |program: &str, args: &[String]| {
            if program == crate::tools::NETWORKSETUP {
                if args.first().map(String::as_str) == Some("-getdnsservers") {
                    return Ok(get_dns.to_string());
                }
                if args.first().map(String::as_str) == Some("-setdnsservers") {
                    sink.borrow_mut().push(args.to_vec());
                }
                if args.first().map(String::as_str) == Some("-getsearchdomains") {
                    return Ok("There aren't any search domains set on Wi-Fi.\n".to_string());
                }
            }
            Ok(String::new())
        });
        (exec, seen)
    }

    /// **判据 ②（改前红）**：当前值就是哨兵时，`backup()` 不许把它当用户原值。
    ///
    /// 改前：`backup().servers == ["198.18.0.2"]`（照抄） ⇒ 这里红。
    /// 改后：剔掉哨兵 ⇒ 原值记为 **未设置**（空 = 交还 DHCP）。
    #[test]
    fn backup_never_records_the_sentinel_as_the_users_original_dns() {
        use crate::macos::with_executor;

        let sentinel = xt_proto::DEFAULT_SENTINEL_DNS_V4;
        let (exec, _seen) = recording_exec("198.18.0.2\n");
        let b = with_executor(exec, || backup("Wi-Fi")).expect("读 DNS");
        assert!(
            !b.servers.iter().any(|s| s == sentinel),
            "哨兵绝不能被当成用户原值备份（否则每次还原都还原成死地址）: {:?}",
            b.servers
        );
        assert!(
            b.servers.is_empty(),
            "读到的唯一值就是哨兵 ⇒ 原值应记为「未设置」(DHCP): {:?}",
            b.servers
        );
    }

    /// **判据 ②的补强（改前红）**：混合列表里只剔哨兵，用户自己的服务器要留下。
    #[test]
    fn backup_drops_only_the_sentinel_from_a_mixed_list() {
        use crate::macos::with_executor;

        let (exec, _seen) = recording_exec("198.18.0.2\n1.1.1.1\n");
        let b = with_executor(exec, || backup("Wi-Fi")).expect("读 DNS");
        assert_eq!(b.servers, vec!["1.1.1.1"], "只许剔哨兵，用户自己的 DNS 必须留下");
    }

    /// **历史快照兜底（改前红）**：磁盘上可能已有被污染的备份
    /// （`servers = ["198.18.0.2"]`，旧版写下的）。还原时也必须清成 DHCP，
    /// 不能把死地址重新写回系统。
    #[test]
    fn restore_of_a_legacy_sentinel_backup_clears_to_dhcp() {
        use crate::macos::with_executor;

        let sentinel = xt_proto::DEFAULT_SENTINEL_DNS_V4;
        let (exec, seen) = recording_exec("There aren't any DNS Servers set on Wi-Fi.\n");
        let legacy = DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec![sentinel.to_string()],
            search_domains: vec![],
        };
        with_executor(exec, || restore(&legacy)).expect("还原");
        let calls = seen.borrow().clone();
        assert!(
            calls.iter().any(|a| a == &vec!["-setdnsservers".to_string(), "Wi-Fi".into(), "Empty".into()]),
            "被哨兵污染的旧备份必须按「未设置」还原成 DHCP，而不是把哨兵写回去: {calls:?}"
        );
    }
}
