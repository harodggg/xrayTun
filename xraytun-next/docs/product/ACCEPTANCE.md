# ACCEPTANCE · 本轮可执行验收判据

> 作者：product · 配套 `SCOPE.md`（做什么）与 `NON-GOALS.md`（不做什么）
> 用途：**ux 直接翻译成测试**；每条判据必须有一个可跑的命令或用例。没有验证手段的判据不许写进来。
> 契约锚点全部来自 `crates/xt-contract/src/{model,protocol,error}.rs`；不变量编号（I1–I5）来自 `docs/architecture/00-CONTRACT-FREEZE.md §0`。

## 0. 三条使用规则

1. **状态只有三种**（外加一个条件交付标记）：
   * `已验证` = 有可复现命令 + 输出摘要 + 日期（口径见 `HANDOFF.md §3`「证据分级」）。
   * `待验证` = 实现或测试尚未落地；**不是通过，也不是失败**。
   * `本轮不验证` = 本轮不做 / 环境不具备（必须附理由与解锁条件，见 §5）。
   * `条件交付` = 只在某个 `Capability` 被宣告时才成立（当前只有 probe，见 §4）；未宣告时按「本轮不验证 + 入口不存在」处理。
2. **谁负责 ≠ 谁验证**：`负责（实现）`是写入者；`负责（独立验证）`必须换人（ux / lead）。实现者自测通过只算 `待验证` 的证据，不算 `已验证`。
3. **判据本身不可被实现者改**：实现者可改测试文件名、socket 路径、fixture 构造方式；改判据（阈值、必须出现的字段、必须不出现的字符串）必须先找 product。证据写到 `docs/verification/**`（该路径归 lead）或实现者自己的报告里。

**本轮总判据（一句话）**：真 xray 在环回上跑通「连接 → 真实 SOCKS 请求 → 真实字节 → 切节点 → 再请求 → 断开」，且界面上每个字都能追到契约字段（`SCOPE.md §3.1`）。

---

## 1. 静态 / 契约层判据（不需要 daemon 跑起来）

| ID | 一句话判据 | 怎么验（命令 / 用例） | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **A-01** | I1/I2/I3 的机器判据全绿 | `bash xraytun-next/scripts/guard.sh` → 期望末行 `GUARD PASSED`、`违规=0 警告=0`。**本轮重跑（2026-09-29，product 回扫时）：`GUARD PASSED`，`crates=26`、`ui=16`、`违规=0 警告=0`**（上一轮的 15 处 `.unwrap()` 警告已由 lead 修复：检查现在跳过 `crates/*/tests/*` 与末尾 `#[cfg(test)]` 之后的行，并打印具体位置）。**数字会随并行开发变动**（本次回扫期间就从 25→26）⇒ 这条判据的**通过条件是 `GUARD PASSED` + `违规=0`**，crate/ui 计数只是当次快照，引用前必须重跑 | lead / lead | **已验证**（当次运行；每次引用前重跑） |
| **A-02** | `ErrorCode` 封闭，无 `Retry`/`Fallback`/`Degraded` 变体；`Unsupported` 语义被限定 | `sed -n '/pub enum ErrorCode/,/^}/p' crates/xt-contract/src/error.rs \| grep -cE '^ +[A-Z][A-Za-z]+,$'` → **11**（本次实测）；`grep -nE '^ +(Retry\|Fallback\|Degraded),' crates/xt-contract/src/error.rs` → **无输出**（本次实测）。⚠️ 不要用不带锚点的 `grep 'Retry\|Fallback\|Degraded'`——它会命中 `:8` 的**注释**，看起来像违规 | lead / product | **已验证**（命令已实跑，2026-09-29） |
| **A-03** | 「未采样」有类型表达，不可能被写成 0 | `sed -n '110,118p' crates/xt-contract/src/model.rs` → `StatsView{uplink_bytes,downlink_bytes,sampled_at_ms}`；`grep -n 'pub stats: Option<StatsView>' crates/xt-contract/src/model.rs` → **`:135`**（本次实测）。`stats` 是 `Option`，没有 `#[serde(default)]` 把它填成 0 | lead / product | **已验证**（命令已实跑，2026-09-29） |
| **A-04** | 「未知」一律 `None`；没有「默认 0 字节 / 默认延迟」字段 | `sed -n '120,139p' crates/xt-contract/src/model.rs \| grep -n 'pub '` → 本次实测：`stage` 是唯一非 `Option` 的状态字段，其余 `phase`/`mode`/`node_id`/`connected_since_ms`/`stats`/`last_error` 均为 `Option`（`datapath` 是结构体，其内部三字段也全是 `Option`）；规则声明在 `model.rs:3-5` | lead / product | **已验证**（命令已实跑，2026-09-29） |
| **A-05** | 能力是可枚举、可宣告的（含订阅拆分） | `sed -n '/pub enum Capability/,/^}/p' crates/xt-contract/src/model.rs` → **6** 个成员 `{ProxyMode,TunMode,Stats,Probe,Subscriptions,SubscriptionFetch}`（`SubscriptionFetch` 在 `:317`，本次实测）；配合 `DaemonHello.capabilities`（`model.rs:293-301`）与契约冻结 §3.1 | lead / product | **已验证**（命令已实跑，2026-09-29）；**「宣告与界面入口一致」见 CAP-01** |
| **A-06** | `cargo tree --workspace` 无环；分层与契约冻结 §2 一致 | `cargo tree --workspace` 对照契约冻结 §2 表 | lead / lead | **待验证**（crate 仍多为占位；依赖落地后跑） |
| **A-07** | 新连接第一帧必须是 `hello`；`protocol_version` 不符即 `invalid_request` 并关连接 | `cargo test -p xt-ipc`（建议用例名 `hello_must_be_first` / `version_mismatch_rejected`）：先发 `Status` 断言错误 + 连接关闭；发 `hello{protocol_version: 0}` 同样 | backend-1 / ux | **待验证** |
| **A-08** | 事件 `seq` 从 1 连续自增；客户端发现跳号要如实报告（不静默） | `cargo test -p xt-ipc`：注入 `seq = 1,2,4` → 断言客户端产出可见的丢帧报告（日志/通知），且**不**把它当成正常续流 | backend-1 / frontend | **待验证** |

---

## 2. 本轮承诺功能的判据

> 判据只看**契约字段与用户可见结果**，不规定内部实现。所有命令里的 `<node>` 为真实节点 id，`$S` 为 daemon socket 路径（实现者定，通常 `/run/xraytun/daemon.sock`）。

### 2.1 连接（D1）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **CONN-01** | `connect` 只受理，终态必达 | `timeout 10 cargo run -p xt-cli -- --socket $S connect <node>`：stdout 出现 `accepted`，且 stdout/事件流在超时前出现 `event state`（成功 `connected` 或失败 `disconnected`）。**超时 = 失败**（不允许石沉大海） | backend-3 / lead | **待验证** |
| **CONN-02** | 阶段顺序不跳、不倒：`Disconnected → Connecting{preparing_config → starting_core → awaiting_ready} → Connected` | 同一命令加 `--print-events`；断言收到的 `Event::State` 序列里 `phase` 按上述顺序各出现一次、`stage` 不发生倒退 | backend-1 + backend-3 / ux | **待验证** |
| **CONN-03** | 显示「已连接」⟺ 数据面**已被证明可连**（proxy：SOCKS 端口真的接受连接） | 收到 `stage == Connected` 后立刻对 `SettingsView.socks_listen`（默认 `127.0.0.1:1080`）做一次非阻塞 TCP connect，断言成功；反向：`Connected` 事件之前的一次尝试必须失败（否则是假连接） | backend-3 / lead | **待验证** |
| **CONN-04** | `connected_since_ms` 来自真实时钟，不是计数器 | 测试记录发起连接前的 `now_ms`（墙上时钟）与 daemon 启动时刻；断言 `connected_since_ms` 落在 `[daemon 启动, 当前]` 且与当前时刻差 < 测试超时上限（10 s） | backend-3 / ux | **待验证** |
| **CONN-05** | `datapath` 是真实进程事实 | 断言 `datapath.pid` 对应的进程存在（`/proc/<pid>` 或 `kill -0 <pid>`）、其可执行文件是 xray；`datapath.version` 与 `/Users/xbtg-/deepseek-harness/.scratch/bin/xray version` 的输出一致；`ready_at_ms` 非空且 ≤ `connected_since_ms` 附近 | backend-3 / lead | **待验证** |
| **CONN-06** | 核心提前退出是**终态**、不是要掩盖的中间态 | 用测试专用的假 xray（**必须标注为测试数据**）：立刻退出 → 断言收到 `stage=disconnected` + `last_error.code = core_exited_early`，且在超时之前到达（不是干等到超时） | backend-3 / ux | **待验证** |

### 2.2 断开（D2）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **DISC-01** | 断开有终态，且进程与端口真的没了 | 已连接时发 `Request::Disconnect`：断言 `Accepted` → 终态 `stage=disconnected`、`datapath.pid=None`；再用 `kill -0` 断言进程不存在、对 SOCKS 端口的连接被拒 | backend-3 / lead | **待验证** |
| **DISC-02** | 重复断开无副作用，且不产生 `internal` 错误 | 连续发两次 `Disconnect`：两次之后终态都是 `disconnected`、`datapath.pid=None`，且任何一次 `last_error.code != internal`（允许 `conflict` 或 `Accepted`，但行为必须显式、不得静默变成「成功连接」） | backend-1 + backend-3 / lead | **待验证** |

### 2.3 切节点（D3）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **SW-01** | 切到可用节点后 `node_id == 目标` | 已连接 A → `SwitchNode(B)`（B 真实可用）→ 断言终态 `Connected` 且 `ConnectionView.node_id == B` | backend-2 + backend-3 / lead | **待验证** |
| **SW-02** | **不回落**（I2 的关键）：切到不可用节点，停在失败并保留选择 | 已连接 A → `SwitchNode(B)`（B 不可达）→ 断言：终态 `stage=disconnected`、`node_id == B`（保留选择）、`last_error` 非空；**且切换请求之后不得再出现 `node_id == A` 的 `Connected` 事件** | backend-2 + backend-3 / **lead 亲自验** | **待验证** |
| **SW-03** | 节点清单只包含真实可解析的节点 | `ListNodes` → 断言每个 `NodeView.endpoint` 可解析为 `host:port`；`source` 如实（`subscription{sub id}` 或 `manual`）；解析不出来的节点**不出现**（`model.rs:194-195` 注释「宁缺毋假」） | backend-2 / ux | **待验证** |

### 2.4 真实流量统计（D4）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **STAT-01** | 未采样就显示「未采样」，**绝不显示 0** | 构造 `stats == None`（未连接，或 StatsService 不可达）→ UI 渲染文本含「未采样」或等价表述，且**不包含 `0 B`**；反向：`stats = Some(uplink=0,downlink=0)` 是**合法真实值**（真的传了 0 字节），可以显示 0 | frontend + ui / ux | **待验证** |
| **STAT-02** | 字节数来自真实采样，与源站真实发出的字节对账 | E2E 内：本地 HTTP 源站记录发出的 body 字节 `N`；经 SOCKS 下载完成后断言 `downlink_bytes` 增量 ≥ `N`（下界不可改）；同时断言增量 ≤ `N + 64 KiB`（防止把无关流量算进来；阈值调整需在报告里给理由）；`sampled_at_ms` 递增 | backend-3 / lead | **待验证** |
| **STAT-03** | 采样时刻真实；断开重连后不沿用旧值 | 断言 `sampled_at_ms > 0`（构造强制）且 `sampled_at_ms >= connected_since_ms`；断开 → 再连接后，新 `StatsView.sampled_at_ms` 必须大于上一次断开时刻 | backend-3 / ux | **待验证** |
| **STAT-04** | 统计只有一个来源（没有第三条） | `grep -rn "stats\|traffic" crates/ apps/ui/src/`：生产路径只出现 `xt-stats`（StatsService 采样）与进程事件；`grep -rn "mock\|preview\|fixture" crates/ apps/ui/src/` 在生产路径 0 命中（测试目录允许，且 fixture 必须由契约类型构造并标注） | lead / product | **待验证** |

### 2.5 真实界面与能力宣告（D5 / D9）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **UI-01** | 生产构建里没有 mock/preview 作为运行时数据源 | 构建产物（`apps/ui` 的 build 输出 + 打包产物）grep `preview`/`mock`/`fixture` → 0；运行时数据只能经 `xt-ipc` 契约类型进入（契约冻结 §7） | frontend / lead | **待验证** |
| **UI-02** | 「已连接」当且仅当 `stage == Connected`；`Connecting` 必须显示子阶段 | 用 6 个契约构造的 `Event::State` 帧（`Disconnected` / `Connecting`×3 个子阶段 / `Connected` / `Disconnecting`）驱动界面：断言只有 `Connected` 帧渲染「已连接」，且 `Connecting` 帧渲染对应子阶段文案（不渲染「已连接」） | ui / ux | **待验证** |
| **UI-03** | 未知不许显示成默认值 | `stats=None` → 「未采样」（不是 0）；`datapath.pid=None` → 界面不出现 pid 文本；`selected_node=None` → 不出现任何节点名 | ui / ux | **待验证** |
| **UI-04** | daemon 身份不写死 | 用契约构造的 `Response::Hello{daemon_version:"9.9.9-test"}` 驱动界面，断言该字符串出现在界面上；同时 grep 生产源码里没有硬编码版本常量 | frontend + ui / ux | **待验证** |
| **UI-05** | seq 跳号对用户可见（不静默漂移） | 注入 `seq = 5` 后下一帧 `seq = 7`：断言 CLI/界面产出可见报告（通知或日志），不得静默续流（`protocol.rs:11-12`） | backend-1 + frontend / ux | **待验证** |
| **CAP-01** | **能力必须由 `capabilities` 宣告；界面只渲染已宣告的入口**（未实现 = 入口不存在，不是灰按钮/禁用态） | 用契约构造的三个 `DaemonHello` fixture 驱动界面：① 含 `Capability::{Subscriptions,Probe}` → 订阅区块与探测入口都存在；② 只含 `Subscriptions`（本轮默认形态，`SubscriptionFetch` 不宣告）→ 探测入口 `querySelector` 为 `null`；③ 三者共同断言：**「添加订阅 / 刷新订阅」入口在任何 fixture 下都不存在**（远端拉取本轮不做），且不存在 `disabled` 的替代控件 | frontend + ui / ux | **待验证** |
| **CAP-02** | 不存在第二套开关来绕过 `capabilities` | `grep -rn "capabilities" apps/ui/src/`：capability → 入口的映射是唯一判据；不得有独立的 feature flag / 环境变量 / 版本号比较来控制入口 | frontend / lead | **待验证** |
| **CAP-03** | `Unsupported` **只能**表示「本版本不提供该能力」，绝不包装真实失败 | 审 daemon 里每个 `ErrorCode::Unsupported` / `unsupported(` 调用点（`error.rs:33/52/105-106`）：每处都必须对应一个**未宣告的能力**（`AddSubscription`/`RefreshSubscription`/TunMode 等）；反向造例——让核心退出/配置非法/IO 失败各触发一次，断言错误码是 `core_exited_early`/`config_invalid`/`io` 而**不是** `unsupported`。规则依据：契约冻结 §3.1 | backend-3 / lead | **待验证** |

### 2.6 本地订阅文件（D8）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **SUB-01** | 本地订阅原文能被解析成真实节点，且来源如实 | 把一份测试用订阅原文放到 `$state_dir/subscription.txt`（或 `--subscription-file` 指定），启动后 `ListNodes`：断言节点数 > 0、每个 `endpoint` 可解析、`source == subscription{id}`；4 种格式的用例覆盖 `xt-subs` 的既有解析路径 | backend-2 / ux | **待验证** |
| **SUB-02** | 不接受远端拉取，也不假装接受 | 界面/CLI **不得**出现订阅 URL 输入框或「刷新订阅」入口；对 `Request::AddSubscription{url}` / `RefreshSubscription{id}`，daemon 必须回 `Outcome::Error{code: Unsupported}`（不许 `Accepted` 后无终态）。前置依赖**已解除**：`ErrorCode::Unsupported` 已落地（`error.rs:33/52/105-106`）、`Capability::SubscriptionFetch` 已落地（`model.rs:317`），见 §7 | backend-2 + frontend / ux | **待验证** |

### 2.7 失败诚实归因（D7）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **ERR-01** | 失败必须落到 `Disconnected`/`Disconnecting` 且带 `last_error` | 对 connect / switch / disconnect 各注入一次失败：断言终态 `stage ∈ {disconnected, disconnecting}` 且 `last_error` 非空（契约冻结 §4） | backend-1 + backend-3 / lead | **待验证** |
| **ERR-02** | 失败是终态，不会被「后来起来了」替换成成功话术 | 失败后再成功连接：断言失败事件里的 `last_error` 在该事件中不被改写；成功后的快照显示新的 `Connected`，但**不出现**「刚才失败过但没事」之类归纳性文案 | backend-1 + backend-3 / ux | **待验证** |
| **ERR-03** | 全链路没有 retry/fallback/backoff 的行踪 | `bash scripts/guard.sh` 全绿 + 在 E2E 事件日志里 `grep -Ei 'retry\|fallback\|backoff'` 0 命中 | lead / lead | **待验证**（guard 部分本次已 PASSED；含行为的部分待 E2E） |
| **ERR-04** | `last_error.message` 是可直接给用户看的人话，且 `code` 稳定可本地化 | 对每个可触发的 `ErrorCode`，断言 `message` 非空、不含内部路径/堆栈；`code` 用 `as_str()` 的稳定串（`error.rs:35-51`），UI 用它做本地化而不是匹配中文 | backend-* + frontend / ux | **待验证** |

### 2.8 日志（D10，lead 2026-09-29 裁定「本轮做」）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **LOG-01** | 日志行来自**核心真实输出**，不是我们编的摘要 | 真 xray 跑起来后 `Request::TailLogs{lines:200}` → 断言返回的 `LogLine.message` 里包含核心自己输出的行（用 xray 启动输出的稳定片段匹配，例如版本行/入站监听行）；`target` 是发出日志的组件，不是 UI 页签名（`model.rs:180`） | backend-2 + backend-3 / ux | **待验证** |
| **LOG-02** | 时间与等级是真实观测 | 断言 `LogLine.ts_ms > 0` 且随时间递增（不出现同一 `ts_ms` 大量重复的伪造值）；`level ∈ {error,warn,info,debug}`（`model.rs:156-174`）来自真实行，不许把所有行硬塞成 `info` | backend-2 / ux | **待验证** |
| **LOG-03** | 只看订阅过的主题；`TailLogs` 不超过请求条数 | 未 `Subscribe{Topic::Log}` 时不推送 `Event::Log`（`model.rs:313-314`「客户端只收到它订阅过的事件类型」）；`TailLogs{lines:N}` 返回条数 ≤ `N` 且是最近 `N` 条 | backend-1 + backend-2 / ux | **待验证** |

### 2.9 设置（D11，lead 2026-09-29 裁定「本轮做」）

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **SET-01** | patch 语义：`None` = 不改这一项 | 先 `GetSettings` 记下 `selected_node`/`log_level`；只发 `PatchSettings{patch:{socks_listen:"127.0.0.1:11080", others: None}}` 后再 `GetSettings`：断言 `socks_listen` 变了、另两项**逐字节不变**（`model.rs:281-291`） | backend-2 / ux | **待验证** |
| **SET-02** | 改动**真的生效**，或明确说清何时生效 | 改 `socks_listen` 后：新端口上发起一次 TCP connect 成功、旧端口不再接受（**或**界面/响应明确给出「需要重连才生效」并验证重连后生效）；改 `log_level=debug` 后日志里真的出现 `debug` 行。两条路径二选一必须显式可见，不许静默 | backend-2 + backend-3 / lead | **待验证** |
| **SET-03** | 设置落盘，且不产生任何自动行为（I2） | patch 后重启 daemon → `GetSettings` 仍为改动后的值（`xt-settings` 单文件 + 原子替换）；另外断言：重启后**不会**因为 `selected_node` 或上次连接状态而自动 `Connected`——必须有显式 `Request::Connect{node_id}` 才连接 | backend-2 + backend-3 / lead | **待验证** |

---

## 3. 端到端判据（本轮总判据，真进程 / 真字节）

环境事实（契约冻结 §8）：真 xray 在 `/Users/xbtg-/deepseek-harness/.scratch/bin/xray`（Xray 26.3.27 linux/amd64）；本地 HTTP 源站（真 socket、真字节）；真 AF_UNIX 传输；允许把服务端放环回以避免外网，**不允许在被测路径上放 mock**。

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **E2E-01** | 整条真实通路：连接 → 真实 SOCKS 请求 → 真实字节 → 切节点 → 再请求 → 断开 | `cargo test -p xt-daemon --test e2e_proxy -- --nocapture`（测试名由 backend-3 定，判据不可改）。断言顺序：`Connected(A)`（SOCKS 可连）→ 经 SOCKS 取 `N` 字节 → `downlink_bytes` 增量 ≥ `N` → `SwitchNode(B)` → `Connected(B)` → 再取数 → 字节继续增长 → `Disconnect` → `Disconnected` + 进程消失 | backend-3 / **lead 亲自跑** | **待验证** |
| **E2E-02** | E2E 不依赖外网 | 断网环境（或 `iptables`/网络命名空间隔离）下 E2E 仍全绿；断言没有到公网的连接 | backend-3 / lead | **待验证** |
| **E2E-03** | E2E 内没有 sleep / 轮询 | `grep -nE 'sleep\|setInterval\|loop \{' crates/xt-daemon/tests/` → 无命中；就绪只由「事件 + 一次非阻塞尝试」判定（I1） | backend-3 / lead | **待验证** |
| **E2E-04** | 无头与界面同契约（「UI 能做的，无头也能做」） | E2E 用 `xt-cli` 覆盖 connect / switch / disconnect / stats 四件事；界面层测试复用同一批契约 fixture（不各写一套） | backend-2 + ux / lead | **待验证** |

---

## 4. 条件交付：probe（延迟探测）

> `[裁决]` lead 2026-09-29：probe **做成才宣告**。下表是**条件判据**——不满足「已宣告」前提时，「本轮不验证」即视为正确。

| ID | 一句话判据 | 怎么验 | 负责（实现 / 独立验证） | 状态 |
| --- | --- | --- | --- | --- |
| **COND-PROBE-01** | 若 `DaemonHello.capabilities` 含 `Capability::Probe`：`ProbeNodes` 的每条结果必须恰好 `Some` 一个字段（`ttfb_ms` 或 `error`），且 `ttfb_ms` 来自真实 TTFB | `ProbeResult::is_consistent()` 为 `true`（`model.rs:229-231`）；`Event::Probe` 条数与请求的 `node_ids` 数一致；`ttfb_ms` 与本地对环回源的实测 TTFB 同量级（记录两者） | backend-3 / ux | **条件交付**（若未宣告 `Capability::Probe` → 本条记「本轮不验证」，并执行 CAP-01 的「入口不存在」断言） |
| **COND-PROBE-02** | 若未宣告：`ProbeNodes` 必须给明确 `ErrorBody`，不得 `Accepted` 后无终态 | 在未宣告 probe 的 daemon 上发 `ProbeNodes`：断言立刻收到 `Outcome::Error`（例如 `not_found`/`conflict`/`invalid_request`），且事件流里不会出现 `Event::Probe` | backend-3 / lead | **条件交付** |

---

## 5. 本轮不验证（附理由与解锁条件）

> 「本轮不验证」是**如实登记**，不是「干净」。允许进入本表的理由只有三类：本轮不做、环境不具备、判据依赖不存在的数据源。

| ID | 不验证的东西 | 理由（证据） | 解锁条件 |
| --- | --- | --- | --- |
| **NV-01** | macOS TUN：建卡 / 路由接管 / DNS / 崩溃回滚 | 本轮不做（`SCOPE.md §4`）；开发机非 macOS（`HANDOFF.md §4`）；旧仓库同类现场仍开放（`OPEN-FINDINGS.md` F-1 / F-4） | 有 helper + macOS 真机 |
| **NV-02** | 特权 helper 安装 / 授权 / 卸载 | 本轮不做 | 与 NV-01 同批 |
| **NV-03** | 审计上传（加密、定时、撤回） | 本轮不做（契约冻结 §8） | 用户要求支持通道时 |
| **NV-04** | 地理 / 拓扑可视化（流向图、地球仪、规则链） | 本轮不做；数据源（访问日志/规则链/位置查询）不存在 | 数据源落地之后 |
| **NV-05** | 自动重连 / 回落 / 重试 / 看门狗 | **永不做**（I2 + v0.9.2 用户裁决）；不是「待验证」——`ErrorCode` 里连成员都没有 | 需先在 `ErrorCode` 加成员并过评审，再做 |
| **NV-06** | 真机 macOS 交互：`⌘,`、托盘、Esc、helper 弹窗、真实切换耗时 | 无 macOS 机器（`0.9-USER-TEST.md §3.2` 人工清单；`HANDOFF.md §2.2` 真机验收只有用户能做） | 用户在真机上点一遍并回填证据 |
| **NV-07** | 订阅远端拉取（http/https）/ 分流 / 开机自启 / 自更新 / 托盘 | 本轮不做（`NON-GOALS.md §1.7`）；订阅区块只读，无添加/刷新入口 | lead 裁定并写入范围之后 |
| **NV-08** | 延迟探测（probe）的完整验收 | 条件交付（§4） | `Capability::Probe` 被宣告之后 |
| **NV-09** | 像素级布局判据（旧 PRD 的 `top < 636` 之类） | 新仓库没有 `?preview=1`（契约冻结 §7 禁止运行时 mock）；jsdom 无布局（`0.9-USER-TEST.md §5.2`） | 有真实渲染环境的 UI 测试通道；在那之前只用 DOM 顺序/文本/字段判据 |
| **NV-10** | 旧仓库 v0.9.2 的所有既有行为在重写后的**等价性** | 重写不复用旧实现（旧仓库只读参考）；逐项等价性不是本轮目标 | 按功能逐轮补回时各自建立判据 |

> 说明：**日志页与设置页本轮已从本表移出**——lead 2026-09-29 裁定它们「本轮做」，判据见 §2.8 / §2.9。某条判据从本表移出，意味着它必须转成「待验证」并最终拿到证据，不能停在「不验证」。

---

## 6. 状态统计（随实现更新；本表是「待验证 ≠ 通过」的提醒）

| 状态 | 条数 | 明细 |
| --- | --- | --- |
| 已验证 | 5 | A-01 `guard.sh`（2026-09-29 当次运行：`crates=26`/`ui=16`/`违规=0 警告=0`）+ A-02 ~ A-05 四条契约代码事实（命令已实跑） |
| 待验证 | 42 | §1 的 A-06/A-07/A-08（3）＋ §2.1–§2.9 功能判据（35）＋ §3 E2E（4） |
| 条件交付 | 2 | COND-PROBE-01 / -02（取决于 `Capability::Probe` 是否宣告） |
| 本轮不验证 | 10 | §5，全部附理由与解锁条件 |

§2 的 35 条明细：CONN 6 / DISC 2 / SW 3 / STAT 4 / UI 5 + CAP 3 / SUB 2 / ERR 4 / LOG 3 / SET 3。

**「已验证」回扫记录（2026-09-29，lead 要求）**：A-01 ~ A-05 逐条在本轮仓库里重跑命令并写下当次输出，**没有一条引用旧仓库结论**（旧仓库 `xray-tun/` 只作为"为什么"的证据出现在旁注里，不作为通过证据）。回扫中修掉一条**假判据**：A-02 原先写的 `grep -nE 'Retry|Fallback|Degraded' error.rs → 无输出` 是错的——它会命中 `error.rs:8` 的注释；现已改成带锚点的变体匹配并实跑。若以后再出现"命令没跑过就写已验证"，按同口径改回「待验证」。

**转状态的硬要求**：`待验证 → 已验证` 必须附「命令 + 输出摘要 + 日期」，并且由**非实现者**复跑（ux 或 lead）。没有这一步，任何「通过」都是 `HANDOFF.md §3` 说的那种「没有证据 ≠ 干净」。

---

## 7. 契约变更记录（**已落地，不再是阻塞**）

lead 2026-09-29 的裁定需要动冻结契约（`crates/xt-contract/**` 是 lead 的独占写入路径，契约冻结 §9）。product 不自行修改；**以下是 product 于 2026-09-29 回扫时重新读取当前文件后复核的结果**：

| # | 变更 | 落地位置（已复核） | 影响的判据 |
| --- | --- | --- | --- |
| P1 | 新增 `Capability::SubscriptionFetch`（本轮**不宣告**） | `model.rs:317`，与 `Subscriptions` 并列；粒度理由在变体注释里（粗粒度会逼界面显示按不动的「刷新」假控件） | CAP-01、SUB-02 |
| P2 | 新增 `ErrorCode::Unsupported` + `as_str` + 便捷构造 | 变体 `error.rs:33`；`as_str` `error.rs:52`（`"unsupported"`）；`unsupported(msg)` `error.rs:105-106`。现共 **11** 个成员 | SUB-02、CAP-03 |
| P3 | 能力宣告 + `Unsupported` 使用纪律成文 | 契约冻结 **§3.1**：`capabilities` 是唯一事实来源；未宣告能力的请求返回 `Unsupported`；该码**只能**表示「本版本不提供」，**不得**包装「试了但失败」 | CAP-01、CAP-02、CAP-03、ERR-03 |

⇒ 原「阻塞项」已解除：`SUB-02` / `CAP-01` 按正常「待验证 → 已验证」流程推进即可（本节只是变更记录，不再需要谁先动手）。实现者仍**不得**自造不在契约里的错误码或能力串。

> 为什么把 §3.1 也记在这里：它是本轮唯一一条新增的**跨层硬规则**——daemon 的 `Unsupported` 和界面的「入口是否存在」必须指向同一份 `capabilities`；两边各写一套判据，就会回到旧仓库「有入口无实现 / 有实现无入口」的老问题（见 `NON-GOALS.md §2` O6/O7）。
