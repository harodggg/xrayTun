//! 极小的网络辅助函数。
//!
//! 放这里而不是 `xt-tun`：这些逻辑是**跨平台**的（DNS 解析、TCP 可达性探测），
//! 和「哪个系统调用能建虚拟网卡」无关。

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

/// 把主机名或 IP 解析成 IP 列表。
///
/// **这是防路由环的第一步**：TUN 模式必须在接管默认路由之前，知道
/// 「代理服务器本身的地址是什么」，才能给它单独留一条走物理出口的 host 路由。
/// 否则服务器的流量也会被 `0.0.0.0/1` 送进隧道，形成路由环。
///
/// 传进来已经是 IP 时直接返回，不做多余的系统解析。
pub fn resolve_host(host: &str) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }

    match (host, 0u16).to_socket_addrs() {
        Ok(addrs) => {
            let mut ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            // 去重并保持稳定顺序：IPv4 优先。
            //
            // 为什么 IPv4 优先：隧道的 `gateway` 在 macOS 上只取第一个 IPv4 前缀
            // （Xray 的行为），IPv6 侧往往是链路本地地址，给它加 host 路由
            // 未必有效。把 IPv4 排在前面能让 host 路由更可能真的生效。
            ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
            ips.dedup();
            ips
        }
        Err(e) => {
            tracing::warn!(host, error = %e, "解析代理服务器地址失败");
            Vec::new()
        }
    }
}

/// 在给定时限内尝试一次 TCP 连接。
///
/// 用于回答一个非常具体的问题：**「在当前的网络配置下，我还能不能连上代理服务器？」**
///
/// 这个问题必须在两个时刻各问一次：
///
/// * 接管默认路由**之前** —— 至少证明服务器本身是可达的；
/// * 接管默认路由**之后** —— 证明我们的 bypass 路由真的生效了。
///
/// 第二次检查是关键：如果它失败，说明流量已经被送进隧道而服务器又连不上，
/// 此时正确的做法是**立刻回滚并报错**，而不是让用户面对一个
/// 「显示已连接、但什么都打不开」的隧道。
pub fn tcp_reachable(addr: SocketAddr, timeout: Duration) -> bool {
    use std::net::TcpStream;
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_literals_pass_through_without_resolution() {
        let ips = resolve_host("203.0.113.10");
        assert_eq!(ips, vec!["203.0.113.10".parse::<IpAddr>().unwrap()]);

        let v6 = resolve_host("2001:db8::1");
        assert_eq!(v6, vec!["2001:db8::1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn unresolvable_host_yields_empty_not_panic() {
        // `.invalid` 是 RFC 2606 保留的、保证不会解析成功的顶级域。
        let ips = resolve_host("definitely-not-a-real-host.invalid");
        assert!(ips.is_empty());
    }

    #[test]
    fn localhost_resolves_and_prefers_ipv4() {
        let ips = resolve_host("localhost");
        assert!(!ips.is_empty(), "localhost 必须能解析");
        if ips.len() > 1 {
            assert!(!ips[0].is_ipv6(), "IPv4 必须排在前面，否则 host 路由可能无效");
        }
    }

    #[test]
    fn tcp_reachable_is_false_for_closed_port() {
        // 端口 1 上不会有服务。
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(!tcp_reachable(addr, Duration::from_millis(200)));
    }

    #[test]
    fn tcp_reachable_is_true_for_a_listening_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(tcp_reachable(addr, Duration::from_millis(500)));
    }
}
