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
# # 一把锁只管一个 target dir（**隔离 worktree 与主树不互斥**）
#
# 锁目录 = `${CARGO_TARGET_DIR}.lock.d`。所以：
#   * 主树与隔离 worktree（`scripts/wt.sh` 用的 `.cargo-target.wt/<名字>`）**各持各的锁**，
#     同时编译**不互相阻塞** —— 那是 `task-112` 刻意建立的隔离，两者产物身份互不影响；
#   * 但 `check.sh` 的 strict 判据曾经是**纯 `pgrep -fl '[c]argo|[r]ustc'`**，把系统上**任何** cargo
#     都算成外来并发 ⇒ 上面那种隔离并发会让发版门禁**空跑 75**（v0.8.36 真实发生一次，10 秒退出）；
#     而且 `pgrep -f` 匹配的是**命令行文本** ⇒ 一个「内容里写着 `cargo test …` 的 heredoc 包装进程」
#     也被当成并发构建（v0.8.37 真实发生一次）。
#   * 现在：**只对「与我们同一个 target dir（或拿不到 target dir）的未持锁编译进程」失败**，
#     跨 target dir 降级为**提示**（并打印对方的 target dir）；且看**可执行体**而不是命令行文本。
#     详细判定顺序见下面「外来编译进程（strict 判据）」一节。
#
# 环境变量：
#   BUILD_LOCK_WAIT        拿不到锁时最长等待秒数（默认 3600；0 = 立即失败）
#   BUILD_LOCK_STALE_MAX   超过这个持有秒数判定为 stale（默认 14400）
#   BUILD_LOCK_DIR         直接指定锁目录（默认 `${CARGO_TARGET_DIR}.lock.d`）
#   BUILD_LOCK_DISABLE=1   **只用于敏感性验证**：跳过锁并打印醒目警告（默认不开）
#   BUILD_LOCK_STRICT=1    发现「**与我们同一个 target dir**（或拿不到 target dir）的未持锁
#                          cargo/rustc」⇒ **明确失败（75）**；跨 target dir 只在提示里点名
#   BUILD_LOCK_FOREIGN_WAIT=<秒>  **可视地等**同一 target dir 的未持锁编译进程结束（每 5 秒一行、
#                          带已等秒数），等不到仍 75；**不等**别的 target dir（等一个与我们无关的
#                          构建没有意义）
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

# ---------------------------------------------------------------- 外来编译进程（strict 判据）
#
# # 语义（task-162 收窄）：**只对「与我们同一个 target dir 的未持锁 cargo/rustc」失败**
#
# 锁是**按 `CARGO_TARGET_DIR` 分开**的：主树用 `<target>.lock.d`，隔离 worktree 用
# `<target>.wt/<名字>.lock.d`。所以「别人的 cargo 在自己的 target dir 上跑」与我们的产物
# **根本不冲突** —— 那正是 `task-112` 刻意建立的隔离。
#
# 旧判据是**纯 `pgrep -fl '[c]argo|[r]ustc'`**，它把「系统上任何一个 cargo」都算进来，于是：
#   · 隔离 worktree 与主树并发 ⇒ 发版门禁**空跑 75**（v0.8.36 真实发生一次，10 秒退出）；
#   · `pgrep -f` 匹配的是**命令行文本** ⇒ 一个「内容里写着 `cargo test …` 的 heredoc 包装进程」
#     也被当成并发构建（v0.8.37 真实发生一次）。
#
# # 判定顺序（证据越弱，结论越保守）
#
#   1. 命令行里的 `CARGO_TARGET_DIR=<dir>` / `--target-dir <dir>` / rustc 的 `--out-dir <dir>` ⇒ 明确；
#   2. `ps eww -p <pid>` 里的 `CARGO_TARGET_DIR=<dir>`（有些环境禁止 `ps`；拿不到就走 3）；
#   3. **打开的文件**（`lsof -p <pid>`）里形如 `<target>/debug|release/…` 的路径 ⇒ 反推 target dir
#      （**不依赖 `ps`、也不依赖环境变量**，真构建必有；比读字符串慢，所以放最后一条证据里）；
#   4. 以上都没有、但**可执行体确实是** cargo/rustc ⇒ `unknown`；
#   5. **可执行体不是** cargo/rustc（`bash`/`sh`/`env`/`python3`…，只是命令行里含这个词）⇒ `wrapper`。
#
# strict=1 时：`same` 与 `unknown` ⇒ **75**（`unknown` 保守当作同一 target dir ⇒ 保住 task-109
# 的安全属性：**同一 target dir 上的未持锁并发依然明确失败**）；
# `other` 与 `wrapper` ⇒ **只提示，不失败**。
#
# `BUILD_LOCK_FOREIGN_WAIT=<秒>`：**可视地等** `same`/`unknown` 结束（每 5 秒打一行，带已等秒数）；
# **不等** `other` —— 等一个与我们无关的 target dir 上的构建没有意义。等不到仍然 75。
#
# # 测试缝（**只给 `scripts/verify-build-lock.sh` 用**；生产不设这两个变量）
#
#   BUILD_LOCK_PROC_TABLE=<文件>   每行 `pid cmdline`，替代 `pgrep -fl`
#   BUILD_LOCK_ENV_TABLE=<文件>    每行 `pid=target_dir`，替代 `ps eww`
#   BUILD_LOCK_LSOF_TABLE=<文件>   每行 `pid=<target>/debug/…`，替代 `lsof`
#
_build_lock_canon() {
  local p="${1%/}"
  if [ -n "$p" ] && [ -d "$p" ]; then (cd "$p" && pwd -P); else printf '%s' "$p"; fi
}

# 我们自己的 target dir（与 `_build_lock_dir` 同源，规范化后比较）
_build_lock_our_target_dir() {
  local t="${CARGO_TARGET_DIR:-}"
  if [ -z "$t" ]; then
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    t="$here/../.cargo-target"
  fi
  _build_lock_canon "$t"
}

_build_lock_scan_procs() {
  if [ -n "${BUILD_LOCK_PROC_TABLE:-}" ] && [ -f "${BUILD_LOCK_PROC_TABLE}" ]; then
    cat "$BUILD_LOCK_PROC_TABLE"
    return 0
  fi
  pgrep -fl '[c]argo|[r]ustc' 2>/dev/null | grep -v "build-lock.sh" | grep -v "verify-build-lock" || true
}

# 从**打开的文件**反推 target dir（不依赖 `ps` / 不依赖环境变量）：
# 真正的构建进程一定在 `<target>/debug|release/...` 下有打开的文件。
# 只在前面两条证据都拿不到时才调用（`lsof` 比读字符串慢）。
_build_lock_lsof_target_dir() {
  local pid="$1" names p td=""
  if [ -n "${BUILD_LOCK_LSOF_TABLE:-}" ] && [ -f "${BUILD_LOCK_LSOF_TABLE}" ]; then
    sed -n "s/^${pid}=//p" "$BUILD_LOCK_LSOF_TABLE" | head -1
    return 0
  fi
  command -v lsof >/dev/null 2>&1 || return 1
  names="$(lsof -p "$pid" -Fn 2>/dev/null)" || return 1
  [ -n "$names" ] || return 1
  # 取第一个形如 `.../(debug|release)/...` 的路径；不用 `| head -1`（避免上游吃 SIGPIPE）
  p="$(printf '%s\n' "$names" | sed -n 's/^n//p' | awk '/\/(debug|release)\// { if (p == "") p = $0 } END { print p }')"
  [ -n "$p" ] || return 1
  case "$p" in
    */debug/*) td="${p%%/debug/*}" ;;
    */release/*) td="${p%%/release/*}" ;;
  esac
  [ -n "$td" ] || return 1
  printf '%s' "$td"
}

# 该 pid 的环境变量 CARGO_TARGET_DIR（拿不到 ⇒ 非 0）
_build_lock_env_target_dir() {
  local pid="$1" raw
  if [ -n "${BUILD_LOCK_ENV_TABLE:-}" ] && [ -f "${BUILD_LOCK_ENV_TABLE}" ]; then
    sed -n "s/^${pid}=//p" "$BUILD_LOCK_ENV_TABLE" | head -1
    return 0
  fi
  command -v ps >/dev/null 2>&1 || return 1
  raw="$(ps eww -p "$pid" 2>/dev/null)" || return 1
  [ -n "$raw" ] || return 1
  printf '%s' "$raw" | tr ' ' '\n' | sed -n 's/^CARGO_TARGET_DIR=//p' | head -1
}

# 命令行里的 target dir 证据（cargo: --target-dir / CARGO_TARGET_DIR=…；rustc: --out-dir）
_build_lock_cmdline_target_dir() {
  local cmd="$1" td=""
  td="$(printf '%s' "$cmd" | sed -n 's/.*CARGO_TARGET_DIR=\([^ "][^ "]*\).*/\1/p' | head -1)"
  if [ -z "$td" ]; then
    td="$(printf '%s' "$cmd" | sed -n 's/.*--target-dir[= ]\([^ "][^ "]*\).*/\1/p' | head -1)"
  fi
  if [ -z "$td" ]; then
    td="$(printf '%s' "$cmd" | sed -n 's/.*--out-dir[= ]\([^ "][^ "]*\).*/\1/p' | head -1)"
  fi
  [ -n "$td" ] || return 1
  printf '%s' "$td"
}

# 把 `--out-dir <target>/debug/deps` 这类子路径折算回 target dir
_build_lock_norm_dir() {
  local d="$1"
  case "$d" in
    */debug/deps) d="${d%/debug/deps}" ;;
    */debug/*) d="${d%%/debug/*}" ;;
    */release/deps) d="${d%/release/deps}" ;;
    */release/*) d="${d%%/release/*}" ;;
  esac
  _build_lock_canon "$d"
}

# **可执行体**是不是真正的 cargo/rustc —— 看 argv[0]，**不看命令行文本**
# （这正是 v0.8.37 那次 75 的根因：`bash -c` 包装进程的命令行里写着 `cargo test`）
_build_lock_is_compiler() {
  case "$(basename "${1:-}")" in
    cargo | cargo-* | rustc | rustc-* | rustdoc) return 0 ;;
    *) return 1 ;;
  esac
}

# 分类：把扫描到的每行 `pid cmdline` 归到 same / other / unknown / wrapper
# 输出：`类别|pid|target_dir（或空）|cmd`（用 `|` 而不是 TAB —— bash 的 IFS 把 tab 当空白，连续 tab 会被折叠）
_build_lock_classify_rows() {
  local our lines line pid cmd argv0 td cand
  our="$(_build_lock_our_target_dir)"
  lines="$(_build_lock_scan_procs)"
  [ -n "$lines" ] || return 0
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    pid="${line%% *}"
    case "$pid" in
      '' | *[!0-9]*) continue ;;
    esac
    cmd="${line#* }"
    argv0="${cmd%% *}"
    if ! _build_lock_is_compiler "$argv0"; then
      printf 'wrapper|%s||%s\n' "$pid" "$cmd"
      continue
    fi
    td=""
    cand="$(_build_lock_cmdline_target_dir "$cmd" || true)"
    [ -n "$cand" ] && td="$(_build_lock_norm_dir "$cand")"
    if [ -z "$td" ]; then
      cand="$(_build_lock_env_target_dir "$pid" || true)"
      [ -n "$cand" ] && td="$(_build_lock_norm_dir "$cand")"
    fi
    if [ -z "$td" ]; then
      cand="$(_build_lock_lsof_target_dir "$pid" || true)"
      [ -n "$cand" ] && td="$(_build_lock_norm_dir "$cand")"
    fi
    if [ -z "$td" ]; then
      printf 'unknown|%s||%s\n' "$pid" "$cmd"
    elif [ "$td" = "$our" ]; then
      printf 'same|%s|%s|%s\n' "$pid" "$td" "$cmd"
    else
      printf 'other|%s|%s|%s\n' "$pid" "$td" "$cmd"
    fi
  done <<EOF
$lines
EOF
}

# 兼容旧名字（外部可能有人在用）：返回**真正的编译进程**（三类）的 pgrep 风格行
_build_lock_foreign_cargo() {
  _build_lock_classify_rows | awk -F'|' '$1 != "wrapper" { print $2 " " $4 }'
}

_build_lock_warn_foreign() {
  local rows strict wait_s waited=0
  local n_same n_unknown n_other n_wrapper
  rows="$(_build_lock_classify_rows)"
  [ -n "$rows" ] || return 0
  strict="${BUILD_LOCK_STRICT:-0}"
  wait_s="${BUILD_LOCK_FOREIGN_WAIT:-0}"
  n_same="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "same" { c++ } END { print c + 0 }')"
  n_unknown="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "unknown" { c++ } END { print c + 0 }')"
  n_other="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "other" { c++ } END { print c + 0 }')"
  n_wrapper="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "wrapper" { c++ } END { print c + 0 }')"

  echo "  ⚠️  检测到未持锁的编译进程/疑似进程（本锁挡不住它们）：同一 target dir **${n_same}** 个 ·" \
    "拿不到 target dir（**保守**）**${n_unknown}** 个 · 别的 target dir（不冲突）**${n_other}** 个 ·" \
    "命令行含 cargo 字样但不是编译进程 **${n_wrapper}** 个"
  local k n
  for k in same unknown other wrapper; do
    case "$k" in
      same) echo "       【同一 target dir】⇒ 会与我们的产物冲突：" ;;
      unknown) echo "       【拿不到它的 target dir】⇒ **保守当作同一 target dir**（安全优先）：" ;;
      other) echo "       【别的 target dir】⇒ per-target-dir 锁本来就允许（仅提示，**不**触发 75）：" ;;
      wrapper) echo "       【不是编译进程】⇒ 只是命令行里含 cargo 字样（例如 heredoc 包装）⇒ **不计入**：" ;;
    esac
    printf '%s\n' "$rows" | awk -F'|' -v k="$k" '$1 == k { if (++c <= 5) print }' |
      while IFS='|' read -r _kind pid td cmd; do
        if [ -n "$td" ]; then
          printf '         pid=%s target=%s | %s\n' "$pid" "$td" "$(printf '%s' "$cmd" | cut -c1-110)"
        else
          printf '         pid=%s | %s\n' "$pid" "$(printf '%s' "$cmd" | cut -c1-110)"
        fi
      done
    n="$(printf '%s\n' "$rows" | awk -F'|' -v k="$k" '$1 == k { c++ } END { print c + 0 }')"
    [ "$n" -gt 5 ] && echo "         …还有 $((n - 5)) 个"
  done

  local blocking=$((n_same + n_unknown))
  if [ "$blocking" -gt 0 ]; then
    echo "      同一 target dir 的并发会让门禁**假红**（实例：Doc-tests 报 E0463 can't find crate）。"
    echo "      建议：请对方改用 ./scripts/build-lock.sh run -- <cargo 命令>；或在没有并发时再跑。"
  fi

  if [ "$strict" = "1" ] && [ "$blocking" -gt 0 ]; then
    echo "  ✗ BUILD_LOCK_STRICT=1：检测到 ${blocking} 个**与我们同一个 target dir**（或拿不到 target dir）的未持锁编译进程" >&2
    echo "    ⇒ 明确失败（退出码 75），不带着未知并发去跑门禁" >&2
    return 75
  fi

  if [ "${wait_s:-0}" -gt 0 ] 2>/dev/null && [ "$blocking" -gt 0 ]; then
    while [ "$blocking" -gt 0 ] && [ "$waited" -lt "$wait_s" ]; do
      [ $((waited % 5)) -eq 0 ] && echo "  ⏳ 等待**同一 target dir** 的未持锁编译进程结束（已等 ${waited}s / ${wait_s}s；别的 target dir 不等）"
      sleep 1
      waited=$((waited + 1))
      rows="$(_build_lock_classify_rows)"
      n_same="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "same" { c++ } END { print c + 0 }')"
      n_unknown="$(printf '%s\n' "$rows" | awk -F'|' '$1 == "unknown" { c++ } END { print c + 0 }')"
      blocking=$((n_same + n_unknown))
    done
    if [ "$blocking" -gt 0 ]; then
      echo "  ✗ 仍有 ${blocking} 个同一 target dir 的未持锁编译进程在跑（已等 ${waited}s）⇒ 明确失败（退出码 75）" >&2
      return 75
    fi
    echo "  ✓ 同一 target dir 的未持锁编译进程已结束，继续（等了 ${waited}s）"
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
