//! xt-state 的转换矩阵与不变量测试。
//!
//! 这里的 orcale 用「字符串标签」独立描述每个状态，而不是复用 State 自身的比较：
//! 如果测试直接拿 apply 的结果去对 apply 的实现，就等于没测。标签表是手写的契约。

use xt_contract::error::{ErrorBody, ErrorCode};
use xt_contract::model::{ConnectPhase, NodeId, RunMode, Stage, StatsView};
use xt_state::{apply, begin_connect, begin_switch, to_view, Action, Signal, State, Transition};

fn node(id: &str) -> NodeId {
    NodeId::new(id)
}

fn boom() -> ErrorBody {
    ErrorBody::new(ErrorCode::CoreExitedEarly, "核心在就绪前退出")
}

/// 覆盖每个状态变体（Connecting 的 mode 差异用单独状态覆盖，因为 CoreReady 之后
/// proxy/tun 分叉）。
fn states() -> Vec<State> {
    vec![
        State::Disconnected { last_error: None },
        State::Connecting {
            phase: ConnectPhase::PreparingConfig,
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: None,
            ready_at_ms: None,
        },
        State::Connecting {
            phase: ConnectPhase::StartingCore,
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: None,
            ready_at_ms: None,
        },
        State::Connecting {
            phase: ConnectPhase::AwaitingReady,
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: Some(7),
            ready_at_ms: None,
        },
        State::Connecting {
            phase: ConnectPhase::AwaitingReady,
            mode: RunMode::Tun,
            node_id: node("n1"),
            pid: Some(7),
            ready_at_ms: None,
        },
        State::Connecting {
            phase: ConnectPhase::CommittingRoutes,
            mode: RunMode::Tun,
            node_id: node("n1"),
            pid: Some(7),
            ready_at_ms: Some(1000),
        },
        State::Connected {
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: 7,
            ready_at_ms: 1000,
            connected_since_ms: 1000,
        },
        State::Connected {
            mode: RunMode::Tun,
            node_id: node("n1"),
            pid: 7,
            ready_at_ms: 1000,
            connected_since_ms: 2000,
        },
        State::Disconnecting { mode: RunMode::Proxy, node_id: node("n1"), pid: Some(7) },
        State::Disconnecting { mode: RunMode::Tun, node_id: node("n1"), pid: None },
    ]
}

fn signals() -> Vec<(&'static str, Signal)> {
    vec![
        ("config_ready", Signal::ConfigReady),
        ("core_started", Signal::CoreStarted { pid: 8 }),
        ("core_ready", Signal::CoreReady { at_ms: 1234 }),
        ("routes_committed", Signal::RoutesCommitted),
        ("core_exited", Signal::CoreExited { error: boom() }),
        ("disconnected", Signal::Disconnected),
    ]
}

fn tag(state: &State) -> &'static str {
    match state {
        State::Disconnected { .. } => "disconnected",
        State::Connecting { phase: ConnectPhase::PreparingConfig, .. } => "preparing_config",
        State::Connecting { phase: ConnectPhase::StartingCore, .. } => "starting_core",
        State::Connecting { phase: ConnectPhase::AwaitingReady, mode: RunMode::Proxy, .. } => {
            "awaiting_ready_proxy"
        }
        State::Connecting { phase: ConnectPhase::AwaitingReady, mode: RunMode::Tun, .. } => {
            "awaiting_ready_tun"
        }
        State::Connecting { phase: ConnectPhase::CommittingRoutes, .. } => "committing_routes",
        State::Connected { mode: RunMode::Proxy, .. } => "connected_proxy",
        State::Connected { mode: RunMode::Tun, .. } => "connected_tun",
        State::Disconnecting { mode: RunMode::Proxy, .. } => "disconnecting_proxy",
        State::Disconnecting { mode: RunMode::Tun, .. } => "disconnecting_tun",
    }
}

/// 手写契约：`Err(())` = 必须返回 conflict。任何没列出的组合都必须是 conflict，
/// 所以「忘了写」会立刻变成测试失败，而不是被默认放过。
fn want(state_tag: &str, sig: &str) -> Result<(&'static str, &'static [Action]), ()> {
    use Action::*;
    match (state_tag, sig) {
        // 连接推进
        ("preparing_config", "config_ready") => Ok(("starting_core", &[SpawnCore, PublishState])),
        ("starting_core", "core_started") => {
            Ok(("awaiting_ready_proxy", &[AwaitCoreReady, PublishState]))
        }
        ("awaiting_ready_proxy", "core_ready") => Ok(("connected_proxy", &[PublishState])),
        ("awaiting_ready_tun", "core_ready") => {
            Ok(("committing_routes", &[CommitRoutes, PublishState]))
        }
        ("committing_routes", "routes_committed") => Ok(("connected_tun", &[PublishState])),

        // 失败：全部落回 disconnected
        ("preparing_config", "core_exited") => Ok(("disconnected", &[PublishState])),
        ("starting_core", "core_exited")
        | ("awaiting_ready_proxy", "core_exited")
        | ("awaiting_ready_tun", "core_exited")
        | ("committing_routes", "core_exited")
        | ("connected_proxy", "core_exited")
        | ("connected_tun", "core_exited")
        | ("disconnecting_proxy", "core_exited")
        | ("disconnecting_tun", "core_exited") => Ok(("disconnected", &[StopCore, PublishState])),

        // 断开意图
        ("connected_proxy", "disconnected") => Ok(("disconnecting_proxy", &[StopCore, PublishState])),
        ("connected_tun", "disconnected") => Ok(("disconnecting_tun", &[StopCore, PublishState])),
        ("preparing_config", "disconnected")
        | ("starting_core", "disconnected")
        | ("awaiting_ready_proxy", "disconnected") => {
            Ok(("disconnecting_proxy", &[StopCore, PublishState]))
        }
        ("awaiting_ready_tun", "disconnected") | ("committing_routes", "disconnected") => {
            Ok(("disconnecting_tun", &[StopCore, PublishState]))
        }

        // 断开完成：正常停止不带失败
        ("disconnecting_proxy", "disconnected") | ("disconnecting_tun", "disconnected") => {
            Ok(("disconnected", &[PublishState]))
        }

        _ => Err(()),
    }
}

#[test]
fn transition_matrix_is_exhaustive() {
    let mut checked = 0;
    for state in states() {
        for (sig_name, signal) in signals() {
            checked += 1;
            let got = apply(&state, signal, 4242);
            let want = want(tag(&state), sig_name);
            let ctx = format!("{} + {}", tag(&state), sig_name);
            match (got, want) {
                (Ok(t), Ok((want_tag, want_actions))) => {
                    assert_eq!(tag(&t.state), want_tag, "{ctx}: 结果状态不符");
                    assert_eq!(t.actions, want_actions, "{ctx}: 动作不符");
                    assert!(
                        t.actions.contains(&Action::PublishState),
                        "{ctx}: 每次成功迁移都必须 PublishState"
                    );
                    // 失败信号必须把真实原因带进 Disconnected；正常断开必须不带失败。
                    if sig_name == "core_exited" && want_tag == "disconnected" {
                        match &t.state {
                            State::Disconnected { last_error: Some(e) } => {
                                assert_eq!(e.code, ErrorCode::CoreExitedEarly, "{ctx}")
                            }
                            other => panic!("{ctx}: 失败路径必须带 last_error，实得 {other:?}"),
                        }
                    }
                    if sig_name == "disconnected" && want_tag == "disconnected" {
                        assert_eq!(
                            t.state,
                            State::Disconnected { last_error: None },
                            "{ctx}: 正常断开完成不带失败"
                        );
                    }
                }
                (Err(e), Err(())) => {
                    assert_eq!(e.code, ErrorCode::Conflict, "{ctx}: 非法迁移必须是 conflict");
                }
                (got, want) => {
                    let want_desc = want.map(|(t, a)| (t, a.to_vec()));
                    panic!("{ctx}: 与手写契约不符，实得 {got:?}，期望 {want_desc:?}");
                }
            }
        }
    }
    assert_eq!(checked, states().len() * signals().len());
}

#[test]
fn proxy_connect_only_reaches_connected_after_core_ready() {
    let started = begin_connect(&State::disconnected(), RunMode::Proxy, node("n1")).unwrap();
    assert_eq!(
        started.state,
        State::Connecting {
            phase: ConnectPhase::PreparingConfig,
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: None,
            ready_at_ms: None,
        }
    );
    assert_eq!(started.actions, vec![Action::PrepareConfig, Action::PublishState]);
    assert_ne!(started.state.stage(), Stage::Connected);

    let spawned = apply(&started.state, Signal::ConfigReady, 5).unwrap();
    assert_eq!(tag(&spawned.state), "starting_core");
    assert_ne!(spawned.state.stage(), Stage::Connected);

    let awaiting = apply(&spawned.state, Signal::CoreStarted { pid: 4242 }, 6).unwrap();
    assert_eq!(tag(&awaiting.state), "awaiting_ready_proxy");
    assert_eq!(awaiting.actions, vec![Action::AwaitCoreReady, Action::PublishState]);
    // 还没收到 CoreReady，界面不能出现「已连接」。
    assert_ne!(awaiting.state.stage(), Stage::Connected);

    // now_ms 故意取一个和 at_ms 不同的值：证明 connected_since_ms 来自真实观测，不是本地计数。
    let connected = apply(&awaiting.state, Signal::CoreReady { at_ms: 1111 }, 9999).unwrap();
    assert_eq!(
        connected.state,
        State::Connected {
            mode: RunMode::Proxy,
            node_id: node("n1"),
            pid: 4242,
            ready_at_ms: 1111,
            connected_since_ms: 1111,
        }
    );

    let view = to_view(&connected.state, None, None);
    assert_eq!(view.stage, Stage::Connected);
    assert_eq!(view.connected_since_ms, Some(1111));
    assert_eq!(view.datapath.pid, Some(4242));
    assert_eq!(view.datapath.ready_at_ms, Some(1111));
    // 未采样就是 None，绝不显示 0。
    assert_eq!(view.stats, None);
    assert_eq!(view.node_id, Some(node("n1")));
    assert_eq!(view.mode, Some(RunMode::Proxy));
}

#[test]
fn tun_connect_requires_routes_before_connected() {
    let t = begin_connect(&State::disconnected(), RunMode::Tun, node("n2")).unwrap();
    let t = apply(&t.state, Signal::ConfigReady, 1).unwrap();
    let t = apply(&t.state, Signal::CoreStarted { pid: 11 }, 2).unwrap();
    let t = apply(&t.state, Signal::CoreReady { at_ms: 111 }, 3).unwrap();
    assert_eq!(tag(&t.state), "committing_routes");
    assert_ne!(t.state.stage(), Stage::Connected);
    assert_eq!(t.actions, vec![Action::CommitRoutes, Action::PublishState]);

    let t = apply(&t.state, Signal::RoutesCommitted, 777).unwrap();
    assert_eq!(
        t.state,
        State::Connected {
            mode: RunMode::Tun,
            node_id: node("n2"),
            pid: 11,
            ready_at_ms: 111,
            connected_since_ms: 777,
        }
    );
    let view = to_view(&t.state, None, None);
    assert_eq!(view.stage, Stage::Connected);
    // tun：ready_at_ms 是核心就绪时刻，connected_since_ms 是路由提交时刻，两者都来自真实时钟。
    assert_eq!(view.datapath.ready_at_ms, Some(111));
    assert_eq!(view.connected_since_ms, Some(777));
}

#[test]
fn bare_signals_on_disconnected_are_conflict() {
    let state = State::disconnected();
    for (name, signal) in signals() {
        let err = apply(&state, signal, 1).expect_err("disconnected 上任何信号都应被拒绝");
        assert_eq!(err.code, ErrorCode::Conflict, "{name}");
    }
    // 契约点名的例子：Disconnected 收到 CoreReady。
    let err = apply(&state, Signal::CoreReady { at_ms: 1 }, 1).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[test]
fn every_failure_path_lands_in_disconnected_with_error() {
    for state in states() {
        if matches!(state, State::Disconnected { .. }) {
            continue;
        }
        let error = boom();
        let t = apply(&state, Signal::CoreExited { error: error.clone() }, 9).unwrap();
        match &t.state {
            State::Disconnected { last_error: Some(recorded) } => assert_eq!(recorded, &error),
            other => panic!("{other:?} 不是带 last_error 的 Disconnected"),
        }
        // 调用方哪怕忘了传 last_error，状态机自己记录的失败也必须显示出来。
        let view = to_view(&t.state, None, None);
        assert_eq!(view.stage, Stage::Disconnected);
        assert_eq!(view.last_error, Some(error));
        assert!(t.actions.contains(&Action::PublishState));
    }
}

#[test]
fn disconnect_flow_reports_no_failure() {
    let connected = State::Connected {
        mode: RunMode::Proxy,
        node_id: node("n1"),
        pid: 7,
        ready_at_ms: 10,
        connected_since_ms: 10,
    };
    let t = apply(&connected, Signal::Disconnected, 11).unwrap();
    assert_eq!(t.actions, vec![Action::StopCore, Action::PublishState]);
    assert_eq!(tag(&t.state), "disconnecting_proxy");

    let t2 = apply(&t.state, Signal::Disconnected, 12).unwrap();
    assert_eq!(t2.state, State::Disconnected { last_error: None });
    assert_eq!(to_view(&t2.state, None, None).last_error, None);
}

#[test]
fn connection_view_never_claims_connected_unless_connected() {
    for state in states() {
        let view = to_view(&state, None, None);
        assert_eq!(view.stage, state.stage());
        if view.stage == Stage::Connected {
            assert!(matches!(state, State::Connected { .. }), "{state:?} 谎报了 Connected");
            assert!(view.connected_since_ms.is_some());
            assert!(view.datapath.pid.is_some());
        } else {
            assert_eq!(view.connected_since_ms, None, "{state:?}");
        }
    }
}

#[test]
fn stats_passthrough_and_error_precedence() {
    let stats = StatsView { uplink_bytes: 10, downlink_bytes: 20, sampled_at_ms: 30 };
    let view = to_view(&State::disconnected(), Some(stats), None);
    assert_eq!(view.stats, Some(stats));

    let recorded = boom();
    let newer = ErrorBody::new(ErrorCode::Io, "更晚的一次失败");
    let disconnected = State::Disconnected { last_error: Some(recorded.clone()) };
    assert_eq!(to_view(&disconnected, None, None).last_error, Some(recorded.clone()));
    assert_eq!(to_view(&disconnected, None, Some(newer.clone())).last_error, Some(newer));
}

#[test]
fn begin_connect_rejects_non_idle_states() {
    for state in states() {
        let result = begin_connect(&state, RunMode::Proxy, node("n9"));
        if matches!(state, State::Disconnected { .. }) {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::Conflict, "{state:?}");
        }
    }
}

#[test]
fn begin_switch_only_from_connected_and_stops_before_reconnecting() {
    let connected = State::Connected {
        mode: RunMode::Tun,
        node_id: node("old"),
        pid: 7,
        ready_at_ms: 10,
        connected_since_ms: 10,
    };
    let t = begin_switch(&connected, node("new")).unwrap();
    assert_eq!(
        t.state,
        State::Connecting {
            phase: ConnectPhase::PreparingConfig,
            mode: RunMode::Tun,
            node_id: node("new"),
            pid: None,
            ready_at_ms: None,
        }
    );
    // 先停旧核心，再准备新配置；动作里没有任何「回到 old」的东西。
    assert_eq!(
        t.actions,
        vec![Action::StopCore, Action::PrepareConfig, Action::PublishState]
    );

    // 不回落：切过去失败就停在 Disconnected + last_error，绝不回到旧节点。
    let error = boom();
    let mut path = vec![t.state.clone()];
    let t1 = apply(&path[0], Signal::ConfigReady, 20).unwrap();
    path.push(t1.state.clone());
    let t2 = apply(&t1.state, Signal::CoreExited { error: error.clone() }, 21).unwrap();
    path.push(t2.state.clone());
    for (i, state) in path.iter().enumerate() {
        match state {
            State::Connected { node_id, .. } => panic!("第 {i} 步回到了已连接 {node_id}"),
            State::Connecting { node_id, .. } if node_id == &node("old") => {
                panic!("第 {i} 步回到了旧节点")
            }
            _ => {}
        }
    }
    assert_eq!(t2.state, State::Disconnected { last_error: Some(error) });

    // 其他阶段不接受切换意图。
    for state in states() {
        let result = begin_switch(&state, node("new"));
        if matches!(state, State::Connected { .. }) {
            assert!(result.is_ok(), "{state:?}");
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::Conflict, "{state:?}");
        }
    }
}

#[test]
fn transition_never_panics_on_any_pair() {
    // 穷举一遍，确认没有配对会 panic / 产生 internal（internal 只允许出现在
    // 状态机不变量被破坏时，而这里所有状态都是可构造的合法状态）。
    for state in states() {
        for (_, signal) in signals() {
            match apply(&state, signal, 0) {
                Ok(t) => {
                    assert!(matches!(
                        t.state.stage(),
                        Stage::Disconnected | Stage::Connecting | Stage::Connected | Stage::Disconnecting
                    ));
                }
                Err(e) => assert_eq!(e.code, ErrorCode::Conflict),
            }
        }
    }
    let _: fn(&State, Signal, u64) -> Result<Transition, ErrorBody> = apply;
}
