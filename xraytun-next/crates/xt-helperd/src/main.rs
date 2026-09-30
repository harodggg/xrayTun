//! xt-helperd —— 特权 helper 守护进程（root）。
//!
//! 职责边界是**封闭的**：只接受 xt-helperproto 里那几条指令；
//! 数据面可执行文件路径必须在白名单目录内；外部命令一律绝对路径 + argv，永不经过 shell。
//!
//! 本文件在非 macOS 平台不会被打包（S2 阶段由 macos-core 流水线构建）。所有者：macosnet。
fn main() {
    eprintln!("xt-helperd: 尚未实现（S2）；见 docs/design/MACOS-APP-PLAN.md");
    std::process::exit(2);
}
