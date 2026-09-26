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
    pub(crate) fn advice(self) -> &'static str {
        match self {
            Self::TcpUnreachable => {
                "换一个网络（如手机热点）重试；核对节点地址/端口；或点「刷新订阅」重拉一次节点列表"
            }
            Self::EgressBroken => {
                "换一个节点；这个节点本身能连上，但它转发不出去（墙或节点出口的问题）"
            }
            Self::LocalPort => {
                "本地端口被占用或被拒绝：把「设置」里的 SOCKS 端口换一个，或关掉占用它的程序后重试"
            }
            Self::Unknown => "把这条错误原文发给开发者；同时可以先换一个节点或换网络试试",
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
#[derive(Debug, Clone)]
pub(crate) struct NodeAttempt {
    pub node_id: String,
    pub node_name: String,
    /// 失败类别。**成功**的尝试也给一个（`None` 表示成功）。
    pub class: Option<NodeFailureClass>,
    pub elapsed: Duration,
    pub detail: String,
}

impl NodeAttempt {
    pub(crate) fn ok(node: &Node, elapsed: Duration) -> Self {
        Self {
            node_id: node.id.clone(),
            node_name: node.name.clone(),
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
            class: Some(class),
            elapsed,
            detail: detail.into(),
        }
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
    /// 形状：先说清「网络没被动过」，再逐节点列（类别 + 用时 + 各自的下一步），
    /// 最后给一条**不承诺做不到的事**的总体说明。
    pub(crate) fn all_failed_message(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "试了 {} 个节点都没能建立可用隧道，本次启动已中止：\
             **默认路由没有被接管、系统网络没有被改动**。\n各节点的结果：\n",
            self.attempts.len()
        ));
        for (i, a) in self.attempts.iter().enumerate() {
            let class = a.class.unwrap_or(NodeFailureClass::Unknown);
            out.push_str(&format!(
                "  {}. 节点「{}」：{}（{:.1}s）—— 下一步：{}\n",
                i + 1,
                a.node_name,
                class.label(),
                a.elapsed.as_secs_f32(),
                class.advice(),
            ));
        }
        out.push_str(
            "说明：App **不能**让一个不可达的节点变得可达（节点宕机、地址写错、\
             出网链路被挡都在我们之外）。这里能保证的是：不接管你的默认路由、\
             不让你猜、也不会只试一个节点就放弃。\n\
             下一步：换一个网络（如手机热点）后重试；如果这些节点都来自订阅，\
             点「刷新订阅」重拉一次列表。",
        );
        out
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
                    "{}:{}:{}:{}ms:{}",
                    a.node_id,
                    a.node_name,
                    a.class.map(|c| c.slug()).unwrap_or("ok"),
                    a.elapsed.as_millis(),
                    a.detail.replace('\n', " ")
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
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

    /// **验收判据**：两个节点全挂 ⇒ 清单里两个节点**各自一条**，
    /// 带类别与下一步；并说清「系统网络没被动过」与「App 做不到什么」。
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
        assert!(msg.contains("1. 节点「香港 A」"), "{msg}");
        assert!(msg.contains("2. 节点「日本 B」"), "{msg}");
        assert!(msg.contains("本机→节点 TCP 不通"), "A 的类别要写清：{msg}");
        assert!(msg.contains("节点可达但出口不通"), "B 的类别要写清：{msg}");
        assert!(msg.contains("换一个网络"), "A 的下一步：{msg}");
        assert!(msg.contains("换一个节点"), "B 的下一步：{msg}");
        assert!(msg.contains("默认路由没有被接管"), "好消息必须保留：{msg}");
        assert!(
            msg.contains("不能**让一个不可达的节点变得可达"),
            "诚实边界必须写进文案：{msg}"
        );
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
}
