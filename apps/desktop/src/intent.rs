//! 意图过滤的桌面运行态。
//!
//! # 本切片**刻意**只做三件事
//!
//! 1. 从核心日志里挑候选端点（`observe`）；
//! 2. 后台按节拍判定并缓存（`tick`）；
//! 3. 把每一步写进审计文件。
//!
//! **不下发任何路由规则、不重启核心、不改任何系统网络配置。** 这不是"还没做完"
//! 的借口，而是一条刻意的推进顺序：先把"判定对不对"用真实流量量出来（演练模式），
//! 再谈"要不要拦"。所以这一版即使开了功能，也**不可能**让用户的网出问题 ——
//! 它连一条规则都不生成。
//!
//! 规则下发与 `RoutingService.AddRule` 免重启热加是下一步（设计文档 §10 的 P2b/P2c）。
//!
//! # 为什么引擎可以整个重建
//!
//! 设置一变（换预设/换模型/改阈值），缓存指纹就变、旧判决必须作废。
//! [`IntentRuntime::follow_settings`] 因此是**重建**而不是"就地改字段"：
//! 少一条"忘了同步哪个字段"的路径。
//!
//! # 密钥
//!
//! 目前**只支持免密钥的 Zen 预设**：`IntentSettings.api_key_ref` 的 Keychain
//! 读写是独立的一步（`store.rs` 已约定 `keychain:<service>/<account>` 的形式，
//! 但实现还没落地）。需要密钥的预设会在 `config_from_settings` 里被明确拒绝，
//! 并在界面上给出"改用 Zen"的提示 —— **不静默失败**。

use std::path::PathBuf;
use std::time::Duration;

use xt_core::model::AppSettings;
use xt_core::xray::access_log::ConnectionRecord;
use xt_intent::engine::{ClassifyReport, EngineStats, IntentEngine};
// `describe` 来自这个 trait —— 忘了它时编译器只会说"没有这个方法"。
use xt_intent::gateway::Gateway;
use xt_intent::transport::TlsTransport;
use xt_intent::{config_from_settings, JevConfig, JevGateway};

/// 判定节拍。10 秒一次与看门狗的探测节拍一致 —— 不为了"更快拦到广告"
/// 把判定频率堆上去，因为首访本来就放行（设计文档 §3.2）。
pub const TICK_INTERVAL: Duration = Duration::from_secs(10);

/// 缓存落盘的节拍（每 N 次 tick 一次，避免每隔 10 秒写一次磁盘）。
const PERSIST_EVERY_TICKS: u64 = 6;

/// 具体网关类型。写成别名是因为它出现在结构体字段里，
/// 换传输实现时只改这一行。
type Engine = IntentEngine<JevGateway<TlsTransport>>;

/// 意图过滤运行态。
pub struct IntentRuntime {
    engine: Option<Engine>,
    data_root: PathBuf,
    ticks: u64,
    /// 最近一次判定的一轮账（界面/日志用）。
    pub last_report: Option<ClassifyReport>,
    /// 最近一次统计快照。
    pub last_stats: Option<EngineStats>,
    /// 跟随设置时的说明（"缓存整库作废""缓存从磁盘恢复 12 条"这类）。
    pub notes: Vec<String>,
    /// 最近的配置/运行错误（**不阻断任何东西**，只给界面看）。
    pub last_error: Option<String>,
    /// 引擎建起来的时刻。
    pub built_at_unix: Option<u64>,
}

impl std::fmt::Debug for IntentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentRuntime")
            .field("has_engine", &self.engine.is_some())
            .field("data_root", &self.data_root)
            .field("ticks", &self.ticks)
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl IntentRuntime {
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            engine: None,
            data_root,
            ticks: 0,
            last_report: None,
            last_stats: None,
            notes: Vec::new(),
            last_error: None,
            built_at_unix: None,
        }
    }

    pub fn has_engine(&self) -> bool {
        self.engine.is_some()
    }

    /// 按当前设置（重建）运行态。返回要写进 App 日志的说明。
    ///
    /// * 功能关闭 ⇒ 丢掉引擎（**不删磁盘缓存**：用户可能只是暂时关掉，
    ///   而重建一次缓存要重新花钱问一遍）；
    /// * 配置不合法/缺密钥 ⇒ 不建引擎，把原因记进 `notes` 与 `last_error`；
    /// * 成功 ⇒ 建引擎，并把缓存加载结果（命中多少条 / 为什么作废）写进 `notes`。
    pub fn follow_settings(&mut self, settings: &AppSettings, now: u64) -> Vec<String> {
        self.notes.clear();
        self.last_error = None;

        if !settings.intent.enabled {
            let dropped = self.engine.take().is_some();
            if dropped {
                self.notes.push("意图过滤已关闭（磁盘上的判决缓存保留）".into());
            }
            self.built_at_unix = None;
            return self.notes.clone();
        }

        // 密钥：本切片只支持免密钥预设，所以恒为 None —— 需要密钥的预设
        // 会在下面以**明确原因**被拒绝，而不是发一个必然 401 的请求。
        let config = match config_from_settings(&settings.intent, None) {
            Ok(c) => c,
            Err(errs) => {
                let msg = format!("意图过滤未启用：{}", errs.join("；"));
                self.notes.push(msg.clone());
                self.last_error = Some(msg);
                self.engine = None;
                self.built_at_unix = None;
                return self.notes.clone();
            }
        };

        let gateway_config = JevConfig {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            api_key: None,
            timeout: xt_intent::jev::DEFAULT_TIMEOUT,
            max_retries: 2,
        };
        let gateway = match JevGateway::new(gateway_config, TlsTransport::new()) {
            Ok(g) => g,
            Err(e) => {
                let msg = format!("意图过滤未启用：网关配置不合法（{e}）");
                self.notes.push(msg.clone());
                self.last_error = Some(msg);
                self.engine = None;
                self.built_at_unix = None;
                return self.notes.clone();
            }
        };

        match IntentEngine::new(config, gateway, Some(self.data_root.clone()), now) {
            Ok(engine) => {
                match engine.cache_load() {
                    xt_intent::cache::CacheLoadOutcome::Missing => {
                        self.notes.push("意图判决缓存：首次运行，还没有缓存".into())
                    }
                    xt_intent::cache::CacheLoadOutcome::Loaded { entries, dropped_expired } => self.notes.push(
                        format!("意图判决缓存：恢复 {entries} 条（丢掉 {dropped_expired} 条过期的）"),
                    ),
                    other => self.notes.push(format!(
                        "意图判决缓存：整体作废（{}）—— 换了模型/网关/阈值或缓存格式",
                        other.as_str()
                    )),
                }
                if engine.config().drill {
                    self.notes.push("演练模式：只记录本该拦谁，不下发拦截规则".into());
                } else {
                    // 本切片即使关掉演练也不下发规则 —— 必须说出来，不能让用户以为已经在拦了。
                    self.notes.push(
                        "⚠️ 本版只观察不拦截：判定结果只进审计与缓存，路由规则的下发是下一步"
                            .into(),
                    );
                }
                self.engine = Some(engine);
                self.built_at_unix = Some(now);
            }
            Err(errs) => {
                let msg = format!("意图过滤未启用：{}", errs.join("；"));
                self.notes.push(msg.clone());
                self.last_error = Some(msg);
                self.engine = None;
                self.built_at_unix = None;
            }
        }
        self.notes.clone()
    }

    /// 吃一条连接记录。**便宜、同步**，所以可以直接挂在核心日志转发循环上。
    pub fn observe(&mut self, rec: &ConnectionRecord, now: u64) {
        if let Some(engine) = self.engine.as_mut() {
            engine.observe(rec, now);
        }
    }

    /// 后台节拍：判定积压的候选 + 周期性落盘。返回这一轮的账（没引擎则 `None`）。
    pub fn tick(&mut self, now: u64) -> Option<ClassifyReport> {
        let engine = self.engine.as_mut()?;
        let report = engine.classify_pending(now);
        self.last_report = Some(report.clone());
        self.ticks += 1;
        if self.ticks % PERSIST_EVERY_TICKS == 0 {
            if let Err(e) = engine.persist_cache() {
                // 缓存写不进去只是"下次要重新问"，不该升级成功能故障。
                self.last_error = Some(format!("判决缓存落盘失败：{e}"));
            }
        }
        self.last_stats = Some(engine.stats(now));
        Some(report)
    }

    /// 一次性把最终状态也落盘（停止/退出前调用）。
    pub fn flush(&mut self) {
        if let Some(engine) = self.engine.as_ref() {
            if let Err(e) = engine.persist_cache() {
                self.last_error = Some(format!("判决缓存落盘失败：{e}"));
            }
        }
    }

    /// 给界面看的摘要（**不触发任何 IO**）。
    pub fn summary(&self) -> IntentSummary {
        let Some(engine) = self.engine.as_ref() else {
            return IntentSummary {
                active: false,
                note: self.last_error.clone().or_else(|| self.notes.first().cloned()),
                ..Default::default()
            };
        };
        let stats = self.last_stats.clone();
        IntentSummary {
            active: true,
            drill: engine.config().drill,
            enabled: engine.config().enabled,
            model: engine.config().model.clone(),
            gateway: engine.gateway().describe(),
            fingerprint: engine.fingerprint().to_string(),
            pending: engine.pending(),
            cache_len: engine.cache().len(),
            built_at_unix: self.built_at_unix,
            block_rules: stats.as_ref().map(|s| s.block_rules).unwrap_or(0),
            allow_rules: stats.as_ref().map(|s| s.allow_rules).unwrap_or(0),
            gateway_calls: stats.as_ref().map(|s| s.gateway_calls).unwrap_or(0),
            gateway_errors: stats.as_ref().map(|s| s.gateway_errors).unwrap_or(0),
            cache_hits: stats.as_ref().map(|s| s.cache_hits).unwrap_or(0),
            blocked: engine.cache().entries().filter(|e| e.verdict.is_block()).count(),
            note: self.last_error.clone().or_else(|| self.notes.first().cloned()),
        }
    }

    /// 用户点"这个拦错了"（本切片：只登记放行纠正，因为还没有规则可撤销）。
    pub fn allow_now(&mut self, host: &str) -> bool {
        match self.engine.as_mut() {
            Some(engine) => engine.allow_now(host, xt_intent::rules::AllowAction::Direct),
            None => false,
        }
    }

    /// 某个域名的判决（界面的"为什么"）。
    pub fn explain(&self, host: &str) -> Option<xt_intent::cache::CacheEntry> {
        self.engine.as_ref().and_then(|e| e.explain(host))
    }
}

/// 界面/日志用的摘要。
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq)]
pub struct IntentSummary {
    pub active: bool,
    pub enabled: bool,
    /// 演练模式。
    pub drill: bool,
    pub model: String,
    pub gateway: String,
    pub fingerprint: String,
    pub pending: usize,
    pub cache_len: usize,
    pub built_at_unix: Option<u64>,
    /// **本版恒为 0**：本切片不下发规则。字段留着是为了让界面早点有地方显示它，
    /// 而不是等规则下发那一步再加。
    pub block_rules: usize,
    pub allow_rules: usize,
    pub gateway_calls: u64,
    pub gateway_errors: u64,
    pub cache_hits: u64,
    pub blocked: usize,
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::IntentPreset;

    fn root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xraytun-intent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn zen_settings() -> AppSettings {
        let mut s = AppSettings::default();
        s.intent.enabled = true;
        s.intent.preset = IntentPreset::Zen;
        s
    }

    #[test]
    fn a_disabled_feature_builds_no_engine_and_says_so() {
        let dir = root("disabled");
        let mut rt = IntentRuntime::new(dir.clone());
        let notes = rt.follow_settings(&AppSettings::default(), 100);
        assert!(!rt.has_engine());
        assert!(notes.is_empty(), "关着的时候不该有噪音：{notes:?}");
        assert!(!rt.summary().active);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_keyless_zen_setup_builds_an_engine_in_drill_mode() {
        let dir = root("zen");
        let mut rt = IntentRuntime::new(dir.clone());
        let notes = rt.follow_settings(&zen_settings(), 100);
        assert!(rt.has_engine(), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("首次运行")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("演练模式")), "{notes:?}");
        let s = rt.summary();
        assert!(s.active && s.enabled && s.drill);
        assert_eq!(s.model, "jev-1.13-free");
        // 本版一条规则都不下发。
        assert_eq!(s.block_rules, 0);
        assert_eq!(s.allow_rules, 0);
        assert_eq!(s.pending, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_preset_that_needs_a_key_is_refused_with_a_readable_reason() {
        let dir = root("keyed");
        let mut rt = IntentRuntime::new(dir.clone());
        let mut s = zen_settings();
        s.intent.preset = IntentPreset::Typesafe;
        let notes = rt.follow_settings(&s, 100);
        assert!(!rt.has_engine(), "缺密钥时不许建引擎（发了也是 401）");
        let err = rt.summary().note.unwrap_or_default();
        assert!(err.contains("Jev API Key"), "{err}");
        assert!(notes.iter().any(|n| n.contains("Zen")), "要给出可操作的下一步：{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turning_the_feature_off_keeps_the_cache_on_disk() {
        let dir = root("off-keeps");
        let mut rt = IntentRuntime::new(dir.clone());
        rt.follow_settings(&zen_settings(), 100);
        assert!(rt.has_engine());
        // 先落一次盘，这样"关掉之后缓存还在不在"才有东西可查。
        rt.flush();
        let cache_file = xt_intent::cache::VerdictCache::default_path(&dir);
        assert!(cache_file.is_file(), "flush 之后缓存文件必须存在");

        // 关掉：引擎丢掉，但磁盘上的缓存文件**不被删**。
        rt.follow_settings(&AppSettings::default(), 200);
        assert!(!rt.has_engine());
        assert!(cache_file.is_file(), "关闭功能不该删掉已经花过钱的判决缓存");
        assert!(rt.last_error.is_none(), "关闭是正常状态，不是错误");
        let note = rt.summary().note.unwrap_or_default();
        assert!(note.contains("关闭"), "关闭的原因要说出来：{note}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_enabled_engine_reports_a_tick_and_stays_fail_open() {
        let dir = root("tick");
        let mut rt = IntentRuntime::new(dir.clone());
        rt.follow_settings(&zen_settings(), 100);
        // 没有任何候选：这一轮应该是"零件事"，而且不许 panic。
        let report = rt.tick(110).expect("有引擎就该有一轮账");
        assert_eq!(report.candidates, 0);
        assert_eq!(report.asked, 0);
        assert_eq!(rt.tick(120).unwrap().candidates, 0);
        rt.flush();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn observe_without_an_engine_is_a_no_op() {
        let dir = root("noop");
        let mut rt = IntentRuntime::new(dir.clone());
        let rec = ConnectionRecord {
            ts_ms: 0,
            ts_text: String::new(),
            from: String::new(),
            network: "tcp".into(),
            target_host: "203.0.113.1".into(),
            target_port: Some(443),
            inbound_tag: "tun".into(),
            outbound_tag: "node-a".into(),
            domain: Some("ads.example".into()),
            domain_paired: true,
            domain_pair_delta_us: Some(1),
            sniff_id: None,
        };
        rt.observe(&rec, 1); // 不该 panic
        assert!(rt.tick(2).is_none(), "没引擎就没有一轮账");
        assert!(!rt.allow_now("ads.example"));
        assert!(rt.explain("ads.example").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebuilding_from_the_same_settings_keeps_the_cache() {
        let dir = root("rebuild");
        let mut rt = IntentRuntime::new(dir.clone());
        let s = zen_settings();
        rt.follow_settings(&s, 100);
        let fp1 = rt.summary().fingerprint;
        let notes = rt.follow_settings(&s, 200);
        assert!(rt.has_engine());
        assert_eq!(rt.summary().fingerprint, fp1);
        // 同一份设置重建时不该报"整库作废"。
        assert!(!notes.iter().any(|n| n.contains("整体作废")), "{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changing_the_model_invalidates_the_cache_and_says_so() {
        let dir = root("model-change");
        let mut rt = IntentRuntime::new(dir.clone());
        rt.follow_settings(&zen_settings(), 100);
        // **必须真的落盘**：指纹作废是在"从磁盘加载"那条路径上判定的，
        // 内存里的引擎重建并不会自己写盘（写盘由 flush/tick 触发）。
        rt.flush();
        let mut s = zen_settings();
        s.intent.model = "some-other-model".into();
        let notes = rt.follow_settings(&s, 200);
        assert!(rt.has_engine());
        assert!(notes.iter().any(|n| n.contains("整体作废")), "{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
