//! 特权 helper（`xt-helperd`）的安装 / 卸载 / 重启 / 状态。
//!
//! # 为什么必须在壳里做
//!
//! 装它需要 root：写 `/Library/PrivilegedHelperTools`、写 `/Library/LaunchDaemons`、
//! `launchctl bootstrap system`。daemon 是**无特权**进程（权限边界见
//! `docs/architecture/00-CONTRACT-FREEZE.md` I5），界面更不能提权 —— 所以只剩壳这一处。
//!
//! # 用的是哪种安装方式（以及为什么不是 Apple 推荐的那种）
//!
//! **方式 B：`/Library/PrivilegedHelperTools` + `/Library/LaunchDaemons` + 一次管理员授权**
//! （`osascript … with administrator privileges`）。老仓库 v0.9.2 用的就是这一条，是被
//! ClashX 一类工具长期使用的形态；代价是**用户要输一次管理员密码**。
//!
//! Apple 推荐的方式 A（`SMAppService.daemon`）要求 app 与 daemon 用**同一 Team ID** 签名，
//! 而当前发行包只有 ad-hoc 签名（S5 签名/公证未做）—— 方式 A 在这种包上根本注册不上。
//! 所以这里不假装支持它；`docs/design/MACOS-APP-PLAN.md` 里「发行版切方式 A」仍是待办。
//!
//! # 安全口径（这些脚本会以 root 执行，所以写清楚）
//!
//! * 脚本里的每个变量都来自**我们自己的固定路径或 app bundle 路径**，不含任何用户输入；
//! * 脚本落到**私有临时目录**（0700，目录与文件都是），再用
//!   `/bin/sh <文件>` 执行 —— 不把脚本内容拼进 osascript 字符串（只有**路径**进 osascript，
//!   且做了转义）。老仓库同一条纪律。
//! * 路径里出现单引号会拒绝生成脚本（而不是靠转义去赌）。
//!
//! # 只有真机能验的部分
//!
//! 授权弹窗、`launchctl bootstrap` 的实际行为、装完能不能真的接管网络 —— 见
//! `docs/design/MACOS-APP-PLAN.md` S6：那三样只有用户真机做得了。这里能保证的是
//! 「脚本内容正确 + plist 形状正确 + 安装/卸载是同一套固定路径」，并用单测钉住。

use std::path::{Path, PathBuf};

/// launchd 的 label。它会出现在 `launchctl print system/<label>` 与 socket 名里。
pub const HELPER_LABEL: &str = "com.xraytun.xt-helperd";
/// 装入后的 helper 二进制位置（root:wheel 0755）。
pub const HELPER_INSTALLED_PATH: &str = "/Library/PrivilegedHelperTools/com.xraytun.xt-helperd";
/// LaunchDaemon plist 位置（root:wheel 0644）。
pub const HELPER_PLIST_PATH: &str = "/Library/LaunchDaemons/com.xraytun.xt-helperd.plist";
/// helper 的 AF_UNIX socket。**必须与 `crates/xt-daemon/src/flow.rs` 的 `HELPER_SOCKET` 逐字一致**，
/// 否则 daemon 连不上一个「装好了」的 helper —— 那是最难查的一类故障。
pub const HELPER_SOCKET_PATH: &str = "/var/run/xraytun-helper.sock";
/// helper 的状态目录（它是 root 进程，快照写这里）。
pub const HELPER_STATE_DIR: &str = "/Library/Application Support/XrayTun";
/// helper 的日志（排障时看这里）。
pub const HELPER_LOG_PATH: &str = "/Library/Logs/XrayTun/helper.log";
/// 覆盖 helper 二进制路径（开发/测试用）。
const HELPER_BIN_ENV: &str = "XT_HELPERD_BIN";
/// 覆盖 launchd label（开发/测试用：避免测试往真实的 `com.xraytun.xt-helperd` 上撞）。
const HELPER_LABEL_ENV: &str = "XT_HELPER_LABEL";

/// helper 当前状态。四个都是**客观事实**，没有推断值。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HelperStatus {
    /// 二进制在不在 `/Library/PrivilegedHelperTools/…`。
    pub installed: bool,
    /// LaunchDaemon plist 在不在。
    pub plist_present: bool,
    /// socket 文件在不在（**在 ≠ 有人听**：残留 socket 是常见故障态）。
    pub socket_present: bool,
    /// 现在能不能连上那个 socket（这才是「helper 真的在服务」的判据）。
    pub reachable: bool,
    /// 装它需要的那份二进制在 App 包里找不找得到（找不到 ⇒ 界面不给「安装」按钮）。
    pub binary_in_bundle: bool,
    /// **客观事实**：已安装的助手二进制与当前 App 包里那份是否逐字节不同。
    ///
    /// 为什么需要它：helper 是独立 launchd 服务，App 升级**不会**自动替换已经装好的
    /// helper；如果界面不提示，用户会「装了新版 App 但 helper 还是旧代码」——
    /// 真机上 v1.2.4 的 audit-token 校验就是这样撞上的。
    /// 只在「已安装 + 包内二进制存在」时有意义，其余情况为 false。
    pub update_available: bool,
}

/// launchd 的 label（可用环境变量覆盖，仅开发/测试用）。
pub fn helper_label() -> String {
    std::env::var(HELPER_LABEL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| HELPER_LABEL.to_string())
}

/// App 包里的 helper 二进制：`XT_HELPERD_BIN` → `Contents/Resources/xt-helperd` → 与可执行文件同级。
///
/// 与 `daemon_launch::resolve_binary` / `resolve_xray` 同一套布局判断。
pub fn helper_binary_path() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(HELPER_BIN_ENV) {
        if !raw.is_empty() {
            let candidate = PathBuf::from(raw);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("..").join("Resources").join("xt-helperd"),
        dir.join("xt-helperd"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// 读当前的四个客观事实。**不做任何推断**（例如不因为 plist 在就说「已生效」）。
pub fn status() -> HelperStatus {
    let bundled = helper_binary_path();
    let installed = Path::new(HELPER_INSTALLED_PATH).is_file();
    let update_available = match (&bundled, installed) {
        (Some(bundled), true) => !files_identical(bundled, Path::new(HELPER_INSTALLED_PATH)),
        _ => false,
    };
    HelperStatus {
        installed,
        plist_present: Path::new(HELPER_PLIST_PATH).is_file(),
        socket_present: Path::new(HELPER_SOCKET_PATH).exists(),
        reachable: std::os::unix::net::UnixStream::connect(HELPER_SOCKET_PATH).is_ok(),
        binary_in_bundle: bundled.is_some(),
        update_available,
    }
}

/// 两个文件是否逐字节相同。读不到任何一个都返回 `false`（推断不出相等，就不能说相同）。
fn files_identical(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// LaunchDaemon plist。
///
/// 几个关键项（照老仓库踩过的经验）：
/// * `RunAtLoad`：开机即起，GUI 启动时它一定已经被拉起过；
/// * `KeepAlive`：崩了自动回来（不加 `SuccessfulExit`：正常退出后也还要按需回来）；
/// * `StandardErrorPath`：helper 的日志都写这里，排障唯一入口；
/// * 命令行显式带上 `--socket` / `--state-dir`：不依赖 helperd 的默认值，
///   因为「daemon 期望的 socket」和「helperd 默认的 socket」是**两份常量**，
///   它们一旦漂移就是「装好了但连不上」。
pub fn plist_contents() -> String {
    let label = helper_label();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{HELPER_INSTALLED_PATH}</string>
        <string>--socket</string>
        <string>{HELPER_SOCKET_PATH}</string>
        <string>--state-dir</string>
        <string>{HELPER_STATE_DIR}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{HELPER_LOG_PATH}</string>
    <key>StandardErrorPath</key>
    <string>{HELPER_LOG_PATH}</string>
</dict>
</plist>
"#
    )
}

/// 安装脚本。`helper_src` 是 App 包里那份二进制（调用方保证不含单引号）。
pub fn install_script(helper_src: &str) -> String {
    let label = helper_label();
    let plist = plist_contents();
    format!(
        r#"#!/bin/sh
# XrayTun 特权助手安装脚本（由 App 生成，以 root 执行）。所有变量都来自固定路径。
set -eu

HELPER_SRC='{helper_src}'
HELPER_DST='{HELPER_INSTALLED_PATH}'
PLIST_DST='{HELPER_PLIST_PATH}'
LABEL='{label}'

if [ ! -x "$HELPER_SRC" ]; then
  echo "错误：找不到 helper 二进制：$HELPER_SRC" >&2
  exit 1
fi

mkdir -p /Library/Logs/XrayTun

# 先摘掉旧版本：launchd 会缓存旧的二进制路径，不摘就可能是「装了新的、跑的是旧的」。
launchctl bootout "system/$LABEL" 2>/dev/null || true
launchctl remove "$LABEL" 2>/dev/null || true
rm -f '{HELPER_SOCKET_PATH}'

mkdir -p /Library/PrivilegedHelperTools
# 用 install 而不是 cp：一次就保证属主 root:wheel 与权限 0755。
/usr/bin/install -o root -g wheel -m 0755 "$HELPER_SRC" "$HELPER_DST"

cat > "$PLIST_DST" <<'PLIST_EOF'
{plist}
PLIST_EOF

# plist 必须 root:wheel 0644，否则 launchd 拒绝加载。
chown root:wheel "$PLIST_DST"
chmod 0644 "$PLIST_DST"

launchctl bootstrap system "$PLIST_DST"
launchctl enable "system/$LABEL"

# `bootstrap` 是异步的：它返回时进程可能还没 bind 到 socket。
# 不等就报成功，用户会看到「安装成功」紧跟着「连不上」——纯时序造成的假故障。
i=0
while [ "$i" -lt 50 ]; do
  if launchctl print "system/$LABEL" 2>/dev/null | grep -q "state = running"; then
    break
  fi
  i=$((i + 1))
  sleep 0.2
done

if launchctl print "system/$LABEL" 2>/dev/null | grep -q "state = running"; then
  echo "helper 已安装并启动"
else
  echo "helper 已安装，但进程未在 10 秒内进入 running 状态" >&2
  echo "请查看 {HELPER_LOG_PATH}" >&2
  exit 1
fi
"#
    )
}

/// 卸载脚本：摘掉 launchd 注册 + 删 plist / 二进制 / 残留 socket。
/// **不留残留**是 S6 的验收项之一（`MACOS-APP-PLAN.md` §7 第 5 条）。
pub fn uninstall_script() -> String {
    let label = helper_label();
    format!(
        r#"#!/bin/sh
# XrayTun 特权助手卸载脚本（由 App 生成，以 root 执行）。
set -eu

LABEL='{label}'
PLIST_DST='{HELPER_PLIST_PATH}'
HELPER_DST='{HELPER_INSTALLED_PATH}'

launchctl bootout "system/$LABEL" 2>/dev/null || true
launchctl remove "$LABEL" 2>/dev/null || true
rm -f "$PLIST_DST"
rm -f "$HELPER_DST"
# socket 在 /var/run 下，重启会清掉；立刻删更干净，也避免残留 socket 让 daemon 误以为有人在听。
rm -f '{HELPER_SOCKET_PATH}'

echo "helper 已卸载"
"#
    )
}

/// 重启脚本。
///
/// 为什么值得单独有一个：**「socket 文件还在、但守护进程没跑」**（客户端拿到 ECONNREFUSED）
/// 是这类组件最常见的故障态，而修它只要 `launchctl kickstart -k`。让用户为此去终端敲命令
/// 是不合理的 —— 界面上就该有一个按钮。
pub fn restart_script() -> String {
    let label = helper_label();
    format!(
        r#"#!/bin/sh
# 重启 XrayTun 特权助手（由 App 生成，以 root 执行）。
set -eu

LABEL='{label}'
PLIST_DST='{HELPER_PLIST_PATH}'

# 先摘掉陈旧 socket：kickstart 之前的窗口里，它会让客户端拿到 ECONNREFUSED。
rm -f '{HELPER_SOCKET_PATH}'

if launchctl print "system/$LABEL" >/dev/null 2>&1; then
  launchctl kickstart -k "system/$LABEL"
else
  launchctl bootstrap system "$PLIST_DST"
fi

i=0
while [ "$i" -lt 50 ]; do
  if launchctl print "system/$LABEL" 2>/dev/null | grep -q "state = running"; then
    echo "helper 已重启"
    exit 0
  fi
  i=$((i + 1))
  sleep 0.2
done

echo "helper 未能进入 running 状态，请查看 {HELPER_LOG_PATH}" >&2
exit 1
"#
    )
}

/// 安装（会弹一次系统管理员授权框）。
pub fn install() -> Result<(), String> {
    let source = helper_binary_path()
        .ok_or_else(|| "App 包里没有 xt-helperd（打包版才会有）".to_string())?;
    let source = source.display().to_string();
    if source.contains('\'') || source.contains('\n') {
        // 不靠转义去赌：路径里出现这些字符说明环境不对劲，宁可不生成 root 脚本。
        return Err(format!("helper 路径含引号或换行，拒绝以 root 执行：{source}"));
    }
    run_with_admin(
        &install_script(&source),
        "XrayTun 需要安装特权助手（一次管理员授权），它只用来创建 VPN 网卡与改路由/DNS。",
    )
}

/// 卸载（同样要一次授权）。
pub fn uninstall() -> Result<(), String> {
    run_with_admin(
        &uninstall_script(),
        "XrayTun 需要管理员授权来卸载特权助手（会把它的二进制与 launchd 配置一起删掉）。",
    )
}

/// 重启（同样要一次授权）。
pub fn restart() -> Result<(), String> {
    run_with_admin(&restart_script(), "XrayTun 需要管理员授权来重启特权助手。")
}

/// 把脚本写到**私有**临时目录（目录与文件都是 0700），再用 osascript 提权执行。
///
/// 为什么不直接把脚本内容塞进 osascript：那要经过 AppleScript 字符串转义，
/// 而这段字符串会以 root 执行 —— 少一层转义少一类注入面（老仓库同一条纪律）。
fn run_with_admin(script: &str, prompt: &str) -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("xraytun-helper-{}", std::process::id()));
    let path = write_private_script(&dir, script)?;
    let result = run_osascript(&path, prompt);
    // 无论成败都清掉临时脚本：它没有任何理由留在磁盘上。
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 写脚本：目录 0700、文件 0700（`/tmp` 是全局可写的，不能给别的本机用户换文件的机会）。
fn write_private_script(dir: &Path, script: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).map_err(|error| format!("创建临时目录失败：{error}"))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("设置临时目录权限失败：{error}"))?;
    let path = dir.join("helper-install.sh");
    std::fs::write(&path, script).map_err(|error| format!("写临时脚本失败：{error}"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("设置临时脚本权限失败：{error}"))?;
    Ok(path)
}

/// 用 `/usr/bin/osascript` 请求授权并执行脚本；失败时把系统给的原文带出来
/// （「用户取消」与「脚本报错」是两件不同的事，必须能分辨）。
fn run_osascript(script_path: &Path, prompt: &str) -> Result<(), String> {
    let escaped_path = escape_applescript(&script_path.display().to_string());
    let escaped_prompt = escape_applescript(prompt);
    let applescript = format!(
        r#"do shell script "/bin/sh \"{}\"" with administrator privileges with prompt "{}""#,
        escaped_path, escaped_prompt
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&applescript)
        .output()
        .map_err(|error| format!("无法调用 osascript：{error}"))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        Err("管理员授权被取消（或系统未给出原因）".to_string())
    } else {
        Err(format!("操作未完成：{stderr}"))
    }
}

/// AppleScript 字符串转义：只处理它认的两类字符（反斜杠与双引号）。
fn escape_applescript(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Tauri 命令：helper 的当前状态（四个客观事实，不做推断）。
#[tauri::command]
pub fn helper_status() -> HelperStatus {
    status()
}

/// Tauri 命令：安装特权助手（会弹一次系统管理员授权框）。
#[tauri::command]
pub fn install_helper() -> Result<(), String> {
    install()
}

/// Tauri 命令：卸载特权助手（同样要一次授权；删二进制 + plist + 残留 socket）。
#[tauri::command]
pub fn uninstall_helper() -> Result<(), String> {
    uninstall()
}

/// Tauri 命令：重启特权助手（修「socket 在但没人听」这类故障态的按钮）。
#[tauri::command]
pub fn restart_helper() -> Result<(), String> {
    restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plist 必须把 helper 装到**我们固定的位置**、并且显式给出 socket 与 state-dir。
    /// 「daemon 期望的 socket」与「helperd 默认的 socket」是两份常量，漂移过一次就是
    /// 「装好了但连不上」，所以这里逐字钉住。
    #[test]
    fn plist_pins_label_paths_and_socket() {
        let plist = plist_contents();
        assert!(plist.contains(&format!("<string>{}</string>", helper_label())));
        assert!(plist.contains(HELPER_INSTALLED_PATH));
        assert!(plist.contains("--socket"));
        assert!(plist.contains(HELPER_SOCKET_PATH));
        assert!(plist.contains("--state-dir"));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("KeepAlive"));
        assert!(plist.contains(HELPER_LOG_PATH));
        // daemon 连的是这个 socket（crates/xt-daemon/src/flow.rs 的 HELPER_SOCKET）：
        // 两边任何一处改动而另一处没跟上，都会变成「装好了但连不上」。
        assert_eq!(HELPER_SOCKET_PATH, "/var/run/xraytun-helper.sock");
    }

    /// 安装脚本的要点：先摘旧的、install 带 root:wheel 0755、plist 0644、
    /// bootstrap 之后**等 running**（不等就会「安装成功 + 连不上」）。
    #[test]
    fn install_script_covers_the_traps() {
        let script = install_script("/Applications/XrayTun.app/Contents/Resources/xt-helperd");
        assert!(script.contains("launchctl bootout"), "必须先摘掉旧注册");
        assert!(script.contains("/usr/bin/install -o root -g wheel -m 0755"));
        assert!(script.contains("chmod 0644 \"$PLIST_DST\""));
        assert!(script.contains("launchctl bootstrap system"));
        assert!(script.contains("state = running"), "必须等它真的 running");
        assert!(script.contains("/Applications/XrayTun.app/Contents/Resources/xt-helperd"));
    }

    /// 卸载脚本必须**不留残留**（plist / 二进制 / socket 三样都要删）。
    #[test]
    fn uninstall_script_removes_everything() {
        let script = uninstall_script();
        assert!(script.contains("launchctl bootout"));
        assert!(script.contains(HELPER_PLIST_PATH));
        assert!(script.contains(HELPER_INSTALLED_PATH));
        assert!(script.contains(HELPER_SOCKET_PATH));
    }

    /// 脚本里只允许出现**我们自己的固定路径**：一旦有用户输入混进来，
    /// 这段文本就会以 root 执行。这里用一个最直接的判据：脚本里不含占位符残留。
    #[test]
    fn scripts_have_no_unresolved_placeholders() {
        for script in [install_script("/tmp/xt-helperd"), uninstall_script(), restart_script()] {
            assert!(!script.contains('{'), "脚本里不该留下未替换的占位符：{script}");
            assert!(!script.contains('}'), "脚本里不该留下未替换的占位符：{script}");
        }
    }

    #[test]
    fn applescript_escaping_handles_quotes_and_backslashes() {
        assert_eq!(escape_applescript(r#"/tmp/a"b"#), r#"/tmp/a\"b"#);
        assert_eq!(escape_applescript(r"C:\tmp"), r"C:\\tmp");
    }
}
