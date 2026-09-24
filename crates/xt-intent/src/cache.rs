//! 判决缓存：同一域名**只问一次**。
//!
//! # 缓存 key 里必须有模型与网关
//!
//! `jev-x-filter` 的缓存指纹（`pipeline.js:93-115`）把阈值、动作、范围、白名单、
//! 预算都算了进去，**唯独漏了 `model` / `baseURL` / `preset`** —— 于是换了模型之后，
//! 旧模型的判决会被继续复用。这里把它当成必须避免的缺陷：
//! 指纹由 [`fingerprint`] 统一计算，`model`、网关 `base_url`、问题措辞版本、
//! 阈值一起参与；**任何一项变化 ⇒ 整库作废**（不是逐条失效，因为无法判断哪些条会变）。
//!
//! # 落盘是原子的，且允许"读不出来"
//!
//! 缓存是**优化**，不是事实来源：读失败一律当作空缓存继续跑（fail-open），
//! 并把原因交给调用方去审计。绝不允许"缓存坏了 → 功能不可用"。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::verdict::Verdict;

/// 问题措辞的版本。**改任何一句 instructions 都要 +1** ——
/// 模型的答案分布会随之改变，旧判决不能复用。
pub const QUESTIONS_REVISION: u32 = 1;

/// 判决缓存的内容版本（结构变化时 +1）。
pub const CACHE_VERSION: u32 = crate::CACHE_FORMAT_VERSION;

/// 指纹：模型 + 网关 + 问题版本 + 阈值摘要。
///
/// 用 FNV-1a 64 而不是密码学哈希：这里要的是"变了就不同"，不是抗碰撞。
pub fn fingerprint(model: &str, base_url: &str, thresholds_repr: &str) -> String {
    let canonical = format!(
        "model={model}\nbase={base_url}\nquestions_rev={QUESTIONS_REVISION}\nthresholds={thresholds_repr}\n"
    );
    format!("{:016x}", fnv1a64(canonical.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// 一条缓存的判决。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub host: String,
    pub verdict: Verdict,
    /// 判决时刻（Unix 秒）。
    pub decided_at_unix: u64,
    /// 过期时刻（Unix 秒）。
    pub expires_at_unix: u64,
    /// 命中次数。界面上"这条判决省了多少次请求"就靠它。
    #[serde(default)]
    pub hits: u64,
    /// 造出这条判决的模型 id（审计与解释用）。
    #[serde(default)]
    pub model: Option<String>,
}

impl CacheEntry {
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at_unix
    }
}

/// 加载结果。**必须能区分"文件不存在"与"文件坏了/过期了"** ——
/// 前者是正常首次运行，后者要写进审计。
#[derive(Debug, Clone, PartialEq)]
pub enum CacheLoadOutcome {
    Missing,
    Loaded { entries: usize, dropped_expired: usize },
    /// 指纹不匹配（换了模型/网关/阈值/措辞）⇒ 整库作废。
    DiscardedFingerprint { stored: String },
    /// 内容版本不匹配。
    DiscardedVersion { stored: u32 },
    /// 读不出来（截断 / JSON 坏 / 权限）。
    DiscardedUnreadable { error: String },
}

impl CacheLoadOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Loaded { .. } => "loaded",
            Self::DiscardedFingerprint { .. } => "discarded_fingerprint",
            Self::DiscardedVersion { .. } => "discarded_version",
            Self::DiscardedUnreadable { .. } => "discarded_unreadable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    fingerprint: String,
    entries: Vec<CacheEntry>,
}

/// 内存 + 落盘的判决缓存。
#[derive(Debug, Clone)]
pub struct VerdictCache {
    fingerprint: String,
    entries: BTreeMap<String, CacheEntry>,
    max_entries: usize,
}

impl VerdictCache {
    pub fn new(fingerprint: impl Into<String>, max_entries: usize) -> Self {
        Self { fingerprint: fingerprint.into(), entries: BTreeMap::new(), max_entries: max_entries.max(1) }
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 取判决；过期即当作没有（并顺手删掉）。
    ///
    /// 取 `&mut self` 是为了累加 `hits` —— 这是"缓存省了多少请求"的唯一数据源。
    pub fn get(&mut self, host: &str, now: u64) -> Option<&CacheEntry> {
        let expired = self.entries.get(host).map(|e| e.is_expired(now)).unwrap_or(false);
        if expired {
            self.entries.remove(host);
            return None;
        }
        if let Some(e) = self.entries.get_mut(host) {
            e.hits = e.hits.saturating_add(1);
        }
        self.entries.get(host)
    }

    /// 只看不记命中（界面展示用）。
    pub fn peek(&self, host: &str, now: u64) -> Option<&CacheEntry> {
        self.entries.get(host).filter(|e| !e.is_expired(now))
    }

    pub fn put(&mut self, entry: CacheEntry) {
        self.entries.insert(entry.host.clone(), entry);
        self.evict_if_needed();
    }

    pub fn remove(&mut self, host: &str) -> Option<CacheEntry> {
        self.entries.remove(host)
    }

    /// 清空（用户点"清空缓存"）。
    pub fn clear(&mut self) -> usize {
        let n = self.entries.len();
        self.entries.clear();
        n
    }

    pub fn entries(&self) -> impl Iterator<Item = &CacheEntry> {
        self.entries.values()
    }

    /// 清掉过期项，返回条数。
    pub fn purge_expired(&mut self, now: u64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| !e.is_expired(now));
        before - self.entries.len()
    }

    /// 超出上限时按 `decided_at_unix` 从旧到新淘汰。
    ///
    /// 为什么按时间而不是按命中次数：命中最少的条目可能只是"最近才加进来"，
    /// 淘汰它会立刻重新花钱问一遍；而最旧的条目至少已经服务过一段时间。
    fn evict_if_needed(&mut self) {
        if self.entries.len() <= self.max_entries {
            return;
        }
        let mut keys: Vec<(u64, String)> =
            self.entries.values().map(|e| (e.decided_at_unix, e.host.clone())).collect();
        keys.sort_unstable();
        let overflow = self.entries.len() - self.max_entries;
        for (_, host) in keys.into_iter().take(overflow) {
            self.entries.remove(&host);
        }
    }

    /// 从磁盘加载。**不匹配就当作空库**，并把原因返回给调用方去审计。
    pub fn load(path: &Path, fingerprint: &str, max_entries: usize, now: u64) -> (Self, CacheLoadOutcome) {
        let mut cache = Self::new(fingerprint, max_entries);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (cache, CacheLoadOutcome::Missing)
            }
            Err(e) => {
                return (cache, CacheLoadOutcome::DiscardedUnreadable { error: e.to_string() })
            }
        };
        let file: CacheFile = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                return (cache, CacheLoadOutcome::DiscardedUnreadable { error: e.to_string() })
            }
        };
        if file.version != CACHE_VERSION {
            return (cache, CacheLoadOutcome::DiscardedVersion { stored: file.version });
        }
        if file.fingerprint != fingerprint {
            return (
                cache,
                CacheLoadOutcome::DiscardedFingerprint { stored: file.fingerprint },
            );
        }
        for e in file.entries {
            cache.entries.insert(e.host.clone(), e);
        }
        let dropped_expired = cache.purge_expired(now);
        cache.evict_if_needed();
        let entries = cache.entries.len();
        (cache, CacheLoadOutcome::Loaded { entries, dropped_expired })
    }

    /// 原子落盘（同目录临时文件 + rename），权限 0600。
    ///
    /// 目录不存在时先建 —— 首次运行不该因为"目录还没建"而丢缓存。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let file = CacheFile {
            version: CACHE_VERSION,
            fingerprint: self.fingerprint.clone(),
            entries: self.entries.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(path, &bytes)
    }

    /// 路径是否为"缓存文件"（调用方拼路径时用同一处定义，避免两处漂移）。
    pub fn default_path(data_root: &Path) -> PathBuf {
        data_root.join("intent-cache.json")
    }
}

/// 原子写：先写同目录 `.tmp`（**权限在写入内容之前就已收紧**），再 rename。
///
/// 与 `xt-core::store` 的做法一致，但刻意不引它：那个函数是私有的，
/// 而这里只是"一份 JSON 覆盖一份 JSON"，重复 15 行比引一个 crate 更划算
/// （同样的取舍在 `xt-core/src/util.rs` 的文档里已经写明了）。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{AllowReason, BlockVerdict, Category, DeferReason};

    fn block(host: &str, at: u64, expires: u64) -> CacheEntry {
        CacheEntry {
            host: host.into(),
            verdict: Verdict::Block(BlockVerdict {
                category: Category::AdOrMonetization,
                ads_intent: 0.97,
                risk_of_breakage: 0.05,
                choice_confidence: 0.93,
                effective_min: 0.85,
            }),
            decided_at_unix: at,
            expires_at_unix: expires,
            hits: 0,
            model: Some("jev-latest".into()),
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("xt-intent-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn fingerprint_changes_with_any_input() {
        let a = fingerprint("jev-latest", "https://api.typesafe.ai", "t1");
        assert_eq!(a, fingerprint("jev-latest", "https://api.typesafe.ai", "t1"), "必须确定性");
        assert_ne!(a, fingerprint("jev-1.13", "https://api.typesafe.ai", "t1"));
        assert_ne!(a, fingerprint("jev-latest", "https://opencode.ai/zen", "t1"));
        assert_ne!(a, fingerprint("jev-latest", "https://api.typesafe.ai", "t2"));
    }

    #[test]
    fn questions_revision_is_part_of_the_fingerprint() {
        // 这条断言的意义：改问题措辞时**必须**同时改 QUESTIONS_REVISION，
        // 否则指纹不变、旧判决会被继续用。测试通过"指纹包含版本号"来钉住这条纪律。
        let fp = fingerprint("m", "u", "t");
        let manual = format!(
            "model=m\nbase=u\nquestions_rev={QUESTIONS_REVISION}\nthresholds=t\n"
        );
        assert_eq!(fp, format!("{:016x}", fnv1a64(manual.as_bytes())));
    }

    #[test]
    fn get_expires_and_counts_hits() {
        let mut c = VerdictCache::new("fp", 10);
        c.put(block("a.example", 100, 200));
        assert!(c.get("a.example", 150).is_some());
        assert_eq!(c.peek("a.example", 150).unwrap().hits, 1);
        assert!(c.get("a.example", 200).is_none(), "到期即不可用");
        assert!(c.is_empty(), "过期项应被顺手删掉");
    }

    #[test]
    fn eviction_drops_the_oldest_decisions_first() {
        let mut c = VerdictCache::new("fp", 3);
        c.put(block("old.example", 100, 10_000));
        c.put(block("mid.example", 200, 10_000));
        c.put(block("new.example", 300, 10_000));
        c.put(block("newest.example", 400, 10_000));
        assert_eq!(c.len(), 3);
        assert!(c.peek("old.example", 500).is_none(), "最旧的应被淘汰");
        assert!(c.peek("newest.example", 500).is_some());
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = tmpdir("roundtrip");
        let path = VerdictCache::default_path(&dir);
        let mut c = VerdictCache::new("fp-A", 10);
        c.put(block("a.example", 100, 10_000));
        c.put(CacheEntry {
            host: "ok.example".into(),
            verdict: Verdict::Allow(AllowReason::CategoryNotBlockable { category: Category::CdnOrInfra }),
            decided_at_unix: 100,
            expires_at_unix: 10_000,
            hits: 7,
            model: Some("jev-latest".into()),
        });
        c.save(&path).unwrap();

        let (loaded, outcome) = VerdictCache::load(&path, "fp-A", 10, 5_000);
        assert_eq!(outcome, CacheLoadOutcome::Loaded { entries: 2, dropped_expired: 0 });
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.peek("ok.example", 5_000).unwrap().hits, 7);
        assert!(loaded.peek("a.example", 5_000).unwrap().verdict.is_block());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "缓存文件权限必须是 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_fingerprint_discards_everything() {
        let dir = tmpdir("fingerprint");
        let path = VerdictCache::default_path(&dir);
        let mut c = VerdictCache::new("fp-A", 10);
        c.put(block("a.example", 100, 10_000));
        c.save(&path).unwrap();

        let (loaded, outcome) = VerdictCache::load(&path, "fp-B", 10, 200);
        assert_eq!(outcome, CacheLoadOutcome::DiscardedFingerprint { stored: "fp-A".into() });
        assert!(loaded.is_empty(), "换了模型/网关/阈值之后旧判决一条都不能留");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_not_a_crash() {
        let dir = tmpdir("corrupt");
        let path = VerdictCache::default_path(&dir);
        std::fs::write(&path, b"{ this is not json").unwrap();
        let (loaded, outcome) = VerdictCache::load(&path, "fp", 10, 100);
        assert!(loaded.is_empty());
        assert_eq!(outcome.as_str(), "discarded_unreadable");

        // 文件不存在是**正常首次运行**，不是错误。
        let missing = dir.join("nope.json");
        let (_, outcome) = VerdictCache::load(&missing, "fp", 10, 100);
        assert_eq!(outcome, CacheLoadOutcome::Missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_entries_are_dropped_on_load() {
        let dir = tmpdir("expired");
        let path = VerdictCache::default_path(&dir);
        let mut c = VerdictCache::new("fp", 10);
        c.put(block("old.example", 100, 200));
        c.put(block("fresh.example", 100, 10_000));
        c.save(&path).unwrap();

        let (loaded, outcome) = VerdictCache::load(&path, "fp", 10, 5_000);
        assert_eq!(outcome, CacheLoadOutcome::Loaded { entries: 1, dropped_expired: 1 });
        assert!(loaded.peek("fresh.example", 5_000).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_creates_the_directory() {
        let dir = tmpdir("mkdir");
        let nested = dir.join("a").join("b");
        let path = VerdictCache::default_path(&nested);
        VerdictCache::new("fp", 10).save(&path).unwrap();
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn always_keeps_at_least_one_slot() {
        let mut c = VerdictCache::new("fp", 0);
        c.put(block("a.example", 1, 10));
        c.put(block("b.example", 2, 10));
        assert_eq!(c.len(), 1, "max_entries=0 也必须留一个位置，否则缓存永远为空");
    }

    #[test]
    fn deferred_verdicts_are_serializable_too() {
        let e = CacheEntry {
            host: "d.example".into(),
            verdict: Verdict::Deferred(DeferReason::GatewayUnavailable { message: "timeout".into() }),
            decided_at_unix: 1,
            expires_at_unix: 2,
            hits: 0,
            model: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: CacheEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }
}
