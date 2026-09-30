# SCOPE · 产品范围（本轮：proxy 纵切面）

> 作者：product（角色：产品范围与验收判据）· 唯一写入范围 `docs/product/**`
> 上游事实源：`docs/architecture/00-CONTRACT-FREEZE.md`（下称「契约冻结」）
> 与 `crates/xt-contract/src/{model,protocol,error}.rs`——**两者冲突时以代码为准**。
> 旧仓库只读参考：`xray-tun/`（HEAD `985c193`，v0.9.2，工作树干净）。**本轮一行都没改、也不许改。**
> 本文件回答「本轮做什么 / 不做什么」；验收判据在 `ACCEPTANCE.md`；不做的理由与代价在 `NON-GOALS.md`。

## 0. 结论（30 秒）

1. 本轮唯一承诺：**proxy 模式下的 连接 / 断开 / 切节点 / 真实流量统计 + 界面真实绑定**（契约冻结 §8）。
2. 判定「做完」的不是页面数量，而是：**一条真实数据通路（真 xray 进程、真字节）能被无头 CLI 与界面同时驱动，且界面上每个字都能追到契约字段**（契约冻结 §7 / I3）。
3. 本轮**不做**：macOS TUN、特权 helper、审计上传、地理/拓扑可视化、自动重连/回落/重试、订阅之外的账号体系、**订阅远端拉取（http/https）**（契约冻结 §8；I2/I5；lead 2026-09-29 裁决）。
4. 延迟探测（probe）是**条件交付**：做成才在 `DaemonHello.capabilities` 宣告，没做成就不宣告、界面不出现入口（lead 2026-09-29 裁决）。
5. 旧仓库 v0.9.2 里**有**而本轮不重建的东西 ≠ 本轮有。禁止把「旧版有 / 计划过」写成「本轮已有」（见 `NON-GOALS.md` §4）。

## 1. 证据口径（先说清楚什么算证据）

| 标记 | 含义 |
| --- | --- |
| `[证据]` | 可复核：给出旧仓库路径（必要时 行号 / 命令），或本仓库 `crates/xt-contract` 的行号 |
| `[裁决]` | 用户或 lead 的明确记录（release-notes / HANDOFF / 本轮消息） |
| `[推断]` | 由证据推得，依据写在同一条里；**不是遥测数据** |
| `[假设·未验证]` | 没有证据。I3：必须这样写，不许写成事实 |

两条纪律：

* 旧仓库**没有任何用量遥测**——`IA-AND-OVERBUILD.md §7.1` 与 `0.9-USER-TEST.md` 开头都自述「没有真实用户、没有访谈、没有遥测」。因此本文所有「每天用 / 高频 / 低频」都是 `[推断]`，不是行为数据。
* 开发机不是 macOS，旧仓库的 TUN/helper 真机行为**从未验证**（`xray-tun/docs/release-notes/v0.9.2.md §4` 明写「真机 macOS 行为依然未验证」）。任何 TUN/helper 结论都不得从旧仓库迁移到本轮。

## 2. 用户任务（动词，不是功能名）

| ID | 用户要完成的动作 | 什么时候用 | 证据 | 契约锚点 |
| --- | --- | --- | --- | --- |
| **T1** | 把本机流量交给一台节点出去（连接） | 每天 | `[裁决]` v0.9.2 §1 用户原话「每一次都是我去连」⇒ 手动连接是常态；`[推断]` PRD §1 J3/J4 用户每天开 App 看状态 | `Request::Connect` / `Event::State` |
| **T2** | 立刻把流量收回本机（断开） | 每天（应急时更关键） | `[裁决]` 用户把「断开/退出」当逃生手段：「断开或退出应用后网络就恢复了」（`IA-AND-OVERBUILD.md §5.1` 引用用户原话） | `Request::Disconnect` / `Event::State` |
| **T3** | 换一台真正能用的节点（切节点） | 高频；故障时必备 | `[推断]` `0.9-USER-TEST.md` J2 标题即「高频，应急时也常用」；`[裁决]` v0.9.2 §2「选择就用，没有回落」 | `Request::SwitchNode` / `Request::ListNodes` |
| **T4** | 确认「现在真的在传数据」 | 每天 | `[裁决]` PRD §1 J4 用户原话「明明 0 B 却有车在跑」——对假视觉零容忍 | `ConnectionView.stats` / `StatsView` |
| **T5** | 出问题时知道「为什么、下一步做什么」 | 出问题时 | `[裁决]` v0.9.2 §4「连接失败就如实告诉你原因，什么时候再连由你决定」；`[推断]` `0.9-USER-TEST.md` J4 | `ConnectionView.last_error` / `ErrorBody` / `ErrorCode` / `Event::Notice` |
| **T6** | 报告问题时说清「我在哪台、哪个版本」 | 出问题时 | `[证据]` `model.rs:293-301` `DaemonHello` 注释「UI 顶栏显示的版本、pid 都取自这里，不写死」 | `Request::Hello` → `Response::Hello` |
| **T7** | 拿到节点（本地订阅文件 → 节点清单） | 一次性 + 偶尔换 | `[证据]` PRD §3.1 订阅是「唯一凭据来源」；`[裁决]` lead 2026-09-29：节点来源是本地 `--subscription-file` | `Request::ListNodes` / `NodeView.source: NodeSource::Subscription` |
| **T8** | 出问题时看核心到底说了什么（日志） | 出问题时 | `[推断]` `IA-AND-OVERBUILD.md §2.2`「看日志 / 诊断：排障时高频」；`[裁决]` v0.9.2 §4「连接失败就如实告诉你原因」；`[证据]` `LogLine.target` 注释要求「发出日志的组件，不是 UI 页签名」（`model.rs:180`） | `Request::TailLogs` / `Event::Log` / `LogLine` |
| **T9** | 改本机入口与偏好（监听地址 / 选中节点 / 日志等级） | 一次性 + 偶尔 | `[证据]` `SettingsView.socks_listen` 是 proxy 模式的入口（`model.rs:262-269`）；PRD §3.3 决策 D 把「低频配置留在设置」定为原则 | `Request::{GetSettings,PatchSettings}` / `SettingsView` / `SettingsPatch` |

**一句话判断**：T1/T2/T4 是**每天都会用**的；T3 高频、故障时最关键；T5/T6/T8 只在**出问题时**用，但它们决定「失败之后用户能不能自己走下去」——`0.9-USER-TEST.md` J4 把「失败时给出下一步动作」列为验收项；T9 是低频一次性配置，但它决定「proxy 入口在哪、连的是哪台」。

## 3. 本轮做什么（每条都能指向契约）

> 规则：下表的「契约锚点」全部来自 `crates/xt-contract/src/{model,protocol,error}.rs`。
> 实现归属见契约冻结 §9；本表只冻结**行为与字段**，不指定 crate 内部分解。

| ID | 做什么（用户可见动作） | 契约锚点 | 用户看到什么 | 不做会怎样 |
| --- | --- | --- | --- | --- |
| **D1** | 连接（proxy） | `Request::Connect{node_id, mode}`，`mode = RunMode::Proxy`（`model.rs:50-57`）；受理只回 `Response::Accepted`；终态由 `Event::State` 携带 `ConnectionView` 到达；`Connecting` 子阶段 `ConnectPhase::{PreparingConfig,StartingCore,AwaitingReady}`（`model.rs:74-94`） | 状态词随 `stage` 变；能看到「卡在哪一步」，不是一直转圈 | 「连上了没有」无从判断；用户会重复点（PRD S1 卡点） |
| **D2** | 断开 | `Request::Disconnect` → `Accepted` → `Event::State`（`stage: Disconnecting → Disconnected`，`datapath.pid → None`） | 断开后界面立刻不再声称已连接 | 用户失去逃生手段（T2 的证据正来自这条） |
| **D3** | 切节点 | `Request::SwitchNode{node_id}`；节点清单 `Request::ListNodes` → `Response::Nodes(Vec<NodeView>)`，`NodeView{id,name,protocol,endpoint,source}`（`model.rs:195-205`） | 能从真实节点清单里选，切换过程有终态 | 无法换掉不可用的节点 |
| **D4** | 真实流量统计 | `ConnectionView.stats: Option<StatsView>`；`StatsView{uplink_bytes,downlink_bytes,sampled_at_ms}`（`model.rs:110-118`、`120-154`）；**`None` = 从未采样成功** | 采样成功显示真实字节；未采样显示「未采样」（**不是 0**） | 重演「0 B 却有车在跑」类的不实陈述（PRD J4） |
| **D5** | 真实界面（绑定，不是新功能） | `Request::{Hello,Subscribe,Status}`；`Response::{Hello(DaemonHello),Subscribed,Status}`；`Event::{State,Log,Probe,Notice}`；`Topic`（`model.rs:313-323`）；`Frame{kind,id,seq}`（`protocol.rs:106-127`） | 每个显示项可溯源；**没有 mock/preview 作为运行时数据源**（契约冻结 §7） | 界面成为「另一份真相」，比没有界面更危险 |
| **D6** | 无头等价入口（`xt-cli`） | 与 UI 同一份契约（`Request`/`Response`/`Event`/`Outcome`） | 「UI 能做的，无头也能做」（契约冻结 §1 理由 3）；也是本轮 E2E 入口（§8 用 `cargo run -p xt-cli`） | 业务逻辑只能靠点界面验证 ⇒ 无法在 CI 里跑真实验证 |
| **D7** | 失败的诚实归因 | `ErrorCode` 封闭枚举（`error.rs:10-33`）、`ErrorBody{code,message,detail}`（`error.rs:61-67`）、`Event::Notice` + `NoticeSeverity` | 失败是「如实上报的终态」，不是悄悄换一条路（I2） | 用户不知道刚才发生了什么（v0.9.2 §1 三层理由） |
| **D8** | 本地订阅文件 → 节点清单（**只读**） | `xt-subs` 解析订阅原文；来源 `--subscription-file`（默认 `$state_dir/subscription.txt`）（`[裁决]` lead 2026-09-29）；产出 `NodeView.source: NodeSource::Subscription{id}`（`model.rs:185-205`）；订阅视图 `SubscriptionView{id,url,node_count,fetched_at_ms,last_error}`（`model.rs:250-260`） | 用户能看到自己那份订阅里的节点与订阅状态；**没有「添加订阅 / 刷新」入口**（远端拉取本轮不做，见 §4 与 §5.1 N1） | 切节点无从谈起（没有节点来源） |
| **D9** | 能力自我宣告（含订阅口径拆分） | `DaemonHello.capabilities: Vec<Capability>`（`model.rs:293-311`）；`Capability::{Subscriptions,SubscriptionFetch}` **已落地**（`model.rs:317`，粒度理由写在变体注释里）；硬规则见契约冻结 **§3.1**（「`capabilities` 是唯一的能力事实来源」；未宣告能力的请求返回 `ErrorCode::Unsupported`，且该码**只能**表示"本版本不提供"，不得包装真实失败） | 界面只渲染**已宣告**能力的入口（未实现 = 入口不存在，不是灰按钮）。订阅区块因此**只读**：`id` / `url` / `node_count` / `fetched_at_ms` / `last_error`；**没有「添加 / 刷新」入口** | 出现「假控件」：能点但什么都没发生（旧仓库反复修过这一类，见 `NON-GOALS.md` §2） |
| **D10** | 日志绑定真实输出 | `Request::TailLogs{lines}` → `Response::Logs(Vec<LogLine>)`；`Event::Log{line}`；`LogLine{ts_ms,level,target,message}`（`model.rs:176-183`）、`LogLevel`（`model.rs:156-174`） | 排障时能看到**核心自己**说的话（真 xray 的 stdout/stderr 行），不是我们编的摘要 | 失败时用户只能看到一句结论，无法自救（T5 的证据正是 v0.9.2 §4 的承诺） |
| **D11** | 设置读写真生效 | `Request::GetSettings` → `Response::Settings(SettingsView{socks_listen,selected_node,log_level})`；`Request::PatchSettings{patch}` → `Response::Ok`；`SettingsPatch` 语义是「`None` = 不改这一项」（`model.rs:262-291`） | 改了监听地址/选中节点/日志等级后，**行为真的跟着变**（或给出明确的「需要重连才生效」说明）——不许「界面显示新值、实际没变」 | proxy 入口不可配置 ⇒ 端口冲突时用户无路可走；「改完没生效」正是旧仓库 v0.9.0 专门修过的一类问题（`release-notes/v0.9.0.md §1`） |

### 3.1 界面「必须可见的事实集合」（不发明页面）

**页面数量 / 导航结构 / 文案 / 视觉不在本文件冻结范围**（归 ux/design）。本文件只冻结：哪些事实必须可见、且必须来自哪个字段——这是 `ACCEPTANCE.md` 能被 ux 直接翻译成测试的前提。

| 必须可见的事实 | 契约字段 | 出现时机 |
| --- | --- | --- |
| 当前阶段 / 子阶段 | `ConnectionView.stage` / `phase` | 任何时刻 |
| 正在连的是哪台 | `ConnectionView.node_id` | `Connecting` / `Connected` |
| 从什么时候连上的 | `ConnectionView.connected_since_ms` | `Connected` |
| 真正在跑的进程 | `ConnectionView.datapath.pid` / `version` / `ready_at_ms` | `Connected`（`None` 就不显示，不显示 0） |
| 真实字节 | `ConnectionView.stats.uplink_bytes` / `downlink_bytes` / `sampled_at_ms` | 采样成功时 |
| 「未采样」 | `ConnectionView.stats == None` | 采样失败时（**禁止渲染 0**） |
| 最近一次真实失败 | `ConnectionView.last_error{code,message}` | 失败之后 |
| daemon 身份 | `DaemonHello.daemon_version` / `pid` / `protocol_version` | 连接建立时 |
| 能力清单 | `DaemonHello.capabilities` | 连接建立时（决定哪些入口存在） |
| 节点清单 | `Response::Nodes(Vec<NodeView>)` | 用户要看节点时 |
| 订阅状态（只读） | `SubscriptionView{id,url,node_count,fetched_at_ms,last_error}` | 用户要看订阅时（**无添加/刷新入口**） |
| 日志行 | `LogLine{ts_ms,level,target,message}`、`Event::Log` | 排障时（`TailLogs{lines}` 取最近 N 条） |
| 设置值 | `SettingsView{socks_listen,selected_node,log_level}` | 用户要看/改设置时 |

## 4. 本轮明确不做（速览；完整理由 + 用户代价在 `NON-GOALS.md`）

| 不做 | 一句话理由 | 用户会看到 / 失去什么 | 什么时候才该做 |
| --- | --- | --- | --- |
| **macOS TUN** | 需要特权 helper；开发机非 macOS ⇒ **无法验证**（契约冻结 I5「任何『TUN 已工作』的说法都是假话」） | 失去「全机所有 App 自动走代理」；只能按 App 配 SOCKS | 有 macOS 真机 + helper + 可复现验收之后 |
| **特权 helper** | 改系统网络状态、需要快照/回滚（旧仓库最贵的一层复杂度，`IA-AND-OVERBUILD.md §4.2`）；且有开放现场问题（`OPEN-FINDINGS.md` F-1/F-4） | 失去崩溃后自动还原系统网络的安全网；同时也没有「改坏系统网络」的风险 | 与 TUN 同批，且必须带真实回滚验证 |
| **审计上传** | 契约冻结 §8 明确不做；启动加密客户端 + 服务端 + token 会扩大验证面 | 失去「自动把问题报告送到开发者」；出问题只能手动贴日志 | 用户明确要求支持通道且隐私模型重新验证之后 |
| **地理 / 拓扑可视化** | 契约冻结 §8；其数据源（访问日志、规则链、位置查询）在 proxy 纵切面里还不存在 | 失去流向图与地球仪；**诚实地说：用户确实用过它**（PRD §3.3 决策 C 记录他要求过放大倍数） | proxy 主通路真机验证通过、且日志/规则数据源存在之后 |
| **自动重连 / 回落 / 重试** | I2 + `[裁决]` v0.9.2 §1 用户原话「自动重连和看门狗本来就没什么用…每一次都是我去连」；`ErrorCode` 里连成员都没有（`error.rs:8-9`） | 换网/唤醒后**必须手动重连**；失败时不会自动换到别的节点 | 永不作为默认；若要做必须先在 `ErrorCode` 加成员并过评审 |
| **订阅之外的账号体系** | 契约里**没有任何**账号/用户类型（`model.rs` 全文）；无证据表明用户需要跨设备同步 | 失去跨设备同步；每台机器重新配订阅与设置 | 用户有多设备需求并明确要求时 |
| **订阅远端拉取（http/https）** | `[裁决]` lead 2026-09-29：不在纵切面关键路径上，引 TLS 客户端会扩大依赖与验证面；`xt-subs` 只实现「解析订阅原文」 | **失去「贴一条订阅 URL 就自动更新」**；必须手动把订阅文本落到 `$state_dir/subscription.txt`。订阅区块只读，没有「添加 / 刷新」入口 | 主通路稳定、且愿意承担 TLS 客户端依赖面之后 |

## 5. 边界与裁定记录（已裁定 ≠ 已实现；未决 ≠ 已做）

处理规则：本节的「已裁定」只锁**范围口径**；是否**已实现**看 `ACCEPTANCE.md` 的状态列，两者不许混。lead 裁定后由 product 回改本文件。

### 5.1 已裁定（lead 2026-09-29，team-message-5f45ef69 / -9eb398cd）

| ID | 事项 | 裁定 | 对契约 / 界面的要求 |
| --- | --- | --- | --- |
| **N1** | 订阅远端拉取的契约面 | `AddSubscription` / `RefreshSubscription` 属**拉取语义** ⇒ daemon 返回 `Unsupported`（`ErrorCode::Unsupported` 已落地 `error.rs:33/52/105-106`）；`Capability::SubscriptionFetch`（`model.rs:317`）本轮**不宣告** | 界面订阅区块**只读**：`id` / `url` / `node_count` / `fetched_at_ms` / `last_error`（`model.rs:250-260`），没有添加/刷新入口 |
| **N2** | 延迟探测（probe） | **条件交付**（维持）：做成才宣告 `Capability::Probe`；本轮能力现状以契约冻结 §3.1 为准（proxy+stats、Subscriptions 宣告；probe 成功才宣告；SubscriptionFetch、TunMode 不宣告） | 未宣告 = 界面无入口（不是灰按钮）；`ACCEPTANCE.md` COND-PROBE-01/-02 |
| **N3** | 日志（`TailLogs` / `Event::Log`） | **本轮做**（契约冻结 §8 未列已由本次裁定覆盖） | 界面绑定真数据：日志行必须来自核心真实输出；`LogLine.target` 是发出日志的组件，不是 UI 页签名（`model.rs:180`） |
| **N4** | 设置（`GetSettings` / `PatchSettings`） | **本轮做** | 改动必须**真的生效**（或给出明确的「需要重连才生效」说明）；不许「界面显示新值、实际没变」 |
| **N6** | `Capability::Subscriptions` 的宣告口径 | `Capability` 拆分**已落地**（`model.rs:317`）：`Subscriptions`（本地订阅原文解析，**本轮宣告**）＋ `SubscriptionFetch`（http/https 拉取，**本轮不宣告**） | 界面入口一律由 `capabilities` 决定；未宣告的能力**不存在**而不是被禁用 |

### 5.2 仍是归属问题，不是未决

* **N5 页面 / 导航 / 文案 / 视觉**：归 ux/design（契约冻结 §9）；本文件不指定，但 §3.1 的「必须可见事实集合」是硬约束。

### 5.3 契约变更记录（**已落地，不再是阻塞**）

lead 的两条裁定需要改**冻结的 `xt-contract`**（独占写入路径，契约冻结 §9）。**两条都已落地并已复核**（product 2026-09-29 在本次回扫中重新读取了当前文件）：

| # | 变更 | 落地位置（已复核） | 影响的判据 |
| --- | --- | --- | --- |
| P1 | 新增 `Capability::SubscriptionFetch`（与 `Subscriptions` 并列；本轮**不宣告**） | `model.rs:317`（粒度理由写在变体注释里：粗粒度会逼界面显示按不动的「刷新」假控件） | CAP-01、SUB-02 |
| P2 | 新增 `ErrorCode::Unsupported` + `as_str` + 便捷构造 | 变体 `error.rs:33`；`as_str` `error.rs:52`（→ `"unsupported"`）；构造 `unsupported(msg)` `error.rs:105-106`。现共 **11** 个成员 | SUB-02、CAP-03 |
| P3 | 能力宣告硬规则成文 | 契约冻结 **§3.1**：`capabilities` 是唯一事实来源；未宣告能力的请求返回 `Unsupported`；该码**只能**表示「本版本不提供」，**不得**包装「试了但失败」（后者必须落到具体失败码，否则它就是新的兜底） | CAP-01、CAP-03、ERR-03 |

评审记录：加成员已过评审（评审人为 lead），裁定成文于契约冻结 §3.1。⇒ `SUB-02` / `CAP-01` 的「前置依赖」解除，按正常「待验证」流程推进即可；product 未改契约（也不许改）。

## 6. 与旧仓库的一处结论冲突（必须写下来，防止照旧文档重建已删机制）

旧 PRD 的第一优先级是「让自愈可见」（J1「开机后网络应该自动连上，不需要点击连接」、J2「换网/唤醒后要能自己恢复」＋看门狗＋恢复态）。v0.9.2 已按用户裁决把这三样**整体删除**：`xray-tun/docs/release-notes/v0.9.2.md §1`、`HANDOFF.md §1.3`（删掉约 55 条测试）。

⇒ 本轮 product 的立场：**以 v0.9.2 的裁决为准，不以旧 PRD §4 P0-1 为准。**
旧 PRD 的 J1/J2 证据仍有价值——它证明了「用户非常在意状态是否真实」；但它的**结论（做自动恢复）已被推翻**。若有人按旧 PRD 重建自愈/看门狗，视为越界（I2 + 契约冻结 §6）。

## 7. 假设与未验证（诚实清单）

1. **频率分类是推断**：旧仓库没有遥测（`IA-AND-OVERBUILD.md §7.1`、`0.9-USER-TEST.md` 开头）。§2 的「每天 / 高频 / 出问题时」逐条标了 `[推断]` 或 `[裁决]`，不是行为数据。
2. **单节点用户**：PRD §0.3 写的「他的真实情况是单节点」来自任务背景、不来自仓库 ⇒ 切节点在真实使用中的频率 `[假设·未验证]`。
3. **用户愿意手动重连**：只有 v0.9.2 的一句裁决记录支撑；没有使用数据。
4. **旧仓库 macOS 真机行为从未验证**（v0.9.1/v0.9.2 都写明「开发机不是 macOS」）⇒ 本轮不能引用旧仓库的 TUN/helper 结论。
5. **本轮的用户任务证据全部来自旧仓库文档 + 用户历次裁决，没有新的用户访谈**。
6. `[假设·未验证]` 一个具体例子：本轮没有验证「用户会因为看不到流量统计而误判网络问题」；T4 的依据是 PRD J4 的原话，属于一次具体抱怨，不是统计。

## 8. 本轮完成定义（DoD）

1. `bash xraytun-next/scripts/guard.sh` → `GUARD PASSED`。
   * 最近一次运行（2026-09-29，product 回扫时重跑）：`crates=26`、`ui=16`、`违规=0 警告=0`、`GUARD PASSED`；上一轮那条 `.unwrap()` 警告已由 lead 修掉（检查现在会跳过 `crates/*/tests/*` 与文件末尾 `#[cfg(test)]` 之后的行，并打印具体位置）。
   * **通过条件是 `GUARD PASSED` + `违规=0`**；crate/ui 计数只是当次快照，会随并行开发变动（本次回扫期间就从 25→26）。引用前必须重跑，并把当次输出写进 `ACCEPTANCE.md` A-01（**不许拿旧一次的 PASSED 当本次证据**）。
2. `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo test --workspace` 全绿。
3. E2E：真 xray（`/Users/xbtg-/deepseek-harness/.scratch/bin/xray`，Xray 26.3.27 linux/amd64）环回跑通——连接 → 真实 SOCKS 请求 → 真实字节 → 切节点 → 再请求 → 断开（契约冻结 §8）。被测路径上**不许有 mock**。
4. `ACCEPTANCE.md` 里状态为「待验证」的条目全部转为「已验证（附命令 + 输出摘要）」或「本轮不验证（附理由）」。
5. §5.3 的两条契约变更（`Capability::SubscriptionFetch`、`ErrorCode::Unsupported`）**已落地并复核**（`model.rs:317`；`error.rs:33/52/105-106`；硬规则成文于契约冻结 §3.1）。
6. 没有一处已裁定/未决项被写成「已实现」；没有任何「旧版有」被写成「本轮有」；没有一条「已验证」是引自旧仓库的结论。
