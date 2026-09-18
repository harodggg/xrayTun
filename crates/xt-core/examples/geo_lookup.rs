//! 用真实的 `geosite.dat` / `geoip.dat` 验证解析与匹配。
//!
//! ```bash
//! cargo run -q -p xt-core --example geo_lookup -- <数据目录> <类别...> -- <查询...>
//! # 例：
//! cargo run -q -p xt-core --example geo_lookup -- apps/desktop/binaries cn google -- www.baidu.com google.com
//! ```
//!
//! 存在的理由：解析器对不对**只能拿真数据验**。格式是我从字节里解出来的，
//! 单测只能证明「我自己构造的输入能解」，证明不了「上游真实文件能解」。

use std::process::ExitCode;

use xt_core::routing::geo::GeoData;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(split) = args.iter().position(|a| a == "--") else {
        eprintln!("用法: geo_lookup <数据目录> <类别...> -- <查询...>");
        return ExitCode::from(2);
    };
    let (head, queries) = args.split_at(split);
    let queries = &queries[1..];
    let Some(dir) = head.first() else {
        eprintln!("缺少数据目录");
        return ExitCode::from(2);
    };
    let categories: Vec<String> = head[1..].to_vec();

    let t0 = std::time::Instant::now();
    let data = match GeoData::load(std::path::Path::new(dir), &categories, &categories) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("加载失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "加载 {} 用时 {:?}（各类别条数见下）",
        dir,
        t0.elapsed()
    );
    for c in &categories {
        let s = data.site_len(c);
        let i = data.ip_len(c);
        if s > 0 || i > 0 {
            println!("  类别 {c}: 域名 {s} 条 / 网段 {i} 条");
        } else {
            println!("  类别 {c}: **未找到**（数据里没有这个类别？）");
        }
    }
    println!();

    for q in queries {
        // 是 IP 就查网段，否则查域名
        if let Ok(ip) = q.parse::<std::net::IpAddr>() {
            let hits: Vec<&String> = categories
                .iter()
                .filter(|c| data.ip_matches(c, ip))
                .collect();
            println!("  {q:32} (IP)  命中: {hits:?}");
        } else {
            let hits: Vec<&String> = categories
                .iter()
                .filter(|c| data.site_matches(c, q))
                .collect();
            println!("  {q:32} (域名) 命中: {hits:?}");
        }
    }
    ExitCode::SUCCESS
}
