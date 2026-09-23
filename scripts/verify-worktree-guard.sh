#!/usr/bin/env bash
#
# 验证 `scripts/check.sh` 的 **worktree 产物身份守卫**（三态判据）。
#
# 为什么单独一条：2026-09-23 的审计发现 `check.sh` 的判据在**两个 `cd` 都失败**时
# 两侧都是**空串 ⇒ 判为相等** ⇒ 打**假警告**；`WT_STRICT=1` 时**以 75 假失败**。
# 那是**门禁里的假信号**，与 task-109 同一族（门禁自己变成谎话的来源）。
#
# 用法：
#     ./scripts/verify-worktree-guard.sh                # 绿：三态判据都在该在的分支上
#     ./scripts/verify-worktree-guard.sh --sensitivity  # 把判据改回 `[ "" = "" ]`（旧 bug）⇒ T3/T4 必须红
#
# 只在临时目录里建一个**一次性 git 仓库 + linked worktree** 来跑，秒级、不占真实 `CARGO_TARGET_DIR`、
# 不跑任何真实的 npm/cargo（PATH 前置 stub）。守卫在任何 step 之前打印 ⇒「看到输出即 kill」。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SENS=0
[ "${1:-}" = "--sensitivity" ] && SENS=1

TMP="$(mktemp -d "${TMPDIR:-/tmp}/wt-guard.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

# 临时 git 仓库（含一个 commit，才能 worktree add）
REPO="$TMP/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
WT="$TMP/wt"
git -C "$REPO" worktree add --detach -q "$WT" HEAD

# 把**当前工作区**的脚本拷进那个 worktree（测的是我们正在改的文件，不是已提交版本）
mkdir -p "$WT/scripts"
cp "$ROOT/scripts/check.sh" "$ROOT/scripts/build-lock.sh" "$WT/scripts/"
if [ "$SENS" = "1" ]; then
  # 复现旧 bug：两侧都取不到时按「空串相等」处理
  python3 - "$WT/scripts/check.sh" <<'PYEOF'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text(encoding='utf-8')
old = """  if [ -z "$_tgt_real" ] || [ -z "$_main_real" ]; then"""
assert old in s
# 旧形态：直接比较（取不到 ⇒ 两侧空串 ⇒ 相等）
buggy = s.replace("""  if [ -z "$_tgt_real" ] || [ -z "$_main_real" ]; then""",
                  """  if false; then""", 1)
buggy = buggy.replace("""  elif [ "$_tgt_real" = "$_main_real" ]; then""",
                      """  elif [ "$_tgt_real" = "$_main_real" ] || { [ -z "$_tgt_real" ] && [ -z "$_main_real" ]; }; then""", 1)
p.write_text(buggy, encoding='utf-8')
PYEOF
fi

# stub：就算守卫没拦住，也不真的编译
mkdir -p "$TMP/bin"
for c in npm npx cargo python3; do printf '#!/bin/sh\nexit 0\n' >"$TMP/bin/$c"; chmod +x "$TMP/bin/$c"; done

pass=0; fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

SHARED_PHRASE='指向**主工作区**的 target dir'
UNKNOWN_PHRASE='无法判定'
# ⚠️ 短语里有 `**`（grep 的 BRE 元字符）⇒ 一律用 `grep -qF`，否则 grep 自己报
#    `repetition-operator operand invalid` 而**静默判成「没命中」**（第一版就踩了：T1 假红）。
has() { printf '%s\n' "$1" | grep -qF "$2"; }

# run_case <名字> <CARGO_TARGET_DIR> <WT_STRICT> ; 输出写到 $TMP/out.<名字>
run_case() {
  local name="$1" tgt="$2" strict="$3"
  : >"$TMP/out.$name"
  ( cd "$WT" && \
    CARGO_TARGET_DIR="$tgt" WT_STRICT="$strict" BUILD_LOCK_DIR="$TMP/lock.$name" BUILD_LOCK_WAIT=5 \
    PATH="$TMP/bin:$PATH" \
      ./scripts/check.sh --no-release-build >>"$TMP/out.$name" 2>&1 ) &
  local pid=$!
  for i in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || break
    grep -qF -e "$UNKNOWN_PHRASE" -e "$SHARED_PHRASE" "$TMP/out.$name" 2>/dev/null && break
    sleep 0.2
  done
  # WT_STRICT=1 的正常路径是**自己退出 75**；先给一小段宽限，再 kill（非 strict 的用例会被 kill，
  # 那是预期的 —— 我们只关心守卫那几行）
  for i in $(seq 1 15); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.2
  done
  kill -TERM "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  echo "$?" >"$TMP/rc.$name"
}

# ------------------------------------------------------------------ 用例
MAIN_TARGET="$TMP/repo/../.cargo-target"      # = <repo>/../.cargo-target（由代码这么推）
mkdir -p "$MAIN_TARGET"                        # T1/T2 需要它**存在**
OTHER="$TMP/other-target"; mkdir -p "$OTHER"
MISSING="$TMP/does-not-exist"

hdr "[T1] worktree + 共享 target dir ⇒ 必须打共享警告；WT_STRICT=1 ⇒ 75"
run_case t1 "$MAIN_TARGET" 1
OUT="$(cat "$TMP/out.t1")"; RC="$(cat "$TMP/rc.t1")"
has "$OUT" "$SHARED_PHRASE" && ok "打出了共享警告" || no "没打出共享警告"
[ "$RC" = "75" ] && ok "WT_STRICT=1 退出码 75" || no "退出码 ${RC}（期望 75）"

hdr "[T2] worktree + 独立 target dir ⇒ 不该有任何警告；WT_STRICT=1 也不该失败"
run_case t2 "$OTHER" 1
OUT="$(cat "$TMP/out.t2")"; RC="$(cat "$TMP/rc.t2")"
has "$OUT" "$SHARED_PHRASE" && no "误报共享警告" || ok "没有共享警告"
has "$OUT" "$UNKNOWN_PHRASE" && no "误报「无法判定」" || ok "也没有「无法判定」"
[ "$RC" = "75" ] && no "不该失败却给了 75" || ok "没有假失败（rc=${RC}，被 kill 属正常）"

# T3/T4 需要「主工作区 target 不存在」这个条件 ⇒ 先把它删掉（T1/T2 用完）
rmdir "$MAIN_TARGET" 2>/dev/null || rm -rf "$MAIN_TARGET"

hdr "[T3] **两侧都取不到** ⇒ 必须说「无法判定」+ 环境问题；WT_STRICT=1 ⇒ 75（且不是共享警告）"
run_case t3 "$MISSING" 1
OUT="$(cat "$TMP/out.t3")"; RC="$(cat "$TMP/rc.t3")"
has "$OUT" "$UNKNOWN_PHRASE" && ok "打出了「无法判定」" || no "没说「无法判定」"
has "$OUT" '这是环境问题，不是代码失败' && ok "写明「环境问题，不是代码失败」" || no "没写明是环境问题"
has "$OUT" "$SHARED_PHRASE" && no "误用了共享警告的措辞" || ok "没有沿用共享警告措辞"
has "$OUT" "$MISSING" && ok "把 CARGO_TARGET_DIR 的值打出来了" || no "没打出 CARGO_TARGET_DIR 的值"
EXP_MAIN="$(cd "$TMP/repo/.." && pwd -P)/.cargo-target"

# 只断言「打印了那个期望值」的**特征部分**：git 返回的路径是否解析 /private 符号链接不确定，
# 而且 TMPDIR 末尾的 `/` 会造成 `T//`。要点是：让人能看到「主 target 的期望值是多少」。
if has "$OUT" "/repo/../.cargo-target"; then
  ok "把主工作区 target 的期望值打出来了（…/repo/../.cargo-target）"
else
  no "没打出主 target 的期望值（期望含 /repo/../.cargo-target；实际输出：$(printf '%s' "$OUT" | grep -F '主工作区 target' | head -1)）"
fi
[ "$RC" = "75" ] && ok "WT_STRICT=1 ⇒ 75" || no "退出码 ${RC}（期望 75）"

hdr "[T4] **单边取不到**（target 存在、主路径不存在）⇒ 同样走「无法判定」"
run_case t4 "$OTHER" 1
OUT="$(cat "$TMP/out.t4")"; RC="$(cat "$TMP/rc.t4")"
has "$OUT" "$UNKNOWN_PHRASE" && ok "打出了「无法判定」" || no "没说「无法判定」"
has "$OUT" "$SHARED_PHRASE" && no "误报共享警告" || ok "没有共享警告"
[ "$RC" = "75" ] && ok "WT_STRICT=1 ⇒ 75（环境问题）" || no "退出码 ${RC}（期望 75）"

hdr "[T5] 普通模式（非 strict）下「无法判定」也必须**可见**，且**不失败**"
run_case t5 "$MISSING" 0
OUT="$(cat "$TMP/out.t5")"; RC="$(cat "$TMP/rc.t5")"
has "$OUT" "$UNKNOWN_PHRASE" && ok "普通模式也打出「无法判定」（不是 debug/静默）" || no "普通模式下看不到「无法判定」"
[ "$RC" = "75" ] && no "普通模式不该给 75" || ok "普通模式不失败（rc=${RC}，被 kill 属正常）"

hdr "结果（MODE=$([ "$SENS" = 1 ] && echo sensitivity || echo green)）"
echo "  pass=$pass fail=$fail"
if [ "$SENS" = "1" ]; then
  # 敏感性：把判据改回旧 bug 后，T3/T4 必须变红
  if [ "$fail" -gt 0 ]; then
    echo "  ✓ 敏感性成立：判据改回「空串相等」后，T3/T4 出现 **${fail} 条红** ⇒ 这些断言真的在验「取不到 ≠ 相等」"
    exit 0
  fi
  echo "  ✗ 敏感性不成立：判据改回旧 bug 后仍然全绿 ⇒ 这些断言没有在验它"
  exit 1
fi
[ "$fail" = "0" ] && { echo "  ✓ worktree 守卫验证通过（三态判据都在该在的分支上）"; exit 0; }
echo "  ✗ worktree 守卫验证失败"
exit 1
