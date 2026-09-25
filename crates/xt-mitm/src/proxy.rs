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
//! * **响应体裁剪是"可选 + 有上限"的**：只有装了 [`BodyRewriter`] 才生效，
//!   且只处理 ≤ [`crate::rewrite::MAX_REWRITE_BYTES`]（64 KiB）的 JSON 响应。
//!   代价要如实说：为了裁剪，**这条路径先把整个响应读全再写回**，
//!   首字节延迟因此变差（对 opt-in 的少数域名才付这个成本，
//!   没装 rewriter 的 `serve` 也照样先把响应读全 —— 见 `exchange` 的注释）。
//! * body 不重压：请求一律**去掉 `Accept-Encoding`**（要求上游给未压缩体），
//!   否则裁剪的字节数与 `Content-Length` 又会打架。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::decide::{blocked_response, Decider, Decision};
use crate::http1::{head_end, remove_header, RequestHead, MAX_HEAD_BYTES};
use crate::rewrite::{
    apply_body_change, length_matches, BodyRewriter, DeclineReason,
};
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
    /// **回连时假定的目标端口**。
    ///
    /// `freedom.redirect` **不传递原始目标**，所以 MITM 只能自己假定一个端口，
    /// 默认 `443`（HTTPS 的常态）。做成旋钮的理由很具体：把 MITM 用在非 443 的
    /// 本地/开发服务上时，没有它就只能拆包到一个没人监听的端口。
    /// 生产（桌面）用默认值。
    pub assumed_port: u16,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        // 用**无失败可能**的构造：`"127.0.0.1:10810".parse().expect(...)` 虽然对
        // 字面量永远成立，但它把一条静态不变量写成了运行期 panic 点。
        Self {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 10810),
            upstream_socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 10811),
            io_timeout: Duration::from_secs(20),
            max_connections: 256,
            assumed_port: 443,
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
    /// 真的改了响应体的条数（裁剪生效）。
    pub body_rewritten: AtomicU64,
    /// 走到裁剪接缝但**没有改**的条数（原因见日志；必须可见，
    /// 否则"功能开着却一直不生效"没人发现）。
    pub body_rewrite_declined: AtomicU64,
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
            body_rewritten: self.body_rewritten.load(Ordering::Relaxed),
            body_rewrite_declined: self.body_rewrite_declined.load(Ordering::Relaxed),
        }
    }
}

/// 计数的只读快照。
///
/// 带 `Serialize`：桌面的 `mitm_status` 命令把它直接送给界面 ——
/// 这些数字是"MITM 到底干了什么"的唯一证据，中间不该再有第二份转写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ProxyStatsSnapshot {
    pub accepted: u64,
    pub blocked: u64,
    pub passed: u64,
    pub rejected_over_limit: u64,
    pub failed: u64,
    pub websocket_refused: u64,
    pub body_rewritten: u64,
    pub body_rewrite_declined: u64,
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

/// 起代理（**不装响应体裁剪**）。返回的 handle 一 drop 就停。
///
/// 只是 [`serve_with`] 的薄封装，为的是让"没有裁剪"这条路径在类型上就是默认的。
pub fn serve(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
) -> Result<ProxyHandle, TlsError> {
    serve_with(config, ca, decider, None)
}

/// 起代理，并（可选）装上响应体裁剪。
pub fn serve_with(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
    rewriter: Option<Arc<dyn BodyRewriter>>,
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
    let rw = rewriter.clone();

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
                    let (cfg, dec, sc, st, inf, rw) = (
                        cfg.clone(),
                        decider.clone(),
                        server_config.clone(),
                        st2.clone(),
                        inf2.clone(),
                        rw.clone(),
                    );
                    std::thread::spawn(move || {
                        handle_connection(sock, &cfg, &dec, sc, &st, rw.as_ref());
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
    rewriter: Option<&Arc<dyn BodyRewriter>>,
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
        match exchange(&mut tls, cfg, &head, rewriter, stats) {
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
    // 把「找到头结尾」的下标直接从循环里带出来：旧实现先 `is_some()` 判断、
    // 循环外再 `expect` 一次，等于在网络可达的路径上放了一个 panic 点。
    let end = loop {
        if let Some(e) = head_end(&buf) {
            break e;
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
    };
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
    rewriter: Option<&Arc<dyn BodyRewriter>>,
    stats: &Arc<ProxyStats>,
) -> std::io::Result<bool> {
    let host = head
        .host()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "请求没有 Host"))?;
    // **端口只能假设**：redirect 不传递原始目标（模块文档的硬约束）。
    // 默认 443，可用 `cfg.assumed_port` 覆盖（非 443 的 HTTPS 服务）。
    let mut upstream = socks_connect(cfg.upstream_socks, host, cfg.assumed_port, cfg.io_timeout)?;
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

    // ---- 响应体裁剪（装了才生效；没装则连内容都不看）----
    if let Some(rw) = rewriter {
        match apply_rewrite(&mut resp_head, &mut body, rw.as_ref(), host, head.path()) {
            None => {
                stats.body_rewritten.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(host = %host, path = %head.path(), "MITM：响应体裁剪已生效");
            }
            Some(reason) => {
                stats.body_rewrite_declined.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    host = %host,
                    path = %head.path(),
                    reason = reason.as_str(),
                    "MITM：响应体裁剪未生效（原样转发）"
                );
            }
        }
    }

    // 写回客户端：头 + body（长度保持一致）。
    //
    // `resp_head` **不含**尾随空行（见 `read_response` 的注释），所以这里补
    // `\r\n\r\n`：一个结束最后一行、一个就是那个空行。少补一个字节，
    // 客户端就找不到头/体分隔，症状是"读不到响应"而不是"读到错响应"。
    let mut out = resp_head.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n\r\n");
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

/// 把一次裁剪应用到 `(响应头行, body)` 上。
///
/// 返回 `None` = 真的改了；`Some(原因)` = 没改（原样转发）。
///
/// **所有失败路径都是放行。** 这里唯一不可接受的结果是"改了 body 但长度没改"，
/// 所以每条改动路径最后都过一遍 [`length_matches`]；自检不过就退回原 body。
///
/// 代价说明：为了让 `apply_body_change` 能重写 framing，改 body 时头块会被重排成
/// `名字: 值`（补一个空格）。这只发生在**我们真的改了 body** 的那条响应上，
/// 语义不变；没改的响应连头块都不动。
fn apply_rewrite(
    resp_head: &mut Vec<String>,
    body: &mut Vec<u8>,
    rewriter: &dyn BodyRewriter,
    host: &str,
    path: &str,
) -> Option<DeclineReason> {
    let Some(status) = resp_head.first().cloned() else {
        return Some(DeclineReason::NotJson);
    };
    let mut headers: Vec<(String, String)> = Vec::with_capacity(resp_head.len());
    // `skip(1)` 而不是 `&resp_head[1..]`：上面已经确认第一行存在，
    // 但切片下标是运行期 panic 点，`skip` 在类型上就没有越界这回事。
    for line in resp_head.iter().skip(1) {
        match line.split_once(':') {
            Some((k, v)) => headers.push((k.trim().to_string(), v.trim().to_string())),
            // 拆不动的畸形头：**放弃裁剪**，而不是去猜它想说什么。
            None => return Some(DeclineReason::NotJson),
        }
    }
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str());

    let changed = match rewriter.rewrite(host, path, content_type, body) {
        crate::rewrite::BodyRewrite::Changed(new) => new,
        crate::rewrite::BodyRewrite::Unchanged(reason) => return Some(reason),
    };
    if apply_body_change(&mut headers, &changed).is_err()
        || length_matches(&headers, &changed).is_err()
    {
        // 这是**我们自己的**不一致（不是对方数据的问题）：退回原 body 并单独计数。
        tracing::warn!(
            host = %host,
            path = %path,
            "MITM：裁剪后的长度自检没过，退回原响应体（不许发出长度对不上的响应）"
        );
        return Some(DeclineReason::FramingRefused);
    }
    let mut lines = Vec::with_capacity(headers.len() + 1);
    lines.push(status);
    lines.extend(headers.into_iter().map(|(k, v)| format!("{k}: {v}")));
    *resp_head = lines;
    *body = changed;
    None
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
    // `end` 是 `"\r\n\r\n"` 之后的下标，所以 [..end-4] 正好是"状态行 + 头字段"，
    // **不含**任何尾随 CRLF。用 `end - 2` 会多留一个 CRLF ⇒ `split` 出一个**尾随空行**，
    // 而原样转发路径（`join("\r\n")` 再补一个 CRLF）恰好会把它掩盖掉：
    // 字节是对的，但"头行列表"多一项。裁剪路径一见到没有冒号的行就会放弃 ——
    // 于是症状是"功能开着却一直不生效"。这里必须精确。
    let head_text = String::from_utf8_lossy(&buf[..end - 4]).to_string();
    let lines: Vec<String> = head_text.split("\r\n").map(str::to_string).collect();
    // 下面两条是**诊断**，不是正确性判据：畸形应答照样原样转发（或在上面的
    // chunked 检查里报错）。旧实现写成 `debug_assert!` —— debug 构建里一条
    // 对端发来的畸形应答就能 panic，而 release（`panic = "abort"`）里它又
    // 完全不存在。现在统一成 debug 日志：两个构建里行为一致，且不会 panic。
    if !lines.first().is_some_and(|l| l.starts_with("HTTP/")) {
        tracing::debug!("MITM：应答头第一行不是状态行，仍原样转发");
    }
    if lines.iter().any(String::is_empty) {
        tracing::debug!("MITM：应答头列表里出现空行（裁剪路径会因此放弃）");
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::{BodyRewrite, DeclineReason};
    use std::io::Cursor;

    /// 每次 `read` 只吐 `step` 字节的读端：用来验证"头跨多次 read 到达"。
    struct ChunkyReader {
        data: Vec<u8>,
        pos: usize,
        step: usize,
    }

    impl Read for ChunkyReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = self.step.min(out.len()).min(self.data.len() - self.pos);
            out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn chunky(data: &[u8], step: usize) -> ChunkyReader {
        ChunkyReader { data: data.to_vec(), pos: 0, step }
    }

    /// 旧的 `Default` 实现是 `"127.0.0.1:10810".parse().expect(...)`。
    /// 现在用无失败构造，值必须**完全相同**。
    #[test]
    fn default_config_is_loopback_and_unchanged() {
        let c = ProxyConfig::default();
        assert_eq!(c.listen, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 10810));
        assert_eq!(c.upstream_socks, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 10811));
        assert_eq!(c.assumed_port, 443);
    }

    /// 判别性：头分 3 次到达时也要解析出来（旧实现在循环外 `expect` 一次
    /// `head_end`，这条路径正是那个 panic 点的入口）。
    #[test]
    fn a_head_split_across_reads_is_parsed() {
        let raw = b"GET /a HTTP/1.1\r\nHost: news.example\r\n\r\n";
        let head = read_head(&mut chunky(raw, 3)).expect("不该报错").expect("应当解析出请求头");
        assert_eq!(head.method, "GET");
        assert_eq!(head.host(), Some("news.example"));
    }

    /// 客户端在请求边界前关连接 = 正常结束，**不是**错误、更不是 panic。
    #[test]
    fn eof_before_any_head_byte_is_a_clean_end() {
        assert!(matches!(read_head(&mut std::io::empty()), Ok(None)));
    }

    /// 头读到一半就 EOF：明确报错（旧路径也会在 `expect` 之前返回，这条
    /// 断言钉住"不许把半截头当成功"）。
    #[test]
    fn a_partial_head_reports_eof_instead_of_passing() {
        let err = read_head(&mut chunky(b"GET / HTTP/1.1\r\nHost: a\r\n", 8)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// 判别性：应答头**不是**状态行时，不许 panic（旧实现是 `debug_assert!`，
    /// 在 debug 构建/测试里会炸），而是原样把这一行当状态行返回。
    #[test]
    fn a_non_http_response_head_does_not_panic() {
        let raw = b"NOT-HTTP 200 OK\r\nContent-Length: 0\r\n\r\n";
        let (lines, body) = read_response(&mut Cursor::new(raw.to_vec())).expect("应当原样接受");
        assert_eq!(lines.first().map(String::as_str), Some("NOT-HTTP 200 OK"));
        assert!(body.is_empty());
    }

    /// 空输入必须报错，不许 panic。
    #[test]
    fn an_empty_response_reports_eof() {
        let err = read_response(&mut std::io::empty()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// 正常应答：状态行 + 头 + body 按 `Content-Length` 精确切出。
    #[test]
    fn a_normal_response_is_split_into_head_lines_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let (lines, body) = read_response(&mut Cursor::new(raw.to_vec())).unwrap();
        assert_eq!(lines.first().map(String::as_str), Some("HTTP/1.1 200 OK"));
        assert_eq!(lines.len(), 3, "尾随空行不许进列表");
        assert_eq!(body, b"{\"a\":1}");
    }

    /// chunked 明确拒绝（本版不支持），而不是把它当 Content-Length=0 放过去。
    #[test]
    fn a_chunked_response_is_refused_explicitly() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let err = read_response(&mut Cursor::new(raw.to_vec())).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported, "{err}");
    }

    struct NoopRewriter;

    impl crate::rewrite::BodyRewriter for NoopRewriter {
        fn rewrite(
            &self,
            _host: &str,
            _path: &str,
            _content_type: Option<&str>,
            _body: &[u8],
        ) -> BodyRewrite {
            BodyRewrite::Unchanged(DeclineReason::NotJson)
        }
    }

    /// 空头块走裁剪接缝时必须**放弃裁剪**（fail-open：原样转发），不许 panic。
    #[test]
    fn rewriting_an_empty_head_declines_instead_of_panicking() {
        let mut head: Vec<String> = Vec::new();
        let mut body: Vec<u8> = Vec::new();
        let got = apply_rewrite(&mut head, &mut body, &NoopRewriter, "h.example", "/");
        assert_eq!(got, Some(DeclineReason::NotJson));
    }

    /// 只有状态行、没有头字段：`skip(1)` 路径不许越界。
    #[test]
    fn rewriting_a_head_with_only_a_status_line_declines() {
        let mut head = vec!["HTTP/1.1 200 OK".to_string()];
        let mut body = b"{}".to_vec();
        let got = apply_rewrite(&mut head, &mut body, &NoopRewriter, "h.example", "/");
        assert_eq!(got, Some(DeclineReason::NotJson));
    }
}
