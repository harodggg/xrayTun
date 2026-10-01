// 关闭 Windows 上的控制台窗口。macOS 上无副作用，保留是为了万一将来扩展到 Windows。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    xraytun_desktop_lib::run()
}
