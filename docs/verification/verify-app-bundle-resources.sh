#!/usr/bin/env bash
#
# 核实「三个脚本真的在 .app 里，且与**某个发布提交**里的字节一致」—— **看产物，基准取 git blob**。
#
# ## 为什么基准必须是 git blob，不能是工作树
#
# 第一版拿**当前工作树**当基准。v0.8.37 发布后核实时就翻车了：tag `ef768fa` 之后工作树又被
# `96df259`（task-171）改过那两个脚本 ⇒ 自检报 **2 红**，而按 **tag 里的文件**比三个脚本**完全相同**。
# 这类假红的代价：把「工作树在前进」误判成「包里带的是旧脚本」，**逼人对一个不存在的缺陷做决定**。
# ⇒ 现在比较对象一律是 `git show <rev>:<path>` 的 **blob**，与工作树无关；工作树只作**信息性**打印。
#
# ## 发布后核实的正确用法（**照这条做**）
#
#     docs/verification/verify-app-bundle-resources.sh /tmp/XrayTun.app --rev v0.8.37
#
# 不传 `--rev` 时的默认：先读 `.app` 的 `CFBundleShortVersionString`，若存在同名 tag `v<版本>`
# 就用它（绝大多数情况就是产出这个包的那个发布提交）；**没有**同名 tag 才退到 `HEAD`，并**显式警告**。
# 无论走哪条路，报告都打印 **基准来源 / rev / commit / 两侧 sha256**，不静默。
#
# ## 用法
#
#     verify-app-bundle-resources.sh <XrayTun.app> [--rev <ref>] [--repo <dir>] [--no-codesign]
#     verify-app-bundle-resources.sh --self-test
#
# 退出码：0 = 全部一致；1 = 有不一致；2 = 用法/环境问题（rev 取不到、不在 git 仓库里等）。
# `--self-test` 用**临时夹具仓库**跑，**不碰共享工作树**。
#
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO=""
for cand in "$HERE/.." "$HERE/../.."; do
  if [ -f "$cand/scripts/incident-bundle.sh" ] && [ -f "$cand/apps/desktop/tauri.conf.json" ]; then
    REPO="$(cd "$cand" && pwd)"; break
  fi
done

# 期望进包的脚本（仓库相对路径）
REPO_FILES=(
  "scripts/incident-bundle.sh"
  "scripts/triage-incident.py"
  "scripts/net-metrics.py"
)
INNER_DIR="scripts"

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "  ✓ $*"; }
bad() { fail=$((fail + 1)); echo "  ✗ $*" >&2; }
warn() { echo "  ⚠️  $*"; }

usage() {
  cat >&2 <<'USAGE'
用法：
  verify-app-bundle-resources.sh <XrayTun.app> [--rev <ref>] [--repo <dir>] [--no-codesign]
  verify-app-bundle-resources.sh --self-test

  发布后核实：**显式传 --rev <tag>**（例如 --rev v0.8.37）。
  不传 --rev 时：优先用与 .app 版本同名的 tag，没有才退到 HEAD 并**警告**。
  基准一律取该 rev 的 git blob（`git show <rev>:<path>`），**与工作树无关**。
USAGE
}

app_version() { # $1 = .app 根
  local plist="$1/Contents/Info.plist"
  [ -f "$plist" ] || return 1
  if command -v plutil >/dev/null 2>&1; then
    plutil -extract CFBundleShortVersionString raw -o - "$plist" 2>/dev/null && return 0
  fi
  if [ -x /usr/libexec/PlistBuddy ]; then
    /usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist" 2>/dev/null && return 0
  fi
  return 1
}

file_sha() { [ -f "$1" ] && shasum -a 256 "$1" | awk '{print $1}'; }
short() { printf '%s' "$1" | cut -c1-16; }

# 单个文件的核对（可被 --self-test 复用）。返回 0 = 一致；非 0 = 不一致/取不到
# $1 = repo  $2 = app根  $3 = rev  $4 = 仓库相对路径  $5 = 包内相对路径
check_one() {
  local repo="$1" app="$2" rev="$3" rel="$4" inner="$5"
  local dst="$app/Contents/Resources/$inner"
  [ -f "$dst" ] || { echo "    产物里没有 $dst" >&2; return 1; }
  if ! git -C "$repo" cat-file -e "${rev}:${rel}" 2>/dev/null; then
    echo "    rev ${rev} 里取不到 ${rel} 的 blob" >&2
    return 1
  fi
  local a b
  a="$(git -C "$repo" show "${rev}:${rel}" | shasum -a 256 | awk '{print $1}')"
  b="$(file_sha "$dst")"
  [ "$a" = "$b" ] || { echo "    sha256 不一致：rev=$a / 包内=$b" >&2; return 1; }
  return 0
}

self_test() {
  echo "[--self-test] 用**临时夹具仓库**验证（不碰共享工作树）"
  local tmp; tmp="$(mktemp -d)"
  local fx="$tmp/fixture"
  mkdir -p "$fx/scripts" "$fx/apps/desktop"
  cat >"$fx/scripts/incident-bundle.sh" <<'SH'
#!/usr/bin/env bash
echo fixture-bundle
SH
  cat >"$fx/scripts/triage-incident.py" <<'PY'
print("fixture-triage")
PY
  cat >"$fx/scripts/net-metrics.py" <<'PY'
print("fixture-metrics")
PY
  cat >"$fx/apps/desktop/tauri.conf.json" <<'JSON'
{
  "bundle": {
    "resources": {
      "../../scripts/incident-bundle.sh": "scripts/incident-bundle.sh",
      "../../scripts/triage-incident.py": "scripts/triage-incident.py",
      "../../scripts/net-metrics.py": "scripts/net-metrics.py"
    }
  }
}
JSON
  git -C "$fx" init -q
  git -C "$fx" add -A
  git -C "$fx" -c user.email=t@t -c user.name=t commit -qm fixture

  # 假 bundle：内容取自 **HEAD 的 blob**（与工作树无关）
  local app="$tmp/Fake.app"
  mkdir -p "$app/Contents/Resources/$INNER_DIR"
  local f base
  for f in "${REPO_FILES[@]}"; do
    base="$(basename "$f")"
    git -C "$fx" show "HEAD:$f" >"$app/Contents/Resources/$INNER_DIR/$base"
  done

  # T1 绿：与 rev 的 blob 一致
  local rc=0
  for f in "${REPO_FILES[@]}"; do
    check_one "$fx" "$app" HEAD "$f" "$INNER_DIR/$(basename "$f")" >/dev/null 2>&1 || rc=1
  done
  [ "$rc" = 0 ] && ok "T1 包内 == rev 的 blob ⇒ 通过" || bad "T1 一致却报红（检查器坏了）"

  # T2 红（负对照）：包内与 rev 差 1 字节
  printf 'x' >>"$app/Contents/Resources/$INNER_DIR/incident-bundle.sh"
  if check_one "$fx" "$app" HEAD "scripts/incident-bundle.sh" "$INNER_DIR/incident-bundle.sh" >/dev/null 2>&1; then
    bad "T2 差 1 字节却通过（假绿）"
  else
    ok "T2 包内与 rev 差 **1 字节** ⇒ 报红"
  fi
  git -C "$fx" show "HEAD:scripts/incident-bundle.sh" >"$app/Contents/Resources/$INNER_DIR/incident-bundle.sh"

  # T3 红：缺文件
  rm -f "$app/Contents/Resources/$INNER_DIR/triage-incident.py"
  if check_one "$fx" "$app" HEAD "scripts/triage-incident.py" "$INNER_DIR/triage-incident.py" >/dev/null 2>&1; then
    bad "T3 产物里缺文件却通过（假绿）"
  else
    ok "T3 产物里缺文件 ⇒ 报红"
  fi
  git -C "$fx" show "HEAD:scripts/triage-incident.py" >"$app/Contents/Resources/$INNER_DIR/triage-incident.py"

  # T4 红：路径写错
  if check_one "$fx" "$app" HEAD "scripts/net-metrics.py" "$INNER_DIR/net-metrics-NOPE.py" >/dev/null 2>&1; then
    bad "T4 路径写错却通过（假绿）"
  else
    ok "T4 路径写错 ⇒ 报红"
  fi

  # T5 ★ 敏感性：工作树脏、基准仍是 blob ⇒ 不许假红
  printf 'DIRTY-IN-WORKTREE\n' >>"$fx/scripts/net-metrics.py"      # 只改工作树，不提交
  rc=0
  for f in "${REPO_FILES[@]}"; do
    check_one "$fx" "$app" HEAD "$f" "$INNER_DIR/$(basename "$f")" >/dev/null 2>&1 || rc=1
  done
  if [ "$rc" = 0 ]; then
    ok "T5 **工作树脏**（改过 net-metrics.py）而基准取 HEAD 的 blob ⇒ **不产生假红**"
  else
    bad "T5 工作树脏却报红（这正是 v0.8.37 那次假红的形状）"
  fi
  git -C "$fx" checkout -q -- .

  # T6 环境问题：rev 不存在 ⇒ 必须判为「取不到基准」（不是静默通过）
  if check_one "$fx" "$app" "v9.9.9-nope" "scripts/net-metrics.py" "$INNER_DIR/net-metrics.py" >/dev/null 2>&1; then
    bad "T6 不存在的 rev 却通过（假绿）"
  else
    ok "T6 不存在的 rev ⇒ 判为「取不到基准」（main 里映射成退出码 2）"
  fi

  rm -rf "$tmp"
  echo "  --self-test：pass=$pass fail=$fail"
  [ "$fail" = 0 ] || return 1
}

main() {
  local app="" rev_opt="" no_codesign=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --self-test) self_test; return $? ;;
      --rev)
        [ $# -ge 2 ] || { usage; return 2; }
        rev_opt="$2"; shift 2 ;;
      --repo)
        [ $# -ge 2 ] || { usage; return 2; }
        REPO="$2"; shift 2 ;;
      --no-codesign) no_codesign=1; shift ;;
      -h | --help) usage; return 0 ;;
      -*) echo "未知参数：$1" >&2; usage; return 2 ;;
      *) app="$1"; shift ;;
    esac
  done

  [ -n "$app" ] || { usage; return 2; }
  [ -d "$app" ] || { echo "没有这个 .app：$app" >&2; return 2; }
  [ -n "$REPO" ] || { echo "找不到仓库根（用 --repo 指定）" >&2; return 2; }
  git -C "$REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "✗ $REPO 不是 git 仓库 —— 基准必须来自 git blob，**不退回工作树**" >&2; return 2; }

  # ---- 选基准：显式 --rev > 与 .app 版本同名的 tag > HEAD（并警告）----
  local rev basis appver="" no_tag=0
  if [ -n "$rev_opt" ]; then
    rev="$rev_opt"; basis="显式 --rev ${rev}"
  else
    appver="$(app_version "$app" || true)"
    if [ -n "$appver" ] && git -C "$REPO" rev-parse -q --verify "refs/tags/v${appver}" >/dev/null 2>&1; then
      rev="v${appver}"; basis="tag v${appver}（由 .app 的 CFBundleShortVersionString 推出）"
    else
      rev="HEAD"; no_tag=1
      basis="HEAD（**没有**与 .app 版本匹配的 tag${appver:+（.app 版本 ${appver}）}）"
    fi
  fi

  local commit
  if ! commit="$(git -C "$REPO" rev-parse "${rev}^{commit}" 2>/dev/null)" || [ -z "$commit" ]; then
    echo "✗ 取不到 rev 的 commit：${rev}" >&2
    echo "  （浅克隆里没有该 rev 的对象/blob 时也会这样 —— 环境问题，不是产物问题）" >&2
    return 2
  fi

  echo "产物：$app"
  echo "仓库：$REPO"
  echo "基准来源：$basis"
  echo "基准 rev：${rev} → commit ${commit}"
  if [ "$no_tag" = 1 ]; then
    warn "**没有与 .app 版本匹配的 tag，基准退到了 HEAD** —— 发布后核实请显式传 \`--rev v<版本>\`"
    warn "（否则 HEAD 已经前进时，你会拿一个**比发布提交更新的**基准去比 —— 那正是 v0.8.37 那次假红的形状）"
  fi

  # 工作树状态：只作信息，**不参与判绿红**
  local dirty
  dirty="$(git -C "$REPO" status --porcelain -- "${REPO_FILES[@]}" 2>/dev/null)"
  if [ -n "$dirty" ]; then
    warn "工作树里这三个脚本**有未提交改动**："
    printf '%s\n' "$dirty" | sed 's/^/      /'
    warn "⇒ 基准**仍是 ${rev} 的 blob**（与工作树无关）；上面这些改动**不会**造成假红"
  else
    echo "工作树（相对该 rev）：这三个脚本没有未提交改动"
  fi

  # ---- ① 声明侧：tauri.conf.json 里 scripts/* 必须**恰好**这三个（同样取 rev 的 blob）----
  local declared expected
  declared="$(git -C "$REPO" show "${rev}:apps/desktop/tauri.conf.json" 2>/dev/null \
    | grep -oE '"\.\./\.\./scripts/[A-Za-z0-9._-]+"[[:space:]]*:[[:space:]]*"scripts/[A-Za-z0-9._-]+"' \
    | sed 's/.*"scripts\///; s/"$//' | sort)"
  expected="$(printf '%s\n' "${REPO_FILES[@]}" | sed 's#.*/##' | sort)"
  if [ "$declared" = "$expected" ]; then
    ok "tauri.conf.json（rev ${rev}）声明的 scripts/* 恰好是 3 个：$(echo "$declared" | tr '\n' ' ')"
  else
    bad "声明与预期不符 —— 声明=[$(echo "$declared" | tr '\n' ' ')] 预期=[$(echo "$expected" | tr '\n' ' ')]"
  fi

  # ---- ② 产物侧：逐个比 rev 的 blob ----
  local rel base inner rev_sha out_sha wt_sha
  for rel in "${REPO_FILES[@]}"; do
    base="$(basename "$rel")"
    inner="$INNER_DIR/$base"
    if [ -f "$app/Contents/Resources/$inner" ]; then
      out_sha="$(file_sha "$app/Contents/Resources/$inner")"
    else
      out_sha="（产物里没有）"
    fi
    if git -C "$REPO" cat-file -e "${rev}:${rel}" 2>/dev/null; then
      rev_sha="$(git -C "$REPO" show "${rev}:${rel}" | shasum -a 256 | awk '{print $1}')"
    else
      rev_sha="（rev 里没有这个文件）"
    fi
    if [ -f "$REPO/$rel" ]; then wt_sha="$(file_sha "$REPO/$rel")"; else wt_sha="（工作树没有）"; fi

    echo "  · $inner  rev=$(short "$rev_sha")  包内=$(short "$out_sha")"
    if [ "$rev_sha" = "$out_sha" ]; then
      ok "$inner == rev 的 blob（sha256 $(short "$rev_sha")…）"
    else
      bad "$inner 与 rev 的 blob 不一致（rev=$(short "$rev_sha")… 包内=$(short "$out_sha")…）"
    fi
    if [ "$wt_sha" != "$rev_sha" ]; then
      warn "    （信息）工作树那份与基准不同：工作树=$(short "$wt_sha")… —— 基准取的是 rev，**不计入判红**"
    fi
  done

  # ---- ③ 产物里实际有什么 ----
  echo "  实际 Contents/Resources/scripts/ 内容："
  ls -l "$app/Contents/Resources/scripts/" 2>/dev/null | sed 's/^/    /' || echo "    （该目录不存在）"

  # ---- ④ 签名 ----
  if [ "$no_codesign" = 1 ]; then
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
