//! 解析 Xray 访问日志里的**连接行**：按出口计数，并把单条连接结构化。
//!
//! # 为什么需要它
//!
//! 核心的 `StatsService` 只提供**字节**计数器，没有连接数计数器。而有两类出口
//! 的字节数**永远读不到**，界面上就会显示成 `0 B` —— 看起来像「这个出口没用」，
//! 实际它在被大量使用：
//!
//! | 出口 | 实测连接数 | `StatsService` 字节 | 为什么字节恒为 0 |
//! |---|---|---|---|
//! | `dns-out`（协议 `dns`） | 4769 条 UDP | **0** | UDP 出站流量不计入统计 |
//! | `api`（本机回环） | 5374 条 TCP | **0** | 回环流量不计入统计 |
//! | `block`（`blackhole`） | 201 条 | 0 | 真的 0 —— 连接被拒，本就没有字节 |
//!
//! 所以「0 字节」对这三类出口是**测量盲区**，不是事实。连接数是那可得的指标，
//! 日志里每建立一条连接就有一行：
//!
//! ```text
//! 2026/09/20 11:15:13.475426 from tcp:198.18.0.1:58137 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]
//! ```
//!
//! 我们在这里把 `[入站 -> 出站]` 里的**出站**取出来计数，并把整行结构化成
//! [`ConnectionRecord`]（单连接可视化的数据源）。
//!
//! # 为什么不用另起一个日志 tail
//!
//! 核心的 stdout 已经有一个转发任务在逐行读取（`commands/core.rs`），
//! 那里是天然的单点。再开一个 tail 会重复读取、还要处理轮转与偏移，
//! 而且两份读取的时间戳会对不齐。
//!
//! # 域名是**时序配对**得来的（近似，必须如实标注）
//!
//! `accepted` 行里**没有域名**；域名在**另一行**：
//!
//! ```text
//! 2026/09/20 13:30:58.560290 [Info] [3163266252] app/dispatcher: sniffed domain: www.google.com
//! ```
//!
//! ⚠️ **`accepted` 行没有连接 ID**（ID 只在 `sniffed` / `proxy` 行上），所以
//! 只能按时序近似配对：为每条 `accepted` 找**时间最近的前一条 `sniffed`**，
//! 且时间差不得超过 [`PAIR_WINDOW_US`]（200ms）。配对结果带上
//! `domain_paired` / `domain_pair_delta_us` / `sniff_id`，界面据此标注「可能不准」。
//!
//! 实测量级（本机 `logs/app.jsonl`，5559 条 accepted）：配对时延
//! **p50 = 26µs、p95 = 129µs、max = 109ms**；只有约 **52%** 的 accepted 能配到域名，
//! 因为另一半**根本没有 sniffed 行**（`api` 回环、`dns-out` 的 IP 直连），
//! 不是算法漏了。按出口看：`node` 90%、`direct` 80%、`api` 0.2%、`dns-out` 0%。
//!
//! 语义上取**保守**策略：一条 `sniffed` 最多被一条 `accepted` 认领（配对即消费），
//! 且内部管理入站 [`INTERNAL_INBOUND_TAG`]（`api`，应用自己查统计的回环通道）
//! **不参与配对** —— 它永远不会有域名，参与配对只会被安上假域名、还会把真正
//! 该配对的 `sniffed` 抢走。
//!
//! # 拿不到的字段（硬约束，接口与界面都不得假装有）
//!
//! * **每连接的字节数**：`StatsService` 只有聚合计数器，**没有 per-connection
//!   流量**。单条连接只能显示「时间 / 来源 / 目标 / 域名 / 入站 / 出站」。
//! * **连接的结束时间 / 持续时间**：日志只记建立（`accepted`），不记结束。
//! * **连接 ID**：`accepted` 行不带 ID，所以无法把同一条连接的多行聚成组。
//!
//! # 其它已知边界
//!
//! * 计数是**累计值**，从核心启动开始算；核心重启后归零（与字节计数器同源，
//!   界面处理应当一致）。
//! * 只在 `loglevel` 足够低、且核心真的写出这条访问行时才有数据。
//! * 解析失败的行**静默跳过**：日志格式由上游决定，我们不该因为一行看不懂就
//!   污染统计；但也绝不臆造数字。
//! * `from DNS accepted https://8.8.8.8/dns-query` 这类 **DoH 形态**没有端口，
//!   所以 `target_port = None`（不按 https 默认值编一个 443 出来）。

use std::collections::{HashMap, VecDeque};

/// `sniffed` 与 `accepted` 允许的最大时间差：超过就**不配对**（宁缺勿错）。
///
/// 200ms 的依据：本机实测 p50 = 26µs、p95 = 129µs、max = 109ms（5559 条 accepted），
/// 阈值留了一个数量级的余量；再放大只会把不相干的 `sniffed` 配进来。
pub const PAIR_WINDOW_US: u64 = 200_000;

/// 应用自己查统计用的内部入站 tag（`dokodemo-door`，回环轮询）。
///
/// 这类连接不可能有 `sniffed domain`，参与配对只会产生假域名，并抢走本应
/// 配给真实连接的 `sniffed`。与 `traffic.rs` 的 `API_TAG` 指同一个入站。
const INTERNAL_INBOUND_TAG: &str = "api";

// ---------------------------------------------------------------------------
// 行的解析
// ---------------------------------------------------------------------------

/// 从一行日志里取出 `[入站 -> 出站]` 的两个 tag。
///
/// 从**行尾**往前找最后一对 `[` `]`：目标里可能出现方括号（IPv6 字面量），
/// 从开头找第一个 `[` 会取到地址里的那个。
fn parse_route_tags(line: &str) -> Option<(&str, &str)> {
    let close = line.rfind(']')?;
    let open = line[..close].rfind('[')?;
    let inner = &line[open + 1..close];
    let (inbound, outbound) = inner.split_once(" -> ")?;
    let inbound = inbound.trim();
    let outbound = outbound.trim();
    if inbound.is_empty() || outbound.is_empty() {
        return None;
    }
    Some((inbound, outbound))
}

/// `accepted` 之后的目标形态是否是本模块认得的连接目标。
///
/// `tcp:` / `udp:` 是常规形态；`http(s)://` 是 DoH 模块
/// （`from DNS accepted https://8.8.8.8/dns-query [dns-module -> node-x]`）。
fn is_connection_target(rest: &str) -> bool {
    rest.starts_with("tcp:")
        || rest.starts_with("udp:")
        || rest.starts_with("https://")
        || rest.starts_with("http://")
}

/// 从一行核心日志里取出「出站 tag」。
///
/// 识别依据是 Xray 访问日志的固定形态：`accepted <网络>:<目标> [<入站> -> <出站>]`。
/// 返回 `None` 表示这不是一行可识别的连接行。
pub fn parse_outbound_tag(line: &str) -> Option<&str> {
    // 只认访问行：必须含 ` accepted `，避免把别处的 `[a -> b]` 也算进来。
    // `accepted ` 后面紧跟网络类型，中间不会有别的空白。
    let accepted_at = line.find(" accepted ")?;
    let rest = &line[accepted_at + " accepted ".len()..];
    if !is_connection_target(rest) {
        return None;
    }
    parse_route_tags(line).map(|(_, outbound)| outbound)
}

/// 解析日志行开头的时间戳 `YYYY/MM/DD HH:MM:SS[.ffffff]`。
///
/// 返回 `(原样时间文本, 当日微秒数)`。用「当日微秒数」而不是 Unix 时间：
/// 日志是**本地墙钟且不带时区**，转 Unix 需要时区数据（本项目不引时区库），
/// 而配对只需要同一台机器上的相对时间差。
fn parse_log_time(line: &str) -> Option<(String, u64)> {
    let mut it = line.split(' ');
    let date = it.next()?;
    let time = it.next()?;
    // 至少还要有 `from` / `[Info]` 之类的后续字段，否则不是访问行。
    it.next()?;

    let mut d = date.split('/');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() || y.len() != 4 || mo.len() != 2 || da.len() != 2 {
        return None;
    }
    if !y.chars().chain(mo.chars()).chain(da.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }

    let (hh, rest) = time.split_once(':')?;
    let (mm, rest) = rest.split_once(':')?;
    let (ss, frac) = match rest.split_once('.') {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let hh: u64 = hh.parse().ok()?;
    let mm: u64 = mm.parse().ok()?;
    let ss: u64 = ss.parse().ok()?;
    if hh > 23 || mm > 59 || ss > 59 || frac.len() > 6 {
        return None;
    }
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let micros: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse::<u64>().ok()? * 10u64.pow(6 - frac.len() as u32)
    };

    Some((
        format!("{date} {time}"),
        ((hh * 60 + mm) * 60 + ss) * 1_000_000 + micros,
    ))
}

/// 目标里的 `主机[:端口]`。IPv6 是 `[addr]:port`；主机名直接给（日志里会出现
/// `tcp:github.com:443`，所以字段名不能叫 `target_ip`）。
fn split_host_port(addr: &str) -> (String, Option<u16>) {
    if let Some(rest) = addr.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => {
                let host = &rest[..end];
                let port = rest[end + 1..]
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok());
                (host.to_string(), port)
            }
            None => (addr.to_string(), None),
        };
    }
    match addr.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(p) => (host.to_string(), Some(p)),
            // 端口不是数字 → 整段当主机名，**不猜端口**
            Err(_) => (addr.to_string(), None),
        },
        None => (addr.to_string(), None),
    }
}

/// `accepted` 之后的那一段目标：`tcp:host:port` / `udp:host:port` / `https://host/...`。
fn parse_target(after_accepted: &str) -> Option<(String, String, Option<u16>)> {
    let token = after_accepted.split_whitespace().next()?;
    if let Some(addr) = token
        .strip_prefix("tcp:")
        .or_else(|| token.strip_prefix("udp:"))
    {
        let network = if token.starts_with("tcp:") { "tcp" } else { "udp" };
        let (host, port) = split_host_port(addr);
        if host.is_empty() {
            return None;
        }
        return Some((network.to_string(), host, port));
    }
    for (scheme, network) in [("https://", "https"), ("http://", "http")] {
        if let Some(url) = token.strip_prefix(scheme) {
            let host = url.split('/').next()?;
            if host.is_empty() {
                return None;
            }
            // DoH 形态的日志里没有端口 → `None`，不按「https 默认 443」编一个。
            return Some((network.to_string(), host.to_string(), None));
        }
    }
    None
}

/// `from <来源> accepted ...` 里的来源，去掉可选的 `tcp:` / `udp:` 前缀。
///
/// `from 127.0.0.1:58135`（api 回环）与 `from DNS`（DoH 模块）都没有网络前缀。
fn parse_source(line: &str, accepted_at: usize) -> Option<String> {
    let from_at = line.find(" from ")?;
    if from_at >= accepted_at {
        return None;
    }
    let raw = line[from_at + " from ".len()..accepted_at].trim();
    let raw = raw
        .strip_prefix("tcp:")
        .or_else(|| raw.strip_prefix("udp:"))
        .unwrap_or(raw);
    if raw.is_empty() {
        return None;
    }
    Some(raw.to_string())
}

/// `sniffed domain: <域名>` 行里的域名与连接 ID。
#[derive(Debug, Clone)]
struct SniffedLine {
    domain: String,
    /// `sniffed` 行里的 `[3163266252]`；没有则 `None`。
    id: Option<String>,
    /// 当日微秒数。
    ts_us: u64,
}

/// `sniffed` 行前缀里的连接 ID：`[3163266252] app/dispatcher:` 那个方括号。
fn parse_sniff_id(prefix: &str) -> Option<String> {
    let close = prefix.rfind(']')?;
    let open = prefix[..close].rfind('[')?;
    let inner = &prefix[open + 1..close];
    if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
        Some(inner.to_string())
    } else {
        None
    }
}

fn parse_sniffed(line: &str) -> Option<SniffedLine> {
    const MARKER: &str = "sniffed domain:";
    let at = line.find(MARKER)?;
    let domain = line[at + MARKER.len()..]
        .split_whitespace()
        .next()?
        .to_string();
    if domain.is_empty() {
        return None;
    }
    let (_, ts_us) = parse_log_time(line)?;
    Some(SniffedLine {
        domain,
        id: parse_sniff_id(&line[..at]),
        ts_us,
    })
}

// ---------------------------------------------------------------------------
// 结构化记录
// ---------------------------------------------------------------------------

/// 一条连接 = 访问日志里的一行 `accepted`。
///
/// ⚠️ **没有字节数、没有持续时间、没有连接 ID**（见模块头注释的硬约束）。
/// 域名是**时序配对**得来的近似值，见 [`ConnectionLog`]。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConnectionRecord {
    /// 本进程**收到**该行的 Unix 毫秒（≈连接建立时刻，通常只差个位数毫秒）。
    ///
    /// 不从日志墙钟换算：日志是本地时间且不带时区，换算需要时区库；
    /// 日志原样的时间在 [`ConnectionRecord::ts_text`]。
    pub ts_ms: u64,
    /// 日志里原样的本地墙钟时间，例如 `2026/09/20 13:30:58.560364`。
    pub ts_text: String,
    /// 来源 socket，去掉 `tcp:` / `udp:` 前缀：`198.18.0.1:49712`。
    /// api 回环是 `127.0.0.1:58135`；DoH 行是字面量 `DNS`。
    pub from: String,
    /// `tcp` / `udp`（DoH 形态是 `https`）。
    pub network: String,
    /// 目标主机：**多数是 IP，但日志里也会直接给域名**（如 `github.com`），
    /// 所以不叫 `target_ip`。
    pub target_host: String,
    /// 目标端口；日志没给就是 `None`（**不是 0** —— 0 是合法端口，语义不同）。
    pub target_port: Option<u16>,
    /// `[入站 -> 出站]` 左边。
    pub inbound_tag: String,
    /// 右边 —— 与拓扑出口卡片对应。
    pub outbound_tag: String,
    /// 时序配对到的域名；配不到是 `None`。
    pub domain: Option<String>,
    /// 域名是否来自 `sniffed` 时序配对（近似）。
    ///
    /// 当前实现里恒等于 `domain.is_some()`：`accepted` 行本身不带域名。
    /// 保留这个字段是为了让「这域名是配来的、可能不准」在类型上显式，
    /// 而不是靠口头约定。
    pub domain_paired: bool,
    /// 配对到的那条 `sniffed` 与本行的日志时间差（微秒）；未配对为 `None`。
    ///
    /// 用微秒而不是毫秒：实测配对时延 **p50 = 26µs**，毫秒会四舍五入成 0。
    pub domain_pair_delta_us: Option<u64>,
    /// 配对到的那条 `sniffed` 里的连接 ID；未配对为 `None`。
    pub sniff_id: Option<String>,
}

/// 一行日志的观察结果。
///
/// `record` 为 `None` 的三种情况：`sniffed` 行、解析失败的行、
/// 以及根本不是连接行的行。三种都**不产生连接记录**，与既有行为一致。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObservedLine {
    /// 命中并被计数的出口 tag（`sniffed` 行与无关行都是 `None`）。
    pub outbound: Option<String>,
    /// 结构化连接记录（**配对之后**的状态）。
    pub record: Option<ConnectionRecord>,
}

/// 解析后的一条连接，外加只用于配对的时间。
struct ParsedConnection {
    record: ConnectionRecord,
    /// 当日微秒数（日志时间）；解析不出时间戳时为 `None`。
    ts_us: Option<u64>,
}

/// 把一行日志解析成连接记录；不是连接行则 `None`。
///
/// `received_ms` 由调用方给（本进程收到该行的时刻），让这个函数保持**纯函数**、
/// 可单测。
pub fn parse_connection_line(line: &str, received_ms: u64) -> Option<ConnectionRecord> {
    parse_connection(line, received_ms).map(|p| p.record)
}

fn parse_connection(line: &str, received_ms: u64) -> Option<ParsedConnection> {
    let accepted_at = line.find(" accepted ")?;
    let after = &line[accepted_at + " accepted ".len()..];
    if !is_connection_target(after) {
        return None;
    }
    let (network, target_host, target_port) = parse_target(after)?;
    let (inbound_tag, outbound_tag) = parse_route_tags(line)?;
    let from = parse_source(line, accepted_at)?;
    // 时间戳缺失（畸形行）时不记录：记录需要时间用于配对与展示。
    let (ts_text, ts_us) = parse_log_time(line)?;

    Some(ParsedConnection {
        ts_us: Some(ts_us),
        record: ConnectionRecord {
            ts_ms: received_ms,
            ts_text,
            from,
            network,
            target_host,
            target_port,
            inbound_tag: inbound_tag.to_string(),
            outbound_tag: outbound_tag.to_string(),
            domain: None,
            domain_paired: false,
            domain_pair_delta_us: None,
            sniff_id: None,
        },
    })
}

/// 配对统计：界面用它如实标注「域名是时序配对、可能不准」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PairingStats {
    /// 观察到的 `accepted` 行总数。
    pub accepted: u64,
    /// 配到域名的条数。
    pub paired: u64,
    /// 没配到的条数（恒有 `paired + unpaired == accepted`）。
    pub unpaired: u64,
    /// 观察到的 `sniffed` 行总数。
    pub sniffed: u64,
    /// 有 `sniffed` 候选但时间差超出 [`PAIR_WINDOW_US`]（含乱序）而拒配的次数。
    pub rejected_stale: u64,
    /// 被下一条 `sniffed` 覆盖、最终没配上任何 `accepted` 的 `sniffed` 数。
    pub sniffed_superseded: u64,
}

/// 一次「最近连接」查询的结果。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RecentConnections {
    /// 最近连接，**最新在前**。
    pub items: Vec<ConnectionRecord>,
    /// 环形缓冲建立以来被挤掉的条数（累计）——界面据此如实说「只保留最近 N 条」。
    pub dropped: u64,
    /// 配对统计。
    pub pairing: PairingStats,
}

/// 「最近连接」的过滤条件。空字段表示不过滤。
#[derive(Debug, Clone, Default)]
pub struct ConnectionFilter {
    /// 精确匹配出站 tag。
    pub outbound: Option<String>,
    /// 精确匹配入站 tag。
    pub inbound: Option<String>,
    /// 域名的**不区分大小写子串**匹配；`domain` 为 `None` 的记录不匹配。
    pub domain: Option<String>,
    /// 返回条数上限。
    pub limit: usize,
}

impl ConnectionFilter {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    fn matches(&self, r: &ConnectionRecord) -> bool {
        if let Some(outbound) = &self.outbound {
            if &r.outbound_tag != outbound {
                return false;
            }
        }
        if let Some(inbound) = &self.inbound {
            if &r.inbound_tag != inbound {
                return false;
            }
        }
        if let Some(want) = &self.domain {
            let Some(got) = &r.domain else { return false };
            if !got.to_lowercase().contains(&want.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// 连接日志：计数 + 环形缓冲 + 配对
// ---------------------------------------------------------------------------

/// 累计各出口的连接数，并把最近 [`ConnectionLog::DEFAULT_CAPACITY`] 条连接结构化留存。
///
/// 只在收到新日志行时调用一次，增量更新，不重扫历史。
#[derive(Debug, Clone)]
pub struct ConnectionLog {
    per_outbound: HashMap<String, u64>,
    /// 环形缓冲：新记录 push_back，超出容量 pop_front。
    records: VecDeque<ConnectionRecord>,
    /// 最近一条**尚未被认领**的 `sniffed`。
    pending: Option<SniffedLine>,
    stats: PairingStats,
    capacity: usize,
    dropped: u64,
}

impl Default for ConnectionLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionLog {
    /// 环形缓冲默认容量：界面只需最近若干条，而日志每秒数十条。
    pub const DEFAULT_CAPACITY: usize = 1000;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            per_outbound: HashMap::new(),
            records: VecDeque::new(),
            pending: None,
            stats: PairingStats::default(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    /// 观察一行日志。返回被计数到的出口 tag（没有则 `None`）。
    ///
    /// 返回 `String` 而不是 `&str`：解析结果借用的是**入参** `line`，
    /// 而方法同时可变借用 `self`，两者生命周期无法统一。
    ///
    /// 需要**结构化记录**的调用方用 [`Self::observe_with_record`]。
    pub fn observe(&mut self, line: &str) -> Option<String> {
        self.observe_with_record(line).outbound
    }

    /// 观察一行日志，并把结构化连接记录一并交出来。
    ///
    /// # 为什么加这个方法而不是改 `observe` 的返回值
    ///
    /// `observe` 有两个既有调用点（出口计数、拓扑），它们的语义是"这一行算哪个出口"，
    /// 不需要记录。意图过滤需要记录（域名 / 端口 / 入站）。改签名会把两个调用点
    /// 一起卷进来，而这个 crate 的规矩是**新增入口而不是改既有语义**
    /// （`observe` 现在是本方法的薄封装，两边的计数与配对行为逐字相同）。
    ///
    /// `record` 是**配对之后**的记录：`domain` 可能来自 `sniffed` 时序配对，
    /// 也可能仍是 `None`（实测约一半的 accepted 行配不到域名）。
    pub fn observe_with_record(&mut self, line: &str) -> ObservedLine {
        // `sniffed` 行：只更新待配对的域名，不产生连接记录。
        if let Some(sniff) = parse_sniffed(line) {
            if self.pending.is_some() {
                self.stats.sniffed_superseded += 1;
            }
            self.pending = Some(sniff);
            self.stats.sniffed += 1;
            return ObservedLine::default();
        }

        // 连接行：先计数（与旧行为一致），再尝试记录。
        let Some(tag) = parse_outbound_tag(line).map(str::to_string) else {
            return ObservedLine::default();
        };
        *self.per_outbound.entry(tag.clone()).or_insert(0) += 1;

        let record = parse_connection(line, now_unix_ms()).map(|parsed| self.push(parsed));
        ObservedLine { outbound: Some(tag), record }
    }

    /// 把一条解析好的连接配对、入环形缓冲，并**返回配对后的记录**。
    fn push(&mut self, parsed: ParsedConnection) -> ConnectionRecord {
        let mut record = parsed.record;
        self.stats.accepted += 1;

        if record.inbound_tag == INTERNAL_INBOUND_TAG {
            // 内部回环（应用自己查统计）：不可能有域名，也**不消耗** sniffed。
            self.stats.unpaired += 1;
        } else if let Some(sniff) = self.pending.take() {
            let delta = parsed
                .ts_us
                .and_then(|accepted| accepted.checked_sub(sniff.ts_us));
            match delta {
                Some(d) if d <= PAIR_WINDOW_US => {
                    record.domain = Some(sniff.domain);
                    record.domain_paired = true;
                    record.domain_pair_delta_us = Some(d);
                    record.sniff_id = sniff.id;
                    self.stats.paired += 1;
                }
                _ => {
                    // 超阈值或乱序（accepted 早于 sniffed）：宁缺勿错。
                    self.stats.unpaired += 1;
                    self.stats.rejected_stale += 1;
                }
            }
        } else {
            self.stats.unpaired += 1;
        }

        self.records.push_back(record.clone());
        if self.records.len() > self.capacity {
            self.records.pop_front();
            self.dropped += 1;
        }
        record
    }

    /// 最近连接（最新在前），按 `filter` 过滤并限制条数。
    pub fn recent(&self, filter: &ConnectionFilter) -> RecentConnections {
        RecentConnections {
            items: self
                .records
                .iter()
                .rev()
                .filter(|r| filter.matches(r))
                .take(filter.limit)
                .cloned()
                .collect(),
            dropped: self.dropped,
            pairing: self.stats,
        }
    }

    /// 某个出口的连接数；没记录过就是 `None`（**不是 0**）。
    ///
    /// 区分「没观察到」与「观察到 0 次」很重要：核心没在跑、或日志级别不够时
    /// 是前者，界面该显示「—」而不是 `0`。
    pub fn get(&self, tag: &str) -> Option<u64> {
        self.per_outbound.get(tag).copied()
    }

    /// 是否至少观察到过一条连接行。用来判断这个数据源是否可用。
    pub fn observed_anything(&self) -> bool {
        !self.per_outbound.is_empty()
    }

    /// 全部计数（快照）。
    pub fn snapshot(&self) -> HashMap<String, u64> {
        self.per_outbound.clone()
    }

    /// 清空（核心重启时调用 —— 计数与记录都从零开始）。
    ///
    /// 记录也清掉：它们来自已经不在的那个核心，留着会让界面把上一轮会话的
    /// 连接当成当前会话的。
    pub fn reset(&mut self) {
        self.per_outbound.clear();
        self.records.clear();
        self.pending = None;
        self.stats = PairingStats::default();
        self.dropped = 0;
    }
}

/// 本进程收到日志行的 Unix 毫秒。
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实日志行（取自本机 `logs/app.jsonl`）。这是最常见的一类。
    const REAL_TUN_TO_NODE: &str = "2026/09/20 11:15:13.475426 from tcp:198.18.0.1:58137 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]";

    /// 造一条 accepted 行。
    fn accepted(ts: &str, from: &str, target: &str, route: &str) -> String {
        format!("{ts} from {from} accepted {target} [{route}]")
    }

    /// 造一条 sniffed 行。
    fn sniffed(ts: &str, domain: &str) -> String {
        format!("{ts} [Info] [3163266252] app/dispatcher: sniffed domain: {domain}")
    }

    /// 一次性拿到最近记录（最新在前）。
    fn items(log: &ConnectionLog, limit: usize) -> Vec<ConnectionRecord> {
        log.recent(&ConnectionFilter::new(limit)).items
    }

    fn stats(log: &ConnectionLog) -> PairingStats {
        log.recent(&ConnectionFilter::new(0)).pairing
    }

    // -----------------------------------------------------------------------
    // observe_with_record：与 observe 逐字同源，只是把记录也交出来
    // -----------------------------------------------------------------------

    /// `observe` 必须是 `observe_with_record` 的薄封装 —— 两边的**计数与配对
    /// 行为**要逐字相同，否则「界面看到的连接数」与「意图过滤看到的候选」
    /// 会来自两套不同的配对结果。这条测试直接对照同一条日志序列。
    #[test]
    fn observe_and_observe_with_record_agree_on_counts_and_pairing() {
        let lines: Vec<String> = vec![
            sniffed("2026/09/20 11:15:13.100000", "www.google.com"),
            accepted("2026/09/20 11:15:13.100030", "tcp:198.18.0.1:1", "tcp:1.2.3.4:443", "tun -> node-a"),
            accepted("2026/09/20 11:15:14.000000", "tcp:198.18.0.1:2", "tcp:5.6.7.8:443", "tun -> direct"),
        ];

        let mut a = ConnectionLog::new();
        for l in &lines {
            a.observe(l);
        }
        let mut b = ConnectionLog::new();
        let observed: Vec<ObservedLine> = lines.iter().map(|l| b.observe_with_record(l)).collect();

        assert_eq!(a.snapshot(), b.snapshot(), "出口计数必须一致");
        assert_eq!(stats(&a).paired, stats(&b).paired);
        assert_eq!(stats(&a).unpaired, stats(&b).unpaired);
        assert_eq!(items(&a, 10), items(&b, 10), "连接记录必须逐条相同");

        // 返回值本身：sniffed 行没有出口、也没有记录。
        assert!(observed[0].outbound.is_none() && observed[0].record.is_none());
        // accepted 行有出口，并且记录里带着配对到的域名。
        assert_eq!(observed[1].outbound.as_deref(), Some("node-a"));
        let rec = observed[1].record.as_ref().expect("accepted 行必须给出记录");
        assert_eq!(rec.domain.as_deref(), Some("www.google.com"));
        assert!(rec.domain_paired);
        // 没有前导 sniffed 的那条：有出口、有记录、但**没有域名**（宁缺勿错）。
        assert_eq!(observed[2].outbound.as_deref(), Some("direct"));
        assert_eq!(observed[2].record.as_ref().unwrap().domain, None);
    }

    /// 不是连接行的输入：出口为 `None`、记录为 `None`，且**不改变出口计数**。
    #[test]
    fn observe_with_record_ignores_lines_that_are_not_connections() {
        let mut log = ConnectionLog::new();
        // ① 真的 sniffed 行（带时间戳）—— 只进待配对队列，不是连接；
        // ② 与本次无关的日志行；
        // ③ 空行。
        let noise = [
            sniffed("2026/09/20 11:15:13.100000", "only-sniff.example"),
            "2026/09/20 11:15:13.000000 some unrelated log line".to_string(),
            String::new(),
        ];
        for l in &noise {
            let o = log.observe_with_record(l);
            assert!(o.record.is_none(), "{l:?} 不该产生记录");
            assert!(o.outbound.is_none(), "{l:?} 不该被算到任何出口");
        }
        // sniffed 行进了待配对队列（这是既有语义），但没有 accepted 行来消费它。
        assert_eq!(stats(&log).sniffed, 1);
        assert_eq!(stats(&log).accepted, 0);
        assert!(log.snapshot().is_empty(), "没有连接行 ⇒ 出口计数为空");

        // 一个**看起来像**但缺时间戳的 sniffed 行不会被认（这条断言是上一版
        // 我写错的那一处：我以为 parse_sniffed 不要求前缀，实测要求）。
        let mut bare = ConnectionLog::new();
        assert!(bare.observe_with_record("[Info] app/dispatcher: sniffed domain: x.example").record.is_none());
        assert_eq!(stats(&bare).sniffed, 0);
    }

    // -----------------------------------------------------------------------
    // 单行解析
    // -----------------------------------------------------------------------

    #[test]
    fn parses_real_access_line() {
        assert_eq!(parse_outbound_tag(REAL_TUN_TO_NODE), Some("node-n1d232c6b8c7a5004"));
    }

    /// 常规行要解析出**全部**字段，包括来源、目标、入站/出站。
    #[test]
    fn parses_a_real_accepted_line_into_all_fields() {
        let r = parse_connection_line(REAL_TUN_TO_NODE, 1_700_000_000_123).expect("应当解析成功");
        assert_eq!(r.ts_ms, 1_700_000_000_123);
        assert_eq!(r.ts_text, "2026/09/20 11:15:13.475426");
        assert_eq!(r.from, "198.18.0.1:58137");
        assert_eq!(r.network, "tcp");
        assert_eq!(r.target_host, "194.221.250.50");
        assert_eq!(r.target_port, Some(443));
        assert_eq!(r.inbound_tag, "tun");
        assert_eq!(r.outbound_tag, "node-n1d232c6b8c7a5004");
        assert_eq!(r.domain, None);
        assert!(!r.domain_paired);
        assert_eq!(r.domain_pair_delta_us, None);
        assert_eq!(r.sniff_id, None);
    }

    /// 日志里的目标**有时直接就是域名**（实测 `tcp:github.com:443`、`tcp:cp.cloudflare.com:80`）。
    /// 所以字段名是 `target_host`，不能叫 `target_ip`，也不能把域名当解析失败丢掉。
    #[test]
    fn hostname_target_is_kept_as_a_host() {
        let line = accepted(
            "2026/09/20 13:31:00.000000",
            "tcp:198.18.0.1:5000",
            "tcp:github.com:443",
            "tun -> node-x",
        );
        let r = parse_connection_line(&line, 0).unwrap();
        assert_eq!(r.target_host, "github.com");
        assert_eq!(r.target_port, Some(443));
    }

    /// DoH 形态：`from DNS accepted https://8.8.8.8/dns-query [dns-module -> node-x]`。
    /// 日志里**没有端口** → `target_port = None`（不按 https 默认值编一个 443）。
    #[test]
    fn doh_url_target_has_no_invented_port() {
        let line = "2026/09/20 13:16:35.583455 from DNS accepted https://8.8.8.8/dns-query [dns-module -> node-n1d232c6b8c7a5004]";
        let r = parse_connection_line(line, 0).unwrap();
        assert_eq!(r.from, "DNS");
        assert_eq!(r.network, "https");
        assert_eq!(r.target_host, "8.8.8.8");
        assert_eq!(r.target_port, None, "日志没给端口，不得猜 443");
        assert_eq!(r.inbound_tag, "dns-module");
        // 出站计数也应当认得这类行（旧的 `accepted tcp:/udp:` 判据漏掉了它们）
        assert_eq!(parse_outbound_tag(line), Some("node-n1d232c6b8c7a5004"));
    }

    /// api 回环的来源没有 `tcp:` 前缀：`from 127.0.0.1:58135 ...`。
    #[test]
    fn api_loopback_source_without_network_prefix_still_parses() {
        let line = "2026/09/20 11:15:12.569534 from 127.0.0.1:58135 accepted tcp:127.0.0.1:10085 [api -> api]";
        let r = parse_connection_line(line, 0).unwrap();
        assert_eq!(r.from, "127.0.0.1:58135");
        assert_eq!(r.target_host, "127.0.0.1");
        assert_eq!(r.target_port, Some(10085));
        assert_eq!(r.inbound_tag, "api");
    }

    /// `dns-out` 与 `api` 是本次要解决的两个出口 —— 它们是 UDP / 回环，
    /// 字节计数器恒为 0，只能靠连接数体现活跃度。
    #[test]
    fn parses_the_two_outbounds_whose_byte_counter_is_always_zero() {
        let dns = "2026/09/19 20:09:14.485964 from udp:198.18.0.1:30369 accepted udp:198.18.0.2:53 [tun -> dns-out]";
        assert_eq!(parse_outbound_tag(dns), Some("dns-out"));

        let api = "2026/09/20 11:15:12.569534 from 127.0.0.1:58135 accepted tcp:127.0.0.1:10085 [api -> api]";
        assert_eq!(parse_outbound_tag(api), Some("api"));
    }

    /// IPv6 目标里带方括号，所以必须从行尾往前找最后一对括号。
    /// 若从开头找第一个 `[`，取到的是地址里的那个，出站 tag 会解析错。
    #[test]
    fn ipv6_brackets_in_target_do_not_confuse_the_parser() {
        let line = "2026/09/20 11:15:13.1 from tcp:198.18.0.1:5 accepted tcp:[2606:4700:4700::1111]:443 [tun -> node-x]";
        assert_eq!(parse_outbound_tag(line), Some("node-x"));
        let r = parse_connection_line(line, 0).unwrap();
        assert_eq!(r.target_host, "2606:4700:4700::1111");
        assert_eq!(r.target_port, Some(443));
    }

    /// 不是连接行的日志（启动信息、警告、路由命中）不应被计数。
    #[test]
    fn ignores_lines_that_are_not_connections() {
        for line in [
            "2026/09/19 20:09:14.485942 [Info] [1963124866] app/dispatcher: Hit route rule: [internal-dns-hijack] so taking detour [dns-out] for [udp:198.18.0.2:53]",
            "Xray 26.9.9 (Xray, Penetrates Everything.) Custom (go1.25.0 darwin/arm64)",
            "2026/09/19 20:09:14.1 [Warning] something happened [a -> b]",
            "",
        ] {
            assert_eq!(parse_outbound_tag(line), None, "不该识别：{line}");
            assert!(parse_connection_line(line, 0).is_none(), "不该记录：{line}");
        }
    }

    /// **路由命中行里也有 `[xxx -> yyy]` 形态**，它必须不被计数。
    ///
    /// 这是最容易写错的一处：上述 dispatcher 行含 `[internal-dns-hijack]`
    /// 与 `[dns-out]`，但没有 ` accepted `，所以要求 ` accepted ` 是必要的。
    #[test]
    fn route_hit_lines_are_not_counted_as_connections() {
        let hit = "[Info] app/dispatcher: Hit route rule: [internal-dns-hijack] so taking detour [dns-out]";
        assert_eq!(parse_outbound_tag(hit), None);
    }

    /// 畸形行不得产生假 tag、不得 panic、也不得留下连接记录。
    #[test]
    fn malformed_lines_produce_no_phantom_tags() {
        for line in [
            "accepted tcp:1.2.3.4:443 [tun -> ]",
            "accepted tcp:1.2.3.4:443 [tun]",
            "accepted tcp:1.2.3.4:443",
            " accepted tcp:1.2.3.4:443 [tun -> x]",
            "accepted tcp:1.2.3.4:443 [tun -> x",
        ] {
            let got = parse_outbound_tag(line);
            assert!(
                got.map(|t| !t.is_empty()).unwrap_or(true),
                "不得产出空 tag：{line} → {got:?}"
            );
        }

        // 没有时间戳的 accepted 行不记录（记录需要时间做配对与展示）。
        assert!(parse_connection_line("accepted tcp:1.2.3.4:443 [tun -> x]", 0).is_none());
        // 没有来源的行不记录。
        assert!(parse_connection_line(
            "2026/09/20 13:00:00.000000 accepted tcp:1.2.3.4:443 [tun -> x]",
            0
        )
        .is_none());
    }

    // -----------------------------------------------------------------------
    // 域名配对（近似）—— 本次新增的核心逻辑
    // -----------------------------------------------------------------------

    /// 正常情况：`sniffed` 在前、`accepted` 紧接（实测 p50 = 26µs），配对成功。
    #[test]
    fn sniffed_then_accepted_pairs_the_domain() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.560290", "www.google.com"));
        log.observe(&accepted(
            "2026/09/20 13:30:58.560364",
            "tcp:198.18.0.1:49712",
            "tcp:194.221.250.50:443",
            "tun -> node-x",
        ));

        let r = &items(&log, 1)[0];
        assert_eq!(r.domain.as_deref(), Some("www.google.com"));
        assert!(r.domain_paired);
        assert_eq!(r.domain_pair_delta_us, Some(74), "560364 - 560290 = 74µs");
        assert_eq!(r.sniff_id.as_deref(), Some("3163266252"));
        assert_eq!(stats(&log).paired, 1);
    }

    /// 没有任何 `sniffed` 的连接不得凭空得到域名。
    #[test]
    fn accepted_without_any_sniffed_has_no_domain() {
        let mut log = ConnectionLog::new();
        log.observe(&accepted(
            "2026/09/20 13:30:58.560364",
            "tcp:198.18.0.1:49712",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));
        let r = &items(&log, 1)[0];
        assert_eq!(r.domain, None);
        assert!(!r.domain_paired);
        let s = stats(&log);
        assert_eq!((s.accepted, s.paired, s.unpaired), (1, 0, 1));
    }

    /// 超过配对窗口的 `sniffed` 不得被使用（宁缺勿错），并被记为 stale。
    #[test]
    fn sniffed_outside_the_pair_window_is_rejected() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.000000", "stale.example"));
        // 相隔 500ms > 200ms
        log.observe(&accepted(
            "2026/09/20 13:30:58.500000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));
        let r = &items(&log, 1)[0];
        assert_eq!(r.domain, None, "超窗口的 sniffed 不得被配上");
        let s = stats(&log);
        assert_eq!(s.rejected_stale, 1);
        assert_eq!((s.paired, s.unpaired), (0, 1));
    }

    /// 两条 `sniffed` 竞争同一条 `accepted`：取**最近的前一条**，旧的被覆盖。
    #[test]
    fn nearest_preceding_sniffed_wins_over_an_older_one() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.100000", "old.example"));
        log.observe(&sniffed("2026/09/20 13:30:58.200000", "new.example"));
        log.observe(&accepted(
            "2026/09/20 13:30:58.250000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));
        let r = &items(&log, 1)[0];
        assert_eq!(r.domain.as_deref(), Some("new.example"), "必须取最近的前一条");
        assert_eq!(stats(&log).sniffed_superseded, 1, "被覆盖的 sniffed 要计数");
    }

    /// **乱序**：`accepted` 先到、`sniffed` 后到 —— 不得回头篡改已经记录的那条，
    /// 也不得把后来的 `sniffed` 配给更早的 `accepted`。
    #[test]
    fn out_of_order_accepted_does_not_steal_a_later_sniffed() {
        let mut log = ConnectionLog::new();
        log.observe(&accepted(
            "2026/09/20 13:30:58.100000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));
        log.observe(&sniffed("2026/09/20 13:30:58.200000", "later.example"));
        log.observe(&accepted(
            "2026/09/20 13:30:58.250000",
            "tcp:198.18.0.1:2",
            "tcp:1.2.3.5:443",
            "tun -> node-x",
        ));

        let all = items(&log, 10);
        assert_eq!(all.len(), 2);
        // 最新在前：all[0] 是后到的那条
        assert_eq!(all[0].domain.as_deref(), Some("later.example"));
        assert_eq!(all[1].domain, None, "更早的 accepted 不得被后来的 sniffed 篡改");
    }

    /// 一条 `sniffed` 最多被一条 `accepted` 认领（保守策略：宁可某个连接没域名，
    /// 也不把同一个域名安到多条连接上）。
    #[test]
    fn one_sniffed_pairs_at_most_one_accepted() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.100000", "one.example"));
        log.observe(&accepted(
            "2026/09/20 13:30:58.150000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));
        log.observe(&accepted(
            "2026/09/20 13:30:58.180000",
            "tcp:198.18.0.1:2",
            "tcp:1.2.3.5:443",
            "tun -> node-x",
        ));

        let all = items(&log, 10);
        assert_eq!(all[0].domain, None, "第二条不得复用已被认领的域名");
        assert_eq!(all[1].domain.as_deref(), Some("one.example"));
        let s = stats(&log);
        assert_eq!((s.accepted, s.paired, s.unpaired), (2, 1, 1));
    }

    /// **内部 `api` 入站不参与配对**：它是应用自己查统计的回环通道，
    /// 永远没有域名；若参与，既会被安上假域名，还会把真正该配对的 `sniffed` 抢走。
    #[test]
    fn internal_api_connection_gets_no_domain_and_does_not_consume_a_sniffed() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.100000", "real.example"));
        // 内部回环先到（不该吃掉 sniffed）
        log.observe(&accepted(
            "2026/09/20 13:30:58.120000",
            "127.0.0.1:50000",
            "tcp:127.0.0.1:10085",
            "api -> api",
        ));
        // 真实连接随后到，应当仍然拿到域名
        log.observe(&accepted(
            "2026/09/20 13:30:58.150000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-x",
        ));

        let all = items(&log, 10);
        assert_eq!(all[1].inbound_tag, "api");
        assert_eq!(all[1].domain, None, "内部回环不得有域名");
        assert_eq!(all[0].domain.as_deref(), Some("real.example"), "sniffed 不该被内部连接吃掉");
        assert_eq!(stats(&log).paired, 1);
    }

    /// 不变量：每条 accepted 要么配对要么不配对。
    #[test]
    fn pairing_stats_invariant_holds() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.000000", "a.example"));
        for i in 0..7u32 {
            log.observe(&accepted(
                &format!("2026/09/20 13:30:58.{:06}", i * 30_000),
                "tcp:198.18.0.1:1",
                &format!("tcp:1.2.3.4:{}", 1000 + i),
                "tun -> node-x",
            ));
        }
        let s = stats(&log);
        assert_eq!(s.accepted, 7);
        assert_eq!(s.paired + s.unpaired, s.accepted, "不接受「既没配对也没计数」");
    }

    // -----------------------------------------------------------------------
    // 环形缓冲与过滤
    // -----------------------------------------------------------------------

    #[test]
    fn ring_buffer_keeps_newest_and_counts_evictions() {
        let mut log = ConnectionLog::with_capacity(3);
        for i in 0..5u32 {
            log.observe(&accepted(
                &format!("2026/09/20 13:30:58.{:06}", i * 1_000),
                "tcp:198.18.0.1:1",
                &format!("tcp:1.2.3.4:{}", 1000 + i),
                "tun -> node-x",
            ));
        }
        let recent = log.recent(&ConnectionFilter::new(10));
        assert_eq!(recent.items.len(), 3, "容量 3 只留 3 条");
        assert_eq!(recent.dropped, 2, "被挤掉的要计数（界面据此说明只留最近 N 条）");
        assert_eq!(recent.items[0].target_port, Some(1004), "最新在前");
        assert_eq!(recent.items[2].target_port, Some(1002));
    }

    #[test]
    fn recent_filters_by_outbound_inbound_domain_and_limit() {
        let mut log = ConnectionLog::new();
        log.observe(&sniffed("2026/09/20 13:30:58.000000", "www.google.com"));
        log.observe(&accepted(
            "2026/09/20 13:30:58.010000",
            "tcp:198.18.0.1:1",
            "tcp:1.2.3.4:443",
            "tun -> node-a",
        ));
        log.observe(&accepted(
            "2026/09/20 13:30:58.020000",
            "tcp:198.18.0.1:2",
            "tcp:5.6.7.8:80",
            "socks -> direct",
        ));
        log.observe(&accepted(
            "2026/09/20 13:30:58.030000",
            "127.0.0.1:3",
            "tcp:127.0.0.1:10085",
            "api -> api",
        ));

        let by_out = ConnectionFilter {
            outbound: Some("node-a".into()),
            ..ConnectionFilter::new(10)
        };
        let got = log.recent(&by_out).items;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].inbound_tag, "tun");

        let by_in = ConnectionFilter {
            inbound: Some("socks".into()),
            ..ConnectionFilter::new(10)
        };
        assert_eq!(log.recent(&by_in).items.len(), 1);

        // 域名子串、不区分大小写；没有域名的记录不匹配
        let by_domain = ConnectionFilter {
            domain: Some("GOOGLE".into()),
            ..ConnectionFilter::new(10)
        };
        let got = log.recent(&by_domain).items;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].domain.as_deref(), Some("www.google.com"));

        // limit 生效：最新在前 → 拿到的是最后一条
        let limited = log.recent(&ConnectionFilter::new(1)).items;
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].outbound_tag, "api");
    }

    #[test]
    fn counters_accumulate_per_outbound() {
        let mut c = ConnectionLog::new();
        c.observe(REAL_TUN_TO_NODE);
        c.observe(REAL_TUN_TO_NODE);
        c.observe("2026/09/20 13:00:00.000000 from udp:198.18.0.1:1 accepted udp:198.18.0.2:53 [tun -> dns-out]");
        c.observe("2026/09/20 13:00:01.000000 from 127.0.0.1:1 accepted tcp:127.0.0.1:10085 [api -> api]");

        assert_eq!(c.get("node-n1d232c6b8c7a5004"), Some(2));
        assert_eq!(c.get("dns-out"), Some(1));
        assert_eq!(c.get("api"), Some(1));
        // 没观察过的出口是 `None`，**不是 0** —— 界面据此显示「—」而不是 `0`。
        assert_eq!(c.get("never-seen"), None);
    }

    #[test]
    fn reset_clears_everything_because_the_core_restarted() {
        let mut c = ConnectionLog::new();
        c.observe(&sniffed("2026/09/20 13:30:58.000000", "a.example"));
        c.observe(REAL_TUN_TO_NODE);
        assert!(c.observed_anything());
        assert_eq!(items(&c, 10).len(), 1);

        c.reset();
        assert!(!c.observed_anything());
        assert_eq!(c.get("node-n1d232c6b8c7a5004"), None);
        assert!(items(&c, 10).is_empty(), "上一轮核心的连接记录不得留着");
        assert_eq!(stats(&c), PairingStats::default());
        assert_eq!(c.recent(&ConnectionFilter::new(10)).dropped, 0);
    }

    /// 未识别行不计数，但也不能把已统计的数字弄丢。
    #[test]
    fn unrecognized_lines_leave_existing_counts_intact() {
        let mut c = ConnectionLog::new();
        c.observe(REAL_TUN_TO_NODE);
        assert_eq!(c.observe("just a regular log line"), None);
        assert_eq!(c.get("node-n1d232c6b8c7a5004"), Some(1));
    }

    /// **前端契约的 Rust 侧锁**：序列化出来的**字段名**就是给
    /// `apps/ui/src/types.ts` 的契约，而 TS 那边是手写的、两边没有编译器检查。
    /// 这里把 JSON 键集合钉住：Rust 一改名/删字段，本测试先红，
    /// 提醒同步 `types.ts`（否则界面上那个字段会静默变成 `undefined`）。
    #[test]
    fn serialized_field_names_are_the_frontend_contract() {
        fn keys(v: &serde_json::Value) -> Vec<String> {
            let mut k: Vec<String> = v
                .as_object()
                .expect("序列化结果应当是 JSON 对象")
                .keys()
                .cloned()
                .collect();
            k.sort();
            k
        }

        let record = parse_connection_line(REAL_TUN_TO_NODE, 1).expect("夹具应当能解析");

        // 刻意用**全非零/非空**的值，而不是 `Default`：将来若有人给某个字段加上
        // `skip_serializing_if`（跳过 0 / None），`Default` 构造的键集合会悄悄
        // 缩水，这条锁就会在「字段改了却没红」的方向上静默失效。
        let pairing = PairingStats {
            accepted: 1,
            paired: 1,
            unpaired: 1,
            sniffed: 1,
            rejected_stale: 1,
            sniffed_superseded: 1,
        };
        let recent = RecentConnections {
            items: vec![record.clone()],
            dropped: 1,
            pairing,
        };

        assert_eq!(
            keys(&serde_json::to_value(&record).unwrap()),
            vec![
                "domain",
                "domain_pair_delta_us",
                "domain_paired",
                "from",
                "inbound_tag",
                "network",
                "outbound_tag",
                "sniff_id",
                "target_host",
                "target_port",
                "ts_ms",
                "ts_text",
            ],
            "字段名变了就要同步 apps/ui/src/types.ts 的 ConnectionRecord"
        );

        assert_eq!(
            keys(&serde_json::to_value(pairing).unwrap()),
            vec![
                "accepted",
                "paired",
                "rejected_stale",
                "sniffed",
                "sniffed_superseded",
                "unpaired",
            ],
            "字段名变了就要同步 apps/ui/src/types.ts 的 PairingStats"
        );

        assert_eq!(
            keys(&serde_json::to_value(&recent).unwrap()),
            vec!["dropped", "items", "pairing"],
            "字段名变了就要同步 apps/ui/src/types.ts 的 RecentConnections"
        );
    }

    // -----------------------------------------------------------------------
    // 真实日志（默认不跑）
    // -----------------------------------------------------------------------

    /// 本机真实核心日志（macOS 应用数据目录）。
    ///
    /// 可用 `XRAYTUN_ACCESS_LOG=/path/to/app.jsonl` 指定别的（例如冻结快照），
    /// 让这个报告可复现、也能在别的机器上跑。
    fn real_log_path() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("XRAYTUN_ACCESS_LOG") {
            let p = std::path::PathBuf::from(p);
            return p.exists().then_some(p);
        }
        let home = std::env::var("HOME").ok()?;
        let p = std::path::Path::new(&home)
            .join("Library/Application Support/com.xraytun.desktop/logs/app.jsonl");
        p.exists().then_some(p)
    }

    /// 用**真实日志**量一次配对成功率与各出口的配对率。
    ///
    /// ```bash
    /// cargo test -p xt-core --lib access_log -- --ignored --nocapture
    /// ```
    ///
    /// 它存在的价值：合成样例证明不了「配对在真实日志上是什么成功率」——
    /// 那个数字必须来自真数据，而且要如实报告（约一半连接**本来就没有**
    /// sniffed 行，不是算法漏了）。
    #[test]
    #[ignore = "需要本机真实核心日志"]
    fn real_log_pairing_report() {
        let Some(path) = real_log_path() else {
            eprintln!("找不到真实日志，跳过");
            return;
        };
        let text = std::fs::read_to_string(&path).expect("读日志失败");
        let mut log = ConnectionLog::new();
        let mut json_lines = 0usize;
        let mut per_outbound: HashMap<String, (u64, u64)> = HashMap::new();
        let mut deltas: Vec<u64> = Vec::new();

        for raw in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
                continue;
            };
            let Some(msg) = v.get("message").and_then(|m| m.as_str()) else {
                continue;
            };
            json_lines += 1;
            let before = stats(&log).accepted;
            log.observe(msg);
            // 这一行产生了新记录吗？
            if stats(&log).accepted > before {
                let r = &items(&log, 1)[0];
                let slot = per_outbound.entry(r.outbound_tag.clone()).or_insert((0, 0));
                slot.1 += 1;
                if r.domain.is_some() {
                    slot.0 += 1;
                }
                if let Some(d) = r.domain_pair_delta_us {
                    deltas.push(d);
                }
            }
        }

        let s = stats(&log);
        let recent = log.recent(&ConnectionFilter::new(10));
        println!("日志 = {}", path.display());
        println!("JSON 行 = {json_lines}");
        println!("accepted = {}  paired = {} ({:.1}%)  unpaired = {}",
                 s.accepted, s.paired, s.paired as f64 / s.accepted.max(1) as f64 * 100.0, s.unpaired);
        println!("sniffed = {}  超窗口拒配 = {}  被覆盖 = {}",
                 s.sniffed, s.rejected_stale, s.sniffed_superseded);
        println!("环形缓冲 dropped = {}（容量 {}）", recent.dropped, ConnectionLog::DEFAULT_CAPACITY);
        let mut rates: Vec<_> = per_outbound.iter().collect();
        rates.sort_by_key(|(_, (_, total))| std::cmp::Reverse(*total));
        for (tag, (paired, total)) in rates {
            println!("  出口 {tag:32} paired {paired:5}/{total:5} = {:5.1}%",
                     *paired as f64 / (*total).max(1) as f64 * 100.0);
        }
        if !deltas.is_empty() {
            deltas.sort_unstable();
            println!("配对时延 µs: min={} p50={} p95={} max={}",
                     deltas[0],
                     deltas[deltas.len() / 2],
                     deltas[deltas.len() * 95 / 100],
                     deltas[deltas.len() - 1]);
        }

        // 只断言与机器无关的结构性事实：真实日志上确实配到了，
        // 且统计自洽。具体百分比打印出来人工核对，不写成断言（日志会变）。
        assert_eq!(s.paired + s.unpaired, s.accepted);
        assert!(s.accepted > 0, "真实日志里应当有 accepted 行");
        assert!(s.paired > 0, "真实日志里应当至少配到一部分域名");
        assert_eq!(recent.dropped, s.accepted.saturating_sub(ConnectionLog::DEFAULT_CAPACITY as u64));
        // 内部 api 回环永远不参与配对（实测 0%）。
        assert_eq!(per_outbound.get("api").map(|(p, _)| *p), Some(0));
    }
}
