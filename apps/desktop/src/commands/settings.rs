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

    // 意图过滤按新设置重建（换预设/模型/阈值 ⇒ 判决缓存整库作废）。
    // **不重启核心**：本版不下发任何路由规则，所以这里只有内存与磁盘上的判定状态
    // 会变。规则下发那一步才需要决定"重启还是 AddRule 热加"。
    let now = xt_core::util::now_unix();
    let intent_notes = state
        .with(|i| i.intent.follow_settings(&settings, now))
        .unwrap_or_default();
    for note in intent_notes {
        state.log("intent", "info", note);
    }

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

/// 模式切换期间实际做过的动作（**按发生顺序**）。
///
/// 存在的意义是让「未运行时不得停机/启核心」这条断言落在**调用序列**上，
/// 而不是只看返回值 —— 返回值相同但偷偷重启了核心，用户感知就是「卡了 30 秒」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeStep {
    Stop,
    Start,
}

/// 模式切换要执行的动作。
///
/// # 语义（这是本函数存在的全部理由）
///
/// * **核心没在跑 → 什么都不做**：模式只是一个**偏好**。用户点它是为了比较
///   三种模式，不是要求连接；连接是「连接」按钮的唯一职责。
///   以前这里无条件下 `start_core`，于是「改个偏好」被实现成「执行一次完整
///   连接」（最坏 30s+），这就是用户说的「点模式卡顿严重」。
/// * **核心在跑 → 必须停机 + （非直连时）重启**：模式变了，路由/DNS/数据面
///   都得按新模式重建，这段耗时是**必要的**。
///
/// 两个动作作为闭包注入，是为了能注入假实现断言调用序列。
pub(crate) async fn apply_mode_switch<S, SF, T, TF>(
    was_running: bool,
    next: ProxyMode,
    stop: S,
    start: T,
) -> Result<Vec<ModeStep>, String>
where
    S: FnOnce() -> SF,
    SF: std::future::Future<Output = Result<(), String>>,
    T: FnOnce() -> TF,
    TF: std::future::Future<Output = Result<(), String>>,
{
    if !was_running {
        return Ok(Vec::new());
    }
    let mut steps = vec![ModeStep::Stop];
    stop().await?;
    if next != ProxyMode::Direct {
        steps.push(ModeStep::Start);
        start().await?;
    }
    Ok(steps)
}

/// 给用户看的模式名（日志/文案用；`as_str()` 是给协议/配置用的）。
pub(crate) fn mode_label(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Direct => "直连",
        ProxyMode::SystemProxy => "系统代理",
        ProxyMode::Tun => "TUN",
    }
}

/// 切换运行模式。
///
/// **核心没在跑时只改偏好**（见 [`apply_mode_switch`]）；**在跑时会重启核心**，
/// 那几秒是必要的（模式变了，路由与 DNS 都得重建）。界面在等待期间会显示
/// 「正在切换模式…」，见 `apps/ui/src/App.tsx`。
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

    let started_at = std::time::Instant::now();
    let steps = apply_mode_switch(
        was_running,
        mode,
        || core::stop_core(&app, &state),
        || core::start_core(&app, &state, CoreStartTrigger::ModeSwitch),
    )
    .await?;

    // 用户排障要看得到「刚才那一下到底干了什么、花了多久」：
    // 以前这里没有任何计时，也没有这条日志，卡了多久全靠猜。
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    state.with(|i| {
        if mode == ProxyMode::Direct {
            // 直连：核心（如果刚才在跑）已经停掉，运行态归零。
            i.runtime = CoreRuntime::default();
        }
        let what = if steps.is_empty() {
            // **未运行时点模式**：只改了偏好。这里必须说清下一步点哪儿，
            // 否则「以前靠点模式来连接」的用户会以为坏了。
            format!(
                "已切换为「{}」模式（当前未连接，点「连接」开始）；耗时 {} ms",
                mode_label(mode),
                elapsed_ms
            )
        } else {
            format!(
                "已切换为「{}」模式（{}）；耗时 {} ms",
                mode_label(mode),
                if steps.contains(&ModeStep::Start) { "已重启核心" } else { "已停止核心" },
                elapsed_ms
            )
        };
        i.push_log("app", "info", what);
    });
    tracing::info!(
        mode = mode.as_str(),
        was_running,
        steps = ?steps,
        elapsed_ms,
        "模式切换完成（逐阶段耗时见各阶段的 tracing 日志）"
    );
    snapshot::build_snapshot(&app, &state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 记录模式切换期间对核心的调用（顺序敏感）。
    #[derive(Default)]
    struct Calls(RefCell<Vec<&'static str>>);

    impl Calls {
        fn push(&self, step: &'static str) {
            self.0.borrow_mut().push(step);
        }
        fn seq(&self) -> Vec<&'static str> {
            self.0.borrow().clone()
        }
    }

    /// **本卡的核心断言**：核心**没在跑**时，改模式**不得**碰核心。
    ///
    /// 以前 `set_mode` 是无条件 `if mode != Direct { start_core }` ——
    /// 用户只是想比较三种模式，却触发了一次完整连接（停机预算 3s + 等端口 10s
    /// + 端到端门禁 12s + TCP 检查 8s），这就是「点模式卡顿严重」。
    #[tokio::test]
    async fn mode_switch_when_idle_never_touches_the_core() {
        let calls = Calls::default();
        let steps = apply_mode_switch(
            false,
            ProxyMode::Tun,
            || {
                calls.push("stop");
                async { Ok::<(), String>(()) }
            },
            || {
                calls.push("start");
                async { Ok::<(), String>(()) }
            },
        )
        .await
        .expect("未运行时切换模式不该失败");

        assert!(steps.is_empty(), "未运行时应零动作，实际 {steps:?}");
        assert!(
            calls.seq().is_empty(),
            "未运行时**不得**调用 stop_core/start_core，实际 {:?}",
            calls.seq()
        );
    }

    /// 未运行 → 直连 同样零动作（只是把偏好改成直连）。
    #[tokio::test]
    async fn mode_switch_when_idle_to_direct_is_also_inert() {
        let calls = Calls::default();
        let steps = apply_mode_switch(
            false,
            ProxyMode::Direct,
            || {
                calls.push("stop");
                async { Ok::<(), String>(()) }
            },
            || {
                calls.push("start");
                async { Ok::<(), String>(()) }
            },
        )
        .await
        .unwrap();
        assert!(steps.is_empty());
        assert!(calls.seq().is_empty());
    }

    /// **反例（必须）**：核心**在跑**时切换 → 必须停机 + 重启（这段耗时是必要的）。
    #[tokio::test]
    async fn mode_switch_while_running_restarts_the_core() {
        let calls = Calls::default();
        let steps = apply_mode_switch(
            true,
            ProxyMode::SystemProxy,
            || {
                calls.push("stop");
                async { Ok::<(), String>(()) }
            },
            || {
                calls.push("start");
                async { Ok::<(), String>(()) }
            },
        )
        .await
        .unwrap();

        assert_eq!(steps, vec![ModeStep::Stop, ModeStep::Start]);
        assert_eq!(calls.seq(), ["stop", "start"], "在跑时必须停机 + 重启");
    }

    /// 在跑 → 直连：只停机，不重启。
    #[tokio::test]
    async fn mode_switch_while_running_to_direct_only_stops() {
        let calls = Calls::default();
        let steps = apply_mode_switch(
            true,
            ProxyMode::Direct,
            || {
                calls.push("stop");
                async { Ok::<(), String>(()) }
            },
            || {
                calls.push("start");
                async { Ok::<(), String>(()) }
            },
        )
        .await
        .unwrap();

        assert_eq!(steps, vec![ModeStep::Stop]);
        assert_eq!(calls.seq(), ["stop"], "直连不需要再起核心");
    }

    /// 停机失败 → 直接把错误抛给调用方，**不得**继续启动。
    #[tokio::test]
    async fn mode_switch_does_not_start_when_stop_failed() {
        let calls = Calls::default();
        let res = apply_mode_switch(
            true,
            ProxyMode::Tun,
            || {
                calls.push("stop");
                async { Err::<(), String>("停止核心失败：boom".into()) }
                },
            || {
                calls.push("start");
                async { Ok::<(), String>(()) }
            },
        )
        .await;

        assert_eq!(res, Err("停止核心失败：boom".to_string()));
        assert_eq!(calls.seq(), ["stop"], "停机失败后不该再启动");
    }

    /// 用户看得懂的模式名（日志文案用）。
    #[test]
    fn mode_labels_are_human_readable() {
        assert_eq!(mode_label(ProxyMode::Direct), "直连");
        assert_eq!(mode_label(ProxyMode::SystemProxy), "系统代理");
        assert_eq!(mode_label(ProxyMode::Tun), "TUN");
    }
}
