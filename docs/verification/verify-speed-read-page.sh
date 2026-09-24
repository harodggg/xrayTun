#!/usr/bin/env bash
# verify-speed-read-page.sh —— 「一目十行 · SpeedRead」相关项目页的本地验收（只读，不写仓库）
#
# 为什么存在：
#   部署工作流的静态检查只在 push 之后才跑，等发现「下载按钮指向空气 / 页面里的图 404」时
#   代价已经付掉了。而相关项目页的引用面又比一般页面大（下载件、许可证、截图、对方语言页、
#   上级的 favicon / manifest / site.css），任何一个相对路径写错都只在真浏览器里才暴露。
#   这个脚本把那一轮检查搬到本地，一条命令跑完。
#
# 判据（与 CF Pages 部署工作流一致，外加两条它做不到的）：
#   1) 必填产物存在；没有根绝对路径（GH Pages 镜像会 404）；/speed-read/ 在 sitemap/llms* 里；
#      site/ 里没有 >25 MiB 的单个文件；YAML 可解析；
#   2) 起一个真实 HTTP server，把两个页面的**每一个相对引用**逐条 GET 一遍（状态 + content-type + 字节数）；
#   3) 把用户真正下载到的那个 zip 解开，检查顶层目录、manifest 是 MV3、manifest 引用的每个文件都在包里、
#      不含 tests/ 与 tools/。
#
# 用法：
#   bash docs/verification/verify-speed-read-page.sh
#   PORT=8318 bash docs/verification/verify-speed-read-page.sh
#
# 退出码：0 = 全通过；1 = 有未通过项。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
SITE="$ROOT/site"
PORT="${PORT:-8317}"

fail=0
pass=0
step() { echo; echo "== $* =="; }
ok() { pass=$((pass + 1)); echo "  ✓ $*"; }
bad() { fail=$((fail + 1)); echo "  ✗ $*" >&2; }

# ============================ 1) 部署工作流的静态检查 ============================
step "部署工作流的静态检查（本地复刻）"

for f in index.html en/index.html llms.txt llms-full.txt robots.txt sitemap.xml \
         assets/site.css assets/site.js; do
  if [ -e "site/$f" ]; then ok "site/$f"; else bad "缺 site/$f"; fi
done

if grep -rInE '(href|src)="/([^/]|$)' site --include='*.html' >/tmp/vsr-abs.txt 2>/dev/null; then
  bad "发现根绝对路径（GH Pages 镜像上会 404）：$(tr '\n' ' ' </tmp/vsr-abs.txt)"
else
  ok "没有根绝对路径"
fi

# /speed-read/ 必须留在发现入口里 —— 并且不能把已有的两个外部项目挤掉
for f in sitemap.xml llms.txt llms-full.txt; do
  for needle in 'speed-read/' 'beauty-meter/' 'jev-x-filter/'; do
    if grep -q "$needle" "site/$f"; then ok "site/$f 收录 $needle"
    else bad "site/$f 缺少 $needle 收录"; fi
  done
done

big=$(find site -type f -size +25500k -print)
if [ -n "$big" ]; then bad "site/ 里有超过 25 MiB 的单文件（Cloudflare Pages 会拒绝）：$big"
else ok "site/ 没有超过 25 MiB 的单文件"; fi

for f in beauty-meter/index.html en/beauty-meter/index.html \
         beauty-meter/beauty-meter-extension-1.0.0.zip beauty-meter/LICENSE \
         speed-read/index.html en/speed-read/index.html \
         speed-read/speed-read-extension-0.1.0.zip speed-read/LICENSE; do
  if [ -e "site/$f" ]; then ok "site/$f"; else bad "缺 site/$f"; fi
done

if python3 -c "import yaml; yaml.safe_load(open('.github/workflows/cloudflare-pages.yml'))" 2>/dev/null; then
  ok "cloudflare-pages.yml 可被 YAML 解析"
else
  bad "cloudflare-pages.yml 无法解析"
fi

# ============================ 2) zip 内容 ============================
step "下载包内容（用户真正拿到的那份）"

SITE_ROOT="$SITE" python3 - <<'PY'
import json, os, sys, zipfile
site = os.environ["SITE_ROOT"]
fail = []
def ok(m): print(f"  ✓ {m}")
def bad(m):
    print(f"  ✗ {m}", file=sys.stderr); fail.append(m)

zp = os.path.join(site, "speed-read/speed-read-extension-0.1.0.zip")
size = os.path.getsize(zp)
ok(f"zip {size:,} 字节") if size else bad("zip 为空")
with zipfile.ZipFile(zp) as z:
    names = z.namelist()
    tops = {n.split("/")[0] for n in names}
    ok(f"顶层目录 {tops}") if tops == {"speed-read-extension"} else bad(f"顶层目录异常 {tops}")
    m = json.loads(z.read("speed-read-extension/manifest.json"))
    ok(f"manifest MV3 v{m.get('version')}") if m.get("manifest_version") == 3 else bad("manifest 不是 MV3")
    refd = [m["background"]["service_worker"], m["action"]["default_popup"], m["options_ui"]["page"],
            *m["content_scripts"][0]["js"], *m["icons"].values()]
    missing = [p for p in refd if f"speed-read-extension/{p}" not in names]
    ok("manifest 引用的每个文件都在包里") if not missing else bad(f"包里缺 {missing}")
    ok("含未压缩源码与许可证") if {"speed-read-extension/src/common/speedread-core.js",
                                  "speed-read-extension/LICENSE",
                                  "speed-read-extension/README.md"} <= set(names) else bad("缺源码/许可证")
    ok("不含 tests/ 与 tools/") if not any(n.startswith(("speed-read-extension/tests/", "speed-read-extension/tools/")) for n in names) else bad("分发包里混进了 tests/ 或 tools/")
sys.exit(1 if fail else 0)
PY
if [ $? -ne 0 ]; then fail=$((fail + 1)); else pass=$((pass + 1)); fi

# ============================ 3) 真实 HTTP 逐条拉取 ============================
step "真实 HTTP：两个页面 + 页面内全部相对引用"

SITE_ROOT="$SITE" PORT="$PORT" python3 - <<'PY'
import http.server, os, re, socketserver, sys, threading, urllib.request
site, port = os.environ["SITE_ROOT"], int(os.environ["PORT"])
base = f"http://127.0.0.1:{port}"
fail = []
def ok(m): print(f"  ✓ {m}")
def bad(m):
    print(f"  ✗ {m}", file=sys.stderr); fail.append(m)

class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw): super().__init__(*a, directory=site, **kw)
    def log_message(self, *a): pass
    def guess_type(self, path):
        return "application/zip" if path.endswith(".zip") else super().guess_type(path)

socketserver.TCPServer.allow_reuse_address = True
httpd = socketserver.TCPServer(("127.0.0.1", port), H)
threading.Thread(target=httpd.serve_forever, daemon=True).start()

def fetch(path):
    try:
        with urllib.request.urlopen(base + path, timeout=10) as r:
            return r.status, r.headers.get("content-type", ""), r.read()
    except Exception as e:  # noqa: BLE001
        return 0, "", str(e).encode()

zip_bytes = os.path.getsize(os.path.join(site, "speed-read/speed-read-extension-0.1.0.zip"))

for page, marker in (("/speed-read/", "一目十行"), ("/en/speed-read/", "SpeedRead")):
    status, ctype, body = fetch(page)
    text = body.decode("utf-8", "replace")
    if status == 200 and "text/html" in ctype: ok(f"GET {page} → 200 text/html")
    else: bad(f"GET {page} → {status} {ctype}")
    if marker in text: ok(f"{page} 正文含专属标题（不是首页兜底）")
    else: bad(f"{page} 正文里找不到页面专属标题")
    refs = sorted({m for m in re.findall(r'(?:href|src)="([^"#]+)"', text)
                   if not m.startswith(("http", "mailto:", "data:"))})
    for ref in refs:
        resolved = os.path.normpath(os.path.join(page, ref))
        s, c, b = fetch(resolved)
        good = s == 200
        if ref.endswith(".zip"): good = good and "zip" in c and len(b) == zip_bytes
        if ref.endswith(".png"): good = good and "image/png" in c
        if good: ok(f"{page} → {ref}（{s} {c} {len(b):,}B）")
        else: bad(f"{page} → {ref} 解析为 {resolved} 得到 {s} {c} {len(b)}B")

for path, needle in (("/", 'href="speed-read/"'), ("/en/", 'href="speed-read/"'),
                     ("/llms.txt", "https://xraytun.top/speed-read/"),
                     ("/llms-full.txt", "https://xraytun.top/speed-read/")):
    s, c, b = fetch(path)
    if s == 200 and needle in b.decode("utf-8", "replace"): ok(f"{path} 含 speed-read 入口")
    else: bad(f"{path} 缺 speed-read 入口（{s}）")

s, c, b = fetch("/sitemap.xml")
if b.decode("utf-8", "replace").count("xraytun.top/speed-read/") >= 2: ok("sitemap.xml 收录两条 speed-read")
else: bad("sitemap.xml 对 speed-read 的收录不足两条")

httpd.shutdown()
sys.exit(1 if fail else 0)
PY
if [ $? -ne 0 ]; then fail=$((fail + 1)); else pass=$((pass + 1)); fi

echo
if [ "$fail" -ne 0 ]; then
  echo "✗ 未通过（$fail 组失败 / 共 $((pass + fail)) 组）" >&2
  exit 1
fi
echo "✓ 全部通过"
