//! 本地预算：滑动窗口限速 + 每日上限。
//!
//! # 两条纪律
//!
//! 1. **超额 = 放行**，不是拦截。预算的作用是"少花点钱"，不是"断了用户的网"。
//!    调用方拿到 [`BudgetExhausted`] 之后的动作必须是 `Verdict::Deferred`，
//!    并写一条 `budget_exhausted` 审计。
//! 2. **超限的那次调用不占额度**（先判后计）。`jev-x-filter` 是"先取快照再调用"
//!    （`pipeline.js:253,267`），我们在这里把它做成类型上的保证：
//!    [`Budget::try_consume`] 只在**成功**时记账，所以"被拒"不会挤掉后面真正该花的那次。
//!
//! # `0` 的含义
//!
//! `per_minute = 0` 或 `per_day = 0` 表示**不允许任何调用**（功能等同于关闭分类），
//! **不是**"无限制"。把 0 解释成无限制是这类代码里最常见的一个静默错误 ——
//! 界面输入框清空后被解析成 0，于是"我明明设了上限"变成了"无限调用"。
//! 要无限制就用 [`Budget::unlimited`]。

use std::collections::VecDeque;

/// 额度用尽（哪个窗口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetExhausted {
    PerMinute,
    PerDay,
}

impl BudgetExhausted {
    pub fn scope(&self) -> &'static str {
        match self {
            Self::PerMinute => "minute",
            Self::PerDay => "day",
        }
    }
}

const MINUTE: u64 = 60;
const DAY: u64 = 24 * 60 * 60;

/// 判定调用的额度。窗口是**滑动**的（记录每次调用的时间戳），不是固定桶 ——
/// 固定桶会在窗口边界上放过两倍的量。
#[derive(Debug, Clone)]
pub struct Budget {
    per_minute: u32,
    per_day: u32,
    minute: VecDeque<u64>,
    day: VecDeque<u64>,
    /// 累计成功的调用数（跨窗口，只增）。
    pub total_calls: u64,
    /// 累计被拒的次数。
    pub total_denied: u64,
}

impl Budget {
    pub fn new(per_minute: u32, per_day: u32) -> Self {
        Self {
            per_minute,
            per_day,
            minute: VecDeque::new(),
            day: VecDeque::new(),
            total_calls: 0,
            total_denied: 0,
        }
    }

    /// 事实上无限制（测试与"我自己付钱，别管"的场景）。
    pub fn unlimited() -> Self {
        Self::new(u32::MAX, u32::MAX)
    }

    pub fn per_minute(&self) -> u32 {
        self.per_minute
    }

    pub fn per_day(&self) -> u32 {
        self.per_day
    }

    /// 申请一次调用额度。成功即**已记账**。
    pub fn try_consume(&mut self, now: u64) -> Result<(), BudgetExhausted> {
        self.prune(now);
        if self.per_minute == 0 || self.minute.len() as u32 >= self.per_minute {
            self.total_denied = self.total_denied.saturating_add(1);
            return Err(BudgetExhausted::PerMinute);
        }
        if self.per_day == 0 || self.day.len() as u32 >= self.per_day {
            self.total_denied = self.total_denied.saturating_add(1);
            return Err(BudgetExhausted::PerDay);
        }
        self.minute.push_back(now);
        self.day.push_back(now);
        self.total_calls = self.total_calls.saturating_add(1);
        Ok(())
    }

    /// 当前窗口已用（会先修剪）。
    pub fn snapshot(&mut self, now: u64) -> BudgetSnapshot {
        self.prune(now);
        BudgetSnapshot {
            used_last_minute: self.minute.len() as u32,
            used_today: self.day.len() as u32,
            per_minute: self.per_minute,
            per_day: self.per_day,
            total_calls: self.total_calls,
            total_denied: self.total_denied,
        }
    }

    fn prune(&mut self, now: u64) {
        while let Some(&t) = self.minute.front() {
            if now.saturating_sub(t) >= MINUTE {
                self.minute.pop_front();
            } else {
                break;
            }
        }
        while let Some(&t) = self.day.front() {
            if now.saturating_sub(t) >= DAY {
                self.day.pop_front();
            } else {
                break;
            }
        }
    }
}

/// 界面展示用的快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub used_last_minute: u32,
    pub used_today: u32,
    pub per_minute: u32,
    pub per_day: u32,
    pub total_calls: u64,
    pub total_denied: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_minute_window_slides() {
        let mut b = Budget::new(2, 100);
        assert!(b.try_consume(1000).is_ok());
        assert!(b.try_consume(1010).is_ok());
        assert_eq!(b.try_consume(1020), Err(BudgetExhausted::PerMinute));
        // 第一次调用已经滑出窗口（60s 后）。
        assert!(b.try_consume(1060).is_ok());
    }

    #[test]
    fn per_day_caps_even_when_the_minute_window_is_free() {
        let mut b = Budget::new(100, 3);
        for i in 0..3 {
            assert!(b.try_consume(1_000 + i * 120).is_ok(), "第 {i} 次应通过");
        }
        assert_eq!(b.try_consume(1_400), Err(BudgetExhausted::PerDay));
        // 一天之后重新可用。
        assert!(b.try_consume(1_000 + DAY).is_ok());
    }

    #[test]
    fn a_denied_call_does_not_consume_quota() {
        // 天上限先给足，这样这条测试只考察"分钟窗口"。
        let mut b = Budget::new(1, 10);
        assert!(b.try_consume(0).is_ok());
        assert_eq!(b.try_consume(1), Err(BudgetExhausted::PerMinute));
        assert_eq!(b.try_consume(2), Err(BudgetExhausted::PerMinute));
        assert_eq!(b.total_calls, 1, "被拒的调用不许记账");
        assert_eq!(b.total_denied, 2);
        // 窗口滑空后额度恢复（1/分钟）。
        assert!(b.try_consume(61).is_ok());

        // 两条上限都收紧时，"天"先成为硬约束：分钟窗口滑空也救不回来。
        let mut tight = Budget::new(1, 1);
        assert!(tight.try_consume(0).is_ok());
        assert_eq!(tight.try_consume(61), Err(BudgetExhausted::PerDay));
        assert_eq!(tight.try_consume(DAY + 1), Ok(()));
    }

    #[test]
    fn zero_means_no_calls_not_unlimited() {
        let mut b = Budget::new(0, 0);
        assert_eq!(b.try_consume(0), Err(BudgetExhausted::PerMinute));
        assert_eq!(b.total_calls, 0);

        // 分钟不限、天为 0 → 仍然拒绝（天窗口是硬约束）。
        let mut b = Budget::new(u32::MAX, 0);
        assert_eq!(b.try_consume(0), Err(BudgetExhausted::PerDay));

        // 真正的无限制要显式要。
        assert!(Budget::unlimited().try_consume(0).is_ok());
    }

    #[test]
    fn snapshot_reports_both_windows() {
        let mut b = Budget::new(10, 100);
        b.try_consume(100).unwrap();
        b.try_consume(110).unwrap();
        let s = b.snapshot(120);
        assert_eq!(s.used_last_minute, 2);
        assert_eq!(s.used_today, 2);
        assert_eq!(s.per_minute, 10);
        assert_eq!(s.total_calls, 2);
    }

    #[test]
    fn clock_going_backwards_does_not_panic_or_grant_quota() {
        let mut b = Budget::new(1, 10);
        b.try_consume(10_000).unwrap();
        // 时间倒退：`saturating_sub` 保证不 panic，且额度不会被"提前释放"。
        assert_eq!(b.try_consume(5_000), Err(BudgetExhausted::PerMinute));
    }
}
