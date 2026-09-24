//! 引擎：把观察、缓存、预算、网关、闸门、审计、规则串起来。
//!
//! # 数据面永不等模型
//!
//! [`IntentEngine::observe`] 是**同步且极便宜**的：它只更新内存里的候选表。
//! 真正花钱的 [`IntentEngine::classify_pending`] 由调用方在**后台任务**里按节拍跑
//! （桌面端用 `spawn_blocking`，因为网关客户端是阻塞的）。
//! 于是"用户正在加载页面"与"我们在问模型"这两件事在时间上完全解耦 ——
//! 这正是"首访放行、次访拦截"能够成立的原因。
//!
//! # fail-open 是默认路径
//!
//! 任何一步失败都落到 [`crate::verdict::Verdict::Deferred`]，也就是**放行 + 审计**。
//! 没有一条失败路径会生成 block 规则。

use std::path::PathBuf;

use tracing::{debug, warn};
use xt_core::xray::access_log::ConnectionRecord;

use crate::audit::{AuditLog, AuditRecord};
use crate::budget::{Budget, BudgetSnapshot};
use crate::cache::{fingerprint, CacheLoadOutcome, CacheEntry, VerdictCache};
use crate::gateway::Gateway;
use crate::observer::{ObserveOutcome, Observer, ObserverStats};
use crate::question::{domain_request, FlowContext};
use crate::rules::{materialize_from_cache, AllowAction, AllowOverride, IntentRules, RuleOptions};
use crate::verdict::{decide, DeferReason, Thresholds, Verdict};

/// 判决的有效期（秒）。
pub const BLOCK_TTL_SECS: u64 = 90 * 24 * 60 * 60;
pub const ALLOW_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// 拿不到答案时的重试间隔。
pub const DEFERRED_TTL_SECS: u64 = 60 * 60;
/// 形状不对（服务端少给字段 / 标签不认识）时的退避：比网关故障长一点，
/// 避免对着一个持续坏掉的服务端猛发请求。
pub const SCHEMA_FAILURE_TTL_SECS: u64 = 10 * 60;

/// 引擎配置。**整体参与缓存指纹** —— 任何一项变化都会让旧判决失效。
#[derive(Debug, Clone, PartialEq)]
pub struct IntentConfig {
    pub enabled: bool,
    /// 演练模式：判决照做、审计照写，但**不生成 block 规则**。默认开。
    pub drill: bool,
    pub model: String,
    /// 网关 base URL（用于指纹与展示）。
    pub base_url: String,
    pub thresholds: Thresholds,
    pub per_minute: u32,
    pub per_day: u32,
    pub cache_max_entries: usize,
    pub max_candidates_per_tick: usize,
    /// 用户白名单（永不判定）。
    pub allow_hosts: Vec<String>,
    /// 用户对误杀的纠正。
    pub allow_overrides: Vec<AllowOverride>,
    /// 是否把发给网关的 `state` 也写进审计（默认**否**）。
    pub store_context_in_audit: bool,
}

impl Default for IntentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            drill: true,
            model: "jev-latest".into(),
            base_url: "https://api.typesafe.ai".into(),
            thresholds: Thresholds::default(),
            per_minute: 10,
            per_day: 200,
            cache_max_entries: 5000,
            max_candidates_per_tick: 8,
            allow_hosts: Vec::new(),
            allow_overrides: Vec::new(),
            store_context_in_audit: false,
        }
    }
}

impl IntentConfig {
    /// 阈值的人读表示（进指纹）。
    pub fn thresholds_repr(&self) -> String {
        let mut cats: Vec<&str> = self.thresholds.block_categories.iter().map(|c| c.as_str()).collect();
        cats.sort_unstable();
        format!(
            "ads_min={:.4};choice_min={:.4};risk_max={:.4};bonus_max={:.4};cats={}",
            self.thresholds.ads_intent_min,
            self.thresholds.choice_confidence_min,
            self.thresholds.risk_of_breakage_max,
            self.thresholds.shape_bonus_max,
            cats.join(",")
        )
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.model, &self.base_url, &self.thresholds_repr())
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        if let Err(e) = self.thresholds.validate() {
            errs.extend(e);
        }
        if self.model.trim().is_empty() {
            errs.push("model 不能为空".into());
        }
        if !self.base_url.starts_with("https://") {
            errs.push("base_url 必须是 https://".into());
        }
        if self.cache_max_entries == 0 {
            errs.push("cache_max_entries 不能为 0".into());
        }
        if self.max_candidates_per_tick == 0 {
            errs.push("max_candidates_per_tick 不能为 0".into());
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    pub fn rule_options(&self) -> RuleOptions {
        RuleOptions {
            allow_overrides: self.allow_overrides.clone(),
            ..Default::default()
        }
    }
}

/// 一次 [`IntentEngine::classify_pending`] 的账。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassifyReport {
    pub candidates: u32,
    pub cache_hits: u32,
    pub asked: u32,
    pub blocked: u32,
    pub allowed: u32,
    pub deferred: u32,
    pub budget_denied: u32,
    pub gateway_errors: u32,
    pub schema_invalid: u32,
    pub requeued: u32,
}

/// 界面上"当前状态"用的快照。
#[derive(Debug, Clone, PartialEq)]
pub struct EngineStats {
    pub enabled: bool,
    pub drill: bool,
    pub gateway: String,
    pub fingerprint: String,
    pub cache_len: usize,
    pub cache_load: CacheLoadOutcome,
    pub pending: usize,
    pub tracked: usize,
    pub block_rules: usize,
    pub allow_rules: usize,
    pub applied_hash: String,
    pub budget: BudgetSnapshot,
    pub observer: ObserverStats,
    pub gateway_calls: u64,
    pub gateway_errors: u64,
    pub cache_hits: u64,
    pub audit_written: u64,
}

/// 判定引擎。
pub struct IntentEngine<G: Gateway> {
    config: IntentConfig,
    gateway: G,
    cache: VerdictCache,
    budget: Budget,
    audit: AuditLog,
    observer: Observer,
    data_root: Option<PathBuf>,
    cache_load: CacheLoadOutcome,
    gateway_calls: u64,
    gateway_errors: u64,
    cache_hits: u64,
}

impl<G: Gateway> IntentEngine<G> {
    /// 建引擎。`data_root` 为 `None` 时全部在内存里（测试用）。
    pub fn new(
        config: IntentConfig,
        gateway: G,
        data_root: Option<PathBuf>,
        now: u64,
    ) -> Result<Self, Vec<String>> {
        config.validate()?;
        let fp = config.fingerprint();
        let (cache, cache_load) = match &data_root {
            Some(root) => VerdictCache::load(
                &VerdictCache::default_path(root),
                &fp,
                config.cache_max_entries,
                now,
            ),
            None => (VerdictCache::new(fp.clone(), config.cache_max_entries), CacheLoadOutcome::Missing),
        };
        let audit = match &data_root {
            Some(root) => AuditLog::new(AuditLog::default_path(root), 4 * 1024 * 1024),
            None => AuditLog::disabled(),
        };
        let mut observer = Observer::default();
        observer.set_allow_hosts(config.allow_hosts.clone());
        Ok(Self {
            budget: Budget::new(config.per_minute, config.per_day),
            config,
            gateway,
            cache,
            audit,
            observer,
            data_root,
            cache_load,
            gateway_calls: 0,
            gateway_errors: 0,
            cache_hits: 0,
        })
    }

    pub fn config(&self) -> &IntentConfig {
        &self.config
    }

    pub fn cache(&self) -> &VerdictCache {
        &self.cache
    }

    pub fn cache_load(&self) -> &CacheLoadOutcome {
        &self.cache_load
    }

    pub fn fingerprint(&self) -> &str {
        self.cache.fingerprint()
    }

    pub fn gateway(&self) -> &G {
        &self.gateway
    }

    /// 改配置。**指纹变了就整库作废**（内存 + 下次落盘），并返回要记进日志的说明。
    pub fn reconfigure(&mut self, config: IntentConfig) -> Result<Vec<String>, Vec<String>> {
        config.validate()?;
        let mut notes = Vec::new();
        let new_fp = config.fingerprint();
        if new_fp != *self.cache.fingerprint() {
            let dropped = self.cache.clear();
            self.cache = VerdictCache::new(new_fp.clone(), config.cache_max_entries);
            notes.push(format!(
                "模型/网关/阈值/措辞变化 ⇒ 判决缓存整体作废（丢了 {dropped} 条，指纹 {new_fp}）"
            ));
        } else if self.cache.len() < config.cache_max_entries {
            // 上限调大时允许重载磁盘上的旧缓存。
        }
        self.observer.set_allow_hosts(config.allow_hosts.clone());
        self.budget = Budget::new(config.per_minute, config.per_day);
        self.config = config;
        Ok(notes)
    }

    /// 吃一条连接记录（**便宜、同步，数据面路径上不该有别的开销**）。
    pub fn observe(&mut self, rec: &ConnectionRecord, now: u64) -> ObserveOutcome {
        if !self.config.enabled {
            return ObserveOutcome::Skipped("disabled");
        }
        let outcome = self.observer.observe(rec, now);
        // 只要这个主机名是"我们认识的候选"，就问一句缓存：
        // * 有未过期的判决 ⇒ 记一次命中，**不进队列**（不重复审计、不重复花钱）；
        // * 没有（从没判过 / 已过期 / 被预算挡过 / 网关失败过）⇒ 重新排队，稍后再问。
        //
        // 这条路径在数据面上（每行日志一次），所以只做一次 BTreeMap 查找。
        if matches!(outcome, ObserveOutcome::Discovered(_) | ObserveOutcome::Updated) {
            if let Some(host) = rec.domain.as_deref().map(crate::rules::normalize) {
                if self.cache.get(&host, now).is_some() {
                    self.cache_hits += 1;
                } else {
                    self.observer.requeue(&host);
                }
            }
        }
        outcome
    }

    pub fn pending(&self) -> usize {
        self.observer.pending()
    }

    /// 跑一轮判定。调用方应在后台任务里按节拍调用（例如每 10 秒一次）。
    pub fn classify_pending(&mut self, now: u64) -> ClassifyReport {
        let mut report = ClassifyReport::default();
        if !self.config.enabled {
            return report;
        }
        let batch = self.observer.drain_pending(self.config.max_candidates_per_tick);
        for candidate in batch {
            report.candidates += 1;
            let host = candidate.host.clone();

            // 1) 缓存：命中就零成本。
            //
            // 正常情况下走到这里的候选都已经在 `observe()` 里被缓存挡过一次了；
            // 能进来的只有"排队期间刚被判过"这种边角情况。**不写审计** ——
            // 缓存命中不是一次新的判决，写进去只会把审计刷成噪音。
            if self.cache.get(&host, now).is_some() {
                self.cache_hits += 1;
                report.cache_hits += 1;
                continue;
            }

            // 2) 预算：超了**放行**，并把候选放回队列等下一个窗口。
            if let Err(exhausted) = self.budget.try_consume(now) {
                let scope = exhausted.scope().to_string();
                let verdict = Verdict::Deferred(DeferReason::BudgetExhausted { scope });
                report.budget_denied += 1;
                report.deferred += 1;
                self.observer.requeue(&host);
                report.requeued += 1;
                // 预算耗尽是**暂时**状态，不进缓存（否则会在预算恢复后继续放行）。
                self.record(&candidate, &verdict, now, false, None);
                debug!(host = %host, "意图判定：预算用尽，放行并排队重试");
                continue;
            }

            // 3) 网关。
            let request = domain_request(
                &FlowContext {
                    host: host.clone(),
                    port: candidate.ports.iter().next().copied(),
                    network: candidate.networks.iter().next().cloned().unwrap_or_default(),
                    inbound: candidate.inbounds.iter().next().cloned().unwrap_or_default(),
                    process: None,
                    seen_before: candidate.connections > 1,
                    // L4 拿不到"上一跳页面"，**不许编**。
                    page_host: None,
                    note: None,
                },
                &self.config.model,
            );
            self.gateway_calls += 1;
            report.asked += 1;
            match self.gateway.ask(&request) {
                Err(e) => {
                    self.gateway_errors += 1;
                    report.gateway_errors += 1;
                    report.deferred += 1;
                    self.observer.requeue(&host);
                    report.requeued += 1;
                    let verdict = Verdict::Deferred(DeferReason::GatewayUnavailable {
                        message: e.to_string(),
                    });
                    // 网关故障不是这个域名的属性 ⇒ 不进缓存，下一轮再试。
                    self.record(&candidate, &verdict, now, false, Some(&request.state));
                    if e.is_transient() {
                        debug!(host = %host, err = %e, "意图判定：网关失败（可重试）");
                    } else {
                        warn!(host = %host, err = %e, "意图判定：网关失败");
                    }
                    continue;
                }
                Ok(response) => {
                    // 4) 答案完整性：少一个 id 都算失败。
                    let Some(_complete) = request.read_answers(&response.answers) else {
                        let missing = request
                            .ids()
                            .into_iter()
                            .find(|id| !response.answers.has(id))
                            .unwrap_or("<unknown>")
                            .to_string();
                        report.schema_invalid += 1;
                        report.deferred += 1;
                        let verdict =
                            Verdict::Deferred(DeferReason::MissingAnswer { id: missing });
                        self.store(&candidate, &verdict, now, SCHEMA_FAILURE_TTL_SECS, &response.model);
                        self.record(&candidate, &verdict, now, true, Some(&request.state));
                        continue;
                    };

                    // 5) 闸门。
                    let bonus = candidate.shape.bonus(self.config.thresholds.shape_bonus_max);
                    let verdict = decide(&response.answers, &self.config.thresholds, bonus);
                    match &verdict {
                        Verdict::Block(_) => report.blocked += 1,
                        Verdict::Allow(_) => report.allowed += 1,
                        Verdict::Deferred(_) => report.deferred += 1,
                    }
                    self.store(&candidate, &verdict, now, ttl_for(&verdict), &response.model);
                    self.record(&candidate, &verdict, now, true, Some(&request.state));
                }
            }
        }
        report
    }

    fn store(&mut self, candidate: &crate::observer::Candidate, verdict: &Verdict, now: u64, ttl: u64, model: &Option<String>) {
        self.cache.put(CacheEntry {
            host: candidate.host.clone(),
            verdict: verdict.clone(),
            decided_at_unix: now,
            expires_at_unix: now.saturating_add(ttl),
            hits: 0,
            model: model.clone(),
        });
    }

    /// 写审计。`cache_hit` 与 `context` 由调用方给。
    fn record(
        &mut self,
        candidate: &crate::observer::Candidate,
        verdict: &Verdict,
        now: u64,
        cache_hit: bool,
        state: Option<&str>,
    ) {
        let evidence = verdict.block_evidence();
        let rec = AuditRecord {
            ts_unix: now,
            host: candidate.host.clone(),
            outcome: verdict.kind_str().to_string(),
            reason: verdict.reason_str().map(str::to_string),
            category: evidence.map(|b| b.category.as_str().to_string()),
            ads_intent: evidence.map(|b| b.ads_intent),
            risk_of_breakage: evidence.map(|b| b.risk_of_breakage),
            choice_confidence: evidence.map(|b| b.choice_confidence),
            effective_min: evidence.map(|b| b.effective_min),
            applied: self.applies(verdict),
            cache_hit,
            model: self.config.model.clone().into(),
            usage: None,
            context_sent: if self.config.store_context_in_audit {
                state.map(str::to_string)
            } else {
                None
            },
        };
        if let Err(e) = self.audit.append(&rec) {
            // 审计写不进去不该打断判定 —— 但要留痕。
            warn!(error = %e, "意图判定：审计写入失败");
        }
    }

    /// 这条判决**真的**变成了规则吗？
    fn applies(&self, verdict: &Verdict) -> bool {
        !self.config.drill && verdict.is_block()
    }

    /// 当前应该生效的规则。
    ///
    /// * 演练模式 / 功能关闭 ⇒ **只返回放行带**（用户纠正仍然生效），没有 block。
    /// * 除此之外 ⇒ 缓存里所有 `Block` 判决 + 用户放行带。
    pub fn rules(&self) -> IntentRules {
        let opts = self.config.rule_options();
        if self.config.enabled && !self.config.drill {
            materialize_from_cache(self.cache.entries(), &opts)
        } else {
            // 关闭或演练模式：只有用户放行带，一条 block 都没有。
            // 演练模式下"本该拦谁"由 [`Self::preview_rules`] 提供 ——
            // 它**不会**被下发给核心。
            materialize_from_cache(std::iter::empty::<&CacheEntry>(), &opts)
        }
    }

    /// 演练模式用：**本该**拦谁（不会下发给核心）。界面与评测夹具用它。
    pub fn preview_rules(&self) -> IntentRules {
        materialize_from_cache(self.cache.entries(), &self.config.rule_options())
    }

    /// 生效集合的哈希。**只有它变化才值得重启核心。**
    pub fn applied_hash(&self) -> String {
        self.rules().domain_set_hash()
    }

    /// 用户纠正一个误杀。返回"是否真的改变了什么"（没变就不该触发重启）。
    pub fn set_allow_override(&mut self, host: &str, action: AllowAction) -> bool {
        let host = crate::rules::normalize(host);
        if crate::rules::valid_host(&host).is_err() {
            return false;
        }
        let unchanged = self
            .config
            .allow_overrides
            .iter()
            .any(|o| crate::rules::normalize(&o.host) == host && o.action == action);
        self.config.allow_overrides.retain(|o| crate::rules::normalize(&o.host) != host);
        self.config.allow_overrides.push(AllowOverride { host, action });
        !unchanged
    }

    /// 撤销放行。返回是否有过。
    pub fn clear_allow_override(&mut self, host: &str) -> bool {
        let host = crate::rules::normalize(host);
        let before = self.config.allow_overrides.len();
        self.config.allow_overrides.retain(|o| crate::rules::normalize(&o.host) != host);
        before != self.config.allow_overrides.len()
    }

    /// 某个域名为什么是这个结论（界面上的"解释"）。
    pub fn explain(&self, host: &str) -> Option<CacheEntry> {
        self.explain_at(host, xt_core::util::now_unix())
    }

    /// 同上，但用调用方给定的时间（测试与离线夹具）。**过期判决返回 `None`**，
    /// 不许把一个已经失效的结论当成当前结论展示。
    pub fn explain_at(&self, host: &str, now: u64) -> Option<CacheEntry> {
        let host = crate::rules::normalize(host);
        self.cache.peek(&host, now).cloned()
    }

    /// 一次性放行并清掉它的判决缓存（用户点"这个拦错了"）。
    pub fn allow_now(&mut self, host: &str, action: AllowAction) -> bool {
        let host = crate::rules::normalize(host);
        self.cache.remove(&host);
        self.set_allow_override(&host, action)
    }

    pub fn persist_cache(&self) -> std::io::Result<()> {
        match &self.data_root {
            Some(root) => self.cache.save(&VerdictCache::default_path(root)),
            None => Ok(()),
        }
    }

    pub fn purge_expired(&mut self, now: u64) -> usize {
        self.cache.purge_expired(now)
    }

    pub fn clear_cache(&mut self) -> usize {
        self.cache.clear()
    }

    pub fn stats(&mut self, now: u64) -> EngineStats {
        let rules = self.rules();
        EngineStats {
            enabled: self.config.enabled,
            drill: self.config.drill,
            gateway: self.gateway.describe(),
            fingerprint: self.fingerprint().to_string(),
            cache_len: self.cache.len(),
            cache_load: self.cache_load.clone(),
            pending: self.observer.pending(),
            tracked: self.observer.tracked(),
            block_rules: rules.block.len(),
            allow_rules: rules.allow.len(),
            applied_hash: rules.domain_set_hash(),
            budget: self.budget.snapshot(now),
            observer: self.observer.stats.clone(),
            gateway_calls: self.gateway_calls,
            gateway_errors: self.gateway_errors,
            cache_hits: self.cache_hits,
            audit_written: self.audit.written,
        }
    }
}

fn ttl_for(verdict: &Verdict) -> u64 {
    match verdict {
        Verdict::Block(_) => BLOCK_TTL_SECS,
        Verdict::Allow(_) => ALLOW_TTL_SECS,
        Verdict::Deferred(DeferReason::SchemaInvalid { .. })
        | Verdict::Deferred(DeferReason::MissingAnswer { .. }) => SCHEMA_FAILURE_TTL_SECS,
        Verdict::Deferred(_) => DEFERRED_TTL_SECS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{scripted_response, GatewayError, ScriptedGateway};
    use crate::question::{KIND_AD_OR_MONETIZATION, KIND_CDN_OR_INFRA};
    use crate::verdict::AllowReason;

    fn conn(domain: &str, port: u16, network: &str) -> ConnectionRecord {
        ConnectionRecord {
            ts_ms: 0,
            ts_text: "x".into(),
            from: "198.18.0.1:1".into(),
            network: network.into(),
            target_host: "203.0.113.9".into(),
            target_port: Some(port),
            inbound_tag: "tun".into(),
            outbound_tag: "node-a".into(),
            domain: Some(domain.into()),
            domain_paired: true,
            domain_pair_delta_us: Some(10),
            sniff_id: None,
        }
    }

    fn enabled(drill: bool) -> IntentConfig {
        IntentConfig { enabled: true, drill, ..Default::default() }
    }

    fn engine(gateway: ScriptedGateway, cfg: IntentConfig) -> IntentEngine<ScriptedGateway> {
        IntentEngine::new(cfg, gateway, None, 1_000).unwrap()
    }

    #[test]
    fn a_block_verdict_becomes_exactly_one_rule_and_is_cached() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));

        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        let report = e.classify_pending(1_001);
        assert_eq!(report.asked, 1);
        assert_eq!(report.blocked, 1);

        let rules = e.rules();
        assert_eq!(rules.block.len(), 1);
        assert_eq!(rules.block[0].id, "intent-block-ads.example");
        assert_eq!(e.gateway().call_count(), 1);

        // 第二条连接命中缓存：不再花钱，也不再进队列。
        e.observe(&conn("ads.example", 443, "tcp"), 1_100);
        let again = e.classify_pending(1_101);
        assert_eq!(again.candidates, 0);
        assert_eq!(again.asked, 0);
        assert_eq!(e.gateway().call_count(), 1, "同一域名只问一次");
        assert_eq!(e.stats(1_101).cache_hits, 1, "缓存命中要计入「省了多少次请求」");
    }

    #[test]
    fn drill_mode_records_but_never_blocks() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(true));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);

        assert!(e.rules().block.is_empty(), "演练模式不许生成 block 规则");
        // 但"本该拦谁"是可见的 —— 这正是用户看几天审计再决定的意义。
        assert_eq!(e.preview_rules().block.len(), 1);
    }

    #[test]
    fn a_disabled_engine_does_no_work() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 1.0, 0.0, 1.0));
        let mut e = engine(g, IntentConfig::default());
        assert_eq!(
            e.observe(&conn("ads.example", 443, "tcp"), 1_000),
            ObserveOutcome::Skipped("disabled")
        );
        let report = e.classify_pending(1_001);
        assert_eq!(report.candidates, 0);
        assert_eq!(e.gateway().call_count(), 0);
    }

    #[test]
    fn gateway_failure_is_fail_open_and_retried_later() {
        let g = ScriptedGateway::always_failing(GatewayError::Timeout);
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        let report = e.classify_pending(1_001);
        assert_eq!(report.gateway_errors, 1);
        assert_eq!(report.requeued, 1);
        assert!(e.rules().block.is_empty(), "网关坏了绝不能拦任何东西");
        assert_eq!(e.pending(), 1, "失败的要排队重试");

        // 下一轮它又被问了 —— 网关故障不是这个域名的属性。
        let report = e.classify_pending(1_010);
        assert_eq!(report.gateway_errors, 1);
        assert_eq!(e.gateway().call_count(), 2);
    }

    #[test]
    fn budget_exhaustion_is_fail_open_and_does_not_poison_the_cache() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut cfg = enabled(false);
        cfg.per_minute = 1;
        cfg.per_day = 10;
        let mut e = engine(g, cfg);

        e.observe(&conn("a.example", 443, "tcp"), 1_000);
        e.observe(&conn("b.example", 443, "tcp"), 1_000);
        let report = e.classify_pending(1_001);
        assert_eq!(report.asked, 1);
        assert_eq!(report.budget_denied, 1);
        assert_eq!(e.gateway().call_count(), 1);
        // 花过钱的那条正常定罪；被预算挡住的那条**没有任何判决**。
        let rules = e.rules();
        assert_eq!(rules.block.len(), 1);
        assert_eq!(rules.block[0].id, "intent-block-a.example");
        assert!(e.explain_at("b.example", 1_001).is_none(), "预算耗尽不能变成一条缓存判决");
        assert_eq!(e.pending(), 1);
    }

    #[test]
    fn an_incomplete_answer_is_deferred_and_cached_briefly() {
        // 服务端只答了两个问题（缺 risk_of_breakage）。
        let partial = crate::gateway::GatewayResponse::from_wire(&serde_json::json!({
            "model": "m",
            "answers": {
                crate::question::Q_ENDPOINT_KIND: { "type": "choice", "choice": KIND_AD_OR_MONETIZATION, "confidence": 0.99 },
                crate::question::Q_ADS_INTENT: { "type": "noul", "noul": 0.99 }
            }
        }))
        .unwrap();
        let g = ScriptedGateway::always(partial);
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        let report = e.classify_pending(1_001);
        assert_eq!(report.schema_invalid, 1);
        assert!(e.rules().block.is_empty());

        // 短期内不再重问（避免对着坏掉的服务端猛发请求）。
        e.observe(&conn("ads.example", 443, "tcp"), 1_100);
        let again = e.classify_pending(1_101);
        assert_eq!(again.candidates, 0);
        assert_eq!(e.gateway().call_count(), 1);
    }

    #[test]
    fn an_allow_verdict_produces_nothing_at_all() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_CDN_OR_INFRA, 0.9, 0.1, 0.99));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("cdn.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        assert!(e.rules().is_empty(), "不拦就是不在名单里，不该生成任何规则");
        assert_eq!(
            e.explain_at("cdn.example", 1_001).unwrap().verdict,
            Verdict::Allow(AllowReason::CategoryNotBlockable {
                category: crate::verdict::Category::CdnOrInfra
            })
        );
    }

    #[test]
    fn a_user_override_beats_an_existing_block() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        assert_eq!(e.rules().block.len(), 1);

        let before = e.applied_hash();
        assert!(e.allow_now("ads.example", AllowAction::Direct));
        let rules = e.rules();
        assert!(rules.block.is_empty(), "用户纠正之后不可能再被拦");
        assert_eq!(rules.allow.len(), 1);
        assert_ne!(before, e.applied_hash(), "生效集合变了 ⇒ 哈希必须变（否则不会重启，规则不生效）");
    }

    #[test]
    fn the_applied_hash_is_stable_for_the_same_set() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        let a = e.applied_hash();
        // 再跑一轮（无新域名）不该改变生效集合。
        e.classify_pending(2_000);
        assert_eq!(a, e.applied_hash());
    }

    #[test]
    fn changing_the_model_invalidates_every_cached_verdict() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        assert_eq!(e.cache().len(), 1);

        let mut cfg = e.config().clone();
        cfg.model = "jev-1.13-free".into();
        let notes = e.reconfigure(cfg).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("整体作废"), "{notes:?}");
        assert_eq!(e.cache().len(), 0, "换模型后旧判决一条都不许留");
    }

    #[test]
    fn reconfigure_with_the_same_inputs_keeps_the_cache() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        let cfg = e.config().clone();
        let notes = e.reconfigure(cfg).unwrap();
        assert!(notes.is_empty());
        assert_eq!(e.cache().len(), 1);
    }

    #[test]
    fn invalid_config_is_refused_with_every_reason() {
        let mut cfg = enabled(false);
        cfg.model = "  ".into();
        cfg.base_url = "http://insecure.example".into();
        cfg.cache_max_entries = 0;
        let errs = match IntentEngine::new(cfg, ScriptedGateway::always_failing(GatewayError::Timeout), None, 0)
        {
            Ok(_) => panic!("非法配置必须被拒绝"),
            Err(e) => e,
        };
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn stats_describe_the_current_state() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(false));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        let s = e.stats(1_100);
        assert!(s.enabled && !s.drill);
        assert_eq!(s.block_rules, 1);
        assert_eq!(s.cache_len, 1);
        assert_eq!(s.gateway_calls, 1);
        assert_eq!(s.budget.total_calls, 1);
        assert_eq!(s.pending, 0);
        assert_eq!(s.gateway, "scripted(scripted)");
        assert!(!s.fingerprint.is_empty());
    }

    #[test]
    fn mode_switching_from_drill_to_live_takes_effect_immediately() {
        let g = ScriptedGateway::always(scripted_response("m", KIND_AD_OR_MONETIZATION, 0.97, 0.05, 0.93));
        let mut e = engine(g, enabled(true));
        e.observe(&conn("ads.example", 443, "tcp"), 1_000);
        e.classify_pending(1_001);
        assert!(e.rules().block.is_empty());

        let mut cfg = e.config().clone();
        cfg.drill = false;
        e.reconfigure(cfg).unwrap();
        assert_eq!(e.rules().block.len(), 1, "关掉演练后，已经攒下的判决应当立刻生效");
    }

    #[test]
    fn ttl_selection_matches_the_policy() {
        let block = Verdict::Block(crate::verdict::BlockVerdict {
            category: crate::verdict::Category::AdOrMonetization,
            ads_intent: 1.0,
            risk_of_breakage: 0.0,
            choice_confidence: 1.0,
            effective_min: 0.85,
        });
        assert_eq!(ttl_for(&block), BLOCK_TTL_SECS);
        assert_eq!(ttl_for(&Verdict::Allow(AllowReason::BelowThreshold { ads_intent: 0.1, effective_min: 0.85 })), ALLOW_TTL_SECS);
        assert_eq!(ttl_for(&Verdict::Deferred(DeferReason::GatewayUnavailable { message: "x".into() })), DEFERRED_TTL_SECS);
        assert_eq!(ttl_for(&Verdict::Deferred(DeferReason::MissingAnswer { id: "x".into() })), SCHEMA_FAILURE_TTL_SECS);
    }
}
