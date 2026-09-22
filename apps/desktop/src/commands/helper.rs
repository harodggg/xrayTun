//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

#[tauri::command]
pub async fn probe_helper(state: State<'_, AppState>) -> Result<HelperAvailability, String> {
    let present = crate::helper_client::socket_present(std::path::Path::new(DEFAULT_SOCKET_PATH));
    let mut helper = state.helper.lock().await;
    // 强制重连，拿到最新状态。
    helper.disconnect();
    Ok(helper.availability(present))
}

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
pub(crate) fn parse_helper_version(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    let mut parts = line.split_whitespace();
    let name = parts.next()?;
    let version = parts.next()?;
    let looks_like_version = version.chars().next().is_some_and(|c| c.is_ascii_digit());
    (name == "xraytun-helper" && looks_like_version).then(|| version.to_string())
}

/// 读一个 helper 二进制**自报的**版本：直接执行它（`version` 子命令）。
///
/// * 读的是**实际工件**，不是「App 版本」这种间接推断 —— 包内那份与 App 版本
///   本来就会一起变，用 App 版本当包内版本会漏掉「包里带的其实是别的版本」；
/// * `version` 子命令是**纯打印**：clap 解析后 `println` 退出，不需要 root、
///   不连 socket、不碰任何系统配置（本机实测 p50 2.7ms）；
/// * 任何失败（文件不在 / 不能执行 / 老版本没有这个子命令 / 输出认不出来）
///   一律 `None` ⇒ 上层表达成「读不到」。
fn read_binary_version(binary: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new(binary)
        .arg("version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_helper_version(&String::from_utf8_lossy(&output.stdout))
}

/// 三态判定：**两边都读到才能比**。
///
/// 抽成纯函数是为了能测 —— 尤其是「**读不到不许猜成不一致**」这条反例。
pub(crate) fn classify_helper_versions(
    installed: Option<String>,
    bundled: Option<String>,
) -> HelperVersionCheck {
    match (installed, bundled) {
        (Some(i), Some(b)) if i == b => HelperVersionCheck::Match { version: i },
        (Some(i), Some(b)) => HelperVersionCheck::Mismatch {
            installed: i,
            bundled: b,
        },
        (installed, bundled) => {
            let reason = match (&installed, &bundled) {
                (None, None) => "已安装的助手与包内助手的版本都读不到".to_string(),
                (None, Some(_)) => "读不到已安装助手的版本（文件不存在或无法执行）".to_string(),
                (Some(_), None) => "读不到包内助手的版本（App 包里没有或无法执行）".to_string(),
                (Some(_), Some(_)) => unreachable!("两边都读到时上面已返回"),
            };
            HelperVersionCheck::Unreadable {
                installed,
                bundled,
                reason,
            }
        }
    }
}

/// 读**已安装**与**包内**两个 helper 的版本，判定三态（快照用）。
///
/// 全程**只读、无副作用、不需要管理员**：只执行两个二进制的 `version` 子命令，
/// 而且**稳态下一次都不执行**（见下面两个带指纹的缓存）。
/// **绝不做任何安装/重启动作** —— 重装是特权操作，必须由用户点界面上的按钮。
pub(crate) fn helper_version_check(app: &AppHandle) -> HelperVersionCheck {
    let installed = installed_helper_version();
    let bundled = bundled_helper_version(app);
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
    last: Option<(BinaryFingerprint, Option<String>)>,
}

impl VersionCache {
    pub(crate) fn get_or_read(
        &mut self,
        fingerprint: BinaryFingerprint,
        read: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
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
fn installed_helper_version() -> Option<String> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<VersionCache>> = std::sync::OnceLock::new();
    let path = std::path::Path::new(xt_proto::HELPER_INSTALLED_PATH);
    let fingerprint = binary_fingerprint(path);
    let mut cache = CACHE
        .get_or_init(|| std::sync::Mutex::new(VersionCache::default()))
        .lock()
        .ok()?;
    cache.get_or_read(fingerprint, || read_binary_version(path))
}

/// **App 包内** helper 的版本。
///
/// 它在 bundle 里、运行期本不会变，但仍用**同一套指纹缓存**而不是「永久缓存一次」：
/// 一是自更新替换 App 的瞬间可能读不到，二是「读不到」绝不能成为永久状态
/// （这条规矩对两边一视同仁）。稳态同样 0 次 spawn。
fn bundled_helper_version(app: &AppHandle) -> Option<String> {
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
        path.as_deref().and_then(read_binary_version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **三态**之一：版本相同 ⇒ 「一致」（反例：不许只要检测就提示）。
    #[test]
    fn same_version_is_match_not_mismatch() {
        assert_eq!(
            classify_helper_versions(Some("0.8.31".into()), Some("0.8.31".into())),
            HelperVersionCheck::Match {
                version: "0.8.31".into()
            },
            "版本相同必须判「一致」——否则就是狼来了",
        );
    }

    /// **三态**之二：版本不同 ⇒ 「不一致」（要提示重装）。
    #[test]
    fn different_version_is_mismatch() {
        assert_eq!(
            classify_helper_versions(Some("0.8.11".into()), Some("0.8.31".into())),
            HelperVersionCheck::Mismatch {
                installed: "0.8.11".into(),
                bundled: "0.8.31".into()
            },
            "磁盘上装的与包里带的不一样 ⇒ 不一致",
        );
    }

    /// **三态**之三（反例）：读不到**不许猜成不一致**，也不许猜成一致。
    #[test]
    fn unreadable_version_is_not_reported_as_mismatch() {
        for (installed, bundled) in [
            (None, None),
            (None, Some("0.8.31".to_string())),
            (Some("0.8.11".to_string()), None),
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
        let installed = classify_helper_versions(None, Some("0.8.31".into()));
        let bundled = classify_helper_versions(Some("0.8.31".into()), None);
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

    /// 版本行解析：**认不出来就不给版本**（宁可说读不到，也不要猜）。
    #[test]
    fn version_line_parsing_is_strict() {
        assert_eq!(
            parse_helper_version("xraytun-helper 0.8.31 (protocol 1)\n"),
            Some("0.8.31".into()),
        );
        assert_eq!(
            parse_helper_version("xraytun-helper 0.8.31"),
            Some("0.8.31".into())
        );
        // 认不出来的：空、缺字段、错程序名、版本不像版本
        assert_eq!(parse_helper_version(""), None);
        assert_eq!(parse_helper_version("xraytun-helper"), None, "没有版本字段");
        assert_eq!(
            parse_helper_version("some-other-tool 0.8.31"),
            None,
            "不是我们的程序"
        );
        assert_eq!(
            parse_helper_version("xraytun-helper abc"),
            None,
            "版本不像版本"
        );
    }

    /// 真实执行路径：**文件不在 ⇒ 读不到**（不是 panic、也不是编一个版本）。
    #[test]
    fn missing_binary_is_unreadable_instead_of_fatal() {
        let missing = std::path::Path::new("/nonexistent/xraytun-helper-probe");
        assert_eq!(read_binary_version(missing), None);
        // 能执行、但输出认不出来 ⇒ 同样是「读不到」
        assert_eq!(
            read_binary_version(std::path::Path::new("/usr/bin/true")),
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
            Some("0.8.31".to_string())
        };
        let before = BinaryFingerprint::Present {
            len: 4147568,
            mtime_secs: 1_700_000_000,
        };
        assert_eq!(cache.get_or_read(before.clone(), read), Some("0.8.31".into()));
        assert_eq!(cache.get_or_read(before.clone(), read), Some("0.8.31".into()));
        assert_eq!(cache.get_or_read(before, read), Some("0.8.31".into()));
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
        assert_eq!(cache.get_or_read(after, read), Some("0.8.31".into()));
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
            Some("0.8.32".to_string())
        };
        assert_eq!(
            cache.get_or_read(installed, read_ok),
            Some("0.8.32".into()),
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
