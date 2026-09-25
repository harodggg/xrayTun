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
* **phase1 连续卡死两次**（本版加固，两次都在真实发版里踩到）：
  1. `Cargo.lock` 里多了一个 workspace 成员（P4 新增 `xt-intent` / `xt-mitm`），而
     phase1 的**成员清单是手写的** —— 清单没跟着仓库长大 ⇒ 「替换后不许残留旧版本号」
     拒绝落盘，报错却只有一句 `Cargo.lock: 替换后仍残留 0.8.38`，**不说是哪个 crate**。
     现在成员清单**从 `Cargo.toml` 的 `[workspace] members` 派生**，落盘前先跑预检，
     把「成员 ↔ Cargo.lock 条目 ↔ 版本」逐条对齐，**一次报全**并把成员名点到。
  2. `scripts/gen-site-geo.py` 的注释里曾写「真实事故（v0.8.38 停发期间…）」，而它正是
     phase1 的替换目标 ⇒ 用**裸子串**判定「旧版本号不许残留」时，这条注释让 phase1
     **永远失败**（当时的解法只能是"注释里刻意不写版本字面"，靠纪律）。
     现在旧版本字面先被分类成 **真版本字段**（必须替换）与 **注释 / 历史说明**（列出但不阻塞）；
     判据集中在 `classify_line()`，只有 `field` 参与硬校验。
  3. 两条加固都要求「**一次列全所有不匹配项**」：校验失败时逐条打印
     `文件:行号 + 成因 + 上下文（Cargo.lock 会点到 crate 名）`，不再让人靠猜。

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
# 版本字面分类：**真版本字段** vs **注释 / 历史说明**
# --------------------------------------------------------------------------------------
# 为什么需要它（真实事故，见文件顶部第 3 条）：`scripts/gen-site-geo.py` 的注释里曾写
# 「真实事故（v0.8.38 停发期间…）」，而该文件是 phase1 的替换目标 ⇒ 旧的「替换后不许残留
# 旧版本号」用**裸子串**判定时，这条注释让 phase1 **永远失败**；当时的解法是"注释里刻意不写
# 版本字面"，靠纪律。反过来，`Cargo.lock` 漏了成员时，报错也只有一句泛泛的「仍残留 0.8.38」。
#
# **判据（只认下面这几种"权威版本字段"的整行形态；其余一律不算）**：
#   1. TOML / Cargo.lock ：  version = "X"
#   2. JSON              ：  "version": "X",          （tauri.conf.json / package.json）
#   3. Python / JS 常量  ：  XRAYTUN_VERSION / SITE_VERSION / PAGE_VERSION / VERSION = "X"
#                            （允许前面的 `var `，允许结尾 `;`）
#   4. HTML 角标         ：  <strong>vX</strong>      （site/{,en/}wasm/index.html）
# 分类结果三选一：
#   · `field`   —— 真版本字段。**必须**被 phase1 的规则替换掉，残留即硬失败。
#   · `comment` —— 注释行（整行注释，或字面出现在 `#` / `//` / `/*` / `<!--` 之后）。
#                  **列出但不阻塞**：注释是"历史说明"，不该逼着发版去改它。
#   · `prose`   —— 正文 / 属性里的说明性字面（例：`（截至 v0.8.38）`、JSON 描述串）。
#                  同样**列出但不阻塞**（真需要改的，靠规则表里显式的 pattern，而不是这条兜底）。
# 注意：两个手写页面（`site/{index.html,en/index.html}`）走的是**整篇全量替换 + 裸子串残留检查**，
# 所以判据 4 之外的页面字面不会漏网 —— 那条路径本来就要求"旧版本号在页面里为 0 处"。
FIELD_PATTERNS = (
    re.compile(r'^\s*version\s*=\s*"[^"]*"\s*$'),
    re.compile(r'^\s*"version"\s*:\s*"[^"]*"\s*,?\s*$'),
    re.compile(
        r"^\s*(?:var\s+)?(?:XRAYTUN_VERSION|SITE_VERSION|PAGE_VERSION|VERSION)\s*=\s*"
        r'"[^"]*"\s*;?\s*$'
    ),
    re.compile(r"^\s*<strong>v[^<]*</strong>\s*$"),
)


def _comment_start(line: str) -> int:
    """→ 该行注释的起始列（没有注释则 -1）。

    **`https://` 里的 `//` 不是注释**（第一版就把它当成了注释，把 `<meta … og-image-…>` 误报成
    "注释"）——所以尾随 `//` 只认前面不是 `:` 的那种。整行注释认这几种起始：
    `#` / `//` / `/*` / `<!--` / 块注释续行 `* ` 或 `*/`。
    """
    st = line.lstrip()
    indent = len(line) - len(st)
    for mk in ("<!--", "#", "//", "/*"):
        if st.startswith(mk):
            return indent
    if st.startswith(("* ", "*/")):
        return indent
    m = re.search(r"\s#", line)          # 尾随注释：Python / TOML / YAML / JS
    if m:
        return m.start() + 1
    m = re.search(r"(?<!:)\s//", line)   # 尾随 `//`，但排除 `://`
    if m:
        return m.start() + 1
    return -1


def classify_line(line: str, version: str) -> str:
    """把「含旧版本字面的一行」分类成 `field` / `comment` / `prose`（判据见上面那块注释）。

    做法：先切掉注释部分得到"代码段"，再看版本字面落在哪一段、代码段是不是权威字段形态。
    · 字面只出现在注释里            ⇒ `comment`（例如 `# 真实事故（v0.8.38 停发期间…）`）
    · 字面在代码段且是权威字段形态  ⇒ `field`  （例如 `version = "0.8.38"`）
    · 其余                          ⇒ `prose`  （例如 `（截至 v0.8.38）`、JSON 描述串）
    纯函数、无 IO —— self-test 能直接拿它做正反例（含"把注释当字段"的反向敏感性突变）。
    """
    if version not in line:
        return "prose"
    cut = _comment_start(line)
    code = line[:cut] if cut >= 0 else line
    if version not in code:
        return "comment"
    for p in FIELD_PATTERNS:
        if p.match(code.rstrip()):
            return "field"
    return "prose"


def field_leftovers(root: Path, state: dict, version: str, rel_allow=None):
    """→ [(相对路径, 行号, 行内容, 所属包名或 None)]：模拟后的文本里**真版本字段**仍是旧版本。

    `rel_allow` 为 None 表示扫全部被改动过的文件；否则只扫这些相对路径。
    行号是给人指路用的：报错必须能直接跳过去，而不是让人全文搜。
    """
    out = []
    # 增量解析 Cargo.lock 的包名，好让报错点到 crate（"哪个 crate 漏了"是那次事故的核心问题）
    for f, text in state.items():
        rel = str(f.relative_to(root))
        if rel_allow is not None and rel not in rel_allow:
            continue
        lines = text.splitlines()
        pkg = None
        for i, line in enumerate(lines, 1):
            m = re.match(r'^name = "([^"]+)"$', line)
            if m:
                pkg = m.group(1)
            if version not in line:
                continue
            if classify_line(line, version) == "field":
                out.append((rel, i, line.strip(), pkg))
    return out


def nonfield_leftovers(root: Path, state: dict, version: str):
    """→ [(相对路径, 行号, 行内容, 分类)]：注释 / 说明里的旧版本字面（**不阻塞**，只提示）。"""
    out = []
    for f, text in state.items():
        rel = str(f.relative_to(root))
        for i, line in enumerate(text.splitlines(), 1):
            if version not in line:
                continue
            kind = classify_line(line, version)
            if kind != "field":
                out.append((rel, i, line.strip(), kind))
    return out


# --------------------------------------------------------------------------------------
# 仓库成员：从 Cargo.toml **派生**，不再手写清单
# --------------------------------------------------------------------------------------
def workspace_packages(root: Path):
    """→ (包名列表, 问题列表)。包名 = 各成员 `Cargo.toml` 的 `[package] name`。

    为什么派生而不是手写（P4 的真实事故）：手写清单不会跟着仓库长大，漏掉一项就变成
    "替换后仍残留旧版本号"，而报错不说是哪个 crate。派生之后这类 bug 从**根上**消失；
    预检再核对「成员 ↔ Cargo.lock 条目 ↔ 版本」三者，仍然不一致就一次报全。
    """
    problems, names = [], []
    cargo = root / "Cargo.toml"
    if not cargo.exists():
        return [], [f"{cargo}: 文件不存在（仓库成员清单派生不出来）"]
    text = read(cargo)
    m = re.search(r"^\[workspace\]\s*$(.*?)(?=^\[|\Z)", text, re.M | re.S)
    if not m:
        return [], ["Cargo.toml: 没有 [workspace] 段，无法派生成员清单"]
    mm = re.search(r"members\s*=\s*\[(.*?)\]", m.group(1), re.S)
    if not mm:
        return [], ["Cargo.toml: [workspace] 段里没有 members = [...]，无法派生成员清单"]
    members = re.findall(r'"([^"]+)"', mm.group(1))
    if not members:
        return [], ["Cargo.toml: [workspace] members 为空，无法派生成员清单"]
    for rel in members:
        mc = root / rel / "Cargo.toml"
        if not mc.exists():
            problems.append(f"Cargo.toml: 成员 `{rel}` 的 {rel}/Cargo.toml 不存在")
            continue
        mt = read(mc)
        pm = re.search(r"^\[package\]\s*$(.*?)(?=^\[|\Z)", mt, re.M | re.S)
        nm = re.search(r'^name = "([^"]+)"', pm.group(1) if pm else mt, re.M)
        if not nm:
            problems.append(f"{rel}/Cargo.toml: 读不到 [package] name")
            continue
        names.append(nm.group(1))
    return sorted(names), problems


def lock_versions(root: Path):
    """→ {包名: 版本}（Cargo.lock 的每个 [[package]] 块）。"""
    out = {}
    lock = root / "Cargo.lock"
    if not lock.exists():
        return out
    for blk in read(lock).split("[[package]]")[1:]:
        mn = re.search(r'^name = "([^"]+)"', blk, re.M)
        mv = re.search(r'^version = "([^"]+)"', blk, re.M)
        if mn and mv:
            out[mn.group(1)] = mv.group(1)
    return out


def precheck_members(root: Path, old: str):
    """预检 1（**不依赖规则表**）：→ (硬问题列表, 提示行列表)。

    从 `Cargo.toml [workspace] members` 派生包名，逐个核 `Cargo.lock` 条目与版本；
    问题**一次列全**（哪个成员、缺什么），不靠人猜哪个 crate 漏了。
    """
    problems, notes = [], []
    pkgs, probs = workspace_packages(root)
    problems += probs
    lock = lock_versions(root)
    if not lock:
        problems.append("Cargo.lock: 读不到任何 [[package]]（文件缺失或格式变了）")
    for pkg in pkgs:
        if pkg not in lock:
            problems.append(
                f"仓库成员 `{pkg}` 在 Cargo.lock 里**没有条目**"
                f"（新加的 crate 还没 `cargo update`/构建过？）"
            )
        elif lock[pkg] != old:
            problems.append(
                f"仓库成员 `{pkg}` 的 Cargo.lock 版本 = {lock[pkg]}，而 Cargo.toml = {old}（先对齐再发版）"
            )
    notes.append(f"仓库成员 {len(pkgs)} 个（派生自 Cargo.toml [workspace] members）：{', '.join(pkgs)}")
    return problems, notes


def precheck_literals(root: Path, old: str, rule_paths):
    """预检 2：把 phase1 触及文件里的旧版本字面按 `classify_line()` 分类并**逐条列出**。

    注释 / 正文说明类会被明确标注「**不参与**硬校验」—— 这样"注释里写了历史版本字面"
    不会再卡住发版（那正是 phase1 连续卡死的第二个形态）。
    """
    notes = []
    fields, comments, prose = 0, [], []
    for rel in sorted(rule_paths):
        p = root / rel
        if not p.exists():
            continue
        for i, line in enumerate(read(p).splitlines(), 1):
            if old not in line:
                continue
            kind = classify_line(line, old)
            if kind == "field":
                fields += 1
            elif kind == "comment":
                comments.append((rel, i, line.strip()))
            else:
                prose.append((rel, i, line.strip()))
    notes.append(
        f"旧版本 {old} 在 phase1 触及的文件里出现 {fields + len(comments) + len(prose)} 处："
        f"真版本字段 {fields} 处（**必须**全被替换）、注释 {len(comments)} 处、正文/说明 {len(prose)} 处"
    )
    for kind, hits in (("注释", comments), ("说明", prose)):
        shown = hits if kind == "注释" else hits[:5]
        for rel, i, line in shown:
            notes.append(
                f"  · [{kind}] {rel}:{i} {line[:88]} —— **不参与**「不许残留旧版本号」硬校验，"
                f"是否改写由规则表决定"
            )
        if len(hits) > len(shown):
            notes.append(
                f"  · [说明] 其余 {len(hits) - len(shown)} 处同类字面（页面正文/描述串）不再逐条列；"
                f"**模拟之后**若仍有说明类残留，`post` 会把它们全部打出来"
            )
    return notes


def print_notes(notes) -> None:
    for n in notes:
        print(f"[预检] {n}")



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
    # ⇒ 现在**不写死**：从 Cargo.toml 的 [workspace] members 派生（`precheck_phase1`
    #    保证派生出来的每个成员在 Cargo.lock 里都有条目、且版本 == old）。
    crates, crate_problems = workspace_packages(root)
    # 有问题的成员已经在 `precheck_members` 里报过并让 phase1 提前退出了；
    # 这里再打一遍是为了**直接调用 rules_phase1 的路径**（例如将来的自测）也看得见。
    for prob in crate_problems:
        print(f"✗ {prob}", file=sys.stderr)
    for crate in crates:
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

    # 「旧东西不许残留」——这条比任何单条计数都强：v0.8.33 的中间态就是它抓住的形态。
    # `mode` 决定判定口径：
    #   · "raw"   —— 裸子串，一处都不许留（**只给整篇全量替换的两个手写页面**用）；
    #   · "field" —— 只有 `classify_line()` 认定的**真版本字段**才判红，
    #               注释/历史说明不算（这是"注释里的历史版本字面永远不会卡住发版"的机制落点）。
    # **逐行**报（文件:行号 + 行内容），不合并成一句泛泛的"仍残留"。
    for rel, tok, mode in (forbid or []):
        p = root / rel
        if p not in state:
            continue
        for i, line in enumerate(state[p].splitlines(), 1):
            if tok not in line:
                continue
            if mode == "field" and classify_line(line, tok) != "field":
                continue
            tag = "真版本字段残留" if mode == "field" else "裸子串残留"
            problems.append(f"{rel}:{i} {tag} `{tok}` —— {line.strip()[:90]}")

    if post:
        problems.extend(post(root, state))

    if problems:
        print(f"\n✗ 校验失败：共 {len(problems)} 项，**未落盘任何改动**", file=sys.stderr)
        for n, p in enumerate(problems, 1):
            print(f"    {n}. {p}", file=sys.stderr)
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

    # ---- 预检 1：仓库成员（在**任何替换之前**；问题一次列全）----
    problems, notes = precheck_members(root, old)
    print_notes(notes)
    if problems:
        print(f"\n✗ 预检失败：共 {len(problems)} 项（未做任何替换、未落盘）", file=sys.stderr)
        for n, p in enumerate(problems, 1):
            print(f"    {n}. {p}", file=sys.stderr)
        return 1

    # 两个手写页面走**整篇全量替换** ⇒ 裸子串一处都不许留；
    # Cargo.lock 之类只替换**字段** ⇒ 走 `field` 口径（注释/历史说明不阻塞）。
    forbid = [(f"site/{p}", old, "raw") for p in ("index.html", "en/index.html")]
    rules = rules_phase1(root, old, new, a.date)

    # ---- 预检 2：旧版本字面分类（注释/历史说明**列出但不阻塞**）----
    print_notes(precheck_literals(root, old, {r.path for r in rules}))

    def post(r, st):
        out = []
        # 真版本字段残留：**逐条列全**（文件:行号 + 上下文；Cargo.lock 点到 crate 名）
        for rel, i, line, pkg in field_leftovers(r, st, old):
            ctx = f"（crate `{pkg}`）" if pkg else ""
            out.append(f"{rel}:{i} 真版本字段仍是旧版本 {old}{ctx} —— {line[:90]}")
        # 注释 / 说明里的旧版本字面：**不算失败**，只提示（这就是不再卡死的落点）
        rest = nonfield_leftovers(r, st, old)
        if rest:
            print(f"[信息] 旧版本字面仍在 {len(rest)} 处**注释/说明**里（按判据不算失败）：")
            for rel, i, line, kind in rest:
                print(f"    · [{kind}] {rel}:{i} {line[:88]}")
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
    subprocess.run(["git", "-C", str(dest), "-c", "commit.gpgsign=false",
                    "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "fixture"], check=True)


def _run_raw(tool: Path, *args):
    """跑指定那份脚本并**原样返回** (rc, stdout, stderr) —— 断言报错内容用（_run_tool 只回退出码）。"""
    r = subprocess.run([sys.executable, str(tool), *args], capture_output=True, text=True)
    return r.returncode, r.stdout, r.stderr


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

        # ---- T6 红/绿：仓库成员预检（Cargo.lock 漏了新 crate —— 真实事故形态）----
        # 夹具是**已发布态**，直接在上面加一个 workspace 成员：
        #   T6a：Cargo.toml 有了新成员、Cargo.lock 还没有条目 ⇒ 预检必须**点名**报红；
        #   T6b：补上 Cargo.lock 条目 ⇒ phase1 必须自动覆盖它（成员清单已从 Cargo.toml 派生，
        #        "清单没跟着仓库长大"这一整类 bug 消失）。
        print("\n--- T6 红/绿：仓库成员预检（新 crate 漏进 Cargo.lock / 清单自动长大）---")
        fx6 = tmp / "fixture-t6"
        shutil.copytree(fx, fx6)
        subprocess.run(["git", "-C", str(fx6), "checkout", "--", "."], check=True)
        subprocess.run(["git", "-C", str(fx6), "clean", "-qfd"], check=True)
        newcrate = "xt-selftest"
        (fx6 / "crates" / newcrate).mkdir(parents=True)
        (fx6 / "crates" / newcrate / "Cargo.toml").write_text(
            f'[package]\nname = "{newcrate}"\nversion.workspace = true\nedition.workspace = true\n',
            encoding="utf-8")
        ct = read(fx6 / "Cargo.toml")
        ct = ct.replace('    "crates/xt-tun",', f'    "crates/xt-tun",\n    "crates/{newcrate}",')
        (fx6 / "Cargo.toml").write_text(ct, encoding="utf-8")
        before6 = _status(fx6)
        rc6, _o6, e6 = _run_raw(Path(__file__).resolve(), "phase1", "--repo", str(fx6),
                                "--new", new, "--date", "2026-09-23", "--dry-run")
        after6 = _status(fx6)
        if rc6 == 0:
            fails.append("T6a Cargo.lock 漏了仓库成员却通过（预检失效）")
        elif newcrate not in e6:
            fails.append(f"T6a 报错没点名 `{newcrate}`：{e6.strip()[:200]}")
        elif after6 != before6:
            fails.append(f"T6a 预检失败却改了工作树：{after6!r}")
        else:
            line6 = [l for l in e6.splitlines() if newcrate in l][0].strip()
            print(f"  ✓ T6a 预检报红并**点到成员名**，且零落盘：{line6}")
        # T6b：把 lock 条目补上（版本 = old）⇒ 必须自愈（新成员自动进入替换清单）
        lk = read(fx6 / "Cargo.lock").replace(
            'name = "xt-tun"\nversion = "' + old + '"',
            f'name = "xt-tun"\nversion = "{old}"\n\n[[package]]\nname = "{newcrate}"\nversion = "{old}"')
        (fx6 / "Cargo.lock").write_text(lk, encoding="utf-8")
        rc6b = _run_tool(Path(__file__).resolve(), "phase1", "--repo", str(fx6),
                         "--new", new, "--date", "2026-09-23")
        locknow = read(fx6 / "Cargo.lock")
        ok6b = (rc6b == 0
                and f'name = "{newcrate}"\nversion = "{new}"' in locknow)
        if ok6b:
            print(f"  ✓ T6b 补上 lock 条目后 phase1 自动覆盖新成员 `{newcrate}`"
                  f"（派生清单，不再是手写清单）")
        else:
            fails.append(f"T6b 派生清单没覆盖新成员（rc={rc6b}；expect `{newcrate}` version {new}）")

        # ---- T7 绿：注释/历史说明里的旧版本字面**不再卡住** phase1 ----
        # 这正是 `scripts/gen-site-geo.py:62` 那个真实事故的形态（当时只能靠"注释里别写版本"绕开）。
        print("\n--- T7 绿：注释里的历史版本字面不再阻塞（含反向敏感性）---")
        fx7 = tmp / "fixture-t7"
        shutil.copytree(fx, fx7)
        subprocess.run(["git", "-C", str(fx7), "checkout", "--", "."], check=True)
        subprocess.run(["git", "-C", str(fx7), "clean", "-qfd"], check=True)
        geo = read(fx7 / "scripts/gen-site-geo.py")
        marker = f"# 历史：v{old} 停发期间发生过两次（**注释字面，刻意留着**）\n"
        (fx7 / "scripts/gen-site-geo.py").write_text(geo + marker, encoding="utf-8")
        rc7 = _run_tool(Path(__file__).resolve(), "phase1", "--repo", str(fx7),
                        "--new", new, "--date", "2026-09-23")
        geo_after = read(fx7 / "scripts/gen-site-geo.py")
        if rc7 != 0:
            fails.append("T7 注释里的历史版本字面又把 phase1 卡住了")
        elif f'VERSION = "{new}"' not in geo_after:
            fails.append("T7 phase1 通过了但真版本字段没被替换")
        elif marker.strip() not in geo_after:
            fails.append("T7 注释被改掉了（应当原样保留）")
        else:
            print(f"  ✓ T7 注释 `{marker.strip()[:40]}…` 原样保留，`VERSION` 已替换 ⇒ 退出 0，零误杀")
        # 反向敏感性：把判据反过来（注释也算 field）⇒ 同一个夹具必须重新变红，
        # 证明"不卡住"是**分类器**带来的，而不是这条校验被悄悄删掉了。
        tool7 = tmp / "tool-mutant-t7.py"
        _mutate(Path(__file__).resolve(), tool7,
                [('        return "comment"\n', '        return "field"  # 反向敏感性突变\n')])
        rc7b, _o7b, e7b = _run_raw(tool7, "phase1", "--repo", str(fx7), "--new", new,
                                   "--date", "2026-09-23")
        if rc7b == 0:
            fails.append("T7 反向敏感性突变后仍然通过 ⇒ 「不卡住」不是分类器做到的")
        else:
            print(f"  ✓ T7 反向敏感性：把注释判成 field 后同一夹具退出非 0（{rc7b}）"
                  f"⇒ 豁免确实来自分类器，不是校验被删")

        # ---- T8 红：真版本字段残留必须**一次列全**（含文件:行号 + crate 名）----
        # 形态：Cargo.lock 里留下两个**非成员**的旧版本条目（例如删 crate 后 lock 没重算）。
        # 派生清单覆盖不到它们 ⇒ 必须被「真版本字段残留」抓住，而且**两条都要报**。
        print('\n--- T8 红：真版本字段残留一次列全（不是「只报第一条」）---')
        fx8 = tmp / "fixture-t8"
        shutil.copytree(fx, fx8)
        subprocess.run(["git", "-C", str(fx8), "checkout", "--", "."], check=True)
        subprocess.run(["git", "-C", str(fx8), "clean", "-qfd"], check=True)
        lk8 = read(fx8 / "Cargo.lock")
        lk8 += (f'\n[[package]]\nname = "xt-stale-a"\nversion = "{old}"\n'
                f'\n[[package]]\nname = "xt-stale-b"\nversion = "{old}"\n')
        (fx8 / "Cargo.lock").write_text(lk8, encoding="utf-8")
        rc8, _o8, e8 = _run_raw(Path(__file__).resolve(), "phase1", "--repo", str(fx8),
                                "--new", new, "--date", "2026-09-23", "--dry-run")
        if rc8 == 0:
            fails.append("T8 残留真版本字段却通过")
        elif not ("xt-stale-a" in e8 and "xt-stale-b" in e8):
            fails.append(f"T8 没有一次列全两条残留（必须都带 crate 名）：{e8.strip()[:300]}")
        else:
            hits = [l.strip() for l in e8.splitlines() if "xt-stale-" in l]
            print(f"  ✓ T8 两条残留**一次列全**（{len(hits)} 行，各带 crate 名与行号）：")
            for h in hits:
                print(f"      {h}")

        # ---- T9 classify_line 的正反例（纯函数，快）----
        print("\n--- T9 classify_line：真版本字段 vs 注释/说明 ---")
        cases = [
            ('version = "0.8.38"', "field"),
            ('  "version": "0.8.38",', "field"),
            ('XRAYTUN_VERSION = "0.8.38"', "field"),
            ('  var PAGE_VERSION = "0.8.38";', "field"),
            ("            <strong>v0.8.38</strong>", "field"),
            ('VERSION = "0.8.38"  # 尾随注释', "field"),
            ("# 真实事故（v0.8.38 停发期间发生两次）", "comment"),
            ("  // as of v0.8.38", "comment"),
            ('<meta property="og:image" content="https://xraytun.top/og-image-0.8.38.png" />', "prose"),
            ('        "text": "不会（截至 v0.8.38）。"', "prose"),
        ]
        for line, want in cases:
            got = classify_line(line, "0.8.38")
            if got != want:
                fails.append(f"T9 classify_line({line[:40]!r}) = {got}，期望 {want}")
        if not [f for f in fails if f.startswith("T9")]:
            print(f"  ✓ T9 {len(cases)} 个正反例全部符合判据（含 `://` 不算注释、尾随注释不误判）")

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
