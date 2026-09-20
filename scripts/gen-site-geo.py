#!/usr/bin/env python3
"""生成 GEO / AI 索引产物（task-19 第 3 步）：robots.txt · sitemap.xml · llms.txt · llms-full.txt

为什么用脚本生成 llms-full.txt：它的要求是「与网页**等价**，不是摘要」。
手抄一定会随页面改动而过期（那就变成「AI 看到的是旧内容」）。
这里**直接把页面 HTML 转成 Markdown**，所以等价性由机制保证。

用法：python3 gen_geo.py
"""
import html
import re
from pathlib import Path

# 脚本住在 <repo>/scripts/ 下，所以 site/ 在上一级。
# （早先它在仓库外的 .scratch/ 里，那时是 parents[1]/"xray-tun"/"site" ——
#  那样脚本不可持续：.scratch/ 是 gitignore 的，别人拿不到它。）
SITE = Path(__file__).resolve().parents[1] / "site"
# 站点绝对基址：**与 gen-site-jsonld.py 的 BASE 必须一致**。
# 域名迁移时两处一起改（生成器里各只有一处；产物由脚本重写，别手改产物）。
BASE = "https://xraytun.top"
LAST_PUB = "2026-09-20"
VERSION = "0.8.28"
DL = f"https://github.com/harodggg/xrayTun/releases/download/v{VERSION}"

# 发行资产：**文件名由 VERSION 派生**，字节数取自 `gh release view v{VERSION}` 的**真实值**
# （不许沿用上一版、不许估算 —— 本项目红线）。
# ⚠️ 取整陷阱：dmg 47,145,126 B = 44.9611 MiB，站点写的是一位小数 → **45.0**，不是 44.9。
DMG = f"XrayTun_{VERSION}_x86_64_arm64.dmg"
ZIP = f"XrayTun_{VERSION}_x86_64_arm64.zip"
DMG_BYTES, DMG_MIB = "47,145,126", "45.0"
ZIP_BYTES, ZIP_MIB = "42,647,148", "40.7"
SHA_BYTES = "200"

# robots.txt 的两组 UA —— **与 Cloudflare 托管段逐条对齐**（原因见 write_robots 的注释）。
#
# 检索与引用类：CF 托管段没有单列它们（落到它的 `User-agent: *` → Allow），
# 所以这里显式放行只是把取向写清楚，不构成矛盾；
# OAI-SearchBot 与 PerplexityBot 在 CF 段本身就是显式 Allow。
CITATION_BOTS = [
    "Googlebot",
    "Bingbot",
    "OAI-SearchBot",
    "PerplexityBot",
    "DuckDuckBot",
]

# 训练 / 批量抓取类：**逐条取自 CF 托管段的 `Disallow: /`**。
# 快照来源：2026-09-20 从 https://xraytun.top/robots.txt 抓下的 3259 字节托管段
# （线上共 3984 B，其中 725 B 是本文件），解析出 45 个被 CF 单独 Disallow 的 UA。
#
# 为什么要在我们这份文件里列全：GitHub Pages 镜像（harodggg.github.io/xrayTun）
# 与 *.pages.dev 上**没有 CF 注入**，我们这份文件就是完整策略 ——
# 不列全就会在这些镜像上把训练爬虫放行（改之前正是如此：线上 10 个 UA 一边 Disallow
# 一边 Allow，等于我们替训练爬虫开脱）。
#
# CF 更新名单后需要重新抓取同步；快照日期见下。
CF_SNAPSHOT = "2026-09-20"
TRAINING_BOTS = [
    "Amazonbot",
    "Amzn-User",
    "Applebot-Extended",
    "AwarioRssBot",
    "Baiduspider",
    "BorderxBot",
    "Bytespider",
    "CCBot",
    "ChatGPT-User",
    "ChathiveCrawler",
    "CitibotSiteCrawler",
    "Claude-User",
    "Claude-Web",
    "ClaudeBot",
    "Cotoyogi",
    "Diffbot",
    "FireCrawl",
    "FirecrawlAgent",
    "FishBot",
    "GPTBot",
    "Google-Agent",
    "Google-CloudVertexBot",
    "Google-Extended",
    "Google-NotebookLM",
    "GoogleOther",
    "ICC-Crawler",
    "Instapaper",
    "Kimi-User",
    "KimiBot",
    "MistralAI-Training",
    "MistralAI-User",
    "NavuBot",
    "Perplexity-User",
    "PetalBot",
    "QualifiedBot",
    "Retool",
    "SemrushBot-SWA",
    "WARDBot",
    "anthropic-ai",
    "atlassian-bot",
    "cohere-ai",
    "magpie-crawler",
    "meta-externalagent",
    "meta-externalfetcher",
    "omgili",
]

# 刻意**不在本文件里单列** Baiduspider（它在上面那份 CF 快照里）。
# 理由：用户希望放行百度，而 CF 托管段对它单独 Disallow。本文件保持沉默 →
# 我们这份落到 `User-agent: *` = Allow（GitHub 镜像上百度可抓），
# 而 canonical 域名上 CF 的更具体规则仍然生效 —— 两边都不矛盾。
# 要让 canonical 域名也放行百度，只能在 CF 关闭 managed robots.txt，代价是失去 CF 对训练爬虫的
# 拦截；**该决定待 lead 与用户确认，本文件不替用户做这个选择**。
BAIDU_NOT_LISTED = "Baiduspider"


def inline(s: str) -> str:
    """行内标签 → Markdown（链接、code、strong/em）。"""
    s = re.sub(r"<code[^>]*>([\s\S]*?)</code>", lambda m: "`" + html.unescape(re.sub(r"<[^>]+>", "", m.group(1))).strip() + "`", s)
    s = re.sub(r'<a[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>', lambda m: f"[{re.sub(r'<[^>]+>', '', m.group(2)).strip()}]({m.group(1)})", s)
    s = re.sub(r"<(strong|b)[^>]*>([\s\S]*?)</\1>", lambda m: "**" + m.group(2).strip() + "**", s)
    s = re.sub(r"<(em|i)[^>]*>([\s\S]*?)</\1>", lambda m: "*" + m.group(2).strip() + "*", s)
    s = re.sub(r"<[^>]+>", "", s)
    return re.sub(r"\s+", " ", html.unescape(s)).strip()


def html_to_md(src: str, lang: str) -> str:
    """把页面正文（main 之后）转成 Markdown。结构保留：h1/h2/h3/ul/pre/table。"""
    body = src[src.index("<main") : src.index("</main>")]
    out: list[str] = []
    # 按块切：pre 优先（里面不能当普通标签处理）
    tokens = re.split(r"(<pre[\s\S]*?</pre>|<h1[\s\S]*?</h1>|<h2[\s\S]*?</h2>|<h3[\s\S]*?</h3>|<ul[\s\S]*?</ul>|<table[\s\S]*?</table>|<p[\s\S]*?</p>)", body)
    for tok in tokens:
        t = tok.strip()
        if not t:
            continue
        if t.startswith("<pre"):
            code = re.sub(r"<[^>]+>", "", t).strip()
            code = html.unescape(code)
            out.append("```bash\n" + code + "\n```")
        elif t.startswith("<h1"):
            out.append("# " + inline(t))
        elif t.startswith("<h2"):
            out.append("## " + inline(t))
        elif t.startswith("<h3"):
            out.append("### " + inline(t))
        elif t.startswith("<ul"):
            for li in re.findall(r"<li[^>]*>([\s\S]*?)</li>", t):
                out.append("- " + inline(li))
        elif t.startswith("<table"):
            rows = re.findall(r"<tr[^>]*>([\s\S]*?)</tr>", t)
            for i, r in enumerate(rows):
                cells = [inline(c) for c in re.findall(r"<t[hd][^>]*>([\s\S]*?)</t[hd]>", r)]
                if not cells:
                    continue
                out.append("| " + " | ".join(cells) + " |")
                if i == 0:
                    out.append("|" + "|".join(["---"] * len(cells)) + "|")
        else:
            txt = inline(t)
            if txt:
                out.append(txt)
    md = "\n\n".join(out)
    return re.sub(r"\n{3,}", "\n\n", md).strip()


def write_robots() -> None:
    """robots.txt：**本站自己完整表达策略**（镜像站上没有任何注入兜底）。

    ## 为什么必须与 Cloudflare 托管段逐条对齐

    canonical 域名 `xraytun.top` 由 Cloudflare 托管，它会把托管策略**注入在我们这份文件之前**
    （2026-09-20 实测：线上 3984 字节 = CF 托管段 3259 B + 本文件 725 B；
    GitHub Pages 镜像与 `*.pages.dev` 上只有本文件）。
    同一 UA、同一路径，两段一边 Allow 一边 Disallow 时**结果取决于抓取实现** ——
    改之前线上就有 **10 个 UA 处于这种矛盾状态**（Applebot-Extended / Bytespider / CCBot /
    ChatGPT-User / Claude-Web / ClaudeBot / GPTBot / Google-Extended / anthropic-ai /
    meta-externalagent），等于我们这份文件在替训练爬虫开脱。现在按 CF 快照逐条对齐。

    ## CF managed robots.txt 不支持按爬虫例外

    已查证 Cloudflare 官方文档：整个功能只有一个总开关
    （Security Settings → Bot traffic → *Set your preference to block training in robots.txt*），
    **没有**「把某个爬虫从名单里拿掉」这个选项；而且它是 prepend。

    ## Baiduspider（已裁决：保持 CF managed，本文件不单列）

    用户已裁决（选 A）：**保持 Cloudflare managed robots.txt 开启**，本文件与托管段逐条对齐。
    因此 Baiduspider 继续被 CF 托管段拦截 —— 这是该裁决的**已知限制（known limitation）**，
    不是本文件的遗漏。本文件不单列它（见 BAIDU_NOT_LISTED 的注释）。
    **不要在这里写「已放行百度」** —— 那是不成立的断言；唯一放行办法是在 CF 关闭 managed
    robots.txt，代价是失去 CF 对训练爬虫的拦截。

    ## Content-Signal

    本文件**不写** `Content-Signal:` 行：CF 托管段已经给了 content signals，两边都写可能不一致。
    """
    lines = [
        "# XrayTun 官网 robots.txt —— 由 scripts/gen-site-geo.py 生成；别手改（重跑会覆盖）。",
        "#",
        f"# 本文件与 Cloudflare 托管段**逐条对齐**（快照 {CF_SNAPSHOT}）。原因：",
        "#   canonical 域名 xraytun.top 由 Cloudflare 托管，其托管策略会**注入在本文件之前**：",
        "#   线上实测 3984 B = CF 托管段 3259 B + 本文件 725 B（镜像站上只有本文件）。",
        "#   同一 UA 两边一边 Allow 一边 Disallow 时，谁生效取决于抓取实现 ——",
        "#   对齐之前线上有 10 个 UA 处于这种矛盾状态，本文件事实上在替训练爬虫开脱。",
        "#",
        "# 你在线上文件里看到本行之前的托管段，那是平台注入，**不是文件被篡改**。",
        "#",
        "# Cloudflare managed robots.txt **不支持按爬虫例外**（已查证官方文档：只有",
        "# 「block training」一个总开关，且是 prepend），所以本文件不能替某个爬虫开例外。",
        "#",
        "# Baiduspider：本文件**刻意不单列**它 —— known limitation，不是本文件的遗漏。",
        "#   用户已裁决（选 A）：保持 CF managed robots.txt 开启，本文件与托管段逐条对齐；",
        "#   因此 Baiduspider 继续被 CF 托管段拦截，这是该裁决的已知代价。",
        "#   不单列时我们这份落到 `User-agent: *` = Allow（GitHub 镜像与 pages.dev 上百度可抓），",
        "#   canonical 域名上 CF 的更具体规则生效。唯一放行办法是在 CF 关闭 managed robots.txt，",
        "#   代价是失去 CF 对训练爬虫的拦截。**不要写成「已放行百度」。**",
        "#",
        "# 本文件不写 Content-Signal 行（CF 托管段已提供，两边都写可能不一致）。",
        "",
        "User-agent: *",
        "Allow: /",
        "",
        "# ---- 检索与引用类：显式放行（CF 托管段未单列它们，落到其 `*` = Allow，故无矛盾）----",
    ]
    for bot in CITATION_BOTS:
        lines += [f"User-agent: {bot}", "Allow: /", ""]
    lines += ["# ---- 训练 / 批量抓取类：显式禁止（逐条取自 CF 托管段；镜像站上这就是全部策略）----"]
    for bot in TRAINING_BOTS:
        if bot == BAIDU_NOT_LISTED:
            continue  # 见 BAIDU_NOT_LISTED 的注释：保持沉默，避免与 CF 打架
        lines += [f"User-agent: {bot}", "Disallow: /", ""]
    lines += [f"Sitemap: {BASE}/sitemap.xml", ""]
    (SITE / "robots.txt").write_text("\n".join(lines), encoding="utf-8")
    print(
        f"  写出 robots.txt：`*` 放行 + 显式放行 {len(CITATION_BOTS)} 个检索/引用类 + "
        f"显式禁止 {len(TRAINING_BOTS) - 1} 个训练/抓取类（Baiduspider 按注释刻意不列）"
    )


def write_sitemap() -> None:
    # 每个「逻辑页面」一对中英路径 + 各自优先级。
    #
    # hreflang 必须指向**同一逻辑页面**的中英版本 —— 早先这里写死了
    # `{BASE}/` 与 `{BASE}/en/`，加子页面时 `/wasm/` 的 alternate 会错误地
    # 指向首页（那会让搜索引擎把子页面当成首页的副本）。
    pages = [
        ("/", "/en/", "1.0", "0.9"),
        ("/wasm/", "/en/wasm/", "0.8", "0.7"),
    ]

    def url(zh_path: str, en_path: str, priority: str, self_lang: str) -> str:
        alts = "".join(
            f'\n    <xhtml:link rel="alternate" hreflang="{h}" href="{BASE}{u}"/>'
            for h, u in (("zh-Hans", zh_path), ("en", en_path), ("x-default", zh_path))
        )
        loc = zh_path if self_lang == "zh-Hans" else en_path
        return (
            "  <url>\n"
            f"    <loc>{BASE}{loc}</loc>\n"
            f"    <lastmod>{LAST_PUB}</lastmod>\n"
            "    <changefreq>weekly</changefreq>\n"
            f"    <priority>{priority}</priority>{alts}\n"
            "  </url>"
        )

    entries = []
    for zh_path, en_path, zh_pri, en_pri in pages:
        entries.append(url(zh_path, en_path, zh_pri, "zh-Hans"))
        entries.append(url(zh_path, en_path, en_pri, "en"))

    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"\n'
        '        xmlns:xhtml="http://www.w3.org/1999/xhtml">\n'
        + "\n".join(entries)
        + "\n</urlset>\n"
    )
    (SITE / "sitemap.xml").write_text(xml, encoding="utf-8")
    print(f"  写出 sitemap.xml：{len(entries)} 个 URL（按逻辑页面成对、各自三向 hreflang）")


def write_llms() -> None:
    txt = f"""# XrayTun

> XrayTun 是 macOS 13.0+ 的 Xray 图形客户端，用 Xray-core 原生 TUN 入站接管系统流量（整机按规则走代理）。
> 当前版本 v{VERSION}（{LAST_PUB}），通用包（Apple Silicon + Intel），**包内自带 Xray 核心**。
> 安装包是 ad-hoc 签名、**未公证**的，首次打开会被 Gatekeeper 拦截，需按系统版本手动放行。
> 仅支持 macOS；没有 Windows / Linux / 移动端版本。本页所有断言都可在仓库文档里逐条核对。

## 官网

- [中文站（完整正文）]({BASE}/)：是什么、解决什么问题、核心能力与刻意不做的边界、下载、安装、FAQ、链接
- [English site (equivalent content)]({BASE}/en/)：与中文站逐段等价，不是半份翻译
- [本站全文（供一次性摄取）]({BASE}/llms-full.txt)：官网所有页面的**完整正文** + 事实与边界清单；**不含**仓库 `docs/` 下的全量设计文档（那是另一处，见下）

## 相关项目：xray-wasm（**独立项目，XrayTun 不使用它**）

- [xray-wasm 页面（中文）]({BASE}/wasm/)：把 Xray 的 VLESS + XTLS-Vision + REALITY 协议栈用**纯 Rust** 重写，
  编译成 **wasm32-wasip2**，在 **wasmtime** 下跑真实 TCP 代理；客户端与服务端两个方向都已实现
- [xray-wasm page (English)]({BASE}/en/wasm/)：与中文页逐段等价
- [xray-wasm 仓库](https://github.com/harodggg/xray-wasm)（**另一个仓库**，不是本仓库的子目录）

## 下载

- [{DMG}]({DL}/{DMG})：{DMG_BYTES} 字节（{DMG_MIB} MiB），主下载
- [{ZIP}]({DL}/{ZIP})：{ZIP_BYTES} 字节（{ZIP_MIB} MiB），备用
- [SHA256SUMS.txt]({DL}/SHA256SUMS.txt)：校验和（{SHA_BYTES} 字节）
- [所有版本](https://github.com/harodggg/xrayTun/releases/latest)

## 安装（要点）

安装包 **ad-hoc 签名、未公证**，首次打开会被 macOS 拦截 —— 这是预期行为，不是文件损坏。
三条放行路径（任选一条，完整说明见官网 `#install`）：

- **macOS 15 及以后**：系统设置 → 隐私与安全性 → 「仍要打开」→ 再双击一次
- **macOS 14 及更早**：Finder 里 Control 点按（右键）应用 → 「打开」→ 再点一次「打开」
- **终端（全版本可用）**：`xattr -d com.apple.quarantine /Applications/XrayTun.app`
  —— 注意是 `-d`，**不是** `-dr`（现代 macOS 的 `xattr` 没有 `-r`，会报 `option -r not recognized`）

TUN 模式还需要在应用内安装一次特权 helper（要求一次管理员密码）。

## 文档与源码

- [源码仓库](https://github.com/harodggg/xrayTun)
- [更新记录](https://github.com/harodggg/xrayTun/blob/main/CHANGELOG.md)
- [设计文档目录](https://github.com/harodggg/xrayTun/tree/main/docs)
- [安装与 TUN 权限说明](https://github.com/harodggg/xrayTun/blob/main/docs/02-tun-and-privileges.md)
- [分流与 DNS 说明](https://github.com/harodggg/xrayTun/blob/main/docs/04-routing-and-dns.md)

## 事实与边界（引用时请保留）

- 支持 macOS 13.0+；通用包（arm64 + x86_64）；**不需要自己安装 Xray 核心**（包内含 Xray-core 与 geoip/geosite 数据）。
- 节点协议：vmess / vless / trojan / shadowsocks / socks / http；**不支持 ShadowsocksR（`ssr://`）**。
- 订阅：Xray JSON / Clash-Mihomo YAML / base64 链接列表 / 明文链接列表，自动识别。
- 分流：4 个预设 + 自定义规则（追加在预设之后）；规则顺序敏感；支持 geoip/geosite。
- DNS：4 种策略；Fake-IP 可选（默认关闭）。
- **「系统代理」模式不修改 macOS 的系统代理设置** —— 只启动本机 SOCKS5(10808)/HTTP(10809)。
- 自动恢复：看门狗每 10 秒探测，连续 2 次失败自动重建隧道；重建失败退回直连。
- 自更新：比对 `SHA256SUMS.txt`，**只校验 SHA256、没有签名校验**。
- **拿不到的（上游没有，不要替它推算）**：每条连接的字节数与持续时间；`dns-out`（UDP 出站）与 `api`（本机回环）的字节计数恒为 0，那是统计盲区。
- 「最近连接」的域名是**时序配对**的近似值（可能配错，界面标 `*`）；约一半连接本来就没有域名。
- 许可证：XrayTun 以 **MIT** 发布（仓库有 `LICENSE`，`Cargo.toml` 亦声明 `license = "MIT"`）；随包分发的 Xray-core 是 MPL-2.0。
- 仅 macOS；界面目前只有中文。
"""
    (SITE / "llms.txt").write_text(txt, encoding="utf-8")
    print("  写出 llms.txt：H1 + blockquote 摘要 + 6 个分节（绝对 URL）")


def write_llms_full() -> None:
    """把所有官网页面转成 Markdown 拼起来 —— 等价性由「直接转换」保证，不手抄。"""
    pages = [
        ("中文：XrayTun 主页面", "/", SITE / "index.html", "zh"),
        ("中文：xray-wasm（同一作者的另一个项目）", "/wasm/", SITE / "wasm" / "index.html", "zh"),
        ("English: XrayTun home", "/en/", SITE / "en" / "index.html", "en"),
        ("English: xray-wasm (a separate project by the same author)", "/en/wasm/",
         SITE / "en" / "wasm" / "index.html", "en"),
    ]

    bodies = []
    for title, path, file, lang in pages:
        text = html_to_md(file.read_text(encoding="utf-8"), lang)
        bodies.append(f"# {title}\n\n来源：{BASE}{path}\n\n{text}")

    total_zh = sum(len(b) for (t, _, _, lg), b in zip(pages, bodies) if lg == "zh")
    total_en = sum(len(b) for (t, _, _, lg), b in zip(pages, bodies) if lg == "en")

    header = f"""# XrayTun — 全文（llms-full.txt）

> 站点：{BASE}/ ｜ 版本：v{VERSION}（{LAST_PUB}）｜ 生成方式：由页面 HTML 直接转换，
> 因此与网页**等价**（不是摘要）。改页面文案后应重新生成，避免 AI 读到旧内容。
> 结构化数据见各页面 `<head>` 里的 JSON-LD：软件块（XrayTun 页面是 SoftwareApplication，
> xray-wasm 页面是 SoftwareSourceCode）+ FAQPage + WebSite；`/wasm/` 与 `/en/wasm/`
> 另有 BreadcrumbList。
>
> **完整性说明（重要，别把 "full" 读成全量文档）**：本文件包含的是**官网所有页面的完整正文**
> 与官网里的「事实与边界」清单。仓库 `docs/` 下的**完整设计文档（数千行规范）没有逐字复制**
> 到这里 —— 那样既膨胀、又必然与仓库文档不同步，反而更糟。需要细节请看绝对链接：
> <https://github.com/harodggg/xrayTun/tree/main/docs>
> （安装与 TUN 权限：docs/02-tun-and-privileges.md；分流与 DNS：docs/04-routing-and-dns.md）
>
> **`xray-wasm` 的仓库是另一个**：<https://github.com/harodggg/xray-wasm>
> 它是同一作者的**独立项目**（纯 Rust 实现 VLESS + XTLS-Vision + REALITY，编译到 wasm32-wasip2），
> **XrayTun 并不使用它** —— XrayTun 随包附带的是官方 Go 版 Xray-core。

收录页面（{len(pages)} 个，中英各 2）：

{chr(10).join(f"- {t} → {BASE}{p}" for t, p, _, _ in pages)}

---

{chr(10).join("\n---\n\n" + b for b in bodies)}
"""
    (SITE / "llms-full.txt").write_text(header, encoding="utf-8")
    print(f"  写出 llms-full.txt：{len(pages)} 个页面（中文 {total_zh} 字 + 英文 {total_en} 字，由 HTML 直接转换）")


def main() -> int:
    print("生成 GEO / AI 索引产物：")
    write_robots()
    write_sitemap()
    write_llms()
    write_llms_full()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
