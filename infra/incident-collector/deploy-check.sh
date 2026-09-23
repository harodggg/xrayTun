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
DO_SELF_TEST=0
# **强制把返回的 id 存下来**：2026-09-23 tester 用 `-o /dev/null` 丢了 201 的响应 ⇒
# 拿不到 id、删不掉那个测试包，只能等 30 天过期（lead 只好用 R2 API 逐对象清）。
ID_FILE="${ID_FILE:-${TMPDIR:-/tmp}/xraytun-incident-uploaded-ids.txt}"
UPLOADED_IDS=""
NOCLEAN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --deploy-log) DEPLOY_LOG="${2:-}"; shift 2 ;;
    --upload) DO_UPLOAD=1; shift ;;
    --self-test) DO_SELF_TEST=1; shift ;;
    -h | --help) sed -n '3,20p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

pass=0; fail=0; skip=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
sk() { echo "  ⏭ $*"; skip=$((skip + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

# 从响应体里取 id：**取到了就打印 + 落盘 + 给出 DELETE 命令**；
# 若是 2xx 却没有 id ⇒ 明确报「无法清理」（绝不静默留一个删不掉的对象）。
save_id_from_body() { # $1=body 文件  $2=HTTP 码
  local body="$1" code="$2" id
  id="$(sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' "$body" | head -1)"
  if [ -n "$id" ]; then
    echo "      ID=$id"
    printf '%s\t%s\t%s\n' "$(date '+%Y-%m-%d %H:%M:%S%z')" "$id" "$BASE" >>"$ID_FILE"
    echo "      已记录到 ${ID_FILE}（删不掉的包凭它去 DELETE）"
    echo "      手动删除：curl -X DELETE -H 'X-Auth-Token: <端点令牌>' $BASE/$id"
    UPLOADED_IDS="$UPLOADED_IDS $id"
    return 0
  fi
  case "$code" in
    2*)
      echo "  ✗ **无法清理**：服务端返回 ${code}，但响应里没有 id ⇒ 可能已落盘一个我们删不掉的对象" >&2
      echo "      原始响应（前 200 字）：$(head -c 200 "$body")" >&2
      NOCLEAN=1
      return 75
      ;;
  esac
  return 0
}

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


# ------------------------------------------------------------------ --self-test
#
# 用**本地 mock 服务**验证这个工具自己的安全行为（不碰 Cloudflare、不往 R2 写东西）：
#   A) 服务端返回带 id 的 201 ⇒ id 必须被打印 + 落盘，并且**自动 DELETE**
#   B) 服务端返回**不带 id** 的 201 ⇒ 必须明确报「无法清理」并以非 0 退出（绝不静默留下对象）
if [ "$DO_SELF_TEST" = "1" ] && [ -z "${SELF_TEST_CHILD:-}" ]; then
  hdr "自测：本地 mock 服务（验证 id 记录 / 无法清理）"
  MOCK="$TMP/mock.py"
  cat >"$MOCK" <<'PYEOF'
import http.server, json, os, sys, threading
PORT, MODE, DEL_LOG = int(sys.argv[1]), sys.argv[2], sys.argv[3]
state = {}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):  # 静音
        pass
    def _send(self, code, body, ctype='application/json'):
        raw = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code); self.send_header('content-type', ctype)
        self.send_header('content-length', str(len(raw))); self.end_headers(); self.wfile.write(raw)
    def do_POST(self):
        n = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(n)
        # 像真端点一样：含 vless:// 的包 ⇒ 422（自测里的「脏包」就是这一份）
        if b'vless://' in body:
            return self._send(422, {'error': 'secret_detected', 'hits': [{'type': 'node_url', 'file': 'nodes.txt', 'line': 1}]})
        if MODE == 'with-id':
            state['id'] = 'INC-SELFTEST-0001'
            self._send(201, {'id': state['id'], 'sha256': 'x' * 64, 'received_at': '2026-09-23T00:00:00Z', 'bytes': 123})
        else:
            self._send(201, {'ok': True, 'note': 'server gave no id'})
    def do_GET(self):
        if self.path.endswith('/blob'):
            if self.headers.get('X-Auth-Token') == 'dummy-token':
                self._send(200, b'PK\x03\x04fake', 'application/zip')
            else:
                self._send(401, {'error': 'unauthorized'})
        elif state.get('id') and self.path.rstrip('/').endswith(state['id']):
            if state.get('deleted'):
                self._send(404, {'error': 'not_found'})
            else:
                self._send(200, {'id': state['id'], 'schema_version': 1})
        else:
            self._send(404, {'error': 'not_found'})
    def do_DELETE(self):
        if self.headers.get('X-Auth-Token') != 'dummy-token':
            return self._send(401, {'error': 'unauthorized'})
        state['deleted'] = True
        with open(DEL_LOG, 'a') as f:
            f.write(self.path + '\n')
        self._send(200, {'deleted': True})
srv = http.server.HTTPServer(('127.0.0.1', PORT), H)
print('READY', flush=True)
srv.serve_forever()
PYEOF
  self_case() { # $1=mode  $2=port  $3=期望(ok|noid)
    # ⚠️ 必须分开赋值：bash 3.2 会把 `local` 里所有词**先展开再赋值**，
    # 同一行里的 `$mode` 还是空的（`set -u` 下直接 unbound）。
    local mode port want del_log ids out rc
    mode="$1"; port="$2"; want="$3"
    del_log="$TMP/del.$mode.log"; ids="$TMP/ids.$mode.txt"
    : >"$del_log"; : >"$ids"
    python3 "$MOCK" "$port" "$mode" "$del_log" >"$TMP/mock.$mode.log" 2>&1 &
    local mpid=$!
    for i in $(seq 1 40); do grep -q READY "$TMP/mock.$mode.log" 2>/dev/null && break; sleep 0.1; done
    out="$(SELF_TEST_CHILD=1 BASE="http://127.0.0.1:$port/api/incident" INCIDENT_TOKEN=dummy-token \
           ID_FILE="$ids" "$0" --upload 2>&1)"; rc=$?
    kill -TERM "$mpid" 2>/dev/null; wait "$mpid" 2>/dev/null
    echo "    —— case ${mode}（rc=${rc}）——"
    printf '%s\n' "$out" | grep -E "ID=|已记录到|无法清理|已删除|上传 201" | sed 's/^/      /'
    if [ "$want" = "ok" ]; then
      printf '%s\n' "$out" | grep -q "ID=INC-SELFTEST-0001" && ok "带 id 的 201：id 被打印" || no "带 id 的 201：没打印 id"
      grep -q "INC-SELFTEST-0001" "$ids" && ok "id 已落盘（${ids}）" || no "id 没落盘"
      grep -q "INC-SELFTEST-0001" "$del_log" && ok "自动 DELETE 真的发出去了" || no "没有自动 DELETE"
      [ "$rc" = "0" ] && ok "带 id 的 201：退出码 0" || no "带 id 的 201：退出码 ${rc}（期望 0）"
    else
      printf '%s\n' "$out" | grep -q "无法清理" && ok "缺 id 的 201：明确报「无法清理」" || no "缺 id 的 201：没报「无法清理」"
      [ "$rc" != "0" ] && ok "缺 id 的 201：以非 0 退出（不静默）" || no "缺 id 的 201：竟然退出 0"
    fi
  }
  self_case with-id 18971 ok
  self_case no-id 18972 noid
  echo
  echo "  pass=$pass fail=$fail"
  [ "$fail" = "0" ] && { echo "  ✓ deploy-check 自测通过"; exit 0; }
  echo "  ✗ deploy-check 自测失败"; exit 1
fi

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
save_id_from_body "$TMP/dirty.json" "$CODE" || no "脏包响应异常（见上面的「无法清理」）"

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
  save_id_from_body "$TMP/up.json" "$UP_CODE" || true
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
[ "$NOCLEAN" = "1" ] && fail=$((fail + 1))
echo "  pass=$pass fail=$fail skip=$skip"
[ -n "$UPLOADED_IDS" ] && echo "  本次创建过的 id：${UPLOADED_IDS}（记录在 ${ID_FILE}）"
if [ "$fail" = "0" ]; then
  echo "  ✓ 部署后自检通过（部署日志说成功 ≠ 端点能用 —— 这三条才是判据）"
  exit 0
fi
echo "  ✗ 部署后自检失败（见上面每一条 ✗；405 通常就是路由没注册或 pattern 少了裸路径那条）"
exit 1
