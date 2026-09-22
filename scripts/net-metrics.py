#!/usr/bin/env python3
"""net-metrics.py —— XrayTun 日志的**只读**指标提取（给「改前 vs 改后」做可比基线）

## 它回答哪三个问题

**① task-97：IPv6 改写会不会让连接失败？**（`analyze_task97`）
   * 对**每一个连接**（按日志行首的 session id `[NNNNNNNNN]` 归并）统计它出现过哪些**目标改写**：
     `replace destination with tcp:[<v6>]` ⇒ v6；`replace destination with tcp:<v4>` ⇒ v4。
     分三类：**v6-only / v4-only / mixed**；
   * 每类里有多少个连接出现了 `failed to open connection`（给条数与百分比）；
   * 另外给两行**行级**计数：`replace destination with tcp:[240e…` 行数、`failed to open connection` 行数。
   * **它回答**：v6 改写是否是失败的必要条件/风险因子（**不是**「v6 改写是否导致用户上不了网」——见下面边界）。

**② task-98 / task-100：探针判据本身可靠吗？**（`analyze_probes`）
   * 探针连接 = 日志里 `proxy/socks: TCP Connect request to tcp:<目标>`，目标默认
     `cp.cloudflare.com:80`（境外）与 `www.baidu.com:80`（境内，v0.8.33 的第二个目标）；
   * 每个探针连接的结局分三种：**成功**（后续出现 `connection opened` 或 `tunneling request`）、
     **失败·有 failed 行**（出现 `failed to process outbound traffic`）、
     **失败·无结局行**（两者都没有 —— 轨迹停在半路，例如只到 `dialing to tcp:<节点>`）；
   * 分轮（>5s 分轮，口径写在输出里）、成功探针 **connect→首次成功行** 的耗时分布（中位/p90/p95/p99/max）、
     以及 **>6s 才成功的条数**（探针侧超时 = 6s ⇒ 这些是**被判据误杀的假阴性**）。
   * **它回答**：探针假阴性有多少、6s 阈值卡在分布哪里、哪一类失败是「快失败」哪一类是「挂住」。

**③ 自愈事件与代价**（`analyze_selfheal`）
   * `已作废「自动重连」意图` / `物理出口已变化` / `隧道已自动恢复` 的**逐条时间**；
   * 每次「已作废」之后到**下一次核心启动**（`core: Xray … started`）之间的**空档秒数** ——
     这就是「隧道被拆掉后多久没有隧道」的量化。
   * **它回答**：看门狗误判的**代价**（用户实际断多久），以及自愈到底有没有成功过（`隧道已自动恢复` 行数）。

## 口径（**会写进输出的那种** —— 见输出的「口径头」那一段）

输出开头有**口径头**（⓪–⑤ 六行）。引用本报告里的**任何数字**，都必须连口径头一起引用；
只引一个数、不引口径，就是本项目反复栽的那类错。

* **切（窗口）**：`--since` / `--until` 把「时点快照」变成「可复现的选择集」。
  日志在被**持续追加**：**N**（窗口内命中条数）与 **T**（窗口右端）都是**移动标记**。
* **解（解析）**：一行**可能含多个 JSON 对象**（`…}{…`，中间没有换行），用 `raw_decode`
  循环解析到行尾。坏行分**四类**计数：**多对象行** / 截断·残缺 / 非 JSON / 空行。
* **匹配（去重）**：**按 `(ts_unix, message)` 去重，先见者胜**；跨文件顺序
  `app.1.jsonl` → `app.jsonl`。两个文件**有重叠**（`trim_log_file` 把同一段尾部写了两代，
  实测重叠 ~39.9k 行）⇒ **不去重会把同一件事算两遍**。
* **单位**：事件时间用 `ts_unix`（秒）；耗时用**消息体内的微秒时间戳**
  （`2026/09/22 13:32:36.785649`，`ts_unix` 只有秒级）；「探针轮」= 相邻探针间隔 **> 5s** 分轮；
  探针超时 = **6s**；字节 = B。
* **行内上报的写入侧前提**：`message 含 LF 的记录数`。
  写入侧（`AppState::log`，`apps/desktop/src/state.rs:387`）把 `\n` 转义成字面 `\n`、删掉 `\r`，
  所以这个数**应当恒为 0**。它被写进输出，是因为「安全」必须是**被监控的量**而不是一句前提。
* **选择内容指纹**（sha256，口径随本文件冻结）：
  对「本次选中的记录」按 `(ts_unix, message)` 排序，逐条拼接**帧**再取 sha256。
  帧 = `<body 的 UTF-8 字节数>:<body>\n`，`body = "<ts_unix>|<message>"`（**不含 source**）。
  长度前缀把**记录边界**写进字节里 ⇒ **一条 message 含真实 LF 的记录永远 ≠ 两条记录**；
  朴素的 `<ts>|<msg>\n` 拼接**不是单射**（self_test 夹具C 就是那个碰撞）。
  ⚠️ **它不是「日志文件指纹」**：只覆盖「本次选中的记录」（窗口 + 去重之后）。
  换窗口、换去重键、换文件顺序都会让它变；文件里**没被选中**的行它一无所知。

## 已知边界（**测不到什么**）

1. **探针失败 ≠ 用户当时上不了网**：它只证明「这一个 URL 经 SOCKS 的这一次请求没成」；
   用户浏览器的体验要端到端证据。
2. **不做因果**：① 只给相关性数字（v6 改写 vs 失败、tun 错误 vs 重建窗口）。
3. **看不到应用侧超时**：探测的 6s 超时**不落盘**，所以「某条探针在 6s 被放弃」无法直接观测，
   只能用「>6s 才成功」的条数间接量化误杀。
4. **`app.jsonl` 不是完整应用日志**：只有 `state.log()` 落盘，`push_log()` 只进内存环形缓冲
   ⇒ 看门狗过程行（「隧道连续 N 次不通」「正在自动重建」）**不在文件里**。
5. 真机安装/更新行为、helper 行为、UI 表现 —— 本脚本一概看不到。

用法：
  python3 scripts/net-metrics.py                      # 默认定位日志目录，全部记录
  python3 scripts/net-metrics.py --until 13:42:30     # 固定到某个时点（可复现）
  python3 scripts/net-metrics.py --since 13:00 --until 14:00
  python3 scripts/net-metrics.py --json
  python3 scripts/net-metrics.py --export /tmp/selection.blob   # 证据子集（被指纹的字节）
  python3 scripts/net-metrics.py --self-test          # 人造小日志的自测 + 敏感性（不联网、只写 /tmp）

⚠️ 引用任何数字都要连**口径头**（切/解/匹配/单位 + 选择内容指纹）一起引用；
   同一个命令在不同时刻会给出不同数字（日志在追加）⇒ 可复现的前提是**钉住 --until**。
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, statistics as st, sys, tempfile
from datetime import datetime, timedelta

# ---------------------------------------------------------------- 常量 / 正则
LOG_DIR_DEFAULT = os.path.expanduser("~/Library/Application Support/com.xraytun.desktop/logs")
LOG_FILES = ("app.1.jsonl", "app.jsonl")          # 旧 → 新；两者有重叠，必须去重
DEFAULT_PROBE_TARGETS = ("cp.cloudflare.com:80", "www.baidu.com:80")
ROUND_GAP_SECS = 5.0        # 分轮阈值（写在输出里）
PROBE_TIMEOUT_SECS = 6.0    # 应用侧探针超时（v0.8.33；用于统计「>6s 才成功」）
DEDUPE_KEY = "(ts_unix, message)"   # 去重键（写在输出里；改它必须改同名的自测）
# 口径版本：改口径（切/解/匹配/单位/指纹）就必须改这个字符串，
# 好让文档里的旧数字一眼能看出「不是同一把尺子量的」。
CALIBER_VERSION = "口径头 v2（2026-09-22：切/解/匹配/单位 + 选择内容指纹）"
FINGERPRINT_ALGO = "sha256"

RE_ID = re.compile(r"\[(\d{6,})\]\s")
RE_CONNECT = re.compile(r"proxy/socks: TCP Connect request to (tcp:[^ ]+)")
RE_V6 = re.compile(r"replace destination with tcp:\[([0-9a-fA-F:]+)\]")
RE_V4 = re.compile(r"replace destination with tcp:(\d+\.\d+\.\d+\.\d+)")
RE_MSG_TS = re.compile(r"^(\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}\.\d+)")
SUCCESS_MARKS = ("connection opened", "tunneling request")
FAILED_MARK = "failed to process outbound traffic"
CORE_START_MARK = "core: Xray"
VOID_MARK = "已作废「自动重连」意图"
EGRESS_MARK = "物理出口已变化"
RECOVERED_MARK = "隧道已自动恢复"


def msg_time(msg: str):
    """消息体内的微秒时间戳 → epoch 秒（拿不到就 None）。"""
    m = RE_MSG_TS.match(msg)
    return datetime.strptime(m.group(1), "%Y/%m/%d %H:%M:%S.%f").timestamp() if m else None


# ---------------------------------------------------------------- 载入 + 去重
_DEC = json.JSONDecoder()


def parse_line(line):
    """解析**一行**。返回 (objects, err)。

    ⚠️ **一行可能含多个 JSON 对象** —— 实测 2026-09-22 有 **4 行**是这样：
    一个 `source=app` 事件紧挨着一个 `source=core` 横幅（`…}{…`，中间没有换行）。
    **一行一次 `json.loads` 会把这类行整行丢掉**（第一版就栽在这里：把「隧道已自动恢复」
    漏成 0 行，实际是 3 次）。所以这里用 `raw_decode` 循环解析到行尾。

    err=True 表示**中途解析失败**（尾部残缺/截断）—— 此时仍返回已成功解析的对象。
    """
    objs, i, n, err = [], 0, len(line), False
    while i < n:
        while i < n and line[i] in " \t\r\n":
            i += 1
        if i >= n:
            break
        try:
            obj, end = _DEC.raw_decode(line, i)
        except ValueError:
            err = True
            break
        objs.append(obj)
        i = end
    return objs, err


def load_records(paths, since=None, until=None):
    """读所有日志文件，按 (ts_unix, message) 去重；返回按时间排序的 record 列表。

    record = {"t": ts_unix, "src": source, "level": level, "msg": message}
    去重是**必须**的：app.jsonl 与 app.1.jsonl 有重叠。

    「坏行」**分三类**统计（不再混成一个数）：**多对象行** / 截断·残缺行 / 非 JSON 行。
    另外统计 `lf_in_message`：**选中的**记录里 message 含真实 LF 的条数（写入侧前提的被监控量）。
    """
    seen, out = set(), []
    stats = {"lines": 0, "objects": 0, "multi_object_lines": 0, "blank_lines": 0,
             "dup": 0, "kept": 0, "truncated_lines": 0, "non_json_lines": 0,
             "src_app": 0, "src_core": 0, "src_other": 0, "lf_in_message": 0,
             "dup_in_window": 0}
    for p in paths:
        if not os.path.exists(p):
            continue
        with open(p, "rb") as f:
            for raw in f:
                stats["lines"] += 1
                line = raw.decode("utf-8", "replace")
                if not line.strip():               # 空行不是「坏行」——单列一类（实测 3 行）
                    stats["blank_lines"] += 1
                    continue
                objs, err = parse_line(line)
                stats["objects"] += len(objs)
                if len(objs) > 1:
                    stats["multi_object_lines"] += 1
                if not objs:
                    if line.lstrip().startswith("{"):
                        stats["truncated_lines"] += 1
                    else:
                        stats["non_json_lines"] += 1
                    continue
                if err:
                    stats["truncated_lines"] += 1      # 部分成功：对象照用，同时记一笔残缺
                for d in objs:
                    if not isinstance(d, dict):
                        stats["non_json_lines"] += 1
                        continue
                    key = (d.get("ts_unix"), d.get("message"))
                    if key in seen:
                        stats["dup"] += 1
                        # 「丢弃了几条」有两个意思：**全文件扫描**丢了多少，
                        # 与**本窗口内**丢了多少。两个都算，输出里分列 —— 否则
                        # 读者会把一个全文件口径的数当成窗口内的数（这是本项目的常见栽法）。
                        td = d.get("ts_unix") or 0
                        if (since is None or td >= since) and (until is None or td <= until):
                            stats["dup_in_window"] += 1
                        continue
                    seen.add(key)
                    t = d.get("ts_unix") or 0
                    if since is not None and t < since:
                        continue
                    if until is not None and t > until:
                        continue
                    src = d.get("source")
                    stats["src_app" if src == "app" else ("src_core" if src == "core" else "src_other")] += 1
                    msg = d.get("message") or ""
                    if "\n" in msg:                      # 写入侧本该把 LF 转义掉 ⇒ 期望恒为 0
                        stats["lf_in_message"] += 1
                    out.append({"t": t, "src": src, "level": d.get("level"), "msg": msg})
                    stats["kept"] += 1
    out.sort(key=lambda r: r["t"])
    return out, stats


# ---------------------------------------------------------------- 选择内容指纹
def frame_record(t, msg):
    """单条记录的**帧**：`<body 的 UTF-8 字节数>:<body>\\n`，其中 `body = "<ts>|<message>"`。

    长度前缀是**分帧安全**的关键：朴素的 `<ts>|<msg>\\n` 拼接**不是单射** ——
    一条 message 含真实 LF 的记录（`"C1\\n3001|C2"`）与两条记录（`"C1"` / `"C2"`）
    会拼出**完全相同**的字节串（self_test 夹具C 就是这个碰撞）。
    把长度写进帧之后，「记录边界」不再依赖 message 的内容。

    实测：本机 2026-09-22 的日志里 **message 含 LF 的记录 = 0** ⇒ 今天两种口径的指纹
    *恰好*一致；**正因为如此**，这个前提必须被行内上报、被监控，而不是被假设。
    """
    body = f"{int(t)}|{msg}"
    return f"{len(body.encode('utf-8'))}:{body}\n"


def selection_blob(records):
    """被哈希的字节串本身：按 `(ts_unix, message)` 排序后逐条拼帧。

    排序是**口径的一部分**（与文件顺序、去重时谁先见无关）⇒ 同一选择集**必然**同指纹。
    """
    ordered = sorted(records, key=lambda r: (int(r["t"]), r["msg"]))
    return "".join(frame_record(r["t"], r["msg"]) for r in ordered)


def selection_fingerprint(records):
    """**选择内容指纹**（sha256 of [`selection_blob`]）。

    ⚠️ **它不是「日志文件指纹」**：只覆盖「本次选中的记录」（窗口 + 去重之后）。
    日志在被持续追加 ⇒ 同一条命令在不同时刻得到**不同**指纹，除非同时钉住 `--until`。
    想指纹整个日志文件是**另一件事**，本脚本不提供、也不假装提供。
    """
    return hashlib.sha256(selection_blob(records).encode("utf-8")).hexdigest()


# ---------------------------------------------------------------- ① task-97
def analyze_task97(records):
    kinds, failed_ids = {}, set()
    v6_lines = fail_lines = 0
    for r in records:
        if r["src"] != "core":
            continue
        m = r["msg"]
        if "replace destination with tcp:[" in m and "240e:" in m:
            v6_lines += 1
        if "failed to open connection" in m:
            fail_lines += 1
        mid = RE_ID.search(m)
        if not mid:
            continue
        cid = mid.group(1)
        ks = kinds.setdefault(cid, set())
        if RE_V6.search(m):
            ks.add("v6")
        if RE_V4.search(m):
            ks.add("v4")
        if "failed to open connection" in m:
            failed_ids.add(cid)
    classes = {"v6_only": [], "v4_only": [], "mixed": []}
    for cid, ks in kinds.items():
        if ks == {"v6"}:
            classes["v6_only"].append(cid)
        elif ks == {"v4"}:
            classes["v4_only"].append(cid)
        elif ks == {"v6", "v4"}:
            classes["mixed"].append(cid)
    res = {"v6_rewrite_lines": v6_lines, "failed_open_lines": fail_lines, "classes": {}}
    for name, ids in classes.items():
        n = len(ids)
        f = sum(1 for c in ids if c in failed_ids)
        res["classes"][name] = {"connections": n, "failed": f,
                                "pct": (100.0 * f / n) if n else 0.0}
    res["connections_with_rewrite"] = sum(len(v) for v in classes.values())
    return res


# ---------------------------------------------------------------- ② 探针
def analyze_probes(records, targets=DEFAULT_PROBE_TARGETS):
    # 1) 找出探针连接
    meta = {}
    for r in records:
        if r["src"] != "core":
            continue
        m = r["msg"]
        if "TCP Connect request" not in m:
            continue
        g = RE_CONNECT.search(m)
        if not g or g.group(1).replace("tcp:", "") not in targets:
            continue
        mid = RE_ID.search(m)
        if not mid:
            continue
        meta[mid.group(1)] = {"t0": msg_time(m) or r["t"], "target": g.group(1).replace("tcp:", "")}
    # 2) 收集每个探针连接的轨迹
    trace = {cid: [] for cid in meta}
    for r in records:
        if r["src"] != "core":
            continue
        mid = RE_ID.search(r["msg"])
        if mid and mid.group(1) in trace:
            trace[mid.group(1)].append(r["msg"])
    # 3) 判定
    conns = []
    for cid, info in meta.items():
        msgs = trace[cid]
        succ_t = None
        for m in msgs:
            if any(s in m for s in SUCCESS_MARKS):
                succ_t = msg_time(m)
                if succ_t:
                    break
        has_fail = any(FAILED_MARK in m for m in msgs)
        if succ_t is not None:
            outcome = "success"
        elif has_fail:
            outcome = "failed_line"
        else:
            outcome = "no_outcome_line"
        lat = (succ_t - info["t0"]) if (succ_t and info["t0"]) else None
        conns.append({"cid": cid, "t0": info["t0"], "target": info["target"],
                      "outcome": outcome, "lat": lat, "n": len(msgs)})
    conns.sort(key=lambda c: c["t0"])
    # 4) 分轮：>5s 分轮
    rounds, cur = [], []
    for c in conns:
        if cur and c["t0"] - cur[-1]["t0"] > ROUND_GAP_SECS:
            rounds.append(cur); cur = []
        cur.append(c)
    if cur:
        rounds.append(cur)
    # 5) 汇总
    def pct(v, q):
        v = sorted(v)
        return v[min(len(v) - 1, int(q * len(v)))] if v else None
    by_target = {}
    for tgt in targets:
        sub = [c for c in conns if c["target"] == tgt]
        lat = [c["lat"] for c in sub if c["outcome"] == "success" and c["lat"] is not None]
        by_target[tgt] = {
            "connections": len(sub),
            "success": sum(1 for c in sub if c["outcome"] == "success"),
            "failed_line": sum(1 for c in sub if c["outcome"] == "failed_line"),
            "no_outcome_line": sum(1 for c in sub if c["outcome"] == "no_outcome_line"),
            "latency_ms": ({
                "median": st.median(lat) * 1000, "p90": pct(lat, .90) * 1000,
                "p95": pct(lat, .95) * 1000, "p99": pct(lat, .99) * 1000,
                "max": max(lat) * 1000, "n": len(lat),
            } if lat else None),
            "success_over_timeout": sum(1 for x in lat if x > PROBE_TIMEOUT_SECS),
        }
    return {
        "targets": by_target,
        "total_connections": len(conns),
        "success": sum(1 for c in conns if c["outcome"] == "success"),
        "failed_line": sum(1 for c in conns if c["outcome"] == "failed_line"),
        "no_outcome_line": sum(1 for c in conns if c["outcome"] == "no_outcome_line"),
        "rounds": len(rounds),
        "rounds_with_failure": sum(1 for r in rounds if any(c["outcome"] != "success" for c in r)),
        "round_gap_secs": ROUND_GAP_SECS,
        "timeout_secs": PROBE_TIMEOUT_SECS,
        "_rounds": rounds, "_conns": conns,
    }


# ---------------------------------------------------------------- ③ 自愈事件
def analyze_selfheal(records):
    events, traffic = [], []
    for r in records:
        m = r["msg"]
        if r["src"] == "core" and any(s in m for s in SUCCESS_MARKS):
            traffic.append(r["t"])           # 隧道确实在转发流量的时刻
        if VOID_MARK in m:
            events.append({"t": r["t"], "kind": "void_intent"})
        elif EGRESS_MARK in m:
            events.append({"t": r["t"], "kind": "egress_changed"})
        elif RECOVERED_MARK in m:
            events.append({"t": r["t"], "kind": "auto_recovered"})
        elif r["src"] == "core" and CORE_START_MARK in m and "started" in m:
            events.append({"t": r["t"], "kind": "core_start"})
    events.sort(key=lambda e: e["t"])
    traffic.sort()
    starts = [e["t"] for e in events if e["kind"] == "core_start"]
    voids = []
    for e in events:
        if e["kind"] != "void_intent":
            continue
        nxt_start = next((s for s in starts if s > e["t"]), None)
        t_before = next((t for t in reversed(traffic) if t <= e["t"]), None)
        t_after = next((t for t in traffic if t > e["t"]), None)
        voids.append({
            "t": e["t"],
            "next_core_start": nxt_start,
            # 口径 A：**作废 → 下一次 core 启动**（「多久之后才又有核心」）
            "gap_to_next_core_secs": (nxt_start - e["t"]) if nxt_start else None,
            # 口径 B：**最后一条流量证据 → 下一条流量证据**（「没有隧道在转发」的实际空档）
            #          —— 这是 task-95 用的口径，也是卡片里 2660/690/2620 的来源。
            "traffic_gap_secs": (t_after - t_before) if (t_before and t_after) else None,
            "last_traffic_before": t_before, "first_traffic_after": t_after,
        })
    return {
        "void_intents": voids,
        "egress_changed": [e["t"] for e in events if e["kind"] == "egress_changed"],
        "auto_recovered": [e["t"] for e in events if e["kind"] == "auto_recovered"],
        "core_starts": starts,
        "traffic_evidence_lines": len(traffic),
    }


# ---------------------------------------------------------------- 输出
def hm(t):
    return datetime.fromtimestamp(t).strftime("%H:%M:%S") if t else "—"


def full(t):
    """窗口端点用**带日期**的格式：只写 HH:MM:SS 在跨天窗口里是有歧义的。"""
    return datetime.fromtimestamp(t).strftime("%Y-%m-%d %H:%M:%S") if t else "—"


def caliber_lines(stats, m2, ctx):
    """**口径头**（六行）。引用本报告任何数字时**必须**连它一起引用。

    ⓪ 身份 + 移动标记（N/T） ① 切（窗口） ② 解（解析） ③ 匹配（去重） ④ 单位 ⑤ 选择内容指纹
    """
    win = ctx["window"]
    return [
        "口径头（引用任何数字请连这一段一起引用）：",
        f"  ⓪ 身份/移动标记：工具=scripts/net-metrics.py；{CALIBER_VERSION}",
        f"     N=窗口内命中 {stats['kept']} 条（**移动标记**：日志在被持续追加）；"
        f"T=窗口右端 {win['until']}（**移动标记**）",
        f"     日志目录={ctx['log_dir']}；文件 {ctx['n_present']}/{len(LOG_FILES)} 个存在：{ctx['snap']}",
        f"  ① 切（窗口）  ：{win['since']} → {win['until']}（本地时区）",
        f"  ② 解（解析）  ：**全文件扫描**：读 {stats['lines']} 行 → {stats['objects']} 个对象；"
        f"多对象行 {stats['multi_object_lines']} 行（一行含 >1 个 JSON 对象，已全部计入）",
        f"     坏行分四类：截断·残缺 {stats['truncated_lines']} / 非 JSON {stats['non_json_lines']} / "
        f"空行 {stats['blank_lines']}",
        f"     **message 含 LF 的记录数 = {stats['lf_in_message']}**"
        f"（写入侧 `AppState::log` 转义后应恒为 0 —— 这是**被监控的量**，不是假设）",
        f"  ③ 匹配（去重）：键 = {DEDUPE_KEY}，先见者胜；跨文件顺序 {' → '.join(LOG_FILES)}；"
        f"丢弃重复 **{stats['dup']} 条（全文件扫描口径）**，其中落在本窗口内 {stats['dup_in_window']} 条；"
        f"命中 {stats['kept']} 条",
        f"     来源：source=app {stats['src_app']} / source=core {stats['src_core']} / 其它 {stats['src_other']}",
        f"  ④ 单位        ：时间=ts_unix(秒)；耗时=消息内微秒时间戳；"
        f"探针轮=相邻间隔>{m2['round_gap_secs']:.0f}s；探针超时={m2['timeout_secs']:.0f}s；字节=B",
        f"  ⑤ 选择内容指纹：{FINGERPRINT_ALGO}:{ctx['fingerprint']}",
        f"     ⚠️ 这是「本次选中的 {stats['kept']} 条」的指纹，**不是日志文件指纹**"
        f"（换窗口 / 换去重键即变；未选中的行不覆盖）",
    ]


def print_report(stats, m1, m2, m3, ctx):
    print("=" * 78)
    print("net-metrics：XrayTun 日志指标（只读）")
    for ln in caliber_lines(stats, m2, ctx):
        print(ln)
    print("=" * 78)

    print("\n【① task-97：目标改写 × failed to open connection】")
    print(f"  `replace destination with tcp:[240e…` 行数 = {m1['v6_rewrite_lines']}")
    print(f"  `failed to open connection`           行数 = {m1['failed_open_lines']}")
    print(f"  有改写的连接总数（按 session id 归并） = {m1['connections_with_rewrite']}")
    print("  类别           连接数   失败数   失败率")
    for k, label in (("v6_only", "全是 v6 改写"), ("v4_only", "全是 v4 改写"), ("mixed", "v6+v4 混合")):
        c = m1["classes"][k]
        print(f"  {label:12s} {c['connections']:7d} {c['failed']:8d}  {c['pct']:6.1f}%")

    print("\n【② 探针指标（按目标）】")
    print(f"  探针连接总数={m2['total_connections']}  成功={m2['success']}  失败·有 failed 行={m2['failed_line']}  失败·无结局行={m2['no_outcome_line']}")
    print(f"  探针轮数={m2['rounds']}（其中含失败的轮 {m2['rounds_with_failure']}）")
    for tgt, d in m2["targets"].items():
        print(f"  -- {tgt}")
        print(f"     连接={d['connections']}  成功={d['success']}  失败·failed行={d['failed_line']}  失败·无结局行={d['no_outcome_line']}")
        L = d["latency_ms"]
        if L:
            print(f"     成功耗时(ms)：中位={L['median']:.1f} p90={L['p90']:.1f} p95={L['p95']:.1f} p99={L['p99']:.1f} max={L['max']:.1f}  (n={L['n']})")
            print(f"     **>{m2['timeout_secs']:.0f}s 才成功的条数 = {d['success_over_timeout']}**（会被 6s 判据误杀）")
        else:
            print("     （无成功样本，无法给耗时分布）")

    print("\n【③ 自愈事件】")
    print(f"  「已作废」{len(m3['void_intents'])} 次：「隧道已自动恢复」{len(m3['auto_recovered'])} 行；流量证据行 {m3['traffic_evidence_lines']}")
    print("  两个口径都列（**引用时必须写明是哪个**）：")
    print("    口径A = 作废 → 下一次 core 启动；口径B = 最后一条流量证据 → 下一条流量证据（task-95 用的口径）")
    for v in m3["void_intents"]:
        ga = v["gap_to_next_core_secs"]; gb = v["traffic_gap_secs"]
        print(f"    已作废 {hm(v['t'])} → 下次 core 启动 {hm(v['next_core_start'])}"
              f"（口径A {ga if ga is not None else '—'} s；口径B {gb if gb is not None else '—'} s）")
    print(f"  「物理出口已变化」{len(m3['egress_changed'])} 次：" + "、".join(hm(t) for t in m3["egress_changed"]))
    print(f"  core 启动 {len(m3['core_starts'])} 次：" + "、".join(hm(t) for t in m3["core_starts"]))


# ---------------------------------------------------------------- 自测
def self_test():
    """人造小日志（写在 /tmp）+ 断言 + 敏感性。

    覆盖：① 跨文件去重 ② v6/v4 分类与失败率 ③ 探针三种结局（含「无结局行」）
          ④ 分轮 ⑤ 空档秒数 ⑥ 敏感性（改一条 → 数字必须变）。
    """
    tmp = tempfile.mkdtemp(prefix="net-metrics-selftest-")
    def rec(sid, msg, t, src="core"):
        # 真实日志里核心行都是「<微秒时间戳> [级别] [sid] …」；自测也必须带时间戳，
        # 否则 msg_time() 取不到时间（第一版自测就栽在这里）。
        stamp = datetime.fromtimestamp(t).strftime("%Y/%m/%d %H:%M:%S.%f")
        return json.dumps({"ts_unix": int(t), "level": "Info", "source": src,
                           "message": f"{stamp} [Info] {msg}"})
    base = int(datetime(2026, 9, 22, 13, 0, 0).timestamp())
    rows_old, rows_new = [], []
    # --- 连接 A：v6-only + failed to open connection
    rows_old.append(rec(1, "[111111111] replace destination with tcp:[240e:1::1]:80", base + 1))
    rows_old.append(rec(1, "[111111111] failed to open connection", base + 2))
    # --- 连接 B：v4-only 成功
    rows_new.append(rec(2, "[222222222] replace destination with tcp:1.2.3.4:80", base + 3))
    rows_new.append(rec(2, "[222222222] proxy/freedom: connection opened to tcp:x:80", base + 4))
    # --- 连接 C：混合
    rows_new.append(rec(3, "[333333333] replace destination with tcp:[240e:2::2]:80", base + 5))
    rows_new.append(rec(3, "[333333333] replace destination with tcp:5.6.7.8:80", base + 6))
    # --- 探针 1：成功（cloudflare），耗时 0.5s
    rows_new.append(rec(4, "[444444444] proxy/socks: TCP Connect request to tcp:cp.cloudflare.com:80", base + 10))
    rows_new.append(rec(4, "[444444444] proxy/vless/outbound: tunneling request to tcp:cp.cloudflare.com:80", base + 10.5))
    # --- 探针 2：失败·无结局行（cloudflare），10 秒后（新的一轮）
    rows_new.append(rec(5, "[555555555] proxy/socks: TCP Connect request to tcp:cp.cloudflare.com:80", base + 20))
    rows_new.append(rec(5, "[555555555] transport/internet/tcp: dialing to tcp:45.207.197.185:443", base + 20.1))
    # --- 探针 3：失败·有 failed 行（baidu），同一轮
    rows_new.append(rec(6, "[666666666] proxy/socks: TCP Connect request to tcp:www.baidu.com:80", base + 20.2))
    rows_new.append(rec(6, "[666666666] failed to process outbound traffic > proxy/freedom", base + 20.3))
    # --- 自愈：13:10 作废 → 13:11 core 启动（口径A = 60s）；流量在 13:00:10.5 与 13:11:40（口径B = 689.5s）
    rows_new.append(json.dumps({"ts_unix": base + 600, "level": "info", "source": "app",
                                "message": "已作废「自动重连」意图（看门狗重建隧道失败，已退回直连）：下次启动不会自动连"}))
    rows_new.append(rec(7, "core: Xray 26.9.9 started", base + 660))
    rows_new.append(rec(9, "[999999999] proxy/freedom: connection opened to tcp:z:80", base + 700))
    # --- 同一个「去重」样本：一条在旧文件里，一条在新文件里（完全相同的 ts+message）
    dup = rec(8, "[888888888] replace destination with tcp:9.9.9.9:80", base + 700)
    rows_old.append(dup); rows_new.append(dup)
    # --- ⚠️ **一行两个 JSON 对象**（真实日志里出现过 4 次：一个 app 事件 + 一个 core 横幅）
    #     这一行是 task-103 的回归用例：用「一行一次 json.loads」会整行丢掉 ⇒「自动恢复」被漏。
    two_obj = ('{"ts_unix": %d, "source": "app", "level": "info", '
               '"message": "隧道已自动恢复（第 1 次自动重建）"}' % (base + 800)) + \
              ('{"ts_unix": %d, "source": "core", "level": "info", '
               '"message": "%s [Warning] core: Xray 26.9.9 started"}'
               % (base + 800, datetime.fromtimestamp(base + 800).strftime("%Y/%m/%d %H:%M:%S.%f")))
    rows_new.append(two_obj)

    p_old = os.path.join(tmp, "app.1.jsonl"); p_new = os.path.join(tmp, "app.jsonl")
    open(p_old, "w").write("\n".join(rows_old) + "\n")
    open(p_new, "w").write("\n".join(rows_new) + "\n")
    print(f"=== --self-test：人造日志在 {tmp}（{len(rows_old)}+{len(rows_new)} 行，其中 1 行跨文件重复）===")

    fails = []
    def check(name, got, want):
        ok = got == want
        print(f"  {'✓' if ok else '✗'} {name}: got={got!r} want={want!r}")
        if not ok:
            fails.append(name)

    recs, stats = load_records([p_old, p_new])
    check("去重后条数（18 行 → 18 个对象 → 去重后 18 条）", stats["kept"], 18)
    check("丢弃重复条数", stats["dup"], 1)

    m1 = analyze_task97(recs)
    check("v6_only 连接数", m1["classes"]["v6_only"]["connections"], 1)
    check("v6_only 失败数", m1["classes"]["v6_only"]["failed"], 1)
    check("v6_only 失败率%", round(m1["classes"]["v6_only"]["pct"], 1), 100.0)
    check("v4_only 连接数", m1["classes"]["v4_only"]["connections"], 2)   # B(2) + 去重样本(8)
    check("v4_only 失败数", m1["classes"]["v4_only"]["failed"], 0)
    check("mixed 连接数", m1["classes"]["mixed"]["connections"], 1)
    check("replace [240e 行数", m1["v6_rewrite_lines"], 2)
    check("failed to open 行数", m1["failed_open_lines"], 1)

    m2 = analyze_probes(recs)
    check("探针连接总数", m2["total_connections"], 3)
    check("探针成功数", m2["success"], 1)
    check("探针 失败·有failed行", m2["failed_line"], 1)
    check("探针 失败·无结局行", m2["no_outcome_line"], 1)
    check("探针轮数（>5s 分轮 → 2 轮）", m2["rounds"], 2)
    check("cloudflare 成功耗时≈500ms", round(m2["targets"]["cp.cloudflare.com:80"]["latency_ms"]["median"]), 500)

    m3 = analyze_selfheal(recs)
    check("已作废次数", len(m3["void_intents"]), 1)
    check("口径A：作废→下次 core 启动", m3["void_intents"][0]["gap_to_next_core_secs"], 60)
    check("口径B：流量空档（13:00:10→13:11:40）", m3["void_intents"][0]["traffic_gap_secs"], 690)
    check("「隧道已自动恢复」行数（来自**一行两个对象**那一行）", len(m3["auto_recovered"]), 1)

    # ---------- 用例：一行两个对象（task-103 回归） ----------
    print("\n=== 用例：一行两个 JSON 对象（app 事件 + core 横幅）===")
    check("多对象行计数", stats["multi_object_lines"], 1)
    check("解析出的对象数 = 行数 + 1（多对象行多出 1 个）", stats["objects"], stats["lines"] + 1)
    check("source=app 条数（作废 1 + 自动恢复 1）", stats["src_app"], 2)
    print(f"  该行原文（截断显示）：{two_obj[:110]}…")

    # ---------- 敏感性 0（本卡新增）：退回「一行一次 json.loads」⇒ 自动恢复必须丢 ----------
    print("\n=== 敏感性 0：退回「一行一次 json.loads」⇒ 「隧道已自动恢复」必须丢（红）===")
    def legacy_load(paths):
        """只用于对照的**旧实现**：一行一次 json.loads（task-103 之前的行为）。"""
        seen2, out2 = set(), []
        for p in paths:
            if not os.path.exists(p):
                continue
            for raw in open(p, "rb"):
                try:
                    d = json.loads(raw)
                except Exception:
                    continue                      # ← 多对象行在这里被整行丢弃
                k = (d.get("ts_unix"), d.get("message"))
                if k in seen2:
                    continue
                seen2.add(k)
                out2.append({"t": d.get("ts_unix") or 0, "src": d.get("source"),
                             "level": d.get("level"), "msg": d.get("message") or ""})
        return out2
    legacy_recs = legacy_load([p_old, p_new])
    m3_legacy = analyze_selfheal(legacy_recs)
    print(f"  旧实现（一行一次 json.loads）：记录 {len(legacy_recs)} 条，"
          f"「隧道已自动恢复」= **{len(m3_legacy['auto_recovered'])} 行**   ← 丢事件")
    print(f"  新实现（raw_decode 循环）：   记录 {len(recs)} 条，"
          f"「隧道已自动恢复」= **{len(m3['auto_recovered'])} 行**")
    check("敏感性 0：旧实现丢掉该事件（=0）", len(m3_legacy["auto_recovered"]), 0)
    check("敏感性 0：新实现看见它（=1）", len(m3["auto_recovered"]), 1)

    # ---------- 敏感性：把探针 2 的「dialing」行删掉 ⇒ 它变成 n=1，但结局仍是 no_outcome；
    #            真正该变的是「把 v6 那行改成 v4」⇒ 分类必须从 v6_only 变 v4_only。
    print("\n=== 敏感性 1：把连接 A 的 v6 改写改成 v4 改写 ⇒ 必须不再计入 v6_only ===")
    mut = []
    for r in recs:
        m = r["msg"]
        if "[111111111]" in m and "240e:" in m:
            m = m.replace("tcp:[240e:1::1]:80", "tcp:7.7.7.7:80")
        mut.append({**r, "msg": m})
    m1b = analyze_task97(mut)
    print(f"  改前 v6_only=1/失败1  改后 v6_only={m1b['classes']['v6_only']['connections']}/失败{m1b['classes']['v6_only']['failed']}")
    check("敏感性 1：v6_only 变 0", m1b["classes"]["v6_only"]["connections"], 0)
    if m1b["classes"]["v6_only"]["connections"] == m1["classes"]["v6_only"]["connections"]:
        fails.append("敏感性 1 未生效")

    print("\n=== 敏感性 2：把探针 1 的成功行删掉 ⇒ 成功数必须 -1、无结局行 +1 ===")
    mut2 = [r for r in recs if not (r["msg"].find("[444444444]") >= 0 and "tunneling request" in r["msg"])]
    m2b = analyze_probes(mut2)
    print(f"  改前 成功={m2['success']}/无结局行={m2['no_outcome_line']}  改后 成功={m2b['success']}/无结局行={m2b['no_outcome_line']}")
    check("敏感性 2：成功 -1", m2b["success"], m2["success"] - 1)
    check("敏感性 2：无结局行 +1", m2b["no_outcome_line"], m2["no_outcome_line"] + 1)

    print("\n=== 敏感性 3：把 core 启动推后 120s ⇒ 口径A 必须从 60s 变 180s ===")
    mut3 = [{**r, "t": r["t"] + 120 if ("core: Xray" in r["msg"] and "started" in r["msg"]) else r["t"]} for r in recs]
    m3b = analyze_selfheal(mut3)
    print(f"  改前 口径A={m3['void_intents'][0]['gap_to_next_core_secs']}s  改后 口径A={m3b['void_intents'][0]['gap_to_next_core_secs']}s")
    check("敏感性 3：口径A 变 180s", m3b["void_intents"][0]["gap_to_next_core_secs"], 180)

    # ---------- 夹具 A/B/C：指纹口径的**六个写死的期望值** ----------
    # 口径（本文件冻结，见模块 docstring）：键 = (ts_unix, message)；串 = "<ts>|<message>"；分帧安全。
    #
    # ⚠️ 每个「错误口径」**只偏一个自由度**，并且**按偏差写标签** ——
    #    只写「错误口径」这四个字，本身就是一个**没写死的口径**（2026-09-22 真的因此算错过两个值）：
    #      夹具A 错误 = **仅**帧里的串改成含 src（去重键与排序不变）
    #      夹具B 错误 = **仅**去重键改成 (ts, src, message)（串与排序不变）
    #      夹具C      = 分帧安全性：**一条 message 含真实 LF 的记录 ≠ 两条记录**
    print("\n=== 夹具：选择内容指纹（三个夹具 / 六个期望值 / 每个错误口径只偏一个自由度）===")

    FP = {
        # 六个期望值：**写死的常量**。它们只对「本文件的帧口径」（见 frame_record）成立；
        # 想跨实现核对，看下面打印出来的**规范化字节串**（digest 只是它的 sha256）。
        "A_correct": "fb823743341ac392f201f700e078604c06741c2821d076919e2bc76895566f4a",
        "A_wrong_src": "4119d46d0c291d3459df2da7ebeaf5b7aaa375fa26c3538a4377375e177ff484",
        "B_correct": "f1648152cbbe8db8ed94452ca6a63ecea4d518682868e132a7550064e5678b79",
        "B_wrong_key": "46c953690bb58b122163786a7681cbe72bed0ff10084cecd4e596fec941884b6",
        "C_one": "72d7b49ebbc8d725bdf31e0f56ca9c0037bb15a92e201ccdb57bf698cf20d3d1",
        "C_two": "9e36ab5bc974241444d282109127d0a75f33efa29a4c24a6fc1b1e9f1cf88dfe",
    }

    def fp_variant(records, *, key_uses_src=False, body_uses_src=False, framed=True):
        """自测用的**偏差实现**（生产代码里没有这些旋钮）。三个自由度各对应一个夹具。"""
        seen, sel = set(), []
        for r in sorted(records, key=lambda r: (int(r["t"]), r["msg"])):
            k = (int(r["t"]), r["src"], r["msg"]) if key_uses_src else (int(r["t"]), r["msg"])
            if k in seen:
                continue
            seen.add(k)
            sel.append(r)
        def body(r):
            return (f"{int(r['t'])}|{r['src']}|{r['msg']}" if body_uses_src
                    else f"{int(r['t'])}|{r['msg']}")
        if framed:
            blob = "".join(f"{len(body(r).encode('utf-8'))}:{body(r)}\n" for r in sel)
        else:
            blob = "".join(body(r) + "\n" for r in sel)
        return hashlib.sha256(blob.encode("utf-8")).hexdigest()

    def dedupe_correct(records):
        seen, sel = set(), []
        for r in records:
            k = (int(r["t"]), r["msg"])
            if k in seen:
                continue
            seen.add(k)
            sel.append(r)
        return sel

    # --- 夹具A：钉「帧里的串怎么组成」。**内部没有**跨源同 (ts,msg) ⇒ 对去重键这个自由度不敏感。
    fx_a = [{"t": 1000, "src": "app", "level": "info", "msg": "A1"},
            {"t": 1001, "src": "core", "level": "info", "msg": "A2"},
            {"t": 1002, "src": "app", "level": "info", "msg": "A3"}]
    fp_a_ok = selection_fingerprint(dedupe_correct(fx_a))
    fp_a_wrong = fp_variant(fx_a, body_uses_src=True)
    check("期望_夹具A_正确（键=(ts,msg)、串=ts|msg、分帧安全）", fp_a_ok, FP["A_correct"])
    check("期望_夹具A_错误（**仅**串改含 src；键与排序不变）", fp_a_wrong, FP["A_wrong_src"])
    check("夹具A 确实**对去重键不敏感**（所以 A 钉的是「串」、B 钉的是「键」）",
          fp_variant(fx_a, key_uses_src=True) == fp_a_ok, True)

    # --- 夹具B：钉「去重键」。**故意**含一对「同 (ts,msg) 不同 src」的重复 ⇒ 换键就换选择集。
    fx_b = [{"t": 2000, "src": "app", "level": "info", "msg": "B-dup"},
            {"t": 2000, "src": "core", "level": "info", "msg": "B-dup"},
            {"t": 2001, "src": "app", "level": "info", "msg": "B-uniq"}]
    fp_b_ok = selection_fingerprint(dedupe_correct(fx_b))
    fp_b_wrong = fp_variant(fx_b, key_uses_src=True)
    check("期望_夹具B_正确（键=(ts,msg)、串=ts|msg、分帧安全）", fp_b_ok, FP["B_correct"])
    check("期望_夹具B_错误（**仅**键改 (ts,src,msg)；串与排序不变）", fp_b_wrong, FP["B_wrong_key"])
    check("夹具B 正确口径下 (ts,msg) 重复被去掉：3 条 → 2 条", len(dedupe_correct(fx_b)), 2)
    check("夹具B 错误口径下不去重：3 条 → 3 条",
          len({(int(r["t"]), r["src"], r["msg"]) for r in fx_b}), 3)

    # --- 夹具C：分帧安全性。**一条含真实 LF 的 message** vs **两条记录**。
    #     ts 必须是 3000/3001，message 里那截必须是 "3001|C2" —— 这样朴素拼接才会**逐字节相同**。
    c_one = {"t": 3000, "src": "app", "level": "info", "msg": "C1\n3001|C2"}
    c_two = [{"t": 3000, "src": "app", "level": "info", "msg": "C1"},
             {"t": 3001, "src": "app", "level": "info", "msg": "C2"}]
    fp_c_one = selection_fingerprint([c_one])
    fp_c_two = selection_fingerprint(c_two)
    fp_c_one_naive = fp_variant([c_one], framed=False)
    fp_c_two_naive = fp_variant(c_two, framed=False)
    check("期望_夹具C_一条（message 含真实 LF）", fp_c_one, FP["C_one"])
    check("期望_夹具C_两条", fp_c_two, FP["C_two"])
    check("反例_夹具C：朴素拼接（非分帧）下「一条 == 两条」← 这正是被分帧挡住的碰撞",
          fp_c_one_naive == fp_c_two_naive, True)
    check("夹具C：分帧后「一条 ≠ 两条」", fp_c_one != fp_c_two, True)
    # 跨实现核对**不需要猜 digest 口径**：把规范化字节串打出来，任何实现都能逐字节比。
    print(f"  夹具C 规范化字节串：一条 = {selection_blob([c_one])!r}")
    print(f"  夹具C 规范化字节串：两条 = {selection_blob(c_two)!r}")
    naive_one = c_one["msg"] + "\n"
    naive_two = c_two[0]["msg"] + "\n" + f"{int(c_two[1]['t'])}|{c_two[1]['msg']}" + "\n"
    print(f"  朴素拼接字节串：一条 = {naive_one!r}")
    print(f"  朴素拼接字节串：两条 = {naive_two!r}；**逐字节相同 = {naive_one == naive_two}**")

    # --- LF 上报口径的端到端检查：JSON 里写的是转义 `\n`，解析后是**真实 LF** ⇒ 计数必须为 1。
    p_lf = os.path.join(tmp, "lf.jsonl")
    with open(p_lf, "w") as f:
        f.write(json.dumps({"ts_unix": 4000, "source": "app", "level": "info",
                            "message": "第一行\n第二行"}) + "\n")
    _, stats_lf = load_records([p_lf])
    check("行内上报：message 含 LF 的记录数（夹具=1）", stats_lf["lf_in_message"], 1)
    check("行内上报：本自测的「正常」人造日志里为 0", stats["lf_in_message"], 0)

    print()
    if fails:
        print(f"self-test：**失败**（{len(fails)} 项）：{fails}")
        return 1
    print("self-test：**全部通过**（去重 / 多对象行 / v6-v4 分类 / 探针三种结局 / 分轮 / 空档 / "
          "LF 上报 / 指纹三夹具六期望值 / 六项敏感性）")
    return 0


# ---------------------------------------------------------------- CLI
def parse_when(s):
    """接受 `HH:MM[:SS]`（今天）或 `YYYY-MM-DD HH:MM[:SS]`（本地时区）。"""
    s = s.strip()
    for f in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"):
        try:
            return datetime.strptime(s, f).timestamp()
        except ValueError:
            pass
    for f in ("%H:%M:%S", "%H:%M"):
        try:
            d = datetime.strptime(s, f)
            return d.replace(year=2026, month=9, day=22).timestamp()
        except ValueError:
            pass
    raise argparse.ArgumentTypeError(f"无法解析时间：{s}")


def main(argv=None):
    ap = argparse.ArgumentParser(description="XrayTun 日志只读指标（task-97 / 探针 / 自愈）",
                                 formatter_class=argparse.RawDescriptionHelpFormatter,
                                 epilog=__doc__.split("用法：")[-1])
    ap.add_argument("--log-dir", default=LOG_DIR_DEFAULT, help="日志目录（默认：macOS 应用日志目录）")
    ap.add_argument("--since", type=parse_when, help="起始时间 HH:MM[:SS] 或 'YYYY-MM-DD HH:MM[:SS]'")
    ap.add_argument("--until", type=parse_when, help="截止时间（推荐固定它，数字才可复现）")
    ap.add_argument("--json", action="store_true", help="以 JSON 输出（便于机器比对；含口径头字段）")
    ap.add_argument("--self-test", action="store_true", help="人造小日志的自测 + 敏感性（只写 /tmp）")
    ap.add_argument("--export", metavar="PATH",
                    help="把「被指纹的那串字节」导出到 PATH（证据子集；可 shasum -a 256 自校验）")
    a = ap.parse_args(argv)

    if a.self_test:
        return self_test()

    paths = [os.path.join(a.log_dir, f) for f in LOG_FILES]
    if not any(os.path.exists(p) for p in paths):
        print(f"✗ 在 {a.log_dir} 找不到日志文件（{', '.join(LOG_FILES)}）", file=sys.stderr)
        return 2
    recs, stats = load_records(paths, a.since, a.until)
    if not recs:
        print("✗ 窗口内没有记录（检查 --since/--until）", file=sys.stderr)
        return 2
    m1, m2, m3 = analyze_task97(recs), analyze_probes(recs), analyze_selfheal(recs)
    fingerprint = selection_fingerprint(recs)
    win = {"since": "全部" if a.since is None else full(a.since),
           "until": "全部" if a.until is None else full(a.until)}
    snap = "；".join(f"{f}={os.path.getsize(os.path.join(a.log_dir,f))}B/mtime{datetime.fromtimestamp(os.path.getmtime(os.path.join(a.log_dir,f))).strftime('%H:%M:%S')}"
                     for f in LOG_FILES if os.path.exists(os.path.join(a.log_dir, f)))
    ctx = {"window": win, "fingerprint": fingerprint, "snap": snap,
           "log_dir": a.log_dir, "n_present": sum(1 for p in paths if os.path.exists(p))}

    if a.export:
        # 证据子集：写出来的**就是**被指纹的那串字节 ⇒ 任何人拿它跑 `shasum -a 256` 就能复算指纹，
        # 不需要 80MB 的原始日志、也不需要本脚本。原始日志**只读**，不被改动。
        if os.path.abspath(a.export).startswith(os.path.abspath(a.log_dir) + os.sep):
            print("✗ 导出路径不能落在日志目录里（会在日志目录里造出非日志文件）", file=sys.stderr)
            return 2
        blob = selection_blob(recs)
        with open(a.export, "w", encoding="utf-8", newline="") as f:
            f.write(blob)
        to = sys.stderr if a.json else sys.stdout
        print(f"证据子集已导出：{a.export}（{len(blob.encode('utf-8'))} 字节；内容 = 被指纹的那串字节）", file=to)
        print(f"  自校验：`shasum -a 256 {a.export}` 的第一个字段应当等于 {fingerprint}", file=to)

    if a.json:
        m2.pop("_rounds", None); m2.pop("_conns", None)
        print(json.dumps({
            "caliber": {
                "version": CALIBER_VERSION,
                "window_since": win["since"], "window_until": win["until"],
                "log_dir": a.log_dir, "log_files_order": list(LOG_FILES),
                "files_present": ctx["n_present"], "snapshot": snap,
                "parse": "raw_decode 循环（一行可含多个 JSON 对象）",
                "dedupe_key": DEDUPE_KEY, "duplicates_dropped": stats["dup"],
                "duplicates_dropped_in_window": stats["dup_in_window"],
                "hits": stats["kept"], "message_with_lf_records": stats["lf_in_message"],
                "selection_fingerprint": f"{FINGERPRINT_ALGO}:{fingerprint}",
                "selection_fingerprint_is_not_file_fingerprint": True,
            },
            "window": f"{win['since']} → {win['until']}；共 {len(recs)} 条记录",
            "snapshot": snap, "stats": stats,
            "task97": m1, "probes": m2, "selfheal": m3}, ensure_ascii=False, indent=2))
    else:
        print_report(stats, m1, m2, m3, ctx)
        print(f"\n快照标识（日志在增长 ⇒ 请连同时点一起引用）：{snap}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
