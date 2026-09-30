//! xt-probe —— 真实 TTFB（Time To First Byte）。
//!
//! # 为什么不 ping
//!
//! ICMP 延迟和「通过代理取一个网页要多久」几乎无关。真实可用的指标是
//! **TTFB**：从发出 HTTP 请求到收到第一个响应字节，中间包含了握手、加密协商、
//! 代理转发和目标站响应 —— 也就是用户真正会感觉到的那段时间。
//!
//! # 怎么让流量走指定节点
//!
//! SOCKS 入站没法「按请求选出口」。所以探测用一份**独立的临时核心配置**
//! （由 `xt_xrayconf::generate_probe` 生成）：每个节点一个独立的 socks 入站端口，
//! 每个入站配一条到该节点出口的路由规则。本 crate 只做两件事：
//! 拉起这个临时实例、连对应端口发一个 HTTP GET。
//!
//! 隔离带来的直接好处：探测**不会碰用户正在用的那个实例**，也不会改它的配置。
//!
//! # 无等待
//!
//! 就绪判定与 `xt-datapath` 同构：核心每输出一行就试一次连接，进程退出（或两条
//! 输出流 EOF）就立刻把全部节点报成失败并带上真实原因。`deadline` 只是失败上限。
//! 节点**串行**探测（不并发轮询）：并发探测会互相抢同一个核心的 CPU 与上行，
//! 让每个数字都变差，而这和节点好坏无关。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{NodeId, ProbeResult};

/// 默认探测目标：Cloudflare 的 `generate_204`，全球可达、响应体为空。
///
/// 它只用来判「这个节点能不能用」。daemon 可以把它覆盖成环回地址，
/// 这样 E2E 与离线环境下探测**不依赖外网**，量到的仍然是真实的经节点路径。
pub const DEFAULT_PROBE_URL: &str = "http://cp.cloudflare.com/generate_204";

/// 单个节点的整体预算（连接 + 握手 + 请求 + 首字节）。它是失败上限，不是周期。
pub const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// 临时核心就绪的失败上限。
const READY_DEADLINE: Duration = Duration::from_secs(10);

/// 失败 detail 里保留的最近输出行数。
const RECENT_LINES: usize = 20;

/// 一个待探测的节点：它的 id 与「只属于它」的本地 socks 端口。
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    pub node_id: NodeId,
    pub socks_addr: SocketAddr,
}

/// 一次批量探测所需的全部输入。
#[derive(Debug, Clone)]
pub struct ProbePlan {
    pub xray_bin: PathBuf,
    /// `xt_xrayconf::generate_probe` 生成的配置正文。
    /// 探测 crate 不做配置生成（那是 xt-xrayconf 的职责），只负责把它跑起来。
    pub config_json: String,
    /// 落盘路径。给一个真实路径是为了出错时人能打开看核心到底读到了什么。
    pub config_path: PathBuf,
    pub targets: Vec<ProbeTarget>,
}

/// 真实 TTFB 的测量者。
#[derive(Debug)]
pub struct Prober {
    host: String,
    port: u16,
    path: String,
    /// 原始 URL，用于错误消息（用户看到的应该是自己填的那个串）。
    url: String,
    deadline: Duration,
}

impl Prober {
    /// 解析探测 URL。只支持 `http://`：本构建没有 TLS 依赖，
    /// 碰到 `https://` 就**如实拒绝**，而不是悄悄降级成明文。
    pub fn new(url: &str) -> Result<Self, ErrorBody> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| bad_request("探测 URL 只支持 http://（本构建没有 TLS 依赖）"))?;
        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(bad_request("探测 URL 缺少 host"));
        }
        // IPv6 字面量形如 [::1]:8080；最后一个冒号只在方括号之外才是端口分隔符。
        let (host, port) = if let Some(end) = authority.rfind(']') {
            let host = &authority[..=end];
            let port = match authority[end + 1..].strip_prefix(':') {
                Some(p) => p
                    .parse::<u16>()
                    .map_err(|_| bad_request("探测 URL 的端口不是合法数字"))?,
                None => 80,
            };
            (host, port)
        } else if let Some((h, p)) = authority.rsplit_once(':') {
            (
                h,
                p.parse::<u16>()
                    .map_err(|_| bad_request("探测 URL 的端口不是合法数字"))?,
            )
        } else {
            (authority, 80)
        };
        if host.is_empty() {
            return Err(bad_request("探测 URL 缺少 host"));
        }
        // 域名交给被探测的核心去解析（SOCKS5 支持传域名），这不影响 TTFB 的真实性。
        Ok(Self {
            host: host.to_string(),
            port,
            path: path.to_string(),
            url: url.to_string(),
            deadline: PROBE_DEADLINE,
        })
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn target_url(&self) -> &str {
        &self.url
    }

    /// 起一个临时核心实例，逐节点真实探测。返回顺序与 `plan.targets` 一致，
    /// **一条节点一条结果**：成功是 `measured`，失败是 `failed`（都带真实时间）。
    pub async fn run(&self, plan: &ProbePlan) -> Result<Vec<ProbeResult>, ErrorBody> {
        if plan.targets.is_empty() {
            return Err(bad_request("没有任何待探测的节点"));
        }
        if !plan.xray_bin.is_file() {
            return Err(ErrorBody::new(
                ErrorCode::DatapathUnavailable,
                format!("xray 二进制不存在或不是文件：{}", plan.xray_bin.display()),
            ));
        }
        if let Some(dir) = plan.config_path.parent() {
            if !dir.as_os_str().is_empty() {
                tokio::fs::create_dir_all(dir).await.map_err(|e| {
                    ErrorBody::new(ErrorCode::Io, format!("创建探测配置目录失败: {e}"))
                })?;
            }
        }
        tokio::fs::write(&plan.config_path, plan.config_json.as_bytes())
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("写入探测配置失败: {e}")))?;

        let mut child = spawn_core(&plan.xray_bin, &plan.config_path)?;
        // 两条输出流各一个读取任务，把「核心说了话」变成事件；两个任务都结束后
        // 接收端返回 None = 它不会再说话了。
        let (tick_tx, mut ticks) = mpsc::unbounded_channel::<()>();
        // 最近输出行：失败时它是唯一能说明「核心为什么没起来」的证据。
        let recent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_line_reader(out, tick_tx.clone(), recent.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_line_reader(err, tick_tx.clone(), recent.clone()));
        }
        drop(tick_tx);

        // ---- 事件驱动就绪：等第一个节点的 socks 端口可连 ----
        let first = plan.targets[0].socks_addr;
        let readiness = if socks_is_connectable(first).await {
            Ok(())
        } else {
            let deadline_at = tokio::time::Instant::now() + READY_DEADLINE;
            loop {
                tokio::select! {
                    biased;
                    status = child.wait() => {
                        break Err(core_error(
                            "临时探测核心在就绪前退出",
                            status.ok().and_then(|s| s.code()),
                            // 进程退出与读取任务收行是两件事：先把剩余输出等出来，
                            // 否则「为什么起不来」的证据永远是空的。
                            &drain_recent(&mut ticks, &recent).await,
                        ));
                    }
                    event = ticks.recv() => {
                        if socks_is_connectable(first).await {
                            break Ok(());
                        }
                        if event.is_none() {
                            let code = child.wait().await.ok().and_then(|s| s.code());
                            break Err(core_error("临时探测核心退出（输出流已结束）", code, &recent_snapshot(&recent)));
                        }
                    }
                    _ = tokio::time::sleep_until(deadline_at) => {
                        break Err(core_error("临时探测核心在期限内没有就绪", None, &recent_snapshot(&recent)));
                    }
                }
            }
        };

        // recent 里还没有内容时（读取任务来不及跑）也允许为空：它不是主证据，
        // 主证据是端口能不能连。
        if let Err(error) = readiness {
            // 收尾：临时实例一定要消失，否则每次探测都留一个进程。
            let _ = child.start_kill();
            let _ = child.wait().await;
            for reader in readers {
                reader.abort();
            }
            return Ok(plan
                .targets
                .iter()
                .map(|t| ProbeResult::failed(t.node_id.clone(), error.clone(), now_ms()))
                .collect());
        }

        // ---- 串行真实探测（不并发：并发会让每个数字都变差）----
        let mut results = Vec::with_capacity(plan.targets.len());
        for target in &plan.targets {
            let result = self.probe_one(target).await;
            results.push(result);
        }

        // ---- 收尾 ----
        // 用的是 SIGKILL：这个实例是纯暂时的、没有任何需要 flush 的状态，
        // 而 SIGTERM 需要一个 libc 依赖才发得出去。
        let _ = child.start_kill();
        let _ = child.wait().await;
        for reader in readers {
            reader.abort();
        }
        let _ = tokio::fs::remove_file(&plan.config_path).await;
        Ok(results)
    }

    /// 探测单个节点：一条节点一条结果，绝不把失败伪装成「0ms」。
    pub async fn probe_one(&self, target: &ProbeTarget) -> ProbeResult {
        match self.measure(target.socks_addr).await {
            Ok(ttfb_ms) => ProbeResult::measured(target.node_id.clone(), ttfb_ms, now_ms()),
            Err(error) => ProbeResult::failed(target.node_id.clone(), error, now_ms()),
        }
    }

    /// 经 `socks_addr` 建一条 SOCKS5 连接并 GET，返回真实首字节耗时（毫秒）。
    pub async fn measure(&self, socks_addr: SocketAddr) -> Result<u32, ErrorBody> {
        let deadline = Instant::now() + self.deadline;
        let mut stream = socks5_connect(socks_addr, &self.host, self.port, deadline).await?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: xraytun-probe/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            env!("CARGO_PKG_VERSION")
        );
        // TTFB 的起点就是「请求发出去」的那一刻：把本地写入也算进去，
        // 才对应「用户点一下要等多久」。
        let started = Instant::now();
        write_before(deadline, &mut stream, request.as_bytes(), "发送探测请求").await?;

        let mut buf = [0u8; 64];
        let n = read_before(deadline, &mut stream, &mut buf, "等待首字节").await?;
        if n == 0 {
            return Err(ErrorBody::new(
                ErrorCode::Io,
                "对端在返回任何数据前关闭了连接（拿不到首字节）",
            ));
        }
        Ok(started.elapsed().as_millis().min(u32::MAX as u128) as u32)
    }
}

/// 最小 SOCKS5 客户端（无认证 + CONNECT）。刻意不引第三方 socks 库：
/// 这里只需要几十行协议，而依赖越少，供应链风险与版本冲突越少。
async fn socks5_connect(
    proxy: SocketAddr,
    host: &str,
    port: u16,
    deadline: Instant,
) -> Result<tokio::net::TcpStream, ErrorBody> {
    let mut stream = match tokio::time::timeout_at(deadline.into(), tokio::net::TcpStream::connect(proxy)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(ErrorBody::new(ErrorCode::Io, format!("连接本地探测端口 {proxy} 失败: {e}")))
        }
        Err(_) => {
            return Err(ErrorBody::new(
                ErrorCode::Io,
                format!("连接本地探测端口 {proxy} 超时"),
            ))
        }
    };
    let _ = stream.set_nodelay(true);

    // 1) 方法协商：只声明「无认证」。
    write_before(deadline, &mut stream, &[0x05, 0x01, 0x00], "SOCKS5 协商").await?;
    let mut greeting = [0u8; 2];
    read_exact_before(deadline, &mut stream, &mut greeting, "SOCKS5 协商响应").await?;
    if greeting[0] != 0x05 {
        return Err(ErrorBody::new(
            ErrorCode::Io,
            format!("对端不是 SOCKS5（版本字节 {}）", greeting[0]),
        ));
    }
    if greeting[1] != 0x00 {
        return Err(ErrorBody::new(
            ErrorCode::Io,
            format!("代理要求认证方式 {}，而探测只支持无认证", greeting[1]),
        ));
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
        // 域名交给核心解析：它有自己的 DNS 策略，我们提前解析反而会改变路径。
        let trimmed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        if trimmed.len() > 255 {
            return Err(bad_request("探测目标域名过长"));
        }
        req.push(0x03);
        req.push(trimmed.len() as u8);
        req.extend_from_slice(trimmed.as_bytes());
    }
    req.extend_from_slice(&port.to_be_bytes());
    write_before(deadline, &mut stream, &req, "SOCKS5 CONNECT 请求").await?;

    // 3) 应答：丢掉 BND.ADDR/BND.PORT，长度由 ATYP 决定。
    let mut head = [0u8; 4];
    read_exact_before(deadline, &mut stream, &mut head, "SOCKS5 应答").await?;
    if head[1] != 0x00 {
        return Err(ErrorBody::new(
            ErrorCode::Io,
            format!("SOCKS5 CONNECT 被拒绝（reply={}）", head[1]),
        ));
    }
    match head[3] {
        0x01 => skip_before(deadline, &mut stream, 4 + 2, "SOCKS5 应答").await?,
        0x04 => skip_before(deadline, &mut stream, 16 + 2, "SOCKS5 应答").await?,
        0x03 => {
            let mut len = [0u8; 1];
            read_exact_before(deadline, &mut stream, &mut len, "SOCKS5 应答").await?;
            skip_before(deadline, &mut stream, len[0] as usize + 2, "SOCKS5 应答").await?;
        }
        other => {
            return Err(ErrorBody::new(
                ErrorCode::Io,
                format!("SOCKS5 返回未知地址类型 {other}"),
            ))
        }
    }
    Ok(stream)
}

/// 每个 await 都吃同一个 deadline：`timeout` 是整条探测的预算，
/// 不是「每个阶段各续一份」—— 后者会让最坏耗时变成预算的数倍。
async fn write_before(
    deadline: Instant,
    stream: &mut tokio::net::TcpStream,
    bytes: &[u8],
    what: &'static str,
) -> Result<(), ErrorBody> {
    match tokio::time::timeout_at(deadline.into(), stream.write_all(bytes)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}失败: {e}"))),
        Err(_) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}超时（预算耗尽）"))),
    }
}

async fn read_before(
    deadline: Instant,
    stream: &mut tokio::net::TcpStream,
    buf: &mut [u8],
    what: &'static str,
) -> Result<usize, ErrorBody> {
    match tokio::time::timeout_at(deadline.into(), stream.read(buf)).await {
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}失败: {e}"))),
        Err(_) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}超时（预算耗尽）"))),
    }
}

async fn read_exact_before(
    deadline: Instant,
    stream: &mut tokio::net::TcpStream,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), ErrorBody> {
    match tokio::time::timeout_at(deadline.into(), stream.read_exact(buf)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}失败: {e}"))),
        Err(_) => Err(ErrorBody::new(ErrorCode::Io, format!("{what}超时（预算耗尽）"))),
    }
}

async fn skip_before(
    deadline: Instant,
    stream: &mut tokio::net::TcpStream,
    len: usize,
    what: &'static str,
) -> Result<(), ErrorBody> {
    let mut buf = vec![0u8; len];
    read_exact_before(deadline, stream, &mut buf, what).await
}

fn spawn_core(xray_bin: &Path, config_path: &Path) -> Result<Child, ErrorBody> {
    let mut cmd = Command::new(xray_bin);
    cmd.arg("run")
        .arg("-c")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 临时实例绝不允许变成孤儿。
        .kill_on_drop(true);
    if let Some(dir) = config_path.parent() {
        if !dir.as_os_str().is_empty() {
            cmd.current_dir(dir);
        }
    }
    cmd.spawn().map_err(|e| {
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("无法执行 {}: {e}", xray_bin.display()),
        )
    })
}

/// 读一行就 tick 一次，并把最近若干行留在内存里供失败 detail 使用。
fn spawn_line_reader<R>(
    reader: R,
    ticks: mpsc::UnboundedSender<()>,
    recent: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut recent) = recent.lock() {
                if recent.len() == RECENT_LINES {
                    recent.remove(0);
                }
                recent.push(line);
            }
            if ticks.send(()).is_err() {
                break;
            }
        }
    })
}

fn recent_snapshot(recent: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    match recent.lock() {
        Ok(lines) => lines.clone(),
        // 锁中毒只可能是读取任务 panic；取空不影响对外事实，但绝不 panic
        //（release 下 panic = abort）。
        Err(_) => Vec::new(),
    }
}

/// 进程退出后等读取任务把剩余输出搬完（上限 500ms），再取快照。
///
/// 进程死亡与「读取任务收行」是两个独立事件：不等一下，错误 detail 里的
/// recent_output 就会是空的 —— 那正好把最有用的排查证据丢掉了。
async fn drain_recent(
    ticks: &mut mpsc::UnboundedReceiver<()>,
    recent: &Arc<Mutex<Vec<String>>>,
) -> Vec<String> {
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        while ticks.recv().await.is_some() {}
    })
    .await;
    recent_snapshot(recent)
}

async fn socks_is_connectable(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(addr)
        )
        .await,
        Ok(Ok(_))
    )
}

fn core_error(what: &str, code: Option<i32>, recent: &[String]) -> ErrorBody {
    let code_text = code.map(|c| c.to_string()).unwrap_or_else(|| "未知".to_string());
    ErrorBody::new(ErrorCode::CoreExitedEarly, format!("{what}（退出码 {code_text}）")).with_detail(
        serde_json::json!({
            "exit_code": code,
            "recent_output": recent,
        }),
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_urls() {
        let p = Prober::new("http://cp.cloudflare.com/generate_204").unwrap();
        assert_eq!(p.host, "cp.cloudflare.com");
        assert_eq!(p.port, 80);
        assert_eq!(p.path, "/generate_204");

        let p = Prober::new("http://127.0.0.1:18080/").unwrap();
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 18080);
        assert_eq!(p.path, "/");

        let p = Prober::new("http://[::1]:9090/x").unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, 9090);
        assert_eq!(p.path, "/x");

        // 没有路径
        let p = Prober::new("http://example.com").unwrap();
        assert_eq!(p.path, "/");
        assert_eq!(p.port, 80);
    }

    /// https 必须**如实拒绝**：本构建没有 TLS 依赖，悄悄降级成明文就是假话。
    #[test]
    fn rejects_https_instead_of_downgrading() {
        let err = Prober::new("https://example.com/").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(err.message.contains("http://"));
    }

    /// 用一个真实的最小 SOCKS5 假服务端验证握手 + TTFB 计时（不依赖 Xray）。
    #[tokio::test]
    async fn measures_ttfb_through_a_socks5_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
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
            sock.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
            // 读到请求后回一个字节：那就是「首字节」。
            let mut buf = [0u8; 256];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"H").await.unwrap();
        });

        let prober = Prober::new("http://example.com/generate_204").unwrap();
        let ms = prober.measure(addr).await.unwrap();
        assert!(ms < 3000, "回环 TTFB 不该到秒级: {ms}");
    }

    /// 代理拒绝 CONNECT ⇒ 失败原因必须来自代理自己，而不是一句「超时」。
    #[tokio::test]
    async fn reports_socks_rejection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
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
            // reply = 0x02 (connection not allowed)
            sock.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.unwrap();
        });
        let prober = Prober::new("http://example.com/").unwrap();
        let err = prober.measure(addr).await.unwrap_err();
        assert!(err.message.contains("被拒绝"), "实际：{}", err.message);
    }

    /// 没有任何东西监听 ⇒ 如实失败，而不是返回一个 0。
    #[tokio::test]
    async fn measure_fails_when_nothing_listens() {
        let prober = Prober::new("http://example.com/")
            .unwrap()
            .with_deadline(Duration::from_millis(300));
        let err = prober.measure("127.0.0.1:1".parse().unwrap()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
    }

    /// `ProbeResult` 的一致性：恰好一个字段是 Some。
    #[tokio::test]
    async fn probe_one_produces_consistent_results() {
        let prober = Prober::new("http://example.com/")
            .unwrap()
            .with_deadline(Duration::from_millis(300));
        let target = ProbeTarget {
            node_id: NodeId::new("n1"),
            socks_addr: "127.0.0.1:1".parse().unwrap(),
        };
        let result = prober.probe_one(&target).await;
        assert!(result.is_consistent(), "必须恰好是 measured 或 failed");
        assert!(result.ttfb_ms.is_none());
        assert!(result.error.is_some());
        assert_eq!(result.node_id, NodeId::new("n1"));
    }
}
