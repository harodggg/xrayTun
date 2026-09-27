//! 节点不可达时的**可避免伤害**：失败三分类、自动回落排序、全挂时的节点清单。
//!
//! # 诚实边界（不许承诺做不到的事）
//!
//! **App 无法让一个不可达的节点变得可达**：节点宕机、地址写错、出网链路被挡，
//! 都在我们之外。本模块能永久避免的是这件事的**伤害**：
//!
//! 1. **不让你整机断网** —— 所有尝试都在「接管默认路由之前」中止并回滚（既有行为）；
//! 2. **不让你猜** —— 每个节点一条结果，并区分三类失败，各自给下一步；
//! 3. **不只试一个节点就放弃** —— 选中的节点不可达时按顺序试其它节点
//!    （[`rank_candidates`]），全试完才报错（[`TrialReport::all_failed_message`]）。
//!
//! # 为什么分类要读「真实错误原文」
//!
//! 分类**不是**新的探测，而是对既有失败信息的**解读**：谁（本机 / 节点 / 本地端口）
//! 出了问题，决定了下一步该做什么。所以判据用代码里真实产生的错误串
//! （见各常量与单测里的原文），而不是猜形状。

use std::collections::HashMap;
use std::time::Duration;

use xt_core::model::Node;
use xt_core::xray::ProbeResult;

/// 同一个节点连续失败多少次后，提示「重拉订阅」。
///
/// 3 次：一次失败可能是抖动（本仓库的 TCP 预检本来就有重试），
/// 两次还可能是网络刚切换；三次仍不可达就值得怀疑订阅里的地址过期了。
pub(crate) const CONSECUTIVE_FAILURES_BEFORE_SUB_REFRESH: u32 = 3;

/// 失败的三种类别（外加一个「认不出来」兜底）。
///
/// **只有 [`NodeFailureClass::is_node_level`] 为真的类别才值得换节点重试**：
/// 本地端口被占用时，换多少个节点都没用 —— 那会让用户白等一轮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeFailureClass {
    /// 本机 → 节点 TCP 不通（含 DNS 解析不出）。
    TcpUnreachable,
    /// 节点 TCP 可达，但经它的真实请求拿不到响应（REALITY/TLS 被挡、出口坏）。
    EgressBroken,
    /// 本地端口/绑定问题（SOCKS 入站端口被占用等）。
    LocalPort,
    /// 认不出是哪一类 —— 如实说「不知道」，不硬塞进另外三类。
    Unknown,
}

impl NodeFailureClass {
    /// 日志/机器可读的短名（**不要**拿去做文案）。
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::TcpUnreachable => "tcp-unreachable",
            Self::EgressBroken => "egress-broken",
            Self::LocalPort => "local-port",
            Self::Unknown => "unknown",
        }
    }

    /// 给用户看的类别名。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TcpUnreachable => "本机→节点 TCP 不通",
            Self::EgressBroken => "节点可达但出口不通",
            Self::LocalPort => "本地端口/绑定问题",
            Self::Unknown => "原因不明",
        }
    }

    /// **下一步做什么**（错误文案必须回答这个）。
    ///
    /// # 为什么「换一个网络」不许排第一（用户明确抗议过）
    ///
    /// 用户原话：「总会会有这个问题。切换网络不应该影响网络。」他遇到的是
    /// **本机网络完全正常、只是某个节点不通**，而旧文案把「先换一个网络
    /// （例如切到热点）」列在第一位 —— 把他引去改一个没坏的东西，改完还是不通。
    /// 现在每个类别的第一步都是**在 App 里就能做、而且指向真正原因**的动作；
    /// 「换网络」只在最后，并且写明**前提**（整台 Mac 都上不了网）。
    pub(crate) fn advice(self) -> &'static str {
        match self {
            Self::TcpUnreachable => {
                "先在节点列表里换一个节点重试（App 已按真实可用性替你试过一轮）；\
                 核对这个节点的地址 / 端口是否过期；订阅节点可点「刷新订阅」重拉一次；\
                 **只有**整台 Mac 直连也上不了网时，才需要换网络（如手机热点）"
            }
            Self::EgressBroken => {
                "换一个节点；这个节点本身能连上，但它转发不出去（墙或节点出口的问题）"
            }
            Self::LocalPort => {
                "本地端口被占用或被拒绝：把「设置」里的 SOCKS 端口换一个，或关掉占用它的程序后重试"
            }
            Self::Unknown => {
                "把这条错误原文发给开发者；同时先在节点列表里换一个节点，\
                 **只有**整台 Mac 直连也上不了网时才需要换网络"
            }
        }
    }

    /// 换节点重试有没有意义。
    pub(crate) fn is_node_level(self) -> bool {
        matches!(self, Self::TcpUnreachable | Self::EgressBroken | Self::Unknown)
    }
}

/// 分类时能拿到的**事实**。`None` = 没测过/不知道，那一维度不参与判定。
#[derive(Debug, Clone, Copy)]
pub(crate) struct FailureFacts<'a> {
    /// 启动失败时返回的错误原文。
    pub message: &'a str,
    /// 本机 → 节点 TCP 是否可达（预检结果；不知道就给 `None`）。
    pub node_tcp_ok: Option<bool>,
    /// 本地 SOCKS 端口现在是不是空闲的（`local_port_free` 的结果）。
    pub local_port_free: Option<bool>,
}

// 真实错误原文的判据片段（都来自本仓库代码，单测逐条钉住）。
const DNS_FAIL: &str = "无法解析节点地址";
const TCP_PRE_CHECK_FAIL: &str = "联系不上代理服务器";
const TCP_SYS_UNREACHABLE: &str = "Network is unreachable";
const TCP_SYS_NO_ROUTE: &str = "No route to host";
const TCP_SYS_REFUSED: &str = "Connection refused";
const TCP_SYS_TIMEOUT: &str = "Operation timed out";
const EGRESS_GATE_FAIL: &str = "经它发出的真实请求拿不到响应";
const EGRESS_GATE_ABORTED: &str = "已在接管默认路由之前中止";
const PORT_BIND_IN_USE: &str = "address already in use";
const PORT_BIND_FAILED: &str = "failed to listen";
const PORT_LOCAL_REFUSED: &str = "Failed to connect to 127.0.0.1";

/// 判定这次失败属于哪一类。
///
/// 优先级是刻意的，且**先看错误原文指向谁**：
///
/// 1. 文案/事实**明确指向节点**（DNS 解析不出、TCP 预检两次都失败、系统级
///    `No route to host` 等）⇒ 就是节点侧 —— 哪怕本地端口恰好也被占着，
///    也不该把这句报错改写成"端口问题"（那是另一个独立故障）；
/// 2. 否则看**本地端口证据**（端口确实不空闲 / bind 系原文）⇒ 本地端口；
/// 3. 再否则，TCP 事实为「可达」或门禁原文 ⇒ 节点可达但出口不通；
/// 4. 都没有 ⇒ 如实说「原因不明」。
pub(crate) fn classify(facts: &FailureFacts<'_>) -> NodeFailureClass {
    let msg = facts.message;

    if facts.node_tcp_ok == Some(false)
        || msg.contains(DNS_FAIL)
        || msg.contains(TCP_PRE_CHECK_FAIL)
        || msg.contains(TCP_SYS_UNREACHABLE)
        || msg.contains(TCP_SYS_NO_ROUTE)
        || msg.contains(TCP_SYS_REFUSED)
        || msg.contains(TCP_SYS_TIMEOUT)
    {
        return NodeFailureClass::TcpUnreachable;
    }

    if facts.local_port_free == Some(false)
        || msg.contains(PORT_BIND_IN_USE)
        || msg.contains("Address already in use")
        || msg.contains(PORT_BIND_FAILED)
        || msg.contains(PORT_LOCAL_REFUSED)
    {
        return NodeFailureClass::LocalPort;
    }

    if facts.node_tcp_ok == Some(true)
        || msg.contains(EGRESS_GATE_FAIL)
        || msg.contains(EGRESS_GATE_ABORTED)
    {
        return NodeFailureClass::EgressBroken;
    }

    NodeFailureClass::Unknown
}

/// 一个节点的一次尝试结果（成功也记 —— 清单里要能看出「哪个是活的」）。
///
/// **带地址与端口**：用户报的问题原话就是「请检查节点地址 / 端口」，
/// 而旧清单里只有节点**名字** —— 名字是用户自己起的，凭它核不了任何东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeAttempt {
    pub node_id: String,
    pub node_name: String,
    /// 节点地址（域名或 IP）—— 与 `port` 一起给用户核对。
    pub address: String,
    pub port: u16,
    /// 失败类别。**成功**的尝试给 `None`。
    pub class: Option<NodeFailureClass>,
    pub elapsed: Duration,
    pub detail: String,
}

impl NodeAttempt {
    pub(crate) fn ok(node: &Node, elapsed: Duration) -> Self {
        Self {
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            address: node.address.clone(),
            port: node.port,
            class: None,
            elapsed,
            detail: "连接成功".into(),
        }
    }

    pub(crate) fn failed(
        node: &Node,
        class: NodeFailureClass,
        elapsed: Duration,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            address: node.address.clone(),
            port: node.port,
            class: Some(class),
            elapsed,
            detail: detail.into(),
        }
    }

    /// `地址:端口` —— 用户能直接照着核对 / 复制的形态。
    pub(crate) fn endpoint(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// `「名字」（地址:端口）` —— 名字与地址都要在，缺一个都核不了。
    pub(crate) fn label(&self) -> String {
        format!("「{}」（{}）", self.node_name, self.endpoint())
    }
}

/// 自动回落的账本：**每个节点一条**，全试完才报错。
#[derive(Debug, Clone, Default)]
pub(crate) struct TrialReport {
    attempts: Vec<NodeAttempt>,
}

impl TrialReport {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record(&mut self, attempt: NodeAttempt) {
        self.attempts.push(attempt);
    }

    pub(crate) fn attempts(&self) -> &[NodeAttempt] {
        &self.attempts
    }

    /// 全部尝试都失败时的**节点级失败清单**。
    ///
    /// 形状（顺序是刻意的，见 [`NodeFailureClass::advice`] 的说明）：
    /// 1. 先说清「网络没被动过」；
    /// 2. **逐节点**列（名字 + 地址:端口 + 类别 + 用时 + 各自的下一步）——
    ///    这是用户要求的「先把我们已经替你试过哪些节点、各自怎么失败的讲清楚」；
    /// 3. 「本机网络是不是有问题」这条**用证据回答**：只要有一个节点在 TCP 层
    ///    可达，就明说本机网络没问题、不需要先换网络；
    /// 4. 最后才是**可做的动作**，并且「换网络」排在最后、带前提。
    pub(crate) fn all_failed_message(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "试了 {} 个节点都没能建立可用隧道，本次启动已中止：\
             **默认路由没有被接管、系统网络没有被改动**。\n\
             我们已经替你逐个试过这些节点，各自的结果（名字 · 地址:端口 · 类别 · 耗时）：\n",
            self.attempts.len()
        ));
        for (i, a) in self.attempts.iter().enumerate() {
            let class = a.class.unwrap_or(NodeFailureClass::Unknown);
            out.push_str(&format!(
                "  {}. 节点{}：{}（{:.1}s）\n     下一步：{}\n",
                i + 1,
                a.label(),
                class.label(),
                a.elapsed.as_secs_f32(),
                class.advice(),
            ));
        }
        if self.any_node_tcp_reachable() {
            out.push_str(
                "另外：上面有节点在 **TCP 层是可达的** ⇒ **本机网络本身没问题**，\
                 问题在那个节点或它的出口 —— 这种情况**不需要**先换网络。\n",
            );
        }
        out.push_str(
            "说明：App **不能**让一个不可达的节点变得可达（节点宕机、地址写错、\
             出网链路被挡都在我们之外）。这里能保证的是：不接管你的默认路由、\
             不让你猜、也不会只试一个节点就放弃。\n\
             下一步（按这个顺序）：\n\
             \x20 1) 先在节点列表里换一个节点重试 —— 上面的清单就是「哪个能用」的结论；\n\
             \x20 2) 核对清单里每个节点的地址 / 端口是否过期；订阅来的节点点「刷新订阅」重拉一次；\n\
             \x20 3) **只有**换了节点仍然全部不通、而且整台 Mac 直连也上不了网时，\
             才需要换网络（如手机热点）。",
        );
        out
    }

    /// 有没有节点在 **TCP 层可达**（= 本机网络没问题，问题在节点/出口）。
    ///
    /// 判据是 [`NodeFailureClass::EgressBroken`]：它成立的前提正是
    /// 「本机 → 节点 的 TCP 握手成功、但经它转发的真实请求拿不到响应」。
    /// 有这条证据时，文案就**不许**把「换网络」排在前面 —— 用户明确抗议过那条引导。
    pub(crate) fn any_node_tcp_reachable(&self) -> bool {
        self.attempts
            .iter()
            .any(|a| a.class == Some(NodeFailureClass::EgressBroken))
    }

    /// 一行一条的**机器友好**摘要（进日志；给用户看的是 [`Self::all_failed_message`]）。
    ///
    /// 为什么单独一份：日志要能直接 grep 出「哪个节点、什么类别、多久、原文」，
    /// 而用户文案要可读 —— 两者混用必然有一边不合适。
    pub(crate) fn summary_for_log(&self) -> String {
        self.attempts
            .iter()
            .map(|a| {
                format!(
                    "{}:{}@{}:{}:{}ms:{}",
                    a.node_id,
                    a.node_name,
                    a.endpoint(),
                    a.class.map(|c| c.slug()).unwrap_or("ok"),
                    a.elapsed.as_millis(),
                    a.detail.replace('\n', " ")
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// 一次**自动回落的结局**：实际用了哪个节点、为什么换、试了几个。
///
/// # 为什么要有这个类型
///
/// 「首连」与「自动重建（看门狗 / 换网）」走的是**同一条回落策略**
/// （`commands::core` 里唯一的那一个候选循环 `run_node_fallbacks`）。但策略的
/// 结局过去只写进一句内联提示，`start_core` 的返回值是 `Result<(), String>` ——
/// 于是**重建路径拿到 `Ok(())` 时说不出「这次实际用了哪个节点、为什么换」**，
/// 只能写一句「已自动恢复」。这个类型就是那个缺失的返回值。
///
/// # 不许静默改用户选中的节点
///
/// `selected_id` 只用于**对比与文案**：策略从来不写回
/// `settings.selected_node`。`switched()` 为真时 [`Self::notice`] 会明说
/// 「你的选择没有被改动」，`false` 时它返回 `None`（没换就不打扰）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeFallbackOutcome {
    /// 用户选中的节点 id（`None` = 没选）。
    pub selected_id: Option<String>,
    /// 实际成功的那个节点（`class = None`）。
    pub used: NodeAttempt,
    /// 这一次一共试了几个节点（含成功那次）。
    pub attempts: usize,
    /// 选中节点这一次的失败类别；选中节点就是成功那个时为 `None`。
    pub selected_failure: Option<NodeFailureClass>,
    /// 选中节点的 `「名」（addr:port）`；账本里没有就是 `None`。
    pub selected_label: Option<String>,
}

impl NodeFallbackOutcome {
    /// 从「试到成功为止」的账本里取出结局。**纯函数**（不发起任何探测）。
    ///
    /// `used` 必须已经记进 `report`（调用方先 `record` 再构造），
    /// 否则 `attempts` 会比实际少一条。
    pub(crate) fn from_report(
        selected: Option<&str>,
        used: NodeAttempt,
        report: &TrialReport,
    ) -> Self {
        let sel = selected.and_then(|id| report.attempts().iter().find(|a| a.node_id == id));
        let selected_id = selected.map(str::to_string);
        Self {
            selected_failure: sel.and_then(|a| a.class),
            selected_label: sel.map(NodeAttempt::label),
            selected_id,
            used,
            attempts: report.attempts().len(),
        }
    }

    /// 这次是不是**没按用户选中的节点**连（回落发生了）。
    pub(crate) fn switched(&self) -> bool {
        self.selected_id.as_deref() != Some(self.used.node_id.as_str())
    }

    pub(crate) fn used_node_id(&self) -> &str {
        &self.used.node_id
    }

    pub(crate) fn used_node_name(&self) -> &str {
        &self.used.node_name
    }

    /// 选中节点的可读标签（回落到别的节点时用；缺失时退回 id 或「（未选择）」）。
    fn selected_text(&self) -> String {
        self.selected_label
            .clone()
            .or_else(|| self.selected_id.clone())
            .unwrap_or_else(|| "（未选择）".to_string())
    }

    /// 选中节点**为什么**没被用 —— 有类别就给类别，否则如实说本次不可达。
    fn selected_reason(&self) -> &'static str {
        self.selected_failure
            .map(NodeFailureClass::label)
            .unwrap_or("本次不可达")
    }

    /// **一行、进日志**：这次实际用了哪个节点、是不是换了、为什么。
    ///
    /// 无论有没有换都必须能说清「实际用的是哪个」—— 这正是用户要的
    /// 「别静默改我选的节点」。
    pub(crate) fn describe(&self) -> String {
        if !self.switched() {
            return format!("本次实际使用节点{}（就是你选中的那个）", self.used.label());
        }
        format!(
            "本次实际使用节点{}；选中的{} {} —— 已自动回落（共试了 {} 个节点，\
             你的选择没有被改动）",
            self.used.label(),
            self.selected_text(),
            self.selected_reason(),
            self.attempts,
        )
    }

    /// 给用户的提示条：**只在实际换了节点时**有内容（没换不打扰）。
    ///
    /// 三件事缺一不可：换了哪个、为什么换、**你的选择没有被改动**。
    pub(crate) fn notice(&self) -> Option<String> {
        if !self.switched() {
            return None;
        }
        Some(format!(
            "原选中节点{} {}（本次共试了 {} 个节点），已自动改用节点{}连接。\
             你的选择没有被改动 —— 设置里仍然是原节点；要固定用新节点，\
             请在节点列表里手动选中它。",
            self.selected_text(),
            self.selected_reason(),
            self.attempts,
            self.used.label(),
        ))
    }
}

/// 自动回落的**候选顺序**（纯函数，可测）。
///
/// 排序依据（都来自既有事实，不发起新探测）：
/// 1. **用户选中的节点永远排第一** —— 先尊重用户的选择，不因为它上次失败就跳过；
/// 2. 其次 `last_good`（**验证过**能用的节点）；
/// 3. 其余按「测延迟」的真实可用性排：`available=true` 在前（按 `server_rtt_ms` 升序），
///    然后**没测过**的（可能只是没点过"测延迟"），最后是测过但 `available=false` 的。
///
/// 注意：**所有节点都会被排进列表**（`rank_candidates().len() == nodes.len()`），
/// 这是「全试完才报错」的前提。
pub(crate) fn rank_candidates(
    selected: Option<&str>,
    nodes: &[Node],
    latencies: &HashMap<String, ProbeResult>,
    last_good: Option<&str>,
) -> Vec<String> {
    let key = |id: &str| -> (u8, u32) {
        match latencies.get(id) {
            // 真实可用（经节点取到过东西）→ 按 RTT 升序；RTT 未知排在可用组的末尾。
            Some(r) if r.available => (0, r.server_rtt_ms.unwrap_or(u32::MAX)),
            // 没测过 —— 不能当成坏节点。
            None => (1, 0),
            // 测过、明确不可用 —— 排最后（但还是要试：用户可能已修好网络）。
            Some(_) => (2, 0),
        }
    };

    let mut rest: Vec<&Node> = nodes
        .iter()
        .filter(|n| Some(n.id.as_str()) != selected && Some(n.id.as_str()) != last_good)
        .collect();
    rest.sort_by(|a, b| key(&a.id).cmp(&key(&b.id)).then_with(|| a.id.cmp(&b.id)));

    let mut ordered: Vec<String> = Vec::with_capacity(nodes.len());
    if let Some(sel) = selected {
        if nodes.iter().any(|n| n.id == sel) {
            ordered.push(sel.to_string());
        }
    }
    if let Some(good) = last_good {
        if nodes.iter().any(|n| n.id == good) && !ordered.iter().any(|o| o == good) {
            ordered.push(good.to_string());
        }
    }
    ordered.extend(rest.into_iter().map(|n| n.id.clone()));
    ordered
}

/// 同一节点连续失败且来自订阅时，给一条「重拉订阅」的提示。
///
/// 达到 [`CONSECUTIVE_FAILURES_BEFORE_SUB_REFRESH`] 次才提示：低于它只是抖动。
/// 手工添加的节点没有订阅可重拉，返回 `None`（由调用方判断）。
pub(crate) fn subscription_refresh_hint(
    node_name: &str,
    subscription_name: &str,
    consecutive_failures: u32,
) -> Option<String> {
    if consecutive_failures < CONSECUTIVE_FAILURES_BEFORE_SUB_REFRESH {
        return None;
    }
    Some(format!(
        "节点「{node_name}」已连续 {consecutive_failures} 次不可达，且它来自订阅「{subscription_name}」——\
         可能是订阅里的地址过期了：点「刷新订阅」重拉一次列表（App 不会自动改你选中的节点）。"
    ))
}

/// 本机 SOCKS 端口现在是否空闲。
///
/// 用「能不能 bind 上」判定：这与核心真正去 `listen` 时的条件一致，
/// 比读某个进程列表可靠。**不保留**这个 listener（drop 即释放）。
pub(crate) fn local_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 按 id 取节点的**显示名**；找不到（或 `id` 为 `None`）时返回 `fallback`。
///
/// # 为什么集中这一处（简化，不是取巧）
///
/// 「id → 名字」在本 crate 里手写了 6 处以上，写法都是
/// `nodes.iter().find(|n| n.id == id).map(|n| n.name.clone()).unwrap_or_else(...)`，
/// 而**兜底文案各不相同**（有的是 `""`、有的是 `"（未选择）"`）。集中成一个函数后：
/// 只有一处需要知道「怎么按 id 找节点」，调用点只保留自己的兜底文案 ⇒ **行为逐字不变**。
pub(crate) fn node_name_or(nodes: &[Node], id: Option<&str>, fallback: &str) -> String {
    id.and_then(|id| nodes.iter().find(|n| n.id == id))
        .map(|n| n.name.clone())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::{NodeSource, Protocol};

    fn node(id: &str, name: &str, address: &str) -> Node {
        Node {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            port: 443,
            protocol: Protocol::Vless {
                uuid: "00000000-0000-0000-0000-000000000000".into(),
                flow: String::new(),
                encryption: "none".into(),
            },
            transport: Default::default(),
            tls: Default::default(),
            mux: None,
            source: NodeSource::Manual,
            tags: Vec::new(),
            raw_uri: None,
        }
    }

    fn probe(node_id: &str, available: bool, rtt: Option<u32>) -> ProbeResult {
        ProbeResult {
            node_id: node_id.into(),
            node_name: node_id.into(),
            server_rtt_ms: rtt,
            available,
            through_node_ms: None,
            http_status: if available { Some(204) } else { None },
            error: None,
            tested_at: 0,
        }
    }

    // ---------------------------------------------------------------- 三分类

    /// **本机→节点 TCP 不通**：用两条真实原文 —— 预检失败串（用户报的那条）
    /// 与 DNS 解析失败串。
    #[test]
    fn tcp_unreachable_uses_the_real_error_texts() {
        // 原文：`Supervisor::start` 的 TCP 预检失败
        let precheck = "接管默认路由之前就联系不上代理服务器 45.207.197.185:443\
                        （第1次失败、第2次失败，每次 4 秒）。\n请检查节点地址 / 端口…";
        assert_eq!(
            classify(&FailureFacts {
                message: precheck,
                node_tcp_ok: None,
                local_port_free: Some(true)
            }),
            NodeFailureClass::TcpUnreachable
        );
        // 原文：`resolve_server_addrs`
        let dns = "无法解析节点地址「node.example.com」。TUN 模式需要先知道服务器的 IP…";
        assert_eq!(
            classify(&FailureFacts {
                message: dns,
                node_tcp_ok: None,
                local_port_free: Some(true)
            }),
            NodeFailureClass::TcpUnreachable
        );
        // 事实优先：预检明确说不可达
        assert_eq!(
            classify(&FailureFacts {
                message: "启动失败",
                node_tcp_ok: Some(false),
                local_port_free: Some(true)
            }),
            NodeFailureClass::TcpUnreachable
        );
        // 系统调用原文（不同平台/内核给出不同串）
        for sys in ["Network is unreachable", "No route to host", "Connection refused"] {
            assert_eq!(
                classify(&FailureFacts {
                    message: sys,
                    node_tcp_ok: None,
                    local_port_free: Some(true)
                }),
                NodeFailureClass::TcpUnreachable,
                "系统原文 `{sys}` 应归到 TCP 不通"
            );
        }
    }

    /// **节点可达但出口不通**：门禁的真实文案（TCP 通、真实请求拿不到响应）。
    #[test]
    fn egress_broken_uses_the_gate_message() {
        let gate = "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应：http://x/（000）。\
                    **已在接管默认路由之前中止**，系统网络未被改动。";
        assert_eq!(
            classify(&FailureFacts {
                message: gate,
                node_tcp_ok: Some(true),
                local_port_free: Some(true)
            }),
            NodeFailureClass::EgressBroken
        );
        // 即使没有 TCP 事实，光看门禁原文也要认出来。
        assert_eq!(
            classify(&FailureFacts {
                message: gate,
                node_tcp_ok: None,
                local_port_free: Some(true)
            }),
            NodeFailureClass::EgressBroken
        );
    }

    /// **本地端口问题**：端口不空闲这一事实最硬；其次是 bind 系原文。
    ///
    /// 但**节点侧原文优先**：端口恰好被占不该把一句「联系不上代理服务器」
    /// 改写成端口问题（那是另一个独立故障，改写了就把用户引去修错的东西）。
    #[test]
    fn local_port_evidence_beats_generic_failures() {
        let asserts = [
            FailureFacts {
                message: "启动失败：节点不可达", // 文案很泛，没有任何节点侧特征
                node_tcp_ok: None,
                local_port_free: Some(false),
            },
            FailureFacts {
                message: "listen tcp 127.0.0.1:10808: bind: address already in use",
                node_tcp_ok: Some(true),
                local_port_free: Some(true),
            },
            FailureFacts {
                message: "Failed to connect to 127.0.0.1 port 10808 after 0 ms: Couldn't connect to server",
                node_tcp_ok: Some(true),
                local_port_free: Some(true),
            },
        ];
        for facts in asserts {
            assert_eq!(
                classify(&facts),
                NodeFailureClass::LocalPort,
                "没有节点侧特征时，本地端口证据优先：{facts:?}"
            );
        }

        // 反例：节点侧原文明确时，哪怕端口也被占，类别仍是「节点不可达」。
        let node_text = "接管默认路由之前就联系不上代理服务器 1.1.1.1:443（第1次失败、第2次失败，每次 4 秒）";
        assert_eq!(
            classify(&FailureFacts {
                message: node_text,
                node_tcp_ok: None,
                local_port_free: Some(false),
            }),
            NodeFailureClass::TcpUnreachable,
            "节点侧原文不该被端口事实改写成端口问题"
        );
    }

    /// 认不出来就说不知道 —— 不硬塞进另外三类。
    #[test]
    fn unknown_stays_unknown() {
        assert_eq!(
            classify(&FailureFacts {
                message: "内核返回了一个我们从没见过的错误",
                node_tcp_ok: None,
                local_port_free: Some(true)
            }),
            NodeFailureClass::Unknown
        );
    }

    /// 换节点有没有意义 —— 本地端口问题换多少节点都白搭。
    #[test]
    fn only_node_level_classes_worth_another_node() {
        assert!(NodeFailureClass::TcpUnreachable.is_node_level());
        assert!(NodeFailureClass::EgressBroken.is_node_level());
        assert!(NodeFailureClass::Unknown.is_node_level());
        assert!(!NodeFailureClass::LocalPort.is_node_level());
    }

    // ------------------------------------------- 全挂时的节点级失败清单（验收）

    /// **验收判据 ② 的文案面**：两个节点全挂 ⇒ 清单里两个节点**各自一条**，
    /// 每条都带**名字 + 地址:端口 + 类别 + 耗时 + 下一步**；
    /// 并说清「系统网络没被动过」与「App 做不到什么」。
    ///
    /// **为什么必须带地址**（这是本次修的缺口）：用户原话就是
    /// 「请检查节点地址 / 端口」，而旧清单只写节点**名字** —— 名字是用户自己起的，
    /// 照着它核不了任何东西。
    ///
    /// 判别性：把 `NodeAttempt` 的 `address`/`port` 从清单里去掉 ⇒ 前两条断言红。
    #[test]
    fn two_dead_nodes_produce_a_per_node_failure_list() {
        let a = node("n-a", "香港 A", "1.1.1.1");
        let b = node("n-b", "日本 B", "2.2.2.2");
        let facts_a = FailureFacts {
            message: "接管默认路由之前就联系不上代理服务器 1.1.1.1:443\
                      （第1次失败、第2次失败，每次 4 秒）",
            node_tcp_ok: None,
            local_port_free: Some(true),
        };
        let facts_b = FailureFacts {
            message: "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应",
            node_tcp_ok: Some(true),
            local_port_free: Some(true),
        };

        let mut report = TrialReport::new();
        report.record(NodeAttempt::failed(
            &a,
            classify(&facts_a),
            Duration::from_millis(8400),
            facts_a.message,
        ));
        report.record(NodeAttempt::failed(
            &b,
            classify(&facts_b),
            Duration::from_millis(6200),
            facts_b.message,
        ));

        let msg = report.all_failed_message();
        assert_eq!(report.attempts().len(), 2, "每个节点都要有一条结果");
        assert!(
            report.attempts().iter().all(|a| a.class.is_some()),
            "两个都挂了，不该有成功"
        );
        assert!(msg.contains("1. 节点「香港 A」（1.1.1.1:443）"), "{msg}");
        assert!(msg.contains("2. 节点「日本 B」（2.2.2.2:443）"), "{msg}");
        assert!(msg.contains("本机→节点 TCP 不通"), "A 的类别要写清：{msg}");
        assert!(msg.contains("节点可达但出口不通"), "B 的类别要写清：{msg}");
        assert!(msg.contains("8.4s"), "A 的耗时要写清：{msg}");
        assert!(msg.contains("6.2s"), "B 的耗时要写清：{msg}");
        assert!(msg.contains("换一个节点"), "B 的下一步：{msg}");
        assert!(msg.contains("默认路由没有被接管"), "好消息必须保留：{msg}");
        assert!(
            msg.contains("不能**让一个不可达的节点变得可达"),
            "诚实边界必须写进文案：{msg}"
        );
        // B 是 EgressBroken（TCP 可达）⇒ 文案必须**用证据**说本机网络没问题。
        assert!(
            report.any_node_tcp_reachable(),
            "B 的类别是「节点可达但出口不通」⇒ 本机网络已被证明是通的"
        );
        assert!(msg.contains("本机网络本身没问题"), "{msg}");
    }

    /// **验收判据 ③（文案顺序）**：本机网络正常、只是节点不通时，
    /// **不许**把「先换一个网络（例如切到热点）」列在第一位 —— 用户已明确抗议。
    ///
    /// 判别性：把 `TcpUnreachable::advice` 改回「换一个网络（如手机热点）重试；…」
    /// ⇒ 这条红。
    #[test]
    fn network_switch_is_never_the_first_thing_we_tell_the_user() {
        let tcp = NodeFailureClass::TcpUnreachable.advice();
        assert!(
            !tcp.starts_with("换一个网络") && !tcp.starts_with("先换一个网络"),
            "「换网络」不许排第一：{tcp}"
        );
        assert!(
            tcp.starts_with("先在节点列表里换一个节点"),
            "第一步必须是 App 里就能做、而且指向真正原因的动作：{tcp}"
        );
        assert!(
            tcp.contains("只有") && tcp.contains("整台 Mac"),
            "「换网络」必须带前提（整台 Mac 都上不了网）：{tcp}"
        );

        // 全挂清单也一样：讲完「替用户试过哪些节点、各自怎么失败」之后，
        // 动作里的「换网络」必须排在最后，且带同一个前提。
        let a = node("n-a", "香港 A", "1.1.1.1");
        let mut report = TrialReport::new();
        report.record(NodeAttempt::failed(
            &a,
            NodeFailureClass::TcpUnreachable,
            Duration::from_millis(8400),
            "接管默认路由之前就联系不上代理服务器 1.1.1.1:443（第1次失败、第2次失败，每次 4 秒）",
        ));
        let msg = report.all_failed_message();
        let list_at = msg.find("1. 节点「香港 A」").expect("清单要在");
        let switch_at = msg.find("换网络").expect("最后仍要允许换网络");
        let node_action_at = msg.find("先在节点列表里换一个节点").expect("换节点动作要在");
        assert!(
            list_at < node_action_at && node_action_at < switch_at,
            "顺序必须是：节点清单 → 换节点 → 换网络：\n{msg}"
        );
    }

    // ------------------------------------------- 自动回落的结局（验收判据 ①）

    /// **验收判据 ① 的文案面**：选中节点不通、另一个节点通 ⇒
    /// 回落成功，而且**结局里说清「实际用了哪个、为什么换」**，
    /// 同时明说**用户选中的节点没有被改动**。
    ///
    /// 判别性：把 `FallbackOutcome::notice` 改回旧的
    /// 「原选中节点「A」不可达……已自动改用「B」连接」（不带地址与类别）
    /// ⇒ 这条红。
    #[test]
    fn a_successful_fallback_says_which_node_and_why_without_touching_the_choice() {
        let a = node("n-a", "香港 A", "1.1.1.1");
        let b = node("n-b", "日本 B", "2.2.2.2");
        let mut report = TrialReport::new();
        report.record(NodeAttempt::failed(
            &a,
            NodeFailureClass::TcpUnreachable,
            Duration::from_millis(8400),
            "接管默认路由之前就联系不上代理服务器 1.1.1.1:443（第1次失败、第2次失败，每次 4 秒）",
        ));
        let used = NodeAttempt::ok(&b, Duration::from_millis(1500));
        report.record(used.clone());

        let choice = NodeFallbackOutcome::from_report(Some("n-a"), used, &report);
        assert!(choice.switched(), "选中 A、实际用 B ⇒ 必须认得出「换了」");
        assert_eq!(choice.used_node_id(), "n-b");
        assert_eq!(choice.used_node_name(), "日本 B");
        assert_eq!(choice.attempts, 2, "账本要记满两个节点");

        // ① 用了哪个：name + address:port 都要在（用户要照着核对）。
        let desc = choice.describe();
        assert!(desc.contains("日本 B"), "{desc}");
        assert!(desc.contains("2.2.2.2:443"), "{desc}");
        // ② 为什么换：选中节点的**类别**要在，不能只写一句「不可达」。
        assert!(desc.contains("香港 A"), "{desc}");
        assert!(desc.contains("1.1.1.1:443"), "{desc}");
        assert!(desc.contains("本机→节点 TCP 不通"), "{desc}");

        let notice = choice.notice().expect("换了节点就必须有提示条");
        assert!(notice.contains("已自动改用节点「日本 B」（2.2.2.2:443）"), "{notice}");
        assert!(notice.contains("本机→节点 TCP 不通"), "要说清为什么换：{notice}");
        assert!(
            notice.contains("你的选择没有被改动"),
            "不许静默改用户的选择，这句必须在：{notice}"
        );

        // 反向：没换节点 ⇒ 不许打扰（`notice` 为 None），但日志仍要能说出用了哪个。
        let mut same = TrialReport::new();
        let used_a = NodeAttempt::ok(&a, Duration::from_millis(900));
        same.record(used_a.clone());
        let kept = NodeFallbackOutcome::from_report(Some("n-a"), used_a, &same);
        assert!(!kept.switched());
        assert!(kept.notice().is_none(), "没换就别弹提示条");
        assert!(
            kept.describe().contains("就是你选中的那个"),
            "{}",
            kept.describe()
        );
        assert!(kept.describe().contains("1.1.1.1:443"), "{}", kept.describe());
    }

    /// 一个成功一个失败 ⇒ 清单里也要有，但 `succeeded()` 认得出成功那个。
    #[test]
    fn report_remembers_which_node_succeeded() {
        let a = node("n-a", "香港 A", "1.1.1.1");
        let b = node("n-b", "日本 B", "2.2.2.2");
        let mut report = TrialReport::new();
        report.record(NodeAttempt::failed(
            &a,
            NodeFailureClass::TcpUnreachable,
            Duration::from_millis(8400),
            "预检两次都失败",
        ));
        report.record(NodeAttempt::ok(&b, Duration::from_millis(1500)));
        let ok = report
            .attempts()
            .iter()
            .find(|a| a.class.is_none())
            .expect("应当记得成功的是哪个节点");
        assert_eq!(ok.node_id, "n-b");
        assert_eq!(ok.node_name, "日本 B");
        assert!(ok.class.is_none());
    }

    // ------------------------------------------------------------ 回落顺序

    #[test]
    fn selected_node_is_always_first_even_if_it_looked_bad() {
        let nodes = vec![
            node("n-a", "A", "1.1.1.1"),
            node("n-b", "B", "2.2.2.2"),
        ];
        let mut lat = HashMap::new();
        lat.insert("n-a".to_string(), probe("n-a", false, None)); // 测过不可用
        lat.insert("n-b".to_string(), probe("n-b", true, Some(30)));
        let order = rank_candidates(Some("n-a"), &nodes, &lat, None);
        assert_eq!(order, vec!["n-a".to_string(), "n-b".to_string()], "用户的选择永远先试");
    }

    #[test]
    fn available_before_untested_before_known_dead_and_rtt_sorted() {
        let nodes = vec![
            node("dead", "Dead", "1.1.1.1"),
            node("untested", "Untested", "2.2.2.2"),
            node("slow", "Slow", "3.3.3.3"),
            node("fast", "Fast", "4.4.4.4"),
        ];
        let mut lat = HashMap::new();
        lat.insert("dead".to_string(), probe("dead", false, Some(10))); // RTT 有但不可用
        lat.insert("slow".to_string(), probe("slow", true, Some(200)));
        lat.insert("fast".to_string(), probe("fast", true, Some(40)));
        let order = rank_candidates(Some("dead"), &nodes, &lat, None);
        assert_eq!(
            order,
            vec!["dead", "fast", "slow", "untested"],
            "选中的先试；其余按真实可用性（available + RTT）排，没测过的排在已知死节点前"
        );
        assert_eq!(order.len(), nodes.len(), "所有节点都要进列表（全试完才报错）");
    }

    #[test]
    fn last_good_comes_right_after_the_selected_node() {
        let nodes = vec![
            node("new", "New", "1.1.1.1"),
            node("old", "Old", "2.2.2.2"),
            node("other", "Other", "3.3.3.3"),
        ];
        let lat = HashMap::new();
        let order = rank_candidates(Some("new"), &nodes, &lat, Some("old"));
        assert_eq!(order, vec!["new", "old", "other"]);
    }

    // ------------------------------------------------------ 订阅重拉提示

    #[test]
    fn subscription_hint_only_after_three_consecutive_failures() {
        assert!(subscription_refresh_hint("香港 A", "机场 X", 1).is_none());
        assert!(subscription_refresh_hint("香港 A", "机场 X", 2).is_none());
        let hint = subscription_refresh_hint("香港 A", "机场 X", 3).expect("第 3 次必须提示");
        assert!(hint.contains("香港 A"), "{hint}");
        assert!(hint.contains("机场 X"), "要说清是哪个订阅：{hint}");
        assert!(hint.contains("刷新订阅"), "要给下一步：{hint}");
        assert!(
            hint.contains("不会自动改你选中的节点"),
            "不许静默改用户的选择，这句要有：{hint}"
        );
    }

    // ------------------------------------------------------------ 端口预检

    /// 端口预检必须真的能测出「被占用」：占一个，断言 false；放开，断言 true。
    #[test]
    fn local_port_free_detects_a_bound_socket() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("绑定一个随机端口");
        let port = listener.local_addr().expect("拿到端口").port();
        assert!(!local_port_free(port), "正在被占用的端口必须判成不空闲");
        drop(listener);
        assert!(local_port_free(port), "放开之后必须判成空闲");
    }

    /// `node_name_or` 必须**逐字保留调用点各自的兜底文案**（集中化的等价性论据）。
    #[test]
    fn node_name_or_keeps_each_callsite_fallback_text() {
        let nodes = vec![node("n-a", "香港 A", "1.1.1.1")];
        assert_eq!(node_name_or(&nodes, Some("n-a"), "（未选择）"), "香港 A");
        assert_eq!(
            node_name_or(&nodes, Some("不存在"), "（未选择）"),
            "（未选择）",
            "找不到时用调用点的兜底文案"
        );
        assert_eq!(node_name_or(&nodes, None, ""), "", "连通性检查用的兜底是空串");
        assert_eq!(node_name_or(&[], Some("n-a"), "?"), "?");
    }
}
