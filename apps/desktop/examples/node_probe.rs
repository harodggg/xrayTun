//! 手动跑一次多节点延迟探测，把每个节点的结果原样打出来。
//!
//! 为什么单独有这个工具：批量探测是**一个探针核心 + N 个 SOCKS 入站**，
//! 任何一步出问题都表现为「所有节点一起失败」，而界面只给一句笼统的
//! 结论。这个工具跑的是 App 里**同一份** `supervisor::probe`，
//! 直接把每个节点的 `error` 字段和探针核心的 stderr 打出来。
//!
//! ```bash
//! cargo run -p xraytun-desktop --example node_probe            # 全部节点
//! cargo run -p xraytun-desktop --example node_probe -- --first 3
//! ```
//!
//! 只读：不改设置、不碰系统网络。

use std::process::ExitCode;
use std::time::Duration;

use xt_core::store::Store;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first: Option<usize> = args
        .iter()
        .position(|a| a == "--first")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());

    let store = Store::with_default_root();
    let all = store.load_nodes();
    let nodes: Vec<_> = match first {
        Some(n) => all.iter().take(n).cloned().collect(),
        None => all.clone(),
    };
    println!("数据目录: {}", store.root().display());
    println!("节点总数 {}，本次探测 {}\n", all.len(), nodes.len());
    if nodes.is_empty() {
        println!("没有节点可测");
        return ExitCode::SUCCESS;
    }

    let settings = store.load_settings();
    let binary = xt_core::xray::resolve_core_binary(
        settings.core_path.as_deref(),
        Some(&xt_core::update::managed_core_dir(store.root())),
        None,
        xraytun_desktop_lib::dev_binaries_dir().as_deref(),
    );
    let binary = match binary {
        Ok(b) => b,
        Err(e) => {
            println!("找不到核心：{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("核心: {}\n", binary.display());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("建 tokio runtime 失败");

    let started = std::time::Instant::now();
    let interface = xt_tun::macos::route::default_route()
        .ok()
        .map(|r| r.interface);
    println!("物理出口: {:?}（RTT 绑在它上面，绕开隧道）\n", interface);
    let results = runtime.block_on(xraytun_desktop_lib::supervisor::probe(
        &nodes,
        &binary,
        Duration::from_secs(5),
        interface.as_deref(),
    ));
    let results = match results {
        Ok(r) => r,
        Err(e) => {
            println!("探测整体失败：{e}");
            return ExitCode::FAILURE;
        }
    };

    println!("耗时 {:.1}s\n", started.elapsed().as_secs_f32());
    let mut ok = 0;
    for r in &results {
        let mark = if r.ok() { "✓" } else { "✗" };
        if r.ok() {
            ok += 1;
        }
        println!(
            "  {mark} {:<24} rtt {:>6?}  经节点 {:>7?}  http {:?}",
            r.node_name, r.server_rtt_ms, r.through_node_ms, r.http_status
        );
        if let Some(e) = &r.error {
            println!("      └─ {e}");
        }
    }
    println!("\n可用 {ok} / {}", results.len());
    if ok == 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
