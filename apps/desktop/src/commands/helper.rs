//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn install_helper(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::install_script(&app)?;
    crate::helper_install::run_with_admin(&script, "安装 XrayTun 网络配置助手")?;
    state.log("app", "info", "helper 安装完成");
    snapshot::build_snapshot(&app, &state).await
}

/// 重启 helper。
///
/// 对应 UI 上「helper 已安装但进程没在运行」那个状态的一键修复。
#[tauri::command]
pub async fn restart_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::restart_script();
    crate::helper_install::run_with_admin(&script, "重启 XrayTun 网络配置助手")?;
    // 连接状态可能已变，强制重连一次。
    state.helper.lock().await.disconnect();
    state.log("app", "info", "helper 已重启");
    snapshot::build_snapshot(&app, &state).await
}

#[tauri::command]
pub async fn uninstall_helper(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let script = crate::helper_install::uninstall_script();
    crate::helper_install::run_with_admin(&script, "卸载 XrayTun 网络配置助手")?;
    state.log("app", "warn", "helper 已卸载");
    snapshot::build_snapshot(&app, &state).await
}

/// 回滚磁盘上遗留的会话。网络出问题时的「一键修复」。
#[tauri::command]
pub async fn restore_stale(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let mut helper = state.helper.lock().await;
    let response = helper.call(&Request::Restore);
    drop(helper);

    match response {
        Ok(_) => {
            state.log("app", "info", "已请求 helper 回滚遗留会话");
        }
        Err(e) => {
            state.log("app", "error", format!("回滚失败：{}", e.message));
            return Err(e.message);
        }
    }
    snapshot::build_snapshot(&app, &state).await
}

// ---------------------------------------------------------------------------
// task-84：已安装 helper vs App 包内 helper 的版本对照
//
// **为什么不能省**：App 更新**不会**刷新特权 helper（`restart_helper` 只
// `kickstart` 磁盘上那份旧二进制，只有 `install_helper` 会把包内那份拷过去），
// 而**路由/DNS 的安装与回滚都在 helper 里** —— 于是「我更新了 App」并不等于
// 「helper 侧修复生效了」，而且这件事完全无声（只校验协议号，不校验版本）。
// ---------------------------------------------------------------------------

use crate::state::HelperVersionCheck;

/// 从 `<binary> version` 的输出里取版本号。
///
/// 期望形如 `xraytun-helper 0.8.31 (protocol 1)`（`xt-helper/src/main.rs` 的
/// `Version` 子命令）。**认不出来就返回 `None`** —— 调用方据此如实说「读不到」，
/// 而不是猜一个版本出来。
/// 一个 helper 二进制**自报**的两件事：包版本 + 协议号。
///
/// # 为什么必须把协议号读出来（task-111）
///
/// **包版本 ≠ 兼容性。** v0.8.34 只改了 App、helper 一行没改（依赖图核过：
/// `crates/xt-helper` 只依赖 `xt-proto + xt-tun`），但包版本从 0.8.33 变成 0.8.34
/// ⇒ 拿「包版本相等」当判据，设置页每个只改 App 的版本都会喊一次「助手版本不匹配」。
/// 那是**误报**：危害不是功能，而是训练用户忽略警告（真不匹配时就没人看了）。
/// 真正决定双方能不能对话的是**协议号**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperProbe {
    /// `xraytun-helper 0.8.33 (protocol 1)` 里的 `0.8.33`。
    pub version: String,
    /// 同一行里的 `protocol 1`；老/异种二进制可能没有 ⇒ `None`（按**不可比**处理）。
    pub protocol: Option<u32>,
}

/// 从整行里取 `protocol <N>`；没有就是 `None`（**不猜**）。
pub(crate) fn parse_helper_protocol(line: &str) -> Option<u32> {
    let rest = line.split("(protocol ").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 解析 `version` 子命令的输出：`xraytun-helper 0.8.33 (protocol 1)`。
pub(crate) fn parse_helper_probe(output: &str) -> Option<HelperProbe> {
    let line = output.lines().next()?.trim();
    let mut parts = line.split_whitespace();
    let name = parts.next()?;
    let version = parts.next()?;
    let looks_like_version = version.chars().next().is_some_and(|c| c.is_ascii_digit());
    if name != "xraytun-helper" || !looks_like_version {
        return None;
    }
    Some(HelperProbe {
        version: version.to_string(),
        protocol: parse_helper_protocol(line),
    })
}


/// **兼容性判据：协议号相等。**
///
/// # 依据（都指到具体代码行）
///
/// * 协议号的**唯一来源**是 `xt-proto` 的 `PROTOCOL_VERSION`
///   （`crates/xt-proto/src/lib.rs:32`）。App 与 helper **各自把自己编译时链接的
///   那一份**报出来：helper 在 `version` 输出里（`crates/xt-helper/src/main.rs:77`），
///   App 在握手时发 `protocol: PROTOCOL_VERSION`（`apps/desktop/src/helper_client.rs:116`），
///   helper 也回自己的那一份（`crates/xt-helper/src/main.rs:235`）
///   ⇒ **握手本来就是拿协议号当兼容键**，这里只是把它用到版本检查上。
/// * 判据用**相等**而不是 `>=`：协议是 wire contract，App 侧对新字段/新请求的期望
///   是**编译进这份二进制**的；「App 更新」与「助手更新」谁先谁后都可能不兼容，
///   `>=` 会放过「App 旧、helper 新」这一半。
/// * 协议号读不到时**保守**：退回「包版本相等」。因为那意味着对面不是我们认识的
///   二进制（老版本没这行、或输出被改过）⇒ 此时**宁可按不兼容处理**（真旧助手仍报警）。
pub(crate) fn helper_versions_are_compatible(
    installed: &HelperProbe,
    bundled: &HelperProbe,
) -> bool {
    match (installed.protocol, bundled.protocol) {
        (Some(a), Some(b)) => a == b,
        _ => installed.version == bundled.version,
    }
}

/// 读一个 helper 二进制**自报的**版本：直接执行它（`version` 子命令）。
///
/// * 读的是**实际工件**，不是「App 版本」这种间接推断 —— 包内那份与 App 版本
///   本来就会一起变，用 App 版本当包内版本会漏掉「包里带的其实是别的版本」；
/// * `version` 子命令是**纯打印**：clap 解析后 `println` 退出，不需要 root、
///   不连 socket、不碰任何系统配置（本机实测 p50 2.7ms）；
/// * 任何失败（文件不在 / 不能执行 / 老版本没有这个子命令 / 输出认不出来）
///   一律 `None` ⇒ 上层表达成「读不到」。
fn read_binary_probe(binary: &std::path::Path) -> Option<HelperProbe> {
    let output = std::process::Command::new(binary)
        .arg("version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_helper_probe(&String::from_utf8_lossy(&output.stdout))
}

/// 三态判定：**两边都读到才能比**；比的是**协议兼容性**（task-111），不是包版本相等。
///
/// 三种情形（都有测试）：
/// * 协议相同（包版本可以不同）⇒ `Match`（**界面不该提示**）；
/// * 协议不同 / 协议号读不到且包版本也不同 ⇒ `Mismatch`（**真不兼容，照样要求重装**）；
/// * 任一边读不到 ⇒ `Unreadable`（**不许猜成不一致** —— task-84 的反例）。
///
/// 抽成纯函数是为了能测。
pub(crate) fn classify_helper_versions(
    installed: Option<HelperProbe>,
    bundled: Option<HelperProbe>,
) -> HelperVersionCheck {
    match (installed, bundled) {
        (Some(i), Some(b)) if helper_versions_are_compatible(&i, &b) => {
            // 报**已安装**那份的版本：那才是实际在跑的助手。
            HelperVersionCheck::Match { version: i.version }
        }
        (Some(i), Some(b)) => HelperVersionCheck::Mismatch {
            installed: i.version,
            bundled: b.version,
        },
        (installed, bundled) => {
            let reason = unreadable_reason(&installed, &bundled);
            HelperVersionCheck::Unreadable {
                installed: installed.map(|p| p.version),
                bundled: bundled.map(|p| p.version),
                reason,
            }
        }
    }
}

/// 「读不到」的三种（理论上四种）情形各自的可读理由。
///
/// 单拎出来是为了让第四支 —— 两个探针**都读到了**、却仍落到 `Unreadable` ——
/// 能被直接测到：它经由 [`classify_helper_versions`] 不可达（上面两个
/// `(Some, Some)` 分支会先接住），但它接的是「兼容性判据自身异常」这条兜底路径。
///
/// **为什么不是 `unreachable!()`**：release profile 是 `panic = "abort"`，
/// 一个不可达分支会变成 SIGABRT（用户只看到 `abort() called`，0.8.38 的形态）。
/// 这里退化成一句可读的「下一步做什么」，与另外三支同级。
fn unreadable_reason(installed: &Option<HelperProbe>, bundled: &Option<HelperProbe>) -> String {
    match (installed, bundled) {
        (None, None) => "已安装的助手与包内助手的版本都读不到".to_string(),
        (None, Some(_)) => "读不到已安装助手的版本（文件不存在或无法执行）".to_string(),
        (Some(_), None) => "读不到包内助手的版本（App 包里没有或无法执行）".to_string(),
        (Some(i), Some(b)) => format!(
            "助手版本的兼容性判据没有给出结论（已安装 {}，包内 {}）—— \
             请按「读不到版本」处理，并重装助手后重试",
            i.version, b.version
        ),
    }
}

/// 读**已安装**与**包内**两个 helper 的版本与协议号，判定三态（快照用）。
///
/// 全程**只读、无副作用、不需要管理员**：只执行两个二进制的 `version` 子命令，
/// 而且**稳态下一次都不执行**（见下面两个带指纹的缓存）。
/// **绝不做任何安装/重启动作** —— 重装是特权操作，必须由用户点界面上的按钮。
pub(crate) fn helper_version_check(app: &AppHandle) -> HelperVersionCheck {
    let installed = installed_helper_probe();
    let bundled = bundled_helper_probe(app);
    classify_helper_versions(installed, bundled)
}

/// 一个 helper 二进制「现在长什么样」—— 缓存键。
///
/// 用它当键、而不是「只读一次就永久缓存」：**重装助手会换掉这个二进制**，
/// 指纹一变就重读 ⇒ **「读不到」不会被永久缓存**（用户重装后界面必须能立刻改口）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BinaryFingerprint {
    /// 文件不在（没装 / 被删 / 路径变了）。
    Missing,
    /// 文件在：`(大小, mtime 秒)`。
    Present { len: u64, mtime_secs: i64 },
}

/// 取一个二进制的指纹。只做一次 `metadata`（微秒级），不 spawn。
pub(crate) fn binary_fingerprint(path: &std::path::Path) -> BinaryFingerprint {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime_secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            BinaryFingerprint::Present {
                len: m.len(),
                mtime_secs,
            }
        }
        Err(_) => BinaryFingerprint::Missing,
    }
}

/// 「按指纹缓存版本」的小缓存。
///
/// 纯逻辑（读函数由调用方注入）⇒ **可测**：同一指纹只读一次；指纹一变必须重读；
/// 「读不到」（`None`）同样按指纹缓存，并在指纹变化时重读。
#[derive(Default)]
pub(crate) struct VersionCache {
    last: Option<(BinaryFingerprint, Option<HelperProbe>)>,
}

impl VersionCache {
    pub(crate) fn get_or_read(
        &mut self,
        fingerprint: BinaryFingerprint,
        read: impl FnOnce() -> Option<HelperProbe>,
    ) -> Option<HelperProbe> {
        if let Some((cached_fp, cached_version)) = &self.last {
            if *cached_fp == fingerprint {
                return cached_version.clone();
            }
        }
        let version = read();
        self.last = Some((fingerprint, version.clone()));
        version
    }
}

/// **已安装** helper 的版本：按二进制指纹缓存。
///
/// 稳态（没重装）**0 次 spawn**；重装（大小/mtime 变）自动重读。
fn installed_helper_probe() -> Option<HelperProbe> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<VersionCache>> = std::sync::OnceLock::new();
    let path = std::path::Path::new(xt_proto::HELPER_INSTALLED_PATH);
    let fingerprint = binary_fingerprint(path);
    let mut cache = CACHE
        .get_or_init(|| std::sync::Mutex::new(VersionCache::default()))
        .lock()
        .ok()?;
    cache.get_or_read(fingerprint, || read_binary_probe(path))
}

/// **App 包内** helper 的版本。
///
/// 它在 bundle 里、运行期本不会变，但仍用**同一套指纹缓存**而不是「永久缓存一次」：
/// 一是自更新替换 App 的瞬间可能读不到，二是「读不到」绝不能成为永久状态
/// （这条规矩对两边一视同仁）。稳态同样 0 次 spawn。
fn bundled_helper_probe(app: &AppHandle) -> Option<HelperProbe> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<VersionCache>> = std::sync::OnceLock::new();
    let path = crate::helper_install::helper_binary_path(app).ok();
    let fingerprint = match &path {
        Some(p) => binary_fingerprint(p),
        None => BinaryFingerprint::Missing,
    };
    let mut cache = CACHE
        .get_or_init(|| std::sync::Mutex::new(VersionCache::default()))
        .lock()
        .ok()?;
    cache.get_or_read(fingerprint, || {
        path.as_deref().and_then(read_binary_probe)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只取版本串（测试里读起来更短；实现已统一到 `parse_helper_probe`）。
    fn version_of(output: &str) -> Option<String> {
        parse_helper_probe(output).map(|p| p.version)
    }

    /// 造一个探针（测试用）。
    fn probe(version: &str, protocol: Option<u32>) -> Option<HelperProbe> {
        Some(HelperProbe {
            version: version.into(),
            protocol,
        })
    }

    /// **task-173 跨语言契约**：Python（`scripts/helper_tristate.py`）与本文件**读同一份夹具**
    /// `scripts/fixtures/helper-version-cases.json`。**Rust 是权威**，夹具是双方共同的真源；
    /// 夹具里任一 `expect_state` 被改坏 ⇒ 本用例与 Python 自测**都必须红**。
    ///
    /// 用 `read_to_string`（不是 `include_str!`）：改夹具不必重编译就能被发现；
    /// **文件缺失即失败**（夹具是共同真源，缺了不许静默跳过）。
    #[test]
    fn helper_version_cases_fixture_matches_authoritative_rule() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/fixtures/helper-version-cases.json");
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("读不到共享夹具 {}（共同真源，缺了必须红）：{e}", path.display())
        });
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(&raw).expect("共享夹具必须是 JSON 数组");
        assert!(!cases.is_empty(), "共享夹具不许为空");
        for c in &cases {
            let name = c["name"].as_str().expect("夹具每条都要有 name");
            let inst = parse_helper_probe(c["installed_out"].as_str().unwrap_or(""));
            let bund = parse_helper_probe(c["bundled_out"].as_str().unwrap_or(""));
            let ip = inst.as_ref().and_then(|p| p.protocol);
            let bp = bund.as_ref().and_then(|p| p.protocol);
            let state = match classify_helper_versions(inst, bund) {
                HelperVersionCheck::Match { .. } => "Match",
                HelperVersionCheck::Mismatch { .. } => "Mismatch",
                HelperVersionCheck::Unreadable { .. } => "Unreadable",
            };
            let reason = if state == "Unreadable" {
                "unreadable"
            } else if ip.is_some() && bp.is_some() {
                if state == "Match" {
                    "protocol_equal"
                } else {
                    "protocol_differ"
                }
            } else if state == "Match" {
                "fallback_equal"
            } else {
                "fallback_differ"
            };
            assert_eq!(state, c["expect_state"].as_str().unwrap(), "夹具用例 state 不符：{name}");
            assert_eq!(reason, c["expect_reason"].as_str().unwrap(), "夹具用例 reason 不符：{name}");
        }
        println!("共享夹具 {} 条：Rust（权威）判定全部符合预期", cases.len());
    }

    /// **task-111 主回归**：包版本不同但**协议相同** ⇒ **不许**报不一致。
    ///
    /// 实测情形：v0.8.34 只改 App（helper 一行未改），包内 0.8.34 / 已装 0.8.33 ⇒
    /// 旧判据（包版本相等）让设置页每次都喊「助手版本不匹配」——**误报**。
    #[test]
    fn same_protocol_different_package_version_is_match() {
        assert_eq!(
            classify_helper_versions(probe("0.8.33", Some(1)), probe("0.8.34", Some(1))),
            HelperVersionCheck::Match {
                version: "0.8.33".into()
            },
            "协议相同 ⇒ 一致；报**已安装**那份的版本（那才是实际在跑的助手）",
        );
    }

    /// 版本与协议都相同 ⇒ 一致。
    #[test]
    fn same_version_is_match_not_mismatch() {
        assert_eq!(
            classify_helper_versions(probe("0.8.31", Some(1)), probe("0.8.31", Some(1))),
            HelperVersionCheck::Match {
                version: "0.8.31".into()
            },
            "版本相同必须判「一致」——否则就是狼来了",
        );
    }

    /// **真不兼容必须照样报警**：协议号不同 ⇒ 不一致（要求重装）。
    #[test]
    fn different_protocol_is_still_mismatch() {
        assert_eq!(
            classify_helper_versions(probe("0.8.33", Some(1)), probe("0.8.40", Some(2))),
            HelperVersionCheck::Mismatch {
                installed: "0.8.33".into(),
                bundled: "0.8.40".into()
            },
            "协议不同就是真不兼容 —— 这道检查的初衷（抓真旧助手）不许被削弱",
        );
        // 包版本**看起来一样**但协议不同，同样不兼容（不能因为版本串相同就放过）。
        assert!(
            matches!(
                classify_helper_versions(probe("0.8.33", Some(1)), probe("0.8.33", Some(2))),
                HelperVersionCheck::Mismatch { .. }
            ),
            "协议是兼容键，包版本相同也救不了协议不同",
        );
    }

    /// 协议号**读不到**时保守退回「包版本相等」：老/异种二进制仍会被抓出来。
    #[test]
    fn missing_protocol_falls_back_to_conservative_version_compare() {
        assert!(
            matches!(
                classify_helper_versions(probe("0.8.33", None), probe("0.8.34", None)),
                HelperVersionCheck::Mismatch { .. }
            ),
            "协议号读不到 + 包版本不同 ⇒ 保守按不兼容处理（对面不是我们认识的二进制）",
        );
        assert_eq!(
            classify_helper_versions(probe("0.8.33", None), probe("0.8.33", None)),
            HelperVersionCheck::Match {
                version: "0.8.33".into()
            },
        );
    }

    /// 协议号从 helper **自己那行**里取；取不到就是 `None`（不猜）。
    #[test]
    fn protocol_is_parsed_from_the_helper_line() {
        let p = parse_helper_probe("xraytun-helper 0.8.33 (protocol 1)\n").expect("应当能解析");
        assert_eq!(p.version, "0.8.33");
        assert_eq!(p.protocol, Some(1));
        let legacy = parse_helper_probe("xraytun-helper 0.8.33\n").expect("老格式也要能读出版本");
        assert_eq!(legacy.protocol, None, "没有协议号就是 None，不许猜成 1");
        assert_eq!(
            parse_helper_protocol("xraytun-helper 0.8.33 (protocol abc)"),
            None
        );
    }

    /// **三态**之三（反例）：读不到**不许猜成不一致**，也不许猜成一致。
    #[test]
    fn unreadable_version_is_not_reported_as_mismatch() {
        for (installed, bundled) in [
            (None, None),
            (None, probe("0.8.31", Some(1))),
            (probe("0.8.11", Some(1)), None),
        ] {
            let got = classify_helper_versions(installed.clone(), bundled.clone());
            assert!(
                matches!(got, HelperVersionCheck::Unreadable { .. }),
                "读不到时必须如实降级成 Unreadable，实际：{got:?}（installed={installed:?} bundled={bundled:?}）",
            );
            assert!(
                !matches!(got, HelperVersionCheck::Mismatch { .. }),
                "**不许**把「读不到」猜成「不一致」——那会让用户去重装一个没问题的助手",
            );
            assert!(
                !matches!(got, HelperVersionCheck::Match { .. }),
                "也不许猜成「一致」——那是没有证据时宣布好",
            );
        }
    }

    /// 读不到时要**说清是哪一边**读不到（否则用户不知道该修什么）。
    #[test]
    fn unreadable_reason_names_the_missing_side() {
        let both = classify_helper_versions(None, None);
        let installed = classify_helper_versions(None, probe("0.8.31", Some(1)));
        let bundled = classify_helper_versions(probe("0.8.31", Some(1)), None);
        let text = |c: &HelperVersionCheck| match c {
            HelperVersionCheck::Unreadable { reason, .. } => reason.clone(),
            other => panic!("应当是 Unreadable：{other:?}"),
        };
        assert!(text(&both).contains("都读不到"), "{}", text(&both));
        assert!(
            text(&installed).contains("已安装"),
            "要说清是已安装那份读不到：{}",
            text(&installed),
        );
        assert!(
            text(&bundled).contains("包内"),
            "要说清是包内那份读不到：{}",
            text(&bundled),
        );
    }

    /// task-1：**第四支兜底不许 panic**（原来写的是 `unreachable!("两边都读到时上面已返回")`）。
    ///
    /// 这一支经由 `classify_helper_versions` 不可达（两个 `(Some, Some)` 分支先接住），
    /// 所以单独把它拎成 `unreadable_reason` 才测得到。判别性：把实现换回
    /// `unreachable!()`，这条会 panic ⇒ 红。
    #[test]
    fn unreadable_reason_for_two_readable_probes_is_a_message_not_a_panic() {
        let reason = unreadable_reason(&probe("0.8.39", Some(2)), &probe("0.9.0", Some(1)));
        assert!(reason.contains("0.8.39"), "理由要带上已安装版本：{reason}");
        assert!(reason.contains("0.9.0"), "理由要带上包内版本：{reason}");
        assert!(
            reason.contains("重装") || reason.contains("重试"),
            "错误文案必须写「下一步做什么」：{reason}"
        );
    }

    /// 版本行解析：**认不出来就不给版本**（宁可说读不到，也不要猜）。
    #[test]
    fn version_line_parsing_is_strict() {
        assert_eq!(
            version_of("xraytun-helper 0.8.31 (protocol 1)\n"),
            Some("0.8.31".to_string()),
        );
        assert_eq!(
            version_of("xraytun-helper 0.8.31"),
            Some("0.8.31".to_string()),
            "只有版本串的老输出也要能读出版本（协议号另算）",
        );
        // 认不出来的：空、缺字段、错程序名、版本不像版本
        assert_eq!(version_of(""), None);
        assert_eq!(version_of("xraytun-helper"), None, "没有版本字段");
        assert_eq!(
            version_of("some-other-tool 0.8.31"),
            None,
            "不是我们的程序"
        );
        assert_eq!(
            version_of("xraytun-helper abc"),
            None,
            "版本不像版本"
        );
    }

    /// 真实执行路径：**文件不在 ⇒ 读不到**（不是 panic、也不是编一个版本）。
    #[test]
    fn missing_binary_is_unreadable_instead_of_fatal() {
        let missing = std::path::Path::new("/nonexistent/xraytun-helper-probe");
        assert_eq!(read_binary_probe(missing), None);
        // 能执行、但输出认不出来 ⇒ 同样是「读不到」
        assert_eq!(
            read_binary_probe(std::path::Path::new("/usr/bin/true")),
            None
        );
    }

    /// **防「读不到被静默丢掉」**：快照里必须真的把三态塞进去。
    ///
    /// 那个赋值点在 `snapshot.rs` 的 async 流程里（要 Tauri `AppHandle` 才跑得到），
    /// 纯函数测试证明不了「它还在」。源码级断言：删掉那行 → 本测试红。
    #[test]
    fn snapshot_still_reports_the_helper_version_check() {
        let prod = include_str!("snapshot.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        assert!(
            prod.contains("helper.version_check = helper_version_check(app)"),
            "快照必须把 helper 版本三态塞进去 —— 否则界面永远看不到「助手过旧」",
        );
    }

    // -----------------------------------------------------------------------
    // task-84 ②：进程 spawn 缓存（稳态 0 次 spawn，且「读不到」必须有失效路径）
    // -----------------------------------------------------------------------

    /// 指纹没变 ⇒ **只读一次**（稳态 0 次 spawn）；指纹一变（重装）⇒ 必须重读。
    #[test]
    fn version_cache_reads_once_and_only_rereads_when_the_fingerprint_changes() {
        let mut cache = VersionCache::default();
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            Some(HelperProbe {
                version: "0.8.31".to_string(),
                protocol: Some(1),
            })
        };
        let before = BinaryFingerprint::Present {
            len: 4147568,
            mtime_secs: 1_700_000_000,
        };
        assert_eq!(cache.get_or_read(before.clone(), read), probe("0.8.31", Some(1)));
        assert_eq!(cache.get_or_read(before.clone(), read), probe("0.8.31", Some(1)));
        assert_eq!(cache.get_or_read(before, read), probe("0.8.31", Some(1)));
        assert_eq!(
            reads.get(),
            1,
            "同一个二进制只该读一次 —— 稳态每次快照都起进程正是这条要消灭的",
        );

        // 重装：大小/mtime 变了 ⇒ 指纹变 ⇒ 必须重读
        let after = BinaryFingerprint::Present {
            len: 4149999,
            mtime_secs: 1_700_009_999,
        };
        assert_eq!(cache.get_or_read(after, read), probe("0.8.31", Some(1)));
        assert_eq!(reads.get(), 2, "二进制变了⇒必须重读（重装后要能改口）");
    }

    /// **「读不到」不许被永久缓存**：它同样按指纹缓存，指纹一变就必须重读。
    #[test]
    fn unreadable_is_not_cached_forever() {
        let mut cache = VersionCache::default();
        let reads = std::cell::Cell::new(0);
        let read_missing = || {
            reads.set(reads.get() + 1);
            None
        };
        // 没装（文件不在）
        assert_eq!(
            cache.get_or_read(BinaryFingerprint::Missing, read_missing),
            None
        );
        assert_eq!(
            cache.get_or_read(BinaryFingerprint::Missing, read_missing),
            None
        );
        assert_eq!(reads.get(), 1, "还是没装 ⇒ 用缓存，不必反复 spawn");

        // **用户重装了**：文件出现 ⇒ 指纹变 ⇒ 必须重读，而且必须能改口
        let installed = BinaryFingerprint::Present {
            len: 4147568,
            mtime_secs: 1_700_000_100,
        };
        let read_ok = || {
            reads.set(reads.get() + 1);
            Some(HelperProbe {
                version: "0.8.32".to_string(),
                protocol: Some(1),
            })
        };
        assert_eq!(
            cache.get_or_read(installed, read_ok),
            probe("0.8.32", Some(1)),
            "重装之后必须能读到新版本 —— 否则界面永远说「读不到」",
        );
        assert_eq!(reads.get(), 2);
    }

    /// 指纹本身：**文件出现/消失/变大**都要被看出来（重装就会变大小）。
    #[test]
    fn binary_fingerprint_notices_appearance_size_and_disappearance() {
        let dir = std::env::temp_dir().join(format!("xt-helper-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fake-helper");

        assert_eq!(binary_fingerprint(&f), BinaryFingerprint::Missing, "不存在 ⇒ Missing");

        std::fs::write(&f, b"abc").unwrap();
        let small = binary_fingerprint(&f);
        assert!(
            matches!(small, BinaryFingerprint::Present { len: 3, .. }),
            "应当带上大小：{small:?}",
        );

        // 大小变了（真实的重装就是换一个不同大小的二进制）⇒ 指纹必须变
        std::fs::write(&f, b"abcdefghij").unwrap();
        assert_ne!(small, binary_fingerprint(&f), "大小变了 ⇒ 指纹必须变");

        // 被删掉（卸载）⇒ 回到 Missing ⇒ 版本缓存会重读成「读不到」
        std::fs::remove_file(&f).unwrap();
        assert_eq!(binary_fingerprint(&f), BinaryFingerprint::Missing);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
