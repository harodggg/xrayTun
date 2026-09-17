//! 跨模块共用的小工具。
//!
//! 只放**无状态、无依赖、被多处复用**的东西。判断标准很简单：
//! 当一个函数在 crate 内出现第三次时，它就该在这里，而不是再抄一遍。
//!
//! 刻意不放的东西：
//! * 与特权/平台耦合的辅助（如绑网卡）留在 `net`，那里有它们的完整语境；
//! * `xt-helper` / `xt-tun` 各自的副本**不合并到这里** —— 那两个 crate
//!   只依赖轻量的 `xt-proto`，为了一个时间戳函数把它们拖进整个 `xt-core`
//!   是拿架构换几行代码。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 Unix 时间戳（秒）。
///
/// 系统时钟早于 1970 时返回 0 而不是 panic：这条路径出现在日志、更新检查、
/// 探测结果里，任何一个都不值得为「时钟不对」把程序炸掉。
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 取中位数（会就地对切片排序）。空切片返回 `None`。
///
/// 用中位数而不是平均值，是因为延迟样本里的单点异常必须被压掉：
/// 首次冷 DNS 缓存那种 850ms 会把平均值整个带偏，而中位数不受影响。
pub fn median(values: &mut [u32]) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_ignores_a_single_outlier() {
        assert_eq!(median(&mut [190]), Some(190));
        assert_eq!(median(&mut [210, 190, 191]), Some(191));
        // 单点异常被压掉：192 vs 850（首次冷 DNS 缓存那种）
        assert_eq!(median(&mut [192, 850, 191]), Some(192));
        assert_eq!(median(&mut []), None);
    }

    #[test]
    fn now_unix_is_plausible() {
        // 2020-01-01 之后、且不是 0（0 表示时钟异常，不该在正常环境出现）
        let t = now_unix();
        assert!(t > 1_577_836_800, "时间戳看起来不对: {t}");
    }
}
