//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn test_latency(
    app: AppHandle,
    state: State<'_, AppState>,
    node_ids: Option<Vec<String>>,
) -> Result<AppSnapshot, String> {
    let (nodes, core_path) = state
        .with(|i| {
            let nodes: Vec<Node> = i
                .nodes
                .iter()
                .filter(|n| node_ids.as_ref().map(|v| v.contains(&n.id)).unwrap_or(true))
                .cloned()
                .collect();
            (nodes, i.settings.core_path.clone())
        })
        .ok_or(util::STATE_UNAVAILABLE)?;

    if nodes.is_empty() {
        return Err("没有可测试的节点".into());
    }

    let resource_dir = app.path().resource_dir().ok();
    let dev_dir = crate::dev_binaries_dir();
    let binary = xray::resolve_core_binary(
        core_path.as_deref(),
        Some(&xt_core::update::managed_core_dir(state.store.root())),
        resource_dir.as_deref(),
        dev_dir.as_deref(),
    )
        .map_err(util::user_msg)?;

    state.log("app", "info", format!("开始测试 {} 个节点的延迟", nodes.len()));
    events::probe_started(&app, nodes.len());

    // 物理出口：RTT 必须**在隧道之外**测，否则隧道开着时握手被本地协议栈
    // 立刻应答，测出来是 0ms（实测：不绑 en0 是 0ms，绑了是 53ms）。
    let interface = xt_tun::macos::route::default_route()
        .ok()
        .map(|r| r.interface);

    let started = Instant::now();
    let results = crate::supervisor::probe(&nodes, &binary, Duration::from_secs(5), interface.as_deref())
        .await
        .map_err(|e| {
            state.log("app", "error", format!("探测失败：{e}"));
            e
        })?;

    let ok_count = results.iter().filter(|r| r.ok()).count();
    state.with(|i| {
        for r in &results {
            i.latencies.insert(r.node_id.clone(), r.clone());
        }
        i.push_log(
            "app",
            "info",
            format!(
                "探测完成：{ok_count}/{} 可用，耗时 {:.1}s",
                results.len(),
                started.elapsed().as_secs_f32()
            ),
        );
    });
    events::latency_updated(&app, &results);
    snapshot::build_snapshot(&app, &state).await
}
