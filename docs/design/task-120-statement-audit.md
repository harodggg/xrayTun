# task-120 · 界面「陈述 vs 实现」审计登记表

> 口径：**每一条面向用户的文案/状态/徽章/空态/确认语，都要能回答「它是从哪个真实字段推出来的」。**
> 推不出来 = 缺陷。分级：**A** = 会把用户导向错误结论或错误操作；**B** = 含糊或过强；**C** = 可保留。
>
> 审计方式：先机械提取 `apps/ui/src/**` 里所有含中文的**非注释**字符串（464 处候选），
> 再按页面分组逐条对照 `types.ts` 字段定义与 Rust 实现（只读）。
> 三个子系统（Settings / Nodes·Subscriptions·Logs / Globe·Topology）的原始逐条清单
> 由并行的只读子审计产出（`/tmp/t120-settings.md`、`/tmp/t120-nodes.md`、`/tmp/t120-topology.md`），
> 本文件只收**登记项**与复核状态。

## 一、本次已修（6 条，全部已提交 `5d2e66e`，均配「字段=X ⇒ 文案=Y」测试 + 反例 + 反向敏感性）

| # | 位置 | 原文 | 判据（实现） | 反例 |
|---|---|---|---|---|
| 1 | `App.tsx:289` | 只设置系统 HTTP/SOCKS 代理 | 全仓 `setwebproxy`/`getwebproxy`/`scutil`/`SCDynamicStore` **0 命中**；`model.rs:451` 只是注释里的意图 | 用户选「系统代理」以为已配置好，实际什么都没发生（同一屏的状态区却写着「需要手动指向」） |
| 2 | `Dashboard.tsx:151` | `{rtt} ms`（无条件 `latencyTier` 上色） | `ProbeResult.available` 与 `server_rtt_ms` 是两个独立字段；`probe.rs:296-298` 明确保留「不可用但量得到距离」 | `available=false, rtt=53` ⇒ 绿色「53 ms」；`Nodes.tsx:257` 早就是 `available===false ? "unknown"` |
| 3 | `topbarStatus.ts:286` | 未设系统代理 · 需指向 … | App 从不读系统代理设置 | 用户照做之后徽章仍然是「未设系统代理」 |
| 4 | `Subscriptions.tsx:151,213` | 会同时删除它带来的 N 个节点（N = `sub.node_count`） | 后端按 `source.id` 真删（`nodes.rs:283-292`）；`node_count` 只在刷新成功时写（`nodes.rs:348`），删节点/去重不回写 | 预览数据 `sub-1`：`node_count=3` 而真实归属只有 2 个 |
| 5 | `Logs.tsx:175` | 已抹掉订阅 URL、**节点地址**与 UUID —— 可以直接贴到公开的 issue 里 | 脱敏 = `redact_url` + `is_uuid_like`（36 字符 + 4 个 `-`）；IP/域名/IP:port 原样保留，报告收最近 ≤50 条日志 | 本机日志里含节点 IP 的行 **11421** 条（`dialing TCP to tcp:<IP>:443`）⇒ 照这句话贴出去就公开了服务器地址 |
| 6 | `Logs.tsx:204` / `:111` | 核心还没启动过 / 「错误：1 条」 | 前者判据原来只是 `!running`（现在时）；`runtime.started_at_unix` 只在启动时写、停止时不清。后者是**已加载窗口**（`tailLogs(500)`，封顶 `MAX_UI_LOGS=1500`）里的条数 | 跑过再停 / 清空后 ⇒ 说「还没启动过」；文件 35 万行含 400 条 error、窗口里 1 条 ⇒ 徽章显示 1 |

## 二、A 级登记表（22 条；已修 6 条，待修 16 条）

**复核状态列**：`✓我复核` = 我本人读过给出依据的代码；`子审计` = 由并行只读子审计报告，**我未逐条复核**（采用前需复核）。

| # | 位置 | 原文（要旨） | 反例 | 复核 | 待修原因 |
|---|---|---|---|---|---|
| A1 | `App.tsx:289` | 只设置系统 HTTP/SOCKS 代理 | 见上 | ✓我复核 | **已修** |
| A2 | `Dashboard.tsx:151` | 延迟数字替可用性背书 | 见上 | ✓我复核 | **已修** |
| A3 | `topbarStatus.ts:286` | 未设系统代理 | 见上 | ✓我复核 | **已修** |
| A4 | `Subscriptions.tsx:151,213` | 会删掉 N 个节点（N 为快照值） | 见上 | ✓我复核 | **已修** |
| A5 | `Logs.tsx:175` | 已抹掉节点地址，可直接公开 | 见上 | ✓我复核 | **已修** |
| A6 | `Logs.tsx:204` | 核心还没启动过（判据 `!running`） | 见上 | ✓我复核 | **已修（文案）** |
| A7 | `Logs.tsx:111` | 等级计数当作总数 | 见上 | ✓我复核 | **已修（title 口径）**；页面上仍没有「只统计已加载」的可见说明 |
| A8 | `Nodes.tsx:270,275` | 「正在使用这个节点」/「当前」 | 判据是 `settings.selected_node`（意图）。断开后 / `mode=direct` / 删掉当前节点（`nodes.rs:239-241` 静默改选且不重启核心）时流量并不走它 | ✓我复核 | UI 可改（`Dashboard.tsx:148` 已有正确口径 `connected && selected`），本轮未做 |
| A9 | `Nodes.tsx:342-346` | 「未测」把「测不到」与「没测过」合并 | `available=true, server_rtt_ms=null, error=null`（`probe.rs:282-292`）时显示「未测」且标题为空 | 子审计 | UI 可改（需把「有没有 ProbeResult」传进 `distanceLabelFor`），本轮未做 |
| A10 | `Settings.tsx:728,779,794` | 「状态未知」+「**重新**安装 helper」 | `HelperState::NotInstalled` 唯一构造点不可达（`helper_client.rs:225-228,265`，`state.rs:577` 默认 `Unknown`）⇒ 从没装过也显示「未知」 | 子审计 | **需要后端字段**（helper 状态机），超出本卡边界 |
| A11 | `Settings.tsx:526-535` | 选项「禁用 IPv6」 | `plan.rs:238` 把 `Disabled` 与 `Passthrough` 并在同一分支，全仓无第二处 ⇒ 与「不接管」逐字节相同，v6 仍泄漏 | 子审计 | **需要 Rust 实现**，超出本卡边界 |
| A12 | `Settings.tsx:629-639` | 「关闭嗅探后就失效」 | `config.rs:346` `sniffing \|\| fakedns.enabled` ⇒ 开 Fake-IP 后取消勾选仍为 true | 子审计 | **需要后端语义** |
| A13 | `Settings.tsx:731-732` | 「遗留会话：无」 | `tun_active=true`（已提交路由的孤儿会话，`lib.rs:242` 正把它当遗留物）时仍写「无」，与同结构体字段矛盾 | 子审计 | **需要后端字段** |
| A14 | `Settings.tsx:897-898` | DNS 徽章「不通」 | 判据只是 `latency_ms===null`；`answered` 是独立字段（`dns_probe.rs:445-447`）⇒ 答出但采样超时被写成「不通」而颜色是绿 | 子审计 | UI 可改；本轮未做 |
| A15 | `Settings.tsx:886-890` | 「当前首选 X」 | `chosen` 无条件写入，只有 `auto_select` 才写回配置（`snapshot.rs:122-160`）⇒ 显示的不是生效值 | 子审计 | UI 可改（需读 `auto_select`）；本轮未做 |
| A16 | `Settings.tsx:711-714` | 「保存并重启核心」 | `restart()` 不看 `save()` 结果；`store.tsx:155` 的 `setError(null)` 把保存错误抹掉 ⇒ 保存失败也照重启，用户以为已生效 | 子审计 | UI 可改（顺序 + 错误保留）；本轮未做 |
| A17 | `Settings.tsx:1051-1056` | 「下载中，请勿关闭…」 | `progress !== null` 即算下载中，geo 成功/失败都不清 `progress`（`snapshot.rs:377,413-422`）⇒ 永久「下载中」并永久禁用按钮 | 子审计 | UI+后端（快照字段） |
| A18 | `Settings.tsx:418` | 日志级别选项 `silent` | 原样写进 `"loglevel"`（`config.rs:177`），Xray 只认 debug/info/warning/error/**none** ⇒ `silent` 落到 warning，真正静音的 `none` 没提供 | 子审计（判据来自上游 Xray 源码） | UI 可改（改选项名/值）；本轮未做 |
| A19 | `Globe.tsx:155-156` | 「出口累计流量（**实测**）」 | `traffic_ok=false` 时 `bytes` 是占位 0，而 `Globe.tsx` 对 `traffic_ok` 引用 0 处 ⇒ 核心没跑时显示「0 B（实测）」 | 子审计 | UI 可改（读 `traffic_ok`）；本轮未做 |
| A20 | `Globe.tsx:155-156` | 同一个数字的**归属** | `read_exit_traffic` 取「所有 outbound 里 up+down 最大」（`globe.rs:239-245`）—— 是假设；`direct` 流量更大的用户会把直连流量显示在「出口 · 节点名」下 | 子审计 | UI 需后端给出「哪个出站是这个节点」；本轮未做 |
| A21 | `Globe.tsx:151,645` | 「本机 · <IP>」 | 两端坐标都是 IP 归属**推算**；`physical_interface()` 返回 `None` 时 curl 不绑卡 ⇒ 隧道开着时查到的是**节点的位置**，仍标「本机」 | 子审计 | 需要校验/后端字段 |
| A22 | `DestChecker.tsx:51` | 「这条结论是确定的（已与真实核心对拍过）」 | `explain_dest` 写死 `port:443, network:tcp, inbound:None`（`topology.rs:456-469`），界面从不说明 ⇒ 规则链里 `198.18.0.2:53 → dns-out` 这类会被判错 | 子审计 | UI 至少必须写清「按 443/tcp 求值」；本轮未做 |

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
3. 第二张表里标 `子审计` 的 16 条我没有逐条复核（复核方法是读它给出的 `file:line`，
   本轮只做了抽检）。采用前请按行号复核一遍。
4. 未覆盖的界面表面：`ipc.ts` 的 notice 文本、`store.tsx` 的 `errorText`（内部错误串）、
   `preview*.ts`（非生产路径）。
