//! 会话快照的落盘 / 读取 / 清理。
//!
//! **为什么必须落盘，而不是只放在 helper 的内存里？**
//! 因为快照是「为了让 TUN 工作而对系统做的所有改动」的唯一账本：装过哪些路由、
//! 把 DNS 改成了什么。helper 可能被 `kill -9`、系统可能断电、用户可能强制退出，
//! 这些情况下内存里的账本随进程一起消失，下次启动就没人知道该撤销什么 ——
//! 用户会永久停在断网状态且无从恢复。这是这一类工具最严重的故障模式。
//!
//! 因此本模块的语义与普通配置读写有三点刻意的不同：
//!
//! 1. **原子写**：先写 `*.tmp` 再 `rename`。断电只可能留下旧快照或新快照，
//!    绝不会留下半截 JSON；半截 JSON 恰恰是回滚时最读不出来的东西。
//! 2. **解析失败必须报错**：文件存在但读不出来，绝不能当成「没有快照」。
//!    损坏发生在磁盘上，而这正是回滚最需要它的时候，静默吞掉等于放弃恢复。
//! 3. **清理幂等**：文件不存在时 `clear` 返回 `Ok(())`，让「收尾」可以被重复调用
//!    而不必先探测文件是否存在（探测本身就是一次 TOCTOU 竞态）。
//!
//! 快照文件的路径由调用方给定的 `state_dir` 决定（helper 传的是
//! [`super::DEFAULT_STATE_DIR`]），本模块不假设任何全局目录。

use std::path::{Path, PathBuf};

use xt_contract::error::{ErrorBody, ErrorCode};

use crate::model::Snapshot;

/// 快照文件名。与调用方传入的 `state_dir` 拼成最终路径。
pub(crate) const SNAPSHOT_FILE: &str = "helper-session.json";

/// 快照文件路径 = `state_dir/helper-session.json`。
fn snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SNAPSHOT_FILE)
}

/// 把 IO 失败包成 `ErrorCode::Io`，并带上出错的绝对路径（排查时最关键的一条信息）。
fn io_error(action: &str, path: &Path, e: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, format!("{action} {} 失败：{e}", path.display()))
        .with_detail(serde_json::json!({ "path": path.display().to_string() }))
}

/// 落盘。**每次状态变更后都应调用**（增量记账：每成功一步落一次盘，
/// 这样「装完第 3 条路由时崩溃」也能在重启后精确回滚那 3 条）。
pub(crate) fn save(snap: &Snapshot, state_dir: &Path) -> Result<(), ErrorBody> {
    // 目录可能还不存在（首次启动 / 测试里的临时目录）。
    std::fs::create_dir_all(state_dir)
        .map_err(|e| io_error("创建快照目录", state_dir, e))?;

    let path = snapshot_path(state_dir);
    let bytes = serde_json::to_vec_pretty(snap)
        .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("序列化快照失败：{e}")))?;

    // 原子写：写临时文件 → 收紧权限 → rename 覆盖正式文件。
    // 权限收紧放在 rename **之前**：正式文件一出现就已经是 0o600，
    // 不存在「已被读到但还没 chmod」的窗口。
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| io_error("写临时快照", &tmp, e))?;
    set_owner_only(&tmp);
    if let Err(e) = std::fs::rename(&tmp, &path) {
        // rename 失败时清掉临时文件，避免留下会让人误判的残骸（尽力而为）。
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error("替换快照", &path, e));
    }
    Ok(())
}

/// 读取快照。
///
/// - 文件不存在 → `Ok(None)`：这是「没有残留会话」的正常路径。
/// - 文件存在但 JSON 解析失败 → `Err`：快照损坏是回滚最需要它的时候，
///   **不许**静默降级成 `None`，否则系统上的路由与 DNS 就永远撤不掉了。
///
/// `Snapshot` 的字段都带 `#[serde(default)]`，所以旧版本写下的、
/// 缺字段的快照依然能读出来 —— 这正是「升级后第一次启动」要回滚旧会话的场景。
pub(crate) fn load(state_dir: &Path) -> Result<Option<Snapshot>, ErrorBody> {
    let path = snapshot_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error("读快照", &path, e)),
    };

    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        ErrorBody::new(
            ErrorCode::Io,
            format!("快照损坏：{} 无法解析（{e}）", path.display()),
        )
        .with_detail(serde_json::json!({ "path": path.display().to_string() }))
    })
}

/// 删除快照。**幂等**：文件不存在同样返回 `Ok(())`。
pub(crate) fn clear(state_dir: &Path) -> Result<(), ErrorBody> {
    let path = snapshot_path(state_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error("删除快照", &path, e)),
    }
}

/// 把快照权限收紧到 owner-only（0o600）：里面有 DNS 备份、接口地址等信息。
///
/// **有意忽略失败**：到这一步字节已经安全落盘了，若因为 chmod 失败而返回 `Err`，
/// 调用方会误以为「没落盘」而放弃回滚 —— 那比权限松一点危险得多。
/// 而且 `state_dir` 本身就是 root-only 的系统目录（见 [`super::DEFAULT_STATE_DIR`]）。
fn set_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cidr, DnsBackup, InstalledRoute, PhysicalUplink, RouteVia, SessionState};

    /// 每个测试用独立子目录，避免同一进程内并行跑测试时互相删文件。
    fn temp_state_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("xt-macosnet-snapshot-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> Snapshot {
        Snapshot {
            session_id: "sess-1".into(),
            interface: "utun4".into(),
            state: SessionState::BringingUp,
            addresses: vec![Cidr::parse("198.18.0.1/15").unwrap()],
            installed_routes: vec![InstalledRoute {
                destination: Cidr::parse("192.168.1.0/24").unwrap(),
                via: RouteVia::Gateway { addr: "192.168.1.1".parse().unwrap() },
                replaced: None,
            }],
            pending_routes: vec![InstalledRoute {
                destination: Cidr::parse("0.0.0.0/1").unwrap(),
                via: RouteVia::Interface { name: "utun4".into() },
                replaced: None,
            }],
            dns_servers: vec!["1.1.1.1".into()],
            physical: Some(PhysicalUplink { interface: "en0".into(), gateway: None }),
            dns: Some(DnsBackup {
                service: "Wi-Fi".into(),
                servers: vec!["192.168.1.1".into()],
            }),
        }
    }

    #[test]
    fn save_then_load_roundtrips_and_locks_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_state_dir("roundtrip");
        let snap = sample();

        save(&snap, &dir).expect("save 应当成功");
        let back = load(&dir).expect("load 应当成功").expect("快照应当存在");
        assert_eq!(back, snap, "快照必须原样 roundtrip");

        let mode = std::fs::metadata(snapshot_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "快照权限必须是 owner-only");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_none() {
        let dir = temp_state_dir("missing");
        assert_eq!(load(&dir).expect("不存在不是错误"), None);
    }

    #[test]
    fn load_corrupt_file_is_an_error_not_none() {
        let dir = temp_state_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(snapshot_path(&dir), b"{ this is not json").unwrap();

        let err = load(&dir).expect_err("损坏的快照绝不能静默变成 None");
        assert_eq!(err.code, ErrorCode::Io);
        assert!(err.message.contains("快照损坏"), "消息应说明快照损坏：{}", err.message);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 升级后的第一次启动：旧版快照缺 `addresses` / `pending_routes` / `dns_servers`
    /// 等字段，靠 `#[serde(default)]` 仍须能读出来 —— 那一刻正是回滚现场。
    #[test]
    fn an_old_snapshot_without_defaulted_fields_still_loads() {
        let dir = temp_state_dir("old-fields");
        std::fs::create_dir_all(&dir).unwrap();
        let old = serde_json::json!({
            "session_id": "s-old",
            "interface": "utun4",
            "state": "up"
        });
        std::fs::write(
            snapshot_path(&dir),
            serde_json::to_vec_pretty(&old).unwrap(),
        )
        .unwrap();

        let back = load(&dir)
            .expect("旧快照必须能读")
            .expect("文件存在就应当有快照");
        assert_eq!(back.session_id, "s-old");
        assert_eq!(back.state, SessionState::Up);
        assert!(back.addresses.is_empty());
        assert!(back.installed_routes.is_empty());
        assert!(back.pending_routes.is_empty());
        assert!(back.dns_servers.is_empty());
        assert!(back.physical.is_none());
        assert!(back.dns.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = temp_state_dir("clear");
        // 从没写过也能清。
        clear(&dir).expect("空目录上 clear 应当成功");

        save(&sample(), &dir).expect("save 应当成功");
        clear(&dir).expect("clear 应当成功");
        assert_eq!(load(&dir).unwrap(), None);
        // 再清一次仍然成功（幂等）。
        clear(&dir).expect("重复 clear 应当成功");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
