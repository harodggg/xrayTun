# task-120 · 界面「陈述 vs 实现」审计登记表

> 口径：**每一条面向用户的文案/状态/徽章/空态/确认语，都要能回答「它是从哪个真实字段推出来的」。**
> 推不出来 = 缺陷。分级：**A** = 会把用户导向错误结论或错误操作；**B** = 含糊或过强；**C** = 可保留。
>
> 审计方式：先机械提取 `apps/ui/src/**` 里所有含中文的**非注释**字符串（464 处候选），
> 再按页面分组逐条对照 `types.ts` 字段定义与 Rust 实现（只读）。
> 三个子系统（Settings / Nodes·Subscriptions·Logs / Globe·Topology）的原始逐条清单
> 由并行的只读子审计产出（`/tmp/t120-settings.md`、`/tmp/t120-nodes.md`、`/tmp/t120-topology.md`），
> 本文件只收**登记项**与复核状态。

## 一、本次已修（15 条，已提交 `5d2e66e` + `31d1e6d` + 第三批，均配「字段=X ⇒ 文案=Y」测试 + 反例 + 反向敏感性）

| # | 位置 | 原文 | 判据（实现） | 反例 |
|---|---|---|---|---|
| 1 | `App.tsx:289` | 只设置系统 HTTP/SOCKS 代理 | 全仓 `setwebproxy`/`getwebproxy`/`scutil`/`SCDynamicStore` **0 命中**；`model.rs:451` 只是注释里的意图 | 用户选「系统代理」以为已配置好，实际什么都没发生（同一屏的状态区却写着「需要手动指向」） |
| 2 | `Dashboard.tsx:151` | `{rtt} ms`（无条件 `latencyTier` 上色） | `ProbeResult.available` 与 `server_rtt_ms` 是两个独立字段；`probe.rs:296-298` 明确保留「不可用但量得到距离」 | `available=false, rtt=53` ⇒ 绿色「53 ms」；`Nodes.tsx:257` 早就是 `available===false ? "unknown"` |
| 3 | `topbarStatus.ts:286` | 未设系统代理 · 需指向 … | App 从不读系统代理设置 | 用户照做之后徽章仍然是「未设系统代理」 |
| 4 | `Subscriptions.tsx:151,213` | 会同时删除它带来的 N 个节点（N = `sub.node_count`） | 后端按 `source.id` 真删（`nodes.rs:283-292`）；`node_count` 只在刷新成功时写（`nodes.rs:348`），删节点/去重不回写 | 预览数据 `sub-1`：`node_count=3` 而真实归属只有 2 个 |
| 5 | `Logs.tsx:175` | 已抹掉订阅 URL、**节点地址**与 UUID —— 可以直接贴到公开的 issue 里 | 脱敏 = `redact_url` + `is_uuid_like`（36 字符 + 4 个 `-`）；IP/域名/IP:port 原样保留，报告收最近 ≤50 条日志 | 本机日志里含节点 IP 的行 **11421** 条（`dialing TCP to tcp:<IP>:443`）⇒ 照这句话贴出去就公开了服务器地址 |
| 6 | `Logs.tsx:204` / `:111` | 核心还没启动过 / 「错误：1 条」 | 前者判据原来只是 `!running`（现在时）；`runtime.started_at_unix` 只在启动时写、停止时不清。后者是**已加载窗口**（`tailLogs(500)`，封顶 `MAX_UI_LOGS=1500`）里的条数 | 跑过再停 / 清空后 ⇒ 说「还没启动过」；文件 35 万行含 400 条 error、窗口里 1 条 ⇒ 徽章显示 1 |

## 二、A 级登记表（22 条；标「已修」**15 行**、待修 **7 行** —— 待修里 4 条需要后端字段/实现、2 条**已裁决为 UI 侧**、1 条**改为后端**）

**「已修」的粒度（按行可复核：`grep -c '^| A[0-9]* |.*已修'`）**：
标「已修」的是 **15 行**（A1–A9、A14–A16、A18、A19、**A22**）；
**A17 标的是「改为后端」**（修法转移到后端、尚未完成），**不计入「已修」**。
因此 22 = **15（已修）+ 7（待修：A10/A11/A12/A13/A17/A20/A21）**。

§一 标题的「15 条」是**按提交批次/条目**的另一种口径，本节是**按 A 级行**——两个口径不同，**不要用其中一个去推另一个**。
**A22 是最后补入登记表的一行**（修复同属 `31d1e6d` 批次，§一 的三批清单里没有单独列它）；
另有口径把它记作「补入的第 16 条」（= §一 的 15 + A22）。**读表时以「A 级行」为准**（本节即按此口径）。

**复核状态列**：
* `✓我复核` = **登记表作者本人**读过给出依据的代码；
* `✓已复核` = 由**他人**按当前 HEAD 独立复核过（复核来源见该行备注，例如 `docs/design/DECISION-A11-A12.md`）；
* `子审计` = 由并行只读子审计报告产出、**未逐条复核**（采用前需复核）。**当前已无此行**：
  原先的 6 条（A10/A11/A12/A13/A20/A21）已全部复核升级。

| # | 位置 | 原文（要旨） | 反例 | 复核 | 待修原因 |
|---|---|---|---|---|---|
| A1 | `App.tsx:289` | 只设置系统 HTTP/SOCKS 代理 | 见上 | ✓我复核 | **已修** |
| A2 | `Dashboard.tsx:151` | 延迟数字替可用性背书 | 见上 | ✓我复核 | **已修** |
| A3 | `topbarStatus.ts:286` | 未设系统代理 | 见上 | ✓我复核 | **已修** |
| A4 | `Subscriptions.tsx:151,213` | 会删掉 N 个节点（N 为快照值） | 见上 | ✓我复核 | **已修** |
| A5 | `Logs.tsx:175` | 已抹掉节点地址，可直接公开 | 见上 | ✓我复核 | **已修** |
| A6 | `Logs.tsx:204` | 核心还没启动过（判据 `!running`） | 见上 | ✓我复核 | **已修（文案）** |
| A7 | `Logs.tsx:111` | 等级计数当作总数 | 见上 | ✓我复核 | **已修（title 口径）**；页面上仍没有「只统计已加载」的可见说明 |
| A8 | `Nodes.tsx:270,275` | 「正在使用这个节点」/「当前」 | 见上 | ✓我复核 | **已修**（`31d1e6d`）：改为「已选中：核心运行时流量走这个节点」+ 徽章「已选中」 |
| A9 | `Nodes.tsx:342-346` | 「未测」把「测不到」与「没测过」合并 | 见上 | ✓我复核 | **已修**：加 `probed` 判据 ⇒ 探测过但量不到距离时写「距离未知」+ 标题说明 |
| A10 | `Settings.tsx:152,806,872` | 「状态未知」+「**重新**安装 helper」 | `HelperState::NotInstalled` 唯一构造点不可达（`helper_client.rs:225-228,265`，`state.rs:577`（变体）+ `:586-587`（`#[default] Unknown`））⇒ 从没装过也显示「未知」 | ✓已复核 | **需要后端字段**（helper 状态机） |
| A11 | `Settings.tsx:602-618` | 选项「禁用 IPv6」 | `plan.rs:238` 把 `Disabled` 与 `Passthrough` 并在同一分支，全仓无第二处 ⇒ 与「不接管」逐字节相同，v6 仍泄漏 | ✓已复核 | **已裁决**：`task-141` → `task-142`（UI 侧：删掉该选项；**不需要重装助手**）。复核来源：`docs/design/DECISION-A11-A12.md` |
| A12 | `Settings.tsx:708-717` | 「关闭嗅探后就失效」 | `config.rs:346` `sniffing \|\| fakedns.enabled` ⇒ 开 Fake-IP 后取消勾选仍为 true | ✓已复核 | **已裁决**：`task-141` → `task-142`（UI 侧：如实说明耦合；**不需要重装助手**）。复核来源：`docs/design/DECISION-A11-A12.md` |
| A13 | `Settings.tsx:809-810` | 「遗留会话：无」 | `tun_active=true`（已提交路由的孤儿会话，`lib.rs:242` 正把它当遗留物）时仍写「无」，与同结构体字段矛盾；**`Dashboard.tsx:288` 折叠区有「 · 有活跃隧道」⇒ 矛盾仅限设置页这一行** | ✓已复核 | **需要后端字段** |
| A14 | `Settings.tsx:897-898` | DNS 徽章「不通」 | 见上（我复核了 `dns_probe.rs:445-447`） | ✓我复核 | **已修**：`answered && latency_ms===null` ⇒ 「答得出，量不到延迟」 |
| A15 | `Settings.tsx:886-890` | 「当前首选 X」 | 见上（我复核了 `snapshot.rs:136-160` 的 `if settings.dns.auto_select`） | ✓我复核 | **已修**：按 `auto_select` 分两句，关着时写出真正生效的 `direct_servers[0]` |
| A16 | `Settings.tsx:711-714` | 「保存并重启核心」 | 见上（我复核了 `restart()` 的实现） | ✓我复核 | **已修**：`save()` 返回布尔值，保存失败即 return，不再 stop/start |
| A17 | `Settings.tsx:1051-1056` | 「下载中，请勿关闭…」 | 见上（我复核了 `snapshot.rs:400-421`：geo 路径**没有** `i.update.progress = None`，而 app/core 路径有） | ✓我复核 | **改为后端**：`UpdateProgress` 只有 label/done_bytes/total_bytes，**没有终态标记**，前端无从判断已结束 ⇒ 需要后端在 geo 成功/失败时清 progress |
| A18 | `Settings.tsx:418` | 日志级别选项 `silent` | 见上（我复核了 `model.rs:767` 是 `String`、`config.rs:177` 原样透传、默认值是 `warning`） | ✓我复核 | **已修**：选项改为 Xray 的 `none/error/warning/info/debug`；配置里存着历史值（如 `silent`）时如实标出「Xray 不识别」 |
| A19 | `Globe.tsx:155-156` | 「出口累计流量（**实测**）」 | 见上（我复核了 `types.ts:664` 的注释与 `Globe.tsx` 对 `traffic_ok` 的引用为 0 处） | ✓我复核 | **已修**（`31d1e6d`）：`traffic_ok=false` ⇒ 「出口流量读不到（不是 0）」，并如实带出 `counter_resets` |
| A20 | `Globe.tsx:163,174` | 同一个数字的**归属** | `read_exit_traffic` 取「所有 outbound 里 up+down 最大」（`globe.rs:232-250`；max 在 `:243-248`，把「最大者=节点出站」写成假设的是 `:229-231` 的注释）—— 是假设；`direct` 流量更大的用户会把直连流量显示在「出口 · 节点名」下 | ✓已复核 | UI 需后端给出「哪个出站是这个节点」；本轮未做 |
| A21 | `Globe.tsx:661`（`本机 · `）/ `:146-152`（「起点」） | 「本机 · <IP>」 | 两端坐标都是 IP 归属**推算**；`globe.rs:60-64` 的 `physical_interface()` 返回 `None` 时，`curl_get`（`:156-176`）不会加 `--interface`（`query_self` 在 `:107`）⇒ 隧道开着时查到的是**节点的位置**，仍标「本机」 | ✓已复核 | 需要校验/后端字段 |
| A22 | `DestChecker.tsx:51` | 「这条结论是确定的（已与真实核心对拍过）」 | 见上（我复核了 `topology.rs:456-469` 的固定 `port:443/network:tcp`） | ✓我复核 | **已修**（`31d1e6d`）：写明「按 443/tcp 求值」+ 只按端口/udp 命中的规则不在结论里 |

## 三、B 级与 C 级（计数）

| 来源 | A | B | C |
|---|---|---|---|
| App / topbarStatus / Dashboard / ipc / store / types / InlineConfirm | 3 | ~8 | 大量 |
| Nodes / Subscriptions / Logs | 6 | 11 | 42 |
| Settings | 9 | 17 | 62 |
| Globe / Topology / ConnectionsPanel / DestChecker | 4 | 10 | 67 |

B 级里值得优先看的几条：`Nodes.tsx:270`「点击切换到该节点」在 busy 时按钮不可点而 title 不变；`Settings.tsx:289`「有未保存的改动」不跟快照比对；`Settings.tsx:707`「建议使用最新的 26.9.x」是硬编码且会过时；`Topology.tsx:131`「累计值只增不减」的单调化基数在进程内，重启 App 会从 0 重来；`ConnectionsPanel.tsx:62`「点一条就会高亮那条路」—— 内部通道（实测占多数）点了不画线。

## 四、本轮**没有**验证的部分（诚实清单）

1. `apps/desktop/**` 与 `crates/**` 一条 Rust 测试都没跑：本环境禁止写 `~/.cargo/registry`
   （`Operation not permitted`）且锁定的 `libc 0.2.189` 不在本地缓存，`--offline` 解析失败。
   本卡也没有改任何 Rust 文件；上面所有 Rust 依据都是**读代码**，不是跑出来的。
2. 真机 WKWebView 无法自动化。本轮的替代证据 = jsdom 测试 + 本机真实的
   `runtime/config.json` / 核心日志 / `settings.json`（只读）。
3. **计数口径（已对齐）**：第二张表里原先标 `子审计` 的是 **6 条**（A10/A11/A12/A13/A20/A21），
   本节此前误写成「16 条」；**16 恰好是「已复核」的行数**（A1–A9、A14–A19、A22），
   只有「已复核 16 + 未复核 6」并列才对得上 22 行。
   原 6 条现已全部按当前 HEAD 复核并升级为 `✓已复核`（A11/A12 见 `docs/design/DECISION-A11-A12.md`，
   A10/A13/A20/A21 见本次同步）；复核方法 = 读它给出的 `file:line` 并核对结论是否仍成立。
4. 未覆盖的界面表面：`ipc.ts` 的 notice 文本、`store.tsx` 的 `errorText`（内部错误串）、
   `preview*.ts`（非生产路径）。
