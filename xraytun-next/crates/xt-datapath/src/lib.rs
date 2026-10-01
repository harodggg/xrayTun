//! xt-datapath —— 真 xray 子进程的生命周期：拉起 / 事件驱动就绪 / 停止。
//!
//! # 就绪为什么必须是事件驱动（而不是「等 N 毫秒再探」）
//!
//! 「端口可连」是唯一与版本无关的就绪信号（日志格式会随版本变），但**何时去试**
//! 决定了两个性质：最坏等待时间与失败原因的准确性。
//!
//! * 定时轮询（每 50ms 一次）有两个坏处：平均多等半个周期；进程已经死了却要
//!   等到下一个周期甚至 deadline 才发现，而且报出来的是「未就绪」而不是
//!   「它自己退出了」—— 用户会去查错方向。
//! * 事件驱动：核心每输出一行就试一次连接。它一开口（尤其是那句
//!   「started」）我们立刻就可能就绪；stdout/stderr 双双 EOF 就是**它再也不会
//!   说话**的确定性事件，此刻立刻报 [`ErrorCode::CoreExitedEarly`] 并带上真实
//!   退出码与最后几行输出。
//!
//! 这里的 `select!` 只有一个 `deadline`（失败上限），没有轮询周期。
//!
//! # 停止为什么是 SIGTERM → deadline → SIGKILL
//!
//! xray 收到 SIGTERM 会关监听、断开连接，比 SIGKILL 干净；但内核偶尔会卡在
//! 关闭长连接上，所以必须有一个明确的强杀上限。`deadline` 在这里的语义只是
//! **失败上限**：到点没退就升级成 SIGKILL 并等 reap —— 它永远不是「先等 X 毫秒
//! 再检查」。

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc};
use xt_contract::error::{ErrorBody, ErrorCode};
use xt_contract::model::{LogLevel, LogLine};

/// 就绪等待的默认失败上限。超时 = [`ErrorCode::DatapathUnavailable`]，
/// 与「进程提前退出」是两件不同的事，错误码也不同。
pub const READY_DEADLINE: Duration = Duration::from_secs(15);

/// SIGTERM 之后等待 reap 的上限；超时即升级为 SIGKILL。
pub const STOP_DEADLINE: Duration = Duration::from_secs(5);

/// `xray version` 的失败上限。真 xray 立刻返回；但如果二进制把 `version` 当成
/// 长驻模式（或被换成了一个不认参数的脚本），没有这个上限就会让 `start()` 永久挂住。
const VERSION_DEADLINE: Duration = Duration::from_secs(10);

/// `xray run -test` 预检的失败上限。同上：它必须是一个会结束的进程。
const CONFIG_TEST_DEADLINE: Duration = Duration::from_secs(20);

/// 保存在内存里的最近输出行数（`CoreExitedEarly` 的 detail 用它）。
const RECENT_LINES: usize = 20;

/// 单次「试连」的上限。连本地端口拒绝/成功都在微秒级，这只是防御性上界。
const CONNECT_ATTEMPT_DEADLINE: Duration = Duration::from_millis(250);

const LOG_CHANNEL_CAPACITY: usize = 512;

/// 拉起一个数据面实例所需的全部真实输入。
#[derive(Debug, Clone)]
pub struct DatapathSpec {
    pub xray_bin: PathBuf,
    pub config_path: PathBuf,
    /// proxy 模式的就绪判据：这个 SOCKS 端口可连。
    pub socks_addr: SocketAddr,
    /// 除 SOCKS 之外**还必须接受连接**的地址（proxy 模式就是 StatsService 的 api 入站）。
    ///
    /// 为什么它们必须一起进就绪判定：xray 的各个入站不是同一个就绪事件，
    /// api 可能比 socks 晚几十毫秒才 accept。只等 socks 就宣布「已连接」，会让
    /// 随后一次性建立的统计连接撞上 `Connection refused`，整段会话显示「未采样」——
    /// 那是「明明能知道却一直说不知道」，同样违反 I3。
    pub required_addrs: Vec<SocketAddr>,
    /// 用户选择的日志档位。它不传给 xray（xray 的 loglevel 在配置文件里，
    /// 由 xt-xrayconf 写入），而是决定我们把核心输出转发到 tracing 的粒度。
    pub log_level: LogLevel,
    /// TUN 模式的 utun fd：`Some` 时经 `XRAY_TUN_FD` 传给 xray（xray 据此跳过
    /// 自己的地址/路由配置，配置责任在 helper）。proxy 模式为 `None`。
    pub tun_fd: Option<std::os::fd::RawFd>,
}

/// 核心已被证明可连的事实。三个字段都来自真实观测：
/// pid 来自 spawn、version 来自 `xray version`、ready_at_ms 来自真实时钟。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyInfo {
    pub pid: u32,
    pub version: String,
    pub ready_at_ms: u64,
}

/// 一个活着的（或刚死掉的）数据面进程。
pub struct RunningDatapath {
    child: Child,
    pid: u32,
    version: String,
    ready_at_ms: Option<u64>,
    /// 已观测到的退出码；`Some` 表示进程已经退出并被 wait 收尸。
    exited: Option<Option<i32>>,
    socks_addr: SocketAddr,
    /// 就绪判定的全部地址（socks 在前，去重后）。ReadyInfo 返回前它们都已被证明可连。
    ready_addrs: Vec<SocketAddr>,
    log_tx: broadcast::Sender<LogLine>,
    recent: Arc<Mutex<VecDeque<String>>>,
    /// 每读到一行输出就有一个 tick；两个读取任务都结束后 recv() 返回 None。
    ticks: mpsc::UnboundedReceiver<()>,
    /// 是否已经把输出读干过。避免错误路径重复等。
    drained: bool,
    readers: Vec<tokio::task::JoinHandle<()>>,
}

/// 拉起来并**立刻返回**（不等就绪）；就绪判定交给 [`RunningDatapath::wait_ready`]。
pub async fn start(spec: &DatapathSpec) -> Result<RunningDatapath, ErrorBody> {
    if !spec.xray_bin.is_file() {
        return Err(ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("xray 二进制不存在或不是文件：{}", spec.xray_bin.display()),
        ));
    }
    if !spec.config_path.is_file() {
        return Err(ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("配置文件不存在：{}", spec.config_path.display()),
        ));
    }

    // 版本是真实观测，不是常量：先问一次二进制自己。
    let version = read_version(&spec.xray_bin).await?;

    // 就绪判定的地址集合：socks 优先，其余去重后并入。顺序只影响「先试哪个」。
    let mut ready_addrs = vec![spec.socks_addr];
    for addr in &spec.required_addrs {
        if !ready_addrs.contains(addr) {
            ready_addrs.push(*addr);
        }
    }

    let mut cmd = Command::new(&spec.xray_bin);
    cmd.arg("run")
        .arg("-c")
        .arg(&spec.config_path);
    // TUN 模式：把 helper 交来的 utun fd 经 XRAY_TUN_FD 传给 xray（xray 收到后
    // 跳过自己的地址/路由配置，配置责任在 helper）。proxy 模式 tun_fd 为 None。
    if let Some(fd) = spec.tun_fd {
        cmd.env("XRAY_TUN_FD", fd.to_string());
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 即便上层忘了 stop，daemon 崩溃时也不会留下孤儿进程。
        .kill_on_drop(true);
    // 让核心的工作目录落在配置文件旁边，这样它写出的相对路径资源
    // （geoip.dat / geosite.dat）能被找到。
    if let Some(dir) = spec.config_path.parent() {
        if !dir.as_os_str().is_empty() {
            cmd.current_dir(dir);
        }
    }

    let mut child = cmd.spawn().map_err(|e| {
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("无法执行 {}: {e}", spec.xray_bin.display()),
        )
    })?;
    let pid = child.id().ok_or_else(|| {
        // spawn 成功却拿不到 pid 说明我们对内核的假设坏了 —— 这是我们的 bug，
        // 不能悄悄用一个假 pid 继续。
        ErrorBody::new(ErrorCode::Internal, "spawn 成功但读不到 pid")
    })?;

    let (log_tx, _) = broadcast::channel(LOG_CHANNEL_CAPACITY);
    let (tick_tx, ticks) = mpsc::unbounded_channel();
    let recent = Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_LINES)));
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(spawn_reader(
            out,
            "stdout",
            log_tx.clone(),
            tick_tx.clone(),
            recent.clone(),
            spec.log_level,
        ));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(spawn_reader(
            err,
            "stderr",
            log_tx.clone(),
            tick_tx,
            recent.clone(),
            spec.log_level,
        ));
    }

    Ok(RunningDatapath {
        child,
        pid,
        version,
        ready_at_ms: None,
        exited: None,
        socks_addr: spec.socks_addr,
        ready_addrs,
        log_tx,
        recent,
        ticks,
        drained: false,
        readers,
    })
}

/// 用 `xray run -test -c <config>` 让核心自己判配置是否合法。
///
/// 这必须在 spawn 之前做：把一份明显非法的配置送进一个正在运行的核心，
/// 只会得到「启动后又退出」，而真实原因是配置 —— 提前判就能把它报成
/// [`ErrorCode::ConfigInvalid`]，用户拿到的是可行动的信息。
pub async fn validate_config(xray_bin: &Path, config_path: &Path) -> Result<(), ErrorBody> {
    if !xray_bin.is_file() {
        return Err(ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("xray 二进制不存在或不是文件：{}", xray_bin.display()),
        ));
    }
    let output = tokio::time::timeout(CONFIG_TEST_DEADLINE, Command::new(xray_bin)
        .arg("run")
        .arg("-test")
        .arg("-c")
        .arg(config_path)
        .stdin(Stdio::null())
        .output())
    .await
    .map_err(|_| {
        // 预检本身挂住 = 我们无法判断配置合法性。这不是「配置非法」，
        // 所以不能报 ConfigInvalid（那会把用户指向错误的排查方向）。
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("{} run -test 在 {CONFIG_TEST_DEADLINE:?} 内没有返回", xray_bin.display()),
        )
    })?
    .map_err(|e| {
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("执行 {} 失败: {e}", xray_bin.display()),
        )
    })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let reason = if !stderr.is_empty() { stderr } else { stdout };
    Err(ErrorBody::new(ErrorCode::ConfigInvalid, format!("xray 判定配置非法：{reason}"))
        .with_detail(serde_json::json!({
            "exit_code": output.status.code(),
            "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            "config_path": config_path.display().to_string(),
        })))
}

impl RunningDatapath {
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    /// 来自 `xray version` 的真实输出（第一行）。
    pub fn version(&self) -> Option<String> {
        Some(self.version.clone())
    }

    /// 核心被证明可连的时刻（epoch ms）。未就绪时为 `None` —— 不是 0。
    pub fn ready_at_ms(&self) -> Option<u64> {
        self.ready_at_ms
    }

    /// 订阅核心日志。每个订阅者独立一份，慢订阅者不会拖慢就绪判定。
    pub fn logs(&self) -> broadcast::Receiver<LogLine> {
        self.log_tx.subscribe()
    }

    /// 等到核心**开始监听全部必需端口**：事件驱动，不轮询、不 sleep。
    ///
    /// 「就绪」= socks 与 `required_addrs` 里的每一个都接受过连接。每读到核心一行
    /// 输出就只对**尚未就绪**的地址再试一次（已就绪的不会重复试），所以等待时间
    /// 由最慢的那个入站决定，而不是由某个固定周期决定。
    pub async fn wait_ready(&mut self) -> Result<ReadyInfo, ErrorBody> {
        self.wait_ready_within(READY_DEADLINE).await
    }

    /// 与 [`Self::wait_ready`] 相同，但由调用方给失败上限（例如 E2E 用更短的）。
    pub async fn wait_ready_within(&mut self, deadline: Duration) -> Result<ReadyInfo, ErrorBody> {
        if let Some(at) = self.ready_at_ms {
            return Ok(self.ready_info(at));
        }
        if let Some(code) = self.exited {
            return Err(self.exited_early_error(code));
        }
        // 尚未就绪的地址；试通一个就移出一个。
        let mut pending = self.ready_addrs.clone();
        // 先各试一次：核心可能在「spawn 返回」与「我们开始等」之间就开始监听了。
        if probe_all(&mut pending).await {
            return Ok(self.mark_ready(now_ms()));
        }

        let outcome = {
            let child = &mut self.child;
            let ticks = &mut self.ticks;
            let deadline_at = tokio::time::Instant::now() + deadline;
            loop {
                tokio::select! {
                    biased;
                    // 进程退出是确定性事件：立刻报真实退出码，而不是干等到 deadline。
                    status = child.wait() => {
                        break WaitOutcome::Exited(status.ok().and_then(|s| s.code()));
                    }
                    // 核心说了一句话 ⇒ 立刻把还没就绪的地址各试一次（不做任何定时）。
                    event = ticks.recv() => {
                        if probe_all(&mut pending).await {
                            break WaitOutcome::Ready(now_ms());
                        }
                        if event.is_none() {
                            // 两个输出流都到 EOF ⇒ 它不会再监听了。再确认一次退出码。
                            let code = child.wait().await.ok().and_then(|s| s.code());
                            break WaitOutcome::Exited(code);
                        }
                    }
                    // 失败上限：到点还没全就绪 = 它还活着但有入站没监听（与「提前退出」不同）。
                    _ = tokio::time::sleep_until(deadline_at) => break WaitOutcome::NotReady,
                }
            }
        };

        match outcome {
            WaitOutcome::Ready(at) => Ok(self.mark_ready(at)),
            WaitOutcome::Exited(code) => {
                // 进程退出与「读取任务把最后几行搬进 recent」是两件事：
                // 进程可能先死，读取任务还排在调度队列上。错误信息要带上真实
                // 输出，所以先等读取侧收完（上限 500ms，只为不让错误路径挂住）。
                self.drain_output().await;
                self.exited = Some(code);
                Err(self.exited_early_error(code))
            }
            WaitOutcome::NotReady => Err(self.not_ready_error(deadline, &pending)),
        }
    }

    /// 等输出读取任务把剩余字节读完。进程已退出 ⇒ 两条管道必然 EOF。
    async fn drain_output(&mut self) {
        if self.drained {
            return;
        }
        self.drained = true;
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while self.ticks.recv().await.is_some() {}
        })
        .await;
    }

    /// 等到进程退出（自然退出或被信号杀掉），返回真实退出码。
    ///
    /// 与 `wait_ready` 同构：等的是「进程退出」或「两条输出流都结束」这两个事件，
    /// 没有任何定时检查。监督任务用它把「核心自己死了」变成一条真实状态迁移，
    /// 而不是等下一次用户操作才发现界面在说谎。
    pub async fn wait_exit(&mut self) -> Option<i32> {
        if let Some(code) = self.exited {
            return code;
        }
        loop {
            tokio::select! {
                biased;
                status = self.child.wait() => {
                    let code = status.ok().and_then(|s| s.code());
                    self.exited = Some(code);
                    return code;
                }
                event = self.ticks.recv() => {
                    if event.is_none() {
                        // 两个输出流都结束了，进程不会再说话；reap 拿真实退出码。
                        let code = self.child.wait().await.ok().and_then(|s| s.code());
                        self.exited = Some(code);
                        return code;
                    }
                }
            }
        }
    }

    /// 停止并 reap。SIGTERM → deadline → SIGKILL，全程没有 sleep。
    pub async fn stop(mut self) -> Result<(), ErrorBody> {
        if self.exited.is_none() {
            send_sigterm(self.pid);
            match tokio::time::timeout(STOP_DEADLINE, self.child.wait()).await {
                Ok(status) => {
                    self.exited = Some(status.ok().and_then(|s| s.code()));
                }
                Err(_) => {
                    // deadline 只是失败上限：到点还没退说明核心卡住了。
                    // 不升级成 SIGKILL 的话，「断开」在界面上成功、进程却还在跑。
                    tracing::warn!(
                        pid = self.pid,
                        deadline_ms = STOP_DEADLINE.as_millis() as u64,
                        "核心未在期限内响应 SIGTERM，升级为强杀"
                    );
                    let _ = self.child.start_kill();
                    let status = self.child.wait().await;
                    self.exited = Some(status.ok().and_then(|s| s.code()));
                }
            }
        }
        for reader in self.readers.drain(..) {
            reader.abort();
        }
        Ok(())
    }

    fn mark_ready(&mut self, at: u64) -> ReadyInfo {
        self.ready_at_ms = Some(at);
        self.ready_info(at)
    }

    fn ready_info(&self, at: u64) -> ReadyInfo {
        ReadyInfo { pid: self.pid, version: self.version.clone(), ready_at_ms: at }
    }

    fn recent_tail(&self) -> Vec<String> {
        match self.recent.lock() {
            Ok(lines) => lines.iter().cloned().collect(),
            // 锁中毒只可能是某个读取任务 panic；这不影响对外事实，取空即可，
            // 但绝不 panic（release 下 panic = abort，等于整个 daemon 消失）。
            Err(_) => Vec::new(),
        }
    }

    fn exited_early_error(&self, code: Option<i32>) -> ErrorBody {
        let code_text = code.map(|c| c.to_string()).unwrap_or_else(|| "未知".to_string());
        ErrorBody::new(
            ErrorCode::CoreExitedEarly,
            format!(
                "核心在监听 {} 之前退出（退出码 {code_text}）",
                self.socks_addr
            ),
        )
        .with_detail(serde_json::json!({
            "exit_code": code,
            "pid": self.pid,
            "socks_addr": self.socks_addr.to_string(),
            "recent_output": self.recent_tail(),
        }))
    }

    fn not_ready_error(&self, deadline: Duration, pending: &[SocketAddr]) -> ErrorBody {
        let pending_text =
            pending.iter().map(|addr| addr.to_string()).collect::<Vec<_>>().join(", ");
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!(
                "核心在 {deadline:?} 内没有就绪（仍不可连：{pending_text}；进程 pid {} 仍在运行）",
                self.pid
            ),
        )
        .with_detail(serde_json::json!({
            "timeout_ms": deadline.as_millis() as u64,
            "pending_addrs": pending_text,
            "socks_addr": self.socks_addr.to_string(),
            "pid": self.pid,
            "recent_output": self.recent_tail(),
        }))
    }
}

enum WaitOutcome {
    Ready(u64),
    Exited(Option<i32>),
    NotReady,
}

/// SIGTERM 一个子进程。
///
/// 为什么需要 unsafe + libc：tokio 只提供 `start_kill()`（= SIGKILL），std 根本没有
/// 发信号的接口。`kill(2)` 只读一个 pid、不碰内存，是这里唯一安全的做法。
fn send_sigterm(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) 只使用传入的 pid，不读写任何内存。
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// 对尚未就绪的地址各试一次；全部可连返回 `true`，不可连的留在 `pending`。
///
/// 已就绪的地址会被移出，所以同一个端口不会被反复试探 —— 每次尝试都由核心的
/// 一行输出触发，这是「等事件」，不是「定期轮询」。
async fn probe_all(pending: &mut Vec<SocketAddr>) -> bool {
    let mut still = Vec::with_capacity(pending.len());
    for addr in pending.drain(..) {
        if !addr_is_connectable(addr).await {
            still.push(addr);
        }
    }
    *pending = still;
    pending.is_empty()
}

/// 单发连一次本地端口。**这不是轮询**：它由输出行事件或进程退出事件触发。
async fn addr_is_connectable(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(CONNECT_ATTEMPT_DEADLINE, tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

fn spawn_reader<R>(
    reader: R,
    stream: &'static str,
    logs: broadcast::Sender<LogLine>,
    ticks: mpsc::UnboundedSender<()>,
    recent: Arc<Mutex<VecDeque<String>>>,
    level: LogLevel,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(text)) = lines.next_line().await {
            if let Ok(mut recent) = recent.lock() {
                if recent.len() == RECENT_LINES {
                    recent.pop_front();
                }
                recent.push_back(text.clone());
            }
            let line = parse_log_line(&text, stream);
            // 先叫醒等着的人再转发：某个订阅者读得慢不该拖慢就绪判定。
            let _ = ticks.send(());
            forward_to_tracing(&line, level);
            let _ = logs.send(line);
        }
        // 这个流结束 ≠ 进程退出（另一个流可能还在说），所以这里不发 EOF 事件：
        // ticks 的所有发送端都 drop 之后 recv() 才会返回 None，那才是
        // 「它不会再说话了」。
    })
}

/// 把核心输出映射成契约里的 `LogLine`。
///
/// Xray 的行首是 `[Info]` / `[Warning]` / `[Error]` / `[Debug]`。解析不出来时
/// 一律按 Info —— 这是**不假话**的方向：宁可低估严重性，也不把普通一行标成错误
/// （后者会让界面满屏红字，用户很快就不再相信任何红色）。
fn parse_log_line(text: &str, stream: &str) -> LogLine {
    LogLine {
        ts_ms: now_ms(),
        level: detect_level(text),
        target: format!("xray.{stream}"),
        message: text.to_string(),
    }
}

fn detect_level(text: &str) -> LogLevel {
    let lower = text.to_ascii_lowercase();
    if lower.contains("[error]") || lower.contains("failed") || lower.contains("panic") {
        LogLevel::Error
    } else if lower.contains("[warning]") || lower.contains("[warn]") {
        LogLevel::Warn
    } else if lower.contains("[debug]") {
        LogLevel::Debug
    } else {
        LogLevel::Info
    }
}

/// `log_level` 在这里的真实用途：用户选 debug 时每一行都进 tracing，
/// 否则只把 warn/error 转出去 —— 否则一个 24 小时运行的核心会把日志刷爆。
fn forward_to_tracing(line: &LogLine, level: LogLevel) {
    match line.level {
        LogLevel::Error => tracing::error!(target = %line.target, "{}", line.message),
        LogLevel::Warn => tracing::warn!(target = %line.target, "{}", line.message),
        LogLevel::Info if level == LogLevel::Debug => {
            tracing::debug!(target = %line.target, "{}", line.message)
        }
        LogLevel::Debug if level == LogLevel::Debug => {
            tracing::debug!(target = %line.target, "{}", line.message)
        }
        _ => {}
    }
}

/// `xray version` 的第一行。拿不到就是真的拿不到 —— 如实报错，不编一个版本号。
async fn read_version(xray_bin: &Path) -> Result<String, ErrorBody> {
    let probe = Command::new(xray_bin)
        .arg("version")
        .stdin(Stdio::null())
        .output();
    let output = tokio::time::timeout(VERSION_DEADLINE, probe).await.map_err(|_| {
        // `version` 是一个必须会退出的子进程。挂住说明这个二进制不认这个参数，
        // 或者根本不是 xray —— 不能因此让 daemon 启动永久卡住。
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("{} version 在 {VERSION_DEADLINE:?} 内没有返回", xray_bin.display()),
        )
    })?;
    let output = output.map_err(|e| {
        ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("执行 {} version 失败: {e}", xray_bin.display()),
        )
    })?;
    if !output.status.success() {
        return Err(ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("{} version 退出码 {:?}", xray_bin.display(), output.status.code()),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::DatapathUnavailable,
            format!("{} version 没有输出任何内容", xray_bin.display()),
        ));
    }
    Ok(first.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .max(1)
}

#[cfg(test)]
// 这里**刻意**跨 await 持有 std 锁：要的是「同一时刻只有一个用例在写夹具/ fork」，
// 也就是 OS 线程级串行。换成 tokio 的异步锁解决不了 —— 它只在异步任务间让路，
// 而 ETXTBSY 是内核层的 fd 继承问题。理由与背景见 `FIXTURE_LOCK`。
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// **会 spawn 进程的用例必须拿着这把锁跑完。**
    ///
    /// 真实原因（CI 上真红过两次，报 `Text file busy (os error 26)`）：
    /// libtest 每个用例一个线程；线程 A 正在写夹具脚本（fd 处于可写）时，
    /// 线程 B 调 `fork()`，子进程会**继承 A 那个可写的 fd**；
    /// 之后 A 去 exec 这个文件，内核就回 `ETXTBSY`。
    ///
    /// 「写临时文件 → 改名」解决不了这个问题：改名换的是路径，inode 没变，
    /// 那个可写 fd 仍然指向它。唯一稳的做法是让「写夹具」与「任何 fork」
    /// 不重叠 —— 也就是让这些用例串行。夹具极小（毫秒级），串行的代价可以忽略。
    ///
    /// 更彻底的替代方案是给 crate 加一个真的假核心可执行文件（`[[bin]]`），
    /// 运行时不再写任何可执行文件；那需要把用例挪到 `tests/`（`CARGO_BIN_EXE_*`
    /// 只在集成测试里可用），本轮不做，记在这里免得下次重新踩。
    static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serialize_process_spawning() -> std::sync::MutexGuard<'static, ()> {
        // 中毒也继续：测的是进程行为，前一个用例 panic 不影响这把锁保护的资源。
        FIXTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 写一个可执行的假核心脚本。
    ///
    /// 必须处理 `version` 子命令并**退出**：`start()` 会用 `xray version` 做一次
    /// 真实版本探测，若假脚本对任何参数都长驻，那个探测就永远不返回。
    ///
    /// 返回的第二个值是串行锁：**必须绑到变量上**（`let (bin, _serial) = ...`），
    /// 用 `_` 会立刻 drop，锁就白拿了。原因见 [`FIXTURE_LOCK`]。
    fn fake_core(
        dir: &Path,
        name: &str,
        body: &str,
    ) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let serial = serialize_process_spawning();
        let path = dir.join(name);
        let tmp = dir.join(format!("{name}.tmp"));
        {
            let mut file = std::fs::File::create(&tmp).unwrap();
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = \"version\" ]; then echo 'fake-xray {name} 0.0.1'; exit 0; fi\n{body}\n"
            );
            file.write_all(script.as_bytes()).unwrap();
            file.sync_all().unwrap();
        }
        let mut perms = std::fs::metadata(&tmp).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        (path, serial)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xt-datapath-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec(bin: PathBuf, config: PathBuf, socks: SocketAddr) -> DatapathSpec {
        DatapathSpec {
            xray_bin: bin,
            config_path: config,
            socks_addr: socks,
            required_addrs: Vec::new(),
            log_level: LogLevel::Info,
            tun_fd: None,
        }
    }

    /// **就绪 = 全部必需端口都接受连接**，不是「socks 通了就算」。
    ///
    /// 这条钉住的是 ux 独立发现的竞态：xray 的 api 入站与 socks 入站不是同一个
    /// 就绪事件。若只等 socks 就宣布已连接，随后建立的统计连接会撞上 refused，
    /// 整段会话显示「未采样」—— 把已知说成未知。
    #[tokio::test]
    async fn ready_requires_every_required_addr_to_accept() {
        let dir = temp_dir("multi");
        // 一个真实监听器扮演已就绪的 socks。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    break;
                }
            }
        });
        // 第二个必需地址：端口 1 上没有任何服务，因此**还没有**就绪。
        let not_yet: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (bin, _serial) = fake_core(&dir, "fake-xray-multi", "echo 'svc up'\nexec tail -f /dev/null");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let mut dp = start(&DatapathSpec {
            xray_bin: bin,
            config_path: cfg,
            socks_addr: socks,
            required_addrs: vec![not_yet],
            log_level: LogLevel::Info,
            tun_fd: None,
        })
        .await
        .unwrap();

        let err = dp.wait_ready_within(Duration::from_millis(300)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::DatapathUnavailable, "{err:?}");
        let detail = err.detail.expect("必须带 detail");
        assert!(
            detail["pending_addrs"].as_str().unwrap_or("").contains("127.0.0.1:1"),
            "未就绪的地址必须出现在 detail 里：{detail}"
        );
        dp.stop().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正对照：两个必需端口都可连 ⇒ 就绪。
    #[tokio::test]
    async fn ready_when_all_required_addrs_accept() {
        let dir = temp_dir("multi-ok");
        let socks_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks = socks_listener.local_addr().unwrap();
        let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = api_listener.local_addr().unwrap();
        for listener in [socks_listener, api_listener] {
            tokio::spawn(async move {
                loop {
                    if listener.accept().await.is_err() {
                        break;
                    }
                }
            });
        }
        let (bin, _serial) = fake_core(&dir, "fake-xray-multi-ok", "echo 'svc up'\nexec tail -f /dev/null");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let mut dp = start(&DatapathSpec {
            xray_bin: bin,
            config_path: cfg,
            socks_addr: socks,
            required_addrs: vec![api],
            log_level: LogLevel::Info,
            tun_fd: None,
        })
        .await
        .unwrap();
        let info = dp.wait_ready_within(Duration::from_secs(5)).await.unwrap();
        assert!(info.ready_at_ms > 1_700_000_000_000);
        dp.stop().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 就绪路径：核心打印一行、端口已经可连 ⇒ 立刻 Ready。
    /// 判据是「真连一次」，不是「日志里出现了某个词」。
    #[tokio::test]
    async fn ready_when_the_socks_port_accepts_a_connection() {
        let dir = temp_dir("ready");
        // 用一个真实监听器扮演「已就绪的 SOCKS 端口」。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    break;
                }
            }
        });
        let (bin, _serial) = fake_core(&dir, "fake-xray", "echo 'fake core started'\nexec tail -f /dev/null");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let mut dp = start(&spec(bin, cfg, addr)).await.unwrap();
        assert!(dp.pid().is_some());
        assert_eq!(dp.ready_at_ms(), None, "未就绪时必须是 None，不是 0");

        let info = dp.wait_ready_within(Duration::from_secs(5)).await.unwrap();
        assert!(info.ready_at_ms > 1_700_000_000_000, "就绪时刻必须来自真实时钟");
        assert_eq!(dp.ready_at_ms(), Some(info.ready_at_ms));
        // 版本来自假脚本对 `version` 子命令的真实输出（真 xray 下就是 `Xray 26.3.27 ...`）
        assert!(info.version.starts_with("fake-xray"), "实际：{}", info.version);

        dp.stop().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 核心自己死掉 ⇒ 立刻报「它在监听前就退出」并带真实退出码，
    /// **不是**干等到 deadline、也不是报成「未就绪」。
    #[tokio::test]
    async fn early_exit_is_reported_with_the_real_exit_code() {
        let dir = temp_dir("exit");
        let (bin, _serial) = fake_core(&dir, "fake-xray-dies", "echo 'boom: bad config'\nexit 3");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        // 端口 1 上不会有服务。
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let mut dp = start(&spec(bin, cfg, addr)).await.unwrap();
        let started = std::time::Instant::now();
        let err = dp.wait_ready_within(Duration::from_secs(10)).await.unwrap_err();
        let spent = started.elapsed();

        assert_eq!(err.code, ErrorCode::CoreExitedEarly);
        assert!(
            spent < Duration::from_secs(3),
            "必须在进程退出时立刻返回，而不是等到 deadline；实际 {spent:?}"
        );
        let detail = err.detail.expect("必须带结构化 detail");
        assert_eq!(detail["exit_code"], 3, "退出码必须是真的");
        let recent = detail["recent_output"].as_array().unwrap();
        assert!(
            recent.iter().any(|l| l.as_str().unwrap_or("").contains("boom")),
            "必须带上最后几行真实输出"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 假核心不存在 ⇒ `DatapathUnavailable`（一眼看出是本地问题）。
    #[tokio::test]
    async fn start_fails_for_a_missing_binary() {
        // 这条用例自己不写夹具，但它会 fork 一次（exec 不存在的路径）——
        // 只要 fork，就可能继承别的线程"正在写夹具"的 fd，所以同样要串行。
        let _serial = serialize_process_spawning();
        let dir = temp_dir("missing");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let err = start(&spec(
            dir.join("nope"),
            cfg,
            "127.0.0.1:1".parse().unwrap(),
        ))
        .await
        .err()
        .expect("不存在的二进制必须启动失败");
        assert_eq!(err.code, ErrorCode::DatapathUnavailable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 核心活着但始终不监听 ⇒ 到 deadline 报 `DatapathUnavailable`：
    /// 这是与「提前退出」**不同**的事实，错误码也不同。
    #[tokio::test]
    async fn alive_but_not_listening_times_out_as_unavailable() {
        let dir = temp_dir("quiet");
        let (bin, _serial) = fake_core(&dir, "fake-xray-quiet", "exec tail -f /dev/null");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let mut dp = start(&spec(bin, cfg, addr)).await.unwrap();
        let err = dp.wait_ready_within(Duration::from_millis(300)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::DatapathUnavailable, "实际：{err:?}");
        assert!(err.message.contains("没有就绪"));
        dp.stop().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// stop() 真的让进程消失：用 `kill(pid, 0)` 验证（ESRCH = 不存在）。
    #[tokio::test]
    async fn stop_leaves_no_process_behind() {
        let dir = temp_dir("stop");
        let (bin, _serial) = fake_core(&dir, "fake-xray-term", "echo 'up'\nexec tail -f /dev/null");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let dp = start(&spec(bin, cfg, addr)).await.unwrap();
        let pid = dp.pid().unwrap();
        assert!(process_alive(pid), "刚拉起来时它必须活着");

        dp.stop().await.unwrap();
        assert!(!process_alive(pid), "stop() 之后进程必须真的不在了");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 核心忽略 SIGTERM ⇒ stop() 在 deadline 之后升级为 SIGKILL 并 reap。
    /// 这条钉住的是「断开在界面上成功、进程却还在跑」这个谎不被允许。
    #[tokio::test]
    async fn stop_escalates_to_sigkill_when_sigterm_is_ignored() {
        let dir = temp_dir("kill");
        // `signal.pause()` 不用 sleep；`print` 是**就绪事件**：它之后 SIGTERM 才真的被忽略。
        // 没有这个事件，「stop 之前进程是否已经装好处理函数」就成了一场赌博。
        let (bin, _serial) = fake_core(
            &dir,
            "fake-xray-stubborn",
            "echo 'up'\nexec python3 -c \"import signal,sys; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('term-ignored', flush=True); signal.pause()\"",
        );
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let dp = start(&spec(bin, cfg, addr)).await.unwrap();
        let pid = dp.pid().unwrap();

        // 等核心自己说「SIGTERM 已被忽略」——事件驱动，超时只作失败上限。
        let mut logs = dp.logs();
        let ignored = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match logs.recv().await {
                    Ok(line) if line.message.contains("term-ignored") => return true,
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
        })
        .await;
        assert!(matches!(ignored, Ok(true)), "假核心必须先进入忽略 SIGTERM 的状态：{ignored:?}");

        let started = std::time::Instant::now();
        dp.stop().await.unwrap();
        let spent = started.elapsed();
        assert!(!process_alive(pid), "强杀之后进程必须真的不在了");
        assert!(
            spent >= STOP_DEADLINE,
            "它忽略 SIGTERM，必须等到 deadline 才强杀；实际 {spent:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `xray run -test` 预检：非法配置必须报 ConfigInvalid 并带上核心自己的话。
    #[tokio::test]
    async fn validate_config_reports_config_invalid() {
        let dir = temp_dir("validate");
        let (bin, _serial) = fake_core(&dir, "fake-xray-test", "echo 'invalid: missing inbounds' >&2\nexit 23");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        let err = validate_config(&bin, &cfg).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(err.message.contains("missing inbounds"), "实际：{}", err.message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn validate_config_accepts_a_zero_exit() {
        let dir = temp_dir("validate-ok");
        let (bin, _serial) = fake_core(&dir, "fake-xray-ok", "exit 0");
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        assert!(validate_config(&bin, &cfg).await.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `kill(pid, 0)` 探活：用于「进程真的不在了」这类断言。
    fn process_alive(pid: u32) -> bool {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, 0) == 0
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }

    #[test]
    fn log_level_detection_is_conservative() {
        assert_eq!(detect_level("2025/01/01 [Error] boom"), LogLevel::Error);
        assert_eq!(detect_level("2025/01/01 [Warning] hmm"), LogLevel::Warn);
        assert_eq!(detect_level("2025/01/01 [Debug] detail"), LogLevel::Debug);
        // 认不出来时按 Info：不能把普通一行标成错误。
        assert_eq!(detect_level("2025/01/01 started"), LogLevel::Info);
    }
}
