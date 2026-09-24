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

/// 域名匹配项（对应 `DomainRule` 的两种 oneof 分支与 `Domain.Type` 的四种取值）。
///
/// **一定要按 Xray 的语义分型**：`full:` 是精确匹配，裸域名与 `domain:` 是
/// **子域也匹配**。上一版把两者都编成 `Full(3)` —— 那会把"子域匹配"静默收紧成
/// "精确匹配"，是典型的"看起来成功、匹配范围却变了"。有一条测试专门钉这个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiDomain {
    /// `full:` —— 精确匹配（`Domain.Type.Full = 3`）。
    Full(String),
    /// 裸域名或 `domain:` —— 子域也匹配（`Domain.Type.Domain = 2`）。
    Domain(String),
    /// `regexp:` —— 正则（`Domain.Type.Regex = 1`）。
    Regex(String),
    /// `geosite:code` / `ext:file:code`（`DomainRule.geosite`）。
    GeoSite { file: String, code: String, attrs: String },
}

/// IP 匹配项（`IPRule` 的两种 oneof 分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiIp {
    /// `geoip:cn` / `ext:file:code`（`IPRule.geoip`）。
    GeoIp { file: String, code: String, reverse: bool },
    /// `1.2.3.0/24`（`IPRule.custom = CIDRRule{ cidr }`）。
    Cidr { ip: Vec<u8>, prefix: u32 },
}

/// 一条运行期规则（`ApiRule`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRule {
    /// `ruleTag`：排障与删除都靠它，必须全局唯一。
    pub rule_tag: String,
    /// 目标出站 tag（本功能是 `block`）。
    pub outbound_tag: String,
    pub domains: Vec<ApiDomain>,
    pub ip: Vec<ApiIp>,
    /// `port`：`(from, to)`，闭区间。单个端口是 `(p, p)`。
    pub ports: Vec<(u32, u32)>,
    pub source_ip: Vec<ApiIp>,
    /// 限定入站（本功能要显式列，否则 MITM 的回连会命中自己的规则）。
    pub inbound_tags: Vec<String>,
    /// 限定网络层：`2` = TCP，`3` = UDP（**枚举里没有 1**）。
    pub networks: Vec<u64>,
    pub process_names: Vec<String>,
    pub protocols: Vec<String>,
}

impl ApiRule {
    /// 只带"目标出站 + 规则标识"的最小构造（测试与简单场景用）。
    pub fn new(rule_tag: impl Into<String>, outbound_tag: impl Into<String>) -> Self {
        Self {
            rule_tag: rule_tag.into(),
            outbound_tag: outbound_tag.into(),
            domains: Vec::new(),
            ip: Vec::new(),
            ports: Vec::new(),
            source_ip: Vec::new(),
            inbound_tags: Vec::new(),
            networks: Vec::new(),
            process_names: Vec::new(),
            protocols: Vec::new(),
        }
    }
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

/// `Domain{ type, value }`。`type` 必须是 1/2/3（`Substr=0` 是子串匹配，我们不用）。
pub fn encode_domain(domain_type: u64, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(1, domain_type, &mut out);
    put_string(2, value, &mut out);
    out
}

/// `full:` 精确匹配的 `Domain`（`Domain.Type.Full = 3`）。
///
/// 保留这个薄封装是因为它是最常用的一种，而且**误用 `Substr(0)` 的后果特别隐蔽**
/// （一条规则会变成子串匹配、拦掉一堆无关域名），所以有一条专测盯着它。
pub fn encode_domain_full(host: &str) -> Vec<u8> {
    encode_domain(3, host)
}

/// `DomainRule`（oneof：`geosite = 1` / `custom = 2`）。
pub fn encode_domain_rule(domain: &ApiDomain) -> Vec<u8> {
    let mut out = Vec::new();
    match domain {
        ApiDomain::Full(v) => put_bytes(2, &encode_domain(3, v), &mut out),
        ApiDomain::Domain(v) => put_bytes(2, &encode_domain(2, v), &mut out),
        ApiDomain::Regex(v) => put_bytes(2, &encode_domain(1, v), &mut out),
        ApiDomain::GeoSite { file, code, attrs } => {
            let mut g = Vec::new();
            if !file.is_empty() {
                put_string(1, file, &mut g);
            }
            put_string(2, code, &mut g);
            if !attrs.is_empty() {
                put_string(3, attrs, &mut g);
            }
            put_bytes(1, &g, &mut out);
        }
    }
    out
}

/// `IPRule`（oneof：`geoip = 1` / `custom = 2`）。
pub fn encode_ip_rule(ip: &ApiIp) -> Vec<u8> {
    let mut out = Vec::new();
    match ip {
        ApiIp::GeoIp { file, code, reverse } => {
            let mut g = Vec::new();
            if !file.is_empty() {
                put_string(1, file, &mut g);
            }
            put_string(2, code, &mut g);
            if *reverse {
                put_u64(3, 1, &mut g);
            }
            put_bytes(1, &g, &mut out);
        }
        ApiIp::Cidr { ip, prefix } => {
            let mut c = Vec::new();
            put_bytes(1, ip, &mut c);
            put_u64(2, u64::from(*prefix), &mut c);
            let mut rule = Vec::new();
            put_bytes(1, &c, &mut rule);
            put_bytes(2, &rule, &mut out);
        }
    }
    out
}

/// `PortList{ repeated PortRange range = 1 }`。
fn encode_port_list(ports: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (from, to) in ports {
        let mut r = Vec::new();
        put_u64(1, u64::from(*from), &mut r);
        put_u64(2, u64::from(*to), &mut r);
        put_bytes(1, &r, &mut out);
    }
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
    for d in &rule.domains {
        put_bytes(2, &encode_domain_rule(d), &mut out);
    }
    for p in &rule.process_names {
        put_string(21, p, &mut out);
    }
    for p in &rule.protocols {
        put_string(9, p, &mut out);
    }
    for i in &rule.ip {
        put_bytes(10, &encode_ip_rule(i), &mut out);
    }
    for i in &rule.source_ip {
        put_bytes(11, &encode_ip_rule(i), &mut out);
    }
    for inbound in &rule.inbound_tags {
        put_string(8, inbound, &mut out);
    }
    // networks = 13（repeated enum ⇒ 每个元素一个 varint 字段）
    for n in &rule.networks {
        put_u64(13, *n, &mut out);
    }
    if !rule.ports.is_empty() {
        put_bytes(14, &encode_port_list(&rule.ports), &mut out);
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
/// 表达不了就**拒绝**（返回 `Err`），由调用方回落"重启核心"，
/// 而不是把一份缺了几条的规则表塞进去（那会静默丢掉用户的预设与自定义规则）。
///
/// # 覆盖范围
///
/// `MatchCondition` 的**全部字段**都能表达：域名（`full:` / `domain:` / 裸域名 /
/// `regexp:` / `geosite:` / `ext:`）、`ip`、`source_ip`、`ports`、
/// `inbound_tags`、`network`、`process_names`、`protocols`。
///
/// `Err` 只剩**输入本身不合法**这一种：空域名、认不出的 CIDR、
/// 解析不了的端口表达式。此时同样拒绝，因为吞掉它就等于放宽/收紧匹配。
pub fn to_api_rules(
    rules: &[crate::routing::RoutingRule],
    selected_tag: &str,
) -> Result<Vec<ApiRule>, Vec<String>> {
    use crate::routing::{Network, RuleAction};

    let mut problems: Vec<String> = Vec::new();
    let mut out: Vec<ApiRule> = Vec::with_capacity(rules.len());

    for rule in rules.iter().filter(|r| r.enabled) {
        let when = &rule.when;

        let mut domains = Vec::new();
        for d in &when.domains {
            match parse_domain(d) {
                Ok(x) => domains.push(x),
                Err(e) => problems.push(format!("{}：{e}", rule.id)),
            }
        }
        let mut ip = Vec::new();
        for i in &when.ip {
            match parse_ip(i) {
                Ok(x) => ip.push(x),
                Err(e) => problems.push(format!("{}：{e}", rule.id)),
            }
        }
        // `source_ip` 已经是结构化的 `Cidr`，交给同一个解析器（它实现 Display）。
        let mut source_ip = Vec::new();
        for c in &when.source_ip {
            match parse_ip(&c.to_string()) {
                Ok(x) => source_ip.push(x),
                Err(e) => problems.push(format!("{}：source_ip {e}", rule.id)),
            }
        }
        let mut ports = Vec::new();
        if !when.ports.is_empty() {
            let text = when.ports.iter().map(|p| p.as_xray()).collect::<Vec<_>>().join(",");
            match parse_ports(&text) {
                Ok(x) => ports = x,
                Err(e) => problems.push(format!("{}：port {e}", rule.id)),
            }
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
            domains,
            ip,
            ports,
            source_ip,
            inbound_tags: when.inbound_tags.clone(),
            networks,
            process_names: when.process_names.clone(),
            protocols: when.protocols.clone(),
        });
    }

    if problems.is_empty() {
        Ok(out)
    } else {
        Err(problems)
    }
}

/// 域名表达式 → [`ApiDomain`]。
///
/// **前缀决定匹配语义**，不能一律当精确匹配：
/// * `full:` 精确；`domain:` 与**裸域名**都是"子域也匹配"（Xray 的既有语义）；
/// * `geosite:` / `ext:` 是规则集；`regexp:` 是正则。
pub fn parse_domain(raw: &str) -> Result<ApiDomain, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("空域名表达式".into());
    }
    if let Some(code) = raw.strip_prefix("geosite:") {
        let (code, attrs) = split_attrs(code);
        if code.is_empty() {
            return Err("geosite: 后面没有类别名".into());
        }
        return Ok(ApiDomain::GeoSite { file: String::new(), code, attrs });
    }
    if let Some(rest) = raw.strip_prefix("ext:") {
        let (file, code) = rest
            .split_once(':')
            .ok_or_else(|| format!("ext: 需要 file:code 形式（现在是 {raw}）"))?;
        let (code, attrs) = split_attrs(code);
        if file.is_empty() || code.is_empty() {
            return Err(format!("ext: 的 file 或 code 为空（{raw}）"));
        }
        return Ok(ApiDomain::GeoSite { file: file.to_string(), code, attrs });
    }
    if let Some(v) = raw.strip_prefix("regexp:") {
        if v.is_empty() {
            return Err("regexp: 后面没有表达式".into());
        }
        return Ok(ApiDomain::Regex(v.to_string()));
    }
    if let Some(v) = raw.strip_prefix("full:") {
        if v.is_empty() {
            return Err("full: 后面没有域名".into());
        }
        return Ok(ApiDomain::Full(v.to_string()));
    }
    if let Some(v) = raw.strip_prefix("domain:") {
        if v.is_empty() {
            return Err("domain: 后面没有域名".into());
        }
        return Ok(ApiDomain::Domain(v.to_string()));
    }
    Ok(ApiDomain::Domain(raw.to_string()))
}

/// `code@attr1@attr2` → `(code, "attr1,attr2")`（geosite 的属性过滤）。
fn split_attrs(raw: &str) -> (String, String) {
    match raw.split_once('@') {
        Some((code, attrs)) => (code.to_string(), attrs.replace('@', ",")),
        None => (raw.to_string(), String::new()),
    }
}

/// IP 表达式 → [`ApiIp`]。
pub fn parse_ip(raw: &str) -> Result<ApiIp, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("空 IP 表达式".into());
    }
    if let Some(code) = raw.strip_prefix("geoip:") {
        if code.is_empty() {
            return Err("geoip: 后面没有国家码".into());
        }
        return Ok(ApiIp::GeoIp { file: String::new(), code: code.to_string(), reverse: false });
    }
    if let Some(rest) = raw.strip_prefix("ext:") {
        let (file, code) = rest
            .split_once(':')
            .ok_or_else(|| format!("ext: 需要 file:code 形式（现在是 {raw}）"))?;
        if file.is_empty() || code.is_empty() {
            return Err(format!("ext: 的 file 或 code 为空（{raw}）"));
        }
        return Ok(ApiIp::GeoIp { file: file.to_string(), code: code.to_string(), reverse: false });
    }
    // CIDR 或裸地址。**只接受 IPv4/IPv6 字面量** —— 认不出来就报错，不猜。
    let (addr_text, prefix) = match raw.split_once('/') {
        Some((a, p)) => {
            let prefix: u32 = p.parse().map_err(|_| format!("前缀不是数字（{raw}）"))?;
            (a, Some(prefix))
        }
        None => (raw, None),
    };
    let addr: std::net::IpAddr = addr_text
        .parse()
        .map_err(|_| format!("认不出这个 IP/CIDR：{raw}"))?;
    let (bytes, max) = match addr {
        std::net::IpAddr::V4(v4) => (v4.octets().to_vec(), 32u32),
        std::net::IpAddr::V6(v6) => (v6.octets().to_vec(), 128u32),
    };
    let prefix = prefix.unwrap_or(max);
    if prefix > max {
        return Err(format!("前缀 {prefix} 超过 {max}（{raw}）"));
    }
    Ok(ApiIp::Cidr { ip: bytes, prefix })
}

/// `"80,443"` / `"1000-2000"` → `[(from, to)]`。闭区间；单端口是 `(p, p)`。
pub fn parse_ports(raw: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let item = match part.split_once('-') {
            Some((a, b)) => {
                let from: u32 = a.trim().parse().map_err(|_| format!("端口不是数字：{part}"))?;
                let to: u32 = b.trim().parse().map_err(|_| format!("端口不是数字：{part}"))?;
                (from, to)
            }
            None => {
                let p: u32 = part.parse().map_err(|_| format!("端口不是数字：{part}"))?;
                (p, p)
            }
        };
        if item.0 > item.1 || item.1 > 65535 {
            return Err(format!("端口区间不合法：{part}"));
        }
        out.push(item);
    }
    if out.is_empty() {
        return Err(format!("端口表达式为空：{raw}"));
    }
    Ok(out)
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
            domains: vec![ApiDomain::Full("ads.example".into())],
            inbound_tags: vec!["tun".into()],
            ..ApiRule::new("", "")
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
        // 这条用例覆盖的是 `Domain.Type.Full = 3` 的那条分支（精确匹配）。

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
    fn a_minimal_rule_set_converts() {
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
        assert_eq!(api[1].domains, vec![ApiDomain::Full("ads.example".into())]);
        assert_eq!(api[1].outbound_tag, "block");
        assert_eq!(api[2].networks, vec![2, 3], "catch-all 必须带 tcp,udp");
    }

    /// **上一版这里有个真 bug**：裸域名与 `domain:` 都被编成 `Full(3)`，
    /// 而 Xray 的语义是"子域也匹配"（`Domain.Type.Domain = 2`）。
    /// 那会把匹配范围静默收紧 —— 用户规则少命中一批域名，却没有任何报错。
    #[test]
    fn bare_and_domain_prefixed_hosts_keep_subdomain_semantics() {
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
            api[0].domains,
            vec![
                ApiDomain::Domain("ads.example".into()),   // 裸域名 = 子域匹配
                ApiDomain::Domain("sub.example".into()),   // domain: = 子域匹配
                ApiDomain::Full("exact.example".into()),   // full: = 精确
            ]
        );
        // 字节层面也确认一次：type 字段（`08 02`）与 Full 的（`08 03`）不同。
        let domain_bytes = encode_domain_rule(&ApiDomain::Domain("a.example".into()));
        let full_bytes = encode_domain_rule(&ApiDomain::Full("a.example".into()));
        assert_ne!(domain_bytes, full_bytes, "Domain 与 Full 编出来的字节必须不同");
        // 内层 `Domain` 消息的第一个字段就是 type：`08 02` / `08 03`。
        assert!(
            domain_bytes.windows(2).any(|w| w == [0x08, 0x02]),
            "Domain 分支应当是 type=2（子域匹配）：{domain_bytes:?}"
        );
        assert!(
            full_bytes.windows(2).any(|w| w == [0x08, 0x03]),
            "Full 分支应当是 type=3（精确匹配）：{full_bytes:?}"
        );
    }

    /// `geosite:` / `ext:` / `regexp:` 各自编成**对应的那种**规则，不能混。
    /// 上一版把它们整个拒绝（那时确实表达不了）；现在能表达了，
    /// 但"编成什么类型"仍然是最容易错的地方 —— 所以逐项断言。
    #[test]
    fn geosite_ext_and_regexp_are_encoded_by_kind() {
        use crate::routing::{MatchCondition, RuleAction};
        let rules = vec![ir(
            "r",
            MatchCondition {
                domains: vec![
                    "geosite:cn".into(),
                    "geosite:category-ads-all@cn".into(),
                    "ext:mine.dat:mycode".into(),
                    "regexp:^a.*bz$".into(),
                ],
                ..Default::default()
            },
            RuleAction::Block,
        )];
        let api = to_api_rules(&rules, "n").unwrap();
        assert_eq!(
            api[0].domains,
            vec![
                ApiDomain::GeoSite { file: String::new(), code: "cn".into(), attrs: String::new() },
                ApiDomain::GeoSite {
                    file: String::new(),
                    code: "category-ads-all".into(),
                    attrs: "cn".into(),
                },
                ApiDomain::GeoSite { file: "mine.dat".into(), code: "mycode".into(), attrs: String::new() },
                ApiDomain::Regex("^a.*bz$".into()),
            ]
        );
    }

    /// `MatchCondition` 里每个字段都要被编出来（漏一个就是静默放宽匹配）。
    #[test]
    fn every_match_field_is_encoded() {
        use crate::routing::{MatchCondition, PortMatcher, RuleAction};
        let rules = vec![ir(
            "busy",
            MatchCondition {
                domains: vec!["full:a.example".into()],
                ip: vec!["geoip:cn".into(), "10.0.0.0/8".into()],
                ports: vec![PortMatcher::Raw("80,443,1000-2000".into())],
                process_names: vec!["Safari".into()],
                protocols: vec!["tls".into()],
                inbound_tags: vec!["tun".into()],
                network: crate::routing::Network::Tcp,
                ..Default::default()
            },
            RuleAction::Block,
        )];
        let api = to_api_rules(&rules, "n").unwrap();
        let r = &api[0];
        assert_eq!(r.domains.len(), 1);
        assert_eq!(
            r.ip,
            vec![
                ApiIp::GeoIp { file: String::new(), code: "cn".into(), reverse: false },
                ApiIp::Cidr { ip: vec![10, 0, 0, 0], prefix: 8 },
            ]
        );
        assert_eq!(r.ports, vec![(80, 80), (443, 443), (1000, 2000)]);
        assert_eq!(r.process_names, vec!["Safari".to_string()]);
        assert_eq!(r.protocols, vec!["tls".to_string()]);
        assert_eq!(r.inbound_tags, vec!["tun".to_string()]);
        assert_eq!(r.networks, vec![2], "Tcp ⇒ 只有 2");

        // 字节里确实出现了 port_list(14) / ip(10) / process(21) / protocol(9)。
        let bytes = encode_routing_rule(r);
        for (field, hint) in [(14u8, "port_list"), (10, "ip"), (21, "process"), (9, "protocol")] {
            let key = field << 3 | 2;
            assert!(
                bytes.contains(&key),
                "{hint}（字段 {field}，键 0x{key:02x}）没被编进去"
            );
        }
    }

    /// 真实预设现在**可以**热加了 —— 这条断言的作用是：一旦有人把某类编码弄丢，
    /// 它会立刻红，而不是等到生产上"热加悄悄少了几条规则"。
    #[test]
    fn the_real_bypass_mainland_preset_is_hot_swappable() {
        let rules = crate::routing::preset_rules(crate::model::RoutingPreset::BypassMainland);
        let api = to_api_rules(&rules, "node-x").expect("真实预设必须能整份编码");
        assert_eq!(api.len(), rules.len());
        // 预设里必然有 geosite: 与 geoip:；它们要出现在对应的分支里。
        assert!(
            api.iter().flat_map(|r| &r.domains).any(|d| matches!(d, ApiDomain::GeoSite { .. })),
            "geosite 规则集没被编出来"
        );
        assert!(
            api.iter().flat_map(|r| &r.ip).any(|i| matches!(i, ApiIp::GeoIp { .. })),
            "geoip 规则集没被编出来"
        );
    }

    #[test]
    fn malformed_values_are_refused_with_the_offending_text() {
        use crate::routing::{MatchCondition, PortMatcher, RuleAction};
        let cases: Vec<(MatchCondition, &str)> = vec![
            (MatchCondition { domains: vec!["".into()], ..Default::default() }, "空域名"),
            (MatchCondition { domains: vec!["geosite:".into()], ..Default::default() }, "类别名"),
            (
                MatchCondition { domains: vec!["ext:nocolon".into()], ..Default::default() },
                "file:code",
            ),
            (MatchCondition { ip: vec!["geoip:".into()], ..Default::default() }, "国家码"),
            (MatchCondition { ip: vec!["not-an-ip".into()], ..Default::default() }, "认不出"),
            (MatchCondition { ip: vec!["10.0.0.0/99".into()], ..Default::default() }, "前缀"),
            (
                MatchCondition { ports: vec![PortMatcher::Raw("70000".into())], ..Default::default() },
                "端口区间",
            ),
        ];
        for (when, needle) in cases {
            let rules = vec![ir("bad", when.clone(), RuleAction::Block)];
            let errs = to_api_rules(&rules, "n")
                .expect_err(&format!("{when:?} 应当被拒绝"));
            assert!(
                errs.iter().any(|e| e.contains(needle)),
                "{needle} 没出现在错误里：{errs:?}"
            );
        }
    }

    #[test]
    fn unsupported_is_now_empty_but_disabled_rules_are_still_skipped() {
        use crate::routing::{MatchCondition, RuleAction};
        let mut off = ir("off", MatchCondition { domains: vec!["full:a.example".into()], ..Default::default() }, RuleAction::Block);
        off.enabled = false;
        assert!(to_api_rules(&[off], "n").unwrap().is_empty());
    }

    #[test]
    fn the_two_paths_are_the_documented_ones() {
        assert_eq!(PATH_ADD_RULE, "/xray.app.router.command.RoutingService/AddRule");
        assert_eq!(PATH_REMOVE_RULE, "/xray.app.router.command.RoutingService/RemoveRule");
        assert_eq!(PATH_LIST_RULE, "/xray.app.router.command.RoutingService/ListRule");
        assert_eq!(CONFIG_TYPE_URL, "xray.app.router.Config");
    }
}
