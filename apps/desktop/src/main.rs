// 关闭 Windows 上的控制台窗口。macOS 上无副作用，但保留这行是为了
// 万一将来扩展到 Windows 时不用再想起来。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    xraytun_desktop_lib::run()
}
