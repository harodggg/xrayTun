//! xt-cli —— 无头客户端：用与 UI 完全相同的契约驱动 daemon（端到端入口）
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 存在的理由：证明「UI 能做的，无头也能做」。它用的是同一个
//! [`xt_ipc::Client`] 与同一份 `xt-contract`，没有自己的协议分支；
//! 因此 CI 里不需要 WebView 也能端到端验证（契约 §1）。
//!
//! ## 没有等待（I1）
//!
//! `connect` / `disconnect` / `switch` 只发意图，终态**等事件**。
//! `--timeout-ms` 是**失败上限**，不是轮询间隔：超时产生一个具体的 `ErrorBody`
//! 并以非 0 退出，绝不「睡一会儿再看看」。事件流出现跳帧（Lagged）同样如实报错，
//! 因为跳帧后我们无法证明终态是否已经过去。

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::sync::broadcast;
use xt_contract::error::{bad_request, internal, ErrorBody, ErrorCode};
use xt_contract::model::{
    Capability, ConnectionView, LogLevel, NodeId, NoticeSeverity, ProbeResult, RunMode,
    SettingsPatch, SettingsView, Stage,
};
use xt_contract::protocol::{Event, Request, Response};
use xt_ipc::Client;

/// 客户端上报给 daemon 的版本，取自 Cargo 包版本（不是手写常量）。
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Parser, Debug)]
#[command(
    name = "xt-cli",
    version,
    about = "xraytun-next 无头客户端：用与 UI 相同的契约驱动 daemon",
    long_about = "本轮支持本地订阅原文解析（`subscriptions`）；远端拉取（`add-subscription` / `refresh-subscription`）属于 subscription_fetch 能力，本轮未宣告，会以非 0 退出。"
)]
pub struct Cli {
    /// daemon 的 AF_UNIX socket 路径。
    ///
    /// 声明成 `Option` 而不是必填：clap 不允许「全局 + 必填」组合，
    /// 而 `--socket` 必须在子命令前后都能写。缺了就在 `run` 里按用法错误（退出码 2）处理。
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,

    /// 输出契约的原始 JSON（供脚本消费），而不是给人看的行。
    #[arg(long, global = true)]
    pub json: bool,

    /// 等待终态事件的失败上限（毫秒）。超时按真实错误上报，不是重试间隔。
    #[arg(long, global = true, default_value_t = DEFAULT_TIMEOUT_MS)]
    pub timeout_ms: u64,

    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// 显示 daemon 的真实身份（版本 / pid / 协议版本 / 能力）。
    Hello,
    /// 当前连接状态快照。
    Status,
    /// 节点目录。
    Nodes,
    /// 连接指定节点（终态以事件到达）。
    Connect {
        /// 节点 id（`xt-cli nodes` 里的 id）。
        node: String,
    },
    /// 断开（已经断开时如实说明，不假报「已断开」）。
    Disconnect,
    /// 切换到另一个节点：切过去失败就停在失败，不自动换下一个。
    Switch {
        node: String,
    },
    /// 探测给定节点的真实 TTFB。
    Probe {
        ids: Vec<String>,
    },
    /// 持续打印事件，直到 Ctrl-C 或 daemon 断开。
    Events,
    /// 取最近 N 行日志。
    TailLogs {
        lines: u32,
    },
    /// 读取设置。
    GetSettings,
    /// 局部修改设置（只改给出的字段）。
    PatchSettings {
        #[arg(long)]
        socks_listen: Option<String>,
        #[arg(long)]
        selected_node: Option<String>,
        #[arg(long)]
        log_level: Option<String>,
    },
    /// 本地订阅目录（只读展示；远端拉取不在本轮）。
    Subscriptions,
    /// 本轮不支持：新增订阅属于远端拉取语义（`subscription_fetch` 未宣告）。
    AddSubscription {
        url: String,
    },
    /// 本轮不支持：刷新订阅属于远端拉取语义（`subscription_fetch` 未宣告）。
    RefreshSubscription {
        id: String,
    },
}

/// 解析参数并以进程退出码返回：0 = 成功，1 = daemon/IO 报错，2 = 用法或能力不支持。
pub async fn run(cli: Cli) -> i32 {
    // `--socket` 缺失属用法错误：不是「连不上」，也不该被当成 io 错报。
    // 只有纯本地拒绝的 `add-subscription` / `refresh-subscription` 不需要 socket。
    let needs_socket = !matches!(
        cli.command,
        CliCommand::AddSubscription { .. } | CliCommand::RefreshSubscription { .. }
    );
    if needs_socket && cli.socket.is_none() {
        eprintln!("必须指定 --socket <path>（用 --help 看用法）");
        return 2;
    }
    match execute(&cli).await {
        Ok(code) => code,
        Err(error) => {
            report_error(&error, cli.json);
            1
        }
    }
}

fn report_error(error: &ErrorBody, json: bool) {
    if json {
        match serde_json::to_string_pretty(error) {
            Ok(text) => eprintln!("{text}"),
            Err(err) => eprintln!("{{\"code\":\"internal\",\"message\":\"错误序列化失败: {err}\"}}"),
        }
    } else {
        eprintln!("错误 [{}] {}", error.code, error.message);
        if let Some(detail) = &error.detail {
            eprintln!("  详情: {detail}");
        }
    }
}

async fn execute(cli: &Cli) -> Result<i32, ErrorBody> {
    match &cli.command {
        // `Subscriptions`（本地原文解析）本轮**已实现**，所以这里真发请求：
        // CLI 是「无头等价入口」，它比 UI 少一个能力就等于把等价性破坏了。
        CliCommand::Subscriptions => {
            let client = connect(cli).await?;
            let subscriptions = match client.request(Request::ListSubscriptions).await? {
                Response::Subscriptions { subscriptions } => subscriptions,
                other => return Err(unexpected("subscriptions", &other)),
            };
            if cli.json {
                print_json(&subscriptions)?;
            } else if subscriptions.is_empty() {
                println!("（没有订阅；本轮只认本地订阅原文文件，未拉取过任何远端）");
            } else {
                for subscription in subscriptions {
                    // fetched_at_ms 为 None 就是「从未拉取」：不打印 0 伪装成时间。
                    let fetched = subscription
                        .fetched_at_ms
                        .map_or_else(|| "未拉取".to_string(), |ms| ms.to_string());
                    print!("{}\t{}\t节点数={}\tfetched_at_ms={}", subscription.id, subscription.url, subscription.node_count, fetched);
                    if let Some(error) = &subscription.last_error {
                        print!("\tlast_error=[{}] {}", error.code, error.message);
                    }
                    println!();
                }
            }
            client.close().await;
            Ok(0)
        }
        // 拉取语义（`SubscriptionFetch`）未宣告 → 子命令必须失败，
        // 而不是留一个看起来能用的入口（契约 §3.1）。
        CliCommand::AddSubscription { .. } | CliCommand::RefreshSubscription { .. } => {
            eprintln!(
                "本版本不提供订阅远端拉取（capability subscription_fetch 未宣告）；\
                 请用 daemon 的 --subscription-file 提供本地订阅原文。"
            );
            Ok(2)
        }
        CliCommand::Hello => {
            let client = connect(cli).await?;
            let hello = client.hello().clone();
            if cli.json {
                print_json(&hello)?;
            } else {
                println!("daemon_version: {}", hello.daemon_version);
                println!("protocol_version: {}", hello.protocol_version);
                println!("pid: {}", hello.pid);
                println!("started_at_ms: {}", hello.started_at_ms);
                let capabilities: Vec<&str> = hello.capabilities.iter().map(|c| capability_str(*c)).collect();
                println!("capabilities: {}", capabilities.join(", "));
            }
            client.close().await;
            Ok(0)
        }
        CliCommand::Status => {
            let client = connect(cli).await?;
            let view = match client.request(Request::Status).await? {
                Response::Status(view) => view,
                other => return Err(unexpected("status", &other)),
            };
            print_view(&view, cli.json)?;
            client.close().await;
            Ok(0)
        }
        CliCommand::Nodes => {
            let client = connect(cli).await?;
            let nodes = match client.request(Request::ListNodes).await? {
                Response::Nodes { nodes } => nodes,
                other => return Err(unexpected("nodes", &other)),
            };
            if cli.json {
                print_json(&nodes)?;
            } else if nodes.is_empty() {
                println!("（目录为空：没有订阅文件或手工节点）");
            } else {
                for node in nodes {
                    println!("{}\t{}\t{}\t{}", node.id, node.protocol, node.endpoint, node.name);
                }
            }
            client.close().await;
            Ok(0)
        }
        CliCommand::Connect { node } => {
            let client = connect(cli).await?;
            let node_id = NodeId::new(node.clone());
            // 先建事件订阅再发意图：否则意图被受理后、订阅建立前的事件会丢。
            let events = client.events();
            match client.request(Request::Connect { node_id: node_id.clone(), mode: RunMode::Proxy }).await? {
                Response::Accepted => {}
                other => return Err(unexpected("connect", &other)),
            }
            let view = wait_for_state(events, cli.timeout_ms, Terminal::Connected { node: &node_id }, "connect").await?;
            print_view(&view, cli.json)?;
            client.close().await;
            Ok(0)
        }
        CliCommand::Disconnect => {
            let client = connect(cli).await?;
            // 先如实看一眼现在是什么状态：已经断开就不发意图，也不假装「断开成功」。
            let current = match client.request(Request::Status).await? {
                Response::Status(view) => view,
                other => return Err(unexpected("status", &other)),
            };
            if current.stage == Stage::Disconnected {
                println!("当前已经是断开状态（未发送 disconnect 意图）");
                client.close().await;
                return Ok(0);
            }
            let events = client.events();
            match client.request(Request::Disconnect).await? {
                Response::Accepted => {}
                other => return Err(unexpected("disconnect", &other)),
            }
            let view = wait_for_state(events, cli.timeout_ms, Terminal::Disconnected, "disconnect").await?;
            print_view(&view, cli.json)?;
            client.close().await;
            Ok(0)
        }
        CliCommand::Switch { node } => {
            let client = connect(cli).await?;
            let node_id = NodeId::new(node.clone());
            let events = client.events();
            match client.request(Request::SwitchNode { node_id: node_id.clone() }).await? {
                Response::Accepted => {}
                other => return Err(unexpected("switch", &other)),
            }
            let view = wait_for_state(events, cli.timeout_ms, Terminal::Connected { node: &node_id }, "switch").await?;
            print_view(&view, cli.json)?;
            client.close().await;
            Ok(0)
        }
        CliCommand::Probe { ids } => {
            if ids.is_empty() {
                return Err(bad_request("probe 至少需要一个节点 id"));
            }
            let client = connect(cli).await?;
            let node_ids: Vec<NodeId> = ids.iter().map(NodeId::new).collect();
            let events = client.events();
            match client.request(Request::ProbeNodes { node_ids: node_ids.clone() }).await? {
                Response::Accepted => {}
                other => return Err(unexpected("probe", &other)),
            }
            let results = wait_for_probes(events, cli.timeout_ms, node_ids.len()).await?;
            if cli.json {
                print_json(&results)?;
            } else {
                for result in &results {
                    match (&result.ttfb_ms, &result.error) {
                        (Some(ttfb), _) => println!("{}\t{} ms", result.node_id, ttfb),
                        (None, Some(error)) => println!("{}\t失败 [{}] {}", result.node_id, error.code, error.message),
                        // 契约保证「恰好一个字段为 Some」；两个都没有是契约被破坏，如实说。
                        (None, None) => println!("{}\t无结果（探测结果不自洽）", result.node_id),
                    }
                }
            }
            client.close().await;
            Ok(0)
        }
        CliCommand::Events => {
            let client = connect(cli).await?;
            let mut events = client.events();
            loop {
                tokio::select! {
                    received = events.recv() => match received {
                        Ok(event) => print_event(&event, cli.json)?,
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(ErrorBody::new(ErrorCode::Io, "事件流已关闭：daemon 断开了连接"));
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Err(internal(format!(
                                "事件流积压，跳过了 {skipped} 条事件：终态是否已经出现无法证明"
                            )));
                        }
                    },
                    signal = tokio::signal::ctrl_c() => {
                        signal.map_err(|err| ErrorBody::new(ErrorCode::Io, format!("监听 Ctrl-C 失败: {err}")))?;
                        eprintln!("收到 Ctrl-C，停止打印事件");
                        return Ok(0);
                    }
                }
            }
        }
        CliCommand::TailLogs { lines } => {
            let client = connect(cli).await?;
            let logs = match client.request(Request::TailLogs { lines: *lines }).await? {
                Response::Logs { logs } => logs,
                other => return Err(unexpected("tail-logs", &other)),
            };
            if cli.json {
                print_json(&logs)?;
            } else {
                for line in logs {
                    println!("{} [{}] {} {}", line.ts_ms, line.level.as_str(), line.target, line.message);
                }
            }
            client.close().await;
            Ok(0)
        }
        CliCommand::GetSettings => {
            let client = connect(cli).await?;
            let settings = match client.request(Request::GetSettings).await? {
                Response::Settings(settings) => settings,
                other => return Err(unexpected("get-settings", &other)),
            };
            print_settings(&settings, cli.json)?;
            client.close().await;
            Ok(0)
        }
        CliCommand::PatchSettings { socks_listen, selected_node, log_level } => {
            if socks_listen.is_none() && selected_node.is_none() && log_level.is_none() {
                return Err(bad_request("patch-settings 至少要给一个要改的字段"));
            }
            let patch = SettingsPatch {
                socks_listen: socks_listen.clone(),
                selected_node: selected_node.clone().map(NodeId::new),
                log_level: match log_level {
                    Some(text) => Some(parse_log_level(text)?),
                    None => None,
                },
            };
            let client = connect(cli).await?;
            match client.request(Request::PatchSettings { patch }).await? {
                // daemon 可以回 Ok（没有要展示的新状态）或 Settings（回传改完的设置）。
                Response::Ok => println!("设置已更新"),
                Response::Settings(settings) => print_settings(&settings, cli.json)?,
                other => return Err(unexpected("patch-settings", &other)),
            }
            client.close().await;
            Ok(0)
        }
    }
}

async fn connect(cli: &Cli) -> Result<Client, ErrorBody> {
    let socket = cli.socket.as_ref().ok_or_else(|| bad_request("必须指定 --socket <path>"))?;
    Client::connect(socket, CLIENT_VERSION).await
}

/// 终态判据。只有这两个状态算「一次意图有了结果」，其余一律继续等。
#[derive(Clone, Copy)]
enum Terminal<'a> {
    /// 连接（或切换）成功到指定节点。
    Connected { node: &'a NodeId },
    /// 已断开。
    Disconnected,
}

/// 等终态事件。`timeout` 只做失败上限：超时说明 daemon 既没报成功也没报失败，
/// 这是需要如实告诉用户的事实，不能靠重试或再等一会儿掩盖。
async fn wait_for_state(
    mut events: broadcast::Receiver<Event>,
    timeout_ms: u64,
    terminal: Terminal<'_>,
    what: &str,
) -> Result<ConnectionView, ErrorBody> {
    let waiting = async {
        loop {
            match events.recv().await {
                Ok(Event::State { view }) => match (terminal, view.stage) {
                    (Terminal::Connected { node }, Stage::Connected) if view.node_id.as_ref() == Some(node) => {
                        return Ok(view);
                    }
                    (Terminal::Connected { .. }, Stage::Disconnected) if view.last_error.is_some() => {
                        return Err(view.last_error.clone().unwrap_or_else(|| {
                            internal("状态为 Disconnected 且 last_error 为空，契约被破坏")
                        }));
                    }
                    (Terminal::Disconnected, Stage::Disconnected) => return Ok(view),
                    _ => continue,
                },
                Ok(Event::Notice { notice }) if notice.severity == NoticeSeverity::Error => {
                    return Err(ErrorBody::new(
                        notice.code,
                        format!("daemon 报告失败（notice）: {}", notice.message),
                    ));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ErrorBody::new(ErrorCode::Io, format!("{what} 等待中事件流关闭：daemon 断开")));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Err(internal(format!("{what} 等待中事件流积压，跳过 {skipped} 条，终态无法证明")));
                }
            }
        }
    };

    tokio::time::timeout(Duration::from_millis(timeout_ms), waiting)
        .await
        .map_err(|_| {
            ErrorBody::new(
                ErrorCode::Io,
                format!("等待 {what} 终态超时（上限 {timeout_ms}ms）：daemon 既未报告成功，也未报告失败"),
            )
        })?
}

/// 等 `expected` 条 Probe 事件。少一条就是少一条，不拿「没测到」当成功。
async fn wait_for_probes(
    mut events: broadcast::Receiver<Event>,
    timeout_ms: u64,
    expected: usize,
) -> Result<Vec<ProbeResult>, ErrorBody> {
    let waiting = async {
        let mut results = Vec::with_capacity(expected);
        while results.len() < expected {
            match events.recv().await {
                Ok(Event::Probe { result }) => results.push(result),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ErrorBody::new(
                        ErrorCode::Io,
                        format!("探测等待中事件流关闭：已收到 {}/{} 条结果", results.len(), expected),
                    ));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Err(internal(format!("探测等待中事件流积压，跳过 {skipped} 条")));
                }
            }
        }
        Ok(results)
    };

    tokio::time::timeout(Duration::from_millis(timeout_ms), waiting)
        .await
        .map_err(|_| {
            ErrorBody::new(
                ErrorCode::Io,
                format!("等待探测结果超时（上限 {timeout_ms}ms），期望 {expected} 条"),
            )
        })?
}

fn parse_log_level(text: &str) -> Result<LogLevel, ErrorBody> {
    match text.to_ascii_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        other => Err(bad_request(format!("未知日志级别: {other}（error/warn/info/debug）"))),
    }
}

fn unexpected(what: &str, response: &Response) -> ErrorBody {
    internal(format!("{what} 收到了非预期的应答: {response:?}"))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), ErrorBody> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|err| internal(format!("序列化输出失败: {err}")))?;
    println!("{text}");
    Ok(())
}

fn print_view(view: &ConnectionView, json: bool) -> Result<(), ErrorBody> {
    if json {
        return print_json(view);
    }
    println!("stage: {}", stage_str(view.stage));
    if let Some(phase) = view.phase {
        println!("phase: {}", phase.as_str());
    }
    if let Some(node) = &view.node_id {
        println!("node_id: {node}");
    }
    if let Some(since) = view.connected_since_ms {
        println!("connected_since_ms: {since}");
    }
    println!("pid: {}", view.datapath.pid.map_or_else(|| "未运行".into(), |p| p.to_string()));
    // 未采样就是未采样：不打印 0，那会让人以为「没有流量」。
    match &view.stats {
        Some(stats) => println!("stats: up={} down={} @{}", stats.uplink_bytes, stats.downlink_bytes, stats.sampled_at_ms),
        None => println!("stats: 未采样"),
    }
    if let Some(error) = &view.last_error {
        println!("last_error: [{}] {}", error.code, error.message);
    }
    Ok(())
}

fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::Disconnected => "disconnected",
        Stage::Connecting => "connecting",
        Stage::Connected => "connected",
        Stage::Disconnecting => "disconnecting",
    }
}

/// 能力的线上字符串。手写 match 而不是 `Debug` 小写化的理由：
/// 契约新增能力时会**编译失败**，逼我们决定这一栏打印什么；
/// 而 `Debug` 会把 `SubscriptionFetch` 打成 `subscriptionfetch`，
/// 与线上的 `subscription_fetch` 不一致，读起来像另一个能力。
fn capability_str(capability: Capability) -> &'static str {
    match capability {
        Capability::ProxyMode => "proxy_mode",
        Capability::TunMode => "tun_mode",
        Capability::Stats => "stats",
        Capability::Probe => "probe",
        Capability::Subscriptions => "subscriptions",
        Capability::SubscriptionFetch => "subscription_fetch",
    }
}

fn print_settings(settings: &SettingsView, json: bool) -> Result<(), ErrorBody> {
    if json {
        return print_json(settings);
    }
    println!("socks_listen: {}", settings.socks_listen);
    println!(
        "selected_node: {}",
        settings.selected_node.as_ref().map_or_else(|| "（未选择）".into(), |id| id.to_string())
    );
    println!("log_level: {}", settings.log_level.as_str());
    Ok(())
}

fn print_event(event: &Event, json: bool) -> Result<(), ErrorBody> {
    if json {
        return print_json(event);
    }
    match event {
        Event::State { view } => {
            print!("state: {}", stage_str(view.stage));
            if let Some(phase) = view.phase {
                print!(" ({})", phase.as_str());
            }
            if let Some(node) = &view.node_id {
                print!(" node={node}");
            }
            println!();
        }
        Event::Log { line } => println!("log [{}] {} {}", line.level.as_str(), line.target, line.message),
        Event::Probe { result } => match (&result.ttfb_ms, &result.error) {
            (Some(ttfb), _) => println!("probe {} = {ttfb}ms", result.node_id),
            (None, Some(error)) => println!("probe {} 失败 [{}] {}", result.node_id, error.code, error.message),
            (None, None) => println!("probe {} 无结果（结果不自洽）", result.node_id),
        },
        Event::Notice { notice } => println!("notice [{:?}] [{}] {}", notice.severity, notice.code, notice.message),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("参数应合法")
    }

    #[test]
    fn every_documented_subcommand_parses() {
        // 命令面就是验收单，缺一个都会在这里红。
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "hello"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "status"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "nodes"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "connect", "node-1"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "disconnect"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "switch", "node-2"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "probe", "a", "b"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "events"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "tail-logs", "50"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "get-settings"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "patch-settings", "--log-level", "debug"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "subscriptions"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "add-subscription", "https://example.com/sub"]);
        parse(&["xt-cli", "--socket", "/tmp/x.sock", "refresh-subscription", "sub-1"]);
        let cli = parse(&["xt-cli", "--socket", "/tmp/x.sock", "--json", "status"]);
        assert!(cli.json);
        assert_eq!(cli.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn missing_socket_is_a_usage_error() {
        let cli = parse(&["xt-cli", "status"]);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let code = runtime.block_on(run(cli));
        assert_eq!(code, 2);
    }

    #[test]
    fn log_level_accepts_the_wire_spelling_and_the_xray_spelling() {
        assert_eq!(parse_log_level("warn").expect("warn"), LogLevel::Warn);
        assert_eq!(parse_log_level("warning").expect("warning"), LogLevel::Warn);
        assert!(parse_log_level("loud").is_err());
    }

    /// 拉取语义的子命令必须失败（`subscription_fetch` 未宣告），而不是看起来能用。
    #[tokio::test]
    async fn fetch_semantics_commands_exit_non_zero() {
        let add = parse(&["xt-cli", "add-subscription", "https://example.com/sub"]);
        assert_eq!(run(add).await, 2);
        let refresh = parse(&["xt-cli", "refresh-subscription", "sub-1"]);
        assert_eq!(run(refresh).await, 2);
    }

    /// `subscriptions` 是真实现：没有 daemon 时它应报 io 失败（exit 1），
    /// 而不是像「不支持」那样固定 exit 2。
    #[tokio::test]
    async fn subscriptions_needs_a_daemon_and_reports_io() {
        let dir = std::env::temp_dir().join(format!("xt-cli-sub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let cli = parse(&["xt-cli", "--socket", dir.join("nope.sock").to_str().expect("路径"), "subscriptions"]);
        assert_eq!(run(cli).await, 1);
    }

    #[test]
    fn capability_names_match_the_wire_spelling() {
        assert_eq!(capability_str(Capability::SubscriptionFetch), "subscription_fetch");
        assert_eq!(capability_str(Capability::Subscriptions), "subscriptions");
        assert_eq!(capability_str(Capability::ProxyMode), "proxy_mode");
    }

    /// 连不上 socket 要如实报 io 错并以非 0 退出（不做重试、不假装成功）。
    #[tokio::test]
    async fn missing_socket_is_an_io_error() {
        let dir = std::env::temp_dir().join(format!("xt-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let cli = parse(&["xt-cli", "--socket", dir.join("nope.sock").to_str().expect("路径"), "status"]);
        let code = run(cli).await;
        assert_eq!(code, 1);
    }

    #[test]
    fn patch_settings_without_any_field_is_a_usage_error() {
        let cli = parse(&["xt-cli", "--socket", "/tmp/x.sock", "patch-settings"]);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let code = runtime.block_on(run(cli));
        assert_eq!(code, 1);
    }
}
