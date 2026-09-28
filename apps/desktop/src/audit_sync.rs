//! 审计自动同步的**桌面运行态**：偏好、密钥、定时检查与一次性执行。
//!
//! 判定、加密与协议全在 `xt-intent`（[`xt_intent::audit_sync`]）。这里只做
//! 桌面该做的那部分：**文件放哪、什么时候跑、把结果说成一句人话**。
//!
//! # 四个文件（都在数据目录，0600）
//!
//! | 文件 | 内容 | 谁写 |
//! | --- | --- | --- |
//! | `intent-audit-sync.json` | 进度：设备 id + 已上传到哪一天 | 本模块**与 `intent_audit` CLI 共用同一份** |
//! | `audit-sync.json` | 偏好：`enabled` / `base_url` | 本模块 |
//! | `audit-sync.key` | 加密密钥（hex） | 本模块（首次开启生成） |
//! | `audit-sync.token` | 上传 token | 用户填 |
//!
//! 共用进度文件是**故意**的：命令行与 App 谁先跑都不会把对方已经传过的天再传一遍
//! （R2 key 由 device+day 决定，重传也只是覆盖）。
//!
//! # 为什么不是 Keychain（这是取舍，不是遗漏）
//!
//! `store.rs` 约定的是 `keychain:<service>/<account>` 引用，但**本仓库还没有 Keychain 实现**
//! —— `apps/desktop/src/intent.rs` 的模块头就写明 Jev 的 API Key 也还没落地。
//! 与其为了让日志好看而假装有 Keychain，不如把密钥放在数据目录里、
//! **和 `intent-audit.jsonl` 同级同权限（0600）**：
//! 审计文件本身就是明文域名，再加一个同目录同权限的密钥文件**没有扩大暴露面**
//! （同一个用户身份读得到两者）。代价写清楚：备份/迁移时这几个文件要一起带走；
//! 密钥丢了，已上传的密文就再也解不开（本机审计文件仍是第一副本）。
//!
//! # 隐私上的两条硬规则
//!
//! 1. **默认关闭**，而且偏好文件坏掉时**按关闭处理**（fail-closed）——
//!    一个读不懂的配置绝不能被解读成"用户同意上传"。
//! 2. 关闭状态下**一个请求都不发**（`sync_now` 第一件事就是看 `enabled`）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use xt_intent::audit_sync::{
    bundle_for_day, is_device_id, new_device_id, new_key, pending_days, read_audit_files,
    revoke_remote, sync_once, SyncConfig, SyncError, SyncState, DEFAULT_BASE_URL, KEY_LEN,
    MAX_CATCHUP_DAYS,
};
use xt_intent::audit_report::utc_day;
use xt_intent::transport::{parse_https_url, join_url, TlsTransport, Transport};

/// 偏好文件名。
pub const PREFS_FILE: &str = "audit-sync.json";
/// 进度文件名（与 CLI 共用）。
pub const STATE_FILE: &str = "intent-audit-sync.json";
/// 密钥文件名。
pub const KEY_FILE: &str = "audit-sync.key";
/// token 文件名。
pub const TOKEN_FILE: &str = "audit-sync.token";
/// 审计文件（当前 + 轮转）。
pub const AUDIT_FILE: &str = "intent-audit.jsonl";
pub const AUDIT_FILE_ROTATED: &str = "intent-audit.jsonl.1";

/// 检查间隔：30 分钟一次（没有待传的天就是空转，一次请求都不发）。
pub const SYNC_INTERVAL_SECS: u64 = xt_intent::audit_sync::CHECK_INTERVAL_SECS;
/// 启动后第一次检查的延迟：60 秒（别和启动窗口里的活抢）。
pub const FIRST_CHECK_DELAY_SECS: u64 = xt_intent::audit_sync::FIRST_CHECK_DELAY_SECS;

/// 偏好（`audit-sync.json`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncPrefs {
    pub v: u32,
    pub enabled: bool,
    pub base_url: String,
}

impl Default for SyncPrefs {
    fn default() -> Self {
        Self {
            v: 1,
            // **默认关闭**：开启代表数据离开设备，必须是显式同意。
            enabled: false,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }
}

/// 状态（给界面的那一份；字段名与 `apps/ui/src/types.ts` 逐字一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSyncStatus {
    pub enabled: bool,
    /// 未初始化时为 `""`（界面据此显示"还没生成"）。
    pub device: String,
    pub key_present: bool,
    pub token_present: bool,
    pub base_url: String,
    pub last_uploaded_day: Option<String>,
    pub last_ok_unix: Option<u64>,
    pub last_attempt_unix: Option<u64>,
    pub last_error: Option<String>,
    /// 待上传的天（升序，只含**已结束**的 UTC 天）。
    pub pending_days: Vec<String>,
    /// 因为太老被明说跳过、不再重试的天。
    pub skipped_days: Vec<String>,
    /// 退避中才有的下次可试时间。
    pub next_retry_unix: Option<u64>,
}

/// 一次同步的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSyncRun {
    pub uploaded: Vec<String>,
    pub error: Option<String>,
    pub status: AuditSyncStatus,
}

/// 「将要上传的内容」预览。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSyncPreview {
    pub day: Option<String>,
    pub rows: usize,
    pub bytes: usize,
    /// 明文 bundle（**本机数据**；预览不联网）。
    pub plaintext: Option<String>,
    /// 为什么没有内容（一句人话）。有内容时为 `None`。
    pub note: Option<String>,
}

/// 撤回结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSyncRevoke {
    pub deleted: u64,
    pub error: Option<String>,
}

/// 桌面运行态。
#[derive(Debug)]
pub struct AuditSyncRuntime {
    root: PathBuf,
    prefs: SyncPrefs,
    /// 本进程内最近一次执行的账。
    pub last_run: Option<AuditSyncRun>,
    /// 本进程内的错误（偏好坏掉、密钥读不出来……）。
    pub last_error: Option<String>,
}

impl AuditSyncRuntime {
    pub fn new(root: PathBuf) -> Self {
        let mut rt = Self {
            root,
            prefs: SyncPrefs::default(),
            last_run: None,
            last_error: None,
        };
        rt.reload_prefs();
        rt
    }

    // ---- 路径 --------------------------------------------------------------

    pub fn prefs_path(&self) -> PathBuf {
        self.root.join(PREFS_FILE)
    }
    pub fn state_path(&self) -> PathBuf {
        self.root.join(STATE_FILE)
    }
    pub fn key_path(&self) -> PathBuf {
        self.root.join(KEY_FILE)
    }
    pub fn token_path(&self) -> PathBuf {
        self.root.join(TOKEN_FILE)
    }
    pub fn audit_paths(&self) -> Vec<PathBuf> {
        vec![
            self.root.join(AUDIT_FILE),
            self.root.join(AUDIT_FILE_ROTATED),
        ]
    }

    // ---- 偏好 --------------------------------------------------------------

    /// 读偏好。**坏文件 = 默认（关闭）**，并把原因记进 `last_error`：
    /// 一个读不懂的配置绝不能被读成"用户同意上传"。
    fn reload_prefs(&mut self) {
        match std::fs::read_to_string(self.prefs_path()) {
            Ok(text) => match serde_json::from_str::<SyncPrefs>(&text) {
                Ok(p) => self.prefs = p,
                Err(e) => {
                    self.prefs = SyncPrefs::default();
                    self.last_error = Some(format!(
                        "审计同步偏好文件读不懂（{e}）：已按**关闭**处理，请重新设置"
                    ));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                self.last_error = Some(format!("审计同步偏好文件读不出来：{e}"));
            }
        }
        if self.prefs.base_url.trim().is_empty() {
            self.prefs.base_url = DEFAULT_BASE_URL.to_string();
        }
    }

    fn save_prefs(&mut self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.prefs)
            .map_err(|e| format!("偏好序列化失败：{e}"))?;
        write_secret(&self.prefs_path(), &text)
    }

    pub fn is_enabled(&self) -> bool {
        self.prefs.enabled
    }

    pub fn base_url(&self) -> &str {
        &self.prefs.base_url
    }

    // ---- 密钥 / token ------------------------------------------------------

    pub fn key_present(&self) -> bool {
        std::fs::read_to_string(self.key_path())
            .ok()
            .and_then(|t| xt_intent::audit_sync::hex_decode(t.trim()).ok())
            .map(|b| b.len() == KEY_LEN)
            .unwrap_or(false)
    }

    pub fn token_present(&self) -> bool {
        std::fs::read_to_string(self.token_path())
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false)
    }

    fn read_key(&self) -> Result<[u8; KEY_LEN], String> {
        let text = std::fs::read_to_string(self.key_path())
            .map_err(|e| format!("读密钥文件失败：{e}"))?;
        let bytes = xt_intent::audit_sync::hex_decode(text.trim()).map_err(|e| e.user_message())?;
        if bytes.len() != KEY_LEN {
            return Err(format!(
                "密钥文件里是 {} 字节，必须是 {KEY_LEN} 字节 —— 请删除它后重新开启（会生成新密钥，已上传的旧密文将无法解开）",
                bytes.len()
            ));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&bytes);
        Ok(k)
    }

    /// 没密钥就生成一个并落盘。返回 `(key, 是否新生成)`。
    fn ensure_key(&self) -> Result<([u8; KEY_LEN], bool), String> {
        if self.key_present() {
            return Ok((self.read_key()?, false));
        }
        let k = new_key().map_err(|e| e.user_message())?;
        write_secret(&self.key_path(), &xt_intent::audit_sync::hex_encode(&k))?;
        Ok((k, true))
    }

    fn read_token(&self) -> Result<String, String> {
        match std::fs::read_to_string(self.token_path()) {
            Ok(t) => Ok(t.trim().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("读 token 文件失败：{e}")),
        }
    }

    // ---- 状态 --------------------------------------------------------------

    /// 读进度文件。**坏文件返回 Err**（不静默换设备 id）—— 换 id 等于换一个
    /// 服务端前缀，旧对象既读不到也看不出来。
    fn read_state(&self) -> Result<Option<SyncState>, String> {
        SyncState::load(&self.state_path()).map_err(|e| e.user_message())
    }

    /// 没有就建一个（生成设备 id 并落盘）。
    fn ensure_state(&mut self) -> Result<SyncState, String> {
        match self.read_state() {
            Ok(Some(s)) => Ok(s),
            Ok(None) => {
                let device = new_device_id().map_err(|e| e.user_message())?;
                let s = SyncState::new(device);
                s.save(&self.state_path()).map_err(|e| e.user_message())?;
                Ok(s)
            }
            Err(e) => Err(e),
        }
    }

    // ---- 状态（给界面）-----------------------------------------------------

    pub fn status(&self) -> AuditSyncStatus {
        let state = match self.read_state() {
            Ok(s) => s,
            Err(e) => {
                // 不吞掉：设备 id 显示为空 + 错误上屏，让用户看到"这里有问题"。
                return AuditSyncStatus {
                    enabled: self.prefs.enabled,
                    device: String::new(),
                    key_present: self.key_present(),
                    token_present: self.token_present(),
                    base_url: self.prefs.base_url.clone(),
                    last_uploaded_day: None,
                    last_ok_unix: None,
                    last_attempt_unix: None,
                    last_error: Some(e),
                    pending_days: Vec::new(),
                    skipped_days: Vec::new(),
                    next_retry_unix: None,
                };
            }
        };

        let (pending, skipped) = match read_audit_files(&self.audit_paths()) {
            Ok(rows) => {
                let today = utc_day(xt_core::util::now_unix());
                let last = state.as_ref().and_then(|s| s.last_uploaded_day.clone());
                pending_days(&rows, last.as_deref(), &today, MAX_CATCHUP_DAYS)
            }
            // 审计文件读不出来：这不是"没有待传"，但我们也不编造待传天数。
            Err(_) => (Vec::new(), Vec::new()),
        };

        let (device, last_uploaded, last_ok, last_attempt, state_err, state_skipped, next_retry) =
            match state {
                Some(s) => {
                    let next = s.next_retry_unix();
                    (
                        s.device,
                        s.last_uploaded_day,
                        s.last_ok_unix,
                        s.last_attempt_unix,
                        s.last_error,
                        s.skipped_days,
                        next,
                    )
                }
                None => (
                    String::new(),
                    None,
                    None,
                    None,
                    None,
                    Vec::new(),
                    None,
                ),
            };

        AuditSyncStatus {
            enabled: self.prefs.enabled,
            device,
            key_present: self.key_present(),
            token_present: self.token_present(),
            base_url: self.prefs.base_url.clone(),
            last_uploaded_day: last_uploaded,
            last_ok_unix: last_ok,
            last_attempt_unix: last_attempt,
            // 本进程刚发生的错误优先（比如归档失败），否则用上次落盘的那个。
            last_error: self.last_error.clone().or(state_err),
            pending_days: pending,
            skipped_days: state_skipped,
            next_retry_unix: next_retry,
        }
    }

    // ---- 设置 --------------------------------------------------------------

    /// 开/关。**开启时会确保设备 id 与密钥都已生成**（否则"开着但没有密钥"
    /// 是一个永远失败的状态）；关闭时什么都不删。
    pub fn set_enabled(&mut self, enabled: bool) -> Result<AuditSyncStatus, String> {
        if enabled {
            let (_, fresh_key) = self.ensure_key()?;
            self.ensure_state()?;
            if fresh_key {
                // 这不是错误，但必须让用户知道要去备份。
                self.last_error = Some(
                    "已生成加密密钥（audit-sync.key，0600）。**请备份它**：丢了就再也解不开已上传的密文。"
                        .to_string(),
                );
            }
        }
        self.prefs.enabled = enabled;
        self.save_prefs()?;
        Ok(self.status())
    }

    /// 设置端点。空串 = 恢复默认。非 `https://` 一律拒绝（不许明文）。
    pub fn set_base_url(&mut self, url: &str) -> Result<AuditSyncStatus, String> {
        let trimmed = url.trim();
        let candidate = if trimmed.is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            trimmed.to_string()
        };
        parse_https_url(&join_url(&candidate, xt_intent::audit_sync::UPLOAD_PATH))
            .map_err(|e| format!("端点必须是一个可用的 https:// 地址：{e}"))?;
        self.prefs.base_url = candidate;
        self.save_prefs()?;
        Ok(self.status())
    }

    /// 设置上传 token（空串 = 清除）。
    pub fn set_token(&mut self, token: &str) -> Result<AuditSyncStatus, String> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            match std::fs::remove_file(self.token_path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("清除 token 失败：{e}")),
            }
            return Ok(self.status());
        }
        if trimmed.len() > 512 {
            return Err("token 太长了（>512 字符），是不是贴错了？".into());
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            // 头注入由传输层静默丢弃，那会变成"说不清的 401"；这里当场说清。
            return Err("token 里不能有空白或控制字符（复制时是不是多带了换行？）".into());
        }
        write_secret(&self.token_path(), trimmed)?;
        Ok(self.status())
    }

    // ---- 执行 --------------------------------------------------------------

    fn config(&mut self) -> Result<SyncConfig, String> {
        let (key, _) = self.ensure_key()?;
        let state = self.ensure_state()?;
        let token = self.read_token()?;
        Ok(SyncConfig {
            enabled: self.prefs.enabled,
            base_url: self.prefs.base_url.clone(),
            token,
            device: state.device,
            key,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// 跑一轮。**关闭时立刻返回、不发任何请求、不动状态。**
    ///
    /// 传输层是参数：生产用 [`TlsTransport`]，测试用假实现
    /// （`Transport` 这个接缝本来就是为这件事存在的）。
    pub fn sync_now_with(&mut self, transport: &dyn Transport) -> AuditSyncRun {
        let run = self.run_once(transport);
        self.last_run = Some(run.clone());
        match &run.error {
            Some(e) => self.last_error = Some(e.clone()),
            // 成功要**清掉**上一次的进程内错误，否则界面会一直挂着一条旧错误。
            None => self.last_error = None,
        }
        run
    }

    /// 生产入口。
    pub fn sync_now(&mut self) -> AuditSyncRun {
        self.sync_now_with(&TlsTransport::new())
    }

    fn run_once(&mut self, transport: &dyn Transport) -> AuditSyncRun {
        let empty = |status: AuditSyncStatus, error: Option<String>| AuditSyncRun {
            uploaded: Vec::new(),
            error,
            status,
        };

        if !self.prefs.enabled {
            // 关闭不是错误：调用方不该看到红字。
            return empty(self.status(), None);
        }
        let cfg = match self.config() {
            Ok(c) => c,
            Err(e) => {
                self.last_error = Some(e.clone());
                return empty(self.status(), Some(e));
            }
        };
        let mut state = match self.ensure_state() {
            Ok(s) => s,
            Err(e) => {
                self.last_error = Some(e.clone());
                return empty(self.status(), Some(e));
            }
        };
        let rows = match read_audit_files(&self.audit_paths()) {
            Ok(r) => r,
            Err(e) => {
                let msg = e.user_message();
                self.last_error = Some(msg.clone());
                return empty(self.status(), Some(msg));
            }
        };
        let now = xt_core::util::now_unix();
        let today = utc_day(now);
        let run = sync_once(
            &cfg,
            transport,
            &mut state,
            &self.state_path(),
            &rows,
            &today,
            now,
        );
        AuditSyncRun {
            uploaded: run.uploaded,
            error: run.error,
            status: self.status(),
        }
    }

    /// 「将要上传什么」：出**明文**（本机数据，不联网）。
    ///
    /// 还没开启过时用**临时设备 id**，并在 `note` 里说清楚 —— 为了让用户在同意
    /// 之前就能看到内容，而不是先开开关再看。
    pub fn preview(&self, day: Option<String>) -> AuditSyncPreview {
        let rows = match read_audit_files(&self.audit_paths()) {
            Ok(r) => r,
            Err(e) => {
                return AuditSyncPreview {
                    day: None,
                    rows: 0,
                    bytes: 0,
                    plaintext: None,
                    note: Some(e.user_message()),
                }
            }
        };
        let state = self.read_state().ok().flatten();
        let (device, temp_device) = match state.as_ref() {
            Some(s) => (s.device.clone(), false),
            None => match new_device_id() {
                Ok(d) => (d, true),
                Err(e) => {
                    return AuditSyncPreview {
                        day: None,
                        rows: 0,
                        bytes: 0,
                        plaintext: None,
                        note: Some(e.user_message()),
                    }
                }
            },
        };

        let today = utc_day(xt_core::util::now_unix());
        let day = match day {
            Some(d) => d,
            None => {
                let last = state.as_ref().and_then(|s| s.last_uploaded_day.clone());
                match pending_days(&rows, last.as_deref(), &today, MAX_CATCHUP_DAYS).0.last() {
                    Some(d) => d.clone(),
                    None => {
                        return AuditSyncPreview {
                            day: None,
                            rows: 0,
                            bytes: 0,
                            plaintext: None,
                            note: Some(format!(
                                "没有可预览的**已结束**的天（今天是 {today}；今天的数据要等它过完才会上传）"
                            )),
                        }
                    }
                }
            }
        };

        match bundle_for_day(&day, &device, env!("CARGO_PKG_VERSION"), &rows) {
            Some(b) => {
                let text = serde_json::to_string_pretty(&b).unwrap_or_default();
                AuditSyncPreview {
                    day: Some(day.clone()),
                    rows: b.counts.rows,
                    bytes: text.len(),
                    plaintext: Some(text),
                    note: if temp_device {
                        Some("还没开启过同步：上面用的是**临时**设备 id（开启时会生成并保存一个）".into())
                    } else {
                        None
                    },
                }
            }
            None => AuditSyncPreview {
                day: Some(day.clone()),
                rows: 0,
                bytes: 0,
                plaintext: None,
                note: Some(format!("{day} 没有记录")),
            },
        }
    }

    /// 撤回：让服务端删掉本设备全部已上传对象。**不动本机状态文件**
    /// （本机的审计与进度都不属于服务端）。
    pub fn revoke_with(&mut self, transport: &dyn Transport) -> AuditSyncRevoke {
        match self.revoke_inner(transport) {
            Ok(deleted) => AuditSyncRevoke {
                deleted,
                error: None,
            },
            Err(e) => {
                self.last_error = Some(e.clone());
                AuditSyncRevoke {
                    deleted: 0,
                    error: Some(e),
                }
            }
        }
    }

    pub fn revoke(&mut self) -> AuditSyncRevoke {
        self.revoke_with(&TlsTransport::new())
    }

    fn revoke_inner(&mut self, transport: &dyn Transport) -> Result<u64, String> {
        let state = self.ensure_state()?;
        let c = SyncConfig {
            enabled: true,
            base_url: self.prefs.base_url.clone(),
            token: self.read_token()?,
            device: state.device,
            // 撤回不需要密钥：这里不加密也不解密。填一个全零密钥是为了让
            // `SyncConfig` 这个类型保持"总是完整"（比给密钥开个 Option 更少分支）。
            key: [0u8; KEY_LEN],
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        c.ready().map_err(|e: SyncError| e.user_message())?;
        revoke_remote(transport, &c).map_err(|e| e.user_message())
    }
}

/// 写一个 0600 的文件（密钥、token、偏好都走它）。
///
/// 先写临时文件再 rename：避免"写了一半就被读到"—— 密钥被截断的话，
/// 之后每一次上传/解密都会莫名其妙地失败。
fn write_secret(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    // 「原名 + .tmp」而不是 `with_extension("tmp")`：后者会把 `audit-sync.key`、
    // `audit-sync.token`、`audit-sync.json` 全映射成同一个 `audit-sync.tmp`。
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
        use std::io::Write;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("落盘失败：{e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("换名失败：{e}"))?;
    Ok(())
}

/// 设备 id 是否可用（给界面做输入校验用）。
pub fn device_id_ok(s: &str) -> bool {
    is_device_id(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use xt_intent::audit::AuditRecord;
    use xt_intent::transport::{HttpRequest, HttpResponse, TransportError};

    struct FakeTransport {
        calls: Mutex<Vec<HttpRequest>>,
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl FakeTransport {
        fn ok() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(vec![HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: "{\"ok\":true,\"replaced\":false}".into(),
                }]),
            }
        }
        fn json(body: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(vec![HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into(),
                }]),
            }
        }
        fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Transport for FakeTransport {
        fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push(request.clone());
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(TransportError::Io("没有预置应答".into()));
            }
            Ok(q.remove(0))
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "xt-desktop-audit-sync-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn rec(ts: u64, host: &str) -> AuditRecord {
        AuditRecord {
            ts_unix: ts,
            host: host.into(),
            outcome: "block".into(),
            reason: None,
            category: Some("ad_or_monetization".into()),
            ads_intent: Some(0.97),
            risk_of_breakage: Some(0.05),
            choice_confidence: Some(0.93),
            effective_min: Some(0.85),
            applied: true,
            cache_hit: false,
            model: Some("jev-1.13-free".into()),
            usage: None,
            context_sent: None,
        }
    }

    /// 往数据目录写一条审计记录（天由调用方给出）。
    fn write_audit(root: &Path, rows: &[AuditRecord]) {
        let text: Vec<String> = rows
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        std::fs::write(root.join(AUDIT_FILE), format!("{}\n", text.join("\n"))).unwrap();
    }

    /// 一个已经配好的运行态：开启 + token + 一天待传的审计。
    fn ready(tag: &str) -> (AuditSyncRuntime, PathBuf) {
        let dir = tmpdir(tag);
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        rt.set_token("tok-abc").unwrap();
        // 2026-09-24 已经结束；今天按 2026-09-25 算。
        let t = 20_720 * 86_400;
        write_audit(&dir, &[rec(t, "ads.example")]);
        (rt, dir)
    }

    #[test]
    fn defaults_are_disabled_and_point_at_our_endpoint() {
        let dir = tmpdir("defaults");
        let rt = AuditSyncRuntime::new(dir);
        let s = rt.status();
        assert!(!s.enabled, "默认必须关闭");
        assert_eq!(s.base_url, DEFAULT_BASE_URL);
        assert_eq!(s.device, "", "还没生成设备 id");
        assert!(!s.key_present);
        assert!(!s.token_present);
        assert!(s.last_uploaded_day.is_none());
        assert!(s.pending_days.is_empty() || !s.enabled);
    }

    /// 偏好文件坏掉 ⇒ 按**关闭**处理（fail-closed），并且要说出来。
    /// 一个读不懂的配置绝不能被解读成"用户同意上传"。
    #[test]
    fn a_corrupt_prefs_file_disables_instead_of_enabling() {
        let dir = tmpdir("corrupt-prefs");
        std::fs::write(
            dir.join(PREFS_FILE),
            "{\"v\":1,\"enabled\":true,\"base_url\":\"不是 json 的一半",
        )
        .unwrap();
        let rt = AuditSyncRuntime::new(dir);
        assert!(!rt.is_enabled(), "坏文件必须按关闭处理");
        let s = rt.status();
        assert!(!s.enabled);
        assert!(
            s.last_error.unwrap().contains("关闭"),
            "必须把'按关闭处理'说出来"
        );
    }

    #[test]
    fn enabling_generates_a_device_and_a_key_then_persists_them() {
        let dir = tmpdir("enable");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        let s = rt.set_enabled(true).unwrap();
        assert!(s.enabled);
        assert!(is_device_id(&s.device), "{}", s.device);
        assert!(s.key_present, "开启时必须把密钥准备好");
        assert!(s.last_error.is_some(), "生成密钥要提醒用户备份");
        assert!(dir.join(KEY_FILE).exists());
        assert!(dir.join(STATE_FILE).exists());

        // 重新构造一个运行态：设备 id 与密钥必须**不变**
        let rt2 = AuditSyncRuntime::new(dir.clone());
        assert_eq!(rt2.status().device, s.device);
        assert!(rt2.key_present);

        // 再关一次：不删任何东西
        let s2 = rt.set_enabled(false).unwrap();
        assert!(!s2.enabled);
        assert!(dir.join(KEY_FILE).exists(), "关闭不该删密钥");
        assert!(dir.join(STATE_FILE).exists(), "关闭不该删进度");
    }

    #[test]
    fn a_bad_base_url_is_refused_and_a_good_one_persists() {
        let dir = tmpdir("baseurl");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        let err = rt.set_base_url("http://xraytun.top").unwrap_err();
        assert!(err.contains("https"), "{err}");
        assert_eq!(rt.base_url(), DEFAULT_BASE_URL, "非法值不许写进去");

        rt.set_base_url("https://audit.example").unwrap();
        assert_eq!(rt.base_url(), "https://audit.example");
        let rt2 = AuditSyncRuntime::new(dir);
        assert_eq!(rt2.base_url(), "https://audit.example", "必须落盘");

        // 空串 = 恢复默认
        let mut rt = rt2;
        rt.set_base_url("   ").unwrap();
        assert_eq!(rt.base_url(), DEFAULT_BASE_URL);
    }

    #[test]
    fn the_token_is_written_privately_and_can_be_cleared() {
        let dir = tmpdir("token");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        assert!(!rt.status().token_present);
        rt.set_token("tok-abc").unwrap();
        assert!(rt.status().token_present);
        assert_eq!(
            std::fs::read_to_string(dir.join(TOKEN_FILE)).unwrap().trim(),
            "tok-abc"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(TOKEN_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "token 必须 0600");
        }
        // 带换行/空格的 token 当场拒绝（否则会变成说不清的 401）
        assert!(rt.set_token("tok abc").is_err());
        assert!(rt.set_token("tok\nabc").is_err());
        // 清空
        rt.set_token("").unwrap();
        assert!(!rt.status().token_present);
        assert!(!dir.join(TOKEN_FILE).exists());
    }

    #[test]
    fn status_reports_pending_days_for_finished_days_only() {
        let dir = tmpdir("pending");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        // 09-23 / 09-24 已结束，09-25 是"今天"
        write_audit(
            &dir,
            &[
                rec(20_719 * 86_400, "a.example"),
                rec(20_720 * 86_400, "b.example"),
            ],
        );
        let s = rt.status();
        assert_eq!(s.pending_days.len(), 2, "{:?}", s.pending_days);
        assert!(s.pending_days.iter().all(|d| d.as_str() < &utc_day(xt_core::util::now_unix())));
    }

    /// 关闭状态：**一个请求都不发**，状态也不动。
    #[test]
    fn a_disabled_runtime_sends_nothing_at_all() {
        let dir = tmpdir("disabled");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_token("tok-abc").unwrap();
        let t = FakeTransport::ok();
        let run = rt.sync_now_with(&t);
        assert_eq!(t.calls(), 0, "关着的时候一个请求都不许发");
        assert!(run.uploaded.is_empty());
        assert!(run.error.is_none(), "关闭不是错误");
        assert!(!dir.join(STATE_FILE).exists(), "关着不该建进度文件");
    }

    /// 开着但没 token：说清原因，且一个请求都不发。
    #[test]
    fn an_enabled_runtime_without_a_token_says_so_and_sends_nothing() {
        let dir = tmpdir("notoken");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        write_audit(&dir, &[rec(20_720 * 86_400, "ads.example")]);
        let t = FakeTransport::ok();
        let run = rt.sync_now_with(&t);
        assert_eq!(t.calls(), 0);
        assert!(run.error.clone().unwrap().contains("token"), "{run:?}");
    }

    #[test]
    fn a_full_run_uploads_the_pending_day_and_advances_the_marker() {
        let (mut rt, dir) = ready("upload");
        let t = FakeTransport::ok();
        let run = rt.sync_now_with(&t);
        assert!(run.error.is_none(), "{run:?}");
        assert_eq!(run.uploaded, vec!["2026-09-24".to_string()]);
        assert_eq!(t.calls(), 1);
        assert_eq!(rt.status().last_uploaded_day.as_deref(), Some("2026-09-24"));
        assert!(rt.status().last_ok_unix.is_some());
        assert!(rt.status().pending_days.is_empty(), "传过就不该再待传");
        // 重放：不再产生请求（幂等）
        let t2 = FakeTransport::ok();
        let run2 = rt.sync_now_with(&t2);
        assert_eq!(t2.calls(), 0);
        assert!(run2.uploaded.is_empty());
        // 状态文件是**共用**的那份，CLI 也读得到
        let shared = SyncState::load(&dir.join(STATE_FILE)).unwrap().unwrap();
        assert_eq!(shared.last_uploaded_day.as_deref(), Some("2026-09-24"));
    }

    #[test]
    fn a_failed_upload_surfaces_the_error_and_keeps_the_marker() {
        let (mut rt, _dir) = ready("fail");
        let t = FakeTransport {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(vec![HttpResponse {
                status: 401,
                headers: Vec::new(),
                body: "{}".into(),
            }]),
        };
        let run = rt.sync_now_with(&t);
        assert!(run.uploaded.is_empty());
        let err = run.error.clone().unwrap();
        assert!(err.contains("401"), "{err}");
        let s = rt.status();
        assert_eq!(s.last_uploaded_day, None, "失败不许推进进度");
        assert!(s.last_error.clone().unwrap().contains("401"));
        assert!(s.next_retry_unix.is_some(), "失败后要进退避");
    }

    #[test]
    fn preview_shows_the_plaintext_and_strips_context_sent() {
        let dir = tmpdir("preview");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        let mut r = rec(20_720 * 86_400, "ads.example");
        r.context_sent = Some("用户正在看的页面内容".into());
        write_audit(&dir, &[r]);

        let p = rt.preview(Some("2026-09-24".into()));
        assert_eq!(p.day.as_deref(), Some("2026-09-24"));
        assert_eq!(p.rows, 1);
        let text = p.plaintext.clone().unwrap();
        assert!(text.contains("ads.example"), "预览要能让人看清会传什么");
        assert!(
            !text.contains("用户正在看的页面内容"),
            "开了'记录外发内容'也不该出现在预览里"
        );
        assert!(!text.contains("context_sent"));
        assert!(p.bytes > 0);
    }

    #[test]
    fn preview_before_enabling_uses_a_temporary_device_id_and_says_so() {
        let dir = tmpdir("preview-temp");
        let rt = AuditSyncRuntime::new(dir.clone());
        write_audit(&dir, &[rec(20_720 * 86_400, "ads.example")]);
        let p = rt.preview(None);
        assert_eq!(p.rows, 1);
        assert!(p.note.clone().unwrap().contains("临时"), "要说清这是临时设备 id");
        assert!(!dir.join(STATE_FILE).exists(), "预览不该写状态文件");
    }

    #[test]
    fn preview_without_any_finished_day_explains_why_it_is_empty() {
        let dir = tmpdir("preview-empty");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        // 只有"今天"的记录
        let now = xt_core::util::now_unix();
        write_audit(&dir, &[rec(now, "ads.example")]);
        let p = rt.preview(None);
        assert!(p.plaintext.is_none());
        assert!(p.note.clone().unwrap().contains("已结束"), "{p:?}");
    }

    #[test]
    fn a_corrupt_state_file_is_surfaced_instead_of_silently_rotating_the_device() {
        let dir = tmpdir("badstate");
        let mut rt = AuditSyncRuntime::new(dir.clone());
        rt.set_enabled(true).unwrap();
        let device = rt.status().device;
        std::fs::write(dir.join(STATE_FILE), "{ 这不是 json").unwrap();
        let s = rt.status();
        assert_eq!(s.device, "", "坏状态不许静默换一个设备 id");
        let err = s.last_error.unwrap();
        assert!(err.contains("解析失败"), "必须把问题说出来：{err}");
        // 也不许把原来的设备 id 悄悄覆盖掉（文件原样保留，等用户处理）
        assert_eq!(
            std::fs::read_to_string(dir.join(STATE_FILE)).unwrap(),
            "{ 这不是 json"
        );
        assert!(is_device_id(&device));
    }

    #[test]
    fn revoke_reports_what_the_server_confirmed() {
        let (mut rt, _dir) = ready("revoke");
        let t = FakeTransport::json("{\"ok\":true,\"deleted\":3}");
        let r = rt.revoke_with(&t);
        assert!(r.error.is_none(), "{r:?}");
        assert_eq!(r.deleted, 3);
        assert_eq!(t.calls(), 1);

        // 没 token 时拒绝，且不发请求
        let dir = tmpdir("revoke-notoken");
        let mut rt2 = AuditSyncRuntime::new(dir);
        rt2.set_enabled(true).unwrap();
        let t2 = FakeTransport::json("{\"ok\":true,\"deleted\":0}");
        let r2 = rt2.revoke_with(&t2);
        assert!(r2.error.is_some());
        assert_eq!(t2.calls(), 0);
    }

    #[test]
    fn write_secret_replaces_atomically_and_leaves_no_temp_behind() {
        let dir = tmpdir("atomic");
        let p = dir.join("secret");
        write_secret(&p, "v1").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v1");
        write_secret(&p, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2");
        assert!(!dir.join("secret.tmp").exists(), "临时文件必须被换名掉");
    }
}
