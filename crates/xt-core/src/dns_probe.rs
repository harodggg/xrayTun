//! DNS 解析器的候选池与「哪个最好用」的探测。
//!
//! # 为什么不能只看延迟
//!
//! 实测（本机，绑 en0 直连，每次用全新随机域名防缓存）：
//!
//! ```text
//! 223.5.5.5   直连 31ms
//! 8.8.8.8     直连 69ms     ← 中国到 Google DNS 不可能这么快
//! 8.8.8.8     经节点 200ms
//! ```
//!
//! 69ms 那个说明**运营商在 53 端口抢答**：任何 UDP DNS 查询它都回一个自己的
//! 答案。抢答的解析器**延迟一定很漂亮**，但答案可能是错的（投毒、广告跳转）。
//!
//! 所以「最好用」要同时判两件事：
//!
//! 1. **能不能通、多快** —— 直接 UDP 查询，取中位数；
//! 2. **答得对不对** —— 让所有候选查同一个域名，答案和**多数派**不一致的
//!    标记为可疑。
//!
//! 多数派投票不需要任何外部参照：国内解析器绝大多数是诚实的，抢答者会
//! 独自给出一套不同的地址。
//!
//! # 为什么查询名每次都随机
//!
//! 第一版用了固定域名，结果 `114.114.115.115` 报出 **1ms** —— 公网 DNS 不可能
//! 1ms，那是**核心自己的 DNS 缓存**答的（TUN 模式下所有 :53 都被接管）。
//! 每次换一个不存在的随机域名，任何缓存都答不上来，只能真的去问。

use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// 候选池里的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsKind {
    /// 国内明文解析器：用于 `geosite:cn`，走直连。
    Domestic,
    /// 国外明文解析器：**不建议**用于分流解析 —— 明文入墙会被投毒。
    ForeignPlain,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DnsCandidate {
    pub server: &'static str,
    pub label: &'static str,
    pub kind: DnsKind,
}

/// 候选池。**每一项都在本机实测过可用性与延迟**，不是抄来的清单。
///
/// 括号里是实测中位延迟，可作为「这台机器上大概什么水平」的参考。
/// 换网络环境（换 ISP、换城市）后这些数字会变，所以运行时仍要重新探测。
pub const DNS_POOL: &[DnsCandidate] = &[
    DnsCandidate { server: "223.5.5.5", label: "阿里 AliDNS", kind: DnsKind::Domestic },        // 32ms
    DnsCandidate { server: "223.6.6.6", label: "阿里 AliDNS 备", kind: DnsKind::Domestic },     // 34ms
    DnsCandidate { server: "180.76.76.76", label: "百度 DNS", kind: DnsKind::Domestic },        // 46ms
    DnsCandidate { server: "180.184.1.1", label: "字节 DNS", kind: DnsKind::Domestic },         // 47ms
    DnsCandidate { server: "119.29.29.29", label: "腾讯 DNSPod", kind: DnsKind::Domestic },     // 50ms
    DnsCandidate { server: "114.114.114.114", label: "114DNS", kind: DnsKind::Domestic },       // 51ms
    DnsCandidate { server: "101.226.4.6", label: "电信 上海", kind: DnsKind::Domestic },         // 54ms
    DnsCandidate { server: "1.2.4.8", label: "CNNIC", kind: DnsKind::Domestic },                // 54ms
    DnsCandidate { server: "218.30.118.6", label: "电信 北京", kind: DnsKind::Domestic },        // 58ms
    DnsCandidate { server: "119.28.28.28", label: "腾讯 DNSPod 备", kind: DnsKind::Domestic },  // 59ms
    DnsCandidate { server: "123.125.81.6", label: "联通", kind: DnsKind::Domestic },             // 60ms
    DnsCandidate { server: "52.80.66.66", label: "OneDNS 备", kind: DnsKind::Domestic },        // 69ms
    DnsCandidate { server: "117.50.10.10", label: "OneDNS", kind: DnsKind::Domestic },          // 129ms
    DnsCandidate { server: "210.2.4.8", label: "CNNIC 备", kind: DnsKind::Domestic },           // 353ms（慢）
    // 国外明文：列出来是为了让界面能显示「这些不适合放国内解析位」，
    // 而不是推荐使用。
    DnsCandidate { server: "8.8.8.8", label: "Google（明文，易被抢答）", kind: DnsKind::ForeignPlain },
    DnsCandidate { server: "9.9.9.9", label: "Quad9（明文）", kind: DnsKind::ForeignPlain },
];

/// 探测结果。
#[derive(Debug, Clone, Serialize)]
pub struct DnsProbe {
    pub server: String,
    pub label: String,
    pub kind: DnsKind,
    /// 中位延迟。全失败为 `None`。
    pub latency_ms: Option<u32>,
    /// 是否给出了答案（通了没）。
    pub answered: bool,
    /// 答案与多数派不一致 —— 大概率被抢答/投毒。
    pub suspect: bool,
}

impl DnsProbe {
    pub fn usable(&self) -> bool {
        self.answered && self.latency_ms.is_some() && !self.suspect
    }
}

/// 用来做「答得对不对」比对的参照域名。
///
/// **必须选一个地址稳定、不按地域调度的域名。**
/// 第一版用的是 `www.microsoft.com` —— 它重度 CDN 化，不同解析器合法地
/// 返回不同地址，于是电信的两个解析器被误判成「可疑」。`example.com` 是
/// IANA 保留的示例域名，只有一条稳定 A 记录，答案不一致才是真的有问题。
///
/// 已知局限：这个比对**只能发现「对普通域名乱答」**，发现不了
/// 「对被墙域名投毒」—— 后者恰恰是多数派都错、少数派才对，
/// 多数派投票在这种场景下会把正确答案标记成异常。
pub const REFERENCE_DOMAIN: &str = "example.com";

/// 构造一个 A 查询。纯函数，可测。
pub fn build_query(name: &str, id: u16) -> Vec<u8> {
    let mut q = Vec::with_capacity(32);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // 标准查询，递归期望
    q.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    q.extend_from_slice(&[0x00, 0x00]); // ANCOUNT
    q.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    q.extend_from_slice(&[0x00, 0x00]); // ARCOUNT
    for label in name.split('.').filter(|l| !l.is_empty()) {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0x00, 0x01]); // TYPE A
    q.extend_from_slice(&[0x00, 0x01]); // CLASS IN
    q
}

/// 从应答里取出所有 A 记录。
///
/// 只实现到够用为止：跳过问题段，遍历回答段，遇到压缩指针按 2 字节跳过。
pub fn parse_a_records(buf: &[u8]) -> Option<Vec<IpAddr>> {
    if buf.len() < 12 {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12;

    // 跳过问题段（名字 + 4 字节 type/class）
    for _ in 0..qd {
        i = skip_name(buf, i)?;
        i = i.checked_add(4)?;
        if i > buf.len() {
            return None;
        }
    }

    let mut out = Vec::new();
    for _ in 0..an {
        i = skip_name(buf, i)?;
        if i + 10 > buf.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let rdlen = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        i += 10;
        if i + rdlen > buf.len() {
            return None;
        }
        if rtype == 1 && rdlen == 4 {
            out.push(IpAddr::from([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]));
        }
        i += rdlen;
    }
    Some(out)
}

/// 跳过一个域名（可能是压缩指针）。
fn skip_name(buf: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *buf.get(i)?;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(i + 2); // 压缩指针，到此为止
        }
        i += 1 + len as usize;
        if i > buf.len() {
            return None;
        }
    }
}

/// 用来测延迟的域名：**轮换一批极常见的域名**。
///
/// 第一版用的是「每次随机的、不存在的域名」，想的是「任何缓存都答不上来」。
/// 实测下来它是错的工具：不存在的域名会逼解析器做一次**完整递归**，
/// 于是测到的是「递归一次要多久」，而不是「到解析器有多快」。同一台阿里
/// DNS 用随机名测是 142ms，用常见域名测是 30ms —— 而后者才是我们排序想要的量。
///
/// 轮换是为了不让某一次恰好撞上解析器的慢路径。真正必须防的是**本机**
/// 的缓存（TUN 模式下所有 :53 都被核心接管过），那由绑定 en0 解决。
const PROBE_NAMES: &[&str] = &[
    "www.baidu.com",
    "www.qq.com",
    "www.taobao.com",
    "www.jd.com",
    "www.163.com",
];

fn probe_name(sample: usize) -> &'static str {
    PROBE_NAMES[sample % PROBE_NAMES.len()]
}

/// 把 socket 绑到物理网卡，**绕开隧道与核心**。
///
/// 不绑的话所有查询都会经 TUN → 核心的 gVisor 栈 → 再到解析器，于是
/// 并发探测时测到的是**核心的排队**而不是解析器的延迟：实测同一个
/// 阿里 DNS，绑 en0 是 32ms，不绑（并发 6）是 155ms —— 差 5 倍，
/// 而且会把慢的解析器排到前面。
///
/// 这也是核心自己的 `direct` 出站用的机制（`IP_BOUND_IF`，见 docs/04 §8）。
fn bind_to_interface(socket: &std::net::UdpSocket, interface: &str) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let cname = std::ffi::CString::new(interface)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "网卡名含 NUL"))?;
    // SAFETY: if_nametoindex 只读字符串；setsockopt 的参数长度与类型匹配。
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
            socket.as_raw_fd(),
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

async fn udp_query(
    server: &str,
    name: &str,
    timeout: Duration,
    interface: Option<&str>,
) -> Result<Vec<IpAddr>> {
    use tokio::net::UdpSocket;
    // 先用标准库建 socket 才能设置 `IP_BOUND_IF`（tokio 的 UdpSocket 不暴露
    // setsockopt 的入口），设完再交给 tokio。
    let std_socket = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| Error::Probe(format!("建 UDP socket 失败：{e}")))?;
    if let Some(iface) = interface {
        if let Err(e) = bind_to_interface(&std_socket, iface) {
            // 绑定失败不是致命错误，但必须让调用方知道这次测的是绕了隧道的数字。
            tracing::warn!(interface = iface, error = %e, "DNS 探测无法绑定物理网卡，延迟会偏大");
        }
    }
    std_socket
        .set_nonblocking(true)
        .map_err(|e| Error::Probe(format!("设置非阻塞失败：{e}")))?;
    let socket = UdpSocket::from_std(std_socket)
        .map_err(|e| Error::Probe(format!("接管 socket 失败：{e}")))?;
    let addr = format!("{server}:53");
    let packet = build_query(name, (std::process::id() & 0xffff) as u16);
    socket
        .send_to(&packet, &addr)
        .await
        .map_err(|e| Error::Probe(format!("发送查询失败：{e}")))?;

    let mut buf = [0u8; 1500];
    let (n, _) = tokio::time::timeout(timeout, socket.recv_from(&mut buf))
        .await
        .map_err(|_| Error::Probe("查询超时".into()))?
        .map_err(|e| Error::Probe(format!("接收应答失败：{e}")))?;
    parse_a_records(&buf[..n]).ok_or_else(|| Error::Probe("应答无法解析".into()))
}

/// 探测一个解析器：先测延迟（用随机名），再用参照域名取答案。
pub async fn probe_one(
    server: &str,
    label: &str,
    kind: DnsKind,
    timeout: Duration,
    samples: usize,
    interface: Option<&str>,
) -> DnsProbe {
    let mut latencies = Vec::new();
    for i in 0..samples.max(1) {
        let start = Instant::now();
        if udp_query(server, probe_name(i), timeout, interface).await.is_ok() {
            // 空答案也算通：我们量的是往返，不是解析结果。
            latencies.push(start.elapsed().as_millis().min(u32::MAX as u128) as u32);
        }
    }

    // 参照域名：能答出来才有资格参与「答得对不对」的比对。
    let reference = udp_query(server, REFERENCE_DOMAIN, timeout, interface).await.ok();

    DnsProbe {
        server: server.to_string(),
        label: label.to_string(),
        kind,
        latency_ms: median(&mut latencies),
        answered: reference.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
            || !latencies.is_empty(),
        suspect: false, // 由 majority_suspects 统一判定
    }
}

fn median(v: &mut [u32]) -> Option<u32> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// 多数派投票：返回答案与多数不一致的服务器下标。
///
/// 抢答者有一个很明显的特征 —— 它**独自**给出一套地址，而诚实的解析器
/// 之间是互相一致的。所以不需要外部参照物就能把它挑出来。
pub fn majority_suspects(answers: &[(String, Vec<IpAddr>)]) -> Vec<String> {
    use std::collections::HashMap;
    if answers.len() < 3 {
        // 样本太少，「多数派」没有意义，宁可不下结论。
        return Vec::new();
    }
    let mut votes: HashMap<Vec<IpAddr>, usize> = HashMap::new();
    for (_, ips) in answers {
        if ips.is_empty() {
            continue;
        }
        let mut key = ips.clone();
        key.sort();
        *votes.entry(key).or_insert(0) += 1;
    }
    let Some((winner, _)) = votes.iter().max_by_key(|(_, c)| **c) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (server, ips) in answers {
        if ips.is_empty() {
            continue;
        }
        let mut key = ips.clone();
        key.sort();
        if &key != winner {
            out.push(server.clone());
        }
    }
    out
}

/// 并发探测整池，按「可用 + 快」排序。
pub async fn probe_pool(
    candidates: &[DnsCandidate],
    timeout: Duration,
    samples: usize,
    concurrency: usize,
    interface: Option<&str>,
) -> Vec<DnsProbe> {
    use tokio::sync::Semaphore;
    use std::sync::Arc;
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::new();
    for c in candidates {
        let sem = sem.clone();
        // 把字段拷成 owned 再 move 进任务：`DnsCandidate` 借用自入参，
        // 而 `tokio::spawn` 要求 'static。
        let (server, label, kind) = (c.server.to_string(), c.label.to_string(), c.kind);
        let iface = interface.map(str::to_string);
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.expect("信号量不会关闭");
            probe_one(&server, &label, kind, timeout, samples, iface.as_deref()).await
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        if let Ok(r) = h.await {
            out.push(r);
        }
    }

    // 取参照答案做多数派投票。
    let mut answers = Vec::new();
    for r in &out {
        if let Ok(ips) = udp_query(&r.server, REFERENCE_DOMAIN, timeout, interface).await {
            answers.push((r.server.clone(), ips));
        }
    }
    let suspects = majority_suspects(&answers);
    for r in &mut out {
        r.suspect = suspects.contains(&r.server);
    }

    // 排前面的是「可用」的，同组内按延迟升序。
    out.sort_by_key(|r| (!r.usable(), r.latency_ms.unwrap_or(u32::MAX)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_packet_is_well_formed() {
        let q = build_query("a.example.com", 0x1234);
        assert_eq!(&q[..2], &[0x12, 0x34], "ID 应在头部");
        assert_eq!(&q[2..4], &[0x01, 0x00], "标准查询 + 递归期望");
        assert_eq!(&q[4..6], &[0x00, 0x01], "QDCOUNT = 1");
        // 名字段编码：<len><label>... 0
        // "a.example.com" → 1,'a', 7,"example", 3,"com", 0
        assert_eq!(q[12], 1);
        assert_eq!(q[13], b'a');
        assert_eq!(q[14], 7);
        assert_eq!(&q[15..22], b"example");
        assert_eq!(q[22], 3);
        assert_eq!(&q[23..26], b"com");
        assert_eq!(q[26], 0, "名字段以 0 结束");
        assert_eq!(&q[q.len() - 4..], &[0x00, 0x01, 0x00, 0x01], "TYPE A / CLASS IN");
    }

    /// 造一个含压缩指针的应答，确认 A 记录能被取出来。
    #[test]
    fn parses_a_records_with_compression() {
        let mut r = Vec::new();
        r.extend_from_slice(&[0x12, 0x34, 0x81, 0x80]); // ID + flags
        r.extend_from_slice(&[0x00, 0x01]); // QDCOUNT 1
        r.extend_from_slice(&[0x00, 0x01]); // ANCOUNT 1
        r.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        // 问题：example.com A IN
        r.extend_from_slice(&[7]);
        r.extend_from_slice(b"example");
        r.extend_from_slice(&[3]);
        r.extend_from_slice(b"com");
        r.push(0);
        r.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        // 回答：名字用指针指回 0x0c
        r.extend_from_slice(&[0xc0, 0x0c]);
        r.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        r.extend_from_slice(&[0x00, 0x00, 0x00, 0x3c]); // TTL
        r.extend_from_slice(&[0x00, 0x04]); // RDLENGTH
        r.extend_from_slice(&[93, 184, 216, 34]);

        let ips = parse_a_records(&r).unwrap();
        assert_eq!(ips, vec!["93.184.216.34".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn truncated_response_is_rejected() {
        // 声称有 1 条回答但数据不够
        let bad = vec![0, 1, 0x81, 0x80, 0, 0, 0, 1, 0, 0, 0, 0, 0xc0];
        assert!(parse_a_records(&bad).is_none());
        assert!(parse_a_records(&[0u8; 4]).is_none());
    }

    /// 多数派投票要把「独自给一套地址」的那个挑出来。
    #[test]
    fn majority_flags_the_odd_one_out() {
        let a: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap()];
        let b: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap()];
        let c: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap()];
        let hijacked: Vec<IpAddr> = vec!["10.0.0.1".parse().unwrap()];
        let answers = vec![
            ("honest-1".to_string(), a),
            ("honest-2".to_string(), b),
            ("honest-3".to_string(), c),
            ("hijacker".to_string(), hijacked),
        ];
        let s = majority_suspects(&answers);
        assert_eq!(s, vec!["hijacker".to_string()]);
    }

    /// 样本不足时不下结论 —— 宁可不说，也不要误伤。
    #[test]
    fn majority_stays_quiet_with_too_few_samples() {
        let a: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap()];
        let b: Vec<IpAddr> = vec!["10.0.0.1".parse().unwrap()];
        assert!(majority_suspects(&[(("s1".into()), a), (("s2".into()), b)]).is_empty());
        assert!(majority_suspects(&[]).is_empty());
    }

    /// 地址顺序不同但集合相同，不应算作不一致。
    #[test]
    fn majority_ignores_record_order() {
        let a: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap(), "2.2.2.2".parse().unwrap()];
        let b: Vec<IpAddr> = vec!["2.2.2.2".parse().unwrap(), "1.1.1.1".parse().unwrap()];
        let c: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap(), "2.2.2.2".parse().unwrap()];
        let answers = vec![
            ("s1".to_string(), a),
            ("s2".to_string(), b),
            ("s3".to_string(), c),
        ];
        assert!(majority_suspects(&answers).is_empty(), "同集合不同顺序不该被判可疑");
    }

    /// 池子里不能有重复项，也不能有空的标签。
    #[test]
    fn pool_is_clean() {
        let mut seen = std::collections::HashSet::new();
        for c in DNS_POOL {
            assert!(seen.insert(c.server), "池子里有重复：{}", c.server);
            assert!(!c.label.is_empty(), "{} 没有名称", c.server);
            assert!(c.server.parse::<IpAddr>().is_ok(), "池子里应只放 IP（域名会引入自举依赖）：{}", c.server);
        }
    }

    /// 国内池至少要够挑——太少的话「选最好的」没有意义。
    #[test]
    fn pool_has_enough_domestic_candidates() {
        let n = DNS_POOL.iter().filter(|c| c.kind == DnsKind::Domestic).count();
        assert!(n >= 6, "国内候选只有 {n} 个，不够挑");
    }

    /// 参照域名必须是个正常域名，否则每次查询都会失败。
    #[test]
    fn reference_domain_is_sane() {
        assert!(REFERENCE_DOMAIN.contains('.'));
        assert!(!REFERENCE_DOMAIN.starts_with('.'));
    }
}
