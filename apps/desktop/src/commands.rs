//! Tauri 命令层：UI 能调用的全部入口。
//!
//! # 约定
//!
//! * 所有命令返回 `Result<T, String>`。`String` 是**给用户看的**中文消息，
//!   不是调试信息 —— 上游错误在 `map_err` 里已经被翻译成人话。
//! * 每个会改变状态或系统的命令，成功后都返回最新的 [`AppSnapshot`]，
//!   让 UI 一次拿全，避免「调完再查一次」产生的竞态和额外 IPC。
//! * **绝不跨 `.await` 持有 `inner` 锁**。耗时动作（起进程、改网络、跑探针）
//!   都在锁外做，做完再短暂加锁写回结果。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

use xt_core::model::{AppSettings, Node, ProxyMode, Subscription};
use xt_core::xray;
use xt_proto::{Request, DEFAULT_SOCKET_PATH};

use crate::events;
use crate::state::{
    persist_settings, AppSnapshot, CoreAvailability, CoreRuntime, HelperAvailability,
};
use crate::AppState;

/// 状态锁不可用时的统一提示。
///
/// 抽成常量而不是散落的字面量：这句话就是用户看到的全部信息，
/// 改一次要能全局生效，而不是漏掉某几处导致同一个故障有两种说法。
const STATE_UNAVAILABLE: &str = "应用状态不可用";

/// 把领域错误翻成给用户看的一句话。
///
/// `xt_core::Error` 的 `Display` 本来就是中文人话（见 `crates/xt-core/src/error.rs`），
/// 命令层要做的只是脱掉错误类型、留下那句话。这里统一收口，
/// 避免 16 处各写一遍 `map_err(user_msg)`。
fn user_msg<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// 快照
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn snapshot(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    build_snapshot(&app, &state).await
}

async fn build_snapshot(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let helper_socket = DEFAULT_SOCKET_PATH;
    let socket_present = crate::helper_client::socket_present(std::path::Path::new(helper_socket));

    let mut guard = state.helper.lock().await;
    let helper = guard.availability(socket_present);
    drop(guard);

    // ⚠️ **这几项必须在 `state.with` 闭包外面算完。**
    //
    // `AppState::with` 用的是 `std::sync::Mutex`（非递归）。在已经持有锁的
    // 闭包里再调用 `state.with(...)` 会**自死锁** —— 而症状是「进程活着、
    // 但界面什么都加载不出来」，没有任何报错。这个坑真的踩了：
    // `update_status` 内部要拿锁，却被写在了快照闭包里面。
    //
    // 顺带这也是正确的做法：这几项要做文件 IO、甚至起进程问核心版本，
    // 不该握着状态锁去做。
    let core = core_availability(app, state);
    let login_item = login_item_state();
    let update = update_status(app, state);
    let dns = state.with(|i| i.dns.clone()).unwrap_or_default();
    let app_version = app.package_info().version.to_string();

    state
        .with(|inner| AppSnapshot {
            settings: inner.settings.clone(),
            subscriptions: inner.subscriptions.clone(),
            nodes: inner.nodes.clone(),
            runtime: inner.runtime.clone(),
            latency: inner.latencies.clone(),
            traffic: inner.traffic.clone(),
            notice: inner.last_notice.clone(),
            helper,
            core,
            login_item,
            update,
            dns,
            app_version,
        })
        .ok_or_else(|| "应用状态不可用".to_string())
}

/// 探测 DNS 解析器，并按需把最快的排到前面。
///
/// 两组走**两条不同的路径**，因为它们在配置里的用法本来就不同：
///
/// * **国内组**（`geosite:cn` → `direct_servers`）绑物理网卡直连测。
///   不绑的话查询会经 TUN → 核心的 gVisor 栈 → 解析器，并发时测到的是核心
///   排队：实测同一台阿里 DNS，绑 en0 是 32ms，不绑（并发 6）是 155ms。
/// * **国外组**（`geosite:geolocation-!cn` → `remote_servers`）经本地 SOCKS
///   入站测（也就是经节点）。直连根本连不上国外 DoH —— 实测 8 秒超时。
async fn run_dns_probe(app: &AppHandle, state: &AppState, apply: bool) -> Result<(), String> {
    let interface = xt_tun::macos::route::default_route()
        .ok()
        .map(|r| r.interface);

    // 核心没跑时传 `None`：探测会把国外组标成「未探测」，而不是硬走直连
    // 测一遍、再把必然的超时报成「这台解析器不通」。
    let (running, socks_port) = state
        .with(|i| (i.runtime.running, i.settings.socks_port))
        .unwrap_or((false, 10808));
    let socks = running.then(|| format!("127.0.0.1:{socks_port}"));

    let spec = xt_core::dns_probe::ProbeSpec {
        timeout: std::time::Duration::from_secs(2),
        samples: 3,
        interface,
        socks,
    };

    let pool = xt_core::dns_probe::DNS_POOL;
    let probes = xt_core::dns_probe::probe_pool(pool, &spec, 4).await;

    let usable = |kind: xt_core::dns_probe::DnsKind| -> Vec<String> {
        probes
            .iter()
            .filter(|p| p.usable() && p.kind == kind)
            .map(|p| p.server.clone())
            .collect()
    };
    let domestic = usable(xt_core::dns_probe::DnsKind::Domestic);
    let foreign = usable(xt_core::dns_probe::DnsKind::Foreign);
    let chosen = domestic.first().cloned();
    let chosen_foreign = foreign.first().cloned();

    let foreign_error = if spec.socks.is_none() {
        Some("节点未连接，国外解析器未探测".to_string())
    } else if foreign.is_empty() {
        Some("没有任何国外解析器可用".to_string())
    } else {
        None
    };

    state.with(|i| {
        i.dns.probes = probes.clone();
        i.dns.chosen = chosen.clone();
        i.dns.chosen_foreign = chosen_foreign.clone();
        i.dns.probed_at = Some(crate::state::now_unix());
        i.dns.error = if domestic.is_empty() {
            Some("没有任何国内解析器可用".into())
        } else {
            None
        };
        i.dns.foreign_error = foreign_error.clone();
    });

    if apply {
        let mut settings = state.with(|i| i.settings.clone()).ok_or(STATE_UNAVAILABLE)?;
        if settings.dns.auto_select {
            let mut changes = Vec::new();

            if !domestic.is_empty() {
                let next = merge_ranked(
                    &settings.dns.direct_servers,
                    &domestic,
                    xt_core::dns_probe::DnsKind::Domestic,
                    pool,
                );
                if next != settings.dns.direct_servers {
                    settings.dns.direct_servers = next;
                    changes.push(format!("国内首选 {}", chosen.clone().unwrap_or_default()));
                }
            }
            if !foreign.is_empty() {
                let next = merge_ranked(
                    &settings.dns.remote_servers,
                    &foreign,
                    xt_core::dns_probe::DnsKind::Foreign,
                    pool,
                );
                if next != settings.dns.remote_servers {
                    settings.dns.remote_servers = next;
                    changes.push(format!("国外首选 {}", chosen_foreign.clone().unwrap_or_default()));
                }
            }

            if !changes.is_empty() {
                persist_settings(state, &settings)?;
                state.with(|i| {
                    i.push_log(
                        "app",
                        "info",
                        format!("DNS 已自动选优：{}", changes.join("，")),
                    )
                });
            }
        }
    }

    let _ = app;
    Ok(())
}

/// 把探测出来的排序放到前面，用户手填的（不在候选池里的）留在后面。
///
/// 自动排序**只调池内项的相对顺序**，不删用户自己加的东西 —— 那些是用户
/// 明确想要的结果，探测器没资格替他把它们丢掉。
fn merge_ranked(
    current: &[String],
    ranked: &[String],
    kind: xt_core::dns_probe::DnsKind,
    pool: &[xt_core::dns_probe::DnsCandidate],
) -> Vec<String> {
    let in_pool =
        |s: &String| pool.iter().any(|c| c.server == s.as_str() && c.kind == kind);
    let mut out = ranked.to_vec();
    let extra: Vec<String> = current
        .iter()
        .filter(|s| !in_pool(s) && !out.contains(s))
        .cloned()
        .collect();
    out.extend(extra);
    out
}

/// 启动流程调用的包装：与手动探测同一条代码路径，只是不需要返回快照。
pub async fn run_dns_probe_bg(app: &AppHandle, state: &AppState) -> Result<(), String> {
    run_dns_probe(app, state, true).await
}

/// 手动触发一次 DNS 探测并应用结果。
#[tauri::command]
pub async fn probe_dns(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    run_dns_probe(&app, &state, true).await?;
    build_snapshot(&app, &state).await
}

/// 查客户端**自己**的最新版。
///
/// 仓库现在**是公开的**，所以匿名就能查；token 变成可选 —— 但填了能把
/// GitHub 的配额从 60 次/小时提到 5000，而那个配额是**按 IP** 算的，
/// 我们的请求又多是经节点出去的，等于和整台节点的用户共用。
///
/// 无论哪种情况，「拿不到更新」和「已经是最新」都必须分开报 ——
/// 这正是 `gh_status_error` 把 403/404 翻译成人话的原因。
#[tauri::command]
pub async fn check_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let proxy = state
        .with(|i| i.runtime.running.then_some(i.settings.socks_port))
        .ok_or(STATE_UNAVAILABLE)?;

    let result = tauri::async_runtime::spawn_blocking(move || xt_core::update::check_app(proxy))
    .await
    .map_err(|e| format!("检查任务失败：{e}"))?;

    state.with(|i| {
        i.update.checked_at = Some(crate::state::now_unix());
        match result {
            Ok(a) => {
                i.push_log("app", "info", format!("客户端最新版 {}", a.version));
                i.update.latest_app = Some(a);
                i.update.check_error = None;
            }
            Err(e) => {
                i.update.check_error = Some(e.to_string());
            }
        }
    });
    build_snapshot(&app, &state).await
}

/// 当前进程是不是从某个 `.app` 里跑起来的。是的话返回那个 bundle 的路径。
///
/// 开发构建（`cargo run`）不满足这个条件 —— 那种情况不自动更新，
/// 因为「替换掉自己正在跑的那个目录」在开发场景下只会让人困惑。
fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../XrayTun.app/Contents/MacOS/xraytun-desktop
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().and_then(|e| e.to_str()) == Some("app")).then(|| bundle.to_path_buf())
}

/// 读一个 `.app` 的版本号。
fn bundle_version(app: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 下载 + 校验 + 解开，返回暂存目录里那个 `.app`。
///
/// 三步都要做，缺一不可：
/// * **下载**：私有仓库的资产要带 token；
/// * **校验 sha256**：和 release 里的 `SHA256SUMS.txt` 比对。这只防「下载坏了/被截断」，
///   防不了「上游被换掉」—— 那需要签名，而这个包是 ad-hoc 签名的，没有可验的根。
///   这一点必须如实写进界面，不能假装验过了；
/// * **版本核对**：解开之后读一下包里的版本号，和 release 声称的一致才继续。
///   防止拿到一个名字对、内容错的包。
fn stage_app_update(
    latest: &xt_core::update::Available,
    proxy: Option<u16>,
    tmp: &std::path::Path,
    total: Option<u64>,
    mut on_progress: impl FnMut(u64),
) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(tmp);
    std::fs::create_dir_all(tmp).map_err(|e| format!("建暂存目录失败：{e}"))?;

    let zip_name = latest
        .download_url
        .rsplit('/')
        .next()
        .filter(|s| s.ends_with(".zip"))
        .ok_or("更新地址不是一个 zip")?
        .to_string();
    let zip = tmp.join(&zip_name);

    // 进度条的百分比要用 release 报的字节数当分母；上游没报就传 None，
    // 界面退化成「只显示已下载多少」而不是画一个假的百分比。
    let _ = total;
    xt_core::update::download_with_progress(&latest.download_url, &zip, proxy, &mut on_progress)
        .map_err(|e| format!("下载 {zip_name} 失败：{e}"))?;

    if let Some(url) = &latest.digest_url {
        let text = xt_core::update::fetch_text(url, proxy)
            .map_err(|e| format!("取校验和失败：{e}"))?;
        let want = xt_core::update::parse_sha256sum_for(&text, &zip_name)
            .ok_or_else(|| format!("校验文件里没有 {zip_name}"))?;
        let got = xt_core::update::sha256_file(&zip).map_err(user_msg)?;
        if want != got {
            return Err(format!("校验和不匹配：期望 {want}，实际 {got}"));
        }
    } else {
        return Err("这次 release 没有 SHA256SUMS.txt，拒绝安装".into());
    }

    let stage = tmp.join("stage");
    xt_core::update::unzip_tree(&zip, &stage).map_err(user_msg)?;

    let app = stage.join("XrayTun.app");
    if !app.is_dir() {
        return Err(format!("解压后没找到 {}", app.display()));
    }
    let got_version = bundle_version(&app).unwrap_or_default();
    if got_version != latest.version {
        return Err(format!(
            "包内版本 {got_version} 与 release 声称的 {} 不一致",
            latest.version
        ));
    }
    Ok(app)
}

/// 安装客户端更新：下载 → 校验 → 交给独立脚本替换 → 退出应用。
///
/// **不能自己替换正在运行的 .app**，所以最后一步是把脚本 detached 地跑起来，
/// 由它等我们退出再换、然后重新打开。见 `update::self_update_script`。
#[tauri::command]
pub async fn install_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let target = current_app_bundle()
        .ok_or("当前不是从 .app 里运行的（开发构建不支持自动更新）")?;
    let (proxy, latest) = state
        .with(|i| {
            (
                i.runtime.running.then_some(i.settings.socks_port),
                i.update.latest_app.clone(),
            )
        })
        .ok_or(STATE_UNAVAILABLE)?;
    let latest = latest.ok_or("还没有检查过客户端更新")?;

    let tmp = std::env::temp_dir().join(format!("xraytun-update-{}", std::process::id()));
    let tmp2 = tmp.clone();
    let latest2 = latest.clone();
    let total = latest.size;
    let reporter = progress_reporter(app.clone(), "客户端", latest.size);
    let staged = tauri::async_runtime::spawn_blocking(move || {
        stage_app_update(&latest2, proxy, &tmp2, total, reporter)
    })
    .await
    .map_err(|e| format!("更新任务失败：{e}"))?
    .map_err(|e| {
        state.with(|i| {
            i.update.progress = None;
            i.push_log("app", "error", format!("准备更新失败：{e}"));
        });
        e
    })?;

    // 把「等退出 → 替换 → 重启」写成脚本，脱离父子关系地跑起来。
    let script_path = tmp.join("apply-update.sh");
    let script = xt_core::update::self_update_script(std::process::id(), &staged, &target, &tmp);
    std::fs::write(&script_path, script).map_err(|e| format!("写更新脚本失败：{e}"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700));
    }
    std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("启动更新脚本失败：{e}"))?;

    state.with(|i| {
        i.push_log(
            "app",
            "info",
            format!("正在更新到 {}，应用即将退出并自动重启", latest.version),
        )
    });
    events::runtime_changed(&app, &state);

    // 让退出流程正常跑（回滚隧道、停核心），再结束进程。
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        handle.exit(0);
    });

    build_snapshot(&app, &state).await
}

/// 组装更新状态：当前生效的版本 + 上次检查的缓存。
fn update_status(app: &AppHandle, state: &AppState) -> crate::state::UpdateStatus {
    let root = state.store.root();
    let managed_dir = xt_core::update::managed_core_dir(root);
    let meta = xt_core::update::InstalledMeta::load(&managed_dir);
    let core_version = core_availability(app, state).version;
    let core_managed = managed_dir.join("xray").is_file();

    state
        .with(|i| {
            let mut u = i.update.clone();
            u.core_version = core_version;
            u.core_managed = core_managed;
            u.core_managed_version = meta.core_version;
            u.geo_tag = meta.geo_tag;
            u.geo_installed_at = meta.geo_installed_at;
            u
        })
        .unwrap_or_default()
}

/// 检查更新（联网，几秒）。
///
/// 走本地 SOCKS 入站：GitHub 在国内直连经常不通，而用户的节点通常是通的。
/// 核心没跑时退回直连，至少不会因为「没连接」而完全没法检查。
#[tauri::command]
pub async fn check_updates(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let proxy = {
        let running = state.with(|i| i.runtime.running).unwrap_or(false);
        running.then(|| state.with(|i| i.settings.socks_port).unwrap_or(10808))
    };
    let (core, geo) = tauri::async_runtime::spawn_blocking(move || {
        (xt_core::update::check_core(proxy), xt_core::update::check_geo(proxy))
    })
    .await
    .map_err(|e| format!("检查任务失败：{e}"))?;

    state.with(|i| {
        i.update.checked_at = Some(crate::state::now_unix());
        match core {
            Ok(a) => {
                i.update.latest_core = Some(a);
                i.update.check_error = None;
            }
            Err(e) => {
                i.push_log("app", "warn", format!("检查核心更新失败：{e}"));
                i.update.check_error = Some(e.to_string());
            }
        }
        if let Ok(a) = geo {
            i.update.latest_geo = Some(a);
        }
    });
    build_snapshot(&app, &state).await
}

/// 安装核心更新。
///
/// **不会自动重启核心**：换核心必然中断一次连接，用户应该在自己选的时候发生。
/// 装完提示他重新连接即可（下次启动就是用新核心）。
#[tauri::command]
pub async fn install_core_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let available = state
        .with(|i| i.update.latest_core.clone())
        .flatten()
        .ok_or("请先检查更新")?;
    let proxy = state.with(|i| i.settings.socks_port).unwrap_or(10808);
    let dir = xt_core::update::managed_core_dir(state.store.root());

    let reporter = progress_reporter(app.clone(), "核心", available.size);
    let meta = tauri::async_runtime::spawn_blocking(move || {
        xt_core::update::install_core(&available, &dir, Some(proxy), reporter)
    })
    .await
    .map_err(|e| format!("安装任务失败：{e}"))?
    .map_err(user_msg)?;

    state.with(|i| {
        i.update.progress = None;
        i.push_log(
            "app",
            "info",
            format!(
                "核心已更新到 {}（下次连接生效）",
                meta.core_version.clone().unwrap_or_default()
            ),
        );
        i.update.latest_core = None;
    });
    build_snapshot(&app, &state).await
}

/// 安装 geo 数据更新。
#[tauri::command]
pub async fn install_geo_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let available = state
        .with(|i| i.update.latest_geo.clone())
        .flatten()
        .ok_or("请先检查更新")?;
    let proxy = state.with(|i| i.settings.socks_port).unwrap_or(10808);
    let dir = xt_core::update::managed_core_dir(state.store.root());

    let reporter = progress_reporter(app.clone(), "geo 数据", available.size);
    let meta = tauri::async_runtime::spawn_blocking(move || {
        xt_core::update::install_geo(&available, &dir, Some(proxy), reporter)
    })
    .await
    .map_err(|e| format!("安装任务失败：{e}"))?
    .map_err(user_msg)?;

    state.with(|i| {
        i.push_log(
            "app",
            "info",
            format!("geo 数据已更新到 {}（下次连接生效）", meta.geo_tag.clone().unwrap_or_default()),
        );
        i.update.latest_geo = None;
    });
    build_snapshot(&app, &state).await
}

/// 回退到包内自带的版本。
///
/// 实现就是**删掉托管目录** —— 更新从来没碰过 `.app` 包，所以这就是完整的回退。
#[tauri::command]
pub async fn revert_managed_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let dir = xt_core::update::managed_core_dir(state.store.root());
    xt_core::update::revert_managed(&dir).map_err(user_msg)?;
    state.with(|i| {
        i.push_log("app", "info", "已回退到随包版本（核心与 geo）");
        i.update.latest_core = None;
        i.update.latest_geo = None;
    });
    build_snapshot(&app, &state).await
}

/// 读系统的登录项状态。读失败不是致命错误 —— 界面照常可用，
/// 只是那一栏显示「读取失败」，并把原因写出来。
fn login_item_state() -> crate::state::LoginItemState {
    match crate::login_item::status() {
        Ok(s) => crate::state::LoginItemState {
            status: s.as_str().to_string(),
            detail: s.describe().to_string(),
            needs_approval: s == crate::login_item::LoginItem::RequiresApproval,
        },
        Err(e) => crate::state::LoginItemState {
            status: "error".into(),
            detail: e,
            needs_approval: false,
        },
    }
}

/// 导出结果：分享链接 + 二维码 + 没能表达进链接的字段。
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeExport {
    pub node_id: String,
    pub node_name: String,
    /// 分享链接。可以直接复制粘贴，也是二维码的内容。
    pub uri: String,
    /// 二维码（SVG 内联，前端直接塞进 DOM，不额外请求图片）。
    pub svg: String,
    /// 分享链接表达不了、因此**没能带出去**的字段。
    /// 界面必须显示它 —— 静默丢弃是这类功能最容易犯的错。
    pub lost: Vec<String>,
}

/// 导出节点为分享链接 + 二维码。
///
/// 链接由 `xt_core::subscription::share` 生成，那边对每种协议都有
/// 「导出→解析必须回到同一个节点」的往返测试。这里只负责渲染二维码。
#[tauri::command]
pub async fn export_node(
    state: State<'_, AppState>,
    node_id: String,
) -> Result<NodeExport, String> {
    // `with` 自身返回 Option，闭包又返回 Option，所以要 flatten 一层。
    let node = state
        .with(|i| i.nodes.iter().find(|n| n.id == node_id).cloned())
        .flatten()
        .ok_or("找不到该节点")?;

    let export = xt_core::subscription::share::export_uri(&node);

    // 纠错级别用 M：二维码被手机扫时通常完整无遮挡，M 在容错和密度之间
    // 比较平衡；低纠错会让长链接的码过密，高纠错会让码太大。
    let code = qrcode::QrCode::with_error_correction_level(export.uri.as_bytes(), qrcode::EcLevel::M)
        .map_err(|e| format!("生成二维码失败：{e}"))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .quiet_zone(true)
        .min_dimensions(240, 240)
        .dark_color(qrcode::render::svg::Color("#0f172a"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();

    Ok(NodeExport {
        node_id: node.id.clone(),
        node_name: node.name.clone(),
        uri: export.uri,
        svg,
        lost: export.lost,
    })
}

/// 开关开机自启动。
#[tauri::command]
pub async fn set_launch_at_login(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<AppSnapshot, String> {
    crate::login_item::apply(enabled)?;

    // 设置字段跟着系统的真实结果走，而不是跟着请求走 ——
    // 注册成功了但需要用户批准时，字段要不要置 true？
    // 由 status() 决定，避免又造出一个「字段和现实不一致」的状态。
    let actual = crate::login_item::status()?;
    let mut settings = state.with(|i| i.settings.clone()).ok_or(STATE_UNAVAILABLE)?;
    settings.launch_at_login = actual.is_on();
    persist_settings(&state, &settings)?;

    state.with(|i| {
        i.push_log("app", "info", format!("开机自启动：{}", actual.describe()));
    });
    build_snapshot(&app, &state).await
}

/// 打开系统设置的登录项页面（`RequiresApproval` 时用）。
#[tauri::command]
pub async fn open_login_item_settings() -> Result<(), String> {
    crate::login_item::open_system_settings()
}

fn core_availability(app: &AppHandle, state: &AppState) -> CoreAvailability {
    let explicit = state.with(|i| i.settings.core_path.clone()).flatten();
    let resource_dir = app.path().resource_dir().ok();

    let dev_dir = crate::dev_binaries_dir();
    let managed = xt_core::update::managed_core_dir(state.store.root());
    match xray::resolve_core_binary(
        explicit.as_deref(),
        Some(&managed),
        resource_dir.as_deref(),
        dev_dir.as_deref(),
    ) {
        Ok(path) => {
            let version = std::process::Command::new(&path)
                .arg("version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string());
            let supports = version
                .as_deref()
                .map(crate::supervisor::core_supports_native_tun)
                .unwrap_or(false);
            CoreAvailability {
                path: Some(path),
                version,
                error: None,
                supports_native_tun: supports,
                min_native_tun_version: xray::MIN_CORE_VERSION_NATIVE_TUN.to_string(),
            }
        }
        Err(e) => CoreAvailability {
            path: None,
            version: None,
            error: Some(e.to_string()),
            supports_native_tun: false,
            min_native_tun_version: xray::MIN_CORE_VERSION_NATIVE_TUN.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSnapshot, String> {
    // 登录项要对齐到设置里的期望值。
    //
    // 放在 persist 之前：apply 是幂等的（已经是目标状态就什么都不做），
    // 所以每保存一次设置都会走到这里，不会有副作用。
    // 失败不阻断保存 —— 其余设置该存还是要存，登录项的问题单独报给用户。
    if let Err(e) = crate::login_item::apply(settings.launch_at_login) {
        tracing::warn!(error = %e, "同步开机自启动设置失败");
        state.with(|i| {
            i.push_log("app", "warn", format!("设置开机自启动失败：{e}"));
            i.last_notice = Some(format!("设置开机自启动失败：{e}"));
        });
    }

    // **`was_connected` 归后端所有，前端不得覆盖它。**
    //
    // 它是「用户希望它连着」这个**意图**，只有 `start_core`（连上）和
    // `stop_proxy`（用户主动点停止）能改。
    //
    // 而前端保存的是**整份** `AppSettings`，那份快照可能是**连接之前**取的
    // —— 于是用户只是改了个「显示网速」或日志级别，就把意图悄悄清成了
    // false，下一次开机自然不自动连。
    //
    // 这是 docs/08 的 A 类：一个字段的**来源**（后端）和**去向**（前端整份回传）
    // 不是同一个地方。凡是"前端不拥有"的字段，都不能让整份回传覆盖它。
    let mut settings = settings;
    if let Some(intent) = state.with(|i| i.settings.was_connected) {
        if settings.was_connected != intent {
            tracing::debug!(
                from = settings.was_connected,
                to = intent,
                "忽略前端回传的 was_connected（它由后端拥有）"
            );
        }
        settings.was_connected = intent;
    }

    persist_settings(&state, &settings)?;

    // 「显示网速」是个纯展示开关，不该为了它重启核心。这里立刻按新设置
    // 重画一次标题；核心没在跑时用全 0 的采样，等价于恢复成 App 名字。
    let (traffic, show) = state
        .with(|i| (i.traffic.clone(), i.settings.show_speed_in_title))
        .unwrap_or_default();
    crate::traffic::update_titles(&app, &traffic, show);

    events::settings_changed(&app);
    build_snapshot(&app, &state).await
}

/// 切换运行模式。**会重启核心**（如果之前正在运行）。
#[tauri::command]
pub async fn set_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: ProxyMode,
) -> Result<AppSnapshot, String> {
    let mut settings = state
        .with(|i| i.settings.clone())
        .ok_or_else(|| "应用状态不可用".to_string())?;
    let was_running = state.with(|i| i.runtime.running).unwrap_or(false);
    settings.mode = mode;
    persist_settings(&state, &settings)?;

    if was_running {
        stop_core(&app, &state).await?;
    }
    if mode != ProxyMode::Direct {
        start_core(&app, &state).await?;
    } else {
        state.with(|i| {
            i.runtime = CoreRuntime::default();
            i.push_log("app", "info", "已切换到直连模式");
        });
    }
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 核心启停
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn start_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_core(&app, &state).await?;
    spawn_dns_reprobe(&app, &state);
    build_snapshot(&app, &state).await
}

/// 启动时把上次的连接状态恢复回来。
///
/// **为什么需要它**：自更新会先退出 app（核心随之优雅关闭）、替换 `.app`、
/// 再重启。如果不恢复，用户没关过的东西就断了 —— 表现就是「运行一段时间
/// 网就停了」，而原因藏在一份 `app-update.log` 里。崩溃后被系统重启、
/// 开机自启同理。
///
/// 只在**上次确实是连着的**（`was_connected`）且用户没禁止自动重连时才做。
/// 用户主动点过「停止」的话这个标记是 false，所以「除非我关闭，否则不该断」
/// 是成立的。
pub async fn reconnect_if_needed(app: &AppHandle, state: &AppState) {
    let (was, auto, mode) = state
        .with(|i| {
            (
                i.settings.was_connected,
                i.settings.auto_reconnect,
                i.settings.mode,
            )
        })
        .unwrap_or((false, false, ProxyMode::Direct));
    let running = state.with(|i| i.runtime.running).unwrap_or(false);
    if !should_auto_reconnect(was, auto, &mode, running) {
        return;
    }

    state.with(|i| i.push_log("app", "info", "上次退出时是连接状态，正在自动重连…"));
    events::runtime_changed(app, state);

    // **必须重试，而且要在后台重试。**
    //
    // 开机时登录项会把 app **立刻**拉起来，而那一刻 Wi-Fi 往往还没连上、
    // helper 也可能刚启动 —— `start_core` 必然失败。只试一次的话，用户看到的
    // 就是「每次开机都要手动点连接」，而那正是要消灭的行为。
    //
    // 调用方是 `spawn` 出来的（见 lib.rs），所以这里等几分钟也不会挡住窗口。
    let mut last_err = String::new();
    for attempt in 1..=RECONNECT_ATTEMPTS {
        match start_core(app, state).await {
            Ok(()) => {
                let msg = if attempt == 1 {
                    "已自动重连".to_string()
                } else {
                    format!("已自动重连（第 {attempt} 次尝试成功）")
                };
                let _ = state.with(|i| {
                    i.push_log("app", "info", msg);
                    i.last_notice = None;
                });
                events::runtime_changed(app, state);
                return;
            }
            Err(e) => {
                last_err = e;
                let (wants, running) = state
                    .with(|i| (i.settings.was_connected, i.runtime.running))
                    .unwrap_or((false, false));
                if !should_keep_reconnecting(wants, running) {
                    let _ = state.with(|i| {
                        i.push_log(
                            "app",
                            "info",
                            if running {
                                "隧道已在运行，停止自动重连"
                            } else {
                                "用户已关闭，停止自动重连"
                            },
                        )
                    });
                    return;
                }
                // 逐次失败只记 debug：默认日志级别是 warning，不会刷屏；
                // 而调试时打开 debug 就能看到每次失败的具体原因。
                let _ = state.with(|i| {
                    i.push_log(
                        "app",
                        "debug",
                        format!("自动重连第 {attempt}/{RECONNECT_ATTEMPTS} 次未成功：{last_err}"),
                    )
                });
                tokio::time::sleep(RECONNECT_INTERVAL).await;
            }
        }
    }

    let msg = format!(
        "自动重连试了 {RECONNECT_ATTEMPTS} 次（约 {} 秒）仍失败：{last_err} —— 请手动连接",
        RECONNECT_ATTEMPTS as u64 * RECONNECT_INTERVAL.as_secs()
    );
    let _ = state.with(|i| {
        i.push_log("app", "warn", msg.clone());
        i.last_notice = Some(msg);
    });
    events::runtime_changed(app, state);
}

/// 连上之后**真的发一个请求出去**，确认这条隧道能用。
///
/// 为什么必须做：启动流程里那两次检查问的都是「**服务器** TCP 可达吗」，
/// 而「节点活着、却转发不了流量」是完全可能的 —— 实测某个节点正是如此：
/// TCP 握手 55ms 正常，但经它访问任何目标都超时。
///
/// 这时 App 显示「已连接」，用户看到的却是一屏：
///
/// ```text
/// app/dns: failed to retrieve response for x.com.
///   > Post "https://9.9.9.9/dns-query": context deadline exceeded
/// ```
///
/// 五台国外解析器轮流失败（同层回退在正常工作），但真正的原因在**节点那一侧**，
/// 日志里完全看不出来。
///
/// 这个检查**经本地 SOCKS 入站**发一个 204 请求 —— 那是真实用户路径。
/// 用 `--socks5-hostname`，域名由节点去解析，所以它同时覆盖了「转发」和
/// 「节点侧解析」两件事。失败时直接点名是节点的问题。
/// 探测结果算不算「隧道不通」。
///
/// `curl` 拿不到 HTTP 码时（连不上代理、超时、被 reset）`%{http_code}` 是
/// `000`；进程根本没起来时是空串。两种都算不通，别只认其中一种。
fn tunnel_is_dead(http_code: &str) -> bool {
    http_code.is_empty() || http_code == "000"
}

/// 启动时该不该自动连回来。
///
/// 四个条件缺一不可 —— 抽成纯函数是为了能测，而不是散在 async 流程里。
fn should_auto_reconnect(
    was_connected: bool,
    auto_reconnect: bool,
    mode: &ProxyMode,
    already_running: bool,
) -> bool {
    // 「上次是连着的」= 用户的意图。用户主动停止会清掉它，所以这里成立。
    was_connected
        && auto_reconnect
        && *mode != ProxyMode::Direct
        && !already_running
}

/// 看门狗该不该继续盯着这条隧道。
///
/// 判据是**意图 + 代次**，而不是观测到的 `runtime.running`：
///
/// * 核心自己死掉时，日志转发任务会把 `running` 置 false —— 而那恰恰是
///   最需要有人把它救回来的时刻。看 `running` 的话看门狗会当场退出，
///   于是没人恢复，按钮又被幂等守卫挡住，**彻底卡死**（实测症状）。
/// * 用户主动关闭时才该收手 —— 那个意图由 `was_connected` 承载。
/// * 用户重连会换 pid，旧的那条隧道不归我管了。
fn watchdog_should_watch(
    user_wants_it: bool,
    my_pid: Option<u32>,
    current_pid: Option<u32>,
) -> bool {
    user_wants_it && my_pid == current_pid
}

/// 自动重连最多试几次、每次隔多久。
///
/// 开机场景下网络和 helper 都可能还没就绪，所以预算给得宽一点：
/// 24 × 5s ≈ 2 分钟。超过就如实报"请手动连接"，而不是无限重试。
const RECONNECT_ATTEMPTS: u32 = 24;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

/// 睡过了多久。
///
/// 睡眠时**单调时钟（`Instant`）不推进，墙上时钟继续走**，所以两者的差就是
/// 睡眠时长。这是不引入任何系统 API 就能检测「睡过了」的标准做法。
///
/// 拿它来干什么：唤醒后隧道几乎必然已经失效（节点连接断了，网关也可能变了），
/// 而看门狗本来要等「连续 2 次探测失败」才重建。知道刚醒过来，就可以
/// **只等 1 次失败**，把恢复从 ~30 秒压到 ~10 秒。
///
/// 为什么不醒来就无条件重建：隧道有时真的没坏，白拆一次要断几秒。
/// 所以只把「失败的判据」提前，不把「重建」提前。
fn slept_for(monotonic_elapsed: Duration, wall_elapsed: Duration) -> Duration {
    wall_elapsed.saturating_sub(monotonic_elapsed)
}

/// 超过这个时长没跑循环，就认为中间睡过（而不是单纯被调度延迟）。
const SLEEP_THRESHOLD: Duration = Duration::from_secs(30);

/// 自动重连还要不要继续试。
///
/// 两种情况都该停：
/// * 用户明确关掉了（意图变了）—— 继续试就是「关不掉」；
/// * 隧道已经在跑 —— 用户自己点了连接并成功了，再插一手就是抢。
fn should_keep_reconnecting(user_wants_it: bool, already_running: bool) -> bool {
    user_wants_it && !already_running
}

/// 连续失败几次之后才重建隧道。
///
/// 一次失败可能只是节点抖了一下；连续两次才算隧道真的没了。
const FAILURES_BEFORE_REBUILD: u32 = 2;

/// 看门狗该不该重建隧道。
///
/// 三个条件都必须满足：
/// * `still_mine` —— 这次连接还是我负责的那次（用户重连会换 pid）；
/// * `user_wants_it` —— 用户**现在还**想连着（探测是异步的，等结果回来时
///   他可能已经点了关闭；不看这个就会「点了关闭，几秒后它自己又连上」）；
/// * 失败次数到阈值。
fn should_rebuild_tunnel(still_mine: bool, user_wants_it: bool, consecutive_failures: u32) -> bool {
    still_mine && user_wants_it && consecutive_failures >= FAILURES_BEFORE_REBUILD
}

/// 经本地 SOCKS 入站发一个**真实请求**，返回 HTTP 状态码（失败时空串）。
///
/// 用 `--socks5-hostname` 让节点去解析域名，所以这一个检查同时覆盖
/// 「能不能转发」和「节点侧能不能解析」两件事。抽出来是因为连接后的
/// 一次性检查和看门狗都要用它。
async fn tunnel_probe(port: u16, timeout_secs: u32) -> String {
    let probe = xt_core::xray::DEFAULT_PROBE_URL.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("/usr/bin/curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                &timeout_secs.to_string(),
                "--socks5-hostname",
                &format!("127.0.0.1:{port}"),
                &probe,
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

/// 隧道看门狗：**只要用户没主动断开，网络就不该是坏的。**
///
/// 熄屏/睡眠唤醒、换网、路由器重发 DHCP、节点抖动 —— 这些都会让一条看起来
/// 「已连接」的隧道实际失效（路由指向旧网关、核心到节点的连接全断），
/// 而界面不会变，用户能看到的只是「网断了」。换网那次我们只做到「报一句」，
/// 那不够：**报一句不解决"任意时刻都不该断"的要求。**
///
/// 这个任务每 10 秒经本地 SOCKS 入站做一次真实请求；**连续两次**失败就
/// 自动重建隧道（用当前的物理出口重新算路由与 DNS）。
///
/// 重建也失败时**退回直连**（拆掉隧道）而不是把用户留在断网状态 ——
/// 「能上网但不走代理」永远好过「什么都上不了」。
///
/// 用户主动断开时 `runtime.running` 变 false，这个任务下一轮就自己退出；
/// 重连会换 pid，旧的那个同样会退出 —— 所以不会出现多个看门狗打架。
fn spawn_tunnel_watchdog(app: &AppHandle, pid: Option<u32>) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut failures = 0u32;
        let mut last_mono = std::time::Instant::now();
        let mut last_wall = std::time::SystemTime::now();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;

            // 这一轮到底隔了多久？单调时钟 vs 墙上时钟的差就是睡眠时长。
            let now_mono = std::time::Instant::now();
            let now_wall = std::time::SystemTime::now();
            let slept = slept_for(
                now_mono.duration_since(last_mono),
                now_wall
                    .duration_since(last_wall)
                    .unwrap_or_default(),
            );
            last_mono = now_mono;
            last_wall = now_wall;
            // 刚睡醒：下一轮只要探测失败就立刻重建，不用再等第二次。
            let just_woke = slept > SLEEP_THRESHOLD;
            if just_woke {
                tracing::info!(slept_secs = slept.as_secs(), "检测到从睡眠中唤醒");
            }

            let Some(state) = handle.try_state::<AppState>() else {
                return;
            };
            let (wants, pid_now, port, node_name) = state
                .with(|i| {
                    let selected = i.settings.selected_node.clone();
                    (
                        // **用意图而不是观测到的 `running`。**
                        //
                        // 核心自己死掉时，日志转发任务会把 `running` 置 false。
                        // 如果这里看 `running`，看门狗会以为「用户断开了」而退出 ——
                        // 于是核心死了没人恢复，而按钮又被幂等守卫挡住（见
                        // `Supervisor::is_running`），表现就是**彻底卡死**。
                        //
                        // 意图（was_connected）只有用户主动停止才会变，所以它
                        // 才是「我该不该继续守着」的正确判据。
                        i.settings.was_connected,
                        i.runtime.pid,
                        i.settings.socks_port,
                        i.nodes
                            .iter()
                            .find(|n| Some(&n.id) == selected.as_ref())
                            .map(|n| n.name.clone())
                            .unwrap_or_default(),
                    )
                })
                .unwrap_or((false, None, 10808, String::new()));
            if !watchdog_should_watch(wants, pid, pid_now) {
                return;
            }

            let code = tunnel_probe(port, 6).await;
            if !tunnel_is_dead(&code) {
                failures = 0;
                continue;
            }
            failures += 1;
            if just_woke {
                // 唤醒这一次失败几乎必然是"隧道真的死了"，不必再等第二次。
                failures = failures.max(FAILURES_BEFORE_REBUILD);
            }

            // **用户可能就在刚才点了「关闭」。** 探测是异步的，等它回来时
            // 意图可能已经变了 —— 那就什么都别做，否则就是
            // 「点了关闭，几秒后它自己又连上了」。再查一次意图。
            let user_wants_it = state.with(|i| i.settings.was_connected).unwrap_or(false);
            if !user_wants_it {
                state.with(|i| i.push_log("app", "info", "用户已关闭，取消自动重建"));
                return;
            }
            if !should_rebuild_tunnel(true, user_wants_it, failures) {
                continue;
            }

            state.with(|i| {
                i.push_log(
                    "app",
                    "warn",
                    format!(
                        "隧道连续 {failures} 次不通（熄屏/换网/节点抖动，当前节点「{node_name}」），正在自动重建…"
                    ),
                );
                i.last_notice = Some("网络中断，正在自动恢复…".into());
            });
            events::runtime_changed(&handle, &state);

            // 重建：用**当前**的物理出口重新算路由与 DNS。熄屏唤醒后网关
            // 变了也能对上，这正是"能自愈"的关键。
            if stop_core(&handle, &state).await.is_ok()
                && start_core(&handle, &state).await.is_ok()
            {
                state.with(|i| i.push_log("app", "info", "隧道已自动恢复"));
                // start_core 会 spawn 新的看门狗，这里退出即可。
                return;
            }

            // 重建也失败：退回直连。用户至少能上网 —— 这比死守一条
            // 走不通的隧道更符合「除非我关闭，网络不该断」。
            let _ = stop_core(&handle, &state).await;
            state.with(|i| {
                i.push_log(
                    "app",
                    "error",
                    "自动重建失败，已退回直连：网络可用，但流量不再走代理",
                );
                i.last_notice = Some("自动恢复失败，已退回直连（不再走代理）".into());
            });
            events::runtime_changed(&handle, &state);
            return;
        }
    });
}

fn spawn_connectivity_check(app: &AppHandle, pid: Option<u32>) {
    let port = app
        .try_state::<AppState>()
        .and_then(|s| s.with(|i| i.settings.socks_port))
        .unwrap_or(10808);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        // 刚连上时 SOCKS 入站可能还在处理头几个连接，稍等一下再问。
        tokio::time::sleep(Duration::from_secs(2)).await;

        let Some(state) = handle.try_state::<AppState>() else {
            return;
        };
        // 只认自己那一次连接：用户可能已经重连或断开了。
        let (still_mine, node_name, node_id) = state
            .with(|i| {
                let selected = i.settings.selected_node.clone();
                (
                    i.runtime.running && i.runtime.pid == pid,
                    i.nodes
                        .iter()
                        .find(|n| Some(&n.id) == selected.as_ref())
                        .map(|n| n.name.clone())
                        .unwrap_or_default(),
                    selected.unwrap_or_default(),
                )
            })
            .unwrap_or((false, String::new(), String::new()));
        if !still_mine {
            return;
        }

        let code = tunnel_probe(port, 10).await;

        let Some(state) = handle.try_state::<AppState>() else {
            return;
        };
        if tunnel_is_dead(&code) {
            // 这个节点用不了。**如果手上有验证过的好节点，自动退回去** ——
            // 用户刚才是从那个节点切过来的（切换会拆掉旧隧道），
            // 留在这里等于让他断网。
            let fallback = state
                .with(|i| {
                    i.runtime
                        .last_good_node
                        .clone()
                        .filter(|b| b != &node_id && !node_id.is_empty())
                })
                .unwrap_or(None);

            let msg = match &fallback {
                Some(_) => format!(
                    "节点「{node_name}」连上了但流量出不去，正在自动退回上一个可用节点"
                ),
                None => format!(
                    "节点「{node_name}」已连接，但流量出不去（经它访问目标超时）。请换一个节点。"
                ),
            };
            state.with(|i| {
                i.push_log("app", "error", msg.clone());
                i.last_notice = Some(msg);
                // 先清掉，避免回退后那次检查再失败时又触发一次回退（来回弹）。
                i.runtime.last_good_node = None;
            });
            events::runtime_changed(&handle, &state);

            if let Some(back) = fallback {
                let _ = select_node(handle.clone(), state, back).await;
            }
        } else {
            state.with(|i| {
                i.push_log("app", "info", format!("隧道连通性检查通过（HTTP {code}）"));
                // **验证过**才算好节点 —— 这个字段会被切换失败时的回退用到。
                i.runtime.last_good_node = Some(node_id.clone());
            });
            events::runtime_changed(&handle, &state);
        }
    });
}

/// 物理出口的「身份」。隧道是照它建的，换网之后要拿它比对。
#[derive(Debug, Clone, PartialEq)]
struct Egress {
    interface: String,
    gateway: Option<std::net::IpAddr>,
}

impl Egress {
    fn now() -> Option<Self> {
        xt_tun::macos::route::default_route().ok().map(|d| Self {
            interface: d.interface,
            gateway: d.gateway,
        })
    }

    fn describe(&self) -> String {
        match self.gateway {
            Some(g) => format!("{} ({g})", self.interface),
            None => self.interface.clone(),
        }
    }
}

/// 物理出口换了吗。网卡换了、或者同一张网卡换了网关（换 WiFi、插网线、
/// 开热点、路由器重发 DHCP），都算换网。
fn network_moved(before: &Egress, after: &Egress) -> bool {
    before != after
}

/// 连上之后盯着物理出口有没有变。
///
/// 隧道是**按连接那一刻的物理出口**建的：helper 装的路由指向当时的网关，
/// `direct` 出站绑的是当时那张网卡（`sockopt.interface`），核心的 DoH 长连接
/// 也建在那条路径上。换网之后这三样**一起失效**，而且内核不会因此报任何错，
/// 表现就是满屏：
///
/// ```text
/// app/dns: failed to retrieve response for query.ess.apple.com.
///   > Post "https://1.1.1.1/dns-query": io: read/write on closed pipe
/// ```
///
/// 那句 `read/write on closed pipe` 是「连接被人从脚下抽走了」，**不是超时** ——
/// 这也是区分「换网」和「节点抖动」的关键：后者报的是
/// `context deadline exceeded`。
///
/// 这里**只报警、不自动重连**。拆掉再重建 TUN 是全项目最危险的动作，而网络
/// 切换时常常会抖几下（WiFi 掉一下再回来），自动重连会跟着来回拆建，
/// 风险远大于收益。把「静默失效」变成一句能读的报错，让用户在网络稳定之后
/// 自己点重连 —— 那样才真的有效。
fn spawn_network_watch(app: &AppHandle, baseline: Option<Egress>, pid: Option<u32>) {
    let Some(before) = baseline else {
        return;
    };
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let Some(state) = handle.try_state::<AppState>() else {
                return;
            };
            // 只认**自己那一次连接**：这个 watcher 是每次连接都会 spawn 的，
            // 用户快速重连时旧的那些必须自己退出，否则会留一堆在跑、
            // 同一次换网报出好几遍。pid 每次连接都不同，用它当身份。
            let still_mine = state
                .with(|i| i.runtime.running && i.runtime.pid == pid)
                .unwrap_or(false);
            if !still_mine {
                return;
            }
            let Some(now) = Egress::now() else {
                continue; // 查不到默认路由是暂时的，下一轮再看
            };
            if !network_moved(&before, &now) {
                continue;
            }
            let msg = format!(
                "物理出口已变化（{} → {}），隧道不再有效，请断开后重新连接",
                before.describe(),
                now.describe()
            );
            state.with(|i| i.push_log("app", "error", msg));
            events::runtime_changed(&handle, &state);
            return; // 只报一次，别刷屏
        }
    });
}

/// 造一个把下载进度同时送到**状态**和**事件**的回调。
///
/// 两条路都要走：事件让进度条立刻动起来（每 200ms 一次），状态则保证
/// 用户中途切页面/刷新快照之后，进度条不会凭空消失。
fn progress_reporter(
    app: AppHandle,
    label: &'static str,
    total: Option<u64>,
) -> impl FnMut(u64) + Send + 'static {
    let mut last = 0u64;
    move |done: u64| {
        // 只在真正前进时才报，避免 curl 卡住时刷屏。
        if done == last {
            return;
        }
        last = done;
        if let Some(state) = app.try_state::<AppState>() {
            state.with(|i| {
                i.update.progress = Some(crate::state::UpdateProgress {
                    label: label.to_string(),
                    done_bytes: done,
                    total_bytes: total,
                });
            });
        }
        events::update_progress(&app, label, done, total);
    }
}

/// 连上之后在后台重探一次 DNS。
///
/// 启动时探的那一次，国外组必然是「未探测」—— 那时节点还没连上，而国外 DNS
/// **只有经节点才测得了**（见 docs/04 §6.7）。不补这一次，用户就得自己点
/// 「立即检测」，等于这个功能默认不生效。
///
/// **不阻塞连接**（这轮探测要 5–8 秒，国外组是串行的），也**不重启核心**：
/// DNS 配置只在生成配置时被读取，所以结果对**下一次连接**生效。为了几毫秒的
/// 解析器差异，把刚建好的 TUN 拆掉重建，不划算。
fn spawn_dns_reprobe(app: &AppHandle, state: &AppState) {
    if !state.with(|i| i.settings.dns.auto_select).unwrap_or(false) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = handle.try_state::<AppState>() else {
            return;
        };
        if let Err(e) = run_dns_probe_bg(&handle, &state).await {
            tracing::warn!(error = %e, "连接后重探 DNS 失败");
        }
        // 探测结果是状态的一部分，得主动推给前端 —— 它不会自己来问。
        events::runtime_changed(&handle, &state);
    });
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    stop_core(&app, &state).await?;
    // **用户主动停止** —— 这是唯一会清掉「该连着」的地方。其它调用 `stop_core`
    // 的路径（切换节点、看门狗重建）都不该清，它们只是过程，不是意图。
    let settings = state.with(|i| {
        i.settings.was_connected = false;
        i.settings.clone()
    });
    if let Some(settings) = settings {
        let _ = persist_settings(&state, &settings);
    }
    state.with(|i| {
        i.runtime = CoreRuntime::default();
    });
    build_snapshot(&app, &state).await
}

async fn start_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
    // 先在锁外把需要的数据克隆出来。
    let (settings, nodes) = state
        .with(|i| (i.settings.clone(), i.nodes.clone()))
        .ok_or_else(|| "应用状态不可用".to_string())?;

    if settings.mode == ProxyMode::Direct {
        return Err("当前是直连模式，请先切换到「系统代理」或「TUN」".into());
    }

    // 记下**连接之前**的物理出口：隧道是照它建的（helper 的路由指向它的网关、
    // direct 出站绑它的网卡、核心的 DoH 连接也建在它上面）。换网之后这三样
    // 一起失效，所以要留着基线做比对，见 `spawn_network_watch`。
    let egress_before = Egress::now();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<xray::CoreEvent>();
    let resource_dir = app.path().resource_dir().ok();

    let mut supervisor = state.supervisor.lock().await;
    let mut helper = state.helper.lock().await;

    // **已经在跑就当作成功，不要报错。**
    //
    // 「启动」会被好几处并发调用：用户点按钮、看门狗重建、自动重连、
    // 切换节点。对调用方来说「核心已经在运行」不是失败，而是
    // 「你要的状态已经达成了」。
    //
    // 之前它返回错误，用户看到的就是最迷惑的那种：
    // **点「关闭」没反应，再点一下却被告知「核心已经在运行」** ——
    // 因为那几秒里快照的 running 和 supervisor 的真实状态对不上。
    //
    // 这个检查必须在**拿到 supervisor 锁之后**做：在锁外检查的话，
    // 「检查完 → 真正 start」之间照样会被插进来。
    if supervisor.is_running() {
        drop(helper);
        drop(supervisor);
        return Ok(());
    }

    let result = supervisor
        .start(
            &state.store,
            &settings,
            &nodes,
            &mut helper,
            Some(tx),
            crate::supervisor::CoreSearchPaths {
                managed_core_dir: Some(xt_core::update::managed_core_dir(state.store.root())),
                app_resource_dir: resource_dir,
                dev_binaries_dir: crate::dev_binaries_dir(),
            },
        )
        .await;
    drop(helper);
    drop(supervisor);

    let runtime = match result {
        Ok(rt) => rt,
        Err(e) => {
            state.with(|i| {
                i.runtime = CoreRuntime { running: false, last_error: Some(e.clone()), ..Default::default() };
                i.push_log("app", "error", format!("启动失败：{e}"));
            });
            events::runtime_changed(app, state);
            return Err(e);
        }
    };

    state.with(|i| {
        i.runtime = runtime.clone();
        // 记下「用户希望它连着」。自更新/重启之后要靠它自动连回来 ——
        // 否则就是用户没关过、网却断了。
        i.settings.was_connected = true;
        i.push_log(
            "app",
            "info",
            format!(
                "核心已启动（pid {:?}，模式 {}，隧道会话 {:?}）",
                runtime.pid,
                settings.mode.as_str(),
                runtime.tun_session
            ),
        );
    });

    // **必须落盘。** 只在内存里改是不够的：这个标记的**全部用途**就是跨进程
    // 存活（自更新会重启 app），而重启后读的是磁盘上那份。
    // 第一版漏了这一步，于是"修好了自动重连"其实没生效 —— 磁盘上始终是
    // false，重启后照样不连。（实测发现：核心在跑，was_connected 却是 false。）
    if let Some(current) = state.with(|i| i.settings.clone()) {
        if let Err(e) = persist_settings(state, &current) {
            tracing::warn!(error = %e, "记录「上次是连接状态」失败，自更新后可能不会自动重连");
        }
    }
    events::runtime_changed(app, state);

    // 换网之后隧道不会自愈（路由/网卡绑定/长连接全指向旧出口），
    // 盯着它，变了就报一句能读的话。带上 pid 是为了让旧 watcher 自己退出。
    spawn_network_watch(app, egress_before, runtime.pid);

    // 「连上了」不等于「能用」：节点可能活着却转发不了流量。
    // 这直接决定用户接下来会不会面对一屏看不懂的 DNS 超时。
    spawn_connectivity_check(app, runtime.pid);

    // 一直盯着：熄屏唤醒、换网、节点抖动之后，隧道可能已经死了而界面还显示
    // 「已连接」。**只要用户没主动断开，网络就不该是坏的** —— 所以这里不是
    // 报警，而是自动重建（重建也失败就退回直连，至少能上网）。
    spawn_tunnel_watchdog(app, runtime.pid);

    // 日志转发任务：核心的 stdout/stderr → 状态环形缓冲 + UI 事件。
    let app_handle = app.clone();
    let forward_pid = runtime.pid;
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let level = classify_log(&event.line);
            if let Some(state) = app_handle.try_state::<AppState>() {
                state.with(|i| i.push_log("core", level, event.line.clone()));
            }
            let _ = app_handle.emit(events::CORE_LOG, events::LogPayload { line: event.line, level: level.into() });
        }

        // 循环结束 = 核心的 stdout 关了 = **核心已经不在了**。
        //
        // 这里以前什么都不做，于是界面继续显示「已连接」而隧道早就死了，
        // 用户能看到的只有「网断了」而不知道为什么。现在至少把状态改对，
        // 让界面别再骗人；真正的自愈由看门狗负责。
        if let Some(state) = app_handle.try_state::<AppState>() {
            let stale = state
                .with(|i| (i.runtime.running, i.runtime.pid))
                .unwrap_or((false, None));
            if stale.0 && stale.1 == forward_pid {
                state.with(|i| {
                    i.runtime.running = false;
                    i.push_log("app", "error", "核心进程已退出，隧道不再有效");
                });
                events::runtime_changed(&app_handle, &state);
            }
        }
    });

    // 流量采样任务：跟着核心一起生灭（见 traffic.rs 顶部注释）。
    // 先收掉可能还在跑的上一个 —— 切换节点会 stop + start，
    // 忘了收就会有两个任务同时往 state.traffic 里写。
    let monitor = crate::traffic::spawn(app.clone(), xt_core::xray::config::API_PORT);
    state.with(|i| {
        if let Some(old) = i.traffic_task.replace(monitor) {
            old.abort();
        }
    });

    Ok(())
}

async fn stop_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let mut supervisor = state.supervisor.lock().await;
    let mut helper = state.helper.lock().await;
    let result = supervisor.stop(&mut helper).await;
    drop(helper);
    drop(supervisor);

    state.with(|i| {
        // 采样任务必须先收掉：核心没了，api 端口也没人监听，
        // 留着它只会每秒产生一次连接失败。
        if let Some(monitor) = i.traffic_task.take() {
            monitor.abort();
        }
        i.traffic = crate::state::TrafficSample::default();
        i.runtime.running = false;
        i.runtime.pid = None;
        i.runtime.tun_session = None;
        match &result {
            Ok(()) => i.push_log("app", "info", "核心已停止，网络配置已回滚"),
            Err(e) => {
                i.runtime.last_error = Some(e.clone());
                i.push_log("app", "error", format!("停止过程中出错：{e}"));
            }
        }
    });
    // 采样任务已经收掉，标题会永远停在最后一拍的读数上 —— 手动清掉。
    let show = state.with(|i| i.settings.show_speed_in_title).unwrap_or(true);
    crate::traffic::update_titles(app, &crate::state::TrafficSample::default(), show);

    events::runtime_changed(app, state);
    result
}

/// 从 Xray 的日志行里粗分级别，让 UI 能做颜色区分。
///
/// **先信 Xray 自己写的等级标记**，只有在没有标记时才退回关键字判断。
///
/// 之前是纯关键字判断（含 `failed` / `error` / `rejected` 就算错误），结果把
/// 内核的正常信息整片塞进了「错误」页签：
///
/// * `[Info] proxy/dns: rejected type TypeHTTPS query for domain x.com.`
///   内核在说「这个查询类型我不处理」。实测它返回的是一个**快速的空
///   NOERROR**（TYPE65 查询 1ms 返回 `ANSWER: 0`），客户端会立刻回退去问
///   A 记录。这是正常行为，改配置只会更差（见 docs/04 §6.8）。
/// * `[Info] ... write tcp 127.0.0.1:10808->...: write: broken pipe`
///   客户端（浏览器）提前断开连接，keep-alive 连接的日常 churn。
///
/// 关键词判断还有个更隐蔽的坏处：**它把真正的错误淹掉了** —— 错误页签里
/// 全是这两类噪音，用户翻不到真的。而且只要消息里出现 `failed`，连
/// `[Debug]` 行都会被升级成「错误」。
fn classify_log(line: &str) -> &'static str {
    // Xray 的格式：`2026/09/14 17:45:47.320581 [Info] [755193655] 消息`。
    // 这几个标记互不包含，顺序无关。
    for (marker, level) in [
        ("[Error]", "error"),
        ("[Warning]", "warn"),
        ("[Info]", "info"),
        ("[Debug]", "debug"),
    ] {
        if line.contains(marker) {
            return level;
        }
    }

    // 没有等级标记的行（核心启动横幅、或核心写到裸 stderr 的东西）才用关键字。
    let lower = line.to_ascii_lowercase();
    if lower.contains("failed")
        || lower.contains("error")
        || lower.contains("fatal")
        || lower.contains("panic")
    {
        "error"
    } else if lower.contains("warn") {
        "warn"
    } else if lower.contains("debug") {
        "debug"
    } else {
        "info"
    }
}

// ---------------------------------------------------------------------------
// 节点与订阅
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn select_node(
    app: AppHandle,
    state: State<'_, AppState>,
    node_id: String,
) -> Result<AppSnapshot, String> {
    let exists = state.with(|i| i.nodes.iter().any(|n| n.id == node_id)).unwrap_or(false);
    if !exists {
        return Err("找不到该节点".into());
    }
    let mut settings = state.with(|i| i.settings.clone()).ok_or(STATE_UNAVAILABLE)?;
    let previous = settings.selected_node.clone();
    settings.selected_node = Some(node_id.clone());
    persist_settings(&state, &settings)?;

    // 核心在跑就重启，让新节点立即生效（配置变更走重启，见 docs/03）。
    //
    // **这一步是几秒钟的拆建，不是瞬时切换**：Xray 没有配置热重载，
    // 换节点必须换配置、换配置必须重启核心。所以要有明确的过程提示 ——
    // 否则用户看到的就是「点了没反应，然后所有连接断一遍」。
    if state.with(|i| i.runtime.running).unwrap_or(false) {
        let name = state
            .with(|i| i.nodes.iter().find(|n| n.id == node_id).map(|n| n.name.clone()))
            .unwrap_or(None)
            .unwrap_or_else(|| node_id.clone());
        state.with(|i| i.push_log("app", "info", format!("正在切换到「{name}」，需要重建隧道（几秒）")));
        let was_good = state
            .with(|i| i.runtime.last_good_node.clone())
            .unwrap_or(None)
            .or(previous.clone());

        stop_core(&app, &state).await?;

        // **这里失败必须回退。** 旧的隧道已经拆了，如果新节点起不来就直接
        // 把用户丢在断网状态 —— 而「新节点是坏的」是常见情况（实测有节点
        // TCP 可达却转发不了流量）。没有这一段，一次误选就是一次连环爆炸。
        if let Err(e) = start_core(&app, &state).await {
            state.with(|i| {
                i.push_log(
                    "app",
                    "error",
                    format!("切到该节点失败（{e}），正在退回上一个可用节点"),
                )
            });
            if let Some(back) = was_good.filter(|b| b != &node_id) {
                let mut s2 = state.with(|i| i.settings.clone()).ok_or(STATE_UNAVAILABLE)?;
                s2.selected_node = Some(back);
                persist_settings(&state, &s2)?;
                start_core(&app, &state).await?;
            }
            return Err(format!("切到该节点失败，已退回：{e}"));
        }
    }
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn add_manual_node(
    app: AppHandle,
    state: State<'_, AppState>,
    link: String,
) -> Result<AppSnapshot, String> {
    // 用 parse_manual 而不是 parse_share_link：用户粘贴的可能是
    // 一条分享链接、一段订阅正文、或者一条 Clash proxy 定义。
    let outcome = xt_core::subscription::parse_manual(&link).map_err(user_msg)?;
    let node = outcome
        .nodes
        .into_iter()
        .next()
        .ok_or_else(|| "没能解析出节点".to_string())?;
    // 一次加锁完成「改内存 + 取出要落盘的两份数据」。
    // 这里曾经是两次 `state.with`：第一次的返回值被整个丢掉，紧接着再加锁
    // 克隆同样的两样东西 —— 多一次锁往返加一整份重复克隆。
    let (settings, nodes) = state
        .with(|i| {
            if !i.nodes.iter().any(|n| n.id == node.id) {
                i.nodes.push(node.clone());
            }
            if i.settings.selected_node.is_none() {
                i.settings.selected_node = Some(node.id.clone());
            }
            (i.settings.clone(), i.nodes.clone())
        })
        .ok_or(STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(user_msg)?;
    state.store.save_settings(&settings).map_err(user_msg)?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn delete_node(
    app: AppHandle,
    state: State<'_, AppState>,
    node_id: String,
) -> Result<AppSnapshot, String> {
    let (settings, nodes) = state
        .with(|i| {
            i.nodes.retain(|n| n.id != node_id);
            i.latencies.remove(&node_id);
            if i.settings.selected_node.as_deref() == Some(node_id.as_str()) {
                i.settings.selected_node = i.nodes.first().map(|n| n.id.clone());
            }
            (i.settings.clone(), i.nodes.clone())
        })
        .ok_or(STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(user_msg)?;
    state.store.save_settings(&settings).map_err(user_msg)?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn add_subscription(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    url: String,
) -> Result<AppSnapshot, String> {
    let sub = Subscription {
        id: format!("sub{}", crate::state::now_unix()),
        name: if name.trim().is_empty() { url.clone() } else { name },
        url,
        enabled: true,
        update_interval_hours: 24,
        last_updated: None,
        last_error: None,
        node_count: 0,
        usage: None,
    };
    state.with(|i| i.subscriptions.push(sub.clone()));
    persist_subscriptions(&state)?;
    events::subscriptions_changed(&app);

    // 加完立刻拉一次，用户不用再点「更新」。
    refresh_subscriptions(app.clone(), state, Some(vec![sub.id])).await
}

#[tauri::command]
pub async fn remove_subscription(
    app: AppHandle,
    state: State<'_, AppState>,
    subscription_id: String,
) -> Result<AppSnapshot, String> {
    let (nodes, subs) = state
        .with(|i| {
            i.subscriptions.retain(|s| s.id != subscription_id);
            // 同时清掉该订阅带来的节点，否则会留下永远更新不到的孤儿。
            i.nodes.retain(|n| match &n.source {
                xt_core::model::NodeSource::Subscription { id } => id != &subscription_id,
                xt_core::model::NodeSource::Manual => true,
            });
            (i.nodes.clone(), i.subscriptions.clone())
        })
        .ok_or(STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(user_msg)?;
    state.store.save_subscriptions(&subs).map_err(user_msg)?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

/// 拉取并解析订阅。`ids` 为 `None` 表示全部更新。
#[tauri::command]
pub async fn refresh_subscriptions(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
) -> Result<AppSnapshot, String> {
    let targets: Vec<Subscription> = state
        .with(|i| {
            i.subscriptions
                .iter()
                .filter(|s| s.enabled && ids.as_ref().map(|v| v.contains(&s.id)).unwrap_or(true))
                .cloned()
                .collect()
        })
        .ok_or(STATE_UNAVAILABLE)?;

    if targets.is_empty() {
        return build_snapshot(&app, &state).await;
    }

    let client = reqwest_lite();
    let mut messages = Vec::new();

    for sub in targets {
        // URL 里通常带着 token，日志里绝不打印完整 URL。
        let safe = redact_url(&sub.url);
        state.with(|i| i.push_log("app", "info", format!("正在更新订阅 {safe}")));

        match fetch_subscription(&client, &sub.url).await {
            Ok((body, usage)) => match xt_core::subscription::parse_any(&body) {
                Ok(outcome) => {
                    let count = outcome.nodes.len();
                    state.with(|i| {
                        // 原子替换：先移掉该订阅的旧节点，再插入新解析出的节点。
                        i.nodes.retain(|n| match &n.source {
                            xt_core::model::NodeSource::Subscription { id } => id != &sub.id,
                            xt_core::model::NodeSource::Manual => true,
                        });
                        for mut node in outcome.nodes {
                            node.source = xt_core::model::NodeSource::Subscription { id: sub.id.clone() };
                            if !i.nodes.iter().any(|n| n.id == node.id) {
                                i.nodes.push(node);
                            }
                        }
                        if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                            s.last_updated = Some(crate::state::now_unix());
                            s.last_error = None;
                            s.node_count = count;
                            s.usage = usage.clone();
                        }
                        i.push_log(
                            "app",
                            "info",
                            format!("订阅更新完成：{count} 个节点（跳过 {} 行）", outcome.warnings.len()),
                        );
                        for w in outcome.warnings.iter().take(5) {
                            i.push_log("app", "warn", w.clone());
                        }
                    });
                    messages.push(format!("{count} 个节点"));
                }
                Err(e) => {
                    let msg = format!("订阅 {safe} 解析失败：{e}");
                    state.with(|i| {
                        i.push_log("app", "error", msg.clone());
                        if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                            s.last_error = Some(e.to_string());
                        }
                    });
                    messages.push(msg);
                }
            },
            Err(e) => {
                let msg = format!("订阅 {safe} 拉取失败：{e}");
                state.with(|i| {
                    i.push_log("app", "error", msg.clone());
                    if let Some(s) = i.subscriptions.iter_mut().find(|s| s.id == sub.id) {
                        s.last_error = Some(e.to_string());
                    }
                });
                messages.push(msg);
            }
        }
    }

    state.with(|i| {
        // 选中节点可能在更新中消失了，回退到第一个可用节点。
        let still_valid = i
            .settings
            .selected_node
            .as_deref()
            .map(|id| i.nodes.iter().any(|n| n.id == id))
            .unwrap_or(false);
        if !still_valid {
            i.settings.selected_node = i.nodes.first().map(|n| n.id.clone());
        }
        i.last_notice = Some(messages.join("；"));
    });
    persist_subscriptions(&state)?;
    let (settings, nodes) = state.with(|i| (i.settings.clone(), i.nodes.clone())).ok_or(STATE_UNAVAILABLE)?;
    state.store.save_nodes(&nodes).map_err(user_msg)?;
    state.store.save_settings(&settings).map_err(user_msg)?;
    events::nodes_changed(&app);
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 延迟探测
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn test_latency(
    app: AppHandle,
    state: State<'_, AppState>,
    node_ids: Option<Vec<String>>,
) -> Result<AppSnapshot, String> {
    let (nodes, core_path) = state
        .with(|i| {
            let nodes: Vec<Node> = i
                .nodes
                .iter()
                .filter(|n| node_ids.as_ref().map(|v| v.contains(&n.id)).unwrap_or(true))
                .cloned()
                .collect();
            (nodes, i.settings.core_path.clone())
        })
        .ok_or(STATE_UNAVAILABLE)?;

    if nodes.is_empty() {
        return Err("没有可测试的节点".into());
    }

    let resource_dir = app.path().resource_dir().ok();
    let dev_dir = crate::dev_binaries_dir();
    let binary = xray::resolve_core_binary(
        core_path.as_deref(),
        Some(&xt_core::update::managed_core_dir(state.store.root())),
        resource_dir.as_deref(),
        dev_dir.as_deref(),
    )
        .map_err(user_msg)?;

    state.with(|i| i.push_log("app", "info", format!("开始测试 {} 个节点的延迟", nodes.len())));
    events::probe_started(&app, nodes.len());

    // 物理出口：RTT 必须**在隧道之外**测，否则隧道开着时握手被本地协议栈
    // 立刻应答，测出来是 0ms（实测：不绑 en0 是 0ms，绑了是 53ms）。
    let interface = xt_tun::macos::route::default_route()
        .ok()
        .map(|r| r.interface);

    let started = Instant::now();
    let results = crate::supervisor::probe(&nodes, &binary, Duration::from_secs(5), interface.as_deref())
        .await
        .map_err(|e| {
            state.with(|i| i.push_log("app", "error", format!("探测失败：{e}")));
            e
        })?;

    let ok_count = results.iter().filter(|r| r.ok()).count();
    state.with(|i| {
        for r in &results {
            i.latencies.insert(r.node_id.clone(), r.clone());
        }
        i.push_log(
            "app",
            "info",
            format!(
                "探测完成：{ok_count}/{} 可用，耗时 {:.1}s",
                results.len(),
                started.elapsed().as_secs_f32()
            ),
        );
    });
    events::latency_updated(&app, &results);
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// helper 管理
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn probe_helper(state: State<'_, AppState>) -> Result<HelperAvailability, String> {
    let present = crate::helper_client::socket_present(std::path::Path::new(DEFAULT_SOCKET_PATH));
    let mut helper = state.helper.lock().await;
    // 强制重连，拿到最新状态。
    helper.disconnect();
    Ok(helper.availability(present))
}

#[tauri::command]
pub async fn install_helper(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::install_script(&app)?;
    crate::helper_install::run_with_admin(&script, "安装 XrayTun 网络配置助手")?;
    state.with(|i| i.push_log("app", "info", "helper 安装完成"));
    build_snapshot(&app, &state).await
}

/// 重启 helper。
///
/// 对应 UI 上「helper 已安装但进程没在运行」那个状态的一键修复。
#[tauri::command]
pub async fn restart_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::restart_script();
    crate::helper_install::run_with_admin(&script, "重启 XrayTun 网络配置助手")?;
    // 连接状态可能已变，强制重连一次。
    state.helper.lock().await.disconnect();
    state.with(|i| i.push_log("app", "info", "helper 已重启"));
    build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn uninstall_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::uninstall_script();
    crate::helper_install::run_with_admin(&script, "卸载 XrayTun 网络配置助手")?;
    state.with(|i| i.push_log("app", "warn", "helper 已卸载"));
    build_snapshot(&app, &state).await
}

/// 回滚磁盘上遗留的会话。网络出问题时的「一键修复」。
#[tauri::command]
pub async fn restore_stale(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let mut helper = state.helper.lock().await;
    let response = helper.call(&Request::Restore);
    drop(helper);

    match response {
        Ok(_) => {
            state.with(|i| i.push_log("app", "info", "已请求 helper 回滚遗留会话"));
        }
        Err(e) => {
            state.with(|i| i.push_log("app", "error", format!("回滚失败：{}", e.message)));
            return Err(e.message);
        }
    }
    build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// 日志与诊断
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn tail_logs(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<crate::state::LogEntry>, String> {
    let limit = limit.unwrap_or(400);
    state
        .with(|i| {
            let skip = i.logs.len().saturating_sub(limit);
            i.logs.iter().skip(skip).cloned().collect::<Vec<_>>()
        })
        .ok_or_else(|| "应用状态不可用".to_string())
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    state.with(|i| i.logs.clear());
    Ok(())
}

/// 生成一份可直接贴给维护者的诊断报告。
///
/// 刻意**不包含**订阅 URL、节点地址、UUID/password 等敏感信息 —— 用户会把它
/// 贴到公开的 issue 里，所以在生成端就把它们抹掉，而不是指望用户自己删。
#[tauri::command]
pub async fn diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let snap = build_snapshot(&app, &state).await?;
    let mut out = String::new();
    out.push_str(&format!("XrayTun {}\n", snap.app_version));
    out.push_str(&format!("macOS: {}\n", macos_version()));
    out.push_str(&format!("架构: {}\n", std::env::consts::ARCH));
    out.push_str(&format!("模式: {}\n", snap.settings.mode.as_str()));
    out.push_str(&format!(
        "内核: {:?} / {:?}（原生 TUN 支持: {}，需要 >= {}）\n",
        snap.core.path, snap.core.version, snap.core.supports_native_tun, snap.core.min_native_tun_version
    ));
    out.push_str(&format!(
        "helper: 已安装={} 可连接={} 版本={:?} 隧道活跃={}\n",
        snap.helper.socket_present, snap.helper.reachable, snap.helper.version, snap.helper.tun_active
    ));
    if let Some(e) = &snap.helper.error {
        out.push_str(&format!("helper 错误: {e}\n"));
    }
    out.push_str(&format!(
        "数据目录: {}\n",
        state.store.root().display()
    ));
    out.push_str(&format!(
        "配置: socks={} http={} 允许局域网={} TUN 网段={} MTU={}\n",
        snap.settings.socks_port,
        snap.settings.http_port,
        snap.settings.allow_lan,
        snap.settings.tun.network,
        snap.settings.tun.mtu
    ));
    out.push_str(&format!(
        "订阅数: {}，节点数: {}\n",
        snap.subscriptions.len(),
        snap.nodes.len()
    ));
    out.push_str("\n最近日志:\n");
    if let Some(logs) = state.with(|i| i.logs.iter().rev().take(50).cloned().collect::<Vec<_>>()) {
        for entry in logs.into_iter().rev() {
            out.push_str(&format!("[{}] {} {}\n", entry.source, entry.level, redact_secrets(&entry.message)));
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn open_data_dir(state: State<'_, AppState>) -> Result<(), String> {
    let root = state.store.root().to_path_buf();
    std::process::Command::new("/usr/bin/open")
        .arg(&root)
        .status()
        .map_err(|e| format!("打开数据目录失败：{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

fn persist_subscriptions(state: &AppState) -> Result<(), String> {
    let subs = state.with(|i| i.subscriptions.clone()).ok_or(STATE_UNAVAILABLE)?;
    state.store.save_subscriptions(&subs).map_err(user_msg)
}

/// 极简 HTTP 客户端配置。用 `std::net` 之外的东西会引入新依赖，
/// 而这里只需要「GET 一个 URL、拿 body、带超时」。
fn reqwest_lite() -> HttpClientConfig {
    HttpClientConfig { timeout: Duration::from_secs(20), user_agent: format!("XrayTun/{}", env!("CARGO_PKG_VERSION")) }
}

pub struct HttpClientConfig {
    pub timeout: Duration,
    pub user_agent: String,
}

/// 拉取订阅正文 + 解析 `subscription-userinfo` 响应头。
///
/// 用 `curl` 而不是引入 HTTP 客户端库：macOS 自带 `/usr/bin/curl`，
/// 支持 HTTPS（走系统信任链）、gzip、重定向，且零依赖。
/// 代价是不能复用连接 —— 对「一天更新几次订阅」这个频率完全无所谓。
async fn fetch_subscription(
    cfg: &HttpClientConfig,
    url: &str,
) -> Result<(String, Option<xt_core::model::SubscriptionUsage>), String> {
    let output = tokio::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--location",
            "--compressed",
            "--max-time",
            &cfg.timeout.as_secs().to_string(),
            "--user-agent",
            &cfg.user_agent,
            "--dump-header",
            "-",
            url,
        ])
        .output()
        .await
        .map_err(|e| format!("调用 curl 失败：{e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    // curl 把 header 和 body 一起输出到 stdout，中间用一个空行分隔。
    let (headers, body) = match raw.split_once("\r\n\r\n") {
        Some((h, b)) => (h.to_string(), b.to_string()),
        None => (String::new(), raw.to_string()),
    };

    let usage = headers
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.to_ascii_lowercase()
                .starts_with("subscription-userinfo:")
                .then(|| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                .flatten()
        })
        .map(|v| xt_core::model::SubscriptionUsage::parse_header(&v));

    Ok((body, usage))
}

/// 抹掉 URL 里的凭据部分，只保留 host。
///
/// 机场订阅的 URL 里带 token，用户把日志贴出来就等于把订阅泄漏了。
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}://{}/…", u.scheme(), u.host_str().unwrap_or("<unknown>")),
        Err(_) => "<非法 URL>".to_string(),
    }
}

/// 日志里可能出现的凭据特征串（UUID、长 token）做粗粒度脱敏。
fn redact_secrets(line: &str) -> String {
    line.split_whitespace()
        .map(|token| {
            let looks_like_uuid = token.len() == 36 && token.matches('-').count() == 4;
            if looks_like_uuid {
                "<uuid>".to_string()
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn macos_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 让 `PathBuf` 在诊断输出里可读。
pub fn display_path(p: &Option<PathBuf>) -> String {
    p.as_ref().map(|x| x.display().to_string()).unwrap_or_else(|| "<未找到>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 日志分级要**先信内核自己写的 `[Level]` 标记**。
    ///
    /// 之前纯按关键字判，于是上面那些 `[Info] ... rejected type ...` 和
    /// `[Info] ... broken pipe` 全被归类成「错误」，错误页签里翻不到真错误。
    #[test]
    fn log_classification_trusts_the_level_marker() {
        // 这两条是用户实际报上来的原文。
        assert_eq!(
            classify_log(
                "2026/09/14 17:45:47.320581 [Info] [755193655] proxy/dns: rejected type TypeHTTPS query for domain x.com."
            ),
            "info",
            "内核说的是 Info，消息里带 rejected 不该把它升级成错误",
        );
        assert_eq!(
            classify_log(
                "2026/09/14 17:45:50.210081 [Info] [4072004086] app/proxyman/outbound: failed to process outbound traffic > ... write: broken pipe"
            ),
            "info",
            "消息里带 failed 也一样",
        );
        assert_eq!(
            classify_log("2026/01/01 00:00:00 [Warning] failed to dial"),
            "warn",
            "内核说是 Warning 就是 Warning",
        );
        assert_eq!(classify_log("2026/01/01 00:00:00 [Error] something exploded"), "error");
        assert_eq!(classify_log("2026/01/01 00:00:00 [Debug] dialing 1.2.3.4"), "debug");
        // 没有标记才用关键字。
        assert_eq!(classify_log("Xray 26.9.9 (Xray, Penetrates Everything.)"), "info");
        assert_eq!(classify_log("something debug level"), "debug");
        assert_eq!(classify_log("failed to write config"), "error");
        assert_eq!(classify_log("WARNING: %v"), "warn");
    }

    #[test]
    fn url_redaction_hides_credentials() {
        let redacted = redact_url("https://example.com/sub?token=SECRET123");
        assert!(!redacted.contains("SECRET123"), "{redacted}");
        assert!(redacted.contains("example.com"));
        assert_eq!(redact_url("not a url"), "<非法 URL>");
    }

    #[test]
    fn uuid_is_redacted_from_logs() {
        let line = "user b831381d-6324-4d53-ad4f-8cda48b30811 connected";
        let out = redact_secrets(line);
        assert!(!out.contains("b831381d"), "{out}");
        assert!(out.contains("<uuid>"));
    }

    #[test]
    fn display_path_handles_none() {
        assert_eq!(display_path(&None), "<未找到>");
    }

    /// 换网必须能被识别出来：网卡换了、或同一张网卡换了网关（换 WiFi、
    /// 插网线、开热点、路由器重发 DHCP）都算。
    ///
    /// 这条判据把「静默失效」变成一句报错。实测的判别特征：
    /// 换网报的是 `io: read/write on closed pipe`（连接被抽走），
    /// 节点抖动报的是 `context deadline exceeded`（超时）—— 两者的处置完全不同。
    #[test]
    fn egress_change_is_detected_by_interface_or_gateway() {
        let gw = |s: &str| Some(s.parse().unwrap());
        let base = Egress {
            interface: "en0".into(),
            gateway: gw("192.168.0.1"),
        };
        assert!(
            network_moved(
                &base,
                &Egress {
                    interface: "en0".into(),
                    gateway: gw("192.168.100.1")
                }
            ),
            "同一张网卡换了网关也算换网",
        );
        assert!(
            network_moved(
                &base,
                &Egress {
                    interface: "en1".into(),
                    gateway: gw("192.168.0.1")
                }
            ),
            "换了网卡也算换网",
        );
        assert!(
            network_moved(
                &base,
                &Egress {
                    interface: "en0".into(),
                    gateway: None
                }
            ),
            "网关从有到无（掉线）也算",
        );
        assert!(
            !network_moved(
                &base,
                &Egress {
                    interface: "en0".into(),
                    gateway: gw("192.168.0.1")
                }
            ),
            "没变就不该报",
        );
        assert_eq!(base.describe(), "en0 (192.168.0.1)");
    }

    /// 自动排序要把测出来的顺序放前面，**但不能删掉用户手填的解析器**。
    #[test]
    fn merge_ranked_keeps_user_servers_after_probed_ones() {
        let pool = xt_core::dns_probe::DNS_POOL;
        let current = vec!["210.2.4.8".to_string(), "1.1.1.1".to_string()];
        let ranked = vec!["223.5.5.5".to_string(), "119.29.29.29".to_string()];
        let out = merge_ranked(
            &current,
            &ranked,
            xt_core::dns_probe::DnsKind::Domestic,
            pool,
        );
        assert_eq!(
            out,
            vec!["223.5.5.5", "119.29.29.29", "1.1.1.1"],
            "池内项被换成新顺序，用户手填的 1.1.1.1 保留在后面"
        );
    }

    /// 国外组只动 `remote_servers`：池里的国内 IP 出现在这一列时算「用户自己
    /// 加的」，不该被国外候选顶掉。
    #[test]
    fn merge_ranked_is_scoped_to_its_own_kind() {
        let pool = xt_core::dns_probe::DNS_POOL;
        let current = vec!["https://8.8.8.8/dns-query".to_string(), "223.5.5.5".to_string()];
        let ranked = vec!["https://1.1.1.1/dns-query".to_string()];
        let out = merge_ranked(
            &current,
            &ranked,
            xt_core::dns_probe::DnsKind::Foreign,
            pool,
        );
        assert_eq!(out, vec!["https://1.1.1.1/dns-query", "223.5.5.5"]);
    }

    /// 排名为空时不能把列表清掉 —— 那是「没测出来」，不是「都不要了」。
    #[test]
    fn merge_ranked_with_empty_ranking_keeps_current() {
        let pool = xt_core::dns_probe::DNS_POOL;
        let current = vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()];
        let out = merge_ranked(&current, &[], xt_core::dns_probe::DnsKind::Domestic, pool);
        assert_eq!(out, current);
    }

    // ---------------------------------------------------------------------
    // 0.7.x 的隧道生命周期与自愈
    //
    // 下面这些**全部是新增用例**，没有改动上面任何一条已有的。它们钉的是
    // 这一轮真实出过问题的三个判断 —— 抽成纯函数正是为了能这样钉住。
    // 整体的失败模式分类见 docs/08-failure-modes.md。
    // ---------------------------------------------------------------------

    /// `curl` 的输出要分得清「没通」和「通了但服务器不高兴」。
    ///
    /// `000` 是连不上/超时/被 reset，空串是进程压根没起来 —— 都算不通。
    /// 但 403 说明**链路是好的**，只是目标拒绝了我们；把它算成不通会
    /// 让一条能用的隧道被判死并重建。
    #[test]
    fn tunnel_probe_result_is_read_as_dead_or_alive() {
        assert!(tunnel_is_dead(""), "进程没起来时 curl 不输出");
        assert!(tunnel_is_dead("000"), "连不上/超时/reset 都报 000");
        assert!(!tunnel_is_dead("204"));
        assert!(!tunnel_is_dead("200"));
        assert!(!tunnel_is_dead("403"), "服务器答了任何码都说明链路通");
    }

    /// 自动重连的四个条件缺一不可。
    ///
    /// 自更新会先退出 app、替换、再重启 —— 重启后要不要连回来，完全由
    /// 这个判断决定。它宽松一点就是「用户关过的隧道自己回来了」，
    /// 严一点就是「用户没关过的东西断了」。
    #[test]
    fn auto_reconnect_requires_intent_and_absence_of_a_running_core() {
        let tun = ProxyMode::Tun;
        assert!(should_auto_reconnect(true, true, &tun, false), "上次连着就该连回来");
        assert!(
            !should_auto_reconnect(false, true, &tun, false),
            "用户主动停止过 —— 不该自己连回来",
        );
        assert!(
            !should_auto_reconnect(true, false, &tun, false),
            "用户关掉了自动重连",
        );
        assert!(
            !should_auto_reconnect(true, true, &ProxyMode::Direct, false),
            "直连模式没有隧道可连",
        );
        assert!(
            !should_auto_reconnect(true, true, &tun, true),
            "已经在跑就别重复启动（那会撞出「核心已经在运行」）",
        );
    }

    /// 睡眠检测：墙上时钟比单调时钟多走的那部分就是睡眠时长。
    ///
    /// 这条判据决定「唤醒后多久开始恢复」—— 判错成「没睡」就退化成
    /// 等两次失败（约 30 秒），判错成「睡了」则只是早一次探测、无害。
    #[test]
    fn sleep_is_detected_as_wall_clock_running_ahead_of_monotonic() {
        let s = Duration::from_secs;
        // 正常的一轮：两个时钟走的一样多 → 没睡。
        assert_eq!(slept_for(s(10), s(10)), Duration::ZERO);
        // 睡了 8 小时：单调走了 10 秒，墙上走了 8 小时。
        let slept = slept_for(s(10), s(8 * 3600));
        assert!(slept > SLEEP_THRESHOLD, "8 小时必须被认成睡过：{slept:?}");
        // 边界：刚好一分钟。
        assert!(slept_for(s(10), s(70)) > SLEEP_THRESHOLD);
        // 墙上时钟落后（NTP 回调）不能 panic，也不能当成睡过。
        assert_eq!(slept_for(s(60), s(10)), Duration::ZERO);
    }

    /// 自动重连该不该继续试。
    ///
    /// 它一次性最多试约 2 分钟（开机时网络和 helper 都可能没就绪），
    /// 所以「什么时候停」必须判对：用户关掉了还继续试 = 关不掉；
    /// 用户自己连上了还继续试 = 抢。
    #[test]
    fn auto_reconnect_stops_when_user_says_so_or_it_is_already_up() {
        assert!(should_keep_reconnecting(true, false), "用户还想要、还没起来 —— 继续试");
        assert!(
            !should_keep_reconnecting(false, false),
            "用户明确关掉了 —— 再试就是「关不掉的软件」",
        );
        assert!(
            !should_keep_reconnecting(true, true),
            "已经在跑了（用户自己点成功了）—— 再插一手就是抢",
        );
        assert!(
            !should_keep_reconnecting(false, true),
            "两种情况同时成立也该停",
        );
    }

    /// 看门狗该不该继续盯着：**意图 + 代次**，不看观测到的 `running`。
    ///
    /// 这条钉的是一个会「彻底卡死」的组合：核心自己死掉 → 日志转发任务把
    /// `running` 置 false → 如果看门狗看 `running` 就会当场退出 → 没人恢复；
    /// 而按钮那边又被幂等守卫挡住（`Supervisor::is_running` 曾经只看
    /// `process.is_some()`）。两边一起坏，用户就只能看到「点了没反应」。
    #[test]
    fn watchdog_keys_off_intent_and_generation_not_observed_state() {
        assert!(
            watchdog_should_watch(true, Some(9), Some(9)),
            "用户还想要、还是我那次连接 —— 继续盯",
        );
        assert!(
            watchdog_should_watch(true, Some(9), Some(9)),
            "注意：这里**没有** running 参数 —— 核心刚死时 running 已是 false，"
        );
        assert!(!watchdog_should_watch(false, Some(9), Some(9)), "用户关掉了，收手");
        assert!(
            !watchdog_should_watch(true, Some(9), Some(11)),
            "已经重连过（换了 pid），这条隧道不归我管了",
        );
        // 两边都拿不到 pid 时**继续盯**：宁可多看一会儿，也不要因为
        // 「分不清代次」就放着一条坏隧道不管（那正是卡死的成因）。
        // 一旦新的连接有了 pid，这里就不相等，旧看门狗自然退出。
        assert!(
            watchdog_should_watch(true, None, None),
            "拿不到代次信息时继续盯 —— 别放着坏隧道不管",
        );
    }

    /// 看门狗重建的三个条件：还是我负责的那次连接、用户**现在还**想要、
    /// 失败次数到阈值。
    ///
    /// 中间那个条件最容易被忽略，而漏掉它的后果很具体：
    /// **点了「关闭」，几秒后它自己又连上了** —— 因为探测是异步的，
    /// 等结果回来时用户的意图已经变了。
    #[test]
    fn watchdog_rebuild_needs_mine_intent_and_threshold() {
        assert!(
            !should_rebuild_tunnel(true, true, FAILURES_BEFORE_REBUILD - 1),
            "一次失败可能只是节点抖了一下，不该立刻拆建",
        );
        assert!(should_rebuild_tunnel(true, true, FAILURES_BEFORE_REBUILD));
        assert!(should_rebuild_tunnel(true, true, 5), "一直不通就该重建");
        assert!(
            !should_rebuild_tunnel(false, true, 5),
            "用户重连过了 —— 旧的看门狗该自己退出，不能去动新的那条隧道",
        );
        assert!(
            !should_rebuild_tunnel(true, false, 5),
            "用户已关闭 —— 重建它就等于「关闭按钮没用」",
        );
    }
}
