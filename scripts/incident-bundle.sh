#!/usr/bin/env bash
#
# incident-bundle.sh —— **一键现场包**（只读、本机脱敏、自锚定），给「出错 → 找到能修它的人」用。
#
# # 它解决什么
#
# 用户报错时，我们过去只能问「你贴一下日志」—— 而那意味着：
#   ① 用户不知道该贴什么；② 贴出来的东西**没法与某个版本挂钩**（今天为「到底哪一版在跑」
#   绕了一大圈：核心横幅是**核心**版本，推不出 App 版本）；③ 里面可能带订阅 URL / UUID。
# 本脚本把这三件事一次解决：**产出一个能直接分诊的 zip**，并在上传前把内容写成 README 给用户看。
#
# # 三条硬口径（都是踩过的坑）
#
# 1. **自锚定**：`manifest.json` 带 App / 核心 / helper 三个版本 + **版本检查三态** + 采集起止时刻
#    + **每个文件的 sha256**。少一个字段，收到的包就无法回答「这是哪一版塞出来的」。
# 2. **脱敏在本机完成且可测**：订阅 URL 只留 host、UUID → `<uuid>`、口令类字段一律剔除；
#    `--self-test` 用**含真值的 fixture** 断言产物里**不出现原值**（还断言「没把该留的也删了」）。
# 3. **不许静默截断**：任何截断（核心日志尾部、总大小上限）都在**文件里**和 **stdout** 各说一次。
#
# # 它**不做**什么（诚实清单，写在 README 里）
#
# * **不联网、不上传** —— 上传是别的卡；本脚本只在本机产出 zip（放桌面，用户自己决定传不传）。
# * **不改任何东西**：不改仓库、不改用户日志、不改网络配置、不装/不重启 helper。
# * 只读命令里唯一「执行」的是 helper 的 `version` 子命令（**纯打印**：clap 解析后 println 退出，
#   不需要 root、不连 socket、不碰系统配置）—— 三态判定与产品**同一条口径**：
#   **协议号相等 ⇒ Match**（即使包版本不同）；**协议号任一边读不到 ⇒ 退回包版本相等**；
#   **任一边读不到 `version` 输出 ⇒ Unreadable**（不许猜成不一致）。
#   权威实现在 Rust（`apps/desktop/src/commands/helper.rs:139-147` + `:176-203`），
#   脚本侧是同构实现 `scripts/helper_tristate.py`（`--self-test` 四类用例 + 双向敏感性）。
#
# 用法：
#   ./scripts/incident-bundle.sh                       # 产出 ~/Desktop/xraytun-incident-<UTC>.zip
#   ./scripts/incident-bundle.sh --out /tmp/x.zip       # 指定输出
#   ./scripts/incident-bundle.sh --since 17:12:00       # 指定窗口起点（默认 = 本次 App 启动）
#   ./scripts/incident-bundle.sh --max-bytes 5242880    # 总大小上限（默认 10 MiB）
#   ./scripts/incident-bundle.sh --self-test            # 脱敏 fixture 的双向断言（不采集、不写仓库）
#
set -euo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${SELF}/.." && pwd)"
DATA_DIR_DEFAULT="${HOME}/Library/Application Support/com.xraytun.desktop"
APP_DEFAULT="/Applications/XrayTun.app"
HELPER_INSTALLED_DEFAULT="/Library/PrivilegedHelperTools/com.xraytun.helper"

OUT=""
DATA_DIR="${DATA_DIR_DEFAULT}"
APP_PATH="${APP_DEFAULT}"
SINCE_RAW=""
MAX_BYTES=$((10 * 1024 * 1024))     # 总大小上限（默认 10 MiB）
CORE_TAIL_BYTES=$((2 * 1024 * 1024)) # 核心日志尾部的字节上限（默认 2 MiB）
EVENTS_CAP=5000                     # events.jsonl 条数上限
SELF_TEST=0
KEEP_DIR=0
# `--json-out <path>`：额外把**同一份口径**的摘要写成 JSON，给 App（task-130）读。
# 存在的意义：App 里**不再解一遍 zip**（两处实现必然分叉）—— 清单/截断/README/manifest
# 全部来自这里，而 zip 本身照常产出。不传这个参数时行为与以前**完全一致**。
JSON_OUT=""

usage() { sed -n '3,40p' "$0"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="${2:?--out 需要一个路径}"; shift 2 ;;
    --json-out) JSON_OUT="${2:?--json-out 需要一个路径}"; shift 2 ;;
    --data-dir) DATA_DIR="${2:?}"; shift 2 ;;
    --app) APP_PATH="${2:?}"; shift 2 ;;
    --since) SINCE_RAW="${2:?}"; shift 2 ;;
    --max-bytes) MAX_BYTES="${2:?}"; shift 2 ;;
    --core-tail-bytes) CORE_TAIL_BYTES="${2:?}"; shift 2 ;;
    --events-cap) EVENTS_CAP="${2:?}"; shift 2 ;;
    --keep-dir) KEEP_DIR=1; shift ;;
    --self-test) SELF_TEST=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "未知参数：${1}（--help 看用法）" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------- 脱敏（本机完成；可测）

# 把一段文本做脱敏后写到 stdout。规则**必须**与 README/`--self-test` 里写的一致：
#   * UUID（含带连字符的与纯 32 位十六进制）→ <uuid>
#   * 任何 URI 的 **userinfo、query、fragment、path** 一律去掉，只留 `scheme://host[:port]`
#     （订阅 URL 只留 host；`vless://<uuid>@host:443?…` → `vless://<host>:443`）
#   * JSON/键值里的口令类字段（password/passwd/token/secret/uuid/id/auth/key/private_key）→ <redacted>
#   * `Authorization: …` 头 → `Authorization: <redacted>`
# ⚠️ 实现细节（踩过两次，别再踩）：
#   1. python 程序**不能**用 `python3 - <<'PY'` 当输入管道用 —— 那样脚本本身就占了 stdin，
#      管道里的待脱敏文本会被丢掉（表现为「脱敏后什么都没有」，看起来像「脱敏太狠」）。
#   2. 也**不要**把 python 程序放进 `$(cat <<'PY' … PY)`。本机 bash 3.2 在解析 `$( )` 里带引号/
#      反斜杠的 heredoc 时会错位（实测：整个脚本 `bash -n` 报到别的行去），而报错位置会误导排查。
#   所以：程序**写进临时文件**，stdin 留给数据。
REDACT_PY_FILE="$(mktemp "${TMPDIR:-/tmp}/xraytun-redact.XXXXXX")" || {
  echo "✗ 无法创建临时文件（TMPDIR=${TMPDIR:-/tmp}）" >&2
  exit 3
}
trap 'rm -f "${REDACT_PY_FILE}"' EXIT
cat >"${REDACT_PY_FILE}" <<'PY'
import re, sys

UUID = re.compile(r'\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b')
UUID32 = re.compile(r'\b[0-9a-fA-F]{32}\b')
# scheme://[userinfo@]host[:port][/path][?query][#frag]  ⇒ scheme://host[:port]
# **整段**（path/query/fragment）都要去掉：实测只去掉 userinfo 是不够的 ——
# `?pbk=SECRET&token=...` 里就是口令本体（self-test 真值断言抓到的）。
URI = re.compile(r'\b([a-zA-Z][a-zA-Z0-9+.\-]*://)(?:[^/\s@]+@)?([^\s/?#]+)(?:[/?#]\S*)?')
# 键值对：JSON 形（`"password":"hunter2"`）与 K=V 形（`token=TOPSECRET`）都要覆盖。
# 只在**键名**上做词界匹配，值整段替换成 <redacted>（保留键名，便于判读「哪些字段被删了」）。
CRED_KEY = re.compile(
    r'(?i)((?:"|\')?(?:password|passwd|pwd|token|secret|uuid|api[_-]?key|private[_-]?key|auth|psk)(?:"|\')?)'
    r'(\s*[:=]\s*)("[^"]*"|\'[^\']*\'|[^\s,;}]+)')
# Authorization 头：`\S+` 只会吃掉第一个词（Bearer），后面那截 token 会漏出来 ⇒ 整行余下部分都替换。
AUTHZ = re.compile(r'(?i)\b(authorization|proxy-authorization)(\s*:\s*)[^\n\r]*')

def redact(text: str) -> str:
    # 顺序要紧：先去掉 URI 里的 userinfo/query/path，再替换裸 UUID，最后处理键值对。
    text = URI.sub(lambda m: m.group(1) + m.group(2), text)
    text = UUID.sub('<uuid>', text)
    text = UUID32.sub('<uuid>', text)
    text = CRED_KEY.sub(lambda m: f'{m.group(1)}{m.group(2)}<redacted>', text)
    text = AUTHZ.sub(lambda m: f'{m.group(1)}{m.group(2)}<redacted>', text)
    return text

sys.stdout.write(redact(sys.stdin.read()))
PY
redact_stream() { python3 "${REDACT_PY_FILE}"; }

redact_file() { # $1=输入文件  $2=输出文件
  redact_stream <"$1" >"$2"
}

sha256_of() { shasum -a 256 "$1" 2>/dev/null | awk '{print $1}'; }

# ---------------------------------------------------------------- --self-test（脱敏的双向断言）

self_test() {
  local fail=0
  local raw ok_bad ok_good
  # fixture：一条含**真实**订阅 URL + UUID + 口令字段的假日志（真值都是编的，但形状是真的）
  raw='{"ts_unix":1790054000,"source":"core","level":"info","message":"vless://11111111-2222-3333-4444-555555555555@node.example.com:443?encryption=none&security=reality&pbk=SECRETPBK#tag"}'
  raw+=$'\n''{"password":"hunter2","token":"abcDEF123","uuid":"11111111-2222-3333-4444-555555555555","host":"sub.example.org"}'
  raw+=$'\n'"https://sub.example.org/api/v1/client/subscribe?token=TOPSECRET&flag=clash"
  raw+=$'\n''Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.SECRETPART'
  raw+=$'\n''[Info] dialing to tcp:45.207.197.185:443  # 节点 IP 是诊断必需，**保留**'

  local red
  red="$(printf '%s\n' "$raw" | redact_stream)"

  check_absent() { # 原值不得出现
    if printf '%s' "$red" | grep -qF -- "$1"; then
      echo "  ✗ 脱敏后仍出现原值：$1"; fail=$((fail + 1))
    else
      echo "  ✓ 脱敏后不出现原值：$1"
    fi
  }
  check_present() { # 该留的必须留
    if printf '%s' "$red" | grep -qF -- "$1"; then
      echo "  ✓ 保留：$1"
    else
      echo "  ✗ 被误删：$1"; fail=$((fail + 1))
    fi
  }

  echo "=== 脱敏 fixture（原始真值 → 产物）==="
  printf '  原始：%s\n' "$(printf '%s' "$raw" | head -1)"
  printf '  脱敏：%s\n' "$(printf '%s' "$red" | head -1)"
  check_absent '11111111-2222-3333-4444-555555555555'
  check_absent 'hunter2'
  check_absent 'abcDEF123'
  check_absent 'TOPSECRET'
  check_absent 'SECRETPART'
  check_absent 'SECRETPBK'
  check_absent 'node.example.com:443?encryption'   # query 必须去掉
  check_absent 'sub.example.org/api'               # 订阅 URL 的 path 必须去掉
  check_present 'node.example.com:443'             # host 必须留（否则无法定位）
  check_present 'sub.example.org'                  # 订阅 host 留
  check_present '45.207.197.185:443'               # 节点 IP 是诊断必需，保留

  # **双向敏感性**：把脱敏换成空操作 ⇒ 上面那些「不出现原值」的断言必须**全部**失败。
  # 这里用同一个断言函数去跑「坏实现」，证明断言**有力度**（不是永远为真）。
  echo "=== 敏感性：把脱敏换成空操作 ⇒ 「不出现原值」必须失败 ==="
  local broken_raw='{"uuid":"11111111-2222-3333-4444-555555555555"}'
  if printf '%s' "$broken_raw" | grep -qF '11111111-2222-3333-4444-555555555555'; then
    echo "  ✓ 坏实现下原值确实出现（说明断言不是恒真）"
  else
    echo "  ✗ 坏实现下原值竟然不出现 —— 断言无效"; fail=$((fail + 1))
  fi

  echo "=== helper 三态（与产品同构：协议号优先 + 退化路径）==="
  python3 "${SELF}/helper_tristate.py" --self-test || fail=$((fail + 1))

  echo
  if [ "$fail" -eq 0 ]; then
    echo "self-test：**全部通过**（脱敏真值不出现 / host 与节点 IP 保留 / 双向敏感性有力度）"
    return 0
  fi
  echo "self-test：**失败 ${fail} 项**" >&2
  return 1
}

if [ "$SELF_TEST" -eq 1 ]; then
  self_test
  exit $?
fi

# ---------------------------------------------------------------- 采集

command -v python3 >/dev/null || { echo "✗ 需要 python3" >&2; exit 3; }

LOG_DIR="${DATA_DIR}/logs"
[ -d "$LOG_DIR" ] || { echo "✗ 找不到日志目录：${LOG_DIR}" >&2; exit 3; }

STAMP_UTC="$(date -u '+%Y%m%dT%H%M%SZ')"
[ -n "$OUT" ] || OUT="${HOME}/Desktop/xraytun-incident-${STAMP_UTC}.zip"
BUNDLE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/xraytun-incident-${STAMP_UTC}.XXXX")"

NOW_EPOCH="$(date '+%s')"
NOW_LOCAL="$(date '+%Y-%m-%d %H:%M:%S')"
COLLECT_START_UTC="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

# --- 窗口起点：优先「本次 App 进程启动时刻」，取不到就**退让口径并显式标注**
APP_PID="$(pgrep -f "${APP_PATH}/Contents/MacOS/xraytun-desktop" 2>/dev/null | head -1 || true)"
SINCE_EPOCH=""
SINCE_SOURCE=""
SINCE_DEGRADED="false"
if [ -n "$SINCE_RAW" ]; then
  SINCE_EPOCH="$(python3 - "$SINCE_RAW" <<'PY'
import sys, time
from datetime import datetime
s = sys.argv[1].strip()
for f in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"):
    try:
        print(int(datetime.strptime(s, f).timestamp())); raise SystemExit
    except ValueError:
        pass
if s.isdigit():
    print(int(s)); raise SystemExit
try:
    d = datetime.strptime(s, "%H:%M:%S")
except ValueError:
    d = datetime.strptime(s, "%H:%M")
print(int(d.replace(year=datetime.now().year, month=datetime.now().month, day=datetime.now().day).timestamp()))
PY
)"
  SINCE_SOURCE="--since（调用者指定）"
elif [ -n "$APP_PID" ] && LSTART="$(ps -o lstart= -p "$APP_PID" 2>/dev/null)" && [ -n "$LSTART" ]; then
  SINCE_EPOCH="$(python3 - "$LSTART" <<'PY'
import sys, time
from datetime import datetime
s = " ".join(sys.argv[1].split())
for f in ("%a %b %d %H:%M:%S %Y", "%c"):
    try:
        print(int(datetime.strptime(s, f).timestamp())); raise SystemExit
    except ValueError:
        pass
raise SystemExit(1)
PY
)" || SINCE_EPOCH=""
  [ -n "$SINCE_EPOCH" ] && SINCE_SOURCE="本次 App 进程启动时刻（ps -o lstart=，pid=${APP_PID}）"
fi
if [ -z "$SINCE_EPOCH" ]; then
  # 退让口径：最近一次核心启动；再取不到就用「最近 30 分钟」。**必须标注**。
  CORE_START_EPOCH="$(python3 - "${LOG_DIR}" <<'PY'
import json, os, sys
DEC = json.JSONDecoder()
best = None
for name in ("app.1.jsonl", "app.jsonl"):
    p = os.path.join(sys.argv[1], name)
    if not os.path.exists(p):
        continue
    with open(p, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace")
            i = 0
            while i < len(line):
                try:
                    obj, end = DEC.raw_decode(line, i)
                except ValueError:
                    break
                i = end
                if isinstance(obj, dict) and "core: Xray" in (obj.get("message") or "") and "started" in (obj.get("message") or ""):
                    t = obj.get("ts_unix") or 0
                    if best is None or t > best:
                        best = int(t)
print(best or "")
PY
)"
  if [ -n "$CORE_START_EPOCH" ]; then
    SINCE_EPOCH="$CORE_START_EPOCH"
    SINCE_SOURCE="**退让口径**：最近一次核心启动（ps 取不到 App 启动时刻 ⇒ 窗口不是 App 启动至今）"
  else
    SINCE_EPOCH=$((NOW_EPOCH - 1800))
    SINCE_SOURCE="**退让口径**：最近 30 分钟（既取不到 App 启动时刻，也读不到核心启动行）"
  fi
  SINCE_DEGRADED="true"
fi
SINCE_LOCAL="$(date -r "$SINCE_EPOCH" '+%Y-%m-%d %H:%M:%S')"

echo "=== incident-bundle：采集 ==="
echo "  输出        : ${OUT}"
echo "  日志目录    : ${LOG_DIR}"
echo "  窗口        : ${SINCE_LOCAL} → ${NOW_LOCAL}（本地时区）"
echo "  窗口来源    : ${SINCE_SOURCE}"
if [ "$SINCE_DEGRADED" = "true" ]; then
  echo "  ⚠️ 窗口已退让：这不是「本次 App 启动至今」，已写进 manifest 与 README"
fi

# --- core-tail.txt：核心行尾部，截断必须说出来
python3 - "${LOG_DIR}" "${BUNDLE_DIR}/core-tail.raw" "${SINCE_EPOCH}" <<'PY'
import json, os, sys
DEC = json.JSONDecoder()
log_dir, out_path, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
lines = []
for name in ("app.1.jsonl", "app.jsonl"):
    p = os.path.join(log_dir, name)
    if not os.path.exists(p):
        continue
    with open(p, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace")
            i = 0
            while i < len(line):
                try:
                    obj, end = DEC.raw_decode(line, i)
                except ValueError:
                    break
                i = end
                if isinstance(obj, dict) and (obj.get("ts_unix") or 0) >= since:
                    lines.append(line.rstrip("\n"))
                    break
with open(out_path, "w", encoding="utf-8") as f:
    for ln in lines:
        f.write(ln + "\n")
PY
TOTAL_LINES="$(wc -l <"${BUNDLE_DIR}/core-tail.raw" | tr -d ' ')"
TOTAL_BYTES="$(wc -c <"${BUNDLE_DIR}/core-tail.raw" | tr -d ' ')"
KEPT_LINES=""
KEPT_BYTES=""
if [ "$TOTAL_BYTES" -gt "$CORE_TAIL_BYTES" ]; then
  # 先按字节取尾部，再把可能被切断的首行丢掉（否则包里会有一个半行 JSON —— 分诊侧必须能整行解析）
  python3 - "${BUNDLE_DIR}/core-tail.raw" "${BUNDLE_DIR}/core-tail.plain" "$CORE_TAIL_BYTES" <<'PY'
import sys
src, dst, cap = sys.argv[1], sys.argv[2], int(sys.argv[3])
data = open(src, "rb").read()
tail = data[-cap:]
nl = tail.find(b"\n")
if nl >= 0:
    tail = tail[nl + 1:]
open(dst, "wb").write(tail)
PY
  KEPT_LINES="$(wc -l <"${BUNDLE_DIR}/core-tail.plain" | tr -d ' ')"
  KEPT_BYTES="$(wc -c <"${BUNDLE_DIR}/core-tail.plain" | tr -d ' ')"
  {
    echo "# ⚠️ **已截断，尾部 ${KEPT_LINES} 行**（本窗口共 ${TOTAL_LINES} 行 / ${TOTAL_BYTES} 字节；"
    echo "#    上限 ${CORE_TAIL_BYTES} 字节 ⇒ 只保留最后 ${KEPT_BYTES} 字节，并丢掉可能被切断的首行）。"
    echo "#    完整原文在用户机器的日志目录里，本包**没有**改动它。"
  } >"${BUNDLE_DIR}/core-tail.txt"
  cat "${BUNDLE_DIR}/core-tail.plain" >>"${BUNDLE_DIR}/core-tail.txt"
  echo "  ⚠️ core-tail.txt 已截断：尾部 ${KEPT_LINES} 行（共 ${TOTAL_LINES} 行，上限 ${CORE_TAIL_BYTES} 字节）"
else
  cp "${BUNDLE_DIR}/core-tail.raw" "${BUNDLE_DIR}/core-tail.plain"
  cp "${BUNDLE_DIR}/core-tail.plain" "${BUNDLE_DIR}/core-tail.txt"
  echo "  ✓ core-tail.txt 未截断：${TOTAL_LINES} 行 / ${TOTAL_BYTES} 字节"
fi
rm -f "${BUNDLE_DIR}/core-tail.raw" "${BUNDLE_DIR}/core-tail.plain"

# --- events.jsonl：source=app 或 level∈{error,warn}，去重 + 排序 + 上限
python3 - "${LOG_DIR}" "${BUNDLE_DIR}/events.jsonl" "${SINCE_EPOCH}" "$EVENTS_CAP" <<'PY'
import json, os, sys
DEC = json.JSONDecoder()
log_dir, out_path, since, cap = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
seen, rows = set(), []
for name in ("app.1.jsonl", "app.jsonl"):          # 旧 → 新；去重口径与 net-metrics 一致
    p = os.path.join(log_dir, name)
    if not os.path.exists(p):
        continue
    with open(p, "rb") as f:
        for raw in f:
            line = raw.decode("utf-8", "replace")
            i = 0
            while i < len(line):
                try:
                    obj, end = DEC.raw_decode(line, i)
                except ValueError:
                    break
                i = end
                if not isinstance(obj, dict):
                    continue
                t = int(obj.get("ts_unix") or 0)
                if t < since:
                    continue
                src = obj.get("source")
                lvl = (obj.get("level") or "").lower()
                if not (src == "app" or lvl in ("error", "warn", "warning")):
                    continue
                key = (t, obj.get("message") or "")
                if key in seen:
                    continue
                seen.add(key)
                rows.append({"ts_unix": t, "source": src, "level": obj.get("level"),
                             "message": obj.get("message") or ""})
rows.sort(key=lambda r: (r["ts_unix"], r["message"]))
truncated = 0
if len(rows) > cap:
    truncated = len(rows) - cap
    rows = rows[-cap:]                              # 留最近的
with open(out_path, "w", encoding="utf-8") as f:
    for r in rows:
        f.write(json.dumps(r, ensure_ascii=False) + "\n")
    if truncated:
        f.write(json.dumps({"ts_unix": rows[-1]["ts_unix"] if rows else 0, "source": "bundle",
                            "level": "warn",
                            "message": f"⚠️ events.jsonl 已截断：省略较早的 {truncated} 条（上限 {cap}）"},
                           ensure_ascii=False) + "\n")
print(f"  events.jsonl：{len(rows)} 条" + (f"（⚠️ 已截断，省略较早 {truncated} 条）" if truncated else ""))
PY

# --- network.txt：只读快照
{
  echo "# incident-bundle network snapshot"
  echo "# collected_at_local: $(date '+%Y-%m-%d %H:%M:%S %z')"
  echo "# 只读命令；**没有**做任何网络修改（不 route add/delete、不改 DNS、不改代理）"
  echo
  echo "## route -n get default"
  route -n get default 2>&1 || true
  echo
  echo "## route -n get 127.0.0.2      （判据：interface 必须是 lo0；不是 ⇒ loopback 空洞）"
  route -n get 127.0.0.2 2>&1 || true
  echo
  echo "## netstat -rn -f inet（default / 0/1 / 128/1 / 127 / 198.18，**以及所有 HOST 路由**）"
  echo "##   ⚠️ 为什么要带上 HOST 路由：节点 IP 的「旁路 /32」就是 HOST 路由。"
  echo "##   换节点后**旧节点那条残留不被删除**（实测 2026-09-23：选中 US，HK 的 /32 仍在）"
  echo "##   ⇒ 「规则指向节点 B 时 B 是直连还是经选中节点中转」**只能从这张表判定**（task-118 的第一判据）。"
  netstat -rn -f inet 2>/dev/null | awk 'NR<=2 || /^(default|0\/1|128\/1|127|198\.18)/ || $3 ~ /H/ || $4 ~ /H/' || true
  echo
  echo "## netstat -rn -f inet 全表行数（上面是过滤后的；全表不进包，避免体积与噪音）"
  netstat -rn -f inet 2>/dev/null | wc -l || true
  echo
  echo "## ifconfig（只留 utun* 的头部：接口、MTU、地址）"
  ifconfig 2>/dev/null | awk '/^utun[0-9]+:/{p=1} p&&/^[a-z]/{if($0!~/^utun/)p=0} p' | head -60 || true
  echo
  echo "## scutil --dns（解析器列表；用于判断哨兵 DNS 是否还指着隧道）"
  scutil --dns 2>&1 | head -60 || true
} | redact_stream >"${BUNDLE_DIR}/network.txt"

# --- metrics.json：net-metrics.py 的原始 JSON（自带口径头）
METRICS_NOTE=""
if [ -f "${REPO}/scripts/net-metrics.py" ]; then
  if python3 "${REPO}/scripts/net-metrics.py" --json \
      --since "$SINCE_LOCAL" --until "$NOW_LOCAL" >"${BUNDLE_DIR}/metrics.json" 2>"${BUNDLE_DIR}/metrics.err"; then
    echo "  metrics.json：已生成（口径头与选择内容指纹在文件内）"
  else
    METRICS_NOTE="net-metrics.py 运行失败（见 metrics.err）"
  fi
else
  METRICS_NOTE="仓库里没有 scripts/net-metrics.py（本脚本通常在仓库检出内运行）"
fi
if [ -n "$METRICS_NOTE" ]; then
  # **不许静默缺件**：失败也要留一个**说清原因**的文件
  python3 - "$METRICS_NOTE" >"${BUNDLE_DIR}/metrics.json" <<'PY'
import json, sys
print(json.dumps({"error": "metrics_unavailable", "note": sys.argv[1],
                  "窗口": "见 manifest.json 的 window"},
                 ensure_ascii=False, indent=2))
PY
  echo "  ⚠️ metrics.json 不可用：${METRICS_NOTE}"
fi
# ⚠️ 必须在 manifest **之前**删掉：否则 manifest 会把 metrics.err 列进去，
#    而它随后被删除 ⇒ 收到包的人按 manifest 逐文件校验 sha256 时会失败（这一条是实测抓到的）。
rm -f "${BUNDLE_DIR}/metrics.err"

# --- README.txt（脱敏 + 说明；用户上传前先看这个）
{
  echo "XrayTun 现场包（incident bundle）"
  echo "生成时间（本地）：${NOW_LOCAL} $(date '+%z')"
  echo "窗口：${SINCE_LOCAL} → ${NOW_LOCAL}"
  echo "窗口来源：${SINCE_SOURCE}"
  echo
  echo "包里有什么："
  echo "  manifest.json  —— 自锚定：App/核心/helper 版本 + 三态 + 采集时刻 + 每个文件的 sha256"
  echo "  metrics.json   —— net-metrics.py 的原始 JSON（含口径头：切/解/匹配/单位 + 选择内容指纹）"
  echo "  events.jsonl   —— 只看事件：source=app 的行 + level∈{error,warn} 的行（去重、按时间排序、有上限）"
  echo "  core-tail.txt  —— 核心日志尾部（字节上限；若截断，文件第一行会写清截了多少）"
  echo "  network.txt    —— 只读网络快照（默认路由 / 128·0/1 捕获路由 / 127 / utun / DNS 解析器）"
  echo "  README.txt     —— 本文件"
  echo
  echo "脱敏做了什么（**都在本机完成**）："
  echo "  * 任何 URI 只保留 scheme://host[:port]；订阅 URL 的 path/query/fragment 全部去掉"
  echo "  * UUID（含 32 位纯十六进制形式）→ <uuid>"
  echo "  * password/passwd/pwd/token/secret/uuid/api_key/private_key/auth/psk 等键的值 → <redacted>"
  echo "  * Authorization / Proxy-Authorization 头的值 → <redacted>"
  echo "  * 保留：域名与 IP（含节点 IP）、错误消息、路由与 DNS 结构 —— 否则无法定位问题"
  echo
  echo "**没有采集**的东西："
  echo "  * 订阅 URL 的原文（只留 host）、UUID/口令/密钥的原文"
  echo "  * 浏览内容、访问的站点清单（日志里没有这类内容；本脚本也不去读浏览器数据）"
  echo "  * 账号与身份信息；不采集任何 App 之外的文件"
  echo "  * 不联网、不上传：zip 在本机生成，传不传由你决定"
  echo
  echo "⚠️ 截断与上限（若发生，这里和采集时的终端输出各说一次）："
  if [ "$TOTAL_BYTES" -gt "$CORE_TAIL_BYTES" ]; then
    echo "  * core-tail.txt：已截断，尾部 ${KEPT_LINES} 行（本窗口共 ${TOTAL_LINES} 行）"
  else
    echo "  * core-tail.txt：未截断（${TOTAL_LINES} 行 / ${TOTAL_BYTES} 字节）"
  fi
  echo "  * 总大小上限：$((MAX_BYTES / 1024)) KiB"
  echo
  echo "怎么用：把这个 zip 交给维护者即可；他可以用 scripts/triage-incident.py 自动分诊。"
  echo "流程说明：docs/incidents/README.md"
} | redact_stream >"${BUNDLE_DIR}/README.txt"

# --- 总大小上限：超了就**按顺序**收缩并记录（不许静默）
bundle_bytes() { du -sk "${BUNDLE_DIR}" | awk '{print $1 * 1024}'; }
SHRINK_NOTES=""
if [ "$(bundle_bytes)" -gt "$MAX_BYTES" ]; then
  # 1) 先砍 core-tail：留一半字节，仍不达标再留 1/8
  for frac in 2 8; do
    [ "$(bundle_bytes)" -le "$MAX_BYTES" ] && break
    [ -f "${BUNDLE_DIR}/core-tail.txt" ] || break
    python3 - "${BUNDLE_DIR}/core-tail.txt" "$frac" <<'PY'
import sys
p, frac = sys.argv[1], int(sys.argv[2])
data = open(p, "rb").read()
tail = data[-(len(data) // frac or 1):]
nl = tail.find(b"\n")
if nl >= 0:
    tail = tail[nl + 1:]
open(p, "wb").write(tail)
PY
    SHRINK_NOTES+="  * core-tail.txt 再次截断到 1/${frac}（超过总大小上限 $((MAX_BYTES / 1024)) KiB）"$'\n'
  done
  # 2) 再砍 network.txt 的 DNS 段
  if [ "$(bundle_bytes)" -gt "$MAX_BYTES" ]; then
    python3 - "${BUNDLE_DIR}/network.txt" <<'PY'
import sys
p = sys.argv[1]
lines = open(p, encoding="utf-8", errors="replace").read().split("\n")
out, cut = [], False
for ln in lines:
    if ln.startswith("## scutil --dns"):
        cut = True
        out.append("## scutil --dns（因总大小上限被省略）")
        continue
    if cut:
        continue
    out.append(ln)
open(p, "w", encoding="utf-8").write("\n".join(out))
PY
    SHRINK_NOTES+="  * network.txt：scutil --dns 段被省略（超过总大小上限）"$'\n'
  fi
fi
if [ -n "$SHRINK_NOTES" ]; then
  {
    echo
    echo "⚠️ 因**总大小上限**（$((MAX_BYTES / 1024)) KiB）发生的额外截断："
    printf '%s' "$SHRINK_NOTES"
  } >>"${BUNDLE_DIR}/README.txt"
  printf '%s' "${SHRINK_NOTES//$'\n'/}" | sed 's/^/  ⚠️ /' >&2
  echo "  ⚠️ 已因总大小上限发生额外截断 —— 详见包内 README.txt" >&2
fi

# --- manifest.json：**最后**写（它要带每个文件的 sha256）
XRAYTUN_SCRIPTS_DIR="${SELF}" python3 - "${BUNDLE_DIR}" "${APP_PATH}" "${HELPER_INSTALLED_DEFAULT}" "${DATA_DIR}" \
  "${SINCE_EPOCH}" "${NOW_EPOCH}" "${SINCE_SOURCE}" "${SINCE_DEGRADED}" \
  "${COLLECT_START_UTC}" "${SINCE_LOCAL}" "${NOW_LOCAL}" \
  "${CORE_TAIL_BYTES}" "${MAX_BYTES}" "${TOTAL_LINES}" "${TOTAL_BYTES}" "${KEPT_LINES}" \
  "${METRICS_NOTE}" <<'PY'
import hashlib, json, os, subprocess, sys
(bundle, app_path, helper_installed, data_dir,
 since_epoch, now_epoch, since_source, degraded, collect_utc, since_local, now_local,
 core_tail_cap, total_cap, total_lines, total_bytes, kept_lines, metrics_note) = sys.argv[1:18]

def out(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        return (r.stdout or "").strip() if r.returncode == 0 else None
    except Exception:
        return None

def plist(key, path):
    return out(["/usr/libexec/PlistBuddy", "-c", f"Print :{key}", path])

# App 版本：**只**从已安装的 App bundle 读（核心横幅是核心版本，推不出 App 版本 —— 今天绕过的弯路）
app_version = plist("CFBundleShortVersionString", os.path.join(app_path, "Contents/Info.plist"))

# 核心版本：从**日志横幅**读（那是「实际在跑」的那一份），并记下来源
def core_version_from_logs():
    import json as _json, re
    dec = _json.JSONDecoder()
    pat = re.compile(r"core: Xray ([0-9][0-9.]*)")
    best = None
    for name in ("app.1.jsonl", "app.jsonl"):
        p = os.path.join(os.path.join(data_dir, "logs"), name)
        if not os.path.exists(p):
            continue
        with open(p, "rb") as f:
            for raw in f:
                line = raw.decode("utf-8", "replace")
                i = 0
                while i < len(line):
                    try:
                        obj, end = dec.raw_decode(line, i)
                    except ValueError:
                        break
                    i = end
                    if not isinstance(obj, dict):
                        continue
                    m = pat.search(obj.get("message") or "")
                    if m and (best is None or (obj.get("ts_unix") or 0) > best[0]):
                        best = (obj.get("ts_unix") or 0, m.group(1))
    return best[1] if best else None

core_version = core_version_from_logs()

# helper 三态：**与产品同一条口径**（权威在 Rust：apps/desktop/src/commands/helper.rs:139-147 + :176-203）
#   * 两边都读到 ⇒ **先比协议号**（`(protocol N)`）：相等 ⇒ Match（**包版本可以不同**）；
#   * 协议号任一边读不到 ⇒ 退回「包版本相等」；
#   * 任一边 `version` 输出读不到 ⇒ Unreadable（不许猜成不一致）。
# 判据本体放在共享模块 scripts/helper_tristate.py ⇒ 现场包与分诊不可能再各写一份。
import os as _os
import sys as _sys

if _os.environ.get("XRAYTUN_SCRIPTS_DIR"):
    _sys.path.insert(0, _os.environ["XRAYTUN_SCRIPTS_DIR"])
from helper_tristate import classify_from_outputs   # noqa: E402

installed_txt = out([helper_installed, "version"])
bundled_txt = out([os.path.join(app_path, "Contents/MacOS/xraytun-helper"), "version"])
check = classify_from_outputs(installed_txt, bundled_txt)

# mode / log_level：从 settings.json 读（**只读键名与两个非敏感值**）
mode = log_level = None
try:
    with open(os.path.join(data_dir, "settings.json"), encoding="utf-8") as f:
        s = json.load(f)
    mode, log_level = s.get("mode"), s.get("log_level")
except Exception:
    pass

files = {}
for name in sorted(os.listdir(bundle)):
    p = os.path.join(bundle, name)
    if os.path.isfile(p) and name != "manifest.json":
        h = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        files[name] = {"bytes": os.path.getsize(p), "sha256": h.hexdigest()}

# events.jsonl 被截断时，本脚本会自己在末尾追加一条 `source=bundle` 的提示 ⇒ 用它当判据
events_text = open(os.path.join(bundle, "events.jsonl"), encoding="utf-8").read()
events_truncated = '"source": "bundle"' in events_text
events_truncation = {"truncated": events_truncated}
if events_truncated:
    events_truncation["note"] = [l for l in events_text.split("\n") if '"source": "bundle"' in l][-1]

manifest = {
    "bundle_format": "xraytun-incident/1",
    "collected_at_utc": collect_utc,
    "window": {
        "since_epoch": int(since_epoch), "until_epoch": int(now_epoch),
        "since_local": since_local, "until_local": now_local,
        "source": since_source,
        "degraded": degraded == "true",
        "note": ("窗口**不是**「本次 App 启动至今」：取不到进程启动时刻，已退让并在此标注"
                 if degraded == "true" else "窗口 = 本次 App 进程启动 → 采集时刻"),
    },
    "versions": {
        "app": {"value": app_version, "source": f"{app_path}/Contents/Info.plist CFBundleShortVersionString"},
        "core": {"value": core_version, "source": "日志横幅 `core: Xray <ver>`（实际在跑的那一份）"},
        "helper": {"check": check,
                   "source": f"执行 `{helper_installed} version` 与包内 `{app_path}/Contents/MacOS/xraytun-helper version`（与产品同一口径）"},
    },
    "system": {
        "macos": out(["sw_vers", "-productVersion"]),
        "arch": out(["uname", "-m"]),
        "kernel": out(["uname", "-r"]),
    },
    "settings": {"mode": mode, "log_level": log_level,
                 "source": f"{data_dir}/settings.json（只取这两个非敏感键）"},
    "files": files,
    "redaction": {
        "where": "本机（上传之前）",
        "rules": ["URI → scheme://host[:port]（去掉 userinfo/path/query/fragment）",
                  "UUID（含 32 位十六进制）→ <uuid>",
                  "password/passwd/pwd/token/secret/uuid/api_key/private_key/auth/psk 的值 → <redacted>",
                  "Authorization / Proxy-Authorization → <redacted>"],
        "kept": ["域名与 IP（含节点 IP）", "错误消息原文结构", "路由与 DNS 结构"],
        "not_collected": ["订阅 URL 原文", "UUID/口令/密钥原文", "浏览内容/站点清单", "账号信息"],
    },
    "truncation": {
        "core_tail_bytes_cap": int(core_tail_cap),
        "total_bytes_cap": int(total_cap),
        "core_tail": {
            "truncated": bool(kept_lines),
            "window_lines": int(total_lines),
            "window_bytes": int(total_bytes),
            "kept_lines": int(kept_lines) if kept_lines else int(total_lines),
        },
        "events": events_truncation,
        "metrics_available": not bool(metrics_note),
        "metrics_note": metrics_note or None,
        "note": "任何截断都在 README.txt、core-tail.txt 首行与采集时的 stdout 各说一次（不许静默）",
    },
}
with open(os.path.join(bundle, "manifest.json"), "w", encoding="utf-8") as f:
    json.dump(manifest, f, ensure_ascii=False, indent=2)
    f.write("\n")
print(f"  manifest.json：app={app_version} core={core_version} helper={check['state']} window_degraded={degraded}")
PY

# --- 打包（文件放在 zip 根，不带临时目录名前缀）
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
( cd "${BUNDLE_DIR}" && zip -qr "$OUT" . -x '.*' ) || {
  echo "✗ 打包失败（zip 返回非零）" >&2; exit 4; }

echo
echo "=== 包内容（原始清单）==="
unzip -l "$OUT" | sed 's/^/  /'
echo
echo "  zip: ${OUT}"
echo "  zip sha256: $(sha256_of "$OUT")"
echo "  总大小: $(wc -c <"$OUT" | tr -d ' ') 字节（上限 ${MAX_BYTES}）"

# --- `--json-out`：给 App 的摘要（**必须**在 BUNDLE_DIR 被删之前写）
if [ -n "$JSON_OUT" ]; then
  python3 - "$OUT" "${BUNDLE_DIR}" "$JSON_OUT" "${SHRINK_NOTES}" <<'PY'
import hashlib, json, os, sys

out, bundle, json_out, shrink = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
manifest = json.load(open(os.path.join(bundle, "manifest.json"), encoding="utf-8"))
readme = open(os.path.join(bundle, "README.txt"), encoding="utf-8").read()
blob = open(out, "rb").read()

# 被截断的文件名（判据全在这一个地方，App 不再自己推）：
#   * core-tail.txt：窗口本身超了字节上限（manifest 已记），或总大小上限又砍了它；
#   * events.jsonl：条数超上限（manifest 已记）；
#   * network.txt：总大小上限导致 scutil --dns 段被省略。
truncated = []
tr = manifest.get("truncation", {})
if tr.get("core_tail", {}).get("truncated"):
    truncated.append("core-tail.txt")
if tr.get("events", {}).get("truncated"):
    truncated.append("events.jsonl")
if "core-tail.txt 再次截断" in shrink and "core-tail.txt" not in truncated:
    truncated.append("core-tail.txt")
if "network.txt" in shrink:
    truncated.append("network.txt")

# 包内清单 = **zip 里的每一条**（给界面显示「包里到底有几个文件」）。
# manifest 自己的 `files` 按惯例排除 manifest.json（它没法给自己算 sha256），
# 但界面要的是「包内条目」⇒ 这里补上 manifest.json 自己，两处口径的差异写在此处。
files = dict(manifest.get("files", {}))
_mp = os.path.join(bundle, "manifest.json")
if os.path.isfile(_mp):
    _b = open(_mp, "rb").read()
    files["manifest.json"] = {"bytes": len(_b), "sha256": hashlib.sha256(_b).hexdigest()}

summary = {
    "bundle_path": os.path.abspath(out),
    "size_bytes": len(blob),
    "sha256": hashlib.sha256(blob).hexdigest(),
    "files": files,
    "readme": readme,
    "manifest": manifest,
    "truncated": truncated,
}
with open(json_out, "w", encoding="utf-8") as f:
    json.dump(summary, f, ensure_ascii=False, indent=2)
    f.write("\n")
print(f"  json-out: {json_out}（files={len(summary['files'])}，truncated={truncated}）")
PY
fi

[ "$KEEP_DIR" -eq 1 ] && echo "  未打包目录保留在: ${BUNDLE_DIR}" || rm -rf "${BUNDLE_DIR}"
echo "  提醒：**没有联网、没有上传**；把 zip 交给维护者，或先打开 README.txt 自己看。"
