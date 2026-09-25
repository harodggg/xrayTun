//! 核心与隧道的生命周期编排。
//!
//! # 启动顺序是这里的全部难点
//!
//! TUN 模式下的正确顺序（每一步都有理由，不能随意调换）：
//!
//! ```text
//! 1. 探测物理出口（route -n get default）        ← 必须在建卡之前，否则拿到的是 utun
//! 2. helper.TunUp(defer=true)                     ← 建 utun + 配地址 + 装 bypass 路由
//! 3. helper.TakeTunFd                             ← SCM_RIGHTS 取回 fd
//! 4. 清除 fd 的 FD_CLOEXEC 并 spawn xray            ← 否则子进程继承不到 fd
//! 5. 等待 SOCKS 端口可连                            ← 唯一的、与版本无关的就绪信号
//! 6. helper.CommitRoutes                          ← 此时才接管默认路由 + 切 DNS
//! ```
//!
//! 第 6 步放在最后，是为了让「流量被接管」与「数据面可用」之间**没有窗口**。
//! 如果第 2 步就把默认路由接管了，第 5 步之前的几百毫秒里所有流量会被送进
//! 一个还没人读的 utun，同时 DNS 也已经指向隧道内的哨兵地址 ——
//! 用户看到的现象是「一连上所有网页都打不开」。
//!
//! 停止时顺序相反，并且**先杀数据面再回滚网络**：数据面还持有 utun fd，
//! 不先停掉的话接口不会消失，内核可能拒绝删除挂在它上面的路由。

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xt_core::model::{AppSettings, Node, ProxyMode};
use xt_core::store::Store;
use xt_core::xray::{
    self, CoreConfigInput, CoreEvent, InboundProfile, ProbeOptions, ProbeResult, XrayProcess,
    MIN_CORE_VERSION_NATIVE_TUN,
};
use xt_proto::{DatapathPlan, DefaultRouteMode, DnsMode, Request, TunUpRequest};
use xt_tun::macos::route::{RouteAudit, RouteAuditLog, RouteAuditPhase};

use crate::helper_client::HelperClient;
use crate::state::{profile_for, CoreRuntime};

/// 启动核心的总超时。超过这个时间认为「起不来了」，走回滚。
const CORE_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 关核心时的优雅期，超过就 SIGKILL。
const CORE_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// 探测代理服务器可达性的单次超时。
///
/// 不能太长：这是在启动路径上，用户正在等。
/// 也不能太短：跨国线路首次握手可能要 1 秒以上。
const REGION_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// 端到端门禁失败后、重试之前的停顿。
///
/// 与 `tcp_reachable_with_retry` 的 700ms 同一个量级，理由也同一条：
/// 刚睡醒 / 刚换网 / 节点侧瞬时丢包时，一次单发探测的失败不等于"这条路走不通"。
const GATE_RETRY_PAUSE: Duration = Duration::from_millis(500);

/// 预览 TUN 启动将要做的网络改动（**只读，不改动任何东西**）。
///
/// 与 `start()` 走的是同一套计算：探测物理出口 → 解析服务器地址 →
/// 生成请求 → 编译成路由计划。所以预览出来的东西就是真会执行的东西。
pub fn preview_tun_plan(
    settings: &AppSettings,
    node: &Node,
) -> Result<xt_tun::plan::TunPlan, String> {
    let supervisor = Supervisor::default();
    let dr = xt_tun::macos::route::default_route().map_err(|e| e.to_string())?;
    let physical = xt_tun::plan::PhysicalUplink {
        interface: dr.interface.clone(),
        gateway: dr.gateway,
        service: xt_tun::macos::dns::service_for_device(&dr.interface).ok(),
    };
    let server_addrs = resolve_server_addrs(Some(node))?;
    let request = supervisor.build_tun_request(settings, physical.gateway, &server_addrs)?;
    xt_tun::plan::build_plan(&request, physical).map_err(|e| e.to_string())
}

/// 解析选中节点的服务器地址。
fn resolve_server_addrs(node: Option<&Node>) -> Result<Vec<std::net::IpAddr>, String> {
    let node = node.ok_or("TUN 模式必须选中一个节点")?;
    let addrs = xt_core::net::resolve_host(&node.address);
    if addrs.is_empty() {
        return Err(format!(
            "无法解析节点地址「{}」。TUN 模式需要先知道服务器的 IP 才能给它\n\
             单独留一条走物理出口的路由，否则会形成路由环。",
            node.address
        ));
    }
    Ok(addrs)
}

fn println_servers(addrs: &[std::net::IpAddr]) {
    let list = addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(servers = %list, "已解析代理服务器地址，将为它们安装 bypass host 路由");
}

/// 异步版的可达性探测（把阻塞的 `connect` 丢到阻塞线程池，不卡住 runtime）。
async fn tcp_reachable(addr: std::net::SocketAddr, timeout: Duration) -> bool {
    tokio::task::spawn_blocking(move || xt_core::net::tcp_reachable(addr, timeout))
        .await
        .unwrap_or(false)
}

/// 单次 TCP 探测，**失败再试一次**；返回 `(是否可达, 两次的轨迹)`。
///
/// # 为什么要重试（这不是"保守"，是修一个真实的误判）
///
/// 这是一次**单发的 4 秒 TCP 握手**，而它的结果决定「整次启动中止 + 回滚」。
/// 刚睡醒、刚换网、首个连接走慢路径时，一次超时**不等于**"节点不可达"。
/// 实测：同一台机器上节点直连 116 ms 可达，但用户遇到过一次这样的中止 ——
/// 错误文案还把瞬时超时归因成「请检查节点地址 / 端口」，往错误方向指路。
///
/// 重试一次能把「瞬时抖动」与「真的不可达」分开；轨迹进日志与错误文案，
/// 于是"到底是几次都失败"这件事可见，而不是一句笼统的"联系不上"。
async fn tcp_reachable_with_retry(addr: std::net::SocketAddr, timeout: Duration) -> (bool, String) {
    tcp_reachable_with_retry_by(|| tcp_reachable(addr, timeout), Duration::from_millis(700)).await
}

/// 可注入探测函数的版本（测试用：不碰真网络也能钉住"重试一次"的语义）。
async fn tcp_reachable_with_retry_by<F, Fut>(mut probe: F, pause: Duration) -> (bool, String)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut trace: Vec<String> = Vec::new();
    for attempt in 1..=2u32 {
        if attempt > 1 {
            tokio::time::sleep(pause).await;
        }
        if probe().await {
            trace.push(format!("第{attempt}次成功"));
            return (true, trace.join("、"));
        }
        trace.push(format!("第{attempt}次失败"));
    }
    (false, trace.join("、"))
}

/// 经**本机 SOCKS 入站**发一个真实 HTTP 请求，返回 HTTP 状态码（拿不到时空串）。
///
/// `--socks5-hostname` 让**节点**去解析域名，所以这一个检查同时覆盖
/// 「能不能转发」与「节点侧能不能解析」；而且**不依赖本机 DNS** ——
/// 这点很关键：连接期间系统的 DNS 已被换成隧道内哨兵地址，用本机解析
/// 会测出假结果。
///
/// # 为什么放在 supervisor 而不是 commands/core.rs
///
/// 它是接管默认路由**之前**那道端到端门禁的实现，而门禁在
/// [`Supervisor::start`] 里。看门狗与连通性检查（`commands/core.rs`）
/// 也复用同一实现（那边只是薄封装 `tunnel_probe`）—— 一处实现、两处调用，
/// 依赖方向仍是 `commands → supervisor`（本来就存在）。
/// 复制第二份 curl 调用必然漂移，正是本项目反复踩过的坑。
/// 域名由**谁**解析 —— 这是一个产品语义选择，不是 curl 参数细节。
///
/// # 为什么必须显式区分（2026-09-25 现场踩到）
///
/// 上面那条注释写着域名目标的职责是"**域名经核心 dns 模块能不能解析**"，
/// 但实现一直是 `--socks5-hostname` ⇒ 实际测的是**节点解析**。两回事：
///
/// * **TUN 模式**：域名由**核心**的 `dns-out` 解析，节点只收到 IP
///   ⇒ 节点侧 DNS 坏不坏**不影响**真实使用；
/// * **系统代理（socks/http）模式**：客户端可能把域名交给节点
///   ⇒ 节点侧 DNS 就是链路上真的一环。
///
/// 现场（用户报错）正是这个错位的后果：节点 IP 字面量目标全通、域名目标全挂
/// （节点侧 DNS 被污染/不可用），于是**接管前的门禁在 TUN 模式下把用户拦在门外** ——
/// 而按 TUN 的真实数据路径，这个节点是可用的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeResolve {
    /// 让**节点**解析（`--socks5-hostname`）：系统代理模式下这才是真实路径。
    AtNode,
    /// **本机解析**后把 IP 交给节点（`--socks5`）：TUN 模式下这才是真实路径
    /// （核心解析 → 节点只看到 IP）。
    Locally,
}

/// curl 的参数表（**纯函数，可逐字断言**）。
///
/// 抽出来是因为这里每一个参数都决定"这条探测到底在证明什么"：
/// `--socks5` 与 `--socks5-hostname` 只差一个后缀，语义却是"谁解析域名"。
pub(crate) fn curl_probe_args(
    port: u16,
    target: &str,
    timeout_secs: u32,
    resolve: ProbeResolve,
) -> Vec<String> {
    vec![
        "-sS".into(),
        "-o".into(),
        "/dev/null".into(),
        "-w".into(),
        "%{http_code}".into(),
        "--max-time".into(),
        timeout_secs.to_string(),
        match resolve {
            ProbeResolve::AtNode => "--socks5-hostname",
            ProbeResolve::Locally => "--socks5",
        }
        .into(),
        format!("127.0.0.1:{port}"),
        target.to_string(),
    ]
}

/// 经本机 SOCKS 发一次探测（**默认让节点解析**，与改造前的行为一致）。
pub(crate) async fn socks_http_probe(port: u16, target: String, timeout_secs: u32) -> String {
    socks_http_probe_with(port, target, timeout_secs, ProbeResolve::AtNode).await
}

/// 同上，但显式指定"域名由谁解析"。
pub(crate) async fn socks_http_probe_with(
    port: u16,
    target: String,
    timeout_secs: u32,
    resolve: ProbeResolve,
) -> String {
    tokio::task::spawn_blocking(move || {
        let args = curl_probe_args(port, &target, timeout_secs, resolve);
        std::process::Command::new("/usr/bin/curl")
            .args(&args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 接管默认路由**之前**的端到端门禁
// ---------------------------------------------------------------------------

/// 接管默认路由之前必须通过的探测目标。
///
/// **三条目标不是冗余，是「两类职责 × 两条路径」**：
///
/// | 目标 | 路径 | 职责 |
/// |---|---|---|
/// | `http://1.1.1.1/` | 境外（代理链路） | **传输**：经隧道能不能把包送到（IP 字面量，**不依赖解析**） |
/// | `http://cp.cloudflare.com/generate_204` | 境外 | **解析/端到端**：域名这条路能不能走通（**由谁解析由模式决定**，见 [`ProbeResolve`]）|
/// | `http://223.5.5.5/` | 境内（`geoip:cn` → direct） | **传输**：CN 直连路径通不通（IP 字面量，**不依赖解析**） |
///
/// # 为什么要成对（task-92）
///
/// 原来两条全是域名，而 `socks_http_probe` 用 `--socks5-hostname` 把域名交给
/// **核心**解析 ⇒ **解析一坏，就把整条其实活着的链路判死**，而且分不清
/// 「传输坏」与「只是解析坏」。**实测活证**（task-91，同一时刻、同一台机器）：
/// 境内 IP 字面量 `http://223.5.5.5/` 经 SOCKS 回 **404**，而
/// `http://www.baidu.com/` 经 SOCKS 回 **000**（curl 52 empty reply）。
/// 成对之后 `describe_dead_targets()` 的输出本身就能区分两类：
/// **IP 目标活着、只有域名目标死 = 只是解析坏**。
///
/// # 为什么**去掉** `www.baidu.com`（多轮实测）
///
/// 它在真实链路上**不稳定**：经 SOCKS 10 轮测到 **4/10 失败**（另一轮 1/10），
/// 而同期 `1.1.1.1` / `223.5.5.5` 各 **10/10**、`cp.cloudflare.com` **10/10**。
/// 门禁要求**每个目标都答**（task-42/54），所以留一个 40% 失败率的目标 =
/// 让 40% 的连接起不来 —— 那是「失败方向」错误的一侧，代价比少一个境内域名判据大。
/// 境内那一路由 `223.5.5.5`（IP 字面量，走 `geoip:cn → direct`）覆盖 ✓。
///
/// # 判据不严也不松
///
/// `commands::core::tunnel_is_dead` 只把空串 / `000` 判死 ⇒ **301 / 404 / 204 / 200
/// 都算活着**，所以 IP 字面量目标**不必**要求 204。
///
/// # 三条目标**并行**探测
///
/// 门禁里用 `JoinSet`（task-54 的做法）⇒ 最坏等待仍是**单次 6s**，不是 18s。
///
/// # 「只有解析坏、传输通」时判什么？（task-92 的结论：**判「链路不可用」**）
///
/// 1. 判据的终点是「**用户能不能上网**」，不是「包能不能送出去」。非中国域名在真实
///    浏览里占大头；解析全挂时用户看到的就是「网坏了」—— 门禁若在这种状态下接管
///    默认路由，就是把一个已经坏掉的体验升级成系统级接管；
/// 2. **回退链已经试过了**：`disableFallback: false` 且没有任何 `skipFallback`
///    （见 `config.rs` 的生成），所以一次「域名目标失败」= 境外 DoH **和**境内
///    解析器都失败（task-91 的配置分析），不是「只问了一个服务器」；
/// 3. 误判的代价由**连续失败策略**兜住：看门狗要连续 `FAILURES_BEFORE_REBUILD` 次
///    才重建，一次解析抖动不会触发；
/// 4. 反过来的代价更大：判「可用」= **明明解析不了却宣称一切正常**，正是本项目
///    最忌讳的「界面比事实强」。
///
/// **但诊断必须说清是哪一类**：IP 目标活着、只有域名目标死时，日志里只会点名
/// 域名目标 —— 用户/我们能看到「传输是通的，是解析坏了」，而不是笼统一句「隧道不通」。
///
/// **看门狗共用这一份**（`commands/core.rs::watchdog_probe_all`）：同一个盲区在
/// 门禁那边修过（task-42/54），在看门狗那边却漏了 —— 结果是「国内全断、
/// 国外正常」时看门狗永远认为一切正常（task-82）。**别再分叉出第二份清单。**
///
/// # 硬编码 IP 的风险与取舍
///
/// 一个写死的 IP 一旦失效，门禁会把**所有**用户拦在门外（失败方向错）。这里选的是
/// **DNS 基础设施的 anycast IP**（1.1.1.1 / 223.5.5.5 —— 本身就是长期稳定的服务
/// 地址），而不是某个网站的业务 IP（后者才是真会变的那类，也正是 `www.baidu.com`
/// 被多轮实测筛掉的原因）。**实测**：`http://1.1.1.1/` → 301、`http://223.5.5.5/`
/// → 404（直连与经 SOCKS 都稳定，各 10/10）。`http://8.8.8.8/` 实测 6s 超时 ⇒ **不用**。
/// 探针目标属于**哪一侧**（境内 / 境外）。
///
/// # 唯一真源（task-106）
///
/// 目标与侧写在**同一张表**里（[`REQUIRED_PROBE_TARGETS`]）：新增/修改目标时
/// 必须在同一行表态 —— 类型上没有默认值，**漏表态编译不过**；
/// 表里没有的目标 [`probe_side`] 返回 `None`，由测试拦住。
///
/// **刻意没有**「不在境内清单里就算境外」这种默认分支：那正是
/// 「新加了一个境内目标、却被静默算成境外」的成因（现场包里 `223.5.5.5` 的
/// 归属靠的是硬编码清单，不是机制）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeSide {
    Domestic,
    Overseas,
}

/// **必需探针目标的唯一真源**：`(URL, 它属于哪一侧)`。
///
/// 与 `scripts/fixtures/probe-targets.json` 是**同一份真源**（Python / 分诊脚本
/// 读夹具，**Rust 是权威**）：`probe_targets_fixture_matches_authoritative_table`
/// 钉住两者逐条一致 —— 改一边不改另一边，测试必红。
///
/// # 硬编码 IP 的风险与取舍
///
/// 一个写死的 IP 一旦失效，门禁会把**所有**用户拦在门外（失败方向错）。这里选的是
/// **DNS 基础设施的 anycast IP**（1.1.1.1 / 223.5.5.5 / 119.29.29.29 —— 本身就是长期稳定的服务
/// 地址），而不是某个网站的业务 IP（后者才是真会变的那类，也正是 `www.baidu.com`
/// 被多轮实测筛掉的原因）。**实测**：`http://1.1.1.1/` → 301、`http://223.5.5.5/`
/// → 404、`http://119.29.29.29/` → 404（直连与经 SOCKS 都稳定）。`http://8.8.8.8/`
/// 实测 6s 超时 ⇒ **不用**；`114.114.114.114` 6s 超时、`180.76.76.76` 空回复 ⇒ 也不**用**。
pub(crate) const REQUIRED_PROBE_TARGETS: &[(&str, ProbeSide)] = &[
    // 境外 · 传输（IP 字面量，不依赖解析）
    ("http://1.1.1.1/", ProbeSide::Overseas),
    // 境外 · 解析（域名，项目原有的探测目标）
    (xt_core::xray::DEFAULT_PROBE_URL, ProbeSide::Overseas),
    // 境内 · 传输（IP 字面量，走 geoip:cn → direct）
    ("http://223.5.5.5/", ProbeSide::Domestic),
    // 境内 · 传输（**task-106 新增**）：境内侧只有 1 个目标时，「境内全灭」最多
    // 只贡献 1 个失败 ⇒ 永远到不了「一轮 ≥2 个失败」的门槛 ⇒ **永不重建**
    // （用户现场包的真实实例：境内 全灭 1/1、境外 0/2 死 ⇒ 只记账不重建）。
    // 目标选型有只读实测数据支撑：`119.29.29.29 → 404`（有真实 HTTP 状态，
    // 满足 `responded()` 判据），`114.114.114.114` 超时、`180.76.76.76` 空回复。
    ("http://119.29.29.29/", ProbeSide::Domestic),
];

/// 只要 URL 的视图（启动门禁 / 看门狗循环用）。
pub(crate) fn required_probe_urls() -> Vec<&'static str> {
    REQUIRED_PROBE_TARGETS.iter().map(|(url, _)| *url).collect()
}

/// 目标属于哪一侧；**表里没有的返回 `None`**（不猜、不默认）。
pub(crate) fn probe_side(target: &str) -> Option<ProbeSide> {
    REQUIRED_PROBE_TARGETS
        .iter()
        .find(|(url, _)| *url == target)
        .map(|(_, side)| *side)
}

/// URL 的主机部分是不是 **IP 字面量**（⇒ 这次探测**不需要解析**）。
///
/// 用途：目标清单里「不依赖解析」的那一半靠它认出来（task-92），
/// 它同时也是「只有解析坏」这个诊断的基础。
pub(crate) fn url_host_is_ip_literal(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // 去端口（探测目标都是 v4，用不到 IPv6 的方括号形式）
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority);
    host.parse::<std::net::IpAddr>().is_ok()
}

/// 目标里**不依赖解析**的那些（IP 字面量）。
///
/// 「传输是否通」看它们；「解析是否通」看域名目标。两者都活才算链路可用
/// （理由见 [`REQUIRED_PROBE_TARGETS`] 的文档）。
#[cfg(test)]
pub(crate) fn probe_targets_without_dns<'a>(targets: &[&'a str]) -> Vec<&'a str> {
    targets
        .iter()
        .copied()
        .filter(|t| url_host_is_ip_literal(t))
        .collect()
}

/// 门槛探测的单次超时（秒）。
///
/// 5–8s 是刻意的：跨国首次握手可能 1s 以上，太短会误拦；而它挂在启动路径上，
/// 两个目标最坏约 12s，不能再长。
const PRE_COMMIT_PROBE_TIMEOUT_SECS: u32 = 6;

/// 一次门禁探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeOutcome {
    pub target: String,
    /// curl 报的 HTTP 码；空串 / `000` = 没拿到响应（超时、被 reset、代理不可用）。
    pub http_code: String,
}

impl ProbeOutcome {
    /// 「拿到了真实 HTTP 响应」。**任何**状态码都算通 —— 要证明的是
    /// 「数据出得去、对端回得来」，不是某个站点返回 200。
    pub(crate) fn responded(&self) -> bool {
        !self.http_code.is_empty() && self.http_code != "000"
    }

    fn describe(&self) -> String {
        if self.http_code.is_empty() {
            format!("{}（无响应/超时）", self.target)
        } else {
            format!("{}（HTTP {}）", self.target, self.http_code)
        }
    }
}

/// 门禁失败。**两种必须分开**：
/// * `Probe` —— 默认路由**还没**被接管（系统网络是干净的）；
/// * `Commit` —— 探测全过，但接管动作本身被 helper 拒绝（调用方回滚）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateFailure {
    /// 有目标没拿到真实响应 → **不得接管默认路由**。
    Probe { failed: Vec<ProbeOutcome> },
    /// 探测全过，但 `CommitRoutes` 自己失败。
    Commit(String),
    /// 配置错误：一个探测目标都没有 → fail-safe，拒绝接管。
    NoTargets,
}

impl GateFailure {
    /// 给用户看的说明（节点名等上下文由调用方补）。
    pub(crate) fn describe(&self) -> String {
        match self {
            GateFailure::Probe { failed } => {
                let list = failed.iter().map(ProbeOutcome::describe).collect::<Vec<_>>().join("、");
                // **说清是哪一类失败**（task-92）：探测目标成对配置 ——
                // IP 字面量（不依赖解析）管「传输」，域名目标管「解析」。
                // 失败清单里**只有域名目标** ⇒ 传输是通的，问题在解析链路上。
                // 不这么说的话，用户/我们只能看到笼统一句「隧道不通」，然后去换节点。
                let resolution_only = !failed.is_empty()
                    && failed.iter().all(|f| !url_host_is_ip_literal(&f.target));
                let diagnosis = if resolution_only {
                    // ⚠️ 这段文案在 2026-09-25 被**更正过**（原 task-92 的版本说
                    // "更像是解析链路的问题（本机网络/DNS 被干扰也可能导致）"）。
                    // 那句话在**当前这张目标表**上是错的：唯一的域名目标走的是
                    // `--socks5-hostname`，域名是**节点解析**的，本机 DNS 根本不在这条路径上。
                    // 顺着旧文案去修本机 DNS，等于修一个没参与的东西。
                    "**这次探测里失败的全是域名目标**，而域名是由**节点**去解析的\
                     （探测用 `--socks5-hostname`，刻意绕开本机 DNS）⇒ 隧道与传输是通的\
                     （IP 字面量那条路有响应），是这个域名在**节点侧**没解析出来或连不上\
                     （节点自身 DNS 不可用/被污染，或该域名在节点出口不可达）。\
                     **这不是本机 DNS 的问题**，改本机 DNS 不会有用。\
                     先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。\
                     （边界：目标表里只有**一个**域名目标，所以「节点侧解析坏了」与\
                     「只有这一个域名不可达」目前分不开 —— 这是诊断能力的边界，不是结论。）"
                } else {
                    "可能是这个节点不可用，也可能本机网络本身不通（或被链路干扰）；\
                     先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。"
                };
                format!(
                    "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应：{list}。\n\
                     国内网络下「TCP 能连到服务器、代理协议握手被墙」是常见情形。\n\
                     **已在接管默认路由之前中止**，系统网络未被改动。{diagnosis}"
                )
            }
            GateFailure::Commit(msg) => msg.clone(),
            GateFailure::NoTargets => {
                "端到端门禁没有配置探测目标，拒绝接管默认路由（配置错误，不是网络问题）".to_string()
            }
        }
    }
}

/// **接管默认路由之前的端到端门禁**：所有目标都拿到真实 HTTP 响应之后，
/// 才允许执行 `commit`（= `Request::CommitRoutes`）。
///
/// `commit` 作为参数注入，是为了让测试能**断言调用序列** —— 探测不过时
/// 它一次都不能被调用。「接管了默认路由但真实路径不通」正是当前零覆盖的
/// 致命组合，只断言返回值是抓不住的。
///
/// # 两个目标**并行**探测（task-54）
///
/// 每个目标各有一次超时，串行等待 ⇒ 最坏是两个超时**相加**（2 × 6s = 12s），
/// 而这段等待挂在每一次连接的启动路径上。改成同时发、一起等之后，最坏
/// ≤ 一个超时，正常网络下也更快返回。
///
/// **语义一个字没改**：仍然要求**每个**目标都拿到真实响应才允许 `commit`；
/// 只是把「依次等」换成「同时等」。任一目标失败 → 同样不得 commit。
pub(crate) async fn verify_paths_then_commit<Pr, Pf, Cm, Cf>(
    targets: &[&str],
    mut probe: Pr,
    commit: Cm,
) -> Result<(), GateFailure>
where
    Pr: FnMut(String) -> Pf,
    Pf: std::future::Future<Output = String> + Send + 'static,
    Cm: FnOnce() -> Cf,
    Cf: std::future::Future<Output = Result<(), xt_proto::HelperError>>,
{
    if targets.is_empty() {
        return Err(GateFailure::NoTargets);
    }

    // 先把所有探测**同时**发出去（闭包在这里同步调用，顺序仍是 targets 顺序，
    // 所以测试里的调用序列断言依然确定）。
    let mut set: tokio::task::JoinSet<(String, String)> = tokio::task::JoinSet::new();
    for target in targets {
        let target = (*target).to_string();
        let probe_fut = probe(target.clone());
        set.spawn(async move { (target, probe_fut.await) });
    }

    let mut codes: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((target, http_code)) => {
                codes.insert(target, http_code);
            }
            Err(e) => {
                // 探测任务 panic 不该让整次启动 panic：当作「没拿到响应」处理。
                return Err(GateFailure::Probe {
                    failed: vec![ProbeOutcome {
                        target: "<探测任务异常结束>".into(),
                        http_code: format!("join error: {e}"),
                    }],
                });
            }
        }
    }

    // 判定按 `targets` 原顺序做 —— 报错文案与测试断言都不受完成先后影响。
    let mut failed = Vec::new();
    for target in targets {
        let http_code = codes.remove(*target).unwrap_or_default();
        let outcome = ProbeOutcome { target: (*target).to_string(), http_code };
        if !outcome.responded() {
            failed.push(outcome);
        }
    }
    if !failed.is_empty() {
        // ---- 失败再试一次（**只重试失败的那些**）----
        //
        // 理由与 `tcp_reachable_with_retry` 完全一样：这是**单发**探测，而它的结果
        // 决定"整次启动中止 + 回滚"。现场报错里就出现过"四个目标里只有一个域名目标
        // 拿到 000"的中止 —— 那可能是节点侧真的解析不了，也可能只是一次抖动。
        // 重试一次能把两者分开，而且只多花 500ms + 失败目标那一次探测。
        //
        // 语义**没有放松**：重试后仍要求每个目标都拿到真实响应才允许 commit。
        tracing::info!(
            failed = failed.len(),
            "端到端门禁首次失败，重试失败的目标一次"
        );
        tokio::time::sleep(GATE_RETRY_PAUSE).await;
        let retry_targets: Vec<String> = failed.iter().map(|f| f.target.clone()).collect();
        let mut again: tokio::task::JoinSet<(String, String)> = tokio::task::JoinSet::new();
        for target in retry_targets {
            let probe_fut = probe(target.clone());
            again.spawn(async move { (target, probe_fut.await) });
        }
        let mut second: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        while let Some(joined) = again.join_next().await {
            if let Ok((target, http_code)) = joined {
                second.insert(target, http_code);
            }
        }
        // 按原顺序重建失败清单：第二次拿到响应的就不再算失败。
        let mut still_failed = Vec::new();
        for outcome in failed {
            let code = second.get(&outcome.target).cloned().unwrap_or_default();
            let retried = ProbeOutcome { target: outcome.target.clone(), http_code: code };
            if !retried.responded() {
                still_failed.push(retried);
            }
        }
        if !still_failed.is_empty() {
            tracing::warn!(
                failed = still_failed.len(),
                "端到端门禁重试后仍未通过，放弃接管默认路由"
            );
            return Err(GateFailure::Probe { failed: still_failed });
        }
        tracing::info!("端到端门禁：首次失败的目标在重试后通过（已继续）");
    }
    commit().await.map_err(|e| GateFailure::Commit(e.message))
}

/// 去哪儿找 xray 可执行文件。
///
/// 这两个目录**类型完全一样**，作为相邻的位置参数极容易被写反 ——
/// 而写反之后的表现是「开发期找不到核心」，与「路径本身错了」难以区分
/// （见 docs/03 里 `CARGO_MANIFEST_DIR` 那次事故）。用具名结构体钉住。
#[derive(Debug, Clone, Default)]
pub struct CoreSearchPaths {
    /// **更新下来的核心**（用户数据目录下的 `core/`）。
    ///
    /// 优先于包内那份。更新故意不写进 `.app`（会破坏签名），
    /// 于是「回退到出厂版本」= 删掉这个目录。
    pub managed_core_dir: Option<PathBuf>,
    /// 打包后的资源目录（生产环境）。
    pub app_resource_dir: Option<PathBuf>,
    /// 开发期的 `apps/desktop/binaries`（由应用侧提供，核心不能自己猜）。
    pub dev_binaries_dir: Option<PathBuf>,
}

#[derive(Default)]
pub struct Supervisor {
    process: Option<XrayProcess>,
    /// 从 helper 拿到的 utun fd。
    ///
    /// **必须一直持有**：它是 utun 接口的存活凭证，关掉接口就消失。
    /// 子进程（xray）有自己的一份副本，但两份都关掉才会真正销毁接口。
    tun_fd: Option<RawFd>,
    session_id: Option<String>,
    physical_interface: Option<String>,
    /// task-176：本次 TUN 生命周期里**按采样时点**累积的路由审计。
    ///
    /// 为什么不在这里直接落盘：落盘要用 `AppState`，而 `supervisor` 拿不到它。
    /// `start()` 返回后由调用方（`commands/core.rs`）`take_route_audits()` 统一落盘 ——
    /// 这样**成功与失败两条路径都会记**，日志里也带得上采样时点。
    route_audits: RouteAuditLog,
    /// 下一次 [`Supervisor::start`] 要用的意图过滤规则（放行带 + 拦截带）。
    ///
    /// # 为什么是 setter 而不是 `start()` 的参数
    ///
    /// `start()` 有 5 个调用点（1 个生产 + 4 个测试）。规则属于"这一次启动"，
    /// 而生成配置的地方就是 `start()` 自己 —— 用 setter 让既有测试一行都不用改。
    ///
    /// # 不变量（两条都不是可选的）
    ///
    /// * **不 `take`**：规则留在 supervisor 上，直到下一次 `set_intent_rules`。
    ///   看门狗重建会重新调 `start()`；若这里把规则取空，重建之后拦截就静默失效，
    ///   而用户看到的只是"广告又回来了"（没有任何报错）。
    /// * **调用方每次 `start()` 前都应重新 set**：设置与判决缓存都可能变，
    ///   这两个字段只是"上次给过来的快照"。
    intent_allow: Vec<xt_core::routing::RoutingRule>,
    intent_block: Vec<xt_core::routing::RoutingRule>,
}

impl Supervisor {
    /// 数据面进程**现在还活着吗**。
    ///
    /// **不能用 `process.is_some()`** —— 那只说明「我们手里有个 handle」。
    /// 核心自己退出（崩溃、被 OOM 杀掉、被别的工具清理）之后句柄还在，
    /// `is_some()` 照样返回 true。而 `start_core` 的幂等守卫正是看它 ——
    /// 于是**按钮点了永远没反应，而核心其实早就没了**（实测症状）。
    ///
    /// 这又是一次「状态的来源与真实不一致」（见 docs/08 的 A 类）：
    /// **观测到的**（我们记的 handle）不能替代**现实**（进程活着）。
    ///
    /// 这里顺便把死掉的句柄回收掉，让状态和现实一致 —— 否则后面
    /// `stop()` 还会对一个已经不存在的进程发信号。
    ///
    /// 调用方**必须和 `start` 用同一把锁**来判断：在锁外检查的话，
    /// 「检查完 → 真正 start」之间会被别人插进来，又变成「核心已经在运行」。
    pub fn is_running(&mut self) -> bool {
        // 模式守卫里不能可变借用，所以先取出结果再决定要不要回收。
        let alive = match self.process.as_mut() {
            Some(p) => !p.has_exited(),
            None => return false,
        };
        if !alive {
            tracing::warn!("数据面进程已不在，回收陈旧句柄（下次 start 才能真的起来）");
            self.process = None;
        }
        alive
    }

    /// 运行中的核心 pid（进程真的活着才算）。
    ///
    /// 给 `start_core` 的「已经在跑」分支用：那条路径上核心是活的，
    /// 但监控任务可能还没启动 —— 需要 pid 才能补上。
    pub fn running_pid(&self) -> Option<u32> {
        self.process.as_ref().and_then(|p| p.pid())
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn physical_interface(&self) -> Option<&str> {
        self.physical_interface.as_deref()
    }

    /// 设置下一次启动要用的意图规则（覆盖式）。
    ///
    /// 传空 `Vec` 表示"这次不要意图规则" —— 功能关闭或演练模式下就是这种形态。
    pub fn set_intent_rules(
        &mut self,
        allow: Vec<xt_core::routing::RoutingRule>,
        block: Vec<xt_core::routing::RoutingRule>,
    ) {
        self.intent_allow = allow;
        self.intent_block = block;
    }

    /// 当前挂着的意图规则条数 `(放行, 拦截)`（诊断与测试用）。
    pub fn intent_rule_counts(&self) -> (usize, usize) {
        (self.intent_allow.len(), self.intent_block.len())
    }

    /// **`start()` 实际使用的那一个**规则表（预设 + 意图 + 自定义，已排序）。
    ///
    /// 抽出来是为了让"意图规则真的进了配置"这件事**能被单测钉住**：
    /// 否则只能靠"启动一次核心、把 config.json 读出来看"这种重验收，
    /// 而那种验收在没有核心二进制的机器上会静默跳过。
    pub fn rules_for_config(&self, settings: &AppSettings) -> Vec<xt_core::routing::RoutingRule> {
        // 顺序由 `merge_rules_with_intent` 一处表达，避免"每个调用点各排一遍"。
        xray::merge_rules_with_intent(settings, &self.intent_allow, &self.intent_block)
    }

    /// 启动核心（含可选的 TUN）。
    pub async fn start(
        &mut self,
        store: &Store,
        settings: &AppSettings,
        nodes: &[Node],
        helper: &mut HelperClient,
        events: Option<tokio::sync::mpsc::UnboundedSender<CoreEvent>>,
        paths: CoreSearchPaths,
    ) -> Result<CoreRuntime, String> {
        if self.process.is_some() {
            return Err("核心已经在运行".into());
        }

        // ---- 0) 前置检查：不要在改动系统之后才发现没得救 ----
        let selected = settings
            .selected_node
            .as_deref()
            .and_then(|id| nodes.iter().find(|n| n.id == id));
        if settings.mode != ProxyMode::Direct && selected.is_none() {
            return Err("请先选择一个节点".into());
        }

        let core_path = xray::resolve_core_binary(
            settings.core_path.as_deref(),
            paths.managed_core_dir.as_deref(),
            paths.app_resource_dir.as_deref(),
            paths.dev_binaries_dir.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        let version = xray::core_version(&core_path).await.unwrap_or_default();
        let native_tun = version_meets(&version, MIN_CORE_VERSION_NATIVE_TUN);
        if settings.mode == ProxyMode::Tun && !native_tun {
            return Err(format!(
                "当前核心版本（{version}）不支持原生 TUN，需要 >= {MIN_CORE_VERSION_NATIVE_TUN}。\n\
                 请在「设置」里指定新的 xray 可执行文件。"
            ));
        }

        // ---- 1) 探测物理出口（必须在建 utun 之前） ----
        let physical = match settings.mode {
            ProxyMode::Tun => {
                let d = xt_tun::macos::route::default_route().map_err(|e| e.to_string())?;
                Some(d)
            }
            _ => None,
        };
        self.physical_interface = physical.as_ref().map(|d| d.interface.clone());

        // ---- 2) 生成并落盘配置 ----
        let profile: InboundProfile =
            profile_for(settings, self.physical_interface.as_deref(), native_tun);
        let rules = self.rules_for_config(settings);
        let config = xray::build_pretty(&CoreConfigInput {
            settings,
            nodes,
            selected: selected.map(|n| n.id.as_str()),
            rules: &rules,
            profile,
            // 物理网卡：TUN 模式下 direct 出站要靠它逃出隧道。
            physical_interface: self.physical_interface.as_deref(),
        });
        let config_path = store
            .write_core_config(&config)
            .map_err(|e| e.to_string())?;

        // 静态校验：把明显非法的配置挡在进程启动之前，错误信息也更可读。
        //
        // 失败文案由 `config_self_check_message` 组装：**先结论 + 下一步**，再附
        // **截断后**的核心输出。核心失败时 stderr 常为空、stdout 却是 35 行机器话
        // （可操作的那句在最后一行），原样透传等于没给用户任何线索（task-165）。
        xray::validate_config(&core_path, &config_path)
            .await
            .map_err(|e| xray::config_self_check_message(settings, &config, &e.to_string()))?;

        // ---- 3) TUN：建卡 → 取 fd → 拉起核心 → 提交路由 ----
        let mut deferred_commit = false;
        // 在 TUN 分支里赋值、在第 6 步使用，所以必须在外层声明。
        let mut server_probe_target: Option<std::net::SocketAddr> = None;
        if settings.mode == ProxyMode::Tun {
            let gateway = physical.as_ref().and_then(|d| d.gateway);
            // 解析代理服务器地址 —— 防路由环的第一步，必须在接管默认路由之前做。
            let server_addrs = resolve_server_addrs(selected)?;
            println_servers(&server_addrs);

            let request = self.build_tun_request(settings, gateway, &server_addrs)?;
            // 用**节点的真实端口**探测，而不是随便挑一个端口。
            //
            // 提交前后都用同一个目标，于是两次探测构成一个干净的差分实验：
            // 唯一变化的是「默认路由被接管了」。前通后不通 → 一定是路由问题。
            // 换成别的端口就把「端口没开」和「路由环」混在一起了。
            server_probe_target = server_addrs
                .first()
                .map(|ip| std::net::SocketAddr::new(*ip, selected.map(|n| n.port).unwrap_or(443)));
            let session_id = request.session_id.clone();
            // 会话泄漏（运行期间留下，或上一个 App 进程未干净退出）会在这里撞上
            // 「已有活跃会话」。走自愈：拆掉泄漏会话后**重试一次**，而不是把
            // 「请先 tun_down」丢给用户 —— 那与本项目「开机后自动连上、
            // 不需要点击」的目标直接冲突。
            let tun_started = std::time::Instant::now();
            tun_up_with_self_heal(helper, request)?;
            self.session_id = Some(session_id.clone());
            // task-176：**采样时点 1/3** —— utun 与旁路路由已装，默认接管还没发生。
            // 此时「作用域默认路由」本来就不该有（接管前），所以这一条只作基线，不报警。
            self.audit_routes(RouteAuditPhase::AfterTunUp);

            let (info, fd) = helper
                .take_tun_fd(&session_id)
                .map_err(|e| format!("helper 交付 utun fd 失败：{}", e.message))?;
            tracing::info!(interface = %info.interface, fd, "已取得 utun fd");
            tracing::info!(
                stage = "tun_up_and_fd",
                ms = tun_started.elapsed().as_millis() as u64,
                "启动阶段耗时"
            );
            self.tun_fd = Some(fd);
            deferred_commit = true;
        }

        // ---- 4) 拉起核心 ----
        // geo 的退路：核心自己旁边没有就去包内资源目录 / 托管目录找。
        // 顺序与核心解析一致（包内优先于托管），但这里更宽松：
        // 只要哪个目录真的有 geo 文件就用哪个。
        let geo_fallback: Vec<PathBuf> = [
            paths.app_resource_dir.clone(),
            paths.managed_core_dir.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let process =
            spawn_core(&core_path, &config_path, self.tun_fd, events, &geo_fallback).await?;

        // ---- 5) 等待就绪 ----
        let port_started = std::time::Instant::now();
        if let Err(e) = xray::wait_for_port(settings.socks_port, CORE_READY_TIMEOUT).await {
            // 核心没干净退出**不影响**网络回滚：回滚是随后单独调用 helper 做的
            // （见后面的 `rollback_tun` / helper 的 Restore），所以这里是 B 级 ——
            // 只留痕、不改控制流（task-122 A-3）。
            if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
            }
            self.rollback_tun(helper);
            return Err(format!("核心未在预期时间内就绪：{e}"));
        }
        tracing::info!(
            stage = "wait_for_port",
            ms = port_started.elapsed().as_millis() as u64,
            "启动阶段耗时"
        );

        // ---- 6) 接管默认路由 + 切换 DNS ----
        //
        // 这一步是「有副作用的临界点」。之前任何失败都只影响我们自己，
        // 之后系统流量就真的进隧道了。所以两侧都要做可达性校验：
        //
        //   提交前 → 证明服务器本身是通的（排除节点配置错误）
        //   提交后 → 证明 bypass 路由真的生效（排除路由环）
        //
        // 第二次检查尤其关键：没有它，防环失效时会变成「显示已连接、
        // 但什么都打不开」—— 用户完全无从下手。有了它，启动直接失败并
        // 回滚，错误信息指向真正的原因。
        if deferred_commit {
            if let Some(target) = server_probe_target {
                let (reachable, trace) = tcp_reachable_with_retry(target, REGION_PROBE_TIMEOUT).await;
                if !reachable {
                    // 核心没干净退出**不影响**网络回滚：回滚是随后单独调用 helper 做的
                    // （见后面的 `rollback_tun` / helper 的 Restore），所以这里是 B 级 ——
                    // 只留痕、不改控制流（task-122 A-3）。
                    if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                        tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
                    }
                    self.rollback_tun(helper);
                    return Err(format!(
                        "接管默认路由之前就联系不上代理服务器 {target}（{trace}，每次 {} 秒）。\n\
                         请检查节点地址 / 端口，以及本机到该服务器的直连是否正常。\n\
                         本次启动已中止并回滚，**默认路由没有被接管**。",
                        REGION_PROBE_TIMEOUT.as_secs()
                    ));
                }
                if trace != "第1次成功" {
                    // 抖过去了也要留痕：这解释了"为什么这次启动慢了几秒"，
                    // 也是"节点侧偶发丢包 / 本机刚换网"的早期信号。
                    tracing::info!(%target, %trace, "提交路由前探测：首次失败、重试后成功（已继续）");
                }
                tracing::info!(%target, %trace, "提交路由前：服务器可达");
            }

            // ---- 端到端门禁：TCP 通 ≠ 代理能用 ----
            //
            // 上面那次 `tcp_reachable` 只是**直连 TCP 三次握手**。国内
            // 「TCP 能连到 443、但 REALITY/TLS 握手被墙」是常态 —— 两道 TCP
            // 检查都会通过，然后我们就会把默认路由接管过去，用户看到「已连接」
            // 而整机断网（这正是线上事故的形态）。
            //
            // 所以接管之前必须问一句**真实用户路径**：经本机 SOCKS 发真实
            // HTTP 请求，境外 + 境内各一个目标；不过就**不接管**。
            // 此时默认路由还没动，回滚只需拆掉 bypass 路由与 utun，系统网络干净。
            let socks_port = settings.socks_port;
            let session_id = self.session_id.clone().unwrap_or_default();
            let gate_started = std::time::Instant::now();
            let gate_targets = required_probe_urls();
            // **按产品真实数据路径探测**：TUN 模式下域名由核心解析、节点只看到 IP，
            // 所以用本机解析（`--socks5`）；系统代理模式下客户端可能把域名交给节点，
            // 所以保持 `--socks5-hostname`。用错会 fail-closed 掉本来可用的节点。
            let probe_resolve = if settings.mode == ProxyMode::Tun {
                ProbeResolve::Locally
            } else {
                ProbeResolve::AtNode
            };
            tracing::info!(?probe_resolve, mode = ?settings.mode, "端到端门禁：域名解析位置");
            let gate = verify_paths_then_commit(
                &gate_targets,
                |target| {
                    socks_http_probe_with(
                        socks_port,
                        target,
                        PRE_COMMIT_PROBE_TIMEOUT_SECS,
                        probe_resolve,
                    )
                },
                || async {
                    let commit_started = std::time::Instant::now();
                    let result = helper
                        .call(&Request::CommitRoutes { session_id: session_id.clone() })
                        .map(|_| ());
                    tracing::info!(
                        stage = "commit_routes",
                        ms = commit_started.elapsed().as_millis() as u64,
                        "启动阶段耗时"
                    );
                    result
                },
            )
            .await;
            tracing::info!(
                stage = "pre_commit_gate",
                ms = gate_started.elapsed().as_millis() as u64,
                "启动阶段耗时（含两次探测，已并行）"
            );
            match gate {
                Ok(()) => {
                    // task-176：**采样时点 2/3（关键）** —— 默认路由已接管、DNS 已切。
                    // 从这里开始，绑了物理网卡的 `direct` 出站必须有一条作用域默认路由
                    // 才能出去；缺了 ⇒ `ENETUNREACH`（task-172 的形态）。
                    self.audit_routes(RouteAuditPhase::AfterCommitRoutes);
                }
                Err(GateFailure::Commit(msg)) => {
                    // 核心没干净退出**不影响**网络回滚：回滚是随后单独调用 helper 做的
                    // （见后面的 `rollback_tun` / helper 的 Restore），所以这里是 B 级 ——
                    // 只留痕、不改控制流（task-122 A-3）。
                    if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                        tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
                    }
                    self.rollback_tun(helper);
                    return Err(format!("接管默认路由失败（已回滚）：{msg}"));
                }
                Err(e) => {
                    // 核心没干净退出**不影响**网络回滚：回滚是随后单独调用 helper 做的
                    // （见后面的 `rollback_tun` / helper 的 Restore），所以这里是 B 级 ——
                    // 只留痕、不改控制流（task-122 A-3）。
                    if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                        tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
                    }
                    self.rollback_tun(helper);
                    tracing::warn!(reason = %e.describe(), "端到端门禁未通过，已放弃接管默认路由");
                    return Err(e.describe());
                }
            }

            // 关键检查：默认路由已经指向隧道，此时**从本机**再连一次服务器。
            // 如果 bypass 路由没生效，这个连接会被送进隧道而永远出不去。
            if let Some(target) = server_probe_target {
                let (reachable, trace) = tcp_reachable_with_retry(target, REGION_PROBE_TIMEOUT).await;
                if !reachable {
                    // 核心没干净退出**不影响**网络回滚：回滚是随后单独调用 helper 做的
                    // （见后面的 `rollback_tun` / helper 的 Restore），所以这里是 B 级 ——
                    // 只留痕、不改控制流（task-122 A-3）。
                    if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                        tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
                    }
                    self.rollback_tun(helper);
                    return Err(format!(
                        "接管默认路由之后无法再联系代理服务器 {target}（{trace}，每次 {} 秒）—— \
                         这是**路由环**：服务器自身的流量也被送进了隧道。\n\
                         原因通常是「服务器地址没有走物理出口的 host 路由」。\n\
                         已自动回滚，网络应已恢复。",
                        REGION_PROBE_TIMEOUT.as_secs()
                    ));
                }
                tracing::info!(%target, %trace, "提交路由后：服务器仍可达（bypass 路由生效）");
            }
        }

        self.process = Some(process);

        Ok(CoreRuntime {
            running: true,
            pid: self.process.as_ref().and_then(|p| p.pid()),
            started_at_unix: Some(crate::state::now_unix()),
            config_path: Some(config_path),
            tun_session: self.session_id.clone(),
            tun_interface: self.physical_interface.clone().map(|_| "utun".to_string()),
            routes_committed: true,
            last_error: None,
            // 由连通性检查在验证通过后写入（见 commands::spawn_connectivity_check）。
            // supervisor 这里不认识「哪个节点算好」—— 它只负责建隧道。
            last_good_node: None,
            // 刚建好隧道时没有任何自动恢复在进行；只有看门狗会置位它。
            recovery: Default::default(),
        })
    }

    /// 停止核心并回滚隧道。
    pub async fn stop(&mut self, helper: &mut HelperClient) -> Result<(), String> {
        let stopped_at = std::time::Instant::now();
        let mut errors: Vec<String> = Vec::new();

        // 先停数据面：它还持有 utun fd，不停掉接口不会消失。
        if let Some(process) = self.process.take() {
            if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                errors.push(format!("停止核心失败: {e}"));
            }
        }

        if let Err(e) = self.rollback_tun_inner(helper) {
            errors.push(e);
        }

        // 逐阶段计时（task-54 (d)）：`XRAYTUN_LOG=info` 时可见。
        tracing::info!(
            stage = "stop_core",
            ms = stopped_at.elapsed().as_millis() as u64,
            failures = errors.len(),
            "停止阶段耗时"
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }
}

/// 建立 TUN 所需、且自愈要用到的最小 helper 操作集。
///
/// **抽成 trait 的唯一目的是让自愈链路可测。** `HelperClient` 是挂在
/// Unix socket 上的具体类型，测试里没法让它「第一次返回会话冲突、
/// 第二次成功」—— 只能注入一个假实现。除此之外不要把它当接口用。
trait TunUpOps {
    fn tun_up(&mut self, request: TunUpRequest) -> Result<(), xt_proto::HelperError>;
    fn restore(&mut self) -> Result<(), xt_proto::HelperError>;
}

impl TunUpOps for HelperClient {
    fn tun_up(&mut self, request: TunUpRequest) -> Result<(), xt_proto::HelperError> {
        // 全限定调用，避免落到本 trait 自己的同名方法上造成递归。
        HelperClient::tun_up(self, request)
    }

    fn restore(&mut self) -> Result<(), xt_proto::HelperError> {
        self.call(&Request::Restore).map(|_| ())
    }
}

/// 会话泄漏自愈：`tun_up` 撞上「已有活跃会话」时，自动拆掉泄漏会话并**重试一次**。
///
/// # 为什么值得自愈
///
/// 会话可能是**运行期间**泄漏的（一次连接中途失败留下会话，或上一个 App
/// 进程未干净退出而本次已启动过）。启动期清理只跑一次，覆盖不到这种情况。
/// 此时用户看到的是原始报错「请先 tun_down」—— 要求他手工介入。
///
/// # 触发条件刻意收窄
///
/// **只认** [`xt_proto::HelperError::is_session_conflict`]。其余错误
/// （路径白名单、helper 未安装、权限不足）**原样上报**：这类「自动修复」
/// 最常见的事故就是把自愈做得太宽，把本该暴露给用户的真实错误吃掉。
///
/// # 为什么「先 Restore 再重试」是安全的
///
/// 被拆的那条会话必然是**没人负责的**：如果它还有主人，helper 的 `tun_up`
/// 本来就会把「另一个进程的合法会话」判成冲突并拒绝。
/// 且 `Restore` 正是启动期清理遗留物所用的同一条路径
/// （`tear_down_live_session()` + `controller::force_cleanup()`），
/// 本来就为「处理遗留物」而存在。
fn tun_up_with_self_heal(ops: &mut impl TunUpOps, request: TunUpRequest) -> Result<(), String> {
    let failure = match ops.tun_up(request.clone()) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };

    if !failure.is_session_conflict() {
        // 不是会话冲突：一行都不多管，原样上报。
        return Err(format!("helper 建立 TUN 失败：{}", failure.message));
    }

    // 用户排障时要知道「发生过自动清理」，否则他只会看到一次莫名其妙的成功。
    tracing::warn!(
        code = ?failure.code,
        error = %failure.message,
        "TUN 建立撞上已有活跃会话，自动清理泄漏会话后重试一次"
    );

    if let Err(e) = ops.restore() {
        // 连清理都失败 —— 必须如实说清是**哪一步**坏的，而不是把原始的
        // 会话冲突再抛一遍（那会让用户以为自愈压根没触发）。
        return Err(format!(
            "helper 建立 TUN 失败：{}；尝试自动清理泄漏会话时又失败了：{}",
            failure.message, e.message
        ));
    }

    match ops.tun_up(request) {
        Ok(()) => {
            tracing::info!("自动清理泄漏会话后，TUN 建立成功");
            Ok(())
        }
        Err(e) => Err(format!(
            "helper 建立 TUN 失败：{}（已自动清理泄漏会话并重试一次，仍然失败）",
            e.message
        )),
    }
}

impl Supervisor {
    fn rollback_tun(&mut self, helper: &mut HelperClient) {
        if let Err(e) = self.rollback_tun_inner(helper) {
            tracing::error!(error = %e, "TUN 回滚失败，helper 侧快照已保留，下次启动会重试");
        }
    }

    fn rollback_tun_inner(&mut self, helper: &mut HelperClient) -> Result<(), String> {
        if let Some(session_id) = self.session_id.take() {
            let response = helper.call(&Request::TunDown { session_id });
            if let Err(e) = response {
                // 交给 helper 的 Restore 兜底：快照还在磁盘上。
                return Err(format!("helper 回滚 TUN 失败：{}", e.message));
            }
        }
        if let Some(fd) = self.tun_fd.take() {
            // SAFETY: fd 由本 struct 独占；关闭后 utun 接口（若无其它持有者）会消失。
            unsafe { libc::close(fd) };
        }
        // task-176：**采样时点 3/3** —— 回滚之后（必须**先取名字再清** `physical_interface`）。
        self.audit_routes(RouteAuditPhase::AfterRollback);
        self.physical_interface = None;
        Ok(())
    }

    /// task-176：按采样时点记一次路由审计（**只读**一次 `netstat -rn -f inet`）。
    ///
    /// 读不到就如实记 `None` —— 现场包里要能看出「这次没采到」，不许静默跳过。
    fn audit_routes(&mut self, phase: RouteAuditPhase) {
        let audit = self
            .physical_interface
            .as_deref()
            .and_then(|iface| xt_tun::macos::route::current_route_audit(iface).ok());
        self.route_audits.push(phase, audit);
    }

    /// 取走本次 TUN 生命周期累积的路由审计（由 `commands/core.rs` 统一落盘）。
    pub fn take_route_audits(&mut self) -> Vec<(RouteAuditPhase, Option<RouteAudit>)> {
        self.route_audits.take()
    }

    pub fn build_tun_request(
        &self,
        settings: &AppSettings,
        physical_gateway: Option<std::net::IpAddr>,
        server_addrs: &[std::net::IpAddr],
    ) -> Result<TunUpRequest, String> {
        let tun = &settings.tun;

        // ---- 防环手段 1：代理服务器 IP 的 host 路由 ----
        //
        // 这是**必须**的，而且必须是第一条被安装的路由。
        //
        // 一度这里被留空过，理由是「Xray 的 `autoOutboundsInterface` 已经用
        // `IP_BOUND_IF` 把出站绑到物理网卡了」。那是错的，而且错得很隐蔽：
        //
        // `IP_BOUND_IF` 会把路由查找**限定在该接口上**。而我们的 `0.0.0.0/1`
        // 接管路由挂在 utun 上 —— 于是查找 203.0.113.10 时，最具体的匹配
        // 虽然在，却不在 en0 上，scoped 查找直接失败，返回 `ENETUNREACH`。
        //
        // 症状：隧道建起来了、路由也对、但核心连不上自己的服务器，
        // 日志里只有一句 `connect: network is unreachable`。
        //
        // 加上这个 host 路由之后，最具体的匹配就落在 en0 上了，
        // scoped 与 unscoped 两种查找都能成功。
        let mut bypass_hosts: Vec<std::net::IpAddr> = Vec::new();
        for ip in server_addrs {
            if !bypass_hosts.contains(ip) {
                bypass_hosts.push(*ip);
            }
        }
        // 物理网关本身也是「必须走物理出口的具体主机」。
        //
        // 放在 `bypass_hosts` 而不是 `bypass_networks`：语义上它是主机而不是
        // 网段，而且 `bypass_hosts` 会被**最先安装** —— 这正好是想要的顺序，
        // 网关可达是所有后续路由的前提。
        if let Some(gw) = physical_gateway {
            if !bypass_hosts.contains(&gw) {
                bypass_hosts.push(gw);
            }
        }
        if bypass_hosts.is_empty() {
            return Err(
                "无法确定代理服务器的 IP，不能安全地接管默认路由（会形成路由环）。\n\
                 请检查节点地址是否可解析。"
                    .to_string(),
            );
        }

        // ---- 防环手段 2：核心侧的出站接口绑定 ----
        // 保留它作为第二道保险：即使服务器换了 IP（域名的 A 记录变了），
        // `IP_BOUND_IF` 仍然能把出站钉在物理网卡上。
        // 两种手段的失效条件不重叠，所以两个都要有。
        let bypass = if tun.bypass_private {
            xt_proto::default_bypass_networks()
        } else {
            Vec::new()
        };

        Ok(TunUpRequest {
            // 会话 id 用「启动时刻的纳秒」而不是随机数：它天然单调，
            // 且出问题时能从日志时间反查到是哪一次启动。
            session_id: format!("s{}", now_nanos()),
            interface_name: None,
            mtu: tun.mtu,
            addresses: vec![tun
                .network_cidr()
                .ok_or_else(|| format!("非法 TUN 网段: {}", tun.network))?],
            routes: xt_proto::RoutePlan {
                default_route: DefaultRouteMode::SplitDefault,
                bypass_hosts,
                bypass_networks: bypass,
                ipv6: tun.ipv6,
            },
            dns: xt_proto::DnsPlan {
                mode: DnsMode::Automatic,
                // 哨兵地址：位于隧道网段内、不会被真实路由，唯一目的是
                // 让所有 53 端口流量必然进入隧道，从而被 Xray 的 dns-out 接管。
                servers: tun.sentinel_dns.parse().ok().into_iter().collect(),
                search_domains: Vec::new(),
            },
            // 数据面就是 Xray 自己（配置里已有 `protocol: "tun"` 入站），
            // helper 不需要再拉起任何东西。
            datapath: DatapathPlan::HandoffFd,
            defer_default_routes: true,
        })
    }
}

async fn spawn_core(
    core_path: &Path,
    config_path: &Path,
    tun_fd: Option<RawFd>,
    events: Option<tokio::sync::mpsc::UnboundedSender<CoreEvent>>,
    // geo_fallback_dirs：核心自己旁边没有 geo 文件时，退到这些目录里找。
    geo_fallback_dirs: &[PathBuf],
) -> Result<XrayProcess, String> {
    let mut envs: Vec<(&str, String)> = Vec::new();

    // geoip.dat / geosite.dat 的位置。
    //
    // 核心会在运行期读这两个文件来解析 `geoip:cn` / `geosite:cn` 规则，
    // 而它的工作目录是 runtime 目录（配置文件所在处），不是核心二进制所在处。
    // 不用这个环境变量的话，缺失时的行为是**规则静默不命中** ——
    // 日志里没有任何错误，用户只会看到「绕过大陆」预设完全没起作用。
    //
    // **注意 geo 和核心是两份独立的更新**：只更新了核心、托管目录里没有 geo
    // 文件是完全正常的状态。这时如果直接把 ASSET 指向托管目录，规则就会
    // 静默失效 —— 所以这里按「谁真的有 geo 文件」来选，而不是按「谁提供了核心」。
    let has_geo =
        |d: &std::path::Path| d.join("geosite.dat").is_file() || d.join("geoip.dat").is_file();
    let asset_dir = core_path
        .parent()
        .filter(|d| has_geo(d))
        .map(|d| d.to_path_buf())
        .or_else(|| geo_fallback_dirs.iter().find(|d| has_geo(d)).cloned());
    match asset_dir {
        Some(dir) => {
            tracing::debug!(dir = %dir.display(), "geo 数据目录");
            envs.push(("XRAY_LOCATION_ASSET", dir.display().to_string()));
        }
        None => tracing::warn!(
            "找不到 geoip.dat / geosite.dat，geoip:/geosite: 规则将不会命中（分流会静默失效）"
        ),
    }

    if let Some(fd) = tun_fd {
        // `xt_proto::transport` 收到 fd 时会设 CLOEXEC（避免泄漏给无关子进程），
        // 所以这里必须显式清掉，否则核心继承不到它。
        clear_cloexec(fd).map_err(|e| format!("清理 fd 的 FD_CLOEXEC 失败: {e}"))?;
        // Xray 同时接受 `xray.tun.fd` 与 `XRAY_TUN_FD`，两个都给上更保险。
        // 传给核心的编号就是 fd 自身的编号（`Command` 不会重排 fd）。
        envs.push(("XRAY_TUN_FD", fd.to_string()));
        envs.push(("xray.tun.fd", fd.to_string()));
    }

    XrayProcess::spawn_with_env(core_path, config_path, events, &envs)
        .await
        .map_err(|e| e.to_string())
}

fn clear_cloexec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fcntl 只操作标志位。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 探测一批节点的延迟，直到 `deadline` 为止。
pub async fn probe(
    nodes: &[Node],
    core_path: &Path,
    timeout: Duration,
    interface: Option<&str>,
) -> Result<Vec<ProbeResult>, String> {
    let opts = ProbeOptions {
        binary: core_path.to_path_buf(),
        timeout,
        // 必须传：隧道开着时，不绑物理网卡的「服务器 RTT」是假的 0ms。
        interface: interface.map(str::to_string),
        ..Default::default()
    };
    xray::probe_nodes(nodes, &opts, None)
        .await
        .map_err(|e| e.to_string())
}

/// 解析 `Xray 26.1.31 (go1.24.0 ...)` 这类版本串，判断是否 >= `min`。
fn version_meets(version_output: &str, min: &str) -> bool {
    let Some(found) = extract_version(version_output) else {
        return false;
    };
    compare_versions(&found, min) != std::cmp::Ordering::Less
}

fn extract_version(text: &str) -> Option<String> {
    // "Xray 26.1.31 (go1.24.0 darwin/arm64)" -> "26.1.31"
    for token in text.split_whitespace() {
        let cleaned = token.trim_start_matches('v');
        // 形如 "26.1.31"：至少一个点、以数字开头、其余只含数字与点。
        let looks_like_version = cleaned.split('.').count() >= 2
            && cleaned.chars().next().is_some_and(|c| c.is_ascii_digit())
            && cleaned.chars().all(|c| c.is_ascii_digit() || c == '.');
        if looks_like_version {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 把两次采样换算成速率。
///
/// 单独抽出来是因为它有三个容易写错的点：第一次采样（`previous` 全 0）会
/// 产生一个虚假的峰值、计数器回绕（接口重开后）会得到巨大负值、
/// 以及 `elapsed` 很短时会把噪声放大成几百 MB/s。
pub fn rate_from(
    previous: &crate::state::TrafficSample,
    rx: u64,
    tx: u64,
    elapsed: Duration,
) -> crate::state::TrafficSample {
    let secs = elapsed.as_secs_f64();
    // 采样间隔过短（<100ms）就不算速率，避免把调度抖动放大成带宽尖峰。
    let (rx_rate, tx_rate) = if secs < 0.1 {
        (0, 0)
    } else {
        (
            ((rx.saturating_sub(previous.rx_bytes)) as f64 / secs) as u64,
            ((tx.saturating_sub(previous.tx_bytes)) as f64 / secs) as u64,
        )
    };
    crate::state::TrafficSample {
        rx_bytes: rx,
        tx_bytes: tx,
        rx_rate,
        tx_rate,
    }
}

/// 等待核心可执行文件出现（用户在设置里改路径后的校验用）。
#[allow(dead_code)]
pub async fn wait_for_core_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    path.is_file()
}

/// 供 UI 显示的「是否支持原生 TUN」。
pub fn core_supports_native_tun(version: &str) -> bool {
    version_meets(version, MIN_CORE_VERSION_NATIVE_TUN)
}

/// geo 数据文件（`geosite.dat` / `geoip.dat`）所在目录。
///
/// **与「核心在哪」是两件事**：geo 和核心由两次独立的更新分发，只更新了核心、
/// 托管目录里没有 geo 文件是完全正常的状态。所以这里按「谁真的有 geo 文件」
/// 判断，而不是按「谁提供了核心」—— 后者会让 `geosite:` 规则静默失效。
///
/// 抽成函数是为了让「路由判定」也能用同一份判断：界面要说「能不能判定域名规则」，
/// 判断依据必须与核心实际使用的那个目录一致，否则会出现「核心能用、界面说不能用」。
pub fn geo_dir(data_root: &Path) -> Option<PathBuf> {
    let managed = xt_core::update::managed_core_dir(data_root);
    let candidates: Vec<PathBuf> = vec![
        managed,
        // 包内资源目录（发行版）
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default(),
        // 开发期：apps/desktop/binaries
        crate::dev_binaries_dir().unwrap_or_default(),
    ];
    candidates.into_iter().find(|d| {
        !d.as_os_str().is_empty()
            && (d.join("geosite.dat").is_file() || d.join("geoip.dat").is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::{AppSettings, ProxyMode};

    /// 「拿到真实响应码」的判据只有一份实现，在 `commands::core` —— 这里**引用**
    /// 而不是抄一份（task-92：IP 目标回 301/404 也算活着，就是靠它）。
    use crate::commands::tunnel_is_dead;

    /// 临时数据目录 + 一个「从不连接」的 helper 客户端。
    ///
    /// `HelperClient::new(None)` 只是构造对象，不会去连 socket —— 所以下面这些
    /// **入口守卫**测试不需要特权、不需要 helper、也不需要真的起核心。
    fn scaffold(tag: &str) -> (Store, HelperClient) {
        let dir = std::env::temp_dir().join(format!("xt-sup-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::new(dir), HelperClient::new(None))
    }

    /// 一个结构合法的节点，用于让「已选节点」这层校验通过。
    /// 用 JSON 构造：字段与线上数据同形状，避免测试里手抄一堆协议细节。
    fn fixture_node() -> Node {
        serde_json::from_str(
            r#"{
                "id": "n1",
                "name": "fixture",
                "address": "127.0.0.1",
                "port": 443,
                "protocol": { "kind": "vless", "uuid": "11111111-2222-3333-4444-555555555555",
                              "flow": "", "encryption": "none" },
                "transport": { "kind": "tcp" },
                "source": { "kind": "manual" }
            }"#,
        )
        .expect("测试夹具节点应当能解析")
    }

    /// 代理模式下没选节点 -> 必须**在改动任何系统状态之前**就拒绝。
    ///
    /// 守卫的**位置**和文案一样重要：它排在「探出口、写配置、建 utun」之前。
    /// 顺序一旦被改动，用户会先经历一次网络被拆掉、然后才看到「请先选择节点」。
    /// **重试语义必须是可判别的**：首次失败、第二次成功 ⇒ 继续（不是中止）。
    ///
    /// 这条测试存在的理由：用户遇到过一次"节点直连明明通、却被告知联系不上"的中止。
    /// 单发探测把瞬时抖动当成了不可达；重试之后"抖一下"不该再挡人。
    #[tokio::test]
    async fn a_single_transient_probe_failure_is_retried_and_does_not_abort() {
        let mut calls = 0;
        let (ok, trace) = tcp_reachable_with_retry_by(
            || {
                calls += 1;
                let n = calls;
                async move { n > 1 }
            },
            Duration::from_millis(1),
        )
        .await;
        assert!(ok, "首次失败、第二次成功 ⇒ 应当判定为可达");
        assert_eq!(calls, 2, "只重试一次（不无限重试）");
        assert!(trace.contains("第1次失败") && trace.contains("第2次成功"), "{trace}");
    }

    /// 负对照：两次都失败 ⇒ 判定不可达，而且轨迹里两次都记着
    /// （用户看到的文案要能区分"抖了一下"与"真的连不上"）。
    #[tokio::test]
    async fn two_real_failures_still_abort_with_a_visible_trace() {
        let (ok, trace) =
            tcp_reachable_with_retry_by(|| async { false }, Duration::from_millis(1)).await;
        assert!(!ok);
        assert_eq!(trace, "第1次失败、第2次失败");
    }

    #[tokio::test]
    async fn start_refuses_without_a_selected_node_before_touching_the_system() {
        let (store, mut helper) = scaffold("no-node");
        let mut sup = Supervisor::default();
        let settings = AppSettings::default(); // 默认代理模式，且未选节点

        let err = sup
            .start(
                &store,
                &settings,
                &[],
                &mut helper,
                None,
                CoreSearchPaths::default(),
            )
            .await
            .expect_err("没有节点时必须拒绝启动");
        assert!(err.contains("请先选择一个节点"), "错误文案变了: {err}");
        assert!(
            !store.root().join("core").exists(),
            "被拒绝的启动不该留下任何落盘产物"
        );
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// TUN 模式下核心版本过老 -> 必须给**可操作**的提示，而不是底层报错。
    ///
    /// 用户能做的事是「去设置里换一个核心」，所以文案必须带上版本要求。
    /// 这一条容易被后续重构改成 `map_err(|e| e.to_string())` 而丢掉人话。
    ///
    /// 注意守卫顺序（本次测试就是照着它写的）：**先解析核心路径、再比版本**。
    /// 路径解析不了时给的是「找不到核心」，不是版本提示 —— 两者都是人话，
    /// 但指向的动作不同：一个去装核心，一个去换核心。
    #[tokio::test]
    async fn start_in_tun_mode_requires_a_core_that_supports_native_tun() {
        use std::os::unix::fs::PermissionsExt;

        let (store, mut helper) = scaffold("tun-version");
        let mut sup = Supervisor::default();

        // 造一个「能跑但版本过老」的假核心：只回一行 version 输出。
        let fake = store.root().join("fake-old-xray");
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(&fake, "#!/bin/sh\necho 'Xray 1.0.0 (fake)'\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let settings = AppSettings {
            mode: ProxyMode::Tun,
            core_path: Some(fake.clone()),
            selected_node: Some("n1".into()),
            ..Default::default()
        };

        let err = sup
            .start(
                &store,
                &settings,
                &[fixture_node()],
                &mut helper,
                None,
                CoreSearchPaths::default(),
            )
            .await
            .expect_err("核心版本过老时 TUN 模式必须拒绝");
        assert!(
            err.contains("不支持原生 TUN") && err.contains(MIN_CORE_VERSION_NATIVE_TUN),
            "错误应当说清版本要求，实际: {err}"
        );
        // 版本过老的核心也不该被启动
        assert!(!sup.is_running(), "被拒绝的启动不该留下运行中的核心");
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 意图规则必须**真的**进到 supervisor 生成的那份规则表里，而且位置正确。
    ///
    /// 这条测试钉的是"接线"这一层：`set_intent_rules` → `rules_for_config`。
    /// 配置级别（真实核心 exit 0、重复 ruleTag 被拒）由
    /// `crates/xt-intent/tests/real_core.rs` 覆盖 —— 那条更强，但它需要核心二进制。
    #[test]
    fn intent_rules_reach_the_rule_table_the_supervisor_builds() {
        use xt_core::model::RoutingPreset;
        use xt_core::routing::{MatchCondition, RuleAction, RoutingRule};

        fn domain_rule(id: &str, host: &str, then: RuleAction) -> RoutingRule {
            RoutingRule::new(
                id,
                format!("测试 {id}"),
                MatchCondition { domains: vec![format!("full:{host}")], ..Default::default() },
                then,
            )
        }

        let mut sup = Supervisor::default();
        assert_eq!(sup.intent_rule_counts(), (0, 0), "默认没有任何意图规则");
        let settings = AppSettings { routing_preset: RoutingPreset::BypassMainland, ..Default::default() };
        assert!(
            !sup.rules_for_config(&settings).iter().any(|r| r.id.starts_with("intent-")),
            "没设过就不该凭空出现意图规则"
        );

        sup.set_intent_rules(
            vec![domain_rule("intent-allow-ok.example", "ok.example", RuleAction::Direct)],
            vec![domain_rule("intent-block-ads.example", "ads.example", RuleAction::Block)],
        );
        assert_eq!(sup.intent_rule_counts(), (1, 1));

        let rules = sup.rules_for_config(&settings);
        let pos = |id: &str| {
            rules
                .iter()
                .position(|r| r.id == id)
                .unwrap_or_else(|| panic!("规则表里没有 {id}"))
        };
        assert!(pos("preset-private") < pos("intent-allow-ok.example"), "私有直连必须最先");
        assert!(pos("intent-allow-ok.example") < pos("intent-block-ads.example"), "放行早于拦截");
        assert!(pos("intent-block-ads.example") < pos("preset-ads"), "意图拦截早于静态广告名单");
        assert!(pos("intent-block-ads.example") < pos("preset-cn-domain"), "意图拦截早于大陆直连");

        // 覆盖式：再设一次空 ⇒ 回到"没有意图规则"（这就是关闭功能/演练模式的形态）。
        sup.set_intent_rules(vec![], vec![]);
        assert_eq!(sup.intent_rule_counts(), (0, 0));
        assert!(!sup.rules_for_config(&settings).iter().any(|r| r.id.starts_with("intent-")));
    }

    /// 同一个 `Supervisor` 上重复 `start` -> 幂等守卫必须拦下。
    ///
    /// 守卫看的是 `is_running()`（进程**真的还活着吗**），不是
    /// `process.is_some()`。两者混用曾经导致「核心早就崩了，按钮却点了没反应」。
    #[tokio::test]
    async fn start_twice_is_refused_by_the_idempotency_guard() {
        let (store, mut helper) = scaffold("twice");
        // 用 test 自己起的一个长驻进程占住槽位（不依赖 xray 二进制）
        let running = XrayProcess::spawn(
            std::path::Path::new("/bin/sh"),
            std::path::Path::new("/dev/null"),
            None,
        )
        .await
        .expect("起 /bin/sh 不该失败");
        let mut sup = Supervisor {
            process: Some(running),
            ..Default::default()
        };
        assert!(sup.is_running(), "刚起的进程应当是活的");

        let err = sup
            .start(
                &store,
                &AppSettings::default(),
                &[],
                &mut helper,
                None,
                CoreSearchPaths::default(),
            )
            .await
            .expect_err("核心在跑时重复 start 必须被拒绝");
        assert!(err.contains("已经在运行"), "错误文案变了: {err}");

        if let Some(p) = sup.process.take() {
            let _ = p.shutdown(Duration::from_secs(2)).await;
        }
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 守卫顺序：**核心解析不出来**时给的是「找不到核心」，而不是版本提示。
    ///
    /// 两条都是人话，但指向的动作不同：一个去装核心，一个去换核心。
    /// 顺序被改（先比版本再解析）会让提示指错方向 —— 用户按「版本太老」
    /// 去换核心，而实际上根本没有核心。
    #[tokio::test]
    async fn start_reports_missing_core_rather_than_a_version_problem() {
        let (store, mut helper) = scaffold("no-core");
        let mut sup = Supervisor::default();
        let settings = AppSettings {
            mode: ProxyMode::Tun,
            core_path: Some(PathBuf::from("")), // 解析不出核心
            selected_node: Some("n1".into()),
            ..Default::default()
        };

        let err = sup
            .start(
                &store,
                &settings,
                &[fixture_node()],
                &mut helper,
                None,
                CoreSearchPaths::default(),
            )
            .await
            .expect_err("找不到核心时必须拒绝");
        assert!(
            err.contains("找不到 Xray 核心"),
            "应当报「找不到核心」而不是版本问题，实际: {err}"
        );
        assert!(
            !err.contains("不支持原生 TUN"),
            "核心都没有时不该谈版本: {err}"
        );
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 回归测试：TUN 请求**必须**把代理服务器 IP 放进 `bypass_hosts`。
    ///
    /// 这个字段一度被留空，理由是「核心的 `autoOutboundsInterface` 已经用
    /// `IP_BOUND_IF` 绑定了物理网卡」。结果 `IP_BOUND_IF` 会把路由查找限定在
    /// en0 上，而 `0.0.0.0/1` 挂在 utun 上 —— 于是核心连自己的服务器都
    /// 连不上，报 `network is unreachable`，隧道「建起来了但什么都打不开」。
    #[test]
    fn tun_request_always_bypasses_the_server_ip() {
        let sup = Supervisor::default();
        let settings = AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        };
        let server: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        let gw: std::net::IpAddr = "192.168.0.1".parse().unwrap();

        let req = sup
            .build_tun_request(&settings, Some(gw), &[server])
            .unwrap();
        assert!(
            req.routes.bypass_hosts.contains(&server),
            "服务器 IP 必须在 bypass_hosts 里，否则会形成路由环"
        );
        assert!(req.routes.bypass_hosts.contains(&gw), "物理网关也要能直连");
    }

    /// 拿不到服务器 IP 时必须**拒绝启动**，而不是硬着头皮接管默认路由。
    #[test]
    fn tun_request_refuses_without_server_ips() {
        let sup = Supervisor::default();
        let settings = AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        };
        let err = sup.build_tun_request(&settings, None, &[]);
        assert!(err.is_err(), "没有服务器 IP 就不能接管默认路由");
        let msg = err.unwrap_err();
        assert!(msg.contains("路由环"), "错误信息要说明后果：{msg}");
    }

    /// 跨 crate 断言：服务器 host 路由必须排在 `/1` 接管路由**之前**。
    ///
    /// 顺序反了的话，中间会有一个窗口「默认流量已进隧道、但服务器的包
    /// 还被送进隧道」，也就是路由环。plan.rs 里也测了同一件事，
    /// 但那用的是合成数据 —— 这里用的是真实的节点地址。
    #[test]
    fn server_host_route_precedes_default_capture_in_real_request() {
        let sup = Supervisor::default();
        let settings = AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        };
        let server: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        let gw: std::net::IpAddr = "192.168.0.1".parse().unwrap();
        let req = sup
            .build_tun_request(&settings, Some(gw), &[server])
            .unwrap();

        let physical = xt_tun::plan::PhysicalUplink {
            interface: "en0".into(),
            gateway: Some(gw),
            service: Some("Wi-Fi".into()),
        };
        let plan = xt_tun::plan::build_plan(&req, physical).unwrap();

        let dests: Vec<String> = plan
            .routes
            .iter()
            .map(|r| r.destination.to_string())
            .collect();
        let host_idx = dests.iter().position(|d| d == "203.0.113.10/32");
        let split_idx = dests.iter().position(|d| d == "0.0.0.0/1");

        assert!(
            host_idx.is_some(),
            "plan 里必须有服务器 host 路由：{dests:?}"
        );
        assert!(split_idx.is_some(), "plan 里必须有 /1 接管路由");
        assert!(
            host_idx.unwrap() < split_idx.unwrap(),
            "服务器 host 路由必须先于 /1 接管安装：{dests:?}"
        );
    }

    #[test]
    fn version_comparison_handles_real_output() {
        let sample = "Xray 26.1.31 (go1.24.0 darwin/arm64)";
        assert!(version_meets(sample, "26.1.31"));
        assert!(version_meets(sample, "26.1.18"));
        assert!(!version_meets(sample, "26.2.0"));
        assert!(!version_meets("Xray 25.12.8 (go1.23)", "26.1.31"));
    }

    #[test]
    fn version_extraction_is_strict() {
        assert_eq!(
            extract_version("Xray 26.1.31 (go1.24.0)"),
            Some("26.1.31".into())
        );
        assert_eq!(extract_version("v26.9.9"), Some("26.9.9".into()));
        assert_eq!(extract_version("no version here"), None);
        // go1.24.0 含有 'o'，不应被误认为版本号
        assert_eq!(extract_version("go1.24.0"), None);
    }

    #[test]
    fn version_comparison_pads_short_versions() {
        assert_eq!(
            compare_versions("26.1", "26.1.0"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_versions("26.1.1", "26.1"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(compare_versions("26", "26.0.1"), std::cmp::Ordering::Less);
    }

    #[test]
    fn unknown_version_is_treated_as_unsupported() {
        assert!(!version_meets("", "26.1.31"));
        assert!(!version_meets("garbage", "26.1.31"));
    }

    #[test]
    fn rate_from_ignores_short_intervals_and_counter_resets() {
        use crate::state::TrafficSample;
        let prev = TrafficSample {
            rx_bytes: 1000,
            tx_bytes: 2000,
            rx_rate: 0,
            tx_rate: 0,
        };

        // 采样间隔过短 → 不报速率
        let r = rate_from(&prev, 2000, 4000, Duration::from_millis(10));
        assert_eq!(r.rx_rate, 0);
        assert_eq!(r.rx_bytes, 2000);

        // 正常间隔 → 速率正确
        let r = rate_from(&prev, 3000, 6000, Duration::from_secs(2));
        assert_eq!(r.rx_rate, 1000);
        assert_eq!(r.tx_rate, 2000);

        // 计数器回绕（接口重开）→ saturating_sub 兜住，不会得到天文数字
        let r = rate_from(&prev, 10, 20, Duration::from_secs(1));
        assert_eq!(r.rx_rate, 0);
    }

    #[test]
    fn core_supports_native_tun_matches_min_version() {
        assert!(core_supports_native_tun("Xray 26.9.9 (go1.24.0)"));
        assert!(!core_supports_native_tun("Xray 26.1.18 (go1.24.0)"));
    }

    // -----------------------------------------------------------------------
    // 会话泄漏自愈（task-31 第 2 步）
    // -----------------------------------------------------------------------

    /// 假 helper：按脚本返回 `tun_up` 结果，并**记录真实调用序列**。
    ///
    /// 记录序列是刻意的：这组测试要证明的不只是「最终成功」，而是
    /// 「**确实**先 Restore 再重试一次」，以及「非冲突错误**没有**触发 Restore」。
    /// 只看最终返回值的话，一个「无条件 Restore 一次」的错误实现也能骗过测试。
    struct FakeOps {
        tun_up_queue: std::collections::VecDeque<Result<(), xt_proto::HelperError>>,
        restore_fails: bool,
        calls: Vec<&'static str>,
    }

    impl FakeOps {
        fn new(tun_up: Vec<Result<(), xt_proto::HelperError>>, restore_fails: bool) -> Self {
            Self {
                tun_up_queue: tun_up.into(),
                restore_fails,
                calls: Vec::new(),
            }
        }
    }

    impl TunUpOps for FakeOps {
        fn tun_up(&mut self, _request: TunUpRequest) -> Result<(), xt_proto::HelperError> {
            self.calls.push("tun_up");
            self.tun_up_queue.pop_front().unwrap_or_else(|| {
                Err(xt_proto::HelperError::new(
                    xt_proto::ErrorCode::Internal,
                    "测试脚本给的 tun_up 次数少于实际调用次数",
                ))
            })
        }

        fn restore(&mut self) -> Result<(), xt_proto::HelperError> {
            self.calls.push("restore");
            if self.restore_fails {
                Err(xt_proto::HelperError::new(
                    xt_proto::ErrorCode::Internal,
                    "Restore 也失败了",
                ))
            } else {
                Ok(())
            }
        }
    }

    /// 一个内容合法、可直接喂给自愈函数的 TUN 请求（复用真实构造路径）。
    fn a_tun_request() -> TunUpRequest {
        Supervisor::default()
            .build_tun_request(
                &AppSettings {
                    mode: ProxyMode::Tun,
                    ..Default::default()
                },
                Some("192.168.0.1".parse().unwrap()),
                &["203.0.113.10".parse().unwrap()],
            )
            .expect("夹具请求应当能构造出来")
    }

    fn conflict(code: xt_proto::ErrorCode, msg: &str) -> Result<(), xt_proto::HelperError> {
        Err(xt_proto::HelperError::new(code, msg))
    }

    /// 主链路：会话冲突 → 自动 Restore → 重试一次 → 成功。
    #[test]
    fn session_conflict_is_healed_by_restore_and_one_retry() {
        let mut ops = FakeOps::new(
            vec![
                conflict(
                    xt_proto::ErrorCode::SessionConflict,
                    "已有活跃会话 s178…（接口 utun6），请先 tun_down",
                ),
                Ok(()),
            ],
            false,
        );
        let out = tun_up_with_self_heal(&mut ops, a_tun_request());
        assert!(out.is_ok(), "清理泄漏会话后重试应当成功：{out:?}");
        assert_eq!(
            ops.calls,
            ["tun_up", "restore", "tun_up"],
            "顺序与次数是契约"
        );
    }

    /// 用户机器上装的是**旧 helper**（单独安装的特权二进制，长期共存）：
    /// 它只发 `InvalidRequest` + 那句中文，同样必须触发自愈。
    #[test]
    fn legacy_helper_conflict_also_triggers_self_heal() {
        let mut ops = FakeOps::new(
            vec![
                conflict(
                    xt_proto::ErrorCode::InvalidRequest,
                    "已有活跃会话 s178…（接口 utun6），请先 tun_down",
                ),
                Ok(()),
            ],
            false,
        );
        let out = tun_up_with_self_heal(&mut ops, a_tun_request());
        assert!(out.is_ok(), "旧 helper 的冲突也要能自愈：{out:?}");
        assert_eq!(ops.calls, ["tun_up", "restore", "tun_up"]);
    }

    /// **反例（本卡重点）**：非会话冲突的错误必须**原样上报**，且**不得**触发 Restore。
    ///
    /// 路径白名单失败也是 `InvalidRequest` —— 判据若写成「所有 InvalidRequest
    /// 都算冲突」，这里就会去拆一条**不相干**的会话，并把真实错误吞掉。
    #[test]
    fn non_conflict_error_is_reported_verbatim_and_never_restores() {
        let mut ops = FakeOps::new(
            vec![conflict(
                xt_proto::ErrorCode::InvalidRequest,
                "路径不在白名单内",
            )],
            false,
        );
        let err = tun_up_with_self_heal(&mut ops, a_tun_request())
            .expect_err("白名单错误必须上报，不能被自愈吞掉");
        assert!(err.contains("路径不在白名单内"), "原文必须保留：{err}");
        assert_eq!(ops.calls, ["tun_up"], "非冲突错误不得触发 Restore");
    }

    /// 清理之后仍然失败：要如实写「已自动清理并重试过」，
    /// 而不是把原始的会话冲突再抛一遍（那会让用户以为自愈压根没触发）。
    #[test]
    fn conflict_surviving_the_retry_is_reported_as_such() {
        let mut ops = FakeOps::new(
            vec![
                conflict(
                    xt_proto::ErrorCode::SessionConflict,
                    "已有活跃会话 s1（接口 utun6），请先 tun_down",
                ),
                conflict(
                    xt_proto::ErrorCode::SessionConflict,
                    "已有活跃会话 s2（接口 utun6），请先 tun_down",
                ),
            ],
            false,
        );
        let err =
            tun_up_with_self_heal(&mut ops, a_tun_request()).expect_err("重试后仍失败就该失败");
        assert!(
            err.contains("已自动清理泄漏会话并重试一次"),
            "要说清做过什么：{err}"
        );
        assert_eq!(
            ops.calls,
            ["tun_up", "restore", "tun_up"],
            "只重试一次，不无限循环"
        );
    }

    /// 连清理都失败：必须指出坏在哪一步，否则用户无从下手。
    #[test]
    fn restore_failure_is_reported_as_such() {
        let mut ops = FakeOps::new(
            vec![conflict(
                xt_proto::ErrorCode::SessionConflict,
                "已有活跃会话 s1",
            )],
            true,
        );
        let err = tun_up_with_self_heal(&mut ops, a_tun_request()).expect_err("清理失败就该失败");
        assert!(
            err.contains("尝试自动清理泄漏会话时又失败了"),
            "要指出是哪一步：{err}"
        );
        assert_eq!(ops.calls, ["tun_up", "restore"]);
    }

    /// 一次就成功时不得多做任何动作 —— 否则自愈会给正常启动平白加一次 Restore。
    #[test]
    fn success_on_first_try_does_no_extra_work() {
        let mut ops = FakeOps::new(vec![Ok(())], false);
        assert!(tun_up_with_self_heal(&mut ops, a_tun_request()).is_ok());
        assert_eq!(ops.calls, ["tun_up"]);
    }

    // -----------------------------------------------------------------------
    // 接管默认路由之前的端到端门禁（task-42）
    //
    // 这组测试的重点是**调用序列**：探测不过时 `commit` 一次都不能被调用。
    // 「接管了默认路由但真实路径不通」是线上事故的形态，只断言返回值抓不住它。
    // -----------------------------------------------------------------------

    /// 记录探测/提交的调用序列。
    #[derive(Default)]
    struct GateLog(std::sync::Mutex<Vec<String>>);

    impl GateLog {
        fn push(&self, step: &str) {
            self.0.lock().unwrap().push(step.to_string());
        }
        fn seq(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    /// 境外通、境内通 → **唯一允许**调用 commit 的组合。
    #[tokio::test]
    async fn gate_commits_only_after_every_target_responds() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &["overseas", "domestic"],
            |t: String| {
                log.push(&format!("probe:{t}"));
                async { "204".to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        assert!(res.is_ok(), "两个目标都通时必须通过：{res:?}");
        assert_eq!(log.seq(), ["probe:overseas", "probe:domestic", "commit"]);
    }

    /// **(d) 实测：两个探测真的并行**（不是只看代码形状）。
    ///
    /// 每个目标各睡 150ms：串行 ≥300ms（这正是门禁给每次连接加的最坏 12s 的来源），
    /// 并行 ≈150ms。阈值 260ms 留足余量，但足以在「退化成串行」时变红。
    #[tokio::test]
    async fn gate_probes_run_in_parallel_not_sequentially() {
        let started = std::time::Instant::now();
        let res = verify_paths_then_commit(
            &["slow-a", "slow-b"],
            |_t| async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                "204".to_string()
            },
            || async { Ok::<(), xt_proto::HelperError>(()) },
        )
        .await;
        let elapsed = started.elapsed();

        assert!(res.is_ok(), "两个目标都通时必须通过：{res:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(260),
            "两次 150ms 探测并行时应当 ≈150ms；实际 {}ms（≥300ms 说明又变回串行了）",
            elapsed.as_millis()
        );
    }

    /// **本卡的核心反例（读法 B）**：境外通、境内黑洞 —— 不得接管默认路由。
    ///
    /// 若只探境外，这个组合会恒通过，门禁形同虚设；这里断言到**调用序列**上：
    /// `commit` 必须一次都没被调用。
    #[tokio::test]
    async fn gate_refuses_commit_when_domestic_path_is_blackholed() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &["overseas", "domestic"],
            |t: String| {
                log.push(&format!("probe:{t}"));
                let code = if t.as_str() == "domestic" { "000" } else { "204" };
                async move { code.to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        match &res {
            Err(GateFailure::Probe { failed }) => {
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].target, "domestic");
                assert_eq!(failed[0].http_code, "000");
            }
            other => panic!("境内黑洞时必须 Probe 失败，实际 {other:?}"),
        }
        assert_eq!(
            log.seq(),
            // 末尾那一次是**失败重试**：只重试失败的那个目标（domestic），
            // 通过的那个绝不重复探测。
            ["probe:overseas", "probe:domestic", "probe:domestic"],
            "探测不过时 commit 一次都不能被调用（否则默认路由已被接管）"
        );
    }

    /// 境外黑洞（S1：「TCP 通、协议被墙」的形态）→ 同样不得接管。
    #[tokio::test]
    async fn gate_refuses_commit_when_overseas_proxy_path_is_dead() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &["overseas", "domestic"],
            |t: String| {
                log.push(&format!("probe:{t}"));
                let code = if t.as_str() == "overseas" { "000" } else { "204" };
                async move { code.to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        assert!(matches!(res, Err(GateFailure::Probe { .. })), "实际 {res:?}");
        // 末尾那一次是**失败重试**：这个用例里失败的是 overseas，所以重试的也只有它。
        assert_eq!(
            log.seq(),
            ["probe:overseas", "probe:domestic", "probe:overseas"]
        );
    }

    /// 超时（curl 拿不到码 → 空串）也必须算**不通**，且不得接管。
    #[tokio::test]
    async fn gate_treats_empty_code_as_timeout_and_refuses() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &["overseas", "domestic"],
            |t: String| {
                log.push(&format!("probe:{t}"));
                let code = if t.as_str() == "domestic" { "" } else { "204" };
                async move { code.to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        match &res {
            Err(GateFailure::Probe { failed }) => {
                assert_eq!(failed[0].http_code, "", "空串要保留原样，便于排障");
                assert!(failed[0].describe().contains("无响应"), "文案要能读");
            }
            other => panic!("超时必须 Probe 失败，实际 {other:?}"),
        }
        assert_eq!(log.seq(), ["probe:overseas", "probe:domestic", "probe:domestic"]);
    }

    /// 一个目标的响应码不是 204 也算通（403/301 都证明路径真的通）。
    #[test]
    fn probe_responded_accepts_any_real_http_code() {
        for ok in ["200", "204", "301", "403", "500"] {
            assert!(
                ProbeOutcome { target: "t".into(), http_code: ok.into() }.responded(),
                "{ok} 是真实响应，应算通"
            );
        }
        for dead in ["", "000"] {
            assert!(
                !ProbeOutcome { target: "t".into(), http_code: dead.into() }.responded(),
                "{dead:?} 不是响应，应算不通"
            );
        }
    }

    /// 探测全过、commit 自己失败 → 必须报 `Commit` 而不是 `Probe`
    /// （两者的用户文案不同：一个说网络没被动过，一个说已回滚）。
    #[tokio::test]
    async fn gate_surfaces_commit_failure_after_probes_pass() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &["overseas", "domestic"],
            |t: String| {
                log.push(&format!("probe:{t}"));
                async { "204".to_string() }
            },
            || {
                log.push("commit");
                async { Err(xt_proto::HelperError::new(xt_proto::ErrorCode::Internal, "boom")) }
            },
        )
        .await;

        assert_eq!(res, Err(GateFailure::Commit("boom".into())));
        assert_eq!(log.seq(), ["probe:overseas", "probe:domestic", "commit"]);
    }

    /// 目标列表为空是**配置错误**，必须 fail-safe（拒绝接管），不能静默放行。
    #[tokio::test]
    async fn gate_refuses_when_no_targets_are_configured() {
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &[],
            |t: String| {
                log.push(&format!("probe:{t}"));
                async { "204".to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        assert_eq!(res, Err(GateFailure::NoTargets));
        assert!(log.seq().is_empty(), "没配置目标时不该探测、更不该提交");
    }

    /// 门禁确实配了「两类职责 × 两条路径」三个目标 —— 少一类就等于把自己测盲（task-92）。
    #[test]
    fn required_probe_targets_pair_ip_literals_with_domains() {
        let targets = required_probe_urls();
        assert!(targets.len() >= 3, "至少要 3 条（2 个 IP 字面量 + 1 个域名），实际 {targets:?}");
        // 不依赖解析的那一半（IP 字面量）必须存在 —— 这是「传输通不通」的判据
        let ip = probe_targets_without_dns(&targets);
        assert!(
            ip.len() >= 2,
            "至少要有两个**不依赖解析**的目标（境内外各一），实际 {ip:?}"
        );
        assert!(ip.iter().any(|t| t.contains("1.1.1.1")), "缺境外 IP 字面量：{targets:?}");
        assert!(ip.iter().any(|t| t.contains("223.5.5.5")), "缺境内 IP 字面量：{targets:?}");
        // 域名目标必须**保留**：IP 字面量发现不了「只有解析坏」
        assert!(targets.contains(&xt_core::xray::DEFAULT_PROBE_URL), "缺域名目标（解析判据）");
        // `www.baidu.com` 被实测筛掉（经 SOCKS 10 轮 4 失败）—— 别悄悄加回来：
        // 门禁要求每个目标都答，留一个 40% 失败率的目标 = 40% 的连接起不来。
        assert!(
            !targets.iter().any(|t| t.contains("baidu.com")),
            "不要加回 baidu：多轮实测经 SOCKS 10 轮 4 失败（失败方向错的那一侧）",
        );
    }

    /// URL 主机判 IP 字面量必须**严格**：把域名误判成 IP，「不依赖解析」就是假的。
    /// **task-176 守卫（L3 文本层）**：三个采样时点必须在生产源码里各出现一次，
    /// 且都通过 `self.audit_routes(...)` 记录。
    ///
    /// 为什么这里只有文本层：`Supervisor::start` 要真 core/helper/root 才能跑；
    /// **端到端行为证据**在 `commands/core.rs` 的 `log_route_audits` 行为测试
    /// （真的写盘 + 读回）与 `xt-tun` 的解析测试（真实样例 → `Some/None`）。
    /// 这条只负责「**别把采样点删掉**」：删任意一处 ⇒ 本测试红。
    #[test]
    fn every_tun_phase_is_audited_in_production_source() {
        let prod = include_str!("supervisor.rs")
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or("");
        for phase in ["AfterTunUp", "AfterCommitRoutes", "AfterRollback"] {
            let needle = format!("RouteAuditPhase::{phase}");
            assert_eq!(
                prod.matches(&needle).count(),
                1,
                "{phase} 的采样点必须恰好一处（现在 {} 处）",
                prod.matches(&needle).count()
            );
        }
        assert_eq!(
            prod.matches("self.audit_routes(").count(),
            3,
            "三个采样点都要走 `audit_routes`（否则不会进审计日志）"
        );
    }

    #[test]
    fn url_host_is_ip_literal_is_strict() {
        assert!(url_host_is_ip_literal("http://1.1.1.1/"));
        assert!(url_host_is_ip_literal("http://223.5.5.5/"));
        assert!(url_host_is_ip_literal("http://1.1.1.1"));
        assert!(!url_host_is_ip_literal("http://cp.cloudflare.com/generate_204"));
        assert!(!url_host_is_ip_literal("http://www.baidu.com/"));
    }

    /// **task-92 真正要回答的问题**：「只有解析坏、传输通」时怎么判？
    ///
    /// 结论：**判「链路不可用」**（门禁不接管、看门狗计一次失败），理由写在
    /// [`REQUIRED_PROBE_TARGETS`] 的文档里。这条同时钉住两件事：
    /// * IP 字面量目标**活着**（301/404 都算活着 —— 判据只认空/`000`）；
    /// * 失败清单里**只有域名目标** ⇒ 诊断能说清「传输是通的，是解析坏了」；
    /// * 结论仍是**不接管**。
    #[tokio::test]
    async fn only_resolution_broken_is_not_mistaken_for_a_dead_transport() {
        // ① 判据：301/404 都不是「死」，所以 IP 目标不必要求 204
        assert!(!tunnel_is_dead("301"), "IP 字面量回 301 就是活着");
        assert!(!tunnel_is_dead("404"), "IP 字面量回 404 也是活着");
        // ①b 这个场景**只有存在不依赖解析的目标**才有意义 ——
        // 把它们删掉，这条就必然红（敏感性就钉在这里，而不是靠人自觉）。
        let urls = required_probe_urls();
        assert!(
            probe_targets_without_dns(&urls).len() >= 2,
            "「只有解析坏」的判据依赖 IP 字面量目标存在，实际清单：{urls:?}",
        );

        // ② 实际目标清单跑一遍假探测：IP 全活、域名全死
        let log = GateLog::default();
        let res = verify_paths_then_commit(
            &urls,
            |t: String| {
                log.push(&format!("probe:{t}"));
                let code = if t.contains("1.1.1.1") {
                    "301"
                } else if t.contains("223.5.5.5") || t.contains("119.29.29.29") {
                    // task-106：新增的境内 IP 字面量同属「不依赖解析」那一半，必须是活的
                    "404"
                } else {
                    "000"
                };
                async move { code.to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        match &res {
            Err(GateFailure::Probe { failed }) => {
                let names: Vec<&str> = failed.iter().map(|f| f.target.as_str()).collect();
                assert_eq!(failed.len(), 1, "只该有**域名**目标失败（IP 字面量是活的）：{names:?}");
                assert!(
                    names.iter().all(|t| t.contains("cloudflare.com")),
                    "失败的必须只有域名目标 —— 这才是「传输通、只有解析坏」：{names:?}"
                );
            }
            other => panic!("只有解析坏时**也不该**接管默认路由（判不可用），实际 {other:?}"),
        }
        assert!(
            !log.seq().iter().any(|s| s == "commit"),
            "**不得**调用 commit：解析全挂时接管默认路由 = 把一个已经坏掉的体验升级成系统级接管",
        );
    }

    /// **反例（别变成惊弓之鸟）**：所有目标都真答了 ⇒ 必须能提交。
    #[tokio::test]
    async fn gate_still_commits_when_all_required_targets_answer() {
        let log = GateLog::default();
        let urls = required_probe_urls();
        let res = verify_paths_then_commit(
            &urls,
            |t: String| {
                log.push(&format!("probe:{t}"));
                let code = if t.contains("1.1.1.1") {
                    "301"
                } else if t.contains("223.5.5.5") || t.contains("119.29.29.29") {
                    "404"
                } else {
                    "204"
                };
                async move { code.to_string() }
            },
            || {
                log.push("commit");
                async { Ok::<(), xt_proto::HelperError>(()) }
            },
        )
        .await;

        assert!(res.is_ok(), "四条都拿到真实响应就必须能提交，实际 {res:?}");
        assert!(log.seq().iter().any(|s| s == "commit"), "实际序列 {:?}", log.seq());
    }

    // -----------------------------------------------------------------------
    // 门禁失败文案：好消息保留、结论不预设（task-67）
    // -----------------------------------------------------------------------

    /// 门禁拦下时：**「已在接管默认路由之前中止」必须保留**（那是好消息），
    /// 但「请换一个节点后重试」这种唯一归因要去掉，并点出本机网络的可能性。
    #[test]
    fn gate_failure_message_keeps_the_good_news_without_presuming_the_cause() {
        let failure = GateFailure::Probe {
            failed: vec![ProbeOutcome {
                target: "http://www.baidu.com/".into(),
                http_code: "000".into(),
            }],
        };
        let msg = failure.describe();

        // 好消息：门禁生效了，系统网络没被动过
        assert!(msg.contains("已在接管默认路由之前中止"), "实际：{msg}");
        assert!(msg.contains("系统网络未被改动"), "实际：{msg}");
        // 具体的探测证据要保留（哪个目标、拿到什么码）
        assert!(msg.contains("www.baidu.com") && msg.contains("000"), "实际：{msg}");
        // 自救动作必须留着。
        assert!(msg.contains("断开"), "实际：{msg}");
        // 「不把因果唯一归到一处」这条要求仍在，但**候选原因必须是这条路线上真有的**：
        // 域名由节点解析 ⇒ 列出的是节点侧 DNS 与"该域名在节点出口不可达"两种，
        // 而不是旧文案里那个没参与这条路径的"本机网络/DNS"（2026-09-25 更正）。
        assert!(msg.contains("节点自身 DNS"), "要给出节点侧 DNS 这个可能：{msg}");
        assert!(msg.contains("不可达"), "要给出域名在节点出口不可达这个可能：{msg}");
        assert!(!msg.contains("请换一个节点后重试"), "别把因果唯一归到节点：{msg}");
        assert!(
            !msg.contains("本机网络/DNS 被干扰"),
            "本机 DNS 不在这条路径上，不许出现在域名目标的诊断里：{msg}"
        );
    }

    /// **task-92 的诊断**：失败的全是域名目标 ⇒ 要说清「传输通、问题出在域名这一步」。
    ///
    /// # 这条测试在 2026-09-25 修过一次措辞（原来钉的是一个错信念）
    ///
    /// 原断言是 `msg.contains("本机网络")`，配套文案是"更像是解析链路的问题（本机网络/
    /// DNS 被干扰也可能导致）"。但在**当前目标表**上：唯一的域名目标用的是
    /// `--socks5-hostname` ⇒ 域名由**节点**解析，本机 DNS 不在这条路径上。
    /// 一句"本机网络/DNS 被干扰"会把用户引去修一个没参与的东西（现场报错正是这一条）。
    /// 所以现在钉的是**正确**的归因：问题在节点侧，且明说不是本机 DNS。
    #[test]
    fn gate_failure_diagnoses_resolution_only_failures() {
        let msg = GateFailure::Probe {
            failed: vec![ProbeOutcome {
                target: xt_core::xray::DEFAULT_PROBE_URL.into(),
                http_code: "000".into(),
            }],
        }
        .describe();
        assert!(msg.contains("失败的全是域名目标"), "要说清是哪一类失败：{msg}");
        assert!(msg.contains("节点侧"), "域名是节点解析的 ⇒ 归因必须在节点侧：{msg}");
        assert!(
            msg.contains("不是本机 DNS 的问题"),
            "必须明说本机 DNS 不在这条路径上，否则用户会去修它：{msg}"
        );
        assert!(!msg.contains("本机网络/DNS 被干扰"), "旧文案的错信念不许回来：{msg}");
        assert!(msg.contains("断开"), "自救动作保留：{msg}");
        assert!(
            msg.contains("分不开") || msg.contains("边界"),
            "只有一个域名目标 ⇒ 要写出诊断能力的边界，别把猜测说成结论：{msg}"
        );
    }

    /// **`--socks5` vs `--socks5-hostname` 是产品语义，不是 curl 细节。**
    ///
    /// 这条测试存在的理由：现场（2026-09-25）用户 TUN 模式连不上，门禁报"域名目标
    /// 拿不到响应、IP 目标全通" —— 而那是**节点侧 DNS** 的问题，TUN 模式下产品的
    /// 真实路径根本不用节点解析（核心 `dns-out` 解析，节点只看到 IP）。
    /// 也就是说：门禁原本在验一个产品不用的能力，并在它上面 fail-closed。
    /// 所以这里逐字钉住两种模式各自的参数。
    #[test]
    fn the_probe_asks_the_right_side_to_resolve_names() {
        let at_node = curl_probe_args(10808, "http://cp.cloudflare.com/generate_204", 6, ProbeResolve::AtNode);
        assert!(at_node.contains(&"--socks5-hostname".to_string()), "{at_node:?}");
        assert!(!at_node.contains(&"--socks5".to_string()), "{at_node:?}");

        let local =
            curl_probe_args(10808, "http://cp.cloudflare.com/generate_204", 6, ProbeResolve::Locally);
        assert!(
            local.contains(&"--socks5".to_string()),
            "TUN 模式要本机解析：{local:?}"
        );
        assert!(
            !local.contains(&"--socks5-hostname".to_string()),
            "本机解析就不能同时让节点解析：{local:?}"
        );
        // 其余参数两边一致（端口、目标、超时、只看状态码）。
        for args in [&at_node, &local] {
            assert!(args.contains(&"127.0.0.1:10808".to_string()));
            assert!(args.contains(&"%{http_code}".to_string()));
            assert!(args.contains(&"6".to_string()));
            assert!(args.contains(&"http://cp.cloudflare.com/generate_204".to_string()));
        }
    }

    /// **门禁的失败重试**：首次全失败、重试后拿到响应 ⇒ 继续（并只 commit 一次）。
    ///
    /// 这条与 `a_single_transient_probe_failure_is_retried_and_does_not_abort`
    /// 是同一个理由在另一道门禁上的落地：单发探测的一次抖动不该把整次启动打掉。
    #[tokio::test]
    async fn the_gate_retries_failed_targets_once_and_continues() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicU64::new(0));
        let commits = Arc::new(AtomicU64::new(0));
        let (c1, c2) = (calls.clone(), commits.clone());
        let res = verify_paths_then_commit(
            &["http://a.test/", "http://b.test/"],
            move |_t: String| {
                let n = c1.fetch_add(1, Ordering::Relaxed);
                async move {
                    // 每个目标：第一次空（没响应），第二次给真实状态码。
                    if n < 2 {
                        String::new()
                    } else {
                        "204".to_string()
                    }
                }
            },
            move || {
                c2.fetch_add(1, Ordering::Relaxed);
                async { Ok(()) }
            },
        )
        .await;
        assert!(res.is_ok(), "重试后拿到响应就该继续，实际：{res:?}");
        assert_eq!(calls.load(Ordering::Relaxed), 4, "两个目标各探测两次");
        assert_eq!(commits.load(Ordering::Relaxed), 1, "commit 只能调一次");
    }

    /// **负对照**：重试后还是拿不到 ⇒ 仍然中止，而且**不许** commit。
    ///
    /// 没有这一条，"加了重试"就可能变成"把门禁放宽成不检查"。
    #[tokio::test]
    async fn the_gate_still_aborts_when_the_retry_also_fails() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicU64::new(0));
        let commits = Arc::new(AtomicU64::new(0));
        let (c1, c2) = (calls.clone(), commits.clone());
        let res = verify_paths_then_commit(
            &["http://a.test/"],
            move |_t: String| {
                c1.fetch_add(1, Ordering::Relaxed);
                async { String::new() }
            },
            move || {
                c2.fetch_add(1, Ordering::Relaxed);
                async { Ok(()) }
            },
        )
        .await;
        assert!(matches!(res, Err(GateFailure::Probe { .. })), "实际：{res:?}");
        assert_eq!(calls.load(Ordering::Relaxed), 2, "只重试一次（不是无限重试）");
        assert_eq!(commits.load(Ordering::Relaxed), 0, "没通过就绝不许 commit");
    }

    /// **反例**：IP 字面量目标也失败了 ⇒ **不许**说「传输通/只有解析坏」。
    #[test]
    fn gate_failure_does_not_claim_resolution_only_when_an_ip_target_failed() {
        let msg = GateFailure::Probe {
            failed: vec![ProbeOutcome {
                target: "http://1.1.1.1/".into(),
                http_code: "000".into(),
            }],
        }
        .describe();
        assert!(
            !msg.contains("失败的全是域名目标"),
            "IP 目标都失败了还说「只有解析坏」就是编造：{msg}",
        );
        assert!(!msg.contains("解析链路"), "同上：{msg}");
        // 这种情况仍要给原本的多种可能与自救动作
        assert!(msg.contains("本机网络") && msg.contains("断开"), "实际：{msg}");
    }

    // -----------------------------------------------------------------------
    // task-82 (a)/(c)：`direct` 出站的网卡绑定必须是**这次**探测到的那张
    // -----------------------------------------------------------------------

    /// **核心断言**：配置里的 `sockopt.interface` 就是传进来的那张网卡。
    ///
    /// 为什么这一条就够说明「换网后能恢复」：`Supervisor::start` **每次启动都会
    /// 重新探测默认路由**（本文件 `start()` 里那段 `default_route()` +
    /// `self.physical_interface = ...`），而 (a) 让换网后走一次 stop + start。
    /// ⇒ 新配置里的 direct 出站会绑到**新**网卡（旧实现换网后只写日志、不重建，
    /// 所以一直绑在旧网卡上 ⇒ 国内分流全断，task-82 根因）。
    #[test]
    fn config_binds_direct_outbound_to_the_freshly_detected_interface() {
        let settings = AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        };
        let build = |iface: Option<&str>| {
            xt_core::xray::build_pretty(&xt_core::xray::CoreConfigInput {
                settings: &settings,
                nodes: &[],
                selected: None,
                rules: &[],
                profile: crate::state::profile_for(&settings, iface, true),
                physical_interface: iface,
            })
        };
        let interface_of = |config: &str| -> String {
            let v: serde_json::Value =
                serde_json::from_str(config).expect("生成的配置必须是合法 JSON");
            v["outbounds"]
                .as_array()
                .expect("要有 outbounds")
                .iter()
                .find(|o| o["tag"] == "direct")
                .and_then(|o| o["streamSettings"]["sockopt"]["interface"].as_str())
                .unwrap_or("<缺失>")
                .to_string()
        };

        let baseline = build(Some("en0"));
        let after_move = build(Some("en5"));
        assert_eq!(interface_of(&baseline), "en0");
        assert_eq!(
            interface_of(&after_move),
            "en5",
            "换网重建后 direct 出站必须绑**新**网卡 —— 否则国内分流仍旧走 en0（task-82）",
        );
        // 非 TUN 模式没有物理出口时不写这个字段（别凭空造一个网卡名）。
        assert_eq!(interface_of(&build(None)), "<缺失>");
    }

    /// **task-122 A-3 守卫（task-134 收窄成「按站点 + 正向断言」）**：核心退出失败不许
    /// 静默吞掉 —— **每一处** `process.shutdown(` 都要把 Err 绑下来并 `tracing::warn!`。
    ///
    /// 原来是文件级「不许出现 `let _ = process.shutdown(`」：换成 `.ok()` 或别的写法
    /// 就能绕过（tester 的 M3 是同族缺口）。现在逐个站点正向检查，并带两条负例。
    #[test]
    fn core_shutdown_result_is_not_swallowed_in_production_source() {
        // ⚠️ **不能**按第一个 `#[cfg(test)]` 截断：本文件在 :232 就有一个
        // （测试用的小函数），那样会把 :594 起的 warn 也切掉 —— 这条守卫第一次
        // 跑就因为这个假红了。按**测试模块**的锚点切。
        let src = include_str!("supervisor.rs");
        let prod = match src.find("\n#[cfg(test)]\nmod tests") {
            Some(i) => &src[..i],
            None => src,
        };
        assert!(
            every_shutdown_failure_is_warned(prod),
            "每一处 `process.shutdown(` 都要绑 Err 并 tracing::warn!（失败不许静默）"
        );
        assert_eq!(
            prod.matches("process.shutdown(").count(),
            prod.matches("if let Err(e) = process.shutdown(").count(),
            "不许有站点用别的写法（`let _ =` / `.ok()` 都不行）"
        );

        // 负例 1（task-122 之前的写法）：`let _ =`。
        let m1 = prod.replace(
            "if let Err(e) = process.shutdown(",
            "let _ = process.shutdown(",
        );
        assert_ne!(m1, prod, "fixture 必须真的改到生产源码");
        assert!(
            !every_shutdown_failure_is_warned(&m1),
            "`let _ = process.shutdown(…)` 必须被判据抓住"
        );

        // 负例 2（换一种吞法，与 tester 的 M3 同族）：`.ok()`。
        let m2 = prod.replace(
            "if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {",
            "process.shutdown(CORE_SHUTDOWN_GRACE).await.ok();",
        );
        assert_ne!(m2, prod, "fixture 必须真的改到生产源码");
        assert!(
            !every_shutdown_failure_is_warned(&m2),
            "`.ok()` 这种吞法也必须被抓（这是原守卫的 lint 缺口）"
        );

        // 负例 3（tester 在 task-157 实测的**窗口绕过**）：本站点改成 `let _ = e;`，
        // 靠同一段里**邻近**的 `return Err(format!("核心未在预期时间内就绪…"))` 充数。
        // 400B 窗口判据对它绿 —— 块级判据必须抓住。
        let m3 = prod.replace(
            "tracing::warn!(error = %e, \"数据面进程未干净退出（网络配置仍会单独回滚）\");",
            "let _ = e; // 吞掉：靠邻近的 return Err 给窗口判据充数",
        );
        assert_ne!(m3, prod, "fixture 必须真的改到生产源码");
        assert!(
            !every_shutdown_failure_is_warned(&m3),
            "本站点吞掉、靠邻近 `return Err(` 充数 —— 块级判据必须红（task-157 的窗口绕过）"
        );

        // 负例 4（tester 的 s3）：把留痕塞进**不可达**分支 —— 朴素判据「块内出现 warn」
        // 会绿，而 `if false` 在 release 下等于静默。handler 必须在块的**顶层**才算数。
        let s3 = prod.replace(
            "tracing::warn!(error = %e, \"数据面进程未干净退出（网络配置仍会单独回滚）\");",
            "if false {\n                    tracing::warn!(error = %e, \"数据面进程未干净退出（网络配置仍会单独回滚）\");\n                }",
        );
        assert_ne!(s3, prod, "s3 fixture 必须真的改到生产源码");
        assert!(
            !every_shutdown_failure_is_warned(&s3),
            "不可达留痕（`if false {{ tracing::warn! }}`）必须红 —— tester 的 s3"
        );

        // 负例 5：顶层 warn，但**不引用**被绑定的错误名（空泛留痕不该算 handled，
        // 否则 `tracing::warn!("出错了")` 就能给任何站点充数）。
        let m5 = prod.replace(
            "tracing::warn!(error = %e, \"数据面进程未干净退出（网络配置仍会单独回滚）\");",
            "tracing::warn!(\"核心没退干净\");",
        );
        assert_ne!(m5, prod, "m5 fixture 必须真的改到生产源码");
        assert!(
            !every_shutdown_failure_is_warned(&m5),
            "顶层但不打印错误的 warn 不算留痕"
        );

        // 正向：**改绑定名**是无害重构（判据从 `if let Err(<name>) = ` 动态取名）⇒ 不许假红。
        let p1 = prod.replace(
            "if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {\n                tracing::warn!(error = %e, \"数据面进程未干净退出（网络配置仍会单独回滚）\");",
            "if let Err(shutdown_err) = process.shutdown(CORE_SHUTDOWN_GRACE).await {\n                tracing::warn!(error = %shutdown_err, \"数据面进程未干净退出（网络配置仍会单独回滚）\");",
        );
        assert_ne!(p1, prod, "p1 fixture 必须真的改到生产源码");
        assert!(
            every_shutdown_failure_is_warned(&p1),
            "改绑定名是无害重构，守卫不许假红"
        );

        assert!(
            prod.contains("数据面进程未干净退出"),
            "要留下可搜的痕迹，说明「核心没干净退出、但网络仍会单独回滚」"
        );
    }

    /// **正向判据（task-160 起改成「站点级块判据」）**：每个 `process.shutdown(` 站点
    /// 前面必须是 `if let Err(e) = `，而且**这一站自己的块内**必须**处理**了它 ——
    /// `tracing::warn!` 留痕，**或**把错误攒进 `errors`（第 6 处在 `stop` 的收尾里就是
    /// 攒起来一起返回的，比 warn 更强）。
    ///
    /// # 为什么从「后 400B 窗口」改成「本站点块」
    ///
    /// `task-157` 实测：本站点写成 `let _ = e;` 时，窗口里**邻近**的
    /// `return Err(format!("核心未在预期时间内就绪…"))` 会给它充数 ⇒ 守卫仍绿。
    /// 这正是 `docs/verification/GUARD-FALSE-GREEN-PATTERNS.md` §2 R1（「保留受检文本 +
    /// 运行时吞掉」）与 §3「站点无法注入时：必须站点级**块**判据」那一条。
    fn every_shutdown_failure_is_warned(src: &str) -> bool {
        let mut sites = 0usize;
        let mut from = 0usize;
        while let Some(rel) = src[from..].find("process.shutdown(") {
            let idx = from + rel;
            sites += 1;
            let before = window_before(src, idx, 60);
            // 只要求「是 `if let Err(<名>) = ` 这种绑定」，**不锚具体名字** ——
            // 锚 `e` 会挡住无害的重命名重构（与 tester 的 `n5` 同族：守卫对形状过敏 ⇒ 假红；
            // 这条是被 `p1` fixture 实测出来的）。
            if !before.contains("if let Err(") {
                return false;
            }
            // **本站点自己的块**：从它后面第一个 `{` 到配对 `}`，不看邻近文本。
            let Some(open_rel) = src[idx..].find('{') else {
                return false;
            };
            let open = idx + open_rel;
            let Some(close) = matching_brace(src, open) else {
                return false;
            };
            let block = &src[open + 1..close];
            // **被绑定的错误名**（从 `if let Err(<name>) = ` 里取；不锚具体名字 ⇒ 改名不误伤）。
            let bound = before
                .rfind("if let Err(")
                .map(|p| {
                    let rest = &before[p + "if let Err(".len()..];
                    rest.split(')').next().unwrap_or("").trim().to_string()
                })
                // `_` 等于没绑定错误 ⇒ 不算取了名字。
                .filter(|n| !n.is_empty() && n != "_");
            // **handler 必须在块的顶层**（task-163 的 s3：`if false { warn! }` 的 handler
            // 在深度 1，属于「不可达留痕」，不许算 handled），并且要**引用那个错误名**。
            if !handler_at_top_level(block, bound.as_deref()) {
                return false;
            }
            from = close;
        }
        sites > 0
    }

    /// 块内**顶层**（深度 0）是否有一个 handler，且它**引用了被绑定的错误名**。
    ///
    /// 为什么要「顶层」：`if false { tracing::warn!(..) }` 也是「块内出现 warn」，
    /// 但那条留痕**不可达**（tester 的 `s3`；release 下等于静默）。为什么要求引用错误名：
    /// 否则空 warn 也能充数。
    fn handler_at_top_level(block: &str, bound: Option<&str>) -> bool {
        let mut depth = 0usize;
        for (i, c) in block.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth != 0 {
                continue;
            }
            let rest = &block[i..];
            for h in ["tracing::warn!", "errors.push(", "return Err("] {
                if rest.starts_with(h) {
                    // handler 的实参窗口里必须出现被绑定的错误名。
                    let window = &rest[..rest.len().min(200)];
                    if let Some(name) = bound {
                        if window.contains(name) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// 从 `open`（一个 `{` 的下标）找配对的 `}`（朴素深度计数；被测块的括号都成对）。
    fn matching_brace(src: &str, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(open + i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 取 `idx` 之前至多 `n` 字节的窗口。**必须向内收**到字符边界为止 ——
    /// 本文件到处是中文注释，直接 `&src[idx-60..idx]` 会落在多字节字符中间
    /// （`get` 返回 `None` ⇒ 窗口变空 ⇒ **判据假红**；这条第一次跑就踩了）。
    fn window_before(src: &str, idx: usize, n: usize) -> &str {
        let mut start = idx.saturating_sub(n);
        while start < idx {
            if let Some(s) = src.get(start..idx) {
                return s;
            }
            start += 1;
        }
        ""
    }

}
