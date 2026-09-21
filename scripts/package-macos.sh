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
# 别人从网上下载后 Gatekeeper 仍会拦截。放行方式**按 macOS 版本分**：
#
#   * macOS 15 及以上：右键「打开」**已被 Apple 移除**（2024-08-06 公告），
#     只能去「系统设置 → 隐私与安全性」里对被拦的 App 点「仍要打开」；
#   * macOS 14 及更早：右键（或 Control-点击）→「打开」；
#   * 终端（两者皆可）：逐文件清 quarantine —— 用 `find … -exec … +`，因为 `xattr`
#     有**两个实现**、`-r` 的支持**随实现与版本而异**：
#       find /Applications/XrayTun.app -exec xattr -d com.apple.quarantine {} + 2>/dev/null
#
# ⚠️ 这里**必须**逐文件清，两点都是本机（macOS 26.6.2 / build 25G83）实测的：
#   * `xattr` 有**两个实现**：Apple 的 `/usr/bin/xattr`（支持 `-r`）与 PATH 上先命中的
#     Python `xattr` 包（**没有 `-r`**：`xattr -dr …` → exit 64，打印
#     `option -r not recognized`）。所以**产品脚本一律写绝对路径 `/usr/bin/xattr`**，
#     且**不要依赖 `-r`** —— 需要递归就用上面的 `find … -exec … +`；
#   * 只给 bundle 根路径的 `xattr -d com.apple.quarantine /Applications/XrayTun.app`
#     只清掉根上那一个 —— 实测 13 个带 quarantine 的文件里还剩 12 个。
# 上面示例是**给用户手动执行**的，保留 `2>/dev/null` 只为压掉 `No such xattr` 刷屏；
# **产品脚本里不许 `2>/dev/null`**（失败要留痕）。
# README 与 Release Notes 里的说法必须与这里一致。
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
# Homebrew 装的 rust 没有 rustup、只有宿主架构的 std，所以本地一般用默认。
TARGET_TRIPLE="${XRAYTUN_TARGET:-}"
TARGET_FLAG=""
RELEASE_DIR="$CARGO_TARGET_DIR/release"
# `universal-apple-darwin` 是 **Tauri CLI 的伪 target，不是 rustc 的 target**。
# 直接 `cargo build --target universal-apple-darwin` 必失败：
#
#     error: could not find specification for target "universal-apple-darwin"
#
# （`rustc --print target-list` 里没有它，实测 rustc 1.98。）Tauri 内部会把这个
# 伪 target 展开成「分别编 x86_64 与 aarch64，再 lipo」，但那只覆盖它自己构建
# 的 .app —— helper 是我们自己的 crate，得我们自己做同样的事（见下面 1/4）。
UNIVERSAL=0
if [ "$TARGET_TRIPLE" = "universal-apple-darwin" ]; then
  UNIVERSAL=1
  TARGET_FLAG="--target $TARGET_TRIPLE" # 只给 tauri build 用
  RELEASE_DIR="$CARGO_TARGET_DIR/$TARGET_TRIPLE/release"
elif [ -n "$TARGET_TRIPLE" ]; then
  TARGET_FLAG="--target $TARGET_TRIPLE"
  RELEASE_DIR="$CARGO_TARGET_DIR/$TARGET_TRIPLE/release"
  # 其余显式 target 必须是 rustc 真认识的，否则在这里就给出可读的报错，
  # 而不是让它变成编译日志中段一句看不出所以然的 cargo 错误。
  if ! rustc --print target-list 2>/dev/null | grep -qx "$TARGET_TRIPLE"; then
    echo "rustc 不认识 target '$TARGET_TRIPLE'。" >&2
    echo "  universal 请用 universal-apple-darwin（本脚本会自己处理）；" >&2
    echo "  其它可用目标见 rustc --print target-list。本机已装 std：" >&2
    ls "$(rustc --print sysroot)/lib/rustlib/" 2>/dev/null | grep -- "-apple-" | sed 's/^/    /' >&2
    exit 1
  fi
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
if [ "$UNIVERSAL" -eq 1 ]; then
  # helper 不在 Tauri 的构建范围内（它是我们自己的 crate），所以要自己做
  # 「分别编 + lipo」。Tauri 稍后会为 .app 里的主程序做同样的事。
  for t in x86_64-apple-darwin aarch64-apple-darwin; do
    echo "    · cargo build --release --target $t"
    cargo build --release --workspace --target "$t"
  done
  mkdir -p "$RELEASE_DIR"
  lipo -create \
    "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/xraytun-helper" \
    "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/xraytun-helper" \
    -output "$RELEASE_DIR/xraytun-helper"
  echo "    helper 架构：$(lipo -archs "$RELEASE_DIR/xraytun-helper")"
else
  cargo build --release --workspace $TARGET_FLAG
fi

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

# Tauri 把 universal 的合并产物放在 <target-dir>/universal-apple-darwin/release/
# （它自己处理的伪 target），非 universal 时就在对应 triple 目录下 —— 这两种
# 我们都靠 `RELEASE_DIR` 猜。猜错时的表现是后面一句「找不到 XrayTun.app」，
# 而真正的原因（Tauri 换了布局）被藏起来了。所以这里直接把**实际产出的位置**
# 列出来，让 CI 日志自己交代。
APP="$RELEASE_DIR/bundle/macos/XrayTun.app"
if [ ! -d "$APP" ]; then
  echo "Tauri 没有在预期位置产出 .app：" >&2
  echo "  期望：$APP" >&2
  echo "  实际找到的 XrayTun.app：" >&2
  find "$CARGO_TARGET_DIR" -type d -name "XrayTun.app" 2>/dev/null | head -10 | sed 's/^/    /' >&2
  echo "  TARGET_TRIPLE=${TARGET_TRIPLE:-<空>}  RELEASE_DIR=$RELEASE_DIR" >&2
  exit 1
fi

# 版本号从 tauri.conf.json 读，**不要硬编码**。
# 之前这里写死了 0.1.0，改版本时忘了同步就会打出一个名字对、内容错的包，
# 而且产物名还会和上一版撞车（下载页面上分不清哪个是哪个）。
APP_VERSION="$(python3 -c "import json,sys;print(json.load(open('$ROOT/apps/desktop/tauri.conf.json'))['version'])")"
if [ -z "$APP_VERSION" ]; then
  echo "无法从 tauri.conf.json 读出版本号" >&2
  exit 1
fi
echo "  · 版本 ${APP_VERSION}"

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
#
# **签名前必须先清掉多余 xattr。** v0.8.26 的真实产物里，App 的 **13 个文件**
# （含 `Contents/MacOS/xraytun-helper`、`xraytun-desktop`、`Resources/xray`）
# 全都带着 `com.apple.FinderInfo`，于是：
#
#   codesign --verify --verbose=1  → 通过（CI 一直在用这条，所以从没发现）
#   codesign --verify --strict     → **失败**：
#     "resource fork, Finder information, or similar detritus not allowed"
#     "Disallowed xattr com.apple.FinderInfo found on .../xraytun-helper"
#
# 现在不阻塞启动，但**公证与 Developer ID 分发一定会被挡住**，而且是非严格
# 校验看不见的静默失效。所以这里逐文件清掉，两个理由：
#   * `xattr -c` 对**目录不递归**（只清目录自己那一个）；
#   * `-r` 的支持**随实现与版本而异**（PATH 上先命中的 Python `xattr` 没有，
#     `xattr -cr` 会 exit 64），所以不能依赖 `xattr -cr`。
#
# 清完 `com.apple.provenance` 可能仍在（系统加的、不可删），但 codesign
# 容忍它 —— 实测上述 13 个 FinderInfo 清掉后 `--strict` 即通过。
#
# 为什么用 `-c`（清全部可清属性）而不是 `-d com.apple.FinderInfo`：
#   * 实测（本机）：`-c` 会清掉 FinderInfo / quarantine / 自定义属性，**留下 provenance**；
#     它 quiet 且幂等（无可清时 exit 0），适合逐文件批量；
#   * 而 `-d <名字>` 打在**没有该属性**的文件上会 `No such xattr` + **exit 1**，
#     `find … -exec … +` 会对每个干净文件报一次错 —— 构建日志里全是噪声；
#   * 构建产物上没有任何「需要保留的 xattr」，所以「清全部可清属性」在这里是安全的取舍。
#   * **绝对路径 `/usr/bin/xattr`**：PATH 上可能命中 Python 的 `xattr` 包（口径见 README）。
#   * **不吞错误**：失败要留在构建日志里（所以没有 `2>/dev/null`）；`|| true` 只是不让
#     `set -e` 因个别文件失败而中断整个打包。
find "$APP" -exec /usr/bin/xattr -c {} + || true

codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
  && echo "  ✓ 已 ad-hoc 签名（签名前已逐文件清 xattr）" \
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
# 改成两步，先不挂载：
#   1. makehybrid 直接从目录生成 HFS 镜像
#   2. convert 把它压成 UDZO
#
# ⚠️ 但 HFS 这一步会**给镜像里每个文件写 `com.apple.FinderInfo`**，即使源目录
# 一个 xattr 都没有 —— 实测同一份干净源目录：
#     makehybrid -hfs    → 挂载后每个文件都带 FinderInfo，`codesign --verify --strict` 失败
#     ditto -c -k (zip)  → 解出来干净，`--strict` 通过
# 所以「签名前清 xattr」只能保证**构建树里的 .app**干净（CI 校验的是它），
# 用户从 dmg 装出来的 App 仍会带 FinderInfo。要在**交付物**上也干净，必须对
# 可写镜像再清一次：makehybrid(UDRW) → 挂载 RW → 逐文件 xattr -c → 卸载 → 转 UDZO。
DMG_DIR="$RELEASE_DIR/bundle/dmg"
# **先清空。** 这个目录会被 CI 的 cargo 缓存带着跨版本存活，而收集步骤是
# `cp *.dmg *.zip` —— 不清的话上一版的包会被一起打进这一版的 Release。
# 实测 v0.7.8 的 Release 里就混着 0.7.7 的 dmg/zip，而更新器可能下到那个旧的
# （幸好包内版本核对会拒装）。
rm -rf "$DMG_DIR"
mkdir -p "$DMG_DIR"
mkdir -p "$DMG_DIR"
DMG="$DMG_DIR/XrayTun_${APP_VERSION}_$APP_ARCH.dmg"
RAW="$DMG_DIR/.xraytun-raw.dmg"
RW="$DMG_DIR/.xraytun-rw.dmg"
rm -f "$DMG" "$RAW" "$RW"
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

# 造镜像 → 在可写镜像里逐文件清 `com.apple.FinderInfo` → 转 UDZO。
#
# 为什么不能只靠「签名前清一次」：见上面那段 —— makehybrid 的 HFS 文件系统
# 会给镜像内每个文件补 FinderInfo。这里多挂载一次正是为了把它清掉。
# （本脚本后面本来就会挂载镜像做交付前实检，所以挂载不是新增的环境要求；
#   挂不上时退化为旧行为，但在 CI 里按**错误**处理，不再静默发出带 detritus 的 dmg。）
CLEANED=0
if hdiutil makehybrid -quiet -hfs -o "$RAW" -default-volume-name XrayTun "$STAGE"; then
  RMNT="$(mktemp -d)"
  if hdiutil convert -quiet "$RAW" -format UDRW -o "$RW" \
     && hdiutil attach -nobrowse -mountpoint "$RMNT" "$RW" >/dev/null 2>&1; then
    # 逐文件：`-c` 对目录**不递归**，而 `-r` 的支持**随实现与版本而异**
    # （PATH 上先命中的 Python `xattr` 没有 `-r`，`xattr -cr` 会 exit 64），
    # 所以两者都不能依赖 —— 直接用 `find … -exec … +` 逐个文件清。
    # 绝对路径 + 不吞错误：理由同上（见签名前那一段）。
    find "$RMNT" -exec /usr/bin/xattr -c {} + || true
    hdiutil detach "$RMNT" >/dev/null 2>&1 || hdiutil detach "$RMNT" -force >/dev/null 2>&1 || true
    CLEANED=1
  fi
  rmdir "$RMNT" 2>/dev/null || true

  if [ "$CLEANED" != "1" ]; then
    echo "  ⚠ 本环境无法挂载可写镜像 → dmg 内仍会带 com.apple.FinderInfo（用户装出来的 App 严格校验会失败）" >&2
    # **CI 下宁可失败，也不放过。**
    #
    # 这不是「环境挑剔」，而是取舍：清不掉 = **无法验证交付物**；而「无法验证时放行」
    # 正是本项目反复踩的那一类坑（`$arg（` 未定义、`xattr -dr` 是否可用随实现而异、
    # 探针看不见 B1、
    # `codesign --verify` 不带 `--strict`）—— 全是「看起来做了、其实没做」。
    #
    # GitHub 的 macOS runner 有完整的 hdiutil 与挂载能力，所以正常 CI 不会误伤；
    # 万一将来遇到挂载受限的环境，**失败是诚实的输出**：那时应该改流程，
    # 而不是把这条去掉让 CI 变绿。
    if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
      echo "  ✗ CI 下不再静默发出这种 dmg（zip 那条路不受影响）" >&2
      fail=1
    fi
  fi

  SRC_IMG="$RAW"
  [ "$CLEANED" = "1" ] && SRC_IMG="$RW"
  if hdiutil convert -quiet "$SRC_IMG" -format UDZO -o "$DMG"; then
    rm -f "$RAW" "$RW"
    if [ "$CLEANED" = "1" ]; then
      echo "  ✓ 已生成 dmg（含 helper 与 /Applications 快捷方式；已逐文件清 xattr）"
    else
      echo "  ✓ 已生成 dmg（含 helper 与 /Applications 快捷方式；⚠ 未清 xattr）"
    fi
  else
    rm -f "$RAW" "$RW"
    DMG=""
    echo "  ⚠ 打 dmg 失败（本环境可能禁止挂载镜像），.app 仍然可用" >&2
  fi
else
  rm -f "$RAW" "$RW"
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
    # 镜像里的 App 必须是「严格校验能过」的：HFS 会给每个文件补
    # com.apple.FinderInfo，只清构建树里的 .app 是挡不住它的。
    if [ -e "$MP/XrayTun.app" ]; then
      if codesign --verify --strict "$MP/XrayTun.app" >/dev/null 2>&1; then
        echo "  ✓ 镜像内 App 的 codesign --strict 校验通过"
      else
        echo "  ✗ 镜像内 App 未通过 codesign --strict（多半又带上了 com.apple.FinderInfo）" >&2
        codesign --verify --strict --verbose=1 "$MP/XrayTun.app" 2>&1 | head -3 >&2 || true
        fail=1
      fi
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
放行方式**按 macOS 版本分**（详见 README「安装（普通用户）」）：

  * macOS 15+：右键「打开」已被 Apple 移除，去「系统设置 → 隐私与安全性」
    对被拦的 App 点「仍要打开」；
  * macOS 14-：右键（或 Control-点击）→「打开」；
  * 终端（都适用）：逐文件清 quarantine（`xattr` 有两个实现，`-r` 支持随实现/版本而异）：
      find /Applications/XrayTun.app -exec xattr -d com.apple.quarantine {} + 2>/dev/null

架构：App 是 ${APP_ARCH}，核心是 ${CORE_ARCH:-未知}。
两者不一致时（本机就是：x86_64 的 App + arm64 的核心），
App 经由 Rosetta 运行，核心仍是原生的。

要出真正的 universal 包，需要 rustup 装的 rust（而不是 Homebrew
/usr/local 那份），然后设 XRAYTUN_TARGET=universal-apple-darwin 再跑一次；
或者直接打 tag 交给 .github/workflows/release.yml 出包。
EOF
