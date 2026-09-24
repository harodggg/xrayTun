//! HTTPS 传输：一个 POST，读回响应。
//!
//! # 为什么手写而不是引 `reqwest` / `hyper`
//!
//! 完整理由在 `docs/design/INTENT-FILTER.md` §5.1。一句话：我们只需要
//! "POST 一段 JSON、读回一段 JSON"，而 `reqwest` 会带进 `hyper + tower + …`
//! 一整棵树。本项目已经为两条 protobuf 消息手写过 h2c（`xt-core/src/xray/stats.rs`），
//! 这里是同一个取舍。
//!
//! # 分成三层，各自可测
//!
//! 1. [`build_request`] —— 纯函数：请求字节。
//! 2. [`parse_response`] —— 纯函数：响应字节（`Content-Length` / `chunked` / 读到 EOF）。
//! 3. [`TlsTransport`] —— 只剩"连 TCP + 套 rustls + 跑上面两个纯函数"。
//!
//! 协议层的重试、状态映射、退避在 [`crate::jev`]，用 [`Transport`] 这个 trait
//! 注入假实现即可完整测试 —— **测试里不发一次网络请求**。
//!
//! # 超时是**总预算**，不是每步预算
//!
//! 与 `xt-core` 的延迟探针同一条纪律（`probe_timeout_is_a_total_budget`）：
//! 连接、写、读共用同一个 deadline。分步超时会让"最坏情况"变成各步之和，
//! 而调用方以为自己设的是总时间。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// 一次 HTTP 请求。URL 已经在校验层保证是 `https://`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

/// 响应。**只保留状态码与正文** —— 我们不需要 header、不需要流式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// 响应头。协议层要用它读 `Retry-After`。
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// URL 不认识 / 不支持的 scheme。
    BadUrl(String),
    /// DNS 解析或 TCP 连接失败。
    Connect(String),
    /// TLS 握手失败（含证书校验失败）。
    Tls(String),
    /// 连接、写、读共用总预算，超了就是它。
    Timeout { phase: &'static str, budget_ms: u64 },
    /// 写不进去 / 读不出来。
    Io(String),
    /// 响应根本不是 HTTP/1.x。
    Malformed(String),
}

impl TransportError {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BadUrl(_) => "bad_url",
            Self::Connect(_) => "connect",
            Self::Tls(_) => "tls",
            Self::Timeout { .. } => "timeout",
            Self::Io(_) => "io",
            Self::Malformed(_) => "malformed",
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadUrl(m) => write!(f, "URL 不合法：{m}"),
            Self::Connect(m) => write!(f, "连接失败：{m}"),
            Self::Tls(m) => write!(f, "TLS 失败：{m}"),
            Self::Timeout { phase, budget_ms } => {
                write!(f, "{phase} 超过 {budget_ms}ms 的总预算")
            }
            Self::Io(m) => write!(f, "IO 失败：{m}"),
            Self::Malformed(m) => write!(f, "响应不像 HTTP：{m}"),
        }
    }
}

/// 传输层接缝。生产实现是 [`TlsTransport`]，测试用假实现。
pub trait Transport: Send + Sync {
    fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;

    /// 人类可读的描述（**不要**包含密钥）。
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// URL
// ---------------------------------------------------------------------------

/// 拆出来的 URL 片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    pub host: String,
    pub port: u16,
    /// 含查询串的路径。
    pub path: String,
}

/// 只支持 `https://`（**没有** http 分支：一个把域名发给远端网关的功能，
/// 不该有一次例外允许明文）。
pub fn parse_https_url(url: &str) -> Result<ParsedUrl, TransportError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| TransportError::BadUrl(format!("必须是 https:// 开头：{url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        return Err(TransportError::BadUrl("没有主机名".into()));
    }
    if authority.contains('@') {
        return Err(TransportError::BadUrl("URL 里不许出现 userinfo（凭据走请求头）".into()));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| TransportError::BadUrl(format!("端口不是数字：{p}")))?;
            (h.to_string(), port)
        }
        None => (authority.to_string(), 443),
    };
    if host.is_empty() {
        return Err(TransportError::BadUrl("没有主机名".into()));
    }
    if host.contains(' ') {
        return Err(TransportError::BadUrl("主机名里有空格".into()));
    }
    Ok(ParsedUrl { host, port, path })
}

/// 把 base URL 与路径拼起来（避免双斜杠，避免丢掉 base 里的子路径）。
pub fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

// ---------------------------------------------------------------------------
// 请求字节
// ---------------------------------------------------------------------------

/// 生成请求字节。`Host` 头必须自己写 —— 这是唯一一处"HTTP 客户端才知道"的必需头。
pub fn build_request(url: &ParsedUrl, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("POST ");
    out.push_str(&url.path);
    out.push_str(" HTTP/1.1\r\n");
    out.push_str(&format!(
        "Host: {}\r\n",
        if url.port == 443 {
            url.host.clone()
        } else {
            format!("{}:{}", url.host, url.port)
        }
    ));
    out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    out.push_str("Connection: close\r\n");
    let mut has_ua = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
        {
            continue; // 这三个由我们统一管理，不许调用方覆盖
        }
        if k.eq_ignore_ascii_case("user-agent") {
            has_ua = true;
        }
        if k.contains('\r') || k.contains('\n') || v.contains('\r') || v.contains('\n') {
            continue; // 头注入：静默丢，绝不写出去
        }
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_ua {
        out.push_str(&format!("User-Agent: xt-intent/{}\r\n", env!("CARGO_PKG_VERSION")));
    }
    out.push_str("\r\n");

    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

// ---------------------------------------------------------------------------
// 响应解析
// ---------------------------------------------------------------------------

/// 上限：一条响应最多读这么多。Jev 的答案只有几百字节，
/// 没有上限的话一个坏掉的/恶意的服务端就能把内存吃光。
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// 解析响应字节。
///
/// 三种取正文的方式，按优先级：
/// 1. `Transfer-Encoding: chunked`（去分块）；
/// 2. `Content-Length`；
/// 3. 都没有 —— 读到 EOF 的全部剩余（HTTP/1.0 风格；我们发的是 `Connection: close`）。
pub fn parse_response(raw: &[u8]) -> Result<HttpResponse, TransportError> {
    let header_end = find_header_end(raw)
        .ok_or_else(|| TransportError::Malformed("没有找到空行结尾的响应头".into()))?;
    let head = std::str::from_utf8(&raw[..header_end])
        .map_err(|e| TransportError::Malformed(format!("响应头不是 UTF-8：{e}")))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| TransportError::Malformed("没有状态行".into()))?;
    let mut parts = status_line.split(' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(TransportError::Malformed(format!("状态行不是 HTTP/1.x：{status_line}")));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| TransportError::Malformed(format!("状态码读不出来：{status_line}")))?;

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim().to_string();
        let v = v.trim().to_string();
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v.parse().ok();
        }
        if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
        headers.push((k, v));
    }

    let body_start = header_end + 4; // \r\n\r\n
    let rest = &raw[body_start.min(raw.len())..];
    let body_bytes: Vec<u8> = if chunked {
        dechunk(rest)?
    } else if let Some(len) = content_length {
        rest[..len.min(rest.len())].to_vec()
    } else {
        rest.to_vec()
    };
    if body_bytes.len() > MAX_RESPONSE_BYTES {
        return Err(TransportError::Malformed(format!(
            "响应体超过上限（{} 字节）",
            MAX_RESPONSE_BYTES
        )));
    }
    let body = String::from_utf8_lossy(&body_bytes).to_string();
    Ok(HttpResponse { status, headers, body })
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n")
}

/// 去分块编码。坏掉的输入返回 `Malformed` 而不是 panic ——
/// 这是完全由对端控制的输入。
fn dechunk(raw: &[u8]) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let line_end = raw[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| TransportError::Malformed("chunked：分块头没有结尾".into()))?;
        let size_line = std::str::from_utf8(&raw[pos..pos + line_end])
            .map_err(|e| TransportError::Malformed(format!("chunked：分块头不是 ASCII：{e}")))?;
        // `1a;ext=1` 这种要取分号前的十六进制。
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| TransportError::Malformed(format!("chunked：分块长度不是十六进制：{size_hex}")))?;
        pos += line_end + 2;
        if size == 0 {
            return Ok(out);
        }
        if pos + size > raw.len() {
            return Err(TransportError::Malformed("chunked：分块长度超出响应体".into()));
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size;
        // 跳过分块数据后面的 CRLF。
        if raw[pos..].starts_with(b"\r\n") {
            pos += 2;
        }
        if out.len() > MAX_RESPONSE_BYTES {
            return Err(TransportError::Malformed("chunked：解出来的正文超过上限".into()));
        }
    }
}

// ---------------------------------------------------------------------------
// TLS 传输
// ---------------------------------------------------------------------------

/// `rustls` + `webpki-roots` 的实现。
///
/// 证书校验**必须**用系统/内置根（`webpki-roots`）：
/// 一个判定"这是不是广告"的功能，没有任何理由允许被中间人替换判定答案。
pub struct TlsTransport {
    roots: std::sync::Arc<rustls::RootCertStore>,
}

impl Default for TlsTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl TlsTransport {
    pub fn new() -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self { roots: std::sync::Arc::new(roots) }
    }

    /// 信任锚数量（诊断用：0 说明构建有问题，所有校验都会失败）。
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    fn connect_tls(&self, url: &ParsedUrl) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, TransportError> {
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(self.roots.clone())
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(url.host.clone())
            .map_err(|e| TransportError::BadUrl(format!("主机名不能做 TLS 校验：{e}")))?;
        let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
            .map_err(|e| TransportError::Tls(e.to_string()))?;
        let sock = TcpStream::connect((url.host.as_str(), url.port))
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(rustls::StreamOwned::new(conn, sock))
    }
}

impl Transport for TlsTransport {
    fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let deadline = Instant::now() + request.timeout;
        let url = parse_https_url(&request.url)?;

        let mut stream = self.connect_tls(&url)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::Timeout {
                phase: "connect",
                budget_ms: request.timeout.as_millis() as u64,
            });
        }
        // 读写超时也吃同一个 deadline（每一次重设都按"还剩多少"算）。
        let _ = stream.sock.set_read_timeout(Some(remaining));
        let _ = stream.sock.set_write_timeout(Some(remaining));

        let bytes = build_request(&url, &request.headers, &request.body);
        stream.write_all(&bytes).map_err(|e| classify_io(e, &deadline, request.timeout, "write"))?;
        stream.flush().map_err(|e| classify_io(e, &deadline, request.timeout, "write"))?;

        let mut raw = Vec::with_capacity(4096);
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    if raw.len() > MAX_RESPONSE_BYTES {
                        return Err(TransportError::Malformed(format!(
                            "响应超过上限（{} 字节）",
                            MAX_RESPONSE_BYTES
                        )));
                    }
                    // 头读完且 Content-Length 已满足 ⇒ 不必等服务端关连接。
                    if let Some(end) = find_header_end(&raw) {
                        let head = String::from_utf8_lossy(&raw[..end]).to_string();
                        if let Some(len) = content_length_of(&head) {
                            if raw.len() >= end + 4 + len {
                                break;
                            }
                        }
                        if !head.to_ascii_lowercase().contains("chunked") && raw.len() > end + 4 {
                            // 没有 Content-Length 也没有 chunked：只能读到 EOF。
                        }
                    }
                }
                Err(e) => return Err(classify_io(e, &deadline, request.timeout, "read")),
            }
            if Instant::now() >= deadline {
                return Err(TransportError::Timeout {
                    phase: "read",
                    budget_ms: request.timeout.as_millis() as u64,
                });
            }
        }
        parse_response(&raw)
    }

    fn describe(&self) -> String {
        "https(rustls)".into()
    }
}

fn content_length_of(head: &str) -> Option<usize> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok()).flatten()
    })
}

fn classify_io(
    e: std::io::Error,
    deadline: &Instant,
    budget: Duration,
    phase: &'static str,
) -> TransportError {
    // `WouldBlock` / `TimedOut` 在设置了读写超时的 socket 上就是超时。
    if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ) {
        return TransportError::Timeout { phase, budget_ms: budget.as_millis() as u64 };
    }
    // rustls 的 `UnexpectedEof` 常见于"对端在握手期就关了"。
    if Instant::now() >= *deadline {
        return TransportError::Timeout { phase, budget_ms: budget.as_millis() as u64 };
    }
    TransportError::Io(format!("{phase}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(u: &str) -> ParsedUrl {
        parse_https_url(u).unwrap()
    }

    #[test]
    fn url_parsing_handles_subpaths_ports_and_rejects_plain_http() {
        assert_eq!(
            url("https://api.typesafe.ai/v1/systemone"),
            ParsedUrl { host: "api.typesafe.ai".into(), port: 443, path: "/v1/systemone".into() }
        );
        assert_eq!(
            url("https://opencode.ai/zen/v1/systemone"),
            ParsedUrl { host: "opencode.ai".into(), port: 443, path: "/zen/v1/systemone".into() }
        );
        assert_eq!(url("https://gw.example:8443/v1/systemone").port, 8443);
        assert_eq!(url("https://gw.example").path, "/");

        // 明文一律拒绝：没有例外。
        assert!(matches!(
            parse_https_url("http://gw.example/v1/systemone"),
            Err(TransportError::BadUrl(_))
        ));
        // URL 里带凭据也拒绝（密钥走请求头，不进 URL 日志）。
        assert!(parse_https_url("https://user:pass@gw.example/v1").is_err());
        assert!(parse_https_url("https:///v1").is_err());
        assert!(parse_https_url("https://gw.example:abc/v1").is_err());
    }

    #[test]
    fn join_url_never_produces_a_double_slash() {
        assert_eq!(join_url("https://gw.example", "/v1/systemone"), "https://gw.example/v1/systemone");
        assert_eq!(join_url("https://gw.example/", "/v1/systemone"), "https://gw.example/v1/systemone");
        assert_eq!(join_url("https://gw.example/zen", "/v1/systemone"), "https://gw.example/zen/v1/systemone");
        assert_eq!(join_url("https://gw.example/zen/", "v1/x"), "https://gw.example/zen/v1/x");
    }

    #[test]
    fn request_bytes_are_well_formed() {
        let u = url("https://opencode.ai/zen/v1/systemone");
        let bytes = build_request(&u, &[("content-type".into(), "application/json".into())], b"{\"a\":1}");
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with("POST /zen/v1/systemone HTTP/1.1\r\n"));
        assert!(text.contains("Host: opencode.ai\r\n"));
        assert!(text.contains("Content-Length: 7\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("content-type: application/json\r\n"));
        assert!(text.contains("User-Agent: xt-intent/"));
        assert!(text.ends_with("\r\n\r\n{\"a\":1}"));
        // 正文长度与 Content-Length 必须一致（差一个字节就是"服务端等正文等到超时"）。
        assert_eq!(bytes.len(), text.find("\r\n\r\n").unwrap() + 4 + 7);
    }

    #[test]
    fn non_443_port_goes_into_the_host_header() {
        let u = url("https://gw.example:8443/v1/systemone");
        let text = String::from_utf8(build_request(&u, &[], b"")).unwrap();
        assert!(text.contains("Host: gw.example:8443\r\n"), "{text}");
        assert!(text.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn caller_supplied_headers_cannot_override_the_framing_headers() {
        let u = url("https://gw.example/v1");
        let text = String::from_utf8(build_request(
            &u,
            &[
                ("Host".into(), "evil.example".into()),
                ("Content-Length".into(), "999".into()),
                ("Connection".into(), "keep-alive".into()),
                ("accept".into(), "application/json".into()),
            ],
            b"x",
        ))
        .unwrap();
        assert!(!text.contains("evil.example"));
        assert!(text.contains("Content-Length: 1\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("accept: application/json\r\n"));
    }

    #[test]
    fn header_injection_is_dropped_not_written() {
        let u = url("https://gw.example/v1");
        let text = String::from_utf8(build_request(
            &u,
            &[("x-bad".into(), "a\r\nX-Injected: 1".into()), ("x-good".into(), "ok".into())],
            b"",
        ))
        .unwrap();
        assert!(!text.contains("X-Injected"), "头注入必须被丢掉：{text}");
        assert!(text.contains("x-good: ok"));
    }

    #[test]
    fn caller_can_set_its_own_user_agent() {
        let u = url("https://gw.example/v1");
        let text = String::from_utf8(build_request(&u, &[("User-Agent".into(), "xraytun/0.9".into())], b"")).unwrap();
        assert!(text.contains("User-Agent: xraytun/0.9\r\n"));
        assert_eq!(text.matches("User-Agent").count(), 1, "不许出现两个 UA：{text}");
    }

    #[test]
    fn parses_a_content_length_response() {
        // 注意 `Content-Length: 8` 与正文 `{"a": 1}` 必须**逐字节**一致 ——
        // 差一个字节就是"服务端还在等正文"或"客户端提前截断"。
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n{\"a\": 1}xxx";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"a\": 1}");
        assert!(!r.body.contains("xxx"), "Content-Length 之后的内容不该被算进正文");
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("CONTENT-TYPE"), Some("application/json"), "头名大小写不敏感");
    }

    #[test]
    fn parses_a_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"a\":\r\n2\r\n1}\r\n0\r\n\r\n";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, "{\"a\":1}");
    }

    #[test]
    fn chunked_with_extensions_and_uppercase_hex_works() {
        let payload = r#"{"model":"jev-latest","x":""}"#;
        // 长度用程序算 —— 手数十六进制是这类夹具最容易写错的地方（我第一版就数错了）。
        let raw = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:X};ext=1\r\n{}\r\n0\r\n\r\n",
            payload.len(),
            payload
        );
        let r = parse_response(raw.as_bytes()).unwrap();
        assert_eq!(r.body, payload);
    }

    #[test]
    fn falls_back_to_read_until_eof() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"answers\":{}}";
        let r = parse_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "{\"answers\":{}}");
    }

    #[test]
    fn a_429_with_a_json_error_body_parses_fine() {
        // 这是 Zen 免密钥档实测返回的形状。
        let body = r#"{"type":"error","error":{"type":"FreeUsageLimitError","message":"Rate limit exceeded. Please try again later."},"metadata":{}}"#;
        let raw = format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
        let r = parse_response(raw.as_bytes()).unwrap();
        assert_eq!(r.status, 429);
        assert!(r.body.contains("FreeUsageLimitError"));
    }

    #[test]
    fn malformed_inputs_are_errors_not_panics() {
        assert!(parse_response(b"").is_err());
        assert!(parse_response(b"NOT HTTP AT ALL\r\n\r\n{}").is_err());
        assert!(parse_response(b"HTTP/1.1 XYZ\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 999999999999\r\n\r\nshort").is_ok());
        // 坏掉的 chunked 不能 panic。
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\nx").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab").is_err());
    }

    #[test]
    fn a_huge_declared_length_cannot_blow_up_memory() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 100000000\r\n\r\nsmall";
        // 声明长度远大于实际内容时，只能按实际读到的返回（而不是分配 100MB）。
        let r = parse_response(raw).unwrap();
        assert_eq!(r.body, "small");
    }

    #[test]
    fn tls_transport_has_trust_anchors() {
        let t = TlsTransport::new();
        assert!(t.root_count() > 50, "根证书store 看起来是空的：{}", t.root_count());
        assert_eq!(t.describe(), "https(rustls)");
    }
}
