//! **TUN 配置形状**的真实核心验收（不需要 root）。
//!
//! # 为什么需要单独一条
//!
//! TUN 是这个项目的主路径，但在此之前**没有任何测试**用 `InboundProfile::Tun`
//! 生成过配置 —— 所有真实核心验收走的都是 `LocalProxy`（socks/http）。
//! 于是"TUN 那份配置核心到底认不认"这件事一直没有证据：profile 之间共享大部分
//! 生成代码，但入站形状（`protocol: "tun"` + gVisor 字段 + `autoRoute`/`strictRoute`）
//! 是**只有 TUN 才会走到**的分支。
//!
//! 这里只验**配置层**：生成 → 形状断言 → 交给真核心 `run -test` 自检。
//! **不验数据面**：真正建 utun 要 root，那是 §16.3 的手动验收。
//! 把这条写清楚，免得"TUN 有测试了"被读成"TUN 通了"。
//!
//! # 覆盖的三个组合
//!
//! 1. 纯 TUN（无意图、无 MITM）：基线，证明 profile 本身没问题；
//! 2. TUN + 意图拦截规则：证明 `blackhole`/`block-silent` 出站在 TUN 配置里也在；
//! 3. TUN + MITM 引导（`mitm-steer` + `mitm-upstream` + QUIC 兜底）：
//!    **steer 规则的 `inboundTag` 必须含 `tun`** —— 少了它，TUN 模式下整条
//!    MITM 链路就是死的（配置合法、核心启动成功、但一个包都不会被引导）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use xt_core::model::{AppSettings, ProxyMode, RoutingPreset};
use xt_core::xray::{
    build_pretty, merge_rules_with_intent, tun_inbound_spec, CoreConfigInput, InboundProfile,
};
use xt_intent::rules::{materialize, RuleOptions};
use xt_intent::verdict::{BlockVerdict, Category, Verdict};

const BLOCKED: &str = "ads.tun-profile";

fn core_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XT_CORE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    [repo.join("apps/desktop/binaries/xray"), repo.join(".scratch/xray-server/xray")]
        .into_iter()
        .find(|c| c.is_file())
}

/// TUN 模式的基础设置。`physical_interface` 给一个名字即可 ——
/// **配置层不看它是否存在**（真正绑网卡是运行时的事），所以这里不需要 root。
fn tun_settings() -> AppSettings {
    AppSettings {
        mode: ProxyMode::Tun,
        routing_preset: RoutingPreset::BypassMainland,
        log_level: "warning".into(),
        ..Default::default()
    }
}

fn config(s: &AppSettings, rules: &[xt_core::routing::RoutingRule], auto_routes: bool) -> String {
    build_pretty(&CoreConfigInput {
        settings: s,
        nodes: &[],
        selected: None,
        rules,
        profile: InboundProfile::Tun(tun_inbound_spec(s, Some("en0"), auto_routes)),
        physical_interface: Some("en0"),
    })
}

/// 交给真核心做自检。返回 `(退出码, 输出)`。
fn core_self_check(core: &Path, cfg: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("xt-tun-profile-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("tun.json");
    std::fs::write(&path, cfg).unwrap();
    let out = Command::new(core)
        .args(["run", "-test", "-c"])
        .arg(&path)
        .output()
        .expect("必须能启动核心做自检");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn the_tun_profile_with_intent_and_mitm_rules_is_accepted_by_the_real_core() {
    let Some(core) = core_binary() else {
        eprintln!("SKIP：没找到真实核心（XT_CORE 未设，两个默认路径都不存在）");
        return;
    };

    let mut s = tun_settings();
    s.dns.direct_servers = vec!["223.5.5.5".into()];

    // 意图拦截规则（走真实判决 → 物化路径，不手写 MatchCondition）
    let block = Verdict::Block(BlockVerdict {
        category: Category::AdOrMonetization,
        ads_intent: 0.97,
        risk_of_breakage: 0.05,
        choice_confidence: 0.93,
        effective_min: 0.85,
    });
    let intent = materialize(vec![(BLOCKED, &block)], &RuleOptions::default());

    // ---- 组合 1：纯 TUN ----
    let plain = merge_rules_with_intent(&s, &[], &[]);
    let cfg = config(&s, &plain, true);
    let v: Value = serde_json::from_str(&cfg).expect("生成的配置必须是合法 JSON");
    let tun_in = v["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["tag"] == "tun")
        .expect("TUN profile 必须生成 tun 入站");
    assert_eq!(tun_in["protocol"], "tun");
    // 形状按**实际生成**的来断言（不猜字段位置 —— 我第一次就猜错了 `autoRoute`）。
    assert_eq!(
        tun_in["settings"]["autoSystemRoutingTable"],
        serde_json::json!(["0.0.0.0/1", "128.0.0.0/1"]),
        "auto_routes=true 必须把默认路由的两半写进配置（否则 TUN 不接管流量）"
    );
    assert_eq!(
        tun_in["settings"]["autoOutboundsInterface"], "en0",
        "物理网卡必须传给 autoOutboundsInterface —— direct 出站要靠它逃出隧道"
    );
    // 负对照：`auto_routes=false` 时必须**不**接管默认路由。
    // 没有这一条，"表里恰好有那两行"可能只是常量，而不是这个开关真的在起作用。
    let no_auto: Value = serde_json::from_str(&config(&s, &plain, false)).unwrap();
    let tun_no_auto = no_auto["inbounds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["tag"] == "tun")
        .expect("tun 入站");
    assert!(
        tun_no_auto["settings"]["autoSystemRoutingTable"].is_null(),
        "auto_routes=false 时不该有 autoSystemRoutingTable：{tun_no_auto}"
    );
    let (code, out) = core_self_check(&core, &cfg);
    assert_eq!(code, 0, "纯 TUN 配置被真实核心拒绝了：\n{out}");
    println!("① 纯 TUN：核心自检通过 ✓");

    // ---- 组合 2：TUN + 意图拦截 ----
    s.mitm.enabled = false;
    let rules = merge_rules_with_intent(&s, &intent.allow, &intent.block);
    let cfg2 = config(&s, &rules, true);
    let v2: Value = serde_json::from_str(&cfg2).unwrap();
    let tags: Vec<String> = v2["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o["tag"].as_str().map(str::to_string))
        .collect();
    assert!(tags.contains(&"block".to_string()), "拦截出站必须在：{tags:?}");
    assert!(
        tags.contains(&"block-silent".to_string()),
        "静默拦截出站必须在（UDP-only 的规则指着它）：{tags:?}"
    );
    assert!(
        v2["routing"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("intent-block-")),
        "意图拦截规则必须进了 TUN 配置"
    );
    let (code2, out2) = core_self_check(&core, &cfg2);
    assert_eq!(code2, 0, "TUN + 意图规则被真实核心拒绝了：\n{out2}");
    println!("② TUN + 意图拦截规则：核心自检通过 ✓");

    // ---- 组合 3：TUN + MITM 引导（**steer 必须覆盖 tun 入站**）----
    s.mitm.enabled = true;
    s.mitm.domains = vec!["news.example".into()];
    s.mitm.block_quic = true;
    let rules3 = merge_rules_with_intent(&s, &intent.allow, &intent.block);
    let cfg3 = config(&s, &rules3, true);
    let v3: Value = serde_json::from_str(&cfg3).unwrap();
    let rules3_json = v3["routing"]["rules"].as_array().unwrap();
    let steer = rules3_json
        .iter()
        .find(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("mitm-steer"))
        .expect("开了 MITM 就该有 mitm-steer 规则");
    let steer_inbounds: Vec<String> = steer["inboundTag"]
        .as_array()
        .expect("steer 规则必须限定入站（否则回连会自环）")
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    assert!(
        steer_inbounds.contains(&"tun".to_string()),
        "**steer 规则必须覆盖 tun 入站**，否则 TUN 模式下这条链路是死的：{steer_inbounds:?}"
    );
    // 回连用的 socks 入站**绝不能**在 steer 的入站列表里 —— 防自环靠的就是这条。
    assert!(
        !steer_inbounds.contains(&"mitm-upstream".to_string()),
        "回连入站出现在 steer 列表里 = 自环：{steer_inbounds:?}"
    );
    let quic = rules3_json
        .iter()
        .find(|r| r["ruleTag"] == "mitm-quic-fallback")
        .expect("block_quic 开着就该有 QUIC 兜底规则");
    assert_eq!(quic["outboundTag"], "block-silent", "QUIC 兜底必须静默丢弃");
    let (code3, out3) = core_self_check(&core, &cfg3);
    assert_eq!(code3, 0, "TUN + MITM 引导被真实核心拒绝了：\n{out3}");
    println!("③ TUN + MITM 引导：核心自检通过，且 steer 覆盖 tun、不含回连入站 ✓");
}
