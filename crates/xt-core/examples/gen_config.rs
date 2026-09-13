//! 从分享链接生成一份真实的 Xray 配置。
//!
//! 存在的理由：这个项目的一个核心取舍是「配置变更走重启核心」，好处是
//! **配置文件总是完整落盘的**，用户可以原样复现任何一个现场。
//! 这个 example 就是那个能力的命令行入口 —— 不需要启动 GUI、不需要 root，
//! 就能拿到一份和 App 内部完全同源的配置。
//!
//! ```bash
//! # 从分享链接生成（支持 vmess/vless/trojan/ss/socks/http）
//! cargo run -p xt-core --example gen_config -- \
//!     --node 'socks://127.0.0.1:7897#上游' --out /tmp/xray.json
//!
//! # 校验它
//! ./apps/desktop/binaries/xray run -test -c /tmp/xray.json
//!
//! # 或者直接跑起来，然后用 SOCKS5 访问
//! XRAY_LOCATION_ASSET=./apps/desktop/binaries \
//!   ./apps/desktop/binaries/xray run -c /tmp/xray.json
//! curl --socks5-hostname 127.0.0.1:10808 https://example.com
//! ```
//!
//! 也可以给一个订阅 URL 的**正文**（而不是链接）：`--node-file`。

use std::process::ExitCode;

use xt_core::model::{AppSettings, ProxyMode, RoutingPreset};
use xt_core::xray::{build_pretty, merge_rules, tun_inbound_spec, CoreConfigInput, InboundProfile};
use xt_core::subscription::parse_any;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
用法：
  gen_config --node <分享链接> [--mode system_proxy|tun|direct] [--out <文件>]
  gen_config --node-file <文件>  [--mode ...] [--out <文件>]

选项：
  --node <链接>     一条分享链接（vmess:// vless:// trojan:// ss:// socks:// http://）
  --node-file <f>   含多条链接或整份订阅正文的文件（自动嗅探格式）
  --mode <模式>     system_proxy（默认）| tun | direct
  --preset <预设>   bypass_mainland（默认）| global_proxy | whitelist_proxy | direct_all
  --socks <端口>    SOCKS 入站端口（默认 10808）
  --http <端口>     HTTP 入站端口（默认 10809）
  --interface <名>  TUN 模式下绑定出站的物理网卡（如 en0）
  --out <文件>      写到文件；省略则输出到 stdout
  --pretty          强制 pretty 输出（默认就已经是 pretty）";

struct Options {
    node: Option<String>,
    node_file: Option<String>,
    mode: ProxyMode,
    preset: RoutingPreset,
    socks_port: u16,
    http_port: u16,
    interface: Option<String>,
    out: Option<String>,
}

fn run(args: &[String]) -> Result<(), String> {
    let opts = parse_args(args)?;

    // ---- 解析节点 ----
    let body = match (&opts.node, &opts.node_file) {
        (Some(link), None) => link.clone(),
        (None, Some(path)) => {
            std::fs::read_to_string(path).map_err(|e| format!("读 {path} 失败：{e}"))?
        }
        (Some(_), Some(_)) => return Err("--node 与 --node-file 只能给一个".into()),
        (None, None) => return Err("必须给 --node 或 --node-file".into()),
    };

    let outcome = parse_any(&body).map_err(|e| format!("解析订阅失败：{e}"))?;
    if outcome.nodes.is_empty() {
        return Err("没有解析出任何节点".into());
    }

    eprintln!(
        "解析到 {} 个节点（格式 {}），跳过 {} 行",
        outcome.nodes.len(),
        outcome.format.as_str(),
        outcome.warnings.len()
    );
    for w in outcome.warnings.iter().take(10) {
        eprintln!("  ⚠ {w}");
    }

    // ---- 组装设置 ----
    let mut settings = AppSettings {
        mode: opts.mode,
        socks_port: opts.socks_port,
        http_port: opts.http_port,
        routing_preset: opts.preset,
        ..Default::default()
    };
    settings.selected_node = outcome.nodes.first().map(|n| n.id.clone());
    settings.tun.bind_outbound_to = opts.interface.clone();
    settings.validate().map_err(|e| format!("设置非法：{e}"))?;

    let profile = match opts.mode {
        ProxyMode::Tun => InboundProfile::Tun(tun_inbound_spec(&settings, opts.interface.as_deref(), false)),
        _ => InboundProfile::LocalProxy,
    };

    let rules = merge_rules(&settings);
    let config = build_pretty(&CoreConfigInput {
        settings: &settings,
        nodes: &outcome.nodes,
        selected: settings.selected_node.as_deref(),
        rules: &rules,
        profile,
        physical_interface: opts.interface.as_deref(),
    });

    // 自检一下生成的确实是合法 JSON（build_pretty 理论上不可能失败，
    // 但这条断言能挡住未来重构引入的低级错误）。
    serde_json::from_str::<serde_json::Value>(&config)
        .map_err(|e| format!("生成的配置不是合法 JSON（这是个 bug）：{e}"))?;

    match &opts.out {
        Some(path) => {
            std::fs::write(path, &config).map_err(|e| format!("写 {path} 失败：{e}"))?;
            eprintln!("已写入 {path}");
            eprintln!(
                "校验：xray run -test -c {path}\n启动：XRAY_LOCATION_ASSET=<含 geoip.dat 的目录> xray run -c {path}"
            );
        }
        None => println!("{config}"),
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        node: None,
        node_file: None,
        mode: ProxyMode::SystemProxy,
        preset: RoutingPreset::BypassMainland,
        socks_port: 10808,
        http_port: 10809,
        interface: None,
        out: None,
    };

    let mut i = 0;
    while i < args.len() {
        let key = args[i].as_str();
        // 取下一个参数。用闭包是为了让「缺参数」的错误信息统一。
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} 缺少参数"))
        };
        match key {
            "--node" => opts.node = Some(value("--node")?),
            "--node-file" => opts.node_file = Some(value("--node-file")?),
            "--out" => opts.out = Some(value("--out")?),
            "--interface" => opts.interface = Some(value("--interface")?),
            "--socks" => {
                opts.socks_port = value("--socks")?.parse().map_err(|_| "--socks 需要端口号")?
            }
            "--http" => {
                opts.http_port = value("--http")?.parse().map_err(|_| "--http 需要端口号")?
            }
            "--mode" => {
                opts.mode = match value("--mode")?.as_str() {
                    "system_proxy" => ProxyMode::SystemProxy,
                    "tun" => ProxyMode::Tun,
                    "direct" => ProxyMode::Direct,
                    other => return Err(format!("未知模式 {other}")),
                }
            }
            "--preset" => {
                opts.preset = match value("--preset")?.as_str() {
                    "bypass_mainland" => RoutingPreset::BypassMainland,
                    "global_proxy" => RoutingPreset::GlobalProxy,
                    "whitelist_proxy" => RoutingPreset::WhitelistProxy,
                    "direct_all" => RoutingPreset::DirectAll,
                    other => return Err(format!("未知预设 {other}")),
                }
            }
            "--pretty" => {}
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("未知选项 {other}")),
        }
        i += 1;
    }

    Ok(opts)
}
