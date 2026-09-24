#!/usr/bin/env bash
# 校验 `scripts/gen-site-geo.py` 的「外部条目不许被抹掉」机制（task-180）。
#
# 五个案子，全部在**隔离副本**里跑（`mktemp -d` 里放一份 site/ + scripts/ 的拷贝）——
# 绝不在真树跑生成器：真树里可能正有另一条工作流的未提交内容。
#
#   [1] 改前对照（旧行为）：`GEN_SITE_EXTERNAL_OFF=1` ⇒ 外部条目**被抹掉**（红，复现事故）
#   [2] 改后：跑全部生成器 ⇒ 外部条目**逐行不变**（Jev + beauty-meter 两个项目一起验）
#   [3] 幂等：连跑两次 ⇒ 产物零 diff
#   [4] 大声失败：把外部内容塞进**生成区块内**（不在标记区间）⇒ 非零退出 + 指名到行
#   [5] 通用性：判据里没有项目名字面量（只按「是不是本产品」判定）
#
# 用法：bash docs/verification/verify-gen-site-external.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PASS=0
FAIL=0
ok()  { printf '  ✓ %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  ✗ %s\n' "$1"; FAIL=$((FAIL + 1)); }

FILES=(sitemap.xml llms.txt llms-full.txt)
# 外部项目的站点路径 —— **只用来测量**，不参与脚本里的判据。
EXTERNAL_RE='jev-x-filter/|beauty-meter/'

fresh() { # 建一份隔离副本，回显路径
  local d
  d="$(mktemp -d)"
  cp -a "$ROOT/scripts" "$d/scripts"
  cp -a "$ROOT/site" "$d/site"
  find "$d/site" -name '*.before' -delete 2>/dev/null || true
  printf '%s\n' "$d"
}
snapshot() { # $1=副本目录
  local d="$1" f
  for f in "${FILES[@]}"; do
    cp "$d/site/$f" "$d/site/$f.before"
  done
}
external_lines() { grep -E "$EXTERNAL_RE" "$1" || true; }

echo "== [1] 改前对照：旧行为会抹掉外部条目（测试缝 GEN_SITE_EXTERNAL_OFF=1）=="
D1="$(fresh)"; snapshot "$D1"
if GEN_SITE_EXTERNAL_OFF=1 python3 "$D1/scripts/gen-site-geo.py" >"$D1/out.txt" 2>&1; then
  ok "旧行为照常退出 0（不报错）"
else
  bad "旧行为退出非 0 —— 它应当'静默抹掉'（rc=$(echo $? )）"
fi
for f in "${FILES[@]}"; do
  n="$(diff <(external_lines "$D1/site/$f.before") <(external_lines "$D1/site/$f") | grep -c '^[<>]' || true)"
  if [ "$n" -gt 0 ]; then ok "${f}：外部条目被抹掉 ${n} 行（复现事故，符合预期）"
  else bad "${f}：旧行为竟然没抹掉（这个案子失去意义）"; fi
done

echo
echo "== [2] 改后：跑全部生成器 ⇒ 外部条目逐行不变 =="
D2="$(fresh)"; snapshot "$D2"
jrc=0
python3 "$D2/scripts/gen-site-jsonld.py" gen >"$D2/jsonld-gen.txt" 2>&1 || jrc=$?
python3 "$D2/scripts/gen-site-geo.py"      >"$D2/geo.txt" 2>&1 || jrc=$((jrc + 100))
printf '    jsonld gen rc=%s ；geo rc=%s\n' "$((jrc % 100))" "$((jrc / 100))"
for f in "${FILES[@]}"; do
  if diff <(external_lines "$D2/site/$f.before") <(external_lines "$D2/site/$f") >"$D2/d.txt"; then
    ok "${f}：外部条目与运行前逐行一致（Jev + beauty-meter）"
  else
    bad "${f}：外部条目变了："; sed 's/^/      /' "$D2/d.txt" | head -6
  fi
done

echo
echo "== [3] 幂等：连跑两次零 diff =="
for f in "${FILES[@]}"; do cp "$D2/site/$f" "$D2/site/$f.run1"; done
python3 "$D2/scripts/gen-site-geo.py" >"$D2/geo2.txt" 2>&1 || bad "第二次 geo 退出非 0"
python3 "$D2/scripts/gen-site-jsonld.py" gen >"$D2/jsonld-gen2.txt" 2>&1 || bad "第二次 jsonld gen 退出非 0"
same=1
for f in "${FILES[@]}"; do diff -q "$D2/site/$f.run1" "$D2/site/$f" >/dev/null || same=0; done
diff -q "$D2/site/index.html" "$D2/site/index.html" >/dev/null || true
[ "$same" = 1 ] && ok "连跑两次：三份产物零 diff" || bad "连跑两次有 diff"

echo
echo "== [4] 大声失败：生成区块内的外部内容 ⇒ 非零 + 指名到行 =="
D4="$(fresh)"
printf '\n- 更多：见 https://xraytun.top/beauty-meter/ 的介绍页\n' >>"$D4/site/llms-full.txt"
if python3 "$D4/scripts/gen-site-geo.py" >"$D4/out.txt" 2>&1; then
  bad "竟然退出 0（外部内容会被静默丢掉）"
else
  ok "退出非 0（拒绝静默删除）"
fi
if grep -q 'llms-full.txt:[0-9]*' "$D4/out.txt"; then
  ok "报错指名到文件与行：$(grep -o 'site/llms-full.txt:[0-9]*' "$D4/out.txt" | head -1)"
else
  bad "报错没指名到行：$(head -2 "$D4/out.txt" | tr '\n' ' ')"
fi
D4b="$(fresh)"
printf '\n## 相关项目：粉底仪 · Beauty Meter\n\n一个完全不同的外部项目：https://xraytun.top/beauty-meter/\n' >>"$D4b/site/llms-full.txt"
if python3 "$D4b/scripts/gen-site-geo.py" >"$D4b/out.txt" 2>&1; then
  bad "整节外部内容竟然退出 0"
else
  ok "整节外部内容也大声失败：$(grep -o 'site/llms-full.txt:[0-9]*' "$D4b/out.txt" | head -1)"
fi

echo
echo "== [5] 通用性：判据里没有项目名字面量 =="
lit="$(grep -nE 'jev|beauty' "$ROOT/scripts/gen-site-geo.py" | grep -vE ':[[:space:]]*#' || true)"
if [ -z "$lit" ]; then ok "逻辑里 0 处项目名字面量（只按归属判定）"; else bad "逻辑里有字面量：$lit"; fi
for p in 'jev-x-filter' 'beauty-meter'; do
  n1="$(grep -c "$p" "$D2/site/sitemap.xml" || true)"
  n2="$(grep -c "$p" "$D2/site/llms.txt" || true)"
  [ "${n1:-0}" -ge 1 ] && [ "${n2:-0}" -ge 1 ] && ok "夹具 ${p}：sitemap=${n1} llms.txt=${n2}（都在）" || bad "夹具 ${p} 缺失（sitemap=${n1} llms.txt=${n2}）"
done

echo
echo "== [6] 版本 bump 不被机制误拦（模拟提交 1：VERSION 0.8.38 + PUBLISHED=False）=="
D6="$(fresh)"; snapshot "$D6"
python3 - "$D6" <<'PY'
import re, sys
from pathlib import Path
d = Path(sys.argv[1])
g = d / "scripts/gen-site-geo.py"
s = g.read_text(encoding="utf-8")
s = s.replace('VERSION = "0.8.37"', 'VERSION = "0.8.38"')
s = s.replace("PUBLISHED = True", "PUBLISHED = False", 1)
s = re.sub(r'DMG_BYTES, DMG_MIB = "[^"]*", "[^"]*"', 'DMG_BYTES, DMG_MIB = "", ""', s)
s = re.sub(r'ZIP_BYTES, ZIP_MIB = "[^"]*", "[^"]*"', 'ZIP_BYTES, ZIP_MIB = "", ""', s)
s = re.sub(r'SHA_BYTES = "[^"]*"', 'SHA_BYTES = ""', s)
g.write_text(s, encoding="utf-8")
PY
if python3 "$D6/scripts/gen-site-geo.py" >"$D6/out.txt" 2>&1; then
  ok "版本 bump 时生成器正常退出 0（机制没有误拦）"
else
  bad "版本 bump 被机制拦下（会挡住发版）：$(head -3 "$D6/out.txt" | tr '\n' ' ')"
fi
if grep -q '0\.8\.38' "$D6/site/llms.txt"; then ok "产物确实换了版本号（0.8.38 已写入）"; else bad "产物没换版本号（案子无效）"; fi
for f in "${FILES[@]}"; do
  if diff <(external_lines "$D6/site/$f.before") <(external_lines "$D6/site/$f") >"$D6/d.txt"; then
    ok "${f}：版本 bump 后外部条目仍逐行不变"
  else
    bad "${f}：版本 bump 后外部条目变了："; sed 's/^/      /' "$D6/d.txt" | head -4
  fi
done

echo
printf '== 汇总：pass=%d fail=%d ==\n' "$PASS" "$FAIL"
rm -rf "$D1" "$D2" "$D4" "$D4b" "$D6"
[ "$FAIL" = 0 ]
