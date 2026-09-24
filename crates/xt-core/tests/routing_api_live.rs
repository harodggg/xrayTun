//! `RoutingService` 的真实核心验收（P2b-3 的 Definition of Done）。
//!
//! # 为什么这一条必须跑真实核心
//!
//! 手写的 protobuf 有三个"看起来成功、其实什么也没做"的失败模式：
//! 字段号写错、`Domain.Type` 用了 `Substr(0)` 而不是 `Full(3)`、
//! gRPC 的错误只出现在 **trailers** 里（HTTP 200 也可能是失败）。
//! 单测只能钉住字节，钉不住"核心真的照它改了路由"。所以这里对**真实核心**做五件事：
//!
//! 1. `ListRule` 能读出**真实的**规则表（不是空列表）；
//! 2. `AddRule` 之后 `ListRule` 里能看到那条 `ruleTag`；
//! 3. **新连接**到被拦域名 ⇒ 拿不到 200（对照组域名照常 200）；
//! 4. **已建立的连接不断** —— 这是相对"重启核心"的全部价值所在，也是本条的 DoD；
//! 5. 负对照：`RemoveRule` 之后同一个域名**恢复**可达
//!    （证明第 3 步的"拿不到 200"不是因为规则压根没生效，而是真的被拦了）。
//!
//! 全离线、端口全现取（本机实测有正在跑的 xray 占着 10808/10809/10085）、
//! `Drop` 守卫收尾。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use xt_core::xray::routing_api::{
    add_rules, list_rules, remove_rules, replace_rules, ApiDomain, ApiRule,
};

const TCP: u64 = 2;
const UDP: u64 = 3;

/// 只带域名的一条规则（其余字段留空 = 不匹配）。
fn domain_rule(rule_tag: &str, outbound: &str, domain: &str, inbounds: &[&str]) -> ApiRule {
    ApiRule {
        domains: vec![ApiDomain::Full(domain.to_string())],
        inbound_tags: inbounds.iter().map(|s| s.to_string()).collect(),
        ..ApiRule::new(rule_tag, outbound)
    }
}

const ALLOWED: &str = "allowed.intent-live";
const BLOCKED: &str = "blocked.intent-live";
const RULE_TAG: &str = "intent-block-blocked.intent-live";

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
    l.local_addr().unwrap().port()
}

/// 会 **keep-alive** 的本地 HTTP 服务：同一个连接上可以连续发多个请求。
///
/// 这一点是刻意的：`已建立的连接不断` 这条 DoD 必须能"在同一个 socket 上再发一次",
/// 而"每条请求都新建连接"的服务器根本区分不出这两种情况。
struct KeepAliveServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl KeepAliveServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地服务");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((sock, _)) => {
                        let stop3 = stop2.clone();
                        workers.push(std::thread::spawn(move || {
                            serve_keep_alive(sock, stop3);
                        }));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            for w in workers {
                let _ = w.join();
            }
        });
        Self { port, stop, handle: Some(handle) }
    }
}

fn serve_keep_alive(mut sock: TcpStream, stop: Arc<AtomicBool>) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 2048];
    let mut carry: Vec<u8> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match sock.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                // 每收到一个完整的请求头就回一次 200（不带 Connection: close ⇒ keep-alive）。
                while let Some(end) = carry.windows(4).position(|w| w == b"\r\n\r\n") {
                    carry.drain(..end + 4);
                    if sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                    let _ = sock.flush();
                }
            }
            Err(ref e)
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(_) => return,
        }
    }
}

impl Drop for KeepAliveServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
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

/// 手写一份最小配置：**只为了测 API**，所以刻意不用生成的配置 ——
/// 被测对象是 `RoutingService`，配置越少读起来越清楚。
///
/// 本地服务端口**不进配置**：两个测试域名由 `dns.hosts` 映射到 127.0.0.1，
/// 端口由客户端（SOCKS 请求里）指定。
///
/// `direct` 出站必须带 `streamSettings.sockopt.domainStrategy = "UseIPv4"`
/// （与 `config.rs:513` 一致）：否则 freedom 用**系统**解析器解析目标域名，
/// 而 `dns.hosts` 的映射在内核 DNS 模块里 —— 表现是"连上了但一个字节都没有"。
/// 第一版就是这么红的。
fn config_json(socks: u16, api: u16) -> String {
    format!(
        r#"{{
  "log": {{ "loglevel": "warning" }},
  "api": {{ "tag": "api", "services": ["RoutingService"] }},
  "dns": {{
    "hosts": {{ "domain:{ALLOWED}": "127.0.0.1", "domain:{BLOCKED}": "127.0.0.1" }},
    "servers": ["223.5.5.5"]
  }},
  "inbounds": [
    {{ "tag": "socks", "port": {socks}, "listen": "127.0.0.1", "protocol": "socks", "settings": {{ "udp": true }} }},
    {{ "tag": "api-in", "port": {api}, "listen": "127.0.0.1", "protocol": "dokodemo-door", "settings": {{ "address": "127.0.0.1" }} }}
  ],
  "outbounds": [
    {{ "tag": "direct", "protocol": "freedom", "streamSettings": {{ "sockopt": {{ "domainStrategy": "UseIPv4" }} }} }},
    {{ "tag": "block", "protocol": "blackhole", "settings": {{ "response": {{ "type": "none" }} }} }},
    {{ "tag": "api", "protocol": "freedom" }}
  ],
  "routing": {{
    "domainStrategy": "IPIfNonMatch",
    "rules": [
      {{ "type": "field", "inboundTag": ["api-in"], "outboundTag": "api", "ruleTag": "internal-api" }},
      {{ "type": "field", "domain": ["full:{ALLOWED}"], "outboundTag": "direct", "ruleTag": "test-allow" }},
      {{ "type": "field", "network": "tcp,udp", "outboundTag": "direct", "ruleTag": "internal-fallback" }}
    ]
  }}
}}"#
    )
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

/// 一条**长期持有**的 SOCKS 连接：握手一次，之后可以反复发请求。
struct SocksConn {
    sock: TcpStream,
    host: String,
    port: u16,
}

impl SocksConn {
    fn open(socks: u16, host: &str, port: u16, timeout: Duration) -> std::io::Result<Self> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], socks));
        let mut sock = TcpStream::connect_timeout(&addr, timeout)?;
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;

        sock.write_all(&[5, 1, 0])?;
        let mut hello = [0u8; 2];
        sock.read_exact(&mut hello)?;
        assert_eq!(hello, [5, 0], "SOCKS5 握手失败");

        let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
        req.extend_from_slice(host.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        sock.write_all(&req)?;

        let mut head = [0u8; 4];
        sock.read_exact(&mut head)?;
        if head[1] != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("SOCKS5 被拒 reply={}", head[1]),
            ));
        }
        match head[3] {
            1 => {
                let mut b = [0u8; 6];
                sock.read_exact(&mut b)?;
            }
            4 => {
                let mut b = [0u8; 18];
                sock.read_exact(&mut b)?;
            }
            _ => {
                let mut l = [0u8; 1];
                sock.read_exact(&mut l)?;
                let mut b = vec![0u8; l[0] as usize + 2];
                sock.read_exact(&mut b)?;
            }
        }
        Ok(Self { sock, host: host.to_string(), port })
    }

    /// 在**同一个**连接上再发一次 GET，返回响应体（静默失败时返回空串）。
    fn get(&mut self, timeout: Duration) -> std::io::Result<String> {
        self.sock.set_read_timeout(Some(timeout))?;
        let req = format!(
            "GET / HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
            self.host, self.port
        );
        self.sock.write_all(req.as_bytes())?;
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.sock.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    // 一次响应读完（Content-Length: 2）就够判断了。
                    if out.windows(4).any(|w| w == b"\r\n\r\n") && out.len() >= 4 {
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
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xt-routing-api-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// **必须是多线程 runtime**：这个用例里既有 `await` 的 gRPC 调用，也有同步阻塞的
/// SOCKS 收发。单线程 runtime 上阻塞那几秒会饿死 h2 连接驱动任务，表现是"卡住不动"。
/// 两个 worker 就够：一个跑驱动，一个跑本体的阻塞 IO。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_and_removing_rules_live_never_drops_an_existing_connection() {
    let Some(core) = core_binary() else {
        eprintln!(
            "SKIP：没找到真实核心（XT_CORE 未设，apps/desktop/binaries/xray 与 \
             .scratch/xray-server/xray 都不存在）。P2b-3 的 DoD 必须手动跑一次。"
        );
        return;
    };

    let server = KeepAliveServer::start();
    let socks = free_port();
    let api = free_port();
    let dir = temp_dir("live");
    let cfg = dir.join("routing-api.json");
    std::fs::write(&cfg, config_json(socks, api)).unwrap();

    let child = Command::new(&core)
        .args(["run", "-c"])
        .arg(&cfg)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("必须能启动核心");
    let mut guard = CoreGuard { child };
    if !wait_for_port(socks, Duration::from_secs(10)) || !wait_for_port(api, Duration::from_secs(5)) {
        let mut err = String::new();
        if let Some(mut e) = guard.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        panic!("核心没起来（socks={socks} api={api}）：\n{err}");
    }
    let api_addr: std::net::SocketAddr = ([127, 0, 0, 1], api).into();
    let budget = Duration::from_secs(5);
    println!("真实核心已就绪：socks={socks} api={api} web={}", server.port);

    // ---- 1) ListRule 能读出真实规则表 ----
    let baseline = list_rules(api_addr, budget).await.expect("ListRule 必须成功");
    let tags: Vec<&str> = baseline.iter().map(|i| i.rule_tag.as_str()).collect();
    assert!(
        tags.contains(&"internal-fallback"),
        "读不到配置里的规则说明 API 没真的连上：{tags:?}"
    );
    println!("ListRule 基线：{} 条（含 internal-fallback）", baseline.len());

    // ---- 2) 先建立一条**长期连接**（后面要证明它不断） ----
    let mut live_conn = SocksConn::open(socks, ALLOWED, server.port, Duration::from_secs(5))
        .expect("对照组连接必须能建立");
    let first = live_conn.get(Duration::from_secs(3)).unwrap_or_default();
    assert!(first.contains("200"), "对照组第一次请求应当是 200：{first:?}");
    println!("长期连接已建立并在用（第一次 200）");

    // ---- 3) **追加**规则：本机实测会排在 catch-all 之后 ⇒ 永不命中 ----
    //
    // 这不是"实现没写好"，而是机制本身的约束：生成的配置末尾永远有一条
    // `internal-fallback`（catch-all），而 `AddRule` 是**追加**。把这一点钉成
    // 可复跑的断言，比在文档里写一句"注意顺序"有用得多。
    let rule = domain_rule(RULE_TAG, "block", BLOCKED, &["socks"]);
    add_rules(api_addr, std::slice::from_ref(&rule), budget)
        .await
        .expect("AddRule 必须成功");
    let after_add = list_rules(api_addr, budget).await.unwrap();
    assert!(
        after_add.iter().any(|i| i.rule_tag == RULE_TAG),
        "AddRule 之后 ListRule 里必须有它"
    );
    assert_eq!(after_add.len(), baseline.len() + 1, "追加只该多一条");
    for item in &baseline {
        assert!(
            after_add.iter().any(|i| i.rule_tag == item.rule_tag),
            "既有规则 {} 被弄丢了（shouldAppend 写错会整份替换）",
            item.rule_tag
        );
    }
    println!("AddRule 成功：{} → {} 条（追加，未整份替换）", baseline.len(), after_add.len());

    let appended = test_get(socks, BLOCKED, server.port);
    assert!(
        appended.contains("200"),
        "追加的规则居然生效了 —— 那说明 catch-all 的位置变了，附录 A 第 7 条要重写"
    );
    println!("追加的规则**被 catch-all 吃掉**（拿到 200）—— 预期行为，见附录 A 第 7 条");

    // 删掉它，回到基线。
    remove_rules(api_addr, &[RULE_TAG.to_string()], budget)
        .await
        .expect("RemoveRule 必须成功");
    assert_eq!(
        list_rules(api_addr, budget).await.unwrap().len(),
        baseline.len(),
        "删掉之后应当回到基线"
    );

    // ---- 4) **整份替换**：唯一能真正让规则生效的原语（且顺序由我们决定） ----
    //
    // 注意这份列表是"完整规则表"：三条原样保留 + 一条插在 catch-all **之前**。
    // 生产里这一步要求把 `build_routing` 的产物逐条编码；目前的 `ApiRule` 只覆盖
    // domain(full:) / inbound_tag / networks / outbound（见 `replace_rules` 文档）。
    let full = vec![
        ApiRule {
            inbound_tags: vec!["api-in".into()],
            ..ApiRule::new("internal-api", "api")
        },
        rule.clone(),
        domain_rule("test-allow", "direct", ALLOWED, &[]),
        ApiRule { networks: vec![TCP, UDP], ..ApiRule::new("internal-fallback", "direct") },
    ];
    replace_rules(api_addr, &full, budget)
        .await
        .expect("整份替换必须成功");
    let after_replace = list_rules(api_addr, budget).await.unwrap();
    assert_eq!(
        after_replace.len(),
        full.len(),
        "替换之后条数应当一致：{:?}",
        after_replace.iter().map(|i| &i.rule_tag).collect::<Vec<_>>()
    );
    println!("整份替换成功：{} 条（拦截规则排在 catch-all 之前）", after_replace.len());

    // ---- 5) DoD：新连接被拦，而**已建立的连接照常** ----
    let blocked_new = test_get(socks, BLOCKED, server.port);
    assert!(
        !blocked_new.contains("200"),
        "整份替换之后，新连接居然还是拿到了 200：{blocked_new:?}"
    );
    println!("新连接 {BLOCKED} → 未拿到 200 ✓");

    let second = live_conn.get(Duration::from_secs(3)).unwrap_or_default();
    assert!(
        second.contains("200"),
        "**已建立的连接被规则变更打断了** —— 这正是热加要避免的事：{second:?}"
    );
    println!("已建立的连接在规则变更后仍然 200 ✓（这就是 DoD）");

    // ---- 6) 负对照：把拦截规则拿掉之后恢复可达 ----
    let without_block: Vec<ApiRule> = full.iter().filter(|r| r.rule_tag != RULE_TAG).cloned().collect();
    replace_rules(api_addr, &without_block, budget)
        .await
        .expect("再替换一次必须成功");
    let recovered = test_get(socks, BLOCKED, server.port);
    assert!(
        recovered.contains("200"),
        "拿掉规则之后必须恢复可达（否则上面那次'拦住了'是别的原因）：{recovered:?}"
    );
    println!("拿掉规则之后 {BLOCKED} 恢复 200 ✓（负对照成立）");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 一次性的"开一条新连接 → 发一个请求"（用来看**新**连接是否被拦）。
fn test_get(socks: u16, host: &str, port: u16) -> String {
    match SocksConn::open(socks, host, port, Duration::from_secs(3)) {
        Ok(mut c) => c.get(Duration::from_secs(2)).unwrap_or_default(),
        Err(e) => format!("<{e}>"),
    }
}
