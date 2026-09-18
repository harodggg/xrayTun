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
