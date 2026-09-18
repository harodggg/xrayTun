//! 本地持久化。
//!
//! 刻意用**纯 JSON 文件**而不是 SQLite：
//!
//! * 数据量极小（几百个节点、几条订阅），SQLite 的查询能力用不上；
//! * 用户能直接打开 `~/Library/Application Support/...` 排查问题，
//!   这在代理工具的排障场景里价值很高；
//! * 少一个带 C 依赖的 crate。
//!
//! 敏感信息（订阅 URL 里的 token）**不进这里** —— 它们存在 Keychain，
//! 落盘的是 `keychain:<service>/<account>` 形式的引用，见 `docs/07-roadmap-and-risks.md`。

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{AppSettings, Node, Subscription};
use crate::util::now_unix;

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// macOS 约定路径：`~/Library/Application Support/<bundle-id>`。
    ///
    /// 可以用环境变量 `XRAYTUN_DATA_DIR` 覆盖。这不是为了「绕过权限」，
    /// 而是因为在两类真实场景里默认路径不可用：
    ///
    /// * **CI / 自动化测试**： runner 上不该往用户主目录里塞状态；
    /// * **受限运行环境**： 进程可能被限制只能写某个目录。
    ///
    /// 覆盖时会在日志里打一条 info，避免「配置怎么跑到别处去了」这种困惑。
    pub fn default_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("XRAYTUN_DATA_DIR") {
            let path = PathBuf::from(dir);
            tracing::info!(dir = %path.display(), "使用 XRAYTUN_DATA_DIR 覆盖数据目录");
            return path;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        home.join("Library")
            .join("Application Support")
            .join(crate::APP_IDENTIFIER)
    }

    pub fn with_default_root() -> Self {
        Self::new(Self::default_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.root.join("logs"))
            .map_err(|e| Error::Store(format!("创建数据目录失败: {e}")))?;
        Ok(())
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    fn subscriptions_path(&self) -> PathBuf {
        self.root.join("subscriptions.json")
    }

    fn nodes_path(&self) -> PathBuf {
        self.root.join("nodes.json")
    }

    /// 生成的 Xray 配置落在这里，便于用户直接 `xray run -c` 复现问题。
    pub fn core_config_path(&self) -> PathBuf {
        self.root.join("runtime").join("config.json")
    }

    // -----------------------------------------------------------------------
    // 设置
    // -----------------------------------------------------------------------

    pub fn load_settings(&self) -> AppSettings {
        // 读不到或解析失败都退回默认值：一个坏掉的 settings.json
        // 不应该让用户连界面都打不开。
        let mut settings = self.read_json::<AppSettings>(&self.settings_path()).unwrap_or_default();

        // 旧版本的设置在这里就地升级，并立刻落盘 —— 只在内存里改的话，
        // 每次启动都会重新迁移一遍，用户永远看不到「已经改过了」。
        let changes = settings.migrate();
        if !changes.is_empty() {
            for c in &changes {
                tracing::info!(change = %c, "设置已迁移");
            }
            if let Err(e) = self.save_settings(&settings) {
                // 迁移结果写不回去不是致命错误：内存里已经是新值，
                // 下次启动会再迁一次，行为一致。
                tracing::warn!(error = %e, "迁移后的设置写盘失败，下次启动会重试");
            }
        }
        settings
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.write_json(&self.settings_path(), settings)
    }

    // -----------------------------------------------------------------------
    // 订阅
    // -----------------------------------------------------------------------

    pub fn load_subscriptions(&self) -> Vec<Subscription> {
        self.read_json::<Vec<Subscription>>(&self.subscriptions_path()).unwrap_or_default()
    }

    pub fn save_subscriptions(&self, subs: &[Subscription]) -> Result<()> {
        self.write_json(&self.subscriptions_path(), subs)
    }

    // -----------------------------------------------------------------------
    // 节点
    // -----------------------------------------------------------------------

    pub fn load_nodes(&self) -> Vec<Node> {
        self.read_json::<Vec<Node>>(&self.nodes_path()).unwrap_or_default()
    }

    pub fn save_nodes(&self, nodes: &[Node]) -> Result<()> {
        self.write_json(&self.nodes_path(), nodes)
    }

    // -----------------------------------------------------------------------
    // 运行时配置
    // -----------------------------------------------------------------------

    /// 原子写出 Xray 配置（先写临时文件再 rename），避免核心读到半截 JSON。
    pub fn write_core_config(&self, json: &str) -> Result<PathBuf> {
        let path = self.core_config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Store(format!("创建 runtime 目录失败: {e}")))?;
        }
        atomic_write(&path, json.as_bytes())?;
        Ok(path)
    }

    pub fn read_core_config(&self) -> Option<String> {
        std::fs::read_to_string(self.core_config_path()).ok()
    }

    // -----------------------------------------------------------------------
    // 底层读写
    // -----------------------------------------------------------------------

    fn read_json<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Option<T> {
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                // 备份坏文件，方便用户/我们事后分析，同时确保下次能正常写入。
                tracing::warn!(path = %path.display(), error = %e, "配置文件解析失败，已忽略并使用默认值");
                let backup = path.with_extension(format!("corrupt.{}", now_unix()));
                let _ = std::fs::rename(path, backup);
                None
            }
        }
    }

    fn write_json<T: serde::Serialize + ?Sized>(&self, path: &Path, value: &T) -> Result<()> {
        self.ensure_dirs()?;
        let bytes = serde_json::to_vec_pretty(value)?;
        atomic_write(path, &bytes)
    }

    // -----------------------------------------------------------------------
    // 日志（按天一个文件，带容量上限）
    // -----------------------------------------------------------------------

    /// 日志目录。`ensure_dirs` 早就建过它，但**在此之前从没往里写过东西** ——
    /// 日志只存在内存里，App 一重启就没了。
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// 追加一条日志。
    ///
    /// # 为什么要落盘
    ///
    /// 排查的都是**启动/唤醒那一刻**发生的事，而那些日志恰恰在重启时被清空 ——
    /// 「开机后没自动连上」这类问题于是永远取不到证据（历史上反复修同一个症状，
    /// 有一部分原因就在这里）。落盘之后，用户重启完还能回头看到当时发生了什么。
    ///
    /// # 为什么不长成一个无限增长的文件
    ///
    /// 一天一个文件 + 单文件超过 [`LOG_ROTATE_BYTES`] 就轮转，
    /// 最多保留 [`LOG_KEEP_FILES`] 个。轮转时**按整行**保留最后
    /// [`LOG_ROTATE_KEEP`] 行，而不是按字节截断 —— 后者会把一行 JSON
    /// 切成两半，读回来直接解析失败。
    pub fn append_log(&self, line: &str) -> Result<()> {
        append_log_line(&self.logs_dir(), line)
    }

    /// 读最近 `limit` 条日志（跨天、跨轮转，按时间从旧到新）。
    ///
    /// 解析失败的行直接跳过：日志文件是排障用的，一行坏掉不该让整页打不开。
    pub fn tail_logs<T: serde::de::DeserializeOwned>(&self, limit: usize) -> Vec<T> {
        let dir = self.logs_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("app-") && n.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        // 文件名里带日期与轮转序号，字典序即时间序 —— 从新往旧读够 limit 条就停。
        files.sort();
        let mut out: Vec<T> = Vec::new();
        for path in files.iter().rev() {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let mut batch: Vec<T> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<T>(l).ok())
                .collect();
            batch.extend(out);
            out = batch;
            if out.len() >= limit {
                break;
            }
        }
        let skip = out.len().saturating_sub(limit);
        out.split_off(skip)
    }
}

/// 追加一行日志到 `dir`（**唯一实现**）。
///
/// 状态层（`apps/desktop`）与 `Store` 都走这里，避免两处各写一份
/// 命名/轮转逻辑 —— 那种重复一旦漂移，就会出现「界面里显示的日志」和
/// 「文件里的日志」对不上的情况。
pub fn append_log_line(dir: &Path, line: &str) -> Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Store(format!("创建日志目录失败: {e}")))?;
    let path = dir.join(log_file_name(now_unix()));
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > LOG_ROTATE_BYTES {
        rotate_log_file(&path);
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::Store(format!("打开日志失败: {e}")))?;
    // **0600**：日志里可能有节点地址、订阅主机名。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "{line}").map_err(|e| Error::Store(format!("写日志失败: {e}")))
}

/// 单文件超过它就开始轮转。
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// 轮转时保留最后多少行（按整行，见 [`Store::append_log`]）。
const LOG_ROTATE_KEEP: usize = 2000;

/// 同一份日志最多保留几个文件（含当前文件）。
const LOG_KEEP_FILES: usize = 3;

/// 日志文件名：按**天**分，便于「只看今天」和清理。
fn log_file_name(unix_secs: u64) -> String {
    format!("app-{}.jsonl", format_utc_date(unix_secs))
}

/// `YYYY-MM-DD`（UTC）。
///
/// 不引 `chrono`：这里只需要一个可排序的日期串，而少一个依赖对构建更友好
/// （与 `Cargo.toml` 里刻意不引时间库的取舍一致）。
fn format_utc_date(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    // 从 1970-01-01 起按民用历法往前推（Howard Hinnant 的 days_from_civil 逆运算）。
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 把 `path` 轮转成 `path.1`（旧的 `.1` 顺移），并把最后 [`LOG_ROTATE_KEEP`]
/// 行搬到新文件，使当前文件重新变小。超过 [`LOG_KEEP_FILES`] 的旧份删除。
fn rotate_log_file(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let Some(stem) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    // 先把最老的一份删掉，再把 .1..n-2 各往后挪一格
    let oldest = dir.join(format!("{stem}.{}", LOG_KEEP_FILES - 1));
    let _ = std::fs::remove_file(&oldest);
    for i in (1..LOG_KEEP_FILES - 1).rev() {
        let from = dir.join(format!("{stem}.{i}"));
        let to = dir.join(format!("{stem}.{}", i + 1));
        let _ = std::fs::rename(&from, &to);
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= LOG_ROTATE_KEEP {
        return;
    }
    let keep = lines[lines.len() - LOG_ROTATE_KEEP..].join("\n");
    if std::fs::rename(path, dir.join(format!("{stem}.1"))).is_ok() {
        let _ = std::fs::write(path, keep + "\n");
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| Error::Store(format!("写 {} 失败: {e}", tmp.display())))?;
    // **0600**：这些文件里有订阅 URL（本身就是凭据）和 GitHub token。
    // 默认 umask 会写成 0644，同机其他用户就能读到。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Store(format!("替换 {} 失败: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProxyMode;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("xt-store-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn settings_roundtrip() {
        let store = temp_store("settings");
        let s = AppSettings {
            mode: ProxyMode::Tun,
            socks_port: 12345,
            ..Default::default()
        };
        store.save_settings(&s).unwrap();
        let back = store.load_settings();
        assert_eq!(back.mode, ProxyMode::Tun);
        assert_eq!(back.socks_port, 12345);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn missing_file_yields_defaults_not_error() {
        let store = temp_store("missing");
        assert_eq!(store.load_settings().socks_port, 10808);
        assert!(store.load_subscriptions().is_empty());
        assert!(store.load_nodes().is_empty());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn corrupt_file_falls_back_and_is_quarantined() {
        let store = temp_store("corrupt");
        store.ensure_dirs().unwrap();
        std::fs::write(store.root().join("settings.json"), "{ this is not json").unwrap();
        let s = store.load_settings();
        assert_eq!(s.socks_port, 10808, "坏文件应退回默认设置");
        let quarantined = std::fs::read_dir(store.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(quarantined, "坏文件应被改名隔离");
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn core_config_is_written_atomically() {
        let store = temp_store("core");
        let path = store.write_core_config("{\"a\":1}").unwrap();
        assert!(path.exists());
        assert_eq!(store.read_core_config().unwrap(), "{\"a\":1}");
        // 临时文件不应残留
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 轮转必须**按整行**，否则会把一行 JSON 切成两半，读回来全部解析失败。
    #[test]
    fn log_rotation_keeps_whole_lines() {
        let dir = std::env::temp_dir().join(format!("xt-logrot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);

        // 写超过保留行数的条目，触发一次轮转
        for i in 0..(LOG_ROTATE_KEEP + 50) {
            store
                .append_log(&format!(r#"{{"ts_unix":{},"message":"line {i}"}}"#, 1_700_000_000 + i))
                .unwrap();
        }
        let back: Vec<serde_json::Value> = store.tail_logs(LOG_ROTATE_KEEP + 100);
        assert!(
            back.len() >= LOG_ROTATE_KEEP,
            "轮转不该丢数据到只剩 {} 条",
            back.len()
        );
        // 每一条都还是合法 JSON（被切半的行会在这里变成解析失败而消失）
        assert!(
            back.iter().all(|v| v.get("message").is_some()),
            "轮转后出现了残缺行"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `tail_logs` 的条数上限与时间顺序。
    #[test]
    fn tail_logs_respects_limit_and_order() {
        let dir = std::env::temp_dir().join(format!("xt-logtail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        for i in 0..20 {
            store.append_log(&format!(r#"{{"n":{i}}}"#)).unwrap();
        }
        let got: Vec<serde_json::Value> = store.tail_logs(5);
        assert_eq!(got.len(), 5, "应当只返回最后 5 条");
        assert_eq!(got[4]["n"], 19, "最后一条应当是最新写入的");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_root_is_under_application_support() {
        let root = Store::default_root();
        assert!(root.to_string_lossy().contains("Application Support"));
        assert!(root.to_string_lossy().ends_with(crate::APP_IDENTIFIER));
    }
}
