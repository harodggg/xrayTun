#!/bin/sh
# 打一个可分发（但未签名）的 macOS 包：XrayTun.app + XrayTun_<版本>_<架构>.dmg
#
# ```bash
# ./scripts/package-macos.sh
# ```
#
# # 为什么需要这个脚本，而不是直接 `tauri build`
#
# Tauri 的 `bundle.resources` 只会把文件放进 `Contents/Resources/`，
# 而这个 App 对两个位置有**硬性**要求：
#
#   * 核心：`Contents/Resources/xray`
#     —— `resolve_core_binary` 找的就是资源目录下的 `xray`；
#       而且 geoip.dat / geosite.dat 必须和它**同级**，因为
#       `XRAY_LOCATION_ASSET` 取的是核心二进制的父目录。放错地方
#       不会报错，只会让 `geoip:cn` / `geosite:cn` 静默不命中。
#
#   * helper：`Contents/MacOS/xraytun-helper`
#     —— `helper_binary_path` 只在自己可执行文件的**同级目录**找它。
#       `resources` 放不进 `Contents/MacOS/`，所以这一步由本脚本补。
#
# 少了 helper 的后果不是「退化成系统代理模式」，而是点安装时直接报
# 「找不到 helper 二进制」。
#
# # 签名
#
# 这里只做 **ad-hoc 签名**（`codesign -s -`）。它能保证 App 在本机
# 完整性校验通过；但因为不是 Developer ID 签名，也没有公证（notarize），
# 别人从网上下载后 Gatekeeper 仍会拦截，需要右键「打开」或：
#
#   xattr -dr com.apple.quarantine /Applications/XrayTun.app
#
# 要真正免打扰分发，必须有付费开发者账号，然后：
#   codesign --deep --force --options runtime --sign "Developer ID Application: ..." \
#            --entitlements apps/desktop/entitlements.plist XrayTun.app
#   xcrun notarytool submit ... && xcrun stapler staple XrayTun.app

set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/../.cargo-target}"
export npm_config_cache="${npm_config_cache:-$ROOT/../.npm-cache}"

# 可选：交叉/通用构建的目标三元组。
#
# 设成 `universal-apple-darwin` 就出一个同时含 x86_64 与 arm64 的包
# （CI 里就是这么用的，见 .github/workflows/release.yml）。前提是
# 两个 target 都装了：`rustup target add x86_64-apple-darwin aarch64-apple-darwin`。
# Homebrew 装的 rust 没有 rustup，加不了 target，只能用默认主机架构。
TARGET_TRIPLE="${XRAYTUN_TARGET:-}"
if [ -n "$TARGET_TRIPLE" ]; then
  TARGET_FLAG="--target $TARGET_TRIPLE"
  # cargo 会把产物写进 <target-dir>/<triple>/release
  RELEASE_DIR="$CARGO_TARGET_DIR/$TARGET_TRIPLE/release"
else
  TARGET_FLAG=""
  RELEASE_DIR="$CARGO_TARGET_DIR/release"
fi

TAURI="$ROOT/apps/ui/node_modules/.bin/tauri"
if [ ! -x "$TAURI" ]; then
  echo "缺少 Tauri CLI。先执行：" >&2
  echo "  (cd apps/ui && npm install)" >&2
  exit 1
fi

BIN="$ROOT/apps/desktop/binaries"
for f in xray geoip.dat geosite.dat; do
  if [ ! -f "$BIN/$f" ]; then
    echo "缺少 $BIN/$f —— 先执行 ./scripts/fetch-xray.sh" >&2
    exit 1
  fi
done

echo "==> 1/4 构建 release 版 Rust（含 helper）"
cargo build --release --workspace $TARGET_FLAG

# 前端单独构建一次。
#
# 不能只依赖 tauri.conf.json 里的 `beforeBuildCommand`：Tauri 是以
# `apps/`（app 目录的**父目录**）为 cwd 执行它的，而不是 `apps/desktop`。
# 这一点极反直觉 —— 写错时的表现是 npm 在 `xray-tun/ui` 找不到 package.json。
# 这里显式构建，既绕开那个基准问题，也保证**不会把过期的 dist 打进包里**；
# 静默打出一个旧前端是最难发现的一类打包事故。
echo "==> 2/4 构建前端"
(cd "$ROOT/apps/ui" && npm run --silent build)
if [ ! -f "$ROOT/apps/ui/dist/index.html" ]; then
  echo "前端构建没有产出 apps/ui/dist/index.html" >&2
  exit 1
fi

echo "==> 3/4 打包 .app"
# 只打 .app，**不用** Tauri 的 dmg 目标。
#
# Tauri 生成 dmg 时调的是它自带的 bundle_dmg.sh，那个脚本要靠 AppleScript
# 让 Finder 去摆图标位置。在没有图形会话、或没给自动化权限的环境里
# （CI、被沙箱限制的终端）它必定失败，报错只有一句
# "error running bundle_dmg.sh"，看不出是权限问题。
# 我们随后用 hdiutil 直接打 dmg：功能上少一个漂亮的背景图，但到处都能跑。
(cd "$ROOT/apps/desktop" && "$TAURI" build --bundles app $TARGET_FLAG)

# 版本号从 tauri.conf.json 读，**不要硬编码**。
# 之前这里写死了 0.1.0，改版本时忘了同步就会打出一个名字对、内容错的包，
# 而且产物名还会和上一版撞车（下载页面上分不清哪个是哪个）。
APP_VERSION="$(python3 -c "import json,sys;print(json.load(open('$ROOT/apps/desktop/tauri.conf.json'))['version'])")"
if [ -z "$APP_VERSION" ]; then
  echo "无法从 tauri.conf.json 读出版本号" >&2
  exit 1
fi
echo "  · 版本 ${APP_VERSION}"

APP="$RELEASE_DIR/bundle/macos/XrayTun.app"
if [ ! -d "$APP" ]; then
  echo "打包完成但没找到 $APP" >&2
  exit 1
fi

echo "==> 4/4 把 helper 放进 Contents/MacOS/ 并校验"
HELPER_SRC="$RELEASE_DIR/xraytun-helper"
if [ ! -f "$HELPER_SRC" ]; then
  echo "找不到 release 版 helper：$HELPER_SRC" >&2
  exit 1
fi
install -m 0755 "$HELPER_SRC" "$APP/Contents/MacOS/xraytun-helper"

fail=0
check() {
  if [ -e "$2" ]; then
    echo "  ✓ $1  ($(du -h "$2" | cut -f1 | tr -d ' '))"
  else
    echo "  ✗ $1 缺失：$2" >&2
    fail=1
  fi
}
check "核心"        "$APP/Contents/Resources/xray"
check "geoip.dat"   "$APP/Contents/Resources/geoip.dat"
check "geosite.dat" "$APP/Contents/Resources/geosite.dat"
check "helper"      "$APP/Contents/MacOS/xraytun-helper"
check "Info.plist"  "$APP/Contents/Info.plist"
check "主程序"      "$APP/Contents/MacOS/xraytun-desktop"

# 前端**不在** Resources 里：Tauri 会把 dist 压缩后嵌进可执行文件，
# 所以这里查不到 dist 目录是正常的（早先这里写了一条错误检查，
# 结果每次打包都误报「前端产物缺失」）。真正的风险是「打包时 dist 是旧的」，
# 那一条已经在第 2 步用「构建后再校验产物」挡住了。

# geo 文件必须和核心同级，否则 XRAY_LOCATION_ASSET 指不到它们。
if [ -f "$APP/Contents/Resources/xray" ] && [ ! -f "$APP/Contents/Resources/geosite.dat" ]; then
  echo "  ✗ geoip/geosite 与核心不同级，分流规则会静默失效" >&2
  fail=1
fi

# 未签名的话，本机 macOS 也可能拒绝启动（尤其带 helper 安装流程时）。
# ad-hoc 签名不解决分发问题，但能让本机跑起来。
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
  && echo "  ✓ 已 ad-hoc 签名" \
  || { echo "  ⚠ ad-hoc 签名失败（本机仍可能能用）" >&2; }

# 产物名里的架构必须取**主程序真实的架构**，而不是 `uname -m`。
# 本机是 Apple Silicon 但 Rust 工具链可能是 x86_64（Homebrew 装在 /usr/local），
# 这时 uname 会给出 arm64，而包里其实是 x86_64 的可执行文件 ——
# 下载的人会挑错包。
APP_ARCH="$(lipo -archs "$APP/Contents/MacOS/xraytun-desktop" 2>/dev/null | tr ' ' '_')"
[ -n "$APP_ARCH" ] || APP_ARCH="$(uname -m)"
CORE_ARCH="$(lipo -archs "$APP/Contents/Resources/xray" 2>/dev/null | tr ' ' '_')"
echo "  · App 架构 $APP_ARCH / 核心架构 ${CORE_ARCH:-未知}"

# dmg 必须在 inject helper + 签名**之后**再做，否则里面是一个没有 helper 的 App。
#
# 为什么不用 `hdiutil create -srcfolder`（教科书上的那一条）：
# 它内部要把临时镜像**挂载**起来才能拷文件，而在没有图形会话、
# 或设备挂载被禁止的环境里必定失败，报错还只有一句
# `hdiutil: create failed - 目录非空`，完全指不到真正的原因。
# 改成两步，全程不挂载：
#   1. makehybrid 直接从目录生成 HFS 镜像
#   2. convert 把它压成 UDZO
DMG_DIR="$RELEASE_DIR/bundle/dmg"
mkdir -p "$DMG_DIR"
DMG="$DMG_DIR/XrayTun_${APP_VERSION}_$APP_ARCH.dmg"
RAW="$DMG_DIR/.xraytun-raw.dmg"
rm -f "$DMG" "$RAW"
# 必须先搭一个**暂存目录**，且这个目录里要放 .app 本身。
#
# `makehybrid -srcfolder <dir>` 是把 dir 的**内容**当作镜像根目录。
# 早先直接写 `-srcfolder "$APP"` 时，镜像根目录变成了 .app 内部的
# `Contents/` —— 打开 dmg 看到的是一个裸的 Contents 文件夹，
# 没有可拖拽的 App。用户只能自己去别处找 .app 再手动拷。
STAGE="$DMG_DIR/.dmg-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/XrayTun.app"
# /Applications 快捷方式：有它才能「拖进去就装完」。
# 没有它，用户面对一个孤零零的 .app 只能自己猜该放哪。
ln -s /Applications "$STAGE/Applications"

if hdiutil makehybrid -quiet -hfs -o "$RAW" -default-volume-name XrayTun "$STAGE" \
   && hdiutil convert -quiet "$RAW" -format UDZO -o "$DMG"; then
  rm -f "$RAW"
  echo "  ✓ 已生成 dmg（含 helper 与 /Applications 快捷方式）"
else
  rm -f "$RAW"
  DMG=""
  echo "  ⚠ 打 dmg 失败（本环境可能禁止挂载镜像），.app 仍然可用" >&2
fi
rm -rf "$STAGE"

# 挂载镜像实检一次。
#
# 这一条是补出来的：`-srcfolder` 的语义写错时，**退出码、产物大小、
# 签名校验全都正常**，只有用户双击打开 dmg 才会发现里面是一堆散文件。
# 唯一能在交付前发现它的办法就是把镜像挂起来看内容。
if [ -n "$DMG" ] && [ -f "$DMG" ]; then
  MP="$(mktemp -d)"
  if hdiutil attach -nobrowse -readonly -mountpoint "$MP" "$DMG" >/dev/null 2>&1; then
    if [ -d "$MP/XrayTun.app" ]; then
      echo "  ✓ 镜像根目录里有 XrayTun.app"
    else
      echo "  ✗ 镜像根目录里没有 XrayTun.app（用户会看到一堆散文件）" >&2
      fail=1
    fi
    if [ -L "$MP/Applications" ]; then
      echo "  ✓ 有 /Applications 快捷方式"
    else
      echo "  ✗ 缺 /Applications 快捷方式（用户不知道往哪拖）" >&2
      fail=1
    fi
    hdiutil detach "$MP" >/dev/null 2>&1 || hdiutil detach "$MP" -force >/dev/null 2>&1 || true
  else
    echo "  · 本环境无法挂载镜像做校验，已跳过（不影响产物）"
  fi
  rmdir "$MP" 2>/dev/null || true
fi

# 再给一个 zip：dmg 打不出来时它是唯一能直接分发的形态，
# 而且它天然保留符号链接与可执行位（用 ditto，不要用 zip 命令）。
ZIP="$DMG_DIR/XrayTun_${APP_VERSION}_$APP_ARCH.zip"
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP" \
  && echo "  ✓ 已生成 zip" \
  || { echo "  ⚠ 打 zip 失败" >&2; ZIP=""; }

if [ "$fail" != "0" ]; then
  echo
  echo "包不完整，已中止。" >&2
  exit 1
fi

cat <<EOF

完成：

  App : $APP
  DMG : ${DMG:-（本环境无法生成，见上面的警告）}
  ZIP : ${ZIP:-（生成失败）}

注意：这是 **ad-hoc 签名**的包，没有公证。别人下载后 Gatekeeper 会拦，
需要右键「打开」，或者：

  xattr -dr com.apple.quarantine /Applications/XrayTun.app

架构：App 是 ${APP_ARCH}，核心是 ${CORE_ARCH:-未知}。
两者不一致时（本机就是：x86_64 的 App + arm64 的核心），
App 经由 Rosetta 运行，核心仍是原生的。

要出真正的 universal 包，需要 rustup 装的 rust（而不是 Homebrew
/usr/local 那份），然后设 XRAYTUN_TARGET=universal-apple-darwin 再跑一次；
或者直接打 tag 交给 .github/workflows/release.yml 出包。
EOF
