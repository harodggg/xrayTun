#!/usr/bin/env python3
"""从页面 HTML 生成 / 校验 JSON-LD（task-19 第 3 步；task-29 扩展为多页面 + 两种软件类型）。

为什么用脚本而不是手抄：
「FAQPage 与页面 FAQ 逐字对应」如果靠人抄，下一次改文案就会静默不一致
（结构化数据与页面说法不同，对 AI 与搜索结果都是一种说谎）。
这里**从 HTML 里抽文本**生成 JSON-LD，并在生成后立刻反向校验：
JSON-LD 里每一条 Q/A 必须能在页面可见文本里找到。

用法：
  python3 scripts/gen-site-jsonld.py gen      # 生成并写入各页面的 <head>
  python3 scripts/gen-site-jsonld.py check    # 只校验（不改文件）

校验口径（每页都必须过）：
  · JSON-LD 恰好 2 块：软件块 + FAQPage；
  · FAQPage 与页面 #faq 区块逐条逐字一致，且每条 Q/A 都出现在可见文本里；
  · 软件块的 @type 与页面登记的 type 一致，version 与登记值一致；
  · **canonical / 三向 hreflang** 与登记值逐字一致（域名迁移时这里会挡住半途而废）；
  · meta description 与登记值一致（结构化数据不能描述一个页面里没有的说法）；
  · 各类型的专属红线，见 check() 里的注释。
"""
import html
import json
import re
import sys
from pathlib import Path

# 脚本住在 <repo>/scripts/ 下，所以 site/ 在上一级。
# （早先它在仓库外的 .scratch/ 里，那时是 parents[1]/"xray-tun"/"site" ——
#  那样脚本不可持续：.scratch/ 是 gitignore 的，别人拿不到它。）
SITE = Path(__file__).resolve().parents[1] / "site"

# 站点绝对基址：**与 gen-site-geo.py 的 BASE 必须一致**。
# 域名迁移时两处一起改（生成器里各只有一处，产物由脚本重写，别手改产物）。
BASE = "https://xraytun.top"

XRAYTUN_VERSION = "0.8.41"
RELEASES_PAGE = "https://github.com/harodggg/xrayTun/releases"
# **发布两阶段**：资产还没发布时（提交 1），downloadUrl 指向 Releases 页面而不是
# `.../download/v{版本}/...`（那会 404）。发布后（提交 2）把 PUBLISHED 置 True，
# 结构化数据才写 pinned 直链 —— 与 `gen-site-geo.py` 的开关同名同义。
PUBLISHED = False
XRAYTUN_DL = (
    f"https://github.com/harodggg/xrayTun/releases/download/v{XRAYTUN_VERSION}"
    f"/XrayTun_{XRAYTUN_VERSION}_x86_64_arm64.dmg"
    if PUBLISHED
    else RELEASES_PAGE
)

WASM_REPO = "https://github.com/harodggg/xray-wasm"

# 相关项目：黄推过滤器 · Jev（同一个作者的独立项目，页面在 site/jev-x-filter/）。
# 它与 XrayTun **不是同一个产品**：下载地址、Release、许可证都指向它自己的仓库，
# 所以 SoftwareApplication 分支允许逐页覆盖这些字段（默认值仍是 XrayTun 的那套）。
JEVX_REPO = "https://github.com/harodggg/jev-x-filter"
JEVX_VERSION = "0.4.8"
JEVX_RELEASES = f"{JEVX_REPO}/releases"
JEVX_DL = f"{JEVX_REPO}/releases/download/v{JEVX_VERSION}/jev-x-filter-{JEVX_VERSION}.zip"

# 美颜程度检测（素颜镜）：**没有独立仓库**，包直接由本站分发（site/beauty-meter/ 下），
# 因此 license 指向本站自己发的 MIT 全文（site/beauty-meter/LICENSE），releaseNotes 指向项目页 ——
# 不借用 XrayTun 的链接。（check() 要求 license 以 /LICENSE 结尾，所以文件就叫 LICENSE。）
BM_VERSION = "1.3.0"
BM_LIGHT_VERSION = "1.0.0"   # 轻量版：只有美颜检测，无模型（页面上也提供下载）
BM_PAGE = f"{BASE}/beauty-meter/"
BM_EN_PAGE = f"{BASE}/en/beauty-meter/"
# 完整版 50MB：官网（Cloudflare Pages）单文件上限 25 MiB，所以托管在仓库的 Releases（tag 不以 v 开头，
# 不会触发 XrayTun 的发版工作流；已用 --latest=false 保证 XrayTun 仍是 Latest）。
BM_RELEASE_TAG = "beauty-meter-v1.3.0"
BM_DL = (f"https://github.com/harodggg/xrayTun/releases/download/{BM_RELEASE_TAG}"
         f"/beauty-meter-extension-{BM_VERSION}.zip")
BM_DL_LIGHT = f"{BM_PAGE}beauty-meter-extension-{BM_LIGHT_VERSION}.zip"
BM_LICENSE = f"{BM_PAGE}LICENSE"

# 一目十行 · SpeedRead：与素颜镜同一套做法（**没有独立仓库**，包直接由本站分发在
# site/speed-read/ 下），因此下载与 license 都指向本站自己的资产，绝不落到 XrayTun 的 dmg / LICENSE
# —— 把本站的 dmg 说成这个扩展的下载地址是不实陈述。
SR_VERSION = "0.1.0"
SR_PAGE = f"{BASE}/speed-read/"
SR_EN_PAGE = f"{BASE}/en/speed-read/"
SR_DL = f"{SR_PAGE}speed-read-extension-{SR_VERSION}.zip"
SR_LICENSE = f"{SR_PAGE}LICENSE"

PAGES = [
    {
        "path": SITE / "index.html",
        "pair": "home",  # 同一 pair 的两种语言页面互为 hreflang alternate
        "lang": "zh-Hans",
        "url": f"{BASE}/",
        "type": "SoftwareApplication",
        "name": "XrayTun",
        "version": XRAYTUN_VERSION,
        # 必须与页面 <meta name="description"> 逐字一致 —— 由 check() 强制。
        # （2026-09-20 发现：这里的旧文案比页面 meta 少了关键词那半句，
        #  也就是结构化数据在描述一个页面上没有的旧说法；以页面为准改齐。）
        "description": "XrayTun 是面向 macOS 13 及以上的 Xray 图形客户端，使用 Xray-core 原生 TUN 入站接管系统流量，支持 vmess / vless / trojan / shadowsocks 节点、四种订阅格式、geoip/geosite 分流与 Fake-IP。当前版本 v0.8.41，通用包（Apple Silicon + Intel），包内自带 Xray 核心。",
        "os": "macOS 13.0 or later",
        # 许可证事实（2026-09-20 起）：源码 MIT 且仓库有 LICENSE；随包 Xray-core 是 MPL-2.0。
        "faq_must_contain": ["MPL-2.0"],
    },
    {
        "path": SITE / "en" / "index.html",
        "pair": "home",
        "lang": "en",
        "url": f"{BASE}/en/",
        "type": "SoftwareApplication",
        "name": "XrayTun",
        "version": XRAYTUN_VERSION,
        "description": "XrayTun is an Xray GUI client for macOS 13 and later. It uses Xray-core's native TUN inbound to take over system traffic, supports vmess / vless / trojan / shadowsocks nodes, four subscription formats, geoip/geosite routing and Fake-IP. Current version v0.8.41, universal build (Apple Silicon + Intel), Xray core included.",
        "os": "macOS 13.0 or later",
        "faq_must_contain": ["MPL-2.0"],
    },
    {
        # task-29：xray-wasm 独立页。它是**另一个项目**，不是 XrayTun 的功能页。
        "path": SITE / "wasm" / "index.html",
        "pair": "wasm",
        "lang": "zh-Hans",
        "url": f"{BASE}/wasm/",
        # 用 SoftwareSourceCode：它是「源代码 + 运行时」而不是桌面应用。
        # 关键：**不写 operatingSystem** —— 它跑在 wasmtime / 容器里，不是某个桌面系统。
        "type": "SoftwareSourceCode",
        "name": "xray-wasm",
        "version": "0.7.0",
        "description": "xray-wasm 把 Xray 的 VLESS + XTLS-Vision + REALITY 协议栈用纯 Rust 重写，编译成 wasm32-wasip2，在 wasmtime 下跑真实 TCP 代理。客户端与服务端两个方向，当前 v0.7.0，镜像 ghcr.io/harodggg/xray-wasm:v0.7.0（amd64 + arm64）。它与 XrayTun 是同一个作者的两个独立项目。",
        "repo": WASM_REPO,
        "runtime": "wasmtime (WASI Preview 2)",
        # 页面必须**逐字**出现的两句话 —— 这是红线，用机制钉住，不靠自觉：
        #  ① 与 XrayTun 是非集成关系（不许暗示 XrayTun 用了它）；
        #  ② 许可证必须带上 LICENSE.meow-rs，不能只写 MIT（GitHub 因此识别为 Other）。
        "must_contain": ["不使用 xray-wasm", "LICENSE.meow-rs"],
    },
    {
        "path": SITE / "en" / "wasm" / "index.html",
        "pair": "wasm",
        "lang": "en",
        "url": f"{BASE}/en/wasm/",
        "type": "SoftwareSourceCode",
        "name": "xray-wasm",
        "version": "0.7.0",
        "description": "xray-wasm rewrites Xray's VLESS + XTLS-Vision + REALITY protocol stack in pure Rust and compiles it to wasm32-wasip2, where it runs a real TCP proxy under wasmtime. Two directions (client and server), current version v0.7.0, image ghcr.io/harodggg/xray-wasm:v0.7.0 (amd64 + arm64). It is a separate project by the same author as XrayTun.",
        "repo": WASM_REPO,
        "runtime": "wasmtime (WASI Preview 2)",
        "must_contain": ["does not use xray-wasm", "LICENSE.meow-rs"],
    },
    {
        # 相关项目页：黄推过滤器 · Jev。与 xray-wasm 同一套做法 ——
        # 独立仓库 + 独立页面，下载/Release/许可证全部指向它自己的仓库。
        "path": SITE / "jev-x-filter" / "index.html",
        "pair": "jev-x-filter",
        "lang": "zh-Hans",
        "url": f"{BASE}/jev-x-filter/",
        "type": "SoftwareApplication",
        "name": "信息过滤器 · Jev",
        "version": JEVX_VERSION,
        # 必须与页面 <meta name="description"> 逐字一致 —— 由 check() 强制。
        "description": "信息过滤器 · Jev 是一个 Chrome MV3 扩展：用 Jev（TypeSafe System One）的类型化决策在本地判定 x.com 时间线、回复区与推荐流里的垃圾信息 —— 色情/性交易引流、诈骗/博彩/荐股、广告导流、标题党、低质 AI、重复文案农场，六类独立开关。命中即隐藏，只有高置信度的类别才会动账号（默认演练模式）。它与 XrayTun 是同一个作者的两个独立项目。",
        "os": "Chrome 120 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": f"{BASE}/jev-x-filter/",
        "download_url": JEVX_DL,
        "install_url": JEVX_DL,
        "release_notes": JEVX_RELEASES,
        "license_url": f"{JEVX_REPO}/blob/main/LICENSE",
        # 红线：必须写明它与 XrayTun 是两个独立项目（不许暗示集成）。
        "must_contain": ["独立项目"],
    },
    {
        "path": SITE / "en" / "jev-x-filter" / "index.html",
        "pair": "jev-x-filter",
        "lang": "en",
        "url": f"{BASE}/en/jev-x-filter/",
        "type": "SoftwareApplication",
        "name": "Info Filter · Jev",
        "version": JEVX_VERSION,
        "description": "Info Filter · Jev is a Chrome MV3 extension that uses Jev (TypeSafe System One) typed decisions to judge timeline junk in your x.com timeline, replies and recommendations locally: adult solicitation, scam/gambling/stock-tip fraud, ad and traffic spam, clickbait, low-quality AI filler and repeated-text farms — six categories with individual switches. Hits are hidden; only high-confidence categories can touch the account (dry-run by default). It is a separate project by the same author as XrayTun.",
        "os": "Chrome 120 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": f"{BASE}/en/jev-x-filter/",
        "download_url": JEVX_DL,
        "install_url": JEVX_DL,
        "release_notes": JEVX_RELEASES,
        "license_url": f"{JEVX_REPO}/blob/main/LICENSE",
        "must_contain": ["separate project"],
    },
    {
        # 相关项目页：素颜镜 · 美颜程度检测（Chrome MV3 扩展）。
        # 与 Jev 同一套做法，但它**没有独立仓库**：下载与许可证都指向本站自己的资产，
        # 绝不能落到 XrayTun 的 dmg / LICENSE（那是不实陈述）。
        # v1.1 起下载页提供两个包：完整版（含 52.5MB AI 模型，约 50MB）与轻量版（74KB，仅美颜）；
        # 结构化数据里的 downloadUrl 指向**完整版**（功能最全的那个）。
        "path": SITE / "beauty-meter" / "index.html",
        "pair": "beauty-meter",
        "lang": "zh-Hans",
        "url": BM_PAGE,
        "type": "SoftwareApplication",
        "name": "素颜镜 · 美颜程度检测",
        "version": BM_VERSION,
        "description": "素颜镜是一个 Chrome MV3 扩展：用纯本地像素分析给出 0~100 的「美颜程度」评分，拆成磨皮去纹理、美白提亮、肤色均匀、去色低饱和、通透度压缩五个维度；悬停网页图片 0.3 秒看角标，点击展开完整报告，也可以拖拽、粘贴或选择本地图片分析。悬停图片时角标左上角先给出纯像素的「像素指纹」证据（虚线，约 30ms、无模型），再由本地模型给出「是否像 AI 生成」的判定（实心彩色，红=疑似 AI、绿=像真实拍摄）；报告里另有证据明细（完整版 50MB，另有 74KB 轻量版只做美颜）。推理全在本机，不联网、不上传图片。它与 XrayTun 是同一个作者的两个独立项目。",
        "os": "Chrome 109 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": BM_PAGE,
        "download_url": BM_DL,
        "install_url": BM_DL,
        "release_notes": BM_PAGE,
        "license_url": BM_LICENSE,
        # 红线：必须写明它与 XrayTun 是两个独立项目（不许暗示集成）。
        "must_contain": ["独立项目"],
    },
    {
        "path": SITE / "en" / "beauty-meter" / "index.html",
        "pair": "beauty-meter",
        "lang": "en",
        "url": BM_EN_PAGE,
        "type": "SoftwareApplication",
        "name": "Beauty Meter",
        "version": BM_VERSION,
        "description": "Beauty Meter is a Chrome MV3 extension that scores how heavily a photo was beautified on a 0~100 scale using purely local pixel analysis, broken into five dimensions: skin smoothing, whitening, skin-tone uniformity, desaturation and contrast compression. Hover an image for 0.3s to see a badge, click it for the full report; drag, paste or pick a local file in the popup. Hovering an image first shows a pure-pixel “fingerprint” chip on the top-left corner of the badge (dashed, ~30ms, no model) and then the local model's AI verdict (solid colour: red = likely AI-generated, green = looks real), with the detailed evidence listed in the report (50MB; a 74KB light build does beautification only). All inference stays on your machine: no network, no uploads. It is a separate project by the same author as XrayTun.",
        "os": "Chrome 109 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": BM_EN_PAGE,
        "download_url": BM_DL,
        "install_url": BM_DL,
        "release_notes": BM_EN_PAGE,
        "license_url": BM_LICENSE,
        "must_contain": ["separate project"],
    },
    {
        # 相关项目页：一目十行 · SpeedRead（Chrome MV3 扩展：把整页压成 3 条）。
        # 与素颜镜同一套做法：**没有独立仓库**，下载与许可证都指向本站自己的资产。
        # 红线：页面必须写明它与 XrayTun 是两个独立项目（不许暗示集成）。
        "path": SITE / "speed-read" / "index.html",
        "pair": "speed-read",
        "lang": "zh-Hans",
        "url": SR_PAGE,
        "type": "SoftwareApplication",
        "name": "一目十行 · SpeedRead",
        "version": SR_VERSION,
        "description": "一目十行是一个 Chrome MV3 扩展：把整个网页的正文压成恰好 3 条信息 —— α 阿尔法（全文最重要的核心结论）、β 贝塔（第二关键信息：支撑证据或代价/风险/限制/反面观点）、γ 人说最多（全页被重复强调最多的议题，附高频词证据）。摘要由你自己配置的 OpenAI 兼容大模型接口生成，Key 只存在本机、只由扩展的 Service Worker 发送；没有 Key 或调用失败时自动降级为本地词频加句子打分，仍然给满 3 条。页面一变就自动重算：DOM 变化、SPA 路由、切回标签页、兜底轮询四路触发；内容指纹没变则命中缓存、不重复调用模型。它与 XrayTun 是同一个作者的两个独立项目。",
        "os": "Chrome 110 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": SR_PAGE,
        "download_url": SR_DL,
        "install_url": SR_DL,
        "release_notes": SR_PAGE,
        "license_url": SR_LICENSE,
        "must_contain": ["独立项目"],
    },
    {
        "path": SITE / "en" / "speed-read" / "index.html",
        "pair": "speed-read",
        "lang": "en",
        "url": SR_EN_PAGE,
        "type": "SoftwareApplication",
        "name": "SpeedRead",
        "version": SR_VERSION,
        "description": "SpeedRead is a Chrome MV3 extension that compresses the body of a whole web page into exactly 3 items: alpha (the single most important conclusion), beta (the second key fact: supporting evidence, or the cost, risk, limit or counterpoint) and gamma (the topic the page repeats and emphasises most, with high-frequency terms as evidence). Summaries come from your own OpenAI-compatible endpoint; the API key stays on your machine and is only ever read by the extension service worker. With no key, or when a call fails, it falls back to local term-frequency plus sentence scoring and still returns 3 items. It recomputes automatically whenever the page changes: DOM mutations, SPA routes, returning to the tab, and a fallback poll; a content fingerprint means unchanged pages hit the cache instead of the model. It is a separate project by the same author as XrayTun.",
        "os": "Chrome 110 or later (Manifest V3)",
        "application_category": "BrowserApplication",
        "help_url": SR_EN_PAGE,
        "download_url": SR_DL,
        "install_url": SR_DL,
        "release_notes": SR_EN_PAGE,
        "license_url": SR_LICENSE,
        "must_contain": ["separate project"],
    },
]

MARK_START = "<!-- JSON-LD: generated by scripts/gen-site-jsonld.py — 改文案后重跑，别手改 -->"
# 早先脚本在仓库外，注释里写的是 .scratch/ 下的路径（那句是错的：别人拿不到那个目录）。
# 重新生成时要连旧标记一起删掉，否则页面上会留下两份 JSON-LD。
LEGACY_MARKS = ["<!-- JSON-LD: generated by .scratch/gen_jsonld.py — 改文案后重跑，别手改 -->"]


def strip_tags(s: str) -> str:
    s = re.sub(r"<[^>]+>", "", s)
    s = html.unescape(s)
    return re.sub(r"\s+", " ", s).strip()


def faq_pairs(src: str) -> list[tuple[str, str]]:
    """从 #faq 区块抽出 (问题, 答案)。答案取该 h3 之后、下一个 h3 之前的全部段落文本。"""
    start = src.index('id="faq"')
    block = src[start:]
    block = block[: block.index("</section>")]
    parts = re.split(r"<h3[^>]*>", block)[1:]
    out = []
    for p in parts:
        q_end = p.index("</h3>")
        q = strip_tags(p[:q_end])
        rest = p[q_end + 5 :]
        # 答案 = 后面的所有 <p> 文本拼接
        paras = re.findall(r"<p[^>]*>([\s\S]*?)</p>", rest)
        a = " ".join(strip_tags(x) for x in paras).strip()
        if q and a:
            out.append((q, a))
    return out


def alternates(page: dict) -> list[tuple[str, str]]:
    """本页所在 pair 的三向 hreflang（x-default 指向中文页）。"""
    sibs = {p["lang"]: p["url"] for p in PAGES if p["pair"] == page["pair"]}
    return [("zh-Hans", sibs["zh-Hans"]), ("en", sibs["en"]), ("x-default", sibs["zh-Hans"])]


def site_url(page: dict) -> str:
    """该**语言版本**的站点根。

    WebSite 实体的 url 必须是站点根，不能指向某个子页面 ——
    把 `/wasm/` 说成站点 URL 是不实陈述（那是页面，不是站点）。
    """
    return f"{BASE}/" if page["lang"] == "zh-Hans" else f"{BASE}/en/"


def breadcrumb(page: dict) -> list[tuple[str, str]] | None:
    """面包屑条目；**首页返回 None**（首页没有上一级，不编造层级）。"""
    if page["pair"] == "home":
        return None
    first = "XrayTun 首页" if page["lang"] == "zh-Hans" else "XrayTun home"
    return [(first, site_url(page)), (page["name"], page["url"])]


def build_ld(page: dict, faqs: list[tuple[str, str]]) -> list[dict]:
    if page["type"] == "SoftwareApplication":
        software = {
            "@context": "https://schema.org",
            "@type": "SoftwareApplication",
            "name": page["name"],
            "operatingSystem": page["os"],
            # 逐页覆盖：默认仍是 XrayTun 自己；独立项目页必须指向自己的仓库/Release，
            # 否则结构化数据会声称「这个扩展的下载地址是 XrayTun 的 dmg」—— 那是不实陈述。
            "applicationCategory": page.get("application_category", "UtilitiesApplication"),
            "softwareVersion": page["version"],
            "softwareHelp": page.get("help_url", f"{BASE}/"),
            "downloadUrl": page.get("download_url", XRAYTUN_DL),
            "installUrl": page.get("install_url", page.get("download_url", XRAYTUN_DL)),
            "releaseNotes": page.get("release_notes", "https://github.com/harodggg/xrayTun/blob/main/CHANGELOG.md"),
            # 仓库已有 LICENSE（MIT）→ 指向它（2026-09-20 用户决定补上，commit 12c523d）
            "license": page.get("license_url", "https://github.com/harodggg/xrayTun/blob/main/LICENSE"),
            "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"},
            "description": page["description"],
            "inLanguage": page["lang"],
        }
    else:
        # SoftwareSourceCode：**没有 operatingSystem 字段**（它跑在 WASI 运行时里）。
        software = {
            "@context": "https://schema.org",
            "@type": "SoftwareSourceCode",
            "name": page["name"],
            "description": page["description"],
            "codeRepository": page["repo"],
            "programmingLanguage": "Rust",
            "runtimePlatform": page["runtime"],
            "version": page["version"],
            # 指向仓库里的 LICENSE；**双许可的事实写在 FAQPage 的答案里**（同一份 JSON-LD 内），
            # 页面上也逐字写了「MIT（另含 LICENSE.meow-rs 第三方许可，见仓库）」。
            "license": f"{page['repo']}/blob/main/LICENSE",
            "inLanguage": page["lang"],
        }
    blocks = [
        software,
        {
            "@context": "https://schema.org",
            "@type": "FAQPage",
            "inLanguage": page["lang"],
            "mainEntity": [
                {
                    "@type": "Question",
                    "name": q,
                    "acceptedAnswer": {"@type": "Answer", "text": a},
                }
                for q, a in faqs
            ],
        },
        # WebSite：站点实体。没有站内搜索，所以**不写 SearchAction**（那会声称一个不存在的能力）。
        {
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": "XrayTun",
            "url": site_url(page),
            "inLanguage": page["lang"],
        },
    ]
    bc = breadcrumb(page)
    if bc:
        # 只给**有多级**的页面（子页面）；首页不编造上一级。
        blocks.append(
            {
                "@context": "https://schema.org",
                "@type": "BreadcrumbList",
                "itemListElement": [
                    {"@type": "ListItem", "position": i + 1, "name": n, "item": u}
                    for i, (n, u) in enumerate(bc)
                ],
            }
        )
    return blocks


def inject(path: Path, blocks: list[dict]) -> None:
    src = path.read_text(encoding="utf-8")
    payload = "\n".join(
        MARK_START
        + "\n"
        + '<script type="application/ld+json">'
        + json.dumps(b, ensure_ascii=False, indent=2)
        + "</script>"
        for b in blocks
    )
    # 幂等：先删掉上次生成的块（含旧标记写的那些）
    for mark in [MARK_START] + LEGACY_MARKS:
        src = re.sub(
            re.escape(mark) + r'\s*<script type="application/ld\+json">[\s\S]*?</script>\s*',
            "",
            src,
        )
    assert "</head>" in src
    src = src.replace("</head>", payload + "\n  </head>", 1)
    path.write_text(src, encoding="utf-8")


def check() -> int:
    bad = 0
    for page in PAGES:
        src = page["path"].read_text(encoding="utf-8")
        rel = page["path"].relative_to(SITE)
        problems: list[str] = []
        blocks = re.findall(r'<script type="application/ld\+json">([\s\S]*?)</script>', src)
        # 期望的块集合：软件块 + FAQPage + WebSite（+ 子页面才有 BreadcrumbList）。
        expected_types = [page["type"], "FAQPage", "WebSite"]
        if breadcrumb(page):
            expected_types.append("BreadcrumbList")
        if len(blocks) != len(expected_types):
            print(f"✗ {rel}: JSON-LD 块数 {len(blocks)}（期望 {len(expected_types)}：{expected_types}）")
            bad += 1
            continue
        parsed = []
        for b in blocks:
            try:
                parsed.append(json.loads(b))
            except json.JSONDecodeError as e:
                problems.append(f"JSON 解析失败 {e}")
        actual_types = [p.get("@type") for p in parsed if isinstance(p, dict)]
        if sorted(actual_types) != sorted(expected_types):
            print(f"✗ {rel}: JSON-LD 类型 {actual_types}（期望 {expected_types}）")
            bad += 1
            continue
        software = next((p for p in parsed if p.get("@type") == page["type"]), None)
        faq = next((p for p in parsed if p.get("@type") == "FAQPage"), None)
        website = next((p for p in parsed if p.get("@type") == "WebSite"), None)
        if not software or not faq or not website:
            print(f"✗ {rel}: 缺 {page['type']} / FAQPage / WebSite（实际 {actual_types}）")
            bad += 1
            continue

        # ---- WebSite / BreadcrumbList 与登记值一致（不许把子页面说成站点根）----
        if website.get("url") != site_url(page):
            problems.append(f"WebSite.url 应为 {site_url(page)}")
        if website.get("inLanguage") != page["lang"]:
            problems.append(f"WebSite.inLanguage 应为 {page['lang']}")
        bc = breadcrumb(page)
        if bc:
            bcl = next((p for p in parsed if p.get("@type") == "BreadcrumbList"), None)
            items = [
                (i.get("name"), i.get("item"), i.get("position"))
                for i in (bcl or {}).get("itemListElement", [])
            ]
            want = [(n, u, i + 1) for i, (n, u) in enumerate(bc)]
            if items != want:
                problems.append(f"BreadcrumbList 与登记值不一致：{items} != {want}")

        visible = strip_tags(src)

        # ---- canonical / hreflang：与登记值逐字一致（域名迁移的护栏）------------
        if f'<link rel="canonical" href="{page["url"]}" />' not in src:
            problems.append(f'canonical 不等于登记的 {page["url"]}')
        for h, u in alternates(page):
            if f'<link rel="alternate" hreflang="{h}" href="{u}" />' not in src:
                problems.append(f'hreflang {h} 不等于登记的 {u}')

        # ---- meta description：结构化数据不能描述页面里没有的说法 ----------------
        m = re.search(r'name="description"[^>]*content="([^"]*)"', src, re.S)
        meta_desc = re.sub(r"\s+", " ", html.unescape(m.group(1))).strip() if m else ""
        if meta_desc != page["description"]:
            problems.append("meta description 与登记值不一致")

        # ---- FAQPage 与页面 #faq 逐条逐字一致 ---------------------------------
        page_faqs = faq_pairs(src)
        ld_faqs = [(q["name"], q["acceptedAnswer"]["text"]) for q in faq["mainEntity"]]
        if ld_faqs != page_faqs:
            problems.append(f"FAQPage 与页面 FAQ 不一致（{len(ld_faqs)} vs {len(page_faqs)}）")
        for q, a in ld_faqs:
            if q not in visible:
                problems.append(f"问题不在页面里：{q[:40]}")
            if a[:60] not in visible:
                problems.append(f"答案前 60 字不在页面里：{a[:60]}")

        faq_text = json.dumps(faq, ensure_ascii=False)
        # 版本键名两种类型不同：SoftwareApplication 用 softwareVersion，SoftwareSourceCode 用 version。
        ld_version = software.get("softwareVersion" if page["type"] == "SoftwareApplication" else "version")
        if str(ld_version or "") != page["version"]:
            problems.append(f"version 不是 {page['version']}")
        if not str(software.get("license", "")).endswith("/LICENSE"):
            problems.append("license 字段应指向 /LICENSE")

        if page["type"] == "SoftwareApplication":
            if software.get("operatingSystem") != page["os"]:
                problems.append(f"operatingSystem 应为「{page['os']}」")
            for s in page.get("faq_must_contain", []):
                if s not in faq_text:
                    problems.append(f"FAQ 里必须写明 {s}")
            # 页面里必须逐字出现的红线（与 SoftwareSourceCode 同一套机制）
            for s in page.get("must_contain", []):
                if s not in visible:
                    problems.append(f"页面里必须有这句话：{s}")
        else:
            # 红线：不能声称某个操作系统 —— 它跑在 wasmtime / 容器里。
            if "operatingSystem" in software:
                problems.append("SoftwareSourceCode 不得写 operatingSystem")
            if software.get("codeRepository") != page["repo"]:
                problems.append(f"codeRepository 不是 {page['repo']}")
            for s in page.get("must_contain", []):
                if s not in visible:
                    problems.append(f"页面里必须有这句话：{s}")

        if problems:
            for p in problems:
                print(f"✗ {rel}: {p}")
            bad += 1
        print(
            f"{'✓' if not problems else '✗'} {rel}: JSON.parse ok · {page['type']} ok · "
            f"FAQPage {len(ld_faqs)} 条与页面逐条一致 · canonical/hreflang ok · 可见文本 {len(visible)} 字"
        )
    return bad


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "check"
    if mode == "gen":
        for page in PAGES:
            faqs = faq_pairs(page["path"].read_text(encoding="utf-8"))
            inject(page["path"], build_ld(page, faqs))
            print(f"  写入 JSON-LD：{page['path'].relative_to(SITE)}（{page['type']} + FAQ {len(faqs)} 条）")
        mode = "check"
    if mode == "check":
        bad = check()
        print("结论:", "全部通过" if bad == 0 else f"{bad} 处不一致")
        return 1 if bad else 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
