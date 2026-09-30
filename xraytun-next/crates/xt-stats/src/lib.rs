//! xt-stats —— 真实流量计数：向 Xray 的 `StatsService` 要数字，不估算。
//!
//! # 为什么是 StatsService
//!
//! 「显示网速」看起来是纯 UI 需求，实际上**没有任何现成数据**：物理网卡计数器
//! 在系统代理模式下混着无关流量，utun 计数器只在 TUN 模式存在，自己数包又看不见
//! （fd 已经交给核心）。唯一两条模式都对、还能分开上下行的来源，就是核心自己
//! 维护的计数器。它在生成配置时已经开好（`stats` + `api` 入站 + `policy` 开关），
//! 这里只负责把它读出来。
//!
//! # 采样的节拍归消费者
//!
//! 本模块**没有定时器、没有后台任务**：`sample()` 被调用一次就查一次。
//! 理由是「周期性采样」会自己制造两个谎言：
//! * 没人看界面时也在产生网络流量（api 入站的每次查询都是真实字节），
//!   空闲机器上也能读出几十字节 —— 用户会问「我又没上网，这数字哪来的」；
//! * 定时器一旦和核心重启撞上，就会留下一个刚归零的读数被展示成「流量掉了」。
//!
//! daemon 只在客户端请求 status/stats 时调用；100ms 内的重复请求由 daemon
//! 直接用上一次样本（那是缓存闸门，不是定时器）。未采样成功就是 `None`，
//! 绝不显示 0。

mod proto;

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use xt_contract::error::{ErrorBody, ErrorCode};
use xt_contract::model::StatsView;

use proto::{
    decode_grpc_frame, decode_query_stats_response, encode_grpc_frame, encode_query_stats_request,
    StatEntry,
};

/// `StatsService` 的全限定方法名。
const QUERY_STATS_PATH: &str = "/xray.app.stats.command.StatsService/QueryStats";

/// 自己轮询用的那个 api 入站的名字。它的计数器**必须排除**：
/// 每查一次统计都要走一次这个入站，把它算进去等于「看仪表盘的动作会让仪表盘转」。
const API_INBOUND_TAG: &str = "api";

/// 节点出口 tag 前缀（`node-<NodeId>`，由 xt-xrayconf 生成）。
/// 只收这个前缀是因为 `direct` / `api` 这类出口的字节不属于「隧道流量」。
const NODE_OUTBOUND_PREFIX: &str = "node-";

/// 一次 TCP+h2 握手与一次查询各自的失败上限。
/// 它们是**失败上限**，不是轮询周期 —— 超时即如实报错，不重试。
const CONNECT_DEADLINE: Duration = Duration::from_secs(3);
const SAMPLE_DEADLINE: Duration = Duration::from_secs(5);

/// 一个连着核心 api 入站的 h2c 客户端。
///
/// `connect()` 会真的完成 TCP 连接与 HTTP/2 握手：返回 `Ok` 就意味着
/// 「此刻 StatsService 的传输层是活的」。这是刻意的 —— 一个只记下地址、
/// 到 `sample()` 才可能失败的 `connect` 等于在骗调用方。
pub struct StatsClient {
    /// `send_request` 需要 `&mut`，而 `sample` 只有 `&self`。
    sender: tokio::sync::Mutex<h2::client::SendRequest<Bytes>>,
    authority: String,
    /// h2 的 Connection 必须被持续驱动，否则请求永远不完成。
    /// 持有句柄是为了在客户端被 drop 时收掉它，不留孤儿任务。
    connection: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for StatsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsClient")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl Drop for StatsClient {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

fn io_error(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, message)
}

impl StatsClient {
    /// 连接核心 api 入站（h2c，明文 HTTP/2）。
    pub async fn connect(api_addr: SocketAddr) -> Result<Self, ErrorBody> {
        let stream =
            tokio::time::timeout(CONNECT_DEADLINE, tokio::net::TcpStream::connect(api_addr))
                .await
                .map_err(|_| {
                    io_error(format!("连接 api 入站 {api_addr} 超时（{CONNECT_DEADLINE:?}）"))
                })?
                .map_err(|e| io_error(format!("连接 api 入站 {api_addr} 失败: {e}")))?;
        // 统计是小报文，禁用 Nagle 让往返更干脆。
        let _ = stream.set_nodelay(true);

        let (sender, connection) =
            tokio::time::timeout(CONNECT_DEADLINE, h2::client::handshake(stream))
                .await
                .map_err(|_| io_error(format!("与 {api_addr} 的 HTTP/2 握手超时")))?
                .map_err(|e| io_error(format!("与 {api_addr} 的 HTTP/2 握手失败: {e}")))?;

        let connection = tokio::spawn(async move {
            // 会话结束是正常事件（核心被停掉时就会发生）。这里不把它解释成错误，
            // 因为后续 `sample()` 会用自己的失败如实上报。
            let _ = connection.await;
        });

        Ok(Self {
            sender: tokio::sync::Mutex::new(sender),
            authority: api_addr.to_string(),
            connection,
        })
    }

    /// 查一次全部计数器，折算成上下行字节。
    pub async fn sample(&self) -> Result<StatsView, ErrorBody> {
        let entries = self.query().await?;
        let (uplink_bytes, downlink_bytes) = fold_traffic(&entries);
        Ok(StatsView {
            uplink_bytes,
            downlink_bytes,
            sampled_at_ms: now_ms(),
            // 说明：0 字节是**真实样本**（确实还没流量）；「没采到」是 Err，
            // 由 daemon 转成 stats: None。两者在界面上必须是两件事。
        })
    }

    async fn query(&self) -> Result<Vec<StatEntry>, ErrorBody> {
        let mut sender = self.sender.lock().await;

        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("http://{}{QUERY_STATS_PATH}", self.authority))
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .body(())
            .map_err(|e| io_error(format!("构造 StatsService 请求失败: {e}")))?;

        let (response, mut body_tx) = sender
            .send_request(request, false)
            .map_err(|e| io_error(format!("发送 StatsService 请求失败: {e}")))?;

        body_tx
            .send_data(Bytes::from(encode_grpc_frame(&encode_query_stats_request(""))), true)
            .map_err(|e| io_error(format!("发送 StatsService 请求体失败: {e}")))?;

        let response = tokio::time::timeout(SAMPLE_DEADLINE, response)
            .await
            .map_err(|_| io_error(format!("等待 StatsService 响应超时（{SAMPLE_DEADLINE:?}）")))?
            .map_err(|e| io_error(format!("等待 StatsService 响应失败: {e}")))?;

        if response.status() != http::StatusCode::OK {
            return Err(io_error(format!("StatsService 返回 HTTP {}", response.status())));
        }

        let mut body = response.into_body();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::time::timeout(SAMPLE_DEADLINE, body.data())
                .await
                .map_err(|_| io_error("读取 StatsService 响应体超时"))?;
            match chunk {
                Some(chunk) => buf.extend_from_slice(
                    &chunk.map_err(|e| io_error(format!("读取 StatsService 响应体失败: {e}")))?,
                ),
                None => break,
            }
        }

        // 出错时 gRPC 会把状态放在 trailers 里、帧通常是空的；帧解析失败会
        // 给出明确的错误，而不是一个「什么都没有」的成功。
        decode_query_stats_response(decode_grpc_frame(&buf)?)
    }
}

/// 计数器 → 上下行字节。
///
/// 只收两类：
/// * 入站，排除 `api`（我们自己的轮询 —— 见 [`API_INBOUND_TAG`]）；
/// * 出口，且 tag 以 `node-` 开头（真正的隧道出口；`direct`/`api` 不算）。
///
/// 名字不认识或 flow 不认识时**整条丢弃**：先建条目再忽略会让一个陌生名字
/// 变成值为 0 的项 ——「这个名字没被识别」看起来就像「这条链路流量是 0」。
fn fold_traffic(entries: &[StatEntry]) -> (u64, u64) {
    let mut uplink = 0u64;
    let mut downlink = 0u64;
    for entry in entries {
        let Some(parts) = parse_traffic_counter(&entry.name) else {
            continue;
        };
        let counted = match parts.direction {
            "inbound" => parts.tag != API_INBOUND_TAG,
            "outbound" => parts.tag.starts_with(NODE_OUTBOUND_PREFIX),
            _ => false,
        };
        if !counted {
            continue;
        }
        // 负值理论上不该出现；真出现时按 0，不能让 u64 回绕成天文数字。
        let value = entry.value.max(0) as u64;
        match parts.flow {
            "uplink" => uplink = uplink.saturating_add(value),
            "downlink" => downlink = downlink.saturating_add(value),
            _ => continue,
        }
    }
    (uplink, downlink)
}

/// 计数器名里解析出来的分量。格式只解析这一处 ——
/// 各写一份手切分的话，格式一变必有一处漏改，而漏改的表现是数字悄悄变 0。
struct CounterParts<'a> {
    direction: &'a str,
    tag: &'a str,
    flow: &'a str,
}

/// 解析 `inbound>>>socks>>>traffic>>>uplink` 形状的名字。
fn parse_traffic_counter(name: &str) -> Option<CounterParts<'_>> {
    let mut parts = name.split(">>>");
    let (Some(direction), Some(tag), Some("traffic"), Some(flow), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if direction != "inbound" && direction != "outbound" {
        return None;
    }
    // 空 tag 不是合法计数器：放过去会把所有「名字被切坏」的计数归到一个空名上。
    if tag.is_empty() {
        return None;
    }
    Some(CounterParts { direction, tag, flow })
}

/// 真实时钟（epoch 毫秒）。契约要求 `sampled_at_ms > 0`，因此下限取 1。
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

    fn stat(name: &str, value: i64) -> StatEntry {
        StatEntry { name: name.into(), value }
    }

    /// 方向映射：入站 downlink = 用户的下载。与契约 `StatsView` 的字段一一对应。
    #[test]
    fn downlink_is_download_and_uplink_is_upload() {
        let (up, down) = fold_traffic(&[
            stat("inbound>>>socks>>>traffic>>>downlink", 64 * 1024),
            stat("inbound>>>socks>>>traffic>>>uplink", 300),
        ]);
        assert_eq!(down, 64 * 1024);
        assert_eq!(up, 300);
    }

    /// 任务要求把 `inbound>>>socks` 与 `outbound>>>node-*` 两路加总。
    #[test]
    fn sums_socks_inbound_and_node_outbound() {
        let (up, down) = fold_traffic(&[
            stat("inbound>>>socks>>>traffic>>>downlink", 100),
            stat("outbound>>>node-abc>>>traffic>>>downlink", 100),
            stat("inbound>>>socks>>>traffic>>>uplink", 10),
            stat("outbound>>>node-abc>>>traffic>>>uplink", 12),
        ]);
        assert_eq!(down, 200);
        assert_eq!(up, 22);
    }

    /// 轮询自己的 api 入站不能算：否则空闲机器也有读数，用户会以为流量在跑。
    #[test]
    fn excludes_the_api_inbound_and_non_node_outbounds() {
        let (up, down) = fold_traffic(&[
            stat("inbound>>>api>>>traffic>>>downlink", 999),
            stat("inbound>>>api>>>traffic>>>uplink", 999),
            stat("outbound>>>direct>>>traffic>>>downlink", 500),
            stat("outbound>>>api>>>traffic>>>uplink", 500),
            stat("inbound>>>socks>>>traffic>>>downlink", 7),
        ]);
        assert_eq!(down, 7, "只有真实链路被计入");
        assert_eq!(up, 0);
    }

    #[test]
    fn unknown_flow_and_malformed_names_are_dropped_whole() {
        let (up, down) = fold_traffic(&[
            stat("inbound>>>socks>>>traffic>>>sideways", 500),
            stat("inbound>>>socks>>>traffic", 500),
            stat("inbound>>>>>traffic>>>uplink", 500),
            stat("user>>>a>>>traffic>>>uplink", 500),
            stat("outbound>>>node-a>>>traffic>>>downlink>>>extra", 500),
            stat("", 500),
        ]);
        assert_eq!((up, down), (0, 0));
    }

    #[test]
    fn negative_values_are_clamped_instead_of_wrapping() {
        let (_, down) = fold_traffic(&[stat("inbound>>>socks>>>traffic>>>downlink", -1)]);
        assert_eq!(down, 0);
    }

    #[test]
    fn sampled_at_is_a_real_nonzero_clock() {
        let ms = now_ms();
        assert!(ms > 1_700_000_000_000, "必须是 epoch 毫秒而不是计数器: {ms}");
    }

    /// 没有 api 入站可连时 `connect` 必须**如实失败**，而不是返回一个
    /// 假装已连接、到 sample 才炸的客户端。
    #[tokio::test]
    async fn connect_fails_when_nothing_listens() {
        let err = StatsClient::connect("127.0.0.1:1".parse().unwrap()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
    }
}
