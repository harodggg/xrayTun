#!/usr/bin/env bash
# verify-speed-read-page-live.sh —— 「一目十行 · SpeedRead」相关项目页的**线上**验收（只读）
#
# 为什么存在：本地能证明「提交是对的」，但证明不了「线上跑的是这份提交」。
# 而本仓库踩过一个具体的坑：**apex 上不存在的路径也可能返回 200 + 首页 HTML**（SPA 兜底），
# 所以「HTTP 200 ⇒ 资产存在」这条判据在 xraytun.top 上会给出假绿。
#
# 判据（**双判，不看状态码**）：
#   · 200 但正文与首页逐字节相同                ⇒ 兜底，不是我们的页面
#   · 200 且含页面专属标题、canonical、结构化数据 ⇒ 真页面
#   · 不存在的路径必须 404                      ⇒ 证明上面的 200 可信（兜底对照）
#   · 下载件 / 截图 / LICENSE 与仓库里的文件**逐字节比对**（cmp）
#   · LICENSE 必须是 text/plain                 ⇒ 证明 _headers 覆写在线上生效
#
# 用法：
#   bash docs/verification/verify-speed-read-page-live.sh
#   BASE=https://xraytun.top SITE_DIR=/path/to/repo bash ...   # 也可只验收镜像
#
# 退出码：0 = 全通过；1 = 有未通过项。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SITE_DIR="${SITE_DIR:-$ROOT}"
BASE="${BASE:-https://xraytun.top}"
MIRROR="${MIRROR:-https://harodggg.github.io/xrayTun}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ✓ $*"; }
bad() { fail=$((fail+1)); echo "  ✗ $*" >&2; }
get() { curl -sS --max-time 30 -D "$TMP/h" -o "$TMP/b" -w "%{http_code}" "$1"; }
ctype() { grep -i '^content-type:' "$TMP/h" | tail -1 | sed 's/.*: *//' | tr -d '\r'; }

echo "== A) 两个页面（内容判据，不看状态码）=="
curl -sS --max-time 30 -o "$TMP/home.html" "$BASE/"
for spec in "/speed-read/|一目十行 · SpeedRead：把整个网页压成 3 条" \
            "/en/speed-read/|SpeedRead: compress a whole web page into 3 items"; do
  path="${spec%%|*}"; marker="${spec#*|}"
  code=$(get "$BASE$path"); ct=$(ctype); bytes=$(wc -c <"$TMP/b" | tr -d ' ')
  if [ "$code" != "200" ]; then bad "$path → $code"; continue; fi
  case "$ct" in text/html*) ok "$path → 200 text/html, $bytes 字节" ;; *) bad "$path content-type 异常：$ct" ;; esac
  grep -qF "$marker" "$TMP/b" && ok "$path 含页面专属标题" || bad "$path 找不到专属标题"
  cmp -s "$TMP/b" "$TMP/home.html" && bad "$path 正文与首页相同（是兜底）" || ok "$path 正文与首页不同（不是兜底）"
  grep -q '<link rel="canonical"' "$TMP/b" && ok "$path 含 canonical" || bad "$path 缺 canonical"
  grep -q '"@type": "SoftwareApplication"' "$TMP/b" && ok "$path 含 SoftwareApplication" || bad "$path 缺结构化数据"
done

echo "== B) 下载件与静态资源（与提交逐字节比对）=="
for spec in "speed-read/speed-read-extension-0.1.0.zip|application/zip" \
            "speed-read/LICENSE|text/plain" \
            "speed-read/preview-page.png|image/png" \
            "speed-read/preview-panel.png|image/png"; do
  rel="${spec%%|*}"; want="${spec#*|}"
  code=$(get "$BASE/$rel"); ct=$(ctype)
  if [ "$code" != "200" ]; then bad "/$rel → $code"; continue; fi
  case "$ct" in *"$want"*) ok "/$rel → 200 $ct" ;; *) bad "/$rel content-type 应为 $want，实际 $ct" ;; esac
  if [ -f "$SITE_DIR/site/$rel" ]; then
    if cmp -s "$TMP/b" "$SITE_DIR/site/$rel"; then ok "/$rel 与提交逐字节一致（$(wc -c <"$TMP/b" | tr -d ' ') 字节）"
    else bad "/$rel 与提交不一致：线上 $(wc -c <"$TMP/b"|tr -d ' ') vs 仓库 $(wc -c <"$SITE_DIR/site/$rel"|tr -d ' ')"; fi
  else
    echo "  · 仓库里没有 site/$rel，跳过逐字节比对"
  fi
done

echo "== C) 发现入口（含「不许挤掉已有外部项目」）=="
for f in sitemap.xml llms.txt llms-full.txt; do
  curl -sS --max-time 30 -o "$TMP/$f" "$BASE/$f"
  for needle in 'speed-read/' 'beauty-meter/' 'jev-x-filter/'; do
    if grep -q "$needle" "$TMP/$f"; then ok "/$f 收录 $needle"; else bad "/$f 缺 $needle"; fi
  done
done
grep -q 'href="speed-read/"' "$TMP/home.html" && ok "首页导航含 speed-read/" || bad "首页导航缺 speed-read/"
curl -sS --max-time 30 -o "$TMP/en.html" "$BASE/en/"
grep -q 'href="speed-read/"' "$TMP/en.html" && ok "英文首页导航含 speed-read/" || bad "英文首页导航缺 speed-read/"

echo "== D) 兜底对照（证明 200 不是 SPA 兜底）=="
code=$(get "$BASE/speed-read/__does_not_exist__")
if [ "$code" = "404" ]; then ok "不存在的路径 → 404（本 zone 无 SPA 兜底，上面的 200 可信）"
else bad "不存在的路径返回 $code —— 200 判据不可信，需改用内容指纹"; fi

echo "== E) GitHub Pages 镜像（子路径，验证没有根绝对路径）=="
code=$(get "$MIRROR/speed-read/")
[ "$code" = "200" ] && ok "镜像 /speed-read/ → 200（$(wc -c <"$TMP/b"|tr -d ' ') 字节）" || bad "镜像 /speed-read/ → $code"
code=$(get "$MIRROR/speed-read/speed-read-extension-0.1.0.zip")
if [ "$code" = "200" ] && cmp -s "$TMP/b" "$SITE_DIR/site/speed-read/speed-read-extension-0.1.0.zip"; then
  ok "镜像 zip 与提交逐字节一致"
else bad "镜像 zip 异常（$code）"; fi

echo "== F) 线上正文与提交的差异（zone 级注入是预期差异）=="
curl -sS --max-time 30 -o "$TMP/live.html" "$BASE/speed-read/"
if diff <(sed 's/[[:space:]]*$//' "$SITE_DIR/site/speed-read/index.html") <(sed 's/[[:space:]]*$//' "$TMP/live.html") >"$TMP/d.txt" 2>&1; then
  ok "线上正文与提交逐行一致（连 zone 级注入都没有）"
else
  echo "  · 差异 $(grep -c '^[<>]' "$TMP/d.txt") 行（预期为 Cloudflare Web Analytics 的 zone 级注入）："
  grep '^[<>]' "$TMP/d.txt" | cut -c1-160 | head -6 | sed 's/^/      /'
  if grep -q 'beacon.min.js' "$TMP/d.txt"; then ok "唯一差异是 zone 级注入的 beacon.min.js（首页同样有）"
  else bad "线上正文与提交存在非预期差异"; fi
fi

echo
if [ "$fail" -ne 0 ]; then echo "✗ 线上验收未通过（$fail 项失败 / 共 $((pass+fail)) 项）" >&2; exit 1; fi
echo "✓ 线上验收全部通过（$pass 项）"
