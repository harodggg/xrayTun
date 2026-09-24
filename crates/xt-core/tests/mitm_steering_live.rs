//! MITM 引导的真实数据面验收（P4 第三步的第一半）。
//!
//! # 为什么要用「假 MITM」
//!
//! 真正的 MITM 要终结 TLS（rustls + 证书 + `Content-Length` 一致性……），那是另一半工作。
//! 但**引导这一半**可以先独立验证，而且它正好是最容易搞错、后果最难查的部分：
//!
//! * 被 steer 的流量**真的**到了本地 MITM 端口吗？
//! * MITM 通过 `mitm-upstream` **回连**时，会不会再次命中 steer 规则（**自环**）？
//! * 没在 opt-in 名单里的域名，是不是**一个字节都没经过** MITM？
//!
//! 所以这里起一个**只做 TCP 转发**的"假 MITM"：它收到连接后，经 `mitm-upstream`
//! （Xray 的本地 socks 入站）回连到目标。它不做 TLS —— 但它走的是**真 MITM 将要走的
//! 那条路**，因此上面三个问题的答案都是真的。
//!
//! ```
//! 客户端 ──SOCKS(核心的 socks 入站)──▶ 核心 ──steer──▶ mitm-out(redirect)
//!                                                        │
//!                                                        ▼
//!                                                   假 MITM（本测试）
//!                                                        │ 经 mitm-upstream 回连
//!                                                        ▼
//!                                   核心 ──(inboundTag=mitm-upstream，不命中 steer)──▶ 目标
//! ```
//!
//! 第三条断言（非 opt-in 不经过）是**负对照**：没有它，一个"把所有流量都 steer 过去"
//! 的实现也能让前两条通过。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::xray::{build_pretty, merge_rules_with_intent, CoreConfigInput, InboundProfile};

const STEERED: &str = "steered.intent-mitm";
const PLAIN: &str = "plain.intent-mitm";

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
    l.local_addr().unwrap().port()
}

/// 只回一次 `200 ok` 的本地 HTTP 服务。
struct Web {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Web {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = [0u8; 1024];
                        let _ = sock.read(&mut buf);
                        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
                        let _ = sock.flush();
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

impl Drop for Web {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// **假 MITM**：接受被 steer 的连接，经 `mitm-upstream` 回连到目标，然后双向搬字节。
///
/// 它同时记录"收到过几条连接" —— 那正是"steer 有没有生效"的唯一判据。
struct FakeMitm {
    port: u16,
    accepted: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeMitm {
    /// `target` 是它要回连的目标（真 MITM 会从 SNI/Host 还原，这里直接给定）。
    fn start(upstream_port: u16, target: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicU64::new(0));
        let (s2, a2) = (stop.clone(), accepted.clone());
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut inbound, _)) => {
                        a2.fetch_add(1, Ordering::Relaxed);
                        // **回连走 mitm-upstream**（核心的本地 socks 入站）——
                        // 这正是真 MITM 要做的事，也是自环会不会发生的分界线。
                        let Ok(mut outbound) = socks_connect(upstream_port, &target) else {
                            continue;
                        };
                        let _ = std::thread::spawn(move || {
                            let mut buf = [0u8; 4096];
                            // 先把客户端已经发来的请求读一段转发过去（简化：一次性）。
                            let _ = inbound.set_read_timeout(Some(Duration::from_millis(800)));
                            if let Ok(n) = inbound.read(&mut buf) {
                                if n > 0 {
                                    let _ = outbound.write_all(&buf[..n]);
                                }
                            }
                            let _ = outbound.flush();
                            let _ = outbound.set_read_timeout(Some(Duration::from_millis(800)));
                            while let Ok(n) = outbound.read(&mut buf) {
                                if n == 0 {
                                    break;
                                }
                                if inbound.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                                let _ = inbound.flush();
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, accepted, stop, handle: Some(handle) }
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }
}

impl Drop for FakeMitm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 通过某个 socks 端口建立一条连接（SOCKS5 无认证 + 域名目标）。
fn socks_connect(socks_port: u16, target: &str) -> std::io::Result<TcpStream> {
    let (host, port) = target.rsplit_once(':').expect("target 要 host:port");
    let port: u16 = port.parse().expect("端口");
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], socks_port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.set_write_timeout(Some(Duration::from_secs(3)))?;
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

fn http_get(socks_port: u16, host: &str, port: u16) -> std::io::Result<String> {
    let mut s = socks_connect(socks_port, &format!("{host}:{port}"))?;
    s.write_all(format!("GET / HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(ref e)
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                break
            }
            Err(e) => return Err(e),
        }
    }
    Ok(String::from_utf8_lossy(&out).to_string())
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
fn mitm_steering_reaches_only_the_opt_in_domain_and_the_reconnect_does_not_loop() {
    let Some(core) = core_binary() else {
        eprintln!("SKIP：没找到真实核心（XT_CORE 未设，apps/desktop/binaries/xray 与 .scratch/xray-server/xray 都不存在）");
        return;
    };

    let web = Web::start();
    let socks = free_port();
    let http = free_port();
    let api = free_port();
    let upstream = free_port();

    // 假 MITM 需要知道往哪回连；端口先占位（下面用真实端口重建配置）。
    let mitm_placeholder = free_port();
    let mut s = AppSettings {
        routing_preset: RoutingPreset::BypassMainland,
        socks_port: socks,
        http_port: http,
        ..Default::default()
    };
    s.dns.hosts = vec![
        (STEERED.to_string(), "127.0.0.1".to_string()),
        (PLAIN.to_string(), "127.0.0.1".to_string()),
    ];
    s.dns.direct_servers = vec!["223.5.5.5".into()];
    s.mitm.enabled = true;
    s.mitm.domains = vec![STEERED.to_string()];
    s.mitm.listen_port = mitm_placeholder;
    s.mitm.upstream_port = upstream;
    s.mitm.block_quic = false;

    let fake = FakeMitm::start(upstream, format!("127.0.0.1:{}", web.port));
    // 假 MITM 真正监听的端口可能与占位不同 —— 用真实端口重建一次配置。
    s.mitm.listen_port = fake.port;

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
    // api 入站的端口是常量（10085），本机可能有别的实例占着 ⇒ 只改这一处。
    for i in cfg["inbounds"].as_array_mut().unwrap().iter_mut() {
        if i["tag"] == "api" {
            i["port"] = serde_json::Value::from(api);
        }
    }

    let dir = std::env::temp_dir().join(format!("xt-mitm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mitm.json");
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
        panic!("核心没起来（socks={socks} api={api} mitm={} upstream={upstream}）：\n{err}", fake.port);
    }
    println!("核心已就绪：socks={socks} mitm={} upstream={upstream}", fake.port);

    // ---- ① opt-in 域名：必须经过假 MITM，并且请求要能完成 ----
    let steered = http_get(socks, STEERED, web.port).unwrap_or_default();
    assert!(
        fake.accepted() >= 1,
        "opt-in 域名的流量**没有**被 steer 到 MITM 端口（accepted={}）",
        fake.accepted()
    );
    assert!(
        steered.contains("200"),
        "经 MITM 回连之后请求没完成（自环或回连不通）：{steered:?}"
    );
    println!("opt-in 域名：steer 生效（accepted={}）且请求完成 ✓", fake.accepted());

    // ---- ② 负对照：非 opt-in 域名一个字节都不该经过 MITM ----
    let before = fake.accepted();
    let plain = http_get(socks, PLAIN, web.port).unwrap_or_default();
    assert!(plain.contains("200"), "非 opt-in 域名应当照常直连成功：{plain:?}");
    assert_eq!(
        fake.accepted(),
        before,
        "非 opt-in 域名也被 steer 了 —— 那就不是 opt-in 而是全量拆包"
    );
    println!("非 opt-in 域名：未经过 MITM 且仍然 200 ✓");

    let _ = std::fs::remove_dir_all(&dir);
}
