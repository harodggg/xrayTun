//! 设置 → 引擎配置的转换。
//!
//! # 为什么转换放在这里，而不是桌面层
//!
//! `IntentSettings` 在 `xt-core`，`IntentConfig` 在本 crate。两边都看得见的
//! 只有"同时依赖两者"的代码 —— 而 `xt-intent` 依赖 `xt-core`（反向会成环）。
//! 所以转换函数放这里，桌面层就只剩一行调用，**没有第二处需要同步的字段清单**。
//!
//! # 为什么要有那条"默认值交叉校验"测试
//!
//! `xt-core` 不能依赖本 crate，所以 `IntentThresholds` 是 [`crate::verdict::Thresholds`]
//! 的一份**镜像**。两份默认值一旦漂移，用户看到的是"界面写着 0.85、实际按别的数判"。
//! 镜像本身没法用类型系统消除，那就用测试钉住：字段同名、默认值逐个相等、
//! 枚举映射双向一致。

use xt_core::model::{IntentAllowAction, IntentCategory, IntentSettings, IntentThresholds};

use crate::question::FlowContext;
use crate::rules::{AllowAction, AllowOverride};
use crate::verdict::{Category, Thresholds};

use crate::engine::IntentConfig;

/// 把设置翻成引擎配置。`api_key` 由调用方从 Keychain 取出来再传进来 ——
/// 这个函数**永远不碰密钥存储**，所以它可以被完整单测。
///
/// 返回 `Err` 时是**全部**问题（不是第一个）：设置页要一次把问题都列出来。
pub fn config_from_settings(
    settings: &IntentSettings,
    api_key: Option<String>,
) -> Result<IntentConfig, Vec<String>> {
    let mut errs = settings.validate();

    let base_url = match settings.base_url() {
        Some(u) => u.to_string(),
        None => {
            errs.push("没有可用的网关地址（自建网关需要填 base URL）".into());
            String::new()
        }
    };

    // 只有"需要密钥"的预设才把密钥带进去；Zen 档即使配了 Key 也不用
    // （带上去反而可能在日志/报错里泄露一个用不到的凭据）。
    let api_key = if settings.preset.needs_key() { api_key.filter(|k| !k.trim().is_empty()) } else { None };
    if settings.preset.needs_key() && settings.enabled && api_key.is_none() {
        errs.push(format!(
            "预设「{}」需要 Jev API Key，但 Keychain 里读不到（引用是 {:?}）",
            settings.preset.as_str(),
            settings.api_key_ref
        ));
    }

    let config = IntentConfig {
        enabled: settings.enabled,
        drill: settings.drill,
        model: settings.model().to_string(),
        base_url,
        thresholds: thresholds_from(&settings.thresholds),
        per_minute: settings.per_minute,
        per_day: settings.per_day,
        cache_max_entries: settings.cache_max_entries,
        max_candidates_per_tick: 8,
        allow_hosts: settings.allow_hosts.clone(),
        allow_overrides: settings
            .allow_overrides
            .iter()
            .map(|o| AllowOverride {
                host: o.host.clone(),
                action: match o.action {
                    IntentAllowAction::Direct => AllowAction::Direct,
                    IntentAllowAction::Proxy => AllowAction::Proxy,
                },
            })
            .collect(),
        store_context_in_audit: settings.store_context_in_audit,
    };

    if config.enabled {
        if let Err(more) = config.validate() {
            errs.extend(more);
        }
        // 开了功能但连一次都问不出去，属于"配了个用不了的东西"，必须当场拦下。
        if settings.preset.needs_key() && api_key.is_none() {
            errs.push("意图过滤已开启，但没有可用的 Jev API Key".into());
        }
    }

    if errs.is_empty() {
        Ok(config)
    } else {
        errs.dedup();
        Err(errs)
    }
}

/// 阈值镜像 → 引擎阈值。
pub fn thresholds_from(t: &IntentThresholds) -> Thresholds {
    Thresholds {
        ads_intent_min: t.ads_intent_min,
        choice_confidence_min: t.choice_confidence_min,
        risk_of_breakage_max: t.risk_of_breakage_max,
        shape_bonus_max: t.shape_bonus_max,
        block_categories: t.block_categories.iter().map(|c| category_of(*c)).collect(),
    }
}

fn category_of(c: IntentCategory) -> Category {
    match c {
        IntentCategory::AdOrMonetization => Category::AdOrMonetization,
        IntentCategory::TrackerOrAnalytics => Category::TrackerOrAnalytics,
        IntentCategory::CdnOrInfra => Category::CdnOrInfra,
        IntentCategory::ApiOrService => Category::ApiOrService,
        IntentCategory::HumanSite => Category::HumanSite,
        IntentCategory::Unknown => Category::Unknown,
    }
}

/// 界面上的"为什么现在不能用"：把设置翻译成一句人话。
///
/// 返回 `None` 表示"配置没问题"。**不判断额度/网络** —— 那要真的问一次才知道。
pub fn readiness_note(settings: &IntentSettings) -> Option<String> {
    if !settings.enabled {
        return Some("未开启".into());
    }
    if settings.preset.needs_key() && settings.api_key_ref.trim().is_empty() {
        return Some(format!(
            "预设「{}」需要 Jev API Key；想零密钥试水请改用 Zen 预设",
            settings.preset.as_str()
        ));
    }
    if settings.base_url().is_none() {
        return Some("自建网关需要填一个 https:// 地址".into());
    }
    if settings.drill {
        return Some("演练模式：只记录本该拦谁，不下发拦截规则".into());
    }
    None
}

/// 观察阶段只需要一个"这是哪一类候选"的说明（审计与调试用）。
pub fn describe_flow(ctx: &FlowContext) -> String {
    let mut bits = vec![format!("host={}", ctx.host)];
    if let Some(p) = ctx.port {
        bits.push(format!("port={p}"));
    }
    if !ctx.network.is_empty() {
        bits.push(format!("net={}", ctx.network));
    }
    if !ctx.inbound.is_empty() {
        bits.push(format!("in={}", ctx.inbound));
    }
    bits.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::IntentPreset;

    fn enabled_zen() -> IntentSettings {
        IntentSettings {
            enabled: true,
            preset: IntentPreset::Zen,
            ..Default::default()
        }
    }

    /// **这条测试的意义**：`IntentThresholds` 是镜像，镜像漂移在界面上表现为
    /// "写着 0.85、实际按别的数判"。默认值必须逐个相等。
    #[test]
    fn the_threshold_mirror_matches_the_engine_defaults() {
        let mirror = thresholds_from(&IntentThresholds::default());
        assert_eq!(mirror, Thresholds::default(), "镜像与引擎默认值漂移了");
    }

    #[test]
    fn every_category_round_trips_through_the_mirror() {
        for c in [
            IntentCategory::AdOrMonetization,
            IntentCategory::TrackerOrAnalytics,
            IntentCategory::CdnOrInfra,
            IntentCategory::ApiOrService,
            IntentCategory::HumanSite,
            IntentCategory::Unknown,
        ] {
            let me = category_of(c);
            assert_eq!(
                me.as_str(),
                c.as_str(),
                "枚举映射不对称：{c:?} → {me:?}"
            );
        }
    }

    #[test]
    fn a_zen_setup_converts_without_a_key() {
        let c = config_from_settings(&enabled_zen(), None).unwrap();
        assert!(c.enabled);
        assert!(c.drill, "默认必须是演练模式");
        assert_eq!(c.base_url, "https://opencode.ai/zen");
        assert_eq!(c.model, "jev-1.13-free");
        assert_eq!(c.per_day, 200);
        assert_eq!(c.max_candidates_per_tick, 8);
        assert!(c.thresholds == Thresholds::default());
    }

    #[test]
    fn a_typesafe_setup_without_a_key_is_refused_at_configure_time() {
        let mut s = enabled_zen();
        s.preset = IntentPreset::Typesafe;
        s.api_key_ref = "keychain:com.xraytun.intent/jev-api-key".into();
        let errs = config_from_settings(&s, None).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("Jev API Key")),
            "必须在配置期就拒绝，而不是等到第一次判定失败：{errs:?}"
        );

        // 给了 Key 就能过。
        let c = config_from_settings(&s, Some("sk-test".into())).unwrap();
        assert_eq!(c.base_url, "https://api.typesafe.ai");
        assert_eq!(c.model, "jev-latest");
    }

    #[test]
    fn a_key_is_not_carried_for_a_keyless_preset() {
        // Zen 档即使 Keychain 里存了 Key，也不该把它传进引擎
        // （一个用不到的凭据只会多一个泄露面）。
        let mut s = enabled_zen();
        s.api_key_ref = "keychain:com.xraytun.intent/jev-api-key".into();
        let c = config_from_settings(&s, Some("sk-should-not-be-used".into())).unwrap();
        assert_eq!(c.model, "jev-1.13-free");
        // 引擎只认 base_url/model；密钥不进 IntentConfig（它在 JevConfig 里），
        // 所以这里断言的是"转换没有把密钥塞进任何字段"。
        assert!(!format!("{c:?}").contains("sk-should-not-be-used"));
    }

    #[test]
    fn a_custom_gateway_needs_an_https_url() {
        let mut s = enabled_zen();
        s.preset = IntentPreset::Custom;
        let errs = config_from_settings(&s, None).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("网关地址")), "{errs:?}");

        s.custom_base_url = "http://plain.example".into();
        let errs = config_from_settings(&s, None).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("https")), "{errs:?}");
    }

    #[test]
    fn a_disabled_setup_converts_without_complaining_about_keys() {
        // 关着的时候不该因为"没配 Key"报错 —— 用户只是还没开。
        let s = IntentSettings::default();
        let c = config_from_settings(&s, None).unwrap();
        assert!(!c.enabled);
        assert!(c.drill);
        assert_eq!(c.base_url, "https://api.typesafe.ai");
    }

    #[test]
    fn allow_overrides_and_whitelists_survive_the_conversion() {
        let mut s = enabled_zen();
        s.allow_hosts = vec!["good.example".into()];
        s.allow_overrides = vec![
            xt_core::model::IntentAllowOverride {
                host: "cdn.news.example".into(),
                action: IntentAllowAction::Direct,
            },
            xt_core::model::IntentAllowOverride {
                host: "video.example".into(),
                action: IntentAllowAction::Proxy,
            },
        ];
        let c = config_from_settings(&s, None).unwrap();
        assert_eq!(c.allow_hosts, vec!["good.example".to_string()]);
        assert_eq!(c.allow_overrides.len(), 2);
        assert_eq!(c.allow_overrides[0].action, AllowAction::Direct);
        assert_eq!(c.allow_overrides[1].action, AllowAction::Proxy);
    }

    #[test]
    fn custom_thresholds_are_carried_verbatim() {
        let mut s = enabled_zen();
        s.thresholds.ads_intent_min = 0.92;
        s.thresholds.risk_of_breakage_max = 0.1;
        s.thresholds.block_categories = vec![IntentCategory::TrackerOrAnalytics];
        let c = config_from_settings(&s, None).unwrap();
        assert_eq!(c.thresholds.ads_intent_min, 0.92);
        assert_eq!(c.thresholds.risk_of_breakage_max, 0.1);
        assert_eq!(c.thresholds.block_categories, vec![Category::TrackerOrAnalytics]);
    }

    #[test]
    fn invalid_thresholds_are_reported_with_the_other_problems() {
        let mut s = enabled_zen();
        s.thresholds.ads_intent_min = 5.0;
        s.cache_max_entries = 0;
        let errs = config_from_settings(&s, None).unwrap_err();
        assert!(errs.len() >= 2, "要一次报全，不是只报第一个：{errs:?}");
    }

    #[test]
    fn readiness_notes_say_something_useful_in_every_state() {
        assert_eq!(readiness_note(&IntentSettings::default()).as_deref(), Some("未开启"));

        let mut s = enabled_zen();
        assert!(readiness_note(&s).unwrap().contains("演练模式"));

        s.drill = false;
        assert!(readiness_note(&s).is_none(), "Zen + 非演练 = 配置上没问题");

        let mut keyed = enabled_zen();
        keyed.preset = IntentPreset::Typesafe;
        assert!(readiness_note(&keyed).unwrap().contains("API Key"));

        let mut custom = enabled_zen();
        custom.preset = IntentPreset::Custom;
        // 先补上密钥引用，否则"缺 Key"会先被报出来（两个问题都在时，Key 在前）。
        custom.api_key_ref = "keychain:com.xraytun.intent/jev-api-key".into();
        assert!(readiness_note(&custom).unwrap().contains("https://"));
    }

    #[test]
    fn describe_flow_never_mentions_a_field_we_do_not_have() {
        let ctx = FlowContext {
            host: "a.example".into(),
            port: Some(443),
            network: "tcp".into(),
            inbound: "tun".into(),
            process: Some("Safari".into()),
            ..Default::default()
        };
        let d = describe_flow(&ctx);
        assert!(d.contains("host=a.example"));
        assert!(d.contains("port=443"));
        // 进程名不在里面 —— 上游稳定版 macOS 拿不到它，这里也就不该假装有。
        assert!(!d.contains("Safari"), "{d}");
    }
}
