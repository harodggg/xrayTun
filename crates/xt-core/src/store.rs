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
const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// 修剪后保留的字节数（见 [`Store::append_log`] 的说明）。
///
/// **判据是字节，不是行数。** 早先触发看字节、放弃看行数，于是「行很大、
/// 行数不多」时会出现：每次跨过阈值都白做一遍破坏性重命名，然后什么都不修剪 ——
/// 文件于是无界增长，而备份被反复churn。（这是审查指出的，测试也复现了。）
const LOG_KEEP_BYTES: u64 = LOG_MAX_BYTES / 5 * 4;

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
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > LOG_MAX_BYTES {
        trim_log_file(&path);
    }
    append_line_to(&path, line)
}

/// 与 [`append_log_line`] 相同，但**假定目录已存在**，跳过 `create_dir_all`。
///
/// 给高频调用方用：核心日志多的时候每秒几十条，每条都做一次
/// 「建目录」系统调用是白花的。
pub fn append_log_line_existing(dir: &Path, line: &str) -> Result<()> {
    let _ = dir;
    append_line_to(&dir.join(LOG_FILE), line)
}

/// 以 **0600** 追加多行（`text` 内部可含换行）。
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

/// 把活动文件修剪到 [`LOG_KEEP_BYTES`]：**被丢弃的头部进备份，活动文件只留尾部**。
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
fn trim_log_file(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut acc: u64 = 0;
    let mut keep_from = lines.len();
    for (i, l) in lines.iter().enumerate().rev() {
        acc += l.len() as u64 + 1;
        if acc > LOG_KEEP_BYTES {
            break;
        }
        keep_from = i;
    }
    if keep_from == 0 || keep_from >= lines.len() {
        return; // 没有可丢的头部、或没有可留的尾部：不该走到这里
    }
    let discarded = lines[..keep_from].join("\n") + "\n";
    let kept = lines[keep_from..].join("\n") + "\n";
    let Some(dir) = path.parent() else { return };
    // 顺序**不许颠倒**：
    // * 先备份：这步失败 ⇒ 活动文件一个字都没动 ⇒ **一条不丢**（只是仍重叠，可重试）；
    // * 若先收缩活动文件、而备份写失败 ⇒ 那段头部**永久消失**。
    if replace_log_file(&dir.join(LOG_FILE_BAK), &discarded).is_err() {
        return;
    }
    let _ = replace_log_file(path, &kept);
}

/// 原子替换一个日志文件：先写同目录临时文件（**0600**，权限在写入前就已收紧），
/// 再 `rename` 覆盖目标 —— 同文件系统内的 `rename` 才是原子的。
///
/// 为什么不「先 remove 再写」：那会留下「文件不存在」的窗口，读的人正好撞上就
/// 一条日志都读不到。
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
    /// 根本没跨过 10MB 的触发线，于是「轮转」从未发生 —— 断言在有没有轮转时
    /// 都通过。现在测试态下 `LOG_KEEP_LINES` 被调小（见该常量），
    /// 用很少的数据就能真正走一遍修剪。
    #[test]
    fn log_trimming_keeps_whole_lines_without_duplication() {
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
        for _ in 0..(LOG_MAX_BYTES * 3 / 2 / per_line + 2) {
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
        let store = temp_store("rotate");
        std::fs::create_dir_all(store.logs_dir()).unwrap();
        let mut next_id = 0u64;

        for round in 1..=2u32 {
            // 把活动文件堆到触发线以上（直接写盘：比 append_log 快几百倍）
            let mut text = String::new();
            while (text.len() as u64) <= LOG_MAX_BYTES {
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
}
