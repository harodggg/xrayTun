//! 离线评测 CLI：把本机真实连接日志跑成一份报告。
//!
//! ```bash
//! # 只算 L0 静态名单的覆盖（不需要模型、不联网）
//! cargo run -p xt-intent --example eval_domains -- \
//!   --corpus "$HOME/Library/Application Support/com.xraytun.desktop/logs/app.jsonl" \
//!   --corpus "$HOME/Library/Application Support/com.xraytun.desktop/logs/app.1.jsonl" \
//!   --geosite-dir apps/desktop/binaries \
//!   --report /tmp/intent-eval.md
//!
//! # 带上模型（需要一个能问通的 Jev 网关）
//! cargo run -p xt-intent --example eval_domains -- \
//!   --corpus ... --geosite-dir apps/desktop/binaries --report /tmp/intent-eval.md \
//!   --live --base-url https://opencode.ai/zen --model jev-1.13-free --limit 200
//! ```
//!
//! # 隐私
//!
//! 报告只有聚合数字。域名列表要用 `--dump-unknown <路径>` **显式**导出，
//! 而且应该导到仓库之外 —— 那里面是这台机器的浏览记录。

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use xt_core::xray::access_log::{ConnectionLog, ConnectionRecord};
use xt_intent::engine::{IntentConfig, IntentEngine};
use xt_intent::eval::{
    analyze_record, metrics_from, render_diagnosis, render_report, CorpusObserver, GeoSiteLabels,
    Judgement, Label, RawAnswerRecord,
};
use xt_intent::gateway::{Gateway, GatewayError, GatewayResponse};
use xt_intent::jev::{JevConfig, JevGateway};
use xt_intent::observer::is_candidate;
use xt_intent::question::IntentRequest;
use xt_intent::transport::TlsTransport;

struct Args {
    corpus: Vec<PathBuf>,
    geosite_dir: PathBuf,
    report: PathBuf,
    dump_unknown: Option<PathBuf>,
    live: bool,
    base_url: String,
    model: String,
    api_key: Option<String>,
    limit: usize,
    /// 每条候选之间的间隔。
    ///
    /// **默认不是 0，而且这不是"保守"而是"必须"**：免密钥档的额度是按分钟计的，
    /// 紧循环跑会把额度打满、把 `gateway_errors` 刷到覆盖大部分样本 ——
    /// 那样量到的是配额，不是模型（本机实测：第一轮 120 条拿到 36 条无答案，
    /// 紧接着的第二轮 196 条错误 / 106 条候选有 93 条无答案）。
    sleep_ms: u64,
    /// `ads_intent_min`：判定"是广告"的概率下限。**默认取产品默认值 0.85**。
    ///
    /// 它是这次实验唯一要动的旋钮：把阈值调低看**无标注桶会不会真的产出 block** ——
    /// 那是区分「阈值太保守」与「这个档位的模型根本判不动」的唯一办法。
    ads_min: f32,
    /// `risk_of_breakage_max`（风险刹车）。产品默认 0.3。
    risk_max: f32,
    /// `choice_confidence_min`（模型自报置信度下限）。产品默认 0.5。
    ///
    /// 第三个闸门。**不测它就说"不是阈值问题"是不严谨的** —— 模型可能给出很高的
    /// `ads_intent` 但自报置信度很低，那样前两个旋钮怎么调都不会有 block。
    choice_min: f32,
    /// 把引擎的审计（含 outcome/reason）写到这里（**仓库外**）。
    ///
    /// 只是旁证：判定用的原始答案由 `--raw-answers` 单独录。给了它就等于给引擎
    /// 一个 `data_root`，缓存与审计都在这个目录里（用**全新目录**才不会命中旧缓存）。
    audit_dir: Option<PathBuf>,
    /// 把**每一次网关调用的原始答案**写成 JSONL（**仓库外**，含域名）。
    raw_answers: Option<PathBuf>,
    /// 把聚合诊断（不含域名）另存一份 Markdown。
    answers_summary: Option<PathBuf>,
    /// 只把预算给**无标注桶**（task-13 的诊断跑法）。
    ///
    /// 不设它时 `--limit` 会先喂正负样本（那是为了量精确率/误杀率），
    /// 无标注桶可能拿到很少的预算 —— 而"模型对无标注域名到底答了什么"
    /// 需要整份预算都花在无标注桶上。
    only_unknown: bool,
    /// **只重算诊断，不联网、不读语料**：把已有的 `--raw-answers` 文件重新渲染成
    /// 聚合表（用 `--ads-min` / `--risk-max` / `--choice-min` 这组阈值重算闸门）。
    ///
    /// 存在的理由：诊断结论必须能被别人**不复跑模型**地复核（跑模型要花钱，
    /// 而且不同档位的额度窗口会让样本量变化）。
    summarize: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        corpus: Vec::new(),
        geosite_dir: PathBuf::from("apps/desktop/binaries"),
        report: PathBuf::from("/tmp/intent-eval.md"),
        dump_unknown: None,
        live: false,
        base_url: "https://opencode.ai/zen".into(),
        model: "jev-1.13-free".into(),
        api_key: std::env::var("JEV_API_KEY").ok(),
        limit: 200,
        sleep_ms: 1200,
        ads_min: xt_intent::verdict::Thresholds::default().ads_intent_min,
        risk_max: xt_intent::verdict::Thresholds::default().risk_of_breakage_max,
        choice_min: xt_intent::verdict::Thresholds::default().choice_confidence_min,
        audit_dir: None,
        raw_answers: None,
        answers_summary: None,
        only_unknown: false,
        summarize: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} 需要一个值"));
        match arg.as_str() {
            "--corpus" => a.corpus.push(PathBuf::from(value("--corpus")?)),
            "--geosite-dir" => a.geosite_dir = PathBuf::from(value("--geosite-dir")?),
            "--report" => a.report = PathBuf::from(value("--report")?),
            "--dump-unknown" => a.dump_unknown = Some(PathBuf::from(value("--dump-unknown")?)),
            "--audit-dir" => a.audit_dir = Some(PathBuf::from(value("--audit-dir")?)),
            "--raw-answers" => a.raw_answers = Some(PathBuf::from(value("--raw-answers")?)),
            "--answers-summary" => {
                a.answers_summary = Some(PathBuf::from(value("--answers-summary")?))
            }
            "--only-unknown" => a.only_unknown = true,
            "--summarize" => a.summarize = Some(PathBuf::from(value("--summarize")?)),
            "--live" => a.live = true,
            "--base-url" => a.base_url = value("--base-url")?,
            "--model" => a.model = value("--model")?,
            "--limit" => {
                a.limit = value("--limit")?
                    .parse()
                    .map_err(|_| "--limit 必须是数字".to_string())?
            }
            "--ads-min" => {
                a.ads_min = value("--ads-min")?
                    .parse()
                    .map_err(|_| "--ads-min 必须是数字（0~1）".to_string())?
            }
            "--risk-max" => {
                a.risk_max = value("--risk-max")?
                    .parse()
                    .map_err(|_| "--risk-max 必须是数字（0~1）".to_string())?
            }
            "--choice-min" => {
                a.choice_min = value("--choice-min")?
                    .parse()
                    .map_err(|_| "--choice-min 必须是数字（0~1）".to_string())?
            }
            "--sleep-ms" => {
                a.sleep_ms = value("--sleep-ms")?
                    .parse()
                    .map_err(|_| "--sleep-ms 必须是数字（毫秒）".to_string())?
            }
            "--help" | "-h" => {
                println!(
                    "用法：--corpus <jsonl> [--corpus …] --geosite-dir <dir> --report <md> \
                     [--live] [--limit N] [--sleep-ms N] [--dump-unknown <path>] \
                     [--only-unknown] [--audit-dir <dir>] [--raw-answers <path>] \
                     [--answers-summary <path>]\n\
                     只重算：--summarize <raw.jsonl> [--ads-min X --risk-max X --choice-min X] \
                     [--answers-summary <md>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    if a.corpus.is_empty() && a.summarize.is_none() {
        return Err("至少要给一个 --corpus（或 --summarize <raw.jsonl>）".into());
    }
    Ok(a)
}

/// 只重算诊断：读已有的 `--raw-answers` 文件，不联网、不碰语料。
fn run_summary_only(args: &Args, raw: &std::path::Path) {
    let thresholds = xt_intent::verdict::Thresholds {
        ads_intent_min: args.ads_min,
        risk_of_breakage_max: args.risk_max,
        choice_confidence_min: args.choice_min,
        ..Default::default()
    };
    let text = match std::fs::read_to_string(raw) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("读不了 {}：{e}", raw.display());
            std::process::exit(2);
        }
    };
    let mut analyses = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(rec) = serde_json::from_str::<RawAnswerRecord>(line) else {
            continue;
        };
        // 没有引擎判决 ⇒ 按给定阈值重算（`decide`，bonus=0）。
        analyses.push(analyze_record(&rec, None, &thresholds));
    }
    let md = render_diagnosis(&analyses, analyses.len());
    println!("{md}");
    if let Some(out) = &args.answers_summary {
        if let Err(e) = std::fs::write(out, &md) {
            eprintln!("写聚合诊断失败：{e}");
            std::process::exit(2);
        }
        eprintln!("聚合诊断已写入 {}（不含域名）", out.display());
    }
}

/// 从一行日志里取出真正给 Xray 转发的那段文本。
///
/// App 的日志是 JSONL（`{"message": "...", ...}`）；核心自己的 stdout 是纯文本。
/// 两种都吃，是为了让这个工具在"拿 App 日志"和"拿一份 core stdout 转储"时都能用。
fn message_of(line: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => v
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| line.to_string()),
        Err(_) => line.to_string(),
    }
}

/// 网关装饰器：把**每一次调用的原始答案**录到仓库外的 JSONL。
///
/// # 为什么必须在这一层录，而不是读审计
///
/// `AuditRecord` 的 `category` / `ads_intent` / `risk_of_breakage` /
/// `choice_confidence` 只在 **`Block`** 时才有值（来自 `block_evidence()`）。
/// 而 task-13 要回答的问题恰恰是"**没被拦的那些**，模型到底答了什么"。
/// 审计里那些字段全是 `None`，所以只看审计永远分不开"模型判不动"与
/// "我们的闸门把答案丢了"。
///
/// 这里录的是 `GatewayResponse.answers` 的**原样 JSON**（每个 id 一个对象），
/// 含 `type` / `choice` / `noul` / `confidence` 的真实形状。
///
/// # 隐私
///
/// 文件里**有域名** ⇒ 调用方只会把它写到仓库外（`/tmp`）。
/// 聚合诊断由 `render_diagnosis` 生成，里面没有域名。
struct RecordingGateway<G: Gateway> {
    inner: G,
    /// `None` = 不录（默认）。
    path: Option<PathBuf>,
}

impl<G: Gateway> RecordingGateway<G> {
    fn new(inner: G, path: Option<PathBuf>) -> Self {
        Self { inner, path }
    }

    fn record(&self, request: &IntentRequest, result: &Result<GatewayResponse, GatewayError>) {
        let Some(path) = &self.path else {
            return;
        };
        let host = request
            .state
            .lines()
            .find_map(|l| l.strip_prefix("host="))
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let rec = match result {
            Ok(resp) => {
                let mut answers = serde_json::Map::new();
                for id in resp.answers.ids() {
                    if let Some(v) = resp.answers.raw_of(id) {
                        answers.insert(id.to_string(), v.clone());
                    }
                }
                serde_json::json!({
                    "ts_unix": xt_core::util::now_unix(),
                    "host": host,
                    "ok": true,
                    "model": resp.model.clone(),
                    "expected_ids": request.ids(),
                    "answer_ids": resp.answers.ids(),
                    "answers": answers,
                })
            }
            Err(e) => serde_json::json!({
                "ts_unix": xt_core::util::now_unix(),
                "host": host,
                "ok": false,
                "expected_ids": request.ids(),
                "answer_ids": [],
                "answers": {},
                "error_kind": e.as_str(),
                "error": e.to_string(),
            }),
        };
        match append_private_line(path, &serde_json::to_string(&rec).unwrap_or_default()) {
            Ok(()) => {}
            // 录不上不该让判定失败，但必须留痕（否则"诊断缺数据"无从解释）。
            Err(e) => eprintln!("写原始答案失败（不影响判定）：{e}"),
        }
    }
}

impl<G: Gateway> Gateway for RecordingGateway<G> {
    fn ask(&self, request: &IntentRequest) -> Result<GatewayResponse, GatewayError> {
        let result = self.inner.ask(request);
        self.record(request, &result);
        result
    }

    fn describe(&self) -> String {
        match &self.path {
            Some(p) => format!("{}（原始答案 → {}）", self.inner.describe(), p.display()),
            None => self.inner.describe(),
        }
    }
}

/// 追加一行到**仓库外**的私有文件（0600）。不经过 shell。
fn append_private_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    writeln!(f, "{line}")
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误：{e}");
            std::process::exit(2);
        }
    };

    // 只重算诊断：不联网、不读语料（复核别人的原始答案时用这条）。
    if let Some(raw) = args.summarize.clone() {
        run_summary_only(&args, &raw);
        return;
    }

    // 1) 读语料。**不落盘、不外发**，只在内存里计数。
    let mut log = ConnectionLog::with_capacity(1024); // 环形缓冲不用来统计，小容量即可
    let mut observer = CorpusObserver::new();
    let mut lines_read = 0u64;
    for path in &args.corpus {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("读不了 {}：{e}", path.display());
                std::process::exit(2);
            }
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            lines_read += 1;
            let msg = message_of(&line);
            if !msg.contains("accepted") && !msg.contains("sniffed") {
                continue;
            }
            let observed = log.observe_with_record(&msg);
            if observed.record.is_some() || observed.outbound.is_some() {
                observer.observe(&observed);
            }
        }
    }

    // 2) 标注（geosite）+ 与线上同一套候选过滤。
    let labels = match GeoSiteLabels::load(&args.geosite_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let allow: BTreeSet<String> = BTreeSet::new();
    let dataset = observer.finish(&labels, |host| is_candidate(host, "tun", Some(443), &allow));

    eprintln!(
        "语料 {lines_read} 行 → 连接 {}（配不到域名 {}），观测域名 {}：正样本 {} / 负样本 {} / 未知 {}",
        dataset.connections_total,
        dataset.without_domain,
        dataset.observed_domains(),
        dataset.positives.len(),
        dataset.negatives.len(),
        dataset.unknown.len()
    );

    // 3) 可选：真的问一遍模型。
    let mut notes = vec![
        format!("语料文件 {} 个，共读入 {lines_read} 行", args.corpus.len()),
        format!("候选之间间隔 {} ms（限速；用 --sleep-ms 0 关掉）", args.sleep_ms),
        format!(
            "阈值：ads_intent_min = {}（产品默认 0.85）、risk_of_breakage_max = {}（产品默认 0.3）、\
             choice_confidence_min = {}（产品默认 0.5）",
            args.ads_min, args.risk_max, args.choice_min
        ),
        "否定样本刻意不含 `cn`（它里面既有正常站点也有投放域名，拿它当正常会系统性高估精确率）"
            .to_string(),
    ];
    let mut metrics = None;

    if args.live {
        let config = JevConfig {
            base_url: args.base_url.clone(),
            model: args.model.clone(),
            api_key: args.api_key.clone(),
            timeout: Duration::from_secs(15),
            max_retries: 2,
        };
        let gateway = match JevGateway::new(config, TlsTransport::new()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("网关配置不合法：{e}");
                std::process::exit(2);
            }
        };
        // 原始答案录制（可选）：文件**含域名**，只能写到仓库外。
        let gateway = RecordingGateway::new(gateway, args.raw_answers.clone());
        let mut ic = IntentConfig {
            enabled: true,
            // 评测**必须关掉演练模式**，否则拿不到 block 判决；但下发与否这里无关
            // （评测只读缓存，不碰任何核心）。
            drill: false,
            model: args.model.clone(),
            base_url: args.base_url.clone(),
            per_minute: args.limit as u32,
            per_day: args.limit as u32,
            max_candidates_per_tick: 4,
            cache_max_entries: args.limit + 16,
            ..Default::default()
        };
        // 阈值：默认与产品默认一致（0.85 / 0.3），可用 `--ads-min` / `--risk-max` 覆盖。
        // **报告里会写明用了哪一组**，否则两份数字放一起无法比较。
        ic.thresholds = xt_intent::verdict::Thresholds {
            ads_intent_min: args.ads_min,
            risk_of_breakage_max: args.risk_max,
            choice_confidence_min: args.choice_min,
            ..Default::default()
        };

        // 用**真实时间**：审计的 ts、缓存的 TTL、预算窗口都要有意义。
        // （以前传 0，审计里每条都是 1970 —— 诊断时要按时间对齐就做不了。）
        let now = xt_core::util::now_unix();
        let mut engine = match IntentEngine::new(ic, gateway, args.audit_dir.clone(), now) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("引擎建不起来：{e:?}");
                std::process::exit(2);
            }
        };
        if let Some(dir) = &args.audit_dir {
            eprintln!(
                "审计通道已开启：{}/intent-audit.jsonl（outcome/reason 旁证）",
                dir.display()
            );
        }
        if let Some(p) = &args.raw_answers {
            eprintln!(
                "原始答案录取到 {}（**含域名，勿入库**）",
                p.display()
            );
        }
        let thresholds_used = engine.config().thresholds.clone();

        // 预算怎么分给三个桶，**顺序就是优先级**：
        //   1. 正样本（通常很少）：漏拦它们说明模型失灵，必须全问；
        //   2. 负样本：**误杀率的主指标靠它**，分掉一半剩余预算；
        //   3. 未知桶：真正"模型自己发现"的那批，剩下的都给它。
        //
        // 之前写成"未知桶优先"是错的：`--limit` 一小，正负样本全被挤掉，
        // 于是精确率与误杀率都算不出来，报告只能给"证据不足"。
        //
        // `--only-unknown` 关掉 1/2：诊断"模型对无标注域名到底答了什么"时，
        // 预算必须整份给未知桶（否则样本量不够，结论不可判）。
        let mut queue: Vec<(String, Label, u64)> = Vec::new();
        if !args.only_unknown {
            for o in dataset.positives.iter() {
                queue.push((o.host.clone(), Label::Positive, o.connections));
            }
            let half = args.limit / 2;
            for o in dataset.negatives.iter().take(half) {
                queue.push((o.host.clone(), Label::Negative, o.connections));
            }
        }
        for o in dataset.unknown.iter() {
            queue.push((o.host.clone(), Label::Unknown, o.connections));
        }
        queue.truncate(args.limit);

        eprintln!(
            "开始判定：{} 个候选（预算 {}，{}）",
            queue.len(),
            args.limit,
            if args.only_unknown { "只跑无标注桶" } else { "正/负/未知混跑" }
        );
        let mut samples: Vec<(Label, Judgement, u64)> = Vec::new();
        let mut totals = xt_intent::engine::ClassifyReport::default();
        for (host, label, connections) in &queue {
            // 限速：**在发请求之前**等，而不是撞到 429 之后靠退避重试
            // （重试会把同一个额度窗口浪费掉，还会让"无答案"的比例看起来像模型不行）。
            if args.sleep_ms > 0 {
                std::thread::sleep(Duration::from_millis(args.sleep_ms));
            }
            let rec = synthetic_record(host);
            engine.observe(&rec, now);
            // 立刻跑一轮，让每条候选都有判决（节拍在这里只是形式）。
            //
            // **`asked == 0` 也要退出**：网关失败会把候选放回队列（生产里等下一个
            // 10 秒节拍再试），而这个循环是紧的 —— 不退的话会拿同一条候选反复撞预算，
            // 把额度烧光并刷出一堆 budget_denied（实测 12 条候选产生了 2676 次拒绝）。
            for _ in 0..64 {
                let r = engine.classify_pending(now);
                let done = r.candidates == 0 || r.asked == 0;
                totals.asked += r.asked;
                totals.gateway_errors += r.gateway_errors;
                totals.budget_denied += r.budget_denied;
                totals.schema_invalid += r.schema_invalid;
                totals.blocked += r.blocked;
                totals.allowed += r.allowed;
                totals.deferred += r.deferred;
                if done {
                    break;
                }
            }
            let judgement = match engine.explain_at(host, now) {
                Some(entry) => Judgement::from(&entry.verdict),
                None => Judgement::Deferred,
            };
            samples.push((*label, judgement, *connections));
        }

        let mut m = metrics_from(&samples);
        m.connections_total = dataset.connections_total;
        eprintln!(
            "判定完成：拦 {}（真阳 {} / 误杀 {}），放行 {}，拿不到答案 {}",
            m.true_positive + m.false_positive + m.unknown_blocked,
            m.true_positive,
            m.false_positive,
            m.unknown_allowed + m.true_negative,
            m.deferred
        );
        // 拿不到答案的**原因分布**要写进说明：否则用户只看到"N 条 deferred"，
        // 分不清是"网关限流"还是"我们自己的 schema 解析坏了"。
        //
        // 数据来自 `ClassifyReport` 的计数器，**不是缓存** —— 网关失败与预算耗尽
        // 都刻意不写缓存（免得一个临时故障变成一个持久生效的判决），缓存里查不到。
        let mut reasons: Vec<String> = Vec::new();
        for (name, n) in [
            ("gateway_errors", totals.gateway_errors),
            ("budget_denied", totals.budget_denied),
            ("schema_invalid", totals.schema_invalid),
        ] {
            if n > 0 {
                reasons.push(format!("{name}={n}"));
            }
        }
        if !reasons.is_empty() {
            notes.push(format!("拿不到答案的原因分布：{}", reasons.join("、")));
        }
        notes.push(format!("本次只判定了 {} 个域名（--limit），不是全量", queue.len()));
        notes.push("未知桶里的 block **无法验证**，所以不计入精确率分子（那正是要量的东西）".to_string());
        metrics = Some(m);

        // ---- 6) 原始答案的聚合诊断（task-13）----
        //
        // 只输出**聚合数字**；域名只存在于 `--raw-answers` 那个仓库外文件里。
        if let Some(path) = &args.raw_answers {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let mut analyses = Vec::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(rec) = serde_json::from_str::<RawAnswerRecord>(line) else {
                    continue;
                };
                // 引擎**实际**给的判决（含形状加分）；缓存里没有的（网关失败/预算）
                // 就退回按本次阈值重算，保证每条都有个可读的原因。
                let verdict = engine.explain_at(&rec.host, now).map(|e| e.verdict);
                analyses.push(analyze_record(&rec, verdict.as_ref(), &thresholds_used));
            }
            let md = render_diagnosis(&analyses, queue.len());
            println!("\n{md}");
            if let Some(out) = &args.answers_summary {
                if let Err(e) = std::fs::write(out, &md) {
                    eprintln!("写聚合诊断失败：{e}");
                } else {
                    eprintln!("聚合诊断已写入 {}（不含域名）", out.display());
                }
            }
            notes.push(format!(
                "原始答案诊断：{} 条记录，见输出里的「无标注桶：原始答案分布」一节",
                analyses.len()
            ));
        }
    } else {
        notes.push("未加 --live：只算了 L0 静态名单的覆盖，模型指标缺失".to_string());
    }

    // 4) 报告（聚合数字）。
    let report = render_report(&dataset, metrics.as_ref(), &notes);
    if let Err(e) = std::fs::write(&args.report, &report) {
        eprintln!("写报告失败：{e}");
        std::process::exit(2);
    }
    println!("{report}");
    eprintln!("报告已写入 {}", args.report.display());

    // 5) 可选的域名导出（默认关；写给用户自己看，**不要**进仓库）。
    if let Some(path) = &args.dump_unknown {
        let mut text = String::from("# 无标注的域名（模型要判的那一批）—— 含浏览记录，勿入库\n");
        for o in dataset.unknown.iter().take(args.limit) {
            text.push_str(&format!("{}\t{}\n", o.connections, o.host));
        }
        if let Err(e) = std::fs::write(path, text) {
            eprintln!("写域名列表失败：{e}");
            std::process::exit(2);
        }
        eprintln!("已导出未知域名到 {}（请勿提交进仓库）", path.display());
    }
}

/// 造一条"喂观察器"的记录。域名与入站是观察器真正需要的两个字段。
fn synthetic_record(host: &str) -> ConnectionRecord {
    ConnectionRecord {
        ts_ms: 0,
        ts_text: String::new(),
        from: "198.18.0.1:1".into(),
        network: "tcp".into(),
        target_host: host.to_string(),
        target_port: Some(443),
        inbound_tag: "tun".into(),
        outbound_tag: "node-a".into(),
        domain: Some(host.to_string()),
        domain_paired: true,
        domain_pair_delta_us: None,
        sniff_id: None,
    }
}
