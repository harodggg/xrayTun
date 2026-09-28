//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

/// 核心**为什么**被启动（task-108）：由**调用点显式传入**，不在 `start_core`
/// 内部靠猜（同一个函数被六个地方复用）。
///
/// # 为什么需要它
///
/// 以前日志里查不出「谁启动了核心」：`start_proxy`（用户点「连接」）**一行都不
/// 落盘**，而 `stop_proxy`（用户点「断开」）会落一条「已作废…」。实测后果：
/// `14:16:41` / `14:19:09` 两次作废之后，`14:17:31` / `14:19:32` 各起来一个新核心
/// （间隔 50s / 23s），**日志里没有任何一行说明它是谁启动的** ⇒ task-95 的 Q7
/// 只能写「无法判定」。启动来源同时也是 after 对照的锚点（配合 App 版本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreStartTrigger {
    /// 用户在界面上点了「连接」。
    UserConnect,
    /// 切换代理模式（`set_mode`：系统代理 / TUN / 直连）。
    ModeSwitch,
    /// 切换节点（`select_node`）。
    NodeSwitch,
    /// 物理出口变化（换网）触发的重建。
    EgressChange,
    /// 看门狗发现隧道不通触发的重建。
    WatchdogRebuild,
    /// 启动时按上次的连接意图自动重连。
    AutoReconnect,
    /// 用户点了「应用意图规则」（规则在核心启动时才下发，所以要重连一次）。
    IntentRulesApply,
}

impl CoreStartTrigger {
    /// 给用户/日志看的名字（报告与日志里都直接用它）。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::UserConnect => "用户点击连接",
            Self::ModeSwitch => "切换模式",
            Self::NodeSwitch => "切换节点",
            Self::EgressChange => "物理出口变化（换网）",
            Self::WatchdogRebuild => "看门狗重建",
            Self::AutoReconnect => "启动时自动重连",
            Self::IntentRulesApply => "应用意图规则",
        }
    }

    /// **全集**（有测试断言：每个调用点用的变体都在这里，且标签互不相同）。
    ///
    /// 只有测试用它，所以生产构建里允许 dead_code —— 与 `FailureExit::ALL`
    /// 同一手法（那个也是被守卫测试用的全集）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const ALL: &'static [Self] = &[
        Self::UserConnect,
        Self::ModeSwitch,
        Self::NodeSwitch,
        Self::EgressChange,
        Self::WatchdogRebuild,
        Self::AutoReconnect,
    ];
}

/// 启动核心时落盘的那一行（**纯函数**，便于断言「版本 + 触发者」两件事都在）。
///
/// 格式照抄诊断报告首行的 `XrayTun {版本}`；一眼能看出「哪个版本、为什么启动」。
pub(crate) fn core_start_log_line(app_version: &str, trigger: CoreStartTrigger) -> String {
    format!("XrayTun {app_version} · 启动核心（触发者：{}）", trigger.label())
}

#[tauri::command]
pub async fn start_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_core(&app, &state, CoreStartTrigger::UserConnect).await?;
    spawn_dns_reprobe(&app, &state);
    snapshot::build_snapshot(&app, &state).await
}

/// 用户点「断开」但回滚失败时，写进提示条的那条**持久**陈述。
///
/// # 为什么必须有它（与看门狗 / 换网两条路径对齐）
///
/// 回滚失败时 `stop_core` 只把自己的日志写进环形缓冲，并以 `Err` 返回；而
/// `stop_proxy` 原先直接 `?` 出去 —— 用户那一刻只看到一次**瞬时**的命令报错，
/// 状态里没留下任何「未能确认网络已恢复」。同一种失败，看门狗回退与换网重建失败
/// 都会写 [`FallbackOutcome::messages`] 的诚实 notice，只有这条路径没说 ⇒ 不一致
/// 本身就是缺陷。
///
/// 成功时返回 `None`：回滚成功没有坏消息可讲，**不新增任何断言**
/// （成功路径的运行态与文案保持改前逐字节一致）。
///
/// 关键：`reason` 是 helper / supervisor 的**原始错误文本**，必须原样带进提示条 ——
/// 只写一句笼统的「失败」等于把真实原因丢掉。
pub(crate) fn stop_proxy_failure_notice(result: &Result<(), String>) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(reason) => Some(format!(
            "点「断开」时未能完成网络回滚：{reason}；**未能确认网络已恢复**\
             （helper 上的会话可能仍在，路由/DNS 可能没还原，已保留会话 id）。\
             可到「设置 → 修复网络（回滚遗留配置）」重试回滚。"
        )),
    }
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let result = stop_core(&app, &state).await;
    if let Some(notice) = stop_proxy_failure_notice(&result) {
        // **回滚失败不撒谎，也不静默。** `stop_core` 已经保留了 `tun_session`
        // （那是「helper 上这条会话可能还活着」的唯一线索）并把原因写进
        // `last_error`；这里再补一条**持久**的提示条，否则用户离开那条瞬时错误后，
        // 界面上再也没有任何「网络可能还没恢复」的陈述。
        state.with(|i| i.last_notice = Some(notice));
        // **Err 契约不变**：仍然返回 Err，载荷仍是原始错误文本。
        return Err(result.err().unwrap_or_default());
    }
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

/// 记一次**节点尝试失败**：连续失败计数 + 一条 warn 日志 + 订阅重拉提示。
///
/// # 为什么抽出来（简化，且行为不变）
///
/// 这段原来是 `start_core` 回落循环里的内联代码，只做**记账**、不参与控制流
/// （循环的走/停由 `class.is_node_level()` 决定）。抽出来之后：
///   · `start_core` 的循环只剩「决策 + 调用」，长度可读；
///   · 「连续 N 次 + 来自订阅 ⇒ 提示」这条从「只能整条启动路径间接验」
///     变成可单测（见 `note_node_failure_counts_streaks_and_only_hints_for_subscriptions`）。
/// 顺序与内容与内联版逐条相同：**计数 → 日志 → 提示**；本次在计数之后**新增**
/// 一件给界面的**账本**（见下），它不参与控制流。
///
/// # 账本（新增的那一件）
///
/// `i.node_health` 是**给界面**的那一份（类别 + 次数 + 时间 + 原文）：用户
/// 「切换节点，没用」的根因之一是列表里看不出哪台必然回落。它必须在这里写，
/// 因为这里是「一个节点这次以什么类别失败」的**唯一**判定点。
fn note_node_failure(
    state: &AppState,
    node: &Node,
    class: crate::node_health::NodeFailureClass,
    elapsed: Duration,
    message: &str,
) {
    let now = xt_core::util::now_unix();
    let streak = state
        .with(|i| {
            let n = i.node_fail_streak.entry(node.id.clone()).or_insert(0);
            *n += 1;
            let streak = *n;
            // 给界面的账本：**成功即清**（见 `start_core` 成功那一处），
            // 否则一个已经恢复的节点会一直挂着失败标记 —— 另一种假陈述。
            i.node_health.insert(
                node.id.clone(),
                crate::state::NodeHealthRecord {
                    class: class.slug().to_string(),
                    label: class.label().to_string(),
                    advice: class.advice().to_string(),
                    failures: streak,
                    last_failed_at: now,
                    detail: message.to_string(),
                },
            );
            streak
        })
        .unwrap_or(1);
    state.log(
        "app",
        "warn",
        format!(
            "节点「{}」尝试失败（{}，第 {} 次连续失败，{:.1}s）：{}",
            node.name,
            class.slug(),
            streak,
            elapsed.as_secs_f32(),
            message
        ),
    );
    // 订阅节点连续失败 ⇒ 提示重拉订阅（**不自动改用户选中的节点**）。
    if let xt_core::model::NodeSource::Subscription { id: sub_id } = &node.source {
        let sub_name = state
            .with(|i| {
                i.subscriptions
                    .iter()
                    .find(|s| &s.id == sub_id)
                    .map(|s| s.name.clone())
            })
            .unwrap_or(None);
        if let Some(sub_name) = sub_name {
            if let Some(hint) =
                crate::node_health::subscription_refresh_hint(&node.name, &sub_name, streak)
            {
                state.with(|i| i.last_notice = Some(hint.clone()));
                state.log("app", "warn", hint);
            }
        }
    }
}

/// `start_core` 的返回值：**说清这次用的是哪个节点、有没有换**。
///
/// # 为什么必须把它从 `Result<(), String>` 换掉
///
/// 「首连」与「自动重建（看门狗 / 换网）」走的是同一条回落策略，但重建路径
/// 拿到的是 `Ok(())` —— 于是自动重建成功时只能写一句「已自动恢复」，
/// **说不出这次实际用了哪个节点、是不是悄悄换掉了用户选的那个**。
/// 用户要的正是后一句（「不许静默改我选中的节点」）。
#[derive(Debug, Clone)]
pub(crate) enum CoreStartOutcome {
    /// 核心本来就在跑（幂等早退）：这次**没有**做任何节点回落。
    AlreadyRunning { pid: Option<u32> },
    /// 这次真的起来了：带上回落结局（哪个节点、有没有换、为什么）。
    Started {
        choice: crate::node_health::NodeFallbackOutcome,
    },
}

impl CoreStartOutcome {
    /// 一行、进日志 / 提示条。
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::AlreadyRunning { pid } => {
                format!("核心已在运行（pid {pid:?}），本次未做节点回落")
            }
            Self::Started { choice } => choice.describe(),
        }
    }
}

/// 一次「按候选顺序试节点」的成功结局（内部聚合，避免四元组）。
struct FallbackSuccess {
    runtime: CoreRuntime,
    events: tokio::sync::mpsc::UnboundedReceiver<xray::CoreEvent>,
    choice: crate::node_health::NodeFallbackOutcome,
    report: crate::node_health::TrialReport,
}

/// **首连与自动重建共用的那一条回落策略**（本文件里唯一的实现）。
///
/// 语义三件事，缺一不可：
/// 1. 顺序由 [`crate::node_health::rank_candidates`] 给出 —— 用户选中的永远第一，
///    其次上次验证过的，其次按真实可用性；
/// 2. 节点级失败 ⇒ 试下一个；**本地端口问题 ⇒ 立刻停**（换多少节点都白搭）；
/// 3. **全试完才失败** —— 失败时返回每节点一条记录的
///    [`crate::node_health::TrialReport`]（用户看到的是它的
///    [`crate::node_health::TrialReport::all_failed_message`]）。
///
/// 它**不写回** `settings.selected_node`：每次尝试用的是自己的
/// `attempt_settings`。成功时把「用了哪个、为什么换」装进
/// [`crate::node_health::NodeFallbackOutcome`] 作为返回值。
#[allow(clippy::too_many_arguments)]
async fn run_node_fallbacks(
    state: &AppState,
    settings: &AppSettings,
    nodes: &[Node],
    candidates: &[String],
    port_free: bool,
    search_paths: crate::supervisor::CoreSearchPaths,
    supervisor: &mut crate::supervisor::Supervisor,
    helper: &mut crate::helper_client::HelperClient,
) -> Result<FallbackSuccess, crate::node_health::TrialReport> {
    let mut report = crate::node_health::TrialReport::new();
    for (idx, candidate_id) in candidates.iter().enumerate() {
        let Some(node) = nodes.iter().find(|n| &n.id == candidate_id).cloned() else {
            continue;
        };
        // 每个尝试一个**独立**事件通道：失败尝试的日志不该混进成功那次。
        let (tx, attempt_rx) = tokio::sync::mpsc::unbounded_channel::<xray::CoreEvent>();
        let mut attempt_settings = settings.clone();
        attempt_settings.selected_node = Some(candidate_id.clone());
        let started = std::time::Instant::now();
        match supervisor
            .start(
                &state.store,
                &attempt_settings,
                nodes,
                helper,
                Some(tx),
                search_paths.clone(),
            )
            .await
        {
            Ok(runtime) => {
                let used = crate::node_health::NodeAttempt::ok(&node, started.elapsed());
                report.record(used.clone());
                let choice = crate::node_health::NodeFallbackOutcome::from_report(
                    settings.selected_node.as_deref(),
                    used,
                    &report,
                );
                return Ok(FallbackSuccess {
                    runtime,
                    events: attempt_rx,
                    choice,
                    report,
                });
            }
            Err(e) => {
                let elapsed = started.elapsed();
                let class = crate::node_health::classify(&crate::node_health::FailureFacts {
                    message: &e,
                    node_tcp_ok: None,
                    local_port_free: Some(port_free),
                });
                report.record(crate::node_health::NodeAttempt::failed(
                    &node,
                    class,
                    elapsed,
                    e.clone(),
                ));
                note_node_failure(state, &node, class, elapsed, &e);
                // 本地端口问题：换节点没用，立刻停（别让用户白等一轮）。
                if !class.is_node_level() {
                    break;
                }
                if idx + 1 < candidates.len() {
                    state.log(
                        "app",
                        "info",
                        format!("继续尝试下一个节点（候选 {}/{}）", idx + 2, candidates.len()),
                    );
                }
            }
        }
    }
    Err(report)
}

/// **首连 / 切节点 / 切模式 / 自动重连**都走这个入口：只要 `Result<(), String>`。
///
/// 需要知道「这次实际用了哪个节点、有没有换」的调用点用孪生入口
/// [`start_core_with_outcome`] —— 这里保持原签名，是因为
/// `settings::apply_mode_switch` 的类型约束就是 `Result<(), String>`
/// （那个文件不在本次改动范围内）。
pub(crate) async fn start_core(
    app: &AppHandle,
    state: &AppState,
    trigger: CoreStartTrigger,
) -> Result<(), String> {
    start_core_with_outcome(app, state, trigger).await.map(|_| ())
}

/// 与 [`start_core`] **完全相同**，只是把**回落结局**也返回出来。
///
/// # 为什么需要孪生入口（本次修的核心之一）
///
/// 自动重建（看门狗 / 换网）成功后必须能说出「这次实际用了哪个节点、为什么换」。
/// 旧实现 `start_core` 只返回 `Ok(())`，于是重建路径只能写一句「已自动恢复」——
/// 用户不知道它是不是悄悄换掉了他选的节点。判据就是
/// [`CoreStartOutcome::describe`] 里那两句。
pub(crate) async fn start_core_with_outcome(
    app: &AppHandle,
    state: &AppState,
    trigger: CoreStartTrigger,
) -> Result<CoreStartOutcome, String> {
    // **先落盘「谁启动了核心」**（task-108）：这一行是 Q7「来源不明的 core 启动」
    // 的唯一解药，也是 after 对照的锚点（带 App 版本 ⇒ 不用再猜「新版在跑吗」）。
    //
    // 放在最前面（而不是等启动成功）：失败的启动同样需要归因 ——
    // 「用户点了连接但核心没起来」与「看门狗重建失败」是两回事。
    state.log(
        "app",
        "info",
        core_start_log_line(&app.package_info().version.to_string(), trigger),
    );

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
        // 用 `running_pid()` 而不是 `runtime.pid`：这里要的是**进程真的活着**那个 pid。
        let pid = supervisor.running_pid();
        drop(helper);
        drop(supervisor);
        return Ok(CoreStartOutcome::AlreadyRunning { pid });
    }

    // 意图过滤：把**这一次应该生效**的规则交给 supervisor（它在生成配置时用）。
    //
    // 放在 `start()` 之前是必须的：配置在 `start()` 里一次性生成，
    // 之后再设就来不及了。而"规则从哪来"这里只问一次 —— 判决缓存、演练模式、
    // 用户放行纠正都在 `IntentRuntime::rules()` 里合成完毕。
    //
    // 传空两带是完全正常的形态（功能关闭 / 演练模式 / 还没有判决）。
    if let Some((allow, block)) = state.with(|i| {
        let r = i.intent.rules();
        (r.allow, r.block)
    }) {
        supervisor.set_intent_rules(allow, block);
    }

    // ---- MITM 的 fail-open 闸门（**这一句决定了用户会不会"网站打不开"**）----
    //
    // 引导规则只能挂在核心启动时的配置里（`mitm-out` 出站 + `mitm-upstream` 入站
    // 都没法热加），所以闸门必须开在这里：**根证书没被信任就摘掉 MITM**，
    // 让那些域名照常直连。否则被 steer 的流量会撞上一张没人信的证书 ——
    // 用户看到的是"网站打不开"，而不是"广告没拦住"。
    //
    // 查询要跑一次 `security(1)`（几十毫秒，本机、只读）。读不出来按"没信任"处理。
    let mitm_trusted = state
        .with(|i| crate::mitm::ca_is_trusted(i.mitm.existing_fingerprint().as_deref()))
        .unwrap_or(false);
    let effective_settings = crate::mitm::core_settings(&settings, mitm_trusted);

    // ---- 自动回落：先试用户选中的节点，再按真实可用性试其它节点 ----
    //
    // # 诚实边界（写在实现里，也写进失败文案）
    //
    // App **不能**让一个不可达的节点变得可达（节点宕机、地址写错、出网链路被挡
    // 都在我们之外）。这里能永久避免的是**伤害**：
    //   · 不让整机断网 —— 每次尝试都在「接管默认路由之前」中止并回滚；
    //   · 不让你猜 —— 每个节点一条结果，失败分三类，各自给下一步；
    //   · 不只试一个节点就放弃 —— 候选列表**包含全部节点**，全试完才报错。
    //
    // # 不许静默改用户选中的节点
    //
    // 回落只改**这一次**用的节点（`attempt_settings`），**不写回**
    // `settings.selected_node`；成功时把「用了哪个、为什么换」写进日志与提示条。
    // 取舍：界面上「当前节点」仍显示用户选中的那个，靠提示条说明实际用的是哪个 ——
    // 要把它做成界面字段需要前端配合（`CoreRuntime` 是类型契约，不能偷偷加字段）。
    let search_paths = crate::supervisor::CoreSearchPaths {
        managed_core_dir: Some(xt_core::update::managed_core_dir(state.store.root())),
        app_resource_dir: resource_dir,
        dev_binaries_dir: crate::dev_binaries_dir(),
    };
    let selected_id = settings.selected_node.clone();
    // **一个节点都没选** ≠ 「选中的节点不可达」：前者要用户先选（原有行为），
    // 后者才轮到自动回落。不加这一条，回落会把「没选」悄悄变成「替你挑一个」。
    if settings.mode != ProxyMode::Direct && selected_id.is_none() {
        return Err("请先选择一个节点".into());
    }
    let (latencies, last_good) = state
        .with(|i| (i.latencies.clone(), i.runtime.last_good_node.clone()))
        .ok_or_else(|| "应用状态不可用".to_string())?;
    // 端口预检：本地端口被占用时，换多少节点都白搭（分类会据此判 LocalPort）。
    let port_free = crate::node_health::local_port_free(effective_settings.socks_port);
    let candidates = crate::node_health::rank_candidates(
        selected_id.as_deref(),
        &nodes,
        &latencies,
        last_good.as_deref(),
    );
    let attempted = run_node_fallbacks(
        state,
        &effective_settings,
        &nodes,
        &candidates,
        port_free,
        search_paths,
        &mut supervisor,
        &mut helper,
    )
    .await;

    // task-176：**先取走路由审计再 drop**（审计要落盘，不能随 supervisor 一起丢）。
    // 放在成功/失败判定之前 ⇒ **两条路径都会记**。
    let route_audits = supervisor.take_route_audits();
    drop(helper);
    drop(supervisor);
    log_route_audits(state, &route_audits);

    // 节点尝试账：一行一个节点（id:名称@地址:端口:类别:耗时:原文），成功与失败都记。
    // 这是「为什么这次用了/没用某个节点」的唯一结构化留痕。
    let report = match &attempted {
        Ok(success) => success.report.clone(),
        Err(report) => report.clone(),
    };
    if !report.attempts().is_empty() {
        state.log(
            "app",
            "info",
            format!("节点尝试账：{}", report.summary_for_log()),
        );
    }

    let FallbackSuccess {
        runtime,
        events: mut rx,
        choice,
        ..
    } = match attempted {
        Ok(success) => success,
        Err(report) => {
            // 全试完才报错：给一份**节点级失败清单**（名字 + 地址 + 类别 + 用时 + 下一步）。
            let msg = report.all_failed_message();
            state.with(|i| {
                i.runtime = CoreRuntime {
                    running: false,
                    last_error: Some(msg.clone()),
                    ..Default::default()
                };
                i.active_node = None;
                i.push_log("app", "error", format!("启动失败：{msg}"));
                i.last_notice = Some(msg.clone());
            });
            events::runtime_changed(app, state);
            // 全试完才失败：这次的**每个节点**都在账本里留了一条（类别 + 时间）。
            // 命令返回的是 `Err`，前端那条 `run()` 路径**不会**自动拉新快照 ——
            // 所以这里必须主动说一声，否则「刚才试过的那批节点各自怎么失败的」
            // 在界面上（节点列表的失败标记）永远是空的。
            events::nodes_changed(app);
            return Err(msg);
        }
    };

    // 成功：记下**实际**用的节点（不写回 `settings.selected_node`），并清零失败计数。
    let used_node_id = choice.used_node_id().to_string();
    state.with(|i| {
        i.active_node = Some(used_node_id.clone());
        i.node_fail_streak.remove(&used_node_id);
        // **成功即清失败标记**：这台已经能用了，界面上不该再挂着「上次失败」。
        // （只清**成功**的这一台；其它失败过的节点各自的记录留着 —— 那正是
        // 「坏节点看得出来」的全部依据。）
        i.node_health.remove(&used_node_id);
    });
    // **无论有没有换，都把「这次实际用了哪个」写进日志。**
    // 换了还要再给一条**提示条**：说清换了哪个、为什么换、用户的选择没被改动。
    if let Some(notice) = choice.notice() {
        let selected_name =
            crate::node_health::node_name_or(&nodes, selected_id.as_deref(), "（未选择）");
        state.with(|i| {
            i.push_log("app", "warn", notice.clone());
            i.last_notice = Some(notice.clone());
        });
        tracing::warn!(
            from = %selected_name,
            to = %choice.used_node_name(),
            "选中节点不可达，已自动回落到其它节点"
        );
    }
    state.log("app", "info", choice.describe());

    // 记下"这次下发的配置里到底带了没有引导规则" —— 证书刚装上/刚卸掉时，
    // 界面靠它说"要重连一次核心才生效"（`MitmStatus::core_restart_required`）。
    state.with(|i| i.mitm.mark_core_steering(effective_settings.mitm.is_active()));

    // 核心已经用这份规则起来了 ⇒ 记下"这一版已经生效"。
    // 放在**成功之后**是关键：起不来的时候界面必须继续显示"待生效"，
    // 否则就是在骗人（配置没被加载过，规则却显示已生效）。
    let now = xt_core::util::now_unix();
    let applied = state.with(|i| {
        let count = {
            let r = i.intent.rules();
            (r.allow.len(), r.block.len())
        };
        i.intent.mark_applied(now);
        count
    });
    if let Some((allow, block)) = applied {
        if allow + block > 0 {
            state.log(
                "intent",
                "info",
                format!("意图规则已随核心启动生效：放行 {allow} 条 / 拦截 {block} 条"),
            );
        }
    }

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

    // **节点账本变了 ⇒ 让界面重读快照。**
    //
    // `runtime://changed` 的载荷只带 runtime/traffic（那是既有契约），而
    // `active_node` 与 `node_health` 在快照里 —— 自动重建（看门狗 / 换网）
    // 这条路径**不经过命令返回值**，不主动说一声的话，界面会一直拿着上一份
    // 「谁在用、谁坏过」。复用既有的 `nodes://changed`（前端收到就重拉快照），
    // 不新增事件名、不改任何载荷形状。
    events::nodes_changed(app);

    // 日志转发任务：核心的 stdout/stderr → 状态环形缓冲 + UI 事件。
    let app_handle = app.clone();
    let forward_pid = runtime.pid;
    // **统一走 `tauri::async_runtime::spawn`，不用裸 `tokio::spawn`。**
    // 这里在 `async fn` 里，裸 `tokio::spawn` 目前也能跑；但 App 侧一旦有人在
    // **同步上下文**（Tauri 的 `setup` 回调就是）沿用这个写法，就会 panic
    // `there is no reactor running…` ⇒ `panic = "abort"` ⇒ 双击即 SIGABRT
    // （0.8.39 的真实事故）。Tauri 的 runtime 在同步/异步上下文里都能用。
    tauri::async_runtime::spawn(async move {
        let mut throttle = LogThrottle::default();
        let mut persist_gate = PersistSummaryGate::default();
        // **一秒一次心跳**，但两条通路**分开兑现**（`throttle_tick` 是纯函数，可测）：
        // * 界面通路：每秒一条摘要，只 `emit`（用户正在看时它有意义）；
        // * 持久化通路：每个窗口一条**窗口账**，只 `state.log`。
        // 以前两条通路共用同一条摘要 ⇒ debug 级别下 `source=app` 的事件流
        // （自愈/重建/作废/连接）被每秒一条的计数淹掉（task-121 的实测）。
        // 空闲时两条通路都返回 None，不会打噪音。
        let mut flush = tokio::time::interval(LOG_THROTTLE_SUMMARY_INTERVAL);
        loop {
            tokio::select! {
                maybe = rx.recv() => {
                    let Some(event) = maybe else { break };
                    let level = classify_log(&event.line);
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        // 顺手统计各出口的连接数。放在这里而不是另起一个日志 tail：
                        // 这是核心输出的**单点**，重复读取会带来两份不一致的时间线。
                        //
                        // 为什么需要连接数：`dns-out`（UDP）与 `api`（本机回环）的
                        // 字节计数器恒为 0，那是测量盲区 —— 只显示 `0 B` 会让人以为
                        // 这两个出口没在用（本机实测各有 4769 / 5374 条连接）。
                        // 同一行日志喂两处：出口连接计数（既有）与意图过滤的候选观察。
                        // 放在同一个位置是刻意的 —— 核心 stdout 是**单点**，再开一个
                        // tail 会得到两份对不齐的时间线（`xt_core::xray::access_log`
                        // 的模块文档已经把这条理由写死了）。
                        //
                        // 意图侧只做"去重 + 过滤掉不该问的"，**判定在后台节拍里**：
                        // 这条循环在数据面路径上，任何阻塞都会拖慢日志转发。
                        state.with(|i| {
                            let observed = i.connections.observe_with_record(&event.line);
                            if let Some(record) = observed.record.as_ref() {
                                i.intent.observe(record, xt_core::util::now_unix());
                            }
                        });
                        // **原文照旧完整落盘 + 进环形缓冲**：限流只影响下面那条界面事件。
                        state.log("core", level, event.line.clone());
                        if throttle.admit(xt_core::util::now_unix(), &event.line) {
                            let _ = app_handle.emit(events::CORE_LOG, events::LogPayload { line: event.line.clone(), level: level.into() });
                        }
                    }
                }
                _ = flush.tick() => {
                    let tick = throttle_tick(&mut throttle, &mut persist_gate, xt_core::util::now_unix());
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        // 界面：只推事件，**不落盘**（磁盘上要的是窗口账）。
                        if let Some((n, sample)) = tick.ui {
                            let msg = throttled_summary_message(n, &sample);
                            let _ = app_handle.emit(events::CORE_LOG, events::LogPayload { line: msg, level: "warn".into() });
                        }
                        // 持久化：窗口聚合（`state.log` 同时进环形缓冲，日志页刷新也能看到）。
                        if let Some((n, sample)) = tick.persist {
                            state.log("app", "warn", throttled_window_summary_message(n, &sample));
                        }
                    }
                }
            }
        }

        // 循环结束时**两条通路都要收尾** —— 否则核心退出前那一段被压下的行
        // **永远不可见**（这正是不许静默丢弃的意思）。持久化这条**不看窗口节拍**：
        // 不到点也得把账落下来，否则退出会吞掉最后不足一个窗口的计数。
        // （窗口长度按「至多」理解：这条覆盖的是退出前那一段。）
        let ui_left = throttle.take_ui_summary();
        let persist_left = throttle.take_persist_summary();
        if let Some(state) = app_handle.try_state::<AppState>() {
            if let Some((n, sample)) = ui_left {
                let _ = app_handle.emit(events::CORE_LOG, events::LogPayload { line: throttled_summary_message(n, &sample), level: "warn".into() });
            }
            if let Some((n, sample)) = persist_left {
                state.log("app", "warn", throttled_window_summary_message(n, &sample));
            }
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

    Ok(CoreStartOutcome::Started { choice })
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
    // 核心没了 ⇒ "核心那边生不生效"这个问题不该再有答案（否则界面会拿上一次的
    // 事实去回答现在的问题）。
    state.with(|i| i.mitm.clear_core_steering());
    // 核心已经停了：它的监控凭据作废，否则表里会留下永远不会释放的旧 pid。

    state.with(|i| {
        // 采样任务必须先收掉：核心没了，api 端口也没人监听，
        // 留着它只会每秒产生一次连接失败。
        if let Some(monitor) = i.traffic_task.take() {
            monitor.abort();
        }
        i.traffic = crate::state::TrafficSample::default();
        // **核心没了，「实际在用哪个节点」这个问题就不该再有答案** ——
        // 留着它会让界面在断开之后继续说「正在用 X」（假陈述）。
        i.active_node = None;
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

/// **每一个「已知失败」的退场点**（task-75 ③）。
///
/// 列成枚举有两个用处：
///
/// 1. 让「该不该作废意图」成为**可断言的值**（`what_happened()` 是给日志的文案）；
/// 2. 让「调用点还在不在」**能被测试抓住** —— 这些调用点各自埋在 async 流程里
///    （要 Tauri `AppHandle` 才执行得到），纯函数测试证明不了「它还在」。
///    测试 `every_failure_exit_still_invalidates_intent_in_production_source`
///    按 `FailureExit::<Variant>` 这个标记在生产源码里逐个计数，**删一个就红**。
///
/// ⚠️ 新增退场点时：加变体 + 在 [`FailureExit::ALL`] 里登记 +
/// 在出口调用 [`invalidate_after_failure`]。三件事缺一，守卫测试都会红。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureExit {
    /// 看门狗重建隧道失败，已退回直连。
    WatchdogRebuild,
    /// 换网后重建隧道失败，已退回直连。
    NetworkWatchRebuild,
    /// 自动重连试满 `RECONNECT_ATTEMPTS` 仍未成功（门禁没过）。
    ReconnectExhausted,
    /// 切换节点失败：**隧道已还原，用户选中的节点保留**。
    ///
    /// 「回退到别的节点」这条路 2026-09-28 被整个删掉了（用户裁决：选择就使用，
    /// 不使用任何回落）。所以这里原本的四个出口收敛成一个 ——
    /// 「切到该节点失败」就是一个**终局**，不再有"回退也失败"这种中间态。
    NodeSwitchFailed,
}

impl FailureExit {
    /// 全部退场点。**新增变体必须登记在这里**；`ALL` 与生产调用点的一致性
    /// 由源码守卫测试保证。
    ///
    /// 只有测试读它（生产代码不需要遍历退场点），所以非测试构建里显式关掉
    /// `dead_code`。但**它必须留在生产模块里**：它就是「清单」本身 —— 守卫测试
    /// 靠它知道该检查哪些变体；挪进测试模块，新增变体就能悄悄溜过守卫。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const ALL: [FailureExit; 4] = [
        Self::WatchdogRebuild,
        Self::NetworkWatchRebuild,
        Self::ReconnectExhausted,
        Self::NodeSwitchFailed,
    ];

    /// 给人看的「发生了什么」。**只陈述事实**，不猜原因（不许写「节点被封了」这种）。
    pub(crate) fn what_happened(self) -> &'static str {
        match self {
            Self::WatchdogRebuild => "看门狗重建隧道失败，已退回直连",
            Self::NetworkWatchRebuild => "换网后重建隧道失败，已退回直连",
            Self::ReconnectExhausted => "自动重连多次仍未成功（门禁未过）",
            Self::NodeSwitchFailed => "切换到该节点失败（隧道已还原，你选的节点保留）",
        }
    }
}

/// **已知失败退场**：登记是哪个出口，并作废「自动重连」意图（落盘 + 留痕）。
///
/// `exit` 是枚举而不是散文案，所以每个调用点都能被源码守卫测试逐个计数。
pub(crate) fn invalidate_after_failure(state: &AppState, exit: FailureExit) {
    // 被动哨兵（task-130）：「作废自动重连意图」是最该进现场的一类事件。
    record(state, "watchdog_invalidated", "warn", exit.what_happened());
    invalidate_connect_intent(state, IntentDrop::KnownFailure, exit.what_happened());
}

/// 自动重连还要不要继续试。
///
/// 两种情况都该停：
/// * 用户明确关掉了（意图变了）—— 继续试就是「关不掉」；
/// * 隧道已经在跑 —— 用户自己点了连接并成功了，再插一手就是抢。
pub(crate) fn should_keep_reconnecting(user_wants_it: bool, already_running: bool) -> bool {
    user_wants_it && !already_running
}

/// 探针目标属于哪一侧 —— **真源在 `supervisor.rs` 的那张表**（task-106）。
///
/// 这里只做**转发**，不再自己维护一份「境内清单」：
///
/// * 旧实现有一份 `DOMESTIC_PROBE_TARGETS`，`probe_side()` 用
///   「不在境内清单 ⇒ 算境外」的默认分支兜底 —— 于是**新加一个境内目标却忘了
///   在这里表态，就会被静默算成境外**（诊断形状错、`probe_side` 语义被污染）；
/// * 现在目标与侧写在**同一张表**（`supervisor::REQUIRED_PROBE_TARGETS`，
///   `&[(url, ProbeSide)]`，**没有默认值**），`probe_side()` 只读它，
///   表里没有的目标返回 `None`（由测试拦住）。
pub(crate) use crate::supervisor::{probe_side, ProbeSide};

/// task-176：把一次 TUN 生命周期里累积的**路由审计**写进可回溯的 App 日志。
///
/// 口径（与 `task-121` 的「事件 vs 每秒计数」一致）：
/// * **按次**：一次会话 2–3 条（TunUp 后 / 接管后 / 回滚后），看门狗**只在状态变化时**；
/// * `scoped_default` 缺失且时点是「接管后 / 看门狗变化」⇒ `warn` + 哨兵记录
///   （`task-172` 的形态：绑该网卡的直连会 `ENETUNREACH`）；
/// * 「接管前 / 回滚后」缺这条路由**本来就不该有** ⇒ 只记 `info`（否则是噪声）；
/// * 采不到路由表（`netstat` 失败）⇒ 记一条 `warn`「不可判读」，**不许静默跳过**。
pub(crate) fn log_route_audits(
    state: &AppState,
    records: &[(
        xt_tun::macos::route::RouteAuditPhase,
        Option<xt_tun::macos::route::RouteAudit>,
    )],
) {
    for (phase, audit) in records {
        let Some(audit) = audit else {
            state.log(
                "app",
                "warn",
                format!(
                    "路由审计[{}]：读不到路由表（netstat 失败）⇒ 本次不可判读",
                    phase.label()
                ),
            );
            continue;
        };
        let message = audit.summary(*phase);
        if audit.is_anomaly(*phase) {
            record(state, "route_audit", "warn", &message);
            state.log("app", "warn", message);
        } else {
            state.log("app", "info", message);
        }
    }
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

// ---------------------------------------------------------------------------
// task-91 A：持续失败时的日志洪水（**界面可见**）
//
// 实测（用户机器 2026-09-22 12:05–12:35，`loglevel: debug`）：
//   30 分钟 56,292 行核心日志 ⇒ **平均 31.3 行/秒、峰值 801 行/秒、379 种「形状」**；
//   其中 `app/dns:` 26,054 行，但真正 `[Error]` 的只有 69 行 —— 洪水的主体是
//   debug 级的 DNS 记账（用户自己开的 debug）。
// 界面日志页只有 1500 行 ⇒ 被灌满并**持续滚动**（用户两次投诉的「关闭跟随还在跳」）。
// ---------------------------------------------------------------------------

/// 同一「形状」的日志，推给界面的最小间隔（秒）。
pub(crate) const LOG_SHAPE_WINDOW_SECS: u64 = 5;
/// 推给界面的核心日志**全局**上限（行/秒）—— 压住 801 行/秒那样的突发。
pub(crate) const LOG_UI_LINES_PER_SEC: u32 = 5;
/// 限流摘要的补发间隔：被压下的条数最多延迟这么久就可见。
pub(crate) const LOG_THROTTLE_SUMMARY_INTERVAL: Duration = Duration::from_secs(1);
/// **持久化**摘要的窗口长度（秒）。
///
/// # 为什么持久化要单独一个窗口（task-121）
///
/// 界面与磁盘要的是**两种东西**：界面上「每秒有多少条没实时显示」是**正在刷屏**的
/// 信号（用户盯着看有意义）；而磁盘上同一句话每秒一条，会把 `source=app` 的**事件流**
/// （自愈 / 重建 / 作废意图 / 连接 / 断开）淹掉 —— 实测「最近 12 条 `source=app` 全是
/// 同一句限流摘要」。所以磁盘上只留**窗口账**：每个非空窗口一条，条数 = 窗口内被压下的
/// 总和（计数在 `LogThrottle` 里累积，**一条都不丢**）。
///
/// 60 秒是权衡：再长，用户翻日志时容易以为「没在被限流」；再短，就接近原来的每秒噪声。
pub(crate) const LOG_PERSIST_SUMMARY_WINDOW_SECS: u64 = 60;
/// 形状记忆的容量上限。它不是账本，超了整体清一次即可（代价：这些形状各重放行一次）。
const LOG_SHAPE_MEMORY: usize = 1024;

/// 把一行日志折成「形状」：时间戳、域名/IP、数字、引号内容都换成占位符。
///
/// 目的：**同一类故障反复出现**在限流器眼里是同一个形状（否则 379 种形状里
/// 每种都放行一次，等于没限流 —— 这是模拟实测数据后才定下来的口径）。
/// **保守替换**：宁可少折一点，也不要把两类不同故障折成一条（那会丢信息）。
pub(crate) fn log_shape(line: &str) -> String {
    // 1) 引号内的内容整体折掉：URL、规则全集这些全是可变噪声
    let mut masked = String::with_capacity(line.len());
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                if !in_quotes {
                    masked.push_str("\"<q>\"");
                }
                in_quotes = !in_quotes;
            }
            _ if !in_quotes => masked.push(ch),
            _ => {}
        }
    }
    // 2) 逐 token 折；开头两个 token 是 xray 自己的时间戳（`2026/09/22 12:16:00.226232`）
    let mut out = String::with_capacity(masked.len());
    for (i, tok) in masked.split_whitespace().enumerate() {
        if !out.is_empty() {
            out.push(' ');
        }
        if i < 2 && looks_like_timestamp(tok) {
            out.push_str("<ts>");
            continue;
        }
        // `UDP:1.2.4.8:53` / `tcp:127.0.0.1:10808` 这类按 ':' 拆开逐段判
        let parts: Vec<String> = tok.split(':').map(mask_token).collect();
        out.push_str(&parts.join(":"));
    }
    out
}

fn looks_like_timestamp(tok: &str) -> bool {
    (tok.contains('/') && tok.chars().any(|c| c.is_ascii_digit()))
        || (tok.contains(':') && tok.contains('.') && tok.chars().any(|c| c.is_ascii_digit()))
}

/// 折掉一个 token 里「像数字 / 像主机名」的核心部分，保留首尾标点。
fn mask_token(tok: &str) -> String {
    let boundary = |c: char| c.is_ascii_alphanumeric() || c == '.';
    let Some(start) = tok.find(boundary) else {
        return tok.to_string();
    };
    let end = tok.rfind(boundary).map(|i| i + 1).unwrap_or(tok.len());
    if start >= end {
        return tok.to_string();
    }
    let (pre, core, post) = (&tok[..start], &tok[start..end], &tok[end..]);
    let masked = if core.chars().all(|c| c.is_ascii_digit()) {
        "<n>".to_string()
    } else if core.contains('.')
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        // 域名或 IPv4
        "<host>".to_string()
    } else {
        core.to_string()
    };
    format!("{pre}{masked}{post}")
}

/// 推给界面的核心日志**限流器**。
///
/// # 一行都不丢
///
/// * 原文照旧进后端环形缓冲与**日志文件**（调用点里 `state.log` 不受本限流影响）；
/// * 本限流只决定「哪些行**立即**推给界面」；
/// * 被压下的条数**必须可见**，而且是**两条通路各记一份**（task-121）：
///   界面每秒兑现一次（`take_ui_summary`）、磁盘按窗口兑现（`take_persist_summary`）。
#[derive(Default)]
pub(crate) struct LogThrottle {
    /// 形状 → 上次推给界面的秒。
    last_sent: std::collections::HashMap<String, u64>,
    second: u64,
    sent_this_second: u32,
    /// **界面通路**：自上次界面摘要以来被压下的条数（每秒兑现、**不落盘**）。
    suppressed_ui: u64,
    sample_ui: Option<String>,
    /// **持久化通路**：自上次落盘摘要以来被压下的条数（按窗口兑现）。
    ///
    /// 与界面通路**独立计数**：界面摘要每秒就把 `suppressed_ui` 清空，但它不落盘；
    /// 磁盘上看到的是这个窗口累计的账（见 `throttle_tick`）。
    suppressed_persist: u64,
    sample_persist: Option<String>,
}

impl LogThrottle {
    /// 这一行现在能不能推给界面？被压下的会计入两条通路的账（各由对应的 take 兑现可见性）。
    pub(crate) fn admit(&mut self, now_unix: u64, line: &str) -> bool {
        let shape = log_shape(line);
        if now_unix != self.second {
            self.second = now_unix;
            self.sent_this_second = 0;
        }
        let shape_ok = match self.last_sent.get(&shape) {
            None => true, // 这个形状还没见过 → 放行（第一现场）
            Some(&last) => now_unix.saturating_sub(last) >= LOG_SHAPE_WINDOW_SECS,
        };
        // 全局上限对**所有**行生效（新形状也一样）：否则「很多种形状」的突发等于没限流。
        // 被压下的新形状下一秒就会轮到（配额逐秒重置），最多晚 1 秒，且摘要里可见。
        let global_ok = self.sent_this_second < LOG_UI_LINES_PER_SEC;
        if shape_ok && global_ok {
            if self.last_sent.len() >= LOG_SHAPE_MEMORY {
                self.last_sent.clear();
            }
            self.last_sent.insert(shape, now_unix);
            self.sent_this_second += 1;
            true
        } else {
            self.suppressed_ui += 1;
            if self.sample_ui.is_none() {
                self.sample_ui = Some(shape.clone());
            }
            self.suppressed_persist += 1;
            if self.sample_persist.is_none() {
                self.sample_persist = Some(shape);
            }
            false
        }
    }

    /// 取走「自上次取走以来被压下的条数 + 一个形状示例」（**界面**通路，每秒一次）。
    /// `None` = 这段时间没压过任何行（**不打无谓的噪音**）。
    pub(crate) fn take_ui_summary(&mut self) -> Option<(u64, String)> {
        take_summary_from(&mut self.suppressed_ui, &mut self.sample_ui)
    }

    /// 取走**持久化**通路自上次落盘以来的条数（窗口兑现；核心退出前也会收一次尾）。
    pub(crate) fn take_persist_summary(&mut self) -> Option<(u64, String)> {
        take_summary_from(&mut self.suppressed_persist, &mut self.sample_persist)
    }
}

fn take_summary_from(count: &mut u64, sample: &mut Option<String>) -> Option<(u64, String)> {
    if *count == 0 {
        return None;
    }
    let n = *count;
    *count = 0;
    let sample = sample.take().unwrap_or_else(|| "（无示例）".to_string());
    Some((n, sample))
}

/// **持久化摘要的窗口节拍**（纯逻辑，可测）。
///
/// 抽出来是为了让「多久一条」只有**一处实现**：生产循环每个心跳调一次，
/// 单测驱动的也是它 —— 不是测试里另写一遍节奏。
#[derive(Debug, Default)]
pub(crate) struct PersistSummaryGate {
    next_due: Option<u64>,
}

impl PersistSummaryGate {
    /// 现在到点了吗？第一次调用只是**起表**（窗口还没满 ⇒ 不落）。
    ///
    /// 跨过整数个窗口也不补多条：计数在 `LogThrottle` 里累积，一条窗口摘要把这段时间
    /// 的全部条数一起报出来（**不静默丢弃**这条红线与「不淹没事件流」并不冲突）。
    pub(crate) fn due(&mut self, now_unix: u64) -> bool {
        match self.next_due {
            None => {
                self.next_due = Some(now_unix + LOG_PERSIST_SUMMARY_WINDOW_SECS);
                false
            }
            Some(due) if now_unix >= due => {
                self.next_due = Some(now_unix + LOG_PERSIST_SUMMARY_WINDOW_SECS);
                true
            }
            Some(_) => false,
        }
    }
}

/// 一次心跳（1 秒）的全部产出：**两条通路各一条**，互不影响。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ThrottleTick {
    /// 界面通路：每秒一条，只 `emit`，**不落盘**。
    pub(crate) ui: Option<(u64, String)>,
    /// 持久化通路：每个窗口一条，只落盘（`state.log`）。
    pub(crate) persist: Option<(u64, String)>,
}

/// **一次心跳的全部决策**（纯函数）：生产循环只负责把两个结果分别接到
/// `emit(CORE_LOG)` 与 `state.log` 上。
///
/// 这就是 task-121 的修法：以前两条通路**共用**同一条摘要（每秒一条同时 `emit` +
/// `state.log`），于是持久化的 app 日志被每秒计数淹没。
pub(crate) fn throttle_tick(
    throttle: &mut LogThrottle,
    gate: &mut PersistSummaryGate,
    now_unix: u64,
) -> ThrottleTick {
    let ui = throttle.take_ui_summary();
    let persist = if gate.due(now_unix) {
        throttle.take_persist_summary()
    } else {
        None
    };
    ThrottleTick { ui, persist }
}

/// 限流摘要的**界面**文案。**必须说清「有 N 条没实时显示」并指出原文在哪** ——
/// 界面不许比事实弱。
pub(crate) fn throttled_summary_message(n: u64, sample: &str) -> String {
    format!(
        "核心日志已限流：最近有 {n} 条未实时显示（原文已完整写入日志文件；刷新日志页可看到最近 2000 条）。示例格式：{sample}"
    )
}

/// 限流摘要的**持久化**文案（窗口账）。与界面那条**刻意不同**：磁盘上要能看出
/// 这是「一个窗口的汇总」，而不是每秒一次的快照。
pub(crate) fn throttled_window_summary_message(n: u64, sample: &str) -> String {
    format!(
        "核心日志限流汇总（{LOG_PERSIST_SUMMARY_WINDOW_SECS} 秒窗口）：本窗口有 {n} 条未实时显示（原文已完整写入日志文件）。示例格式：{sample}"
    )
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

    /// **用户点「断开」回滚失败**：必须留下一条**持久**的诚实陈述。
    ///
    /// 看门狗回退与换网重建失败都会写 `FallbackOutcome::messages` 的 notice，
    /// 只有 `stop_proxy` 原先直接把 `Err` `?` 出去 —— 状态里什么都没留下。
    /// 这条测试钉住三件事：提示条存在、带上**真实原因**、且**不含**「网络可用」。
    #[test]
    fn stop_proxy_failure_leaves_a_truthful_persistent_notice() {
        let reason = "helper 回滚 TUN 失败：连接被拒绝";
        let notice = stop_proxy_failure_notice(&Err(reason.to_string()))
            .expect("回滚失败必须留下提示条（否则用户离开瞬时错误后就无从得知）");

        assert!(
            notice.contains("未能确认网络已恢复"),
            "拿不到证据就要如实说不知道，实际：{notice}"
        );
        assert!(
            notice.contains(reason),
            "**原始 helper 原因必须原样带进提示条**，不许只写一句笼统的「失败」：{notice}"
        );
        assert!(
            !notice.contains("网络可用"),
            "**不许**断言没验证过的事，实际：{notice}"
        );
        assert!(
            notice.contains("修复网络"),
            "要给出下一步能做什么：{notice}"
        );
    }

    /// **成功路径不受影响**：回滚成功没有坏消息可讲 ⇒ 不写提示条
    /// （成功路径的运行态与文案保持改前逐字节一致，见上面两条测试）。
    #[test]
    fn stop_proxy_success_writes_no_failure_notice() {
        assert!(
            stop_proxy_failure_notice(&Ok(())).is_none(),
            "成功时不得凭空造一条失败陈述"
        );
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

        // 节点级失败清单（`start_core` 失败时返回的原文）必须**原样**进提示条：
        // 「先讲清试过哪些节点、各自怎么失败」是用户明确要求的顺序。
        let node_failures = "试了 2 个节点都没能建立可用隧道……\n  1. 节点「香港 A」（1.1.1.1:443）：本机→节点 TCP 不通（8.4s）";
        let (level, log_line, notice) = outcome.messages(node_failures);
        assert_eq!(level, "error");
        for text in [&log_line, &notice] {
            assert!(
                text.contains("未能确认网络已恢复"),
                "必须如实说未验证：{text}"
            );
            assert!(!text.contains("网络可用"), "不许编好消息：{text}");
        }
        assert!(notice.contains("修复网络"), "要给出下一步能做什么：{notice}");
        assert!(
            notice.contains("香港 A") && notice.contains("1.1.1.1:443"),
            "节点级失败清单必须先讲清楚：{notice}"
        );
    }

    /// 回退成功：只声称「网络配置已回滚」（有证据），不声称「能上网」；
    /// 并且提示条里**先**是「这次实际试过的节点与失败原因」，再是动作。
    #[test]
    fn fallback_success_claims_rollback_not_reachability() {
        let outcome = FallbackOutcome::from_stop(&Ok(()));
        assert_eq!(outcome, FallbackOutcome::DirectRestored);

        let node_failures = "试了 2 个节点都没能建立可用隧道……\n  1. 节点「香港 A」（1.1.1.1:443）：本机→节点 TCP 不通（8.4s）";
        let (_, log_line, notice) = outcome.messages(node_failures);
        for text in [&log_line, &notice] {
            assert!(text.contains("网络配置已回滚"), "实际：{text}");
            assert!(!text.contains("未能确认"), "成功路径不该说未确认：{text}");
            assert!(!text.contains("网络可用"), "实际：{text}");
        }
        let list_at = notice.find("实际试过的节点").expect("先讲清单");
        let node_at = notice.find("香港 A").expect("清单里要有节点");
        assert!(list_at < node_at, "顺序：先说明试过什么，再列节点：{notice}");
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

    // -----------------------------------------------------------------------
    // task-82：换网必须重建 + 看门狗必须探国内
    //
    // 根因（用户机器上实测 + 读码）：`direct` 出站的 `sockopt.interface` 是
    // **连接那一刻**的网卡；换网后国内分流仍走 direct ⇒ **国内全断、国外正常**；
    // 而看门狗只探境外 ⇒ 一直认为正常 ⇒ **永不重建**，卡在坏状态里。
    //
    // 下面把「换网 ⇒ 重建（stop→start）」与「只坏国内 ⇒ 判为异常」变成断言。
    // **不碰真机网络**：全部是纯函数 + seam，没有 route/DNS 操作。
    // -----------------------------------------------------------------------

    fn egress(interface: &str, gateway: &str) -> Egress {
        Egress {
            interface: interface.into(),
            gateway: Some(gateway.parse().unwrap()),
        }
    }

    /// **核心断言**：基线 en0、现在 en5 ⇒ 必须重建；没变 ⇒ 不许重建。
    #[test]
    fn egress_change_triggers_rebuild_and_unchanged_never_does() {
        let baseline = egress("en0", "192.168.0.1");
        assert_eq!(
            egress_action(&baseline, Some(&egress("en5", "192.168.5.1"))),
            EgressAction::Rebuild,
            "网卡换了（en0 → en5）必须重建：旧隧道的 direct 出站还绑在 en0 上",
        );
        assert_eq!(
            egress_action(&baseline, Some(&egress("en0", "192.168.9.1"))),
            EgressAction::Rebuild,
            "同一张网卡换了网关（换 WiFi / 路由器重发 DHCP）也必须重建：路由是按旧网关装的",
        );
        // **反例**：出口没变 ⇒ 不许重建（否则每 5 秒拆一次隧道）。
        assert_eq!(
            egress_action(&baseline, Some(&egress("en0", "192.168.0.1"))),
            EgressAction::Ignore,
            "出口没变还重建 = 每 5 秒自断一次网",
        );
        // **反例**：这一刻查不到默认路由（网络正在切换）⇒ 不动，下一轮再看。
        assert_eq!(
            egress_action(&baseline, None),
            EgressAction::Ignore,
            "查不到默认路由只是「还在切」，不是「换好了」——那时重建没有意义",
        );
    }

    /// 重建的三道闸门：不是我的隧道 / 用户已断开 / 已有恢复在跑 ⇒ 都不重建。
    #[test]
    fn egress_rebuild_needs_mine_intent_and_no_concurrent_recovery() {
        assert!(should_rebuild_after_egress_change(true, true, false));
        assert!(
            !should_rebuild_after_egress_change(false, true, false),
            "不是我这代隧道 —— 旧的 watcher 该退出，别去动新的那条",
        );
        assert!(
            !should_rebuild_after_egress_change(true, false, false),
            "用户点了断开 —— 再重建就是「关不掉」",
        );
        assert!(
            !should_rebuild_after_egress_change(true, true, true),
            "已经有一次自动恢复在跑：它同样会重新探测网卡，这里再拆一次只会多断一次网",
        );
    }

    /// **重建的调用序列**：先 stop、再 start。
    ///
    /// 「调用序列」正是 task-82 要的证据：用 seam 记录顺序，不需要 Tauri harness。
    #[tokio::test]
    async fn egress_rebuild_calls_stop_then_start() {
        use std::sync::Mutex;
        let calls = Mutex::new(Vec::<&str>::new());
        let out = rebuild_tunnel_in_order(
            || async {
                calls.lock().unwrap().push("stop");
                Ok::<(), String>(())
            },
            || async {
                calls.lock().unwrap().push("start");
                Ok::<(), String>(())
            },
        )
        .await;
        assert_eq!(out, Ok(()));
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["stop", "start"],
            "必须先停再起：旧隧道没拆干净，start 会撞「已有活跃会话」",
        );
    }

    /// stop 失败 ⇒ **不许**继续 start（不要在坏状态上再叠一层）。
    #[tokio::test]
    async fn rebuild_stops_short_when_teardown_fails() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let started = AtomicBool::new(false);
        let out = rebuild_tunnel_in_order(
            || async { Err::<(), String>("helper 不可用".to_string()) },
            || async {
                started.store(true, Ordering::SeqCst);
                Ok::<(), String>(())
            },
        )
        .await;
        assert!(out.is_err(), "停不下来就该如实失败");
        assert!(
            !started.load(Ordering::SeqCst),
            "停不下来还去起 = 在坏状态上再叠一层",
        );
    }

    /// **决定②的前提：证明等价。**
    ///
    /// 看门狗原来写的是 `stop_core(...).is_ok() && start_core(...).is_ok()`。
    /// 逐个枚举四种结果，断言 `rebuild_tunnel_in_order` 与它
    /// **结果相同、调用序列相同**（`&&` 短路 ⇒ stop 失败时不调用 start）。
    /// 只有这条绿，才允许把看门狗统一到 seam 上。
    #[tokio::test]
    async fn rebuild_seam_is_equivalent_to_the_inline_and_then_short_circuit() {
        use std::sync::Mutex;
        for stop_ok in [true, false] {
            for start_ok in [true, false] {
                let calls = Mutex::new(Vec::<&str>::new());
                let out = rebuild_tunnel_in_order(
                    || async {
                        calls.lock().unwrap().push("stop");
                        if stop_ok {
                            Ok::<(), String>(())
                        } else {
                            Err("stop 失败".to_string())
                        }
                    },
                    || async {
                        calls.lock().unwrap().push("start");
                        if start_ok {
                            Ok::<(), String>(())
                        } else {
                            Err("start 失败".to_string())
                        }
                    },
                )
                .await;
                // ① 结果 == 原来 `&&` 的真值
                assert_eq!(
                    out.is_ok(),
                    stop_ok && start_ok,
                    "stop_ok={stop_ok} start_ok={start_ok}：结果必须与 `&&` 一致",
                );
                // ② 调用序列 == 原来 `&&` 的短路行为
                let expected: Vec<&str> = if stop_ok {
                    vec!["stop", "start"]
                } else {
                    vec!["stop"]
                };
                assert_eq!(
                    *calls.lock().unwrap(),
                    expected,
                    "stop_ok={stop_ok} start_ok={start_ok}：调用序列必须与 `&&` 一致",
                );
            }
        }
    }

    /// **冷却窗口**：第一次变化立刻放行；紧随其后的重复被挡；冷却过后再放行。
    #[test]
    fn egress_rebuild_cooldown_gates_only_repeats_not_the_first() {
        let now = 1_700_000_000u64;
        assert!(
            egress_rebuild_allowed(now, None, EGRESS_REBUILD_COOLDOWN),
            "这次运行里还没重建过 —— 第一次变化必须立刻重建",
        );
        assert!(
            !egress_rebuild_allowed(now + 1, Some(now), EGRESS_REBUILD_COOLDOWN),
            "刚刚重建过又变了一次（Wi-Fi 抖动）—— 冷却期内不许再拆一次",
        );
        assert!(
            !egress_rebuild_allowed(
                now + EGRESS_REBUILD_COOLDOWN.as_secs() - 1,
                Some(now),
                EGRESS_REBUILD_COOLDOWN,
            ),
            "冷却还差 1 秒也不行",
        );
        assert!(
            egress_rebuild_allowed(
                now + EGRESS_REBUILD_COOLDOWN.as_secs(),
                Some(now),
                EGRESS_REBUILD_COOLDOWN,
            ),
            "冷却一到就必须允许重建（否则隧道一直坏着）",
        );
        assert!(
            egress_rebuild_allowed(now + 3600, Some(now), EGRESS_REBUILD_COOLDOWN),
            "过了很久当然允许",
        );
        assert!(
            egress_rebuild_allowed(now - 100, Some(now), EGRESS_REBUILD_COOLDOWN),
            "墙上时钟被往回调 ⇒ 无法判断间隔，宁可去恢复网络，不许永久卡死",
        );
    }

    /// **(b)** 看门狗要探的目标必须**覆盖境内 + 境外**（不是只探境外）。
    ///
    /// task-92 之后境内那一半由 **IP 字面量 `223.5.5.5`** 覆盖：
    /// `www.baidu.com` 经 SOCKS 多轮实测不稳定（10 轮 4 失败）被筛掉，
    /// 理由写在 `supervisor.rs` 的 `REQUIRED_PROBE_TARGETS` 文档里。
    #[test]
    fn watchdog_probes_cover_domestic_and_overseas() {
        let targets = crate::supervisor::required_probe_urls();
        let ips = crate::supervisor::probe_targets_without_dns(&targets);
        assert!(
            ips.len() >= 2,
            "境内 + 境外各要有一个**不依赖解析**的目标：{targets:?}",
        );
        assert!(
            ips.iter().any(|t| t.contains("223.5.5.5")),
            "必须有一个**境内**目标：只探境外时「国内全断、国外正常」会让看门狗永远认为正常（task-82）",
        );
        assert!(
            ips.iter().any(|t| t.contains("1.1.1.1")),
            "也要保留境外目标（代理链路是否真能转发）",
        );
        assert!(
            targets.contains(&xt_core::xray::DEFAULT_PROBE_URL),
            "域名目标必须保留：IP 字面量发现不了「只有解析坏」（task-92）",
        );
    }

    /// **(b)** 只坏国内 ⇒ 看门狗必须判为异常；两个都通才算通。
    #[test]
    fn only_the_domestic_target_dead_is_an_anomaly() {
        let pair = |t: &str, c: &str| (t.to_string(), c.to_string());
        let overseas = "http://cp.cloudflare.com/generate_204";
        let domestic = "http://www.baidu.com/";
        assert!(
            !probe_results_all_alive(&[pair(overseas, "204"), pair(domestic, "000")]),
            "境外 204、国内 000 —— 这正是用户报的形状，必须算异常",
        );
        assert!(
            !probe_results_all_alive(&[pair(overseas, ""), pair(domestic, "200")]),
            "境外无响应同样算异常",
        );
        assert!(
            probe_results_all_alive(&[pair(overseas, "204"), pair(domestic, "200")]),
            "两个都通才算通",
        );
        assert!(!probe_results_all_alive(&[]), "没有证据不算好");
    }

    // -----------------------------------------------------------------------
    // task-98：看门狗判据 —— 单条失败不判死 / ≥2 个目标才算一轮失败 / 轮内重试 / 退避
    // -----------------------------------------------------------------------

    fn targets_of_side(side: ProbeSide) -> Vec<&'static str> {
        // 侧只从**唯一真源表**里读（task-106：不再有第二份境内清单，也没有默认侧）
        crate::supervisor::REQUIRED_PROBE_TARGETS
            .iter()
            .filter(|(_, declared)| *declared == side)
            .map(|(url, _)| *url)
            .collect()
    }

    /// 造一轮结果：前 `dead_count` 个目标给 `000`（死），其余 200。
    fn round_with_dead(dead_count: usize) -> Vec<(String, String)> {
        crate::supervisor::required_probe_urls()
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    t.to_string(),
                    if i < dead_count { "000" } else { "200" }.to_string(),
                )
            })
            .collect()
    }

    /// **守卫（task-106）**：唯一真源表必须自洽 ——
    /// ① 表里每条 `probe_side(url) == 声明侧`；② URL 不重复；③ 两侧都有人；
    /// ④ **表里没有的目标返回 `None`**（不许再有「不在境内清单就算境外」的兜底）。
    ///
    /// **敏感性**：把 `223.5.5.5` 的声明侧改成 `Overseas`（或删掉
    /// `119.29.29.29`）⇒ `domestic_side_has_at_least_two_targets_and_keeps_the_domestic_literal`
    /// 必红；给 `probe_side` 加回「不在表里就算境外」⇒ 第 ④ 条必红。
    #[test]
    fn every_required_probe_target_declares_a_side_in_the_single_table() {
        let mut domestic = 0;
        let mut overseas = 0;
        let mut seen: Vec<&str> = Vec::new();
        for (url, declared) in crate::supervisor::REQUIRED_PROBE_TARGETS {
            assert_eq!(
                probe_side(url),
                Some(*declared),
                "表里的声明侧必须就是 probe_side 的答案：{url}"
            );
            assert!(!seen.contains(url), "同一个目标在表里出现了两次：{url}");
            seen.push(url);
            match declared {
                ProbeSide::Domestic => domestic += 1,
                ProbeSide::Overseas => overseas += 1,
            }
        }
        assert!(domestic >= 1, "境内侧至少一个目标，否则日志分不清两种病");
        assert!(overseas >= 1, "境外侧至少一个目标");
        assert_eq!(
            seen.len(),
            crate::supervisor::REQUIRED_PROBE_TARGETS.len(),
            "表里不许有重复项"
        );
        // ④ 表外目标必须返回 None（旧实现会「默认境外」——正是本卡要消除的静默误分类）
        assert_eq!(
            probe_side("http://203.0.113.99/"),
            None,
            "表里没有的目标必须返回 None：不许再有「不在境内清单就算境外」的默认分支",
        );
    }

    /// **task-106 跨语言契约**：Python（`scripts/net-metrics.py`，task-175 起）与
    /// 本文件**读同一份夹具** `scripts/fixtures/probe-targets.json`。
    /// **Rust 是权威**，夹具是双方共同的真源 —— 任一侧改坏，本用例或 Python 自测必红。
    ///
    /// 用 `read_to_string`（不是 `include_str!`）：改夹具不必重编译就能被发现；
    /// **文件缺失即失败**（共同真源缺了不许静默跳过）。
    #[test]
    fn probe_targets_fixture_matches_authoritative_table() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures/probe-targets.json");
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "读不到共享夹具 {}（共同真源，缺了必须红）：{e}",
                path.display()
            )
        });
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(&raw).expect("共享夹具必须是 JSON 数组");
        assert_eq!(
            cases.len(),
            crate::supervisor::REQUIRED_PROBE_TARGETS.len(),
            "夹具与 Rust 表的条数不同：{cases:?}"
        );
        for (i, (url, side)) in crate::supervisor::REQUIRED_PROBE_TARGETS.iter().enumerate() {
            let c = &cases[i];
            assert_eq!(c["url"].as_str(), Some(*url), "夹具第 {i} 条的 url 与 Rust 表不一致");
            let want = match side {
                ProbeSide::Domestic => "domestic",
                ProbeSide::Overseas => "overseas",
            };
            assert_eq!(
                c["side"].as_str(),
                Some(want),
                "夹具第 {i} 条的 side 与 Rust 表不一致（{url}）"
            );
        }
        println!("共享夹具 {} 条与 Rust 权威表逐条一致", cases.len());
    }

    /// **task-106 主判据**：境内侧必须 **≥2 个**目标。
    ///
    /// 理由：一轮算失败的门槛是「≥2 个目标失败」（`task-98`）。境内侧只有 1 个目标时，
    /// 「境内全灭」最多只贡献 1 个失败 ⇒ **永不触发重建** —— 用户现场包的真实形状
    /// （境内 全灭 1/1、境外 0/2 死 ⇒ 只记账、不重建），也正是 `task-172` 那种
    /// 「绑卡直连全挂」不会自愈的原因。
    ///
    /// **敏感性**：把 `223.5.5.5` 的声明侧改成 `Overseas`（或删掉 `119.29.29.29`）
    /// ⇒ 本测试必红。
    #[test]
    fn domestic_side_has_at_least_two_targets_and_keeps_the_domestic_literal() {
        let domestic = targets_of_side(ProbeSide::Domestic);
        assert!(
            domestic.len() >= 2,
            "境内侧必须 ≥2 个目标，否则「境内全灭」到不了门槛 2：{domestic:?}"
        );
        assert_eq!(
            domestic,
            vec!["http://223.5.5.5/", "http://119.29.29.29/"],
            "境内侧就应该是这两条 anycast 字面量（顺序按表）"
        );
        let overseas = targets_of_side(ProbeSide::Overseas);
        assert!(overseas.contains(&"http://1.1.1.1/"), "境外 IP 字面量不许被挪走");
        assert!(
            overseas.contains(&xt_core::xray::DEFAULT_PROBE_URL),
            "域名目标必须留在境外侧（「只有解析坏」的判据）"
        );
    }

    /// **单条探针失败不判死**：哪怕连续 100 轮，也不许凑够阈值。
    ///
    /// 旧判据是「所有目标都通才算通」，于是任意一条抖动都能在 20 秒内凑够
    /// 连续两轮并**拆掉一条正在转发流量的隧道**（task-95 定性为误判）。
    #[test]
    fn one_dead_target_is_not_a_failed_round_no_matter_how_many_rounds() {
        let mut streak = ProbeStreak::new();
        for _ in 0..100 {
            let round = classify_probe_round(&round_with_dead(1));
            assert!(!round.is_dead(), "1 个目标失败 = 没到 2 个的门槛");
            assert_eq!(streak.record(&round), 0, "未达门槛的轮不许累计");
        }
        assert!(
            !should_rebuild_tunnel(true, true, streak.rounds()),
            "100 轮单条失败也不该重建"
        );
    }

    /// **两个目标失败才算一轮失败；连续两轮才允许重建。**
    #[test]
    fn two_dead_targets_take_two_consecutive_rounds_to_rebuild() {
        let mut streak = ProbeStreak::new();
        let round = classify_probe_round(&round_with_dead(2));
        assert!(round.is_dead(), "2 个目标失败 = 到达门槛");
        assert_eq!(streak.record(&round), 1);
        assert!(
            !should_rebuild_tunnel(true, true, streak.rounds()),
            "第一轮不许重建（要连续 2 轮）"
        );
        assert_eq!(streak.record(&round), 2);
        assert!(
            should_rebuild_tunnel(true, true, streak.rounds()),
            "连续两轮到阈值才允许重建"
        );
    }

    /// 连续计数只认**连续**：中间夹一轮「只有 1 个目标失败」，计数必须清零。
    ///
    /// task-99 的反例：13:28:36 之后 baidu 仍零星失败 11 次，但彼此隔 30s–3
    /// 分钟 ⇒ 永远凑不齐连续两轮。**「差一轮就是两种命运」**，所以这行要有测试。
    #[test]
    fn a_round_below_the_threshold_resets_the_streak() {
        let mut streak = ProbeStreak::new();
        let dead_two = classify_probe_round(&round_with_dead(2));
        let dead_one = classify_probe_round(&round_with_dead(1));
        assert_eq!(streak.record(&dead_two), 1);
        assert_eq!(streak.record(&dead_one), 0, "被打断就必须清零");
        assert_eq!(streak.record(&dead_two), 1, "重新从 1 开始");
        assert!(!should_rebuild_tunnel(true, true, streak.rounds()));
    }

    /// **task-106 修掉的结构性盲区**：境内全灭（现在境内侧有 2 个目标）**必须**触发重建。
    ///
    /// 修复前境内只有 1 个目标 ⇒ 「境内全灭」= 1 个失败 < 门槛 2 ⇒ **永不重建**
    /// （旧用例 `domestic_only_total_failure_is_below_the_threshold` 曾把这个盲区
    /// 写成「已知取舍」；用户现场包证明它真的咬到了人：境内 全灭 1/1、境外 0/2 死
    /// ⇒ 只记账、不重建，于是 `task-172` 那种「绑卡直连全挂」一直不恢复）。
    #[test]
    fn domestic_full_outage_reaches_the_threshold_now() {
        let mut results: Vec<(String, String)> = targets_of_side(ProbeSide::Domestic)
            .iter()
            .map(|t| (t.to_string(), "000".to_string()))
            .collect();
        for t in targets_of_side(ProbeSide::Overseas) {
            results.push((t.to_string(), "200".to_string()));
        }
        let round = classify_probe_round(&results);
        assert!(round.domestic_is_dead(), "境内侧确实全灭");
        assert!(!round.overseas_is_dead(), "境外侧没事");
        assert!(
            round.is_dead(),
            "境内 {} 个目标全灭 ≥ 门槛 2 ⇒ **必须触发重建**",
            round.domestic_dead
        );
        assert!(round.one_side_only(), "日志仍要能看出这是单侧失败");
    }

    /// **安全属性保留（task-98）**：任一侧只死 **1 个**目标 ⇒ 仍只记账、不拆隧道。
    ///
    /// 与上一条是一对：**修掉盲区 ≠ 变得一惊一乍**（单条抖动拆掉正在转发流量的
    /// 隧道是 task-95 定性过的误判）。
    #[test]
    fn a_single_dead_target_still_does_not_trigger_a_rebuild() {
        let dead_target = "http://223.5.5.5/";
        let results: Vec<(String, String)> = crate::supervisor::required_probe_urls()
            .into_iter()
            .map(|t| {
                (
                    t.to_string(),
                    if t == dead_target { "000" } else { "200" }.to_string(),
                )
            })
            .collect();
        let round = classify_probe_round(&results);
        assert_eq!(round.dead, 1, "只死一个（境内侧 1/2）");
        assert!(!round.domestic_is_dead(), "境内侧没全灭");
        assert!(!round.is_dead(), "单条失败不许拆隧道（task-95/98 的安全属性）");
        let mut streak = ProbeStreak::new();
        for _ in 0..100 {
            assert_eq!(streak.record(&round), 0, "未达门槛的轮不许累计");
        }
        assert!(!should_rebuild_tunnel(true, true, streak.rounds()));
    }

    // -----------------------------------------------------------------------
    // task-176：路由审计落盘（**行为级**：真的写进 App 日志 + 哨兵）
    // -----------------------------------------------------------------------

    use xt_core::store::Store;

    /// 事故形态的路由表（只有系统的 default、没有 `I` 标志；`0/1` 捕获在；两条 `/32`）。
    fn audit_missing_scoped_default() -> xt_tun::macos::route::RouteAudit {
        xt_tun::macos::route::parse_netstat_inet(
            "0/1                utun6              UScg                utun6\n\
             default            192.168.0.1        UGScg                 en0\n\
             203.0.113.7        192.168.0.1        UGHS                  en0\n\
             203.0.113.9        192.168.0.1        UGHS                  en0\n",
            "en0",
        )
    }

    fn route_audit_state(tag: &str) -> (AppState, Store) {
        let dir = std::env::temp_dir().join(format!("xt-routeaudit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(dir);
        (AppState::new(store.clone()), store)
    }

    /// **行为级**：节点失败记账（原来埋在 `start_core` 的回落循环里）。
    ///
    /// 等价性论据：`note_node_failure` 与内联版做同样三件事（计数 → 日志 → 提示），
    /// 所以把「连续失败递增」与「手工节点不提示订阅」这两条钉出来即可。
    /// 判别性：把计数那一段删掉 ⇒ 第一条断言红；把 `Manual` 也当成订阅 ⇒ 第二条红。
    #[test]
    fn note_node_failure_counts_streaks_and_only_hints_for_subscriptions() {
        let (state, store) = route_audit_state("node-fail-streak");
        let node = Node {
            id: "n-manual".into(),
            name: "手工节点".into(),
            address: "203.0.113.9".into(),
            port: 443,
            protocol: xt_core::model::Protocol::Vless {
                uuid: "00000000-0000-0000-0000-000000000000".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            transport: Default::default(),
            tls: Default::default(),
            mux: None,
            source: xt_core::model::NodeSource::Manual,
            tags: Vec::new(),
            raw_uri: None,
        };
        for round in 1..=3u32 {
            note_node_failure(
                &state,
                &node,
                crate::node_health::NodeFailureClass::TcpUnreachable,
                Duration::from_secs(8),
                "接管默认路由之前就联系不上代理服务器 203.0.113.9:443",
            );
            let seen = state
                .with(|i| i.node_fail_streak.get("n-manual").copied())
                .unwrap();
            assert_eq!(seen, Some(round), "第 {round} 次失败后计数应当递增");
        }
        let notice = state.with(|i| i.last_notice.clone()).unwrap_or(None);
        assert!(
            notice.is_none(),
            "手工节点没有订阅可重拉 ⇒ 不许出现订阅提示：{notice:?}"
        );
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// **节点失败账本**（给界面的那一份）：类别 / 次数 / 时间 / 原文都要写进去。
    ///
    /// 判据是用户原话那条路径（「切换节点，没用，没有切换到香港，还是在美国」）：
    /// 界面上「坏节点看得出来」的**唯一**依据就是这条记录 —— 所以它必须由
    /// `note_node_failure`（类别判定的唯一处）来写，而不是界面按错误文案猜。
    ///
    /// 已知边界：**「成功即清」发生在 `start_core` 成功那一条路**（要真的起核心），
    /// 这条单测覆盖不到；它由 `start_core` 里 `i.node_health.remove(&used_node_id)`
    /// 那一行 + UI 侧 `nodeFallbackHonesty.test.tsx` 的负向对照共同钉住。
    #[test]
    fn node_failure_is_recorded_in_the_ui_ledger() {
        let (state, store) = route_audit_state("node-health-ledger");
        let node = Node {
            id: "n-egress".into(),
            name: "香港 · REALITY 01".into(),
            address: "45.207.197.185".into(),
            port: 443,
            protocol: xt_core::model::Protocol::Vless {
                uuid: "00000000-0000-0000-0000-000000000000".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            transport: Default::default(),
            tls: Default::default(),
            mux: None,
            source: xt_core::model::NodeSource::Manual,
            tags: Vec::new(),
            raw_uri: None,
        };
        note_node_failure(
            &state,
            &node,
            crate::node_health::NodeFailureClass::EgressBroken,
            Duration::from_secs(3),
            "经它发出的真实请求拿不到响应（000）",
        );
        let rec = state
            .with(|i| i.node_health.get("n-egress").cloned())
            .unwrap()
            .expect("失败必须进「给界面」的账本（否则列表里看不出哪台必然回落）");
        assert_eq!(rec.class, "egress-broken", "机器可读的类别必须与 classify 的结论一致");
        assert_eq!(rec.label, "节点可达但出口不通", "中文类别名给用户看");
        assert!(!rec.advice.is_empty(), "下一步必须能原样转述给用户");
        assert_eq!(rec.failures, 1);
        assert!(rec.last_failed_at > 0, "最近一次失败时间必须有值（界面要显示它）");
        assert!(rec.detail.contains("拿不到响应"), "原文必须带着：{}", rec.detail);

        // 第二次失败：次数递增、类别可被更新（同一个节点只有一条「最近一次」）。
        note_node_failure(
            &state,
            &node,
            crate::node_health::NodeFailureClass::TcpUnreachable,
            Duration::from_secs(8),
            "联系不上代理服务器",
        );
        let rec2 = state
            .with(|i| i.node_health.get("n-egress").cloned())
            .unwrap()
            .expect("第二次失败后账本还在");
        assert_eq!(rec2.failures, 2);
        assert_eq!(rec2.class, "tcp-unreachable", "留下的是**最近一次**的类别");
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// **L1 行为级**：接管后缺「作用域默认路由」⇒ App 落盘日志里出现**可直接判读**的
    /// `warn`（含后果与证据），且哨兵 `logs/anomalies.jsonl` 里有一条 `route_audit`。
    ///
    /// **反向**：同一份缺失在「接管前」是**正常**的 ⇒ 不许报警（否则接管流程必刷屏）。
    #[test]
    fn a_missing_scoped_default_after_commit_is_logged_loudly() {
        use crate::commands::incident::anomaly_file;
        use crate::state::LogEntry;

        let (state, store) = route_audit_state("commit");
        let missing = audit_missing_scoped_default();
        assert!(missing.scoped_default_missing(), "夹具必须是缺失形态");
        log_route_audits(
            &state,
            &[(
                xt_tun::macos::route::RouteAuditPhase::AfterCommitRoutes,
                Some(missing.clone()),
            )],
        );

        let lines: Vec<LogEntry> = store.tail_logs(50);
        let warn = lines
            .iter()
            .find(|l| l.level == "warn" && l.message.contains("路由审计"))
            .unwrap_or_else(|| panic!("接管后缺 scoped 默认路由必须落一条 warn：{lines:?}"));
        assert!(warn.message.contains("的作用域默认路由**缺失**"), "{}", warn.message);
        assert!(warn.message.contains("ENETUNREACH"), "{}", warn.message);
        assert!(
            warn.message.contains("CommitRoutes 之后"),
            "采样时点必须写进日志本身：{}",
            warn.message
        );
        let sentinel = std::fs::read_to_string(anomaly_file(store.root())).unwrap_or_default();
        assert!(sentinel.contains("route_audit"), "哨兵里应有一条 route_audit：{sentinel}");

        // 反向：**接管前**缺这条路由是正常的 ⇒ 只 info、不 warn、不写哨兵
        let (state2, store2) = route_audit_state("tunup");
        log_route_audits(
            &state2,
            &[(xt_tun::macos::route::RouteAuditPhase::AfterTunUp, Some(missing))],
        );
        let lines2: Vec<LogEntry> = store2.tail_logs(50);
        assert!(
            lines2.iter().any(|l| l.message.contains("路由审计")),
            "每次采样都要留痕：{lines2:?}"
        );
        assert!(
            lines2.iter().all(|l| l.level != "warn"),
            "接管前缺这条路由是正常的，不许报警：{lines2:?}"
        );
        let sentinel2 = std::fs::read_to_string(anomaly_file(store2.root())).unwrap_or_default();
        assert!(sentinel2.is_empty(), "接管前不该写哨兵：{sentinel2}");

        let _ = std::fs::remove_dir_all(store.root());
        let _ = std::fs::remove_dir_all(store2.root());
    }

    /// **L1 行为级**：采不到路由表时**如实记「不可判读」**，不许静默跳过。
    #[test]
    fn an_unreadable_route_table_is_recorded_as_unavailable() {
        use crate::state::LogEntry;
        let (state, store) = route_audit_state("unavailable");
        log_route_audits(
            &state,
            &[(xt_tun::macos::route::RouteAuditPhase::AfterRollback, None)],
        );
        let lines: Vec<LogEntry> = store.tail_logs(50);
        let line = lines
            .iter()
            .find(|l| l.message.contains("路由审计"))
            .unwrap_or_else(|| panic!("采样失败也要留痕：{lines:?}"));
        assert!(line.message.contains("不可判读"), "{}", line.message);
        assert!(line.message.contains("回滚之后"), "采样时点要写进日志：{}", line.message);
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// **轮内重试**是本卡最高性价比的杠杆（task-100 实测：失败后同目标下一次
    /// 成功 baidu 94%、cloudflare 56%）。这条测试同时是它的**敏感性守卫**：
    /// 把 `PROBE_ATTEMPTS_PER_ROUND` 改回 1 ⇒ 这里红。
    #[test]
    fn a_failed_probe_is_retried_once_in_the_same_round() {
        assert!(!should_retry_probe("204", 1), "成功不许再打一次请求");
        assert!(
            should_retry_probe("000", 1),
            "第一次失败、还有预算 ⇒ 必须重试（快失败那种病）"
        );
        assert!(
            should_retry_probe("", 1),
            "无响应（挂住那种病）同样要重试"
        );
        assert!(
            !should_retry_probe("000", PROBE_ATTEMPTS_PER_ROUND),
            "预算用完就停，不许无限重试"
        );
    }

    /// 重建失败之后：老隧道还活着（停止失败 / 进程仍在）⇒ 必须保持原状。
    ///
    /// 这正是 task-95 那次误判伤人的地方：重建失败后把**还活着**的隧道拆掉、
    /// 写「已退回直连」、作废自动重连，然后看门狗自己 `return`，
    /// 空档 690–2660 秒。
    #[test]
    fn failed_rebuild_keeps_a_still_alive_tunnel() {
        assert_eq!(
            after_failed_rebuild(true, false),
            FailedRebuildAction::KeepTunnel,
            "停都停不下来 ⇒ 老隧道很可能还在，不许退直连"
        );
        assert_eq!(
            after_failed_rebuild(false, true),
            FailedRebuildAction::KeepTunnel,
            "核心进程仍在 ⇒ 不许拆"
        );
        assert_eq!(
            after_failed_rebuild(false, false),
            FailedRebuildAction::FallBackToDirect,
            "老隧道确实没了 ⇒ 退直连，别把用户留在断网状态"
        );
    }

    /// 进程存活判据（与 HTTP 探针**正交**）：自己一定活着，不存在的 pid 一定不是。
    #[test]
    fn core_process_alive_is_orthogonal_evidence() {
        assert!(
            core_process_alive(Some(std::process::id())),
            "本进程必须判为活着"
        );
        assert!(!core_process_alive(None), "没见过 pid 不算活着");
        let bogus = 4_000_000; // 远超 macOS 的 pid 上限（maxproc 量级），必然不存在
        assert!(!core_process_alive(Some(bogus)), "不存在的 pid 必须是 false");
    }

    /// 重建失败的退避：逐档增长、最后一档封顶（既不许死循环重试，也不许放弃）。
    #[test]
    fn rebuild_backoff_grows_then_caps() {
        assert_eq!(next_backoff_secs(0), 30);
        assert_eq!(next_backoff_secs(1), 60);
        assert_eq!(next_backoff_secs(2), 120);
        assert_eq!(next_backoff_secs(3), 300);
        assert_eq!(next_backoff_secs(99), 300, "封顶，不许涨到天上去");
        assert!(
            REBUILD_BACKOFF_SECS[0] >= 10,
            "至少给网络一点恢复时间，别立刻重试"
        );
    }

    /// 唤醒只把**等待**提前，判据不变：那一轮仍须 ≥2 个目标失败。
    #[test]
    fn wake_bumps_the_wait_but_not_the_proof() {
        let dead_one = classify_probe_round(&round_with_dead(1));
        let mut streak = ProbeStreak::new();
        assert_eq!(streak.record(&dead_one), 0);
        assert_eq!(
            streak.bump_to_threshold_for(&dead_one),
            0,
            "没到门槛的轮，唤醒也不许把计数抬到阈值（旧实现正是这样误判的）"
        );
        let dead_two = classify_probe_round(&round_with_dead(2));
        assert_eq!(streak.record(&dead_two), 1);
        assert_eq!(
            streak.bump_to_threshold_for(&dead_two),
            FAILURES_BEFORE_REBUILD,
            "到门槛的轮：唤醒可以把等待提前"
        );
    }

    /// 日志必须说出**是哪个目标**不通（否则又回到「只报一句」查不动）。
    #[test]
    fn dead_target_description_names_the_target() {
        let results = vec![
            (
                "http://cp.cloudflare.com/generate_204".to_string(),
                "204".to_string(),
            ),
            ("http://www.baidu.com/".to_string(), String::new()),
        ];
        let text = describe_dead_targets(&results);
        assert!(text.contains("baidu.com"), "要点名国内目标：{text}");
        assert!(
            text.contains("无响应"),
            "空码要说「无响应」，不要假装它是一个状态码：{text}",
        );
        assert!(
            !text.contains("cloudflare"),
            "通的目标不该出现在失败描述里：{text}",
        );
    }

    /// **防「换网又变回只写日志」与「看门狗又只探境外」**（task-82 双向敏感性靠它成立）。
    ///
    /// 这两个调用点都埋在 async 任务里（要 Tauri `AppHandle` 才执行得到），
    /// 纯函数测试证明不了「任务里真的调了它」。所以这里做**源码级断言**：
    /// 删掉任意一处调用 → 本测试变红（它只保证「调用还在」，不保证运行时行为）。
    #[test]
    fn network_watch_rebuilds_and_watchdog_probes_both_paths_in_production_source() {
        let prod = include_str!("core.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        assert!(
            prod.contains("rebuild_tunnel_in_order("),
            "换网检测必须**继续调用重建**（而不是「只写一条日志就 return」）——\
             删掉这个调用就是回到 task-82 的坏状态",
        );
        assert!(
            prod.contains("watchdog_probe_all(port, PROBE_TIMEOUT_SECS)"),
            "看门狗必须用**多目标探测**（国内 + 境外）—— 回到只探境外就又会漏掉「国内全断」；\
             超时值必须走命名常量 `PROBE_TIMEOUT_SECS`（task-98：阈值要与判据分开）",
        );
        assert!(
            prod.contains("for target in crate::supervisor::required_probe_urls()"),
            "`watchdog_probe_all` 必须遍历门禁那份必需目标清单（不能只探一个）",
        );
        assert!(
            prod.contains("!egress_rebuild_allowed(now_unix, last_started"),
            "换网重建必须**继续过冷却判据**（删掉它 = Wi-Fi 抖动时每 5 秒拆一次网）",
        );
        assert!(
            !prod.contains("请断开后重新连接"),
            "旧文案「隧道不再有效，请断开后重新连接」= 只报不修，不许回来",
        );
    }

    // -----------------------------------------------------------------------
    // task-75 ③：**已知失败的退场点必须能被测试抓住**
    //
    // 这些作废调用各自埋在 async 流程里（要 Tauri `AppHandle` 才执行得到），
    // 纯函数测试证明不了「它还在」；而 Lead 已明确否决「搭假 Tauri harness」。
    // 所以这里做**源码级守卫**：每个 `FailureExit` 变体在生产源码里**必须恰好
    // 出现一次**（= 那一处作废调用）。**删掉任意一处 → 本测试变红。**
    //
    // 它只保证「调用还在」，不保证运行时时序或文案 —— 这一点写在测试名里。
    // -----------------------------------------------------------------------

    #[test]
    fn every_failure_exit_still_invalidates_intent_in_production_source() {
        // 只看 `#[cfg(test)]` 之前的部分：测试代码里也会拼 `FailureExit::X`
        // （构造具体变体做断言），那不该算进锚点计数。
        let strip = |src: &str| src.split("#[cfg(test)]").next().unwrap_or("").to_string();
        let prod = strip(include_str!("core.rs")) + &strip(include_str!("nodes.rs"));
        // **必须先去掉注释**：把作废调用**注释掉**同样让它失效，而纯文本计数会
        // 把它算成「还在」—— 第一次做这张卡的敏感性实验时就被它骗过（守卫仍绿）。
        let code = strip_comments(&prod);
        for exit in FailureExit::ALL {
            let anchor = format!("FailureExit::{exit:?}");
            let count = code.matches(&anchor).count();
            assert_eq!(
                count, 1,
                "退场点 `{anchor}` 在**去掉注释后的**生产源码里应当恰好出现一次\
                 （那一处作废调用），现在出现 {count} 次。删掉它、或把它注释掉，\
                 都等于这处作废失效 —— 那正是本测试要防的回归。",
            );
            assert!(
                code.contains(&format!("({anchor})")) || code.contains(&format!(", {anchor})")),
                "`{anchor}` 必须**作为调用参数**出现（`invalidate_after_failure(&state, {anchor})`\
                 或 `SwitchEnd::NoTunnel({anchor})`）—— 只是提到它、没传进调用，等于没作废。",
            );
        }
    }

    /// 去掉 `//` 行注释与 `/* */` 块注释（保留换行，保证行结构不变）。
    ///
    /// 守卫测试专用。只管 Rust 注释，不管字符串字面量里出现的 `//` ——
    /// 在这个用途下够用且**更安全**：过度截断只会让计数变少（更容易变红），
    /// 不会把「注释掉的调用」误算成存在。而 7 个锚点各自独占一行、行首是代码，
    /// 所以截断不会影响它们。
    fn strip_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars().peekable();
        let mut in_block = false;
        while let Some(c) = chars.next() {
            if in_block {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block = false;
                }
                continue;
            }
            if c == '/' {
                match chars.peek() {
                    Some('/') => {
                        // 行注释：丢到行尾，但保留那个换行
                        for c in chars.by_ref() {
                            if c == '\n' {
                                out.push('\n');
                                break;
                            }
                        }
                        continue;
                    }
                    Some('*') => {
                        chars.next();
                        in_block = true;
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(c);
        }
        out
    }

    // -----------------------------------------------------------------------
    // task-91 A：日志洪水的**限流**（量化依据：实测 31.3 行/秒、峰值 801、379 形状）
    // -----------------------------------------------------------------------

    /// 「形状」要能把**同类故障**折成同一条（域名/时间戳/数字都是噪声）。
    #[test]
    fn log_shape_collapses_volatile_parts() {
        let a = log_shape(
            "2026/09/22 12:16:00.226232 [Debug] app/dns: domain a.b.com matches following rules: [geosite:cn]",
        );
        let b = log_shape(
            "2026/09/22 12:16:03.111111 [Debug] app/dns: domain x.y.net matches following rules: [geosite:cn]",
        );
        assert_eq!(a, b, "同一类故障必须折成同一形状（否则限流等于没做）：\n{a}\n{b}");
        assert!(a.contains("<ts>"), "时间戳要折掉：{a}");
        assert!(a.contains("<host>"), "域名要折掉：{a}");
    }

    /// **反例**：不同的故障**不许**折成同一条（那会丢信息）。
    #[test]
    fn log_shape_keeps_different_failures_apart() {
        let lookup = log_shape(
            "2026/09/22 12:16:00.1 [Error] app/dns: failed to lookup ip for domain x.com at server UDP:1.2.4.8:53",
        );
        let hit = log_shape(
            "2026/09/22 12:16:00.1 [Debug] app/dns: UDP:1.2.4.8:53 cache HIT x.com. -> [1.2.3.4]",
        );
        assert_ne!(lookup, hit, "不同故障折成一条就等于把它们混成一条：\n{lookup}\n{hit}");
    }

    /// 同一形状：首条立刻放行，窗口内不再放行，且**被压下的条数能对账**。
    #[test]
    fn throttle_admits_first_of_a_shape_and_limits_repeats() {
        let mut t = LogThrottle::default();
        let a = "2026/09/22 12:16:00.1 [Error] app/dns: failed to lookup ip for domain x.com";
        let b = "2026/09/22 12:16:00.2 [Error] app/dns: failed to lookup ip for domain y.com";
        assert!(t.admit(100, a), "第一次见到这个形状 → 放行（第一现场）");
        assert!(!t.admit(100, b), "同一形状在窗口内不再推给界面");
        assert_eq!(
            t.take_ui_summary(),
            Some((1, log_shape(a))),
            "被压下的 1 条必须能对账（这就是「不许静默丢弃」）",
        );
        // 两条通路**各自记账**（task-121）：界面取过之后，持久化那条仍然欠着这笔账。
        assert_eq!(
            t.take_persist_summary(),
            Some((1, log_shape(a))),
            "持久化通路是独立的窗口账，不能被界面通路取走",
        );
        assert_eq!(t.take_ui_summary(), None, "没有新的省略就不该打噪音");
        assert_eq!(t.take_persist_summary(), None, "持久化侧也不该重复报");
        assert!(
            t.admit(100 + LOG_SHAPE_WINDOW_SECS, b),
            "窗口过后放行",
        );
    }

    /// **全局上限**对「很多种形状」同样生效；被压下的照样进账。
    #[test]
    fn throttle_caps_distinct_shapes_per_second_and_accounts_for_the_rest() {
        let mut t = LogThrottle::default();
        let mut admitted = 0;
        for i in 0..50 {
            // 每条形状都不同（不同子系统前缀），但仍受全局 5 行/秒约束
            let line = format!("2026/09/22 12:16:00.1 [Info] subsystem{i}: something happened");
            if t.admit(100, &line) {
                admitted += 1;
            }
        }
        assert_eq!(
            admitted, LOG_UI_LINES_PER_SEC,
            "一秒内的全局上限必须生效（否则「形状多」的突发等于没限流）",
        );
        let (n, _) = t.take_ui_summary().expect("被压下的必须有摘要");
        assert_eq!(n, 50 - u64::from(LOG_UI_LINES_PER_SEC), "省略数必须精确对账");
        // 下一秒：剩下的 45 种形状会各放行一次（又被 5 行/秒截住）
        let mut admitted_next = 0;
        for i in 0..50 {
            let line = format!("2026/09/22 12:16:01.1 [Info] subsystem{i}: something happened");
            if t.admit(101, &line) {
                admitted_next += 1;
            }
        }
        assert_eq!(admitted_next, LOG_UI_LINES_PER_SEC, "配額逐秒重置");
    }

    /// 用实测速率搭一个洪流（**31 行/秒、两种形状、10 分钟**），界面入库行数必须有界，
    /// 且**持久化的窗口账**必须对得上（task-121）。
    ///
    /// 依据：用户机器 12:05–12:35 实测平均 31.3 行/秒；前两种形状占 14%。
    /// 界面日志页只有 1500 行 ⇒ 不限制的话 10 分钟就是 18,600 行灌进去、持续滚动。
    ///
    /// 驱动的是**生产同一个纯函数** `throttle_tick`（心跳 + 窗口节拍），
    /// 不是在测试里另写一遍「每 60 秒一次」。
    #[test]
    fn throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate() {
        let mut t = LogThrottle::default();
        let mut gate = PersistSummaryGate::default();
        let mut admitted = 0u64;
        let mut ui_summaries = 0u64;
        let mut ui_accounted = 0u64;
        let mut persist_summaries = 0u64;
        let mut persist_accounted = 0u64;
        for sec in 0..600u64 {
            for i in 0..31 {
                let line = if i % 2 == 0 {
                    format!("2026/09/22 12:16:00.{i} [Debug] app/dns: domain host{i}.com matches following rules: [geosite:cn]")
                } else {
                    format!("2026/09/22 12:16:00.{i} [Debug] app/dns: UDP:1.2.4.8:53 cache HIT host{i}.com. -> [1.2.3.4]")
                };
                if t.admit(sec, &line) {
                    admitted += 1;
                }
            }
            let tick = throttle_tick(&mut t, &mut gate, sec);
            if let Some((n, _)) = tick.ui {
                ui_summaries += 1;
                ui_accounted += n;
            }
            if let Some((n, _)) = tick.persist {
                persist_summaries += 1;
                persist_accounted += n;
            }
        }
        // 循环结束时的收尾（生产里就是 `rx` 关掉之后那一段）：**两条通路都要收**。
        if let Some((n, _)) = t.take_ui_summary() {
            ui_summaries += 1;
            ui_accounted += n;
        }
        if let Some((n, _)) = t.take_persist_summary() {
            persist_summaries += 1;
            persist_accounted += n;
        }
        let total = 600 * 31;
        assert_eq!(
            admitted + ui_accounted,
            total,
            "**界面通路**每一行都要有着落：放行的 + 明确记账省略的 = 全部（不许静默丢弃）",
        );
        assert_eq!(
            admitted + persist_accounted,
            total,
            "**持久化通路**同样要有着落（窗口聚合不许把计数吃掉）",
        );
        assert!(
            admitted <= 600 / LOG_SHAPE_WINDOW_SECS * 2 + 2,
            "两种形状 × 每 {LOG_SHAPE_WINDOW_SECS}s 一条 ⇒ 放行数必须有界，实际 {admitted}",
        );
        assert_eq!(ui_summaries, 600, "界面：每秒一条摘要（被压下过的那一秒）");
        assert_eq!(
            persist_summaries,
            600 / LOG_PERSIST_SUMMARY_WINDOW_SECS,
            "**持久化：每 {LOG_PERSIST_SUMMARY_WINDOW_SECS} 秒一条窗口账**（600 秒 ⇒ 10 条），\
             而不是每秒一条 —— 这就是 task-121 修的「淹没事件流」",
        );
        // 换算：31 行/秒 → 放行 ≤ (2/5) 行/秒 + 摘要 1 行/秒
        assert!(
            (admitted as f64) / 600.0 <= 0.5,
            "界面入库速率必须从 31 行/秒降到 ≤0.5 行/秒（实测数据模拟），实际 {}",
            (admitted as f64) / 600.0,
        );
        // 磁盘那条路的噪声水平：10 分钟只写 10 行（原来 600 行）。
        assert_eq!(
            persist_summaries * LOG_PERSIST_SUMMARY_WINDOW_SECS,
            600,
            "窗口账必须恰好覆盖这 600 秒",
        );
    }

    /// **task-121 主回归**：持久化摘要按**窗口聚合**，且每个窗口的账 == 该窗口内
    /// 每秒账之和（聚合不丢一条）；不足一个窗口的部分在退出时收尾补上。
    #[test]
    fn persist_summary_is_window_aggregated_and_covers_every_second() {
        let mut t = LogThrottle::default();
        let mut gate = PersistSummaryGate::default();
        let mut ui_counts: Vec<u64> = Vec::new();
        let mut windows: Vec<(u64, u64)> = Vec::new(); // (落点秒, 该窗口账)
        for sec in 0..(LOG_PERSIST_SUMMARY_WINDOW_SECS * 3) {
            for i in 0..31 {
                let line = format!("2026/09/22 12:16:00.{i} [Debug] app/dns: UDP:1.2.4.8:53 cache HIT host{i}.com. -> [1.2.3.4]");
                let _ = t.admit(sec, &line);
            }
            let tick = throttle_tick(&mut t, &mut gate, sec);
            ui_counts.push(tick.ui.as_ref().map(|(n, _)| *n).unwrap_or(0));
            if let Some((n, _)) = tick.persist {
                windows.push((sec, n));
            }
        }
        assert_eq!(
            windows.len() as u64,
            2,
            "3 个窗口的数据里，到点的只有 2 个（60s / 120s）—— 第三个窗口还没满：{windows:?}",
        );
        // 窗口在 `t = k * WINDOW` 秒**关闭**，并且把「关闭那一秒」的账也一起报出
        // （心跳是在该秒的行到达之后才处理的）⇒ 用**累计**对账，不对边界秒做假设。
        let mut cum_win = 0u64;
        for (i, (sec, n)) in windows.iter().enumerate() {
            let close = (i as u64 + 1) * LOG_PERSIST_SUMMARY_WINDOW_SECS;
            cum_win += n;
            let cum_ui: u64 = ui_counts[..=close as usize].iter().sum();
            assert_eq!(
                *sec, close,
                "窗口必须恰好在 {LOG_PERSIST_SUMMARY_WINDOW_SECS} 秒的整数倍关闭",
            );
            assert_eq!(
                cum_win, cum_ui,
                "第 {} 个窗口关闭时（@{sec}s）的累计账必须与界面通路**完全一致**",
                i + 1,
            );
        }
        // 退出收尾：最后一个没满的窗口也要落下来，**不能因为不到点就吞掉**。
        let (last_n, _) = t.take_persist_summary().expect("退出时必须收尾");
        let tail_start = (LOG_PERSIST_SUMMARY_WINDOW_SECS * 2 + 1) as usize;
        let tail: u64 = ui_counts[tail_start..].iter().sum();
        assert_eq!(last_n, tail, "退出收尾必须把最后一个窗口的账补齐");
        let all: u64 = ui_counts.iter().sum();
        assert_eq!(
            windows.iter().map(|(_, n)| n).sum::<u64>() + last_n,
            all,
            "窗口账之和 + 退出收尾必须覆盖全程（一条都不能丢）",
        );
    }

    /// 摘要文案必须**明确说出省略了多少条**并指出原文在哪（红线：界面不许比事实弱）。
    #[test]
    fn throttled_summary_says_how_many_were_omitted() {
        let msg = throttled_summary_message(137, "app/dns: failed to lookup ip for domain <host>");
        assert!(msg.contains("137"), "要点出省略条数：{msg}");
        assert!(msg.contains("日志文件"), "要指出完整原文在哪：{msg}");
        assert!(msg.contains("刷新"), "要告诉用户怎么看全（刷新日志页）：{msg}");
    }

    /// 窗口摘要文案必须说清「这是窗口的账」+ 多少条 —— 与界面那条**要能区分开**
    /// （否则读日志的人分不清「每秒快照」还是「窗口汇总」）。
    #[test]
    fn throttled_window_summary_says_it_is_a_window() {
        let msg = throttled_window_summary_message(137, "app/dns: failed to lookup ip for domain <host>");
        assert!(msg.contains("137"), "要点出省略条数：{msg}");
        assert!(msg.contains("窗口"), "磁盘那条要能看出是窗口账：{msg}");
        assert!(
            msg.contains(&LOG_PERSIST_SUMMARY_WINDOW_SECS.to_string()),
            "要写出窗口长度：{msg}"
        );
        assert!(msg.contains("日志文件"), "要指出完整原文在哪：{msg}");
        assert_ne!(
            msg,
            throttled_summary_message(137, "app/dns: failed to lookup ip for domain <host>"),
            "磁盘那条与界面那条必须不同",
        );
    }

    /// **手动证据工具**（`#[ignore]`，不进 CI）：把**真实** `app.jsonl` 里已经写下的
    /// 每秒限流摘要，按**新的窗口口径**重放一遍 —— 得到「改前 / 改后持久化日志片段对照」。
    ///
    /// **诚实边界**：这不是「真机跑过新版本」（用户机器上仍是 0.8.33），而是
    /// **用真实数据驱动生产函数**（`PersistSummaryGate` + `throttled_window_summary_message`）；
    /// 「窗口账 = 逐秒账之和、退出收尾不丢」由 `persist_summary_is_window_aggregated_and_covers_every_second` 钉住。
    ///
    /// ```text
    /// XT_REAL_LOG="$HOME/Library/Application Support/com.xraytun.desktop/logs/app.jsonl" \
    ///   cargo test -p xraytun-desktop --lib real_throttle_summaries_under_the_new_window_rule -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "手动证据工具：需要 XT_REAL_LOG=<app.jsonl 路径>"]
    fn real_throttle_summaries_under_the_new_window_rule() {
        let Ok(path) = std::env::var("XT_REAL_LOG") else {
            eprintln!("跳过：请设置 XT_REAL_LOG");
            return;
        };
        let raw = std::fs::read_to_string(&path).expect("读 app.jsonl");
        // 真实每秒账：(ts, n, 示例格式)
        let mut per_sec: Vec<(u64, u64, String)> = Vec::new();
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("source").and_then(|s| s.as_str()) != Some("app") {
                continue;
            }
            let Some(msg) = v.get("message").and_then(|m| m.as_str()) else {
                continue;
            };
            if !msg.contains("核心日志已限流") {
                continue;
            }
            let n = msg
                .split_whitespace()
                .find_map(|t| t.parse::<u64>().ok())
                .unwrap_or(0);
            let sample = msg
                .split("示例格式：")
                .nth(1)
                .unwrap_or("（无示例）")
                .to_string();
            let ts = v.get("ts_unix").and_then(|t| t.as_u64()).unwrap_or(0);
            per_sec.push((ts, n, sample));
        }
        if per_sec.is_empty() {
            println!("真实日志里没有限流摘要");
            return;
        }
        let (first, last) = (per_sec[0].0, per_sec[per_sec.len() - 1].0);
        let total: u64 = per_sec.iter().map(|(_, n, _)| n).sum();
        println!("=== 改前（真实 app.jsonl 里已经写下的行）===");
        println!("摘要行数 {}，被压下的条数合计 {}，时间跨度 {} 秒", per_sec.len(), total, last - first);
        for (ts, n, _) in per_sec.iter().take(3) {
            println!(
                "  [{}] 核心日志已限流：最近有 {} 条未实时显示（原文已完整写入日志文件…）",
                crate::commands::diagnostics::utc_iso(*ts),
                n
            );
        }
        // 改后：同一批计数按窗口节拍重放（**生产的 gate + 文案**）。
        let mut gate = PersistSummaryGate::default();
        let mut pending = 0u64;
        let mut sample = String::new();
        let mut emitted: Vec<(u64, u64, String)> = Vec::new();
        let mut idx = 0usize;
        for sec in first..=last {
            while idx < per_sec.len() && per_sec[idx].0 == sec {
                pending += per_sec[idx].1;
                if sample.is_empty() {
                    sample = per_sec[idx].2.clone();
                }
                idx += 1;
            }
            if gate.due(sec) && pending > 0 {
                emitted.push((sec, pending, std::mem::take(&mut sample)));
                pending = 0;
            }
        }
        // 退出收尾（生产里 `rx` 关掉之后那一段）。
        if pending > 0 {
            emitted.push((last, pending, sample));
        }
        println!("=== 改后（同一批计数，按新的 {LOG_PERSIST_SUMMARY_WINDOW_SECS} 秒窗口口径重放）===");
        println!("窗口账行数 {}", emitted.len());
        for (ts, n, s) in emitted.iter().take(3) {
            println!(
                "  [{}] {}",
                crate::commands::diagnostics::utc_iso(*ts),
                throttled_window_summary_message(*n, s)
            );
        }
        let sum: u64 = emitted.iter().map(|(_, n, _)| n).sum();
        assert_eq!(sum, total, "重放不许丢计数");
        println!("（对账：窗口账合计 {sum} == 真实被压下合计 {total}）");
    }

    // -----------------------------------------------------------------------
    // task-108：核心启动必须**自证**（App 版本 + 触发者）
    // -----------------------------------------------------------------------

    /// 六个触发者各有**互不相同**的名字（合成一个标签会让人没法归因）。
    ///
    /// ⚠️ 这里原来是 **7** 类：第 7 个是「切换节点（回退）」。节点回落 2026-09-28
    /// 被整个删掉（用户裁决：选择就使用），触发者也随之收敛成 6 个 ——
    /// 这个数字变了不是"放宽断言"，而是**那一类触发者真的不存在了**。
    #[test]
    fn every_start_trigger_has_its_own_label() {
        let labels: Vec<&str> = CoreStartTrigger::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(
            labels.len(),
            6,
            "全集是 6 类（`grep -rn 'start_core('` 核过所有调用点；回退路径已删）：{labels:?}"
        );
        let uniq: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(uniq.len(), labels.len(), "标签不许重复：{labels:?}");
        assert!(labels.iter().all(|l| !l.is_empty()), "{labels:?}");
    }

    /// 落盘那一行必须**同时**带 App 版本与真实触发者。
    #[test]
    fn core_start_line_names_version_and_trigger() {
        let line = core_start_log_line("0.8.34", CoreStartTrigger::WatchdogRebuild);
        assert!(
            line.contains("XrayTun 0.8.34"),
            "必须带 **App** 版本：核心横幅是核心版本，不随 App 变，不能代替它：{line}"
        );
        assert!(line.contains("看门狗重建"), "必须带真实触发者：{line}");
    }

    /// **源码级守卫**：每个调用点必须**显式**传入它自己的触发者。
    ///
    /// 为什么需要：函数签名已经强制「必须传一个变体」（不传编译不过），但**传错**
    /// 或写死某个变体，编译器不会知道 —— 那时「来源不明」会变成「来源错」，
    /// 比不记更难查。逐点钉住（与 task-75 / task-98 的源码守卫同一手法）。
    #[test]
    fn each_start_core_call_site_names_its_own_trigger() {
        let strip = |src: &str| src.split("#[cfg(test)]").next().unwrap_or("").to_string();
        let core = strip(include_str!("core.rs"));
        let nodes = strip(include_str!("nodes.rs"));
        let settings = strip(include_str!("settings.rs"));
        for (file, anchor, what) in [
            (
                &core,
                "start_core(&app, &state, CoreStartTrigger::UserConnect)",
                "用户点连接",
            ),
            (
                &settings,
                "start_core(&app, &state, CoreStartTrigger::ModeSwitch)",
                "切模式",
            ),
            (
                &nodes,
                "start_core(&app, &state, CoreStartTrigger::NodeSwitch)",
                "切节点",
            ),
            (
                &core,
                "start_core_with_outcome(&handle, &state, CoreStartTrigger::EgressChange)",
                "换网重建",
            ),
            (
                &core,
                "start_core_with_outcome(&handle, &state, CoreStartTrigger::WatchdogRebuild)",
                "看门狗重建",
            ),
            (
                &core,
                "start_core(app, state, CoreStartTrigger::AutoReconnect)",
                "启动时自动重连",
            ),
        ] {
            assert!(
                file.contains(anchor),
                "「{what}」这个调用点不见了它自己的触发者（改成别的来源或写死都会让锚点消失）：{anchor}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // 自动重建 ⇄ 首连：**同一条回落策略**，且成功必须说出用了哪个节点
    // -----------------------------------------------------------------------

    /// **源码级守卫**：自动重建（看门狗 / 换网）**不许自己再写一份回落**。
    ///
    /// 判据：
    /// 1. 两条重建路径都调 [`start_core_with_outcome`]（回落 + 带回结局）；
    /// 2. 回落循环**只有一处实现** —— `run_node_fallbacks(` 全文件出现 2 次
    ///    （定义 1 + 调用 1）。多一处就说明有人复制了第二份策略。
    ///
    /// 判别性：把任一重建点改回 `start_core(`（丢掉结局）或把循环复制一份 ⇒ 红。
    #[test]
    fn auto_rebuild_shares_the_single_fallback_strategy() {
        let strip = |src: &str| src.split("#[cfg(test)]").next().unwrap_or("").to_string();
        let core = strip(include_str!("core.rs"));

        for anchor in [
            "start_core_with_outcome(&handle, &state, CoreStartTrigger::WatchdogRebuild)",
            "start_core_with_outcome(&handle, &state, CoreStartTrigger::EgressChange)",
        ] {
            assert!(core.contains(anchor), "重建路径必须走带回结局的入口：{anchor}");
        }
        assert!(
            core.contains("start_core_with_outcome(app, state, trigger).await.map(|_| ())"),
            "首连入口必须与重建入口**共用同一个实现**（薄包装），不许各写一份回落"
        );
        assert_eq!(
            core.matches("run_node_fallbacks(").count(),
            2,
            "回落策略只许有一处实现（定义 1 次 + 调用 1 次）：{}",
            core.matches("run_node_fallbacks(").count()
        );
    }

    /// **验收判据 ①（自动重建那一半）**：重建成功后返回的结局必须能说出
    /// 「实际用了哪个节点、有没有换」—— 否则用户无从知道它是不是悄悄换了节点。
    ///
    /// 这里用 [`CoreStartOutcome`] 的两个变体把「返回值里说明」钉死。
    #[test]
    fn rebuild_outcome_names_the_node_it_actually_used() {
        use crate::node_health::{NodeAttempt, NodeFailureClass, NodeFallbackOutcome, TrialReport};
        let sel = node_fixture("n-sel", "旧节点", "1.1.1.1");
        let other = node_fixture("n-other", "备用节点", "2.2.2.2");
        let mut report = TrialReport::new();
        report.record(NodeAttempt::failed(
            &sel,
            NodeFailureClass::TcpUnreachable,
            Duration::from_millis(8400),
            "接管默认路由之前就联系不上代理服务器 1.1.1.1:443（第1次失败、第2次失败，每次 4 秒）",
        ));
        let used = NodeAttempt::ok(&other, Duration::from_millis(1200));
        report.record(used.clone());
        let choice = NodeFallbackOutcome::from_report(Some("n-sel"), used, &report);

        let started = CoreStartOutcome::Started { choice };
        let text = started.describe();
        assert!(text.contains("备用节点") && text.contains("2.2.2.2:443"), "{text}");
        assert!(text.contains("1.1.1.1:443"), "要说清为什么换：{text}");
        assert!(text.contains("本机→节点 TCP 不通"), "{text}");

        // 幂等早退那条路径**没有**做回落 ⇒ 不许假装知道用了哪个节点。
        let already = CoreStartOutcome::AlreadyRunning { pid: Some(42) };
        let text = already.describe();
        assert!(text.contains("未做节点回落"), "{text}");
        assert!(!text.contains("本次实际使用节点"), "{text}");
    }

    fn node_fixture(id: &str, name: &str, address: &str) -> Node {
        Node {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            port: 443,
            protocol: xt_core::model::Protocol::Vless {
                uuid: "00000000-0000-0000-0000-000000000000".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            transport: Default::default(),
            tls: Default::default(),
            mux: None,
            source: xt_core::model::NodeSource::Manual,
            tags: Vec::new(),
            raw_uri: None,
        }
    }
}
