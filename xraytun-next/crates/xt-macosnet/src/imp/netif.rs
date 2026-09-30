//! 接口地址配置：把 [`Cidr`] 翻译成 `ifconfig(8)` 调用。
//!
//! 本模块只负责「给一张已经建好的接口配地址」。接口的创建/销毁属于
//! [`super::utun`]，路由与 DNS 分别是 [`super::route`] / [`super::dns`]。
//!
//! # 为什么 utun 要写两次地址
//!
//! utun 是**点对点**接口，`ifconfig` 的 `inet` 形式需要「本地地址 + 对端地址」
//! 两个参数。实践中把两者设成同一个地址（配合真实 netmask）是最省事且被广泛
//! 采用的写法：
//!
//! * 让路由表里的 `-interface utunN` 规则能正常工作；
//! * 避免多出一个需要维护、又不会真的被使用的假对端地址。
//!
//! IPv6 侧没有对端地址的概念，直接给 `inet6 <addr> prefixlen <n>` 即可。
//!
//! # 安全边界
//!
//! 所有命令都是**绝对路径 + argv 数组**（见 [`crate::imp::run`]），永不经过
//! shell；接口名在拼参数前先过 [`validate_interface_name`] 白名单。因此来自
//! GUI 的字符串不可能被解释成 shell 语法或 `ifconfig` 选项。

use xt_contract::error::ErrorBody;

use crate::imp::tools::IFCONFIG;
use crate::imp::{args, run_ok};
use crate::model::Cidr;
use crate::validate::validate_interface_name;

/// 给 utun 配置一个地址并拉起接口。
///
/// * IPv4：`ifconfig <if> inet <addr> <addr> netmask <mask> mtu <mtu> up`
/// * IPv6：`ifconfig <if> inet6 <addr> prefixlen <prefix> mtu <mtu> up`
///
/// netmask 由前缀换算（`prefix == 0 → 0.0.0.0`，否则 `u32::MAX << (32-prefix)`），
/// 复用 [`Cidr::netmask_v4`]，因此 IPv6 地址传给本函数不会走到掩码分支。
///
/// 失败时 `run_ok` 已经把 `ifconfig` 的退出码与 stderr 塞进 `ErrorBody.detail`，
/// 调用方（`imp::tun_up`）据此回滚整张卡。
pub(crate) fn configure_address(interface: &str, address: &Cidr, mtu: u16) -> Result<(), ErrorBody> {
    validate_interface_name(interface)?;
    let argv = ifconfig_args(interface, address, mtu)?;
    run_ok(IFCONFIG, &argv)
}

/// 纯函数：按地址族拼出 `ifconfig` 的 argv。
///
/// 与执行分离是为了让参数形状可单测——单测里不能真的去改系统接口。
fn ifconfig_args(interface: &str, address: &Cidr, mtu: u16) -> Result<Vec<String>, ErrorBody> {
    let mtu = mtu.to_string();
    Ok(match address.addr {
        std::net::IpAddr::V4(v4) => {
            let mask = address.netmask_v4()?.to_string();
            let ip = v4.to_string();
            // 点对点：本地地址与对端地址取同一个值。
            args(&[
                interface,
                "inet",
                &ip,
                &ip,
                "netmask",
                &mask,
                "mtu",
                &mtu,
                "up",
            ])
        }
        std::net::IpAddr::V6(v6) => {
            let ip = v6.to_string();
            let prefix = address.prefix.to_string();
            args(&[
                interface,
                "inet6",
                &ip,
                "prefixlen",
                &prefix,
                "mtu",
                &mtu,
                "up",
            ])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(text: &str) -> Cidr {
        Cidr::parse(text).expect("测试用例里的 CIDR 必须合法")
    }

    /// IPv4 的关键形状：地址出现两次（本端 + 对端），并且带真实 netmask。
    #[test]
    fn ipv4_duplicates_address_and_carries_netmask() {
        let argv = ifconfig_args("utun4", &cidr("198.18.0.1/15"), 1420).unwrap();
        assert_eq!(
            argv,
            args(&[
                "utun4",
                "inet",
                "198.18.0.1",
                "198.18.0.1",
                "netmask",
                "255.254.0.0",
                "mtu",
                "1420",
                "up",
            ])
        );
    }

    /// `/0` 不能左移 32 位（会 panic），必须退化成全零掩码。
    #[test]
    fn ipv4_prefix_zero_yields_zero_mask() {
        let argv = ifconfig_args("utun4", &cidr("0.0.0.0/0"), 1500).unwrap();
        assert_eq!(argv[4], "netmask");
        assert_eq!(argv[5], "0.0.0.0");
    }

    /// IPv6 用 `prefixlen`，且不得出现 `netmask` / 对端地址。
    #[test]
    fn ipv6_uses_prefixlen() {
        let argv = ifconfig_args("utun7", &cidr("fd00::1/64"), 1420).unwrap();
        assert_eq!(
            argv,
            args(&[
                "utun7",
                "inet6",
                "fd00::1",
                "prefixlen",
                "64",
                "mtu",
                "1420",
                "up",
            ])
        );
        assert!(!argv.iter().any(|a| a == "netmask"));
        assert_eq!(argv.iter().filter(|a| *a == "fd00::1").count(), 1);
    }

    /// 非法接口名必须在**调用 `ifconfig` 之前**被拒——这条不会碰系统。
    #[test]
    fn rejects_bad_interface_name_before_running() {
        assert!(configure_address("-rf", &cidr("198.18.0.1/15"), 1420).is_err());
        assert!(configure_address("utun0; rm -rf /", &cidr("198.18.0.1/15"), 1420).is_err());
        assert!(configure_address("", &cidr("198.18.0.1/15"), 1420).is_err());
    }
}
