//! 菜单栏图标。
//!
//! macOS 上的常态是「窗口关着但代理在跑」，所以菜单栏是这个应用唯一的兜底入口。
//! 本轮菜单只有两项：**显示窗口** 与 **退出**（任务书要求的最小集）。
//! 代理相关的动作（连接/断开/切节点）刻意不放在这里：它们全部由 daemon 拥有，
//! 托盘要触发就得先定义一条「壳 → daemon」的意图通道，那属于 S3/S4，不猜。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const ID_SHOW: &str = "tray.show";
const ID_QUIT: &str = "tray.quit";

/// 建菜单栏。返回 `Err` 时调用方只记日志 —— 图标是可选能力，不该拖垮整个 App。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, ID_SHOW, "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "退出 XrayTun Next", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &separator, &quit])?;

    // `icon_as_template(true)`：macOS 按菜单栏明暗主题自动反色。漏掉它，深色模式下
    // 图标会变成一块看不清的色斑。
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("XrayTun Next")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // 左键单击显示窗口 —— macOS 菜单栏应用的默认交互预期。
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
        // 退出的生命周期语义见 `daemon_launch` 的模块文档：壳不杀 daemon，
        // 它作为独立进程留在后台，下次启动会被复用。真机 S6 再决定这是不是想要的。
        ID_QUIT => app.exit(0),
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
