//! 真实核心的**数据面**验收：意图拦截规则在真实核心上真的把连接拦掉了。
//!
//! # 与 `real_core.rs` 的分工
//!
//! * `real_core.rs` 证明"配置合法、核心愿意加载、重复 ruleTag 被拒" —— 配置层；
//! * 本文件证明"**规则真的生效**" —— 数据面。这两件事之间差着一整条数据路径，
//!   本项目吃过"配置合法但核心拒启"的亏（v0.8.37 的 P0），所以两层都要有证据。
//!
//! # 全离线，且不打扰机器上正在跑的 xray
//!
//! * 本地起一个 HTTP 服务（`127.0.0.1:0`），两个测试域名通过 `dns.hosts`
//!   都映射到它 —— **不发一次外部请求**；
//! * **端口全部现取**：本机实测有正在运行的 xray 占着 `10808/10809/10085`，
//!   照搬默认值会让我们的核心起不来，而症状是"测试莫名超时"；
//!   唯一需要动生成配置的地方就是 **api 入站的端口**（`API_PORT` 是常量），
//!   测试会显式把那一处改掉并说明原因，其余分毫不动。
//! * 进程用 `Drop` 守卫收尾：断言失败/panic 时也不会留下野核心。
//!
//! # 正负对照
//!
//! 两个域名指向**同一个**本地服务：`allowed.*` 没有规则（对照组，必须拿到 200），
//! `blocked.*` 有意图拦截规则（实验组，必须拿不到 200）。
//! 只有实验组时会假绿（核心根本没起来也可能"拿不到 200"），对照组把这种可能钉死。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::xray::{build_pretty, merge_rules_with_intent, CoreConfigInput, InboundProfile};
use xt_intent::rules::{materialize, RuleOptions};
use xt_intent::verdict::{BlockVerdict, Category, Verdict};

const ALLOWED: &str = "allowed.intent-dataplane";
const BLOCKED: &str = "blocked.intent-dataplane";

/// 取一个当前空闲的端口。
///
/// 注意这是**取完就放**：理论上存在竞争窗口，但在"只在本机跑一次"的场景里，
/// 靠 `127.0.0.1:0` 拿到的端口比写死一个 1xxxx 的值可靠得多 ——
/// 写死的值会撞上本机正在跑的 xray（实测 10808/10809/10085 全被占）。
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
    l.local_addr().unwrap().port()
}

/// 一个只会回 `200 ok` 的本地 HTTP 服务。
struct TestServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地服务");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut buf = [0u8; 1024];
                        let _ = sock.read(&mut buf);
                        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
                        let _ = sock.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        println!("本地 HTTP 服务：127.0.0.1:{port}");
        Self { port, stop, handle: Some(handle) }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 起了就别留下 —— 断言失败也一样。
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
    [
        repo.join("apps/desktop/binaries/xray"),
        repo.join(".scratch/xray-server/xray"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

/// 造配置：**走应用真实的生成路径**，只把 api 入站的端口换成现取的。
///
/// 为什么必须换：`API_PORT` 是常量（10085），而本机正在运行的 xray 占着它 ——
/// 撞上之后核心会起不来，症状是"测试超时"，而不是一条能读的报错。
fn build_config() -> (String, u16, u16) {
    let socks = free_port();
    let http = free_port();
    let api = free_port();

    let mut s = AppSettings {
        routing_preset: RoutingPreset::BypassMainland,
        socks_port: socks,
        http_port: http,
        ..Default::default()
    };
    // 两个测试域名都指向本地服务；`dns.hosts` 的 key 会被 builder 加上 `domain:` 前缀。
    s.dns.hosts = vec![
        (ALLOWED.to_string(), "127.0.0.1".to_string()),
        (BLOCKED.to_string(), "127.0.0.1".to_string()),
    ];
    // 兜底解析器：本测试里 hosts 必然命中，这个值不会被用到。
    s.dns.direct_servers = vec!["223.5.5.5".to_string()];
    s.dns.remote_servers = vec!["https://1.1.1.1/dns-query".to_string()];

    let block = Verdict::Block(BlockVerdict {
        category: Category::AdOrMonetization,
        ads_intent: 0.97,
        risk_of_breakage: 0.05,
        choice_confidence: 0.93,
        effective_min: 0.85,
    });
    let rules = materialize(vec![(BLOCKED, &block)], &RuleOptions::default());
    let merged = merge_rules_with_intent(&s, &rules.allow, &rules.block);

    let mut cfg: Value = serde_json::from_str(&build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &merged,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    }))
    .expect("生成的配置必须是合法 JSON");

    let inbounds = cfg["inbounds"].as_array_mut().expect("必须有 inbounds");
    let mut patched = false;
    for i in inbounds.iter_mut() {
        if i["tag"] == "api" {
            i["port"] = Value::from(api);
            patched = true;
        }
    }
    assert!(patched, "生成配置里必须有 api 入站（否则本测试的端口改写说明就过期了）");
    (serde_json::to_string_pretty(&cfg).unwrap(), socks, api)
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

/// 手写 SOCKS5（无认证）CONNECT + 一个 HTTP GET。
///
/// 不依赖 curl：验收测试不该依赖机器上装了什么。
fn socks_get(socks_port: u16, host: &str, port: u16, timeout: Duration) -> std::io::Result<Vec<u8>> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], socks_port));
    let mut s = TcpStream::connect_timeout(&addr, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;

    s.write_all(&[5, 1, 0])?;
    let mut hello = [0u8; 2];
    s.read_exact(&mut hello)?;
    assert_eq!(hello, [5, 0], "SOCKS5 无认证握手失败");

    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req)?;

    let mut head = [0u8; 4];
    s.read_exact(&mut head)?;
    assert_eq!(head[0], 5, "SOCKS5 版本不对");
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

    s.write_all(format!("GET / HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > 16384 {
                    break;
                }
            }
            // 读超时/复位：把已经读到的交出来，让调用方去判断"有没有 200"。
            Err(e) => {
                if out.is_empty() {
                    return Err(e);
                }
                break;
            }
        }
    }
    Ok(out)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xt-intent-dataplane-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 真正的验收：意图拦截规则在真实核心上把连接拦掉了，而没有规则的同源域名正常。
#[test]
fn an_intent_block_rule_really_blackholes_on_a_real_core() {
    let Some(core) = core_binary() else {
        eprintln!(
            "SKIP：没找到真实核心（XT_CORE 未设，apps/desktop/binaries/xray 与 \
             .scratch/xray-server/xray 都不存在）。数据面验收必须在本地/发布前手动跑一次。"
        );
        return;
    };

    let server = TestServer::start();
    let (config, socks_port, api_port) = build_config();
    let dir = temp_dir("blackhole");
    let cfg_path = dir.join("intent-dataplane.json");
    std::fs::write(&cfg_path, &config).unwrap();

    let child = Command::new(&core)
        .args(["run", "-c"])
        .arg(&cfg_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("必须能启动核心");
    let mut guard = CoreGuard { child };

    if !wait_for_port(socks_port, Duration::from_secs(10)) {
        let mut err = String::new();
        if let Some(mut e) = guard.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        panic!("核心 10 秒内没有监听 SOCKS 端口 {socks_port}（api {api_port}）：\n{err}");
    }
    println!("真实核心已就绪：socks=127.0.0.1:{socks_port}，本地服务=127.0.0.1:{}", server.port);

    // ---- 对照组：没有规则的域名必须通 ----
    let allowed = socks_get(socks_port, ALLOWED, server.port, Duration::from_secs(5));
    match &allowed {
        Ok(body) => {
            let text = String::from_utf8_lossy(body);
            assert!(
                text.contains("200"),
                "对照组（没有规则）必须拿到 200，实际：{text:?}"
            );
            println!("对照组 {} → 200 ✓", ALLOWED);
        }
        Err(e) => panic!("对照组必须连通（否则本测试是假绿）：{e}"),
    }

    // ---- 实验组：有意图拦截规则的域名必须拿不到 200 ----
    let blocked = socks_get(socks_port, BLOCKED, server.port, Duration::from_secs(5));
    let blocked_text = match &blocked {
        Ok(b) => String::from_utf8_lossy(b).to_string(),
        Err(e) => format!("<{e}>"),
    };
    assert!(
        !blocked_text.contains("200"),
        "被意图规则拦掉的域名居然拿到了 200 —— 规则没有生效：{blocked_text:?}"
    );
    println!("实验组 {} → 未拿到 200（{blocked_text:?}）✓", BLOCKED);

    // ---- 再确认一次：拦截不是"核心整体坏了" ----
    // 对照组已经在拦截之前跑过；这里再跑一次，排除"拦掉之后核心就死了"。
    let allowed_again = socks_get(socks_port, ALLOWED, server.port, Duration::from_secs(5));
    match &allowed_again {
        Ok(body) => assert!(
            String::from_utf8_lossy(body).contains("200"),
            "拦截之后对照组应仍然可用"
        ),
        Err(e) => panic!("拦截之后对照组不通了（说明核心被搞坏了，而不是规则生效）：{e}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同一份配置里，**规则表里必须有那条拦截规则**，而且指向 blackhole。
///
/// 这条不需要核心，所以它在没有核心的机器上也会跑 —— 它保证上面的数据面测试
/// 不是"配置里压根没有规则"这种假绿。
#[test]
fn the_config_really_contains_the_block_rule() {
    // 这条不需要真实核心，也不需要那个本地服务 —— 它只看生成出来的 JSON。
    let (config, _socks, _api) = build_config();
    let v: Value = serde_json::from_str(&config).unwrap();
    let rules = v["routing"]["rules"].as_array().unwrap();

    let hit = rules
        .iter()
        .find(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("intent-block-"))
        .expect("配置里必须有意图拦截规则");
    assert_eq!(hit["outboundTag"], "block");
    assert_eq!(hit["domain"][0], format!("full:{BLOCKED}"));

    // 对照组域名**不许**出现在任何规则里（否则上面的对照就不是对照了）。
    let dumped = serde_json::to_string(rules).unwrap();
    assert!(!dumped.contains(ALLOWED), "对照组域名不该被任何规则提及");
}
