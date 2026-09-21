#!/usr/bin/env python3
"""检查「被引用但未定义」的 CSS 自定义属性（token）。

# 为什么需要这道检查

这是本项目**第三次**出现「**静默失效的声明**」：

1. `--line` **从未定义** → 拓扑页那条分隔线 `border-top: 1px dashed var(--line)`
   **从 v0.8.24 起一直没渲染**（task-12 发现，`e7ed509` 修）；
2. `xattr -dr com.apple.quarantine` **必然失败**，而 `2>/dev/null` 把失败证据也吞了
   （task-46 修）—— 同族问题的另一种形态：**失败不报错**；
3. `--border-interactive` 在**应用侧未定义** → `border-color` 在计算值阶段失效 →
   落到 `currentColor` → 选中态多了一圈计划外的**近白描边**（task-57 发现，task-61 修）。

三次的共同点：**没有任何检查会发现它**。浏览器不报错、样式表不报错、`tsc` 不报错，
只有人拿放大镜量计算样式才看得出来。

（CSS 规范把这种情况叫 *invalid at computed-value time*：`var()` 引用一个没有值的自定义属性、
且**没有兜底值**时，整条声明在计算值阶段失效 —— 对简写属性是整个简写被丢弃，
对 `border-color` 这类长写是落到 initial，而 `border-color` 的 initial 是 `currentColor`。
所以症状各不相同：**有的什么都没发生，有的变出个你没要的颜色**。）

# 作用域是**按 bundle** 的，不是全仓库

这是本检查最关键、也最容易做错的一点：

**`--border-interactive` 在整个仓库里「搜得到」** —— 它在 `site/assets/site.css` 里有定义，
那是**官网**的 token 表。但应用不加载那个文件，所以对应用而言它**就是未定义的**。

⇒ 如果用一个「全仓库定义集合」去判，这条**永远测不出来**（我就先踩了这个坑：
第一版原型用全局集合跑，结论是「0 违规」，而当时真 bug 就在文件里）。

所以按 bundle 分别判定：每个 bundle 的引用只能由**同一个 bundle 内**的定义满足。
这道检查要防的正是「**跨 bundle 泄漏**」：一个名字在别处有定义，在自己这儿没有。

# 边界（已知局限，故意不处理）

* **只有在 `@media` 里定义**的 token 会被算作已定义。严格说，那种 token 在媒体条件不成立时
  也是「未定义」—— 但要做对必须逐引用求值媒体条件，成本远高于收益。
  当前仓库没有这种 token（已核）；若将来出现，应单独处理而不是让本检查变复杂。
* **字符串里的 `var(--x)`**（如 `content: "var(--x)"`）会被当成引用。CSS 里那是普通字符串、
  不会做替换，所以是**误报**。当前仓库没有这种写法（已核）。
* 不检查「定义了但没人用」—— 未使用的 token 是合法的（前向预留）。

用法：
    ./scripts/check-css-tokens.py          # 在仓库根跑
退出码：0 = 全部有定义；1 = 存在违规。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 每个 bundle：`defs` 是**能提供定义**的样式表；`refs` 是**可能出现引用**的文件。
# `refs` 比 `defs` 宽：TSX 的内联样式（`style={{ color: "var(--danger)" }}`）同样是引用，
# 而它引用的 token 只能由本 bundle 的 CSS 提供。
BUNDLES = [
    {
        "name": "应用 UI",
        "defs": ["apps/ui/src/**/*.css"],
        # 排除 *.test.*：测试里会写 `var\((--[\w-]+)\)` 这类**正则/字符串**来解析样式表，
        # 那不是真的引用，算进来就是误报。
        "refs": [
            "apps/ui/src/**/*.css",
            "apps/ui/src/**/*.tsx",
            "apps/ui/src/**/*.ts",
        ],
        "refs_exclude": ["**/*.test.tsx", "**/*.test.ts"],
    },
    {
        "name": "官网",
        "defs": ["site/assets/**/*.css"],
        # 官网的 HTML 目前没有内联 var()（已核：`grep -c 'var(--' site/index.html site/en/index.html`
        # 均为 0）；以后要是有了，把 `site/**/*.html` 加进来即可。
        "refs": ["site/assets/**/*.css"],
        "refs_exclude": [],
    },
]

# 行注释：剥掉 `//` 之后的内容，但**不能碰 `://`**（URL 里的 `//`）。
_TX_LINE_COMMENT = re.compile(r"(?<!:)//[^\n]*")
_BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.S)

# 定义：`--name: value`（任何位置，包括媒体查询内）。
# 也接受 `@property --name {`（自定义属性注册也是定义）—— 当前仓库没用，但便宜。
_DEF = re.compile(r"(--[\w-]+)\s*:|@property\s+(--[\w-]+)")

# 引用：`var(--name` 后面紧跟 `,` 或 `)`。
#   `var(--x, fallback)` ⇒ 有兜底 ⇒ **合法**，不算违规（`--panel` 就是这种，刻意保留）。
#   `var(--x)`           ⇒ 必须有定义。
# 注意不能用「第一个逗号」来判兜底：`var(--x, rgb(1,2,3))` 的兜底里也有逗号。
# 紧跟名字后的那个字符就足以判定 —— 这是 CSS 语法决定的（`var( <name> , <value>? )`）。
_REF = re.compile(r"var\(\s*(--[\w-]+)\s*([,)])")


def strip_comments(text: str, *, line_comments: bool) -> str:
    """剥注释但**保留行号**：把注释内容替换成等量空白（换行保留）。

    行号必须准确 —— 报错要给人 `文件:行`，差几行就没人能一眼找到。
    """
    def blank(m: re.Match[str]) -> str:
        return re.sub(r"[^\n]", " ", m.group(0))

    out = _BLOCK_COMMENT.sub(blank, text)
    if line_comments:
        out = _TX_LINE_COMMENT.sub(blank, out)
    return out


def expand(patterns: list[str], exclude: list[str]) -> list[Path]:
    files: set[Path] = set()
    for pat in patterns:
        files |= {p for p in ROOT.glob(pat) if p.is_file()}
    for pat in exclude:
        files -= {p for p in ROOT.glob(pat)}
    return sorted(files)


def definitions(paths: list[Path]) -> set[str]:
    found: set[str] = set()
    for p in paths:
        text = strip_comments(p.read_text(encoding="utf-8"), line_comments=False)
        for m in _DEF.finditer(text):
            found.add(m.group(1) or m.group(2))
    return found


def references(paths: list[Path]) -> list[tuple[Path, int, str, bool]]:
    out: list[tuple[Path, int, str, bool]] = []
    for p in paths:
        is_style = p.suffix == ".css"
        text = strip_comments(p.read_text(encoding="utf-8"), line_comments=not is_style)
        for m in _REF.finditer(text):
            line = text[: m.start()].count("\n") + 1
            out.append((p, line, m.group(1), m.group(2) == ","))
    return out


def main() -> int:
    print()
    print("==============================================================")
    print("  CSS token 定义性（被 var() 引用但未定义）")
    print("==============================================================")

    problems: list[str] = []
    for bundle in BUNDLES:
        def_files = expand(bundle["defs"], [])
        ref_files = expand(bundle["refs"], bundle["refs_exclude"])
        defined = definitions(def_files)
        refs = references(ref_files)
        hard = [r for r in refs if not r[3]]
        soft = len(refs) - len(hard)

        bad = [(p, ln, name) for p, ln, name, _ in hard if name not in defined]
        rel = lambda p: p.relative_to(ROOT)  # noqa: E731

        print(
            f"  {bundle['name']}：{len(def_files)} 个样式表定义 {len(defined)} 个 token；"
            f"{len(refs)} 处 var() 引用（其中 {soft} 处带兜底）"
        )
        for p, ln, name in bad:
            print(f"      ✗ {rel(p)}:{ln}: var({name}) —— 无兜底且本 bundle 未定义", file=sys.stderr)
            problems.append(f"{rel(p)}:{ln}: var({name})")

    if problems:
        print(file=sys.stderr)
        print(f"  ✗ 发现 {len(problems)} 处「无兜底且未定义」的 token 引用：", file=sys.stderr)
        for line in problems:
            print(f"      {line}", file=sys.stderr)
        print(file=sys.stderr)
        print("    这类声明的症状是**静默**的：浏览器不报错，但整条声明在计算值阶段失效 ——", file=sys.stderr)
        print("      · 简写属性（如 `border-top: 1px dashed var(--line)`）⇒ 整条被丢弃；", file=sys.stderr)
        print("      · `border-color` 这类长写 ⇒ 落到 initial，而 border-color 的 initial 是", file=sys.stderr)
        print("        `currentColor` ⇒ **变出一圈你没要的描边**。", file=sys.stderr)
        print("    本项目已被这一类咬过三次（--line / xattr -dr / --border-interactive）。", file=sys.stderr)
        print(file=sys.stderr)
        print("    修法二选一：① 删掉这条声明；② 在**本 bundle 的**样式表里定义这个 token", file=sys.stderr)
        print("    （注意：在另一个 bundle 里定义**不算** —— 那正是 --border-interactive 的坑）。", file=sys.stderr)
        print("    若确实想容忍缺失，写成 `var(--x, <兜底值>)`。", file=sys.stderr)
        return 1

    print("  ✓ 所有无兜底的 var() 引用，都能在本 bundle 内找到定义")
    return 0


if __name__ == "__main__":
    sys.exit(main())
