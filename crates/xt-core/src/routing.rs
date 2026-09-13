//! 路由规则：应用层的中间表示 + 到 Xray `routing.rules` 的编译。
//!
//! 为什么要有中间表示？因为用户的心智模型是「什么流量 → 怎么走」，
//! 而 Xray 的规则是「一组条件字段的 AND 匹配 + 一个 outboundTag」。两者形状不同：
//!
//! * Xray 规则的条件是**合取**（AND），要做 OR 必须拆成多条规则 —— 由 [`compile`] 负责展开。
//! * Xray 内建 `geosite:` / `geoip:` / `ext:` 规则集，不用自己维护大陆域名表。
//! * 规则顺序敏感：Xray 自上而下取第一条命中，所以「阻断广告」要排在「大陆直连」之前。

use serde::{Deserialize, Serialize};
use xt_proto::Cidr;

use crate::model::RoutingPreset;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Both,
    Tcp,
    Udp,
}

impl Network {
    /// Xray 的 `network` 字段；`Both` 表示不写该字段（即不限）。
    pub fn as_xray(self) -> Option<&'static str> {
        match self {
            Self::Both => None,
            Self::Tcp => Some("tcp"),
            Self::Udp => Some("udp"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortMatcher {
    /// 形如 `"443"`、`"1000-2000"`、`"80,443"`（Xray 原生就吃这种字符串）。
    Raw(String),
    Single(u16),
    Range { from: u16, to: u16 },
    List(Vec<u16>),
}

impl PortMatcher {
    pub fn as_xray(&self) -> String {
        match self {
            Self::Raw(s) => s.clone(),
            Self::Single(p) => p.to_string(),
            Self::Range { from, to } => format!("{from}-{to}"),
            Self::List(v) => v.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(","),
        }
    }

    /// 判断某个端口是否命中（用于 UI 侧预览，不参与核心运行）。
    pub fn matches(&self, port: u16) -> bool {
        match self {
            Self::Raw(s) => s.split(',').any(|part| match part.split_once('-') {
                Some((a, b)) => match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                    (Ok(a), Ok(b)) => (a..=b).contains(&port),
                    _ => false,
                },
                None => part.trim().parse::<u16>() == Ok(port),
            }),
            Self::Single(p) => *p == port,
            Self::Range { from, to } => (*from..=*to).contains(&port),
            Self::List(v) => v.contains(&port),
        }
    }
}

/// 一条规则的全部匹配条件。字段之间是 **AND**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MatchCondition {
    /// 域名匹配，支持 Xray 语法：`example.com`（子域+自身）、`domain:example.com`、
    /// `full:example.com`、`regexp:^a.*`、`geosite:cn`、`ext:custom.srs`。
    #[serde(default)]
    pub domains: Vec<String>,
    /// IP/CIDR 匹配，支持 `geoip:cn`、`ext:cn.srs`、`10.0.0.0/8`。
    #[serde(default)]
    pub ip: Vec<String>,
    #[serde(default)]
    pub ports: Vec<PortMatcher>,
    #[serde(default)]
    pub source_ip: Vec<Cidr>,
    /// 命中哪些 inbound（`socks` / `http` / `tun` / `dns`）。空表示不限。
    #[serde(default)]
    pub inbound_tags: Vec<String>,
    #[serde(default)]
    pub network: Network,
    /// 按**进程名**分流（仅 macOS/Windows 支持；Xray 需要 `sniffing` 之外的
    /// `processName` 字段，取值是进程可执行名）。
    #[serde(default)]
    pub process_names: Vec<String>,
    /// 嗅探出的应用层协议：`http` / `tls` / `quic` / `bittorrent`。
    #[serde(default)]
    pub protocols: Vec<String>,
}

impl MatchCondition {
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
            && self.ip.is_empty()
            && self.ports.is_empty()
            && self.source_ip.is_empty()
            && self.inbound_tags.is_empty()
            && self.network == Network::Both
            && self.process_names.is_empty()
            && self.protocols.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    /// 走某个 outbound。`None` 表示“当前选中的节点”。
    Proxy {
        #[serde(default)]
        outbound: Option<String>,
    },
    Direct,
    Block,
}

impl RuleAction {
    /// 对应的 Xray outboundTag（`None` 由调用方替换为当前节点 tag）。
    pub fn tag(&self) -> Option<&'static str> {
        match self {
            Self::Proxy { outbound: None } => None,
            Self::Proxy { outbound: Some(_) } => None, // 由调用方按 outbound 值填充
            Self::Direct => Some("direct"),
            Self::Block => Some("block"),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Proxy { outbound: None } => "代理".into(),
            Self::Proxy { outbound: Some(t) } => format!("代理({t})"),
            Self::Direct => "直连".into(),
            Self::Block => "拦截".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingRule {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub when: MatchCondition,
    pub then: RuleAction,
}

fn yes() -> bool {
    true
}

impl RoutingRule {
    pub fn new(id: impl Into<String>, name: impl Into<String>, when: MatchCondition, then: RuleAction) -> Self {
        Self { id: id.into(), name: name.into(), enabled: true, when, then }
    }
}

// ===========================================================================
// 预设
// ===========================================================================

/// 生成某个预设对应的规则列表。
///
/// **顺序即优先级**，别随意调整：
/// 1. 私有/保留地址 → 直连（否则访问路由器、NAS 会被塞进隧道）
/// 2. 广告域名 → 拦截
/// 3. 大陆域名/IP → 直连
/// 4. 其余 → 代理（由调用方追加兜底规则）
pub fn preset_rules(preset: RoutingPreset) -> Vec<RoutingRule> {
    match preset {
        RoutingPreset::GlobalProxy => vec![],
        RoutingPreset::DirectAll => vec![RoutingRule::new(
            "preset-direct-all",
            "全部直连",
            MatchCondition {
                network: Network::Both,
                ..Default::default()
            },
            RuleAction::Direct,
        )],
        RoutingPreset::BypassMainland => vec![
            RoutingRule::new(
                "preset-private",
                "私有与保留地址直连",
                MatchCondition {
                    domains: vec!["geosite:private".into()],
                    ip: vec!["geoip:private".into()],
                    ..Default::default()
                },
                RuleAction::Direct,
            ),
            RoutingRule::new(
                "preset-ads",
                "拦截常见广告域名",
                MatchCondition {
                    domains: vec!["geosite:category-ads-all".into()],
                    ..Default::default()
                },
                RuleAction::Block,
            ),
            // 这条是**从数据里挖出来的**，不是拍脑袋加的。
            //
            // `geosite:cn` 收了大约 130 个 Google 域名：`www.gstatic.com`、
            // `fonts.gstatic.com`、`g0-g3.gstatic.com`、`dl.google.com`、
            // `fonts.googleapis.com`、`update.googleapis.com`、
            // `safebrowsing.googleapis.com`，还有一整套 `pki.goog`
            // （OCSP / CRL，证书吊销检查）。它们在列表里被当作「国内可达」，
            // 实际早就被墙 —— 于是被判去直连，连接直接超时。
            //
            // 症状很有迷惑性：`google.com`、`youtube.com` 都正常（不在 CN
            // 列表里，走的是兜底代理），只有 `www.gstatic.com`、`dl.google.com`
            // 这类挂掉。用户看到的是「Google 有的能开、有的不能开」。
            //
            // 顺序很关键：必须排在 `preset-cn-domain` **之前**才覆盖得住它；
            // 又必须排在 `preset-ads` **之后**，否则 `google-analytics.com`、
            // `doubleclick.net` 这些本就属于广告拦截目标的域名会被放去代理。
            //
            // 为什么不用 `geosite:gfw`：CN 与 GFW 的交集只有 40 条，
            // 而且**一条 Google 域名都没有**（实测）。用 gfw 看着合理，
            // 实际什么都不会变。`geosite:google` 则完整覆盖上面这些。
            RoutingRule::new(
                "preset-proxy-google",
                "Google 系域名走代理（被大陆列表误收录）",
                MatchCondition {
                    domains: vec!["geosite:google".into()],
                    ..Default::default()
                },
                RuleAction::Proxy { outbound: None },
            ),
            RoutingRule::new(
                "preset-cn-domain",
                "大陆域名直连",
                MatchCondition {
                    domains: vec!["geosite:cn".into()],
                    ..Default::default()
                },
                RuleAction::Direct,
            ),
            RoutingRule::new(
                "preset-cn-ip",
                "大陆 IP 直连",
                MatchCondition {
                    ip: vec!["geoip:cn".into()],
                    ..Default::default()
                },
                RuleAction::Direct,
            ),
        ],
        RoutingPreset::WhitelistProxy => vec![
            RoutingRule::new(
                "preset-private",
                "私有与保留地址直连",
                MatchCondition {
                    ip: vec!["geoip:private".into()],
                    ..Default::default()
                },
                RuleAction::Direct,
            ),
            RoutingRule::new(
                "preset-proxy-list",
                "代理列表内走代理（在“规则”页编辑）",
                MatchCondition {
                    domains: vec!["geosite:geolocation-!cn".into()],
                    ..Default::default()
                },
                RuleAction::Proxy { outbound: None },
            ),
            RoutingRule::new(
                "preset-fallback-direct",
                "兜底直连",
                MatchCondition::default(),
                RuleAction::Direct,
            ),
        ],
        RoutingPreset::Custom => vec![],
    }
}

/// 把中间表示编译成 Xray 的 `routing.rules` 数组。
///
/// * `selected_tag` 用于替换 `RuleAction::Proxy { outbound: None }`。
/// * 返回的规则里，凡是 `MatchCondition` **所有字段都为空**的，会成为 Xray 的
///   catch-all 规则（无任何条件 → 匹配一切），必须由调用方保证它排在最后。
pub fn compile(rules: &[RoutingRule], selected_tag: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for rule in rules.iter().filter(|r| r.enabled) {
        let tag = match &rule.then {
            RuleAction::Proxy { outbound: Some(t) } => t.clone(),
            RuleAction::Proxy { outbound: None } => selected_tag.to_string(),
            RuleAction::Direct => "direct".to_string(),
            RuleAction::Block => "block".to_string(),
        };
        let when = &rule.when;

        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), "field".into());
        obj.insert("outboundTag".into(), tag.into());
        obj.insert("ruleTag".into(), rule.id.clone().into());

        if !when.inbound_tags.is_empty() {
            obj.insert("inboundTag".into(), json_arr(&when.inbound_tags));
        }
        if !when.domains.is_empty() {
            obj.insert("domain".into(), json_arr(&when.domains));
        }
        if !when.ip.is_empty() {
            obj.insert("ip".into(), json_arr(&when.ip));
        }
        if !when.source_ip.is_empty() {
            obj.insert(
                "source".into(),
                json_arr(&when.source_ip.iter().map(|c| c.to_string()).collect::<Vec<_>>()),
            );
        }
        if !when.ports.is_empty() {
            obj.insert(
                "port".into(),
                serde_json::Value::String(
                    when.ports.iter().map(|p| p.as_xray()).collect::<Vec<_>>().join(","),
                ),
            );
        }
        if !when.process_names.is_empty() {
            obj.insert("processName".into(), json_arr(&when.process_names));
        }
        if !when.protocols.is_empty() {
            obj.insert("protocol".into(), json_arr(&when.protocols));
        }
        if let Some(n) = when.network.as_xray() {
            obj.insert("network".into(), n.into());
        }

        // Xray 要求每条 `field` 规则**至少有一个生效字段**，否则启动时报
        //     app/router: this rule has no effective fields
        // 并拒绝加载整个配置。
        //
        // 这在「匹配一切」的规则上很容易踩到：`MatchCondition::default()` 编译出来
        // 是一个空对象 `{}` —— 从我们的角度看语义明确（无条件 = 匹配所有），
        // 但 Xray 不这么认为。
        //
        // 修法不是删掉这条规则，而是给它一个**恒真**的条件：Xray 只有 tcp 和 udp
        // 两种网络，写全就等于不限。这也正是 `internal-fallback` 一直在用的形式。
        let has_condition = obj.keys().any(|k| !matches!(k.as_str(), "type" | "outboundTag" | "ruleTag"));
        if !has_condition {
            obj.insert("network".into(), "tcp,udp".into());
        }

        out.push(serde_json::Value::Object(obj));
    }
    out
}

fn json_arr(items: &[String]) -> serde_json::Value {
    serde_json::Value::Array(items.iter().map(|s| serde_json::Value::String(s.clone())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_matcher_matches() {
        assert!(PortMatcher::Single(443).matches(443));
        assert!(!PortMatcher::Single(443).matches(80));
        assert!(PortMatcher::Range { from: 1000, to: 2000 }.matches(1500));
        assert!(PortMatcher::Raw("80,443".into()).matches(443));
        assert!(PortMatcher::Raw("1000-2000".into()).matches(2000));
        assert!(!PortMatcher::Raw("1000-2000".into()).matches(2001));
    }

    #[test]
    fn compile_skips_disabled_and_resolves_selected_tag() {
        let mut a = RoutingRule::new(
            "r1",
            "代理某域名",
            MatchCondition { domains: vec!["example.com".into()], ..Default::default() },
            RuleAction::Proxy { outbound: None },
        );
        let b = RoutingRule::new("r2", "禁用", MatchCondition::default(), RuleAction::Direct);
        let mut b2 = b.clone();
        b2.enabled = false;
        let rules = vec![a.clone(), b2];
        let out = compile(&rules, "node-abc");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["outboundTag"], "node-abc");
        assert_eq!(out[0]["domain"][0], "example.com");
        assert_eq!(out[0]["ruleTag"], "r1");

        // 禁用开关生效
        a.enabled = false;
        assert!(compile(&[a], "node-abc").is_empty());
    }

    /// 回归测试：空条件规则必须补上恒真的 `network`，否则 Xray 拒绝加载。
    ///
    /// 真实故障：预设选「全部直连」时，`MatchCondition::default()` 编译成 `{}`，
    /// 核心报 `app/router: this rule has no effective fields` 并拒绝启动。
    #[test]
    fn catch_all_rule_gets_a_truthy_condition() {
        let r = RoutingRule::new("all", "兜底", MatchCondition::default(), RuleAction::Direct);
        let out = compile(&[r], "node-abc");
        let obj = out[0].as_object().unwrap();
        assert_eq!(obj["network"], "tcp,udp", "必须补上恒真条件");
        assert_eq!(obj["outboundTag"], "direct");
        assert_eq!(obj.len(), 4, "type/outboundTag/ruleTag + network");
    }

    /// 更强的保证：**任何**预设产出的规则，编译后都必须至少有一个生效字段。
    ///
    /// 单点修 `catch_all_rule` 只能挡住已知的那一条；这条测试扫全部预设，
    /// 将来新增预设时也会被覆盖。
    #[test]
    fn every_preset_compiles_to_rules_with_effective_fields() {
        use crate::model::RoutingPreset;
        for preset in [
            RoutingPreset::GlobalProxy,
            RoutingPreset::BypassMainland,
            RoutingPreset::WhitelistProxy,
            RoutingPreset::DirectAll,
            RoutingPreset::Custom,
        ] {
            for rule in preset_rules(preset) {
                let compiled = compile(std::slice::from_ref(&rule), "node-abc");
                for r in &compiled {
                    let obj = r.as_object().unwrap();
                    let effective = obj
                        .keys()
                        .filter(|k| !matches!(k.as_str(), "type" | "outboundTag" | "ruleTag"))
                        .count();
                    assert!(
                        effective >= 1,
                        "预设 {preset:?} 的规则 {:?} 编译后没有生效字段，Xray 会拒绝加载：{obj:?}",
                        rule.id
                    );
                }
            }
        }
    }

    /// 同理，用户手写的空条件自定义规则也要被兜住。
    #[test]
    fn custom_empty_condition_rule_also_gets_network() {
        let r = RoutingRule::new("mine", "我的兜底", MatchCondition::default(), RuleAction::Proxy { outbound: None });
        let out = compile(&[r], "node-xyz");
        assert_eq!(out[0]["network"], "tcp,udp");
        assert_eq!(out[0]["outboundTag"], "node-xyz");
    }

    #[test]
    fn bypass_mainland_orders_ads_before_cn() {
        let rules = preset_rules(RoutingPreset::BypassMainland);
        let ids: Vec<_> = rules.iter().map(|r| r.id.as_str()).collect();
        let ads = ids.iter().position(|x| *x == "preset-ads").unwrap();
        let cn = ids.iter().position(|x| *x == "preset-cn-domain").unwrap();
        assert!(ads < cn, "广告拦截必须先于大陆直连，否则会被直连规则吃掉");
    }

    /// Google 必须夹在「广告拦截」与「大陆直连」之间。
    ///
    /// 钉住的是一个真实故障：`geosite:cn` 里收了约 130 个 Google 域名
    /// （`www.gstatic.com`、`dl.google.com`、`fonts.googleapis.com`、
    /// `pki.goog` 等），它们被判去直连、然后被墙掉，表现为
    /// 「Google 有的能开有的不能开」。
    ///
    /// 两侧的顺序都不能动：
    /// * 跑到 `preset-cn-domain` 后面 → 被直连规则吃掉，修复失效；
    /// * 跑到 `preset-ads` 前面 → `google-analytics.com` / `doubleclick.net`
    ///   这些本该被拦的广告域名会被放去代理。
    #[test]
    fn bypass_mainland_proxies_google_between_ads_and_cn() {
        let rules = preset_rules(RoutingPreset::BypassMainland);
        let ids: Vec<_> = rules.iter().map(|r| r.id.as_str()).collect();
        let ads = ids.iter().position(|x| *x == "preset-ads").unwrap();
        let google = ids.iter().position(|x| *x == "preset-proxy-google").unwrap();
        let cn = ids.iter().position(|x| *x == "preset-cn-domain").unwrap();
        assert!(ads < google, "Google 规则必须在广告拦截之后");
        assert!(google < cn, "Google 规则必须在大陆直连之前，否则形同虚设");

        let rule = &rules[google];
        assert!(
            matches!(rule.then, RuleAction::Proxy { outbound: None }),
            "Google 规则必须走当前节点（outbound: None 表示由调用方替换为选中节点）"
        );
        assert!(
            rule.when.domains.iter().any(|d| d == "geosite:google"),
            "必须用 geosite:google；用 geosite:gfw 无效 —— 它与 CN 的交集里没有任何 Google 域名"
        );
    }
}
