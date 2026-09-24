//! 客户端版本**自动检测**（task-188）。
//!
//! 需求：「增加自动检测最新的版本」。此前只有设置页的手动按钮会调
//! `xt_core::update::check_app`（`commands/snapshot.rs::check_app_update`），
//! 用户不点开设置就永远不知道有新版本。
//!
//! 本模块只做**检测与如实呈现**：绝不下载、绝不安装 ——
//! 安装仍然是用户点「更新」那一步（`install_app_update`，本卡不动它）。
//!
//! # 为什么逻辑长这样
//!
//! * 判据与落盘全部是**纯函数**（[`should_check`] / [`proxy_for_check`] /
//!   [`apply_app_check_result`] / [`auto_check_once`]），网络调用由调用方**注入** ⇒
//!   单测不碰真网络（本仓既有口径：联网的东西不进单测）。真网络只有一层：
//!   [`check_now`] 里的 `spawn_blocking`。
//! * **失败绝不说「已是最新」**：`check_error` 只说「没查到」+ 原始错误
//!   （GitHub 403 是**限流**，`xt_core::update::gh_status_error` 已经把它翻成人话）。
//! * 文案里刻意不出现「已是最新」这四个字（`tests::no_path_ever_says_up_to_date`
//!   把这条钉住）—— 「没查到」与「没有新版」是两件事，混了就是撒谎。

use std::time::Duration;

use tauri::Manager;

use crate::state::{AppState, UpdateStatus};
use xt_core::update::Available;

/// 首次自动检查前的延迟。
///
/// 启动窗口里已经有四件事（找核心 / 探 helper / 回滚遗留 / 探 DNS，见 `lib.rs` 的
/// `bootstrap`），版本检查**排在它们之后、而且自己再等一会儿** ——
/// 它最不急（用户又不等着看版本号），没道理去抢启动窗口与网络。
pub(crate) const INITIAL_DELAY: Duration = Duration::from_secs(20);

/// 复查周期：6 小时。
///
/// 上限其实由 GitHub 的**匿名配额**决定：60 次/小时且**按 IP** 算，而我们的请求
/// 多经节点出去（等于和整台节点的用户共用那份配额，见 `xt_core::update::check_app`
/// 的注释）。6 小时 = 4 次/天，足够及时，也不去惹限流。
pub(crate) const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// 一次自动检查要不要落日志、落哪一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogLine {
    /// 与上一次**一模一样**的失败 ⇒ 不再刷第二条（见 [`apply_app_check_result`]）。
    Silent,
    /// 要落的一行：`(level, message)`。
    Line(&'static str, String),
}

/// 到点了吗？`last` = 上次检查时刻（`UpdateStatus::checked_at`）。
///
/// 没查过 ⇒ 查；距上次不足一个周期 ⇒ 不查；够一个周期 ⇒ 查。
///
/// 这是**第二道**守卫：ticker 到点时也可能不该查（例如用户刚在设置页点过手动检查，
/// `checked_at` 是新的）—— 那就别白花一次配额。
pub(crate) fn should_check(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        // `saturating_sub`：时钟被往回调时不 panic，退化成「还不到点」。
        Some(t) => now.saturating_sub(t) >= INTERVAL.as_secs(),
    }
}

/// 自动检查用的代理参数。
///
/// 隧道在跑 ⇒ 走节点（与手动检查一致）；**没在跑 ⇒ `None`，直连**。
///
/// 这一支是必须的：否则「系统代理 / 直连」模式下永远检测不到新版 ——
/// 那等于这个功能没做（用户抱怨的正是「从来不知道有新版本」）。
/// 反过来，直连也只**读**一个公开仓库的 release 列表，不碰任何用户数据。
pub(crate) fn proxy_for_check(running: bool, socks_port: u16) -> Option<u16> {
    running.then_some(socks_port)
}

/// 把一次检查的结果落到 [`UpdateStatus`]，并决定要不要落日志（**纯函数**）。
///
/// **核心 / geo** 检查的落盘（纯函数，便于断言「不污染客户端专属字段」）。
///
/// 与客户端路径 [`apply_app_check_result`] **分开**：本函数只写
/// `checked_at` / `check_error` / `latest_core` / `latest_geo`，
/// **绝不碰** `check_error_app` / `checked_at_app`（task-191：合并字段与客户端字段是两回事）。
/// 返回需要落盘的那条 warn 文案（没有失败就是 `None`）。
pub(crate) fn apply_core_geo_check_result(
    update: &mut UpdateStatus,
    core: Result<Available, String>,
    geo: Result<Available, String>,
    now: u64,
) -> Option<String> {
    update.checked_at = Some(now);
    let mut warn = None;
    match core {
        Ok(a) => {
            update.latest_core = Some(a);
            update.check_error = None;
        }
        Err(e) => {
            warn = Some(format!("检查核心更新失败：{e}"));
            update.check_error = Some(e);
        }
    }
    if let Ok(a) = geo {
        update.latest_geo = Some(a);
    }
    warn
}

/// 三条硬规矩：
/// 1. **失败绝不清空 `latest_app`** —— 那是**上次**查到的版本，抹掉它会让界面上的
///    更新按钮凭空消失（用户会因为一次网络抖动丢掉已知的新版本）；
/// 2. **失败绝不说「已是最新」** —— 只写 `check_error`，文案里只有原始错误；
/// 3. **同样的失败重复出现不刷屏**：只有 `check_error` **变化**时才落一条
///    （第一次失败 / 失败原因变了 / 恢复）。6 小时一轮，网络一直不好时
///    一天就该只有一条，而不是四条。
pub(crate) fn apply_app_check_result(
    update: &mut UpdateStatus,
    result: Result<Available, String>,
    now: u64,
) -> LogLine {
    update.checked_at = Some(now);
    // 失败也是一次「客户端检查」⇒ 时刻照样记（与 `checked_at` 对齐；`check_error_app` 只在失败分支写）
    update.checked_at_app = Some(now);
    match result {
        Ok(a) => {
            // 「恢复」= 上一次是失败的。取走它，顺便把 `check_error` 清空。
            let recovered = update.check_error.take().is_some();
            update.latest_app = Some(a.clone());
            // task-191：客户端专属字段 —— 成功 ⇒ 清错误 + 记时刻（`check_error`/`checked_at` 照旧写）
            update.check_error_app = None;
            let msg = if recovered {
                format!("更新检查已恢复：客户端最新版 {}", a.version)
            } else {
                // 与手动检查同一条文案（一处实现，两处共用）。
                format!("客户端最新版 {}", a.version)
            };
            LogLine::Line("info", msg)
        }
        Err(e) => {
            let same = update.check_error.as_deref() == Some(e.as_str());
            // 先把文案拼好再移动 `e`，否则 `check_error` 拿走后就用不了它了。
            let line = format!("检查客户端更新失败：{e}");
            update.check_error_app = Some(e.clone());
            update.check_error = Some(e);
            if same {
                LogLine::Silent
            } else {
                LogLine::Line("warn", line)
            }
        }
    }
}

/// 「这一轮要不要查、用什么代理」—— `None` = 周期守卫拦下（**一次请求都不发**）。
///
/// 生产路径（[`check_once`]）与测试路径（[`auto_check_once`]）都走这一个决策函数：
/// 守卫与实际发出的请求必须是同一个判据，否则「测过的」和「跑起来的」是两回事。
pub(crate) fn plan_check(
    last: Option<u64>,
    running: bool,
    socks_port: u16,
    now: u64,
) -> Option<Option<u16>> {
    should_check(last, now).then(|| proxy_for_check(running, socks_port))
}

/// 跑一轮自动检查（**网络调用注入** ⇒ 单测不碰真网络）。
///
/// `#[cfg(test)]`：生产路径不能注入 fetch —— 它必须走 [`check_now`] 的
/// `spawn_blocking`（阻塞客户端）。这个函数存在的唯一理由是**把决策链测出来**。
/// 先例：`xt_core::store::with_log_limits` 也是 `#[cfg(test)]`。
#[cfg(test)]
pub(crate) fn auto_check_once(
    update: &mut UpdateStatus,
    running: bool,
    socks_port: u16,
    now: u64,
    fetch: impl FnOnce(Option<u16>) -> Result<Available, String>,
) -> Option<LogLine> {
    let proxy = plan_check(update.checked_at, running, socks_port, now)?;
    Some(apply_app_check_result(update, fetch(proxy), now))
}

/// 真网络那一层：`check_app` 是阻塞的（reqwest 阻塞客户端）⇒ 必须 `spawn_blocking`，
/// 不然会把 async 运行时的一个 worker 卡住好几秒。
pub(crate) async fn check_now(proxy: Option<u16>) -> Result<Available, String> {
    tauri::async_runtime::spawn_blocking(move || {
        xt_core::update::check_app(proxy).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("检查任务失败：{e}"))?
}

/// 做一次自动检查：写状态、落日志（若该落）、通知界面。
///
/// 周期守卫不通过时**什么都不做**（连事件都不发 —— 状态没变化，没有可播报的）。
pub(crate) async fn check_once(app: &tauri::AppHandle, state: &AppState) {
    let Some((running, socks_port, last)) =
        state.with(|i| (i.runtime.running, i.settings.socks_port, i.update.checked_at))
    else {
        return;
    };
    let now = crate::state::now_unix();
    let Some(proxy) = plan_check(last, running, socks_port, now) else {
        return;
    };

    let result = check_now(proxy).await;
    let line = state.with(|i| apply_app_check_result(&mut i.update, result, now));
    if let Some(LogLine::Line(level, msg)) = line {
        state.with(|i| i.push_log("app", level, msg));
    }
    // `Silent`（同样的失败）不落日志，但状态确实变了 ⇒ 仍要通知界面。
    // 不通知的话，20 秒后查到的结果在下次拉快照前都不会出现在界面上。
    crate::events::app_update_checked(app, state);
}

/// 后台起一个**不阻塞启动**的版本检查循环：延迟一次 → 首次检查 → 每 6 小时复查。
///
/// `lib.rs` 只在 `bootstrap` 之后调这一个函数（启动流程里唯一的接线点）。
pub fn watch(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // 启动延迟：见 [`INITIAL_DELAY`]。
        tokio::time::sleep(INITIAL_DELAY).await;
        if let Some(state) = app.try_state::<AppState>() {
            check_once(&app, &state).await;
        }

        let mut ticker = tokio::time::interval(INTERVAL);
        // `interval` 的**第一跳立刻发生** —— 跳过它，否则刚查完又立刻查一次。
        // （这与 `lib.rs` 里意图判定节拍踩的是同一个坑，那里有同样的注释。）
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Some(state) = app.try_state::<AppState>() else {
                continue;
            };
            check_once(&app, &state).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::update::Available;

    fn an_available(version: &str) -> Available {
        Available {
            version: version.to_string(),
            published_at: "2026-01-01T00:00:00Z".into(),
            prerelease: false,
            download_url: "https://example.invalid/x.zip".into(),
            digest_url: None,
            size: Some(1),
        }
    }

    /// 周期常量本身也要被钉住：卡片要求「可以读它」，改动必须是有意的。
    #[test]
    fn the_schedule_constants_are_the_documented_ones() {
        assert!(
            INITIAL_DELAY.as_secs() >= 15 && INITIAL_DELAY.as_secs() <= 30,
            "启动延迟要在 15–30 秒之间（别去抢启动窗口）：{}s",
            INITIAL_DELAY.as_secs()
        );
        assert_eq!(INTERVAL, Duration::from_secs(6 * 60 * 60), "复查周期是 6 小时");
    }

    /// `should_check` 三态：没查过 ⇒ 查；刚查过 ⇒ 不查；超周期 ⇒ 查。
    #[test]
    fn should_check_is_false_only_inside_the_interval() {
        let now = 1_800_000_000u64;
        assert!(should_check(None, now), "从没查过 ⇒ 必须查");

        let just_checked = Some(now - 60);
        assert!(!should_check(just_checked, now), "刚查过 1 分钟 ⇒ 别白花配额");

        let exactly_a_period_ago = Some(now - INTERVAL.as_secs());
        assert!(should_check(exactly_a_period_ago, now), "正好一个周期 ⇒ 该查");

        let long_ago = Some(now - INTERVAL.as_secs() * 3);
        assert!(should_check(long_ago, now), "超过一个周期 ⇒ 该查");

        // 时钟往回调不能 panic、也不能变成「该查」。
        assert!(!should_check(Some(now + 3600), now), "时钟回拨 ⇒ 保守地不查");
    }

    /// **失败绝不撒谎**：`check_error` 有值、`latest_app` 不变、文案里没有「已是最新」。
    #[test]
    fn failure_keeps_the_previous_latest_and_never_says_up_to_date() {
        let mut u = UpdateStatus {
            latest_app: Some(an_available("0.9.0")),
            checked_at: Some(1),
            ..Default::default()
        };
        let now = 2_000_000_000u64;

        let line = apply_app_check_result(&mut u, Err("GitHub 403（限流）".into()), now);

        let msg = assert_log(&line, "warn");
        assert!(msg.contains("403"), "要把原始错误带上：{msg}");
        assert!(!msg.contains("已是最新"), "没查到 ≠ 没有新版：{msg}");
        assert_eq!(u.checked_at, Some(now), "失败也要记「查过了」");
        assert_eq!(
            u.check_error.as_deref(),
            Some("GitHub 403（限流）"),
            "失败原因要留着给界面显示"
        );
        assert_eq!(
            u.latest_app.as_ref().map(|a| a.version.as_str()),
            Some("0.9.0"),
            "失败不许清掉上次查到的版本（否则更新按钮凭空消失）"
        );
    }

    /// 重复同样的失败**只记一条**；失败原因变了就再记一条。
    #[test]
    fn repeated_identical_failure_logs_only_once() {
        let mut u = UpdateStatus::default();
        let now = 2_000_000_000u64;

        let first = apply_app_check_result(&mut u, Err("timeout".into()), now);
        assert_log(&first, "warn");

        let second = apply_app_check_result(&mut u, Err("timeout".into()), now + 3600);
        assert_eq!(second, LogLine::Silent, "同样的失败不许刷屏");
        let third = apply_app_check_result(&mut u, Err("timeout".into()), now + 7200);
        assert_eq!(third, LogLine::Silent, "第三次也一样");

        let changed = apply_app_check_result(&mut u, Err("GitHub 403（限流）".into()), now + 10800);
        let msg = assert_log(&changed, "warn");
        assert!(msg.contains("403"), "原因变了要再报一条：{msg}");
    }

    /// 成功 ⇒ 更新版本、清错误、日志含版本号；失败之后成功 ⇒ 有一条**恢复**日志。
    #[test]
    fn success_updates_and_a_recovery_is_announced() {
        let mut u = UpdateStatus::default();
        let now = 2_000_000_000u64;

        let first = apply_app_check_result(&mut u, Ok(an_available("0.9.0")), now);
        let msg = assert_log(&first, "info");
        assert!(msg.contains("0.9.0"), "日志要含版本号：{msg}");
        assert_eq!(u.latest_app.as_ref().map(|a| a.version.as_str()), Some("0.9.0"));
        assert!(u.check_error.is_none());

        // 失败一次，再成功 ⇒ 恢复。
        apply_app_check_result(&mut u, Err("timeout".into()), now + 10);
        assert!(u.check_error.is_some());
        assert_eq!(
            u.latest_app.as_ref().map(|a| a.version.as_str()),
            Some("0.9.0"),
            "失败期间旧版本仍在"
        );

        let recovered =
            apply_app_check_result(&mut u, Ok(an_available("0.9.1")), now + 20);
        let msg = assert_log(&recovered, "info");
        assert!(msg.contains("恢复"), "恢复要说出来：{msg}");
        assert!(msg.contains("0.9.1"), "{msg}");
        assert!(u.check_error.is_none(), "成功后错误必须清掉");
    }

    /// 周期守卫：不到点 ⇒ 跳过（`None`），而且**一次请求都不发**。
    #[test]
    fn auto_check_does_not_hit_the_network_inside_the_interval() {
        let mut u = UpdateStatus {
            checked_at: Some(1_800_000_000),
            ..Default::default()
        };
        let called = std::cell::Cell::new(false);
        let line = auto_check_once(
            &mut u,
            false,
            7890,
            1_800_000_060, // 才过 60 秒
            |_| {
                called.set(true);
                Ok(an_available("0.9.9"))
            },
        );

        assert!(line.is_none(), "不到周期就该跳过");
        assert!(!called.get(), "跳过时不许发请求");
        assert!(u.latest_app.is_none(), "跳过时状态不该被改");
    }

    /// **没连隧道也要能查**（否则系统代理 / 直连模式下等于没做）。
    #[test]
    fn auto_check_still_queries_directly_when_the_tunnel_is_not_running() {
        let mut u = UpdateStatus::default();
        let seen = std::cell::Cell::new(None);
        let now = 2_000_000_000u64;

        let line = auto_check_once(&mut u, false, 7890, now, |proxy| {
            seen.set(Some(proxy));
            Ok(an_available("0.9.0"))
        });

        assert_log(&line.expect("没连隧道也必须去查"), "info");
        assert_eq!(
            seen.get(),
            Some(None),
            "没在跑 ⇒ 必须以 `None`（直连）去查，而不是跳过或走一个不存在的代理"
        );
        assert_eq!(u.latest_app.as_ref().map(|a| a.version.as_str()), Some("0.9.0"));
    }

    /// 隧道在跑 ⇒ 走节点（用配置里的 SOCKS 端口）。
    #[test]
    fn auto_check_uses_the_node_when_the_tunnel_is_running() {
        let mut u = UpdateStatus::default();
        let seen = std::cell::Cell::new(None);

        let line = auto_check_once(&mut u, true, 7890, 2_000_000_000, |proxy| {
            seen.set(Some(proxy));
            Ok(an_available("0.9.0"))
        });

        assert_log(&line.expect("在跑时当然要查"), "info");
        assert_eq!(seen.get(), Some(Some(7890)), "在跑 ⇒ 经节点出去");
    }

    /// 把「不许说已是最新」钉在**所有文案**上：任何分支都不得出现那四个字。
    #[test]
    fn no_path_ever_says_up_to_date() {
        let now = 2_000_000_000u64;
        let mut u = UpdateStatus {
            latest_app: Some(an_available("0.9.0")),
            ..Default::default()
        };
        let mut lines = vec![
            Some(apply_app_check_result(&mut u, Err("GitHub 403（限流）".into()), now)),
            Some(apply_app_check_result(&mut u, Err("GitHub 404".into()), now + 1)),
            Some(apply_app_check_result(
                &mut u,
                Ok(an_available("0.8.38")),
                now + 2,
            )),
        ];
        // 也把自动路径拉进来：这里该**跳过**（`checked_at` 刚被上面改成 now+2），
        // 但一旦守卫被改坏，它返回的那条失败日志也要过同一句检查。
        lines.push(auto_check_once(&mut u, false, 7890, now + 3, |_| {
            Err("network unreachable".into())
        }));
        for line in lines.into_iter().flatten() {
            if let LogLine::Line(_, msg) = line {
                assert!(
                    !msg.contains("已是最新"),
                    "「没查到」不许写成「已是最新」：{msg}"
                );
            }
        }
    }

    /// 小工具：断言这一行是日志且级别正确，返回文案。
    fn assert_log(line: &LogLine, level: &str) -> String {
        match line {
            LogLine::Line(got, msg) => {
                assert_eq!(*got, level, "级别不对：{msg}");
                msg.clone()
            }
            other => panic!("期望一条日志，实际是 {other:?}"),
        }
    }
}


#[cfg(test)]
mod tester_task191 {
    use super::*;

    fn avail(v: &str) -> Available {
        Available {
            version: v.to_string(),
            published_at: "2026-09-24T00:00:00Z".to_string(),
            prerelease: false,
            download_url: "https://example.invalid/a.zip".to_string(),
            digest_url: None,
            size: Some(1),
        }
    }

    /// 核心/geo 失败 ⇒ 合并字段写、**客户端专属字段一个都不许被污染**。
    #[test]
    fn core_geo_failure_does_not_pollute_app_specific_fields() {
        let mut u = UpdateStatus {
            check_error_app: Some("上一次客户端失败".into()),
            checked_at_app: Some(111),
            ..UpdateStatus::default()
        };
        let warn = apply_core_geo_check_result(
            &mut u, Err("core net down".into()), Err("geo net down".into()), 999);
        assert_eq!(warn.as_deref(), Some("检查核心更新失败：core net down"));
        assert_eq!(u.check_error.as_deref(), Some("core net down"), "合并字段照旧写");
        assert_eq!(u.checked_at, Some(999), "合并时刻照旧写");
        assert_eq!(u.check_error_app.as_deref(), Some("上一次客户端失败"), "客户端错误不许被核心/geo 覆盖");
        assert_eq!(u.checked_at_app, Some(111), "客户端时刻不许被核心/geo 覆盖");
    }

    /// 客户端失败 ⇒ 写 `check_error_app`，且**不清 `latest_app`**（task-188 口径）。
    #[test]
    fn app_failure_sets_app_error_and_keeps_latest() {
        let mut u = UpdateStatus { latest_app: Some(avail("0.9.0")), ..UpdateStatus::default() };
        apply_app_check_result(&mut u, Err("net down".into()), 500);
        assert_eq!(u.check_error_app.as_deref(), Some("net down"));
        assert_eq!(u.latest_app.as_ref().map(|a| a.version.as_str()), Some("0.9.0"), "失败绝不清 latest_app");
        assert_eq!(u.checked_at_app, Some(500), "失败也记一次检查时刻");
    }

    /// 客户端成功 ⇒ 清 `check_error_app` + 更新 `checked_at_app`。
    #[test]
    fn app_success_clears_app_error_and_stamps_time() {
        let mut u = UpdateStatus {
            check_error_app: Some("旧错误".into()),
            ..UpdateStatus::default()
        };
        apply_app_check_result(&mut u, Ok(avail("0.9.1")), 777);
        assert_eq!(u.check_error_app, None, "成功必须清掉客户端错误");
        assert_eq!(u.checked_at_app, Some(777));
        assert_eq!(u.latest_app.as_ref().map(|a| a.version.as_str()), Some("0.9.1"));
    }
}
