//! 从 Xray 的 `StatsService` 读取流量计数器。
//!
//! # 为什么是这个方案
//!
//! 「显示网速」看起来是个纯 UI 需求，实际上**没有任何现成的数据**：
//! 配置里的 `traffic` 字段一直是默认值 0，面板上的速率从来就不动。
//! 要拿到真实数字，只有几条路：
//!
//! | 方案 | 问题 |
//! |---|---|
//! | 读物理网卡计数器 | 系统代理模式下和无关流量混在一起，分不出来 |
//! | 读 utun 计数器 | 只对 TUN 模式有效，系统代理模式没有 utun |
//! | 自己数包 | TUN 的 fd 已经交给核心了，我们看不见 |
//! | **问核心要** | ✅ 两种模式都对，且能区分上下行 |
//!
//! 所以这里实现的是最后一条：Xray 的 `StatsService`。它在生成配置时
//! 就已经开好了（`"stats": {}` + `api` 入站 + `policy` 里的各项计数器），
//! 我们只是去把它读出来。
//!
//! # 为什么手写 protobuf
//!
//! Xray 的 api 入站说的是 gRPC。要完整支持 gRPC 需要 tonic + prost +
//! 一套 `.proto` 代码生成，为了一个一元 RPC 太重。而这个 RPC 的报文
//! 结构极简单，手写编解码比引入整条工具链更可控（也更好写测试）：
//!
//! ```text
//! QueryStatsRequest  { string pattern = 1; bool reset = 2; }
//! QueryStatsResponse { repeated Stat stat = 1; }
//! Stat               { string name = 1; int64 value = 2; }
//! ```
//!
//! 唯一的传输层依赖是 `h2`（明文 h2c，不需要 TLS）。
//!
//! # 上下行方向（实测确定，不是猜的）
//!
//! 计数器名字形如 `inbound>>>{tag}>>>traffic>>>{uplink|downlink}`。
//! 光看名字判断哪个是「用户的下载」很容易搞反，所以做了一次实测：
//! 通过隧道下载一个 20,000,000 字节的文件，前后对比计数器：
//!
//! ```text
//! inbound>>>tun>>>traffic>>>downlink   +20,089,105   ← 用户的下载
//! inbound>>>tun>>>traffic>>>uplink        +108,420   ← 只有 ACK
//! ```
//!
//! 结论：**入站的 `downlink` 是用户的下载（rx），`uplink` 是上传（tx）**。
//! 这个对应关系由 [`traffic_from_stats`] 的测试钉住。

use std::net::SocketAddr;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use bytes::Bytes;

use crate::error::{Error, Result};

/// `StatsService` 的全限定方法名。
pub const QUERY_STATS_PATH: &str = "/xray.app.stats.command.StatsService/QueryStats";

/// 单个计数器的名字（`inbound>>>tun>>>traffic>>>downlink`）与值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatEntry {
    pub name: String,
    pub value: i64,
}

/// 一次采样的累计字节数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrafficCounters {
    /// 用户的下载量（入站 downlink 之和）。
    pub rx_bytes: u64,
    /// 用户的上传量（入站 uplink 之和）。
    pub tx_bytes: u64,
}

// ---------------------------------------------------------------------------
// protobuf：只实现用到的那两条消息
// ---------------------------------------------------------------------------

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            return;
        }
    }
}

/// 写一个 length-delimited 字段（wire type 2）。
fn put_bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
    put_varint(u64::from(field) << 3 | 2, out);
    put_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

/// 读 varint，返回 (值, 新下标)。
fn read_varint(buf: &[u8], mut i: usize) -> Result<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let Some(&byte) = buf.get(i) else {
            return Err(Error::Stats("protobuf 数据在 varint 中途截断".into()));
        };
        i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, i));
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::Stats("protobuf varint 超过 64 位".into()));
        }
    }
}

/// 依次读出一个消息的所有字段。
fn read_fields(buf: &[u8]) -> Result<Vec<(u32, FieldValue)>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let (key, next) = read_varint(buf, i)?;
        i = next;
        let field = (key >> 3) as u32;
        let wire = (key & 0x7) as u8;
        match wire {
            0 => {
                let (v, next) = read_varint(buf, i)?;
                i = next;
                out.push((field, FieldValue::Varint(v)));
            }
            2 => {
                let (len, next) = read_varint(buf, i)?;
                i = next;
                let len = len as usize;
                let end = i
                    .checked_add(len)
                    .filter(|e| *e <= buf.len())
                    .ok_or_else(|| Error::Stats("protobuf 字段长度越界".into()))?;
                out.push((field, FieldValue::Bytes(buf[i..end].to_vec())));
                i = end;
            }
            // 我们只发/收上面两种 wire type；遇到别的说明解析错位了。
            other => {
                return Err(Error::Stats(format!("protobuf wire type {other} 不受支持")));
            }
        }
    }
    Ok(out)
}

enum FieldValue {
    Varint(u64),
    Bytes(Vec<u8>),
}

/// 构造 `QueryStatsRequest`。`pattern` 为空表示「全部计数器」。
pub fn encode_query_stats_request(pattern: &str, reset: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if !pattern.is_empty() {
        put_bytes(1, pattern.as_bytes(), &mut out);
    }
    if reset {
        put_varint(2 << 3, &mut out);
        put_varint(1, &mut out);
    }
    out
}

/// 解析 `QueryStatsResponse`。
pub fn decode_query_stats_response(buf: &[u8]) -> Result<Vec<StatEntry>> {
    let mut stats = Vec::new();
    for (field, value) in read_fields(buf)? {
        if field != 1 {
            continue; // 未知字段按 protobuf 约定跳过
        }
        let FieldValue::Bytes(stat) = value else {
            return Err(Error::Stats("Stat 字段的 wire type 不是 length-delimited".into()));
        };
        let mut name = String::new();
        let mut val = 0i64;
        for (f, v) in read_fields(&stat)? {
            match (f, v) {
                (1, FieldValue::Bytes(b)) => {
                    name = String::from_utf8_lossy(&b).into_owned();
                }
                (2, FieldValue::Varint(n)) => val = n as i64,
                _ => {}
            }
        }
        if !name.is_empty() {
            stats.push(StatEntry { name, value: val });
        }
    }
    Ok(stats)
}

// ---------------------------------------------------------------------------
// gRPC 分帧
// ---------------------------------------------------------------------------

/// gRPC 消息帧：1 字节压缩标志 + 4 字节大端长度 + 消息体。
fn encode_grpc_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(0); // 不压缩
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 从响应体里取出第一条消息。多余的数据（如果有）忽略。
fn decode_grpc_frame(buf: &[u8]) -> Result<&[u8]> {
    if buf.len() < 5 {
        return Err(Error::Stats(format!("gRPC 响应只有 {} 字节，读不出帧头", buf.len())));
    }
    if buf[0] != 0 {
        return Err(Error::Stats("gRPC 响应使用了压缩，本实现不支持".into()));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let end = 5usize
        .checked_add(len)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| Error::Stats(format!("gRPC 帧声明 {len} 字节，实际只有 {}", buf.len() - 5)))?;
    Ok(&buf[5..end])
}

// ---------------------------------------------------------------------------
// 语义：计数器 → 上下行字节数
// ---------------------------------------------------------------------------

/// 把计数器列表折成 rx/tx。
///
/// `ignore_inbound` 通常传 `"api"` —— 那是我们自己轮询用的入站，
/// 把它算进去会让「网速」在空闲时也一直有几十字节的读数。
///
/// 入站 `downlink` = 用户的下载（rx），`uplink` = 上传（tx），
/// 这个方向是实测确定的，见模块头注释。
/// 计数器名里解析出来的一个分量。
///
/// 名字形如 `inbound>>>tun>>>traffic>>>uplink`：
/// `direction` 是 `inbound` / `outbound`，`tag` 是入口或出口的名字，
/// `traffic` 固定不变，`flow` 是 `uplink` / `downlink`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterParts {
    pub direction: String,
    pub tag: String,
    pub flow: String,
}

/// 解析一个流量计数器的名字。
///
/// **只在这里实现一次**：这个格式在三个地方要用（整体流量、按入口分车道、
/// 按出口取最大）。各写一份手切分的话，格式一变就会有一处漏改，
/// 而漏改的表现是「数字悄悄变 0」——不会报错。
pub fn parse_traffic_counter(name: &str) -> Option<CounterParts> {
    let mut parts = name.split(">>>");
    let (Some(direction), Some(tag), Some("traffic"), Some(flow), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if direction != "inbound" && direction != "outbound" {
        return None;
    }
    // 空 tag（例如 `inbound>>>>>traffic>>>uplink`）不是合法计数器：
    // 放过去会凭空多出一个名字为空的条目，把所有「名字被切坏」的计数
    // 都算到它头上 —— 数字对不上，却不会报错。
    if tag.is_empty() {
        return None;
    }
    Some(CounterParts {
        direction: direction.to_string(),
        tag: tag.to_string(),
        flow: flow.to_string(),
    })
}

/// 把一批「计数器名 → 值」按方向折成 `tag -> (uplink, downlink)`。
///
/// **flow 名不认识时必须整条丢弃**，不能先建 entry 再忽略：那样
/// `inbound>>>tun>>>traffic>>>sideways` 会凭空造出一个值为 0 的 `tun`，
/// 让「这个名字没被识别」看起来像「这个入口流量是 0」。
fn aggregate_by_tag<'a, I>(entries: I, direction: &str) -> HashMap<String, (u64, u64)>
where
    I: IntoIterator<Item = (&'a str, u64)>,
{
    let mut out: HashMap<String, (u64, u64)> = HashMap::new();
    for (name, value) in entries {
        let Some(parts) = parse_traffic_counter(name) else {
            continue;
        };
        if parts.direction != direction {
            continue;
        }
        let is_uplink = match parts.flow.as_str() {
            "uplink" => true,
            "downlink" => false,
            _ => continue,
        };
        let entry = out.entry(parts.tag).or_insert((0, 0));
        if is_uplink {
            entry.0 = entry.0.saturating_add(value);
        } else {
            entry.1 = entry.1.saturating_add(value);
        }
    }
    out
}

/// 按 tag 汇总某个方向的上下行字节。
///
/// 返回 `tag -> (uplink, downlink)`。
pub fn traffic_by_tag(stats: &[StatEntry], direction: &str) -> HashMap<String, (u64, u64)> {
    aggregate_by_tag(
        stats.iter().map(|s| (s.name.as_str(), s.value.max(0) as u64)),
        direction,
    )
}

/// 全体入站的 rx/tx 合计。
///
/// 与 [`traffic_by_tag`] 共用 [`parse_traffic_counter`] —— 这个格式**只解析
/// 一次**：各写一份手切分的话，格式一变就会有一处漏改，而漏改的表现是
/// 「数字悄悄变 0」，不会报错。
pub fn traffic_from_stats(stats: &[StatEntry], ignore_inbound: &str) -> TrafficCounters {
    let mut total = TrafficCounters::default();
    for s in stats {
        let Some(parts) = parse_traffic_counter(&s.name) else {
            continue;
        };
        if parts.direction != "inbound" || parts.tag == ignore_inbound {
            continue;
        }
        let value = s.value.max(0) as u64;
        match parts.flow.as_str() {
            "downlink" => total.rx_bytes = total.rx_bytes.saturating_add(value),
            "uplink" => total.tx_bytes = total.tx_bytes.saturating_add(value),
            _ => {}
        }
    }
    total
}

// ---------------------------------------------------------------------------
// 跨核心重启的单调化
// ---------------------------------------------------------------------------

/// 单个累计计数器的跨重启单调化。
///
/// Xray 的计数器是**累计值**，核心重启（换网 / 熄屏唤醒 / 节点抖动时看门狗
/// 的 `stop_core` + `start_core`）会让它归零。直接把原始值给界面，用户看到的
/// 就是「8 GiB → 0 → 再涨」—— 那正是被当成「数据乱跳」的现象。
///
/// 这里把归零前的值累进 `base`，返回值 = `base + 新原始值`：读数不回退，
/// 而「归零」这件事本身用 [`MonotonicCounter::resets`] 如实上报，
/// 不做平滑掩盖。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MonotonicCounter {
    /// 历次归零前累计下来的量。
    base: u64,
    /// 上一次看到的原始值。
    last_raw: u64,
    /// 观察到归零的次数。
    resets: u32,
}

impl MonotonicCounter {
    /// 观察一个原始值，返回单调化后的累计值。
    pub fn observe(&mut self, raw: u64) -> u64 {
        if raw < self.last_raw {
            // 原始值回退 = 计数器被重置（核心重启）。把上一段累计量接上，
            // 而不是把读数掉回去。
            self.base = self.base.saturating_add(self.last_raw);
            self.resets = self.resets.saturating_add(1);
        }
        self.last_raw = raw;
        self.base.saturating_add(raw)
    }

    /// 观察到的归零次数。
    pub fn resets(&self) -> u32 {
        self.resets
    }
}

/// 一组计数器的单调化状态（`计数器全名 -> 状态`）。
///
/// 用 `BTreeMap` 而不是 `HashMap`：遍历顺序确定，不会因为哈希顺序不同
/// 让两次采样看起来像变了。
#[derive(Debug, Clone, Default)]
pub struct MonotonicCounters {
    inner: BTreeMap<String, MonotonicCounter>,
}

impl MonotonicCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// 观察一个计数器名对应的原始值，返回单调化后的值。
    pub fn observe(&mut self, name: &str, raw: u64) -> u64 {
        self.inner.entry(name.to_string()).or_default().observe(raw)
    }

    /// 观察一整批原始值（`计数器全名 -> 原始累计值`），返回同形状的单调值。
    pub fn observe_all(&mut self, raw: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
        raw.iter()
            .map(|(name, value)| (name.clone(), self.observe(name, *value)))
            .collect()
    }

    /// 到目前为止观察到的最多归零次数。`>0` 表示核心重启过。
    pub fn max_resets(&self) -> u32 {
        self.inner
            .values()
            .map(MonotonicCounter::resets)
            .max()
            .unwrap_or(0)
    }
}

/// 一次采样里每个流量计数器的原始累计值（`计数器全名 -> 字节数`）。
///
/// 只保留 [`parse_traffic_counter`] 认得的名字：畸形名字不能混进来，
/// 否则它会被当成真实历史参与 base 计算，把别的计数器的值带偏。
pub fn counter_values(stats: &[StatEntry]) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for s in stats {
        if parse_traffic_counter(&s.name).is_none() {
            continue;
        }
        let value = s.value.max(0) as u64;
        let slot = out.entry(s.name.clone()).or_insert(0);
        *slot = slot.saturating_add(value);
    }
    out
}

/// 按 tag 汇总某个方向的**跨核心重启单调**累计字节。
///
/// 返回 `(tag -> (uplink, downlink), 观察到的归零次数)`。
pub fn monotonic_traffic_by_tag(
    counters: &mut MonotonicCounters,
    stats: &[StatEntry],
    direction: &str,
) -> (HashMap<String, (u64, u64)>, u32) {
    let smoothed = counters.observe_all(&counter_values(stats));
    (
        aggregate_by_tag(
            smoothed.iter().map(|(name, v)| (name.as_str(), *v)),
            direction,
        ),
        counters.max_resets(),
    )
}

// ---------------------------------------------------------------------------
// 传输
// ---------------------------------------------------------------------------

/// 向核心的 api 入站发一次 `QueryStats`。
///
/// 每次调用新建一条连接。一秒一次的轮询下这不值得复用连接，而且核心
/// 重启后旧连接会变成哑连接 —— 那个坑我们在 helper 的 unix socket 上
/// 已经踩过一次了（见 docs/06 §6.5）。
pub async fn query_stats(addr: SocketAddr, timeout: Duration) -> Result<Vec<StatEntry>> {
    match tokio::time::timeout(timeout, query_stats_inner(addr)).await {
        Ok(r) => r,
        Err(_) => Err(Error::Stats(format!("查询统计超时（{timeout:?}）"))),
    }
}

async fn query_stats_inner(addr: SocketAddr) -> Result<Vec<StatEntry>> {
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| Error::Stats(format!("连接 api 入站 {addr} 失败: {e}")))?;
    // 统计是小报文，禁用 Nagle 让往返更干脆。
    let _ = stream.set_nodelay(true);

    let (mut sender, connection) = h2::client::handshake(stream)
        .await
        .map_err(|e| Error::Stats(format!("HTTP/2 握手失败: {e}")))?;
    tokio::spawn(async move {
        // 连接对象必须被驱动，否则请求永远不会完成。
        if let Err(e) = connection.await {
            tracing::debug!(error = %e, "统计连接的 h2 会话结束");
        }
    });

    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}{QUERY_STATS_PATH}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .map_err(|e| Error::Stats(format!("构造请求失败: {e}")))?;

    let (response, mut body_tx) = sender
        .send_request(request, false)
        .map_err(|e| Error::Stats(format!("发送请求失败: {e}")))?;

    let frame = encode_grpc_frame(&encode_query_stats_request("", false));
    body_tx
        .send_data(Bytes::from(frame), true)
        .map_err(|e| Error::Stats(format!("发送请求体失败: {e}")))?;

    let response = response
        .await
        .map_err(|e| Error::Stats(format!("等待响应失败: {e}")))?;
    if response.status() != http::StatusCode::OK {
        return Err(Error::Stats(format!("api 返回 HTTP {}", response.status())));
    }

    let mut body = response.into_body();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| Error::Stats(format!("读取响应体失败: {e}")))?;
        buf.extend_from_slice(&chunk);
    }

    // gRPC 会把错误状态放在 trailers 里。没读 trailers 时，至少用
    // 「帧能不能解析」兜底 —— 出错时帧通常为空，解析会明确报错。
    decode_query_stats_response(decode_grpc_frame(&buf)?)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_request_is_empty_for_all_pattern() {
        assert!(encode_query_stats_request("", false).is_empty());
    }

    #[test]
    fn query_request_encodes_pattern_and_reset() {
        // field 1, wire 2, len 3, "abc"
        assert_eq!(
            encode_query_stats_request("abc", false),
            vec![0x0a, 0x03, b'a', b'b', b'c']
        );
        // reset=true 追加 field 2 = 1
        assert_eq!(
            encode_query_stats_request("abc", true),
            vec![0x0a, 0x03, b'a', b'b', b'c', 0x10, 0x01]
        );
    }

    /// 用**真实核心的响应**做夹具。
    ///
    /// 这 30 个字节是从跑着的 Xray 上抓下来的原始响应体开头：
    /// `Stat{name:"outbound>>>api>>>traffic>>>uplink"}`（value=0 被省略）。
    #[test]
    fn decodes_a_real_response_prefix() {
        let raw: Vec<u8> = vec![
            0x0a, 0x23, 0x0a, 0x21, b'o', b'u', b't', b'b', b'o', b'u', b'n', b'd', b'>', b'>',
            b'>', b'a', b'p', b'i', b'>', b'>', b'>', b't', b'r', b'a', b'f', b'f', b'i', b'c',
            b'>', b'>', b'>', b'u', b'p', b'l', b'i', b'n', b'k',
        ];
        let stats = decode_query_stats_response(&raw).unwrap();
        assert_eq!(
            stats,
            vec![StatEntry {
                name: "outbound>>>api>>>traffic>>>uplink".into(),
                value: 0,
            }]
        );
    }

    /// 计数器名的解析：**只此一处**，三处调用共用。
    #[test]
    fn parses_counter_names() {
        let p = parse_traffic_counter("inbound>>>tun>>>traffic>>>downlink").unwrap();
        assert_eq!(p.direction, "inbound");
        assert_eq!(p.tag, "tun");
        assert_eq!(p.flow, "downlink");

        let p = parse_traffic_counter("outbound>>>direct>>>traffic>>>uplink").unwrap();
        assert_eq!(p.direction, "outbound");
        assert_eq!(p.tag, "direct");
    }

    /// 名字不对时必须**拒绝**，而不是解析出一个半成品 ——
    /// 半成品会让某个 tag 悄悄变成别的名字，数字对不上还查不出来。
    #[test]
    fn rejects_malformed_counter_names() {
        for bad in [
            "",
            "inbound>>>tun>>>traffic",              // 少了方向
            "inbound>>>tun>>>traffic>>>uplink>>>x", // 多了字段
            "user>>>a@b>>>traffic>>>uplink",        // 不是 inbound/outbound
            "inbound>>>tun>>>nottraffic>>>uplink",  // 中间不是 traffic
            "inbound>>>tun>>>traffic>>>sideways",   // 方向名不对但结构合法（保留判断在调用方）
        ] {
            let got = parse_traffic_counter(bad);
            if bad.ends_with("sideways") {
                // 结构对、flow 名不认识：解析通过，但不会被计入任何一项
                assert!(got.is_some(), "{bad} 结构合法应当解析成功");
            } else {
                assert!(got.is_none(), "{bad} 应当被拒绝");
            }
        }
    }

    /// 按 tag 汇总：同一 tag 的上下行要分别累加，不同 direction 不能混。
    #[test]
    fn aggregates_per_tag_and_keeps_directions_apart() {
        let stats = vec![
            StatEntry { name: "inbound>>>tun>>>traffic>>>uplink".into(), value: 10 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 100 },
            StatEntry { name: "inbound>>>socks>>>traffic>>>uplink".into(), value: 3 },
            StatEntry { name: "outbound>>>direct>>>traffic>>>uplink".into(), value: 7 },
            // 负值（理论上不该出现）按 0 处理，避免把总量减出一个负数
            StatEntry { name: "inbound>>>tun>>>traffic>>>uplink".into(), value: -5 },
        ];
        let inbound = traffic_by_tag(&stats, "inbound");
        assert_eq!(inbound.get("tun"), Some(&(10, 100)));
        assert_eq!(inbound.get("socks"), Some(&(3, 0)));
        // 出口的计数不会混进入口
        assert!(!inbound.contains_key("direct"));

        let outbound = traffic_by_tag(&stats, "outbound");
        assert_eq!(outbound.get("direct"), Some(&(7, 0)));
    }

    /// 真实响应里 value 是 54 的那条：`inbound>>>api>>>traffic>>>downlink`。
    #[test]
    fn decodes_a_nonzero_value() {
        let raw: Vec<u8> = vec![
            0x0a, 0x26, 0x0a, 0x22, b'i', b'n', b'b', b'o', b'u', b'n', b'd', b'>', b'>', b'>',
            b'a', b'p', b'i', b'>', b'>', b'>', b't', b'r', b'a', b'f', b'f', b'i', b'c', b'>',
            b'>', b'>', b'd', b'o', b'w', b'n', b'l', b'i', b'n', b'k', 0x10, 0x36,
        ];
        let stats = decode_query_stats_response(&raw).unwrap();
        assert_eq!(stats[0].name, "inbound>>>api>>>traffic>>>downlink");
        assert_eq!(stats[0].value, 54);
    }

    #[test]
    fn grpc_frame_roundtrip() {
        let payload = b"hello";
        let framed = encode_grpc_frame(payload);
        assert_eq!(&framed[..5], &[0, 0, 0, 0, 5]);
        assert_eq!(decode_grpc_frame(&framed).unwrap(), payload);
    }

    #[test]
    fn grpc_frame_rejects_truncated_body() {
        // 声明 5 字节但只给 2 字节
        let bad = vec![0u8, 0, 0, 0, 5, 1, 2];
        assert!(decode_grpc_frame(&bad).is_err());
        assert!(decode_grpc_frame(&[0u8, 0, 0]).is_err());
    }

    #[test]
    fn grpc_frame_rejects_compressed() {
        let bad = vec![1u8, 0, 0, 0, 0];
        assert!(decode_grpc_frame(&bad).is_err());
    }

    /// 方向映射：入站 downlink = 用户下载。这条是实测结论的固化。
    #[test]
    fn downlink_is_rx_and_uplink_is_tx() {
        let stats = vec![
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 20_089_105 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>uplink".into(), value: 108_420 },
        ];
        let t = traffic_from_stats(&stats, "api");
        assert_eq!(t.rx_bytes, 20_089_105);
        assert_eq!(t.tx_bytes, 108_420);
    }

    #[test]
    fn sums_all_inbounds_except_api() {
        let stats = vec![
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 100 },
            StatEntry { name: "inbound>>>socks>>>traffic>>>downlink".into(), value: 20 },
            StatEntry { name: "inbound>>>http>>>traffic>>>downlink".into(), value: 3 },
            // 我们自己的轮询：不能算进网速，否则空闲时也有读数
            StatEntry { name: "inbound>>>api>>>traffic>>>downlink".into(), value: 999 },
            StatEntry { name: "inbound>>>api>>>traffic>>>uplink".into(), value: 999 },
        ];
        let t = traffic_from_stats(&stats, "api");
        assert_eq!(t.rx_bytes, 123);
        assert_eq!(t.tx_bytes, 0);
    }

    #[test]
    fn ignores_outbound_and_malformed_names() {
        let stats = vec![
            StatEntry { name: "outbound>>>direct>>>traffic>>>downlink".into(), value: 500 },
            StatEntry { name: "inbound>>>tun>>>traffic".into(), value: 500 },
            StatEntry { name: "inbound>>>tun".into(), value: 500 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>sideways".into(), value: 500 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink>>>extra".into(), value: 500 },
            StatEntry { name: "".into(), value: 500 },
        ];
        assert_eq!(traffic_from_stats(&stats, "api"), TrafficCounters::default());
    }

    #[test]
    fn negative_values_are_clamped() {
        // 计数器理论上只会增长，但真读回负数时不能让 u64 回绕成天文数字。
        let stats = vec![StatEntry {
            name: "inbound>>>tun>>>traffic>>>downlink".into(),
            value: -1,
        }];
        assert_eq!(traffic_from_stats(&stats, "api").rx_bytes, 0);
    }

    #[test]
    fn varint_roundtrip_over_boundaries() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(v, &mut buf);
            assert_eq!(read_varint(&buf, 0).unwrap(), (v, buf.len()), "值 {v}");
        }
    }

    /// 空 tag 不是合法计数器：放过去会凭空多出一个名字为空的条目，
    /// 把所有「名字被切坏」的计数都算到它头上 —— 数字对不上却不报错。
    #[test]
    fn rejects_counter_names_with_empty_tag() {
        assert!(parse_traffic_counter("inbound>>>>>traffic>>>uplink").is_none());
        assert!(parse_traffic_counter("outbound>>>>>traffic>>>downlink").is_none());
        // 正常名字当然要认，别把保护做成误伤
        assert!(parse_traffic_counter("inbound>>>tun>>>traffic>>>uplink").is_some());
    }

    /// flow 名不认识时必须整条丢弃。先建 entry 再忽略的话，会造出一个
    /// 值为 0 的 tag ——「这个名字没被识别」看起来就像「这个入口流量是 0」。
    #[test]
    fn unknown_flow_does_not_create_a_phantom_tag() {
        let stats = vec![StatEntry {
            name: "inbound>>>tun>>>traffic>>>sideways".into(),
            value: 500,
        }];
        assert!(traffic_by_tag(&stats, "inbound").is_empty());
        let (by_tag, _) = monotonic_traffic_by_tag(&mut MonotonicCounters::new(), &stats, "inbound");
        assert!(by_tag.is_empty(), "未知 flow 不得造出 0 值条目");
    }

    /// **本次要修的核心回归**：核心重启让计数器归零，累计读数不得回退。
    #[test]
    fn monotonic_counter_does_not_go_backwards_across_a_reset() {
        let mut c = MonotonicCounter::default();
        assert_eq!(c.observe(8_600_000_000), 8_600_000_000);
        // 核心重启：计数器从 0 重新计
        assert_eq!(c.observe(0), 8_600_000_000, "归零不得让读数掉回去");
        assert_eq!(c.resets(), 1);
        // 重启之后的新流量继续累加在续接值上
        assert_eq!(c.observe(4_096), 8_600_004_096);
        assert_eq!(c.resets(), 1);
    }

    /// 连续归零两次：每一次都要把「该段重启前的量」累进基数。
    /// 少加一次就丢一段真实流量，多加一次就凭空涨 —— 两种都是数字悄悄错。
    #[test]
    fn monotonic_counter_accumulates_each_session_before_a_reset() {
        let mut c = MonotonicCounter::default();
        assert_eq!(c.observe(1_000), 1_000);
        assert_eq!(c.observe(0), 1_000); // 第一次重启：base += 1000
        assert_eq!(c.observe(500), 1_500);
        assert_eq!(c.observe(3), 1_503); // 第二次重启：base += 500
        assert_eq!(c.resets(), 2);
        assert_eq!(c.observe(10), 1_510);
    }

    /// base 必须按计数器分开记：某个出口归零补偿不能凭空加到别的出口上。
    #[test]
    fn monotonic_counters_keep_tags_independent() {
        let mut counters = MonotonicCounters::new();
        let first = counter_values(&[
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 5_000 },
            StatEntry { name: "outbound>>>node-b>>>traffic>>>downlink".into(), value: 100 },
        ]);
        counters.observe_all(&first);

        // 只有 node-a 归零，node-b 继续正常增长
        let second = counter_values(&[
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 0 },
            StatEntry { name: "outbound>>>node-b>>>traffic>>>downlink".into(), value: 150 },
        ]);
        let smoothed = counters.observe_all(&second);

        assert_eq!(
            smoothed.get("outbound>>>node-a>>>traffic>>>downlink").copied(),
            Some(5_000)
        );
        assert_eq!(
            smoothed.get("outbound>>>node-b>>>traffic>>>downlink").copied(),
            Some(150),
            "没归零的 tag 不该被抬高"
        );
        assert_eq!(counters.max_resets(), 1);
    }

    /// 畸形名字与负值不能进入 base 计算：否则会被当成真实历史，
    /// 在下次采样时凭空抬高读数。
    #[test]
    fn counter_values_ignore_malformed_names_and_clamp_negatives() {
        let v = counter_values(&[
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 100 },
            StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 9_999 },
            StatEntry { name: ">>>".into(), value: 9_999 },
            StatEntry { name: "inbound>>>>>traffic>>>uplink".into(), value: 9_999 },
            // 理论上不该出现负值；真出现时按 0，不能回绕成天文数字
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: -5 },
        ]);
        assert_eq!(v.len(), 1, "只有合法名字能进入单调化");
        assert_eq!(v.get("outbound>>>node-a>>>traffic>>>downlink").copied(), Some(100));
    }

    /// 聚合本身不得制造回退：计数器只增时，按 tag 的合计与总量只能增。
    /// （防的是聚合里出现重置/取 max/改方向之类的改动。）
    #[test]
    fn aggregates_are_monotonic_when_counters_grow() {
        let earlier = vec![
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 1_000 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>uplink".into(), value: 100 },
            StatEntry { name: "inbound>>>socks>>>traffic>>>downlink".into(), value: 50 },
        ];
        let later = vec![
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 7_000 },
            StatEntry { name: "inbound>>>tun>>>traffic>>>uplink".into(), value: 900 },
            StatEntry { name: "inbound>>>socks>>>traffic>>>downlink".into(), value: 80 },
        ];

        let before = traffic_by_tag(&earlier, "inbound");
        let after = traffic_by_tag(&later, "inbound");
        for (tag, (up0, down0)) in &before {
            let (up1, down1) = after.get(tag).copied().expect("同一 tag 应当还在");
            assert!(up1 >= *up0 && down1 >= *down0, "{tag} 的合计回退了");
        }
        let t0 = traffic_from_stats(&earlier, "api");
        let t1 = traffic_from_stats(&later, "api");
        assert!(t1.rx_bytes >= t0.rx_bytes && t1.tx_bytes >= t0.tx_bytes);
    }

    /// 端到端（不含网络）：计数器归零后单调化汇总不回退，且归零次数被上报。
    #[test]
    fn monotonic_traffic_by_tag_reports_resets_without_regressing() {
        let mut counters = MonotonicCounters::new();
        let before = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 8_000,
        }];
        let (first, resets0) = monotonic_traffic_by_tag(&mut counters, &before, "outbound");
        assert_eq!(first.get("node-a"), Some(&(0, 8_000)));
        assert_eq!(resets0, 0);

        let reset = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 0,
        }];
        let (second, resets1) = monotonic_traffic_by_tag(&mut counters, &reset, "outbound");
        assert_eq!(second.get("node-a"), Some(&(0, 8_000)), "归零不得回退");
        assert_eq!(resets1, 1, "归零事件要如实上报");

        let grown = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 64,
        }];
        let (third, _) = monotonic_traffic_by_tag(&mut counters, &grown, "outbound");
        assert_eq!(third.get("node-a"), Some(&(0, 8_064)));
    }
}
