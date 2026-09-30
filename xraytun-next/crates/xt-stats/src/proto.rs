//! 手写 protobuf + gRPC 分帧：只实现 Xray `StatsService.QueryStats` 用到的那两条消息。
//!
//! 为什么不引 tonic/prost：这里只需要一个一元 RPC，报文结构简单到可以直接数清字段；
//! 引入整条代码生成链（build.rs + protoc + 版本耦合）换来的是「proto 一升级就编译不过」，
//! 而收益为零。真正需要的是**能被单元测试钉住的字节级行为** —— 下面每个函数都有测试。
//!
//! ```text
//! QueryStatsRequest  { string pattern = 1; bool reset = 2; }
//! QueryStatsResponse { repeated Stat stat = 1; }
//! Stat               { string name = 1; int64 value = 2; }
//! ```

use xt_contract::error::{ErrorBody, ErrorCode};

/// 单个计数器的名字（`inbound>>>socks>>>traffic>>>downlink`）与原始值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatEntry {
    pub name: String,
    pub value: i64,
}

fn malformed(message: impl Into<String>) -> ErrorBody {
    // 解析失败不是「系统 IO」，但它确实来自对端字节；用 Io 而不是 Internal：
    // Internal 的含义是「我们自己的不变量坏了」，把对端发来的畸形字节说成我们的 bug
    // 会让人查错方向。
    ErrorBody::new(ErrorCode::Io, message)
}

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
fn read_varint(buf: &[u8], mut i: usize) -> Result<(u64, usize), ErrorBody> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let Some(&byte) = buf.get(i) else {
            return Err(malformed("protobuf 在 varint 中途截断"));
        };
        i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, i));
        }
        shift += 7;
        if shift >= 64 {
            return Err(malformed("protobuf varint 超过 64 位"));
        }
    }
}

enum FieldValue {
    Varint(u64),
    Bytes(Vec<u8>),
}

/// 依次读出一个消息的所有字段。
fn read_fields(buf: &[u8]) -> Result<Vec<(u32, FieldValue)>, ErrorBody> {
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
                    .ok_or_else(|| malformed("protobuf 字段长度越界"))?;
                out.push((field, FieldValue::Bytes(buf[i..end].to_vec())));
                i = end;
            }
            // 我们只发/收上面两种 wire type；别的类型说明字节流已经错位，
            // 继续解析只会读出垃圾数字 —— 那比报错更糟。
            other => return Err(malformed(format!("protobuf wire type {other} 不受支持"))),
        }
    }
    Ok(out)
}

/// 构造 `QueryStatsRequest`。`pattern` 为空表示「全部计数器」。
///
/// `reset` 固定传 false：我们只读不重置。重置会让计数器归零，
/// 而界面上「流量突然清零」正是用户最不能接受的假象。
pub fn encode_query_stats_request(pattern: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if !pattern.is_empty() {
        put_bytes(1, pattern.as_bytes(), &mut out);
    }
    out
}

/// 解析 `QueryStatsResponse`。
pub fn decode_query_stats_response(buf: &[u8]) -> Result<Vec<StatEntry>, ErrorBody> {
    let mut stats = Vec::new();
    for (field, value) in read_fields(buf)? {
        if field != 1 {
            continue; // 未知字段按 protobuf 约定跳过
        }
        let FieldValue::Bytes(stat) = value else {
            return Err(malformed("Stat 字段的 wire type 不是 length-delimited"));
        };
        let mut name = String::new();
        let mut val = 0i64;
        for (f, v) in read_fields(&stat)? {
            match (f, v) {
                (1, FieldValue::Bytes(b)) => name = String::from_utf8_lossy(&b).into_owned(),
                (2, FieldValue::Varint(n)) => val = n as i64,
                _ => {}
            }
        }
        // 没有名字的条目无法溯源到任何真实计数器，直接丢弃而不是塞个空名。
        if !name.is_empty() {
            stats.push(StatEntry { name, value: val });
        }
    }
    Ok(stats)
}

/// gRPC 消息帧：1 字节压缩标志 + 4 字节大端长度 + 消息体。
pub fn encode_grpc_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(0); // 不压缩
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 从响应体里取出第一条消息；多余数据（如果有）忽略。
pub fn decode_grpc_frame(buf: &[u8]) -> Result<&[u8], ErrorBody> {
    if buf.len() < 5 {
        return Err(malformed(format!("gRPC 响应只有 {} 字节，读不出帧头", buf.len())));
    }
    if buf[0] != 0 {
        return Err(malformed("gRPC 响应使用了压缩，本实现不支持"));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let end = 5usize
        .checked_add(len)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| malformed(format!("gRPC 帧声明 {len} 字节，实际只有 {}", buf.len() - 5)))?;
    Ok(&buf[5..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_request_is_empty_for_all_pattern() {
        assert!(encode_query_stats_request("").is_empty());
    }

    #[test]
    fn query_request_encodes_a_pattern() {
        // field 1, wire 2, len 3, "abc"
        assert_eq!(encode_query_stats_request("abc"), vec![0x0a, 0x03, b'a', b'b', b'c']);
    }

    /// 用**真实核心抓下来的响应前缀**做夹具（旧仓库实测采样）：
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
            vec![StatEntry { name: "outbound>>>api>>>traffic>>>uplink".into(), value: 0 }]
        );
    }

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
    fn varint_roundtrip_over_boundaries() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(v, &mut buf);
            assert_eq!(read_varint(&buf, 0).unwrap(), (v, buf.len()), "值 {v}");
        }
    }

    #[test]
    fn grpc_frame_roundtrip() {
        let framed = encode_grpc_frame(b"hello");
        assert_eq!(&framed[..5], &[0, 0, 0, 0, 5]);
        assert_eq!(decode_grpc_frame(&framed).unwrap(), b"hello");
    }

    #[test]
    fn grpc_frame_rejects_truncated_and_compressed() {
        assert!(decode_grpc_frame(&[0u8, 0, 0, 0, 5, 1, 2]).is_err());
        assert!(decode_grpc_frame(&[0u8, 0, 0]).is_err());
        assert!(decode_grpc_frame(&[1u8, 0, 0, 0, 0]).is_err());
    }
}
