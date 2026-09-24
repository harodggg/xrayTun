//! **真 MITM** 的真实核心验收（P4 第三步的收尾）。
//!
//! `mitm_steering_live.rs` 用"假 MITM"单独验证引导那一半；这里把假替身换成
//! [`xt_mitm`] 本身，于是被验证的不再是"字节搬过去了"，而是**整条产品链路**：
//!
//! ```text
//! 测试客户端 ──SOCKS(核心)──▶ 核心 ──steer(域名+inboundTag)──▶ mitm-out(redirect)
//!                                                                  │
//!                                                                  ▼
//!                                                         真 MITM（本测试内）
//!                                                         ① 终结 TLS（按 SNI 现签叶子）
//!                                                         ② 判定：拦 ⇒ 204，回源站 0 次
//!                                                         ③ 放行 ⇒ 经 mitm-upstream 回连
//!                                                                  │
//!                                                                  ▼
//!                                          核心（inboundTag=mitm-upstream 不命中 steer）──▶ 源站
//! ```
//!
//! 三条断言互为对照，缺一条都能让错实现变绿：
//!
//! * 名单内的广告域名：**TLS 握手成功**（证明证书是按 SNI 现签的）、拿到 204 与原因、
//!   而且**源站一次都没被请求**；
//! * 名单内的正常域名：真的经 `mitm-upstream` 回连拿到源站的 JSON（证明"拆完还能上网"，
//!   且**没有自环**——自环的话这条会超时/失败）；
//! * 名单外的域名：**一个字节都不经过 MITM**（负对照；没有它，"全量 steer"也能过前两条）。
//!
//! 全程不需要 root：CA 在测试进程内生成，客户端用**同一张 CA** 的 DER 去信它；
//! 源站是本地 HTTP 服务，域名靠核心的 `dns.hosts` 解析。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::xray::{build_pretty, merge_rules_with_intent, CoreConfigInput, InboundProfile};
use xt_mitm::{serve_with, BlocklistDecider, LocalCa, ProxyConfig, ALPN_HTTP1};

/// 在 MITM 名单里、且被判定为广告 ⇒ 必须被拦。
const BLOCKED: &str = "blocked.intent-mitm";
/// 在 MITM 名单里、但判定放行 ⇒ 必须经回连拿到源站内容。
const ALLOWED: &str = "allowed.intent-mitm";
/// **不在**名单里 ⇒ 负对照：不许经过 MITM。
const PLAIN: &str = "plain.intent-mitm";

/// 源站的应答体：带一个 `promoted` 条目（裁剪路径的现成夹具）。
const ORIGIN_BODY: &[u8] = br#"{"data":{"items":[{"id":1},{"id":2,"promoted":true}]}}"#;

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
    l.local_addr().unwrap().port()
}

/// 只回一次 JSON 的本地 HTTP 源站，并记录被请求过几次
/// （"被拦的请求绝不能到源站"这条断言全靠它）。
struct Origin {
    port: u16,
    hits: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Origin {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (h2, s2) = (hits.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // 非阻塞监听 ⇒ accept 出来的已连接套接字也是非阻塞，
                        // 必须设回阻塞，否则读请求会立刻 WouldBlock。
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(800)));
                        h2.fetch_add(1, Ordering::Relaxed);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            ORIGIN_BODY.len()
                        );
                        let mut buf = [0u8; 2048];
                        let _ = sock.read(&mut buf);
                        let _ = sock.write_all(head.as_bytes());
                        let _ = sock.write_all(ORIGIN_BODY);
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

/// 通过核心的 socks 入站建立一条 TCP 连接（SOCKS5 无认证 + 域名目标）。
fn socks_connect(socks_port: u16, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], socks_port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(&[5, 1, 0])?;
    let mut hello = [0u8; 2];
    s.read_exact(&mut hello)?;
    if hello != [5, 0] {
        return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "socks 握手失败"));
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
            format!("socks 拒绝 reply={}", head[1]),
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

fn client_config(ca: &LocalCa) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(ca.cert_der())
        .expect("测试 CA 必须能装进根仓库");
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
    Arc::new(cfg)
}

/// 经核心的 socks 入站，对一个域名做一次真 HTTPS 请求。
///
/// 返回 `(响应文本, 是否干净收尾)`：第二项在"代理发了 close_notify"时才为真，
/// 所以它同时钉住了代理的 TLS 收尾行为。
fn https_get(
    socks_port: u16,
    host: &str,
    path: &str,
    client: &Arc<rustls::ClientConfig>,
) -> std::io::Result<(String, bool)> {
    let tcp = socks_connect(socks_port, host, 443)?;
    let name = ServerName::try_from(host.to_string())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let conn = rustls::ClientConnection::new(client.clone(), name)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    tls.flush()?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let mut clean = false;
    loop {
        match tls.read(&mut buf) {
            Ok(0) => {
                clean = true;
                break;
            }
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                eprintln!("[client] TLS 读错误（未收到 close_notify）：{e}");
                break;
            }
        }
    }
    Ok((String::from_utf8_lossy(&out).to_string(), clean))
}

struct CoreGuard {
    child: Child,
}

impl Drop for CoreGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn core_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XT_CORE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    [repo.join("apps/desktop/binaries/xray"), repo.join(".scratch/xray-server/xray")]
        .into_iter()
        .find(|c| c.is_file())
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn the_real_mitm_blocks_one_opt_in_domain_and_passes_another_through_the_real_core() {
    let Some(core) = core_binary() else {
        eprintln!("SKIP：没找到真实核心（XT_CORE 未设，apps/desktop/binaries/xray 与 .scratch/xray-server/xray 都不存在）");
        return;
    };

    let origin = Origin::start();
    let socks = free_port();
    let http = free_port();
    let api = free_port();
    let upstream = free_port();
    let mitm_port = free_port();

    let ca = Arc::new(LocalCa::generate().expect("生成测试 CA"));

    // ---- 起真 MITM：只拦 BLOCKED，其余经 mitm-upstream 回连 ----
    let cfg_proxy = ProxyConfig {
        listen: format!("127.0.0.1:{mitm_port}").parse().unwrap(),
        upstream_socks: format!("127.0.0.1:{upstream}").parse().unwrap(),
        io_timeout: Duration::from_secs(10),
        max_connections: 32,
        // `freedom.redirect` 丢掉了原始端口，所以回连端口只能假定；
        // 测试的源站在随机端口上，这里把假定值指过去。
        assumed_port: origin.port,
    };
    let proxy = serve_with(
        cfg_proxy,
        ca.clone(),
        Arc::new(BlocklistDecider::new(vec![BLOCKED.to_string()])),
        None,
    )
    .expect("起真 MITM");

    // ---- 用**产品自己的**配置生成器造核心配置 ----
    let mut s = AppSettings {
        routing_preset: RoutingPreset::BypassMainland,
        socks_port: socks,
        http_port: http,
        ..Default::default()
    };
    s.dns.hosts = vec![
        (BLOCKED.to_string(), "127.0.0.1".to_string()),
        (ALLOWED.to_string(), "127.0.0.1".to_string()),
        (PLAIN.to_string(), "127.0.0.1".to_string()),
    ];
    s.dns.direct_servers = vec!["223.5.5.5".into()];
    s.mitm.enabled = true;
    s.mitm.domains = vec![BLOCKED.to_string(), ALLOWED.to_string()];
    s.mitm.listen_port = mitm_port;
    s.mitm.upstream_port = upstream;
    s.mitm.block_quic = false;

    let rules = merge_rules_with_intent(&s, &[], &[]);
    let mut cfg: serde_json::Value = serde_json::from_str(&build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &rules,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    }))
    .unwrap();
    // api 入站端口是常量（10085），本机可能有别的实例占着 ⇒ 只改这一处。
    for i in cfg["inbounds"].as_array_mut().unwrap().iter_mut() {
        if i["tag"] == "api" {
            i["port"] = serde_json::Value::from(api);
        }
    }

    let dir = std::env::temp_dir().join(format!("xt-mitm-tls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mitm-tls.json");
    std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

    let child = Command::new(&core)
        .args(["run", "-c"])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("起核心");
    let mut guard = CoreGuard { child };
    if !wait_for_port(socks, Duration::from_secs(10)) {
        let mut err = String::new();
        if let Some(mut e) = guard.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        panic!("核心没起来（socks={socks} api={api} mitm={mitm_port} upstream={upstream}）：\n{err}");
    }

    let client = client_config(&ca);
    println!(
        "核心已就绪：socks={socks} mitm={mitm_port} upstream={upstream} origin={}",
        origin.port
    );

    // ---- ① 名单内的广告域名：真 TLS 握上手、拿到 204、源站 0 次 ----
    let (blocked, clean) = https_get(socks, BLOCKED, "/banner.png", &client)
        .expect("被 steer 的连接应当能完成 TLS 握手（证书是按 SNI 现签的）");
    assert!(clean, "代理必须发 close_notify（否则正常响应会被当成截断）");
    assert!(
        blocked.starts_with("HTTP/1.1 204"),
        "被判定为广告的域名应当拿到 204：{blocked:?}"
    );
    assert!(
        blocked.contains("X-XrayTun-Blocked"),
        "阻断必须给出可读原因：{blocked:?}"
    );
    assert_eq!(origin.hits(), 0, "**被拦的请求绝不能到源站**");
    assert_eq!(proxy.stats().blocked, 1);
    assert_eq!(proxy.stats().passed, 0, "只发了一条被拦的请求");
    assert_eq!(proxy.stats().failed, 0, "不该有失败：{:?}", proxy.stats());
    println!("① 广告域名：TLS 终结 + 204 + 源站 0 次 ✓");

    // ---- ② 名单内的正常域名：经 mitm-upstream 回连拿到源站内容（没有自环）----
    let (allowed, clean2) = https_get(socks, ALLOWED, "/index", &client)
        .expect("放行路径应当能完成请求");
    assert!(clean2, "放行路径也要干净收尾");
    assert!(
        allowed.contains("200 OK") && allowed.contains("promoted"),
        "放行路径必须真的拿到源站内容（自环或回连不通都会失败）：{allowed:?}"
    );
    assert_eq!(origin.hits(), 1, "放行路径必须**真的**到源站，且只到一次");
    assert_eq!(proxy.stats().passed, 1);
    assert_eq!(proxy.stats().blocked, 1, "放行不该顺带多拦一条");
    assert_eq!(proxy.stats().failed, 0, "不该有失败：{:?}", proxy.stats());
    println!("② 正常域名：经 mitm-upstream 回连到源站（无自环）✓");

    // ---- ③ 负对照：名单外的域名一个字节都不经过 MITM ----
    let before = proxy.stats().accepted;
    let mut plain = socks_connect(socks, PLAIN, origin.port).expect("直连源站");
    plain
        .write_all(format!("GET / HTTP/1.1\r\nHost: {PLAIN}\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match plain.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break
            }
            Err(e) => panic!("非 opt-in 域名应当照常直连成功：{e}"),
        }
    }
    let plain = String::from_utf8_lossy(&out).to_string();
    assert!(plain.contains("200 OK"), "非 opt-in 域名应当照常直连成功：{plain:?}");
    assert_eq!(
        proxy.stats().accepted,
        before,
        "非 opt-in 域名也被 steer 了 —— 那就不是 opt-in 而是全量拆包"
    );
    println!("③ 非 opt-in 域名：未经过 MITM 且仍然 200 ✓");

    let _ = std::fs::remove_dir_all(&dir);
}
