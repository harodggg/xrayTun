//! 探测路径的性能测量工具（重构用，不属于产品流程）。
//!
//! 用法：
//!   cargo run -p xraytun-desktop --example probe_bench -- <xray二进制> [节点json] [次数]
//!
//! 输出每个阶段的分段耗时，便于对「连接效率」类改动做前后对比。

use std::time::{Duration, Instant};

use xt_core::model::Node;
use xt_core::xray::probe::{probe_nodes, ProbeOptions};

fn load_nodes(path: &str) -> Vec<Node> {
    let raw = std::fs::read_to_string(path).expect("读取节点文件失败");
    serde_json::from_str(&raw).expect("解析节点文件失败")
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let binary = args.next().unwrap_or_else(|| {
        eprintln!("用法: probe_bench <xray二进制> [节点json] [次数] [并发]");
        std::process::exit(2)
    });
    let nodes_path = args
        .next()
        .unwrap_or_else(|| "/Users/xbtg-/deepseek-harness/.xraytun-data/nodes.json".to_string());
    let rounds: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(3);
    let concurrency: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(8);

    let nodes = load_nodes(&nodes_path);
    // 多节点批量：把同一节点复制成 N 份，模拟「订阅里有 N 个节点」的批量探测。
    // 端口/地址相同，因此每份的探测成本等价，用来观察批量模型的开销。
    let batch: usize = std::env::var("PROBE_BATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let mut probe_set: Vec<Node> = Vec::new();
    for i in 0..batch {
        let mut n = nodes[0].clone();
        n.id = format!("bench-{i}");
        n.name = format!("bench-{i}");
        probe_set.push(n);
    }

    let opts = ProbeOptions {
        binary: binary.clone().into(),
        base_port: 39_300,
        target_url: std::env::var("PROBE_TARGET")
            .unwrap_or_else(|_| "http://www.gstatic.com/generate_204".to_string()),
        timeout: Duration::from_secs(5),
        concurrency,
        ..Default::default()
    };

    println!(
        "binary={binary}\n节点数={} 轮次={rounds} 并发={concurrency} 目标={}",
        probe_set.len(),
        opts.target_url
    );

    let mut totals = Vec::new();
    for round in 1..=rounds {
        let t0 = Instant::now();
        let results = probe_nodes(&probe_set, &opts, None)
            .await
            .expect("probe_nodes 失败");
        let total = t0.elapsed();

        let ok = results.iter().filter(|r| r.available).count();
        let rtt: Vec<u32> = results.iter().filter_map(|r| r.server_rtt_ms).collect();
        let through: Vec<u32> = results.iter().filter_map(|r| r.through_node_ms).collect();
        println!(
            "第 {round} 轮: 总耗时={:>7.0?}  可用={ok}/{}  rtt中位={:?}  穿透中位={:?}",
            total,
            results.len(),
            median(&rtt),
            median(&through),
        );
        if let Some(err) = results.iter().find_map(|r| r.error.clone()) {
            println!("        首个错误: {err}");
        }
        totals.push(total.as_secs_f64() * 1000.0);
    }

    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "\n总耗时 中位={:.0}ms 最小={:.0}ms 最大={:.0}ms",
        median_f(&totals),
        totals.first().unwrap(),
        totals.last().unwrap()
    );
}

fn median(v: &[u32]) -> Option<u32> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    Some(s[s.len() / 2])
}

fn median_f(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v[v.len() / 2]
}
