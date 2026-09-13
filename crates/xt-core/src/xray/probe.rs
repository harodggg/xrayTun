//! 节点延迟探针。
//!
//! ## 为什么不能直接 ping
//!
//! ICMP 延迟和「通过代理访问一个网站要多久」几乎无关。真实可用的指标是
//! **TTFB（首字节时间）**：从发出请求到收到第一个响应字节，包含了
//! 握手、加密协商、代理转发、目标站响应。
//!
//! ## 怎么让流量走指定节点
//!
//! Xray 的 SOCKS 入站无法「按请求选择 outbound」。所以这里为**每个节点开一个
//! 独立的 SOCKS 入站端口**（`base_port + index`），并配一条
//! `inboundTag → 该节点 outbound` 的路由规则。探针只做两件事：
//! 连上对应端口、发一个 HTTP GET。测速期间每个节点完全隔离，互不干扰。
//!
//! 这也是 v2rayN / Clash 系客户端的通行做法。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use url::Url;

use crate::error::{Error, Result};
use crate::model::Node;
use crate::xray::config::{node_to_outbound, API_PORT};
use crate::xray::process::{wait_for_port, CoreEvent, XrayProcess};

/// 默认探测目标：Cloudflare 的 `generate_204`，全球可达、响应体为空、
/// 不参与任何 CDN 地域调度，是延迟测量的标准靶点。
pub const DEFAULT_PROBE_URL: &str = "http://cp.cloudflare.com/generate_204";

#[derive(Debug, Clone)]
pub struct ProbeOptions {
    pub binary: PathBuf,
    /// 探针 SOCKS 端口的起始值。
    pub base_port: u16,
    pub target_url: String,
    /// 单个节点的整体超时（含连接 + 首字节）。
    pub timeout: Duration,
    /// 并发度。太高会让本地 CPU 成为瓶颈，反而让延迟失真。
    pub concurrency: usize,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::new(),
            base_port: 21000,
            target_url: DEFAULT_PROBE_URL.to_string(),
            timeout: Duration::from_secs(5),
            concurrency: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub node_id: String,
    pub node_name: String,
    /// TTFB，单位毫秒。失败时为 `None`。
    pub latency_ms: Option<u32>,
    pub http_status: Option<u16>,
    pub error: Option<String>,
    pub tested_at: u64,
}

impl ProbeResult {
    fn failure(node: &Node, err: impl Into<String>) -> Self {
        Self {
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            latency_ms: None,
            http_status: None,
            error: Some(err.into()),
            tested_at: now_unix(),
        }
    }

    pub fn ok(&self) -> bool {
        self.latency_ms.is_some()
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 生成探针专用的 Xray 配置。**与主配置完全隔离**，避免污染用户正在用的实例。
pub fn build_probe_config(nodes: &[Node], base_port: u16) -> Result<Value> {
    let last = base_port as u32 + nodes.len() as u32;
    if last > 65000 {
        return Err(Error::Probe(format!(
            "探针端口区间 {base_port}..{last} 越界，请调小 base_port 或分批探测"
        )));
    }

    let mut inbounds = vec![json!({
        "tag": "api",
        "listen": "127.0.0.1",
        "port": API_PORT,
        "protocol": "dokodemo-door",
        "settings": { "address": "127.0.0.1" }
    })];
    let mut outbounds = vec![
        // 同 config.rs：`domainStrategy` 在 26.x 已迁到 sockopt。
        json!({
            "tag": "direct",
            "protocol": "freedom",
            "settings": {},
            "streamSettings": { "sockopt": { "domainStrategy": "UseIP" } }
        }),
        json!({ "tag": "block", "protocol": "blackhole" }),
        json!({ "tag": "api", "protocol": "freedom", "settings": {} }),
    ];
    let mut rules = vec![json!({
        "type": "field",
        "inboundTag": ["api"],
        "outboundTag": "api",
        "ruleTag": "internal-api"
    })];

    for (i, node) in nodes.iter().enumerate() {
        let inbound_tag = format!("probe-{i}");
        inbounds.push(json!({
            "tag": inbound_tag,
            "listen": "127.0.0.1",
            "port": base_port + i as u16,
            "protocol": "socks",
            // 探针只需要 TCP，关掉 UDP 可以少开一组端口。
            "settings": { "auth": "noauth", "udp": false, "userLevel": 0 }
        }));
        outbounds.push(node_to_outbound(node));
        rules.push(json!({
            "type": "field",
            "inboundTag": [inbound_tag],
            "outboundTag": node.outbound_tag(),
            "ruleTag": format!("probe-route-{i}")
        }));
    }

    // 兜底：不应被命中（每条探针入站都有专属规则），保险起见给 direct。
    rules.push(json!({
        "type": "field",
        "network": "tcp,udp",
        "outboundTag": "direct",
        "ruleTag": "probe-fallback"
    }));

    Ok(json!({
        "log": { "loglevel": "error", "access": "", "error": "" },
        "api": { "tag": "api", "services": ["StatsService"] },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": { "domainStrategy": "IPIfNonMatch", "rules": rules }
    }))
}

/// 批量探测。返回结果顺序与 `nodes` 一致。
pub async fn probe_nodes(
    nodes: &[Node],
    opts: &ProbeOptions,
    events: Option<tokio::sync::mpsc::UnboundedSender<CoreEvent>>,
) -> Result<Vec<ProbeResult>> {
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let target = Url::parse(&opts.target_url)
        .map_err(|e| Error::Probe(format!("探测 URL 非法: {e}")))?;

    // ---- 落盘探针配置 ----
    let config = build_probe_config(nodes, opts.base_port)?;
    let config_path = std::env::temp_dir().join(format!(
        "xraytun-probe-{}-{}.json",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;

    // ---- 启动探针核心 ----
    let proc = XrayProcess::spawn(&opts.binary, &config_path, events).await?;

    // 等所有探针端口就绪。并发等待，整体超时按单节点超时上浮一点。
    let ready_deadline = opts.timeout + Duration::from_secs(2);
    let mut set = tokio::task::JoinSet::new();
    for i in 0..nodes.len() {
        let port = opts.base_port + i as u16;
        set.spawn(async move { (i, wait_for_port(port, ready_deadline).await.is_ok()) });
    }
    let mut not_ready: Vec<usize> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_i, true)) => {}
            Ok((i, false)) => not_ready.push(i),
            Err(_) => {} // 任务 panic：交给后续探测阶段的连接失败去暴露
        }
    }

    // ---- 并发探测 ----
    let sem = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let mut handles = Vec::with_capacity(nodes.len());
    for (i, node) in nodes.iter().enumerate() {
        let port = opts.base_port + i as u16;
        let node = node.clone();
        let target = target.clone();
        let timeout = opts.timeout;
        let sem = sem.clone();
        let pre_failed = not_ready.contains(&i);

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("信号量不会关闭");
            if pre_failed {
                return ProbeResult::failure(&node, "探针端口未在超时内就绪（可能是核心启动失败）");
            }
            match probe_one(port, &target, timeout).await {
                Ok((latency, status)) => ProbeResult {
                    node_id: node.id.clone(),
                    node_name: node.name.clone(),
                    latency_ms: Some(latency),
                    http_status: Some(status),
                    error: None,
                    tested_at: now_unix(),
                },
                Err(e) => ProbeResult::failure(&node, e.to_string()),
            }
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(join_err) => {
                results.push(ProbeResult {
                    node_id: String::new(),
                    node_name: String::new(),
                    latency_ms: None,
                    http_status: None,
                    error: Some(format!("探测任务 panic: {join_err}")),
                    tested_at: now_unix(),
                });
            }
        }
    }

    // ---- 收尾：一定要杀掉核心并删掉临时配置 ----
    let _ = proc.shutdown(Duration::from_secs(2)).await;
    let _ = std::fs::remove_file(&config_path);

    // 按输入顺序返回，方便 UI 直接按行应用结果。
    let order: std::collections::HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    results.sort_by_key(|r| order.get(r.node_id.as_str()).copied().unwrap_or(usize::MAX));
    Ok(results)
}

/// 探测单个节点：TCP 连接 + TTFB。
async fn probe_one(port: u16, target: &Url, timeout: Duration) -> Result<(u32, u16)> {
    let host = target.host_str().ok_or_else(|| Error::Probe("探测 URL 缺少 host".into()))?;
    let target_port = target.port_or_known_default().unwrap_or(80);
    let path = if target.path().is_empty() { "/" } else { target.path() };

    let mut stream = socks5_connect(port, host, target_port, timeout).await?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: XrayTun/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );

    let start = Instant::now();
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| Error::Probe(format!("发送请求失败: {e}")))?;

    let mut buf = [0u8; 128];
    let n = tokio::time::timeout(timeout, stream.read(&mut buf))
        .await
        .map_err(|_| Error::Probe(format!("等待首字节超时（>{}s）", timeout.as_secs())))?
        .map_err(|e| Error::Probe(format!("读取响应失败: {e}")))?;

    if n == 0 {
        return Err(Error::Probe("对端在返回任何数据前关闭了连接".into()));
    }
    let latency = start.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let status = parse_http_status(&buf[..n]).unwrap_or(0);
    Ok((latency, status))
}

fn parse_http_status(buf: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(buf);
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse().ok()
}

/// 最小 SOCKS5 客户端（仅无认证 + CONNECT）。
///
/// 刻意不引第三方 SOCKS 库：这里只需要 30 行协议，而依赖越少，
/// 供应链风险和版本冲突越少。
async fn socks5_connect(
    proxy_port: u16,
    host: &str,
    target_port: u16,
    timeout: Duration,
) -> Result<TcpStream> {
    let mut stream = tokio::time::timeout(
        timeout,
        TcpStream::connect(("127.0.0.1", proxy_port)),
    )
    .await
    .map_err(|_| Error::Probe("连接本地探针端口超时".into()))?
    .map_err(|e| Error::Probe(format!("连接本地探针端口 {proxy_port} 失败: {e}")))?;
    let _ = stream.set_nodelay(true);

    // 1) 方法协商：只声明「无认证」。
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 0x05 {
        return Err(Error::Probe(format!("对端不是 SOCKS5（版本字节 {}）", greeting[0])));
    }
    if greeting[1] != 0x00 {
        return Err(Error::Probe(format!("代理要求认证方式 {}，而探针只支持无认证", greeting[1])));
    }

    // 2) CONNECT 请求。
    let mut req = vec![0x05, 0x01, 0x00];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        req.push(0x01);
        req.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        req.push(0x04);
        req.extend_from_slice(&ip.octets());
    } else {
        if host.len() > 255 {
            return Err(Error::Probe("域名过长".into()));
        }
        req.push(0x03);
        req.push(host.len() as u8);
        req.extend_from_slice(host.as_bytes());
    }
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req).await?;

    // 3) 应答。
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(Error::Probe(format!("SOCKS5 CONNECT 被拒绝（reply={}）", head[1])));
    }
    // 丢弃 BND.ADDR / BND.PORT —— 长度由 ATYP 决定。
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 4 + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x04 => {
            let mut skip = [0u8; 16 + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut skip = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await?;
        }
        other => return Err(Error::Probe(format!("SOCKS5 返回未知地址类型 {other}"))),
    }

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeSource, Protocol, Transport};

    fn node(name: &str, port: u16) -> Node {
        let mut n = Node {
            id: String::new(),
            name: name.into(),
            address: format!("{}.example.com", name),
            port,
            protocol: Protocol::Trojan { password: "pw".into() },
            transport: Transport::Tcp,
            tls: Default::default(),
            mux: None,
            source: NodeSource::Manual,
            tags: vec![],
            raw_uri: None,
        };
        n.refresh_id();
        n
    }

    #[test]
    fn probe_config_gives_each_node_its_own_inbound_and_rule() {
        let nodes = vec![node("a", 443), node("b", 8443)];
        let cfg = build_probe_config(&nodes, 21000).unwrap();
        let inbounds = cfg["inbounds"].as_array().unwrap();
        // api + 2 个探针入站
        assert_eq!(inbounds.len(), 3);
        assert_eq!(inbounds[1]["port"], 21000);
        assert_eq!(inbounds[2]["port"], 21001);
        assert_eq!(inbounds[1]["settings"]["udp"], false);

        let rules = cfg["routing"]["rules"].as_array().unwrap();
        let probe_rule = rules.iter().find(|r| r["ruleTag"] == "probe-route-1").unwrap();
        assert_eq!(probe_rule["inboundTag"][0], "probe-1");
        assert_eq!(probe_rule["outboundTag"], nodes[1].outbound_tag());
    }

    #[test]
    fn probe_config_rejects_port_overflow() {
        let nodes: Vec<Node> = (0..60_000).map(|_| node("x", 443)).collect();
        assert!(build_probe_config(&nodes, 21000).is_err());
    }

    #[test]
    fn http_status_parsing() {
        assert_eq!(parse_http_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_http_status(b"HTTP/1.0 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status(b"garbage"), None);
    }

    #[test]
    fn probe_result_ok_flag() {
        let n = node("a", 443);
        assert!(!ProbeResult::failure(&n, "boom").ok());
    }

    /// 用一个真实的本地 SOCKS5 假服务端验证握手逻辑，不依赖 Xray。
    #[tokio::test]
    async fn socks5_handshake_against_fake_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            sock.read_exact(&mut greet).await.unwrap();
            sock.write_all(&[0x05, 0x00]).await.unwrap();

            let mut head = [0u8; 5];
            sock.read_exact(&mut head).await.unwrap();
            let alen = head[4] as usize;
            let mut rest = vec![0u8; alen + 2];
            sock.read_exact(&mut rest).await.unwrap();
            // 成功应答，BND 用 0.0.0.0:0
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
            sock.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
        });

        let mut s = socks5_connect(port, "example.com", 80, Duration::from_secs(2)).await.unwrap();
        let mut buf = [0u8; 32];
        let n = s.read(&mut buf).await.unwrap();
        assert_eq!(parse_http_status(&buf[..n]), Some(204));
    }

    #[tokio::test]
    async fn socks5_rejects_when_server_demands_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            let _ = sock.read_exact(&mut greet).await;
            let _ = sock.write_all(&[0x05, 0x02]).await; // 要求用户名密码
        });
        let err = socks5_connect(port, "example.com", 80, Duration::from_secs(2)).await;
        assert!(err.is_err());
    }
}
