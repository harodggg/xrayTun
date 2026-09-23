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

/// 真正执行外部命令（**生产路径就是这一条**）。
fn real_run(program: &str, args: &[String]) -> Result<String> {
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

/// 统一的外部命令调用入口。
///
/// **只用绝对路径 + argv 数组**，永不经过 shell。这一条是 helper 安全性的基石：
/// 任何来自 GUI 的字符串都不可能被解释成 shell 语法。
///
/// # 接缝（task-134）
///
/// **测试构建**下可以先注入一个替身执行器（[`with_executor`]），用来在
/// **完全不碰真实路由/DNS** 的前提下验证「某一步失败时上层是否如实上报」。
/// 注入钩子整个包在 `#[cfg(test)]` 里 ⇒ 生产构建编译出来的就是
/// 对 [`real_run`] 的直接调用，**行为与以前逐字一致、零额外开销**。
pub(crate) fn run(program: &str, args: &[String]) -> Result<String> {
    #[cfg(test)]
    {
        if let Some(exec) = current_test_executor() {
            return exec(program, args);
        }
    }
    real_run(program, args)
}

/// 可注入的执行器类型（**只在测试构建里存在**）。
///
/// 单独起个别名有两个理由：① 让接缝的「形状」可读；② 不触发 `clippy::type_complexity`
/// （本仓 `-D warnings`，复杂类型直接红）。
#[cfg(test)]
pub(crate) type TestExecutor = std::rc::Rc<dyn Fn(&str, &[String]) -> Result<String>>;

#[cfg(test)]
thread_local! {
    /// 测试注入的替身执行器（**只在测试构建里存在**）。
    static TEST_EXECUTOR: std::cell::RefCell<Option<TestExecutor>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn current_test_executor() -> Option<TestExecutor> {
    TEST_EXECUTOR.with(|c| c.borrow().clone())
}

/// 在 `f()` 期间把外部命令执行换成 `exec`；**退出（含 panic）时自动还原**。
///
/// `#[cfg(test)]` —— 生产二进制里没有这个入口，想注入也没有路径。
#[cfg(test)]
pub(crate) fn with_executor<R>(
    exec: TestExecutor,
    f: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<TestExecutor>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_EXECUTOR.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = TEST_EXECUTOR.with(|c| c.borrow_mut().replace(exec));
    let _guard = Restore(prev);
    f()
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
