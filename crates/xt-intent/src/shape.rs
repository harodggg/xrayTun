//! L2：流量形状。**它只调阈值、只排优先级，永不定罪。**
//!
//! # 先说清楚一件事：这一层能拿到什么
//!
//! Xray 的访问日志只给「连接建立了」这一件事（`accepted` 行），
//! 没有响应码、没有 referer、没有 URL、没有"谁带出来的"。所以能够**诚实**计算的形状
//! 特征只有时间与端口维度的这几条：
//!
//! | 特征 | 怎么来 | 强不强 |
//! |---|---|---|
//! | 连接次数 / 时间跨度 | 同一主机名的多次 `accepted` | 中 |
//! | 间隔规律（心跳） | 相邻两次间隔的均值/标准差 | 中 |
//! | 非标准端口 | `target_port` 不在 {80,443} | 弱 |
//! | 只有 UDP | 同一主机名只出现过 `udp` | 弱 |
//!
//! 拿不到的（**不许假装有**）："只被某一个主域引用"、"从未作为主文档出现"、
//! "无用户交互"。这些需要 referer 或页面上下文，L4 看不到
//! （设计文档 §2 的 L2 行据此修正过）。
//!
//! # 为什么它不许单独定罪
//!
//! 这些特征非常便宜（零请求），但也**非常容易过拟合**：一次系统更新、
//! 一个天气小部件、一个 IM 心跳，都长成"周期性、非标准端口、只有 UDP"的样子。
//!
//! 所以产出是一个**有上限的加分**（默认最多 0.10，见
//! [`crate::verdict::Thresholds::shape_bonus_max`]）：它能帮模型把 0.80 的边缘判决
//! 推过 0.75 的闸门，但**永远不可能**把 0.0 推过任何阈值 ——
//! 有测试专门钉住这一点。

/// 一条候选在观察窗口里累积出来的形状。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowShape {
    /// 被观察到的连接数。
    pub connections: u64,
    /// 从第一次到最近一次跨越的秒数。
    pub spans_secs: u64,
    /// 连接间隔是否有规律（心跳形态）。
    pub regular_interval: bool,
    /// 只出现在非标准端口上（既不是 80 也不是 443）。
    pub nonstandard_port: bool,
    /// 只出现过 UDP。
    pub udp_only: bool,
}

impl FlowShape {
    /// 形状加分。上限由调用方给定（来自阈值配置），这里**只负责算**。
    pub fn bonus(&self, max: f32) -> f32 {
        if !max.is_finite() || max <= 0.0 {
            return 0.0;
        }
        let mut b = 0.0f32;
        // 规律心跳 —— 追踪/遥测的典型形态，但心跳类业务也有。
        if self.regular_interval {
            b += 0.03;
        }
        // 反复出现且跨越了相当一段时间 —— 排除了"页面加载时的顺手一次"。
        if self.connections >= 3 && self.spans_secs >= 60 {
            b += 0.03;
        }
        if self.nonstandard_port {
            b += 0.02;
        }
        if self.udp_only {
            b += 0.02;
        }
        b.min(max).max(0.0)
    }

    /// 分类优先级（越大越先花钱）。**不是判决**，只影响顺序。
    pub fn priority(&self) -> u32 {
        let mut p = 0u32;
        if self.regular_interval {
            p += 2;
        }
        if self.connections >= 3 {
            p += 1;
        }
        if self.nonstandard_port {
            p += 1;
        }
        if self.udp_only {
            p += 1;
        }
        p
    }

    /// 从一串连接时刻推出"间隔是否有规律"。
    ///
    /// 判据：至少 4 次连接、均值间隔 ≥ 5s、且**标准差 / 均值 ≤ 0.25**。
    /// 下限定在 5s 是为了避开"页面加载时几十个请求挤在一起"这种伪规律。
    pub fn intervals_are_regular(times: &[u64]) -> bool {
        if times.len() < 4 {
            return false;
        }
        let mut gaps: Vec<f64> = Vec::with_capacity(times.len() - 1);
        for w in times.windows(2) {
            gaps.push(w[1].saturating_sub(w[0]) as f64);
        }
        let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
        if mean < 5.0 {
            return false;
        }
        let var = gaps.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / gaps.len() as f64;
        (var.sqrt() / mean) <= 0.25
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_signal_means_no_bonus() {
        let s = FlowShape::default();
        assert_eq!(s.bonus(0.10), 0.0);
        assert_eq!(s.priority(), 0);
        assert!(!FlowShape::intervals_are_regular(&[]));
        assert!(!FlowShape::intervals_are_regular(&[0, 60, 120]));
    }

    #[test]
    fn bonus_is_capped_by_the_caller_supplied_max() {
        let s = FlowShape {
            connections: 100,
            spans_secs: 100_000,
            regular_interval: true,
            nonstandard_port: true,
            udp_only: true,
        };
        // 原始和是 0.10，上限 0.02 时必须被夹住。
        assert_eq!(s.bonus(0.02), 0.02);
        assert!((s.bonus(0.10) - 0.10).abs() < 1e-6, "{}", s.bonus(0.10));
        // 上限非法时退回 0，绝不返回 NaN。
        assert_eq!(s.bonus(0.0), 0.0);
        assert_eq!(s.bonus(-1.0), 0.0);
        assert_eq!(s.bonus(f32::NAN), 0.0);
    }

    #[test]
    fn shape_is_monotone_in_its_inputs() {
        let base = FlowShape { regular_interval: true, ..Default::default() };
        let more = FlowShape { regular_interval: true, nonstandard_port: true, ..Default::default() };
        assert!(more.bonus(0.10) >= base.bonus(0.10));
        assert!(more.priority() >= base.priority());
    }

    #[test]
    fn a_page_load_burst_is_not_a_heartbeat() {
        // 同一次页面加载：5 个请求挤在同一秒。均值间隔 < 5s ⇒ 不算心跳。
        assert!(!FlowShape::intervals_are_regular(&[100, 100, 100, 101, 101]));
    }

    #[test]
    fn a_real_heartbeat_is_recognized() {
        // 每 30s 一次，轻微抖动。
        assert!(FlowShape::intervals_are_regular(&[0, 30, 61, 90, 121]));
        // 抖动太大（30s / 90s 交替）⇒ 不算。
        assert!(!FlowShape::intervals_are_regular(&[0, 30, 120, 150, 240]));
    }
}
