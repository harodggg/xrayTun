//! 离线审计 + 审计同步的**命令行入口**（人用，不是产品路径）。
//!
//! ```bash
//! # 1) 出报告（不联网、不需要密钥）
//! cargo run -p xt-intent --example intent_audit -- report
//!
//! # 2) 看今天/某天会传出什么（本地明文，仍然不联网）
//! cargo run -p xt-intent --example intent_audit -- bundle --day 2026-09-24
//!
//! # 3) 真的同步一次（会联网；等价于 App 里那个定时任务做的一轮）
//! cargo run -p xt-intent --example intent_audit -- upload \
//!   --key-file  ~/.xraytun-audit-key \
//!   --token-file ~/.xraytun-audit-token \
//!   --state ~/.xraytun-audit-sync.json
//!
//! # 4) 把下载回来的密文解开（要同一把密钥）
//! cargo run -p xt-intent --example intent_audit -- decrypt --in env.json --key-file ~/.xraytun-audit-key
//!
//! # 5) 撤回：让服务端删掉本设备全部已上传对象
//! cargo run -p xt-intent --example intent_audit -- revoke --token-file ~/.xraytun-audit-token \
//!   --state ~/.xraytun-audit-sync.json
//! ```
//!
//! # 隐私（这个工具自己也要守）
//!
//! - 默认读的是本机 App 的审计文件；`report` 与 `bundle` **不联网**。
//! - `report` 里有域名（= 浏览记录）⇒ 用 `--out` 时**导到仓库之外**，别提交。
//! - 密钥与 token 从**文件**读（这个 CLI 不进 Keychain）：只放在你自己家里，权限 0600。

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use xt_intent::audit_report::{utc_day, AuditReport};
use xt_intent::audit_sync::{
    self, bundle_for_day, decrypt_envelope, encrypt_bundle, hex_decode, hex_encode, is_device_id,
    new_device_id, new_key, pending_days, read_audit_files, sync_once, Envelope, SyncConfig,
    SyncState, BUNDLE_KIND, DEFAULT_BASE_URL, KEY_LEN, MAX_CATCHUP_DAYS,
};
use xt_intent::transport::TlsTransport;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 本机 App 的数据目录（macOS）。
fn default_data_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("com.xraytun.desktop")
}

struct Args {
    cmd: String,
    audit: Vec<PathBuf>,
    day: Option<String>,
    out: Option<String>,
    input: Option<PathBuf>,
    key_file: Option<PathBuf>,
    token_file: Option<PathBuf>,
    state: PathBuf,
    base_url: String,
    device: Option<String>,
    dry_run: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut it = std::env::args().skip(1);
        let cmd = it.next().ok_or_else(usage)?;
        let mut a = Args {
            cmd,
            audit: Vec::new(),
            day: None,
            out: None,
            input: None,
            key_file: None,
            token_file: None,
            state: PathBuf::from("intent-audit-sync.json"),
            base_url: DEFAULT_BASE_URL.to_string(),
            device: None,
            dry_run: false,
        };
        while let Some(flag) = it.next() {
            let mut take = |name: &str| -> Result<String, String> {
                it.next().ok_or_else(|| format!("{name} 后面缺一个值"))
            };
            match flag.as_str() {
                "--audit" => a.audit.push(PathBuf::from(take("--audit")?)),
                "--day" => a.day = Some(take("--day")?),
                "--out" => a.out = Some(take("--out")?),
                "--in" => a.input = Some(PathBuf::from(take("--in")?)),
                "--key-file" => a.key_file = Some(PathBuf::from(take("--key-file")?)),
                "--token-file" => a.token_file = Some(PathBuf::from(take("--token-file")?)),
                "--state" => a.state = PathBuf::from(take("--state")?),
                "--base-url" => a.base_url = take("--base-url")?,
                "--device" => a.device = Some(take("--device")?),
                "--dry-run" => a.dry_run = true,
                "-h" | "--help" => return Err(usage()),
                other => return Err(format!("不认识的参数：{other}\n\n{}", usage())),
            }
        }
        Ok(a)
    }

    /// 没给 `--audit` 就用本机 App 的路径（当前文件 + 轮转文件）。
    fn audit_paths(&self) -> Vec<PathBuf> {
        if !self.audit.is_empty() {
            return self.audit.clone();
        }
        let root = default_data_root();
        vec![
            root.join("intent-audit.jsonl"),
            root.join("intent-audit.jsonl.1"),
        ]
    }
}

fn usage() -> String {
    r#"用法：intent_audit <子命令> [参数]

子命令：
  report                    出离线报告（不联网）
  bundle                    只打印某一天的明文 bundle（不联网）
  encrypt | decrypt         用密钥加密 / 解密密文信封
  keygen                    生成 32 字节密钥（十六进制）
  deviceid                  生成一个设备 id
  upload                    真的同步一次（联网）
  list                      问服务端：本设备已上传哪些天（联网）
  revoke                    让服务端删掉本设备全部已上传对象（联网）

参数：
  --audit <路径>            可重复；默认读本机 App 的 intent-audit.jsonl(.1)
  --day <YYYY-MM-DD>        指定某天（bundle 用）
  --in/--out <路径|->       输入 / 输出文件（- = 标准输出）
  --key-file <路径>         十六进制密钥文件（缺失时 upload 会生成并落盘）
  --token-file <路径>       上传 token 文件（内容就是 token 原文）
  --state <路径>            同步状态文件（默认 ./intent-audit-sync.json）
  --base-url <https://…>    端点（默认 https://xraytun.top）
  --device <16 位 hex>      覆盖设备 id（默认从状态文件读）
  --dry-run                 只算不传
"#
    .to_string()
}

fn die(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(1);
}

fn write_out(out: &Option<String>, text: &str) {
    match out.as_deref() {
        None | Some("-") => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
        }
        Some(path) => {
            if let Err(e) = std::fs::write(path, text) {
                die(&format!("写 {path} 失败：{e}"));
            }
            eprintln!("已写入 {path}");
        }
    }
}

fn read_key(path: &PathBuf, allow_create: bool) -> [u8; KEY_LEN] {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let bytes = match hex_decode(text.trim()) {
                Ok(b) => b,
                Err(e) => die(&format!("{} 不是十六进制密钥：{e}", path.display())),
            };
            if bytes.len() != KEY_LEN {
                die(&format!(
                    "{} 里是 {} 字节，密钥必须是 {KEY_LEN} 字节",
                    path.display(),
                    bytes.len()
                ));
            }
            let mut k = [0u8; KEY_LEN];
            k.copy_from_slice(&bytes);
            k
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && allow_create => {
            let k = match new_key() {
                Ok(k) => k,
                Err(e) => die(&e.user_message()),
            };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(path, hex_encode(&k)) {
                die(&format!("写 {} 失败：{e}", path.display()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            eprintln!(
                "⚠️ 已生成新的加密密钥：{}\n   **请备份它**：丢了就再也解不开已上传的密文（本机审计文件仍是第一副本）。",
                path.display()
            );
            k
        }
        Err(e) => die(&format!("读 {} 失败：{e}", path.display())),
    }
}

fn read_token(path: &PathBuf) -> String {
    match std::fs::read_to_string(path) {
        Ok(t) => t.trim().to_string(),
        Err(e) => die(&format!("读 {} 失败：{e}", path.display())),
    }
}

fn load_or_init_state(path: &PathBuf, device_override: Option<&str>) -> SyncState {
    let mut state = match SyncState::load(path) {
        Ok(Some(s)) => s,
        Ok(None) => {
            let device = match device_override {
                Some(d) => d.to_string(),
                None => match new_device_id() {
                    Ok(d) => d,
                    Err(e) => die(&e.user_message()),
                },
            };
            if !is_device_id(&device) {
                die(&format!("设备 id 形状不对：{device:?}"));
            }
            let s = SyncState::new(device);
            if let Err(e) = s.save(path) {
                die(&e.user_message());
            }
            eprintln!("已建状态文件：{}", path.display());
            s
        }
        Err(e) => die(&e.user_message()),
    };
    if let Some(d) = device_override {
        if !is_device_id(d) {
            die(&format!("--device 形状不对：{d:?}"));
        }
        state.device = d.to_string();
    }
    state
}

fn main() {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(msg) => {
            if msg.contains("用法") || msg.contains("子命令") {
                println!("{msg}");
            } else {
                eprintln!("{msg}");
            }
            std::process::exit(if msg.contains("用法") { 0 } else { 2 });
        }
    };

    match args.cmd.as_str() {
        "report" => {
            let paths = args.audit_paths();
            let rows = match read_audit_files(&paths) {
                Ok(r) => r,
                Err(e) => die(&e.user_message()),
            };
            eprintln!(
                "读了 {} 条审计记录（来自 {}）",
                rows.len(),
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let report = AuditReport::build(&rows);
            write_out(&args.out, &report.render_markdown());
            if args.out.is_some() {
                eprintln!("⚠️ 报告里有域名（= 浏览记录）—— 不要提交进仓库。");
            }
        }
        "bundle" => {
            let day = match args.day.clone() {
                Some(d) => d,
                None => {
                    // 默认：最后一个**已经结束**的 UTC 天
                    let today = utc_day(now_unix());
                    let rows = match read_audit_files(&args.audit_paths()) {
                        Ok(r) => r,
                        Err(e) => die(&e.user_message()),
                    };
                    let (pending, _) = pending_days(&rows, None, &today, MAX_CATCHUP_DAYS);
                    match pending.last() {
                        Some(d) => d.clone(),
                        None => die(&format!("没有任何**已结束**的 UTC 天可出（今天是 {today}）")),
                    }
                }
            };
            let b = day.as_bytes();
            if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
                die(&format!("--day 要写成 YYYY-MM-DD，收到：{day:?}"));
            }
            let state_path = args.state.clone();
            let state = load_or_init_state(&state_path, args.device.as_deref());
            let rows = match read_audit_files(&args.audit_paths()) {
                Ok(r) => r,
                Err(e) => die(&e.user_message()),
            };
            let bundle = match bundle_for_day(&day, &state.device, env!("CARGO_PKG_VERSION"), &rows) {
                Some(b) => b,
                None => die(&format!("{day} 没有任何记录（空天不上传）")),
            };
            let text = serde_json::to_string_pretty(&bundle).unwrap_or_else(|e| die(&format!("{e}")));
            eprintln!(
                "设备 {}（kind={}），{} 行，明文 {} 字节 —— 这些都是**本机数据**，本次不联网。",
                state.device,
                BUNDLE_KIND,
                bundle.counts.rows,
                text.len()
            );
            write_out(&args.out, &text);
        }
        "encrypt" => {
            let input = args.input.clone().unwrap_or_else(|| die("encrypt 需要 --in"));
            let key_file = args
                .key_file
                .clone()
                .unwrap_or_else(|| die("encrypt 需要 --key-file"));
            let key = read_key(&key_file, true);
            let text = std::fs::read_to_string(&input)
                .unwrap_or_else(|e| die(&format!("读 {} 失败：{e}", input.display())));
            let bundle: audit_sync::Bundle = serde_json::from_str(&text)
                .unwrap_or_else(|e| die(&format!("{} 不是 bundle：{e}", input.display())));
            let env = encrypt_bundle(&bundle, &key).unwrap_or_else(|e| die(&e.user_message()));
            let out = serde_json::to_string_pretty(&env).unwrap_or_else(|e| die(&format!("{e}")));
            write_out(&args.out, &out);
        }
        "decrypt" => {
            let input = args.input.clone().unwrap_or_else(|| die("decrypt 需要 --in"));
            let key_file = args
                .key_file
                .clone()
                .unwrap_or_else(|| die("decrypt 需要 --key-file"));
            let key = read_key(&key_file, false);
            let text = std::fs::read_to_string(&input)
                .unwrap_or_else(|e| die(&format!("读 {} 失败：{e}", input.display())));
            let env: Envelope = serde_json::from_str(&text)
                .unwrap_or_else(|e| die(&format!("{} 不是信封：{e}", input.display())));
            let bundle =
                decrypt_envelope(&env, &key).unwrap_or_else(|e| die(&e.user_message()));
            let out = serde_json::to_string_pretty(&bundle).unwrap_or_else(|e| die(&format!("{e}")));
            write_out(&args.out, &out);
        }
        "keygen" => {
            let k = new_key().unwrap_or_else(|e| die(&e.user_message()));
            let hex = hex_encode(&k);
            match args.out.as_deref() {
                None | Some("-") => println!("{hex}"),
                Some(path) => {
                    if let Err(e) = std::fs::write(path, &hex) {
                        die(&format!("写 {path} 失败：{e}"));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ =
                            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                    }
                    eprintln!("已写入 {path}（0600）。**请备份**：丢了就解不开已上传的密文。");
                }
            }
        }
        "deviceid" => {
            let d = new_device_id().unwrap_or_else(|e| die(&e.user_message()));
            println!("{d}");
        }
        "upload" => {
            let key_file = args
                .key_file
                .clone()
                .unwrap_or_else(|| die("upload 需要 --key-file（没有会生成一个）"));
            let token_file = args
                .token_file
                .clone()
                .unwrap_or_else(|| die("upload 需要 --token-file"));
            let key = read_key(&key_file, true);
            let token = read_token(&token_file);
            let state_path = args.state.clone();
            let mut state = load_or_init_state(&state_path, args.device.as_deref());
            let rows = match read_audit_files(&args.audit_paths()) {
                Ok(r) => r,
                Err(e) => die(&e.user_message()),
            };
            let now = now_unix();
            let today = utc_day(now);
            let (pending, skipped) = pending_days(
                &rows,
                state.last_uploaded_day.as_deref(),
                &today,
                MAX_CATCHUP_DAYS,
            );
            eprintln!(
                "设备 {}；今天(UTC) {today}；待传 {} 天；跳过 {} 天（太老）",
                state.device,
                pending.len(),
                skipped.len()
            );
            if args.dry_run {
                for d in &pending {
                    match bundle_for_day(d, &state.device, env!("CARGO_PKG_VERSION"), &rows) {
                        Some(b) => {
                            let plain = serde_json::to_vec(&b).map(|v| v.len()).unwrap_or(0);
                            println!("{d}\t{} 行\t明文约 {plain} 字节", b.counts.rows);
                        }
                        None => println!("{d}\t（空，不会上传）"),
                    }
                }
                eprintln!("--dry-run：一个请求都没发。");
                return;
            }
            let cfg = SyncConfig {
                enabled: true,
                base_url: args.base_url.clone(),
                token,
                device: state.device.clone(),
                key,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let transport = TlsTransport::new();
            let run = sync_once(
                &cfg,
                &transport,
                &mut state,
                &state_path,
                &rows,
                &today,
                now,
            );
            for d in &run.uploaded {
                println!("已上传 {d}");
            }
            if let Some(d) = &run.failed_day {
                eprintln!("卡在 {d}：{}", run.error.clone().unwrap_or_default());
                std::process::exit(1);
            }
            println!(
                "本次请求 {} 个；最后成功的天：{}",
                run.requests,
                state
                    .last_uploaded_day
                    .clone()
                    .unwrap_or_else(|| "（还没有）".to_string())
            );
        }
        "list" => {
            let token_file = args
                .token_file
                .clone()
                .unwrap_or_else(|| die("list 需要 --token-file"));
            let state = load_or_init_state(&args.state.clone(), args.device.as_deref());
            let cfg = SyncConfig {
                enabled: true,
                base_url: args.base_url.clone(),
                token: read_token(&token_file),
                device: state.device.clone(),
                key: [0u8; KEY_LEN],
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let transport = TlsTransport::new();
            let items = match audit_sync::list_remote(&transport, &cfg) {
                Ok(i) => i,
                Err(e) => die(&e.user_message()),
            };
            println!("设备 {} 在服务端有 {} 个对象：", state.device, items.len());
            for i in &items {
                let up = match i.uploaded_unix {
                    Some(u) => utc_day(u),
                    None => "（服务端没给时间）".to_string(),
                };
                println!("  {}  {} 行  {} 字节  上传于 {}  {}", i.day, i.rows, i.bytes, up, i.key);
            }
        }
        "revoke" => {
            let token_file = args
                .token_file
                .clone()
                .unwrap_or_else(|| die("revoke 需要 --token-file"));
            let state = load_or_init_state(&args.state.clone(), args.device.as_deref());
            let cfg = SyncConfig {
                enabled: true,
                base_url: args.base_url.clone(),
                token: read_token(&token_file),
                device: state.device.clone(),
                key: [0u8; KEY_LEN],
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let transport = TlsTransport::new();
            match audit_sync::revoke_remote(&transport, &cfg) {
                Ok(n) => {
                    println!("服务端确认删除了 {n} 个对象（设备 {}）。", state.device);
                    println!(
                        "本机状态文件没动：last_uploaded_day 还是 {:?}。要重传就先删掉它，或再同步一次（会从那天之后继续）。",
                        state.last_uploaded_day
                    );
                }
                Err(e) => die(&e.user_message()),
            }
        }
        other => die(&format!("不认识的子命令：{other}\n\n{}", usage())),
    }
    let _ = std::io::stdout().flush();
}
