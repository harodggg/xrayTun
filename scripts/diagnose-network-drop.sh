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
#   ./scripts/diagnose-network-drop.sh --self-test          # **离线自检**：只跑判读逻辑（不联网、不写报告）
#
# **为什么必须「在故障当下」跑这份取证**：用户的症状是**间歇性**的 ——
#   有时连不上 deepseek、有时连不上本机 127.0.0.1:3080、国内不通而国外良好；
#   维护者**事后复现不到**（事后跑，国内国外全通）。故障当下那一刻的路由表、活动 DNS 解析器、
#   utun 数量与代理旁路表才是证据；晚一步，现场可能已被自动重连/回滚抹掉。
#   ⇒ **感觉不对就立刻跑一次**并留下整份报告，而不是等「确定坏了」再跑。
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

# ---------------------------------------------------------------------------
# 判读逻辑（**纯函数，可离线自检**）：回环路由形态
# ---------------------------------------------------------------------------
# 输入：`netstat -rn -f inet` 的文本（来自本机，或 --self-test 里的现场样例）。
# 判据是**确定性的**：BSD 路由表里 127.0.0.0/8 的入口必须落在 lo0。
# 若 127/8 指向某个网关（现场出现的是局域网路由器）而 netif 又不是 lo0，
# 则**除 127.0.0.1 自带 /32 → lo0 之外**，其它 127.x.x.x 都会被送到那个网关 —— 回环局部失效。
judge_loopback_routes() {
  awk '
    /^Destination/ { next }
    NF < 4 { next }
    {
      dest=$1; gw=$2; flags=$3; netif=$4
      if (dest=="127" || dest=="127/8" || dest=="127.0.0.0/8") {
        if (netif=="lo0" && gw ~ /^127\./) {
          printf "  [正常] 127/8 的回环路由：dest=%s gateway=%s flags=%s netif=%s\n", dest, gw, flags, netif
        } else {
          printf "  [异常] 127/8 的回环路由指向**非 lo0** 的出口：dest=%s gateway=%s flags=%s netif=%s\n", dest, gw, flags, netif
          printf "         ⇒ 除 127.0.0.1（自带 /32 → lo0）以外，其它 127.x.x.x 会被送到 %s。\n", gw
          bad++
        }
      }
      if (dest=="127.0.0.1" || dest=="127.0.0.1/32") {
        if (netif=="lo0") { onlo++ }
        else { printf "  [异常] 127.0.0.1 的 /32 主机路由不在 lo0：netif=%s flags=%s\n", netif, flags; bad++ }
      }
    }
    END {
      if (onlo==0) { printf "  [异常] 路由表里没有「127.0.0.1 → lo0」的主机路由：回环本身可能不可用\n"; bad++ }
      if (bad==0) print "  [正常] 回环路由形态健康（127.0.0.1/32 → lo0，且没有 127/8 via 网关）"
      exit (bad>0 ? 1 : 0)
    }
  '
}

# 异常收集（§10 会汇总；让读者一眼分出「现在正常」与「发现异常」）
ANOMALIES=""
note_anomaly() { ANOMALIES="${ANOMALIES}  · $1
"; }
anomaly_count() { [ -n "$ANOMALIES" ] && printf '%s' "$ANOMALIES" | grep -c '  · ' || echo 0; }

# ---------------------------------------------------------------------------
# --self-test：用**现场样例**验证判据真的能抓到那条异常（离线、不联网、不写报告）
# ---------------------------------------------------------------------------
run_self_test() {
  local rc=0
  local fixture_bad fixture_good
  # ↓↓ 逐字来自 lead 在用户机器上抓到的现场（task-81 卡面）。
  #    这两行是 `netstat -rn -f inet` 的输出行本身；
  #    卡面里跟在后面的「← ⚠️ 127.0.0.0/8 被静态路由指到局域网路由器」是**注解**，不是命令输出，故不录入。
  fixture_bad='127        192.168.0.1   UGSc   en0
127.0.0.1  127.0.0.1     UH     lo0'
  # ↓↓ 健全形态（卡面要求：127.0.0.1 走 lo0，且**没有** 127/8 via 网关）
  fixture_good='127.0.0.1  127.0.0.1  UH  lo0'

  echo "=== --self-test 1/2：lead 现场样例（**必须**报「异常」）==="
  printf '%s\n' "$fixture_bad"
  echo "---- 判读 ----"
  printf '%s\n' "$fixture_bad" | judge_loopback_routes
  local rc_bad=$?
  echo "判读退出码=${rc_bad}（0=正常，1=发现异常）"
  if [ "$rc_bad" = "1" ]; then echo "✓ 符合预期：抓到「127 路由指向非 lo0 网关 ⇒ 异常」"; else echo "✗ 不符合预期：竟然判成正常"; rc=1; fi

  echo
  echo "=== --self-test 2/2：健全形态（**必须**报「正常」）==="
  printf '%s\n' "$fixture_good"
  echo "---- 判读 ----"
  printf '%s\n' "$fixture_good" | judge_loopback_routes
  local rc_good=$?
  echo "判读退出码=${rc_good}（0=正常，1=发现异常）"
  if [ "$rc_good" = "0" ]; then echo "✓ 符合预期：健全形态判成正常"; else echo "✗ 不符合预期：把健全形态判成了异常"; rc=1; fi

  echo
  if [ "$rc" = "0" ]; then echo "self-test：双向敏感性**通过**（异常样例报异常 / 健全样例报正常）"; else echo "self-test：**失败**"; fi
  return "$rc"
}

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
    --self-test) run_self_test; exit $? ;;
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
# 2c) 回环本身（不只是「SOCKS 端口在不在监听」）
# ---------------------------------------------------------------------------
section "2c. 回环可用性（127.0.0.1 **以及其它 127.x.x.x**）"
{
  echo "为什么要测「其它 127.x.x.x」：现场出现过一条 127 → 192.168.0.1 UGSc en0 的静态路由，"
  echo "只有 127.0.0.1 自带 /32 → lo0 才侥幸可用 ⇒ **只测 127.0.0.1 会漏掉这个故障**。"
  echo "**归属（有代码证据，不是按进程名猜的）**：这条路由是 **XrayTun 自己装的** ——"
  echo "  修复前 crates/xt-proto/src/lib.rs 的 default_bypass_networks() 里含 \"127.0.0.0/8\"（与连接态实读的旁路集合逐条吻合），"
  echo "  而 crates/xt-tun/src/plan.rs 把**所有 IPv4 旁路网段一律指向物理网关**；"
  echo "  修复 63b84dc（task-83）已把 127/8 移出该列表，并在规划层加防御。"
  echo "  ⇒ 机器上仍看到它，最可能是**已安装的 App 还是旧版本**（新代码尚未上机）。"
  echo
  for A in 127.0.0.1 127.0.0.2; do
    echo "-- ${A}"
    run_cap 4 ping -c 1 -t 1 "$A"
    printf '%s\n' "$RUN_OUT" | head -2 | sed 's/^/    ping: /'
    if [ -n "$SOCKS_PORT" ]; then
      run_cap 4 nc -z -G 2 -w 2 "$A" "$SOCKS_PORT"
      printf '    nc -z %s:%s → rc=%s\n' "$A" "$SOCKS_PORT" "$RUN_RC"
      run_cap 6 curl -s -o /dev/null --max-time 4 -w 'http=%{http_code} connect=%{time_connect}s total=%{time_total}s' "http://${A}:${SOCKS_PORT}/"
      printf '    curl http://%s:%s/ → %s\n' "$A" "$SOCKS_PORT" "$(printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-120)"
    else
      echo "    （未检测到 SOCKS 端口，跳过 nc/curl 那两行；可用 --socks-port 指定）"
    fi
  done
  # 判读：127.0.0.1 通而 127.0.0.2 不通 ⇒ 回环是**局部**坏的
  run_cap 4 nc -z -G 2 -w 2 127.0.0.1 "${SOCKS_PORT:-1}"
  RC_LO1=$RUN_RC
  run_cap 4 nc -z -G 2 -w 2 127.0.0.2 "${SOCKS_PORT:-1}"
  RC_LO2=$RUN_RC
  echo
  echo "-- 判读"
  if [ "$RC_LO1" = "0" ] && [ "$RC_LO2" != "0" ]; then
    echo "    [异常] 127.0.0.1 可达但 127.0.0.2 不可达 ⇒ **回环局部失效**（与 §3c 的路由形态一起看）"
    note_anomaly "回环局部失效：127.0.0.1 通、127.0.0.2 不通（§2c）"
  elif [ "$RC_LO1" = "0" ] && [ "$RC_LO2" = "0" ]; then
    echo "    [正常] 127.0.0.1 与 127.0.0.2 都可连"
  else
    echo "    （没有可比对的监听端口，或两个都不通 —— 结合 §3c 的路由判读看）"
  fi
} >> "$OUT"

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
# 3c) 回环路由形态的判读（**确定性判据**，含异常判定）
# ---------------------------------------------------------------------------
section "3c. 回环路由形态的判读（127/8 必须落在 lo0）"
{
  echo "-- netstat -rn -f inet 的相关原始行（^127 / ^0/1 / ^128/1 / ^198.18 / ^default）"
  run_cap 15 netstat -rn -f inet
  printf '%s\n' "$RUN_OUT" | awk '$1 ~ /^127/ || $1=="0/1" || $1=="0.0.0.0/1" || $1=="128/1" || $1=="128.0.0.0/1" || $1 ~ /^198\.18/ || $1=="default" {print}' | redact | sed 's/^/    /'
  echo
  echo "-- 判读（输入是上面同一份 netstat 文本；判据：127/8 必须 lo0）"
  JUDGE_OUT="$(printf '%s\n' "$RUN_OUT" | judge_loopback_routes)"
  JUDGE_RC=$?
  printf '%s\n' "$JUDGE_OUT"
  if [ "$JUDGE_RC" != "0" ]; then
    echo "  归属：这条 127/8 → 物理网关的路由由 **XrayTun 的连接态规划**安装（修复前 127/8 在 bypass 列表里，"
    echo "        且所有 IPv4 旁路网段一律指向物理网关）；修复 63b84dc（task-83）已移出该网段并加规划层防御。"
    echo "        ⇒ 若机器上仍见它，多为「已安装 App 仍是旧版本」，不是第三方 VPN 的锅。"
    note_anomaly "回环路由形态异常：127/8 未落在 lo0（§3c）"
  fi
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
# 6b) 多个 VPN / 代理栈的共存证据（**只报名字，不 dump 配置**）
# ---------------------------------------------------------------------------
section "6b. 多 VPN / 代理栈共存证据"
{
  echo "-- 所有 utun* 接口与 MTU（现场曾有 utun0..utun6 共 7 个）"
  UTUN_ALL="$(ifconfig -a 2>/dev/null | awk -F: '/^utun[0-9]+:/{print $1}')"
  if [ -z "$UTUN_ALL" ]; then
    echo "    （没有 utun 接口）"
  else
    for u in $UTUN_ALL; do
      M="$(ifconfig "$u" 2>/dev/null | awk '/mtu/{for(i=1;i<=NF;i++) if($i=="mtu") print $(i+1)}')"
      H="$(ifconfig "$u" 2>/dev/null | awk '/inet /{print $2; exit}')"
      printf '    %-8s mtu=%-6s inet=%s\n' "$u" "${M:-?}" "${H:-（无地址）}"
    done
    printf '    合计：%s 个 utun 接口\n' "$(printf '%s\n' $UTUN_ALL | grep -c .)"
  fi
  echo; echo "-- 第三方代理/VPN/隧道进程（**只列进程名**，不看命令行参数）"
  pgrep -l -f 'Karing|Tailscale|tailscaled|clash|ClashX|mihomo|sing-box|Surge|Quantumult|Stash|v2ray|V2Ray|WireGuard|wg-quick|OpenVPN|tun2socks' 2>/dev/null | awk '{print "    "$2}' | sort -u
  echo "    （空 = 没发现这些进程名；不代表没有别的栈）"
  echo; echo "-- scutil --proxy（系统代理设置）"
  scutil --proxy 2>&1 | redact | sed 's/^/    /'
  echo; echo "-- 每个网络服务的代理旁路表（networksetup -getproxybypassdomains）"
  networksetup -listallnetworkservices 2>/dev/null | tail -n +2 | while IFS= read -r svc; do
    echo "  [${svc}]"
    networksetup -getproxybypassdomains "$svc" 2>&1 | sed 's/^/    /'
  done
  echo
  UTUN_N="$(printf '%s\n' $UTUN_ALL | grep -c .)"
  echo "-- 判读"
  if [ "$UTUN_N" -ge 3 ]; then
    echo "    [可疑] utun 接口 ${UTUN_N} 个（≥3）⇒ **多栈共存**；只能提示可疑，不能据此断言是故障原因。"
    note_anomaly "多栈共存：utun 接口 ${UTUN_N} 个（§6b，**仅提示可疑**）"
  else
    echo "    [正常] utun 接口 ${UTUN_N} 个，未见明显多栈痕迹。"
  fi
  echo "    注：这条是**提示可疑**级别（确定性判据只有 §3c 的路由形态与 §8.7 的国内外对照）。"
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

# --- 8.7 国内 vs 国外：**同一时刻**对照（判「国内不通」的直接判据）---
{
  echo
  echo "-- 8.7 国内 vs 国外**同一时刻**对照（直连 / 经 SOCKS 各一遍；给 http 状态码 + 耗时）"
  echo "   为什么必须「同一时刻」：用户症状是间歇的，分两次测会把「时间差」当成「路径差」。"
  while IFS='|' read -r where name url; do
    [ -z "${url:-}" ] && continue
    curl_probe "直连   ${where} ${name}" "$url"
  done <<'EOF'
国内|www.baidu.com|https://www.baidu.com/
国内|api.deepseek.com|https://api.deepseek.com/
国外|google generate_204|https://www.google.com/generate_204
国外|github.com|https://github.com/
EOF
  if [ "$SOCKS_LISTEN" = "yes" ]; then
    while IFS='|' read -r where name url; do
      [ -z "${url:-}" ] && continue
      curl_probe "SOCKS  ${where} ${name}" --socks5-hostname "127.0.0.1:${SOCKS_PORT}" "$url"
    done <<'EOF'
国内|www.baidu.com|https://www.baidu.com/
国内|api.deepseek.com|https://api.deepseek.com/
国外|google generate_204|https://www.google.com/generate_204
国外|github.com|https://github.com/
EOF
  else
    echo "  （本机 SOCKS 未监听/未检测到端口，跳过「经 SOCKS」那一遍）"
  fi
  # 判读用同一时刻抓到的四个状态码
  run_cap 10 curl -s -o /dev/null --max-time 7 -w '%{http_code}' https://www.baidu.com/
  CB="$RUN_OUT"
  run_cap 10 curl -s -o /dev/null --max-time 7 -w '%{http_code}' https://api.deepseek.com/
  CD="$RUN_OUT"
  run_cap 10 curl -s -o /dev/null --max-time 7 -w '%{http_code}' https://www.google.com/generate_204
  CG="$RUN_OUT"
  echo
  echo "-- 判读（直连状态码：baidu=${CB} deepseek=${CD} google=${CG}；000 = 连不上/超时）"
  if [ "$CB" = "000" ] && [ "$CG" != "000" ]; then
    echo "    [异常] **国内不通、国外通** —— 与用户症状一致（分流/规则或上游链路，而不是整机断网）"
    note_anomaly "国内不通而国外通（§8.7：baidu=000, google=${CG}）"
  elif [ "$CB" = "000" ] && [ "$CG" = "000" ]; then
    echo "    [异常] 国内国外**都不通** ⇒ 更像整机断网/默认路由黑洞（见 §9 的 A 态）"
    note_anomaly "国内国外都不通（§8.7）"
  elif [ "$CB" != "000" ] && [ "$CG" != "000" ]; then
    echo "    [正常] 直连国内国外都通（此刻不处于「国内不通」状态）"
  else
    echo "    [可疑] 只有国外不通 —— 与用户症状相反，可能是本机到国外的路径问题"
  fi
  # 再判「经代理」这一路：用户症状在这里最常见 —— 直连全通、经代理时国内站挂。
  # ⚠️ **必须跑多轮再下结论**：实测到过一次 SOCKS → baidu rc=35 SSL_ERROR_SYSCALL，
  #    但随后 4 轮复测全部 200 —— **单次采样不能当「活体形态」**（那是假红，
  #    与「按 80 行就当重复够了」是同一类错误：单点采样 + 结论性措辞）。
  #    所以这里跑 N 轮，只报「几轮中几轮失败」，并按**多数轮**才升格为异常。
  PROXY_ROUNDS=3
  if [ "$SOCKS_LISTEN" = "yes" ]; then
    FAIL_D=0; FAIL_G=0; _i=1
    while [ "$_i" -le "$PROXY_ROUNDS" ]; do
      run_cap 10 curl -s -o /dev/null --max-time 7 -w '%{http_code}' --socks5-hostname "127.0.0.1:${SOCKS_PORT}" https://www.baidu.com/
      _sb="$RUN_OUT"; [ "$_sb" = "000" ] && FAIL_D=$((FAIL_D+1))
      run_cap 12 curl -s -o /dev/null --max-time 9 -w '%{http_code}' --socks5-hostname "127.0.0.1:${SOCKS_PORT}" https://www.google.com/generate_204
      _sg="$RUN_OUT"; [ "$_sg" = "000" ] && FAIL_G=$((FAIL_G+1))
      printf '    轮%s：SOCKS 国内 baidu=%s  国外 google=%s\n' "$_i" "$_sb" "$_sg"
      _i=$((_i+1))
    done
    echo "   ${PROXY_ROUNDS} 轮中失败次数：国内 baidu ${FAIL_D}/${PROXY_ROUNDS}；国外 google ${FAIL_G}/${PROXY_ROUNDS}"
    if [ "$FAIL_D" -ge 2 ] && [ "$FAIL_G" -eq 0 ]; then
      echo "    [异常] 经代理：国内站**多数轮失败**、国外站全通 ⇒ 与用户症状（国内不通、国外良好）一致"
      note_anomaly "经代理：国内站多数轮失败而国外站全通（§8.7：SOCKS baidu ${FAIL_D}/${PROXY_ROUNDS}）"
    elif [ "$FAIL_D" -ge 1 ] && [ "$FAIL_G" -eq 0 ]; then
      echo "    [可疑·瞬态，不下结论] 国内站只有 ${FAIL_D}/${PROXY_ROUNDS} 轮失败、国外站全通。"
      echo "       单次采样**不足以**定性（本项目栽过「单点采样 + 结论性措辞」）；请在**故障当下**再多跑几次。"
      note_anomaly "经代理：国内站偶发失败 ${FAIL_D}/${PROXY_ROUNDS} 轮（§8.7，**疑为瞬态，不据此定性**）"
    elif [ "$FAIL_D" -ge 2 ] && [ "$FAIL_G" -ge 2 ]; then
      echo "    [异常] 经代理国内国外**多数轮都失败** ⇒ 代理/隧道本身不通（不是分流问题）"
      note_anomaly "经代理国内国外多数轮都失败（§8.7）"
    else
      echo "    [正常] 经代理国内外都通（国内失败 ${FAIL_D}/${PROXY_ROUNDS}、国外 ${FAIL_G}/${PROXY_ROUNDS}）"
    fi
  fi
} >> "$OUT"

# --- 8.8 哨兵 DNS 与隧道状态：显式判定「隧道死了但 DNS 还指着哨兵」---
{
  echo
  echo "-- 8.8 哨兵 DNS 与隧道状态（两者**分开报**，再给一句结论）"
  run_cap 10 scutil --dns
  NS0="$(printf '%s\n' "$RUN_OUT" | awk '/nameserver\[0\]/{print $3; exit}')"
  SENT_ACTIVE="no"
  case "$NS0" in 198.18.*) SENT_ACTIVE="yes" ;; esac
  UTUN_UP="$(ifconfig -a 2>/dev/null | awk -F: '/^utun[0-9]+:/{print $1}' | while read -r u; do ifconfig "$u" 2>/dev/null | grep -q 'inet ' && echo "$u"; done | tr '\n' ' ')"
  TUNNEL_ALIVE="no"
  [ -n "$UTUN_UP" ] && TUNNEL_ALIVE="yes"
  CORE_ALIVE="不在"; pgrep -f '/xray( |$)' >/dev/null 2>&1 && CORE_ALIVE="在"
  printf '  活动解析器 nameserver[0] = %s\n' "${NS0:-（无）}"
  printf '  哨兵 198.18.x 仍是活动解析器 = %s（scutil --dns 里 198.18 行数=%s）\n' "$SENT_ACTIVE" "$SENTINEL_DNS"
  printf '  隧道（有 inet 地址的 utun）= %s（%s）；核心进程 = %s\n' "$TUNNEL_ALIVE" "${UTUN_UP:-无}" "$CORE_ALIVE"
  run_cap 8 dig +short +time=3 +tries=1 @198.18.0.2 www.baidu.com
  printf '  dig www.baidu.com @198.18.0.2（哨兵）→ %s\n' "$(printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-80)"
  run_cap 8 dig +short +time=3 +tries=1 @223.5.5.5 www.baidu.com
  printf '  dig www.baidu.com @223.5.5.5（直连）→ %s\n' "$(printf '%s' "$RUN_OUT" | tr '\n' ' ' | cut -c1-80)"
  echo "  【结论】"
  if [ "$SENT_ACTIVE" = "yes" ] && [ "$TUNNEL_ALIVE" = "no" ]; then
    echo "    [异常] **隧道已经不在，但系统 DNS 仍指向哨兵 198.18.x** —— 此刻域名解析会失败/挂住，"
    echo "           这正是「App 断开/退出前连域名都解析不了」的形态（H4）。"
    note_anomaly "隧道已不在但 DNS 仍指向哨兵 198.18.x（§8.8）"
  elif [ "$SENT_ACTIVE" = "yes" ] && [ "$TUNNEL_ALIVE" = "yes" ]; then
    echo "    [正常] 隧道在跑、DNS 指向哨兵 —— 这是**设计内**形态（解析走隧道内 DNS）。"
  elif [ "$SENT_ACTIVE" = "no" ] && [ "$TUNNEL_ALIVE" = "no" ]; then
    echo "    [正常] 没有隧道、DNS 也没指着哨兵 —— 没有残留。"
  else
    echo "    [可疑] 隧道在跑但 DNS 不指向哨兵 —— 解析可能没走隧道（与预期分流不一致）。"
  fi
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

# ---------------------------------------------------------------------------
# 10) 异常清单（让读者一眼分出「现在正常」与「发现异常」）
# ---------------------------------------------------------------------------
section "10. 异常清单（一眼看）"
{
  ANOM_N="$(anomaly_count)"
  if [ "$ANOM_N" = "0" ]; then
    echo "  [正常] 本次取证**没有**判定出异常（§2c / §3c / §6b / §8.7 / §8.8 都没报异常）。"
    echo
    echo "  ⚠️ 但这**不等于**「问题不存在」：用户症状是**间歇性**的 —— 现在正常只说明"
    echo "     「这一次没抓到」。**下一次感觉不对时立刻再跑一次**（见脚本头部的说明）。"
  else
    echo "  [发现异常] 共 ${ANOM_N} 条："
    printf '%s' "$ANOMALIES"
  fi
  echo
  echo "  确定性判据（可直接当异常）："
  echo "    · §3c  回环路由形态：127/8 必须落在 lo0（否则 127.x.x.x 会被送去网关）"
  echo "    · §2c  127.0.0.1 与 127.0.0.2 的可达性差异"
  echo "    · §8.7 国内 vs 国外**同一时刻**对照（直连状态码）"
  echo "    · §8.8 「隧道已不在但 DNS 仍指向哨兵 198.18.x」"
  echo "  只能提示可疑（要人工判断）：§6b 的 utun 数量与第三方栈共存、代理旁路表内容。"
} >> "$OUT"

echo
echo "✓ 取证完成：$OUT"
echo "  把它发给维护者即可。报告默认含 IP 与节点地址；如需打码：--redact-ip --redact-node 重跑。"
ANOM_FINAL="$(anomaly_count)"
if [ "$ANOM_FINAL" != "0" ]; then
  echo "  ⚠️ 本次判定出 ${ANOM_FINAL} 条异常（见报告第 10 节）。"
else
  echo "  （本次未判定出异常；症状是间歇性的，下一次发生时请立刻重跑一次。）"
fi
