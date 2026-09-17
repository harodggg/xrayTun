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
    let previous = settings.selected_node.clone();
    settings.selected_node = Some(node_id.clone());
    persist_settings(&state, &settings)?;

    // 核心在跑就重启，让新节点立即生效（配置变更走重启，见 docs/03）。
    //
    // **这一步是几秒钟的拆建，不是瞬时切换**：Xray 没有配置热重载，
    // 换节点必须换配置、换配置必须重启核心。所以要有明确的过程提示 ——
    // 否则用户看到的就是「点了没反应，然后所有连接断一遍」。
    let running = state.with(|i| i.runtime.running).unwrap_or(false);
    let last_good = state.with(|i| i.runtime.last_good_node.clone()).unwrap_or(None);
    let plan = switch_plan(running, previous.as_deref(), last_good.as_deref(), &node_id);

    if plan.restart {
        let name = state
            .with(|i| i.nodes.iter().find(|n| n.id == node_id).map(|n| n.name.clone()))
            .unwrap_or(None)
            .unwrap_or_else(|| node_id.clone());
        state.with(|i| i.push_log("app", "info", format!("正在切换到「{name}」，需要重建隧道（几秒）")));

        core::stop_core(&app, &state).await?;

        // **这里失败必须回退。** 旧的隧道已经拆了，如果新节点起不来就直接
        // 把用户丢在断网状态 —— 而「新节点是坏的」是常见情况（实测有节点
        // TCP 可达却转发不了流量）。没有这一段，一次误选就是一次连环爆炸。
        if let Err(e) = core::start_core(&app, &state).await {
            state.with(|i| {
                i.push_log(
                    "app",
                    "error",
                    format!("切到该节点失败（{e}），正在退回上一个可用节点"),
                )
            });
            if let Some(back) = plan.fallback_to {
                let mut s2 = state.with(|i| i.settings.clone()).ok_or(util::STATE_UNAVAILABLE)?;
                s2.selected_node = Some(back);
                persist_settings(&state, &s2)?;
                core::start_core(&app, &state).await?;
            }
            return Err(format!("切到该节点失败，已退回：{e}"));
        }
    }
    snapshot::build_snapshot(&app, &state).await
}

/// 决定「要不要重启核心」以及「失败后退回哪里」。
///
/// * `previous` —— 这次切换**之前**选中的节点（正在跑的那台）。
/// * `last_good` —— 上一次**验证过能用**的节点。
///
/// 两者都可能是坏的，也都没有「接下来该用谁」的完整答案。关键约束：
/// **绝不为了回退而换到「刚刚失败的那台」**，也尽量不退回「已经不在运行的那台」。
pub(crate) fn switch_plan(
    running: bool,
    previous: Option<&str>,
    last_good: Option<&str>,
    target: &str,
) -> SwitchPlan {
    if !running {
        return SwitchPlan {
            restart: false,
            fallback_to: None,
        };
    }
    // 优先级：验证过的 > 切换前正在跑的。
    let candidate = last_good.or(previous);
    SwitchPlan {
        restart: true,
        // 等于目标节点时不算回退 —— 那正是刚刚失败的那台。
        fallback_to: candidate.filter(|c| *c != target).map(str::to_string),
    }
}

/// 切换节点时的决策。
///
/// 抽成纯函数是为了能测：这段逻辑决定「切换失败后用户会不会断网」，
/// 而它是整个 App 里最危险的链路（旧的隧道已经被拆掉了）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SwitchPlan {
    /// 核心在跑才需要重建隧道；没跑就只是改个选择。
    pub restart: bool,
    /// 新节点起不来时退回哪台。`None` = 无处可退。
    pub fallback_to: Option<String>,
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

        /// 正常情况：从 a 切到 b，b 起不来就退回 a —— 用户不该断网。
        #[test]
        fn switch_plan_falls_back_to_running_old_node() {
            let plan = switch_plan(true, Some("a"), None, "b");
            assert!(plan.restart);
            assert_eq!(plan.fallback_to.as_deref(), Some("a"));
        }
        /// 核心没在跑时，切换只是改个选择：不重启、也谈不上回退。
        #[test]
        fn switch_plan_is_inert_when_core_is_idle() {
            let plan = switch_plan(false, Some("a"), Some("a"), "b");
            assert!(!plan.restart, "核心没跑就不该重建隧道");
            assert_eq!(plan.fallback_to, None, "没跑就不用回退");
        }
        /// **回归测试（最容易把用户搞断网的那条边界）。**
        ///
        /// 场景：正在跑的是 b，`last_good` 也记着 b，用户要切到 c。
        ///
        /// `last_good` 因为等于**当时的选中节点** b 而被过滤掉，此时必须退到
        /// 「切换前正在跑的 b」，而不是退化成一个都不回退。
        /// 早先用 `last_good.or(previous)` 会得到 `Some(b)`，再被 `!= target`
        /// 过滤成 `None` —— 于是切换失败就直接断网。
        #[test]
        fn switch_plan_keeps_running_node_when_last_good_equals_previous() {
            let plan = switch_plan(true, Some("b"), Some("b"), "c");
            assert!(plan.restart);
            assert_eq!(
                plan.fallback_to.as_deref(),
                Some("b"),
                "切 c 失败时必须退回正在跑的 b"
            );
        }
        /// 从不回退到「刚刚失败的那台」：只有它可退时，宁可如实返回无处可退，
        /// 也不要假装回退成功而把用户丢在同一个坑里。
        #[test]
        fn switch_plan_never_falls_back_to_the_failed_target() {
            let plan = switch_plan(true, Some("x"), Some("x"), "x");
            assert!(plan.restart);
            assert_eq!(plan.fallback_to, None, "回退目标不能是刚失败的那台");
        }
        /// 验证过的节点优先于「切换前选中的节点（可能其实连不上）」。
        #[test]
        fn switch_plan_prefers_last_verified_node() {
            let plan = switch_plan(true, Some("broken"), Some("good"), "new");
            assert_eq!(plan.fallback_to.as_deref(), Some("good"));
        }
        /// 没有历史信息时（首次启动、记录被清）不能凭空编一个回退目标。
        #[test]
        fn switch_plan_reports_no_fallback_without_history() {
            let plan = switch_plan(true, None, None, "only-one");
            assert!(plan.restart);
            assert_eq!(plan.fallback_to, None);
        }
}
