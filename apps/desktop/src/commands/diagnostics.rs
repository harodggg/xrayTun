//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use xt_core::model::{Node, Transport};

#[tauri::command]
pub async fn tail_logs(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<crate::state::LogEntry>, String> {
    let limit = limit.unwrap_or(400);
    // 先读**文件**：它跨重启、跨轮转，能拿到「上次开机那一刻」的日志 ——
    // 那正是排查「开机后没自动连上」唯一有用的证据。
    //
    // 读盘放到阻塞线程池：这是 `async fn`，而 `tail_logs` 会 read_dir +
    // 读整个文件 + 逐行 JSON 解析（上限约 10MB），直接在这里做会占住
    // tokio worker（本 crate 别处的阻塞工作也是走 spawn_blocking）。
    let root = state.store.root().to_path_buf();
    let (mut from_file, stats): (Vec<crate::state::LogEntry>, _) =
        tauri::async_runtime::spawn_blocking(move || {
            xt_core::store::Store::new(root).tail_logs_with_stats(limit)
        })
        .await
        .unwrap_or_default();
    if !from_file.is_empty() {
        // **读的时候丢了东西 ⇒ 必须在用户看得见的地方说出来**（task-110）。
        //
        // 为什么不能只 `tracing::warn!`：tracing 走 stderr（`lib.rs` 的
        // `with_writer(std::io::stderr)`），而 GUI 从 Finder/Dock 启动时看不到
        // stderr —— 那条通道只有开发者用。日志页的数据源就是这个文件，所以
        // 「读侧发现的问题」只能**变成日志里的一条记录**才谈得上用户可见。
        //
        // 去重（见 `LossNotify`）：坏行会一直留在文件里，不去重就会每刷新一次
        // 日志就写一条自己的提醒，把日志刷爆。
        if let Some(warning) = loss_warning(&stats) {
            let first_time = {
                let mut guard = LOSS_NOTIFY.lock().unwrap_or_else(|e| e.into_inner());
                guard.mark(loss_signature(&stats))
            };
            if first_time {
                state.log("app", "warn", warning);
                // 让这一条**本次**就能被用户看到：重读一次（只在首次提醒时发生）。
                let root = state.store.root().to_path_buf();
                if let Ok((again, _)) = tauri::async_runtime::spawn_blocking(move || {
                    xt_core::store::Store::new(root).tail_logs_with_stats(limit)
                })
                .await
                {
                    from_file = again;
                }
            }
        }
        return Ok(from_file);
    }
    // 文件还没有（首次运行、或写入失败）：退回内存缓冲，至少不空手。
    state
        .with(|i| {
            let skip = i.logs.len().saturating_sub(limit);
            i.logs.iter().skip(skip).cloned().collect::<Vec<_>>()
        })
        .ok_or_else(|| "应用状态不可用".to_string())
}

/// 进程内「丢行提醒」的去重状态：**同一个签名只提醒一次**。
///
/// 签名变化（坏行数量或样本变了）说明出现了**新的**读取问题，值得再提醒一次。
#[derive(Debug, Default)]
pub(crate) struct LossNotify {
    last: Option<String>,
}

impl LossNotify {
    /// 这个签名要不要提醒？返回 `true` 表示「是新的，去提醒」，并记住它。
    pub(crate) fn mark(&mut self, signature: String) -> bool {
        if self.last.as_deref() == Some(signature.as_str()) {
            return false;
        }
        self.last = Some(signature);
        true
    }
}

/// 进程级的丢行去重状态（跨 Tauri 命令调用保持）。
static LOSS_NOTIFY: std::sync::Mutex<LossNotify> = std::sync::Mutex::new(LossNotify { last: None });

/// 丢行的**签名**：坏行数与坏行样本都相同才算「同一个问题」。
pub(crate) fn loss_signature(stats: &xt_core::store::TailLogStats) -> String {
    format!("{}|{}", stats.malformed_lines, stats.bad_lines.join(","))
}

/// 有内容读不出来时的**用户可见**提醒（没有丢失则 `None`）。
///
/// 措辞要求：**不许把「有丢失」说成「正常」**；说清「读不出来的是哪几行」，
/// 并给出下一步（把这行一起贴出去）。
pub(crate) fn loss_warning(stats: &xt_core::store::TailLogStats) -> Option<String> {
    if !stats.has_loss() {
        return None;
    }
    let samples = if stats.bad_lines.is_empty() {
        String::new()
    } else {
        format!("（具体位置：{}）", stats.bad_lines.join("、"))
    };
    Some(format!(
        "读取日志时发现 {} 行无法解析，这些行的内容读不出来{samples}；其余日志不受影响，已继续读出（这不是正常情况）",
        stats.malformed_lines
    ))
}

/// 诊断报告里的**读取统计**一行（用户会把它贴到 issue 里 ⇒ 必须自解释、无黑话）。
pub(crate) fn read_stats_line(stats: &xt_core::store::TailLogStats, shown: usize) -> String {
    let mut parts = vec![format!("读取 {} 个日志文件", stats.files_read)];
    // **行与记录都写**：多对象行存在时两者不相等，而「记录」才是用户关心的条数
    // （旧版本会在这种行上整行丢两条）。
    parts.push(format!("共 {} 行 / {} 条记录", stats.lines, stats.records));
    if stats.multi_object_lines > 0 {
        parts.push(format!(
            "其中 {} 行一条里含多条记录（已全部读出，旧版本会整行丢掉）",
            stats.multi_object_lines
        ));
    }
    if stats.has_loss() {
        parts.push(format!(
            "**{} 行无法解析、内容读不出来**（具体位置：{}）",
            stats.malformed_lines,
            if stats.bad_lines.is_empty() {
                "未记录".to_string()
            } else {
                stats.bad_lines.join("、")
            }
        ));
    } else {
        parts.push("0 行无法解析".to_string());
    }
    if stats.files_unreadable > 0 {
        parts.push(format!("另有 {} 个日志文件打不开", stats.files_unreadable));
    }
    // **别把「只显示最近 N 条」写成「截断」**：真实日志上这一项是 35 万量级，
    // 写成「截断 354428 行」会让用户以为丢了东西 —— 而这行存在的意义恰恰是
    // 让人分清「丢」与「没丢」。
    let intact = !stats.has_loss() && stats.files_unreadable == 0;
    if stats.truncated > 0 {
        parts.push(format!(
            "{}只列出最近 {shown} 条（更早的日志没丢，只是没显示）",
            if intact { "内容完整，" } else { "" }
        ));
    } else {
        parts.push(format!("全部列出（{shown} 条）"));
    }
    format!("日志读取统计：{}\n", parts.join("；"))
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    state.with(|i| i.logs.clear());
    // **文件也要清。** `tail_logs` 优先读文件，只清内存缓冲的话界面刷新一次
    // 日志就全回来了 —— 那不是「清空」，是把用户当傻子。
    state
        .store
        .clear_logs()
        .map_err(|e| format!("清空日志文件失败：{e}"))
}

/// 生成一份可直接贴给维护者的诊断报告。
///
/// 刻意**不包含**节点地址/域名、IP、订阅 URL 凭据、UUID 等敏感信息 —— 用户会把它
/// 贴到公开的 issue 里，所以在生成端就把它们抹掉，而不是指望用户自己删。
/// **具体抹什么、依据什么判据**见 [`redact_secrets`]（task-124 起才与这句话相符：
/// 此前 `redact_secrets` 只抹 UUID 形状的 token，节点地址是**原样**留在报告里的）。
#[tauri::command]
pub async fn diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let snap = snapshot::build_snapshot(&app, &state).await?;
    // 地址判据来自**当前节点列表**（不是猜日志里哪种形状像域名）。
    let addresses = NodeAddresses::from_nodes(&snap.nodes);
    let mut out = String::new();
    out.push_str(&format!("XrayTun {}\n", snap.app_version));
    out.push_str(&format!("macOS: {}\n", util::macos_version()));
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
    // 与 `tail_logs` 用**同一个来源**：报告是用户贴到 issue 里的东西，
    // 而重启之后内存缓冲是空的 —— 那时报告里最该有的恰恰是重启前那段日志。
    //
    // 这一行**读取统计**（task-110）是给用户看的：读的时候有没有丢行、
    // 有没有「一行里多条记录」（旧版本会整行丢），都在这里说清楚。
    // 报告是贴出去给维护者的产物 ⇒ 措辞不许含糊、不许把「有丢失」说成「正常」。
    let (recent, read_stats): (Vec<crate::state::LogEntry>, _) =
        state.store.tail_logs_with_stats(50);
    let recent = if recent.is_empty() {
        state.with(|i| i.logs.iter().rev().take(50).cloned().collect::<Vec<_>>()).unwrap_or_default()
    } else {
        recent
    };
    out.push_str(&read_stats_line(&read_stats, recent.len()));
    if let Some(warning) = loss_warning(&read_stats) {
        out.push_str(&format!("⚠ {warning}\n"));
    }
    if !recent.is_empty() {
        for entry in recent {
            out.push_str(&report_log_line(&entry, &addresses));
        }
    }
    Ok(out)
}

/// 诊断报告里**一条日志**的渲染（纯函数，可测）：`[UTC 时间] [source] level message`。
///
/// 以前没有时间 —— 报告因此是「有版本标签、**零时间锚点**」的产物：
/// 用户贴到 issue 里，谁都说不清「这是什么时候的日志」。`entry.ts_unix` 本来就在手里。
///
/// `addresses` 必须传进来：**报告的头与正文走同一条脱敏**（task-124 第 2 条）。
/// 封套里的时间/source/level 以及报告头部的 App/核心/helper 版本、模式、日志级别
/// **都是自锚定信息，一律不动** —— 那是排查必需，且不含隐私。
pub(crate) fn report_log_line(
    entry: &crate::state::LogEntry,
    addresses: &NodeAddresses,
) -> String {
    format!(
        "[{}] [{}] {} {}\n",
        utc_iso(entry.ts_unix),
        entry.source,
        entry.level,
        redact_secrets(&entry.message, addresses)
    )
}

/// 把 Unix 秒渲染成 **UTC ISO 8601**（例如 `2026-09-22T12:29:17Z`）。
///
/// 为什么用 UTC、而不是本地时间：本项目**没有时区库**（`access_log` 的注释写过
/// 「日志是本地时间且不带时区，换算需要时区库」），而**猜时区**、或者为报告里
/// 每一行去 spawn 一个 `date`，都是坏主意。前端日志页导出的文本用的就是
/// `new Date(ts * 1000).toISOString()` ⇒ 这里与它**同口径**，两边贴出来能对上。
///
/// 算法是 Howard Hinnant 的 `civil_from_days`（只用到整除，无依赖、可单测）。
pub(crate) fn utc_iso(ts_unix: u64) -> String {
    let days = (ts_unix / 86_400) as i64;
    let secs = ts_unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
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

/// 抹掉 URL 里的凭据部分，只保留 host。
///
/// 机场订阅的 URL 里带 token，用户把日志贴出来就等于把订阅泄漏了。
pub(crate) fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}://{}/…", u.scheme(), u.host_str().unwrap_or("<unknown>")),
        Err(_) => "<非法 URL>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// task-124：报告的地址脱敏
//
// **判据是「出现在当前节点列表里」，不是「长得像域名」。** 按形状抹会把
// `www.baidu.com` 这类**公开、且排查必需**的目标域名一起抹掉，报告就没用了；
// 而节点域名/地址必须抹 —— 那是用户自己的服务器。
// ---------------------------------------------------------------------------

/// 报告里会被替换成 `<addr>` 的地址集合 —— 由**当前节点列表**派生。
///
/// # 收什么（都来自节点条目本身）
///
/// * `Node::address`（域名或 IP，不含方括号）；
/// * `tls.server_name`（SNI；留空时核心会回退到 `address`，见 `xt_core::model`）；
/// * 传输层的 `host`（WebSocket / HttpUpgrade / XHTTP 的 Host 头 —— 那通常
///   就是用户服务器的域名）；
/// * **节点名里形如域名/IP 的段**：订阅常把节点名起成 `jp1.example.com`，
///   名字里的国旗/「香港 01」这类标签不含地址，不动。
#[derive(Debug, Default, Clone)]
pub(crate) struct NodeAddresses {
    /// 去重后按**长度降序**（见 `from_nodes` 里的理由）。
    entries: Vec<String>,
    /// `entries` 里本身是 IP 字面量的那些 —— 命中即替换，**私网/自建的也算**。
    ips: Vec<IpAddr>,
}

impl NodeAddresses {
    pub(crate) fn from_nodes(nodes: &[Node]) -> Self {
        let mut raw: Vec<String> = Vec::new();
        for node in nodes {
            push_address(&mut raw, &node.address);
            push_address(&mut raw, &node.tls.server_name);
            if let Some(host) = transport_host(&node.transport) {
                push_address(&mut raw, host);
            }
            for run in hostname_runs(&node.name) {
                push_address(&mut raw, &run);
            }
        }
        // 长串优先：否则短串会把长串的前缀先吃掉、留下尾巴
        // （例如 `1.2.3.4` 与 `1.2.3.45` 同时存在时，后者必须整串替换）。
        raw.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut entries: Vec<String> = Vec::new();
        for e in raw {
            if !entries.iter().any(|o: &String| o.eq_ignore_ascii_case(&e)) {
                entries.push(e);
            }
        }
        let ips = entries
            .iter()
            .filter_map(|e| e.parse::<IpAddr>().ok())
            .collect();
        Self { entries, ips }
    }

    /// 这个 IP 是否**就是**节点列表里的某个地址。
    pub(crate) fn contains(&self, ip: &IpAddr) -> bool {
        self.ips.iter().any(|x| x == ip)
    }

    /// 在 `i` 处匹配一个节点地址/域名（忽略大小写），连同紧跟的 `:port`。
    ///
    /// 边界只挡 `[A-Za-z0-9_-]`，**不挡 `.`**：这样
    /// `www.<节点域名>` 与 `<节点域名>.cn` 里的节点域名都会被吃掉，
    /// 不会因为多了个前缀/后缀就把节点域名漏在报告里。
    fn match_at(&self, line: &str, i: usize) -> Option<usize> {
        if !boundary_before(line, i) {
            return None;
        }
        for cand in &self.entries {
            let end = i + cand.len();
            let Some(slice) = line.get(i..end) else { continue };
            if !slice.eq_ignore_ascii_case(cand) || !boundary_after(line, end) {
                continue;
            }
            return Some(cand.len() + optional_port_len(&line[end..]));
        }
        None
    }
}

/// 一段文本里**形如地址**的那些段（用于节点名）：按 `[A-Za-z0-9._-]` 切段，
/// 再要求每段**整体**是 IP 字面量或域名形状 —— 这一步只在**节点列表内部**做，
/// 不拿它去扫日志内容。
fn hostname_runs(name: &str) -> Vec<String> {
    name.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
        .map(|t| t.trim_matches(|c: char| c == '.' || c == '-' || c == '_'))
        .filter(|t| address_shaped(t))
        .map(str::to_string)
        .collect()
}

fn address_shaped(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok() || hostname_shaped(s)
}

/// 域名形状：≥2 个标签、标签内只用 `[A-Za-z0-9_-]`、顶级标签 ≥2 字符且含字母
/// （`1.1` 这类节点名因此不会被当成域名 —— 否则会把日志里的 `1.1` 到处误伤）。
fn hostname_shaped(s: &str) -> bool {
    let labels: Vec<&str> = s.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.len() >= 2 && tld.chars().any(|c| c.is_ascii_alphabetic()))
}

fn push_address(out: &mut Vec<String>, s: &str) {
    let s = s.trim().trim_matches(|c| c == '[' || c == ']');
    // 太短的串会到处误伤（例如节点名里的 `HK`）；带空白的不是地址。
    if s.len() >= 4 && !s.chars().any(char::is_whitespace) {
        out.push(s.to_string());
    }
}

fn transport_host(t: &Transport) -> Option<&str> {
    match t {
        Transport::WebSocket { host, .. }
        | Transport::HttpUpgrade { host, .. }
        | Transport::Xhttp { host, .. } => Some(host.as_str()),
        _ => None,
    }
}

/// 报告用的脱敏：**UUID 形状 token → `<uuid>`；地址 → `<addr>`；其余字节原样**。
///
/// # 只替换、不重排
///
/// 换行、缩进、连续空格全部保留。早先的写法是 `split_whitespace().join(" ")`，
/// 它会把多行报错（配置片段、栈、对齐过的表格）塌成一坨，恰好毁掉排查时最需要的
/// 结构；没有东西可抹时也必须逐字节原样返回。
///
/// # 地址三条规则（判据里没有「猜形状的域名正则」）
///
/// 1. **命中节点列表**（[`NodeAddresses`]，含私网/自建节点）⇒ `<addr>`；
/// 2. **未命中节点列表的公网 IP 字面量** ⇒ `<addr>`。这条堵的是节点列表
///    **盖不住**的两类真实泄漏：① 域名形式的节点，日志里出现的是**解析后的 IP**；
///    ② **已经被切走的旧节点**的 IP（本机那条 `45.207.197.185 → 192.168.0.1 en0`
///    的 host 路由就是例子）；
/// 3. **未命中节点列表的本机管道地址 ⇒ 原样保留**：`127.0.0.0/8`、`::1`、RFC1918
///    （`10/8`、`172.16/12`、`192.168/16`）、链路本地 `169.254/16`、ULA `fc00::/7`、
///    `0.0.0.0`/`::`、组播与广播。它们描述的是**本机自己的拓扑**，不带身份信息，
///    却是排查的命门 —— 「回环洞」的判据正是 `route -n get 127.0.0.2` 的 interface
///    不是 `lo0`；把这些全抹掉等于把它变成永久不可诊断。
///    公网判据是**保守**的：除上面这些，一律按公网处理（CGNAT、文档网段也抹）。
///
/// `IP:port` / `[v6]:port` 连端口一起替换（端口本身不含身份，但留着没有意义）。
pub(crate) fn redact_secrets(line: &str, addresses: &NodeAddresses) -> String {
    redact_with(line, addresses, true)
}

fn redact_with(line: &str, addresses: &NodeAddresses, allow_url: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    while i < line.len() {
        if let Some(len) = match_uuid(line, i) {
            out.push_str("<uuid>");
            i += len;
            continue;
        }
        if allow_url {
            if let Some(len) = match_credentialed_url(line, i) {
                // URL 只保留 scheme + host（既有 `redact_url` 的口径），
                // 但**保留下来的 host 还要再走一遍地址规则** —— 否则
                // `https://<节点域名>/…` 会把节点域名原样留在报告里。
                let trimmed = redact_url(&line[i..i + len]);
                out.push_str(&redact_with(&trimmed, addresses, false));
                i += len;
                continue;
            }
        }
        if let Some((ip, len)) = match_ip_literal(line, i) {
            if addresses.contains(&ip) || is_public_ip(ip) {
                out.push_str("<addr>");
                i += len;
                continue;
            }
            // 本机管道地址：原样保留（见函数注释第 3 条）—— 不 continue，
            // 落到下面逐字符原样输出。
        }
        if let Some(len) = addresses.match_at(line, i) {
            out.push_str("<addr>");
            i += len;
            continue;
        }
        let ch = line[i..].chars().next().expect("i 落在字符边界上");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 边界：**匹配串之前**那个字符不能是 `[A-Za-z0-9_-]`（`.` 允许 —— 见
/// `NodeAddresses::match_at`：这样才能吃掉 `www.<节点域名>` 的前缀）。
fn boundary_before(line: &str, at: usize) -> bool {
    !is_word_char(line[..at].chars().next_back())
}

/// 边界：**匹配串之后**那个字符不能是 `[A-Za-z0-9_-]`。
fn boundary_after(line: &str, at: usize) -> bool {
    !is_word_char(line[at..].chars().next())
}

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn match_uuid(line: &str, i: usize) -> Option<usize> {
    let end = i + 36;
    let slice = line.get(i..end)?;
    if !util::is_uuid_like(slice)
        || !boundary_before(line, i)
        || !boundary_after(line, end)
    {
        return None;
    }
    Some(36)
}

/// `IP:port` / 裸 `IP` / `[v6]:port` / 裸 `[v6]`。返回 (地址, 匹配的字节数)。
fn match_ip_literal(line: &str, i: usize) -> Option<(IpAddr, usize)> {
    if !boundary_before(line, i) {
        return None;
    }
    let rest = &line[i..];
    // 带方括号的 IPv6（Xray 日志里的形式：`tcp:[240e:…]:443`）。
    if let Some(inner) = rest.strip_prefix('[') {
        let close = inner.find(']')?;
        let ip: Ipv6Addr = inner.get(..close)?.parse().ok()?;
        let len = 1 + close + 1 + optional_port_len(&inner[close + 1..]);
        return boundary_after(line, i + len).then_some((IpAddr::V6(ip), len));
    }
    // 取一段地址字符，够长就行（v6 最长 39 + 端口 6）。
    let run_len = rest
        .bytes()
        .take(48)
        .take_while(|b| b.is_ascii_hexdigit() || *b == b'.' || *b == b':')
        .count();
    let run = rest.get(..run_len)?;
    // 四段点分 + 可选端口。
    let head = run.split(':').next().unwrap_or(run);
    if let Ok(v4) = head.parse::<Ipv4Addr>() {
        let len = head.len() + optional_port_len(&run[head.len()..]);
        return boundary_after(line, i + len).then_some((IpAddr::V4(v4), len));
    }
    // 裸 IPv6（`12:34:56` 这类三段不是合法 v6，因此时间戳不会被误抹）。
    if let Ok(v6) = run.parse::<Ipv6Addr>() {
        return boundary_after(line, i + run.len()).then_some((IpAddr::V6(v6), run.len()));
    }
    None
}

fn optional_port_len(after: &str) -> usize {
    let Some(digits) = after.strip_prefix(':').map(|r| {
        r.bytes().take_while(u8::is_ascii_digit).count()
    }) else {
        return 0;
    };
    if digits == 0 || digits > 5 { 0 } else { 1 + digits }
}

/// `http(s)://…` 且**带 query 或 userinfo**（订阅 URL 的凭据就在这两处）。
/// 不带的话交给地址规则处理（`https://<节点域名>/` 里的域名照样会被抹）。
fn match_credentialed_url(line: &str, i: usize) -> Option<usize> {
    if !boundary_before(line, i) {
        return None;
    }
    let bytes = line.as_bytes();
    let rest = &bytes[i..];
    let is_http = rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"http://");
    let is_https = rest.len() >= 8 && rest[..8].eq_ignore_ascii_case(b"https://");
    if !is_http && !is_https {
        return None;
    }
    let end = line[i..]
        .find(char::is_whitespace)
        .map(|o| i + o)
        .unwrap_or(line.len());
    let raw = &line[i..end];
    if !raw.contains('?') && !raw.contains('@') {
        return None;
    }
    Some(end - i)
}

/// 公网 IP ⇒ 携带身份、必须抹；返回 `false` 的是**本机管道**地址（保留）。
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast())
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unicast_link_local()
                || (v6.segments()[0] & 0xfe00) == 0xfc00) // fc00::/7 ULA
        }
    }
}

pub struct HttpClientConfig {
    pub timeout: Duration,
    pub user_agent: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use xt_core::model::{NodeSource, Protocol, TlsSettings};

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
            let out = redact_secrets(line, &NodeAddresses::default());
            assert!(!out.contains("b831381d"), "{out}");
            assert!(out.contains("<uuid>"));
        }
        /// **脱敏必须只替换凭据，不能顺手把整个日志的排版揉掉。**
        ///
        /// 真实事故面：Xray 与核心报错经常是多行文本（配置片段、栈、表格），
        /// 而"按空白切分再拼回去"会把换行、缩进、连续空格全部塌成单个空格 ——
        /// 用户点开日志看到的是一大坨，恰好是在排查问题时最需要结构的时候。
        #[test]
        fn redaction_preserves_layout_of_multiline_logs() {
            let line = "启动失败:\n    \"port\": 10808\n    uuid b831381d-6324-4d53-ad4f-8cda48b30811\n";
            let out = redact_secrets(line, &NodeAddresses::default());
            assert!(!out.contains("b831381d"), "凭据没被脱敏: {out}");
            assert!(out.contains('\n'), "换行被吃掉了，多行日志被压成一行: {out:?}");
            assert!(out.contains("    \"port\""), "缩进被吃掉了: {out:?}");
        }
        /// 脱敏是**只读**的：没有任何要抹的东西时必须原样返回，一个字符都不能变。
        ///
        /// ⚠️ task-124 前这条 fixture 用的是 `已连接 45.207.197.185:443 …` ——
        /// 一个**公网 IP**，而它现在正是**必须**抹掉的东西（旧断言等于把缺陷钉成了规格）。
        /// 现在换成真正没有东西可抹的一行，公网 IP 的两种情形另见下面两条测试。
        #[test]
        fn redaction_is_identity_when_nothing_to_hide() {
            let empty = NodeAddresses::default();
            let line = "已连接 用时 54ms";
            assert_eq!(redact_secrets(line, &empty), line);
        }
        /// 边界：整行为空白、行首/行尾都是空白时也必须原样返回。
        #[test]
        fn redaction_handles_pure_whitespace_and_edges() {
            let empty = NodeAddresses::default();
            assert_eq!(redact_secrets("", &empty), "");
            assert_eq!(redact_secrets("   \n\t ", &empty), "   \n\t ");
            assert_eq!(
                redact_secrets("  b831381d-6324-4d53-ad4f-8cda48b30811  ", &empty),
                "  <uuid>  "
            );
            // 恰好 36 字符但不是 UUID 形状 -> 不动
            let not_uuid = "a".repeat(36);
            assert_eq!(redact_secrets(&not_uuid, &empty), not_uuid);
        }

        // -------------------------------------------------------------------
        // task-124：报告的**地址**脱敏（判据来自节点列表，不是猜域名形状）
        // -------------------------------------------------------------------

        /// fixture 节点：地址 / 域名 / SNI / 传输层 host / 名字，五种载体都覆盖。
        fn fixture_node(address: &str, name: &str, server_name: &str, ws_host: &str) -> Node {
            Node {
                id: format!("n-{address}"),
                name: name.to_string(),
                address: address.to_string(),
                port: 443,
                protocol: Protocol::Vless {
                    uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                    flow: String::new(),
                    encryption: "none".into(),
                },
                transport: Transport::WebSocket {
                    path: "/ws".into(),
                    host: ws_host.to_string(),
                },
                tls: TlsSettings {
                    enabled: true,
                    server_name: server_name.to_string(),
                    ..Default::default()
                },
                mux: None,
                source: NodeSource::Manual,
                tags: vec![],
                raw_uri: None,
            }
        }

        /// **主回归**：报告里不许留下节点地址/域名/名字域名/IP、UUID、订阅 URL 凭据；
        /// 但公开目标域名（`www.baidu.com`）与**本机管道**地址
        /// （`127.0.0.2` / `192.168.1.1`）必须原样保留 —— 否则报告不再可排查。
        #[test]
        fn report_redaction_hides_node_addresses_but_keeps_public_targets_and_local_pipes() {
            let nodes = vec![fixture_node(
                "45.207.197.185",
                "🇯🇵 jp1.node-example.xyz",
                "jp1.node-example.xyz",
                "cdn.node-example.net",
            )];
            let addresses = NodeAddresses::from_nodes(&nodes);
            let line = concat!(
                "proxy/vless/outbound: tunneling request to tcp:www.baidu.com:443 via ",
                "45.207.197.185:443\n",
                "transport/internet/tcp: dialing TCP to tcp:jp1.node-example.xyz:443 ",
                "(host cdn.node-example.net)\n",
                "uuid b831381d-6324-4d53-ad4f-8cda48b30811 sub ",
                "https://sub.example.com/api/v1/client/subscribe?token=SECRET123\n",
                "route -n get 127.0.0.2 -> 127.0.0.2; gw 192.168.1.1; peer 1.0.0.1:443\n"
            );
            let out = redact_secrets(line, &addresses);

            // 六类原值一个都不许留下。
            for secret in [
                "45.207.197.185",
                "jp1.node-example.xyz",
                "cdn.node-example.net",
                "b831381d-6324-4d53-ad4f-8cda48b30811",
                "SECRET123",
                "1.0.0.1",
            ] {
                assert!(!out.contains(secret), "原值泄漏：{secret}\n{out}");
            }
            assert!(!out.contains("node-example"), "节点域名不许留任何片段：\n{out}");
            assert!(!out.contains("45.207.197"), "{out}");
            // 公开目标域名与本机管道地址必须还在。
            assert!(out.contains("www.baidu.com"), "{out}");
            assert!(out.contains("127.0.0.2"), "{out}");
            assert!(out.contains("192.168.1.1"), "{out}");
            // 替换形状统一。
            assert!(out.contains("<addr>"), "{out}");
            assert!(out.contains("<uuid>"), "{out}");
            assert!(
                !out.contains("<非法 URL>"),
                "订阅 URL 要按 URL 识别（只抹凭据），不是当成非法 URL：{out}"
            );
        }

        /// 规则 3：**没命中节点列表的本机管道地址必须原样保留** ——
        /// 「回环洞」的判据就是 `route -n get 127.0.0.2` 的 interface，全抹掉等于
        /// 把它变成永久不可诊断；而**没在列表里的公网 IP 必须抹**
        /// （域名节点解析后的 IP、已经被切走的旧节点 IP）。
        #[test]
        fn local_pipe_addresses_are_kept_while_unknown_public_ips_are_redacted() {
            let empty = NodeAddresses::default();
            let kept = concat!(
                "route -n get 127.0.0.2 -> 127.0.0.2; ::1; 10.1.2.3:1080; 172.16.9.9; ",
                "192.168.1.1; 169.254.1.1; fe80::1%en0; fc00::1; 0.0.0.0:10808; [fe80::1]:1080"
            );
            assert_eq!(
                redact_secrets(kept, &empty),
                kept,
                "本机管道地址不许被抹"
            );

            let mixed = redact_secrets(
                "dialing 1.0.0.1:443 and 240e:3b7::1 and [240e:3b7::1]:443",
                &empty,
            );
            assert!(!mixed.contains("1.0.0.1"), "{mixed}");
            assert!(!mixed.contains("240e:3b7::1"), "{mixed}");
            assert_eq!(mixed.matches("<addr>").count(), 3, "{mixed}");
        }

        /// 规则 1 优先于规则 3：**命中节点列表的私网地址也要抹**（自建节点常在 LAN 里）。
        #[test]
        fn node_address_in_the_list_wins_even_when_it_is_a_private_ip() {
            let addresses =
                NodeAddresses::from_nodes(&[fixture_node("192.168.1.50", "", "", "")]);
            let out = redact_secrets("via 192.168.1.50:443 and 127.0.0.2", &addresses);
            assert!(!out.contains("192.168.1.50"), "{out}");
            assert!(out.contains("127.0.0.2"), "没在列表里的本机地址仍要保留：{out}");
        }

        /// 节点域名出现在**更长的主机名里**也要抹：`www.<节点域名>` 与
        /// `<节点域名>.cn` 都含节点域名这段文本；但左边紧贴字母的其他域名不许误伤。
        #[test]
        fn node_domain_is_redacted_inside_longer_hostnames_too() {
            let addresses =
                NodeAddresses::from_nodes(&[fixture_node("1.0.0.1", "", "node-example.xyz", "")]);
            let out = redact_secrets(
                "a www.node-example.xyz and node-example.xyz.cn and xnode-example.xyz",
                &addresses,
            );
            // 两处都带着节点域名这段文本 ⇒ 都必须被替换掉（前缀/后缀都留不住它）。
            assert!(!out.contains("www.node-example.xyz"), "{out}");
            assert!(!out.contains("node-example.xyz.cn"), "{out}");
            assert!(out.contains("www.<addr>"), "{out}");
            assert!(out.contains("<addr>.cn"), "{out}");
            assert!(out.contains("xnode-example.xyz"), "别把相似域名误伤成节点域名：{out}");
        }

        /// **不许把报告抹成没法看**：公开目标域名、版本号、时间戳都要原样保留。
        #[test]
        fn redaction_does_not_touch_public_domains_versions_or_timestamps() {
            let addresses = NodeAddresses::from_nodes(&[fixture_node(
                "45.207.197.185",
                "🇯🇵 jp1.node-example.xyz",
                "jp1.node-example.xyz",
                "",
            )]);
            let line = concat!(
                "2026-09-22 12:29:17.123 [Info] Xray 25.9.11 tunneling to tcp:www.baidu.com:443 ",
                "and tcp:www.google.com:443 via 45.207.197.185:443 (v1.8.24)"
            );
            let out = redact_secrets(line, &addresses);
            assert!(out.contains("www.baidu.com"), "{out}");
            assert!(out.contains("www.google.com"), "{out}");
            assert!(out.contains("25.9.11"), "{out}");
            assert!(out.contains("2026-09-22 12:29:17.123"), "时间戳不许被动：{out}");
            assert!(out.contains("v1.8.24"), "版本号不许被动：{out}");
            assert!(!out.contains("45.207.197.185"), "{out}");
        }

        /// 订阅 URL 的**凭据**必须抹（既有 `redact_url` 口径：只留 scheme + host）；
        /// 而**保留下来的 host 还要再走一遍地址规则** —— 否则
        /// `https://<节点域名>/…` 会把节点域名原样留在报告里。
        #[test]
        fn subscription_url_credentials_are_redacted_and_the_kept_host_too() {
            let addresses =
                NodeAddresses::from_nodes(&[fixture_node("1.0.0.1", "", "node-example.xyz", "")]);
            let out = redact_secrets(
                concat!(
                    "更新失败: https://sub.example.com/sub?token=SECRET123 与 ",
                    "https://node-example.xyz/sub?token=SECRET123"
                ),
                &addresses,
            );
            assert!(!out.contains("SECRET123"), "{out}");
            assert!(
                out.contains("https://sub.example.com/…"),
                "订阅站点的 host 保留（既有口径）：{out}"
            );
            assert!(!out.contains("node-example.xyz"), "URL 里保留下来的 host 也要脱敏：{out}");
        }

        /// `from_nodes` 只收**地址类**的条目：名字里的国旗/「香港 01」这类标签不是地址，
        /// 收了就会到处误伤日志。
        #[test]
        fn node_addresses_collects_addresses_from_the_node_list_only() {
            let addresses = NodeAddresses::from_nodes(&[fixture_node(
                "45.207.197.185",
                "🇭🇰 HK-01 香港",
                "sni.example.net",
                "cdn.example.org",
            )]);
            let out = redact_secrets(
                "45.207.197.185 sni.example.net cdn.example.org HK-01 香港 www.baidu.com",
                &addresses,
            );
            assert!(!out.contains("45.207.197.185"), "{out}");
            assert!(!out.contains("sni.example.net"), "{out}");
            assert!(!out.contains("cdn.example.org"), "{out}");
            assert!(out.contains("HK-01"), "节点名里的标签不是地址，不该抹：{out}");
            assert!(out.contains("香港"), "{out}");
            assert!(out.contains("www.baidu.com"), "{out}");
        }

        // -------------------------------------------------------------------
        // task-110：读取统计必须**用户可见**，且不许把「有丢失」说成「正常」
        // -------------------------------------------------------------------

        fn stats_with_loss() -> xt_core::store::TailLogStats {
            xt_core::store::TailLogStats {
                files_read: 2,
                lines: 100,
                records: 104,
                multi_object_lines: 4,
                malformed_lines: 2,
                bad_lines: vec!["app.jsonl:12".into(), "app.1.jsonl:88".into()],
                ..Default::default()
            }
        }

        /// 没有丢失时：报告那行必须**明说 0 行无法解析**（而不是含糊不提）。
        #[test]
        fn read_stats_line_reports_zero_loss_explicitly() {
            let stats = xt_core::store::TailLogStats {
                files_read: 2,
                lines: 441_032,
                records: 441_036,
                multi_object_lines: 4,
                truncated: 441_036 - 50,
                ..Default::default()
            };
            let line = read_stats_line(&stats, 50);
            assert!(line.starts_with("日志读取统计："), "{line}");
            assert!(line.contains("0 行无法解析"), "没有丢失也要明说 0：{line}");
            assert!(line.contains("441036") || line.contains("441,036"), "数值要带出来：{line}");
            assert!(line.contains("只列出最近 50 条"), "{line}");
            assert!(line.contains("内容完整"), "没丢东西时要敢说「内容完整」：{line}");
            assert!(
                !line.contains("截断"),
                "「只显示最近 N 条」不许写成「截断」（真实日志上那是 35 万量级，会吓人也误导）：{line}"
            );
            assert!(!line.contains("**"), "没有丢失时不该有加粗告警：{line}");

            // 没被显示上限砍过时（读取量 ≤ limit）不许说「只列出最近 N 条」。
            let small = xt_core::store::TailLogStats { files_read: 1, lines: 3, records: 3, ..Default::default() };
            let line = read_stats_line(&small, 3);
            assert!(line.contains("全部列出（3 条）"), "{line}");
        }

        /// **有丢失时必须写成丢失** —— 这是本卡的敏感性核心：
        /// 把统计换成恒为「0 / 无丢失」的假值 ⇒ 本测试红。
        #[test]
        fn read_stats_line_and_warning_do_not_hide_the_loss() {
            let stats = stats_with_loss();
            let line = read_stats_line(&stats, 3);
            assert!(line.contains("2 行无法解析"), "必须报出坏行数：{line}");
            assert!(line.contains("app.jsonl:12"), "必须报出可定位的样本：{line}");
            assert!(line.contains("读不出来"), "要说清后果，而不是只给数字：{line}");

            let warning = loss_warning(&stats).expect("有丢失就必须有提醒");
            assert!(warning.contains("这不是正常情况"), "不许把它说成正常：{warning}");
            assert!(warning.contains("2 行无法解析"), "{warning}");
            assert!(warning.contains("app.1.jsonl:88"), "样本要跟上：{warning}");

            // 没有丢失时**不许**凭空生成提醒（狼来了会让人忽略真的告警）。
            assert!(loss_warning(&Default::default()).is_none());
        }

        /// 提醒去重：同一个签名只提醒一次；签名变化（新的坏行）要再提醒。
        #[test]
        fn loss_notify_fires_once_per_signature() {
            let mut n = LossNotify::default();
            let s1 = loss_signature(&stats_with_loss());
            assert!(n.mark(s1.clone()), "第一次要提醒");
            assert!(!n.mark(s1.clone()), "同一个问题不许每次刷新都写一遍（会把日志刷爆）");
            let mut other = stats_with_loss();
            other.bad_lines.push("app.jsonl:900".into());
            assert!(
                n.mark(loss_signature(&other)),
                "出现**新的**坏行（签名变了）要再提醒一次"
            );
        }

        // -------------------------------------------------------------------
        // task-108：报告必须**自锚定**（带时间），启动来源必须**自证**（版本+触发者）
        // -------------------------------------------------------------------

        /// UTC ISO 换算：含纪元、闰日、整百年（2100 不是闰年）三个边界。
        #[test]
        fn utc_iso_converts_known_timestamps() {
            assert_eq!(utc_iso(0), "1970-01-01T00:00:00Z");
            // 本机日志里那三次自愈（本地 12:29:17 / 13:22:14 / 13:27:38 = UTC+8）
            assert_eq!(utc_iso(1790051357), "2026-09-22T04:29:17Z");
            assert_eq!(utc_iso(1790054534), "2026-09-22T05:22:14Z");
            assert_eq!(utc_iso(1790054858), "2026-09-22T05:27:38Z");
            assert_eq!(utc_iso(951782400), "2000-02-29T00:00:00Z", "闰日");
            assert_eq!(utc_iso(4102444800), "2100-01-01T00:00:00Z", "整百年");
        }

        /// 报告里那一行**必须带时间**（这是本卡「自锚定」的验收点）。
        #[test]
        fn report_log_line_carries_a_time() {
            let entry = crate::state::LogEntry {
                ts_unix: 1790051357,
                source: "app".into(),
                level: "info".into(),
                message: "隧道已自动恢复（第 1 次自动重建）".into(),
            };
            let line = report_log_line(&entry, &NodeAddresses::default());
            assert!(
                line.starts_with("[2026-09-22T04:29:17Z] [app] info "),
                "报告里的日志行必须带时间（以前没有）：{line}"
            );
            assert!(line.contains("隧道已自动恢复"), "{line}");
        }

        /// 报告里的**日志行**也走同一套地址脱敏（不能只处理报告头部），
        /// 且自锚定的封套（时间/source/level）**一个字都不动**。
        #[test]
        fn report_log_line_redacts_node_addresses_and_keeps_the_envelope() {
            let addresses =
                NodeAddresses::from_nodes(&[fixture_node("45.207.197.185", "", "", "")]);
            let entry = crate::state::LogEntry {
                ts_unix: 1790051357,
                source: "core".into(),
                level: "info".into(),
                message: "tunneling request to tcp:www.baidu.com:443 via 45.207.197.185:443"
                    .into(),
            };
            let line = report_log_line(&entry, &addresses);
            assert!(!line.contains("45.207.197.185"), "{line}");
            assert!(line.contains("www.baidu.com"), "{line}");
            assert!(
                line.starts_with("[2026-09-22T04:29:17Z] [core] info "),
                "封套是自锚定信息，不许动：{line}"
            );
        }

        /// **手动证据工具**（`#[ignore]`，不进 CI）：在**真实日志**上渲染报告会写出的那
        /// 50 行，用来做「改前 / 改后**同一段真实报告**」的对照。
        ///
        /// 输入是 `ts\0source\0level\0message\0` 重复的二进制文件（由 `python3` 从真实
        /// `app.jsonl` 尾部 50 条记录导出）+ 真实 `nodes.json`：
        ///
        /// ```text
        /// XT_T124_NUL=/tmp/t124-tail50.nul \
        /// XT_T124_NODES="$HOME/Library/Application Support/com.xraytun.desktop/nodes.json" \
        ///   cargo test -p xraytun-desktop --lib redacts_the_real_report_tail -- --ignored --nocapture
        /// ```
        #[test]
        #[ignore = "手动证据工具：需要 XT_T124_NUL / XT_T124_NODES 指向真实数据"]
        fn redacts_the_real_report_tail() {
            let (Ok(nul), Ok(nodes_path)) =
                (std::env::var("XT_T124_NUL"), std::env::var("XT_T124_NODES"))
            else {
                eprintln!("跳过：请设置 XT_T124_NUL 与 XT_T124_NODES");
                return;
            };
            let raw = std::fs::read(&nul).expect("读 XT_T124_NUL");
            let fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
            let nodes: Vec<Node> = serde_json::from_str(
                &std::fs::read_to_string(&nodes_path).expect("读 nodes.json"),
            )
            .expect("解析 nodes.json");
            let addresses = NodeAddresses::from_nodes(&nodes);
            println!("节点 {} 个，地址候选 {} 条", nodes.len(), addresses.entries.len());
            for chunk in fields.chunks(4) {
                if chunk.len() < 4 {
                    break;
                }
                let entry = crate::state::LogEntry {
                    ts_unix: String::from_utf8_lossy(chunk[0]).parse().unwrap_or(0),
                    source: String::from_utf8_lossy(chunk[1]).to_string(),
                    level: String::from_utf8_lossy(chunk[2]).to_string(),
                    message: String::from_utf8_lossy(chunk[3]).to_string(),
                };
                print!("{}", report_log_line(&entry, &addresses));
            }
        }
}
