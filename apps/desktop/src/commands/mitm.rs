//! MITM 通道的 UI 入口（P4 第四步）。
//!
//! # 这一层只做四件事
//!
//! 1. [`mitm_status`]：把运行态、CA 指纹、"为什么没生效"一次交给界面；
//! 2. [`mitm_ca_install`] / [`mitm_ca_remove`]：经**特权 helper** 装/卸根证书
//!    （这是整个功能里唯一会改系统状态的动作，所以它必须是一次显式的用户点击）；
//! 3. [`mitm_apply`]：按当前设置起/停本地代理。
//!
//! # 为什么"装完证书"不等于"已经生效"
//!
//! 引导规则要挂在 `mitm-out` 出站与 `mitm-upstream` 入站上，而这两样**没法热加**
//! （`RoutingService` 只改规则表）。所以链路是：
//!
//! ```text
//! 点「装根证书」→ 状态里出现 core_restart_required=true → 重连核心 → 引导规则生效
//! ```
//!
//! 界面必须把这一步画出来，而不是让用户面对"装了却没用"的沉默。

use tauri::State;

use xt_proto::{Request, Response};

use crate::mitm::MitmStatus;
use crate::state::AppState;

/// 当前 MITM 状态（含"为什么没在跑"的一句人话）。
#[tauri::command]
pub async fn mitm_status(state: State<'_, AppState>) -> Result<MitmStatus, String> {
    state
        .with(|i| {
            let settings = i.settings.mitm.clone();
            let trusted = crate::mitm::ca_is_trusted(i.mitm.existing_fingerprint().as_deref());
            i.mitm.status(&settings, trusted)
        })
        .ok_or_else(|| "读取 MITM 状态失败（状态锁不可用）".to_string())
}

/// 把本会话的根证书装进系统钥匙串。
///
/// **先取 PEM 与指纹**（同步、短锁），再在锁外 await helper —— 绝不跨 await 持锁。
/// helper 侧会校验 PEM 形状（拒收含私钥的 PEM）与指纹格式（它是文件名的一部分），
/// 并把它记进会话快照，于是"helper 被杀掉 / 下次启动"都能自动回滚。
#[tauri::command]
pub async fn mitm_ca_install(state: State<'_, AppState>) -> Result<MitmStatus, String> {
    let pair = state.with(|i| {
        let fp = i.mitm.ca_fingerprint()?;
        let pem = i.mitm.ca_pem()?;
        Ok::<_, String>((pem, fp))
    });
    let (pem, fingerprint) = match pair {
        Some(Ok(v)) => v,
        // 生成失败的原因要原样透出来（"为什么装不上"必须可读）。
        Some(Err(e)) => return Err(e),
        None => return Err("读取状态失败（状态锁不可用）".to_string()),
    };

    let response = {
        let mut helper = state.helper.lock().await;
        helper
            .call(&Request::InstallTrustAnchor { pem, fingerprint: fingerprint.clone() })
            .map_err(|e| format!("调用特权 helper 失败：{e}"))?
    };
    match response {
        Response::Trust(t) if t.installed => {
            state.log(
                "mitm",
                "info",
                format!(
                    "已把本地根证书装进系统钥匙串（{}；之前{}）",
                    fingerprint,
                    if t.existed_before { "已存在" } else { "不存在" }
                ),
            );
        }
        Response::Trust(t) => {
            return Err(format!(
                "根证书没有装进钥匙串：{}",
                t.note.unwrap_or_else(|| "helper 没有给出原因".to_string())
            ));
        }
        other => return Err(format!("helper 返回了意外的应答：{other:?}")),
    }

    mitm_apply(state).await
}

/// 从系统钥匙串里撤掉本会话的根证书（幂等）。
#[tauri::command]
pub async fn mitm_ca_remove(state: State<'_, AppState>) -> Result<MitmStatus, String> {
    let fingerprint = state
        .with(|i| i.mitm.existing_fingerprint())
        .flatten()
        .ok_or_else(|| "本会话还没有生成过根证书（没有东西可以撤）".to_string())?;

    let response = {
        let mut helper = state.helper.lock().await;
        helper
            .call(&Request::RemoveTrustAnchor { fingerprint: fingerprint.clone() })
            .map_err(|e| format!("调用特权 helper 失败：{e}"))?
    };
    match response {
        Response::Trust(t) => {
            state.log(
                "mitm",
                "info",
                format!("已从系统钥匙串撤掉根证书 {}", t.fingerprint),
            );
        }
        other => return Err(format!("helper 返回了意外的应答：{other:?}")),
    }

    // 撤掉信任之后**必须**停掉代理：它会继续用一张没人信的证书接流量，
    // 而核心下次重连时也不会再带引导规则（`core_settings` 的闸门）。
    state.with(|i| i.mitm.stop());
    mitm_status(state).await
}

/// 按当前设置起/停本地代理。返回最新状态。
///
/// 判定名单取**意图引擎的拦截带**（扣掉用户放行过的），于是同一个域名在
/// 域名层与内容层不会得到两个结论。
#[tauri::command]
pub async fn mitm_apply(state: State<'_, AppState>) -> Result<MitmStatus, String> {
    let snapshot = state
        .with(|i| {
            let settings = i.settings.mitm.clone();
            let allow = i.intent.rules().allow_domains();
            let mut block: Vec<String> = i.intent.rules().block_domains();
            block.retain(|d| !allow.iter().any(|a| a == d));
            let trusted = crate::mitm::ca_is_trusted(i.mitm.existing_fingerprint().as_deref());
            (settings, block, trusted)
        })
        .ok_or_else(|| "读取状态失败（状态锁不可用）".to_string())?;
    let (settings, block_hosts, trusted) = snapshot;

    // 证书没被信任时**不起代理**：起了也只会拆出一堆没人信的证书。
    // 状态里的 note 会说清楚是哪一道闸门没过。
    if !settings.is_active() || !trusted {
        state.with(|i| i.mitm.stop());
        return mitm_status(state).await;
    }

    let rewriter = crate::mitm::rewriter_for(&settings);
    state
        .with(|i| i.mitm.start(&settings, block_hosts, rewriter))
        .ok_or_else(|| "启动 MITM 失败（状态锁不可用）".to_string())??;
    mitm_status(state).await
}
