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
use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use xt_core::xray::access_log::{ConnectionLog, ConnectionRecord};
use xt_intent::engine::{IntentConfig, IntentEngine};
use xt_intent::eval::{
    metrics_from, render_report, CorpusObserver, GeoSiteLabels, Judgement, Label,
};
use xt_intent::jev::{JevConfig, JevGateway};
use xt_intent::observer::is_candidate;
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
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} 需要一个值"));
        match arg.as_str() {
            "--corpus" => a.corpus.push(PathBuf::from(value("--corpus")?)),
            "--geosite-dir" => a.geosite_dir = PathBuf::from(value("--geosite-dir")?),
            "--report" => a.report = PathBuf::from(value("--report")?),
            "--dump-unknown" => a.dump_unknown = Some(PathBuf::from(value("--dump-unknown")?)),
            "--live" => a.live = true,
            "--base-url" => a.base_url = value("--base-url")?,
            "--model" => a.model = value("--model")?,
            "--limit" => {
                a.limit = value("--limit")?
                    .parse()
                    .map_err(|_| "--limit 必须是数字".to_string())?
            }
            "--sleep-ms" => {
                a.sleep_ms = value("--sleep-ms")?
                    .parse()
                    .map_err(|_| "--sleep-ms 必须是数字（毫秒）".to_string())?
            }
            "--help" | "-h" => {
                println!("用法：--corpus <jsonl> [--corpus …] --geosite-dir <dir> --report <md> [--live] [--limit N] [--sleep-ms N] [--dump-unknown <path>]");
                std::process::exit(0);
            }
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    if a.corpus.is_empty() {
        return Err("至少要给一个 --corpus".into());
    }
    Ok(a)
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

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误：{e}");
            std::process::exit(2);
        }
    };

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
        ic.thresholds = Default::default();

        let mut engine = match IntentEngine::new(ic, gateway, None, 0) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("引擎建不起来：{e:?}");
                std::process::exit(2);
            }
        };

        // 预算怎么分给三个桶，**顺序就是优先级**：
        //   1. 正样本（通常很少）：漏拦它们说明模型失灵，必须全问；
        //   2. 负样本：**误杀率的主指标靠它**，分掉一半剩余预算；
        //   3. 未知桶：真正"模型自己发现"的那批，剩下的都给它。
        //
        // 之前写成"未知桶优先"是错的：`--limit` 一小，正负样本全被挤掉，
        // 于是精确率与误杀率都算不出来，报告只能给"证据不足"。
        let mut queue: Vec<(String, Label, u64)> = Vec::new();
        for o in dataset.positives.iter() {
            queue.push((o.host.clone(), Label::Positive, o.connections));
        }
        let half = args.limit / 2;
        for o in dataset.negatives.iter().take(half) {
            queue.push((o.host.clone(), Label::Negative, o.connections));
        }
        for o in dataset.unknown.iter() {
            queue.push((o.host.clone(), Label::Unknown, o.connections));
        }
        queue.truncate(args.limit);

        eprintln!("开始判定：{} 个候选（预算 {}）", queue.len(), args.limit);
        let mut samples: Vec<(Label, Judgement, u64)> = Vec::new();
        let mut totals = xt_intent::engine::ClassifyReport::default();
        for (host, label, connections) in &queue {
            // 限速：**在发请求之前**等，而不是撞到 429 之后靠退避重试
            // （重试会把同一个额度窗口浪费掉，还会让"无答案"的比例看起来像模型不行）。
            if args.sleep_ms > 0 {
                std::thread::sleep(Duration::from_millis(args.sleep_ms));
            }
            let rec = synthetic_record(host);
            engine.observe(&rec, 0);
            // 立刻跑一轮，让每条候选都有判决（节拍在这里只是形式）。
            //
            // **`asked == 0` 也要退出**：网关失败会把候选放回队列（生产里等下一个
            // 10 秒节拍再试），而这个循环是紧的 —— 不退的话会拿同一条候选反复撞预算，
            // 把额度烧光并刷出一堆 budget_denied（实测 12 条候选产生了 2676 次拒绝）。
            for _ in 0..64 {
                let r = engine.classify_pending(1);
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
            let judgement = match engine.explain_at(host, 1) {
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
