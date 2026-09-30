# 真实信息契约 —— 界面每一处可见信息 → 契约字段

> 这是 I3（没有假话）在前端的落地判据。ux 的自动化测试**直接对着这张表写**：
> 左列是界面上能被读到的信息，右列是它唯一合法的来源字段。
> 表格里的字段路径一律是 **wire 上的 serde 字段名**（`crates/xt-contract/src/model.rs` /
> `protocol.rs`），与 TS 侧同名，可直接用于断言。
>
> 判据：**如果一处可见信息在本文里找不到对应字段，那它就不允许出现在界面上。**
> 需要新增显示项 → 先改契约（找 lead），再回来加这一行。

## 0. 读法与不变量

**路径记号**：`ConnectionView.stats.uplink_bytes` 表示
`{"event":"state","view":{"stats":{"uplink_bytes":…}}}`；
`NodeView.probe.ttfb_ms` 表示节点对象里的 `probe.ttfb_ms`（`probe` 本身可为 `null`）。

**数据到达方式（决定「显示哪个」）**：

| 数据 | 请求响应（初始） | 事件（增量/权威） |
| --- | --- | --- |
| `ConnectionView` | `Response(Status)` → 直接是 `ConnectionView` | `Event{event:"state"}` → `view` |
| `NodeView[]` | `Response(Nodes)` | 无（本轮没有节点事件） |
| `LogLine[]` | `Response(Logs)` | `Event{event:"log"}` → `line` |
| `ProbeResult` | 无 | `Event{event:"probe"}` → `result` |
| `Notice` | 无 | `Event{event:"notice"}` → `notice` |
| `DaemonHello` | `Response(Hello)` | 无 |
| `SettingsView` | `Response(Settings)` | 无（`PatchSettings` 后重读响应） |
| `SubscriptionView[]` | `Response(Subscriptions)` | 无 |

四条硬规则：

1. **`Event{event:"state"}.view` 是连接状态的唯一权威**。`Response(Status)` 只是首次快照；
   之后每个事件按 `Frame.Event.seq` 覆盖。UI 不得从日志/通知文本反推状态。
2. **`null` 一律显示「未知」系**（`—` + 词），不显示 `0`、不显示空字符串、不复用上一帧的值。
3. **`Response(Accepted)` 只表示「意图已受理」**，它**不是**结果。界面不得在收到
   `Accepted` 后就把状态改成「已连接」或在按钮上显示完成。
4. **派生值必须有本文件 §4 的公式与前提**；没有公式的数字（评分、百分比、节省量）一律禁止。

---

## 1. Dashboard

| 界面位置 | 可见信息 | 契约字段路径 | 可空 | `null` / 缺失时 | 禁止 |
| --- | --- | --- | --- | --- | --- |
| 状态徽章 | 快照尚未到达时的徽章 | **无字段**（`Response(Status)` / `Event{state}` 都还没到） | — | `.badge` / `.badge--unknown` + 「未知」+ 虚线 | 渲染成 `stage=="disconnected"`（把「不知道」说成「确定没连」） |
| 状态徽章 | 徽章 class（`badge--*`）与色 | `ConnectionView.stage` | 否 | — | 用 `phase` 或 `last_error` 改徽章色 |
| 状态徽章 | 文字「未连接/连接中/已连接/断开中」 | `ConnectionView.stage` | 否 | — | 「已保护/安全/正常」；`connecting` 写「已连接」 |
| 状态徽章 | 子阶段条（class + 文字） | `ConnectionView.phase` | 是 | 不渲染 `.badge__phase` | `null` 时猜一个阶段；`stage != connecting` 时渲染 |
| 运行模式 | 「SOCKS 代理 / TUN / …」 | `ConnectionView.mode` | 是 | 「未知」 | 把 `null` 写成「直连」或「系统代理」 |
| 当前节点名 | 节点标题 | `NodeView.name`，其中 `NodeView.id == ConnectionView.node_id` | 是（`node_id` 可空） | 「未选择节点」 | 用列表第一项顶替；用订阅名顶替 |
| 当前节点 | 端点 | `NodeView.endpoint` | 否（节点存在即非空） | — | 显示解析失败的上游地址（契约保证这类节点不入列表） |
| 当前节点 | 协议 | `NodeView.protocol` | 否 | — | 写死协议名 |
| 已连接时长 | 「已连接 12m」 | `ConnectionView.connected_since_ms` + 本地真实时钟 | 是 | 「未连接」；`stage=="connected"` 而该值为 `null` → 显示「未知」并按 `internal` 记日志 | 显示 `0s`；用计数器代替时间戳 |
| 数据面 | 核心 pid | `ConnectionView.datapath.pid` | 是 | 「—」+「未知」 | 显示 `0`；用 daemon 的 pid 顶替 |
| 数据面 | 核心版本 | `ConnectionView.datapath.version` | 是 | 「未知」 | 写死版本号 |
| 数据面 | 就绪时刻 | `ConnectionView.datapath.ready_at_ms` | 是 | 「未就绪」 | 显示 `0`（epoch 0） |
| 数据面 | 启动耗时 | **派生**：`ready_at_ms − t0`，`t0` = UI 在发出 `Request(Connect)` 那一刻记下的本地时钟 | — | 缺 `ready_at_ms` 或没记到 `t0` → **不显示耗时**，只显示就绪时刻 | 用固定数字；用「约 1s」；用「准备中 3s」这类无字段进度 |
| 流量 | 上行 | `ConnectionView.stats.uplink_bytes` | `stats` 可空 | 整个统计区 `.stats--unsampled` + 「未采样」 | 显示 `0 B` |
| 流量 | 下行 | `ConnectionView.stats.downlink_bytes` | `stats` 可空 | 同上 | 显示 `0 B` |
| 流量 | 合计 | **派生**：`uplink_bytes + downlink_bytes`（同一个 `StatsView`） | — | `stats == null` 时与统计区一起显示「未采样」 | 单独算一半；写「已节省」 |
| 流量 | 采样时刻 | `ConnectionView.stats.sampled_at_ms` | `stats` 可空 | 「未采样」 | 用本地 `Date.now()` 冒充采样时刻 |
| 流量 | 数据陈旧提示 | **派生**：`now − sampled_at_ms > 阈值`（阈值为 UI 常量） | — | — | 把陈旧数据当实时；把陈旧画成错误色之外的成功 |
| 流量 | 速率 | **派生**：`Δbytes / Δsampled_at_ms`，需要**两个** `StatsView` 样本且 `sampled_at_ms` 不同 | — | 只有一个样本 → 不显示 | 单样本显示 `0 B/s`；用本地时钟算速率 |
| 失败 | 错误盒 | `ConnectionView.last_error.code` / `.message` / `.detail` | 是 | `null` → 不渲染 `.error-box` | 因 `stage=="connected"` 就隐藏历史 `last_error`；改写 `message` |
| 操作 | 按钮可用/禁用 | **派生**：`ConnectionView.stage`（`disconnected` 才可 connect；`connected`/`connecting` 才能 disconnect） | 否 | — | 用禁用态表达错误色；`Accepted` 后立即改状态 |
| daemon | 连接指示（UI↔daemon） | **无契约字段**（本地传输状态） | — | — | 用绿色/`badge--connected`；文字不得写「已保护」。合法措辞只有「daemon 未连接/已连接」并**标明是本地传输** |

---

## 2. Nodes / 订阅

| 界面位置 | 可见信息 | 契约字段路径 | 可空 | `null` / 缺失时 | 禁止 |
| --- | --- | --- | --- | --- | --- |
| 列表 | 行集合 | `Response(Nodes)` → `NodeView[]` | 否（空数组合法） | 空数组 → `.page__empty`「还没有节点」 | 用 fixture/示例节点填充 |
| 行 | 名称 | `NodeView.name` | 否 | — | 缺名时显示 endpoint 当名字（那是另一字段） |
| 行 | 协议 | `NodeView.protocol` | 否 | — | 写死映射表 |
| 行 | 端点 | `NodeView.endpoint` | 否 | — | 拼接/美化上游地址 |
| 行 | 来源 | `NodeView.source.kind`（`"subscription"` / `"manual"`） | 否 | — | 「导入」「推荐」等非契约措辞 |
| 行 | 来源订阅 id | `NodeView.source.id`（仅 `kind=="subscription"`） | 否 | — | 显示订阅 URL（含 token） |
| 行 | 选中标记 | `SettingsView.selected_node == NodeView.id` → `.nodes__row--selected` | `selected_node` 可空 | 无选中行 | 把「选中」画成绿色（那是 `--state-connected` 的语义） |
| 行 | 当前标记 | `ConnectionView.node_id == NodeView.id` **且** `ConnectionView.stage=="connected"` → `.nodes__row--current` | 是 | 无当前行 | 用 `selected_node` 当当前；`stage!="connected"` 时给当前标记 |
| 行 | 探测结果 | `NodeView.probe.ttfb_ms` | `probe` 可空 | `probe == null` → `.probe--none`「未探测」 | 显示 `0 ms`；显示 `--` 却带「快」评价 |
| 行 | 探测失败 | `NodeView.probe.error.code` / `.message` | 是（与 `ttfb_ms` 恰好一个非空） | — | 失败行显示 `0 ms`；两个都空时当成功 |
| 行 | 探测时间 | `NodeView.probe.at_ms` | 是 | — | 用本地时钟 |
| 探测按钮 | 「已请求探测」 | `Response(Accepted)`（只证明请求被受理） | — | — | 给单个节点画「探测中」进度/转圈（**没有**契约字段证明它在探测）；改成整区提示「已请求，等待结果」 |
| 订阅行 | 订阅 id | `SubscriptionView.id` | 否 | — | — |
| 订阅行 | URL | `SubscriptionView.url` | 否 | — | **明文显示含 token 的完整 URL**；必须脱敏（只显示 scheme+host，或掩码 path） |
| 订阅行 | 节点数 | `SubscriptionView.node_count` | 否 | — | 用列表长度顶替 `node_count` |
| 订阅行 | 上次刷新 | `SubscriptionView.fetched_at_ms` | 是 | 「未刷新」 | 显示 `0` / epoch |
| 订阅行 | 失败 | `SubscriptionView.last_error.code` / `.message` | 是 | 不渲染 | 因有节点就隐藏刷新失败 |

---

## 3. Logs

| 界面位置 | 可见信息 | 契约字段路径 | 可空 | `null` / 缺失时 | 禁止 |
| --- | --- | --- | --- | --- | --- |
| 列表 | 初始日志 | `Response(Logs)` → `LogLine[]` | 否 | 空数组 → 「暂无日志」 | 写「一切正常」「无异常」 |
| 列表 | 增量日志 | `Event{event:"log"}.line` | 否 | — | 本地造日志补位 |
| 行 | 时间 | `LogLine.ts_ms` | 否 | — | 用本地接收时刻 |
| 行 | 等级 | `LogLine.level`（`"error"｜"warn"｜"info"｜"debug"`） | 否 | — | 只有颜色没有文字；把 info 画成绿 |
| 行 | 来源组件 | `LogLine.target` | 否 | — | 用它当页签名（它可能是空字符串，空则显示「—」而不是猜） |
| 行 | 正文 | `LogLine.message` | 否 | — | 改写/截断成另一句话（截断必须可展开看原文） |
| 过滤 | 当前过滤条件 | **本地状态**，映射自 `LogLevel`；不是契约字段 | — | — | 过滤后为空时写「无异常」；改为「当前过滤条件下没有日志」 |
| 丢帧 | 序号跳号提示 | `Frame.Event.seq`（客户端计算跳号） | 否 | — | 伪造一条 `Notice`（含 `Notice.code`）来提示丢帧；必须标为「本地检测」 |
| 顶部 | 行数/请求行数 | `Request(TailLogs).lines`（只证明请求了什么） | — | — | 写「共 N 行」（数组长度 ≠ 历史总数） |

---

## 4. Settings

| 界面位置 | 可见信息 | 契约字段路径 | 可空 | `null` / 缺失时 | 禁止 |
| --- | --- | --- | --- | --- | --- |
| 表单 | SOCKS 监听地址（已保存值） | `SettingsView.socks_listen` | 否 | — | 把输入框里未提交的草稿当作已保存值 |
| 表单 | 选中节点 | `SettingsView.selected_node` | 是 | 「未选择」 | 显示 `0` / 默认选中第一项 |
| 表单 | 日志级别 | `SettingsView.log_level` | 否 | — | 本地默认值顶替 |
| 保存 | 保存结果 | `PatchSettings` 的响应：`Response(Settings)`（新 `SettingsView`）或 `Outcome::Error.error` | — | — | 乐观更新（请求没回来就先改界面）；改写 `error.message` |
| daemon | 版本 | `DaemonHello.daemon_version` | 否 | — | 写死版本号 |
| daemon | 协议版本 | `DaemonHello.protocol_version` | 否 | — | 隐藏（不匹配时协议本身会拒绝连接） |
| daemon | 能力 | `DaemonHello.capabilities[]`（`proxy_mode`/`tun_mode`/`stats`/`probe`/`subscriptions`） | 否 | — | 能力缺失时仍展示对应功能；必须禁用并写明「daemon 未声明 X 能力」 |
| daemon | 进程 pid | `DaemonHello.pid` | 否 | — | 与 `datapath.pid` 混用（一个是控制面，一个是数据面） |
| daemon | 启动时刻 | `DaemonHello.started_at_ms` | 否 | — | 显示运行时长之外的时间却不知道起点来源 |
| 契约外 | 订阅 URL token | `SubscriptionView.url`（脱敏后显示） | 否 | — | 明文 token；复制整串含 token 的 URL |

---

## 5. 通知（全局）

| 可见信息 | 契约字段路径 | 可空 | `null` 时 | 禁止 |
| --- | --- | --- | --- | --- |
| 通知条 severity | `Notice.severity`（`"info"｜"warning"｜"error"`） | 否 | — | info 画成绿/「成功」 |
| 通知 code | `Notice.code`（`ErrorCode`） | 否 | — | 本地改写成别的 code |
| 通知原文 | `Notice.message` | 否 | — | 替换为模板话术（如「请稍后重试」） |
| 通知时刻 | `Notice.at_ms` | 否 | — | 用接收时刻 |
| 通知列表为空 | **无字段** | — | — | 显示「无异常」「一切正常」（空列表只等于「没有收到通知」） |

---

## 6. `ErrorCode` 的展示规则

`ErrorCode` 是封闭枚举，界面上必须**原样**出现（可作为 `banner__code` / `error-box__code` /
`probe--fail` / `log-line--target` 旁的小字），不得翻译成会承诺行为的词：

| `ErrorCode` | 中文说明（允许） | 禁止追加 |
| --- | --- | --- |
| `invalid_request` | 请求不合法 | 「请重试」 |
| `not_found` | 对象不存在 | — |
| `conflict` | 当前状态不允许该操作 | — |
| `permission_denied` | 没有权限 | 「请用管理员运行」（helper 本轮未实现） |
| `datapath_unavailable` | 数据面不可用 | 「正在重新拉起」 |
| `config_invalid` | 配置被 xray 判为非法 | 「已自动修正」 |
| `core_exited_early` | 核心在就绪前退出 | 「已自动重连」 |
| `helper_unavailable` | 特权 helper 不可用 | — |
| `io` | 系统 IO 失败 | 「换个节点就好」 |
| `internal` | 内部不变量被破坏 | 任何「已恢复」 |

**理由**：契约里没有 `Retry` / `Fallback` / `Degraded`（`error.rs` 注释即证据）。
任何暗示「系统自己换了一条路成功」的文案，都是在替一个不存在的机制说话。

---

## 7. 禁止出现的视觉（逐条说明为什么它是在说谎）

1. **没有证据的绿勾 / 「已保护」/「安全」/「正常」**
   绿色在本系统唯一绑定 `Stage::Connected`，而 `Connected` 的契约含义只有
   「数据面已被证明可连（proxy：SOCKS 端口接受连接）」（冻结页 §4）。它**不**证明这台机器
   的系统流量走了代理；proxy 模式下没配 SOCKS 的应用依然直连。写「已保护」就是把
   「端点可连」偷换成「你被保护了」。本轮 TUN/helper 未实现（冻结页 §8），
   任何「系统级接管」的说法都是纯粹的假话。

2. **把未知画成 0**
   `ConnectionView.stats` 为 `null` 表示**从未采样成功**（`model.rs` 注释）。显示 `0 B`
   会让用户以为「没流量」，实际是「不知道」。两者后果相反：前者安心，后者可能意味着
   数据面根本没计数。

3. **把 `connecting` 画成 `connected`**
   `Stage` 是唯一判据；`ConnectPhase` 只描述「卡在哪一步」。把 blue/进行中渲染成绿色或
   打勾，等于在 SOCKS 还没接受连接时宣告成功（`stage == connected ⟺ 已证明可连`）。

4. **没有契约字段的装饰性数字**
   例如「已拦截 128 条广告」「节省流量 2.1 MB」「连接质量 98 分」「节点评分 ★4.8」
   「覆盖 32 个国家」。`xt-contract` 里没有对应字段，也没有任何采样来源
   （冻结页 §7：统计只有两条来源）。这些数字**只能**是编的。

5. **把 daemon IPC 连通画成绿色 / 「已受保护」**
   UI↔daemon 的 socket 是本地传输状态，不是隧道状态。daemon 活着而隧道未连是常态；
   用绿色表示 daemon 在线会把「进程在」误读成「流量被代理」。

6. **「已自动恢复 / 正在重连 / 已切换备用节点 / 已降级到直连」**
   契约里没有这些状态：`ErrorCode` 无 `Retry/Fallback/Degraded`，`Stage` 无 `Recovering`
   （冻结页 §6：失败就是如实上报的终态）。这些话术描述的是不存在的机制。

7. **延迟 `0 ms` 或失败显示 `0 ms`**
   `ProbeResult` 保证 `ttfb_ms` 与 `error` **恰好一个**非空（`is_consistent()` 即断言）。
   `0` 是一个合法测量值，会被读成「零延迟」；失败行显示 `0` 则直接掩盖失败。

8. **因为现在 `connected` 就隐藏历史 `last_error`**
   `ConnectionView.last_error` 的注释写明它「不会因为后来起来了而被替换成描述成功的话术」。
   隐藏它 = 抹掉真实发生过的失败。反向也不行：因为 `last_error` 有值就把当前
   `stage==connected` 的徽章画成红色，等于谎报「现在断了」。

9. **把 `mode == null` 写成「直连」**
   `null` 是「不知道模式」。直连与未知的处置完全不同（未知要查，直连是事实）。

10. **假进度**：「正在连接… 47%」「预计 2 秒」「准备中 (3/5)」
    没有任何字段给出分母或总数；`ConnectPhase` 只有 4 个离散值。百分比只能来自编造。

11. **空列表说「无异常」/「一切正常」**
    空的 `Response(Logs)` 只证明「没有日志行」，不证明「没有异常」；没有通知只证明
    「没有收到通知」。把「没看见」说成「不存在」是经典假话。

12. **写死的版本号 / pid / 端口**
    `daemon_version`、`pid`、`datapath.version`、`socks_listen` 都是真实观测值
    （冻结页 §7 末条）。写死会在升级后立刻变成假信息。

13. **订阅 URL 明文（含 token）**
    `SubscriptionView.url` 原样保存用户填的 URL（含 token）。把它整串渲染在界面上，
    一次截图/录屏就泄露凭据。这不是状态谎，但它让界面**可见信息**超出了安全边界。

14. **给探测中的节点画转圈**
    `ProbeNodes` 只返回 `Accepted`；单个节点的进行中状态**没有字段**。转圈会让人以为
    系统知道进度（其实不知道）。

15. **把「未知」画成 `--`（破折号）**
    `--` 常见于「无此项」，与「未采样」混用后无法区分「不存在」和「不知道」。
    未知统一用词：「未采样 / 未知 / 未探测 / 未刷新」。

---

## 8. 派生值台账（允许的唯一算式）

| 派生值 | 公式 | 合法前提 | 缺前提时 |
| --- | --- | --- | --- |
| 合计流量 | `stats.uplink_bytes + stats.downlink_bytes` | 同一个 `StatsView` | 「未采样」 |
| 流量速率 | `Δbytes / Δsampled_at_ms` | ≥2 个 `StatsView`，且 `sampled_at_ms` 不同 | 不显示速率 |
| 已连接时长 | `now − connected_since_ms` | `connected_since_ms != null` | 「未知」 |
| 数据陈旧 | `now − stats.sampled_at_ms > 阈值` | `stats != null`；阈值是 UI 常量 | 不显示 |
| 启动耗时 | `datapath.ready_at_ms − t0` | `ready_at_ms != null` **且** `t0` 是 UI 在发出 `Connect` 请求时的真实本地时刻 | 只显示 `ready_at_ms` 的绝对时刻 |
| 节点「当前」判定 | `node_id == NodeView.id && stage == "connected"` | — | 不显示当前标记 |
| 节点「选中」 | `SettingsView.selected_node == NodeView.id` | — | 不显示选中标记 |
| 事件丢帧 | `seq` 不连续 | `Frame.Event.seq` | 不提示（但也不得假装连续） |

**所有派生值必须标注为派生**（例如「合计」），不得伪装成契约字段。

---

## 9. 文案白名单 / 黑名单（给 ux 做 grep 断言）

**黑名单（UI 源码与渲染文本里都不允许出现）**：
`已保护` `保护中` `安全` `正常` `一切正常` `无异常` `已恢复` `自动恢复` `正在重连`
`重试` `重新尝试` `备用节点` `降级` `直连兜底` `已节省` `已拦截` `评分` `健康度`
`预计` `大约需要` `完成 1` `%`（进度语义）、`推荐节点`、`最佳节点`

其中 `重试/降级/兜底/fallback` 一类标识符已被 `scripts/guard.sh` 在 `apps/ui/src` 下扫描。

**白名单（绿色唯一允许出现的上下文）**：
`stage == "connected"` 对应的徽章文字「已连接」；以及 `.probe--ok` 的**实测**数值
（`.probe--ok` 用 `--text`，不是绿色 —— 探测成功不等于连接成功）。

---

## 10. 可自动化断言清单（ux 直接用）

| # | 断言 | 判据 |
| --- | --- | --- |
| U1 | `stats == null` 时统计区含 `.stats--unsampled`，且其中**没有任何数字字符** | `/\d/` 不匹配 `.stats__value` / `.stats__sampled-at` |
| U2 | `stage != "connected"` 时**不存在** `.badge--connected`、`.nodes__row--current` | DOM 查询 |
| U3 | `stage == "connected"` 时徽章文字不含黑名单词 | §9 |
| U4 | `last_error == null` 时不存在 `.error-box`；非空时 `.error-box__code` == `last_error.code` | 文本相等 |
| U5 | 每个 `.log-line__level` 的文本非空 | DOM 查询 |
| U6 | 每个 `.unknown` 内无数字；且 `probe == null` 用 `.probe--none` | `/\d/` |
| U7 | `.badge__phase` 只在 `stage == "connecting" && phase != null` 时存在 | DOM 查询 |
| U8 | `settings.selected_node != node.id` 时不存在 `.nodes__row--selected` | DOM 查询 |
| U9 | 界面上的字节数与 `ConnectionView.stats` 完全一致（含 0） | 数值相等，不做本地四舍五入以外的加工 |
| U10 | UI 源码里不出现 §9 黑名单词与 `preview*`/`mock*` 数据源 | grep（guard.sh 已覆盖 mock 部分） |
| U11 | CSS 中不存在把 `.unknown` / `.stats--unsampled` 指到 `--state-error` / `--state-connected` 的规则 | 静态检查 `styles.css` |
| U12 | `--state-idle`、`--state-unknown`、`--state-error` 三个 token 值互不相等 | 解析 `:root` |
| U13 | 未收到 `Event{state}` 时（只有 `Response(Status)`）界面显示该快照，不显示任何「已完成」 | 断言 `Accepted` 不改变徽章 |
| U14 | `mode == null` 时模式位置显示「未知」，不显示「直连」 | 文本断言 |
| U15 | `stage` 快照未到达时徽章为 `.badge--unknown`（或 `.badge` 无 modifier），**不得**是 `.badge--disconnected` | DOM 查询 |
| U16 | `.stats__value` 带 `.mono`（或 `.num`）：字节数用 tabular-nums，否则实时数字会跳动 | DOM 查询 |

---

## 11. 未验证 / 边界

* 本文的字段路径来自 `crates/xt-contract/src/{model,error,protocol}.rs`（已逐字段核对）；
  **没有**在本机跑过真实 daemon 的 end-to-end 数据（daemon 与 UI 侧文件在本次交付时还不完整）。
* `Frame.Event.seq` 跳号检测是客户端行为，`protocol.rs` 只给出 `seq` 字段与注释要求
  「发现跳号要如实报告」，没有定义提示文案；文案由本文件 §3 Logs 约束。
* `settings` 保存的 `Response` 形状取决于 daemon 实现（`Settings(SettingsView)` 或 `Ok`）：
  协议里两者都可能。UI 必须**两种都处理**：拿到 `Settings` 用返回的 view，拿到 `Ok` 则重发
  `GetSettings` 再渲染 —— 不允许乐观更新。
