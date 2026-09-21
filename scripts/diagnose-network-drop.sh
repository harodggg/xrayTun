#!/usr/bin/env bash
#
# XrayTun 断网现场取证（**只读、脱敏、断网也能跑**）
#
# 用法：
#   ./scripts/diagnose-network-drop.sh                  # 输出到 ~/Desktop/xraytun-diag-<时间>.txt
#   ./scripts/diagnose-network-drop.sh --out /tmp/x.txt
#   ./scripts/diagnose-network-drop.sh --minutes 30     # 日志回看窗口（默认 10 分钟）
#   ./scripts/diagnose-network-drop.sh --socks-port 10808 --node 1.2.3.4:443
#   ./scripts/diagnose-network-drop.sh --redact-ip --redact-node
#
# 用户症状（本脚本围绕它取证）：连接状态下整机断网，**一断开/退出就恢复**，
# 且**国外可以、国内直接断掉**。脚本必须能区分下面三态：
#
#   A 整机断网（默认路由进黑洞）：连**绕过隧道**的直连测试也失败
#   B TCP 可达但代理不通：对节点 IP:端口 `nc -z` **成功**，但经本机 SOCKS 发真实请求**失败**
#     —— 这正是「国外可以、国内断掉」最像的机制（TCP 三次握手过、协议被阻断/上游被丢）
#   C 代理正常：经 SOCKS 的真实请求成功
#
# # 三条硬保证
#
# 1. **只读**：只跑读命令（netstat/ifconfig/scutil/networksetup -get…/ps/launchctl list/
#    log show/pmset/dig/curl/nc/ping/traceroute）。**不执行** `networksetup -set*`、
#    `route add/del`、`ifconfig up/down`、`kill`、`launchctl load/unload`，不改任何文件
#    （除了写出这一份报告）。
# 2. **脱敏**：UUID → `<UUID>`；URL → 只留 `scheme://host/<REDACTED>`；
#    password/token/secret/api_key 的值 → `<REDACTED>`；40+ 字符的 base64/blob → `<REDACTED-BLOB>`。
#    **订阅与服务器配置（nodes.json / settings.json / runtime/config.json）只列清单，不读内容**；
#    脚本只从中**提取两个字段**：本机 SOCKS 端口、当前节点的 `address:port`（用于做 TCP 三次握手）。
#    报告默认会包含内网/公网 IP 与那个节点地址；`--redact-ip` / `--redact-node` 可打码。
# 3. **断网可用**：所有联网探测都有短超时、失败不影响后续；不下载任何东西、不依赖网络装包。
#
set -u

OUT=""
MIN=10
REDACT_IP=0
REDACT_NODE=0
SOCKS_ARG=""
NODE_ARG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="${2:-}"; shift 2 ;;
    --minutes) MIN="${2:-10}"; shift 2 ;;
    --socks-port) SOCKS_ARG="${2:-}"; shift 2 ;;
    --node) NODE_ARG="${2:-}"; shift 2 ;;
    --redact-ip) REDACT_IP=1; shift ;;
    --redact-node) REDACT_NODE=1; shift ;;
    -h|--help) sed -n '3,30p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

TS="$(date +%Y%m%d-%H%M%S)"
if [ -n "$OUT" ]; then
  # 用户显式指定了路径：不可写就**明确报错**，不要偷偷换地方 ——
  # 否则他会去约定好的位置找文件，而文件在别处。
  mkdir -p "$(dirname "$OUT")" 2>/dev/null || true
  : > "$OUT" || { echo "无法写入 $OUT" >&2; exit 1; }
else
  # 默认写桌面；桌面不可用时（没有桌面目录 / 权限受限 / 受限运行环境）
  # 依次退回「家目录 → /tmp」。
  #
  # 为什么要退：**报告必须能落地**，否则用户跑完一场空，
  # 而这类问题恰恰发生在「网络已经坏了」的时候，没有重跑的机会。
  #
  # 为什么**不用** `$PWD`：脚本常被从 git 仓库里调用，写进去会污染工作区
  # （实测：退回 `$PWD` 时在仓库根留下了一个 23 KB 的报告文件）。
  # 家目录与 /tmp 都不会污染仓库。
  OUT=""
  for _dir in "$HOME/Desktop" "$HOME" "/tmp"; do
    _cand="$_dir/xraytun-diag-$TS.txt"
    # 用子 shell 收敛「目录不可建」与「文件不可写」的报错 ——
    # 失败是预期分支，不该在用户面前刷错误。
    if ( mkdir -p "$_dir" && : > "$_cand" ) >/dev/null 2>&1; then
      OUT="$_cand"
      break
    fi
  done
  [ -n "$OUT" ] || {
    echo "无法创建报告文件（试过 ~/Desktop、~、/tmp）" >&2
    exit 1
  }
fi

TMP="/tmp/.xraytun-diag.$$"
trap 'rm -f "$TMP" "$TMP.out" 2>/dev/null' EXIT

# ---------------------------------------------------------------------------
# 只读命令执行 + 脱敏
# ---------------------------------------------------------------------------

# run_cap <超时秒> <命令...>：把输出放进 RUN_OUT，退出码放进 RUN_RC。
# 超时用「后台 + 看门狗 kill」实现（macOS 没有 timeout）；kill 只针对本脚本起的子进程。
RUN_OUT=""
RUN_RC=0
run_cap() {
  _t="$1"; shift
  "$@" >"$TMP.out" 2>&1 &
  _pid=$!
  ( sleep "$_t"; kill -TERM "$_pid" 2>/dev/null ) >/dev/null 2>&1 &
  _w=$!
  wait "$_pid" 2>/dev/null
  RUN_RC=$?
  kill "$_w" 2>/dev/null
  wait "$_w" 2>/dev/null
  RUN_OUT="$(cat "$TMP.out" 2>/dev/null)"
  rm -f "$TMP.out"
  [ "$RUN_RC" = "143" ] && RUN_OUT="$RUN_OUT
  …（超时 ${_t}s，已截断）"
  return 0
}

# 脱敏：UUID / URL 路径 / 密钥值 / 超长 blob
sed_redact() {
  sed -E \
    -e 's/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/<UUID>/g' \
    -e 's#(https?://[^/[:space:]]+)(/[^[:space:]]*)?#\1/<REDACTED>#g' \
    -e 's/([Pp][Aa][Ss][Ss][Ww][Oo][Rr][Dd]|[Tt][Oo][Kk][Ee][Nn]|[Ss][Ee][Cc][Rr][Ee][Tt]|[Aa][Pp][Ii][-_]?[Kk][Ee][Yy])([[:space:]]*[:=][[:space:]]*)[^[:space:],;"]*/\1\2<REDACTED>/g' \
    -e 's/("[Ss]hort[_-]?[Ii][Dd]"[[:space:]]*:[[:space:]]*")[^"]*"/\1<REDACTED>"/g' \
    -e 's/[A-Za-z0-9+\/=]{40,}/<REDACTED-BLOB>/g'
}

redact() {
  if [ "$REDACT_IP" = "1" ]; then
    # 保留 127/10/172.16-31/192.168/198.18 与 utun 以便判读，其余 IPv4 打码
    sed_redact | sed -E \
      -e 's/\b(127|10)\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\b/KEEP\1.\2.\3.\4/g' \
      -e 's/\b192\.168\.([0-9]{1,3})\.([0-9]{1,3})\b/KEEP192.168.\1.\2/g' \
      -e 's/\b198\.18\.([0-9]{1,3})\.([0-9]{1,3})\b/KEEP198.18.\1.\2/g' \
      -e 's/\b172\.(1[6-9]|2[0-9]|3[01])\.([0-9]{1,3})\.([0-9]{1,3})\b/KEEP172.\1.\2.\3/g' \
      -e 's/\b((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])\b/<IP>/g' \
      -e 's/KEEP/<KEEP>/g'
  else
    sed_redact
  fi
}

section() {
  {
    echo
    echo "--------------------------------------------------------------------------------"
    echo "===== $1 ====="
    echo "--------------------------------------------------------------------------------"
  } >> "$OUT"
}

# ---------------------------------------------------------------------------
# 从本地配置里**只提取两个字段**（SOCKS 端口 / 节点 address:port），不读别的
# ---------------------------------------------------------------------------
CFGDIR="$HOME/Library/Application Support/com.xraytun.desktop"
RUNTIME_CFG="$CFGDIR/runtime/config.json"
SETTINGS="$CFGDIR/settings.json"

SOCKS_PORT=""
NODE_TARGET=""
SOCKS_SRC="未检测到"
NODE_SRC="未检测到"

if command -v python3 >/dev/null 2>&1 && [ -f "$RUNTIME_CFG" ]; then
  pair="$(python3 - "$RUNTIME_CFG" <<'PY' 2>/dev/null
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print(""); print(""); raise SystemExit
socks = ""
for i in (d.get("inbounds") or []):
    if str(i.get("protocol", "")).lower() == "socks":
        socks = str(i.get("port") or "")
        break
node = ""
for ob in (d.get("outbounds") or []):
    v = (ob.get("settings") or {}).get("vnext") or []
    if v and v[0].get("address"):
        node = "%s:%s" % (v[0]["address"], v[0].get("port") or "")
        break
print(socks)
print(node)
PY
)"
  SOCKS_PORT="$(printf '%s\n' "$pair" | sed -n 1p | tr -d '[:space:]')"
  NODE_TARGET="$(printf '%s\n' "$pair" | sed -n 2p | tr -d '[:space:]')"
  [ -n "$SOCKS_PORT" ] && SOCKS_SRC="runtime/config.json（只取 port 字段）"
  [ -n "$NODE_TARGET" ] && NODE_SRC="runtime/config.json（只取 address:port 字段）"
fi
# grep 兜底（无 python3 时）
if [ -z "$SOCKS_PORT" ] && [ -f "$SETTINGS" ]; then
  SOCKS_PORT="$(sed -n 's/.*"socks_port"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$SETTINGS" | head -1)"
  [ -n "$SOCKS_PORT" ] && SOCKS_SRC="settings.json（只取 socks_port 字段）"
fi
[ -n "$SOCKS_ARG" ] && { SOCKS_PORT="$SOCKS_ARG"; SOCKS_SRC="命令行 --socks-port"; }
[ -n "$NODE_ARG" ] && { NODE_TARGET="$NODE_ARG"; NODE_SRC="命令行 --node"; }

NODE_SHOWN="$NODE_TARGET"
[ "$REDACT_NODE" = "1" ] && NODE_SHOWN="<NODE>"
[ -n "$NODE_TARGET" ] || NODE_SHOWN="（未检测到，可用 --node host:port 指定）"

# ---------------------------------------------------------------------------
# 头部
# ---------------------------------------------------------------------------
{
  echo "XrayTun 断网现场取证报告"
  echo "生成时间：$(date '+%Y-%m-%d %H:%M:%S %z')"
  echo "采样窗口：最近 ${MIN} 分钟日志"
  echo
  echo "脱敏：UUID / URL 路径 / token 类字段 / 超长 blob 已替换；日志尾部已脱敏；"
  echo "      订阅与服务器配置只列清单不读内容（仅提取 SOCKS 端口与节点 address:port）"
  if [ "$REDACT_IP" = "1" ]; then echo "IP：已打码（保留 127/10/172.16-31/192.168/198.18）"; else echo "IP：未打码（路由/DNS 判读需要；可加 --redact-ip）"; fi
  echo "节点地址：${NODE_SHOWN}（来源：${NODE_SRC}）"
  echo "本机 SOCKS 端口：${SOCKS_PORT:-未检测到}（来源：${SOCKS_SRC}）"
  echo
  echo "本脚本只读：不修改任何网络配置（见脚本头部说明）"
  echo
  echo "【怎么读这份报告】先看第 8 节的「三态判定」与第 9 节的自动判读，"
  echo "  再看第 3 节路由（0.0.0.0/1 + 128.0.0.0/1 是否被 utun 接管）与第 5 节 helper 状态。"
} >> "$OUT"

# ---------------------------------------------------------------------------
# 1) 系统 / 电源
# ---------------------------------------------------------------------------
section "1. 系统 / 电源 / 时间"
{
  echo "-- sw_vers"; sw_vers 2>&1
  echo; echo "-- uname"; uname -a 2>&1
  echo; echo "-- uptime"; uptime 2>&1
  echo; echo "-- pmset -g（前 30 行）"; pmset -g 2>&1 | head -30
  echo; echo "-- pmset -g assertions（前 30 行，唤醒/睡眠相关）"; pmset -g assertions 2>&1 | head -30
  echo; echo "-- IP 转发（0 = 正常）"; sysctl -n net.inet.ip.forwarding 2>&1
  echo; echo "-- 睡眠/唤醒记录（最近 20 条，看是不是掉在唤醒之后）"; pmset -g log 2>/dev/null | grep -iE "wake|sleep" | tail -20
} | redact >> "$OUT"

# ---------------------------------------------------------------------------
# 2) 网卡
# ---------------------------------------------------------------------------
section "2. 网卡与地址（ifconfig -a）"
run_cap 15 ifconfig -a
printf '%s\n' "$RUN_OUT" | redact >> "$OUT"

PHYS_IF="$(networksetup -listallhardwareports 2>/dev/null | awk '/Device: en/{print $2; exit}')"
[ -n "${PHYS_IF:-}" ] || PHYS_IF="en0"
section "2b. 物理端口与 Wi-Fi 关联"
{
  echo "-- 硬件端口"; networksetup -listallhardwareports 2>&1
  echo; echo "-- Wi-Fi 关联（${PHYS_IF}）"; networksetup -getairportnetwork "$PHYS_IF" 2>&1
  echo; echo "-- ${PHYS_IF} 的 IPv4"; ipconfig getifaddr "$PHYS_IF" 2>&1
  echo; echo "-- utun 接口与地址"
  ifconfig -a 2>/dev/null | awk '/^utun[0-9]+:/{iface=$1} /inet /{if (iface != "") {print iface, $2; iface=""}}'
} | redact >> "$OUT"

# ---------------------------------------------------------------------------
# 3) 路由
# ---------------------------------------------------------------------------
section "3. 路由表（netstat -rn）"
run_cap 15 netstat -rn
printf '%s\n' "$RUN_OUT" | redact >> "$OUT"

section "3b. 关键前缀（TUN 是否接管默认路由）"
{
  echo "-- 默认路由条目"
  netstat -rn -f inet 2>/dev/null | awk '$1=="default"{print}' | redact
  echo; echo "-- route -n get default"; route -n get default 2>&1 | redact
  echo; echo "-- route -n get 1.1.1.1（真实出口判定）"; route -n get 1.1.1.1 2>&1 | redact
  echo; echo "-- 0.0.0.0/1"; run_cap 10 netstat -rn -f inet; printf '%s\n' "$RUN_OUT" | awk '$1=="0.0.0.0/1"||$1=="0/1"{print}' | redact
  echo; echo "-- 128.0.0.0/1"; run_cap 10 netstat -rn -f inet; printf '%s\n' "$RUN_OUT" | awk '$1=="128.0.0.0/1"||$1=="128/1"{print}' | redact
  echo; echo "-- 198.18.0.0/15（fake-dns 哨兵网段）"; run_cap 10 netstat -rn -f inet; printf '%s\n' "$RUN_OUT" | grep -E '198\.18\.' | redact
} >> "$OUT"

# ---------------------------------------------------------------------------
# 4) DNS
# ---------------------------------------------------------------------------
section "4. DNS（scutil --dns）"
run_cap 15 scutil --dns
printf '%s\n' "$RUN_OUT" | redact >> "$OUT"
section "4b. /etc/resolv.conf 与各服务 DNS"
{
  echo "-- /etc/resolv.conf"; cat /etc/resolv.conf 2>&1
  echo; echo "-- 各网络服务的 DNS"
  networksetup -listallnetworkservices 2>/dev/null | tail -n +2 | while IFS= read -r svc; do
    [ -n "$svc" ] || continue
    printf '  %-30s ' "$svc"
    networksetup -getdnsservers "$svc" 2>/dev/null | tr '\n' ' '
    echo
  done
} | redact >> "$OUT"
SENTINEL_DNS="$(scutil --dns 2>/dev/null | grep -c '198\.18\.')"

# ---------------------------------------------------------------------------
# 5) helper
# ---------------------------------------------------------------------------
section "5. 特权 helper"
{
  echo "-- 进程"; pgrep -fl "xraytun-helper" 2>/dev/null || echo "  （没有 xraytun-helper 进程）"
  echo; echo "-- launchctl list 里的 xraytun 条目"; launchctl list 2>/dev/null | grep -i xraytun || echo "  （无）"
  echo; echo "-- 二进制 / plist / socket"
  ls -l /Library/PrivilegedHelperTools/com.xraytun.helper 2>&1 || true
  ls -l /Library/LaunchDaemons/com.xraytun.helper.plist 2>&1 || true
  ls -l /var/run/com.xraytun.helper.sock 2>&1 || true
  echo; echo "-- launchctl print（普通用户多半是权限错误，属正常）"
  launchctl print system/com.xraytun.helper 2>&1 | head -25
} | redact >> "$OUT"

# ---------------------------------------------------------------------------
# 6) 应用与核心
# ---------------------------------------------------------------------------
section "6. 应用与核心进程 / 数据清单"
{
  echo "-- App 进程"; pgrep -fl "xraytun-desktop|XrayTun.app" 2>/dev/null || echo "  （App 没在跑）"
  echo; echo "-- 核心进程"; pgrep -fl "/xray( |$)|xray-core" 2>/dev/null || echo "  （没有 xray 核心进程）"
  echo; echo "-- 数据目录（只列清单）"; ls -l "$CFGDIR" 2>&1
  echo; echo "-- runtime 目录（只列清单）"; ls -l "$CFGDIR/runtime" 2>&1
  echo; echo "-- 配置/订阅文件：**只列文件名与大小，不读内容**"
  for f in nodes.json settings.json runtime/config.json; do
    p="$CFGDIR/$f"
    [ -f "$p" ] && printf '  %-22s %8s bytes  mtime=%s\n' "$f" "$(stat -f%z "$p" 2>/dev/null)" "$(stat -f%Sm "$p" 2>/dev/null)"
  done
  echo; echo "-- 遗留会话快照（helper 的回滚快照目录，如存在只列清单）"
  ls -l /var/run/com.xraytun.helper.sock 2>/dev/null
  ls -l /Library/Application\ Support/com.xraytun.helper 2>/dev/null || true
} >> "$OUT"

# ---------------------------------------------------------------------------
# 7) 日志尾部
# ---------------------------------------------------------------------------
section "7. 日志尾部（已脱敏）"
for d in "$HOME/Library/Logs/XrayTun" "$CFGDIR/logs"; do
  [ -d "$d" ] || continue
  find "$d" -maxdepth 2 -type f -name '*.log' 2>/dev/null | while IFS= read -r f; do
    echo "-- ${f}（尾部 200 行）"
    tail -n 200 "$f" 2>/dev/null | redact
    echo
  done
done >> "$OUT"
section "7b. 统一日志（最近 ${MIN} 分钟）"
run_cap 30 log show --last "${MIN}m" --style compact \
  --predicate 'process == "xraytun-desktop" OR process == "xraytun-helper" OR process == "xray"' 2>&1
printf '%s\n' "$RUN_OUT" | tail -n 400 | redact >> "$OUT"

# ---------------------------------------------------------------------------
# 8) 三态对照（本卡的核心）
# ---------------------------------------------------------------------------
section "8. 三态判定所需的两组对照 + 直连对照"
{
  echo "设计："
  echo "  · 直连对照  ：curl --interface ${PHYS_IF}（绕过隧道与代理，走物理出口）"
  echo "  · 默认路由  ：curl 不加参数（TUN 接管时走隧道）"
  echo "  · TCP 三次握手：nc -z 到**节点服务器** ${NODE_SHOWN}（不经代理，只测 TCP 通不通）"
  echo "  · 经 SOCKS ：curl --socks5-hostname 127.0.0.1:${SOCKS_PORT:-?}（真实请求，走代理）"
  echo "  判读：TCP 通 + SOCKS 失败 = B 态（TCP 可达但代理不通，最像「国外可以、国内断掉」）；"
  echo "        直连也失败 = A 态（整机断网）；两者都通 = C 态（代理正常）。"
  echo
} >> "$OUT"

# --- 8.1 本地 SOCKS 是否在监听 ---
SOCKS_LISTEN="no"
if [ -n "$SOCKS_PORT" ]; then
  run_cap 6 nc -z -G 3 -w 3 127.0.0.1 "$SOCKS_PORT"
  [ "$RUN_RC" = "0" ] && SOCKS_LISTEN="yes"
fi
{
  echo "-- 8.1 本机 SOCKS 监听"
  if [ -z "$SOCKS_PORT" ]; then echo "  （未检测到端口，跳过；可用 --socks-port 指定）"
  else printf '  nc -z 127.0.0.1:%s → %s\n' "$SOCKS_PORT" "$SOCKS_LISTEN"; fi
  echo
} >> "$OUT"

# --- 8.2 直连 vs 默认路由 ---
curl_probe() { # 标签 附加参数...
  _label="$1"; shift
  run_cap 10 curl -sS -o /dev/null --max-time 7 \
    -w 'http=%{http_code} connect=%{time_connect}s total=%{time_total}s' "$@"
  _rc="$RUN_RC"
  _msg="$(printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-160)"
  printf '  %-52s rc=%-4s %s\n' "$_label" "$_rc" "$_msg"
}

CODE_DIRECT="$( { curl_probe "直连(${PHYS_IF}) → https://1.1.1.1/" --interface "$PHYS_IF" https://1.1.1.1/; } )"
CODE_DEFAULT="$( { curl_probe "默认路由 → https://1.1.1.1/" https://1.1.1.1/; } )"
{
  echo "-- 8.2 HTTP 探测（rc 是 curl 退出码；0=成功。35=TLS 握手失败，52/56=连接被重置/空响应，28=超时，7=连不上）"
  echo "$CODE_DIRECT"
  echo "$CODE_DEFAULT"
  curl_probe "默认路由 → https://www.baidu.com/（国内站）" https://www.baidu.com/
  curl_probe "直连(${PHYS_IF}) → https://www.baidu.com/" --interface "$PHYS_IF" https://www.baidu.com/
  curl_probe "默认路由 → https://xraytun.top/（官网）" https://xraytun.top/
  echo
} >> "$OUT"

# --- 8.3 节点 TCP 三次握手 ---
NODE_TCP="skip"
if [ -n "$NODE_TARGET" ]; then
  HOST="${NODE_TARGET%%:*}"; PORT="${NODE_TARGET##*:}"
  run_cap 8 nc -z -G 4 -w 4 "$HOST" "$PORT"; RC_TCP="$RUN_RC"
  run_cap 8 ping -c 2 -t 3 "$HOST"; PING_OUT="$(printf '%s' "$RUN_OUT" | tail -2 | tr '\n' ' ')"
  [ "$RC_TCP" = "0" ] && NODE_TCP="yes" || NODE_TCP="no"
  {
    echo "-- 8.3 节点 TCP 三次握手（不经代理）"
    printf '  nc -z %s → rc=%s（%s）\n' "$NODE_SHOWN" "$RC_TCP" "$NODE_TCP"
    printf '  ping 同一主机：%s\n' "$PING_OUT"
    echo
  } >> "$OUT"
else
  echo "-- 8.3 节点 TCP：未检测到节点地址（用 --node host:port 指定）" >> "$OUT"
  echo >> "$OUT"
fi

# --- 8.4 经本机 SOCKS 的真实请求（同一目标分别记录）---
SOCKS_OK="no"
{
  echo "-- 8.4 经本机 SOCKS5 的真实请求（curl --socks5-hostname）"
  if [ "$SOCKS_LISTEN" != "yes" ]; then
    echo "  本机 SOCKS 未监听（或未检测到端口）→ 无法测「经代理」这一路；"
    echo "  这一条通常说明 App/核心没在跑，或端口不是 ${SOCKS_PORT:-?}。"
  else
    curl_probe "SOCKS → https://www.google.com/generate_204（国外）" \
      --socks5-hostname "127.0.0.1:$SOCKS_PORT" https://www.google.com/generate_204
    curl_probe "SOCKS → https://www.baidu.com/（国内）" \
      --socks5-hostname "127.0.0.1:$SOCKS_PORT" https://www.baidu.com/
    curl_probe "SOCKS → https://xraytun.top/（官网）" \
      --socks5-hostname "127.0.0.1:$SOCKS_PORT" https://xraytun.top/
  fi
  echo
} >> "$OUT"

# 用一次真实请求判定 SOCKS_OK（http 2xx/204 即算通）
if [ "$SOCKS_LISTEN" = "yes" ]; then
  run_cap 10 curl -sS -o /dev/null --max-time 8 -w '%{http_code}' \
    --socks5-hostname "127.0.0.1:$SOCKS_PORT" https://www.google.com/generate_204
  case "$RUN_OUT" in 2*|204) SOCKS_OK="yes" ;; esac
fi

# --- 8.5 DNS 对照 ---
{
  echo "-- 8.5 DNS 对照"
  printf '  %-44s %s\n' "系统解析器 dig xraytun.top" "$(run_cap 8 dig +short +time=3 +tries=1 xraytun.top; printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-100)"
  printf '  %-44s %s\n' "系统解析器 dig www.baidu.com" "$(run_cap 8 dig +short +time=3 +tries=1 www.baidu.com; printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-100)"
  printf '  %-44s %s\n' "指定 223.5.5.5 dig xraytun.top" "$(run_cap 8 dig +short @223.5.5.5 +time=3 +tries=1 xraytun.top; printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-100)"
  printf '  %-44s %s\n' "指定 1.1.1.1 dig xraytun.top" "$(run_cap 8 dig +short @1.1.1.1 +time=3 +tries=1 xraytun.top; printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-100)"
  echo "  解析器里出现 198.18.x 的行数：${SENTINEL_DNS}（>0 说明系统 DNS 仍指向隧道内哨兵）"
  echo
} | redact >> "$OUT"

# --- 8.6 traceroute（参考）---
{
  echo "-- 8.6 traceroute 到 1.1.1.1（最多 5 跳，仅参考）"
  run_cap 20 traceroute -n -m 5 -w 1 1.1.1.1 2>&1 | head -8
} >> "$OUT"

# ---------------------------------------------------------------------------
# 9) 自动判读
# ---------------------------------------------------------------------------
section "9. 自动判读（基于上面事实的**推断**，不是结论）"
{
  UTUNS="$(ifconfig -a 2>/dev/null | awk '/^utun[0-9]+:/{gsub(":","",$1); print $1}' | tr '\n' ' ')"
  UTUN_UP=""
  for u in $UTUNS; do ifconfig "$u" 2>/dev/null | grep -q 'inet ' && UTUN_UP="$UTUN_UP $u"; done
  DEF_IF="$(route -n get default 2>/dev/null | awk '/interface:/{print $2}')"
  SPLIT1="$(netstat -rn -f inet 2>/dev/null | awk '$1=="0.0.0.0/1"||$1=="0/1"{c++} END{print c+0}')"
  SPLIT2="$(netstat -rn -f inet 2>/dev/null | awk '$1=="128.0.0.0/1"||$1=="128/1"{c++} END{print c+0}')"
  DNS1="$(scutil --dns 2>/dev/null | awk '/nameserver\[0\]/{print $3; exit}')"
  PHYS_IP="$(ipconfig getifaddr "$PHYS_IF" 2>/dev/null)"

  echo "物理接口：${PHYS_IF} 地址=${PHYS_IP:-（无）}"
  echo "utun 接口：${UTUNS:-（无）}；其中有地址的：${UTUN_UP:-（无）}"
  echo "默认路由出口：${DEF_IF:-（无）}  0.0.0.0/1 条目=${SPLIT1}  128.0.0.0/1 条目=${SPLIT2}"
  echo "系统首选 DNS：${DNS1:-（无）}；指向 198.18.x 的行数=${SENTINEL_DNS}"
  echo "helper：进程=$(pgrep -f xraytun-helper >/dev/null 2>&1 && echo 在 || echo 不在) socket=$([ -S /var/run/com.xraytun.helper.sock ] && echo 在 || echo 不在)"
  echo "App=$(pgrep -f xraytun-desktop >/dev/null 2>&1 && echo 在 || echo 不在) 核心=$(pgrep -f '/xray( |$)' >/dev/null 2>&1 && echo 在 || echo 不在)"
  echo
  echo "本机 SOCKS 监听=${SOCKS_LISTEN}  节点 TCP 握手=${NODE_TCP}  经 SOCKS 真实请求成功=${SOCKS_OK}"
  echo
  echo "【按用户症状（连着就断、断开即恢复、国外可以国内断）逐条对照】"
  if [ "$NODE_TCP" = "yes" ] && [ "$SOCKS_OK" = "no" ]; then
    echo "  → 最像 **B 态：TCP 三次握手成功，但经代理的真实请求失败** ——"
    echo "     物理链路与「到节点端口」的 TCP 是通的，问题在 TLS/协议层或代理出口（可能被干扰/重置）。"
  elif [ "$NODE_TCP" = "no" ] && [ "$NODE_TCP" != "skip" ]; then
    echo "  → 连**节点端口的 TCP 三次握手都失败**：不像「协议被单独阻断」，更像链路/路由/节点不可达。"
  fi
  if [ "$SPLIT1" -gt 0 ] && [ "$SPLIT2" -gt 0 ] && [ -z "$UTUN_UP" ]; then
    echo "  → **拆分默认路由已存在但 utun 没有地址**：流量会被送进黑洞（H1）。"
  fi
  if [ "$SENTINEL_DNS" -gt 0 ]; then
    echo "  → 系统解析器里仍有 198.18.x 哨兵：**连接期间域名解析会走隧道内 DNS**；"
    echo "     若隧道坏掉，断开/退出前连域名都解析不了（H4）。"
  fi
  echo
  echo "其它假设（自行对照上面的原始输出）："
  echo "  H1 隧道占用默认路由但 utun 无地址/核心已死 → 整机流量黑洞（典型「突然全网断」）"
  echo "  H2 直连可用、默认路由不可用 → 问题在隧道/代理/核心，物理链路正常"
  echo "  H3 直连与默认都不可用且物理接口无地址 → 物理链路/路由被清空"
  echo "  H4 只有 DNS 失败（IP 能通、域名不通）→ 解析器仍指向失效的隧道内哨兵"
  echo "  H5 helper 不在 / socket 不在 → 断开时的回滚可能没执行，路由/DNS 可能残留"
  echo "  H6 国内站不通、国外站通 → 分流/规则或上游链路问题（与「整机断」不同）"
} >> "$OUT"

echo
echo "✓ 取证完成：$OUT"
echo "  把它发给维护者即可。报告默认含 IP 与节点地址；如需打码：--redact-ip --redact-node 重跑。"
