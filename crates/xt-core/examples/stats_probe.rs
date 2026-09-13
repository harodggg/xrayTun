//! 对着一个**正在运行的** Xray 核心读一次流量计数器。
//!
//! 单元测试只能证明 protobuf 编解码是对的，证明不了「我们发的东西
//! 核心真的认」。这个示例补上那一段 —— 它连的是真实的 api 入站。
//!
//! ```bash
//! # 先在 App 里连上（TUN 或系统代理都行），然后：
//! cargo run -p xt-core --example stats_probe
//!
//! # 连续观察，看计数器是不是在涨：
//! cargo run -p xt-core --example stats_probe -- 5
//! ```

use std::time::Duration;

use xt_core::xray::stats::{query_stats, traffic_from_stats};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let rounds: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], xt_core::xray::config::API_PORT).into();
    println!("api 入站：{addr}");

    let mut last = None;
    for round in 0..rounds {
        match query_stats(addr, Duration::from_secs(5)).await {
            Ok(stats) => {
                let t = traffic_from_stats(&stats, "api");
                match last {
                    Some((prx, ptx)) => {
                        let drx = t.rx_bytes.saturating_sub(prx);
                        let dtx = t.tx_bytes.saturating_sub(ptx);
                        println!(
                            "第 {} 次：共 {} 个计数器  累计 ↓{} ↑{}  本次增量 ↓{} ↑{}",
                            round + 1,
                            stats.len(),
                            t.rx_bytes,
                            t.tx_bytes,
                            drx,
                            dtx
                        );
                    }
                    None => {
                        println!(
                            "第 {} 次：共 {} 个计数器  累计 ↓{} 字节 ↑{} 字节",
                            round + 1,
                            stats.len(),
                            t.rx_bytes,
                            t.tx_bytes
                        );
                        for s in &stats {
                            println!("    {:>12}  {}", s.value, s.name);
                        }
                    }
                }
                last = Some((t.rx_bytes, t.tx_bytes));
            }
            Err(e) => {
                eprintln!("✗ 查询失败：{e}");
                eprintln!("  （核心在跑吗？这个端口只在连接后监听）");
                return std::process::ExitCode::FAILURE;
            }
        }
        if round + 1 < rounds {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    std::process::ExitCode::SUCCESS
}
