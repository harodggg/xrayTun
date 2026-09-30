//! xt-ipc —— 本机 IPC：AF_UNIX + 4 字节大端长度前缀 JSON 帧，client/server
//!
//! 所有者：backend-1。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 帧格式：`u32 大端长度 || JSON 帧体`，长度只算帧体，上限 [`MAX_FRAME_BYTES`]。
//! 为什么不分片：超过上限几乎一定是 bug，而分片会把「一帧」变成需要状态机的
//! 字节流 —— 出错时谁也说不清丢的是哪一半。宁可当场拒绝。
//!
//! 为什么没有等待：读侧永远由 `read_exact` 的完成事件唤醒，写侧直接写；
//! 请求/响应用 oneshot 分发，事件用 broadcast 广播。整条链路里没有一个
//! 「等一会儿再看看」的地方。

use std::collections::HashMap;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, oneshot, Mutex};

use xt_contract::error::{bad_request, internal, ErrorBody, ErrorCode};
use xt_contract::model::{DaemonHello, Notice, NoticeSeverity, Topic, ALL_TOPICS};
use xt_contract::protocol::{Event, EventSeq, Frame, Outcome, Request, RequestId, Response};
use xt_contract::MAX_FRAME_BYTES;

/// 每个 `Client` 的事件缓冲。慢消费者只会丢自己的事件（不阻塞 daemon），
/// 丢帧会被 `Event.seq` 的连续性检查发现并如实报出来。
const CLIENT_EVENT_CAPACITY: usize = 256;

/// 握手占用的两个 RequestId；业务请求从它们之后开始编号。
const HELLO_REQUEST_ID: RequestId = 1;
const SUBSCRIBE_REQUEST_ID: RequestId = 2;
const FIRST_REQUEST_ID: RequestId = 3;

/// 把一帧编成 `长度前缀 || JSON`。超限**当场**返回 invalid_request。
pub fn encode(frame: &Frame) -> Result<Vec<u8>, ErrorBody> {
    let body = serde_json::to_vec(frame).map_err(|e| bad_request(format!("帧序列化失败：{e}")))?;
    let len = u32::try_from(body.len()).map_err(|_| frame_too_big(body.len()))?;
    check_len(len)?;
    let mut bytes = Vec::with_capacity(4 + body.len());
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// 从一帧完整字节（含 4 字节前缀）解出 [`Frame`]。
/// 长度声明必须与实际帧体**完全相等**：多一个字节少一个字节都是 invalid_request。
pub fn decode(bytes: &[u8]) -> Result<Frame, ErrorBody> {
    if bytes.len() < 4 {
        return Err(bad_request(format!(
            "帧只有 {} 字节，不足 4 字节长度前缀",
            bytes.len()
        )));
    }
    let (header, body) = bytes.split_at(4);
    let len = check_len(u32::from_be_bytes([header[0], header[1], header[2], header[3]]))? as usize;
    if body.len() != len {
        return Err(bad_request(format!(
            "长度前缀声明 {len} 字节，实际帧体 {} 字节：不允许分片或多余字节",
            body.len()
        )));
    }
    serde_json::from_slice(body).map_err(|e| bad_request(format!("帧体不是合法的 Frame JSON：{e}")))
}

fn check_len(len: u32) -> Result<u32, ErrorBody> {
    if len > MAX_FRAME_BYTES {
        return Err(frame_too_big(len as usize));
    }
    Ok(len)
}

fn frame_too_big(len: usize) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidRequest,
        format!("帧体 {len} 字节超过上限 {MAX_FRAME_BYTES} 字节：不允许分片"),
    )
}

/// AF_UNIX 服务端。
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// 绑定 AF_UNIX 监听。上次崩溃留下的 socket 文件会让 bind 直接失败，
    /// 所以先清掉它 —— 但**只删确实是 socket 的那一个文件**：目录、普通文件
    /// 一律拒绝，绝不覆盖。误删用户文件比启动失败严重得多。
    pub async fn bind(path: &Path) -> Result<Server, ErrorBody> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => {
                std::fs::remove_file(path)
                    .map_err(|e| io_error(format!("删除残留 socket {} 失败", path.display()), e))?;
                tracing::info!(path = %path.display(), "清掉上次残留的 socket 文件");
            }
            Ok(_) => {
                return Err(ErrorBody::new(
                    ErrorCode::Io,
                    format!("{} 已存在且不是 socket 文件，拒绝覆盖", path.display()),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(format!("探测 {} 失败", path.display()), e)),
        }
        let listener = UnixListener::bind(path)
            .map_err(|e| io_error(format!("绑定 {} 失败", path.display()), e))?;
        Ok(Server { listener, path: path.to_path_buf() })
    }

    /// 接受一条连接。每条连接是独立的帧流；daemon 为每条连接开一个任务。
    pub async fn accept(&self) -> Result<Connection, ErrorBody> {
        let (stream, _addr) = self
            .listener
            .accept()
            .await
            .map_err(|e| io_error("accept 失败", e))?;
        Ok(Connection::from_stream(stream))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 服务端的一条连接。读写各占一个半连接，所以 `next_frame` 与 `send`
/// 都只需要 `&mut self`：单任务里用一个 `tokio::select!` 就能边收边推。
pub struct Connection {
    reader: FrameReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Connection {
    fn from_stream(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Connection { reader: FrameReader::new(reader), writer }
    }

    /// 下一帧（**取消安全**，见 [`FrameReader`]）。对端关闭（或帧读到一半就断）
    /// → `Err`，`code = Io`，`detail.eof = true`。这里不返回 `Option`：调用方
    /// 需要区分的是「正常帧」与「连接没了」，而后者在契约里就是一个具体错误。
    pub async fn next_frame(&mut self) -> Result<Frame, ErrorBody> {
        self.reader.next_frame().await
    }

    pub async fn send(&mut self, frame: Frame) -> Result<(), ErrorBody> {
        write_frame(&mut self.writer, &frame).await
    }
}

/// 读侧缓冲。存在的唯一理由：**`read_exact` 不是取消安全的**。
///
/// daemon 会把 `Connection::next_frame()` 和事件推送放在同一个 `tokio::select!` 里；
/// 事件分支先就绪时，next_frame 的 future 会被 drop。如果已读字节只存在 future 的
/// 局部变量里，那一半帧就永久丢了，下一轮会把帧体当成长度头 —— 只在事件恰好打断时
/// 复现的随机 JSON 解析失败。所以：await 点只用取消安全的 [`AsyncReadExt::read`]，
/// 读到的字节立刻搬进 `self.buffer`（属于连接，不属于 future）。
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

    async fn next_frame(&mut self) -> Result<Frame, ErrorBody> {
        loop {
            if let Some(frame) = take_frame(&mut self.buffer)? {
                return Ok(frame);
            }
            let mut chunk = [0_u8; 8192];
            let read = match self.reader.read(&mut chunk).await {
                Ok(0) => return Err(peer_closed(self.buffer.is_empty())),
                Ok(read) => read,
                Err(error) => return Err(io_error("读 socket 失败", error)),
            };
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// 缓冲够一帧就取走；不够就 `Ok(None)` 继续读。长度超限**不必等帧体到齐**，
/// 当场拒绝。
fn take_frame(buffer: &mut Vec<u8>) -> Result<Option<Frame>, ErrorBody> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let len = check_len(u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]))? as usize;
    let total = 4 + len;
    if buffer.len() < total {
        return Ok(None);
    }
    let frame = decode(&buffer[..total])?;
    buffer.drain(..total);
    Ok(Some(frame))
}

/// 本机客户端。`connect` 内部完成 hello 握手与版本校验，所以构造成功即
/// 意味着「协议一致」这件事已经被证明，而不是被假设。
pub struct Client {
    inner: Arc<ClientInner>,
    reader_task: tokio::task::JoinHandle<()>,
}

struct ClientInner {
    hello: DaemonHello,
    next_request_id: AtomicU64,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<Result<Response, ErrorBody>>>>,
    events: broadcast::Sender<Event>,
    topics: Vec<Topic>,
    last_seq: Mutex<Option<EventSeq>>,
    /// `None` = 连接已关闭。之后任何 request 立刻得到 Io，而不是写进死 socket。
    writer: Mutex<Option<OwnedWriteHalf>>,
}

impl Client {
    /// 连接 daemon 并完成握手。`client_version` 只用于上报真实版本（UI 顶栏显示），
    /// 协议兼容性由 [`xt_contract::PROTOCOL_VERSION`] 严格判定：不符即拒绝，不做兼容分支。
    pub async fn connect(path: &Path, client_version: &str) -> Result<Client, ErrorBody> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|e| io_error(format!("连接 {} 失败", path.display()), e))?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = FrameReader::new(reader);

        // 握手在读循环启动前同步完成：Client 一旦构造成功，hello 就是真实观测值。
        write_frame(
            &mut writer,
            &Frame::request(
                HELLO_REQUEST_ID,
                Request::Hello {
                    client_version: client_version.to_string(),
                    protocol_version: xt_contract::PROTOCOL_VERSION,
                },
            ),
        )
        .await?;
        let hello = match expect_response(&mut reader, HELLO_REQUEST_ID, "hello").await? {
            Response::Hello(hello) => hello,
            other => return Err(internal(format!("hello 期望 Response::Hello，收到 {other:?}"))),
        };
        if hello.protocol_version != xt_contract::PROTOCOL_VERSION {
            return Err(bad_request(format!(
                "daemon 协议版本 {} 与本客户端 {} 不符，拒绝连接",
                hello.protocol_version,
                xt_contract::PROTOCOL_VERSION
            )));
        }

        // 公开形状只有 `events()`，没有 subscribe 入口，因此建立连接即订阅全部主题，
        // 由消费方自己挑。服务端仍按 topic 过滤，读循环再校验一次。
        let topics = ALL_TOPICS.to_vec();
        write_frame(
            &mut writer,
            &Frame::request(SUBSCRIBE_REQUEST_ID, Request::Subscribe { topics: topics.clone() }),
        )
        .await?;
        match expect_response(&mut reader, SUBSCRIBE_REQUEST_ID, "subscribe").await? {
            Response::Subscribed { .. } => {}
            other => {
                return Err(internal(format!("subscribe 期望 Response::Subscribed，收到 {other:?}")))
            }
        }

        let (events, _) = broadcast::channel(CLIENT_EVENT_CAPACITY);
        let inner = Arc::new(ClientInner {
            hello,
            next_request_id: AtomicU64::new(FIRST_REQUEST_ID),
            pending: Mutex::new(HashMap::new()),
            events,
            topics,
            last_seq: Mutex::new(None),
            writer: Mutex::new(Some(writer)),
        });
        let reader_task = tokio::spawn(reader_loop(reader, inner.clone()));
        Ok(Client { inner, reader_task })
    }

    /// 发一个请求并等它的响应。`&self`：连接内部自己做 id 分发，
    /// 所以多条请求可以并发；Event 帧不会插队到返回值里，它们走 [`Client::events`]。
    pub async fn request(&self, request: Request) -> Result<Response, ErrorBody> {
        let id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, sender);

        let written = {
            let mut guard = self.inner.writer.lock().await;
            match guard.as_mut() {
                Some(writer) => write_frame(writer, &Frame::request(id, request)).await,
                None => Err(closed_error()),
            }
        };
        if let Err(error) = written {
            self.inner.pending.lock().await.remove(&id);
            return Err(error);
        }

        // 读循环保证：连接一断就把所有在途请求唤醒成 Io。所以这里不会无限等。
        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(closed_error()),
        }
    }

    /// 事件流。读循环已按订阅主题过滤；广播语义下慢消费者丢自己的帧。
    ///
    /// 调用次序：**先 `events()`，再触发会产生事件的操作**。契约里没有事件重放，
    /// 所以订阅之前已经广播出去的事件不会被补发（幸好丢帧会被 seq 检查发现）。
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// daemon 的真实身份（版本 / pid / 协议版本），来自握手响应而非本地常量。
    pub fn hello(&self) -> &DaemonHello {
        &self.inner.hello
    }

    /// 主动关闭：先停读循环，再关写半（对端立刻看到 EOF），最后唤醒残余请求。
    pub async fn close(self) {
        self.reader_task.abort();
        let half = self.inner.writer.lock().await.take();
        if let Some(mut half) = half {
            let _ = half.shutdown().await;
        }
        self.inner.fail_pending(closed_error()).await;
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // 没有显式 close 时也要收掉读循环，否则任务会一直挂到进程结束。
        self.reader_task.abort();
    }
}

impl ClientInner {
    /// 把当前所有在途请求唤醒成同一个错误。连接断了就没有任何请求还能有结果，
    /// 让它们永远挂着是最糟糕的选择。
    async fn fail_pending(&self, error: ErrorBody) {
        let waiters: Vec<_> = {
            let mut guard = self.pending.lock().await;
            guard.drain().map(|(_, sender)| sender).collect()
        };
        for sender in waiters {
            let _ = sender.send(Err(error.clone()));
        }
    }

    async fn settle(&self, id: RequestId, outcome: Outcome) {
        let waiter = self.pending.lock().await.remove(&id);
        let result = match outcome {
            Outcome::Ok { response } => Ok(response),
            Outcome::Error { error } => Err(error),
        };
        match waiter {
            Some(sender) => {
                let _ = sender.send(result);
            }
            None => {
                // 响应 id 对不上任何在途请求 = 对端的 id 分发坏了。此时整条连接的
                // 结果都不可信，所以不是「忽略这一帧」，而是把所有在途请求判为 internal。
                tracing::error!(id, "收到没有对应请求的响应，所有在途请求判定为 internal");
                self.fail_pending(internal(format!("响应 id {id} 与任何在途请求都不匹配")))
                    .await;
            }
        }
    }

    async fn deliver_event(&self, seq: EventSeq, event: Event) {
        if !self.topics.contains(&event.topic()) {
            return;
        }
        let gap = {
            let mut last = self.last_seq.lock().await;
            let gap = last.map(|previous| previous + 1).filter(|expected| *expected != seq);
            *last = Some(seq);
            gap
        };
        if let Some(expected) = gap {
            // 契约要求：丢帧必须如实报出来，不能静默漂移。这里生成一条本地
            // Notice（这是真实观测到的事实），而不是伪造缺失的那几帧。
            tracing::error!(expected, seq, "事件序号不连续");
            let notice = Notice {
                severity: NoticeSeverity::Error,
                code: ErrorCode::Internal,
                message: format!("事件序号不连续：期望 {expected}，收到 {seq}"),
                at_ms: now_ms(),
            };
            let _ = self.events.send(Event::Notice { notice });
        }
        let _ = self.events.send(event);
    }
}

/// 读循环：它是这条连接唯一的读侧所有者。Response 按 id 分发；Event 广播；
/// 退出前把在途请求全部唤醒。
async fn reader_loop(mut reader: FrameReader<OwnedReadHalf>, inner: Arc<ClientInner>) {
    loop {
        match reader.next_frame().await {
            Ok(Frame::Response { id, outcome }) => inner.settle(id, outcome).await,
            Ok(Frame::Event { seq, event }) => inner.deliver_event(seq, event).await,
            Ok(Frame::Request { id, .. }) => {
                // 方向被搞反了：客户端连接上不该收到 Request。对端 bug，必须留日志。
                tracing::error!(id, "客户端连接收到 Request 帧（方向错误），已忽略");
            }
            Err(error) => {
                inner.fail_pending(error).await;
                return;
            }
        }
    }
}

/// 等一个指定 id 的响应（只用于握手阶段的同步往返）。
async fn expect_response<R>(
    reader: &mut FrameReader<R>,
    expected_id: RequestId,
    what: &str,
) -> Result<Response, ErrorBody>
where
    R: AsyncRead + Unpin,
{
    match reader.next_frame().await? {
        Frame::Response { id, outcome } if id == expected_id => match outcome {
            Outcome::Ok { response } => Ok(response),
            Outcome::Error { error } => Err(error),
        },
        Frame::Response { id, .. } => Err(internal(format!(
            "{what} 响应 id 不匹配：期望 {expected_id}，收到 {id}"
        ))),
        Frame::Event { .. } => Err(internal(format!("{what} 之前收到事件帧"))),
        Frame::Request { .. } => Err(internal(format!("{what} 之前收到请求帧"))),
    }
}

async fn write_frame<W>(writer: &mut W, frame: &Frame) -> Result<(), ErrorBody>
where
    W: AsyncWrite + Unpin,
{
    let bytes = encode(frame)?;
    writer.write_all(&bytes).await.map_err(|e| io_error("写帧失败", e))?;
    writer.flush().await.map_err(|e| io_error("刷新帧失败", e))
}

fn peer_closed(clean: bool) -> ErrorBody {
    let message = if clean {
        "对端已关闭连接"
    } else {
        "对端在帧读完之前关闭了连接"
    };
    ErrorBody::new(ErrorCode::Io, message)
        .with_detail(serde_json::json!({ "eof": true, "partial_frame": !clean }))
}

fn closed_error() -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, "与 daemon 的连接已关闭")
}

fn io_error(what: impl std::fmt::Display, error: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, format!("{what}：{error}"))
        .with_detail(serde_json::json!({ "kind": format!("{:?}", error.kind()) }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
