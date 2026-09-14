//! XrayTun 桌面端（Tauri 2 外壳）。
//!
//! # 权限边界
//!
//! ```text
//! XrayTun.app（普通用户）
//!   ├── 订阅 / 节点 / 规则 / UI        ← xt-core
//!   ├── 生成 Xray 配置并拉起核心         ← xt-core（作为普通用户的子进程）
//!   └── 通过 Unix socket 请求 helper     ← xt-proto::transport
//!
//! com.xraytun.helper（root LaunchDaemon）
//!   └── 只做：建 utun、装路由、改 DNS、按快照回滚
//! ```
//!
//! TUN 模式下核心由 **GUI 以普通用户身份**拉起，utun fd 由 helper 通过
//! `SCM_RIGHTS` 交付，核心通过 `XRAY_TUN_FD` 接收。
//! 代价是要依赖一条上游文档未承诺（但对 macOS 生效）的代码路径，
//! 收益是**整个代理内核都不需要 root**。取舍与退路见
//! `docs/02-tun-and-privileges.md`。

pub mod commands;
pub mod events;
pub mod helper_client;
pub mod helper_install;
pub mod login_item;
pub mod state;
pub mod supervisor;
pub mod traffic;
pub mod tray;

use tauri::{Manager, WindowEvent};

use state::AppState;

/// 开发期核心所在的目录。
///
/// **必须由应用提供**，不能让 `xt-core` 自己去猜：`option_env!("CARGO_MANIFEST_DIR")`
/// 在**包含它的 crate** 编译时展开，写在 `xt-core` 里拿到的是 `crates/xt-core`，
/// 而应用把核心放在 `apps/desktop/binaries/`。这个坑曾经真的发生过，
/// 表现是「路径看起来都对，但永远找不到核心」。
pub fn dev_binaries_dir() -> Option<std::path::PathBuf> {
    if cfg!(debug_assertions) {
        option_env!("CARGO_MANIFEST_DIR").map(|d| std::path::Path::new(d).join("binaries"))
    } else {
        // 发行版不猜路径：核心要么在 bundle 的 Resources 里，要么由用户指定。
        None
    }
}

/// 应用入口。`main.rs` 只有一行，真正的逻辑都在这里，方便将来加集成测试。
pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_noop())
        .setup(|app| {
            let store = xt_core::store::Store::with_default_root();
            let handle = app.handle().clone();
            app.manage(AppState::new(store));

            // 托盘是可选能力：构建失败不应该让整个 App 起不来。
            if let Err(e) = tray::build(&handle) {
                tracing::warn!(error = %e, "菜单栏图标创建失败，功能不受影响");
            }

            // 启动时做一次「健康检查 + 遗留清理」，并把结果写进日志与提示条。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                bootstrap(handle).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口不退出 App：这是 macOS 菜单栏应用的预期行为。
            // 真的退出要走托盘菜单的「退出 XrayTun」（那里会先回滚网络）。
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::save_settings,
            commands::set_mode,
            commands::start_proxy,
            commands::stop_proxy,
            commands::select_node,
            commands::add_manual_node,
            commands::delete_node,
            commands::add_subscription,
            commands::remove_subscription,
            commands::refresh_subscriptions,
            commands::test_latency,
            commands::probe_helper,
            commands::install_helper,
            commands::restart_helper,
            commands::uninstall_helper,
            commands::restore_stale,
            commands::tail_logs,
            commands::clear_logs,
            commands::diagnostics,
            commands::open_data_dir,
            commands::set_launch_at_login,
            commands::open_login_item_settings,
            commands::export_node,
            commands::check_updates,
            commands::install_core_update,
            commands::install_geo_update,
            commands::revert_managed_update,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败")
        .run(|app, event| {
            // ⌘Q 与菜单里的退出都会走到这里，而它们**不经过**托盘那个
            // 「退出 XrayTun」菜单项 —— 不在这里接一手的话，退出时既不会
            // 回滚路由，也不会杀掉核心：隧道留在系统上、核心变成孤儿并继续
            // 占着入站端口，用户下次点连接会直接失败。
            //
            // `sync_cleanup` 是幂等的，所以托盘那条路已经清理过也没关系。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                crate::tray::sync_cleanup(app);
            }
        });
}

/// 排障用的命令行入口。返回 `Some(退出码)` 表示「已处理，别启动 GUI」。
///
/// 为什么把它放进 App 自己的二进制，而不是单独的排障工具：
/// `SMAppService.mainAppService` 操作的是**调用方所在的 bundle**。
/// 换个二进制来跑，注册的就是那个二进制，而不是这个 App ——
/// 所以只有从 `XrayTun.app/Contents/MacOS/` 里执行才有意义。
///
/// ```bash
/// XrayTun.app/Contents/MacOS/xraytun-desktop --login-item status
/// XrayTun.app/Contents/MacOS/xraytun-desktop --login-item enable
/// XrayTun.app/Contents/MacOS/xraytun-desktop --login-item disable
/// ```
pub fn login_item_cli(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("--login-item") {
        return None;
    }
    let op = args.get(1).map(String::as_str).unwrap_or("status");
    let result = match op {
        "status" => login_item::status().map(|s| println!("{}（{}）", s.describe(), s.as_str())),
        "enable" => login_item::apply(true).map(|s| println!("{}（{}）", s.describe(), s.as_str())),
        "disable" => login_item::apply(false).map(|s| println!("{}（{}）", s.describe(), s.as_str())),
        "settings" => login_item::open_system_settings().map(|()| println!("已打开系统设置的登录项页面")),
        other => {
            eprintln!("未知操作：{other}（可用：status / enable / disable / settings）");
            return Some(2);
        }
    };
    Some(match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("✗ {e}");
            1
        }
    })
}

/// 启动后的自检。
///
/// 做三件事，全部是「尽早把问题暴露给用户」：
/// 1. 找核心：找不到就直接在 UI 上说明怎么放；
/// 2. 探 helper：区分「没装」「没批准」「连不上」三种情况，给出不同指引；
/// 3. 清理遗留：上次崩溃留下的路由/DNS 必须在这里被回滚，否则用户一直断网。
async fn bootstrap(app: tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let snapshot = match commands::snapshot(app.clone(), app.state::<AppState>()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "初始快照获取失败");
            return;
        }
    };

    state.with(|i| {
        if let Some(err) = &snapshot.core.error {
            i.push_log("app", "error", format!("未找到 Xray 核心：{err}"));
        } else if let Some(v) = &snapshot.core.version {
            i.push_log("app", "info", format!("已找到核心：{v}"));
        }

        if !snapshot.helper.socket_present {
            i.push_log("app", "warn", "helper 尚未安装，TUN 模式不可用（系统代理模式仍可正常使用）");
        } else if !snapshot.helper.reachable {
            i.push_log(
                "app",
                "warn",
                format!(
                    "helper 不可连接：{}",
                    snapshot.helper.error.clone().unwrap_or_else(|| "未知原因".into())
                ),
            );
        } else {
            i.push_log(
                "app",
                "info",
                format!("helper {} 已就绪", snapshot.helper.version.clone().unwrap_or_default()),
            );
        }

        if let Some(stale) = &snapshot.helper.stale_session {
            i.push_log(
                "app",
                "warn",
                format!("检测到上次崩溃遗留的 TUN 会话 {stale}，正在请求回滚"),
            );
            i.last_notice = Some("检测到上次异常退出，正在修复网络配置…".into());
        } else if snapshot.helper.tun_active {
            // helper 里挂着一条**活着的**会话，但这个 App 实例从没连接过。
            //
            // 这只有一种解释：上一个 App 进程没有干净退出（被 kill、
            // 崩溃、或走的是没拆隧道的退出路径），而 helper 是常驻的，
            // 于是那条会话一直留在内存里、路由也还指着 utun。
            //
            // 它不会出现在 `stale_session` 里 —— 那个字段的判据是
            // 「崩在半路」（`is_stale()`），而一条提交完路由的会话
            // 状态是 `Up`，不算崩在半路。但对我们来说它就是遗留物：
            // 没人再负责拆它了。
            i.push_log(
                "app",
                "warn",
                "检测到 helper 上有一条没有归属的 TUN 会话，正在回滚",
            );
            i.last_notice = Some("检测到上次未正常退出，正在修复网络配置…".into());
        }
    });

    // 遗留会话必须立刻回滚：这直接决定用户「打开 App 之后网能不能通」。
    let orphaned = snapshot.helper.stale_session.is_some() || snapshot.helper.tun_active;
    if orphaned {
        {
            let mut helper = state.helper.lock().await;
            let outcome = helper.call(&xt_proto::Request::Restore);
            drop(helper);
            state.with(|i| match outcome {
                Ok(_) => {
                    i.push_log("app", "info", "遗留网络配置已回滚");
                    i.last_notice = None;
                }
                Err(e) => {
                    let msg = format!("自动修复失败，请点「修复网络」重试：{}", e.message);
                    i.push_log("app", "error", format!("回滚遗留配置失败：{}", e.message));
                    i.last_notice = Some(msg);
                }
            });
        }
    }

    // 必须发**完整载荷**。这里曾经是 `app.emit(RUNTIME_CHANGED, ())`，
    // 而前端的处理函数会直接读 `payload.runtime` / `payload.traffic` ——
    // 于是这一发在 webview 里抛 TypeError，运行时与流量都拿不到更新。
    events::runtime_changed(&app, &state);
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("XRAYTUN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// 占位插件：留出扩展点（自动更新、单实例、通知等）。
///
/// 这里刻意不引入 `tauri-plugin-*` 依赖：骨架阶段多一个插件就多一份
/// 版本兼容负担，而它们都不是「跑起来」的必要条件。
fn tauri_plugin_noop() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("xraytun-noop").build()
}

#[cfg(test)]
mod tests {
    /// 回归测试：开发期核心目录必须指向**应用自己的** `binaries/`，
    /// 而不是 `xt-core` 的。
    ///
    /// 这个 bug 曾经真实发生过：`option_env!("CARGO_MANIFEST_DIR")` 写在
    /// `xt-core` 里，展开成 `crates/xt-core`，于是解析器永远找不到核心 ——
    /// 而代码、路径、注释看起来全都是对的。修复方式是由应用提供这个目录。
    #[test]
    fn dev_binaries_dir_points_at_this_app() {
        // release 构建不猜路径，这是刻意的：找不到就跳过这个测试。
        #[cfg(not(debug_assertions))]
        let Some(dir) = super::dev_binaries_dir() else {
            return;
        };
        #[cfg(debug_assertions)]
        let Some(dir) = super::dev_binaries_dir() else {
            panic!("debug 构建必须提供开发期目录");
        };
        assert!(
            dir.ends_with("apps/desktop/binaries"),
            "开发期目录应指向 apps/desktop/binaries，实际是 {}",
            dir.display()
        );
        assert!(
            !dir.to_string_lossy().contains("crates/xt-core"),
            "绝不能用 xt-core 的 manifest 目录 —— 这正是曾经的 bug"
        );
    }

    /// 在开发机上核心确实存在时，完整的解析链必须能找到它。
    ///
    /// 找不到就跳过（例如 CI 上没下载核心），但**不能让测试假装通过**。
    #[test]
    fn resolve_finds_bundled_core_when_present() {
        let dir = match super::dev_binaries_dir() {
            Some(d) => d,
            None => return,
        };
        if !dir.join("xray").is_file() {
            eprintln!("跳过：{} 下没有 xray（先跑 scripts/fetch-xray.sh）", dir.display());
            return;
        }
        let found = xt_core::xray::resolve_core_binary(None, None, None, Some(&dir))
            .expect("应当能在开发期目录里找到核心");
        assert_eq!(found, dir.join("xray"));
    }
}
