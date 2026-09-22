#!/usr/bin/env bash
#
# worktree 辅助：**每个 worktree 用自己的 `CARGO_TARGET_DIR`**。
#
# # 为什么需要它（一次真实的「不可能的编译错误」）
#
# 2026-09-22，`task-110` 实测：
#
#     error[E0425]: cannot find type `TailLogStats` in module `xt_core::store`   ← 源码里明明有
#
# 排查结论（他已用实验坐实）：
# * `/tmp/wt-lock109` 那个 worktree 在 `6144d3c`（**早于** `d0b5e39`），其 `store.rs` 里
#   `tail_logs_with_stats` 出现 **0 次**；
# * 那次构建用的却是**主工作区的** `CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target`
#   （从 `pgrep` 的完整命令行看到）；
# * ⇒ desktop 构建**链到了那个 worktree 编出来的旧 `xt-core` rlib**；
# * **坐实方式**：`touch crates/xt-core/src/store.rs` 强制重编主树那份 ⇒ 立刻通过（stale artifact 假红）。
#
# 这不是「并发时序」问题（那是 `scripts/build-lock.sh` 管的），而是**产物身份**问题：
# 同一个 target dir 里装着**两个 checkout 的同名同版本 crate**（`xt-core 0.8.34` vs `xt-core 0.8.34`，
# 源码不同），谁最后写就链谁。**排队排完照样可能链错。**
#
# 后果比慢一步严重：本项目做敏感性/回退实验的**标准做法就是 worktree**
# （「把修复回退 ⇒ 测试必须变红」）。若 worktree 与主树共用一个 target dir，
# 「回退版」可能实际链到「修复版」的 rlib ⇒ 我们会得到「**回退也不红**」的假结论。
#
# 所以：**worktree 必须用自己的 target dir**。本脚本把它变成默认行为。
#
# # 用法
#
#     ./scripts/wt.sh new fix1 6144d3c        # 建 worktree，并给它的 target dir 打印出来
#     ./scripts/wt.sh run fix1 -- cargo test -p xt-core --lib
#                                             # 在该 worktree 里跑命令：**自动隔离 target dir + 走构建锁**
#     ./scripts/wt.sh env fix1                # 打印 export 行（想自己 cd 进去时用）
#     ./scripts/wt.sh check [目录]            # 守卫：共享 target dir 且 cwd 在 worktree ⇒ 警告/失败
#     ./scripts/wt.sh list                    # 现有 worktree 及其 target dir
#     ./scripts/wt.sh rm fix1                 # 删 worktree（**同时删它自己的 target dir**）
#
# 环境变量：
#   WT_DIR_ROOT       worktree 放哪（默认 `${TMPDIR:-/tmp}/xraytun-wt`）
#   WT_TARGET_ROOT    各 worktree 的 target dir 放哪（默认 `<repo>/../.cargo-target.wt`）
#   WT_STRICT=1       `check` / `run` 命中「共享 target dir」时**失败**（退出码 75）而不是只警告
#   BUILD_LOCK_*      见 scripts/build-lock.sh（`run` 会走那把锁）
#
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WT_DIR_ROOT="${WT_DIR_ROOT:-${TMPDIR:-/tmp}/xraytun-wt}"
WT_TARGET_ROOT="${WT_TARGET_ROOT:-$ROOT/../.cargo-target.wt}"
MAIN_TARGET="${MAIN_TARGET_DIR:-$ROOT/../.cargo-target}"

die() { echo "✗ $*" >&2; exit 2; }

# 主工作区路径（worktree 的公共 git 目录指向主树）
main_worktree() {
  local common
  common="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
  printf '%s' "$(dirname "$common")"
}

wt_path() { printf '%s/%s' "$WT_DIR_ROOT" "$1"; }
wt_target() { printf '%s/%s' "$WT_TARGET_ROOT" "$1"; }

# cwd 是否在「非主工作区」里（linked worktree）
in_linked_worktree() {
  local dir="${1:-$PWD}" gitdir common
  gitdir="$(git -C "$dir" rev-parse --path-format=absolute --git-dir 2>/dev/null)" || return 1
  common="$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 1
  [ "$gitdir" != "$common" ]
}

# 规范化路径（去掉 ../、符号链接）——比较 target dir 时必须做，否则 `/x/../y` 判不出来
norm() { (cd "$1" 2>/dev/null && pwd -P) || printf '%s' "$1"; }

# ------------------------------------------------------------------ 守卫
# 共享 target dir + cwd 不在主工作区 ⇒ 这是本卡要防的那件事。
cmd_check() {
  local dir="${1:-$PWD}" tgt tgt_n main_n
  tgt="${CARGO_TARGET_DIR:-$MAIN_TARGET}"
  tgt_n="$(norm "$tgt")"
  main_n="$(norm "$MAIN_TARGET")"
  if ! in_linked_worktree "$dir"; then
    echo "  ✓ 当前目录不是 linked worktree（$(norm "$dir")）——本检查只针对 worktree"
    return 0
  fi
  if [ "$tgt_n" != "$main_n" ]; then
    echo "  ✓ 在 worktree 里，且 CARGO_TARGET_DIR 与主树不同："
    echo "      cwd    = $(norm "$dir")"
    echo "      target = $tgt_n"
    return 0
  fi
  cat >&2 <<EOF
  ⚠️  **共享 CARGO_TARGET_DIR + cwd 在 worktree** —— 这正是「链到另一个 checkout 的旧 rlib」的场景：
      cwd            = $(norm "$dir")
      CARGO_TARGET_DIR = $tgt_n   （= 主工作区的 target dir）
  后果：同一个 target dir 里会同时存在两个 checkout 的同名同版本 crate，**谁最后写就链谁** ——
        可能的症状是「源码里明明有的类型却报 E0425」或更糟：**敏感性实验静默链到对面那份**。
  正确做法（二选一）：
      ./scripts/wt.sh run <name> -- <命令>          # 自动用 <repo>/.cargo-target.wt/<name>
      CARGO_TARGET_DIR=<repo>/../.cargo-target.wt/<name> <命令>   # 或自己指定一个**独立的** target dir
EOF
  [ "${WT_STRICT:-0}" = "1" ] && { echo "  ✗ WT_STRICT=1：共享 target dir ⇒ 明确失败（退出码 75）" >&2; return 75; }
  return 0
}

# ------------------------------------------------------------------ 子命令
cmd_new() {
  local name="${1:-}" ref="${2:-HEAD}"
  [ -n "$name" ] || die "用法：wt.sh new <name> [ref]"
  local dir; dir="$(wt_path "$name")"
  [ -e "$dir" ] && die "已存在：${dir}（先 wt.sh rm ${name}）"
  mkdir -p "$WT_DIR_ROOT" "$WT_TARGET_ROOT"
  git -C "$ROOT" worktree add --detach "$dir" "$ref" >/dev/null || die "worktree add 失败"
  # 让 worktree 立刻可用、又不复制大文件：node_modules 整个软链；binaries **按文件**软链
  # （`apps/desktop/binaries/` 在 worktree 里本就存在——它有 tracked 的 `.gitkeep`——
  #   所以不能整目录判断「不存在」，第一版就是这么漏掉 xray 的，结果 check.sh 去下载了一份新的）
  if [ -d "$ROOT/apps/ui/node_modules" ] && [ ! -e "$dir/apps/ui/node_modules" ]; then
    mkdir -p "$dir/apps/ui"
    ln -s "$ROOT/apps/ui/node_modules" "$dir/apps/ui/node_modules"
  fi
  if [ -d "$ROOT/apps/desktop/binaries" ]; then
    mkdir -p "$dir/apps/desktop/binaries"
    for f in "$ROOT"/apps/desktop/binaries/*; do
      [ -e "$f" ] || continue
      bn="$(basename "$f")"
      [ -e "$dir/apps/desktop/binaries/$bn" ] || ln -s "$f" "$dir/apps/desktop/binaries/$bn"
    done
  fi
  echo "  ✓ worktree: $dir （ref=$(git -C "$dir" rev-parse --short HEAD)）"
  echo "  ✓ 它的 target dir（**独立**）: $(wt_target "$name")"
  echo
  echo "  下一步二选一："
  echo "    ./scripts/wt.sh run $name -- cargo test --workspace"
  echo "    eval \"\$(./scripts/wt.sh env $name)\" && cd \"\$(./scripts/wt.sh path $name)\""
}

cmd_env() {
  local name="${1:-}"; [ -n "$name" ] || die "用法：wt.sh env <name>"
  echo "export CARGO_HOME=\"${CARGO_HOME:-$ROOT/../.cargo}\""
  echo "export CARGO_TARGET_DIR=\"$(wt_target "$name")\""
  echo "export npm_config_cache=\"${npm_config_cache:-$ROOT/../.npm-cache}\""
}

cmd_path() { local name="${1:-}"; [ -n "$name" ] || die "用法：wt.sh path <name>"; wt_path "$name"; }

cmd_rm() {
  local name="${1:-}"; [ -n "$name" ] || die "用法：wt.sh rm <name>"
  local dir; dir="$(wt_path "$name")"
  git -C "$ROOT" worktree remove --force "$dir" >/dev/null 2>&1 || true
  rm -rf "$dir" "$(wt_target "$name")"
  echo "  ✓ 已删 worktree 与它的 target dir：$dir / $(wt_target "$name")"
}

cmd_list() {
  printf '%-24s %s\n' "WORKTREE" "CARGO_TARGET_DIR"
  git -C "$ROOT" worktree list --porcelain | awk '/^worktree /{print $2}' | while read -r w; do
    if [ "$(norm "$w")" = "$(norm "$(main_worktree)")" ]; then
      printf '%-24s %s  ← 主工作区\n' "$w" "$MAIN_TARGET"
    else
      printf '%-24s %s\n' "$w" "$(wt_target "$(basename "$w")")"
    fi
  done
}

cmd_run() {
  local name="${1:-}"; shift || true
  [ $# -gt 0 ] && [ "$1" = "--" ] && shift
  [ -n "$name" ] || die "用法：wt.sh run <name> -- <命令…>"
  [ $# -gt 0 ] || die "run 需要一条命令（wt.sh run <name> -- cargo test）"
  local dir; dir="$(wt_path "$name")"
  [ -d "$dir" ] || die "没有这个 worktree：${dir}（先 wt.sh new ${name}）"
  local tgt; tgt="$(wt_target "$name")"
  mkdir -p "$tgt"
  # 自检：这里的 target dir 必须**不是**主树那个（这正是本脚本存在的理由）
  if [ "$(norm "$tgt")" = "$(norm "$MAIN_TARGET")" ]; then
    echo "  ⚠️  目标目录与主树相同（${tgt}）—— 隔离失效" >&2
    [ "${WT_STRICT:-0}" = "1" ] && { echo "  ✗ WT_STRICT=1 ⇒ 明确失败（退出码 75）" >&2; return 75; }
  fi
  echo "  ▶ 在 $dir 运行（CARGO_TARGET_DIR=$tgt ← 独立；并走构建锁）"
  ( cd "$dir" && \
      CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}" \
      CARGO_TARGET_DIR="$tgt" \
      npm_config_cache="${npm_config_cache:-$ROOT/../.npm-cache}" \
      "$ROOT/scripts/build-lock.sh" run --wait "${BUILD_LOCK_WAIT:-3600}" -- "$@" )
}

check_quiet() { WT_STRICT="${WT_STRICT:-0}" cmd_check "$PWD" ; }

cmd_help() { sed -n '3,50p' "$0"; }

case "${1:-}" in
  new) shift; cmd_new "$@" ;;
  run) shift; cmd_run "$@" ;;
  env) shift; cmd_env "$@" ;;
  path) shift; cmd_path "$@" ;;
  check) shift; cmd_check "$@" ;;
  list) shift; cmd_list "$@" ;;
  rm) shift; cmd_rm "$@" ;;
  -h | --help | "") cmd_help ;;
  *) die "未知子命令：$1（可用：new / run / env / path / check / list / rm）" ;;
esac
