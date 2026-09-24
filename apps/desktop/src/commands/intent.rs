//! 意图过滤的 UI 入口。
//!
//! # 这一层的职责只有"翻译"
//!
//! 判定、缓存、审计全在 `crate::intent` 与 `xt-intent` 里。这里只做三件事：
//! 加锁取状态、把动作转成一次调用、返回最新的 [`IntentSummary`]。
//! **不在这里写任何判定逻辑** —— 否则界面看到的与数据面走的就是两套规则。
//!
//! # 「待生效」是这一层最需要说清楚的东西
//!
//! 规则**在核心启动时**才下发（设计文档 §10 的 P2b-2）。所以：
//!
//! * `intent_apply` 是**唯一**会把规则真正推给核心的动作，它会重连一次；
//! * 核心没在跑时调它**不会**报错 —— 那时规则会在下次启动时自然带上，
//!   报错只会让用户以为出了问题。

use tauri::{AppHandle, State};

use crate::state::AppState;

/// 当前意图过滤的运行态。
#[tauri::command]
pub async fn intent_status(state: State<'_, AppState>) -> Result<crate::intent::IntentSummary, String> {
    state
        .with(|i| i.intent.summary())
        .ok_or_else(|| "读取意图过滤状态失败（状态锁不可用）".to_string())
}

/// 用户纠正一个误杀：把它加进放行带。
///
/// 动作（直连 / 走代理）由界面显式给 —— 我们**不替用户猜**他想要哪种路由，
/// 因为那会顺手改变那个域名的分流行为（设计文档 §6 的"不做静默行为变更"）。
#[tauri::command]
pub async fn intent_allow(
    state: State<'_, AppState>,
    host: String,
    action: Option<xt_core::model::IntentAllowAction>,
) -> Result<crate::intent::IntentSummary, String> {
    let action = action.unwrap_or_default();
    let ok = state.with(|i| {
        // 先落到设置里（**要持久化**：用户纠正过的域名不该下次开机又拦一遍），
        // 再同步到运行中的引擎。
        let host_norm = host.trim().to_ascii_lowercase();
        i.settings.intent.allow_overrides.retain(|o| o.host != host_norm);
        i.settings
            .intent
            .allow_overrides
            .push(xt_core::model::IntentAllowOverride { host: host_norm.clone(), action });
        let mapped = match action {
            xt_core::model::IntentAllowAction::Direct => xt_intent::rules::AllowAction::Direct,
            xt_core::model::IntentAllowAction::Proxy => xt_intent::rules::AllowAction::Proxy,
        };
        i.intent.allow_now(&host_norm, mapped)
    });
    let Some(ok) = ok else {
        return Err("写入放行纠正失败（状态锁不可用）".into());
    };
    if !ok {
        return Err(format!("「{host}」不是一个有效的域名，没有写进放行名单"));
    }
    // 设置也要落盘 —— 这是用户做出的决定，不是运行时状态。
    let settings = state.with(|i| i.settings.clone()).ok_or("读取设置失败")?;
    crate::state::persist_settings(&state, &settings).map_err(|e| e.to_string())?;
    state.log("intent", "info", format!("已把 {host} 加入意图放行名单（{}）", action_zh(action)));

    state
        .with(|i| i.intent.summary())
        .ok_or_else(|| "读取意图过滤状态失败".to_string())
}

fn action_zh(action: xt_core::model::IntentAllowAction) -> &'static str {
    match action {
        xt_core::model::IntentAllowAction::Direct => "直连",
        xt_core::model::IntentAllowAction::Proxy => "走代理",
    }
}

/// 清空判决缓存。返回清掉的条数。
#[tauri::command]
pub async fn intent_clear_cache(state: State<'_, AppState>) -> Result<usize, String> {
    let n = state.with(|i| i.intent.clear_cache()).unwrap_or(0);
    state.log("intent", "info", format!("已清空意图判决缓存（{n} 条）"));
    Ok(n)
}

/// 最近若干条审计（时间倒序由界面决定，这里按文件顺序给）。
#[tauri::command]
pub async fn intent_audit(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<xt_intent::audit::AuditRecord>, String> {
    let limit = limit.unwrap_or(200).min(2000);
    // 读盘放到阻塞线程池：审计文件可能有几 MB，直接在 async 里读会占住执行器。
    let path = state
        .with(|i| i.intent.audit_path())
        .ok_or_else(|| "读取审计路径失败".to_string())?;
    tauri::async_runtime::spawn_blocking(move || xt_intent::audit::AuditLog::tail(&path, limit).unwrap_or_default())
        .await
        .map_err(|e| format!("读取审计失败：{e}"))
}

/// 某个域名为什么是这个结论（"为什么"面板）。
#[tauri::command]
pub async fn intent_explain(
    state: State<'_, AppState>,
    host: String,
) -> Result<Option<xt_intent::cache::CacheEntry>, String> {
    Ok(state.with(|i| i.intent.explain(&host)).flatten())
}

/// **把待生效的规则真正推给核心** —— 唯一会重连一次的动作。
///
/// 核心没在跑时不报错：那时规则会在下次启动自然带上（并已被标记为"已应用"）。
#[tauri::command]
pub async fn intent_apply(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<crate::intent::IntentSummary, String> {
    let (needs, running) = state
        .with(|i| (i.intent.needs_apply(), i.runtime.running))
        .ok_or_else(|| "读取状态失败".to_string())?;

    if !needs {
        state.log("intent", "info", "意图规则没有待生效的变更，未动核心");
        return state.with(|i| i.intent.summary()).ok_or_else(|| "读取状态失败".into());
    }

    if !running {
        // 没有核心可重启 ⇒ 规则会在下次启动带上。这里只把"已应用"的账记对。
        let now = xt_core::util::now_unix();
        state.with(|i| i.intent.mark_applied(now));
        state.log("intent", "info", "核心未运行：意图规则将在下次连接时带上");
        return state.with(|i| i.intent.summary()).ok_or_else(|| "读取状态失败".into());
    }

    // 重连一次：这是**用户明确点的动作**，不是我们自作主张（见模块文档）。
    super::core::stop_core(&app, &state).await?;
    super::core::start_core(&app, &state, super::core::CoreStartTrigger::IntentRulesApply).await?;
    state.with(|i| i.intent.summary()).ok_or_else(|| "读取状态失败".to_string())
}
