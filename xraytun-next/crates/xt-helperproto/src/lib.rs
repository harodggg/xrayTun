//! xt-helperproto —— 特权 helper 的**封闭**指令集。
//!
//! helper 不认识「代理」，只认识六条指令：`Status` / `TunUp` / `TakeTunFd` /
//! `CommitRoutes` / `TunDown` / `RestoreStale`。不接受任意命令、任意路径、
//! 任意文件写入 —— 这是 C1+C2 推出的权限边界的落地（见 `ARCHITECTURE.md` §1.2）。
//!
//! # 帧格式
//!
//! 与 `xt-ipc` **同一套帧格式约定**：`u32 大端长度前缀 || JSON 帧体`，
//! 长度只算帧体，上限 [`xt_contract::MAX_FRAME_BYTES`]。这里不直接复用
//! `xt_ipc::encode`（它绑定了 daemon 的 `Frame` 类型），而是复刻同一套
//! 字节级约定：这样 helper 帧与 daemon 帧在线路上长得一样，排查工具通用。
//!
//! # 封闭性怎么保证
//!
//! [`Request`] / [`Response`] 是 `#[serde(tag = ...)]` 的内部 tag 枚举：
//! 未知的 `cmd` / `result` tag 在反序列化时**直接报错**，不存在「收到不认识的
//! 指令就忽略」的路径。数据面可执行文件路径的白名单是 helperd 的运行时约束
//! （本 crate 只负责线上词汇，不负责校验运行时路径）。

use serde::{Deserialize, Serialize};

use xt_contract::error::{bad_request, ErrorBody};
use xt_contract::MAX_FRAME_BYTES;

/// helper 协议版本。与 daemon 的 `PROTOCOL_VERSION` 无关，独立演进。
pub const HELPER_PROTOCOL_VERSION: u32 = 1;

/// `TunUp` 的入参。字段形状与 `xt_macosnet::model::TunRequest` 一致，但这里是
/// **线上词汇**：helperproto 不依赖 xt-macosnet（两者都是 helperd 的下游），
/// 所以形状在这里独立声明，由 helperd 负责把线上词汇映射成 macosnet 的调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunUpArgs {
    pub addresses: Vec<String>,
    pub mtu: u16,
    /// 建卡阶段就装（走物理网关）。
    pub bypass_routes: Vec<String>,
    /// 提交阶段才装（走 utun 接口）。
    pub default_routes: Vec<String>,
    pub dns_servers: Vec<String>,
}

/// 一个已建好的 TUN 会话的引用（id + 接口名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub id: String,
    pub interface: String,
}

/// 客户端（daemon）→ 服务端（helper）的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// 握手 + 状态查询：协议版本、是否已安装、是否有活跃会话。
    Status,
    TunUp { args: TunUpArgs },
    /// 交 fd：响应帧之后 helper 会经 `SCM_RIGHTS` 把 utun fd 发给对端。
    TakeTunFd { session: SessionRef },
    CommitRoutes { session: SessionRef },
    TunDown { session: SessionRef },
    /// 启动时回滚上一次崩溃留下的半残状态。
    RestoreStale { state_dir: String },
}

/// 服务端（helper）→ 客户端（daemon）的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Status { version: u32, active_session: Option<SessionRef> },
    TunUp { session: SessionRef },
    /// fd 随后经 `SCM_RIGHTS` 到达（**帧先、fd 后**，见 fdpass 的时序约束）。
    TakeTunFd { session: SessionRef },
    /// 同步完成的成功（无结果需要携带）。
    Ok,
    /// 回滚结果：被回滚的接口名；`None` = 没有残留。
    RestoreStale { interface: Option<String> },
    /// **服务端的错误通道**。
    ///
    /// helper 的 `Response` 原本没有错误变体，于是「指令被拒」只能靠关连接
    /// 表达 —— 对端看到的是 EOF，拿不到 `ErrorBody` 里的 `code`/`message`，
    /// 排障时无法区分「会话不存在」与「帧读坏了」。
    ///
    /// 约定（由 helperd 的 `dispatch` 落地）：`dispatch` 返回
    /// `Err(ErrorBody)`，连接层把它包成这一变体写回一帧；`Ok` 才写业务响应。
    /// 所以线上每一条请求**恰好**得到一帧响应，成功与失败都有可读的结构化原因。
    ///
    /// JSON 形状：`{"result":"error","error":{"code":...,"message":...}}`。
    Error { error: ErrorBody },
}

/// 序列化一帧：`u32 大端长度前缀 || JSON`。
pub fn encode_request(request: &Request) -> Result<Vec<u8>, ErrorBody> {
    encode_len_prefixed(request)
}

/// 从一帧完整字节（含 4 字节前缀）解出请求。长度声明必须与帧体**完全相等**。
pub fn decode_request(bytes: &[u8]) -> Result<Request, ErrorBody> {
    decode_len_prefixed(bytes)
}

/// 序列化一帧响应。
pub fn encode_response(response: &Response) -> Result<Vec<u8>, ErrorBody> {
    encode_len_prefixed(response)
}

/// 从一帧完整字节解出响应。
pub fn decode_response(bytes: &[u8]) -> Result<Response, ErrorBody> {
    decode_len_prefixed(bytes)
}

fn encode_len_prefixed<T: Serialize>(value: &T) -> Result<Vec<u8>, ErrorBody> {
    let body = serde_json::to_vec(value)
        .map_err(|e| bad_request(format!("帧序列化失败：{e}")))?;
    let len = u32::try_from(body.len())
        .map_err(|_| frame_too_big(body.len()))?;
    if len > MAX_FRAME_BYTES {
        return Err(frame_too_big(body.len()));
    }
    let mut bytes = Vec::with_capacity(4 + body.len());
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

fn decode_len_prefixed<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, ErrorBody> {
    if bytes.len() < 4 {
        return Err(bad_request(format!("帧只有 {} 字节，不足 4 字节长度前缀", bytes.len())));
    }
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if len > MAX_FRAME_BYTES as usize {
        return Err(frame_too_big(len));
    }
    if bytes.len() != 4 + len {
        return Err(bad_request(format!(
            "长度前缀声明 {len} 字节，实际帧体 {} 字节：不允许分片或多余字节",
            bytes.len() - 4
        )));
    }
    serde_json::from_slice(&bytes[4..])
        .map_err(|e| bad_request(format!("帧体不是合法的 helper 帧 JSON：{e}")))
}

fn frame_too_big(len: usize) -> ErrorBody {
    bad_request(format!("帧体 {len} 字节超过上限 {MAX_FRAME_BYTES} 字节：不允许分片"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tun_up() -> Request {
        Request::TunUp {
            args: TunUpArgs {
                addresses: vec!["198.18.0.1/15".into()],
                mtu: 1420,
                bypass_routes: vec!["192.168.1.0/24".into()],
                default_routes: vec!["0.0.0.0/1".into(), "128.0.0.0/1".into()],
                dns_servers: vec!["1.1.1.1".into()],
            },
        }
    }

    #[test]
    fn every_request_roundtrips() {
        let session = SessionRef { id: "sess-1".into(), interface: "utun4".into() };
        let requests = vec![
            Request::Status,
            tun_up(),
            Request::TakeTunFd { session: session.clone() },
            Request::CommitRoutes { session: session.clone() },
            Request::TunDown { session: session.clone() },
            Request::RestoreStale { state_dir: "/Library/Application Support/XrayTun".into() },
        ];
        for req in requests {
            let bytes = encode_request(&req).unwrap();
            let back = decode_request(&bytes).unwrap();
            assert_eq!(back, req, "请求必须原样 roundtrip");
        }
    }

    #[test]
    fn every_response_roundtrips() {
        let session = SessionRef { id: "sess-1".into(), interface: "utun4".into() };
        let responses = vec![
            Response::Status { version: HELPER_PROTOCOL_VERSION, active_session: Some(session.clone()) },
            Response::TunUp { session: session.clone() },
            Response::TakeTunFd { session: session.clone() },
            Response::Ok,
            Response::RestoreStale { interface: Some("utun4".into()) },
            Response::RestoreStale { interface: None },
            // 错误通道：`code` / `message` 必须原样往返 —— helperd 的 dispatch
            // 失败时写的就是这一变体，它对端要靠它区分失败原因。
            Response::Error {
                error: ErrorBody::new(xt_contract::error::ErrorCode::PermissionDenied, "对端签名校验未通过"),
            },
            Response::Error {
                error: ErrorBody::new(xt_contract::error::ErrorCode::NotFound, "没有活跃 TUN 会话"),
            },
        ];
        for resp in responses {
            let bytes = encode_response(&resp).unwrap();
            let back = decode_response(&bytes).unwrap();
            assert_eq!(back, resp, "响应必须原样 roundtrip");
        }
    }

    /// 错误变体的线上形状：内部 tag 是 `error`，`ErrorBody` 的 `code` 是稳定的
    /// 蛇形字符串，**不是**中文散文。对端据此做本地化与分支。
    #[test]
    fn error_response_has_a_stable_wire_shape() {
        let response = Response::Error {
            error: ErrorBody::new(xt_contract::error::ErrorCode::Conflict, "已有活跃会话"),
        };
        let bytes = encode_response(&response).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
        assert_eq!(json["result"], "error");
        assert_eq!(json["error"]["code"], "conflict");
        assert_eq!(json["error"]["message"], "已有活跃会话");
        // 且能原样解回来（不是只测了 JSON 形状）。
        assert_eq!(decode_response(&bytes).unwrap(), response);
    }

    /// 封闭指令集：未知 cmd 必须被拒，而不是被静默忽略。
    #[test]
    fn unknown_command_is_rejected() {
        let body = br#"{"cmd":"run_arbitrary_binary","path":"/bin/rm"}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(body);
        let err = decode_request(&bytes).unwrap_err();
        assert_eq!(err.code, xt_contract::error::ErrorCode::InvalidRequest);
        assert!(err.message.contains("helper 帧"), "错误应说明帧不合法：{}", err.message);
    }

    /// 长度前缀与实际帧体不一致（分片 / 多余字节）必须被拒。
    #[test]
    fn mismatched_length_is_rejected() {
        let body = br#"{"cmd":"status"}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((body.len() + 5) as u32).to_be_bytes()); // 多声明 5 字节
        bytes.extend_from_slice(body);
        assert!(decode_request(&bytes).is_err());

        // 少声明：截断。
        let mut short = Vec::new();
        short.extend_from_slice(&((body.len() - 2) as u32).to_be_bytes());
        short.extend_from_slice(body);
        assert!(decode_request(&short).is_err());
    }

    /// 超过上限的帧体必须被拒（不必等帧体到齐）。
    #[test]
    fn oversized_frame_is_rejected() {
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF, 0x00]; // 声明 4GB 长度
        let err = decode_request(&bytes).unwrap_err();
        assert!(err.message.contains("超过上限"));
    }

    /// 小于 4 字节的帧（连长度前缀都不完整）必须被拒。
    #[test]
    fn truncated_header_is_rejected() {
        assert!(decode_request(&[0x00, 0x01]).is_err());
    }
}
