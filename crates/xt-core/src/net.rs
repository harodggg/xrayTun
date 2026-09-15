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

/// 把 socket 绑到指定物理网卡（`IP_BOUND_IF`），**绕开隧道与核心**。
///
/// 为什么到处都需要它：TUN 模式接管默认路由之后，任何一个新 socket 的
/// `connect()` 都会被本地协议栈**在 TUN 那一侧立刻应答**，于是：
///
/// * 「直连握手 RTT」会测出 **0ms**（远端服务器不可能 0ms）；
/// * 并发探测测到的是核心的排队，而不是对端的延迟。
///
/// 绑到物理网卡之后，路由查找被限定在这张网卡上，包走真实链路 ——
/// 这才是「我离那台服务器多远」的真实数字。DNS 探测（`dns_probe`）和
/// 节点 RTT（`xray::probe`）都靠它。
///
/// `interface` 传了但不存在时返回 `Err`：调用方应当知道这次测的是绕了隧道的
/// 数字，而不是静默拿到一个假的 0ms。
pub fn bind_to_interface_fd(fd: std::os::unix::io::RawFd, interface: &str) -> std::io::Result<()> {
    let cname = std::ffi::CString::new(interface)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "网卡名含 NUL"))?;
    // SAFETY: `if_nametoindex` 只读这个字符串；`setsockopt` 的实参类型与
    // `socklen_t` 长度和 `c_int` 完全对应。
    let index = unsafe { libc::if_nametoindex(cname.as_ptr()) };
    if index == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("找不到网卡 {interface}"),
        ));
    }
    let idx = index as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_BOUND_IF,
            &idx as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网卡名不存在时必须**报错**，不能静默成功。
    ///
    /// 静默成功的后果很隐蔽：绑定没生效，而调用方以为绕开了隧道，
    /// 于是拿到一个假的 0ms 延迟还以为是真的。
    #[test]
    fn bind_to_interface_rejects_unknown_nic() {
        use std::os::unix::io::AsRawFd;
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(
            bind_to_interface_fd(sock.as_raw_fd(), "definitely-not-a-nic").is_err(),
            "不存在的网卡必须报错",
        );
        assert!(
            bind_to_interface_fd(sock.as_raw_fd(), "lo0").is_ok(),
            "lo0 一定存在，应当成功",
        );
    }
}
