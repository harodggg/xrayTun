//! 真 xray 端到端：真 HTTP 源站 + 真 xray 服务端 + daemon + 真 SOCKS5 请求。
//!
//! 被测路径上**没有任何 mock**：源站是真 socket、服务端是真 xray 进程、代理跳是
//! 真的 vless 环回、daemon 走真 AF_UNIX 帧、字节数取自真 StatsService。
//!
//! 无等待：所有等待都是「事件 + timeout 上限」。整个文件没有 sleep，
//! 也没有「等一会儿再看看」的轮询。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use xt_contract::model::{Capability, ConnectionView, LogLevel, ProbeResult, RunMode, Stage};
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
    // 能力表是唯一事实来源：本轮必须宣告 proxy/stats/probe/subscriptions，
    // 且不能宣告 tun / 远端拉取（未实现的能力不许出现在界面上）。
    for expected in [
        Capability::ProxyMode,
        Capability::Stats,
        Capability::Probe,
        Capability::Subscriptions,
    ] {
        assert!(hello.capabilities.contains(&expected), "缺少能力 {expected:?}");
    }
    assert!(!hello.capabilities.contains(&Capability::TunMode));
    assert!(!hello.capabilities.contains(&Capability::SubscriptionFetch));

    let mut events = client.events();
    let nodes = match client.request(Request::ListNodes).await.expect("ListNodes") {
        Response::Nodes { nodes } => nodes,
        other => panic!("ListNodes 期望 Nodes，收到 {other:?}"),
    };
    assert_eq!(nodes.len(), 2, "本地订阅应解析出 2 个节点：{nodes:?}");
    let node1 = nodes.iter().find(|n| n.name == "n1").expect("n1").id.clone();
    let node2 = nodes.iter().find(|n| n.name == "n2").expect("n2").id.clone();
    eprintln!("[e2e] 节点 {node1} / {node2}");

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
