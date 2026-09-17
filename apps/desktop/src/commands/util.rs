//! 本模块由原 `commands.rs` 按职责拆分而来，逻辑与文案未改。
//!
//! `use super::*;` 引入的是 `commands/mod.rs` 里的公共导入，以及各兄弟模块的
//! 公开条目（`mod.rs` 里逐个 `pub use`）—— 拆分前它们都在同一个文件里。

#![allow(unused_imports)] // 通配导入：各模块用到的子集不同，无需逐个精确列举

use super::*;

/// 状态锁不可用时的统一提示。
///
/// 抽成常量而不是散落的字面量：这句话就是用户看到的全部信息，
/// 改一次要能全局生效，而不是漏掉某几处导致同一个故障有两种说法。
pub(crate) const STATE_UNAVAILABLE: &str = "应用状态不可用";

/// 把领域错误翻成给用户看的一句话。
///
/// `xt_core::Error` 的 `Display` 本来就是中文人话（见 `crates/xt-core/src/error.rs`），
/// 命令层要做的只是脱掉错误类型、留下那句话。这里统一收口，
/// 避免 16 处各写一遍 `map_err(user_msg)`。
pub(crate) fn user_msg<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// 36 个字符、恰好 4 个连字符 —— UUID 的形状。
pub(crate) fn is_uuid_like(token: &str) -> bool {
    token.len() == 36 && token.matches('-').count() == 4
}

/// 让 `PathBuf` 在诊断输出里可读。
///
/// **目前只有测试在用它。** 它从初始提交起就没有生产调用者 —— 早先它在一个
/// 大单文件里，`pub` 让它逃过了死代码检查；拆成模块私有后才暴露出来。
/// 这里标成 `cfg(test)` 而不是直接删掉：测试钉住的是「`None` 该显示成什么」
/// 这个约定，将来诊断输出要用时这里是现成的实现。
#[cfg(test)]
fn display_path(p: &Option<PathBuf>) -> String {
    p.as_ref().map(|x| x.display().to_string()).unwrap_or_else(|| "<未找到>".to_string())
}

pub(crate) fn macos_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

        #[test]
        fn display_path_handles_none() {
            assert_eq!(display_path(&None), "<未找到>");
        }
}
