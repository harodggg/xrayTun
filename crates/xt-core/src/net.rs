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
///
/// # 选项要**按地址族**选：AF_INET6 必须用 `IPV6_BOUND_IF`
///
/// 这里踩过一个坑，是实测出来的（macOS / arm64，`en0` 的 idx 是 14，`lo0` 是 1）：
///
/// | socket 族 | 选项 | 结果 |
/// |---|---|---|
/// | AF_INET（UDP/TCP，刚建好 / bind 后 / connect 后） | `IPPROTO_IP` + `IP_BOUND_IF` | 成功 |
/// | AF_INET6（UDP/TCP，刚建好 / bind 后 / connect 后） | `IPPROTO_IP` + `IP_BOUND_IF` | **`EINVAL(22)`，任何状态下都失败** |
/// | AF_INET6（同上） | `IPPROTO_IPV6` + `IPV6_BOUND_IF(125)` | 成功 |
/// | 任意（idx 传 0） | `IPPROTO_IP` + `IP_BOUND_IF` | 成功（等于不绑） |
/// | 任意（网卡名存在但 idx 查不到） | 同上 | `ENXIO(6)` |
/// | AF_UNIX | 同上 | `EOPNOTSUPP(102)` |
///
/// 也就是说 v4 选项**不是**「谁的 socket 都能用」：喂给它一个 AF_INET6 socket，
/// 内核一律回 `EINVAL`。这条内核行为正好解释了核心自己的那条日志
/// （`failed to set interface ... : invalid argument`，见
/// `xray::config` 里 `autoOutboundsInterface` 的注释）：Go 的 `net.Dial("tcp", …)`
/// / `DialUDP` 建出来的是 dual-stack socket，走 v4 选项就必然 `EINVAL`。
///
/// 我们这边**同一个错**，只是没被触发：两个调用方
/// （`dns_probe::udp_query` 的 `0.0.0.0:0`、`xray::probe::sample_rtt_once`
/// 的 `TcpSocket::new_v4()`）建的都是 AF_INET socket，所以一直是「侥幸正确」。
/// 但 `fd` 是别人传进来的，来个 v6 socket（例如给 IPv6 上游做 DNS 探测）就会
/// 静默走进这条 `EINVAL`。既然机制已经确定，就按族选选项，而不是继续靠运气。
///
/// 认族用 `getsockname`：它对**未 bind** 的 socket 也返回地址族
/// （`sample_rtt_once` 就是在 bind/connect **之前**调这里的，这点已被测试固定）。
///
/// 非 inet 的 socket 仍落到 v4 选项上，行为与修前一致（`EOPNOTSUPP`），
/// 因为这里唯一的语义是「绑到某张网卡」，对 AF_UNIX 本来就无意义。
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
    let (level, optname) = if socket_family(fd)? == libc::AF_INET6 {
        (libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF)
    } else {
        (libc::IPPROTO_IP, libc::IP_BOUND_IF)
    };
    let idx = index as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            optname,
            &idx as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `fd` 的地址族（`AF_INET` / `AF_INET6` / …）。
///
/// 未 bind 的 socket 也会返回它被创建时的族，所以可以在 `bind`/`connect`
/// 之前调用 —— 这正是 [`bind_to_interface_fd`] 需要的时机。
fn socket_family(fd: std::os::unix::io::RawFd) -> std::io::Result<libc::c_int> {
    // SAFETY: `sockaddr_storage` 是内核写 sockaddr 的通用容器，长度如实传进去；
    // `getsockname` 只会写我们自己栈上的这块 buffer。
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    let rc =
        unsafe { libc::getsockname(fd, &mut storage as *mut _ as *mut libc::sockaddr, &mut len) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(storage.ss_family as libc::c_int)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::{AsRawFd, RawFd};

    /// 直接建一个裸 socket，用来控制地址族 —— 这正是坑的来源。
    fn raw_socket(family: libc::c_int, ty: libc::c_int) -> RawFd {
        let fd = unsafe { libc::socket(family, ty, 0) };
        assert!(
            fd >= 0,
            "建 socket 失败：{}",
            std::io::Error::last_os_error()
        );
        fd
    }

    /// fd 的 RAII 包装：测试里建的 socket 必须关，不然几百个测试跑下来会漏 fd。
    struct FdGuard(RawFd);
    impl Drop for FdGuard {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    /// 网卡名不存在时必须**报错**，不能静默成功。
    ///
    /// 静默成功的后果很隐蔽：绑定没生效，而调用方以为绕开了隧道，
    /// 于是拿到一个假的 0ms 延迟还以为是真的。
    #[test]
    fn bind_to_interface_rejects_unknown_nic() {
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

    /// **受控实验**：同一个网卡、同一份 idx，只换地址族和选项，看内核怎么回。
    ///
    /// 这是「核心那条 `EINVAL` 日志是怎么来的」的可重复证据：
    /// v4 socket + v4 选项成功；v6 socket + v4 选项 `EINVAL`；
    /// v6 socket + v6 选项又成功。唯一变量就是族与选项的搭配。
    #[test]
    fn ip_bound_if_on_a_v6_socket_is_einval_and_ipv6_bound_if_is_not() {
        let cname = std::ffi::CString::new("lo0").unwrap();
        // SAFETY: 只读这个字符串。
        let idx = unsafe { libc::if_nametoindex(cname.as_ptr()) } as libc::c_int;
        assert_ne!(idx, 0, "lo0 一定存在");
        let set = |fd: RawFd, level: libc::c_int, opt: libc::c_int| unsafe {
            libc::setsockopt(
                fd,
                level,
                opt,
                &idx as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };

        let v4 = FdGuard(raw_socket(libc::AF_INET, libc::SOCK_DGRAM));
        let v6 = FdGuard(raw_socket(libc::AF_INET6, libc::SOCK_DGRAM));

        assert_eq!(
            set(v4.0, libc::IPPROTO_IP, libc::IP_BOUND_IF),
            0,
            "v4 socket + IP_BOUND_IF 本该成功：{}",
            std::io::Error::last_os_error()
        );
        assert_eq!(
            set(v6.0, libc::IPPROTO_IP, libc::IP_BOUND_IF),
            -1,
            "v6 socket 上 v4 选项应当是失败（否则这条机制解释不成立）"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EINVAL),
            "而且要正好是 EINVAL"
        );
        assert_eq!(
            set(v6.0, libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF),
            0,
            "换成 v6 选项就该成功：{}",
            std::io::Error::last_os_error()
        );
    }

    /// 两个地址族都要绑得上，包括**未 bind**的 v6 TCP socket
    /// （`xray::probe::sample_rtt_once` 的用法：在 connect 之前调这里）。
    #[test]
    fn bind_to_interface_handles_both_address_families() {
        let v4 = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        assert!(bind_to_interface_fd(v4.as_raw_fd(), "lo0").is_ok());

        let v6 = std::net::UdpSocket::bind("[::]:0").unwrap();
        assert!(
            bind_to_interface_fd(v6.as_raw_fd(), "lo0").is_ok(),
            "AF_INET6 socket 也必须能绑（修前这里是 EINVAL）：{}",
            std::io::Error::last_os_error()
        );

        let fresh = FdGuard(raw_socket(libc::AF_INET6, libc::SOCK_STREAM));
        assert!(
            bind_to_interface_fd(fresh.0, "lo0").is_ok(),
            "未 bind 的 v6 socket 也要能绑"
        );

        // 换族不能把「网卡不存在」这类错误吞掉。
        assert!(bind_to_interface_fd(v6.as_raw_fd(), "definitely-not-a-nic").is_err());
    }

    /// 认族必须发生在 bind/connect **之前**也能用（`getsockname` 对未 bind
    /// 的 socket 也返回族）。这条是上面那个修法的前提，单独钉住。
    #[test]
    fn socket_family_is_known_before_bind_or_connect() {
        let v4 = FdGuard(raw_socket(libc::AF_INET, libc::SOCK_STREAM));
        let v6 = FdGuard(raw_socket(libc::AF_INET6, libc::SOCK_STREAM));
        assert_eq!(socket_family(v4.0).unwrap(), libc::AF_INET);
        assert_eq!(socket_family(v6.0).unwrap(), libc::AF_INET6);
    }
}
