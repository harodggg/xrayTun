//! 审计自动同步的 UI 入口。
//!
//! # 这一层只做三件事
//!
//! 1. 拿**审计同步自己的那把锁**（不是 `inner` —— 见下）；
//! 2. 调用 `crate::audit_sync` 里对应的方法；
//! 3. 把结果翻译成一句人话，并在 App 日志里留一行。
//!
//! **不在这里写任何加密、组包或调度逻辑** —— 那些在 `xt-intent` 与
//! `crate::audit_sync` 里，能单测、能离线跑。
//!
//! # 为什么单独一把锁
//!
//! `state.inner` 那把锁的约定是「只覆盖纯内存操作」（`state.rs` 开头写明）。
//! 而这里的动作会**读文件、出成百 KB 的 JSON、发 HTTPS** —— 塞进 `inner`
//! 会把快照、托盘、日志读取全部堵在后面。所以审计同步有自己的
//! `tokio::sync::Mutex`（与 `helper` 同一个理由：不让一条慢路径拖住所有读者）。
//!
//! # 错误消息的规矩
//!
//! `Result<_, String>` 里的字符串是**给用户看的**：要说清"哪里、为什么、怎么办"。
//! 传输出错不许写成"失败了" —— 那等于把用户丢在黑暗里。

use tauri::State;

use crate::audit_sync::{AuditSyncPreview, AuditSyncRevoke, AuditSyncRun, AuditSyncStatus};
use crate::state::AppState;

/// 当前状态（关着也返回一份完整状态，界面不需要先判断）。
#[tauri::command]
pub async fn audit_sync_status(state: State<'_, AppState>) -> Result<AuditSyncStatus, String> {
    let rt = state.audit_sync.lock().await;
    Ok(rt.status())
}

/// 开关。**开启时会先生成设备 id 与密钥**（否则"开着但没有密钥"是一个永远失败的状态）。
#[tauri::command]
pub async fn audit_sync_set_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<AuditSyncStatus, String> {
    let (status, note) = {
        let mut rt = state.audit_sync.lock().await;
        let status = rt.set_enabled(enabled)?;
        let note = if enabled {
            if status.key_present {
                format!(
                    "审计同步已开启（设备 {}）；每天上传一次，内容为端到端加密密文",
                    status.device
                )
            } else {
                "审计同步已开启".to_string()
            }
        } else {
            "审计同步已关闭（已上传的历史不会被删除；要删请用「撤回」）".to_string()
        };
        (status, note)
    };
    state.log("audit-sync", "info", note);
    Ok(status)
}

/// 设置端点（空串 = 恢复默认）。非 `https://` 一律拒绝。
#[tauri::command]
pub async fn audit_sync_set_base_url(
    state: State<'_, AppState>,
    base_url: String,
) -> Result<AuditSyncStatus, String> {
    let mut rt = state.audit_sync.lock().await;
    let status = rt.set_base_url(&base_url)?;
    let applied = status.base_url.clone();
    drop(rt);
    state.log("audit-sync", "info", format!("审计同步端点已设为 {applied}"));
    Ok(status)
}

/// 设置上传 token（空串 = 清除）。token 不回显。
#[tauri::command]
pub async fn audit_sync_set_token(
    state: State<'_, AppState>,
    token: String,
) -> Result<AuditSyncStatus, String> {
    let (status, cleared) = {
        let mut rt = state.audit_sync.lock().await;
        let clearing = token.trim().is_empty();
        let status = rt.set_token(&token)?;
        (status, clearing)
    };
    state.log(
        "audit-sync",
        "info",
        if cleared {
            "审计同步的上传 token 已清除".to_string()
        } else {
            "审计同步的上传 token 已保存（不回显）".to_string()
        },
    );
    Ok(status)
}

/// 立刻同步一次（等价于定时任务做的那一轮）。
#[tauri::command]
pub async fn audit_sync_now(state: State<'_, AppState>) -> Result<AuditSyncRun, String> {
    let (run, note, level) = {
        let mut rt = state.audit_sync.lock().await;
        if !rt.is_enabled() {
            return Err("审计同步没开启：先在设置里打开开关，并填上传 token".into());
        }
        let run = rt.sync_now();
        let (note, level) = match &run.error {
            Some(e) => (format!("审计同步失败：{e}"), "warn"),
            None if run.uploaded.is_empty() => (
                // **不把"没事可做"说成"成功上传"**：没有已结束的新天就是没有。
                format!(
                    "审计同步：没有需要上传的天（已传到 {}）",
                    run.status
                        .last_uploaded_day
                        .clone()
                        .unwrap_or_else(|| "（还没有成功过）".to_string())
                ),
                "info",
            ),
            None => (
                format!("审计同步：上传 {} 天（{}）", run.uploaded.len(), run.uploaded.join(", ")),
                "info",
            ),
        };
        (run, note, level)
    };
    state.log("audit-sync", level, note);
    Ok(run)
}

/// 「将要上传的内容」：明文 JSON，**本机数据，不联网**。
#[tauri::command]
pub async fn audit_sync_preview(
    state: State<'_, AppState>,
    day: Option<String>,
) -> Result<AuditSyncPreview, String> {
    let rt = state.audit_sync.lock().await;
    Ok(rt.preview(day))
}

/// 撤回：让服务端删掉本设备全部已上传对象。返回**服务端确认**删掉的个数。
#[tauri::command]
pub async fn audit_sync_revoke(state: State<'_, AppState>) -> Result<AuditSyncRevoke, String> {
    let (result, note, level) = {
        let mut rt = state.audit_sync.lock().await;
        let result = rt.revoke();
        let (note, level) = match &result.error {
            Some(e) => (format!("撤回失败：{e}"), "warn"),
            None => (
                format!("已撤回服务端上的 {} 个对象", result.deleted),
                "info",
            ),
        };
        (result, note, level)
    };
    state.log("audit-sync", level, note);
    Ok(result)
}
