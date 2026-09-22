#!/usr/bin/env bash
#
# 构建锁：同一时刻只允许一个「会用 `CARGO_TARGET_DIR` 的进程」持有。
#
# # 为什么有这把锁（一次真实的**假红**，不是理论问题）
#
# `scripts/check.sh` 是本项目**唯一的发版前门禁**。2026-09-22 14:46 在 `6aa3b5e` 上，
# tester 独立跑它得到 **exit 1**：逐项全绿，**只在最后一步** `Doc-tests xraytun_desktop_lib`
# 红 —— `error[E0463]: can't find crate for xt_proto / tauri / xt_core / …`，
# 而那条 rustdoc 命令行确实传了这些 `--extern`。同一时刻**另一个人正在同一个
# `CARGO_TARGET_DIR` 上跑 `cargo test`**；等文件锁释放后单独跑那一步 ⇒ **EXIT=0**。
#
# ⇒ 产物没问题，**是并发把门禁弄红了**。而「门禁失败」与「产品坏了」必须能区分，
#   所以这条教训不能只写成纪律（「不要并行跑」），得变成**机制**。
#
# 权威对照：同一套检查在隔离 runner 上全绿 —— CI 35696466193 / 35694470992 / 35693699077。
#
# # 锁在哪、怎么实现
#
# * 锁 = **`mkdir` 出来的一个目录**（`mkdir` 是原子的；macOS 没有 `flock(1)`）；
# * 位置 = `${CARGO_TARGET_DIR}.lock.d` —— 挂在**被保护的那个 target dir 旁边**，
#   所以「用不同 `CARGO_TARGET_DIR` 的人」天然互不阻塞（语义正确：他们本来就不冲突）；
# * 目录里写 `owner` 文件：`PID / PPID / CMD / STARTED_EPOCH / STARTED_ISO / HOST`。
#
# # 拿不到锁时的确切行为
#
# **不静默等待**：每一轮都打印持有者（pid / 命令 / 开始时间 / 已持有时长），随后每 5 秒重复一次；
# 超过 `BUILD_LOCK_WAIT`（默认 **3600s**）仍未拿到 ⇒ **明确失败**，退出码 **75**（`EX_TEMPFAIL`），
# 并再次打印持有者信息。**绝不会「等超时后继续跑」** —— 那正是假红的成因。
#
# # 死锁自救（三种 stale 判定）
#
# 1. 持有者 `owner` 里的 pid **不存活**（`kill -0` 失败）⇒ 判定 stale，打印一行并接管；
# 2. 锁目录存在但 `owner` 缺失且持续 **>30s**（写入前的极短竞态除外）⇒ 判定 stale，接管；
# 3. 持有时间超过 `BUILD_LOCK_STALE_MAX`（默认 **14400s / 4 小时**）⇒ 打印警告并接管
#    （正常一次 `check.sh` 是 15–30 分钟，4 小时只可能是被 kill -9 或挂死）。
# 接管一律带 `stale` 字样打印，事后可对齐。
#
# # 两种用法
#
# 1) 作为库（`check.sh` 就是这么用的）：
#
#        source "$ROOT/scripts/build-lock.sh"
#        trap 'release_build_lock' EXIT INT TERM HUP
#        acquire_build_lock "scripts/check.sh"
#
# 2) 作为命令（给**手写的 cargo 命令**用，比如只想跑一套单测）：
#
#        ./scripts/build-lock.sh run -- cargo test -p xt-core --lib
#        ./scripts/build-lock.sh status          # 谁持有？持有多久？
#        ./scripts/build-lock.sh hold --seconds 5 --label demo   # 用于验证/演示
#
# 环境变量：
#   BUILD_LOCK_WAIT        拿不到锁时最长等待秒数（默认 3600；0 = 立即失败）
#   BUILD_LOCK_STALE_MAX   超过这个持有秒数判定为 stale（默认 14400）
#   BUILD_LOCK_DIR         直接指定锁目录（默认 `${CARGO_TARGET_DIR}.lock.d`）
#   BUILD_LOCK_DISABLE=1   **只用于敏感性验证**：跳过锁并打印醒目警告（默认不开）
#
set -u

# --------------------------------------------------------------------------- 库

_build_lock_dir() {
  if [ -n "${BUILD_LOCK_DIR:-}" ]; then
    printf '%s' "$BUILD_LOCK_DIR"
    return 0
  fi
  local t="${CARGO_TARGET_DIR:-}"
  if [ -z "$t" ]; then
    # 与 `scripts/check.sh` 的默认值一致：`<repo>/../.cargo-target`。
    # 否则「手写的 cargo」会锁到别处，等于没锁。
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    t="$here/../.cargo-target"
  fi
  printf '%s.lock.d' "$t"
}

_build_lock_now() { date +%s; }

_build_lock_iso() { date '+%Y-%m-%d %H:%M:%S %z'; }

# 读 owner 文件里的一个字段：_build_lock_field <file> <KEY>
_build_lock_field() {
  [ -f "$1" ] || return 0
  sed -n "s/^$2=//p" "$1" | head -1
}

_build_lock_held_seconds() { # $1 = started epoch
  local s
  s="$(_build_lock_now)"
  printf '%s' "$((s - ${1:-$s}))"
}

# 打印持有者（供等待与失败路径复用）
_build_lock_describe() {
  local dir owner pid cmd started
  dir="$1"
  owner="$dir/owner"
  pid="$(_build_lock_field "$owner" PID)"
  cmd="$(_build_lock_field "$owner" CMD)"
  started="$(_build_lock_field "$owner" STARTED_ISO)"
  if [ -z "$pid" ] && [ ! -f "$owner" ]; then
    echo "  持有者：锁目录存在但 owner 文件还没写出来（dir=${dir}）"
    return 0
  fi
  echo "  持有者：pid=${pid:-?}  命令=${cmd:-?}"
  echo "          开始于 ${started:-?}（已持有 $(_build_lock_held_seconds "$(_build_lock_field "$owner" STARTED_EPOCH)") 秒）"
  echo "          锁目录：$dir"
}

# 判断当前锁是否可以判定为 stale。返回 0 = 可接管。
_build_lock_is_stale() {
  local dir owner pid started held started_epoch
  dir="$1"
  owner="$dir/owner"
  if [ ! -f "$owner" ]; then
    # owner 还没出现：用一个短计时器区分「写入前的竞态」与「残骸」
    if [ -f "$dir/.missing-since" ]; then
      local since
      since="$(cat "$dir/.missing-since" 2>/dev/null || echo 0)"
      [ $(( $(_build_lock_now) - since )) -gt 30 ] && return 0
    else
      _build_lock_now >"$dir/.missing-since" 2>/dev/null || true
    fi
    return 1
  fi
  pid="$(_build_lock_field "$owner" PID)"
  started_epoch="$(_build_lock_field "$owner" STARTED_EPOCH)"
  if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
    echo "  ⚠️  发现 stale 锁：持有者 pid=$pid 已不存在（被 kill 过？）⇒ 接管"
    return 0
  fi
  if [ -n "$started_epoch" ]; then
    held="$(( $(_build_lock_now) - started_epoch ))"
    if [ "$held" -gt "${BUILD_LOCK_STALE_MAX:-14400}" ]; then
      echo "  ⚠️  发现 stale 锁：已持有 ${held}s > ${BUILD_LOCK_STALE_MAX:-14400}s（挂死？）⇒ 接管"
      return 0
    fi
  fi
  return 1
}

# 列出**没有持锁**的 cargo/rustc 进程（本锁挡不住它们）。
#
# 为什么需要这一段：2026-09-22 那次假红的对手是**裸 `cargo test`**（tester 与 backend-dev 各跑一条，
# 都没走本锁）。所以「锁」只能串行化**愿意用锁的**进程；对裸 cargo，我们至少要做到**说话**：
# 探测到就打印 pid + 命令（默认只警告；`BUILD_LOCK_STRICT=1` 直接失败，
# `BUILD_LOCK_FOREIGN_WAIT=<秒>` 则可视地等它们结束）。
_build_lock_foreign_cargo() {
  pgrep -fl '[c]argo|[r]ustc' 2>/dev/null | grep -v "build-lock.sh" | grep -v "verify-build-lock" || true
}

_build_lock_warn_foreign() {
  local found strict wait_s waited=0 n shown
  found="$(_build_lock_foreign_cargo)"
  [ -n "$found" ] || return 0
  strict="${BUILD_LOCK_STRICT:-0}"
  wait_s="${BUILD_LOCK_FOREIGN_WAIT:-0}"
  n="$(printf '%s\n' "$found" | grep -c .)"
  echo "  ⚠️  检测到 ${n} 个**没有持锁**的 cargo/rustc 进程 —— 本锁挡不住它们（它们不会看到这把锁）："
  # 只列前 5 个，且每行截断到 140 字符（rustc 的命令行可能上千字符，全打出来没人看）
  shown=0
  # 命令里可能有**换行**（`bash -c` 的多行脚本）⇒ 先把换行折成空格，否则一行警告会被撑成几十行
  found="$(printf '%s' "$found" | tr '\n' ' ')"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    [ "$shown" -ge 5 ] && { echo "       …还有 $((n - 5)) 个（用 pgrep -fl cargo 看全）"; break; }
    echo "       $(printf '%s' "$line" | cut -c1-140)"
    shown=$((shown + 1))
  done <<EOF
$found
EOF
  echo "      如果你在跑发版门禁，这会让门禁**假红**（实例：Doc-tests 报 E0463 can't find crate）。"
  echo "      建议：请对方改用 ./scripts/build-lock.sh run -- <cargo 命令>；或在没有并发时再跑。"
  if [ "$strict" = "1" ]; then
    echo "  ✗ BUILD_LOCK_STRICT=1：检测到未持锁的 cargo ⇒ 明确失败（退出码 75），不带着未知并发去跑门禁" >&2
    return 75
  fi
  if [ "${wait_s:-0}" -gt 0 ] 2>/dev/null; then
    while [ -n "$(_build_lock_foreign_cargo)" ] && [ "$waited" -lt "$wait_s" ]; do
      [ $(( waited % 5 )) -eq 0 ] && echo "  ⏳ 等待未持锁的 cargo 结束（已等 ${waited}s / ${wait_s}s）"
      sleep 1
      waited=$((waited + 1))
    done
    found="$(_build_lock_foreign_cargo)"
    if [ -n "$found" ]; then
      echo "  ✗ 仍有未持锁的 cargo 在跑（已等 ${waited}s）⇒ 明确失败（退出码 75）" >&2
      return 75
    fi
    echo "  ✓ 未持锁的 cargo 已结束，继续（等了 ${waited}s）"
  fi
  return 0
}

# 获取锁。$1 = 可读的命令标签（写进 owner，便于事后对齐）。
acquire_build_lock() {
  local label="${1:-?}" dir owner waited=0 max wait_s start pid
  dir="$(_build_lock_dir)"
  owner="$dir/owner"
  wait_s="${BUILD_LOCK_WAIT:-3600}"

  if [ "${BUILD_LOCK_DISABLE:-0}" = "1" ]; then
    echo "  ⚠️  BUILD_LOCK_DISABLE=1 —— **构建锁被禁用**（只用于敏感性验证；真跑请去掉这个变量）"
    BUILD_LOCK_HELD_BY_US=0
    return 0
  fi
  if [ "${BUILD_LOCK_HELD_BY_US:-0}" = "1" ]; then
    return 0 # 重入：同一进程已经持有
  fi

  mkdir -p "$(dirname "$dir")" 2>/dev/null || true
  start="$(_build_lock_now)"
  while :; do
    if mkdir "$dir" 2>/dev/null; then
      {
        echo "PID=$$"
        echo "PPID=$PPID"
        echo "CMD=$label"
        echo "STARTED_EPOCH=$(_build_lock_now)"
        echo "STARTED_ISO=$(_build_lock_iso)"
        echo "HOST=$(hostname 2>/dev/null || echo '?')"
      } >"$owner" 2>/dev/null || true
      rm -f "$dir/.missing-since" 2>/dev/null || true
      BUILD_LOCK_HELD_BY_US=1
      BUILD_LOCK_ACQUIRED_EPOCH="$(_build_lock_now)"
      echo "  🔒 已获取构建锁：pid=$$ 命令=${label} 开始=$(_build_lock_iso)（锁目录 ${dir}）"
      _build_lock_warn_foreign || return $?
      return 0
    fi

    if _build_lock_is_stale "$dir"; then
      rm -rf "$dir" 2>/dev/null || true
      continue
    fi

    if [ "$waited" -eq 0 ]; then
      if [ "$wait_s" -ge 60 ]; then
        echo "  ⏳ 等待构建锁（另一个进程正在用同一个 CARGO_TARGET_DIR；等超过 $(( wait_s / 60 )) 分钟会明确失败，不会静默继续）"
      else
        echo "  ⏳ 等待构建锁（另一个进程正在用同一个 CARGO_TARGET_DIR；等超过 ${wait_s} 秒会明确失败，不会静默继续）"
      fi
      _build_lock_describe "$dir"
      echo "     提示：只想跑一条 cargo 命令的话，用 ./scripts/build-lock.sh run -- <命令> 排队。"
    elif [ $(( waited % 5 )) -eq 0 ]; then
      echo "  ⏳ 仍在等待构建锁（已等 ${waited}s）"
    fi

    if [ "$waited" -ge "$wait_s" ]; then
      echo "  ✗ 等待构建锁超时（${waited}s ≥ BUILD_LOCK_WAIT=${wait_s}）——**不会在没有锁的情况下继续跑**" >&2
      _build_lock_describe "$dir" >&2
      return 75
    fi
    sleep 1
    waited=$(( $(_build_lock_now) - start ))
  done
}

# 释放锁：只有「确实由本进程持有」时才删，避免把别人的锁删掉。
release_build_lock() {
  local dir owner pid held
  [ "${BUILD_LOCK_HELD_BY_US:-0}" = "1" ] || return 0
  dir="$(_build_lock_dir)"
  owner="$dir/owner"
  pid="$(_build_lock_field "$owner" PID)"
  if [ "$pid" != "$$" ]; then
    echo "  ⚠️  锁的 owner pid=$pid 不是本进程（$$）——不删，留给它的主人" >&2
    BUILD_LOCK_HELD_BY_US=0
    return 0
  fi
  held="$(_build_lock_held_seconds "${BUILD_LOCK_ACQUIRED_EPOCH:-$(_build_lock_now)}")"
  rm -rf "$dir" 2>/dev/null || true
  BUILD_LOCK_HELD_BY_US=0
  echo "  🔓 已释放构建锁：pid=$$ 持有 ${held}s"
}

# --------------------------------------------------------------------------- CLI

_build_lock_usage() {
  sed -n '3,60p' "$0"
}

_build_lock_cmd_status() {
  local dir owner
  dir="$(_build_lock_dir)"
  if [ -d "$dir" ]; then
    echo "状态：**已持有**"
    _build_lock_describe "$dir"
    return 0
  fi
  echo "状态：空闲（锁目录不存在：${dir}）"
  return 1
}

_build_lock_cmd_run() {
  local wait_s=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --wait) wait_s="$2"; shift 2 ;;
      --) shift; break ;;
      *) break ;;
    esac
  done
  [ $# -gt 0 ] || { echo "run 需要一条命令，例如：run -- cargo test -p xt-core --lib" >&2; return 2; }
  [ -n "$wait_s" ] && export BUILD_LOCK_WAIT="$wait_s"
  trap 'release_build_lock' EXIT INT TERM HUP
  acquire_build_lock "build-lock.sh run: $*" || return $?
  "$@"
}

_build_lock_cmd_hold() {
  local secs=5 label="build-lock.sh hold（验证/演示用）"
  while [ $# -gt 0 ]; do
    case "$1" in
      --seconds) secs="$2"; shift 2 ;;
      --label) label="$2"; shift 2 ;;
      *) echo "未知参数：$1" >&2; return 2 ;;
    esac
  done
  trap 'release_build_lock' EXIT INT TERM HUP
  acquire_build_lock "$label" || return $?
  echo "  （持锁 ${secs}s，用于验证「另一个进程会看到谁持有」）"
  sleep "$secs"
}

# 被 source（而非执行）时只提供函数，不跑任何动作。
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  case "${1:-}" in
    run) shift; _build_lock_cmd_run "$@" ;;
    hold) shift; _build_lock_cmd_hold "$@" ;;
    status | who) _build_lock_cmd_status ;;
    -h | --help | "") _build_lock_usage ;;
    *) echo "未知子命令：$1（可用：run / hold / status / -h）" >&2; exit 2 ;;
  esac
fi
