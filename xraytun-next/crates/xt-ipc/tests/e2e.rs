//! 真 AF_UNIX socket 端到端测试：真实 tempdir、真实字节、真实握手。
//!
//! 顺序约定：所有事件测试都先 `client.events()` 再触发会产生事件的操作。
//! 契约里没有事件重放，所以「先订阅、后触发」是使用方必须遵守的次序。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use xt_contract::error::{bad_request, ErrorCode};
use xt_contract::model::{
    Capability, ConnectionView, DaemonHello, LogLevel, LogLine, RunMode, Stage, ALL_TOPICS,
};
use xt_contract::protocol::{Event, Frame, Outcome, Request, RequestId, Response};
use xt_contract::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
use xt_ipc::{Client, Connection, Server};

/// 每个测试独占一个目录，进程退出前删掉。不用外部 crate：依赖面越小越好。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("xt-ipc-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn socket(&self) -> PathBuf {
        self.path.join("daemon.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn hello(protocol_version: u32) -> DaemonHello {
    DaemonHello {
        daemon_version: "xt-testd 1.0".to_string(),
        protocol_version,
        capabilities: vec![Capability::ProxyMode, Capability::Stats],
        pid: std::process::id(),
        started_at_ms: 1,
    }
}

fn connected_view(since_ms: u64) -> ConnectionView {
    ConnectionView {
        stage: Stage::Connected,
        mode: Some(RunMode::Proxy),
        connected_since_ms: Some(since_ms),
        ..ConnectionView::default()
    }
}

fn log_event(message: &str) -> Event {
    Event::Log {
        line: LogLine {
            ts_ms: 1,
            level: LogLevel::Info,
            target: "xt-testd".to_string(),
            message: message.to_string(),
        },
    }
}

async fn expect_request(conn: &mut Connection) -> (RequestId, Request) {
    match conn.next_frame().await.unwrap() {
        Frame::Request { id, request } => (id, request),
        other => panic!("期望 Request 帧，收到 {other:?}"),
    }
}

/// 服务端桩：完成 hello + subscribe 握手（与 xt-ipc::Client 的握手顺序一致）。
async fn serve_handshake(conn: &mut Connection, protocol_version: u32) {
    let (id, request) = expect_request(conn).await;
    assert!(matches!(request, Request::Hello { .. }), "第一个请求必须是 hello");
    conn.send(Frame::response(id, Outcome::ok(Response::Hello(hello(protocol_version)))))
        .await
        .unwrap();

    let (id, request) = expect_request(conn).await;
    assert!(matches!(request, Request::Subscribe { .. }), "hello 之后必须订阅");
    conn.send(Frame::response(
        id,
        Outcome::ok(Response::Subscribed { topics: ALL_TOPICS.to_vec() }),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn request_response_and_event_seq_over_real_socket() {
    let dir = TempDir::new("e2e");
    let server = Server::bind(&dir.socket()).await.unwrap();
    assert_eq!(server.path(), dir.socket().as_path());

    let srv = tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        serve_handshake(&mut conn, PROTOCOL_VERSION).await;

        let (id, request) = expect_request(&mut conn).await;
        assert_eq!(request, Request::Status);
        conn.send(Frame::response(id, Outcome::ok(Response::Status(connected_view(77)))))
            .await
            .unwrap();
        for seq in 1..=5 {
            conn.send(Frame::event(seq, log_event(&format!("e{seq}")))).await.unwrap();
        }
        // 等客户端关闭（读到 EOF 即退出）。
        while conn.next_frame().await.is_ok() {}
    });

    let client = Client::connect(&dir.socket(), "xt-test-cli/1.0").await.unwrap();
    assert_eq!(client.hello().daemon_version, "xt-testd 1.0");
    assert_eq!(client.hello().protocol_version, PROTOCOL_VERSION);

    // 先订阅，再触发事件。
    let mut events = client.events();
    let response = client.request(Request::Status).await.unwrap();
    assert_eq!(response, Response::Status(connected_view(77)));

    // 事件 seq 必须连续到达（跳号会被客户端报成 Notice，这里不该出现）。
    for seq in 1..=5 {
        match events.recv().await {
            Ok(Event::Log { line }) => assert_eq!(line.message, format!("e{seq}")),
            other => panic!("第 {seq} 个事件不是预期日志：{other:?}"),
        }
    }

    client.close().await;
    srv.await.unwrap();
}

#[tokio::test]
async fn daemon_rejecting_hello_propagates_error() {
    let dir = TempDir::new("hello-err");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let sock = dir.socket();
    tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        let (id, _) = expect_request(&mut conn).await;
        conn.send(Frame::response(id, Outcome::error(bad_request("协议版本不符"))))
            .await
            .unwrap();
    });

    let error = match Client::connect(&sock, "cli").await {
        Ok(_) => panic!("daemon 拒绝 hello 时 connect 必须失败"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn daemon_protocol_version_mismatch_is_rejected() {
    let dir = TempDir::new("ver");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let sock = dir.socket();
    tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        let (id, _) = expect_request(&mut conn).await;
        // daemon 报了一个我们不认识的协议版本：不做兼容，直接拒绝。
        conn.send(Frame::response(
            id,
            Outcome::ok(Response::Hello(hello(PROTOCOL_VERSION + 1))),
        ))
        .await
        .unwrap();
        while conn.next_frame().await.is_ok() {}
    });

    let error = match Client::connect(&sock, "cli").await {
        Ok(_) => panic!("协议版本不符必须被拒绝"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert!(error.message.contains("协议版本"), "{}", error.message);
}

#[tokio::test]
async fn closed_connection_wakes_in_flight_request_with_io() {
    let dir = TempDir::new("closed");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let srv = tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        serve_handshake(&mut conn, PROTOCOL_VERSION).await;
        // 请求已经到了，但故意不回答，然后断开。
        let _ = expect_request(&mut conn).await;
        drop(conn);
    });

    let client = Client::connect(&dir.socket(), "cli").await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), client.request(Request::Status)).await;
    match outcome {
        Ok(Err(error)) => {
            assert_eq!(error.code, ErrorCode::Io, "断开应得到 io，实得 {error:?}");
        }
        Ok(Ok(response)) => panic!("连接已经断了，不该有响应：{response:?}"),
        Err(_) => panic!("对端断开后在途 request 挂死了：必须被立刻唤醒为 io 错误"),
    }
    srv.await.unwrap();
}

#[tokio::test]
async fn response_id_mismatch_is_internal() {
    let dir = TempDir::new("orphan");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let srv = tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        serve_handshake(&mut conn, PROTOCOL_VERSION).await;
        let (id, _) = expect_request(&mut conn).await;
        // 用错误的 id 回答：对端 id 分发坏了，客户端必须判定为 internal。
        conn.send(Frame::response(id + 1000, Outcome::ok(Response::Ok))).await.unwrap();
        while conn.next_frame().await.is_ok() {}
    });

    let client = Client::connect(&dir.socket(), "cli").await.unwrap();
    let error = client.request(Request::Status).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    client.close().await;
    srv.await.unwrap();
}

#[tokio::test]
async fn event_seq_gap_is_reported_as_notice() {
    let dir = TempDir::new("gap");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let srv = tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        serve_handshake(&mut conn, PROTOCOL_VERSION).await;
        let (id, _) = expect_request(&mut conn).await;
        conn.send(Frame::response(id, Outcome::ok(Response::Status(connected_view(1)))))
            .await
            .unwrap();
        // seq 1 之后直接跳到 3：客户端必须如实报出跳号，而不是静默。
        conn.send(Frame::event(1, log_event("a"))).await.unwrap();
        conn.send(Frame::event(3, log_event("c"))).await.unwrap();
        while conn.next_frame().await.is_ok() {}
    });

    let client = Client::connect(&dir.socket(), "cli").await.unwrap();
    let mut events = client.events();
    let _ = client.request(Request::Status).await.unwrap();

    match events.recv().await {
        Ok(Event::Log { line }) => assert_eq!(line.message, "a"),
        other => panic!("第一个事件不对：{other:?}"),
    }
    match events.recv().await {
        Ok(Event::Notice { notice }) => {
            assert_eq!(notice.code, ErrorCode::Internal);
            assert!(notice.message.contains("序号"), "{}", notice.message);
        }
        other => panic!("跳号必须产生 Notice，实得 {other:?}"),
    }
    match events.recv().await {
        Ok(Event::Log { line }) => assert_eq!(line.message, "c"),
        other => panic!("第三个事件不对：{other:?}"),
    }

    client.close().await;
    srv.await.unwrap();
}

#[tokio::test]
async fn oversized_incoming_frame_is_rejected_at_the_server() {
    let dir = TempDir::new("big");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let sock = dir.socket();
    tokio::spawn(async move {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let mut bytes = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        stream.write_all(&bytes).await.unwrap();
        stream.flush().await.unwrap();
    });

    let mut conn = server.accept().await.unwrap();
    let error = conn.next_frame().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn bind_replaces_stale_socket_file_only() {
    let dir = TempDir::new("stale");
    let sock = dir.socket();
    {
        let server = Server::bind(&sock).await.unwrap();
        drop(server);
    }
    assert!(sock.exists(), "listener drop 后 socket 文件应残留（这是要清的场景）");
    let server = Server::bind(&sock).await.expect("残留 socket 必须能被清掉并重新绑定");
    assert_eq!(server.path(), sock.as_path());
}

#[tokio::test]
async fn bind_refuses_to_delete_non_socket_file() {
    let dir = TempDir::new("regular");
    let sock = dir.socket();
    std::fs::write(&sock, b"important user data").unwrap();
    let error = Server::bind(&sock).await.err().expect("普通文件在路径上必须拒绝");
    assert_eq!(error.code, ErrorCode::Io);
    assert_eq!(std::fs::read(&sock).unwrap(), b"important user data");
}

#[tokio::test]
async fn next_frame_is_cancel_safe_across_select_branches() {
    let dir = TempDir::new("cancel");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let mut stream = UnixStream::connect(&dir.socket()).await.unwrap();
    let mut conn = server.accept().await.unwrap();

    // 先只发「长度头 + 半个帧体」。
    let frame = xt_ipc::encode(&Frame::request(1, Request::Status)).unwrap();
    let (header, body) = frame.split_at(4);
    let split = body.len() / 2;
    stream.write_all(header).await.unwrap();
    stream.write_all(&body[..split]).await.unwrap();
    stream.flush().await.unwrap();

    // 另一个分支必然先到期（timeout 只是测试里的取消驱动）：next_frame 的 future 被 drop。
    tokio::select! {
        result = conn.next_frame() => panic!("帧还没发完却成功了：{result:?}"),
        _ = tokio::time::timeout(Duration::from_millis(50), std::future::pending::<()>()) => {}
    }

    // 补齐剩余字节：下一次 next_frame 必须接着缓冲把整帧拼出来。
    // 如果已读字节是存在 future 局部变量里的，这里就会把帧体当成长度头而解析失败。
    stream.write_all(&body[split..]).await.unwrap();
    stream.flush().await.unwrap();

    match conn.next_frame().await.unwrap() {
        Frame::Request { id, request } => {
            assert_eq!(id, 1);
            assert_eq!(request, Request::Status);
        }
        other => panic!("取消后拼出来的帧不对：{other:?}"),
    }
}

/// 证明「单任务 select 同时收请求 + 推事件」在冻结的 Connection 形状下可用：
/// `next_frame()` 的 future 在 `select!` 内部创建，所以分支体里还能再用 `conn.send()`。
#[tokio::test]
async fn one_task_can_recv_requests_and_push_events_with_select() {
    let dir = TempDir::new("select");
    let server = Server::bind(&dir.socket()).await.unwrap();
    let sock = dir.socket();
    let srv = tokio::spawn(async move {
        let mut conn = server.accept().await.unwrap();
        serve_handshake(&mut conn, PROTOCOL_VERSION).await;

        let (push_tx, mut push_rx) = tokio::sync::mpsc::channel::<Frame>(4);
        loop {
            tokio::select! {
                incoming = conn.next_frame() => match incoming {
                    Ok(Frame::Request { id, request }) => {
                        let outcome = match request {
                            Request::Status => Outcome::ok(Response::Status(connected_view(9))),
                            other => Outcome::error(bad_request(format!("测试桩不支持 {other:?}"))),
                        };
                        conn.send(Frame::response(id, outcome)).await.unwrap();
                        // 收到请求后把事件塞进通道：下一次 select 必须走事件分支。
                        for seq in 1..=3 {
                            push_tx
                                .send(Frame::event(seq, log_event(&format!("p{seq}"))))
                                .await
                                .unwrap();
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Some(frame) = push_rx.recv() => {
                    conn.send(frame).await.unwrap();
                }
            }
        }
    });

    let client = Client::connect(&sock, "cli").await.unwrap();
    let mut events = client.events();
    let response = client.request(Request::Status).await.unwrap();
    assert_eq!(response, Response::Status(connected_view(9)));
    for seq in 1..=3 {
        match events.recv().await {
            Ok(Event::Log { line }) => assert_eq!(line.message, format!("p{seq}")),
            other => panic!("第 {seq} 个事件不对：{other:?}"),
        }
    }
    client.close().await;
    srv.await.unwrap();
}
