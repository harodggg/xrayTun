//! xt-helperproto —— 特权 helper 的**封闭**指令集。
//!
//! 设计约束（见 docs/design/MACOS-APP-PLAN.md §5 与 docs/architecture/ARCHITECTURE.md §1.2）：
//! helper 不认识"代理"，只认识「建卡/交 fd/提交路由/还原」这几件事；
//! 不接受任意命令、任意路径、任意文件写入。帧格式复用 `xt-ipc`。
//!
//! 所有者：macosnet（S1/S2）。
