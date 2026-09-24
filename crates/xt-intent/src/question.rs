//! 把一条候选**端点**翻译成 Jev 的「类型化问题」。
//!
//! # 三条来自上游的硬约束（违反任何一条都会 422）
//!
//! * `state` 只能是文本 / JSON，**没有图片** —— Jev 不是多模态对话模型；
//! * `choice` 的选项最多 **255** 个、非空；`score` 的级别只能是 **2–10** 个；
//! * **问题 id 不会发给模型**（上游明确写了 "Question ids are yours only"），
//!   所以完整问题必须写在 `instructions` 里，而且要用**英文** ——
//!   上游说模型在英文上最稳。
//!
//! # 与 `jev-x-filter` 的关系
//!
//! 那个扩展问的是**推文内容**（`adult` / `solicitation` / `category` / `severity`），
//! 这里问的是**端点身份**。措辞必须重新设计（沿用"问原型、不问关键词"的方法论），
//! 不能照抄 —— 见 `docs/design/INTENT-FILTER.md` §7.2。

use serde_json::{json, Map, Value};

use crate::answer::Answers;

/// 端点的类别。id 只在本地使用（不会发给模型），但**响应的 `answers` 用它做键**，
/// 所以它必须与 `questions` 里的键逐字一致。
pub const Q_ENDPOINT_KIND: &str = "endpoint_kind";

/// 它像不像广告/投放基础设施（`noul` = 「是」的概率）。
pub const Q_ADS_INTENT: &str = "ads_intent";

/// 拦了它会不会把用户正在用的东西弄坏（`noul` = 「会」的概率）。
///
/// 这一问是专门给**误杀**留的刹车：它让模型有机会说"我知道它像广告，但拦了会坏"，
/// 而不是被一个词逼着表态。
pub const Q_RISK_OF_BREAKAGE: &str = "risk_of_breakage";

/// `state` 的长度上限。与扩展一致（4000 字符）—— 多出来的信息只会稀释信号。
pub const STATE_LIMIT: usize = 4000;

/// 类别的稳定字符串（上游 `choice` 的标签）。
pub const KIND_AD_OR_MONETIZATION: &str = "ad_or_monetization";
pub const KIND_TRACKER_OR_ANALYTICS: &str = "tracker_or_analytics";
pub const KIND_CDN_OR_INFRA: &str = "cdn_or_infra";
pub const KIND_API_OR_SERVICE: &str = "api_or_service";
pub const KIND_HUMAN_SITE: &str = "human_site";
pub const KIND_UNKNOWN: &str = "unknown";

/// `endpoint_kind` 的全部合法标签。解析答案时用它做白名单 ——
/// 不认识的标签一律判 `Unknown`（扩展那边会原样接受，**这个坑不要抄**）。
pub const KIND_LABELS: &[&str] = &[
    KIND_AD_OR_MONETIZATION,
    KIND_TRACKER_OR_ANALYTICS,
    KIND_CDN_OR_INFRA,
    KIND_API_OR_SERVICE,
    KIND_HUMAN_SITE,
    KIND_UNKNOWN,
];

const CHOICE_INSTRUCTIONS: &str = "Classify the network endpoint named in HOST from the name itself and the context lines. \
Answer ad_or_monetization for advertising, retargeting or ad-exchange infrastructure; \
tracker_or_analytics for telemetry, analytics, fingerprinting or tracking beacons; \
cdn_or_infra for a content delivery network, static asset host or infrastructure endpoint; \
api_or_service for an application programming interface or backend service of a product; \
human_site for a website a person intentionally visits; \
unknown when the name carries no usable signal. \
A hostname that merely sounds promotional is NOT automatically an ad endpoint; \
a first-party API or CDN of the site the user is visiting is NOT an ad endpoint.";

const ADS_INSTRUCTIONS: &str = "This network endpoint is advertising, retargeting, tracking or data-monetization infrastructure \
rather than a site or API the user intentionally uses. \
Third-party ad exchanges, bidder and mediation endpoints, fingerprinting and telemetry beacons count. \
A site's own CDN, its own API, and first-party analytics on the site's own domain do NOT count. \
The user never typed this hostname; the device connected to it as a side effect of loading something else.";

const RISK_INSTRUCTIONS: &str = "Blocking this network endpoint would break something the user is actively using, \
or stop a page, app or login from working. \
Content delivery, authentication, payment, update and API endpoints on which a visible product depends count as high risk. \
A pure advertising or tracking endpoint does not. \
If you are not sure whether something depends on it, answer high.";

/// 一道类型化问题。序列化形状必须与上游逐字一致（`types.d.ts` 的三种 answer type）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// 一个是/否概率。`criteria` 可选（`{true, false}` 的说明）。
    Noul {
        instructions: String,
        criteria: Option<(String, String)>,
    },
    /// 多选一。`criteria` 是 `标签 -> 说明`；说明可以是 `None`（映射成 JSON `null`）。
    Choice {
        instructions: String,
        criteria: Vec<(String, Option<String>)>,
    },
    /// 打分。级别是**有序数组**，索引即分值。
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

impl Question {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul { instructions: instructions.into(), criteria: None }
    }

    pub fn noul_with_criteria(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some((yes.into(), no.into())),
        }
    }

    pub fn choice(instructions: impl Into<String>, criteria: Vec<(String, Option<String>)>) -> Self {
        Self::Choice { instructions: instructions.into(), criteria }
    }

    /// 这道问题在线上 JSON 里的形状。
    pub fn to_wire(&self) -> Value {
        match self {
            Self::Noul { instructions, criteria } => {
                let mut m = Map::new();
                m.insert("type".into(), json!("noul"));
                m.insert("instructions".into(), json!(instructions));
                if let Some((yes, no)) = criteria {
                    m.insert("criteria".into(), json!({ "true": yes, "false": no }));
                }
                Value::Object(m)
            }
            Self::Choice { instructions, criteria } => {
                let mut c = Map::new();
                for (label, desc) in criteria {
                    c.insert(label.clone(), desc.clone().map(Value::String).unwrap_or(Value::Null));
                }
                let mut m = Map::new();
                m.insert("type".into(), json!("choice"));
                m.insert("instructions".into(), json!(instructions));
                m.insert("criteria".into(), Value::Object(c));
                Value::Object(m)
            }
            Self::Score { instructions, criteria } => {
                let mut m = Map::new();
                m.insert("type".into(), json!("score"));
                m.insert("instructions".into(), json!(instructions));
                m.insert("criteria".into(), json!(criteria));
                Value::Object(m)
            }
        }
    }

    /// 上游的本地校验规则（违反会在**发请求之前**就被我们拦下来）。
    ///
    /// 为什么要在本地也做一遍：把 422 留给网络的代价是一次往返 + 一条不可读的错误，
    /// 而这里的判据是确定的。
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Noul { instructions, .. } | Self::Choice { instructions, .. } | Self::Score { instructions, .. } => {
                if instructions.trim().is_empty() {
                    return Err("instructions 不能为空".into());
                }
            }
        }
        match self {
            Self::Choice { criteria, .. } => {
                if criteria.is_empty() {
                    return Err("choice 的 criteria 不能为空".into());
                }
                if criteria.len() > 255 {
                    return Err(format!("choice 最多 255 个选项，现在是 {}", criteria.len()));
                }
                let mut seen = std::collections::BTreeSet::new();
                for (label, _) in criteria {
                    if !seen.insert(label.as_str()) {
                        return Err(format!("choice 有重复标签：{label}"));
                    }
                }
            }
            Self::Score { criteria, .. } => {
                if criteria.len() < 2 || criteria.len() > 10 {
                    return Err(format!("score 的级别必须在 2..=10，现在是 {}", criteria.len()));
                }
            }
            Self::Noul { .. } => {}
        }
        Ok(())
    }
}

/// 一条端点的上下文。字段刻意少而稳：每一个都是**在数据面真的能拿到**的东西
/// （见 `ConnectionRecord`），而不是"希望有"的字段。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowContext {
    /// 被判定主机名（小写，已剥掉端口）。
    pub host: String,
    pub port: Option<u16>,
    /// `tcp` / `udp`。
    pub network: String,
    /// 命中的入站 tag：`tun` / `socks` / `http`。
    pub inbound: String,
    /// 发起连接的进程名（拿不到就 `None`）。**不要编**一个默认值。
    pub process: Option<String>,
    /// 这个主机名在本次会话里是否出现过（第二次起可以直接命中缓存，不会走到这里）。
    pub seen_before: bool,
    /// 能推断出的"上一跳"主文档域名（拿不到就 `None`）。
    pub page_host: Option<String>,
    /// 额外的一行自由说明（例如"同 registrable 域下已有广告判决"）。
    pub note: Option<String>,
}

/// 构造一次域名判定的请求（一个域名一次请求）。
///
/// 为什么一个域名一次请求、而不是把 N 个域名塞进一次：`state` 是**整条**上下文的文本，
/// 多个域名会让模型分不清"哪句话在说哪个域名"。省下的那点钱不值得赌判决质量。
pub fn domain_request(ctx: &FlowContext, model: &str) -> IntentRequest {
    IntentRequest {
        state: render_state(ctx),
        questions: vec![
            (Q_ENDPOINT_KIND.to_string(), endpoint_kind_question()),
            (Q_ADS_INTENT.to_string(), ads_intent_question()),
            (Q_RISK_OF_BREAKAGE.to_string(), risk_question()),
        ],
        model: model.to_string(),
    }
}

pub fn endpoint_kind_question() -> Question {
    Question::choice(
        CHOICE_INSTRUCTIONS,
        KIND_LABELS
            .iter()
            .map(|label| {
                let desc = match *label {
                    KIND_AD_OR_MONETIZATION => "Advertising, retargeting or ad-exchange infrastructure",
                    KIND_TRACKER_OR_ANALYTICS => "Telemetry, analytics, fingerprinting or tracking",
                    KIND_CDN_OR_INFRA => "Content delivery, static assets or infrastructure",
                    KIND_API_OR_SERVICE => "API or backend service of a product",
                    KIND_HUMAN_SITE => "A website a person intentionally visits",
                    _ => "",
                };
                ((*label).to_string(), if desc.is_empty() { None } else { Some(desc.to_string()) })
            })
            .collect(),
    )
}

pub fn ads_intent_question() -> Question {
    Question::noul_with_criteria(
        ADS_INSTRUCTIONS,
        "advertising, retargeting, tracking or monetization infrastructure",
        "a site, CDN, API or service the user's activity actually needs",
    )
}

pub fn risk_question() -> Question {
    Question::noul_with_criteria(
        RISK_INSTRUCTIONS,
        "blocking it would break something the user is using",
        "blocking it would not break anything",
    )
}

/// 渲染 `state` 文本。
///
/// 形态与扩展的 `buildState` 同构（`字段组:` 段落 + `key=value;`），
/// 因为那是实测能工作的一种形状；但内容完全不同 —— 这里没有正文，只有端点身份。
pub fn render_state(ctx: &FlowContext) -> String {
    let mut out = String::new();
    out.push_str("ENDPOINT:\n");
    out.push_str(&format!("host={}\n", ctx.host));
    out.push_str(&format!(
        "port={}\n",
        ctx.port.map(|p| p.to_string()).unwrap_or_else(|| "unknown".into())
    ));
    out.push_str(&format!("network={}\n", if ctx.network.is_empty() { "unknown" } else { &ctx.network }));

    out.push_str("\nCONTEXT:\n");
    let mut bits = Vec::new();
    bits.push(format!(
        "inbound={}",
        if ctx.inbound.is_empty() { "unknown" } else { &ctx.inbound }
    ));
    if let Some(p) = ctx.process.as_deref().filter(|p| !p.trim().is_empty()) {
        bits.push(format!("process={p}"));
    }
    bits.push(format!("seen_before={}", if ctx.seen_before { "yes" } else { "no" }));
    if let Some(page) = ctx.page_host.as_deref().filter(|p| !p.trim().is_empty()) {
        bits.push(format!("loaded_while_visiting={page}"));
    }
    out.push_str(&bits.join("; "));
    out.push('\n');
    if let Some(note) = ctx.note.as_deref().filter(|n| !n.trim().is_empty()) {
        out.push_str(&format!("note={note}\n"));
    }
    truncate(out, STATE_LIMIT)
}

/// 一次请求的全部内容。**问题 id 只在本地使用**，但 `answers` 会用同一个 id 回来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRequest {
    pub state: String,
    pub questions: Vec<(String, Question)>,
    pub model: String,
}

impl IntentRequest {
    /// 上游的线上形状：`{ state, questions: { id: question }, model }`。
    pub fn to_wire(&self) -> Value {
        let mut questions = Map::new();
        for (id, q) in &self.questions {
            questions.insert(id.clone(), q.to_wire());
        }
        json!({
            "state": self.state,
            "questions": Value::Object(questions),
            "model": self.model,
        })
    }

    /// 发请求前把自己校验一遍（返回**全部**问题，而不是第一个）。
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        if self.state.trim().is_empty() {
            errs.push("state 不能为空".into());
        }
        if self.questions.is_empty() {
            errs.push("questions 不能为空".into());
        }
        for (id, q) in &self.questions {
            if let Err(e) = q.validate() {
                errs.push(format!("{id}: {e}"));
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    pub fn ids(&self) -> Vec<&str> {
        self.questions.iter().map(|(id, _)| id.as_str()).collect()
    }

    /// 从响应里读答案。**期望的 id 少一个都算失败** —— 宁可放行，也不要拿半份答案定罪。
    pub fn read_answers(&self, answers: &Answers) -> Option<Answers> {
        for (id, _) in &self.questions {
            if !answers.has(id) {
                return None;
            }
        }
        Some(answers.clone())
    }
}

/// 按**字符**（不是字节）截断，避免在多字节码点上切出非法 UTF-8。
fn truncate(mut s: String, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s;
    }
    let cut = s.char_indices().nth(limit).map(|(i, _)| i).unwrap_or(s.len());
    s.truncate(cut);
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FlowContext {
        FlowContext {
            host: "adsrv-7f3.example".into(),
            port: Some(443),
            network: "tcp".into(),
            inbound: "tun".into(),
            process: Some("Safari".into()),
            seen_before: false,
            page_host: Some("news.example".into()),
            note: None,
        }
    }

    #[test]
    fn wire_shape_matches_upstream_exactly() {
        let req = domain_request(&ctx(), "jev-latest");
        let wire = req.to_wire();

        // 顶层只有三个键，一个不多一个不少。
        let obj = wire.as_object().expect("顶层必须是对象");
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["model", "questions", "state"]);

        // 三道问题：choice + noul + noul，键与 id 逐字一致。
        let qs = wire["questions"].as_object().unwrap();
        assert_eq!(qs.len(), 3);
        assert!(qs.contains_key(Q_ENDPOINT_KIND));
        assert!(qs.contains_key(Q_ADS_INTENT));
        assert!(qs.contains_key(Q_RISK_OF_BREAKAGE));
        assert_eq!(qs[Q_ENDPOINT_KIND]["type"], "choice");
        assert_eq!(qs[Q_ADS_INTENT]["type"], "noul");
        assert_eq!(qs[Q_RISK_OF_BREAKAGE]["type"], "noul");

        // choice 的 criteria 是「标签 -> 说明」，`unknown` 的说明是 null（上游用 null 表示无说明）。
        let crit = qs[Q_ENDPOINT_KIND]["criteria"].as_object().unwrap();
        assert_eq!(crit.len(), KIND_LABELS.len());
        assert!(crit[KIND_UNKNOWN].is_null());
        assert!(crit[KIND_AD_OR_MONETIZATION].is_string());

        // noul 的 criteria 是 {true,false}。
        let noul_crit = qs[Q_ADS_INTENT]["criteria"].as_object().unwrap();
        assert!(noul_crit.contains_key("true") && noul_crit.contains_key("false"));
    }

    #[test]
    fn request_validates_and_reports_every_problem() {
        let good = domain_request(&ctx(), "m");
        assert!(good.validate().is_ok());

        let bad = IntentRequest {
            state: "   ".into(),
            questions: vec![(
                "q".into(),
                Question::Choice { instructions: "x".into(), criteria: vec![] },
            )],
            model: "m".into(),
        };
        let errs = bad.validate().unwrap_err();
        assert_eq!(errs.len(), 2, "state 与空 criteria 都要报出来：{errs:?}");
    }

    #[test]
    fn choice_and_score_limits_follow_upstream() {
        // choice：256 个选项 → 拒绝
        let too_many: Vec<(String, Option<String>)> =
            (0..256).map(|i| (format!("l{i}"), None)).collect();
        assert!(Question::choice("x", too_many).validate().is_err());

        // score：1 级与 11 级都拒绝，2..=10 接受
        assert!(Question::Score { instructions: "x".into(), criteria: vec!["a".into()] }.validate().is_err());
        assert!(Question::Score { instructions: "x".into(), criteria: (0..11).map(|i| i.to_string()).collect() }
            .validate()
            .is_err());
        assert!(Question::Score { instructions: "x".into(), criteria: vec!["a".into(), "b".into()] }
            .validate()
            .is_ok());

        // 空 instructions 拒绝
        assert!(Question::noul("  ").validate().is_err());
    }

    #[test]
    fn repeated_choice_label_is_rejected() {
        let q = Question::choice(
            "x",
            vec![("a".into(), None), ("a".into(), None)],
        );
        assert!(q.validate().is_err());
    }

    #[test]
    fn state_carries_the_endpoint_and_never_invents_a_process() {
        let s = render_state(&ctx());
        assert!(s.contains("host=adsrv-7f3.example"));
        assert!(s.contains("port=443"));
        assert!(s.contains("process=Safari"));
        assert!(s.contains("loaded_while_visiting=news.example"));
        assert!(s.contains("seen_before=no"));

        // 拿不到 process 时**不许**出现 `process=` 这种空字段。
        let mut c = ctx();
        c.process = None;
        c.page_host = None;
        let s = render_state(&c);
        assert!(!s.contains("process="), "不许编进程名：{s}");
        assert!(s.contains("inbound=tun"));
    }

    #[test]
    fn state_is_truncated_by_chars_not_bytes() {
        let mut c = ctx();
        c.note = Some("中".repeat(STATE_LIMIT * 2));
        let s = render_state(&c);
        assert!(s.chars().count() <= STATE_LIMIT + 1, "截断后长度 {} 超标", s.chars().count());
        // 仍然是合法 UTF-8 且以省略号结尾
        assert!(s.ends_with('…'));
    }

    #[test]
    fn answers_missing_any_expected_id_are_refused() {
        let req = domain_request(&ctx(), "m");
        let partial = json!({
            "answers": {
                Q_ENDPOINT_KIND: { "type": "choice", "choice": KIND_AD_OR_MONETIZATION, "confidence": 0.9 },
                Q_ADS_INTENT: { "type": "noul", "noul": 0.99 }
                // 缺 risk_of_breakage
            }
        });
        let answers = Answers::from_wire(&partial).unwrap();
        assert!(req.read_answers(&answers).is_none());
    }
}
