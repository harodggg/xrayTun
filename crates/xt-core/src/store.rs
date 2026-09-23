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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

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
    /// 排查的都是**启动/唤醒那一刻**发生的事，而那些日志在重启时会被清空 ——
    /// 「开机后没自动连上」这类问题于是永远取不到证据。落盘之后，用户重启完
    /// 还能回头看到当时发生了什么。
    ///
    /// # 为什么只有一个文件 + 一个备份
    ///
    /// 早先按天分文件、每天再按 5MB 轮转出 `.1/.2`，结果有四处毛病（都靠审查发现）：
    /// 按字节触发却按行数放弃、轮转时既保留整份又复制尾部（**同一条日志出现两次**）、
    /// `tail_logs` 的过滤条件漏掉了 `.1` 后缀、以及**没有任何地方清理旧日期文件**
    /// （于是「有容量上限」根本不成立）。
    ///
    /// 现在只留一个活动文件 + 一个备份，只按字节上限约束：总量上界就是
    /// [`LOG_MAX_BYTES`] + 修剪后的尾部，一眼能算清，也不需要清理任务。
    pub fn append_log(&self, line: &str) -> Result<()> {
        append_log_line(&self.logs_dir(), line)
    }

    /// 删掉全部日志文件（活动文件与备份）。
    ///
    /// 「清空」按钮必须清掉**它显示的那些东西**。`tail_logs` 优先读文件，
    /// 所以只清内存缓冲的话，界面刷新一次日志就全回来了 —— 那不是清空，
    /// 是把用户当傻子。
    pub fn clear_logs(&self) -> Result<()> {
        let dir = self.logs_dir();
        for name in log_file_names() {
            let p = dir.join(name);
            if p.exists() {
                std::fs::remove_file(&p)
                    .map_err(|e| Error::Store(format!("删除 {} 失败: {e}", p.display())))?;
            }
        }
        Ok(())
    }

    /// 读最近 `limit` 条日志（按时间从旧到新），跨轮转。
    ///
    /// 语义与 [`Store::tail_logs_with_stats`] 完全相同，只是丢掉统计 ——
    /// **新调用方请用带统计的那个**，否则「读的时候丢了多少」又变得看不见。
    pub fn tail_logs<T: serde::de::DeserializeOwned>(&self, limit: usize) -> Vec<T> {
        self.tail_logs_with_stats(limit).0
    }

    /// 读最近 `limit` 条日志，**并把「读的时候发生了什么」一并返回**。
    ///
    /// # 为什么不再「逐行 `from_str` + `.ok()`」（task-107）
    ///
    /// 旧实现是 `text.lines().filter_map(|l| from_str::<T>(l).ok())`：一行里若有
    /// **两个 JSON 对象**（task-104 实测 4 行，错误原文 `trailing characters at
    /// line 1 column 14`），整行解析失败 ⇒ **那一行里的两条记录一起被静默丢掉**。
    /// 界面日志页与诊断报告都走这条路，于是「全天零自动恢复」这个错觉
    /// **在用户界面里也成立** —— 两边都靠原始文本 grep 才发现。
    ///
    /// 现在：① 每行按「**可能含多个对象**」解析（与 `scripts/net-metrics.py` 的
    /// `raw_decode` 循环同口径，用 `serde_json` 的流式解析器实现）；
    /// ② 丢了多少**说得出来**（[`TailLogStats`]），不再有静默丢弃。
    ///
    /// # 既有语义**刻意未变**
    ///
    /// * `limit` 仍是「最近 limit 条」、返回仍是**从旧到新**；
    /// * 仍是**活动文件优先**（备份在前、活动在后，拼起来才是时间顺序），
    ///   且活动文件够数就提前 `break`（备份没被读 —— 统计里的 `files_read`
    ///   会把这件事如实说出来，别把它读成「备份丢了」）；
    /// * 跨代重叠（轮转把尾部复制进两代）**仍然不去重** —— 那是 task-105 的事，
    ///   本卡不动它。
    pub fn tail_logs_with_stats<T: serde::de::DeserializeOwned>(
        &self,
        limit: usize,
    ) -> (Vec<T>, TailLogStats) {
        let dir = self.logs_dir();
        let mut out: Vec<T> = Vec::new();
        let mut stats = TailLogStats::default();
        // 备份在前、活动文件在后 —— 这样拼出来就是时间顺序。
        for name in log_file_names().iter().rev() {
            let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
                stats.files_unreadable += 1;
                continue;
            };
            stats.files_read += 1;
            let mut batch: Vec<T> = Vec::new();
            for (idx, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    stats.empty_lines += 1;
                    continue;
                }
                stats.lines += 1;
                // **一行可能含多个对象**：流式解析逐个取，取到失败为止。
                // 取到几个算几个 —— 前半段合法、后半段残缺的行也能救回前半段。
                let mut parsed_here = 0usize;
                let mut failed = false;
                for item in serde_json::Deserializer::from_str(line).into_iter::<T>() {
                    match item {
                        Ok(v) => {
                            batch.push(v);
                            parsed_here += 1;
                        }
                        Err(_) => {
                            failed = true;
                            break;
                        }
                    }
                }
                stats.records += parsed_here;
                if parsed_here > 1 {
                    stats.multi_object_lines += 1;
                }
                if failed {
                    stats.malformed_lines += 1;
                    if stats.bad_lines.len() < MAX_BAD_LINE_SAMPLES {
                        stats.bad_lines.push(format!("{name}:{}", idx + 1));
                    }
                }
            }
            batch.extend(out);
            out = batch;
            if out.len() >= limit {
                break;
            }
        }
        let skip = out.len().saturating_sub(limit);
        stats.truncated = skip;
        if stats.malformed_lines > 0 {
            // 注意：tracing 走 **stderr**（`lib.rs` 的 `with_writer(std::io::stderr)`），
            // GUI 从 Finder 启动时看不到 —— 所以它**不是**用户可见的替代品，
            // 用户可见靠调用方把 `TailLogStats` 显示出来。这里留一份给开发/终端。
            tracing::warn!(
                files_read = stats.files_read,
                lines = stats.lines,
                records = stats.records,
                multi_object_lines = stats.multi_object_lines,
                malformed_lines = stats.malformed_lines,
                where = ?stats.bad_lines,
                "读取日志时有无法解析的行（不再静默丢弃，见 store::TailLogStats）"
            );
        }
        (out.split_off(skip), stats)
    }
}

/// 日志读取统计（task-107）：**丢了多少必须说得出来**。
///
/// 字段刻意分成「读到了什么」与「丢掉了什么」两组 —— 后者此前根本不存在，
/// 于是任何丢行都只能靠人拿原始文本 grep 才发现。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailLogStats {
    /// 实际读到（打开成功）的文件数。跨代读取有提前退出，所以不一定是 2。
    pub files_read: usize,
    /// 打开失败的文件数（不存在 / 没权限）。
    pub files_unreadable: usize,
    /// 读到的非空行数。
    pub lines: usize,
    /// 空行数（跳过；**不计入 `records`**）。
    pub empty_lines: usize,
    /// 解析出的**对象**总数 —— 多对象行会贡献 >1（这正是旧实现丢掉的东西）。
    pub records: usize,
    /// 含 ≥2 个对象的行数。
    pub multi_object_lines: usize,
    /// 至少有一个对象解析失败的行数（**这些行不再静默**）。
    pub malformed_lines: usize,
    /// 因 `limit` 被丢掉的条数（**不是**解析失败，别混为一谈）。
    pub truncated: usize,
    /// 前几条坏行的 `文件名:行号`（最多 [`MAX_BAD_LINE_SAMPLES`] 条），给日志与报告引用。
    pub bad_lines: Vec<String>,
}

impl TailLogStats {
    /// 有没有**无法解析**的内容（= 曾经的静默丢弃）。
    pub fn has_loss(&self) -> bool {
        self.malformed_lines > 0
    }

    /// 一行给用户/报告看的摘要。
    pub fn summary(&self) -> String {
        format!(
            "读取：{} 个文件 / {} 行（空行 {}）/ {} 条记录；多对象行 {}、**无法解析的行 {}**、因 limit 截断 {}",
            self.files_read,
            self.lines,
            self.empty_lines,
            self.records,
            self.multi_object_lines,
            self.malformed_lines,
            self.truncated
        )
    }
}

/// 坏行最多留几条样本（够定位即可，避免统计本身变成内存与日志噪声）。
const MAX_BAD_LINE_SAMPLES: usize = 3;

/// 活动日志文件与备份的文件名（顺序 = 时间顺序）。
///
/// 固定名字，不含日期：见 [`Store::append_log`] 里关于「为什么只有一个文件」的说明。
const LOG_FILE: &str = "app.jsonl";
const LOG_FILE_BAK: &str = "app.1.jsonl";

fn log_file_names() -> [&'static str; 2] {
    [LOG_FILE_BAK, LOG_FILE]
}

/// 活动文件超过它就修剪一次。
///
/// # 为什么是 128 MiB（task-144，实测定的）
///
/// 保证窗口 = `LOG_MAX_BYTES / 速率`（两代合起来 ≈ 一个 `MAX` 字节的流；
/// 「活跃 3.6 h + 备份 3 h」那种算法把两代**重复计了一次**）。按本机**只读实测**
/// 的速率（出处见 `docs/verification/NET-METRICS.md`）：
///
/// | 速率 | 旧值 10 MiB | **新值 128 MiB** |
/// |---|---|---|
/// | 63 MiB/天（task-121） | 3.8 h | 48.8 h |
/// | 116 MiB/天（09-22 会话） | 2.1 h | 26.4 h |
/// | 312 MiB/天（09-23 会话） | 0.8 h | 9.8 h |
/// | 612 MiB/天（峰值小时外推） | 0.4 h | 5.0 h |
///
/// 磁盘上界从「**其实不存在**」（见 [`append_log_line_existing`] 的历史）变成
/// `2×MAX − KEEP` ≈ 160 MiB —— 只比修复前实测的 155.65 MiB 略多，但**是界的**。
const LOG_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// 修剪后保留的字节数（见 [`Store::append_log`] 的说明）。
///
/// **判据是字节，不是行数。** 早先触发看字节、放弃看行数，于是「行很大、
/// 行数不多」时会出现：每次跨过阈值都白做一遍破坏性重命名，然后什么都不修剪 ——
/// 文件于是无界增长，而备份被反复churn。（这是审查指出的，测试也复现了。）
///
/// 取 `MAX × 3/4`：`2×MAX − KEEP = 1.25×MAX` 就是磁盘上界，同时也是
/// 「下次修剪前」能回溯的最大窗口。
const LOG_KEEP_BYTES: u64 = LOG_MAX_BYTES / 4 * 3;

/// 高频路径（[`append_log_line_existing`]）**每累计写入这么多字节才查一次**文件大小。
///
/// # 为什么不是每行都查
///
/// 那条路径存在的意义就是少做系统调用（见其注释）。改成「按字节预算查」之后，
/// **超出上界仍然是可算的**：
///
/// ```text
/// 文件大小 ≤ LOG_MAX_BYTES + LOG_CHECK_EVERY_BYTES + 单行最大字节
/// ```
///
/// 依据：检查发生在**写入之前**；两次检查之间最多再写进一个预算周期，
/// 而触发检查的那一行本身也可能是「一整个周期都装不下」的大行。
/// 本机实测单行最大 **1294 B**（两代合计 819,256 行样本），所以超出量 ≈ 1 MiB + 1.3 KiB。
/// ⚠️ 若将来出现远大于本预算的单行记录，这个上界会**随之变大** ——
/// 对策见 `docs/verification/NET-METRICS.md`（只报不改）。
const LOG_CHECK_EVERY_BYTES: u64 = 1024 * 1024;

/// 距上次检查已写入的字节数（**进程内**）。日志写入者只有 App 一个进程，
/// 两个线程（核心 stdout 转发与看门狗）共用这个计数器。
static LOG_BYTES_SINCE_CHECK: AtomicU64 = AtomicU64::new(0);

/// 修剪互斥：两个写入者可能同时判定「该修剪了」。
///
/// 旧实现事实上「每个 App 进程只在第一条日志时修剪一次」
/// （见 [`append_log_line_existing`] 的说明），所以并发修剪从未暴露过；
/// 让高频路径也参与触发之后必须显式串行化 —— 否则两次修剪会互相覆盖
/// 对方写出的备份 / 收缩结果。
static LOG_TRIM_LOCK: Mutex<()> = Mutex::new(());

/// 追加一行日志到 `dir`（**唯一实现**）。
///
/// 状态层（`apps/desktop`）与 `Store` 都走这里，避免两处各写一份命名/轮转逻辑 ——
/// 那种重复一旦漂移，就会出现「界面里显示的日志」和「文件里的日志」对不上。
///
/// `line` **必须不含换行**：这是 JSONL 格式的要求，含换行的会被拆成多行，
/// 每一行都解析失败、被 `tail_logs` 静默丢掉。调用方负责转义
/// （见 `apps/desktop` 的 `AppState::log`）。
pub fn append_log_line(dir: &Path, line: &str) -> Result<()> {
    debug_assert!(
        !line.contains('\n'),
        "日志行不能含换行：JSONL 要求一行一条，含换行的会被拆散后静默丢弃"
    );
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Store(format!("创建日志目录失败: {e}")))?;
    let path = dir.join(LOG_FILE);
    // 低频路径：**每行都查**（行为与旧版一致）。
    trim_if_oversized(&path);
    append_line_to(&path, line)
}

/// 与 [`append_log_line`] 相同，但**假定目录已存在**，跳过 `create_dir_all`；
/// 并且按**字节预算**参与轮转触发（不是每行一次 `stat`）。
///
/// 给高频调用方用：核心日志多的时候每秒几十条，每条都做一次
/// 「建目录」系统调用是白花的。
///
/// # 这里修的是一个真实缺陷（task-144）
///
/// 本函数**曾经只 `append_line_to`** —— 没有 [`append_log_line`] 里的超限检查。
/// 而 `apps/desktop/src/state.rs` 在第一条日志之后**总是**走本函数
/// （`logs_dir_ready` 一旦置位就不再复位），于是实测：
///
/// * 生产里 `Store::append_log`（带检查）**没有任何调用方**（只有本文件测试用）；
/// * **每个 App 进程只在第一条日志时修剪一次**，会话内活动文件**无界增长**：
///   本机活动文件 67.59 MiB ≫ 旧上限 10 MiB，备份里躺着 88.07 MiB 的被丢弃头部
///   （⇒ 上次修剪时已涨到 ~96 MiB ≈ 9.6× 旧上限）；
/// * 本文件里「总量上界 = `LOG_MAX_BYTES` + 修剪后的尾部，一眼能算清」那句话
///   在那条路径上**不成立**（磁盘上界事实上不存在）。
///
/// 现在两条路径都参与触发；本路径用 [`LOG_CHECK_EVERY_BYTES`] 的字节预算控制
/// `stat` 频率，上界公式见该常量的文档。
pub fn append_log_line_existing(dir: &Path, line: &str) -> Result<()> {
    let path = dir.join(LOG_FILE);
    if log_check_due(line.len() as u64 + 1) {
        trim_if_oversized(&path);
    }
    append_line_to(&path, line)
}

/// 轮转参数（生产取 [`LOG_LIMITS`]；测试可临时改小，见 [`with_log_limits`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogLimits {
    max_bytes: u64,
    keep_bytes: u64,
}

const LOG_LIMITS: LogLimits = LogLimits {
    max_bytes: LOG_MAX_BYTES,
    keep_bytes: LOG_KEEP_BYTES,
};

/// 距上次检查累计写入 ≥ `every` ⇒ 本次该查文件大小了（并清零计数）。
///
/// 抽成纯函数（计数器注入）是为了可测：见
/// `the_bound_above_max_is_one_check_period_plus_one_line`。
fn log_check_due_with(counter: &AtomicU64, written: u64, every: u64) -> bool {
    let total = counter.fetch_add(written, Ordering::Relaxed) + written;
    if total < every {
        return false;
    }
    counter.store(0, Ordering::Relaxed);
    true
}

fn log_check_due(written: u64) -> bool {
    log_check_due_with(&LOG_BYTES_SINCE_CHECK, written, LOG_CHECK_EVERY_BYTES)
}

/// 活动文件超过上限就修剪。**两条写入路径共用这一处判据** ——
/// 曾经的问题正是「一条路径有检查、另一条完全没有」。
fn trim_if_oversized(path: &Path) {
    trim_if_oversized_with(path, log_limits());
}

fn trim_if_oversized_with(path: &Path, limits: LogLimits) {
    if log_file_len(path) <= limits.max_bytes {
        return;
    }
    // 拿锁之后再查一次：另一个线程可能已经修剪过。
    let _guard = LOG_TRIM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if log_file_len(path) > limits.max_bytes {
        trim_log_file(path, limits.keep_bytes);
    }
}

fn log_file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// 生产返回 [`LOG_LIMITS`]；测试里可被 [`with_log_limits`] 临时覆盖。
///
/// 覆盖的理由很实际：上限就是 128 MiB，而「跨过阈值 ⇒ 轮转」这类测试若真写
/// 192 MiB，既慢又费盘。覆盖只在测试构建里存在（生产分支编译期就被消掉）。
fn log_limits() -> LogLimits {
    #[cfg(test)]
    if let Some(override_limits) = TEST_LOG_LIMITS.with(|c| c.get()) {
        return override_limits;
    }
    LOG_LIMITS
}

#[cfg(test)]
thread_local! {
    static TEST_LOG_LIMITS: std::cell::Cell<Option<LogLimits>> = const { std::cell::Cell::new(None) };
}

/// 在 `f()` 期间使用更小的轮转参数；**退出（含 panic）时自动还原**。
///
/// `#[cfg(test)]` —— 生产二进制里没有这个入口。
#[cfg(test)]
fn with_log_limits<R>(limits: LogLimits, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<LogLimits>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LOG_LIMITS.with(|c| c.set(self.0.take()));
        }
    }
    let prev = TEST_LOG_LIMITS.with(|c| c.replace(Some(limits)));
    let _guard = Restore(prev);
    f()
}

/// 以 **0600** 追加多行（`text` 内部可含换行）。
///
/// **task-144 起只在测试构建里**：它只被 [`replace_log_file`]（等价性对照用的
/// 参考实现）用到；生产路径走 [`replace_log_file_with_lines`]（逐行、不 join）。
#[cfg(test)]
fn append_lines_to(path: &Path, text: &str) -> Result<()> {
    append_to(path, text)
}

/// 以 **0600** 追加一行。
///
/// 权限在**创建时**就指定，而不是先按 umask 建好再 chmod：后者会留下一个
/// 「真实内容已经写进去、但权限还没收紧」的窗口，而这正是同一文件里
/// `atomic_write` 已经处理对的事情。
fn append_line_to(path: &Path, line: &str) -> Result<()> {
    append_to(path, line)
}

fn append_to(path: &Path, text: &str) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| Error::Store(format!("打开日志失败: {e}")))?;
    write_record(&mut f, text).map_err(|e| Error::Store(format!("写日志失败: {e}")))
}

/// 写一条记录：**一次 `write_all` 写完 `<text>\n`**。
///
/// # 为什么必须一次 write（task-104 实测出来的缺陷）
///
/// 原来是 `writeln!(f, "{text}")` —— 它展开成**两次** `write`：
/// `write_all(text)` + `write_all("\n")`。而日志是**两个写入者并发追加**
/// （核心 stdout 转发 `state.log("core", …)` 与看门狗 `state.log("app", …)`，
/// 见 `apps/desktop/src/commands/core.rs`），文件以 **O_APPEND** 打开。
/// O_APPEND 只保证**单次** write 的「定位 + 写入」原子，**两次之间可以被插进来**：
///
/// ```text
/// A: write("{app}")                 ← 还没写换行
/// B: write("{core}") write("\n")
/// A: write("\n")
/// ⇒ 文件里：{app}{core}\n   外加一个空行
/// ```
///
/// 本机 `app.jsonl` 实测到 4 行「一行两个 JSON 对象」（其中 2 行后面正好跟着
/// 一个空行 —— 这个指纹只有两次 write 交错能解释），直接导致 **3 条
/// 「隧道已自动恢复」在逐行解析的工具里静默消失**。
///
/// 一次 `write_all` 之后，O_APPEND 的原子性覆盖**整条记录** ⇒ 行不会交错；
/// 并顺带关掉「两次 write 之间进程崩掉 ⇒ 留下半行」的窗口。
///
/// 抽成独立函数是为了可测：测试用「计数 writer」断言**恰好 1 次 write**
/// （`a_log_record_is_written_with_exactly_one_write_call`）—— 那个属性是
/// **确定的**，不依赖调度；并发跑只能给出概率性的证据。
fn write_record<W: std::io::Write>(w: &mut W, text: &str) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(text.len() + 1);
    buf.extend_from_slice(text.as_bytes());
    buf.push(b'\n');
    w.write_all(&buf)
}

/// 把活动文件修剪到 `keep_bytes`：**被丢弃的头部进备份，活动文件只留尾部**。
///
/// 两代文件**不重叠**（task-105）：`app.1.jsonl` 恰好是这次丢掉的那段（更旧），
/// `app.jsonl` 是保留的尾部（更新）；合起来正好是修剪前的全部内容，交集为空。
/// 这既让跨代读取不会重复，也让同样的两个文件装下更多**不同的**历史
/// （旧实现把整份旧文件留作备份、又把尾部复制一份 —— 一半容量白费在重复上）。
///
/// **按整行保留**（从后往前累加，直到超过目标字节）—— 按字节硬截会把一行 JSON
/// 切成两半，读回来那一行解析失败。「整行」的边界始终是 `\n`，
/// **不因为「一行里可能含多个对象」而改变**：切了 `\n` 同样会把一个对象切成两半。
///
/// 顺序：**先写备份、再收缩活动文件**（理由见函数体内注释，不许颠倒）。
///
/// ⚠️ **历史文件不会因此变干净**：旧版本留下的那两代文件里的重叠（本机实测
/// 39,931 行）仍在原地，`scripts/net-metrics.py` 的跨代去重**必须保留**。
fn trim_log_file(path: &Path, keep_bytes: u64) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = line_contents(&text).filter(|l| !l.trim().is_empty()).collect();
    let mut acc: u64 = 0;
    let mut keep_from = lines.len();
    for (i, l) in lines.iter().enumerate().rev() {
        acc += l.len() as u64 + 1;
        if acc > keep_bytes {
            break;
        }
        keep_from = i;
    }
    if keep_from == 0 || keep_from >= lines.len() {
        return; // 没有可丢的头部、或没有可留的尾部：不该走到这里
    }
    let Some(dir) = path.parent() else { return };
    // 顺序**不许颠倒**：
    // * 先备份：这步失败 ⇒ 活动文件一个字都没动 ⇒ **一条不丢**（只是仍重叠，可重试）；
    // * 若先收缩活动文件、而备份写失败 ⇒ 那段头部**永久消失**。
    //
    // **逐行写、不 join**（task-144）：每行都是 `text` 的切片，输出与旧的
    // 「两段 join」**逐字节相同**（`zero_copy_trim_is_byte_identical_to_the_reference_implementation`）。
    if replace_log_file_with_lines(&dir.join(LOG_FILE_BAK), lines[..keep_from].iter().copied())
        .is_err()
    {
        return;
    }
    let _ = replace_log_file_with_lines(path, lines[keep_from..].iter().copied());
}

/// 与 `str::lines()` **同语义**的行切分：在 `\n` 分割；行尾 `\r\n` 去掉 `\r`；
/// 最后一行没有换行符也算一行；空串没有行。
///
/// 刻意用标准库的实现方式（`split_inclusive('\n')` + 去后缀），而不是自己写一遍扫描 ——
/// CRLF / 无尾换行的差异会精确地差一个字节，而这类差异正是等价性测试要抓的。
fn line_contents(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n').map(|line| match line.strip_suffix('\n') {
        Some(rest) => rest.strip_suffix('\r').unwrap_or(rest),
        None => line,
    })
}

/// 把一个**行序列**写成 `final_path`：先写同目录临时文件（**0600 在写入前就已收紧**），
/// 再 `rename` 覆盖 —— 同文件系统内的 `rename` 才是原子的。
///
/// # 为什么逐行写而不是 `join`（task-144）
///
/// 旧实现把丢弃段与保留段各自 `join("\n")` 成 `String`，再加上整份文本与
/// `write_record` 的整块拷贝 ⇒ 修剪时峰值内存 ≈ **2.8×`LOG_MAX_BYTES`**
/// （128 MiB 上限时约 360 MiB）。这里每行都是 `text` 的**切片**（不复制），
/// 写入走 8 KiB 的 `BufWriter` ⇒ 峰值 ≈ **1.08×`LOG_MAX_BYTES`**（约 138 MiB）：
/// 整份文本（128）+ 行索引（16 B/行 × ~64 万行 ≈ 10）+ 缓冲区（8 KiB）。
/// 输出精确到字节不变（有专门的等价性测试对照旧实现）。
fn replace_log_file_with_lines<'a>(
    final_path: &Path,
    lines: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let Some(dir) = final_path.parent() else {
        return Err(Error::Store(format!("{} 没有父目录", final_path.display())));
    };
    let name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("app.jsonl");
    let tmp = dir.join(format!("{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);

    if let Err(e) = write_lines(&tmp, lines) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Store(format!("写 {} 失败: {e}", tmp.display())));
    }
    std::fs::rename(&tmp, final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Store(format!("替换 {} 失败: {e}", final_path.display()))
    })
}

/// 逐行写 `<line>\n`（每行都是调用方的切片）到 `path`，权限 **0600**。
fn write_lines<'a>(path: &Path, lines: impl Iterator<Item = &'a str>) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 权限在**写入内容之前**就已收紧（与 `atomic_write` 同一条要求）。
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    let mut w = std::io::BufWriter::with_capacity(8 * 1024, file);
    for line in lines {
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
    }
    w.flush()
}

/// 原子替换一个日志文件：先写同目录临时文件（**0600**，权限在写入前就已收紧），
/// 再 `rename` 覆盖目标 —— 同文件系统内的 `rename` 才是原子的。
///
/// 为什么不「先 remove 再写」：那会留下「文件不存在」的窗口，读的人正好撞上就
/// 一条日志都读不到。
///
/// **task-144 起只在测试构建里**：它保留为 `task-105` 时代的**参考实现**，
/// 供 `zero_copy_trim_is_byte_identical_to_the_reference_implementation` 逐字节对照；
/// 生产路径改用 [`replace_log_file_with_lines`]（逐行写、不 join，峰值内存低得多）。
#[cfg(test)]
fn replace_log_file(final_path: &Path, text: &str) -> Result<()> {
    let Some(dir) = final_path.parent() else {
        return Err(Error::Store(format!("{} 没有父目录", final_path.display())));
    };
    let name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("app.jsonl");
    let tmp = dir.join(format!("{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = append_lines_to(&tmp, text.trim_end_matches('\n')) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Store(format!("替换 {} 失败: {e}", final_path.display()))
    })
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

    /// 修剪必须**按整行**，且不重复、不越界。
    ///
    /// 上一版这条测试是**假的**：它只写了 2050 行 ~50 字节的内容（约 100KB），
    /// 根本没跨过当时的触发线（10 MB），于是「轮转」从未发生 —— 断言在有没有
    /// 轮转时都通过。**task-144** 把上限抬到 128 MiB 之后，真写 192 MiB 太贵，
    /// 所以测试用 [`with_log_limits`] 把参数改小（语义完全一样）。
    #[test]
    fn log_trimming_keeps_whole_lines_without_duplication() {
        let limits = LogLimits { max_bytes: 32 * 1024, keep_bytes: 24 * 1024 };
        with_log_limits(limits, || log_trimming_with(limits));
    }

    /// [`log_trimming_keeps_whole_lines_without_duplication`] 的主体。
    fn log_trimming_with(limits: LogLimits) {
        let dir = std::env::temp_dir().join(format!("xt-logtrim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);

        // 写超过保留行数，触发一次修剪
        let total = 200;
        for i in 0..total {
            store
                .append_log(&format!(r#"{{"n":{i}}}"#))
                .unwrap();
        }

        // 触发条件是字节数，测试态下行很小，所以显式跨过一次阈值
        // 每条 ~256 字节，写够 1.5 倍阈值 —— 确保**确实**跨过触发线。
        // （上一版按 64 字节估算，实际写入量刚好卡在阈值下方，于是修剪从未发生。）
        let per_line = 256u64;
        for _ in 0..(limits.max_bytes * 3 / 2 / per_line + 2) {
            store.append_log(&format!(r#"{{"pad":"{}"}}"#, "x".repeat(236))).unwrap();
        }
        // 文件在 `root/logs/` 下，不是 root 本身 —— 早先这里读错了路径，
        // 于是断言一直在看一个不存在的文件（0 字节），把「测试写错」伪装成
        // 「实现没写」。
        let logs = store.logs_dir();
        assert!(
            logs.join(LOG_FILE_BAK).exists(),
            "跨过阈值后应当产生备份文件（说明修剪真的发生了）"
        );

        let back: Vec<serde_json::Value> = store.tail_logs(2000);
        assert!(!back.is_empty(), "修剪后不该一条都读不回来");
        // 每一条都是完整 JSON（被切半的行会在这里消失）
        assert!(back.iter().all(|v| v.is_object()), "修剪后出现残缺行");

        // **不重复**：早先的实现把整份旧文件留作备份、又把尾部复制进新文件，
        // 同一条日志会同时存在于两代里，跨代读取就会显示两遍。
        let seq: Vec<i64> = back.iter().filter_map(|v| v.get("n")?.as_i64()).collect();
        let mut sorted = seq.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seq.len(), "同一条日志被读到了两次");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「清空」必须连文件一起清 —— 否则界面刷新一次日志就全回来了。
    #[test]
    fn clear_logs_removes_files_too() {
        let dir = std::env::temp_dir().join(format!("xt-logclear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(&dir);
        store.append_log(r#"{"n":1}"#).unwrap();
        assert!(!store.tail_logs::<serde_json::Value>(10).is_empty());

        store.clear_logs().unwrap();
        assert!(
            store.tail_logs::<serde_json::Value>(10).is_empty(),
            "清空之后不该还能读到日志"
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

    /// **主守卫（task-104）**：一条记录必须**只做一次 `write`**，且以 `\n` 结尾。
    ///
    /// 为什么用「计数 writer」而不是并发跑：并发能不能撞上交错窗口**依赖调度**，
    /// 那种测试是概率性的；而「一条记录 = 一次 write」是**确定的**属性，
    /// 也正是 O_APPEND 的原子性能够覆盖整条记录的前提。
    ///
    /// **敏感性**：把 `write_record` 退回 `writeln!(w, "{text}")` ⇒ 计数变 2 ⇒ 本测试红。
    #[test]
    fn a_log_record_is_written_with_exactly_one_write_call() {
        #[derive(Default)]
        struct CountingWriter {
            writes: usize,
            bytes: Vec<u8>,
        }
        impl std::io::Write for CountingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut w = CountingWriter::default();
        write_record(&mut w, r#"{"n":1}"#).unwrap();
        assert_eq!(String::from_utf8(w.bytes.clone()).unwrap(), "{\"n\":1}\n");
        assert_eq!(
            w.writes, 1,
            "一条记录必须**一次 write** 写完：两次 write 之间会被另一个写入者插进来，\
             产生「一行两个 JSON 对象」（task-104 实测 4 行，吞掉 3 条自动恢复记录）"
        );

        // 多行文本（trim 之后整体追加）同样只写一次。
        let mut w = CountingWriter::default();
        write_record(&mut w, "a\nb").unwrap();
        assert_eq!(w.writes, 1, "多行记录也必须是单次 write");
        assert_eq!(String::from_utf8(w.bytes).unwrap(), "a\nb\n");
    }

    /// **产物级不变量（task-104，概率性）**：多线程并发追加之后，行数必须等于
    /// 写出的条数，且**每一行都能独立解析**。
    ///
    /// ⚠️ 诚实说明：这条是**概率性**的 —— 旧实现（每条两次 `write`）只有在两个
    /// 写入者刚好撞进中间那一步时才会红。它证明的是「修好之后产物是干净的」，
    /// **不能替代**上面那条确定性守卫（那条才是主守卫）。
    #[test]
    fn concurrent_appends_keep_every_record_on_its_own_line() {
        let store = temp_store("concurrent");
        let threads = 8usize;
        let per_thread = 200usize;
        std::thread::scope(|scope| {
            for t in 0..threads {
                let store = &store;
                scope.spawn(move || {
                    for i in 0..per_thread {
                        store
                            .append_log(&format!(r#"{{"t":{t},"i":{i}}}"#))
                            .expect("追加日志不该失败");
                    }
                });
            }
        });

        let text = std::fs::read_to_string(store.logs_dir().join("app.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            threads * per_thread,
            "行数必须等于写出的条数：多了 = 交错（一行两个对象），少了 = 记录被并进上一行"
        );
        for (n, line) in lines.iter().enumerate() {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(line) {
                panic!("第 {} 行不是合法 JSON（并发写入把两条记录挤到一行了）: {e}\n{line}", n + 1);
            }
        }
        let _ = std::fs::remove_dir_all(store.root());
    }

    // -----------------------------------------------------------------------
    // task-107：读侧不再静默丢行 —— 一行可能含多个对象，坏行必须说得出来
    // -----------------------------------------------------------------------

    /// **task-107 主回归**：一行两个对象 ⇒ **两条都要读到**。
    ///
    /// fixture 用的是**真实日志行的原文**（`app.jsonl:20724`，task-104 那 4 行之一：
    /// 一条 `app`「隧道已自动恢复」+ 一条 `core` 启动横幅）。旧实现
    /// （逐行 `from_str().ok()`）对整行报 `trailing characters` ⇒ **两条一起丢**，
    /// 于是那 3 次自愈在**界面日志页与诊断报告里也看不见**。
    ///
    /// **敏感性**：把解析退回「逐行 `from_str().ok()`」⇒ 本测试红（见 task-107 报告）。
    #[test]
    fn a_line_with_two_objects_yields_both_records() {
        let store = temp_store("tail-multi");
        let logs = store.logs_dir();
        std::fs::create_dir_all(&logs).unwrap();
        // 真实行原文（未做任何改写；最后再补一条正常行，证明坏行不影响上下文）。
        let doubled = concat!(
            r#"{"ts_unix":1790051357,"source":"app","level":"info","message":"隧道已自动恢复（第 1 次自动重建）"}"#,
            r#"{"ts_unix":1790051357,"source":"core","level":"info","message":"Xray 26.9.9 (Xray, Penetrates Everything.) 52a412d (go1.27.1 darwin/arm64)"}"#,
        );
        let next = r#"{"ts_unix":1790051358,"source":"app","level":"info","message":"下一条正常记录"}"#;
        std::fs::write(logs.join("app.jsonl"), format!("{doubled}\n{next}\n")).unwrap();

        let (got, stats) = store.tail_logs_with_stats::<serde_json::Value>(10);
        assert_eq!(got.len(), 3, "多对象行贡献 2 条 + 正常行 1 条：{got:?}");
        assert!(
            got.iter().any(|v| v["message"] == "隧道已自动恢复（第 1 次自动重建）"),
            "多对象行里的 app 记录必须读到（旧实现正是把它整行丢了）"
        );
        assert!(
            got.iter()
                .any(|v| v["source"] == "core" && v["message"].as_str().unwrap_or("").starts_with("Xray 26.9.9")),
            "同一行里的 core 横幅也要读到"
        );
        assert_eq!(stats.records, 3);
        assert_eq!(stats.multi_object_lines, 1, "要能说出「有 1 行含多个对象」");
        assert_eq!(stats.malformed_lines, 0, "多对象行**不是**坏行");
        assert!(!stats.has_loss());
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// 残缺行**不再静默**：计数 + 可定位，且**不影响后面的行**（也不丢同行前半段）。
    #[test]
    fn a_malformed_line_is_counted_and_later_lines_still_read() {
        let store = temp_store("tail-bad");
        let logs = store.logs_dir();
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("app.jsonl"),
            "{\"n\":1}\n{\"n\":2} {\"n\":99\"\n{\"n\":3}\n{ 这不是 JSON\n{\"n\":4}\n",
        )
        .unwrap();

        let (got, stats) = store.tail_logs_with_stats::<serde_json::Value>(10);
        let ns: Vec<i64> = got.iter().map(|v| v["n"].as_i64().unwrap_or(-1)).collect();
        assert_eq!(ns, vec![1, 2, 3, 4], "坏行不许吞掉其它行；第 2 行前半段合法 ⇒ 应救回 n=2");
        assert_eq!(stats.malformed_lines, 2, "第 2 行（尾部残缺）与第 4 行（完全不是 JSON）都要计数");
        assert_eq!(
            stats.bad_lines,
            vec!["app.jsonl:2".to_string(), "app.jsonl:4".to_string()],
            "坏行要能定位到 文件:行号"
        );
        assert!(stats.has_loss());
        let summary = stats.summary();
        assert!(summary.contains("无法解析的行 2"), "摘要要能给人看：{summary}");
        let _ = std::fs::remove_dir_all(store.root());
    }

    /// **既有语义不许变**：活动文件优先、够数就提前退出、从旧到新、limit 截断。
    ///
    /// 这三条正是 after 对照协议依赖的行为（`files_read` 把「备份读没读」如实说出来）。
    #[test]
    fn tail_logs_stats_document_early_break_and_truncation() {
        let store = temp_store("tail-break");
        let logs = store.logs_dir();
        std::fs::create_dir_all(&logs).unwrap();
        // 备份（旧）：1,2,3；活动（新）：4,5,6
        std::fs::write(logs.join("app.1.jsonl"), "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
        std::fs::write(logs.join("app.jsonl"), "{\"n\":4}\n{\"n\":5}\n{\"n\":6}\n").unwrap();

        // limit 小于活动文件条数 ⇒ 备份**根本没被读**（提前 break）
        let (small, s1) = store.tail_logs_with_stats::<serde_json::Value>(2);
        assert_eq!(small.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(s1.files_read, 1, "活动文件够数就该提前退出（备份没读 —— 别误读成「备份丢了」）");
        assert_eq!(s1.truncated, 1, "因 limit 丢 1 条，且要与「解析失败」分开报");

        // limit 更大 ⇒ 两个文件都读，仍是**从旧到新**
        let (big, s2) = store.tail_logs_with_stats::<serde_json::Value>(10);
        assert_eq!(big.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(s2.files_read, 2);
        assert_eq!(s2.truncated, 0);
        assert!(!s2.has_loss(), "业务上没问题时不许报「丢行」");
        let _ = std::fs::remove_dir_all(store.root());
    }

    // -----------------------------------------------------------------------
    // task-105：轮转后两代**不重叠**（并集无损 + 交集为空 + 跨代读不重复）
    // -----------------------------------------------------------------------

    /// 读出某个日志文件里的**记录** id 列表。
    ///
    /// 口径与 `tail_logs` 一致：**一行可能含多个对象** ⇒ 按记录收集，不是按行
    /// （按行算会把「一行两个对象」当 1 个元素，于是重叠可能假通过）。
    fn record_ids(store: &Store, name: &str) -> Vec<u64> {
        let Ok(text) = std::fs::read_to_string(store.logs_dir().join(name)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            for item in serde_json::Deserializer::from_str(line).into_iter::<serde_json::Value>() {
                match item {
                    Ok(v) => {
                        if let Some(n) = v["n"].as_u64() {
                            out.push(n);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        out
    }

    /// **task-105 主回归**：每次轮转后，两代文件必须**并集无损 + 交集为空**，
    /// 且跨代读取里**每条记录只出现一次**。
    ///
    /// 旧实现是「rename 整份活动文件 → 备份，再把尾部复制进新活动文件」⇒
    /// 那段尾部同时活在两代里（本机实测 39,931 行重叠），跨代读会**重复**
    /// （`tail_logs(limit > 活动文件行数)` 时真的能看到）。
    ///
    /// 三条断言**每次轮转后立即做**（不是跑完才比一次 —— 那样中间态会被掩盖）：
    /// ① 并集无损：轮转后的「活动 + 备份」== 轮转前的活动文件内容；
    /// ② 交集为空（新契约）；③ 用户可见：`tail_logs` 里没有重复记录。
    ///
    /// **敏感性**：把轮转改回「rename 整份 + 写尾部」⇒ 本测试红（见 task-105 报告）。
    #[test]
    fn each_rotation_is_lossless_disjoint_and_leaves_no_duplicates() {
        // task-144：上限抬到 128 MiB 之后，真堆 128 MiB 太贵 ⇒ 测试态用小参数
        // （`with_log_limits`）；「跨过阈值 ⇒ 轮转」的语义完全一样。
        let limits = LogLimits { max_bytes: 64 * 1024, keep_bytes: 48 * 1024 };
        with_log_limits(limits, || each_rotation_lossless_with(limits));
    }

    /// [`each_rotation_is_lossless_disjoint_and_leaves_no_duplicates`] 的主体。
    fn each_rotation_lossless_with(limits: LogLimits) {
        let store = temp_store("rotate");
        std::fs::create_dir_all(store.logs_dir()).unwrap();
        let mut next_id = 0u64;

        for round in 1..=2u32 {
            // 把活动文件堆到触发线以上（直接写盘：比 append_log 快几百倍）
            let mut text = String::new();
            while (text.len() as u64) <= limits.max_bytes {
                text.push_str(&format!("{{\"n\":{next_id}}}\n"));
                next_id += 1;
            }
            // 第 1 轮加一条脏行：一行两个对象（历史文件里真实存在这种行）
            if round == 1 {
                text.push_str(&format!("{{\"n\":{next_id}}}{{\"n\":{}}}\n", next_id + 1));
                next_id += 2;
            }
            std::fs::write(store.logs_dir().join(LOG_FILE), &text).unwrap();
            let before = record_ids(&store, LOG_FILE);

            // 再追加一条 ⇒ 触发这一轮轮转
            store.append_log(&format!(r#"{{"n":{next_id}}}"#)).unwrap();
            next_id += 1;

            let active = record_ids(&store, LOG_FILE);
            let backup = record_ids(&store, LOG_FILE_BAK);

            // ⓪ 并集里**不许有重复**：旧轮转语义（整份进备份 + 尾部复制）必然重复，
            //    而「按记录」的定义也要求每条只出现一次。放在最前面：它的失败信息
            //    最长也被我限成几条样本（否则 80 万个 id 会把输出淹掉）。
            let mut union = active.clone();
            union.extend(backup.iter().copied());
            let union_set: std::collections::BTreeSet<u64> = union.iter().copied().collect();
            if union_set.len() != union.len() {
                let dup = union.len() - union_set.len();
                panic!(
                    "第 {round} 次轮转后并集里有 {dup} 条重复（两代文件重叠 ⇒ 旧语义）—— \
                     跨代读取会把同一条记录显示两遍"
                );
            }

            // ① 并集无损：轮转后的「活动 + 备份」== 轮转前的活动文件内容
            let mut expected = before.clone();
            expected.push(next_id - 1);
            let expected_set: std::collections::BTreeSet<u64> = expected.iter().copied().collect();
            if union_set != expected_set {
                let missing: Vec<u64> = expected_set.difference(&union_set).take(5).copied().collect();
                let extra: Vec<u64> = union_set.difference(&expected_set).take(5).copied().collect();
                panic!(
                    "第 {round} 次轮转：并集与轮转前不一致 —— 少了 {} 条（例 {missing:?}）、多了 {} 条（例 {extra:?}）",
                    expected_set.difference(&union_set).count(),
                    union_set.difference(&expected_set).count()
                );
            }

            // ② 交集为空（新契约）
            let sa: std::collections::BTreeSet<u64> = active.iter().copied().collect();
            let sb: std::collections::BTreeSet<u64> = backup.iter().copied().collect();
            assert!(
                sa.is_disjoint(&sb),
                "第 {round} 次轮转后两代文件重叠了（task-105 修的就是这个）"
            );
            // 语义方向也要对：备份装的是**更旧**的那段
            assert!(
                sb.iter().max() < sa.iter().min(),
                "备份应当是被丢弃的**头部**（更旧），活动文件是保留的尾部（更新）"
            );

            // ③ 用户可见：跨代读一次，每条记录只出现一次
            let all: Vec<serde_json::Value> = store.tail_logs(usize::MAX);
            let ids: Vec<u64> = all.iter().filter_map(|v| v["n"].as_u64()).collect();
            let mut uniq = ids.clone();
            uniq.sort_unstable();
            uniq.dedup();
            assert_eq!(
                uniq.len(),
                ids.len(),
                "第 {round} 次轮转后跨代读取出现重复记录：{} 条里只有 {} 条不同",
                ids.len(),
                uniq.len()
            );
        }
        let _ = std::fs::remove_dir_all(store.root());
    }

    // -----------------------------------------------------------------------
    // task-144：轮转触发（高频路径曾完全绕过）+ 窗口预算
    // -----------------------------------------------------------------------

    /// 实测速率，单位 **MiB/天**。出处：`docs/verification/NET-METRICS.md`
    /// 「日志能回溯多久」一节（2026-09-23 20:13 只读扫描本机真实日志）。
    const RATE_TASK121: u64 = 63; // 41.7 MB / 0.63 天（task-121）
    const RATE_SESSION_1: u64 = 116; // 09-22 20:52 → 09-23 15:01（18.14 h，88.07 MiB）
    const RATE_SESSION_2: u64 = 312; // 09-23 15:01 → 20:13（5.20 h，67.59 MiB）
    const RATE_PEAK_HOUR: u64 = 612; // 峰值整点 25.5 MiB/h 外推
    const MIB: f64 = 1024.0 * 1024.0;

    /// 生产参数必须与文档里的表一致（**改参数就得重算窗口表**）。
    #[test]
    fn log_rotation_constants_match_the_documented_budget() {
        assert_eq!(LOG_MAX_BYTES, 128 * MIB as u64, "上限改了 ⇒ 文档窗口表要重算");
        assert_eq!(LOG_KEEP_BYTES, 96 * MIB as u64);
        assert_eq!(LOG_CHECK_EVERY_BYTES, MIB as u64);
        // 磁盘上界 = 2·MAX − KEEP（两代合计；它们不重叠，见 task-105）。
        assert_eq!(2 * LOG_MAX_BYTES - LOG_KEEP_BYTES, 160 * 1024 * 1024);
    }

    /// **窗口预算**：保证窗口 = `LOG_MAX_BYTES / 速率`。
    ///
    /// 为什么是除法、而不是「活跃 + 备份」相加：两代合起来约等于**一个** `MAX`
    /// 字节的流（`app.1.jsonl` 是被丢弃的头部、`app.jsonl` 是保留的尾部，两者不重叠）。
    /// 旧卡面把「活跃 3.6 h + 备份 3 h」相加，**把两代重复计了一次**。
    ///
    /// 目标：典型速率 ≥ 24 h；最坏实测会话 ≥ 8 h；峰值小时外推 ≥ 4 h。
    /// **反向敏感性**：`LOG_MAX_BYTES` 改回 10 MiB ⇒ 本测试红（第一档只剩 2.1 h）。
    #[test]
    fn log_budget_covers_the_target_window_at_the_measured_rates() {
        let window_hours =
            |rate_mib_per_day: u64| LOG_MAX_BYTES as f64 / (rate_mib_per_day as f64 * MIB / 24.0);
        for (rate, need_hours, label) in [
            (RATE_SESSION_1, 24.0, "典型速率（本机 09-22 会话）"),
            (RATE_SESSION_2, 8.0, "最坏实测会话（本机 09-23）"),
            (RATE_PEAK_HOUR, 4.0, "峰值小时外推"),
        ] {
            let got = window_hours(rate);
            assert!(
                got >= need_hours,
                "{label} {rate} MiB/天：只有 {got:.1} h < 目标 {need_hours} h（MAX={} MiB）",
                LOG_MAX_BYTES / 1024 / 1024
            );
        }
        // task-121 那个速率只登记、不设目标：防它被无意删掉（它是最保守的一段实测）。
        assert!(window_hours(RATE_TASK121) > 40.0);
    }

    /// **超出上界是可算的**：≤ 一个检查周期 + 单行最大字节；
    /// 并把这个「超出量」钉成远小于上限本身（否则「有界」就没意义）。
    ///
    /// 实现见 [`log_check_due_with`] 与 [`LOG_CHECK_EVERY_BYTES`] 的文档。
    #[test]
    fn the_bound_above_max_is_one_check_period_plus_one_line() {
        let counter = AtomicU64::new(0);
        let every = 1000u64;
        // 预算没满 ⇒ 不查
        assert!(!log_check_due_with(&counter, 400, every));
        assert!(!log_check_due_with(&counter, 400, every));
        // 满一个周期 ⇒ 查，并清零
        assert!(log_check_due_with(&counter, 400, every));
        assert!(!log_check_due_with(&counter, 400, every));
        // 单行就超过一个周期（大行）⇒ 也必须查，否则大行会让检查永远不触发
        assert!(log_check_due_with(&counter, every * 5, every));

        // 代入实测：单行最大 1294 B（两代 819,256 行样本），超出量 ≈ 1 MiB + 1.3 KiB。
        const MEASURED_MAX_LINE: u64 = 1294;
        let excess = LOG_CHECK_EVERY_BYTES + MEASURED_MAX_LINE;
        assert!(
            excess < LOG_MAX_BYTES / 100,
            "超出上界（{excess} B）必须远小于上限（{LOG_MAX_BYTES} B），否则「有界」名不副实"
        );
    }

    /// **新增覆盖（task-144）**：`append_log_line_existing`（高频路径，占实测日志行数的
    /// 97.7%–99.6%）也必须触发轮转 —— 在这之前**全仓没有任何测试碰过这个函数**，
    /// 而它正是「磁盘上界不存在」的那个入口。
    ///
    /// **敏感性**：把本函数里的 `if log_check_due(..) { trim_if_oversized(..) }` 删掉
    /// ⇒ 本测试红（备份文件不会出现）。
    #[test]
    fn the_high_frequency_path_rotates_too() {
        let dir = std::env::temp_dir().join(format!("xt-logfast-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 轮转参数改小（否则要真写 128 MiB），但**检查周期保持生产值** ——
        // 这条测试要证明的正是「按预算触发」在生产周期下也生效。
        let limits = LogLimits { max_bytes: 32 * 1024, keep_bytes: 24 * 1024 };
        let line = format!(r#"{{"pad":"{}"}}"#, "x".repeat(120));
        let per_line = line.len() as u64 + 1;
        let total_lines = LOG_CHECK_EVERY_BYTES / per_line + 8; // 跨过至少一个检查周期
        with_log_limits(limits, || {
            LOG_BYTES_SINCE_CHECK.store(0, Ordering::Relaxed);
            for _ in 0..total_lines {
                append_log_line_existing(&dir, &line).unwrap();
            }
        });

        let active = dir.join(LOG_FILE);
        assert!(
            dir.join(LOG_FILE_BAK).exists(),
            "高频路径跨过检查周期后必须产生备份（说明轮转真的发生了）"
        );
        let len = std::fs::metadata(&active).unwrap().len();
        let written = per_line * total_lines;
        assert!(
            len < written / 2,
            "活动文件没有被修剪：{len} B（一共写了 {written} B）"
        );
        assert!(
            len <= limits.max_bytes + LOG_CHECK_EVERY_BYTES + per_line,
            "超出上界应 ≤ 一个检查周期 + 单行：{len} B"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // task-144：零拷贝修剪的**等价性**证明（动的是 task-105/107 验证过的代码）
    // -----------------------------------------------------------------------

    /// `task-105` 时代的两段 `join` **参考实现** —— 逐字复制当时的生产逻辑，
    /// 只用于与新的零拷贝实现做逐字节对照（它用 [`replace_log_file`]，
    /// 那个函数同样只在测试构建里保留）。
    fn trim_log_file_reference(path: &Path, keep_bytes: u64) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut acc: u64 = 0;
        let mut keep_from = lines.len();
        for (i, l) in lines.iter().enumerate().rev() {
            acc += l.len() as u64 + 1;
            if acc > keep_bytes {
                break;
            }
            keep_from = i;
        }
        if keep_from == 0 || keep_from >= lines.len() {
            return;
        }
        let discarded = lines[..keep_from].join("\n") + "\n";
        let kept = lines[keep_from..].join("\n") + "\n";
        let Some(dir) = path.parent() else { return };
        if replace_log_file(&dir.join(LOG_FILE_BAK), &discarded).is_err() {
            return;
        }
        let _ = replace_log_file(path, &kept);
    }

    /// 每次对照用一个独立临时目录（同一个测试里要跑很多次）。
    fn trim_equivalence_dir() -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("xt-trimeq-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 把 `input` 写进活动文件、跑一次修剪，返回 `(备份字节, 活动文件字节)`。
    fn run_trim(f: fn(&Path, u64), input: &str, keep_bytes: u64) -> (Option<Vec<u8>>, Vec<u8>) {
        let dir = trim_equivalence_dir();
        let active = dir.join(LOG_FILE);
        std::fs::write(&active, input).unwrap();
        f(&active, keep_bytes);
        let got_active = std::fs::read(&active).unwrap();
        let got_backup = std::fs::read(dir.join(LOG_FILE_BAK)).ok();
        let _ = std::fs::remove_dir_all(&dir);
        (got_backup, got_active)
    }

    /// **task-144 硬要求**：零拷贝修剪与旧「两段 join」参考实现**逐字节相同**。
    ///
    /// 输入覆盖点名的边界：空文件 / 恰好等于上限 / 整行边界 / CRLF / 无尾换行 /
    /// 超长单行 / 一行多个对象 / 空行 / 纯空白行；外加一段**确定性伪随机**输入
    /// （含随机长度、随机行尾、整行空白）。
    #[test]
    fn zero_copy_trim_is_byte_identical_to_the_reference_implementation() {
        // 确定性伪随机（LCG，不用外部 crate）：200 行，长度/行尾/空白都随机会。
        let mut seed = 0x5DEE_CE66u64;
        let mut rnd = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        let mut random_input = String::new();
        for _ in 0..200 {
            let len = (rnd() % 40) as usize;
            if len == 0 {
                random_input.push('\n');
                continue;
            }
            if rnd() % 7 == 0 {
                // 整行空白（实现里会被过滤掉）
                random_input.push_str("   ".repeat(1 + (rnd() % 3) as usize).as_str());
            } else {
                for _ in 0..len {
                    random_input.push((b'a' + (rnd() % 26) as u8) as char);
                }
            }
            random_input.push_str(if rnd() % 3 == 0 { "\r\n" } else { "\n" });
        }

        let cases: Vec<(String, String, u64)> = vec![
            ("空文件".into(), String::new(), 64),
            ("只有换行".into(), "\n\n\n".into(), 64),
            ("一行无尾换行".into(), "a".into(), 64),
            ("恰好等于上限".into(), "0123456789\n".into(), 11),
            ("整行边界".into(), "aaaa\nbbbb\n".into(), 5),
            ("CRLF".into(), "a\r\nb\r\nc\r\n".into(), 4),
            ("无尾换行的多行".into(), "a\nb\nc".into(), 3),
            ("空行夹在中间".into(), "a\n\n\nb\n".into(), 2),
            (
                "超长单行".into(),
                format!("{}\n{}\n", "x".repeat(5000), "y".repeat(10)),
                8,
            ),
            ("多对象行".into(), "{\"n\":1}{\"n\":2}\n{\"n\":3}\n".into(), 17),
            ("只有空白行".into(), "   \n\t\n".into(), 4),
            ("伪随机 200 行（keep=64）".into(), random_input.clone(), 64),
            ("伪随机 200 行（keep=257）".into(), random_input, 257),
        ];

        for (label, input, keep_bytes) in cases {
            let got = run_trim(trim_log_file, &input, keep_bytes);
            let want = run_trim(trim_log_file_reference, &input, keep_bytes);
            assert_eq!(
                got, want,
                "输入「{label}」(keep={keep_bytes}) 下零拷贝实现与参考实现输出不同"
            );
            assert_eq!(
                got,
                run_trim(trim_log_file, &input, keep_bytes),
                "输入「{label}」下同一实现两次结果不同（不确定）"
            );
        }
    }
}
