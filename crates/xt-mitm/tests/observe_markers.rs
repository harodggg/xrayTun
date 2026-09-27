//! 判据 3：观察记录的**标记词计数**（正例 + 负例）与摘要字段。
//!
//! 这个文件用的是观察模式**新增的**接缝（`Observer` / `ObserveConfig` /
//! `serve_observing`），所以在实现之前它**编译不过** —— 这本身就是"改前红"。
//! 实现之后它必须绿，而且负例（干净响应 ⇒ 全 0）必须真的能失败：
//! 把计数实现成"只要 body 非空就记 1"时，负例应当红。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    ca_cert_der, client_config, https_raw, install_crypto_provider, FakeUpstream, ScriptedOrigin,
};
use xt_mitm::observe::host_in_opt_in;
use xt_mitm::{
    serve_observing, BlocklistDecider, ExchangeRecord, LocalCa, ObserveConfig, Observer,
    ProxyConfig,
};

/// 把每次交换的摘要 + body 收进内存（测试用）。**opt-in 匹配与产品同一个函数。**
struct Recording {
    enabled: bool,
    hosts: Vec<String>,
    markers: Vec<String>,
    seen: Mutex<Vec<(ExchangeRecord, Vec<u8>)>>,
}

impl Recording {
    fn new(cfg: &ObserveConfig) -> Self {
        Self {
            enabled: cfg.enabled,
            hosts: cfg.hosts.clone(),
            markers: cfg.markers.clone(),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<ExchangeRecord> {
        self.seen.lock().unwrap().iter().map(|(r, _)| r.clone()).collect()
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().iter().map(|(_, b)| b.clone()).collect()
    }
}

impl Observer for Recording {
    fn observes(&self, host: &str) -> bool {
        self.enabled && host_in_opt_in(&self.hosts, host)
    }
    fn markers(&self) -> &[String] {
        &self.markers
    }
    fn observe(&self, record: &ExchangeRecord, body: &[u8]) {
        self.seen.lock().unwrap().push((record.clone(), body.to_vec()));
    }
}

struct Harness {
    proxy: xt_mitm::ProxyHandle,
    origin: ScriptedOrigin,
    client: Arc<rustls::ClientConfig>,
    observer: Arc<Recording>,
    _upstream: FakeUpstream,
}

fn harness(body: &[u8], cfg: ObserveConfig) -> Harness {
    install_crypto_provider();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    let mut full = resp;
    full.extend_from_slice(body);
    let origin = ScriptedOrigin::start(vec![full]);
    let upstream = FakeUpstream::start(origin.port);
    let ca = Arc::new(LocalCa::generate().unwrap());
    let client = client_config(ca_cert_der(&ca));
    let pcfg = ProxyConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream_socks: format!("127.0.0.1:{}", upstream.port).parse().unwrap(),
        io_timeout: Duration::from_secs(5),
        max_connections: 16,
        assumed_port: 443,
    };
    let observer = Arc::new(Recording::new(&cfg));
    let obs: Arc<dyn Observer> = observer.clone();
    let proxy = serve_observing(pcfg, ca, Arc::new(BlocklistDecider::default()), obs)
        .expect("起观察代理");
    Harness { proxy, origin, client, observer, _upstream: upstream }
}

fn get(h: &Harness, host: &str, path: &str) -> String {
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let (resp, clean) = https_raw(h.proxy.listen, host, req.as_bytes(), &h.client);
    assert!(clean, "代理必须干净收尾");
    resp
}

fn opt_in(hosts: &[&str]) -> ObserveConfig {
    ObserveConfig {
        enabled: true,
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        ..Default::default()
    }
}

/// **正例**：响应里 `promoted` 出现两次 ⇒ 计数必须是 2（不许"命中就记 1"）。
#[test]
fn a_response_with_two_promoted_markers_counts_exactly_two() {
    let body = br#"{"items":[{"promoted":true},{"promoted":false}]}"#;
    let h = harness(body, opt_in(&["news.example"]));
    let resp = get(&h, "news.example", "/api/timeline?uid=secret&token=abc");
    assert!(resp.contains("200 OK"), "{resp:?}");
    assert_eq!(h.origin.hits(), 1);

    let recs = h.observer.records();
    assert_eq!(recs.len(), 1, "一次交换应当写恰好一条摘要");
    let r = &recs[0];
    assert_eq!(r.host, "news.example");
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/api/timeline", "query 里的敏感值不许进摘要");
    assert_eq!(r.status, 200);
    assert_eq!(r.content_type.as_deref(), Some("application/json; charset=utf-8"));
    assert_eq!(r.body_bytes, body.len());
    assert_eq!(r.body_hash.len(), 16, "短哈希必须是 16 位十六进制");
    assert!(r.body_hash.chars().all(|c| c.is_ascii_hexdigit()), "{}", r.body_hash);
    let promoted = r
        .marker_hits
        .iter()
        .find(|h| h.marker == "promoted")
        .expect("词表里必须有 promoted");
    assert_eq!(promoted.count, 2, "promoted 出现两次 ⇒ 计数 2：{:?}", r.marker_hits);
    assert_eq!(r.marker_total, 2);

    // body 逐字节交给观察者（默认实现**不落盘**；这里证明接缝拿得到真内容）。
    assert_eq!(h.observer.bodies()[0], body);

    let st = h.proxy.stats();
    assert_eq!(st.observed, 1, "观察条数必须可见：{st:?}");
    assert_eq!(st.observed_marker_hits, 2, "标记命中总数必须可见：{st:?}");
}

/// **负例**：干净响应 ⇒ 每个标记词都是 0、总数为 0。
///
/// 没有这条，"body 非空就记 1"也能让正例变绿。
#[test]
fn a_clean_response_has_zero_marker_hits() {
    let body = br#"{"items":[{"id":1},{"id":2}],"title":"ordinary"}"#;
    let h = harness(body, opt_in(&["news.example"]));
    get(&h, "news.example", "/api/timeline");

    let recs = h.observer.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert!(!r.marker_hits.is_empty(), "摘要必须列出词表里的每个词（含 0）");
    assert!(
        r.marker_hits.iter().all(|h| h.count == 0),
        "干净响应不许有任何命中：{:?}",
        r.marker_hits
    );
    assert_eq!(r.marker_total, 0);
    assert_eq!(h.proxy.stats().observed_marker_hits, 0);
}

/// **按域名 opt-in**：名单外的域名一次摘要都不写；名单内的子域必须命中。
#[test]
fn only_opt_in_hosts_are_observed() {
    let h = harness(br#"{"promoted":true}"#, opt_in(&["news.example"]));
    get(&h, "other.example", "/api/timeline");
    assert!(h.observer.records().is_empty(), "名单外的域名不许被观察");
    assert_eq!(h.proxy.stats().observed, 0);

    get(&h, "cdn.news.example", "/api/timeline");
    let recs = h.observer.records();
    assert_eq!(recs.len(), 1, "名单内域名的子域必须命中");
    assert_eq!(recs[0].host, "cdn.news.example");
    assert_eq!(h.proxy.stats().observed, 1);
}

/// `ObserveConfig::default()` 必须是**关着**的（"默认关闭"不能只是文案）。
#[test]
fn the_default_observe_config_is_off() {
    let c = ObserveConfig::default();
    assert!(!c.enabled, "观察模式默认必须关着");
    assert!(c.hosts.is_empty(), "默认没有 opt-in 域名");
    assert!(c.capture_body_dir.is_none(), "默认绝不落盘 body");
    for m in ["promoted", "is_ad", "ad_type", "sponsored", "adsbygoogle", "广告"] {
        assert!(c.markers.iter().any(|x| x == m), "默认词表必须含 {m}");
    }
}
