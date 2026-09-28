#!/usr/bin/env bash
#
# workerd 冒烟测试：用**真的 Workers 运行时**（`wrangler dev --local` = Miniflare/workerd）
# 起一个最小请求，专门抓「本地单测抓不到」的那类运行时限制。
#
# # 为什么需要它（一次真实的部署事故）
#
# 2026-09-22 incident-collector 首次部署被 Cloudflare 拒了：
#
#     Uncaught Error: Disallowed operation called within global scope.
#     Asynchronous I/O (ex: fetch() or connect()), setting a timeout, and generating random values
#     are not allowed within global scope.                                     [code: 10021]
#
# 起因是 worker 在**模块顶层**用 `crypto.getRandomValues` 生成限流盐。
# **`node --test` 全绿也抓不到它** —— node 不执行 workerd 的全局作用域限制。
# ⇒ 教训：**本地绿 ≠ 运行时绿**。本端点照抄了同样的惰性盐写法，所以同样需要这条冒烟。
#
# 用法：
#     ./infra/audit-collector/smoke.sh                # 绿：真运行时能起来并正常响应（401 JSON）
#     ./infra/audit-collector/smoke.sh --sensitivity  # 把「顶层取随机值」这个 bug 回退 ⇒ 必须红
#
# 需要 `npx`（会拉 wrangler@3，**只跑本地模式，不碰 Cloudflare 账号、不写任何远程资源**）。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="green"
[ "${1:-}" = "--sensitivity" ] && MODE="sensitivity"

# 这台机器 `~/.npm` 里有 root 拥有的文件 ⇒ npx 会 EPERM。
# **不要去 `sudo chown`**（那是用户的机器）：换一个**可写**的缓存目录即可。
# 注意：环境里可能已经设了 `npm_config_cache=/Users/<user>/.npm`（就是那个坏的）⇒
# 这里**验可写性**，可写才沿用，否则换到临时目录（实测：不换就必然 EPERM）。
_cc="${npm_config_cache:-}"
if [ -n "$_cc" ] && mkdir -p "$_cc" 2>/dev/null && [ -w "$_cc" ]; then
  export npm_config_cache="$_cc"
else
  export npm_config_cache="${TMPDIR:-/tmp}/dsh-npm-cache"
fi
unset _cc
export WRANGLER_LOG_PATH="${WRANGLER_LOG_PATH:-${TMPDIR:-/tmp}/dsh-wrangler-logs}"
mkdir -p "$npm_config_cache" "$WRANGLER_LOG_PATH"

BASE_PORT=$(( (RANDOM % 500) + 19400 ))
BASE="http://127.0.0.1:${BASE_PORT}/api/audit"
CFG="$HERE/wrangler.toml"
WHAT="真源码"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/audit-smoke.XXXXXX")"
DEV_PID=""
cleanup() {
  [ -n "$DEV_PID" ] && kill -TERM "$DEV_PID" 2>/dev/null
  pkill -f "wrangler@3 dev .*$BASE_PORT" 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

if [ "$MODE" = "sensitivity" ]; then
  WHAT="把「顶层取随机值」的 bug 回退后的变体"
  python3 - "$HERE/src/worker.mjs" "$TMP/bug.mjs" <<'PYEOF'
import pathlib, re, sys
src, out = sys.argv[1], sys.argv[2]
s = pathlib.Path(src).read_text(encoding='utf-8')
# 回退成事故里的写法：顶层 const rateSalt = (() => { crypto.getRandomValues(...) })();
s2 = s.replace("let rateSalt = null;", """const rateSalt = (() => {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join('');
})();""", 1)
s2 = re.sub(r"function getRateSalt\(\) \{.*?\n\}\n\n", "", s2, flags=re.S)
s2 = s2.replace("${getRateSalt()}", "${rateSalt}")
assert s2 != s, '没能构造出带 bug 的变体'
assert 'crypto.getRandomValues' in s2.split('export async function handle', 1)[0], 'bug 没落在模块顶层'
pathlib.Path(out).write_text(s2, encoding='utf-8')
PYEOF
  CFG="$TMP/wrangler.toml"
  python3 - "$HERE/wrangler.toml" "$CFG" "$TMP/bug.mjs" <<'PYEOF'
import pathlib, re, sys
cfg_src, cfg_out, main = sys.argv[1], sys.argv[2], sys.argv[3]
c = pathlib.Path(cfg_src).read_text(encoding='utf-8')
c = re.sub(r'main = "[^"]*"', f'main = "{main}"', c)
c = re.sub(r'routes = \[[^\]]*\]', '', c)   # 本地 dev 不需要路由
pathlib.Path(cfg_out).write_text(c, encoding='utf-8')
PYEOF
fi

echo "workerd 冒烟：MODE=${MODE}（对象：${WHAT}）"
echo "  config=$CFG  port=$BASE_PORT  npm_config_cache=$npm_config_cache"
echo

LOG="$TMP/dev.log"
( cd "$HERE" && npx --yes wrangler@3 dev --local --ip 127.0.0.1 --port "$BASE_PORT" --config "$CFG" >"$LOG" 2>&1 ) &
DEV_PID=$!
echo "  已启动 wrangler dev（pid=${DEV_PID}），等它起来…"

ready=0
for i in $(seq 1 60); do
  if grep -q "Ready on" "$LOG" 2>/dev/null; then ready=1; break; fi
  if grep -q "The Workers runtime failed to start" "$LOG" 2>/dev/null; then break; fi
  sleep 2
done
echo "  （等 Ready：$([ "$ready" = 1 ] && echo 已就绪 || echo 未就绪)）"

CODE=""
BODY=""
# 不带 token 的 POST ⇒ 走完路由 + 鉴权，期望本端点的 401 JSON（不是运行时错误页）
for i in $(seq 1 8); do
  CODE="$(curl -sS -o "$TMP/body.json" -w '%{http_code}' --max-time 5 -X POST \
            -H 'content-type: application/json' --data '{}' "$BASE" 2>/dev/null || true)"
  if [ -n "$CODE" ] && [ "$CODE" != "000" ]; then
    BODY="$(cat "$TMP/body.json" 2>/dev/null || true)"
    break
  fi
  sleep 2
done

echo "  HTTP 响应码：${CODE:-（无响应）}"
[ -n "$BODY" ] && echo "  响应体：$(printf '%s' "$BODY" | tr -d '\n' | cut -c1-200)"
if grep -qiE "Disallowed operation called within global scope" "$LOG"; then
  echo "  ⚠️ 运行时日志里出现了「在全局作用域里做禁止操作」："
  grep -iE "Disallowed operation called within global scope" "$LOG" | head -1 | sed 's/^/       /'
fi
grep -q "The Workers runtime failed to start" "$LOG" && echo "  ⚠️ 运行时日志：The Workers runtime failed to start"

answered=0
# 期望 401 + 我们的 JSON（fail closed）；Pages/运行时错误页都不会是这个形状
if [ "$CODE" = "401" ] && printf '%s' "$BODY" | grep -q '"error"'; then answered=1; fi

echo
if [ "$MODE" = "green" ]; then
  if [ "$answered" = "1" ]; then
    echo "  ✓ 真运行时起来了，并且用我们的 JSON 回话（401 unauthorized）——说明模块能加载、handler 能跑"
    exit 0
  fi
  echo "  ✗ 真运行时没能正常回话 —— 见上面的响应码与日志尾部"
  [ -f "$LOG" ] && tail -25 "$LOG" | sed 's/^/       /'
  exit 1
else
  if [ "$answered" = "0" ]; then
    echo "  ✓ 敏感性成立：把「顶层取随机值」回退后，真运行时**起不来 / 不回话** ⇒ 这条冒烟测试抓得住它"
    exit 0
  fi
  echo "  ✗ 敏感性不成立：带 bug 的变体竟然正常回话了 —— 冒烟测试抓不到这类问题"
  exit 1
fi
