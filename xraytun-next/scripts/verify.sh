#!/usr/bin/env bash
# xraytun-next · 一条命令跑完全部验收
#
# 为什么要这个脚本：验收步骤一旦散落在好几份文档里，就会出现"某一步其实没跑"的情况。
# 这个脚本把每一步的**命令与退出码**都打出来，跑完给一张汇总表 —— 有哪一步没过，
# 一眼就能看见，而不是靠人记得。
#
# 用法：
#   bash scripts/verify.sh              # 全量（含真 xray 端到端与 UI）
#   bash scripts/verify.sh --fast       # 跳过 E2E 与 UI（只跑守卫 + clippy + 单测）
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

# 环境和真 xray 都**不写死绝对路径**：公开仓库里不该有某台机器的布局。
# 顺序是「环境变量 → 本机开发目录（存在才用）→ 系统默认」。
export RUSTUP_HOME="${RUSTUP_HOME:-$( [ -d "$ROOT/../.rustup" ] && echo "$ROOT/../.rustup" || echo "$HOME/.rustup" )}"
export CARGO_HOME="${CARGO_HOME:-$( [ -d "$ROOT/../.cargo" ] && echo "$ROOT/../.cargo" || echo "$HOME/.cargo" )}"
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

resolve_xray() {
  if [ -n "${XT_XRAY_BIN:-}" ]; then printf '%s' "$XT_XRAY_BIN"; return; fi
  if [ -x "$ROOT/../.scratch/bin/xray" ]; then printf '%s' "$ROOT/../.scratch/bin/xray"; return; fi
  command -v xray 2>/dev/null || printf ''
}
export XT_XRAY_BIN="$(resolve_xray)"

declare -a NAMES=()
declare -a RESULTS=()
overall=0

run_stage() {
  local name="$1"; shift
  printf '\n=== %s ===\n$ %s\n' "$name" "$*"
  if "$@"; then
    NAMES+=("$name"); RESULTS+=("PASS")
  else
    local rc=$?
    NAMES+=("$name"); RESULTS+=("FAIL(rc=$rc)")
    overall=1
  fi
}

cd "$ROOT"

run_stage "guard（五条不变量的机器判据）" bash scripts/guard.sh
run_stage "clippy（--workspace --all-targets -D warnings）" \
  cargo clippy --workspace --all-targets -- -D warnings
run_stage "cargo test（全 workspace）" cargo test --workspace

if [ "$FAST" -eq 0 ]; then
  if [ -x "$XT_XRAY_BIN" ]; then
    run_stage "E2E：真 xray 进程 + 真字节（e2e_real_xray）" \
      cargo test -p xt-daemon --test e2e_real_xray -- --nocapture
  else
    NAMES+=("E2E：真 xray 进程 + 真字节"); RESULTS+=("SKIP(没有 $XT_XRAY_BIN)")
  fi

  if [ -d apps/ui/node_modules ]; then
    run_stage "UI：tsc --noEmit" bash -c 'cd apps/ui && npx tsc --noEmit'
    run_stage "UI：vitest" bash -c 'cd apps/ui && npm test'
    run_stage "UI：vite build" bash -c 'cd apps/ui && npm run build'
  else
    NAMES+=("UI：tsc / vitest / build"); RESULTS+=("SKIP(未 npm install)")
  fi
fi

printf '\n================ 验收汇总 ================\n'
for i in "${!NAMES[@]}"; do
  printf '%-10s %s\n' "${RESULTS[$i]}" "${NAMES[$i]}"
done
printf '==========================================\n'
if [ "$overall" -eq 0 ]; then
  echo "全部通过"
else
  echo "存在未通过项"
fi
exit "$overall"
