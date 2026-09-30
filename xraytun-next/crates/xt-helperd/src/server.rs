//! AF_UNIX 服务端 + 连接循环。
//!
//! # 帧读写为什么不是 xt-ipc 的 `Server`
//!
//! `xt_ipc::Server`/`Connection` 绑定的是 daemon 的 `Frame` 类型；helper 用的是
//! `xt-helperproto` 的 `Request`/`Response`（**独立演进的封闭指令集**，见
//! `xt-helperproto` 的模块文档）。两者只共享字节级约定（`u32 大端长度前缀 ||
//! JSON`），不共享类型。所以这里自己写 tokio 的 `UnixListener` 循环，只把
//! `xt-ipc` 的两个做法原样搬过来：
//!
//! 1. **残留 socket 只删确实是 socket 的那一个文件**（目录 / 普通文件一律拒绝）；
//! 2. **`read_exact` 不是取消安全的** —— 已读字节搬进连接自己的 [`FrameReader`]，
//!    而不是 future 的局部变量。
//!
//! # 一条连接的生命周期
//!
//! ```text
//! accept
//!   └─ trust::authorize(fd)         ← 处理任何指令之前，先校验对端身份
//!        ├─ 失败 → 写一帧 Response::Error 后关闭
//!        └─ 成功 → FrameReader + OwnedWriteHalf
//!             └─ loop { 读请求 → dispatch → 写响应（TakeTunFd 再补一发 SCM_RIGHTS） }
//! ```
//!
//! 每条连接一个 tokio 任务；请求频率极低，且**严格一问一答**正是 `SCM_RIGHTS`
//! 传递时序正确性的前提（帧先、fd 后，见 [`crate::fdpass`]）。

use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use xt_contract::error::{bad_request, internal, ErrorBody, ErrorCode};
use xt_contract::MAX_FRAME_BYTES;
use xt_helperproto::{Request, Response};

use crate::dispatch::{self, SharedState};
use crate::fdpass;
use crate::trust::{self, TrustPolicy};

/// 每次从 socket 读的字节数。帧体上限 [`MAX_FRAME_BYTES`]（1 MiB），
/// 这个块大小只影响系统调用次数，不影响语义。
const READ_CHUNK: usize = 8192;

/// socket 文件的权限位：`root:admin 0660`。
///
/// **它不是真正的门**（`SecCode` 校验才是）：它只是把「非 admin 组用户」挡在
/// 最外层，减少无谓的校验开销与日志噪音。macOS 首个用户默认就在 `admin` 组，
/// 所以绝不能只靠权限位。
const SOCKET_MODE: u32 = 0o660;

/// AF_UNIX 服务端。
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    /// 启动时确定的授权策略，被每条连接共享（只读）。
    policy: Arc<TrustPolicy>,
}

impl Server {
    /// 绑定 AF_UNIX 监听，并把 socket 文件收紧到 `root:admin 0660`。
    ///
    /// 上次崩溃留下的 socket 文件会让 `bind` 直接 `EADDRINUSE`，所以先清掉它 ——
    /// 但**只删确实是 socket 的那一个文件**：符号链接、目录、普通文件一律拒绝，
    /// 绝不覆盖。helper 以 root 运行，「误删用户文件」比「启动失败」严重得多。
    pub async fn bind(path: &Path, policy: Arc<TrustPolicy>) -> Result<Server, ErrorBody> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => {
                std::fs::remove_file(path).map_err(|e| {
                    io_error(format!("删除残留 socket {} 失败", path.display()), e)
                })?;
                tracing::info!(path = %path.display(), "清掉上次残留的 socket 文件");
            }
            Ok(_) => {
                return Err(bad_request(format!(
                    "{} 已存在且不是 socket 文件，拒绝覆盖",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(format!("探测 {} 失败", path.display()), e)),
        }

        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    io_error(format!("创建 socket 目录 {} 失败", dir.display()), e)
                })?;
            }
        }

        let listener = UnixListener::bind(path)
            .map_err(|e| io_error(format!("绑定 {} 失败", path.display()), e))?;
        restrict_socket_permissions(path)?;

        Ok(Server { listener, path: path.to_path_buf(), policy })
    }

    /// 接受并服务连接，**永不返回**（除非 `accept` 本身坏到不可恢复）。
    ///
    /// `accept` 报错（例如 `EMFILE`）时记一条 warn 继续服务：一个瞬时错误不该
    /// 让整个特权进程退出，否则任何本地进程都能靠打满 fd 把 helper 打停。
    pub async fn serve(&self, state: SharedState) -> Result<(), ErrorBody> {
        tracing::info!(path = %self.path.display(), "helper 已就绪");
        loop {
            match self.listener.accept().await {
                Ok((stream, _addr)) => {
                    let state = state.clone();
                    let policy = self.policy.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, state, policy).await {
                            tracing::warn!(error = %error, "连接处理结束（带错误）");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(error = %error, "accept 失败，继续服务");
                }
            }
        }
    }

    /// 退出前摘掉 socket 文件。找不到不算失败（可能已经被清理过）。
    pub fn cleanup(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, path = %self.path.display(), "清理 socket 文件失败"),
        }
    }
}

/// 处理一条连接：先校验对端，再进入一问一答循环。
async fn handle_connection(
    mut stream: UnixStream,
    state: SharedState,
    policy: Arc<TrustPolicy>,
) -> Result<(), ErrorBody> {
    // ---- 第一道：处理任何指令之前校验对端身份 ----
    let raw = stream.as_raw_fd();
    let identity = match trust::authorize(raw, &policy) {
        Ok(identity) => identity,
        Err(error) => {
            // 拒绝时也尽量把原因写回一帧：对端可能是我们自己人（排障时需要
            // 看到「为什么被拒」），而写失败只说明它已经走了。
            tracing::warn!(error = %error, "拒绝对端；写一帧 Error 后关闭连接");
            let _ = write_response(&mut stream, &Response::Error { error }).await;
            return Ok(());
        }
    };
    tracing::info!(
        uid = identity.uid,
        gid = identity.gid,
        pid = identity.pid,
        "已接受的连接"
    );

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = FrameReader::new(read_half);

    loop {
        let request = match reader.next_request().await {
            Ok(request) => request,
            Err(error) => {
                // `Io` = 对端关闭 / 读失败（写回去也没意义）；
                // 其余 = 帧本身不合法（协议错误）⇒ 回一帧 Error 再关。
                if error.code == ErrorCode::Io {
                    tracing::debug!(error = %error, "对端断开");
                } else {
                    tracing::warn!(error = %error, "helper 帧不合法，关闭连接");
                    let _ = write_response(&mut write_half, &Response::Error { error }).await;
                }
                return Ok(());
            }
        };

        match dispatch::dispatch(state.as_ref(), request) {
            Ok(outcome) => {
                // 协议时序：**先响应帧，再 fd**（见 fdpass 的模块文档）。
                write_response(&mut write_half, &outcome.response).await?;
                if let Some(fd) = outcome.fd {
                    let socket = write_half.as_ref().as_raw_fd();
                    fdpass::send_fd(socket, fd)
                        .map_err(|e| io_error("sendmsg(SCM_RIGHTS) 发送 utun fd 失败", e))?;
                }
            }
            Err(error) => {
                // 指令失败**不是**连接错误：回一帧结构化 Error，连接继续可用
                // （例如「没有活跃会话」之后对端仍可发 TunUp）。
                tracing::warn!(error = %error, "指令被拒绝");
                write_response(&mut write_half, &Response::Error { error }).await?;
            }
        }
    }
}

/// 读侧的连接私有缓冲。存在的唯一理由：**`read_exact` 不是取消安全的**。
///
/// 只要将来有人在 `next_request()` 与别的 future 之间用 `tokio::select!`，
/// 被 drop 的 future 里那半截帧就会永久丢失，下一轮会把帧体当长度头 ——
/// 一种「只在恰好被打断时复现」的随机 JSON 解析失败。所以：await 点只用
/// 取消安全的 [`AsyncReadExt::read`]，读到的字节立刻搬进属于连接的 `buffer`。
struct FrameReader<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R> FrameReader<R>
where
    R: AsyncRead + Unpin,
{
    fn new(reader: R) -> Self {
        FrameReader { reader, buffer: Vec::new() }
    }

    /// 下一帧请求。对端关闭（或帧读到一半就断）→ `Err`（`code = Io`）。
    async fn next_request(&mut self) -> Result<Request, ErrorBody> {
        loop {
            if let Some(request) = take_request(&mut self.buffer)? {
                return Ok(request);
            }
            let mut chunk = [0_u8; READ_CHUNK];
            let read = match self.reader.read(&mut chunk).await {
                Ok(0) => return Err(peer_closed(self.buffer.is_empty())),
                Ok(read) => read,
                Err(error) => return Err(io_error("读 socket 失败", error)),
            };
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// 缓冲够一帧就取走；不够就 `Ok(None)` 继续读。
///
/// 长度前缀**超过上限时不必等帧体到齐**，当场拒绝：否则一条 `0xFFFFFFFF` 的
/// 长度头就会让 helper 无限缓冲直到 OOM。
fn take_request(buffer: &mut Vec<u8>) -> Result<Option<Request>, ErrorBody> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let declared = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    if declared > MAX_FRAME_BYTES {
        return Err(bad_request(format!(
            "帧体声明 {declared} 字节，超过上限 {MAX_FRAME_BYTES} 字节：不允许分片"
        )));
    }
    let total = 4 + declared as usize;
    if buffer.len() < total {
        return Ok(None);
    }
    // decode_request 会再校验一次「长度声明 == 实际帧体」，这里不重复它的判断。
    let request = xt_helperproto::decode_request(&buffer[..total])?;
    buffer.drain(..total);
    Ok(Some(request))
}

/// 写一帧响应（`长度前缀 || JSON`）并 flush。
async fn write_response<W>(writer: &mut W, response: &Response) -> Result<(), ErrorBody>
where
    W: AsyncWrite + Unpin,
{
    let bytes = xt_helperproto::encode_response(response)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| io_error("写帧失败", e))?;
    writer.flush().await.map_err(|e| io_error("刷新帧失败", e))
}

/// 把 socket 收紧到 `root:admin 0660`。
fn restrict_socket_permissions(path: &Path) -> Result<(), ErrorBody> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(|e| io_error(format!("设置 socket 权限失败：{}", path.display()), e))?;

    // macOS 上 admin 组的 gid 通常是 80，但用 getgrnam 现查更稳（不写死）。
    let gid = admin_gid().unwrap_or(80);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| internal("socket 路径含 NUL 字节"))?;
    // SAFETY: c_path 是有效的 NUL 结尾 C 字符串，chown 只读它；gid 来自
    // getgrnam 或常量 80，都不是指针。
    let rc = unsafe { libc::chown(c_path.as_ptr(), 0, gid) };
    if rc != 0 {
        return Err(io_error("chown socket 失败", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// 查 `admin` 组的 gid。
fn admin_gid() -> Option<libc::gid_t> {
    let name = std::ffi::CString::new("admin").ok()?;
    // SAFETY: getgrnam 返回指向 libc 静态缓冲区的指针；我们在同一线程内、
    // 没有其它 libc 调用插入的情况下立即读取 gr_gid。
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        None
    } else {
        // SAFETY: gr 非空，且按 getgrnam 的约定在本读取点仍然有效。
        Some(unsafe { (*gr).gr_gid })
    }
}

fn peer_closed(clean: bool) -> ErrorBody {
    let message = if clean {
        "对端已关闭连接"
    } else {
        "对端在帧读完之前关闭了连接"
    };
    ErrorBody::new(ErrorCode::Io, message)
}

fn io_error(what: impl std::fmt::Display, error: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, format!("{what}：{error}"))
}
