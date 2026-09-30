# INTERACTION · 交互规范（逐状态逐字文案 + 证据条件）

> 所有者：ux（task-3）。配套 `IA.md`。本文给出界面**逐字**文案与行为，
> 并对每一条回答同一个问题：**界面在什么证据下才允许这么说？**
>
> 唯一的输入是 `crates/xt-contract`（`ConnectionView` / `Event` / `ErrorBody` / `DaemonHello`）。
> 界面不得引入契约以外的任何事实来源（I3）。

## 0. 三条总则

| 总则 | 含义 | 落到交互上的硬规则 |
| --- | --- | --- |
| **I1 无等待** | 不用时间推进状态 | 所有状态变化只由「一次请求的响应」或「一个事件」驱动；不写 `setTimeout`/`setInterval`/轮询；测试里等待只用 `waitFor` 与事件 |
| **I2 无回落** | 一条意图只有一条路径，失败即终态 | 不出现「重试 / 自动恢复 / 自动换节点 / 已为你切换」；失败后停在失败态并显示真实原因 |
| **I3 无假话** | 每个显示项都能追到真实字段 | 未知显示「未采样 / 未知 / 未探测」；**绝不把未知显示成 0**；失败原文照抄 |

本文每条按统一格式写：

```
状态 / 证据（必须是哪些字段的值）→ 逐字文案 → 行为 → 反例
```

---

## 1. 连接生命周期

### 1.0 状态总表

舞台只有四个值（`Stage` 封闭枚举），界面**逐字**映射，不做近义词替换：

| `ConnectionView.stage` | 状态徽章逐字 | 连接按钮 | 断开按钮 | 其它可见 |
| --- | --- | --- | --- | --- |
| `disconnected` | **未连接** | 「连接」，可用（未选节点时 disabled，理由见 1.1） | 不渲染 | `last_error`（若有）原样 |
| `connecting` | **连接中** | 「连接」，**disabled** | **disabled** | `phase-label` + `phase-code` |
| `connected` | **已连接** | 「断开」→ 实际是断开按钮可用 | 「断开」，可用 | `connected_since_ms` → 连接时长（可选）；`stats` 真值 |
| `disconnecting` | **断开中** | **disabled** | **disabled** | 无 |

**允许这么说的证据**：徽章文本**只**由 `stage` 决定。`phase`、`last_error`、
`stats` 的存在与否都**不得**改变徽章。`connected` 不得在任何其它 stage 下出现
（`00-CONTRACT-FREEZE.md` §4：`stage == Connected` ⟺ 数据面已被证明可连）。

**反例（禁止）**：`awaiting_ready` 时显示「已连接」；`last_error` 存在时把徽章改成
「连接失败」却又没有对应 `stage`（契约里失败态就是 `disconnected` + `last_error`，
徽章必须仍是「未连接」）；把 `connecting` 显示成「正在重试」。

### 1.1 `disconnected`

| 项 | 内容 |
| --- | --- |
| **证据** | `stage === 'disconnected'`；`phase === undefined` |
| **逐字文案** | 徽章「未连接」；当前节点为 `node_id`（若 `node_id` 在 `NodeView[]` 里存在则显示 `name`，否则显示 `id` 本身；`node_id === undefined` → 「未选择」） |
| **连接按钮** | 文本「连接」。若 `SettingsView.selected_node === undefined`（且当前无 `node_id`）→ `disabled`，并在按钮旁显示真实原因**「未选择节点」**。若 `last_error?.code === 'unsupported'` 或其它不可用原因，同样不假装可点 |
| **行为** | 点击「连接」→ 发出 `Request::Connect{node_id, mode:'proxy'}`，**不本地乐观改成 connecting**；等 `Event::State{stage:'connecting'}` 到达才变 |
| **反例** | 点击后立刻显示「已连接」；未选节点却让连接可点然后失败；显示「请先安装 helper」（本轮无 helper） |

### 1.2 `connecting`（四个 phase，逐字）

**证据**：`stage === 'connecting'`，`phase ∈ {preparing_config, starting_core, awaiting_ready, committing_routes}`。
`phase` 为 `null` 时只显示徽章「连接中」，不编造阶段。

| `phase` | `phase-label` 逐字 | `phase-code` 逐字（原始值） | 这一句在解释什么 |
| --- | --- | --- | --- |
| `preparing_config` | **正在生成配置** | `preparing_config` | 还在准备配置，核心还没起来 |
| `starting_core` | **正在启动核心进程** | `starting_core` | 核心进程正在被拉起 |
| `awaiting_ready` | **正在等待核心可连** | `awaiting_ready` | **核心尚未被证明可连**——这一句必须让用户知道还不能说「已连接」 |
| `committing_routes` | **正在提交路由** | `committing_routes` | 仅 TUN 语义；本轮 proxy 不应出现，若出现就如实显示（不隐藏、不美化） |

**行为**：连接按钮与断开按钮**都 disabled**（契约里 `Connecting` 没有 disconnect 转换）。
不显示倒计时、不显示「预计剩余时间」（契约里没有这个字段）。

**允许这么说的证据**：`phase` 必须来自 `ConnectionView.phase`。`phase === null` 时
不显示任何阶段文案，**也不得用「正在连接…」以外的猜测**。

**反例**：显示「即将完成」；显示进度百分比；`awaiting_ready` 显示「已连上，正在优化」。

### 1.3 `connected`

| 项 | 内容 |
| --- | --- |
| **证据** | `stage === 'connected'` |
| **逐字文案** | 徽章「已连接」；按钮「断开」 |
| **额外事实** | `datapath.pid`/`version`/`ready_at_ms` 与 `connected_since_ms` **只在字段非空时**渲染该行；`stats` 按 §4 处理 |
| **行为** | 点击「断开」→ `Request::Disconnect`，等事件把 `stage` 推进到 `disconnecting` |
| **反例** | 只要 socks 端口开着就把徽章改「已连接」（契约判据是数据面被证明可连，不是端口存在）；显示「已保护」「安全」等契约里没有的评价 |

### 1.4 `disconnecting`

| 项 | 内容 |
| --- | --- |
| **证据** | `stage === 'disconnecting'` |
| **逐字文案** | 徽章「断开中」；两个按钮都 disabled |
| **行为** | 等 `Event::State{stage:'disconnected'}` |
| **反例** | 显示「已断开」；显示「正在还原网络设置」（helper 本轮不做，这是假话） |

---

## 2. 切节点（选择就用，失败就是失败）

| 项 | 内容 |
| --- | --- |
| **证据** | `NodeView.id` / `NodeView.name`；`SettingsView.selected_node`；已连接时用 `Request::SwitchNode` |
| **逐字说明（页面固定文案）** | **「选择就用：失败会如实报错，不会自动换下一个节点。」** |
| **按钮文案** | 未连接：「选为当前节点」（走 `PatchSettings{selected_node}`，因为契约里未连接时 `switch_node` = `Conflict`）；已连接：「切换并立即使用」（走 `Request::SwitchNode`） |
| **行为** | 点一个节点 = 一个意图。切换后**不本地乐观**改当前节点；等 `Event::State{node_id}` 到达才改。切换失败 → 停在失败态，`node_id` 保持真实的旧值或变为契约给出的值，并显示 `last_error` 原文 |
| **禁止** | 「已为你切换到最快节点」「正在尝试其它节点」「已切换到备用节点」「自动选择最佳节点」；失败后自动再试下一个 |
| **允许这么说的证据** | 只有当 `Event::State` 里的 `node_id` 变成目标节点时，界面才允许显示该节点为当前节点 |

**为什么这条值得单独写**：旧仓库真实事故 `08-failure-modes.md` §B H1 记录过
「切到坏节点后彻底断网」，当时的修法是**自动退回上一个可用节点**——本轮的契约
明确删掉了这条路径（`ErrorCode` 无 `Retry/Fallback`，`00-CONTRACT-FREEZE.md` §6）。
界面必须把「没有回落」这件事**主动告诉用户**，否则用户会把「停在失败」误解成「还在重试」。

---

## 3. 失败（原样展示，不加戏）

| 项 | 内容 |
| --- | --- |
| **证据** | `ConnectionView.last_error: ErrorBody {code, message, detail?}`；订阅失败用 `SubscriptionView.last_error`；探测失败用 `ProbeResult.error`；传输层失败用 store 的 `transportError` |
| **逐字文案** | **`code` 与 `message` 原样输出**（`data-testid="last-error"` 同时含两者）。`message` 允许换行显示，但**不得改写、不得翻译、不得摘要** |
| **行为** | 失败是终态：不自动重试、不自动换节点、不倒计时。用户能做的是自己再点一次（那是一次**新意图**，不是「重试」话术） |
| **禁止话术** | 「重试」「自动重试」「自动恢复」「自动重连」「已为你切换」「已切换」「已保护」「已自动」「正在恢复连接」「稍后会自动重试」 |
| **`unsupported`（新增）** | 当 daemon 用 `ErrorCode::Unsupported` 表示「本版本不提供该能力」时，界面同样**原样**显示其 `message`；不得为它造「即将支持」之类话术 |
| **允许这么说的证据** | 只有 `last_error !== undefined` 时才允许出现错误块。不允许用 `stage === 'disconnected'` 自己编一句「连接失败」——没有 `last_error` 时「未连接」只是一个状态，不是失败 |

**与徽章的关系**：错误块是**附加**信息，不替换状态徽章（§1.0）。

---

## 4. 未采样（未知 ≠ 0）

这是本项目 I3 的核心。规则只有一条：

> `stats === undefined`（或 `null`）→ 显示**「未采样」**；**不得出现任何形式的 0 字节**。

| 项 | 内容 |
| --- | --- |
| **证据** | `ConnectionView.stats: StatsView \| undefined` |
| **逐字文案** | `stats-uplink` 与 `stats-downlink` 的文本**恰为**「未采样」；`stats-sampled-at` 也显示「未采样」 |
| **视觉** | 「未采样」与真实数值必须**可区分**：真实数值带单位（B/KB/MB/GB）与采样时间；「未采样」不带单位、不带 `0`、不画空进度条假装是 0 |
| **能力门控** | 若 `hello.capabilities` 不含 `stats`，整个统计卡**不渲染**（而不是渲染一个「未采样」的卡，让人以为有能力只是没数据） |
| **有值时的规则** | `stats.uplink_bytes`/`downlink_bytes` 必须如实渲染注入值（格式化自由，但必须是该数值的忠实表示）；`sampled_at_ms` 渲染为本地时间 |
| **允许这么说的证据** | 「未采样」只在 `stats == null` 时出现。`stats != null` 且两个字节数都是 `0` 时，显示的是**真实 0 B**（这是真值：已经采样成功、确实还没有流量）——此时**不允许**显示「未采样」 |
| **反例** | 显示 `0 B` / `0 B/s` / `0 字节` 表示未知；显示上一帧的旧值；显示一个正在跳动的假速率；用「暂无数据」模糊过去（那既可能是未采样也可能是 0） |

**这一条的历史代价**（`crates/xt-contract/src/model.rs` 顶部注释）：
「把未知显示成 0 是本项目最不能接受的谎——用户会以为没有流量，其实只是没采样。」

### 4.1 延迟（同族规则）

| 证据 | 逐字文案 |
| --- | --- |
| `NodeView.probe === undefined` | **「未探测」** |
| `NodeView.probe.ttfb_ms !== undefined` | **`<n> ms`**（`n` 为真实整数） |
| `NodeView.probe.error !== undefined` | `code` + `message` 原文 |

禁止：未测显示 `0 ms`；失败显示 `-1 ms`；用颜色代替文字（颜色可以加，文字必须有）。

---

## 5. 空态（逐字）

| 空态 | 证据 | 逐字文案 | 必须给出的下一步 | 禁止 |
| --- | --- | --- | --- | --- |
| 无节点 | 节点列表长度为 0 | **「暂无节点」** | 若 `subscription_fetch` 已宣告 → 指向设置页订阅区的添加动作；**未宣告（本轮真实情况）→ 不给出任何入口**，只说明节点目录由 daemon 侧决定 | 渲染一个可点的「连接」（未选节点时连接必须 disabled 并说明「未选择节点」）；给一个点了会报 `unsupported` 的「添加订阅」死入口 |
| 无日志 | 日志列表长度为 0 | **「暂无日志」** | 无 | 用占位假行填充；显示「运行正常」 |
| 无订阅 | 订阅列表长度为 0（且 `subscriptions` 能力已宣告） | **「暂无订阅」** | 若 `subscription_fetch` 已宣告 → 「添加订阅」入口；未宣告 → 不给死入口 | 显示 0 个节点的假订阅行 |
| daemon 未握手 | `hello === undefined` | daemon 版本 / pid 显示 **「未知」** | 无 | 编造版本号；显示 pid `0` |

---

## 6. 禁语表（机器可扫）

以下字符串**不得**出现在渲染出的界面文本里（列表本身可以出现在测试代码/文档里）：

| 禁语 | 它为什么是假话 |
| --- | --- |
| `重试` / `自动重试` / `正在重试` | 契约里没有重试这个 Request，也没有 `Retry` 错误码 |
| `自动恢复` / `正在恢复连接` / `自动重连` | 契约里没有 `Degraded/Fallback`；本轮没有看门狗 |
| `已为你切换` / `已切换` / `自动选择` / `已自动` | 界面不得代替 daemon 声称已切换；只有 `node_id` 变化才算 |
| `已保护` / `安全保护` / `已接管` | 契约里没有任何「保护」字段；helper 本轮不实现（I5） |
| `兜底` / `回落` | I2 明确禁止的机制名 |
| `即将支持` / `敬请期待` | 对未实现能力的假承诺 |

**机器判据**：对整棵渲染树的 `textContent` 做子串扫描，命中即失败。
另有 **0 字节扫描**：`/(?<![\d.])0(?:\.0+)?\s*(?:B|B\/s|字节|bytes?|KB|MB|GB)\b/`
（负向后行断言避免把 `10 B`、`1.0 B` 误判）。

---

## 7. 与 `truthfulness.test.tsx` 的对应

| 本文章节 | 测试断言 |
| --- | --- |
| §1.2 阶段文案 | `stage='connecting'` → 连接按钮 disabled，且 `phase-label`/`phase-code` 出现对应逐字值 |
| §3 失败 | `last_error` 存在 → `last-error` 文本含原样 `message` 与 `code` |
| §4 未采样 | `stats == null` → 「未采样」出现，0 字节正则无命中 |
| §4 有值溯源 | 改注入的 `uplink_bytes`/`ttfb_ms` → 界面显示随之改变 |
| §6 禁语 | 整树文本扫描无禁语；扫描器自身对含禁语的合成文本能报错（证明不是恒真） |
| `IA.md` §0 规则 B | `capabilities` 不含 `probe`/`subscriptions`/`stats` → 对应入口不存在；含 `probe` → 探测入口出现 |
| `IA.md` §5 | 导航项恰为 4 项，无 TUN/helper/地理/重试入口 |
