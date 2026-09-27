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
//! # 请求/应答的 framing（本版支持到哪）
//!
//! * **请求体**：`Content-Length` 与 `Transfer-Encoding: chunked` 都支持；body
//!   **逐字节原样**转发（chunked 连分块框架一起原样转发，不重新编码）。
//! * **响应体**：`Content-Length`、`chunked`（读到终止 chunk 为止）、以及
//!   "既没有 CL 也没有 TE"（读到上游关闭）三种都支持。原样转发时**一个字节都不动**。
//! * body 不重压：请求一律**去掉 `Accept-Encoding`**（要求上游给未压缩体），
//!   否则既没法看广告标记、裁剪的字节数与 `Content-Length` 又会打架。
//!
//! # 「只观察、不改写」模式（默认关闭，按域名 opt-in）
//!
//! 装了 [`Observer`] 的代理会为每条交换写一条**摘要**（见 [`crate::observe`]）：
//! host / method / path / status / content-type / body 字节数 / 标记词命中计数 / 短哈希。
//! 它**不改写任何字节**，`apply_rewrite` 那条路径的行为一点没动；
//! 观察看到的永远是**上游原样**的响应体。隐私取舍与"看不到什么"写在
//! [`crate::observe`] 的模块文档里，那里才是准的。
//!
//! # 本版的已知限制（写在这里，不藏）
//!
//! * **WebSocket 升级不支持**：双向长期搬运需要非阻塞手动泵 TLS 记录，
//!   本版直接回 `501`，**但会计入 `websocket_refused` 并写 `warn` 日志**（不许静默）。
//!   缓解：**opt-in 名单里不要放 WebSocket 端点**。
//!   修法明确（把 `rustls::ServerConnection` 拿在手里手动 `read_tls`/`write_tls`），
//!   但那是独立一步。
//! * **响应体裁剪是"可选 + 有上限"的**：只有装了 [`BodyRewriter`] 才生效，
//!   且只处理 ≤ [`crate::rewrite::MAX_REWRITE_BYTES`]（64 KiB）的 JSON 响应。
//!   代价要如实说：为了裁剪，**这条路径先把整个响应读全再写回**，
//!   首字节延迟因此变差（对 opt-in 的少数域名才付这个成本，
//!   没装 rewriter 的 `serve` 也照样先把响应读全 —— 见 `exchange` 的注释）。
//! * **body 有上限**：请求体/响应体超过 [`MAX_BODY_BYTES`] 时**明确关闭连接**
//!   （fail-open：宁可断开，也不发半截请求或截断的响应）。
//! * **`Expect: 100-continue` 不代传**：客户端可能在等 100 时超时。已知盲点。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::decide::{blocked_response, Decider, Decision};
use crate::http1::{head_end, remove_header, RequestHead, MAX_HEAD_BYTES};
use crate::observe::{ExchangeMeta, Observer};
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
    /// 真的被观察并写了摘要的交换条数（"观察模式到底看到了多少条"）。
    pub observed: AtomicU64,
    /// 观察到的**标记词命中总数**（采数据的产出量指标）。
    pub observed_marker_hits: AtomicU64,
    /// 成功转发的**带 body 请求**条数。
    pub request_bodies_forwarded: AtomicU64,
    /// 原样透传的 **chunked 响应**条数。
    pub chunked_responses: AtomicU64,
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
            observed: self.observed.load(Ordering::Relaxed),
            observed_marker_hits: self.observed_marker_hits.load(Ordering::Relaxed),
            request_bodies_forwarded: self.request_bodies_forwarded.load(Ordering::Relaxed),
            chunked_responses: self.chunked_responses.load(Ordering::Relaxed),
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
    /// 观察模式：写入摘要的交换条数。
    pub observed: u64,
    /// 观察模式：标记词命中总数。
    pub observed_marker_hits: u64,
    /// 带 body 的请求被成功转发的条数。
    pub request_bodies_forwarded: u64,
    /// chunked 响应被原样透传的条数。
    pub chunked_responses: u64,
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

/// 起代理（**不装响应体裁剪、不观察**）。返回的 handle 一 drop 就停。
///
/// 只是 [`serve_with`] 的薄封装，为的是让"没有裁剪、没有观察"这条路径在类型上就是默认的。
pub fn serve(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
) -> Result<ProxyHandle, TlsError> {
    serve_full(config, ca, decider, None, None)
}

/// 起代理，并（可选）装上响应体裁剪。
pub fn serve_with(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
    rewriter: Option<Arc<dyn BodyRewriter>>,
) -> Result<ProxyHandle, TlsError> {
    serve_full(config, ca, decider, rewriter, None)
}

/// 起代理，并装上**观察者**（只观察、不改写；默认关闭由观察者自己保证）。
///
/// 这是"数据采集"那条路：传 [`crate::observe::DomainObserver`] 就是产品行为，
/// 传自定义 [`Observer`] 就是测试/实验。要同时裁剪用 [`serve_with_observer`]。
pub fn serve_observing(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
    observer: Arc<dyn Observer>,
) -> Result<ProxyHandle, TlsError> {
    serve_full(config, ca, decider, None, Some(observer))
}

/// 起代理：**同时**装响应体裁剪与观察者。
///
/// 观察看到的是**上游原样**的响应体（在 `apply_rewrite` 之前汇总），
/// 而写回客户端的是裁剪后的结果 —— 两者刻意分开，免得"观察"被裁剪污染。
pub fn serve_with_observer(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
    rewriter: Option<Arc<dyn BodyRewriter>>,
    observer: Arc<dyn Observer>,
) -> Result<ProxyHandle, TlsError> {
    serve_full(config, ca, decider, rewriter, Some(observer))
}

/// 所有 `serve*` 的唯一实现（外部签名保持兼容，别在 `serve_with` 上加参数）。
fn serve_full(
    config: ProxyConfig,
    ca: Arc<LocalCa>,
    decider: Arc<dyn Decider>,
    rewriter: Option<Arc<dyn BodyRewriter>>,
    observer: Option<Arc<dyn Observer>>,
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
    let obs = observer.clone();

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
                    let (cfg, dec, sc, st, inf, rw, obs) = (
                        cfg.clone(),
                        decider.clone(),
                        server_config.clone(),
                        st2.clone(),
                        inf2.clone(),
                        rw.clone(),
                        obs.clone(),
                    );
                    std::thread::spawn(move || {
                        handle_connection(sock, &cfg, &dec, sc, &st, rw.as_ref(), obs.as_ref());
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
    observer: Option<&Arc<dyn Observer>>,
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
        // `leftover` = 已经读进缓冲、但属于 body（或流水线的下一请求）的字节。
        // 丢掉它就等于把 body 前缀吃掉 —— 这是"带 body 的请求"最容易写错的地方。
        let (head, leftover) = match read_head(&mut tls) {
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
                path = %head.path(),
                total_refused = stats.websocket_refused.load(Ordering::Relaxed),
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

        // ---- 读请求体（`Content-Length` / `chunked`；**逐字节原样**持有）----
        //
        // 放在判定之后：被阻断/被拒的请求不需要为 body 付内存与时间。
        // 读失败**明确关闭**（fail-open 的"明确关闭"那一支）：绝不把半截请求发给上游。
        let request_body = match read_request_body(&mut tls, &head, leftover) {
            Ok(b) => b,
            Err(e) => {
                stats.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    error = %e,
                    host = ?head.host(),
                    path = %head.path(),
                    "MITM：读请求体失败，明确关闭连接（不许发半截请求给上游）"
                );
                break 'conn;
            }
        };

        // ---- 转发一次请求/应答 ----
        match exchange(&mut tls, cfg, &head, &request_body, rewriter, observer, stats) {
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

/// 读一个完整的请求头，并**把它后面已经读进来的字节一并交出来**。
///
/// 返回 `Ok(None)` = 客户端在请求边界前关闭（正常结束）。
///
/// `leftover` 是本函数读头时"读多了"的字节 —— 对带 body 的请求，它通常就是 body 的
/// 开头。**丢掉它 = 吃掉 body 前缀**，这是这类代理最经典的 bug，所以类型上强制调用方接住。
fn read_head<S: Read>(tls: &mut S) -> std::io::Result<Option<(RequestHead, Vec<u8>)>> {
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
    let leftover = buf[end..].to_vec();
    Ok(Some((head, leftover)))
}

/// 按请求头里的 framing 读出**完整请求体**，返回逐字节原样的字节。
///
/// * `Transfer-Encoding: chunked` ⇒ 连分块框架一起原样持有（不重新编码）；
/// * `Content-Length: N` ⇒ 精确读 N 字节；
/// * 两者都没有 ⇒ 没有 body（`leftover` 里若还有字节，那是不支持的流水线，只记日志）。
///
/// 超过 [`MAX_BODY_BYTES`] ⇒ 明确报错，调用方关连接（fail-open：宁可断开也不发半截请求）。
fn read_request_body<S: Read>(
    tls: &mut S,
    head: &RequestHead,
    leftover: Vec<u8>,
) -> std::io::Result<Vec<u8>> {
    let chunked = head.headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding")
            && v.to_ascii_lowercase().contains("chunked")
    });
    if chunked {
        let (raw, _payload) = read_chunked(tls, leftover)?;
        return Ok(raw);
    }
    if let Some(want) = head
        .header("content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if want > MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("请求体 {want} 字节超过上限 {MAX_BODY_BYTES}"),
            ));
        }
        return read_exact_body(tls, leftover, want);
    }
    if !leftover.is_empty() {
        // 没有 framing 却读到了多余字节：要么是流水线的下一请求，要么是对端不守规矩。
        // 本版一次连接只处理一条请求，直接丢弃并留痕（不静默）。
        tracing::debug!(
            bytes = leftover.len(),
            "MITM：无 body 的请求后仍有多余字节，已丢弃（本版不支持流水线）"
        );
    }
    Ok(Vec::new())
}

/// 转发一次请求并读回应答，写回客户端。返回 `true` 表示"该关连接了"。
fn exchange<S: Read + Write>(
    tls: &mut S,
    cfg: &ProxyConfig,
    head: &RequestHead,
    request_body: &[u8],
    rewriter: Option<&Arc<dyn BodyRewriter>>,
    observer: Option<&Arc<dyn Observer>>,
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
    // 我们要的是未压缩的响应体（否则既搜不到广告标记、裁剪也会与 Content-Length 打架），
    // 同时把 `Connection: close` 写死 —— 本版一次连接一条请求，语义最简单也最安全。
    //
    // **body 不动**：`request_body` 是逐字节原样的字节（chunked 连框架一起），
    // 紧跟在头后面原样写出。
    let mut forwarded = head.clone();
    remove_header(&mut forwarded.headers, "accept-encoding");
    remove_header(&mut forwarded.headers, "connection");
    forwarded.headers.push(("Connection".to_string(), "close".to_string()));
    upstream.write_all(&forwarded.to_bytes())?;
    if !request_body.is_empty() {
        upstream.write_all(request_body)?;
        stats.request_bodies_forwarded.fetch_add(1, Ordering::Relaxed);
    }
    upstream.flush()?;

    // 读应答头 → 读应答体（`Content-Length` / `chunked` / 读到关闭三种 framing）。
    let resp = read_response(&mut upstream)?;
    if resp.chunked {
        stats.chunked_responses.fetch_add(1, Ordering::Relaxed);
    }
    tracing::debug!(
        host = %host,
        path = %head.path(),
        status = %resp.status_line,
        body_len = resp.payload.len(),
        chunked = resp.chunked,
        "MITM：放行"
    );

    // ---- 观察（**任何改写之前**：看的永远是上游原样的内容）----
    //
    // 只有观察者点名了这个域名才做汇总；`observe` 里的失败绝不影响转发。
    if let Some(obs) = observer {
        if obs.observes(host) {
            let meta = ExchangeMeta {
                host: host.to_string(),
                method: head.method.clone(),
                path: crate::observe::sanitize_path(head.path()),
                status: resp.status,
                status_line: resp.status_line.clone(),
                content_type: resp.content_type.clone(),
                body_bytes: resp.payload.len(),
                request_body_bytes: request_body.len(),
            };
            let record = crate::observe::summarize(&meta, &resp.payload, obs.markers());
            stats.observed.fetch_add(1, Ordering::Relaxed);
            stats
                .observed_marker_hits
                .fetch_add(record.marker_total as u64, Ordering::Relaxed);
            obs.observe(&record, &resp.payload);
        }
    }

    // ---- 决定写回客户端的 (头行, body) ----
    //
    // **没装 rewriter 时不碰任何字节**：原样写回头与 `raw`（chunked 连框架一起）。
    // 装了 rewriter 时：
    //   * 真改了 ⇒ 用改后的头 + 载荷（`apply_body_change` 会去掉 TE、写上精确 CL）；
    //   * 没改 ⇒ 回到**上游原样**的 `raw`（chunked 仍然 chunked）。
    let Response { lines, raw, payload, .. } = resp;
    let (out_lines, out_body) = match rewriter {
        Some(rw) => {
            let mut head_lines = lines.clone();
            let mut body = payload;
            match apply_rewrite(&mut head_lines, &mut body, rw.as_ref(), host, head.path()) {
                None => {
                    stats.body_rewritten.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(host = %host, path = %head.path(), "MITM：响应体裁剪已生效");
                    (head_lines, body)
                }
                Some(reason) => {
                    stats.body_rewrite_declined.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(
                        host = %host,
                        path = %head.path(),
                        reason = reason.as_str(),
                        "MITM：响应体裁剪未生效（原样转发）"
                    );
                    (lines, raw)
                }
            }
        }
        // 只观察（或纯放行）：**一个字节都不改**。
        None => (lines, raw),
    };

    // 写回客户端：头 + body。
    //
    // 头行列表**不含**尾随空行（见 `read_response` 的注释），所以这里补
    // `\r\n\r\n`：一个结束最后一行、一个就是那个空行。少补一个字节，
    // 客户端就找不到头/体分隔，症状是"读不到响应"而不是"读到错响应"。
    let mut out = out_lines.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n\r\n");
    tls.write_all(&out)?;
    if !out_body.is_empty() {
        tls.write_all(&out_body)?;
    }
    tls.flush()?;
    // 上游是 `Connection: close`，所以我们也让客户端关掉。
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

/// 一条应答的读入结果。
///
/// `raw` 是**要原样写回客户端**的体字节（chunked 时含分块框架），
/// `payload` 是去掉 chunked 框架后的载荷（观察/裁剪用；非 chunked 时两者相同）。
#[derive(Debug)]
struct Response {
    /// 状态行 + 头字段（**不含**尾随 CRLF）。
    lines: Vec<String>,
    /// 解析出的状态码（解析不出来是 0）。
    status: u16,
    /// 原样状态行。
    status_line: String,
    content_type: Option<String>,
    raw: Vec<u8>,
    payload: Vec<u8>,
    chunked: bool,
}

/// 读一个应答：头 + 体。**三种 framing 都支持**：
///
/// * `Transfer-Encoding: chunked` ⇒ 读到终止 chunk（含 trailer）为止，`raw` 原样保留；
/// * `Content-Length: N` ⇒ 精确读 N 字节；
/// * 都没有 ⇒ 读到上游关闭（我们强制了 `Connection: close`）。
///
/// 无 body 的状态码（1xx/204/304）直接当空体，免得为一个没有体的响应等 EOF。
fn read_response<S: Read>(upstream: &mut S) -> std::io::Result<Response> {
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
    // 下面两条是**诊断**，不是正确性判据：畸形应答照样原样转发。
    // 旧实现写成 `debug_assert!` —— debug 构建里一条对端发来的畸形应答就能 panic，
    // 而 release（`panic = "abort"`）里它又完全不存在。现在统一成 debug 日志：
    // 两个构建里行为一致，且不会 panic。
    if !lines.first().is_some_and(|l| l.starts_with("HTTP/")) {
        tracing::debug!("MITM：应答头第一行不是状态行，仍原样转发");
    }
    if lines.iter().any(String::is_empty) {
        tracing::debug!("MITM：应答头列表里出现空行（裁剪路径会因此放弃）");
    }

    let status_line = lines.first().cloned().unwrap_or_default();
    let status = parse_status(&status_line);
    let content_type = header_in_lines(&lines, "content-type").map(str::to_string);
    let leftover = buf[end..].to_vec();

    let chunked = lines.iter().any(|l| {
        l.split_once(':')
            .map(|(k, v)| {
                k.eq_ignore_ascii_case("transfer-encoding")
                    && v.to_ascii_lowercase().contains("chunked")
            })
            .unwrap_or(false)
    });

    // 这些状态码按 RFC 没有 body：不要为它去等 EOF/CL。
    let bodyless = (100..200).contains(&status) || status == 204 || status == 304;
    if bodyless {
        return Ok(Response {
            lines,
            status,
            status_line,
            content_type,
            raw: Vec::new(),
            payload: Vec::new(),
            chunked: false,
        });
    }

    if chunked {
        let (raw, payload) = read_chunked(upstream, leftover)?;
        return Ok(Response { lines, status, status_line, content_type, raw, payload, chunked: true });
    }

    if let Some(want) = header_in_lines(&lines, "content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if want > MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("应答体 {want} 字节超过上限 {MAX_BODY_BYTES}"),
            ));
        }
        let body = read_exact_body(upstream, leftover, want)?;
        return Ok(Response {
            lines,
            status,
            status_line,
            content_type,
            raw: body.clone(),
            payload: body,
            chunked: false,
        });
    }

    // 既没有 CL 也没有 chunked：HTTP/1.x 靠"连接关闭"分帧（我们已强制 Connection: close）。
    let body = read_until_eof(upstream, leftover)?;
    Ok(Response {
        lines,
        status,
        status_line,
        content_type,
        raw: body.clone(),
        payload: body,
        chunked: false,
    })
}

/// 状态行里的数字状态码（解析不出来返回 0，绝不 panic）。
fn parse_status(status_line: &str) -> u16 {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0)
}

/// 在"状态行 + 头字段"的行列表里取一个头（大小写不敏感，取第一个）。
fn header_in_lines<'a>(lines: &'a [String], name: &str) -> Option<&'a str> {
    lines.iter().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// 精确读 `want` 字节；`initial` 是"已经读进来的"前缀。
fn read_exact_body<S: Read>(
    src: &mut S,
    mut body: Vec<u8>,
    want: usize,
) -> std::io::Result<Vec<u8>> {
    if body.len() > want {
        // 只可能来自流水线/上游多话；本版一次一条请求，多出来的直接截掉。
        body.truncate(want);
    }
    let mut chunk = [0u8; 8192];
    while body.len() < want {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "应答体没读完就 EOF",
            ));
        }
        let take = (want - body.len()).min(n);
        body.extend_from_slice(&chunk[..take]);
    }
    Ok(body)
}

/// 读到上游关闭（上限 [`MAX_BODY_BYTES`]）。
fn read_until_eof<S: Read>(src: &mut S, mut body: Vec<u8>) -> std::io::Result<Vec<u8>> {
    if body.len() > MAX_BODY_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "应答体超过上限"));
    }
    let mut chunk = [0u8; 8192];
    loop {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > MAX_BODY_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "应答体超过上限"));
        }
    }
    Ok(body)
}

/// 读一个 chunked 体：返回 `(原样字节, 去框架后的载荷)`。
///
/// 原样字节包含分块框架、终止 chunk 与 trailer —— 透明转发要的就是它们。
/// 载荷用于观察（搜标记词）与裁剪（重算 `Content-Length`）。
fn read_chunked<S: Read>(src: &mut S, mut raw: Vec<u8>) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut payload: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut chunk = [0u8; 8192];
    loop {
        // chunk-size 行（允许 chunk-ext：分号后面的部分忽略；转发时原样保留）。
        let line_end = loop {
            if let Some(i) = find_crlf(&raw, pos) {
                break i;
            }
            if raw.len().saturating_sub(pos) > MAX_HEAD_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "chunk 长度行超过上限",
                ));
            }
            let n = src.read(&mut chunk)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "chunked 应答体没读完就 EOF",
                ));
            }
            raw.extend_from_slice(&chunk[..n]);
        };
        let token = raw[pos..line_end].split(|&b| b == b';').next().unwrap_or(&[]);
        let size = parse_hex_usize(token).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk 长度不是十六进制")
        })?;
        pos = line_end + 2;
        if size == 0 {
            // last-chunk 之后的 trailer-part：读到空行为止（`0\r\n\r\n` 就是空 trailer）。
            loop {
                let le = loop {
                    if let Some(i) = find_crlf(&raw, pos) {
                        break i;
                    }
                    let n = src.read(&mut chunk)?;
                    if n == 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "chunked 结束块没读完就 EOF",
                        ));
                    }
                    raw.extend_from_slice(&chunk[..n]);
                };
                let empty = le == pos;
                pos = le + 2;
                if empty {
                    break;
                }
            }
            break;
        }
        let need = pos + size + 2; // chunk-data + 结尾 CRLF
        if need > MAX_BODY_BYTES.saturating_add(MAX_HEAD_BYTES) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "chunked 体超过上限"));
        }
        while raw.len() < need {
            let n = src.read(&mut chunk)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "chunked 应答体没读完就 EOF",
                ));
            }
            raw.extend_from_slice(&chunk[..n]);
        }
        if payload.len() + size > MAX_BODY_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "chunked 载荷超过上限"));
        }
        payload.extend_from_slice(&raw[pos..pos + size]);
        pos = need;
    }
    raw.truncate(pos);
    Ok((raw, payload))
}

/// 在 `buf[from..]` 里找下一个 `\r\n` 的起始下标。
fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..].windows(2).position(|w| w == b"\r\n").map(|p| p + from)
}

/// 十六进制 chunk 长度（允许前后空白；空串/非十六进制 = `None`）。
fn parse_hex_usize(token: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(token).ok()?.trim();
    if s.is_empty() {
        return None;
    }
    usize::from_str_radix(s, 16).ok()
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
        let (head, leftover) = read_head(&mut chunky(raw, 3))
            .expect("不该报错")
            .expect("应当解析出请求头");
        assert_eq!(head.method, "GET");
        assert_eq!(head.host(), Some("news.example"));
        assert!(leftover.is_empty(), "没有 body 时 leftover 必须是空的");
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
        let resp = read_response(&mut Cursor::new(raw.to_vec())).expect("应当原样接受");
        assert_eq!(resp.lines.first().map(String::as_str), Some("NOT-HTTP 200 OK"));
        assert_eq!(resp.status, 200, "状态码仍要从第二段解析出来");
        assert!(resp.payload.is_empty());
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
        let resp = read_response(&mut Cursor::new(raw.to_vec())).unwrap();
        assert_eq!(resp.lines.first().map(String::as_str), Some("HTTP/1.1 200 OK"));
        assert_eq!(resp.lines.len(), 3, "尾随空行不许进列表");
        assert_eq!(resp.payload, b"{\"a\":1}");
        assert_eq!(resp.raw, b"{\"a\":1}", "非 chunked 时 raw 就是 payload");
        assert!(!resp.chunked);
        assert_eq!(resp.content_type.as_deref(), Some("application/json"));
    }

    /// **判据 1**：`Content-Length` 请求体逐字节读出来（含 leftover 前缀）。
    #[test]
    fn a_content_length_request_body_is_read_byte_for_byte() {
        let body: &[u8] = &[0x00, 0x01, 0xff, 0xfe, b'\r', b'\n', 0x7f];
        let mut raw = b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 7\r\n\r\n".to_vec();
        raw.extend_from_slice(body);
        let mut cur = Cursor::new(raw);
        let (head, leftover) = read_head(&mut cur).unwrap().unwrap();
        let got = read_request_body(&mut cur, &head, leftover).unwrap();
        assert_eq!(got, body, "请求体必须逐字节原样");
    }

    /// **判据 1**：chunked 请求体**连分块框架一起**原样读出（不许解码重编码）。
    #[test]
    fn a_chunked_request_body_is_read_verbatim() {
        let chunked: &[u8] = b"4;ext=1\r\n\x00\x01\xff\xfe\r\n5\r\nhello\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let mut raw = b"POST /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        raw.extend_from_slice(chunked);
        let mut cur = Cursor::new(raw);
        let (head, leftover) = read_head(&mut cur).unwrap().unwrap();
        let got = read_request_body(&mut cur, &head, leftover).unwrap();
        assert_eq!(got, chunked, "chunked 请求体必须原样（含 chunk-ext 与 trailer）");
    }

    /// 没有 framing 的请求 = 没有 body。
    #[test]
    fn a_request_without_framing_has_an_empty_body() {
        let mut cur = Cursor::new(b"GET /x HTTP/1.1\r\nHost: h\r\n\r\n".to_vec());
        let (head, leftover) = read_head(&mut cur).unwrap().unwrap();
        assert!(read_request_body(&mut cur, &head, leftover).unwrap().is_empty());
    }

    /// **判据 2**：chunked 应答读到终止 chunk；`raw` 原样、`payload` 是去框架后的载荷。
    /// 用每次只吐 1 字节的读端，专门压 chunk 边界跨 read 的情况。
    #[test]
    fn a_chunked_response_is_read_to_the_terminating_chunk() {
        let chunked: &[u8] = b"7\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\n\r\n";
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        raw.extend_from_slice(chunked);
        let resp = read_response(&mut chunky(&raw, 1)).expect("chunked 必须被支持");
        assert!(resp.chunked);
        assert_eq!(resp.raw, chunked, "raw 必须逐字节保留分块框架");
        assert_eq!(resp.payload, b"{\"a\":1,\"b\":2}", "payload 必须是去框架后的载荷");
    }

    /// `Transfer-Encoding` 大小写不敏感；`chunked` 后面带别的编码也认（RFC 允许）。
    #[test]
    fn chunked_detection_is_case_insensitive() {
        let raw = b"HTTP/1.1 200 OK\r\ntransfer-encoding: CHUNKED\r\n\r\n0\r\n\r\n";
        let resp = read_response(&mut Cursor::new(raw.to_vec())).unwrap();
        assert!(resp.chunked);
        assert!(resp.raw.ends_with(b"0\r\n\r\n"));
        assert!(resp.payload.is_empty());
    }

    /// 截断的 chunked（没有终止 chunk）必须报错，不许把半截体当完整响应放行。
    #[test]
    fn a_truncated_chunked_response_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel";
        let err = read_response(&mut Cursor::new(raw.to_vec())).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "{err}");
    }

    /// 非法 chunk 长度：明确报错，不做任何猜测。
    #[test]
    fn a_non_hex_chunk_size_is_an_error() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n";
        let err = read_response(&mut Cursor::new(raw.to_vec())).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    }

    /// 既没有 CL 也没有 chunked ⇒ 读到上游关闭（`Connection: close` 的语义）。
    #[test]
    fn a_response_without_framing_is_read_until_eof() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nuntil the close";
        let resp = read_response(&mut Cursor::new(raw.to_vec())).unwrap();
        assert_eq!(resp.payload, b"until the close");
        assert_eq!(resp.raw, b"until the close");
    }

    /// 204/304/1xx 没有 body：不要为一个没有体的响应去等 EOF。
    #[test]
    fn bodyless_statuses_do_not_wait_for_a_body() {
        for raw in [
            &b"HTTP/1.1 204 No Content\r\n\r\n"[..],
            &b"HTTP/1.1 304 Not Modified\r\n\r\n"[..],
            &b"HTTP/1.1 100 Continue\r\n\r\n"[..],
        ] {
            let resp = read_response(&mut Cursor::new(raw.to_vec())).unwrap();
            assert!(resp.payload.is_empty(), "{raw:?}");
            assert!(resp.raw.is_empty(), "{raw:?}");
        }
    }

    /// 超过上限的 `Content-Length` 明确报错（fail-open：关连接，不发截断的响应）。
    #[test]
    fn an_oversized_content_length_is_refused() {
        let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1);
        let err = read_response(&mut Cursor::new(raw.into_bytes())).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
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
