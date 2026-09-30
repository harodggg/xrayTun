# IA · 信息架构（xraytun-next 本轮）

> 所有者：ux（task-3）。本文是**本轮纵切面**（`00-CONTRACT-FREEZE.md` §8：proxy 模式的
> 连接 / 断开 / 切节点 / 真实流量统计 + UI 真实绑定）的信息架构。
> 唯一事实源是 `crates/xt-contract/`；本文只解释「界面把哪些事实放在哪一层」。
>
> 本文件的每一条「允许出现在界面上」都对应一个契约字段或一个 Request/Event；
> 对应不上的，一律**不允许出现在导航里**（§5）。

## 0. 两条不可协商的 IA 规则

**规则 A：导航层级里不允许出现「还没有能力」的入口。**

判据分两层，缺一不可：

1. **契约层**：这个入口要回答的问题，能否指向 `xt-contract` 里的一个真实字段
   或一个 `Request`/`Event`？指向不了 → 不进导航。
2. **实现层**：本轮是否真的把它接上了？（`crates/*` 是否存在、`DaemonHello.capabilities`
   是否包含对应能力。）没接上 → 不进导航，即使契约里有类型。

这条规则是「不要做多余的事」的 UX 版本：**一个入口就是一句承诺**。
一个点进去什么都没有、或者点了什么都不发生的入口，等价于界面上的一句假话（I3）。

**规则 B（lead 追加，`00-CONTRACT-FREEZE.md` §3.1）：能力宣告 = 唯一事实源。**

`DaemonHello.capabilities` 里没有的能力，界面**不得出现任何入口**；有该能力，才允许出现。
这条规则把「是否显示」从界面自己的假设，变成 daemon 亲口说的一个字段。

三种情形，处理方式不同，不许混：

| 情形 | 证据 | 界面允许做什么 | 界面不允许做什么 |
| --- | --- | --- | --- |
| **已宣告具备** | `hello.capabilities` 含该能力 | 渲染该入口 | 把它藏起来（用户会以为没这功能） |
| **已宣告不具备** | `hello` 已到达且 `capabilities` 不含该能力 | 不渲染该入口 | 渲染一个点了会报 `unsupported` 的入口 |
| **尚未握手** | `hello === null` | 先不渲染（未知要按未知处理） | 默认「大概有」，先给入口 |

对应的能力门控（本轮 5 个能力，全部来自契约）：

| 能力 | 门控的界面入口 | 本轮是否会被宣告 |
| --- | --- | --- |
| `proxy_mode` | 连接 / 断开（仪表盘与顶栏） | 是（本轮纵切面） |
| `stats` | 上下行字节统计卡 | 是 |
| `probe` | 节点页「探测」按钮 | 是（`xt-probe` 在范围内） |
| `subscriptions` | 设置页「订阅」**只读**二级区（列表 / 空态 / 每条的真实 `node_count`、`last_error`） | 是（本地订阅文件解析，lead 认可） |
| `subscription_fetch` | 「添加订阅 / 刷新」**写入口**（`AddSubscription`/`RefreshSubscription`） | **否**——lead 明确远端拉取本轮不做；因此输入框与刷新按钮一律不渲染 |
| `tun_mode` | Dashboard 的 `run-mode-select`（proxy / tun 二选一） | 否（helper 不实现，§5）。若某天宣告了，按规则 B **允许**出现门控开关；本轮 daemon 不会宣告 |

**导航本身的处理**：四个一级导航项是「页面」，其中仪表盘/节点/日志/设置各自的主体能力
（连接、节点目录、日志、设置）里，只有 `proxy_mode` 与 `subscriptions` 有对应能力位；
`logs` 与 `settings` 契约里**没有能力位**，属于 daemon 恒有的控制面，因此不做门控。
门控落在**能力位存在的那几个入口**上（连接按钮、模式选择、探测按钮、订阅区、统计卡），
不落在「页面」这个容器上。这是我 lint 到的解释边界，已报 lead；若 lead 要求连页面也门控，
先改本文再改测试。

## 1. 页面层级

```
┌──────────────────────────────────────────────────────────────────────┐
│ 顶栏（全局常驻，不属于任何页面）                                        │
│   · 连接状态徽章   ← ConnectionView.stage                             │
│   · 当前节点       ← ConnectionView.node_id → NodeView.name           │
│   · 连接 / 断开按钮 ← stage 与 selected_node                          │
├───────────────┬──────────────────────────────────────────────────────┤
│ 侧栏导航（4 项）│ 页面内容                                              │
│  仪表盘         │                                                      │
│  节点           │                                                      │
│  日志           │                                                      │
│  设置           │  └─ 二级区：订阅管理                                  │
├───────────────┴──────────────────────────────────────────────────────┤
│ 全局传输错误条（任何页面都可能出现）← transportError(code + message)     │
└──────────────────────────────────────────────────────────────────────┘
```

**四个一级页面是全部。** 没有第五个。（与旧版 8 项导航的差异见 §6。）

## 2. 导航表：每个入口回答一个问题

| 入口 | 回答用户的哪个问题 | 首屏必须有的信息 | 可退到二级 | 数据来源（契约字段 / 请求） | 能力门控 |
| --- | --- | --- | --- | --- | --- |
| **仪表盘** | 现在通不通？走的是哪个节点？有多少真实流量？最近为什么失败？ | ① `stage` 徽章；② `connecting` 时的 `phase` 文案；③ `node_id` → 节点名；④ 连接/断开按钮及其可用性；⑤ `stats.uplink_bytes`/`downlink_bytes` 或「未采样」；⑥ `last_error` 原文 | `datapath.pid` / `version` / `ready_at_ms`；`stats.sampled_at_ms` | `Event::State{view: ConnectionView}`、`Request::Status` | `proxy_mode`（连接）；`stats`（字节卡） |
| **节点** | 有哪些节点？哪个测过、多快？我要用哪一个？ | ① 每行 `name`/`protocol`/`endpoint`/`source`；② 每行 `probe.ttfb_ms` 或 `probe.error` 或「未探测」；③ 每行「选择并使用」动作；④ 固定说明「选择就用……不会自动换下一个」 | 「探测」（用户主动发起 `Request::ProbeNodes`） | `Request::ListNodes` → `NodeView[]`、`Event::Probe{result}` | `subscriptions`（节点目录）；`probe`（探测按钮） |
| **日志** | 刚刚发生了什么？失败的确切原因是什么？ | 真实日志行的尾部 N 行（`ts_ms`/`level`/`target`/`message`） | 更早的历史行 | `Request::TailLogs{n}` → `LogLine[]`、`Event::Log{line}` | 无能力位（daemon 恒有） |
| **设置** | daemon 是谁？SOCKS 入口在哪？日志级别？订阅从哪来？ | ① `DaemonHello.daemon_version`/`pid`（无 hello 时「未知」）；② `SettingsView.socks_listen`；③ `SettingsView.log_level` | **订阅管理**（`SubscriptionView.url`/`node_count`/`fetched_at_ms`/`last_error`；`AddSubscription`/`RefreshSubscription`）；传输错误原文 | `Request::Hello`→`Response::Hello`、`GetSettings`/`PatchSettings`、`ListSubscriptions`/`AddSubscription`/`RefreshSubscription` | `subscriptions`（仅订阅区） |

### 2.1 顶栏为什么是全局的而不是仪表盘的一部分

「现在通不通」是本产品被打开时最常问的一个问题（旧仓库审计 `UX-AND-HABITS.md` §2：
T1 打开应用默认落在仪表盘、T2 换节点两步到底）。把它放在顶栏，意味着用户在**任何**页面
都能立刻看到状态并执行连接/断开，不需要先回到仪表盘。

代价：旧版实测出现过「同一屏两个『断开』按钮」（`IA-AND-OVERBUILD.md` §2.2）。
本轮**接受**这个代价，但要求两个按钮**引用同一个 `stage`**、可用性规则完全一致（见 `INTERACTION.md` §1），
不允许一个禁用另一个可点。

## 3. 首屏 vs 二级：分层判据

判断一项信息该放首屏还是二级，用两个问题（顺序固定，先问第一个）：

1. **它在「出故障时」会被需要吗？** 会 → 首屏。用户打开界面的时机大多是出问题的时候
   （`05-ui-spec.md` §1「大多数时候用户不看它」）。
2. **它是「装一次就不动」的配置吗？** 是 → 二级。首屏留给每次都要看的事实。

按这条判据的落点：

| 信息 | 层级 | 理由 |
| --- | --- | --- |
| `stage` / `phase` / `last_error` | 首屏（顶栏 + 仪表盘） | 故障时第一眼要看 |
| 上下行字节 / 「未采样」 | 首屏 | 「到底有没有流量」是 I3 的核心问题 |
| 当前节点 | 首屏 | 「我走的是哪条路」 |
| 每个节点的 TTFB / 失败原因 | 首屏（节点页） | 选择依据 |
| `datapath.pid` / `version` / `ready_at_ms` | 二级 | 排障时才需要；且全为 `Option` |
| `stats.sampled_at_ms` | 二级 | 「这个数字多新」是追问，不是第一眼 |
| `socks_listen` / `log_level` | 首屏（设置页） | 装一次，但设置页本身就是二级页面 |
| 订阅管理与 `last_error` | 设置页的二级区 | 低频；但**节点空态必须直达它**（§4） |

## 4. 空态与首跑路径

空态不是「空白」，是**当前唯一正确动作的指路牌**。逐字文案见 `INTERACTION.md` §5。

| 空态 | 触发证据 | 必须给出 | 不允许 |
| --- | --- | --- | --- |
| 无节点 | `nodes.length === 0` | 「暂无节点」+ **仅当** `subscription_fetch` 已宣告时指向**设置 → 订阅**的添加动作；未宣告时不给入口 | 显示一个可点的「连接」（无节点时连接按钮必须 disabled，并说明原因）；给一个点了会报 `unsupported` 的「添加订阅」死入口 |
| 无日志 | `logs.length === 0` | 「暂无日志」 | 用占位假行填充 |
| 无订阅 | `subscriptions.length === 0`（且 `subscriptions` 已宣告） | 「暂无订阅」+ 仅当 `subscription_fetch` 已宣告时的添加入口 | 显示 0 个节点的假订阅行 |
| daemon 未握手 | `hello === null` | 版本 / pid 显示「未知」 | 编造版本号或 pid 0 |

**首跑路径**（旧仓库 `UX-AND-HABITS.md` §2 T1 的教训：节点为空时仪表盘曾经没有
指向订阅页的入口，用户得先猜着点进节点页）：
`打开 → 仪表盘空态（暂无节点）→ 一步到设置·订阅 → 添加并拉取 → 回节点页选一个 → 连接`。
本轮导航只有 4 项，所以仪表盘空态的那个动作是**跨页直达**，不是「去侧栏找找」。
**诚实标注**：这条路径的「添加并拉取」只在 `subscription_fetch` 已宣告时才成立；
本轮不宣告它，因此**首跑路径在界面上走不通**（节点只能靠 daemon 侧的订阅文件）。
这不是可以含糊过去的取舍，是必须上报的产品后果。

## 5. 导航禁令（本轮明确不出现的入口）

| 不出现的入口 | 为什么（契约层） | 为什么（实现层） | 机器判据 |
| --- | --- | --- | --- |
| TUN 模式 / 模式切换 | `RunMode::Tun` 存在，但需要特权 helper | 本轮 helper **不实现**（§7/§8）；「TUN 已工作」的说法是假话（I5） | 由**能力门控**而不是硬编码：`capabilities` 不含 `tun_mode`（本轮真实情况）→ 页面无任何 TUN 控件/文案；含 `tun_mode` → 允许出现 `run-mode-select`（见 `truthfulness.test.tsx` 的正向对照）。导航项集合恰为 4 项 |
| 特权 helper / 授权状态 | `ErrorCode::HelperUnavailable` 存在 | 本轮不实现 | 页面无 helper/特权助手入口 |
| 自动重连 / 自动恢复 | 契约里**没有** `Retry`/`Fallback`/`Degraded`（I2） | 后端无回落路径 | 文本扫描（`INTERACTION.md` §6 禁语表） |
| 地理可视化 / 拓扑 / 地球仪 | 契约里**没有**任何地理或连接列表字段 | 本轮不做（§8） | 导航项集合恰为 4 项 |
| 审计上传 / 账号体系 | 契约里没有 | 本轮不做 | 导航项集合恰为 4 项 |
| 重试 / 自动换节点按钮 | 契约里没有「重试」这个 Request | 切节点失败即终态（I2） | 禁语扫描 + 无「重试」按钮 |
| 探测入口（当未宣告 `probe`） | `Request::ProbeNodes` 存在，但能力未宣告 | 由 daemon 决定 | `capabilities` 不含 `probe` → `probe-button` 不存在 |
| 订阅只读区（当未宣告 `subscriptions`） | `ListSubscriptions` 存在 | 由 daemon 决定 | `capabilities` 不含 `subscriptions` → `subscriptions-section` 不存在 |
| 订阅写入口（当未宣告 `subscription_fetch`） | `AddSubscription`/`RefreshSubscription` 存在，但远端拉取本轮不做 | 由 daemon 决定 | `capabilities` 不含 `subscription_fetch` → `add-subscription-button`/`refresh-subscription-button`/`subscription-url-input` 全不存在 |
| 统计卡（当未宣告 `stats`） | `StatsView` 存在 | 由 daemon 决定 | `capabilities` 不含 `stats` → 不渲染统计卡 |

**订阅为什么没有独立导航项**：本轮订阅的唯一作用是**生成节点目录**（`xt-nodes` 消费
`xt-subs`）。它是「装一次」的配置（§3 判据 2），所以放设置页二级区。
**诚实记录代价**：这样首次导入比旧版多一层（旧版「订阅」是一级导航）。
补偿措施只有一条，但必须有：**节点空态直达订阅区**（§4）。若该直达动作未实现，
这条 IA 决策就变成「用户要自己去设置里找」——那属于需要如实上报的缺陷，不是可以含糊过去的取舍。

**但还有一条更强的约束（规则 B）**：订阅能力被拆成两层——
`subscriptions`（读：解析本地文件并列出节点）与 `subscription_fetch`（写：http/https 拉取）。
本轮只宣告前者，所以正确形态是：**只读区在、写入口一个都没有**。
不给出「添加订阅」按钮（哪怕灰色/点了报 `unsupported`）——那是一个假控件。
代价（用户无法在界面里导入新订阅）是真实的产品后果，要如实写进最终报告。

## 6. 与旧版（v0.9.2）IA 的差异及理由

| 旧版 | 本轮 | 理由 |
| --- | --- | --- |
| 8 项导航：仪表盘/节点/订阅/规则/拓扑/位置/日志/设置 | 4 项：仪表盘/节点/日志/设置 | 规则、拓扑、位置在契约里没有对应字段；订阅并入设置。旧版审计已判定「问题不是页太多，而是拓扑一页装了四件事」（`IA-AND-OVERBUILD.md` §2.2） |
| 顶栏 3 模式 segmented（直连/系统代理/TUN） | 仅当宣告 `tun_mode` 时才出现 proxy/tun 选择；本轮不宣告 → 无模式切换 | 契约只有 `RunMode::{Proxy,Tun}`；本轮只做 proxy。用能力宣告门控而不是写死，改由 daemon 说 |
| 「连接成功」存在中间态被显示成已连接的行业问题 | `stage`+`phase` 双字段**逐字**呈现（`INTERACTION.md` §1） | `05-ui-spec.md` §1：把中间态显示成「已连接」会直接摧毁信任 |
| 未测延迟显示 `0 ms`/`-1 ms` 的历史风险 | 「未探测」纯文本 | `05-ui-spec.md` §5、`model.rs` 的 `ProbeResult` 恰好一个字段为 `Some` |

## 7. 本文件的可证伪性（对应 `apps/ui/tests/acceptance/truthfulness.test.tsx`）

本文不是散文，以下条目由测试钉住：

1. 导航项**恰好**是 `nav-dashboard`/`nav-nodes`/`nav-logs`/`nav-settings` 四项（§0、§5）。
2. 整页文本中不出现禁语表里的任何证据性话术（§5，`INTERACTION.md` §6）。
3. `stats == null` → 「未采样」，且不出现任何把未知说成 `0 B` 的写法（§3、`INTERACTION.md` §4）。
4. 每一个显示的字节数/延迟都能回溯到注入的契约字段值（改注入值 → 界面跟着变）。
5. 导航可达的每个页面都能在无数据时给出空态文案，而不是白屏或假数据（§4）。
6. **规则 B 的能力门控**（§0）：`capabilities` 不含 `probe`/`subscriptions`/`stats`/`tun_mode`
   → 对应入口一律不存在；含 `probe`/`subscriptions`/`tun_mode` → 对应入口出现（正向对照必须也有，
   否则无法区分「门控生效」与「功能根本没写」）。`hello === null` 时按未知处理，先不渲染任何门控入口。

## 8. 我还没验证的（诚实清单）

1. 本文的页面层级以 `ui` 给出的 DOM 契约（`data-testid`）与本轮实现为准；
   截至写下这段时 `apps/ui/src/**` 尚未落盘，**§1/§2 的层级图是待验证的设计声明**，
   不是已渲染事实。落盘后会逐条核对（测试 + 人工读 `App.tsx`）。
2. **§4 的「空态直达订阅区」尚未确认实现**。若 `ui` 未实现，我会在最终报告里
   如实列为 IA 缺口，而不会把 IA 文档改写成「本来就该让用户自己找」。
3. 顶栏「两个断开按钮引用同一 stage」由 `INTERACTION.md` §1 的测试间接覆盖；
   如果实现只渲染一个按钮，这条约束退化为无对象（不算通过，也不算失败，需人工确认）。
