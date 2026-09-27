//! MITM「**只观察、不改写**」模式：拿到真实内容、写一条摘要、**一个字节都不动**。
//!
//! # 这个模式解决什么
//!
//! 用户的原话是「你可以把 tcp 的内容拿出呀」—— 我们想拿真实响应体来判断
//! "这是不是广告"。而在这一步之前，`xt-mitm` 的代理循环**恰好挡住了最要紧的那部分流量**
//! （带 body 的 POST、chunked 响应），所以看到的永远是那几类规整的 GET。
//!
//! 观察模式把这些内容拿回来，只做三件事：
//!
//! 1. **逐字节转发**（请求体与响应体都不重编码、不改 header 语义）；
//! 2. 每条交换写一条**摘要**（`tracing::info!`）—— host / method / path / status /
//!    content-type / body 字节数 / **标记词命中计数** / body **短哈希**；
//! 3. 可选的**显式** body 落盘（默认绝不落盘，见下）。
//!
//! # 默认关闭，按域名 opt-in
//!
//! [`ObserveConfig::default()`] 是 `enabled: false`、`hosts: []`。即使调用方
//! 起了一个带观察者的代理，**没有点名任何域名也一条摘要都不会写**。
//! 生产里只有用户逐域点名的那几张小名单会被观察。
//!
//! # 隐私取舍（具体到字段）
//!
//! * **完整 body 默认绝不落盘**；要留必须是显式的 [`ObserveConfig::capture_body_dir`]，
//!   而且**只接受绝对路径**（拒绝相对路径，免得手滑写进仓库），落盘文件权限在 Unix 上是 `0600`。
//! * `path` **去掉 `?query` 与 `#fragment`**，并截断到 [`MAX_PATH_CHARS`] 字符 ——
//!   查询串里经常带 token/uid/邮箱。
//! * body 只留**16 位十六进制短哈希**（FNV-1a 64 位），用来判断"同一条响应被重复请求"。
//!   它是内容指纹，**不是密码学摘要**，不能拿来做安全用途。
//! * 摘要里保留 `host`：不记域名的话观察结果没有可执行性（"哪个站点有广告标记"才是结论）。
//!
//! # 诚实边界：这个模式**看不到**什么
//!
//! 别把"观察模式开着"读成"什么都能看"：
//!
//! * **HTTP/2 / HTTP/3（QUIC）看不到**：代理只广告 `http/1.1`（见 `tls.rs`）。
//!   只跑 h2 的客户端要么退化成 h1，要么握手失败；QUIC 根本不经过这里。
//! * **WebSocket 看不到**：`Upgrade: websocket` 一律回 501（只计数，不拆）。
//! * **TLS 失败/证书固定的站点看不到**：客户端不信任本地 CA，或 App 做了证书固定，
//!   握手直接失败 —— 连请求都读不到。
//! * **无法解码的体看不到**：代理会去掉 `Accept-Encoding` 要未压缩体，但上游若无视它
//!   仍回 `Content-Encoding: gzip`，我们拿到的是压缩字节，标记词当然也搜不到。
//!   同理，加密/混淆的载荷、protobuf/二进制协议里的广告标记都搜不到。
//! * **标记缺失 ≠ 没有广告**：`is_ad`/`promoted` 只是**猜想词表**，不是规范。
//!   命中是强证据，不命中什么也证明不了。
//! * **过大的体看不到**：请求体超过 [`crate::proxy::MAX_BODY_BYTES`] 时**明确关闭连接**
//!   （fail-open：宁可断开，不许发半截请求）。响应体同理。
//! * **`Expect: 100-continue` 的请求看不到**：本版不代传 100 继续，客户端可能等不到
//!   而超时。已知盲点，不要在名单里放这类上传接口。
//! * **`HEAD` 响应**：按"没有 body"处理（否则会把后续字节当 body 读）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 默认标记词表。
///
/// 这些词来自 [`crate::proxy`] 之外的真实观察结论（见 `docs/verification/DOMAIN-FIRST-AD-FILTERING.md`）：
/// 时间线/信息流接口里最常出现的机器可读广告标记。**它不是规范**，只是起点 ——
/// 词表可配置正是这个模式的用途：先采数据，再决定用确定性规则还是模型。
pub const DEFAULT_MARKERS: &[&str] = &[
    "is_ad",
    "ad_type",
    "promoted",
    "sponsored",
    "adsbygoogle",
    "广告",
];

/// 默认词表的拥有版（给 `ObserveConfig` 用）。
pub fn default_markers() -> Vec<String> {
    DEFAULT_MARKERS.iter().map(|s| s.to_string()).collect()
}

/// `path` 进摘要前最多留这么多**字符**（不是字节）。
pub const MAX_PATH_CHARS: usize = 128;

/// 一个标记词的命中次数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerHit {
    /// 词表里的原词。
    pub marker: String,
    /// 在 body 里出现的次数（ASCII 大小写不敏感，允许重叠）。
    pub count: usize,
}

/// 一次交换的**元数据**（不含 body 字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeMeta {
    pub host: String,
    pub method: String,
    /// 已去掉 query/fragment 并截断。
    pub path: String,
    /// 响应状态码（解析不出来时是 `0`）。
    pub status: u16,
    /// 原样的状态行（诊断用；进摘要日志前会截断）。
    pub status_line: String,
    pub content_type: Option<String>,
    /// **响应**体字节数。
    pub body_bytes: usize,
    /// **请求**体字节数。
    pub request_body_bytes: usize,
}

/// 一条**观察摘要**（本模式的核心产物）。
///
/// 字段就是设计里答应的那一组；`Serialize` 是为了"显式开启落盘"时把摘要写成旁文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRecord {
    pub host: String,
    pub method: String,
    /// 不含 query/fragment（敏感值最多的就是那里）。
    pub path: String,
    pub status: u16,
    pub content_type: Option<String>,
    /// **响应**体字节数。
    pub body_bytes: usize,
    /// **请求**体字节数（POST/GraphQL 的查询体有多大）。
    pub request_body_bytes: usize,
    /// body 的 16 位十六进制短哈希（FNV-1a 64 位，非密码学）。
    pub body_hash: String,
    /// 词表里**每个**词的命中次数（包含 0 —— 负例必须能一眼看出"全 0"）。
    pub marker_hits: Vec<MarkerHit>,
    /// 所有标记词的命中总数。
    pub marker_total: usize,
}

/// FNV-1a 64 位 → 16 位十六进制。
///
/// **刻意不引哈希库**：这里只要"同样的字节给同样的短串"，用于判断重复响应；
/// 它不是安全边界，所以不需要抗碰撞。
pub fn short_hash(body: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in body {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 一个标记词在 body 里出现了几次（**ASCII 大小写不敏感**，允许重叠命中）。
///
/// 中文词（`广告`）按 UTF-8 字节序列精确匹配，`to_ascii_lowercase` 对它无影响。
pub fn count_marker(body: &[u8], marker: &str) -> usize {
    let needle = marker.as_bytes();
    if needle.is_empty() || body.len() < needle.len() {
        return 0;
    }
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle.len() <= body.len() {
        if body[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            count += 1;
        }
        i += 1;
    }
    count
}

/// 词表里每个词的命中次数（顺序与词表一致；**零命中也保留**）。
pub fn marker_hits(body: &[u8], markers: &[String]) -> Vec<MarkerHit> {
    markers
        .iter()
        .map(|m| MarkerHit { marker: m.clone(), count: count_marker(body, m) })
        .collect()
}

/// 摘要用的 `path`：**去掉 query/fragment** 并截断。
///
/// `target` 可能是 origin-form（`/a?b=1`）或 absolute-form（`https://h/a?b=1`）——
/// 调用方传的是已经取过 path 的串，这里只管 query 与长度。
pub fn sanitize_path(target: &str) -> String {
    let cut = target.find(['?', '#']).unwrap_or(target.len());
    let trimmed: String = target[..cut].chars().take(MAX_PATH_CHARS).collect();
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed
    }
}

/// 把一次交换的元数据 + 真 body 汇总成一条摘要。
pub fn summarize(meta: &ExchangeMeta, body: &[u8], markers: &[String]) -> ExchangeRecord {
    let hits = marker_hits(body, markers);
    let total = hits.iter().map(|h| h.count).sum();
    ExchangeRecord {
        host: meta.host.clone(),
        method: meta.method.clone(),
        path: meta.path.clone(),
        status: meta.status,
        content_type: meta.content_type.clone(),
        body_bytes: meta.body_bytes,
        request_body_bytes: meta.request_body_bytes,
        body_hash: short_hash(body),
        marker_hits: hits,
        marker_total: total,
    }
}

/// 域名是否在 opt-in 名单里（**子域也算命中**，按标签边界）。
///
/// 与 [`crate::decide::BlocklistDecider`] 同一套匹配规则：名单写 `news.example` 时
/// `cdn.news.example` 也算命中（投放端换子域是常见做法），但 `notnews.example` 不算。
pub fn host_in_opt_in(hosts: &[String], host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    hosts.iter().any(|listed| {
        let listed = listed.trim().trim_end_matches('.').to_ascii_lowercase();
        !listed.is_empty() && (h == listed || h.ends_with(&format!(".{listed}")))
    })
}

/// 观察模式的配置。**默认关闭**（见模块文档）。
#[derive(Debug, Clone)]
pub struct ObserveConfig {
    /// 总开关。默认 `false`：构造了观察者也不会观察任何东西。
    pub enabled: bool,
    /// 按域名 opt-in（子域命中）。空 = 什么都不观察（"开了但没配"等于没开）。
    pub hosts: Vec<String>,
    /// 标记词表。默认 [`default_markers`]。
    pub markers: Vec<String>,
    /// **显式**留 body 的目录。默认 `None`（绝不落盘）。
    ///
    /// 只接受**绝对路径**；相对路径会被拒绝并记一条 `warn`。
    /// 调用方必须保证这个目录在**仓库之外** —— 完整 body 可能含隐私内容。
    pub capture_body_dir: Option<PathBuf>,
}

impl Default for ObserveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hosts: Vec::new(),
            markers: default_markers(),
            capture_body_dir: None,
        }
    }
}

/// 观察接缝。
///
/// 与 [`crate::decide::Decider`] / [`crate::rewrite::BodyRewriter`] 同一个套路：
/// 代理只依赖这个 trait，谁来观察、看哪些域名、留不留 body 都由上层装配。
///
/// **实现必须 fail-open**：`observe` 里出的任何错都只记日志，绝不许影响转发
/// （它是"只观察"，没有资格弄坏用户的流量）。
pub trait Observer: Send + Sync {
    /// 这个 host 是否在观察名单里。返回 `false` 时代理**连 body 都不汇总**。
    fn observes(&self, host: &str) -> bool;

    /// 观察词表。摘要里的命中计数就按它算（[`NoopObserver`] 是空的）。
    fn markers(&self) -> &[String];

    /// 记一条摘要。`body` 是**响应体载荷**（chunked 已解码），只读借用；
    /// 要不要落盘由实现自己决定。
    fn observe(&self, record: &ExchangeRecord, body: &[u8]);
}

/// 什么都不做的观察者（默认）。**它永远说 `false`**。
#[derive(Debug, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn observes(&self, _host: &str) -> bool {
        false
    }
    fn markers(&self) -> &[String] {
        &[]
    }
    fn observe(&self, _record: &ExchangeRecord, _body: &[u8]) {}
}

/// 产品用的观察者：按域名 opt-in → `tracing::info!` 写摘要 → 可选落盘。
pub struct DomainObserver {
    enabled: bool,
    hosts: Vec<String>,
    markers: Vec<String>,
    capture_dir: Option<PathBuf>,
}

impl DomainObserver {
    /// 从配置构造。会在这里把不合规的落盘目录**降级成不落盘**（而不是带病运行）。
    pub fn new(config: &ObserveConfig) -> Self {
        let capture_dir = match &config.capture_body_dir {
            Some(p) if p.is_absolute() => Some(p.clone()),
            Some(p) => {
                tracing::warn!(
                    dir = %p.display(),
                    "MITM 观察：落盘目录必须是绝对路径（且要在仓库外），已降级为不落盘"
                );
                None
            }
            None => None,
        };
        Self {
            enabled: config.enabled,
            hosts: config.hosts.clone(),
            markers: if config.markers.is_empty() {
                default_markers()
            } else {
                config.markers.clone()
            },
            capture_dir,
        }
    }

    /// 当前是否留 body（诊断/测试用）。
    pub fn captures_body(&self) -> bool {
        self.capture_dir.is_some()
    }
}

impl Observer for DomainObserver {
    fn observes(&self, host: &str) -> bool {
        self.enabled && host_in_opt_in(&self.hosts, host)
    }

    fn markers(&self) -> &[String] {
        &self.markers
    }

    fn observe(&self, record: &ExchangeRecord, body: &[u8]) {
        // 摘要走**现有日志通道**（tracing）。只记元数据与计数，**不记 body**。
        // 命中词只列非零项，日志短；零命中看 `marker_total=0`。
        let hits = record
            .marker_hits
            .iter()
            .filter(|h| h.count > 0)
            .map(|h| format!("{}={}", h.marker, h.count))
            .collect::<Vec<_>>()
            .join(",");
        tracing::info!(
            host = %record.host,
            method = %record.method,
            path = %record.path,
            status = record.status,
            content_type = record.content_type.as_deref().unwrap_or(""),
            body_bytes = record.body_bytes,
            request_body_bytes = record.request_body_bytes,
            marker_total = record.marker_total,
            markers = %hits,
            body_hash = %record.body_hash,
            "MITM 观察：交换摘要（只记录，不改写）"
        );

        if let Some(dir) = &self.capture_dir {
            if let Err(e) = capture_body(dir, record, body) {
                // 落盘失败**绝不影响转发**。
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "MITM 观察：body 落盘失败（不影响转发）"
                );
            }
        }
    }
}

/// 把 body 与摘要写到 `dir`（文件名用短哈希，天然去重）。
fn capture_body(dir: &Path, record: &ExchangeRecord, body: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let base = dir.join(&record.body_hash);
    write_private(&base.with_extension("body"), body)?;
    let meta = serde_json::to_vec(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    write_private(&base.with_extension("json"), &meta)?;
    Ok(())
}

/// 写文件并在 Unix 上收紧到 `0600`（body 可能含隐私内容）。
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        use std::io::Write;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(body: &[u8]) -> ExchangeMeta {
        ExchangeMeta {
            host: "news.example".into(),
            method: "GET".into(),
            path: "/api/timeline".into(),
            status: 200,
            status_line: "HTTP/1.1 200 OK".into(),
            content_type: Some("application/json".into()),
            body_bytes: body.len(),
            request_body_bytes: 0,
        }
    }

    /// 默认词表必须含设计里点名的那些词（少一个，采到的数据就少一类）。
    #[test]
    fn the_default_marker_list_covers_the_named_markers() {
        let d = default_markers();
        for want in ["is_ad", "ad_type", "promoted", "sponsored", "adsbygoogle", "广告"] {
            assert!(d.iter().any(|m| m == want), "默认词表缺 {want}: {d:?}");
        }
    }

    /// **正例**：`promoted` 两次 ⇒ 2（不是"命中就 1"）。
    #[test]
    fn counting_is_per_occurrence_not_per_presence() {
        let body = br#"{"a":{"promoted":true},"b":{"promoted":false}}"#;
        assert_eq!(count_marker(body, "promoted"), 2);
    }

    /// **负例**：干净 body ⇒ 每个词 0、总数 0。
    #[test]
    fn a_clean_body_has_all_zero_hits() {
        let markers = default_markers();
        let rec = summarize(&m(b"{\"id\":1}"), b"{\"id\":1}", &markers);
        assert_eq!(rec.marker_hits.len(), markers.len(), "每个词都要在摘要里（含 0）");
        assert!(rec.marker_hits.iter().all(|h| h.count == 0), "{:?}", rec.marker_hits);
        assert_eq!(rec.marker_total, 0);
    }

    /// 大小写不敏感；中文词按字节匹配。
    #[test]
    fn matching_is_ascii_case_insensitive_and_handles_chinese() {
        let body = "Promoted PROMOTED 广告x广告".as_bytes();
        assert_eq!(count_marker(body, "promoted"), 2);
        assert_eq!(count_marker(body, "广告"), 2);
        assert_eq!(count_marker(body, "is_ad"), 0);
    }

    /// 空词不许变成"无限命中"（空 needle 是经典的除零式 bug）。
    #[test]
    fn an_empty_marker_never_matches() {
        assert_eq!(count_marker(b"anything", ""), 0);
    }

    /// 短哈希：固定长度、确定性、内容变了就变。
    #[test]
    fn the_short_hash_is_stable_and_content_sensitive() {
        let a = short_hash(b"same");
        let b = short_hash(b"same");
        let c = short_hash(b"different");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|ch| ch.is_ascii_hexdigit()), "{a}");
    }

    /// `path` 去 query/fragment 并截断 —— 敏感值最多的就是这两处。
    #[test]
    fn the_path_is_stripped_of_query_and_truncated() {
        assert_eq!(sanitize_path("/api/timeline?uid=1&token=secret"), "/api/timeline");
        assert_eq!(sanitize_path("/a#frag"), "/a");
        assert_eq!(sanitize_path("?only=query"), "/");
        let long = format!("/{}", "x".repeat(500));
        assert_eq!(sanitize_path(&long).chars().count(), MAX_PATH_CHARS);
        // 中文按**字符**截断，不许把 UTF-8 切成半个字符。
        let zh = format!("/{}", "广".repeat(300));
        assert_eq!(sanitize_path(&zh).chars().count(), MAX_PATH_CHARS);
    }

    /// opt-in 匹配：子域命中、按标签边界（`notnews.example` 不算）。
    #[test]
    fn host_opt_in_matches_subdomains_but_not_suffix_lookalikes() {
        let hosts = vec!["news.example".to_string()];
        assert!(host_in_opt_in(&hosts, "news.example"));
        assert!(host_in_opt_in(&hosts, "NEWS.EXAMPLE."));
        assert!(host_in_opt_in(&hosts, "cdn.news.example"));
        assert!(!host_in_opt_in(&hosts, "notnews.example"));
        assert!(!host_in_opt_in(&hosts, ""));
        assert!(!host_in_opt_in(&[], "news.example"), "空名单 = 什么都不观察");
    }

    /// **默认关闭**：默认配置下即使 host 在名单里也不观察（更别说名单是空的）。
    #[test]
    fn the_observer_is_off_until_explicitly_enabled() {
        let off = DomainObserver::new(&ObserveConfig {
            hosts: vec!["news.example".to_string()],
            ..Default::default()
        });
        assert!(!off.observes("news.example"), "enabled=false 时必须不观察");

        let on = DomainObserver::new(&ObserveConfig {
            enabled: true,
            hosts: vec!["news.example".to_string()],
            ..Default::default()
        });
        assert!(on.observes("cdn.news.example"));
        assert!(!on.observes("other.example"));
        assert!(!on.captures_body(), "默认绝不落盘完整 body");
    }

    /// **相对路径的落盘目录必须被拒**（免得手滑写进仓库），并降级为不落盘。
    #[test]
    fn a_relative_capture_dir_is_refused() {
        let ob = DomainObserver::new(&ObserveConfig {
            enabled: true,
            hosts: vec!["news.example".to_string()],
            capture_body_dir: Some(PathBuf::from("crates/xt-mitm/leak")),
            ..Default::default()
        });
        assert!(!ob.captures_body(), "相对路径必须被拒（不许写进仓库）");
    }

    /// 显式的绝对路径才真的落盘，而且是 `0600`。
    #[test]
    fn an_absolute_capture_dir_writes_body_and_metadata() {
        let dir = std::env::temp_dir().join(format!(
            "xt-mitm-observe-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let ob = DomainObserver::new(&ObserveConfig {
            enabled: true,
            hosts: vec!["news.example".to_string()],
            capture_body_dir: Some(dir.clone()),
            ..Default::default()
        });
        assert!(ob.captures_body());
        let body = br#"{"promoted":true}"#;
        let rec = summarize(&m(body), body, &default_markers());
        ob.observe(&rec, body);

        let body_path = dir.join(format!("{}.body", rec.body_hash));
        let meta_path = dir.join(format!("{}.json", rec.body_hash));
        assert_eq!(std::fs::read(&body_path).unwrap(), body, "落盘必须是原始 body 字节");
        let meta: ExchangeRecord =
            serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
        assert_eq!(meta, rec, "旁文件必须就是这条摘要");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&body_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "含隐私的 body 必须 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `summarize` 把元数据字段原样带进摘要（别在中间丢字段）。
    #[test]
    fn summarize_carries_every_metadata_field() {
        let body = b"hello";
        let rec = summarize(&m(body), body, &default_markers());
        assert_eq!(rec.host, "news.example");
        assert_eq!(rec.method, "GET");
        assert_eq!(rec.path, "/api/timeline");
        assert_eq!(rec.status, 200);
        assert_eq!(rec.content_type.as_deref(), Some("application/json"));
        assert_eq!(rec.body_bytes, body.len());
        assert_eq!(rec.request_body_bytes, 0);
        assert_eq!(rec.body_hash, short_hash(body));
    }
}
