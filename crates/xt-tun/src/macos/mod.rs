//! macOS 平台实现。

pub mod controller;
pub mod dns;
pub mod fdpass;
pub mod netif;
pub mod route;
pub mod snapshot;
pub mod utun;

use std::process::Command;

use crate::error::{Error, Result};

/// 统一的外部命令调用入口。
///
/// **只用绝对路径 + argv 数组**，永不经过 shell。这一条是 helper 安全性的基石：
/// 任何来自 GUI 的字符串都不可能被解释成 shell 语法。
pub(crate) fn run(program: &str, args: &[String]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::Command {
            program: program.to_string(),
            args: args.to_vec(),
            code: None,
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(Error::Command {
            program: program.to_string(),
            args: args.to_vec(),
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 与 [`run`] 相同，但丢弃 stdout。
pub(crate) fn run_ok(program: &str, args: &[String]) -> Result<()> {
    run(program, args).map(|_| ())
}

/// 构造 `Vec<String>` 参数列表的小工具，省掉满屏的 `.to_string()`。
pub(crate) fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_stdout() {
        let out = run("/bin/echo", &args(&["hello"])).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn run_reports_failure_with_stderr() {
        // /bin/ls 读一个不存在的路径会返回非 0 并往 stderr 写。
        let err = run("/bin/ls", &args(&["/definitely/not/here"])).unwrap_err();
        match err {
            Error::Command { code, stderr, .. } => {
                assert_ne!(code, Some(0));
                assert!(!stderr.is_empty());
            }
            other => panic!("期望 Command 错误，得到 {other:?}"),
        }
    }

    #[test]
    fn missing_program_is_reported() {
        assert!(run("/nonexistent/tool", &[]).is_err());
    }

    #[test]
    fn no_shell_interpretation_happens() {
        // 关键安全断言：分号不会被当成命令分隔符。
        let out = run("/bin/echo", &args(&["a; rm -rf /"])).unwrap();
        assert_eq!(out.trim(), "a; rm -rf /");
    }
}
