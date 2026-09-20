//! **跨语言契约测试**：Rust 的序列化输出 vs 前端手写的 TypeScript 类型。
//!
//! # 为什么需要它
//!
//! `apps/ui/src/types.ts` 是**手写**的，字段名必须与 Rust 侧 `serde` 的输出一致，
//! 但两者之间没有编译器检查。这里从两个方向取字段名并比对：一边把真实的 Rust
//! 值序列化成 JSON 取键，另一边直接读 `types.ts` 里对应接口的字段名。
//!
//! # 它到底覆盖什么（用变异测试实测过，不要高估）
//!
//! * **TS 声明了 Rust 不提供的字段** -> 本测试抓到
//!   （实测：往 `AppSnapshot` 注入 `nonexistent_field_probe`，测试红）。
//! * **Rust 侧改名/删除字段而前端没跟上** -> 通常**编译器先抓到**：
//!   结构体一改，"构造 AppSnapshot"的代码就编不过。实测确认过这一点，
//!   所以不要把这个测试当成那类问题的唯一防线。
//! * **Rust 有、前端未声明的字段** -> 本测试抓到，这是它**独有**的价值：
//!   它第一次运行时就在 `AppSettings` 上抓到 3 个（见下面的登记表）。
//!
//! # 已知不覆盖
//!
//! **不校验字段类型**（那需要完整解析 TS）。也就是说 `port: string` 写成
//! `port: number` 这类错误它看不出来 —— 只校验**名字集合**，而名字正是
//! 「界面静默读到 undefined」的成因。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use xraytun_desktop_lib::state::{
    AppSnapshot, CoreAvailability, CoreRuntime, HelperAvailability, LoginItemState, RecoveryOutcome,
    RecoveryState, TrafficSample, UpdateStatus,
};
use xt_core::model::AppSettings;
use xt_core::store::Store;

/// 仓库根目录。
///
/// 刻意不在生产代码里加「测试用」的路径导出：`dev_binaries_dir` 的语义是
/// 「开发期去哪找核心」，与前端资源无关，不该为测试改它。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("apps/desktop 之上应当有仓库根")
        .to_path_buf()
}

fn types_ts() -> String {
    let p = repo_root().join("apps/ui/src/types.ts");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}: {e}", p.display()))
}

/// 从一个 `export interface X { ... }` 里取出**顶层**字段名。
///
/// 只认「行首两个空格 + 标识符 + 可选 ? + 冒号」这种形状 —— 这个文件里的
/// 接口都是这个风格，够用且不引入 TS 解析器依赖。嵌套对象与注释被跳过：
/// 嵌套行的缩进更深，注释以 `/` 或 `*` 开头。
fn ts_interface_fields(src: &str, name: &str) -> BTreeSet<String> {
    let header = format!("export interface {name} {{");
    let start = src
        .find(&header)
        .unwrap_or_else(|| panic!("types.ts 里找不到 interface {name}"))
        + header.len();

    // 花括号配平找到接口结尾
    let mut depth = 1usize;
    let mut end = start;
    for (i, ch) in src[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(depth == 0, "interface {name} 的花括号不配平");

    let mut out = BTreeSet::new();
    for line in src[start..end].lines() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        // 顶层字段只有一层缩进（2 空格）；更深的属于嵌套对象
        let indent = line.len() - trimmed.len();
        if indent != 2 {
            continue;
        }
        if let Some(colon) = trimmed.find(':') {
            let key = trimmed[..colon].trim().trim_end_matches('?').trim();
            if !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.insert(key.to_string());
            }
        }
    }
    out
}

fn json_keys(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("应当是 JSON 对象")
        .keys()
        .cloned()
        .collect()
}

/// 一份「所有字段都填上」的 AppSettings，避免序列化时缺键。
fn full_settings() -> AppSettings {
    let dir = std::env::temp_dir().join(format!("xt-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::new(dir);
    let settings = store.load_settings(); // 走真实的默认值路径
    let _ = std::fs::remove_dir_all(store.root());
    settings
}

fn full_snapshot() -> AppSnapshot {
    AppSnapshot {
        settings: full_settings(),
        subscriptions: Vec::new(),
        nodes: Vec::new(),
        runtime: CoreRuntime::default(),
        latency: Default::default(),
        traffic: TrafficSample::default(),
        notice: None,
        helper: HelperAvailability::default(),
        core: CoreAvailability::default(),
        login_item: LoginItemState::default(),
        update: UpdateStatus::default(),
        dns: Default::default(),
        app_version: "0.0.0".into(),
    }
}

/// TS 声明了、Rust 却不提供的字段 —— 这类**必须为零**。
///
/// 这正是「界面读到 `undefined`」的成因：前端照着 `types.ts` 去读一个
/// 后端根本不发的键，TypeScript 看不出问题，运行期也不报错。
fn assert_ts_is_covered_by_rust(
    label: &str,
    value: &serde_json::Value,
    interface: &str,
    src: &str,
) -> BTreeSet<String> {
    let rust = json_keys(value);
    let ts = ts_interface_fields(src, interface);
    let missing: Vec<_> = ts.difference(&rust).collect();
    assert!(
        missing.is_empty(),
        "\n{label}: 前端 types.ts 声明了 Rust **不提供**的字段 {missing:?}\n\
         前端会读到 undefined。要么在 Rust 侧补上，要么从 types.ts 删掉。"
    );
    rust
}

/// Rust 有、但前端**刻意不暴露**的字段，逐个登记并说明理由。
///
/// 允许这种差异是为了不让契约测试变成「每加一个内部字段就必须改前端类型」的
/// 噪音源；但差异必须是**显式写在这里**的，不能是没人注意的漂移。
const SETTINGS_FIELDS_NOT_IN_TS: &[(&str, &str)] = &[
    ("settings_version", "迁移用的内部版本号，界面不需要也不该依赖"),
    (
        "was_connected",
        "重连意图（上次退出时是否连着），由 Rust 侧读写；**界面仍不显示这个字段**。\
         注意别和 task-22 的自动恢复混淆：恢复状态走 CoreRuntime.recovery，不是它",
    ),
    (
        "auto_reconnect",
        "开机自动重连的策略开关，界面目前没有对应控件；它**不控制**看门狗的自愈\
         （看门狗只认 was_connected 这个意图）",
    ),
];

#[test]
fn snapshot_fields_match_the_frontend_types() {
    // 快照是界面渲染的唯一数据来源，应当是**完全一致**的：
    // 多一个字段说明界面有东西没用到，少一个说明界面会读到 undefined。
    let json = serde_json::to_value(full_snapshot()).expect("快照应当能序列化");
    let rust = json_keys(&json);
    let ts = ts_interface_fields(&types_ts(), "AppSnapshot");

    assert_eq!(
        rust,
        ts,
        "\nAppSnapshot 与前端 types.ts 的字段不一致：\n  仅 Rust 有: {:?}\n  仅 TS   有: {:?}",
        rust.difference(&ts).collect::<Vec<_>>(),
        ts.difference(&rust).collect::<Vec<_>>()
    );
}

#[test]
fn settings_fields_cover_what_the_frontend_declares() {
    let src = types_ts();
    // 1) 前端声明的字段必须都真的存在（否则界面会读到 undefined）
    let rust = assert_ts_is_covered_by_rust(
        "AppSettings",
        &serde_json::to_value(full_settings()).unwrap(),
        "AppSettings",
        &src,
    );

    // 2) 剩下的就是「Rust 有、前端没声明」的部分，必须与登记表**精确一致**
    let ts = ts_interface_fields(&src, "AppSettings");
    let extra: BTreeSet<String> = rust.difference(&ts).cloned().collect();
    let declared: BTreeSet<String> =
        SETTINGS_FIELDS_NOT_IN_TS.iter().map(|(n, _)| n.to_string()).collect();
    assert_eq!(
        extra,
        declared,
        "\nAppSettings 里「Rust 有、前端未声明」的字段与登记表不一致：\n  未登记: {:?}\n  登记了但已不存在: {:?}\n\
         界面不需要的请加进 SETTINGS_FIELDS_NOT_IN_TS 并写明理由；界面需要的就补进 types.ts。",
        extra.difference(&declared).collect::<Vec<_>>(),
        declared.difference(&extra).collect::<Vec<_>>()
    );
}

/// 这两个结构是独立命令的返回值（`probe_helper` / 快照里的 `core`），
/// 单独比对是因为它们不走 AppSnapshot 的字段集。
#[test]
fn helper_and_core_shapes_match_the_frontend_types() {
    let src = types_ts();
    for (label, value, interface) in [
        (
            "HelperAvailability",
            serde_json::to_value(HelperAvailability::default()).unwrap(),
            "HelperAvailability",
        ),
        (
            "CoreAvailability",
            serde_json::to_value(CoreAvailability::default()).unwrap(),
            "CoreAvailability",
        ),
    ] {
        let rust = assert_ts_is_covered_by_rust(label, &value, interface, &src);
        let ts = ts_interface_fields(&src, interface);
        let extra: Vec<_> = rust.difference(&ts).collect();
        assert!(
            extra.is_empty(),
            "\n{label}: Rust 提供了前端未声明的字段 {extra:?}\n\
             界面若需要就补进 types.ts。"
        );
    }
}

/// 单连接可视化（`recent_connections` 命令）的三个返回类型。
///
/// 它们不走 `AppSnapshot`，所以必须单独比对；而这条链路**没有任何编译器
/// 检查**：字段缺失不会让前端编不过，界面只会静默读到 `undefined`。
///
/// 夹具刻意用**真实解析出来的**记录（而不是手工搓一个空结构）：
/// `ConnectionRecord` 的字段全部由 `parse_connection_line` 填充，
/// 手搓的空结构将来漏填字段时这条测试会看不出来。
#[test]
fn connection_shapes_match_the_frontend_types() {
    use xt_core::xray::access_log::{
        parse_connection_line, ConnectionFilter, ConnectionLog, PairingStats,
    };

    let src = types_ts();
    let accepted = "2026/09/20 13:30:58.560364 from tcp:198.18.0.1:49712 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]";
    let sniffed = "2026/09/20 13:30:58.560290 [Info] [3163266252] app/dispatcher: sniffed domain: www.google.com";

    let record = parse_connection_line(accepted, 1_700_000_000_000).expect("真实访问行应当能解析");
    let mut log = ConnectionLog::new();
    log.observe(sniffed);
    log.observe(accepted);
    let recent = log.recent(&ConnectionFilter::new(10));
    assert_eq!(recent.items.len(), 1, "夹具应当产生一条连接记录");

    // 刻意用**全非零**的值而不是 `Default`：将来若有人给某个字段加上
    // `skip_serializing_if`（跳过 0 / None），`Default` 的键集合会悄悄缩水，
    // 这条契约就会静默失效。`recent` 里的 `pairing` 同理（它必然有 0 字段）。
    let pairing = PairingStats {
        accepted: 1,
        paired: 1,
        unpaired: 1,
        sniffed: 1,
        rejected_stale: 1,
        sniffed_superseded: 1,
    };
    let recent_all_set = xt_core::xray::access_log::RecentConnections {
        items: recent.items.clone(),
        dropped: 1,
        pairing,
    };

    // 第 4 项是**预期字段数**：`ts_interface_fields` 只认「恰好 2 空格缩进」的
    // 顶层成员，若 TS 侧某个字段被误缩进、而 Rust 恰好也没有它，双向比对会
    // 一起漏报。钉住数量能把那个盲区变成一条明确的失败信息。
    for (label, value, interface, expected_fields) in [
        (
            "ConnectionRecord",
            serde_json::to_value(&record).unwrap(),
            "ConnectionRecord",
            12,
        ),
        (
            "PairingStats",
            serde_json::to_value(pairing).unwrap(),
            "PairingStats",
            6,
        ),
        (
            "RecentConnections",
            serde_json::to_value(&recent_all_set).unwrap(),
            "RecentConnections",
            3,
        ),
    ] {
        let rust = assert_ts_is_covered_by_rust(label, &value, interface, &src);
        let ts = ts_interface_fields(&src, interface);
        assert_eq!(
            ts.len(),
            expected_fields,
            "\n{label}: 从 types.ts 只解析出 {} 个字段，预期 {expected_fields} 个。\n\
             若 Rust 侧字段数确实变了，请更新这个常量；若是某一个字段被误缩进\n\
             （解析器只认恰好 2 空格缩进），它会从这里暴露出来。实际解析到: {ts:?}",
            ts.len()
        );
        let extra: Vec<_> = rust.difference(&ts).collect();
        assert!(
            extra.is_empty(),
            "\n{label}: Rust 提供了前端未声明的字段 {extra:?}\n\
             界面若需要就补进 types.ts。"
        );
    }
}

/// `CoreRuntime`：`runtime://changed` 事件与快照**共用**的运行时形状。
///
/// 它此前**不在**本测试的覆盖里 —— 后果实测过：`last_good_node` 一路漂移
/// （Rust 一直在发，`types.ts` 没声明，界面读它就是 `undefined`）。
/// 现在把它也钉住；task-22 新加的 `recovery` 一并受保护。
#[test]
fn core_runtime_shape_matches_the_frontend_types() {
    let src = types_ts();

    // 刻意把每个字段都填上非默认值：`RecoveryState` 里全是 0/None 字段，
    // 用 `Default` 构造会让「将来加了 skip_serializing_if」悄悄缩键。
    let runtime = CoreRuntime {
        running: true,
        pid: Some(4321),
        started_at_unix: Some(1_700_000_000),
        config_path: Some(PathBuf::from("/tmp/runtime/config.json")),
        tun_session: Some("sess-1".into()),
        tun_interface: Some("utun3".into()),
        routes_committed: true,
        last_error: Some("示例错误".into()),
        last_good_node: Some("node-1".into()),
        recovery: RecoveryState {
            recovering: true,
            attempt: 2,
            probe_failures: 3,
            started_unix: Some(1_700_000_010),
            last_outcome: Some(RecoveryOutcome::DirectFallback),
            finished_unix: Some(1_700_000_020),
        },
    };

    let json = serde_json::to_value(&runtime).expect("CoreRuntime 应当能序列化");
    let label = "CoreRuntime";
    let rust = assert_ts_is_covered_by_rust(label, &json, label, &src);
    let ts = ts_interface_fields(&src, label);
    assert_eq!(
        ts.len(),
        10,
        "\nCoreRuntime: 从 types.ts 只解析出 {} 个字段，预期 10 个；\
         新增字段的话请同时更新这里与 types.ts。实际解析到: {ts:?}",
        ts.len()
    );
    let extra: Vec<_> = rust.difference(&ts).collect();
    assert!(
        extra.is_empty(),
        "\n{label}: Rust 提供了前端未声明的字段 {extra:?}\n\
         界面上这些字段会静默读到 undefined（`last_good_node` 就这样漂移过）。"
    );
    // 嵌套的 `recovery` 单独再比一次：它自己也是前端要读的形状。
    let recovery = json.get("recovery").expect("必须有 recovery 字段");
    let rust = assert_ts_is_covered_by_rust("RecoveryState", recovery, "RecoveryState", &src);
    let ts = ts_interface_fields(&src, "RecoveryState");
    assert_eq!(ts.len(), 6, "RecoveryState 预期 6 个字段，实际 {ts:?}");
    let extra: Vec<_> = rust.difference(&ts).collect();
    assert!(extra.is_empty(), "\nRecoveryState: Rust 多出字段 {extra:?}");
}
