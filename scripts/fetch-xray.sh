#!/bin/sh
# 下载 Xray-core 到 apps/desktop/binaries/。
#
# 为什么要写成脚本而不是手写下载链接：macOS 上原生 TUN 的**版本下限是
# v26.1.31**（该版本才补齐了地址与路由编程，见 docs/03-xray-integration.md）。
# 低版本会「建了卡但不配置网络」，表现为隧道在、路由对、但完全不通。
# 脚本会在下载后校验版本，不满足就直接失败。
set -eu

VERSION="${XRAY_VERSION:-v26.9.9}"
DEST="$(cd "$(dirname "$0")/.." && pwd)/apps/desktop/binaries"
MIN_VERSION="26.1.31"

case "$(uname -m)" in
  arm64) ASSET="Xray-macos-arm64-v8a.zip" ;;
  x86_64) ASSET="Xray-macos-64.zip" ;;
  *)
    echo "不支持的架构：$(uname -m)" >&2
    exit 1
    ;;
esac

URL="https://github.com/XTLS/Xray-core/releases/download/${VERSION}/${ASSET}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "下载 ${VERSION} / ${ASSET}"
echo "  ${URL}"

if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 -o "$TMP/xray.zip" "$URL"
elif command -v wget >/dev/null 2>&1; then
  wget -O "$TMP/xray.zip" "$URL"
else
  echo "需要 curl 或 wget" >&2
  exit 1
fi

mkdir -p "$DEST"
unzip -o -q "$TMP/xray.zip" -d "$TMP/extracted"

# 官方产物解压后是：xray + geoip.dat + geosite.dat + README.md + LICENSE
#
# geoip.dat / geosite.dat 不是可选的：配置里的 geoip:cn / geosite:cn 会在
# 运行期读它们，缺失时的行为是「规则不命中」——也就是静默地不分流，
# 而日志里没有任何错误。所以必须一起装。
for f in xray geoip.dat geosite.dat; do
  if [ ! -f "$TMP/extracted/$f" ]; then
    echo "产物里缺少 $f —— 上游的打包格式可能变了，请检查" >&2
    exit 1
  fi
  cp "$TMP/extracted/$f" "$DEST/$f"
done

[ -f "$TMP/extracted/LICENSE" ] && cp "$TMP/extracted/LICENSE" "$DEST/LICENSE-xray" || true

chmod +x "$DEST/xray"

# 校验版本下限。
ACTUAL="$("$DEST/xray" version 2>/dev/null | head -1 || echo "")"
echo "已安装：$ACTUAL"

if ! printf '%s' "$ACTUAL" | grep -qE "Xray[[:space:]]+[0-9]+\.[0-9]+\.[0-9]+"; then
  echo "无法解析版本号，请手动确认 >= ${MIN_VERSION}" >&2
  exit 1
fi

FOUND="$(printf '%s' "$ACTUAL" | sed -nE 's/.*Xray[[:space:]]+v?([0-9]+\.[0-9]+\.[0-9]+).*/\1/p')"
if [ -z "$FOUND" ]; then
  echo "无法从「${ACTUAL}」中提取版本号" >&2
  exit 1
fi

# 逐段比较。用 awk 避免依赖 GNU sort -V（macOS 的 BSD sort 不支持 -V）。
if ! awk -v a="$FOUND" -v b="$MIN_VERSION" 'BEGIN {
      n = split(a, A, "."); m = split(b, B, ".");
      for (i = 1; i <= (n > m ? n : m); i++) {
        x = (i <= n) ? A[i] + 0 : 0;
        y = (i <= m) ? B[i] + 0 : 0;
        if (x > y) exit 0;
        if (x < y) exit 1;
      }
      exit 0;
    }'; then
  echo "" >&2
  echo "版本过低：$FOUND < $MIN_VERSION" >&2
  echo "macOS 上低于 $MIN_VERSION 的核心只会创建网卡，不会配置地址与路由，" >&2
  echo "TUN 模式下会表现为「隧道在、路由对、但完全不通」。" >&2
  echo "请设置 XRAY_VERSION 指向更新的版本，例如：" >&2
  echo "  XRAY_VERSION=v26.9.9 $0" >&2
  exit 1
fi

cat <<EOF

完成。产物在 ${DEST}：
$(ls -1 "$DEST" | sed 's/^/  /')

注意：geoip.dat / geosite.dat 必须和 xray 一起被放进 app bundle
（tauri.conf.json 的 bundle.resources），否则 geoip:/geosite: 规则不会命中。
见 docs/07-roadmap-and-risks.md 的「未完成项」第 1 条。
EOF
