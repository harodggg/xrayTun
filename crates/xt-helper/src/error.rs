//! helper 的错误类型：统一用线协议里的 [`xt_proto::HelperError`]。
//!
//! 这样做的原因：helper 的每个失败最终都要变成一条 `Response::Error` 发给 GUI，
//! 中间再引入一套内部错误类型只会多一次无意义的映射。唯一需要转换的是
//! `xt_tun::Error`（网络配置层的错误）。
//!
//! 注意这里不能用 `impl From`：`HelperError` 和 `xt_tun::Error` 都是外部类型，
//! 孤儿规则不允许。所以用显式的映射函数，顺便把错误归类到正确的 `ErrorCode`。

pub use xt_proto::{ErrorCode, HelperError};

pub type Result<T, E = HelperError> = std::result::Result<T, E>;

/// 把网络配置层的错误映射成线协议错误。
pub fn tun_err(e: xt_tun::Error) -> HelperError {
    use xt_tun::Error as E;
    let code = match &e {
        E::Invalid(_) | E::Parse { .. } => ErrorCode::InvalidRequest,
        E::NoDefaultRoute | E::NoNetworkService { .. } => ErrorCode::NetworkConfigFailed,
        E::Command { .. } => ErrorCode::NetworkConfigFailed,
        E::Snapshot(_) => ErrorCode::Internal,
        E::Syscall { .. } => ErrorCode::TunCreateFailed,
    };
    HelperError::new(code, e.to_string())
}

pub fn internal(msg: impl Into<String>) -> HelperError {
    HelperError::new(ErrorCode::Internal, msg)
}

/// 仅测试使用：构造一个 `InvalidRequest` 错误。
#[cfg(test)]
pub fn invalid(msg: impl Into<String>) -> HelperError {
    HelperError::new(ErrorCode::InvalidRequest, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tun_errors_map_to_sensible_codes() {
        let e = tun_err(xt_tun::Error::Invalid("bad cidr".into()));
        assert_eq!(e.code, ErrorCode::InvalidRequest);

        let e = tun_err(xt_tun::Error::NoDefaultRoute);
        assert_eq!(e.code, ErrorCode::NetworkConfigFailed);

        let e = tun_err(xt_tun::Error::Snapshot("disk".into()));
        assert_eq!(e.code, ErrorCode::Internal);
    }

    #[test]
    fn helper_error_displays_code_and_message() {
        let e = invalid("nope");
        assert!(e.to_string().contains("nope"));
    }
}
