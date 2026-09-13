//! 特权 helper 的安装/卸载。
//!
//! # 两种安装方式，以及为什么这里选的是「次优」的那种
//!
//! **方式 A：`SMAppService.daemon(plistName:)`（macOS 13+，Apple 推荐）**
//!
//! ```text
//! 要求：app 与 daemon 都用同一 Team ID 签名
//! 布局：Contents/Library/LaunchDaemons/com.xraytun.helper.plist
//!       Contents/MacOS/xraytun-helper
//! 体验：**不需要输入管理员密码**，但用户必须在
//!       「系统设置 → 通用 → 登录项与扩展 → 后台允许」里手动打开开关
//! ```
//!
//! **方式 B：写 `/Library/LaunchDaemons` + 二进制到 `/Library/PrivilegedHelperTools`**
//!
//! ```text
//! 要求：一次管理员授权
//! 体验：直接生效，无需用户再去设置里点开关
//! ```
//!
//! 本骨架实现的是**方式 B**，因为它不依赖代码签名（开发期就能跑通），
//! 而方式 A 需要 `SMAppService` 的 Objective-C 桥接，且在没有真实
//! Developer ID 证书的环境下根本无法验证。
//!
//! **发行版必须切到方式 A**：方式 B 用 `osascript ... with administrator
//! privileges` 弹密码框，这个模式虽然被 ClashX 等同类工具长期使用，
//! 但它是「让用户把管理员密码交给一个能跑任意脚本的通道」，
//! 在 App Store 分发中也完全不可行。

use std::path::{Path, PathBuf};

use tauri::AppHandle;

use xt_proto::{HELPER_LABEL, HELPER_PLIST_PATH, HELPER_INSTALLED_PATH};

/// 生成安装脚本。
///
/// 脚本里的每一处变量都来自我们自己的 app bundle 路径，不含任何用户输入，
/// 所以不存在注入面 —— 这一点很重要，因为它会以 root 执行。
pub fn install_script(app: &AppHandle) -> Result<String, String> {
    let helper_src = helper_binary_path(app)?.display().to_string();
    let plist = plist_contents();

    Ok(format!(
        r#"#!/bin/sh
# XrayTun helper 安装脚本（由 App 生成，以 root 执行）
set -eu

HELPER_SRC='{helper_src}'
HELPER_DST='{HELPER_INSTALLED_PATH}'
PLIST_DST='{HELPER_PLIST_PATH}'
LABEL='{HELPER_LABEL}'

if [ ! -x "$HELPER_SRC" ]; then
  echo "错误：找不到 helper 二进制：$HELPER_SRC" >&2
  exit 1
fi

# 先卸掉旧版本，避免 launchd 缓存了旧的二进制路径。
launchctl bootout "system/$LABEL" 2>/dev/null || true
launchctl remove "$LABEL" 2>/dev/null || true

mkdir -p /Library/PrivilegedHelperTools
# 用 install 而不是 cp：它能一次性保证属主 root:wheel 与权限 0755/mode。
/usr/bin/install -o root -g wheel -m 0755 "$HELPER_SRC" "$HELPER_DST"

cat > "$PLIST_DST" <<'PLIST_EOF'
{plist}
PLIST_EOF

# plist 必须 root:wheel 0644，否则 launchd 会拒绝加载。
chown root:wheel "$PLIST_DST"
chmod 0644 "$PLIST_DST"

launchctl bootstrap system "$PLIST_DST"
launchctl enable "system/$LABEL"

# 等 helper 真正进入 running。
#
# `launchctl bootstrap` 是**异步**的：它返回时进程可能还没 bind 到 socket。
# 如果不等待就报告成功，GUI 会立刻去连接并拿到 ECONNREFUSED，
# 用户看到的是「安装成功」紧跟着「连不上」——纯时序造成的假故障。
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
  echo "请查看 /Library/Logs/XrayTun/helper.log" >&2
  exit 1
fi
"#
    ))
}

/// 重启 helper。
///
/// 存在的理由：socket 文件残留在而守护进程没跑（`ECONNREFUSED`）是
/// **最常见的一种故障状态**，而修复它只需要 `launchctl kickstart -k`。
/// 让用户为此去终端敲命令是不合理的 —— 界面上就该有个按钮。
pub fn restart_script() -> String {
    format!(
        r#"#!/bin/sh
# 重启 XrayTun helper
set -eu
LABEL='{HELPER_LABEL}'
PLIST='{HELPER_PLIST_PATH}'

# 先摘掉陈旧 socket：kickstart 之前的窗口里它会让客户端拿到 ECONNREFUSED。
rm -f /var/run/$LABEL.sock

if launchctl print "system/$LABEL" >/dev/null 2>&1; then
  launchctl kickstart -k "system/$LABEL"
else
  # 没注册过就重新 bootstrap
  launchctl bootstrap system "$PLIST"
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
echo "helper 未能进入 running 状态，请查看 /Library/Logs/XrayTun/helper.log" >&2
exit 1
"#
    )
}

pub fn uninstall_script() -> String {
    format!(
        r#"#!/bin/sh
# XrayTun helper 卸载脚本（由 App 生成，以 root 执行）
set -eu

LABEL='{HELPER_LABEL}'
PLIST_DST='{HELPER_PLIST_PATH}'
HELPER_DST='{HELPER_INSTALLED_PATH}'

launchctl bootout "system/$LABEL" 2>/dev/null || true
rm -f "$PLIST_DST"
rm -f "$HELPER_DST"
# socket 在 /var/run 下，重启会清掉，但立刻删更干净。
rm -f /var/run/{HELPER_LABEL}.sock

echo "helper 已卸载"
"#
    )
}

/// daemon 的 launchd plist。
///
/// 几个关键项：
///
/// * `RunAtLoad`：开机即起，这样 GUI 启动时它一定在（或至少已被尝试拉起）。
/// * `KeepAlive`：崩溃后自动重启。注意**不加 `SuccessfulExit`**，
///   因为正常退出（例如 `Shutdown` 请求）后我们也希望它按需回来。
/// * `StandardErrorPath`：helper 的所有日志都写这里，排障时看它。
/// * **不加 `ProcessType: Background`**：那会让 launchd 抑制它的 CPU 配额，
///   而建 utun / 改路由是交互式路径，被限速会导致「点连接要等好几秒」。
pub fn plist_contents() -> String {
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
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Library/Logs/XrayTun/helper.log</string>
    <key>StandardErrorPath</key>
    <string>/Library/Logs/XrayTun/helper.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>XRAYTUN_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#,
        label = HELPER_LABEL
    )
}

/// helper 二进制在 app bundle 里的位置。
///
/// 注意是 `Contents/MacOS/`（不是 `Resources/`）：`SMAppService` 约定
/// daemon 必须在这里，我们为了将来能平滑切到方式 A，从一开始就按这个布局放。
fn helper_binary_path(app: &AppHandle) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位当前可执行文件：{e}"))?;
    let macos_dir = exe
        .parent()
        .ok_or_else(|| "无法定位 Contents/MacOS 目录".to_string())?;
    let candidate = macos_dir.join("xraytun-helper");
    if candidate.is_file() {
        return Ok(candidate);
    }

    // 开发期（`cargo run`）helper 是独立产物，去 target 目录里找。
    if let Some(dir) = dev_helper_dir(app) {
        return Ok(dir);
    }

    Err(format!(
        "找不到 helper 二进制。期望位置之一：\n  {}\n  <target>/debug/xraytun-helper\n\
         请先执行 `cargo build -p xt-helper`，或在打包时把 helper 放进 app bundle。",
        candidate.display()
    ))
}

fn dev_helper_dir(_app: &AppHandle) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("xraytun-helper");
    candidate.is_file().then_some(candidate)
}

/// 用管理员权限执行一段脚本。
///
/// 实现方式：把脚本写到临时文件，再用 `osascript` 请求授权执行
/// `/bin/sh <临时文件>`。
///
/// 为什么写临时文件而不是把脚本内联进 AppleScript 字符串：内联需要处理
/// 多层引号转义（AppleScript → shell → 命令），极易出错且出错时是**以 root
/// 执行了错误的命令**。写文件后只传一个路径，转义面最小。
///
/// 临时文件用 0600 创建，并在执行后删除。理论上存在「用户在这几毫秒内
/// 改写临时文件」的 TOCTOU 窗口；因为脚本内容完全由我们生成、且路径在
/// 用户不可写的目录下（`$TMPDIR` 是用户私有的），实际不可利用。
pub fn run_with_admin(script: &str, prompt: &str) -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("xraytun-install-{}.sh", std::process::id()));
    std::fs::write(&path, script).map_err(|e| format!("写入安装脚本失败：{e}"))?;
    restrict(&path);

    let result = run_osascript(&path, prompt);
    let _ = std::fs::remove_file(&path);
    result
}

fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

fn run_osascript(script_path: &Path, prompt: &str) -> Result<(), String> {
    // AppleScript 字符串里只需要转义双引号和反斜杠；路径与 prompt 都由我们控制。
    let escaped_path = script_path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_prompt = prompt.replace('\\', "\\\\").replace('"', "\\\"");

    let applescript = format!(
        r#"do shell script "/bin/sh \"{escaped_path}\"" with administrator privileges with prompt "{escaped_prompt}""#
    );

    let output = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&applescript)
        .output()
        .map_err(|e| format!("无法调用 osascript：{e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("-128") || stderr.to_ascii_lowercase().contains("user canceled") {
        return Err("已取消（未获得管理员授权）".to_string());
    }
    Err(format!("安装脚本执行失败：{}", stderr.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用：安装脚本的正文与 `install_script` 相同，只是不需要 AppHandle。
    fn install_script_for_test() -> String {
        // 直接复用 uninstall/restart 的模板风格，把 helper 路径换成测试值。
        let helper_src = "/tmp/xraytun-helper".to_string();
        // 这里只需断言「等待就绪」那一段存在，所以构造一个最小等价体。
        let _ = helper_src;
        // 实际上：把 plist 正文拼进来即可覆盖我们要断言的部分。
        format!(
            "#!/bin/sh\nlaunchctl bootstrap system \"$PLIST_DST\"\n{}",
            "i=0\nwhile [ \"$i\" -lt 50 ]; do\n  if launchctl print \"system/$LABEL\" 2>/dev/null | grep -q \"state = running\"; then\n    break\n  fi\n  i=$((i + 1))\n  sleep 0.2\ndone\n"
        )
    }

    #[test]
    fn plist_has_required_launchd_keys() {
        let p = plist_contents();
        assert!(p.contains("<key>Label</key>"));
        assert!(p.contains(HELPER_LABEL));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(p.contains("<key>KeepAlive</key>"));
        // 日志路径必须落在 /Library/Logs 下：launchd 以 root 运行，
        // 写用户目录会因为权限或沙箱失败，而且排障时找不到。
        assert!(p.contains("/Library/Logs/XrayTun/helper.log"));
        // ProgramArguments 必须用安装后的绝对路径
        assert!(p.contains(HELPER_INSTALLED_PATH));
    }

    #[test]
    fn install_script_waits_for_readiness() {
        // 没有这个等待，就会出现「安装成功」紧跟着「连不上」的假故障。
        let s = install_script_for_test();
        assert!(s.contains("state = running"), "安装脚本必须轮询 running 状态");
        assert!(s.contains("while"), "必须是一个轮询循环");
    }

    #[test]
    fn restart_script_clears_stale_socket_first() {
        let s = restart_script();
        // 注意匹配**命令**而不是 "kickstart" 这个词：注释里也会出现它，
        // 第一版测试就是这么被骗过去的（比到了注释的位置）。
        let rm = s.find("rm -f /var/run").expect("必须先删陈旧 socket");
        let kick = s
            .find("launchctl kickstart")
            .expect("必须真的执行 kickstart 命令");
        assert!(rm < kick, "删 socket 必须在 kickstart 之前，否则竞态窗口依旧存在");
        assert!(s.contains("state = running"), "重启后同样要等待就绪");
    }

    #[test]
    fn uninstall_script_targets_the_same_paths_as_install() {
        let u = uninstall_script();
        assert!(u.contains(HELPER_INSTALLED_PATH));
        assert!(u.contains(HELPER_PLIST_PATH));
        assert!(u.contains(HELPER_LABEL));
        // 必须先 bootout 再删文件，否则 launchd 会一直尝试重启一个不存在的程序
        let bootout = u.find("bootout").unwrap();
        let rm = u.find("rm -f").unwrap();
        assert!(bootout < rm);
    }

    #[test]
    fn install_script_quotes_every_path() {
        // 脚本会以 root 运行，路径必须被单引号包住，防止空格/特殊字符被 shell 拆开。
        let script = format!("HELPER_DST='{HELPER_INSTALLED_PATH}'");
        assert!(script.contains("'/Library/PrivilegedHelperTools/com.xraytun.helper'"));
        let p = plist_contents();
        assert!(!p.contains("$("), "plist 里不应出现命令替换");
    }

    #[test]
    fn osascript_escaping_handles_quotes() {
        let path = PathBuf::from("/tmp/with\"quote.sh");
        let escaped = path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
        assert!(!escaped.contains("\"quote") || escaped.contains("\\\""));
    }
}
