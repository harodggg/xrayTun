//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSnapshot, String> {
    // 登录项要对齐到设置里的期望值。
    //
    // 放在 persist 之前：apply 是幂等的（已经是目标状态就什么都不做），
    // 所以每保存一次设置都会走到这里，不会有副作用。
    // 失败不阻断保存 —— 其余设置该存还是要存，登录项的问题单独报给用户。
    if let Err(e) = crate::login_item::apply(settings.launch_at_login) {
        tracing::warn!(error = %e, "同步开机自启动设置失败");
        state.with(|i| {
            i.push_log("app", "warn", format!("设置开机自启动失败：{e}"));
            i.last_notice = Some(format!("设置开机自启动失败：{e}"));
        });
    }

    // **`was_connected` 归后端所有，前端不得覆盖它。**
    //
    // 它是「用户希望它连着」这个**意图**，只有 `core::start_core`（连上）和
    // `stop_proxy`（用户主动点停止）能改。
    //
    // 而前端保存的是**整份** `AppSettings`，那份快照可能是**连接之前**取的
    // —— 于是用户只是改了个「显示网速」或日志级别，就把意图悄悄清成了
    // false，下一次开机自然不自动连。
    //
    // 这是 docs/08 的 A 类：一个字段的**来源**（后端）和**去向**（前端整份回传）
    // 不是同一个地方。凡是"前端不拥有"的字段，都不能让整份回传覆盖它。
    let mut settings = settings;
    if let Some(intent) = state.with(|i| i.settings.was_connected) {
        if settings.was_connected != intent {
            tracing::debug!(
                from = settings.was_connected,
                to = intent,
                "忽略前端回传的 was_connected（它由后端拥有）"
            );
        }
        settings.was_connected = intent;
    }

    persist_settings(&state, &settings)?;

    // 「显示网速」是个纯展示开关，不该为了它重启核心。这里立刻按新设置
    // 重画一次标题；核心没在跑时用全 0 的采样，等价于恢复成 App 名字。
    let (traffic, show) = state
        .with(|i| (i.traffic.clone(), i.settings.show_speed_in_title))
        .unwrap_or_default();
    crate::traffic::update_titles(&app, &traffic, show);

    events::settings_changed(&app);
    snapshot::build_snapshot(&app, &state).await
}

/// 开关开机自启动。
#[tauri::command]
pub async fn set_launch_at_login(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<AppSnapshot, String> {
    crate::login_item::apply(enabled)?;

    // 设置字段跟着系统的真实结果走，而不是跟着请求走 ——
    // 注册成功了但需要用户批准时，字段要不要置 true？
    // 由 status() 决定，避免又造出一个「字段和现实不一致」的状态。
    let actual = crate::login_item::status()?;
    let mut settings = state.with(|i| i.settings.clone()).ok_or(util::STATE_UNAVAILABLE)?;
    settings.launch_at_login = actual.is_on();
    persist_settings(&state, &settings)?;

    state.with(|i| {
        i.push_log("app", "info", format!("开机自启动：{}", actual.describe()));
    });
    snapshot::build_snapshot(&app, &state).await
}

/// 打开系统设置的登录项页面（`RequiresApproval` 时用）。
#[tauri::command]
pub async fn open_login_item_settings() -> Result<(), String> {
    crate::login_item::open_system_settings()
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
        core::stop_core(&app, &state).await?;
    }
    if mode != ProxyMode::Direct {
        core::start_core(&app, &state).await?;
    } else {
        state.with(|i| {
            i.runtime = CoreRuntime::default();
            i.push_log("app", "info", "已切换到直连模式");
        });
    }
    snapshot::build_snapshot(&app, &state).await
}
