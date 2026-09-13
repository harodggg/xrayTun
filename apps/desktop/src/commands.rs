//! Tauri 命令层：UI 能调用的全部入口。
//!
//! # 约定
//!
//! * 所有命令返回 `Result<T, String>`。`String` 是**给用户看的**中文消息，
//!   不是调试信息 —— 上游错误在 `map_err` 里已经被翻译成人话。
//! * 每个会改变状态或系统的命令，成功后都返回最新的 [`AppSnapshot`]，
//!   让 UI 一次拿全，避免「调完再查一次」产生的竞态和额外 IPC。
//! * **绝不跨 `.await` 持有 `inner` 锁**。耗时动作（起进程、改网络、跑探针）
//!   都在锁外做，做完再短暂加锁写回结果。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use xt_core::model::{AppSettings, Node, ProxyMode, Subscription};
use xt_core::xray;
use xt_proto::{Request, DEFAULT_SOCKET_PATH};

use crate::events;
use crate::state::{
    persist_settings, AppSnapshot, CoreAvailability, CoreRuntime, HelperAvailability,
};
use crate::AppState;

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn snapshot(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    build_snapshot(&app, &state).await
}

async fn build_snapshot(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let helper_socket = DEFAULT_SOCKET_PATH;
    let socket_present = crate::helper_client::socket_present(std::path::Path::new(helper_socket));

    let mut guard = state.helper.lock().await;
    let helper = guard.availability(socket_present);
    drop(guard);

    let core = core_availability(app, state);

    state
        .with(|inner| AppSnapshot {
            settings: inner.settings.clone(),
            subscriptions: inner.subscriptions.clone(),
            nodes: inner.nodes.clone(),
            runtime: inner.runtime.clone(),
            latency: inner.latencies.clone(),
            traffic: inner.traffic.clone(),
            notice: inner.last_notice.clone(),
            helper,
            core,
            app_version: app.package_info().version.to_string(),
        })
        .ok_or_else(|| "应用状态不可用".to_string())
}

fn core_availability(app: &AppHandle, state: &AppState) -> CoreAvailability {
    let explicit = state.with(|i| i.settings.core_path.clone()).flatten();
    let resource_dir = app.path().resource_dir().ok();

    let dev_dir = crate::dev_binaries_dir();
    match xray::resolve_core_binary(explicit.as_deref(), resource_dir.as_deref(), dev_dir.as_deref()) {
        Ok(path) => {
            let version = std::process::Command::new(&path)
                .arg("version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string());
            let supports = version
                .as_deref()
                .map(crate::supervisor::core_supports_native_tun)
                .unwrap_or(false);
            CoreAvailability {
                path: Some(path),
                version,
                error: None,
                supports_native_tun: supports,
                min_native_tun_version: xray::MIN_CORE_VERSION_NATIVE_TUN.to_string(),
            }
        }
        Err(e) => CoreAvailability {
            path: None,
            version: None,
            error: Some(e.to_string()),
            supports_native_tun: false,
            min_native_tun_version: xray::MIN_CORE_VERSION_NATIVE_TUN.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSnapshot, String> {
    persist_settings(&state, &settings)?;

    // 「显示网速」是个纯展示开关，不该为了它重启核心。这里立刻按新设置
    // 重画一次标题；核心没在跑时用全 0 的采样，等价于恢复成 App 名字。
    let (traffic, show) = state
        .with(|i| (i.traffic.clone(), i.settings.show_speed_in_title))
        .unwrap_or_default();
    crate::traffic::update_titles(&app, &traffic, show);

    events::settings_changed(&app);
    build_snapshot(&app, &state).await
}

/// 切换运行模式。**会重启核心**（如果之前正在运行）。
#[tauri::command]
pub async fn set_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: ProxyMode,
) -> Result<AppSnapshot, String> {
    let mut settings = state
        .with(|i| i.settings.clone())
        .ok_or_else(|| "应用状态不可用".to_string())?;
    let was_running = state.with(|i| i.runtime.running).unwrap_or(false);
    settings.mode = mode;
    persist_settings(&state, &settings)?;

    if was_running {
        stop_core(&app, &state).await?;
    }
    if mode != ProxyMode::Direct {
        start_core(&app, &state).await?;
    } else {
        state.with(|i| {
            i.runtime = CoreRuntime::default();
            i.push_log("app", "info", "已切换到直连模式");
        });
    }
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 核心启停
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_core(&app, &state).await?;
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    stop_core(&app, &state).await?;
    state.with(|i| {
        i.runtime = CoreRuntime::default();
    });
    build_snapshot(&app, &state).await
}

async fn start_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
    // 先在锁外把需要的数据克隆出来。
    let (settings, nodes) = state
        .with(|i| (i.settings.clone(), i.nodes.clone()))
        .ok_or_else(|| "应用状态不可用".to_string())?;

    if settings.mode == ProxyMode::Direct {
        return Err("当前是直连模式，请先切换到「系统代理」或「TUN」".into());
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xray::CoreEvent>();
    let resource_dir = app.path().resource_dir().ok();

    let mut supervisor = state.supervisor.lock().await;
    let mut helper = state.helper.lock().await;

    let result = supervisor
        .start(
            &state.store,
            &settings,
            &nodes,
            &mut helper,
            Some(tx),
            crate::supervisor::CoreSearchPaths {
                app_resource_dir: resource_dir,
                dev_binaries_dir: crate::dev_binaries_dir(),
            },
        )
        .await;
    drop(helper);
    drop(supervisor);

    let runtime = match result {
        Ok(rt) => rt,
        Err(e) => {
            state.with(|i| {
                i.runtime = CoreRuntime { running: false, last_error: Some(e.clone()), ..Default::default() };
                i.push_log("app", "error", format!("启动失败：{e}"));
            });
            events::runtime_changed(app, state);
            return Err(e);
        }
    };

    state.with(|i| {
        i.runtime = runtime.clone();
        i.push_log(
            "app",
            "info",
            format!(
                "核心已启动（pid {:?}，模式 {}，隧道会话 {:?}）",
                runtime.pid,
                settings.mode.as_str(),
                runtime.tun_session
            ),
        );
    });
    events::runtime_changed(app, state);

    // 日志转发任务：核心的 stdout/stderr → 状态环形缓冲 + UI 事件。
    let app_handle = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let level = classify_log(&event.line);
            if let Some(state) = app_handle.try_state::<AppState>() {
                state.with(|i| i.push_log("core", level, event.line.clone()));
            }
            let _ = app_handle.emit(events::CORE_LOG, events::LogPayload { line: event.line, level: level.into() });
        }
    });

    // 流量采样任务：跟着核心一起生灭（见 traffic.rs 顶部注释）。
    // 先收掉可能还在跑的上一个 —— 切换节点会 stop + start，
    // 忘了收就会有两个任务同时往 state.traffic 里写。
    let monitor = crate::traffic::spawn(app.clone(), xt_core::xray::config::API_PORT);
    state.with(|i| {
        if let Some(old) = i.traffic_task.replace(monitor) {
            old.abort();
        }
    });

    Ok(())
}

async fn stop_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let mut supervisor = state.supervisor.lock().await;
    let mut helper = state.helper.lock().await;
    let result = supervisor.stop(&mut helper).await;
    drop(helper);
    drop(supervisor);

    state.with(|i| {
        // 采样任务必须先收掉：核心没了，api 端口也没人监听，
        // 留着它只会每秒产生一次连接失败。
        if let Some(monitor) = i.traffic_task.take() {
            monitor.abort();
        }
        i.traffic = crate::state::TrafficSample::default();
        i.runtime.running = false;
        i.runtime.pid = None;
        i.runtime.tun_session = None;
        match &result {
            Ok(()) => i.push_log("app", "info", "核心已停止，网络配置已回滚"),
            Err(e) => {
                i.runtime.last_error = Some(e.clone());
                i.push_log("app", "error", format!("停止过程中出错：{e}"));
            }
        }
    });
    // 采样任务已经收掉，标题会永远停在最后一拍的读数上 —— 手动清掉。
    let show = state.with(|i| i.settings.show_speed_in_title).unwrap_or(true);
    crate::traffic::update_titles(app, &crate::state::TrafficSample::default(), show);

    events::runtime_changed(app, state);
    result
}

/// 从 Xray 的日志行里粗分级别，让 UI 能做颜色区分。
fn classify_log(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();
    if lower.contains("failed") || lower.contains("error") || lower.contains("rejected") {
        "error"
    } else if lower.contains("warning") || lower.contains("warn") {
        "warn"
    } else if lower.contains("debug") {
        "debug"
    } else {
        "info"
    }
}

// ---------------------------------------------------------------------------
// 节点与订阅
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn select_node(
    app: AppHandle,
    state: State<'_, AppState>,
    node_id: String,
) -> Result<AppSnapshot, String> {
    let exists = state.with(|i| i.nodes.iter().any(|n| n.id == node_id)).unwrap_or(false);
    if !exists {
        return Err("找不到该节点".into());
    }
    let mut settings = state.with(|i| i.settings.clone()).ok_or("应用状态不可用")?;
    settings.selected_node = Some(node_id);
    persist_settings(&state, &settings)?;

    // 核心在跑就重启，让新节点立即生效（配置变更走重启，见 docs/03）。
    if state.with(|i| i.runtime.running).unwrap_or(false) {
        stop_core(&app, &state).await?;
        start_core(&app, &state).await?;
    }
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn add_manual_node(
    app: AppHandle,
    state: State<'_, AppState>,
    link: String,
) -> Result<AppSnapshot, String> {
    // 用 parse_manual 而不是 parse_share_link：用户粘贴的可能是
    // 一条分享链接、一段订阅正文、或者一条 Clash proxy 定义。
    let outcome = xt_core::subscription::parse_manual(&link).map_err(|e| e.to_string())?;
    let node = outcome
        .nodes
        .into_iter()
        .next()
        .ok_or_else(|| "没能解析出节点".to_string())?;
    state.with(|i| {
        if !i.nodes.iter().any(|n| n.id == node.id) {
            i.nodes.push(node.clone());
        }
        if i.settings.selected_node.is_none() {
            i.settings.selected_node = Some(node.id.clone());
        }
        let settings = i.settings.clone();
        let nodes = i.nodes.clone();
        (settings, nodes)
    });
    let (settings, nodes) = state.with(|i| (i.settings.clone(), i.nodes.clone())).ok_or("应用状态不可用")?;
    state.store.save_nodes(&nodes).map_err(|e| e.to_string())?;
    state.store.save_settings(&settings).map_err(|e| e.to_string())?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn delete_node(
    app: AppHandle,
    state: State<'_, AppState>,
    node_id: String,
) -> Result<AppSnapshot, String> {
    let (settings, nodes) = state
        .with(|i| {
            i.nodes.retain(|n| n.id != node_id);
            i.latencies.remove(&node_id);
            if i.settings.selected_node.as_deref() == Some(node_id.as_str()) {
                i.settings.selected_node = i.nodes.first().map(|n| n.id.clone());
            }
            (i.settings.clone(), i.nodes.clone())
        })
        .ok_or("应用状态不可用")?;
    state.store.save_nodes(&nodes).map_err(|e| e.to_string())?;
    state.store.save_settings(&settings).map_err(|e| e.to_string())?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn add_subscription(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    url: String,
) -> Result<AppSnapshot, String> {
    let sub = Subscription {
        id: format!("sub{}", crate::state::now_unix()),
        name: if name.trim().is_empty() { url.clone() } else { name },
        url,
        enabled: true,
        update_interval_hours: 24,
        last_updated: None,
        last_error: None,
        node_count: 0,
        usage: None,
    };
    state.with(|i| i.subscriptions.push(sub.clone()));
    persist_subscriptions(&state)?;
    events::subscriptions_changed(&app);

    // 加完立刻拉一次，用户不用再点「更新」。
    refresh_subscriptions(app.clone(), state, Some(vec![sub.id])).await
}

#[tauri::command]
pub async fn remove_subscription(
    app: AppHandle,
    state: State<'_, AppState>,
    subscription_id: String,
) -> Result<AppSnapshot, String> {
    let (nodes, subs) = state
        .with(|i| {
            i.subscriptions.retain(|s| s.id != subscription_id);
            // 同时清掉该订阅带来的节点，否则会留下永远更新不到的孤儿。
            i.nodes.retain(|n| match &n.source {
                xt_core::model::NodeSource::Subscription { id } => id != &subscription_id,
                xt_core::model::NodeSource::Manual => true,
            });
            (i.nodes.clone(), i.subscriptions.clone())
        })
        .ok_or("应用状态不可用")?;
    state.store.save_nodes(&nodes).map_err(|e| e.to_string())?;
    state.store.save_subscriptions(&subs).map_err(|e| e.to_string())?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

/// 拉取并解析订阅。`ids` 为 `None` 表示全部更新。
#[tauri::command]
pub async fn refresh_subscriptions(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
) -> Result<AppSnapshot, String> {
    let targets: Vec<Subscription> = state
        .with(|i| {
            i.subscriptions
                .iter()
                .filter(|s| s.enabled && ids.as_ref().map(|v| v.contains(&s.id)).unwrap_or(true))
                .cloned()
                .collect()
        })
        .ok_or("应用状态不可用")?;

    if targets.is_empty() {
        return build_snapshot(&app, &state).await;
    }

    let client = reqwest_lite();
    let mut messages = Vec::new();

    for sub in targets {
        // URL 里通常带着 token，日志里绝不打印完整 URL。
        let safe = redact_url(&sub.url);
        state.with(|i| i.push_log("app", "info", format!("正在更新订阅 {safe}")));

        match fetch_subscription(&client, &sub.url).await {
            Ok((body, usage)) => match xt_core::subscription::parse_any(&body) {
                Ok(outcome) => {
                    let count = outcome.nodes.len();
                    state.with(|i| {
                        // 原子替换：先移掉该订阅的旧节点，再插入新解析出的节点。
                        i.nodes.retain(|n| match &n.source {
                            xt_core::model::NodeSource::Subscription { id } => id != &sub.id,
                            xt_core::model::NodeSource::Manual => true,
                        });
                        for mut node in outcome.nodes {
                            node.source = xt_core::model::NodeSource::Subscription { id: sub.id.clone() };
                            if !i.nodes.iter().any(|n| n.id == node.id) {
                                i.nodes.push(node);
                            }
                        }
                        if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                            s.last_updated = Some(crate::state::now_unix());
                            s.last_error = None;
                            s.node_count = count;
                            s.usage = usage.clone();
                        }
                        i.push_log(
                            "app",
                            "info",
                            format!("订阅更新完成：{count} 个节点（跳过 {} 行）", outcome.warnings.len()),
                        );
                        for w in outcome.warnings.iter().take(5) {
                            i.push_log("app", "warn", w.clone());
                        }
                    });
                    messages.push(format!("{count} 个节点"));
                }
                Err(e) => {
                    let msg = format!("订阅 {safe} 解析失败：{e}");
                    state.with(|i| {
                        i.push_log("app", "error", msg.clone());
                        if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                            s.last_error = Some(e.to_string());
                        }
                    });
                    messages.push(msg);
                }
            },
            Err(e) => {
                let msg = format!("订阅 {safe} 拉取失败：{e}");
                state.with(|i| {
                    i.push_log("app", "error", msg.clone());
                    if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                        s.last_error = Some(e.to_string());
                    }
                });
                messages.push(msg);
            }
        }
    }

    state.with(|i| {
        // 选中节点可能在更新中消失了，回退到第一个可用节点。
        let still_valid = i
            .settings
            .selected_node
            .as_deref()
            .map(|id| i.nodes.iter().any(|n| n.id == id))
            .unwrap_or(false);
        if !still_valid {
            i.settings.selected_node = i.nodes.first().map(|n| n.id.clone());
        }
        i.last_notice = Some(messages.join("；"));
    });
    persist_subscriptions(&state)?;
    let (settings, nodes) = state.with(|i| (i.settings.clone(), i.nodes.clone())).ok_or("应用状态不可用")?;
    state.store.save_nodes(&nodes).map_err(|e| e.to_string())?;
    state.store.save_settings(&settings).map_err(|e| e.to_string())?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 延迟探测
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn test_latency(
    app: AppHandle,
    state: State<'_, AppState>,
    node_ids: Option<Vec<String>>,
) -> Result<AppSnapshot, String> {
    let (nodes, core_path) = state
        .with(|i| {
            let nodes: Vec<Node> = i
                .nodes
                .iter()
                .filter(|n| node_ids.as_ref().map(|v| v.contains(&n.id)).unwrap_or(true))
                .cloned()
                .collect();
            (nodes, i.settings.core_path.clone())
        })
        .ok_or("应用状态不可用")?;

    if nodes.is_empty() {
        return Err("没有可测试的节点".into());
    }

    let resource_dir = app.path().resource_dir().ok();
    let dev_dir = crate::dev_binaries_dir();
    let binary = xray::resolve_core_binary(core_path.as_deref(), resource_dir.as_deref(), dev_dir.as_deref())
        .map_err(|e| e.to_string())?;

    state.with(|i| i.push_log("app", "info", format!("开始测试 {} 个节点的延迟", nodes.len())));
    events::probe_started(&app, nodes.len());

    let started = Instant::now();
    let results = crate::supervisor::probe(&nodes, &binary, Duration::from_secs(5))
        .await
        .map_err(|e| {
            state.with(|i| i.push_log("app", "error", format!("探测失败：{e}")));
            e
        })?;

    let ok_count = results.iter().filter(|r| r.ok()).count();
    state.with(|i| {
        for r in &results {
            i.latencies.insert(r.node_id.clone(), r.clone());
        }
        i.push_log(
            "app",
            "info",
            format!(
                "探测完成：{ok_count}/{} 可用，耗时 {:.1}s",
                results.len(),
                started.elapsed().as_secs_f32()
            ),
        );
    });
    events::latency_updated(&app, &results);
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// helper 管理
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn probe_helper(state: State<'_, AppState>) -> Result<HelperAvailability, String> {
    let present = crate::helper_client::socket_present(std::path::Path::new(DEFAULT_SOCKET_PATH));
    let mut helper = state.helper.lock().await;
    // 强制重连，拿到最新状态。
    helper.disconnect();
    Ok(helper.availability(present))
}

#[tauri::command]
pub async fn install_helper(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::install_script(&app)?;
    crate::helper_install::run_with_admin(&script, "安装 XrayTun 网络配置助手")?;
    state.with(|i| i.push_log("app", "info", "helper 安装完成"));
    build_snapshot(&app, &state).await
}

/// 重启 helper。
///
/// 对应 UI 上「helper 已安装但进程没在运行」那个状态的一键修复。
#[tauri::command]
pub async fn restart_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::restart_script();
    crate::helper_install::run_with_admin(&script, "重启 XrayTun 网络配置助手")?;
    // 连接状态可能已变，强制重连一次。
    state.helper.lock().await.disconnect();
    state.with(|i| i.push_log("app", "info", "helper 已重启"));
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn uninstall_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::uninstall_script();
    crate::helper_install::run_with_admin(&script, "卸载 XrayTun 网络配置助手")?;
    state.with(|i| i.push_log("app", "warn", "helper 已卸载"));
    build_snapshot(&app, &state).await
}

/// 回滚磁盘上遗留的会话。网络出问题时的「一键修复」。
#[tauri::command]
pub async fn restore_stale(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let mut helper = state.helper.lock().await;
    let response = helper.call(&Request::Restore);
    drop(helper);

    match response {
        Ok(_) => {
            state.with(|i| i.push_log("app", "info", "已请求 helper 回滚遗留会话"));
        }
        Err(e) => {
            state.with(|i| i.push_log("app", "error", format!("回滚失败：{}", e.message)));
            return Err(e.message);
        }
    }
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 日志与诊断
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn tail_logs(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<crate::state::LogEntry>, String> {
    let limit = limit.unwrap_or(400);
    state
        .with(|i| {
            let skip = i.logs.len().saturating_sub(limit);
            i.logs.iter().skip(skip).cloned().collect::<Vec<_>>()
        })
        .ok_or_else(|| "应用状态不可用".to_string())
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    state.with(|i| i.logs.clear());
    Ok(())
}

/// 生成一份可直接贴给维护者的诊断报告。
///
/// 刻意**不包含**订阅 URL、节点地址、UUID/password 等敏感信息 —— 用户会把它
/// 贴到公开的 issue 里，所以在生成端就把它们抹掉，而不是指望用户自己删。
#[tauri::command]
pub async fn diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let snap = build_snapshot(&app, &state).await?;
    let mut out = String::new();
    out.push_str(&format!("XrayTun {}\n", snap.app_version));
    out.push_str(&format!("macOS: {}\n", macos_version()));
    out.push_str(&format!("架构: {}\n", std::env::consts::ARCH));
    out.push_str(&format!("模式: {}\n", snap.settings.mode.as_str()));
    out.push_str(&format!(
        "内核: {:?} / {:?}（原生 TUN 支持: {}，需要 >= {}）\n",
        snap.core.path, snap.core.version, snap.core.supports_native_tun, snap.core.min_native_tun_version
    ));
    out.push_str(&format!(
        "helper: 已安装={} 可连接={} 版本={:?} 隧道活跃={}\n",
        snap.helper.socket_present, snap.helper.reachable, snap.helper.version, snap.helper.tun_active
    ));
    if let Some(e) = &snap.helper.error {
        out.push_str(&format!("helper 错误: {e}\n"));
    }
    out.push_str(&format!(
        "数据目录: {}\n",
        state.store.root().display()
    ));
    out.push_str(&format!(
        "配置: socks={} http={} 允许局域网={} TUN 网段={} MTU={}\n",
        snap.settings.socks_port,
        snap.settings.http_port,
        snap.settings.allow_lan,
        snap.settings.tun.network,
        snap.settings.tun.mtu
    ));
    out.push_str(&format!(
        "订阅数: {}，节点数: {}\n",
        snap.subscriptions.len(),
        snap.nodes.len()
    ));
    out.push_str("\n最近日志:\n");
    if let Some(logs) = state.with(|i| i.logs.iter().rev().take(50).cloned().collect::<Vec<_>>()) {
        for entry in logs.into_iter().rev() {
            out.push_str(&format!("[{}] {} {}\n", entry.source, entry.level, redact_secrets(&entry.message)));
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn open_data_dir(state: State<'_, AppState>) -> Result<(), String> {
    let root = state.store.root().to_path_buf();
    std::process::Command::new("/usr/bin/open")
        .arg(&root)
        .status()
        .map_err(|e| format!("打开数据目录失败：{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn persist_subscriptions(state: &AppState) -> Result<(), String> {
    let subs = state.with(|i| i.subscriptions.clone()).ok_or("应用状态不可用")?;
    state.store.save_subscriptions(&subs).map_err(|e| e.to_string())
}

/// 极简 HTTP 客户端配置。用 `std::net` 之外的东西会引入新依赖，
/// 而这里只需要「GET 一个 URL、拿 body、带超时」。
fn reqwest_lite() -> HttpClientConfig {
    HttpClientConfig { timeout: Duration::from_secs(20), user_agent: format!("XrayTun/{}", env!("CARGO_PKG_VERSION")) }
}

pub struct HttpClientConfig {
    pub timeout: Duration,
    pub user_agent: String,
}

/// 拉取订阅正文 + 解析 `subscription-userinfo` 响应头。
///
/// 用 `curl` 而不是引入 HTTP 客户端库：macOS 自带 `/usr/bin/curl`，
/// 支持 HTTPS（走系统信任链）、gzip、重定向，且零依赖。
/// 代价是不能复用连接 —— 对「一天更新几次订阅」这个频率完全无所谓。
async fn fetch_subscription(
    cfg: &HttpClientConfig,
    url: &str,
) -> Result<(String, Option<xt_core::model::SubscriptionUsage>), String> {
    let output = tokio::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--compressed",
            "--max-time",
            &cfg.timeout.as_secs().to_string(),
            "--user-agent",
            &cfg.user_agent,
            "--dump-header",
            "-",
            url,
        ])
        .output()
        .await
        .map_err(|e| format!("调用 curl 失败：{e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    // curl 把 header 和 body 一起输出到 stdout，中间用一个空行分隔。
    let (headers, body) = match raw.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_string(), b.to_string()),
        None => (String::new(), raw.to_string()),
    };

    let usage = headers
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.to_ascii_lowercase()
                .starts_with("subscription-userinfo:")
                .then(|| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                .flatten()
        })
        .map(|v| xt_core::model::SubscriptionUsage::parse_header(&v));

    Ok((body, usage))
}

/// 抹掉 URL 里的凭据部分，只保留 host。
///
/// 机场订阅的 URL 里带 token，用户把日志贴出来就等于把订阅泄漏了。
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}://{}/…", u.scheme(), u.host_str().unwrap_or("<unknown>")),
        Err(_) => "<非法 URL>".to_string(),
    }
}

/// 日志里可能出现的凭据特征串（UUID、长 token）做粗粒度脱敏。
fn redact_secrets(line: &str) -> String {
    line.split_whitespace()
        .map(|token| {
            let looks_like_uuid = token.len() == 36 && token.matches('-').count() == 4;
            if looks_like_uuid {
                "<uuid>".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn macos_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 让 `PathBuf` 在诊断输出里可读。
pub fn display_path(p: &Option<PathBuf>) -> String {
    p.as_ref().map(|x| x.display().to_string()).unwrap_or_else(|| "<未找到>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_classification_covers_real_xray_lines() {
        assert_eq!(classify_log("2026/01/01 00:00:00 [Warning] failed to dial"), "error");
        assert_eq!(classify_log("[Info] Xray 26.9.9 started"), "info");
        assert_eq!(classify_log("something debug level"), "debug");
    }

    #[test]
    fn url_redaction_hides_credentials() {
        let redacted = redact_url("https://example.com/sub?token=SECRET123");
        assert!(!redacted.contains("SECRET123"), "{redacted}");
        assert!(redacted.contains("example.com"));
        assert_eq!(redact_url("not a url"), "<非法 URL>");
    }

    #[test]
    fn uuid_is_redacted_from_logs() {
        let line = "user b831381d-6324-4d53-ad4f-8cda48b30811 connected";
        let out = redact_secrets(line);
        assert!(!out.contains("b831381d"), "{out}");
        assert!(out.contains("<uuid>"));
    }

    #[test]
    fn display_path_handles_none() {
        assert_eq!(display_path(&None), "<未找到>");
    }
}
