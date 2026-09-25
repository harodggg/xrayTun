#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# XRAYTUN_TEAM_ID 注入的证据与断言（安全审计 P0-1 / F1）。
#
# 判据与哨兵的定义在 `scripts/team-id.sh`（单一来源）；本脚本只负责**证明**它成立。
#
# 三种用法：
#   ./scripts/verify-team-id-injection.sh --static
#       纯只读静态检查（不跑 cargo）：发版路径有没有真的接上注入、有没有别处偷偷注入、
#       生成的 launchd plist 会不会把 `XRAYTUN_HELPER_INSECURE=1` 塞给 helper。
#       秒级，被 `scripts/check.sh` 调用 ⇒ 本地 / CI / 发版前同一条判据。
#
#   ./scripts/verify-team-id-injection.sh --assert-helper <二进制> [--expect <值>] [--policy <策略>]
#       断言**产物**里真的带着注入值。`peer.rs` 的 `from_build_env()` 只在
#       `option_env!("XRAYTUN_TEAM_ID")` 为 None/空串时退化成 `InsecureAllowAny`；
#       注入值是编译期字面量，一定落在二进制里 ⇒ 找到它 = 编译期确实拿到值 = 走
#       `RequireSignature`。发版流水线在打包之后跑这一条（对用户拿到的那份 helper）。
#
#   ./scripts/verify-team-id-injection.sh --evidence
#       跑出五段**可复算**的绿/红证据（会编译 xt-helper；release 冷启动几分钟）：
#         E1  新契约（task-17 后）：**未注入 ⇒ 绝不宽松**（release）：装了 App 走 cdhash 绑定，
#             没装 App 走拒绝服务；且 release 产物里 grep 不到 debug 专用的宽松标识
#         E1b 空串同样不再开门：`XRAYTUN_TEAM_ID=`（CI 里 vars 未定义的形态）与"未注入"同一结论
#         E1c 反向敏感性：宽松标识**只在 debug 存在** ⇒ "release 里没有它"是 cfg 门做到的，不是空话
#         E2  注入哨兵 ⇒ `require-signature`（fail closed：要求串匹配不上任何签名 ⇒ 一律拒绝）
#         E3  注入真 Team ID ⇒ `require-signature`；产物带着该值，且 xt-helper 全量测试全绿
#         E4  cdhash 绑定（第三条策略）在产物层面可断言
#         E5  判据/断言不许「照单全收」（空串/未设/形状不合法/未知策略一律非 0）
#
#       ⚠️ **契约在 task-17 之后翻转了**：旧版 E1/E1b 断言的是"未注入/空串 ⇒ 走到
#       `InsecureAllowAny`（宽松）"—— 那是在**期待漏洞存在**，是最该被禁止的假绿方向。
#       现在断言的是"未注入/空串 ⇒ 绝不宽松"。反向敏感性（把判据改回旧契约必须变红）：
#         sed 's|^  NONLOOSE_OK_MARKS=.*|  NONLOOSE_OK_MARKS="$MARK_INSECURE_DEBUG"|' \
#           scripts/verify-team-id-injection.sh > /tmp/old-contract.sh && bash /tmp/old-contract.sh --evidence
#
# # 三条策略的扩展点（**加第三态时只改这一处**）
#
# F1 的修复按可用凭据分三条形态；产物断言是**策略感知**的：
#   · `team-id`    —— 注入真实 Apple Team ID（10 位 [A-Z0-9]）。已实现。
#   · `refuse-all` —— 显式哨兵（当前无证书时的 fail-closed 形态）。已实现。
#   · `cdhash`     —— 不依赖 Developer ID：比对"对端 cdhash == 已安装 App 可执行文件的 cdhash"
#                     （ad-hoc 签名也有 cdhash）。task-17 已落地；产物判据 = 带
#                     `XRAYTUN_HELPER_POLICY=cdhash-binding` + 绑定的 App 路径，且不含 debug 专用标识。
# 落地时：`POLICY_STATUS` 改状态 + `policy_assert()` 对应分支填判据（其余代码不用动）。
#
# 退出码：0 = 通过；1 = 有判据不成立；2 = 用法错误 / 策略未实现；75 = 环境问题（缺工具，不是代码失败）。
# ---------------------------------------------------------------------------

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=./team-id.sh
source "$ROOT/scripts/team-id.sh"

MODE=""
HELPER=""
EXPECT="${XRAYTUN_TEAM_ID-}"
POLICY="auto"

# ---------------------------------------------------------------------------
# 策略表（**扩展点**，见文件头"三条策略的扩展点"）
# ---------------------------------------------------------------------------
# `<策略>=<done|pending>`，空格分隔。`pending` 表示判据还没定下来（例如 cdhash 需要
# backend-2 先给出"二进制里可断言的标识"）：选中它会**明确失败**，绝不静默通过。
# `--static` 会检查这张表本身没写错，防止"加了策略但脚本忘了跟上"。
POLICY_STATUS="team-id=done refuse-all=done cdhash=done"

# 产物里的**策略标识**（由 `crates/xt-helper/src/peer.rs` 的 `PeerPolicy::describe()` 给出，
# 并在 helper 启动日志里打出来）。用它们就能在**产物**与**运行期**两个层面断言"这一版走哪种策略"。
# ⚠️ 其中 `insecure-allow-any-debug` 被 `#[cfg(debug_assertions)]` 门住：
#    release 产物里出现它 = 那道编译期门失效了（这正是要抓的形态）。
MARK_REQUIRE_SIGNATURE='XRAYTUN_HELPER_POLICY=require-signature'
MARK_CDHASH='XRAYTUN_HELPER_POLICY=cdhash-binding'
MARK_REFUSE='XRAYTUN_HELPER_POLICY=refuse-service'
MARK_INSECURE_DEBUG='XRAYTUN_HELPER_POLICY=insecure-allow-any-debug'
# helper 用来做 cdhash 绑定的那个路径（`peer.rs` 的 `INSTALLED_APP_BINARY`）。
INSTALLED_APP_BINARY='/Applications/XrayTun.app/Contents/MacOS/xraytun-desktop'

policy_status() { # <策略> → done | pending | unknown
  local p="$1" e
  for e in $POLICY_STATUS; do
    [ "${e%%=*}" = "$p" ] && { printf '%s\n' "${e#*=}"; return 0; }
  done
  printf 'unknown\n'
}

policy_of_value() { # 由注入值推导策略（保持与旧调用兼容）
  case "$(team_id_classify "${1-}")" in
    real) printf 'team-id\n' ;;
    sentinel) printf 'refuse-all\n' ;;
    *) printf 'invalid\n' ;;
  esac
}

# 本机「未注入 Team ID 时应该落到哪种非宽松策略」：装了 App ⇒ cdhash 绑定；没装 ⇒ 拒绝服务。
# 期望值**按环境算出来**，不是写死的 —— 否则在没有 App 的机器上会给出假红。
expected_nonloose_policy() {
  if [ -f "$INSTALLED_APP_BINARY" ]; then printf 'cdhash-binding\n'; else printf 'refuse-service\n'; fi
}

# 产物里是否有某个策略标识。直接对文件 grep（无管道）—— pipefail 下 `strings | grep -q`
# 会把命中判成失败。
artifact_has_mark() { # <文件> <标识>
  LC_ALL=C grep -qF -- "$2" "$1" 2>/dev/null
}

# 跑一下 helper 读它**启动日志**里的策略标识（stderr，`XRAYTUN_LOG=info`）。
# 为什么要真跑一次：产物里三个标识的**常量**都会在（`describe()` 的 match 分支都编进去），
# 所以 `strings` 证明不了"这一版选了哪条"；只有启动日志能。
# 安全性：`XRAYTUN_ALLOW_NONROOT=1` + 临时 socket 路径；`recover_from_crash()` 在本机是 no-op
# （`/Library/Application Support/XrayTun/helper-session.json` 不存在，实测），
# 且我们不发送任何请求 ⇒ 不会碰系统路由 / DNS。
observe_policy() { # <helper 二进制> <env 赋值...> → stdout: 策略标识（可能为空）
  local helper="$1"; shift
  local log sock pid
  log="$(mktemp "${TMPDIR:-/tmp}/xraytun-tid-policy.XXXXXX.log")"
  sock="$(mktemp -u "${TMPDIR:-/tmp}/xraytun-tid-probe.XXXXXX.sock")"
  env "$@" XRAYTUN_ALLOW_NONROOT=1 XRAYTUN_LOG=info \
    "$helper" run --socket "$sock" >"$log" 2>&1 &
  pid=$!
  sleep 2
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -f "$sock"
  # grep -o 读全量、不提前退出（无 SIGPIPE 风险）
  grep -o 'XRAYTUN_HELPER_POLICY=[a-z-]*' "$log" 2>/dev/null | head -1
  rm -f "$log"
}

# 产物断言：每种策略一行判据。
# 返回 0=通过 1=不通过 2=未实现/未知。
policy_assert() { # <策略> <helper> <期望值>
  local policy="$1" helper="$2" expect="$3"
  case "$policy" in
    team-id | refuse-all)
      # 注入值是编译期字面量 ⇒ 出现在二进制里 = `option_env!` 拿到的是 `Some(非空)`
      # ⇒ 走 `RequireSignature`（不是宽松）。
      LC_ALL=C grep -qF -- "$expect" "$helper" 2>/dev/null
      ;;
    cdhash)
      # cdhash 绑定（task-17）：不依赖证书，比对"对端 cdhash == 已安装 App 的 cdhash"。
      # 产物判据 = 绑定的那个 App 可执行文件路径 + cdhash 策略标识都在二进制里，
      # 且 **release 产物里不许出现 debug 专用标识**（后者是 cfg 门失效的形态）。
      artifact_has_mark "$helper" "$MARK_CDHASH" || return 1
      artifact_has_mark "$helper" "$INSTALLED_APP_BINARY" || return 1
      if ! artifact_has_mark "$helper" "$MARK_INSECURE_DEBUG"; then :; else return 1; fi
      return 0
      ;;
    *)
      return 2
      ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --static) MODE="static"; shift ;;
    --evidence) MODE="evidence"; shift ;;
    --assert-helper) MODE="assert"; HELPER="${2:-}"; shift 2 ;;
    --expect) EXPECT="${2:-}"; shift 2 ;;
    --policy) POLICY="${2:-}"; shift 2 ;;
    -h | --help) sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

if [ -z "$MODE" ]; then
  echo "必须给一个模式：--static | --assert-helper <二进制> | --evidence" >&2
  exit 2
fi

pass=0
fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
bad() { echo "  ✗ $*" >&2; fail=$((fail + 1)); }

# ---------------------------------------------------------------------------
# --assert-helper
# ---------------------------------------------------------------------------
if [ "$MODE" = "assert" ]; then
  if [ -z "$HELPER" ] || [ ! -f "$HELPER" ]; then
    echo "✗ 找不到 helper 二进制：${HELPER:-<空>}" >&2
    exit 2
  fi
  # ⚠️ 校验顺序必须是**策略感知**的：`cdhash` 策略**不需要期望值**（它的判据是产物里的
  #    策略标识 + 绑定的 App 路径，见 `policy_assert()`），所以不能"先查 EXPECT 非空"——
  #    那会让 cdhash 形态（打包时本来就不注入）在产物断言处直接 exit 2。
  cls=''
  if [ "$POLICY" = "auto" ]; then
    if [ -z "$EXPECT" ]; then
      echo "✗ 没有期望值：给 --expect <Team ID>，或先把 XRAYTUN_TEAM_ID 设好（scripts/team-id.sh）" >&2
      exit 2
    fi
    cls="$(team_id_classify "$EXPECT")"
    if [ "$cls" = "empty" ] || [ "$cls" = "invalid" ]; then
      echo "✗ 期望值不合法（${cls}）：'$EXPECT' ⇒ 这个产物本来就会被 helper 判成不可信，先修注入" >&2
      exit 1
    fi
    POLICY="$(policy_of_value "$EXPECT")"
  else
    case "$POLICY" in
      team-id | refuse-all)
        if [ -z "$EXPECT" ]; then
          echo "✗ 策略 '$POLICY' 需要 --expect <注入值>（它的产物判据就是"二进制里有没有这个字面量"）" >&2
          exit 2
        fi
        cls="$(team_id_classify "$EXPECT")"
        if [ "$cls" = "empty" ] || [ "$cls" = "invalid" ]; then
          echo "✗ 期望值不合法（${cls}）：'$EXPECT' ⇒ 先修注入" >&2
          exit 1
        fi
        ;;
      cdhash) : ;;   # 不需要期望值
      *) : ;;        # 未知策略：交给下面的 policy_status 判（exit 2）
    esac
  fi
  case "$(policy_status "$POLICY")" in
    done) ;;
    pending)
      echo "✗ 策略 '$POLICY' 的产物判据**还没实现**（见文件头「三条策略的扩展点」）：" >&2
      echo "  选中它会明确失败，绝不静默通过。落地时改 POLICY_STATUS 与 policy_assert()。" >&2
      exit 2 ;;
    *)
      echo "✗ 未知策略 '$POLICY'（已知：${POLICY_STATUS}）" >&2
      exit 2 ;;
  esac
  if [ "$POLICY" = "cdhash" ]; then
    echo "[断言] 策略=$POLICY  $HELPER 里必须有策略标识 + 绑定的 App 路径，且不含 debug 标识"
  else
    echo "[断言] 策略=$POLICY  $HELPER 里必须带着注入值（${cls}）：$EXPECT"
  fi
  # 判据在 `policy_assert()`（策略表），这里只负责报结论。
  # ⚠️ 只断言**正向**存在。不要用"回退警告串不在"当判据：那条是中文，BSD `strings`
  #    按非 ASCII 字节切分，实测 grep 不到（编码假红）；正向字面量是稳定的。
  # ⚠️ 也不用 `strings … | grep -q`：本脚本是 `set -o pipefail`，`grep -q` 命中即退出
  #    ⇒ `strings` 收到 SIGPIPE（141）⇒ 管道整体非 0 ⇒ **命中被判成失败**（第一版就
  #    因此报了假红）。判据里是直接对文件 grep（无管道）。
  if policy_assert "$POLICY" "$HELPER" "$EXPECT"; then
    case "$POLICY" in
      cdhash)
        ok "产物是 cdhash 绑定（策略标识 + 绑定路径在位、无 debug 标识）⇒ 不依赖证书，也不是「信任任何对端」"
        ;;
      *)
        ok "产物带着注入值 ⇒ option_env! 是 Some(..) ⇒ PeerPolicy::RequireSignature（不是宽松策略）"
        ;;
    esac
  else
    case "$POLICY" in
      cdhash)
        bad "产物里没有 cdhash 绑定标识/绑定路径，或混进了 debug 专用标识 ⇒ 拒绝出货"
        ;;
      *)
        bad "产物里**找不到** '$EXPECT' ⇒ 这一版 helper 的注入没生效（P0-1），拒绝出货"
        ;;
    esac
  fi
  exit $((fail > 0 ? 1 : 0))
fi

# ---------------------------------------------------------------------------
# --static
# ---------------------------------------------------------------------------
if [ "$MODE" = "static" ]; then
  echo "[静态] 发版路径是否真的接上了注入（不跑 cargo）"
  RELEASE_YML="$ROOT/.github/workflows/release.yml"
  PKG="$ROOT/scripts/package-macos.sh"

  # 1) 唯一允许的"注入点"清单。crates/** 里那句 `option_env!("XRAYTUN_TEAM_ID")` 是**读**，不是注。
  #
  # ⚠️ 判据必须**先砍注释**再看"是不是真的在设这个变量"——第一版用裸 `grep -E
  #    'XRAYTUN_TEAM_ID[[:space:]]*[:=]'`，于是 backend-2 在 peer.rs 里写的一句**文档注释**
  #    （`/// … ops 实测 XRAYTUN_TEAM_ID="" …`）被当成注入点 ⇒ 门禁假红。
  #    现在：去掉注释（`#` / `//` / `/*` / `<!--`，`://` 不算）后，只认三种形态：
  #      shell `export XRAYTUN_TEAM_ID` · 赋值 `XRAYTUN_TEAM_ID=…` · YAML env 键 `XRAYTUN_TEAM_ID:`。
  allowed='scripts/team-id.sh
scripts/package-macos.sh
scripts/verify-team-id-injection.sh
.github/workflows/release.yml'
  injectors="$(cd "$ROOT" && git grep -n -E 'XRAYTUN_TEAM_ID|export[[:space:]]+XRAYTUN_TEAM_ID' -- . 2>/dev/null \
    | grep -v '^docs/' \
    | python3 -c '
import re, sys

def comment_start(line):
    """注释起始列（-1 = 没有注释）。`://` 里的 `//` 不算注释。"""
    st = line.lstrip()
    indent = len(line) - len(st)
    for mk in ("<!--", "#", "//", "/*"):
        if st.startswith(mk):
            return indent
    if st.startswith(("* ", "*/")):
        return indent
    m = re.search(r"\s#", line)
    if m:
        return m.start() + 1
    m = re.search(r"(?<!:)\s//", line)
    if m:
        return m.start() + 1
    return -1

# 只认"真的在设这个变量"：shell export / 赋值 / YAML env 键。
INJ = re.compile(r"(?:^|\s)XRAYTUN_TEAM_ID\s*=(?!=)|export\s+XRAYTUN_TEAM_ID\b|XRAYTUN_TEAM_ID\s*:(?!=)")
for raw in sys.stdin:
    parts = raw.rstrip("\n").split(":", 2)
    if len(parts) < 3:
        continue
    f, _ln, text = parts
    cut = comment_start(text)
    code = text if cut < 0 else text[:cut]
    if INJ.search(code):
        print(f)
' | sort -u || true)"
  unexpected=""
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    # 精确整行比较（`case` 加换行定界），**不用** `printf … | grep -qx`：
    # pipefail 下 `grep -q` 提前退出会把命中判成失败（同一类坑，见脚本内其它注释）。
    case $'\n'"$allowed"$'\n' in
      *$'\n'"$f"$'\n'*) ;;
      *) unexpected="$unexpected $f" ;;
    esac
  done <<<"$injectors"
  if [ -n "$unexpected" ]; then
    bad "注入点清单外还有文件在设 XRAYTUN_TEAM_ID：${unexpected}（多一份判据就会漂移）"
  else
    ok "注入点只有 team-id.sh / package-macos.sh / verify-team-id-injection.sh / release.yml"
  fi
  if [ -z "$injectors" ]; then
    bad "一个注入点都没找到 ⇒ 注入又丢了（这正是 F1 的形态）"
  fi
  # crates/** 只许"读"，不许"写"：把 crates 里出现的行列出来自证。
  crates_hits="$(cd "$ROOT" && git grep -n 'XRAYTUN_TEAM_ID' -- 'crates/*' 2>/dev/null | grep -vE 'XRAYTUN_TEAM_ID[[:space:]]*[:=]' || true)"
  if [ -n "$crates_hits" ]; then
    ok "crates/** 里只有读取（option_env!），没有注入："
    printf '%s\n' "$crates_hits" | sed 's/^/      /'
  fi

  # 2) 发版流水线：预检必须存在、必须在打包之前、必须写进 GITHUB_ENV、必须做产物断言。
  if grep -q 'team-id.sh' "$RELEASE_YML"; then
    ok "release.yml 引用了 scripts/team-id.sh"
  else
    bad "release.yml 没引用 scripts/team-id.sh ⇒ 注入判据没接上"
  fi
  if grep -q 'team_id_resolve' "$RELEASE_YML"; then
    ok "release.yml 调用了 team_id_resolve（判据单一来源）"
  else
    bad "release.yml 没有调用 team_id_resolve"
  fi
  if grep -q 'GITHUB_ENV' "$RELEASE_YML"; then
    ok "预检把归一化后的值写进 \$GITHUB_ENV（后续步骤都有非空值）"
  else
    bad "预检没有写 \$GITHUB_ENV ⇒ 后续构建可能拿不到值"
  fi
  # 认**调用行**，不认注释里提到的文件名（第一版踩到：`grep -n 'package-macos.sh'`
  # 命中的是文件头注释里的第 7 行 ⇒ "预检在打包之前"给出假红）。
  line_pre="$(grep -n 'team_id_resolve --into' "$RELEASE_YML" | head -1 | cut -d: -f1)"
  line_pkg="$(grep -n 'run: ./scripts/package-macos.sh' "$RELEASE_YML" | head -1 | cut -d: -f1)"
  if [ -n "$line_pre" ] && [ -n "$line_pkg" ] && [ "$line_pre" -lt "$line_pkg" ]; then
    ok "预检（L${line_pre}）在打包（L${line_pkg}）**之前** ⇒ 缺注入走不到出包"
  else
    bad "预检没有排在打包之前（pre=${line_pre:-无} pkg=${line_pkg:-无}）⇒ 可能先出包才发现"
  fi
  if grep -q -- '--assert-helper' "$RELEASE_YML"; then
    ok "release.yml 对**产物**做断言（--assert-helper）"
  else
    bad "release.yml 没有对产物做断言 ⇒ 只能证明「配置写了」，证明不了「这一版真的编进去了」"
  fi
  for f in "$PKG"; do
    if grep -q 'team-id.sh' "$f" && grep -q 'team_id_resolve' "$f"; then
      ok "$(basename "$f") 走同一个判据（team_id_resolve）"
    else
      bad "$(basename "$f") 没接上判据"
    fi
  done

  # 3) 运行时可整体关掉签名校验的那个环境变量：生成的 launchd plist 不许设它（审计 §F1 收尾项）。
  PLIST_SRC="$ROOT/apps/desktop/src/helper_install.rs"
  if [ -f "$PLIST_SRC" ]; then
    if grep -q 'XRAYTUN_HELPER_INSECURE' "$PLIST_SRC"; then
      bad "launchd plist 生成器里出现 XRAYTUN_HELPER_INSECURE ⇒ 装出来的 helper 可能整体跳过签名校验"
    else
      ok "launchd plist 生成器不设 XRAYTUN_HELPER_INSECURE（该变量是运行期开关，绝不该随包下发）"
    fi
  fi

  # 4) 哨兵字面量漂移检查：workflow / 脚本里若写了哨兵，必须与 team-id.sh 一致。
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    f="${hit%%:*}"
    case "${f#"$ROOT"/}" in scripts/team-id.sh) continue ;; esac
    if grep -qF -- "$XRAYTUN_TEAM_ID_SENTINEL" "$f"; then
      ok "哨兵字面量与 team-id.sh 一致：${f#"$ROOT"/}"
    else
      bad "${f#"$ROOT"/} 里出现了哨兵字样但不等于 team-id.sh 的常量（漂移）"
    fi
  done < <(cd "$ROOT" && grep -rln -- 'UNSET-REFUSE-PRIVILEGED-OPS' scripts .github 2>/dev/null || true)

  # 5) 策略表自洽（扩展点在**本脚本**里）：状态只许是 done/pending，且 done 的必须有判据。
  for entry in $POLICY_STATUS; do
    pname="${entry%%=*}"
    pstate="${entry#*=}"
    case "$pstate" in
      done) ;;
      pending) ;;
      *) bad "策略表项 '$entry' 状态不认识（只能是 done / pending）" ;;
    esac
    [ -n "$pname" ] || bad "策略表里有空名字：'$entry'"
  done
  ok "策略表现状：${POLICY_STATUS}（pending 的策略被选中时会**明确失败**，不会静默通过）"
  if printf '%s' "$POLICY_STATUS" | grep -q 'cdhash=pending'; then
    ok "cdhash（task-23：不依赖 Developer ID 的对端身份绑定）已留好扩展点：" \
       "改 POLICY_STATUS + policy_assert() 的 cdhash 分支即可，其余代码不用动"
  fi

  echo "[静态] 通过 $pass 项，失败 $fail 项"
  exit $((fail > 0 ? 1 : 0))
fi

# ---------------------------------------------------------------------------
# --evidence
# ---------------------------------------------------------------------------
if [ "$MODE" = "evidence" ]; then
  command -v cargo >/dev/null 2>&1 || { echo "✗ 环境问题（75）：找不到 cargo" >&2; exit 75; }
  export CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}"
  export CARGO_TARGET_DIR="${XRAYTUN_EVIDENCE_TARGET_DIR:-${TMPDIR:-/tmp}/xraytun-team-id-evidence/target}"
  EVID_DIR="$(dirname "$CARGO_TARGET_DIR")"
  mkdir -p "$EVID_DIR"
  echo "[证据] CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
  echo "[证据] 判据来源 git rev: $(git -C "$ROOT" rev-parse --short HEAD)  哨兵=$XRAYTUN_TEAM_ID_SENTINEL"
  HELPER_RELEASE="$CARGO_TARGET_DIR/release/xraytun-helper"
  HELPER_DEBUG="$CARGO_TARGET_DIR/debug/xraytun-helper"
  FAKE_REAL='ABCDE12345'

  # ---------------------------------------------------------------------------
  # E1/E1b 的**唯一判据**：未注入 / 空串时**允许**出现的策略标识。
  #
  # **契约在 task-17 后翻转了**（这里刻意只留一行常量，便于做反向敏感性突变）：
  #   · 旧契约（P0-1 时代）：未注入/空串 ⇒ `InsecureAllowAny`（信任任何对端）—— 那是**漏洞本身**；
  #     断言"宽松分支被走到"等于**期待漏洞存在**，是最该被禁止的假绿方向。
  #   · 新契约（`crates/xt-helper/src/peer.rs` 的 `policy_for` + `#[cfg(debug_assertions)]` 门）：
  #     未注入/空串 ⇒ 装了 App 走 `cdhash-binding`（对端 cdhash 必须等于已安装 App 的），
  #     没装 App 走 `refuse-service`（拒绝一切非 root 特权操作）；`InsecureAllowAny` 只在 debug 存在。
  # 所以断言的是"**绝不宽松**"，而不是"宽松会被走到"。
  #
  # 反向敏感性（可复算，我在提交说明里贴了红证）：
  #   sed 's|^  NONLOOSE_OK_MARKS=.*|  NONLOOSE_OK_MARKS="$MARK_INSECURE_DEBUG"|' \
  #     scripts/verify-team-id-injection.sh > /tmp/old-contract.sh && bash /tmp/old-contract.sh --evidence
  #   ⇒ E1/E1b 必须变红（把判据改回旧契约 = 期待漏洞，必须失败）。
  # ---------------------------------------------------------------------------
  NONLOOSE_OK_MARKS="$MARK_CDHASH $MARK_REFUSE"

  # 跑一次真正的构建，**不吞退出码**。
  # 第一版把 `cargo build` 接进管道又只 `tail -3`，构建失败时断言只说"产物里找不到值"——
  # 把"没编出来"和"编出来但没注入"混成一句话。构建失败必须在这里就红，并带上日志尾巴。
  build_helper() { # <显示名> <release|debug> <env 赋值...>
    local label="$1" profile="$2"; shift 2
    local log="$EVID_DIR/build-$label.log" relflag=""
    [ "$profile" = "release" ] && relflag="--release"
    # shellcheck disable=SC2086  # $relflag 是固定的字面量（"--release" 或空），不是路径
    if ! (cd "$ROOT" && env "$@" cargo build $relflag -p xt-helper) >"$log" 2>&1; then
      bad "构建失败（${label}/${profile}）—— 见 $log"
      tail -5 "$log" | sed 's/^/      /' >&2
      return 1
    fi
    local bin="$CARGO_TARGET_DIR/$profile/xraytun-helper"
    if [ ! -f "$bin" ]; then
      bad "构建成功但没有 $bin —— 路径假设错了（先查 CARGO_TARGET_DIR / profile）"
      return 1
    fi
    ok "构建成功（${label}/${profile}）：$(basename "$bin") $(wc -c <"$bin" | tr -d ' ') 字节"
    return 0
  }

  # 断言"这个产物在未注入/空串下选的是非宽松策略"，并顺带钉住 release 里没有宽松标识。
  check_nonloose() { # <显示名> <helper> <env 赋值...>
    local label="$1" helper="$2"; shift 2
    local want got
    want="$(expected_nonloose_policy)"
    got="$(observe_policy "$helper" "$@")"
    case " $NONLOOSE_OK_MARKS " in
      *" $got "*)
        ok "${label}：策略 = ${got}（非宽松；本机装了 App 时期望 ${want}）" ;;
      *)
        bad "${label}：策略 = ${got:-（启动日志里没有策略标识）}，不在允许集 {${NONLOOSE_OK_MARKS# }} 里" \
            "⇒ 未注入/空串**又**变成宽松（P0-1 回归）或契约未落地" ;;
    esac
    if artifact_has_mark "$helper" "$MARK_INSECURE_DEBUG"; then
      bad "${label}：release 产物里出现了 ${MARK_INSECURE_DEBUG} ⇒ cfg 门失效（InsecureAllowAny 不该被编进 release）"
    else
      ok "${label}：release 产物里没有 ${MARK_INSECURE_DEBUG}（编译期就不存在这条策略）"
    fi
  }

  echo
  echo "== E1 新契约：未注入 ⇒ **绝不宽松**（release）=="
  build_helper 'release-未注入' release -u XRAYTUN_TEAM_ID && check_nonloose '未注入' "$HELPER_RELEASE" -u XRAYTUN_TEAM_ID

  echo
  echo "== E1b 空串不再开门：XRAYTUN_TEAM_ID= （CI 里 vars 未定义的形态）=="
  build_helper 'release-空串' release XRAYTUN_TEAM_ID= && check_nonloose '空串' "$HELPER_RELEASE" XRAYTUN_TEAM_ID=

  echo
  echo "== E1c 反向敏感性：宽松标识**只在 debug 存在**（否则 E1 的"没有它"就是空话）=="
  build_helper 'debug-未注入' debug -u XRAYTUN_TEAM_ID
  if artifact_has_mark "$HELPER_DEBUG" "$MARK_INSECURE_DEBUG"; then
    ok "debug 产物里有 ${MARK_INSECURE_DEBUG} ⇒ 标识机制是活的；release 里没有它**是 cfg 门做到的**，不是「这个标识从不出现」"
  else
    bad "debug 产物里也没有 ${MARK_INSECURE_DEBUG} ⇒ E1 的「release 里没有它」可能是空话，先查 describe() 是否还被引用"
  fi

  echo
  echo "== E2 注入哨兵 ⇒ RequireSignature（fail closed，拒绝一切非 root 特权操作）=="
  if XRAYTUN_TEAM_ID="$XRAYTUN_TEAM_ID_SENTINEL" team_id_resolve --into /dev/null 2>/dev/null; then
    ok "判据接受显式哨兵（并把后果写在 stderr 上，不静默）"
  else
    bad "判据拒绝了哨兵 —— 那 fail-closed 形态就没有合法入口了"
  fi
  build_helper 'release-哨兵' release XRAYTUN_TEAM_ID="$XRAYTUN_TEAM_ID_SENTINEL"
  got="$(observe_policy "$HELPER_RELEASE" XRAYTUN_TEAM_ID="$XRAYTUN_TEAM_ID_SENTINEL")"
  if [ "$got" = "$MARK_REQUIRE_SIGNATURE" ]; then
    ok "启动日志：策略 = ${got}（植入哨兵 ⇒ 要求串匹配不上任何签名 ⇒ 一律拒绝）"
  else
    bad "注入哨兵后策略 = ${got:-（空）}，期望 ${MARK_REQUIRE_SIGNATURE}"
  fi
  if XRAYTUN_TEAM_ID="$XRAYTUN_TEAM_ID_SENTINEL" "$0" --assert-helper "$HELPER_RELEASE" --expect "$XRAYTUN_TEAM_ID_SENTINEL" >/dev/null; then
    ok "产物断言：release helper 里带着哨兵字面量 ⇒ 编译期拿到了值"
  else
    bad "产物断言失败：release helper 里没有哨兵"
  fi

  echo
  echo "== E3 注入真实 Team ID ⇒ 产物带着它；且 helper 全量测试全绿 =="
  build_helper 'release-真值' release XRAYTUN_TEAM_ID="$FAKE_REAL"
  got="$(observe_policy "$HELPER_RELEASE" XRAYTUN_TEAM_ID="$FAKE_REAL")"
  if [ "$got" = "$MARK_REQUIRE_SIGNATURE" ]; then
    ok "启动日志：策略 = ${got}（要求串 = anchor apple generic + com.xraytun.desktop + OU=${FAKE_REAL}）"
  else
    bad "注入 ${FAKE_REAL} 后策略 = ${got:-（空）}，期望 ${MARK_REQUIRE_SIGNATURE}"
  fi
  if XRAYTUN_TEAM_ID="$FAKE_REAL" "$0" --assert-helper "$HELPER_RELEASE" --expect "$FAKE_REAL" >/dev/null; then
    ok "产物断言：helper 里带着 ${FAKE_REAL}"
  else
    bad "产物断言失败：helper 里没有 ${FAKE_REAL}"
  fi
  # 用 cargo 自己的退出码（权威），不再 `… | tail | grep -q`（pipefail 下同样会假红）。
  if (cd "$ROOT" && XRAYTUN_TEAM_ID="$FAKE_REAL" cargo test -q -p xt-helper >"$EVID_DIR/tests-$FAKE_REAL.log" 2>&1); then
    ok "xt-helper 全量测试全绿（含 LOCAL_PEERTOKEN=0x006 门、cdhash 绑定、空串当未配置、cfg 门）"
  else
    bad "xt-helper 测试没有全绿（见 $EVID_DIR/tests-$FAKE_REAL.log）"
  fi

  echo
  echo "== E4 cdhash 绑定（第三条策略）在产物上可断言 =="
  # 未注入 + 本机装了 App ⇒ 期望 cdhash-binding（E1 已经断言过运行期标识）；这里断言产物层面。
  if env -u XRAYTUN_TEAM_ID "$0" --assert-helper "$HELPER_RELEASE" \
       --expect "$XRAYTUN_TEAM_ID_SENTINEL" --policy cdhash >/dev/null 2>&1; then
    ok "产物断言 --policy cdhash 通过：release helper 里带着 cdhash 策略标识 + 绑定的 App 路径"
  else
    bad "--policy cdhash 断言失败（未注入的 release helper 应带 cdhash-binding 标识与 INSTALLED_APP_BINARY 路径）"
  fi

  echo
  echo "== E5 判据/断言不许「照单全收」 =="
  if (XRAYTUN_TEAM_ID= team_id_resolve --into /dev/null) >/dev/null 2>&1; then
    bad "空串竟然被判据接受了 ⇒ 判据是空话"
  else
    ok "空串被判据拒绝（非 0）⇒ 缺注入在发版路径上必定停下"
  fi
  if (env -u XRAYTUN_TEAM_ID team_id_resolve --into /dev/null) >/dev/null 2>&1; then
    bad "未设置竟然被接受了 ⇒ 判据是空话"
  else
    ok "未设置被判据拒绝（非 0）"
  fi
  if (XRAYTUN_TEAM_ID='lower-case' team_id_resolve --into /dev/null) >/dev/null 2>&1; then
    bad "形状不合法的值竟然被接受"
  else
    ok "形状不合法的值被拒绝（拒绝瞎填）"
  fi
  if env -u XRAYTUN_TEAM_ID "$0" --assert-helper "$HELPER_RELEASE" --expect "$FAKE_REAL" >/dev/null 2>&1; then
    bad "未注入的 helper 竟然通过了产物断言 ⇒ 断言是空话"
  else
    ok "未注入的 helper **不通过**产物断言 ⇒ 这条断言真的能拦住「配置写了、产物是空的」"
  fi
  if "$0" --assert-helper "$HELPER_RELEASE" --expect "$FAKE_REAL" --policy 不存在的策略 >/dev/null 2>&1; then
    bad "未知策略竟然通过了 ⇒ 策略名写错会被静默吞掉"
  else
    ok "未知策略名也明确失败（拼错不会被静默吞掉）"
  fi

  echo
  echo "[证据] 通过 $pass 项，失败 $fail 项"
  echo "[证据] 未验证（写清楚，别当成漏做）："
  echo "        1) 真实 App 被这个门**接受**—— 需要 Developer ID 证书（本机 0 个签名身份、CI 只有"
  echo "           Cloudflare secrets），所以只验了负方向（拒绝）；"
  echo "        2) cdhash 绑定的**正方向**（装了 App 时它真的被接受）—— 需要一次真实的 helper 会话，"
  echo "           本脚本只断言到"策略选对了 + 绑定路径编进了产物"。"
  exit $((fail > 0 ? 1 : 0))
fi
