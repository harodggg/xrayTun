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
