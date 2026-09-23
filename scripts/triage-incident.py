#!/usr/bin/env python3
"""triage-incident.py —— 把现场包（incident bundle）**自动分诊**成 signature + 证据。

## 它回答什么

「这个包说明什么？」—— 输出 `incident.json`（机器读）+ 一段 Markdown（人读），**两者都带口径头**。
上游是 `scripts/incident-bundle.sh` 产出的 zip（或目录），本脚本**只读**，不联网。

## 纪律（本项目的三条红线，逐条落到代码里）

1. **不许编造**：命中不了就输出 `unknown`，并列出「**最像的三条**」连同**原始行** —— 不猜根因。
   每条 signature 的判据都是一个**可测谓词**（阈值写成常量、写出来源），不是一个印象。
2. **不许静默**：任何「读不到/缺件」都进 `notes`，出现在 JSON 与 Markdown 里；缺 `metrics.json`
   时对应 signature 记为 `unavailable`（**不是** `not hit`）——「没数据」与「数据说没有」必须分开。
3. **口径头先行**：输出开头就有 `caliber`：包身份（manifest 的 sha256）、窗口、文件清单与各自的 sha256、
   工具版本、**所有阈值**。引用任何一个命中都必须连它一起引。

## 边界（诚实清单，也写进 Markdown）

* 命中的是**症状**，不是根因。「v6-rewrite 命中」= 窗口里**有** v6 改写且**有**失败相关性；
  它**不**证明「v6 改写导致了用户的问题」（那是 task-97 的相关性口径）。
* 只看包里有的东西：包里没有的（真机 WKWebView、GFW 侧行为、App 内部的读侧统计）本脚本一概看不到。
* `log-read-loss` 只覆盖**脚本侧**的解析损失；App 自己的 `tail_logs` 读侧损失要等它把统计写进日志才可见。

用法：
  python3 scripts/triage-incident.py --bundle /tmp/xraytun-incident-XXXX.zip
  python3 scripts/triage-incident.py --bundle <目录> --json-out incident.json --md-out SUMMARY.md
  python3 scripts/triage-incident.py --privacy-check /tmp/xraytun-incident-XXXX.zip
                                                          # **上传前的隐私闸**：命中即非 0（fail closed）
  python3 scripts/triage-incident.py --privacy-check <包> --privacy-json
  python3 scripts/triage-incident.py --self-test          # 每个 signature 一条 fixture + 双向敏感性
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile

TOOL_VERSION = "triage-incident/1（2026-09-22）"

# ---------------------------------------------------------------- 阈值（全部写出来源）

# 写侧「一行两个 JSON 对象」的修复版本（task-104 / dd95fdb 进 v0.8.34）。
# 在这个版本**之后**还出现多对象行 ⇒ 回归信号；之前 ⇒ 历史数据，属预期。
WRITE_INTERLEAVE_FIXED_VERSION = (0, 8, 34)

# `tun` 接口 EINVAL（核心自己的拼写错误 `falied`）的每分钟阈值。
# 来源：本机 2026-09-22 实测 44296 行核心日志里 **0 次**（安静的窗口），
# task-93 的现场记录里是持续刷屏 ⇒ 用「≥1 次/分钟」当「在刷」的下限，
# 不追求精确 —— 这个 signature 只用来指出「该去看看」，不作为根因判定。
TUN_EINVAL_PER_MIN_THRESHOLD = 1.0

# 判「误判」时看事件前后多少秒内还有转发证据（task-95 的口径）。
WATCHDOG_WINDOW_SECS = 60

# 签名清单（顺序 = 输出顺序；`unknown` 是兜底，不在这里）
SIGNATURES = [
    "v6-rewrite",
    "watchdog-false-positive",
    "probe-false-negative",
    "log-read-loss",
    "log-write-interleave",
    "helper-mismatch",
    "loopback-hole",
    "tun-iface-einval",
]


# ---------------------------------------------------------------- 读包


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_bundle(path):
    """读一个 bundle（zip 或目录）→ dict(manifest, metrics, events, core_lines, network, files, root)。

    core_lines 每项是 (ts_unix|None, 原始行)；`ts_unix` 从该行的 JSON 里取（解析失败则 None）。
    """
    tmp = None
    root = path
    if os.path.isfile(path):
        tmp = tempfile.mkdtemp(prefix="triage-bundle-")
        shutil.unpack_archive(path, tmp)
        root = tmp
    if not os.path.isdir(root):
        raise SystemExit(f"✗ 不是目录也不是可解压的 zip：{path}")

    out = {"root": root, "unpacked_to": tmp, "files": {}, "manifest": None, "metrics": None,
           "metrics_error": None, "events": [], "core_lines": [], "network": None, "notes": []}

    def rd(name):
        p = os.path.join(root, name)
        return open(p, encoding="utf-8", errors="replace").read() if os.path.exists(p) else None

    for name in ("manifest.json", "metrics.json", "events.jsonl", "core-tail.txt", "network.txt", "README.txt"):
        p = os.path.join(root, name)
        if os.path.exists(p):
            out["files"][name] = {"bytes": os.path.getsize(p), "sha256": sha256_file(p)}
        else:
            out["notes"].append(f"包里没有 {name}")

    m = rd("manifest.json")
    if m:
        try:
            out["manifest"] = json.loads(m)
        except Exception as e:  # noqa: BLE001
            out["notes"].append(f"manifest.json 解析失败：{e}")

    mj = rd("metrics.json")
    if mj:
        try:
            out["metrics"] = json.loads(mj)
        except Exception as e:  # noqa: BLE001
            out["metrics_error"] = f"metrics.json 解析失败：{e}"

    ev = rd("events.jsonl")
    if ev:
        for ln in ev.split("\n"):
            ln = ln.strip()
            if not ln:
                continue
            try:
                out["events"].append(json.loads(ln))
            except Exception:  # noqa: BLE001
                out["notes"].append("events.jsonl 里有一行不是合法 JSON（已跳过）")

    ct = rd("core-tail.txt")
    if ct:
        dec = json.JSONDecoder()
        for ln in ct.split("\n"):
            if not ln.strip() or ln.startswith("#"):
                continue
            ts = None
            i, first = 0, None
            while i < len(ln):
                try:
                    obj, end = dec.raw_decode(ln, i)
                except ValueError:
                    break
                i = end
                if first is None and isinstance(obj, dict):
                    first = obj
            if isinstance(first, dict):
                ts = first.get("ts_unix")
            out["core_lines"].append((ts, ln))

    out["network"] = rd("network.txt")
    return out


# ---------------------------------------------------------------- 每条 signature 的判据
# 每个判据返回 dict(hit, evidence, near_miss, samples)：
#   hit      : 是否命中
#   evidence : 命中时的关键数字（人读与机器读都用它）
#   near_miss: 0..1，未命中时用来排「最像的三条」（**只是一个排序启发式，不是结论**）
#   samples  : 原始行（截断到 3 条）—— 给「不许编造、要能自己看」用的


def sig_v6_rewrite(b):
    m = b["metrics"]
    if not m:
        return {"hit": False, "unavailable": "缺 metrics.json", "near_miss": 0.0, "samples": []}
    t = m.get("task97") or {}
    lines = t.get("v6_rewrite_lines", 0)
    cls = t.get("classes") or {}
    v6f = (cls.get("v6_only") or {}).get("failed", 0)
    mixf = (cls.get("mixed") or {}).get("failed", 0)
    fails = v6f + mixf
    hit = lines > 0 and fails > 0
    return {
        "hit": hit,
        "evidence": {"replace_v6_lines": lines, "v6_only_failed": v6f, "mixed_failed": mixf,
                     "predicate": "v6 改写行数 > 0 且（v6-only 失败 + 混合失败）> 0"},
        "near_miss": 0.5 if lines > 0 else 0.0,
        "samples": [ln for _, ln in b["core_lines"] if "replace destination with tcp:[" in ln][:3],
    }


def _traffic_lines_near(b, ts):
    lo, hi = ts - WATCHDOG_WINDOW_SECS, ts + WATCHDOG_WINDOW_SECS
    return [ln for t, ln in b["core_lines"]
            if isinstance(t, (int, float)) and lo <= t <= hi
            and ("tunneling request" in ln or "connection opened" in ln)]


def sig_watchdog_false_positive(b):
    voids = [e for e in b["events"] if "已作废" in (e.get("message") or "")]
    if not voids:
        return {"hit": False, "near_miss": 0.0, "samples": [],
                "evidence": {"已作废": 0, "predicate": "有「已作废」且其 ±60s 内仍有转发证据"}}
    hits = []
    for e in voids:
        t = e.get("ts_unix")
        near = _traffic_lines_near(b, t) if isinstance(t, (int, float)) else []
        if near:
            hits.append({"ts_unix": t, "traffic_lines_within_60s": len(near), "sample": near[0][:160]})
    return {
        "hit": bool(hits),
        "evidence": {"已作废": len(voids), "±60s 内仍有转发": len(hits), "细节": hits[:5],
                     "predicate": "存在「已作废」事件，且其前后 60 秒内仍能读到 `tunneling request` / `connection opened`"},
        "near_miss": 0.3 if voids and not hits else 1.0 if hits else 0.0,
        "samples": [h["sample"] for h in hits[:3]],
    }


def sig_probe_false_negative(b):
    m = b["metrics"]
    if not m:
        return {"hit": False, "unavailable": "缺 metrics.json", "near_miss": 0.0, "samples": []}
    p = m.get("probes") or {}
    fl, nol = p.get("failed_line", 0), p.get("no_outcome_line", 0)
    hit = (fl + nol) > 0
    return {
        "hit": hit,
        "evidence": {"探针连接": p.get("total_connections"), "成功": p.get("success"),
                     "失败·有 failed 行": fl, "失败·无结局行": nol, "轮数": p.get("rounds"),
                     "predicate": "探针「有 failed 行」或「无结局行」> 0"},
        "near_miss": 0.9 if hit else 0.1,
        "samples": [],
    }


def sig_log_read_loss(b):
    m = b["metrics"]
    if not m:
        return {"hit": False, "unavailable": "缺 metrics.json", "near_miss": 0.0, "samples": []}
    s = m.get("stats") or {}
    trunc = s.get("truncated_lines", 0)
    nonjson = s.get("non_json_lines", 0)
    hit = (trunc + nonjson) > 0
    return {
        "hit": hit,
        "evidence": {"截断·残缺行": trunc, "非 JSON 行": nonjson, "空行": s.get("blank_lines"),
                     "predicate": "读统计里「截断·残缺」或「非 JSON」> 0（脚本侧损失；App 内部读侧统计不在包里）"},
        "near_miss": 0.2 if not hit else 1.0,
        "samples": [],
    }


def _app_version_tuple(b):
    v = (((b.get("manifest") or {}).get("versions") or {}).get("app") or {}).get("value")
    if not v:
        return None
    try:
        return tuple(int(x) for x in re.findall(r"\d+", v)[:3])
    except Exception:  # noqa: BLE001
        return None


def sig_log_write_interleave(b):
    multi = [(t, ln) for t, ln in b["core_lines"] if ln.count("}{") >= 1]
    ver = _app_version_tuple(b)
    post_fix = ver is not None and ver >= WRITE_INTERLEAVE_FIXED_VERSION
    hit = len(multi) > 0 and post_fix
    return {
        "hit": hit,
        "evidence": {"多对象行": len(multi), "App 版本": ".".join(map(str, ver)) if ver else None,
                     "修复版本": ".".join(map(str, WRITE_INTERLEAVE_FIXED_VERSION)),
                     "predicate": f"多对象行 > 0 **且** App 版本 ≥ {'.'.join(map(str, WRITE_INTERLEAVE_FIXED_VERSION))}"
                                  "（修复前出现属历史数据，不算回归）"},
        "near_miss": 0.8 if len(multi) > 0 else 0.0,
        "samples": [ln[:160] for _, ln in multi[:3]],
    }


def sig_helper_mismatch(b):
    chk = (((b.get("manifest") or {}).get("versions") or {}).get("helper") or {}).get("check") or {}
    state = chk.get("state")
    hit = state == "Mismatch"
    return {
        "hit": hit,
        "evidence": {"版本检查三态": state, "version": chk.get("version"),
                     "installed": chk.get("installed"), "bundled": chk.get("bundled"),
                     "predicate": "manifest 的 helper 版本三态 == Mismatch（读不到时是 Unreadable，**不算命中**）"},
        "near_miss": 0.7 if state == "Unreadable" else (1.0 if hit else 0.0),
        "samples": [],
    }


def sig_loopback_hole(b):
    net = b["network"]
    if not net:
        return {"hit": False, "unavailable": "缺 network.txt", "near_miss": 0.0, "samples": []}
    sec = None
    for part in net.split("## "):
        if part.startswith("route -n get 127.0.0.2"):
            sec = part
            break
    if sec is None:
        return {"hit": False, "unavailable": "network.txt 里没有 127.0.0.2 段", "near_miss": 0.0, "samples": []}
    iface = None
    for ln in sec.split("\n"):
        m = re.match(r"\s*interface:\s*(\S+)", ln)
        if m:
            iface = m.group(1)
    not_in_table = "not in table" in sec
    hit = not_in_table or (iface is not None and iface != "lo0")
    shape = "路由不存在（not in table）" if not_in_table else (f"interface={iface}" if iface else "无法判读")
    return {
        "hit": hit,
        "evidence": {"interface": iface, "shape": shape,
                     "predicate": "`route -n get 127.0.0.2` 的 interface ≠ lo0（或路由不存在）"},
        "near_miss": 0.4 if iface is None else (1.0 if hit else 0.0),
        "samples": [ln for ln in sec.split("\n") if ln.strip()][:5],
    }


def sig_tun_iface_einval(b):
    rows = [(t, ln) for t, ln in b["core_lines"] if "falied to set interface" in ln]
    if not rows:
        return {"hit": False, "near_miss": 0.1, "samples": [],
                "evidence": {"次数": 0, "每分钟阈值": TUN_EINVAL_PER_MIN_THRESHOLD,
                             "predicate": "`falied to set interface`（核心自己的拼写）每分钟计数 ≥ 阈值"}}
    ts = [t for t, _ in rows if isinstance(t, (int, float))]
    span_min = ((max(ts) - min(ts)) / 60.0) if len(ts) >= 2 else 1.0
    span_min = max(span_min, 1.0 / 60.0)
    rate = len(rows) / span_min
    return {
        "hit": rate >= TUN_EINVAL_PER_MIN_THRESHOLD,
        "evidence": {"次数": len(rows), "跨度分钟": round(span_min, 2), "每分钟": round(rate, 3),
                     "每分钟阈值": TUN_EINVAL_PER_MIN_THRESHOLD,
                     "predicate": "`falied to set interface` 每分钟计数 ≥ 阈值（阈值来源见脚本常量注释）"},
        "near_miss": 0.5 if rows else 0.1,
        "samples": [ln[:160] for _, ln in rows[:3]],
    }


PREDICATES = {
    "v6-rewrite": sig_v6_rewrite,
    "watchdog-false-positive": sig_watchdog_false_positive,
    "probe-false-negative": sig_probe_false_negative,
    "log-read-loss": sig_log_read_loss,
    "log-write-interleave": sig_log_write_interleave,
    "helper-mismatch": sig_helper_mismatch,
    "loopback-hole": sig_loopback_hole,
    "tun-iface-einval": sig_tun_iface_einval,
}


# ---------------------------------------------------------------- 隐私闸（上传前最后一道自检）
#
# 为什么要有它：脱敏在**客户端**做（`scripts/incident-bundle.sh`），而上传端点会把包**原样**存起来
# ⇒ **漏一次，密钥就在云上躺很久**。task-113 的真实教训：脱敏自己做漏了三处
# （JSON 形键值 `"password":"x"`、URI 的 `?query`（`pbk=SECRET`）、`Authorization` 只吃掉 `Bearer`）。
#
# 红线：**报告本身不许成为泄漏源** ⇒ 命中项只报 `文件:行号:类型` + **前 4 字符与长度**，绝不回显原值。
#
# 与 task-114（服务端拒收）**互不替代**：这一层是**上传前自检**，那一层是**最后防线**。
PRIVACY_PATTERNS = [
    ("uuid", re.compile(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")),
    ("uuid-32hex", re.compile(r"\b[0-9a-fA-F]{32}\b")),
    # 值里排除 `<`/`>`，这样我们自己的 `<uuid>` / `<redacted>` 占位不会被误报
    ("uri-secret-param", re.compile(
        r"(?i)[?&#;](?:pbk|sid|spx|token|password|passwd|pwd|uuid|key|api[_-]?key|secret|auth|psk"
        r"|private[_-]?key)=([^&\s\"'<>]{4,})")),
    # 同时覆盖**转义形**（`\"password\":\"x\"`：核心日志里 message 本身是 JSON 字符串时就是这种）
    ("credential-field-json", re.compile(
        r'''(?i)\\?"(?:password|passwd|pwd|token|secret|uuid|api[_-]?key|private[_-]?key|auth|psk|key)\\?"'''
        r'''\s*:\s*\\?"([^"\\]{4,})''')),
    ("subscription-url", re.compile(
        r"https?://[^\s\"']*(?:/subscribe|/sub\?|/api/v\d+/client/subscribe|/link/[A-Za-z0-9]+)[^\s\"']*")),
    ("proxy-url-with-credentials", re.compile(
        r"(?i)\b(?:vmess|vless|ss|ssr|trojan|hysteria2?|tuic)://[^\s\"']*@[^\s\"']+")),
    ("vmess-base64", re.compile(r"(?i)\bvmess://[A-Za-z0-9+/=]{16,}")),
    ("private-key-block", re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")),
    ("bearer-token", re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._\-+/=]{16,}")),
    ("authorization-header", re.compile(r"(?i)\b(?:authorization|proxy-authorization)\s*:\s*(?!<redacted>)\S{4,}")),
    ("email", re.compile(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b")),
]

# 文档/示例里**故意**写的邮箱域：不白名单的话，每份文档都会报一片（假阳性会让闸门被绕过）
EMAIL_WHITELIST = ("example.com", "example.org", "example.net", "localhost",
                   "users.noreply.github.com", "xraytun.top")
# 我们自己的脱敏占位：它们出现在产物里是**好事**，不是命中
PLACEHOLDERS = {"<uuid>", "<redacted>", "<host>", "<home>", "<HOME>", "<your-uuid>", "xxx", "***", "（无示例）"}


def _mask(v):
    """只给「前 4 字符 + 长度」：报告本身不许成为泄漏源。"""
    v = v.strip().strip("\"'")
    return f"{v[:4]}…（len={len(v)}）"


def _is_placeholder(v):
    s = v.strip().strip("\"'")
    return s in PLACEHOLDERS or (s.startswith("<") and s.endswith(">"))


def _should_skip(typ, val, whole):
    if _is_placeholder(val):
        return True
    if typ == "proxy-url-with-credentials":
        userinfo = whole.split("://", 1)[-1].split("@", 1)[0]
        if _is_placeholder(userinfo):      # `vless://<uuid>@host:443` 是**已脱敏**的，不算命中
            return True
    if typ == "email":
        dom = val.rsplit("@", 1)[-1].lower()
        # **精确匹配**白名单域：`corp-mail.example.net` 这种子域**照报**（fail closed）。
        # 一开始写成「后缀匹配」，结果把真值 fixture 里的邮箱也一起吞了 —— 自测的对照断言当场抓到。
        if any(dom == d for d in EMAIL_WHITELIST):
            return True
    return False


def _looks_like_uri_userinfo(line, start):
    """`xxx://<userinfo>@host` 里的 userinfo 会被**邮箱**正则误判（实测两处：
    `vless://<uuid>@host`、`ss://<base64>@host`）⇒ 紧邻的前几个字符里有 `//` 就跳过。
    这些 case 已经由 `proxy-url-with-credentials` 报过，重复报只会让闸门变噪音。"""
    prefix = line[max(0, start - 3):start]
    return "//" in prefix or prefix.endswith("@")


def privacy_scan_text(name, text):
    """扫一段文本 → findings（每项 `{file,line,type,masked}`）。**不回显原值。**"""
    findings = []
    for lineno, line in enumerate(text.split("\n"), 1):
        for typ, pat in PRIVACY_PATTERNS:
            for m in pat.finditer(line):
                val = m.group(1) if m.groups() else m.group(0)
                if typ == "email" and _looks_like_uri_userinfo(line, m.start()):
                    continue
                if _should_skip(typ, val, m.group(0)):
                    continue
                findings.append({"file": name, "line": lineno, "type": typ, "masked": _mask(val)})
    return findings


def privacy_scan_bundle(path, patterns=None):
    """扫一个包（zip 或目录）里的**所有文本文件**。只读：不改包内任何东西。

    `patterns` 只给自测用（逐条删掉一个谓词做敏感性）；生产路径不传。
    """
    global PRIVACY_PATTERNS
    saved = PRIVACY_PATTERNS
    if patterns is not None:
        PRIVACY_PATTERNS = patterns
    try:
        b = load_bundle(path)
        root = b["root"]
        found, scanned = [], []
        for dirpath, _dirs, names in os.walk(root):
            for n in sorted(names):
                p = os.path.join(dirpath, n)
                rel = os.path.relpath(p, root)
                try:
                    text = open(p, encoding="utf-8", errors="replace").read()
                except Exception:  # noqa: BLE001
                    continue
                scanned.append(rel)
                found.extend(privacy_scan_text(rel, text))
        return found, scanned
    finally:
        PRIVACY_PATTERNS = saved


def privacy_check(path, as_json=False):
    """CLI 入口。命中 ⇒ 返回 1（**fail closed**）；干净 ⇒ 0；路径不存在 ⇒ 2。"""
    if not os.path.exists(path):
        print(f"✗ 找不到：{path}", file=sys.stderr)
        return 2
    findings, scanned = privacy_scan_bundle(path)
    if as_json:
        print(json.dumps({"ok": not findings, "path": os.path.basename(os.path.abspath(path)),
                          "scanned_files": scanned, "findings": findings,
                          "note": "值只显示前 4 字符与长度；本输出不含原值"},
                         ensure_ascii=False, indent=2))
        return 1 if findings else 0
    print(f"隐私闸：扫描 {os.path.basename(os.path.abspath(path))}（{len(scanned)} 个文件）")
    if findings:
        print(f"✗ 命中 {len(findings)} 处疑似密钥/隐私模式 —— **fail closed（退出码 1）**，请先修脱敏再上传：")
        for f in findings:
            print(f"    {f['file']}:{f['line']}:{f['type']}    {f['masked']}")
        print("  说明：只显示前 4 字符与长度；本报告不含原值，也不会写下原值。")
        print(f"  复现：python3 scripts/triage-incident.py --privacy-check {path}")
        return 1
    print("✓ 未发现疑似密钥模式")
    return 0


def privacy_self_test():
    """真值 fixture（必须全中）/ 干净 fixture（必须零中）/ **逐条谓词的双向敏感性**。"""
    fails = []
    tmp = tempfile.mkdtemp(prefix="privacy-selftest-")

    def check(name, got, want):
        ok = got == want
        print(f"  {'✓' if ok else '✗'} {name}: got={got!r} want={want!r}")
        if not ok:
            fails.append(name)

    dirty = "\n".join([
        '{"ts_unix":1790070000,"source":"core","level":"info","message":"start"}',
        "vless://11111111-2222-3333-4444-555555555555@node.example.com:443?pbk=SECRETPBK&sid=abc123def456",
        '{"password":"hunter2","token":"abcDEF123456"}',
        '{"message":"{\\"password\\":\\"escapedSECRET1\\"}"}',
        "https://sub.example.org/api/v1/client/subscribe?token=TOPSECRETVALUE",
        "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.SECRETPART",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "contact: alice.smith@corp-mail.example.net",
        "vmess://eyJ2IjoiMiIsInBzIjoibm9kZSIsImFkZCI6Im5vZGUuZXhhbXBsZS5jb20ifQ==",
        "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQ@node.example.com:8388",
        "api_key=abcd1234efgh5678",
        "deadbeefdeadbeefdeadbeefdeadbeef",
        "route to: 127.0.0.2",
    ]) + "\n"
    clean = "\n".join([
        '{"ts_unix":1,"source":"app","level":"info","message":"vless://<uuid>@node.example.com:443?pbk=<redacted>"}',
        '{"ts_unix":2,"source":"app","level":"info","message":"订阅已更新"}',
        "password=<redacted>",
        "uuid=<uuid>",
        "contact: someone@example.com",      # 白名单域
        "route to: 127.0.0.2",
        "app.1.jsonl=29293980B/mtime13:16:42",
    ]) + "\n"

    d_dir = os.path.join(tmp, "dirty")
    os.makedirs(d_dir, exist_ok=True)
    with open(os.path.join(d_dir, "logs.txt"), "w", encoding="utf-8") as f:
        f.write(dirty)
    c_dir = os.path.join(tmp, "clean")
    os.makedirs(c_dir, exist_ok=True)
    with open(os.path.join(c_dir, "logs.txt"), "w", encoding="utf-8") as f:
        f.write(clean)

    print("=== 隐私闸：真值 fixture ⇒ 必须全部命中 ===")
    d_find, d_files = privacy_scan_bundle(d_dir)
    got_types = {f["type"] for f in d_find}
    print(f"  扫了 {d_files}；命中类型 = {sorted(got_types)}")
    for typ, _ in PRIVACY_PATTERNS:
        check(f"真值 fixture 命中 `{typ}`", typ in got_types, True)
    check("命中项里**不含原值**（SECRETPBK 不得出现在报告里）",
          any("SECRETPBK" in json.dumps(f, ensure_ascii=False) for f in d_find), False)
    check("命中项的值被掩成「前 4 字符 + 长度」",
          all("（len=" in f["masked"] for f in d_find), True)
    # 反例（实测抓到的假阳性）：`vless://<uuid>@host` 与 `ss://<base64>@host` 的 userinfo
    # 会被邮箱正则当成 `xxxx@host` ⇒ 必须被排除，否则闸门全是噪音、会被绕过。
    emails = [f for f in d_find if f["type"] == "email"]
    check("脏 fixture 的 email 命中**只有 1 处**（URI 里的 userinfo 不再被误报）", len(emails), 1)
    check("那 1 处是真实邮箱（掩码以 alic 开头）", emails[0]["masked"].startswith("alic"), True)

    print("\n=== 隐私闸：干净 fixture（已脱敏 + 白名单域）⇒ 必须零命中 ===")
    c_find, _ = privacy_scan_bundle(c_dir)
    check("干净 fixture 零命中", c_find, [])
    check("干净 fixture 退出码为 0（fail closed 的反面）", privacy_check(c_dir), 0)

    print("\n=== 隐私闸：双向敏感性 —— 逐条删掉一个谓词 ⇒ 该类型必须不再被报出 ===")
    for typ, _ in PRIVACY_PATTERNS:
        weakened = [p for p in PRIVACY_PATTERNS if p[0] != typ]
        miss_find, _ = privacy_scan_bundle(d_dir, patterns=weakened)
        miss_types = {f["type"] for f in miss_find}
        check(f"删掉 `{typ}` 的规则后不再报出该类型（⇒ 原断言会红）", typ in miss_types, False)
        others = [p[0] for p in weakened]
        check(f"（对照）删掉 `{typ}` 后其余类型仍然报出", all(o in miss_types for o in others), True)

    shutil.rmtree(tmp, ignore_errors=True)
    print()
    if fails:
        print(f"privacy self-test：**失败**（{len(fails)} 项）：{fails}")
        return 1
    print("privacy self-test：**全部通过**（真值 11 类全中 / 干净零中 / 逐条谓词双向敏感性）")
    return 0


# ---------------------------------------------------------------- 分诊 + 输出


def triage(bundle):
    results = {}
    for name in SIGNATURES:
        try:
            r = PREDICATES[name](bundle)
        except Exception as e:  # noqa: BLE001  —— 判据自己崩了要说出来，不能假装没命中
            r = {"hit": False, "unavailable": f"判据抛异常：{e}", "near_miss": 0.0, "samples": []}
        results[name] = r
    hits = [n for n in SIGNATURES if results[n].get("hit")]
    if hits:
        signature = hits[0] if len(hits) == 1 else "multiple"
    else:
        signature = "unknown"
    # 「最像的三条」：未命中里 near_miss 最高的三条（**只是排序启发式**）
    closest = sorted((n for n in SIGNATURES if not results[n].get("hit")),
                     key=lambda n: results[n].get("near_miss", 0.0), reverse=True)[:3]
    return {"signature": signature, "hits": hits, "results": results, "closest": closest}


def caliber_block(bundle, path):
    m = bundle.get("manifest") or {}
    files = bundle["files"]
    return {
        "tool": TOOL_VERSION,
        "bundle": os.path.basename(os.path.abspath(path)),
        "bundle_files": files,
        "manifest_sha256": files.get("manifest.json", {}).get("sha256"),
        "window": m.get("window"),
        "app_version": ((m.get("versions") or {}).get("app") or {}).get("value"),
        "core_version": ((m.get("versions") or {}).get("core") or {}).get("value"),
        "helper_check": ((m.get("versions") or {}).get("helper") or {}).get("check"),
        "thresholds": {
            "tun_einval_per_min": TUN_EINVAL_PER_MIN_THRESHOLD,
            "watchdog_window_secs": WATCHDOG_WINDOW_SECS,
            "write_interleave_fixed_version": ".".join(map(str, WRITE_INTERLEAVE_FIXED_VERSION)),
        },
        "notes": bundle.get("notes", []),
        "boundary": "命中是**症状**不是根因；只看包里有的东西",
    }


def to_markdown(bundle, tri, caliber):
    L = []
    L.append(f"# 现场分诊：{tri['signature']}")
    L.append("")
    L.append("## 口径头（引用任何命中都要带上这一段）")
    L.append("")
    L.append(f"* 工具：`{caliber['tool']}`")
    L.append(f"* 包：`{caliber['bundle']}`；`manifest.json` sha256 = `{caliber['manifest_sha256']}`")
    w = caliber["window"] or {}
    L.append(f"* 窗口：{w.get('since_local')} → {w.get('until_local')}"
             f"（来源：{w.get('source')}；degraded={w.get('degraded')}）")
    L.append(f"* 版本：App {caliber['app_version']} / 核心 {caliber['core_version']} / "
             f"helper 三态 {((caliber['helper_check'] or {}).get('state'))}")
    L.append(f"* 阈值：{json.dumps(caliber['thresholds'], ensure_ascii=False)}")
    L.append(f"* 包内文件（bytes/sha256）：")
    for name, meta in caliber["bundle_files"].items():
        L.append(f"  * `{name}` {meta['bytes']} B `{meta['sha256'][:16]}…`")
    if caliber["notes"]:
        L.append(f"* ⚠️ 读包时的说明：{'；'.join(caliber['notes'])}")
    L.append("")
    L.append("## 判定")
    L.append("")
    if tri["hits"]:
        L.append(f"**命中 {len(tri['hits'])} 条**：{', '.join('`' + h + '`' for h in tri['hits'])}")
    else:
        L.append("**未命中任何 signature ⇒ `unknown`**（不猜根因）。最像的三条：")
        for n in tri["closest"]:
            r = tri["results"][n]
            L.append(f"* `{n}`（near_miss={r.get('near_miss', 0):.2f}）：{json.dumps(r.get('evidence'), ensure_ascii=False)}")
    L.append("")
    L.append("## 逐条判据")
    L.append("")
    L.append("| signature | 结果 | 关键数字 | 判据 |")
    L.append("|---|---|---|---|")
    for n in SIGNATURES:
        r = tri["results"][n]
        state = "**命中**" if r.get("hit") else ("不可用" if r.get("unavailable") else "未命中")
        ev = r.get("evidence") or {}
        pred = ev.get("predicate", r.get("unavailable", ""))
        key = {k: v for k, v in ev.items() if k not in ("predicate", "细节")}
        L.append(f"| `{n}` | {state} | {json.dumps(key, ensure_ascii=False)[:220]} | {pred} |")
    L.append("")
    L.append("## 原始行（能自己看，不用信我）")
    L.append("")
    any_sample = False
    for n in SIGNATURES:
        for s in (tri["results"][n].get("samples") or []):
            L.append(f"* `{n}`：{s}")
            any_sample = True
    if not any_sample:
        L.append("* （本次判据没有附带原始行 —— 因为它们都是**计数型**判据，原始行见包内文件）")
    L.append("")
    L.append("## 边界（本流程不能证明什么）")
    L.append("")
    L.append("* 命中是**症状**不是根因（例：`v6-rewrite` 只说明「有 v6 改写且有失败」）。")
    L.append("* 包里没有的东西看不到：真机 WKWebView、GFW 侧行为、App 内部读侧统计。")
    L.append("* `unknown` 只表示「这些谓词都没命中」，**不表示没有问题**。")
    L.append("")
    return "\n".join(L)


# ---------------------------------------------------------------- --self-test（fixture + 双向敏感性）


def _write_fixture(root, manifest, metrics=None, events=None, core=None, network=None):
    os.makedirs(root, exist_ok=True)
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, ensure_ascii=False)
    if metrics is not None:
        with open(os.path.join(root, "metrics.json"), "w", encoding="utf-8") as f:
            json.dump(metrics, f, ensure_ascii=False)
    if events is not None:
        with open(os.path.join(root, "events.jsonl"), "w", encoding="utf-8") as f:
            for e in events:
                f.write(json.dumps(e, ensure_ascii=False) + "\n")
    if core is not None:
        with open(os.path.join(root, "core-tail.txt"), "w", encoding="utf-8") as f:
            for ln in core:
                f.write(ln + "\n")
    if network is not None:
        with open(os.path.join(root, "network.txt"), "w", encoding="utf-8") as f:
            f.write(network)
    return root


def _core_line(ts, msg):
    return json.dumps({"ts_unix": ts, "source": "core", "level": "info",
                       "message": f"2026/09/22 19:00:00.000000 [Info] {msg}"}, ensure_ascii=False)


def _base_manifest(app="0.8.34"):
    return {"bundle_format": "xraytun-incident/1",
            "window": {"since_local": "2026-09-22 18:00:00", "until_local": "2026-09-22 19:00:00",
                       "source": "self-test 人造", "degraded": False},
            "versions": {"app": {"value": app}, "core": {"value": "26.9.9"},
                         "helper": {"check": {"state": "Match", "version": app}}}}


def self_test():
    fails = []
    tmp = tempfile.mkdtemp(prefix="triage-selftest-")
    t0 = 1790070000

    def check(name, got, want):
        ok = got == want
        print(f"  {'✓' if ok else '✗'} {name}: got={got!r} want={want!r}")
        if not ok:
            fails.append(name)

    # --- 每条 signature 一条 fixture：期望「命中」
    fixtures = {}
    fixtures["v6-rewrite"] = _write_fixture(
        os.path.join(tmp, "v6"), _base_manifest(),
        metrics={"stats": {}, "task97": {"v6_rewrite_lines": 12,
                                         "classes": {"v6_only": {"connections": 5, "failed": 4},
                                                     "mixed": {"connections": 2, "failed": 1}}},
                 "probes": {}, "selfheal": {}},
        core=[_core_line(t0, "[123456789] replace destination with tcp:[240e:1::1]:80"),
              _core_line(t0 + 1, "[123456789] failed to open connection")])

    fixtures["watchdog-false-positive"] = _write_fixture(
        os.path.join(tmp, "wd"), _base_manifest(),
        events=[{"ts_unix": t0, "source": "app", "level": "info",
                 "message": "已作废「自动重连」意图（看门狗重建隧道失败，已退回直连）"}],
        core=[_core_line(t0 + 5, "[999999999] proxy/vless/outbound: tunneling request to tcp:x:80")])

    fixtures["probe-false-negative"] = _write_fixture(
        os.path.join(tmp, "probe"), _base_manifest(),
        metrics={"stats": {}, "task97": {}, "selfheal": {},
                 "probes": {"total_connections": 10, "success": 7, "failed_line": 2, "no_outcome_line": 1,
                            "rounds": 5}})

    fixtures["log-read-loss"] = _write_fixture(
        os.path.join(tmp, "readloss"), _base_manifest(),
        metrics={"stats": {"truncated_lines": 1, "non_json_lines": 0, "blank_lines": 0},
                 "task97": {}, "probes": {}, "selfheal": {}})

    fixtures["log-write-interleave"] = _write_fixture(
        os.path.join(tmp, "interleave"), _base_manifest(app="0.8.34"),
        core=[_core_line(t0, "x") + _core_line(t0, "y")])   # 一行两个对象（中间无换行）

    fixtures["helper-mismatch"] = _write_fixture(
        os.path.join(tmp, "helper"),
        {"bundle_format": "xraytun-incident/1", "window": {"since_local": "a", "until_local": "b", "source": "s"},
         "versions": {"app": {"value": "0.8.34"}, "core": {"value": "26.9.9"},
                      "helper": {"check": {"state": "Mismatch", "installed": "0.8.33", "bundled": "0.8.34"}}}})

    fixtures["loopback-hole"] = _write_fixture(
        os.path.join(tmp, "loop"), _base_manifest(),
        network="## route -n get 127.0.0.2      （判据：interface 必须是 lo0）\n"
                "   route to: 127.0.0.2\n"
                "destination: default\n"
                "       mask: 128.0.0.0\n"
                "    gateway: 192.168.0.1\n"
                "  interface: en0\n")

    fixtures["tun-iface-einval"] = _write_fixture(
        os.path.join(tmp, "tun"), _base_manifest(),
        core=[_core_line(t0 + i * 10, "[tun] falied to set interface > invalid argument") for i in range(6)])

    fixtures["unknown"] = _write_fixture(
        os.path.join(tmp, "unknown"), _base_manifest(),
        metrics={"stats": {}, "task97": {"v6_rewrite_lines": 0, "classes": {}},
                 "probes": {}, "selfheal": {}},
        events=[], core=[_core_line(t0, "一切正常")],
        network="## route -n get 127.0.0.2\n  interface: lo0\n")

    print("=== 每条 signature 的 fixture ⇒ 期望命中 ===")
    for name, root in fixtures.items():
        tri = triage(load_bundle(root))
        if name == "unknown":
            check("fixture `unknown` ⇒ 没有任何 signature 命中", tri["hits"], [])
        else:
            check(f"fixture `{name}` ⇒ 命中 `{name}`", name in tri["hits"], True)

    # --- 边界 fixture：**必须不命中**。它们才是「谓词被改坏」的探测器：
    #     每一份都带着「像命中但其实不该命中」的那一半特征（少一个关键条件）。
    edge = {}
    edge["v6-rewrite"] = _write_fixture(
        os.path.join(tmp, "v6-edge"), _base_manifest(),
        metrics={"stats": {}, "task97": {"v6_rewrite_lines": 12,
                                         "classes": {"v6_only": {"connections": 5, "failed": 0},
                                                     "mixed": {"connections": 2, "failed": 0}}},
                 "probes": {}, "selfheal": {}},
        core=[_core_line(t0, "[123456789] replace destination with tcp:[240e:1::1]:80")])
    edge["watchdog-false-positive"] = _write_fixture(
        os.path.join(tmp, "wd-edge"), _base_manifest(),
        events=[{"ts_unix": t0, "source": "app", "level": "info",
                 "message": "已作废「自动重连」意图（看门狗重建隧道失败，已退回直连）"}],
        core=[_core_line(t0 + 3600, "[999999999] proxy/vless/outbound: tunneling request to tcp:x:80")])
    edge["probe-false-negative"] = _write_fixture(
        os.path.join(tmp, "probe-edge"), _base_manifest(),
        metrics={"stats": {}, "task97": {}, "selfheal": {},
                 "probes": {"total_connections": 10, "success": 10, "failed_line": 0,
                            "no_outcome_line": 0, "rounds": 5}})
    edge["log-read-loss"] = _write_fixture(
        os.path.join(tmp, "readloss-edge"), _base_manifest(),
        metrics={"stats": {"truncated_lines": 0, "non_json_lines": 0, "blank_lines": 3},
                 "task97": {}, "probes": {}, "selfheal": {}})
    edge["log-write-interleave"] = _write_fixture(
        os.path.join(tmp, "interleave-edge"), _base_manifest(app="0.8.33"),   # **修复前**的版本
        core=[_core_line(t0, "x") + _core_line(t0, "y")])
    edge["helper-mismatch"] = _write_fixture(
        os.path.join(tmp, "helper-edge"),
        {"bundle_format": "xraytun-incident/1", "window": {"since_local": "a", "until_local": "b", "source": "s"},
         "versions": {"app": {"value": "0.8.34"}, "core": {"value": "26.9.9"},
                      "helper": {"check": {"state": "Unreadable", "installed": None, "bundled": None}}}})
    edge["loopback-hole"] = _write_fixture(
        os.path.join(tmp, "loop-edge"), _base_manifest(),
        network="## route -n get 127.0.0.2\n   route to: 127.0.0.2\n"
                "destination: 127.0.0.2\n  interface: lo0\n")
    edge["tun-iface-einval"] = _write_fixture(
        os.path.join(tmp, "tun-edge"), _base_manifest(),
        core=[_core_line(t0 + i * 120, "[tun] falied to set interface > invalid argument") for i in range(5)])

    print("\n=== 边界 fixture：正确口径必须**不**命中（谓词太松就会在这里翻车）===")
    for name, root in edge.items():
        r = PREDICATES[name](load_bundle(root))
        check(f"边界 fixture `{name}` ⇒ 不命中", r.get("hit"), False)

    # --- 双向敏感性：把每条谓词**改坏**（去掉那个关键条件），再拿**同一条边界 fixture** 跑：
    #     改坏后的谓词**必须**会命中 ⇒ 说明「边界 fixture + 这条断言」真的能挡住这种改坏。
    #     （注意：这些是**在真实读数上**判断的弱化版谓词，不是把结果写死。）
    mutants = {
        "v6-rewrite": lambda ev: ev["replace_v6_lines"] > 0,                                # 丢掉失败要求
        "watchdog-false-positive": lambda ev: ev["已作废"] > 0,                              # 丢掉 ±60s 转发要求
        "probe-false-negative": lambda ev: (ev["失败·有 failed 行"] + ev["失败·无结局行"]) >= 0,  # 恒真
        "log-read-loss": lambda ev: ev["截断·残缺行"] + ev["非 JSON 行"] >= 0,                 # 恒真
        "log-write-interleave": lambda ev: ev["多对象行"] > 0,                              # 丢掉版本要求
        "helper-mismatch": lambda ev: ev["版本检查三态"] is not None,                        # 读不到也算命中
        "loopback-hole": lambda ev: ev["interface"] is not None,                            # 两个方向都算命中
        "tun-iface-einval": lambda ev: ev["次数"] > 0,                                      # 丢掉每分钟阈值
    }
    print("\n=== 双向敏感性：谓词改坏 ⇒ 边界 fixture 被误判为命中（= 原断言变红）===")
    for name in SIGNATURES:
        edge_ev = PREDICATES[name](load_bundle(edge[name]))["evidence"]
        pos_ev = PREDICATES[name](load_bundle(fixtures[name]))["evidence"]
        check(f"改坏 `{name}` 后边界 fixture 会被误判命中（⇒ 原断言红）", bool(mutants[name](edge_ev)), True)
        check(f"（对照）同一改坏版在正 fixture 上也为真：`{name}`", bool(mutants[name](pos_ev)), True)


    # --- unknown 的「最像三条」必须给出，且不猜根因
    print("\n=== unknown 行为 ===")
    b = load_bundle(fixtures["unknown"])
    tri = triage(b)
    check("unknown 时 signature = unknown", tri["signature"], "unknown")
    check("unknown 时给出最像的三条", len(tri["closest"]), 3)

    shutil.rmtree(tmp, ignore_errors=True)
    print()
    if fails:
        print(f"self-test：**失败**（{len(fails)} 项）：{fails}")
        return 1
    print("self-test：**全部通过**（8 条 signature 各一条 fixture + 8 组双向敏感性 + unknown 行为）")
    return 0


# ---------------------------------------------------------------- CLI


def main(argv=None):
    ap = argparse.ArgumentParser(description="把现场包自动分诊成 signature + 证据（只读）",
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__.split("用法：")[-1])
    ap.add_argument("--bundle", help="bundle 的 zip 或目录")
    ap.add_argument("--json-out", help="把 incident.json 写到这个路径（默认只打印）")
    ap.add_argument("--md-out", help="把 Markdown 摘要写到这个路径（默认打印到 stdout）")
    ap.add_argument("--privacy-check", metavar="PATH",
                    help="上传前的隐私闸：扫包内文本的密钥模式，命中即**非 0 退出**（fail closed）")
    ap.add_argument("--privacy-json", action="store_true",
                    help="配合 --privacy-check：以 JSON 输出（给流水线/服务端用）；命中仍非 0")
    ap.add_argument("--self-test", action="store_true", help="fixture + 双向敏感性（不读真实数据）")
    a = ap.parse_args(argv)

    if a.privacy_check:
        return privacy_check(a.privacy_check, as_json=a.privacy_json)
    if a.self_test:
        rc = self_test()
        rc2 = privacy_self_test()
        return rc or rc2
    if not a.bundle:
        ap.error("需要 --bundle、--privacy-check 或 --self-test")

    b = load_bundle(a.bundle)
    tri = triage(b)
    caliber = caliber_block(b, a.bundle)
    incident = {"caliber": caliber, "signature": tri["signature"], "hits": tri["hits"],
                "closest": tri["closest"],
                "signatures": {n: {k: v for k, v in tri["results"][n].items() if k != "samples"}
                               for n in SIGNATURES},
                "samples": {n: tri["results"][n].get("samples", []) for n in SIGNATURES
                            if tri["results"][n].get("samples")}}
    md = to_markdown(b, tri, caliber)

    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8") as f:
            json.dump(incident, f, ensure_ascii=False, indent=2)
            f.write("\n")
        print(f"  incident.json → {a.json_out}", file=sys.stderr)
    if a.md_out:
        with open(a.md_out, "w", encoding="utf-8") as f:
            f.write(md)
        print(f"  SUMMARY.md → {a.md_out}", file=sys.stderr)
    if not a.json_out:
        print(json.dumps(incident, ensure_ascii=False, indent=2))
    if not a.md_out:
        print(md)
    if b.get("unpacked_to"):
        shutil.rmtree(b["unpacked_to"], ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
