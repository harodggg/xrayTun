# 拓扑 / 地球仪：视觉与交互评审清单

> **审查时间**：2026-09-19 17:31–17:37 CST
> **基线**：commit `24eee9a`（v0.8.21）。截图时 `apps/ui/src/pages/Topology.tsx` 与
> `apps/ui/src/styles.css` **尚无改动**（`git status` 干净），所以下面每一条现象都能在
> v0.8.21 上复现。截图之后（17:38 起）task-2/5/6 开始写文件
> （`topology.rs` / `stats.rs` / `types.ts` / `preview.ts` / `topologyAnimation.test.ts`），
> 但 `Topology.tsx` / `styles.css` 仍未改。
> **本文只新建这一个文件，没有改任何源码。**

## 0. 复现方式

```bash
cd apps/ui && npx vite --port 5173
# 拓扑：http://localhost:5173/?preview=1&state=connected&view=topology
# 地球仪：http://localhost:5173/?preview=1&state=connected&view=globe
```

截图与量数用一份临时的 CDP 脚本（`/tmp/ui-review/shot.mjs`，不在仓库里）：无头 Chrome +
`Emulation.setDeviceMetricsOverride` + `Runtime.evaluate` 注入探针 + `Page.captureScreenshot`。
本文的每个数字都来自这些探针，脚本片段见 §4 附录，可以直接抄进 devtools。

## 1. 一页数字（全是量出来的）

| 量 | 值 | 来源 |
|---|---|---|
| 流动区尺寸 | 820 × 482 px（`.page` max-width 820） | `.flow` 的 `getBoundingClientRect` |
| 彩色支线 | **15 条**（mock：3 入口 × 5 出口；真实配置 3 × 6 = **18 条**） | `opacity === 0.3` 的 `.flow__route` |
| 灰色线 | **21 条** = 3 ×（1 主干 + 5 回程 + 1 回入口） | `opacity === 0.22` 的 `.flow__route` |
| 描边墨迹（长 × 宽） | 彩色 **3 571 px²**（2 976 px 长）／灰色 **6 010 px²**（5 008 px 长） | `getTotalLength() × stroke-width` |
| 货车 | 9 辆，每辆 `rect 9 × 5`，合计 **405 px²** | `.flow__truck rect` |
| 线 : 车 墨迹比 | **23.7 : 1**（仅彩色线也 **8.8 : 1**） | 上两行相除 |
| 扇面（彩色支线）包围盒 | **145 × 277 px**（宽高比 0.52） | 逐条采样 `getPointAtLength` |
| 密度 | 12 × 12 px 单元内最多 **12 条线**；242 个占用单元里 **98 个 ≥ 4 条** | 采样点落入网格计数 |
| 实际渲染的线色 / 对比度 | 经节点 rgb(43,67,108) **1.85:1**；直连 rgb(35,85,82) **2.20:1**；拦截 rgb(88,59,72) **1.87:1**；内部灰 rgb(48,60,79) **1.65:1** | `opacity .3` 再叠回程灰 `rgba(120,160,210,.45)@.22` 的合成值 |
| 图例色块对比度 | 5.73:1（100% 不透明），是它代表的线的 **3.1 倍** | 图例 `#4f8ef7` vs 上一行 |
| 货车颜色与脚下线不符 | **8 / 9 辆**（两次独立采样都是 8/9） | 车 `transform` 与最近线 `stroke` 比对 |
| 隐藏 guide 路径与可见线的偏离 | 最大 **27.5 px**，**25–28.5%** 的行程离任何可见线 > 1.5 px | 密集采样比对 |
| `trucksOnLane` 实际输出 | 9.6 GiB / 352 MiB / 0 B 三条入口 → **3 / 3 / 3 辆** | 见 E3 |
| 地球仪两端屏幕间距 | 广州↔香港 1.163°（129.4 km）→ zoom 1 时 **4.78 CSS px**，zoom 8 时 **38.2 px** | `2R·sin(θ/2)`，R = 0.42×720 |
| 默认窗口（1080×720） | `.content` 可视 636 px，`scrollHeight` **816 px** | `.content` 测量 |

---

## A. 信息层级

### A1　货车是页面上最小的对象，"主视觉留给货车"与实际相反

- **现象**：货车是 `rect x=-4.5 y=-2.5 width=9 height=5 rx=1`（9×5 px）。9 辆车总墨迹 405 px²，
  而 36 条线的描边墨迹 9 581 px² —— **线是车的 23.7 倍**。截图里车看起来像散落的彩色小方块/噪点，
  不像"在路上走的车"。代码注释写的是"线是轨道、货车才是主视觉"（`styles.css` `.flow__route` 上方）。
- **为什么是问题**：这一页的整个比喻是"货车沿公路走"，但视网膜上最抢眼的是 36 条细线；
  用户反馈过的"货车在线上走"其实是**看不出来**车在走 —— 车太小、没有朝向、和线颜色还常不一致（B1）。
- **建议怎么改**：**不是把线调暗**（D2 要求承载意义的线达到 3:1），而是三件事一起做：
  ① 车放大到 `x={-8} y={-4.5} width={16} height={9} rx={2}`；
  ② 15 条重复的灰回程腿不再渲染（C1），描边墨迹从 9 581 → 3 571 + 主干，直接砍掉 40% 的线；
  ③ 无意义的"结构线"（主干）压到 `.flow__route--trunk { opacity: .14; stroke-dasharray: 2 4 }`，
  承载意义的彩色支线**不降反升**到 `opacity: .6`（D2）。
  这样"线多、线暗、车小"变成"线少、有意义的线亮、结构线退后、车大"。
- **影响范围**：`apps/ui/src/pages/Topology.tsx`（Flow 的 rect / 可见路径过滤）、
  `apps/ui/src/styles.css`（`.flow__route`、`.flow__route--trunk`）。
- **判断**：**值得做**。是"18 条线 vs 货车"这条评审重点的正解；单独把线调暗会与 D2 冲突，不要那么做。

### A2　中性灰线的墨迹比"有意义的彩色线"还多 68%

- **现象**：彩色支线 3 571 px²，灰线 6 010 px²。灰色包含两类东西：3 条主干 + **15 条与彩色支线完全重合的回程腿**
  （见 C1）+ 3 条回入口段。
- **为什么是问题**：图例教用户"灰 = 内部通道"，但画面上墨迹最多、最长的灰线既不是内部通道、
  也不承载任何语义（它们只是回程）。视觉权重被没有信息的元素拿走。
- **建议怎么改**：与 C1 同一处修改 —— 回程腿不再作为可见 `<path>` 渲染（`Topology.tsx` 里
  `r.segs.map(sg => <path .../>)` 跳过回程段），主干 `opacity` 降到 0.14 并加 `stroke-dasharray: 2 4`
  明确"这是结构线，不是流向"。
- **影响范围**：`Topology.tsx`（Flow 的可见路径渲染）、`styles.css`（`.flow__route--trunk`）。
- **判断**：**值得做**（与 C1 合并做，等于删代码）。

### A3　页面上最亮的信息是 5 张出口卡片的文字（14.19:1），图表是背景

- **现象**：出口卡片 tag 用 `--text #e6ecf5`（14.19:1），12 px / font-weight 500；而图表里最亮的线只有 2.2:1。
- **为什么是问题**：第一眼落在卡片上，落不到"流量在动"上 —— 与 A1 是同一枚硬币的两面。
- **建议怎么改**：**不改卡片**。理由：卡片里的 ↓↑ 字节数才是用户真正要读的数据，它是页面唯一可精确读取的数字；
  要提升图表权重应该去改 A1/C1/D2，而不是把数据调暗。
- **影响范围**：无（这一条是"确认不改"）。
- **判断**：**不值得做**。

### A4　扇面被挤在右侧 145 px，70% 的横向空隙给了 3 条水平直线

- **现象**：分叉点 `forkX = gapStart + (gapEnd - gapStart) * 0.7`（`Topology.tsx` 第 337 行）。
  实测：入口卡片右缘 x=168、出口卡片左缘 x=652（相对容器），空隙 484 px；
  分叉点落在 507 → **左侧 339 px 只有 3 条水平主干线**，右侧 **145 px 塞了 15 条曲线**。
  扇面包围盒 145 × 277（宽高比 0.52）→ 外侧几条支线几乎是竖线。
- **为什么是问题**：这正是 `docs/ui/topology/README.md` 记过的老毛病（"扇形看着像一堆竖线"，当时量到 4×129）。
  现在仍然是"竖"的（0.52），而左边 70% 的空间空着 —— 空间分配正好反了：汇入只有 3 条线，
  扇出有 15 条曲线。
- **建议怎么改**：`forkX` 的系数从 `0.7` 改到 `0.35`（`gapStart + (gapEnd-gapStart) * 0.35`）。
  分支区宽度从 145 → **315 px**，包围盒变成 315 × 277（宽高比 1.14），扇面横向摊开一倍；
  主干仍有 169 px 够画汇入弧。
- **影响范围**：`Topology.tsx` 第 337 行（一个数字）。
- **判断**：**值得做**。改一个数字，直接改善"连线形状不对 / 看着像竖线"，且不影响动画逻辑。

### A5　出口 tag 被截断成"节点 n1d232c6…"，本图内看不到全文

- **现象**：`shortTag()` 把 `node-n1d232c6b8c7a5004` 截成 `节点 n1d232c6…`，卡片没有 `title`，没有 tooltip。
- **为什么是问题**：多节点时无法从卡片确认是哪一台；但**规则链表格里同一行写着完整 tag**
  （`node-n1d232c6b8c7a5004`），信息没有丢失。
- **建议怎么改**：若要做，成本最低的是给 `<div className="highway__lane-label--...">` 加 `title={o.tag}`。
- **影响范围**：`Topology.tsx` 出口卡片一行。
- **判断**：**不值得做**（本轮）。全文在同页的规则链里能查到，加 `title` 属于锦上添花，
  不值得占用 task-2 的冲突窗口。

---

## B. 颜色语义

### B1　★ 货车走的是一条**几何错误**的隐藏路径：26% 的行程在离线 27 px 的地方；9 辆车 8 辆颜色也和脚下的线不符

- **现象**（三个独立测量，互相印证）：
  1. `.flow__guide`（唯一给车算位置的路径）的 `d` 只有 **1 个 `M`、12 个 `C`**：
     `M 168 101.7 C … 506.8 101.7 C 579.4 101.7,579.4 101.7,652 101.7 C 579.4 101.7,579.4 170.8,652 170.8 …`
     第 2 个 `C`（去第 2 个出口）**从第 1 个出口的终点 (652,101.7) 继续**，而不是从分叉点 (506.8,101.7) 出发。
     而可见支线每条都是自己带 `M` 的：`M 506.8 101.7 C …652 170.8`。
  2. 于是车走的路线和画出来的线不是同一条：guide 上 **25.0–28.5%** 的采样点离**任何**可见线 > 1.5 px，
     最大偏离 **27.5 px**（`at [636,268]`，那里正好是两条支线之间的空白）。9 辆车离 guide 都 ≤ 0.68 px
     —— 车严格走 guide，所以**车有 1/4 的时间在空白处飞行**。
  3. 颜色：实测 **8/9** 辆车的 `fill` 与它脚下最近线的 `stroke` 不一致。例：脚下是 `#64748b`（内部灰）
     的车涂成 `#f87171`（已拦截红）；脚下是 `#34d399`（直连绿）的车涂成 `#4f8ef7`（经节点蓝）。
     原因：`trunkFrac()` 用 `segs 总长 / 2` 近似"去程占比"，分支起点比例也按**设想几何**算，
     与 `getPointAtLength` 的真实落点无关。
- **为什么是问题**：这是本次评审最严重的一条，一次串起三个用户反馈：
  ①"货车在线上走"（其实 1/4 时间不在线上）；②"颜色语义不清"（车色和线色对不上，
  图例的第一条承诺就破产了）；③信息层级（车是最小、最不准的元素）。
  另外，这条也影响 task-2 的"乱跳"定位：几何一重测，这条被扭坏的链式路径会整体变形，
  同一进度对应的屏幕位置变化远大于"路径长度 ±30%"的理想模型。
- **建议怎么改**（二选一，推荐 a）：
  - **a. 让 guide 与可见线同构**：把 route 构造改成"每条支线前面先补一段回分叉点的回程"，
    即 `[trunk, branch0, back0, branch1, back1, …, branchN, backN, backToInlet]`，
    并让 `routeToD()` 对每个 seg 都输出 `M`（`segs.map(s => routeToD([s])).join(" ")` 等价）。
    **注意**：不能只把 guide 改成多个 `M` 子路径 —— `getPointAtLength` 在多子路径之间是瞬移的
    （`totalLength` 不计子路径间距），车会在出口处闪回分叉点。必须显式补出"回程"这一段几何。
  - **b. 退一步**：把车改为"只沿**单条**支线来回"（一条路线 = 入口 → 分叉 → 某出口 → 原路返回），
    路线数与"入口 × 出口"解耦。
  两种改法都同时修掉颜色：着色判据改成"当前 seg 的区间"，即给每个 seg 预存
  `[startLen, endLen]`（用 `route.segs` 累加，或直接用 `guide.getTotalLength()` 归一），
  车的颜色 = 包含当前 `u * total` 的 seg 的颜色；不再用 `trunkFrac()` 猜。
- **影响范围**：`apps/ui/src/pages/Topology.tsx`（`measure()` 里 segs 的构造、`routeToD`、
  `trunkFrac`、动画 effect 里的着色分支）。`styles.css` 不用改。
- **判断**：**值得做（优先级最高）**。它不是"更好看"，而是"当前画出来的东西和车的运动不是同一件事"。
  即使只想先止血，也可以只做"每条支线后补一段回程"这一处（不动颜色），能立刻消掉 27 px 离线。

### B2　图例色块与实际线不是一种"东西"：亮度差 3.1 倍

- **现象**：`.highway__legend-dot` 是 9 × 5 px、100% 不透明的色块（经节点 5.73:1）；
  实际线的合成对比度只有 1.65–2.20:1（见 §1），且线宽 1.2 px、再被灰色回程覆盖（C1）。
- **为什么是问题**：图例的任务是"让你在图上认出这个颜色"。取色器量一下会发现两个像素不是一回事：
  图例上是鲜亮的 `#4f8ef7`，图上是一条 1.85:1 的暗蓝灰。用户会觉得"图例上那个蓝在图里找不到"。
- **建议怎么改**：把色块改成"线段的缩影"：
  `.highway__legend-dot { width: 18px; height: 0; border-top: 2px solid currentColor; background: none !important; border-radius: 0 }`，
  并用 `style={{ color: OUTBOUND_COLOR.node }}` 传色（而不是 `background`）。
  这样图例同时表达了**形状（线）**和颜色。
- **影响范围**：`styles.css`（`.highway__legend-dot`）+ `Topology.tsx` 图例 4 处的 `style`。
- **判断**：**值得做**。改动小，而且随手把 D1 要加的线型也放进图例就有地方放了。

### B3　"内部通道灰"和主干/回程灰几乎是同一个颜色

- **现象**：内部灰线合成色 rgb(48,60,79)，主干 rgb(38,51,71) —— 只差约 11 级；
  而图例第 4 项写着"内部通道 = 灰"。画面上大多数灰线（主干 + 15 条回程）**不是**内部通道。
- **为什么是问题**：图例在给一个颜色指派一个它不独占的含义，用户会把主干线误读成 dns/api 通道。
- **建议怎么改**：两件事一起做 ——
  ① 图例文案改成"内部通道（dns / api）"；
  ② 主干从"同一种灰"里分出来：`.flow__route--trunk { opacity: .14; stroke-dasharray: 2 4 }`，
  颜色仍用 `rgba(120,160,210,.45)`。虚线的"结构线"与实线的"内部通道"一眼可分。
- **影响范围**：`Topology.tsx`（图例文案）、`styles.css`（`.flow__route--trunk`、`.flow__route` 的 `stroke-dasharray` 默认值）。
- **判断**：**值得做**。与 C1/A2 同一处修改，边际成本几乎为零。

### B4　出口卡片的颜色条画在**背离连线**的一侧

- **现象**：`.highway__side--right .highway__lane-label--node { box-shadow: inset -2px 0 0 var(--accent) }`
  —— `-2px` 是**右边缘**。而扇形线全部落在卡片的**左边缘**（实测分支 `x2 = 652` = 出口卡片左缘）。
- **为什么是问题**：颜色本来是"线的去向"的重复编码，现在它出现在离落点 168 px 的另一头。
  用户的视线从线端扫到卡片时，颜色线索在远端，不能在线落下的那一刻闭合。
- **建议怎么改**：右列的 4 条规则改成 `inset 2px 0 0 <色>`（左边缘），即把
  `--node / --direct / --block / --dns,--internal` 四条的 `-2px` 改成 `2px`；或者两侧都画
  （`inset 2px 0 0 var(--accent), inset -2px 0 0 var(--accent)`）。注意窄窗媒体查询里那条
  `box-shadow: inset 2px 0 0 currentColor` 是 bug，见 G1。
- **影响范围**：`styles.css`（第 1599–1611 行 4 条规则）。
- **判断**：**值得做**。一行 CSS，把"线 → 卡片"的颜色对应收在落点上。

---

## C. 密度

### C1　每条彩色支线都被一条 100% 重合的灰线盖住

- **现象**：21 条灰线里 **15 条**的采样点 100% 落在某条彩色线 1 px 之内（另外 6 条是主干，占比 2%）。
  这 15 条就是回程腿 —— 它们与去程支线是**同一条贝塞尔曲线的反向**，画在彩色线**上面**
  （同一 `<g>` 里 DOM 顺序靠后）。合成后：`#4f8ef7@.3` rgb(34,57,97) → 叠灰后 rgb(43,67,108)。
- **为什么是问题**：① 颜色被灰"洗"了一道（rgb(34,57,97) → rgb(43,67,108)，色相往灰上偏，
  蓝/绿/红三色一起变浑），拿掉它才是设计时的那个颜色；注意它同时把亮度**抬**高了一点，
  所以 C1 和 D2 必须一起调，单做 C1 会让线变暗；
  ② 描边墨迹凭空翻倍（3 571 → 9 581 px²），直接造成 A2 的"灰线比彩线多"；
  ③ 车在这条重合线上走时，"有去有回"这个视觉信息实际上是不可见的（两条线完全重叠）。
- **建议怎么改**：`Topology.tsx` 里渲染可见路径时跳过回程 seg（`sg.color === undefined` 且属于
  回程区间的段），或者给回程 seg 也用与去程相同的 `color`。guide 仍保留完整 `d`（B1 修完后）。
- **影响范围**：`Topology.tsx`（`geo.routes.map` 里的 segs 渲染，第 529–540 行）。
- **判断**：**值得做**。这是"删代码"型修改，同时改善 A2 和 D2。

### C2　最密处 12 × 12 px 里有 12 条线；而且**现在无法 hover 隔离**

- **现象**：按 12 × 12 px 网格统计，占用单元 242 个，其中 **98 个 ≥ 4 条线**，**最大值 12 条**。
  想用 hover 只看一条也不行：`.flow { pointer-events: none }`（styles.css 第 1662 行），
  且全文没有 `.flow__route:hover` 规则。
- **为什么是问题**：3 × 6 = 18 条彩色支线（真实配置）在一个 820 × 482 的区域里两两交叉，
  最密处肉眼分不出哪条去哪；而唯一能救它的交互（悬停高亮）被 `pointer-events: none` 关掉了。
- **建议怎么改**（按性价比排序）：
  1. **hover 隔离（小，值得做）**：给每条 `<path className="flow__route">` 加
     `pointer-events: stroke`（`pointer-events` 可被子元素覆盖，父级 `.flow` 的 `none` 保留没问题）；
     CSS 追加 `.flow__route:hover { opacity: 1; stroke-width: 2.5 }` 与
     `svg.flow:has(.flow__route:hover) .flow__route:not(:hover) { opacity: .08 }`。
     `:has()` 需要 Safari 15.4+ / Chrome 105+（macOS 上的 WKWebView 已支持）；
     若要零风险，用 React state 在 `onPointerEnter` 里记 `hoverKey` 代替 `:has()`。
     车的颜色条（`.flow__truck rect`）不受影响。
  2. **按入口展开（大，值得做但要单独一轮）**：点入口卡片 → 只画该入口扇出的 6 条，
     默认只画 3 条主干 + 汇总。这一条能真把 18 条降到 6 条，但属于交互改动，
     要和 task-2 的动画修复**分开**做（改的是同一段渲染）。
  3. **不值得做的**：把线整体再调暗。那只是把"看不见"换成"更看不见"。
- **影响范围**：`.flow` / `.flow__route`（styles.css）、`Topology.tsx`（hover/选中状态 + 渲染过滤）。
- **判断**：**hover 值得做**（本轮就能做，不动动画）；**"按入口展开"值得做但要另开任务**。

### C3　18 条线表达的是"结构上可能"，不是"实际发生"，但画得和实测流量一样实

- **现象**：`README` 已经写明连接来自配置、核心没有 per-rule 计数器；但 18 条支线的
  `opacity .3` 一律相同，`block`（↓0 B）和 `node`（↓8.01 GiB）的线一样实在。
- **为什么是问题**：用户会把线的存在读成"有流量走这里"。出口卡片上的字节数其实已经回答了
  "哪些出口在用"，但不看数字的话图上分不出"结构存在"与"真的在走"。
- **建议怎么改**：按出口字节数分层（在 D2 的基线上再分）：有流量
  （`o.uplink_bytes + o.downlink_bytes > 0`）的支线保持 D2 的 `opacity: .6`、
  无流量的降到 `.15; stroke-dasharray: 2 3`（并在图例里加一句
  "虚线 = 结构上可能，暂无流量"）。这是把 C2 的"密度"换成"信息"。
- **影响范围**：`Topology.tsx`（构建 segs 时按出口字节数给 `color` 之外再加一个 `dim`），
  `styles.css` 新增一条类。
- **判断**：**部分值得做**。口径需要 lead 拍板：`README` 明确声明"不声称这辆车的货去了哪个出口"，
  所以如果按出口字节数给**支线**加权重，等于把"出口总量"投影到"入口→出口"这条边上，
  仍然不是真实归属。我倾向于**做**，但必须同步改 `page__desc` 说明这是"按出口总量加权的结构图"。

---

## D. 可访问性

### D1　只靠颜色区分不够：色盲模拟下"直连绿"和"内部灰"距离只有 12/255

- **现象**：把 §1 的**实际渲染色**做 deuteranopia 模拟后两两距离（0–255 欧氏）：
  直连↔内部 **12**、经节点↔直连 **17**、经节点↔内部 **24**、已拦截↔其它 **35–49**。
  protanopia 同量级（直连↔内部 14）。也就是说：红/绿/蓝/灰四色里，**只有"已拦截"还能认出来**，
  其余三色对红绿色盲用户基本是同一个暗蓝灰。
- **为什么是问题**：颜色是这张图唯一的分类编码。虽然出口卡片上写着 `kind` 文字，
  但"这条线通向哪一类出口"在线上是纯颜色。
- **建议怎么改**：加**线型**作为第二编码（颜色全部保留）：
  `node: 实线`、`direct: stroke-dasharray: 5 3`、`block: stroke-dasharray: 1.5 3`、
  `dns/internal: stroke-dasharray: 1 4`。实现上给 `<path>` 加
  `` className={`flow__route flow__route--${outbound[k].kind}`} ``，在 `styles.css` 写 4 条规则
  （`.flow__route--direct { stroke-dasharray: 5 3 }` …）。图例的 B2 改造后同色同线型。
- **影响范围**：`Topology.tsx`（path 的 className + 图例）、`styles.css`（4 条规则）。
- **判断**：**值得做**。这是成本最低、唯一真正的非颜色编码；不做的话"色盲用户能不能用"的答案是"不能"。

### D2　彩色线的对比度 1.85–2.20:1，低于 WCAG 1.4.11 对"有意义图形"的 3:1

- **现象**：实际渲染色（含回程灰覆盖）见 §1：经节点 1.85:1、直连 2.20:1、拦截 1.87:1、内部灰 1.65:1。
  只算设计值（`opacity .3` 直接叠在背景上）是 1.59 / 1.93 / 1.63 / 1.40。
  算了一下达到 3:1 需要多少 `opacity`：**经节点 ≈ 0.63、已拦截 ≈ 0.59、直连 ≈ 0.48、内部灰 ≈ 0.87**。
- **为什么是问题**：图形对象（这里的线承载"去向"信息）在 3:1 以下，在低亮度屏/强光下会糊；
  这也是 A1/C1 想解决却解决不了的根因之一（线的颜色本身就不够亮）。
- **建议怎么改**：`styles.css` `.flow__route { opacity: .3 }` → **`.6`**（此时经节点 2.84:1、
  直连 4.14:1、拦截 3.10:1）。两条配套约束：
  ① 必须与 C1（去掉回程灰覆盖）和 A1（车放大）一起做，否则 18 条线会糊成一片；
  ② **内部灰不要靠 opac 追 3:1** —— 它要到 0.87 才达标，那就和"经节点"一样抢眼了，
  与"内部通道是次要的"相矛盾；它的可辨识度交给 D1 的虚线 + 卡片上的 `kind` 文字。
- **影响范围**：`styles.css` 一处 + 与 C1、A1 的联动。
- **判断**：**值得做**（数值取舍需要一个数字，但要跟 A1/C1 绑定评审；只做单项则取 `.5` 并在汇报里注明仍未达 3:1）。

### D3　`--text-faint` 的小字号文本只有 3.54–3.87:1

- **现象**：`--text-faint #64748b` 用在卡片底（#161d2c）上 = **3.54:1**，用在页面底 = **3.87:1**；
  涉及 `.highway__lane-meta`（10.5 px，协议 / kind）、`.highway__side-title`（11 px）、
  `.highway__legend-note`（10.5 px）、`.facts__hint` / `.fact__src`（10.5 px）。
  WCAG AA 小字要求 4.5:1。
- **为什么是问题**：出口卡片的 `node / direct / block / dns / internal` 就写在 `.highway__lane-meta` 里，
  它是颜色之外唯一说明"这一类是什么"的文字，却最不清楚。
- **建议怎么改**：把这 4 处（`styles.css` 第 1581、1551、1690、1885 行附近）改用
  `--text-dim #94a3b8`（6.57:1）。**不要**改 `:root` 的 `--text-faint` 本身 —— 它全站 6 页共用，
  改 token 会波及无关页面。
- **影响范围**：`styles.css` 4 条规则（不要动 `:root`）。
- **判断**：**值得做**。零风险，且 `.highway__lane-meta` 正好是 D1 线型编码的对照标签。

### D4　`.flow` 是 `aria-hidden`，图内没有文本替代

- **现象**：`<svg className="flow" aria-hidden>`（实测 `aria-hidden="true"`），无 `role`、无 `<title>`。
- **为什么是问题**：严格说屏幕阅读器读不到这张图。
- **建议怎么改**：若要做，加 `role="img"` + `<title>入口到出口的扇出关系…</title>`。
- **影响范围**：`Topology.tsx` SVG 一个属性。
- **判断**：**不值得做**。同一份信息在 4 张入口卡、6 张出口卡、规则链表格里都有 DOM 文本，
  屏幕阅读器不丢数据；图本身是"结构可视化"。要守住的红线是：**以后不要把数据只画进 SVG**。

---

## E. 空态 / 异常态

### E1　`traffic_error` 非空时，卡片照旧显示字节数，和"不画假流量"的文案矛盾

- **现象**：我用 CDP 覆盖 `invoke` 注入 `traffic_error: "dial unix …: no such file or directory"`
  后截图：卡片仍然显示 `tun ↓7.84 GiB ↑1.12 GiB`、`节点 ↓8.01 GiB ↑1.16 GiB`，
  而图表**下方**的 `.note` 写着"取不到实时流量…（这里不画 0 字节的假流量）"。
  在 task-5 修之前，真实场景里这些字段是 0，卡片会显示 `↓0 B ↑0 B` —— 正好就是那句文案否认的"假 0"。
- **为什么是问题**：免责声明与画面互相打脸；数字不可信时，用户没有**在数字上**看到任何标记，
  要滚到下面才读到说明。异常态的核心是"别让人误信"。
- **建议怎么改**：
  1. `formatBytes` 的调用点加不可用分支，渲染 `—`（em dash）而不是 `0 B`，例如
     `const bytes = (v: number | null) => (v == null ? "—" : formatBytes(v))`；
     配合 task-5 把 `uplink_bytes/downlink_bytes` 改成 `Option<u64>`（或按 `traffic_ok` 判断）。
  2. `.highway__lane-bytes` 在不可用时加 `color: var(--text-faint)`（这里的暗色是有意的：
     它不表示"小字数据"，而是"没有数据"；与 D3 要提高 `--text-faint` 的场景不冲突）。
  3. 把 `{topo.traffic_error && <div className="note">…}` 从 `<Highway>` **后面**移到**前面**
     （`Topology.tsx` 第 65–72 行 → 放到第 65 行之前），用户先看到"流量不可用"再看数字。
     若要更强，用 `.banner--warn`（黄）而不是中性 `.note`。
- **影响范围**：`Topology.tsx`（卡片渲染 + 提示位置）、`apps/ui/src/types.ts`（task-5 的字段形状）。
- **判断**：**值得做**。这是"空态是否清楚"的正题，也是 task-5 改字段后前端必须接的一半工作。

### E2　流量不可用时车还在跑；而且"0 流量"和"9 GiB"画面上完全一样

- **现象**：`trucksOnLane(0)` 明确返回 3（注释："空车道也画几辆"），而 `trucksOnLane(>1 MiB)` 也是 3（见 E3）。
  实测 9.6 GiB 的三条入口各 3 辆 = 9 辆。地球仪同理：`vehicleCount(0)` 返回 1 → 没有流量也有一架飞机在飞。
- **为什么是问题**："在动"是这个页面唯一的活性信号，而它和真实流量无关。核心没跑、流量查不到时，
  画面依然繁忙 —— 用户会得到"正在传输"的错误结论。
- **建议怎么改**：`traffic_error` 非空（或字节字段为不可用）时，车改为"空心 + 静止"：
  `.flow__truck--unknown rect { fill: none; stroke: var(--text-faint); stroke-dasharray: 2 2 }`，
  并且不启动位置动画（effect 里 `if (trafficUnavailable) return`）。地球仪的 `vehicleCount(0)` 改为 0 架
  （只画弧线 + 标记），或同样给"未知"样式。
- **影响范围**：`Topology.tsx`（truck 渲染 + 动画 effect 开头）、`Globe.tsx`（`vehicleCount`）。
- **判断**：**值得做**（低层改动小；但它和 task-2 改的是同一个 effect，需要排序）。

### E3　★ `trucksOnLane` 里 `1 << 40` 溢出 32 位，货车数量恒等于 3

- **现象**：`const hi = Math.log10(1 << 40)`。JS 的 `<<` 是 32 位运算，**`1 << 40 === 256`**
  → `hi = 2.408 < lo = log10(1<<20) = 6.021`，`t` 为负被 `Math.max(0, …)` 夹到 0。
  实测输出：`0 B → 3 辆`、`1 KiB → 7 辆`、`1 MiB → 3`、`352 MiB → 3`、`9.6 GiB → 3`、`1 TiB → 3`。
  即**映射是反的**（流量越小车越多），而且对任何真实流量都恒为 3 辆。
- **为什么是问题**：页面文案承诺"车辆数量由实测速率决定"，实际是一个常数 —— 功能声明是假的；
  也解释了 A1 里"9 辆车"这个数字为什么和流量无关。
- **建议怎么改**：`const hi = Math.log10(2 ** 40)`（或直接 `1e12`）。一行。修完记得复测
  `trucksOnLane` 的端点：0 B → 3、1 MiB → 3、1 GiB → 6、1 TiB → 8。
- **影响范围**：`Topology.tsx` 第 228 行。
- **判断**：**值得做（一行，优先级最高的三项之一）**。

### E4　文案说"速率"，代码用的是"累计字节"

- **现象**：`page__desc` 写"车辆数量由实测速率决定"，但代码传入的是
  `downlink_bytes + uplink_bytes`（累计值，只增不减）。地球仪页同样写"飞机的数量与快慢由实测速率决定"。
- **为什么是问题**：累计值随会话时间单调增长，车数会因为"攒够了"而变化，与"现在跑多快"无关；
  用户会把车多读成"正在大量传输"。对一个网络工具来说这是结论性误导。
- **建议怎么改**：二选一 —— ① 改文案为"车辆数量由**累计流量**决定"（两页都改）；
  ② 真用速率：从快照的 `traffic.rx_rate / tx_rate` 传入。我建议 **①**（`Topology` 的 props 只有
  `Topology` 数据，速率不在里面，取速率要动数据流，成本大）。
- **影响范围**：`Topology.tsx` 第 63 行文案、`Globe.tsx` 第 109 行文案。
- **判断**：**值得做**（一句话，消除一个假声明）。若选 ② 则值得做但要单开任务。

### E5　取拓扑失败时：中性灰提示 + 无重试 + 不说明"在自动重试"

- **现象**：`loadError` 分支（`Topology.tsx` 44–54 行）渲染一个 `.note`
  （`border-left: 2px solid var(--border-strong)`，中性灰）。实测截图：整页只有左上角一块灰提示，
  其余约 85% 空白，没有 `.page__title`、没有重试按钮、也没有写"每 2 秒会自动重试"
  （其实 `setInterval` 一直在重试）。同类失败在 `Globe.tsx` 用 `.banner.banner--error`（红底 ✕，
  实测 `#2a161a` / `#f8c9cd`）并配"重新定位"按钮。
- **为什么是问题**：同一个"拿不到数据"的失败，在拓扑页比在地球仪页弱一个等级；
  用户看到空白页无法判断"是在重试、还是要我做什么"。
- **建议怎么改**：改成
  `<div className="banner banner--error"><span>✕</span><div>取拓扑失败：{loadError}<br/>核心没在跑时读不到运行中的配置。每 2 秒自动重试。</div></div>`
  并在标题行给一个 `<button className="btn btn--ghost" onClick={() => void load()}>重试</button>`
  （放在 `.page__sec` 里的 `.row--between`，与地球仪一致）。
- **影响范围**：`Topology.tsx` 44–54 行。
- **判断**：**值得做**（低成本，且是"跨页一致性"的实项）。

### E6　空拓扑（`outbound: []`）时画出空图例 + 空白图，没有任何说明

- **现象**：注入 `outbound: []`（入口只剩 tun + api）后实测：图例照旧宣传 4 种颜色，
  左侧一张 `tun ↓0 B ↑0 B`、两个"合计 出入 0 B"，中间一片空白，一条线都没有，也没有提示。
- **为什么是问题**：图例在承诺图里存在的东西；空态却什么也没说，用户会以为"图没加载出来"。
- **建议怎么改**：`Highway` 里加早退分支：`lanes.length === 0 || topo.outbound.length === 0`
  时渲染 `<div className="empty">运行中的配置里没有出口（或入口）—— 核心可能没在运行</div>`，
  并隐藏图例（`{topo.outbound.length > 0 && <div className="highway__legend">…}`）。
- **影响范围**：`Topology.tsx`（`Highway` 开头 + 图例条件）。
- **判断**：**值得做**（约 6 行，直接对应"空态的呈现是否清楚"）。

### E7　预览里**复现不出**这三种异常态，评审只能靠临时脚本

- **现象**：`preview.ts` 的 `MOCK_TOPOLOGY.traffic_error` 恒为 `null`（第 319 行），
  `globe_data` 恒返回 `route`；`scenarioSnapshot()` 只有
  `connected/uncommitted/disconnected/no-core/stale/notice` 六种，都改不到拓扑页。我这次是用 CDP
  在运行时覆盖 `window.__TAURI_INTERNALS__.invoke` 才拍到 E1/E5/E6 三张图。
- **为什么是问题**：异常态是这个页面最需要被反复检查的部分（用户报的正是"数据乱跳"），
  却是唯一没法一键复现的部分。下一轮评审还得重写一遍我这套脚本。
- **建议怎么改**：`scenarioSnapshot()` 加两个 case：`traffic-error`
  （返回 `{...MOCK_TOPOLOGY, traffic_error: "dial unix … connect: no such file or directory",
  uplink_bytes: null, downlink_bytes: null}`）与 `empty-topology`
  （返回 `{inbound:[tun, api], outbound:[], rule:[], traffic_error:null, geo_available:false}`）。
- **影响范围**：`apps/ui/src/preview.ts`（task-6 的写入范围，与自检探针同一个文件）。
- **判断**：**值得做**。我给出上面的确切断言与参数，task-6 可以直接照抄。

---

## F. 与地球仪的一致性

### F1　同一个东西（在动的字节）在两页是两种视觉语言

- **现象**：拓扑页的车 = 9 × 5 px 实心方块 + 0.8 px 深描边，**没有朝向**；地球仪的飞机 = 7 px 三角
  + 22 px 渐变尾迹，按航向旋转（`drawPlane`）。地球仪上明显更"像一个东西在动"。
- **为什么是问题**：用户要在两页之间建立同一个心智模型（"流量的载体"），现在一个是噪点、一个是飞机；
  而且拓扑页的车没有朝向，"从入口开到出口"这个方向性完全没有表达。
- **建议怎么改**：给车加朝向 —— 每帧除了 `translate` 再写 `rotate(deg)`，
  用路径前后两点（`getPointAtLength(u ± 0.005)`）算切线角，同 `drawPlane` 的做法；
  形状用 `<rect>` 太钝，可换成 `<path d="M 7 0 L -4 3.5 L -4 -3.5 Z">`（箭头/车头，
  尺寸与 A1 的放大值一致）。
- **影响范围**：`Topology.tsx`（车形状 + 动画 effect 的 transform）。
- **判断**：**值得做**（"货车在线上走"的比喻核心需要朝向；不过它和 task-2 改同一个 effect，
  必须排在 task-2 之后或并进 task-2）。

### F2　★ 地球仪默认视角下，近距离航线的**航线、飞机、标记全都看不见**

- **现象**：`?state=connected` 的广州(23.1317, 113.266) ↔ 香港(22.3193, 114.169)，角距 **1.163°（129.4 km）**。
  正交投影下两点屏幕间距 = `2R·sin(θ/2)`，`R = 0.42 × min(720,720) = 302 canvas px`：
  **zoom 1 时 6.14 canvas px = 4.78 CSS px**；即使推到 `MAX_ZOOM = 8` 也只有 **38.2 CSS px**。
  而两个标签框各约 110 × 30 px、飞机 7 px + 22 px 尾迹，全部压在这 4.78 px 上。
  6× 放大截图显示：绿色起点标记与蓝色节点标记**圆心重合**，两个标签框把航线整段盖住，
  一架飞机都看不见。`fitZoom()` 对近距航线 `return clampZoom(Math.min(1, 0.7/sin(θ/2)))` → 恒为 1。
- **为什么是问题**：这一页的主体就是那条航线；默认窗口下主体不可见（而近节点正是最常见的情形）。
  任务背景里用户反馈的"放大倍数不足"在这里有一个可复现的数字根因：
  要把两端拉开到 200 CSS px，需要 zoom ≈ **41.9**。
- **建议怎么改**（三条一起，成本都低）：
  1. `fitZoom()` 给近距航线一个**屏幕间距下限**：除"装得下"以外再要求
     `2R·sin(θ/2)·zoom ≥ 120`（即 `zoom ≥ 120 / (2R sin(θ/2))`）。对这条 129 km 航线算出 **zoom ≈ 25.1**；
     因此自动缩放要有一个独立上限（如 `MAX_AUTO_ZOOM = 28`），`MAX_ZOOM` 从 8 提到 **48** 左右，
     否则用户手动也拉不开。**代价要说清**：2° 海陆位图在这个放大倍数下海岸线会明显发虚
     （`Globe.tsx` 的注释和 `docs/ui/globe/README.md` 都已经声明过是粗略示意），
     换来的正是"看得见从哪飞到哪"。
  2. `drawScene()` 里把 `marker()` 两次调用**移到** `drawPlane()` 循环**之前**（现在飞机先画、
     标签后画 → 标签盖住飞机），并让标签优先往航线的**垂线方向**偏移，别横在航线上。
  3. 同步 `docs/ui/globe/README.md`：那里写"上限 1.8 倍"，与代码的 `MAX_ZOOM = 8` 已经不一致。
- **影响范围**：`Globe.tsx`（`fitZoom`、`MAX_ZOOM`、`drawScene` 的绘制顺序）、`docs/ui/globe/README.md`。
- **判断**：**值得做**。这是唯一一条"默认状态下主体完全不可见"的缺陷，且不需要新依赖。

### F3　默认窗口（1080 × 720）下，地球仪的事实面板和操作提示都在折叠线以下

- **现象**：`tauri.conf.json` 默认 `width 1080 / height 720`。实测：`.content` 可视高 **636 px**，
  `scrollHeight` **816 px**；`.globe__canvas` 是 560 × 560（占可视高的 **88%**），
  于是 `.facts`（起点 / 129 km / 9.17 GiB / 出口）整块在折叠线以下，
  `.globe__hint`（"滚轮缩放（最多 8×）· 拖动旋转"）也被切掉（截图里只剩 +/− / 复位三个按钮）。
- **为什么是问题**：首屏只有球和两个标签，看不到任何数字；操作提示不可见，新用户不知道能缩放/拖动。
- **建议怎么改**：`.globe__canvas { width: min(100%, 560px); max-height: calc(100vh - 300px) }`
  （或 `width: min(100%, 460px, calc(100vh - 320px))`），并把 `.globe__hint`
  从"绝对定位在右下"改成紧贴画布下方的一行小字（`position: static; text-align: right`），
  保证它永远在可视区内。
- **影响范围**：`styles.css`（`.globe__canvas`、`.globe__hint`）。
- **判断**：**值得做**（首屏看不到数据 + 看不到操作提示，是可用性硬伤）。

### F4　地球仪把"129 km"当主数字，流量是次要信息 —— 和拓扑页的重点相反

- **现象**：`.facts__km`（大圆距离）是 15 px / 粗体 / `--accent`；同一张卡片里
  "9.17 GiB 出口累计流量（实测）"用的是 `.facts__km--small`（12 px / 普通字重）。
- **为什么是问题**：整个应用的其他地方（拓扑页、顶栏）都把**字节**当主指标；距离在这个工具里
  是最不具行动价值的一个数（不可改变、和代理质量无关）。
- **建议怎么改**：两级互换 —— 流量用 `.facts__km`（15 px / `--accent`），
  距离用 `.facts__km--small`；或至少给两个数相同的字号与颜色权重，让"标签"（大圆距离 / 出口累计流量）
  承担区分。改 `Globe.tsx` 的 `RouteFacts` 两行 class 顺序即可。
- **影响范围**：`Globe.tsx`（`RouteFacts`）、`styles.css`（不需要改）。
- **判断**：**值得做**（两行；跨页主指标一致）。

### F5　绿/蓝在两页的含义不同（绿 = 直连 vs 绿 = 本机）

- **现象**：拓扑页 `direct = #34d399`（绿）；地球仪起点标记 `#34d399`（绿），节点标记 `#4f8ef7`（蓝）
  = 拓扑页的 `node`。
- **为什么是问题**：严格说颜色语义没有统一（同一色在不同页指不同对象）。
- **建议怎么改**：可选做法是在地球仪加图例，或在 `.facts` 的"起点"标签里写"本机（绿）"。
- **影响范围**：`Globe.tsx`。
- **判断**：**不值得做**。绿在两页都可以读成"不经过节点的本地/直连一侧"，语义大体自洽；
  地球仪的标签文字（"本机 · IP"、"Xray-45.207.197.185"）已经把颜色说清楚了，加图例是重复。

### F6　两页的"线 vs 移动体"权重正好相反

- **现象**：地球仪的航线是 `rgba(79,142,247,0.6)`（2.83:1），飞机是 `#dbeafe`（近白）；
  拓扑页的线 0.3/0.22、车是 `#4f8ef7` 等中亮色，且线在数量与墨迹上压过车（A1/A2）。
- **为什么是问题**：同一个人切换两页时，"什么在动"的可读性不一致；地球仪一眼看到飞机，
  拓扑页要盯着找。
- **建议怎么改**：与 A1/C1/D2 是同一组动作 —— 砍掉重复的回程灰线、主干压到 `.14`、
  彩色支线提到 `.6`、车放大到 16×9 并加朝向。不需要为地球仪改。
- **影响范围**：`styles.css`（`.flow__route` / `.flow__route--trunk`）。
- **判断**：**值得做**（不额外产生工作，属于 A1 的验收标准之一）。

---

## G. 响应式

### G1　窗口在 900–939 px 时拓扑图**整个塌掉**，且色条变白

- **现象**：`tauri.conf.json` 的 `minWidth: 900`。当视口 ≤ 940 px 时
  `@media (max-width: 940px)` 把 `.highway` 改成 `grid-template-columns: 1fr`，
  但 `.highway__side--right` 仍然写着 `grid-column: 3` → 浏览器生成**隐式列**。
  实测 900 px 视口下 `grid-template-columns: 501.141px 0px 150.859px`：
  **入口卡片被拉到 501 px 宽、流动区塌成 0 px**（`forkX`/`gapEnd` 全挤在一点），
  15 条彩色线**一条都画不出来**（截图里扇形区完全空白）；出口卡片被压到 151 px。
  同一媒体查询里 `.highway__side--right .highway__lane-label { box-shadow: inset 2px 0 0 currentColor }`
  让 5 张卡片的色条全部变成 `currentColor` = `rgb(230,236,245)`（实测）—— 蓝/绿/红/灰语义全丢。
- **为什么是问题**：用户在允许的窗口范围内（minWidth 900）拖窄窗口，会看到一张空图 + 失去颜色含义，
  而这是"核心图表"，没有任何兜底提示。
- **建议怎么改**（任一）：
  1. 媒体查询里给两侧都改成同列：`.highway__side, .highway__side--right { grid-column: 1; grid-row: auto }`
     并去掉 `currentColor` 那条，色条改回显式类别色；
  2. 或最小改动：把断点从 `max-width: 940px` 改成 `max-width: 880px` —— 低于 `minWidth: 900`，
     这个分支永不触发（同时删掉那条 `currentColor` 规则以免以后被误用）。
  注意**两处重复的媒体查询都要改**（见 H1）。
- **影响范围**：`styles.css` 的 `@media (max-width: 940px)` 块（当前在 1782 行与 1986 行各一份）。
- **判断**：**值得做**（`minWidth: 900` 明确允许这个宽度；改 4 行）。

---

## H. 附带发现（不影响本轮视觉，但评审时量到了）

### H1　`styles.css` 有两段**完全重复**的规则

- **现象**：`.chain` / `.verdict` 在 1698–1780 行与 1902–1984 行各一份；
  `.globe` / `.facts` 在 1796–1898 行与 2000–2102 行各一份。逐字相同（含注释头）。
  这是 `docs/ui/topology/README.md` 里记过的"同名规则后者优先"事故的残留。
- **为什么是问题**：以后改一处忘一处就会踩同样的坑（G1 的媒体查询就在这两份里）。
- **建议怎么改**：删掉后一份。
- **影响范围**：`styles.css`。
- **判断**：**不值得做（本轮）**。零视觉收益，纯维护；但建议在 G1 改动时顺手删掉后一份，
  因为改 G1 必须同时改两处，删掉就只需要改一处。

### H2　`docs/ui/globe/README.md` 写的缩放上限与代码不一致

- **现象**：README 写"上限 1.8 倍是按球缘要看得见定的"，`Globe.tsx` 是 `export const MAX_ZOOM = 8`。
- **为什么是问题**：下次有人按文档判断"zoom 已经到顶"会得到错误结论（F2 的数值推理正依赖最大值）。
- **建议怎么改**：改文档，或在 F2 调整上限时一起改。
- **影响范围**：`docs/ui/globe/README.md`。
- **判断**：**值得做**（一行；和 F2 一起）。

### H3　给 task-3 / task-6 的探针提醒：只量"离 guide 的距离"量不到 B1

- **现象**：task-6 刚加的 `window.__topologyProbe()` 是"把车的位置投到 `.flow__guide` 上反推进度"，
  我实测车离 guide 只有 0.06–0.68 px —— 也就是说这个探针会报告"车在线上"，
  而实际上 guide 本身离**画出来的线**有 27.5 px、26% 的行程离线。
- **为什么是问题**：探针的口径如果只对着 guide，就永远发现不了"车走的和画的是两条路"这类故障；
  而它正是用户看到的"车不在线上"。
- **建议怎么改**：`__topologyProbe()` 的返回里加一项
  `offLineDist`：车的位置到**所有 `.flow__route`** 的最短距离（以及一份
  `maxGuideDeviation`：guide 采样点到可见线的最短距离的最大值）。
- **影响范围**：`apps/ui/src/preview.ts`（task-6）。
- **判断**：**值得做**（给队友的接口，不是视觉项；实现约 15 行）。

---

## 2. 我认为最该先做的 3 条

**先说一句冲突提示**：B1、A1、A4、C1、E3、E4、E5、E6、F1 全都落在
`apps/ui/src/pages/Topology.tsx`（B2/B3/B4/D1/D2/G1 落在 `styles.css`），而这两个文件是
task-2 的写入范围。建议 lead 把这些**排到 task-2 之后**，或者直接交给 task-2 一并做，
否则会和 task-2 的动画修复互相覆盖。

1. **B1 —— 修 guide 路径几何（车才真的在线上了）**。
   一个 bug 同时是三个用户反馈的根因：车有 1/4 行程在空白处（最大偏离 27.5 px）、
   9 辆车 8 辆颜色与脚下的线不符、"货车在线上走"这个比喻不成立。
   修法明确（每条支线后补回分叉点的回程段；着色改为按当前 seg 区间）。
   它还顺带解释了 task-2 的"跳"为什么难收敛：几何一重测，这条被扭坏的链式路径整体变形。
   不修的代价：后面所有关于"颜色/层级/密度"的调整都建在错的地基上。

2. **E3 + E4 —— `1 << 40` 与"速率"文案**。
   `1 << 40 === 256` 让"车辆数量由实测速率决定"变成常数 3（一行修）；文案又把"累计字节"
   说成"速率"（两行改）。这两条是页面上**对外承诺**的准确性问题，成本最低、毫无争议，
   而且修完才能判断 A1 的"放大车"到底要看几种数量级。

3. **A4 —— `forkX` 系数 0.7 → 0.35**。
   一个数字把扇面从 145 px 摊到 315 px（宽高比 0.52 → 1.14），直接消掉 README 里记录过的
   "扇形看着像一堆竖线"这个回归，同时把左侧 70% 的空白还给曲线。风险为零（只影响绘图几何）。

（若还有余量，第 4 条我会选 **B2 + D1**：图例色块改成线段、给四类去向加线型 ——
一次解决"图例和线不是一回事"和"色盲用户分不清"两个问题，且都在 `styles.css` 里加规则。）

## 3. 未验证 / 边界

- **真实配置（3 入口 × 6 出口 = 18 条线）我没有截图**：`preview.ts` 的 `MOCK_TOPOLOGY` 是 3 × 5，
  所以文中的"18 条"是推算，**实测数字都是 15 条彩色 + 21 条灰 + 36 条路径**。
  密度会随出口数从 5 → 6 再恶化一档（18 条彩线、24 条灰线），所以我给的密度结论是下界。
- **仅有的浏览器是 headless Chrome 153**（`--headless=new`，无 GPU）；柔和阴影、次像素描边的观感
  可能与 Tauri 里在 macOS 上略有差异。所有数值结论（尺寸、颜色合成、对比度、几何）与渲染后端无关。
- **未做**：真实 macOS 窗口在 900 px 宽度下的实测（我只用了 CDP 的 deviceMetricsOverride 模拟视口）；
  `document.hidden`、切换页面再回来对动画的影响（属于 task-3 的范围，我没有碰）。
- **未改任何源码**，包括 `styles.css`；下面这些建议都还没有实施。

## 4. 附录：本报告用到的探针（可直接粘进 devtools 控制台）

> (2)(3) 复用 (1) 里的 `vis` / `pts`，请**按顺序**粘进同一个控制台会话。

```js
// (1) 车 vs 线：谁的颜色不对、车离可见线多远
const vis = [...document.querySelectorAll('.flow__route')];
const trucks = [...document.querySelectorAll('.flow__truck')];
const pts = vis.map(p => { const L = p.getTotalLength(); return {
  stroke: p.getAttribute('stroke'), arr: [...Array(201)].map((_, i) => p.getPointAtLength(L * i / 200)) }; });
trucks.map(g => {
  const m = /translate\(([-\d.]+) ([-\d.]+)\)/.exec(g.getAttribute('transform'));
  const [x, y] = [+m[1], +m[2]];
  let best = { d: Infinity };
  for (const s of pts) { let d = Infinity;
    for (const q of s.arr) d = Math.min(d, Math.hypot(q.x - x, q.y - y));
    if (d < best.d) best = { d, stroke: s.stroke }; }
  return { pos: [x, y], truckFill: g.querySelector('rect').getAttribute('fill'),
           lineStroke: best.stroke, offLine: +best.d.toFixed(2) };
});

// (2) guide 与可见线的最大偏离（B1 的核心证据）
const guides = [...document.querySelectorAll('.flow__guide')];
const g = guides[0], L = g.getTotalLength();
let max = 0;
for (let i = 0; i <= 800; i++) {
  const q = g.getPointAtLength(L * i / 800);
  let d = Infinity;
  for (const s of vis) { const sl = s.getTotalLength();
    for (let k = 0; k <= 200; k++) { const r = s.getPointAtLength(sl * k / 200);
      d = Math.min(d, Math.hypot(r.x - q.x, r.y - q.y)); } }
  max = Math.max(max, d);
}
console.log('guide 0 离可见线最大偏离 px =', max.toFixed(1), '/ guide 长度', L.toFixed(0));

// (3) 密度：12×12 px 网格里最多几条线
const cell = 12, grid = new Map();
vis.forEach((p, pi) => { const L = p.getTotalLength();
  for (let i = 0; i <= L; i += 2) { const q = p.getPointAtLength(i);
    const k = Math.round(q.x / cell) + ',' + Math.round(q.y / cell);
    (grid.get(k) ?? grid.set(k, new Set()).get(k)).add(pi); } });
console.log('最多线数/单元 =', Math.max(...[...grid.values()].map(s => s.size)),
            '单元数 =', grid.size, '≥4 条的单元 =', [...grid.values()].filter(s => s.size >= 4).length);

// (4) 图例色块 vs 实际线的合成色（对比度用 sRGB 相对亮度算）
const comp = (fg, a, bg = [15, 20, 32]) => {
  const c = document.createElement('canvas'); c.width = c.height = 1;
  const x = c.getContext('2d'); x.fillStyle = `rgb(${bg})`; x.fillRect(0, 0, 1, 1);
  x.globalAlpha = a; x.fillStyle = fg; x.fillRect(0, 0, 1, 1);
  return [...x.getImageData(0, 0, 1, 1).data].slice(0, 3);
};
console.log('线@.3 + 回程灰@.22 =', comp('rgba(120,160,210,0.45)', 0.22, comp('#4f8ef7', 0.3)));
```
