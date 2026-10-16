//! 真 xray 端到端：真 HTTP 源站 + 真 xray 服务端 + daemon + 真 SOCKS5 请求。
//!
//! 被测路径上**没有任何 mock**：源站是真 socket、服务端是真 xray 进程、代理跳是
//! 真的 vless 环回、daemon 走真 AF_UNIX 帧、字节数取自真 StatsService。
//!
//! 无等待：所有等待都是「事件 + timeout 上限」。整个文件没有 sleep，
//! 也没有「等一会儿再看看」的轮询。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use xt_contract::error::{ErrorBody, ErrorCode};
use xt_contract::model::{
    Capability, ConnectionView, LogLevel, NodeSource, ProbeResult, RunMode, Stage, SubscriptionId,
    SubscriptionView,
};
use xt_contract::protocol::{Event, Request, Response};
use xt_daemon::{Daemon, DaemonConfig};
use xt_datapath::DatapathSpec;

/// 源站返回的固定字节数。64KiB 足够跨越 Xray 的拷贝缓冲，能看出计数是否完整。
const PAYLOAD_LEN: usize = 64 * 1024;

/// 连接/切节点等待上限（失败上限，不是轮询周期）。
const CONNECT_DEADLINE: Duration = Duration::from_secs(15);
/// 断开等待上限。
const STOP_DEADLINE: Duration = Duration::from_secs(10);
/// 探测等待上限。
const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// 测试用节点凭据。**这是测试数据，不是密钥**，只在本机环回的一次性实例里用。
const UUID_1: &str = "11111111-1111-1111-1111-111111111111";
const UUID_2: &str = "22222222-2222-2222-2222-222222222222";

/// 真 xray 二进制：`XT_XRAY_BIN`（或旧名 `XRAY_BIN`）→ PATH 里的 `xray`。
///
/// 两条纪律：
/// 1. **不写死绝对路径**。这里曾默认指向某台开发机上的
///    `/Users/.../.scratch/bin/xray`，一进 CI 就红 —— 公开仓库里不该有某个人的机器布局。
/// 2. **找不到就失败，不静默跳过**。"跳过"会让"端到端通过"这句话失去依据。
fn xray_bin() -> PathBuf {
    for key in ["XT_XRAY_BIN", "XRAY_BIN"] {
        if let Ok(path) = std::env::var(key) {
            let path = PathBuf::from(path);
            assert!(path.is_file(), "{key} 指向的文件不存在：{}", path.display());
            return path;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("xray");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "端到端测试需要真 xray 二进制：设置 XT_XRAY_BIN=/path/to/xray，或把它放进 PATH。\
         （本测试不做静默跳过：跳过会让结论失去依据。）"
    );
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xt-daemon-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn expected_payload() -> Vec<u8> {
    (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect()
}

/// 找一对连续的空闲端口：daemon 的 socks 与 api（= socks + 1）。
///
/// 先绑再放是唯一能拿到「真的空闲」的办法；绑不上就换一个，不用等待。
fn free_port_pair() -> u16 {
    for _ in 0..200 {
        let Ok(first) = std::net::TcpListener::bind("127.0.0.1:0") else {
            continue;
        };
        let port = first.local_addr().expect("本地地址").port();
        if port >= u16::MAX - 1 {
            continue;
        }
        let Ok(second) = std::net::TcpListener::bind(("127.0.0.1", port + 1)) else {
            continue;
        };
        drop(second);
        drop(first);
        return port;
    }
    panic!("找不到一对空闲端口");
}

/// 真 HTTP 源站：真 socket、确定字节、`Connection: close`。
async fn start_origin() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定源站");
    let addr = listener.local_addr().expect("源站地址");
    let body = Arc::new(expected_payload());
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let body = Arc::clone(&body);
            tokio::spawn(async move {
                // 读到请求头结束即可：我们只服务最简单的 GET。
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
            });
        }
    });
    (addr, handle)
}

/// 订阅源站：最小 HTTP/1.1，只用 `std::net::TcpListener` + `std::thread`（**零新增依赖**）。
///
/// 为什么不用 tokio（本文件上面那个 `start_origin` 就是 tokio 版）：订阅拉取走的是
/// `/usr/bin/curl` —— 一个**真子进程**。服务端跑在独立的 OS 线程上，测试的重心就只剩
/// 「daemon 真的把正文拉回来了」这一件事，不需要再往运行时里塞一个服务任务。
///
/// 返回 `(地址, 命中计数)`。计数是「真的联网拉了一次」的证据：断言它 1 → 2，
/// 才能区分「刷新重新拉了」和「刷新只是把内存里的旧节点又写了一遍」。
///
/// 前提：运行环境没有给 curl 设 `http_proxy`（CI 的 Linux/macOS 默认不设）。
/// 生产代码 `fetch_body` 不传 `--noproxy` —— 测试不替它绕开，否则证的不是同一条路径。
fn start_subscription_server(body: String) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定订阅源站");
    let addr = listener.local_addr().expect("订阅源站地址");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        use std::io::{Read, Write};

        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { break };
            // 读到请求头结束就够：我们只认「有一次请求到达」这个事实。
            let mut head = Vec::new();
            let mut chunk = [0u8; 512];
            loop {
                match socket.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        head.extend_from_slice(&chunk[..n]);
                        if head.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            counter.fetch_add(1, Ordering::SeqCst);
            // `Content-Length` 必须按**字节数**算，`Connection: close` 让 curl 知道读到尾。
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes());
            let _ = socket.flush();
        }
    });
    (addr, hits)
}

/// 一次真 SOCKS5 GET：最小握手 + CONNECT + HTTP，返回响应体字节。
async fn socks5_get(socks: SocketAddr, host: &str, port: u16, path: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(socks).await.expect("连接 daemon 的 socks 入站");
    stream.set_nodelay(true).expect("nodelay");

    // 1) 无认证协商
    stream.write_all(&[0x05, 0x01, 0x00]).await.expect("协商写入");
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await.expect("协商响应");
    assert_eq!(greeting, [0x05, 0x00], "daemon 的 socks 入站必须接受无认证");

    // 2) CONNECT 到 127.0.0.1:origin
    let ip: std::net::Ipv4Addr = host.parse().expect("测试里只连 IPv4");
    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    request.extend_from_slice(&ip.octets());
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await.expect("CONNECT 写入");
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.expect("CONNECT 应答");
    assert_eq!(head[1], 0x00, "CONNECT 必须成功（reply={}）", head[1]);
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            stream.read_exact(&mut skip).await.expect("BND.ADDR");
        }
        0x04 => {
            let mut skip = [0u8; 18];
            stream.read_exact(&mut skip).await.expect("BND.ADDR");
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.expect("BND.ADDR 长度");
            let mut skip = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await.expect("BND.ADDR");
        }
        other => panic!("未知地址类型 {other}"),
    }

    // 3) HTTP GET，读到对端关闭
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("请求写入");
    let mut all = Vec::new();
    stream.read_to_end(&mut all).await.expect("读取响应体");
    let split = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("响应必须包含头体分隔");
    all[split + 4..].to_vec()
}

/// 等一个目标阶段。事件先到就立刻返回；`deadline` 只是失败上限。
async fn wait_for_stage(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    want: Stage,
    deadline: Duration,
) -> ConnectionView {
    wait_for_state(events, &format!("{want:?}"), move |view| view.stage == want, deadline).await
}

/// 等一个满足断言的状态事件。
///
/// 为什么要谓词而不是只比 stage：`Status` 会发布一条带当前 stats 的 State 事件，
/// 它可能还排在事件队列里；切节点时若只等 `Connected`，会命中**切换前**那条。
/// 谓词让「等的是哪一次 Connected」这件事是明确的。
async fn wait_for_state<F>(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    what: &str,
    predicate: F,
    deadline: Duration,
) -> ConnectionView
where
    F: Fn(&ConnectionView) -> bool,
{
    let wait = async {
        loop {
            match events.recv().await {
                Ok(Event::State { view }) => {
                    eprintln!(
                        "[e2e] State stage={:?} phase={:?} pid={:?} stats={:?} last_error={:?}",
                        view.stage, view.phase, view.datapath.pid, view.stats, view.last_error
                    );
                    if predicate(&view) {
                        return view;
                    }
                }
                Ok(Event::Log { line }) => eprintln!("[e2e][{}] {}", line.target, line.message),
                Ok(Event::Probe { result }) => eprintln!("[e2e] Probe {result:?}"),
                Ok(Event::Notice { notice }) => {
                    eprintln!("[e2e] Notice {} {}", notice.code, notice.message)
                }
                Err(RecvError::Lagged(skipped)) => eprintln!("[e2e] 事件落后 {skipped} 条"),
                Err(RecvError::Closed) => panic!("事件通道被关闭"),
            }
        }
    };
    match tokio::time::timeout(deadline, wait).await {
        Ok(view) => view,
        Err(_) => panic!("等待 {what} 超过上限 {deadline:?}（daemon 事件见上方输出）"),
    }
}

/// 等够 `count` 条探测结果。
async fn wait_for_probes(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    count: usize,
    deadline: Duration,
) -> Vec<ProbeResult> {
    let wait = async {
        let mut results = Vec::new();
        while results.len() < count {
            match events.recv().await {
                Ok(Event::Probe { result }) => {
                    eprintln!("[e2e] Probe {result:?}");
                    results.push(result);
                }
                Ok(Event::State { view }) => {
                    eprintln!("[e2e] State(探测期间) stage={:?}", view.stage)
                }
                Ok(Event::Log { line }) => eprintln!("[e2e][{}] {}", line.target, line.message),
                Ok(Event::Notice { notice }) => {
                    eprintln!("[e2e] Notice {} {}", notice.code, notice.message)
                }
                Err(RecvError::Lagged(skipped)) => eprintln!("[e2e] 事件落后 {skipped} 条"),
                Err(RecvError::Closed) => panic!("事件通道被关闭"),
            }
        }
        results
    };
    match tokio::time::timeout(deadline, wait).await {
        Ok(results) => results,
        Err(_) => panic!("等待 {count} 条探测结果超时"),
    }
}

fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// 只跑**控制面**的 daemon 夹具：从不 `Connect`，因此不需要真 xray 二进制。
///
/// 为什么不复用 `real_xray_end_to_end` 里的 daemon：那条路径证的是数据面，前提是真 xray
/// 存在；而「远端订阅拉取」证的是控制面 + `/usr/bin/curl`。把两者揉在一起，会让
/// 「订阅拉取坏了」和「CI 机器上没装 xray」变成同一条红 —— 排查时没有信息量。
struct ControlHarness {
    dir: PathBuf,
    client: xt_ipc::Client,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    serve: tokio::task::JoinHandle<Result<(), ErrorBody>>,
}

impl ControlHarness {
    /// `local_subscription` 传 `Some` 时会写成 `--subscription-file`：daemon 启动时把它
    /// 读成 id = `"local"` 的那条订阅（用来证明「local 存在也不许刷新」，而不是
    /// 「local 不存在所以报错」这种假通过）。
    async fn start(tag: &str, local_subscription: Option<&str>) -> ControlHarness {
        ControlHarness::start_with_saved_subscriptions(tag, local_subscription, None).await
    }

    /// `saved_subscriptions` 会先落成 `subscriptions.json` —— 模拟「上次运行留下的状态」。
    /// 用来验证启动时的**校正**：`--subscription-file` 已经不在时，那份 `local` 记录必须
    /// 被摘掉，它的节点也不许留在目录里（否则界面会「列表里没有、目录里一直有」）。
    async fn start_with_saved_subscriptions(
        tag: &str,
        local_subscription: Option<&str>,
        saved_subscriptions: Option<&str>,
    ) -> ControlHarness {
        let dir = temp_dir(tag);
        let subscription_file = dir.join("subscription.txt");
        if let Some(body) = local_subscription {
            std::fs::write(&subscription_file, body).expect("写本地订阅文件");
        }
        if let Some(state) = saved_subscriptions {
            std::fs::write(dir.join("subscriptions.json"), state).expect("写订阅状态文件");
        }
        let socket_path = dir.join("daemon.sock");
        let daemon = Daemon::bootstrap(DaemonConfig {
            socket_path: socket_path.clone(),
            state_dir: dir.clone(),
            // 本夹具从不 Connect：这个字段只有真的拉起核心进程时才会被读。
            xray_bin: dir.join("xray-not-installed"),
            log_level: LogLevel::Info,
            subscription_file,
            // 本夹具从不 ProbeNodes：同理，给它一个不会有人访问的环回地址。
            probe_url: "http://127.0.0.1:1/generate_204".to_string(),
        })
        .await
        .expect("daemon 装配");
        // bind() 返回即监听已建立 —— 与主测试同一条可观测事实，不需要等文件出现。
        let bound = daemon.bind().await.expect("绑定 AF_UNIX socket");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let serve = tokio::spawn(async move { bound.serve(shutdown_rx).await });
        let client = xt_ipc::Client::connect(&socket_path, "subs-e2e")
            .await
            .expect("IPC 连接 daemon");
        ControlHarness { dir, client, shutdown_tx: Some(shutdown_tx), serve }
    }

    /// 收尾：发关闭信号 → 等 serve 真的返回（有上限，不是轮询）→ 删临时目录。
    /// 断言中途失败时不会走到这里（临时目录留给排查），与主测试口径一致。
    async fn shutdown(self) {
        // 一次解构，避免「部分移动的 self 跨 await」这类没有价值的编译期麻烦。
        let ControlHarness { dir, client, shutdown_tx, serve } = self;
        // 先放开 IPC 连接：之后不再发请求，让 daemon 那边那条连接自然结束。
        drop(client);
        if let Some(tx) = shutdown_tx {
            let _ = tx.send(());
        }
        let _ = tokio::time::timeout(STOP_DEADLINE, serve).await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 取一次订阅列表。形状断言集中在这里，测试体只关心用户可见的事实。
async fn list_subscriptions(client: &xt_ipc::Client) -> Vec<SubscriptionView> {
    match client.request(Request::ListSubscriptions).await.expect("ListSubscriptions 应答") {
        Response::Subscriptions { subscriptions } => subscriptions,
        other => panic!("ListSubscriptions 期望 Subscriptions，收到 {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_xray_end_to_end() {
    let xray = xray_bin();
    let dir = temp_dir("run");

    // ---- 1. 真 HTTP 源站 ----------------------------------------------------
    let (origin_addr, origin_handle) = start_origin().await;
    eprintln!("[e2e] 源站 http://{origin_addr}（{PAYLOAD_LEN} 字节固定内容）");

    // ---- 2. 真 xray 服务端（vless inbound + freedom outbound，环回）---------
    let server_port = free_port_pair();
    let server_addr = SocketAddr::from(([127, 0, 0, 1], server_port));
    let server_config = serde_json::json!({
        "log": { "loglevel": "warning" },
        "inbounds": [{
            "tag": "vless-in",
            "listen": "127.0.0.1",
            "port": server_port,
            "protocol": "vless",
            "settings": {
                "clients": [{ "id": UUID_1 }, { "id": UUID_2 }],
                "decryption": "none"
            },
            "streamSettings": { "network": "tcp" }
        }],
        "outbounds": [{ "tag": "freedom-out", "protocol": "freedom" }]
    });
    let server_config_path = dir.join("server.json");
    std::fs::write(&server_config_path, serde_json::to_vec_pretty(&server_config).unwrap())
        .expect("写服务端配置");
    // 服务端就绪 = vless 端口可连；复用 xt-datapath 的事件驱动就绪判定，
    // 测试里不需要再写一遍「等端口」，更不需要 sleep。
    let mut server = xt_datapath::start(&DatapathSpec {
        xray_bin: xray.clone(),
        config_path: server_config_path,
        socks_addr: server_addr,
        // 服务端由测试自己驱动，没有额外必需端口。
        required_addrs: Vec::new(),
        log_level: LogLevel::Info,
        tun_fd: None,
    })
    .await
    .expect("拉起真 xray 服务端");
    server.wait_ready().await.expect("真 xray 服务端就绪");
    eprintln!("[e2e] vless 服务端就绪于 {server_addr}");

    // ---- 3. daemon（含本地订阅文件里的两个节点）----------------------------
    let socks_port = free_port_pair();
    let socks_addr = SocketAddr::from(([127, 0, 0, 1], socks_port));
    std::fs::write(
        dir.join("settings.json"),
        format!(r#"{{"socks_listen":"127.0.0.1:{socks_port}","log_level":"info"}}"#),
    )
    .expect("写 settings.json");
    let subscription = format!(
        "vless://{UUID_1}@127.0.0.1:{server_port}?encryption=none&type=tcp&security=none#n1\n\
         vless://{UUID_2}@127.0.0.1:{server_port}?encryption=none&type=tcp&security=none#n2\n"
    );
    let subscription_path = dir.join("subscription.txt");
    std::fs::write(&subscription_path, subscription).expect("写订阅原文");

    let socket_path = dir.join("daemon.sock");
    let daemon = Daemon::bootstrap(DaemonConfig {
        socket_path: socket_path.clone(),
        state_dir: dir.clone(),
        xray_bin: xray.clone(),
        log_level: LogLevel::Info,
        subscription_file: subscription_path,
        // 探测靶点指向本机源站：不依赖外网，但经节点的路径仍然是真的。
        probe_url: format!("http://{origin_addr}/generate_204"),
    })
    .await
    .expect("daemon 装配");

    // bind() 返回即 socket 已监听 —— 一个可观测事件，不需要轮询等待文件出现。
    let bound = daemon.bind().await.expect("绑定 AF_UNIX socket");
    assert_eq!(bound.socket_path(), socket_path.as_path());
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serve = tokio::spawn(async move { bound.serve(shutdown_rx).await });

    // ---- 4. 真 IPC 客户端 ---------------------------------------------------
    let client = xt_ipc::Client::connect(&socket_path, "backend-3-e2e")
        .await
        .expect("IPC 连接 daemon");
    let hello = client.hello();
    eprintln!(
        "[e2e] daemon {} pid={} 能力={:?}",
        hello.daemon_version, hello.pid, hello.capabilities
    );
    // 能力表是唯一事实来源。2026-10-01（T2）起 TunMode 也宣告（用户裁决本版打开 TUN）。
    // `SubscriptionFetch`（远端拉取）与 `NodeImport`（手工导入）现在也是**代码事实**，
    // 必须宣告：界面靠能力表决定渲染「添加订阅 / 导入」入口，不宣告就等于把入口藏掉。
    // ⚠️ 宣告 ≠ 已验证：TUN 真机接管（helper 安装、utun、路由/DNS）仍只有 S6 能证；
    // 订阅拉取的端到端证据在本文件 `remote_subscription_*` 两个测试里（对着本地 HTTP 服务，不需要外网）。
    for expected in [
        Capability::ProxyMode,
        Capability::TunMode,
        Capability::Stats,
        Capability::Probe,
        Capability::Subscriptions,
        Capability::NodeImport,
        Capability::SubscriptionFetch,
    ] {
        assert!(hello.capabilities.contains(&expected), "缺少能力 {expected:?}");
    }

    let mut events = client.events();
    let nodes = match client.request(Request::ListNodes).await.expect("ListNodes") {
        Response::Nodes { nodes } => nodes,
        other => panic!("ListNodes 期望 Nodes，收到 {other:?}"),
    };
    assert_eq!(nodes.len(), 2, "本地订阅应解析出 2 个节点：{nodes:?}");
    let node1 = nodes.iter().find(|n| n.name == "n1").expect("n1").id.clone();
    let node2 = nodes.iter().find(|n| n.name == "n2").expect("n2").id.clone();
    eprintln!("[e2e] 节点 {node1} / {node2}");

    // ---- 4.5 手工导入节点（`ImportNodes`）------------------------------------
    //
    // 为什么这条必须在 CI 里（不需要真机、不需要网络）：它是用户把节点弄进来的两条入口
    // 之一（另一条是远端拉取 `SubscriptionFetch`，见本文件末尾那两个测试），且**完全不联网**。
    // 只证明「界面上有个输入框」不够 ——
    // 这里证明「用户粘贴的分享链接真的进了目录，且来源标成 Manual」。
    let payload = "vless://11111111-2222-3333-4444-555555555555@imported.example.com:443?encryption=none&security=tls#导入节点";
    let response = client
        .request(Request::ImportNodes { payload: payload.to_string() })
        .await
        .expect("ImportNodes 应答");
    assert!(matches!(response, Response::Ok), "导入必须成功，收到 {response:?}");
    let imported = match client.request(Request::ListNodes).await.expect("ListNodes") {
        Response::Nodes { nodes } => nodes,
        other => panic!("ListNodes 期望 Nodes，收到 {other:?}"),
    };
    assert!(
        imported
            .iter()
            .any(|node| node.name == "导入节点" && node.source == NodeSource::Manual),
        "导入的节点必须出现在目录里、来源为 Manual：{imported:?}"
    );
    assert_eq!(imported.len(), 3, "本地订阅 2 个 + 手工导入 1 个：{imported:?}");
    eprintln!("[e2e] 手工导入后节点数：{}", imported.len());

    // ---- 5. connect → 等 Connected（事件驱动）------------------------------
    let response = client
        .request(Request::Connect { node_id: node1.clone(), mode: RunMode::Proxy })
        .await
        .expect("Connect 应答");
    assert!(matches!(response, Response::Accepted), "Connect 必须先回 Accepted：{response:?}");
    let connected = wait_for_stage(&mut events, Stage::Connected, CONNECT_DEADLINE).await;
    let pid1 = connected.datapath.pid.expect("Connected 必须带真实 pid");
    assert!(connected.datapath.ready_at_ms.is_some(), "Connected 必须带真实就绪时刻");
    let version = connected.datapath.version.clone().unwrap_or_default();
    assert!(version.contains("Xray"), "版本必须来自 xray version 真实输出：{version:?}");
    eprintln!("[e2e] Connected pid={pid1} version={version}");

    // ---- 6. 经 daemon 的 socks 入站做真 SOCKS5 请求 -------------------------
    let body = socks5_get(socks_addr, "127.0.0.1", origin_addr.port(), "/generate_204").await;
    assert_eq!(body.len(), PAYLOAD_LEN, "必须拿到源站的全部字节");
    assert_eq!(body, expected_payload(), "字节内容必须与源站一致");
    eprintln!("[e2e] SOCKS5 请求拿到 {} 字节", body.len());

    // ---- 7. Status：真 StatsService 采样 ------------------------------------
    let status = match client.request(Request::Status).await.expect("Status") {
        Response::Status(view) => view,
        other => panic!("Status 期望 Status，收到 {other:?}"),
    };
    let stats = status.stats.expect("Status 必须带真实采样（不是 null）");
    assert!(
        stats.downlink_bytes >= PAYLOAD_LEN as u64,
        "downlink 必须 >= 实际传输的 {PAYLOAD_LEN} 字节：{stats:?}"
    );
    assert!(stats.uplink_bytes > 0, "uplink 必须 > 0（真实请求字节）：{stats:?}");
    assert!(stats.sampled_at_ms > 1_700_000_000_000, "采样时刻必须是真实时钟");
    eprintln!(
        "[e2e] stats uplink={} downlink={} sampled_at_ms={}",
        stats.uplink_bytes, stats.downlink_bytes, stats.sampled_at_ms
    );

    // ---- 8. SwitchNode → 新进程 → 再请求 ------------------------------------
    let response = client
        .request(Request::SwitchNode { node_id: node2.clone() })
        .await
        .expect("SwitchNode 应答");
    assert!(matches!(response, Response::Accepted), "SwitchNode 必须先回 Accepted：{response:?}");
    let switched = wait_for_state(
        &mut events,
        "切换后的 Connected（新 pid）",
        move |view| view.stage == Stage::Connected && view.datapath.pid != Some(pid1),
        CONNECT_DEADLINE,
    )
    .await;
    let pid2 = switched.datapath.pid.expect("切换后必须有新 pid");
    assert_ne!(pid1, pid2, "切节点必须换一个真实进程（旧 pid 还在）");
    assert!(!process_alive(pid1), "切节点后旧核心进程必须真的退出");
    let body2 = socks5_get(socks_addr, "127.0.0.1", origin_addr.port(), "/generate_204").await;
    assert_eq!(body2, expected_payload(), "切节点后必须仍能拿到全部字节");
    eprintln!("[e2e] 切节点后 pid={pid2}，再次拿到 {} 字节", body2.len());

    // 第二个节点的真实字节数：再次采样必须为正。
    let status = match client.request(Request::Status).await.expect("Status(2)") {
        Response::Status(view) => view,
        other => panic!("Status 期望 Status，收到 {other:?}"),
    };
    let stats2 = status.stats.expect("切换后仍必须能采样");
    assert!(stats2.downlink_bytes >= PAYLOAD_LEN as u64, "{stats2:?}");
    eprintln!("[e2e] 切换后 stats downlink={}", stats2.downlink_bytes);

    // ---- 9. 真日志：TailLogs 必须含核心自己输出的行 --------------------------
    let logs = match client.request(Request::TailLogs { lines: 200 }).await.expect("TailLogs") {
        Response::Logs { logs } => logs,
        other => panic!("TailLogs 期望 Logs，收到 {other:?}"),
    };
    assert!(
        logs.iter().any(|line| line.target.starts_with("xray.")),
        "TailLogs 必须包含核心真实输出（当前 {} 行）",
        logs.len()
    );
    eprintln!("[e2e] TailLogs {} 行，含核心输出", logs.len());

    // ---- 10. Disconnect → 等 Disconnected → 进程真的不存在 ------------------
    let response = client.request(Request::Disconnect).await.expect("Disconnect 应答");
    assert!(matches!(response, Response::Accepted));
    let disconnected = wait_for_stage(&mut events, Stage::Disconnected, STOP_DEADLINE).await;
    assert!(
        disconnected.last_error.is_none(),
        "正常断开必须清空 last_error：{:?}",
        disconnected.last_error
    );
    assert!(!process_alive(pid2), "断开后核心进程必须真的不存在（pid={pid2}）");
    eprintln!("[e2e] Disconnected，pid={pid2} 已消失");

    // ---- 11. ProbeNodes：临时实例 + 每节点独立 socks 端口，真实 TTFB ---------
    let response = client
        .request(Request::ProbeNodes { node_ids: vec![node1.clone(), node2.clone()] })
        .await
        .expect("ProbeNodes 应答");
    assert!(matches!(response, Response::Accepted));
    let results = wait_for_probes(&mut events, 2, PROBE_DEADLINE).await;
    for result in &results {
        assert!(result.is_consistent(), "探测结果必须恰好是 measured 或 failed：{result:?}");
        assert!(result.ttfb_ms.is_some(), "经真节点的 TTFB 必须测到：{result:?}");
    }
    for result in &results {
        eprintln!("[e2e] TTFB {} = {}ms", result.node_id, result.ttfb_ms.unwrap_or(0));
    }

    // ---- 12. 竞态回归：连续 3 轮 connect/disconnect，**每轮都要有真实采样** ----
    //
    // 这条钉住的是 ux 独立发现的缺陷：api 入站与 socks 入站不是同一个就绪事件。
    // 如果就绪判定只等 socks，某些轮次会一次性 stats 连接 refused → 整段会话
    // 「未采样」。多轮断言让这种偶发竞态变成必现失败。
    let mut previous_pid = pid2;
    for round in 1..=3u32 {
        let node = if round % 2 == 1 { node1.clone() } else { node2.clone() };
        let response = client
            .request(Request::Connect { node_id: node, mode: RunMode::Proxy })
            .await
            .expect("轮次 Connect 应答");
        assert!(matches!(response, Response::Accepted));
        let prev = previous_pid;
        let connected = wait_for_state(
            &mut events,
            "轮次 Connected（新 pid）",
            move |view| view.stage == Stage::Connected && view.datapath.pid != Some(prev),
            CONNECT_DEADLINE,
        )
        .await;
        let pid = connected.datapath.pid.expect("轮次 Connected 必须带 pid");

        let body = socks5_get(socks_addr, "127.0.0.1", origin_addr.port(), "/generate_204").await;
        assert_eq!(body.len(), PAYLOAD_LEN, "第 {round} 轮必须拿到全部字节");

        let status = match client.request(Request::Status).await.expect("轮次 Status") {
            Response::Status(view) => view,
            other => panic!("Status 期望 Status，收到 {other:?}"),
        };
        let stats = match status.stats {
            Some(stats) => stats,
            None => panic!("第 {round} 轮必须采到真实 stats（api 入站就绪竞态回归）"),
        };
        assert!(
            stats.downlink_bytes >= PAYLOAD_LEN as u64,
            "第 {round} 轮 downlink 必须 >= {PAYLOAD_LEN}：{stats:?}"
        );
        assert!(stats.sampled_at_ms > 1_700_000_000_000);
        eprintln!(
            "[e2e] 第 {round} 轮 pid={pid} uplink={} downlink={}",
            stats.uplink_bytes, stats.downlink_bytes
        );

        let response = client.request(Request::Disconnect).await.expect("轮次 Disconnect 应答");
        assert!(matches!(response, Response::Accepted));
        wait_for_stage(&mut events, Stage::Disconnected, STOP_DEADLINE).await;
        assert!(!process_alive(pid), "第 {round} 轮断开后 pid={pid} 必须消失");
        previous_pid = pid;
    }

    // ---- 12.5 T2：Tun 意图必须被受理（不再被「只开放 proxy」早退拒绝）---------
    //
    // 用户 2026-10-01 裁决（T2）：本版打开 TUN —— 能力表宣告 TunMode，且
    // `begin_connect_intent` 不再用 `mode != Proxy` 把 tun 意图拒掉。
    //
    // 这里只断言**受理**（Response::Accepted）：终态取决于运行环境 ——
    // 没有特权 helper 的机器上它会以真实错误（helper 连不上）收尾，
    // 所以不断言「连上」。收尾尽力而为（此时可能已经自己失败回到 Disconnected）。
    let response = client
        .request(Request::Connect { node_id: node1.clone(), mode: RunMode::Tun })
        .await
        .expect("Connect(tun) 应答");
    assert!(
        matches!(response, Response::Accepted),
        "Tun 意图必须被受理（T2），收到 {response:?}"
    );
    eprintln!("[e2e] Connect(tun) 已受理（终态由环境决定：无 helper 时以真实错误收尾）");
    let _ = client.request(Request::Disconnect).await;

    // ---- 13. 收尾 -----------------------------------------------------------
    shutdown_tx.send(()).expect("发送关闭信号");
    tokio::time::timeout(STOP_DEADLINE, serve)
        .await
        .expect("daemon 必须在关闭信号后返回")
        .expect("serve 任务 join")
        .expect("serve 正常退出");
    server.stop().await.expect("停掉真 xray 服务端");
    origin_handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// 远端订阅的完整往返：`AddSubscription` → `ListSubscriptions` → `RefreshSubscription`。
///
/// 每条断言都对应一个用户可见的事实：
///   1. 添加之后列表里真的多了一条，节点数与成功时刻来自**真实拉取**；
///   2. 刷新**真的再拉了一次**（HTTP 命中计数 1 → 2），不是把旧节点又写一遍；
///   3. 本地文件订阅（id = `"local"`）不许被「刷新」管 —— 它每次启动由文件重读，
///      根本没有远程拉取这个动作。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_subscription_add_list_and_refresh() {
    // 本地文件订阅真实存在（1 个节点）：这样第 3 条断言不是「记录不存在」的假通过。
    let local = format!(
        "vless://{UUID_1}@127.0.0.1:443?encryption=none&security=none#local-node\n"
    );
    let harness = ControlHarness::start("subs", Some(&local)).await;

    // 起始事实：只有本地文件那一条，且它的 id 就是 "local"。
    let before = list_subscriptions(&harness.client).await;
    assert_eq!(before.len(), 1, "启动时只应有本地文件订阅：{before:?}");
    assert_eq!(before[0].id.as_str(), "local", "{before:?}");

    // ---- 本地 HTTP 源站返回一段**真订阅正文**（两个 vless 节点）------------
    let body = format!(
        "vless://{UUID_1}@127.0.0.1:443?encryption=none&security=none#remote-1\n\
         vless://{UUID_2}@127.0.0.1:8443?encryption=none&security=none#remote-2\n"
    );
    let (origin, hits) = start_subscription_server(body);
    let url = format!("http://{origin}/sub");

    // ---- 1. AddSubscription -------------------------------------------------
    let response = harness
        .client
        .request(Request::AddSubscription { url: url.clone() })
        .await
        .expect("AddSubscription 应答");
    assert!(matches!(response, Response::Ok), "新增订阅必须成功，收到 {response:?}");
    // 计数器没动，就说明正文不是这次网络拉回来的（缓存 / 旧文件都不算数）。
    assert_eq!(hits.load(Ordering::SeqCst), 1, "新增订阅必须真的发过一次 HTTP 请求");

    let views = list_subscriptions(&harness.client).await;
    assert_eq!(views.len(), 2, "本地 + 新订阅：{views:?}");
    let remote = views
        .iter()
        .find(|view| view.id.as_str() == "sub-1")
        .unwrap_or_else(|| panic!("新订阅必须是 sub-1（local 不参与编号）：{views:?}"));
    assert_eq!(remote.url, url, "列表必须回显用户填的 URL");
    assert_eq!(remote.node_count, 2, "源站正文里有两个 vless 链接：{remote:?}");
    let fetched_at_ms = remote.fetched_at_ms.expect("成功拉取必须带真实时刻，不能是 None");
    assert!(
        fetched_at_ms > 1_700_000_000_000,
        "fetched_at_ms 必须来自真实时钟（不是 0、不是写死值）：{fetched_at_ms}"
    );
    assert!(remote.last_error.is_none(), "成功拉取不许留错误：{remote:?}");

    // 拉下来的节点必须真的进目录，并且来源标成这条订阅（界面据此分组）。
    let nodes = match harness.client.request(Request::ListNodes).await.expect("ListNodes") {
        Response::Nodes { nodes } => nodes,
        other => panic!("ListNodes 期望 Nodes，收到 {other:?}"),
    };
    let source = NodeSource::Subscription { id: SubscriptionId::new("sub-1") };
    for name in ["remote-1", "remote-2"] {
        assert!(
            nodes.iter().any(|node| node.name == name && node.source == source),
            "远端节点 {name} 必须进目录且来源是 sub-1：{nodes:?}"
        );
    }
    assert!(
        nodes.iter().any(|node| node.name == "local-node"),
        "本地文件订阅的节点不许被远端拉取顶掉：{nodes:?}"
    );
    assert_eq!(nodes.len(), 3, "本地 1 + 远端 2：{nodes:?}");

    // ---- 2. RefreshSubscription：再次成功、且真的又拉了一次 ---------------
    let response = harness
        .client
        .request(Request::RefreshSubscription { id: SubscriptionId::new("sub-1") })
        .await
        .expect("RefreshSubscription 应答");
    assert!(matches!(response, Response::Ok), "刷新必须成功，收到 {response:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "刷新必须重新发一次 HTTP 请求（计数不涨 = 刷新只读了内存）"
    );
    let views = list_subscriptions(&harness.client).await;
    let remote = views
        .iter()
        .find(|view| view.id.as_str() == "sub-1")
        .expect("刷新后 sub-1 仍必须在列表里");
    assert_eq!(remote.node_count, 2, "刷新后节点数仍应是 2：{remote:?}");
    assert!(remote.fetched_at_ms.is_some(), "刷新成功必须更新 fetched_at_ms：{remote:?}");
    assert!(remote.last_error.is_none(), "刷新成功不许留错误：{remote:?}");

    // ---- 3. local 不许刷新：它是文件驱动的，没有「重新拉取」这个动作 --------
    let error = harness
        .client
        .request(Request::RefreshSubscription { id: SubscriptionId::new("local") })
        .await
        .expect_err("本地文件订阅刷新必须被拒");
    assert_eq!(error.code, ErrorCode::InvalidRequest, "{error:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "被拒的刷新不许碰网络（计数器不该变）");

    harness.shutdown().await;
}

/// 拉不到正文时：必须如实失败（不许假装成功），而且错误消息里**不许出现 URL 的
/// path/query** —— 订阅 URL 通常带账号令牌。口径是 `subs::redact_url`：只允许
/// `scheme://host/…`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_subscription_failure_never_leaks_the_url() {
    let harness = ControlHarness::start("subs-fail", None).await;
    assert!(
        list_subscriptions(&harness.client).await.is_empty(),
        "既没有本地文件、也没加过订阅时应为空"
    );

    // 127.0.0.1:1 上没有任何监听者，也不需要外网：curl 必然连不上。
    // 令牌故意放在 path/query 里 —— 它绝不允许出现在错误消息里。
    let url = "http://127.0.0.1:1/sub?token=SECRET-TOKEN-1234";
    let error = harness
        .client
        .request(Request::AddSubscription { url: url.to_string() })
        .await
        .expect_err("拉不到正文的 URL 必须失败");
    assert_eq!(error.code, ErrorCode::Io, "拉取失败必须落到 Io：{error:?}");

    // 口径：错误里只允许 `scheme://host/…`。注意 curl 的连接失败 stderr 通常只带
    // host:port、本来就没有 path，所以这条当下可能「轻松」通过；它的价值是**回归守卫** ——
    // 哪天一更换拉取实现（或 curl 改了措辞）把完整 URL 写进错误，它会立刻变红。
    assert!(!error.message.contains(url), "错误消息不许出现完整 URL：{error:?}");
    assert!(
        !error.message.contains("SECRET-TOKEN-1234"),
        "错误消息不许出现 query 里的令牌：{error:?}"
    );
    assert!(!error.message.contains("/sub"), "错误消息不许出现 path：{error:?}");

    // 失败不许留下半条订阅：否则界面会出现一条永远拉不到的僵尸条目。
    assert!(
        list_subscriptions(&harness.client).await.is_empty(),
        "拉取失败不许落盘任何订阅记录"
    );

    harness.shutdown().await;
}

/// 上次运行留下过 `local` 记录、这次 `--subscription-file` 指向的文件已经不在了：
/// **列表里不许有它，目录里也不许留着它的节点**。
///
/// 这条专门盯一个真 bug：启动时如果先把「已保存的节点」灌进目录、再读订阅文件，
/// 那么文件缺失（或解析失败）的分支不会清目录 —— 界面于是变成「订阅列表里没有这条，
/// 节点列表里却一直是它的节点」，而且每次重启都复现。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_local_subscription_is_pruned_when_the_file_is_gone() {
    // 逐字写一份 daemon 自己的磁盘格式（`subs::SubscriptionRecord` + `StoredNode`）。
    let saved = r#"[
      {
        "id": "local",
        "url": "/tmp/这条路径已经不存在/subscription.txt",
        "fetched_at_ms": 1700000000000,
        "nodes": [
          {
            "id": "stale-node",
            "name": "stale-node",
            "protocol": "vless",
            "endpoint": "127.0.0.1:443",
            "outbound": { "protocol": "vless" }
          }
        ],
        "last_error": null
      }
    ]"#;
    let harness =
        ControlHarness::start_with_saved_subscriptions("subs-stale", None, Some(saved)).await;

    assert!(
        list_subscriptions(&harness.client).await.is_empty(),
        "订阅文件已经不在了，这条 local 记录必须被摘掉"
    );
    let nodes = match harness.client.request(Request::ListNodes).await.expect("ListNodes") {
        Response::Nodes { nodes } => nodes,
        other => panic!("ListNodes 期望 Nodes，收到 {other:?}"),
    };
    assert!(
        nodes.is_empty(),
        "目录里不许留下「没有订阅记录」的节点（否则界面自相矛盾）：{nodes:?}"
    );

    harness.shutdown().await;
}
