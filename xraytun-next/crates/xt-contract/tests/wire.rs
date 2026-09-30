//! 线上形状的回归守卫。
//!
//! 这个文件存在的唯一原因是一个**真实事故**：`Response::Nodes(Vec<NodeView>)` 这种
//! 「内部 tag + 单字段包序列」的写法能编译通过，但序列化时会在**运行时**炸
//! （`cannot serialize tagged newtype variant ... containing a sequence`）。
//! 换句话说：类型系统不管这件事，只有"真的发一帧"才知道。
//!
//! 所以这里对每个变体做一次 **JSON → Frame → JSON 的 round-trip**：
//! 形状变了、或者某个变体根本发不出去，测试立刻红。
//!
//! 顺带把 wire 形状钉成可读的样例 —— 以后谁改了字段名，diff 在这里最明显。

use xt_contract::protocol::Frame;
use xt_contract::PROTOCOL_VERSION;

/// JSON → Frame → JSON，断言形状完全一致，并且不发生到 `Frame` 之外的漂移。
fn round_trip(wire: &str) {
    let frame: Frame = serde_json::from_str(wire)
        .unwrap_or_else(|e| panic!("反序列化失败: {e}\n  wire: {wire}"));
    let back = serde_json::to_value(&frame)
        .unwrap_or_else(|e| panic!("序列化失败（这类失败正是本文件要抓的）: {e}\n  wire: {wire}"));
    let original: serde_json::Value = serde_json::from_str(wire).expect("wire 本身应当是合法 JSON");
    assert_eq!(back, original, "round-trip 改变了形状\n  原: {wire}");
}

#[test]
fn request_shapes() {
    for wire in [
        r#"{"kind":"request","id":1,"request":{"op":"hello","client_version":"1.0.0","protocol_version":1}}"#,
        r#"{"kind":"request","id":2,"request":{"op":"subscribe","topics":["state","log","probe","notice"]}}"#,
        r#"{"kind":"request","id":3,"request":{"op":"status"}}"#,
        r#"{"kind":"request","id":4,"request":{"op":"connect","node_id":"n1","mode":"proxy"}}"#,
        r#"{"kind":"request","id":5,"request":{"op":"disconnect"}}"#,
        r#"{"kind":"request","id":6,"request":{"op":"switch_node","node_id":"n2"}}"#,
        r#"{"kind":"request","id":7,"request":{"op":"list_nodes"}}"#,
        r#"{"kind":"request","id":8,"request":{"op":"probe_nodes","node_ids":["n1","n2"]}}"#,
        r#"{"kind":"request","id":9,"request":{"op":"get_settings"}}"#,
        r#"{"kind":"request","id":10,"request":{"op":"patch_settings","patch":{"log_level":"debug"}}}"#,
        r#"{"kind":"request","id":11,"request":{"op":"list_subscriptions"}}"#,
        r#"{"kind":"request","id":12,"request":{"op":"add_subscription","url":"http://localhost:9/sub"}}"#,
        r#"{"kind":"request","id":13,"request":{"op":"refresh_subscription","id":"s1"}}"#,
        r#"{"kind":"request","id":14,"request":{"op":"tail_logs","lines":200}}"#,
    ] {
        round_trip(wire);
    }
}

/// 携带集合的应答是本次事故的现场：单独钉一次，并断言键名是 `nodes`/`subscriptions`/`logs`
/// （TS 侧按这个键名取值，改键名必须两边一起改）。
#[test]
fn collection_responses_use_named_keys() {
    for (wire, key) in [
        (r#"{"kind":"response","id":7,"outcome":{"status":"ok","response":{"result":"nodes","nodes":[]}}}"#, "nodes"),
        (
            r#"{"kind":"response","id":11,"outcome":{"status":"ok","response":{"result":"subscriptions","subscriptions":[]}}}"#,
            "subscriptions",
        ),
        (r#"{"kind":"response","id":14,"outcome":{"status":"ok","response":{"result":"logs","logs":[]}}}"#, "logs"),
    ] {
        round_trip(wire);
        let frame: Frame = serde_json::from_str(wire).expect("可反序列化");
        let value = serde_json::to_value(&frame).expect("可序列化");
        assert!(
            value["outcome"]["response"].get(key).is_some(),
            "应答缺少具名键 `{key}`：{value}"
        );
    }
}

#[test]
fn response_shapes() {
    for wire in [
        r#"{"kind":"response","id":1,"outcome":{"status":"error","error":{"code":"core_exited_early","message":"核心在就绪前退出"}}}"#,
        r#"{"kind":"response","id":2,"outcome":{"status":"ok","response":{"result":"accepted"}}}"#,
        r#"{"kind":"response","id":3,"outcome":{"status":"ok","response":{"result":"ok"}}}"#,
        r#"{"kind":"response","id":4,"outcome":{"status":"ok","response":{"result":"status","stage":"connected","datapath":{}}}}"#,
        r#"{"kind":"response","id":5,"outcome":{"status":"ok","response":{"result":"status","stage":"connecting","phase":"starting_core","datapath":{"pid":4242}}}}"#,
        r#"{"kind":"response","id":6,"outcome":{"status":"ok","response":{"result":"hello","daemon_version":"1.0.0","protocol_version":1,"capabilities":["proxy_mode","stats"],"pid":1,"started_at_ms":1}}}"#,
        r#"{"kind":"response","id":7,"outcome":{"status":"ok","response":{"result":"subscribed","topics":["state"]}}}"#,
        r#"{"kind":"response","id":8,"outcome":{"status":"ok","response":{"result":"settings","socks_listen":"127.0.0.1:1080","log_level":"info"}}}"#,
    ] {
        round_trip(wire);
    }
}

/// `stats: None` 与 `stats: Some(0)` 必须是两种不同的线上形状 ——
/// 这是 I3（未知 ≠ 0）在协议层的落点。
#[test]
fn unknown_stats_is_not_zero() {
    let unsampled = r#"{"kind":"response","id":1,"outcome":{"status":"ok","response":{"result":"status","stage":"connected","datapath":{}}}}"#;
    let zero = r#"{"kind":"response","id":1,"outcome":{"status":"ok","response":{"result":"status","stage":"connected","datapath":{},"stats":{"uplink_bytes":0,"downlink_bytes":0,"sampled_at_ms":1}}}}"#;
    let a: serde_json::Value = serde_json::to_value(serde_json::from_str::<Frame>(unsampled).unwrap()).unwrap();
    let b: serde_json::Value = serde_json::to_value(serde_json::from_str::<Frame>(zero).unwrap()).unwrap();
    assert_ne!(a, b, "未采样与 0 字节在线上必须是可区分的两种形状");
    assert!(a["outcome"]["response"].get("stats").is_none(), "未采样的帧里不该出现 stats 键");
}

#[test]
fn error_codes_are_closed_and_have_no_fallback() {
    // I2 的类型级判据：失败分类里不许出现"再试一次/换条路"这一类成员。
    let forbidden = ["retry", "fallback", "degraded", "failover"];
    for code in [
        "invalid_request",
        "not_found",
        "conflict",
        "permission_denied",
        "datapath_unavailable",
        "config_invalid",
        "core_exited_early",
        "helper_unavailable",
        "unsupported",
        "io",
        "internal",
    ] {
        assert!(!forbidden.contains(&code), "错误码 `{code}` 属于被否决的回落类");
        let body = xt_contract::error::ErrorBody::new(
            serde_json::from_str(&format!("\"{code}\"")).expect("是合法 ErrorCode"),
            "x",
        );
        assert_eq!(body.code.as_str(), code, "ErrorCode 的线上字符串必须稳定");
    }
}

#[test]
fn event_shapes() {
    for wire in [
        r#"{"kind":"event","seq":1,"event":{"event":"state","view":{"stage":"disconnected","datapath":{}}}}"#,
        r#"{"kind":"event","seq":2,"event":{"event":"log","line":{"ts_ms":1,"level":"info","target":"xt-datapath","message":"就绪"}}}"#,
        r#"{"kind":"event","seq":3,"event":{"event":"probe","result":{"node_id":"n1","ttfb_ms":42,"at_ms":1}}}"#,
        r#"{"kind":"event","seq":4,"event":{"event":"notice","notice":{"severity":"warning","code":"conflict","message":"当前状态不允许该操作","at_ms":1}}}"#,
    ] {
        round_trip(wire);
    }
}

#[test]
fn serde_rejects_sequence_newtype_under_internal_tag() {
    // 这是事故的最小复现，也是"为什么必须写成结构体变体"的活证据：
    // 内部 tag + 单字段包序列 → 编译期没问题，序列化时运行时报错。
    // 如果哪天 serde 改了行为，这条会红，提醒我们重新检查 Response 的形状约定。
    #[derive(serde::Serialize)]
    #[serde(tag = "result", rename_all = "snake_case")]
    enum BadShape {
        Nodes(Vec<u32>),
    }

    let err = serde_json::to_string(&BadShape::Nodes(vec![1, 2]))
        .expect_err("serde 本应拒绝这种形状；若不再报错，说明约定需要重新评估");
    let message = err.to_string();
    assert!(
        message.contains("sequence"),
        "serde 的报错文案变了（现在：{message}），请更新这条守卫的断言"
    );
}

#[test]
fn protocol_version_is_declared() {
    assert_eq!(PROTOCOL_VERSION, 1, "改协议版本必须同步改这里与文档");
}
