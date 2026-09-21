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
use crate::util::now_unix;

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

/// 下载超时。核心约 20MB、geo 约 30MB、客户端包约 42MB。
///
/// **这是整个 curl 进程的总预算，不是单次尝试的预算。** 它必须容下
/// 「被断流 → 重试 → 从断点继续」这几次尝试加起来的时间，否则重试会挤掉自己：
/// 实测 300 秒时，客户端包在慢链路上单次就要 140 秒，重试几次必然撞上超时
/// （curl 退出码 28），于是**越重试越失败**。
///
/// 15 分钟的依据：按实测最差 ~300KB/s 算，42MB 约需 140 秒，留出 4 次完整
/// 尝试的余量；且每次失败都从断点继续，实际远用不满。真正的「服务器无响应」
/// 由 API 那一档 30 秒兜住，不会让用户干等。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(900);
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
    /// 字节数。GitHub 的 assets 里一直有，用来画下载进度条。
    #[serde(default)]
    size: u64,
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
    /// 产物字节数。用来画下载进度条；`None` 表示上游没报（那就画不出百分比）。
    #[serde(default)]
    pub size: Option<u64>,
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
                size: Some(asset.size).filter(|n| *n > 0),
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

/// 取 GitHub API 的 JSON。三个上游共用一份 curl 参数。
///
/// 返回正文和 HTTP 状态码。**必须把状态码带出来**：匿名访问时
/// 「403 限流」和「404 不存在」是完全不同的两回事，而 curl 的 `-f`
/// 会把它们压成同一句「退出码 22」。
fn gh_get(url: &str, proxy: Option<u16>) -> Result<(String, u16)> {
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
        403 => "GitHub 限流了（匿名每小时只有 60 次，且按 IP 算）—— 过一会儿再试",
        404 => "仓库或 release 不存在",
        _ => "GitHub 返回了非预期状态",
    };
    Error::Update(format!("{what} 失败（HTTP {code}）：{hint}"))
}

/// 取某个仓库的 release 列表（原始结构）。
fn fetch_raw(repo: &str, per_page: usize, proxy: Option<u16>) -> Result<Vec<GhRelease>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={per_page}");
    let (body, code) = gh_get(&url, proxy)?;
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
    let releases = fetch_raw(repo, per_page, proxy)?;

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
                size: Some(a.size).filter(|n| *n > 0),
            });
        }
    }
    Ok(out)
}

/// 查核心的最新版。
pub fn check_core(proxy: Option<u16>) -> Result<Available> {
    let releases = fetch_raw(XRAY_REPO, 30, proxy)?;
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
///
/// 仓库是**公开**的，所以匿名就能查，不再需要 token（也刻意不提供 ——
/// 一个存着只读凭据的配置字段，收益远小于它带来的风险）。
/// 代价是匿名配额只有 60 次/小时且**按 IP** 算，而请求多经节点出去，
/// 所以「限流」是会真实发生的 —— 因此 403 必须和 404 分开报，见
/// [`gh_status_error`]。
pub fn check_app(proxy: Option<u16>) -> Result<Available> {
    let releases = fetch_raw(APP_REPO, 30, proxy)?;
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
        size: Some(asset.size).filter(|n| *n > 0),
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
    let releases = fetch_raw(GEO_REPO, 5, proxy)?;
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
        size: Some(asset.size).filter(|n| *n > 0),
    })
}

/// 下载到文件。
pub fn download(url: &str, dest: &Path, proxy: Option<u16>) -> Result<()> {
    download_with_progress(url, dest, proxy, |_| {})
}

/// 下载到文件，并在过程中反复报告**已写入的字节数**（给进度条用）。
///
/// 为什么自己轮询而不是解析 curl 的进度输出：`--progress-bar` 把进度写到
/// stderr，格式带 `\r` 且随版本变，解析它比读文件大小脆得多。curl 是流式
/// 写入的，所以「目标文件现在多大」就是「已经下了多少」—— 准确且不依赖输出格式。
///
/// 这是个阻塞函数（内部 sleep + 轮询子进程），调用方要放在
/// `spawn_blocking` 里。
///
/// # 抗断流（0.8.1 修）
///
/// 实测在 ~300KB/s 的链路上，42MB 的包下到 30% 会碰到
/// 「接收数据时连接被重置」。原来的参数对此**完全无能为力**：
/// 单独的 `--retry N` 只覆盖超时与 5xx，连接重置（curl 退出码 56）不在其中；
/// 而且没有断点续传，重试也是从零开始。
///
/// 现在：`--retry-all-errors` 让**所有**错误都触发重试，`-C -` 让每次重试
/// 从已下载的字节接着来。于是「网线抖一下」不再等于「重来一遍」。
///
/// 为了不让 `-C -` 变成隐患，函数开头会**删掉目标文件**：这样续传只可能
/// 发生在同一次调用内的重试之间，不会把上一次失败留下的半成品
/// 与这一次的响应拼在一起（那种拼接会产出坏文件）。
pub fn download_with_progress(
    url: &str,
    dest: &Path,
    proxy: Option<u16>,
    mut on_progress: impl FnMut(u64),
) -> Result<()> {
    // 从干净状态开始：见上面关于 `-C -` 的说明。
    let _ = std::fs::remove_file(dest);

    let mut args = vec![
        "-sSL".to_string(),
        "-f".to_string(),
        // 断点续传：curl 自己会从目标文件的现有长度接着下。
        // 42MB 的包在 ~300KB/s 的链路上要 140 秒，中途被重置很常见，
        // 没有它就得每次从头再来。
        "-C".to_string(),
        "-".to_string(),
        // 只加 `--retry` 是不够的：curl 默认**只重试有限的几种错误**
        // （超时、5xx），而「接收数据时连接被重置」（退出码 56）
        // 不在其中 —— 实测就是这样失败的：下到 30% 直接放弃，
        // 重试形同虚设。`--retry-all-errors` 才让它对所有错误重试。
        "--retry".to_string(),
        "3".to_string(),
        "--retry-all-errors".to_string(),
        "--retry-delay".to_string(),
        "1".to_string(),
        "--max-time".to_string(),
        DOWNLOAD_TIMEOUT.as_secs().to_string(),
        "-o".to_string(),
        dest.display().to_string(),
    ];
    args.extend(proxy_args(proxy));
    args.push(url.to_string());

    let mut child = Command::new("/usr/bin/curl")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Update(format!("执行 curl 失败：{e}")))?;

    let done_bytes = |dest: &Path| std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                on_progress(done_bytes(dest));
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => break Err(Error::Update(format!("等待 curl 失败：{e}"))),
        }
    };

    let status = outcome?;
    // 收尾再报一次：文件刚好在最后一次轮询之后写完的情况很常见。
    on_progress(done_bytes(dest));
    if !status.success() {
        return Err(Error::Update(format!(
            "下载失败（curl 退出码 {:?}）：{url}",
            status.code()
        )));
    }
    Ok(())
}

/// 取一段文本（校验文件用）。
pub fn fetch_text(url: &str, proxy: Option<u16>) -> Result<String> {
    let mut args = vec![
        "-sSL".to_string(),
        "-f".to_string(),
        "--max-time".to_string(),
        API_TIMEOUT.as_secs().to_string(),
    ];
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
        // **只比文件名，不比整行里的路径。**
        //
        // 我们的 `SHA256SUMS.txt` 是 `shasum -a 256 ./*` 生成的，所以每一行
        // 长这样（实测）：
        //
        //     be1fd342…06cc  ./XrayTun_0.6.2_x86_64_arm64.zip
        //
        // 拿整段 `./XrayTun…zip` 去比 `XrayTun…zip` 永远不相等 —— 症状是
        // 「校验文件里没有 <包名>」，而文件明明就在里面。这里取 path 的
        // 最后一段，`./x`、`dir/x`、`/abs/x` 都能对上；
        // 开头的 `*` 是 sha256sum 的二进制模式标记，也要剥掉。
        let base = Path::new(file.trim_start_matches('*'))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(file);
        if base != name {
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
{quarantine}
# 4) 清理暂存目录后重启
rm -rf {tmp}
/usr/bin/open {target}
"#,
        pid = pid,
        target = sh_quote(target_app),
        src = sh_quote(src_app),
        tmp = sh_quote(tmp_root),
        quarantine = quarantine_cleanup_block(target_app),
    )
}

/// 把一个路径包成 shell 单引号字面量（单引号自身转义）。
fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// `self_update_script` 里「去掉隔离标记 + 读回验证」那一段（第 3 步）。
///
/// # 为什么是 `find … -exec xattr -d … +`
///
/// * **不能用递归开关**：`-r` 的支持**随 macOS 版本与解析到的 xattr 实现而异**。
///   2026-09 在本机（macOS 26.6.2）实测：
///   * `/usr/bin/xattr -dr …` → **exit 0，真的递归删掉了**（这个版本的 Apple
///     xattr 已经支持 `-r`）；
///   * 但 PATH 上先命中的 `/usr/local/bin/xattr` 与
///     `/Library/Frameworks/Python.framework/.../xattr` 是 Python `xattr` 包，
///     它们**没有** `-r` → `option -r not recognized`，**exit 64**；
///   * 更早的 macOS 版本里 `/usr/bin/xattr` 也没有 `-r`（README 记的就是这一条）。
///
///   也就是说「`-dr` 到底行不行」取决于用户机器 —— 而原文是
///   `… 2>/dev/null || true`，失败完全不可见，于是在不支持的环境里隔离标记
///   **从来没被去掉**，用户更新后被 Gatekeeper 拦。`find` 逐文件 + `-d`
///   在所有版本、两种实现上都成立，所以不再赌 `-r`。
/// * **只对 bundle 根路径执行不够**：`com.apple.quarantine` 会落在 bundle 内
///   多个文件上，而 `xattr -d` 是**非递归**的。`find` 本身递归，`-exec … +`
///   把命中项批量传给 xattr（`\;` 会为每个文件起一个进程，上万个文件会极慢）。
/// * 不用 `xattr -c`（清掉该文件**全部**扩展属性）：它同样非递归（还是得靠
///   `find` 逐文件），而且会顺手抹掉 `com.apple.provenance` 这类无关属性 ——
///   我们只想删这一个标记。
///
/// # 退出码语义（实测，决定了判据）
///
/// * 每个路径都带该属性 → 退出码 **0**；
/// * 只要有**一个**路径没有该属性 → 报 `No such xattr`，整条命令退出码 **1**。
///   这是**正常情况**（bundle 里不是每个文件都被打过标记），不是失败。
///
/// 因此**判据是读回的残留数，不是退出码**；退出码与 stderr 仍然原样写进
/// `app-update.log`（此前是 `2>/dev/null || true`，失败完全不可见）。
/// 这一段永远不中断替换：隔离标记清不掉只影响首次打开，包本身已经装好了。
fn quarantine_cleanup_block(target: &Path) -> String {
    let target = sh_quote(target);
    format!(
        r#"# 3) 去掉隔离标记，否则新包第一次打开会被 Gatekeeper 拦。
#
# ⚠️ 不要给 xattr 加「递归开关」：`-r` 的支持随 macOS 版本与 xattr 实现而异
#    （老版本 /usr/bin/xattr 没有它；PATH 上先命中的 Python xattr 也没有），
#    在那些机器上会以 `option -r not recognized`（exit 64）失败 —— 看起来像
#    清掉了。`find` 逐文件 + `-d` 在所有版本上都成立，所以不赌递归开关。
#    bundle 里多个文件都可能带标记，而 `-d` 非递归，所以必须用 find 遍历；
#    用 `+` 批量传参（`\;` 会为每个文件起一个进程，上万个文件会非常慢）。
# 注意：对**没有**该属性的文件，`xattr -d` 会报 `No such xattr` 并让整条命令
#    返回非 0 —— 那是正常情况，不是失败。真正的判据是下面的**读回**。
quarantine_rc=0
quarantine_out=$(find {target} -exec /usr/bin/xattr -d com.apple.quarantine {{}} + 2>&1) || quarantine_rc=$?
if [ "$quarantine_rc" -ne 0 ]; then
  echo "去隔离标记：命令退出码 $quarantine_rc（含「No such xattr」这种正常情况）；输出如下："
  echo "$quarantine_out"
fi
# 3b) 读回验证：数一数还有多少文件带着 com.apple.quarantine
quarantine_read_rc=0
quarantine_list=$(find {target} -exec /usr/bin/xattr -l {{}} + 2>&1) || quarantine_read_rc=$?
quarantine_left=$(printf '%s\n' "$quarantine_list" | grep -c com.apple.quarantine || true)
if [ "$quarantine_left" -eq 0 ] && [ "$quarantine_read_rc" -eq 0 ]; then
  echo "去隔离标记：已确认全部清除（读回 0 个残留）"
elif [ "$quarantine_left" -eq 0 ]; then
  echo "去隔离标记：读回未发现残留，但读回命令退出码 $quarantine_read_rc（目标可能不存在或权限不足），请人工确认"
else
  echo "去隔离标记：**仍有 $quarantine_left 个文件带 com.apple.quarantine**，首次打开可能被 Gatekeeper 拦；不中断安装"
fi
"#
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

    /// **下载前必须清掉目标文件。**
    ///
    /// 这条钉住的是 `-C -`（断点续传）的**安全边界**：续传只允许发生在
    /// 同一次调用内的重试之间。如果目标文件里留着上一次失败的半成品，
    /// curl 会从那个偏移接着写 —— 一旦远端内容变了或偏移对不上，
    /// 拼出来的就是一个坏文件（校验能挡住，但那本该是成功）。
    ///
    /// 用不可达的 URL 触发失败，然后断言目标文件已被清掉（而不是留着旧内容）。
    #[test]
    fn download_starts_from_a_clean_destination() {
        let dir = std::env::temp_dir().join(format!("xt-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("pkg.zip");
        std::fs::write(&dest, b"STALE-PARTIAL-CONTENT").unwrap();

        // 127.0.0.1:1 上没有服务，必然失败
        let r = download_with_progress("http://127.0.0.1:1/nope", &dest, None, |_| {});
        assert!(r.is_err(), "不可达地址应当失败");

        let left = std::fs::read(&dest).unwrap_or_default();
        assert!(
            !left.starts_with(b"STALE-PARTIAL"),
            "旧内容必须被清掉，否则续传会把两次响应拼起来: {:?}",
            String::from_utf8_lossy(&left)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **解压按精确条目名匹配，因此上游包必须是平铺的。**
    ///
    /// 这条同时是 zip slip 的防线，值得写清楚：
    ///
    /// * `unzip_into` 传的是 `-j`（junk paths）+ 精确名。带目录的条目
    ///   （`inner/xray`）**根本匹配不上** `xray`，unzip 以退出码 11 结束，
    ///   调用方拿到的是「解压失败」—— 而不是把目录层级重建到目标目录下。
    /// * 实测确认过：去掉 `-j` 后 `inner/xray` 会变成 `dest/xray`（被压平），
    ///   但换一个条目名就能重新建出子目录，所以 `-j` 仍必须保留。
    ///
    /// 结论：畸形包只会被拒绝，不会写出目标目录之外的东西。
    #[test]
    fn unzip_refuses_archives_that_are_not_flat() {
        let dir = std::env::temp_dir().join(format!("xt-unzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("payload/inner")).unwrap();
        std::fs::write(dir.join("payload/inner/xray"), b"#!/bin/sh\n").unwrap();

        let zip = dir.join("payload.zip");
        let ok = std::process::Command::new("/usr/bin/zip")
            .current_dir(dir.join("payload"))
            .args(["-q", "-X", zip.to_str().unwrap(), "inner/xray"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("跳过：系统没有可用的 zip");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let dest = dir.join("dest");
        let out = unzip_into(&zip, &dest, &["xray"]);
        assert!(out.is_err(), "非平铺的包应当被拒绝，而不是解出一堆层级");

        // 目录里绝不能出现重建出来的层级
        if dest.exists() {
            let entries: Vec<_> = std::fs::read_dir(&dest)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            assert!(
                entries.is_empty(),
                "失败的解压不该留下任何产物，实际: {entries:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 平铺的条目能被正常解出来（这是上游真实的包形状）。
    #[test]
    fn unzip_extracts_flat_entries() {
        let dir = std::env::temp_dir().join(format!("xt-unzip-flat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        std::fs::write(dir.join("payload/xray"), b"core-bytes").unwrap();

        let zip = dir.join("p.zip");
        let ok = std::process::Command::new("/usr/bin/zip")
            .current_dir(dir.join("payload"))
            .args(["-q", "-X", zip.to_str().unwrap(), "xray"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("跳过：系统没有可用的 zip");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let dest = dir.join("dest");
        unzip_into(&zip, &dest, &["xray"]).expect("平铺条目应当解出来");
        assert_eq!(
            std::fs::read(dest.join("xray")).unwrap(),
            b"core-bytes",
            "解出来的内容应当一致"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 归档里的其它条目一律不碰：只解出点名的那几个。
    ///
    /// 上游包里有 LICENSE、README、geo 数据等多个条目。点名解压既省时间，
    /// 也避免把一个我们没打算用的文件（可能是可执行文件）落到托管目录里。
    #[test]
    fn unzip_takes_only_the_named_entries() {
        let dir = std::env::temp_dir().join(format!("xt-unzip-sel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        std::fs::write(dir.join("payload/xray"), b"core").unwrap();
        std::fs::write(dir.join("payload/LICENSE"), b"license text").unwrap();

        let zip = dir.join("p.zip");
        let ok = std::process::Command::new("/usr/bin/zip")
            .current_dir(dir.join("payload"))
            .args(["-q", "-X", zip.to_str().unwrap(), "xray", "LICENSE"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("跳过：系统没有可用的 zip");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let dest = dir.join("dest");
        unzip_into(&zip, &dest, &["xray"]).expect("解压应当成功");
        assert!(dest.join("xray").is_file(), "点名的条目要解出来");
        assert!(
            !dest.join("LICENSE").exists(),
            "没点名的条目不该落到目标目录"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    mut on_progress: impl FnMut(u64),
) -> Result<InstalledMeta> {
    let staging = managed_dir.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| Error::Update(format!("建暂存目录失败：{e}")))?;

    let zip = staging.join("core.zip");
    download_with_progress(&available.download_url, &zip, proxy, &mut on_progress)?;

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
    mut on_progress: impl FnMut(u64),
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
        // geo 是两个文件（geoip + geosite），进度按「每个文件各自从 0 开始」
        // 报 —— 分母由调用方按当前文件大小定，界面上表现为两段。
        download_with_progress(&url, &dest, proxy, &mut on_progress)?;

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

    /// 整包一份的 `SHA256SUMS.txt` 必须**按文件名查行**，而且要认得出真实格式。
    ///
    /// 这段是**线上文件的原文**（v0.6.2 的 `SHA256SUMS.txt`，逐字抄的）：
    ///
    /// ```text
    /// 1c216baa…4c57  ./XrayTun_0.6.2_x86_64_arm64.dmg
    /// be1fd342…06cc  ./XrayTun_0.6.2_x86_64_arm64.zip
    /// ```
    ///
    /// 注意 `./` 前缀 —— 我们的管道是 `shasum -a 256 ./*` 生成的。
    /// 早先的测试手写了一个**没有** `./` 的理想输入，于是单测全绿而真机上的
    /// 客户端自更新**一次都没成功过**，报的是「校验文件里没有 <包名>」。
    /// 教训：测试要用管道真正产出的那份格式，别用手写的理想版。
    #[test]
    fn checksums_are_looked_up_by_file_name() {
        const REAL: &str = "\
1c216baa89fa26b5b05d583acb003a88e011abbd6cdb6b57109df657b5eb4c57  ./XrayTun_0.6.2_x86_64_arm64.dmg
be1fd34274975e55ef96cf804459b952be06e3dc159011c688616f2211b106cc  ./XrayTun_0.6.2_x86_64_arm64.zip
";
        assert_eq!(
            parse_sha256sum_for(REAL, "XrayTun_0.6.2_x86_64_arm64.zip").as_deref(),
            Some("be1fd34274975e55ef96cf804459b952be06e3dc159011c688616f2211b106cc"),
            "必须剥掉 ./ 前缀，并且取 zip 那一行（dmg 排在前面）",
        );
        assert_eq!(
            parse_sha256sum_for(REAL, "XrayTun_0.6.2_x86_64_arm64.dmg").as_deref(),
            Some("1c216baa89fa26b5b05d583acb003a88e011abbd6cdb6b57109df657b5eb4c57"),
        );
        assert_eq!(parse_sha256sum_for(REAL, "没有这个文件.zip"), None);

        // 其它写法也要认：裸名字、子目录、绝对路径、二进制模式的 `*`
        let h = "b".repeat(64);
        for line in [
            format!("{h}  f.zip"),
            format!("{h}  ./f.zip"),
            format!("{h}  dist/f.zip"),
            format!("{h}  /abs/path/f.zip"),
            format!("{h} *f.zip"),
        ] {
            assert_eq!(
                parse_sha256sum_for(&line, "f.zip").as_deref(),
                Some(h.as_str()),
                "认不出这种写法：{line}",
            );
        }
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

    // -----------------------------------------------------------------------
    // 去隔离标记（task-46）
    //
    // 线上真实发生过：脚本里写的是**带递归开关**的 xattr 调用（原文
    // `/usr/bin/xattr -dr … 2>/dev/null || true`）。在不支持 `-r` 的环境里
    // 它会以 `option -r not recognized`（exit 64）失败，而 `2>/dev/null || true`
    // 把失败完全吞掉 —— 隔离标记从来没被去掉，用户更新后被 Gatekeeper 拦，
    // app-update.log 里还没有任何线索。
    //
    // 2026-09 复测补充：`-r` 的支持**随版本/实现而异** —— macOS 26.6.2 的
    // `/usr/bin/xattr -dr` 实测 exit 0 且真的删掉了；但 PATH 上先命中的
    // Python xattr（`/usr/local/bin/xattr`）没有 `-r`（exit 64），更早的
    // macOS 版本里 `/usr/bin/xattr` 也没有。所以修法是**不再赌 `-r`**。
    //
    // 下面四类断言分别钉：静态形态、真实行为、退出码语义、失败必须留痕。
    // -----------------------------------------------------------------------

    /// **静态防回归**：不得再出现 `xattr -dr` / `xattr -cr`，也不得再静默。
    #[test]
    fn self_update_script_never_uses_the_broken_recursive_xattr_flags() {
        let s = self_update_script(
            1,
            Path::new("/tmp/stage/XrayTun.app"),
            Path::new("/Applications/XrayTun.app"),
            Path::new("/tmp/stage"),
        );
        for bad in [
            "xattr -dr",
            "xattr -cr",
            "-dr com.apple.quarantine",
            "-cr com.apple.quarantine",
        ] {
            assert!(
                !s.contains(bad),
                "不得再出现 `{bad}`（xattr 有两个实现，`-r` 的支持随实现与版本而异）：{s}"
            );
        }
        // 必须用与 README / release.yml 一致的那一种写法
        assert!(
            s.contains("-exec /usr/bin/xattr -d com.apple.quarantine {} +"),
            "要用 `find … -exec xattr -d … +`（逐文件、批量、只删这一个属性）：{s}"
        );
        // 失败必须留痕：退出码与 stderr 都要进日志（脚本的 stdout/stderr 已整体重定向到 app-update.log）
        assert!(s.contains("命令退出码 $quarantine_rc"), "失败要记退出码：{s}");
        assert!(s.contains(r#"echo "$quarantine_out""#), "失败要把 stderr 抄进日志：{s}");
        assert!(
            !s.contains("com.apple.quarantine {target} 2>/dev/null"),
            "不能再把 xattr 的 stderr 丢进 /dev/null：{s}"
        );
        // 读回验证
        assert!(s.contains("已确认全部清除"), "要有读回验证并写进日志：{s}");
    }

    /// 读回统计：目标树里还有多少个路径带着 `com.apple.quarantine`。
    #[cfg(target_os = "macos")]
    fn count_quarantine(dir: &Path) -> usize {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "find {} -exec /usr/bin/xattr -l {{}} + 2>/dev/null | grep -c com.apple.quarantine || true",
                sh_quote(dir)
            ))
            .output()
            .expect("跑读回命令");
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
    }

    /// 给一个路径打上隔离标记（测试夹具）。
    #[cfg(target_os = "macos")]
    fn mark_quarantined(p: &Path) {
        let ok = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0083;xraytun-test"])
            .arg(p)
            .status()
            .expect("写 quarantine");
        assert!(ok.success(), "夹具：给 {p:?} 写隔离标记失败");
    }

    /// **行为断言**：所有路径都带标记时，脚本里那条命令真的成功（退出码 0），
    /// 而且标记真的没了。
    #[test]
    #[cfg(target_os = "macos")]
    fn quarantine_command_succeeds_when_all_paths_are_marked() {
        let dir = std::env::temp_dir().join(format!("xt-quarantine-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("Contents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Info.plist"), b"x").unwrap();
        for p in [dir.as_path(), sub.as_path(), sub.join("Info.plist").as_path()] {
            mark_quarantined(p);
        }
        assert_eq!(count_quarantine(&dir), 3, "夹具：应当有 3 个路径带标记");

        // 就是脚本里那一条命令
        let cmd = format!(
            "find {} -exec /usr/bin/xattr -d com.apple.quarantine {{}} +",
            sh_quote(&dir)
        );
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(&cmd).output().unwrap();
        assert!(
            out.status.success(),
            "所有路径都带标记时必须退出 0，实际 {:?}；stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(count_quarantine(&dir), 0, "标记必须真的被删掉（不是「看起来删了」）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **退出码语义（实测）**：真实 bundle 里只有部分文件带标记 →
    /// `xattr -d` 报 `No such xattr` 并让整条命令返回 1，但标记**确实已删掉**。
    ///
    /// 所以判据必须是「读回残留数」，不是退出码；脚本那一段也必须照此报告。
    #[test]
    #[cfg(target_os = "macos")]
    fn quarantine_exit_code_one_is_benign_when_some_files_are_unmarked() {
        let dir = std::env::temp_dir().join(format!("xt-quarantine-mixed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marked = dir.join("marked");
        let plain = dir.join("plain");
        std::fs::write(&marked, b"x").unwrap();
        std::fs::write(&plain, b"x").unwrap();
        mark_quarantined(&marked);

        let cmd = format!(
            "find {} -exec /usr/bin/xattr -d com.apple.quarantine {{}} +",
            sh_quote(&dir)
        );
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(&cmd).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "有未标记文件时实测退出码是 1");
        assert_eq!(count_quarantine(&dir), 0, "尽管退出码是 1，标记必须已经被删掉");

        // 脚本那一段在同样的夹具上要给出「已确认全部清除」（判据=读回）
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -e\n{}", quarantine_cleanup_block(&dir)))
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stdout);
        assert!(log.contains("已确认全部清除"), "读回为 0 时要给出确认：{log}");
        assert!(out.status.success(), "这一段不能在 set -e 下中断替换流程");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **失败必须留痕**：对不存在的目标，日志里要有退出码与真实 stderr，
    /// 而且**不能**给出「已确认全部清除」这种假安慰；同时不中断替换流程。
    #[test]
    fn quarantine_block_logs_failure_instead_of_swallowing_it() {
        let missing =
            std::env::temp_dir().join(format!("xt-quarantine-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);

        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -e\n{}", quarantine_cleanup_block(&missing)))
            .output()
            .unwrap();
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(log.contains("命令退出码"), "失败必须记退出码：{log}");
        assert!(
            log.contains("No such file or directory"),
            "必须把 find 的 stderr 抄进日志（这才是排障线索）：{log}"
        );
        assert!(
            !log.contains("已确认全部清除"),
            "失败时不能给假安慰：{log}"
        );
        assert!(
            out.status.success(),
            "清理失败不该中断替换流程（记录后继续）：{:?}",
            out.status
        );
    }

    /// 403 和 404 必须给出**不同的**指引。
    ///
    /// 两者都表现为「检查更新失败」，但处置完全不同：403 是匿名配额被刷满了
    /// （请求多经节点出去，所以这个额度是整台节点共用的），等一会儿就好；
    /// 404 是仓库/release 真的不存在。压成同一句话，用户只会以为功能坏了。
    #[test]
    fn gh_status_errors_point_at_the_right_cause() {
        let m = |c| gh_status_error("检查", c).to_string();
        assert!(m(403).contains("限流"), "{}", m(403));
        assert!(!m(403).contains("不存在"), "403 不该说成不存在：{}", m(403));
        assert!(m(404).contains("不存在"), "{}", m(404));
        assert_ne!(m(403), m(404), "限流和不存在的处置不同，不能是同一句话");
        // 每一句都要带上状态码，否则用户没法自己查。
        for c in [401u16, 403, 404, 500] {
            assert!(m(c).contains(&c.to_string()), "{}", m(c));
        }
    }

}
