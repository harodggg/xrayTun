#!/usr/bin/env bash
#
# 审计密文端点的**部署前静态自检**（本机没有 CF 凭据 ⇒ 这里**只做静态检查，不部署、不联网**）。
#
# 为什么需要它：`wrangler.toml` 的 `routes` 一旦落在任何表头之后，一条路由都不会注册，
# 而部署日志看起来是成功的、`POST https://xraytun.top/api/audit` 却返回 **405**（请求落到 Pages）。
# incident-collector 在 2026-09-23 连踩两次（`env.routes` + 少注册裸路径 pattern）。
# 这个脚本把「部署前必须成立」的文本判据固化成一条命令，避免靠人眼。
#
# 用法：
#   ./infra/audit-collector/deploy-check.sh          # 静态自检（不需要任何凭据）
#   ./infra/audit-collector/deploy-check.sh --help
#
# 真正部署后还**必须**在 Cloudflare 侧复核的三条（见 README §3.4；本脚本无法代跑）：
#   1) 部署日志里没有 `env.routes` / `Unexpected fields`；
#   2) `GET /zones/<zone>/workers/routes` 能看到 `xraytun.top/api/audit` 与 `.../api/audit/*` 两条；
#   3) `POST {BASE}` 不是 405/404（不带 token 应得本端点的 401 JSON）。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/src/worker.mjs"
TEST="$HERE/test/worker.test.mjs"
CFG="$HERE/wrangler.toml"
README="$HERE/README.md"
EXPECTED_RETENTION_DAYS=400

while [ $# -gt 0 ]; do
  case "$1" in
    -h | --help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

pass=0; fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

echo "审计密文端点：部署前静态自检（不部署、不联网、不需要 CF 凭据）"
echo "  配置：$CFG"

# ------------------------------------------------------------------ 1) 路由作用域
hdr "[1] routes 必须在**任何表头之前**，且两条 pattern 都在（少一条 ⇒ 405 落到 Pages）"
if [ ! -f "$CFG" ]; then
  no "找不到 $CFG"
else
  rline="$(grep -nE '^[[:space:]]*routes[[:space:]]*=' "$CFG" | head -1 | cut -d: -f1)"
  tline="$(grep -nE '^[[:space:]]*\[' "$CFG" | head -1 | cut -d: -f1)"
  if [ -z "$rline" ]; then
    no "没有顶层 routes = [...]"
  elif [ -n "$tline" ] && [ "$rline" -lt "$tline" ]; then
    ok "routes 在第 ${rline} 行、第一个表头在第 ${tline} 行（在前）"
  else
    no "routes（第 ${rline:-?} 行）不在任何表头之前 ⇒ 一条路由都不会注册，部署却可能显示成功"
  fi
  grep -qE 'pattern[[:space:]]*=[[:space:]]*"xraytun\.top/api/audit"' "$CFG" \
    && ok 'pattern "xraytun.top/api/audit"（上传入口本身）' \
    || no '缺 pattern "xraytun.top/api/audit" ⇒ POST /api/audit 会 405'
  grep -qE 'pattern[[:space:]]*=.*"xraytun\.top/api/audit/\*"' "$CFG" \
    && ok 'pattern "xraytun.top/api/audit/*"（list/revoke）' \
    || no '缺 pattern "xraytun.top/api/audit/*"'
fi

# ------------------------------------------------------------------ 2) 资源与 secret
hdr "[2] 只碰 xraytun-audit-collector / xraytun-audit；AUDIT_TOKEN 只做 secret"
grep -qE '^name[[:space:]]*=[[:space:]]*"xraytun-audit-collector"' "$CFG" \
  && ok 'Worker = xraytun-audit-collector' || no 'Worker 名字不对'
grep -qE '^bucket_name[[:space:]]*=[[:space:]]*"xraytun-audit"' "$CFG" \
  && ok 'R2 桶 = xraytun-audit' || no 'R2 桶名不对'
grep -qE '^binding[[:space:]]*=[[:space:]]*"AUDIT_BUCKET"' "$CFG" \
  && ok 'binding = AUDIT_BUCKET' || no 'binding 不是 AUDIT_BUCKET'
[ -f "$SRC" ] && ok "main 指向的文件存在：$SRC" || no "main 指向的 $SRC 不存在"
if grep -nE '^[[:space:]]*AUDIT_TOKEN[[:space:]]*=' "$CFG" >/dev/null; then
  no '**AUDIT_TOKEN 被写进 wrangler.toml**（secret 泄漏！必须改用 `wrangler secret put`）'
else
  ok 'AUDIT_TOKEN 不在 [vars] 里'
fi
grep -q 'wrangler secret put AUDIT_TOKEN' "$CFG" \
  && ok '注释里有 `wrangler secret put AUDIT_TOKEN`' || no '缺 secret put 命令'
# 别的资源名（拆开写，避免本脚本自匹配）
for b in "xraytun-""incident" "eth-arb-""scout" "mymulti""cloud"; do
  if grep -rl -- "$b" "$HERE" 2>/dev/null | grep -qv '^$'; then
    no "本目录出现别的资源名「${b}」"
  else
    ok "本目录没有「${b}」"
  fi
done
# 只有指向本项目的 delete 才允许（只匹配真命令，排除检查器自身）
bad=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  printf '%s' "$line" | grep -q 'wrangler delete' && ! printf '%s' "$line" | grep -q -- '--config infra/audit-collector/wrangler.toml' && { no "越界的 delete：$line"; bad=1; }
  printf '%s' "$line" | grep -q 'r2 bucket delete' && ! printf '%s' "$line" | grep -q 'xraytun-audit' && { no "越界的 bucket delete：$line"; bad=1; }
done < <(grep -rnE '^[[:space:]]*npx wrangler delete|^[[:space:]]*npx wrangler r2 bucket delete' "$HERE" \
  --include='*.sh' --include='*.md' --include='*.toml' --exclude=verify.sh --exclude=deploy-check.sh 2>/dev/null || true)
[ "$bad" = "0" ] && ok '所有 delete 命令都只指向本项目资源'

# ------------------------------------------------------------------ 3) 隐私配置与保留期一致性
hdr "[3] 不采集请求日志；RETENTION_DAYS 与 README 的 lifecycle 天数**一致**"
if awk '/^\[observability\]/{f=1;next} /^\[/{f=0} f' "$CFG" | grep -qE '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false'; then
  ok '[observability] enabled = false'
else
  no '[observability] enabled = false 没设对（会采集请求日志）'
fi
# ⚠️ 取**引号里的值**，不要 `sed 's/[^0-9]//g'`：那一行的尾注释里也写着「400 天」，
#    去掉所有非数字会拼成 `400400`（第一版就踩了这个，判据假装不一致）。
toml_days="$(grep -E '^[[:space:]]*RETENTION_DAYS[[:space:]]*=' "$CFG" | sed -n 's/.*"\([0-9]*\)".*/\1/p' | head -1)"
readme_days="$(grep -oE 'expire-days [0-9]+' "$README" | head -1 | grep -oE '[0-9]+')"
if [ -z "$toml_days" ]; then
  no 'RETENTION_DAYS 没设'
elif [ "$toml_days" != "$readme_days" ]; then
  no "RETENTION_DAYS=${toml_days} 与 README 的 expire-days=${readme_days:-未找到} 不一致 ⇒ 两处承诺不同"
else
  ok "RETENTION_DAYS=${toml_days} 与 README lifecycle 一致"
fi
[ "$toml_days" = "$EXPECTED_RETENTION_DAYS" ] \
  && ok "保留期是本契约建议的 ${EXPECTED_RETENTION_DAYS} 天" \
  || no "保留期是 ${toml_days} 天，不是建议的 ${EXPECTED_RETENTION_DAYS}（改了就要同步改 README 与 lifecycle）"
grep -q 'r2 bucket lifecycle list xraytun-audit' "$README" \
  && ok 'README 要求 lifecycle 加完读回复核' || no 'README 缺读回复核步骤'

# ------------------------------------------------------------------ 4) base path 一致
hdr "[4] BASE_PATH 在配置与代码里一致"
toml_base="$(grep -E '^[[:space:]]*BASE_PATH[[:space:]]*=' "$CFG" | sed -n 's/.*"\(.*\)".*/\1/p')"
if [ "$toml_base" = "/api/audit" ] && grep -q "BASE_PATH: '/api/audit'" "$SRC"; then
  ok "BASE_PATH = /api/audit（$CFG 与 $SRC 一致）"
else
  no "BASE_PATH 不一致：配置=「${toml_base}」，代码里的默认值需人工核对"
fi

# ------------------------------------------------------------------ 5) 单测
hdr "[5] node --test 必须全绿（本机可跑，不需要账号）"
OUT="$(node --test "$TEST" 2>&1)"
printf '%s\n' "$OUT" | grep -E '^ℹ (tests|pass|fail)' | sed 's/^/    /'
T="$(printf '%s\n' "$OUT" | sed -n 's/^ℹ tests \([0-9]*\)$/\1/p' | head -1)"
F="$(printf '%s\n' "$OUT" | sed -n 's/^ℹ fail \([0-9]*\)$/\1/p' | head -1)"
if [ "${F:-x}" = "0" ] && [ -n "$T" ] && [ "$T" -ge 30 ]; then
  ok "${T} 个用例全过"
else
  no "单测不是全绿（tests=${T:-?} fail=${F:-?}）"
fi

hdr "结果"
echo "  pass=$pass fail=$fail"
if [ "$fail" = "0" ]; then
  echo "  ✓ 静态自检通过（**这不等于端点可用**：部署后必须按 README §3.4 复核路由与 401）"
  exit 0
fi
echo "  ✗ 静态自检失败（见上面每一条 ✗）"
exit 1
