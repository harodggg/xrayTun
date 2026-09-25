# UX 走查：意图过滤 / MITM / 连接失败横幅 / 崩溃后的可行动性

**审查员**：ux（task-5）
**对象**：`main` @ `0e09214`
**读过的实现**：`apps/ui/src/pages/Intent.tsx`、`apps/ui/src/pages/Dashboard.tsx`、`apps/ui/src/pages/Logs.tsx`、`apps/ui/src/pages/Settings.tsx`、`apps/ui/src/App.tsx`、`apps/ui/src/store.tsx`、`apps/ui/src/topbarStatus.ts`、`apps/ui/src/ipc.ts`、`apps/ui/src/types.ts`；`apps/desktop/src/{mitm.rs,supervisor.rs,helper_client.rs,tray.rs,lib.rs}`、`apps/desktop/src/commands/{core.rs,mitm.rs,helper.rs}`；`crates/xt-intent/src/{engine.rs,audit.rs}`、`crates/xt-core/src/{model.rs,store.rs}`、`crates/xt-helper/src/server.rs`
**视角**：第一次打开这个 App 的人 —— 不知道 Jev / blackhole / helper 协议号 / 「闸门」是什么，也分不清「本地代理在跑」与「核心在跑」。

---

## 结论 + 证据链接

**结论**：这一页的整体骨架（开关 / 演练模式 / 规则数 / 审计 / MITM 五道状态行）方向是对的，但**在四处把「意图」说成了「事实」**，其中三处会让第一次打开的人形成**错误信念**，而且都是**同屏自相矛盾**（页面上一行说「已生效」，下一行说「还没下发」）。最严重的一条是 **MITM 状态行的「生效」**：当核心根本没在跑时，它会写「引导规则已随核心生效」（`Intent.tsx:453-460` + `mitm.rs:252-255`）。第二严重的是**审计表的「生效 = 是」**：判定时刻的 `applied` 只表示「这条判决将会变成规则」，不表示「已经下发到核心」（`engine.rs:444-446`），而页面注释还把它解释错了（`Intent.tsx:607`）。第三严重的是**连接失败横幅**：它把一条多行、带 `**` 记号的工程文案原样塞进两个横幅（`supervisor.rs:459-462` → `App.tsx:159-166`、`Dashboard.tsx:501-508`），**没有下一步动作按钮**，也**没有把「重装助手」放到该出现的地方** —— 而 v0.8.38 的真实修复动作正是重装助手（`docs/release-notes/v0.8.38.md:1-9`），该入口只在两页之外的 `Settings.tsx:935-963`。

**证据链接（全部可点开复核）**

| 编号 | 严重度 | 一句话 | 文件:行 |
|---|---|---|---|
| U1 | 高 | MITM「生效」把「核心没在跑」说成「已随核心生效」 | `apps/ui/src/pages/Intent.tsx:453-460`、`apps/desktop/src/mitm.rs:252-255` |
| U2 | 高 | 审计列「生效 = 是」会被读成「已经在拦」；页面注释也把它解释错了 | `apps/ui/src/pages/Intent.tsx:620,646,605-608`、`crates/xt-intent/src/engine.rs:444-446` |
| U3 | 高 | 连接失败横幅无动作、无重装助手入口，且原文带 `**` 与坍缩的换行 | `apps/desktop/src/supervisor.rs:459-462`、`apps/ui/src/App.tsx:159-166`、`apps/ui/src/pages/Dashboard.tsx:501-508` |
| U4 | 高 | 启动即崩时，`logs/panic.log` 这条唯一的证据链在 App 内 0 命中 | `apps/desktop/src/lib.rs:85-96`、`docs/release-notes/v0.8.39.md:20-27`、`apps/ui/src/pages/Logs.tsx:291-295` |
| U5 | 中 | 「生效的规则 拦截 N 条」与同屏「还没下发给核心」矛盾 | `apps/ui/src/pages/Intent.tsx:208-212`、`228-242` |
| U6 | 中 | 「闸门」一词在页面上指意图阈值、在发布说明里指 MITM 三道闸门 | `apps/ui/src/pages/Intent.tsx:326`、`docs/release-notes/v0.8.38.md:31` |
| U7 | 中 | MITM 顺序叙述与实现不一致；「重连核心」这一步没有落点 | `Intent.tsx:562-599`、`apps/desktop/src/commands/mitm.rs:86` |
| U8 | 中 | 失败横幅叫用户「点『断开』」，而那一刻按钮写着「连接」 | `supervisor.rs:452,457`、`topbarStatus.ts:201-208`、`App.tsx:392` |
| U9 | 中 | 「退出会撤掉根证书」没写 helper 没应答时不会撤 | `Intent.tsx:436-439`、`apps/desktop/src/tray.rs:179-186` |
| U10 | 低 | 「撤掉根证书」会顺手停代理，界面没说 | `Intent.tsx:573-582`、`commands/mitm.rs:114-117` |
| U11 | 低 | 审计「拦截」在演练模式下是绿色徽章 | `Intent.tsx:630-639` |
| U12 | 低 | 首屏文案里的 `blackhole` / `Jev 的类型化判定` / 反引号是术语与外泄的记号 | `Intent.tsx:183-185`、`605-608` |
| U13 | 低 | 开关关掉后，演练行仍无条件写「会生成拦截规则」 | `Intent.tsx:199` |
| U14 | 中 | 同一条运行错误在两个横幅里重复出现（一个能关、一个不能） | `apps/ui/src/App.tsx:159-166`、`apps/ui/src/pages/Dashboard.tsx:501-508` |

**走查方式**：先读源码（含后端字段语义），再跑渲染断言 `cd apps/ui && ./node_modules/.bin/vitest run src/intentPage.test.tsx` → **12 passed**（实跑结果，两次运行均为 12 passed）。下面 U1/U3/U4 恰恰是这 12 条**没有断言到**的渲染路径 —— 所以它们是「测试没覆盖」，不是「测试红了」。

---

## 一、意图过滤页：默认关闭 / 演练模式 / 待生效 / 审计 `applied`

### U1 ｜ **高** ｜ MITM 状态行「生效」：核心没在跑，却写「引导规则已随核心生效」

**现状原文**（`apps/ui/src/pages/Intent.tsx:453-460`）

```tsx
<span>生效</span>
<strong>
  {mitm?.core_restart_required
    ? "要重连一次核心才会下发引导规则"
    : mitm?.active
      ? "引导规则已随核心生效"
      : "不生效（见上面的原因）"}
</strong>
```

**为什么会错**：`core_restart_required` 的计算是 `core_steering: Option<bool>`，`None`（**核心从没启动过、或已经停了**）时直接取 `false`（`apps/desktop/src/mitm.rs:252-255`，字段语义见 `:128`「`None` = 没记录过/核心没在跑」）。而 `mitm.active` 只等于 `settings.enabled && !domains.is_empty()`（`crates/xt-core/src/model.rs:1134-1136`），**与核心无关**。⇒ 只要用户「开了 MITM + 填了名单 + 装了证书 + 起了本地代理」而**没点过连接**，这一格就写「引导规则已随核心生效」，而同一屏的顶栏状态是「未连接 —— 核心没有运行」（`topbarStatus.ts:212-218`）。

**第一次打开的人会形成什么信念**：以为「广告已经在被拆包/被拦了」，从而不去点连接，也看不懂为什么审计里没有内容级动作。这是本次走查里唯一一条**把「没发生」说成「已发生」**的文案。

**建议文案**（把 `core_steering === null` 单独做成一态，它已经是快照里的真实字段）

```tsx
<span>生效</span>
<strong>
  {mitm?.core_steering === null
    ? "核心没在跑 —— 引导规则现在不在任何核心里（先连接核心）"
    : mitm?.core_restart_required
      ? "核心正在用旧配置：要重连一次才会下发引导规则"
      : mitm?.core_steering
        ? "引导规则已随核心生效"
        : "本次核心没带引导规则（证书或名单是在它启动后才满足的，重连一次即可）"}
</strong>
```

### U2 ｜ **高** ｜ 审计列「生效 = 是」会被读成「已经在拦」

**现状原文**

- 表头：`<th>生效</th>`（`apps/ui/src/pages/Intent.tsx:620`）
- 单元格：`<td>{row.applied ? "是" : "否"}</td>`（`Intent.tsx:646`）
- 页面自己的解释（`Intent.tsx:605-608`）：

  > 每一条判决都可复查：结论、原因、分数、**是否真的变成了规则**。`applied=false` 表示当时在演练模式或还没下发。

**为什么会错**：后端的 `applied` 是**判定那一刻**按 `!drill && verdict.is_block()` 算的（`crates/xt-intent/src/engine.rs:444-446`，字段注释见 `crates/xt-intent/src/audit.rs:51-52`）。它与「这条规则有没有被下发到正在跑的核心」**完全无关**。所以：

1. 演练模式关掉、还没点「应用（会重连一次）」时，审计行显示「生效 = 是」，而同屏横幅写着「判决变了但还没下发给核心」（`Intent.tsx:228-230`）—— **同一屏两句话互相否定**；
2. 页面注释说 `applied=false` 可能是「还没下发」，但后端**永远不会**因为「还没下发」把 `applied` 置 false。这条注释把用户引向一个错误的反向推论：「`applied=true` 就说明已经下发了」。

**第一次打开的人会形成什么信念**：扫一眼审计表看到一列「是」，就认为这些域名当下正在被 blackhole 掉；发现网站其实还能打开时，会怀疑产品在说假话。

**建议文案**（**改列名与注释，而不是改数据口径**——后端字段是「判决是否构成规则」，这个语义本身没错）

```tsx
<th>会生成规则</th>
...
<td>{row.applied ? "会" : "不会（演练模式）"}</td>
```

并把页面注释改成与后端同义：

> 这一列说的是**这条判决是否构成一条拦截规则**（演练模式下一律「不会」）。
> 它**不代表规则已经下发到核心** —— 规则只在核心启动时下发，所以还要看上面的「应用（会重连一次）」。

如果希望这一列真的表达「已生效」，那必须在渲染时把 `applied && !summary.rules_pending_apply` 合起来才显示「是」，并在 `rules_pending_apply` 时显示「待下发」。

### U5 ｜ **中** ｜ 「生效的规则 拦截 N 条」与同屏「还没下发」矛盾

**现状原文**（`apps/ui/src/pages/Intent.tsx:208-212`）

```tsx
<span>生效的规则</span>
<strong>拦截 {summary?.block_rules ?? 0} 条 · 放行 {summary?.allow_rules ?? 0} 条</strong>
```

而后端给的 `block_rules/allow_rules` 是 `IntentRuntime::rules()` 的条数，也就是**当前应该生效的规则集合**，不是「核心已经加载的规则」（`apps/desktop/src/intent.rs:240-251`）。紧接着的横幅说这些规则「还没下发给核心」（`Intent.tsx:228-230`）。

**建议文案**：把「生效」改成「当前规则集合」，并补一句它什么时候才会真的生效：

```tsx
<span>当前规则集合</span>
<strong>拦截 {block_rules} 条 · 放行 {allow_rules} 条</strong>
{summary?.rules_pending_apply && <span className="field__hint">（尚未下发到核心）</span>}
```

### U13 ｜ **低** ｜ 默认关闭 + 演练模式本身是清楚的；但开关关掉后，演练行仍写「会生成拦截规则」

**现状原文**（`Intent.tsx:195`、`:199`）

```tsx
<strong>{s.enabled ? "已开启" : "未开启"}</strong>
...
<strong>{s.drill ? "开（只记录，不下发规则）" : "关（会生成拦截规则）"}</strong>
```

**判断**：默认值 `enabled=false`、`drill=true`（`crates/xt-core/src/model.rs:949-951`），首屏会看到「开关 未开启 / 演练模式 开（只记录，不下发规则）」，加上下面的「要让它真的能拦，还差什么：意图过滤未开启」（`Intent.tsx:246-255`）—— **这两条合起来是能看懂的**，这一页最担心的「演练模式被读成已开启」没有发生（`intentPage.test.tsx:165-170` 也钉着它）。

**但**第二行是无条件渲染的：当用户把开关关掉、护栏整行写「关（会生成拦截规则）」时，首屏会出现「开关 未开启」+「会生成拦截规则」的并置（关掉演练之后就是这个状态）。建议这一行只在开关打开时给「会生成拦截规则」这个后果，否则回落到中性说明：

```tsx
<strong>{!s.enabled ? "未启用（开关打开后才谈得上）" : s.drill ? "开（只记录，不下发规则）" : "关（会生成拦截规则）"}</strong>
```

### U6 ｜ **中** ｜ 「闸门」一词两义：页面上指意图阈值，发布说明里指 MITM 三道闸门

**现状原文**（`apps/ui/src/pages/Intent.tsx:326`）

```tsx
<h2>闸门（三条件全满足才拦）</h2>
```

这一节讲的是 `ads_intent_min` / `risk_of_breakage_max` / `choice_confidence_min` 三个**判定阈值**；而用户拿到的发布说明里「三道闸门」指的是 MITM 的**开关+名单 / 证书已装 / 代理在跑**（`docs/release-notes/v0.8.38.md:31`）。MITM 卡片里从头到尾**没有出现「闸门」这个词**，五道状态行（开关/名单/代理/指纹/生效，`Intent.tsx:444-461`）也没标注哪三条是闸门。

**第一次打开的人会形成什么信念**：把阈值那一节的「三道闸门」当成 MITM 不生效的原因，去调滑块。

**建议文案**：两处各用各的词，且 MITM 一节显式列闸门。

- 阈值节标题改：`命中判据（三条全满足才拦）`
- MITM 状态区加一行：`闸门：① 开关+名单 ② 根证书已信任 ③ 本地代理在跑（三条都满足且核心带上引导规则，才算真的在拆包）`

### U12 ｜ **低** ｜ 首屏与审计注释里的术语和裸记号

**现状原文**

- `Intent.tsx:183-185`：`用 Jev 的类型化判定判断端点是不是投放/追踪基础设施，然后把它交给 Xray 的 blackhole。`
- `Intent.tsx:607`：`` `applied=false` 表示当时在演练模式或还没下发。``

JSX 里的反引号**不会**被渲染成代码样式，用户看到的就是两个反引号字符。`blackhole` 与 `Jev` 对第一次打开的人是纯术语。

**建议文案**

- 首屏：「先用模型判断一个域名是不是广告/追踪端点，命中的交给核心黑洞掉（连接直接丢弃）。」
- 审计注释：去掉反引号，直接写 `生效 = 否`（并见 U2 的改法）。

### U11 ｜ **低** ｜ 演练模式下「拦截」是绿色徽章

**现状原文**（`Intent.tsx:630-640`）

```tsx
className={row.outcome === "block" ? "badge badge--ok" : ...}
...
{OUTCOME_LABEL[row.outcome] ?? row.outcome}   // block → "拦截"
```

演练模式下这一行右边写着「生效 否」，左边却是绿色「拦截」。绿色在这套配色里是「好/已完成」（`topbarStatus.ts:34-43` 把绿色定义成「整机受保护」）。

**建议文案**：演练模式（`applied=false`）下的 block 用中性徽章 + 文案「本该拦截」：

```tsx
{row.applied ? "拦截" : row.outcome === "block" ? "本该拦截（演练）" : OUTCOME_LABEL[row.outcome]}
```

---

## 二、MITM 一节：三道闸门 / 每次启动换证书 / 只拆点名域名 / 装证书→应用→重连

### 说得清的部分（先说好的）

- 「只对你点名的域名生效」在首屏（`Intent.tsx:185`）与 MITM 警告段（`Intent.tsx:436-439`）各写了一次，名单框标签也写「只拆这些域名」（`:488`）—— 这一条**不会**被误解。
- 空名单的状态行明确写「空（不会拆任何域名）」（`:448`），后端给的原因是「名单为空：一个域名都不会被拆包」（`mitm.rs:241`）。
- 「证书没装就不会下发引导规则」在页面上有完整后果说明（`:594-598`），后端也确实这么实现（`mitm.rs:45-51` + 判别性测试 `mitm.rs:341-364`）。
- 「WebSocket 会被拒 501，别放进名单」写进了代理账（`:468-473`）。

### U1 见上（MITM 状态行的「生效」是本页最严重的一条）

### U7 ｜ **中** ｜ 顺序叙述「装证书 → 应用 → 重连核心」与实现不一致，且最后一步在 MITM 卡片里没有落点

**现状原文**（`Intent.tsx:594-599`）

> 顺序是**装证书 → 应用 → 重连核心**：引导规则挂在核心的出站/入站上，没法热加，所以最后那一步必须重连一次（和上面意图规则的「应用」一样）。证书没装时**不会**下发引导规则 —— 否则那几个域名的 HTTPS 会撞上一张没人信的证书，那就不是过滤而是把网站搞坏。

两个实现事实与这段话对不上：

1. **点「装入根证书」已经自动把「应用」也做掉了**：`mitm_ca_install` 在装完锚之后 `mitm_apply(state).await`（`apps/desktop/src/commands/mitm.rs:86`）。所以「装证书 → 应用」其实是**一次点击**完成的两步，而按钮排布（`Intent.tsx:562-593`）把「装入根证书 / 撤掉根证书 / 应用（起/停代理）」并列成三个平等动作。
2. **「最后那一步必须重连一次」没有按钮**。意图那一半有「应用（会重连一次）」（`Intent.tsx:239`），MITM 这一半只有「应用（起/停代理）」（`:591`），它的后端语义是起停本地代理（`commands/mitm.rs:125-149`），**不是**重连核心。用户被告知「必须重连一次」之后，唯一的办法是自己去顶栏把「断开→连接」点一遍 —— 而顶栏那一刻写的是「断开」。

**第一次打开的人会形成什么信念**：① 以为点完「装入根证书」还要再点「应用」，因而重复点击、看到状态没变而困惑；② 以为「应用（起/停代理）」就是那句「重连核心」，点完发现状态还是「要重连一次核心才会下发引导规则」。

**建议文案**

- 「装入根证书」按钮下加一句：`（会同时启动本地代理；你只需要再做最后一步：重连核心）`
- 在「生效」格为 `core_restart_required` 时，就地给出动作而不是只给结论：

  ```tsx
  <button className="btn btn--primary" onClick={() => onNavigate("dashboard")}>去重连核心（顶栏「断开」→「连接」）</button>
  ```

  （若要保持「绝不自动重连」的约束，就只做导航，不要在这里直接调 `api.stop/start`。）

### U9 ｜ **中** ｜ 「退出会撤掉」漏掉了 helper 没应答时不撤的情况

**现状原文**（`Intent.tsx:436-439`）

> 这是整个功能里**唯一会改系统状态**的部分：会把一张本地根证书装进系统钥匙串，并**只对你点名的域名**拆 TLS。本版每次启动重新生成一张，退出（或下次启动的过期会话回滚）会撤掉。

机制本身是真的（退出走 `Request::Restore` → `force_cleanup` → 快照里含 `trust_anchors`，`apps/desktop/src/tray.rs:179-186`、`crates/xt-tun/src/macos/controller.rs:263`），但退出那条路有**明写的失败分支**：

> `Err(_) => tracing::warn!("helper 正忙，跳过退出前回滚；下次启动会自动修复")`（`tray.rs:186`）

也就是说「退出会撤掉」不是无条件事实。考虑到这句话是**用户做装证书决定时的唯一凭据**，漏掉失败分支会让人以为「退出即干净」。

**建议文案**（不夸大、只说已确认的）

> ……本版每次启动重新生成一张；正常退出时（或下次启动回滚过期会话时）会把上一张从钥匙串里撤掉。**如果退出时特权助手没有应答，这一步会跳过并留到下次启动重试** —— 想立刻确认，可以在「设置 → 系统与助手」里点一次「修复网络」。

### U10 ｜ **低** ｜ 「撤掉根证书」会顺手停掉本地代理，界面没说

**现状原文**：按钮「撤掉根证书」（`Intent.tsx:581`）没有说明；后端在撤掉锚之后**必定**调 `i.mitm.stop()`（`commands/mitm.rs:114-117`），注释里写了理由（代理会继续用一张没人信的证书接流量）。

**建议文案**：按钮 title 或旁边一行 `（会同时停掉本地代理，并让核心下次重连不再带引导规则）`。

---

## 三、连接失败横幅（真实现场：接管路由前门禁未过）

### U3 ｜ **高** ｜ 横幅没有动作、没有重装助手入口，原文还带着 `**` 和坍缩的换行

**现状原文 1**：门禁失败时后端生成的消息（`apps/desktop/src/supervisor.rs:459-462`）

```rust
format!(
    "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应：{list}。\n\
     国内网络下「TCP 能连到服务器、代理协议握手被墙」是常见情形。\n\
     **已在接管默认路由之前中止**，系统网络未被改动。{diagnosis}"
)
```

其中 `diagnosis` 还含 `**这次探测里失败的全是域名目标**`、`**这不是本机 DNS 的问题**`（`supervisor.rs:447-454`）或 `可能是这个节点不可用……先点「断开」恢复直连`（`:455-458`）。

**现状原文 2**：它被写进 `runtime.last_error`（`apps/desktop/src/commands/core.rs:219-226`）之后，同时出现在两处：

- 全局错误横幅：`<div className="banner banner--error"><span>⚠︎</span><div style={{flex:1}}>{error}</div><button…>关闭</button></div>`（`apps/ui/src/App.tsx:159-166`）
- 仪表盘最急的那条 notice：`text: <>上次运行出错：{runtime.last_error}</>` —— **`rank: 0`、`tone: "error"`，而且这条 notice 没有带 `action`**（`apps/ui/src/pages/Dashboard.tsx:501-508`；`Notice` 接口本身是支持 `action` 的，定义在 `:47-58`）

**凭什么说是问题**：

1. **文案（原因 / 下一步 / 自救）其实写全了**：说了原因（真实请求拿不到响应）、好消息（未接管路由、系统网络未改动）、下一步（先试换一个节点；整机上不了网先点「断开」）。这一条**要给后端文案记功**。
2. **但它没有动作按钮**，而这句话里有两个隐含动作（换节点、断开）。用户在仪表盘看到一条红色长文，只能自己去侧栏找「节点」；而后端早就写好了一个导航真源：`onNavigate("settings", "set-helper")` 这类带目标的跳转（`Dashboard.tsx:583-594`、`App.tsx:93-96`）。通知结构对「最后一条错误」不提供 `action`，所以这条路上完全用不上。
3. **`**` 与换行被原样渲染**：两个载体都是普通 `<div>`（`.banner` 没有 `white-space` 设置，`apps/ui/src/styles.css:571-583`），默认 `white-space: normal` ⇒ 换行折叠成一个空格，`**已在接管默认路由之前中止**` 会**带星号**显示。第一次打开的人看到一条 100+ 字、带 `**` 的单段红字。
4. **「重装助手」不在这里**。v0.8.38 的给用户动作是「本版必须重新安装特权助手」，且明写「装着旧助手的机器上，设置页会报助手版本不匹配，TUN 与 MITM 都无法工作」（`docs/release-notes/v0.8.38.md:1-9`）。而：
   - 门禁失败文案（`supervisor.rs:431-470`）**一个字都没提 helper**；
   - 助手协议不匹配时 TUN 的报错是 `helper 建立 TUN 失败：协议版本不匹配：helper=N 客户端=M`（`supervisor.rs:1104` + `helper_client.rs` 的 `classify` 没有 ProtocolMismatch 分支、落到 `Unknown` 原文透出，`:300-343`）——**没有下一步**；
   - 「重新安装助手」按钮只存在于 `Settings → 系统与助手`，且只有当 `helper.version_check.state === "mismatch"` 时才出现（`Settings.tsx:935-963`）；仪表盘的「环境自检与诊断」折叠区**只显示版本与协议号，不做对照**（`Dashboard.tsx:430-447`，最坏情况是「已安装但无法连接：协议版本不匹配…」）；
   - 版本核对结果**从不进入**仪表盘 notice 集合（`collectNotices` 只看 `core/helper/notice/runtime`，`Dashboard.tsx:492-599`；`last_notice` 的写入点全在 `lib.rs`/`tray.rs`/`core.rs` 的隧道/恢复路径，没有一处是版本核对）。

**第一次打开的人会形成什么信念**：① 失败一定是节点被封了（于是反复换节点，而真实原因是旧助手）；② 「产品没告诉我能做什么」；③ `**` 是乱码/渲染 bug。

**建议文案**（分三层落）

1. **后端：把结构化信息也带出来，而不是只带一句散文。** 门禁/助手失败时，除 `last_error` 外给一个可判定的字段（如 `runtime.failure_kind = "helper_protocol_mismatch" | "gate_probe" | ...`），让界面能选动作。若这一步太大，至少在**协议不匹配**的失败文案里直接写上文档里已有的那句话：

   > helper 建立 TUN 失败：协议版本不匹配（助手 v{installed} 协议 {p1}，App 需要协议 {p2}）。
   > **App 更新不会刷新特权助手** —— 请到「设置 → 系统与助手」点「重新安装助手」（需要一次管理员授权）。装完不需要再做别的。

2. **前端：这条错误横幅给动作。** 给 `last-error` notice 加 `action`（结构支持，`Dashboard.tsx:47-58`），按原因二选一：
   - 助手类 → `{ label: "去重装助手", run: () => onNavigate("settings", "set-helper") }`
   - 门禁类 → `{ label: "去换一个节点", run: () => onNavigate("nodes") }`
3. **渲染：不要把工程记号直接给用户。** 两个选择：（a）后端文案去掉 `**`，改用普通句子；（b）前端把错误文本按行渲染 —— 至少 `white-space: pre-wrap`，并去掉成对的 `**`。推荐 (a)：后端那份文案是给用户看的，不是给 issue 看的。

### U8 ｜ **中** ｜ 失败横幅叫用户「点『断开』」，而那一刻按钮写的是「连接」

**现状原文**（`supervisor.rs:452`、`:457`）：`先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。`

在「接管路由前门禁未过」这一现实场景里，门禁失败发生在 `commit` 之前并已回滚（`supervisor.rs:950-977`），所以 `runtime.running=false`、`runtime.last_error≠null`，状态词是「核心未运行」（`topbarStatus.ts:201-208`），顶栏按钮文字是「连接」（`App.tsx:392`）。用户按文案去找「断开」，**屏幕上没有这个按钮**。

**建议文案**：把动作按「有没有在跑」拆开，或写成不依赖按钮名的说法：

> - 还没接管路由、当前没有隧道：`这条连接没有建立，系统网络也没被改动 —— 直接换一个节点再连即可。`
> - 已经在跑的隧道出问题时：`如果你现在整台上不了网，点顶栏右上角的按钮断开（它会显示为「断开」）即可恢复直连。`

### U14 ｜ **中** ｜ 同一条错误在全局横幅与仪表盘 notice 里重复出现，且一条能关、一条不能

**现状原文**：`App.tsx:159-166`（带「关闭」按钮）与 `Dashboard.tsx:501-508`（无关闭）同时渲染同一 `runtime.last_error`。关掉全局横幅后，仪表盘那条 rank 0 的红条还在，用户会以为「关不掉」。

**建议文案**：同一事实一处陈述 —— 让全局横幅只承担「发送命令失败」的即时反馈，把「上次运行出错」留在仪表盘并给它动作（见 U3）；或在全局横幅的「关闭」同时 `clearError` 且清掉 `runtime.last_error` 的展示（后者需要后端支持，属实现变更）。

---

## 四、崩溃后的可行动性：`logs/panic.log` 该不该引导

### U4 ｜ **高** ｜ 启动即崩时，唯一的证据链在 App 内 0 命中

**现状原文 1**（后端确实把证据写下来了，`apps/desktop/src/lib.rs:85-96`）

```rust
// ① 文件：App 数据目录下的 `logs/panic.log`（用户能直接打开、能发给我们）。
let dir = xt_core::store::Store::default_root().join("logs");
```

路径展开是 `~/Library/Application Support/com.xraytun.desktop/logs/panic.log`（`crates/xt-core/src/store.rs:41-51` + `crates/xt-core/src/lib.rs:39`），与 `docs/release-notes/v0.8.39.md:20-27` 写给用户的完全一致。

**现状原文 2**：App 里**没有任何地方**提这件事。实测：

```
grep -rn "panic" apps/ui/src | wc -l        → 0
grep -rn "panic.log" apps/ui/src            → 0
grep -rln "panic.log" README.md docs site   → docs/release-notes/v0.8.39.md（只有发布说明）
```

Logs 页有「诊断」按钮（`Logs.tsx:154-159`）与「报告问题」（`Logs.tsx:291-295`），但它们都在**能打开 App** 的前提下才有用；「打开数据目录」按钮在仪表盘的折叠区里（`Dashboard.tsx:467`）—— 同样需要 App 能起来。

**第一次打开的人会形成什么信念**：双击图标 → 图标闪一下没了 → 系统「报告崩溃」里只有一句 `abort() called`（这正是 v0.8.38 的现场，`docs/release-notes/v0.8.39.md:9-14`）→ **以为这个 App 什么线索都没留下**，于是既不能自助、也交不出可定位的信息。

**要不要在界面上引导？要，但要按「App 可能起不来」分两层落**：

1. **App 起不来时**（这条最要紧，界面帮不上忙）—— 引导必须落在 App 之外的载体：
   - 发布说明保留（已有，`docs/release-notes/v0.8.39.md:26-27`），并且**发行物里要能被看到**（这一点我未能验证，见末节）；
   - 站点加一页「启动崩溃怎么办」，内容就两句：证据路径 + 「把 panic.log 尾部发来即可定位到文件:行」。当前 `site/` 下没有排障页（`site/index.html`、`site/en/` 等均为产品页），建议新增；
   - 打包脚本里把这一句放进 DMG 的说明/README（`scripts/package-macos.sh` 目前只校验 Resources，见 `:199-201`）。
2. **App 能起来时**（崩溃发生在非启动路径、或用户第二次侥幸起来）—— 在 Logs 页「报告问题」上方加一行常驻说明与动作：

   > 如果 App 曾经**启动就退出**过：证据在 `~/Library/Application Support/com.xraytun.desktop/logs/panic.log`（含 `文件:行:列` 与 backtrace）。点「打开数据目录」→ `logs/` 就能找到它。

   动作复用既有的 `api.openDataDir()`（`Dashboard.tsx:467`）；消息文案可直接复用现有常量风格。
3. **可选（需要改代码，非本次 UX 结论）**：启动时探测 `logs/panic.log` 是否存在且 mtime 晚于上次正常退出，存在就给一次性横幅「上次启动崩溃过，证据在 …」。当前 `bootstrap` 已有遗留会话横幅的既有模式（`Dashboard.tsx:526-558`），可挂在同一处。

---

## 五、错误信念清单（本次审查要拦的，按严重度）

| # | 用户会相信什么 | 触发它的原文 | 实际事实 |
|---|---|---|---|
| 1 | 「MITM 已经在拆这些域名了」 | `Intent.tsx:458`「引导规则已随核心生效」 | 核心没在跑时 `core_steering=null`，一条引导规则都不在任何核心里（`mitm.rs:252-255`） |
| 2 | 「审计里『生效=是』的域名正在被拦」 | `Intent.tsx:620,646` | `applied` 只表示「这条判决构成规则」，与是否下发无关（`engine.rs:444-446`） |
| 3 | 「`applied=false` 只可能是演练模式或还没下发」⇒ 反过来 `applied=true` 一定已下发 | `Intent.tsx:607` | 后端从不因「还没下发」置 false |
| 4 | 「核心里已经有 N 条拦截规则」（因为标题写着『生效的规则』） | `Intent.tsx:208-209` | 那是**当前应该生效**的集合，尚未下发时也照显（`intent.rs:240-251`） |
| 5 | 「TUN 连不上就是节点被封了」 | `Dashboard.tsx:507` 只透传门禁原文；`supervisor.rs:1104` 只说协议不匹配 | v0.8.38 的真实修复动作是**重装助手**（`docs/release-notes/v0.8.38.md:1-9`） |
| 6 | 「点完『断开』就能恢复，按钮就在那儿」 | `supervisor.rs:452,457` | 门禁失败时核心没在跑，按钮写的是「连接」（`App.tsx:392`） |
| 7 | 「退出 App 一定撤掉了根证书」 | `Intent.tsx:437` | helper 没应答时该步会被跳过（`tray.rs:186`） |
| 8 | 「App 崩溃什么都不留」 | App 内 0 处提及 panic.log | panic hook 已写位置到 `logs/panic.log`（`lib.rs:85-96`） |

---

## 六、优先修复建议（按「一条改动挡住最多错误信念」排序）

1. `Intent.tsx:453-460` 的「生效」四态化（U1）—— 一行改动，消掉第 1 条错误信念。
2. 审计列改名为「会生成规则」+ 改注释（U2）—— 两行改动，消掉第 2、3 条。
3. 「生效的规则」→「当前规则集合（尚未下发）」并挂到 `rules_pending_apply`（U5）—— 消掉第 4 条。
4. 给 `last-error` notice 加 `action`（U3/U8），助手类跳 `set-helper`、门禁类跳 `nodes`；同时后端错误文案去掉 `**`（或前端 `pre-wrap`）。
5. panic.log 引导按「App 起不来 / 能起来」两层落（U4）。

---

## 七、复核基准（行号怎么复算）与工作树里的并行改动

**本文所有 `文件:行` 都按 `0e09214`（HEAD）复核** —— 逐条可复算，例如：

```sh
git show 0e09214:apps/ui/src/pages/Intent.tsx | sed -n '453,460p'   # U1 的「生效」三态
git show 0e09214:apps/ui/src/pages/Intent.tsx | sed -n '605,608p'   # U2 的页面注释
git show 0e09214:apps/desktop/src/supervisor.rs | sed -n '459,462p' # U3 的门禁文案
```

关键文件在 `0e09214` 的 blob（拿到它就能确认行号没漂）：

| 文件 | blob |
|---|---|
| `apps/ui/src/pages/Intent.tsx` | `aec47ace24f6f67caa353fe4213a0ab7aa46677f` |
| `apps/ui/src/pages/Dashboard.tsx` | `874bd6c97494009b84bc52f800bc64129652ed2c` |
| `apps/ui/src/App.tsx` | `850eb3caad3130a63357fc31128476a96b5f2296` |
| `apps/ui/src/topbarStatus.ts` | `62e4f43bbc04dcde526611a046a73548b569b6b9` |
| `apps/desktop/src/supervisor.rs` | `3eff4e76f7d2742a193b384456953be147f4f3c2` |
| `apps/desktop/src/mitm.rs` | `69dff3775d97ccdfa61ad356bbfdd0ff98652b02` |

**工作树里有并行改动（写报告时观察到的）**：`git status --short` 显示 `apps/ui/src/{App.tsx,ipc.ts,pages/Dashboard.tsx,pages/Intent.tsx,store.tsx,styles.css,topbarStatus.ts}` 已修改，并新增 `apps/ui/src/failure.ts`。从改动内容看，它们正冲着本文的 U1/U3/U6/U8 去（新增 `mitmGates()` 的三道闸门 ✓/✗/？ 列表、`failureAdvice()/nextSteps()` 的「原因 + 下一步（含重装助手）」）；但 `Intent.tsx` 里 `引导规则已随核心生效`（当前工作树 `:702`）、`` `applied=false` `` 注释（`:851`）、表头 `生效`（`:864`）**仍在**。

⇒ 本文按**已提交的 main**列问题；那批未提交改动我**没有验收**（不属本任务范围）。若它们落地，请用上表 blob 对齐后重新核对 U1/U3/U5/U6/U8 是否已消除。

---

## 我没能验证的

- **没跑真机 GUI**：本会话只有文件系统与仓库，没有 macOS 桌面、没有装上 helper、没有真实的 `security(1)`/钥匙串。所有渲染结论来自**读源码 + 跑既有渲染断言**（`vitest run src/intentPage.test.tsx` → 12 passed），**没有**在真实窗口里逐条对照像素与换行折叠效果。特别是「`.banner` 里 `\n` 是否真的坍缩成空格」是从 `styles.css:571-583` 没有 `white-space` 设置推断的，未在浏览器里量。
- **没有跑后端**：`MitmStatus.core_steering=null` 时界面显示「已随核心生效」是从 `mitm.rs:252-255` 与 `Intent.tsx:453-460` 的代码路径推出来的；我**没有**构造一个「装了证书、起了代理、从未连过核心」的真实会话去看那一格。同理，v0.8.38 那种「旧 helper + 新 App」的现场报错原文只是从 `supervisor.rs:1104` + `helper_client.rs:300-343` 推断，未在真机复现。
- **没验证发布说明是否随发行物到达用户**：`docs/release-notes/v0.8.39.md` 里有 `logs/panic.log` 的路径，但仓库里没有发行流程的证据说明用户在哪看到它（`site/` 下无发布说明页，`scripts/package-macos.sh:199-201` 只校验核心/geo 资源）。所以「用户能否在崩溃后读到这条路径」我无法确认。
- **没验证崩溃后 UI 的表现**：App 启动即崩时窗口是否闪现、是否有系统弹窗，都没有实机观察；U4 的建议基于「panic → abort → 进程消失」这一 release profile 事实（`docs/release-notes/v0.8.39.md:9-14`），未实测。
- **未数真机上的审计行数**：报告里没有引用任何「拦截了多少」「误杀多少」的实测数字；U5 中的 `N` 是占位符，不代表本机数值。
- **未跑 `scripts/check.sh` 与全量 vitest**：只跑了 `src/intentPage.test.tsx` 一个文件（12 条）。其它页面的文案未在本次范围内逐条走查。
- **未验收工作树里那批未提交的并行改动**（新增 `failure.ts`、`mitmGates()` 等，见 §七）：它们不在 `0e09214` 里，我没有审它们的正确性，也没有跑它们引入的测试文件（`failureHonesty.test.tsx` 等）。本文的问题清单因此**只对已提交的 main 有效**。
