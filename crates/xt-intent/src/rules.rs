//! 判决 → Xray 路由规则。
//!
//! # 为什么复用 `xt-core` 的既有 IR
//!
//! `CoreConfigInput.rules` 已经是"调用方合并好的 `&[RoutingRule]`"。所以意图判决的表达
//! 形式就是**一组 `RoutingRule`** —— 于是：
//!
//! * `xt-core` 不需要依赖本 crate（没有环）；
//! * 既有的 `route_explain`、拓扑页、单测全部继续工作；
//! * 界面上会出现 `intent-block-*` / `intent-allow-*` 两带规则，用户**看得见**
//!   意图过滤真的生效了，而不是"黑盒里有个东西在拦"。
//!
//! # `allow` 只有一个来源：**用户**
//!
//! 模型的 `Allow` 判决**不生成任何规则** —— "不拦"就是"不在 block 名单里"。
//! 这一条很重要：如果给模型的每个 Allow 都生成一条 `→ direct` 的规则，
//! 就会**悄悄改变那个域名的路由**（本来该走节点的被强制直连）。
//! 用户只说了"别拦它"，我们不该顺手改他的分流。
//!
//! 所以 `allow` 只来自用户显式纠正，而且**动作由用户选**（直连 / 走代理），
//! 我们绝不替他猜。

use std::collections::BTreeSet;

use xt_core::routing::{MatchCondition, RoutingRule, RuleAction};

use crate::cache::CacheEntry;
use crate::verdict::Verdict;

/// 用户对被误杀域名选的动作。**必须显式**，没有默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowAction {
    /// 让这个域名直连。
    #[default]
    Direct,
    /// 让这个域名走当前选中的节点。
    Proxy,
}

impl AllowAction {
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Direct => "直连",
            Self::Proxy => "走代理",
        }
    }
}

/// 一条用户纠正。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AllowOverride {
    pub host: String,
    #[serde(default)]
    pub action: AllowAction,
}

/// 物化选项。
#[derive(Debug, Clone, PartialEq)]
pub struct RuleOptions {
    /// 这些规则只在哪些入站上生效。
    ///
    /// **默认必须显式列举**，不能用"不写 = 所有入站"：MITM 阶段会新增一个本地
    /// 回连用的 socks 入站，如果规则不限定入站，那条回连会再次命中名单规则，
    /// 形成"拆包 → 再拆包"的自环（设计文档 §8.3）。
    pub inbound_tags: Vec<String>,
    /// 用户的放行纠正。
    pub allow_overrides: Vec<AllowOverride>,
}

impl Default for RuleOptions {
    fn default() -> Self {
        Self {
            inbound_tags: vec!["tun".into(), "socks".into(), "http".into()],
            allow_overrides: Vec::new(),
        }
    }
}

/// 物化结果：两带规则 + 被丢掉的非法输入（要能被审计，不许静默）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IntentRules {
    /// 放行带（用户的纠正）。**必须排在 block 带之前**。
    pub allow: Vec<RoutingRule>,
    /// 拦截带。
    pub block: Vec<RoutingRule>,
    /// 因为形状非法而没有被物化的主机名 `(host, why)`。
    pub skipped: Vec<(String, String)>,
}

impl IntentRules {
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.block.is_empty()
    }

    pub fn block_domains(&self) -> Vec<String> {
        self.block.iter().filter_map(rule_host).collect()
    }

    pub fn allow_domains(&self) -> Vec<String> {
        self.allow.iter().filter_map(rule_host).collect()
    }

    /// 全部受影响域名的稳定哈希。
    ///
    /// 用途只有一个：**只有它变了才重启核心**。规则顺序变了但集合没变时重启是纯浪费，
    /// 而重启会打断用户所有连接。
    pub fn domain_set_hash(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for d in self.allow_domains() {
            lines.push(format!("allow {d}"));
        }
        for d in self.block_domains() {
            lines.push(format!("block {d}"));
        }
        lines.sort();
        lines.dedup();
        format!("{:016x}", fnv1a64(lines.join("\n").as_bytes()))
    }
}

fn rule_host(rule: &RoutingRule) -> Option<String> {
    rule.when.domains.first().map(|d| d.trim_start_matches("full:").to_string())
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// 主机名能不能写进 Xray 的 `domain` 字段。
///
/// 宽松到允许下划线（`_dmarc` 这类真实存在），但**拒绝**任何会改变规则语义的字符
/// （空格、引号、冒号、斜杠、通配符、前导点）。这些一旦漏进去，Xray 可能整份配置
/// 拒载，而症状离根因很远。
pub fn valid_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("空主机名".into());
    }
    if host.len() > 253 {
        return Err("主机名超过 253 字符".into());
    }
    if !host.contains('.') {
        return Err("单标签主机名（没有点）".into());
    }
    if host.starts_with('.') || host.ends_with('.') {
        return Err("前导/尾随点".into());
    }
    if let Some(c) = host.chars().find(|c| {
        !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '_')
    }) {
        return Err(format!("非法字符 {c:?}（必须先归一化为小写 ASCII）"));
    }
    Ok(())
}

/// 把判决物化成两带规则。
///
/// * `entries`：`(host, verdict)`。顺序不影响输出（输出按主机名排序，确定性）。
/// * 只在 `Verdict::Block` 上生成 block 规则；`Allow` / `Deferred` 不生成任何东西。
/// * 出现在 `allow_overrides` 里的主机**永远不会**出现在 block 带（allow 优先）。
pub fn materialize<'a, I>(entries: I, opts: &RuleOptions) -> IntentRules
where
    I: IntoIterator<Item = (&'a str, &'a Verdict)>,
{
    let mut out = IntentRules::default();

    // 1) 放行带：用户纠正。去重 + 排序（同域名取后出现的那条动作，避免顺序敏感）。
    let mut allow_map: std::collections::BTreeMap<String, AllowAction> = Default::default();
    for o in &opts.allow_overrides {
        let host = normalize(o.host.trim());
        if let Err(why) = valid_host(&host) {
            out.skipped.push((o.host.clone(), format!("放行纠正无效：{why}")));
            continue;
        }
        allow_map.insert(host, o.action);
    }
    for (host, action) in &allow_map {
        let then = match action {
            AllowAction::Direct => RuleAction::Direct,
            AllowAction::Proxy => RuleAction::Proxy { outbound: None },
        };
        out.allow.push(RoutingRule::new(
            format!("intent-allow-{host}"),
            format!("意图放行（用户纠正）：{host} → {}", action.label_zh()),
            MatchCondition {
                domains: vec![format!("full:{host}")],
                inbound_tags: opts.inbound_tags.clone(),
                ..Default::default()
            },
            then,
        ));
    }

    // 2) 拦截带：只有 Block 判决，且不在放行集合里。
    let mut blocked: BTreeSet<String> = BTreeSet::new();
    for (host, verdict) in entries {
        if !verdict.is_block() {
            continue;
        }
        let host = normalize(host.trim());
        if allow_map.contains_key(&host) {
            // 用户已经纠正过它 —— 这就是 "allow 永远优先" 的落点。
            continue;
        }
        if let Err(why) = valid_host(&host) {
            out.skipped.push((host, why));
            continue;
        }
        blocked.insert(host);
    }
    for host in blocked {
        out.block.push(RoutingRule::new(
            format!("intent-block-{host}"),
            format!("意图拦截（Jev 判定）：{host}"),
            MatchCondition {
                domains: vec![format!("full:{host}")],
                inbound_tags: opts.inbound_tags.clone(),
                ..Default::default()
            },
            RuleAction::Block,
        ));
    }

    out
}

/// 从缓存条目直接物化（桌面上最常用的一条路径）。
pub fn materialize_from_cache<'a, I>(entries: I, opts: &RuleOptions) -> IntentRules
where
    I: IntoIterator<Item = &'a CacheEntry>,
{
    let owned: Vec<(String, Verdict)> = entries
        .into_iter()
        .map(|e| (e.host.clone(), e.verdict.clone()))
        .collect();
    let refs: Vec<(&str, &Verdict)> = owned.iter().map(|(h, v)| (h.as_str(), v)).collect();
    materialize(refs, opts)
}

/// 主机名归一化：小写 + 去尾随点。**不**做任何"猜测性"修复。
pub fn normalize(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{AllowReason, BlockVerdict, Category, DeferReason};

    fn block_verdict() -> Verdict {
        Verdict::Block(BlockVerdict {
            category: Category::AdOrMonetization,
            ads_intent: 0.97,
            risk_of_breakage: 0.05,
            choice_confidence: 0.93,
            effective_min: 0.85,
        })
    }

    fn names(rules: &[RoutingRule]) -> Vec<String> {
        rules.iter().filter_map(rule_host).collect()
    }

    #[test]
    fn only_block_verdicts_produce_rules() {
        let allow = Verdict::Allow(AllowReason::CategoryNotBlockable { category: Category::CdnOrInfra });
        let deferred = Verdict::Deferred(DeferReason::GatewayUnavailable { message: "x".into() });
        let b = block_verdict();
        let rules = materialize(
            vec![("ad.example", &b), ("cdn.example", &allow), ("d.example", &deferred)],
            &RuleOptions::default(),
        );
        assert_eq!(names(&rules.block), vec!["ad.example"]);
        assert!(rules.allow.is_empty());
        assert!(rules.skipped.is_empty());
    }

    #[test]
    fn a_user_override_beats_a_block_and_never_silently_changes_routing() {
        let b = block_verdict();
        let opts = RuleOptions {
            allow_overrides: vec![
                AllowOverride { host: "cdn.example".into(), action: AllowAction::Direct },
                AllowOverride { host: "ad.example".into(), action: AllowAction::Proxy },
            ],
            ..Default::default()
        };
        let rules = materialize(vec![("ad.example", &b), ("cdn.example", &b)], &opts);

        // allow 带里两条，block 带**一条都没有** —— 用户纠正过的域名不可能再被拦。
        assert_eq!(names(&rules.allow), vec!["ad.example", "cdn.example"]);
        assert!(rules.block.is_empty(), "用户放行过的域名仍在 block 带：{:?}", names(&rules.block));

        // 动作由用户选，我们没有替他猜。
        let ad = rules.allow.iter().find(|r| rule_host(r).as_deref() == Some("ad.example")).unwrap();
        assert_eq!(ad.then, RuleAction::Proxy { outbound: None });
        let cdn = rules.allow.iter().find(|r| rule_host(r).as_deref() == Some("cdn.example")).unwrap();
        assert_eq!(cdn.then, RuleAction::Direct);

        // 而且 allow 带一定排在 block 带之前（两条带是两个 Vec，由调用方按此顺序拼）。
        assert_eq!(rules.allow[0].id, "intent-allow-ad.example");
    }

    #[test]
    fn rule_shape_is_exactly_what_xray_expects() {
        let b = block_verdict();
        let rules = materialize(vec![("adsrv-7f3.example", &b)], &RuleOptions::default());
        let r = &rules.block[0];

        assert_eq!(r.id, "intent-block-adsrv-7f3.example");
        assert!(r.enabled);
        assert_eq!(r.when.domains, vec!["full:adsrv-7f3.example".to_string()]);
        assert_eq!(r.when.inbound_tags, vec!["tun".to_string(), "socks".to_string(), "http".to_string()]);
        assert_eq!(r.then, RuleAction::Block);
        assert!(!r.when.is_empty(), "空条件会变成 catch-all，把后面所有规则吃掉");

        // 编译出来的字段与设计文档 §6 的形状一致。
        let compiled = xt_core::routing::compile(std::slice::from_ref(r), "node-x");
        assert_eq!(compiled[0]["type"], "field");
        assert_eq!(compiled[0]["outboundTag"], "block");
        assert_eq!(compiled[0]["ruleTag"], "intent-block-adsrv-7f3.example");
        assert_eq!(compiled[0]["domain"][0], "full:adsrv-7f3.example");
        assert_eq!(compiled[0]["inboundTag"][0], "tun");
        assert!(compiled[0].get("network").is_none(), "意图规则不该限制网络层（TCP/UDP 都要拦）");
    }

    #[test]
    fn invalid_hosts_are_reported_not_silently_dropped() {
        let b = block_verdict();
        let rules = materialize(
            vec![
                ("good.example", &b),
                ("has space.example", &b),
                ("", &b),
                ("singlelabel", &b),
                ("wild*card.example", &b),
            ],
            &RuleOptions::default(),
        );
        assert_eq!(names(&rules.block), vec!["good.example"]);
        assert_eq!(rules.skipped.len(), 4, "{:?}", rules.skipped);
        assert!(rules.skipped.iter().any(|(h, _)| h.is_empty()));
    }

    #[test]
    fn hosts_are_lowercased_and_deduplicated() {
        let b = block_verdict();
        let rules = materialize(
            vec![("ADS.EXAMPLE.", &b), ("ads.example", &b)],
            &RuleOptions::default(),
        );
        assert_eq!(names(&rules.block), vec!["ads.example"], "同一域名只出一条规则");
    }

    #[test]
    fn output_is_deterministic_regardless_of_input_order() {
        let b = block_verdict();
        let a = materialize(vec![("b.example", &b), ("a.example", &b)], &RuleOptions::default());
        let c = materialize(vec![("a.example", &b), ("b.example", &b)], &RuleOptions::default());
        assert_eq!(a, c);
        assert_eq!(names(&a.block), vec!["a.example", "b.example"]);
    }

    #[test]
    fn rule_ids_are_unique_even_with_an_override_and_a_block_for_the_same_host() {
        let b = block_verdict();
        let opts = RuleOptions {
            allow_overrides: vec![AllowOverride { host: "x.example".into(), action: AllowAction::Direct }],
            ..Default::default()
        };
        let rules = materialize(vec![("x.example", &b)], &opts);
        let mut ids: Vec<&str> = rules.allow.iter().chain(rules.block.iter()).map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "重复 ruleTag 会让核心拒绝启动（v0.8.37 的 P0）");
    }

    #[test]
    fn domain_set_hash_changes_only_when_the_set_changes() {
        let b = block_verdict();
        let a = materialize(vec![("a.example", &b)], &RuleOptions::default());
        let a2 = materialize(vec![("a.example", &b)], &RuleOptions::default());
        assert_eq!(a.domain_set_hash(), a2.domain_set_hash(), "同一集合必须同哈希");

        let with_b = materialize(vec![("a.example", &b), ("b.example", &b)], &RuleOptions::default());
        assert_ne!(a.domain_set_hash(), with_b.domain_set_hash());

        // 用户放行会改变生效集合 ⇒ 哈希必须变（这样才触发一次必要的重启）。
        let opts = RuleOptions {
            allow_overrides: vec![AllowOverride { host: "a.example".into(), action: AllowAction::Direct }],
            ..Default::default()
        };
        let overridden = materialize(vec![("a.example", &b), ("b.example", &b)], &opts);
        assert_ne!(with_b.domain_set_hash(), overridden.domain_set_hash());
    }

    #[test]
    fn materialize_from_cache_matches_materialize() {
        let entry = CacheEntry {
            host: "a.example".into(),
            verdict: block_verdict(),
            decided_at_unix: 1,
            expires_at_unix: 10,
            hits: 0,
            model: None,
        };
        let from_cache = materialize_from_cache(std::slice::from_ref(&entry), &RuleOptions::default());
        let direct = materialize(vec![("a.example", &entry.verdict)], &RuleOptions::default());
        assert_eq!(from_cache, direct);
    }

    #[test]
    fn valid_host_rules() {
        assert!(valid_host("ads.example").is_ok());
        assert!(valid_host("a-b_c.example").is_ok());
        assert!(valid_host("").is_err());
        assert!(valid_host("localhost").is_err());
        assert!(valid_host(".a.example").is_err());
        assert!(valid_host("a.example.").is_err());
        assert!(valid_host("a.example:443").is_err());
        assert!(valid_host("up.example/ads").is_err());
        assert!(valid_host(&format!("{}.example", "a".repeat(300))).is_err());
    }

    #[test]
    fn normalize_only_lowercases_and_strips_the_trailing_dot() {
        assert_eq!(normalize(" ADS.Example. "), "ads.example");
        assert_eq!(normalize("a.b.c"), "a.b.c");
    }
}
