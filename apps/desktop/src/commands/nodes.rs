//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

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
    let mut settings = state.with(|i| i.settings.clone()).ok_or(util::STATE_UNAVAILABLE)?;
    // 先把**选择**落盘：用户点了就是选了，后面无论成败都保留这个选择
    // （这正是「选择就使用」的最低要求 —— 连选择都没存住就谈不上）。
    settings.selected_node = Some(node_id.clone());
    persist_settings(&state, &settings)?;

    // 核心在跑就重启，让新节点立即生效（配置变更走重启，见 docs/03）。
    //
    // # 「选择就用」——本路径**不做任何回落**（2026-09-28 按用户裁决简化）
    //
    // 旧实现在新节点起不来时会**自动退回**上一个节点（`switch_plan.fallback_to`）。
    // 那条逻辑整个删掉了，因为它带来的坏处大于好处：
    //
    // * 用户点的是某台节点，App 却把出口换成另一台 —— 屏幕上的「已连接」背后
    //   是他没选的那条路。这是**行为上的撒谎**，不只是慢；
    // * 失败一次要跑**两遍完整拆建**（实测最坏 ≈ 30 秒），而它想避免的
    //   「用户断网」恰恰是用户自己点一下就能解决的事；
    // * 「哪台能用」本来就**不该由我们的探测来判**（见 `supervisor` 里
    //   「接管前门禁已删」那段注释：我们测不出用户真正在意的"流量能不能出去"）。
    //
    // 现在：失败就把**真实原因**原样交出去，**保留用户的选择**，隧道回到
    // 「没有接管」的干净状态。要不要换一台，由用户自己决定。
    let running = state.with(|i| i.runtime.running).unwrap_or(false);

    if running {
        let name = state
            .with(|i| i.nodes.iter().find(|n| n.id == node_id).map(|n| n.name.clone()))
            .unwrap_or(None)
            .unwrap_or_else(|| node_id.clone());
        state.log("app", "info", format!("正在切换到「{name}」，需要重建隧道（几秒）"));

        // 拆隧道。**这一步失败不算「已知失败」**：根本没拆成，旧核心可能还在跑 ——
        // 状态未知时凭不确定作废意图，会误伤一条可能仍然可用的连接
        // （判据见 `SwitchEnd::TeardownFailed`）。
        if let Err(e) = core::stop_core(&app, &state).await {
            settle_switch(&state, SwitchEnd::TeardownFailed);
            return Err(e);
        }

        // **失败不再回退到别的节点。**（旧实现在这里会把用户的出口悄悄换成
        // `last_good`/切换前那台 —— 见上面「选择就用」那段。）
        //
        // 但失败仍然**必须收尾**：旧的隧道已经拆了，新节点又没起来 ⇒ 隧道是断的。
        // 所以照旧作废「自动重连」意图（否则下次启动会拿这个刚被证明起不来的节点
        // 再接管一次网络，task-75 ①），并把**真实原因**交给用户。
        if let Err(e) = core::start_core(&app, &state, CoreStartTrigger::NodeSwitch).await {
            state.with(|i| {
                i.push_log(
                    "app",
                    "error",
                    format!("切到该节点失败（{e}）；隧道已还原，你选的节点保留"),
                )
            });
            settle_switch(&state, SwitchEnd::NoTunnel(FailureExit::NodeSwitchFailed));
            return Err(format!(
                "切到该节点失败：{e}\n\n隧道已还原；**你选的那台节点仍然选中**（选择就使用）。\n要不要换一台，由你决定。"
            ));
        }

        // 目标节点起来了 —— 用户想要的状态，意图保留。
        settle_switch(&state, SwitchEnd::TargetUp);
    }
    snapshot::build_snapshot(&app, &state).await
}

/// 一次节点切换把隧道留在了什么状态。**这是「意图保不保留」的判据输入**（task-75 ①）。
///
/// 为什么要有它：用户「换一个节点」**不等于**「想断开」——
/// 只有**切换失败、隧道没起来**才该作废旧意图；切换成功（或回退成功）必须保留，
/// 那正是用户想要的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchEnd {
    /// 目标节点起来了。
    TargetUp,
    /// 隧道是断的。带上 `FailureExit` 说明是哪个出口（源码守卫的锚点）。
    NoTunnel(FailureExit),
    /// 拆隧道那一步就失败了：**状态未知**（旧核心可能还在跑）。
    TeardownFailed,
}

/// 切换结束后，还要不要留住「用户希望连着」的意图。
///
/// * `TargetUp` → **留住**：切换动作本身不等于想断开；
/// * `NoTunnel(_)` → **作废**：否则下次启动会拿这个刚被证明起不来的节点
///   再接管一次网络（与 task-64 修的是同一族）。**删掉回落之后**，
///   `NoTunnel` 只剩一个出口（`NodeSwitchFailed`）——"切换失败但用户仍连着"
///   这种中间态不再存在（那正是回落的产物）；
/// * `TeardownFailed` → **留住**：根本没拆成、状态未知；凭不确定作废会误伤
///   一条可能仍然可用的连接。
pub(crate) fn switch_end_keeps_intent(end: SwitchEnd) -> bool {
    match end {
        SwitchEnd::TargetUp => true,
        SwitchEnd::NoTunnel(_) => false,
        SwitchEnd::TeardownFailed => true,
    }
}

/// 切换路径上**唯一**碰「自动重连」意图的地方：按结局决定要不要作废。
///
/// 成功 / 回退成功走这里是**故意什么都不做** —— 让「成功路径不动作废」成为一个
/// 可断言的调用（测试拿它对照失败路径），而不是「成功代码里恰好没写那行」。
pub(crate) fn settle_switch(state: &AppState, end: SwitchEnd) {
    if switch_end_keeps_intent(end) {
        return;
    }
    if let SwitchEnd::NoTunnel(exit) = end {
        core::invalidate_after_failure(state, exit);
    }
}

#[tauri::command]
pub async fn add_manual_node(
    app: AppHandle,
    state: State<'_, AppState>,
    link: String,
) -> Result<AppSnapshot, String> {
    // 用 parse_manual 而不是 parse_share_link：用户粘贴的可能是
    // 一条分享链接、一段订阅正文、或者一条 Clash proxy 定义。
    let outcome = xt_core::subscription::parse_manual(&link).map_err(util::user_msg)?;
    let node = outcome
        .nodes
        .into_iter()
        .next()
        .ok_or_else(|| "没能解析出节点".to_string())?;
    // 一次加锁完成「改内存 + 取出要落盘的两份数据」。
    // 这里曾经是两次 `state.with`：第一次的返回值被整个丢掉，紧接着再加锁
    // 克隆同样的两样东西 —— 多一次锁往返加一整份重复克隆。
    let (settings, nodes) = state
        .with(|i| {
            if !i.nodes.iter().any(|n| n.id == node.id) {
                i.nodes.push(node.clone());
            }
            if i.settings.selected_node.is_none() {
                i.settings.selected_node = Some(node.id.clone());
            }
            (i.settings.clone(), i.nodes.clone())
        })
        .ok_or(util::STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(util::user_msg)?;
    state.store.save_settings(&settings).map_err(util::user_msg)?;
    events::nodes_changed(&app);
    snapshot::build_snapshot(&app, &state).await
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
            // 失败账本也跟着走：节点都没了，它的「上次失败」再留着就是一条
            // 指不到任何东西的记录（同一个 id 将来复用时还会误标新节点）。
            i.node_health.remove(&node_id);
            if i.settings.selected_node.as_deref() == Some(node_id.as_str()) {
                i.settings.selected_node = i.nodes.first().map(|n| n.id.clone());
            }
            (i.settings.clone(), i.nodes.clone())
        })
        .ok_or(util::STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(util::user_msg)?;
    state.store.save_settings(&settings).map_err(util::user_msg)?;
    events::nodes_changed(&app);
    snapshot::build_snapshot(&app, &state).await
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
        .ok_or(util::STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(util::user_msg)?;
    state.store.save_subscriptions(&subs).map_err(util::user_msg)?;
    events::nodes_changed(&app);
    snapshot::build_snapshot(&app, &state).await
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
        .ok_or(util::STATE_UNAVAILABLE)?;

    if targets.is_empty() {
        return snapshot::build_snapshot(&app, &state).await;
    }

    let client = reqwest_lite();
    let mut messages = Vec::new();

    for sub in targets {
        // URL 里通常带着 token，日志里绝不打印完整 URL。
        let safe = redact_url(&sub.url);
        state.log("app", "info", format!("正在更新订阅 {safe}"));

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
    let (settings, nodes) = state.with(|i| (i.settings.clone(), i.nodes.clone())).ok_or(util::STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(util::user_msg)?;
    state.store.save_settings(&settings).map_err(util::user_msg)?;
    events::nodes_changed(&app);
    snapshot::build_snapshot(&app, &state).await
}

pub(crate) fn persist_subscriptions(state: &AppState) -> Result<(), String> {
    let subs = state.with(|i| i.subscriptions.clone()).ok_or(util::STATE_UNAVAILABLE)?;
    state.store.save_subscriptions(&subs).map_err(util::user_msg)
}

/// 极简 HTTP 客户端配置。用 `std::net` 之外的东西会引入新依赖，
/// 而这里只需要「GET 一个 URL、拿 body、带超时」。
pub(crate) fn reqwest_lite() -> HttpClientConfig {
    HttpClientConfig { timeout: Duration::from_secs(20), user_agent: format!("XrayTun/{}", env!("CARGO_PKG_VERSION")) }
}

/// 拉取订阅正文 + 解析 `subscription-userinfo` 响应头。
///
/// 用 `curl` 而不是引入 HTTP 客户端库：macOS 自带 `/usr/bin/curl`，
/// 支持 HTTPS（走系统信任链）、gzip、重定向，且零依赖。
/// 代价是不能复用连接 —— 对「一天更新几次订阅」这个频率完全无所谓。
pub(crate) async fn fetch_subscription(
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

/// 导出结果：分享链接 + 二维码 + 没能表达进链接的字段。
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeExport {
    pub node_id: String,
    pub node_name: String,
    /// 分享链接。可以直接复制粘贴，也是二维码的内容。
    pub uri: String,
    /// 二维码（SVG 内联，前端直接塞进 DOM，不额外请求图片）。
    pub svg: String,
    /// 分享链接表达不了、因此**没能带出去**的字段。
    /// 界面必须显示它 —— 静默丢弃是这类功能最容易犯的错。
    pub lost: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

        // -------------------------------------------------------------------
        // task-75 ①：**切换失败才作废意图；切换成功必须保留**
        //
        // 用户「换一个节点」不等于「想断开」。以前只有 `core.rs` 的**自动**回退
        // 调用点会作废意图，手动切换失败那条路不作废 ⇒ 隧道断了、盘上意图仍是
        // true ⇒ 下次启动拿这个刚被证明起不来的节点再接管一次网络。
        //
        // `select_node` 本身要 `AppHandle`（Tauri），所以这里钉的是它**每个出口
        // 都走**的那条收尾路径 `settle_switch`：真写盘、真读回（另开一个 `Store`
        // 实例 = 模拟重启后的读取）。「哪个出口调用了它」由 core.rs 的源码守卫
        // 测试保证（`every_failure_exit_still_invalidates_intent_in_production_source`）。
        // -------------------------------------------------------------------

        /// 纯判据：四种结局里，「隧道是断的」才作废。
        #[test]
        fn switch_end_intent_decision_is_pinned() {
            assert!(switch_end_keeps_intent(SwitchEnd::TargetUp), "切换成功 —— 用户要的就是这个");
            assert!(
                !switch_end_keeps_intent(SwitchEnd::NoTunnel(FailureExit::NodeSwitchFailed)),
                "隧道是断的 —— 必须作废意图（否则下次启动拿坏节点再接管一次网络）",
            );
            assert!(
                switch_end_keeps_intent(SwitchEnd::TeardownFailed),
                "拆都没拆成 = 状态未知（旧核心可能还在跑）—— 不作废",
            );
        }

        fn intent_store(tag: &str) -> xt_core::store::Store {
            let dir = std::env::temp_dir().join(format!("xt-nodes-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            xt_core::store::Store::new(dir)
        }

        /// 造一个「上次确实是连着的」盘上状态（`start_core` 成功时就是这样）。
        fn connected_state(tag: &str) -> (AppState, std::path::PathBuf) {
            let store = intent_store(tag);
            let dir = store.root().to_path_buf();
            let state = AppState::new(store);
            state.with(|i| i.settings.was_connected = true);
            let settings = state.with(|i| i.settings.clone()).unwrap();
            persist_settings(&state, &settings).unwrap();
            assert!(
                xt_core::store::Store::new(&dir).load_settings().was_connected,
                "前置条件：盘上先得有 true",
            );
            (state, dir)
        }

        /// **切换失败、隧道没起来 ⇒ 意图作废**（四个出口各验一遍）。
        #[test]
        fn failed_switch_drops_intent_on_disk() {
            // 删掉回落之后只剩一个出口：「切到该节点失败」。旧实现的三个
            // "回退也失败"出口随回落逻辑一起删掉了（`FailureExit::ALL` 里已无它们）。
            for exit in [FailureExit::NodeSwitchFailed] {
                let (state, dir) = connected_state("switch-fail");
                settle_switch(&state, SwitchEnd::NoTunnel(exit));
                // **从盘上读回来**：这就是「重启后」看到的东西。
                let after = xt_core::store::Store::new(&dir).load_settings();
                assert!(
                    !after.was_connected,
                    "切换失败（{exit:?}）后盘上必须是 false —— 否则下次启动会用这个坏节点自动重连",
                );
                assert!(
                    !should_auto_reconnect(after.was_connected, true, &ProxyMode::Tun, false),
                    "切换失败后不该自动重连",
                );
                let _ = std::fs::remove_dir_all(&dir);
            }
        }

        /// **独立反例：切换成功 ⇒ 意图必须原样留着。**
        ///
        /// 这条不许被上面那条吃掉：它证明「我们不是见切换就作废」。
        #[test]
        fn successful_switch_keeps_intent_on_disk() {
            for end in [SwitchEnd::TargetUp] {
                let (state, dir) = connected_state("switch-ok");
                settle_switch(&state, end);
                let after = xt_core::store::Store::new(&dir).load_settings();
                assert!(
                    after.was_connected,
                    "{end:?} 不是失败 —— 那正是用户想要的状态，意图不许被写掉",
                );
                assert!(
                    should_auto_reconnect(after.was_connected, true, &ProxyMode::Tun, false),
                    "{end:?} 之后重启仍应自动连回来",
                );
                let _ = std::fs::remove_dir_all(&dir);
            }
        }

        /// 拆隧道就失败 = 状态未知 ⇒ **不动意图**（理由见 `switch_end_keeps_intent`）。
        #[test]
        fn teardown_failure_leaves_intent_alone() {
            let (state, dir) = connected_state("switch-teardown");
            settle_switch(&state, SwitchEnd::TeardownFailed);
            assert!(
                xt_core::store::Store::new(&dir).load_settings().was_connected,
                "根本没拆成隧道（旧核心可能还在跑）—— 凭不确定作废会误伤一条仍可用的连接",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
}
