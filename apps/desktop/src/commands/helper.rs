//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

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
    snapshot::build_snapshot(&app, &state).await
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
    snapshot::build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn uninstall_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::uninstall_script();
    crate::helper_install::run_with_admin(&script, "卸载 XrayTun 网络配置助手")?;
    state.with(|i| i.push_log("app", "warn", "helper 已卸载"));
    snapshot::build_snapshot(&app, &state).await
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
    snapshot::build_snapshot(&app, &state).await
}
