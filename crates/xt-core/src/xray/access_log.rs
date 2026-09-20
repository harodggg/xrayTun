//! 解析 Xray 访问日志里的**连接行**，按出口 tag 统计连接数。
//!
//! # 为什么需要它
//!
//! 核心的 `StatsService` 只提供**字节**计数器，没有连接数计数器。而有两类出口
//! 的字节数**永远读不到**，界面上就会显示成 `0 B` —— 看起来像「这个出口没用」，
//! 实际它在被大量使用：
//!
//! | 出口 | 实测连接数 | `StatsService` 字节 | 为什么字节恒为 0 |
//! |---|---|---|---|
//! | `dns-out`（协议 `dns`） | 4769 条 UDP | **0** | UDP 出站流量不计入统计 |
//! | `api`（本机回环） | 5374 条 TCP | **0** | 回环流量不计入统计 |
//! | `block`（`blackhole`） | 201 条 | 0 | 真的 0 —— 连接被拒，本就没有字节 |
//!
//! 所以「0 字节」对这三类出口是**测量盲区**，不是事实。连接数是那可得的指标，
//! 日志里每建立一条连接就有一行：
//!
//! ```text
//! 2026/09/20 11:15:13.475426 from tcp:198.18.0.1:58137 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]
//! ```
//!
//! 我们在这里把 `[入站 -> 出站]` 里的**出站**取出来计数。
//!
//! # 为什么不用另起一个日志 tail
//!
//! 核心的 stdout 已经有一个转发任务在逐行读取（`commands/core.rs`），
//! 那里是天然的单点。再开一个 tail 会重复读取、还要处理轮转与偏移，
//! 而且两份读取的时间戳会对不齐。
//!
//! # 已知边界（如实说明）
//!
//! * **累计值**，从核心启动开始算；核心重启后归零（与字节计数器同源，界面
//!   对两者的处理应当一致）。
//! * 只在 `loglevel` 足够低、且核心真的写出这条访问行时才有数据。核心退出后
//!   不再增长 —— 界面不应把它当成「当前并发数」。
//! * 解析失败的行**静默跳过**：日志格式由上游决定，我们不该因为一行看不懂就
//!   污染统计；但也绝不臆造数字。

use std::collections::HashMap;

/// 从一行核心日志里取出「出站 tag」。
///
/// 识别依据是 Xray 访问日志的固定形态：`accepted <网络>:<目标> [<入站> -> <出站>]`。
/// 目标里可能出现方括号（IPv6 字面量），所以从**行尾**往前找最后一对 `[` `]`，
/// 而不是从开头找第一个。
///
/// 返回 `None` 表示这不是一行可识别的连接行。
pub fn parse_outbound_tag(line: &str) -> Option<&str> {
    // 只认访问行：必须含 ` accepted `，避免把别处的 `[a -> b]` 也算进来。
    // `accepted ` 后面紧跟网络类型，中间不会有别的空白。
    let accepted_at = line.find(" accepted ")?;
    let rest = &line[accepted_at + " accepted ".len()..];
    // 目标在 `accepted` 之后、`[` 之前；这里只用来确认形态，不解析内容。
    if !rest.starts_with("tcp:") && !rest.starts_with("udp:") {
        return None;
    }

    // 从行尾往回找最后一对括号。
    let close = line.rfind(']')?;
    let open = line[..close].rfind('[')?;
    let inner = &line[open + 1..close];

    // 形如 `<入站> -> <出站>`
    let (_, outbound) = inner.split_once(" -> ")?;
    let outbound = outbound.trim();
    if outbound.is_empty() {
        return None;
    }
    Some(outbound)
}

/// 累计各出口的连接数。
///
/// 只在收到新日志行时调用一次，增量更新，不重扫历史。
#[derive(Debug, Default, Clone)]
pub struct ConnectionCounters {
    per_outbound: HashMap<String, u64>,
}

impl ConnectionCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// 观察一行日志。返回被计数到的出口 tag（没有则 `None`）。
    ///
    /// 返回 `String` 而不是 `&str`：解析结果借用的是**入参** `line`，
    /// 而方法同时可变借用 `self`，两者生命周期无法统一。
    pub fn observe(&mut self, line: &str) -> Option<String> {
        let tag = parse_outbound_tag(line)?.to_string();
        *self.per_outbound.entry(tag.clone()).or_insert(0) += 1;
        Some(tag)
    }

    /// 某个出口的连接数；没记录过就是 `None`（**不是 0**）。
    ///
    /// 区分「没观察到」与「观察到 0 次」很重要：核心没在跑、或日志级别不够时
    /// 是前者，界面该显示「—」而不是 `0`。
    pub fn get(&self, tag: &str) -> Option<u64> {
        self.per_outbound.get(tag).copied()
    }

    /// 是否至少观察到过一条连接行。用来判断这个数据源是否可用。
    pub fn observed_anything(&self) -> bool {
        !self.per_outbound.is_empty()
    }

    /// 全部计数（快照）。
    pub fn snapshot(&self) -> HashMap<String, u64> {
        self.per_outbound.clone()
    }

    /// 清空（核心重启时调用 —— 计数器从零开始）。
    pub fn reset(&mut self) {
        self.per_outbound.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实日志行（取自本机 `logs/app.jsonl`）。这是最常见的一类。
    const REAL_TUN_TO_NODE: &str = "2026/09/20 11:15:13.475426 from tcp:198.18.0.1:58137 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]";

    #[test]
    fn parses_real_access_line() {
        assert_eq!(parse_outbound_tag(REAL_TUN_TO_NODE), Some("node-n1d232c6b8c7a5004"));
    }

    /// `dns-out` 与 `api` 是本次要解决的两个出口 —— 它们是 UDP / 回环，
    /// 字节计数器恒为 0，只能靠连接数体现活跃度。
    #[test]
    fn parses_the_two_outbounds_whose_byte_counter_is_always_zero() {
        let dns = "2026/09/19 20:09:14.485964 from udp:198.18.0.1:30369 accepted udp:198.18.0.2:53 [tun -> dns-out]";
        assert_eq!(parse_outbound_tag(dns), Some("dns-out"));

        let api = "2026/09/20 11:15:12.569534 from 127.0.0.1:58135 accepted tcp:127.0.0.1:10085 [api -> api]";
        assert_eq!(parse_outbound_tag(api), Some("api"));
    }

    /// IPv6 目标里带方括号，所以必须从行尾往前找最后一对括号。
    /// 若从开头找第一个 `[`，取到的是地址里的那个，出站 tag 会解析错。
    #[test]
    fn ipv6_brackets_in_target_do_not_confuse_the_parser() {
        let line = "2026/09/20 11:15:13.1 from tcp:198.18.0.1:5 accepted tcp:[2606:4700:4700::1111]:443 [tun -> node-x]";
        assert_eq!(parse_outbound_tag(line), Some("node-x"));
    }

    /// 不是连接行的日志（启动信息、警告、路由命中）不应被计数。
    #[test]
    fn ignores_lines_that_are_not_connections() {
        for line in [
            "2026/09/19 20:09:14.485942 [Info] [1963124866] app/dispatcher: Hit route rule: [internal-dns-hijack] so taking detour [dns-out] for [udp:198.18.0.2:53]",
            "Xray 26.9.9 (Xray, Penetrates Everything.) Custom (go1.25.0 darwin/arm64)",
            "2026/09/19 20:09:14.1 [Warning] something happened [a -> b]",
            "",
        ] {
            assert_eq!(parse_outbound_tag(line), None, "不该识别：{line}");
        }
    }

    /// **路由命中行里也有 `[xxx -> yyy]` 形态**，它必须不被计数。
    ///
    /// 这是最容易写错的一处：上述 dispatcher 行含 `[internal-dns-hijack]`
    /// 与 `[dns-out]`，但没有 ` accepted `，所以要求 ` accepted ` 是必要的。
    #[test]
    fn route_hit_lines_are_not_counted_as_connections() {
        let hit = "[Info] app/dispatcher: Hit route rule: [internal-dns-hijack] so taking detour [dns-out]";
        assert_eq!(parse_outbound_tag(hit), None);
    }

    /// 畸形行不得产生假 tag，也不得 panic。
    #[test]
    fn malformed_lines_produce_no_phantom_tags() {
        for line in [
            "accepted tcp:1.2.3.4:443 [tun -> ]",
            "accepted tcp:1.2.3.4:443 [tun]",
            "accepted tcp:1.2.3.4:443",
            " accepted tcp:1.2.3.4:443 [tun -> x]",
            "accepted tcp:1.2.3.4:443 [tun -> x",
        ] {
            let got = parse_outbound_tag(line);
            assert!(
                got.map(|t| !t.is_empty()).unwrap_or(true),
                "不得产出空 tag：{line} → {got:?}"
            );
        }
    }

    #[test]
    fn counters_accumulate_per_outbound() {
        let mut c = ConnectionCounters::new();
        c.observe(REAL_TUN_TO_NODE);
        c.observe(REAL_TUN_TO_NODE);
        c.observe("... accepted udp:198.18.0.2:53 [tun -> dns-out]");
        c.observe("... accepted tcp:127.0.0.1:10085 [api -> api]");

        assert_eq!(c.get("node-n1d232c6b8c7a5004"), Some(2));
        assert_eq!(c.get("dns-out"), Some(1));
        assert_eq!(c.get("api"), Some(1));
        // 没观察过的出口是 `None`，**不是 0** —— 界面据此显示「—」而不是 `0`。
        assert_eq!(c.get("never-seen"), None);
    }

    #[test]
    fn reset_clears_everything_because_the_core_restarted() {
        let mut c = ConnectionCounters::new();
        c.observe(REAL_TUN_TO_NODE);
        assert!(c.observed_anything());
        c.reset();
        assert!(!c.observed_anything());
        assert_eq!(c.get("node-n1d232c6b8c7a5004"), None);
    }

    /// 未识别行不计数，但也不能把已统计的数字弄丢。
    #[test]
    fn unrecognized_lines_leave_existing_counts_intact() {
        let mut c = ConnectionCounters::new();
        c.observe(REAL_TUN_TO_NODE);
        assert_eq!(c.observe("just a regular log line"), None);
        assert_eq!(c.get("node-n1d232c6b8c7a5004"), Some(1));
    }
}
