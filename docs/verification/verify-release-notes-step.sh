#!/usr/bin/env bash
#
# 「Release 正文里的版本特有用户动作」这条机制的**本地复刻**。
#
# 为什么需要它：`release.yml` 里的那一步只有在**推 tag** 时才跑（CI、要 GitHub 令牌），
# 本地没法真跑。而这一步恰恰是从「发布后手工 `gh release edit` 补一句」（v0.8.35）变成
# 机制的地方 —— 机制不能只靠「看一眼 YAML 觉得对」。
#
# 做法：
#   1. 从 `.github/workflows/release.yml` **抽出**那一步的 `run:` 真身（用 PyYAML，
#      不是手抄），并断言它确实走 `scripts/bump-release.py notes`（改掉就会在这里红）；
#   2. 在一个临时目录里铺好仓库的 `scripts/` + `docs/release-notes/` + 假 `dist/`；
#   3. 用**假 `gh`**（记录调用、不发网络请求）真跑那一段，三种情形各验一次：
#      A 缺动作文件 ⇒ 必须失败，而且**在调用任何 `gh` 之前**就失败（不许先建 Release）；
#      B 有动作文件 ⇒ 成功，NOTES.md 里动作段在最前、固定模板在后；
#      C 显式「本版无需用户额外动作」⇒ 也允许（但必须显式写出来）。
#
# 用法：docs/verification/verify-release-notes-step.sh
#
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT=""
for cand in "$HERE/../.." "$HERE/.." "/Users/xbtg-/deepseek-harness/xray-tun"; do
  if [ -f "$cand/.github/workflows/release.yml" ]; then ROOT="$(cd "$cand" && pwd)"; break; fi
done
[ -n "$ROOT" ] || { echo "找不到仓库根" >&2; exit 2; }

TMP="$(mktemp -d)"
pass=0
fail=0
ok() { pass=$((pass + 1)); echo "  ✓ $*"; }
bad() { fail=$((fail + 1)); echo "  ✗ $*" >&2; }
trap 'rm -rf "$TMP"' EXIT

echo "仓库：$ROOT"
echo "临时目录：$TMP"

# ---- 1. 抽出 release.yml 里那一步的 run 真身 ----
python3 - "$ROOT" "$TMP/step.sh" <<'PY'
import sys, pathlib, yaml
root, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
d = yaml.safe_load((root / ".github/workflows/release.yml").read_text(encoding="utf-8"))
steps = d["jobs"]["release"]["steps"]
body = [s for s in steps if s.get("name") == "发布到 GitHub Release"][0]["run"]
assert "bump-release.py notes" in body, "release.yml 那一步没有走 scripts/bump-release.py notes"
out.write_text(body, encoding="utf-8")
print(f"  抽出 {len(body.encode())} 字节的 run 真身（含 `bump-release.py notes` ✓）")
PY
[ $? -eq 0 ] || { echo "✗ 抽取失败（PyYAML 缺失？）" >&2; exit 2; }
ok "从 release.yml 抽出该步真身（不是手抄）"

# ---- 2. 假 gh：记录调用；不发任何网络请求 ----
mkdir -p "$TMP/bin" "$TMP/work/dist"
cat >"$TMP/bin/gh" <<'SH'
#!/usr/bin/env bash
echo "gh $*" >>"$GH_LOG"
# 第一次 `gh release view "$TAG"`（不带 --json）要**失败**，好走到 create 分支
if [ "$1" = "release" ] && [ "$2" = "view" ] && [ "$3" = "$TAG" ]; then
  case "$*" in
    *--json\ assets*) printf '%s\n' "XrayTun_${TAG#v}_x86_64_arm64.dmg" "XrayTun_${TAG#v}_x86_64_arm64.zip" "SHA256SUMS.txt"; exit 0 ;;
    *--json\ isDraft*) echo "已发布"; exit 0 ;;
    *) exit 1 ;;
  esac
fi
exit 0
SH
chmod +x "$TMP/bin/gh"

prepare() { # $1 = work dir
  rm -rf "$1"
  mkdir -p "$1/dist" "$1/docs/release-notes" "$1/scripts"
  cp "$ROOT/scripts/bump-release.py" "$1/scripts/"
  cp "$ROOT"/docs/release-notes/*.md "$1/docs/release-notes/" 2>/dev/null || true
  : >"$1/dist/XrayTun_0.8.36_x86_64_arm64.dmg"
  : >"$1/dist/XrayTun_0.8.36_x86_64_arm64.zip"
  : >"$1/dist/SHA256SUMS.txt"
}

run_step() { # $1 = work dir  $2 = tag
  ( cd "$1" && GH_LOG="$1/gh.log" TAG="$2" PATH="$TMP/bin:$PATH" bash "$TMP/step.sh" )
}

# ---- A. 缺动作文件 ⇒ 必须失败，且不许先动 gh ----
W="$TMP/work-A"
prepare "$W"
if run_step "$W" v0.8.36 >"$TMP/a.out" 2>"$TMP/a.err"; then
  bad "A：缺 docs/release-notes/v0.8.36.md 却成功了（静默退化）"
else
  if grep -q "缺少版本特有的用户动作文件" "$TMP/a.err"; then
    ok "A：缺动作文件 ⇒ 非 0 退出，且报错点名了期望路径"
  else
    bad "A：失败了但报错不是「缺少版本特有的用户动作文件」：$(head -1 "$TMP/a.err")"
  fi
  if [ ! -s "$W/gh.log" ]; then
    ok "A：**在调用任何 gh 之前**就失败（不会先建 Release 再报错）"
  else
    bad "A：已经调用过 gh：$(head -2 "$W/gh.log")"
  fi
fi

# ---- B. 有动作文件 ⇒ 成功，动作段在最前 ----
W="$TMP/work-B"
prepare "$W"
cat >"$W/docs/release-notes/v0.8.36.md" <<'MD'
> ## ⚠️ 本版需要重新安装特权助手
>
> 示例：helper 侧改了，去「设置 → 系统与助手」重新安装。
MD
if run_step "$W" v0.8.36 >"$TMP/b.out" 2>&1; then
  if head -3 "$W/NOTES.md" | grep -q "重新安装特权助手" && grep -q "^## 安装" "$W/NOTES.md"; then
    ok "B：动作段在最前、固定模板「## 安装」在后（NOTES.md $(wc -c <"$W/NOTES.md" | tr -d ' ') 字节）"
  else
    bad "B：NOTES.md 结构不对"
  fi
  if grep -q -- "--notes-file NOTES.md" "$W/gh.log"; then
    ok "B：gh release create 用的就是 NOTES.md（$(grep -c . "$W/gh.log" | tr -d ' ') 次 gh 调用）"
  else
    bad "B：create 没有用 NOTES.md"
  fi
else
  bad "B：有动作文件却失败：$(tail -2 "$TMP/b.out")"
fi

# ---- C. 显式「本版无需用户额外动作」⇒ 允许 ----
W="$TMP/work-C"
prepare "$W"
echo "本版无需用户额外动作" >"$W/docs/release-notes/v0.8.36.md"
if run_step "$W" v0.8.36 >"$TMP/c.out" 2>&1; then
  if grep -q "本版无需用户额外动作" "$W/NOTES.md"; then
    ok "C：显式声明「本版无需用户额外动作」也放行（但必须写出来）"
  else
    bad "C：通过了但 NOTES.md 里没有那句声明"
  fi
else
  bad "C：显式声明却失败：$(tail -2 "$TMP/c.out")"
fi

# ---- D. 空文件 / 只有空白 ⇒ 必须失败 ----
W="$TMP/work-D"
prepare "$W"
printf '   \n\n' >"$W/docs/release-notes/v0.8.36.md"
if run_step "$W" v0.8.36 >"$TMP/d.out" 2>"$TMP/d.err"; then
  bad "D：空白动作文件却成功了"
else
  ok "D：空白动作文件 ⇒ 非 0 退出（不许当「没动作」）"
fi

echo
echo "pass=$pass fail=$fail"
[ "$fail" = 0 ]
