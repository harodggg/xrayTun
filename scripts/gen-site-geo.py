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
BASE = "https://harodggg.github.io/xrayTun"
LAST_PUB = "2026-09-20"
VERSION = "0.8.26"
DL = f"https://github.com/harodggg/xrayTun/releases/download/v{VERSION}"

AI_BOTS = [
    "GPTBot",
    "OAI-SearchBot",
    "ChatGPT-User",
    "ClaudeBot",
    "Claude-Web",
    "anthropic-ai",
    "PerplexityBot",
    "Google-Extended",
    "CCBot",
    "Applebot-Extended",
    "Bytespider",
    "meta-externalagent",
]


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
    lines = [
        "# XrayTun 官网：内容就是给人（和 AI）读的，所以明确允许抓取。",
        "# 站点是纯静态 HTML —— 关键内容（下载、安装、FAQ）都在原始响应里，",
        "# 不需要执行 JavaScript 就能读到。",
        "",
        "User-agent: *",
        "Allow: /",
        "",
    ]
    for bot in AI_BOTS:
        lines += [f"User-agent: {bot}", "Allow: /", ""]
    lines += [f"Sitemap: {BASE}/sitemap.xml", ""]
    (SITE / "robots.txt").write_text("\n".join(lines), encoding="utf-8")
    print("  写出 robots.txt：显式允许", len(AI_BOTS), "个 AI 爬虫")


def write_sitemap() -> None:
    def url(loc: str, self_lang: str) -> str:
        alts = "".join(
            f'\n    <xhtml:link rel="alternate" hreflang="{h}" href="{u}"/>'
            for h, u in (("zh-Hans", f"{BASE}/"), ("en", f"{BASE}/en/"), ("x-default", f"{BASE}/"))
        )
        return f"""  <url>
    <loc>{loc}</loc>
    <lastmod>{LAST_PUB}</lastmod>
    <changefreq>weekly</changefreq>
    <priority>{"1.0" if self_lang == "zh-Hans" else "0.9"}</priority>{alts}
  </url>"""

    xml = f"""<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"
        xmlns:xhtml="http://www.w3.org/1999/xhtml">
{url(f"{BASE}/", "zh-Hans")}
{url(f"{BASE}/en/", "en")}
</urlset>
"""
    (SITE / "sitemap.xml").write_text(xml, encoding="utf-8")
    print("  写出 sitemap.xml：2 个 URL + 三向 hreflang alternate")


def write_llms() -> None:
    txt = f"""# XrayTun

> XrayTun 是 macOS 13.0+ 的 Xray 图形客户端，用 Xray-core 原生 TUN 入站接管系统流量（整机按规则走代理）。
> 当前版本 v0.8.26（{LAST_PUB}），通用包（Apple Silicon + Intel），**包内自带 Xray 核心**。
> 安装包是 ad-hoc 签名、**未公证**的，首次打开会被 Gatekeeper 拦截，需按系统版本手动放行。
> 仅支持 macOS；没有 Windows / Linux / 移动端版本。本页所有断言都可在仓库文档里逐条核对。

## 官网

- [中文站（完整正文）]({BASE}/)：是什么、解决什么问题、核心能力与刻意不做的边界、下载、安装、FAQ、链接
- [English site (equivalent content)]({BASE}/en/)：与中文站逐段等价，不是半份翻译
- [本站全文（供一次性摄取）]({BASE}/llms-full.txt)：中英两个页面的**完整正文** + 事实与边界清单；**不含**仓库 `docs/` 下的全量设计文档（那是另一处，见下）

## 下载

- [XrayTun_0.8.26_x86_64_arm64.dmg]({DL}/XrayTun_0.8.26_x86_64_arm64.dmg)：47,128,987 字节（44.9 MiB），主下载
- [XrayTun_0.8.26_x86_64_arm64.zip]({DL}/XrayTun_0.8.26_x86_64_arm64.zip)：42,636,964 字节（40.7 MiB），备用
- [SHA256SUMS.txt]({DL}/SHA256SUMS.txt)：校验和（200 字节）
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
    zh = html_to_md((SITE / "index.html").read_text(encoding="utf-8"), "zh")
    en = html_to_md((SITE / "en" / "index.html").read_text(encoding="utf-8"), "en")
    header = f"""# XrayTun — 全文（llms-full.txt）

> 站点：{BASE}/ ｜ 版本：v{VERSION}（{LAST_PUB}）｜ 生成方式：由页面 HTML 直接转换，
> 因此与网页**等价**（不是摘要）。改页面文案后应重新生成，避免 AI 读到旧内容。
> 结构化数据见两个页面 `<head>`里的 JSON-LD（SoftwareApplication + FAQPage）。
>
> **完整性说明（重要，别把 "full" 读成全量文档）**：本文件包含的是**两个语言页面的完整正文**
> 与官网里的「事实与边界」清单。仓库 `docs/` 下的**完整设计文档（数千行规范）没有逐字复制**
> 到这里 —— 那样既膨胀、又必然与仓库文档不同步，反而更糟。需要细节请看绝对链接：
> <https://github.com/harodggg/xrayTun/tree/main/docs>
> （安装与 TUN 权限：docs/02-tun-and-privileges.md；分流与 DNS：docs/04-routing-and-dns.md）

来源：{BASE}/（中文）与 {BASE}/en/（英文）。
这是同一份内容的两种语言，段落一一对应；任一处不一致以对应语言页面为准。

---

# 中文页面全文

{zh}

---

# English page (full text)

{en}
"""
    (SITE / "llms-full.txt").write_text(header, encoding="utf-8")
    print(f"  写出 llms-full.txt：中文 {len(zh)} 字 + 英文 {len(en)} 字（由页面直接转换）")


def main() -> int:
    print("生成 GEO / AI 索引产物：")
    write_robots()
    write_sitemap()
    write_llms()
    write_llms_full()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
