// 关闭 Windows 上的控制台窗口。macOS 上无副作用，但保留这行是为了
// 万一将来扩展到 Windows 时不用再想起来。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 排障子命令必须在启动 GUI 之前处理掉。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = xraytun_desktop_lib::login_item_cli(&args) {
        std::process::exit(code);
    }
    xraytun_desktop_lib::run()
}
