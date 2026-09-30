# 设计系统 v1 —— xraytun-next

> 这份文件是 `apps/ui/src/styles.css` 的说明书，也是 ui 写组件时的**类名契约**。
> 唯一事实源仍然是代码：`crates/xt-contract/src/model.rs`（`Stage` / `ConnectPhase` /
> `LogLevel` / `NoticeSeverity` / `ErrorCode`）与 `00-CONTRACT-FREEZE.md` §7（真实数据）。
> 两者与本文件不一致时，以代码和冻结页为准，并立刻回来改这里。
>
> 范围：token、组件规格、状态色 ↔ 契约字段值的绑定、类名清单、动效与无障碍。
> 不做：亮色主题、图标库、组件框架、任何装饰性可视化（见 §9）。

---

## 1. 设计立场（可执行的判据）

1. **颜色只承载语义**。界面里出现的每一种色相都在 §4 有判据字段；没有判据的颜色一律
   用明度表达层级（深浅灰）。这条直接服务 I3：颜色本身也是一句话，说了就要有证据。
2. **绿色只有一个含义**：`stage == connected`（契约已证明 SOCKS 端点可连）。
   **绿色不得用于**：daemon 连接正常、探测成功、设置已保存、列表非空、按钮主色。
3. **未知独立于 0，也独立于错误**。`stats == null` → 「未采样」，不打印任何数字；
   真正的 0 字节是**实测结果**，用正常文字色；错误用 `--state-error`。三者互不相似（§4.5）。
4. **颜色不是唯一载体**。徽章有文字、日志等级有文字、探测失败带 `ErrorCode` 文本。
   灰度截图或色盲用户读到的信息不少于彩色用户。
5. **不做任务外的装饰**。没有阴影、没有渐变、没有圆角动画、没有骨架屏、没有进度条伪百分比。
   字阶 6 档、间距 8 档、圆角 4 档，其余值不允许出现。

---

## 2. Token 全表

`styles.css` 的 `:root` 就是这张表，两边必须逐字一致。

### 2.1 层级与表面

| 变量 | 值 | 用途 |
| --- | --- | --- |
| `--bg` | `#0e1320` | 应用主背景 |
| `--bg-sunken` | `#090d16` | 比主背景更深的凹槽：日志正文、代码块、未采样占位 |
| `--bg-chrome` | `#0b1018` | 固定框架：左侧导航 |
| `--surface-1` | `#161d2c` | 一级表面：卡片、列表行 |
| `--surface-2` | `#1c2436` | 二级表面：输入框、按钮、胶囊内部 |
| `--surface-hover` | `#1a2333` | 悬停 |
| `--surface-active` | `#1b2740` | 选中/按下 |

一页最多 3 种表面色；层级靠留白 + 一条弱分隔线表达，不给每块画边框。

### 2.2 边界

| 变量 | 值 | 用途 |
| --- | --- | --- |
| `--border` | `#263148` | **装饰性**分隔线（豁免 3:1，实测 1.19–1.49:1） |
| `--border-strong` | `#5c7290` | **承载信息的轮廓**：按钮、输入框、下拉框、可聚焦控件的边界（实测 ≥3.02:1，过 WCAG 1.4.11） |
| `--border-dashed` | `var(--tint-unknown-border)` | 未采样/未知占位的虚线边（≥3:1；内联 `.unknown` 的文字色另用 `--state-unknown`） |

### 2.3 文字

| 变量 | 值 | 用途 | 最差背景（`--surface-active`）对比度 |
| --- | --- | --- | --- |
| `--text` | `#e6ecf5` | 主文字、实测数值 | 12.53:1 ✓ |
| `--text-dim` | `#a7b3c6` | 次要文字、字段名、单位描述 | 7.02:1 ✓ |
| `--text-faint` | `#7f8fa6` | 元信息、脚注、路径 | 4.52:1 ✓ |
| `--on-accent` | `#06101f` | 强调色上的文字 | — |

三档文字**全部**在 6 个背景上过 AA 小字 4.5:1（§8 有实算脚本）。

### 2.4 交互与语义色

| 变量 | 值 | 语义 | 允许出现的条件（契约字段） |
| --- | --- | --- | --- |
| `--accent` | `#4f8ef7` | 交互强调（焦点环、主按钮、可点击） | 与数据无关，仅表示「可操作」 |
| `--accent-hover` | `#6ba0ff` | 悬停态 | 同上 |
| `--state-idle` | `#8b98ab` | 中性：未连接 | `ConnectionView.stage == "disconnected"` **且** `last_error == null` |
| `--state-progress` | `#4f8ef7` | 进行中（与 `--accent` 同值，刻意共用，不新造色） | `stage ∈ {"connecting","disconnecting"}` |
| `--state-connected` | `#34d399` | **只有它可以是绿的** | `stage == "connected"`（且 `mode` 明确，见 §4.3） |
| `--state-warning` | `#fbbf24` | 警告 | `NoticeSeverity.warning` / `LogLevel.warn` / 本文件 §4.4 列出的明确降级判据 |
| `--state-error` | `#f87171` | 失败 | `last_error != null` / `NoticeSeverity.error` / `LogLevel.error` |
| `--state-unknown` | `#a3a8cf` | **未采样/未知**（独立色相：淡紫，与中性灰的明度比 1.26） | `stats == null` 或任何 `Option` 字段为 `null` |

状态 tint 六件套（徽章、通知条、未知占位的底色与描边）：

| 状态 | `--tint-*-bg` | `--tint-*-border` | 文字/底色 | 底色/表面比 |
| --- | --- | --- | --- | --- |
| connected | `#12302a` | `#1f5545` | 7.37:1 ✓ | 1.19 |
| progress | `#14243a` | `#2a4d78` | 4.87:1 ✓ | 1.08 |
| error | `#2d171b` | `#5b2b33` | 6.07:1 ✓ | 1.00 |
| warning | `#2a2310` | `#57471f` | 9.34:1 ✓ | 1.08 |
| idle | `#1d2534` | `#3b4a63` | 5.25:1 ✓ | 1.10 |
| unknown | `#0d121c` | `#5f6b85` | 8.09:1 ✓ | 1.11 |

> tint 底色对表面的对比度**刻意压在 1.0–1.2**：底色只是让 chip 有一点体积感，
> **信息全部由文字标签 + 描边 + 色相承载**。任何人把底色当作唯一信号都会读错。

### 2.5 间距

| 变量 | 值 | 用途 |
| --- | --- | --- |
| `--sp-1` | `2px` | 图标与文字、胶囊内上下边 |
| `--sp-2` | `4px` | 紧凑 gap |
| `--sp-3` | `6px` | 行内 gap、按钮内边距 |
| `--sp-4` | `8px` | 控件内边距、列表行间距 |
| `--sp-5` | `12px` | 区块内边距 |
| `--sp-6` | `16px` | 卡片内边距、字段间距 |
| `--sp-7` | `24px` | 区块之间 |
| `--sp-8` | `32px` | 页面底部留白 |
| `--gutter` | `20px` | 内容左右边距（窗口级常量，不进刻度） |

### 2.6 圆角 / 字号 / 字重 / 行高

| 变量 | 值 |
| --- | --- |
| `--radius-xs` | `3px` |
| `--radius` | `6px` |
| `--radius-lg` | `10px` |
| `--radius-pill` | `999px` |
| `--fs-2xs` | `10.5px` |
| `--fs-xs` | `11.5px` |
| `--fs-sm` | `12px` |
| `--fs-md` | `13px`（正文基准） |
| `--fs-lg` | `14px` |
| `--fs-xl` | `20px`（主数字） |
| `--fw-regular` / `--fw-medium` / `--fw-semibold` | `400` / `500` / `600` |
| `--lh-tight` / `--lh-ui` / `--lh-read` | `1.25` / `1.55` / `1.75` |
| `--font-ui` | `system-ui, -apple-system, "Segoe UI", "Noto Sans SC", sans-serif` |
| `--font-mono` | `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace` |

### 2.7 等宽数字（硬规则）

**所有数字都必须用 tabular-nums**，否则实时字节数每秒变宽变窄、整行跳动：

```css
.mono, .num {
  font-variant-numeric: tabular-nums;
  font-feature-settings: "tnum" 1;
}
```

适用：`--fs-xl` 统计主数字（`.stats__value`）、`nodes__endpoint`、`probe`、`log-line__ts`、
`kv__value` 里的时间与字节数、`banner__time`。**不做**字号动画（数字跳动是视觉噪声）。
`.mono` 用等宽字族；`.num` 保持正文字族只开 tabular（当字族不含等宽数字时用）。

### 2.8 动效

| 变量 | 值 | 用途 |
| --- | --- | --- |
| `--dur-fast` | `120ms` | 默认且当前唯一：hover、颜色/描边变化 |
| `--ease-out` | `cubic-bezier(0.2, 0, 0, 1)` | 出现/进入 |

> **不允许有「备着以后用」的 token**：本表只有实际被引用的值。
> 需要更长的时长或另一条缓动时，先有真实使用点再加 token。

规则：
1. **只允许 `transition`，不允许 `setInterval`/`setTimeout`/`rAF`**（I1；guard.sh 扫 UI 的
   `setInterval(`/`setTimeout(`）。连接进度由 `Event::State` 驱动，不由定时器驱动。
2. 无限循环动画只允许 1 个：`.badge--connecting .badge__dot` 的 `xt-pulse`（1.4s linear）。
   它**不是唯一的进度信号** —— 文字「连接中」与子阶段文案始终在同一枚徽章里。
3. 不给 `width/height/top/left` 做过渡。
4. `prefers-reduced-motion: reduce` 时关掉 pulse 与全部过渡（§7）。

### 2.9 焦点

```css
--focus-ring: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent);
```

所有可聚焦元素用 `:focus-visible { outline: none; box-shadow: var(--focus-ring) }`。
两层环保证在任意底色上都可见（外圈 `--accent` 对 `--surface-1` 为 5.25:1）。

---

## 3. 组件清单

每个组件给出：类名、契约字段来源、必须有的文字、禁止项。
组件里出现的**每一个可见信息**都要能追到 §4 或 `TRUE-INFORMATION.md` 的映射表。

### 3.1 阶段徽章 `badge`（stage × phase）

结构：

```html
<span class="badge badge--connected">
  <span class="badge__dot" aria-hidden="true"></span>
  <span class="badge__label">已连接</span>
  <span class="badge__phase">…</span>          <!-- 仅 connecting 时有 -->
  <span class="badge__code">core_exited_early</span> <!-- 仅失败时有 -->
</span>
```

类名 ↔ 契约值（**类名后缀 = 枚举的 serde snake_case 值**，不做二次命名）：

| 类名 | 契约条件 | 颜色 | 必须显示的文字 |
| --- | --- | --- | --- |
| `.badge`（基态，无 modifier） | `stage == null`（快照还没到） | `--state-unknown` + 虚线 | 「未知」 |
| `.badge--unknown` | 同上（显式写法） | `--state-unknown` + 虚线 | 「未知」 |
| `.badge--disconnected` | `stage == "disconnected"` | `--state-idle` | 「未连接」 |
| `.badge--connecting` | `stage == "connecting"` | `--state-progress` | 「连接中」+ 子阶段 |
| `.badge--connected` | `stage == "connected"` | `--state-connected`（**唯一的绿**） | 「已连接」 |
| `.badge--disconnecting` | `stage == "disconnecting"` | `--state-progress`（空心环，见下） | 「断开中」 |
| `.badge__phase--preparing_config` | `phase == "preparing_config"` | progress | 「生成配置」 |
| `.badge__phase--starting_core` | `phase == "starting_core"` | progress | 「启动核心」 |
| `.badge__phase--awaiting_ready` | `phase == "awaiting_ready"` | progress | 「等待就绪」 |
| `.badge__phase--committing_routes` | `phase == "committing_routes"` | progress | 「提交路由」 |

形状区分（颜色不是唯一载体）：
* 基态/`unknown`：虚线圆角框（不是实线）——「不知道」与「知道它是灰的」不同；
* `connecting`：实心圆点 + pulse；
* `disconnecting`：**空心环**、无 pulse —— 同一色相下用形状区分「正在建立」与「正在拆除」；
* `connected`：实心圆点（绿）；`disconnected`：空心细环（灰）。

**基态默认是未知**：漏配 modifier 时渲染成中性灰的 `idle` 会把「界面还不知道状态」
说成「确定没有连接」，所以 `.badge` 不带 modifier 时是虚线未知态。

**禁止**：
* `stage == "connecting"` 时出现绿色或 `--state-connected`（把进行中画成成功）；
* `badge--connected` 旁出现「已保护/安全/正常」文案 —— `Connected` 的契约含义只有
  「数据面已被证明可连（proxy：SOCKS 端口接受连接）」，它不证明**系统流量**走了代理；
* `phase != null` 而 `stage != "connecting"` 时渲染 `.badge__phase` —— phase 只在 Connecting 有意义；
* `committing_routes` 在本轮**不可能合法出现**（helper 未实现，见冻结页 §8）；样式在，
  但任何让它出现在界面上的路径都说明有人伪造了 phase。

### 3.2 状态点 `badge__dot`

只作为徽章的图形部分；**必须**有同源的文字标签在其旁边。不允许任何页面另外画一个
「孤立的彩色小圆点」表示连接状态 —— 没有文字的点等于让用户猜。

### 3.3 统计卡 `stats` / `stats__item`

结构：

```html
<section class="stats" aria-label="真实流量统计">      <!-- 或 .stats--unsampled -->
  <div class="stats__item">
    <div class="stats__label">上行</div>
    <div class="stats__value num">1.24</div>
    <div class="stats__unit">MiB</div>
  </div>
  …
  <div class="stats__sampled-at num">采样于 12:04:31</div>
</section>
```

绑定（`ConnectionView.stats`）：

| 可见信息 | 契约字段 | 空值显示 |
| --- | --- | --- |
| 上行字节 | `stats.uplink_bytes` | **整个 `.stats--unsampled`，不出现任何数字** |
| 下行字节 | `stats.downlink_bytes` | 同上 |
| 采样时刻 | `stats.sampled_at_ms` | 同上 |

* `.stats--unsampled` 的判据**只有** `stats == null`：卡片改虚线描边 + `--state-unknown` +
  `.unknown` 占位「未采样」，`__value` 里**禁止出现 0 / 0 B / -- / N/A 之外的数字**。
* `stats != null` 时 0 是**合法实测值**：显示 `0`，用 `--text`，不得用 `--state-unknown`。
* 速率（B/s）**只有在拿到两个 `stats` 样本且 `sampled_at_ms` 不同**时才允许显示；
  单样本一律不显示速率（不许编 0 B/s）。
* 累计 = `uplink_bytes + downlink_bytes`，属**派生**值，必须与两个分项同帧（同一个
  `StatsView`），标签写「合计」，不得写成「已节省」。

### 3.4 节点行 `nodes__row`

```html
<li class="nodes__row nodes__row--selected nodes__row--current"> … </li>
```

| 类名 | 契约条件 | 视觉 |
| --- | --- | --- |
| `.nodes__row--selected` | `SettingsView.selected_node == NodeView.id` | 左侧 2px `--accent` 色条（inset） |
| `.nodes__row--current` | `ConnectionView.node_id == NodeView.id` **且** `stage == "connected"` | 行尾 `.badge--connected`，绿色只在这一处 |

两者是**不同事实**（「下次连接会用谁」vs「现在连着谁」），可以同时成立，也可以都不成立；
禁止用同一枚绿点同时表达两者。

行内元素：`nodes__name`(`NodeView.name`)、`nodes__protocol`(`NodeView.protocol`)、
`nodes__endpoint`(`NodeView.endpoint`, `.mono`)、`nodes__meta`(协议 + 订阅来源)、
`nodes__probe`(容器，内含 `.probe--*`)、
`nodes__source--subscription|manual`(`NodeView.source.kind`)、`nodes__actions`。

### 3.5 探测 `probe`

| 类名 | 契约条件 | 显示 |
| --- | --- | --- |
| `.probe--ok` | `NodeView.probe.ttfb_ms != null` | `NN ms`（`.mono`），**用 `--text`，不是绿色** |
| `.probe--fail` | `NodeView.probe.error != null` | `ErrorCode` 文本 + `--state-error` |
| `.probe--none` | `NodeView.probe == null` | `.unknown`「未探测」，**不显示 `--` 以外的数字** |

`ProbeResult` 的契约保证 `ttfb_ms` 与 `error` **恰好一个**为 `Some`。UI 不得用
「有值就算成功」的兜底分支；两个都空是契约被破坏，按 `.probe--none` 显示并在日志里报 `internal`。

`.probe--ok` 是「**已测量**」标记，不是成功标记：探测到 800ms 也是 `.probe--ok`，
颜色不得随延迟数值变化（不做红黄绿分级 —— 没有阈值字段可依据）。

### 3.6 日志行 `log-line`

| 类名 | 契约条件 | `log-line__level` 文字 |
| --- | --- | --- |
| `.log-line--error` | `LogLine.level == "error"` | 「错误」 |
| `.log-line--warn` | `level == "warn"` | 「警告」 |
| `.log-line--info` | `level == "info"` | 「信息」 |
| `.log-line--debug` | `level == "debug"` | 「调试」 |

四列：`log-line__ts`(`ts_ms`, `.mono`)、`log-line__level`（文字 + 色）、
`log-line__target`(`target`)、`log-line__message`(`message`)。
等级**必须**有文字，不允许只有颜色。日志空列表 → 「暂无日志」，禁止写「一切正常」。

### 3.7 通知条 `banner` / 错误盒 `error-box`

| 类名 | 契约条件 | 用途 |
| --- | --- | --- |
| `.banner--info` | `Notice.severity == "info"` | 中性说明 |
| `.banner--warn` | `severity == "warning"` | 警告 |
| `.banner--error` | `severity == "error"` | 错误 |
| `.banner__code` | `Notice.code` | `ErrorCode` 原文（可被 ux 测试断言） |
| `.banner__message` | `Notice.message` | 原样显示，不改写 |
| `.banner__time` | `Notice.at_ms` | `.mono` 绝对时刻 |
| `.error-box` | `last_error != null` | 最近一次真实失败，`error-box__code` = `last_error.code`，`error-box__message` = `last_error.message` 原样，`error-box__detail` 可选折叠 |

* `banner__message` **不得**用模板改写（不做「连接失败，请稍后重试」这类话术）：
  契约里没有 retry，写出来就是假话。
* `last_error` 与 `stage` 是两个独立事实：`stage == "connected"` 时仍要显示
  `error-box`（历史失败不应被隐藏），但**不得**因此把 `.badge--*` 改成红色。

### 3.8 空态 `page__empty` / 未采样 `unknown`

```html
<div class="page__empty">
  <div class="page__empty-title">还没有节点</div>
  <div class="page__empty-hint">在「设置」里添加订阅，或手填一个节点。</div>
</div>
```

* 空态只描述**事实**（哪个集合为空）+ **下一步**，不得出现「一切正常 / 已保护」。
* `.unknown` 是内联占位（未采样/未知）：`--state-unknown` 文字 + 虚线描边 + 圆角 +
  `--tint-unknown-bg`，内容只能是词（「未采样」「未知」「未探测」）或 `—` + 词。
  **`.unknown` 内不允许出现数字**。它与 0、与 `--state-error` 在色相/描边/内容三个维度都不同。

### 3.9 通用容器

`card` / `card__title` / `card__body`、`kv` / `kv__key` / `kv__value`（键值对，如 daemon
版本、pid）、`row` / `row__label` / `row__value`（表单行）、`muted`（`--text-dim`）。
`kv__value` 里的时间戳/字节数一律加 `.mono`；daemon 的 `pid`、`protocol_version`
全部来自 `DaemonHello`，不许写死。

### 3.10 按钮与输入

`btn` / `btn--primary` / `btn--danger` / `btn--ghost`、`input`、`select`、`field` /
`field__label` / `field__hint` / `field__error`。
`btn--danger` 用于**破坏性操作**（断开、删除），不是「操作失败」；失败信息只在
`error-box` / `banner--error`。
`:disabled` 用 `--text-faint` + `--border`，不用绿色或红色。
设置页的值以 `PatchSettings` 的**响应体 `SettingsView`** 为准，不乐观更新。

---

## 4. 状态色 ↔ 契约字段值（允许出现的条件）

**这张表是 ux 写自动化测试时的判据。** 任何状态色在界面上的出现都必须能对上其中一行。

### 4.1 stage → 徽章色

| 契约值（`ConnectionView.stage`） | class | 色变量 | 允许出现的前提 |
| --- | --- | --- | --- |
| *尚未收到任何 `ConnectionView`（`stage` 还不存在）* | `.badge` / `.badge--unknown` | `--state-unknown` + 虚线 | 只表示「界面还不知道」，**不得**渲染成 `disconnected` |
| `"disconnected"` | `.badge--disconnected` | `--state-idle` | 无附加条件 |
| `"connecting"` | `.badge--connecting` | `--state-progress` | 无附加条件 |
| `"connected"` | `.badge--connected` | `--state-connected` | 无附加条件（stage 本身就是证据） |
| `"disconnecting"` | `.badge--disconnecting` | `--state-progress` | 无附加条件 |

### 4.2 phase → 子阶段色（只在 connecting 内）

| `phase` | class | 色变量 |
| --- | --- | --- |
| `"preparing_config"` | `.badge__phase--preparing_config` | `--state-progress` |
| `"starting_core"` | `.badge__phase--starting_core` | `--state-progress` |
| `"awaiting_ready"` | `.badge__phase--awaiting_ready` | `--state-progress` |
| `"committing_routes"` | `.badge__phase--committing_routes` | `--state-progress`（本轮不可达，见 §3.1） |
| `null` | 不渲染 `.badge__phase` | — |

### 4.3 「已连接」的措辞边界

`stage == "connected"` **只**允许这些措辞：「已连接」「代理端点可连」。
**禁止**：「已保护」「安全」「系统流量已代理」「TUN 已接管」「防火墙已接管」。
理由：本轮只有 `RunMode::Proxy`（SOCKS 入站，且 helper 未实现），`Connected` 证明的是
「SOCKS 端口接受连接」，不是「这台机器的流量被接管」。
`ConnectionView.mode == null` 时不得显示任何模式词（未知 ≠ 直连）。

### 4.4 错误 / 警告 / 未知色

| 色变量 | 唯一允许的判据 | 不得用于 |
| --- | --- | --- |
| `--state-error` | `ConnectionView.last_error != null`、`NoticeSeverity == "error"`、`LogLevel == "error"`、`SubscriptionView.last_error != null`、`ProbeResult.error != null`、`field__error`（本地校验失败） | 未连接（无错误时）、探测慢、按钮悬停 |
| `--state-warning` | `NoticeSeverity == "warning"`、`LogLevel == "warn"`、`Frame::Event.seq` 跳号 | 「进行中」、探测延迟不理想 |
| `--state-unknown` | 任何 `Option` 字段为 `null`：`stats`、`phase`、`mode`、`node_id`、`connected_since_ms`、`datapath.pid`、`datapath.version`、`datapath.ready_at_ms`、`NodeView.probe`、`SubscriptionView.fetched_at_ms` | 实测 0、空列表、空字符串 |
| `--state-connected` | 仅 `stage == "connected"` | daemon IPC 正常、设置已保存、探测成功、列表非空 |
| `--state-progress` | 仅 `stage ∈ {"connecting","disconnecting"}` | 任何已完成的操作 |

### 4.5 三种「什么都没有」必须长得不一样

| 情形 | 契约 | 视觉 | 内容 |
| --- | --- | --- | --- |
| 实测为 0 | `stats != null`，字节为 `0` | 正常文字 `--text`，实线边界 | `0 B` |
| 从未采样 | `stats == null` | `--state-unknown` + 虚线边界 + tint | 「未采样」（**无数字**） |
| 失败 | `last_error != null` / `ProbeResult.error` | `--state-error` + `error-box` | `ErrorCode` + 真实 message |

三者互不共享类名，也互不共享色变量。测试可以直接断言：
`:root` 里 `--state-idle`、`--state-unknown`、`--state-error` 三个值互不相等，
且 CSS 中不存在把 `.stats--unsampled` 或 `.unknown` 指向 `--state-error`/`--state-connected` 的规则。

---

## 5. 类名清单（ui 照这个填）

> 只允许使用本清单里的类；清单外的类没有样式，等价于没写。
> 命名规则：block / block__element / block--modifier；**modifier 后缀 = 契约枚举值**
> （`badge--connected`、`log-line--warn`、`nodes__source--subscription`）。

**布局/导航**
`app` `app__nav` `app__nav-item` `app__nav-item--active` `app__main`

**页面**
`page` `page__title` `page__hint` `page__section` `page__section-title`
`page__empty` `page__empty-title` `page__empty-hint`

**通用**
`card` `card__title` `card__body` `kv` `kv__key` `kv__value`
`row` `row__label` `row__value` `muted` `mono` `num` `unknown`

**徽章**
`badge` `badge__dot` `badge__label` `badge__phase` `badge__code`
`badge--unknown` `badge--disconnected` `badge--connecting` `badge--connected` `badge--disconnecting`
`badge__phase--preparing_config` `badge__phase--starting_core`
`badge__phase--awaiting_ready` `badge__phase--committing_routes`

**按钮/输入**
`btn` `btn--primary` `btn--danger` `btn--ghost` `input` `select`
`field` `field__label` `field__hint` `field__error`

**错误/通知**
`error-box` `error-box__code` `error-box__message` `error-box__detail`
`banner` `banner__severity` `banner__code` `banner__message` `banner__time`
`banner--info` `banner--warn` `banner--error`

**Dashboard**
`dashboard` `dashboard__grid` `stats` `stats__item` `stats__label` `stats__value`
`stats__unit` `stats__sampled-at` `stats--unsampled` `controls` `controls__note`

**Nodes**
`nodes` `nodes__list` `nodes__row` `nodes__row--selected` `nodes__row--current`
`nodes__name` `nodes__meta` `nodes__protocol` `nodes__endpoint` `nodes__source`
`nodes__source--subscription` `nodes__source--manual` `nodes__actions` `nodes__probe`
`probe` `probe--ok` `probe--fail` `probe--none`

**Logs**
`logs` `logs__list` `log-line` `log-line__ts` `log-line__level` `log-line__target`
`log-line__message` `log-line--error` `log-line--warn` `log-line--info` `log-line--debug`

**Settings**
`settings` `subs` `subs__list` `subs__row` `subs__url` `subs__meta`

**无障碍工具**
`sr-only`

---

## 6. 无障碍

* 彩色徽章/等级/通知条都带文字标签（§3.1/3.6/3.7）；灰度高对比下仍可读。
* 可聚焦元素统一 `:focus-visible` 双层焦点环（§2.9）。
* `--text-faint` 是最低对比度文字（4.52:1），任何新加的「更淡」文字都是不合格的。
* 状态变化用一个 `.sr-only` + `role="status"` 节点播报（内容与徽章文字同源）；
  页面不得只靠颜色变化表达状态切换。
* `prefers-reduced-motion: reduce`：关闭 `xt-pulse` 与全部过渡（样式里显式写了）。

---

## 7. 对比度实算（复现方式）

本文所有比值由 WCAG 相对亮度公式（`L = 0.2126R + 0.7152G + 0.0722B`，线性化后再加权）
针对**本文件的 token 值**算出，不是估的。复现脚本（写入 `/tmp` 即可，不提交）：

```bash
node -e '
const hex=h=>{h=h.replace("#","");return [0,2,4].map(i=>parseInt(h.slice(i,i+2),16)/255)};
const lin=c=>c<=0.03928?c/12.92:Math.pow((c+0.055)/1.055,2.4);
const L=h=>{const[r,g,b]=hex(h).map(lin);return 0.2126*r+0.7152*g+0.0722*b};
const R=(a,b)=>{let x=L(a),y=L(b);if(x<y)[x,y]=[y,x];return((x+0.05)/(y+0.05)).toFixed(2)};
for(const c of ["#e6ecf5","#a7b3c6","#7f8fa6","#8b98ab","#4f8ef7","#34d399","#fbbf24","#f87171","#a3a8cf","#5c7290"])
  console.log(c, "on s1", R(c,"#161d2c"), "on s2", R(c,"#1c2436"), "on active", R(c,"#1b2740"));
'
```

结论：三档文字 + 6 个状态色 + 控件描边在 `--surface-1` / `--surface-2` / `--surface-active`
三个实际承载文字的背景上**全部** ≥ 4.5:1（描边按 1.4.11 要求 ≥3:1，最差 3.02:1）；
装饰性 `--border` 不承载信息，豁免。

---

## 8. 动效落地（无 JS 依赖）

```css
@keyframes xt-pulse { 50% { opacity: 0.35 } }
.badge--connecting .badge__dot { animation: xt-pulse 1.4s linear infinite }
@media (prefers-reduced-motion: reduce) {
  .badge--connecting .badge__dot { animation: none }
  *, *::before, *::after {
    transition-duration: 0.01ms !important;
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
  }
}
```

状态推进**只**来自 `Event::State`。CSS 里没有任何「过一会儿自己变」的东西：
如果界面在动，那是 daemon 推了事件，不是定时器。

---

## 9. 不做（明确清单）

* **亮色主题**：本轮不算色值，不加 `prefers-color-scheme: light` 分支（没实算过的配色不发布）。
* **阴影/渐变/毛玻璃、骨架屏、伪进度条百分比、动画图标**：无契约字段，全是装饰。
* **拓扑图 / 地球仪 / 地图 / 地理可视化**：冻结页 §8 明确不做。
* **TUN / helper 相关状态**：`RunMode::Tun` 与 `CommittingRoutes` 的样式在，但本轮
  不可能合法出现；出现即为伪造。
* **图标库**：不引入依赖，符号用文本（「·」「—」）或内联字符。

---

## 10. 验收证据与未验证项

### 10.1 实测通过（命令 + 结果）

| 检查 | 命令（工作目录） | 结果 |
| --- | --- | --- |
| 不变量机器判据 | `bash scripts/guard.sh`（`xraytun-next/`） | **PASSED**：违规=0 警告=0 |
| TS 类型 + 打包 | `npm run build`（`xraytun-next/apps/ui/`，= `tsc --noEmit && vite build`） | **exit 0** |
| 真实应用打包 | `./node_modules/.bin/vite build`（`apps/ui`，vite 6.4.3） | 45 modules，CSS 产物 `index-*.css` 16.91 kB（本文件） |
| CSS 语法 | `esbuild apps/ui/src/styles.css --outfile=/dev/null` | exit 0 |
| 类名自洽 | 脚本比对 §5 清单 ↔ `styles.css` ↔ ui 的 `className` | 清单 109 个全部有样式；CSS 无清单外类；ui 已用的类无缺失 |
| token 自洽 | 同上脚本 | 64 个 token，无「未定义即用」，无「定义未用」 |
| 状态色独立 | 同上脚本 | `--state-idle/#8b98ab`、`--state-unknown/#a3a8cf`、`--state-error/#f87171` 互不相等；`--state-connected` 只被 `.badge--connected` 一个选择器使用；`.unknown`/`.stats--unsampled`/`.probe--none` 不被指向 error/connected |

> 这些是**跑过**的结果；本文件不写没跑过的「通过」。

### 10.2 未验证 / 残余风险

* **观感未经浏览器截图核对**：间距、换行、长 endpoint（`.nodes__endpoint` 的
  `overflow-wrap: anywhere`）、日志行 4 列在窄窗下的挤压，都需要 ui 落地后目视复核。
* 对比度是公式实算 + 纯色样本，不是从渲染结果采样（对纯色文字两者等价；渐变/透明度场景不适用，
  本系统没有渐变）。
* `CommittingRoutes`（TUN 专用）的样式**没有真实数据可验证**：本轮 helper 未实现，
  它只在契约类型里可达。
* `prefers-reduced-motion` 的实际效果需要系统级开关配合才能演示，未截图验证。
