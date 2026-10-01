//! 桥：UI ↔ daemon 的唯一通道。
//!
//! # 它做什么（只有三件）
//!
//! 1. **懒连接**：第一条请求到达时才 `xt_ipc::Client::connect`；socket 路径变了就换连接。
//! 2. **代理请求**：把 UI 发来的 JSON 反序列化成 `xt_contract::protocol::Request`，
//!    交给 `Client::request`，把 `Response`（或 `ErrorBody`）原样交回去。
//! 3. **转发事件**：后台任务从 `Client::events()` 收帧，`app.emit("xt_daemon_event", ..)`
//!    推给 WebView。
//!
//! 它**不做**：重试、回落、缓存、把错误翻译成另一种错误、替 UI 拼请求。
//! 连接失败就返回 `Io`，UI 会把它如实显示出来。
//!
//! # `seq` 为什么是壳自己生成的（诚实说明，不是掩饰）
//!
//! UI 的事件处理器（`tauri.ts` 的 `handleEvent`）要求载荷是 `{ seq, event }`，并按
//! `seq` 从 1 开始做**连续性检查**。但 `xt_ipc::Client::events()` 交出来的是
//! `broadcast::Receiver<Event>` —— 真实序号 `Frame::Event.seq` 在 `xt-ipc` 内部就被
//! 消费掉了（它用序号检测丢帧，并在跳号时补一条本地 `Notice` 事件），**没有对外暴露**。
//!
//! 所以壳只能自己维护一个从 1 开始的单调计数器。这个计数器的语义是诚实的：
//! **它描述的是「桥 → WebView 这一跳」的帧序号**。daemon → 桥 这一跳的丢帧由
//! `xt-ipc` 自己检测，并以 `Event::Notice` 的形式放进同一条事件流 —— 也就是说，
//! 丢帧不会被吞掉，用户会在界面上看到那条 Notice。
//!
//! 局限（真机 S6 必须知道）：WebView 整页重载后 UI 的 `expectedSeq` 归 1，而壳的
//! 计数器不会归零，于是 UI 会判「seq 跳号」并进入致命错误。彻底修法是让 xt-ipc 的
//! `events()` 暴露真实序号（`(EventSeq, Event)`），那是 xt-ipc 的接口变更，不在本
//! 任务的范围内 —— 这里只把事实写清楚，不假装它不存在。
//!
//! # 错误形状
//!
//! 命令的返回值是 `Outcome`（`{"status":"ok"|"error",...}`），**任何**失败都编码成
//! `Outcome::Error { error }` 正常返回。`ErrorBody` 是 `{code, message, detail?}`，
//! 与 `xt-contract` 完全一致，UI 的 `toErrorBody` 能原样识别。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::protocol::{Outcome, Request, Response};
use xt_ipc::Client;

/// 与 `apps/ui/src/transport/tauri.ts` 的默认值逐字一致；改名就是破坏契约。
const COMMAND_NAME: &str = "xt_daemon_request";
const EVENT_NAME: &str = "xt_daemon_event";

/// `hello` 里如实上报的客户端版本。
///
/// 取自 UI 的 `contract.ts`（`CLIENT_VERSION = '0.0.0'`）。刻意**不**在这里编一个
/// 「桌面壳版本号」：`hello` 的语义是「谁连上来了」，编造版本就是假话。
const UI_CLIENT_VERSION: &str = "0.0.0";

/// 当前 daemon 连接（socket 路径 + 客户端句柄）。
struct Connection {
    socket_path: PathBuf,
    client: Arc<Client>,
}

/// 桥的全局状态，由 `tauri::Builder::manage` 注入。
pub struct BridgeState {
    /// `None` = 还没连过（或上一次连接已被判定失效）。锁的粒度是整个「连接决策」：
    /// 首次并发请求由同一把锁串行化，不会重复握手。
    connection: Mutex<Option<Connection>>,
    /// 事件序号计数器，从 1 开始（UI 的 `expectedSeq = 1`）。
    event_seq: AtomicU64,
}

impl BridgeState {
    pub fn new() -> Self {
        BridgeState { connection: Mutex::new(None), event_seq: AtomicU64::new(1) }
    }

    /// 下一个事件序号。`Relaxed` 足够：这里只需要原子自增，不靠它同步别的内存。
    fn next_event_seq(&self) -> u64 {
        self.event_seq.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for BridgeState {
    fn default() -> Self {
        BridgeState::new()
    }
}

/// 桥命令。名字里的 `xt_daemon_request` 必须与 `tauri.ts` 的 `commandName` 逐字一致。
///
/// 参数刻意全部是**拥有所有权**的类型（`String` / `u64` / `serde_json::Value`）而不是
/// `State<'_, _>`：Tauri 的异步命令对借用参数有额外约束，用拥有类型 + 命令内
/// `app.state()` 取状态可以完全绕开它，也更好读。
#[tauri::command]
pub async fn xt_daemon_request(
    app: AppHandle,
    socket_path: String,
    id: u64,
    request: serde_json::Value,
) -> Outcome {
    match handle_request(&app, &socket_path, id, request).await {
        Ok(response) => Outcome::ok(response),
        Err(error) => Outcome::error(error),
    }
}

/// 一次请求的完整处理：反序列化 → 确保连接 → 代理 → 判失效。
async fn handle_request(
    app: &AppHandle,
    socket_path: &str,
    ui_request_id: u64,
    request: serde_json::Value,
) -> Result<Response, ErrorBody> {
    // 请求形状不对是**调用方**的问题，如实说清楚，并且带上 UI 的 id 方便对日志。
    let request: Request = serde_json::from_value(request).map_err(|error| {
        eprintln!("xt bridge: {COMMAND_NAME} 收到无法解析的请求（ui_request_id={ui_request_id}）：{error}");
        bad_request(format!("请求不是 xt-contract 的 Request：{error}"))
            .with_detail(serde_json::json!({ "ui_request_id": ui_request_id }))
    })?;

    let client = ensure_client(app, socket_path).await?;
    match client.request(request).await {
        Ok(response) => Ok(response),
        Err(error) => {
            // `Io` 在 IPC 上压倒性地意味着「连接没了」。把缓存清掉，**下一次**请求
            // 会重新握手 —— 注意：本次请求的错误原样返回，绝不重发（无回落）。
            // 即使 `Io` 来自 daemon 的业务失败，代价也只是下一次多一次握手。
            if error.code == ErrorCode::Io {
                forget_connection(app, socket_path).await;
            }
            Err(error)
        }
    }
}

/// 确保有一条指向 `socket_path` 的连接。返回 `Arc<Client>` 是为了让调用方**不持有
/// 锁**也能发请求（`Client::request` 是 `&self`，本身就能并发）。
async fn ensure_client(app: &AppHandle, socket_path: &str) -> Result<Arc<Client>, ErrorBody> {
    let path = PathBuf::from(socket_path);
    if !path.is_absolute() {
        // 相对路径会随进程 cwd 漂移，而 cwd 对 GUI 进程没有稳定含义。拒绝比猜测好。
        return Err(bad_request(format!(
            "socketPath 必须是绝对路径，收到 {socket_path:?}"
        )));
    }

    let state = app.state::<BridgeState>();
    let mut guard = state.connection.lock().await;

    if let Some(existing) = guard.as_ref() {
        if existing.socket_path == path {
            return Ok(existing.client.clone());
        }
        // socket 变了：丢掉旧连接。旧的事件转发任务持有 broadcast::Receiver，
        // 会在最后一个 Sender（在 ClientInner 里）随 Arc 一起 drop 后收到 Closed 并退出。
        *guard = None;
    }

    let client = Client::connect(&path, UI_CLIENT_VERSION)
        .await
        .map_err(|error| {
            // 出处：`xt-ipc` 连接/握手失败统一给 `Io`（socket 不存在、连不上、
            // 协议版本不符、hello/subscribe 未按序应答）。
            //
            // 刻意**不**改写成 `HelperUnavailable`：在这个契约里那个码专指
            // 「特权 helper 不可用」（见 error.rs），而这里是控制面 daemon 连不上。
            // 用错码就是假话 —— UI 会按错误的分类给出错误的指引。
            let code = error.code;
            let detail = serde_json::json!({
                "socket_path": socket_path,
                "daemon_code": code.as_str(),
            });
            ErrorBody::new(
                code,
                format!("连接 daemon {} 失败：{}", path.display(), error.message),
            )
            .with_detail(detail)
        })?;

    let client = Arc::new(client);
    spawn_event_forwarder(app.clone(), &client);
    *guard = Some(Connection { socket_path: path, client: client.clone() });
    Ok(client)
}

/// 忘掉缓存连接（如果它还指向同一个 socket）。只影响「下一次请求要不要重新握手」，
/// 不影响任何在途请求：它们各自持有 `Arc<Client>` 的克隆，会正常拿到结果或错误。
async fn forget_connection(app: &AppHandle, socket_path: &str) {
    let state = app.state::<BridgeState>();
    let mut guard = state.connection.lock().await;
    let matches = guard
        .as_ref()
        .map(|connection| connection.socket_path.as_path() == Path::new(socket_path))
        .unwrap_or(false);
    if matches {
        *guard = None;
    }
}

/// 为一条**新**连接启动事件转发任务。
///
/// 任务只持有 `broadcast::Receiver`，不持有 `Client`：连接被换掉后，Sender 随
/// `ClientInner` 一起 drop，`recv()` 返回 `Closed`，任务自然结束，不会泄一个循环。
fn spawn_event_forwarder(app: AppHandle, client: &Client) {
    // 订阅必须在连接之后、任何事件产生之前的窗口内完成；`broadcast` 有 256 帧缓冲，
    // 握手期间到达的事件不会被丢。
    let mut events = client.events();

    // `setup` / 命令之外也可能是同步上下文，统一用 Tauri 的 runtime，不用裸 `tokio::spawn`。
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let seq = app.state::<BridgeState>().next_event_seq();
                    // 形状 = `{ seq, event }`，逐字对齐 `tauri.ts` 的 `handleEvent`。
                    let payload = serde_json::json!({ "seq": seq, "event": event });
                    if let Err(error) = app.emit(EVENT_NAME, payload) {
                        // 推送失败只留痕：窗口可能已关闭、监听方可能已注销。壳不补发、
                        // 不缓存 —— 补发就是伪造一段没真正到达过的序列。
                        eprintln!("xt bridge: 事件推送失败（{EVENT_NAME}）：{error}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // 慢消费者丢的是**它自己**的帧（不阻塞 daemon）。daemon → 桥 的跳号
                    // 已由 xt-ipc 生成 `Event::Notice` 放进同一通道，所以这里继续收即可，
                    // 不伪造补帧。
                    eprintln!(
                        "xt bridge: 事件消费者落后 {skipped} 帧；跳号已由 xt-ipc 的 Notice 如实上报"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
