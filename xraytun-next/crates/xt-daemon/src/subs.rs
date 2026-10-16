//! 订阅：远端拉取（`/usr/bin/curl`）+ 本地持久化 + 视图。
//!
//! # 为什么用 `/usr/bin/curl` 而不是引入 HTTP 客户端库
//!
//! 这是老仓库 v0.9.2 对**同一个功能**留下的结论，原文就写在它 `nodes.rs` 的
//! `fetch_subscription` 上：
//!
//! > 用 `curl` 而不是引入 HTTP 客户端库：macOS 自带 `/usr/bin/curl`，支持 HTTPS
//! > （走系统信任链）、gzip、重定向，且零依赖。代价是不能复用连接 —— 对「一天更新
//! > 几次订阅」这个频率完全无所谓。
//!
//! 按同一把尺子量：拉订阅是**低频、一次性、要跟随重定向、要跟随系统证书**的请求。
//! 为它拉进 reqwest/rustls 那一棵树（几十个 crate、CI 编译时间、审计面）不划算；
//! 而 `/usr/bin/curl` 在两平台上都在，并且**绝对路径 + argv**（永不经过 shell）。
//!
//! # 安全口径（订阅 URL 里通常带账号令牌）
//!
//! * **日志/错误里只出现 `scheme://host`**（[`redact_url`]，含**剥掉 userinfo**：
//!   `https://user:token@host/x` 只许出现 `https://host/…`）：完整 URL 会进本地状态文件
//!   与界面，但绝不进日志、绝不进错误消息 —— 老仓库同一条规矩。
//! * 状态文件（含完整 URL）落盘权限 **0600**，且只允许 `http(s)`。
//! * 限制重定向协议（`--proto-redir`），避免被一个 302 引到 `file://` 之类的地方。
//! * 正文大小有**两道**限制：`--max-filesize`（源站给了 `Content-Length` 时 curl 自己
//!   中止）+ 落盘前的尺寸复查；再加 `--max-time 20` 兜住「慢慢喂」。**已知的诚实边界**：
//!   如果源站用 chunked 且不给长度，curl 仍会把正文读进内存，所以这不是一个
//!   「内存上限」的证明 —— 对「用户自己填的订阅 URL」这个威胁模型够用，故不为此引入
//!   一套自行分帧的读取代码。
//!
//! # 没有联网的测试怎么办
//!
//! `fetch_body` 本身要有网络才有意义，所以它在单元测试里只测「参数拼装」那一层；
//! 真正的「拉下来 → 解析 → 进目录」由 `tests/e2e_real_xray.rs` 里对着**本地
//! HTTP 服务**跑（CI 上就能过，不需要外网）。

use std::io::Write;
use std::path::Path;

use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{SubscriptionId, SubscriptionView};

use crate::StoredNode;

/// 绝对路径：不依赖 PATH（GUI 启动的进程 PATH 与我们以为的不一样）。
const CURL_BIN: &str = "/usr/bin/curl";
/// 单次拉取的失败上限（curl `--max-time`）。
const FETCH_TIMEOUT_SECS: u64 = 20;
/// 正文上限：订阅是文本，5 MiB 已经远超任何正常机场。
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// User-Agent：机场按它区分客户端，写清楚是哪个程序在拉。
const USER_AGENT: &str = concat!("XrayTunNext/", env!("CARGO_PKG_VERSION"));

/// 一条订阅的完整记录（磁盘格式就是这个）。
///
/// `url` 含令牌 ⇒ 这个结构**只允许**出现在状态文件、内存与 `SubscriptionView` 里；
/// 任何日志/错误都要先过 [`redact_url`]。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SubscriptionRecord {
    pub(crate) id: String,
    /// 用户填的原始 URL（含令牌）。
    pub(crate) url: String,
    /// 最近一次**成功**拉取的时刻；从没成功过就是 `None`。
    #[serde(default)]
    pub(crate) fetched_at_ms: Option<u64>,
    /// 最近一次拉取到的节点。**落盘**：重启后不该依赖网络才能有节点。
    #[serde(default)]
    pub(crate) nodes: Vec<StoredNode>,
    /// 最近一次拉取的失败原因（成功即清空）。
    #[serde(default)]
    pub(crate) last_error: Option<ErrorBody>,
}

/// 读状态文件。读不到 = 没有订阅（首次运行是正常状态）；损坏 = 空 + error 日志。
pub(crate) async fn load(path: &Path) -> Vec<SubscriptionRecord> {
    match tokio::fs::read(path).await {
        Ok(bytes) => match serde_json::from_slice::<Vec<SubscriptionRecord>>(&bytes) {
            Ok(records) => records,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    path = %path.display(),
                    "订阅状态文件损坏，本次按「没有订阅」处理（界面会显示空，重新添加即可覆盖）"
                );
                Vec::new()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            tracing::error!(error = %error, path = %path.display(), "读取订阅状态文件失败");
            Vec::new()
        }
    }
}

/// 原子落盘，权限 **0600**（文件里有完整 URL，通常带账号令牌）。
///
/// 用同步 IO 而不是 tokio：这是一个几 KiB 的小文件、一天写几次，
/// 没必要为它引入一套异步文件句柄（也要不到 `mode(0o600)` 这个语义）。
pub(crate) fn save(path: &Path, records: &[SubscriptionRecord]) -> Result<(), ErrorBody> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            ErrorBody::new(ErrorCode::Io, format!("创建状态目录 {} 失败: {error}", dir.display()))
        })?;
    }
    let body = serde_json::to_vec_pretty(records)
        .map_err(|error| ErrorBody::new(ErrorCode::Internal, format!("序列化订阅失败: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|error| {
                ErrorBody::new(ErrorCode::Io, format!("写入 {} 失败: {error}", tmp.display()))
            })?;
        file.write_all(&body).map_err(|error| {
            ErrorBody::new(ErrorCode::Io, format!("写入 {} 失败: {error}", tmp.display()))
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        ErrorBody::new(ErrorCode::Io, format!("原子替换 {} 失败: {error}", path.display()))
    })?;
    Ok(())
}

/// 下一个订阅 id：`sub-<n>`，取现有最大编号 + 1（重启后仍然稳定）。
pub(crate) fn next_id(records: &[SubscriptionRecord]) -> String {
    let mut max = 0u32;
    for record in records {
        let number = record.id.strip_prefix("sub-").and_then(|rest| rest.parse::<u32>().ok());
        if let Some(number) = number {
            max = max.max(number);
        }
    }
    format!("sub-{}", max + 1)
}

/// 磁盘记录 → 界面视图（`node_count` 取自真实节点数，不另存一份计数）。
pub(crate) fn view(record: &SubscriptionRecord) -> SubscriptionView {
    SubscriptionView {
        id: SubscriptionId::new(record.id.clone()),
        url: record.url.clone(),
        node_count: record.nodes.len() as u32,
        fetched_at_ms: record.fetched_at_ms,
        last_error: record.last_error.clone(),
    }
}

/// 校验用户填的 URL：只允许 `http://` / `https://`，且必须有主机名。
///
/// 不引入 `url` crate：这里要判的规则只有这两条，手写比多一个依赖清楚。
pub(crate) fn validate_url(raw: &str) -> Result<String, ErrorBody> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(bad_request("订阅 URL 为空"));
    }
    let rest = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        rest
    } else {
        return Err(bad_request("订阅 URL 必须以 http:// 或 https:// 开头"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(bad_request("订阅 URL 缺少主机名"));
    }
    if authority.chars().any(char::is_whitespace) {
        return Err(bad_request("订阅 URL 的主机名里有空白字符"));
    }
    Ok(trimmed.to_string())
}

/// 把 URL 压成 `scheme://host/…`。**日志与错误里只许出现这个形状。**
pub(crate) fn redact_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            // authority 里可能带 userinfo（`user:token@host`）。`validate_url` 允许它，
            // 所以这里必须**剥掉 `@` 之前的部分** —— 否则「只记 host」这句承诺
            // 恰恰在带 userinfo 的 URL 上漏掉令牌。
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            if host.is_empty() {
                format!("{scheme}://…")
            } else {
                format!("{scheme}://{host}/…")
            }
        }
        None => "订阅 URL".to_string(),
    }
}

/// 把一段可能包含完整 URL 的文本里的 URL 换成脱敏形状（curl 的 stderr 会带 URL）。
fn redact_text(text: &str, url: &str) -> String {
    if url.is_empty() {
        return text.to_string();
    }
    text.replace(url, &redact_url(url))
}

/// curl 的退出码 → 一句「下一步该怀疑什么」。只处理**网络类**退出码，
/// 其余一律返回空字符串（不多嘴）：HTTP 4xx/5xx 归 `--fail` 的 22，那是「地址/订阅本身」的问题，
/// 不该被说成网络问题。
///
/// 判据是 curl 定义好的退出码，不是猜测；文案里也如实写明这是**推断**，
/// 并且只带脱敏后的 `scheme://host/…`（URL 里可能带令牌）。
fn network_hint(code: Option<i32>, url: &str) -> String {
    let what = match code {
        Some(6) => "解析不了订阅地址里的主机名",
        Some(7) => "连不上订阅服务器",
        Some(28) => "连接订阅服务器超时",
        Some(35) => "与订阅服务器建立 TLS 连接失败",
        _ => return String::new(),
    };
    format!(
        "（推断：{what} —— 先确认这台机器能打开别的网站，再确认订阅地址还有效；地址 {}）",
        redact_url(url)
    )
}

/// 拉取订阅正文。成功返回正文；任何失败都带**脱敏后**的原因。
pub(crate) async fn fetch_body(url: &str) -> Result<String, ErrorBody> {
    // 三个数字参数都先转成 `&str`：让 `.args([...])` 的元素类型**统一**是 `&str`，
    // 不依赖「`&String` 在数组字面量里被强制成 `&str`」这条容易读错的规则。
    let max_time = FETCH_TIMEOUT_SECS.to_string();
    let max_filesize = MAX_BODY_BYTES.to_string();
    let output = tokio::process::Command::new(CURL_BIN)
        .args([
            "--silent",
            "--show-error",
            // HTTP 4xx/5xx 必须以**失败**收场：否则错误页会被送去解析，
            // 用户看到的失败原因就成了「不是合法订阅」——把真正的病（HTTP 状态）
            // 归因错了地方。`--fail` 兼容性最好（`--fail-with-body` 要 curl ≥ 7.76）。
            "--fail",
            // 跟随重定向，但只允许 http(s)——防止被 302 引到 file:// 之类的地方。
            "--location",
            "--proto",
            "=https,http",
            "--proto-redir",
            "=https,http",
            // 机场普遍用 gzip 压订阅正文。
            "--compressed",
            "--max-time",
            max_time.as_str(),
            // 有 Content-Length 时让 curl 自己中止（chunked 的情况见模块头「已知边界」）。
            "--max-filesize",
            max_filesize.as_str(),
            "--user-agent",
            USER_AGENT,
            url,
        ])
        .output()
        .await
        .map_err(|error| {
            ErrorBody::new(ErrorCode::Io, format!("调用 {CURL_BIN} 失败：{error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let reason = if stderr.is_empty() {
            format!("curl 退出码 {:?}", output.status.code())
        } else {
            stderr.to_string()
        };
        return Err(ErrorBody::new(
            ErrorCode::Io,
            // 原文（curl 自己说的话）一字不改，后面只加一句**标明是推断**的提示：
            // 「连不上」与「地址写错了」对用户是完全不同的下一步，而 curl 的退出码
            // 足以区分这两类（见 `network_hint`）。
            format!("拉取订阅失败：{}{}", redact_text(&reason, url), network_hint(output.status.code(), url)),
        ));
    }
    if output.stdout.len() > MAX_BODY_BYTES {
        return Err(ErrorBody::new(
            ErrorCode::Io,
            format!("订阅正文超过 {} MiB，拒绝解析", MAX_BODY_BYTES / 1024 / 1024),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| ErrorBody::new(ErrorCode::Io, "订阅正文不是合法 UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, url: &str) -> SubscriptionRecord {
        SubscriptionRecord {
            id: id.to_string(),
            url: url.to_string(),
            fetched_at_ms: None,
            nodes: Vec::new(),
            last_error: None,
        }
    }

    /// 只允许 http(s) 且必须有主机名 —— 这是「不给用户填出奇怪东西」的第一道门。
    #[test]
    fn validate_url_accepts_only_http_and_https_with_a_host() {
        assert_eq!(
            validate_url("  https://example.com/api/v1/client/subscribe?token=abc  ").expect("https"),
            "https://example.com/api/v1/client/subscribe?token=abc"
        );
        assert!(validate_url("http://127.0.0.1:8080/sub").is_ok());
        for bad in ["", "   ", "example.com/sub", "file:///etc/passwd", "https://", "https://a b.com/x"] {
            let error = validate_url(bad).expect_err("必须拒绝：{bad}");
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{bad} → {error:?}");
        }
    }

    /// 日志/错误里只许出现 host：token 在 path/query 里。
    #[test]
    fn redact_url_hides_path_and_query() {
        assert_eq!(
            redact_url("https://example.com/api/v1/client/subscribe?token=SECRET"),
            "https://example.com/…"
        );
        assert_eq!(redact_url("http://127.0.0.1:8080/x?t=1"), "http://127.0.0.1:8080/…");
        assert_eq!(redact_url("不是 URL"), "订阅 URL");
        // 关键判据：脱敏结果里不能留下 token。
        assert!(!redact_url("https://a.com/p?token=SECRET").contains("SECRET"));
        assert!(!redact_text(
            "curl: (22) https://a.com/p?token=SECRET 返回 403",
            "https://a.com/p?token=SECRET"
        )
        .contains("SECRET"));
    }

    /// 网络类退出码给一句提示（带脱敏 host），HTTP 类与未知码不多嘴。
    #[test]
    fn network_hint_only_speaks_for_network_exit_codes() {
        let url = "https://user:token@example.com/sub";
        let hint = network_hint(Some(7), url);
        assert!(hint.contains("连不上订阅服务器"), "{hint}");
        assert!(hint.contains("https://example.com/…"), "{hint}");
        assert!(!hint.contains("token"), "提示里不许出现令牌：{hint}");
        assert!(network_hint(Some(28), url).contains("超时"));
        assert!(network_hint(Some(6), url).contains("主机名"));
        // 22 = HTTP 4xx/5xx（`--fail`）：那是地址/订阅本身的问题，不许说成网络问题。
        assert!(network_hint(Some(22), url).is_empty());
        // 退出码缺失（例如被信号杀掉）：不知道就不多说。
        assert!(network_hint(None, url).is_empty());
    }

    /// userinfo 也要剥掉：`validate_url` 允许 `https://user:token@host/…`，
    /// 而日志/错误里只许出现 host（否则「只记 host」就是句空话）。
    #[test]
    fn redact_url_strips_userinfo_and_never_leaks_a_token() {
        assert_eq!(redact_url("https://user:token@example.com/sub"), "https://example.com/…");
        assert!(!redact_url("https://user:token@example.com/sub").contains("token"));
        assert!(!redact_text(
            "curl: (22) https://user:token@example.com/sub 返回 404",
            "https://user:token@example.com/sub"
        )
        .contains("token"));
        // 没有 userinfo 时行为不变。
        assert_eq!(redact_url("https://example.com/sub"), "https://example.com/…");
    }

    /// `validate_url` 允许 userinfo（用户可能真在用这种写法），
    /// 但**完整 URL 只允许进状态文件/界面**：这条测试把两件事钉在一起。
    #[test]
    fn validate_url_keeps_userinfo_but_redaction_hides_it() {
        let url = validate_url("https://user:token@example.com/sub").expect("带 userinfo 也合法");
        assert!(url.contains("token"), "存下来的是用户填的原文：{url}");
        assert!(!redact_url(&url).contains("token"), "但日志里不许出现：{}", redact_url(&url));
    }

    #[test]
    fn next_id_keeps_numbering_stable_across_restarts() {
        assert_eq!(next_id(&[]), "sub-1");
        let records = vec![record("sub-1", "https://a.com"), record("sub-3", "https://b.com")];
        assert_eq!(next_id(&records), "sub-4");
        // 本地订阅文件那条（id = local）不参与编号。
        let with_local = vec![record("local", "/tmp/subscription.txt")];
        assert_eq!(next_id(&with_local), "sub-1");
    }

    #[test]
    fn view_reports_the_real_node_count() {
        let mut one = record("sub-1", "https://a.com");
        one.nodes.push(StoredNode {
            id: "n1".into(),
            name: "n1".into(),
            protocol: "vless".into(),
            endpoint: "a.example.com:443".into(),
            outbound: serde_json::json!({ "protocol": "vless" }),
        });
        let view = view(&one);
        assert_eq!(view.node_count, 1);
        assert_eq!(view.id.as_str(), "sub-1");
        assert!(view.last_error.is_none());
    }

    /// 落盘 + 读回；并且**权限必须是 0600**（文件里有完整 URL，通常带令牌）。
    #[tokio::test]
    async fn save_and_load_round_trip_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("xt-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("subscriptions.json");

        let records = vec![record("sub-1", "https://example.com/sub?token=SECRET")];
        save(&path, &records).expect("落盘");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "订阅状态文件必须是 0600（含令牌）");

        let back = load(&path).await;
        assert_eq!(back, records, "读回必须与写入一致");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
