#!/usr/bin/env bash
#
# 部署后自检：**部署日志说成功 ≠ 端点能用**。
#
# 为什么需要它（2026-09-23 两次真实事故）：
#   · `routes` 写在了 TOML 的表作用域里 ⇒ 一条路由都没注册，**部署输出看起来是成功的**，
#     而 `POST https://xraytun.top/api/incident` 返回 **405**（请求落到了 Pages）；
#   · 只注册了 `xraytun.top/api/incident/*` ⇒ 匹配不到上传入口 `/api/incident` 本身 ⇒ 同样 **405**。
# 这两条本地 `verify.sh`（23 个单测）与 `smoke.sh`（真 workerd）**都抓不到** —— 它们不经过路由。
# ⇒ 这是「本地绿 ≠ 运行时绿」的第三次：**必须有一条跑在真实部署之后的自检**。
#
# 用法：
#   ./infra/incident-collector/deploy-check.sh                       # 不做上传（不落盘）
#   ./infra/incident-collector/deploy-check.sh --deploy-log deploy.log
#   CLOUDFLARE_API_TOKEN=… ./infra/incident-collector/deploy-check.sh # 额外查路由注册
#   INCIDENT_TOKEN=… ./infra/incident-collector/deploy-check.sh --upload   # 完整环回（会建+删一条记录）
#
# 默认**不会**往 R2 写任何东西：只用「脏包」探活（期望 422，天然不落盘）。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE="${BASE:-https://xraytun.top/api/incident}"
ZONE_ID="${CLOUDFLARE_ZONE_ID:-d5c9855127ff057a8f2c640f51024663}"   # 非秘密（lead 实测给出）
WORKER_NAME="xraytun-incident-collector"
DEPLOY_LOG=""
DO_UPLOAD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --deploy-log) DEPLOY_LOG="${2:-}"; shift 2 ;;
    --upload) DO_UPLOAD=1; shift ;;
    -h | --help) sed -n '3,20p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

pass=0; fail=0; skip=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
sk() { echo "  ⏭ $*"; skip=$((skip + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/deploy-check.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

# 两个 fixture：干净包与**含密钥**的包（脏包用来探活，且保证不落盘）
python3 - "$TMP" <<'PYEOF'
import sys, zipfile, pathlib
d = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(d / 'dirty.zip', 'w') as z:
    z.writestr('nodes.txt', 'vless://11111111-2222-3333-4444-555555555555@example.com:443#node\n')
with zipfile.ZipFile(d / 'clean.zip', 'w') as z:
    z.writestr('README.txt', 'XrayTun incident bundle (deploy-check fixture)\nversion=0.8.35\n')
PYEOF
SECRET_MARK='11111111-2222-3333-4444-555555555555'

echo "部署后自检：BASE=$BASE  worker=$WORKER_NAME"

# ------------------------------------------------------------------ 1) 部署日志
hdr "[1] 部署日志里**不该**出现「路由没注册」的迹象"
if [ -z "$DEPLOY_LOG" ]; then
  sk "没给 --deploy-log：跳过日志检查（可手动看：不得出现 env.routes 行、不得出现 Unexpected fields）"
elif [ ! -f "$DEPLOY_LOG" ]; then
  no "--deploy-log 指向的文件不存在：$DEPLOY_LOG"
else
  bad_env="$(grep -nE '(^|[[:space:]])env\.routes' "$DEPLOY_LOG" || true)"
  bad_uf="$(grep -n 'Unexpected fields' "$DEPLOY_LOG" || true)"
  if [ -n "$bad_env" ]; then
    no "日志里出现「env.routes」⇒ routes 被当成了环境变量（路由不会注册）"; printf '%s\n' "$bad_env" | sed 's/^/       /'
  else
    ok "日志里没有 env.routes"
  fi
  if [ -n "$bad_uf" ]; then
    no "日志里出现「Unexpected fields」⇒ routes 落进了别的表"; printf '%s\n' "$bad_uf" | sed 's/^/       /'
  else
    ok "日志里没有 Unexpected fields"
  fi
fi

# ------------------------------------------------------------------ 2) 路由注册
hdr "[2] zone 上必须能看到**两条** pattern（用 CF API 读回，不信部署日志）"
if [ -z "${CLOUDFLARE_API_TOKEN:-}" ]; then
  sk "没有 CLOUDFLARE_API_TOKEN：跳过（我**不会**去读 ~/.cf-incident-token；由部署者带环境变量跑这一步）"
  echo "      手动命令：curl -sS -H \"Authorization: Bearer \$CLOUDFLARE_API_TOKEN\" \\"
  echo "                 https://api.cloudflare.com/client/v4/zones/$ZONE_ID/workers/routes"
else
  ROUTES_JSON="$TMP/routes.json"
  curl -sS -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
    "https://api.cloudflare.com/client/v4/zones/$ZONE_ID/workers/routes" -o "$ROUTES_JSON"
  python3 - "$ROUTES_JSON" "$WORKER_NAME" <<'PYEOF' && ok "两条 pattern 都已注册，且指向 $WORKER_NAME" || no "路由注册不完整（见上面的原始输出）"
import json, sys
path, worker = sys.argv[1], sys.argv[2]
raw = open(path, encoding='utf-8').read()
try:
    d = json.loads(raw)
except Exception as e:
    print(f"      无法解析 API 响应：{e}；原始前 300 字：{raw[:300]}"); sys.exit(1)
if not d.get('success'):
    print(f"      API 失败：{json.dumps(d.get('errors'), ensure_ascii=False)[:300]}"); sys.exit(1)
rs = [r for r in d.get('result') or [] if worker in json.dumps(r, ensure_ascii=False)]
pats = sorted({r.get('pattern') for r in rs})
print(f"      实测 pattern：{pats}")
print(f"      （该 worker 的路由条目 {len(rs)} 条；zone 上共 {len(d.get('result') or [])} 条）")
want = {'xraytun.top/api/incident', 'xraytun.top/api/incident/*'}
missing = want - set(pats)
if missing:
    print(f"      ✗ 缺少：{sorted(missing)} —— 少了前者 ⇒ POST /api/incident 会落到 Pages 返回 405")
    sys.exit(1)
PYEOF
fi

# ------------------------------------------------------------------ 3) HTTP 探活（不落盘）
hdr "[3] 真请求：POST 入口必须由 Worker 处理（脏包 ⇒ 期望 422，且**不落盘**）"
CODE="$(curl -sS -o "$TMP/dirty.json" -w '%{http_code}' -X POST "$BASE" \
          -H 'content-type: application/zip' --data-binary @"$TMP/dirty.zip")"
echo "      POST $BASE → HTTP $CODE"
sed -n '1,12p' "$TMP/dirty.json" | sed 's/^/       /'
case "$CODE" in
  200 | 201 | 202) no "脏包被接受了（期望 422）—— 隐私拒收没生效？" ;;
  405) no "HTTP 405 ⇒ **请求落到了 Pages，Worker 路由没命中**（`routes` 作用域或 pattern 缺失）" ;;
  404) no "HTTP 404 ⇒ 没命中 Worker 路由" ;;
  422) ok "HTTP 422 —— 请求确实进了 Worker（Pages 不会给 422），且脏包不会落盘" ;;
  *) no "意外的状态码：$CODE" ;;
esac
if printf '%s' "$(cat "$TMP/dirty.json")" | grep -q '"secret_detected"'; then
  ok "响应体是 secret_detected（命中信息只有 type/file/line）"
else
  no "响应体不是 secret_detected"
fi
if grep -q "$SECRET_MARK" "$TMP/dirty.json"; then
  no "**响应体里出现了密钥原文**（泄漏面！）"
else
  ok "响应体里没有密钥原文"
fi

# ------------------------------------------------------------------ 4) 可选：完整环回（会建一条记录，带 token 时自动删除）
if [ "$DO_UPLOAD" = "1" ]; then
  hdr "[4] 完整环回：上传干净包 → manifest → blob(带/不带 token) → DELETE"
  UP_CODE="$(curl -sS -o "$TMP/up.json" -w '%{http_code}' -X POST "$BASE" \
              -H 'content-type: application/zip' --data-binary @"$TMP/clean.zip")"
  echo "      POST → $UP_CODE"; sed -n '1,8p' "$TMP/up.json" | sed 's/^/       /'
  ID="$(sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' "$TMP/up.json" | head -1)"
  if [ "$UP_CODE" = "201" ] && [ -n "$ID" ]; then
    ok "上传 201，id=$ID"
    echo "      MANIFEST → $(curl -sS -o /dev/null -w '%{http_code}' "$BASE/$ID")（期望 200）"
    echo "      BLOB 无 token → $(curl -sS -o /dev/null -w '%{http_code}' "$BASE/$ID/blob")（期望 401）"
    if [ -n "${INCIDENT_TOKEN:-}" ]; then
      echo "      BLOB 带 token → $(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $INCIDENT_TOKEN" "$BASE/$ID/blob")（期望 200）"
      DEL_CODE="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "X-Auth-Token: $INCIDENT_TOKEN" "$BASE/$ID")"
      echo "      DELETE → ${DEL_CODE}（期望 200）"
      echo "      DELETE 后 MANIFEST → $(curl -sS -o /dev/null -w '%{http_code}' "$BASE/$ID")（期望 404）"
      [ "$DEL_CODE" = "200" ] && ok "已删除，没有残留" || no "删除失败（残留 ${ID}）"
    else
      sk "没有 INCIDENT_TOKEN：**已留一条记录**（id=${ID}，180 B，30 天后自动过期）—— 要清就用它调 DELETE"
    fi
  else
    no "上传失败（code=${UP_CODE}）"
  fi
else
  hdr "[4] 完整环回（默认跳过，避免往 R2 写东西）"
  sk "未加 --upload：只做了非落盘探活；要跑完整环回加 --upload（带 INCIDENT_TOKEN 会自动删）"
fi

hdr "结果"
echo "  pass=$pass fail=$fail skip=$skip"
if [ "$fail" = "0" ]; then
  echo "  ✓ 部署后自检通过（部署日志说成功 ≠ 端点能用 —— 这三条才是判据）"
  exit 0
fi
echo "  ✗ 部署后自检失败（见上面每一条 ✗；405 通常就是路由没注册或 pattern 少了裸路径那条）"
exit 1
