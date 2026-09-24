//! 真实核心验收：意图规则**真的**能被 Xray 接受，而且顺序真的对。
//!
//! # 为什么必须有这一条
//!
//! `xt-intent` 的 94 个单测全部在**没有网络、没有核心**的条件下跑。它们能证明
//! 判决逻辑对，但证明不了"生成的这段 JSON 核心愿意加载"。这两件事之间的差距，
//! 本项目已经吃过一次亏：v0.8.37 的 P0 就是**配置合法但核心拒启**
//! （`duplicate ruleTag preset-private`）。
//!
//! # 正负对照（缺一不可）
//!
//! * **正**：带意图规则的配置 ⇒ 核心 `exit 0`；
//! * **负**：人为把两条规则的 `ruleTag` 改成一样 ⇒ 核心**必须**拒载。
//!
//! 只有正例时，一条"核心压根没读这个文件"的假绿也能通过；负例把这种可能性钉死。
//!
//! # 没有核心时这条用例会 SKIP 并打印原因
//!
//! 与 `rule_tag_uniqueness.rs` 同一约定：核心路径从 `XT_CORE` 取，
//! 否则找 `<repo>/apps/desktop/binaries/xray`。SKIP 时**打印**而不是静默通过 ——
//! 静默通过会让"这条验收其实没跑"一直没人发现。
//!
//! ```bash
//! XT_CORE=/path/to/xray cargo test -p xt-intent --test real_core -- --nocapture
//! ```
//!
//! # ⚠️ 版本敏感：`ruleTag` 唯一性的检查是**新核心才有的**
//!
//! 本机实测（同一份重复 `ruleTag` 的配置）：
//!
//! | 核心 | `run -test` | 真实 `run` |
//! |---|---|---|
//! | **26.9.9**（`apps/desktop/binaries/xray`，本项目的目标版本） | exit 0 | **拒载**，报 `duplicate ruleTag <tag>` |
//! | 26.3.27（旧 stable，例如 `.scratch/xray-server/xray`） | exit 0 | **接受并正常启动** |
//!
//! 也就是说：把 `XT_CORE` 指向一个**旧核心**时，负对照会以"核心居然接受了"失败 ——
//! 那不是规范错，是核心版本旧。下面的失败信息里会带上核心版本，省得下次再查一遍。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::routing::{MatchCondition, RoutingRule, RuleAction};
use xt_core::xray::{build_pretty, merge_rules_with_intent, CoreConfigInput, InboundProfile};
use xt_intent::rules::{materialize, AllowAction, AllowOverride, RuleOptions};
use xt_intent::verdict::{BlockVerdict, Category, Verdict};

/// 一份"典型"的广告判决。
fn block_verdict() -> Verdict {
    Verdict::Block(BlockVerdict {
        category: Category::AdOrMonetization,
        ads_intent: 0.97,
        risk_of_breakage: 0.05,
        choice_confidence: 0.93,
        effective_min: 0.85,
    })
}

/// 用户对某条误杀的纠正。
fn allow_verdict() -> Verdict {
    Verdict::Allow(xt_intent::verdict::AllowReason::BreakageRiskTooHigh {
        risk: 0.8,
        max: 0.3,
    })
}

fn intent_rules() -> (Vec<RoutingRule>, Vec<RoutingRule>) {
    let b = block_verdict();
    let a = allow_verdict();
    let opts = RuleOptions {
        allow_overrides: vec![AllowOverride {
            host: "cdn.news.example".into(),
            action: AllowAction::Direct,
        }],
        ..Default::default()
    };
    let rules = materialize(
        vec![("adsrv-7f3.example", &b), ("tracker.example", &b), ("cdn.news.example", &a)],
        &opts,
    );
    (rules.allow, rules.block)
}

fn settings() -> AppSettings {
    AppSettings {
        routing_preset: RoutingPreset::BypassMainland,
        log_level: "warning".into(),
        ..Default::default()
    }
}

fn config_with_intent() -> String {
    let s = settings();
    let (allow, block) = intent_rules();
    let rules = merge_rules_with_intent(&s, &allow, &block);
    build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &rules,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    })
}

fn rule_tags(cfg: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(cfg).expect("生成的配置必须是合法 JSON");
    v["routing"]["rules"]
        .as_array()
        .expect("配置必须有 routing.rules")
        .iter()
        .filter_map(|r| r["ruleTag"].as_str().map(str::to_string))
        .collect()
}

fn core_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XT_CORE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // 优先项目自带的（26.9.9，本项目的目标版本）；`.scratch` 那个是较旧的 stable，
    // 只在没有自带核心时才用 —— 两者对 `ruleTag` 唯一性的检查不一样（见文件头）。
    [
        repo.join("apps/desktop/binaries/xray"),
        repo.join(".scratch/xray-server/xray"),
    ]
    .into_iter()
    .find(|c| c.is_file())
}

fn core_self_check(core: &Path, cfg: &Path) -> (i32, String) {
    let out = Command::new(core)
        .args(["run", "-test", "-c"])
        .arg(cfg)
        .output()
        .expect("必须能启动核心做自检");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let code = out.status.code().unwrap_or(-1);
    (code, text)
}

/// 核心自报的版本行（失败信息里带上它，省得为"是不是核心太旧"再查一遍）。
fn core_version(core: &Path) -> String {
    Command::new(core)
        .arg("version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
        .unwrap_or_default()
}

fn scratch_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xt-intent-realcore-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 不依赖核心的那部分：意图规则**在生成配置里的位置与形状**。
///
/// 这条永远跑（不需要核心二进制），它保证"进核心之前"的这段是对的。
#[test]
fn intent_rules_have_the_right_place_and_shape() {
    let cfg = config_with_intent();
    let tags = rule_tags(&cfg);

    let pos = |t: &str| {
        tags.iter()
            .position(|x| x.starts_with(t))
            .unwrap_or_else(|| panic!("配置里没有 {t}：{tags:?}"))
    };
    let private = pos("internal-dns-hijack"); // 内部 DNS 劫持永远第一
    assert_eq!(private, 0);
    let private_preset = pos("preset-private");
    let allow = pos("intent-allow-cdn.news.example");
    let block = pos("intent-block-adsrv-7f3.example");
    let ads = pos("preset-ads");
    let cn = pos("preset-cn-domain");

    assert!(private_preset < allow, "意图放行必须在私有直连之后：{tags:?}");
    assert!(allow < block, "放行必须早于拦截：{tags:?}");
    assert!(block < ads, "意图拦截必须早于静态广告名单：{tags:?}");
    assert!(block < cn, "意图拦截必须早于大陆直连：{tags:?}");

    // 用户纠正过的域名**不许**出现在拦截带里。
    assert!(
        !tags.iter().any(|t| t.starts_with("intent-block-cdn.news.example")),
        "放行过的域名又进了拦截带：{tags:?}"
    );

    // 形状：拦截规则指向 blackhole，且带 inboundTag 限定。
    let v: Value = serde_json::from_str(&cfg).unwrap();
    let rules = v["routing"]["rules"].as_array().unwrap();
    let hit = rules
        .iter()
        .find(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("intent-block-adsrv-7f3"))
        .expect("拦截规则必须在编译结果里");
    assert_eq!(hit["outboundTag"], "block");
    assert_eq!(hit["domain"][0], "full:adsrv-7f3.example");
    assert_eq!(hit["inboundTag"][0], "tun");
}

/// 真实核心：正例 `exit 0`，负例（重复 `ruleTag`）必须被拒。
#[test]
fn real_core_accepts_intent_rules_and_still_rejects_duplicate_tags() {
    let Some(core) = core_binary() else {
        eprintln!(
            "SKIP：没找到真实核心（XT_CORE 未设，且 apps/desktop/binaries/xray 与 \
             .scratch/xray-server/xray 都不存在）。这条验收在本地/发布前必须手动跑一次。"
        );
        return;
    };
    let dir = scratch_dir("core");

    // ---- 正例 ----
    let good = dir.join("intent-good.json");
    std::fs::write(&good, config_with_intent()).unwrap();
    let (code, text) = core_self_check(&core, &good);
    assert_eq!(
        code, 0,
        "真实核心拒绝了带意图规则的配置：exit {code}\n{text}\n（核心 {core:?}）"
    );
    println!("真实核心接受意图配置：exit 0");

    // ---- 负例：把两条规则的 ruleTag 改成一样 ----
    let cfg = config_with_intent();
    let mut v: Value = serde_json::from_str(&cfg).unwrap();
    let rules = v["routing"]["rules"].as_array_mut().unwrap();
    let first = rules
        .iter()
        .position(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("intent-block-"))
        .expect("必须有一条意图拦截规则");
    let tag = rules[first]["ruleTag"].as_str().unwrap().to_string();
    // 找一条别的规则，把它的 tag 改成同一个。
    let other = rules
        .iter()
        .position(|r| r["ruleTag"].as_str().unwrap_or("") != tag)
        .expect("必须还有别的规则");
    rules[other]["ruleTag"] = Value::String(tag.clone());

    let bad = dir.join("intent-duplicate-tag.json");
    std::fs::write(&bad, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let (code, text) = core_self_check(&core, &bad);
    let version = core_version(&core);
    assert_ne!(
        code, 0,
        "重复 ruleTag 必须被核心拒绝，但它加载成功了。\n核心版本：{version}\n         （26.3.27 及更早**不检查** ruleTag 唯一性；26.9.x 才检查。\
         若这里报的是旧核心，请把 XT_CORE 指向 apps/desktop/binaries/xray）\n{text}"
    );
    assert!(
        text.contains("duplicate ruleTag"),
        "拒载的错误信息里必须指名冲突（排障靠它）：\n{text}"
    );
    println!("真实核心按预期拒载重复 ruleTag：{tag}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空意图（功能关着）时，配置必须与之前**逐字节相同** ——
/// "没开这个功能"就不该对既有行为有任何影响。
#[test]
fn an_empty_intent_layer_changes_nothing() {
    let s = settings();
    let without = xt_core::xray::merge_rules(&s);
    let with = merge_rules_with_intent(&s, &[], &[]);
    assert_eq!(without, with);

    let cfg_without = build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &without,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    });
    let cfg_with = build_pretty(&CoreConfigInput {
        settings: &s,
        nodes: &[],
        selected: None,
        rules: &with,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    });
    assert_eq!(cfg_without, cfg_with, "没开意图过滤时配置必须逐字节相同");
}

/// 规则列表里**只有**用户已有的东西 + 我们加的两条带；没有多余的规则被塞进来。
#[test]
fn only_the_intent_bands_are_added() {
    let s = settings();
    let base = xt_core::xray::merge_rules(&s);
    let (allow, block) = intent_rules();
    let merged = merge_rules_with_intent(&s, &allow, &block);
    assert_eq!(
        merged.len(),
        base.len() + allow.len() + block.len(),
        "只允许新增意图规则，不许增删其它规则"
    );
    // 除意图规则外，其余规则的 id 与顺序必须与基线一致。
    let stripped: Vec<&String> = merged
        .iter()
        .filter(|r| !r.id.starts_with("intent-"))
        .map(|r| &r.id)
        .collect();
    let base_ids: Vec<&String> = base.iter().map(|r| &r.id).collect();
    assert_eq!(stripped, base_ids, "既有规则的顺序被改动了");
}

/// 意图规则的形状与 `MatchCondition` 的既定语义一致（不会被误当成 catch-all）。
#[test]
fn intent_rules_are_never_catch_all() {
    let (allow, block) = intent_rules();
    for r in allow.iter().chain(block.iter()) {
        assert!(!r.when.is_empty(), "{} 变成了 catch-all", r.id);
        assert!(!r.when.domains.is_empty(), "{} 没有域名条件", r.id);
        assert!(!r.when.inbound_tags.is_empty(), "{} 没有限定入站（MITM 阶段会自环）", r.id);
        assert_eq!(r.when.network, xt_core::routing::Network::Both, "{} 不该限制网络层", r.id);
    }
    for r in &block {
        assert_eq!(r.then, RuleAction::Block);
        assert!(r.when.domains.iter().all(|d| d.starts_with("full:")), "必须用 full: 精确匹配");
    }
    let _ = MatchCondition::default();
}
