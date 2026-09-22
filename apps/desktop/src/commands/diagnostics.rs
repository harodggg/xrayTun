//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

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
/// 刻意**不包含**订阅 URL、节点地址、UUID/password 等敏感信息 —— 用户会把它
/// 贴到公开的 issue 里，所以在生成端就把它们抹掉，而不是指望用户自己删。
#[tauri::command]
pub async fn diagnostics(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let snap = snapshot::build_snapshot(&app, &state).await?;
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

/// 抹掉 URL 里的凭据部分，只保留 host。
///
/// 机场订阅的 URL 里带 token，用户把日志贴出来就等于把订阅泄漏了。
pub(crate) fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}://{}/…", u.scheme(), u.host_str().unwrap_or("<unknown>")),
        Err(_) => "<非法 URL>".to_string(),
    }
}

/// 日志里可能出现的凭据特征串（UUID）做粗粒度脱敏。
///
/// **只替换凭据本身，其余字节原样保留。**
/// 早先的写法是 `split_whitespace().join(" ")`，它会把换行、缩进和连续空格
/// 一起塌成单个空格 —— 于是多行报错（配置片段、栈、对齐过的表格）在界面上
/// 变成一大坨，恰好毁掉排查问题时最需要的结构。没有凭据时也不再改写文本。
pub(crate) fn redact_secrets(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut at = 0usize;
    for (idx, ch) in line.char_indices() {
        if !ch.is_whitespace() {
            continue;
        }
        // 一段非空白 token 在 [at, idx) 之间（可能为空，例如行首缩进）
        if idx > at {
            let token = &line[at..idx];
            out.push_str(if util::is_uuid_like(token) { "<uuid>" } else { token });
        }
        out.push(ch); // 空白原样保留：换行、缩进、连续空格都不动
        at = idx + ch.len_utf8();
    }
    // 收尾：最后一段 token 后面不一定有空白
    if at < line.len() {
        let token = &line[at..];
        out.push_str(if util::is_uuid_like(token) { "<uuid>" } else { token });
    }
    out
}

pub struct HttpClientConfig {
    pub timeout: Duration,
    pub user_agent: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        /// **脱敏必须只替换凭据，不能顺手把整个日志的排版揉掉。**
        ///
        /// 真实事故面：Xray 与核心报错经常是多行文本（配置片段、栈、表格），
        /// 而"按空白切分再拼回去"会把换行、缩进、连续空格全部塌成单个空格 ——
        /// 用户点开日志看到的是一大坨，恰好是在排查问题时最需要结构的时候。
        #[test]
        fn redaction_preserves_layout_of_multiline_logs() {
            let line = "启动失败:\n    \"port\": 10808\n    uuid b831381d-6324-4d53-ad4f-8cda48b30811\n";
            let out = redact_secrets(line);
            assert!(!out.contains("b831381d"), "凭据没被脱敏: {out}");
            assert!(out.contains('\n'), "换行被吃掉了，多行日志被压成一行: {out:?}");
            assert!(out.contains("    \"port\""), "缩进被吃掉了: {out:?}");
        }
        /// 脱敏是**只读**的：没有任何凭据特征时必须原样返回，一个字符都不能变。
        #[test]
        fn redaction_is_identity_when_nothing_to_hide() {
            let line = "已连接 45.207.197.185:443   用时 54ms";
            assert_eq!(redact_secrets(line), line);
        }
        /// 边界：整行为空白、行首/行尾都是空白时也必须原样返回。
        #[test]
        fn redaction_handles_pure_whitespace_and_edges() {
            assert_eq!(redact_secrets(""), "");
            assert_eq!(redact_secrets("   \n\t "), "   \n\t ");
            assert_eq!(
                redact_secrets("  b831381d-6324-4d53-ad4f-8cda48b30811  "),
                "  <uuid>  "
            );
            // 恰好 36 字符但不是 UUID 形状 -> 不动
            let not_uuid = "a".repeat(36);
            assert_eq!(redact_secrets(&not_uuid), not_uuid);
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
}
