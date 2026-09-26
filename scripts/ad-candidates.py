#!/usr/bin/env python3
"""ad-candidates.py —— 从 XrayTun 日志里捞「高置信投放端」候选（只读、离线、不自动下发）

## 这是什么、不是什么

用户给的产品方向是「**先判域名，再判内容去验证**」。这个工具只做**第一层**：

* **是**：把本机日志里出现过的域名，用**标签边界**规则筛出"名字本身就是投放/追踪
  基础设施"的那一批，给人看；
* **不是**：不是 AI、不联网、不读页面内容、不自动下发规则、**不改任何配置**。
  它给的是**候选**，要不要拦由人在界面里勾选（自定义规则/静态名单）。

## 匹配规则（**标签边界**，不是子串）

一个主机名按 `.` 拆成标签，标签再按 `-` / `_` 拆成段。命中条件只有两条：

1. **段完全等于**已知广告/追踪标签（`ad` / `ads` / `analytics` / `tracker` /
   `beacon` / `pixel` / `sponsor` / `badjs` / `ogads` / `doubleclick` …）；
2. **整个标签以**已知广告**前缀**开头，且后面是空的或数字/分隔符
   （`adserver1`、`tracker-cdn` 这种；`adobe` 不会命中，因为 `ad` **不在**前缀表里）。

刻意**不做**宽松子串匹配：`status.<x>`、`polkadot.js.org`、`_dns-sd._udp…` 这类
会被误伤（实测宽匹配能捞 60 条、精度极低）。需要看误伤长什么样时加
`--include-weak`：它单独一层列出来，**默认不进候选表**。

## 它漏什么（一定要和命中一起看）

* **名字里没有广告字的投放端全漏**：自建广告服务（`ads.<你的域名>` 之外的
  `promo.<x>`、`<x>-cdn`）、第一方广告 API（`youtubei.googleapis.com`）、
  CNAME 伪装过的追踪域名（`metrics.<大站>`）；
* 只在**页面/接口响应内容**里能看出来的广告位（那属于第二层：MITM 验证，
  见 `docs/verification/DOMAIN-FIRST-AD-FILTERING.md`）；
* 日志里**没有出现过**的域名（没有访问就没有观测）；
* **名字像广告但不是**的域名会出现在候选里（例如公司自建的 `ads.<自己域名>`
  可能是他们自己的投放后台）——所以必须人工确认，工具不替用户决定。

## 隐私

* 输入是**浏览记录**，输出（含域名）**只能写到仓库外**（`/tmp` 等）。
  `--out` 指到仓库里会直接**拒绝**并退出（非 0），防止误提交。
* 默认不联网（本文件没有任何网络 import）；不读页面内容。

## 用法

```bash
LOGS="$HOME/Library/Application Support/com.xraytun.desktop/logs"

# 扫整个日志目录（自动只取 *.jsonl 系列），把候选报告写到仓库外
python3 scripts/ad-candidates.py --corpus "$LOGS" --out /tmp/ad-candidates.md

# 只看出现 >=5 次的；并把宽匹配（误伤层）也列出来
python3 scripts/ad-candidates.py --corpus "$LOGS" --min-count 5 --include-weak

# 不用日志，直接分析一份域名清单（每行一个域名，可选 "次数 域名"）
python3 scripts/ad-candidates.py --domains /tmp/some-domains.txt --out /tmp/ad-candidates.md

# 机器可读
python3 scripts/ad-candidates.py --corpus "$LOGS" --json
```

`--count-mode`：`sniffed`（默认，`sniffed domain:` 行数）/ `paired`（按产品
`ConnectionLog` 的时序配对：上一条 sniffed 被 200ms 内下一条 accepted 消费）/
`mentions`（该域名在日志任意位置出现的行数）。三个数在表里都打印，避免"口径不同
所以数字对不上"。
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# 词典（**这是召回的上限**：不在表里的投放端一律漏掉）
# ---------------------------------------------------------------------------

# 整段完全相等即命中。
STRONG_TOKENS = {
    "ad", "ads", "adjs", "adx", "adserver", "adservers", "adservice", "adservices",
    "adsystem", "adsystems", "adtech", "adtechs", "adnet", "adnetwork", "adnetworks",
    "advert", "adverts", "advertise", "advertisement", "advertiser", "advertisers",
    "advertising", "adops", "admob",
    "analytics", "track", "tracks", "tracker", "trackers", "tracking", "trackjs",
    "click", "clicks", "clicktrack", "clicktracker",
    "beacon", "beacons", "pixel", "pixels",
    "sponsor", "sponsored", "sponsorship", "promoted", "promotion", "promotions",
    "doubleclick", "badjs", "ogads", "adnxs", "adsrvr", "adform", "criteo",
    "taboola", "outbrain", "mgid", "bidswitch", "pubmatic", "rubiconproject",
    "openx", "casalemedia", "smartadserver", "googleadservices",
    "googlesyndication", "googleads", "adroll", "quantserve", "scorecardresearch",
    "moatads", "demdex", "krxd", "bluekai", "mathtag", "agkn", "rlcdn", "tapad",
    "zedo", "serving-sys",
}

# 整个标签以它开头即命中（后接空/数字/`-`/`_`）。**不放 `ad`**：
# 那会把 `adobe` / `admin` / `address` 全捞进来（精度崩掉）。
PREFIX_TOKENS = {
    "ads", "adserver", "adservice", "adsystem", "adtech", "advert", "adnetwork",
    "adnxs", "adform", "adroll", "adjs", "badjs", "ogads", "doubleclick",
    "analytics", "tracker", "tracking", "clicktrack", "beacon", "pixel",
    "sponsor", "promoted", "criteo", "taboola", "outbrain", "googleadservices",
    "googlesyndication", "googleads", "quantserve", "scorecardresearch",
}

# 宽匹配（误伤层）用的子串；只在 --include-weak 时使用。
WEAK_SUBSTRINGS = {"ad", "ads", "track", "analytic", "pixel", "beacon", "click", "sponsor"}

# ---------------------------------------------------------------------------
# 解析
# ---------------------------------------------------------------------------

_TS = re.compile(r"(\d{4})/(\d{2})/(\d{2}) (\d{2}):(\d{2}):(\d{2})\.(\d{6})")
_SNIFF = re.compile(r"sniffed domain:\s*([^\s\"]+)")
_ACCEPTED = re.compile(r"accepted (?:tcp|udp):(\[[^\]]+\]|[^:\s]+):(\d+)")
_PAIR_WINDOW_US = 200_000


def host_of(token: str) -> str:
    return token.strip().strip("[]").rstrip(".").lower()


def ts_us(msg: str) -> int | None:
    m = _TS.search(msg)
    if not m:
        return None
    hh, mm, ss, us = (int(m.group(i)) for i in (4, 5, 6, 7))
    return ((hh * 60 + mm) * 60 + ss) * 1_000_000 + us


def message_of(line: str) -> str:
    """App 日志是 JSONL（取 `message`）；核心 stdout 是纯文本（原样用）。"""
    line = line.strip()
    if not line:
        return ""
    if line.startswith("{"):
        try:
            obj = json.loads(line)
            if isinstance(obj, dict) and isinstance(obj.get("message"), str):
                return obj["message"]
        except (ValueError, TypeError):
            pass
    return line


def iter_corpus_files(paths: list[Path]) -> list[Path]:
    out: list[Path] = []
    for p in paths:
        if p.is_dir():
            # 日志目录：只吃 app*.jsonl 系列（含 .1/.2 轮转），跳过 anomalies。
            out.extend(sorted(q for q in p.iterdir() if ".jsonl" in q.name and not q.name.startswith("anomalies")))
        elif p.is_file():
            out.append(p)
        else:
            print(f"读不了 {p}（不存在）", file=sys.stderr)
            sys.exit(2)
    return out


def scan(files: list[Path]) -> tuple[collections.Counter, collections.Counter, collections.Counter, dict]:
    """返回 (sniffed, paired, mentions, stats)。"""
    sniffed: collections.Counter = collections.Counter()
    paired: collections.Counter = collections.Counter()
    mentions: collections.Counter = collections.Counter()
    stats = {"lines": 0, "files": len(files), "accepted": 0, "sniffed_lines": 0}

    domain_re = re.compile(r"[a-z0-9][a-z0-9._-]*\.[a-z]{2,}")

    for path in files:
        pending: tuple[int | None, str] | None = None
        with open(path, "r", errors="replace") as fh:
            for raw in fh:
                stats["lines"] += 1
                msg = message_of(raw)
                if not msg:
                    continue
                for host in _SNIFF.findall(msg):
                    host = host_of(host)
                    if host:
                        sniffed[host] += 1
                        stats["sniffed_lines"] += 1
                        pending = (ts_us(msg), host)
                m = _ACCEPTED.search(msg)
                if m:
                    stats["accepted"] += 1
                    if pending is not None:
                        t = ts_us(msg)
                        if t is not None and pending[0] is not None and 0 <= (t - pending[0]) <= _PAIR_WINDOW_US:
                            paired[pending[1]] += 1
                        pending = None  # 配对即消费（与 xt-core ConnectionLog 同语义）
                # mentions：任意位置出现（粗口径，只用于对照）
                for host in set(domain_re.findall(msg.lower())):
                    if host in sniffed or "." in host:
                        mentions[host] += 1
    return sniffed, paired, mentions, stats


# ---------------------------------------------------------------------------
# 标签边界匹配
# ---------------------------------------------------------------------------

def boundary_match(host: str) -> list[str]:
    """返回**强匹配**的命中理由（空 = 不命中）。"""
    reasons: list[str] = []
    for label in host.split("."):
        for seg in re.split(r"[-_]", label):
            if seg and seg in STRONG_TOKENS:
                reasons.append(f"标签 {label!r} 里的段 {seg!r} 是已知广告/追踪标签")
        if label in PREFIX_TOKENS:
            reasons.append(f"标签 {label!r} 就是已知广告前缀")
            continue
        for pref in sorted(PREFIX_TOKENS, key=len, reverse=True):
            if label.startswith(pref) and len(label) > len(pref):
                rest = label[len(pref):]
                if rest[0].isdigit() or rest[0] in "-_":
                    reasons.append(f"标签 {label!r} 以广告前缀 {pref!r} 开头（后接 {rest!r}）")
                    break
    return reasons


def weak_match(host: str) -> list[str]:
    """宽匹配（**误伤层**）：子串命中，只在 --include-weak 时列出来。"""
    reasons: list[str] = []
    for label in host.split("."):
        for token in sorted(WEAK_SUBSTRINGS, key=len, reverse=True):
            if token in label:
                reasons.append(f"标签 {label!r} 含子串 {token!r}（**不是**标签边界，可能是误伤）")
                break
    return reasons


# ---------------------------------------------------------------------------
# 输出
# ---------------------------------------------------------------------------

MISS_NOTE = """\
## 这个工具会漏什么（和命中一起看）

1. **名字里没有广告字的投放端全漏**。自建广告服务、第一方广告 API
   （`youtubei.<...>` 这种）、CNAME 伪装过的追踪域（`metrics.<大站>`）都不会命中；
2. **内容里的广告位**（接口 JSON 的 `promoted` / `is_ad` 字段、脚本里的广告位）
   这个工具看不见 —— 那是第二层（MITM 抓一次真实响应验证），见
   `docs/verification/DOMAIN-FIRST-AD-FILTERING.md`；
3. **日志里没出现过的域名**漏（没有观测就没有候选）；
4. **会误报**：公司自建的 `ads.<自己域名>`、内部投放后台，名字像广告但不是
   用户想拦的东西 —— 所以候选必须人工确认。
"""

NEXT_NOTE = """\
## 下一步（**本工具不自动下发任何规则**）

1. 人工过一遍上面的候选，勾掉误报；
2. 把要拦的域名抄进 App 的「自定义规则」（`block`）或静态名单 —— **由用户决定**；
3. 拿不准的候选走第二层：用 MITM 抓一次真实响应看有没有机器可读标记
   （领域/字段），**只作为验证证据**，不要"看到 `promoted` 就全局拦"。
"""


def render(rows, stats, weak_rows, count_mode: str, include_weak: bool, observed: int = 0) -> str:
    out = []
    out.append("# 投放端候选（域名层，只读离线；**不自动下发**）\n")
    out.append(
        f"扫了 {stats['files']} 个文件 / {stats['lines']} 行；"
        f"sniffed 行 {stats['sniffed_lines']}，accepted 连接行 {stats['accepted']}。\n"
    )
    out.append(
        f"观测域名 {observed} 个 → **命中候选 {len(rows)} 条**（按 `{count_mode}` 计数排序；"
        f"占观测域名 {100.0 * len(rows) / observed:.1f}%）。\n" if observed else
        f"命中候选 {len(rows)} 条（按 `{count_mode}` 计数排序）。\n"
    )
    out.append(
        "| # | " + count_mode + " | sniffed | paired | mentions | 域名 | 命中理由 |\n"
        "|---:|---:|---:|---:|---:|---|---|\n"
    )
    for i, r in enumerate(rows, 1):
        out.append(
            f"| {i} | {r[count_mode]} | {r['sniffed']} | {r['paired']} | {r['mentions']} "
            f"| `{r['host']}` | {'；'.join(r['reasons'])} |\n"
        )
    out.append("\n")
    if include_weak:
        out.append(f"## 宽匹配层（默认不进候选；{len(weak_rows)} 条 —— 精度极低，只用来对照）\n")
        out.append("| 域名 | mentions | 宽理由 |\n|---|---:|---|\n")
        for r in weak_rows[:50]:
            out.append(f"| `{r['host']}` | {r['mentions']} | {'；'.join(r['reasons'])} |\n")
        if len(weak_rows) > 50:
            out.append(f"| … | | 还有 {len(weak_rows) - 50} 条（见 --json） |\n")
        out.append("\n")
    out.append(MISS_NOTE)
    out.append(NEXT_NOTE)
    return "".join(out)


def repo_root() -> Path | None:
    here = Path(__file__).resolve().parent
    for p in [here, *here.parents]:
        if (p / ".git").exists():
            return p
    return None


def ensure_outside_repo(path: Path) -> None:
    root = repo_root()
    if root is None:
        return
    rp = path.resolve()
    if rp == root or root in rp.parents:
        print(
            f"拒绝：--out 只能写到仓库外（{rp} 在 {root} 里面）。\n"
            f"      输出里含**浏览记录**，请写到 /tmp 之类的地方。",
            file=sys.stderr,
        )
        sys.exit(3)


def main() -> int:
    ap = argparse.ArgumentParser(description="从日志里捞投放端候选（只读、离线、不自动下发）")
    ap.add_argument("--corpus", action="append", default=[], type=Path,
                    help="日志文件或目录（可重复；目录会吃 app*.jsonl 系列）")
    ap.add_argument("--domains", type=Path, default=None,
                    help="直接给域名清单（每行一个；可写 `次数 域名`）")
    ap.add_argument("--out", type=Path, default=None, help="报告写到哪（**必须在仓库外**）")
    ap.add_argument("--min-count", type=int, default=2, help="按所选口径的最小出现次数（默认 2）")
    ap.add_argument("--count-mode", choices=["sniffed", "paired", "mentions"], default="sniffed",
                    help="排序/过滤用哪个口径（表里三列都会打印）")
    ap.add_argument("--include-weak", action="store_true", help="额外列出宽匹配（误伤层）")
    ap.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    args = ap.parse_args()

    if not args.corpus and args.domains is None:
        ap.error("至少给 --corpus 或 --domains")

    sniffed: collections.Counter = collections.Counter()
    paired: collections.Counter = collections.Counter()
    mentions: collections.Counter = collections.Counter()
    stats = {"files": 0, "lines": 0, "sniffed_lines": 0, "accepted": 0}

    if args.domains is not None:
        if not args.domains.is_file():
            print(f"读不了 {args.domains}", file=sys.stderr)
            return 2
        for line in args.domains.read_text(errors="replace").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) == 1:
                host, n = host_of(parts[0]), 1
            elif parts[0].isdigit():
                n, host = int(parts[0]), host_of(parts[1])      # `次数 域名`
            elif parts[1].isdigit():
                host, n = host_of(parts[0]), int(parts[1])      # `域名 次数`（也接受）
            else:
                host, n = host_of(parts[0]), 1
            sniffed[host] += n
            stats["lines"] += n
    else:
        files = iter_corpus_files(args.corpus)
        sniffed, paired, mentions, stats = scan(files)

    rows = []
    weak_rows = []
    observed = 0
    for host, n in sniffed.items():
        observed += 1
        reasons = boundary_match(host)
        row = {
            "host": host,
            "sniffed": sniffed.get(host, 0),
            "paired": paired.get(host, 0),
            "mentions": mentions.get(host, 0),
            "reasons": reasons,
        }
        if reasons and row[args.count_mode] >= args.min_count:
            rows.append(row)
        elif args.include_weak and not reasons:
            weak = weak_match(host)
            if weak:
                weak_rows.append({**row, "reasons": weak})

    rows.sort(key=lambda r: (-r[args.count_mode], r["host"]))
    weak_rows.sort(key=lambda r: (-r["mentions"], r["host"]))

    if args.json:
        print(json.dumps({"stats": stats, "observed": observed, "candidates": rows,
                          "weak": weak_rows if args.include_weak else []},
                         ensure_ascii=False, indent=2))
    else:
        text = render(rows, stats, weak_rows, args.count_mode, args.include_weak, observed)
        print(text)

    if args.out is not None:
        ensure_outside_repo(args.out)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        if args.json:
            args.out.write_text(json.dumps({"stats": stats, "observed": observed,
                                            "candidates": rows}, ensure_ascii=False, indent=2))
        else:
            args.out.write_text(render(rows, stats, weak_rows, args.count_mode, args.include_weak, observed))
        print(f"\n报告已写到 {args.out}（含域名，**请勿提交进仓库**）", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
