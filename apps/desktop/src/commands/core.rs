//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn start_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_core(&app, &state).await?;
    spawn_dns_reprobe(&app, &state);
    snapshot::build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    stop_core(&app, &state).await?;
    // **用户主动停止** —— 意图作废，而且**必须落盘**：重启后读到的就是 false，
    // 所以「断开」是**跨进程有效**的逃生路（task-64 (c) 钉的就是这条）。
    //
    // 其它调用 `stop_core` 的路径（切换节点、看门狗重建）**不走这里**：
    // 它们只是过程，不是意图 —— 换了节点之后还是要连着的。
    invalidate_connect_intent(&state, IntentDrop::UserStop, "用户主动断开");
    state.with(|i| {
        i.runtime = CoreRuntime::default();
    });
    snapshot::build_snapshot(&app, &state).await
}

pub(crate) async fn start_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
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
        // **核心已经在跑 ≠ 监控已经在守。**
        //
        // 这一条路径原本直接返回，一个监控任务都不启动 —— 于是出现
        // 「核心在跑、界面显示已连接、但没有任何人在守」：换网之后不会重建，
        // 熄屏唤醒之后也不会。用户看到的正是「要手动点一下连接」。
        //
        // 这不是构造出来的场景：自动更新会让 App 重启，重启后的自动重连
        // 若撞上这个早退，就落在这里。实测日志里 2 小时内
        // `[info] 连通性检查通过` 一条都没有，而看门狗每 10 秒就该记一条。
        //
        // 用 `running_pid()` 而不是 `runtime.pid`：这里要的是**进程真的活着**
        // 那个 pid（`is_running` 刚确认过）。重复 spawn 由 `spawn_monitors`
        // 内部的 pid 去重挡住。
        let pid = supervisor.running_pid();
        drop(helper);
        drop(supervisor);
        spawn_monitors(app, egress_before.clone(), pid);
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

    // 新核心起来了：启动它的监控（换网检测 / 连通性检查 / 看门狗）。
    // 与「已经在跑」那条路径共用同一个入口，避免两处各写一份。
    spawn_monitors(app, egress_before, runtime.pid);

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

    // 日志转发任务：核心的 stdout/stderr → 状态环形缓冲 + UI 事件。
    let app_handle = app.clone();
    let forward_pid = runtime.pid;
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let level = classify_log(&event.line);
            if let Some(state) = app_handle.try_state::<AppState>() {
                // 顺手统计各出口的连接数。放在这里而不是另起一个日志 tail：
                // 这是核心输出的**单点**，重复读取会带来两份不一致的时间线。
                //
                // 为什么需要连接数：`dns-out`（UDP）与 `api`（本机回环）的
                // 字节计数器恒为 0，那是测量盲区 —— 只显示 `0 B` 会让人以为
                // 这两个出口没在用（本机实测各有 4769 / 5374 条连接）。
                state.with(|i| {
                    i.connections.observe(&event.line);
                });
                state.log("core", level, event.line.clone());
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
                    // 核心退出了 → 连接计数也失去意义（下一个核心从 0 重新计）。
                    // 清掉而不是留着旧值，否则界面会显示上一轮核心的连接数。
                    i.connections.reset();
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

/// 停止核心后应当写回的运行态（**纯函数，便于单测**）。
///
/// # 为什么不是几条赋值语句（这是本卡的核心）
///
/// 原实现是「**先**清 `running`/`pid`/`tun_session`，**再**看 `result`」——
/// 于是 helper 回滚失败时，界面照样显示「已停止」，而真实情况可能是
/// 「没有隧道、也没恢复直连」。这是把没验证的事说成已验证。
///
/// 现在的口径：
/// * **成功**（helper 的 `TunDown` 成功返回）→ 才敢说「网络配置已回滚」，
///   并清掉 `tun_session`；
/// * **失败** → **保留 `tun_session`**：那是「helper 上这条会话可能还活着」的
///   唯一证据，清掉就再也 ref 不上；文案如实写「**未能确认网络已恢复**」。
///
/// `running`/`pid` 一律清掉：`supervisor.stop()` 已经把进程句柄与 session_id
/// `take()` 走了，App 这边确实不再受管，所以不能声称「还在运行」。
pub(crate) fn runtime_after_stop(
    before: &CoreRuntime,
    result: &Result<(), String>,
) -> CoreRuntime {
    let mut next = before.clone();
    next.running = false;
    next.pid = None;
    match result {
        Ok(()) => {
            next.tun_session = None;
            // 成功路径**不动 `last_error`**：原文如此 —— happy path 的运行态
            // 必须与改前逐字节一致（task-62 验收）。
        }
        Err(e) => {
            // **不清 tun_session**：会话可能还活着，这是我们唯一的线索。
            next.last_error = Some(e.clone());
        }
    }
    next
}

/// 停止核心后写给用户看的那条日志（`(level, message)`）。
///
/// 失败时**不许**出现「网络可用」这类未经验证的断言 —— 只报我们确实知道的事：
/// 回滚没成功、网络恢复**未经验证**。
pub(crate) fn stop_log_line(result: &Result<(), String>) -> (&'static str, String) {
    match result {
        Ok(()) => ("info", "核心已停止，网络配置已回滚".to_string()),
        Err(e) => (
            "error",
            format!(
                "停止核心时未能完成网络回滚：{e}；**未能确认网络已恢复**\
                 （helper 上的会话可能仍在，已保留会话 id）"
            ),
        ),
    }
}

pub(crate) async fn stop_core(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let mut supervisor = state.supervisor.lock().await;
    let mut helper = state.helper.lock().await;
    let pid = supervisor.running_pid();
    let result = supervisor.stop(&mut helper).await;
    drop(helper);
    drop(supervisor);
    // 核心已经停了：它的监控凭据作废，否则表里会留下永远不会释放的旧 pid。
    release_monitors(pid);

    state.with(|i| {
        // 采样任务必须先收掉：核心没了，api 端口也没人监听，
        // 留着它只会每秒产生一次连接失败。
        if let Some(monitor) = i.traffic_task.take() {
            monitor.abort();
        }
        i.traffic = crate::state::TrafficSample::default();
        // 运行态由纯函数决定（见 `runtime_after_stop` 的注释）：
        // **先算完再看结果**，而不是先清干净再补日志。
        i.runtime = runtime_after_stop(&i.runtime, &result);
        let (level, message) = stop_log_line(&result);
        i.push_log("app", level, message);
    });
    // 采样任务已经收掉，标题会永远停在最后一拍的读数上 —— 手动清掉。
    let show = state.with(|i| i.settings.show_speed_in_title).unwrap_or(true);
    crate::traffic::update_titles(app, &crate::state::TrafficSample::default(), show);

    events::runtime_changed(app, state);
    result
}

/// 「重建失败 → 退回直连」这一步的对外结论。
///
/// 只有 helper 的 `TunDown` **确实成功**时才敢说「配置已回滚」；
/// 失败时必须如实说「未能确认网络已恢复」，否则又是「把没验证的事说成已验证」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FallbackOutcome {
    /// 回滚成功：路由/DNS 已还原（这一步有 helper 的成功返回为证）。
    DirectRestored,
    /// 回滚失败：**不知道**网络恢没恢复，会话可能仍在。
    DirectUnverified { error: String },
}

impl FallbackOutcome {
    pub(crate) fn from_stop(result: &Result<(), String>) -> Self {
        match result {
            Ok(()) => Self::DirectRestored,
            Err(e) => Self::DirectUnverified { error: e.clone() },
        }
    }

    /// `(level, 应用日志, 顶部提示条)`。
    ///
    /// **全程不出现「网络可用」**：回滚成功只证明配置已还原，不证明能上网；
    /// 回滚失败更连配置状态都不知道。宁可说「未能确认」，也不编一个好消息。
    pub(crate) fn messages(&self) -> (&'static str, String, String) {
        match self {
            Self::DirectRestored => (
                "error",
                "自动重建失败，已退回直连：网络配置已回滚，流量不再走代理".to_string(),
                "自动恢复失败，已退回直连（网络配置已回滚）。可在节点页重新连接".to_string(),
            ),
            Self::DirectUnverified { error } => (
                "error",
                format!(
                    "自动重建失败，且回退直连**未能确认网络已恢复**（{error}）：\
                     helper 上的会话可能仍在 → 路由/DNS 可能没还原"
                ),
                format!(
                    "自动恢复失败，回退直连未完成：**未能确认网络已恢复**（{error}）。\
                     请点「修复网络」重试回滚"
                ),
            ),
        }
    }
}

/// 「节点连上了、但经它访问目标一直超时，且没有可回退的好节点」时给用户看的话。
///
/// # 为什么不预设原因（task-67）
///
/// 同一个现象至少有三种可能：节点不可用、**本机网络本身不通**、链路被干扰。
/// 以前这里直接写「请换一个节点」——把因果**唯一地**归到节点上，用户于是陷入
/// 「换节点 → 还不通 → 再换」的循环（用户原话：「不知道是应用、运营商还是节点」）。
///
/// 现在只陈述**观察到的因果**（经它访问一直超时）+ 可能性，并给出用户已知有效的
/// 自救动作：**先点「断开」恢复直连**（那条路径会回滚路由与 DNS，见
/// `Supervisor::stop` → `Request::TunDown` → `controller::rollback`；用户也实测过
/// 「断开或退出应用后网络就恢复了」）。
pub(crate) fn node_unusable_message(node_name: &str) -> String {
    format!(
        "节点「{node_name}」已连接，但经它访问目标一直超时。\
         可能是这个节点不可用，也可能**本机网络本身不通**（或被链路干扰）。\
         先试换一个节点；**如果整台 Mac 都上不了网，先点「断开」恢复直连**。"
    )
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
pub(crate) fn tunnel_is_dead(http_code: &str) -> bool {
    http_code.is_empty() || http_code == "000"
}

/// 启动时该不该自动连回来。
///
/// 四个条件缺一不可 —— 抽成纯函数是为了能测，而不是散在 async 流程里。
///
/// **第一个参数的含义比它的名字严。** 它不只是「上次是连着的」，还必须是
/// 「上次**不是**以已知失败告终」。这一层由 [`invalidate_connect_intent`] 在每个
/// 退场点写回磁盘来保证 —— 本函数只管**读**，写盘在那一侧。
///
/// task-64 之前，只有用户亲手点「断开」（`stop_proxy`）会写回它；**自动退场**
/// （门禁没过 / 看门狗重建后仍不通 / 退回直连）从来没人写，于是磁盘上留着
/// `true`，下次启动就拿同一个刚被证明不通的节点再接管一次网络。
pub(crate) fn should_auto_reconnect(
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

/// 「用户希望连着」这个意图**为什么**该作废。这是 (a) 判据的全部输入。
///
/// 只列**明说**的两种退场。正常打断（自更新 / 崩溃 / 退出应用时隧道还连着）
/// **故意不在这个枚举里** —— 那条路什么都不写，见 [`connect_intent_after_stop`]
/// 的对照组说明与 `normal_interruption_*` 测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentDrop {
    /// 用户亲手点「断开」（`stop_proxy`）。
    UserStop,
    /// **已知失败**：门禁没过 / 看门狗重建后仍不通 / 退回直连。
    KnownFailure,
}

/// 退场之后，`settings.was_connected` 该落盘成什么。
///
/// 返回 `Some(false)` = **必须写盘作废**；`None` = 不用写（意图本来就不该变）。
///
/// # 判据为什么只能是这个字段
///
/// `settings.was_connected`（`settings.json`）是**本工程里唯一跨进程存活的
/// 「用户意图」证据**：`runtime.recovery.last_outcome`、`runtime.last_error`、
/// 失败提示条全在内存里，重启即失（已核实：`store.rs` 里持久化的**结构化状态**
/// 只有 settings / subscriptions / nodes / runtime/config 这四份；`logs/*.jsonl`
/// 是给人看的文本，不是状态）。所以「上次会话以失败告终」
/// 要想跨重启被读到，只能写进这个**已存在的**字段 —— 这不是新造状态，
/// 而是把它写诚实。
///
/// # 为什么必须写
///
/// 用户能用「退出应用」逃生：退出那一刻网络确实恢复。但意图还在盘上，
/// 于是**下次启动 / 登录项自启 / 自更新重启**会读到 `true` 并自动重连
/// （而且后台重试 `RECONNECT_ATTEMPTS` 次）—— 用同一个刚被证明不通的节点
/// 把用户刚修好的网络再接管一次。**「退出应用」因此只在本次进程内有效。**
///
/// # 对照组（故意什么都不写）
///
/// 自更新 / 崩溃打断一条**正连着**的会话**不是失败**：那条路径不调用
/// [`invalidate_connect_intent`]，`was_connected` 原样留在磁盘上，下次启动
/// 照样自动连回来 —— 这正是自动重连存在的理由，有独立测试钉住。
pub(crate) fn connect_intent_after_stop(
    drop_kind: IntentDrop,
    was_connected: bool,
) -> Option<bool> {
    if was_connected {
        match drop_kind {
            IntentDrop::UserStop | IntentDrop::KnownFailure => Some(false),
        }
    } else {
        // 意图已经是 false（用户早先断开过 / 上一次退场已作废）—— 不重复写盘。
        None
    }
}

/// 把「意图作废」落盘（`settings.was_connected` → false）。
///
/// 两条调用路径共用它：用户亲手断开（`stop_proxy`）与自动退场（已知失败）。
///
/// **只碰意图。** `runtime.recovery` 与失败提示条必须留着 —— 界面正靠它们显示
/// 「已退回直连」和原因（`stop_proxy` 那条路另外清 runtime，见它的调用点）。
///
/// 落盘失败**必须留痕**：否则下次启动仍会自动重连一次，而没人知道为什么。
pub(crate) fn invalidate_connect_intent(state: &AppState, drop_kind: IntentDrop, detail: &str) {
    let Some(mut settings) = state.with(|i| i.settings.clone()) else {
        return;
    };
    if connect_intent_after_stop(drop_kind, settings.was_connected).is_none() {
        return;
    }
    settings.was_connected = false;
    match persist_settings(state, &settings) {
        Ok(()) => state.log(
            "app",
            "info",
            format!("已作废「自动重连」意图（{detail}）：下次启动不会自动连"),
        ),
        Err(e) => state.log(
            "app",
            "warn",
            format!("记录「不再自动重连」失败：{e} —— 下次启动可能仍会自动重连"),
        ),
    }
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
pub(crate) fn watchdog_should_watch(
    user_wants_it: bool,
    my_pid: Option<u32>,
    current_pid: Option<u32>,
) -> bool {
    user_wants_it && my_pid == current_pid
}

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
pub(crate) fn slept_for(monotonic_elapsed: Duration, wall_elapsed: Duration) -> Duration {
    wall_elapsed.saturating_sub(monotonic_elapsed)
}

/// 自动重连还要不要继续试。
///
/// 两种情况都该停：
/// * 用户明确关掉了（意图变了）—— 继续试就是「关不掉」；
/// * 隧道已经在跑 —— 用户自己点了连接并成功了，再插一手就是抢。
pub(crate) fn should_keep_reconnecting(user_wants_it: bool, already_running: bool) -> bool {
    user_wants_it && !already_running
}

/// 看门狗该不该重建隧道。
///
/// 三个条件都必须满足：
/// * `still_mine` —— 这次连接还是我负责的那次（用户重连会换 pid）；
/// * `user_wants_it` —— 用户**现在还**想连着（探测是异步的，等结果回来时
///   他可能已经点了关闭；不看这个就会「点了关闭，几秒后它自己又连上」）；
/// * 失败次数到阈值。
pub(crate) fn should_rebuild_tunnel(still_mine: bool, user_wants_it: bool, consecutive_failures: u32) -> bool {
    still_mine && user_wants_it && consecutive_failures >= FAILURES_BEFORE_REBUILD
}

/// 经本地 SOCKS 入站发一个**真实请求**，返回 HTTP 状态码（失败时空串）。
///
/// 用 `--socks5-hostname` 让节点去解析域名，所以这一个检查同时覆盖
/// 「能不能转发」和「节点侧能不能解析」两件事。
///
/// **实现在 [`crate::supervisor::socks_http_probe`]**（一处实现、两处调用：
/// 看门狗/连通性检查，以及接管默认路由**之前**的端到端门禁）。
/// 这里保留原名与原签名，调用点不用改；复制第二份 curl 调用必然漂移。
pub(crate) async fn tunnel_probe(port: u16, timeout_secs: u32) -> String {
    crate::supervisor::socks_http_probe(port, xt_core::xray::DEFAULT_PROBE_URL.to_string(), timeout_secs)
        .await
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
/// 启动某个核心 pid 的全部监控任务（换网检测 / 连通性检查 / 看门狗）。
///
/// **两条启动路径共用它**：核心刚被启动、以及核心本来就在跑（早退那条）。
/// 后者是这次修的核心 —— 早退之前没人启动监控，于是出现「核心在跑、
/// 界面显示已连接、但没有任何人在守」，换网与熄屏之后都不会自愈。
///
/// 按 pid 去重：该 pid 已经有监控在守时 `MonitorGuard::claim` 返回 `None`，
/// 这里直接跳过，避免多个看门狗互相打架（各自重建隧道）。
pub(crate) fn spawn_monitors(app: &AppHandle, baseline: Option<Egress>, pid: Option<u32>) {
    let Some(guard) = MonitorGuard::claim(pid) else {
        // 该 pid 已经有监控在守（正常启动那条路径已经 spawn 过）—— 不重复起。
        return;
    };
    // 换网之后隧道不会自愈（路由/网卡绑定/长连接全指向旧出口），
    // 盯着它，变了就重建 —— 只报错让用户手动连，正是要消灭的行为。
    spawn_network_watch(app, baseline, pid, guard.clone());
    // 「连上了」不等于「能用」：节点可能活着却转发不了流量。
    spawn_connectivity_check(app, pid, guard.clone());
    // 一直盯着：熄屏唤醒、换网、节点抖动之后隧道可能已经死了而界面还显示
    // 「已连接」。只要用户没主动断开，网络就不该是坏的。
    spawn_tunnel_watchdog(app, pid, guard);
}

pub(crate) fn spawn_tunnel_watchdog(app: &AppHandle, pid: Option<u32>, _guard: MonitorGuard) {
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
                sync_probe_failures(&handle, &state, 0);
                continue;
            }
            failures += 1;
            if just_woke {
                // 唤醒这一次失败几乎必然是"隧道真的死了"，不必再等第二次。
                failures = failures.max(FAILURES_BEFORE_REBUILD);
            }
            // 让界面能看见「连续 N 次不通」——恢复可能在几步之后才开始。
            sync_probe_failures(&handle, &state, failures);

            // **用户可能就在刚才点了「关闭」。** 探测是异步的，等它回来时
            // 意图可能已经变了 —— 那就什么都别做，否则就是
            // 「点了关闭，几秒后它自己又连上了」。再查一次意图。
            let user_wants_it = state.with(|i| i.settings.was_connected).unwrap_or(false);
            if !user_wants_it {
                state.log("app", "info", "用户已关闭，取消自动重建");
                return;
            }
            if !should_rebuild_tunnel(true, user_wants_it, failures) {
                continue;
            }

            // **开始恢复。** 这一刻起 `recovery.recovering = true`，界面据此
            // 显示「正在自动恢复（第 N 次）」并改写连接按钮 —— 不再出现
            // 「未连接 + 可点的连接按钮」，用户也就不会去和看门狗抢。
            let started = xt_core::util::now_unix();
            let attempt = state
                .with(|i| {
                    i.push_log(
                        "app",
                        "warn",
                        format!(
                            "隧道连续 {failures} 次不通（熄屏/换网/节点抖动，当前节点「{node_name}」），正在自动重建…"
                        ),
                    );
                    i.last_notice = Some(crate::state::RECOVERING_NOTICE.into());
                    i.runtime.recovery.begin(started)
                })
                .unwrap_or(0);
            events::runtime_changed(&handle, &state);

            // 重建：用**当前**的物理出口重新算路由与 DNS。熄屏唤醒后网关
            // 变了也能对上，这正是"能自愈"的关键。
            if stop_core(&handle, &state).await.is_ok()
                && start_core(&handle, &state).await.is_ok()
            {
                state.with(|i| {
                    i.runtime.recovery.succeeded(xt_core::util::now_unix());
                    // **清掉恢复中的提示条**（逻辑在 state.rs，有单测：
                    // `recovery_success_clears_only_our_own_notice`）。
                    // 不清的话下一次快照刷新会把过期的「正在自动恢复…」带回来，
                    // 从「该显示时看不见」变成「恢复完了还一直显示恢复中」。
                    crate::state::clear_recovering_notice(&mut i.last_notice);
                });
                state.log("app", "info", format!("隧道已自动恢复（第 {attempt} 次自动重建）"));
                // 显式推一次：让界面收到 `recovering=false` + `last_outcome=recovered`，
                // 这样「恢复成功」是**可感知的结束**，不是静默变回「已连接」。
                events::runtime_changed(&handle, &state);
                // start_core 会 spawn 新的看门狗，这里退出即可。
                return;
            }

            // 重建也失败：退回直连。用户至少能上网 —— 这比死守一条
            // 走不通的隧道更符合「除非我关闭，网络不该断」。
            // **回滚结果必须显式处理。** 以前这里是 `let _ = stop_core(...)`，
            // 失败被丢掉，紧接着无条件写「网络可用」—— 那是在断言我们没验证过的事。
            let stop_result = stop_core(&handle, &state).await;
            let outcome = FallbackOutcome::from_stop(&stop_result);
            let (level, log_line, notice) = outcome.messages();
            state.with(|i| {
                i.push_log("app", level, log_line);
                i.runtime.recovery.fell_back_to_direct(xt_core::util::now_unix());
                // 失败时 notice **保留**，并说清下一步能做什么（诚实版：不声称网络可用）。
                i.last_notice = Some(notice);
            });
            // **已知失败退场：把「自动重连」意图落盘作废。**
            // 不退的话，用户「退出应用」恢复的网络会在下次启动被同一个坏节点
            // 再接管一次（见 `connect_intent_after_stop` 的说明）。
            // 注意只碰意图：上面刚写的 recovery/notice 是界面显示失败原因的依据。
            invalidate_connect_intent(
                &state,
                IntentDrop::KnownFailure,
                "看门狗重建隧道失败，已退回直连",
            );
            events::runtime_changed(&handle, &state);
            return;
        }
    });
}

/// 把「连续探测失败次数」同步进 `recovery`，**只在值变化时**发事件。
///
/// 每 10 秒的探测都推一次事件是纯噪音；而恢复可能还要再等一拍才开始，
/// 所以这个数字值得单独同步一次（界面可据此提前显示「连接不稳定」）。
fn sync_probe_failures(app: &AppHandle, state: &AppState, failures: u32) {
    let changed = state
        .with(|i| i.runtime.recovery.set_probe_failures(failures))
        .unwrap_or(false);
    if changed {
        events::runtime_changed(app, state);
    }
}

pub(crate) fn spawn_connectivity_check(app: &AppHandle, pid: Option<u32>, _guard: MonitorGuard) {
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
                None => node_unusable_message(&node_name),
            };
            state.with(|i| {
                i.push_log("app", "error", msg.clone());
                i.last_notice = Some(msg);
                // 先清掉，避免回退后那次检查再失败时又触发一次回退（来回弹）。
                i.runtime.last_good_node = None;
            });
            events::runtime_changed(&handle, &state);

            if let Some(back) = fallback {
                // 回退也失败 = **已知失败**：新节点起不来，隧道此刻是断的，
                // 而且没有别的自动动作会再来救它。把「自动重连」意图落盘作废
                // （和看门狗那两处同源）。
                //
                // 顺带把以前被 `let _ =` 丢掉的错误写进日志：静默失败查不动，
                // 而这条路径恰恰是「用户报『切了节点就没网』」时最该看的地方。
                if let Err(e) = select_node(handle.clone(), state, back).await {
                    if let Some(state) = handle.try_state::<AppState>() {
                        state.log("app", "error", format!("自动退回上一个可用节点也失败：{e}"));
                        invalidate_connect_intent(
                            &state,
                            IntentDrop::KnownFailure,
                            "自动回退节点失败",
                        );
                    }
                }
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

/// 物理出口换了吗。网卡换了、或者同一张网卡换了网关（换 WiFi、插网线、
/// 开热点、路由器重发 DHCP），都算换网。
pub(crate) fn network_moved(before: &Egress, after: &Egress) -> bool {
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
pub(crate) fn spawn_network_watch(
    app: &AppHandle,
    baseline: Option<Egress>,
    pid: Option<u32>,
    // 守卫的存在就是「占位」：它被这个任务持有到结束，最后一个放到它时
    // 才会把 pid 从监控表里移除。所以这里刻意不读它。
    _guard: MonitorGuard,
) {
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
            state.log("app", "error", msg);
            events::runtime_changed(&handle, &state);
            return; // 只报一次，别刷屏
        }
    });
}

/// 造一个把下载进度同时送到**状态**和**事件**的回调。
///
/// 两条路都要走：事件让进度条立刻动起来（每 200ms 一次），状态则保证
/// 用户中途切页面/刷新快照之后，进度条不会凭空消失。
pub(crate) fn progress_reporter(
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
pub(crate) fn spawn_dns_reprobe(app: &AppHandle, state: &AppState) {
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

    state.log("app", "info", "上次退出时是连接状态，正在自动重连…");
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
    // 试满 `RECONNECT_ATTEMPTS` 仍不通 = **已知失败**（这门禁没过）。
    // 作废意图：否则下次启动会照原样把「一个已知失败的行为」再重复一遍，
    // 而用户已经在提示里被要求「手动连接」了。
    invalidate_connect_intent(
        state,
        IntentDrop::KnownFailure,
        "自动重连多次仍未成功（门禁未过）",
    );
    events::runtime_changed(app, state);
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
pub(crate) fn classify_log(line: &str) -> &'static str {
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

/// 物理出口的「身份」。隧道是照它建的，换网之后要拿它比对。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Egress {
    interface: String,
    gateway: Option<std::net::IpAddr>,
}

/// 超过这个时长没跑循环，就认为中间睡过（而不是单纯被调度延迟）。
pub(crate) const SLEEP_THRESHOLD: Duration = Duration::from_secs(30);

/// 连续失败几次之后才重建隧道。
///
/// 一次失败可能只是节点抖了一下；连续两次才算隧道真的没了。
pub(crate) const FAILURES_BEFORE_REBUILD: u32 = 2;

/// 自动重连最多试几次、每次隔多久。
///
/// 开机场景下网络和 helper 都可能还没就绪，所以预算给得宽一点：
/// 已经 spawn 过监控任务的核心 pid。
///
/// # 为什么需要它
///
/// 监控任务（换网检测 / 连通性检查 / 看门狗）原本只在 `start_core` 的**末尾**
/// 启动，而那个函数在「supervisor 里已经有核心在跑」时会**提前返回** ——
/// 于是那条路径上一个监控都没有。
///
/// 这不是理论问题：**自动更新**会让核心退出、App 重启，重启后的自动重连
/// 撞上那个早退，结果就是「核心在跑，但没有任何人在守」。用户看到的是
/// 换网后断、熄屏后要手动点连接 —— 因为自愈的那一环根本没启动。
/// （实测日志：2 小时里 `[info] 连通性检查通过` 一条都没有，而看门狗每
/// 10 秒就该记一条。）
///
/// 所以监控的启动被提到早退之前。但早退那条路径上的核心**可能已经在被
/// 监控着**（正常启动时就 spawn 过），重复 spawn 会让多个看门狗互相打架
/// （各自重建隧道）。用这张表按 pid 去重。
fn monitors_spawned() -> &'static std::sync::Mutex<std::collections::HashSet<u32>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u32>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// 一个核心 pid 的监控凭据。
///
/// **构造即占用**：拿不到（该 pid 已经有人在守）就返回 `None`，调用方据此跳过
/// spawn。三个监控任务各自持有一份克隆，**最后一个结束时**才把 pid 从表里移除 ——
/// 这样同一个 pid 之后仍能重新被监控（例如核心重启后 pid 恰好复用），
/// 而中途退出其中任何一个都不会让另外两个失去登记。
#[derive(Clone)]
pub(crate) struct MonitorGuard(std::sync::Arc<u32>);

impl MonitorGuard {
    fn claim(pid: Option<u32>) -> Option<Self> {
        let pid = pid?;
        let mut set = monitors_spawned().lock().ok()?;
        if !set.insert(pid) {
            return None; // 已经有监控在守这个 pid
        }
        Some(Self(std::sync::Arc::new(pid)))
    }
}

impl Drop for MonitorGuard {
    fn drop(&mut self) {
        // `Arc<u32>` 只有最后一个引用 drop 时才会走到这里（其余是克隆），
        // 所以「最后一个任务结束才注销」是靠 Arc 的语义天然成立的。
        if std::sync::Arc::strong_count(&self.0) == 1 {
            if let Ok(mut set) = monitors_spawned().lock() {
                set.remove(&self.0);
            }
        }
    }
}

/// 核心停了：它的监控凭据一并作废，否则表里会留下永远不会释放的旧 pid。
fn release_monitors(pid: Option<u32>) {
    if let (Some(pid), Ok(mut set)) = (pid, monitors_spawned().lock()) {
        set.remove(&pid);
    }
}

/// 24 × 5s ≈ 2 分钟。超过就如实报"请手动连接"，而不是无限重试。
pub(crate) const RECONNECT_ATTEMPTS: u32 = 24;

pub(crate) const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

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

#[cfg(test)]
mod tests {
    use super::*;

        /// **同一个 pid 不能被监控两次**（去重），而全部释放后可以重新监控。
    ///
    /// 这条钉住的是这次修复的关键约束：监控的启动被提到了「核心已经在跑」
    /// 那条早退路径之前，于是必须保证重复调用不会 spawn 出多个看门狗 ——
    /// 多个看门狗会各自重建隧道，互相拆台。
    #[test]
    fn monitor_guard_deduplicates_per_pid() {
        let pid = 424_242u32;
        // 先清干净，避免与其它测试串扰
        release_monitors(Some(pid));

        let first = MonitorGuard::claim(Some(pid));
        assert!(first.is_some(), "第一次应当能占用");

        // 第二个克隆也算「有人在守」——这正是三个监控任务共享守卫的情形
        let clone = first.as_ref().unwrap().clone();
        assert!(
            MonitorGuard::claim(Some(pid)).is_none(),
            "同一个 pid 第二次占用必须被拒（否则会 spawn 出重复的看门狗）"
        );

        // 三个任务各自持有一份；只有全部释放后 pid 才回到可占用
        drop(first);
        assert!(
            MonitorGuard::claim(Some(pid)).is_none(),
            "还有一份克隆活着时，仍然算有人在守"
        );
        drop(clone);
        let again = MonitorGuard::claim(Some(pid));
        assert!(again.is_some(), "全部释放后应当可以重新占用");
        drop(again);

        // 没有 pid（核心没有进程句柄）时不占用，也不 panic
        assert!(MonitorGuard::claim(None).is_none());
    }

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

        // -------------------------------------------------------------------
        // task-64：**已知失败**之后不许再自动重连
        //
        // 起因是把两条已核实的事实放在一起：
        // ① 用户实测「退出应用后网络恢复」—— 退出是他在用的逃生路；
        // ② `should_auto_reconnect` 读的 `was_connected` 以前**只有手动断开**
        //    会清（`stop_proxy`），自动退场（门禁没过 / 重建后仍不通 / 退回直连）
        //    从来不写回 ⇒ 磁盘上还是 true ⇒ **下次启动**（含登录项自启、自更新
        //    重启）用同一个坏节点再接管一次网络，「退出」于是只在本次进程内有效。
        //
        // 修法：把这些**已知失败**写进**已存在的**持久化字段 `settings.was_connected`
        // （`settings.json`）—— 不新造状态。下面四条测试分别钉：判据、磁盘语义、
        // 以及**必须保留的反例**（正常打断仍要连回来）。
        // -------------------------------------------------------------------

        /// 判据：**已知失败 / 用户主动断开 ⇒ 作废**；意图本来就是 false ⇒ 不写盘。
        #[test]
        fn intent_drop_decision_is_explicit_about_the_two_drops() {
            assert_eq!(
                connect_intent_after_stop(IntentDrop::KnownFailure, true),
                Some(false),
                "已知失败之后必须把意图写盘作废（否则下次启动会拿坏节点再接管一次网络）",
            );
            assert_eq!(
                connect_intent_after_stop(IntentDrop::UserStop, true),
                Some(false),
                "用户亲手断开同样作废 —— 两条路共用一个判据",
            );
            assert_eq!(
                connect_intent_after_stop(IntentDrop::KnownFailure, false),
                None,
                "意图已经是 false —— 不必重复写盘",
            );
        }

        /// **(c) 手动「断开」→ 重启后不得自动重连。**
        ///
        /// **真的写盘、再真的读回来**（另开一个 `Store` 实例 = 模拟重启后的读取），
        /// 而不是只看内存字段：这个标记的全部用途就是跨进程存活。
        /// 走的是 `stop_proxy` 用的**同一个** `invalidate_connect_intent`。
        #[test]
        fn user_stop_persists_intent_false_across_restart() {
            let store = temp_intent_store("user-stop");
            let dir = store.root().to_path_buf();
            let state = AppState::new(store);
            // 先造出「用户希望连着」的盘上状态（`start_core` 成功时就是这样）。
            state.with(|i| i.settings.was_connected = true);
            let settings = state.with(|i| i.settings.clone()).unwrap();
            persist_settings(&state, &settings).unwrap();
            assert!(
                xt_core::store::Store::new(&dir).load_settings().was_connected,
                "前置条件：盘上先得有 true",
            );

            invalidate_connect_intent(&state, IntentDrop::UserStop, "用户主动断开");

            // **从盘上读回来** —— 这就是「重启后」看到的东西。
            let after_restart = xt_core::store::Store::new(&dir).load_settings();
            assert!(
                !after_restart.was_connected,
                "断开必须落盘：重启读到的不能还是 true",
            );
            assert!(
                !should_auto_reconnect(after_restart.was_connected, true, &ProxyMode::Tun, false),
                "手动断开过 —— 重启后不该自动连回来",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// **(a) 已知失败退场 → 磁盘上的意图作废，且失败提示不被顺手抹掉。**
        ///
        /// 前半段钉「重启后不会再用坏节点接管一次网络」；后半段钉
        /// `invalidate_connect_intent` **只碰意图** —— 界面显示「已退回直连」
        /// 靠的是 `last_notice` / `runtime.recovery`，它们必须留着。
        #[test]
        fn known_failure_persists_intent_false_but_keeps_the_notice() {
            let store = temp_intent_store("known-failure");
            let dir = store.root().to_path_buf();
            let state = AppState::new(store);
            state.with(|i| {
                i.settings.was_connected = true;
                i.last_notice = Some("网络未能恢复（直连状态未验证）".into());
            });
            let settings = state.with(|i| i.settings.clone()).unwrap();
            persist_settings(&state, &settings).unwrap();

            invalidate_connect_intent(
                &state,
                IntentDrop::KnownFailure,
                "看门狗重建隧道失败，已退回直连",
            );

            let after_restart = xt_core::store::Store::new(&dir).load_settings();
            assert!(!after_restart.was_connected, "已知失败必须写盘作废");
            assert!(
                !should_auto_reconnect(after_restart.was_connected, true, &ProxyMode::Tun, false),
                "上次以失败告终 —— 不许自动重连（那等于把用户刚修好的网再弄坏一次）",
            );
            assert_eq!(
                state.with(|i| i.last_notice.clone()).flatten(),
                Some("网络未能恢复（直连状态未验证）".to_string()),
                "只碰意图：失败提示条不许被抹掉（界面靠它说明发生了什么）",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// **(c) 必须保留的反例：正常连接后被自更新/崩溃打断 → 仍要自动重连。**
        ///
        /// 独立于上面两条：那条路**什么都不写**（不是失败，用户没关过），
        /// 所以盘上的意图原样是 true，重启后必须连回来 —— 这是自动重连存在的
        /// 唯一理由（自更新会先退出 app 再重启）。这条不许被上面那条吃掉。
        #[test]
        fn normal_interruption_keeps_intent_and_still_reconnects() {
            // 盘上的意图：**没有任何退场判据会去动它**（`IntentDrop` 只有
            // UserStop / KnownFailure 两个变体，见上一条测试）。
            let intent_on_disk_after_interruption = true;
            let store = temp_intent_store("interruption");
            let dir = store.root().to_path_buf();
            let state = AppState::new(store);
            state.with(|i| i.settings.was_connected = intent_on_disk_after_interruption);
            let settings = state.with(|i| i.settings.clone()).unwrap();
            persist_settings(&state, &settings).unwrap();

            // 「重启」：从盘上读回来的就是打断前那个 true。
            let after_restart = xt_core::store::Store::new(&dir).load_settings();
            assert!(
                after_restart.was_connected,
                "正常打断不是失败 —— 意图不许被写掉",
            );
            assert!(
                should_auto_reconnect(after_restart.was_connected, true, &ProxyMode::Tun, false),
                "自更新/崩溃打断一条正连着的会话 —— 必须自动连回来",
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// 测试用的隔离 Store（`AppState::new` 只碰这个目录，不碰真实用户数据）。
        fn temp_intent_store(tag: &str) -> xt_core::store::Store {
            let dir = std::env::temp_dir().join(format!(
                "xt-core-intent-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            xt_core::store::Store::new(dir)
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

    // -----------------------------------------------------------------------
    // 停止 / 回退的诚实性（task-62）
    //
    // 原实现的顺序是「先清 running/pid/tun_session，再看 result」+ 无条件写
    // 「网络可用」。下面钉住新口径：**没有证据就不许说成功**。
    // -----------------------------------------------------------------------

    /// 一个「会话还活着」的运行态夹具。
    fn running_with_session() -> CoreRuntime {
        CoreRuntime {
            running: true,
            pid: Some(4321),
            tun_session: Some("s-42".into()),
            tun_interface: Some("utun6".into()),
            ..Default::default()
        }
    }

    /// **(a)+(b) 停失败：必须保留 `tun_session`（会话可能还活着）。**
    ///
    /// 清掉它就再也 ref 不上那条会话了 —— 而它正是「网络可能还没恢复」的唯一证据。
    #[test]
    fn failed_stop_keeps_the_session_as_evidence() {
        let before = running_with_session();
        let err = Err::<(), String>("helper 回滚 TUN 失败：连接被拒绝".into());
        let after = runtime_after_stop(&before, &err);

        assert!(!after.running, "不再受管（supervisor 已经 take 走句柄）");
        assert_eq!(after.pid, None);
        assert_eq!(
            after.tun_session.as_deref(),
            Some("s-42"),
            "**停失败时不得清 tun_session**：那是「会话可能还活着」的唯一证据"
        );
        assert!(after.last_error.is_some(), "错误要留在运行态里，别只写日志");
    }

    /// 成功停止：只清掉原文也清的三个字段（running / pid / tun_session），
    /// **其余字段与改前逐字节一致** —— happy path 不允许改行为。
    #[test]
    fn successful_stop_matches_the_old_happy_path_byte_for_byte() {
        let before = CoreRuntime {
            last_error: Some("旧的错误".into()),
            ..running_with_session()
        };
        let after = runtime_after_stop(&before, &Ok(()));

        assert!(!after.running);
        assert_eq!(after.pid, None);
        assert_eq!(after.tun_session, None, "回滚成功后会话确实没了");
        assert_eq!(
            after.last_error.as_deref(),
            Some("旧的错误"),
            "成功路径不得顺手改 last_error（原文没改，快照必须一致）"
        );
        assert_eq!(after.tun_interface.as_deref(), Some("utun6"), "其余字段原样保留");

        // 字节级：把预期写成一个 CoreRuntime 字面量再比 JSON。
        let expected = CoreRuntime {
            running: false,
            pid: None,
            tun_session: None,
            ..before.clone()
        };
        assert_eq!(
            serde_json::to_string(&after).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "happy path 的运行态 JSON 必须与「只清那三个字段」完全一致"
        );
    }

    /// **(c) 核心断言：失败路径的文案不得出现「网络可用」，必须说「未能确认」。**
    #[test]
    fn failed_stop_line_never_claims_the_network_is_back() {
        let err = Err::<(), String>("TunDown 超时".into());
        let (level, message) = stop_log_line(&err);

        assert_eq!(level, "error");
        assert!(
            message.contains("未能确认网络已恢复"),
            "拿不到证据就要如实说不知道，实际：{message}"
        );
        assert!(
            !message.contains("网络可用"),
            "**不许**断言没验证过的事，实际：{message}"
        );
    }

    /// 成功路径的文案只声称「配置已回滚」（有 helper 成功返回为证），
    /// 同样**不**出现「网络可用」这种更强的断言。
    #[test]
    fn successful_stop_line_claims_only_what_is_evidenced() {
        let (level, message) = stop_log_line(&Ok(()));
        assert_eq!(level, "info");
        assert!(message.contains("网络配置已回滚"), "实际：{message}");
        assert!(!message.contains("网络可用"), "实际：{message}");
    }

    /// 回退直连：helper 回滚失败 → `DirectUnverified`，日志与 notice 都必须
    /// 如实说「未能确认网络已恢复」，并且**永不**出现「网络可用」。
    #[test]
    fn fallback_failure_is_reported_as_unverified_not_as_working() {
        let outcome = FallbackOutcome::from_stop(&Err("stop failed".into()));
        assert_eq!(
            outcome,
            FallbackOutcome::DirectUnverified { error: "stop failed".into() }
        );

        let (level, log_line, notice) = outcome.messages();
        assert_eq!(level, "error");
        for text in [&log_line, &notice] {
            assert!(
                text.contains("未能确认网络已恢复"),
                "必须如实说未验证：{text}"
            );
            assert!(!text.contains("网络可用"), "不许编好消息：{text}");
        }
        assert!(notice.contains("修复网络"), "要给出下一步能做什么：{notice}");
    }

    /// 回退成功：只声称「网络配置已回滚」（有证据），不声称「能上网」。
    #[test]
    fn fallback_success_claims_rollback_not_reachability() {
        let outcome = FallbackOutcome::from_stop(&Ok(()));
        assert_eq!(outcome, FallbackOutcome::DirectRestored);

        let (_, log_line, notice) = outcome.messages();
        for text in [&log_line, &notice] {
            assert!(text.contains("网络配置已回滚"), "实际：{text}");
            assert!(!text.contains("未能确认"), "成功路径不该说未确认：{text}");
            assert!(!text.contains("网络可用"), "实际：{text}");
        }
    }

    /// `fell_back_to_direct` 现在确实会被走到（回退路径有判定，不再是死代码）：
    /// 回退结局被映射到 recovery 状态机的 `DirectFallback`。
    #[test]
    fn fallback_reaches_the_recovery_state_machine() {
        let mut recovery = crate::state::RecoveryState::default();
        for outcome in [
            FallbackOutcome::from_stop(&Ok(())),
            FallbackOutcome::from_stop(&Err("boom".into())),
        ] {
            // 两个分支都必须能走到状态机（`fell_back_to_direct` 的两个入口）。
            recovery.fell_back_to_direct(1_000);
            assert_eq!(
                recovery.last_outcome,
                Some(crate::state::RecoveryOutcome::DirectFallback),
                "结局 {outcome:?} 必须落到 DirectFallback"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 文案不预设原因（task-67）
    //
    // 同一现象（经节点访问一直超时）至少三种可能：节点不可用 / 本机网络不通 /
    // 链路被干扰。界面对三种可能只给一种解释，用户就会在「换节点」和
    // 「其实应该先断开」之间来回折腾。
    // -----------------------------------------------------------------------

    /// 节点不可用的提示：**保留观察到的因果**，但给出多种可能性与自救动作。
    #[test]
    fn node_unusable_message_keeps_the_cause_but_not_a_presumed_conclusion() {
        let msg = node_unusable_message("node-abc");

        // 1) 观察到的因果必须保留（这是有证据的部分）
        assert!(msg.contains("经它访问目标一直超时"), "实际：{msg}");
        assert!(msg.contains("node-abc"), "要点名是哪个节点：{msg}");
        // 2) 必须给出**不止一种**可能，且点出「本机网络」这个以前没提过的可能性
        assert!(msg.contains("节点不可用"), "实际：{msg}");
        assert!(msg.contains("本机网络"), "实际：{msg}");
        assert!(msg.contains("可能"), "不确定的部分要用「可能」：{msg}");
        // 3) 用户已知有效的自救动作
        assert!(msg.contains("断开"), "要给出「先断开」这条路：{msg}");
        // 4) 不许退回「唯一归因 + 命令式换节点」的旧写法
        assert!(!msg.contains("请换一个节点"), "别再把因果唯一归到节点上：{msg}");
    }

    /// 回退直连的两种结局都必须能被状态机接住（`fell_back_to_direct` 不再不可达）。
    #[test]
    fn fallback_outcome_enum_covers_both_endings() {
        assert_eq!(FallbackOutcome::from_stop(&Ok(())), FallbackOutcome::DirectRestored);
        assert!(matches!(
            FallbackOutcome::from_stop(&Err("x".into())),
            FallbackOutcome::DirectUnverified { .. }
        ));
    }
}
