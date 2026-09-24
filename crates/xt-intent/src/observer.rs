//! 从核心日志的连接记录里挑出**值得花钱判定**的端点。
//!
//! # 数据来源
//!
//! `xt_core::xray::access_log::ConnectionRecord` —— 它已经把 `accepted` 行结构化，
//! 并把 `sniffed domain` 时序配对上来（`domain` 字段）。这里**不重新解析日志**：
//! 核心的 stdout 已经是单点，重复读会得到两份不一致的时间线
//! （`access_log.rs` 的模块文档里已经把这条理由写死了）。
//!
//! # 什么时候没有域名
//!
//! 实测只有约 52% 的 `accepted` 行能配对到域名（`access_log.rs` 的实测量级）。
//! 配不到就**不问** —— 拿一个 IP 去问"这是不是广告"是在浪费钱，
//! 而且答案没有可执行性（我们不会因为一个 IP 被判广告就拦一整个 CDN 段）。
//!
//! IP → 域名的回填（把已判定的域名解析出的 IP 变成 `ip` 规则）是后续阶段的事，
//! 这里不假装有。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use xt_core::xray::access_log::ConnectionRecord;

use crate::rules::normalize;
use crate::shape::FlowShape;

/// 每个主机名保留多少个连接时刻（用于判断心跳规律）。
const MAX_TIMES_PER_HOST: usize = 64;
/// 同时跟踪的主机名上限。超了按"最近最少使用"淘汰 —— 一个只出现一次、
/// 再也没出现过的候选，本来也轮不到它花钱。
const DEFAULT_MAX_TRACKED: usize = 4096;

/// 一条候选端点。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub host: String,
    /// 见过的端口（升序）。
    pub ports: BTreeSet<u16>,
    /// 见过的网络层（`tcp` / `udp`）。
    pub networks: BTreeSet<String>,
    /// 命中的入站。
    pub inbounds: BTreeSet<String>,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    pub connections: u64,
    pub shape: FlowShape,
}

impl Candidate {
    fn new(host: String, now: u64, port: Option<u16>, network: &str, inbound: &str) -> Self {
        let mut c = Self {
            host,
            ports: BTreeSet::new(),
            networks: BTreeSet::new(),
            inbounds: BTreeSet::new(),
            first_seen_unix: now,
            last_seen_unix: now,
            connections: 0,
            shape: FlowShape::default(),
        };
        c.absorb(now, port, network, inbound);
        c
    }

    fn absorb(&mut self, now: u64, port: Option<u16>, network: &str, inbound: &str) {
        if let Some(p) = port {
            self.ports.insert(p);
        }
        if !network.is_empty() {
            self.networks.insert(network.to_string());
        }
        if !inbound.is_empty() {
            self.inbounds.insert(inbound.to_string());
        }
        self.last_seen_unix = now;
        self.connections = self.connections.saturating_add(1);
    }

    /// 用累积到的时刻重算形状特征。
    fn refresh_shape(&mut self, times: &[u64]) {
        self.shape.connections = self.connections;
        self.shape.spans_secs = self.last_seen_unix.saturating_sub(self.first_seen_unix);
        self.shape.regular_interval = FlowShape::intervals_are_regular(times);
        self.shape.nonstandard_port = match (self.ports.len(), self.ports.contains(&80), self.ports.contains(&443)) {
            (0, _, _) => false,
            _ => !self.ports.contains(&80) && !self.ports.contains(&443),
        };
        self.shape.udp_only = !self.networks.is_empty() && self.networks.iter().all(|n| n == "udp");
    }

    /// 判定优先级：金额有限时先问谁。**不是判决**。
    pub fn priority(&self) -> u32 {
        self.shape.priority()
    }
}

/// 一次观测的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ObserveOutcome {
    /// 第一次见到这个主机名（已进待判定队列）。
    Discovered(Candidate),
    /// 已经见过，只更新了计数/形状。
    Updated,
    /// 没进队列，附原因（要能被统计与审计，不许静默）。
    Skipped(&'static str),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObserverStats {
    pub lines: u64,
    /// 配不到域名的连接行（实测约占一半，见模块文档）。
    pub without_domain: u64,
    pub discovered: u64,
    pub updated: u64,
    /// 因为"不像一个可判定的端点"被跳过的次数，按原因分桶。
    pub skipped: BTreeMap<&'static str, u64>,
    /// 因为跟踪表满而被淘汰的主机名数。
    pub evicted: u64,
}

/// 候选端点的观察者。
#[derive(Debug, Clone)]
pub struct Observer {
    known: BTreeMap<String, Candidate>,
    times: BTreeMap<String, Vec<u64>>,
    pending: VecDeque<String>,
    max_tracked: usize,
    /// 用户白名单（永不判定）。
    allow_hosts: BTreeSet<String>,
    pub stats: ObserverStats,
}

impl Default for Observer {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TRACKED)
    }
}

impl Observer {
    pub fn new(max_tracked: usize) -> Self {
        Self {
            known: BTreeMap::new(),
            times: BTreeMap::new(),
            pending: VecDeque::new(),
            max_tracked: max_tracked.max(16),
            allow_hosts: BTreeSet::new(),
            stats: ObserverStats::default(),
        }
    }

    /// 设置用户白名单（列表整体替换）。
    pub fn set_allow_hosts<I: IntoIterator<Item = String>>(&mut self, hosts: I) {
        self.allow_hosts = hosts.into_iter().map(|h| normalize(&h)).collect();
    }

    pub fn allow_hosts(&self) -> &BTreeSet<String> {
        &self.allow_hosts
    }

    pub fn tracked(&self) -> usize {
        self.known.len()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn candidate(&self, host: &str) -> Option<&Candidate> {
        self.known.get(host)
    }

    pub fn reset(&mut self) {
        self.known.clear();
        self.times.clear();
        self.pending.clear();
    }

    /// 吃一条连接记录。
    pub fn observe(&mut self, rec: &ConnectionRecord, now: u64) -> ObserveOutcome {
        self.stats.lines = self.stats.lines.saturating_add(1);

        let Some(raw_host) = rec.domain.as_deref() else {
            self.stats.without_domain = self.stats.without_domain.saturating_add(1);
            return ObserveOutcome::Skipped("no_domain");
        };
        let host = normalize(raw_host);
        if let Err(why) = self.filter(&host, rec) {
            *self.stats.skipped.entry(why).or_insert(0) += 1;
            return ObserveOutcome::Skipped(why);
        }

        let port = rec.target_port;
        let network = rec.network.as_str();
        let inbound = rec.inbound_tag.as_str();

        if let Some(existing) = self.known.get_mut(&host) {
            existing.absorb(now, port, network, inbound);
            let times = self.times.entry(host.clone()).or_default();
            push_capped(times, now, MAX_TIMES_PER_HOST);
            let snapshot = times.clone();
            if let Some(c) = self.known.get_mut(&host) {
                c.refresh_shape(&snapshot);
            }
            self.stats.updated = self.stats.updated.saturating_add(1);
            return ObserveOutcome::Updated;
        }

        let candidate = Candidate::new(host.clone(), now, port, network, inbound);
        self.times.insert(host.clone(), vec![now]);
        self.pending.push_back(host.clone());
        self.known.insert(host.clone(), candidate.clone());
        self.stats.discovered = self.stats.discovered.saturating_add(1);
        self.evict_if_needed();
        ObserveOutcome::Discovered(candidate)
    }

    /// `Err(reason)` = 不值得花钱判定。
    fn filter(&self, host: &str, rec: &ConnectionRecord) -> Result<(), &'static str> {
        is_candidate(host, &rec.inbound_tag, rec.target_port, &self.allow_hosts)
    }

    /// 取最多 `max` 条待判定候选（按优先级降序、再按首次出现升序）。
    ///
    /// 顺序是有意义的：预算有限时，心跳型/非标准端口的候选先花。
    pub fn drain_pending(&mut self, max: usize) -> Vec<Candidate> {
        let take = max.min(self.pending.len());
        let mut hosts: Vec<String> = self.pending.drain(..take).collect();
        hosts.sort_by(|a, b| {
            let pa = self.known.get(a).map(Candidate::priority).unwrap_or(0);
            let pb = self.known.get(b).map(Candidate::priority).unwrap_or(0);
            pb.cmp(&pa)
                .then_with(|| {
                    let fa = self.known.get(a).map(|c| c.first_seen_unix).unwrap_or(0);
                    let fb = self.known.get(b).map(|c| c.first_seen_unix).unwrap_or(0);
                    fa.cmp(&fb)
                })
                .then_with(|| a.cmp(b))
        });
        hosts.iter().filter_map(|h| self.known.get(h).cloned()).collect()
    }

    /// 把一条候选放回队列（网关失败、预算耗尽时**稍后重试**）。
    pub fn requeue(&mut self, host: &str) {
        let host = normalize(host);
        if self.known.contains_key(&host) && !self.pending.contains(&host) {
            self.pending.push_back(host);
        }
    }

    fn evict_if_needed(&mut self) {
        while self.known.len() > self.max_tracked {
            // 淘汰最近最少出现的那一条。
            let victim = self
                .known
                .iter()
                .min_by_key(|(h, c)| (c.last_seen_unix, (*h).clone()))
                .map(|(h, _)| h.clone());
            match victim {
                Some(h) => {
                    self.known.remove(&h);
                    self.times.remove(&h);
                    self.pending.retain(|p| p != &h);
                    self.stats.evicted = self.stats.evicted.saturating_add(1);
                }
                None => break,
            }
        }
    }
}

fn push_capped(v: &mut Vec<u64>, now: u64, cap: usize) {
    v.push(now);
    if v.len() > cap {
        let drop = v.len() - cap;
        v.drain(..drop);
    }
}

/// "这个主机名值得花钱判定吗？" —— **线上与离线评测的唯一实现**。
///
/// 抽成自由函数是为了让 `eval` 模块能复用同一套规则：如果离线评测用另一套
/// 过滤（比如忘了排除 IP 字面量），量出来的精确率就不是线上那个系统的。
///
/// `Err(reason)` 里的 reason 会进统计（`ObserverStats::skipped`），
/// 所以这些字符串是**可观测口径**，改名等于让历史数据失去意义。
pub fn is_candidate(
    host: &str,
    inbound: &str,
    port: Option<u16>,
    allow_hosts: &std::collections::BTreeSet<String>,
) -> Result<(), &'static str> {
    if host.is_empty() {
        return Err("empty_host");
    }
    if allow_hosts.contains(host) {
        return Err("allowlisted");
    }
    // 内部回环（应用自己查统计）与 DoH 字面量：永远没有可判定的身份。
    if inbound == "api" || host == "dns" {
        return Err("internal");
    }
    if host.starts_with('[') || host.contains(':') {
        return Err("ip_literal");
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Err("ip_literal");
    }
    if is_local_name(host) {
        return Err("local_name");
    }
    if !host.contains('.') {
        return Err("single_label");
    }
    if port == Some(53) {
        return Err("dns_transport");
    }
    Ok(())
}

/// 内网 / mDNS / 保留名。**判域名分流之前必须先排除它们** ——
/// 拿 `macbook.local` 去问"这是不是广告"既浪费钱又危险。
fn is_local_name(host: &str) -> bool {
    const LOCAL: &[&str] = &[
        "localhost",
        ".local",
        ".localdomain",
        ".lan",
        ".home",
        ".internal",
        ".in-addr.arpa",
        ".ip6.arpa",
        ".onion",
    ];
    LOCAL.iter().any(|s| host == s.trim_start_matches('.') || host.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(domain: Option<&str>, port: u16, network: &str, inbound: &str) -> ConnectionRecord {
        ConnectionRecord {
            ts_ms: 0,
            ts_text: "2026/09/24 12:00:00.000000".into(),
            from: "198.18.0.1:50000".into(),
            network: network.into(),
            target_host: "203.0.113.7".into(),
            target_port: Some(port),
            inbound_tag: inbound.into(),
            outbound_tag: "node-a".into(),
            domain: domain.map(str::to_string),
            domain_paired: domain.is_some(),
            domain_pair_delta_us: Some(30),
            sniff_id: Some("1".into()),
        }
    }

    #[test]
    fn a_new_domain_is_discovered_once_and_then_only_updated() {
        let mut o = Observer::default();
        let rec = conn(Some("ads.example"), 443, "tcp", "tun");
        match o.observe(&rec, 100) {
            ObserveOutcome::Discovered(c) => assert_eq!(c.host, "ads.example"),
            other => panic!("第一次应当是发现：{other:?}"),
        }
        assert_eq!(o.observe(&rec, 200), ObserveOutcome::Updated);
        assert_eq!(o.tracked(), 1);
        assert_eq!(o.pending(), 1, "同一个域名只排队一次");
        assert_eq!(o.stats.discovered, 1);
        assert_eq!(o.stats.updated, 1);

        let c = o.candidate("ads.example").unwrap();
        assert_eq!(c.connections, 2);
        assert_eq!(c.first_seen_unix, 100);
        assert_eq!(c.last_seen_unix, 200);
        assert_eq!(c.shape.spans_secs, 100);
    }

    #[test]
    fn records_without_a_domain_are_never_paid_for() {
        let mut o = Observer::default();
        assert_eq!(o.observe(&conn(None, 443, "tcp", "tun"), 1), ObserveOutcome::Skipped("no_domain"));
        assert_eq!(o.stats.without_domain, 1);
        assert_eq!(o.pending(), 0);
        assert_eq!(o.tracked(), 0);
    }

    #[test]
    fn hosts_that_are_not_worth_a_question_are_filtered_with_a_reason() {
        let cases: Vec<(Option<&str>, u16, &str, &'static str)> = vec![
            (Some("203.0.113.7"), 443, "ip_literal", "ip_literal"),
            (Some("2606:4700::1111"), 443, "ip_literal", "ip_literal"),
            (Some("localhost"), 443, "tcp", "local_name"),
            (Some("printer.local"), 443, "tcp", "local_name"),
            (Some("nas"), 445, "tcp", "single_label"),
            (Some("dns"), 443, "tcp", "internal"),
        ];
        for (host, port, inbound, expected) in cases {
            let mut o = Observer::default();
            assert_eq!(
                o.observe(&conn(host, port, "tcp", inbound), 1),
                ObserveOutcome::Skipped(expected),
                "{host:?}"
            );
        }
        // api 入站（应用自己查统计的回环）永远不参与。
        let mut o = Observer::default();
        assert_eq!(o.observe(&conn(Some("x.example"), 443, "tcp", "api"), 1), ObserveOutcome::Skipped("internal"));
    }

    #[test]
    fn dns_transport_and_the_allowlist_are_excluded() {
        let mut o = Observer::default();
        assert_eq!(o.observe(&conn(Some("ns.example"), 53, "udp", "tun"), 1), ObserveOutcome::Skipped("dns_transport"));

        let mut o = Observer::default();
        o.set_allow_hosts(vec!["Good.Example.".into()]);
        assert_eq!(o.observe(&conn(Some("good.example"), 443, "tcp", "tun"), 1), ObserveOutcome::Skipped("allowlisted"));
        assert_eq!(o.stats.skipped.get("allowlisted"), Some(&1));
    }

    #[test]
    fn repeated_traffic_forms_a_heartbeat_shape() {
        let mut o = Observer::default();
        let rec = conn(Some("beacon.example"), 8443, "udp", "tun");
        for t in [0u64, 30, 61, 90] {
            o.observe(&rec, t);
        }
        let c = o.candidate("beacon.example").unwrap();
        assert!(c.shape.regular_interval, "{:?}", c.shape);
        assert!(c.shape.udp_only);
        assert!(c.shape.nonstandard_port);
        assert!(c.priority() > 0);
        // 形状加分有上限，且不可能单独定罪。
        assert!(c.shape.bonus(0.10) <= 0.10);
    }

    #[test]
    fn a_https_endpoint_is_not_marked_nonstandard() {
        let mut o = Observer::default();
        o.observe(&conn(Some("site.example"), 443, "tcp", "tun"), 0);
        let c = o.candidate("site.example").unwrap();
        assert!(!c.shape.nonstandard_port);
        assert!(!c.shape.udp_only);
        assert!(!c.shape.regular_interval);
    }

    #[test]
    fn drain_pending_takes_the_highest_priority_first_and_only_once() {
        let mut o = Observer::default();
        o.observe(&conn(Some("plain.example"), 443, "tcp", "tun"), 0);
        for t in [0u64, 30, 61, 90] {
            o.observe(&conn(Some("beacon.example"), 8443, "udp", "tun"), t);
        }
        o.observe(&conn(Some("other.example"), 443, "tcp", "tun"), 1);

        let taken = o.drain_pending(2);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].host, "beacon.example", "优先级最高的先花预算");
        assert_eq!(o.pending(), 1);
        let again = o.drain_pending(5);
        assert_eq!(again.len(), 1);
        assert!(o.drain_pending(5).is_empty(), "取过的不会再取一次");
    }

    #[test]
    fn requeue_makes_a_failed_candidate_retryable_but_only_once() {
        let mut o = Observer::default();
        o.observe(&conn(Some("a.example"), 443, "tcp", "tun"), 0);
        assert_eq!(o.drain_pending(1).len(), 1);
        o.requeue("a.example");
        o.requeue("a.example");
        assert_eq!(o.pending(), 1, "重复 requeue 不该把队列撑爆");
        o.requeue("never-seen.example");
        assert_eq!(o.pending(), 1, "没观察过的域名不该被凭空排队");
    }

    #[test]
    fn the_tracking_table_is_bounded_and_evicts_the_stalest() {
        let mut o = Observer::new(16);
        for i in 0..40 {
            o.observe(&conn(Some(&format!("h{i}.example")), 443, "tcp", "tun"), i as u64);
        }
        assert!(o.tracked() <= 16, "跟踪表必须是有界的：{}", o.tracked());
        assert!(o.stats.evicted > 0);
        assert!(o.candidate("h39.example").is_some(), "最近的必须还在");
    }

    /// 自由函数与 `Observer::filter` 必须逐例一致 —— 离线评测就是靠这条
    /// 才敢声称"量的是线上那个系统"。
    #[test]
    fn the_free_filter_matches_the_observer() {
        let empty = BTreeSet::new();
        let cases: Vec<(Option<&str>, u16, &str, Option<&'static str>)> = vec![
            (Some("ads.example"), 443, "tun", None),
            (Some("203.0.113.7"), 443, "tun", Some("ip_literal")),
            (Some("localhost"), 443, "tun", Some("local_name")),
            (Some("nas"), 445, "tun", Some("single_label")),
            (Some("dns"), 443, "tun", Some("internal")),
            (Some("ns.example"), 53, "tun", Some("dns_transport")),
            (Some("x.example"), 443, "api", Some("internal")),
            (None, 443, "tun", Some("empty_host")),
        ];
        for (host, port, inbound, expected) in cases {
            let host = host.unwrap_or("");
            let mut o = Observer::default();
            let rec = conn(if host.is_empty() { None } else { Some(host) }, port, "tcp", inbound);
            let via_observer = match o.observe(&rec, 0) {
                ObserveOutcome::Skipped(r) => Some(r),
                _ => None,
            };
            let via_free = is_candidate(host, inbound, Some(port), &empty).err();
            assert_eq!(via_free, expected, "{host}");
            if !host.is_empty() {
                assert_eq!(via_observer, expected, "{host}：两条路径必须一致");
            }
            // 白名单在两个入口上都要生效。
            let mut allow = BTreeSet::new();
            allow.insert("ads.example".to_string());
            assert_eq!(is_candidate("ads.example", "tun", Some(443), &allow).err(), Some("allowlisted"));
        }
    }

    #[test]
    fn reset_clears_everything_including_the_queue() {
        let mut o = Observer::default();
        o.observe(&conn(Some("a.example"), 443, "tcp", "tun"), 0);
        o.reset();
        assert_eq!(o.tracked(), 0);
        assert_eq!(o.pending(), 0);
    }
}
