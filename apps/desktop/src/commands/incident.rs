//! 「报告问题」链路的后端半边（task-130）。
//!
//! # 它把三件事接起来
//!
//! 1. **出包**：复用 `scripts/incident-bundle.sh`（脱敏/自锚定/截断口径都在它里面，
//!    已经被 `task-113` 的 fixture 与 `task-116 --privacy-check` 验证过）——
//!    App 里**不再实现一遍**，只负责「找到随包脚本、跑它、把摘要读回来」；
//! 2. **上传**：`POST https://xraytun.top/api/incident`（body = zip 原始字节）。
//!    走 `/usr/bin/curl`，与 `xt_core::update` 同一口径（macOS 自带、一次满足超时/
//!    重定向/代理，详见那边的模块头注释）。**上传只由用户点击触发**，而且**先过本地
//!    隐私闸**（`triage-incident.py --privacy-check`，fail closed）；
//! 3. **被动哨兵**：`logs/anomalies.jsonl`，只写本地、不联网。五类信号见
//!    [`ANOMALY_KINDS`]。
//!
//! # 契约的最终形状（Lead 在 task-130 里裁决过，与卡文最初的草案不同）
//!
//! * **只注册 3 个命令**：`incident_preview` / `incident_upload` /
//!   `incident_anomaly_count`。前端**刻意**没有封装 `incident_anomalies`
//!   （角标只用计数），注册它会让 `type_contract` 的双向相等当场红；
//! * `manifest` 是 **`String`**（包内 `manifest.json` 的 pretty-print 文本）——
//!   前端按「原始文本，供人眼核对」实现；
//! * `hits` 是**结构化** `{file, line, kind}`（端点返回什么就映射什么），
//!   前端的 `normHits()` 只接受这种形状；**任何情况下都不回显密钥原文**。

use super::*;

use std::io::Write as _;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// 上传端点（`task-114` 已部署并端到端跑通）。
pub(crate) const INCIDENT_URL: &str = "https://xraytun.top/api/incident";
/// 被动哨兵文件名（放在数据目录的 `logs/` 下）。
pub(crate) const ANOMALY_FILE: &str = "anomalies.jsonl";
/// 现场包的临时目录名（`incident_upload` 只接受这里面的包）。
pub(crate) const TEMP_SUBDIR: &str = "xraytun-incident";
/// 打包脚本放在 zip 的哪个临时目录下，以及上传前本地闸的名字。
const BUNDLE_SCRIPT: &str = "incident-bundle.sh";
const TRIAGE_SCRIPT: &str = "triage-incident.py";

/// 六类被动哨兵信号（`kind` 字段的取值）。
///
/// **不许**在这里加值而不在调用点记录它：`every_anomaly_kind_is_recorded_in_production_source`
/// 会按生产源码逐个计数（删掉任意一处调用就红）。
// 只有测试会读这个常量（生产是各处直接写字面量）；按本仓既有做法标成
// 「非测试构建下允许 dead_code」，而不是为了消警告把它塞给生产代码。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ANOMALY_KINDS: [&str; 6] = [
    "probe_round_failed",
    "rebuild_failed",
    "watchdog_invalidated",
    "log_read_loss",
    "self_healed",
    // task-176：接管后缺「作用域默认路由」⇒ 绑该网卡的直连会 ENETUNREACH（task-172 的形态）
    "route_audit",
];

// ---------------------------------------------------------------------------
// 契约类型
// ---------------------------------------------------------------------------

/// 包内一个文件（与脚本 `manifest.json` 的 `files` 同源）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

/// `incident_preview()` 的结果：**上传之前**要给用户看的东西全在这里。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncidentPreview {
    /// 本地包的绝对路径；`incident_upload` 要的就是它。
    pub bundle_path: String,
    pub size_bytes: u64,
    pub files: Vec<IncidentFile>,
    /// 脱敏说明（界面用 `pre-wrap` 原样显示）。
    pub readme: String,
    /// 包内 `manifest.json` 的 pretty-print 文本（自锚定：版本/时间/mode/日志级别）。
    pub manifest: String,
    /// 因为太大/太多而**被截断**的文件名 —— 必须说出来，不能悄悄少给。
    pub truncated: Vec<String>,
}

/// `incident_upload()` 成功的结果。`received_at` 是 **ISO 8601 字符串**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentUpload {
    pub id: String,
    pub sha256: String,
    pub bytes: u64,
    pub received_at: String,
}

/// 一条命中：**只有位置与类型，没有密钥原文**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretHit {
    pub file: String,
    pub line: Option<u64>,
    pub kind: String,
}

/// 上传失败的四类（形状冻结；前端 `parseIncidentFailure` 按 `kind` 分派）。
///
/// ⚠️ `Server { code: 0 }` 表示**本地拒绝**（路径不是本机刚生成的包、本地隐私闸
/// 跑不起来）——不是服务端返回的。枚举是冻结的，没有为「本地拒绝」单开变体，
/// 所以用 `code: 0` 把两种「服务端」明确区分开。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncidentUploadError {
    SecretDetected { message: String, hits: Vec<SecretHit> },
    RateLimited { message: String },
    Network { message: String },
    Server { code: u16, message: String },
}

/// 待上报的一条异常（哨兵文件的每行一个 JSON 对象）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anomaly {
    pub ts_unix: u64,
    pub kind: String,
    pub level: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 子进程边界（可注入 ⇒ 单测不真打线上、也不依赖真实脚本）
// ---------------------------------------------------------------------------

/// 一次子进程的结果（不用 `std::process::Output`：`ExitStatus` 没法在测试里造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CmdOutput {
    /// 退出码（拿不到时 `-1`）。
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 跑命令的边界。生产是 [`RealRunner`]；单测注入替身。
pub(crate) trait CmdRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CmdOutput, String>;
}

pub(crate) struct RealRunner;

impl CmdRunner for RealRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CmdOutput, String> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("执行 {} 失败：{e}", program.display()))?;
        Ok(CmdOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }
}

/// 随包携带的三个脚本（Tauri resources 的 `scripts/` 目录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncidentTools {
    pub python: PathBuf,
    pub bundle: PathBuf,
    pub triage: PathBuf,
}

impl IncidentTools {
    /// 资源目录 → 脚本路径。`dir` 是 Tauri 的 `resource_dir()`。
    pub(crate) fn from_resource_dir(dir: &Path) -> Self {
        let scripts = dir.join("scripts");
        Self {
            python: PathBuf::from("/usr/bin/python3"),
            bundle: scripts.join(BUNDLE_SCRIPT),
            triage: scripts.join(TRIAGE_SCRIPT),
        }
    }

    /// 缺哪些文件（**缺了就不许假装成功**）。
    pub(crate) fn missing(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in [&self.bundle, &self.triage] {
            if !p.is_file() {
                out.push(p.display().to_string());
            }
        }
        if !self.python.is_file() {
            out.push(self.python.display().to_string());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// 出包的前置闸（**纯函数 ⇒ 可测**；命令层拿到资源目录后立刻调用）
// ---------------------------------------------------------------------------

/// **出包前**的脚本存在性闸：缺一个就明确报错 ——
/// 「包已生成」这种结论不能靠猜（task-130 的「不许假装成功」）。
pub(crate) fn preview_tools_gate(tools: &IncidentTools) -> Result<(), String> {
    let missing = tools.missing();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "随包缺少脚本：{} —— 现场包无法生成（这不是「包已生成」）。",
            missing.join("、")
        ))
    }
}

/// **上传前**的脚本存在性闸：同名同义，错误类型是上传契约里的那一种
/// （`Server { code: 0 }` = **本地拒绝**，不是服务端返回的）。
pub(crate) fn upload_tools_gate(tools: &IncidentTools) -> Result<(), IncidentUploadError> {
    let missing = tools.missing();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(IncidentUploadError::Server {
            code: 0,
            message: format!("随包缺少脚本：{} ⇒ 无法做上传前复核。", missing.join("、")),
        })
    }
}

// ---------------------------------------------------------------------------
// 出包（复用脚本；不在 Rust 里再解一遍 zip）
// ---------------------------------------------------------------------------

/// 把脚本产出的**摘要 JSON** 变成 [`IncidentPreview`]（纯函数，可测）。
///
/// 摘要由 `incident-bundle.sh --json-out` 写：`{bundle_path, size_bytes, files,
/// readme, manifest, truncated}` —— 口径**只有脚本那一处实现**，这里只做映射，
/// 不在 Rust 里重新扫一遍 zip（两处实现必然分叉）。
pub(crate) fn preview_from_summary(text: &str) -> Result<IncidentPreview, String> {
    #[derive(Deserialize)]
    struct Summary {
        bundle_path: String,
        size_bytes: u64,
        #[serde(default)]
        files: serde_json::Map<String, serde_json::Value>,
        #[serde(default)]
        readme: String,
        manifest: serde_json::Value,
        #[serde(default)]
        truncated: Vec<String>,
    }
    let s: Summary = serde_json::from_str(text).map_err(|e| format!("出包摘要解析失败：{e}"))?;
    let mut files: Vec<IncidentFile> = s
        .files
        .iter()
        .map(|(name, v)| IncidentFile {
            name: name.clone(),
            bytes: v.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
            sha256: v
                .get("sha256")
                .and_then(|x| x.as_str())
                .unwrap_or("<缺>")
                .to_string(),
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(IncidentPreview {
        bundle_path: s.bundle_path,
        size_bytes: s.size_bytes,
        files,
        readme: s.readme,
        manifest: serde_json::to_string_pretty(&s.manifest)
            .unwrap_or_else(|_| s.manifest.to_string()),
        truncated: s.truncated,
    })
}

/// 跑脚本出包 + 读回摘要（`zip`/`json_out` 由调用方给，便于单测注入路径）。
pub(crate) fn preview_with(
    runner: &dyn CmdRunner,
    tools: &IncidentTools,
    zip: &Path,
    summary: &Path,
) -> Result<IncidentPreview, String> {
    let _ = std::fs::remove_file(summary);
    let args: Vec<String> = vec![
        tools.bundle.display().to_string(),
        "--out".into(),
        zip.display().to_string(),
        "--json-out".into(),
        summary.display().to_string(),
    ];
    let out = runner.run(Path::new("/bin/bash"), &args)?;
    if out.code != 0 {
        return Err(format!(
            "出包失败（脚本退出码 {}）：{}\n{}",
            out.code,
            out.stderr.trim(),
            out.stdout.trim()
        ));
    }
    let text = std::fs::read_to_string(summary)
        .map_err(|e| format!("脚本没有写出摘要 {}：{e}", summary.display()))?;
    preview_from_summary(&text)
}

// ---------------------------------------------------------------------------
// 本地隐私闸（fail closed）
// ---------------------------------------------------------------------------

/// 把 `--privacy-json` 的输出解析成命中列表（**只有位置与类型**）。
fn parse_findings(stdout: &str) -> Vec<SecretHit> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(list) = v.get("findings").and_then(|f| f.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|f| {
            let file = f.get("file")?.as_str()?.to_string();
            let kind = f.get("type").and_then(|t| t.as_str()).unwrap_or("unknown").to_string();
            Some(SecretHit {
                file,
                line: f.get("line").and_then(|l| l.as_u64()),
                kind,
            })
        })
        .collect()
}

/// **上传前本地闸**：命中或跑不起来都**拒**（fail closed → 一个字节都不出机器）。
pub(crate) fn privacy_gate(
    runner: &dyn CmdRunner,
    python: &Path,
    triage: &Path,
    bundle: &Path,
) -> Result<(), IncidentUploadError> {
    let args: Vec<String> = vec![
        triage.display().to_string(),
        "--privacy-check".into(),
        bundle.display().to_string(),
        "--privacy-json".into(),
    ];
    let out = runner.run(python, &args).map_err(|e| {
        // 闸跑不起来 ⇒ 也拒（否则「闸坏了」等于「没有闸」）。
        IncidentUploadError::Server {
            code: 0,
            message: format!("本地隐私闸跑不起来（{e}）⇒ 按 fail closed 拒绝上传。"),
        }
    })?;
    if out.code == 0 {
        return Ok(());
    }
    let hits = parse_findings(&out.stdout);
    let detail = if hits.is_empty() {
        out.stdout.trim().to_string()
    } else {
        format!("命中 {} 处（只给位置与类型）", hits.len())
    };
    Err(IncidentUploadError::SecretDetected {
        message: format!(
            "本机隐私闸拦下了这个包：{detail} ⇒ 已 fail closed，**没有上传**。请先在本地脱敏后重试。"
        ),
        hits,
    })
}

// ---------------------------------------------------------------------------
// 上传
// ---------------------------------------------------------------------------

/// `curl -w '\n%{http_code}'` 的最后一行是状态码，其余是 body。
fn split_status(stdout: &str) -> (Option<u16>, String) {
    let trimmed = stdout.trim_end_matches(['\n', '\r']);
    let Some((body, code)) = trimmed.rsplit_once('\n') else {
        return (trimmed.parse::<u16>().ok(), String::new());
    };
    (code.trim().parse::<u16>().ok(), body.to_string())
}

fn endpoint_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("message")?.as_str().map(str::to_string)
}

/// 端点 422 的 body → 我们的错误类型（`error` 字段决定是哪一类）。
fn refused(body: &str, code: u16) -> IncidentUploadError {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let kind = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
    let message = v
        .get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("HTTP {code}"));
    if kind == "secret_detected" {
        let hits = v
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|h| {
                        Some(SecretHit {
                            file: h.get("file")?.as_str()?.to_string(),
                            line: h.get("line").and_then(|l| l.as_u64()),
                            kind: h
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("unknown")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        IncidentUploadError::SecretDetected { message, hits }
    } else {
        IncidentUploadError::Server { code, message }
    }
}

/// `curl` 的结果 → 契约里的四类（纯函数，四条路径都在这一个地方）。
pub(crate) fn map_upload_response(out: &CmdOutput) -> Result<IncidentUpload, IncidentUploadError> {
    if out.code != 0 {
        return Err(IncidentUploadError::Network {
            message: format!(
                "上传没有完成（curl 退出码 {}）：{}",
                out.code,
                out.stderr.trim().chars().take(300).collect::<String>()
            ),
        });
    }
    let (status, body) = split_status(&out.stdout);
    match status {
        Some(201) | Some(200) => {
            serde_json::from_str::<IncidentUpload>(&body).map_err(|e| IncidentUploadError::Server {
                code: status.unwrap_or(0),
                message: format!("端点说收到了，但响应解析失败：{e}"),
            })
        }
        Some(429) => Err(IncidentUploadError::RateLimited {
            message: endpoint_message(&body).unwrap_or_else(|| "上传太频繁，稍后再试".into()),
        }),
        Some(422) => Err(refused(&body, 422)),
        Some(code) => Err(IncidentUploadError::Server {
            code,
            message: endpoint_message(&body).unwrap_or_else(|| format!("HTTP {code}")),
        }),
        None => Err(IncidentUploadError::Network {
            message: "拿不到 HTTP 状态码（curl 输出里没有 %{http_code}）".into(),
        }),
    }
}

/// 只允许上传**本机刚生成的那个包**：临时目录下的 `xraytun-incident-*.zip`。
///
/// 这是防「拿这条命令当任意文件外发通道」——前端只会传 `preview.bundle_path`，
/// 但命令本身必须自证。判据与本机包名一致，不依赖前端守规矩。
pub(crate) fn bundle_path_is_ours(path: &Path) -> bool {
    let ours = std::env::temp_dir().join(TEMP_SUBDIR);
    let name_ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("xraytun-incident-") && n.ends_with(".zip"));
    path.is_absolute() && path.starts_with(&ours) && name_ok
}

/// 上传（含**上传前复核本地隐私闸**与路径自证）。
///
/// `curl` 参数：`-sS`（静默但把错误发出来）、`--max-time 60`、
/// `Content-Type: application/zip`、`--data-binary @<包>`（**原样字节**，
/// 不做 multipart）、`-w '\n%{http_code}'`（把状态码拼在 body 后面）。
///
/// **不走本机代理**：报错场景常常正是「隧道坏了」——那时本地 SOCKS 端口也通不了，
/// 走它等于在最需要上传的时候上传不了。
pub(crate) fn upload_with(
    runner: &dyn CmdRunner,
    tools: &IncidentTools,
    url: &str,
    bundle: &Path,
) -> Result<IncidentUpload, IncidentUploadError> {
    if !bundle.is_file() {
        return Err(IncidentUploadError::Server {
            code: 0,
            message: format!("找不到现场包：{}（请重新生成）", bundle.display()),
        });
    }
    if !bundle_path_is_ours(bundle) {
        return Err(IncidentUploadError::Server {
            code: 0,
            message: format!(
                "拒绝上传：{} 不是本机刚生成的现场包（只允许临时目录 {} 下的 xraytun-incident-*.zip）",
                bundle.display(),
                std::env::temp_dir().join(TEMP_SUBDIR).display()
            ),
        });
    }
    // **本地闸在联网之前**：命中就不出机器（fail closed）。
    privacy_gate(runner, &tools.python, &tools.triage, bundle)?;
    let args: Vec<String> = vec![
        "-sS".into(),
        "--max-time".into(),
        "60".into(),
        "-X".into(),
        "POST".into(),
        "-H".into(),
        "Content-Type: application/zip".into(),
        "--data-binary".into(),
        format!("@{}", bundle.display()),
        "-w".into(),
        "\n%{http_code}".into(),
        url.to_string(),
    ];
    let out = runner
        .run(Path::new("/usr/bin/curl"), &args)
        .map_err(|e| IncidentUploadError::Network { message: e })?;
    map_upload_response(&out)
}

// ---------------------------------------------------------------------------
// 被动哨兵（只写本地，不联网）
// ---------------------------------------------------------------------------

pub(crate) fn anomaly_file(root: &Path) -> PathBuf {
    root.join("logs").join(ANOMALY_FILE)
}

/// 追加一条（一行一个 JSON 对象）。调用方决定失败怎么办（见 [`record`]）。
pub(crate) fn append_anomaly(root: &Path, a: &Anomaly) -> std::io::Result<()> {
    let path = anomaly_file(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string());
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

/// 读最近 `limit` 条（读不出来/坏行就跳过 —— 这是本地哨兵，不是账本）。
pub(crate) fn read_anomalies(root: &Path, limit: usize) -> Vec<Anomaly> {
    let Ok(text) = std::fs::read_to_string(anomaly_file(root)) else {
        return Vec::new();
    };
    let mut all: Vec<Anomaly> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Anomaly>(l).ok())
        .collect();
    if all.len() > limit {
        all.drain(..all.len() - limit);
    }
    all
}

/// 记一条哨兵。**不许影响主流程**：写不进去只留一条 `tracing::warn`。
pub(crate) fn record(state: &AppState, kind: &str, level: &str, message: impl AsRef<str>) {
    let anomaly = Anomaly {
        ts_unix: xt_core::util::now_unix(),
        kind: kind.to_string(),
        level: level.to_string(),
        message: message.as_ref().to_string(),
    };
    if let Err(e) = append_anomaly(state.store.root(), &anomaly) {
        tracing::warn!(error = %e, kind = %kind, "写本地异常哨兵失败（不影响主流程）");
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

/// 只在本机出包并返回清单，**不上传**。
///
/// 脚本缺一个就**明确报错** —— 「包已生成」这种结论不能靠猜。
#[tauri::command]
pub async fn incident_preview(app: AppHandle) -> Result<IncidentPreview, String> {
    let dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("取不到随包资源目录：{e}"))?;
    let tools = IncidentTools::from_resource_dir(&dir);
    preview_tools_gate(&tools)?;
    let out_dir = std::env::temp_dir().join(TEMP_SUBDIR);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let stamp = xt_core::util::now_unix();
    let zip = out_dir.join(format!("xraytun-incident-{stamp}.zip"));
    let summary = out_dir.join(format!("xraytun-incident-{stamp}.json"));
    tauri::async_runtime::spawn_blocking(move || {
        preview_with(&RealRunner, &tools, &zip, &summary)
    })
    .await
    .map_err(|e| format!("出包任务失败：{e}"))?
}

/// 上传**本机刚生成的**现场包（用户点「确认上传」才会走到这里）。
#[tauri::command]
pub async fn incident_upload(
    app: AppHandle,
    bundle_path: String,
) -> Result<IncidentUpload, IncidentUploadError> {
    let dir = app.path().resource_dir().map_err(|e| IncidentUploadError::Server {
        code: 0,
        message: format!("取不到随包资源目录：{e}"),
    })?;
    let tools = IncidentTools::from_resource_dir(&dir);
    upload_tools_gate(&tools)?;
    tauri::async_runtime::spawn_blocking(move || {
        upload_with(&RealRunner, &tools, INCIDENT_URL, Path::new(&bundle_path))
    })
    .await
    .map_err(|e| IncidentUploadError::Server {
        code: 0,
        message: format!("上传任务失败：{e}"),
    })?
}

/// 本地待上报的异常条数（给界面角标用）。
#[tauri::command]
pub async fn incident_anomaly_count(state: State<'_, AppState>) -> Result<usize, String> {
    let root = state.store.root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || read_anomalies(&root, usize::MAX).len())
        .await
        .map_err(|e| format!("读本地异常哨兵失败：{e}"))
}

/// 测试用的假 runner 也放在这里，让单元测试与实现同文件。
#[cfg(test)]
struct FakeRunner {
    /// 每次调用的 (program, args)。
    calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    /// 依次返回的结果；取完就 panic（测试应当精确控制次数）。
    replies: std::sync::Mutex<VecDeque<Result<CmdOutput, String>>>,
}

#[cfg(test)]
impl FakeRunner {
    fn new(replies: Vec<Result<CmdOutput, String>>) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            replies: std::sync::Mutex::new(replies.into()),
        }
    }
    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl CmdRunner for FakeRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CmdOutput, String> {
        self.calls
            .lock()
            .unwrap()
            .push((program.display().to_string(), args.to_vec()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("FakeRunner 的回复用完了（测试应当按调用次数精确给定）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "t130-{tag}-{}-{}",
            std::process::id(),
            xt_core::util::now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn tools() -> IncidentTools {
        IncidentTools {
            python: PathBuf::from("/usr/bin/python3"),
            bundle: PathBuf::from("/res/scripts/incident-bundle.sh"),
            triage: PathBuf::from("/res/scripts/triage-incident.py"),
        }
    }

    fn our_bundle(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(TEMP_SUBDIR);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("xraytun-incident-{tag}-{}.zip", std::process::id()));
        std::fs::write(&path, b"PK\x03\x04fake").unwrap();
        path
    }

    fn summary_json() -> String {
        serde_json::json!({
            "bundle_path": "/tmp/xraytun-incident/xraytun-incident-1.zip",
            "size_bytes": 4096,
            "files": {
                "core-tail.txt": {"bytes": 2048, "sha256": "aa"},
                "README.txt": {"bytes": 1024, "sha256": "bb"},
                "manifest.json": {"bytes": 1024, "sha256": "cc"}
            },
            "readme": "本包在本机脱敏后生成。",
            "manifest": {"bundle_format": "xraytun-incident/1", "versions": {"app": {"value": "0.8.34"}}},
            "truncated": ["core-tail.txt"]
        })
        .to_string()
    }

    /// 摘要 → 预览：清单、README、manifest 文本、截断项都要带出来（且**排序稳定**）。
    #[test]
    fn preview_from_summary_carries_files_readme_manifest_and_truncation() {
        let p = preview_from_summary(&summary_json()).expect("摘要合法");
        assert_eq!(p.size_bytes, 4096);
        assert_eq!(
            p.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["README.txt", "core-tail.txt", "manifest.json"],
            "清单要排好序（界面按序渲染）"
        );
        assert_eq!(p.files[1].bytes, 2048);
        assert_eq!(p.files[2].sha256, "cc");
        assert!(p.readme.contains("脱敏"));
        assert!(
            p.manifest.contains("xraytun-incident/1"),
            "manifest 必须是**文本**（前端按 pre-wrap 显示）：{}",
            p.manifest
        );
        assert_eq!(p.truncated, vec!["core-tail.txt"], "截断项必须被带出来");
    }

    /// 反例：摘要缺 `manifest` ⇒ 必须报错，**不许**静默给一个空预览。
    #[test]
    fn preview_summary_without_manifest_is_an_error() {
        let bad = serde_json::json!({
            "bundle_path": "/tmp/x.zip", "size_bytes": 1, "files": {}, "readme": ""
        })
        .to_string();
        let err = preview_from_summary(&bad).expect_err("缺 manifest 必须报错");
        assert!(err.contains("解析失败"), "{err}");
    }

    /// 出包失败（脚本非零）⇒ 把 stderr 带回来，**不许**当成「包已生成」。
    #[test]
    fn preview_fails_loudly_when_the_script_fails() {
        let runner = FakeRunner::new(vec![Ok(CmdOutput {
            code: 3,
            stdout: String::new(),
            stderr: "✗ 需要 python3".into(),
        })]);
        let root = tmp_root("preview-fail");
        let err = preview_with(
            &runner,
            &tools(),
            &root.join("x.zip"),
            &root.join("x.json"),
        )
        .expect_err("脚本非零必须报错");
        assert!(err.contains("退出码 3"), "{err}");
        assert!(err.contains("需要 python3"), "stderr 要带给用户：{err}");
    }

    /// 出包参数：必须是**调用脚本**（bash + 脚本路径 + --out/--json-out），
    /// 而不是在 Rust 里另写一套打包。假 runner 顺手把摘要写出来（模拟真实脚本）。
    #[test]
    fn preview_invokes_the_bundled_script_with_out_and_json_out() {
        struct WritingRunner {
            summary: PathBuf,
            calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
        }
        impl CmdRunner for WritingRunner {
            fn run(&self, program: &Path, args: &[String]) -> Result<CmdOutput, String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.display().to_string(), args.to_vec()));
                // 真实脚本的 `--json-out` 就是在这里把摘要写出来的。
                std::fs::write(&self.summary, summary_json()).unwrap();
                Ok(CmdOutput {
                    code: 0,
                    stdout: "ok".into(),
                    stderr: String::new(),
                })
            }
        }

        let root = tmp_root("preview-args");
        let summary = root.join("x.json");
        let zip = root.join("x.zip");
        let runner = WritingRunner {
            summary: summary.clone(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let p = preview_with(&runner, &tools(), &zip, &summary).expect("成功");
        assert!(p.readme.contains("脱敏"), "{:?}", p.readme);
        let calls = runner.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "/bin/bash");
        assert_eq!(calls[0].1[0], "/res/scripts/incident-bundle.sh");
        assert!(calls[0].1.contains(&"--out".to_string()));
        assert!(calls[0].1.contains(&"--json-out".to_string()));
    }

    // ------------------------------------------------------------- 上传四路径

    fn ok_curl_body() -> CmdOutput {
        CmdOutput {
            code: 0,
            stdout: "{\"id\":\"inc-201\",\"sha256\":\"deadbeef\",\"bytes\":4096,\"received_at\":\"2026-09-23T03:29:48.123Z\"}\n201".into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn upload_201_maps_to_the_frozen_shape() {
        let got = map_upload_response(&ok_curl_body()).expect("201 成功");
        assert_eq!(got.id, "inc-201");
        assert_eq!(got.bytes, 4096);
        assert_eq!(
            got.received_at, "2026-09-23T03:29:48.123Z",
            "received_at 必须是 ISO 串原样（不许转 unix）"
        );
    }

    #[test]
    fn upload_422_secret_keeps_positions_and_never_echoes_the_secret() {
        let body = serde_json::json!({
            "error": "secret_detected",
            "message": "包内疑似含密钥/订阅信息，已拒收",
            "hits": [{"type": "node_url", "file": "core-tail.txt", "line": 42}],
            "hits_truncated": false
        })
        .to_string();
        let out = CmdOutput {
            code: 0,
            stdout: format!("{body}\n422"),
            stderr: String::new(),
        };
        match map_upload_response(&out).expect_err("422 必须失败") {
            IncidentUploadError::SecretDetected { message, hits } => {
                assert!(message.contains("疑似含密钥"), "{message}");
                assert_eq!(hits.len(), 1, "命中位置要带出来：{hits:?}");
                assert_eq!(hits[0].file, "core-tail.txt");
                assert_eq!(hits[0].kind, "node_url");
                assert_eq!(hits[0].line, Some(42));
            }
            other => panic!("422 secret 必须映射成 SecretDetected，实际 {other:?}"),
        }
    }

    #[test]
    fn upload_429_is_rate_limited_and_5xx_is_server() {
        let out = |code: u16, body: &str| CmdOutput {
            code: 0,
            stdout: format!("{body}\n{code}"),
            stderr: String::new(),
        };
        match map_upload_response(&out(429, "{\"error\":\"rate_limited\",\"message\":\"请求过于频繁\"}"))
            .expect_err("429 必须失败")
        {
            IncidentUploadError::RateLimited { message } => assert!(message.contains("频繁"), "{message}"),
            other => panic!("429 必须是 RateLimited，实际 {other:?}"),
        }
        match map_upload_response(&out(500, "{\"error\":\"boom\",\"message\":\"内部错误\"}"))
            .expect_err("5xx 必须失败")
        {
            IncidentUploadError::Server { code, message } => {
                assert_eq!(code, 500);
                assert!(message.contains("内部错误"), "{message}");
            }
            other => panic!("5xx 必须是 Server，实际 {other:?}"),
        }
    }

    #[test]
    fn upload_curl_failure_is_network() {
        let out = CmdOutput {
            code: 28,
            stdout: String::new(),
            stderr: "curl: (28) Operation timed out".into(),
        };
        match map_upload_response(&out).expect_err("curl 非零必须失败") {
            IncidentUploadError::Network { message } => {
                assert!(message.contains("28"), "{message}");
                assert!(message.contains("timed out"), "{message}");
            }
            other => panic!("网络层失败必须是 Network，实际 {other:?}"),
        }
    }

    /// **上传前必须过本地闸**（fail closed）：闸命中 ⇒ **一个字节都不发**（curl 没被调）。
    ///
    /// 这就是「去掉 privacy-check ⇒ 断言红」的那条断言。
    #[test]
    fn upload_runs_the_privacy_gate_before_posting_and_never_posts_when_it_hits() {
        let gate_json = serde_json::json!({
            "ok": false,
            "findings": [{"file": "core-tail.txt", "line": 7, "type": "uuid_literal", "masked": "b831…(36)"}]
        })
        .to_string();
        let runner = FakeRunner::new(vec![Ok(CmdOutput {
            code: 1,
            stdout: gate_json,
            stderr: String::new(),
        })]);
        let bundle = our_bundle("gate");
        match upload_with(&runner, &tools(), INCIDENT_URL, &bundle).expect_err("闸命中必须拒") {
            IncidentUploadError::SecretDetected { hits, .. } => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].line, Some(7));
            }
            other => panic!("本地闸命中必须映射成 SecretDetected，实际 {other:?}"),
        }
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "命中时**只**允许跑闸，绝不许发 POST：{calls:?}");
        assert!(calls[0].0.contains("python3"), "{calls:?}");
        assert!(calls[0].1.contains(&"--privacy-check".to_string()), "{calls:?}");
    }

    /// 闸干净 ⇒ 才发 POST，且 POST 的形状是「原始 zip 字节 + 正确的 Content-Type」。
    #[test]
    fn upload_posts_the_raw_zip_with_the_right_content_type() {
        let runner = FakeRunner::new(vec![
            Ok(CmdOutput {
                code: 0,
                stdout: "{\"ok\":true,\"findings\":[]}".into(),
                stderr: String::new(),
            }),
            Ok(ok_curl_body()),
        ]);
        let bundle = our_bundle("post");
        let got = upload_with(&runner, &tools(), INCIDENT_URL, &bundle).expect("上传成功");
        assert_eq!(got.id, "inc-201");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "先闸、后 POST：{calls:?}");
        let post = &calls[1];
        assert_eq!(post.0, "/usr/bin/curl");
        assert!(post.1.contains(&"Content-Type: application/zip".to_string()));
        assert!(
            post
                .1
                .iter()
                .any(|a| a == &format!("@{}", bundle.display())),
            "body 必须是这个包的原始字节：{post:?}"
        );
        assert_eq!(post.1.last().unwrap(), INCIDENT_URL);
    }

    /// **路径自证**：不是本机刚生成的包 ⇒ 连闸都不用跑，直接拒。
    #[test]
    fn upload_refuses_a_path_that_is_not_ours() {
        let runner = FakeRunner::new(vec![]);
        let outside = std::env::temp_dir().join("not-ours.zip");
        std::fs::write(&outside, b"x").unwrap();
        match upload_with(&runner, &tools(), INCIDENT_URL, &outside).expect_err("必须拒") {
            IncidentUploadError::Server { code, message } => {
                assert_eq!(code, 0, "code 0 = 本地拒绝（不是服务端）");
                assert!(message.contains("拒绝上传"), "{message}");
            }
            other => panic!("本地拒绝必须是 Server{{code:0}}，实际 {other:?}"),
        }
        assert!(runner.calls().is_empty(), "本地拒绝时不该跑任何子进程");
    }

    /// 包不存在也要**说清楚**，不许静默。
    #[test]
    fn upload_reports_a_missing_bundle() {
        let runner = FakeRunner::new(vec![]);
        let missing = std::env::temp_dir()
            .join(TEMP_SUBDIR)
            .join("xraytun-incident-nope.zip");
        match upload_with(&runner, &tools(), INCIDENT_URL, &missing).expect_err("必须拒") {
            IncidentUploadError::Server { message, .. } => assert!(message.contains("找不到现场包"), "{message}"),
            other => panic!("实际 {other:?}"),
        }
    }

    /// **F-2（task-148）**：脚本缺失 / spawn 失败（127）必须是 **fail closed**，
    /// 而且**不许发出任何 POST**（出包阶段本来就不联网，这条防的是「将来有人在
    /// 失败路径上加一步上传」）。
    #[test]
    fn missing_scripts_and_spawn_failure_are_fail_closed() {
        // ① 前置闸：三个脚本都不存在（拿一个不存在的资源目录）。
        let missing_tools = IncidentTools::from_resource_dir(Path::new("/definitely/not/a/resource/dir"));
        let gate = preview_tools_gate(&missing_tools).expect_err("缺脚本必须拦下");
        assert!(gate.contains("缺少脚本"), "文案要指向「脚本缺失」：{gate}");
        assert!(
            gate.contains("incident-bundle.sh"),
            "要点名缺的是哪个：{gate}"
        );
        assert!(!gate.contains("上传失败"), "不许笼统说成上传失败：{gate}");
        assert!(
            upload_tools_gate(&missing_tools).is_err(),
            "上传前那道同样的闸也必须拦"
        );

        // ② `preview_with` 的 spawn 失败（runner 直接返回 Err，模拟 ENOENT/127 场景）。
        let runner = FakeRunner::new(vec![Err("执行 /bin/bash 失败：No such file (os error 2)".into())]);
        let root = tmp_root("preview-spawn");
        let err = preview_with(&runner, &tools(), &root.join("x.zip"), &root.join("x.json"))
            .expect_err("spawn 失败必须报错");
        assert!(err.contains("执行 /bin/bash 失败"), "{err}");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "只该尝试跑脚本，绝不许再发 POST：{calls:?}");
        assert_eq!(calls[0].0, "/bin/bash", "不许出现 curl：{calls:?}");

        // ③ 退出码 127（脚本不存在时 shell 的经典返回）同样 fail closed。
        let runner = FakeRunner::new(vec![Ok(CmdOutput {
            code: 127,
            stdout: String::new(),
            stderr: "bash: /res/scripts/incident-bundle.sh: No such file or directory".into(),
        })]);
        let err = preview_with(&runner, &tools(), &root.join("y.zip"), &root.join("y.json"))
            .expect_err("退出码 127 必须报错");
        assert!(err.contains("退出码 127"), "{err}");
        assert_eq!(runner.calls().len(), 1, "绝不许发 POST");
    }

    /// **F-2 核心（task-148）**：脚本 **exit 0 但没写出摘要** ⇒ 必须 **fail closed**。
    ///
    /// 不许把「没有摘要」当成「空摘要」成功返回 —— 那正是「失败被当成成功」
    /// 的经典形状：界面会显示一个空清单，用户以为包已经好了。
    #[test]
    fn exit_zero_without_summary_is_fail_closed() {
        let runner = FakeRunner::new(vec![Ok(CmdOutput {
            code: 0,
            stdout: "（脚本说成功，但 --json-out 什么也没写）".into(),
            stderr: String::new(),
        })]);
        let root = tmp_root("preview-no-summary");
        let summary = root.join("nope.json");
        let err = preview_with(&runner, &tools(), &root.join("x.zip"), &summary)
            .expect_err("没有摘要必须报错（fail closed）");
        assert!(err.contains("没有写出摘要"), "文案要指向「摘要缺失」：{err}");
        assert!(
            err.contains(&summary.display().to_string()),
            "要点名缺的是哪个文件：{err}"
        );
        assert!(!err.contains("上传失败"), "不许笼统说成上传失败：{err}");
        assert_eq!(runner.calls().len(), 1, "绝不许发 POST");
    }

    /// **F-2**：上传路径上「本地闸跑不起来」（python 缺失/ spawn 失败）也必须
    /// **fail closed**（类型化错误 + **不发 POST**）。
    #[test]
    fn upload_gate_spawn_failure_is_fail_closed_and_never_posts() {
        let runner = FakeRunner::new(vec![Err("执行 /usr/bin/python3 失败：No such file".into())]);
        let bundle = our_bundle("gate-spawn");
        match upload_with(&runner, &tools(), INCIDENT_URL, &bundle).expect_err("闸跑不起来必须拒") {
            IncidentUploadError::Server { code, message } => {
                assert_eq!(code, 0, "code 0 = 本地拒绝（不是服务端）");
                assert!(message.contains("本地隐私闸跑不起来"), "{message}");
                assert!(message.contains("fail closed"), "要说清为什么拒：{message}");
            }
            other => panic!("必须是本地的 Server{{code:0}}，实际 {other:?}"),
        }
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "只在「跑闸」时动了一次子进程：{calls:?}");
        assert!(
            calls[0].0.contains("python3"),
            "绝不许出现 curl：{calls:?}"
        );
    }

    // ------------------------------------------------------------- 哨兵

    /// 构造一条异常 ⇒ 文件多一条、计数 +1（且读回来字段一致）。
    #[test]
    fn anomalies_append_increment_and_read_back() {
        let root = tmp_root("anomaly");
        assert_eq!(read_anomalies(&root, usize::MAX).len(), 0, "起点为空");
        let a = Anomaly {
            ts_unix: 1_790_051_357,
            kind: "probe_round_failed".into(),
            level: "warn".into(),
            message: "境外全灭".into(),
        };
        append_anomaly(&root, &a).expect("写一条");
        assert_eq!(read_anomalies(&root, usize::MAX).len(), 1, "计数必须 +1");
        append_anomaly(&root, &a).expect("再写一条");
        let all = read_anomalies(&root, usize::MAX);
        assert_eq!(all.len(), 2, "每写一条就多一条（哨兵不是去重账本）");
        assert_eq!(all[1], a, "读回来的字段必须一致");
        assert_eq!(
            read_anomalies(&root, 1).len(),
            1,
            "limit 生效（界面只看最近几条）"
        );
        // 文件名与位置是契约的一部分（`logs/anomalies.jsonl`）。
        assert!(anomaly_file(&root).ends_with("logs/anomalies.jsonl"));
    }

    /// 坏行不许让读取整段失败（本地哨兵不是账本，但也**不许**把好行一起吞掉）。
    #[test]
    fn anomalies_skip_malformed_lines_but_keep_the_rest() {
        let root = tmp_root("anomaly-bad");
        let f = anomaly_file(&root);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(
            &f,
            "{\"ts_unix\":1,\"kind\":\"self_healed\",\"level\":\"info\",\"message\":\"ok\"}\n这不是 JSON\n",
        )
        .unwrap();
        let all = read_anomalies(&root, usize::MAX);
        assert_eq!(all.len(), 1, "坏行跳过、好行留下：{all:?}");
        assert_eq!(all[0].kind, "self_healed");
    }

    /// **源码守卫**：五类信号每一类都必须在**生产源码**里有且仅有一次 `record(...)` 调用。
    ///
    /// 删掉任意一处哨兵记录 ⇒ 本测试红（这就是「去掉哨兵记录 ⇒ 断言红」）。
    #[test]
    fn every_anomaly_kind_is_recorded_in_production_source() {
        let src = production_source();
        // 去空白后计数：调用被 rustfmt 折成多行时，按行匹配的 needle 会**假绿**。
        // 同时把 `record(&state, …)` 归一成 `record(state, …)` —— 有些调用点拿到的
        // 已经是 `&AppState`（clippy 的 needless_borrow 要求不加 `&`）。
        let flat: String = src
            .split_whitespace()
            .collect::<String>()
            .replace("record(&state,", "record(state,");
        for kind in ANOMALY_KINDS {
            let needle = format!("record(state,\"{kind}\"");
            assert_eq!(
                flat.matches(&needle).count(),
                1,
                "`{kind}` 必须在生产源码里恰好记录一次（现在 {} 次）",
                flat.matches(&needle).count()
            );
        }
    }

    /// **源码守卫**：上传路径必须调用本地闸（`privacy_gate`）。
    #[test]
    fn upload_checks_privacy_before_posting_in_production_source() {
        let src = production_source();
        assert!(
            src.contains("privacy_gate(runner, &tools.python, &tools.triage, bundle)?;"),
            "上传前必须过本地隐私闸（fail closed）"
        );
    }

    /// 生产源码 = **本模块 + 它的两个信号宿主**（core.rs / diagnostics.rs），
    /// 各自去掉测试模块（否则守卫会被测试里的字面量或解释性注释绊倒 —— task-127 踩过）。
    fn production_source() -> String {
        let mut out = String::new();
        for (name, full) in [
            ("incident.rs", include_str!("incident.rs")),
            ("core.rs", include_str!("core.rs")),
            ("diagnostics.rs", include_str!("diagnostics.rs")),
        ] {
            let prod = full
                .split("\n#[cfg(test)]\nmod tests {")
                .next()
                .unwrap_or(full);
            out.push_str("\n// ==== ");
            out.push_str(name);
            out.push_str(" ====\n");
            out.push_str(prod);
        }
        out
    }

    /// **手动验收工具**（`#[ignore]`，不进 CI）：对**真实端点**跑一遍完整的
    /// [`upload_with`]（本地闸 → `/usr/bin/curl` → 映射）。
    ///
    /// 它会**真的把一个包上传到线上**，所以只在发版验收时手动跑；上传后必须用
    /// 令牌 `DELETE` 清掉，并把「对象数回到 0」写进验收记录。
    ///
    /// ```text
    /// # ⚠️ 路径必须是**本机刚生成的**那个包：`bundle_path_is_ours()` 只接受
    /// # `$TMPDIR/xraytun-incident/xraytun-incident-*.zip`（macOS 的 `temp_dir()`
    /// # 是 `/var/folders/…/T`，**不是** `/tmp`）。用 `/tmp/...` 会当场被拒 ——
    /// # 这条自证规则的存在意义：不许把这条命令当**任意文件外发通道**
    /// # （前端只会传 `preview.bundle_path`，但命令本身必须自证）。
    /// XT_T130_BUNDLE="$TMPDIR/xraytun-incident/xraytun-incident-<ts>.zip" \
    ///   cargo test -p xraytun-desktop --lib real_upload_acceptance -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "手动验收：会真的上传到线上；需要 XT_T130_BUNDLE"]
    fn real_upload_acceptance() {
        let Ok(bundle) = std::env::var("XT_T130_BUNDLE") else {
            eprintln!("跳过：请设置 XT_T130_BUNDLE=<真实现场包 zip>");
            return;
        };
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("仓库根")
            .to_path_buf();
        let tools = IncidentTools {
            python: PathBuf::from("/usr/bin/python3"),
            bundle: repo.join("scripts/incident-bundle.sh"),
            triage: repo.join("scripts/triage-incident.py"),
        };
        match upload_with(&RealRunner, &tools, INCIDENT_URL, Path::new(&bundle)) {
            Ok(u) => println!(
                "UPLOAD_OK id={} bytes={} received_at={} sha256={}",
                u.id, u.bytes, u.received_at, u.sha256
            ),
            Err(e) => panic!("真实上传失败：{e:?}"),
        }
    }

    /// 契约形状的哨兵：三个命令名逐字、`Anomaly` 字段逐字（前端按这些名字调）。
    #[test]
    fn the_frozen_shape_is_what_we_return() {
        let src = production_source();
        for cmd in ["incident_preview", "incident_upload", "incident_anomaly_count"] {
            assert!(src.contains(cmd), "生产源码里必须有命令 {cmd}");
        }
        // 不允许偷偷注册第四个（前端没封装它 ⇒ type_contract 会红）
        assert!(
            !src.contains("pub async fn incident_anomalies"),
            "本版只注册 3 个命令（Lead 裁决）：incident_anomalies 不实现"
        );
        let a = Anomaly {
            ts_unix: 1,
            kind: "self_healed".into(),
            level: "info".into(),
            message: "m".into(),
        };
        let text = serde_json::to_string(&a).unwrap();
        assert!(text.contains("\"ts_unix\""), "{text}");
        assert!(text.contains("\"kind\""), "{text}");
        assert!(text.contains("\"level\""), "{text}");
        assert!(text.contains("\"message\""), "{text}");
        // 错误对象的 tag 必须是 kind（前端 parseIncidentFailure 按它分派）
        let e = IncidentUploadError::RateLimited {
            message: "x".into(),
        };
        let text = serde_json::to_string(&e).unwrap();
        assert!(text.contains("\"kind\":\"rate_limited\""), "{text}");
    }
}
