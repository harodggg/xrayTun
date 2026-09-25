#!/usr/bin/env python3
"""生成 GEO / AI 索引产物（task-19 第 3 步）：robots.txt · sitemap.xml · llms.txt · llms-full.txt

为什么用脚本生成 llms-full.txt：它的要求是「与网页**等价**，不是摘要」。
手抄一定会随页面改动而过期（那就变成「AI 看到的是旧内容」）。
这里**直接把页面 HTML 转成 Markdown**，所以等价性由机制保证。

用法：python3 gen_geo.py
"""
import html
import os
import re
import sys
from pathlib import Path

# 脚本住在 <repo>/scripts/ 下，所以 site/ 在上一级。
# （早先它在仓库外的 .scratch/ 里，那时是 parents[1]/"xray-tun"/"site" ——
#  那样脚本不可持续：.scratch/ 是 gitignore 的，别人拿不到它。）
SITE = Path(__file__).resolve().parents[1] / "site"
# 站点绝对基址：**与 gen-site-jsonld.py 的 BASE 必须一致**。
# 域名迁移时两处一起改（生成器里各只有一处；产物由脚本重写，别手改产物）。
BASE = "https://xraytun.top"
LAST_PUB = "2026-09-23"
VERSION = "0.8.38"
DL = f"https://github.com/harodggg/xrayTun/releases/download/v{VERSION}"
RELEASES_PAGE = "https://github.com/harodggg/xrayTun/releases"

# **发布两阶段开关**（发版时按顺序做，避免站点说谎）：
#
#   · 提交 1（版本号 bump 同一次）：把 VERSION 改成新版本，PUBLISHED 仍为 False。
#     此刻新版本的资产**还不存在**，所以：**不给 pinned 直链**（会 404）、
#     **不沿用上一版的字节数**（那是错的），如实写「正在发布，见 Releases 页面」。
#   · 提交 2（`gh release view` 已经能读到 3 个资产之后）：取**真实**字节数，
#     填进下面的 *_BYTES/*_MIB，把 PUBLISHED 置 True，再重跑本脚本。
#
# 为什么要有这个开关：`release.yml` 在打包前跑 `check.sh`，而 check.sh 断言
# 「站点声明的版本 == Cargo.toml 的版本」——先 bump 会让断言失败；而站点要写新版本
# 又需要真实资产。两阶段是唯一「每个瞬间都不说谎」的解法。
PUBLISHED = False

# 发行资产：**文件名由 VERSION 派生**，字节数取自 `gh release view v{VERSION}` 的**真实值**
# （不许沿用上一版、不许估算 —— 本项目红线）。
# ⚠️ 取整陷阱：dmg 某版 44.9611 MiB 要写 **45.0**，不是 44.9。
#
# **未发布时（PUBLISHED=False）这些必须留空** —— 留着上一版的数字是个陷阱：
# 谁把 PUBLISHED 翻成 True 而忘了换数字，站点就会带着**错字节数**上线。
# 下面的断言会把这种失误直接变成构建失败。
DMG = f"XrayTun_{VERSION}_x86_64_arm64.dmg"
ZIP = f"XrayTun_{VERSION}_x86_64_arm64.zip"
DMG_BYTES, DMG_MIB = "", ""
ZIP_BYTES, ZIP_MIB = "", ""
SHA_BYTES = ""
if PUBLISHED and not (DMG_BYTES and DMG_MIB and ZIP_BYTES and ZIP_MIB and SHA_BYTES):
    raise SystemExit(
        "PUBLISHED=True 但 *_BYTES/*_MIB 是空的："
        "请先用 `gh release view v{VERSION} --json assets` 取真实值填上再发布。"
    )

# ---------------------------------------------------------------------------
# 外部条目：**生成器不允许静默抹掉非本产品页面的收录**（task-180）
#
# 真实事故（v0.8.38 停发期间发生两次）：`site/jev-x-filter/**` 是**另一条工作流**的项目页，
# 它的 sitemap / llms 收录是手写加进 `site/{sitemap.xml,llms.txt,llms-full.txt}` 的；
# 而本脚本只认识本产品页面 ⇒ 每次重跑都把那些条目**静默删掉**，对方自己补了两次。
#
# 机制（**不动对方一个可见字符**）：
#   · `sitemap.xml`    —— 按 `<loc>` **归属**保留：`<loc>` 不是本产品页面的 `<url>` 块逐字插回；
#   · `llms.txt`       —— 按 `## ` **节归属**保留：标题不在生成集里的整节逐字插回原位；
#   · `llms-full.txt`  —— 它的外部内容插在**生成文本内部**（头部提示、索引 + 一节），
#                         用**显式标记区间**界定；区间内容逐字插回固定锚点，
#                         索引上方的计数行按「本产品页 + 区间里数出来的外部页对」计算。
#
# **不许静默丢**（这是本机制的重点）：写之前会比对「旧文件里有、新文件里没有、且带**站内非本产品
# 路径**的整行」—— 那只有一种成因：有人把外部条目塞进了**生成区块**且没放进标记区间。
# 这时**非零退出并指名到行**，绝不把它抹掉、也不"顺手保留"（保留等于把别人的内容挪进我们的生成逻辑）。
# `check` 模式另外把「盘上产物 ≠ 生成器重算结果」直接判红（手写进生成区块同样在这里被抓住）。
#
# 覆盖不到的（诚实清单见 docs/verification/GEN-SITE-EXTERNAL.md）：
#   · 第三个外部项目若只改 `llms-full.txt`，**必须自己包一次标记区间**（sitemap / llms.txt 自动）；
#   · 在**生成器区块内**手写的内容会被重写（`check` 会红，但重跑生成器不会保留它）。
#
# 测试缝：`GEN_SITE_EXTERNAL_OFF=1` 关掉本机制（**只给反向敏感性测试用**；等于旧行为）。
# ---------------------------------------------------------------------------
PRESERVE_EXTERNAL = os.environ.get("GEN_SITE_EXTERNAL_OFF") != "1"

# 本产品页面（**单一来源**：sitemap 的归属判定、llms-full 的页面清单、计数行都用它）。
#   (中文路径, 英文路径, 中文 priority, 英文 priority)
PRODUCT_PAGES = [
    ("/", "/en/", "1.0", "0.9"),
    ("/wasm/", "/en/wasm/", "0.8", "0.7"),
]

# `llms-full.txt` 外部内容的**标记区间**：`(区间名, 生成文本里的插入锚点, 插在锚点前/后)`。
# 锚点必须是生成文本里的**唯一**子串；找不到或不唯一 ⇒ 大声失败（绝不静默丢内容）。
EXT_BEGIN = "<!-- BEGIN external:{name}（其它工作流维护：生成器原样保留本区间，不解析、不重排） -->"
EXT_END = "<!-- END external:{name} -->"
# `(区间名, 生成文本里的锚点, 插在锚点前/后, 区间是否连带标记之后的那个换行)`。
# 锚点必须是生成文本里的**唯一**子串；找不到或不唯一 ⇒ 大声失败（绝不静默丢内容）。
EXT_REGIONS = [
    (
        "header",
        "> **XrayTun 并不使用它** —— XrayTun 随包附带的是官方 Go 版 Xray-core。\n",
        "after",
        True,
    ),
    ("body", "[xray-wasm 是什么、怎么用 →](wasm/)\n", "after", False),
]


def own_paths() -> set:
    """本产品页面路径集合（含中英）。"""
    return {p for zh, en, _, _ in PRODUCT_PAGES for p in (zh, en)}


def read_artifact(name: str) -> str:
    """读盘上的产物；不存在返回空串（首次生成时这是正常的）。"""
    p = SITE / name
    return p.read_text(encoding="utf-8") if p.exists() else ""


def ext_region(text: str, name: str, trailing: bool = False) -> str:
    """取出某个标记区间（含标记行本身）；没有就返回空串。

    `trailing=True` 时连带标记之后的那个换行一起取（区间自己"带着"一个尾随空行，
    这样插回原位后与迁移前的逐字节形态一致）。
    """
    if not text:
        return ""
    m = re.search(
        re.escape(EXT_BEGIN.format(name=name))
        + r".*?"
        + re.escape(EXT_END.format(name=name))
        + (r"\n?" if trailing else ""),
        text,
        re.S,
    )
    return m.group(0) if m else ""


def _site_path(url: str) -> str:
    """从站内 URL 取路径（去掉 query/fragment）。"""
    path = url[len(BASE):]
    for sep in ("#", "?"):
        path = path.split(sep, 1)[0]
    return path


def external_url_lines(text: str) -> list:
    """旧文件里带**站内非本产品路径**的行：`[(行号, 原文), …]`（判据见文件头注释）。"""
    own = own_paths()
    out = []
    for i, line in enumerate(text.splitlines(), 1):
        for url in re.findall(r"https?://[^\s)\]<>\"',]+", line):
            if url.startswith(BASE) and _site_path(url) not in own:
                out.append((i, line))
                break
    return out


def lost_external_lines(name: str, old: str, new: str) -> list:
    """旧里有、新里没有、且带站内非本产品路径的行 ⇒ 生成器**必须大声失败**而不是抹掉它。"""
    new_lines = set(new.splitlines())
    return [(n, line) for n, line in external_url_lines(old) if line not in new_lines]


def _norm(line: str) -> str:
    """把数字抹平：用来区分「我们自己的产物过期」与「别人手写的新内容」。"""
    return re.sub(r"\d", "#", line).strip()


def _is_our_url(url: str) -> bool:
    """这个 URL 是不是**本产品自己**的（站点产品路径，或本仓库）。"""
    if url.startswith(BASE):
        return _site_path(url) in own_paths()
    return url.startswith("https://github.com/harodggg/xrayTun")


def lost_external_sections(name: str, old: str, new: str) -> list:
    """旧里有、新里没有的**外部 `## ` 整节**（外部 = 不在生成集、且正文提到非本产品的地址）。

    这一条补住「外部内容不带站内路径」的漏洞：光看行里的站内 URL 抓不到它，
    但一个**成节**的外部内容一定有自己的标题 —— 标题没了就是没了。
    """
    new_lines = set(new.splitlines())
    new_norm = {_norm(l) for l in new_lines}
    out = []
    lines = old.splitlines()
    heads = [(i, l) for i, l in enumerate(lines) if l.startswith("## ")]
    for idx, (i, head) in enumerate(heads):
        if head in new_lines or _norm(head) in new_norm:
            continue
        end = heads[idx + 1][0] if idx + 1 < len(heads) else len(lines)
        urls = [u for l in lines[i:end] for u in re.findall(r"https?://[^\s)\]<>\"',]+", l)]
        if any(not _is_our_url(u) for u in urls):
            out.append((i + 1, head))
    return out


def merge_external_sitemap(new_xml: str, old: str) -> str:
    """把旧文件里**不属于本产品**的 `<url>` 块逐字插回（顺序不变、内容不重写）。"""
    if not (PRESERVE_EXTERNAL and old):
        return new_xml
    own = {f"{BASE}{p}" for p in own_paths()}
    kept = []
    for block in re.findall(r"  <url>.*?</url>", old, re.S):
        loc = re.search(r"<loc>([^<]*)</loc>", block)
        if loc and loc.group(1) not in own:
            kept.append(block)
    if not kept:
        return new_xml
    print(f"  ⟳ 保留 {len(kept)} 条外部 <url>（非本产品页面，逐字不动）")
    return new_xml.replace("</urlset>", "".join(b + "\n" for b in kept) + "</urlset>", 1)


def merge_external_llms(new_txt: str, old: str) -> str:
    """把旧文件里**标题不在生成集里**的 `## ` 整节逐字插回原位。"""
    if not (PRESERVE_EXTERNAL and old):
        return new_txt
    gen_heads = set(re.findall(r"^## .*$", new_txt, re.M))
    heads = [(m.start(), m.group(0)) for m in re.finditer(r"^## .*$", old, re.M)]
    out = new_txt
    inserted = 0
    for i, (pos, head) in enumerate(heads):
        if head in gen_heads:
            continue
        end = heads[i + 1][0] if i + 1 < len(heads) else len(old)
        section = old[pos:end]
        anchor = next((h for _, h in heads[i + 1:] if h in gen_heads), None)
        if anchor is None or out.count(anchor) != 1:
            out = out.rstrip("\n") + "\n\n" + section
        else:
            out = out.replace(anchor, section + anchor, 1)
        inserted += 1
    if inserted:
        print(f"  ⟳ 保留 {inserted} 个外部 `## ` 分节（标题不在生成集内，逐字不动）")
    return out


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


def build_robots() -> str:
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
        "#   线上是「CF 托管段 + 本文件」**拼接**而成（镜像站上只有本文件）：两段都会变，",
        "#   **别把某一次的字节数当成固定事实** —— 需要时重新抓一次线上文件量。",
        f"#   （{CF_SNAPSHOT} 那一次的快照：CF 段 3259 B + 本文件 725 B = 线上 3984 B。）",
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
    return "\n".join(lines)


def build_sitemap(old: str = "") -> str:
    # 本产品「逻辑页面」清单来自模块级 `PRODUCT_PAGES`（**单一来源**：归属判定也用它），
    # 每个逻辑页面一对中英路径 + 各自优先级。
    #
    # hreflang 必须指向**同一逻辑页面**的中英版本 —— 早先这里写死了
    # `{BASE}/` 与 `{BASE}/en/`，加子页面时 `/wasm/` 的 alternate 会错误地
    # 指向首页（那会让搜索引擎把子页面当成首页的副本）。
    pages = [(zh, en, zh_pri, en_pri) for zh, en, zh_pri, en_pri in PRODUCT_PAGES]

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
    print(f"  构建 sitemap.xml：本产品 {len(entries)} 个 URL（按逻辑页面成对、各自三向 hreflang）")
    return merge_external_sitemap(xml, old)


def build_llms(old: str = "") -> str:
    # 下载段：已发布给 pinned 直链 + 真实字节数；未发布如实写「正在发布，见 Releases 页面」。
    if PUBLISHED:
        dl_section = (
            f"- [{DMG}]({DL}/{DMG})：{DMG_BYTES} 字节（{DMG_MIB} MiB），主下载\n"
            f"- [{ZIP}]({DL}/{ZIP})：{ZIP_BYTES} 字节（{ZIP_MIB} MiB），备用\n"
            f"- [SHA256SUMS.txt]({DL}/SHA256SUMS.txt)：校验和（{SHA_BYTES} 字节）\n"
            f"- [所有版本]({RELEASES_PAGE})\n"
        )
    else:
        dl_section = (
            f"- **v{VERSION} 正在发布**：资产发布后在本页给出直链与**真实字节数**；\n"
            f"  在此之前请到 [GitHub Releases]({RELEASES_PAGE}) 查看（文件名将是\n"
            f"  `{DMG}` 与 `{ZIP}`）。\n"
            f"- 不在这里预先写死字节数或直链 —— 发布前它们还不存在。\n"
        )
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

{dl_section}
## 安装（要点）

安装包 **ad-hoc 签名、未公证**，首次打开会被 macOS 拦截 —— 这是预期行为，不是文件损坏。
三条放行路径（任选一条，完整说明见官网 `#install`）：

- **macOS 15 及以后**：系统设置 → 隐私与安全性 → 「仍要打开」→ 再双击一次
- **macOS 14 及更早**：Finder 里 Control 点按（右键）应用 → 「打开」→ 再点一次「打开」
- **终端（全版本可用）**：`xattr -d com.apple.quarantine /Applications/XrayTun.app`
  —— 注意是 `-d`，**不是** `-dr`：`xattr` 在 macOS 上有**两个实现**（系统自带的 `/usr/bin/xattr`
  与 PATH 上可能先命中的 Python `xattr` 包），`-r` 的支持**随实现与版本而异** —— 旧的 `-dr`
  写法在 PATH 先命中 Python 那份的机器上会报 `option -r not recognized`。
  要递归就用 `find … -exec /usr/bin/xattr -d … {{}} +`，并写绝对路径 `/usr/bin/xattr`。

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
    print("  构建 llms.txt：H1 + blockquote 摘要 + 6 个分节（绝对 URL）")
    return merge_external_llms(txt, old)


def build_llms_full(old: str = "") -> str:
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

    # 外部**索引行**按归属保留：旧文件里「引用站内非本产品路径」的 `- 中文：… / - English: …` 行
    # （不认任何项目名字面量 ⇒ 第三个、第四个外部项目一样成立）。
    # 计数行按「本产品页 + 保留下来的外部索引行」算，不猜数字。
    own_index = [f"- {t} → {BASE}{p}" for t, p, _, _ in pages]
    ext_index = []
    if PRESERVE_EXTERNAL and old:
        new_index_set = set(own_index)
        for _, line in external_url_lines(old):
            if re.match(r"^-\s*(?:中文|English)[：:]", line) and line not in new_index_set:
                ext_index.append(line)
    own_zh = [l for l in own_index if l.startswith("- 中文")]
    own_en = [l for l in own_index if l.startswith("- English")]
    ext_zh_lines = [l for l in ext_index if l.startswith("- 中文")]
    ext_en_lines = [l for l in ext_index if l.startswith("- English")]
    index_lines = own_zh + ext_zh_lines + own_en + ext_en_lines
    n_pages = len(pages) + len(ext_zh_lines) + len(ext_en_lines)
    if len(ext_zh_lines) == len(ext_en_lines):
        count_line = f"收录页面（{n_pages} 个，中英各 {len(pages) // 2 + len(ext_zh_lines)}）："
    else:
        count_line = f"收录页面（{n_pages} 个；本产品 {len(pages)} + 外部 {len(ext_index)}）："
    if ext_index:
        print(f"  ⟳ 保留 {len(ext_index)} 条外部索引行（非本产品页面，逐字不动）")

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

{count_line}

{chr(10).join(index_lines)}

---

{chr(10).join("\n---\n\n" + b for b in bodies)}
"""
    out = header
    if PRESERVE_EXTERNAL and old:
        for name, anchor, where, trailing in EXT_REGIONS:
            region = ext_region(old, name, trailing)
            if not region:
                continue
            if out.count(anchor) != 1:
                raise SystemExit(
                    f"✗ 外部区间 `{name}` 的插入锚点 {anchor!r} 在生成文本里出现 "
                    f"{out.count(anchor)} 次（要求恰好 1 次）——\n"
                    "  生成器**不会**静默丢掉外部内容：请检查 EXT_REGIONS 的锚点常量与模板是否漂移。"
                )
            out = out.replace(anchor, region + anchor if where == "before" else anchor + region, 1)
            print(f"  ⟳ 保留外部区间 `{name}`（{len(region.splitlines())} 行，逐字不动）")
    print(f"  构建 llms-full.txt：本产品 {len(pages)} 个页面（中文 {total_zh} 字 + 英文 {total_en} 字，由 HTML 直接转换）")
    return out


ARTIFACTS = ("robots.txt", "sitemap.xml", "llms.txt", "llms-full.txt")


def build_all(old: dict) -> dict:
    """按盘上现有内容构建四份产物 —— 生成与检查**共用同一段代码**（check 就是"重算一遍比一比"）。"""
    return {
        "robots.txt": build_robots(),
        "sitemap.xml": build_sitemap(old.get("sitemap.xml", "")),
        "llms.txt": build_llms(old.get("llms.txt", "")),
        "llms-full.txt": build_llms_full(old.get("llms-full.txt", "")),
    }


def guard_report(old: dict, new: dict) -> int:
    """「**不许静默丢**」守卫：旧里有、新里没有的外部内容 ⇒ 非零 + 指名到文件与行。

    两类判据（都不认识任何项目名字面量，只看「是不是本产品」）：
      · `lost_external_lines`  —— 带**站内非本产品路径**的整行；
      · `lost_external_sections` —— 连标题一起没了的外部 `## ` 整节（补住「内容不带站内路径」）。

    测试缝 `GEN_SITE_EXTERNAL_OFF=1` 会把整个机制（含本守卫）关掉 ⇒ 那才是"旧行为"，
    用于反向敏感性：旧行为**静默抹掉**，新机制**非零拒绝**。
    """
    if not PRESERVE_EXTERNAL:
        print("  ⚠️ 测试缝 GEN_SITE_EXTERNAL_OFF=1：外部条目保护与守卫都已关闭（等于旧行为）")
        return 0
    bad = 0
    for name in ARTIFACTS:
        lost_lines = lost_external_lines(name, old[name], new[name])
        lost_sections = lost_external_sections(name, old[name], new[name])
        if not (lost_lines or lost_sections):
            continue
        bad = 1
        print(
            f"✗ site/{name}：生成器会把**不属于本产品的外部内容**丢掉（拒绝静默删除）：",
            file=sys.stderr,
        )
        for lineno, line in lost_lines:
            print(f"    site/{name}:{lineno}: {line}", file=sys.stderr)
        for lineno, head in lost_sections:
            print(f"    site/{name}:{lineno}: {head}（整节）", file=sys.stderr)
        print(
            "  修法：外部内容要么走**归属规则**（sitemap 的 `<loc>` / llms.txt 的 `## ` 节自动保留），\n"
            "        要么放进 `<!-- BEGIN external:<名> --> … <!-- END external:<名> -->` 区间。",
            file=sys.stderr,
        )
    return bad


def write_all() -> int:
    old = {name: read_artifact(name) for name in ARTIFACTS}
    new = build_all(old)
    if guard_report(old, new):
        return 1
    for name in ARTIFACTS:
        if old[name] != new[name]:
            (SITE / name).write_text(new[name], encoding="utf-8")
            print(f"  ✓ 写出 site/{name}（{len(new[name])} 字节）")
        else:
            print(f"  = site/{name} 无变化（{len(new[name])} 字节）")
    return 0


def check() -> int:
    """`check` 模式：**不写盘**，把盘上产物与生成器重算结果逐字节比较（漂移即红）。

    这条不变量接在 `scripts/check.sh` 里 —— 门禁会抓住「跑完生成器产物会变」的情况：
    手写进生成区块的内容、过期产物、以及被 `GEN_SITE_EXTERNAL_OFF` 关掉机制后的产物都会红。
    """
    old = {name: read_artifact(name) for name in ARTIFACTS}
    new = build_all(old)
    if guard_report(old, new):
        return 1
    bad = 0
    for name in ARTIFACTS:
        if old[name] == new[name]:
            print(f"  ✓ site/{name} 与生成器重算结果逐字节一致（{len(old[name])} 字节）")
        else:
            bad = 1
            print(f"  ✗ site/{name} 与生成器重算结果不一致（盘上 {len(old[name])} 字节 / 重算 {len(new[name])} 字节）", file=sys.stderr)
            for i, (a, b) in enumerate(zip(old[name].splitlines(), new[name].splitlines()), 1):
                if a != b:
                    print(f"      首个差异 site/{name}:{i}\n        盘上：{a}\n        重算：{b}", file=sys.stderr)
                    break
            else:
                print("      （差异在行数：请直接重跑 `python3 scripts/gen-site-geo.py` 后复核）", file=sys.stderr)
    if bad:
        print(
            "\n✗ 站点 GEO 产物与生成器不一致。修法：\n"
            "  · 那是**手写**内容 ⇒ sitemap/llms.txt 会被自动保留（按归属）；llms-full.txt 请放进\n"
            "    `<!-- BEGIN external:<名> --> … <!-- END external:<名> -->` 区间；\n"
            "  · 那是**产物过期** ⇒ 重跑 `python3 scripts/gen-site-geo.py`（外部条目会自动保留）。",
            file=sys.stderr,
        )
    return bad


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "gen"
    if mode == "check":
        print("检查 GEO / AI 产物是否与生成器一致（不写盘）：")
        return check()
    if mode != "gen":
        print(f"用法：{sys.argv[0]} [gen|check]", file=sys.stderr)
        return 2
    print("生成 GEO / AI 索引产物：")
    return write_all()


if __name__ == "__main__":
    raise SystemExit(main())
