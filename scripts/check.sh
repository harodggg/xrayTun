#!/usr/bin/env bash
#
# 本地跑一遍 CI 跑的全部检查。
#
# # 为什么需要它
#
# `ci.yml` 曾经把 TypeScript 检查写成：
#
#     npm --prefix apps/ui exec tsc -- --noEmit
#
# 而 `npm --prefix` 只决定「去哪找 node_modules」，**不改工作目录**。
# `tsc` 不带 `-p` 时只在当前目录找 `tsconfig.json`，从仓库根跑就找不到 ——
# 于是它打印一大段帮助文本并以**退出码 1** 结束。日志里看不出是路径问题。
#
# 这条 CI 红了**三个提交**都没人发现，因为本地没有任何一条命令会踩到它：
# 我本地跑的是 `npm run build`（`npm run` 会把 cwd 切到包目录，所以那条是对的）。
#
# 教训不是「下次注意」，而是：**CI 里的每一步都必须能在本地用同一条命令跑**。
# 所以 CI 现在直接调用这个脚本，两边不可能再漂移。
#
#     ./scripts/check.sh      # 就是 CI 跑的东西
#
# 发版流程（release.yml）在打包前也会调它，但跳过最后那次 release 构建 ——
# 那一步在发版流程里由 package-macos.sh 用真正的 universal target 做过了：
#
#     ./scripts/check.sh --no-release-build
#
set -euo pipefail

SKIP_RELEASE_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --no-release-build) SKIP_RELEASE_BUILD=1 ;;
    -h | --help)
      sed -n '3,20p' "$0"
      exit 0
      ;;
    *)
      # 必须写成 `${arg}`：紧跟中文全角括号时，`$arg（` 会让 bash 把多字节
      # 字符当成变量名的一部分，在 `set -u` 下报 `arg（: unbound variable`。
      # （之前 `$APP_ARCH，` 踩过一次同样的坑，见 CHANGELOG 0.2.x。）
      echo "未知参数：${arg}（可用：--no-release-build）" >&2
      exit 2
      ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 与 ci.yml 的 env 保持一致：依赖缓存放进工作区，和 scripts/dev.sh 的约定相同。
export CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/../.cargo-target}"
export npm_config_cache="${npm_config_cache:-$ROOT/../.npm-cache}"

# ---------------------------------------------------------------- 构建锁
#
# 为什么：2026-09-22 在 `6aa3b5e` 上，另一个人**同时**在同一个 `CARGO_TARGET_DIR` 上跑
# `cargo test`，于是本脚本最后一步 `Doc-tests` 报 `error[E0463]: can't find crate for …`
# —— **逐项全绿、只在最后一步红**，看起来像产品缺陷，实际是并发把门禁弄红了。
# 隔离 runner 上的同一套检查（CI 35696466193 / 35694470992 / 35693699077）**全绿**。
#
# 所以：这次不是「记住不要并行跑」，而是把它变成机制。实现与行为见
# `scripts/build-lock.sh` 头部注释（原子 `mkdir` 锁目录、拿不到锁会打印持有者、
# 超时退出码 75、pid 探活自救、stale 上限 4 小时）。
source "$ROOT/scripts/build-lock.sh"
trap 'release_build_lock' EXIT INT TERM HUP
# 拿不到锁 ⇒ **明确失败**（退出码 75），绝不「等超时后继续跑」。
acquire_build_lock "scripts/check.sh $*" || exit $?

# ---------------------------------------------------------------- worktree 产物身份守卫
#
# 与上面的锁**不是一回事**：锁管「并发时序」（谁先谁后），这里管「**产物身份**」（链到哪一份）。
# 排队排完照样可能链错：同一个 `CARGO_TARGET_DIR` 里装着两个 checkout 的同名同版本 crate
# （例如 `xt-core 0.8.34` vs `xt-core 0.8.34`、源码不同），谁最后写就链谁。
# 2026-09-22 的真实症状：`error[E0425]: cannot find type … in module xt_core::store`（源码里明明有），
# `touch` 一下源文件强制重编就好了 —— 因为那是**另一个 worktree 编出来的旧 rlib**。
#
# 所以：在 **linked worktree** 里跑门禁时，`CARGO_TARGET_DIR` 必须指向它自己的目录。
# 正确做法：`./scripts/wt.sh run <name> -- ./scripts/check.sh …`（自动隔离 + 自动拿锁）。
# `WT_STRICT=1` 时这里会**明确失败**（75），而不是只警告。
_git_dir="$(git rev-parse --path-format=absolute --git-dir 2>/dev/null || true)"
_git_common="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
# ⚠️ 主工作区的 target dir 必须从 git common dir 推：在 worktree 里 `$ROOT/../.cargo-target`
# 指的是**worktree 自己旁边**的目录，拿它比较会漏判（第一版就漏了）。
_main_target="$(dirname "$_git_common")/../.cargo-target"
#
# 判据是**三态**，不是两态：**只有两侧都成功取到真实路径才比较**。
# 取不到时（例如全新 checkout 还没建过 target dir、或 CARGO_TARGET_DIR 指向不存在的路径）
# ⇒ 明确说「**无法判定**」——**既不能判为相等，也不能因此假失败**：
#   · 普通模式：打一行**可见**提示（不是 debug），然后继续跑；
#   · `WT_STRICT=1`：退出 **75**。75 在本项目的定义是「**环境问题，不是代码失败**」
#     （见 `docs/verification/BUILD-LOCK.md`），而 strict 的意义就是
#     「拿不到干净结论就不要门禁结论」—— fail closed，与本项目其它处一致。
# ⚠️ 已知摩擦（不是 bug）：**全新 checkout 上主 target dir 还不存在** ⇒ `WT_STRICT=1` 会给一次 75。
#     先跑一次构建（或先用普通模式跑一次门禁）让 target dir 出现即可。
if [ -n "$_git_common" ] && [ "$_git_dir" != "$_git_common" ]; then
  _tgt_real="$(cd "${CARGO_TARGET_DIR:-}" 2>/dev/null && pwd -P || true)"
  _main_real="$(cd "$_main_target" 2>/dev/null && pwd -P || true)"
  if [ -z "$_tgt_real" ] || [ -z "$_main_real" ]; then
    echo
    echo "  ℹ️  **无法判定** worktree 产物身份（target dir 的真实路径取不到）—— **这是环境问题，不是代码失败**："
    echo "      CARGO_TARGET_DIR = ${CARGO_TARGET_DIR:-（未设）}"
    echo "                          → ${_tgt_real:-取不到（路径不存在？）}"
    echo "      主工作区 target   = ${_main_target}（期望值，由 git-common-dir 推得）"
    echo "                          → ${_main_real:-取不到（还没建过？）}"
    echo "      因此**不做判定**：它不等于「两边是同一份」，也不等于「已经隔离」。"
    echo "      提示：全新 checkout 还没跑过构建时，主 target dir 尚不存在 —— 先跑一次即可。"
    if [ "${WT_STRICT:-0}" = "1" ]; then
      echo "  ✗ WT_STRICT=1：无法判定 ⇒ 明确失败（退出码 75）；**这是环境问题，不是代码失败**" >&2
      exit 75
    fi
  elif [ "$_tgt_real" = "$_main_real" ]; then
    echo
    echo "  ⚠️  正在 **linked worktree** 里跑门禁，且 CARGO_TARGET_DIR 指向**主工作区**的 target dir："
    echo "      cwd              = $ROOT"
    echo "      CARGO_TARGET_DIR = $CARGO_TARGET_DIR"
    echo "      同一个 target dir 里会有两个 checkout 的同名同版本 crate ⇒ 可能链到**另一份**的 rlib，"
    echo "      症状是「源码里明明有的类型却报 E0425」，或者更糟：敏感性实验静默链到对面那份。"
    echo "      正确做法：./scripts/wt.sh run <name> -- ./scripts/check.sh $*"
    if [ "${WT_STRICT:-0}" = "1" ]; then
      echo "  ✗ WT_STRICT=1：共享 target dir ⇒ 明确失败（退出码 75）" >&2
      exit 75
    fi
  fi
fi

step() {
  echo
  echo "=============================================================="
  echo "  $*"
  echo "=============================================================="
}

# ---------------------------------------------------------------- 核心

# `apps/desktop/binaries/xray` 在 .gitignore 里（上游按架构分发，不能进仓库），
# 所以刚 clone 下来时必须先取一次。CI 上是空目录，本地通常已经有了。
step "取 Xray 核心"
if [ -x apps/desktop/binaries/xray ]; then
  echo "  已存在，跳过"
else
  ./scripts/fetch-xray.sh
fi

# ---------------------------------------------------------------- 前端

step "前端依赖"
if [ -d apps/ui/node_modules ]; then
  echo "  已安装，跳过（CI 上是空目录，会走 npm ci）"
else
  npm --prefix apps/ui ci --no-audit --no-fund
fi

step "前端单元测试"
# 前端也有需要回归保护的行为（例如日志的跟随滚动），跑在类型检查之前：
# 测试挂了就没必要再往下走。
npm --prefix apps/ui test

step "TypeScript 类型检查"
# 项目路径必须显式给：见文件开头的说明。
npm --prefix apps/ui exec tsc -- --noEmit -p apps/ui/tsconfig.json

step "CSS token 定义性（被 var() 引用但未定义的 token）"
# 为什么要有这一步：这一类「**静默失效的声明**」已经咬过本项目三次 ——
#   1. `--line` 从未定义 → 拓扑那条分隔线从未渲染（task-12 / e7ed509）；
#   2. `xattr -dr` 必然失败，而 `2>/dev/null` 把证据吞了（task-46）；← 同族：失败不报错
#   3. `--border-interactive` 在**应用 bundle 里**未定义 → border-color 落到 currentColor
#      → 选中态多一圈近白描边（task-57 / b91ffb1）。
# 浏览器不报错、样式表不报错、tsc 不报错 —— 只有人拿放大镜量计算样式才看得出来。
# 判据的作用域**按 bundle**（不是全仓库）：见 scripts/check-css-tokens.py 头部注释，
# 这条正是第 3 个 bug 逃过所有人眼睛的原因（「全仓库搜一下，官网那边有啊」）。
python3 scripts/check-css-tokens.py

step "现场包脚本自测（helper 三态 / 脱敏 / 分诊；不依赖 cargo）"
# 为什么（task-173）：这三条自测原来**不在任何门禁里** —— 判据存在但没人执行 = 明天改坏了没人知道。
# 任一非 0 ⇒ 门禁红（**禁止** `|| true` 之类的吞错）。
# Python 与 Rust 的 helper 三态判据共用夹具 scripts/fixtures/helper-version-cases.json
# （Rust 是权威；Python 自测与 Rust 测试都读它）—— 见 docs/verification/HELPER-TRISTATE-CALIBER.md。
python3 scripts/helper_tristate.py --self-test
bash scripts/incident-bundle.sh --self-test
python3 scripts/triage-incident.py --self-test

# task-185：worktree 的**位置**也要有守卫 —— 默认值曾经落在 `${TMPDIR}` 下，
# 2026-09-24 16:25 整棵树被系统清理，三个正在编译/验证的 worktree 目录整个消失
# （其中两个是队友的在途工作），而失败方式是「跑到一半目录没了」。
# 这条自测只做**路径解析与守卫行为**（不建 worktree、不跑 cargo），秒级、只读。
step "worktree 位置守卫自测（wt.sh：默认不在 ${TMPDIR} 下）"
bash docs/verification/verify-wt-dir-root.sh

step "前端构建"
# 注意这条是对的：`npm run` 会把 cwd 切到包目录，脚本里的 `tsc --noEmit`
# 因此能找到 tsconfig.json。上一行那个 `npm exec` 不会。
npm --prefix apps/ui run build

# ---------------------------------------------------------------- 站点版本

# 为什么要有这一步（这是**发版流程真的漏过的地方**）：
# v0.8.27 发版时没人同步官网 —— 站点一直写着上一版的版本号，下载按钮指向两个版本前的
# 资产，而当时**没有任何检查会发现**：发版只改 Cargo.toml / tauri.conf.json /
# apps/ui/package.json / CHANGELOG.md，从不碰 site/**。
# 这里把「站点声明的版本 == Cargo.toml 的 workspace 版本」钉死，漂移在 CI 与发版前都红，
# 而不是等用户下载到旧包。
#
# 只断言**真源**（两个生成器常量 + site.js）并对手写页面抽样一条下载文件名；
# 全量扫 site/** 会因为历史锚定（例如「v0.8.27 之前」这类故意的版本边界）而变脆。
step "站点版本一致性（site/** ↔ Cargo.toml）"

want="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
if [ -z "$want" ]; then
  echo "  ✗ 无法从 Cargo.toml 的 [workspace.package] 读出 version" >&2
  exit 1
fi

site_ver_problems=""
check_site_ver() { # $1=文件  $2=sed 提取表达式  $3=可读标签
  local got
  got="$(sed -n "$2" "$1" | head -1)"
  if [ "$got" = "$want" ]; then
    printf '  ✓ %-30s %s\n' "$3" "$got"
  else
    printf '  ✗ %-30s 实际 %s / 期望 %s （期望来自 Cargo.toml [workspace.package] version）\n' \
      "$3" "${got:-（未找到）}" "$want" >&2
    site_ver_problems="${site_ver_problems}  ${1}"
  fi
}

check_site_ver scripts/gen-site-jsonld.py 's/^XRAYTUN_VERSION *= *"\([^"]*\)".*/\1/p' "gen-site-jsonld.py XRAYTUN_VERSION"
check_site_ver scripts/gen-site-geo.py 's/^VERSION *= *"\([^"]*\)".*/\1/p' "gen-site-geo.py VERSION"
check_site_ver site/assets/site.js 's/.*PAGE_VERSION *= *"\([^"]*\)".*/\1/p' "site.js PAGE_VERSION"
check_site_ver scripts/gen-site-images.py 's/^SITE_VERSION *= *"\([^"]*\)".*/\1/p' "gen-site-images.py SITE_VERSION"
check_site_ver site/index.html 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' "site/index.html 下载文件名"
check_site_ver site/en/index.html 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' "site/en/index.html 下载文件名"

if [ -n "$site_ver_problems" ]; then
  echo >&2
  echo "  ✗ 站点声明的版本与 workspace 版本（$want）不一致 —— 访客会下载到旧的安装包。" >&2
  echo "    不一致的文件：${site_ver_problems}" >&2
  echo "    修法：先改两个生成器常量（真源），再改手写页面（含下载文件名、字节数、MiB 取整），" >&2
  echo "    最后重跑：python3 scripts/gen-site-jsonld.py gen && python3 scripts/gen-site-geo.py" >&2
  exit 1
fi
echo "  ✓ 站点声明的版本与 Cargo.toml 一致：$want"

# ---------------------------------------------------------------- 站点 GEO 产物

# task-180：`gen-site-geo.py` 会**整文件重写** robots/sitemap/llms*，而另一条工作流的项目页
# 收录（`/jev-x-filter/`、`/beauty-meter/` …）是**手写**加进去的 —— 以前每次重跑都会把它静默抹掉
# （v0.8.38 停发期间真的发生两次）。生成器现在按归属/标记区间保留外部条目、并在保不住时**非零拒绝**；
# 这里再钉一条：**盘上产物必须等于生成器的重算结果**（只读、不写盘、秒级）。
# 手写进生成区块、或产物过期，都会在这里变红 —— 下一任维护者不读文档也会被拦住。
step "站点 GEO 产物一致性（gen-site-geo.py check）"
python3 scripts/gen-site-geo.py check

# ---------------------------------------------------------------- Rust

step "clippy（warning 视为错误）"
cargo clippy --workspace --all-targets -- -D warnings

step "单元测试"
cargo test --workspace

if [ "$SKIP_RELEASE_BUILD" -eq 1 ]; then
  echo
  echo "  （--no-release-build：跳过 release 构建，发版流程里由 package-macos.sh 代劳）"
else
  step "release 构建（打包路径也要能编过）"
  cargo build --release --workspace
fi

echo
echo "✓ 与 CI 相同的全部检查通过"

# 一行提示（只在没人设这两个变量时打一次）：
# `BUILD_LOCK_STRICT=1` 只在「探测到**与我们同一个 target dir** 的未持锁 cargo/rustc」时
# **明确失败**（跨 target dir 的并发只提示 —— per-target-dir 锁本来就不互斥，见
# scripts/build-lock.sh 头部），而不是可能在并发下给出一场假红（实例：Doc-tests 报 E0463
# can't find crate）。发版前建议用它。
# CI 是隔离 runner、没有并发，所以这个变量在 CI 上无害（不设即可）。
if [ "${BUILD_LOCK_STRICT:-0}" != "1" ] && [ "${BUILD_LOCK_FOREIGN_WAIT:-0}" = "0" ]; then
  echo "  提示：发版前建议用 BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build（**同一 target dir** 的并发编译会明确失败，而不是给出假红）"
fi
