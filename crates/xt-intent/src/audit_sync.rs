//! 审计自动同步：把每天的审计**端到端加密**后送上我们自己的端点。
//!
//! # 三条不可动摇的规则
//!
//! 1. **服务端拿不到明文。** 明文 bundle 里是域名（= 浏览记录）。加密在设备上做，
//!    密钥只在设备上（Keychain）。R2 里只有密文 —— 所以"服务端可读"这个风险**根本不存在**，
//!    而不是"我们保证不读"。
//! 2. **幂等。** R2 key = `audit/<device>/<day>.json` ⇒ 同一天重传只覆盖同一 key，
//!    重试永远不可能产生重复数据。
//! 3. **默认关闭，关闭时零请求。** 未配置 token / 未开启 ⇒ **一个字节都不发**
//!    （有测试用假传输层断言 `calls == 0`）。
//!
//! # 只传"完整的天"
//!
//! `day < 今天(UTC)`。否则同一天会被反复上传，而且内容是半截的。
//! 关机 / 退出期间错过的天，会在下次启动时**按天补齐**（退避 + 上限见常量）。
//!
//! 设计全文见 `docs/design/AUDIT-SYNC.md`（契约在那，实现只按它写）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::audit::{AuditLog, AuditRecord};
use crate::audit_report::utc_day;
use crate::transport::{join_url, parse_https_url, HttpRequest, HttpResponse, Transport};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 密文信封版本（线上格式；改它必须同时改 Worker 与文档）。
pub const ENVELOPE_VERSION: u32 = 1;
/// 明文 bundle 版本。
pub const BUNDLE_VERSION: u32 = 1;
/// 明文 bundle 的类型标识（自查用：解密出来的东西得是它）。
pub const BUNDLE_KIND: &str = "xraytun.intent.audit.day";
/// 信封的算法名（线上格式的一部分，不要随手改字面量）。
pub const ALG: &str = "chacha20poly1305";
/// 设备 id：8 字节 = 16 个小写 hex 字符（**不是**硬件指纹、不含账号信息）。
pub const DEVICE_ID_BYTES: usize = 8;
/// 设备 id 的 hex 长度。
pub const DEVICE_ID_HEX_LEN: usize = DEVICE_ID_BYTES * 2;
/// 密钥长度（ChaCha20-Poly1305 固定 32 字节）。
pub const KEY_LEN: usize = 32;
/// 请求体上限（与 Worker 的 `MAX_BYTES` 一致：10 MiB）。
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// 默认端点（同 zone 路由；`*.workers.dev` 在大陆经常打不开）。
pub const DEFAULT_BASE_URL: &str = "https://xraytun.top";
/// 上传路径。
pub const UPLOAD_PATH: &str = "/api/audit";
pub const LIST_PATH: &str = "/api/audit/list";
pub const REVOKE_PATH: &str = "/api/audit/revoke";
/// 每次检查的间隔（App 运行时）。
pub const CHECK_INTERVAL_SECS: u64 = 30 * 60;
/// 启动后第一次检查的延迟（避开启动抢占；调用方再叠随机抖动）。
pub const FIRST_CHECK_DELAY_SECS: u64 = 60;
/// 失败退避：基础间隔。
pub const RETRY_BASE_SECS: u64 = 30 * 60;
/// 失败退避：上限（6 小时）。
pub const RETRY_MAX_SECS: u64 = 6 * 60 * 60;
/// 一次补齐最多上传这么多天（更老的天记进 `skipped_days`，明说跳过，不无限重试）。
pub const MAX_CATCHUP_DAYS: usize = 31;
/// 单次上传的总预算。
pub const UPLOAD_TIMEOUT_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// hex（不引 base64/hex crate：多两个依赖不如多 20 行）
// ---------------------------------------------------------------------------

/// 小写 hex。信封里所有二进制字段（nonce / 密文）都用它。
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// hex → 字节。长度为奇数或含非 hex 字符时**报错**（不猜、不跳过）。
pub fn hex_decode(s: &str) -> Result<Vec<u8>, SyncError> {
    if s.len() % 2 != 0 {
        return Err(SyncError::Crypto(format!("hex 长度是奇数（{}）", s.len())));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i])?;
        let lo = nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(c: u8) -> Result<u8, SyncError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(SyncError::Crypto(format!(
            "hex 里有非法字符：{:?}",
            c as char
        ))),
    }
}

// ---------------------------------------------------------------------------
// 设备 id 与密钥
// ---------------------------------------------------------------------------

fn random_bytes<const N: usize>() -> Result<[u8; N], SyncError> {
    let mut buf = [0u8; N];
    SystemRandom::new()
        .map_err(|_| SyncError::Crypto("拿不到系统随机数".into()))?
        .fill(&mut buf)
        .map_err(|_| SyncError::Crypto("随机数填充失败".into()))?;
    Ok(buf)
}

/// 新的设备 id（16 个小写 hex）。**只在首次开启时生成一次**，之后必须持久化 ——
/// 换 id 等于换一个 R2 前缀，旧对象再也不会被读到。
pub fn new_device_id() -> Result<String, SyncError> {
    Ok(hex_encode(&random_bytes::<DEVICE_ID_BYTES>()?))
}

/// 新的加密密钥（32 字节）。
pub fn new_key() -> Result<[u8; KEY_LEN], SyncError> {
    random_bytes::<KEY_LEN>()
}

/// 设备 id 形状（与服务端校验一致）。
pub fn is_device_id(s: &str) -> bool {
    s.len() == DEVICE_ID_HEX_LEN
        && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

// ---------------------------------------------------------------------------
// 明文 bundle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleCounts {
    pub rows: usize,
    pub block: usize,
    pub allow: usize,
    pub deferred: usize,
    pub cache_hit: usize,
    pub applied: usize,
}

/// 一天的明文 bundle。**这个结构不会离开设备**（它先被加密）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub v: u32,
    pub kind: String,
    pub day: String,
    pub device: String,
    pub app: AppInfo,
    pub counts: BundleCounts,
    pub rows: Vec<AuditRecord>,
}

/// 剥掉 `context_sent`。
///
/// **这是硬规则**：`context_sent` 里是用户正在访问的站点内容。即使本机开了
/// "记录外发内容"，也不因为开了同步就自动外发。⇒ 有测试钉住。
pub fn strip_context(rec: &AuditRecord) -> AuditRecord {
    let mut out = rec.clone();
    out.context_sent = None;
    out
}

/// 组装某一天的 bundle。该天没有记录时返回 `None`（**不上传空文件**）。
///
/// 确定性：同一批记录重复构建必须逐字节相同（否则"重传覆盖"就没有意义）。
pub fn bundle_for_day(
    day: &str,
    device: &str,
    app_version: &str,
    records: &[AuditRecord],
) -> Option<Bundle> {
    let mut rows: Vec<AuditRecord> = records
        .iter()
        .filter(|r| utc_day(r.ts_unix) == day)
        .map(strip_context)
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by_key(|r| r.ts_unix);
    let mut counts = BundleCounts {
        rows: rows.len(),
        block: 0,
        allow: 0,
        deferred: 0,
        cache_hit: 0,
        applied: 0,
    };
    for r in &rows {
        match r.outcome.as_str() {
            "block" => counts.block += 1,
            "allow" => counts.allow += 1,
            "deferred" => counts.deferred += 1,
            _ => {}
        }
        if r.cache_hit {
            counts.cache_hit += 1;
        }
        if r.applied {
            counts.applied += 1;
        }
    }
    Some(Bundle {
        v: BUNDLE_VERSION,
        kind: BUNDLE_KIND.to_string(),
        day: day.to_string(),
        device: device.to_string(),
        app: AppInfo {
            name: "XrayTun".to_string(),
            version: app_version.to_string(),
        },
        counts,
        rows,
    })
}

// ---------------------------------------------------------------------------
// 密文信封
// ---------------------------------------------------------------------------

/// 线上唯一格式。**明文头**里的字段是服务端可见的元数据（设备 id / 日期 / 行数 / 大小）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub alg: String,
    pub device: String,
    pub day: String,
    pub rows: usize,
    pub bytes: usize,
    pub nonce: String,
    pub ct: String,
}

/// 附加认证数据：把明文头**钉进认证范围**。
///
/// ⇒ 服务端（或中间人）改 `day`/`device`/`rows` 里的任何一个，解密必然失败。
/// 这比"加密了 body 但元数据可以随便改"强得多 —— 否则攻击者能把 9-24 的密文
/// 标成 9-25，让客户端误以为那天已经传过。
pub fn envelope_aad(device: &str, day: &str, rows: usize) -> String {
    format!("xraytun-audit-v{ENVELOPE_VERSION}|{device}|{day}|{rows}")
}

fn aead_key(key: &[u8; KEY_LEN]) -> Result<LessSafeKey, SyncError> {
    let unbound = UnboundKey::new(&CHACHA20_POLY1305, key.as_slice())
        .map_err(|_| SyncError::Crypto("密钥长度不对".into()))?;
    Ok(LessSafeKey::new(unbound))
}

/// 加密一天的 bundle → 信封。nonce 每次都新生成（同一密钥下绝不重用）。
pub fn encrypt_bundle(bundle: &Bundle, key: &[u8; KEY_LEN]) -> Result<Envelope, SyncError> {
    let plaintext = serde_json::to_vec(bundle)
        .map_err(|e| SyncError::Json(format!("bundle 序列化失败：{e}")))?;
    let nonce_bytes = random_bytes::<NONCE_LEN>()?;
    let aad = envelope_aad(&bundle.device, &bundle.day, bundle.rows.len());
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut in_out = plaintext.clone();
    aead_key(key)?
        .seal_in_place_append_tag(nonce, Aad::from(aad.as_bytes()), &mut in_out)
        .map_err(|_| SyncError::Crypto("加密失败".into()))?;
    Ok(Envelope {
        v: ENVELOPE_VERSION,
        alg: ALG.to_string(),
        device: bundle.device.clone(),
        day: bundle.day.clone(),
        rows: bundle.rows.len(),
        bytes: plaintext.len(),
        nonce: hex_encode(&nonce_bytes),
        ct: hex_encode(&in_out),
    })
}

/// 解密信封 → bundle。密钥不对、或明文头被改过，都会失败（统一的错误信息）。
pub fn decrypt_envelope(env: &Envelope, key: &[u8; KEY_LEN]) -> Result<Bundle, SyncError> {
    if env.v != ENVELOPE_VERSION {
        return Err(SyncError::Crypto(format!("信封版本不认识：{}", env.v)));
    }
    if env.alg != ALG {
        return Err(SyncError::Crypto(format!("信封算法不认识：{}", env.alg)));
    }
    let nonce_bytes = hex_decode(&env.nonce)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(SyncError::Crypto(format!(
            "nonce 长度是 {}，应该是 {NONCE_LEN}",
            nonce_bytes.len()
        )));
    }
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(&nonce_bytes);
    let aad = envelope_aad(&env.device, &env.day, env.rows);
    let mut ct = hex_decode(&env.ct)?;
    let key = aead_key(key)?;
    let plaintext = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce_arr),
            Aad::from(aad.as_bytes()),
            &mut ct,
        )
        .map_err(|_| {
            SyncError::Crypto(
                "解密失败：密钥不对，或 day/device/rows 被改过（它们都在认证范围里）".into(),
            )
        })?;
    let bundle: Bundle = serde_json::from_slice(plaintext)
        .map_err(|e| SyncError::Json(format!("解密出来的东西不是 bundle：{e}")))?;
    if bundle.kind != BUNDLE_KIND {
        return Err(SyncError::Json(format!(
            "解密出来的 kind 是 {:?}，期望 {BUNDLE_KIND:?}",
            bundle.kind
        )));
    }
    if bundle.day != env.day || bundle.rows.len() != env.rows {
        // 走到这里理论上不可能（它们在 AAD 里）；留着是为了"不依赖上一个结论也成立"。
        return Err(SyncError::Crypto(
            "解密结果与明文头不一致（day 或 rows 对不上）".into(),
        ));
    }
    Ok(bundle)
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SyncError {
    #[error("审计同步缺少配置：{0}")]
    NotConfigured(String),
    #[error("读写出错：{0}")]
    Io(String),
    #[error("JSON 出错：{0}")]
    Json(String),
    #[error("加密出错：{0}")]
    Crypto(String),
    #[error("传输出错：{0}")]
    Transport(String),
    #[error("服务端拒绝：401（上传 token 不对）")]
    Unauthorized,
    #[error("本次 bundle 太大（{0} 字节），超过上限")]
    TooLarge(usize),
    #[error("服务端限速：429（稍后重试）")]
    RateLimited,
    #[error("服务端返回 {status}：{body}")]
    Http { status: u16, body: String },
    #[error("状态文件坏了：{0} —— 拒绝继续，避免悄悄换一个设备 id")]
    BadState(String),
}

impl SyncError {
    /// 给界面看的一句话。**不含 token、不含域名。**
    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

// ---------------------------------------------------------------------------
// 配置与状态
// ---------------------------------------------------------------------------

/// 同步配置。密钥与 token 由调用方从 Keychain 取出来再传进来 ——
/// 这个模块**永远不碰密钥存储**，所以它可以被完整单测。
#[derive(Debug, Clone, PartialEq)]
pub struct SyncConfig {
    pub enabled: bool,
    pub base_url: String,
    /// 上传用的 bearer token（空 = 没配）。
    pub token: String,
    pub device: String,
    pub key: [u8; KEY_LEN],
    pub app_version: String,
}

impl SyncConfig {
    /// 只检查"能不能发请求"，不检查网络。
    pub fn ready(&self) -> Result<(), SyncError> {
        if !self.enabled {
            return Err(SyncError::NotConfigured("审计同步未开启".into()));
        }
        if self.token.trim().is_empty() {
            return Err(SyncError::NotConfigured(
                "没有上传 token（在 Worker 侧设 AUDIT_TOKEN，然后填到这里）".into(),
            ));
        }
        if !is_device_id(&self.device) {
            return Err(SyncError::NotConfigured(format!(
                "设备 id 形状不对：{:?}",
                self.device
            )));
        }
        parse_https_url(&join_url(&self.base_url, UPLOAD_PATH))
            .map_err(|e| SyncError::NotConfigured(format!("端点不是可用的 https 地址：{e}")))?;
        Ok(())
    }
}

/// 进度状态。放在 `intent-audit-sync.json`（与 `settings.json` 同级）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncState {
    pub v: u32,
    pub device: String,
    #[serde(default)]
    pub last_uploaded_day: Option<String>,
    #[serde(default)]
    pub last_attempt_unix: Option<u64>,
    #[serde(default)]
    pub last_ok_unix: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub skipped_days: Vec<String>,
}

impl SyncState {
    pub fn new(device: String) -> Self {
        Self {
            v: ENVELOPE_VERSION,
            device,
            last_uploaded_day: None,
            last_attempt_unix: None,
            last_ok_unix: None,
            last_error: None,
            consecutive_failures: 0,
            skipped_days: Vec::new(),
        }
    }

    /// 读状态。文件不存在 = `Ok(None)`（调用方此时才生成设备 id）。
    ///
    /// **坏文件是 `Err`，不是"当作没写过"** —— 否则我们会静默换一个设备 id，
    /// 而换 id 等于换一个 R2 前缀：旧数据既读不到，界面上也看不出来。
    pub fn load(path: &Path) -> Result<Option<Self>, SyncError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SyncError::Io(format!("读 {} 失败：{e}", path.display()))),
        };
        let state: SyncState = serde_json::from_str(&text).map_err(|e| {
            SyncError::BadState(format!("{} 解析失败：{e}", path.display()))
        })?;
        if !is_device_id(&state.device) {
            return Err(SyncError::BadState(format!(
                "{} 里的设备 id 形状不对：{:?}",
                path.display(),
                state.device
            )));
        }
        Ok(Some(state))
    }

    /// 落盘（0600）。
    pub fn save(&self, path: &Path) -> Result<(), SyncError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| SyncError::Io(format!("建目录 {} 失败：{e}", dir.display())))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| SyncError::Json(format!("状态序列化失败：{e}")))?;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(path)
            .map_err(|e| SyncError::Io(format!("写 {} 失败：{e}", path.display())))?;
        use std::io::Write;
        f.write_all(text.as_bytes())
            .map_err(|e| SyncError::Io(format!("写 {} 失败：{e}", path.display())))?;
        Ok(())
    }

    /// 退避中时返回"下次可以再试"的时间。
    pub fn next_retry_unix(&self) -> Option<u64> {
        if self.consecutive_failures == 0 {
            return None;
        }
        Some(next_retry_after(
            self.last_attempt_unix.unwrap_or(0),
            self.consecutive_failures,
        ))
    }

    /// 现在允许尝试吗（退避未到就不许）。
    pub fn retry_allowed(&self, now_unix: u64) -> bool {
        match self.next_retry_unix() {
            None => true,
            Some(at) => now_unix >= at,
        }
    }
}

/// 退避：`30min × 2^(n-1)`，上限 6 小时。
pub fn next_retry_after(last_attempt_unix: u64, consecutive_failures: u32) -> u64 {
    let shift = consecutive_failures.saturating_sub(1).min(20);
    let backoff = RETRY_BASE_SECS
        .saturating_mul(1u64 << shift)
        .min(RETRY_MAX_SECS);
    last_attempt_unix.saturating_add(backoff)
}

// ---------------------------------------------------------------------------
// 该传哪几天
// ---------------------------------------------------------------------------

/// 算出"该传的天"与"因为太老被跳过、不再重试的天"。
///
/// 规则：只算**已经结束的 UTC 天**（`day < today`），且只算 `last_uploaded_day` 之后的；
/// 超过 `cap` 天时保留**最新的 cap 天**，更老的记进 `skipped`（明说跳过）。
///
/// 纯函数、只看输入 ⇒ 可以被钉死（时区、日界、补齐、上限各有一条测试）。
pub fn pending_days(
    records: &[AuditRecord],
    last_uploaded_day: Option<&str>,
    today: &str,
    cap: usize,
) -> (Vec<String>, Vec<String>) {
    let mut days: BTreeSet<String> = records
        .iter()
        .map(|r| utc_day(r.ts_unix))
        .filter(|d| d.as_str() < today)
        .collect();
    if let Some(last) = last_uploaded_day {
        days.retain(|d| d.as_str() > last);
    }
    let all: Vec<String> = days.into_iter().collect();
    if all.len() <= cap {
        return (all, Vec::new());
    }
    let split = all.len() - cap;
    (all[split..].to_vec(), all[..split].to_vec())
}

/// 读审计文件（可给多个：当前文件 + 轮转后的 `.1`）。缺文件 = 空，不算错。
///
/// ⚠️ 轮转会把 `.1` 删掉（见 `AuditLog::append`）⇒ **太久没启动 App，很老的天会从
/// 文件里消失**，那时它既不会被上传也不会被跳过 —— 这是"本机没留住的证据"，
/// 不是"那天没问题"。
pub fn read_audit_files(paths: &[PathBuf]) -> Result<Vec<AuditRecord>, SyncError> {
    let mut out: Vec<AuditRecord> = Vec::new();
    for p in paths {
        let mut rows = AuditLog::tail(p, usize::MAX)
            .map_err(|e| SyncError::Io(format!("读 {} 失败：{e}", p.display())))?;
        out.append(&mut rows);
    }
    out.sort_by_key(|r| r.ts_unix);
    Ok(out)
}

// ---------------------------------------------------------------------------
// 传输
// ---------------------------------------------------------------------------

fn post_json(
    transport: &dyn Transport,
    cfg: &SyncConfig,
    path: &str,
    body: Vec<u8>,
) -> Result<HttpResponse, SyncError> {
    let url = join_url(&cfg.base_url, path);
    // Host / Content-Length / Connection 由 `build_request` 统一写，这里**不要**重复给。
    let headers = vec![
        ("accept".to_string(), "application/json".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        (
            "user-agent".to_string(),
            format!("xraytun-audit-sync/{}", cfg.app_version),
        ),
        (
            "authorization".to_string(),
            format!("Bearer {}", cfg.token),
        ),
    ];
    let request = HttpRequest {
        url,
        headers,
        body,
        timeout: Duration::from_secs(UPLOAD_TIMEOUT_SECS),
    };
    transport
        .post(&request)
        .map_err(|e| SyncError::Transport(e.as_str().to_string()))
}

fn status_error(status: u16, body: &str) -> SyncError {
    match status {
        401 => SyncError::Unauthorized,
        429 => SyncError::RateLimited,
        // 413 不单独建一个变体：它由服务端报出，走 Http 分支保留真实 body。
        // `TooLarge` 只用于**客户端预检**（那时我们知道确切字节数）。
        _ => SyncError::Http {
            status,
            // 服务端应答可能很长；**截断**保存，避免把一整页 HTML 写进状态文件。
            body: body.chars().take(200).collect(),
        },
    }
}

#[derive(Debug, Clone, Deserialize)]
struct UploadResponse {
    #[serde(default)]
    replaced: bool,
}

/// 上传一天。返回服务端是否报告"覆盖了已有对象"。
///
/// **幂等**：key 由 `device + day` 决定 ⇒ 重试只会覆盖。
pub fn upload_envelope(
    transport: &dyn Transport,
    cfg: &SyncConfig,
    env: &Envelope,
) -> Result<bool, SyncError> {
    let body =
        serde_json::to_vec(env).map_err(|e| SyncError::Json(format!("信封序列化失败：{e}")))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(SyncError::TooLarge(body.len()));
    }
    let resp = post_json(transport, cfg, UPLOAD_PATH, body)?;
    if resp.status != 200 {
        return Err(status_error(resp.status, &resp.body));
    }
    Ok(serde_json::from_str::<UploadResponse>(&resp.body)
        .map(|r| r.replaced)
        .unwrap_or(false))
}

/// 服务端上一个 bundle 的元数据（**只有元数据，没有内容**）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteItem {
    pub day: String,
    #[serde(default)]
    pub rows: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub uploaded_unix: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListResponse {
    #[serde(default)]
    items: Vec<RemoteItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct RevokeResponse {
    #[serde(default)]
    deleted: u64,
}

/// 列出服务端已有的天（用于"已上传 N 天"这种**有证据**的显示）。
pub fn list_remote(
    transport: &dyn Transport,
    cfg: &SyncConfig,
) -> Result<Vec<RemoteItem>, SyncError> {
    let body = serde_json::to_vec(&serde_json::json!({ "device": cfg.device }))
        .map_err(|e| SyncError::Json(format!("请求体序列化失败：{e}")))?;
    let resp = post_json(transport, cfg, LIST_PATH, body)?;
    if resp.status != 200 {
        return Err(status_error(resp.status, &resp.body));
    }
    let parsed: ListResponse = serde_json::from_str(&resp.body)
        .map_err(|e| SyncError::Json(format!("list 应答解析失败：{e}")))?;
    Ok(parsed.items)
}

/// 撤回：让服务端删掉本设备**全部**已上传对象。返回真正删掉的个数。
pub fn revoke_remote(transport: &dyn Transport, cfg: &SyncConfig) -> Result<u64, SyncError> {
    let body = serde_json::to_vec(&serde_json::json!({ "device": cfg.device }))
        .map_err(|e| SyncError::Json(format!("请求体序列化失败：{e}")))?;
    let resp = post_json(transport, cfg, REVOKE_PATH, body)?;
    if resp.status != 200 {
        return Err(status_error(resp.status, &resp.body));
    }
    let parsed: RevokeResponse = serde_json::from_str(&resp.body)
        .map_err(|e| SyncError::Json(format!("revoke 应答解析失败：{e}")))?;
    Ok(parsed.deleted)
}

// ---------------------------------------------------------------------------
// 一次同步（编排）
// ---------------------------------------------------------------------------

/// 一次同步的结果。**成功与失败都在这里**，调用方不需要再猜。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncRun {
    /// 本次真的传上去的天（升序）。
    pub uploaded: Vec<String>,
    /// 卡在哪一天。
    pub failed_day: Option<String>,
    /// 人类可读的失败原因（成功时为 `None`）。
    pub error: Option<String>,
    /// 因为太老而**明说跳过**的天。
    pub skipped_days: Vec<String>,
    /// 本次真的发出去的请求数（关闭时为 0 —— 有测试钉住）。
    pub requests: usize,
}

/// 做一轮同步：算该传哪天 → 逐天组 bundle → 加密 → 上传 → 更新状态。
///
/// `enabled == false` 时**一个请求都不发、状态也不动**。
pub fn sync_once(
    cfg: &SyncConfig,
    transport: &dyn Transport,
    state: &mut SyncState,
    state_path: &Path,
    records: &[AuditRecord],
    today: &str,
    now_unix: u64,
) -> SyncRun {
    let mut run = SyncRun::default();

    if !cfg.enabled {
        return run;
    }
    if let Err(e) = cfg.ready() {
        run.error = Some(e.user_message());
        return run;
    }
    if !state.retry_allowed(now_unix) {
        run.error = None; // 退避中不是错误
        return run;
    }

    let (pending, skipped) = pending_days(
        records,
        state.last_uploaded_day.as_deref(),
        today,
        MAX_CATCHUP_DAYS,
    );
    run.skipped_days = skipped.clone();
    let mut dirty = false;
    for d in &skipped {
        if !state.skipped_days.contains(d) {
            state.skipped_days.push(d.clone());
            dirty = true;
        }
    }
    if pending.is_empty() {
        if dirty {
            let _ = state.save(state_path);
        }
        return run;
    }

    for day in pending {
        let Some(bundle) = bundle_for_day(&day, &cfg.device, &cfg.app_version, records) else {
            continue;
        };
        let env = match encrypt_bundle(&bundle, &cfg.key) {
            Ok(e) => e,
            Err(e) => {
                run.failed_day = Some(day);
                run.error = Some(e.user_message());
                break;
            }
        };
        run.requests += 1;
        match upload_envelope(transport, cfg, &env) {
            Ok(_) => {
                state.last_uploaded_day = Some(day.clone());
                state.last_ok_unix = Some(now_unix);
                state.last_attempt_unix = Some(now_unix);
                state.last_error = None;
                state.consecutive_failures = 0;
                dirty = true;
                run.uploaded.push(day);
            }
            Err(e) => {
                state.last_attempt_unix = Some(now_unix);
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                state.last_error = Some(e.user_message());
                dirty = true;
                run.failed_day = Some(day);
                run.error = Some(e.user_message());
                break;
            }
        }
    }

    if dirty {
        if let Err(e) = state.save(state_path) {
            if run.error.is_none() {
                run.error = Some(e.user_message());
            }
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::Usage;
    use std::sync::Mutex;

    // ---- 假传输层 ----------------------------------------------------------

    struct FakeTransport {
        calls: Mutex<Vec<HttpRequest>>,
        responses: Mutex<Vec<HttpResponse>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses),
            }
        }
        fn ok() -> Self {
            Self::new(vec![HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: "{\"ok\":true,\"key\":\"k\",\"replaced\":false}".into(),
            }])
        }
        fn status(code: u16) -> Self {
            Self::new(vec![HttpResponse {
                status: code,
                headers: Vec::new(),
                body: "{}".into(),
            }])
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
        fn last_body(&self) -> String {
            let calls = self.calls.lock().unwrap();
            String::from_utf8_lossy(&calls.last().unwrap().body).to_string()
        }
        fn last_url(&self) -> String {
            let calls = self.calls.lock().unwrap();
            calls.last().unwrap().url.clone()
        }
        fn last_auth(&self) -> Option<String> {
            let calls = self.calls.lock().unwrap();
            calls
                .last()
                .unwrap()
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.clone())
        }
    }

    impl Transport for FakeTransport {
        fn post(&self, request: &HttpRequest) -> Result<HttpResponse, crate::transport::TransportError> {
            self.calls.lock().unwrap().push(request.clone());
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(crate::transport::TransportError::Io("没有预置应答".into()));
            }
            Ok(q.remove(0))
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    // ---- 夹具 --------------------------------------------------------------

    fn rec(ts: u64, host: &str, outcome: &str) -> AuditRecord {
        AuditRecord {
            ts_unix: ts,
            host: host.into(),
            outcome: outcome.into(),
            reason: None,
            category: Some("ad_or_monetization".into()),
            ads_intent: Some(0.97),
            risk_of_breakage: Some(0.05),
            choice_confidence: Some(0.93),
            effective_min: Some(0.85),
            applied: true,
            cache_hit: false,
            model: Some("jev-1.13-free".into()),
            usage: Some(Usage { input_tokens: 90, output_tokens: 12 }),
            context_sent: None,
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("xt-audit-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    const D: &str = "3f2a91c4d0be7715";

    fn cfg(transport_ready: bool) -> SyncConfig {
        SyncConfig {
            enabled: transport_ready,
            base_url: DEFAULT_BASE_URL.to_string(),
            token: "tok-123".into(),
            device: D.into(),
            key: key(),
            app_version: "0.9.2".into(),
        }
    }

    // ---- hex ---------------------------------------------------------------

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let bytes = vec![0x00, 0x0f, 0xff, 0x10];
        assert_eq!(hex_encode(&bytes), "000fff10");
        assert_eq!(hex_decode("000fff10").unwrap(), bytes);
        assert_eq!(hex_decode("000FFF10").unwrap(), bytes, "大写也认");
        assert!(hex_decode("abc").is_err(), "奇数长度必须报错");
        assert!(hex_decode("zz").is_err(), "非 hex 必须报错");
        assert!(hex_decode("").unwrap().is_empty());
    }

    #[test]
    fn a_device_id_is_16_lowercase_hex_and_is_random_each_time() {
        let a = new_device_id().unwrap();
        let b = new_device_id().unwrap();
        assert_eq!(a.len(), DEVICE_ID_HEX_LEN);
        assert!(is_device_id(&a), "{a}");
        assert_ne!(a, b, "两次生成必须不同（随机源没接上？）");
        assert!(!is_device_id(""), "空串不是设备 id");
        assert!(!is_device_id("3F2A91C4D0BE7715"), "大写不是我们的格式");
        assert!(!is_device_id("3f2a91c4d0be771"), "长度不对");
        assert!(!is_device_id("../3f2a91c4d0b"), "不能有路径字符");
    }

    // ---- bundle ------------------------------------------------------------

    /// **最重要的一条**：`context_sent` 一定要被剥掉。
    #[test]
    fn the_bundle_never_carries_context_sent_even_when_it_is_recorded_locally() {
        let t = 20_720 * 86_400;
        let mut r = rec(t, "ads.example", "block");
        r.context_sent = Some("用户正在看的页面内容".into());
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[r]).unwrap();
        assert_eq!(b.rows.len(), 1);
        assert!(
            b.rows[0].context_sent.is_none(),
            "开了'记录外发内容'也不该让页面内容跟着同步出去"
        );
        let wire = serde_json::to_string(&b).unwrap();
        assert!(!wire.contains("用户正在看的页面内容"), "{wire}");
        assert!(!wire.contains("context_sent"), "连这个 key 都不该出现：{wire}");
    }

    #[test]
    fn rebuilding_the_same_day_is_byte_identical() {
        let t = 20_720 * 86_400;
        let recs = vec![rec(t + 5, "b.example", "allow"), rec(t + 1, "a.example", "block")];
        let a = bundle_for_day("2026-09-24", D, "0.9.2", &recs).unwrap();
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &recs).unwrap();
        assert_eq!(serde_json::to_vec(&a).unwrap(), serde_json::to_vec(&b).unwrap());
        // 行按 ts 升序，且与输入顺序无关
        assert_eq!(a.rows[0].host, "a.example");
        assert_eq!(a.rows[1].host, "b.example");
        let reversed: Vec<AuditRecord> = recs.iter().rev().cloned().collect();
        let c = bundle_for_day("2026-09-24", D, "0.9.2", &reversed).unwrap();
        assert_eq!(serde_json::to_vec(&a).unwrap(), serde_json::to_vec(&c).unwrap());
    }

    #[test]
    fn a_day_with_no_records_produces_no_bundle() {
        let t = 20_720 * 86_400;
        assert!(bundle_for_day("2026-09-25", D, "0.9.2", &[rec(t, "a.example", "block")]).is_none());
        assert!(bundle_for_day("2026-09-24", D, "0.9.2", &[]).is_none());
    }

    #[test]
    fn bundle_counts_match_the_rows() {
        let t = 20_720 * 86_400;
        let mut allow = rec(t, "a.example", "allow");
        allow.cache_hit = true;
        allow.applied = false;
        let recs = vec![rec(t, "b.example", "block"), allow, rec(t, "c.example", "deferred")];
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &recs).unwrap();
        assert_eq!(b.counts.rows, 3);
        assert_eq!(b.counts.block, 1);
        assert_eq!(b.counts.allow, 1);
        assert_eq!(b.counts.deferred, 1);
        assert_eq!(b.counts.cache_hit, 1);
        assert_eq!(b.counts.applied, 2);
        assert_eq!(b.kind, BUNDLE_KIND);
    }

    // ---- 信封 --------------------------------------------------------------

    #[test]
    fn the_envelope_contains_no_hostname_at_all() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "tracker.evil.example", "block")])
            .unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();
        let wire = serde_json::to_string(&env).unwrap();
        assert!(
            !wire.contains("tracker.evil.example"),
            "信封里出现了明文域名 ⇒ 加密没做对：{wire}"
        );
        assert!(!wire.contains("jev-1.13-free"), "{wire}");
        // 明文头里**允许**有的元数据（这是要如实告诉用户的部分）
        assert!(wire.contains(D));
        assert!(wire.contains("2026-09-24"));
    }

    #[test]
    fn decryption_round_trips_a_bundle() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();
        let back = decrypt_envelope(&env, &key()).unwrap();
        assert_eq!(back, b);
    }

    /// AAD 的意义：明文头被改 ⇒ 解密必须失败，而不是"解出来另一天"。
    #[test]
    fn tampering_with_the_plaintext_header_breaks_decryption() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();

        let mut day = env.clone();
        day.day = "2026-09-25".into();
        assert!(decrypt_envelope(&day, &key()).is_err(), "改 day 必须被发现");

        let mut rows = env.clone();
        rows.rows = 999;
        assert!(decrypt_envelope(&rows, &key()).is_err(), "改 rows 必须被发现");

        let mut device = env.clone();
        device.device = "aa2a91c4d0be7715".into();
        assert!(decrypt_envelope(&device, &key()).is_err(), "改 device 必须被发现");

        // 但只改"不参与认证"的 bytes 字段仍能解开 —— 说明我们没把不可信元数据当真
        let mut bytes = env.clone();
        bytes.bytes = 1;
        assert!(decrypt_envelope(&bytes, &key()).is_ok());
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();
        let mut ct = env.ct.clone().into_bytes();
        // 翻转最后一个 hex 数字
        let last = ct.len() - 1;
        ct[last] = if ct[last] == b'0' { b'1' } else { b'0' };
        let mut broken = env.clone();
        broken.ct = String::from_utf8(ct).unwrap();
        assert!(decrypt_envelope(&broken, &key()).is_err());
    }

    #[test]
    fn the_wrong_key_fails_cleanly_and_says_why() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();
        let mut other = key();
        other[0] ^= 0xff;
        let err = decrypt_envelope(&env, &other).unwrap_err();
        assert!(matches!(err, SyncError::Crypto(_)), "{err:?}");
        assert!(err.user_message().contains("密钥不对"), "{err}");
    }

    #[test]
    fn a_foreign_envelope_version_or_alg_is_refused() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let env = encrypt_bundle(&b, &key()).unwrap();
        let mut v2 = env.clone();
        v2.v = 2;
        assert!(decrypt_envelope(&v2, &key()).is_err());
        let mut alg = env.clone();
        alg.alg = "aes-gcm".into();
        assert!(decrypt_envelope(&alg, &key()).is_err());
    }

    #[test]
    fn each_encryption_uses_a_fresh_nonce() {
        let t = 20_720 * 86_400;
        let b = bundle_for_day("2026-09-24", D, "0.9.2", &[rec(t, "a.example", "block")]).unwrap();
        let a = encrypt_bundle(&b, &key()).unwrap();
        let b2 = encrypt_bundle(&b, &key()).unwrap();
        assert_ne!(a.nonce, b2.nonce, "nonce 重用会毁掉 ChaCha20-Poly1305");
        assert_ne!(a.ct, b2.ct);
        assert_eq!(a.nonce.len(), NONCE_LEN * 2);
    }

    // ---- 该传哪天 ----------------------------------------------------------

    #[test]
    fn only_complete_utc_days_are_pending() {
        let today = 20_721 * 86_400; // 2026-09-25
        let recs = vec![
            rec(20_720 * 86_400 + 10, "a.example", "block"), // 09-24
            rec(today + 60, "b.example", "block"),           // 09-25 = 今天，还没结束
        ];
        let (pending, skipped) = pending_days(&recs, None, "2026-09-25", MAX_CATCHUP_DAYS);
        assert_eq!(pending, vec!["2026-09-24".to_string()], "今天不许传");
        assert!(skipped.is_empty());
    }

    #[test]
    fn a_day_already_uploaded_is_not_sent_again() {
        let recs = vec![
            rec(20_718 * 86_400, "a.example", "block"), // 09-22
            rec(20_719 * 86_400, "b.example", "block"), // 09-23
            rec(20_720 * 86_400, "c.example", "block"), // 09-24
        ];
        let (pending, _) = pending_days(&recs, Some("2026-09-23"), "2026-09-25", MAX_CATCHUP_DAYS);
        assert_eq!(pending, vec!["2026-09-24".to_string()]);
    }

    #[test]
    fn missed_days_are_caught_up_in_order_and_old_ones_are_explicitly_skipped() {
        // 40 天里每天一条；cap = 31 ⇒ 最老的 9 天被明说跳过
        let mut recs = Vec::new();
        for d in 0..40u64 {
            recs.push(rec((20_700 + d) * 86_400, "a.example", "block"));
        }
        let today = utc_day((20_700 + 40) * 86_400);
        let (pending, skipped) = pending_days(&recs, None, &today, MAX_CATCHUP_DAYS);
        assert_eq!(pending.len(), MAX_CATCHUP_DAYS);
        assert_eq!(skipped.len(), 40 - MAX_CATCHUP_DAYS);
        assert!(pending[0] < pending[pending.len() - 1], "升序");
        assert!(skipped[skipped.len() - 1] < pending[0], "跳过的都是更老的");
    }

    #[test]
    fn pending_days_is_driven_only_by_its_inputs() {
        let recs = vec![rec(20_720 * 86_400, "a.example", "block")];
        let a = pending_days(&recs, None, "2026-09-25", MAX_CATCHUP_DAYS);
        let b = pending_days(&recs, None, "2026-09-25", MAX_CATCHUP_DAYS);
        assert_eq!(a, b);
        assert!(pending_days(&[], None, "2026-09-25", MAX_CATCHUP_DAYS).0.is_empty());
    }

    // ---- 退避 --------------------------------------------------------------

    #[test]
    fn backoff_grows_then_caps_at_six_hours() {
        assert_eq!(next_retry_after(1000, 1), 1000 + 30 * 60);
        assert_eq!(next_retry_after(1000, 2), 1000 + 60 * 60);
        assert_eq!(next_retry_after(1000, 3), 1000 + 120 * 60);
        assert_eq!(next_retry_after(1000, 50), 1000 + RETRY_MAX_SECS, "必须封顶");
        // 没有失败史 ⇒ 没有退避
        let s = SyncState::new(D.into());
        assert_eq!(s.next_retry_unix(), None);
        assert!(s.retry_allowed(1));
    }

    #[test]
    fn a_failure_blocks_retries_until_the_backoff_has_passed() {
        let mut s = SyncState::new(D.into());
        s.consecutive_failures = 1;
        s.last_attempt_unix = Some(10_000);
        let at = s.next_retry_unix().unwrap();
        assert!(!s.retry_allowed(at - 1));
        assert!(s.retry_allowed(at));
    }

    // ---- 状态文件 ----------------------------------------------------------

    #[test]
    fn the_state_round_trips_and_a_corrupt_one_is_an_error_not_a_new_device() {
        let dir = tmpdir("state");
        let p = dir.join("intent-audit-sync.json");
        assert!(SyncState::load(&p).unwrap().is_none(), "没写过 = None");

        let mut s = SyncState::new(D.into());
        s.last_uploaded_day = Some("2026-09-24".into());
        s.save(&p).unwrap();
        let back = SyncState::load(&p).unwrap().unwrap();
        assert_eq!(back, s);

        std::fs::write(&p, "{ 这不是 json").unwrap();
        let err = SyncState::load(&p).unwrap_err();
        assert!(matches!(err, SyncError::BadState(_)), "{err:?}");
        assert!(err.user_message().contains("解析失败"), "{err}");

        // 形状不对的设备 id 也要拦（否则会写到别人的前缀下）
        std::fs::write(&p, "{\"v\":1,\"device\":\"NOT-A-DEVICE\"}").unwrap();
        assert!(matches!(SyncState::load(&p).unwrap_err(), SyncError::BadState(_)));
    }

    // ---- 端到端（假传输层）-------------------------------------------------

    #[test]
    fn a_disabled_config_sends_nothing_and_touches_no_state() {
        let dir = tmpdir("disabled");
        let p = dir.join("s.json");
        let mut state = SyncState::new(D.into());
        let recs = vec![rec(20_720 * 86_400, "a.example", "block")];
        let t = FakeTransport::ok();
        let run = sync_once(
            &cfg(false),
            &t,
            &mut state,
            &p,
            &recs,
            "2026-09-25",
            1_000,
        );
        assert_eq!(t.call_count(), 0, "关着的时候一个请求都不许发");
        assert_eq!(run.requests, 0);
        assert!(run.uploaded.is_empty());
        assert!(run.error.is_none(), "关着不是错误");
        assert!(!p.exists(), "关着的时候不该写状态文件");
    }

    #[test]
    fn a_missing_token_is_refused_before_any_request() {
        let dir = tmpdir("notoken");
        let p = dir.join("s.json");
        let mut c = cfg(true);
        c.token = "   ".into();
        let mut state = SyncState::new(D.into());
        let t = FakeTransport::ok();
        let run = sync_once(
            &c,
            &t,
            &mut state,
            &p,
            &[rec(20_720 * 86_400, "a.example", "block")],
            "2026-09-25",
            1_000,
        );
        assert_eq!(t.call_count(), 0);
        assert!(run.error.unwrap().contains("token"));
    }

    #[test]
    fn a_non_https_endpoint_is_refused() {
        let mut c = cfg(true);
        c.base_url = "http://xraytun.top".into();
        let err = c.ready().unwrap_err();
        assert!(err.user_message().contains("https"), "{err}");

        c.base_url = "xraytun.top".into();
        assert!(c.ready().is_err(), "没有 scheme 也不行");
    }

    #[test]
    fn a_successful_run_uploads_one_request_per_day_and_is_idempotent_on_replay() {
        let dir = tmpdir("ok");
        let p = dir.join("s.json");
        let recs = vec![
            rec(20_719 * 86_400, "a.example", "block"),
            rec(20_720 * 86_400, "b.example", "block"),
        ];
        let mut state = SyncState::new(D.into());
        let t = FakeTransport::new(vec![
            HttpResponse { status: 200, headers: vec![], body: "{\"ok\":true,\"replaced\":false}".into() },
            HttpResponse { status: 200, headers: vec![], body: "{\"ok\":true,\"replaced\":true}".into() },
            // 第二次跑不该再用到；预置一个以防万一，测试会断言"没被用"。
            HttpResponse { status: 200, headers: vec![], body: "{\"ok\":true}".into() },
        ]);
        let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 5_000);
        assert_eq!(run.uploaded, vec!["2026-09-23".to_string(), "2026-09-24".to_string()]);
        assert!(run.error.is_none(), "{run:?}");
        assert_eq!(run.requests, 2);
        assert_eq!(t.call_count(), 2);
        assert_eq!(state.last_uploaded_day.as_deref(), Some("2026-09-24"));
        assert_eq!(state.last_ok_unix, Some(5_000));
        assert_eq!(state.consecutive_failures, 0);
        assert!(p.exists(), "成功必须落状态");
        assert!(t.last_url().ends_with("/api/audit"), "{}", t.last_url());
        assert_eq!(t.last_auth().as_deref(), Some("Bearer tok-123"));

        // 重放：一天都不该再传
        let run2 = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 6_000);
        assert!(run2.uploaded.is_empty(), "{run2:?}");
        assert_eq!(t.call_count(), 2, "幂等：重放不该产生新请求");
    }

    #[test]
    fn the_uploaded_body_is_an_encrypted_envelope_with_the_right_shape() {
        let dir = tmpdir("body");
        let p = dir.join("s.json");
        let recs = vec![rec(20_720 * 86_400, "tracker.evil.example", "block")];
        let mut state = SyncState::new(D.into());
        let t = FakeTransport::ok();
        let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 5_000);
        assert_eq!(run.uploaded.len(), 1);
        let body = t.last_body();
        assert!(!body.contains("tracker.evil.example"), "{body}");
        let env: Envelope = serde_json::from_str(&body).unwrap();
        assert_eq!(env.v, ENVELOPE_VERSION);
        assert_eq!(env.alg, ALG);
        assert_eq!(env.device, D);
        assert_eq!(env.day, "2026-09-24");
        assert_eq!(env.rows, 1);
        // 服务端能做到的只是"把它原样存下来"；我们还能自己解回来
        let back = decrypt_envelope(&env, &key()).unwrap();
        assert_eq!(back.rows[0].host, "tracker.evil.example");
    }

    #[test]
    fn a_401_records_the_error_and_does_not_advance_the_progress_marker() {
        let dir = tmpdir("401");
        let p = dir.join("s.json");
        let recs = vec![rec(20_720 * 86_400, "a.example", "block")];
        let mut state = SyncState::new(D.into());
        let t = FakeTransport::status(401);
        let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 5_000);
        assert!(run.uploaded.is_empty());
        assert_eq!(run.failed_day.as_deref(), Some("2026-09-24"));
        assert_eq!(state.last_uploaded_day, None, "失败了就不许推进进度");
        assert_eq!(state.consecutive_failures, 1);
        assert!(state.last_error.unwrap().contains("401"));
        // 状态必须落盘（否则重启后无限重试）
        assert!(p.exists());
        let back = SyncState::load(&p).unwrap().unwrap();
        assert_eq!(back.consecutive_failures, 1);
    }

    #[test]
    fn a_413_says_too_large_and_a_429_says_rate_limited() {
        let dir = tmpdir("codes");
        let p = dir.join("s.json");
        let recs = vec![rec(20_720 * 86_400, "a.example", "block")];
        for (code, needle) in [(413u16, "413"), (429, "429")] {
            let mut state = SyncState::new(D.into());
            let t = FakeTransport::status(code);
            let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 5_000);
            let msg = run.error.unwrap();
            assert!(msg.contains(needle), "code={code} msg={msg}");
        }
    }

    #[test]
    fn a_transport_failure_is_reported_and_the_next_run_waits_for_the_backoff() {
        let dir = tmpdir("backoff");
        let p = dir.join("s.json");
        let recs = vec![rec(20_720 * 86_400, "a.example", "block")];
        let mut state = SyncState::new(D.into());
        // 空应答队列 ⇒ FakeTransport 返回传输错误
        let t = FakeTransport::new(vec![]);
        let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, "2026-09-25", 5_000);
        assert!(run.error.is_some());
        assert_eq!(state.consecutive_failures, 1);

        // 退避期内：一个请求都不发
        let t2 = FakeTransport::ok();
        let run2 = sync_once(&cfg(true), &t2, &mut state, &p, &recs, "2026-09-25", 5_100);
        assert_eq!(t2.call_count(), 0, "退避期内不该再打");
        assert!(run2.uploaded.is_empty());
        assert!(run2.error.is_none(), "退避中不算错误");

        // 退避之后：可以再试
        let t3 = FakeTransport::ok();
        let at = state.next_retry_unix().unwrap();
        let run3 = sync_once(&cfg(true), &t3, &mut state, &p, &recs, "2026-09-25", at);
        assert_eq!(t3.call_count(), 1);
        assert_eq!(run3.uploaded, vec!["2026-09-24".to_string()]);
        assert_eq!(state.consecutive_failures, 0, "成功后失败计数清零");
    }

    #[test]
    fn skipped_days_are_recorded_once_and_survive_a_restart() {
        let dir = tmpdir("skipped");
        let p = dir.join("s.json");
        // 40 天：31 天可传，9 天太老
        let mut recs = Vec::new();
        for d in 0..40u64 {
            recs.push(rec((20_700 + d) * 86_400, "a.example", "block"));
        }
        let today = utc_day((20_700 + 40) * 86_400);
        let mut state = SyncState::new(D.into());
        let responses: Vec<HttpResponse> = (0..MAX_CATCHUP_DAYS)
            .map(|_| HttpResponse { status: 200, headers: vec![], body: "{\"ok\":true}".into() })
            .collect();
        let t = FakeTransport::new(responses);
        let run = sync_once(&cfg(true), &t, &mut state, &p, &recs, &today, 5_000);
        assert_eq!(run.uploaded.len(), MAX_CATCHUP_DAYS);
        assert_eq!(run.skipped_days.len(), 9);
        assert_eq!(state.skipped_days.len(), 9);
        let back = SyncState::load(&p).unwrap().unwrap();
        assert_eq!(back.skipped_days.len(), 9, "跳过记录要落盘");
    }

    #[test]
    fn list_and_revoke_parse_the_server_answers() {
        let t = FakeTransport::new(vec![
            HttpResponse {
                status: 200,
                headers: vec![],
                body: "{\"ok\":true,\"items\":[{\"day\":\"2026-09-24\",\"rows\":3,\"bytes\":900,\"key\":\"audit/x/2026-09-24.json\",\"uploaded_unix\":123}]}".into(),
            },
            HttpResponse {
                status: 200,
                headers: vec![],
                body: "{\"ok\":true,\"deleted\":7}".into(),
            },
        ]);
        let items = list_remote(&t, &cfg(true)).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].day, "2026-09-24");
        assert_eq!(items[0].rows, 3);
        assert_eq!(t.last_url(), format!("{DEFAULT_BASE_URL}{LIST_PATH}"));
        assert_eq!(revoke_remote(&t, &cfg(true)).unwrap(), 7);
        assert_eq!(t.last_url(), format!("{DEFAULT_BASE_URL}{REVOKE_PATH}"));
    }

    #[test]
    fn list_and_revoke_respect_server_errors() {
        let t = FakeTransport::status(401);
        assert!(matches!(list_remote(&t, &cfg(true)), Err(SyncError::Unauthorized)));
        let t = FakeTransport::status(429);
        assert!(matches!(revoke_remote(&t, &cfg(true)), Err(SyncError::RateLimited)));
    }

    #[test]
    fn reading_audit_files_tolerates_missing_files_and_bad_lines() {
        let dir = tmpdir("read");
        let a = dir.join("intent-audit.jsonl");
        let b = dir.join("intent-audit.jsonl.1");
        std::fs::write(&a, format!("{}\n", serde_json::to_string(&rec(20_720 * 86_400, "a.example", "block")).unwrap())).unwrap();
        std::fs::write(&b, "这不是 json\n").unwrap();
        let missing = dir.join("nope.jsonl");
        let rows = read_audit_files(&[a.clone(), b.clone(), missing]).unwrap();
        assert_eq!(rows.len(), 1, "坏行跳过，缺文件当空");
        // 排序：读进来的必须按时间升序（bundle 的确定性依赖它）
        let mut a2 = Vec::new();
        for i in 0..5u64 {
            a2.push(serde_json::to_string(&rec(20_720 * 86_400 + (5 - i), "h.example", "block")).unwrap());
        }
        std::fs::write(&a, format!("{}\n", a2.join("\n"))).unwrap();
        let rows = read_audit_files(&[a]).unwrap();
        assert!(rows.windows(2).all(|w| w[0].ts_unix <= w[1].ts_unix));
    }
}
