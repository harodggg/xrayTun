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
            Self::IntentRulesApply => "应用意图规则",
        }
    }

    /// **全集**（有测试断言：每个调用点用的变体都在这里，且标签互不相同）。
    ///
    /// 只有测试用它，所以生产构建里允许 dead_code。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const ALL: &'static [Self] = &[
        Self::UserConnect,
        Self::ModeSwitch,
        Self::NodeSwitch,
        Self::IntentRulesApply,
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
    start_core_with_outcome(app, state, trigger).await
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
) -> Result<(), String> {
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
        drop(helper);
        drop(supervisor);
        return Ok(());
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
pub(crate) use crate::supervisor::ProbeSide;

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


    /// **成功路径不受影响**：回滚成功没有坏消息可讲 ⇒ 不写提示条
    /// （成功路径的运行态与文案保持改前逐字节一致，见上面两条测试）。
    #[test]
    fn stop_proxy_success_writes_no_failure_notice() {
        assert!(
            stop_proxy_failure_notice(&Ok(())).is_none(),
            "成功时不得凭空造一条失败陈述"
        );
    }




    // -----------------------------------------------------------------------
    // 文案不预设原因（task-67）
    //
    // 同一现象（经节点访问一直超时）至少三种可能：节点不可用 / 本机网络不通 /
    // 链路被干扰。界面对三种可能只给一种解释，用户就会在「换节点」和
    // 「其实应该先断开」之间来回折腾。
    // -----------------------------------------------------------------------



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


    // -----------------------------------------------------------------------
    // task-98：看门狗判据 —— 单条失败不判死 / ≥2 个目标才算一轮失败 / 轮内重试 / 退避
    // -----------------------------------------------------------------------







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








    // -----------------------------------------------------------------------
    // task-75 ③ 的来源（`FailureExit` 的源码级守卫）已经随那一族一起删掉了：
    // 「作废重连意图」这件事本身不存在了，也就没有"每个退场点必须调一次"要守。
    // 留着这段说明是为了让后来的人知道**这里原本有东西、以及它为什么消失**。

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

    /// 剩下的触发者各有**互不相同**的名字（合成一个标签会让人没法归因）。
    ///
    /// ⚠️ 这里原来是 **7** 类：第 7 个是「切换节点（回退）」。节点回落 2026-09-28
    /// 被整个删掉（用户裁决：选择就使用），触发者也随之收敛成 6 个 ——
    /// 这个数字变了不是"放宽断言"，而是**那一类触发者真的不存在了**。
    #[test]
    fn every_start_trigger_has_its_own_label() {
        let labels: Vec<&str> = CoreStartTrigger::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(
            labels.len(),
            4,
            "全集是 4 类（`grep -rn 'start_core('` 核过所有调用点；\n\
             回退 / 换网 / 看门狗 / 启动自动重连四条路径已删）：{labels:?}"
        );
        let uniq: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(uniq.len(), labels.len(), "标签不许重复：{labels:?}");
        assert!(labels.iter().all(|l| !l.is_empty()), "{labels:?}");
    }

    /// 落盘那一行必须**同时**带 App 版本与真实触发者。
    #[test]
    fn core_start_line_names_version_and_trigger() {
        let line = core_start_log_line("0.8.34", CoreStartTrigger::NodeSwitch);
        assert!(
            line.contains("XrayTun 0.8.34"),
            "必须带 **App** 版本：核心横幅是核心版本，不随 App 变，不能代替它：{line}"
        );
        assert!(line.contains("切换节点"), "必须带真实触发者：{line}");
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



}
