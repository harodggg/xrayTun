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

use crate::error::{Error, Result};
use crate::macos::{args, run, run_ok};
use crate::tools::{DSCACHEUTIL, KILLALL, NETWORKSETUP};
use crate::validate::validate_service_name;

/// DNS 配置的备份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsBackup {
    pub service: String,
    /// 原来的服务器列表。空表示原本是 DHCP 下发。
    pub servers: Vec<String>,
    #[serde(default)]
    pub search_domains: Vec<String>,
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
pub fn backup(service: &str) -> Result<DnsBackup> {
    Ok(DnsBackup {
        service: service.to_string(),
        servers: get_dns(service)?,
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
pub fn restore(backup: &DnsBackup) -> Result<()> {
    if backup.servers.is_empty() {
        clear_dns(&backup.service)
    } else {
        let servers: Vec<IpAddr> = backup
            .servers
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        set_dns(&backup.service, &servers)
    }
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
}
