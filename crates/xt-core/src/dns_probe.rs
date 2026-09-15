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
//!
//! # 两组，两条测量路径
//!
//! 国内解析器和国外解析器**不能用同一条路径测**，否则测出来的根本不是它：
//!
//! | 组 | 谁在用 | 怎么测 | 为什么这么测 |
//! |---|---|---|---|
//! | 国内 | `geosite:cn` → `direct_servers[0]` | 明文 UDP，绑 en0 直连 | 它本来就是直连用的 |
//! | 国外 | `geosite:geolocation-!cn` → `remote_servers[0]` | DoH，经本地 SOCKS 入站 | 直连**连不上** |
//!
//! 本机实测：直连 `https://1.1.1.1/dns-query` 用 8 秒超时都拿不到连接；
//! 经节点 0.23–0.39 秒就有答案。所以国外那一组在节点未连接时标成
//! **「未探测」**，而不是硬走直连测一遍、再把超时谎报成「这台解析器不通」。
//!
//! 排序的意义只在第一位：`build_dns` 在分流模式下对每个列表只取**第一个**
//! 元素（`first_or`），所以「把最快的排到最前」就等于「换掉正在用的那台」。

use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// 候选池里的类别。**同时就是界面上的分组。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsKind {
    /// 国内明文解析器：用于 `geosite:cn` 那一条分流规则。
    Domestic,
    /// 国外解析器：用于 `geosite:geolocation-!cn` 那一条。
    Foreign,
}

/// 探测走的传输方式。
///
/// 分组不是随便分的 —— 它直接决定**怎么测才测得到真东西**：
///
/// * 国内解析器是明文 UDP，绑物理网卡直连测。那就是它被使用时的路径。
/// * 国外解析器**只有经节点才连得上**：本机实测直连 `https://1.1.1.1/dns-query`
///   用 8 秒超时都拿不到连接，经节点 0.26–0.37 秒就有答案。而它在配置里本来
///   也是经节点用的（`remote_servers` 配 `geosite:geolocation-!cn`），
///   所以「经节点测」既不是偷懒，也不是近似，就是它的真实成本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsTransport {
    /// 明文 UDP，53 端口。
    PlainUdp,
    /// DoH（RFC 8484），`application/dns-message`。
    Doh,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DnsCandidate {
    pub server: &'static str,
    pub label: &'static str,
    pub kind: DnsKind,
    pub transport: DnsTransport,
}

/// 候选池。**每一项都在本机实测过可用性与延迟**，不是抄来的清单。
///
/// 括号里是实测中位延迟，可作为「这台机器上大概什么水平」的参考。
/// 换网络环境（换 ISP、换城市、换节点）后这些数字会变，所以运行时仍要重新探测。
pub const DNS_POOL: &[DnsCandidate] = &[
    // ---------------- 国内：明文 UDP，直连测量 ----------------
    DnsCandidate { server: "223.5.5.5", label: "阿里 AliDNS", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },        // 32ms
    DnsCandidate { server: "223.6.6.6", label: "阿里 AliDNS 备", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },     // 34ms
    DnsCandidate { server: "180.76.76.76", label: "百度 DNS", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },        // 46ms
    DnsCandidate { server: "180.184.1.1", label: "字节 DNS", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },         // 47ms
    DnsCandidate { server: "119.29.29.29", label: "腾讯 DNSPod", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },     // 50ms
    DnsCandidate { server: "114.114.114.114", label: "114DNS", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },       // 51ms
    DnsCandidate { server: "101.226.4.6", label: "电信 上海", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },         // 54ms
    DnsCandidate { server: "1.2.4.8", label: "CNNIC", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },                // 54ms
    DnsCandidate { server: "218.30.118.6", label: "电信 北京", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },        // 58ms
    DnsCandidate { server: "119.28.28.28", label: "腾讯 DNSPod 备", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },  // 59ms
    DnsCandidate { server: "123.125.81.6", label: "联通", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },             // 60ms
    DnsCandidate { server: "52.80.66.66", label: "OneDNS 备", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },        // 69ms
    DnsCandidate { server: "117.50.10.10", label: "OneDNS", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },          // 129ms
    DnsCandidate { server: "210.2.4.8", label: "CNNIC 备", kind: DnsKind::Domestic, transport: DnsTransport::PlainUdp },           // 353ms（慢）

    // ---------------- 国外：DoH，经节点测量 ----------------
    //
    // 全部写成 **IP 形式的 DoH 端点**。写成域名（`https://dns.google/dns-query`）
    // 会引入一个自举依赖：解析这个域名本身还要先问一次 DNS，而那时隧道可能还没
    // 建立。IP 端点没有这个问题。
    //
    // 实测（经节点，本机，各 3 次取中位）：AdGuard 234ms、Cloudflare 备 250ms、
    // Cloudflare 255ms、Google 321ms、Quad9 390ms（Quad9 抖动大，有一次 1.39s，
    // 所以取样次数的中位数比单次结果可靠）。
    //
    // 这几台都是加密传输，**不存在被抢答/投毒的可能**，所以它们几乎不会被
    // 多数派投票标成可疑 —— 这一组真正要比的就是「经这个节点谁快、谁通」。
    DnsCandidate { server: "https://94.140.14.14/dns-query", label: "AdGuard", kind: DnsKind::Foreign, transport: DnsTransport::Doh },   // 234ms
    DnsCandidate { server: "https://1.0.0.1/dns-query", label: "Cloudflare 备", kind: DnsKind::Foreign, transport: DnsTransport::Doh },  // 250ms
    DnsCandidate { server: "https://1.1.1.1/dns-query", label: "Cloudflare", kind: DnsKind::Foreign, transport: DnsTransport::Doh },     // 255ms
    DnsCandidate { server: "https://8.8.8.8/dns-query", label: "Google", kind: DnsKind::Foreign, transport: DnsTransport::Doh },         // 321ms
    DnsCandidate { server: "https://9.9.9.9/dns-query", label: "Quad9", kind: DnsKind::Foreign, transport: DnsTransport::Doh },          // 390ms
];

/// 探测结果。
#[derive(Debug, Clone, Serialize)]
pub struct DnsProbe {
    pub server: String,
    pub label: String,
    pub kind: DnsKind,
    pub transport: DnsTransport,
    /// 中位延迟。全失败为 `None`。
    pub latency_ms: Option<u32>,
    /// 是否给出了答案（通了没）。
    pub answered: bool,
    /// 答案与**同组**多数派不一致 —— 大概率被抢答/投毒。
    pub suspect: bool,
    /// 「没测」的原因。有值时界面要显示它，**不能把 `None` 一律当成「不通」**：
    /// 节点没连接时国外那一组根本没测，报「不通」是假话。
    pub note: Option<String>,
}

impl DnsProbe {
    pub fn usable(&self) -> bool {
        self.answered && self.latency_ms.is_some() && !self.suspect
    }
}

/// 一次探测的上下文：用哪条路径、测多久、测几次。
#[derive(Debug, Clone)]
pub struct ProbeSpec {
    pub timeout: Duration,
    pub samples: usize,
    /// 直连测量时绑定的物理网卡（`IP_BOUND_IF`）。
    pub interface: Option<String>,
    /// 经节点测量用的 SOCKS5 地址，如 `127.0.0.1:10808`。
    ///
    /// 传 `None` 时国外那一组**不会退化成直连去测** —— 直连测国外 DoH 只会
    /// 得到超时，把「没连节点」误报成「这台解析器不通」。这种情况统一标成
    /// `note = "节点未连接"`。
    pub socks: Option<String>,
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
    // 实现在 `net` 里，和节点 RTT 探测共用同一份 —— 那个也需要绕开隧道。
    crate::net::bind_to_interface_fd(socket.as_raw_fd(), interface)
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

/// 经 SOCKS5 发一次 DoH 查询（RFC 8484 的 POST 形式），返回 A 记录。
///
/// 响应体就是**和明文 DNS 完全相同的线格式**，所以 [`parse_a_records`] 直接能用，
/// 不必为了探测再引入一个 DoH 客户端。
///
/// 走 `curl` 而不是 HTTP 客户端：它自带 `--socks5-hostname`（本地已有 SOCKS
/// 入站在跑）和 `--max-time`，macOS 也自带，和 `update.rs` 的选择一致。
async fn doh_query(
    url: &str,
    name: &str,
    timeout: Duration,
    socks: Option<&str>,
) -> Result<Vec<IpAddr>> {
    let socks = socks.ok_or_else(|| Error::Probe("节点未连接".into()))?;
    let packet = build_query(name, (std::process::id() & 0xffff) as u16);

    let mut cmd = tokio::process::Command::new("/usr/bin/curl");
    cmd.args(curl_doh_args(url, socks, timeout))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Probe(format!("执行 curl 失败：{e}")))?;
    if let Some(mut input) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        input
            .write_all(&packet)
            .await
            .map_err(|e| Error::Probe(format!("写入查询报文失败：{e}")))?;
        // 必须显式关掉：`--data-binary @-` 要读到 EOF 才认为请求体结束。
        drop(input);
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| Error::Probe(format!("等待 curl 失败：{e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Probe(format!(
            "DoH 请求失败：{}",
            err.trim().chars().take(120).collect::<String>()
        )));
    }
    parse_a_records(&out.stdout).ok_or_else(|| Error::Probe("DoH 应答无法解析".into()))
}

/// curl 的 DoH 参数。纯函数，方便断言「确实带了代理和超时」。
fn curl_doh_args(url: &str, socks: &str, timeout: Duration) -> Vec<String> {
    vec![
        "-s".into(),
        "--max-time".into(),
        timeout.as_secs().max(1).to_string(),
        "--socks5-hostname".into(),
        socks.to_string(),
        "-H".into(),
        "content-type: application/dns-message".into(),
        "--data-binary".into(),
        "@-".into(),
        url.into(),
    ]
}

/// 按候选自己的传输方式测一次。
async fn measure(
    server: &str,
    transport: DnsTransport,
    name: &str,
    spec: &ProbeSpec,
) -> Result<Vec<IpAddr>> {
    match transport {
        DnsTransport::PlainUdp => udp_query(server, name, spec.timeout, spec.interface.as_deref()).await,
        DnsTransport::Doh => doh_query(server, name, spec.timeout, spec.socks.as_deref()).await,
    }
}

/// 探测一个解析器：先测延迟，再用参照域名取答案（供多数派投票）。
///
/// 返回的第二个值是参照答案，交给 [`probe_pool`] 做组内投票 —— 不在这里判，
/// 因为「多数派」只有在整池测完之后才存在。
async fn probe_one(c: &DnsCandidate, spec: &ProbeSpec) -> (DnsProbe, Option<Vec<IpAddr>>) {
    let mut probe = DnsProbe {
        server: c.server.to_string(),
        label: c.label.to_string(),
        kind: c.kind,
        transport: c.transport,
        latency_ms: None,
        answered: false,
        suspect: false,
        note: None,
    };

    // 国外那一组在节点没起来时是**测不了**，不是「不通」。
    if c.transport == DnsTransport::Doh && spec.socks.is_none() {
        probe.note = Some("节点未连接，未探测".into());
        return (probe, None);
    }

    let mut latencies = Vec::new();
    for i in 0..spec.samples.max(1) {
        let start = Instant::now();
        if measure(c.server, c.transport, probe_name(i), spec).await.is_ok() {
            // 空答案也算通：我们量的是往返，不是解析结果。
            //
            // DoH 这条路径上的计时包含一次 `curl` 进程启动（约 5–10ms），
            // 相对 250ms 量级的 DoH 往返是 2–4% 的固定偏置，且对所有候选
            // 完全一致，不影响排序。
            latencies.push(start.elapsed().as_millis().min(u32::MAX as u128) as u32);
        }
    }

    // 参照域名：能答出来才有资格参与「答得对不对」的比对。
    let reference = measure(c.server, c.transport, REFERENCE_DOMAIN, spec).await.ok();

    probe.latency_ms = median(&mut latencies);
    probe.answered = reference.as_ref().map(|v| !v.is_empty()).unwrap_or(false)
        || !latencies.is_empty();
    (probe, reference)
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

/// 国外组的并发度：**固定 1（串行）**。
///
/// 这五台全都要经**同一个节点**出去，并发测等于在测**节点的排队**，而不是
/// 在测「这台解析器离我有多远」—— 和国内组「不绑网卡就测到核心排队」是
/// 同一类错误，而且后果更严重：**名次会变**。
///
/// 实测同一批候选（同一个节点）：
///
/// ```text
/// 并发 4： AdGuard 450ms   Cloudflare备 306ms   Cloudflare 277ms   Google 697ms   Quad9 898ms
/// 串行：  AdGuard 234ms   Cloudflare备 250ms   Cloudflare 255ms   Google 321ms   Quad9 390ms
/// ```
///
/// 并发那组的名次被压成了「谁先抢到节点」，串行才是解析器本身的远近。
/// 代价是国外组总耗时变成串行的 5 台 × 4 次请求 ≈ 4–5 秒 —— 这是启动时的
/// 后台任务，值得。
const FOREIGN_CONCURRENCY: usize = 1;

/// 并发探测整池。顺序是「国内组在前、国外组在后，组内按可用 + 快排」。
pub async fn probe_pool(
    candidates: &[DnsCandidate],
    spec: &ProbeSpec,
    concurrency: usize,
) -> Vec<DnsProbe> {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    // 两组各用一个信号量：国内是快而多的明文 UDP，放开并发；
    // 国外共享同一个节点，必须串行（见 `FOREIGN_CONCURRENCY`）。
    let dom_sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let foreign_sem = Arc::new(Semaphore::new(FOREIGN_CONCURRENCY));
    let mut handles = Vec::new();
    for c in candidates {
        let sem = if c.kind == DnsKind::Foreign {
            foreign_sem.clone()
        } else {
            dom_sem.clone()
        };
        // `DnsCandidate` 的所有字段都是 `&'static str` / 无数据枚举，是 `Copy`，
        // 直接拷一份 move 进任务即可满足 `tokio::spawn` 的 'static 要求。
        let c = *c;
        let spec = spec.clone();
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.expect("信号量不会关闭");
            probe_one(&c, &spec).await
        }));
    }
    let mut measured = Vec::new();
    for h in handles {
        if let Ok(r) = h.await {
            measured.push(r);
        }
    }

    // 多数派投票**按组分开做**。跨组混投会把正常答案判成异常：国外解析器经
    // 节点出去，看到的 CDN 边缘和国内直连本来就可能不是同一批地址 —— 实测
    // `example.com` 两边这次恰好一致（都是 Cloudflare 的 104.20.23.154 /
    // 172.66.147.243），但那是运气，不是保证。
    for kind in [DnsKind::Domestic, DnsKind::Foreign] {
        let answers: Vec<(String, Vec<IpAddr>)> = measured
            .iter()
            .filter(|(p, _)| p.kind == kind)
            .filter_map(|(p, a)| a.clone().map(|ips| (p.server.clone(), ips)))
            .collect();
        let suspects = majority_suspects(&answers);
        for (p, _) in measured.iter_mut().filter(|(p, _)| p.kind == kind) {
            p.suspect = suspects.contains(&p.server);
        }
    }

    let mut out: Vec<DnsProbe> = measured.into_iter().map(|(p, _)| p).collect();
    // 国内在前、国外在后（`Domestic` = 0），组内可用的在前、按延迟升序。
    out.sort_by_key(|r| (r.kind as u8, !r.usable(), r.latency_ms.unwrap_or(u32::MAX)));
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
            // 只允许 IP、或 **IP 形式的** DoH 端点。写域名（`dns.google`）会
            // 引入自举依赖：解析它本身还要先问一次 DNS，而那时隧道可能还没建立。
            let host = c.server.strip_prefix("https://").unwrap_or(c.server);
            let host = host.split('/').next().unwrap_or(host);
            assert!(
                host.parse::<IpAddr>().is_ok(),
                "池子里应只放 IP（域名会引入自举依赖）：{}",
                c.server
            );
        }
    }

    /// 国内池至少要够挑——太少的话「选最好的」没有意义。
    #[test]
    fn pool_has_enough_domestic_candidates() {
        let n = DNS_POOL.iter().filter(|c| c.kind == DnsKind::Domestic).count();
        assert!(n >= 6, "国内候选只有 {n} 个，不够挑");
    }

    /// 国外池至少要 3 个：`majority_suspects` 少于 3 份答案就不下结论，
    /// 候选太少的话「答得对不对」这一半判据等于没有。
    #[test]
    fn pool_has_enough_foreign_candidates() {
        let n = DNS_POOL.iter().filter(|c| c.kind == DnsKind::Foreign).count();
        assert!(n >= 3, "国外候选只有 {n} 个，不够多数派投票");
    }

    /// 国内走明文直连、国外走 DoH 经节点。这是一条**语义约束**：写反了会
    /// 得到一组看起来正常、实际毫无意义的数字。
    #[test]
    fn transport_matches_kind() {
        for c in DNS_POOL {
            match c.kind {
                DnsKind::Domestic => {
                    assert_eq!(c.transport, DnsTransport::PlainUdp, "{} 应走明文直连", c.server)
                }
                DnsKind::Foreign => {
                    assert_eq!(c.transport, DnsTransport::Doh, "{} 应走 DoH 经节点", c.server)
                }
            }
        }
    }

    /// DoH 参数必须带上代理和超时，否则要么连不上、要么挂死。
    #[test]
    fn curl_doh_args_carry_proxy_and_timeout() {
        let args = curl_doh_args(
            "https://1.1.1.1/dns-query",
            "127.0.0.1:10808",
            Duration::from_secs(2),
        );
        let joined = args.join(" ");
        assert!(joined.contains("--socks5-hostname 127.0.0.1:10808"), "{joined}");
        assert!(joined.contains("--max-time 2"), "{joined}");
        assert!(joined.contains("--data-binary @-"), "报文要从 stdin 进：{joined}");
        assert_eq!(
            args.last().unwrap(),
            "https://1.1.1.1/dns-query",
            "URL 必须在最后：{joined}"
        );
    }

    /// 国外组必须串行：并发测等于测节点的排队而不是解析器的远近，
    /// 而且**名次会变**。这不是性能取舍，是正确性要求。
    #[test]
    fn foreign_group_is_probed_serially() {
        assert_eq!(
            FOREIGN_CONCURRENCY, 1,
            "国外候选共享同一个节点，并发会扭曲名次"
        );
    }

    /// 节点没连接时，国外那一组必须标「未探测」——不能直连测一遍，
    /// 再把必然的超时报成「这台解析器不通」。
    #[tokio::test]
    async fn foreign_group_is_not_probed_without_node() {
        let spec = ProbeSpec {
            timeout: Duration::from_millis(200),
            samples: 1,
            interface: None,
            socks: None,
        };
        let foreign = DNS_POOL
            .iter()
            .find(|c| c.kind == DnsKind::Foreign)
            .expect("池子里应有国外候选");
        let (probe, reference) = probe_one(foreign, &spec).await;
        assert!(reference.is_none(), "没连节点就不该有参照答案");
        assert!(probe.latency_ms.is_none());
        assert!(!probe.answered);
        assert!(!probe.usable());
        assert!(
            probe.note.as_deref().unwrap_or_default().contains("未探测"),
            "note = {:?}",
            probe.note
        );
        assert_eq!(probe.transport, DnsTransport::Doh);
    }

    /// 参照域名必须是个正常域名，否则每次查询都会失败。
    #[test]
    fn reference_domain_is_sane() {
        assert!(REFERENCE_DOMAIN.contains('.'));
        assert!(!REFERENCE_DOMAIN.starts_with('.'));
    }
}
