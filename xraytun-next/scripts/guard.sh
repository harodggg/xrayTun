#!/usr/bin/env bash
# xraytun-next · 不变量机器判据
#
# 存在理由：本项目三条主张（无等待 / 无回落 / 无假数据）如果只写在文档里，
# 几周后一定会有人「临时」加一个 sleep 或一个 fallback。所以把它们变成脚本：
# 每个人提交前自己跑一遍，CI 跑同一条命令。
#
# 用法：bash scripts/guard.sh          # 全量
#       bash scripts/guard.sh --quiet  # 只打印结论
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QUIET=0
[ "${1:-}" = "--quiet" ] && QUIET=1

violations=0
warnings=0

say() { [ "$QUIET" -eq 0 ] && printf '%s\n' "$*"; return 0; }
bad() { printf 'NOT OK  %s\n' "$*"; violations=$((violations + 1)); }
warn() { printf 'WARN    %s\n' "$*"; warnings=$((warnings + 1)); }
ok() { say "ok      $*"; }

# 收集源码文件（不含 docs / target / node_modules / 生成产物）。
src_files() {
  find "$ROOT/crates" -path '*/target' -prune -o -name '*.rs' -print 2>/dev/null
}
ui_files() {
  find "$ROOT/apps/ui/src" -name '*.ts' -o -name '*.tsx' 2>/dev/null
}

# 去掉注释与字符串字面量后再匹配。
#
# 为什么要连字符串一起去掉：测试里列一张"被禁词表"（`["retry","fallback"]`）是**证伪用**的
# 数据，不是代码。只去注释会把它当违规——守卫误报一次，人就会开始绕过守卫。
# 反过来，真实代码里的 `fn retry()` / `.retry()` 是标识符，一定会被扫到。
#
# 全部替换都保留换行数，这样 grep -n 的行号仍然对得上原文件。
strip_comments() {
  python3 - "$1" <<'PY'
import re, sys

src = open(sys.argv[1], encoding='utf-8', errors='replace').read()

# 单次扫描（不是多次 sub）：分段的多次替换会互相污染——
# 例如先替换 `"retry"` 再替换剩下的引号对，会把 `", "` 也当成字符串吃掉。
# 一次性 token 化最不容易出错。
pattern = re.compile(
    r'''
      (?P<block> /\*.*?\*/ )
    | (?P<line> //[^\n]* )
    | (?P<raw>  r\#*"(?:\\.|[^"\\])*"\#* )
    | (?P<str>  "(?:\\.|[^"\\])*" )
    | (?P<chr>  '(?:\\.|[^'\\\n])*' )
    | (?P<tpl>  `(?:\\.|[^`\\])*` )
    ''',
    re.S | re.X,
)

# 只保留换行数：行号不漂，且被替换内容不再可能被后续规则二次解释。
src = pattern.sub(lambda m: '\n' * m.group(0).count('\n'), src)
sys.stdout.write(src)
PY
}

scan_code() {
  local desc="$1" pattern="$2"
  shift 2
  local hits
  hits="$(for f in "$@"; do [ -f "$f" ] || continue; strip_comments "$f" | grep -nEi "$pattern" | sed "s|^|${f#$ROOT/}:|"; done)"
  if [ -n "$hits" ]; then
    bad "$desc"
    printf '%s\n' "$hits" | head -20 | sed 's/^/          /'
  else
    ok "$desc"
  fi
}

say "== xraytun-next guard =="

# ---------------------------------------------------------------- I1 无等待
#
# 用 while-read 而不是 `mapfile`：macOS 自带的是 bash 3.2，没有 `mapfile`
# （CI 上真红过一次）。这个脚本要在 Linux 与 macOS 上都跑得起来 ——
# 「本机能过、换台机器就过不了」正是这个仓库反复在消除的那类问题。
CRATES=()
while IFS= read -r _f; do CRATES+=("$_f"); done < <(src_files)
UIS=()
while IFS= read -r _f; do UIS+=("$_f"); done < <(ui_files)

scan_code "I1 无 sleep / 无轮询（Rust）" \
  '\b(sleep|sleep_ms|usleep|nanosleep)\s*\(|\bpoll_interval\b|\bspin_loop\b' "${CRATES[@]}"
scan_code "I1 无 sleep / 无定时器（UI）" \
  '\bset(Interval|Timeout)\s*\(|\bsleep\s*\(' "${UIS[@]}"

# ----------------------------------------------------------------- I2 无回落
scan_code "I2 无回落 / 无重试 / 无降级（Rust）" \
  '\b(fn|let|mut)?\s*_?(fallback|fall_back|failover|backoff|retry|retries|retrying|reattempt)\b|回落|兜底|降级' "${CRATES[@]}"
scan_code "I2 无回落 / 无重试（UI）" \
  '\b(fallback|fallbackTo|retry|retryCount|backoff)\b|回落|兜底' "${UIS[@]}"

# ------------------------------------------------------------- I3 无假数据
if [ ${#UIS[@]} -gt 0 ]; then
  runtime_ui=()
  for f in "${UIS[@]}"; do
    case "$f" in *.test.ts|*.test.tsx) continue;; esac
    runtime_ui+=("$f")
  done
  if [ ${#runtime_ui[@]} -gt 0 ]; then
    scan_code "I3 运行时界面无 mock / preview 数据源" \
      'previewSnapshot|previewLogs|previewConnections|previewTopology|mockData|fakeData|dummyData|loremIpsum|placeholderData' "${runtime_ui[@]}"
  fi
fi

# ------------------------------------------------- 未完成 / 被跳过的测试
if [ ${#CRATES[@]} -gt 0 ]; then
  hits="$(grep -nE 'todo!\(|unimplemented!\(|#\[ignore\]|TODO\(|FIXME' "${CRATES[@]}" 2>/dev/null | sed "s|$ROOT/||")"
  if [ -n "$hits" ]; then bad "禁止 todo!/unimplemented!/#[ignore]/FIXME（占位与跳过都是隐性假话）"; printf '%s\n' "$hits" | head -20 | sed 's/^/          /'; else ok "无占位与跳过测试"; fi

  # unwrap 在**非测试**代码里 = 隐藏的 panic 路径，记为警告而不是违规（锁中毒等场景有合理用法）。
  # 约定：crates 里的测试模块写在文件末尾的 `#[cfg(test)]` 之后，所以只统计它之前的行。
  # 报出位置而不是只报数量 —— 只给数字等于让人去猜。
  n_unwrap="$(python3 - "${CRATES[@]}" <<'PY'
import re, sys
hits = []
for path in sys.argv[1:]:
    # 集成测试文件（crates/*/tests/*.rs）允许 unwrap：那里的 panic 就是测试失败
    if '/tests/' in path:
        continue
    try:
        lines = open(path, encoding='utf-8', errors='replace').read().splitlines()
    except OSError:
        continue
    cut = len(lines)
    for i, ln in enumerate(lines):
        if re.match(r'\s*#\[cfg\(test\)\]', ln):
            cut = i
            break
    for i, ln in enumerate(lines[:cut], 1):
        if '.unwrap()' in ln:
            hits.append('%s:%d' % (path, i))
for h in hits[:20]:
    print(h)
print('COUNT=%d' % len(hits))
PY
)"
  n="$(printf '%s\n' "$n_unwrap" | sed -n 's/^COUNT=//p')"
  if [ "${n:-0}" -gt 0 ]; then
    warn "非测试 src 里有 $n 处 .unwrap()（需人工确认不是隐藏失败路径）"
    printf '%s\n' "$n_unwrap" | grep -v '^COUNT=' | sed 's/^/          /'
  else
    ok "非测试 src 无 .unwrap()"
  fi
fi

# ----------------------------------------------------- I4 依赖方向（单向）
check_deps() {
  local crate="$1"; shift
  local allowed=" $* "
  local manifest="$ROOT/crates/$crate/Cargo.toml"
  [ -f "$manifest" ] || return 0
  # 只看 [dependencies] 段（dev-dependencies 允许任意，测试可以依赖任何东西）
  local deps
  # 形如 `xt-contract.workspace = true` → 取第一个点之前的部分
  deps="$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f && /^xt-[a-z-]+/{n=split($1,p,"."); if (n>0) print p[1]}' "$manifest")"
  local d
  for d in $deps; do
    case "$allowed" in
      *" $d "*) ;;
      *) bad "I4 $crate 依赖了 $d —— 违反分层表（见 00-CONTRACT-FREEZE.md §2）";;
    esac
  done
}
ALL="xt-contract xt-bus xt-state xt-settings xt-subs xt-xrayconf xt-ipc xt-nodes xt-stats xt-probe xt-datapath"
check_deps xt-contract
check_deps xt-bus xt-contract
check_deps xt-state xt-contract
check_deps xt-settings xt-contract
check_deps xt-subs xt-contract
check_deps xt-xrayconf xt-contract
check_deps xt-ipc xt-contract
check_deps xt-nodes xt-contract xt-settings xt-subs xt-xrayconf
check_deps xt-stats xt-contract
check_deps xt-probe xt-contract xt-subs
check_deps xt-datapath xt-contract xt-bus xt-xrayconf
check_deps xt-daemon $ALL
check_deps xt-cli xt-contract xt-ipc
# macOS 层（S1/S2）：helperd → helperproto/macosnet → contract；macosnet 不认识协议、只认系统调用。
check_deps xt-helperproto xt-contract xt-ipc
check_deps xt-macosnet xt-contract
check_deps xt-helperd xt-contract xt-ipc xt-helperproto xt-macosnet
[ "$violations" -eq 0 ] && ok "I4 依赖方向与分层表一致"

# ------------------------------------------------------------------- 汇总
say ""
say "crates=$(find "$ROOT/crates" -name '*.rs' | wc -l | tr -d ' ') 个 Rust 文件, ui=$(( ${#UIS[@]} )) 个 TS 文件"
say "违规=$violations 警告=$warnings"
if [ "$violations" -gt 0 ]; then
  echo "GUARD FAILED"
  exit 1
fi
echo "GUARD PASSED"
