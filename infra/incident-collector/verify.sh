#!/usr/bin/env bash
#
# 现场包端点的本地验证（**不碰 Cloudflare 账号**）：
#
#   ./infra/incident-collector/verify.sh                # 绿：16 个用例全过
#   ./infra/incident-collector/verify.sh --sensitivity  # 双向敏感性：把「鉴权」「大小上限」拿掉
#                                                       # ⇒ 对应用例**必须变红**（脚本才认为成立）
#
# 只用 `node --test`（Node ≥ 20 自带），不需要 wrangler、不联网、不装依赖。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/src/worker.mjs"
TEST="$HERE/test/worker.test.mjs"
MODE="green"
[ "${1:-}" = "--sensitivity" ] && MODE="sensitivity"

pass=0; fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }

run_suite() { # $1 = 模块路径（可指向变体）
  INCIDENT_MODULE="file://$1" node --test "$TEST" 2>&1
}

count_fail() { printf '%s\n' "$1" | sed -n 's/^ℹ fail \([0-9]*\)$/\1/p' | head -1; }
failing_tests() { printf '%s\n' "$1" | grep '^✖ ' || true; }

echo "现场包端点验证：MODE=$MODE"
echo "  源码：$SRC"
echo

# ------------------------------------------------------------------ 绿
if [ "$MODE" = "green" ]; then
  echo "=============================================================="
  echo "  [1] 真源码：全部用例"
  echo "=============================================================="
  OUT="$(run_suite "$SRC")"
  printf '%s\n' "$OUT" | tail -8 | sed 's/^/    /'
  F="$(count_fail "$OUT")"
  if [ "${F:-x}" = "0" ]; then
    ok "真源码：fail=0（大小上限 / 类型白名单 / 乱序魔法 / 限流 / 鉴权 / 删除 / 保留期 / 路由 全覆盖）"
  else
    no "真源码有用例失败（fail=${F:-未知}）——见上面原始输出"
  fi

  echo
  echo "=============================================================="
  echo "  [2] 顺带自证：不存在的模块路径必须**报错**（防止「测了个空」）"
  echo "=============================================================="
  OUT2="$(INCIDENT_MODULE="file://$HERE/src/does-not-exist.mjs" node --test "$TEST" 2>&1)"
  if printf '%s\n' "$OUT2" | grep -qE "ERR_MODULE_NOT_FOUND|Cannot find module"; then
    ok "模块缺失时报错（不是静默 0 个用例）"
  else
    no "模块缺失却像没事一样 —— 验证可能在测空"
  fi

  echo
  echo "  pass=$pass fail=$fail"
  [ "$fail" = "0" ] && { echo "  ✓ 端点自测通过"; exit 0; }
  echo "  ✗ 端点自测失败"; exit 1
fi

# ------------------------------------------------------------------ 敏感性
TMP="$(mktemp -d "${TMPDIR:-/tmp}/incident-endpoint-sens.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

# 变体 A：把鉴权拿掉（两个 `if (!authorized(...)) return err(401, ...)` 的**条件**改成 false）
# 变体 B：把大小上限拿掉（`const max = intEnv(env, 'MAX_BYTES', …)` ⇒ Number.MAX_SAFE_INTEGER）
# 用 python3 做字符串替换（sed 的转义在第一次跑时就把模式弄坏了 —— 那次的「红」其实是模块加载失败，
# 而不是断言红了；所以下面还会要求「其它用例仍然通过」，防止这种假红冒充敏感性成立）。
MUT_A="$TMP/worker-no-auth.mjs"
MUT_B="$TMP/worker-no-size-limit.mjs"
python3 - "$SRC" "$MUT_A" "$MUT_B" <<'PYEOF'
import sys, pathlib
src, out_a, out_b = sys.argv[1], sys.argv[2], sys.argv[3]
s = pathlib.Path(src).read_text(encoding='utf-8')
a = s.replace("if (!authorized(request, env)) return err(401, 'unauthorized', '需要 X-Auth-Token');",
              "if (false) return err(401, 'unauthorized', '需要 X-Auth-Token');")
b = s.replace("const max = intEnv(env, 'MAX_BYTES', DEFAULTS.MAX_BYTES);",
              "const max = Number.MAX_SAFE_INTEGER;")
assert a != s, 'auth 变体没改到源码'
assert b != s, 'size 变体没改到源码'
pathlib.Path(out_a).write_text(a, encoding='utf-8')
pathlib.Path(out_b).write_text(b, encoding='utf-8')
PYEOF
[ -s "$MUT_A" ] && [ -s "$MUT_B" ] || { echo "  ✗ 变体文件为空（替换失败）"; exit 1; }

# 只允许这几条因「拿掉鉴权 / 拿掉大小上限」而红；**其它用例必须仍然通过** ——
# 否则整个模块可能只是加载失败（那样 16 条全红，看起来也像“敏感性成立”）。
# 变体 A 会红 3 条（都属于鉴权）：blob 无 token / delete 无 token / 未配 token 时 fail-closed；
# 变体 B 只红 1 条（大小上限）。其余用例**必须仍然通过**。
# 格式：<tag>:<模块路径>:<必须红的用例，| 分隔>#<必须仍通过的用例，| 分隔>
for pair in "A:$MUT_A:blob：不带 token|删除：不带 token|blob：没配置 INCIDENT_TOKEN#上传：合法 zip|上传：限流|上传：超出大小上限" \
            "B:$MUT_B:超出大小上限#上传：合法 zip|上传：限流|blob：不带 token|删除：不带 token"; do
  tag="${pair%%:*}"; rest="${pair#*:}"; mod="${rest%%:*}"; rest2="${rest#*:}"
  want="${rest2%%#*}"; must_pass_all="${rest2#*#}"
  echo "=============================================================="
  if [ "$tag" = "A" ]; then echo "  [A] 去掉「鉴权」⇒ 鉴权相关用例必须红，且**其它用例仍须通过**"; else echo "  [B] 去掉「大小上限」⇒ 该用例必须红，且**其它用例仍须通过**"; fi
  echo "=============================================================="
  OUT="$(run_suite "$mod")"
  printf '%s\n' "$OUT" | grep -E '^(✖|ℹ (tests|pass|fail))' | sed 's/^/    /'
  F="$(count_fail "$OUT")"
  TESTS="$(printf '%s\n' "$OUT" | sed -n 's/^ℹ tests \([0-9]*\)$/\1/p' | head -1)"
  fails="$(failing_tests "$OUT")"
  if printf '%s\n' "$fails" | grep -q "Cannot find module\|ERR_MODULE_NOT_FOUND"; then
    no "变体 ${tag} 是**模块加载失败**（不是断言红）——这种红不算敏感性成立"
    continue
  fi
  if [ "${F:-x}" = "${TESTS:-y}" ]; then
    no "变体 ${tag} 让**全部 ${TESTS} 条**都红了 —— 那不是「这条断言在验它」，而是整体崩了"
    continue
  fi
  ok_flag=1
  IFS='|' read -r -a want_arr <<< "$want"
  for w in "${want_arr[@]}"; do
    printf '%s\n' "$fails" | grep -q "$w" || { no "变体 ${tag}：期望变红的「${w}」没红"; ok_flag=0; }
  done
  IFS='|' read -r -a pass_arr <<< "$must_pass_all"
  for w in "${pass_arr[@]}"; do
    printf '%s\n' "$fails" | grep -q "$w" && { no "变体 ${tag}：不该受影响的「${w}」也红了 ⇒ 这个变体破坏面过大，敏感性证据不可信"; ok_flag=0; }
  done
  [ "$ok_flag" = "1" ] && ok "变体 ${tag} 变红且**只红该红的**（fail=${F}/tests=${TESTS}）"
  echo
done

echo "  pass=$pass fail=$fail"
if [ "$fail" = "0" ]; then
  echo "  ✓ 双向敏感性成立：把鉴权/大小上限拿掉之后，对应用例确实变红"
  exit 0
fi
echo "  ✗ 敏感性不成立（见上面每一条 ✗）"
exit 1
