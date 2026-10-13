# xraytun-next（v1 线）下一版 · UI 设计：仪表板对齐老版 XrayTun

> **用户裁决（2026-10-01，逐字）**：
> 1. 仪表板，以及仪表盘的 UI 逻辑**完全复制** xraytun（= 老版 XrayTun v0.9.2）；
> 2. **删掉**意图、拓扑功能；
> 3. **保留**日志、节点、订阅、开始面板、网速功能；
> 4. **不要动 xraytun v3**（分支 `xraytun3` 与目录 `xraytun3/**`，那是第三个版本，设计文档保持空白）；
> 5. **后端逻辑不要改**。
>
> 一句话：**这是纯 UI 版本** —— `crates/**` 一行不改，仪表板用 v1 契约里**真实存在**的字段重建老版的形状与判据；没有契约字段支撑的老版元素**一律不显示**（不许造控件，I3）。

本文件位置：`xraytun-next/docs/design/NEXT-VERSION-UI.md`（xraytun-next 真源树；仓库快照同步到分支 `xraytun-next-v1`）。
老版只读参照：仓库里 `apps/**`（v0.9.2，main 线）——**只读**，且**不许**把其中的意图/拓扑页复制进来（§5）。

---

## 0. 范围与硬边界（机器可查）

| 项 | 值 |
| --- | --- |
| 目标版本 | **xraytun-next v1 线的下一版**；当前基线 v1.1.5 = 分支 `xraytun-next-v1` @ `1cd4c31` |
| 可以改 | `xraytun-next/apps/ui/**`（`src/**`、`tests/**`、`styles.css`）、本设计文档 |
| 不许改 | `xraytun-next/crates/**`（`xt-contract`/`xt-daemon`/`xt-cli`/…）、`xt-ipc` 帧格式、协议与事件、能力宣告 |
| 明令不动 | 分支 `xraytun3` 与 `xraytun3/**`（裁决 4） |
| 老版参照 | `apps/ui/src/**`（v0.9.2，只读；含 `pages/Dashboard.tsx` 等） |

**判据**

1. `git diff --name-only <base>..HEAD -- xraytun-next/crates xraytun-next/scripts` **输出必须为空**（「后端零改动」的机器判据，不靠跑 CI 自证）。
2. `bash xraytun-next/scripts/guard.sh` 仍绿（五条不变量的判据）。
3. `cd xraytun-next/apps/ui && npm test && npx tsc --noEmit && npm run build` 全绿。
4. `apps/ui/src/transport/contract.guard.test.ts` 仍绿（UI 不得越过契约认 Rust 内部类型）。
5. **本版新增的 UI 判据**：老版有、契约没有的元素，界面上**必须不存在**（逐条见 §2.2 的断言清单）。

**继承的硬约束**（`docs/architecture/00-CONTRACT-FREEZE.md` §0、§9）：I1 无等待（**含 UI 不许 `setInterval`/`setTimeout`/`sleep`，`scripts/guard.sh:96-97` 扫**）、I2 无回落（失败即终态，不许「重试/自动换节点」话术）、I3 无假话（未知 = `None`/「未采样」，不许 0 顶替）、I4 单向依赖、I5 权限最小面。文件所有权仍按 §9：`apps/ui/src/pages/**`、`App.tsx` 属 **ui**；`styles.css`、`docs/design/**` 属 **design**；`apps/ui/tests/**`、`docs/ux/**` 属 **ux**。

---

## 1. 现状：为什么「复制老版仪表板」不是照抄一页

| 维度 | 老版 XrayTun v0.9.2（`apps/**`） | v1.1.5（`xraytun-next/**`） |
| --- | --- | --- |
| 页面 | 8 项导航：仪表盘 / 节点 / 订阅 / 分流 / 意图过滤 / 拓扑 / 位置 / 日志 / 设置（`apps/ui/src/App.tsx:42-56`） | 4 项：**状态 / 节点 / 日志 / 设置**（`xraytun-next/apps/ui/src/App.tsx:13-20`） |
| 仪表板 | 状态区 + meta 行 + 更新 chip + 快捷按钮 + 流量区 + 排序横幅 + 折叠诊断（`apps/ui/src/pages/Dashboard.tsx` 699 行） | 「连接 / 真实流量 / 数据面 / 最近一次失败」四张卡（`xraytun-next/apps/ui/src/pages/Dashboard.tsx` 259 行） |
| 顶栏 | **有**：状态线 + 圆点 + 徽章 + 唯一主操作按钮（`apps/ui/src/App.tsx:386-614`，按钮 `595-611`） | **没有顶栏组件**（连接/断开在 Dashboard 卡里，`xraytun-next/.../Dashboard.tsx:122-162`）——而 `docs/ux/IA.md:57-71` 已声明「顶栏（全局常驻）」 |
| 网速 | 有：**后端 1s 采样定时器**算速率（`apps/desktop/src/traffic.rs:30,58-98`），前端 `formatRate(rx_rate/tx_rate)`（`apps/ui/src/pages/Dashboard.tsx:415-426`） | **没有**：契约只有累计字节，无速率字段（`crates/xt-contract/src/model.rs:112-118`） |
| 通知 | 无事件通道；靠快照字段拼 | 有 `Event::Notice`（`model.rs:242-248`），但**没有任何页面渲染它**（`apps/ui/src/store/daemon.ts:49-53`） |
| 意图 / 拓扑 | 有实现：`pages/Intent.tsx`、`pages/Topology.tsx`、`pages/Globe.tsx`、`pages/Routing.tsx`、`apps/ui/src/topology/**`、`crates/xt-intent/**` | **零实现**（实现/类型/字段 0 命中；命中全在文档里，且都是「不做」的裁决） |
| 后端 | Tauri 命令 + 快照结构 `AppSnapshot`（`apps/ui/src/types.ts:901-934`） | daemon + 冻结契约（`xt-contract`），**本版不许动** |

⇒ 本版的实质：**把老版的「仪表板形状与判据」搬进 v1 的四页壳里，并补回一个顶栏**；凡是老版靠 `AppSnapshot` 里的 `core/helper/update/routing` 才成立的元素，在 v1 里**没有数据源，直接不显示**。

---

## 2. 仪表板：按老版重建（含 UI 逻辑）

### 2.1 要抄的区块（老版证据 → v1 落地绑定）

| # | 区块 | 老版位置 | 老版行为要点（逐字文案以老版文件为准） | v1 绑定 |
| --- | --- | --- | --- | --- |
| 1 | 状态区：圆点 + 状态词 + 节点名 + 延迟徽章 | `apps/ui/src/pages/Dashboard.tsx:253-295`；语义真源 `apps/ui/src/topbarStatus.ts:133-244` | 5 档 tone（on/partial/off/busy/failed）；**顶栏的线/点与这里的状态词必须同源** | `ConnectionView.stage/phase` + `node_id` + `NodeView.probe`（`ProbeResult`：`ttfb_ms` 与 `error` **恰好一个**为 `Some`，`model.rs:207-231`） |
| 2 | meta 行：模式 · 分流预设 · 网卡名 · 已运行 | `Dashboard.tsx:297-313` | 一行串起多个事实，`·` 分隔 | `ConnectionView.mode` + `connected_since_ms`；**预设/网卡名 v1 无**（§2.2） |
| 3 | 更新提示 chip（available/latest/failed/unknown 四态） | `Dashboard.tsx:325-372`、`updateNotice()` `:155-182` | 「客户端检查失败」**绝不**写成「已是最新」 | **v1 无更新检查** → 本版不显示（§2.2） |
| 4 | 快捷按钮：添加订阅 / 选择节点 / 切换节点 + 测试延迟 | `Dashboard.tsx:389-404` | 无节点时主按钮变「添加订阅」；测延迟按钮由 `probing` 门控 | `ListNodes` + `ProbeNodes` + 现有导航（`Request::ProbeNodes`，`protocol.rs:40`） |
| 5 | 直连模式提示 | `Dashboard.tsx:407-411` | 说明「直连模式核心不接管流量」 | v1 无 direct 档 → 不显示该提示（§2.2） |
| 6 | 流量区：↓ 速率 + 累计 ／ ↑ 速率 + 累计 | `Dashboard.tsx:415-426`；格式化 `apps/ui/src/types.ts:1047-1060` | 两个 metric：速率 + 「累计 X」 | `StatsView.uplink_bytes/downlink_bytes/sampled_at_ms`；**速率在 UI 里派生**（§4），**不照抄它的 0 缺陷**（§2.2） |
| 7 | 提示横幅：按 severity 排序，只显最急一条 + 「还有 N 条提示」 | `Dashboard.tsx:429-445` + `collectNotices()` `:536-680` | `rank` 越小越急；`error` → `role="alert"`，其余 `role="status"`；破坏性动作带二次确认 | 骨架照抄；条目按 v1 契约重列（§2.3） |
| 8 | 折叠的「环境自检与诊断」 | `Dashboard.tsx:448-521` | 核心 / helper / 进程 / 配置路径 + 4 个动作按钮 | 只留契约有的：`DatapathView.pid/version/ready_at_ms` + 动作（更新全部订阅 / 打开数据目录 / 查看日志 / 全部设置） |
| 9 | 「还没有状态」的兜底页 | `apps/ui/src/SnapshotState.tsx:24-47`，逐字 `正在读取状态…` / `读不到状态：{原因}` | 区分「还没读到」与「确实是空」 | `stage`/`connection` 为 `null` 时同样只显示「正在读取状态…」，**不许**显示成「未连接」 |
| 10 | 主操作按钮**只在顶栏一颗** | `apps/ui/src/App.tsx:595-611`；`runButtonDisabled()` `topbarStatus.ts:283` | 仪表板里不重复渲染第二颗（老版 task-72 特意收敛过） | 把现在 Dashboard 里的连接/断开**移到新顶栏**（§3） |
| 11 | 样式与配色 | 老版 `apps/ui/src/styles.css`（`dash__*` / `banner--{error,warn,info}` / `badge--{fast,ok,slow,unknown}` / `dot--*` / `topbar--*`） | 线、点、状态词三处同色 | 搬进 `xraytun-next/apps/ui/src/styles.css`（design 所有） |
| 12 | 导航第一项标签 | 老版：`仪表盘`（`App.tsx:43`） | — | v1 现为「状态」（`App.tsx:16`）；本版改为**「仪表盘」**，顺带消除 `docs/ux/IA.md:166` 与实现的漂移 |

### 2.2 绑不上契约的项（后端不改 ⇒ **明确不显示**；以及老版自身的缺陷不许照抄）

| 老版字段 / 行为 | v1 契约 | 本版处理 |
| --- | --- | --- |
| `settings.routing_preset`（分流预设） | `SettingsView` 只有 `socks_listen`/`selected_node`/`log_level`（`model.rs:262-269`） | 不显示 |
| `settings.mode = direct`（直连档）+ 直连提示 | 没有 direct；只有 `RunMode::{Proxy,Tun}` + 断开 | 不显示该档与那条提示；空态由 `stage=disconnected` 表达 |
| `core.path / version / supports_native_tun / min_native_tun_version` | 只有 `DatapathView.version`（真实 `xray version` 输出） | 诊断区只显示 pid/version/ready_at；**不显示核心路径与「是否支持原生 TUN」** |
| `helper.*`（reachable/version/protocol/tun_active/socket_present/stale_session） | 契约里**没有** helper 视图（I5：本轮 helper 未实现） | 不显示（连带老版 rank 1/2/3 三条横幅一并删除） |
| `update.*`（app/core/geo 三路检查） | 无 | 不显示 |
| `runtime.tun_interface` / `runtime.config_path` | 无 | 不显示 |
| `node_health`（每节点最近失败类别） | 无 | 不显示 |
| `login_item` / `dns` / `app_version` | 无（只有 `DaemonHello.daemon_version`） | 不显示 |
| `latency[].server_rtt_ms` + `available` | `ProbeResult.ttfb_ms` + `error`（语义等价，术语从「RTT」改「TTFB」） | 按 v1 语义显示；`error` 非空 = 「不可用」，**不许**用延迟数字替「能不能用」背书 |
| **回落提示**：`你选的：{name}（本次未使用）`、`topbar__in-use` 徽章（`Dashboard.tsx:264-268`、`nodeInUse.ts`） | v1 **没有回落**（I2）；`SettingsView.selected_node` 与 `ConnectionView.node_id` 的区分仍然真实 | **不复制「回落」字样**。要保留的只是「你选的 / 正在用的」这个事实区分，文案不得出现「回落 / 未使用 / 已为你切换」 |
| 流量区**无条件渲染**，未采样时显示 `0 B/s`（`Dashboard.tsx:415-426`；Rust 首拍速率硬编码 0，`apps/desktop/src/traffic.rs:83-89`） | v1：`stats: Option<StatsView>`，`None` = **从未采样成功** | **不照抄**：`stats == null` ⇒ 显示「未采样」，且**不出现** `0 B/s` / `累计 0 B`（I3） |

> **这一节本身就是验收项**：表中每一行都要有一条 UI 断言，证明界面上**没有**该元素（`apps/ui/tests/acceptance/truthfulness.test.tsx` 已用同样手法钉住「导航不得出现拓扑/地球/定位」）。

### 2.3 提示横幅（v1 版条目表）

| rank | 条件 | 文案 | 动作 |
| --- | --- | --- | --- |
| 0 | `ConnectionView.last_error` 非空且当前未连接 | 「上次运行出错：<原文>」 | 「查看日志」（有对应能力时才给别的跳转） |
| 1 | `Event::Notice`（`severity = error`） | daemon 给的原文，**不改写** | 按 `code` 给一个跳转或没有 |
| 2 | `Event::Notice`（`warning` / `info`） | daemon 给的原文 | 无 |
| 3 | `ListNodes` 为空（还没有任何节点） | 「还没有节点 —— 先去添加订阅」 | 跳订阅区（`Settings` 的订阅分区） |

规则照抄老版：rank 越小越急；只显示第一条，其余折成「还有 N 条提示」；`error` → `role="alert"`，其余 `role="status"`。**不许**把 notice 改写成「已自动处理 / 正在重试 / 已为你切换」（I2、I3）。
⚠️ 现状缺口：`store/daemon.ts:49-53` 已经缓冲了 `notices`，但**没有任何页面渲染**——本版把它接上。

### 2.4 状态词映射（老版 tone → v1 stage）

老版 `topbarStatus.appStatus` 的 5 档 tone 全保留（`partial` 只用于 `connecting + phase = committing_routes`，与老版「隧道已建立、默认路由未接管」同义）：

| v1 输入 | tone | 状态词 |
| --- | --- | --- |
| 快照/事件还没到 | `off` | 「正在读取状态…」（**不是**「未连接」，也不是绿色；取自老版 `SnapshotState.tsx:47`） |
| `stage = disconnected` + `last_error` 非空 | `failed` | 「上次运行出错」+ 下一步 |
| `stage = disconnected` + 无错误 | `off` | 「未连接」 |
| `stage = connecting`（`preparing_config`/`starting_core`/`awaiting_ready`） | `busy` | 「连接中」+ 阶段说明（沿用 v1 `PHASE_TEXT`，语义 = 老版「卡在哪一步」） |
| `stage = connecting` + `phase = committing_routes` | `partial` | 「隧道已建立，默认路由尚未接管」 |
| `stage = connected` | `on` | 「已连接」+ 节点名 + TTFB 徽章 |
| `stage = disconnecting` | `busy` | 「断开中」 |

`mode` 为 `None` ⇒ 显示「未知」，不许默认成 proxy。

---

## 3. 开始面板（= 新顶栏）

**「开始面板」是老版顶栏那一块，不是本仓库既有组件名**（全仓源码/文档/git 历史 `开始面板|StartPanel|start-panel` 0 命中；老版对应物是 `apps/ui/src/App.tsx:386-614` 的顶栏 + `apps/ui/src/pages/Dashboard.tsx:253-404` 的状态区与动作行）。v1 现在**没有顶栏**（`docs/ux/IA.md:57-71` 声明的顶栏从未实现），所以本版**补回一个顶栏**，它就是「开始面板」。

内容（照老版顶栏形状，去掉 v1 没有的数据源）：

1. 状态线 + 圆点（与 §2.4 同源、同色）；
2. 当前节点徽章（`node_id`；未选择时显示「未选择」）；
3. 失败/阻塞徽章：有 `last_error` 时显示原文摘要；有操作在跑时显示「请求已受理，等待事件返回终态」（沿用 v1 现有逐字文案，`Dashboard.tsx:53-63`）；
4. **唯一主操作按钮**：`stage = connected` ⇒ 「断开」；否则「连接」；
5. 「为什么现在点不动」的 `title`/note：`requestPending`、`stage ∈ {connecting, disconnecting}`、`node_id == null`（「未选择节点」）各给一句真实原因；
6. `capabilities` 含 `tun_mode` 时才给「入站模式」选择器（选项逐字「代理模式（SOCKS 入站）」/「TUN 模式」，沿用 `Dashboard.tsx:123-137`）；
7. **保留** v1 的传输层失败盒（`App.tsx:61-70`，带「重新连接」动作）——老版没有它，但它是 v1 唯一的传输失败出口，本版放在顶栏下方，不藏。

禁用判据只写一处（老版抽成 `topbarStatus.runButtonDisabled()`，v1 同样抽成纯函数便于单测）。
**不做**老版顶栏的「模式三档 segmented」「系统代理徽章」「回落徽章」「顶栏网速」（前三项的数据源 v1 没有；顶栏网速受老版设置项 `show_speed_in_title` 控制，而 v1 的 `SettingsView` 没有这个字段）。

---

## 4. 网速：契约只有字节，速率必须在 UI 派生 —— 以及一个必须裁决的结构性冲突

**契约事实**：`StatsView { uplink_bytes, downlink_bytes, sampled_at_ms }`（`model.rs:112-118`），`ConnectionView.stats` 为 `None` = **从未采样成功**。
**项目已有口径**（照抄，不另立一套）：`docs/design/TRUE-INFORMATION.md:63,235`、`docs/design/DESIGN-SYSTEM.md:251-252`、`docs/ux/INTERACTION.md:144` 规定：速率是**派生值** `Δbytes / Δsampled_at_ms`，**必须有两个 `sampled_at_ms` 不同的样本**才允许显示；只有一个样本、或 `sampled_at_ms` 相同 ⇒ **不显示速率**（不许编 `0 B/s`）。

**派生的落地规则（纯 UI 实现）**
- 只保留上一次样本 `(bytes, sampled_at_ms)`；`now_ms > prev_ms` 且 `now_bytes >= prev_bytes` 才出速率；
- 计数回退 / 时刻不前进 / 断开重连 ⇒ 清空基准，恢复「未采样」；
- 显示必须带采样时刻（「最近一次采样 12:03:41」），**不得**把一次采样说成实时速率；
- 格式沿用老版语义：`formatBytes`（1024 进制、B/KiB/MiB/GiB/TiB、非 B 保留 2 位）+ `/s`。

### ⚠️ 冲突：谁来产生第二个样本？

- 老版的速率来自**后端 1 秒定时器**（`apps/desktop/src/traffic.rs:30` `SAMPLE_INTERVAL = 1s`、`:58-98` 的 `sleep` 循环 + `previous` 差分）；
- v1 的 daemon **只在收到 `Request::Status` 时才采样**（`crates/xt-daemon/src/lib.rs:52` `STATS_GATE = 100ms`、`:287-315`），且 `Event::State` 只随状态**变化**发出（`:235`、`:340`）；
- `scripts/guard.sh:96-97` **明令 UI 不许 `setInterval` / `setTimeout` / `sleep`**（I1）；
- 裁决 5 又要求后端不改。
⇒ 在没有新节拍的前提下，**「老版那种持续跳动的网速」当前无法合规实现**。可选路径，必须由用户裁决：

| 方案 | 做法 | 代价 / 约束 |
| --- | --- | --- |
| **A（默认推荐，完全合规）** | 事件驱动：收到任意事件（state/log/probe/notice）时**顺带**取一次 `Status`，用「距上次采样 ≥ N ms」的时间戳判断节流（无定时器）；界面明确标注「最近一次采样 …」 | 网速**不连续**：没事件时数字不动；必须如实标注，不能装实时 |
| **B** | 用户显式点「测速」才采样一次 | 最保守，但「网速功能」退化成一次性读数 |
| **C（要用户点头）** | 放开 I1：允许 UI 用一个采样节拍（纯展示用），同步改 `scripts/guard.sh` 的扫描判据 | 动的是**仓库不变量判据**（guard 属 lead），不是后端；commit 里必须写清「为什么这次允许」 |
| **D（要用户点头，违背裁决 5）** | 后端加周期性 stats 事件（daemon 侧 tick，照老版 `traffic.rs` 的做法） | 违反「后端逻辑不要改」，不推荐 |

**本设计的默认选择是 A**；若要老版那种连续速率，请在 C 与 D 之间裁决（C 不动后端，代价是 I1 判据要开一个窄口子）。

---

## 5. 删掉：意图 / 拓扑

**v1 现状：零实现。**
- `intent`/`意图` 在 v1 有 60+ 处命中，**全部是「连接意图」语义**（`xt-state` 状态机；`xt-daemon/src/flow.rs:32` 的 `enum Intent { Connect(RunMode), Switch }`）——**不是**老版的「意图过滤页」；没有 `Intent.tsx`（`find -iname '*intent*'` 0 命中）。⚠️ 因此**不许**拿「搜索 intent」当删除清单：删掉 `flow.rs::Intent`/`xt-state` 会直接砸坏连接流程（I4 之下这属于后端，本版根本不该碰）。
- `topology`/`拓扑`/`globe`/`地理` 的命中全在文档与禁用词表里：`docs/product/NON-GOALS.md:39-41`、`docs/product/SCOPE.md:13`、`docs/ux/IA.md:142,166`、`apps/ui/src/App.tsx:11`（注释）、`apps/ui/tests/acceptance/truthfulness.test.tsx:518`（导航禁用项含 `'拓扑','地球','定位'`）、`scripts/guard.sh:114`（I3 禁用词含 `previewTopology`）。契约里**没有**任何连接列表/地理字段。
- 老版确实有（**这是「不许复制」的清单**）：UI 侧 `pages/Intent.tsx`、`pages/Topology.tsx`、`pages/Globe.tsx`、`pages/Routing.tsx`、`apps/ui/src/topology/**`、`previewTopology.ts`、`previewConnections.ts`、`landmask.ts`、`AuditSyncCard.tsx`、`auditSync.ts`、相关 `types.ts` 区段与测试；Rust 侧 `crates/xt-intent/**`、`apps/desktop/src/{intent,mitm,observe,audit_sync}.rs`、`commands/{intent,mitm,audit,topology,globe}.rs`、`crates/xt-core/src/{geo_lookup,store}.rs`、`xray/{access_log,routing/explain,routing/geo}.rs`。实现时以本清单为准逐项排除。

**本版动作**
1. 不新增这两页/导航项（导航**恰好 4 项**：仪表盘、节点、日志、设置）；
2. 从老版复制时**排除**上述清单（分流页同样没有契约字段支撑，一并排除）；
3. 把裁决变成机器判据：在 `apps/ui/tests/acceptance/` 加一条源码级断言 —— `apps/ui/src/**` 与导航集合里不出现 `Intent|Topology|Globe|意图过滤|拓扑|地球|定位`（现有 `truthfulness.test.tsx:518` 只钉导航项，本版扩到源码）；
4. 文档口径：`docs/product/NON-GOALS.md §1.4`、`docs/ux/IA.md:166` 的「不做」结论**继续有效**，实现时补一句「下一版仍不做，且不随仪表板复制引进」（由 product/ux 文件所有者改，不越权）。

---

## 6. 保留：日志 / 节点 / 订阅（本版**保持 v1 现状，不重做**）

**读法（一条假设，见 §8·4）**：裁决 3 说「保留」，与裁决 1 的「完全复制」相对 ⇒ 本设计理解为**保持 v1 现有实现不动**，不是照老版重建（老版这三页分别 391/633/281 行，功能面远大于 v1，重建属另一版工作量）。

| 功能 | v1 现状（证据） | 本版动作 |
| --- | --- | --- |
| 日志 | `apps/ui/src/pages/Logs.tsx`（45 行）：尾部 500 行事件流；级别有文字（颜色不是唯一信息源）；空态「暂无日志 / daemon 尚未推送日志行。」 | 不动。**已知缺口**：`store/daemon.ts` 引导链从不调 `tailLogs`，日志页无历史回填 —— 不修（要动协议口径，留待另开一版） |
| 节点 | `apps/ui/src/pages/Nodes.tsx`（175 行）：探测（`ProbeNodes`，能力门控）/「切换并立即使用」/「选为当前节点」；`probe == null` → 「未探测」，不自洽 → 「未知」，**从不显示 0 ms** | 不动；仪表板的「选择/切换节点」按钮跳这一页 |
| 订阅 | 在 `apps/ui/src/pages/Settings.tsx:167-246`：标题「订阅」；`subscriptions` 能力才渲染只读列表，`subscription_fetch` 能力才渲染「添加订阅/刷新」（v1.1.5 daemon **不宣告**后者 ⇒ 只剩只读列表）；空态「暂无订阅 / 订阅列表来自 daemon 的真实记录。」 | 不动，**不独立成页**（远端拉取需要后端能力，契约不改） |
| 开始面板 | 见 §3（v1 现在没有顶栏，本版补回） | 新增 |
| 网速 | 见 §4（v1 现在没有速率，只有累计字节） | 新增（UI 派生，后端不改） |

---

## 7. 验收与证据

**必须在本地/CI 跑（不用 cargo）**
```bash
cd xraytun-next/apps/ui && npm test && npx tsc --noEmit && npm run build
bash xraytun-next/scripts/guard.sh
git diff --name-only <base>..HEAD -- xraytun-next/crates xraytun-next/scripts   # 必须为空
```

**新增/扩展的断言清单（逐条）**
1. 顶栏存在且**唯一**一颗连接/断开按钮；仪表板里不再有第二颗（老版 task-72 的判据）。
2. 状态词与顶栏圆点同源：同一 `stage/phase` 下两处文本与 tone class 一致。
3. `stats == null` ⇒ 流量区显示「未采样」，且**不出现** `0 B/s` / `累计 0 B`。
4. 只有一个样本 ⇒ 不显示速率；两个样本（`sampled_at_ms` 不同）⇒ 速率 = `Δbytes/Δms`（固定 fixture 断言数值）。
5. 断开/重连后基准被清空（下一次采样不产生跨会话差分）。
6. §2.2 表中每一项都有「界面上不存在」的断言（核心路径 / helper 状态 / 更新 chip / 分流预设 / 直连档 / TUN 网卡名 / node_health / DNS / 登录项 / 「回落」字样）。
7. 导航恰为 4 项且第一项标签是「仪表盘」；源码与导航不含 `Intent|Topology|Globe|…`（§5.3）。
8. `Event::Notice` 被真实渲染（修复「缓冲了但不显示」的现状缺口），文案与 daemon 原文一致。
9. 能力门控不回退：`tun_mode` 未宣告 ⇒ 没有入站模式选择器；`probe` 未宣告 ⇒ 没有探测按钮；`stats` 未宣告 ⇒ 没有流量卡。

**只有用户真机能判的**：仪表板观感与老版的一致性（截图对比）；顶栏在真实 macOS 窗口里的布局。发布流水线已有的 app 启动探针目前只截两页（状态/设置），本版应扩到四页 + 顶栏可见性。

---

## 8. 非目标与开放问题

**非目标**
- 改后端与契约（裁决 5）：不新增命令/字段/事件/能力宣告；
- 意图、拓扑、地理/地球仪、分流规则页（裁决 2）；
- 更新检查、helper/TUN 新能力、订阅远端拉取、托盘/菜单栏面板；
- 重做日志/节点/订阅页（裁决 3 是「保留」，不是「重做」）；
- 动分支 `xraytun3` 与 `xraytun3/**`（裁决 4）。

**开放问题（各需一句裁决）**
1. **网速节拍**（§4）：选 A（事件驱动 + 标注采样时刻，默认）、C（放开 I1 加一个展示节拍 + 改 guard）还是 D（改后端）？
2. **版本号**：v1 线下一版叫 `1.2.0` 还是 `1.1.6`？（本版是 UI 重构 + 功能删除，建议 `1.2.0`；tag 与发布说明另定。）
3. **「开始面板」**如果指的是 **macOS 菜单栏/托盘面板**（而不是顶栏的连接控件），那需要壳层/后端能力，超出裁决 5 —— 请确认，我好改 §3。
4. **裁决 3「保留」的粒度**：保持 v1 现状（本设计的读法，§6）还是把老版的日志/节点/订阅页也照搬（那是 391+633+281 行的另一版工作量）？默认按前者。
