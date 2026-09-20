# xray-tun 设计系统：token 收敛与跨页一致性

> **任务**：task-12　**作者**：art-designer　**唯一写入文件**：本文件（不改任何源码）
> **基线**：`apps/ui/src/styles.css` **2408 行**，工作区与 `HEAD` **干净**
> （`git diff --stat HEAD -- apps/ui/src/styles.css` 输出为空，因此本文每个行号都能在当前版本直接复现）
> **审计日期**：2026-09-20　**审计机型**：macOS，headless Chrome 153.0.8010.50

---

## 0. 复现方式（本文所有数字的来源）

```bash
# 1) 起预览（不要占 5173，那里可能有人在跑）
cd apps/ui && npx vite --port 5199 --strictPort
# 页面：http://localhost:5199/?preview=1&state=connected&view=<dashboard|nodes|
#        subscriptions|routing|topology|globe|logs|settings>

# 2) headless Chrome + CDP（profile 必须在 /tmp，仓库内曾被 188MB profile 污染）
HOME=/tmp/xraytun-home "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --remote-debugging-port=9333 --remote-allow-origins='*' \
  --user-data-dir=/tmp/xraytun-chrome --crash-dumps-dir=/tmp/xraytun-crash \
  --no-sandbox --disable-gpu --disable-breakpad --window-size=1080,720 \
  --force-device-scale-factor=1 about:blank
# 视口用 CDP Emulation.setDeviceMetricsOverride(1080x720) 对齐 tauri.conf.json
# （1080 x 720，apps/desktop/tauri.conf.json:17-18）
```

> **两个环境注意点**（踩过，记下来给下一个人）：
> 1. DSH 文件沙箱会拦住 Chrome 自己的 sandbox（`sandbox initialization failed: Operation not permitted`），
>    必须 `--no-sandbox`，并且把 `HOME` 指到 `/tmp` 否则 crashpad 写 `~/Library` 被拒。
> 2. CDP 必须连**page target**（`/json/list` 里 `type == "page"` 的 `webSocketDebuggerUrl`），
>    连 browser target 调 `Page.enable` 会报 `-32601`。
>
> 探针脚本在 `/tmp/xraytun-audit/`（`cdp.mjs` 采计算样式 + 截图、`contrast.mjs` WCAG、`report.py` 跨页对比），
> **不在仓库里**，因为本任务只能写这一个文件。

---

## 1. 现状审计（全部实测）

### 1.1 `:root` token 全表 + 实际使用次数

`:root` 在第 **1–18 行**，共定义 **16 个 token**。全文 `var()` 引用 **269 处**（不含下面 2 个未定义引用）。

| # | token | 值 | 定义行 | **实际使用次数** | 用途 |
|---|---|---|---|---|---|
| 1 | `--bg` | `#0f1420` | 2 | **2** | 窗口底色 |
| 2 | `--bg-elevated` | `#161d2c` | 3 | **19** | 一级表面 |
| 3 | `--bg-input` | `#1c2436` | 4 | **4** | 输入/按钮填充 |
| 4 | `--border` | `#263148` | 5 | **31** | 弱边界/分隔线 |
| 5 | `--border-strong` | `#334155` | 6 | **10** | 强边界 |
| 6 | `--text` | `#e6ecf5` | 7 | **17** | 主文字 |
| 7 | `--text-dim` | `#94a3b8` | 8 | **34** | 次要文字 |
| 8 | `--text-faint` | `#64748b` | 9 | **53** | 三级文字 ← **用得最多的 token** |
| 9 | `--accent` | `#4f8ef7` | 10 | **21** | 强调/焦点 |
| 10 | `--accent-hover` | `#6ba0ff` | 11 | **2** | 强调 hover |
| 11 | `--ok` | `#34d399` | 12 | **11** | 成功 |
| 12 | `--warn` | `#fbbf24` | 13 | **14** | 警告 |
| 13 | `--danger` | `#f87171` | 14 | **14** | 危险 |
| 14 | `--radius` | `10px` | 15 | **13** | 大圆角 |
| 15 | `--radius-sm` | `6px` | 16 | **15** | 小圆角 |
| 16 | `--mono` | `ui-monospace, …` | 17 | **9** | 等宽字体栈 |

**统计口径**：用 `var\(\s*--name\s*[,)]` 做**精确**匹配。
这一条很重要 —— 用宽松的 `var\(--text\b` 会把 `var(--text-dim)` / `var(--text-faint)` 也算进去，
得到 `--text` = 104 次这种**错误**数字（我第一次就踩了这个坑）。同一原因，
`--border` 宽松匹配是 41 次，精确匹配是 **31** 次；`--bg` 宽松 25 次，精确 **2** 次。

### 1.2 未使用的 token —— 一个都没有；但有两个**用了却没定义**的 token（真 bug）

**没有 `uses == 0` 的 token。** 但有 3 个「近乎死掉」（≤4 次）：`--bg`(2)、`--accent-hover`(2)、`--bg-input`(4)。
`--bg` 只用在 `body` 背景和滚动条描边各一次 —— 也就是说**整个应用的表面色几乎不用 `--bg`**，
这是后面「14 种深色底」问题的直接后果。

真正的问题在反方向，**两个 `var()` 指向了不存在的 token**：

| 引用处 | 写法 | 后果（实测） |
|---|---|---|
| **第 1632 行** | `border-top: 1px dashed var(--line)` | `--line` **全文未定义**（`grep '--line\s*:' apps/ui/src` 和 `docs/` 都为空）→ 整个 `border-top` 简写 **invalid at computed-value time**，被整条丢弃。CDP 实测 `.highway__internal` 的 `borderTopStyle: "none"`、`borderTopWidth: "0px"`。**注释里说「分隔线用 border-top 而不是额外元素」，但这条线根本没画出来。** |
| **第 619 行** | `background: var(--panel, #131a29)` | `--panel` 全文未定义 → **永远**走 fallback `#131a29`。弹窗底色是一个硬编码值伪装成 token。 |

> `--line` 的失效是**静默**的：浏览器不报错，样式表不报错，只有量 `borderTopStyle` 才看得出来。
> 这是本次审计里唯一一条「说了要做、实际没做」的视觉缺陷。

### 1.3 硬编码值

#### (a) 硬编码**颜色**：30 处、22 个不同值（已排除注释）

先说一条**与任务卡预设相反**的事实：**颜色基本已经 token 化了**。
任务卡举例的 `#1a2332` 在文件里**不存在**（最接近的是 `#1a2333`，出现 1 次）。
所以「硬编码颜色」不是这个文件的主要问题 —— 主要问题是**间距和字号**（见 b/c）。

不过有 22 个硬编码色，它们**不是随手写的，而是 4 类语义色被拆散了**：

| 次数 | 值 | 行号 | 它「应该」是什么 |
|---|---|---|---|
| 3 | `#0a0e16` | 520, 1020, 1277 | 凹槽底（日志正文 / 代码块 / 通用） |
| 3 | `#5b2b33` | 221, 461, 503 | `--danger` 的 border tint |
| 2 | `#06101f` | 211, 259 | 强调色上的**文字**（primary 按钮 / 选中段） |
| 2 | `#1b2740` | 105, 399 | 选中态表面（导航 / 列表行） |
| 2 | `#57471f` | 455, 497 | `--warn` 的 border tint |
| 2 | `#9ecbff` | 1753, 1866 | 连接高亮描边 |
| 1 | `#0b101a` | 57 | 侧栏底 |
| 1 | `#1a2333` | 395 | 列表行 hover |
| 1 | `#141c2b` | 100 | 导航 hover |
| 1 | `#131a29` | 619 | 弹窗底（假 token，见 1.2） |
| 1 | `#131f30` | 510 | `--info` 的 bg tint |
| 1 | `#cfe1f8` | 511 | `--info` 的 text tint |
| 1 | `#241f10` | 498 | `--warn` 的 bg tint |
| 1 | `#f6e3b4` | 499 | `--warn` 的 text tint |
| 1 | `#2a161a` | 504 | `--danger` 的 bg tint |
| 1 | `#f8c9cd` | 505 | `--danger` 的 text tint |
| 1 | `#1f5545` | 450 | `--ok` 的 border tint |
| 1 | `#26456b` | 509 | `--info` 的 border tint |
| 1 | `#2a3548` | 596 | 滚动条滑块 |
| 1 | `#1b2a42` | 1755 | 焦点卡片底 |
| 1 | `#cfe6ff` | 1762 | 高亮路径描边 |
| 1 | `#ffffff` | 640 | 二维码底（**有意为之**，深色底扫不出码，必须保留） |

**结构性发现**：`--ok / --warn / --danger` 三个语义色**只有主色是 token**，
它们的 **border / bg / text 三件套全是硬编码**，而且散落在 §「徽标」(434–480) 与 §「提示条」(481–520) 两处：

```
.badge--fast   { color: var(--ok);     border-color: #1f5545 }   ← 没有 ok 的 bg tint
.badge--ok     { color: var(--warn);   border-color: #57471f }   ← 命名是 --ok 却是 warn 色
.badge--slow,
.badge--unknown{ color: var(--danger); border-color: #5b2b33 }
.banner--warn  { border-color: #57471f; background: #241f10; color: #f6e3b4 }
.banner--error { border-color: #5b2b33; background: #2a161a; color: #f8c9cd }
.banner--info  { border-color: #26456b; background: #131f30; color: #cfe1f8 }
```

⚠️ 顺带一个**语义 bug**：第 455 行 `.badge--ok` 的名字是「ok」但用的是 `--warn` 色。
（不在本任务写入范围，报给 lead。）

#### (b) 硬编码 **px**：**478 处、68 个不同值** ← 这才是主要问题

`px` 字面量出现最多的前 20 个：

| 次数 | 值 | 主要出现在 |
|---|---|---|
| 49 | `12px` | font-size(22) / padding / gap / radius |
| 42 | `1px` | border 宽度 |
| 35 | `2px` | border / padding / gap |
| 34 | `8px` | gap(15) / padding |
| 34 | `10px` | padding(13) / gap(8) |
| 30 | `11px` | font-size(27) |
| 28 | `6px` | gap(7) / padding(5) / radius |
| 22 | `10.5px` | font-size(22) |
| 21 | `14px` | font-size(6) / padding(5) / margin(6) |
| 15 | `11.5px` | font-size(15) |
| 13 | `5px` | padding(5) / border-radius(2) / margin-left(1) |
| 12 | `4px` | gap(4) / padding / margin |
| 11 | `7px` | padding(5) / gap(2) |
| 8 | `3px` | border-radius(2) / gap(2) |
| 7 | `18px` | padding(前 3 个是 `.content`/`.modal__box`, 其余) |
| 6 | `13px` | font-size(5) |
| 6 | `16px` | padding(3) / gap / font-size |
| 5 | `15px` | font-size(5) |
| 5 | `12.5px` | font-size(5) |
| 4 | `20px` | padding / font-size(1) |

按「属性 → 值」拆开看更清楚：

- **`font-size`：113 条声明、11 个不同值** → 见 1.4(d)
- **`gap`：64 条声明、20 个不同值**：8px(15) 10px(8) 6px(7) 2px(5) 12px(5) 4px(4) 5px(4) 3px(2) 14px(2) 7px(2) 9px(1) 22px(1) 26px(1) 28px(1) 30px(1) 34px(1) 1px(1) …
- **`margin-top/bottom`**：14 个不同值，其中 `6px` 出现 10 次、`14px` 6 次、`2px` 5 次
- **`padding`（整条简写）**：`0 8px`(4) `10px 12px`(4) `7px 10px`(3) `12px 14px`(3) `6px 10px`(2) … 共 40+ 条唯一声明
- **`border-radius`**：13 次 `var(--radius)` + 13 次 `var(--radius-sm)`，其余 11 处硬编码（见 1.4 e）

#### (c) 任务卡预设的两处需要更正

| 任务卡说 | 实测 |
|---|---|
| 硬编码「`#1a2332`」 | **不存在**。硬编码色共 22 个不同值、30 处；最接近的是 `#1a2333`（1 次） |
| 硬编码「`13px`」 | 存在但只 **6 次**（其中 5 次是 `font-size`）。真正高频的是 `12px`(49) / `11px`(30) / `10.5px`(22) |

### 1.4 近义重复（两个几乎一样的灰 / 差 1px 的字号）

#### (a) 深色底：**14 种**，并且挤在极窄的亮度带里

全部「表面 / 底色」按 WCAG 相对亮度排序（L 越小越黑）：

| 色值 | 身份 | L | 定义处 |
|---|---|---|---|
| `#0a0e16` | 凹槽（日志/代码） | 0.0044 | 硬编码 520/1020/1277 |
| `#0b101a` | 侧栏 | 0.0052 | 硬编码 57 |
| `#0f1420` | `--bg` | 0.0071 | token |
| `#131a29` | 弹窗（死 token fallback） | 0.0104 | 硬编码 619 |
| `#141c2b` | 导航 hover | 0.0115 | 硬编码 100 |
| `#161d2c` | `--bg-elevated` | 0.0123 | token |
| `#1a2333` | 列表行 hover | 0.0166 | 硬编码 395 |
| `#1c2436` | `--bg-input` | 0.0177 | token |
| `#1b2740` | 导航/列表选中 | 0.0205 | 硬编码 105/399 |
| `#1b2a42` | 焦点卡片底 | 0.0228 | 硬编码 1755 |
| `#263148` | `--border`（**也被当背景用**） | 0.0308 | token |
| `#2a3548` | 滚动条 | 0.0351 | 硬编码 596 |
| `#334155` | `--border-strong` | 0.0514 | token |

**14 个色的亮度范围只有 0.0044–0.0228**（不含最后两个当边界用的）。其中距离近到**肉眼不可分**的：

| 一对 | RGB 欧氏距离 | 说明 |
|---|---|---|
| `--bg-elevated #161d2c` ↔ `.nav-item:hover #141c2b` | **2.4** | 差 2 个灰阶 —— 几乎是同一个色 |
| `#131a29`（弹窗） ↔ `.nav-item:hover #141c2b` | **3.0** | 同上 |
| `--bg-input #1c2436` ↔ `.list__row:hover #1a2333` | **3.7** | 同上 |
| `.nav-item.is-active #1b2740` ↔ `.highway__lane-label--match #1b2a42` | **3.6** | 两个「选中态」用了两个色 |
| 侧栏 `#0b101a` ↔ 日志 `#0a0e16` | **4.6** | 同样是「比窗口更深的凹槽」，用了两个色 |

**结论**：这 14 个色里有 **9 个可以用 4 个 token 表达**（背景 3 档 + 交互态 1–2 档）。

#### (b) 边框色被当背景用（token 语义错位）

第 **1438 行**：`.usage__bar { background: var(--border) }`。
`.usage-bar`（订阅用量条）的**轨道**用了「边界」token。CDP 实测订阅页 `.usage__bar` 的
`background-color: rgb(38, 49, 72)` = `#263148` = `--border`。
这样一旦有人调 `--border`（比如为了可访问性提高边界对比度），订阅页的进度条会跟着变 —— 语义串了。

#### (c) 圆角：7 个不同值，其中 3 个是「差 1px」的

| 次数 | 值 | 身份 |
|---|---|---|
| 13 | `var(--radius)` = `10px` | 容器 |
| 13 | `var(--radius-sm)` = `6px` | 控件 |
| 3 | `999px` | 胶囊（badge / count） |
| 2 | `50%` | 圆点 |
| 2 | **`5px`** | 第 251 行 `.segmented button.is-active`、第 597 行滚动条 |
| 2 | **`3px`** | 第 1437 行 `.usage__bar`、第 1832 行 `.conn-note` |
| 1 | **`1px`** | 第 1675 行 `.highway__legend-dot` |
| 1 | `0 var(--radius-sm) var(--radius-sm) 0` | `.note` 左侧竖条 |

⚠️ **`.segmented` 的坑**：容器 `.segmented` 用 `border-radius: var(--radius-sm)` = **6px**（第 234 行），
里面选中的按钮 `.segmented button` 用 `border-radius: 5px`（第 251 行）。
**内圆角比外圆角小 1px**，而正确做法是「内圆角 = 外圆角 − 内边距」（6 − 2 = 4px）或直接相等。
这不是审美问题，是渲染上能看出「边不贴合」。

#### (d) 字号：**CSS 里 11 个不同值**，且 5 个挤在 1.5px 之内

`font-size` 共 **113 条声明**：

| 声明数 | 值 | 渲染页数（8 页中的几页在用） | 渲染元素数 |
|---|---|---|---|
| 4 | `10px` | 8/8 | 12 |
| 22 | `10.5px` | 4/8 | 213 |
| 27 | `11px` | 8/8 | 151 |
| 15 | `11.5px` | 7/8 | 572 |
| 22 | `12px` | 7/8 | 137 |
| 5 | `12.5px` | 3/8 | 6 |
| 5 | `13px` | 8/8 | 506 |
| 6 | `14px` | 8/8 | 28 |
| 5 | `15px` | 8/8 | 20 |
| 1 | `20px` | 1/8（第 360 行 `.stat__value`） | 4 |
| 1 | `21px` | 1/8（第 755 行 `.dash__state`） | 4 |

**问题一：`10.5 / 11 / 11.5 / 12 / 12.5` 五档挤在 1.5px 内**，相邻只差 0.5px。
在 macOS 上 `10.5px` 与 `11px` 的渲染差异小于抗锯齿噪声 —— 用户看不出区别，维护者却要每次决定选哪个。
（这也解释了为什么 `11.5px` 渲染得最多（572 个元素）而 CSS 里只写了 15 次：它是层层叠加的默认值。）

**问题二：`15px` vs `14px` 是同一角色用了两档。**
第 360 行 `.stat__value` 用 20px、第 755 行 `.dash__state` 用 21px —— **同一页（仪表盘）里两个「主数字」差 1px**，
且各自只有 1 条声明。而 24 行 `.sidebar__brand` 用 15px，1172 行 `.page__title` 用 14px —— 品牌名和页面标题差 1px。

### 1.5 「同名规则块重复」核实 —— 三条**不是**重复，一条是；另外还有 **190 行整段重复**

任务卡给的四个数字（`.logs-page` 10、`.page__details` 7、`.highway__side--right` 7、`.chain__row` 6）
是**「包含该字符串的行数」**，不是「重复的规则块数」。逐条核实：

| 选择器 | 行数 | **真实情况** | 判定 |
|---|---|---|---|
| `.logs-page` | 10 | 第 934 行 1 个基块 + 第 1027/1040/1044/1054/1061/1066/1070/1074/1078 行 **9 个后代/上下文选择器**（`.logs-page .logs`、`.logs-page .log-line__ts` …） | **不是重复**（是作用域限定）。但**过度限定**：这些类名（`.log-line__ts` 等）只出现在日志页，加 `.logs-page` 前缀白白抬高特异性，导致要覆盖它必须写更长选择器 |
| `.page__details` | 7 | 第 1219 行 1 个基块 + 1224/1236/1240/1247/1251/1255 行 **6 个 `>` / `::` / `[open]` 变体** | **不是重复** |
| `.highway__side--right` | 7 | 第 1546 行 1 个基块 + 1571/1599/1602/1605/1608–1609 行 6 个后代选择器；另在 `@media (max-width: 880px)`（2042–2071）里出现 5 处 | **不是重复**，但有两处可合并（见下） |
| `.chain__row` | 6 | 第 **1957/1967/1971** 行与第 **2187/2197/2201** 行 —— **逐字节相同** | ✅ **是真重复** |

#### 唯一被证实为真重复的整段：**190 行**

我用「解析全部 336 个顶层规则块 + 比对（选择器, 声明体）」+「逐行最长公共子串」两种方法交叉验证：

| 原文行范围 | 重复位置 | 行数 | diff 结果 |
|---|---|---|---|
| **1947–2032**（含 `/* 规则链 */` `/* 判定结论 */` 注释头） | **2177–2262** | **86** | 逐字节相同 |
| **2073–2176**（含 `/* 地球仪 */` 注释头） | **2263–2366** | **104** | 逐字节相同 |

合计 **190 行 = 文件 2408 行的 7.9%**。受影响的**顶层规则块共 27 个**（每个出现 2 次）：

- 规则链 13 个：`.chain` `.chain__row` `.chain__row:last-child` `.chain__row:nth-child(odd)` `.chain__idx` `.chain__tag` `.chain__conds` `.chain__arrow` `.chain__out` `.verdict` `.verdict__head` `.verdict__reasons` `.verdict__unknown`
- 地球仪 14 个：`.globe` `.globe__canvas` `.globe__canvas:active` `.globe__hint` `.facts` `.fact` `.fact__label` `.fact__place` `.fact__meta` `.fact__src` `.facts__mid` `.facts__km` `.facts__km--small` `.facts__hint`

**为什么会这样**：文件结构是
`…拓扑页(1506–1946) → 规则链(1947–2032) → @media 880(2042–2071) → 地球仪(2073–2176)`，
然后**有人把「规则链」和「地球仪」两个章节原样粘贴到了文件末尾**（2177–2366），
末尾只多了 4 组新规则（第 2367 行 `.highway{position:relative}`、`.globe__tools`、`.globe__tool`、`.fact__warn`，2367–2408）。
`@media (max-width: 760px)` 也因此出现**两次**（第 2167 行、第 2357 行）。

**为什么这是真问题**（不是「无害的重复」）：
后一份在层叠里赢。所以**改前一份不生效**。第 2034–2041 行的注释正好记录了同类事故：

> 「本文件里这段媒体查询曾有**两份**完全相同的拷贝（改一处漏一处），已删掉后一份。」

—— 媒体查询那一份**已经**被删了，但**它两侧的 190 行没删**。事故的根没拔掉。

#### 另外 3 处「不同选择器、相同声明体」的真重复

| 一对 | 行号 | 相同行数 | 备注 |
|---|---|---|---|
| `.dash__details > summary` = `.page__details > summary` | 896 ↔ 1224 | **2 条规则体完全相同**（`>summary` 与 `>summary::before` 两族，各 9 行 / 7 行） | 仪表盘的折叠区与通用页骨架的折叠区是同一个组件，两份 CSS |
| `.logs-bar` = `.nodes-bar` | 943 ↔ 1346 | **5 行声明体完全相同** | 两个页面的筛选栏同构 |
| `.highway`（1513） vs `.highway`（2367） | 1513 / 2367 | 后者只加 `position: relative` | 合法层叠，但应合并到一处 |

`@media (max-width: 880px)` 里的两处（第 2047 行与 2051 行）也是这个情况：
一个写 `grid-column/grid-row`，一个写 `align-items`，**可以合并成一个块**，但不是重复。

### 1.6 死代码：`.card` 是「定义了但从不生效」的 primitive

| | 内容 |
|---|---|
| `.card` 定义 | 第 319–325 行：`background: var(--bg-elevated); border: 1px solid var(--border); border-radius: var(--radius); padding: 16px; margin-bottom: 14px` |
| `.card` 的**唯一**使用点 | `Settings.tsx:87/139/231/287/336/363/433`，写法都是 `className="card set__sec"` |
| `.set__sec` 定义 | 第 1135–1143 行：`background: none; border: none; border-radius: 0; padding: 0; margin: 0` |

**`.set__sec` 把 `.card` 的 5 条声明全部覆盖**。CDP 实测设置页 `.card` 元素的计算样式：
`radius=0px`、`padding=0px 0px 0px 0px`、`background=rgba(0,0,0,0)` —— 与实测一致。

所以 `.card` 作为**卡片**在应用里**从未出现过**。仪表盘、节点、订阅、规则、拓扑、地球仪、日志
**七个页面都没有卡片**，设置页曾经有（第 1083–1088 行注释：「原来是 9 张带边框的卡片依次堆叠」），
改成留白分组后留下了空壳。**任务卡把这个文件描述为有 `card` 体系，实际没有。**

---

## 2. 设计系统

### 2.0 设计立场：ultra-minimal 的具体含义

用户多次要求「朝简洁方向优化」。落到可执行的判据，本系统的三条规则是：

1. **一页最多 3 种表面色**（现在最多 11 种）。层级靠**留白 + 一条弱分隔线**表达，不靠给每块画边框。
2. **颜色只用来承载语义**，不用来装饰。能用明度区分就不用色相。
3. **字阶 5 档、间距 8 档**，其余一律不允许出现。

### 2.1 层级

```
背景层      bg / bg-sunken / bg-chrome        ← 窗口、凹槽、侧栏（不承载交互）
表面层      surface-1 / surface-2             ← 卡片、列表行、输入框（承载内容）
交互态      surface-hover / surface-active    ← 悬停、选中（只在表面层上加）
边界层      border / border-strong            ← 分隔线与控件轮廓
文字层      text / text-dim / text-faint      ← 三档，覆盖全部文字
强调层      accent / accent-hover / on-accent
语义层      ok / warn / danger / info ×(色, 边, 底, 字)
```

### 2.2 颜色 token（提案）

#### 背景（3）

| token | 值 | 用途 | 现值来源 |
|---|---|---|---|
| `--bg` | `#0f1420` | 应用窗口底 | 不变 |
| `--bg-sunken` | `#0a0e16` | **比窗口更深的凹槽**：日志正文、代码块 | 收敛硬编码 `#0a0e16`（520/1020/1277） |
| `--bg-chrome` | `#0b101a` | 应用框架：侧栏 | 收敛硬编码 `#0b101a`（57） |

> 侧栏要不要和 `--bg` 合并？**建议保留**：侧栏是固定的框架，比内容底更暗一档能形成「窗口边缘」的心理边界，
> 实测两者距离 8.2、对比度 1.11:1 —— 很弱但足以在整块纯色里看出分界。若 lead 选更极简，可合并，代价是侧栏与内容糊在一起。

#### 表面（2）+ 交互态（2）

| token | 值 | 用途 | 现值来源 |
|---|---|---|---|
| `--surface-1` | `#161d2c` | 一级表面：列表行、标签卡、`select`、工具按钮 | = 现 `--bg-elevated`（19 次） |
| `--surface-2` | `#1c2436` | 二级表面：输入框、按钮、分段控件、搜索框 | = 现 `--bg-input`（4 次） |
| `--surface-hover` | `#1a2333` | **悬停**统一值 | 收敛 `#141c2b`(100) + `#1a2333`(395) |
| `--surface-active` | `#1b2740` | **选中**统一值 | 收敛 `#1b2740`(105/399) + `#1b2a42`(1755) |

> **为什么选 `#1a2333` / `#1b2740` 而不是另一对**：它们在既有的 `.list__row:hover` / `.nav-item.is-active`
> 上已经在用，且与 `--surface-1/#161d2c` 的距离分别是 10.0 / 13.6 —— 都能看出状态变化但不跳。
> 被替换掉的 `#141c2b`（距离 2.4）本来就是「看不出变化」的那个。
>
> 这一条把 **14 种深色底收敛成 7 个 token**（bg×3 + surface×2 + 交互态×2）。
> `#131a29`（弹窗）直接用 `--surface-1`（距离 5.2，视觉等价），顺带把假 token `--panel` 清掉。

#### 边界（2）

| token | 值 | 用途 | 可访问性 |
|---|---|---|---|
| `--border` | `#263148` | **装饰性**分隔线、卡片/列表轮廓（不承载「这里有控件」的信息） | 1.30–1.42:1 —— **装饰豁免** |
| `--border-strong` | `#334155` | 控件轮廓（按钮、输入框、胶囊） | **1.50–1.78:1，低于 1.4.11 的 3:1** → 见 §5.4 |

> 待定项：`--border-strong` 是否提升到 `#5c7290`（实测 3.15–3.74:1）。
> 这不是审美决定，是「要合规」还是「要克制」的取舍，需要 lead 拍板 —— §5.4 给了两个方案的代价。

#### 文字（3）

| token | 值 | 用途 | 最差背景上的对比度 |
|---|---|---|---|
| `--text` | `#e6ecf5` | 主文字：标题、正文、数据 | 13.05:1（`--bg-input`） |
| `--text-dim` | `#94a3b8` | 次要文字：说明、标签、`desc` | 6.04:1 |
| `--text-faint` | **`#7b8da6`** ← 建议改（现 `#64748b`） | 三级文字：元信息、单位、脚注 | 现 **3.26:1 ✗** → 改后 **4.58:1 ✓** |

**这是本次审计唯一建议改动色值的 token。** 理由见 §5.2：
`--text-faint` 是**用得最多**的 token（53 次），在所有 5 个背景上**全部**低于 4.5:1。
候选值实测：

| 候选 | on `--bg` | on `--bg-elevated` | on `--bg-input` |
|---|---|---|---|
| `#64748b`（现在） | 3.87 ✗ | 3.54 ✗ | 3.26 ✗ |
| `#6b7c93` | 4.32 ✗ | 3.96 ✗ | 3.64 ✗ |
| `#7386a0` | 4.95 ✓ | 4.53 ✓ | 4.17 ✗ |
| **`#7b8da6`** | **5.44 ✓** | **4.98 ✓** | **4.58 ✓** |

→ **`#7b8da6` 是能在三个背景上同时过 4.5:1 的最小改动。**

#### 强调 / 语义（含缺失的 tint 三件套）

| token | 值 | 用途 |
|---|---|---|
| `--accent` | `#4f8ef7` | 强调：焦点环、选中填充、链接、主按钮 |
| `--accent-hover` | `#6ba0ff` | 强调 hover |
| **`--on-accent`** | `#06101f` | **强调色上的文字**（现硬编码 ×2，211/259） |
| `--ok` | `#34d399` | 成功主色 |
| `--warn` | `#fbbf24` | 警告主色 |
| `--danger` | `#f87171` | 危险主色 |

语义 **tint 四件套**（现在全硬编码，且分散在两处）：

| 语义 | `-border` | `-bg` | `-text` | 实测对比度(text on bg) |
|---|---|---|---|---|
| ok | `#1f5545` | `--surface-1`（现无 bg tint） | `var(--ok)` | 8.77:1 ✓ |
| warn | `#57471f` | `#241f10` | `#f6e3b4` | 12.96:1 ✓ |
| danger | `#5b2b33` | `#2a161a` | `#f8c9cd` | 11.58:1 ✓ |
| info | `#26456b` | `#131f30` | `#cfe1f8` | 12.46:1 ✓ |

> **这四组是全应用对比度最好的颜色**（12–13:1）—— 现在它们却是硬编码的。
> 也就是说：设计上做得对的部分反而没有被 token 化，将来最容易被改坏。

#### 其他

| token | 值 | 用途 |
|---|---|---|
| `--highlight-stroke` | `#9ecbff` | 连接高亮描边 / 焦点轮廓（现硬编码 1753/1866） |
| `--highlight-path` | `#cfe6ff` | 高亮路径描边（现硬编码 1762） |
| `--scrollbar` | `#2a3548` | 滚动条滑块（现硬编码 596） |

### 2.3 字体

**字体栈不变**（`body` 第 31 行已经是对的）：系统 UI 字体 + `--mono` 等宽。

**字阶：11 档收敛成 5 档。** 规则：**相邻档位至少差 1px**。

| token | 值 | 用途 | 吸收掉 |
|---|---|---|---|
| `--fs-2xs` | `10.5px` | 微标签：图例、单位、等宽数字、badge 计数 | `10px`(4)、`10.5px`(22) |
| `--fs-xs` | `11.5px` | 元信息：列表副标题、卡片 meta、`field__hint` | `11px`(27)、`11.5px`(15) |
| `--fs-sm` | `12px` | 密集正文：`page__desc`、`banner`、`note` | `12px`(22)、`12.5px`(5) |
| `--fs-md` | `13px` | **正文基准**（= 现在的 `body` 默认，506 个渲染元素） | `13px`(5) |
| `--fs-lg` | `14px` | 区块/页面标题、品牌 | `14px`(6)、`15px`(5) |
| `--fs-xl` | `20px` | 主数字（仪表盘状态、`stat__value`） | `20px`(1)、`21px`(1) |

> **`12px` 与 `12.5px` 合并到 `12px`、`13px` 保持**：两者只差 1px，但 `12px` 承载说明文字、
> `13px` 承载正文，职责不同，且 13px 是 `body` 基准（改它会动全局）。所以保留两者，把 12.5 并进 12。
>
> **`15px` → `14px`**：第 24 行 `.sidebar__brand`(15px) 与第 1172 行 `.page__title`(14px) 是「品牌名 vs 标题」，
> 差 1px 无法形成层级。合并到 14px，靠字重（600 vs 600 时靠位置和颜色）区分。
>
> **`21px` → `20px`**：`.dash__state`(21px, 注释在 755) 与 `.stat__value`(20px, 360) 同在仪表盘，
> 是两个「主数字」，差 1px 是纯粹的不小心。
>
> ⚠️ **实施警告**：`11.5px` 渲染在 572 个元素上，`10.5px` 在 213 个上。
> 字阶收敛**不要机械全量替换**：`11px → 11.5px` 是 +4.5%，在拓扑页出口卡片
> （`.highway__lane-label` 固定列宽 168px）里可能把 `direct` 这类标签挤换行。
> 正确做法是**逐类替换 + 每类截图核对**，不是 `sed` 一把梭。这一点写进 §6 的分步验收里。

**字重**：现有 `400`（默认）/ `500`（导航选中、列表名）/ `600`（标题、主按钮）。
建议**只保留 400 / 500 / 600**三档 —— 已经没有别的值，无需清理，写进规范防未来加 `700`。

**行高**：`body` 是 `1.55`；`.page__desc` 用 `1.75`；`.logs` 用 `1.65`；`.empty` 用 `1.8`。
建议 token 化：`--lh-tight: 1.25`（大字号标题）/ `--lh-ui: 1.55`（UI 默认）/ `--lh-read: 1.75`（密集说明文字）。

### 2.4 间距刻度

现状：**68 个不同 px 值**。收敛成**双层刻度**（UI 密集界面需要 2px 细调，布局间距用 4px 步进）：

**内间距（控件内部，2px 步进）**

| token | 值 | 用途 | 吸收 |
|---|---|---|---|
| `--sp-1` | `2px` | 图标与文字、胶囊内上下边 | `2px`, `1px` |
| `--sp-2` | `4px` | 小控件内边距、紧凑 gap | `3px`, `4px`, `5px` |
| `--sp-3` | `6px` | 按钮内边距、行内 gap | `6px`, `7px` |
| `--sp-4` | `8px` | 控件内边距、列表 gap | `8px`, `9px` |

**布局间距（块之间，4px 步进）**

| token | 值 | 用途 | 吸收 |
|---|---|---|---|
| `--sp-5` | `12px` | 区块内边距、行 padding | `10px`, `12px` |
| `--sp-6` | `16px` | 卡片内边距、字段间距 | `14px`, `16px`, `18px` |
| `--sp-7` | `24px` | 区块之间（`page__sec` 分隔） | `22px`, `24px`, `26px` |
| `--sp-8` | `32px` | 大段留白、页面底部 | `28px`, `30px`, `32px`, `34px` |

> ⚠️ 注意 `.content` 的 `padding: 18px 20px 32px`（第 177 行）和 `.topbar` 的
> `padding: 34px 20px 12px`（第 133 行）里的 `20px` / `34px` 是**窗口级固定量**
> （20px 是内容左右边距、34px 是 macOS 红绿灯按钮的安全区，见第 60–62 行注释），
> 它们是唯一的合法例外，建议单列 `--gutter: 20px` / `--titlebar-safe: 34px`，不要塞进 sp 刻度。

实测收敛效果：**68 个不同 px 值 → 8 个间距 token + 2 个窗口常量**。

### 2.5 圆角

| token | 值 | 用途 | 现值来源 |
|---|---|---|---|
| `--radius-lg` | `10px` | 容器：列表、日志面板、弹窗 | `--radius`（改名，13 次） |
| `--radius` | `6px` | 控件：按钮、输入、标签卡、badge 容器 | `--radius-sm`（改名，15 次） |
| `--radius-xs` | `3px` | 小指示器：进度条轨道、连接提示块 | 硬编码 `3px`(2) |
| `--radius-pill` | `999px` | 胶囊：badge、计数徽章 | 硬编码 `999px`(3) |

**删除**：`5px`(2)、`1px`(1)。`.segmented button.is-active` 从 5px 改为 **4px**
（= 外圆角 6px − 内边距 2px，实测 `.segmented { padding: 2px }` 第 233 行），使内圆角与外圆角几何贴合。

> 改名（`--radius` ↔ `--radius-sm` 语义互换）会让 26 处引用同时改变含义，
> **建议不要改名**，只新增 `--radius-xs` / `--radius-pill`，把 `5px` / `1px` 消掉即可 ——
> 改名收益为零、风险为「改错一处就全站圆角错位」。这一条我在 §6 里按「不推荐」处理。

### 2.6 阴影

现状**没有阴影 token**，只有 4 处 `box-shadow`，而且**三处在做别的事**：

| 行号 | 用法 | 实际是 |
|---|---|---|
| 454 | `.dot--on { box-shadow: 0 0 0 3px rgba(52,211,153,.15) }` | 光晕（状态指示） |
| 1599–1611 | `.highway__lane-label--* { box-shadow: inset ±2px 0 0 <色> }` | **左侧色条**（不是阴影） |
| 1866 | `.conn-row--on { box-shadow: inset 2px 0 0 #9ecbff }` | **左侧色条** |

深色主题下**真正的投影没有意义**（背景已经接近黑），所以规范是：

- **禁止**新增 `box-shadow` 阴影。层级靠表面色 + 边界表达。
- 需要「左侧色条」时，**统一用 `box-shadow: inset Npx 0 0`**（保持现有做法，改动最小），
  并把它叫 `--inset-bar` 约定而不是 `--shadow`。**不要**改用 `border-left`——
  第 1061–1064 行注释记录了「用 border-left 而不是伪元素，避免与 flex 对齐打架」，同一坑。
- 需要「光晕」时用 `box-shadow: 0 0 0 3px <语义色>@0.15`，目前只有 `.dot--on` 一处，保留即可。

---

## 3. 动效规范

### 3.1 现状（实测）

**CSS 动画 2 个**、**transition 6 条**、**JS rAF 循环 2 处**：

| 类型 | 位置 | 定义 | 时长 |
|---|---|---|---|
| `@keyframes spin` | 584 | `animation: spin 0.7s linear infinite`（加载圈，578 行） | 700ms |
| `@keyframes conn-dash` | 1769 | `animation: conn-dash 0.9s linear infinite`（拓扑高亮虚线流动，1766 行） | 900ms |
| transition | 191 | `background 0.12s ease, color 0.12s ease`（`.set__nav a`） | 120ms |
| transition | 251 附近 | `background 0.12s ease, color 0.12s ease`（分段控件） | 120ms |
| transition | 251/917 | `transform 0.12s ease`（`.details summary::before` 三角） | 120ms |
| transition | 1443 | `width 0.2s ease`（`.usage__fill` 用量条） | 200ms |
| transition | 641 附近 | `border-color 0.12s ease, background 0.12s ease` | 120ms |
| rAF | `Topology.tsx:1267,1269` | 货车位置动画 | 每帧 |
| rAF | `Globe.tsx:383,385` | 地球自转 + 飞机弧线 | 每帧 |

### 3.2 规范

| token | 值 | 用途 |
|---|---|---|
| `--dur-instant` | `80ms` | 按下反馈、开关 |
| `--dur-fast` | `120ms` | **默认**：hover、颜色/边框变化（吸收现有全部 0.12s） |
| `--dur-base` | `200ms` | 尺寸/宽度变化（用量条、展开） |
| `--dur-slow` | `320ms` | 面板展开、弹窗进入 |
| `--ease-out` | `cubic-bezier(0.2, 0, 0, 1)` | 进入/出现 |
| `--ease-in-out` | `cubic-bezier(0.4, 0, 0.2, 1)` | 状态切换 |
| `--ease-linear` | `linear` | **仅**用于两个无限循环：`spin`、`conn-dash` |

规则：
1. **默认 120ms + `ease`**，与现有代码一致，无需改动既有 transition。
2. **无限循环动画只允许 2 个**（加载圈 700ms、高亮流动 900ms），且必须是 `linear` ——
   循环里用非线性的缓动会看出「一顿一顿」。
3. **不允许给 `width/height/top/left` 以外的布局属性做过渡**；现有唯一例外是
   `.usage__fill { transition: width 0.2s }`（第 1443 行），因为它是一次性状态变化、不在滚动热路径上，**保留**。
4. **动画时长必须能整除**：700ms/900ms 与 120ms 不成比例，这是有意的 ——
   循环动画要**下意识可见**，不能和交互反馈混淆。

### 3.3 `prefers-reduced-motion`（**必须对齐拓扑页先例**）

**现状：整个应用只有 1 处处理，而需要处理的有 4 处。**

第 1774–1778 行（拓扑页先例，写法正确）：

```css
@media (prefers-reduced-motion: reduce) {
  .flow__highlight {
    animation: none;
  }
}
```

但下面 **3 处没有被覆盖**：

| 未覆盖的动画 | 位置 | 为什么漏了 |
|---|---|---|
| 加载圈 `spin` | 578–586 行 | CSS 动画，但媒体查询里只列了 `.flow__highlight` |
| `.details summary::before` 旋转 | 917 行附近 | 120ms transform，属于「装饰性动效」，也应降级 |
| **拓扑货车 rAF** | `Topology.tsx:1267` | **JS 循环，CSS 媒体查询管不到** |
| **地球仪自转 + 飞机 rAF** | `Globe.tsx:383` | 同上 |

实测确认：`grep -rn 'prefers-reduced-motion\|matchMedia\|reducedMotion' apps/ui/src/`
**只返回 `styles.css:1774` 一行**，TS 侧零命中。

**规范（对齐拓扑页先例并补齐缺口）**：

```css
/* 1. 所有 CSS 无限动画统一关闭 */
@media (prefers-reduced-motion: reduce) {
  .flow__highlight,
  .spinner { animation: none; }
  .details > summary::before,
  .page__details > summary::before,
  .dash__details > summary::before { transition: none; }
  * , *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
    scroll-behavior: auto !important;
  }
}

/* 2. "关掉动画"不等于"看不出来在加载" —— 加载圈必须换成静态指示 */
@media (prefers-reduced-motion: reduce) {
  .spinner { border-top-color: var(--accent); opacity: 0.6; }  /* 静态圆环 */
}
```

**JS 侧（新增要求，给 frontend-dev）**：两个 rAF 循环必须在每帧开头读一次媒体查询并降级，
而不是停掉一切：

| 页面 | 降级行为 |
|---|---|
| 拓扑货车 | 停在**路径中点**不动（保留「这条路有流量」的信息，去掉运动） |
| 地球仪自转 | 停止自转，但**保留手动拖动/缩放**（用户主动触发的变换不属于「动效」） |
| 地球仪飞机 | 停在弧线中点，保留弧线与起讫标记 |

```ts
// 统一的读取入口（建议放 apps/ui/src/preview.ts 旁边或新 util）
const reduceMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
// 且要监听变化，而不是只在挂载时读一次：
//   mq.addEventListener("change", ...) —— 用户在系统设置里改了就应立即生效
```

> **为什么这条重要**：`prefers-reduced-motion` 的用户里有一部分是**前庭功能障碍**，
> 全屏持续的、与用户操作无关的运动（两个 rAF 循环都是）会引发眩晕和偏头痛。
> 拓扑页已经意识到这件事（写了媒体查询），但只覆盖了自己那一处 —— 规范要把这个「先例」变成「全站规则」。

---

## 4. 跨页一致性（具体条目）

方法：用 CDP 在 8 个页面上采集**所有可见元素**的计算样式，再按「语义角色」对齐比较。
每页采集 69–558 个元素（拓扑 441、日志 558、设置 234），共 1649 个元素。

### 4.1 页面骨架：8 页里只有 6 页用同一套骨架

`.page`（第 1178 行，`max-width: 820px` + `gap: 26px`）在第 1172–1175 行的注释里写着
「仪表盘 / 日志 / 设置 / 节点 / 订阅 / 规则 六页共用这一套」—— 实测**不成立**：

| 页面 | `.page` | `.page__sec` | `.page__title` | `.page__desc` | 实际宽度 |
|---|---|---|---|---|---|
| dashboard | ✗ | ✗ | ✗ | ✗ | 832px（无上限） |
| nodes | ✓ | ✗ | ✗ | ✗ | 820px |
| subscriptions | ✓ | 2 | 2 | 2 | 820px |
| routing | ✓ | 2 | 2 | 3 | 820px |
| topology | ✓ | 4 | 4 | 4 | 820px |
| globe | ✓ | 1 | 1 | 1 | 820px |
| logs | ✗ | ✗ | ✗ | ✗ | 832px（无上限） |
| settings | ✗（用 `.set`） | ✗ | ✗ | ✗ | 820px（`.set` 自带） |

**具体不一致**：
- **仪表盘 / 日志不走 `.page`**，内容宽度 832px（`.content` 872px − 左右 padding 20px×2），
  比其它 6 页的 820px **宽 12px**。这 12px 在跨页切换时是可感知的「内容左右边界跳动」。
- **仪表盘 / 日志 / 设置没有 `.page__sec + .page__sec` 的那条 24px 分隔线**（第 1193 行），
  于是同一套「区块之间画一条弱线」的语言在这三页失效。

### 4.2 区块标题：同一个角色，5 页 5 种写法

| 页面 | 承载元素 | 字号 | 字重 | 颜色 | margin |
|---|---|---|---|---|---|
| subscriptions | `.page__title` | 14px | 600 | `--text` | `0` |
| routing | `.page__title` | 14px | 600 | `--text` | `0` |
| topology | `.page__title` | 14px | 600 | `--text` | `0` |
| globe | `.page__title` | 14px | 600 | `--text` | `0` |
| **settings** | `.set__sec > .card__title` | 14px | 600 | `--text` | **`0 0 6px`** |
| **dashboard** | `.dash__details > summary` | **12.5px** | **500** | **`--text-dim`** | — |
| **nodes** | 无 | — | — | — | — |
| **logs** | 无 | — | — | — | — |

**具体不一致**：
- **settings 的区块标题有 6px 下外边距，其余 4 页是 0**（第 1150–1156 行 vs 第 1196–1201 行）。
- **dashboard 用 12.5px/500/`--text-dim`**（第 897–906 行）表达区块标题 —— 比其它页**小 1.5px、轻 100 字重、暗一档**。
  同样语义（「这是一个区块」），仪表盘看起来像次要说明。
- **nodes / logs 完全没有区块标题**，靠 `.list` 的边框和 `.logs` 的面板边界分区。

### 4.3 分隔线：同一角色 3 种写法，其中 1 种**根本没渲染**

| 行号 | 写法 | 作用域 | padding-top |
|---|---|---|---|
| 1193 | `border-top: 1px solid var(--border)` | `.page__sec + .page__sec` | **24px** |
| 892 | `border-top: 1px solid var(--border)` | `.dash__details` | **14px** |
| 1220 | `border-top: 1px solid var(--border)` | `.page__details` | **18px** |
| **1632** | `border-top: 1px dashed var(--line)` | `.highway__internal` | 10px |
| 1856 | `border-bottom: 1px solid rgba(255,255,255,0.04)` | `.conn-row` | — |

**具体不一致**：
1. **同一个「分隔线 + 上边距」模式有 14 / 18 / 24 三个值**（行 892 / 1220 / 1193）。
2. **第 1632 行的分隔线不渲染**（`--line` 未定义，见 §1.2），它是拓扑页「出口列表 / 内部通道」之间
   唯一的视觉分隔 —— 现在两个区块直接贴在一起。
3. **第 1856 行用 `rgba(255,255,255,0.04)` 而不是 `--border`**：实测这个值在 `--bg` 上
   对比度约 **1.12:1**，而 `--border #263148` 是 **1.42:1** —— 连接列表的行分隔线比全站其它
   分隔线**弱 27%**（第 1856 行是全站唯一一处白 alpha 分隔线）。

### 4.4 提示 / 状态块：2 套并存

| 组件 | 位置 | 外观 |
|---|---|---|
| `.note` | routing（第 1250 行起） | `border-left: 2px solid var(--border-strong)` + `border-radius: 0 6px 6px 0` + `background: --bg-elevated` |
| `.banner--warn/--error/--info` | settings（第 490 行起） | 四边 `1px solid <tint>` + `border-radius: 6px` + `background: <tint>` |
| `.conn-note` | topology（第 1830 行） | `background: rgba(120,160,210,0.08)` + `border-left: 2px solid var(--border-strong)` + `radius: 3px` |

**具体不一致**：
- `.note`（routing）与 `.conn-note`（topology）**都是「左侧竖条 + 淡底」**，但
  `.note` 的 radius 是 **0 6px 6px 0**、`.conn-note` 是 **3px**，圆角不同；padding 是 **12px 14px** vs **4px 7px**。
- `.banner`（settings）用**四边框**，`.note` 用**左竖条** —— 两种「信息提示」语言并存。

### 4.5 表面色：同一个角色在不同页用了不同的灰

| 角色 | 页面 | 实测背景 |
|---|---|---|
| 列表行 | nodes / subscriptions | `--bg-elevated` `#161d2c` |
| 列表行 hover | nodes / subscriptions | `#1a2333`（硬编码，第 395 行） |
| 列表行选中 | nodes | `#1b2740`（硬编码，第 399 行） |
| 标签卡（拓扑） | topology | `--bg-elevated` `#161d2c` |
| 标签卡 hover | topology（`.globe__tool:hover` 同构） | `--border-strong` 描边（1626 行起） |
| 输入框 | settings / subscriptions / nodes | `--bg-input` `#1c2436` |
| 按钮 | 全站 | `--bg-input` `#1c2436` |
| `select`（拓扑） | topology | `--bg-elevated` `#161d2c` |

**具体不一致**：`topology` 的 `.select` 用 `--surface-1`（19 次，第 1586 行附近），
而 `settings` 的 `select` 走通用 `input/select/textarea` 规则用 `--surface-2`（第 10 次）。
同样是下拉框，**拓扑页比设置页暗一档**（实测 `rgb(22,29,44)` vs `rgb(28,36,54)`）。

### 4.6 一致的部分（确认，避免过度重构）

- **`.btn` 全站一致**：8 个页面全部 `13px / radius 6px / padding 6px 12px / color --text`（实测 8/8 逐字段相同）。
- **`.nav-item` 全站一致**：`13px / radius 6px / padding 7px 10px`。
- **`.topbar` 全站一致**：`padding 34px 20px 12px`。
- **`.content` 全站一致**：`padding 18px 20px 32px`，`clientHeight` 恒为 **636px**（1080×720 窗口实测）。
- **`.badge` 全站一致**：`11px / radius 999px / padding 1px 7px`。

→ 所以「跨页不一致」不是全局性的，而是集中在**区块标题、分隔线、提示块、表面色**四类。
这四类正好是重构第一批要收敛的（§6）。

---

## 5. 可访问性：对比度实算

方法：按 WCAG 2.x 相对亮度公式实算（sRGB 线性化 → `0.2126R + 0.7152G + 0.0722B` → `(L1+0.05)/(L2+0.05)`）。
带 alpha 的前景/背景用 **src-over 合成到实际背景**后再算。脚本 `/tmp/xraytun-audit/contrast.mjs`。

### 5.1 结论表：文字 vs 各自背景（AA 小字需 ≥ 4.5:1）

| 前景 \ 背景 | `--bg` `#0f1420` | `--bg-elevated` `#161d2c` | `--bg-input` `#1c2436` | sidebar `#0b101a` | logs `#0a0e16` |
|---|---|---|---|---|---|
| `--text` `#e6ecf5` | 15.49 ✓ | 14.19 ✓ | 13.05 ✓ | 16.03 ✓ | 16.26 ✓ |
| `--text-dim` `#94a3b8` | 7.18 ✓ | 6.57 ✓ | 6.04 ✓ | 7.42 ✓ | 7.53 ✓ |
| **`--text-faint` `#64748b`** | **3.87 ✗** | **3.54 ✗** | **3.26 ✗** | **4.00 ✗** | **4.06 ✗** |
| `--accent` `#4f8ef7` | 5.73 ✓ | 5.25 ✓ | 4.83 ✓ | 5.93 ✓ | 6.02 ✓ |
| `--ok` `#34d399` | 9.57 ✓ | 8.77 ✓ | 8.06 ✓ | 9.90 ✓ | 10.05 ✓ |
| `--warn` `#fbbf24` | 11.02 ✓ | 10.09 ✓ | 9.28 ✓ | 11.40 ✓ | 11.57 ✓ |
| `--danger` `#f87171` | 6.65 ✓ | 6.09 ✓ | 5.60 ✓ | 6.88 ✓ | 6.98 ✓ |

### 5.2 ⚠️ `--text-faint` 在**全部 5 个背景上都不达标**（3.26–4.06:1）

这是本次审计**最严重、最具体**的一条。

- 它是**用得最多**的文字 token：**53 次**引用（第二名的 `--text-dim` 只有 34 次）。
- 低于 4.5:1 的**范围**：`--bg-input` 3.26 → sidebar 4.00 → `--bg` 3.87。
  **最小值 3.26（`--bg-input` 上），距离 4.5 差 27.6%。**
- 具体受害位置（举例，非全部）：

| 行号 | 选择器 | 字号 | 承载的信息 |
|---|---|---|---|
| 1044 | `.logs-page .log-line__src` | 10.5px | 日志来源（xray / helper / app） |
| 1581 附近 | `.highway__lane-meta` | 10.5px | **出口类别文字**（node / direct / block / dns / internal）—— 颜色之外唯一说明分类的文本 |
| 1551 附近 | `.highway__side-title` | 11px | 「入口」/「出口」列标题 |
| 1690 附近 | `.highway__legend-note` | 10.5px | 图例说明 |
| 2162/2352 | `.facts__hint` | 10.5px | 地球仪事实面板脚注 |
| 2139/2329 | `.fact__src` | 10.5px | 定位数据来源 |
| 447 | `.list__meta` | 11px | 列表行副标题 |
| 114 | `.nav-item__badge` | 11px | 导航计数 |
| 78 | `.sidebar__footer` | 11px | 版本号 |

**修复**：`--text-faint: #64748b` → **`#7b8da6`**（实测 5.44 / 4.98 / 4.58，三处全过）。
这一改会**同时修好 53 处引用**，不需要逐个改调用点。

### 5.3 ⚠️ 再叠加 `opacity` 之后，有几处跌到 1.85–2.64:1

CSS 里的 `opacity` 会**乘算**在已经算好的颜色上，这是最容易被忽略的对比度杀手：

| 位置 | 行号 | 声明 | 合成后的实际色 | **对比度** | 判定 |
|---|---|---|---|---|---|
| `.logs-page .log-line__src` | 1051 | `--text-faint` **再 ×0.7** | `#495568` | **2.56:1** | ✗ |
| `.log-line--debug` 全行 | 1080 | 整行 `opacity: 0.72`（前景仍是 `--text-faint`） | `#4b576a` | **2.64:1** | ✗ |
| `.list__row.is-disabled` | 403 | 整行 `opacity: 0.5` | `#3d495c` | **1.85:1** | ✗（禁用态可豁免，但 1.85 太低） |
| `.btn:disabled` | 204 | `opacity: 0.45`（前景 `--text`） | `#777e8c` | 3.80:1 | △（大字号才合规） |

> **`.log-line__src` 的组合最糟**：它先取 `--text-faint`（本来就只有 3.54:1），再乘 0.7 →
> **2.56:1**，而它是等宽字体 10.5px 的日志来源标识，在满屏日志里本来就是最难读的元素。
>
> **规范**：`--text-faint` 提升到 `#7b8da6` 之后，`×0.7` 会变成 `#5f6e83`（3.4:1），**仍然不达标**。
> 所以这两处（1051、1080）应当**去掉 `opacity`**，改用「颜色本身就暗一档」表达层级 ——
> 这也是更正确的做法：`opacity` 会连带影响子元素和边框，语义上不等于「次要文字」。

### 5.4 ⚠️ 非文字边界：**全部低于 1.4.11 要求的 3:1**

WCAG 1.4.11（Non-text Contrast）要求「识别 UI 组件与状态所必需的视觉信息」达到 **3:1**。

| 边界 | 对比对象 | 实测 | 判定 |
|---|---|---|---|
| `--border` `#263148` | `--bg` | **1.42:1** | ✗ |
| `--border` | `--bg-elevated` | **1.30:1** | ✗ |
| `--border` | `--bg-input` | **1.19:1** | ✗ |
| `--border-strong` `#334155` | `--bg` | **1.78:1** | ✗ |
| `--border-strong` | `--bg-elevated` | **1.63:1** | ✗ |
| `--border-strong` | `--bg-input`（按钮边 vs 按钮底） | **1.50:1** | ✗ |
| `--bg-elevated` | `--bg`（卡片 vs 页底） | **1.09:1** | ✗ |
| `--bg-input` | `--bg`（输入框 vs 页底） | **1.19:1** | ✗ |
| `--accent` `#4f8ef7` | `--bg`（焦点环） | 5.73:1 | ✓ |
| `--accent` | `--bg-input` | 4.83:1 | ✓ |
| `--ok` `#34d399` | `--bg`（顶栏状态条） | 9.57:1 | ✓ |

**关键问题**：`.btn` 的**可见轮廓只有 `--border-strong`（1.78:1）**，因为按钮填充 `--bg-input` 与页底
只差 **1.19:1**。也就是说按钮**完全依赖一条 1.78:1 的边**来被认出来。

**两个方案的代价**（需要 lead 拍板，因为它会明显改变观感）：

| 方案 | 做法 | 代价 |
|---|---|---|
| **A. 提高控件边** | `--border-strong` `#334155` → **`#5c7290`**（实测 3.15–3.74:1 全面过 3:1） | 按钮/输入框的描边**明显变亮**，与「ultra-minimal 的弱边界」审美有冲突 |
| **B. 提高控件填充** | `--bg-input` `#1c2436` → `#232d42`（约 1.5:1）并保持弱边 | 只能到 1.5:1，**仍不达标**；且要同时调 `--bg-elevated` 否则表面层级塌掉 |
| **C. 只修交互态** | 保持静止态弱，**焦点/悬停时**给 3:1（现 `.btn:hover { border-color: var(--accent) }` = 5.73:1 ✓） | 合规上是最诚实的解释：键盘用户看到的是焦点态（已达标），鼠标用户靠指针定位 |

**我的建议：C + A 的部分应用** —— 只把**需要被识别的控件**（按钮、输入框、`segmented`）
的边提升到 3:1，**装饰性**分隔线（`.card`、列表行、区块分割）保持 1.3–1.4:1。
也就是说把 `--border-strong` 拆成两个 token：
`--border-strong`（装饰，1.78:1，不动）与 `--border-interactive`（**`#5c7290`**，3.15:1，只给控件用）。
这样「弱边界」的极简观感和「控件可辨识」的合规性同时满足，代价只有 10 处引用需要换 token。

### 5.5 语义色 tint（全部达标，确认）

| 组合 | 实测 |
|---|---|
| 主按钮文字 `#06101f` on `--accent` | **5.94:1 ✓** |
| 主按钮文字 `#06101f` on `--accent-hover` | **7.35:1 ✓** |
| `.banner--warn` `#f6e3b4` on `#241f10` | **12.96:1 ✓** |
| `.banner--error` `#f8c9cd` on `#2a161a` | **11.58:1 ✓** |
| `.banner--info` `#cfe1f8` on `#131f30` | **12.46:1 ✓** |
| `.badge--fast`/`--ok`/`--slow` on `--bg-elevated` | 8.77 / 10.09 / 6.09 ✓ |

### 5.6 拓扑连线（沿用并交叉验证 `DESIGN-REVIEW.md` 的实测）

我用同一套公式复算 `.flow__route` 在各 `opacity` 下的合成对比度（背景 `--bg`）：

| `opacity` | `--accent` 经节点 | `--ok` 直连 | `--danger` 已拦截 | `--text-faint` 内部 |
|---|---|---|---|---|
| `0.22`（`--trunk`） | **1.38 ✗** | — | — | — |
| `0.30`（**当前值**） | **1.60 ✗** | **1.92 ✗** | **1.64 ✗** | **1.41 ✗** |
| **`0.60`** | **2.83 ✗** | 4.16 ✓ | 3.11 ✓ | — |
| `0.87` | — | — | — | 3.24 ✓ |

与 `docs/ui/topology/DESIGN-REVIEW.md` 的 §D2（实测 1.85–2.20:1）**结论一致、口径略有不同**：
该报告量的是**含灰色回程覆盖后**的屏幕合成值（1.65–2.20），我量的是**纯 `opacity` 叠在 `--bg`** 的设计值
（1.40–1.93）。两者都指向同一结论：**当前 `opacity: 0.3` 下没有一条彩色线达到 3:1**，
且**提高到 0.6 也修不好「经节点」蓝（2.83）**。

→ 所以 §D2 的建议（`opacity: .3 → .6`）**不足以让蓝色线达标**。这是我要指出的第一处冲突（§7）。

### 5.7 地球仪首屏（沿用实测，交叉验证）

在 1080×720（`tauri.conf.json:17-18` 默认窗口）下用 CDP 实测：

| 量 | 实测值 |
|---|---|
| `.content` `clientHeight` | **636px** |
| `.content` `scrollHeight` | **816px** |
| `.globe__canvas` | **560 × 560px**（占可视高 **88%**） |
| `.facts` `top` | **756px** → **在折叠线以下**（> 636） |
| `.globe__hint` `top` | **726px**，且 `position: absolute` → 同样在折叠线以下 |

**与 `DESIGN-REVIEW.md` §F3 完全一致**（该报告量到 `clientHeight 636` / `scrollHeight 816`，
我的实测数字**逐项相同**）。截图确认：首屏只有球体与两个标签，`+/−/复位` 三个按钮可见，其余不可见。

---

## 6. `styles.css` 拆分与收敛：分步方案

**拆分原则**：按**层**拆，不按页面拆。

理由（有依据）：文件里**真正跨页共享**的是 `布局(47–183) / 控件(184–316) / 列表(372–433) / 徽标(434–480) / 提示条(481–520)`
共约 470 行；而**页面专属**的 7 段（仪表盘 725–931、日志 932–1082、设置 1083–1171、规则 1284–1343、
节点 1344–1405、订阅 1406–1505、拓扑+地球仪 1506–2408）占 **1900 行**。
按页面拆会得到 7 个「各自再造一遍按钮」的文件；按层拆能让 8 个页面共用一套 primitive。

**目标结构**（`apps/ui/src/styles/`）：

```
styles/
  00-tokens.css         # :root 全部 token（§2）—— 唯一允许出现字面量色值/字阶的文件
  01-base.css           # reset、body、滚动条、:focus-visible、prefers-reduced-motion（全局）
  10-layout.css         # .shell .sidebar .topbar .content .page* .row* （当前 47-183 + 1172-1283）
  20-controls.css       # .btn .segmented .field input/select/textarea （184-316）
  30-surfaces.css       # .list .badge .banner .note .empty .kv .code-block （317-433, 434-520, 552-604）
  40-overlay.css        # .modal .qr （605-724）
  50-dashboard.css      # 725-931
  51-logs.css           # 932-1082
  52-settings.css       # 1083-1171
  53-routing.css        # 1284-1343
  54-nodes.css          # 1344-1405
  55-subscriptions.css  # 1406-1505
  56-topology.css       # 1506-2072
  57-globe.css          # 2073-2176 + 2367-2408 的 4 组新规则
  99-legacy.css         # 过渡期兜底（见步骤 5）
```

### 步骤 1（**先做，且不依赖任何设计决定**）：删掉 190 行整段重复 + 3 处同体重复

| 动作 | 依据 | 验收 |
|---|---|---|
| 删除第 **2177–2366** 行（保留 2367–2408 的 4 组新规则） | §1.5 逐字节相同；后一份在层叠里赢，删后一份等价于「保留生效的那份」 | ① `git diff` 里**只有删除、没有新增**；② 8 个页面 CDP 截图与改动前逐像素相同（`cmp` 或 imagehash）；③ 拓扑页 `.chain__row` 计算样式不变（`border-bottom: 1px solid rgb(38,49,72)`） |
| 删除第 **2357–2366** 行（第二份 `@media max-width:760px`） | 同上 | 窄窗（760px 以下）截图不变 |
| 合并 `.logs-bar` / `.nodes-bar` 为公共 `--filter-bar` 组件（或保留两个类名、只留一份声明） | §1.5，5 行声明体相同 | 节点页与日志页筛选栏截图不变 |
| 合并 `.dash__details > summary` / `.page__details > summary` | §1.5，2 条规则体相同 | 仪表盘「环境自检与诊断」折叠区截图不变 |
| 合并第 2367 行 `.highway{position:relative}` 到第 1513 行的 `.highway` | §1.5 | 拓扑页 `.highway` 计算 `position` 仍为 `relative` |

**预期**：2408 → **约 2180 行**（−9.5%）。**这一步零风险、零视觉变化，且纯收益** —— 我觉得这是最该先做的一步。

### 步骤 2：补 `--line`，把断掉的分隔线修回来（1 行）

`--line` 加进 `:root`（值取 `--border` 的语义，如 `#263148`），或直接把第 1632 行改成 `var(--border)`。
同时把第 619 行的 `var(--panel, #131a29)` 改成明确的 `--surface-1`。

验收：CDP 实测 `.highway__internal` 的 `borderTopStyle === "solid"/"dashed"`、`borderTopWidth === "1px"`（当前是 `none`/`0px`）。

### 步骤 3：加 token（**纯新增，不改任何现有引用**）

把 §2 的 token 全部加进 `:root`（新名字），**旧 token 全部保留**。
这一步**不产生任何视觉变化**，可以独立提交、独立验证（`git diff` 只加行）。

验收：`npx tsc --noEmit` + `npx vitest run` 全绿（当前 **51 passed + 1 todo**）；渲染截图无变化。

### 步骤 4：按类别替换引用（**唯一有视觉变化的一步，逐类独立提交**）

顺序按「风险从低到高」：

| 4.x | 替换内容 | 影响面 | 验收 |
|---|---|---|---|
| 4.1 | 语义 tint（ok/warn/danger/info 的 border/bg/text）+ `--on-accent` | 12 处硬编码 | 徽标/提示条截图：色值**完全相同**（这次替换是等值替换） |
| 4.2 | 表面色（14 → 7）：`#0b101a`→`--bg-chrome`、`#0a0e16`→`--bg-sunken`、`#141c2b`/`#1a2333`→`--surface-hover`、`#1b2740`/`#1b2a42`→`--surface-active`、`#131a29`→`--surface-1` | **有视觉变化**（±2–4 灰阶） | 8 页截图逐一对比，重点看 `nav hover` / `list hover` 是否仍能看出状态 |
| 4.3 | 圆角：`5px`→`4px`（segmented）、`3px`→`--radius-xs`、`1px`→`--radius-xs` | 5 处 | 分段控件选中态、用量条、图例色块截图 |
| 4.4 | 字号：11 档 → 5 档，**逐类替换 + 逐类截图** | 113 条声明 | ⚠️ 见下方警告 |
| 4.5 | 间距：68 值 → 8 token | 478 处 px | ⚠️ 影响面最大，**建议最后做，且分页面提交** |
| 4.6 | `--text-faint: #64748b → #7b8da6` | **53 处引用** | 8 页截图；重新跑 `contrast.mjs` 确认全部 ≥4.5:1 |
| 4.7 | 去掉第 1051 / 1080 行的 `opacity`，改用颜色表达层级 | 2 处 | 日志页 debug 行与来源列截图；对比度复算应 ≥4.5:1 |

> ⚠️ **4.4 / 4.5 的正确做法不是全量 sed**：
> `11px → 11.5px`（+4.5%）在拓扑页 `.highway__lane-label`（固定列宽 168px，第 1540 行
> `grid-template-columns: 168px minmax(0,1fr) 168px`）里可能把标签挤换行，
> 而换行会让卡片变高 → 触发拓扑页的**几何重测**（`Topology.tsx` 里依赖 `getBoundingClientRect`）。
> 所以这两步必须**逐页截图核对**，并复跑拓扑页的 `off_line_frac` 探针确认动画没被带偏。
> 这也意味着 **4.4/4.5 应当排在 task-14（前端重构）的稳定期之后**，不要和 `Topology.tsx` 的改动并发。

### 步骤 5：物理拆分文件（**纯搬移，不改规则**）

在 §6 的目录结构下落盘，`styles.css` 变成一个 `@import` 聚合入口（或直接改 `main.tsx` 的 import）。**每拆一个文件提交一次**，每次都跑构建 + 8 页截图对比。

验收：`npx vite build` 产物 CSS 体积**差异 < 1%**（拆分不应该改变任何规则）；生产产物零残留
（`installPreviewBridge` / `__topologyProbe` 各 0 次）。

### 步骤 6：删死代码 + 清理

- `.card` 块（319–325）：**先确认**「设置页以后要不要真的用卡片」。若不要，删块并保留 `card__title`/`card__desc`
  （它们仍在 `Dashboard.tsx:177` 与 `Settings.tsx` 使用）；若要用，把 `.set__sec` 的覆盖去掉。
- `.logs-page` 的 9 个过度限定后代选择器：去掉前缀（降特异性），但**这是行为相关改动**，需单独验证日志页所有行型。
- 未被引用的 CSS 类：**必须用「减去已渲染类名集合」的方式找**，不能靠字面 grep ——
  拓扑页有 `highway__lane-label--${o.kind}` 这类模板字符串拼出来的类名（`Topology.tsx:1599` 等）。
  可以用我的 CDP 探针（`/tmp/xraytun-audit/cdp.mjs` 的 `PROBE`）采集 8 页实际渲染的 class 集合，
  再与 CSS 里的选择器求差集。

---

## 7. 与 `docs/ui/topology/DESIGN-REVIEW.md` 的关系

我读完了那 33 条（647 行）。**大部分不重复**，因为那份是「拓扑/地球仪两页的交互与几何」，
本文件是「全站视觉系统」。以下是**需要 lead 裁决的冲突/修正**：

### 7.1 ⚠️ 冲突 1：它对重复 CSS 的判断是「不值得做」，我认为是**最该先做**

`DESIGN-REVIEW.md` §H1 的原文：

> `.chain` / `.verdict` 在 1698–1780 行与 1902–1984 行各一份；`.globe` / `.facts` 在 1796–1898 行与 2000–2102 行各一份。
> ……**判断：不值得做（本轮）**。零视觉收益，纯维护；但建议在 G1 改动时顺手删掉后一份。

三点修正：
1. **行号已过时**。它测的是 commit `24eee9a`（v0.8.21）。当前是 **1947–2032 / 2177–2262 / 2073–2176 / 2263–2366**，
   共 **190 行**（该报告记的是 4 段共约 186 行）。
2. **「零视觉收益」不准确**：后一份赢层叠，所以**改前一份的任何修改都不会生效**。
   我在 §1.5 引的第 2034–2041 行注释正是它自己记的同类事故 —— **媒体查询那一份已经删了，两侧的 190 行没删**。
   留着它 = 留着同一个坑的其余部分。
3. **「不值得做」的前提变了**：它说「本轮」不值得，因为当时没有设计系统重构。
   现在 task-14 要在**这个文件上**执行 token 收敛 —— 在 190 行重复存在时做 token 替换，
   等于**每一处 token 要改两遍，而只有一处生效**。所以从「谁先做」的角度，它是 task-14 的**前置条件**。

→ **建议**：把它从「不值得做（本轮）」改为**步骤 1，立即做**（§6 步骤 1）。

### 7.2 ⚠️ 冲突 2：`opacity: .3 → .6` **不足以**让拓扑连线达到 3:1

`DESIGN-REVIEW.md` §D2 的建议原文：

> **建议怎么改**：`styles.css` `.flow__route { opacity: .3 }` → **`.6`**（此时经节点 2.84:1、直连 4.14:1、拦截 3.10:1）

我复算的结果：`opacity: .6` 时 **经节点 2.83:1**（该报告自己也写了 2.84 —— 数字一致），
而 **2.83 < 3:1，仍然不达标**；只有直连（4.16）和已拦截（3.11）刚过线。
该报告在下一句也承认了（「只做单项则取 `.5` 并在汇报里注明仍未达 3:1」），但**结论行写的是「此时…」**，
容易被误读成「改到 .6 就达标了」。

→ **修正**：`.6` 是**必要不充分**。要让蓝色「经节点」线达到 3:1，需要 `opacity ≈ 0.64`
（按 2.83/0.6 线性外推约 0.636）；而 §D1 建议的**线型第二编码**（实线/虚线）才是真正解决问题的那条。
**我的立场**：`.6` + D1 的线型 + 合并 C1（去掉回程灰覆盖）三件一起做，且**不要声称已达 3:1**。

### 7.3 ⚠️ 冲突 3：它的 §D3 建议**不要改 `--text-faint` token**，我认为**应该改**

`DESIGN-REVIEW.md` §D3 的原文：

> **建议怎么改**：把这 4 处（`styles.css` 第 1581、1551、1690、1885 行附近）改用 `--text-dim #94a3b8`（6.57:1）。
> **不要**改 `:root` 的 `--text-faint` 本身 —— 它全站 6 页共用，改 token 会波及无关页面。

冲突点：它把「全站共用」当作**反对改 token 的理由**；而这恰恰是**支持改 token 的理由** ——
`--text-faint` 共 **53 处**引用，**51 处**都在 4.5:1 以下（§5.2）。
改 4 处调用点只修好 4 处，剩下 49 处仍不达标。

→ **修正**：改 token 一次修 53 处。按调用点手改是**治标**，且下一轮会有人重新引入不达标的用法。
（它的顾虑「波及无关页面」是真的 —— 但这正是想要的效果：那些页面的 `--text-faint` 同样不达标。）

### 7.4 一致的结论（交叉验证通过，可以放心引用）

| 项 | 它的数字 | 我的复算 | 结论 |
|---|---|---|---|
| `.content` `clientHeight`（1080×720） | 636px | **636px** | 完全一致 |
| 地球仪 `.content` `scrollHeight` | 816px | **816px** | 完全一致 |
| `--text-faint` on `--bg-elevated` | 3.54:1 | **3.54:1** | 完全一致 |
| `--text-faint` on `--bg` | 3.87:1 | **3.87:1** | 完全一致 |
| 彩色线合成对比度 | 1.65–2.20:1 | 1.41–1.92:1（纯 opacity 口径） | 口径不同、结论相同 |
| `.highway__lane-meta` 文字色不达标 | §D3 | §5.2 同 | 一致 |
| F3 地球仪折叠线 | 数据面板在折叠线下 | `.facts top=756 > 636` | 一致 |

**不重复的部分**：它没有涉及字号/间距/圆角 token 化、`--line` 未定义、`.card` 死代码、
`prefers-reduced-motion` 只覆盖 1/4 处、跨 8 页的骨架不一致 —— 这些是本文件的增量。

---

## 8. 未验证 / 边界（诚实清单）

1. **真实 Tauri 应用未验证**。所有测量都在 `?preview=1` 的浏览器预览上。macOS WKWebView 不可 CDP 调试，
   所以**字体渲染、`-webkit-app-region: drag`、Overlay 标题栏的安全区**这三项在真实应用里的表现我没量。
   （`.topbar { padding-top: 34px }` 是为红绿灯按钮留的，我只能从第 60–62 行的注释确认意图。）
2. **CDP 视口 1080×720 是 `Emulation.setDeviceMetricsOverride` 的结果**，不是真实窗口。
   `.content clientHeight = 636` 与 `DESIGN-REVIEW.md` 在真实 Tauri 上量到的 636 **数值相同**，
   但这是巧合还是等价，我没法从预览里证明。
3. **只测了 `state=connected`**。`preview.ts` 支持 `connected/uncommitted/disconnected/no-core/stale/notice`
   六个场景。异常态（`no-core`、`stale`）的视觉/对比度**没测** —— 那些状态会走 `.banner--error`、
   `.empty` 等分支，颜色相同，但**布局**可能不同。
4. **`focus-visible` 样式没测**。我查了全文没有 `:focus-visible` 规则，只有 `input:focus { border-color: var(--accent) }`
   （第 306 行）。**按钮的键盘焦点不可见**是一个**尚未确认**的疑点（可能要单独开任务实测）——
   我把它列为疑点而不是结论，因为 `-webkit-app-region` 和 WebKit 默认 outline 我没实测。
5. **`.logs-page` 去掉过度限定前缀的后果没验证**。这是行为相关改动（特异性变化可能影响层叠），
   我只指出问题，没给出验证过的方案。
6. **间距全量收敛的视觉影响没实测**。§2.4 的映射表是设计推断，不是逐条截图验证过的结论。
7. **色盲模拟只在 `DESIGN-REVIEW.md` §D1 里做过**（拓扑页 4 色），我没有重跑。本文件不声称覆盖色觉障碍。
8. **`.card` 是不是「有意留作将来用」**：我只证明了它当前 100% 不生效，无法判断删除是否会违背原意图，
   所以 §6 步骤 6 把它列为「先确认」而不是「直接删」。
9. **JS 侧 `prefers-reduced-motion` 的具体改法**（货车停中点、地球仪停自转）
   是规格，**没有实现在预览里验证过** —— 实现属 frontend-dev 的范围。

---

## 9. 任务卡五个必答项的落点索引

| 任务卡要求 | 本文位置 | 一句话答案 |
|---|---|---|
| token 全列出 + 使用次数 + 未使用 | §1.1 / §1.2 | 16 个 token、269 处引用；**没有 0 使用的**，但有 3 个 ≤4 次；**2 个 token 被引用却未定义**（`--line`、`--panel`），其中 `--line` 让一条分隔线**完全没渲染** |
| 硬编码值 top 20 | §1.3 | 颜色**基本已 token 化**（22 个值 / 30 处）；真问题是 **478 个 px / 68 个值** 与 **113 条 font-size / 11 个值**。任务卡举例的 `#1a2332` **不存在** |
| 近义重复 | §1.4 | **14 种深色底**（亮度带仅 0.0044–0.0228，其中 5 对 RGB 距离 ≤4.6）；**11 档字号**有 5 档挤在 1.5px 内；7 种圆角里 `5px`/`6px` 差 1px |
| 同名规则块重复核实 | §1.5 | 任务卡给的 4 个数字**只有 `.chain__row` 是真重复**；另外发现**190 行整段重复**（1947–2032 ↔ 2177–2262，2073–2176 ↔ 2263–2366） |
| 动效规范 + `prefers-reduced-motion` | §3 | 2 个 CSS 动画 + 2 处 rAF；**只有 1/4 处理了 reduced-motion**（第 1774 行），CSS 与 JS 都要补 |
| 跨页一致性（具体） | §4 | 6 类具体不一致：页面骨架（3 页不用 `.page`）、区块标题（5 页 5 种写法）、分隔线（14/18/24px + 1 条没渲染）、提示块（`.note` vs `.banner`）、表面色（`select` 差一档）、`.card` 死代码 |
| 对比度实算 | §5 | `--text-faint` **在全部 5 个背景上不达标**（3.26–4.06:1，53 处引用）；加 `opacity` 后最低 **1.85:1**；**所有非文字边界低于 3:1**（1.09–1.78） |
| 拆分方案分几步 | §6 | 6 步：①删 190 行重复（零风险）②修 `--line` ③加 token 不改引用 ④按类别替换（唯一有视觉变化）⑤物理拆文件 ⑥删死代码 |

---

## 10. 给 lead 的一页简报

### 10.1 token 收敛后能删掉多少硬编码值

| 类别 | 现在 | 收敛后 | 减幅 |
|---|---|---|---|
| 深色底/表面色 | **14 种**（4 token + 9 硬编码 + 1 假 token） | **7 种** | **−50%** |
| 语义 tint | **12 个硬编码值**（ok/warn/danger/info × 边/底/字） | 12 个 **token** | 硬编码 **−100%** |
| 圆角值 | **7 种**（含 `5px`/`1px`/`3px` 各 1–2 处） | **4 种** | −43% |
| 字号 | **11 档**（113 条声明） | **5 档** | **−55%** |
| 间距/px | **68 个不同值**（478 处 px） | **8 token + 2 窗口常量** | **−85%** |
| 颜色字面量 | 30 处 hex + 10 处 rgba | 仅剩 `00-tokens.css` 内的定义 + 二维码白底 1 处 | **−97%** |
| 死代码 | `.card` 5 条声明 100% 不生效；`--panel` 假 token | 0 | — |
| 重复行 | **190 行**（7.9%） | 0 | **−190 行** |

### 10.2 对比度不达标的具体位置（全部实算）

| # | 位置 | 实测 | 标准 | 修法 |
|---|---|---|---|---|
| 1 | **`--text-faint #64748b` 全部 53 处引用**（`--bg` 3.87 / elevated 3.54 / input 3.26 / sidebar 4.00 / logs 4.06） | **3.26–4.06:1** | 4.5 | token 改 **`#7b8da6`**（→ 4.58–5.44） |
| 2 | `.logs-page .log-line__src`（1051 行）`--text-faint` × `opacity .7` | **2.56:1** | 4.5 | 去掉 opacity |
| 3 | `.log-line--debug`（1080 行）整行 `opacity .72` | **2.64:1** | 4.5 | 去掉 opacity |
| 4 | `.list__row.is-disabled`（403 行）`opacity .5` | **1.85:1** | 4.5 | 提到 ≥0.7 或改色 |
| 5 | `.btn:disabled`（204 行）`opacity .45` | **3.80:1** | 4.5 | 提高或视为大字号 |
| 6 | `.flow__route` 全部 4 色（1701 行 `opacity .3`） | **1.41–1.92:1** | 3（图形） | `.6` + 线型第二编码 |
| 7 | `.flow__route--trunk`（1711 行 `opacity .22`） | **1.38:1** | 3 | 结构线可豁免，但要**明说** |
| 8 | 全部控件轮廓（`.btn`/`input`/`select` 的 `--border-strong`） | **1.50–1.78:1** | 3 | 拆 `--border-interactive: #5c7290` |
| 9 | `.card` / 列表行 / 区块分隔（`--border`） | 1.19–1.42:1 | 3 | **装饰，建议豁免**（§5.4 方案 C） |
| 10 | `.conn-row` 行分隔（1856 行 `rgba(255,255,255,.04)`） | ≈**1.12:1** | 3 | 改 `--border`（唯一一处白 alpha） |

**另有一条与对比度无关但视觉上看得见的缺陷**：第 1632 行 `.highway__internal` 的分隔线
因 `--line` 未定义而**完全没有渲染**（实测 `borderTopStyle: "none"`）。

### 10.3 我认为最该先动的一步

**删掉第 2177–2366 行的 190 行整段重复**（§6 步骤 1）。

理由，按重要性排序：
1. **它是 task-14 的前置条件**。后一份赢层叠 → 在重复存在时做 token 替换，
   每一处要改两遍、**只有一遍生效**，把「改一处漏一处」的概率翻倍。
2. **零风险、零视觉变化**。删的是层叠里生效的那一份的**副本**，行为等价；
   验收只需要「`git diff` 只有删除」+「8 页截图逐像素相同」。
3. **它是活的隐患，不是历史包袱**。文件自己第 2034–2041 行的注释记录了同类事故，
   而那次**只修了媒体查询那一段，两侧的 190 行没修** —— 相当于拆了炸弹没拆雷管。
4. **收益可量化**：2408 → 约 2180 行（−9.5%），且让后面 5 个步骤的每一处编辑都只作用于一处。

**紧接着（同一批）做步骤 2**：给 `--line` 定义 → 拓扑页那条「出口 / 内部通道」分隔线**立刻出现**。
这是审计里唯一一条「说了要做、实际没做」的缺陷，一行就能修好，且不需要任何设计决策。

**唯一需要你先拍板的**是 §5.4 的控件边界：`--border-strong` 要不要为满足 WCAG 1.4.11 的 3:1
从 `#334155` 提到 `#5c7290`。这会**明显提高按钮和输入框的描边亮度**，
与「ultra-minimal 的弱边界」审美直接冲突 —— 这是取舍，不是技术问题，所以我把它留给你。
我的建议是拆成两个 token（装饰性保持弱、交互控件提到 3:1），代价只有 10 处引用。
