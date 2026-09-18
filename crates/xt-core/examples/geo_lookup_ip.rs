//! 实测 IP 位置查询（走物理网卡，绕过隧道）。
//!
//! ```bash
//! cargo run -q -p xt-core --example geo_lookup_ip -- 45.207.197.185 en0
//! cargo run -q -p xt-core --example geo_lookup_ip -- --self en0
//! ```

use std::process::ExitCode;

use xt_core::geo_lookup;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: geo_lookup_ip <IP|--self> [网卡名，默认 en0]");
        return ExitCode::from(2);
    }
    let target = &args[0];
    let iface = args.get(1).map(|s| s.as_str()).unwrap_or("en0");
    let t0 = std::time::Instant::now();

    let result = if target == "--self" {
        geo_lookup::lookup_self(Some(iface))
    } else {
        geo_lookup::lookup(target, Some(iface))
    };
    match result {
        Ok(loc) => {
            println!(
                "  {} → {} {} ({:.4}, {:.4})  ISP: {}  来源: {}  用时 {:?}",
                loc.ip, loc.country, loc.city, loc.lat, loc.lon, loc.isp, loc.source,
                t0.elapsed()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("  查询失败: {e}（用时 {:?}）", t0.elapsed());
            ExitCode::FAILURE
        }
    }
}
