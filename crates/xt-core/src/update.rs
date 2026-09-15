//! 核心与 geo 数据的自动更新。
//!
//! # 两个硬约束
//!
//! **1. 绝不写进 `.app` 包里面。**
//!
//! 包里是 ad-hoc 签名的，改 `Contents/Resources/` 会让签名失效
//! （`codesign --verify` 直接失败）。改成把更新落在用户数据目录的
//! `core/` 下，由 `resolve_core_binary` 优先使用它 —— 包里的那份原封不动，
//! 于是**回退永远是「删掉 core/ 目录」**，不需要备份也不需要版本管理。
//!
//! **2. 不能信 GitHub 的 `releases/latest`。**
//!
//! 实测：`latest` 返回 `v26.3.27`，而当时最新的是 `v26.9.9` ——
//! 因为 XTLS 把新版本全部标成 **prerelease**，而 `latest` 会跳过 prerelease。
//! 照它写，一升级就把用户从 26.9.9 **降到** 26.3.27。所以这里拉列表自己排序。
//!
//! # 为什么用 `curl` 而不是 HTTP 客户端
//!
//! 需要 HTTPS + 重定向 + 断点 + SOCKS 代理，而 GitHub 在国内经常不通，
//! 走本地 SOCKS 入站是常态。`/usr/bin/curl` 一次满足全部，而且 macOS 自带。
//! 参数以数组形式传入、**不经过 shell**，所以不存在注入面。
//! 这与项目里已有的做法一致（`route(8)` / `netstat` / `osascript` 都是这么调的）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// 核心更新的上游。
pub const XRAY_REPO: &str = "XTLS/Xray-core";
/// geo 数据的上游：每天更新，且每个文件都带 `.sha256sum`。
pub const GEO_REPO: &str = "Loyalsoldier/v2ray-rules-dat";
/// 客户端**自己**的仓库 —— 应用自身的更新也从这里的 release 拉。
///
/// 这个仓库**已经改成公开**了，所以匿名就能拉。token 现在是**可选**的，
/// 但仍有意义：匿名走 GitHub 的 60 次/小时配额，而且是**按 IP**算的 ——
/// 我们自己的请求大多是经节点出去的，于是整台节点的用户共用那 60 次。
/// 填一个 token 可提到 5000 次/小时。
///
/// 如果哪天仓库又改回私有，就必须填 token，否则一律 404。
pub const APP_REPO: &str = "harodggg/xrayTun";

/// 下载超时。核心约 20MB、geo 约 30MB，给宽一点。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const API_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// 纯函数（全部有测试）
// ---------------------------------------------------------------------------

/// 版本号比较。XTLS 的 tag 形如 `v26.9.9`（年.月.日），按**数值**逐段比较。
///
/// 逐段数值比较很重要：`26.9.9` > `26.3.27` 成立，而字符串比较会得出相反结论
/// （`"9" > "3"` 恰好对，但 `"26.9.10"` vs `"26.9.9"` 字符串比较就错了）。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<u64> {
        s.trim_start_matches('v')
            .split(['.', '-', '_'])
            .map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>())
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (mut x, mut y) = (parts(a), parts(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x.cmp(&y)
}

/// 从 `.dgst` 文件里取 `SHA2-256=` 那一行。
///
/// 实测格式（XTLS 的产物）：
/// ```text
/// MD5= c725...
/// SHA1= f024...
/// SHA2-256= 2e93a67e...
/// SHA2-512= 5568...
/// ```
pub fn parse_dgst_sha256(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("SHA2-256=") else {
            continue;
        };
        let hex = rest.trim().to_ascii_lowercase();
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hex);
        }
    }
    None
}

/// 从 `.sha256sum` 文件里取摘要。
///
/// 标准 `sha256sum` 输出格式：`<hex>  <filename>`（也可能只有裸 hex）。
pub fn parse_sha256sum(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    let hex = first.trim().to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// 当前 macOS 该下哪个产物。
pub fn macos_asset_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("Xray-macos-arm64-v8a.zip"),
        "x86_64" => Ok("Xray-macos-64.zip"),
        other => Err(Error::Update(format!("不支持的架构：{other}"))),
    }
}

/// GitHub release 列表里的一条（只保留需要的字段）。
#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// 一个可用的更新。
#[derive(Debug, Clone, Serialize)]
pub struct Available {
    pub version: String,
    pub published_at: String,
    pub prerelease: bool,
    pub download_url: String,
    /// 校验文件地址。`None` 表示上游没提供 —— 那就只能做结构性校验，
    /// 界面应当如实说明「无上游校验」而不是假装验过了。
    pub digest_url: Option<String>,
}

/// 在 release 列表里挑出版本号最大的那个。
///
/// **刻意包含 prerelease**：XTLS 当前所有新版本都带 prerelease 标记，
/// 排除它们等于永远收不到更新（而且会退回三个月前的旧版）。
/// 是不是 prerelease 会如实报给界面，让用户自己判断。
fn pick_latest(releases: &[GhRelease], want_asset: impl Fn(&str) -> bool) -> Option<Available> {
    releases
        .iter()
        .filter(|r| r.assets.iter().any(|a| want_asset(&a.name)))
        .max_by(|a, b| compare_versions(&a.tag_name, &b.tag_name))
        .and_then(|r| {
            let asset = r.assets.iter().find(|a| want_asset(&a.name))?;
            let digest_url = r
                .assets
                .iter()
                .find(|a| a.name == format!("{}.dgst", asset.name))
                .or_else(|| {
                    r.assets
                        .iter()
                        .find(|a| a.name == format!("{}.sha256sum", asset.name))
                })
                .map(|a| a.browser_download_url.clone());
            Some(Available {
                version: r.tag_name.clone(),
                published_at: r.published_at.clone(),
                prerelease: r.prerelease,
                download_url: asset.browser_download_url.clone(),
                digest_url,
            })
        })
}

// ---------------------------------------------------------------------------
// 命令执行（不经过 shell）
// ---------------------------------------------------------------------------

/// 跑一个命令，返回 stdout。不经过 shell，参数以数组传入。
fn run(program: &str, args: &[String], timeout: Duration) -> Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    // 用 `timeout` 包装会把信号语义搞复杂；这里依赖各工具自己的超时参数
    // （curl 有 --max-time），并在调用处给出足够大的值。
    let _ = timeout;
    let out = cmd
        .output()
        .map_err(|e| Error::Update(format!("执行 {program} 失败：{e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Update(format!(
            "{program} 退出码 {:?}：{}",
            out.status.code(),
            err.trim().chars().take(300).collect::<String>()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 走本地 SOCKS 入站的代理参数。
///
/// 传 `Some(port)` 时所有请求都经它出去 —— GitHub 在国内直连经常不通，
/// 而用户的节点通常是通的。传 `None` 时直连。
fn proxy_args(proxy: Option<u16>) -> Vec<String> {
    match proxy {
        Some(port) => vec!["--socks5-hostname".into(), format!("127.0.0.1:{port}")],
        None => Vec::new(),
    }
}

/// GitHub 认证参数。只读权限的 token 就够。
///
/// 什么时候需要：`APP_REPO` 现在是公开的，匿名可用；但匿名只有 60 次/小时，
/// 而且**按 IP** 算 —— 请求通常是经节点出去的，整台节点的用户共用这个额度。
/// 填 token 可提到 5000 次/小时，也让「检查更新」不会因为别人刷满了而失败。
/// 仓库若改回私有，则**必须**填。XTLS / Loyalsoldier 传 `None` 即可。
///
/// 注意 token 会出现在 `curl` 的命令行里。macOS 上别的用户本来就看不到
/// 你的进程参数（`ps` 对他人进程是受限的），而这是单用户桌面应用，
/// 所以直接用 `-H` 而不额外折腾 header 文件。
fn auth_args(token: Option<&str>) -> Vec<String> {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => vec!["-H".into(), format!("Authorization: Bearer {t}")],
        None => Vec::new(),
    }
}

/// 取 GitHub API 的 JSON。三个上游共用一份 curl 参数。
///
/// 返回正文和 HTTP 状态码。**必须把状态码带出来**：匿名访问时
/// 「403 限流」和「404 不存在」是完全不同的两回事，而 curl 的 `-f`
/// 会把它们压成同一句「退出码 22」。
fn gh_get(url: &str, proxy: Option<u16>, token: Option<&str>) -> Result<(String, u16)> {
    let mut args = vec![
        "-sSL".to_string(),
        "--max-time".to_string(),
        API_TIMEOUT.as_secs().to_string(),
        "-H".to_string(),
        "Accept: application/vnd.github+json".to_string(),
        "-H".to_string(),
        "User-Agent: XrayTun".to_string(),
        // 把状态码附在正文后面，最后一行取出来。
        "-w".to_string(),
        "\n%{http_code}".to_string(),
    ];
    args.extend(auth_args(token));
    args.extend(proxy_args(proxy));
    args.push(url.to_string());
    let raw = run("/usr/bin/curl", &args, API_TIMEOUT)?;
    let (body, code) = raw
        .rsplit_once('\n')
        .ok_or_else(|| Error::Update("GitHub 返回里没有状态码".into()))?;
    let code: u16 = code
        .trim()
        .parse()
        .map_err(|_| Error::Update(format!("无法解析 HTTP 状态码：{code:?}")))?;
    Ok((body.to_string(), code))
}

/// 把非 2xx 状态码翻译成**能指向原因**的话。
///
/// 这里值得多花几行：本仓库曾经是私有的，那时匿名请求返回 404；
/// 而**公开仓库被限流是 403**。两者都表现为「检查更新失败」，
/// 但处置完全不同 —— 前者要去开权限或填 token，后者等一会儿或填 token 提额度。
fn gh_status_error(what: &str, code: u16) -> Error {
    let hint = match code {
        401 => "token 无效或已过期",
        403 => "GitHub 限流了（匿名每小时只有 60 次，填一个 token 可提到 5000 次）",
        404 => "仓库或 release 不存在；如果仓库是私有的，需要填一个只读 token",
        _ => "GitHub 返回了非预期状态",
    };
    Error::Update(format!("{what} 失败（HTTP {code}）：{hint}"))
}

/// 取某个仓库的 release 列表（原始结构）。
fn fetch_raw(
    repo: &str,
    per_page: usize,
    proxy: Option<u16>,
    token: Option<&str>,
) -> Result<Vec<GhRelease>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={per_page}");
    let (body, code) = gh_get(&url, proxy, token)?;
    if code != 200 {
        return Err(gh_status_error(
            &format!("取 {repo} 的 release 列表"),
            code,
        ));
    }
    serde_json::from_str(&body).map_err(|e| Error::Update(format!("解析 GitHub 返回失败：{e}")))
}

/// 取 GitHub release 列表。
pub fn fetch_releases(repo: &str, per_page: usize, proxy: Option<u16>) -> Result<Vec<Available>> {
    let releases = fetch_raw(repo, per_page, proxy, None)?;

    // 把原始结构转成 Available 列表（保留全部条目，供调用方自己挑）。
    let mut out = Vec::new();
    for r in &releases {
        for a in &r.assets {
            let digest_url = r
                .assets
                .iter()
                .find(|d| d.name == format!("{}.dgst", a.name) || d.name == format!("{}.sha256sum", a.name))
                .map(|d| d.browser_download_url.clone());
            out.push(Available {
                version: r.tag_name.clone(),
                published_at: r.published_at.clone(),
                prerelease: r.prerelease,
                download_url: a.browser_download_url.clone(),
                digest_url,
            });
        }
    }
    Ok(out)
}

/// 查核心的最新版。
pub fn check_core(proxy: Option<u16>) -> Result<Available> {
    let releases = fetch_raw(XRAY_REPO, 30, proxy, None)?;
    let asset = macos_asset_name()?;
    pick_latest(&releases, |n| n == asset)
        .ok_or_else(|| Error::Update(format!("最近 30 个 release 里没有 {asset}")))
}

/// 客户端自己的 release 里，哪个资产是要装的。
///
/// 只认 `.zip`：CI 出的是 `XrayTun_<版本>_x86_64_arm64.zip`（通用包）。
/// 用 zip 而不是 dmg —— 解压出来直接就是可以替换的 `.app`，
/// 不用挂载磁盘映像（挂载在无图形会话/受限环境里会失败）。
pub fn app_asset_matches(name: &str) -> bool {
    name.starts_with("XrayTun_") && name.ends_with(".zip")
}

/// 查客户端自己的最新版。
pub fn check_app(proxy: Option<u16>, token: Option<&str>) -> Result<Available> {
    let releases = fetch_raw(APP_REPO, 30, proxy, token)?;
    let latest = releases
        .iter()
        .filter(|r| r.assets.iter().any(|a| app_asset_matches(&a.name)))
        .max_by(|a, b| compare_versions(&a.tag_name, &b.tag_name))
        .ok_or_else(|| Error::Update("本仓库的 release 里没有客户端 zip".into()))?;
    let asset = latest
        .assets
        .iter()
        .find(|a| app_asset_matches(&a.name))
        .ok_or_else(|| Error::Update("release 里没有客户端 zip".into()))?;
    Ok(Available {
        version: latest.tag_name.trim_start_matches('v').to_string(),
        published_at: latest.published_at.clone(),
        prerelease: latest.prerelease,
        download_url: asset.browser_download_url.clone(),
        // 校验和是**整包一份** `SHA256SUMS.txt`，装的是一行一行取，
        // 所以这里只给出文件地址，真正的比对在 install_app_update 里做。
        digest_url: digest_asset_name(&latest.assets),
    })
}

/// `SHA256SUMS.txt` 的下载地址（如果这次 release 带了的话）。
fn digest_asset_name(assets: &[GhAsset]) -> Option<String> {
    assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .map(|a| a.browser_download_url.clone())
}

/// 查 geo 数据的最新版。
pub fn check_geo(proxy: Option<u16>) -> Result<Available> {
    let releases = fetch_raw(GEO_REPO, 5, proxy, None)?;
    let latest = releases
        .iter()
        .filter(|r| r.assets.iter().any(|a| a.name == "geosite.dat"))
        .max_by_key(|r| r.tag_name.clone())
        .ok_or_else(|| Error::Update("取不到 geo 数据的 release".into()))?;
    let asset = latest
        .assets
        .iter()
        .find(|a| a.name == "geosite.dat")
        .ok_or_else(|| Error::Update("release 里没有 geosite.dat".into()))?;
    let digest_url = latest
        .assets
        .iter()
        .find(|a| a.name == "geosite.dat.sha256sum")
        .map(|a| a.browser_download_url.clone());
    Ok(Available {
        version: latest.tag_name.clone(),
        published_at: latest.published_at.clone(),
        prerelease: latest.prerelease,
        download_url: asset.browser_download_url.clone(),
        digest_url,
    })
}

/// 下载到文件。
pub fn download(url: &str, dest: &Path, proxy: Option<u16>) -> Result<()> {
    download_auth(url, dest, proxy, None)
}

/// 下载到文件（带认证）。私有仓库的 release 资产必须走这条。
pub fn download_auth(
    url: &str,
    dest: &Path,
    proxy: Option<u16>,
    token: Option<&str>,
) -> Result<()> {
    let mut args = vec![
        "-sSL".to_string(),
        "-f".to_string(),
        "--retry".to_string(),
        "2".to_string(),
        "--max-time".to_string(),
        DOWNLOAD_TIMEOUT.as_secs().to_string(),
        "-o".to_string(),
        dest.display().to_string(),
    ];
    args.extend(auth_args(token));
    args.extend(proxy_args(proxy));
    args.push(url.to_string());
    run("/usr/bin/curl", &args, DOWNLOAD_TIMEOUT)?;
    Ok(())
}

/// 取一段文本（校验文件用）。
pub fn fetch_text(url: &str, proxy: Option<u16>) -> Result<String> {
    fetch_text_auth(url, proxy, None)
}

/// 取一段文本（带认证）。私有仓库的 `SHA256SUMS.txt` 走这条。
pub fn fetch_text_auth(url: &str, proxy: Option<u16>, token: Option<&str>) -> Result<String> {
    let mut args = vec![
        "-sSL".to_string(),
        "-f".to_string(),
        "--max-time".to_string(),
        API_TIMEOUT.as_secs().to_string(),
    ];
    args.extend(auth_args(token));
    args.extend(proxy_args(proxy));
    args.push(url.to_string());
    run("/usr/bin/curl", &args, API_TIMEOUT)
}

/// 从整包一份的 `SHA256SUMS.txt` 里取**指定文件**那一行。
///
/// 和 [`parse_sha256sum`] 的区别：那个是「一个文件配一个 .sha256sum」，
/// 取第一个 hex 就行；而我们自己的 release 把全部产物写在一份里：
///
/// ```text
/// 45f9a6…1642  XrayTun_0.5.2_x86_64_arm64.dmg
/// 9c1b2e…77aa  XrayTun_0.5.2_x86_64_arm64.zip
/// ```
///
/// 所以必须**按文件名查行**，不能随手取第一行 —— 取错了就是拿 dmg 的摘要
/// 去校验 zip，永远不通过。
pub fn parse_sha256sum_for(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(hex), Some(file)) = (it.next(), it.next()) else {
            continue;
        };
        // 有些工具会写成 `*name`（二进制模式）。
        if file.trim_start_matches('*') != name {
            continue;
        }
        let hex = hex.trim().to_ascii_lowercase();
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hex);
        }
    }
    None
}

/// 解压 zip 并**保留目录结构、符号链接与可执行位**。
///
/// 不能用 [`unzip_into`]：那个带 `-j`，会把路径拍平，而 `.app` 正是靠目录
/// 结构和若干符号链接（Frameworks）才成立，拍平了就废了。
/// `ditto -x -k` 是 macOS 上保留这些东西的标准做法。
pub fn unzip_tree(zip: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| Error::Update(format!("建目录失败：{e}")))?;
    run(
        "/usr/bin/ditto",
        &[
            "-x".into(),
            "-k".into(),
            zip.display().to_string(),
            dest.display().to_string(),
        ],
        Duration::from_secs(300),
    )
    .map_err(|e| Error::Update(format!("解压客户端 zip 失败：{e}")))?;
    Ok(())
}

/// 生成「替换 .app 并重启」的脚本。
///
/// 为什么不能直接自己替换：**不能覆盖一个正在运行的 .app** —— 替换到一半
/// 就会毁掉进程正在读的文件。所以必须交给一个**独立于本进程**的脚本来做，
/// 它先等我们退出，再替换、再拉起。
///
/// 为什么用 `ditto` 而不是 `cp -R`：`.app` 里有符号链接和扩展属性，
/// `ditto` 会原样保留，`cp -R` 不保证。
///
/// 抽成纯函数是为了能单测 —— 这段字符串会在用户机器上以他的权限跑，
/// 里面每个 quoting 都值得钉住。
pub fn self_update_script(pid: u32, src_app: &Path, target_app: &Path, tmp_root: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
    format!(
        r#"#!/bin/sh
# 由 XrayTun 自动更新生成。等待旧进程退出 → 替换 .app → 重启。
set -e
# 日志单独留一份：脚本跑的时候应用已经退出了，出问题只能靠它回溯。
mkdir -p "$HOME/Library/Logs/XrayTun"
exec >>"$HOME/Library/Logs/XrayTun/app-update.log" 2>&1
echo "=== $(date) 开始替换 {target}，等待 pid {pid} 退出 ==="
# 1) 等旧进程真的退出（否则替换的是正在使用的 bundle）
i=0
while kill -0 {pid} 2>/dev/null; do
  sleep 0.5
  i=$((i+1))
  [ "$i" -gt 120 ] && {{ echo "等待超时"; exit 1; }}
done
sleep 1
# 2) 替换。先删后拷，不用 mv —— mv 跨卷会退化成 copy+delete，
#    中途失败就只剩一个残缺的 bundle。
rm -rf {target}
/usr/bin/ditto {src} {target}
# 3) 去掉隔离标记，否则新包第一次打开会被 Gatekeeper 拦
/usr/bin/xattr -dr com.apple.quarantine {target} 2>/dev/null || true
# 4) 清理暂存目录后重启
rm -rf {tmp}
/usr/bin/open {target}
"#,
        pid = pid,
        target = q(target_app),
        src = q(src_app),
        tmp = q(tmp_root),
    )
}

/// 算文件的 SHA-256。用系统的 `shasum`，省掉一个加密库依赖。
pub fn sha256_file(path: &Path) -> Result<String> {
    let out = run(
        "/usr/bin/shasum",
        &["-a".into(), "256".into(), path.display().to_string()],
        Duration::from_secs(120),
    )?;
    out.split_whitespace()
        .next()
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| Error::Update("shasum 没有输出摘要".into()))
}

/// geo 数据文件的结构性校验。
///
/// 上游的 `.sha256sum` 已经能证明「和发布的一致」，这一层额外挡的是
/// 「发布本身有问题」以及**下载到一半被截断**：两者都会让文件不是合法的
/// protobuf，而 Xray 缺了 geo 数据的行为是**规则静默不命中**，
/// 日志里没有任何错误 —— 所以宁可在这里拒收。
pub fn looks_like_geo_dat(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| Error::Update(format!("读 geo 文件失败：{e}")))?;
    if bytes.len() < 1024 * 1024 {
        return Err(Error::Update(format!(
            "geo 文件只有 {} 字节，不像正常产物（geoip 约 17MB / geosite 约 11MB）",
            bytes.len()
        )));
    }
    // 顶层是 `GeoIPList` / `GeoSiteList`，都由若干 length-delimited 字段组成。
    // 走一遍顶层字段，能走完就说明结构没坏、也没被截断。
    let mut i = 0usize;
    let mut entries = 0usize;
    while i < bytes.len() {
        let (key, next) = read_varint(&bytes, i)?;
        i = next;
        let wire = (key & 7) as u8;
        if wire != 2 {
            return Err(Error::Update(format!("geo 文件在第 {i} 字节出现非预期结构")));
        }
        let (len, next) = read_varint(&bytes, i)?;
        i = next;
        let end = i
            .checked_add(len as usize)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| Error::Update("geo 文件被截断（字段长度越界）".into()))?;
        i = end;
        entries += 1;
    }
    if entries == 0 {
        return Err(Error::Update("geo 文件里没有任何分类".into()));
    }
    Ok(())
}

fn read_varint(buf: &[u8], mut i: usize) -> Result<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let Some(&b) = buf.get(i) else {
            return Err(Error::Update("geo 文件在 varint 中途截断".into()));
        };
        i += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok((v, i));
        }
        shift += 7;
        if shift >= 64 {
            return Err(Error::Update("geo 文件里的 varint 超过 64 位".into()));
        }
    }
}

/// 从 zip 里解出需要的文件到目标目录。
pub fn unzip_into(zip: &Path, dest: &Path, names: &[&str]) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| Error::Update(format!("建目录失败：{e}")))?;
    for n in names {
        run(
            "/usr/bin/unzip",
            &[
                "-o".into(),
                "-j".into(),
                zip.display().to_string(),
                n.to_string(),
                "-d".into(),
                dest.display().to_string(),
            ],
            Duration::from_secs(120),
        )
        .map_err(|e| Error::Update(format!("解压 {n} 失败：{e}")))?;
    }
    Ok(())
}

/// 托管目录：更新下来的核心与 geo 数据放这里，**不碰 .app 包**。
pub fn managed_core_dir(data_root: &Path) -> PathBuf {
    data_root.join("core")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本比较必须按数值逐段——这是 `latest` 端点不可信的替代方案。
    #[test]
    fn version_compare_is_numeric_per_segment() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("v26.9.9", "v26.3.27"), Greater);
        assert_eq!(compare_versions("v26.3.27", "v26.9.9"), Less);
        assert_eq!(compare_versions("26.9.9", "v26.9.9"), Equal);
        // 字符串比较会在这里出错：`"26.9.9" > "26.9.10"` 成立，但语义相反
        assert_eq!(compare_versions("v26.9.10", "v26.9.9"), Greater);
        assert_eq!(compare_versions("v26.9.9", "v26.9.10"), Less);
    }

    /// 取最大版本时必须**包含 prerelease**。
    ///
    /// 钉住实测到的事实：XTLS 当前所有新版本都是 prerelease，
    /// 排除它们会让用户永远收不到更新，甚至被"升级"到三个月前的旧版。
    #[test]
    fn pick_latest_includes_prereleases() {
        let releases: Vec<GhRelease> = serde_json::from_str(
            r#"[
              {"tag_name":"v26.3.27","prerelease":false,"published_at":"2026-03-27",
               "assets":[{"name":"Xray-macos-arm64-v8a.zip","browser_download_url":"u-old"},
                         {"name":"Xray-macos-arm64-v8a.zip.dgst","browser_download_url":"d-old"}]},
              {"tag_name":"v26.9.9","prerelease":true,"published_at":"2026-09-08",
               "assets":[{"name":"Xray-macos-arm64-v8a.zip","browser_download_url":"u-new"},
                         {"name":"Xray-macos-arm64-v8a.zip.dgst","browser_download_url":"d-new"}]}
            ]"#,
        )
        .unwrap();
        let got = pick_latest(&releases, |n| n == "Xray-macos-arm64-v8a.zip").unwrap();
        assert_eq!(got.version, "v26.9.9", "必须选到更新的 prerelease 版本");
        assert!(got.prerelease);
        assert_eq!(got.download_url, "u-new");
        assert_eq!(got.digest_url.as_deref(), Some("d-new"));
    }

    #[test]
    fn parse_dgst_extracts_sha256() {
        let text = "MD5= c7253cf3e605d261f5e1a4a55f447d9d\n\
                    SHA1= f02425f9dc1e353388dc9042914b7a0a809b0272\n\
                    SHA2-256= 2e93a67e8aa1936ecefb307e120830fcbd4c643ab9b1c46a2d0838d5f8409eaf\n\
                    SHA2-512= 55683e38\n";
        assert_eq!(
            parse_dgst_sha256(text).as_deref(),
            Some("2e93a67e8aa1936ecefb307e120830fcbd4c643ab9b1c46a2d0838d5f8409eaf")
        );
        assert_eq!(parse_dgst_sha256("MD5= abc\n"), None, "没有 SHA2-256 就该返回 None");
        assert_eq!(parse_dgst_sha256("SHA2-256= 太短\n"), None);
    }

    #[test]
    fn parse_sha256sum_handles_both_layouts() {
        let hex = "a".repeat(64);
        assert_eq!(parse_sha256sum(&format!("{hex}  geosite.dat")), Some(hex.clone()));
        assert_eq!(parse_sha256sum(&format!("{hex}\n")), Some(hex));
        assert_eq!(parse_sha256sum("not-a-hash"), None);
    }

    /// 结构性校验要能挡住「截断的文件」和「内容根本不是 geo 数据」。
    #[test]
    fn geo_structure_check_rejects_junk() {
        let dir = std::env::temp_dir().join("xt-geo-check-test");
        let _ = std::fs::create_dir_all(&dir);

        // 太小
        let small = dir.join("small.dat");
        std::fs::write(&small, b"hi").unwrap();
        assert!(looks_like_geo_dat(&small).is_err());

        // 够大但不是 protobuf（例如下回来一个 HTML 错误页）
        let junk = dir.join("junk.dat");
        std::fs::write(&junk, vec![0xffu8; 2 * 1024 * 1024]).unwrap();
        assert!(looks_like_geo_dat(&junk).is_err());

        // 合法的顶层结构：field 1, wire 2, len 3, "abc"，后面接一串空字段凑够大小。
        // （第一版这里多带了两个 0x00 字节，那会构成 wire type 0 的非法字段 ——
        //   是测试写错了，校验器拒绝它是对的。）
        let ok = dir.join("ok.dat");
        let mut file = vec![0x0au8, 0x03, b'a', b'b', b'c'];
        while file.len() < 1024 * 1024 + 16 {
            file.extend_from_slice(&[0x0a, 0x00]);
        }
        std::fs::write(&ok, &file).unwrap();
        assert!(looks_like_geo_dat(&ok).is_ok(), "结构合法的文件应通过");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macos_asset_matches_arch() {
        let a = macos_asset_name().unwrap();
        assert!(a.starts_with("Xray-macos-"), "{a}");
    }

    #[test]
    fn managed_dir_lives_under_the_data_root() {
        let p = managed_core_dir(Path::new("/tmp/data"));
        // 关键性质：**不在 .app 包里**。改包内文件会让签名失效。
        assert_eq!(p, PathBuf::from("/tmp/data/core"));
        assert!(!p.to_string_lossy().contains(".app"));
    }
}

// ---------------------------------------------------------------------------
// 安装
// ---------------------------------------------------------------------------

/// 托管目录里的元信息。**最后一步写**，所以它的存在等价于「这套文件是完整的」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledMeta {
    pub core_version: Option<String>,
    pub core_installed_at: Option<u64>,
    pub geo_tag: Option<String>,
    pub geo_installed_at: Option<u64>,
}

impl InstalledMeta {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self, dir: &Path) -> Result<()> {
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Update(format!("序列化元信息失败：{e}")))?;
        std::fs::write(dir.join("meta.json"), s)
            .map_err(|e| Error::Update(format!("写元信息失败：{e}")))
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 同步地跑一次 `xray version` 并取第一行。
///
/// 不用 `xray::core_version`（它是 async）：安装流程整体是同步的，
/// 为了这一处引入 async 会把整条调用链染色。
fn core_version_sync(binary: &Path) -> Result<String> {
    let out = Command::new(binary)
        .arg("version")
        .output()
        .map_err(|e| Error::Update(format!("执行失败：{e}")))?;
    if !out.status.success() {
        return Err(Error::Update(format!("退出码 {:?}", out.status.code())));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .next()
        .map(|l| l.trim().to_string())
        .ok_or_else(|| Error::Update("没有输出".into()))
}

/// 安装核心更新。
///
/// 步骤刻意排成「全部验证完再落到生效位置」：下载 → 校验摘要 → 解压到暂存 →
/// 结构校验 → 跑一次 `xray version` 确认能执行且版本达标 → 才改名进托管目录。
/// 任何一步失败都不会留下半套文件。
pub fn install_core(
    available: &Available,
    managed_dir: &Path,
    proxy: Option<u16>,
) -> Result<InstalledMeta> {
    let staging = managed_dir.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| Error::Update(format!("建暂存目录失败：{e}")))?;

    let zip = staging.join("core.zip");
    download(&available.download_url, &zip, proxy)?;

    // 校验摘要。上游给了就必须验；没给要**如实说明**而不是假装验过。
    if let Some(url) = &available.digest_url {
        let text = fetch_text(url, proxy)?;
        let want = parse_dgst_sha256(&text)
            .or_else(|| parse_sha256sum(&text))
            .ok_or_else(|| Error::Update("校验文件里没有可识别的 SHA-256".into()))?;
        let got = sha256_file(&zip)?;
        if got != want {
            return Err(Error::Update(format!(
                "摘要不匹配，已放弃安装\n  期望 {want}\n  实际 {got}"
            )));
        }
    } else {
        tracing::warn!(
            version = %available.version,
            "上游没有提供校验文件，只能做结构校验"
        );
    }

    unzip_into(&zip, &staging, &["xray", "geoip.dat", "geosite.dat"])?;

    let staged_xray = staging.join("xray");
    if !staged_xray.is_file() {
        return Err(Error::Update("产物里没有 xray".into()));
    }
    // 可执行位：unzip 不一定还原它。
    let mut perms = std::fs::metadata(&staged_xray)
        .map_err(|e| Error::Update(format!("读权限失败：{e}")))?
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    let _ = std::fs::set_permissions(&staged_xray, perms);

    // **验证它能跑**，而不只是文件存在。
    // 这是比摘要更贴近实际的一道：下载对了但架构不对（比如把 arm64 装到
    // x86_64 的机器上）摘要照样能对上，只有执行才会暴露。
    let version = core_version_sync(&staged_xray)
        .map_err(|e| Error::Update(format!("新核心无法执行，已放弃安装：{e}")))?;
    let parsed = version
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    if parsed.is_empty() {
        return Err(Error::Update(format!("无法从「{version}」解析版本号")));
    }
    if compare_versions(&parsed, crate::xray::MIN_CORE_VERSION_NATIVE_TUN)
        == std::cmp::Ordering::Less
    {
        return Err(Error::Update(format!(
            "新核心 {parsed} 低于原生 TUN 的下限 {}，已放弃安装",
            crate::xray::MIN_CORE_VERSION_NATIVE_TUN
        )));
    }

    // geo 一并校验（如果这次带着）。
    for geo in ["geoip.dat", "geosite.dat"] {
        let p = staging.join(geo);
        if p.is_file() {
            looks_like_geo_dat(&p)?;
        }
    }

    // ---- 落地 ----
    std::fs::create_dir_all(managed_dir)
        .map_err(|e| Error::Update(format!("建托管目录失败：{e}")))?;
    let mut meta = InstalledMeta::load(managed_dir);

    std::fs::rename(&staged_xray, managed_dir.join("xray"))
        .map_err(|e| Error::Update(format!("安装核心失败：{e}")))?;
    meta.core_version = Some(parsed);
    meta.core_installed_at = Some(now_unix());

    // 核心的 zip 里也带 geo 数据（版本与核心同步）。只有托管目录里**还没有**
    // geo 时才铺进去，避免把用户刚更新过的、更新的 geo 覆盖回旧版。
    if !managed_dir.join("geosite.dat").is_file() {
        for geo in ["geoip.dat", "geosite.dat"] {
            let src = staging.join(geo);
            if src.is_file() {
                std::fs::rename(&src, managed_dir.join(geo))
                    .map_err(|e| Error::Update(format!("安装 {geo} 失败：{e}")))?;
            }
        }
        meta.geo_tag.get_or_insert_with(|| format!("随核心 {}", available.version));
        meta.geo_installed_at.get_or_insert_with(now_unix);
    }

    meta.save(managed_dir)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(meta)
}

/// 安装 geo 数据更新。
///
/// geo 和核心是**两条独立通道**：geo 上游每天更新，核心几个月一次。
/// 所以这里只碰 geo 两个文件，不动核心。
pub fn install_geo(
    available: &Available,
    managed_dir: &Path,
    proxy: Option<u16>,
) -> Result<InstalledMeta> {
    let staging = managed_dir.join(".staging-geo");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| Error::Update(format!("建暂存目录失败：{e}")))?;

    // geosite 的 release 里两个文件各自带一个 .sha256sum，
    // 这里按文件名把 release 里的资源找出来。
    let (geoip_url, geosite_url, base) = geo_urls(available, proxy)?;

    let mut staged: Vec<(String, PathBuf)> = Vec::new();
    for (name, url) in [("geoip.dat", geoip_url), ("geosite.dat", geosite_url)] {
        let dest = staging.join(name);
        download(&url, &dest, proxy)?;

        // 对应的 .sha256sum 与数据文件同目录同名 + 后缀。
        let sum_url = format!("{url}.sha256sum");
        if let Ok(text) = fetch_text(&sum_url, proxy) {
            if let Some(want) = parse_sha256sum(&text) {
                let got = sha256_file(&dest)?;
                if got != want {
                    return Err(Error::Update(format!(
                        "{name} 摘要不匹配，已放弃安装\n  期望 {want}\n  实际 {got}"
                    )));
                }
            }
        } else {
            tracing::warn!(file = name, "没有取到校验文件，只做结构校验");
        }

        // 结构校验：截断的文件在这里被挡住。
        // 缺 geo 数据的表现是**规则静默不命中**，没有报错，所以宁可拒收。
        looks_like_geo_dat(&dest)?;
        staged.push((name.to_string(), dest));
    }

    std::fs::create_dir_all(managed_dir)
        .map_err(|e| Error::Update(format!("建托管目录失败：{e}")))?;
    for (name, src) in staged {
        std::fs::rename(&src, managed_dir.join(&name))
            .map_err(|e| Error::Update(format!("安装 {name} 失败：{e}")))?;
    }

    let mut meta = InstalledMeta::load(managed_dir);
    meta.geo_tag = Some(format!("{base} {}", available.version));
    meta.geo_installed_at = Some(now_unix());
    meta.save(managed_dir)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(meta)
}

/// 从 geo release 里把两个数据文件的下载地址找出来。
fn geo_urls(available: &Available, proxy: Option<u16>) -> Result<(String, String, String)> {
    // available 里只带了 geosite 的地址，geoip 要靠同一 release 的 tag 拼。
    // 用 API 再取一次这一条 release，拿到它全部资源 —— 比猜 URL 可靠。
    let tag = &available.version;
    let mut releases = fetch_releases(GEO_REPO, 10, proxy)?;
    releases.retain(|a| &a.version == tag);
    let dir = available
        .download_url
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .ok_or_else(|| Error::Update("无法从下载地址推断目录".into()))?;
    let geoip = format!("{dir}/geoip.dat");
    let geosite = format!("{dir}/geosite.dat");
    Ok((geoip, geosite, tag.clone()))
}

/// 清空托管目录 —— 回到包内自带的版本。这是唯一的「回退」手段，所以它必须一定能成功。
pub fn revert_managed(managed_dir: &Path) -> Result<()> {
    if managed_dir.exists() {
        std::fs::remove_dir_all(managed_dir)
            .map_err(|e| Error::Update(format!("删除托管目录失败：{e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod install_tests {
    use super::*;

    /// 回退必须在目录不存在时也不报错 —— 它要能被无脑调用。
    #[test]
    fn revert_is_idempotent() {
        let dir = std::env::temp_dir().join("xt-revert-test/core");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(revert_managed(&dir).is_ok(), "目录不存在也该成功");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("xray"), b"x").unwrap();
        assert!(revert_managed(&dir).is_ok());
        assert!(!dir.exists());
    }

    /// 元信息读不到/坏掉时要退回默认值，不能 panic ——
    /// 这个文件在「更新装到一半」时就是缺失的。
    #[test]
    fn meta_load_tolerates_missing_and_corrupt() {
        let dir = std::env::temp_dir().join("xt-meta-test");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(InstalledMeta::load(&dir).core_version, None, "目录不存在");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"{ this is not json").unwrap();
        assert_eq!(InstalledMeta::load(&dir).core_version, None, "内容坏掉");
    }

    // ---------------- 客户端自更新 ----------------

    /// 只认客户端自己的 zip，别把 dmg 或核心包当成更新。
    #[test]
    fn app_asset_matches_only_own_zip() {
        assert!(app_asset_matches("XrayTun_0.5.2_x86_64_arm64.zip"));
        assert!(!app_asset_matches("XrayTun_0.5.2_x86_64_arm64.dmg"));
        assert!(!app_asset_matches("SHA256SUMS.txt"));
        assert!(!app_asset_matches("Xray-macos-arm64-v8a.zip"), "别把核心当成客户端");
        assert!(!app_asset_matches("geosite.dat"));
    }

    /// 整包一份的 `SHA256SUMS.txt` 必须**按文件名查行**。
    ///
    /// 取第一行是错的：里面 dmg 排在 zip 前面，取错了就是拿 dmg 的摘要
    /// 去校验 zip，永远不通过 —— 而且看起来像「下载损坏」，极难查。
    #[test]
    fn checksums_are_looked_up_by_file_name() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text =
            format!("{a}  XrayTun_0.5.2_x86_64_arm64.dmg\n{b}  XrayTun_0.5.2_x86_64_arm64.zip\n");
        assert_eq!(
            parse_sha256sum_for(&text, "XrayTun_0.5.2_x86_64_arm64.zip").as_deref(),
            Some(b.as_str()),
            "要取 zip 那一行，不是第一行",
        );
        assert_eq!(
            parse_sha256sum_for(&text, "XrayTun_0.5.2_x86_64_arm64.dmg").as_deref(),
            Some(a.as_str()),
        );
        assert_eq!(parse_sha256sum_for(&text, "没有这个文件.zip"), None);
        // 二进制模式写的 `*name` 也要认
        assert_eq!(
            parse_sha256sum_for(&format!("{b} *f.zip\n"), "f.zip").as_deref(),
            Some(b.as_str()),
        );
        // 长度不对的 hex 不能认
        assert_eq!(parse_sha256sum_for("deadbeef  f.zip\n", "f.zip"), None);
    }

    /// 自更新脚本：必须等旧进程退出、用 ditto、替换目标并重启。
    ///
    /// 这段字符串会以用户的权限在用户机器上跑，每个关键动作都钉住。
    #[test]
    fn self_update_script_waits_swaps_and_relaunches() {
        let s = self_update_script(
            4321,
            Path::new("/tmp/stage/XrayTun.app"),
            Path::new("/Applications/XrayTun.app"),
            Path::new("/tmp/stage"),
        );
        assert!(s.contains("kill -0 4321"), "必须先等旧进程退出：{s}");
        assert!(s.contains("/usr/bin/ditto"), "必须用 ditto 保留符号链接：{s}");
        assert!(s.contains("'/Applications/XrayTun.app'"), "目标路径要引号包住：{s}");
        assert!(s.contains("'/tmp/stage/XrayTun.app'"), "{s}");
        assert!(
            s.contains("/usr/bin/open '/Applications/XrayTun.app'"),
            "最后要重启：{s}"
        );
        assert!(
            s.contains("com.apple.quarantine"),
            "新包要清隔离标记，否则首次打开被 Gatekeeper 拦：{s}",
        );
        // 删除目标在前、拷贝在后：中途失败不能只剩半个 bundle
        let rm = s.find("rm -rf '/Applications/XrayTun.app'").expect("要有删除");
        let cp = s.find("/usr/bin/ditto '/tmp/stage/XrayTun.app'").expect("要有拷贝");
        assert!(rm < cp, "必须先删后拷：{s}");
    }

    /// 路径里有单引号也不能把脚本写坏（引号必须转义）。
    #[test]
    fn self_update_script_escapes_quotes() {
        let s = self_update_script(
            1,
            Path::new("/tmp/a'b/XrayTun.app"),
            Path::new("/Applications/XrayTun.app"),
            Path::new("/tmp/a'b"),
        );
        assert!(s.contains(r"'/tmp/a'\''b/XrayTun.app'"), "单引号要转义：{s}");
    }

    /// 403 和 404 必须给出**不同的**指引。
    ///
    /// 仓库从私有改成公开之后这条尤其重要：私有期间报 404（要去开权限或填
    /// token），公开之后被限流报 403（等一会儿，或填 token 提额度）。
    /// 两者压成同一句「检查更新失败」，用户只会以为整个功能坏了。
    #[test]
    fn gh_status_errors_point_at_the_right_cause() {
        let m = |c| gh_status_error("检查", c).to_string();
        assert!(m(403).contains("限流"), "{}", m(403));
        assert!(m(404).contains("token"), "{}", m(404));
        assert!(m(401).contains("token 无效"), "{}", m(401));
        assert_ne!(m(403), m(404), "限流和不存在的处置不同，不能是同一句话");
    }

    /// token 为空等于不认证（公开仓库照常工作）。
    #[test]
    fn auth_args_are_optional() {
        assert!(auth_args(None).is_empty());
        assert!(auth_args(Some("")).is_empty(), "空串不算 token");
        assert!(auth_args(Some("   ")).is_empty(), "空白也不算");
        let a = auth_args(Some(" ghp_x "));
        assert_eq!(a.len(), 2);
        assert_eq!(a[0], "-H");
        assert_eq!(a[1], "Authorization: Bearer ghp_x", "两头空白要去掉");
    }
}
