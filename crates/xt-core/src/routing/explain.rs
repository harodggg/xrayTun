//! 「这个目的地会走哪条规则」——对**运行中配置**的真实判定。
//!
//! # 为什么以运行时配置为准
//!
//! 判定结果必须与 Xray 实际加载的东西同源，否则「界面说走代理、实际走了直连」
//! 就成了一种新的谎。所以输入是 `runtime/config.json` 里的 `routing.rules`
//! （Xray 真正读的那份），而不是应用模型再推一遍。
//!
//! # 判定语义（按 Xray 的定义）
//!
//! * 规则**自上而下**取第一条命中；
//! * 一条规则内部的多个条件字段是**合取**（AND）；
//! * 字段内多个取值是**析取**（OR）；
//! * 只比较**出现过的**字段 —— 没写 `port` 就不限制端口。
//!
//! 域名字段的取值前缀：`geosite:` `full:` `domain:` `keyword:` `regexp:` `ext:`；
//! IP 字段：`geoip:` `ext:` 以及 CIDR 字面量。`ext:` 需要额外的数据文件，
//! 当前配置未使用，这里**明确标注为无法判定**而不是当作不命中。

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::geo::{DomainKind, GeoData};

/// 一条路由规则里的条件（只保留本模块用得到的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleConds {
    #[serde(default, rename = "inboundTag")]
    pub inbound_tag: Vec<String>,
    #[serde(default)]
    pub domain: Vec<String>,
    #[serde(default)]
    pub ip: Vec<String>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
}

/// 一条路由规则。
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default, rename = "ruleTag")]
    pub tag: Option<String>,
    #[serde(rename = "outboundTag")]
    pub outbound: String,
    #[serde(flatten)]
    pub conds: RuleConds,
}

/// 查询一个目的地时提供的上下文。
#[derive(Debug, Clone, Default)]
pub struct DestQuery {
    /// 域名（已由 Xray 的 sniffing 得到的那种）。
    pub host: Option<String>,
    /// 目标 IP（域名未提供时用它）。
    pub ip: Option<IpAddr>,
    /// 目标端口。
    pub port: u16,
    /// 入站 tag（每条连接的来源入口）。
    pub inbound_tag: Option<String>,
    /// 网络类型：`tcp` 或 `udp`。
    pub network: String,
}

/// 判定结果。
#[derive(Debug, Clone, Serialize)]
pub struct RouteExplanation {
    /// 命中的规则下标（未命中为 `None`）。
    pub rule_index: Option<usize>,
    /// 命中的规则名（`ruleTag`）。
    pub rule_tag: Option<String>,
    /// 出站 tag。未命中任何规则时给兜底（Xray 会走第一条出站）。
    pub outbound: String,
    /// 命中的字段与具体依据，例如 `域名后缀命中 geosite:cn: baidu.com`。
    pub reasons: Vec<String>,
    /// 那些**无法判定**的规则（例如需要 ext: 数据文件），如实列出。
    pub undecidable: Vec<String>,
}

/// 一条规则内部某个字段是否命中。返回命中的说明。
fn domain_hits(value: &str, host: &str, geo: &GeoData, undecidable: &mut Vec<String>) -> Option<String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Some(cat) = value.strip_prefix("geosite:") {
        if geo.site_len(cat) == 0 {
            // 数据里没有这个类别：不能当作「不命中」——那会让用户以为
            // 「这条规则不管这个域名」，而事实是「我们不知道」。
            undecidable.push(format!("geosite:{cat}（数据里没有这个类别）"));
            return None;
        }
        return geo
            .site_matches(cat, &host)
            .then(|| format!("命中 geosite:{cat}（{host} 在该规则集内）"));
    }
    if let Some(rest) = value.strip_prefix("ext:") {
        let _ = rest;
        undecidable.push(format!("ext:{rest}（需要额外的规则集文件）"));
        return None;
    }
    if let Some(v) = value.strip_prefix("full:") {
        return (host == v.to_ascii_lowercase()).then(|| format!("精确匹配 full:{v}"));
    }
    if let Some(v) = value.strip_prefix("domain:") {
        return super::geo::match_entry(
            &super::geo::DomainEntry {
                kind: DomainKind::Domain,
                value: v.to_string(),
            },
            &host,
        )
        .then(|| format!("后缀匹配 domain:{v}"));
    }
    if let Some(v) = value.strip_prefix("keyword:") {
        return super::geo::match_entry(
            &super::geo::DomainEntry {
                kind: DomainKind::Keyword,
                value: v.to_string(),
            },
            &host,
        )
        .then(|| format!("包含关键字 keyword:{v}"));
    }
    if let Some(v) = value.strip_prefix("regexp:") {
        return super::geo::match_entry(
            &super::geo::DomainEntry {
                kind: DomainKind::Regex,
                value: v.to_string(),
            },
            &host,
        )
        .then(|| format!("正则命中 regexp:{v}"));
    }
    // 没有前缀时 Xray 按 `domain:` 处理
    super::geo::match_entry(
        &super::geo::DomainEntry {
            kind: DomainKind::Domain,
            value: value.to_string(),
        },
        &host,
    )
    .then(|| format!("后缀匹配 {value}"))
}

fn ip_hits(value: &str, ip: IpAddr, geo: &GeoData, undecidable: &mut Vec<String>) -> Option<String> {
    if let Some(cat) = value.strip_prefix("geoip:") {
        if geo.ip_len(cat) == 0 {
            undecidable.push(format!("geoip:{cat}（数据里没有这个类别）"));
            return None;
        }
        return geo
            .ip_matches(cat, ip)
            .then(|| format!("命中 geoip:{cat}"));
    }
    if let Some(rest) = value.strip_prefix("ext:") {
        undecidable.push(format!("ext:{rest}（需要额外的规则集文件）"));
        return None;
    }
    // CIDR 字面量
    if let Some(range) = parse_cidr_text(value) {
        return range.contains(ip).then(|| format!("命中网段 {value}"));
    }
    None
}

/// 解析 `192.168.0.0/16` 或单个 IP。
fn parse_cidr_text(s: &str) -> Option<super::geo::IpRange> {
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, p.parse::<u8>().ok()?),
        None => (s, if s.contains(':') { 128 } else { 32 }),
    };
    let addr: IpAddr = addr.parse().ok()?;
    Some(super::geo::IpRange { addr, prefix })
}

/// 端口字段：`53` / `80,443` / `1000-2000`。
fn port_hits(spec: &str, port: u16) -> bool {
    spec.split(',').any(|part| {
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                (Ok(a), Ok(b)) => port >= a && port <= b,
                _ => false,
            }
        } else {
            part.parse::<u16>().map(|p| p == port).unwrap_or(false)
        }
    })
}

/// 判定一个目的地会走哪条规则。
pub fn explain(rules: &[Rule], geo: &GeoData, q: &DestQuery) -> RouteExplanation {
    let mut undecidable = Vec::new();

    for (idx, rule) in rules.iter().enumerate() {
        let c = &rule.conds;
        let mut reasons = Vec::new();
        let mut ok = true;

        // 入站 tag：规则写了就必须匹配
        if !c.inbound_tag.is_empty() {
            let hit = q
                .inbound_tag
                .as_deref()
                .map(|t| c.inbound_tag.iter().any(|x| x == t))
                .unwrap_or(false);
            if hit {
                reasons.push(format!(
                    "入站 {} 匹配",
                    q.inbound_tag.clone().unwrap_or_default()
                ));
            } else {
                ok = false;
            }
        }

        // **缺输入的条件视为不满足** —— 这是实测出来的，不是推的。
        //
        // 用真实 Xray 对拍过（`scripts/compare-route.py`）：一条同时写了
        // `domain: [geosite:private]` 与 `ip: [geoip:private]` 的规则，
        // 当目标只有 IP（192.168.1.1）时**不命中**，会被送去兜底出站。
        // 也就是说域名条件不能因为「本次没有域名」就跳过 ——
        // 否则私网地址会被判成直连，而实际走了代理，界面就骗了人。
        if ok && !c.domain.is_empty() {
            match q.host.as_deref().and_then(|h| {
                c.domain
                    .iter()
                    .find_map(|v| domain_hits(v, h, geo, &mut undecidable))
            }) {
                Some(r) => reasons.push(r),
                None => ok = false,
            }
        }

        if ok && !c.ip.is_empty() {
            match q
                .ip
                .and_then(|ip| c.ip.iter().find_map(|v| ip_hits(v, ip, geo, &mut undecidable)))
            {
                Some(r) => reasons.push(r),
                None => ok = false,
            }
        }

        if ok {
            if let Some(spec) = &c.port {
                if port_hits(spec, q.port) {
                    reasons.push(format!("端口 {} 匹配 {spec}", q.port));
                } else {
                    ok = false;
                }
            }
        }

        if ok {
            if let Some(net) = &c.network {
                if net.split(',').any(|x| x.trim() == q.network) {
                    reasons.push(format!("网络 {} 匹配", q.network));
                } else {
                    ok = false;
                }
            }
        }

        // 一条规则必须**至少有一个**条件字段才会命中。否则「空规则」会
        // 吞掉一切 —— 而 Xray 的兜底规则正是靠显式的 network 字段表达的。
        let has_any_cond = !c.inbound_tag.is_empty()
            || !c.domain.is_empty()
            || !c.ip.is_empty()
            || c.port.is_some()
            || c.network.is_some();
        if ok && has_any_cond {
            return RouteExplanation {
                rule_index: Some(idx),
                rule_tag: rule.tag.clone(),
                outbound: rule.outbound.clone(),
                reasons,
                undecidable,
            };
        }
    }

    RouteExplanation {
        rule_index: None,
        rule_tag: None,
        // 未命中任何规则时，Xray 用**第一条出站**。
        // 界面据此显示「未命中规则 → 默认出站」，而不是编一个 tag。
        outbound: String::new(),
        reasons: vec!["未命中任何规则，将使用第一条出站（通常是节点）".into()],
        undecidable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(tag: &str, out: &str, domain: &[&str], ip: &[&str]) -> Rule {
        Rule {
            tag: Some(tag.into()),
            outbound: out.into(),
            conds: RuleConds {
                domain: domain.iter().map(|s| s.to_string()).collect(),
                ip: ip.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        }
    }

    fn geo_with_cn() -> GeoData {
        let mut g = GeoData::default();
        g.sites.insert(
            "CN".into(),
            vec![
                super::super::geo::DomainEntry {
                    kind: DomainKind::Full,
                    value: "baidu.com".into(),
                },
                super::super::geo::DomainEntry {
                    kind: DomainKind::Domain,
                    value: "qq.com".into(),
                },
            ],
        );
        g.ips.insert(
            "CN".into(),
            vec![super::super::geo::IpRange {
                addr: "223.5.5.0".parse().unwrap(),
                prefix: 24,
            }],
        );
        g
    }

    #[test]
    fn first_matching_rule_wins_in_order() {
        let geo = geo_with_cn();
        // ads 在前、cn 在后：同一条域名两边都在时，应当命中前面的 ads
        let rules = vec![
            rule("ads", "block", &["full:doubleclick.net"], &[]),
            rule("cn", "direct", &["geosite:cn"], &[]),
        ];
        let q = DestQuery {
            host: Some("doubleclick.net".into()),
            network: "tcp".into(),
            ..Default::default()
        };
        let out = explain(&rules, &geo, &q);
        assert_eq!(out.rule_tag.as_deref(), Some("ads"), "顺序敏感：先命中先返回");
    }

    #[test]
    fn geosite_full_and_suffix_both_work() {
        let geo = geo_with_cn();
        let rules = vec![rule("cn", "direct", &["geosite:cn"], &[])];

        let q = |h: &str| DestQuery {
            host: Some(h.into()),
            network: "tcp".into(),
            ..Default::default()
        };

        // Full 条目：精确命中，子域不算
        assert_eq!(explain(&rules, &geo, &q("baidu.com")).rule_tag.as_deref(), Some("cn"));
        assert_eq!(explain(&rules, &geo, &q("www.baidu.com")).rule_tag, None);
        // Domain 条目：自身与子域都命中
        assert_eq!(explain(&rules, &geo, &q("qq.com")).rule_tag.as_deref(), Some("cn"));
        assert_eq!(explain(&rules, &geo, &q("www.qq.com")).rule_tag.as_deref(), Some("cn"));
    }

    #[test]
    fn ip_rules_match_by_cidr() {
        let geo = geo_with_cn();
        let rules = vec![rule("cn-ip", "direct", &[], &["geoip:cn"])];
        let q = |ip: &str| DestQuery {
            ip: Some(ip.parse().unwrap()),
            network: "tcp".into(),
            ..Default::default()
        };
        assert_eq!(explain(&rules, &geo, &q("223.5.5.5")).rule_tag.as_deref(), Some("cn-ip"));
        assert_eq!(explain(&rules, &geo, &q("8.8.8.8")).rule_tag, None);
    }

    #[test]
    fn port_and_network_conditions_are_and_ed() {
        let geo = GeoData::default();
        let mut r = rule("dns", "dns-out", &[], &[]);
        r.conds.port = Some("53".into());
        let rules = vec![r];

        let hit = DestQuery {
            ip: Some("1.2.3.4".parse().unwrap()),
            port: 53,
            network: "udp".into(),
            ..Default::default()
        };
        assert_eq!(explain(&rules, &geo, &hit).rule_tag.as_deref(), Some("dns"));

        let miss_port = DestQuery { port: 443, ..hit.clone() };
        assert_eq!(explain(&rules, &geo, &miss_port).rule_tag, None);
    }

    /// 命中不了的数据**必须如实说不知道**，不能当作「不命中」。
    #[test]
    fn missing_category_is_reported_as_undecidable() {
        let geo = GeoData::default(); // 什么类别都没有
        let rules = vec![rule("cn", "direct", &["geosite:cn"], &[])];
        let q = DestQuery {
            host: Some("baidu.com".into()),
            network: "tcp".into(),
            ..Default::default()
        };
        let out = explain(&rules, &geo, &q);
        assert_eq!(out.rule_tag, None, "数据缺失时不应当宣称命中");
        assert!(
            out.undecidable.iter().any(|u| u.contains("geosite:cn")),
            "必须如实列出无法判定的原因，实际: {:?}",
            out.undecidable
        );
    }

    #[test]
    fn port_specs_support_lists_and_ranges() {
        assert!(port_hits("53", 53));
        assert!(!port_hits("53", 54));
        assert!(port_hits("80,443", 443));
        assert!(port_hits("1000-2000", 1500));
        assert!(!port_hits("1000-2000", 2001));
    }
}
