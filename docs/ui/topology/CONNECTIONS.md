# 单连接可视化：日志连接 × 拓扑

> 用户原话：「能够设计一种方式，可以看到单个连接，可视化吗。比如日志的连接。和拓扑相结合到一起。」
>
> 本文是**设计说明**，先于实现。实现落点：`apps/ui/src/pages/Topology.tsx`、
> `apps/ui/src/styles.css`、`apps/ui/src/preview.ts`、`apps/ui/src/ipc.ts`；
> 解析与数据面在 `crates/xt-core/src/xray/access_log.rs`、`apps/desktop/src/state.rs`、
> `apps/desktop/src/commands/topology.rs`（backend-dev）。

---

## 0. 一句话设计

拓扑页现在是**聚合视图**（车数按累计字节）。本设计加的第二层是
**个体视图**：从「最近连接」列表里选中一条，拓扑上**只亮它经过的那条路**
（入口卡片 → 出站卡片），其余压暗；详情面板只显示日志里**真的**有的字段。

```
┌─ 网络流动 ──────────────────────────────────────────────┐
│  [聚合] 车流 / 连线 / 卡片            ← 已存在，不动      │
│  [单连接] 选中时：这条路上亮，其它压暗   ← 本次新增（叠加层）│
└────────────────────────────────────────────────────────┘
┌─ 最近连接 ──────────────────────────────────────────────┐
│  过滤: [入站 ▾] [出站 ▾] [域名/目标 ……………]              │
│  ┌───────────────────────────────────────────────────┐  │
│  │ 13:30:58.560  www.google.com*       tun → node…   │  │
│  │ 13:30:58.559  142.250.72.14:443     tun → direct  │  │
│  │ …（只渲染最近 100 条；超出部分明确写出来）           │  │
│  └───────────────────────────────────────────────────┘  │
│  选中详情：时间 / 入站→出站 / 目标 / 协议 / 域名 / 来源   │
│  并**明确写出**：域名是时序配对（可能不准）；没有字节数、  │
│  没有持续时间。（见 §3）                                  │
└────────────────────────────────────────────────────────┘
```

---

## 1. 数据流

```
核心 access.log
   │  逐行（已有 tail 通路）
   ▼
crates/xt-core/src/xray/access_log.rs
   │  解析两类行：
   │    accepted 行 → ConnectionRecord（建立时间/来源/目标/入站/出站）
   │    sniffed 行 → 待配对项（一条 sniffed 最多配一条 accepted）
   ▼
AppState 环形缓冲（≤1000 条，最旧被挤掉 → 用 dropped 累计数如实报告）
   │
   ▼
Tauri 命令 recent_connections(outbound?, inbound?, domain?, limit?)
   →  { items, dropped, pairing }
   │  （前端与拓扑**共用同一个 2s interval** 轮询 —— 见 §6）
   ▼
Topology.tsx「最近连接」区块
   │  选中一条 → { inbound_tag, outbound_tag }
   ▼
Topology 图：按 tag 匹配入口卡片 / 出口卡片 → 高亮叠加层
```

**为什么是轮询而不是逐条事件**：连接可达每秒数十条，逐条 `emit` 会让 React
每秒重渲染几十次；而人眼读列表的节奏是秒级。轮询最近 N 条既够用又稳定。
（真实日志实测：**14 秒 100+ 条**。）

---

## 2. 能拿到的 / 拿不到的（硬约束）

| 字段 | 来源 | 可靠度 |
|---|---|---|
| 建立时间 | `accepted` 行；`ts_text` 是日志原样墙钟（微秒精度），`ts_ms` 是本进程收到该行的 Unix 毫秒 | **高** |
| 来源 `from` | `from tcp:198.18.0.1:49712`；DoH 形态是 `DNS` | **高** |
| 协议 `network` | `tcp` / `udp` / `https`（DoH） | **高** |
| 目标 | `target_host`（**多数是 IP，但日志里也会直接给域名**，如 `github.com`）+ `target_port`（日志没给就是 `null`，不编 443） | **高** |
| 入站 → 出站 `[tun -> node-n1d2…]` | `accepted` 行 | **高**（与拓扑结合的点） |
| 域名 | **另一行** `sniffed domain: …`，时序配对；`domain_pair_delta_us` 是配对时延 | **中 —— 可能不准**（见 §3） |

### ⚠️ 拿不到（不得显示、不得推算）

| 拿不到的东西 | 为什么 |
|---|---|
| **每条连接的字节数** | Xray 的 `StatsService` 只有聚合计数器，**没有任何 per-connection 流量**。所以本视图**不显示**「这条连接传了多少」。 |
| **连接的结束时间 / 持续时间** | 日志只记建立（`accepted`），不记结束。 |
| **把多行聚成同一条连接** | `accepted` 行**不带连接 ID**（ID 只在 `sniffed`/`proxy` 行上）。 |
| **「活跃连接数」** | 列表里消失 ≠ 连接关闭（可能是被环形缓冲挤掉的）。 |

> 底线（本项目反复强调）：**宁可少显示，不要编。** 界面上这四项一个字都不出现；
> 详情面板用一行灰字写明「本视图没有字节数与持续时间，因为日志里没有」。

---

## 3. 域名配对算法（近似，必须标注）

真实日志里两行**相隔极短**，`sniffed` 先、`accepted` 紧接：

```
2026/09/20 13:30:58.560290 [Info] [3163266252] app/dispatcher: sniffed domain: www.google.com
2026/09/20 13:30:58.560364 from tcp:198.18.0.1:49712 accepted tcp:194.221.250.50:443 [tun -> node-n1d232c6b8c7a5004]
```

**策略**：为每条 `accepted` 找**时间上最近的前一条** `sniffed`（窗口 ≤ 200ms），
配对即消费（一条 `sniffed` 最多配一条 `accepted`）。配对时延用**微秒**记录。

**实测配对时延**（定稿快照）：min=0µs、**p50=31µs**、p95=301µs、max=116728µs。
用毫秒会四舍五入成 0，看不出「配得有多近」，所以字段是 `domain_pair_delta_us`。

**定稿快照（可复现）**：`/tmp/access-snapshot.jsonl`，
sha256 `525961ef5c6fabe0d6ef73b1a8e6e05a4fc43248eba18a52da913742005063a0`，
15,462,726 字节 / 77,883 行。

**边界（backend-dev 单测）：**
无前序 sniffed → `domain=null`；多条 sniffed 竞争 → 取最近且不复用；
乱序/超 200ms → 拒配并计数（`rejected_stale`）；畸形行 → 丢弃并计数，绝不产出半条记录。

### 配对率的真实分布（**定稿**：8455 条 accepted，后端实现加固后）

| 出站 | 配到域名的比例 |
|---|---|
| `node-n1d232c6b8c7a5004` | 3787 / 4576 = **82.8%** |
| `direct` | 293 / 366 = **80.1%** |
| `api`（内部回环） | 0 / 2523 = **0.0%**（后端已**强制**不参与配对：它永远没有域名，还会抢走真实连接的 sniffed） |
| `dns-out`（UDP） | 1 / 990 = **0.1%** |
| **总体** | 4081 / 8455 = **48.3%** |

其它计数：`unpaired = 4374`、`sniffed = 4590`、超窗口拒配 = 24、被覆盖 = 485、
环形缓冲 `dropped = 7455`（容量 1000）。

**总体不到一半，不是算法漏了**：`api` 回环与 `dns-out` 的 IP 直连
**根本没有 sniffed 行**。所以界面对 `domain === null` 的处理是**正常态**，
显示目标 host，而不是错误态、更不是空字符串。

**一条已知误配（保留，不特判）**：`dns-out` 那 1 条是真实的时序配对误差 ——

```
13:40:42.092776 [Info] [32516189] app/dispatcher: sniffed domain: accounts.google.com
13:40:42.104256 from udp:198.18.0.1:36083 accepted udp:198.18.0.2:53 [tun -> dns-out]
```

相隔 11.5ms，落在 200ms 窗口内，于是被配上。占全部配对记录的 **1/4081 ≈ 0.02%**。
后端**没有**为它加 tag 特例（不想把 `dns-out` 这种名字硬编码进解析层）。
前端也**不加启发式提示**：详情面板已经显示**精确的配对时延（µs）**，
由人判断；用「Δ 偏大 + 目标是 IP」去猜会引入新的误报，而这一页的底线是不编。

**前端如实标注**（已实现，不是可选项）：
配对来的域名带 `*`，详情里写「域名是时序配对的结果，只能按时间就近配对（这条相差 Nµs），并发时可能对不上」，
并在摘要与详情里显示后端给的 `pairing` 统计（不自己算、不估）。

---

## 4. 与拓扑的耦合

连接里的 `[入站 -> 出站]` 与拓扑里的入口/出口是**同一个概念**，高亮就是拿这对 tag 去找卡片：

| 情况 | 行为 |
|---|---|
| `inbound_tag` 命中入口列 | 该卡片加 `--match`；其余入口/出口卡片压暗 |
| `inbound_tag` 是内部入站（`api`） | **不静默**：「这条连接走的是内部通道（api → api），不在流向图里」 |
| `outbound_tag` 命中出口列 | 该卡片加 `--match`，并画出「入口 → 分叉 → 该出口」的**高亮弧线** |
| `outbound_tag` 是内部通道（`dns-out`/`api`，见 `INTERNAL_KINDS`） | 卡片高亮但**不画线**（内部通道的字节计数器是测量盲区，已在出口列单独分组），并给出内部通道文案 |
| 两边都匹配不到（配置刚换） | 明确写「这条连接的出站不在当前拓扑里」，不画错线 |

**高亮是叠加层**：新增一条 `<path class="flow__highlight">`（主干 + 该出口那一段）
+ 给卡片加 class，**不修改 `Flow` 的车位置/进度逻辑**（那是 task-2 的成果，回归测试锁着）。

**压暗用 CSS class**（`highway--focused`），不在 JS 里逐个元素改 style ——
少一次 DOM 写、也便于将来加过渡。

---

## 5. 界面草图（文字版）

```
网络流动（原有区块，不动）
  ├─ 车流图
  └─ 选中连接时：入口卡片[选中] ──高亮虚线弧──> 出口卡片[选中]，其余压暗

最近连接（新增区块）
  ├─ 标题/说明：每条连接 = 一行 accepted；域名带 * 的是时序配对
  ├─ 过滤行：入站 [全部 ▾]  出站 [全部 ▾]  域名/目标 [____]  [清除过滤] [取消高亮]
  ├─ **过滤范围提示**（必须显示）：过滤范围 = 已取到的**最近 N 条**（不是全量搜索）。
  │          仅当 `dropped > 0` 时追加「；更早的连接已被环形缓冲挤掉，累计 D 条」——
  │          一条都没挤掉过时不提「挤掉」，否则就是给不存在的事加一句事实陈述
  ├─ 摘要行：最近 N 条连接 · 其中匹配 M 条 · 只显示最近 100 条（另有 K 条已隐藏）
  │          · 配到域名 P / A 条（X%）
  ├─ 列表（max-height 260px 可滚动，最多渲染 100 行）
  │   每行：时间(HH:MM:SS.mmm) | 域名*（或 目标host:port） | 入站 → 出站
  └─ 详情（选中后才显示）
       时间 / 入站 → 出站 / 目标 / 协议 / 域名 / 来源
       * 域名是时序配对的结果（这条相差 Nµs），并发时可能对不上
       灰字：本视图没有这条连接的字节数与持续时间（原因）
       已观察 A 条 · 配到域名 P 条（X%） · 拒配（超时/乱序）R 次
```

空态与异常态（都有文案，不留白）：

* 命令失败/核心没跑 → 「取不到最近连接：<原因>（访问日志由核心写入，核心没跑时没有新行可读）」
* 过滤后 0 条 → 「没有匹配的连接（共 N 条）。」
* 接口正常但没有记录 → 「还没有连接记录（核心刚启动时正常）。」

---

## 6. 性能

| 决策 | 理由 |
|---|---|
| 后端环形缓冲 ≤ 1000 条 | 内存有界；`dropped` 如实告诉用户挤掉了多少 |
| 前端只渲染最近 100 行（`CONNECTION_ROW_LIMIT`） | 1000 行 DOM 会让滚动卡顿；用「只显示最近 N 条 + 已隐藏 M 条」而不是偷偷截断 |
| **与拓扑共用一个 2s `setInterval`** | 连接与拓扑同一节奏取，渲染次数有上界。**刻意不注册第二个 interval**：本页回归测试用 `window.setInterval` 桩只保留最后一个回调，多一个会把拓扑刷新挤掉（实测导致 4 条几何/数字测试变红） |
| `React.memo(Highway)` | 连接列表刷新不该让车流图重渲染（动画虽在 effect 里，reconcile 仍要跑） |
| 输入过滤在**本地**做，且**UI 明确写出范围** | 每敲一个字都往返后端太浪费。但本地过滤只覆盖**已取到的窗口**（默认 200 条）：不说清楚会让人以为「搜遍了所有连接」——那是「把局部当全部」的不诚实。所以界面固定显示「过滤范围：已取到的最近 N 条（不是全量搜索）」，空结果也写「在已取到的最近 N 条里没有匹配的连接」。需要全量搜索时再接后端的 `domain`/`inbound`/`outbound` 参数 |
| 高亮用叠加层 + CSS class | 不触碰 `Flow` 的动画循环 |
| 列表行 `key` 稳定 | `ts_ms|from|target_host:port|in|out`，避免 React 复用错行 |

---

## 7. 接口契约（**已定稿，以 `types.ts` 为准**）

```ts
export interface ConnectionRecord {
  ts_ms: number;                        // 本进程收到该行的 Unix ms
  ts_text: string;                      // 日志原样墙钟 "2026/09/20 13:30:58.560364"
  from: string;                         // "198.18.0.1:49712" / DoH 是 "DNS"
  network: string;                      // "tcp" | "udp" | "https"
  target_host: string;                  // ⚠️ 不是 target_ip：日志里也会直接给域名
  target_port: number | null;           // 日志没给就 null（不猜 443）
  inbound_tag: string;
  outbound_tag: string;
  domain: string | null;                // 时序配对；约一半连接本来就没有
  domain_paired: boolean;               // 恒等于 domain !== null
  domain_pair_delta_us: number | null;  // 微秒（p50=26µs，用毫秒会变 0）
  sniff_id: string | null;
}

export interface PairingStats {
  accepted: number; paired: number; unpaired: number;
  sniffed: number; rejected_stale: number; sniffed_superseded: number;
}

export interface RecentConnections {
  items: ConnectionRecord[];            // 最新在前
  dropped: number;                      // 环形缓冲累计挤掉条数
  pairing: PairingStats;                // 界面如实标注配对率用
}

// 命令（参数都可省）：recent_connections(outbound?, inbound?, domain?, limit?)
// limit 默认 200、上限 1000；domain 是不区分大小写子串匹配。
```

前端取数走 `ipc.ts`（该文件规定「invoke 只能在 ipc.ts」）：`api.recentConnections()`；
`preview.ts` 用 `?preview=1&connections=…` 提供 mock。

---

## 8. 测试与验证（实际交付）

**纯函数单测** —— `apps/ui/src/connections.test.ts`（15 条）：
`matchConnectionToTopology`（普通命中 / 内部入站 / 内部出站 / 两边都缺 / 入口命中但出站内部）、
`filterConnections`（空条件零拷贝、入站、出站、域名大小写、目标 host、null 域名、多条件与关系）、
`connectionKey`（稳定、四个字段任一不同即不同）、`CONNECTION_ROW_LIMIT` 有界。

**为什么高亮几何不放进 jsdom 测**：jsdom 没有布局引擎，`getBoundingClientRect` 全 0，
SVG 路径几何根本量不出来 —— 硬测只会测出假绿。所以高亮用**真浏览器 CDP** 验（下）。

**真浏览器实测**（vite 预览 + CDP，`?preview=1&connections=…`）：

| 场景 | 结果 |
|---|---|
| 默认 42 条 | 渲染 42 行；未选中时无 `.flow__highlight` |
| 点第 1 条（`socks → block`） | `.flow__highlight` 出现（`stroke=rgb(207,230,255)`）、`.highway--focused` 生效、命中的卡片恰好 2 张（socks、block）、详情显示「已在车流图上高亮」 |
| 点内部通道（`api → api`） | **无高亮线**、命中卡片 1 张（api）、文案「这条连接走的是内部通道（api → api），不在流向图里」 |
| 过滤 `google` | 42 → **6** 行 |
| 过滤不存在的串 | **0** 行 + 「没有匹配的连接（共 42 条）。」 |
| 清空过滤 | 回到 42 行 |
| `?connections=busy`（600 条） | 只渲染 **100** 行，摘要「另有 500 条已隐藏 · 缓冲已挤掉 137 条 · 配到域名 381 / 737（52%）」；高亮仍正常 |

截图：`09-connection-highlight.png`（选中一条连接后的压暗 + 高亮弧线）、
`10-connections-busy.png`（600 条密集场景）。

**回归**：`npx tsc --noEmit` 干净；`npx vitest run` → **51 passed | 1 todo**，
其中 `topologyAnimation.test.ts` 16 条（几何/数字回归）全绿 —— 高亮是叠加层，
没有动动画核心。

### 验证手段的边界（**不止影响本页**）

下面这条必须写在这里，而不是只写在「未验证清单」里 —— 写在清单里会让人以为
「补一次真机验证就行」，而事实是它在当前平台上**没有成熟手段**。

* **本轮所有 CDP 证据都来自 `?preview=1` 的 vite 浏览器预览，没有一条来自真实应用**：
  探针口径（`off_line_frac`）、动画连续性、单连接高亮、`?connections=busy` 压测，全部如此。
  它们证明的是**前端在真实浏览器里的行为**，不是「真实 Tauri 应用里的行为」。
* **为什么挂不上**：Tauri 2 在 **macOS 用 WKWebView**（不是 Chromium），
  没有 Chrome DevTools 协议 —— CDP 类脚本对真实应用**不可用**。
* **唯一能看真实应用 UI 的路**是 WebKit 的 **Remote Inspector**
  （Safari 开发菜单 / `__TAURI_WEBVIEW_INSPECTOR__` 之类）：
  需要 GUI 会话 + 手工操作，**不可自动化**，所以**不会成为 CI 判据**。
* **这是平台相关的，不是恒定的**：Windows/Linux 上 Tauri 用 **WebView2（Chromium）/
  WebKitGTK**，那时 CDP 类自动化**可能**可行。所以结论应读作
  「**macOS 上**真机 UI 不可自动化验证」。
* **缺的那一层被什么覆盖**：① 真实快照重放（解析 + 配对，8455 条 accepted）；
  ② `apps/desktop/tests/type_contract.rs`（IPC 序列化形状）；
  ③ 预览浏览器（渲染 + 高亮）。三者并集之外只剩 **Tauri State 装配**，
  那是「一行注册 + 编译通过」。
* **因此**：看到「CDP 验证通过」时必须知道它的上限 —— 它**不等于**「真机验证通过」。

---

## 9. 诚实清单（本设计**做不到**的）

1. **不知道每条连接传了多少字节**，也不知道它持续多久 —— 日志里没有，
   `StatsService` 也没有 per-connection 计数。所以「单连接的流量」本设计**不回答**；
   它回答的是「这个连接从哪进、到哪出、目标是谁」。
2. **域名可能配错**：按时序近似，并发时（同一毫秒多条 sniffed）无法区分。
   界面标注 `*` 并显示后端的配对率与配准时延。
3. **没有连接的「结束」**：列表里消失 ≠ 连接关闭（可能是被环形缓冲挤掉的）。
4. **`accepted` 行没有连接 ID**：同一连接的多行无法聚组；本设计不做「会话」概念。
5. **过滤只覆盖已取到的窗口**（默认 200 条 / 2s 刷新）：界面上已固定写明
   「过滤范围：已取到的最近 N 条（不是全量搜索）」，但「在全部历史连接里搜一个域名」
   这个能力**现在没有**。后端有 `domain`/`inbound`/`outbound` 参数可下推，未接。
6. **真实应用里的端到端未验证，且 macOS 上目前没有可行手段**：我验证用的是
   `preview.ts` 的合成数据（payload 形状与 `types.ts` 逐字段一致）。解析正确性与配对率
   由 backend-dev 在定稿快照上的实测负责（§3，含 sha256）。
   「真机上点一条真连接」这一步没跑 —— **原因与替代覆盖见上方「验证手段的边界」**
   （WKWebView 无 CDP；三者并集只差 Tauri State 装配）。
   另有一条已知的时序配对误差（`dns-out` 1/4081，见 §3），界面不特判、不猜测。
7. **真机路径的最后一跳未验证**：`recent_connections` **已在** `apps/desktop/src/lib.rs`
   注册、编译通过；没有跑过的是「真核心写访问日志 → 应用内点一条真连接」。
   若在真机上调用失败，界面会显示「取不到最近连接」而**不是崩** ——
   该降级路径用 `?connections=unavailable` 验过（见 §8）。
8. **未测**：连接数上万时的长期内存行为、滚动列表在触屏上的手感、
   高亮在「出口卡片很多（>10）」时的视觉密度。
