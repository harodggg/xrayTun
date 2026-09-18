//! IP → 地理位置（给地球仪用）。
//!
//! # 数据来自外部，这一点必须在界面上说清
//!
//! 项目自带的 `geoip.dat` **只有国别与网段，没有经纬度**（实测确认：整个文件里
//! 没有任何 8 字节 double 字段）。要在地球上把位置点画出来，就必须另找坐标来源。
//!
//! 当前用的是 `ip-api.com`（实测可用，精度到城市）。代价是**被查的 IP 会发给
//! 第三方** —— 对自建节点来说这等于把「你在用哪台服务器」告诉对方。所以：
//!
//! * 结果按 IP 缓存，同一个 IP 只查一次；
//! * 界面上明确标注「位置来自 ip-api.com」，不假装是本地算出来的。
//!
//! # 必须绕过隧道
//!
//! 隧道开着时直接发出请求会**从节点出去**，那样查到的会是「节点自己的位置」
//! 而不是「被查 IP 的位置」—— 结果会静默地错，而且错得很像对的。
//! 所以这里把出口 socket 绑到物理网卡（与 `xray/probe.rs` 的 RTT 探测同一手法）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 查到的位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    pub ip: String,
    pub country: String,
    pub city: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub isp: String,
    /// 数据来源，界面据此如实标注。
    pub source: String,
}

/// ip-api.com 的响应（字段少，只取我们要的）。
#[derive(Debug, Deserialize)]
struct IpApiResponse {
    status: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    lat: f64,
    #[serde(default)]
    lon: f64,
    #[serde(default)]
    isp: String,
    #[serde(default)]
    message: String,
}

const HOST: &str = "ip-api.com";
const TIMEOUT: Duration = Duration::from_secs(6);

/// 用物理网卡直连查一次。
///
/// `interface` 为 `None` 时按系统默认路由出去（不推荐：隧道开着会查错）。
pub fn lookup(ip: &str, interface: Option<&str>) -> Result<GeoLocation, String> {
    let path = format!("/json/{ip}?fields=status,message,country,city,lat,lon,isp");
    let body = http_get(HOST, 80, &path, interface)?;
    let parsed: IpApiResponse =
        serde_json::from_str(&body).map_err(|e| format!("解析位置响应失败: {e}"))?;
    if parsed.status != "success" {
        return Err(if parsed.message.is_empty() {
            "位置查询失败".into()
        } else {
            parsed.message
        });
    }
    Ok(GeoLocation {
        ip: ip.to_string(),
        country: parsed.country,
        city: parsed.city,
        lat: parsed.lat,
        lon: parsed.lon,
        isp: parsed.isp,
        source: "ip-api.com".into(),
    })
}

/// 查**本机**的公网出口位置（绑物理网卡，避免查到节点的位置）。
pub fn lookup_self(interface: Option<&str>) -> Result<GeoLocation, String> {
    let body = http_get(HOST, 80, "/json/?fields=status,message,country,city,lat,lon,isp,query", interface)?;
    #[derive(Deserialize)]
    struct WithQuery {
        status: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        query: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        lat: f64,
        #[serde(default)]
        lon: f64,
        #[serde(default)]
        isp: String,
    }
    let p: WithQuery = serde_json::from_str(&body).map_err(|e| format!("解析响应失败: {e}"))?;
    if p.status != "success" {
        return Err(if p.message.is_empty() { "查询失败".into() } else { p.message });
    }
    Ok(GeoLocation {
        ip: p.query,
        country: p.country,
        city: p.city,
        lat: p.lat,
        lon: p.lon,
        isp: p.isp,
        source: "ip-api.com".into(),
    })
}

/// 极简 HTTP/1.1 GET。只用于这一处查询：响应是几十字节的 JSON，
/// 引一个 HTTP 客户端依赖不划算（而且不涉及凭据，走明文没有额外暴露）。
fn http_get(
    host: &str,
    port: u16,
    path: &str,
    interface: Option<&str>,
) -> Result<String, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析 {host} 失败: {e}"))?
        .next()
        .ok_or_else(|| format!("{host} 没有可用地址"))?;

    let mut socket = connect_bound(addr, interface)?;
    socket
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|_| socket.set_write_timeout(Some(TIMEOUT)))
        .map_err(|e| format!("设置超时失败: {e}"))?;

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    socket
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送请求失败: {e}"))?;

    let mut raw = Vec::new();
    socket
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取响应失败: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "响应格式异常".to_string())?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!("查询返回非 200：{}", head.lines().next().unwrap_or("")));
    }
    Ok(body.trim().to_string())
}

/// 建一个已连接、且（可选）绑定到指定网卡的 TCP socket。
///
/// 走 raw socket 而不是 `TcpStream::connect`：`IP_BOUND_IF` 必须在 `connect`
/// **之前**设置，而标准库没有暴露「建 socket → 设置选项 → 连接」这条路径。
fn connect_bound(addr: SocketAddr, interface: Option<&str>) -> Result<std::net::TcpStream, String> {
    use std::os::unix::io::FromRawFd;

    let family = if addr.is_ipv4() { libc::AF_INET } else { libc::AF_INET6 };
    // SAFETY: socket(2) 返回 fd 或 -1，下面立刻检查
    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err("创建 socket 失败".into());
    }
    // SAFETY: fd 是刚由 socket(2) 新建的，所有权从此交给 stream
    let stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };

    if let Some(iface) = interface {
        let cname = std::ffi::CString::new(iface).map_err(|_| "网卡名含 NUL".to_string())?;
        // SAFETY: if_nametoindex 只读这个 C 字符串
        let index = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        if index == 0 {
            return Err(format!("找不到网卡 {iface}"));
        }
        // SAFETY: level/optname 与「4 字节的接口索引」这个长度完全对应
        let rc = unsafe {
            libc::setsockopt(
                std::os::unix::io::AsRawFd::as_raw_fd(&stream),
                libc::IPPROTO_IP,
                libc::IP_BOUND_IF,
                &index as *const u32 as *const libc::c_void,
                std::mem::size_of::<u32>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(format!("绑定网卡 {iface} 失败"));
        }
    }

    // connect：非阻塞 + poll 超时，避免系统默认的漫长等待
    stream
        .set_nonblocking(true)
        .map_err(|e| format!("设置非阻塞失败: {e}"))?;
    // 在同一作用域内构造 sockaddr 并借用它 —— 不堆分配、不泄漏。
    let sa = SockAddrStorage::new(addr);
    let rc = {
        // SAFETY: `sa` 覆盖本次调用的生命周期；指针与长度都来自同一个联合体
        unsafe {
            libc::connect(
                std::os::unix::io::AsRawFd::as_raw_fd(&stream),
                sa.as_ptr(),
                sa.len(),
            )
        }
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let in_progress = err.raw_os_error() == Some(libc::EINPROGRESS);
        if !in_progress {
            return Err(format!("连接 {addr} 失败: {err}"));
        }
        let mut pfd = libc::pollfd {
            fd: std::os::unix::io::AsRawFd::as_raw_fd(&stream),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: pfd 是合法且存活的栈上结构，长度 1
        let n = unsafe { libc::poll(&mut pfd, 1, TIMEOUT.as_millis() as libc::c_int) };
        if n <= 0 {
            return Err(format!("连接 {addr} 超时"));
        }
    }
    // 回到阻塞模式：后面的 read/write 用标准库的超时语义
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("恢复阻塞失败: {e}"))?;
    Ok(stream)
}

/// 栈上的 sockaddr 存储：按地址族存 v4 或 v6，并给出指针与长度。
///
/// 用联合体而不是 `Box::into_raw`：后者要靠「永不释放」来延长生命周期，
/// 那是有意的内存泄漏。这里整块都在调用者的栈上，作用域结束即失效。
#[repr(C)]
union SockAddrStorage {
    v4: std::mem::ManuallyDrop<libc::sockaddr_in>,
    v6: std::mem::ManuallyDrop<libc::sockaddr_in6>,
    _size: [u8; 28], // 保证足够大（sockaddr_in 16 / sockaddr_in6 28）
}

impl SockAddrStorage {
    fn new(addr: SocketAddr) -> Self {
        match addr {
            SocketAddr::V4(v4) => Self {
                v4: std::mem::ManuallyDrop::new(libc::sockaddr_in {
                    sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                    sin_family: libc::AF_INET as u8,
                    sin_port: v4.port().to_be(),
                    sin_addr: libc::in_addr {
                        // `s_addr` 需要网络字节序：octets() 已是网络序
                        s_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                    sin_zero: [0; 8],
                }),
            },
            SocketAddr::V6(v6) => Self {
                v6: std::mem::ManuallyDrop::new(libc::sockaddr_in6 {
                    sin6_len: std::mem::size_of::<libc::sockaddr_in6>() as u8,
                    sin6_family: libc::AF_INET6 as u8,
                    sin6_port: v6.port().to_be(),
                    sin6_flowinfo: v6.flowinfo(),
                    sin6_addr: libc::in6_addr {
                        s6_addr: v6.ip().octets(),
                    },
                    sin6_scope_id: v6.scope_id(),
                }),
            },
        }
    }

    fn as_ptr(&self) -> *const libc::sockaddr {
        // SAFETY: 联合体的两个成员都以 sockaddr 开头（C 布局的约定）
        unsafe { &self.v4 as *const _ as *const libc::sockaddr }
    }

    fn len(&self) -> libc::socklen_t {
        // 由 `new` 构造时写入的 family 决定；这里直接按大小取较大者会越界，
        // 所以用 sin6_len（两族里较大的那个结构）里的值判断。
        // SAFETY: `_size` 保证联合体至少 28 字节，读 sin6_len 在范围内
        unsafe {
            let s = &self.v6;
            s.sin6_len as libc::socklen_t
        }
    }
}

/// 位置缓存：同一个 IP 只查一次。
///
/// 两层理由：外部服务有限流（ip-api 免费额度是每分钟 45 次），而且每多查一次
/// 就多一次「把 IP 发给第三方」。
#[derive(Debug, Default)]
pub struct GeoCache {
    map: HashMap<String, GeoLocation>,
}

impl GeoCache {
    pub fn get(&self, key: &str) -> Option<GeoLocation> {
        self.map.get(key).cloned()
    }

    pub fn put(&mut self, key: &str, loc: GeoLocation) {
        self.map.insert(key.to_string(), loc);
    }

    /// 取或查。`key` 与 `ip` 分开：本机那次的缓存键是固定的 `SELF_KEY`，
    /// 而不是某个具体 IP。
    pub fn get_or_lookup(
        &mut self,
        key: &str,
        ip: &str,
        interface: Option<&str>,
    ) -> Result<GeoLocation, String> {
        if let Some(hit) = self.get(key) {
            return Ok(hit);
        }
        let loc = lookup(ip, interface)?;
        self.put(key, loc.clone());
        Ok(loc)
    }
}

/// 本机位置的缓存键：它不是按被查 IP 缓存的，用一个固定键。
pub const SELF_KEY: &str = "__self__";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_successful_response() {
        let body = r#"{"status":"success","country":"Hong Kong","city":"Hong Kong",
                       "lat":22.3193,"lon":114.169,"isp":"Vapeline Technology"}"#;
        let p: IpApiResponse = serde_json::from_str(body).unwrap();
        assert_eq!(p.status, "success");
        assert!((p.lat - 22.3193).abs() < 1e-9);
        assert!((p.lon - 114.169).abs() < 1e-9);
    }

    /// 失败响应必须带出对方的说明，而不是变成「坐标 0,0」——
    /// 那会在地球上画一个几内亚湾的点，看起来像真的。
    #[test]
    fn failure_response_carries_the_message() {
        let body = r#"{"status":"fail","message":"reserved range","country":"","city":"","lat":0,"lon":0}"#;
        let p: IpApiResponse = serde_json::from_str(body).unwrap();
        assert_eq!(p.status, "fail");
        assert_eq!(p.message, "reserved range");
    }

    #[test]
    fn cache_avoids_repeat_lookups() {
        let mut c = GeoCache::default();
        assert!(c.get("1.2.3.4").is_none());
        c.put("1.2.3.4", GeoLocation {
            ip: "1.2.3.4".into(),
            country: "X".into(),
            city: "Y".into(),
            lat: 1.0,
            lon: 2.0,
            isp: String::new(),
            source: "test".into(),
        });
        // 命中缓存时不会发起网络请求（这一步若去查会因无网络而失败）
        let hit = c
            .get_or_lookup("1.2.3.4", "1.2.3.4", None)
            .expect("应当命中缓存");
        assert_eq!(hit.city, "Y");
    }
}
