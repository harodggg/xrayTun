//! 手动跑一遍**客户端自更新的完整准备流程**，但不做替换。
//!
//! 为什么单独有这个工具：`stage_app_update` 里最容易错的一步是「从
//! `SHA256SUMS.txt` 里取出我们那个包的摘要」—— 而它的单测曾经用手写的
//! 「理想」输入（没有 `./` 前缀），于是测试全绿、真机上一次都没成功过
//! （报「校验文件里没有 <包名>」，而文件明明就在里面）。
//!
//! 这个工具跑的是**线上真实的 release**，链路和 App 里完全一样：
//!
//! ```text
//! check_app → 下载 zip（带进度）→ 取 SHA256SUMS.txt
//!   → 按文件名查那一行 → 比对 sha256 → ditto 解压 → 核对包内版本号
//! ```
//!
//! ```bash
//! cargo run -p xraytun-desktop --example app_update_check
//! ```
//!
//! 只读：只往临时目录写，**绝不碰 /Applications**。

use std::process::ExitCode;

use xt_core::update;

fn main() -> ExitCode {
    let proxy = std::env::args()
        .position(|a| a == "--socks")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|p| p.parse().ok());

    println!("=== 1) 查最新版（匿名，仓库现在是公开的）===");
    let latest = match update::check_app(proxy) {
        Ok(a) => a,
        Err(e) => {
            println!("  ✗ {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  最新版 {}  发布于 {}", latest.version, latest.published_at);
    println!("  产物 {}（{:?} 字节）", latest.download_url.rsplit('/').next().unwrap_or("?"), latest.size);
    let total = latest.size;

    let tmp = std::env::temp_dir().join("xraytun-update-dryrun");
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        println!("  ✗ 建临时目录失败：{e}");
        return ExitCode::FAILURE;
    }

    let zip_name = latest.download_url.rsplit('/').next().unwrap_or("").to_string();
    let zip = tmp.join(&zip_name);

    println!("\n=== 2) 下载（每 200ms 报一次进度）===");
    let mut last_pct = u64::MAX;
    let r = update::download_with_progress(&latest.download_url, &zip, proxy, |done| {
        let pct = total.filter(|t| *t > 0).map(|t| done * 100 / t);
        if let Some(p) = pct {
            if p / 10 != last_pct / 10 {
                last_pct = p;
                println!("  {p}%  ({} MB)", done / 1024 / 1024);
            }
        }
    });
    if let Err(e) = r {
        println!("  ✗ {e}");
        return ExitCode::FAILURE;
    }
    println!("  ✓ 下载完成");

    println!("\n=== 3) 校验（这一步以前一直失败）===");
    let Some(digest_url) = &latest.digest_url else {
        println!("  ✗ release 没有 SHA256SUMS.txt");
        return ExitCode::FAILURE;
    };
    let text = match update::fetch_text(digest_url, proxy) {
        Ok(t) => t,
        Err(e) => {
            println!("  ✗ 取校验文件失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  校验文件原文：");
    for line in text.lines() {
        println!("    {line}");
    }
    let Some(want) = update::parse_sha256sum_for(&text, &zip_name) else {
        println!("  ✗ 校验文件里没有 {zip_name}");
        return ExitCode::FAILURE;
    };
    let got = match update::sha256_file(&zip) {
        Ok(g) => g,
        Err(e) => {
            println!("  ✗ {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("  期望 {want}");
    println!("  实际 {got}");
    if want != got {
        println!("  ✗ 校验和不匹配");
        return ExitCode::FAILURE;
    }
    println!("  ✓ 校验通过");

    println!("\n=== 4) 解压并核对包内版本 ===");
    let stage = tmp.join("stage");
    if let Err(e) = update::unzip_tree(&zip, &stage) {
        println!("  ✗ {e}");
        return ExitCode::FAILURE;
    }
    let app = stage.join("XrayTun.app");
    if !app.is_dir() {
        println!("  ✗ 解压后没有 XrayTun.app");
        return ExitCode::FAILURE;
    }
    let out = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(app.join("Contents/Info.plist"))
        .output();
    let inner = out
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("  包内版本 {inner}，release 声称 {}", latest.version);
    if inner != latest.version {
        println!("  ✗ 版本不一致");
        return ExitCode::FAILURE;
    }
    println!("  ✓ 一致");

    let _ = std::fs::remove_dir_all(&tmp);
    println!("\n✓ 整条自更新链路（除最后的替换）全部走通");
    ExitCode::SUCCESS
}
