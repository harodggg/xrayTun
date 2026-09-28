#!/usr/bin/env bash
#
# 审计密文端点的本地验证（**不碰 Cloudflare 账号、不需要任何凭据**）：
#
#   ./infra/audit-collector/verify.sh                # 静态检查 + 全部单测
#   ./infra/audit-collector/verify.sh --sensitivity  # 双向敏感性：把「鉴权」「device 正则」拿掉
#                                                    # ⇒ 对应用例**必须变红**（脚本才认为成立）
#
# 只用 `node --test`（Node ≥ 20 自带）+ 文本检查，不需要 wrangler、不联网、不装依赖。
#
# 为什么静态检查是**脚本的一部分**而不靠人看：`wrangler.toml` 的 `routes` 一旦落在任何表头之后，
# 一条路由都不会注册，而部署日志看起来是成功的（incident-collector 2026-09-23 的两次真实事故）。
# 这类错误单测抓不到（单测不经过路由），必须在提交前用文本判据挡住。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/src/worker.mjs"
TEST="$HERE/test/worker.test.mjs"
CFG="$HERE/wrangler.toml"
README="$HERE/README.md"
MODE="green"
[ "${1:-}" = "--sensitivity" ] && MODE="sensitivity"

pass=0; fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

run_suite() { # $1 = 模块路径（可指向变体）
  AUDIT_MODULE="file://$1" node --test "$TEST" 2>&1
}
count_fail() { printf '%s\n' "$1" | sed -n 's/^ℹ fail \([0-9]*\)$/\1/p' | head -1; }
count_tests() { printf '%s\n' "$1" | sed -n 's/^ℹ tests \([0-9]*\)$/\1/p' | head -1; }
failing_tests() { printf '%s\n' "$1" | grep '^✖ ' || true; }

echo "审计密文端点验证：MODE=$MODE"
echo "  源码：$SRC"
echo "  配置：$CFG"

# ------------------------------------------------------------------ 静态检查
#
# 这些文本判据在两种模式下都跑：变体改的是 src/worker.mjs，配置与文档不变。
static_checks() {
  hdr "[1] wrangler.toml：routes 必须在**任何表头之前**，且两条 pattern 都在"
  if [ ! -f "$CFG" ]; then
    no "找不到 $CFG"
    return
  fi
  local rline tline
  rline="$(grep -nE '^[[:space:]]*routes[[:space:]]*=' "$CFG" | head -1 | cut -d: -f1)"
  tline="$(grep -nE '^[[:space:]]*\[' "$CFG" | head -1 | cut -d: -f1)"
  if [ -z "$rline" ]; then
    no "没有顶层 routes = [...]（路由不会注册）"
  elif [ -z "$tline" ]; then
    no "配置里连表头都没有 —— 配置看起来不对"
  elif [ "$rline" -lt "$tline" ]; then
    ok "routes 在第 ${rline} 行，第一个表头在第 ${tline} 行（routes 在前）"
  else
    no "routes（第 ${rline} 行）落在表头（第 ${tline} 行）之后 ⇒ 会被当成表字段/环境变量 ⇒ **一条路由都不会注册**，而部署可能显示成功"
  fi
  if grep -qE 'pattern[[:space:]]*=[[:space:]]*"xraytun\.top/api/audit"' "$CFG"; then
    ok '有裸路径 pattern "xraytun.top/api/audit"（少这条 ⇒ POST /api/audit 落到 Pages 返回 405）'
  else
    no '缺裸路径 pattern "xraytun.top/api/audit" ⇒ 上传入口会 405'
  fi
  if grep -qE 'pattern[[:space:]]*=.*"xraytun\.top/api/audit/\*"' "$CFG"; then
    ok '有通配 pattern "xraytun.top/api/audit/*"（list/revoke 靠它）'
  else
    no '缺通配 pattern "xraytun.top/api/audit/*" ⇒ /list、/revoke 会 404'
  fi

  hdr "[2] wrangler.toml：名字 / binding / 桶 / secret / 日志"
  grep -qE '^name[[:space:]]*=[[:space:]]*"xraytun-audit-collector"' "$CFG" \
    && ok 'name = xraytun-audit-collector' || no 'Worker 名字不是 xraytun-audit-collector'
  grep -qE '^main[[:space:]]*=[[:space:]]*"src/worker\.mjs"' "$CFG" \
    && ok 'main = src/worker.mjs' || no 'main 不是 src/worker.mjs'
  grep -qE '^binding[[:space:]]*=[[:space:]]*"AUDIT_BUCKET"' "$CFG" \
    && ok 'R2 binding = AUDIT_BUCKET' || no 'R2 binding 不是 AUDIT_BUCKET'
  grep -qE '^bucket_name[[:space:]]*=[[:space:]]*"xraytun-audit"' "$CFG" \
    && ok 'R2 桶 = xraytun-audit' || no 'R2 桶不是 xraytun-audit'
  if grep -nE '^[[:space:]]*AUDIT_TOKEN[[:space:]]*=' "$CFG" >/dev/null; then
    no '**AUDIT_TOKEN 被写进了 wrangler.toml**（它是 secret，必须用 `wrangler secret put`）'
  else
    ok 'AUDIT_TOKEN 不在 [vars] 里（是 secret）'
  fi
  grep -q 'wrangler secret put AUDIT_TOKEN' "$CFG" \
    && ok '配置注释里有 `wrangler secret put AUDIT_TOKEN` 的准确命令' \
    || no '注释里没写 secret put 命令（下一个人会不知道 token 怎么配）'
  if awk '/^\[observability\]/{f=1;next} /^\[/{f=0} f' "$CFG" | grep -qE '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false'; then
    ok '[observability] enabled = false（不采集请求日志）'
  else
    no '[observability] enabled = false 没设对（会采集请求日志）'
  fi
  for v in BASE_PATH MAX_BYTES RETENTION_DAYS RATE_LIMIT_MAX RATE_LIMIT_WINDOW_SECONDS; do
    grep -qE "^[[:space:]]*${v}[[:space:]]*=" "$CFG" && ok "[vars] 有 $v" || no "[vars] 缺 $v"
  done

  hdr "[3] 资源边界：只碰 xraytun-audit-collector 与 xraytun-audit"
  # 别的资源名（在脚本里拆开写，免得本脚本自己被 grep 命中）；README 只以「不属于本项目」的方式提及
  local banned_a="xraytun-""incident"
  local banned_b="eth-arb-""scout"
  local banned_c="mymulti""cloud"
  local hits=""
  for b in "$banned_a" "$banned_b" "$banned_c"; do
    hits="$(grep -rl -- "$b" "$HERE" 2>/dev/null | grep -v '^$' || true)"
    if [ -n "$hits" ]; then
      no "本目录里出现了别的资源名「${b}」：$(printf '%s ' $hits)"
    else
      ok "本目录没有出现「${b}」"
    fi
  done
  # 删除命令只允许指向我们自己的 Worker / 桶。
  # 只匹配**真的命令**（以 `npx wrangler … delete` 开头），不匹配 README 里「别这么干」的警告文字
  # （否则检查器会被自己的说明文字绊倒，第一版就踩了这个假阳性）；
  # 同时排除检查器自身，避免它们的 grep 模式自匹配。
  local bad_del=0 del_lines
  del_lines="$(grep -rnE '^[[:space:]]*npx wrangler delete|^[[:space:]]*npx wrangler r2 bucket delete' "$HERE" \
    --include='*.sh' --include='*.md' --include='*.toml' \
    --exclude=verify.sh --exclude=deploy-check.sh 2>/dev/null || true)"
  if [ -z "$del_lines" ]; then
    ok '本目录没有任何 `npx wrangler … delete` 命令'
  else
    while IFS= read -r line; do
      [ -z "$line" ] && continue
      if printf '%s' "$line" | grep -q 'wrangler delete'; then
        printf '%s' "$line" | grep -q -- '--config infra/audit-collector/wrangler.toml' \
          || { no "非本项目的 wrangler delete：$line"; bad_del=1; }
      fi
      if printf '%s' "$line" | grep -q 'r2 bucket delete'; then
        printf '%s' "$line" | grep -q 'xraytun-audit' \
          || { no "非本项目的 r2 bucket delete：$line"; bad_del=1; }
      fi
    done <<<"$del_lines"
    [ "$bad_del" = "0" ] && ok '所有 delete 命令都只指向本项目的 Worker/桶'
  fi

  hdr "[4] 文档：lifecycle 命令与「限速非授权判据」必须写在 README 里"
  grep -q 'r2 bucket lifecycle add xraytun-audit' "$README" \
    && ok 'README 有 400 天 lifecycle 的具体命令' \
    || no 'README 缺 lifecycle add 命令（只写「会过期」= 不可复核的承诺）'
  grep -q 'r2 bucket lifecycle list xraytun-audit' "$README" \
    && ok 'README 要求加完读回复核' || no 'README 没有读回复核的步骤'
  grep -qi 'best-effort' "$README" \
    && ok 'README 写明限速是 best-effort' || no 'README 没写限速的真实边界'
  grep -q '非授权判据' "$README" \
    && ok 'README 写明限速不是授权判据' || no 'README 没说限速不是授权判据'
}

# ------------------------------------------------------------------ 绿
if [ "$MODE" = "green" ]; then
  static_checks

  hdr "[5] 真源码：全部用例"
  OUT="$(run_suite "$SRC")"
  printf '%s\n' "$OUT" | grep -E '^(✖|ℹ (tests|pass|fail))' | sed 's/^/    /'
  F="$(count_fail "$OUT")"
  T="$(count_tests "$OUT")"
  if [ "${F:-x}" = "0" ] && [ -n "$T" ] && [ "$T" -ge 30 ]; then
    ok "真源码：${T} 个用例、fail=0（鉴权/形状/前缀穿越/大小/限速/key/replaced/list 分页/revoke 分页/404 全覆盖）"
  elif [ "${F:-x}" = "0" ]; then
    no "fail=0 但只跑了 ${T:-未知} 个用例（少于 30 ⇒ 可能是「测了个空」）"
  else
    no "真源码有用例失败（fail=${F:-未知}）——见上面原始输出"
  fi

  hdr "[6] 顺带自证：不存在的模块路径必须**报错**（防止「测了个空」）"
  OUT2="$(AUDIT_MODULE="file://$HERE/src/does-not-exist.mjs" node --test "$TEST" 2>&1)"
  if printf '%s\n' "$OUT2" | grep -qE 'ERR_MODULE_NOT_FOUND|Cannot find module'; then
    ok '模块缺失时报错（不是静默 0 个用例）'
  else
    no '模块缺失却像没事一样 —— 验证可能在测空'
  fi

  echo
  echo "  pass=$pass fail=$fail"
  [ "$fail" = "0" ] && { echo "  ✓ 审计端点自测通过"; exit 0; }
  echo "  ✗ 审计端点自测失败"; exit 1
fi

# ------------------------------------------------------------------ 敏感性
#
# 变体必须放在 **src/ 旁边**：虽然当前 worker.mjs 没有相对 import，但这个约定别改 ——
# 一旦以后拆出模块（例如把形状校验抽成 ./envelope.mjs），放到 $TMP 里会让 import 解析失败
# ⇒ 整份测试加载不起来、看起来「全红」；那种红不是「这条断言在验它」。
# （incident-collector 第一版就这么假红过一次，所以这里照样要求「其它用例仍然通过」。）
TMP="$(mktemp -d "${TMPDIR:-/tmp}/audit-endpoint-sens.XXXXXX")"
# ⚠️ 通配符必须放在引号**外面**：`rm -rf "$HERE/src/.mut-*.mjs"` 里 glob 被引号关掉，
# rm 会去找一个字面名为 `.mut-*.mjs` 的文件 ⇒ 变体文件永远留在 src/（第一版就留下了，
# 后来靠 .gitignore 兜着才没进仓库）。
trap 'rm -rf "$TMP"; rm -f "$HERE"/src/.mut-*.mjs' EXIT INT TERM

MUT_A="$HERE/src/.mut-no-auth.mjs"          # 拿掉鉴权
MUT_B="$HERE/src/.mut-no-device-re.mjs"     # 拿掉 device 正则（含两道 self-guard）
python3 - "$SRC" "$MUT_A" "$MUT_B" <<'PYEOF'
import sys, pathlib
src, out_a, out_b = sys.argv[1], sys.argv[2], sys.argv[3]
s = pathlib.Path(src).read_text(encoding='utf-8')

a = s.replace("if (!authorized(request, env)) {", "if (false) {")
assert a != s, 'auth 变体没改到源码'

b = s.replace(
    "if (typeof body.device !== 'string' || !DEVICE_RE.test(body.device)) {",
    "if (false) {",
)
# list/revoke 里还有一道独立的 device 检查（问的是 query body 的 device）—— 一并拿掉，
# 否则变体只破坏上传路径，而 list/revoke 的前缀守卫仍在，敏感性证据就不完整。
b = b.replace(
    "if (typeof device !== 'string' || !DEVICE_RE.test(device)) {",
    "if (false) {",
)
b = b.replace(
    "if (!DEVICE_RE.test(String(device))) throw new Error('device 未过 ^[a-f0-9]{16}$：拒绝拼 R2 key');",
    "/* mutated: key guard removed */",
)
b = b.replace(
    "if (!DEVICE_RE.test(String(device))) throw new Error('device 未过 ^[a-f0-9]{16}$：拒绝拼前缀');",
    "/* mutated: prefix guard removed */",
)
assert b != s, 'device 变体没改到源码'
assert b.count('DEVICE_RE.test') < s.count('DEVICE_RE.test'), 'device 守卫没被真正拿掉'

pathlib.Path(out_a).write_text(a, encoding='utf-8')
pathlib.Path(out_b).write_text(b, encoding='utf-8')
PYEOF
[ -s "$MUT_A" ] && [ -s "$MUT_B" ] || { echo "  ✗ 变体文件为空（替换失败）"; exit 1; }

# 格式：<tag>:<模块路径>:<必须红的用例，| 分隔>#<必须仍通过的用例，| 分隔>
for pair in \
  "A:$MUT_A:鉴权：三个入口不带 token|鉴权：错 token|鉴权：没配置 AUDIT_TOKEN#replaced 语义|list：返回元数据|revoke：删掉本 device|形状：v 不是 1" \
  "B:$MUT_B:device 没过多正则|auditKey / devicePrefix|list：device 形状不对|revoke：device 形状不对#落盘：key 精确等于|replaced 语义|list：返回元数据|revoke：删掉本 device"; do
  tag="${pair%%:*}"; rest="${pair#*:}"; mod="${rest%%:*}"; rest2="${rest#*:}"
  want="${rest2%%#*}"; must_pass_all="${rest2#*#}"
  hdr "[${tag}] $( [ "$tag" = A ] && echo '去掉「鉴权」' || echo '去掉「device 正则」' ) ⇒ 对应用例必须红，且**其它用例仍须通过**"
  OUT="$(run_suite "$mod")"
  printf '%s\n' "$OUT" | grep -E '^(✖|ℹ (tests|pass|fail))' | sed 's/^/    /'
  F="$(count_fail "$OUT")"
  TESTS="$(count_tests "$OUT")"
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
  echo "  ✓ 双向敏感性成立：把鉴权 / device 正则拿掉之后，对应用例确实变红"
  exit 0
fi
echo "  ✗ 敏感性不成立（见上面每一条 ✗）"
exit 1
