//! 帧编解码测试：长度前缀（大端）、上限、分片拒绝。

use xt_contract::error::ErrorCode;
use xt_contract::model::{ConnectionView, LogLevel, LogLine};
use xt_contract::protocol::{Event, Frame, Outcome, Request, Response};
use xt_contract::MAX_FRAME_BYTES;
use xt_ipc::{decode, encode};

fn log_frame(seq: u64, message: &str) -> Frame {
    Frame::event(
        seq,
        Event::Log {
            line: LogLine {
                ts_ms: 1,
                level: LogLevel::Info,
                target: "xt-ipc-test".to_string(),
                message: message.to_string(),
            },
        },
    )
}

fn samples() -> Vec<Frame> {
    vec![
        Frame::request(1, Request::Status),
        Frame::response(2, Outcome::ok(Response::Status(ConnectionView::default()))),
        Frame::event(3, Event::State { view: ConnectionView::default() }),
        log_frame(4, "hello 世界"),
    ]
}

#[test]
fn encode_decode_roundtrip_keeps_frame_identical() {
    for frame in samples() {
        let bytes = encode(&frame).unwrap();
        // 长度前缀是 4 字节大端，且只算帧体。
        assert!(bytes.len() > 4);
        let declared = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(declared as usize, bytes.len() - 4);
        assert_eq!(decode(&bytes).unwrap(), frame);
    }
}

#[test]
fn oversized_frame_is_rejected_at_encode_time() {
    let huge = LogLine {
        ts_ms: 1,
        level: LogLevel::Info,
        target: "t".to_string(),
        message: "x".repeat(MAX_FRAME_BYTES as usize),
    };
    let error = encode(&Frame::event(1, Event::Log { line: huge })).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.contains("不允许分片"), "{}", error.message);
}

#[test]
fn oversized_declared_length_is_rejected_without_reading_body() {
    // 声明 1 MiB + 1：即使帧体只有 2 字节，也必须先按上限拒绝。
    let bytes = [0x00, 0x10, 0x00, 0x01, b'{', b'}'];
    assert_eq!(bytes.len(), 6);
    let error = decode(&bytes).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[test]
fn partial_or_extra_bytes_are_rejected() {
    let frame = encode(&samples()[0]).unwrap();

    let mut truncated = frame.clone();
    truncated.pop();
    assert!(decode(&truncated).is_err(), "缺少字节必须被拒绝");

    let mut extended = frame.clone();
    extended.push(0);
    assert!(decode(&extended).is_err(), "多余字节必须被拒绝");

    assert!(decode(&frame[..2]).is_err(), "不足长度前缀必须被拒绝");
    assert!(decode(&[]).is_err());
}

#[test]
fn valid_length_but_invalid_json_is_rejected() {
    let body = b"{{";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(body);
    let error = decode(&bytes).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}
