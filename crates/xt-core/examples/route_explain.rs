//! 对**运行中的真实配置**做路由判定，供界面查询，也供与真实 Xray 对拍。
//!
//! ```bash
//! cargo run -q -p xt-core --example route_explain -- <config.json> <数据目录> -- <域名或IP...>
//! ```
//!
//! 输出刻意做成一行一条，便于和 Xray 的 access.log 逐条比对。

use std::process::ExitCode;

use xt_core::routing::explain::{explain, DestQuery, Rule};
use xt_core::routing::geo::GeoData;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(split) = args.iter().position(|a| a == "--") else {
        eprintln!("用法: route_explain <config.json> <数据目录> -- <域名或IP...>");
        return ExitCode::from(2);
    };
    let (head, rest) = args.split_at(split);
    let queries = &rest[1..];
    if head.len() < 2 {
        eprintln!("需要 config.json 与数据目录两个参数");
        return ExitCode::from(2);
    }
    let (cfg_path, data_dir) = (&head[0], &head[1]);

    let raw = match std::fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("读配置失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("解析配置失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rules: Vec<Rule> = match serde_json::from_value(
        cfg.get("routing")
            .and_then(|r| r.get("rules"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("解析规则失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    // 只加载规则里真正引用的类别
    let mut sites = Vec::new();
    let mut ips = Vec::new();
    for r in &rules {
        for d in &r.conds.domain {
            if let Some(c) = d.strip_prefix("geosite:") {
                sites.push(c.to_string());
            }
        }
        for i in &r.conds.ip {
            if let Some(c) = i.strip_prefix("geoip:") {
                ips.push(c.to_string());
            }
        }
    }
    let geo = match GeoData::load(std::path::Path::new(data_dir), &sites, &ips) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("加载 geo 数据失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    for q in queries {
        let query = if let Ok(ip) = q.parse::<std::net::IpAddr>() {
            DestQuery {
                ip: Some(ip),
                port: 443,
                network: "tcp".into(),
                ..Default::default()
            }
        } else {
            DestQuery {
                host: Some(q.clone()),
                port: 443,
                network: "tcp".into(),
                ..Default::default()
            }
        };
        let out = explain(&rules, &geo, &query);
        println!(
            "{q:34} -> 规则 {:<24} 出站 {}  依据: {}",
            out.rule_tag.clone().unwrap_or_else(|| "(未命中)".into()),
            if out.outbound.is_empty() { "(默认第一条出站)" } else { &out.outbound },
            out.reasons.join("；")
        );
        if !out.undecidable.is_empty() {
            println!("{:34}    无法判定: {}", "", out.undecidable.join("；"));
        }
    }
    ExitCode::SUCCESS
}
