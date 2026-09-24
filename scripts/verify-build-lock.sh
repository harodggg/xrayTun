#!/usr/bin/env bash
#
# 验证 `scripts/build-lock.sh` / `scripts/check.sh` 的构建锁**真的在挡并发**。
#
# 这是 task-109 的「可跑的小验证」：一次真实的假红（并发 cargo 共用 `CARGO_TARGET_DIR`
# ⇒ `Doc-tests` 报 `E0463: can't find crate`）之后，把纪律变成机制，并且**机制本身可验证**。
#
# 用法：
#     ./scripts/verify-build-lock.sh            # 全部验证（绿 = 锁生效）
#     ./scripts/verify-build-lock.sh --sensitivity
#         # 双向敏感性：**把锁拿掉**（生成一份去掉 acquire 的 check.sh 副本；CLI 侧用
#         # BUILD_LOCK_DISABLE=1），同一批断言**必须变红**。红说明「验证真的在验锁」，
#         # 而不是永远绿。
#
# 只读仓库（除了自己在 `scripts/` 下生成一个临时副本并在结束时删掉），不联网，不跑 cargo。

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LOCK_SH="$ROOT/scripts/build-lock.sh"
CHECK_SH="$ROOT/scripts/check.sh"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/../.cargo-target}"

MODE="locked"
[ "${1:-}" = "--sensitivity" ] && MODE="unlocked"

TMP="$(mktemp -d)"
NOLOCK=""
cleanup() {
  rm -rf "$TMP" 2>/dev/null || true
  [ -n "$NOLOCK" ] && rm -f "$NOLOCK" 2>/dev/null
  # 兜底：不要把自己的锁留在系统里
  source "$LOCK_SH" 2>/dev/null || true
  if [ -d "$(_build_lock_dir 2>/dev/null)" ]; then
    local p
    p="$(sed -n 's/^PID=//p' "$(_build_lock_dir)/owner" 2>/dev/null | head -1)"
    # 只清理「已经不可能活着」的锁，避免打扰别人
    if [ -n "$p" ] && ! kill -0 "$p" 2>/dev/null; then rm -rf "$(_build_lock_dir)"; fi
  fi
}
trap cleanup EXIT INT TERM

pass=0
fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

lock_dir() {
  ( export CARGO_TARGET_DIR; source "$LOCK_SH"; _build_lock_dir )
}
wait_for_lock() {
  local i
  for i in $(seq 1 60); do [ -d "$(lock_dir)" ] && return 0; sleep 0.1; done
  return 1
}

echo "构建锁验证：MODE=$MODE"
echo "  CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
echo "  锁目录=$(lock_dir)"

# ------------------------------------------------------------------ 1) 持锁 + 第二个进程
hdr "[1] 一个进程持锁时，第二个必须**等待并打印持有者**（不是两个一起跑）"
HOLD_SECONDS=12
"$LOCK_SH" hold --seconds "$HOLD_SECONDS" --label "verify-build-lock [1] 持有者" >"$TMP/hold1.log" 2>&1 &
HOLD_PID=$!
if ! wait_for_lock; then
  no "拿不到锁目录：持有进程没起来（$(cat "$TMP/hold1.log")）"
else
  ok "持有者已持锁（pid=${HOLD_PID}）"
  if [ "$MODE" = "locked" ]; then
    SECOND_OUT="$(BUILD_LOCK_WAIT=2 "$LOCK_SH" run --wait 2 -- echo SHOULD_NOT_RUN 2>&1)"
    SECOND_RC=$?
    echo "     第二个进程输出："; printf '%s\n' "$SECOND_OUT" | sed 's/^/       /'
    echo "     第二个进程退出码：${SECOND_RC}（期望 75）"
    case "$SECOND_OUT" in
      *"等待构建锁"*) ok "第二个进程打印了「等待构建锁」" ;;
      *) no "第二个进程没有打印等待信息 —— 锁没生效？" ;;
    esac
    case "$SECOND_OUT" in
      *"pid=$HOLD_PID"*) ok "第二个进程点名了持有者 pid=$HOLD_PID" ;;
      *) no "第二个进程没有点名持有者 pid=$HOLD_PID" ;;
    esac
    case "$SECOND_OUT" in
      *SHOULD_NOT_RUN*) no "第二个进程**在持锁期间**跑起来了（互斥失效）" ;;
      *) ok "第二个进程没有在持锁期间执行命令" ;;
    esac
    [ "$SECOND_RC" = "75" ] && ok "超时退出码 = 75（EX_TEMPFAIL，明确失败）" || no "超时退出码 = ${SECOND_RC}（期望 75）"
  else
    # 敏感性：锁被拿掉 ⇒ 第二个进程应当**立刻跑起来**（= 验证必须判红）
    SECOND_OUT="$(BUILD_LOCK_WAIT=2 BUILD_LOCK_DISABLE=1 "$LOCK_SH" run --wait 2 -- echo RAN_WITHOUT_LOCK 2>&1)"
    SECOND_RC=$?
    case "$SECOND_OUT" in
      *RAN_WITHOUT_LOCK*) no "（敏感性/预期的红）锁被禁用后，第二个进程**确实一起跑了** —— 这正是要证明的" ;;
      *) no "（敏感性）锁被禁用后第二个进程仍没跑起来？" ;;
    esac
    [ "$SECOND_RC" = "0" ] && no "（敏感性/预期的红）退出码 0（没有互斥）" || no "（敏感性）退出码 $SECOND_RC"
  fi
fi

# ------------------------------------------------------------------ 2) 真实 check.sh 挡在第一步之前
hdr "[2] 持锁时再启动一个真的 \`check.sh\`：必须在**任何一步之前**停住"
if [ -d "$(lock_dir)" ] || wait_for_lock; then
  if [ "$MODE" = "locked" ]; then
    CHK_TARGET="$CHECK_SH"
    EXTRA_ENV=""
  else
    # 敏感性：把 check.sh 里那三行锁代码拿掉（字面意义上「把锁去掉」）
    NOLOCK="$ROOT/scripts/.check-nolock-$$.sh"
    grep -v -e 'source "$ROOT/scripts/build-lock.sh"' \
            -e "trap 'release_build_lock'" \
            -e 'acquire_build_lock "scripts/check.sh' "$CHECK_SH" >"$NOLOCK"
    chmod +x "$NOLOCK"
    CHK_TARGET="$NOLOCK"
    EXTRA_ENV="BUILD_LOCK_DISABLE=1"
    echo "     （敏感性）已生成去掉锁的副本：$(basename "$NOLOCK")（$(grep -c acquire_build_lock "$NOLOCK") 处 acquire）"
  fi
  if [ "$MODE" = "locked" ]; then
    env BUILD_LOCK_WAIT=2 "$CHK_TARGET" --no-release-build >"$TMP/chk2.log" 2>&1
    CHK_RC=$?
  else
    # 敏感性：后台跑，**一看见它越过锁就立刻 kill** —— 证明「锁没了就真的会一起跑」，
    # 同时不让它真的去并发编译（生产机上还有别人在跑 cargo）。
    env BUILD_LOCK_DISABLE=1 BUILD_LOCK_WAIT=2 "$CHK_TARGET" --no-release-build >"$TMP/chk2.log" 2>&1 &
    STRIPPED_PID=$!
    for i in $(seq 1 60); do
      grep -q "取 Xray 核心" "$TMP/chk2.log" 2>/dev/null && break
      kill -0 "$STRIPPED_PID" 2>/dev/null || break
      sleep 0.1
    done
    kill -TERM "$STRIPPED_PID" 2>/dev/null
    wait "$STRIPPED_PID" 2>/dev/null
    CHK_RC=999
    echo "     （敏感性）检测到越锁后立即 kill（pid=${STRIPPED_PID}），不让它真的并发编译"
  fi
  echo "     check.sh 退出码：$CHK_RC"; sed -n '1,12p' "$TMP/chk2.log" | sed 's/^/       /'
  case "$(cat "$TMP/chk2.log")" in
    *"取 Xray 核心"*)
      if [ "$MODE" = "locked" ]; then
        no "check.sh **越过锁**开始跑了（输出里出现第一步「取 Xray 核心」）"
      else
        no "（敏感性/预期的红）去掉锁后 check.sh **越过锁**开始跑 —— 验证确实在验锁"
      fi
      ;;
    *)
      if [ "$MODE" = "locked" ]; then
        ok "check.sh 没有越过锁（未出现第一步）"
      else
        no "（敏感性）去掉锁后 check.sh 仍停住了？那说明另有阻塞"
      fi
      ;;
  esac
  case "$(cat "$TMP/chk2.log")" in
    *"等待构建锁"*)
      [ "$MODE" = "locked" ] && ok "check.sh 打印了持有者信息并等待" || no "（敏感性）副本不该打印等待信息"
      ;;
    *)
      [ "$MODE" = "locked" ] && no "check.sh 没有打印等待信息" || no "（敏感性/预期的红）副本没有等待（无锁）"
      ;;
  esac
  [ "$MODE" = "locked" ] && { [ "$CHK_RC" = "75" ] && ok "check.sh 退出码 75" || no "check.sh 退出码 ${CHK_RC}（期望 75）"; }
else
  no "持有者没能持锁（第 2 项无法验证）"
fi

wait "$HOLD_PID" 2>/dev/null

# ------------------------------------------------------------------ 3) 互斥的时序证据
hdr "[3] 两个 CLI 排队：执行区间**不许重叠**"
F="$TMP/intervals.txt"
: >"$F"
run_one() { "$LOCK_SH" run -- sh -c "date +%s.%N >> '$F'; echo start >> '$F'; sleep 2; date +%s.%N >> '$F'; echo end >> '$F'"; }
if [ "$MODE" = "locked" ]; then
  run_one >"$TMP/r1.log" 2>&1 &
  run_one >"$TMP/r2.log" 2>&1 &
  wait
else
  BUILD_LOCK_DISABLE=1 run_one >"$TMP/r1.log" 2>&1 &
  BUILD_LOCK_DISABLE=1 run_one >"$TMP/r2.log" 2>&1 &
  wait
fi
echo "     时序文件（毫秒级）："; sed 's/^/       /' "$F"
overlap="$(python3 - "$F" <<'PY'
import sys
lines=[l.strip() for l in open(sys.argv[1]) if l.strip()]
times=[]
build=[]
for l in lines:
    if l in ("start","end"): build.append(l)
    else: times.append(float(l))
# times = [s1,e1,s2,e2]（交错时也一样）⇒ 用 start/end 配对判定重叠
ev=[]
ti=iter(times)
for b in build:
    ev.append((b, next(ti)))
intervals=[]
stack=[]
for kind,t in ev:
    if kind=="start": stack.append(t)
    else:
        if stack: intervals.append((stack.pop(), t))
intervals.sort()
bad=False
for i in range(1,len(intervals)):
    if intervals[i][0] < intervals[i-1][1] - 0.05:   # 允许 50ms 抖动
        bad=True
print("OVERLAP" if bad else "SERIAL")
PY
)"
if [ "$overlap" = "SERIAL" ]; then
  [ "$MODE" = "locked" ] && ok "两个 CLI 的区间**串行**（锁生效）" || no "（敏感性）期望重叠，实际串行？"
else
  [ "$MODE" = "locked" ] && no "两个 CLI 的区间**重叠**（互斥失效）" || no "（敏感性/预期的红）去掉锁后区间**重叠** —— 验证确实在验锁"
fi

# ------------------------------------------------------------------ 4) stale 自救
hdr "[4] stale 锁必须能自救（持有者已被 kill -9）"
if [ "$MODE" = "locked" ]; then
  LD="$(lock_dir)"
  rm -rf "$LD"; mkdir -p "$LD"
  { echo "PID=999999"; echo "PPID=1"; echo "CMD=已被 kill -9 的假持有者"; echo "STARTED_EPOCH=$(date +%s)"; echo "STARTED_ISO=$(date '+%Y-%m-%d %H:%M:%S %z')"; echo "HOST=fake"; } >"$LD/owner"
  STALE_OUT="$("$LOCK_SH" run -- echo HEALED 2>&1)"
  echo "$STALE_OUT" | sed 's/^/       /'
  case "$STALE_OUT" in
    *stale*) ok "识别出 stale 锁并接管" ;;
    *) no "没有识别 stale 锁（可能永久死锁）" ;;
  esac
  case "$STALE_OUT" in
    *HEALED*) ok "接管后命令正常执行" ;;
    *) no "接管后命令没有执行" ;;
  esac
else
  echo "     （敏感性模式下跳过：stale 自救验证的是锁自身，禁用后无意义）"
fi

# ------------------------------------------------------------------ 5) strict 判据收窄（task-162）
hdr "[5] strict 判据：只对**同一个 target dir** 的未持锁编译进程失败（task-162）"

# 夹具：本仓库的 target dir（同样的规范化路径），以及三类"进程表"
OUR_TD="$( ( export CARGO_TARGET_DIR; source "$LOCK_SH"; _build_lock_our_target_dir ) )"
PT_REAL="$TMP/proc-real"; PT_WRAP="$TMP/proc-wrap"
ENV_OTHER="$TMP/env-other"; ENV_SAME="$TMP/env-same"; LSOF_OTHER="$TMP/lsof-other"
printf '93885 /usr/local/bin/cargo clippy --workspace --all-targets\n' >"$PT_REAL"
printf '97564 /bin/bash -c cd repo && cargo test --workspace\n' >"$PT_WRAP"
printf '93885=/tmp/some-other-target\n' >"$ENV_OTHER"
printf '93885=%s\n' "$OUR_TD" >"$ENV_SAME"
printf '93885=/tmp/some-other-target/debug/deps/libfoo.rlib\n' >"$LSOF_OTHER"
echo "     我们的 target dir（规范化）=$OUR_TD"

# --sensitivity：把两条机制各自拿掉（**后定义覆盖前定义**）
MUT_LOCK="$TMP/build-lock-$MODE.sh"
cp "$LOCK_SH" "$MUT_LOCK"
if [ "$MODE" = "unlocked" ]; then
  {
    echo ''
    echo '# MUT-A：可执行体判定恒真（≡ 回到「匹配命令行文本」的旧行为）'
    echo '_build_lock_is_compiler() { return 0; }'
    echo '# MUT-B：分类恒为 same（≡ 完全不看 target dir）'
    echo '_build_lock_classify_rows() { _build_lock_scan_procs | while IFS= read -r l; do [ -n "$l" ] || continue; printf "same|%s||%s\n" "${l%% *}" "${l#* }"; done; }'
  } >>"$MUT_LOCK"
  echo "     （敏感性模式：用突变副本 —— MUT-A 不看可执行体、MUT-B 不看 target dir）"
fi

# 跑一次判据：$1 = 描述，其余为 env 赋值；输出 -> OUT，退出码 -> RC
strict_case() {
  local desc="$1"; shift
  OUT="$(env "$@" BUILD_LOCK_STRICT=1 CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    bash -c 'source "$1"; _build_lock_warn_foreign' _ "$MUT_LOCK" 2>&1)"
  RC=$?
  echo "     [$desc] RC=$RC"
  printf '%s\n' "$OUT" | sed -n '1,2p' | sed 's/^/       /'
}

# 5a 跨 target dir（环境变量证据）⇒ 不得 75
strict_case "5a 跨 target dir（ENV 证据）" BUILD_LOCK_PROC_TABLE="$PT_REAL" BUILD_LOCK_ENV_TABLE="$ENV_OTHER"
[ "$RC" = "0" ] && ok "5a 跨 target dir 的未持锁 cargo ⇒ **不** 75（只提示）" || no "5a 跨 target dir 被判 ${RC}（期望 0）"
case "$OUT" in *"别的 target dir"*) ok "5a 提示里点名了「别的 target dir」（含对方 target dir）" ;; *) no "5a 没有点名「别的 target dir」" ;; esac

# 5b **同一** target dir ⇒ 必须 75（task-109 的安全属性）
strict_case "5b 同一 target dir（ENV 证据）" BUILD_LOCK_PROC_TABLE="$PT_REAL" BUILD_LOCK_ENV_TABLE="$ENV_SAME"
[ "$RC" = "75" ] && ok "5b 同一 target dir 的未持锁 cargo ⇒ **75**（安全属性保留）" || no "5b 同一 target dir 只给了 ${RC}（期望 75）"
case "$OUT" in *"同一 target dir"*) ok "5b 报错点名了「同一 target dir」" ;; *) no "5b 没有点名「同一 target dir」" ;; esac

# 5c 跨 target dir，但**没有环境变量证据**（只有 lsof 打开文件证据）⇒ 仍然不得 75
strict_case "5c 跨 target dir（仅 lsof 证据）" BUILD_LOCK_PROC_TABLE="$PT_REAL" BUILD_LOCK_LSOF_TABLE="$LSOF_OTHER"
[ "$RC" = "0" ] && ok "5c 不依赖 \`ps\`/环境变量也能判出「别的 target dir」⇒ 不 75" || no "5c 只靠 lsof 证据时被判 ${RC}（期望 0）"

# 5d heredoc 包装进程（**命令行里含 cargo 字样，可执行体是 bash**）⇒ 不得 75
strict_case "5d heredoc 包装进程" BUILD_LOCK_PROC_TABLE="$PT_WRAP"
[ "$RC" = "0" ] && ok "5d 命令行含 cargo 字样但不是编译进程 ⇒ **不** 75（v0.8.37 那次 75 的形状）" || no "5d 包装进程被判 ${RC}（期望 0）"
case "$OUT" in *"不是编译进程"*) ok "5d 提示里说明了「不是编译进程 ⇒ 不计入」" ;; *) no "5d 没有说明为什么不计入" ;; esac

# 5e 真 cargo，但任何证据都拿不到 ⇒ **保守**当作同一 target dir ⇒ 75
strict_case "5e 真 cargo 但拿不到 target dir" BUILD_LOCK_PROC_TABLE="$PT_REAL"
[ "$RC" = "75" ] && ok "5e 拿不到 target dir ⇒ 保守 75（安全优先，诚实清单里写明）" || no "5e 拿不到 target dir 却给了 ${RC}（期望 75）"
case "$OUT" in *"保守"*) ok "5e 报错说明了「保守当作同一 target dir」" ;; *) no "5e 没有说明保守策略" ;; esac

# ------------------------------------------------------------------ 6) 收尾无残留
hdr "[6] 结束后锁必须已释放（无残留）"
if [ -d "$(lock_dir)" ]; then no "锁目录仍然存在：$(lock_dir)"; else ok "锁目录已清理"; fi

hdr "结果"
echo "  pass=$pass fail=$fail"
if [ "$MODE" = "locked" ]; then
  [ "$fail" = "0" ] && { echo "  ✓ 构建锁验证通过：并发被挡住、stale 能自救、无残留"; exit 0; }
  echo "  ✗ 构建锁验证失败（锁没生效）"; exit 1
else
  # 敏感性模式：必须出现「红」（说明上面那些断言真的依赖锁）
  if [ "$fail" -gt 0 ]; then
    echo "  ✓ 敏感性成立：把锁拿掉后，同一批断言出现 **$fail 条红** ⇒ 验证真的在验锁"
    exit 0
  fi
  echo "  ✗ 敏感性不成立：把锁拿掉后断言**仍然全绿** ⇒ 这些断言没有在验锁"; exit 1
fi
