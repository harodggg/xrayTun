//! 观察模式的**转发**判据（只用现有公开 API，因此可以"改前先红"）：
//!
//! * 判据 1：`Content-Length` 与 `Transfer-Encoding: chunked` 两种请求 body
//!   都**逐字节原样**到达上游；
//! * 判据 2：chunked 响应读到终止 chunk 后**原样**透传给客户端（不重编码、不改成 CL）；
//! * 附加：WebSocket 升级仍然回 501，但**必须计数**（不许静默拒绝）。
//!
//! 这里刻意**不用**观察模式的任何新 API —— 这样在实现之前跑，判据 1/2 会因为
//! "带 body 的请求直接不处理 / chunked 直接报错"而**在运行期红**，而不是编译不过。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    ca_cert_der, client_config, https_raw, install_crypto_provider, FakeUpstream, ScriptedOrigin,
};
use xt_mitm::{serve, BlocklistDecider, LocalCa, ProxyConfig};

struct Harness {
    proxy: xt_mitm::ProxyHandle,
    origin: ScriptedOrigin,
    client: Arc<rustls::ClientConfig>,
    _upstream: FakeUpstream,
}

/// 起一套"真 TLS 终结 + 真 SOCKS 回连 + 可编排源站"的夹具。
fn harness(responses: Vec<Vec<u8>>) -> Harness {
    install_crypto_provider();
    let origin = ScriptedOrigin::start(responses);
    let upstream = FakeUpstream::start(origin.port);
    let ca = Arc::new(LocalCa::generate().unwrap());
    let client = client_config(ca_cert_der(&ca));
    let cfg = ProxyConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream_socks: format!("127.0.0.1:{}", upstream.port).parse().unwrap(),
        io_timeout: Duration::from_secs(5),
        max_connections: 16,
        assumed_port: 443,
    };
    let proxy = serve(cfg, ca, Arc::new(BlocklistDecider::default())).expect("起代理");
    Harness { proxy, origin, client, _upstream: upstream }
}

fn simple_ok() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()
}

/// 判据 1（Content-Length）：body 里放上 NUL / 高位字节 / CRLF，逐字节比对。
#[test]
fn a_content_length_request_body_reaches_upstream_byte_for_byte() {
    let h = harness(vec![simple_ok()]);
    // 故意包含 0x00、0xff、CRLF —— 任何"当字符串处理"、任何重编码都会在这里露馅。
    let body: Vec<u8> = vec![0x00, 0x01, b'{', b'"', b'p', b'}', 0xff, 0xfe, b'\r', b'\n', 0x7f];
    let mut req = format!(
        "POST /api/timeline HTTP/1.1\r\nHost: news.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(&body);

    let (resp, clean) = https_raw(h.proxy.listen, "news.example", &req, &h.client);
    assert!(clean, "代理必须干净收尾");
    assert!(resp.contains("200 OK"), "应当拿到上游应答：{resp:?}");
    assert!(h.origin.wait_bodies(1, Duration::from_secs(3)), "上游必须收到请求");
    assert_eq!(h.origin.last_body(), body, "body 必须逐字节原样到达上游");
    let st = h.proxy.stats();
    assert_eq!((st.passed, st.failed), (1, 0), "{st:?}");
}

/// 判据 1（chunked）：请求体带着分块框架原样转发（不许解码后重编码）。
#[test]
fn a_chunked_request_body_reaches_upstream_byte_for_byte() {
    let h = harness(vec![simple_ok()]);
    let chunked: &[u8] = b"4\r\n\x00\x01\xff\xfe\r\n5\r\nhello\r\n0\r\n\r\n";
    let mut req = b"POST /api/timeline HTTP/1.1\r\nHost: news.example\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    req.extend_from_slice(chunked);

    let (resp, clean) = https_raw(h.proxy.listen, "news.example", &req, &h.client);
    assert!(clean, "代理必须干净收尾");
    assert!(resp.contains("200 OK"), "应当拿到上游应答：{resp:?}");
    assert!(h.origin.wait_bodies(1, Duration::from_secs(3)), "上游必须收到请求");
    assert_eq!(h.origin.last_body(), chunked, "chunked 请求体必须原样到达上游");
}

/// 判据 2：上游发多个 chunk + 终止 chunk，客户端必须收到**完整且逐字节相同**的响应。
///
/// 断言的是"原样透传"：如果代理把 chunked 解码成 `Content-Length`，或把 chunk 边界
/// 重新编码，这条就会红。
#[test]
fn a_chunked_response_is_passed_through_verbatim() {
    let expected = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\n\r\n".to_vec();
    let h = harness(vec![expected.clone()]);

    let req = b"GET /api/timeline HTTP/1.1\r\nHost: news.example\r\nConnection: close\r\n\r\n";
    let (resp, clean) = https_raw(h.proxy.listen, "news.example", req, &h.client);
    assert!(clean, "代理必须干净收尾（含 close_notify）");
    assert_eq!(
        resp.as_bytes(),
        expected.as_slice(),
        "chunked 响应必须逐字节原样透传（不许改写 framing）"
    );
    assert!(
        resp.contains("Transfer-Encoding: chunked"),
        "不许把它改成 Content-Length"
    );
    let st = h.proxy.stats();
    assert_eq!((st.passed, st.failed), (1, 0), "交换必须成功：{st:?}");
}

/// WebSocket 仍然不拆，但**必须**回 501 且计数（静默拒绝是最难查的失败）。
#[test]
fn a_websocket_upgrade_is_refused_with_501_and_counted() {
    let h = harness(vec![simple_ok()]);
    let req = b"GET /ws HTTP/1.1\r\nHost: news.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
    let (resp, _clean) = https_raw(h.proxy.listen, "news.example", req, &h.client);
    assert!(resp.starts_with("HTTP/1.1 501"), "升级请求必须明确回 501：{resp:?}");
    assert_eq!(h.proxy.stats().websocket_refused, 1, "被拒的 WebSocket 必须计数");
    assert_eq!(h.origin.hits(), 0, "升级请求不许到达上游（本版不拆）");
}
