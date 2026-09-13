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
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| Error::Store(format!("写 {} 失败: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Store(format!("替换 {} 失败: {e}", path.display())))?;
    Ok(())
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

    #[test]
    fn default_root_is_under_application_support() {
        let root = Store::default_root();
        assert!(root.to_string_lossy().contains("Application Support"));
        assert!(root.to_string_lossy().ends_with(crate::APP_IDENTIFIER));
    }
}
