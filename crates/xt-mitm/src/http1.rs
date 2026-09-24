//! HTTP/1.1 的**头**解析与序列化（纯函数，不碰网络）。
//!
//! # 为什么自己写而不引一个 HTTP 库
//!
//! 我们只需要"读一段头、取出 method/target/headers"，以及"把头序列化回去"。
//! 引 `hyper` 会带进一整棵异步栈，而本 crate 的其余部分（判定、裁剪）本来是纯逻辑。
//! 取舍与 xt-intent 里手写 HTTP 客户端是同一条理由。
//!
//! # 边界（写清楚，免得被误用）
//!
//! * **只管头**，不解析 body（body 由调用方按 `Content-Length` / `chunked` 读）；
//! * 不接受 obs-fold（续行）—— 现代 HTTP/1.1 已弃用，遇到就报错而不是猜；
//! * 头名大小写不敏感（按 RFC），取值原样保留。

use std::fmt;

/// 解析/序列化过程中能遇到的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    /// 还没收到完整的头（`\r\n\r\n` 未出现）。**不是错误**，是"再读一点"。
    NeedMore,
    /// 请求行/状态行不合法。
    BadStartLine(String),
    /// 头里出现了我们的边界之外的东西（续行、非 ASCII 头名等）。
    BadHeader(String),
    /// 超过我们愿意接受的长度上限。
    TooLarge(usize),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedMore => write!(f, "头还没读完"),
            Self::BadStartLine(s) => write!(f, "起始行不合法：{s:?}"),
            Self::BadHeader(s) => write!(f, "头不合法：{s:?}"),
            Self::TooLarge(n) => write!(f, "头超过上限（{n} 字节）"),
        }
    }
}

impl std::error::Error for HttpError {}

/// 头部的长度上限。**8 KiB 是绝大多数实现的默认**（nginx 是 8k）。
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// 一段头（请求头或响应头）的解析结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestHead {
    pub method: String,
    /// 请求目标。可能是 origin-form（`/a/b?c=1`）或 absolute-form（`http://h/a`）。
    pub target: String,
    /// `HTTP/1.1` 这种版本串。
    pub version: String,
    /// 头字段，**按原样保留顺序**（序列化回去时顺序不变）。
    pub headers: Vec<(String, String)>,
}

/// `\r\n\r\n` 的位置 + 4（即头部分的长度）。没读到就 `None`。
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

impl RequestHead {
    /// 解析请求头。`head` 应当**包含**结尾的空行。
    pub fn parse(head: &[u8]) -> Result<Self, HttpError> {
        if head.len() > MAX_HEAD_BYTES {
            return Err(HttpError::TooLarge(head.len()));
        }
        let text = String::from_utf8_lossy(head);
        if !text.ends_with("\r\n\r\n") {
            return Err(HttpError::NeedMore);
        }
        let mut lines = text.trim_end_matches("\r\n").split("\r\n");
        let start = lines.next().ok_or(HttpError::NeedMore)?;
        // 严格的三个 token：method SP target SP version
        let mut parts = start.split(' ');
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let version = parts.next().unwrap_or("").to_string();
        if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/1.") {
            return Err(HttpError::BadStartLine(start.to_string()));
        }
        if parts.next().is_some() {
            // `GET /a HTTP/1.1 extra` —— 多出来的部分说明这不是我们认识的请求行。
            return Err(HttpError::BadStartLine(start.to_string()));
        }

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            // obs-fold（以空白开头的续行）我们**拒绝**而不是猜它的归属。
            if line.starts_with(' ') || line.starts_with('\t') {
                return Err(HttpError::BadHeader(format!("续行（obs-fold）：{line:?}")));
            }
            let (k, v) = line
                .split_once(':')
                .ok_or_else(|| HttpError::BadHeader(line.to_string()))?;
            if k.is_empty() || !k.is_ascii() {
                return Err(HttpError::BadHeader(line.to_string()));
            }
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        Ok(Self { method, target, version, headers })
    }

    /// 头字段取值（**大小写不敏感**，重复时取第一个）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 目标主机（不含端口）。
    ///
    /// 三种形态都要认：absolute-form（`http://h:p/a`）、带 Host 头的 origin-form、
    /// 以及 Host 头里带端口的情况。
    pub fn host(&self) -> Option<&str> {
        if let Some(rest) = self.target.strip_prefix("http://").or_else(|| self.target.strip_prefix("https://")) {
            let authority = rest.split('/').next().unwrap_or("");
            return authority.split(':').next().filter(|s| !s.is_empty());
        }
        self.header("host")
            .and_then(|h| h.rsplit_once(':').map(|(h, _)| h).or(Some(h)))
            .filter(|h| !h.is_empty())
    }

    /// 请求路径（absolute-form 时剥掉 scheme+authority）。
    pub fn path(&self) -> &str {
        if let Some(rest) = self.target.strip_prefix("http://").or_else(|| self.target.strip_prefix("https://")) {
            return match rest.find('/') {
                Some(i) => &rest[i..],
                None => "/",
            };
        }
        &self.target
    }

    /// 是不是 WebSocket 升级请求 —— 这种**盲转发**，不解析、不裁剪。
    pub fn is_websocket_upgrade(&self) -> bool {
        self.header("upgrade").map(|u| u.eq_ignore_ascii_case("websocket")).unwrap_or(false)
    }

    /// 序列化回头字节（**原样保留头顺序**，便于对照）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("{} {} {}\r\n", self.method, self.target, self.version);
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        out.into_bytes()
    }
}

/// 一串头字段的辅助操作（响应头也要用）。
pub fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(slot) = headers.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
        slot.0 = name.to_string();
        slot.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

/// 删掉某个头（大小写不敏感，**删全部**）。
pub fn remove_header(headers: &mut Vec<(String, String)>, name: &str) {
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
}

/// 取某个头（大小写不敏感）。
pub fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: &str = "GET /api/x?a=1 HTTP/1.1\r\nHost: news.example:443\r\nAccept: */*\r\n\r\n";

    #[test]
    fn parses_a_normal_request_head() {
        let h = RequestHead::parse(REQ.as_bytes()).unwrap();
        assert_eq!(h.method, "GET");
        assert_eq!(h.target, "/api/x?a=1");
        assert_eq!(h.version, "HTTP/1.1");
        assert_eq!(h.host(), Some("news.example"));
        assert_eq!(h.path(), "/api/x?a=1");
        assert_eq!(h.header("accept"), Some("*/*"), "头名大小写不敏感");
        assert!(!h.is_websocket_upgrade());
        // 序列化回去逐字相同（顺序不变）
        assert_eq!(h.to_bytes(), REQ.as_bytes());
    }

    #[test]
    fn absolute_form_target_is_understood() {
        let h = RequestHead::parse(b"GET http://ads.example:8080/p?q=1 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(h.host(), Some("ads.example"));
        assert_eq!(h.path(), "/p?q=1");
    }

    #[test]
    fn a_partial_head_says_need_more_instead_of_failing() {
        assert_eq!(RequestHead::parse(b"GET / HTTP/1.1\r\nHost: a\r\n"), Err(HttpError::NeedMore));
        // 长度**从字面量算**，不手数 —— 手算过一次，差 1 个字节就红了。
        let whole = b"GET / HTTP/1.1\r\nHost: a\r\n\r\n";
        assert_eq!(head_end(whole), Some(whole.len()));
        assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: a\r\n"), None);
    }

    #[test]
    fn malformed_start_lines_are_refused() {
        assert!(matches!(RequestHead::parse(b"GET\r\n\r\n"), Err(HttpError::BadStartLine(_))));
        assert!(matches!(RequestHead::parse(b"GET / HTTP/2.0\r\n\r\n"), Err(HttpError::BadStartLine(_))));
        assert!(matches!(
            RequestHead::parse(b"GET / HTTP/1.1 extra\r\n\r\n"),
            Err(HttpError::BadStartLine(_))
        ));
    }

    #[test]
    fn obs_fold_and_headerless_lines_are_refused_not_guessed() {
        assert!(matches!(
            RequestHead::parse(b"GET / HTTP/1.1\r\nHost: a\r\n  continued\r\n\r\n"),
            Err(HttpError::BadHeader(_))
        ));
        assert!(matches!(
            RequestHead::parse(b"GET / HTTP/1.1\r\nno-colon-here\r\n\r\n"),
            Err(HttpError::BadHeader(_))
        ));
    }

    #[test]
    fn an_oversized_head_is_refused() {
        let big = vec![b'a'; MAX_HEAD_BYTES + 1];
        assert!(matches!(RequestHead::parse(&big), Err(HttpError::TooLarge(_))));
    }

    #[test]
    fn websocket_upgrade_is_detected_case_insensitively() {
        let h = RequestHead::parse(b"GET /ws HTTP/1.1\r\nUpgrade: WebSocket\r\n\r\n").unwrap();
        assert!(h.is_websocket_upgrade());
    }

    #[test]
    fn header_helpers_are_case_insensitive_and_remove_all_copies() {
        let mut hs = vec![("Content-Length".to_string(), "5".to_string()),
                          ("content-length".to_string(), "9".to_string())];
        assert_eq!(get_header(&hs, "CONTENT-LENGTH"), Some("5"));
        set_header(&mut hs, "Content-Length", "12");
        assert_eq!(hs[0].1, "12");
        assert_eq!(hs.len(), 2, "set_header 只改第一处、不新增");
        remove_header(&mut hs, "content-Length");
        assert!(hs.is_empty(), "remove_header 删全部");
    }
}
