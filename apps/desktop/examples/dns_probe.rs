//! 手动跑一次 DNS 解析器探测，把两组结果都打出来。
//!
//! 单元测试证明不了「经节点那组真的测得通」—— 那需要一个真在跑的节点。
//! 这个例子跑的是 App 里**同一份** `probe_pool`，只是它只读不写：
//! 不碰设置、不改系统网络。
//!
//! ```bash
//! # 节点已连接（默认 SOCKS 10808），两组都应可用
//! cargo run -p xraytun-desktop --example dns_probe
//!
//! # 模拟「节点没连」：国外那组应显示「未探测」（这是通过，不是失败）
//! cargo run -p xraytun-desktop --example dns_probe -- --no-socks
//!
//! # 显式指定
//! cargo run -p xraytun-desktop --example dns_probe -- --socks 10808 --interface en0 --samples 3
//! ```
//!
//! 退出码：国内组没有可用解析器 → 1；传了 `--socks` 但国外组全不可用 → 1；
//! 没传 `--socks` 而国外组**没有**被标成「未探测」→ 1。
//! 「节点没连、国外组未探测」本身不算失败。

use std::process::ExitCode;
use std::time::Duration;

use xt_core::dns_probe::{probe_pool, DnsKind, DnsTransport, ProbeSpec, DNS_POOL};

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 默认假设节点在跑 —— 那是手动验证这个功能时最常见的情形。
    let socks = if args.iter().any(|a| a == "--no-socks") {
        None
    } else {
        let port = arg_value(&args, "--socks").unwrap_or_else(|| "10808".into());
        Some(format!("127.0.0.1:{port}"))
    };

    let interface = match arg_value(&args, "--interface") {
        Some(name) => Some(name),
        None => xt_tun::macos::route::default_route().ok().map(|r| r.interface),
    };

    let samples: usize = arg_value(&args, "--samples")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let timeout = Duration::from_secs(
        arg_value(&args, "--timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2),
    );

    let spec = ProbeSpec {
        timeout,
        samples,
        interface: interface.clone(),
        socks: socks.clone(),
    };

    // 先把「这次测的是什么条件」打出来。没有条件的数字没有意义。
    println!("DNS 解析器探测 —— 两组，两条路径");
    println!(
        "  国内组  明文 UDP 直连   绑网卡 {}   （用于 geosite:cn）",
        interface.as_deref().unwrap_or("<自动探测失败>")
    );
    println!(
        "  国外组  DoH 经节点      {}   （用于 geosite:geolocation-!cn）",
        socks.as_deref().unwrap_or("<无：应按未探测处理>")
    );
    println!("  超时 {}s，每台取样 {} 次取中位\n", timeout.as_secs(), samples);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("建 tokio runtime 失败");

    let probes = runtime.block_on(probe_pool(DNS_POOL, &spec, 4));

    for kind in [DnsKind::Domestic, DnsKind::Foreign] {
        let rows: Vec<_> = probes.iter().filter(|p| p.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        let (title, path) = match kind {
            DnsKind::Domestic => ("国内组 · 直连测量", "明文 UDP"),
            DnsKind::Foreign => ("国外组 · 经节点测量", "DoH over SOCKS"),
        };
        println!("── {title}（{path}）");
        for p in &rows {
            let latency = match p.latency_ms {
                Some(ms) => format!("{ms:>5} ms"),
                None => "      —".to_string(),
            };
            let mark = if p.usable() {
                "✓"
            } else if p.latency_ms.is_some() {
                "!"
            } else {
                "✗"
            };
            let mut extra = String::new();
            if p.suspect {
                extra.push_str("  与同组多数派不一致");
            }
            if let Some(note) = &p.note {
                extra.push_str(&format!("  {note}"));
            }
            if p.latency_ms.is_some() && !p.answered {
                extra.push_str("  没有 A 记录");
            }
            println!("  {mark} {:<34} {:<16} {latency}{extra}", p.server, p.label);
        }
        let usable = rows.iter().filter(|p| p.usable()).count();
        println!("  → 可用 {usable} / {}\n", rows.len());
    }

    // 断言式收尾：让它在 CI 之外也能当冒烟测试用。
    let domestic_usable = probes
        .iter()
        .filter(|p| p.kind == DnsKind::Domestic && p.usable())
        .count();
    let foreign_usable = probes
        .iter()
        .filter(|p| p.kind == DnsKind::Foreign && p.usable())
        .count();
    let foreign_skipped = probes
        .iter()
        .any(|p| p.kind == DnsKind::Foreign && p.note.is_some());

    let mut failed = false;
    if domestic_usable == 0 {
        println!("✗ 国内组没有任何可用解析器");
        failed = true;
    }
    match &socks {
        Some(s) if foreign_usable == 0 => {
            println!("✗ 给了 --socks {s}，但国外组没有任何可用解析器");
            failed = true;
        }
        None if !foreign_skipped => {
            println!("✗ 没给 --socks，国外组本应被标成「未探测」");
            failed = true;
        }
        _ => {}
    }
    // 国外候选必须真的走 DoH —— 写反了这组的数字就没有意义。
    if probes
        .iter()
        .any(|p| p.kind == DnsKind::Foreign && p.transport != DnsTransport::Doh)
    {
        println!("✗ 国外候选不是走 DoH");
        failed = true;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        println!("✓ 两组都符合预期");
        ExitCode::SUCCESS
    }
}
