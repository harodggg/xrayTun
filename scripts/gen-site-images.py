#!/usr/bin/env python3
"""生成官网的图标与社交卡片（favicon.svg / favicon.ico / apple-touch-icon.png /
icon-*.png / og-image*.png）。

为什么要有这个脚本：这些是**二进制产物**。如果只把 PNG 提交进仓库，下次要改一句话
就得从头画一遍 —— 那正是本项目反复踩过的「只改产物、没有生成器」的坑。
本脚本是唯一产出路径：改文案或配色 → 重跑。

## 为什么用 Pillow 而不是无头浏览器截图

本会话实测：`Google Chrome --headless` 与 macOS `qlmanage -t` 在沙箱里都起不来
（`sandbox initialization failed: Operation not permitted`），
所以走 Pillow 直接绘制。绘制出的图标与 `favicon.svg` **用同一组常量与同一套几何**
（见 below：SVG 也由本脚本生成），因此两者不会漂移。

## 用法

    /usr/local/bin/python3.10 scripts/gen-site-images.py

**为什么要指定解释器**：默认的 `python3`（3.12）没有 Pillow；而本会话的沙箱里
`pip install` 会挂住（PyPI 可达，但 pip 下载卡死）、`Google Chrome --headless`
与 macOS `qlmanage -t` 都报 `sandbox initialization failed: Operation not permitted`。
`/usr/local/bin/python3.10` 自带 Pillow 10.1.0，实测可用
（`/usr/local/bin/python3.11` + `PYTHONPATH=/opt/homebrew/lib/python3.11/site-packages` 也可以）。

依赖只有 Pillow 与系统字体（拉丁用 Arial Bold/Arial，中日韩用 Hiragino Sans GB；
字体找不到会**直接报错**，不会悄悄退化成方框；文案超出画布也会报错退出，不会静默出界）。
"""
import struct
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - 环境问题，给出可执行的修复命令
    sys.exit(
        "✗ 需要 Pillow。本机可直接用自带 Pillow 的解释器：\n"
        "    /usr/local/bin/python3.10 scripts/gen-site-images.py\n"
        "（详见文件头「为什么用 Pillow」与「用法」两节）"
    )

SITE = Path(__file__).resolve().parents[1] / "site"
ASSETS = SITE / "assets"

# 版本号：**必须与 Cargo.toml 的 [workspace.package] version 一致**（scripts/check.sh 会断言）。
# 卡片的**文件名里带版本号**，原因是一次实测事故：og 卡片内容每个版本都会变，而 `_headers`
# 给了长缓存；文件名不变时，改版后 CDN 继续发旧卡片（实测 `cf-cache-status: HIT`、`age: 1674`、
# 旧字节数，图上还印着旧版本号）。用带版本的文件名 = 每次发版换 URL，缓存可以放心长。
SITE_VERSION = "0.8.33"
WASM_VERSION = "0.7.0"

# 配色**逐字取自 site/assets/site.css 的 :root**（改这里等于改官网，不要另起一套）
BG = "#0f1420"
SURFACE_1 = "#161d2c"
BORDER = "#263148"
TEXT = "#e6ecf5"
TEXT_DIM = "#94a3b8"
TEXT_FAINT = "#7b8da6"
ACCENT = "#4f8ef7"
SITE_TEXT = "#ccd6e4"

# 图标几何（64 单位坐标系）—— favicon.svg 与所有 PNG 尺寸共用这一套
ICON_ROUND = 0.22       # 圆角半径 / 边长
ICON_BORDER = 0.031     # 描边宽度 / 边长
ICON_GLYPH_PAD = 0.28   # 字形两端留白 / 边长
ICON_STROKE = 0.125     # 字形笔画宽度 / 边长

FONT_LATIN_BOLD = [
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
]
FONT_LATIN = [
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
]
FONT_CJK = [
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
]

OG_W, OG_H = 1200, 630


def first_existing(paths: list[str], what: str) -> str:
    for p in paths:
        if Path(p).exists():
            return p
    sys.exit(f"✗ 找不到{what}字体（试过）：\n  " + "\n  ".join(paths))


def font(paths: list[str], size: int, what: str):
    return ImageFont.truetype(first_existing(paths, what), size)


def icon_image(size: int) -> Image.Image:
    """圆角方块 + 极简「X」字形（与应用/官网配色一致，不新造视觉）。

    以 4× 绘制再 LANCZOS 缩小 —— Pillow 的图形绘制不走抗锯齿，超采样是小图标
    看起来干净的关键。
    """
    s = size * 4
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    r = int(s * ICON_ROUND)
    d.rounded_rectangle([0, 0, s - 1, s - 1], radius=r, fill=BG)
    bw = max(2, int(s * ICON_BORDER))
    inset = bw / 2
    d.rounded_rectangle(
        [inset, inset, s - 1 - inset, s - 1 - inset],
        radius=max(1, int(r - inset)),
        outline=BORDER,
        width=bw,
    )
    pad = s * ICON_GLYPH_PAD
    lw = int(s * ICON_STROKE)
    a = (pad, pad)
    b = (s - pad, s - pad)
    c = (s - pad, pad)
    e = (pad, s - pad)
    d.line([a, b], fill=ACCENT, width=lw)
    d.line([c, e], fill=ACCENT, width=lw)
    # 圆头：Pillow 的 line 没有 round cap，用圆点补上
    for (x, y) in (a, b, c, e):
        d.ellipse([x - lw / 2, y - lw / 2, x + lw / 2, y + lw / 2], fill=ACCENT)
    return img.resize((size, size), Image.LANCZOS)


def icon_svg() -> str:
    """favicon.svg —— 与 icon_image() 同一套几何（64 单位坐标系）。"""
    r = 64 * ICON_ROUND
    bw = 64 * ICON_BORDER
    pad = 64 * ICON_GLYPH_PAD
    lw = 64 * ICON_STROKE
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="XrayTun">\n'
        "  <title>XrayTun</title>\n"
        f'  <rect width="64" height="64" rx="{r:g}" fill="{BG}"/>\n'
        f'  <rect x="{bw / 2:g}" y="{bw / 2:g}" width="{64 - bw:g}" height="{64 - bw:g}" '
        f'rx="{r - bw / 2:g}" fill="none" stroke="{BORDER}" stroke-width="{bw:g}"/>\n'
        f'  <path d="M{pad:g} {pad:g} L{64 - pad:g} {64 - pad:g} M{64 - pad:g} {pad:g} '
        f'L{pad:g} {64 - pad:g}" stroke="{ACCENT}" stroke-width="{lw:g}" stroke-linecap="round"/>\n'
        "</svg>\n"
    )


def fit(d: ImageDraw.ImageDraw, s: str, f, max_w: float, where: str) -> float:
    """文本框宽度超过可用宽度就**报错退出** —— 宁可生成失败，也不要出界/被裁的卡片。"""
    w = d.textlength(s, font=f)
    if w > max_w:
        sys.exit(f"✗ 文案过宽（{where}）：{w:.0f}px > 可用 {max_w:.0f}px —— 缩短文案或调小字号：{s!r}")
    return w


CARDS = [
    {
        "out": f"og-image-{SITE_VERSION}.png",
        "fonts": "cjk",
        "brand": "XrayTun",
        "title": "macOS 上的 Xray 图形客户端",
        "subtitle": "原生 TUN 模式接管系统流量",
        "chips": ["macOS 13.0+", "通用包 arm64 + x86_64", "包内自带 Xray 核心", "v0.8.33"],
        "note": None,
    },
    {
        "out": f"og-image-en-{SITE_VERSION}.png",
        "fonts": "latin",
        "brand": "XrayTun",
        "title": "An Xray GUI client for macOS",
        "subtitle": "Native TUN mode takes over system traffic",
        "chips": ["macOS 13.0+", "Universal arm64 + x86_64", "Xray core included", "v0.8.33"],
        "note": None,
    },
    {
        "out": f"og-image-wasm-{WASM_VERSION}.png",
        "fonts": "cjk",
        "brand": "xray-wasm",
        "title": "纯 Rust 的 Xray 协议栈",
        "subtitle": "VLESS + XTLS-Vision + REALITY → wasm32-wasip2",
        "chips": ["wasmtime", "真实 TCP 代理", "v0.7.0"],
        "note": "同一作者的独立项目 —— XrayTun 不使用 xray-wasm",
    },
    {
        "out": f"og-image-wasm-en-{WASM_VERSION}.png",
        "fonts": "latin",
        "brand": "xray-wasm",
        "title": "Xray's protocol stack in pure Rust",
        "subtitle": "VLESS + XTLS-Vision + REALITY compiled to wasm32-wasip2",
        "chips": ["wasmtime", "real TCP proxy", "v0.7.0"],
        "note": "A separate project by the same author — XrayTun does not use xray-wasm",
    },
]


def og_image(spec: dict) -> Image.Image:
    img = Image.new("RGB", (OG_W, OG_H), BG)
    d = ImageDraw.Draw(img)
    if spec["fonts"] == "cjk":
        f_brand, f_title, f_sub, f_chip, f_note = (
            font(FONT_CJK, 34, "品牌"),
            font(FONT_CJK, 62, "标题"),
            font(FONT_CJK, 32, "副标题"),
            font(FONT_CJK, 24, "标签"),
            font(FONT_CJK, 24, "脚注"),
        )
    else:
        f_brand, f_title, f_sub, f_chip, f_note = (
            font(FONT_LATIN_BOLD, 34, "品牌"),
            font(FONT_LATIN_BOLD, 62, "标题"),
            font(FONT_LATIN, 32, "副标题"),
            font(FONT_LATIN, 24, "标签"),
            font(FONT_LATIN, 24, "脚注"),
        )

    margin = 72
    avail = OG_W - 2 * margin

    # 顶部 6px 强调线（站点主色，极简）
    d.rectangle([0, 0, OG_W, 6], fill=ACCENT)

    # 品牌行：图标 + 名称
    icon = icon_image(72)
    img.paste(icon, (margin, 64), icon)
    d.text((margin + 72 + 22, 64 + 36), spec["brand"], font=f_brand, fill=TEXT, anchor="lm")

    # 标题 / 副标题
    fit(d, spec["title"], f_title, avail, "标题")
    fit(d, spec["subtitle"], f_sub, avail, "副标题")
    d.text((margin, 248), spec["title"], font=f_title, fill=TEXT, anchor="lm")
    d.text((margin, 330), spec["subtitle"], font=f_sub, fill=SITE_TEXT, anchor="lm")

    # 标签行（每个标签一个描边胶囊）
    x, y, h = margin, 452, 52
    for c in spec["chips"]:
        w = d.textlength(c, font=f_chip)
        d.rounded_rectangle([x, y, x + w + 44, y + h], radius=h // 2, outline=BORDER, width=2)
        d.text((x + 22, y + h // 2), c, font=f_chip, fill=TEXT_DIM, anchor="lm")
        x += w + 44 + 16
    if x - 16 > OG_W - margin:
        sys.exit(f"✗ 标签行过宽（{spec['out']}）：{x - 16:.0f}px > 右边距 {OG_W - margin}px")

    # 脚注：wasm 卡片必须写明「与 XrayTun 的关系」（红线），其他卡片没有脚注
    if spec["note"]:
        fit(d, spec["note"], f_note, avail, "脚注")
        d.text((margin, 556), spec["note"], font=f_note, fill=TEXT_FAINT, anchor="lm")
    return img


def ico_bytes(pngs: dict[int, bytes]) -> bytes:
    """把若干 PNG 打包成 .ico（Vista+ 支持内嵌 PNG，无需 BMP 转换）。"""
    sizes = sorted(pngs)
    header = struct.pack("<HHH", 0, 1, len(sizes))
    entries, blobs, offset = b"", b"", 6 + 16 * len(sizes)
    for s in sizes:
        data = pngs[s]
        entries += struct.pack(
            "<BBBBHHII", s if s < 256 else 0, s if s < 256 else 0, 0, 0, 1, 32, len(data), offset
        )
        blobs += data
        offset += len(data)
    return header + entries + blobs


def png_bytes(img: Image.Image) -> bytes:
    from io import BytesIO

    buf = BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def main() -> int:
    ASSETS.mkdir(parents=True, exist_ok=True)
    written: list[str] = []

    # ---- 图标：SVG（文本）+ 各尺寸 PNG + 打包成 ICO ----
    (SITE / "favicon.svg").write_text(icon_svg(), encoding="utf-8")
    written.append("favicon.svg")

    (SITE / "favicon.ico").write_bytes(
        ico_bytes({s: png_bytes(icon_image(s)) for s in (16, 32, 48)})
    )
    written.append("favicon.ico")

    icon_image(180).save(SITE / "apple-touch-icon.png")
    written.append("apple-touch-icon.png")
    icon_image(192).save(ASSETS / "icon-192.png")
    icon_image(512).save(ASSETS / "icon-512.png")
    written += ["assets/icon-192.png", "assets/icon-512.png"]

    # ---- 社交卡片 ----
    for spec in CARDS:
        img = og_image(spec)
        img.save(SITE / spec["out"])
        written.append(spec["out"])

    print("生成官网图标与社交卡片：")
    for w in written:
        p = SITE / w
        print(f"  {w:26s} {p.stat().st_size:>8,d} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
