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
pub mod intent;
pub mod helper_install;
pub mod login_item;
pub mod mitm;
/// 节点不可达时的可避免伤害：失败三分类 + 自动回落排序 + 全挂时的节点清单。
pub mod node_health;
pub mod state;
pub mod supervisor;
pub mod traffic;
pub mod tray;
/// 客户端版本自动检测（task-188）。只检测，不下载、不安装。
pub mod version_check;

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

/// 把 panic 写进**文件** —— 否则发行版里的崩溃"没有任何证据"。
///
/// # 为什么必须有它（2026-09-25 现场）
///
/// release profile 是 `panic = "abort"` ⇒ 任何 panic 都变成 `SIGABRT`；
/// 而经 LaunchServices（双击图标）启动时，**stderr 不落在我们能读到的任何地方**：
/// 系统崩溃报告里只有 `abort() called`，没有 Rust 的 panic 消息与位置。
/// 用户报"一打开就崩"，我们手上什么都没有 —— 那次诊断就是这么卡住的。
///
/// 另外 `[profile.release] strip = true` 会**剥掉符号**，所以文件里的 backtrace
/// 只有地址、读不出函数名；但 panic 的**位置字符串**（`文件:行号`）一定在二进制里，
/// 而它已经足够把人直接送到出问题那一行。所以这里优先保证那两行一定落盘。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<未知位置>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<非字符串 panic 负载>".to_string());
        let text = format!(
            "[unix={}] PANIC {location}\n{payload}\nbacktrace:\n{}\n",
            xt_core::util::now_unix(),
            std::backtrace::Backtrace::force_capture()
        );

        // ① 文件：App 数据目录下的 `logs/panic.log`（用户能直接打开、能发给我们）。
        let dir = xt_core::store::Store::default_root().join("logs");
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                use std::io::Write as _;
                let _ = f.write_all(text.as_bytes());
            }
        }
        // ② stderr：终端里直接跑时仍然看得到（`open` 启动时这条会丢，所以①才是主路径）。
        eprintln!("{text}");

        // 保留默认 hook："thread panicked at ..." 那行也别丢。
        default_hook(info);
    }));
}

/// 「启动失败」日志的文件名。
const STARTUP_LOG_NAME: &str = "startup.log";

/// 启动失败时写给用户看的整段文案（**纯函数**，可断言）。
///
/// # 为什么文案里必须有「下一步」
///
/// 现场只有一句「启动失败」时，用户既不知道文件在哪，也不知道怎么复现；
/// 这条日志的全部价值就是把人直接送到原因上。所以文案固定包含：
/// 原因、日志路径、以及**终端里怎么复现**。
///
/// 与 panic hook 的 `panic.log` **分开一个文件**：那里是 panic（带 backtrace），
/// 这里是 `Builder::build` 返回 Err —— 两者成因不同，混在一起会互相误导。
fn startup_failure_text(log_path: &std::path::Path, reason: &str) -> String {
    format!(
        "XrayTun 启动失败：窗口/运行时没能建立，进程以退出码 1 结束（不是 SIGABRT）。\n\
         原因：{reason}\n\
         下一步：\n\
         ① 把下面这个文件发给开发者 —— 它就是这次失败的原因：\n     {}\n\
         ② 想立刻看到完整输出，在「终端」里执行（把同样的原因直接打出来）：\n     \
         /Applications/XrayTun.app/Contents/MacOS/xraytun-desktop\n",
        log_path.display()
    )
}

/// 把启动失败写进 `dir/startup.log`，返回实际写入的路径。
///
/// 覆盖写而不是追加：一个进程只可能「启动失败」一次，保留最新原因即可。
fn write_startup_failure(
    dir: &std::path::Path,
    reason: &str,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(STARTUP_LOG_NAME);
    std::fs::write(&path, startup_failure_text(&path, reason))?;
    Ok(path)
}

/// 应用入口。`main.rs` 只有一行，真正的逻辑都在这里，方便将来加集成测试。
pub fn run() {
    // **第一件事**：装上 panic hook。晚一步都可能错过启动阶段的崩溃。
    install_panic_hook();
    init_tracing();

    let app = match tauri::Builder::default()
        .plugin(tauri_plugin_noop())
        .setup(|app| {
            let store = xt_core::store::Store::with_default_root();
            let handle = app.handle().clone();
            app.manage(AppState::new(store));

            // 托盘是可选能力：构建失败不应该让整个 App 起不来。
            if let Err(e) = tray::build(&handle) {
                tracing::warn!(error = %e, "菜单栏图标创建失败，功能不受影响");
            }

            // 意图过滤：按当前设置（重建）运行态。
            //
            // **这一步不碰核心、不碰系统网络配置** —— 它只是把判定引擎建起来，
            // 让核心日志里的连接记录有人接手。规则下发给核心是下一步。
            if let Some(state) = handle.try_state::<AppState>() {
                let now = xt_core::util::now_unix();
                let settings = state.with(|i| i.settings.clone());
                if let Some(settings) = settings {
                    let notes = state
                        .with(|i| i.intent.follow_settings(&settings, now))
                        .unwrap_or_default();
                    for note in notes {
                        state.log("intent", "info", note);
                    }
                }
                // 判定节拍：与看门狗一样 10 秒一跳。没有引擎时它什么都不做。
                let tick_handle = handle.clone();
                // ⚠️ 必须走 `tauri::async_runtime::spawn`：Tauri 的 `setup` 回调**不在
                // tokio runtime context 里**，`tokio::spawn` 会直接 panic
                // （`there is no reactor running, must be called from the context of a
                // Tokio 1.x runtime`）。release 里 panic=abort ⇒ 整个 App SIGABRT，
                // 被自动拉起后再次 panic，用户侧表现就是「图标一直在跳、打不开」。
                // 同文件下方 bootstrap 用的 `tauri::async_runtime::spawn` 才是正确写法。
                tauri::async_runtime::spawn(async move {
                    let mut ticker = tokio::time::interval(crate::intent::TICK_INTERVAL);
                    // 第一跳立刻发生（interval 的默认行为）—— 跳过它，避免刚启动就白跑一轮。
                    ticker.tick().await;
                    loop {
                        ticker.tick().await;
                        if let Some(state) = tick_handle.try_state::<AppState>() {
                            state.tick_intent(xt_core::util::now_unix());
                        }
                    }
                });
            }

            // 启动时做一次「健康检查 + 遗留清理」，并把结果写进日志与提示条。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                bootstrap(handle).await;
            });

            // 客户端版本自动检测（task-188）：**排在 `bootstrap` 之后**，
            // 并且自己再延迟 20 秒才联网（`version_check::INITIAL_DELAY`）——
            // 它最不急，不该和「找核心 / 探 helper / 回滚遗留 / 探 DNS」抢启动窗口与网络。
            // 之后每 6 小时复查一次；用户不点开设置也能知道有新版本。
            crate::version_check::watch(app.handle().clone());

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
            commands::intent_status,
            commands::intent_allow,
            commands::intent_clear_cache,
            commands::intent_audit,
            commands::intent_explain,
            commands::intent_apply,
            commands::mitm_status,
            commands::mitm_ca_install,
            commands::mitm_ca_remove,
            commands::mitm_apply,
            commands::routing_topology,
            commands::explain_dest,
            commands::recent_connections,
            commands::globe_data,
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
            commands::check_app_update,
            commands::install_app_update,
            commands::probe_dns,
            commands::incident_preview,
            commands::incident_upload,
            commands::incident_anomaly_count,
        ])
        .build(tauri::generate_context!())
    {
        // **绝不 `.expect(...)`。** `Builder::build` 返回 Err 是**真实存在**的路径：
        // Tauri 2 的 `setup` 闭包返回 Err 会被包成 `Error::Setup` 从这里冒出来
        // （tauri-2.11.5 `src/app.rs:2530-2531`），而 release profile 是
        // `panic = "abort"` ⇒ `.expect` 把它变成 SIGABRT：用户只看到 `abort() called`，
        // 拿不到任何可读信息 —— 0.8.38 的现场就是这个形态。
        // 现在：把原因落盘（与 panic hook 同一个 `logs/` 目录），以**非零退出码**结束。
        Ok(app) => app,
        Err(e) => {
            let reason = e.to_string();
            let dir = xt_core::store::Store::default_root().join("logs");
            match write_startup_failure(&dir, &reason) {
                Ok(path) => {
                    eprintln!("XrayTun 启动失败：{reason}\n（详情已写入 {}）", path.display());
                }
                Err(write_err) => {
                    eprintln!("XrayTun 启动失败：{reason}\n（写日志也失败：{write_err}）");
                }
            }
            std::process::exit(1);
        }
    };

    app.run(|app, event| {
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

        // task-183：这行日志与 UI 用**同一判据**（`helper_startup_log`）。
        // 「没看到 socket」**不等于**「没装」—— socket 是守护进程启动时 bind、
        // 退出时删除的（`crates/xt-helper/src/server.rs:114`、`:120-125`），
        // 所以「装了但没跑」不许再被写成「尚未安装」。
        if let Some((level, msg)) = crate::helper_client::helper_startup_log(&snapshot.helper) {
            i.push_log("app", level, msg);
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

    // 兜底自查：**隧道已经不在，但系统 DNS 还指着隧道内的哨兵地址** ——
    // 这个组合等于「所有域名都解析不了」，用户看到的就是「断网」。
    //
    // 放在回滚**之后**：回滚成功的话这里就查不到了。会走到这里的路径有几条，
    // 重启是最容易撞上的那条 —— 内核把路由和 DNS 重置了，而磁盘上的快照还在、
    // helper 进程也重启了（内存里的会话没了），两边对不上，回滚就无从下手。
    // 崩溃、手工 kill helper、上一版留下的状态同理。
    //
    // 这里只**发现并说清楚**，不自己动手：改系统 DNS 需要 root，得走 helper，
    // 而「在不确定的情况下自动改用户的 DNS」比不改更危险。把命令原样给出来，
    // 用户一条粘贴就能修。
    if !state.with(|i| i.runtime.running).unwrap_or(false) {
        let sentinel = state
            .with(|i| i.settings.tun.sentinel_dns.trim().to_string())
            .unwrap_or_default();
        if let Some(service) = sentinel_dns_without_tunnel(&sentinel) {
            let msg = format!(
                "系统 DNS 还指着隧道内的哨兵地址 {sentinel}（网络服务「{service}」），\
                 但隧道已经不在了，所有域名都解析不了。点「修复网络」，或执行：\
                 sudo networksetup -setdnsservers '{service}' Empty"
            );
            state.with(|i| {
                i.push_log("app", "error", msg.clone());
                i.last_notice = Some(msg);
            });
        }
    }

    // 上次是连着的话就连回来。
    //
    // 必须放在**遗留回滚之后**：先确保 helper 那边的旧会话清干净了，
    // 再建新的，否则会撞上「已有活跃会话」。
    //
    // **后台跑，不能 await**：它会重试最多约 2 分钟（开机时网络还没就绪），
    // 在这里 await 的话窗口要等两分钟才出来。
    {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = handle.try_state::<AppState>() {
                crate::commands::reconnect_if_needed(&handle, &state).await;
            }
        });
    }

    // 启动时在后台探一次 DNS，把最快的排到前面。
    //
    // **不阻塞启动**：探测要联网、约 10 秒。启动流程里已经有「找核心 / 探 helper /
    // 回滚遗留」三件事，再加一件同步的联网操作会让窗口迟迟不出来。
    // 结果会在下一次连接时生效（配置是那时生成的）。
    if state.with(|i| i.settings.dns.auto_select).unwrap_or(false) {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = handle.try_state::<AppState>() {
                if let Err(e) = crate::commands::run_dns_probe_bg(&handle, &state).await {
                    tracing::warn!(error = %e, "启动时探测 DNS 失败");
                }
                crate::events::runtime_changed(&handle, &state);
            }
        });
    }

    // 必须发**完整载荷**。这里曾经是 `app.emit(RUNTIME_CHANGED, ())`，
    // 而前端的处理函数会直接读 `payload.runtime` / `payload.traffic` ——
    // 于是这一发在 webview 里抛 TypeError，运行时与流量都拿不到更新。
    events::runtime_changed(&app, &state);
}

/// 找到第一个把 DNS 设成**哨兵地址**的网络服务。
///
/// 单独抽出来是为了让上面那段兜底逻辑读起来只剩「发现 + 报告」这一件事。
/// 纯读操作：`networksetup -getdnsservers` 不需要管理员权限。
fn sentinel_dns_without_tunnel(sentinel: &str) -> Option<String> {
    if sentinel.is_empty() {
        return None;
    }
    for service in xt_tun::macos::dns::list_services().ok()? {
        if let Ok(servers) = xt_tun::macos::dns::get_dns(&service) {
            if servers.iter().any(|s| s == sentinel) {
                return Some(service);
            }
        }
    }
    None
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

    // ---- task-1：生产路径去 panic（`panic = "abort"` ⇒ 任何 panic 都是 SIGABRT）----

    /// 生产源码 = `#[cfg(test)] mod tests` 之前的部分（仓库既有锚点约定，
    /// 见 `supervisor.rs::core_shutdown_result_is_not_swallowed_in_production_source`）。
    fn production_prefix(src: &str) -> &str {
        src.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(src)
    }

    /// `Builder::build` 返回 Err 时**不许** panic，必须落盘后以非零码退出。
    ///
    /// 0.8.38 的现场：`.build(...).expect("Tauri 应用启动失败")` +
    /// `[profile.release] panic = "abort"` ⇒ 双击启动即 `abort() called`，
    /// 用户与我们都拿不到原因。这条测试钉住那个 `.expect` 不被写回来。
    #[test]
    fn build_failure_is_reported_not_panicked() {
        let prod = production_prefix(include_str!("lib.rs"));
        assert!(
            !prod.contains(".expect(\"Tauri 应用启动失败\")"),
            "`build(...).expect(...)` 会把启动失败变成 SIGABRT（0.8.38 事故）；\
             必须改成 match + 落盘 + exit(1)"
        );
        assert!(
            prod.contains("fn write_startup_failure"),
            "启动失败必须落盘 —— 否则用户手上仍然没有任何证据"
        );
        assert!(
            prod.contains("std::process::exit(1)"),
            "启动失败必须走非零退出码（可预期），而不是 abort（不可读）"
        );
    }

    /// `setup` 闭包**任何失败都不得返回 Err**：Tauri 会把它包成 `Error::Setup`
    /// 让 `build()` 返回 Err（tauri-2.11.5 `src/app.rs:2530-2531`），
    /// 于是启动路径直接失败。失败只允许「记日志后继续」。
    #[test]
    fn setup_closure_never_returns_err() {
        let prod = production_prefix(include_str!("lib.rs"));
        let start = prod.find(".setup(|app| {").expect("lib.rs 里的 setup 闭包不见了");
        let rest = &prod[start..];
        let end = rest
            .find(".on_window_event(")
            .expect("setup 之后的 on_window_event 不见了 —— 夹具锚点失效");
        let setup = &rest[..end];
        assert!(setup.contains("Ok(())"), "setup 必须显式以 Ok(()) 结束：{setup}");
        assert!(
            !setup.contains('?'),
            "setup 里不许用 `?` 把 Err 冒泡出去 —— Tauri 会 panic 成 SIGABRT"
        );
        assert!(
            !setup.contains("return Err("),
            "setup 不许返回 Err —— 启动路径上任何失败都要记日志后继续"
        );
    }

    /// 启动失败文案必须包含「下一步做什么」：原因 + 日志路径 + 终端复现命令。
    /// 只写一句「启动失败」，用户拿到也没用。
    #[test]
    fn startup_failure_text_tells_the_user_where_to_look() {
        let path = std::path::Path::new("/tmp/演示/startup.log");
        let text = super::startup_failure_text(path, "模拟原因：WebView 初始化失败");
        assert!(text.contains("模拟原因：WebView 初始化失败"), "必须原样带上原因：{text}");
        assert!(text.contains("/tmp/演示/startup.log"), "必须给出日志文件路径：{text}");
        assert!(text.contains("下一步"), "必须写「下一步做什么」：{text}");
        assert!(
            text.contains("xraytun-desktop"),
            "必须给出终端复现命令（否则用户无法自助）：{text}"
        );
    }

    /// 落盘必须真的写出文件，且内容里带上原因（不是只在内存里 format 一下）。
    #[test]
    fn startup_failure_is_written_to_disk_with_the_cause() {
        let dir = std::env::temp_dir().join(format!("xt-startup-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = super::write_startup_failure(&dir, "模拟原因：核心上下文损坏")
            .expect("启动失败日志必须能写出来（目录会自建）");
        assert_eq!(path, dir.join(super::STARTUP_LOG_NAME));
        let written = std::fs::read_to_string(&path).expect("日志文件必须存在");
        assert!(written.contains("模拟原因：核心上下文损坏"), "落盘内容必须含原因：{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归收集 `apps/desktop/src` 下的 `.rs` 文件（守卫要扫全 crate，不只是 lib.rs）。
    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// 去掉注释（`//` 行注释与**可嵌套的** `/* */` 块注释），**保留换行** ⇒ 行号不变。
    ///
    /// # 为什么不能只截 `//`（tester 在 task-14 独立复验里指出的假红）
    ///
    /// 判据是「生产代码里有没有某个调用」，所以注释里的 `tokio::spawn(` / `panic!(` /
    /// `#[cfg(test)]` 都不该算数。旧实现只按 `//` 截断 ⇒ **块注释里的调用会假红**，
    /// 而块注释里的 `#[cfg(test)]` 还会让「跳过测试项」的判定错位。
    ///
    /// 顺带跳过字符串字面量：否则 `"http://…"` 会被当成注释起始，把它后面的代码吞掉
    /// （那种吞法会**假绿**，比假红更危险）。
    ///
    /// 已知边界：不做字符字面量 `'…'` 解析（`'"'` 这种会让状态机误入字符串）。
    /// 本仓库源码里没有这种写法；若将来出现，加一条用例即可暴露。
    fn strip_comments(src: &str) -> String {
        #[derive(PartialEq)]
        enum S {
            Code,
            Line,
            Block(usize),
            Str,
        }
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars().peekable();
        let mut state = S::Code;
        while let Some(c) = chars.next() {
            match state {
                S::Code => {
                    if c == '/' && chars.peek() == Some(&'/') {
                        chars.next();
                        state = S::Line;
                    } else if c == '/' && chars.peek() == Some(&'*') {
                        chars.next();
                        state = S::Block(1);
                    } else if c == '"' {
                        out.push(c);
                        state = S::Str;
                    } else {
                        out.push(c);
                    }
                }
                S::Line => {
                    if c == '\n' {
                        out.push('\n');
                        state = S::Code;
                    }
                }
                S::Block(depth) => {
                    if c == '\n' {
                        out.push('\n'); // 保留行号
                    } else if c == '/' && chars.peek() == Some(&'*') {
                        chars.next();
                        state = S::Block(depth + 1); // Rust 块注释可嵌套
                    } else if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        state = if depth == 1 { S::Code } else { S::Block(depth - 1) };
                    }
                }
                S::Str => {
                    out.push(c);
                    if c == '\\' {
                        if let Some(n) = chars.next() {
                            out.push(n);
                        }
                    } else if c == '"' || c == '\n' {
                        state = S::Code;
                    }
                }
            }
        }
        out
    }

    /// 命中 panic 家族调用则返回名字。
    ///
    /// **明确允许** `unwrap_or_else` / `unwrap_or` / `unwrap_or_default` /
    /// `unwrap_or_else(|e| e.into_inner())` 这类**不 panic 的降级写法** ——
    /// 它们正是本卡要求的修复方向，不是违规。
    fn panic_family_call(line: &str) -> Option<&'static str> {
        for (needle, name) in [
            (".unwrap()", "unwrap()"),
            (".unwrap_unchecked()", "unwrap_unchecked()"),
            (".expect(", "expect("),
            ("panic!", "panic!"),
            ("unreachable!", "unreachable!"),
            ("todo!", "todo!"),
            ("unimplemented!", "unimplemented!"),
            ("std::process::abort", "std::process::abort"),
        ] {
            if line.contains(needle) {
                return Some(name);
            }
        }
        None
    }

    /// 返回「生产行」的 (1-based 行号, 去注释后的内容)，跳过所有 `#[cfg(test)]` 项。
    ///
    /// **不能**只按第一个 `#[cfg(test)]` 截断：`version_check.rs:221`、
    /// `supervisor.rs:377`、`commands/incident.rs:30` 这类测试专用项夹在生产代码中间，
    /// 截断会把它们后面的生产代码一起漏掉（`supervisor.rs` 的守卫注释里记着这个假绿）。
    fn production_lines(src: &str) -> Vec<(usize, String)> {
        // 先去注释（保留换行 ⇒ 行号不变）：注释里的调用/`#[cfg(test)]` 都不算数。
        let code = strip_comments(src);
        let lines: Vec<&str> = code.lines().collect();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < lines.len() {
            let bare = lines[i].trim().to_string();
            if bare.contains("#[cfg(test)]") {
                let indent = lines[i].len() - lines[i].trim_start().len();
                // 测试项的第一行（跳过空行/纯注释行）
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j >= lines.len() {
                    break;
                }
                let first = lines[j].trim();
                if first.contains(';') && !first.contains('{') {
                    i = j + 1; // `#[cfg(test)] use …;` 这种没有花括号的项
                    continue;
                }
                if first.contains('{') && first.contains('}') {
                    i = j + 1; // 单行项
                    continue;
                }
                // 多行项：跳到同缩进的收尾 `}`
                let mut k = j;
                while k < lines.len() {
                    let ind = lines[k].len() - lines[k].trim_start().len();
                    if lines[k].trim() == "}" && ind == indent {
                        break;
                    }
                    k += 1;
                }
                i = k + 1;
                continue;
            }
            out.push((i + 1, bare));
            i += 1;
        }
        out
    }

    /// **task-1 的机器判据**：`apps/desktop/src/**` 的生产代码里不许有 panic 家族调用。
    ///
    /// 事故背景：release profile 是 `panic = "abort"` ⇒ 任何一个 `unwrap()`/`expect()`/
    /// `panic!` 都是 **SIGABRT**，用户只看到 `abort() called`。「生产不该 panic」
    /// 因此必须是可复算的规则，而不是一句口号。
    ///
    /// 已知盲区（如实记在 `docs/verification/PANIC-POLICY.md`）：切片/索引越界
    /// 不在这条判据里 —— 那是另一类，用 `str::get`/`slice::get` 的写法兜。
    #[test]
    fn production_source_has_no_panic_family_calls() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        assert!(
            files.len() >= 20,
            "没扫到源文件（根 = {}，只看到 {} 个）—— 守卫不能空转",
            root.display(),
            files.len()
        );
        let mut scanned = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for file in &files {
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            for (n, line) in production_lines(&src) {
                scanned += 1;
                if let Some(what) = panic_family_call(&line) {
                    bad.push(format!("{}:{n}: {what} ⇒ {line}", file.display()));
                }
            }
        }
        assert!(
            scanned > 5_000,
            "判据只扫到 {scanned} 行生产代码 —— 可能被 `#[cfg(test)]` 切没了（假绿）"
        );
        assert!(
            bad.is_empty(),
            "生产代码里不许有 panic 家族调用（release 下 panic = abort ⇒ SIGABRT）：\n{}",
            bad.join("\n")
        );
    }

    /// 负例：把历史上真实存在的写法注入生产段，判据必须抓住（否则是假绿）。
    #[test]
    fn panic_family_guard_catches_a_planted_unwrap() {
        let src = include_str!("lib.rs");
        let mut lines: Vec<String> = src.lines().map(str::to_string).collect();
        let at = lines
            .iter()
            .position(|l| l.contains("fn init_tracing"))
            .expect("夹具锚点 `fn init_tracing` 不见了");
        lines.insert(at, "    let _ = std::env::var(\"XRAYTUN_X\").unwrap();".to_string());
        let planted = lines.join("\n");
        let hits: Vec<String> = production_lines(&planted)
            .into_iter()
            .filter_map(|(n, l)| panic_family_call(&l).map(|w| format!("{n}: {w}")))
            .collect();
        assert_eq!(hits.len(), 1, "注入的 `.unwrap()` 必须被抓到：{hits:?}");
    }

    /// 判据不许误伤：`unwrap_or_else` 这类**不 panic** 的降级写法是修复方向，必须放行。
    #[test]
    fn panic_family_guard_allows_non_panicking_fallbacks() {
        for ok in [
            "let x = a.unwrap_or_else(|| 1);",
            "let x = a.unwrap_or_default();",
            "let x = a.unwrap_or(\"\");",
            "let mut g = LOSS_NOTIFY.lock().unwrap_or_else(|e| e.into_inner());",
            "if let Some(v) = opt { }",
        ] {
            assert!(
                panic_family_call(ok).is_none(),
                "`{ok}` 不该被 panic 判据抓住（它是降级写法，不是违规）"
            );
        }
        assert_eq!(panic_family_call("let x = a.expect(\"boom\");"), Some("expect("));
        assert_eq!(panic_family_call("panic!(\"boom\")"), Some("panic!"));
        assert_eq!(
            panic_family_call("(Some(_), Some(_)) => unreachable!(\"x\"),"),
            Some("unreachable!")
        );
    }

    /// 注释与字符串里的「调用」不算数（tester 在 task-14 独立复验里指出的**假红**）。
    ///
    /// 判别性：
    /// * 旧实现只截 `//` ⇒ 块注释里的 `tokio::spawn(` / `panic!()` 会被判据抓住 ⇒ 本测试红；
    /// * 字符串里的 `//` 若被当成注释起始，同一行后面的真实 `panic!()` 会被吞掉 ⇒
    ///   下面 `let g = "http://x"; let h = panic!();` 这条会**少抓一个** ⇒ 同样红。
    #[test]
    fn comments_and_strings_do_not_trip_or_hide_the_guards() {
        let src = "\
// 行注释：tokio::spawn( 与 panic!() 都不算
let a = 1; // 行尾注释：.unwrap()
/* 单行块注释：tokio::spawn( 与 #[cfg(test)] 都不算 */
let b = 2;
/* 多行块注释
   tokio::spawn(async move {
   panic!(\"nope\")
   */
let c = 3;
/* 嵌套：/* tokio::spawn( */ 仍然在注释里 */
let d = 4;
let f = panic!(); // 这一行必须被抓到
let g = \"http://example.com\"; let h = panic!(); // 字符串里的 // 不许吞掉后面的 panic!
";
        let lines = production_lines(src);
        let spawn_hits: Vec<_> = lines
            .iter()
            .filter(|(_, l)| l.contains("tokio::spawn("))
            .collect();
        assert!(
            spawn_hits.is_empty(),
            "注释里的 tokio::spawn( 不许被算成生产调用：{spawn_hits:?}"
        );
        assert_eq!(
            naked_tokio_spawn_sites(src).len(),
            0,
            "行/块/嵌套注释里的 tokio::spawn( 都不算"
        );
        let panic_hits: Vec<_> = lines
            .iter()
            .filter(|(_, l)| panic_family_call(l).is_some())
            .collect();
        assert_eq!(
            panic_hits.len(),
            2,
            "只该抓到两行真实的 panic!()（f 与 h）：{panic_hits:?}"
        );
        assert!(
            panic_hits.iter().any(|(_, l)| l.contains("let h = panic!()")),
            "同一行字符串里的 `//` 不许把后面的 panic!() 吞掉：{panic_hits:?}"
        );
    }

    // ---- task-14：启动路径不许裸 `tokio::spawn`（0.8.39 双击 SIGABRT 的真实根因）----

    /// 扫出生产代码里的裸 `tokio::spawn(` 调用（1-based 行号 + 去注释后的内容）。
    ///
    /// 只认 `tokio::spawn(`：`tokio::task::spawn_blocking(` 与 `JoinSet::spawn`
    /// 是另外的 API，且全部写在 `async fn` 体内（那里 runtime context 一定存在），
    /// 不在本判据范围 —— 本判据针对的是「同步上下文里裸 spawn」这一类。
    fn naked_tokio_spawn_sites(src: &str) -> Vec<(usize, String)> {
        production_lines(src)
            .into_iter()
            .filter(|(_, l)| l.contains("tokio::spawn("))
            .collect()
    }

    /// **task-14 的机器判据**：生产代码里**一个裸 `tokio::spawn` 都不许有**。
    ///
    /// # 为什么原来抓不到（0.8.39 的事故）
    ///
    /// Tauri 的 `setup` 回调**不在 tokio runtime context 里**，裸 `tokio::spawn`
    /// 会 panic：`there is no reactor running, must be called from the context of a
    /// Tokio 1.x runtime`；release profile 是 `panic = "abort"` ⇒ **双击即 SIGABRT**。
    /// 而 `scripts/check.sh` 全绿时这个 panic 依然在：clippy / `cargo test --workspace` /
    /// release 构建**没有任何一步会启动 App** ——「编译得过 + 单测全绿」与
    /// 「一启动就 abort」可以同时成立（真机 panic.log：`lib.rs:178:17`）。
    ///
    /// 这是**运行时语义**错误、不是 panic 家族调用，所以
    /// `production_source_has_no_panic_family_calls` 抓不到它（`tokio::spawn`
    /// 文本上没有 panic 特征），必须单独一条。
    ///
    /// 判据的形状取「统一走 `tauri::async_runtime::spawn`」：它在同步与异步上下文
    /// 里都能调，因此规则不依赖调用方是不是 `async`（`traffic::spawn`、
    /// `commands/globe.rs` 的并发查询、`core.rs` 的日志转发都已统一）。
    #[test]
    fn production_never_calls_naked_tokio_spawn() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        let mut scanned = 0usize;
        let mut tauri_spawns = 0usize;
        let mut bad: Vec<String> = Vec::new();
        for file in &files {
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            let lines = production_lines(&src);
            scanned += lines.len();
            tauri_spawns += lines
                .iter()
                .filter(|(_, l)| l.contains("tauri::async_runtime::spawn"))
                .count();
            for (n, line) in lines {
                if line.contains("tokio::spawn(") {
                    bad.push(format!("{}:{n}: {line}", file.display()));
                }
            }
        }
        assert!(
            scanned > 5_000,
            "判据只扫到 {scanned} 行生产代码 —— 可能被 `#[cfg(test)]` 切没了（假绿）"
        );
        assert!(
            tauri_spawns > 0,
            "一个 `tauri::async_runtime::spawn` 都没看到 —— 夹具锚点失效，判据在空转"
        );
        assert!(
            bad.is_empty(),
            "生产代码里不许有裸 `tokio::spawn`：同步上下文（Tauri `setup` 回调）会 panic \
             `there is no reactor running…` ⇒ release 下 SIGABRT ⇒ 双击即崩。\
             统一改用 `tauri::async_runtime::spawn`：\n{}",
            bad.join("\n")
        );
    }

    /// 负例：把 0.8.38 那行原样放回夹具，判据必须变红。
    ///
    /// 判别性：把 setup 里的 `tauri::async_runtime::spawn` 换回 `tokio::spawn`
    /// （0.8.38 与 task-1 提交 `eef3d10` 里的真实写法）⇒ 命中数 1 ⇒ 这条测试红。
    #[test]
    fn naked_tokio_spawn_guard_catches_the_0_8_38_pattern() {
        let src = include_str!("lib.rs");
        assert!(
            naked_tokio_spawn_sites(src).is_empty(),
            "当前源码不该有裸 `tokio::spawn`"
        );
        let mutated = src.replacen(
            "tauri::async_runtime::spawn(async move {",
            "tokio::spawn(async move {",
            1,
        );
        assert_ne!(mutated, src, "夹具必须真的改到源码（锚点不见了就是空改）");
        let hits = naked_tokio_spawn_sites(&mutated);
        assert_eq!(
            hits.len(),
            1,
            "0.8.38 的裸 `tokio::spawn` 必须被抓到（否则判据是假绿）：{hits:?}"
        );
    }
}
