//! 把分享链接转成节点 JSON（重构期的测试夹具）。
//!
//! 用途：让 probe_bench / probe_phases 这类测量工具能吃真实的 `vless://` 链接，
//! 而不必手写 Node 结构（手写容易漏 `tls`/`reality` 字段，测出来的是假失败）。
//!
//! ```bash
//! cargo run -q -p xt-core --example node_from_uri -- 'vless://...#名字' > /tmp/node.json
//! ```

use std::process::ExitCode;

use xt_core::subscription::parse_manual;

fn main() -> ExitCode {
    let Some(uri) = std::env::args().nth(1) else {
        eprintln!("用法: node_from_uri '<分享链接>'");
        return ExitCode::from(2);
    };
    match parse_manual(&uri) {
        Ok(outcome) => {
            if outcome.nodes.is_empty() {
                eprintln!("没有解析出节点");
                return ExitCode::FAILURE;
            }
            for (i, n) in outcome.nodes.iter().enumerate() {
                eprintln!(
                    "[{i}] {} {}:{} tls={:?} transport={:?}",
                    n.name, n.address, n.port, n.tls, n.transport
                );
            }
            match serde_json::to_string_pretty(&outcome.nodes) {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("序列化失败: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("解析失败: {e}");
            ExitCode::FAILURE
        }
    }
}
