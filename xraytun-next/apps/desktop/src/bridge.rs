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
//! # 会话边界（修 WebView 重载后必然自杀的那个 bug）
//!
//! 这个计数器是**进程级**的，而 UI 的 `expectedSeq` 是**每个 WebView 会话**从 1 开始的：
//! WebView 一旦整页重载，两者必然错位，UI 判「seq 跳号」→ 进入致命错误，且**永不恢复**
//! （`tauri.ts` 的 `fail()` 把实例钉死在 `closed`）。旧注释把这条局限写在明面上、没有修。
//!
//! 会话边界有**两个**信号，缺一不可（[`EventGate`]）：
//!
//! 1. **页面开始加载**（`on_page_load` 的 `PageLoadEvent::Started`，见 `lib.rs`）——这是
//!    唯一的、**及时**的边界信号：从这一刻起闸门关闭、序号归 1，事件先滞留，绝不推给
//!    一个还没有序号的页面。**归零只在这里发生。**
//! 2. **这一会话的第一条 `hello`**（「一条连接上的第一个请求」正是契约给 `hello` 的地位）
//!    —— 闸门打开，滞留的事件按序补发。同一个页面里的第二次 `hello`（用户按「重新连接」）
//!    **不动序号**：UI 的 `expectedSeq` 只在页面重建时才回到 1，这里若也归零，两边就会
//!    错开并把客户端永久钉死 —— 那等于把用户唯一的恢复动作变成自杀。
//!
//! 为什么不能只靠 `hello`：`hello` 只能**事后**知道边界，而事件可能在那之前就推出去
//! （桥缓存着连接时，daemon 的事件是持续到达的）。所以「及时关闸」必须由页面加载事件
//! 提供，「开闸」才由 `hello` 提供。
//!
//! 这不是补一段「没真正到达过」的序列：那些事件确实来自 daemon，只是被推迟到 UI 会话
//! 真正开始之后投递；滞留上限溢出时会如实补一条 `Notice`，绝不让丢帧变成静默。
//!
//! 别把这件事和 daemon → 桥 那一跳混起来：后者的丢帧由 `xt-ipc` 自己检测，并以
//! `Event::Notice` 的形式放进同一条流，本来就不会被吞掉。
//!
//! # 错误形状
//!
//! 命令的返回值是 `Outcome`（`{"status":"ok"|"error",...}`），**任何**失败都编码成
//! `Outcome::Error { error }` 正常返回。`ErrorBody` 是 `{code, message, detail?}`，
//! 与 `xt-contract` 完全一致，UI 的 `toErrorBody` 能原样识别。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{Notice, NoticeSeverity};
use xt_contract::protocol::{Event, Outcome, Request, Response};
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

/// 「桥 → WebView」这一跳的帧闸门 + 序号。
///
/// 为什么要有它（而不是一个裸的原子计数器）：UI 的 `expectedSeq` 是**每个页面会话**
/// 从 1 开始的，而进程级计数器不会自己归位。两个信号配合：**页面开始加载**时关闸并
/// 归零（[`EventGate::close_for_new_page`]），**这一会话的第一条 `hello`** 才开闸
/// （[`EventGate::begin_session`]）；关闸期间到达的事件滞留，开闸时按序补发。
///
/// **归零只发生在页面加载那一处。** 同一页面里的第二次 `hello`（用户按「重新连接」）
/// 不动序号 —— 动了就会和 UI 的 `expectedSeq` 错开，细节见 `begin_session` 的说明。
struct EventGate {
    /// 本会话是否已开始（= 收到过这一轮 UI 发来的 hello）。
    session_started: bool,
    /// 下一个交给 UI 的序号；会话开始后从 1 递增。
    seq: u64,
    /// 会话开始前滞留的事件，按到达顺序。
    held: Vec<Event>,
    /// 因 `held` 到达上限而被丢弃的条数；会话开始时以 `Notice` 如实上报，不静默。
    held_dropped: u64,
}

/// 滞留上限。只覆盖「连接已建好、但这一轮 UI 还没发 hello」这个窗口（通常毫秒级）；
/// 给足余量是因为 xray 跑起来时日志事件可能很密。
const HELD_CAP: usize = 1024;

impl EventGate {
    fn new() -> Self {
        EventGate { session_started: false, seq: 1, held: Vec::new(), held_dropped: 0 }
    }

    /// 收下一条来自 daemon 的事件。返回 `None` = 闸门关着（会话还没开始），已滞留。
    fn admit(&mut self, event: Event) -> Option<(u64, Event)> {
        if !self.session_started {
            if self.held.len() == HELD_CAP {
                // 丢最旧的：越早的事件越可能已经过时。但**记账**，不假装没发生。
                self.held.remove(0);
                self.held_dropped += 1;
            }
            self.held.push(event);
            return None;
        }
        let seq = self.seq;
        self.seq += 1;
        Some((seq, event))
    }

    /// 页面开始加载：关闸、序号归 1、清掉上一会话的滞留。
    ///
    /// **这是唯一给序号归零的地方。** 上一个会话若从未开过闸（页面加载了却没发
    /// `hello`），它滞留的事件对新页面已经作废 —— 但不能静默丢：计进 `held_dropped`，
    /// 由下一次 `begin_session` 如实报成 `Notice`。
    fn close_for_new_page(&mut self) {
        self.held_dropped += self.held.len() as u64;
        self.held.clear();
        self.session_started = false;
        self.seq = 1;
    }

    /// UI 发来一条 `hello`。**只有这是一个新会话时才归零**，返回「按序补齐」的滞留事件。
    ///
    /// 同一个 WebView 会话里的第二次 `hello`（用户按「重新连接」）**绝不能**归零：
    /// UI 的 `expectedSeq`（`tauri.ts`）是每个页面会话从 1 开始的**闭包变量**，它只在
    /// 页面重建时才回到 1。这里若也归零，两边立刻错开，下一条事件就被判「跳号」→
    /// `fail()` 把客户端**永久钉死** —— 那等于把用户唯一的恢复动作变成自杀。
    /// 序号继续往下走，UI 那边也还在同一个计数上，两边始终对齐。
    fn begin_session(&mut self, now_ms: u64) -> Vec<(u64, Event)> {
        if self.session_started {
            return Vec::new();
        }
        self.session_started = true;
        // 页面加载时已经归过一次；这里再显式写一次，让语义不依赖调用顺序。
        self.seq = 1;
        let mut out = Vec::with_capacity(self.held.len() + 1);
        if self.held_dropped > 0 {
            out.push((
                self.seq,
                Event::Notice {
                    notice: Notice {
                        severity: NoticeSeverity::Warning,
                        code: ErrorCode::Internal,
                        message: format!(
                            "桥在 UI 会话开始前滞留上限（{HELD_CAP} 条）已满，丢失 {} 条事件",
                            self.held_dropped
                        ),
                        at_ms: now_ms,
                    },
                },
            ));
            self.seq += 1;
            self.held_dropped = 0;
        }
        for event in std::mem::take(&mut self.held) {
            out.push((self.seq, event));
            self.seq += 1;
        }
        out
    }
}

/// 桥的全局状态，由 `tauri::Builder::manage` 注入。
pub struct BridgeState {
    /// `None` = 还没连过（或上一次连接已被判定失效）。锁的粒度是整个「连接决策」：
    /// 首次并发请求由同一把锁串行化，不会重复握手。
    ///
    /// 这里是 tokio 的 Mutex：它会被**跨 await 持有**（建立连接要 await）。
    connection: Mutex<Option<Connection>>,
    /// 事件序号与滞留缓冲。**投递与补发都在这一把锁内完成**，否则序号会交错。
    ///
    /// 刻意用 **std** 的 Mutex 而不是 tokio 的：`on_page_load` 的回调是**同步**的，
    /// 那里没有 async 上下文，用 tokio 的锁就只能 `block_on` —— 主线程上 deadlock 的
    /// 经典配方。代价是这把锁**绝不能跨 await 持有**；闸门里的操作全是同步的，能保证。
    gate: std::sync::Mutex<EventGate>,
}

impl BridgeState {
    pub fn new() -> Self {
        BridgeState { connection: Mutex::new(None), gate: std::sync::Mutex::new(EventGate::new()) }
    }

    /// 取闸门。锁中毒（持锁期间有人 panic）不该让桥从此推不出任何事件：这里取回内部值。
    /// 闸门里只有「序号 + 一个缓冲」，没有需要靠 panic 才能保护的跨字段不变量。
    fn gate(&self) -> std::sync::MutexGuard<'_, EventGate> {
        match self.gate.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// 页面开始加载：关上闸门、序号归 1。
///
/// 由 `lib.rs` 在 `WebviewWindowBuilder::on_page_load` 的 `PageLoadEvent::Started` 处调用。
/// 为什么这个信号不可省（以及为什么不能只靠 `hello`），见模块文档的「会话边界」。
///
/// 对 `R: Runtime` 泛型而不是写 `WebviewWindow`：后者是 `default_runtime` 宏生成的
/// 别名，直接写成泛型不必依赖那个别名的展开细节。
pub fn close_gate_for_new_page<R: tauri::Runtime>(
    window: &tauri::webview::WebviewWindow<R>,
) {
    let state = window.state::<BridgeState>();
    state.gate().close_for_new_page();
}

/// 按 `tauri.ts` 的 `handleEvent` 认的形状推一条事件给 WebView。
///
/// 推送失败只留痕：窗口可能已关闭、监听方可能已注销。壳不补发、不缓存 ——
/// 补发就是伪造一段没真正到达过的序列，那是撒谎。
fn emit_event(app: &AppHandle, seq: u64, event: Event) {
    let payload = serde_json::json!({ "seq": seq, "event": event });
    if let Err(error) = app.emit(EVENT_NAME, payload) {
        eprintln!("xt bridge: 事件推送失败（{EVENT_NAME}）：{error}");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
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

    // hello / subscribe 是**连接握手**的一部分，桥的 `Client::connect` 已经替 UI 做过了：
    // daemon 只接受一条连接上的**第一条** hello（第二条回 `invalid_request`），
    // 所以这里必须**本地吸收**、绝不转发。否则 UI 的 `hello()` 一发出就被 daemon 拒绝，
    // 整条连接在 UI 看来就是坏的 —— 真机 S6 上的表现正是「daemon 版本/pid 未知 + 设置未加载」。
    //
    // 桥的 `Client::connect` 内部已 `Subscribe` 了 `ALL_TOPICS`，所以 UI 的 subscribe
    // 也只需回一个成功应答（事件由桥的全量订阅覆盖，UI 侧自己按主题过滤）。
    match &request {
        Request::Hello { .. } => {
            // UI 会话从这里开始：序号归 1，并把这一轮开始**之前**滞留的事件按序补发。
            // 为什么必须归零见 `EventGate`：计数器的生命周期是进程，UI 的
            // `expectedSeq` 的生命周期是 WebView 会话 —— 不归零，重载后的第一帧
            // 就会被判「跳号」并自杀，而且那个实例再也救不回来。
            //
            // 补发与直发共用同一把锁，保证推给 UI 的 seq 始终单调。
            {
                let state = app.state::<BridgeState>();
                let mut gate = state.gate();
                for (seq, event) in gate.begin_session(now_ms()) {
                    emit_event(app, seq, event);
                }
            }
            return Ok(Response::Hello(client.hello().clone()));
        }
        Request::Subscribe { topics } => {
            return Ok(Response::Subscribed { topics: topics.clone() });
        }
        _ => {}
    }

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
                    // `admit` 与投递必须在**同一把锁**里：否则「滞留补发」与
                    // 「新事件直发」会交错，推给 UI 的序号就不再单调。
                    // 用**同步**作用域包住，保证这把 std 锁不跨 await（见 `BridgeState::gate`）。
                    let state = app.state::<BridgeState>();
                    let mut gate = state.gate();
                    if let Some((seq, event)) = gate.admit(event) {
                        emit_event(&app, seq, event);
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
