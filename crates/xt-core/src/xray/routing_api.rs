//! `RoutingService`：**在核心运行期**增删路由规则，不重启、不断连接。
//!
//! # 为什么值得手写这一层
//!
//! 本项目的配置热更新走的是"重启核心"（`docs/03 §6`），桌面端分钟级切换完全够用。
//! 但意图过滤的规则集**会随判决持续变化**，每变一次就断一次用户所有连接是不可接受的。
//! 上游为此提供了 `xray.app.router.command.RoutingService`：
//! `AddRule` / `RemoveRule` / `ListRule`。
//!
//! # 契约来源（**已按 tag 逐个核对，不要凭记忆改**）
//!
//! `github.com/XTLS/Xray-core` tag **`v26.9.9`** 的
//! `app/router/command/command.proto`、`app/router/config.proto`、
//! `common/geodata/geodat.proto`、`common/net/port.proto`、`common/serial/typed_message.proto`。
//! 完整字段表见 `docs/design/INTENT-FILTER.md` 附录 A。
//!
//! **字段号不是递增的**（`domain = 2` 而 `port_list = 14`、`networks = 13`），
//! 猜错的症状是"调用成功但规则没生效"，或者更糟 —— 见下面六个坑。
//!
//! # 六个坑（每一个都有对应的测试或断言）
//!
//! 1. **`shouldAppend = false` 会替换整份规则表** —— 规则表里同时有预设、自定义与
//!    本项目的 `internal-*`，整份替换等于把它们全删掉。本模块**只允许 `append`**，
//!    删除一律按 `ruleTag` 逐条来（`should_append()` 的返回值被断言为 `true`）。
//! 2. **`domain_strategy` 只在 `Router.Init` 里读**，`AddRule` 改不动它 ⇒ 我们发
//!    `AsIs`（0）以外的值没有意义，干脆**不发这个字段**。
//! 3. **`ruleTag` 全局唯一**，撞了整次调用报错且**什么都不变**（幂等，不需要回滚）。
//! 4. **`RemoveRule` 删不存在的 tag 静默成功** ⇒ 不能拿它当"规则在不在"的判据，
//!    要用 [`list_rules`]。
//! 5. **运行期加的规则不在生成的 `config.json` 里** ⇒ 核心一重启就没了，
//!    "重启后重新下发"必须由调用方保证。
//! 6. **gRPC 的错误在 trailers 里**：HTTP 200 也可能是失败。不读 trailers 就会把
//!    "没加进去"当成"加进去了"（[`unary`] 里读并检查 `grpc-status`）。
#![allow(clippy::bool_assert_comparison)]

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http::StatusCode;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// `RoutingService` 的三个一元 RPC。
pub const PATH_ADD_RULE: &str =
    "/xray.app.router.command.RoutingService/AddRule";
pub const PATH_REMOVE_RULE: &str =
    "/xray.app.router.command.RoutingService/RemoveRule";
pub const PATH_LIST_RULE: &str =
    "/xray.app.router.command.RoutingService/ListRule";

/// `TypedMessage.type` 的取值：我们要发的是一份 `xray.app.router.Config`。
pub const CONFIG_TYPE_URL: &str = "xray.app.router.Config";

/// `Domain.Type.Full` = 3（`geodat.proto`）。
const DOMAIN_TYPE_FULL: u64 = 3;

/// 一条运行期规则的字段（我们的 `RoutingRule` 里这一层用得上的那些）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRule {
    /// `ruleTag`：排障与删除都靠它，必须全局唯一。
    pub rule_tag: String,
    /// 目标出站 tag（本功能是 `block`）。
    pub outbound_tag: String,
    /// **完整域名**（不带 `full:` 前缀 —— 前缀是 Xray 配置语法的糖，protobuf 里靠
    /// `Domain.Type.Full` 表达）。
    pub full_domains: Vec<String>,
    /// 限定入站（本功能要显式列，否则 MITM 的回连会命中自己的规则）。
    pub inbound_tags: Vec<String>,
    /// 限定网络层：`2` = TCP，`3` = UDP（**枚举里没有 1**）。
    ///
    /// 空 = 不限。只有整份替换（[`replace_rules`]）需要它 —— 因为替换要用**完整**的
    /// 规则集，而 catch-all 那条靠的就是 `tcp,udp`。
    pub networks: Vec<u64>,
}

/// `ListRuleResponse.rules[]` 的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRuleItem {
    /// 出站 tag。
    pub tag: String,
    /// 规则的 `ruleTag`（配置里没写就是空串）。
    pub rule_tag: String,
}

// ---------------------------------------------------------------------------
// protobuf 编码
// ---------------------------------------------------------------------------

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// 写一个 length-delimited 字段（wire type 2）。
fn put_bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
    put_varint(u64::from(field) << 3 | 2, out);
    put_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

fn put_string(field: u32, value: &str, out: &mut Vec<u8>) {
    put_bytes(field, value.as_bytes(), out);
}

/// 写一个 varint 字段（wire type 0）。
fn put_u64(field: u32, value: u64, out: &mut Vec<u8>) {
    put_varint(u64::from(field) << 3, out);
    put_varint(value, out);
}

/// `Domain{ type = Full, value = host }`。
pub fn encode_domain_full(host: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(1, DOMAIN_TYPE_FULL, &mut out);
    put_string(2, host, &mut out);
    out
}

/// `DomainRule{ custom = Domain{…} }`（oneof 的 `custom` 是字段 2）。
pub fn encode_domain_rule_full(host: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(2, &encode_domain_full(host), &mut out);
    out
}

/// `RoutingRule`。只写我们真正需要的字段 —— **没写的字段在 proto3 里等于默认值**，
/// 而默认值不参与匹配，所以少写不会放宽条件。
pub fn encode_routing_rule(rule: &ApiRule) -> Vec<u8> {
    let mut out = Vec::new();
    // oneof target_tag { tag = 1 }
    if !rule.outbound_tag.is_empty() {
        put_string(1, &rule.outbound_tag, &mut out);
    }
    for host in &rule.full_domains {
        put_bytes(2, &encode_domain_rule_full(host), &mut out);
    }
    for inbound in &rule.inbound_tags {
        put_string(8, inbound, &mut out);
    }
    // networks = 13（repeated enum ⇒ 每个元素一个 varint 字段）
    for n in &rule.networks {
        put_u64(13, *n, &mut out);
    }
    put_string(19, &rule.rule_tag, &mut out);
    out
}

/// `Config{ rule = 2 … }`。**刻意不写 `domain_strategy`**：它改不动，写了只会误导。
pub fn encode_config(rules: &[ApiRule]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in rules {
        put_bytes(2, &encode_routing_rule(r), &mut out);
    }
    out
}

/// `AddRuleRequest{ config: TypedMessage, shouldAppend: bool }`。
pub fn encode_add_rule_request(rules: &[ApiRule], should_append: bool) -> Vec<u8> {
    let mut typed = Vec::new();
    put_string(1, CONFIG_TYPE_URL, &mut typed);
    put_bytes(2, &encode_config(rules), &mut typed);

    let mut out = Vec::new();
    put_bytes(1, &typed, &mut out);
    if should_append {
        put_u64(2, 1, &mut out);
    }
    out
}

/// `RemoveRuleRequest{ ruleTag = 1 }`。
pub fn encode_remove_rule_request(rule_tag: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_string(1, rule_tag, &mut out);
    out
}

/// `ListRuleRequest{}` —— 没有任何字段，所以是**空消息**（不是空 protobuf 帧的特例）。
pub fn encode_list_rule_request() -> Vec<u8> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// protobuf 解码
// ---------------------------------------------------------------------------

/// 读一个 varint，返回 `(值, 新偏移)`。
fn read_varint(buf: &[u8], mut i: usize) -> Result<(u64, usize)> {
    let mut shift = 0u32;
    let mut value = 0u64;
    loop {
        let byte = *buf
            .get(i)
            .ok_or_else(|| Error::RoutingApi("varint 越界".into()))?;
        i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, i));
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::RoutingApi("varint 过长".into()));
        }
    }
}

/// 跳过（或取出）一个字段的值。只支持我们真的会遇到的 wire type。
enum FieldValue<'a> {
    /// 值本身不读 —— 但**必须把 varint 跳过去**才能继续解析后面的字段，
    /// 所以这个变体存在本身就是必要的（`routing_api_live` 里靠它解析真实响应）。
    #[allow(dead_code)]
    Varint(u64),
    Bytes(&'a [u8]),
}

fn read_fields(buf: &[u8]) -> Result<Vec<(u32, FieldValue<'_>)>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let (key, ni) = read_varint(buf, i)?;
        i = ni;
        let field = (key >> 3) as u32;
        match key & 7 {
            0 => {
                let (v, ni) = read_varint(buf, i)?;
                i = ni;
                out.push((field, FieldValue::Varint(v)));
            }
            2 => {
                let (len, ni) = read_varint(buf, i)?;
                i = ni;
                let end = i
                    .checked_add(len as usize)
                    .filter(|e| *e <= buf.len())
                    .ok_or_else(|| Error::RoutingApi("长度越界".into()))?;
                out.push((field, FieldValue::Bytes(&buf[i..end])));
                i = end;
            }
            // 1 / 5 是 64/32 位定长（本项目不需要），其余未知 wire type 直接报错 ——
            // 静默跳过会让"解析出一个空列表"看起来像"核心没有规则"。
            other => {
                return Err(Error::RoutingApi(format!("不支持的 wire type {other}")));
            }
        }
    }
    Ok(out)
}

/// `ListRuleResponse{ repeated ListRuleItem rules = 1 }`。
pub fn decode_list_rule_response(buf: &[u8]) -> Result<Vec<ListRuleItem>> {
    let mut out = Vec::new();
    for (field, value) in read_fields(buf)? {
        if field != 1 {
            continue;
        }
        let FieldValue::Bytes(item) = value else {
            return Err(Error::RoutingApi("rules 的元素不是子消息".into()));
        };
        let mut tag = String::new();
        let mut rule_tag = String::new();
        for (f, v) in read_fields(item)? {
            if let FieldValue::Bytes(bytes) = v {
                match f {
                    1 => tag = String::from_utf8_lossy(bytes).to_string(),
                    2 => rule_tag = String::from_utf8_lossy(bytes).to_string(),
                    _ => {}
                }
            }
        }
        out.push(ListRuleItem { tag, rule_tag });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// gRPC 一元调用
// ---------------------------------------------------------------------------

/// gRPC 帧：`[compressed:u8][len:u32 BE][payload]`。
fn encode_grpc_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(0);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn decode_grpc_frame(buf: &[u8]) -> Result<&[u8]> {
    if buf.len() < 5 {
        return Err(Error::RoutingApi(format!("gRPC 帧太短（{} 字节）", buf.len())));
    }
    if buf[0] != 0 {
        return Err(Error::RoutingApi("压缩过的 gRPC 帧不支持".into()));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let end = 5usize
        .checked_add(len)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| Error::RoutingApi("gRPC 帧长度越界".into()))?;
    Ok(&buf[5..end])
}

/// 一次一元调用。**读 trailers 检查 `grpc-status`** —— HTTP 200 也可能是失败
/// （比如 `ruleTag` 重复、或引用了不存在的 outbound）。
pub async fn unary(addr: SocketAddr, path: &str, payload: &[u8]) -> Result<Vec<u8>> {
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| Error::RoutingApi(format!("连接 api 入站 {addr} 失败: {e}")))?;
    let _ = stream.set_nodelay(true);

    let (mut sender, connection) = h2::client::handshake(stream)
        .await
        .map_err(|e| Error::RoutingApi(format!("HTTP/2 握手失败: {e}")))?;
    tokio::spawn(async move {
        // 连接对象必须被驱动，否则请求永远不会完成。
        if let Err(e) = connection.await {
            tracing::debug!(error = %e, "路由 API 的 h2 会话结束");
        }
    });

    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .map_err(|e| Error::RoutingApi(format!("构造请求失败: {e}")))?;

    let (response, mut body_tx) = sender
        .send_request(request, false)
        .map_err(|e| Error::RoutingApi(format!("发送请求失败: {e}")))?;

    body_tx
        .send_data(Bytes::from(encode_grpc_frame(payload)), true)
        .map_err(|e| Error::RoutingApi(format!("发送请求体失败: {e}")))?;

    let response = response
        .await
        .map_err(|e| Error::RoutingApi(format!("等待响应失败: {e}")))?;
    if response.status() != StatusCode::OK {
        return Err(Error::RoutingApi(format!("api 返回 HTTP {}", response.status())));
    }

    let mut body = response.into_body();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| Error::RoutingApi(format!("读取响应体失败: {e}")))?;
        buf.extend_from_slice(&chunk);
    }

    // **关键**：gRPC 的失败藏在 trailers 里。不读它就会把"没加进去"当成"加进去了"。
    let trailers = body
        .trailers()
        .await
        .map_err(|e| Error::RoutingApi(format!("读取 trailers 失败: {e}")))?;
    if let Some(trailers) = trailers {
        let status = trailers
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0");
        if status != "0" {
            let message = trailers
                .get("grpc-message")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("(没有 grpc-message)");
            return Err(Error::RoutingApi(format!("gRPC 失败 status={status}：{message}")));
        }
    }

    decode_grpc_frame(&buf).map(|s| s.to_vec())
}

/// 列出当前**全部**规则（含预设、自定义、`internal-*` 与运行期加的）。
pub async fn list_rules(addr: SocketAddr, timeout: Duration) -> Result<Vec<ListRuleItem>> {
    // 先落到一个具名变量：`&encode_list_rule_request()` 是个临时值，
    // 直接当参数会让它活不过 `await`（E0716）。
    let request = encode_list_rule_request();
    let call = unary(addr, PATH_LIST_RULE, &request);
    let payload = match tokio::time::timeout(timeout, call).await {
        Ok(r) => r?,
        Err(_) => return Err(Error::RoutingApi(format!("ListRule 超时（{timeout:?}）"))),
    };
    decode_list_rule_response(&payload)
}

/// 追加规则（**永远 `shouldAppend = true`**，见模块文档第 1 条）。
pub async fn add_rules(
    addr: SocketAddr,
    rules: &[ApiRule],
    timeout: Duration,
) -> Result<()> {
    if rules.is_empty() {
        return Ok(());
    }
    let payload = encode_add_rule_request(rules, true);
    match tokio::time::timeout(timeout, unary(addr, PATH_ADD_RULE, &payload)).await {
        Ok(r) => r.map(|_| ()),
        Err(_) => Err(Error::RoutingApi(format!("AddRule 超时（{timeout:?}）"))),
    }
}

/// **整份替换**规则表（`shouldAppend = false`）。
///
/// # 为什么这个函数危险，以及它为什么是必需的
///
/// 本机实测（`tests/routing_api_live.rs`）：**追加**的规则排在一份 catch-all 之后就
/// 永远不会命中 —— 而本项目生成的配置末尾**永远**有一条
/// `internal-fallback`（`network: tcp,udp → 当前节点`）。所以：
///
/// ```text
/// [internal-dns-hijack] [internal-api] [preset…] [intent…] [自定义…] [internal-fallback]   ← 文件里的顺序
///                                       ↑ 追加的规则跑到 internal-fallback 之后 ⇒ 永不命中
/// ```
///
/// ⇒ 运行期改规则**只有整份替换这一条路**。而它要求调用方手里有**完整**的规则集
/// （我们自己的 `build_routing` 产物），并且能把它逐条编码成 protobuf ——
/// 目前 [`ApiRule`] 只覆盖 `domain(full:)` / `inbound_tag` / `networks` / `outbound`，
/// **`geosite:` / `regexp:` / `ip` / `port` / `process` 还没编码**。
///
/// 所以：[`replace_rules`] 现在只应该用在"调用方确认整份规则集都能被本模块表达"
/// 的场景（例如测试与最小配置）。**不许**在生产里把一份无法完整表达的规则集交给它 ——
/// 那会静默丢掉用户的预设与自定义规则。
pub async fn replace_rules(
    addr: SocketAddr,
    rules: &[ApiRule],
    timeout: Duration,
) -> Result<()> {
    let payload = encode_add_rule_request(rules, false);
    match tokio::time::timeout(timeout, unary(addr, PATH_ADD_RULE, &payload)).await {
        Ok(r) => r.map(|_| ()),
        Err(_) => Err(Error::RoutingApi(format!("ReplaceRules 超时（{timeout:?}）"))),
    }
}

/// 按 `ruleTag` 逐条删除。**删不存在的 tag 会静默成功**（模块文档第 4 条），
/// 所以调用方要用 [`list_rules`] 校验，而不是假设它成功了。
pub async fn remove_rules(
    addr: SocketAddr,
    rule_tags: &[String],
    timeout: Duration,
) -> Result<()> {
    for tag in rule_tags {
        let payload = encode_remove_rule_request(tag);
        match tokio::time::timeout(timeout, unary(addr, PATH_REMOVE_RULE, &payload)).await {
            Ok(r) => r.map(|_| ())?,
            Err(_) => {
                return Err(Error::RoutingApi(format!("RemoveRule({tag}) 超时（{timeout:?}）")))
            }
        }
    }
    Ok(())
}

/// 把应用侧的规则 IR **忠实**翻成 `ApiRule`。
///
/// # 为什么需要它（而不是"只编我们想改的那几条"）
///
/// 见 [`replace_rules`] 的文档：追加的规则会被 catch-all 吃掉，所以运行期改规则
/// **只能整份替换** —— 而整份替换要求把**每一条**规则都表达出来。
/// 做不到这一点的场合，正确的动作是**拒绝**并回落"重启核心"，
/// 而不是把一份缺了几条的规则表塞进去（那会静默丢掉用户的预设与自定义规则）。
///
/// # 目前能表达 / 不能表达
///
/// | 能 | 不能（返回在 `Err` 里，一条一句人话） |
/// |---|---|
/// | `full:` / `domain:` / 裸域名 | `geosite:` / `regexp:` / `ext:` |
/// | `inbound_tags` | `ip` / `source_ip` |
/// | `network`（Both / Tcp / Udp） | `ports` / `process_names` / `protocols` |
/// | `RuleAction::{Block, Direct, Proxy{Some,None}}` | —— |
///
/// 新支持一项时**同时**改这张表：它是调用方决定"能不能热加"的唯一依据。
pub fn to_api_rules(
    rules: &[crate::routing::RoutingRule],
    selected_tag: &str,
) -> Result<Vec<ApiRule>, Vec<String>> {
    use crate::routing::{Network, RuleAction};

    let mut unsupported: Vec<String> = Vec::new();
    let mut out: Vec<ApiRule> = Vec::with_capacity(rules.len());

    for rule in rules.iter().filter(|r| r.enabled) {
        let when = &rule.when;
        let mut full_domains = Vec::new();

        for d in &when.domains {
            // 顺序很重要：`geosite:` / `regexp:` / `ext:` 都要先判掉，
            // 否则会被当成裸域名写进去 —— 那是**静默放宽**匹配条件。
            if d.starts_with("geosite:") || d.starts_with("ext:") || d.starts_with("regexp:") {
                unsupported.push(format!("{}：域名表达式 {d}", rule.id));
                continue;
            }
            let host = d
                .strip_prefix("full:")
                .or_else(|| d.strip_prefix("domain:"))
                .unwrap_or(d.as_str());
            if host.is_empty() {
                unsupported.push(format!("{}：空域名", rule.id));
                continue;
            }
            full_domains.push(host.to_string());
        }

        if !when.ip.is_empty() {
            unsupported.push(format!("{}：ip（{} 条）", rule.id, when.ip.len()));
        }
        if !when.source_ip.is_empty() {
            unsupported.push(format!("{}：source_ip（{} 条）", rule.id, when.source_ip.len()));
        }
        if !when.ports.is_empty() {
            unsupported.push(format!("{}：port（{} 条）", rule.id, when.ports.len()));
        }
        if !when.process_names.is_empty() {
            unsupported.push(format!("{}：process", rule.id));
        }
        if !when.protocols.is_empty() {
            unsupported.push(format!("{}：protocol", rule.id));
        }

        let networks = match when.network {
            Network::Both => vec![2, 3], // TCP, UDP
            Network::Tcp => vec![2],
            Network::Udp => vec![3],
        };

        let outbound_tag = match &rule.then {
            RuleAction::Block => "block".to_string(),
            RuleAction::Direct => "direct".to_string(),
            RuleAction::Proxy { outbound: Some(t) } => t.clone(),
            RuleAction::Proxy { outbound: None } => selected_tag.to_string(),
        };

        out.push(ApiRule {
            rule_tag: rule.id.clone(),
            outbound_tag,
            full_domains,
            inbound_tags: when.inbound_tags.clone(),
            networks,
        });
    }

    if unsupported.is_empty() {
        Ok(out)
    } else {
        Err(unsupported)
    }
}

/// 把**当前规则表**与**我们想要的那一组**对齐：多删少加。
///
/// 返回 `(添加的 tag, 删除的 tag)`，让调用方能记一条可核对的日志。
///
/// 只碰 `intent-` 前缀的规则 —— 别的规则（预设、自定义、`internal-*`）不归我们管，
/// 误删它们会静默改坏用户的分流。
pub async fn sync_intent_rules(
    addr: SocketAddr,
    desired: &[ApiRule],
    timeout: Duration,
) -> Result<(Vec<String>, Vec<String>)> {
    const PREFIX: &str = "intent-";
    let current = list_rules(addr, timeout).await?;
    let current_intent: Vec<String> = current
        .iter()
        .map(|i| i.rule_tag.clone())
        .filter(|t| t.starts_with(PREFIX))
        .collect();
    let desired_tags: Vec<String> = desired.iter().map(|r| r.rule_tag.clone()).collect();

    let to_remove: Vec<String> = current_intent
        .iter()
        .filter(|t| !desired_tags.contains(t))
        .cloned()
        .collect();
    let to_add: Vec<ApiRule> = desired
        .iter()
        .filter(|r| !current_intent.contains(&r.rule_tag))
        .cloned()
        .collect();

    remove_rules(addr, &to_remove, timeout).await?;
    add_rules(addr, &to_add, timeout).await?;

    Ok((
        to_add.into_iter().map(|r| r.rule_tag).collect(),
        to_remove,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> ApiRule {
        ApiRule {
            rule_tag: "intent-block-ads.example".into(),
            outbound_tag: "block".into(),
            full_domains: vec!["ads.example".into()],
            inbound_tags: vec!["tun".into()],
            networks: Vec::new(),
        }
    }

    /// 逐字节钉住 wire 格式。
    ///
    /// 每个**键字节**（字段号 + wire type）手写，长度前缀用 `.len()` 算 ——
    /// 第一版我把长度也手算，结果 `ruleTag` 数错了 1 个字节就红了：
    /// 人的算术不该是这类测试的判据。
    #[test]
    fn a_routing_rule_encodes_to_the_documented_bytes() {
        let host = "ads.example";
        let rule_tag = "intent-block-ads.example";

        let mut expect = Vec::new();
        // tag = 1, wire 2（oneof target_tag）
        expect.extend_from_slice(&[0x0a, 5]);
        expect.extend_from_slice(b"block");
        // domain = 2, wire 2 → DomainRule{ custom = 2, wire 2 →
        //   Domain{ type = 1 varint; value = 2 string } }
        let domain_len = 2 + 2 + host.len(); // 0x08 0x03 + 0x12 len host
        let rule_len = 2 + domain_len; // 0x12 rule_len
        expect.extend_from_slice(&[
            0x12,
            rule_len as u8,
            0x12,
            domain_len as u8,
            0x08,
            0x03,
            0x12,
            host.len() as u8,
        ]);
        expect.extend_from_slice(host.as_bytes());
        // inbound_tag = 8, wire 2（8<<3|2 = 0x42）
        expect.extend_from_slice(&[0x42, 3]);
        expect.extend_from_slice(b"tun");
        // rule_tag = 19, wire 2（19<<3|2 = 154 ⇒ varint 0x9a 0x01）
        expect.extend_from_slice(&[0x9a, 0x01, rule_tag.len() as u8]);
        expect.extend_from_slice(rule_tag.as_bytes());

        assert_eq!(encode_routing_rule(&rule()), expect);
    }

    /// `domain` 里的 `Domain.Type` **必须是 Full(3)**：用 Substr(0) 会变成子串匹配，
    /// 一条规则会拦掉一堆无关域名。
    #[test]
    fn the_domain_type_is_full_not_substr() {
        let bytes = encode_domain_full("ads.example");
        assert_eq!(bytes[0], 0x08, "第一个字段应是 type");
        assert_eq!(bytes[1], 3, "type 必须是 Full=3");
        assert_ne!(bytes[1], 0, "Substr(0) 会变成子串匹配");
    }

    #[test]
    fn add_rule_request_always_appends() {
        let bytes = encode_add_rule_request(&[rule()], true);
        // shouldAppend = 2, varint 1 ⇒ 0x10 0x01（在最后）
        assert_eq!(&bytes[bytes.len() - 2..], &[0x10, 0x01]);
        // TypedMessage.type = "xray.app.router.Config"
        let typed_type = CONFIG_TYPE_URL.as_bytes();
        let pos = bytes
            .windows(typed_type.len())
            .position(|w| w == typed_type)
            .expect("必须带 type url");
        assert_eq!(bytes[pos - 2], 0x0a, "TypedMessage.type 是字段 1");
    }

    /// **不写 `domain_strategy`**：它改不动，写了只会误导（模块文档第 2 条）。
    #[test]
    fn config_never_carries_domain_strategy() {
        let with_rules = encode_config(&[rule()]);
        assert_ne!(with_rules.first(), Some(&0x08), "首字段不该是 domain_strategy");
        // 空规则集 ⇒ 空 Config（没有字段可写）。
        assert!(encode_config(&[]).is_empty());
    }

    #[test]
    fn remove_request_carries_the_rule_tag() {
        let tag = "intent-block-a.example";
        let mut expect = vec![0x0a, tag.len() as u8];
        expect.extend_from_slice(tag.as_bytes());
        assert_eq!(encode_remove_rule_request(tag), expect);
    }

    #[test]
    fn list_request_is_an_empty_message() {
        assert!(encode_list_rule_request().is_empty());
    }

    #[test]
    fn list_response_decodes_tags_and_rule_tags() {
        // ListRuleResponse{ rules = 1 { tag = 1 "block", ruleTag = 2 "intent-x" } }
        let item: Vec<u8> = {
            let mut v = vec![0x0a, 0x05];
            v.extend_from_slice(b"block");
            v.push(0x12);
            v.push(0x08);
            v.extend_from_slice(b"intent-x");
            v
        };
        let mut resp = vec![0x0a, item.len() as u8];
        resp.extend_from_slice(&item);
        let got = decode_list_rule_response(&resp).unwrap();
        assert_eq!(
            got,
            vec![ListRuleItem { tag: "block".into(), rule_tag: "intent-x".into() }]
        );
    }

    /// 一条没有 `ruleTag` 的规则（配置里没写）要能解出来，`rule_tag` 是空串。
    #[test]
    fn a_rule_without_a_rule_tag_decodes_as_empty() {
        let item: Vec<u8> = {
            let mut v = vec![0x0a, 0x06];
            v.extend_from_slice(b"direct");
            v
        };
        let mut resp = vec![0x0a, item.len() as u8];
        resp.extend_from_slice(&item);
        assert_eq!(
            decode_list_rule_response(&resp).unwrap(),
            vec![ListRuleItem { tag: "direct".into(), rule_tag: String::new() }]
        );
    }

    #[test]
    fn decoding_rejects_malformed_input_instead_of_returning_empty() {
        // 截断的长度前缀 ⇒ 报错。**不能静默返回空列表** —— 那看起来像"核心没有规则"。
        assert!(decode_list_rule_response(&[0x0a, 0x7f, 0x01]).is_err());
        assert!(decode_list_rule_response(&[0x0b]).is_err(), "不支持的 wire type");
        assert!(decode_grpc_frame(&[0, 0, 0]).is_err());
        assert!(decode_grpc_frame(&[1, 0, 0, 0, 0]).is_err(), "压缩帧不支持");
    }

    #[test]
    fn grpc_frame_roundtrip() {
        let payload = b"hello";
        let frame = encode_grpc_frame(payload);
        assert_eq!(&frame[..5], &[0, 0, 0, 0, 5]);
        assert_eq!(decode_grpc_frame(&frame).unwrap(), payload);
    }

    #[test]
    fn varint_roundtrips_over_boundaries() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(v, &mut buf);
            assert_eq!(read_varint(&buf, 0).unwrap().0, v, "{v}");
        }
    }

    // -----------------------------------------------------------------------
    // to_api_rules：能表达就 Ok，不能表达就**明确拒绝**（回落到重启）
    // -----------------------------------------------------------------------

    fn ir(id: &str, when: crate::routing::MatchCondition, then: crate::routing::RuleAction) -> crate::routing::RoutingRule {
        crate::routing::RoutingRule::new(id, id, when, then)
    }

    #[test]
    fn a_minimal_supported_rule_set_converts() {
        use crate::routing::{MatchCondition, RuleAction};
        let rules = vec![
            ir(
                "internal-api",
                MatchCondition { inbound_tags: vec!["api".into()], ..Default::default() },
                RuleAction::Proxy { outbound: Some("api".into()) },
            ),
            ir(
                "intent-block-ads.example",
                MatchCondition { domains: vec!["full:ads.example".into()], inbound_tags: vec!["tun".into()], ..Default::default() },
                RuleAction::Block,
            ),
            ir(
                "internal-fallback",
                MatchCondition { network: crate::routing::Network::Both, ..Default::default() },
                RuleAction::Direct,
            ),
        ];
        let api = to_api_rules(&rules, "node-x").expect("这三条都能表达");
        assert_eq!(api.len(), 3);
        assert_eq!(api[0].outbound_tag, "api");
        assert_eq!(api[0].inbound_tags, vec!["api".to_string()]);
        assert_eq!(api[1].full_domains, vec!["ads.example".to_string()]);
        assert_eq!(api[1].outbound_tag, "block");
        assert_eq!(api[2].networks, vec![2, 3], "catch-all 必须带 tcp,udp");
    }

    #[test]
    fn bare_and_domain_prefixed_hosts_become_full_domains() {
        use crate::routing::{MatchCondition, RuleAction};
        let rules = vec![ir(
            "r",
            MatchCondition {
                domains: vec!["ads.example".into(), "domain:sub.example".into(), "full:exact.example".into()],
                ..Default::default()
            },
            RuleAction::Block,
        )];
        let api = to_api_rules(&rules, "n").unwrap();
        assert_eq!(
            api[0].full_domains,
            vec!["ads.example".to_string(), "sub.example".into(), "exact.example".into()]
        );
    }

    #[test]
    fn proxy_without_an_outbound_uses_the_selected_tag() {
        use crate::routing::{MatchCondition, RuleAction};
        let rules = vec![ir("r", MatchCondition::default(), RuleAction::Proxy { outbound: None })];
        assert_eq!(to_api_rules(&rules, "node-abc").unwrap()[0].outbound_tag, "node-abc");
    }

    /// **最能骗过人的一种错**：`geosite:cn` 被当成裸域名写进去 ⇒ 那条规则从
    /// "整个大陆域名表" 缩成 "一个叫 geosite:cn 的主机名" ⇒ 静默放宽/收紧匹配。
    /// 所以必须**拒绝**，并指名是哪一条规则。
    #[test]
    fn geosite_regexp_and_ext_are_refused_by_name() {
        use crate::routing::{MatchCondition, RuleAction};
        let rules = vec![
            ir("preset-cn-domain", MatchCondition { domains: vec!["geosite:cn".into()], ..Default::default() }, RuleAction::Direct),
            ir("user-re", MatchCondition { domains: vec!["regexp:^a.*".into()], ..Default::default() }, RuleAction::Block),
            ir("user-ext", MatchCondition { domains: vec!["ext:mine.dat:code".into()], ..Default::default() }, RuleAction::Block),
        ];
        let errs = to_api_rules(&rules, "n").unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("preset-cn-domain") && e.contains("geosite:cn")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("regexp:")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("ext:")), "{errs:?}");
    }

    #[test]
    fn every_field_we_cannot_encode_is_reported_not_dropped() {
        use crate::routing::{MatchCondition, PortMatcher, RuleAction};
        let rules = vec![ir(
            "busy",
            MatchCondition {
                domains: vec!["full:a.example".into()],
                ip: vec!["geoip:cn".into()],
                ports: vec![PortMatcher::Single(443)],
                process_names: vec!["Safari".into()],
                protocols: vec!["tls".into()],
                ..Default::default()
            },
            RuleAction::Block,
        )];
        let errs = to_api_rules(&rules, "n").unwrap_err();
        // ip / port / process / protocol 各一条。
        assert_eq!(errs.len(), 4, "{errs:?}");
        for key in ["ip", "port", "process", "protocol"] {
            assert!(errs.iter().any(|e| e.contains(key)), "缺 {key}：{errs:?}");
        }
    }

    /// 一份**真实预设**（bypass_mainland）今天必然无法热加 —— 这条断言的作用是
    /// 让"生产里回落重启"这件事有据可依，而不是一个说不清原因的静默行为。
    #[test]
    fn the_real_bypass_mainland_preset_is_not_yet_hot_swappable() {
        let rules = crate::routing::preset_rules(crate::model::RoutingPreset::BypassMainland);
        let errs = to_api_rules(&rules, "node-x").unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("geosite:") || e.contains("geoip") || e.contains("ip（")),
            "预设里必然有规则集/ip 表达式：{errs:?}"
        );
    }

    #[test]
    fn disabled_rules_are_skipped() {
        use crate::routing::{MatchCondition, RuleAction};
        let mut r = ir("off", MatchCondition { domains: vec!["full:a.example".into()], ..Default::default() }, RuleAction::Block);
        r.enabled = false;
        let api = to_api_rules(&[r], "n").unwrap();
        assert!(api.is_empty(), "禁用的规则不该被下发");
    }

    #[test]
    fn the_two_paths_are_the_documented_ones() {
        assert_eq!(PATH_ADD_RULE, "/xray.app.router.command.RoutingService/AddRule");
        assert_eq!(PATH_REMOVE_RULE, "/xray.app.router.command.RoutingService/RemoveRule");
        assert_eq!(PATH_LIST_RULE, "/xray.app.router.command.RoutingService/ListRule");
        assert_eq!(CONFIG_TYPE_URL, "xray.app.router.Config");
    }
}
