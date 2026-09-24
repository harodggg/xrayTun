#!/usr/bin/env bash
# 校验 `scripts/wt.sh` 的「worktree 根目录必须放在**不会被系统清理**的位置」守卫（task-185）。
#
# 事故：默认值曾是 `${TMPDIR:-/tmp}/xraytun-wt`，而本机 `TMPDIR=/var/folders/…/T/` 是 macOS 的
# 可清理临时目录。2026-09-24 16:25 实测整棵树被系统清掉 —— 三个正在编译/验证的 worktree 目录
# 整个消失（`prunable`），其中两个是队友的在途工作。失败方式是「跑到一半目录没了」，不是明确报错。
#
# 五个案子（**不建任何 worktree、不写仓库**，只调用 `wt.sh dir` 这个纯解析子命令）：
#   [1] 不设 `WT_DIR_ROOT` ⇒ 解析出的路径**不在** `${TMPDIR}` 之下（新默认 = `<repo>/../.wt`）
#   [2] 显式设成 `${TMPDIR}/…` ⇒ **必须大声警告**（且 `under_tmpdir=yes`），但退出码仍是 0
#   [3] `WT_STRICT=1` + 临时目录 ⇒ 升级为**退出 75**
#   [4] 反向敏感性：把守卫从副本里去掉 ⇒ 案子 [2] 的断言**必须变红**（证明它真的在测守卫）
#   [5] 边界值：恰好等于 `${TMPDIR}`、以及一个以 `${TMPDIR}` 为**前缀但不同目录**（`…/T2/x`）
#
# 用法：bash docs/verification/verify-wt-dir-root.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WT="$ROOT/scripts/wt.sh"
PASS=0
FAIL=0
ok()  { printf '  ✓ %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  ✗ %s\n' "$1"; FAIL=$((FAIL + 1)); }

WARN_TEXT='worktree 会建在系统的临时目录里'

echo "== [1] 不设 WT_DIR_ROOT：默认必须落在持久位置 =="
out="$(env -u WT_DIR_ROOT bash "$WT" dir 2>/dev/null)"
p="$(printf '%s\n' "$out" | sed -n 's/^WT_DIR_ROOT=//p')"
tmpdir="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
if [ "$(printf '%s\n' "$out" | sed -n 's/^under_tmpdir=//p')" = "no" ]; then
  ok "解析结果不在临时目录下（WT_DIR_ROOT=${p}）"
else
  bad "默认值落在临时目录下：${p}"
fi
if [ "$p" = "$(cd "$ROOT/.." && pwd -P)/.wt" ]; then
  ok "默认值 = <repo>/../.wt（与 WT_TARGET_ROOT 对称）"
else
  bad "默认值不是 <repo>/../.wt：${p}"
fi

echo
echo "== [2] 显式指到 ${TMPDIR}… ⇒ 必须大声警告（但默认不失败）=="
out="$(WT_DIR_ROOT="${TMPDIR:-/tmp}/xraytun-wt-probe" bash "$WT" dir 2>&1)"; rc=$?
printf '%s\n' "$out" | grep -q "$WARN_TEXT" && ok "出现警告（原文：$(printf '%s\n' "$out" | grep -m1 "$WARN_TEXT" | sed 's/^ *//')…）" \
  || bad "没有警告 —— 显式把 worktree 放进可被清理的目录却没人提醒"
printf '%s\n' "$out" | grep -q '^under_tmpdir=yes$' && ok "under_tmpdir=yes（判据成立）" || bad "under_tmpdir 不是 yes"
[ "${rc:-1}" = 0 ] && ok "默认只警告，退出码 0（显式指定可能是有意的）" || bad "默认就失败了（rc=${rc}）"

echo
echo "== [3] WT_STRICT=1 + 临时目录 ⇒ 退出 75 =="
WT_STRICT=1 WT_DIR_ROOT="${TMPDIR:-/tmp}/xraytun-wt-probe" bash "$WT" dir >/tmp/wt185-strict.out 2>&1; rc=$?
[ "${rc:-0}" = 75 ] && ok "退出码 75" || bad "退出码 ${rc}（要求 75）"
grep -q '退出码 75' /tmp/wt185-strict.out && ok "报错文案写明升级为 75" || bad "没有写明 75 的文案"

echo
echo "== [4] 反向敏感性：把守卫去掉 ⇒ 案子 [2] 的断言必须变红 =="
MUT="$(mktemp -d)/wt-no-guard.sh"
# 去掉守卫调用与安全性输出（= 旧行为：不看不报），其余逐字不动
sed -e 's/^  wt_dir_guard 1 || return \$?$//' \
    -e 's/^  if under_tmpdir "\$WT_DIR_ROOT"; then echo "under_tmpdir=yes"; else echo "under_tmpdir=no"; fi$/  echo "under_tmpdir=no"/' \
    "$WT" >"$MUT"
if diff -q "$WT" "$MUT" >/dev/null; then
  bad "变异没生效（敏感性子案无效）"
else
  mout="$(WT_DIR_ROOT="${TMPDIR:-/tmp}/xraytun-wt-probe" bash "$MUT" dir 2>&1)"
  if printf '%s\n' "$mout" | grep -q "$WARN_TEXT"; then
    bad "去掉守卫后仍然警告 ⇒ 这个案子测不到守卫"
  else
    ok "去掉守卫后**没有**警告 ⇒ 案子 [2] 确实在测守卫（变异后该断言变红）"
  fi
fi
rm -rf "$(dirname "$MUT")"

echo
echo "== [5] 边界：恰好等于 TMPDIR / 只是前缀相同 =="
c1="$(WT_DIR_ROOT="${TMPDIR:-/tmp}" bash "$WT" dir 2>/dev/null | sed -n 's/^under_tmpdir=//p')"
[ "$c1" = "yes" ] && ok "WT_DIR_ROOT == \$TMPDIR ⇒ under_tmpdir=yes" || bad "恰好等于 TMPDIR 时判成了 ${c1}"
c2="$(WT_DIR_ROOT="$(dirname "${TMPDIR%/}")/T2/wt" bash "$WT" dir 2>/dev/null | sed -n 's/^under_tmpdir=//p')"
[ "$c2" = "no" ] && ok "$(dirname "${TMPDIR%/}")/T2/wt（与 TMPDIR 同级、只是字符串前缀相近）⇒ under_tmpdir=no" || bad "前缀相近的目录被误判成 ${c2}"

echo
printf '== 汇总：pass=%d fail=%d ==\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
