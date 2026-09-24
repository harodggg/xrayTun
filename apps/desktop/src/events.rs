//! 后端 → 前端的事件。
//!
//! 事件名用 `域://动作` 的形式，前端 `src/ipc.ts` 里有对应的常量，两边必须一致。
//! 之所以不用「随便一个字符串」，是因为事件名拼错在 Tauri 里**不会报错** ——
//! 只是静静地收不到，排查起来很浪费时间。所以两边都集中定义。

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, CoreRuntime, LogEntry, TrafficSample};

pub const RUNTIME_CHANGED: &str = "runtime://changed";
pub const CORE_LOG: &str = "core://log";
pub const LATENCY_UPDATED: &str = "nodes://latency";
pub const PROBE_STARTED: &str = "nodes://probe-started";
pub const NODES_CHANGED: &str = "nodes://changed";
pub const SUBSCRIPTIONS_CHANGED: &str = "subscriptions://changed";
pub const SETTINGS_CHANGED: &str = "settings://changed";
pub const UPDATE_PROGRESS: &str = "update://progress";
/// 自动版本检测完成了一次（task-188）。
///
/// 新增事件而不是复用 `nodes://changed` 之类：那些会让前端**拉整份快照**
/// （前端只在 nodes/subscriptions/settings 变化时这样做），而版本检测每 6 小时
/// 才发生一次、只改 `update` 里三个字段 —— 用不着拉快照。
/// 载荷就是那三个字段本身（`app_update_available` 由快照按当前版本号算，
/// 见 `commands/snapshot.rs::update_status_with`，不在事件里重复）。
pub const APP_UPDATE_CHECKED: &str = "app://update-checked";

#[derive(Clone, Serialize)]
pub struct LogPayload {
    pub line: String,
    pub level: String,
}

/// `runtime://changed` 的载荷。
///
/// 这里**没有**单独的 `recovery` 字段：自动恢复状态就在
/// [`CoreRuntime::recovery`]（`apps/desktop/src/state.rs`）里，而 `runtime`
/// 是整体下发的 —— 这样事件与快照（`snapshot.runtime`）两条路上的恢复状态
/// 是同一份，刷新快照时不会丢。前端读 `payload.runtime.recovery`。
#[derive(Clone, Serialize)]
pub struct RuntimePayload {
    pub runtime: CoreRuntime,
    pub traffic: TrafficSample,
}

#[derive(Clone, Serialize)]
pub struct ProbeStartedPayload {
    pub total: usize,
}

fn emit<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    // 发事件失败只可能是窗口已关闭，此时静默即可。
    if let Err(e) = app.emit(event, payload) {
        tracing::debug!(event, error = %e, "事件发送失败（窗口可能已关闭）");
    }
}

pub fn runtime_changed(app: &AppHandle, state: &AppState) {
    let payload = state
        .with(|i| RuntimePayload { runtime: i.runtime.clone(), traffic: i.traffic.clone() })
        .unwrap_or(RuntimePayload {
            runtime: CoreRuntime::default(),
            traffic: TrafficSample::default(),
        });
    emit(app, RUNTIME_CHANGED, payload);
}

/// 更新下载进度（核心 / geo / 客户端共用）。
///
/// 单独一个事件而不是塞进 `runtime_changed`：下载期间每 200ms 就报一次，
/// 而快照组装要读文件、问核心版本 —— 用快照推会把一件小事变得很贵。
pub fn update_progress(app: &AppHandle, label: &str, done: u64, total: Option<u64>) {
    #[derive(Clone, Serialize)]
    struct P<'a> {
        label: &'a str,
        done_bytes: u64,
        total_bytes: Option<u64>,
    }
    emit(app, UPDATE_PROGRESS, P { label, done_bytes: done, total_bytes: total });
}

pub fn log_line(app: &AppHandle, entry: LogEntry) {
    emit(
        app,
        CORE_LOG,
        LogPayload { line: entry.message, level: entry.level },
    );
}

/// 自动版本检测完成了一次：播报 `update` 里那三个字段的现状。
///
/// **成功与失败都要发**（失败也要让界面把 `check_error` 显示出来），
/// 但周期守卫拦下的那一轮不发 —— 状态没变，没有可播报的。
pub fn app_update_checked(app: &AppHandle, state: &AppState) {
    #[derive(Clone, Serialize)]
    struct P {
        latest_app: Option<xt_core::update::Available>,
        checked_at: Option<u64>,
        check_error: Option<String>,
    }
    let payload = state.with(|i| P {
        latest_app: i.update.latest_app.clone(),
        checked_at: i.update.checked_at,
        check_error: i.update.check_error.clone(),
    });
    if let Some(payload) = payload {
        emit(app, APP_UPDATE_CHECKED, payload);
    }
}

pub fn latency_updated(app: &AppHandle, results: &[xt_core::xray::ProbeResult]) {
    emit(app, LATENCY_UPDATED, results.to_vec());
}

pub fn probe_started(app: &AppHandle, total: usize) {
    emit(app, PROBE_STARTED, ProbeStartedPayload { total });
}

pub fn nodes_changed(app: &AppHandle) {
    emit(app, NODES_CHANGED, ());
}

pub fn subscriptions_changed(app: &AppHandle) {
    emit(app, SUBSCRIPTIONS_CHANGED, ());
}

pub fn settings_changed(app: &AppHandle) {
    emit(app, SETTINGS_CHANGED, ());
}

/// 供 `try_state` 拿状态的辅助，避免每个调用点都写一遍类型。
pub fn with_state<R>(app: &AppHandle, f: impl FnOnce(&AppState) -> R) -> Option<R> {
    app.try_state::<AppState>().map(|s| f(&s))
}
