#!/bin/sh
# App 启动烟测：证明「双击那条路」不会一启动就崩，且**绝不动用户的系统网络**。
#
# ```bash
# ./scripts/smoke-app-startup.sh --app <path/to/XrayTun.app> --mode open --runs 3   # ≈ 用户双击
# ./scripts/smoke-app-startup.sh --app <path/to/XrayTun.app> --mode direct --runs 3 # 直接 exec
# ```
#
# # 为什么需要它（门禁盲区）
#
# `scripts/check.sh` 的每一步（clippy / `cargo test --workspace` / release 构建）
# **都不会启动 App** ⇒ 0.8.39 的「双击即 SIGABRT」可以全程绿灯。真机 panic.log
# 给出 `apps/desktop/src/lib.rs:178:17` + `there is no reactor running…`：
# Tauri 的 `setup` 回调不在 tokio runtime context 里，裸 `tokio::spawn` 直接 panic，
# `panic = "abort"` ⇒ SIGABRT。静态守卫（`naked_tokio_spawn` 两条测试）抓这一类；
# 本脚本抓**所有**启动期崩溃（不限于已知形态）。
#
# # 只有 `open` 是忠实的「双击」路径
#
# | 模式 | 怎么启动 | 数据目录 | 结论 |
# |---|---|---|---|
# | `open` | `open -n -a <真实 .app>` | **用户真实数据目录** | 忠实（tester 修复前 3/3 复现）；**前提检查**不通过就拒绝 |
# | `direct` | 直接 exec 二进制 | `XRAYTUN_DATA_DIR` 隔离目录 | 安全，但受限终端里可能**静默挂起**（进程活着、零输出）⇒ 记 75 而不是绿 |
#
# ⚠️ 还试过第三种「wrapper .app」（临时 .app 用脚本设好 env 再 exec 真实二进制）：
# 实测**它和 `direct` 一样静默挂起、不复现崩溃**（修复前的 tester 构建跑 3/3 都没崩、
# 零输出）⇒ **不能拿它当崩溃判据**，故不提供该模式。
#
# 三条实测教训（tester 在 `docs/verification/UX-FINDINGS-REVERIFY.md` 附录 A 留档）：
# 1. **`open -n --env XRAYTUN_DATA_DIR=…` 的环境变量不会传进 App**（panic.log 仍落真实目录）；
#    所以 `open` 阶段**不受隔离保护**，必须先做前提检查；
# 2. **修复后 App 会真的启动**：真实 settings 若 `was_connected=true` 且 `mode != direct`，
#    它就会自动重连、接管默认路由 ⇒ 前提检查不通过时**必须拒绝跑 `open` 模式**；
# 3. 受限终端里直接 exec 可能**静默挂起**（进程活着、一行日志都没有）⇒
#    「活着」不足以判通过：必须有**启动证据**（输出或数据目录日志），否则记 75 无法判定。
#
# 隔离目录里的 `settings.json` 写死 `mode=direct` + `was_connected=false` + `auto_reconnect=false`
# （`commands/core.rs::should_auto_reconnect` 要求 `was_connected && auto_reconnect && mode != Direct`
# ⇒ 恒 false）——即使这份 JSON 解析失败，App 退化成默认 settings 同样不会自动重连。
#
# # 判据（每次运行）
#
#   * `--alive` 秒（默认 10）后**仍在运行**（`direct` 用 PID；`open` 用 `pgrep -f <二进制>`）；
#   * 隔离目录里**没有** `logs/panic.log`、输出里没有 `panicked at`（`direct` 模式）；
#   * **有启动证据**（stdout/stderr 非空 或数据目录里写下了日志）——
#     只看「进程活着」会把静默挂起误判成通过；
#   * **`NEW_IPS=0`**：`~/Library/Logs/DiagnosticReports` 里 `xraytun-desktop-*.ips` 不新增；
#   * **真实数据目录的 `logs/panic.log` 字节数不增长**（`open` 模式的主要判据）；
#   * **默认路由与 utun 数与基线相同**（没碰系统网络）。
#
# # 退出码（与仓库约定一致）
#
#   0  通过  · 1  **产品失败**（崩溃 / panic / IPS 新增 / 真实 panic.log 增长 / 网络被动过）
#   75 **无法判定**（没有 .app、`open` 前提检查不通过、静默挂起、没有启动证据等）——不是「绿」
#
# 证据（每次运行的 stdout/stderr 与隔离数据目录）留在打印出来的临时目录里。
set -eu

APP=""
MODE="direct"
RUNS=3
ALIVE=10

while [ $# -gt 0 ]; do
  case "$1" in
    --app)   APP="${2:-}";   shift 2 ;;
    --mode)  MODE="${2:-}";  shift 2 ;;
    --runs)  RUNS="${2:-}";  shift 2 ;;
    --alive) ALIVE="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 75 ;;
  esac
done

case "$MODE" in
  direct|open) ;;
  *) echo "未知 --mode：${MODE}（可用：direct / open）" >&2; exit 75 ;;
esac

if [ -z "$APP" ]; then
  echo "✗ 缺 --app <XrayTun.app>" >&2
  echo "  例：./scripts/smoke-app-startup.sh --app /path/to/XrayTun.app --mode direct --runs 3" >&2
  exit 75
fi
BIN="$APP/Contents/MacOS/xraytun-desktop"
if [ ! -x "$BIN" ]; then
  echo "✗ 找不到可执行文件：${BIN}" >&2
  exit 75
fi

REPORTS="$HOME/Library/Logs/DiagnosticReports"
REALDATA="$HOME/Library/Application Support/com.xraytun.desktop"
REAL_PANIC="$REALDATA/logs/panic.log"

default_route() { route -n get default 2>/dev/null | awk '/gateway|interface/{print}' || true; }
utun_count() { netstat -rn 2>/dev/null | grep -c utun || true; }
net_state() { printf '%s / utun=%s' "$(default_route | tr '\n' ' ')" "$(utun_count)"; }
ips_count() { ls -1 "$REPORTS" 2>/dev/null | grep -c 'xraytun-desktop.*\.ips' || true; }
real_panic_bytes() {
  if [ -f "$REAL_PANIC" ]; then stat -f%z "$REAL_PANIC" 2>/dev/null || echo 0; else echo 0; fi
}
# 进程存活判定用的 **App 路径特征**。脚本自身 argv 里没有这个字面量，所以 pgrep
# 不会命中本脚本（本仓库记过「pgrep -f 读命令行文本」的坑：描述文本也会被算进去）。
alive_procs() { pgrep -f 'XrayTun.app/Contents/MacOS/xraytun-desktop' 2>/dev/null || true; }
kill_app() {
  pkill -f 'XrayTun.app/Contents/MacOS/xraytun-desktop' 2>/dev/null || true
  sleep 1
  pkill -9 -f 'XrayTun.app/Contents/MacOS/xraytun-desktop' 2>/dev/null || true
}

# ---- open 模式的前提检查（env 传不进去 ⇒ 必须检查用户真实设置）----
if [ "$MODE" = "open" ]; then
  if [ ! -f "$REALDATA/settings.json" ]; then
    echo "⚠ 真实 settings.json 不存在：App 会用默认设置（不自动重连）⇒ 允许 open。"
  else
    SAFE="$(python3 - "$REALDATA/settings.json" <<'PY'
import json, sys
try:
    s = json.load(open(sys.argv[1], encoding="utf-8"))
except Exception:
    print("unparseable")
    raise SystemExit
mode = s.get("mode", "?")
was = s.get("was_connected", False)
# should_auto_reconnect = was_connected && auto_reconnect && mode != Direct
print("safe" if (was is not True or mode == "direct") else "unsafe:mode=%s:was_connected=%s" % (mode, was))
PY
)"
    case "$SAFE" in
      safe) echo "==> open 前提检查通过：真实 settings 不会自动重连（${SAFE}）" ;;
      unparseable)
        echo "✗ 拒绝跑 open：真实 settings.json 解析失败，无法判断它会不会自动重连。" >&2
        echo "  （解析失败时 App 会退化成默认设置；但「无法判断」不该当成安全。）" >&2
        exit 75 ;;
      *)
        echo "✗ 拒绝跑 open 模式：真实 settings 是 ${SAFE}" >&2
        echo "  修复后 App 会真的启动；was_connected=true 且 mode!=direct ⇒ 它会自动重连并接管默认路由。" >&2
        echo "  三个安全出路（任选其一）：" >&2
        echo "    1. 备份后把真实 settings 的 was_connected 置 false（或把 mode 置 direct），跑完再还原；" >&2
        echo "    2. 在无人使用网络、且可接受接管的机器上跑；" >&2
        echo "    3. 只跑 --mode direct（安全，但受限终端里可能静默挂起 ⇒ 75）。" >&2
        exit 75 ;;
    esac
  fi
fi

BEFORE_NET="$(net_state)"
BEFORE_IPS="$(ips_count)"
BEFORE_REAL_PANIC="$(real_panic_bytes)"
echo "==> 模式=${MODE}  网络基线：${BEFORE_NET}  ips=${BEFORE_IPS}  真实 panic.log=${BEFORE_REAL_PANIC} B"

EVID="$(mktemp -d "${TMPDIR:-/tmp}/xraytun-smoke.XXXXXX")"
echo "==> 证据目录：${EVID}"

fail=0
undetermined=0
i=1
while [ "$i" -le "$RUNS" ]; do
  DIR="$EVID/run$i"
  mkdir -p "$DIR/logs"
  # 安全设置：不连、不走系统代理、不自动重连。
  cat >"$DIR/settings.json" <<'JSON'
{"settings_version":1,"mode":"direct","was_connected":false,"auto_reconnect":false}
JSON

  echo "--- run ${i}/${RUNS}（${MODE}） ---"
  PID=""

  # 开跑前先清干净：否则「进程活着」可能是**上一次**或用户手动开着的实例。
  if [ "$MODE" = "open" ]; then
    if [ -n "$(alive_procs)" ]; then
      echo "  （开跑前已有 XrayTun 实例，先收掉：$(alive_procs | tr '\n' ' ')）"
      kill_app
    fi
    if [ -n "$(alive_procs)" ]; then
      echo "  ✗ 收不掉已有实例 ⇒ 无法判定本次启动" >&2
      undetermined=1
      i=$((i + 1))
      continue
    fi
    open -n -a "$APP" >"$DIR/open.log" 2>&1 || true
  else
    XRAYTUN_DATA_DIR="$DIR" XRAYTUN_LOG=debug RUST_BACKTRACE=1 \
      "$BIN" >"$DIR/stdout.log" 2>&1 &
    PID=$!
  fi

  # 等进程**出现**（LaunchServices 的 `open` 是异步的：刚 open 完 pgrep 还看不到，
  # 早先这里直接 break，把「还没起来」误判成「已经退出」）。最多等 8 秒。
  appeared=0
  if [ "$MODE" = "direct" ]; then
    appeared=1
  else
    t=0
    while [ "$t" -lt 8 ]; do
      if [ -n "$(alive_procs)" ]; then appeared=1; break; fi
      sleep 1
      t=$((t + 1))
    done
  fi

  waited=0
  if [ "$appeared" -eq 1 ]; then
    while [ "$waited" -lt "$ALIVE" ]; do
      if [ "$MODE" = "direct" ]; then
        if ! kill -0 "$PID" 2>/dev/null; then break; fi
      else
        if [ -z "$(alive_procs)" ]; then break; fi
      fi
      sleep 1
      waited=$((waited + 1))
    done
  fi

  alive=0
  if [ "$MODE" = "direct" ]; then
    if kill -0 "$PID" 2>/dev/null; then alive=1; fi
  else
    if [ -n "$(alive_procs)" ]; then alive=1; fi
  fi

  # 启动证据：App 真的走到初始化了吗？（只看「活着」会把静默挂起误判成通过）
  #
  # `open` 模式拿不到 App 的任何输出（LaunchServices 丢弃 stdout/stderr），也不该
  # 往用户真实数据目录写东西，所以这里**无法取得启动证据** ⇒ 该模式只看
  # 「活着 + 不崩 + 无 IPS + 真实 panic.log 不增长」，并在报告里写明。
  evidence=0
  evidence_na=0
  if [ "$MODE" = "open" ]; then evidence_na=1; fi
  if [ -s "$DIR/stdout.log" ]; then evidence=1; fi
  if find "$DIR/logs" -type f 2>/dev/null | grep -q .; then evidence=1; fi

  panic=0
  if [ -s "$DIR/logs/panic.log" ]; then panic=1; fi
  if grep -q "panicked at" "$DIR/stdout.log" 2>/dev/null; then panic=1; fi

  rc=0
  if [ "$alive" -eq 1 ]; then
    if [ "$MODE" = "direct" ]; then
      kill -TERM "$PID" 2>/dev/null || true
      wait "$PID" 2>/dev/null || true
    else
      kill_app
    fi
    echo "  进程存活 ${waited}s ⇒ 已收尾（未崩溃）"
  else
    if [ "$MODE" = "direct" ]; then
      if wait "$PID" 2>/dev/null; then rc=0; else rc=$?; fi
      echo "  ✗ 进程在 ${waited}s 内退出（exit=${rc}）" >&2
    elif [ "$appeared" -eq 0 ]; then
      echo "  ✗ 8 秒内没看到进程（LaunchServices/启动器没起来？看 open.log）" >&2
    else
      echo "  ✗ 进程出现后在 ${waited}s 内退出（崩溃？）" >&2
    fi
  fi

  cur_net="$(net_state)"
  cur_ips="$(ips_count)"
  cur_panic="$(real_panic_bytes)"
  new_ips=$((cur_ips - BEFORE_IPS))

  if [ "$panic" -eq 1 ]; then
    echo "  ✗ 隔离目录里出现 panic：$DIR/logs/panic.log" >&2
    sed -n '1,6p' "$DIR/logs/panic.log" 2>/dev/null >&2 || true
    fail=1
  fi
  if [ "$new_ips" -ne 0 ]; then
    echo "  ✗ NEW_IPS=${new_ips}（应 0）⇒ 这次启动产生了崩溃报告" >&2
    fail=1
  fi
  if [ "$cur_panic" -ne "$BEFORE_REAL_PANIC" ]; then
    echo "  ✗ 真实 panic.log 从 ${BEFORE_REAL_PANIC} 涨到 ${cur_panic} 字节" >&2
    fail=1
  fi
  if [ "$cur_net" != "$BEFORE_NET" ]; then
    echo "  ✗ 默认路由/utun 变了：${cur_net}（基线 ${BEFORE_NET}）—— 烟测动过系统网络" >&2
    fail=1
    kill_app # 尽力收尾，避免把用户网络留在接管态
  fi
  if [ "$fail" -eq 0 ]; then
    if [ "$alive" -eq 1 ] && { [ "$evidence" -eq 1 ] || [ "$evidence_na" -eq 1 ]; }; then
      if [ "$evidence_na" -eq 1 ]; then
        echo "  ✓ 活着；NEW_IPS=0；真实 panic.log 未增长；网络未变（open 模式拿不到 App 输出，故不判启动证据）"
      else
        echo "  ✓ 活着且有启动证据；NEW_IPS=0；真实 panic.log 未增长；网络未变"
      fi
    elif [ "$alive" -eq 1 ]; then
      echo "  ⚠ 活着但**没有任何启动证据** ⇒ 可能静默挂起（不是通过）" >&2
      undetermined=1
    else
      echo "  ⚠ 退出且没有崩溃证据 ⇒ 无法判定（环境/图形会话问题？）" >&2
      undetermined=1
    fi
  fi

  i=$((i + 1))
done

echo "==> 收尾：网络 $(net_state)（基线 ${BEFORE_NET}）  ips=$(ips_count) 真实 panic.log=$(real_panic_bytes) B"
echo "==> 证据保留在：${EVID}"

if [ "$fail" -ne 0 ]; then
  exit 1
fi
if [ "$undetermined" -ne 0 ]; then
  echo "✗ 有运行无法判定（既不是崩溃，也不是「活着且有启动证据」）" >&2
  exit 75
fi
echo "✓ ${RUNS}/${RUNS} 次（${MODE}）启动均未崩溃；NEW_IPS=0；真实 panic.log 未增长；系统网络未变"
