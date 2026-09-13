//! 菜单栏图标与快速切换菜单。
//!
//! 代理工具的常态是「窗口关着但人在用」。托盘菜单是这个场景下唯一的入口，
//! 所以模式切换、延迟刷新、退出都必须在这里能找到，不能只放在主窗口里。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::events;
use xt_core::model::ProxyMode;
use crate::AppState;

const ID_SHOW: &str = "tray.show";
const ID_DIRECT: &str = "tray.mode.direct";
const ID_SYSTEM_PROXY: &str = "tray.mode.system_proxy";
const ID_TUN: &str = "tray.mode.tun";
const ID_PROBE: &str = "tray.probe";
const ID_RESTORE: &str = "tray.restore";
const ID_QUIT: &str = "tray.quit";

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, ID_SHOW, "显示主窗口", true, None::<&str>)?;
    let direct = MenuItem::with_id(app, ID_DIRECT, "直连", true, None::<&str>)?;
    let system_proxy = MenuItem::with_id(app, ID_SYSTEM_PROXY, "系统代理", true, None::<&str>)?;
    let tun = MenuItem::with_id(app, ID_TUN, "TUN 模式（全局）", true, None::<&str>)?;
    let probe = MenuItem::with_id(app, ID_PROBE, "测试延迟", true, None::<&str>)?;
    let restore = MenuItem::with_id(app, ID_RESTORE, "修复网络（回滚遗留配置）", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "退出 XrayTun", true, None::<&str>)?;

    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&show, &sep, &direct, &system_proxy, &tun, &sep, &probe, &restore, &sep, &quit],
    )?;

    // 图标用 `iconAsTemplate`：macOS 会按菜单栏明暗主题自动反色。
    // 不放这个标志的话，深色模式下图标会变成一块看不清的色斑。
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("XrayTun")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // 左键单击显示窗口 —— 这是 macOS 菜单栏应用的默认交互预期。
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        ID_SHOW => show_main_window(app),
        ID_QUIT => {
            // 直接退出可能会把系统留在「路由还指着已经消失的 utun」的状态。
            // 先同步回滚，再退 —— 这一步不能省。
            shutdown_and_exit(app);
        }
        ID_RESTORE => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(state) = app.try_state::<AppState>() {
                    let mut helper = state.helper.lock().await;
                    let outcome = helper.call(&xt_proto::Request::Restore);
                    drop(helper);
                    state.with(|i| match outcome {
                        Ok(_) => i.push_log("app", "info", "已回滚遗留网络配置"),
                        Err(e) => i.push_log("app", "error", format!("回滚失败：{}", e.message)),
                    });
                }
                events::runtime_changed(&app, &app.state::<AppState>());
            });
        }
        ID_PROBE => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                // 复用命令层，避免在托盘里重写一遍探测逻辑。
                let state = app.state::<AppState>();
                let _ = crate::commands::test_latency(app.clone(), state, None).await;
            });
        }
        ID_DIRECT | ID_SYSTEM_PROXY | ID_TUN => {
            let mode = match id {
                ID_DIRECT => ProxyMode::Direct,
                ID_SYSTEM_PROXY => ProxyMode::SystemProxy,
                _ => ProxyMode::Tun,
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                match crate::commands::set_mode(app.clone(), state, mode).await {
                    Ok(_) => {}
                    Err(e) => {
                        let state = app.state::<AppState>();
                        state.with(|i| {
                            i.push_log("app", "error", format!("切换模式失败：{e}"));
                            i.last_notice = Some(e.clone());
                        });
                        events::runtime_changed(&app, &state);
                    }
                }
            });
        }
        _ => {}
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 退出前回滚网络并停掉核心。
///
/// **完全同步**，刻意不走 `Supervisor::stop`（异步）。原因：这个函数是从
/// 托盘菜单回调里调的，随后立刻 `app.exit(0)` —— 任何还没被 poll 到的
/// async 清理都不会执行，用户就会留在「路由指向一个已消失的 utun」的状态，
/// 也就是彻底断网。
///
/// 另外两条路都堵死了：
/// * `tauri::async_runtime::block_on` 在事件回调线程上可能直接死锁；
/// * 先 spawn 清理再退出，则清理与退出是竞态，等于没清理。
///
/// 所以这里做两件**同步且足够**的事：
/// 1. 让 helper 按磁盘快照回滚（一次本机 IPC，~1ms）；
/// 2. `SIGTERM` 核心进程（不等待退出 —— helper 的回滚不依赖核心先死）。
///
/// 即使这一步也失败，helper 下次启动时会读快照再回滚一次，
/// 这就是快照要落盘的原因。
fn shutdown_and_exit(app: &AppHandle) {
    sync_cleanup(app);
    app.exit(0);
}

/// 退出前**同步**回滚网络并停掉核心。可重复调用（第二次起都是空操作）。
///
/// 为什么必须同步：清理是「改系统路由 + 杀进程」，而调用方紧接着就要让
/// 进程消失。任何还没被 poll 到的 async 清理都不会执行。
///
/// 为什么托盘退出和 `RunEvent::ExitRequested` 都要调它：**`app.exit()`
/// 不会运行析构函数**，所以 `XrayProcess` 上那个 `kill_on_drop(true)`
/// 在退出路径上根本不生效 —— 它注释里承诺的「即便上层忘了 shutdown，
/// 进程也不会变成孤儿」只在正常作用域结束时成立。后果是核心变成孤儿，
/// 继续占着 10808 / 10809 / 10085，**下一次点连接会直接因为端口被占而失败**
/// （实测踩到过：核心被留在后台，入站端口全部仍被监听）。
///
/// 而 ⌘Q 必须单独接：菜单里的退出走 Tauri 的默认流程，
/// 完全绕过托盘那个「退出 XrayTun」菜单项。
pub fn sync_cleanup(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let running = state.with(|i| i.runtime.running).unwrap_or(false);
    let pid = state.with(|i| i.runtime.pid).flatten();
    if !running && pid.is_none() {
        return;
    }

    // 这里**必须**用 try_lock：我们在一个同步回调里，拿不到锁就直接放弃
    // —— helper 下次启动时会按磁盘快照再回滚一次，那才是最终保障。
    // 绝不要在这里阻塞等待（可能死锁）。
    match state.helper.try_lock() {
        Ok(mut helper) => match helper.call(&xt_proto::Request::Restore) {
            Ok(_) => tracing::info!("退出前已回滚网络配置"),
            Err(e) => tracing::error!(
                error = %e.message,
                "退出前回滚失败；helper 会在下次启动时按磁盘快照重试"
            ),
        },
        Err(_) => tracing::warn!("helper 正忙，跳过退出前回滚；下次启动会自动修复"),
    }

    if let Some(pid) = pid {
        tracing::info!(pid, "退出前终止核心进程");
        // SAFETY: kill 只读 pid；进程可能已退出，失败无害。
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        // 给核心一点时间释放端口。等太久会拖慢退出，但端口冲突的代价更大
        // —— 用户下次点连接会直接失败，且看不出原因。
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    // 标记为已停止：ExitRequested 再跑一遍时直接返回，不会重复处理。
    state.with(|i| {
        i.runtime.running = false;
        i.runtime.pid = None;
    });
}
