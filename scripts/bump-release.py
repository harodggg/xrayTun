#!/usr/bin/env python3
"""发布机械动作 —— **先全部校验，再统一落盘**。

    python3 scripts/bump-release.py phase1 --new 0.8.36 [--date YYYY-MM-DD] [--dry-run]
    python3 scripts/bump-release.py phase2 --dmg-bytes N --zip-bytes N [--sha-bytes 200]
                                        --date YYYY-MM-DD [--dry-run]
    python3 scripts/bump-release.py notes  --tag v0.8.36 --out NOTES.md
    python3 scripts/bump-release.py self-test [--fixture-ref <commit>]

# 为什么要这个工具（每条都对应一次真实事故）

* **v0.8.33 的中间态**：页面上写着 `0.8.33` 的文件名却带着 `0.8.32` 的字节数，
  根因是替换脚本的管道吞掉了退出码 ⇒ 半成品落盘。本工具的性质是
  **任一条 pattern 的命中次数不符 ⇒ 整体退出、磁盘上一个字节都不改**。
* **替换顺序**：「已发布 → 正在发布」那组 pattern **依赖版本号替换先生效**
  （它要匹配 `v0.8.36 …`）。拿**原始文本**去校验它们会全部 0 命中 ——
  本工具的第一版就踩过，被干跑抓住。所以校验是**顺序模拟**：在内存里按序应用，
  每一步都在**上一步的结果**上数命中次数，全部通过后才统一写盘。
* **v0.8.35 的线上回归**：`site/og-image-<旧版本>.png` 被删、而引用切换在**下一个提交**，
  中间那段窗口里线上首页的 `og:image` 是 404（实测 HTTP 404）。
  ⇒ 删旧 og 卡片**由 `phase1` 自己做**（删之前先断言 `site/**` 里引用为 0），
  与「切引用」**同一个提交**。
* **v0.8.35 的第二个教训**：Release 正文里那句「本版需要**重新**安装特权助手」靠人记，
  模板里没有 ⇒ 只能发布后手工 `gh release edit`。见 `notes` 子命令：
  **本版用户动作缺文件就失败**，不许静默退化成通用模板。

# 边界

* 只做**文本与删除动作**；生成器（`gen-site-*.py`）默认**不跑**，用 `--run-generators`
  显式打开（它们依赖本机解释器/Pillow，跑不动要看得见地失败）。
* `--dry-run` 绝不写盘、绝不删文件。
"""

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

RELEASES_PAGE = "https://github.com/harodggg/xrayTun/releases"
REPO_SLUG = "harodggg/xrayTun"

# --------------------------------------------------------------------------------------
# 这几个常量**只服务于 self-test 的反例**：它会把这些值改错，用来证明
# 「命中次数不符 ⇒ 非 0 退出且零落盘」不是空话（本项目的规矩：机制要自带反例）。
# 生产逻辑读它们，值本身没别的用处。
# --------------------------------------------------------------------------------------
N_CAPTION_ZH = 1          # 中文页「真实资产（…）」caption 出现次数
N_BUTTON_DMG_ZH = 2       # 中文页 dmg 按钮（顶部 + 底部各一个）
N_PINNED_PER_PAGE = 8     # 每页 pinned 直链（6 条 HTML 链接 + JSON-LD 的 downloadUrl/installUrl）


class Rule:
    """一条替换规则。

    expect 为 int ⇒ 必须**恰好**命中这么多次；为 None ⇒ 全量替换，但至少要命中 min_hits 次。
    """

    def __init__(self, path, name, pattern, repl, expect=1, regex=True, min_hits=None):
        self.path = path
        self.name = name
        self.pattern = pattern
        self.repl = repl
        self.expect = expect
        self.regex = regex
        self.min_hits = min_hits

    def count(self, text):
        if self.regex:
            return len(re.findall(self.pattern, text))
        return text.count(self.pattern)

    def apply(self, text):
        if self.regex:
            return re.sub(self.pattern, self.repl, text)
        return text.replace(self.pattern, self.repl)


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def version_from_cargo(root: Path) -> str:
    m = re.search(r'^version = "([^"]+)"$', read(root / "Cargo.toml"), re.M)
    if not m:
        raise SystemExit(f"✗ 读不到 {root}/Cargo.toml 的 [workspace.package] version")
    return m.group(1)


def next_patch(v: str) -> str:
    parts = v.split(".")
    parts[-1] = str(int(parts[-1]) + 1)
    return ".".join(parts)


def mib(n: int) -> str:
    """字节 → MiB 一位小数，**四舍五入**（上一版专门记过：44.9611 要写 45.0，不是 44.9）。"""
    from decimal import ROUND_HALF_UP, Decimal

    return str((Decimal(n) / Decimal(1024 * 1024)).quantize(Decimal("0.1"), rounding=ROUND_HALF_UP))


# --------------------------------------------------------------------------------------
# phase1：已发布态 → 「正在发布」态
# --------------------------------------------------------------------------------------
def rules_phase1(root: Path, old: str, new: str, date: str | None):
    dl = f"{RELEASES_PAGE}/download/v{new}"
    dmg = f"XrayTun_{new}_x86_64_arm64.dmg"
    zipf = f"XrayTun_{new}_x86_64_arm64.zip"
    R = []

    def add(*a, **k):
        R.append(Rule(*a, **k))

    # ---- 版本号来源（8 处 + Cargo.lock）----
    add("Cargo.toml", "[workspace.package] version", f'version = "{old}"', f'version = "{new}"', 1, False)
    add("apps/desktop/tauri.conf.json", "tauri 版本", f'"version": "{old}",', f'"version": "{new}",', 1, False)
    add("apps/ui/package.json", "ui 包版本", f'"version": "{old}",', f'"version": "{new}",', 1, False)
    add("scripts/gen-site-jsonld.py", "jsonld 版本常量", f'XRAYTUN_VERSION = "{old}"', f'XRAYTUN_VERSION = "{new}"', 1, False)
    add("scripts/gen-site-jsonld.py", "jsonld 中文描述", f"v{old}，通用包", f"v{new}，通用包", 1, False)
    add("scripts/gen-site-jsonld.py", "jsonld 英文描述", f"v{old}, universal build", f"v{new}, universal build", 1, False)
    add("scripts/gen-site-jsonld.py", "jsonld PUBLISHED 关掉", "PUBLISHED = True", "PUBLISHED = False", 1, False)
    add("scripts/gen-site-geo.py", "geo 版本常量", f'VERSION = "{old}"', f'VERSION = "{new}"', 1, False)
    add("scripts/gen-site-geo.py", "geo PUBLISHED 关掉", "PUBLISHED = True", "PUBLISHED = False", 1, False)
    # 清空上一版的**真实**字节数：留着它 = 站点带着错数字上线（v0.8.33 就是这样翻车的）
    add("scripts/gen-site-geo.py", "清空 dmg 字节数", r'DMG_BYTES, DMG_MIB = "[^"]*", "[^"]*"', 'DMG_BYTES, DMG_MIB = "", ""')
    add("scripts/gen-site-geo.py", "清空 zip 字节数", r'ZIP_BYTES, ZIP_MIB = "[^"]*", "[^"]*"', 'ZIP_BYTES, ZIP_MIB = "", ""')
    add("scripts/gen-site-geo.py", "清空 sha 字节数", r'SHA_BYTES = "[^"]*"', 'SHA_BYTES = ""')
    add("scripts/gen-site-images.py", "images 版本常量", f'SITE_VERSION = "{old}"', f'SITE_VERSION = "{new}"', 1, False)
    add("scripts/gen-site-images.py", "og 卡片版本角标", f'"v{old}"', f'"v{new}"', 2, False)
    add("site/assets/site.js", "PAGE_VERSION", f'PAGE_VERSION = "{old}"', f'PAGE_VERSION = "{new}"', 1, False)
    add("site/wasm/index.html", "wasm 页版本", f"<strong>v{old}</strong>", f"<strong>v{new}</strong>", 1, False)
    add("site/en/wasm/index.html", "wasm 页版本(en)", f"<strong>v{old}</strong>", f"<strong>v{new}</strong>", 1, False)
    # **清单必须覆盖全部 workspace 成员的版本字段**：漏掉一个，phase1 的
    # "替换后不许残留旧版本号" 校验就会拒绝落盘（P4 新增 xt-intent/xt-mitm 后正是这样被挡住的
    # —— 校验是对的，错的是这份清单没跟着仓库长大）。
    for crate in [
        "xraytun-desktop",
        "xt-core",
        "xt-helper",
        "xt-intent",
        "xt-mitm",
        "xt-proto",
        "xt-tun",
    ]:
        add("Cargo.lock", f"{crate} 版本", f'name = "{crate}"\nversion = "{old}"', f'name = "{crate}"\nversion = "{new}"', 1, False)

    # ---- 两个手写页面：当前版本字面全量替换（顺序在这一步之后才做「过渡态」）----
    for page in ["site/index.html", "site/en/index.html"]:
        add(page, "版本字面全量替换", old, new, None, False, min_hits=10)

    # ---- 已发布态 → 「正在发布」态（依赖上一步已生效）----
    for page in ["site/index.html", "site/en/index.html"]:
        add(page, "pinned 直链 → Releases 页",
            r"https://github\.com/harodggg/xrayTun/releases/download/v" + re.escape(new) + r'/[^"]*',
            RELEASES_PAGE, N_PINNED_PER_PAGE)

    add("site/index.html", "dmg 按钮文案", rf">下载 macOS 版 v{re.escape(new)} · dmg · [\d.]+ MiB</a>",
        f">下载 macOS 版 v{new} · dmg（大小见 Releases）</a>", N_BUTTON_DMG_ZH)
    add("site/index.html", "zip 按钮文案", r">备用 \.zip · [\d.]+ MiB</a>",
        ">备用 .zip（大小见 Releases）</a>", 1)
    add("site/index.html", "meta 行",
        r"v" + re.escape(new) + r"（\d{4}-\d{2}-\d{2} 发布）· 仅 macOS · 通用包（arm64 \+ x86_64）· dmg [\d.]+ MiB",
        f"v{new} 正在发布 · 仅 macOS · 通用包（arm64 + x86_64）· 大小以 Releases 页面为准", 1)
    add("site/index.html", "表格 caption",
        r"真实资产（v" + re.escape(new) + r"，\d{4}-\d{2}-\d{2} 发布）",
        f"真实资产（v{new}）：**正在发布**，资产发布后本表填入**真实字节数**", N_CAPTION_ZH)
    add("site/index.html", "dmg 字节单元格",
        r'<th scope="row">dmg（主）</th>\n              <td>[\d,]+ 字节（[\d.]+ MiB）</td>',
        '<th scope="row">dmg（主）</th>\n              <td>发布后填入（见 Releases）</td>', 1)
    add("site/index.html", "zip 字节单元格",
        r'<th scope="row">zip（备用）</th>\n              <td>[\d,]+ 字节（[\d.]+ MiB）</td>',
        '<th scope="row">zip（备用）</th>\n              <td>发布后填入（见 Releases）</td>', 1)
    add("site/index.html", "sha 字节单元格",
        r'<th scope="row">校验和</th>\n              <td>[\d,]+ 字节</td>',
        '<th scope="row">校验和</th>\n              <td>—</td>', 1)
    if date:
        add("site/index.html", "页脚更新日期", r"页面最后更新 \d{4}-\d{2}-\d{2}", f"页面最后更新 {date}", 1)

    add("site/en/index.html", "dmg 按钮文案",
        r">Download for macOS v" + re.escape(new) + r" · dmg · [\d.]+ MiB</a>",
        f">Download for macOS v{new} · dmg (size per Releases page)</a>", N_BUTTON_DMG_ZH)
    add("site/en/index.html", "zip 按钮文案", r">Alternative \.zip · [\d.]+ MiB</a",
        ">Alternative .zip (size per Releases page)</a", 1)
    add("site/en/index.html", "meta 行",
        r"v" + re.escape(new) + r" \(released \d{4}-\d{2}-\d{2}\) · macOS only · universal build \(arm64 \+ x86_64\) · dmg [\d.]+ MiB",
        f"v{new} publishing now · macOS only · universal build (arm64 + x86_64) · size per Releases page", 1)
    add("site/en/index.html", "表格 caption",
        r"Release assets \(v" + re.escape(new) + r", released \d{4}-\d{2}-\d{2}\)",
        f"Release assets (v{new}): **publishing**; real byte sizes will be filled in here once published", 1)
    add("site/en/index.html", "dmg 字节单元格",
        r'<th scope="row">dmg \(primary\)</th>\n              <td>[\d,]+ bytes \([\d.]+ MiB\)</td>',
        '<th scope="row">dmg (primary)</th>\n              <td>Filled in at release (see Releases)</td>', 1)
    add("site/en/index.html", "zip 字节单元格",
        r'<th scope="row">zip \(alternative\)</th>\n              <td>[\d,]+ bytes \([\d.]+ MiB\)</td>',
        '<th scope="row">zip (alternative)</th>\n              <td>Filled in at release (see Releases)</td>', 1)
    add("site/en/index.html", "sha 字节单元格",
        r'<th scope="row">Checksums</th>\n              <td>[\d,]+ bytes</td>',
        '<th scope="row">Checksums</th>\n              <td>—</td>', 1)
    if date:
        add("site/en/index.html", "页脚更新日期", r"page last updated \d{4}-\d{2}-\d{2}", f"page last updated {date}", 1)
    return R


# --------------------------------------------------------------------------------------
# phase2：正在发布态 → 已发布态
# --------------------------------------------------------------------------------------
def rules_phase2(root: Path, new: str, dmg_b: int, zip_b: int, sha_b: int, date: str):
    dl = f"{RELEASES_PAGE}/download/v{new}"
    dmg = f"XrayTun_{new}_x86_64_arm64.dmg"
    zipf = f"XrayTun_{new}_x86_64_arm64.zip"
    dmg_s, dmg_m = f"{dmg_b:,}", mib(dmg_b)
    zip_s, zip_m = f"{zip_b:,}", mib(zip_b)
    sha_s = f"{sha_b:,}"
    R = []

    def add(*a, **k):
        R.append(Rule(*a, **k))

    add("scripts/gen-site-jsonld.py", "jsonld PUBLISHED 打开", "PUBLISHED = False", "PUBLISHED = True", 1, False)
    add("scripts/gen-site-geo.py", "geo PUBLISHED 打开", "PUBLISHED = False", "PUBLISHED = True", 1, False)
    add("scripts/gen-site-geo.py", "geo 真实 dmg 字节", 'DMG_BYTES, DMG_MIB = "", ""',
        f'DMG_BYTES, DMG_MIB = "{dmg_s}", "{dmg_m}"', 1, False)
    add("scripts/gen-site-geo.py", "geo 真实 zip 字节", 'ZIP_BYTES, ZIP_MIB = "", ""',
        f'ZIP_BYTES, ZIP_MIB = "{zip_s}", "{zip_m}"', 1, False)
    add("scripts/gen-site-geo.py", "geo sha 字节", 'SHA_BYTES = ""', f'SHA_BYTES = "{sha_s}"', 1, False)
    add("scripts/gen-site-geo.py", "geo 发布日", r'LAST_PUB = "[^"]*"', f'LAST_PUB = "{date}"', 1)

    # 中文页
    add("site/index.html", "dmg 按钮文案",
        f'href="{RELEASES_PAGE}"\n            >下载 macOS 版 v{new} · dmg（大小见 Releases）</a>',
        f'href="{dl}/{dmg}"\n            >下载 macOS 版 v{new} · dmg · {dmg_m} MiB</a>', 2, False)
    add("site/index.html", "zip 按钮文案",
        f'href="{RELEASES_PAGE}"\n            >备用 .zip（大小见 Releases）</a>',
        f'href="{dl}/{zipf}"\n            >备用 .zip · {zip_m} MiB</a>', 1, False)
    add("site/index.html", "dmg 单元格",
        '<th scope="row">dmg（主）</th>\n              <td>发布后填入（见 Releases）</td>',
        f'<th scope="row">dmg（主）</th>\n              <td>{dmg_s} 字节（{dmg_m} MiB）</td>', 1, False)
    add("site/index.html", "zip 单元格",
        '<th scope="row">zip（备用）</th>\n              <td>发布后填入（见 Releases）</td>',
        f'<th scope="row">zip（备用）</th>\n              <td>{zip_s} 字节（{zip_m} MiB）</td>', 1, False)
    add("site/index.html", "sha 单元格",
        '<th scope="row">校验和</th>\n              <td>—</td>',
        f'<th scope="row">校验和</th>\n              <td>{sha_s} 字节</td>', 1, False)
    add("site/index.html", "dmg 链接",
        f'href="{RELEASES_PAGE}"\n                  >{dmg}</a>', f'href="{dl}/{dmg}"\n                  >{dmg}</a>', 1, False)
    add("site/index.html", "zip 链接",
        f'href="{RELEASES_PAGE}"\n                  >{zipf}</a>', f'href="{dl}/{zipf}"\n                  >{zipf}</a>', 1, False)
    add("site/index.html", "sha 链接",
        f'href="{RELEASES_PAGE}"\n                  >SHA256SUMS.txt</a>',
        f'href="{dl}/SHA256SUMS.txt"\n                  >SHA256SUMS.txt</a>', 1, False)
    add("site/index.html", "meta 行",
        r"v" + re.escape(new) + r" 正在发布 · 仅 macOS · 通用包（arm64 \+ x86_64）· 大小以 Releases 页面为准",
        f"v{new}（{date} 发布）· 仅 macOS · 通用包（arm64 + x86_64）· dmg {dmg_m} MiB", 1)
    add("site/index.html", "表格 caption",
        r"真实资产（v" + re.escape(new) + r"）：\*\*正在发布\*\*，资产发布后本表填入\*\*真实字节数\*\*",
        f"真实资产（v{new}，{date} 发布）", 1)

    # 英文页
    add("site/en/index.html", "dmg 按钮文案",
        f'href="{RELEASES_PAGE}"\n            >Download for macOS v{new} · dmg (size per Releases page)</a>',
        f'href="{dl}/{dmg}"\n            >Download for macOS v{new} · dmg · {dmg_m} MiB</a>', 2, False)
    add("site/en/index.html", "zip 按钮文案",
        f'href="{RELEASES_PAGE}"\n            >Alternative .zip (size per Releases page)</a',
        f'href="{dl}/{zipf}"\n            >Alternative .zip · {zip_m} MiB</a', 1, False)
    add("site/en/index.html", "dmg 单元格",
        '<th scope="row">dmg (primary)</th>\n              <td>Filled in at release (see Releases)</td>',
        f'<th scope="row">dmg (primary)</th>\n              <td>{dmg_s} bytes ({dmg_m} MiB)</td>', 1, False)
    add("site/en/index.html", "zip 单元格",
        '<th scope="row">zip (alternative)</th>\n              <td>Filled in at release (see Releases)</td>',
        f'<th scope="row">zip (alternative)</th>\n              <td>{zip_s} bytes ({zip_m} MiB)</td>', 1, False)
    add("site/en/index.html", "sha 单元格",
        '<th scope="row">Checksums</th>\n              <td>—</td>',
        f'<th scope="row">Checksums</th>\n              <td>{sha_s} bytes</td>', 1, False)
    add("site/en/index.html", "dmg 链接",
        f'href="{RELEASES_PAGE}"\n                  >{dmg}</a>', f'href="{dl}/{dmg}"\n                  >{dmg}</a>', 1, False)
    add("site/en/index.html", "zip 链接",
        f'href="{RELEASES_PAGE}"\n                  >{zipf}</a>', f'href="{dl}/{zipf}"\n                  >{zipf}</a>', 1, False)
    add("site/en/index.html", "sha 链接",
        f'href="{RELEASES_PAGE}">SHA256SUMS.txt</a>', f'href="{dl}/SHA256SUMS.txt">SHA256SUMS.txt</a>', 1, False)
    add("site/en/index.html", "meta 行",
        r"v" + re.escape(new) + r" publishing now · macOS only · universal build \(arm64 \+ x86_64\) · size per Releases page",
        f"v{new} (released {date}) · macOS only · universal build (arm64 + x86_64) · dmg {dmg_m} MiB", 1)
    add("site/en/index.html", "表格 caption",
        r"Release assets \(v" + re.escape(new) + r"\): \*\*publishing\*\*; real byte sizes will be filled in here once published",
        f"Release assets (v{new}, released {date})", 1)
    return R


# --------------------------------------------------------------------------------------
# 执行：顺序模拟校验 → 统一落盘
# --------------------------------------------------------------------------------------
def run_rules(root: Path, rules, dry: bool, forbid: list | None = None, post=None) -> int:
    state: dict[Path, str] = {}
    orig: dict[Path, str] = {}
    problems = []
    for r in rules:
        f = root / r.path
        if not f.exists():
            problems.append(f"{r.path}: 文件不存在")
            continue
        if f not in state:
            orig[f] = read(f)
            state[f] = orig[f]
        got = r.count(state[f])
        ok = (got == r.expect) if isinstance(r.expect, int) else (got >= (r.min_hits or 1))
        want = r.expect if isinstance(r.expect, int) else f"≥{r.min_hits or 1}"
        print(f"  {'✓' if ok else '✗'} {r.path:32s} {r.name:18s} 命中 {got}（预期 {want}）")
        if not ok:
            problems.append(f"{r.path} / {r.name}: 命中 {got}，预期 {want}")
            continue
        state[f] = r.apply(state[f])

    # 「旧东西不许残留」——这条比任何单条计数都强：v0.8.33 的中间态就是它抓住的形态
    for rel, tok in (forbid or []):
        p = root / rel
        if p in state and tok in state[p]:
            problems.append(f"{rel}: 替换后仍残留 `{tok}`")

    if post:
        problems.extend(post(root, state))

    if problems:
        print("\n✗ 校验失败，**未落盘任何改动**：", file=sys.stderr)
        for p in problems:
            print(f"    {p}", file=sys.stderr)
        return 1, {}

    changed = [f for f in state if state[f] != orig[f]]
    if dry:
        print(f"\n✓ 全部 {len(rules)} 条校验通过（顺序模拟）；干跑，不落盘。将写 {len(changed)} 个文件：")
        for f in changed:
            print(f"    {f.relative_to(root)}")
        return 0, state

    for f in changed:
        f.write_text(state[f], encoding="utf-8")
        print(f"  写入 {f.relative_to(root)}")
    print(f"\n✓ 全部 {len(rules)} 条校验通过（顺序模拟）；已写 {len(changed)} 个文件。")
    return 0, state


def cmd_phase1(a) -> int:
    root = Path(a.repo).resolve()
    old = a.old or version_from_cargo(root)
    new = a.new
    if a.dry_run:
        print(f"[phase1 干跑] {old} → {new}（仓库 {root}）")
    else:
        print(f"[phase1] {old} → {new}（仓库 {root}）")

    forbid = [(f"site/{p}", old) for p in ("index.html", "en/index.html")] + [("Cargo.lock", old)]
    rules = rules_phase1(root, old, new, a.date)

    def post(r, st):
        out = []
        for f, text in st.items():
            rel = f.relative_to(r)
            if old in text:
                out.append(f"{rel}: 仍残留旧版本号 {old}")
        for page in ("site/index.html", "site/en/index.html"):
            p = r / page
            if p in st and f"releases/download/v{new}" in st[p]:
                out.append(f"{page}: 过渡态里不该有 pinned 直链")
        return out

    rc, state = run_rules(root, rules, a.dry_run, forbid=forbid, post=post)
    if rc != 0:
        return rc

    # 删旧 og 卡片：**与切引用同一个提交**（v0.8.35 的 404 窗口就是这么来的）
    stale = [root / "site" / f"og-image-{old}.png", root / "site" / f"og-image-en-{old}.png"]
    # 数**模拟后**的文本：干跑时磁盘上仍是旧引用（拿磁盘数会假红，第一版就是这样）。
    sim_pages = [v for k, v in state.items() if k.suffix == ".html" and "site" in k.parts]
    site_texts = "\n".join(sim_pages) if sim_pages else "\n".join(read(p) for p in (root / "site").rglob("*.html"))
    refs = site_texts.count(f"og-image-{old}.png") + site_texts.count(f"og-image-en-{old}.png")
    print(f"\n[旧 og 卡片] site/** 里对 {old} 的引用 = {refs}（必须 0）")
    if refs != 0:
        print("✗ 引用不为 0，**不删**旧 og 卡片（先查引用为什么还在）", file=sys.stderr)
        return 1
    for p in stale:
        if p.exists():
            if a.dry_run:
                print(f"  （干跑）会删 {p.relative_to(root)}")
            else:
                p.unlink()
                print(f"  删除 {p.relative_to(root)}")

    if a.run_generators:
        rc = run_generators(root, include_images=True)
        if rc != 0:
            return rc
    else:
        print("\n下一步（本工具不自动跑）：")
        print("  python3 scripts/gen-site-jsonld.py gen && python3 scripts/gen-site-jsonld.py check")
        print("  python3 scripts/gen-site-geo.py")
        print("  /usr/local/bin/python3.10 scripts/gen-site-images.py     # 生成新的 og 卡片")
    return 0


def cmd_phase2(a) -> int:
    root = Path(a.repo).resolve()
    new = a.new or version_from_cargo(root)
    if a.dry_run:
        print(f"[phase2 干跑] {new}：真实字节数 dmg={a.dmg_bytes:,} zip={a.zip_bytes:,} sha={a.sha_bytes:,}（{a.date}）")
    else:
        print(f"[phase2] {new}：真实字节数 dmg={a.dmg_bytes:,} zip={a.zip_bytes:,} sha={a.sha_bytes:,}（{a.date}）")

    def post(r, st):
        out = []
        for page in ("site/index.html", "site/en/index.html"):
            p = r / page
            if p in st:
                if "正在发布" in st[p] or "publishing now" in st[p]:
                    out.append(f"{page}: 已发布态里仍有「正在发布」")
                if "see Releases" in st[p] and page.endswith("en/index.html"):
                    pass
        if r / "scripts/gen-site-geo.py" in st and "PUBLISHED = True" not in st[r / "scripts/gen-site-geo.py"]:
            out.append("scripts/gen-site-geo.py: PUBLISHED 未打开")
        return out

    rc, _state = run_rules(root, rules_phase2(root, new, a.dmg_bytes, a.zip_bytes, a.sha_bytes, a.date),
                           a.dry_run, post=post)
    if rc != 0:
        return rc
    if a.run_generators:
        return run_generators(root, include_images=False)
    print("\n下一步（本工具不自动跑）：")
    print("  python3 scripts/gen-site-jsonld.py gen && python3 scripts/gen-site-jsonld.py check")
    print("  python3 scripts/gen-site-geo.py")
    return 0


GENERATORS = [
    (["python3", "scripts/gen-site-jsonld.py", "gen"], "JSON-LD（gen）"),
    (["python3", "scripts/gen-site-jsonld.py", "check"], "JSON-LD（check）"),
    (["python3", "scripts/gen-site-geo.py"], "GEO 索引"),
]
IMAGE_GEN = (["/usr/local/bin/python3.10", "scripts/gen-site-images.py"], "社交卡片/图标")


def run_generators(root: Path, include_images: bool) -> int:
    jobs = list(GENERATORS) + ([IMAGE_GEN] if include_images else [])
    for cmd, label in jobs:
        print(f"\n[生成器] {label}：{' '.join(cmd)}")
        r = subprocess.run(cmd, cwd=root)
        if r.returncode != 0:
            print(f"✗ 生成器失败（{label}）退出码 {r.returncode} —— **不吞**，停下来处理", file=sys.stderr)
            return r.returncode
    return 0


# --------------------------------------------------------------------------------------
# notes：Release 正文 = 本版用户动作（单一来源）+ 固定模板
# --------------------------------------------------------------------------------------
NO_ACTION_SENTENCE = "本版无需用户额外动作"

TEMPLATE = """## 安装

1. 下载 `.dmg` 或 `.zip`，把 `XrayTun.app` 拖进「应用程序」
2. 首次打开会被 Gatekeeper 拦下（这个包是 **ad-hoc 签名、未公证**的）。
   按你的 macOS 版本选一条放行方式：

   **macOS 15 及以上**：右键「打开」**已被 Apple 移除**（2024-08-06 公告）。
   先双击一次让它被拦下，再打开「系统设置 → 隐私与安全性」，
   在「安全性」区域找到关于 XrayTun 的提示，点「仍要打开」，然后确认。

   **macOS 14 及更早**：在「应用程序」里右键（或 Control-点击）
   `XrayTun.app` →「打开」，在弹窗里再点一次「打开」。

   **终端（两个版本都适用）**：
   ```bash
   find /Applications/XrayTun.app -exec xattr -d com.apple.quarantine {} + 2>/dev/null
   ```
   注意 `xattr` 有**两个不同实现**，`-r` 的支持**随实现与版本而异**
   （实测 macOS 26.6.2 / build 25G83）：
   `which -a xattr` 在 PATH 上先命中 Python 的 `xattr` 包，
   `xattr -dr …` → **exit 64**（`option -r not recognized`）；
   而 Apple 的 `/usr/bin/xattr -dr …` → **exit 0**，标记真的被删掉
   （它的 usage 是 `xattr [-l] [-r] [-s] [-v] [-x] file …`，**有 `-r`**）。
   所以：**一律写绝对路径 `/usr/bin/xattr`**；**不要依赖 `-r`**
   （需要递归就用上面那条 `find … -exec … +`）；
   `2>/dev/null` 按上下文用：**产品脚本里不许用**（失败要留痕，例如自动更新
   会写 `app-update.log`）；这条**给用户手动执行的指引**可以保留它 ——
   只为压掉 `No such xattr` 这类刷屏噪声，不影响判断成败。
   另一个独立事实：只给 bundle 根路径的
   `xattr -d com.apple.quarantine /Applications/XrayTun.app`
   只清掉根上那一个（实测 13 个里还剩 12 个），所以要逐文件清。

3. 打开后：侧栏「设置」→「特权助手（helper）」→点「安装 helper」
   （会弹一次管理员密码）。**只有 TUN 模式需要它**，系统代理模式不需要。

## 不需要自己安装 Xray 核心

核心与规则数据**随包附带**，开箱即用：
`Contents/Resources/{xray, geoip.dat, geosite.dat}`。
不用另外下载 Xray，也不用配 `XRAY_LOCATION_ASSET`。

## 关于 Gatekeeper

这个包是 **ad-hoc 签名、未公证**的，因为仓库里没有 Developer ID 证书。
要免打扰分发，需要付费开发者账号，然后在 CI 里加签名与公证步骤。

## 架构

universal（x86_64 + arm64），App / helper / 核心三者都是。

## 这个 App 会改你的系统网络配置

TUN 模式会创建 utun 网卡、接管默认路由并修改 DNS。它通过一个
特权 helper 完成这些改动，**每一条改动都先落盘成快照**，
退出或崩溃后可按快照完整回滚。详见 `docs/02-tun-and-privileges.md`。
"""


def compose_notes(root: Path, tag: str) -> str:
    """本版用户动作 + 固定模板。**动作文件缺/空/没结构 ⇒ 抛异常（调用方非 0 退出）**。"""
    path = root / "docs" / "release-notes" / f"{tag}.md"
    if not path.exists():
        raise SystemExit(
            f"✗ 缺少版本特有的用户动作文件：docs/release-notes/{tag}.md\n"
            f"  约定：每个 tag 都必须有这一份（写明本版用户**必须**做的动作，例如「重新安装特权助手」）。\n"
            f"  若本版确实没有任何必须动作，也要**显式**写一行「{NO_ACTION_SENTENCE}」——\n"
            f"  缺文件/空文件/只有空白都算失败：不许静默退化成通用模板。"
        )
    text = read(path).strip("\n")
    if not text.strip():
        raise SystemExit(f"✗ docs/release-notes/{tag}.md 是空的：{NO_ACTION_SENTENCE} 也请显式写出来。")
    # 允许「整段引用块」的写法（Release 正文里那种 `> ## ⚠️ …`），所以标题前可以有一个 `>`。
    has_heading = any(re.match(r"^\s*>?\s*#{1,6} ", l) for l in text.split("\n"))
    has_no_action = NO_ACTION_SENTENCE in text
    if not (has_heading or has_no_action):
        raise SystemExit(
            f"✗ docs/release-notes/{tag}.md 既没有 Markdown 标题（`## …`，允许写成引用块 `> ## …`），"
            f"也没有「{NO_ACTION_SENTENCE}」这句 ⇒ 无法判定它是不是有意为之（等于漏写）。"
        )
    kind = "明确声明无需额外动作" if (has_no_action and not has_heading) else "有必须的用户动作"
    print(f"[notes] 动作来源 docs/release-notes/{tag}.md（{len(text.encode())} 字节，{kind}）")
    return text + "\n\n---\n\n" + TEMPLATE


def cmd_notes(a) -> int:
    root = Path(a.repo).resolve()
    notes = compose_notes(root, a.tag)
    if a.out == "-":
        sys.stdout.write(notes)
        return 0
    Path(a.out).write_text(notes, encoding="utf-8")
    print(f"[notes] 写出 {a.out}（{len(notes.encode())} 字节）")
    return 0


# --------------------------------------------------------------------------------------
# self-test：夹具仓库上的绿 / 红 / **零落盘**
# --------------------------------------------------------------------------------------
def _fixture(dest: Path, ref: str) -> None:
    """从 git 历史里取一个**已发布态**的树做夹具（默认 v0.8.34 的「提交 2」）。"""
    p1 = subprocess.run(f"git -C {ROOT} archive {ref} | tar -x -C {dest}",
                        shell=True, capture_output=True, text=True)
    if p1.returncode != 0:
        raise SystemExit(f"✗ 夹具解包失败：{p1.stderr}")
    subprocess.run(["git", "init", "-q", str(dest)], check=True)
    subprocess.run(["git", "-C", str(dest), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(dest), "-c", "user.email=t@t", "-c", "user.name=t",
                    "commit", "-qm", "fixture"], check=True)


def _run_tool(tool: Path, *args) -> int:
    """跑**指定那份**脚本（自测要跑被突变的副本，不能调本进程的 main —— 那样突变无效）。"""
    r = subprocess.run([sys.executable, str(tool), *args], capture_output=True, text=True)
    if r.stdout:
        print("    " + r.stdout.strip().replace("\n", "\n    ")[:1200])
    if r.returncode not in (0, 1) and r.stderr:
        print("    stderr: " + r.stderr.strip()[:400])
    return r.returncode


def _status(root: Path) -> str:
    return subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                          capture_output=True, text=True).stdout


def _mutate(src: Path, dest: Path, pairs) -> None:
    text = read(src)
    for old, new in pairs:
        assert text.count(old) == 1, f"突变点不唯一：{old!r} 出现 {text.count(old)} 次"
        text = text.replace(old, new)
    dest.write_text(text, encoding="utf-8")


def cmd_self_test(a) -> int:
    ref = a.fixture_ref
    tmp = Path(tempfile.mkdtemp(prefix="bump-release-selftest-"))
    fails = []
    try:
        fx = tmp / "fixture"
        fx.mkdir()
        _fixture(fx, ref)
        old = version_from_cargo(fx)
        new = next_patch(old)
        print(f"[self-test] 夹具 = `{ref}` 的树（版本 {old}，已发布态）→ 目标 {new}；临时目录 {tmp}")

        # ---- T1 绿：完整 phase1 在夹具上必须成功，且不变量成立 ----
        print("\n--- T1 绿：phase1 干跑 + 真跑 ---")
        rc = _run_tool(Path(__file__).resolve(), "phase1", "--repo", str(fx), "--new", new,
                       "--date", "2026-09-23", "--dry-run")
        after_dry = _status(fx)
        if rc != 0:
            fails.append("T1 干跑应当成功")
        elif after_dry.strip():
            fails.append(f"T1 干跑改动了工作树：{after_dry!r}")
        else:
            print("  ✓ T1a 干跑：退出 0 且 `git status --porcelain` **空**（干跑不写盘）")
        rc = _run_tool(Path(__file__).resolve(), "phase1", "--repo", str(fx), "--new", new,
                       "--date", "2026-09-23")
        st = _status(fx)
        print(f"  T1b 真跑退出码={rc}；git status 变更文件数={len([l for l in st.splitlines() if l.strip()])}")
        if rc != 0:
            fails.append("T1 真跑应当成功")
        else:
            for rel, tok, want in [("Cargo.toml", f'version = "{new}"', True),
                                   ("scripts/gen-site-geo.py", "PUBLISHED = False", True),
                                   ("site/index.html", old, False),
                                   ("site/index.html", "正在发布", True)]:
                text = read(fx / rel)
                if (tok in text) != want:
                    fails.append(f"T1b {rel} 不变量失败：`{tok}` 存在性应为 {want}")
            if not (fx / f"site/og-image-{old}.png").exists():
                print(f"  ✓ T1b 旧 og 卡片 site/og-image-{old}.png 已在同一批里删除")
            else:
                fails.append("T1b 旧 og 卡片没删")

        # ---- T5 绿：phase2 在同一夹具上把「正在发布」推进到已发布态 ----
        # （phase2 的规则在真实仓库上没机会演练 —— 当前仓库已是已发布态；这里用夹具补上，
        #   否则那 28 条规则要等到下一次发版才第一次被执行。）
        print("\n--- T5 绿：phase2（真实字节数 → 已发布态）---")
        rc = _run_tool(Path(__file__).resolve(), "phase2", "--repo", str(fx),
                       "--dmg-bytes", "47431435", "--zip-bytes", "42919784",
                       "--sha-bytes", "200", "--date", "2026-09-23")
        if rc != 0:
            fails.append("T5 phase2 应当成功")
        else:
            geo = read(fx / "scripts/gen-site-geo.py")
            zh = read(fx / "site/index.html")
            checks = [( "PUBLISHED 打开", "PUBLISHED = True" in geo),
                      ("真实 dmg 字节", "47,431,435" in geo),
                      ("MiB 四舍五入为 45.2", "45.2" in geo),
                      ("页面上出现真实字节数", "47,431,435 字节（45.2 MiB）" in zh),
                      ("「正在发布」已消失", "正在发布" not in zh),
                      ("pinned 直链已出现", f"releases/download/v{new}/" in zh)]
            for label, good in checks:
                if good:
                    print(f"  ✓ T5 {label}")
                else:
                    fails.append(f"T5 {label} 不成立")
        _run_tool(Path(__file__).resolve(), "phase2", "--repo", str(fx), "--dmg-bytes", "1",
                  "--zip-bytes", "1", "--sha-bytes", "1", "--date", "2026-09-23")  # 期望失败（已发布态）
        print("  （已发布态上再跑一次 phase2 应当失败 —— 上面那次非 0 就是预期的）")

        # ---- T2 红（计数不符）⇒ 非 0 且**零落盘** ----
        print("\n--- T2 红：把 caption 的预期次数改成错的 ---")
        fx2 = tmp / "fixture-t2"
        shutil.copytree(fx, fx2)
        subprocess.run(["git", "-C", str(fx2), "checkout", "--", "."], check=True)
        subprocess.run(["git", "-C", str(fx2), "clean", "-qfd"], check=True)
        tool2 = tmp / "tool-mutant-t2.py"
        _mutate(Path(__file__).resolve(), tool2,
                [("N_CAPTION_ZH = 1" + "          #", "N_CAPTION_ZH = 2" + "          #")])
        before = _status(fx2)
        rc = _run_tool(tool2, "phase1", "--repo", str(fx2), "--new", new, "--date", "2026-09-23")
        after = _status(fx2)
        if rc == 0:
            fails.append("T2 计数不符却退出 0（假绿）")
        elif after != before:
            fails.append(f"T2 失败后工作树被改动：{after!r}")
        else:
            print(f"  ✓ T2 退出非 0（{rc}）且 `git status --porcelain` 与失败前**逐字相同**"
                  f"（before={before!r} after={after!r}）")

        # ---- T3 红（顺序依赖被破坏）⇒ 必须被抓住 ----
        print("\n--- T3 红：拿**原始文本**校验依赖项（= 破坏顺序模拟）---")
        fx3 = tmp / "fixture-t3"
        shutil.copytree(fx, fx3)
        subprocess.run(["git", "-C", str(fx3), "checkout", "--", "."], check=True)
        subprocess.run(["git", "-C", str(fx3), "clean", "-qfd"], check=True)
        tool3 = tmp / "tool-mutant-t3.py"
        _mutate(Path(__file__).resolve(), tool3,
                [("got = r.count(state" + "[f])", "got = r.count(orig" + "[f])")])
        rc = _run_tool(tool3, "phase1", "--repo", str(fx3), "--new", new, "--date", "2026-09-23")
        if rc == 0:
            fails.append("T3 破坏顺序模拟却仍然通过 ⇒ 顺序依赖没有被真正校验")
        else:
            print(f"  ✓ T3 退出非 0（{rc}）：依赖前面替换的 pattern 在原始文本上命中不足，被抓住了")

        # ---- T4 notes：缺文件必须失败；有文件必须进正文 ----
        print("\n--- T4 notes：缺动作文件 / 有动作 / 显式声明无动作 ---")
        fx4 = tmp / "fixture-t4"
        shutil.copytree(fx, fx4)
        rc = _run_tool(Path(__file__).resolve(), "notes", "--repo", str(fx4), "--tag", "v" + new,
                       "--out", str(tmp / "n1.md"))
        if rc == 0:
            fails.append("T4 缺动作文件却成功（静默退化）")
        else:
            print(f"  ✓ T4a 缺 docs/release-notes/v{new}.md ⇒ 退出非 0（{rc}）")
        (fx4 / "docs" / "release-notes").mkdir(parents=True, exist_ok=True)
        (fx4 / "docs" / "release-notes" / f"v{new}.md").write_text(
            "## ⚠️ 本版需要重新安装特权助手\n\n示例动作。\n", encoding="utf-8")
        rc = _run_tool(Path(__file__).resolve(), "notes", "--repo", str(fx4), "--tag", "v" + new,
                       "--out", str(tmp / "n2.md"))
        n2 = Path(tmp / "n2.md").read_text(encoding="utf-8")
        if rc != 0 or "示例动作" not in n2 or "## 安装" not in n2 or n2.count("---") < 1:
            fails.append("T4b 有动作文件却没合成出「动作 + 模板」")
        else:
            print(f"  ✓ T4b 动作段在最前、固定模板在后（{len(n2.encode())} 字节）")
        (fx4 / "docs" / "release-notes" / f"v{new}.md").write_text(
            f"{NO_ACTION_SENTENCE}\n", encoding="utf-8")
        rc = _run_tool(Path(__file__).resolve(), "notes", "--repo", str(fx4), "--tag", "v" + new,
                       "--out", str(tmp / "n3.md"))
        if rc != 0:
            fails.append("T4c 显式「无需额外动作」应当通过")
        else:
            print("  ✓ T4c 显式声明「本版无需用户额外动作」也允许（但必须写出来）")

        print(f"\n[self-test] 结论：{'全部通过' if not fails else '有失败'}")
        for f in fails:
            print(f"  ✗ {f}", file=sys.stderr)
        return 1 if fails else 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def build_parser():
    p = argparse.ArgumentParser(description="发布机械动作（先全部校验，再统一落盘）")
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp):
        sp.add_argument("--repo", default=str(ROOT), help="仓库根（默认：本脚本上一级）")
        sp.add_argument("--dry-run", action="store_true", help="只打印命中次数与将写的文件，不落盘")
        sp.add_argument("--run-generators", action="store_true", help="落盘后顺便跑生成器（失败即非 0）")
        sp.add_argument("--quiet-rules", action="store_true", help=argparse.SUPPRESS)

    s1 = sub.add_parser("phase1", help="已发布态 → 「正在发布」态")
    common(s1)
    s1.add_argument("--new", required=True)
    s1.add_argument("--old", default=None, help="默认从 Cargo.toml 读")
    s1.add_argument("--date", default=None, help="页脚「最后更新」日期（YYYY-MM-DD）")

    s2 = sub.add_parser("phase2", help="「正在发布」态 → 已发布态")
    common(s2)
    s2.add_argument("--new", default=None, help="默认从 Cargo.toml 读")
    s2.add_argument("--dmg-bytes", type=int, required=True)
    s2.add_argument("--zip-bytes", type=int, required=True)
    s2.add_argument("--sha-bytes", type=int, default=200)
    s2.add_argument("--date", required=True)

    s3 = sub.add_parser("notes", help="合成 Release 正文")
    s3.add_argument("--repo", default=str(ROOT))
    s3.add_argument("--tag", required=True, help="例如 v0.8.36")
    s3.add_argument("--out", required=True, help="输出文件；`-` 表示 stdout")
    s3.add_argument("--quiet-rules", action="store_true", help=argparse.SUPPRESS)

    s4 = sub.add_parser("self-test", help="夹具仓库上的绿/红/零落盘测试")
    s4.add_argument("--fixture-ref", default="442d65d",
                    help="夹具 = 该提交的树（默认 442d65d = v0.8.34 的「提交 2」已发布态）")
    return p


def main(argv=None) -> int:
    a = build_parser().parse_args(argv)
    quiet = getattr(a, "quiet_rules", False)
    if quiet:
        import io
        import contextlib

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = {"phase1": cmd_phase1, "phase2": cmd_phase2, "notes": cmd_notes,
                  "self-test": cmd_self_test}[a.cmd](a)
        return rc
    return {"phase1": cmd_phase1, "phase2": cmd_phase2, "notes": cmd_notes,
            "self-test": cmd_self_test}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
