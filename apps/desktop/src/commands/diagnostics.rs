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
    let from_file: Vec<crate::state::LogEntry> = tauri::async_runtime::spawn_blocking(move || {
        xt_core::store::Store::new(root).tail_logs(limit)
    })
    .await
    .unwrap_or_default();
    if !from_file.is_empty() {
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
    let recent: Vec<crate::state::LogEntry> = state.store.tail_logs(50);
    let recent = if recent.is_empty() {
        state.with(|i| i.logs.iter().rev().take(50).cloned().collect::<Vec<_>>()).unwrap_or_default()
    } else {
        recent
    };
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
}
