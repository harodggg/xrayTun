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
    // `auto_reconnect` 曾经登记在这里，理由是「界面目前没有对应控件」。
    // **这条理由在 `task-71` 之后已经过期**（那一步给设置页加了控件），
    // **而把它声明进 `types.ts` 是 `task-89`（`383cd3d`）做的** —— `task-71` 当时**有意**没改 `types.ts`
    // （避免与并发改动撞车），是用两个辅助符号绕开类型的；`task-89` 才撤掉那个旁路并补上声明。
    // 声明一落地，本登记表就必须删掉这一条（否则本测试会报「登记了但已不存在」）。
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

/// 这两个是快照里的**嵌套结构**（`AppSnapshot.helper` / `AppSnapshot.core`），
/// 各自的 TS 接口单独声明，单独比对是因为它们不走 AppSnapshot 的字段集。
///
/// `HelperAvailability` 早先还有一个独立命令（`probe_helper`）直接返回它，
/// task-94 复跑证据确认前端 0 调用点后把那条命令删了；结构本身仍在快照里用。
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

// ---------------------------------------------------------------------------
// task-96：命令契约 —— `generate_handler!` 注册集 vs `ipc.ts` 的 `invoke()` 字面量集
// ---------------------------------------------------------------------------

fn lib_rs() -> String {
    let p = repo_root().join("apps/desktop/src/lib.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}: {e}", p.display()))
}

fn ipc_ts() -> String {
    let p = repo_root().join("apps/ui/src/ipc.ts");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}: {e}", p.display()))
}

/// 去掉注释，**保留长度**（注释字节换成空格），并且尊重字符串字面量。
///
/// 为什么必须尊重字面量：若注释里出现 `//`，朴素地"从 `//` 截到行尾"会把
/// 同一行真正的调用（或字符串里的 URL）一起吃掉 —— 那正是**假绿**的来源。
/// 换行原样保留，免得把两行粘成一行后产生新的误匹配。
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            out[i] = c;
            if c == b'\\' && i + 1 < b.len() {
                out[i + 1] = b[i + 1];
                i += 2;
            } else {
                if c == q {
                    quote = None;
                }
                i += 1;
            }
            continue;
        }
        match c {
            b'"' | b'\'' | b'`' => {
                quote = Some(c);
                out[i] = c;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => {
                out[i] = c;
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

/// Rust 侧真源：`lib.rs` 里 `invoke_handler(tauri::generate_handler![…])` 的注册列表。
///
/// 取的是**注册**不是定义：`#[tauri::command]` 只说明"这个函数能当命令"，
/// 只有进了 `generate_handler!` 才会出现在派发表里（task-94 实测：
/// 删注册、留定义时编译与 clippy 都没有任何信号）。
fn registered_commands(lib_rs_src: &str) -> BTreeSet<String> {
    let src = strip_comments(lib_rs_src);
    let at = src
        .find("generate_handler!")
        .expect("lib.rs 里应当有 generate_handler!");
    let open = src[at..]
        .find('[')
        .map(|i| at + i)
        .expect("generate_handler! 后面应当有 [");
    let mut depth = 0usize;
    let mut close = None;
    for (off, ch) in src[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + off);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close.expect("generate_handler![ 应当有配对的 ]");
    src[open + 1..close]
        .split("commands::")
        .skip(1)
        .filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// TS 侧真源：`ipc.ts` 里每个 `invoke(…)` 调用的第一个参数。
///
/// 返回 `(字符串字面量命令名, 非字面量调用的原文片段)`。
///
/// **真源是 `invoke()` 里的字符串**，不是 `export const api = { … }` 的属性名：
/// `start_proxy` 的封装叫 `start`、`stop_proxy` 的叫 `stop` —— task-94 的审计
/// 按属性名换算 camelCase，正是因此把它们误报成死代码。
fn ipc_invoke_calls(src: &str) -> (BTreeSet<String>, Vec<String>) {
    let src = strip_comments(src);
    let b = src.as_bytes();
    let mut literals = BTreeSet::new();
    let mut non_literal = Vec::new();
    let mut i = 0usize;
    while let Some(pos) = src[i..].find("invoke") {
        let at = i + pos;
        let after = at + "invoke".len();
        let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let prev_ok = at == 0 || !ident(b[at - 1]);
        let next_ok = after >= b.len() || !ident(b[after]);
        i = after;
        if !prev_ok || !next_ok {
            continue;
        }
        let mut j = skip_ws(b, after);
        if b.get(j) == Some(&b'<') {
            // 跳过泛型实参（`invoke<AppSnapshot>("snapshot")`），支持嵌套。
            let mut depth = 0usize;
            while j < b.len() {
                match b[j] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            j = skip_ws(b, j);
        }
        if b.get(j) != Some(&b'(') {
            continue; // `import { invoke }`、文档里提到的 `invoke` 字样
        }
        let arg = skip_ws(b, j + 1);
        if b.get(arg) == Some(&b'"') {
            let mut k = arg + 1;
            while k < b.len() && b[k] != b'"' {
                k += 1;
            }
            literals.insert(src[arg + 1..k.min(b.len())].to_string());
        } else {
            let end = (arg + 60).min(b.len());
            non_literal.push(
                src[arg..end]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
        }
    }
    (literals, non_literal)
}

/// 前端 `invoke()` 传**非字面量**的豁免清单（显式、且会被打印出来）。
///
/// 目前为空：`ipc.ts` 的 37 个调用全是字符串字面量。留这个常量是为了将来真有
/// 动态命令名时有个**显式出口** —— 而不是让测试静默跳过（那种"跳过"就是假绿）。
const NON_LITERAL_INVOKE_EXEMPT: &[&str] = &[];

/// **跨语言命令契约**：`lib.rs` 的 `generate_handler!` 注册集，必须与
/// `apps/ui/src/ipc.ts` 里 `invoke("<名字>")` 的字面量集**双向相等**。
///
/// # 为什么需要它（task-94 实测出的**单向**盲区）
///
/// * 删定义、留注册 → 编译器当场红（`error[E0433]: cannot find __cmd__…`），**挡得住**；
/// * **删注册、留定义 → 编译 0、clippy 0 warning，毫无信号**（`pub` + `pub use`
///   不会被判 dead_code），只有运行期 invoke 到未知命令才会暴露。本测试补的就是它。
///
/// # 它挡不住什么（别高估）
///
/// * 只比**名字**：不校验**参数形状**（`{ nodeId }` 写成 `{ id }` 照样通过）、
///   不校验 `ipc.ts` 封装的返回类型、也不校验运行期是否真的注册成功；
/// * 不做 TS/Rust 解析：是**行文约定**级的扫描，故两侧数量都写死做哨兵
///   （解析退化 ⇒ 数量断言先红，不会两边都空而"相等"）。
#[test]
fn registered_commands_match_the_frontend_invoke_literals() {
    let rust = registered_commands(&lib_rs());
    let (ts, non_literal) = ipc_invoke_calls(&ipc_ts());

    // 非字面量调用**不许静默跳过**：打印清单，未登记的一律红。
    let exempt: BTreeSet<&str> = NON_LITERAL_INVOKE_EXEMPT.iter().copied().collect();
    let unexpected: Vec<&String> = non_literal
        .iter()
        .filter(|s| !exempt.contains(s.as_str()))
        .collect();
    println!(
        "ipc.ts 非字面量 invoke：{} 处 {non_literal:?}；显式豁免清单：{NON_LITERAL_INVOKE_EXEMPT:?}",
        non_literal.len()
    );
    assert!(
        unexpected.is_empty(),
        "\nipc.ts 里有 invoke() 的第一个参数不是字符串字面量，契约测试无法判定它调的是哪个命令 \
         —— 请改成字面量，或把它加进 NON_LITERAL_INVOKE_EXEMPT（会随本测试一起打印）：\n  {unexpected:?}\n"
    );

    // 哨兵：数量写死，增删命令时必须同步改这里（否则解析退化会假绿）。
    // 37 = 34（task-94 时的全集）+ task-130 的三个 `incident_*`（`incident_anomalies`
    // **故意不注册**：前端没封装它、角标只用计数 —— Lead 在 task-130 里裁决）。
    assert_eq!(
        rust.len(),
        37,
        "\nlib.rs 的 generate_handler! 注册了 {} 个命令，预期 37。\
         增删命令请同步更新这个数字与 ipc.ts。实际注册: {rust:?}",
        rust.len()
    );
    assert_eq!(
        ts.len(),
        37,
        "\nipc.ts 的 invoke 字面量有 {} 个，预期 37；实际: {ts:?}",
        ts.len()
    );

    // canary：专盯 task-94 那次误报的成因 —— 按封装名换算 camelCase 会漏掉它们。
    for canary in ["start_proxy", "stop_proxy"] {
        assert!(
            ts.contains(canary),
            "ipc.ts 里应当有 invoke(\"{canary}\")（它的封装名是 start/stop，不是 camelCase）"
        );
    }

    assert_eq!(
        rust,
        ts,
        "\n命令契约必须双向相等：\n  只在 Rust 注册、前端没声明: {:?}\n  只在前端声明、Rust 没注册: {:?}\n",
        rust.difference(&ts).collect::<Vec<_>>(),
        ts.difference(&rust).collect::<Vec<_>>()
    );
}
