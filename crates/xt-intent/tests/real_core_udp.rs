//! **UDP 层的拦截**：真实核心 + 真 SOCKS5 UDP ASSOCIATE。
//!
//! # 为什么必须单独一条
//!
//! 用户的原话是"在 tcp/**udp** 层直接把广告拦截了"，而在这条测试之前，
//! 我们**只有 TCP 的证据**：`real_core_dataplane` 证明实验组拿不到 200。UDP 是另一半，
//! 而且它有两个独立的坑：
//!
//! 1. 意图规则物化时**没有设 `network`**（`MatchCondition` 留默认）—— 留默认在 Xray 里
//!    表示 TCP+UDP 都匹配。这只是"读代码得出的结论"，没有跑过就不算证据；
//! 2. 我们最初以为 UDP 的 `blackhole` 是**完全不回包**（只有 `type: none` 才是）。
//!    实测把这个假设推翻了：本项目的 `block` 出站配的是
//!    `{"response": {"type": "http"}}`，于是**UDP 客户端也会收到那个 403 应答字节**
//!    （被 SOCKS UDP 封装原样回传）。所以"拦住了"在客户端看来**不是**沉默。
//!    这条测试因此把断言改成实测事实：**回包有，但不是终点给的，且终点计数没涨**。
//!    真正的保证是"包没到终点"，而不是"客户端收不到东西"。
//!
//! 所以本测试的最小形状是两条断言互相对照：
//!
//! ```text
//! 对照 allowed.udp-dataplane ──SOCKS UDP──▶ 核心 ──▶ 本地 UDP 回显服务 ──"pong"
//! 实验 blocked.udp-dataplane ──SOCKS UDP──▶ 核心 ──✗ 不回包，且回显服务一次都没被摸到
//! ```
//!
//! 第二条的**强断言**是"回显服务的计数没涨"：只断言"客户端没收到回复"是不够的 ——
//! 任何一个坏掉的 relay 也能让它变成"没收到"。而计数是**终点侧**的事实。
//!
//! 前提与 TCP 那套一样：全离线、不需要 root、不需要节点（`dns.hosts` 把域名指到 127.0.0.1）。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::xray::{build_pretty, merge_rules_with_intent, CoreConfigInput, InboundProfile};
use xt_intent::rules::{materialize, RuleOptions};
use xt_intent::verdict::{BlockVerdict, Category, Verdict};

const BLOCKED: &str = "blocked.udp-dataplane";
const ALLOWED: &str = "allowed.udp-dataplane";

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
    l.local_addr().unwrap().port()
}

/// 本地 UDP 回显服务：**收到几个包就记几笔**。
///
/// 计数是这条测试最关键的东西：它记的是"包有没有真的走到终点"，
/// 而客户端那边的"没收到回复"分不清"被拦"和"路径坏了"。
struct UdpEcho {
    port: u16,
    hits: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl UdpEcho {
    fn start() -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("绑 UDP 回显");
        let port = sock.local_addr().unwrap().port();
        sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (h2, s2) = (hits.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while !s2.load(Ordering::Relaxed) {
                match sock.recv_from(&mut buf) {
                    Ok((_n, from)) => {
                        h2.fetch_add(1, Ordering::Relaxed);
                        let _ = sock.send_to(b"pong", from);
                    }
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
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

impl Drop for UdpEcho {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 走一次 SOCKS5 UDP ASSOCIATE，把 `payload` 发给 `host:port`，等最多 `timeout`。
///
/// 返回 `Some(回包字节)` 或 `None`（超时/没有回包）。RFC 1928 §7 的封装：
/// 每个数据报前面挂 `RSV(2) FRAG(1) ATYP(1) ADDR PORT`。
fn udp_echo_via(
    socks_port: u16,
    host: &str,
    port: u16,
    payload: &[u8],
    timeout: Duration,
) -> Option<Vec<u8>> {
    // ① TCP 控制连接 + 无认证握手
    let mut ctl = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], socks_port)),
        Duration::from_secs(5),
    )
    .expect("连 socks 控制连接");
    ctl.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    ctl.write_all(&[5, 1, 0]).unwrap();
    let mut hello = [0u8; 2];
    ctl.read_exact(&mut hello).expect("读握手应答");
    assert_eq!(hello, [5, 0], "socks 无认证握手应当成功");

    // ② UDP ASSOCIATE：ATYP=1（IPv4）+ 0.0.0.0:0 —— 地址由服务端告诉我们
    ctl.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
    let mut head = [0u8; 4];
    ctl.read_exact(&mut head).expect("读 ASSOCIATE 应答");
    assert_eq!(head[1], 0, "UDP ASSOCIATE 应当成功，reply={}", head[1]);
    let relay = match head[3] {
        1 => {
            let mut b = [0u8; 6];
            ctl.read_exact(&mut b).unwrap();
            SocketAddr::from((Ipv4Addr::new(b[0], b[1], b[2], b[3]), u16::from_be_bytes([b[4], b[5]])))
        }
        4 => {
            let mut b = [0u8; 18];
            ctl.read_exact(&mut b).unwrap();
            // 测试里不会发生；真发生就明说，不要装作会解析。
            panic!("收到 IPv6 relay 地址，本测试不解析：{b:?}");
        }
        other => panic!("不认识的 ATYP={other}"),
    };

    // ③ 一个数据报：RSV + FRAG=0 + ATYP=3（域名）+ len + host + port + payload
    let client = UdpSocket::bind("127.0.0.1:0").expect("绑本地 UDP 客户端");
    client.set_read_timeout(Some(timeout)).unwrap();
    let mut pkt = vec![0u8, 0, 0, 3, host.len() as u8];
    pkt.extend_from_slice(host.as_bytes());
    pkt.extend_from_slice(&port.to_be_bytes());
    pkt.extend_from_slice(payload);
    client.send_to(&pkt, relay).expect("发 UDP 数据报");

    let mut buf = [0u8; 4096];
    match client.recv_from(&mut buf) {
        Ok((n, _)) => {
            // 剥掉 SOCKS UDP 头：4 字节 + 地址 + 端口。
            let body_start = match buf.get(3).copied() {
                Some(1) => 4 + 6,
                Some(3) => 4 + 1 + buf[4] as usize + 2,
                _ => return None,
            };
            Some(buf[body_start.min(n)..n].to_vec())
        }
        Err(_) => None,
    }
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
fn an_intent_block_rule_also_drops_udp_on_a_real_core() {
    let Some(core) = core_binary() else {
        eprintln!("SKIP：没找到真实核心（XT_CORE 未设，两个默认路径都不存在）");
        return;
    };

    let echo = UdpEcho::start();
    let socks = free_port();
    let http = free_port();
    let api = free_port();

    let mut s = AppSettings {
        routing_preset: RoutingPreset::BypassMainland,
        socks_port: socks,
        http_port: http,
        ..Default::default()
    };
    s.dns.hosts = vec![
        (BLOCKED.to_string(), "127.0.0.1".to_string()),
        (ALLOWED.to_string(), "127.0.0.1".to_string()),
    ];
    s.dns.direct_servers = vec!["223.5.5.5".to_string()];
    s.dns.remote_servers = vec!["https://1.1.1.1/dns-query".to_string()];

    // 与 TCP 那条测试**同一套物化路径**：判决 → 规则。规则里刻意**不设 `network`**，
    // 于是"TCP+UDP 都匹配"这件事是本测试要验证的结论，而不是我们的假设。
    let block = Verdict::Block(BlockVerdict {
        category: Category::AdOrMonetization,
        ads_intent: 0.97,
        risk_of_breakage: 0.05,
        choice_confidence: 0.93,
        effective_min: 0.85,
    });
    let rules = materialize(vec![(BLOCKED, &block)], &RuleOptions::default());
    let merged = merge_rules_with_intent(&s, &rules.allow, &rules.block);

    // 规则本身不设 network ⇒ 两条都要能命中 UDP 与 TCP。
    let intent_rule = merged
        .iter()
        .find(|r| r.id.starts_with("intent-block-"))
        .expect("必须有一条意图拦截规则");
    // `Network::Both` = Xray 配置里**不写** `network` 字段 = TCP+UDP 都匹配。
    // 正因为它是默认值，"UDP 也会被拦"才是结论而不是假设。
    assert_eq!(
        intent_rule.when.network,
        xt_core::routing::Network::Both,
        "意图规则不该把自己限制在某个协议上（留默认 = TCP+UDP 都匹配）"
    );

    let mut cfg: Value = serde_json::from_str(&build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &merged,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    }))
    .expect("生成的配置必须是合法 JSON");
    for i in cfg["inbounds"].as_array_mut().unwrap().iter_mut() {
        if i["tag"] == "api" {
            i["port"] = Value::from(api);
        }
    }
    // 前置条件：socks 入站必须开着 UDP，否则这个测试什么都验不到。
    let socks_in = cfg["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["tag"] == "socks")
        .expect("必须有 socks 入站");
    assert_eq!(
        socks_in["settings"]["udp"], Value::Bool(true),
        "socks 入站没开 UDP ⇒ 下面那条对照断言必然失败，别把失败归因到规则上"
    );

    let dir = std::env::temp_dir().join(format!("xt-udp-dataplane-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("udp.json");
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
        panic!("核心没起来（socks={socks} api={api}）：\n{err}");
    }
    println!("核心已就绪：socks={socks} udp-echo={}", echo.port);

    // ---- ① 对照组：这条路本来是通的（不然"没回包"什么都证明不了）----
    let ok = udp_echo_via(socks, ALLOWED, echo.port, b"ping", Duration::from_secs(3));
    assert_eq!(
        ok.as_deref(),
        Some(&b"pong"[..]),
        "对照组的 UDP 都没通 ⇒ 这个测试的前提不成立（别拿它去证明规则生效）"
    );
    assert_eq!(echo.hits(), 1, "对照组应当恰好摸到终点一次");
    println!("① 对照组 {ALLOWED}：UDP 往返成功 ✓");

    // ---- ② 实验组：不设 network 的意图规则必须把 UDP 一起拦掉 ----
    //
    // 实测事实（这条断言就是证据）：客户端**会**收到一个回包，但那是 blackhole 的
    // 403 应答字节 —— 不是终点的 `pong`。所以这里两条都要断言：
    // ① 收到的不能是 pong（否则规则没生效）；② 终点计数不能涨（否则包漏到了终点）。
    let blocked =
        udp_echo_via(socks, BLOCKED, echo.port, b"ping", Duration::from_millis(1500));
    let blocked_text = blocked
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).to_string())
        .unwrap_or_default();
    assert_ne!(
        blocked.as_deref(),
        Some(&b"pong"[..]),
        "被拦的域名收到了终点的 pong —— 规则对 UDP 没生效"
    );
    assert!(
        blocked_text.contains("403") || blocked_text.contains("Forbidden"),
        "按本项目的配置（`block` = blackhole + `response.type=http`），UDP 客户端应当收到\
         那个 403 应答字节；收到别的说明配置变了，需要重新确认：{blocked_text:?}"
    );
    assert_eq!(
        echo.hits(),
        1,
        "**终点侧的强断言**：被拦的 UDP 一个包都不该到回显服务（现在 {} 次）",
        echo.hits()
    );
    println!("② 实验组 {BLOCKED}：没到终点、收到的是 blackhole 的 403（不是静默丢弃）✓");

    // ---- ③ 负对照：再发一次对照组，证明"实验组没拿到真应答"不是路径整体坏了 ----
    let ok2 = udp_echo_via(socks, ALLOWED, echo.port, b"ping", Duration::from_secs(3));
    assert_eq!(ok2.as_deref(), Some(&b"pong"[..]), "第二次对照也必须通");
    assert_eq!(echo.hits(), 2);
    println!("③ 再次对照：仍然通（说明 ② 里终点没被摸到是拦截造成的，不是路径坏了）✓");

    let _ = std::fs::remove_dir_all(&dir);
}
