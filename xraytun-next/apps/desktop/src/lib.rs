//! xraytun-next 桌面壳（Tauri 2）：菜单栏 + 主窗口 + daemon IPC 桥。
//!
//! # 薄壳 vs daemon：边界写在这里
//!
//! ```text
//! XrayTun Next.app（普通用户，本 crate）
//!   ├── 承载 apps/ui（WebView）                       ← 只渲染 + 采集输入
//!   ├── 菜单栏（显示窗口 / 退出）                       ← tray.rs
//!   ├── 拉起 xt-daemon（独立二进制，普通用户进程）        ← daemon_launch.rs
//!   └── 桥：UI 的请求 → daemon，daemon 的事件 → UI       ← bridge.rs
//!
//! xt-daemon（普通用户，独立进程，**不是**本 crate 的一部分）
//!   └── settings / subs / nodes / lifecycle / datapath / stats / probe
//! ```
//!
//! 壳**不做**任何业务判断：不选节点、不生成配置、不改系统网络、不缓存业务状态。
//! 它只认识 `xt-contract` 的帧类型与 `xt-ipc::Client`。一旦壳里出现「如果连接失败
//! 就换一个节点」这类逻辑，边界就破了 —— 那属于 daemon。
//!
//! 为什么 daemon 是**独立进程**而不是壳里的一个线程：见
//! `docs/architecture/00-CONTRACT-FREEZE.md` §1（UI 崩溃不牵连隧道、业务可脱离
//! WebView 验证、CLI 与 UI 共用同一份契约）。本文件的职责只有「把它拉起来」。
//!
//! # 桥的线上形状（逐字对齐 `apps/ui/src/transport/tauri.ts`）
//!
//! * 命令名 `xt_daemon_request`，参数 `{ socketPath: string, id: number, request: object }`。
//!   Tauri 2 默认把 Rust 的 snake_case 参数映射成 JS 的 camelCase，所以 Rust 侧写
//!   `socket_path` 就能接住 UI 的 `socketPath`。
//! * **返回值是 `xt_contract::protocol::Outcome`**，不是裸 `Response`：
//!   `{"status":"ok","response":{...}}` 或 `{"status":"error","error":{...}}`。
//!   UI 明确按 `outcome.status` 分流（`tauri.ts` 的 `invoke<Outcome>`），所以任何失败
//!   都必须以 `Outcome::Error` **解析成功**地返回，而不是让 `invoke` reject —— 后者
//!   会被 UI 判成「返回值不是 Outcome」。这一点与任务书里「返回 daemon 的一帧
//!   Response」的说法不一致；以**代码**为准（任务书自己也要求「读它、精确对齐」）。
//! * 事件名 `xt_daemon_event`，载荷 `{ seq: number, event: Event }`（`tauri.ts` 读
//!   `payload.seq` 与 `payload.event`）。见 `bridge` 模块文档里关于 `seq` 的诚实说明。
//!
//! # 未验证声明（不许把它说成「可用」）
//!
//! 本机是 Linux：没有 macOS、没有 webkit2gtk（Linux 上编译 tauri 需要它）、没有
//! tauri-cli，**本轮一次 `cargo check` / `tauri build` / 真实 `invoke`·`listen`
//! 往返都没跑过**（`cargo` 与 stable 工具链在 `.cargo/bin` 下存在，但缺 GUI 系统库，
//! 编译必然停在环境而不是代码上；按任务要求也没有运行）。因此：
//! * 本 crate 只保证「按契约形状写完」，不构成「在 mac 上能用」的声明；
//! * daemon 的路径解析与生命周期是**占位**（见 `daemon_launch`），真机 S6 再对齐；
//! * 打包（`bundle.resources` 里加 `xt-daemon`）**未接线**，见 `binaries/README.md`。
//!
//! # unsafe
//!
//! 本 crate 当前**没有任何 `unsafe` 块**。如果将来必须引入，每一块都要带 `SAFETY:`
//! 注释说明「为什么这里的不变量成立」—— 没有注释的 unsafe 视为未完成。

pub mod bridge;
pub mod daemon_launch;
pub mod tray;

use std::path::Path;

/// 应用入口。`main.rs` 只有一行，真正逻辑在这里（便于将来加集成测试）。
pub fn run() {
    // socket 是壳与 UI 的**唯一**约定值：壳用它拉起 daemon，并通过
    // `initialization_script` 把它写给 UI 的 `window.__XT_SOCKET__`。
    let socket = daemon_launch::socket_path();
    eprintln!("xt shell: daemon socket = {}", socket.display());

    let built = tauri::Builder::default()
        // 桥的全局状态：懒连接 + 事件序号。见 bridge 模块。
        .manage(bridge::BridgeState::new())
        .setup(move |app| {
            let handle = app.handle().clone();

            // ① 主窗口。配置里 `create: false`，所以由这里建 —— 唯一目的是在页面脚本
            //    执行**之前**注入 `__XT_SOCKET__`（UI 在模块求值时读它，见 main.tsx）。
            //    `initialization_script` 是 Tauri 2 里唯一能保证时序的机制；`eval` 在
            //    setup 里调不可靠（页面可能还没加载）。
            create_main_window(app, &socket);

            // ② 菜单栏。它是可选能力：建不起来不该让整个 App 起不来。
            if let Err(error) = tray::build(&handle) {
                eprintln!("xt shell: 菜单栏图标创建失败：{error}（界面功能不受影响）");
            }

            // ③ 拉起 xt-daemon。**必须**走 `tauri::async_runtime::spawn`：
            //    `setup` 回调不在 tokio runtime context 里，裸 `tokio::spawn` /
            //    `tokio::process::Command::spawn` 会 panic
            //    （`there is no reactor running, must be called from the context of a
            //    Tokio 1.x runtime`）。老仓库 0.8.39 正是踩了裸 spawn 的 SIGABRT。
            let launch_socket = socket.clone();
            tauri::async_runtime::spawn(async move {
                match daemon_launch::ensure_daemon(&launch_socket).await {
                    daemon_launch::Launch::AlreadyListening => eprintln!(
                        "xt shell: 已有 daemon 在 {} 上监听，复用（不再启动第二个）",
                        launch_socket.display()
                    ),
                    daemon_launch::Launch::Spawned { pid, bin } => eprintln!(
                        "xt shell: 已拉起 xt-daemon（pid={pid:?}，bin={}）",
                        bin.display()
                    ),
                    daemon_launch::Launch::Unavailable(reason) => eprintln!(
                        "xt shell: 未能拉起 xt-daemon：{reason}（界面连不上时会如实报错，壳不伪造连接）"
                    ),
                }
            });

            // 启动路径上任何失败都只记日志后继续：`setup` 返回 Err 会被 Tauri 包成
            // `Error::Setup` 让 `build()` 失败，启动路径就整条断掉。
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口不退出 App：这是 macOS 菜单栏应用的预期行为。真退出走托盘菜单。
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![bridge::xt_daemon_request])
        .build(tauri::generate_context!());

    match built {
        Ok(app) => app.run(|_app, _event| {}),
        Err(error) => {
            // 不 `.expect(...)`：release 下 panic=abort 会把启动失败变成不可读的
            // `abort() called`。这里给出可读原因并以**非零退出码**结束。
            eprintln!("XrayTun Next 启动失败：{error}");
            std::process::exit(1);
        }
    }
}

/// 按 `tauri.conf.json` 里的 `label = "main"` 配置建主窗口，并注入 socket 路径。
///
/// 为什么用 `from_config` 而不是把窗口完全写死在 Rust 里：窗口的尺寸/标题/最小尺寸
/// 属于配置，应该留在 `tauri.conf.json`（一处可改）；Rust 只额外加**一个**配置里
/// 表达不了的东西 —— 页面加载前执行的初始化脚本。
fn create_main_window(app: &mut tauri::App, socket: &Path) {
    let Some(config) = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .cloned()
    else {
        eprintln!("xt shell: tauri.conf.json 里没有 label=main 的窗口配置，主窗口不会创建");
        return;
    };

    // JSON 编码而不是字符串拼接：socket 路径里任何引号/反斜杠都不会破坏脚本。
    let socket_string = socket.to_string_lossy().to_string();
    let script = match serde_json::to_string(&socket_string) {
        Ok(json) => format!("window.__XT_SOCKET__ = {json};"),
        Err(error) => {
            eprintln!("xt shell: socket 路径无法编码为 JSON：{error}；跳过初始化脚本");
            return;
        }
    };

    let builder = match tauri::WebviewWindowBuilder::from_config(app.handle(), &config) {
        Ok(builder) => builder,
        Err(error) => {
            eprintln!("xt shell: 主窗口配置无效：{error}");
            return;
        }
    };
    if let Err(error) = builder.initialization_script(script).build() {
        eprintln!("xt shell: 主窗口创建失败：{error}（菜单栏仍可用，退出请用托盘菜单）");
    }
}
