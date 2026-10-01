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
//! # 验证状态（不许把它说成「可用」）
//!
//! * **编译与打包**：`macos-app` 作业在 macOS 14 上跑
//!   `tauri build --target universal-apple-darwin`，并校验四个二进制的 `lipo -archs`
//!   与 `codesign --verify --strict`。截至 v1.1.1 全绿；`bundle.resources` 已把
//!   xt-daemon / xt-helperd / xray 映射进 `Contents/Resources/`。
//! * **本机（Linux）**：没有 webkit2gtk，本 crate **无法**在本地 `cargo check` ——
//!   改这里的代码，macOS CI 就是唯一的编译器。别凭「看起来对」就算过。
//! * **运行时**：TUN 接管、helper 安装/回滚、菜单栏交互仍**未真机验收**（S6）。
//!   编译通过、能起来、能看见窗口，都**不等于**在 mac 上可用。
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

            // ① 菜单栏。它是可选能力：建不起来不该让整个 App 起不来。
            //    先把菜单栏建好，这样「等 daemon」期间应用也不是一个没有反应的空壳。
            if let Err(error) = tray::build(&handle) {
                eprintln!("xt shell: 菜单栏图标创建失败：{error}（界面功能不受影响）");
            }

            // ② 拉起 xt-daemon，**等它确认已经在监听**，然后才建主窗口。
            //
            //    顺序绝不能反（2026-10-01 真机 S6 的教训）：UI 的引导链只发一次
            //    hello、失败不重发（这是刻意的 —— 本项目禁自动重试），所以那唯一一次
            //    请求必须落在**已经 bind 完成**的 daemon 上。窗口先建、daemon 后起时，
            //    页面脚本可能抢在 bind 之前发出请求，那一次必然以 `io` 失败，界面就
            //    永久停在「连不上」——用户只看到「未知 + 设置未加载」，没有任何出路。
            //
            //    为什么窗口改到异步任务里建：等就绪是异步的，而 `setup` 必须立刻返回。
            //    `run_on_main_thread` 是 Tauri 给的「回到主线程再执行」的正式通道，
            //    建窗口必须在那里做。窗口建在等待之后，`__XT_SOCKET__` 的注入时序不变
            //    （注入发生在建窗口时、页面脚本之前）。
            //
            //    必须走 `tauri::async_runtime::spawn`：`setup` 回调不在 tokio runtime
            //    context 里，裸 `tokio::spawn` / `tokio::process::Command::spawn` 会 panic
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
                        "xt shell: 已拉起 xt-daemon 并确认在监听（pid={pid:?}，bin={}）",
                        bin.display()
                    ),
                    daemon_launch::Launch::NotListening { pid, bin, reason } => eprintln!(
                        "xt shell: 拉起了 xt-daemon（pid={pid:?}，bin={}）但没能确认它在监听：\
                         {reason}（界面连不上时会如实报错，壳不伪造连接）",
                        bin.display()
                    ),
                    daemon_launch::Launch::Unavailable(reason) => eprintln!(
                        "xt shell: 未能拉起 xt-daemon：{reason}（界面连不上时会如实报错，壳不伪造连接）"
                    ),
                }

                // 无论上面是哪种结局都建窗口：连不上也必须让界面把原因显示出来，
                // 「干脆不建窗口」等于把一次失败藏起来。
                let window_handle = handle.clone();
                let window_socket = launch_socket.clone();
                if let Err(error) = handle.run_on_main_thread(move || {
                    create_main_window(&window_handle, &window_socket);
                }) {
                    eprintln!(
                        "xt shell: 无法回到主线程建主窗口：{error}\
                         （菜单栏仍可用，退出请用托盘菜单）"
                    );
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
///
/// 参数是 `AppHandle` 而不是 `&mut App`：调用点在 `run_on_main_thread` 的回调里，
/// 那里能拿到的就是 `AppHandle`（`setup` 的 `&mut App` 早就随 setup 返回失效了）。
fn create_main_window(app: &tauri::AppHandle, socket: &Path) {
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
    let mut script = match serde_json::to_string(&socket_string) {
        Ok(json) => format!("window.__XT_SOCKET__ = {json};"),
        Err(error) => {
            eprintln!("xt shell: socket 路径无法编码为 JSON：{error}；跳过初始化脚本");
            return;
        }
    };

    // CI/调试用的初始页注入（`XT_INITIAL_PAGE`）。
    //
    // 为什么值得有这一条：真机证据里「daemon 版本 / pid」与「设置未加载」这两件事
    // **只出现在设置页**（`apps/ui/src/pages/Settings.tsx`），而 CI 上**点不动**界面 ——
    // AppleScript 拿不到 WebView 里的按钮（实测报 -1728）。没有这个钩子，那张图只能靠
    // 人手截；有了它，任意一页都能在 CI 里免费出图。
    //
    // 取值走**白名单**，且与 `apps/ui/src/App.tsx` 的页面 id 逐字一致：这个值来自环境
    // 变量，绝不能让任意内容进到注入脚本里。未知值/空值 = 不注入，界面照常停在默认页。
    if let Some(page) = std::env::var("XT_INITIAL_PAGE")
        .ok()
        .filter(|page| matches!(page.as_str(), "dashboard" | "nodes" | "logs" | "settings"))
    {
        if let Ok(json) = serde_json::to_string(&page) {
            script.push_str(&format!("window.__XT_INITIAL_PAGE__ = {json};"));
        }
    }

    let builder = match tauri::WebviewWindowBuilder::from_config(app, &config) {
        Ok(builder) => builder,
        Err(error) => {
            eprintln!("xt shell: 主窗口配置无效：{error}");
            return;
        }
    };
    // 页面**开始加载** = 一个新的 WebView 会话：桥的序号闸门必须在这里关上。
    //
    // 为什么不能只靠 `hello`：`hello` 是**事后**信号，而桥缓存着连接时 daemon 的
    // 事件是持续到达的 —— 完全可能抢在 hello 之前推给这个还没有序号的页面，那第一帧
    // 就会被 UI 判「seq 跳号」并进入不可恢复的致命态（`tauri.ts` 的 `fail()` 会把
    // 客户端实例永久钉死）。重载后「窗口一片错误、点重新连接也没用」正是这么来的。
    if let Err(error) = builder
        .initialization_script(script)
        .on_page_load(|window, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                bridge::close_gate_for_new_page(&window);
            }
        })
        .build()
    {
        eprintln!("xt shell: 主窗口创建失败：{error}（菜单栏仍可用，退出请用托盘菜单）");
    }
}
