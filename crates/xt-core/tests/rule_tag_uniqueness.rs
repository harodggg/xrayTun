//! `ruleTag` 唯一性验收（源自 `task-166` 的临时探针，收进仓库 —— `task-169`）。
//!
//! # 为什么必须有这条测试
//!
//! 用户报的启动阻断：`Failed to start: main: failed to create server > app/router:
//! duplicate ruleTag preset-private` —— 只要 `custom_rules` 里有与当前预设同名的 id，
//! Xray 就在 `app/router` 阶段**拒绝启动**（不是网络问题、不是脏数据特例）。
//! 修复在 `ac82111`（`task-165`）。
//!
//! # 判据分层（**别写成「撤掉第一层真实核心就得红」**）
//!
//! * **最终产物唯一性由第二层保证** —— `build_routing` 在**最终 `rules` 数组**上做
//!   `uniquify_rule_tag_values`，因此它看得见**三处来源**：预设、自定义、以及 App
//!   自己追加的三条内部规则（`internal-dns-hijack` / `internal-api` / `internal-fallback`）。
//! * **第一层（`merge_rules`）另有一组单测直接断言其输出** —— 它保证「预设+自定义」这一段
//!   的 id 唯一，是契约层；但**撤掉它，最终配置仍然唯一、真实核心仍然 exit 0**（第二层兜底）。
//!
//! `task-166` 的按层突变实测（每行一句）：
//! * `m1` 只撤第一层 ⇒ 最终配置**全唯一**、真实核心**全 exit 0**；仓库单测 **1 条红**
//!   （`user_rule_shape_with_bypass_mainland_is_uniquified_without_losing_rules`）。
//! * `m2` 只撤第二层 ⇒ 三个 `internal-*` 入口**复现 `duplicate ruleTag internal-*`（exit 23）**；
//!   预设/自定义那几类仍唯一（第一层兜住）；仓库单测 **2 条红**。
//! * `m3` 两层都撤 ⇒ 用户形态 **4 个 id ×2** 且真实核心 **`duplicate ruleTag preset-private`（exit 23，
//!   与用户逐字一致）**；仓库单测 **4 条红**。⇒ 「去掉唯一化必须红」由 `m3` 成立。
//!
//! # 运行方式
//!
//! ```bash
//! # 1) 不需要核心的部分（CI 上默认跑这些：唯一性 / 顺序 / 条数 / 确定性）
//! cargo test -p xt-core --test rule_tag_uniqueness
//!
//! # 2) 真实核心验收（需要仓库里的核心二进制，或显式给路径）
//! cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture
//! XT_CORE=/abs/path/to/xray cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture
//! ```
//!
//! ## 核心不存在时：**skip，不是失败**
//!
//! `#[ignore]` 的用例先解析核心路径（`XT_CORE` → 否则 `<repo>/apps/desktop/binaries/xray`）。
//! 解析不到可执行文件时它会 `eprintln!` 一行说明并**直接返回**（测试算通过）——
//! 目的是让**没有核心的 CI 依然绿**。代价见文件末尾「诚实清单」：
//! **CI 上这条等于没跑**，真实核心那一步只在本地/发布前手动跑。
//!
//! # 不写用户数据
//!
//! 全部 fixture 在测试内构造；生成的配置写到**临时目录**，并且用 `nodes: &[]`、
//! `selected: None` ⇒ 磁盘上不会出现任何节点的地址/UUID。（`task-166` 同做法。）
//!
//! # 诚实清单（这份测试**测不到**什么）
//!
//! * **App 的 supervisor 接线路径**未覆盖：这里直接调 `merge_rules` + `build_pretty`
//!   + `validate_config`，不是「点界面 → 启动核心」那条路（真机 UI 需要 GUI 与真实网络状态）。
//! * **核心不存在时等于没跑**（CI 常态），所以它**不能**替代发布前的手动真实核心验收。
//! * 顺序判据是「剥离 `#n` 后缀后逐位对应 + 条数相等」，不是 AST 级。
//! * 它只验证**配置能被核心接受**，不验证路由**行为**（哪条规则命中）不变 —— 那需要真机流量。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use xt_core::model::{AppSettings, RoutingPreset};
use xt_core::routing::{self, RoutingRule};
use xt_core::xray::config::{build_pretty, merge_rules, CoreConfigInput, InboundProfile};

/// 用户真实 `settings.json` 里那 5 条 `custom_rules` 的 **id**（只取 id，不含任何用户数据）。
const USER_SHAPE_IDS: [&str; 5] = [
    "preset-private",
    "preset-ads",
    "google-to-us",
    "preset-cn-domain",
    "preset-cn-ip",
];

/// App 在 `build_routing` 里自己追加的三条内部规则 tag（第二层要一起兜的保留名）。
const INTERNAL_TAGS: [&str; 3] = ["internal-dns-hijack", "internal-api", "internal-fallback"];

const PRESETS: [(RoutingPreset, &str); 5] = [
    (RoutingPreset::GlobalProxy, "global_proxy"),
    (RoutingPreset::BypassMainland, "bypass_mainland"),
    (RoutingPreset::WhitelistProxy, "whitelist_proxy"),
    (RoutingPreset::DirectAll, "direct_all"),
    (RoutingPreset::Custom, "custom"),
];

fn rule(id: &str) -> RoutingRule {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("fixture-{id}"),
        "enabled": true,
        "when": { "domains": ["example.test"] },
        "then": { "kind": "direct" },
    }))
    .unwrap_or_else(|e| panic!("fixture 规则（id={id}）必须能反序列化：{e}"))
}

fn settings(preset: RoutingPreset, custom: Vec<RoutingRule>) -> AppSettings {
    // `AppSettings` 的每个字段都带 `#[serde(default)]` ⇒ 空对象即可建出默认值。
    let mut s: AppSettings =
        serde_json::from_value(serde_json::json!({})).expect("AppSettings 全字段都有 default");
    s.routing_preset = preset;
    s.custom_rules = custom;
    s
}

/// 走 **App 自己的构建路径**：`merge_rules` → `build_pretty`。
fn build_config(s: &AppSettings) -> String {
    let rules = merge_rules(s);
    build_pretty(&CoreConfigInput {
        settings: s,
        nodes: &[],
        selected: None,
        rules: &rules,
        profile: InboundProfile::LocalProxy,
        physical_interface: None,
    })
}

/// 从**最终配置**里取用户可见真相：`routing.rules[].ruleTag`（不读实现内部结构）。
fn rule_tags(cfg: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(cfg).expect("生成的配置必须是合法 JSON");
    v["routing"]["rules"]
        .as_array()
        .expect("配置必须有 routing.rules")
        .iter()
        .filter_map(|r| r["ruleTag"].as_str().map(str::to_string))
        .collect()
}

/// 期望顺序：`internal-dns-hijack`、`internal-api`、预设…、自定义…、`internal-fallback`。
fn expected_sequence(preset: RoutingPreset, custom_ids: &[String]) -> Vec<String> {
    let mut out = vec![INTERNAL_TAGS[0].to_string(), INTERNAL_TAGS[1].to_string()];
    out.extend(routing::preset_rules(preset).iter().map(|r| r.id.clone()));
    out.extend(custom_ids.iter().cloned());
    out.push(INTERNAL_TAGS[2].to_string());
    out
}

/// 唯一化只允许加 `#n` 后缀：剥离后必须与期望序列**逐位相同**、条数相等。
fn order_preserved(tags: &[String], expected: &[String]) -> bool {
    tags.len() == expected.len()
        && tags
            .iter()
            .zip(expected)
            .all(|(t, e)| t == e || t.starts_with(&format!("{e}#")))
}

fn duplicates(tags: &[String]) -> Vec<(String, usize)> {
    let mut m: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for t in tags {
        *m.entry(t.as_str()).or_insert(0) += 1;
    }
    m.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, n)| (k.to_string(), n))
        .collect()
}

fn assert_case(label: &str, preset: RoutingPreset, custom: Vec<RoutingRule>) -> String {
    let s = settings(preset, custom);
    let cfg = build_config(&s);
    let tags = rule_tags(&cfg);
    let expected = expected_sequence(preset, &s.custom_rules.iter().map(|r| r.id.clone()).collect::<Vec<_>>());
    let dups = duplicates(&tags);
    assert!(
        dups.is_empty(),
        "{label}: 最终配置里 ruleTag 必须全唯一，重复={dups:?}；tags={tags:?}"
    );
    assert!(
        order_preserved(&tags, &expected),
        "{label}: 顺序/条数必须不变（只允许 #n 后缀）\n  实际={tags:?}\n  期望={expected:?}"
    );
    cfg
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xraytun-ruletag-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

/// 解析真实核心：`XT_CORE` 优先，否则 `<repo>/apps/desktop/binaries/xray`。
fn core_binary() -> Option<PathBuf> {
    let p = match std::env::var_os("XT_CORE") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/binaries/xray"),
    };
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// 跑真实核心自检，返回 (退出码, 末行)。
fn core_self_check(core: &Path, cfg: &Path) -> (i32, String) {
    let out = Command::new(core)
        .args(["run", "-test", "-c"])
        .arg(cfg)
        .output()
        .expect("核心必须能被执行");
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string();
    (out.status.code().unwrap_or(-1), last)
}

// ---------------------------------------------------------------------------
// 不需要核心：配置层判据（CI 上跑这些）
// ---------------------------------------------------------------------------

/// 22 个 case 的关键子集：5 预设 ×（无 / 部分 / 全部同 id）+ `custom` 内部两条同 id + 三个 `internal-*` 入口。
#[test]
fn rule_tags_are_unique_for_every_preset_and_collision_shape() {
    let user_rules: Vec<RoutingRule> = USER_SHAPE_IDS.iter().map(|id| rule(id)).collect();
    let mut cases = 0usize;

    for (preset, name) in PRESETS {
        let preset_ids: Vec<String> = routing::preset_rules(preset).iter().map(|r| r.id.clone()).collect();

        // 无同 id 自定义
        assert_case(&format!("{name}/none"), preset, vec![]);
        cases += 1;

        // 部分同 id（没有同名的就用第 1 条用户规则，仍然要求唯一/顺序不变）
        let partial: Vec<RoutingRule> = user_rules
            .iter()
            .filter(|r| preset_ids.contains(&r.id))
            .take(1)
            .cloned()
            .collect();
        let partial = if partial.is_empty() {
            user_rules.iter().take(1).cloned().collect()
        } else {
            partial
        };
        assert_case(&format!("{name}/partial"), preset, partial);
        cases += 1;

        // 全部同 id（该预设没有预设规则时退化成全部用户规则）
        let all: Vec<RoutingRule> = user_rules
            .iter()
            .filter(|r| preset_ids.contains(&r.id))
            .cloned()
            .collect();
        let all = if all.is_empty() { user_rules.clone() } else { all };
        assert_case(&format!("{name}/all"), preset, all);
        cases += 1;
    }

    // `custom` 分支内部两条同 id
    assert_case(
        "custom/internal-dup",
        RoutingPreset::Custom,
        vec![rule("preset-private"), rule("preset-private")],
    );
    cases += 1;

    // 与 App **自己的**内部规则 tag 撞名（`task-166` 的第三类来源）
    for internal in INTERNAL_TAGS {
        assert_case(
            &format!("internal/{internal}"),
            RoutingPreset::GlobalProxy,
            vec![rule(internal)],
        );
        cases += 1;
    }

    // 19 = 5 预设 ×（none/partial/all）+ custom 内部两条同 id + 3 个 internal-* 入口。
    // task-166 的矩阵是 21 个 case：另外 2 个（用户形态 `bypass_mainland`、以及当前预设
    // `global_proxy`）分别由本文件的 `user_shape_is_uniquified_without_losing_rules`
    // 与 `#[ignore]` 的真实核心用例覆盖；再加上手搓负对照，合计 22 份配置。
    assert_eq!(cases, 19, "case 数应与本文件覆盖的矩阵子集一致（见上面的映射说明）");
}

/// 用户真实形态：5 条 id × `bypass_mainland` ⇒ 13 条规则、全唯一、**不丢任何一条**。
#[test]
fn user_shape_is_uniquified_without_losing_rules() {
    let custom: Vec<RoutingRule> = USER_SHAPE_IDS.iter().map(|id| rule(id)).collect();
    let s = settings(RoutingPreset::BypassMainland, custom);
    let cfg = build_config(&s);
    let tags = rule_tags(&cfg);
    let preset_n = routing::preset_rules(RoutingPreset::BypassMainland).len();

    assert!(duplicates(&tags).is_empty(), "必须全唯一：{tags:?}");
    assert_eq!(
        tags.len(),
        preset_n + 2 + USER_SHAPE_IDS.len() + 1,
        "条数必须 = 2 条内部 + 预设 + 5 条自定义 + 1 条内部（修前是 13，一条都不许丢）"
    );
    assert_eq!(tags.len(), 13, "task-166 实测：用户形态修后仍是 13 条");
    // 5 条自定义规则**都还在**（按后缀剥离后逐条出现）
    for id in USER_SHAPE_IDS {
        assert!(
            tags.iter().any(|t| t == id || t.starts_with(&format!("{id}#"))),
            "自定义规则 {id} 不许被丢掉：{tags:?}"
        );
    }
}

/// 确定性：同一输入两次构建必须**逐字节相同**（后缀稳定、无随机/时间因素）。
#[test]
fn same_input_builds_byte_identical_configs() {
    let custom: Vec<RoutingRule> = USER_SHAPE_IDS.iter().map(|id| rule(id)).collect();
    let s = settings(RoutingPreset::BypassMainland, custom);
    let a = build_config(&s);
    let b = build_config(&s);
    assert_eq!(a, b, "两次构建必须逐字节相同");
}

// ---------------------------------------------------------------------------
// 需要真实核心（`--ignored`；核心不存在则 skip 而非失败）
// ---------------------------------------------------------------------------

/// **真实核心验收**：App 构建路径产出的配置必须被核心接受；同时用一条**手搓的重复配置**做负对照，
/// 证明「全绿」不是因为核心太宽松（`task-166` 的教训：没有负对照的 sweep 可能是空的）。
#[test]
#[ignore = "需要真实核心二进制（XT_CORE 或 apps/desktop/binaries/xray）"]
fn real_core_accepts_generated_configs_and_rejects_duplicates() {
    let Some(core) = core_binary() else {
        eprintln!(
            "SKIP：没找到真实核心（XT_CORE 未设，且 apps/desktop/binaries/xray 不存在）—— \
             这条用例在**没有核心的 CI 上等于没跑**，真实核心验收必须在本地/发布前手动跑。"
        );
        return;
    };
    let dir = temp_dir("core");
    let user_rules: Vec<RoutingRule> = USER_SHAPE_IDS.iter().map(|id| rule(id)).collect();

    // 关键子集：用户形态 + 全部同 id 预设 + custom 内部重复 + 三个 internal-* 入口
    let mut cases: Vec<(String, String)> = vec![
        ("user-bypass".into(), build_config(&settings(RoutingPreset::BypassMainland, user_rules.clone()))),
        (
            "custom-internal-dup".into(),
            build_config(&settings(
                RoutingPreset::Custom,
                vec![rule("preset-private"), rule("preset-private")],
            )),
        ),
    ];
    for internal in INTERNAL_TAGS {
        cases.push((
            format!("internal-{internal}"),
            build_config(&settings(RoutingPreset::GlobalProxy, vec![rule(internal)])),
        ));
    }
    // 用户在同预设下的**全局代理**（对照：本来就没冲突）
    cases.push((
        "user-current-preset".into(),
        build_config(&settings(RoutingPreset::GlobalProxy, user_rules)),
    ));

    for (label, cfg) in &cases {
        let path = dir.join(format!("{label}.json"));
        std::fs::write(&path, cfg).expect("写配置");
        let (code, last) = core_self_check(&core, &path);
        assert_eq!(code, 0, "核心必须接受 App 构建的配置（{label}）：exit={code} / {last}");
    }

    // 负对照：手搓的重复 ruleTag ⇒ 核心必须拒绝，且**指名**重复的那个 tag
    let dup = dir.join("handmade-duplicate.json");
    std::fs::write(
        &dup,
        r#"{
  "log": {"loglevel": "warning"},
  "inbounds": [{"tag":"socks","port":10808,"listen":"127.0.0.1","protocol":"socks","settings":{"udp":true}}],
  "outbounds": [{"tag":"direct","protocol":"freedom"}],
  "routing": {"domainStrategy":"IPIfNonMatch","rules":[
    {"type":"field","domain":["a.test"],"outboundTag":"direct","ruleTag":"preset-private"},
    {"type":"field","domain":["b.test"],"outboundTag":"direct","ruleTag":"preset-private"}
  ]}
}"#,
    )
    .expect("写负对照");
    let (code, last) = core_self_check(&core, &dup);
    assert_ne!(code, 0, "手搓的重复 ruleTag 必须被拒（否则上面那批全绿没有意义）");
    assert!(
        last.contains("duplicate ruleTag preset-private"),
        "负对照的错误必须指名重复 tag，实际末行：{last}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
