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

#[derive(Clone, Serialize)]
pub struct LogPayload {
    pub line: String,
    pub level: String,
}

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

pub fn log_line(app: &AppHandle, entry: LogEntry) {
    emit(
        app,
        CORE_LOG,
        LogPayload { line: entry.message, level: entry.level },
    );
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
