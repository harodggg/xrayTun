#!/bin/sh
# 一次性的开发环境准备。
#
# 这个脚本做的事都可以手动完成；写成脚本是为了让「第一次跑起来」
# 不需要读完整份文档。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> 1/5 检查工具链"
for tool in cargo rustc node npm; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "缺少 ${tool}，请先安装" >&2
    exit 1
  fi
done
echo "  cargo $(cargo --version | cut -d' ' -f2)"
echo "  node  $(node --version)"

echo "==> 2/5 检查 Xray 核心"
if [ -x "$ROOT/apps/desktop/binaries/xray" ]; then
  echo "  已存在：$("$ROOT/apps/desktop/binaries/xray" version | head -1)"
else
  echo "  未找到，运行 scripts/fetch-xray.sh"
  # 不自动下载：它要访问 GitHub，且版本选择应该由使用者决定。
  XRAY_MISSING=1
fi

echo "==> 3/5 构建 Rust 侧（core + helper + 桌面壳）"
# 依赖缓存放工作区里，避免污染用户主目录（在受限环境里 ~/.cargo 可能不可写）。
export CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/../.cargo-target}"
cargo build --workspace

echo "==> 4/5 构建前端"
if [ ! -d "$ROOT/apps/ui/node_modules" ]; then
  # npm 缓存同样放工作区，避免 ~/.npm 权限问题。
  (cd "$ROOT/apps/ui" && npm install --no-audit --no-fund --cache "${NPM_CACHE:-$ROOT/../.npm-cache}")
fi
(cd "$ROOT/apps/ui" && npm run build)

echo "==> 5/5 跑单元测试"
cargo test --workspace

cat <<'EOF'

完成。接下来：

  1) 前台起 helper（需要 root，只用于验证协议层）：
       sudo cargo run -p xt-helper -- run --socket /tmp/xraytun-helper.sock
     另开一个终端查状态：
       cargo run -p xt-helper -- status --socket /tmp/xraytun-helper.sock

  2) 起完整应用（先在「设置」里安装 helper，否则只有系统代理模式可用）：
       (cd apps/ui && npm run dev)      # 终端 A
       cargo run -p xraytun-desktop     # 终端 B

  3) 只看界面（不连后端）：
       (cd apps/ui && npm run dev)

注意：TUN 模式的端到端验证需要 root 环境与真实核心，见 docs/02 §7。
EOF

if [ "${XRAY_MISSING:-0}" = "1" ]; then
  echo
  echo "提醒：没有找到 Xray 核心，TUN 模式无法启动。运行 scripts/fetch-xray.sh 下载。"
fi
