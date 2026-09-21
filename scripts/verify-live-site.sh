#!/usr/bin/env bash
# verify-live-site.sh —— 发版后**可反复跑**的线上站点验收（只读）
#
# 为什么存在这个脚本（这是它的全部理由）：
#   v0.8.31 验收时发现 `https://xraytun.top/` 上**不存在的路径也返回 200 + 首页 HTML**
#   （Cloudflare Pages 的 SPA 兜底）。于是「HTTP 200 ⇒ 资产存在」这条我们一直在用的判据
#   **在 apex 上会给出假绿**。本项目的一贯做法是「教训变成机制」，这个脚本就是那次教训的产物。
#
# 判据（**双判，不看状态码**）：
#   · 状态 404/410                                  ⇒ 不存在（正确）
#   · 200 但 content-type 与期望不符（例：要 image/png 却拿到 text/html）
#                                                   ⇒ **软 404 / 内容不对**（不是「存在」）
#   · 200 + text/html + body sha == 首页指纹         ⇒ **软 404**（首页兜底）
#   · 200 + 期望的 content-type + 不是兜底体          ⇒ 真存在
#
#   ⚠️ 比卡面更强的现实（我在 v0.8.31 后实测到）：兜底体**不一定等于当前首页**。
#      `og-image-0.8.30.png` 命中的是 CDN 里**陈旧的** index.html（43696 B / sha 1e1265…），
#      与当时首页（43980 B / sha a02e24…）**不相等**。
#      ⇒ 只比「当前首页指纹」会把它误判成「存在」；所以本脚本**先看 content-type 族**，
#        再用指纹辅助，并把遇到的**每一个** HTML 兜底指纹都列出来（不假设只有一个）。
#
# 用法：
#   VER=0.8.31 PREV=0.8.30 ./scripts/verify-live-site.sh
#   ./scripts/verify-live-site.sh --ver 0.8.31 --prev 0.8.30
#   ALLOW_STALE_LATEST=1 ./scripts/verify-live-site.sh   # 允许 releases/latest 还没指向新版本
#   SKIP_MIRROR=1 ...                                    # 跳过 GitHub Pages 镜像对照
#
# 退出码：0 = 全通过；1 = 发现问题；2 = 用法/参数错误
#
# 只读：不写仓库、不改站点；临时文件都在 mktemp 目录里。

set -u

BASE=${BASE:-https://xraytun.top}
MIRROR=${MIRROR:-https://harodggg.github.io/xrayTun}
RELEASES=${RELEASES:-https://github.com/harodggg/xrayTun/releases}
VER=${VER:-}
PREV=${PREV:-}
ALLOW_STALE_LATEST=${ALLOW_STALE_LATEST:-0}
SKIP_MIRROR=${SKIP_MIRROR:-0}
TIMEOUT=${TIMEOUT:-30}

usage() { sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --ver)  VER=${2:-}; shift 2 ;;
    --prev) PREV=${2:-}; shift 2 ;;
    --base) BASE=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [ -z "$VER" ] || [ -z "$PREV" ]; then
  echo "✗ 必须给出目标版本与上一版本（不写死在脚本里，否则下一版就过期）：" >&2
  echo "    VER=0.8.31 PREV=0.8.30 $0          # 或 --ver 0.8.31 --prev 0.8.30" >&2
  exit 2
fi

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t verify-live-site)
trap 'rm -rf "$TMP"' EXIT

PROBLEMS=0
WARNINGS=0
HTML_FALLBACK_SHAS=""   # 见过的 HTML 兜底体指纹（空格分隔）

problem() { echo "  ✗ $*"; PROBLEMS=$((PROBLEMS+1)); }
warn()    { echo "  ⚠️  WARNING: $*"; WARNINGS=$((WARNINGS+1)); }
ok()      { echo "  ✓ $*"; }

sha256_of() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1; }

# fetch <url> <outfile> —— 发一个请求，回填全局 CODE / CT / SHA / BYTES
fetch() {
  local url=$1 out=$2 hdrs="${2}.hdr"
  CODE=$(curl -s -L --max-time "$TIMEOUT" -D "$hdrs" -o "$out" -w '%{http_code}' "$url" 2>/dev/null)
  CT=$(tr -d '\r' < "$hdrs" 2>/dev/null | grep -i '^content-type:' | tail -1 | cut -d' ' -f2-)
  BYTES=$(wc -c < "$out" 2>/dev/null | tr -d ' ')
  SHA=$(sha256_of "$out")
  [ -n "$SHA" ] || SHA="(空)"
  echo "    \$ curl -s -L -D hdr -o body -w '%{http_code}' $url"
  echo "      → HTTP $CODE · content-type: ${CT:-（无）} · $BYTES bytes · sha256 ${SHA:0:16}…"
}

ct_matches() { # <actual> <expected-family>
  case "$2" in
    png)  case "$1" in image/png*) return 0 ;; esac ;;
    txt)  case "$1" in text/plain*) return 0 ;; esac ;;
    xml)  case "$1" in application/xml*|text/xml*) return 0 ;; esac ;;
    html) case "$1" in text/html*) return 0 ;; esac ;;
  esac
  return 1
}

echo "================================================================"
echo "线上站点验收（双判：content-type + body sha256）"
echo "  BASE=$BASE   MIRROR=$MIRROR"
echo "  目标版本 VER=$VER   上一版本 PREV=$PREV"
echo "  时间：$(date '+%Y-%m-%d %H:%M:%S %Z')"
echo "================================================================"

# ---------------------------------------------------------------- 0) 首页指纹
echo
echo "[0] 首页兜底指纹（判定「软 404」的基准）"
fetch "$BASE/" "$TMP/home" || true
HOME_SHA=$SHA
HOME_BYTES=$BYTES
if [ "$CODE" != "200" ]; then
  problem "首页不是 200（其 sha 不能作为兜底指纹基准）"
else
  ok "首页 = $HOME_BYTES bytes，sha256=$HOME_SHA"
fi

# ---------------------------------------------------------------- 1) 兜底是否在生效
echo
echo "[1] 软 404 兜底是否正在生效（拿一个**确实不存在**的随机路径去问）"
PROBE="/__verify-live-site-probe-$RANDOM-$RANDOM.html"
fetch "$BASE$PROBE" "$TMP/probe" || true
FALLBACK_ACTIVE=0
if [ "$CODE" = "200" ] && ct_matches "$CT" html; then
  FALLBACK_ACTIVE=1
  HTML_FALLBACK_SHAS="$SHA"
  warn "软 404 兜底正在生效：不存在的路径 $PROBE 返回 HTTP 200 + text/html（body sha ${SHA:0:16}…）。"
  warn "本脚本已按「content-type + body sha」双判绕过它 —— 若你看到这条，说明**状态码在本站不可信**，"
  warn "任何用 200 判资产存在的旧脚本在这里都会给出假绿。GitHub Pages 镜像没有这个兜底（见 §6）。"
  if [ "$SHA" = "$HOME_SHA" ]; then
    echo "      注：本次兜底体 == 当前首页（sha 相同）"
  else
    echo "      注：**本次兜底体 != 当前首页**（兜底体 sha ${SHA:0:16}… vs 首页 ${HOME_SHA:0:16}…）"
    echo "          ⇒ 这就是为什么不能只比首页指纹：CDN 可能命中**陈旧的** index.html。"
  fi
else
  ok "未观察到 SPA 兜底：$PROBE → HTTP ${CODE}（content-type ${CT:-（无）}）"
fi

# ---------------------------------------------------------------- 2) 真资产必须真存在
echo
echo "[2] 产出物：必须是**真文件**（content-type 族要对），不是兜底 HTML"
# 规格：路径 | 期望族 | 说明
ASSETS="
/og-image-$VER.png|png|中文 OG 图
/og-image-en-$VER.png|png|英文 OG 图
/robots.txt|txt|robots
/sitemap.xml|xml|sitemap
/llms.txt|txt|llms.txt
/llms-full.txt|txt|llms-full.txt
"
echo "$ASSETS" | sed '/^$/d' | while IFS='|' read -r path family label; do
  [ -n "${path:-}" ] && echo "-- ${label}：$BASE${path}（期望 content-type 族：${family}）"
done
while IFS='|' read -r path family label; do
  [ -z "${path:-}" ] && continue
  echo "-- 检查 ${label}：$BASE$path"
  fetch "$BASE$path" "$TMP/asset" || true
  if [ "$CODE" = "404" ] || [ "$CODE" = "410" ]; then
    problem "${label}：HTTP $CODE —— **不存在**（产出物缺失）"
  elif [ "$CODE" = "200" ] && ct_matches "$CT" "$family"; then
    ok "${label}：真文件（$BYTES bytes，content-type ${CT}）"
  elif [ "$CODE" = "200" ] && ct_matches "$CT" html; then
    problem "${label}：**软 404** —— 200 但 content-type 是 text/html（期望 ${family}），body $BYTES bytes / sha ${SHA:0:16}…"
    case " $HTML_FALLBACK_SHAS " in
      *" $SHA "*) : ;;
      *) HTML_FALLBACK_SHAS="$HTML_FALLBACK_SHAS $SHA"
         echo "      （这是**新的**兜底指纹，且与当前首页不同 ⇒ CDN 里的陈旧 index.html）" ;;
    esac
  elif [ "$CODE" = "200" ]; then
    problem "${label}：200 但 content-type 是「${CT:-（无）}」，既不是期望的 $family 也不是兜底 HTML —— 内容不对"
  else
    problem "${label}：HTTP ${CODE}（既不是 200 也不是 404）"
  fi
done <<EOF
$(echo "$ASSETS" | sed '/^$/d')
EOF
rm -f "$TMP/asset"

# ---------------------------------------------------------------- 2b) 上一版产出物必须已消失
echo
echo "[2b] 上一版的产出物**必须已经不存在** —— 这一节同时就是**敏感性验证**："
echo "     裸状态码在 apex 上会被软 404 兜底骗成「存在(200)」，双判才是对的。"
GONE="
/og-image-$PREV.png|png|上一版中文 OG 图
/og-image-en-$PREV.png|png|上一版英文 OG 图
"
while IFS='|' read -r path family label; do
  [ -z "${path:-}" ] && continue
  echo "-- ${label}：$BASE$path （期望：**不存在**）"
  fetch "$BASE$path" "$TMP/gone" || true
  bare=$(curl -s -o /dev/null -w '%{http_code}' "$BASE$path" 2>/dev/null)
  bare_verdict="不存在"
  [ "$bare" = "200" ] && bare_verdict="存在"
  echo "     \$ curl -s -o /dev/null -w '%{http_code}' $BASE$path   →  **$bare**"
  echo "       裸状态码判据会说：「${bare_verdict}」；本脚本双判说：见下"
  if [ "$CODE" = "404" ] || [ "$CODE" = "410" ]; then
    ok "${label}：真 404（不存在）"
  elif [ "$CODE" = "200" ] && ct_matches "$CT" html; then
    problem "${label}：**软 404/不存在** —— 裸状态码 ${bare}（会被误判成「存在」），但 content-type 是 text/html（期望 ${family}），body $BYTES bytes / sha ${SHA:0:16}…"
    case " $HTML_FALLBACK_SHAS " in
      *" $SHA "*) : ;;
      *) HTML_FALLBACK_SHAS="$HTML_FALLBACK_SHAS $SHA"
         echo "      （新的兜底指纹，且 != 当前首页 ⇒ 边缘缓存里的陈旧 index.html）" ;;
    esac
  elif [ "$CODE" = "200" ]; then
    problem "${label}：200 且 content-type 是「${CT:-（无）}」—— 上一版的资产不该还在"
  else
    problem "${label}：HTTP $CODE"
  fi
done <<EOF
$(echo "$GONE" | sed '/^$/d')
EOF

# ---------------------------------------------------------------- 3) 页面 + 版本号计数
echo
echo "[3] 页面版本号计数（目标版本 / 上一版本）+ canonical/hreflang/og:url"
# 页面规格：路径 | 期望 canonical | zh-Hans | en | x-default | 标签
# 注意：**wasm 两页是另一个 hreflang pair**（指向 /wasm/ 与 /en/wasm/），不是首页那一组 ——
# 所以期望值必须按页给，不能在函数里写死首页的三向（我第一版就写错过了）。
PAGES="/|$BASE/|$BASE/|$BASE/en/|$BASE/|zh 首页
/en/|$BASE/en/|$BASE/|$BASE/en/|$BASE/|en 首页
/wasm/|$BASE/wasm/|$BASE/wasm/|$BASE/en/wasm/|$BASE/wasm/|wasm 页（自身版本是另一个序列）
/en/wasm/|$BASE/en/wasm/|$BASE/wasm/|$BASE/en/wasm/|$BASE/wasm/|en wasm 页
"
check_page() {
  local path=$1 want_canonical=$2 hl_zh=$3 hl_en=$4 hl_xd=$5 label=$6
  local url="$BASE$path" out="$TMP/page" n_ver n_prev canon ogurl
  echo "-- ${label}：$url"
  fetch "$url" "$out" || true
  if [ "$CODE" != "200" ]; then problem "${label}：HTTP $CODE"; return; fi
  if ! ct_matches "$CT" html; then problem "${label}：content-type 不是 text/html（${CT:-无}）"; return; fi
  if [ "$path" != "/" ] && [ "$SHA" = "$HOME_SHA" ]; then
    problem "${label}：**软 404** —— body 与首页逐字节相同（sha ${SHA:0:16}…），这个页面其实不存在"
    return
  fi
  n_ver=$(grep -o "$VER" "$out" 2>/dev/null | wc -l | tr -d ' ')
  n_prev=$(grep -o "$PREV" "$out" 2>/dev/null | wc -l | tr -d ' ')
  echo "     版本计数：$VER × $n_ver ；$PREV × $n_prev"
  [ "$n_ver" -gt 0 ] || problem "${label}：页面上找不到目标版本 ${VER}（计数 0）"
  [ "$n_prev" -eq 0 ] || problem "${label}：页面上仍有上一版本 $PREV × ${n_prev}（残留）"
  canon=$(grep -o '<link rel="canonical" href="[^"]*"' "$out" | head -1 | sed 's/.*href="//; s/"$//')
  if [ "$canon" = "$want_canonical" ]; then ok "canonical = $canon"; else problem "${label}：canonical = 「${canon:-（无）}」，期望 $want_canonical"; fi
  ogurl=$(grep -o '<meta property="og:url" content="[^"]*"' "$out" | head -1 | sed 's/.*content="//; s/"$//')
  if [ -z "$ogurl" ]; then
    warn "${label}：没有 og:url"
  elif [ "$ogurl" = "$want_canonical" ]; then
    ok "og:url = $ogurl"
  else
    warn "${label}：og:url = ${ogurl}（期望 ${want_canonical}）"
  fi
  local pair h u
  for pair in "zh-Hans=$hl_zh" "en=$hl_en" "x-default=$hl_xd"; do
    h=${pair%%=*}; u=${pair#*=}
    if grep -q "hreflang=\"$h\" href=\"$u\"" "$out"; then :; else problem "${label}：缺 hreflang $h → $u"; fi
  done
  echo "     hreflang 实读：$(grep -o 'hreflang="[^"]*" href="[^"]*"' "$out" | tr '\n' ' ')"
}
echo "$PAGES" | sed '/^$/d' | while IFS='|' read -r p c a b d l; do [ -n "${p:-}" ] && echo "-- ${l}：$BASE${p}（期望 hreflang: $a / $b / ${d}）"; done
while IFS='|' read -r p c a b d l; do
  [ -z "${p:-}" ] && continue
  check_page "$p" "$c" "$a" "$b" "$d" "$l"
done <<EOF
$(echo "$PAGES" | sed '/^$/d')
EOF

# ---------------------------------------------------------------- 4) www（只报不判失败）
echo
echo "[4] www.xraytun.top（**已知未生效**：CF 不支持域级 _redirects，需在 CF 控制台建 Redirect Rule）—— 本项只报不判失败"
WWWHS=$(curl -sI --max-time "$TIMEOUT" https://www.xraytun.top/ 2>/dev/null | tr -d '\r')
WWW_CODE=$(printf '%s\n' "$WWWHS" | awk 'NR==1{print $2}')
WWW_LOC=$(printf '%s\n' "$WWWHS" | grep -i '^location:' | head -1 | sed 's/^[Ll]ocation: *//')
echo "    \$ curl -sI https://www.xraytun.top/"
echo "      → HTTP ${WWW_CODE:-（无响应）} ；Location: ${WWW_LOC:-（无）}"
if [ "$WWW_CODE" = "301" ] || [ "$WWW_CODE" = "308" ]; then
  if [ "$WWW_LOC" = "$BASE/" ] || [ "$WWW_LOC" = "$BASE" ]; then
    ok "www 已 301 → ${WWW_LOC}（这条**曾经未生效**，现在已经好了）"
  else
    problem "www 会跳转，但 Location = 「${WWW_LOC}」，期望 $BASE/"
  fi
else
  echo "      状态：未跳转（既有已知状态，**不是本次发布的缺陷**）"
fi

# ---------------------------------------------------------------- 5) releases/latest
echo
echo "[5] GitHub releases/latest 最终跳向哪个 tag"
LATEST=$(curl -s -L -o /dev/null --max-time "$TIMEOUT" -w '%{url_effective} %{http_code}' "$RELEASES/latest" 2>/dev/null)
echo "    \$ curl -s -L -o /dev/null -w '%{url_effective} %{http_code}' $RELEASES/latest"
echo "      → ${LATEST:-（无输出）}"
LATEST_CODE=${LATEST##* }
case "$LATEST" in
  *"tag/v$VER"*) ok "latest 指向 v$VER" ;;
  "")
    warn "curl 对 releases/latest 没有任何输出（**网络/DNS 问题，不是「latest 指向错」**）—— 这一项本次没结论" ;;
  *" 000")
    warn "curl 连不上 releases/latest（HTTP 000）—— **网络/DNS 问题，不是「latest 指向错」**，这一项本次没结论" ;;
  *)
    if [ "$ALLOW_STALE_LATEST" = "1" ]; then
      warn "latest 还没指向 v${VER}（ALLOW_STALE_LATEST=1，已降级为 WARNING）"
    else
      problem "latest 没有指向 v${VER}（若这是发布前演练，用 ALLOW_STALE_LATEST=1 降级为 WARNING）"
    fi ;;
esac
: "$LATEST_CODE"

# ---------------------------------------------------------------- 6) 镜像对照（可选）
echo
echo "[6] GitHub Pages 镜像（没有 SPA 兜底）—— 用来证明「兜底只发生在 apex」"
if [ "$SKIP_MIRROR" = "1" ]; then
  echo "    （SKIP_MIRROR=1，跳过）"
else
  mk_code=$(curl -s -o /dev/null --max-time "$TIMEOUT" -w '%{http_code}' "$MIRROR/" 2>/dev/null)
  mk_probe=$(curl -s -o /dev/null --max-time "$TIMEOUT" -w '%{http_code}' "$MIRROR$PROBE" 2>/dev/null)
  mk_old=$(curl -s -o /dev/null --max-time "$TIMEOUT" -w '%{http_code}' "$MIRROR/og-image-$PREV.png" 2>/dev/null)
  echo "    \$ curl -so /dev/null -w '%{http_code}' $MIRROR/                                  → $mk_code"
  echo "    \$ curl -so /dev/null -w '%{http_code}' $MIRROR$PROBE                → $mk_probe"
  echo "    \$ curl -so /dev/null -w '%{http_code}' $MIRROR/og-image-$PREV.png   → $mk_old"
  if [ "$mk_probe" = "404" ]; then
    ok "镜像对不存在路径是真 404（与 apex 的 200 形成对照）"
    if [ "$FALLBACK_ACTIVE" = "1" ]; then
      echo "      ⇒ 同一个路径：apex=200（兜底），镜像=404。**这就是「状态码判据在 apex 上不可信」的直接证据。**"
    fi
  else
    warn "镜像对不存在路径返回 HTTP ${mk_probe:-（无响应）}（不是 404）—— 镜像行为变了，值得看一眼"
  fi
fi

# ---------------------------------------------------------------- 总结
echo
echo "================================================================"
echo "见过的 HTML 兜底指纹（都判为「不存在」）：${HTML_FALLBACK_SHAS:-（无）}"
echo "首页指纹：$HOME_SHA"
if [ "$PROBLEMS" -eq 0 ]; then
  echo "总结：全部通过（${WARNINGS} 条 warning）"
  echo "================================================================"
  exit 0
fi
echo "总结：发现 $PROBLEMS 处问题（另有 ${WARNINGS} 条 warning）"
echo "================================================================"
exit 1
