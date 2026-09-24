//! 审计：每一次判决都要能被复查、被申诉。
//!
//! # 为什么审计不是"日志"
//!
//! 用户看到的是一句"这个网站被拦了"。如果我们的回答只有"模型说的"，
//! 那就是一个无法申诉的黑盒。所以每一条判决都要落一行**结构化**的记录：
//! 类别、三个概率、阈值、用的是哪个模型、是缓存命中还是真问了网关、
//! 以及 —— 关键 —— **这条判决到底有没有真的生效**（演练模式下不生效）。
//!
//! # 隐私默认值
//!
//! 审计里**不写**我们发给网关的 `state` 全文（那里面有用户正在访问的站点）。
//! 只写主机名与判决所需的数值。想看"发出去的是什么"，得用户在界面上显式打开
//! "记录外发内容"，那时才会写 `context_sent`。默认值是产品态度。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一次调用的 token 用量（上游可选返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

/// 审计记录。字段只增不改名 —— 历史文件要能一直读。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub ts_unix: u64,
    /// 被判定的主机名（**唯一必须有的身份字段**）。
    pub host: String,
    /// `block` / `allow` / `deferred`。
    pub outcome: String,
    /// 细化原因（`allow` / `deferred` 才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ads_intent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_of_breakage: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_min: Option<f32>,
    /// 这条判决是否真的变成了配置里的规则（演练模式恒为 `false`）。
    pub applied: bool,
    pub cache_hit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 只在用户显式开启"记录外发内容"时才写（默认 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_sent: Option<String>,
}

/// append-only 的 JSONL 审计。
#[derive(Debug, Clone)]
pub struct AuditLog {
    path: Option<PathBuf>,
    max_bytes: u64,
    /// 已写入条数（本进程）。
    pub written: u64,
}

impl AuditLog {
    /// 关掉审计（`AuditLog` 仍然是可用的对象，只是不落盘）——
    /// 比 `Option<AuditLog>` 好在调用点不需要到处 `if let`。
    pub fn disabled() -> Self {
        Self { path: None, max_bytes: 0, written: 0 }
    }

    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self { path: Some(path), max_bytes: max_bytes.max(1024), written: 0 }
    }

    pub fn default_path(data_root: &Path) -> PathBuf {
        data_root.join("intent-audit.jsonl")
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 追加一条。超过上限时**先轮转**（`.1`），保证文件不会无限长。
    ///
    /// 返回错误而不是 panic：审计写不进去不该让判定流程崩掉。
    pub fn append(&mut self, rec: &AuditRecord) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() >= self.max_bytes {
                let rotated = path.with_extension("jsonl.1");
                let _ = std::fs::remove_file(&rotated);
                let _ = std::fs::rename(&path, &rotated);
            }
        }
        let line = serde_json::to_string(rec)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        self.written = self.written.saturating_add(1);
        Ok(())
    }

    /// 读最近 `n` 条（界面审计页）。坏行**跳过而不报错** ——
    /// 一行读不懂不该让整页打不开。
    pub fn tail(path: &Path, n: usize) -> std::io::Result<Vec<AuditRecord>> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out: Vec<AuditRecord> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<AuditRecord>(l).ok())
            .collect();
        if out.len() > n {
            out.drain(..out.len() - n);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("xt-intent-audit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn rec(host: &str, applied: bool) -> AuditRecord {
        AuditRecord {
            ts_unix: 1_700_000_000,
            host: host.into(),
            outcome: "block".into(),
            reason: None,
            category: Some("ad_or_monetization".into()),
            ads_intent: Some(0.97),
            risk_of_breakage: Some(0.05),
            choice_confidence: Some(0.93),
            effective_min: Some(0.85),
            applied,
            cache_hit: false,
            model: Some("jev-latest".into()),
            usage: Some(Usage { input_tokens: 90, output_tokens: 12 }),
            context_sent: None,
        }
    }

    #[test]
    fn append_and_read_back() {
        let dir = tmpdir("append");
        let path = AuditLog::default_path(&dir);
        let mut log = AuditLog::new(path.clone(), 1 << 20);
        log.append(&rec("a.example", false)).unwrap();
        log.append(&rec("b.example", true)).unwrap();
        assert_eq!(log.written, 2);

        let tail = AuditLog::tail(&path, 10).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].host, "a.example");
        assert!(!tail[0].applied, "演练模式必须如实记录未生效");
        assert!(tail[1].applied);
        assert_eq!(tail[1].usage.unwrap().input_tokens, 90);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_context_is_absent_by_default() {
        let dir = tmpdir("privacy");
        let path = AuditLog::default_path(&dir);
        let mut log = AuditLog::new(path.clone(), 1 << 20);
        log.append(&rec("a.example", true)).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("context_sent"), "默认审计里不许出现外发内容：{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_keeps_the_file_bounded() {
        let dir = tmpdir("rotate");
        let path = AuditLog::default_path(&dir);
        // 上限压到 1 KiB：几条记录就会触发轮转。
        let mut log = AuditLog::new(path.clone(), 1024);
        for i in 0..40 {
            log.append(&rec(&format!("h{i}.example"), true)).unwrap();
        }
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size < 4096, "轮转后文件不该继续长：{size}");
        let rotated = path.with_extension("jsonl.1");
        assert!(rotated.is_file(), "轮转文件应该存在");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_line_does_not_break_the_page() {
        let dir = tmpdir("broken");
        let path = AuditLog::default_path(&dir);
        let mut log = AuditLog::new(path.clone(), 1 << 20);
        log.append(&rec("good.example", true)).unwrap();
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{ not json\n").unwrap();
        }
        log.append(&rec("also-good.example", true)).unwrap();
        let tail = AuditLog::tail(&path, 10).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].host, "also-good.example");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_log_is_a_no_op() {
        let mut log = AuditLog::disabled();
        assert!(!log.is_enabled());
        assert!(log.append(&rec("a.example", true)).is_ok());
        assert_eq!(log.written, 0);
    }

    #[test]
    fn tail_of_a_missing_file_is_empty_not_an_error() {
        let dir = tmpdir("missing");
        let tail = AuditLog::tail(&dir.join("nope.jsonl"), 5).unwrap();
        assert!(tail.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_returns_the_last_n_in_order() {
        let dir = tmpdir("tail-n");
        let path = AuditLog::default_path(&dir);
        let mut log = AuditLog::new(path.clone(), 1 << 20);
        for i in 0..10 {
            log.append(&rec(&format!("h{i}.example"), true)).unwrap();
        }
        let tail = AuditLog::tail(&path, 3).unwrap();
        assert_eq!(tail.iter().map(|r| r.host.as_str()).collect::<Vec<_>>(), vec!["h7.example", "h8.example", "h9.example"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
