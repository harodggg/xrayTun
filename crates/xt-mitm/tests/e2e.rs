//! MITM 通道的**端到端**测试：真的终结 TLS、真的判定、真的回连。
//!
//! # 为什么这套测试不需要 root、不碰系统钥匙串
//!
//! CA 在测试进程内生成（[`LocalCa::generate`]），客户端用**同一张 CA 的 DER** 去信它。
//! 于是"信任"这件事被关在测试进程里：不动系统钥匙串、不需要管理员密码、
//! 也不会给用户留下任何东西。而**它验证的是同一条代码路径** ——
//! helper 装进系统钥匙串的那张 CA，在生产里就是同一份 `cert_pem()`。
//!
//! ```text
//! 测试客户端 ──TLS(信测试内 CA, ALPN h1)──▶ xt-mitm 代理
//!                                              │ 判定：阻断？→ 204
//!                                              │ 放行 → SOCKS5 到 fake-upstream
//!                                              ▼
//!                                        fake SOCKS5（测试内）──▶ 本地 HTTP 源站（测试内）
//! ```
//!
//! `fake-upstream` 就是 `mitm-upstream` 的替身：生产里它由 Xray 的 socks 入站扮演
//! （`inboundTag=mitm-upstream` ⇒ 不命中 steer 规则 ⇒ 不自环；那条不变量由
//! `xt-core` 的单测钉住，这里验证的是"MITM 会经它回连且能拿到应答"）。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, ServerName};
use xt_mitm::decide::{Decision, Decider};
use xt_mitm::{
    serve, BlocklistDecider, LocalCa, ProxyConfig, ALPN_HTTP1,
};

/// 本地 HTTP 源站：每次都回一段 JSON，并**记录被真正请求过几次** ——
/// "阻断必须没到源站"这条断言全靠它。
struct Origin {
    port: u16,
    hits: Arc<AtomicU64>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Origin {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (h2, s2) = (hits.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // 非阻塞监听 ⇒ accept 出来的已连接套接字也是非阻塞，
                        // 下面的 read 会立刻 WouldBlock 并直接写出应答（还是会"看起来能过"，
                        // 但其实没读到请求）。替身必须显式设回阻塞。
                        let _ = sock.set_nonblocking(false);
                        h2.fetch_add(1, Ordering::Relaxed);
                        let body = br#"{"data":{"items":[{"id":1},{"id":2,"promoted":true}]}}"#;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = [0u8; 2048];
                        let _ = sock.read(&mut buf);
                        let _ = sock.write_all(resp.as_bytes());
                        let _ = sock.write_all(body);
                        let _ = sock.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, hits, stop, handle: Some(handle) }
    }

    fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// `mitm-upstream` 的替身：一个只说 SOCKS5、然后盲目搬运字节的转发器。
struct FakeUpstream {
    port: u16,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeUpstream {
    fn start(origin_port: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // 同上：不设回阻塞，read_exact 会立刻失败 → `continue`
                        // 直接关掉连接，代理侧表现为 SOCKS 握手读到 EOF。
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
                        let mut hello = [0u8; 2];
                        if sock.read_exact(&mut hello).is_err() || sock.write_all(&[5, 0]).is_err() {
                            continue;
                        }
                        // CONNECT 请求：5,1,0,3,len,host,port(2)
                        let mut fixed = [0u8; 4];
                        if sock.read_exact(&mut fixed).is_err() {
                            continue;
                        }
                        if fixed[3] == 3 {
                            let mut len = [0u8; 1];
                            let _ = sock.read_exact(&mut len);
                            let mut host = vec![0u8; len[0] as usize + 2];
                            let _ = sock.read_exact(&mut host);
                        }
                        if sock.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).is_err() {
                            continue;
                        }
                        // 盲目双向搬运（**忽略**请求里的端口，一律转发到测试源站 ——
                        // 因为真 MITM 也只能假设 443，端口在这儿由测试替身接管）。
                        let Ok(mut out) = TcpStream::connect(("127.0.0.1", origin_port)) else {
                            continue;
                        };
                        let Ok(mut a2) = sock.try_clone() else { continue };
                        let Ok(mut b2) = out.try_clone() else { continue };
                        let t = std::thread::spawn(move || {
                            let _ = std::io::copy(&mut a2, &mut b2);
                        });
                        let _ = std::io::copy(&mut out, &mut sock);
                        let _ = t.join();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, stop, handle: Some(handle) }
    }
}

impl Drop for FakeUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 按**路径**判定的 decider —— 用来证明 MITM 能做到域名层做不到的事。
struct PathDecider;

impl Decider for PathDecider {
    fn decide(&self, head: &xt_mitm::RequestHead) -> Decision {
        if head.path().starts_with("/ads/") {
            return Decision::Block { reason: format!("路径命中：{}", head.path()) };
        }
        Decision::Pass
    }
}

fn install_crypto_provider() {
    // rustls 0.23 要求进程级安装一个 crypto provider（幂等）。
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn client_config(ca_der: Vec<u8>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der))
        .expect("测试 CA 要能装进根仓库");
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
    Arc::new(cfg)
}

/// 一次 HTTPS 请求。返回 `(响应文本, 协商到的 ALPN, 是否干净收尾)`。
///
/// 第三个元素是**判别性**的：rustls 客户端只有在对方发来 `close_notify` 时
/// 才返回 `Ok(0)`；对端直接关 TCP 会得到 `UnexpectedEof`
/// （`peer closed connection without sending TLS close_notify`）。
/// 所以它可以钉住"代理必须显式发 close_notify"这条行为 ——
/// 少了它，严格客户端（OpenSSL 默认）会把正常响应当**截断**。
fn https_get(
    proxy: SocketAddr,
    host: &str,
    path: &str,
    cfg: &Arc<rustls::ClientConfig>,
) -> (String, Option<Vec<u8>>, bool) {
    let tcp = TcpStream::connect(proxy).expect("连代理");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let name = ServerName::try_from(host.to_string()).expect("合法域名");
    let conn = rustls::ClientConnection::new(cfg.clone(), name).expect("客户端会话");
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes())
        .expect("写请求");
    tls.flush().unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let mut clean_eof = false;
    loop {
        match tls.read(&mut buf) {
            Ok(0) => {
                clean_eof = true;
                break;
            }
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                eprintln!("[client] TLS 读错误（未收到 close_notify）：{e}");
                break;
            }
        }
    }
    let alpn = tls.conn.alpn_protocol().map(|p| p.to_vec());
    (String::from_utf8_lossy(&out).to_string(), alpn, clean_eof)
}

struct Harness {
    proxy: xt_mitm::ProxyHandle,
    origin: Origin,
    _upstream: FakeUpstream,
    client: Arc<rustls::ClientConfig>,
}

fn harness(decider: Arc<dyn Decider>) -> Harness {
    install_crypto_provider();
    let origin = Origin::start();
    let upstream = FakeUpstream::start(origin.port);
    let ca = Arc::new(LocalCa::generate().unwrap());
    let client = client_config(ca_cert_der(&ca));
    let cfg = ProxyConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream_socks: format!("127.0.0.1:{}", upstream.port).parse().unwrap(),
        io_timeout: Duration::from_secs(5),
        max_connections: 32,
    };
    let proxy = serve(cfg, ca, decider).expect("起代理");
    Harness { proxy, origin, _upstream: upstream, client }
}

fn ca_cert_der(ca: &LocalCa) -> Vec<u8> {
    // 从 PEM 里取 DER（rcgen 生成的是标准 base64 块）。
    let pem = ca.cert_pem();
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    base64_decode(&body)
}

/// 极简 base64 解码（只为测试；不引新依赖）。
fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = T.iter().position(|t| *t == c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[test]
fn a_passed_request_reaches_the_origin_and_only_negotiates_http1() {
    let h = harness(Arc::new(BlocklistDecider::default()));
    let (resp, alpn, clean) = https_get(h.proxy.listen, "news.example", "/index", &h.client);
    assert!(clean, "代理必须发 close_notify，否则正常响应会被当成截断");
    assert!(resp.contains("200 OK"), "应当拿到源站的应答：{resp:?}");
    assert!(resp.contains("\"promoted\":true"), "源站内容应当原样回来：{resp:?}");
    assert_eq!(h.origin.hits(), 1, "源站应当被请求过恰好一次");
    assert_eq!(
        alpn.as_deref(),
        Some(ALPN_HTTP1),
        "协商结果必须是 http/1.1（不广告 h2）"
    );
    assert!(h.proxy.wait_passed_at_least(1, Duration::from_secs(3)));
}

#[test]
fn a_blocked_host_gets_204_and_never_reaches_the_origin() {
    let decider = Arc::new(BlocklistDecider::new(vec!["ads.example".to_string()]));
    let h = harness(decider);
    let (resp, _, clean) = https_get(h.proxy.listen, "ads.example", "/banner.png", &h.client);
    assert!(clean, "阻断应答也必须干净收尾");
    assert!(resp.starts_with("HTTP/1.1 204 No Content"), "{resp:?}");
    assert!(resp.contains("X-XrayTun-Blocked: "), "阻断必须给出可读原因：{resp:?}");
    assert_eq!(h.origin.hits(), 0, "**被阻断的请求绝不能到源站**");
    assert_eq!(h.proxy.stats().blocked, 1);
    // 子域也要命中（投放端换子域是常见做法）。
    let (resp2, _, _) = https_get(h.proxy.listen, "cdn.ads.example", "/x", &h.client);
    assert!(resp2.starts_with("HTTP/1.1 204"), "{resp2:?}");
    assert_eq!(h.origin.hits(), 0);
}

/// **域名层做不到的那件事**：按 URL 路径拦。
///
/// 同一个主机名上，`/index` 放行、`/ads/banner.js` 拦掉 —— 这是 MTIM 相对
/// `geosite`/域名意图判定的唯一增量，也是本测试存在的理由。
#[test]
fn the_mitm_can_block_by_url_path_which_the_domain_layer_cannot() {
    let h = harness(Arc::new(PathDecider));
    let (ok, _, clean) = https_get(h.proxy.listen, "news.example", "/index", &h.client);
    assert!(clean, "放行路径也要干净收尾");
    assert!(ok.contains("200 OK"), "非广告路径应当放行：{ok:?}");
    assert_eq!(h.origin.hits(), 1);

    let (blocked, _, _) = https_get(h.proxy.listen, "news.example", "/ads/banner.js", &h.client);
    assert!(blocked.starts_with("HTTP/1.1 204"), "广告路径应当被拦：{blocked:?}");
    assert!(blocked.contains("/ads/banner.js"), "原因里要带上路径：{blocked:?}");
    assert_eq!(h.origin.hits(), 1, "被拦的那条不该再次到达源站");

    let s = h.proxy.stats();
    assert_eq!((s.passed, s.blocked), (1, 1), "{s:?}");
}

/// 客户端**不**信任这张 CA 时，握手必须失败 —— 否则"信任锚"这件事就没有意义。
#[test]
fn a_client_that_does_not_trust_the_ca_fails_the_handshake() {
    install_crypto_provider();
    let origin = Origin::start();
    let upstream = FakeUpstream::start(origin.port);
    let ca = Arc::new(LocalCa::generate().unwrap());
    let cfg = ProxyConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream_socks: format!("127.0.0.1:{}", upstream.port).parse().unwrap(),
        io_timeout: Duration::from_secs(5),
        max_connections: 8,
    };
    let proxy = serve(cfg, ca, Arc::new(BlocklistDecider::default())).expect("起代理");

    // 空根仓库 ⇒ 谁都不信。
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(system_root_for_negative_test())).ok();
    let untrusting = Arc::new(rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth());

    let tcp = TcpStream::connect(proxy.listen).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let name = ServerName::try_from("news.example".to_string()).unwrap();
    let conn = rustls::ClientConnection::new(untrusting, name).unwrap();
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // 这条**本身不具鉴别力**（任何失败都算"握手失败"）—— 所以下面把错误打出来，
    // 免得它替真正的 bug 背锅。
    let write = tls.write_all(b"GET / HTTP/1.1\r\nHost: news.example\r\n\r\n");
    let read = match write {
        Ok(()) => {
            let mut b = [0u8; 64];
            tls.read(&mut b)
        }
        Err(e) => Err(e),
    };
    assert!(
        read.is_err(),
        "不受信任的客户端不应握手成功（否则'信任锚'没有意义）"
    );
    assert_eq!(origin.hits(), 0);
}

/// 负对照用的"另一个自签根"（**故意不是**我们那张 CA）。
fn system_root_for_negative_test() -> Vec<u8> {
    // 现场再生成一张无关的 CA：它的 DER 进根仓库，当然验不过代理那张。
    let other = LocalCa::generate().unwrap();
    ca_cert_der(&other)
}
