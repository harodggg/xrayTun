//! Xray-core 进程生命周期管理。
//!
//! 设计取舍：**配置变更 = 重启进程**，不做 gRPC 热更新。
//! 桌面客户端的配置变更频率是分钟级，`xray` 冷启动约 100~300ms，
//! 用户感知不到差别；换来的是完全不需要 `protoc` 代码生成、不需要维护
//! 与内核版本耦合的 proto 定义。需要热更新时再接入 `HandlerService`，
//! 接入点见 `docs/03-xray-integration.md`。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct CoreEvent {
    pub stream: LogStream,
    pub line: String,
}

/// 运行中的 Xray 进程。
pub struct XrayProcess {
    child: Child,
    pub binary: PathBuf,
    pub config_path: PathBuf,
    log_tasks: Vec<JoinHandle<()>>,
    /// 启动时刻，用于「已运行时长」展示。
    pub started_at: Instant,
}

impl XrayProcess {
    /// 拉起 `xray run -c <config>` 并开始转发日志。
    ///
    /// 注意 `kill_on_drop(true)`：即便上层忘了 shutdown，进程也不会变成孤儿。
    pub async fn spawn(
        binary: &Path,
        config_path: &Path,
        events: Option<UnboundedSender<CoreEvent>>,
    ) -> Result<Self> {
        Self::spawn_with_env(binary, config_path, events, &[]).await
    }

    /// 带额外环境变量的版本。
    ///
    /// 存在的唯一理由是 `XRAY_TUN_FD`：TUN 模式下必须告诉核心「utun fd 是几号」。
    /// 之所以要在这里做而不是在外面 `set_var` —— 进程级环境变量是全局可变状态，
    /// 在异步代码里「设置 → await → 启动」会被其它任务穿插，
    /// 属于典型的、只在并发下才复现的 bug。放在 `Command` 上就没有这个问题。
    pub async fn spawn_with_env(
        binary: &Path,
        config_path: &Path,
        events: Option<UnboundedSender<CoreEvent>>,
        envs: &[(&str, String)],
    ) -> Result<Self> {
        if !binary.exists() {
            return Err(Error::CoreNotFound(binary.display().to_string()));
        }

        let mut cmd = Command::new(binary);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.arg("run")
            .arg("-c")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // 让核心的工作目录落在配置文件旁边，这样它写出的相对路径资源
        // （geoip.dat / geosite.dat）能被找到。
        if let Some(dir) = config_path.parent() {
            cmd.current_dir(dir);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::CoreSpawn(format!("{}: {e}", binary.display())))?;

        let mut log_tasks = Vec::new();
        if let Some(tx) = events {
            if let Some(out) = child.stdout.take() {
                log_tasks.push(spawn_reader(out, LogStream::Stdout, tx.clone()));
            }
            if let Some(err) = child.stderr.take() {
                log_tasks.push(spawn_reader(err, LogStream::Stderr, tx));
            }
        }

        Ok(Self {
            child,
            binary: binary.to_path_buf(),
            config_path: config_path.to_path_buf(),
            log_tasks,
            started_at: Instant::now(),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// 进程是否已退出（用于 UI 侧检测核心崩溃）。
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// 优雅停止：先 `SIGTERM`，超过 `grace` 再 `SIGKILL`。
    ///
    /// Xray 收到 SIGTERM 会关闭监听并断开连接，比直接 SIGKILL 干净；
    /// 但内核偶尔会卡在关闭长连接上，所以必须留一个强杀兜底。
    pub async fn shutdown(mut self, grace: Duration) -> Result<()> {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // SAFETY: kill(2) 只读 pid，不涉及内存安全。
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }

        let waited = tokio::time::timeout(grace, self.child.wait()).await;
        if waited.is_err() {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }

        for t in self.log_tasks {
            t.abort();
        }
        Ok(())
    }
}

fn spawn_reader<R>(reader: R, stream: LogStream, tx: UnboundedSender<CoreEvent>) -> JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if tx.send(CoreEvent { stream, line }).is_err() {
                        break; // 接收端已关闭
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    })
}

/// 用 `-test` 静态校验配置，避免把明显非法的配置送进正在运行的核心。
///
/// Xray 的 CLI 形态在历史版本里变过（`xray -test -c` vs `xray run -test -c`），
/// 所以两种都试一次，谁先成功用谁。
pub async fn validate_config(binary: &Path, config_path: &Path) -> Result<()> {
    if !binary.exists() {
        return Err(Error::CoreNotFound(binary.display().to_string()));
    }

    let attempts: [&[&str]; 2] = [&["-test", "-c"], &["run", "-test", "-c"]];
    let mut last_stderr = String::new();

    for args in attempts {
        let output = Command::new(binary)
            .args(args)
            .arg(config_path)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| Error::CoreSpawn(format!("{}: {e}", binary.display())))?;

        if output.status.success() {
            return Ok(());
        }
        last_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if last_stderr.is_empty() {
            last_stderr = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }

    Err(Error::InvalidConfig(last_stderr))
}

/// 轮询等待某个本地端口可连接。
///
/// 为什么不解析日志里的 "started"？因为日志格式随版本变化，而端口可连
/// 是唯一的、与版本无关的就绪信号。
pub async fn wait_for_port(port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::CoreNotReady(timeout));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 解析核心可执行文件位置。
///
/// 查找顺序（前者优先）：
/// 1. 用户在设置里显式指定的路径；
/// 2. app bundle 内的 `Contents/Resources/xray` / `Contents/Resources/bin/xray`；
/// 3. `dev_binaries_dir`（调用方传入；发行版传 `None`）；
/// 4. 与当前可执行文件同级 / 同级 `binaries/` 下的 `xray`；
/// 5. Homebrew / 系统常见路径；
/// 6. `PATH`。
///
/// # 为什么第 3 项由调用方传入，而不是这里用 `option_env!("CARGO_MANIFEST_DIR")`
///
/// 因为**那是错的**，而且错得很隐蔽：`option_env!` 在**包含它的 crate**
/// 编译时展开，所以写在 `xt-core` 里拿到的是 `crates/xt-core`，
/// 而应用把核心放在 `apps/desktop/binaries/`。结果是「解析逻辑看起来对、
/// 路径也看起来对、但永远找不到」。
///
/// 开发期的目录布局只有应用自己知道，所以这个知识必须由应用提供。
pub fn resolve_core_binary(
    explicit: Option<&Path>,
    app_resource_dir: Option<&Path>,
    dev_binaries_dir: Option<&Path>,
) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(p) = explicit {
        candidates.push(p.to_path_buf());
    }
    if let Some(dir) = app_resource_dir {
        candidates.push(dir.join("xray"));
        candidates.push(dir.join("bin").join("xray"));
    }
    if let Some(dir) = dev_binaries_dir {
        candidates.push(dir.join("xray"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("xray"));
            candidates.push(dir.join("binaries").join("xray"));
        }
    }

    candidates.push(PathBuf::from("/opt/homebrew/bin/xray"));
    candidates.push(PathBuf::from("/usr/local/bin/xray"));
    candidates.push(PathBuf::from("/usr/bin/xray"));

    if let Some(found) = candidates.iter().find(|p| p.is_file()) {
        return Ok(found.clone());
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join("xray");
            if p.is_file() {
                return Ok(p);
            }
        }
    }

    Err(Error::CoreNotFound(format!(
        "已尝试：{}；以及 PATH 中的各个目录",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )))
}

/// 读取核心版本（`xray version`）。
pub async fn core_version(binary: &Path) -> Result<String> {
    let out = Command::new(binary)
        .arg("version")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::CoreSpawn(format!("{}: {e}", binary.display())))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or("").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn wait_for_port_times_out_on_closed_port() {
        // 端口 1 上不会有服务；用很短的超时验证错误路径。
        let err = wait_for_port(1, Duration::from_millis(150)).await;
        assert!(matches!(err, Err(Error::CoreNotReady(_))));
    }

    #[tokio::test]
    async fn wait_for_port_succeeds_on_open_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        assert!(wait_for_port(port, Duration::from_secs(2)).await.is_ok());
    }

    #[tokio::test]
    async fn resolve_reports_not_found_for_bad_explicit_path() {
        let err = resolve_core_binary(Some(Path::new("/nonexistent/xray")), None, None);
        assert!(err.is_err());
        // 错误信息里必须列出找过哪些位置 —— 否则用户只知道「找不到」，
        // 却不知道应该把文件放到哪里。
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("/nonexistent/xray"), "错误信息应包含尝试过的路径: {msg}");
    }

    #[test]
    fn dev_binaries_dir_is_searched() {
        let dir = std::env::temp_dir().join(format!("xt-corebin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("xray");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();

        let found = resolve_core_binary(None, None, Some(&dir)).unwrap();
        assert_eq!(found, fake);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_path_wins_over_dev_dir() {
        let dir = std::env::temp_dir().join(format!("xt-corebin2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dev = dir.join("xray");
        std::fs::write(&dev, b"#!/bin/sh\n").unwrap();
        let explicit = dir.join("other-xray");
        std::fs::write(&explicit, b"#!/bin/sh\n").unwrap();

        let found = resolve_core_binary(Some(&explicit), None, Some(&dir)).unwrap();
        assert_eq!(found, explicit, "用户显式指定的路径优先级必须最高");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn spawn_fails_cleanly_for_missing_binary() {
        let tx = tokio::sync::mpsc::unbounded_channel().0;
        let err = XrayProcess::spawn(Path::new("/nonexistent/xray"), Path::new("/tmp/x.json"), Some(tx)).await;
        assert!(matches!(err, Err(Error::CoreNotFound(_))));
    }

    /// 用一个假核心脚本验证：能拉起、能收到 stdout、能优雅关闭。
    #[tokio::test]
    async fn spawn_streams_logs_and_shuts_down() {
        let dir = std::env::temp_dir().join(format!("xt-proc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-xray");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            // 忽略参数，打印一行后挂起等待信号。
            writeln!(f, "#!/bin/sh").unwrap();
            writeln!(f, "echo 'fake core started'").unwrap();
            writeln!(f, "trap 'exit 0' TERM").unwrap();
            writeln!(f, "while true; do sleep 1; done").unwrap();
        }
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let cfg = dir.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let proc = XrayProcess::spawn(&script, &cfg, Some(tx)).await.unwrap();
        assert!(proc.pid().is_some());

        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("应能在 5s 内收到日志")
            .expect("channel 不应关闭");
        assert!(ev.line.contains("fake core started"));

        proc.shutdown(Duration::from_secs(2)).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
