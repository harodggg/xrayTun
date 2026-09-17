//! 探针启动开销的分阶段测量（重构用）。
//!
//! 回答一个问题：一次探测的耗时里，多少花在「起核心 + 等端口」这个固定开销上，
//! 多少花在真正的数据传输上。这个区分决定了优化该往哪儿投。
//!
//! 用法：
//!   cargo run -p xraytun-desktop --example probe_phases -- <xray二进制> [节点json] [批大小]

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use xt_core::model::Node;
use xt_core::xray::probe::build_probe_config;
use xt_core::xray::{wait_for_port, XrayProcess};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let binary: PathBuf = args
        .next()
        .unwrap_or_else(|| {
            eprintln!("用法: probe_phases <xray二进制> [节点json] [批大小]");
            std::process::exit(2)
        })
        .into();
    let nodes_path = args
        .next()
        .unwrap_or_else(|| "/Users/xbtg-/deepseek-harness/.scratch/local-probe-server/node.json".into());
    let batch: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let rounds: usize = std::env::var("PHASE_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let base: Vec<Node> = serde_json::from_str(&std::fs::read_to_string(&nodes_path).unwrap()).unwrap();
    let mut nodes = Vec::new();
    for i in 0..batch {
        let mut n = base[0].clone();
        n.id = format!("phase-{i}");
        nodes.push(n);
    }

    const BASE_PORT: u16 = 39_500;
    let mut spawn_times = Vec::new();
    let mut ready_times = Vec::new();

    for round in 1..=rounds {
        // ---- 阶段 1：生成配置 ----
        let t = Instant::now();
        let config = build_probe_config(&nodes, BASE_PORT).expect("生成探针配置失败");
        let gen = t.elapsed();

        // ---- 阶段 2：落盘 ----
        let t = Instant::now();
        let path = std::env::temp_dir().join(format!(
            "xt-phase-{}-{}.json",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
        let write = t.elapsed();

        // ---- 阶段 3：起核心 ----
        let t = Instant::now();
        let proc = XrayProcess::spawn(&binary, &path, None).await.expect("起核心失败");
        let spawn = t.elapsed();

        // ---- 阶段 4：等端口就绪 ----
        let t = Instant::now();
        let deadline = Duration::from_secs(7);
        let mut set = tokio::task::JoinSet::new();
        for i in 0..nodes.len() {
            let port = BASE_PORT + i as u16;
            set.spawn(async move { wait_for_port(port, deadline).await.is_ok() });
        }
        let mut ready = 0;
        while let Some(r) = set.join_next().await {
            if matches!(r, Ok(true)) {
                ready += 1;
            }
        }
        let wait = t.elapsed();

        let _ = proc.shutdown(Duration::from_secs(2)).await;
        let _ = std::fs::remove_file(&path);

        println!(
            "第 {round} 轮 (节点={batch}): 生成配置={:>6.0?} 落盘={:>6.0?} 起核心={:>7.0?} 等端口={:>7.0?} | 固定开销合计={:>7.0?} 就绪={ready}/{}",
            gen, write, spawn, wait, gen + write + spawn + wait, nodes.len()
        );
        spawn_times.push(spawn.as_secs_f64() * 1000.0);
        ready_times.push(wait.as_secs_f64() * 1000.0);
    }

    spawn_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ready_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "\n起核心中位={:.0}ms  等端口中位={:.0}ms  固定开销中位合计={:.0}ms",
        spawn_times[spawn_times.len() / 2],
        ready_times[ready_times.len() / 2],
        spawn_times[spawn_times.len() / 2] + ready_times[ready_times.len() / 2]
    );
}
