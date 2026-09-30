//! 上层字符串 → 内部类型 的解析与校验。
//!
//! 所有来自上层（daemon 经 IPC 转达）的字符串都必须先过这里：接口名、CIDR、
//! 服务名。这一层是 helper 攻击面的第一道闸——任何带空格/元字符的字符串
//! 都不许进入 argv（尽管 argv 数组本就不经过 shell，多一层校验更稳）。

use xt_contract::error::{bad_request, ErrorBody};

use crate::model::Cidr;

/// 接口名白名单：`[a-zA-Z0-9]+`，且不含任何路径分隔符/空格/元字符。
/// utun 名（`utun4`）、物理网卡名（`en0`）都满足；`-rf`、`utun0;rm` 不满足。
pub fn validate_interface_name(name: &str) -> Result<(), ErrorBody> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric());
    if !ok {
        return Err(bad_request(format!("非法接口名：{name}")));
    }
    Ok(())
}

/// 网络服务名：允许字母数字与常见分隔符（`Wi-Fi`、`USB 10/100/1000 LAN`），
/// 但禁止换行/制表（它们会污染 `networksetup` 的参数边界）。
pub fn validate_service_name(name: &str) -> Result<(), ErrorBody> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii() && !c.is_ascii_control());
    if !ok {
        return Err(bad_request(format!("非法服务名：{name}")));
    }
    Ok(())
}

/// 把一组 CIDR 字符串解析成 `Vec<Cidr>`，任何一条非法即整体失败。
pub fn parse_cidrs(items: &[String]) -> Result<Vec<Cidr>, ErrorBody> {
    items.iter().map(|s| Cidr::parse(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_parses_v4_and_v6() {
        assert_eq!(Cidr::parse("198.18.0.1/15").unwrap().prefix, 15);
        assert_eq!(Cidr::parse("0.0.0.0/1").unwrap().prefix, 1);
        assert_eq!(Cidr::parse("::/1").unwrap().prefix, 1);
        assert_eq!(Cidr::parse("fe80::/10").unwrap().prefix, 10);
    }

    #[test]
    fn cidr_rejects_bad_input() {
        assert!(Cidr::parse("198.18.0.1").is_err());
        assert!(Cidr::parse("198.18.0.1/33").is_err());
        assert!(Cidr::parse("::/129").is_err());
        assert!(Cidr::parse("not-an-ip/24").is_err());
        assert!(Cidr::parse("198.18.0.1/x").is_err());
    }

    #[test]
    fn network_normalizes_host_bits() {
        let c = Cidr::parse("198.18.99.7/15").unwrap();
        assert_eq!(c.network().to_string(), "198.18.0.0");
        let host = Cidr::parse("34.135.247.136/32").unwrap();
        assert_eq!(host.network().to_string(), "34.135.247.136");
    }

    #[test]
    fn netmask_v4_matches_prefix() {
        assert_eq!(
            Cidr::parse("198.18.0.1/15").unwrap().netmask_v4().unwrap().to_string(),
            "255.254.0.0"
        );
        assert_eq!(
            Cidr::parse("198.18.0.1/24").unwrap().netmask_v4().unwrap().to_string(),
            "255.255.255.0"
        );
    }

    #[test]
    fn interface_name_whitelist() {
        assert!(validate_interface_name("utun4").is_ok());
        assert!(validate_interface_name("en0").is_ok());
        assert!(validate_interface_name("lo0").is_ok());
        assert!(validate_interface_name("").is_err());
        assert!(validate_interface_name("-rf").is_err());
        assert!(validate_interface_name("utun0; rm").is_err());
        assert!(validate_interface_name("utun0/../../x").is_err());
    }

    #[test]
    fn service_name_allows_wifi_but_not_control_chars() {
        assert!(validate_service_name("Wi-Fi").is_ok());
        assert!(validate_service_name("USB 10/100/1000 LAN").is_ok());
        assert!(validate_service_name("Wi-Fi\n").is_err());
        assert!(validate_service_name("").is_err());
    }

    #[test]
    fn parse_cidrs_parses_a_batch_and_rejects_any_bad_entry() {
        let items = vec!["198.18.0.1/15".to_string(), "0.0.0.0/1".to_string()];
        let parsed = parse_cidrs(&items).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].prefix, 15);

        let bad = vec!["198.18.0.1/15".to_string(), "not-an-ip/24".to_string()];
        assert!(parse_cidrs(&bad).is_err());
    }
}
