//! MITM 代理循环：**终结 TLS → 判定 → 阻断 或 经 `mitm-upstream` 转发**。
//!
//! # 阻塞 IO + 每连接一个线程（刻意不用 async）
//!
//! opt-in 域名的并发是**几十量级**，而 async TLS 适配（`tokio-rustls`）会再引一层依赖
//! 与一套生命周期。这里用标准库线程 + [`rustls::StreamOwned`]，与 `xt-intent` 的传输层
//! 同一取舍：**能读懂 > 极致吞吐**。
//!
//! # 为什么是"请求/应答"而不是盲搬运
//!
//! 盲搬运是最简单的代理，但它**没有响应体裁剪的挂点** —— 而裁掉同域广告条目
//! （时间线接口里的推广条目）正是 MITM 相对域名层的唯一增量。所以这里按
//! HTTP/1.1 的 **请求 → 应答** 语义走：读完整请求、转发、读完整响应、**再决定要不要裁**、
//! 最后写回客户端。每一步都有明确边界，也都能被测试单独盯住。
//!
//! # 回连为什么必须走 `mitm-upstream`
//!
//! steer 进来的连接已经丢了原始目标（`freedom.redirect` **不传递它**），
//! 所以回连只能靠 MITM 自己还原主机名，而**端口只能假设 443**。这是数据面的硬约束。
//! 回连走 `mitm-upstream` 那个 socks 入站 ⇒ 它的 `inboundTag` 不在 steer 规则里
//! ⇒ **构造上不可能自环**。
//!
//! # 本版的已知限制（写在这里，不藏）
//!
//! * **WebSocket 升级不支持**：双向长期搬运需要非阻塞手动泵 TLS 记录，
//!   本版直接回 `501` 并计入 `failed`。缓解：**opt-in 名单里不要放 WebSocket 端点**。
//!   修法明确（把 `rustls::ServerConnection` 拿在手里手动 `read_tls`/`write_tls`），
//!   但那是独立一步。
//! * **响应体裁剪尚未接进这条循环**（[`crate::rewrite`] 已实现并单测）。
//!   接进来要先定"缓冲上限"，而缓冲会改变首字节延迟 —— 单独一步做、单独验。
//! * body 不重压：请求一律**去掉 `Accept-Encoding`**（要求上游给未压缩体），
//!   否则裁剪的字节数与 `Content-Length` 又会打架。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::decide::{blocked_response, Decider, Decision};
use crate::http1::{head_end, remove_header, RequestHead, MAX_HEAD_BYTES};
use crate::tls::{CertResolver, LocalCa, TlsError};

/// 单个应答体的缓冲上限（超过就**不裁**、原样转发）。
///
/// 12 MiB：足够覆盖时间线接口，又不至于让一个恶意/异常响应吃掉内存。
pub const MAX_BODY_BYTES: usize = 12 * 1024 * 1024;

/// 代理配置。
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// 监听地址（生产是 `127.0.0.1:<mitm.listen_port>`）。
    pub listen: SocketAddr,
    /// 回连用的 socks 入站地址（`127.0.0.1:<mitm.upstream_port>`）。
    pub upstream_socks: SocketAddr,
    /// 读写的空闲超时。**必须设**：否则一个卡住的连接会永久占住一个线程。
    pub io_timeout: Duration,
    /// 同时处理的连接上限（线程数上限）。超了直接关连接 ——
    /// 比"无限开线程"安全（一个页面能轻易开出几百条连接）。
    pub max_connections: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:10810".parse().expect("静态地址"),
            upstream_socks: "127.0.0.1:10811".parse().expect("静态地址"),
            io_timeout: Duration::from_secs(20),
            max_connections: 256,
        }
    }
}

/// 代理的运行计数（诊断 + 测试判据）。
#[derive(Debug, Default)]
pub struct ProxyStats {
    pub accepted: AtomicU64,
    pub blocked: AtomicU64,
    /// 放行并成功完成一次请求/应答的条数。
    pub passed: AtomicU64,
    /// 因为超过连接上限被拒的条数。
    pub rejected_over_limit: AtomicU64,
    /// 握手/解析/转发失败的条数（**必须可见**：静默失败最难查）。
    pub failed: AtomicU64,
    /// 遇到 WebSocket 升级而被拒的条数（本版的已知限制）。
    pub websocket_refused: AtomicU64,
}

impl ProxyStats {
    pub fn snapshot(&self) -> ProxyStatsSnapshot {
        ProxyStatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            blocked: self.blocked.load(Ordering::Relaxed),
            passed: self.passed.load(Ordering::Relaxed),
            rejected_over_limit: self.rejected_over_limit.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            websocket_refused: self.websocket_refused.load(Ordering::Relaxed),
        }
    }
}

/// 计数的只读快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyStatsSnapshot {
    pub accepted: u64,
    pub blocked: u64,
    pub passed: u64,
    pub rejected_over_limit: u64,
    pub failed: u64,
    pub websocket_refused: u64,
}

/// 正在运行的代理。`Drop` 停掉接受循环并等它退出（**不留后台线程**）。
pub struct ProxyHandle {
    pub listen: SocketAddr,
    stats: Arc<ProxyStats>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ProxyHandle {
    pub fn stats(&self) -> ProxyStatsSnapshot {
        self.stats.snapshot()
    }

    /// 等到达 `want` 条"放行完成"或超时（测试用：避免 sleep 猜时间）。
    pub fn wait_passed_at_least(&self, want: u64, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.stats().passed >= want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 起代理。返回的 handle 一 drop 就停。
pub fn serve(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
) -> Result<ProxyHandle, TlsError> {
    let listener = TcpListener::bind(config.listen)
        .map_err(|e| TlsError::Config(format!("绑定 {} 失败: {e}", config.listen)))?;
    let listen = listener
        .local_addr()
        .map_err(|e| TlsError::Config(e.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| TlsError::Config(e.to_string()))?;

    // 整条代理只有一个 ServerConfig：证书由 resolver 按 SNI 现签。
    let resolver = Arc::new(CertResolver::new(ca.clone()));
    let server_config = ca.server_config_with_resolver(resolver)?;

    let stats = Arc::new(ProxyStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let inflight = Arc::new(AtomicU64::new(0));
    let (s2, st2, inf2) = (stop.clone(), stats.clone(), inflight.clone());
    let cfg = config.clone();

    let join = std::thread::spawn(move || {
        while !s2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((sock, _)) => {
                    // ⚠️ **必须显式设回阻塞模式**：监听套接字是非阻塞的（这样停止循环
                    // 不用等 accept），而在 macOS/Linux 上 `accept()` 返回的**已连接**
                    // 套接字会继承非阻塞 —— 于是 TLS 读立刻 WouldBlock，
                    // 代理什么都不写就关连接（症状是客户端收到"peer closed without
                    // close_notify"，和"证书不对/握手失败"很像，但完全不是一回事）。
                    // 而且 `set_read_timeout` 对非阻塞套接字是**无效**的（超时会被忽略）。
                    let _ = sock.set_nonblocking(false);
                    st2.accepted.fetch_add(1, Ordering::Relaxed);
                    if inf2.load(Ordering::Relaxed) as usize >= cfg.max_connections {
                        st2.rejected_over_limit.fetch_add(1, Ordering::Relaxed);
                        drop(sock); // 直接关：比排一个无界队列更安全
                        continue;
                    }
                    inf2.fetch_add(1, Ordering::Relaxed);
                    let (cfg, dec, sc, st, inf) =
                        (cfg.clone(), decider.clone(), server_config.clone(), st2.clone(), inf2.clone());
                    std::thread::spawn(move || {
                        handle_connection(sock, &cfg, &dec, sc, &st);
                        inf.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "MITM 接受连接失败");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });

    Ok(ProxyHandle { listen, stats, stop, join: Some(join) })
}

/// 一条连接的全过程（TLS 终结 → 请求/应答循环）。
fn handle_connection(
    sock: TcpStream,
    cfg: &ProxyConfig,
    decider: &Arc<dyn Decider>,
    server_config: Arc<rustls::ServerConfig>,
    stats: &Arc<ProxyStats>,
) {
    let _ = sock.set_read_timeout(Some(cfg.io_timeout));
    let _ = sock.set_write_timeout(Some(cfg.io_timeout));
    let _ = sock.set_nodelay(true);

    let conn = match rustls::ServerConnection::new(server_config) {
        Ok(c) => c,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %e, "创建 TLS 会话失败");
            return;
        }
    };
    let mut tls = rustls::StreamOwned::new(conn, sock);

    // **keep-alive 循环**：一个 TLS 连接上可以有多条请求（HTTP/1.1 默认）。
    //
    // 用带标签的 `break` 而不是 `return`：所有退出路径都必须走到函数末尾去发
    // `close_notify`（原因见末尾注释）。
    'conn: loop {
        let head = match read_head(&mut tls) {
            Ok(Some(h)) => h,
            Ok(None) => break 'conn, // 客户端正常关闭
            Err(e) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(error = %e, "MITM：读请求头失败");
                break 'conn;
            }
        };

        // ---- WebSocket：本版**明确不支持**（已知限制，见模块文档）----
        if head.is_websocket_upgrade() {
            stats.websocket_refused.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                host = ?head.host(),
                "MITM：收到 WebSocket 升级请求，本版不支持（回 501）——不要把它放进 opt-in 名单"
            );
            let _ = tls.write_all(
                b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = tls.flush();
            break 'conn;
        }

        // ---- 判定 ----
        match decider.decide(&head) {
            Decision::Block { reason } => {
                stats.blocked.fetch_add(1, Ordering::Relaxed);
                tracing::info!(host = ?head.host(), path = %head.path(), %reason, "MITM：阻断");
                let _ = tls.write_all(&blocked_response(&reason));
                let _ = tls.flush();
                break 'conn; // 阻断后直接关连接（不保持 keep-alive：我们不想再陪它聊）
            }
            Decision::Pass => {}
        }

        // ---- 转发一次请求/应答 ----
        match exchange(&mut tls, cfg, &head) {
            Ok(close) => {
                stats.passed.fetch_add(1, Ordering::Relaxed);
                if close {
                    break 'conn;
                }
            }
            Err(e) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(error = %e, "MITM：转发失败");
                break 'conn;
            }
        }
    }

    // **必须显式发 `close_notify`**：`rustls` 的 `StreamOwned` 在 `Drop` 时只是
    // 关掉 TCP，**不会**发 TLS 关闭通知。少了它，客户端看到的是
    // "unexpected EOF / 连接像被截断" —— OpenSSL 默认会报
    // `unexpected eof while reading`，严格的应用会把这个当成**请求失败**而不是
    // "响应正常结束"。对一个要插在真实 App 前面的透明代理，这是必须付的成本
    // （只有 5 字节）。
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// 读一个完整的请求头（**不读 body**；本版只处理无 body 的请求形态）。
///
/// 返回 `Ok(None)` = 客户端在请求边界前关闭（正常结束）。
fn read_head<S: Read>(tls: &mut S) -> std::io::Result<Option<RequestHead>> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if head_end(&buf).is_some() {
            break;
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "请求头超过上限",
            ));
        }
        match tls.read(&mut chunk) {
            Ok(0) if buf.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "头没读完就 EOF",
                ))
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(e),
        }
    }
    let end = head_end(&buf).expect("上面已经确认存在");
    let head = RequestHead::parse(&buf[..end])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    // 本版只支持**没有 body** 的请求形态（GET/HEAD 之类）。带 body 的请求
    // （POST/PUT）如果被 steer 到这里，我们不猜它的长度 —— 直接明确拒绝，
    // 而不是发一个半截请求给上游（那会让上游挂在那里等 body）。
    if let Some(len) = head.header("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
        if len > 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("本版不处理带 body 的请求（Content-Length: {len}）"),
            ));
        }
    }
    Ok(Some(head))
}

/// 转发一次请求并读回应答，写回客户端。返回 `true` 表示"该关连接了"。
fn exchange<S: Read + Write>(
    tls: &mut S,
    cfg: &ProxyConfig,
    head: &RequestHead,
) -> std::io::Result<bool> {
    let host = head
        .host()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "请求没有 Host"))?;
    // **端口只能假设 443**：redirect 不传递原始目标（模块文档的硬约束）。
    let mut upstream = socks_connect(cfg.upstream_socks, host, 443, cfg.io_timeout)?;
    upstream.set_read_timeout(Some(cfg.io_timeout))?;
    upstream.set_write_timeout(Some(cfg.io_timeout))?;

    // 重新序列化请求头，并**去掉 Accept-Encoding**：
    // 我们要的是未压缩的响应体（否则裁剪会与 Content-Length 打架），
    // 同时把 `Connection: close` 写死 —— 本版一次连接一条请求，语义最简单也最安全。
    let mut forwarded = head.clone();
    remove_header(&mut forwarded.headers, "accept-encoding");
    remove_header(&mut forwarded.headers, "connection");
    forwarded.headers.push(("Connection".to_string(), "close".to_string()));
    upstream.write_all(&forwarded.to_bytes())?;
    upstream.flush()?;

    // 读应答头 → 读应答体（按 Content-Length；chunked 本版不支持，明确报错）。
    let (mut resp_head, mut body) = read_response(&mut upstream)?;
    tracing::debug!(
        host = %host,
        path = %head.path(),
        status = resp_head.first().map(String::as_str).unwrap_or(""),
        body_len = body.len(),
        "MITM：放行"
    );

    // 写回客户端：头 + body（长度保持一致；body 原样，裁剪挂点在下一步）。
    let mut out = resp_head.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n");
    tls.write_all(&out)?;
    if !body.is_empty() {
        tls.write_all(&body)?;
    }
    tls.flush()?;
    // 上游是 `Connection: close`，所以我们也让客户端关掉。
    let _ = &mut resp_head;
    body.clear();
    Ok(true)
}

/// 读一个应答：返回 `(头行, 体)`。**只支持 Content-Length**（本版）。
fn read_response<S: Read>(upstream: &mut S) -> std::io::Result<(Vec<String>, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(e) = head_end(&buf) {
            break e;
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "应答头超过上限"));
        }
        let n = upstream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "应答头没读完"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head_text = String::from_utf8_lossy(&buf[..end - 2]).to_string();
    let lines: Vec<String> = head_text.split("\r\n").map(str::to_string).collect();

    let declared = lines
        .iter()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        });
    if lines.iter().any(|l| {
        l.split_once(':')
            .map(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
            .unwrap_or(false)
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "本版不支持 chunked 应答（透明转发 chunked 需要另一套 framing 处理）",
        ));
    }
    let want = declared.unwrap_or(0);
    if want > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("应答体 {want} 字节超过上限 {MAX_BODY_BYTES}"),
        ));
    }
    let mut body = buf[end..].to_vec();
    while body.len() < want {
        let n = upstream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > MAX_BODY_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "应答体超过上限"));
        }
    }
    body.truncate(want);
    Ok((lines, body))
}

/// 手写 SOCKS5 CONNECT（无认证）。与 `xt-intent`/测试里那份同形。
fn socks_connect(
    proxy: SocketAddr,
    host: &str,
    port: u16,
    timeout: Duration,
) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect_timeout(&proxy, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(&[5, 1, 0])?;
    let mut hello = [0u8; 2];
    s.read_exact(&mut hello)?;
    if hello != [5, 0] {
        return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "SOCKS5 握手失败"));
    }
    if host.is_empty() || host.len() > 255 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "主机名长度不合法"));
    }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head)?;
    if head[1] != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 拒绝：reply={}", head[1]),
        ));
    }
    match head[3] {
        1 => {
            let mut b = [0u8; 6];
            s.read_exact(&mut b)?;
        }
        4 => {
            let mut b = [0u8; 18];
            s.read_exact(&mut b)?;
        }
        _ => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l)?;
            let mut b = vec![0u8; l[0] as usize + 2];
            s.read_exact(&mut b)?;
        }
    }
    Ok(s)
}
