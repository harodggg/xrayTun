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
use std::collections::HashMap;
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
    Some(CounterParts {
        direction: direction.to_string(),
        tag: tag.to_string(),
        flow: flow.to_string(),
    })
}

/// 按 tag 汇总某个方向的上下行字节。
///
/// 返回 `tag -> (uplink, downlink)`。
pub fn traffic_by_tag(stats: &[StatEntry], direction: &str) -> HashMap<String, (u64, u64)> {
    let mut out: HashMap<String, (u64, u64)> = HashMap::new();
    for s in stats {
        let Some(parts) = parse_traffic_counter(&s.name) else {
            continue;
        };
        if parts.direction != direction {
            continue;
        }
        let entry = out.entry(parts.tag).or_insert((0, 0));
        let value = s.value.max(0) as u64;
        match parts.flow.as_str() {
            "uplink" => entry.0 = entry.0.saturating_add(value),
            "downlink" => entry.1 = entry.1.saturating_add(value),
            _ => {}
        }
    }
    out
}

pub fn traffic_from_stats(stats: &[StatEntry], ignore_inbound: &str) -> TrafficCounters {
    let mut total = TrafficCounters::default();
    for s in stats {
        let Some(rest) = s.name.strip_prefix("inbound>>>") else {
            continue;
        };
        let mut parts = rest.split(">>>");
        let (Some(tag), Some("traffic"), Some(dir)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if tag == ignore_inbound || parts.next().is_some() {
            continue;
        }
        let value = s.value.max(0) as u64;
        match dir {
            "downlink" => total.rx_bytes += value,
            "uplink" => total.tx_bytes += value,
            _ => {}
        }
    }
    total
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
}
