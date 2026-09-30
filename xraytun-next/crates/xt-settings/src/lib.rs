//! xt-settings —— 设置存储：单文件 + 原子替换（不引入数据库）
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 三条刻意的选择（理由见 docs/architecture/sections/b2-config.md）：
//!
//! 1. **单文件 JSON**：设置总量是三个字段，用数据库只换来一个必须迁移的 schema；
//!    用户还能直接用编辑器打开排查。
//! 2. **原子替换**：同目录写临时文件再 `rename`。核心进程/daemon 可能在任何时刻
//!    读这个文件，绝不能让它读到半截 JSON。
//! 3. **读坏文件如实报错**：只有「文件不存在」才等于「还没设置过，用默认值」。
//!    文件存在但读不动（权限、非 UTF-8、JSON 非法、字段类型不对）一律返回具体
//!    `ErrorBody` —— 静默重置会把用户的设置吃掉，还会让界面显示一个用户从没选过的
//!    状态（I3 无假话）。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{LogLevel, NodeId, SettingsPatch, SettingsView};

/// 落盘形状。字段与 `SettingsView` 一致，但持久化类型不直接复用 wire 类型：
/// wire 形态服务于消费者（将来可能加 UI 专用字段），文件形态服务于我们自己。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub socks_listen: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_node: Option<NodeId>,
    pub log_level: LogLevel,
}

impl Default for Settings {
    fn default() -> Self {
        let view = SettingsView::default();
        Self {
            socks_listen: view.socks_listen,
            selected_node: view.selected_node,
            log_level: view.log_level,
        }
    }
}

impl Settings {
    /// 供 IPC 应答使用。字段一一对应，没有计算出来的假值。
    pub fn view(&self) -> SettingsView {
        SettingsView {
            socks_listen: self.socks_listen.clone(),
            selected_node: self.selected_node.clone(),
            log_level: self.log_level,
        }
    }

    /// 局部更新：`None` = 不动这一项。与契约的 `SettingsPatch` 同语义。
    pub fn apply_patch(&mut self, patch: &SettingsPatch) {
        if let Some(v) = &patch.socks_listen {
            self.socks_listen = v.clone();
        }
        if let Some(v) = &patch.selected_node {
            self.selected_node = Some(v.clone());
        }
        if let Some(v) = patch.log_level {
            self.log_level = v;
        }
    }
}

/// 读取设置。
///
/// * 文件不存在 → 默认值（首次运行，这不是「重置」）；
/// * 其它任何失败 → `Err`，调用方必须把错误原样上报，**不许**在错误路径上构造默认值。
pub fn load(path: &Path) -> Result<Settings, ErrorBody> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(err) => {
            return Err(io_error("读取设置文件失败", path, &err));
        }
    };

    let text = String::from_utf8(bytes).map_err(|err| {
        bad_request("设置文件不是 UTF-8 文本，拒绝猜测内容")
            .with_detail(detail(path, err.to_string()))
    })?;

    serde_json::from_str::<Settings>(&text).map_err(|err| {
        bad_request("设置文件不是合法 JSON（不做任何重置）")
            .with_detail(detail(path, err.to_string()))
    })
}

/// 原子写入：先在同目录写临时文件，再 `rename` 覆盖目标。
///
/// 必须同目录：跨文件系统的 `rename` 会退化成复制+删除，就不是原子的。
pub fn save(path: &Path, settings: &Settings) -> Result<(), ErrorBody> {
    // 存不下一个能用的地址的设置是「注定失败的设置」：与其写进去让核心启动时
    // 才炸，不如在写入点就如实拒绝（错误带实际收到的字符串）。
    settings.socks_listen.trim().parse::<SocketAddr>().map_err(|err| {
        bad_request("socks_listen 不是合法的 host:port")
            .with_detail(serde_json::json!({
                "path": path.to_string_lossy(),
                "socks_listen": settings.socks_listen,
                "reason": err.to_string(),
            }))
    })?;

    let json = serde_json::to_string_pretty(settings).map_err(|err| {
        ErrorBody::new(ErrorCode::Internal, format!("设置序列化失败: {err}"))
    })?;

    let tmp = temp_path(path);
    if let Err(err) = std::fs::write(&tmp, json.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error("写临时设置文件失败", &tmp, &err));
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error("替换设置文件失败", path, &err));
    }
    Ok(())
}

/// 临时文件名带 pid + 进程内计数：同一 daemon 的两次 save 不互相覆盖，
/// 两个进程同时写也不会让对方 `rename` 到一个被删掉的临时文件。
fn temp_path(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "settings".into());
    path.with_file_name(format!("{name}.{}.{seq}.tmp", std::process::id()))
}

fn io_error(message: &str, path: &Path, err: &std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Io, format!("{message}: {err}"))
        .with_detail(serde_json::json!({ "path": path.to_string_lossy(), "kind": format!("{:?}", err.kind()) }))
}

fn detail(path: &Path, reason: String) -> serde_json::Value {
    serde_json::json!({ "path": path.to_string_lossy(), "reason": reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xt-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn missing_file_means_first_run_defaults() {
        let dir = tmp_dir("missing");
        let settings = load(&dir.join("settings.json")).expect("不存在应给默认值");
        assert_eq!(settings, Settings::default());
    }

    /// 核心断言：坏文件绝不被静默重置成默认值。
    #[test]
    fn corrupt_file_is_reported_and_never_reset_to_defaults() {
        let dir = tmp_dir("corrupt");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{\"socks_listen\": \"127.0.0.1:1080\"").expect("写坏文件");

        let err = load(&path).expect_err("截断的 JSON 必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
        assert!(err.message.contains("不是合法 JSON"), "{err:?}");
        let detail = err.detail.expect("错误必须带路径与原因");
        assert_eq!(detail["path"], serde_json::json!(path.to_string_lossy()));
        assert!(detail["reason"].as_str().is_some_and(|r| !r.is_empty()));

        // 文件保持原样：没有被「重置」成合法 JSON。
        assert_eq!(std::fs::read(&path).expect("读回"), b"{\"socks_listen\": \"127.0.0.1:1080\"");
    }

    /// 类型不对同样是「读不懂」，不能当默认值吞掉。
    #[test]
    fn wrong_typed_field_is_an_error() {
        let dir = tmp_dir("typed");
        let path = dir.join("settings.json");
        std::fs::write(&path, br#"{"socks_listen": 1080, "log_level": "info"}"#).expect("写文件");
        let err = load(&path).expect_err("类型错必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
    }

    #[test]
    fn valid_file_round_trips_without_loss() {
        let dir = tmp_dir("roundtrip");
        let path = dir.join("settings.json");
        let settings = Settings {
            socks_listen: "127.0.0.1:2080".into(),
            selected_node: Some(NodeId::new("aGVsbG8")),
            log_level: LogLevel::Debug,
        };
        save(&path, &settings).expect("保存");
        assert_eq!(load(&path).expect("读回"), settings);
        // 原子替换不留临时文件。
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("列目录")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件应已被 rename 掉");
    }

    #[test]
    fn save_refuses_an_unusable_listen_address() {
        let dir = tmp_dir("badaddr");
        let path = dir.join("settings.json");
        let settings = Settings { socks_listen: "not-an-address".into(), ..Default::default() };
        let err = save(&path, &settings).expect_err("非法地址必须被拒绝");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
        assert!(!path.exists(), "拒绝时不许留下半个文件");
    }

    #[test]
    fn patch_only_touches_given_fields() {
        let mut settings = Settings { selected_node: Some(NodeId::new("keep-me")), ..Default::default() };
        let patch = SettingsPatch { log_level: Some(LogLevel::Warn), ..Default::default() };
        settings.apply_patch(&patch);
        assert_eq!(settings.log_level, LogLevel::Warn);
        assert_eq!(settings.selected_node, Some(NodeId::new("keep-me")));
    }

    #[test]
    fn view_matches_persisted_fields() {
        let settings = Settings::default();
        let view = settings.view();
        assert_eq!(view.socks_listen, settings.socks_listen);
        assert_eq!(view.selected_node, settings.selected_node);
        assert_eq!(view.log_level, settings.log_level);
    }
}
