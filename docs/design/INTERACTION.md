# xray-tun 交互规格（INTERACTION）

> 本文回答一件事：**用户怎么用**。每条都是可实现、可验证的规格，不是感受性描述。
> 视觉（长什么样）归 `art-designer`，做什么/优先级归 `product-manager`，本文只管流程、状态、反馈、可访问性、文案与跨页一致性。
>
> **基线**：`HEAD = 7ffbbe5`（`refactor(preview): 拆成 5 个 preview* 模块`）。
> 本文所有「现在是什么行为」都来自两类证据之一：
> 1. **截图/探针实测**（headless Chrome 153，`?preview=1` 预览页，CDP 直连；脚本与原始 JSON 在 `/tmp/interaction-review/`）；
> 2. **代码行号 + 逐字引用**。
> 没有第三条：没有实测也没有代码依据的推断，本文一律标注「未验证」。
>
> 会话期间源码在动：`preview.ts` 被拆成 5 个模块（7ffbbe5），`Globe.tsx` 删了两个 export（9fb15e9）。
> 本文结论在 **7ffbbe5 上重新跑过一遍**（`/tmp/interaction-review/out-s3-reverify.json`），与首轮一致。
> `Topology.tsx` / `Nodes.tsx` / `Logs.tsx` / `store.tsx` / `App.tsx` 在会话期间未变。

---

## 0. 怎么复现（下文所有实测都可用这套命令重放）

```bash
cd apps/ui && npx vite --port 5311 --strictPort      # 不要用 5173，队友可能在用
# 页面：http://127.0.0.1:5311/?preview=1&state=connected&view=<页面>
```

**预览场景开关**（`preview*` 模块，`?preview=1` 才生效）：

| 开关 | 取值 | 用来验什么 |
|---|---|---|
| `state` | `connected` / `uncommitted` / `disconnected` / `no-core` / `stale` / `notice` | 快照级状态 |
| `view` | 8 个页面 id | 直接落在某页 |
| `traffic` | `unavailable` | 拓扑页「本次查不到流量」 |
| `connections` | `normal`(42) / `busy`(600, dropped 137) / `empty`(0) / `unavailable`(reject) | 最近连接四态 |

CDP 驱动：`node /tmp/interaction-review/cdp2.mjs <scenarios.json>`（支持**预加载注入**改写 `invoke`、**真实键盘事件**、**分步脚本**）。
截图：`/tmp/interaction-review/shots/*.png`；原始探针输出：`/tmp/interaction-review/out-s2.json`、`out-s3.json`、`out-s3-reverify.json`、`out-s4.json`。

### 0.1 验证边界（必须先读）

* **所有浏览器自动化都跑在 `?preview=1` 的合成快照上，不是真实 Tauri 应用。**
  真实 macOS 壳用 WKWebView，**没有 CDP**，无法自动化。凡是「真机才成立」的行为，下文单独标 **【真机未验证】**。
* 预览是**假后端**：`invoke` 返回 mock（`preview*` 模块），按钮点了不会真的启动核心。
  所以「点击后的界面反应」可测，「操作是否真的生效」不可测。
* `runtime://changed` 的**前端反应**是实测的（我用预加载注入拿到 `transformCallback` 的注册回调，再手动喂一条载荷）。
  但**后端在换网/唤醒时是否真的发这条事件**，来自代码阅读（`apps/desktop/src/commands/core.rs`），**未经真机验证**。
* 预览里 `routing_topology` / `global_data` 失败态、`tail_logs` 失败态只能用注入改写 `invoke` 造出来（预览没有这些开关）。
  注入只改浏览器里的 mock，**不改仓库源码**。

---

## 1. 核心流程逐步走查

每一步写三件事：**用户做什么 → 界面给什么反馈 → 卡在哪**。
「卡在哪」= 需要用户猜、需要额外步骤、或得到错误结论的地方。

### FLOW-1 首次使用：导入订阅 → 选节点 → 连接

| # | 用户做什么 | 界面给什么反馈 | 卡在哪 |
|---|---|---|---|
| 1 | 打开应用 | 落在**仪表盘**；侧栏 8 项，`节点`/`订阅` 带数量徽章。核心未定位时状态词是「未找到核心 / 缺少 Xray 可执行文件」 | 侧栏底部写「核心 未找到」。**好消息**：这是唯一一处把「缺核心」说成结论而不是让用户猜的地方 |
| 2 | 找地方加订阅 | 侧栏点「订阅」 | **`仪表盘` 没有指向订阅页的入口。** 唯一跨页链接是 `切换节点`(→nodes)、`查看日志`(→logs)、`全部设置`(→settings)（`pages/Dashboard.tsx:121,240,243`）；订阅/规则/拓扑/地球仪四个页面在仪表盘上无入口。新用户得先猜「订阅」这个侧栏词是什么意思 |
| 3 | 填名称 + URL，点「添加并拉取」 | 按钮出现 spinner（`pages/Subscriptions.tsx:69`），**没有任何进度**：订阅可能拉几 MB，界面只有一个 spinner，**不可取消** | 大订阅卡住时用户的选项只有「等」和「杀进程」。没有「正在下载 N/M」、没有超时说明 |
| 4 | 拉取成功 | 表单被清空（`pages/Subscriptions.tsx:31-34`）——**这是唯一的成功反馈** | 没有「已导入 23 个节点」。用户要去「节点」页自己数 |
| 5 | 回到「节点」页 | 4 行节点，行内两个徽章：`距离 53 ms` / `可用`（`pages/Nodes.tsx:288-297`） | **`距离` 与 `可用` 是两个不同问题的答案**（注释见 `pages/Nodes.tsx:283-287`），但页面标题/说明为空 —— 该页**没有 `page__title` / `page__desc`**（实测：`document.querySelector('.page__title')` 为 `null`）。用户看到「距离 53 ms / 不可用」需要自己解释这为什么能同时成立 |
| 6 | 点行选中节点 | 行加 `is-selected` + 「当前」标签（`pages/Nodes.tsx:274`） | **整行是一个 `<div onClick>`，不可键盘聚焦**（实测 Tab 轨迹 26 次 Tab 从未落进 `.node-row`）。鼠标独有 |
| 7 | 点「连接」 | 按钮出现 spinner（`App.tsx:209`），成功后顶栏圆点变绿、按钮文案变「断开」 | `state=uncommitted`（隧道已建、路由未接管）时顶栏**仍然是绿点 + 「断开」**（`App.tsx:202,210`，圆点只看 `runtime.running`）。**「已连上」与「隧道建好但流量还没走代理」在顶栏长得一样**。只有仪表盘的状态词区分（`pages/Dashboard.tsx:65-67`） |

**FLOW-1 的规格要求**

* **FLOW-1-A**：仪表盘新增到「订阅」页的入口（空节点时主按钮应为「添加订阅」，而不是「选择节点」）。
* **FLOW-1-B**：节点页补 `page__title` + `page__desc`，首段解释「距离 ≠ 可用」。
* **FLOW-1-C**：顶栏圆点必须表达**四态**（未连接 / 隧道已建立未接管 / 已连接 / 直连模式），不能只表达 running。
  `App.tsx:202` 现在只有 `dot` / `dot--on` 两个类；`dot--warn` 只在仪表盘用（`pages/Dashboard.tsx:66`）。
* **FLOW-1-D**：订阅拉取要能取消或至少显示进度（后端已有 `nodes://probe-started` 的先例，见 FB-3）。

---

### FLOW-2 换网恢复（咖啡厅 → 家、熄屏唤醒）★ 用户最在意

**先说结论：用户不需要点「连接」——自动恢复确实存在，但界面在恢复期间仍然显示「已连接」，用户没有任何办法知道「正在恢复」。**

**后端做了什么**（代码，非实测）：

* 隧道看门狗每 10s 经本地 SOCKS 发一次真实请求（`apps/desktop/src/commands/core.rs:412`、`:462`），
  **连续 2 次失败**即自动重建隧道（`should_rebuild_tunnel`，`:333`）；
  检测到刚睡醒（单调时钟 vs 墙上时钟的差值，`slept_for` `:313`）时**只等 1 次失败**，把恢复从 ~30s 压到 ~10s（注释 `:302-312`）。
* 重建失败**退回直连**而不是断网（`:507-519`）。
* 开机/登录项路径也会自动重连，最多试 `RECONNECT_ATTEMPTS` 次（`:740-823`）。
* 前提是 `settings.auto_reconnect` 且**上次是用户主动连着的**（`was_connected`，`:272-283`）——
  用户点过「断开」就清掉，所以「除非我关闭，网络不该断」成立。

**界面做了什么（实测）**：

| # | 用户做什么 | 界面给什么反馈 | 卡在哪 |
|---|---|---|---|
| 1 | 从咖啡厅回到家，Wi-Fi 换了 | 什么都不变：状态词仍是「已连接」，顶栏圆点仍是绿的（实测 baseline：`已连接 · 香港 · REALITY 01 53 ms` / `dot dot--on`） | **看门狗要连续失败 2 次（约 20–30s）才会动**，这段时间界面在说谎 |
| 2 | 继续等 | 看门狗判定要重建时，后端写一条 `last_notice = "网络中断，正在自动恢复…"`（`core.rs:493`）并 `emit(runtime://changed)`（`:495`） | **这条 notice 前端收不到**：`runtime_changed` 的载荷只有 `{runtime, traffic}`（`apps/desktop/src/events.rs:45-53`），没有 notice；而前端只在**挂载时**和 `nodes://changed` / `subscriptions://changed` / `settings://changed` 时才重新拉 `snapshot`（`store.tsx:197-199`），notice 恰恰只由 `snapshot` 命令下发（`apps/desktop/src/commands/snapshot.rs:46`）。**于是「正在自动恢复…」这条最关键的信息在真机上不会出现在界面里**（实测复现见下） |
| 3 | 重建开始（`stop_core`） | `runtime.running` 变 false → `runtime://changed` → 状态词变「未连接」、圆点变灰、按钮变「连接」（实测：喂一条 `running:false` 的载荷后 900ms 内完成） | 用户此刻看到的是「未连接」——像是**自己断了**，而不是「正在自愈」。而且按钮变成可点的「连接」，用户很容易去点它 |
| 4 | 用户手快点「连接」 | 会真的发起一次连接 | 与看门狗抢，可能重建两次。规格要求见 FLOW-2-D |
| 5 | 重建成功 | `running` 变 true 的事件到达 → 状态词回到「已连接」（实测：喂 `running:true, routes_committed:true` 后回到 `已连接`） | **没有「已恢复」的确认**。如果用户当时在看别的页面（或窗口在后台），什么都不会被通知 |
| 6 | 重建失败 | 后端写 `"自动恢复失败，已退回直连（不再走代理）"`（`core.rs:516`）+ 日志 error 行 | 同样**收不到 notice**（同第 2 步）。日志页能看到那一行，但要用户主动去看 |

**关键实测（可复现）**：注入 `notice = "网络中断，正在自动恢复…"` 后强制一次快照刷新，仪表盘上同时出现：

```
状态词：已连接 · 香港 · REALITY 01 53 ms
横幅：   ℹ︎ 网络中断，正在自动恢复…  [去处理]
```

→ **「已连接」与「正在自动恢复」同屏矛盾**；而且这个横幅上的按钮写着「去处理」，可恢复是自动的，用户没有任何可做的事（`pages/Dashboard.tsx:335`）。

**FLOW-2 的规格要求**

* **FLOW-2-A（最高优先）**：`runtime://changed` 的载荷必须带 `notice`（或前端在收到该事件时顺带拉一次 `snapshot`）。
  在补上之前，**换网恢复的界面反馈等于不存在**。
* **FLOW-2-B**：新增一个**恢复态** `runtime.recovering: boolean`（后端在 `core.rs:485-495` 那段置 true，成功/失败后置 false），界面：
  * 仪表盘状态词第三态「正在自动恢复…」（黄色，与「隧道已建立」同级）；
  * 顶栏圆点加第三态（黄/脉冲），**不要**在恢复期间把按钮显示成「连接」；
  * 恢复完成后显示一次「已自动恢复」，并带**时间**（「10 秒前自动恢复」）。
* **FLOW-2-C**：恢复期间「连接」按钮应禁用并给出理由（`title`），避免用户与看门狗抢。
* **FLOW-2-D**：`notice` 类横幅只有在**真有动作可做**时才给按钮；恢复类 notice 的按钮应删掉（改成一个「看日志」次按钮）。
* **FLOW-2-E**：恢复状态必须至少出现在**顶栏**（全局可见），不能只在仪表盘。
  当前 5 类 notice 全部只在仪表盘渲染（`pages/Dashboard.tsx:157-172`）。
* **FLOW-2-F**：所有恢复相关的结论**【真机未验证】**——真机上是否真的在换网/唤醒后 10–30s 恢复，需要用户在真机复现一次并记录时间戳。

---

### FLOW-3 排查：「这个网站走了哪条规则」

**要几步：3 步（输入 → 判定 → 读结论），这一点是合格的。**

| # | 用户做什么 | 界面给什么反馈 | 卡在哪 |
|---|---|---|---|
| 1 | 侧栏点「拓扑」 | 滚动到页面下方「某个地址会走哪条路」区块（`pages/Topology.tsx:1489-1532`） | **它在 4 个区块之后**：网络流动图（含 100 行连接列表）→ 规则链 → 才到这里。默认窗口（1080×720）下需要滚动很长距离。空拓扑时更糟：区块还在，但上面是空图 |
| 2 | 输入 `www.google.com`，点「判定」（或直接按 Enter） | 按钮 spinner（`:1512`）；Enter 可用（`:1506-1507`，实测输入+点击后发出 `explain_dest` 命令） | 输入框**没有 `<label>`**，只有 placeholder「例如 www.google.com 或 223.5.5.5」（`:1503`）。屏幕阅读器拿不到名字 |
| 3 | 读结论 | `命中规则「preset-proxy-google」→ 出站 node-n1d232c6b8c7a5004` + 命中原因列表（实测原文） | **结论里没有「为什么是这条而不是上面那条」**。用户看到 `命中 geosite:google` 仍要自己回到规则链表格，比对「它排在 geosite:cn 前面」——**排查的第二步没有链接**：`.verdict` 里没有指向规则链那一行的跳转/高亮 |
| 4 | 想看某个真实连接的去向 | 滚动到「最近连接」，点一行 → 图上高亮那条路 + 出现详情面板（实测：`conn-row--on` 1 个、`.conn-detail` 出现、`.flow__highlight` 1 条、匹配的入口/出口卡片描边 2 个、「取消高亮」按钮出现） | ① **域名是时序配对**，详情里如实写了「这条相差 94µs，并发时可能对不上」（`:722-725`）——这是**正确的诚实**，不要改；② 但「已观察 42 条连接 · 配到域名 26 条（62%）」这类统计和单条结论放在同一个视觉层，用户容易把 62% 的近似当成某条连接的准确率 |

**FLOW-3 的规格要求**

* **FLOW-3-A**：把「某个地址会走哪条路」**提到页面上方**，或给一个页内锚点 + 页内导航（现在完全没有）。
* **FLOW-3-B**：判定结论要**回指规则链**：结论里补「命中第 N 条（在它之前的 M 条未命中）」，并让用户能一键滚到那一行。
* **FLOW-3-C**：判定输入框加 `<label htmlFor>`（或 `aria-label`）。全站 `htmlFor` 出现 0 次。
* **FLOW-3-D**：`?connections=unavailable`（拿不到日志）时，「最近连接」区域必须显示失败态，不能显示成空列表——
  这一条**已经做对了**（实测原文：`取不到最近连接：核心未运行：…（访问日志由核心写入 —— 核心没在跑时没有新行可读。）`），
  与它对比的是日志页（见 ST-3），同一个问题在日志页**没有**做对。

---

### FLOW-4 断线 / 失败：怎么知道原因、怎么恢复

分四种失败，界面处理**强弱不一致**：

| 失败 | 界面（实测） | 能恢复吗 |
|---|---|---|
| **拓扑取不到**（核心没跑） | 整页被替换成一块灰 `.note`：`取拓扑失败：dial unix …socket: connect: no such file or directory / 拓扑来自运行中的配置…核心没在跑时读不到。` 实测：`.page__title` 不存在、**没有任何按钮**、`.banner--error` 0 个；3 秒内 `routing_topology` 被调了 3 次（2s 轮询在重试） | **能自愈但用户不知道**：每 2s 自动重试，界面没写「会自动重试」，也没有「重试」按钮 |
| **地球仪取不到** | **红色** `banner--error` + 常驻「重新定位」按钮（实测：banner 有内容、canvas 仍渲染、`.facts` 不存在、按钮 `重新定位`） | 能，且用户看得到按钮。**与拓扑页是同一个失败、弱一个等级** |
| **最近连接取不到** | 灰 `.note` + 原因 + 「核心没在跑时没有新行可读」 | 2s 轮询自动重试 |
| **快照取不到**（`snapshot` reject） | 顶部一条红横幅「⚠︎ 后端连接失败：helper 未响应 [关闭]」+ 页面永久停在「正在加载…」；侧栏底部也是「正在加载…」 | **不能**：`refresh()` 只在挂载和三个业务事件时调用（`store.tsx:66-73,112-113,197-199`），没有定时器、没有重试按钮。用户只能重启应用 |
| **日志取不到**（`tail_logs` reject） | **显示成「空」**：`还没有日志。核心的 stdout/stderr 会被实时转发到这里 —— 如果一直是空的，通常意味着核心还没启动过。` 实测：`.logs__empty` 有这段文字、`.banner` 为 null、`[role=alert],[aria-live]` 0 个 | 不能，也不告诉用户。**这是「拿不到 ≠ 没有」的残留**（store.tsx:207-213 静默吞掉） |

**FLOW-4 的规格要求**

* **FLOW-4-A**：全站失败态统一为**同一个组件**三要素：**红色徽标 + 一句话原因 + 一个动作**。
  现状：拓扑=灰 note 无按钮，地球仪=红 banner 有按钮，快照=红 banner 只有「关闭」，日志=没有。
* **FLOW-4-B**：拓扑页的失败态必须**保留标题**并写「每 2 秒自动重试中」，同时给一个「立即重试」按钮。
* **FLOW-4-C**：快照失败（`snapshot` reject）必须给「重试」按钮，并调用 `refresh()`；不能只给「关闭」。
* **FLOW-4-D**：日志页必须区分三态：**加载中** / **空**（核心确实没输出过）/ **拿不到**（带原因 + 重试）。
  规格见 ST-3。

---

## 2. 状态规格（五态）

五个状态的定义（全站统一口径）：

| 态 | 定义 |
|---|---|
| **L** 加载 | 请求已发出、还没结果 |
| **E** 空 | 拿到数据了，但确实是 0 条 / 0 字节 |
| **D** 有数据 | 正常 |
| **P** 部分失败 | 主体有数据，某个子区域没拿到（例：拓扑有配置、流量不可用） |
| **F** 完全失败 | 该区域什么都没拿到 |

**判定总原则**：`E` 与 `F` 必须在**文字**上不同（不能只靠颜色/有无内容）。
本项目已多次因此出问题（`dns-out`/`api` 的 `0 B` 是测量盲区；计数器归零被当流量归零），下面逐区域给现状与规格。

### ST-1 拓扑：拓扑数据 + 流量计数

| 态 | 现在的呈现（实测/代码） | 判定 |
|---|---|---|
| L | `.empty`「正在加载…」（`Topology.tsx:249`） | ✅ |
| E（`outbound: []`） | **无分支**：图例照旧宣传 4 类颜色，出口列只剩「合计 出入 0 B」，**一条线、一辆车都没有，也没有一句话**（实测：`legend` 存在、`routes:0`、`trucks:0`、`.note` 0 个） | ❌ 「配置里没有出口」与「图没渲染出来」长得一样 |
| D | 三列图 + 车 + 规则链 | ✅ |
| P（`traffic_ok === false`） | 所有字节位显示「流量不可用」（`:387,396,416,423`）+ 图**下方**说明（`:265-271`） | ⚠️ 见 ST-1-P 的冲突 |
| F（`routing_topology` reject） | 整页替换成灰 note，无标题、无按钮（`:238-248`） | ❌ 见 FLOW-4-B |

**ST-1-A（E 态）**：`inbound.length === 0 | outbound.length === 0` 时早返回一个说明块：
「运行中的配置里没有出口/入口 —— 核心可能没在运行」，并**隐藏图例**（现在图例无条件渲染）。
→ 与 `docs/ui/topology/DESIGN-REVIEW.md` **E6 完全重合**，我确认该条仍然存在（实测于 7ffbbe5），**不重复计数**，由本文件接续为验收项。

**ST-1-B（P 态的自身矛盾，本文件新增）**：
`traffic_ok === false` 时文案写着「这里不画 0 字节的假流量」（`:267-270`），但**车照跑**。
代码是有意的：`laneBytes()` 在不可信时回退到**最后一次可信读数**（`:1286-1294`，注释 `:1280-1285`：「否则每次查询失败车数都会 13 → 9 → 13 地弹，那也是一种乱跳」）。
**实测（可复现）**：页面先正常加载（11 辆车），随后把 `traffic` 切成 `unavailable` →
车道文字全部变成「流量不可用」，**11 辆车在 1.5 秒内 11 辆全部移动**（`before/after` transform 已记录）。

这是本次最微妙的一条：它**修掉了乱跳**，却留下「车道写着不可用、车还在跑」的观感，与同页的诚实文案打架。
**规格**：
* 保留「不跳」的初衷：车数沿用上次读数，**不闪回**；
* 但必须把它**画成「不是当前读数」**：车改为空心 + 虚线描边（复用 `prefers-reduced-motion` 之外的静态样式），
  车道卡片字节位在「流量不可用」后补一行小字「（车上仍是上次读数）」；
* 文案同步改成「**不画假的 0**；没读数时车沿用上次读数并标为陈旧」，让文案与画面一致。

**ST-1-C（真 0 vs 测不到，已修，勿回归）**：
`?traffic=unavailable` 冷启动时 **0 辆车**（实测：`trucks: 0`），字节位是「流量不可用」。
把**所有**字节注入成真 0 且 `traffic_ok: true` 时，**也是 0 辆车**、字节显示 `↓0 B ↑0 B`（实测：`trucks: 0`）。
即「0 B 的车道却有车在跑」**已修复**（`trucksOnLane` 在 `<= 0` 时返回 0，`Topology.tsx:800`）。
→ **`DESIGN-REVIEW.md` 的 E2/E3 已过期**，不要再按它改。

### ST-2 拓扑：最近连接

| 态 | 现在 | 判定 |
|---|---|---|
| L | `.empty`「正在读取访问日志…」（`:530`） | ✅ |
| E | 「还没有连接记录（核心刚启动时正常）。」（`:614`） | ✅ |
| D | 列表（上限 100 行）+ 汇总 | ✅ |
| P（过滤无命中） | 「在已取到的最近 N 条里没有匹配的连接（不是全量搜索）。」（`:615`） | ✅ 措辞合格 |
| F | 「取不到最近连接：{error}（访问日志由核心写入 —— 核心没在跑时没有新行可读。）」（`:523-528`） | ✅ |

**这一区是**全站状态处理最好的一处，建议当作其他区域的模板：过滤范围写清了（`:590`「不是全量搜索」）、配对率如实写、拿不到与空分开。
唯一要改的是**位置**（见 §8 的复杂度裁剪）与**键盘可达**（见 A11Y-5：`role="listitem"` 盖在 `<button>` 上）。

### ST-3 日志页

| 态 | 现在 | 判定 |
|---|---|---|
| L | **无分支**：`logs` 初始 `[]`，首次 `tail_logs` 期间直接渲染**空态文案** | ❌ 加载与空同屏 |
| E | 「还没有日志。核心的 stdout/stderr 会被实时转发到这里 —— 如果一直是空的，通常意味着核心还没启动过。」（`:160-161`） | ⚠️ 文案把 E 说成结论 |
| D | 行列表 + 等级计数（实测：`全部125 / 信息123 / 警告1 / 错误1 / 调试`） | ✅ |
| P（筛选无命中） | 「当前筛选条件下没有日志（共 N 条，换个等级或清空关键字试试）。」（`:164`） | ✅ |
| F（`tail_logs` reject） | **没有分支**，显示成 E（实测：reject 后页面出现上面那段"还没有日志"，`.banner` 为 null、`aria-live` 0 个） | ❌❌ 见下 |

**ST-3-A（本文件最该修的状态缺陷之一）**：`store.tsx:207-213` 明确静默：

> `.catch(() => { /* 日志拿不到不影响主功能，静默即可 */ });`

后果：**读日志失败 → 用户被告知「通常是核心还没启动过」**——一个错误结论。
规格：
1. store 增加 `logsError: string | null`（`tail_logs` 失败时写入，成功后清空）；
2. 日志页三态互斥：`logsError` → 红 banner（原因 + 「重试」调用 `api.tailLogs(500)`）> `logs.length === 0` → 空态 > 列表；
3. 空态文案**去掉因果断言**，改「还没有日志。核心启动后 stdout/stderr 会实时转发到这里。」
4. 加载中显示「正在读取历史日志…」而不是空态。

### ST-4 节点延迟

| 态 | 现在 | 判定 |
|---|---|---|
| L（未测过） | 两个徽章都写「未测」（`:338-339`、`:359`，实测把 `latency` 清空后 4 行都是 `距离 未测 / 未测`） | ✅ 三态分得开 |
| L（正在测） | **只有全局 spinner**：按钮转圈 + 全部按钮禁用（`:81-84`）；**没有逐行状态** | ⚠️ |
| D | `距离 53 ms` + `可用`（`:337,360`） | ✅ |
| P（部分节点失败） | 失败行：`距离 61 ms` + `不可用`，错误原因只在 `title`（`:366`「经该节点取不到数据」） | ⚠️ 原因藏在悬停里 |
| F（全部失败） | 每行一个 `不可用`；无汇总 | ❌ 没有「全部节点都不可用，可能是本机网络问题」的判断 |

**ST-4-A**：测量期间每行应显示「测试中…」（现在行内无任何变化，用户看不出测到哪了）。
**ST-4-B**：`probeProgress` 是**死代码**：`store.tsx:41,59,227` 定义并放进 context，
但**全仓库没有任何页面读它**（grep 确认）。后端已经发 `nodes://probe-started{total}`（`ipc.ts:105`），
`onProbeStarted` 已把它写进 store（`store.tsx:190-196`）——**只差一行 UI**：按钮旁显示 `正在测试 3/40`。
**ST-4-C**：可用性徽章旁给失败原因一句短文本（不必全进 tooltip）；tooltip 只留详细说明。
**ST-4-D**：全部节点不可用时给一条汇总 + 「去日志看原因」的动作。

### ST-5 地球仪

| 态 | 现在 | 判定 |
|---|---|---|
| L | **无文字**：`data === null` 时只画球（`Globe.tsx:603` `const route = data?.route`），只有「重新定位」按钮 spinner（`:113-116`） | ⚠️ 首屏是空球，用户不知道在加载 |
| E（`route: null`，无 error） | **无分支**：`.facts` 不渲染（`:135` `{data?.route && <RouteFacts .../>}`），**没有任何说明**。此态**只有代码依据**（预览的 `globe_data` 恒返回 route，无开关；我用的是 reject 造 F） | ❌ 未实测，标 **【E 态未实测】** |
| D | 航线 + 飞机 + `.facts`（实测文字含起点、129 km、9.17 GiB、出口） | ✅ |
| P（有 route 且 `data.error`） | 两者都画（`:128-133` note + 航线） | ✅ 设计正确 |
| F（reject） | 红色 banner + `重新定位` + canvas 仍渲染 + 无 `.facts`（实测：banner 有内容、`facts:0`、`.empty` 0 个） | ⚠️ 见下 |

**ST-5-A（E 态）**：`route === null && !error` 时给一句「还没有可定位的出口 —— 先连接一个节点」，不要只留一个空球。
**ST-5-B（F 态）**：banner 里要写**下一步**（现在只有原因 `net::ERR_TIMED_OUT`；虽然旁边有「重新定位」按钮，但 banner 文本本身没有动作）。
**ST-5-C**：地球仪**无自动重试**（代码确认：只有按钮触发 `load()`），所以要保证「重新定位」在**任何**失败态下都可见（现在做到了，实测按钮在）。

### ST-6 快照（全局，决定 7 个页面的可达性）

| 态 | 现在 | 判定 |
|---|---|---|
| L | 页面级「正在加载…」/侧栏「正在加载…」（`pages/Dashboard.tsx:51`、`App.tsx:100`） | ✅ |
| E | 不适用 | — |
| D | 正常 | ✅ |
| P | 事件增量更新只覆盖 `runtime`/`traffic`（`store.tsx:116-130`）；`nodes`/`subscriptions`/`settings`/`helper`/`core` 只在事件触发时整取 | ⚠️ |
| F | **永久卡在加载**（实测：`snapshot` reject → 内容区 `⚠︎ 后端连接失败：helper 未响应 [关闭] 正在加载…`，侧栏「正在加载…」） | ❌ |

**ST-6-A**：`snapshot` 失败必须提供「重试」（调 `refresh()`）。当前唯一的按钮是「关闭」（`App.tsx:112`），关掉之后用户再无出路。

### ST-7 状态区分现状总表（本节的结论）

| 区域 | E 与 F 分得开？ | 证据 |
|---|---|---|
| 拓扑数据 | 分得开（F 整页替换）但 F 弱（无按钮、无标题） | 实测 `topology-fail.png` |
| 拓扑流量 | 分得开（「流量不可用」vs `↓0 B`）但车的行为与文案冲突 | 实测 ST-1-B |
| 最近连接 | ✅ 分得开且文案合格 | 实测 `topology-conn-unavailable.png` |
| 日志 | ❌ **分不开**（F 显示成 E，且给出错误原因） | 实测 `logs-tail-fail.png` |
| 节点延迟 | ✅ 分得开（未测 / 测不到 / 不可用） | 实测 + `Nodes.tsx:336-361` |
| 地球仪 | 分得开（F 有红 banner）但 E 无说明 | 实测 `globe-fail.png` + 代码 |
| 快照 | E 不适用；F 无重试 | 实测 `snapshot-fail.png` |
| 订阅用量 | ✅ `last_error` 行级展示 + 可操作建议（`Subscriptions.tsx:186-191`） | 代码 |

---

## 3. 反馈与等待

### 3.1 现在有的反馈机制（实测/代码）

| 机制 | 位置 | 说明 |
|---|---|---|
| 按钮内 spinner（11 处） | `Nodes.tsx:84`、`Subscriptions.tsx:69,94`、`Globe.tsx:114`、`Dashboard.tsx:118,129`、`App.tsx:209`、`Topology.tsx:1512`、`Settings.tsx:356,396,405` | 全局唯一 `busy` 驱动 |
| 纯禁用（无 spinner，20+ 处） | `Routing.tsx:60`、`Subscriptions.tsx:66,91,199,202`、`Nodes.tsx:81,121,300-321`、`Dashboard.tsx:114,126,232`、`Settings.tsx` 多处 | 用户看不出是"在忙"还是"点了没反应" |
| 进度（唯一完整实现） | `Settings.tsx:608`「下载中，请勿关闭…」+ 进度条（`update://progress`） | 只给自更新用 |
| 文字变化代替 spinner | `App.tsx:210` 连接/断开、`Nodes.tsx:85` 测试全部/筛选结果延迟 | 会造成文案跳变 |
| 成功反馈 | **全站没有**（无 toast、无「已保存」、无 undo；grep `toast`/`成功` 在 UI 代码 0 命中） | 唯一信号是 spinner 消失/表单清空/脏横幅消失 |

### 3.2 需要但缺失的（按严重度）

* **FB-1（高）**：**订阅拉取进度与取消**。订阅可能几 MB；现在只有一个 spinner（`Subscriptions.tsx:69`）。
  规格：按钮文案变「正在拉取（N 个节点）」，成功后显示「已导入 N 个节点」；提供取消（或至少超时说明）。
* **FB-2（高）**：**删除类操作的确认与撤销**。见 CON-1；「确认对话框」是反馈的一种。
* **FB-3（中）**：**延迟测试进度**。`probeProgress` 已存在于 store 但无人消费（`store.tsx:41,59,227`）。
  一行 UI 即可：`正在测试 {done}/{total}`。
* **FB-4（中）**：**测延迟失败时 `probing` 可能不复位**。`probing` 只在 `onLatency` 里 `setProbing(false)`（`store.tsx:187`），
  `run()` 的 catch 分支（`:83-85`）不复位；`runVoid` 同理。若后端在失败时不发 `nodes://latency`，按钮会**永久转圈+禁用**。
  **【真机未验证】**（预览里 `test_latency` 走 default 分支，不会失败）。规格：`run()` 的 `finally` 里复位 `probing`/`probeProgress`。
* **FB-5（中）**：**复制类动作无反馈**。实测点日志页「复制」后 `.banner` / `[role=status]` 数量均为 0，按钮文案不变。
  规格：复制成功给一次 2 秒的「已复制 N 行」提示（`role="status"`），失败沿用现有 console 降级但**同时**给可见提示。
* **FB-6（中）**：**busy 期间的点击是静默 no-op**。`store.tsx:77` / `:95`：`if (busyRef.current) return false;`
  —— 用户在忙时点别的按钮，什么都不会发生。规格：忙时禁止的控件必须 `disabled`（大部分已是），
  未 disable 的要给一次轻量提示（或统一在有 `busy` 时给顶栏一条「正在执行「连接」…」）。
* **FB-7（低）**：**成功反馈统一化**。「保存」「添加订阅」「删除」成功后应各有一条 `role="status"` 的短提示；
  破坏性操作成功后给「撤销」（见 CON-1）。

---

## 4. 可访问性

### 4.1 现状：ARIA 总量 = **5 处**（实测 grep，全 `apps/ui/src`）

| 位置 | 用法 |
|---|---|
| `App.tsx:182` | `role="group" aria-label="代理模式"` |
| `Routing.tsx:51` | `role="radiogroup" aria-label="分流预设"` |
| `Topology.tsx:618` / `:625` | `role="list"` / `role="listitem"`（后者盖在 `<button>` 上，见 A11Y-5） |
| `Topology.tsx:1322` | `<svg className="flow" … aria-hidden>` |

**不存在**：`aria-live`、`aria-pressed`、`aria-current`、`aria-expanded`、`aria-busy`、`aria-selected`、`aria-describedby`、
`role="alert"`、`role="dialog"`、`aria-modal`、`htmlFor`、`tabIndex`（全树 0 处）、`autoFocus`、`.focus(`、`Escape` 处理（0 处）。
实测探针：`live: 0`；`[role]` 只有 1 个（`group`）。

### 4.2 键盘可达性（真实 Tab 轨迹实测）

在 1280×860、`state=connected&view=dashboard` 下逐次发真实 Tab 键，记录 `document.activeElement`：

```
0  BODY
1-8  sidebar nav（仪表盘/节点/订阅/规则/拓扑/地球仪/日志/设置）
9-11 代理模式（直连/系统代理/TUN 模式）
12   顶栏「断开」
13   仪表盘「断开」
14   「切换节点」
15   「测试延迟」
16   环境自检与诊断 <summary>
17   → 回到 BODY，循环
```

**结论与规格**：

* **A11Y-1（高）**：**侧栏导航没有 `aria-current`**（`App.tsx:71` 只用 `is-active` 类）。
  屏幕阅读器读不出「当前在第几页」。规格：`aria-current={view === item.id ? "page" : undefined}`。
* **A11Y-2（高）**：**代理模式三个按钮没有选中态**（`App.tsx:186` 只用 `is-active`）。
  规格：`role="radiogroup"` 已有容器语义时改 `role="radio" aria-checked`，或保守做法 `aria-pressed`。
  **注意**：不要改成 radio input（会引入方向键语义与现有 onClick 冲突），优先 `aria-pressed`。
* **A11Y-3（高）**：**运行时状态圆点无文字可读**：`App.tsx:202` `<span className={\`dot${running ? " dot--on" : ""}\`} />`
  —— 空 span、无 `aria-label`、无 `title`。相邻按钮只表达**动作**（`App.tsx:210`「断开」/「连接」），不表达**状态**。
  规格：`<span className="dot" role="img" aria-label={...} />` 或把它换成 `<span className="sr-only">已连接</span>` + 图标。
  同样的圆点在仪表盘**有**文字（`Dashboard.tsx:78-79`）——按 A11Y-3 统一为「图标 + 文字」。
* **A11Y-4（高）**：**节点行无法用键盘选择**。`Nodes.tsx:266-270`：
  `<div className={\`list__row node-row…\`} onClick={busy ? undefined : onSelect} title=…>`，无 `tabIndex`、无 `role`、无 `onKeyDown`。
  实测：连续 26 次 Tab，焦点从搜索框直接跳到「测试全部延迟」→「手动添加」→ 每行的「二维码/删除」按钮，
  **从未落在任何 `.node-row` 上**；`rowTabbable` 探针显示 4 行全部 `tabindex: null, role: null`。
  后果：**键盘用户无法切换节点**（这是应用最核心的操作之一）。
  规格：行改 `<button>`（保持整行可点）或加 `role="row"`+`tabIndex={0}`+`onKeyDown(Enter/Space)`；
  行内「二维码/删除」保留为按钮并 `stopPropagation`（现在已有）。
* **A11Y-5（中）**：`Topology.tsx:623-628` 的 `<button type="button" role="listitem">` **覆盖了 button 的隐式 role**。
  屏幕阅读器不会把它读成可点按钮。规格：删掉 `role="listitem"`，或把 `role="list"` 容器改成 `<ul>`/`<li>` 结构。
* **A11Y-6（中）**：**地球仪 canvas 完全不可键盘操作**：`Globe.tsx:424-428` 只有 `onPointerDown/Move/Up/Leave`，
  无 `tabIndex`、无键盘处理。放大/缩小/复位三个按钮（`:431,441,451`）是键盘唯一入口，**旋转无法键盘完成**。
  规格：至少给 canvas `tabIndex={0}` + `aria-label`，并把「＋/－/复位」标注为「键盘替代路径」；
  旋转可给方向键（低成本：方向键调 `view.lon/lat`）。
* **A11Y-7（中）**：**没有 Escape 关闭**。全树 `Escape`/`keyCode === 27` 0 处（grep）。
  实测（真按键，CDP `Input.dispatchKeyEvent`）：拓扑页选中一条连接后按 Esc，`{before: 1, after: 1}` —— 高亮与详情面板**都不消失**。
  **注**：节点导出模态的 Esc 行为在预览里**无法验证**——预览没有实现 `export_node`，点击「二维码」后模态并未真正打开
  （探针 `modal: false`），所以这一条只有代码依据（`Nodes.tsx:159-165` 无键盘处理）。
  涉及：节点导出模态（`Nodes.tsx:159-165`）、拓扑连接详情（`Topology.tsx:657-663`）、日志诊断面板（`Logs.tsx:136-153`）。
  规格：三处都加 `useEffect` 监听 `keydown` 的 Escape → 关闭；模态额外加**焦点陷阱**与打开时 `focus()` 到标题/第一个按钮。
* **A11Y-8（中）**：**日志滚动区不可聚焦**：`Logs.tsx:155` `<div className="logs" … onScroll=…>` + `styles.css:525 overflow: auto`，
  无 `tabIndex`、无 `role="log"`。键盘用户无法用键盘滚动日志。规格：`tabIndex={0}` + `role="log"`。
* **A11Y-9（中）**：**模态没有对话框语义**：`Nodes.tsx:159` `<div className="modal">`、`:166` `<div className="modal__box">`
  都没有 `role="dialog"` / `aria-modal` / `aria-labelledby`。规格：补三者，标题用 `modal__title` 的 id。
* **A11Y-10（低）**：**没有 skip link**。8 个侧栏按钮在每个页面的 Tab 序列最前面（实测轨迹 1-8），
  键盘用户每次换页都要走 8 次 Tab 才能到内容。规格：`<a href="#content" class="skip-link">跳到主内容</a>` + `<main id="content" tabIndex={-1}>`。

### 4.3 焦点可见性

* 实测（Tab 轨迹里读 `getComputedStyle(activeElement).outline`）：
  **按钮/摘要项拿到 UA 默认焦点环** `outline: auto 1px rgb(0, 95, 204)`（可见，✅）；
  **文本输入拿到 `outline: none 3px rgb(230,236,245)`**（❌ 无焦点环）。
* 代码依据：`styles.css:290` `outline: none;` 在 `input[type=text|number|password], select, textarea` 的基规则里，
  替换是 `styles.css:293-296` `border-color: var(--accent);` —— **只换 1px 边框颜色**，在深色低对比背景下几乎看不见。
* **A11Y-11（高）**：给所有输入控件补可见焦点：`input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px }`，
  并保留现有 `border-color` 变化。全站 `:focus-visible` 目前 **0 处**。
* **A11Y-12（中）**：为按钮补作者侧焦点样式（现依赖 UA 默认），并在 `prefers-reduced-motion` 之外不要用动画表达焦点。

### 4.4 屏幕阅读器的动态内容

* **A11Y-13（高）**：**全站没有 live region**（实测 `aria-live` 数量 0）。
  受影响的异步更新：日志新行（`Logs.tsx:169`）、流量速率（`Dashboard.tsx:146`）、延迟徽章（`Nodes.tsx:288`）、
  拓扑 2s 轮询（`Topology.tsx:211`）、错误横幅（`App.tsx:109`）。
* 规格：
  1. **错误横幅**：`App.tsx:109` 加 `role="alert"`；所有 `banner--error` 同理（`Nodes.tsx:114`、`Globe.tsx:120`、`Topology.tsx:1516`）。
  2. **拓扑轮询**：不要做 live（2s 一次会刷屏）。只在**状态发生质变**时（如「已连接 → 未连接」）用 `role="status"` 播报一次。
  3. **日志**：容器 `role="log"` + `aria-live="polite"`，并**只在「跟随」开启时**开启 live（否则会打断用户）。
  4. 所有 spinner 加 `aria-busy` 或配 `role="status"` 文本（「正在连接…」），现在 spinner 只是空 span。

### 4.5 不依赖颜色传达状态

| 区域 | 颜色是不是唯一编码 | 证据 | 规格 |
|---|---|---|---|
| 顶栏运行圆点 | **是** | `App.tsx:202` 空 span + `styles.css:472-473` `.dot--on { background: var(--ok) }` | **A11Y-14（高）**：补文字/图标（见 A11Y-3） |
| 仪表盘状态词 | 否（有文字） | `Dashboard.tsx:79` | ✅ 保留 |
| 仪表盘延迟徽章 | **分级靠颜色**，数字是文字 | `Dashboard.tsx:85` `badge--${latencyTier(rtt)}`，`types.ts:452-454` 只有 fast/ok/slow 类 | **A11Y-15（中）**：数字后面补「快/一般/慢」词，或给 `title` 同时加文字 |
| 节点延迟/可用 | 否（有「可用/不可用/未测」文字） | `Nodes.tsx:296,359-360` | ✅ |
| 拓扑连线语义 | **是**（SVG 内只有颜色；`aria-hidden`） | `Topology.tsx:1330-1332` + `:1322` | **A11Y-16（中）**：图例/卡片已有文字（`:353-366`、`:414`），所以**数据没丢**；但色盲用户**读不出某条线通向哪类出口**。加线型第二编码（与 `DESIGN-REVIEW.md` **D1 相同**）→ 本文件支持 D1，并要求图例同步显示线型 |
| 地球仪标记 | **是**（canvas 画色 + 画字，无 DOM 替代） | `Globe.tsx:641-642`；`.facts` 里有起点/出口文字（`Globe.tsx:142-157`） | **A11Y-17（中）**：色盲用户在球上分不清起点/节点；规格：两个标记用**形状**区分（起点圆、节点方/菱形）—— 零成本且不需要图例 |
| 日志等级 | **是**（行首色条 + 文字颜色，无等级词） | `Logs.tsx:169` 无等级 span；`styles.css:540-541,1069-1070` | **A11Y-18（高）**：等级筛选标签有计数，但**行内没有等级词**。规格：在 `log-line__src` 后加等级缩写（`ERR`/`WRN`），或对 `warn/error` 加符号（`!`/`✕`）。这是排查场景的核心信息 |
| 订阅错误 | 否（有文字） | `Subscriptions.tsx:186` | ✅ |
| 用量告警 | 否（有「即将用尽/余量偏低」） | `Subscriptions.tsx:227-228` | ✅ |

### 4.6 动效

* `prefers-reduced-motion` **只有一处**：`styles.css:1774-1777` 关掉 `.flow__highlight` 动画。
* **A11Y-19（中）**：以下动画在减少动效偏好下仍会跑：
  * `.spin` 旋转（`styles.css:581`，所有 spinner）；
  * 拓扑货车 JS 动画（`Topology.tsx` 的 rAF 循环）与地球仪飞机（`Globe.tsx` 的 rAF 循环）；
  * 若干 CSS transition（`styles.css:917,1118,1244,1300,1445`）。
  规格：`@media (prefers-reduced-motion: reduce)` 里把 `.spin` 改为静态图标；
  两个 canvas/SVG 动画循环在 `reduce` 时**只渲染一帧**（并保留静态位置的车/飞机，不能空场）；
  transition 改 `none`。**验收**：开启系统「减少动态效果」后，拓扑与地球仪仍在同一位置显示车/飞机，但不动。

---

## 5. 错误文案

**判定标准**：一句话必须同时说清 ① 发生了什么 ② 能做什么。

### 5.1 最差的 6 条（含理由）

| # | 现在（逐字） | 位置 | 为什么差 | 建议改法 |
|---|---|---|---|---|
| MSG-1 | 「还没有日志。核心的 stdout/stderr 会被实时转发到这里 —— 如果一直是空的，通常意味着核心还没启动过。」 | `Logs.tsx:160-161` | **把「读不到日志」说成「核心没启动过」**——一个可验证为假的结论（实测：`tail_logs` 失败时原文照显）。用户会去查核心，而问题在 IPC | 「还没有日志。核心启动后 stdout/stderr 会转发到这里。」+ 失败时改显示「读日志失败：{原因}［重试］」 |
| MSG-2 | 「解析失败，请检查链接格式」 | `Nodes.tsx:62` | **丢掉了真实原因**（`run()` 已经把原因写进全局 `error`，这一行却用固定文案覆盖）。用户不知道是 URI 不支持、字段缺失还是 base64 坏了 | 「解析失败：{后端原文}。检查链接是否完整（vmess/vless/trojan/ss）或改贴订阅正文」 |
| MSG-3 | 「上次运行出错：{runtime.last_error}」 | `Dashboard.tsx:274` | 只有**发生什么**，没有**做什么**；且这是 rank 0 的最高级提示 | 追加动作：核心启动失败 → 给「查看日志」按钮；helper 失败 → 「去设置」 |
| MSG-4 | 「取拓扑失败：{loadError}（拓扑来自运行中的配置…）」 | `Topology.tsx:242-244` | 原文是 `dial unix …: no such file or directory` 这类系统错误，用户读不懂；且**没有重试按钮**、不说会自愈 | 「读不到运行中的配置（核心没在运行）。每 2 秒自动重试。［立即重试］」，系统错误收进「详情」 |
| MSG-5 | 「{err}」（DestChecker / Settings DNS 探测 / helper 错误横幅） | `Topology.tsx:1516`、`Settings.tsx:471,379` | 直接把后端/系统错误串上屏，可出现 `dial unix …` / `Permission denied` 原文；无动作 | 统一成「{人话的一句话}（{原文}）」+ 一个动作 |
| MSG-6 | 「未找到核心 / 缺少 Xray 可执行文件」（状态词） | `Dashboard.tsx:62` | 状态词只给结论；**唯一带动作的那条 notice 没有按钮**（`Dashboard.tsx:278-291` 只写「请把 xray 放到 Resources 或设置里指定路径」，无 action，而 stale/notice 两条都有 action） | 给「去设置指定路径」按钮（与 `:335` 的「去处理」同样处理） |

### 5.2 文案合格、不要动的（作为正面模板）

* `Topology.tsx:590` 「过滤范围：已取到的最近 42 条（不是全量搜索）」——**说清了边界**（这是本项目反复强调的诚实口径）。
* `Topology.tsx:443-446` 「这几个出口的字节数读不到：核心只在 StatsService 里报流量，而它不统计 UDP 出站与本机回环。所以这里显示连接数…」——实测 `dns-out 4769 条连接` / `api 5374 条连接` 与之对应。
* `Topology.tsx:732-735` 「本视图没有这条连接的字节数与持续时间…所以这里不显示，也不推算。」
* `Subscriptions.tsx:186-191` 「上次更新失败：…（已有节点仍然可用，可以稍后重试）」。
* `Dashboard.tsx:301-308` 「检测到上次异常退出遗留的网络配置…建议立即回滚」+「立即修复」。
* `App.tsx:207` `title`「直连模式下无需启动核心」。

### 5.3 文案总则（写进实现约定）

* MSG-R1：**禁止只给结论的因果句**。凡含「通常意味着…」「应该是…」的文案，删掉因果，改成陈述事实。
* MSG-R2：`loadError` / `err` / `last_error` 类字符串**一律不要直接当正文**：包一层「人话 + 原文（可折叠）」。
* MSG-R3：每条 error banner 必须**恰好一个**动作按钮（重试 / 去修复 / 看日志）。
* MSG-R4：空态文案不得使用「核心没启动」这类可验证的断言，除非数据能证明它。

---

## 6. 跨页一致性

### 6.1 确认行为：**全站没有任何确认对话框**（实测+代码）

| 操作 | 位置 | 确认？ | 危险样式？ | 实测 |
|---|---|---|---|---|
| 删除节点 | `Nodes.tsx:151` → 按钮 `:312` `btn btn--ghost btn--danger` | **无** | 是 | 点击后立即发出 `delete_node`，`window.confirm` 调用次数 **0**，`.modal` 0 个 |
| 删除订阅 | `Subscriptions.tsx:106` → 按钮 `:202` `btn--danger` | **无** | 是 | 点击后立即发出 `remove_subscription`，confirm 0 次 |
| 清空日志 | `Logs.tsx:131` `btn btn--ghost` | **无** | **否** | 点击后立即发出 `clear_logs`，confirm 0 次；且**不可恢复** |
| 卸载 helper | `Settings.tsx:416-422` | **无** | 是 `btn--danger` | — |
| 修复网络（回滚遗留配置） | `Settings.tsx:409-415` | **无** | **否** | — |
| 断开连接 | `Dashboard.tsx:113`（danger）/ `App.tsx:204`（**非** danger） | 无 | **两处不一致** | — |
| 更新核心 / geo / 客户端（替换二进制） | `Settings.tsx:547,554,602` `btn--primary` | 无 | 否 | — |

* **CON-1（高）**：**删除节点 / 删除订阅 / 清空日志**必须走统一模式：**二次确认（内联两段式）+ 成功后 5 秒撤销**。
  规格（三处共用同一个 `ConfirmButton` 组件）：
  * 第一次点击 → 按钮变「确认删除？」（红色，`aria-live="polite"` 播报），3 秒无操作自动还原；
  * 第二次点击执行；成功后出现 `role="status"` 提示 + 「撤销」；
  * 撤销需后端支持（删除是可逆的：节点/订阅都在配置里），**若后端不支持撤销，则至少保留双重确认**。
* **CON-2（中）**：`btn--danger` 语义统一：`断开` 在两处必须一致（推荐都用 danger），`清空日志`、`放弃未保存改动`、`回滚网络配置` 这些**不可逆**操作应带 danger 或至少次级确认。
* **CON-3（中）**：`确认` 与 `成功` 两种反馈都要有，现在**两者都没有**。

### 6.2 按钮位置与文案

| 问题 | 证据 | 规格 |
|---|---|---|
| 同一动作三个标签 | Dashboard `更新全部订阅`（`Dashboard.tsx:235`）/ Subscriptions `更新全部`（`:95`）/ 单条 `更新`（`:199`） | 统一为「更新订阅」/「全部更新」 |
| 主操作位置不统一 | 仪表盘在状态区（`Dashboard.tsx:111`）、订阅在段落行内 + 右栏（`Subscriptions.tsx:64,84`）、地球仪在标题右（`Globe.tsx:113`）、节点在工具栏（`Nodes.tsx:79`）、设置散在 9 张卡里 | 页面级主操作固定在**同一条位置**（推荐页头右侧） |
| `row--between` 是空类 | `Globe.tsx:105` 用它排版，但 `styles.css` 里**没有 `.row--between` 规则**（grep 只命中 JSX 一处） | 要么补规则，要么改用 `.page__bar`（目前只有订阅页用） |
| 页面骨架三套 | `.page`(820px) / `.dash`(640px) / `.set`(820px)；Logs 用 `.logs-page`；Nodes/Logs/Settings **没有 `page__title`**（实测 `null`） | 统一页头组件：`title + desc + 主操作`；宽度统一 |
| 「关闭」按钮三种样式 | `Nodes.tsx:212` `btn` / `Logs.tsx:147` `btn--ghost` / `App.tsx:112` `btn--ghost` | 统一 `btn--ghost` |

### 6.3 错误呈现机制（5 套并存）

1. 全局 banner（`store.error`，`App.tsx:108-116`）——但**拓扑页与地球仪页自己 `useState` 存错误，不走 store**，
   所以这两页的错误**不会**出现在全局横幅（`Topology.tsx:168,185`；`Globe.tsx:83`）。
2. 页级 `banner--error`（`Nodes.tsx:114,172`；`Globe.tsx:120`；`Topology.tsx:1516`）。
3. 页级灰色 `.note`（`Topology.tsx:242,267,525,613`；`Routing.tsx:82`；`Nodes.tsx:130`）。
4. 行级 `last_error`（`Subscriptions.tsx:186`；`Settings.tsx:379,469,567`）。
5. 没有 toast。

* **CON-4（中）**：统一为两层：**页内区域态**（`.note`/`.banner--error`）+ **全局动作失败**（store banner）。
  拓扑/地球仪的加载错误要么进 store，要么在页头统一位置——不能一个页面「红 banner + 按钮」、另一个「灰 note 无按钮」。

### 6.4 与 `docs/ui/topology/DESIGN-REVIEW.md`（33 条）的关系

**不重复**：视觉层级（A1-A5）、颜色对比/合成色（B1-B4、D2、D3）、线密度（C1-C3）、响应式塌陷（G1）、
重复 CSS（H1）、文档不一致（H2）全部属于 `art-designer` 或 `frontend-dev`，本文不重复立项。

**冲突 / 需要裁决的 4 处**：

1. **E2（流量不可用时车应停止）与本文件 ST-1-B 冲突。**
   `DESIGN-REVIEW` 建议「不启动位置动画」；现状代码是**故意保留上次读数**以避免车数 13→9→13 抖动
   （`Topology.tsx:1280-1294`）。**我的裁决**：保留「不跳」，但**改画法**（空心 + 虚线）并补一行「车上仍是上次读数」。
   理由：完全停住会让「量小」与「不通」再次混在一起，而这正是本项目刚修好的坑。
2. **D4（拓扑 SVG `aria-hidden` 不值得补文本替代）—— 我同意，但边界要写清。**
   同意理由是数据没丢（卡片 + 规则链都有 DOM 文本）。
   但**地球仪不能照此办理**：`Globe.tsx:641-642` 的标记文字是 canvas 画上去的，DOM 里没有替代，
   所以 A11Y-17 要求「用形状区分标记」，这一条是新增的。
3. **E7（预览复现不出异常态）已过期。** 现在有 `?traffic=unavailable` 与 `?connections=…` 四个开关；
   但**仍缺** `traffic-error-after-data`（ST-1-B 需要）、`logs-error`、`globe-empty`、`empty-topology` 四个开关。
   **规格**：预览补这 4 个开关，否则本文件的 ST-1-B / ST-3 / ST-5-A 无法回归。
4. **E1（把 `traffic_error` 提示移到图上方）我不采纳原方案。**
   位置不是根因；根因是**卡片上的数字没有"不可信"标记**。规格改为：`trafficOk === false` 时
   卡片数字位本身显示「流量不可用」并加一个 `⚠` 图标（现在文字已改，但**没有图标**，色/形层面仍与真实读数同形）。

---

## 7. 与产品/视觉分工的界面

* 本文**不决定**优先级（`product-manager`）、**不决定**颜色/字号/动效曲线（`art-designer`）。
* 本文提出的所有「改法」都只规定**行为与文案**：控件的角色、状态、反馈、键盘路径、错误文本。
  视觉实现（虚线/空心/图标样式）由 `art-designer` 定；本文只要求「必须存在一个非颜色、非文字的区分手段」。
* 本文与 `DESIGN-REVIEW.md` 的重叠**只有 3 条**（E6 空拓扑、D1 线型编码、E5 失败态一致性），
  已在 §6.4 逐条说明关系，其余 30 条不重复。

---

## 8. 结论：最该先修的 3 个交互缺陷 + 最该砍的复杂度

### 8.1 最该先修的 3 个（每条给可复现步骤）

#### FIX-1（最高）换网恢复没有任何界面反馈通道

* **复现**：
  1. `cd apps/ui && npx vite --port 5311`，打开 `?preview=1&state=connected&view=dashboard`；
  2. 用 CDP 注入「注册回调捕获」脚本（`/tmp/interaction-review/s3.json` 的 `event-runtime-dropped-then-recovered`）；
  3. 喂一条 `runtime://changed{runtime:{running:false},traffic:{…}}` → 界面 900ms 内变「未连接」（**只有这一种事件能改变状态**）；
  4. 把 `notice` 注入快照并强制刷新 → 结果同屏出现「已连接」+「网络中断，正在自动恢复…」（截图 `shots/dashboard-recovery-event.png`）。
* **依据**：`events.rs:45-53`（载荷只有 runtime/traffic）、`core.rs:493,516`（写 notice）、`snapshot.rs:46`（notice 只走快照）、
  `store.tsx:197-199`（只在 nodes/subs/settings 事件时拉快照）。
* **规格**：FLOW-2-A/B/C/E。
* **影响**：这是用户明确说了「最在意」的场景；不修则自动恢复在界面上等于不存在。

#### FIX-2 日志页把「拿不到日志」显示成「核心没启动过」

* **复现**：
  1. 打开 `?preview=1&state=connected&view=logs`；
  2. 注入改写：让 `tail_logs` reject（`s3.json` 的 `logs-tail-fail-rerun`）；
  3. 页面显示「还没有日志。核心的 stdout/stderr 会被实时转发到这里 —— 如果一直是空的，通常意味着核心还没启动过。」（截图 `shots/logs-tail-fail.png`），
     `.banner` 为 `null`、`[aria-live]` 数量 0。
* **依据**：`store.tsx:207-213` 静默 catch；`Logs.tsx:156-166` 无错误分支。
* **规格**：ST-3-A（四个子项）。
* **影响**：这是本项目反复修过的「查不到 ≠ 0 / 没有」类缺陷，**在日志页仍未修**；而且它给出的是**错误原因**，比不显示更糟。

#### FIX-3 破坏性操作零确认、零撤销

* **复现**：
  1. `?preview=1&state=connected&view=nodes`，注入 `window.confirm` 计数包装；
  2. 点第一行「删除」→ 探针：`{confirms: [], cmds: [...,"delete_node"], modal: 0, nodes: 4}`（节点数不变是预览 mock 所致，命令确实发出）；
  3. 订阅页同理 → `remove_subscription`；日志页点「清空」→ `clear_logs`，全部 `confirms: []`。
* **依据**：`Nodes.tsx:151`、`Subscriptions.tsx:106`、`Logs.tsx:131`；全仓库 `confirm` 0 命中。
* **规格**：CON-1/CON-2/CON-3。
* **影响**：节点与订阅在机场/多节点场景下价值不低（配置里手写的参数删了就没了）；「清空日志」是排障现场的唯一证据。

### 8.2 最该砍掉的交互复杂度

**砍：拓扑页把「聚合流量图」与「单连接浏览器」塞在同一页。**

* 现状实测：拓扑页从上到下是 **网络流动图（含入口/出口卡片、18 条线、11 辆车）→ 内部通道说明 →
  「某个地址会走哪条路」判定 → 最近连接（600 条时只渲染最近 100，另有 137 条被挤掉的说明）→
  过滤范围说明 → 配对率统计 → 连接详情（8 行字段 + 两段免责说明）→ 规则链（8 条）**，
  共 4 个 `page__title` 区块，一页里同时要"看懂一张图"和"读一份日志"。
* 代价（可量化）：默认 1080×720 下连接列表要把判定与规则链顶到折叠线下很远；
  `?connections=busy` 时 `conn-list` 渲染 100 个按钮（实测 `rows: 100`），DOM 与滚动长度都由它主导。
* **建议砍法（任一，按性价比排序）**：
  1. **连接列表移到「日志」页**（它本质是访问日志的另一种视图），拓扑页只保留 `点日志里的连接 → 回拓扑高亮`
     的**跳转**（跨页高亮而不是同页列表）；
  2. 或保留同页但**默认折叠**：只显示「最近 10 条」+「查看全部」，把过滤/范围说明/配对率收进展开区；
  3. **一定要砍的附带项**：`conn-summary` 里的配对率统计（「配到域名 26/42 条（62%）」）与详情里重复的免责声明合并一处——
     现在同一件事（配对可能不准）在列表项（`*` + title）、汇总、详情里**说了三遍**。
* **同时砍的次要复杂度**：
  * 仪表盘的「还有 N 条提示」是**单向展开无收起**（`Dashboard.tsx:168-172`），且 notice 有 5 类排序：
    砍到「1 条主提示 + 1 个「全部提示」入口（可关）」；
  * 顶栏与仪表盘各有一个主连接按钮（`App.tsx:203`、`Dashboard.tsx:112`），两处样式/状态不一致（一处 danger 一处普通）：
    保留顶栏一个，仪表盘改为状态展示 + 复用同一组件。

---

## 9. 未验证 / 不确定（诚实清单）

1. **真机行为全部未验证**：自动重连/看门狗是否真的在换网与唤醒后 10–30s 恢复、Gatekeeper/helper 流程、
   `window.set_title` 网速、托盘、真机 WKWebView 的焦点环与 `prefers-reduced-motion`。本文相关结论均标 **【真机未验证】**。
2. **ST-5-A（地球仪 `route: null` 且无 error）**：只有代码依据（`Globe.tsx:135`），**未实测**——
   预览的 `globe_data` 恒返回 route，没有对应开关。
3. **`probing` 卡死（FB-4）**：只有代码依据；预览里 `test_latency` 不会失败，**未实测**。
4. **订阅拉取的真实耗时/进度需求**：预览是瞬时返回，**未测真实拉取时长**（结论来自代码：只有 spinner）。
5. **Tab 顺序**：实测于 1280×860 的预览页；真实窗口尺寸（1080×720）与 Zoom 级别下未逐一重测。
6. **键盘事件的最后一步**：Escape 的"无效果"结论同时来自真实按键（CDP `Input.dispatchKeyEvent`，用于 Tab/Escape 轨迹）
   与代码 grep（`Escape`/`Escape` 处理 0 处），两者一致；但模态的焦点陷阱未做 AT（VoiceOver）实测。
7. **对比度/色盲**：本文不裁量颜色（`art-designer` 负责实算）；`DESIGN-REVIEW.md` 的色盲距离数字由它引用，我未复算。
8. **预览在会话期间被重构**（`preview.ts` → 5 个 `preview*` 模块）：本文所有开关与注入都已在 `7ffbbe5` 上重跑通过，
   但若后续再加/改开关，§2 的部分结论需要重新 pin。
