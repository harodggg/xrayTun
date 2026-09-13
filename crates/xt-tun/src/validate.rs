//! 输入校验。
//!
//! 这一层是**特权边界的护栏**。helper 以 root 运行，任何来自 GUI 的字符串
//! 只要最终会变成 `execve` 的一个 argv 元素，就必须先过这里。
//!
//! 虽然我们用 argv 数组而不是 shell（已经杜绝了 `; rm -rf /` 这类注入），
//! 但仍然要防止：
//!
//! * 接口名里塞进 `-` 开头的选项（`--help` 之类的参数注入）；
//! * 服务名里塞进换行/管道，污染 `networksetup` 的输出解析；
//! * 路径穿越，让 helper 去操作预期之外的文件。

use crate::error::{Error, Result};

/// 接口名白名单：`en0` / `utun4` / `bridge100` 这类。
///
/// 只允许 ASCII 字母数字，长度 1..=15（`IFNAMSIZ-1`），且必须以字母开头 ——
/// 这直接排除了 `-foo` 形式的参数注入。
pub fn validate_interface_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 15 {
        return Err(Error::Invalid(format!("接口名长度非法: {name:?}")));
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphabetic() {
        return Err(Error::Invalid(format!("接口名必须以字母开头: {name:?}")));
    }
    if !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'_') {
        return Err(Error::Invalid(format!("接口名含非法字符: {name:?}")));
    }
    Ok(())
}

/// 网络服务名（`networksetup` 的 `<service>`）。
///
/// 这一步比较微妙：服务名**允许空格**（`Wi-Fi`、`USB 10/100/1000 LAN`），
/// 所以我们不能简单地只放行字母数字。策略是：禁止控制字符、换行、
/// 以及会在脚本/输出解析里有特殊含义的字符。
pub fn validate_service_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(Error::Invalid(format!("网络服务名长度非法: {name:?}")));
    }
    if name.starts_with('-') {
        return Err(Error::Invalid(format!("网络服务名不能以 '-' 开头: {name:?}")));
    }
    for ch in name.chars() {
        if ch.is_control() {
            return Err(Error::Invalid(format!("网络服务名含控制字符: {name:?}")));
        }
        if matches!(ch, '"' | '\'' | '`' | '$' | '\\' | '|' | '&' | ';' | '<' | '>' | '\n' | '\r') {
            return Err(Error::Invalid(format!("网络服务名含危险字符 {ch:?}: {name:?}")));
        }
    }
    Ok(())
}

/// 校验一个绝对路径是否位于允许的目录下。
///
/// helper 要 `execve` 数据面程序；如果不校验，GUI 就能让 root 执行任意二进制。
pub fn validate_executable_path(path: &std::path::Path, allowed_roots: &[&std::path::Path]) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::Invalid(format!("必须是绝对路径: {}", path.display())));
    }
    // 先做词法规范化，防止 `.../allowed/../../evil` 绕过前缀检查。
    let normalized = normalize_lexically(path);
    if allowed_roots.iter().any(|root| normalized.starts_with(root)) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{} 不在允许的目录内（允许: {allowed_roots:?}）",
            normalized.display()
        )))
    }
}

/// 纯词法路径规范化（不触碰文件系统，避免 TOCTOU）。
fn normalize_lexically(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 校验一段字符串只含数字、点、冒号、斜杠 —— 用于防御性地检查 CIDR/端口
/// 在拼进命令行之前的形态。
pub fn validate_cidr_text(text: &str) -> Result<()> {
    if text.is_empty() || text.len() > 64 {
        return Err(Error::Invalid(format!("CIDR 长度非法: {text:?}")));
    }
    if !text.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '/')) {
        return Err(Error::Invalid(format!("CIDR 含非法字符: {text:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rejects_flag_injection_in_interface_name() {
        assert!(validate_interface_name("-rf").is_err());
        assert!(validate_interface_name("utun4 -rf /").is_err());
        assert!(validate_interface_name("").is_err());
        assert!(validate_interface_name("averyveryverylongname").is_err());
    }

    #[test]
    fn accepts_real_interface_names() {
        for name in ["en0", "utun4", "bridge100", "lo0", "pdp_ip0"] {
            assert!(validate_interface_name(name).is_ok(), "{name} 应被接受");
        }
    }

    #[test]
    fn service_names_allow_spaces_but_not_metacharacters() {
        assert!(validate_service_name("Wi-Fi").is_ok());
        assert!(validate_service_name("USB 10/100/1000 LAN").is_ok());
        assert!(validate_service_name("Evil; rm -rf /").is_err());
        assert!(validate_service_name("Evil`id`").is_err());
        assert!(validate_service_name("-flag").is_err());
        assert!(validate_service_name("line\nbreak").is_err());
    }

    #[test]
    fn executable_path_must_be_under_allowed_root() {
        let allowed = ["/Library/PrivilegedHelperTools", "/Applications/XrayTun.app"];
        let roots: Vec<&Path> = allowed.iter().map(Path::new).collect();

        assert!(validate_executable_path(Path::new("/Library/PrivilegedHelperTools/tun2socks"), &roots).is_ok());
        assert!(validate_executable_path(Path::new("/tmp/evil"), &roots).is_err());
        // 路径穿越必须被拦住
        assert!(
            validate_executable_path(Path::new("/Library/PrivilegedHelperTools/../../tmp/evil"), &roots).is_err()
        );
        assert!(validate_executable_path(Path::new("relative/tun2socks"), &roots).is_err());
    }

    #[test]
    fn cidr_text_is_restricted() {
        assert!(validate_cidr_text("0.0.0.0/1").is_ok());
        assert!(validate_cidr_text("fe80::/10").is_ok());
        assert!(validate_cidr_text("10.0.0.0/8; rm -rf /").is_err());
    }
}
