//! 错误模型：**封闭**的失败分类，没有「重试 / 回落 / 降级」这一类成员。

use serde::{Deserialize, Serialize};

/// 失败分类。刻意保持封闭且不叫 `Other`：
/// 每一个成员都对应一个**用户能看懂、且我们知道自己该做什么**的状态。
///
/// 不设 `Retry` / `Fallback` / `Degraded` 三个成员是本项目的方法论落点：
/// 想让系统「失败了就换条路」，就得先往这里加成员 —— 而加成员要过评审。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 请求形状不对（缺字段、值越界、协议版本不匹配）。
    InvalidRequest,
    /// 引用的对象不存在（节点 id、订阅 id）。
    NotFound,
    /// 当前状态不允许这个操作（例如未连接时 switch_node）。
    Conflict,
    /// 没有权限（helper 拒绝、socket 权限）。
    PermissionDenied,
    /// 数据面不可用（xray 二进制缺失 / 无法执行）。
    DatapathUnavailable,
    /// 生成的配置被 xray 自己判为非法（`xray run -test` 失败）。
    ConfigInvalid,
    /// 核心进程在就绪前退出 —— 这是**结果**，不是需要掩盖的中间态。
    CoreExitedEarly,
    /// 特权 helper 不可用（未安装 / 版本不符 / 握手被拒）。
    HelperUnavailable,
    /// 本版本**不提供**该能力（对应 `DaemonHello.capabilities` 未宣告）。
    ///
    /// 纪律：它只能表示"我们没做这个能力"，**不能**用来包装"我试了但失败了"——
    /// 后者必须落到具体失败码，否则它就变成了一个新的兜底。
    Unsupported,
    /// 系统 IO 失败。
    Io,
    /// 我们自己的 bug（不变量被破坏）。出现它必须留日志，不能静默吞掉。
    Internal,
}

impl ErrorCode {
    /// 稳定的线上字符串。UI 用它做本地化，不用中文散文当协议。
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Conflict => "conflict",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::DatapathUnavailable => "datapath_unavailable",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::CoreExitedEarly => "core_exited_early",
            ErrorCode::HelperUnavailable => "helper_unavailable",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Io => "io",
            ErrorCode::Internal => "internal",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条失败。`message` 是给人看的（可直接显示给用户），
/// `detail` 是给排查用的结构化补充（可选）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl std::fmt::Display for ErrorBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ErrorBody {}

/// 便捷构造：`bad_request("...")`。存在的理由是让调用点短到不会有人为了省事去 `unwrap`。
pub fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidRequest, message)
}

pub fn not_found(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, message)
}

/// 「本版本不提供该能力」。只在 daemon 未宣告对应 `Capability` 时使用；
/// **不要**用它包装"我试了但失败了"（那必须落到具体失败码）。
pub fn unsupported(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Unsupported, message)
}

pub fn conflict(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Conflict, message)
}

pub fn internal(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}
