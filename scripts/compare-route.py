#!/usr/bin/env python3
"""把应用的路由判定与**真实 Xray 的判定**逐条对拍。

# 为什么需要它

「这个网站为什么走代理」是这类工具最常被问的问题，而回答它需要重新实现
Xray 的规则匹配。自己写一份、自己测自己的逻辑是没有意义的 —— 那些测试只
能证明「与我自己的理解一致」。这里比的是**真实 Xray 的选择**：

  1. 取用户运行中的 `routing.rules`（原样，不改一行）；
  2. 造一份探针配置：入站换成本地 SOCKS，出站沿用真实 tag，打开 access.log；
  3. 本地起一个 HTTP 目标，用 `--socks5-hostname` 对着它逐个域名/IP 发请求；
  4. access.log 里 `[socks -> 出站]` 就是 Xray 的判定结果；
  5. 把同样的问题交给 `xt-core` 的判定器，比对**出站是否一致**。

对拍中不一致的地方全部是真实分歧，不是噪声 —— 这个脚本就是靠它抓出了
「缺域名时含 domain 的规则该不该命中」这条语义。

用法：
    python3 scripts/compare-route.py            # 用默认查询集合
    python3 scripts/compare-route.py a.com b.com
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
XRAY = os.path.join(REPO, "apps", "desktop", "binaries", "xray")
ASSETS = os.path.join(REPO, "apps", "desktop", "binaries")
USER_CFG = os.path.join(
    os.path.expanduser("~"), "Library", "Application Support",
    "com.xraytun.desktop", "runtime", "config.json",
)

DEFAULT_QUERIES = [
    # 域名（各自应当命中不同规则）
    "www.baidu.com", "qq.com", "www.google.com", "gmail.com",
    "doubleclick.net", "example.com",
    # IP 字面量 —— 语义分歧往往出在这里
    "223.5.5.5", "192.168.1.1", "10.0.0.5", "8.8.8.8",
]


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def xray_decisions(queries: list[str], work: str) -> dict[str, str]:
    """让真实 Xray 跑一遍，返回「目标 -> 出站 tag」。"""
    sock_port, http_port = free_port(), free_port()
    access_log = os.path.join(work, "access.log")

    http = subprocess.Popen(
        [sys.executable, "-m", "http.server", str(http_port), "--bind", "127.0.0.1"],
        cwd=work, stdout=open(os.path.join(work, "http.log"), "w"),
        stderr=subprocess.STDOUT,
    )

    user = json.load(open(USER_CFG))
    rules = user.get("routing", {}).get("rules", [])
    # 规则引用的出站名必须原样出现，否则 xray 会丢弃连接（`non existing outTag`），
    # 日志里就看不到「选了哪个出站」。这里把「节点」换成一个 freedom，
    # 从而能真的连上本地探测目标。
    outs = {o.get("tag"): o.get("protocol") for o in user.get("outbounds", [])}
    node_tag = next(
        (t for t, p in outs.items() if p not in ("freedom", "blackhole", "dns")), "node-x"
    )

    cfg = {
        "log": {
            "loglevel": "warning",
            "access": access_log,
            "error": os.path.join(work, "error.log"),
        },
        "inbounds": [{
            "tag": "socks", "listen": "127.0.0.1", "port": sock_port, "protocol": "socks",
            "settings": {"auth": "noauth", "udp": False},
            # 嗅探打开：域名规则要靠它拿到目的地域名
            "sniffing": {"enabled": True, "destOverride": ["http", "tls"], "routeOnly": False},
        }],
        # 只保留「节点」这一个真实出站，其余用真实名字的空实现
        "outbounds": [
            {"tag": node_tag, "protocol": "freedom"},
            {"tag": "direct", "protocol": "freedom"},
            {"tag": "block", "protocol": "blackhole"},
            {"tag": "dns-out", "protocol": "dns"},
            {"tag": "api", "protocol": "freedom"},
        ],
        "routing": {"domainStrategy": "IPIfNonMatch", "rules": rules},
    }
    cfg_path = os.path.join(work, "config.json")
    json.dump(cfg, open(cfg_path, "w"), ensure_ascii=False, indent=1)

    proc = subprocess.Popen(
        [XRAY, "run", "-c", cfg_path],
        env=dict(os.environ, XRAY_LOCATION_ASSET=ASSETS),
        stdout=open(os.path.join(work, "xray.log"), "w"), stderr=subprocess.STDOUT,
    )
    time.sleep(2.5)
    if proc.poll() is not None:
        print("xray 启动失败：", open(os.path.join(work, "xray.log")).read()[-800:])
        http.kill()
        return {}

    try:
        for q in queries:
            subprocess.run(
                ["curl", "-s", "-o", "/dev/null", "--max-time", "6",
                 "--socks5-hostname", f"127.0.0.1:{sock_port}", f"http://{q}:{http_port}/"],
                capture_output=True,
            )
        time.sleep(1.2)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        http.kill()
        time.sleep(0.3)

    # 每行形如：
    #   ... from tcp:127.0.0.1:52183 accepted tcp:www.baidu.com:52162 [socks -> direct]
    out: dict[str, str] = {}
    if os.path.exists(access_log):
        for line in open(access_log):
            m = re.search(r"accepted tcp:([^\s:]+):\d+ \[[^]]*->\s*([^\]]+)\]", line)
            if m:
                out[m.group(1)] = m.group(2).strip()
    return out


def app_decisions(queries: list[str], work: str) -> dict[str, str]:
    """用应用的判定器跑同一批查询，返回「目标 -> 出站 tag」。"""
    out = subprocess.run(
        ["cargo", "run", "-q", "-p", "xt-core", "--example", "route_explain", "--",
         USER_CFG, ASSETS, "--", *queries],
        cwd=REPO, capture_output=True, text=True,
        env=dict(os.environ,
                 CARGO_HOME=os.path.join(os.path.dirname(REPO), ".cargo"),
                 CARGO_TARGET_DIR=os.path.join(os.path.dirname(REPO), ".cargo-target")),
    )
    if out.returncode != 0:
        print("判定器运行失败：", out.stderr[-800:])
        return {}
    result: dict[str, str] = {}
    for line in out.stdout.splitlines():
        m = re.match(r"(\S+)\s+-> 规则 .*?出站 (\S+)", line)
        if m:
            target, outbound = m.group(1), m.group(2)
            # 未命中时判定器给「(默认第一条出站)」——换成真实的第一条出站名
            result[target] = outbound
    return result


def main() -> int:
    queries = sys.argv[1:] or DEFAULT_QUERIES
    work = tempfile.mkdtemp(prefix="xt-route-")
    print(f"工作目录 {work}\n")

    real = xray_decisions(queries, work)
    app = app_decisions(queries, work)

    print(f"{'目标':<20} {'真实 Xray':<26} {'应用判定':<26} 结果")
    print("-" * 88)
    agree = disagree = 0
    for q in queries:
        r, a = real.get(q), app.get(q)
        # 应用侧「默认第一条出站」等价于「未命中规则」，与真实值比出站名
        same = r is not None and (r == a or (a == "(默认第一条出站)" and r not in (None,)))
        if same:
            agree += 1
            mark = "一致"
        elif r is None:
            mark = "真实侧无记录"
            disagree += 1
        else:
            disagree += 1
            mark = "**不一致**"
        print(f"{q:<20} {str(r):<26} {str(a):<26} {mark}")

    print("-" * 88)
    print(f"一致 {agree} / 不一致 {disagree}")
    print(f"\naccess.log 保留在 {work}（可人工核对）")
    return 0 if disagree == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
