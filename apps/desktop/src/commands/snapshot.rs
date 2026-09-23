//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn snapshot(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    build_snapshot(&app, &state).await
}

pub(crate) async fn build_snapshot(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let helper_socket = DEFAULT_SOCKET_PATH;
    let socket_present = crate::helper_client::socket_present(std::path::Path::new(helper_socket));

    let mut guard = state.helper.lock().await;
    let mut helper = guard.availability(socket_present);
    // task-84：**App 更新不会刷新特权 helper**（只有 `install_helper` 会把包内那份
    // 拷过去），而路由/DNS 的安装与回滚都在 helper 里 —— 所以这里读**实际工件**
    // 对照版本，三态进快照供界面判断。读不到就如实说读不到，**不许猜成不一致**。
    // 纯只读：只执行两个二进制的 `version` 子命令（不安装、不重启、不需要管理员）。
    helper.version_check = helper_version_check(app);
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
    // **把已经算好的 `core` 传进去**：原文这里会再算一次 `core_availability`，
    // 而那次会再 spawn 一次 `xray version`。一次快照一次 spawn 是纯浪费。
    let update = update_status(app, state, &core);
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

/// 手动触发一次 DNS 探测并应用结果。
#[tauri::command]
pub async fn probe_dns(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    run_dns_probe(&app, &state, true).await?;
    build_snapshot(&app, &state).await
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
pub(crate) async fn run_dns_probe(app: &AppHandle, state: &AppState, apply: bool) -> Result<(), String> {
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
        let mut settings = state.with(|i| i.settings.clone()).ok_or(util::STATE_UNAVAILABLE)?;
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

/// 启动流程调用的包装：与手动探测同一条代码路径，只是不需要返回快照。
pub async fn run_dns_probe_bg(app: &AppHandle, state: &AppState) -> Result<(), String> {
    run_dns_probe(app, state, true).await
}

/// 把探测出来的排序放到前面，用户手填的（不在候选池里的）留在后面。
///
/// 自动排序**只调池内项的相对顺序**，不删用户自己加的东西 —— 那些是用户
/// 明确想要的结果，探测器没资格替他把它们丢掉。
pub(crate) fn merge_ranked(
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
        .ok_or(util::STATE_UNAVAILABLE)?;

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
        .ok_or(util::STATE_UNAVAILABLE)?;
    let latest = latest.ok_or("还没有检查过客户端更新")?;

    let tmp = std::env::temp_dir().join(format!("xraytun-update-{}", std::process::id()));
    let tmp2 = tmp.clone();
    let latest2 = latest.clone();
    let total = latest.size;
    let reporter = core::progress_reporter(app.clone(), "客户端", latest.size);
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

    let reporter = core::progress_reporter(app.clone(), "核心", available.size);
    let meta = tauri::async_runtime::spawn_blocking(move || {
        xt_core::update::install_core(&available, &dir, Some(proxy), reporter)
    })
    .await
    .map_err(|e| format!("安装任务失败：{e}"))?
    .map_err(util::user_msg)?;

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

    let reporter = core::progress_reporter(app.clone(), "geo 数据", available.size);
    // ⚠️ **不要在收尾之前用 `?` 提早返回**：这条路径的**每个出口**都必须经过下面
    // 那个唯一收尾点。A17（task-153）就是这里漏的 —— geo 成功路径压根没有收尾点，
    // 于是界面永久显示「下载中，请勿关闭…」并且**升级按钮永久禁用**（只有重启才恢复）。
    let result = match tauri::async_runtime::spawn_blocking(move || {
        xt_core::update::install_geo(&available, &dir, Some(proxy), reporter)
    })
    .await
    {
        Ok(r) => r.map_err(util::user_msg),
        Err(e) => Err(format!("安装任务失败：{e}")),
    };

    state.with(|i| {
        // **唯一收尾点：无论成败都清进度**（界面据此恢复升级按钮）。
        i.update.finish_download();
        match &result {
            Ok(meta) => i.push_log(
                "app",
                "info",
                format!(
                    "geo 数据已更新到 {}（下次连接生效）",
                    meta.geo_tag.clone().unwrap_or_default()
                ),
            ),
            Err(e) => i.push_log("app", "error", format!("geo 数据更新失败：{e}")),
        }
        if result.is_ok() {
            i.update.latest_geo = None;
        }
    });
    result?;
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
    xt_core::update::revert_managed(&dir).map_err(util::user_msg)?;
    state.with(|i| {
        i.push_log("app", "info", "已回退到随包版本（核心与 geo）");
        i.update.latest_core = None;
        i.update.latest_geo = None;
    });
    build_snapshot(&app, &state).await
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

/// 当前进程是不是从某个 `.app` 里跑起来的。是的话返回那个 bundle 的路径。
///
/// 开发构建（`cargo run`）不满足这个条件 —— 那种情况不自动更新，
/// 因为「替换掉自己正在跑的那个目录」在开发场景下只会让人困惑。
pub(crate) fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // .../XrayTun.app/Contents/MacOS/xraytun-desktop
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().and_then(|e| e.to_str()) == Some("app")).then(|| bundle.to_path_buf())
}

/// 读一个 `.app` 的版本号。
pub(crate) fn bundle_version(app: &std::path::Path) -> Option<String> {
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
pub(crate) fn stage_app_update(
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
        let got = xt_core::update::sha256_file(&zip).map_err(util::user_msg)?;
        if want != got {
            return Err(format!("校验和不匹配：期望 {want}，实际 {got}"));
        }
    } else {
        return Err("这次 release 没有 SHA256SUMS.txt，拒绝安装".into());
    }

    let stage = tmp.join("stage");
    xt_core::update::unzip_tree(&zip, &stage).map_err(util::user_msg)?;

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

/// 更新状态里**不需要 `AppHandle`/`AppState`** 的那部分（纯函数，可单测）。
///
/// `core` 必须由调用方传入**已经算好的**那份 —— 这就是本卡要消掉的那次重复
/// `xray version` 进程 spawn（`build_snapshot` 原来算两遍）。
pub(crate) fn update_status_with(
    cached: &crate::state::UpdateStatus,
    current_app_version: &str,
    core: &CoreAvailability,
    core_managed: bool,
    meta: &xt_core::update::InstalledMeta,
) -> crate::state::UpdateStatus {
    let mut u = cached.clone();
    // 是否**确实**有新版：`latest_app` 有值只说明「查到了 GitHub 上的最新版」，
    // 你装的就是它时也有值。必须比较版本，否则界面永远显示「更新」按钮。
    u.app_update_available = u
        .latest_app
        .as_ref()
        .is_some_and(|a| xt_core::update::compare_versions(&a.version, current_app_version).is_gt());
    u.core_version = core.version.clone();
    u.core_managed = core_managed;
    u.core_managed_version = meta.core_version.clone();
    u.geo_tag = meta.geo_tag.clone();
    u.geo_installed_at = meta.geo_installed_at;
    u
}

/// 组装更新状态：当前生效的版本 + 上次检查的缓存。
///
/// `core` 由调用方传入（见 [`update_status_with`]）：这里**不再自己 spawn 核心**。
pub(crate) fn update_status(
    app: &AppHandle,
    state: &AppState,
    core: &CoreAvailability,
) -> crate::state::UpdateStatus {
    let root = state.store.root();
    let managed_dir = xt_core::update::managed_core_dir(root);
    let meta = xt_core::update::InstalledMeta::load(&managed_dir);
    let core_managed = managed_dir.join("xray").is_file();
    let current = app.package_info().version.to_string();

    state
        .with(|i| update_status_with(&i.update, &current, core, core_managed, &meta))
        .unwrap_or_default()
}

/// 读系统的登录项状态。读失败不是致命错误 —— 界面照常可用，
/// 只是那一栏显示「读取失败」，并把原因写出来。
pub(crate) fn login_item_state() -> crate::state::LoginItemState {
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

pub(crate) fn core_availability(app: &AppHandle, state: &AppState) -> CoreAvailability {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        /// 排名为空时不能把列表清掉 —— 那是「没测出来」，不是「都不要了」。
        #[test]
        fn merge_ranked_with_empty_ranking_keeps_current() {
            let pool = xt_core::dns_probe::DNS_POOL;
            let current = vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()];
            let out = merge_ranked(&current, &[], xt_core::dns_probe::DnsKind::Domestic, pool);
            assert_eq!(out, current);
        }

    // -----------------------------------------------------------------------
    // task-62 第 2 条：`build_snapshot` 每次调用 spawn 核心两次 → 一次
    //
    // 消重的结构性保证：`update_status_with` 的签名里没有 `AppHandle`、
    // 没有路径、没有 `Command` —— 它**没有能力**起进程，只能消费传进来的
    // `CoreAvailability`。
    // -----------------------------------------------------------------------

    fn an_available(version: &str) -> xt_core::update::Available {
        xt_core::update::Available {
            version: version.to_string(),
            published_at: "2026-09-21T00:00:00Z".into(),
            prerelease: false,
            download_url: "https://example.invalid/x.zip".into(),
            digest_url: None,
            size: Some(1),
        }
    }

    /// 用的是**传进来的** core 版本（证明这里不会再自己 spawn 一次）。
    #[test]
    fn update_status_uses_the_passed_in_core_version_without_spawning() {
        let cached = crate::state::UpdateStatus::default();
        let core = CoreAvailability {
            path: Some(std::path::PathBuf::from("/Applications/XrayTun.app/Contents/Resources/xray")),
            version: Some("Xray 26.9.9 (passed-in)".into()),
            ..Default::default()
        };
        let meta = xt_core::update::InstalledMeta {
            core_version: Some("26.1.0".into()),
            geo_tag: Some("geo-v1".into()),
            geo_installed_at: Some(123),
            ..Default::default()
        };

        let u = update_status_with(&cached, "0.8.30", &core, true, &meta);

        assert_eq!(u.core_version.as_deref(), Some("Xray 26.9.9 (passed-in)"));
        assert!(u.core_managed);
        assert_eq!(u.core_managed_version.as_deref(), Some("26.1.0"));
        assert_eq!(u.geo_tag.as_deref(), Some("geo-v1"));
        assert_eq!(u.geo_installed_at, Some(123));
    }

    /// 「有没有新版」是**逐段比较版本**算出来的，不是「查到最新版就有」。
    #[test]
    fn update_status_recomputes_app_update_availability_by_version() {
        let cached = crate::state::UpdateStatus {
            latest_app: Some(an_available("0.9.0")),
            app_update_available: true, // 缓存里是旧结论，必须被重算
            ..Default::default()
        };
        let core = CoreAvailability::default();
        let meta = xt_core::update::InstalledMeta::default();

        let same = update_status_with(&cached, "0.9.0", &core, false, &meta);
        assert!(
            !same.app_update_available,
            "已经是最新版时不得显示「更新」按钮"
        );

        let older = update_status_with(&cached, "0.8.30", &core, false, &meta);
        assert!(older.app_update_available, "落后时应当显示「更新」");
    }

    /// 没有查到最新版 → 一定没有「可更新」（不能因为缓存而复活）。
    #[test]
    fn update_status_without_latest_app_is_never_available() {
        let cached = crate::state::UpdateStatus {
            latest_app: None,
            app_update_available: true,
            ..Default::default()
        };
        let u = update_status_with(
            &cached,
            "0.8.30",
            &CoreAvailability::default(),
            false,
            &xt_core::update::InstalledMeta::default(),
        );
        assert!(!u.app_update_available);
    }

    // -----------------------------------------------------------------------
    // task-153（A17）：geo 更新后进度不收尾 ⇒ 永久「下载中」+ 升级按钮永久禁用
    // -----------------------------------------------------------------------

    /// **A17 行为级**：`finish_download()` 清掉进度 ⇒ 界面「正在下载」判据翻假、
    /// 升级按钮恢复可用。
    ///
    /// 界面判据就是 **`snapshot.update.progress !== null`**（`Settings.tsx:307` 的
    /// `downloading`）：它同时驱动「下载中，请勿关闭…」那条提示与按钮禁用。
    #[test]
    fn finish_download_clears_progress_so_the_update_button_comes_back() {
        let mut update = crate::state::UpdateStatus {
            progress: Some(crate::state::UpdateProgress {
                label: "geo 数据".into(),
                done_bytes: 8,
                total_bytes: Some(8),
            }),
            ..Default::default()
        };
        // 前置：正在下载 ⇒ 界面此刻显示「下载中，请勿关闭」并**禁用升级按钮**。
        assert!(update.progress.is_some(), "前置：进度在，界面才是「下载中」");
        update.finish_download();
        assert!(
            update.progress.is_none(),
            "清掉进度后界面才认为「没在下载」⇒ 升级按钮恢复可用（A17 的死路就在这里）"
        );
    }

    /// **A17 源码守卫（按站点 + 正向）**：geo 路径必须经过**唯一收尾点**清进度，
    /// 而且收尾必须发生在把错误往上抛**之前**、收尾前不许有 `?` 提早返回。
    ///
    /// 自带负例：把收尾点删掉（= A17 的原始实现）⇒ 判据必须翻假。
    #[test]
    fn geo_update_path_always_finishes_download_in_production_source() {
        let src = include_str!("snapshot.rs");
        let prod = src.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(src);
        assert!(
            geo_update_finishes_download(prod),
            "geo 更新路径必须在唯一收尾点清进度（否则永久「下载中」+ 按钮永久禁用）"
        );

        let broken = prod.replace("        i.update.finish_download();", "");
        assert_ne!(broken, prod, "负例 fixture 必须真的改到生产源码");
        assert!(
            !geo_update_finishes_download(&broken),
            "去掉收尾点必须被判据抓住（这正是 A17 的形状）"
        );

        // 负例 2：**收尾之前提早返回**（在 `let result = …` 里 `return Err(…)`）——
        // 这是「有人把 `?` 加回来」的等价形状，同样必须判假。
        let early = prod.replace(
            "        Ok(r) => r.map_err(util::user_msg),",
            "        Ok(r) => match r { Ok(m) => m, Err(e) => return Err(util::user_msg(e)) },",
        );
        assert_ne!(early, prod, "负例 2 fixture 必须真的改到");
        assert!(
            !geo_update_finishes_download(&early),
            "收尾前提早返回也必须被判据抓住"
        );
    }

    /// 判据：`install_geo_update` 体内 ①有收尾调用；②收尾在 `result?;` **之前**；
    /// ③**从开始报到收尾之间不许有任何 `?` / `return`**（那会跳过收尾 = A17 的形状）。
    fn geo_update_finishes_download(src: &str) -> bool {
        let Some(start) = src.find("pub async fn install_geo_update(") else {
            return false;
        };
        let rest = &src[start..];
        let end = rest.find("\n/// 回退到包内自带的版本").unwrap_or(rest.len());
        let body = &rest[..end];
        // 去行注释：判据不认注释里写的旧写法（task-75 的教训）。
        let code = body
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let (Some(reporting), Some(clear), Some(propagate)) = (
            code.find("progress_reporter("),
            code.find("i.update.finish_download()"),
            code.find("result?;"),
        ) else {
            return false;
        };
        let before_finish = &code[reporting..clear];
        clear < propagate
            && !before_finish.contains('?')
            && !before_finish.contains("return ")
    }
}
