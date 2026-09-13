//! 网络接口配置与统计。

use std::net::Ipv4Addr;

use xt_proto::Cidr;

use crate::error::{Error, Result};
use crate::macos::{args, run};
use crate::tools::{IFCONFIG, NETSTAT};
use crate::validate::validate_interface_name;

/// 接口收发字节计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InterfaceCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

/// 给 utun 配置地址。
///
/// utun 是**点对点**接口，`ifconfig` 需要「本地地址 + 对端地址」两个参数。
/// 实践中把两者设成同一个地址（配合真实 netmask）是最省事且被广泛采用的写法：
/// 它让路由表里的 `-interface utunN` 规则能正常工作，同时避免多出一个
/// 需要维护的假对端地址。
pub fn configure_address(interface: &str, cidr: &Cidr, mtu: u16) -> Result<()> {
    validate_interface_name(interface)?;
    let mtu = mtu.to_string();

    match cidr.addr {
        std::net::IpAddr::V4(v4) => {
            let mask = netmask_from_prefix(cidr.prefix)?;
            run(
                IFCONFIG,
                &args(&[
                    interface,
                    "inet",
                    &v4.to_string(),
                    &v4.to_string(),
                    "netmask",
                    &mask.to_string(),
                    "mtu",
                    &mtu,
                    "up",
                ]),
            )?;
        }
        std::net::IpAddr::V6(v6) => {
            run(
                IFCONFIG,
                &args(&[
                    interface,
                    "inet6",
                    &v6.to_string(),
                    "prefixlen",
                    &cidr.prefix.to_string(),
                    "mtu",
                    &mtu,
                    "up",
                ]),
            )?;
        }
    }
    Ok(())
}

/// IPv4 前缀长度 → 点分十进制掩码。
pub fn netmask_from_prefix(prefix: u8) -> Result<Ipv4Addr> {
    if prefix > 32 {
        return Err(Error::Invalid(format!("IPv4 前缀长度非法: {prefix}")));
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    Ok(Ipv4Addr::from(mask))
}

/// 关掉接口（回滚时用；通常不需要，因为接口会随 fd 关闭而消失）。
pub fn deconfigure(interface: &str) -> Result<()> {
    validate_interface_name(interface)?;
    run(IFCONFIG, &args(&[interface, "down"])).map(|_| ())
}

/// 列出所有接口名。
pub fn list_interfaces() -> Result<Vec<String>> {
    let out = run(IFCONFIG, &args(&["-l"]))?;
    Ok(out.split_whitespace().map(|s| s.to_string()).collect())
}

/// 读取接口收发计数。
///
/// 解析策略：**从行尾往前数**。`netstat -ibn` 的尾部 7 列固定是
/// `Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll`，而前面的 `Network` / `Address`
/// 列在某些接口上会缺失（导致列数变化）。从后往前数对列数变化免疫。
pub fn interface_counters(interface: &str) -> Result<InterfaceCounters> {
    validate_interface_name(interface)?;
    let out = run(NETSTAT, &args(&["-ibn"]))?;
    parse_counters(&out, interface).ok_or_else(|| Error::Parse {
        what: "netstat -ibn 中找不到该接口的统计行",
        raw: interface.to_string(),
    })
}

fn parse_counters(output: &str, interface: &str) -> Option<InterfaceCounters> {
    // 同一接口会有多行（每个地址族一行），取 `<Link#N>` 那一行，它才是链路层真值。
    for line in output.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 || f[0] != interface {
            continue;
        }
        if !f[2].starts_with("<Link") {
            continue;
        }
        let n = f.len();
        let parse = |i: usize| f[i].parse::<u64>().ok();
        return Some(InterfaceCounters {
            rx_packets: parse(n - 7)?,
            rx_bytes: parse(n - 5)?,
            tx_packets: parse(n - 4)?,
            tx_bytes: parse(n - 2)?,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netmask_conversion() {
        assert_eq!(netmask_from_prefix(0).unwrap(), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(netmask_from_prefix(8).unwrap(), Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(netmask_from_prefix(15).unwrap(), Ipv4Addr::new(255, 254, 0, 0));
        assert_eq!(netmask_from_prefix(24).unwrap(), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(netmask_from_prefix(32).unwrap(), Ipv4Addr::new(255, 255, 255, 255));
        assert!(netmask_from_prefix(33).is_err());
    }

    #[test]
    fn parses_link_line_counters() {
        let sample = "\
Name       Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll
lo0        16384 <Link#1>                         100     0     123456      200     0     654321     0
lo0        16384 127           127.0.0.1            100     -     123456      200     -     654321     -
en0         1500 <Link#4>    aa:bb:cc:dd:ee:ff     900     0   99999999      800     0   88888888     0
";
        let c = parse_counters(sample, "en0").unwrap();
        assert_eq!(c.rx_bytes, 99_999_999);
        assert_eq!(c.tx_bytes, 88_888_888);
        assert_eq!(c.rx_packets, 900);
        assert_eq!(c.tx_packets, 800);
    }

    #[test]
    fn tolerates_missing_address_column() {
        // 没有 Address 列时字段会左移，从尾部解析仍然正确。
        let sample = "\
Name  Mtu  Network   Ipkts Ierrs  Ibytes Opkts Oerrs  Obytes Coll
utun4 1500 <Link#12>    10     0    1024    20     0    2048    0
";
        let c = parse_counters(sample, "utun4").unwrap();
        assert_eq!(c.rx_bytes, 1024);
        assert_eq!(c.tx_bytes, 2048);
    }

    #[test]
    fn unknown_interface_returns_none() {
        assert!(parse_counters("Name Mtu\n", "en0").is_none());
    }

    #[test]
    fn rejects_bad_interface_name() {
        assert!(interface_counters("-rf").is_err());
    }
}
