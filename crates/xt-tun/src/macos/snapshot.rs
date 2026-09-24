//! 会话快照：把「为了让 TUN 工作而对系统做的所有改动」记在磁盘上。
//!
//! 为什么必须落盘而不是只放内存？因为 helper 可能被 `kill -9`、
//! 系统可能断电、用户可能强制退出。如果副作用只记在内存里，下次启动时
//! 就没人知道「上次加了哪些路由、把 DNS 改成了什么」，用户会永久处于
//! 断网状态且无从恢复 —— 这是这类工具最严重的故障模式。
//!
//! 快照的写法也是**增量**的：每成功执行一步就落一次盘。这样即使在
//! 「加完第 3 条路由时崩溃」，重启后也能精确回滚那 3 条。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use xt_proto::InstalledRoute;

use crate::error::{Error, Result};
use crate::macos::dns::DnsBackup;
use crate::plan::PhysicalUplink;

/// 快照文件位置。放在系统级目录而不是用户目录，因为它是 root 写的，
/// 而且必须独立于任何用户会话（用户可能没登录）。
pub const SNAPSHOT_DIR: &str = "/Library/Application Support/XrayTun";
pub const SNAPSHOT_FILE: &str = "helper-session.json";

/// 快照目录。**生产固定为系统目录**；测试构建里可以被 [`with_test_root`] 临时
/// 换成临时目录 —— 否则「失败时快照是否被删」这条语义**没法在普通用户下验证**
/// （系统目录要 root 才能写）。
fn snapshot_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_SNAPSHOT_ROOT.with(|c| c.borrow().clone()) {
        return root;
    }
    PathBuf::from(SNAPSHOT_DIR)
}

#[cfg(test)]
thread_local! {
    /// 测试用的快照根（**只在测试构建里存在**）。
    static TEST_SNAPSHOT_ROOT: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// 在 `f()` 期间把快照根换成 `root`；**退出（含 panic）时自动还原**。
#[cfg(test)]
pub(crate) fn with_test_root<R>(root: &std::path::Path, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_SNAPSHOT_ROOT.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = TEST_SNAPSHOT_ROOT.with(|c| c.borrow_mut().replace(root.to_path_buf()));
    let _guard = Restore(prev);
    f()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 正在建立，中途崩溃需要回滚。
    BringingUp,
    /// 已建立并稳定运行。
    Up,
    /// 正在拆除。
    TearingDown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub state: SessionState,
    pub interface: String,
    /// 数据面进程 pid（helper 拉起的那种模式）。
    pub datapath_pid: Option<u32>,
    pub physical: PhysicalUplink,
    /// **按安装顺序**记录，回滚时倒序删除。
    pub installed_routes: Vec<InstalledRoute>,
    /// 两阶段启动里「已经算好但还没装」的路由（通常是接管默认路由的 `/1`）。
    ///
    /// 必须一起落盘：否则在 `TunUp` 与 `CommitRoutes` 之间崩溃时，
    /// 回滚逻辑不知道还有哪些路由可能已经被装上。
    #[serde(default)]
    pub pending_routes: Vec<InstalledRoute>,
    pub dns_backups: Vec<DnsBackup>,
    /// MITM 阶段安装过的**信任锚**（本地根证书）。
    ///
    /// `#[serde(default)]` **不能省**：磁盘上已有的快照没有这个字段，
    /// 少了它会让"升级后第一次启动"读不出旧快照 —— 而那正是回滚最需要它的时候。
    /// （有测试专门用一份**没有该字段**的旧快照 JSON 断言这一点。）
    #[serde(default)]
    pub trust_anchors: Vec<crate::macos::trust::TrustAnchorBackup>,
}

impl SessionSnapshot {
    pub fn new(session_id: String, interface: String, physical: PhysicalUplink) -> Self {
        let now = now_unix();
        Self {
            session_id,
            created_at: now,
            updated_at: now,
            state: SessionState::BringingUp,
            interface,
            datapath_pid: None,
            physical,
            installed_routes: Vec::new(),
            pending_routes: Vec::new(),
            dns_backups: Vec::new(),
            trust_anchors: Vec::new(),
        }
    }

    pub fn snapshot_path() -> PathBuf {
        snapshot_dir().join(SNAPSHOT_FILE)
    }

    /// 落盘。**每次状态变更后都应调用。**
    pub fn save(&mut self) -> Result<()> {
        self.updated_at = now_unix();
        let dir = snapshot_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Snapshot(format!("创建 {} 失败: {e}", dir.display())))?;

        let path = Self::snapshot_path();
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::Snapshot(format!("序列化快照失败: {e}")))?;

        // 原子写：先写临时文件再 rename，避免断电留下半截 JSON。
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| Error::Snapshot(format!("写快照失败: {e}")))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::Snapshot(format!("替换快照失败: {e}")))?;

        // 权限收紧到 root 可读写即可 —— 里面含 DNS 备份与接口信息。
        set_owner_only(&path);
        Ok(())
    }

    /// 读取快照。不存在返回 `Ok(None)`。
    pub fn load() -> Result<Option<Self>> {
        let path = Self::snapshot_path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::Snapshot(format!("快照文件损坏（{}）: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Snapshot(format!("读快照失败: {e}"))),
        }
    }

    pub fn clear() -> Result<()> {
        let path = Self::snapshot_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Snapshot(format!("删除快照失败: {e}"))),
        }
    }

    /// 是否需要回滚（上次没干净地拆掉）。
    pub fn is_stale(&self) -> bool {
        matches!(self.state, SessionState::BringingUp | SessionState::TearingDown)
            // 「已 Up 但还有未提交路由」也算未完成：说明两阶段启动中途崩了。
            || !self.pending_routes.is_empty()
    }
}

fn set_owner_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrips_through_json() {
        let physical = PhysicalUplink {
            interface: "en0".into(),
            gateway: Some("192.168.1.1".parse().unwrap()),
            service: Some("Wi-Fi".into()),
        };
        let mut snap = SessionSnapshot::new("s-1".into(), "utun4".into(), physical);
        snap.installed_routes.push(InstalledRoute {
            destination: "0.0.0.0/1".parse().unwrap(),
            via: xt_proto::RouteVia::Interface { name: "utun4".into() },
            replaced: None,
        });
        snap.dns_backups.push(DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec!["1.1.1.1".into()],
            search_domains: vec![],
        });

        snap.trust_anchors.push(crate::macos::trust::TrustAnchorBackup {
            fingerprint: "AB:CD".into(),
            cert_path: "/tmp/x.pem".into(),
            existed_before: false,
        });

        let json = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "s-1");
        assert_eq!(back.trust_anchors.len(), 1, "信任锚要能roundtrip");
        assert!(!back.trust_anchors[0].existed_before);
        assert_eq!(back.interface, "utun4");
        assert_eq!(back.installed_routes.len(), 1);
        assert_eq!(back.dns_backups[0].servers, vec!["1.1.1.1"]);
        assert_eq!(back.physical.service.as_deref(), Some("Wi-Fi"));
    }

    /// **升级后的第一次启动**：磁盘上的旧快照没有 `trust_anchors` 字段。
    /// 少了 `#[serde(default)]`，这里会直接解析失败 —— 而那一刻正是崩溃回滚的现场。
    #[test]
    fn an_old_snapshot_without_trust_anchors_still_loads() {
        let old = serde_json::json!({
            "session_id": "s-old",
            "created_at": 1,
            "updated_at": 2,
            "state": "up",
            "interface": "utun4",
            "datapath_pid": null,
            "physical": { "interface": "en0", "gateway": "192.168.1.1", "service": "Wi-Fi" },
            "installed_routes": [],
            "dns_backups": []
        });
        let back: SessionSnapshot =
            serde_json::from_value(old).expect("旧快照必须能读（缺字段走 default）");
        assert!(back.trust_anchors.is_empty());
        assert!(back.pending_routes.is_empty(), "pending_routes 也应当有 default");
    }

    #[test]
    fn bringing_up_state_is_stale() {
        let physical = PhysicalUplink { interface: "en0".into(), gateway: None, service: None };
        let mut snap = SessionSnapshot::new("s".into(), "utun4".into(), physical);
        assert!(snap.is_stale());
        snap.state = SessionState::Up;
        assert!(!snap.is_stale());
    }

    #[test]
    fn snapshot_path_is_system_wide() {
        let p = SessionSnapshot::snapshot_path();
        assert!(p.to_string_lossy().starts_with("/Library/Application Support/XrayTun"));
    }

    /// **一条正在生效的隧道不算「陈旧」，但依然必须能被清理。**
    ///
    /// 这条测试钉住一个真实事故的根因：`Request::Restore` 一度写成
    /// `restore_stale()`，于是被 `is_stale()` 挡在门外 —— 而正常连接的
    /// 会话恰恰就是这个状态（`Up` + `pending_routes` 已清空）。结果是
    /// 托盘「退出」与「修复网络」都对着一条活隧道回复「没有需要回滚的
    /// 会话」，路由和 DNS 全部留在系统上，用户点了「修复网络」也没用。
    ///
    /// 结论：**`is_stale()` 的语义是「崩在半路」，不是「需要清理」。**
    /// 需要清理的判据是快照里有没有记着改动。谁要是把 `Restore` 改回
    /// `restore_stale()`，请先读这条测试。
    #[test]
    fn a_committed_session_is_not_stale_but_still_needs_cleanup() {
        let physical = PhysicalUplink { interface: "en0".into(), gateway: None, service: None };
        let mut snap = SessionSnapshot::new("s".into(), "utun4".into(), physical);

        // 模拟一次完整的两阶段启动：pending 里的路由被提交进 installed。
        let destination = "0.0.0.0/1".parse().unwrap();
        let via = xt_proto::RouteVia::Interface { name: "utun4".into() };
        snap.pending_routes.push(InstalledRoute {
            destination,
            via: via.clone(),
            replaced: None,
        });
        snap.installed_routes.push(InstalledRoute {
            destination,
            via,
            replaced: None,
        });
        snap.pending_routes.clear();
        snap.state = SessionState::Up;

        // 崩在半路？不是。
        assert!(!snap.is_stale(), "正常连接的会话不应被判为陈旧");
        // 但系统上确实有我们装上去的东西，必须撤得掉。
        assert!(
            !snap.installed_routes.is_empty(),
            "快照里记着已安装的路由 —— 这正是「需要清理」的判据"
        );
    }
}
