#!/usr/bin/env bash
#
# 核实「三个脚本真的在 .app 里，且和仓库里的字节一致」—— **看产物不看源码**。
#
# 为什么要有它：Tauri 的 `bundle.resources` 是**声明**；声明对了不等于产物里有。
# 这一族缺陷本项目吃过三次（本地绿 ≠ 运行时绿）。所以这里断言的是**解包出来的 .app**。
#
# 口径：
#   * 产物来源 = **发布资产**（release 的 zip），不是本地 `tauri build` 的中间产物 ——
#     用户拿到的就是前者，证据强度更高；
#   * 解包用 `ditto -x -k`（保签名；`unzip` 在 macOS 上可能丢扩展属性）；
#   * 每个脚本比对 `shasum -a 256`，**不比对大小**（大小相同内容可以不同）；
#   * `codesign --verify --strict` 必须 exit 0（新增 resource 会改变被签名内容）；
#   * **反向断言**：`--self-test` 用假 bundle 证明「内容不符 / 缺文件 / 路径写错」都会报红。
#
# 用法：
#   verify-app-bundle-resources.sh <XrayTun.app 路径> [--no-codesign]
#   verify-app-bundle-resources.sh --self-test
#
set -uo pipefail

# 仓库根：本脚本可能被放在 scripts/ 或 docs/verification/ 下 —— 两种都试，最后退到本机路径。
_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT=""
for cand in "$_here/.." "$_here/../.." "/Users/xbtg-/deepseek-harness/xray-tun"; do
  if [ -f "$cand/scripts/incident-bundle.sh" ] && [ -f "$cand/apps/desktop/tauri.conf.json" ]; then
    ROOT="$(cd "$cand" && pwd)"; break
  fi
done
[ -n "$ROOT" ] || { echo "找不到仓库根（本脚本期望放在 <repo>/scripts/ 或 <repo>/docs/verification/ 下）" >&2; exit 2; }

# 期望进包的脚本：仓库路径 → 包内相对 Contents/Resources 的路径
REPO_FILES=(
  "scripts/incident-bundle.sh"
  "scripts/triage-incident.py"
  "scripts/net-metrics.py"
)
INNER_DIR="scripts"

pass=0
fail=0
ok()   { pass=$((pass + 1)); echo "  ✓ $*"; }
bad()  { fail=$((fail + 1)); echo "  ✗ $*" >&2; }

# ---- 单个脚本的核对（可被 --self-test 复用；返回非 0 = 不通过）----
# $1=app 根  $2=仓库相对路径  $3=包内相对路径（相对 Contents/Resources）
check_one() {
  local app="$1" rel="$2" inner="$3"
  local src="$ROOT/$rel" dst="$app/Contents/Resources/$inner"
  [ -f "$src" ] || { echo "    仓库里没有 $rel" >&2; return 1; }
  [ -f "$dst" ] || { echo "    产物里没有 $dst" >&2; return 1; }
  local a b
  a="$(shasum -a 256 "$src" | awk '{print $1}')"
  b="$(shasum -a 256 "$dst" | awk '{print $1}')"
  [ "$a" = "$b" ] || { echo "    sha256 不一致：仓库 $a / 包内 $b" >&2; return 1; }
  return 0
}

self_test() {
  echo "[--self-test] 用假 bundle 证明这条检查不是空的"
  local tmp app
  tmp="$(mktemp -d)"
  app="$tmp/Fake.app"
  mkdir -p "$app/Contents/Resources/$INNER_DIR"

  # T1：内容正确 ⇒ 必须通过
  local rc=0
  for rel in "${REPO_FILES[@]}"; do
    cp "$ROOT/$rel" "$app/Contents/Resources/$INNER_DIR/$(basename "$rel")"
  done
  check_one "$app" "${REPO_FILES[0]}" "$INNER_DIR/$(basename "${REPO_FILES[0]}")" >/dev/null 2>&1 || rc=1
  [ "$rc" = 0 ] && ok "T1 内容一致 ⇒ 通过（检查器能给出绿）" || bad "T1 内容一致却报红（检查器坏了）"

  # T2：内容不符 ⇒ 必须报红
  printf 'x' >>"$app/Contents/Resources/$INNER_DIR/$(basename "${REPO_FILES[0]}")"
  if check_one "$app" "${REPO_FILES[0]}" "$INNER_DIR/$(basename "${REPO_FILES[0]}")" >/dev/null 2>&1; then
    bad "T2 内容不符却通过（假绿）"
  else
    ok "T2 内容不符 ⇒ 报红"
  fi
  cp "$ROOT/${REPO_FILES[0]}" "$app/Contents/Resources/$INNER_DIR/$(basename "${REPO_FILES[0]}")"

  # T3：缺文件 ⇒ 必须报红（先删掉那个文件 —— T1 已经把 3 个都拷进去了，第一版漏了这句）
  rm -f "$app/Contents/Resources/$INNER_DIR/$(basename "${REPO_FILES[1]}")"
  if check_one "$app" "${REPO_FILES[1]}" "$INNER_DIR/$(basename "${REPO_FILES[1]}")" >/dev/null 2>&1; then
    bad "T3 产物里缺文件却通过（假绿）"
  else
    ok "T3 产物里缺文件 ⇒ 报红"
  fi

  # T4：**路径写错**（就是本次要防的那个）⇒ 必须报红
  if check_one "$app" "${REPO_FILES[2]}" "$INNER_DIR/net-metrics-NOPE.py" >/dev/null 2>&1; then
    bad "T4 路径写错却通过（假绿）"
  else
    ok "T4 路径写错 ⇒ 报红"
  fi

  rm -rf "$tmp"
  echo "  --self-test：pass=$pass fail=$fail"
  [ "$fail" = 0 ] || return 1
}

main() {
  local app="" nocodesign=0
  for a in "$@"; do
    case "$a" in
      --self-test) self_test; return $? ;;
      --no-codesign) nocodesign=1 ;;
      -*) echo "未知参数：${a}" >&2; return 2 ;;
      *) app="$a" ;;
    esac
  done

  [ -n "$app" ] || { echo "用法：$0 <XrayTun.app> [--no-codesign]  |  $0 --self-test" >&2; return 2; }
  [ -d "$app" ] || { echo "没有这个 .app：$app" >&2; return 2; }

  echo "产物：$app"
  echo "仓库：$ROOT"

  # ① 声明侧：tauri.conf.json 里 scripts/* 的映射必须**恰好**是这三个
  local declared
  declared="$(grep -oE '"\.\./\.\./scripts/[A-Za-z0-9._-]+"[[:space:]]*:[[:space:]]*"scripts/[A-Za-z0-9._-]+"' \
    "$ROOT/apps/desktop/tauri.conf.json" | sed 's/.*"scripts\///; s/"$//' | sort)"
  local expected
  expected="$(printf '%s\n' "${REPO_FILES[@]}" | sed 's#.*/##' | sort)"
  if [ "$declared" = "$expected" ]; then
    ok "tauri.conf.json 声明的 scripts/* 恰好是 3 个：$(echo "$declared" | tr '\n' ' ')"
  else
    bad "声明与预期不符 —— 声明=[$(echo "$declared" | tr '\n' ' ')] 预期=[$(echo "$expected" | tr '\n' ' ')]"
  fi

  # ② 产物侧：逐个比 sha256
  for rel in "${REPO_FILES[@]}"; do
    local base inner
    base="$(basename "$rel")"
    inner="$INNER_DIR/$base"
    if check_one "$app" "$rel" "$inner"; then
      ok "$inner == ${rel}（sha256 $(shasum -a 256 "$ROOT/$rel" | cut -c1-16)…）"
    else
      bad "$inner 与 $rel 不一致"
    fi
  done

  # ③ 产物里实际有些什么（路径不符时让人一眼看到落在哪）
  echo "  实际 Contents/Resources/scripts/ 内容："
  ls -l "$app/Contents/Resources/scripts/" 2>/dev/null | sed 's/^/    /' || echo "    （该目录不存在）"

  # ④ 签名
  if [ "$nocodesign" = 1 ]; then
    echo "  （--no-codesign：跳过 codesign 核对）"
  else
    if codesign --verify --strict --verbose=2 "$app" 2>/tmp/xt-codesign.log; then
      ok "codesign --verify --strict 通过"
    else
      bad "codesign --verify --strict 失败（详见 /tmp/xt-codesign.log）"
      sed 's/^/    /' /tmp/xt-codesign.log >&2
    fi
  fi

  echo "pass=$pass fail=$fail"
  [ "$fail" = 0 ]
}

main "$@"
