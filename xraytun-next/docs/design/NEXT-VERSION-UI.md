# xraytun-next（v1 线）下一版 · UI 设计：仪表板对齐老版 XrayTun

> **用户裁决（2026-10-01，逐字）**：
> 1. 仪表板，以及仪表盘的 UI 逻辑**完全复制** xraytun（= 老版 XrayTun v0.9.2）；
> 2. **删掉**意图、拓扑功能；
> 3. **保留**日志、节点、订阅、开始面板、网速功能；
> 4. **不要动 xraytun v3**（分支 `xraytun3` 与目录 `xraytun3/**`，那是第三个版本，设计文档保持空白）；
> 5. **后端逻辑不要改**。
>
> 一句话：**这一版 = UI 改版 + 一处后端打开**。仪表板用 v1 契约里**真实存在**的字段重建老版的形状与判据（没有字段支撑的老版元素一律不显示，不许造控件，I3）；同时按 §3.3（T2）把 daemon 的 TUN 通路打开 —— **`crates/xt-contract` 的类型一行不改**。

本文件位置：`xraytun-next/docs/design/NEXT-VERSION-UI.md`（xraytun-next 真源树；仓库快照同步到分支 `xraytun-next-v1`）。
老版只读参照：仓库里 `apps/**`（v0.9.2，main 线）——**只读**，且**不许**把其中的意图/拓扑页复制进来（§5）。

---

## 0. 范围与硬边界（机器可查）

| 项 | 值 |
| --- | --- |
| 目标版本 | **xraytun-next v1.2.0**（用户 2026-10-01 裁决）；当前基线 v1.1.5 = 分支 `xraytun-next-v1` |
| 可以改 | `xraytun-next/apps/ui/**`；`xraytun-next/scripts/guard.sh`（只容许 §4.1 那一处 I1 豁免）；**`crates/xt-daemon/**`（只为 §3.3 打开 TUN：去掉早退 + 宣告 `TunMode` + 改被它钉住的旧断言）**；必要时 `crates/xt-helperproto/**`、`crates/xt-helperd/**`、`crates/xt-macosnet/**`（物理上行发现：§3.3 缺口 1，已裁决 A）；相关 docs |
| 不许改 | `crates/xt-contract/**`（类型不动：`RunMode::Tun`、`Capability::TunMode` 本来就在）、`xt-ipc` 帧格式、协议与事件、**proxy 通路**（CI 里唯一的真数据面证据）、分支 `xraytun3` 与 `xraytun3/**` |
| 明令不动 | 分支 `xraytun3` 与 `xraytun3/**`（裁决 4） |
| 老版参照 | `apps/ui/src/**`（v0.9.2，只读；含 `pages/Dashboard.tsx` 等） |

**判据**

1. `git diff --name-only <base>..HEAD -- xraytun-next/crates/xt-contract xraytun-next/crates/xt-ipc` **必须为空**（冻结契约与帧格式不动）；`crates/**` 里**只允许 §3.3 清单**列出的改动（去早退 / 宣告 `TunMode` / 被它钉住的旧断言 / 必要时网卡名回传）。`scripts/guard.sh` 只容许 §4.1 那一处改动，且它必须打印豁免行。
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
| 4 | 快捷按钮：选择节点 / 切换节点 + 测试延迟 | `Dashboard.tsx:389-404` | 无节点时主按钮变「添加订阅」（本版落在**大启动按钮**上，快捷按钮不再重复）；测延迟按钮由 `probing` 门控 | `ListNodes` + `ProbeNodes` + 现有导航（`Request::ProbeNodes`，`protocol.rs:40`） |
| 5 | 直连模式提示 | `Dashboard.tsx:407-411` | 说明「直连模式核心不接管流量」 | v1 无 direct 档 → 不显示该提示（§2.2） |
| 6 | 流量区：↓ 速率 + 累计 ／ ↑ 速率 + 累计 | `Dashboard.tsx:415-426`；格式化 `apps/ui/src/types.ts:1047-1060` | 两个 metric：速率 + 「累计 X」 | `StatsView.uplink_bytes/downlink_bytes/sampled_at_ms`；**速率在 UI 里派生**（§4），**不照抄它的 0 缺陷**（§2.2） |
| 7 | 提示横幅：按 severity 排序，只显最急一条 + 「还有 N 条提示」 | `Dashboard.tsx:429-445` + `collectNotices()` `:536-680` | `rank` 越小越急；`error` → `role="alert"`，其余 `role="status"`；破坏性动作带二次确认 | 骨架照抄；条目按 v1 契约重列（§2.3） |
| 8 | 折叠的「环境自检与诊断」 | `Dashboard.tsx:448-521` | 核心 / helper / 进程 / 配置路径 + 4 个动作按钮 | 只留契约有的：`DatapathView.pid/version/ready_at_ms` + 动作（更新全部订阅 / 打开数据目录 / 查看日志 / 全部设置） |
| 9 | 「还没有状态」的兜底页 | `apps/ui/src/SnapshotState.tsx:24-47`，逐字 `正在读取状态…` / `读不到状态：{原因}` | 区分「还没读到」与「确实是空」 | `stage`/`connection` 为 `null` 时同样只显示「正在读取状态…」，**不许**显示成「未连接」 |
| 10 | 主操作按钮**全屏只有一处** | `apps/ui/src/App.tsx:595-611`；`runButtonDisabled()` `topbarStatus.ts:283` | 老版 task-72 特意把仪表板里那颗撤掉：同一动作同屏两颗只是重复 | 本版改成**仪表板顶部的大启动按钮**（§3.2）；顶栏只留状态，不再放同动作按钮 |
| 11 | 样式与配色 | 老版 `apps/ui/src/styles.css`（`dash__*` / `banner--{error,warn,info}` / `badge--{fast,ok,slow,unknown}` / `dot--*` / `topbar--*`） | 线、点、状态词三处同色 | 搬进 `xraytun-next/apps/ui/src/styles.css`（design 所有） |
| 12 | 导航第一项标签 | 老版：`仪表盘`（`App.tsx:43`） | — | v1 现为「状态」（`App.tsx:16`）；本版改为**「仪表盘」**，顺带消除 `docs/ux/IA.md:166` 与实现的漂移 |

### 2.2 绑不上契约的项（契约里没有对应字段 ⇒ **明确不显示**；以及老版自身的缺陷不许照抄）

| 老版字段 / 行为 | v1 契约 | 本版处理 |
| --- | --- | --- |
| `settings.routing_preset`（分流预设） | `SettingsView` 只有 `socks_listen`/`selected_node`/`log_level`（`model.rs:262-269`） | 不显示 |
| `settings.mode = direct`（直连档）+ 直连提示 | 没有 direct；只有 `RunMode::{Proxy,Tun}` + 断开 | **不显示**该档与那条提示；空态由 `stage=disconnected` 表达 |
| **代理模式（SOCKS 入站）** 与「入站模式」选择器 | 契约类型不动（`RunMode::{Proxy,Tun}` 本来就在，§3.3）；v1 的 `run-mode-select` 是它的 UI 面 | **整档移除**（用户 2026-10-01 裁决：放弃直连与代理，只保留 TUN）：UI 不渲染任何模式选择器，`Connect` 只发 `tun`（§3.2、§3.3） |
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
| 0 | `ConnectionView.last_error` 非空且当前未连接 | 由 `ErrorBox` 原样渲染（**含 code**），不是横幅 | 「查看日志」 |
| 1 | `Event::Notice`（`severity = error`） | daemon 给的原文，**不改写** | 按 `code` 给一个跳转或没有 |
| 2 | `Event::Notice`（`warning` / `info`） | daemon 给的原文 | 无 |

**「还没有节点」不单独出横幅**：那种情况下大启动按钮本身就是「添加订阅」，按钮下方还写着「节点目录为空 —— 启动前先添加订阅」；同屏再给一条同动作横幅，就是老版 task-72 收敛掉的那种重复。快捷按钮（选择 / 切换节点）也只做导航，不再复述这个动作。

规则照抄老版：rank 越小越急；只显示第一条，其余折成「还有 N 条提示」；`error` → `role="alert"`，其余 `role="status"`。**不许**把 notice 改写成「已自动处理 / 正在重试 / 已为你切换」（I2、I3）。
实现说明（2026-10-01）：`notices` 此前只被缓冲、没有任何页面渲染，本版接上了 —— 但**有界**：只取最近一条 `error` 与最近一条 `warning/info`（历史仍完整留在 store 里，什么都没丢）。
rank 0 **不走横幅**：仪表板已经用 `ErrorBox` 完整陈述了同一条失败（含 `code`），同屏两处说同一件事正是老版 task-23 D1 修掉的重复。

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
4. **启动/停止**：动作收敛到仪表板顶部的大启动按钮（§3.2）；顶栏不再放同一动作的按钮，避免同屏两个「启动/停止」；
5. 「为什么现在点不动」的 `title`/note：`requestPending`、`stage ∈ {connecting, disconnecting}`、`node_id == null`（「未选择节点」）各给一句真实原因；
6. ~~「入站模式」选择器~~ **已删**（裁决：只保留 TUN）：顶栏与仪表板都不再出现任何模式选择器，`Connect` 只发 `tun`（§3.3）。
7. **保留** v1 的传输层失败盒（`App.tsx:61-70`，带「重新连接」动作）——老版没有它，但它是 v1 唯一的传输失败出口，本版放在顶栏下方，不藏。

禁用判据只写一处（老版抽成 `topbarStatus.runButtonDisabled()`，v1 同样抽成纯函数便于单测）。
**不做**老版顶栏的「模式三档 segmented」「系统代理徽章」「回落徽章」「顶栏网速」（前三项的数据源 v1 没有；顶栏网速受老版设置项 `show_speed_in_title` 控制，而 v1 的 `SettingsView` 没有这个字段）。

### 3.1 菜单栏/托盘面板：两个都保留（用户 2026-10-01 裁决）

裁决：**顶栏连接面板与 macOS 菜单栏（托盘）面板两个都保留。**

- v1 的托盘**已经存在**：`xraytun-next/apps/desktop/src/tray.rs`（65 行），菜单项逐字「显示主窗口」「退出 XrayTun Next」；文件头注释写明**刻意不放连接/断开/切节点**（那些动作由 daemon 拥有）。
- 本版**保留托盘现状不动**：即它继续只提供「显示主窗口 / 退出」，**不新增**连接/速率/状态项。
- 若要把连接/状态/速率也放进托盘（老版托盘确实有——老版 `apps/desktop/src/traffic.rs:132-135,166-175` 用 `format_rate_compact` 更新托盘），那是 `apps/desktop/**` 的**壳层**工作：① 超出本版「只改 `apps/ui/**`」的边界；② 老版托盘速率的来源正是 §4 那个后端 1s 定时器，同样要先裁决节拍问题。⇒ 列为 §8 待裁决项，不在本版实施。

### 3.2 大的启动按钮（用户 2026-10-01：重新设计）

**位置**：仪表板最上方的开始面板区，居中、占主视觉。顶栏保留状态（线/圆点/节点/失败徽章），**不再放同一动作的第二颗按钮** —— 老版 task-72 的判据：同一动作同屏两颗只是重复。

**规格**
- 尺寸：宽 `min(520px, 100% - 48px)`、高 ≥ `96px`；主色/危险色随状态切换；**不做百分比进度条与假动画**（契约里没有进度数据 ⇒ 不许造）。
- 文案（逐字；状态取自 `ConnectionView.stage`）：

| 状态 | 按钮文字 | 可点 | 按钮下方说明（逐字） |
| --- | --- | --- | --- |
| `disconnected` + 无节点 | **添加订阅** | ✅（跳订阅区） | 节点目录为空 —— 启动前先添加订阅 |
| `disconnected` + 有节点 + 未选节点 | **启动** | ❌ | 未选择节点 |
| `disconnected` + 已选节点 | **启动** | ✅ | — |
| `connecting` | **启动中…**（附 `PHASE_TEXT` 阶段） | ❌ | 连接进行中：<阶段> |
| `connected` | **停止** | ✅ | 已连接 · <节点名> · 已运行 <时长> |
| `disconnecting` | **停止中…** | ❌ | 断开进行中 |
| 请求已受理、终态未到 | **启动中… / 停止中…** | ❌ | 请求已受理，等待事件返回终态 |
| `disconnected` + `last_error` 非空 | **启动** | ✅ | 上次运行出错：<原文> |
| `capabilities` 不含 `tun_mode` | **启动** | ❌ | 当前 daemon 不提供 TUN 模式（未宣告 `tun_mode` 能力） |

- 可访问性：`aria-busy`、状态变化经 `role="status"` live region 播报；`data-testid="start-button"`；Enter/Space 可触发；全屏只有这一处启动控件。
- **诚实门控**：`tun_mode` 未宣告时按钮禁用并说清真因 —— 这不是假控件（它说的是事实），也**不是**把按钮藏起来假装功能不存在。

### 3.3 只保留 TUN —— 已裁决走 T2：这一版就把 TUN 打开

**裁决（2026-10-01）**：放弃直连与代理（**产品面只留 TUN**）；**同时放开「后端逻辑不要改」**，这一版就让 daemon 打开 TUN 通路。

**好消息：TUN 的后端通路已经写完，只被一道闸拦着**
- `crates/xt-daemon/src/flow.rs:325` 已有完整的 `connect_tun_flow`：helper `TunUp` → `TakeTunFd`（SCM_RIGHTS 取 utun fd）→ 以 `XRAY_TUN_FD` 拉起 xray → 等就绪 → `CommitRoutes`；断开走 `tun_down`（`flow.rs:63-66`、`helper.rs:122-185`）。`flow.rs:327` 那段注释当时写着「当前**不可达**：`begin_connect_intent` 在真机验收（S6）通过前拒绝 tun 模式」——本版已连同入口一起改掉（现在写的是「已可达 + 真机接管仍未经 S6 验收」）。
- 拦住它的只有两处：`lib.rs:352-357` 的「当前只开放 proxy」早退；`lib.rs:323-328` 的能力表没宣告 `Capability::TunMode`。
- **契约类型不用改**：`RunMode::Tun` 与 `Capability::TunMode` 本来就在（`model.rs:50-57,303-318`）⇒ 不碰冻结项（I4 不受影响）。

**本版必须动的后端（清单，逐条给判据）**

| # | 文件:行 | 改什么 | 判据 |
| --- | --- | --- | --- |
| 1 | `crates/xt-daemon/src/lib.rs:352-357` | 去掉 `mode != RunMode::Proxy → unsupported` 的早退 | `Connect{mode: Tun}` 返回 `Accepted`，终态经 `Event::State` |
| 2 | `crates/xt-daemon/src/lib.rs:323-328` | 能力表加 `Capability::TunMode` | UI 的大启动按钮因此变为可点（capability 是唯一真源） |
| 3 | `crates/xt-daemon/tests/e2e_real_xray.rs:375` | 旧断言 `assert!(!hello.capabilities.contains(&TunMode))` 钉的就是「S6 前不宣告」，必须改 | 新断言：宣告 `TunMode`；**proxy 通路断言原样保留** |
| 4 | `apps/ui/tests/acceptance/truthfulness.test.tsx:456-475` | 「TUN 由 tun_mode 门控 → 出现开关」这条随模式选择器一起改 | 改为：界面无模式选择器；`tun_mode` 缺失时**大按钮禁用并说真因** |
| 5 | `apps/ui/tests/acceptance/live-daemon.test.ts:349,406` | 现在点的是 `connect-button`/`disconnect-button`（proxy）；改版后要点 `start-button`（tun） | 见「证据边界」 |
| 6 | `docs/architecture/00-CONTRACT-FREEZE.md`（§8、I5）、`docs/product/NON-GOALS.md`、`docs/product/SCOPE.md`、`docs/ux/IA.md` | 「本轮不做 TUN / helper」的结论要改成「本版开放 TUN」 | 由各文件所有者改（lead / product / ux），不越权 |

**动了后端以后，「宣告 ≠ 已验证」这条线要守住**
- 旧纪律（`lib.rs:355` 原文）：真机验收（S6）通过后才宣告 `TunMode`。T2 明确推翻它 ⇒ 这一版宣告的是「**本版本提供 TUN 通路**」（代码事实），**不是**「TUN 已经能接管流量」（仍未经真机验收，`docs/release-notes/v1.1.5.md` 也这么写着）。
- 所以界面与 release notes 必须保持这个区分：失败时**如实报错**（helper 没装 / socket 连不上 / 路由提交失败都给真原因），**不许**把「已宣告」写成「已可用」。真正「TUN 接管成功」的证据仍然只有 S6 真机。

**已知缺口（T2 会把它们变成用户可见的风险）**
1. ~~`TUN_PHYSICAL_IFACE = "en0"` 占位~~ **已按裁决 A 做掉（2026-10-01）**：
   helper 协议新增只读指令 `Request::DiscoverPhysical`（`xt-helperproto`，`HELPER_PROTOCOL_VERSION` 1→2），
   helperd 回 `Response::PhysicalUplink { interface, gateway }`（内部走
   `xt_macosnet::discover_physical()` → `route -n get default`）；daemon 的 `connect_tun_flow`
   把 `HelperClient::connect` 提前到**生成 xray 配置之前**，用发现的真实网卡名写
   `settings.autoOutboundsInterface` / direct 出站的 `sockopt.interface`，并删掉了后面重复的那次连接。
   **查不到默认路由就失败**（`fail_connect`），不退回到 `en0` —— 绑错网卡会表现为「连上了但出口不对」，
   比一条明确的失败更难查。协议版本升级是为了让「旧 helperd 不认识这条指令」被 `Status` 握先说成
   明确的版本不匹配，而不是一个「帧不合法」的解码错误。
2. helper 安装/回滚、utun 真正接管、DNS 与路由提交**仍然只有真机能验**；macOS CI（`ci.yml` @ `macos-14`）只覆盖构建 / lint / 单测 / UI，**不覆盖** TUN 接管。

**证据边界（改版后）**
- 真实数据面（真进程 / 真字节）继续由 `crates/xt-daemon/tests/e2e_real_xray.rs` 在 **proxy** 通路上覆盖 ⇒ **`RunMode::Proxy` 与 proxy 通路必须保留**（它是 CI 里唯一能跑的真数据面，`xt-cli` 也继续用它），只是**产品界面不再提供**。
- UI 侧 `live-daemon.test.ts` 只有在 TUN-capable 环境（macOS + 已装 helper + 相应权限）才可能连上；否则该用例降级为「渲染 + 失败路径如实」的断言，**并在 CI 输出里标明降级**（不许把 skip 说成通过）。

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
- 裁决 5 原本要求后端不改（T2 已为 TUN 打开，见 §4.1 补充）。
⇒ 在没有新节拍的前提下，**「老版那种持续跳动的网速」当前无法实现**。四个选项的**实际后果**（不是文字游戏，逐条说清）：

| 方案 | 做法 | 实际拿到什么 | 代价 / 约束 |
| --- | --- | --- | --- |
| **A** | 事件驱动：收到任意事件（state/log/probe/notice）时顺带取一次 `Status`，用「距上次采样 ≥ N ms」的时间戳节流（无定时器） | **只有事件来的时候数字才动**。而 v1 生成的 xray 配置把 access log 关掉了（`crates/xt-xrayconf/src/*.rs:200,440` 都是 `"access": ""`）⇒ **流量期间没有日志事件**；连接成功后通常一个事件都不来 ⇒ 速率基本是一条死线 | 完全合规，但「网速」名存实亡 |
| **B** | 用户显式点一次「测速」按钮刷新累计字节 | 一次点击 = 一个采样点；速率需要**两个时刻不同的样本**（`Δbytes/Δms`）⇒ B **只能显示累计字节 + 采样时刻，给不出速率** | 最保守，但裁决 3 的「网速」被实质砍掉 |
| **C（推荐）** | 放开 I1 的一个**窄口子**：允许 UI 里**一处**纯展示用的采样节拍（每 ~1s 调一次 `Status`），同步在 `scripts/guard.sh:96-97` 加白名单（只放行这一处，不关掉整条 I1） | **真正的持续速率**，与老版观感一致；**后端一行不改**（`Status` 是契约里已有的请求） | 动的是**仓库不变量判据**（`scripts/guard.sh` 属 lead，不是后端）；commit 必须写清「为什么这次允许」，例外只限这一处展示采样 |
| **D** | 后端加周期性 stats 事件（daemon 侧 tick，照老版 `apps/desktop/src/traffic.rs:30,58-98` 的做法） | 持续速率，且节拍归 daemon（与老版同构，架构最干净） | 原先违反裁决 5；**T2 之后不再违反**（§4.1 补充） |

**已裁决（2026-10-01）：选 C。** 它是唯一「不动后端、又能真正交付网速」的路径，代价是 I1 判据要开一个被记录在案的窄口子。

### 4.1 方案 C 的落地规则（已裁决）

- **唯一节拍点**：UI 里只允许**一处** `setInterval(..., ~1000ms)`（挂在 store 层，如 `store/daemon.ts` 或同层模块），它只做一件事：发 `Request::Status`。别处一律不许再有定时器。
- **只在连上时跳**：`stage !== connected` 时**不采样**，断开时清空基准 —— 既省请求，也避免拿跨会话样本算速率（§4 的规则）。
- **速率仍按 §4 派生**：只保留上一次样本；`sampled_at_ms` 不同、字节不回退才出速率；`stats == null` ⇒ 「未采样」；显示带采样时刻。
- **guard 的窄白名单（怎么写才不是空头承诺）**：`scripts/guard.sh` 的 I1 UI 扫描（`:96-97`）只放行这一处 —— 该行带显式标记（例如 `// GUARD-I1-EXEMPT: 展示用采样节拍，不参与同步（§4 C）`），guard 命中标记时**跳过该行并打印豁免行**。**不许**把整个文件或整个 UI 目录排除在扫描之外（那正是 `guard.sh:26-29` 记过的「空头承诺」教训）。
- **不许用别的 API 绕过正则**：`requestAnimationFrame` 之类同样能定期回调，但那只是躲 grep，不改变「这是一个节拍」的事实 —— 要走明面：标记 + 白名单 + 理由。
- **契约与 daemon 零改动**：`Status` 是契约里已有的请求（`protocol.rs:32`），采样仍由 daemon 侧 `sample_stats()` 完成（`xt-daemon/src/lib.rs:287-315`）。
- **代价必须进 release notes**：I1 原文是「不许轮询 / 固定延时**用来同步**」；本例外是**展示采样**，不参与任何状态推进。机器判据：guard 的豁免输出恰一行 + `crates/**` 的 diff 只落在 §3.3 清单里。
- **⚠️ T2 之后这条的前提变了（2026-10-01 补充）**：既然本版已经允许动 `xt-daemon`（§3.3），方案 **D（daemon 每秒推一次 stats）不再违背「后端不改」**，而且**比 C 干净** —— 不用给 I1 开白名单，也不会留下「UI 里有定时器」这个例外。C 是用户在原前提下选的；若愿意在本版顺手把节拍放进 daemon，建议改选 D（同一次后端改动做掉）。见 §8·2。

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

**读法（已裁决）**：裁决 3 说「保留」，与裁决 1 的「完全复制」相对；用户 2026-10-01 明确「**不需要额外的功能**」⇒ 本版**保持 v1 现有实现不动，不照老版重建**（老版这三页分别 391/633/281 行，功能面远大于 v1，重建属另一版工作量）。

| 功能 | v1 现状（证据） | 本版动作 |
| --- | --- | --- |
| 日志 | `apps/ui/src/pages/Logs.tsx`（45 行）：尾部 500 行事件流；每行带 `LogLevel` 文字（错误/警告/信息/调试，颜色只是补充）；空态「暂无日志 / daemon 尚未推送日志行。」 | **只保留两个功能**（用户裁决）：① **如实的流动** —— daemon 给的日志行按到达顺序实时渲染，不改写、不补写、不重排；② **分类显示** —— 每行显示 `LogLevel`（分类，如 error/warn/info/debug）。**不做**：搜索、关键字过滤、等级筛选、复制、清空日志、诊断报告、报告问题、历史回填。v1 现有页面恰好就是这个范围 ⇒ **本版不动它**；老版 391 行那套（搜索/筛选/清空/诊断/report）**不搬**。已知缺口 `TailLogs` 从不被调用（只有订阅后新到的行）—— 属「不做额外功能」范围，不修 |
| 节点 | `apps/ui/src/pages/Nodes.tsx`（175 行）：探测（`ProbeNodes`，能力门控）/「切换并立即使用」/「选为当前节点」；`probe == null` → 「未探测」，不自洽 → 「未知」，**从不显示 0 ms** | 不动；仪表板的「选择/切换节点」按钮跳这一页 |
| 订阅 | 在 `apps/ui/src/pages/Settings.tsx:167-246`：标题「订阅」；`subscriptions` 能力才渲染只读列表，`subscription_fetch` 能力才渲染「添加订阅/刷新」（v1.1.5 daemon **不宣告**后者 ⇒ 只剩只读列表）；空态「暂无订阅 / 订阅列表来自 daemon 的真实记录。」 | 不动，**不独立成页**（远端拉取需要后端能力，契约不改） |
| 开始面板 | 见 §3（v1 现在没有顶栏，本版补回） | 新增 |
| 网速 | 见 §4（v1 现在没有速率，只有累计字节） | 新增（UI 派生；若改选 D 则由 daemon 提供节拍） |

---

## 7. 验收与证据

**必须在本地/CI 跑（不用 cargo）**
```bash
cd xraytun-next/apps/ui && npm test && npx tsc --noEmit && npm run build
bash xraytun-next/scripts/guard.sh
git diff --name-only <base>..HEAD -- xraytun-next/crates xraytun-next/scripts   # 必须为空
```

**新增/扩展的断言清单（逐条）**
1. **大启动按钮**是仪表板顶部的主控，且全屏只有这一处启动/停止控件（顶栏不再有同动作按钮）。
2. 状态词与顶栏圆点同源：同一 `stage/phase` 下两处文本与 tone class 一致。
3. `stats == null` ⇒ 流量区显示「未采样」，且**不出现** `0 B/s` / `累计 0 B`。
4. 只有一个样本 ⇒ 不显示速率；两个样本（`sampled_at_ms` 不同）⇒ 速率 = `Δbytes/Δms`（固定 fixture 断言数值）。
5. 断开/重连后基准被清空（下一次采样不产生跨会话差分）。
6. §2.2 表中每一项都有「界面上不存在」的断言（核心路径 / helper 状态 / 更新 chip / 分流预设 / 直连档 / TUN 网卡名 / node_health / DNS / 登录项 / 「回落」字样）。
7. 导航恰为 4 项且第一项标签是「仪表盘」；源码与导航不含 `Intent|Topology|Globe|…`（§5.3）。
8. `Event::Notice` 被真实渲染（修复「缓冲了但不显示」的现状缺口），文案与 daemon 原文一致。
9. 能力门控不回退：`tun_mode` 未宣告 ⇒ 没有入站模式选择器；`probe` 未宣告 ⇒ 没有探测按钮；`stats` 未宣告 ⇒ 没有流量卡。
10. `scripts/guard.sh` 的 I1 豁免输出**恰好一行**（§4.1 那一处节拍）；`crates/xt-contract` 与 `crates/xt-ipc` 的 diff **为空**，`crates/**` 的改动只出现在 §3.3 清单列出的文件里。
11. 界面上**不存在任何模式选择器**（`run-mode-select` 消失），也不出现「代理模式 / 直连」字样。
12. `hello.capabilities` 不含 `tun_mode` ⇒ 大按钮**禁用**，说明逐字「当前 daemon 不提供 TUN 模式（未宣告 `tun_mode` 能力）」；且用假 client 断言 `Connect` 请求里的 `mode` **恒为 `tun`**。
13. TUN 通路（§3.3）：daemon 单测断言 `Connect{mode: Tun}` 被受理、能力表含 `tun_mode`；UI 侧大按钮可点。**真实接管仍归 S6 真机**，CI 不得声称验证过接管。

**只有用户真机能判的**：仪表板观感与老版的一致性（截图对比）；顶栏在真实 macOS 窗口里的布局。发布流水线已有的 app 启动探针目前只截两页（状态/设置），本版应扩到四页 + 顶栏可见性。

---

## 8. 非目标与开放问题

**非目标**
- 改后端与契约（裁决 5）：不新增命令/字段/事件/能力宣告；
- 意图、拓扑、地理/地球仪、分流规则页（裁决 2）；
- 更新检查、helper/TUN 新能力、订阅远端拉取、托盘/菜单栏面板；
- 重做日志/节点/订阅页（裁决 3 是「保留」，不是「重做」）；
- 动分支 `xraytun3` 与 `xraytun3/**`（裁决 4）。

**已裁决（2026-10-01）**
- **版本号 = `1.2.0`**（§0）。
- **开始面板 = 顶栏连接面板 + macOS 菜单栏托盘，两个都保留**（§3、§3.1）；托盘本版保持现状（只有「显示主窗口 / 退出」）。
- **保留的粒度 = 保持 v1 现状、不照老版重建**（「不需要额外的功能」）；**日志只保留两个功能**：如实流动 + 分类显示（§6）。
- **网速节拍 = 方案 C**（§4.1）：UI 里唯一一处展示用采样节拍 + `scripts/guard.sh` 的 I1 窄白名单。**注**：T2 之后可改选 D（§4.1 补充、§8·2）。
- **模式：放弃直连与代理，只保留 TUN**（§3.3）；仪表板顶部改为**一个大启动按钮**（§3.2），全屏只有这一处启动/停止控件。
- **TUN 路径 = T2**（§3.3）：放开「后端不改」，本版就去掉早退并宣告 `Capability::TunMode`；`RunMode::Proxy` 与 proxy 通路**保留**（CI 的真数据面证据 + `xt-cli` 用），只是产品界面不提供。

**仍待裁决**
（物理网卡名占位已在 2026-10-01 裁决为 **A** 并实现，见 §3.3 缺口 1；本条不再是待裁决项。）
2. **网速节拍是否从 C 改成 D**（§4.1 补充）：既然本版已动 `xt-daemon`，把每秒采样放进 daemon（老版同构）比在 UI 里开 I1 白名单更干净。默认维持 **C**（你已裁决），除非你想顺手换。
3. **托盘是否也要连接/状态/速率**（§3.1）：本版默认**不加**（托盘已存在，保持「显示主窗口 / 退出」）；要加属于 `apps/desktop/**` 壳层工作。

---

## 9. 实现状态（2026-10-01 落地记录）

| 文档里的东西 | 落在哪 | 证据 |
| --- | --- | --- |
| 仪表板按老版重建（状态区 / meta / 流量 / 横幅 / 诊断） | `apps/ui/src/pages/Dashboard.tsx`、`components/notices.ts` | `tests/acceptance/truthfulness.test.tsx`（18 条）+ `tests/acceptance/v12-dashboard.test.tsx`（6 条） |
| 大启动按钮 + 顶栏（无模式选择器） | `components/startButton.ts`、`components/Topbar.tsx`、`styles.css`（第 13 区块，+503 行） | `components/startButton.test.ts`（13 条）、`truthfulness.test.tsx` 的 `tun_mode` 门控用例 |
| 网速 UI 派生（方案 C） | `components/trafficRate.ts` + `store/daemon.ts` 里唯一的采样节拍 | `components/trafficRate.test.ts`（10 条）、`v12-dashboard.test.tsx` 的「两个样本才出速率 / 断开清基准」用例 |
| guard 的 I1 窄豁免 | `scripts/guard.sh` 的 `scan_code_exempt` + 行内标记 `GUARD-EXEMPT(I1-UI)` | `bash scripts/guard.sh` → `GUARD PASSED`，且**把豁免行打印出来**（恰一行） |
| TUN 通路打开（T2） | `crates/xt-daemon/src/lib.rs`（去早退 + 宣告 `TunMode`） | `crates/xt-daemon/tests/e2e_real_xray.rs`：能力断言 + 新增 §12.5「`Connect(tun)` 必须被受理」（CI 编译并跑） |
| 日志只保留两个功能 | **未改** `apps/ui/src/pages/Logs.tsx`（现状恰好就是目标范围） | §6 的读法 |
| 意图 / 拓扑仍然不做 | 没新增页面；导航恰 4 项 | `v12-dashboard.test.tsx` 的源码级断言（`src/` 下无 intent/topology/globe 文件）+ `truthfulness.test.tsx` 的导航禁令 |
| 物理上行发现（裁决 A） | `xt-helperproto`（新指令 + 版本 2）、`xt-helperd/dispatch.rs`、`xt-macosnet`（`discover_physical` 公开 API + imp/stub）、`xt-daemon/helper.rs`、`xt-daemon/flow.rs` | helperproto 的 roundtrip 测试（新变体）、Rust 侧由 CI 编译与测试 |

**本版没做到的（不许含糊）**
- TUN **真机接管**（helper 安装 / 回滚、utun、路由与 DNS）仍未经 S6 验收；本版能说的只有「通路已宣告 + 意图被受理」。
- 开发机不是 macOS：本机只跑了 UI（`vitest` / `tsc --noEmit` / `vite build`）与 `scripts/guard.sh`；Rust 侧由 CI（`xraytun-next-release.yml`，分支推送触发）编译验证。
- 物理上行发现按裁决 **A** 实现（helper `DiscoverPhysical` + 协议版本 1→2，daemon 不再写死 `en0`）；§8 仍待裁决的只剩「网速节拍 C→D」。
- `components/StatusBadge.tsx` 在新仪表板里不再被使用（状态词改由 `topbarStatus.ts` + 顶栏/仪表板渲染）；本版**没有删它**，留待下一次清理时一并处理（删它要同时清 `styles.css` 的 `badge--disconnected/connecting/…` 与 `docs/design/DESIGN-SYSTEM.md` 的对应段落）。

### 9.1 第 3 轮落地记录（2026-10-16）：订阅这条路 + 真人走查发现的三处「不告诉你去哪」

这一轮不新增设计，只把**已经写在文档里但用户走不到**的东西接通，并把两处「看得见但没说下一步」的界面补上。

| 东西 | 落在哪 | 证据（本机可验 / 只有 CI 能验） |
| --- | --- | --- |
| 订阅远端拉取（设计 §6 一直记着「保持 v1 现状」，本轮第一次真的做了） | `crates/xt-daemon/src/subs.rs`（`fetch_body` = **`/usr/bin/curl`** 绝对路径 + argv；校验、脱敏、0600 状态文件）、`lib.rs` 的 `add_subscription` / `refresh_subscription` / `record_subscription_error`、宣告 `Capability::SubscriptionFetch` | Rust：CI run `38885737929`（`be0beb6`）四个 job 全绿，含 `clippy -D warnings` 与 `cargo test --workspace` |
| 订阅的端到端用例（真 HTTP + 真 curl，零新增依赖） | `crates/xt-daemon/tests/e2e_real_xray.rs`：`start_subscription_server`（`std::net::TcpListener`）、`ControlHarness`（只跑控制面，不需要真 xray）、`remote_subscription_add_list_and_refresh`、`remote_subscription_failure_never_leaks_the_url` | 同上：CI 编译并运行 |
| 设置页订阅区（添加 / 刷新，仍由能力门控） | `apps/ui/src/pages/Settings.tsx`（`subscription_fetch` 才渲染；错误提示**渲染在订阅卡片内**） | `apps/ui/tests/acceptance/subscription.test.tsx`（4 条，本机 `vitest` 通过） |
| CLI 等价入口（无头也能做同样的事） | `crates/xt-cli/src/lib.rs`：`add-subscription` / `refresh-subscription` / 新增 `import-nodes <原文｜->` | Rust：CI；本地只核对了 shape 与 `guard.sh` |
| 真人走查发现 ①：**有节点但没选节点**时，大按钮灰着只写「未选择节点」，真人不知道该去哪点 | `components/startButton.ts`（第 8 行分支改成可点的**「选择节点」**，说明逐字给出「在节点页点某一行的『选为当前节点』」）+ `Dashboard.tsx` 的 `select` 动作导航 | `components/startButton.test.ts`、`tests/acceptance/node-import.test.tsx`（新增「选了节点之后才是『启动』」对照） |
| 真人走查发现 ②：添加订阅失败的错误提示在**页面顶部**，订阅卡片在页面最下面，用户看不到 | `pages/Settings.tsx`：错误按来源分流，订阅动作的错误渲染在订阅卡片内（同一 testid，任何时刻只有一处） | `tests/acceptance/subscription.test.tsx`（第 4 条） |
| 真人走查发现 ③：节点页没说「手里只有订阅链接怎么办」 | `pages/Nodes.tsx`：`subscription_fetch` 已宣告时，导入区明说「到『设置 → 订阅』填 URL」 | 本机 `tsc --noEmit` + 全量 `vitest`（92 passed / 1 skipped） |

**口径**：本轮把「订阅」从「不做」变成「做完了但只在本机验过 UI 那半」，Rust 半边的编译与端到端**以 CI 为唯一判据**；
真机（helper 授权、`utun` 接管）仍然只有用户能验 —— 所以「能翻墙」这句话依然不说。

### 9.2 第 4 轮落地记录（2026-10-16）：一次独立体验官走查之后的修补

第 3 轮发布后请了一个独立 fresh-context agent 当体验官（只读源码与发布说明）。它给的 10 条与逐条处置
记在 `docs/ux/USER-JOURNEY-REAL-PERSON.md` §7。落到**代码/界面**上的有六处，按危害排序：

| 改了什么 | 落在哪 | 判据 |
| --- | --- | --- |
| **修掉「连着 TUN 时切节点会静默降级成 SOCKS」**：旧路径先跑代理流程（`tun_fd: None`），状态机却沿用 `mode: Tun`，等一个永远不会来的 `RoutesCommitted` —— 界面卡在「提交路由中」而实际是本机 SOCKS，旧 utun/默认路由还可能挂着 | `crates/xt-daemon/src/flow.rs`（`switch_needs_tun_flow` + `connect_flow` 的 TUN 分支：先 `disconnect_flow()` 再 `connect_tun_flow(新节点)`） | 纯函数单测（三个 TUN 分支为真、Proxy/Disconnected 为假） |
| **连不上 daemon 时给出「为什么」+ 日志路径**：壳的 `LaunchReport` 以前只进 `eprintln` | `apps/desktop/src/daemon_launch.rs`（`LaunchReport` / `Launch::report`）、`lib.rs`（状态 + `daemon_launch_report` 命令 + 注入标记）、`apps/ui/src/components/launchReport.ts`、`App.tsx`（只在 `code === 'io'` 时问一次） | `src/components/launchReport.test.ts`（8 条）、`tests/acceptance/transport-diagnosis.test.tsx`（4 条：显示 / 无标记不发命令 / 壳没报告就不显示 / 非 io 不显示） |
| **失败时知道一手现场在哪**：仪表板失败下方显示 daemon 日志路径 | `apps/ui/src/components/DaemonLogHint.tsx` + `Dashboard.tsx` | `tests/acceptance/error-next-step.test.tsx` |
| **订阅错误更像人话**：4xx/5xx 由 `--fail` 归因到 HTTP；curl 6/7/28 附「推断：先确认网络」+ 脱敏 host | `crates/xt-daemon/src/subs.rs`（`--fail`、`network_hint`） | `network_hint` 单测（含「22 不许说成网络问题」） |
| **卸载范围与脚本一致**：逐字列出三样 + 留下的状态目录 | `apps/ui/src/pages/Settings.tsx`（`helper-uninstall-scope`） | 验收用例断言四段路径逐字出现 |
| **首次打开指引进 DMG**（未公证是第 1 个卡死点，而那时 App 还没跑起来） | `docs/install/FIRST-LAUNCH.txt` + 发布工作流的 DMG 步骤 + README + v1.2.2 说明 | 文档 + 工作流 diff；**没有**代码判据（Gatekeeper 只能真机验） |

### 9.3 第 5 轮落地记录（2026-10-16）：产品面只做 TUN（用户裁决）

用户裁决：「放弃掉 sockets 代理，只做 tun 代理。」这一版把**每一个出口**对齐到这句话上，
因为上一个会说假话的 bug（连着 TUN 切节点却掉回 SOCKS）正是「界面说 TUN、底下跑代理」长出来的。

| 出口 | 改法 | 判据 |
| --- | --- | --- |
| 设置页 | 「代理入口」卡 → 「本机 API」（`api-listen-card`）；栏位从「SOCKS 监听地址」→「本机 API 监听地址」；说明逐字写明「本版本只做 TUN、daemon 不再提供 SOCKS 入站」 | `tests/acceptance/tun-only.test.tsx`（整段文本相等，故那句正当的否定句不会被误判） |
| 设置页能力行 | **保持线上原文**（不许美化），旁边加一句「`proxy_mode` 是给 CI 端到端验证留的通路，本产品的连接模式只有 TUN」 | 同上（第 2 条）；已有用例继续钉住原文 |
| CLI | `xt-cli connect` 从 `proxy` 改成 `tun`（`CONNECT_MODE`） | `connect_uses_tun_because_the_product_is_tun_only` 单测 |
| 契约 | `RunMode::{Proxy,Tun}` 与 `Capability::ProxyMode` 的文档就地写明「Proxy 不是产品入口」 | 注释即口径，防下一个人再开产品入口 |
| 文档 | README 口径、DMG 内说明补一句；ARCHITECTURE / TRUE-INFORMATION / IA 的就地更新见各自文件 | — |

**还有一处本机 SOCKS 端口（不是连接模式）**：「测试延迟」用 `xt_xrayconf::generate_probe` 给每个节点
起一个 `listen: 127.0.0.1` 的环回 SOCKS 端口来量真实 TTFB。它是**测量路径**，本轮没有改；
要改就得换一种量真实 TTFB 的办法（TUN 探测需要 root/助手）。

**为什么 daemon 里的 SOCKS 数据面不删**（这是这一轮唯一「不删」的地方，必须写清）：CI 上唯一能证明
「真有字节穿过隧道」的端到端用例走的就是它（`crates/xt-daemon/tests/e2e_real_xray.rs`），TUN 需要
root + 特权助手，CI 跑不了；契约又要求「未宣告的能力必须回 `Unsupported`」，所以删掉数据面等于
连那条证据一起删。用户选择了「产品面 TUN-only + 内部保留验证通路」，口径写进了
`crates/xt-contract/src/model.rs` 与 `crates/xt-daemon/src/flow.rs` 的注释。
