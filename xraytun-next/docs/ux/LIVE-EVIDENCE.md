# LIVE-EVIDENCE · 活体验收证据（ux / task-3）

> 本文只写**跑过的**。没跑成的、跑成一半的、以及「代码推断」的，分别标注。
> 证据来源：`/Users/xbtg-/deepseek-harness/.scratch/ux-live/*.log`（原始输出，未编辑）。
>
> 结论摘要：**活体验收真跑通了**（真 daemon + 真 xray + 真 AF_UNIX + 真 SOCKS + 真
> StatsService 字节）。首次发现一个**间歇性真实缺陷**（StatsService 与 xray api 入站的
> 就绪竞态，2/5 命中），已如实记录在 §3/§4；backend-3 按 lead 裁定修复后，我做了
> **独立 5 连跑复验：5/5 全部进入 stats 断言且真实字节 > 0**（§4.1，原始日志未删）。

## 0. 环境（实测值）

| 项 | 值 |
| --- | --- |
| 时间 | 2026-09-29 09:23–09:42 UTC（首次验收 09:23–09:35；竞态修复后复验 09:41–09:42） |
| 真 xray | `/Users/xbtg-/deepseek-harness/.scratch/bin/xray` → `Xray 26.3.27 (… ) d2758a0 (go1.26.1 linux/amd64)` |
| xt-daemon | `/Users/xbtg-/deepseek-harness/.cargo-targets/ux/debug/xt-daemon`。修复前 sha256 前缀 `da5ab2dac554c70dcc2b52db4185fe6a`；**竞态修复后 `5775f550d576dac5a4727dc21833b0e5`**（mtime 09:41:32，晚于 `xt-datapath/src/lib.rs` 09:38:23 与 `xt-daemon/src/flow.rs` 09:38:28，无旧二进制问题） |
| node / vitest | `v24.21.0` / `vitest 3.2.7 linux-x64` |
| 测试文件 | `apps/ui/tests/acceptance/live-daemon.test.ts`（jsdom 环境，`XT_LIVE=1` 门控） |

## 1. 命令（逐字）

```bash
export RUSTUP_HOME=/Users/xbtg-/deepseek-harness/.rustup
export CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-targets/ux

cd /Users/xbtg-/deepseek-harness/xraytun-next
cargo build -p xt-daemon          # 冷编译 13m29s；协议修复后增量 39.55s；竞态修复后增量 24.82s；三次 exit=0

cd apps/ui
export XT_DAEMON_BIN=$CARGO_TARGET_DIR/debug/xt-daemon
export XT_XRAY_BIN=/Users/xbtg-/deepseek-harness/.scratch/bin/xray
XT_LIVE=1 XT_SOCKET=/tmp/xraytun-live-ux.sock npx vitest run tests/acceptance/live-daemon.test.ts
```

测试内部自己拉起：本地 HTTP 源站（真 socket）、环回真 xray 服务端（vless inbound +
freedom outbound）、订阅文件、真 `xt-daemon`（`--socket/--state-dir/--xray/--log-level/
--subscription-file`）。SOCKS 部分由测试手写的 SOCKS5 客户端完成（不引第三方包）。

## 2. 通过的活体运行（证据）

日志：`/Users/xbtg-/deepseek-harness/.scratch/ux-live/live-pass.log`
（= `live-evidence-attempt-1.log`，2026-09-29 09:34）。命令 exit code **0**，
`Test Files 1 passed (1) / Tests 1 passed (1)`。

原始观察（逐字从日志取）：

```
[live] 真实事件流 stage：connecting → connecting → connecting → connected → connected
[live] 界面渲染 stage：unknown → disconnected → connecting → connected
[live] 经 daemon SOCKS 实际收到：65536 字节（期望 65536）
[live] StatsService 真样本：uplink=80 downlink=131362 sampled_at_ms=1790674462385
[live] 界面 stats-downlink 文本：128.3 KiB（131362 字节）
```

这一行回答了本任务 D 的两条要求：

1. **事件流真的驱动界面**：`Event::State` 序列里 `connecting` 出现在 `connected` 之前；
   React 实际提交的渲染序列是 `unknown → disconnected → connecting → connected`
   （探针在每个 Provider 状态提交时记录，不是靠 `sleep` 猜的）。
2. **`stats` 真的来自 StatsService**：经 daemon 的 SOCKS 入站向环回源站取回 65536 字节，
   `Request::Status` 触发一次真实采样，`downlink_bytes=131362 ≥ 65536`，界面显示
   `128.3 KiB（131362 字节）`（`downlink` 大于单次请求量是因为 StatsService 计的是
   入站+节点出口两路，backend-3 已说明）。

> 注：日志里有两处 `An update to DaemonProvider inside a test was not wrapped in act(...)`
> 警告。这是 React 的测试提示，不是失败；断言全部通过。它来自真实事件在
> `waitFor` 轮询间隙到达，不是被测代码的问题（第 4 节把它列进「未解决但不影响结论」）。

## 3. 失败过的地方（如实记录，含被排除的原因）

| # | 时间 | 结果 | 原因（证据） | 归属 |
| --- | --- | --- | --- | --- |
| L1 | 09:23 | 红 | `写帧失败：write EPIPE`。根因：我的二进制构建于 09:22:51，而 lead 修 `Response::Nodes` 的 serde 缺陷是 09:23:45（`protocol.rs` mtime）。旧二进制上 `list_nodes` 序列化运行时失败 → daemon 直接断开连接。**重编译后消失**。 | ux 构建时序（已解决） |
| L2 | 09:31 | 红 | `[step=ui-disconnected] 徽章变化序列：未连接` —— 我用 jsdom `MutationObserver` 记录阶段序列，`waitFor` 命中后立即 `disconnect()` 把尚未派发的记录丢掉了。这是**测试仪器**的问题，不是界面。改为 React 渲染探针后解决。 | ux 测试仪器（已解决） |
| L3 | 09:31 | 红 | `[step=socks-request-done] status.stats 缺失`，daemon 日志：`WARN xt_daemon::flow: 连接 StatsService 失败：本次连接将显示未采样 error=io: 连接 api 入站 127.0.0.1:28364 失败: Connection refused`。 | backend-3 缺陷 → **已修复（§4.1）** |
| L4 | 09:33 三次连跑 | 红/绿/绿 | 同 L3：竞态。三次里 1 次命中。 | backend-3 缺陷 → **已修复（§4.1）** |
| L5 | 09:34 | 绿 | 证据运行（第 2 节）。 | — |

在**修复前二进制**上、进入 stats 断言的运行共 5 次：**3 次通过、2 次命中 L3 竞态**。
（这段历史保留，不因后来修好而删除。）

## 4. 独立发现（重要）：StatsService 与 xray api 入站的竞态

这是我在活体验收里真正钉出来的东西，不是 ui 或 frontend 的问题：

* `crates/xt-daemon/src/flow.rs:170` 在 `CoreReady`（**SOCKS 可连**）之后**一次性**
  `StatsClient::connect(prepared.api_addr)`；失败只记 warn，然后整段会话保持
  `stats = None`（符合 I2：不重试、不回落）。
* 但 xray 的 **api 入站**与 socks 入站不是同一个就绪事件。同一份二进制、同一份配置，
  有时 api 已在监听（那次能看到 h2 建连、`accepted tcp:… [api -> api]`），
  有时 connect 那一刻 api 还没 accept → `Connection refused (os error 111)`。
* 后果**不是**「把未知说成 0」（I3 的正面守卫是有效的），而是反向的：
  **明明能知道，却整段会话一直说「未采样」**，而真实字节已经发生。
  两种情形我都亲眼观测到了（同一二进制，L3 与第 2 节）。
* 已发消息给 backend-3，建议把「api 入站可连」并入同一个**事件驱动的就绪判定**
  （每读一行 xray 输出试一次，直到 connect deadline；仍无 sleep/轮询，也仍不是失败回退）。

**修复前**，`stats` 显示的「未采样」不能被解释为「确实没采样成功」——
它可能是竞态输掉，而不是事实。这句话本身就是本验收要保护的东西。
（修复后这条约束仍成立：只有 `stats == undefined` 才能显示「未采样」，见 §4.1 的复验。）

### 4.1 修复与独立 5 连跑复验（task-9 → task-10）

**修复（backend-3，按 lead 裁定）**：`xt-datapath::DatapathSpec` 增加 `required_addrs:
Vec<SocketAddr>`，就绪 = **socks + api 全部接受连接**；核心每输出一行就对「尚未就绪」的
地址各试一次 TCP connect，全部可连才返回 `ReadyInfo`；进程提前退出仍是 `CoreExitedEarly`；
deadline 只作失败上限。daemon 传 `required_addrs = [api 地址]`。仍无 sleep / 轮询 / 重试。

**我的复验（不采信修复者自述，自己重编译 + 自己跑）**：

* 先用修复后的源码重编译：`cargo build -p xt-daemon` → exit 0（增量 24.82s）。
* 核对无旧二进制：二进制 mtime `09:41:32` 晚于 `xt-datapath/src/lib.rs` `09:38:23`、
  `xt-daemon/src/flow.rs` `09:38:28`。
* 同一命令连跑 5 次（日志在 `.scratch/ux-live/reverify-run-{1..5}.log`）：

| # | 日志 | 时间 | exit | 经 SOCKS 收到 | StatsService 样本（真值） | 界面 stats-downlink 文本 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `reverify-run-1.log` | 09:41:46 | 0 | 65536 B | uplink=80 downlink=131362 sampled_at_ms=1790674906194 | `128.3 KiB（131362 字节）` |
| 2 | `reverify-run-2.log` | 09:41:53 | 0 | 65536 B | uplink=80 downlink=131362 sampled_at_ms=1790674913465 | `128.3 KiB（131362 字节）` |
| 3 | `reverify-run-3.log` | 09:42:00 | 0 | 65536 B | uplink=80 downlink=131362 sampled_at_ms=1790674920872 | `128.3 KiB（131362 字节）` |
| 4 | `reverify-run-4.log` | 09:42:08 | 0 | 65536 B | uplink=80 downlink=131362 sampled_at_ms=1790674928167 | `128.3 KiB（131362 字节）` |
| 5 | `reverify-run-5.log` | 09:42:15 | 0 | 65536 B | uplink=80 downlink=131362 sampled_at_ms=1790674935665 | `128.3 KiB（131362 字节）` |

**5/5 全部通过**：每次 exit=0、每次进入 stats 断言、每次 `stats` 非 `undefined` 且
`downlink_bytes=131362 > 0`。5 次的 `sampled_at_ms` 各不相同（真采样时刻），
说明不是缓存/写死值。5 次的 `connecting → connected` 事件序列与
`unknown → disconnected → connecting → connected` 渲染序列一致（run 2 多一条开头
`disconnected`，那是 daemon 在 `subscribe` 后补发的初始状态事件，属正常）。

复验期间**没有**再出现 `连接 StatsService 失败 / Connection refused`。我**没有**放宽任何
断言：`stats` 仍要求 `toBeDefined()`，`downlink_bytes >= 实际接收字节`（65536）仍是硬断言。

backend-3 自己也加了回归（`cargo test -p xt-daemon --test e2e_real_xray` 里连续 3 轮
connect/disconnect，每轮 stats 非 None；以及 xt-datapath 的两条就绪单测）。那是**其自述**，
我没有代跑它；我的证据是上面这 5 次我自己的运行。

## 5. 反向验证：truthfulness 的 5 条断言各自都能真的失败

机制：测试内置 `mutated(name, 真值, 破坏值)`，正常运行时永远返回真值；
`XT_TEST_MUTATE=<name>` 时故意注入与断言矛盾的破坏值。命令：

```bash
cd apps/ui
XT_TEST_MUTATE=<name> npx vitest run tests/acceptance/truthfulness.test.tsx
```

| 断言 | 突变名 | 被破坏的东西 | 观察结果 | exit |
| --- | --- | --- | --- | --- |
| A1 `stats` 缺失 → 「未采样」且无 0 字节 | `1-stats-present` | 给一个真实样本 | `1 failed \| 17 passed`；`× A1 … 显示「未采样」而不是 0 B` | 1 |
| A2 `connecting` → 连接按钮 disabled + 阶段逐字 | `2-stage-disconnected` | stage 改成 `disconnected` | `4 failed \| 14 passed`；四个 phase 用例全红 | 1 |
| A3 `last_error` 原样显示 | `3-error-dropped` | 抹掉 `last_error` | `1 failed \| 17 passed`；`× A3 …` | 1 |
| A4 字节/延迟可溯源（改注入值→界面变） | `4-bytes-wrong` | 注入 999999，断言仍期望 1234567 | `1 failed \| 17 passed`；`× A4 …` | 1 |
| A5 无证据性话术（文本扫描） | `5-forbidden-injected` | 往扫描文本注入「已为你切换到最快的节点」 | `1 failed \| 17 passed`；`× A5 …` | 1 |

**A4 的第一次实现是恒真的**（断言里用了被突变的变量，自己跟自己一致 → 突变时仍绿）。
我把它改成「期望值固定常量、只有注入值受突变影响」，重跑才变红。这条自我更正写在这里，
因为「测试能失败」本身也是需要证据的。

补充：A5 的扫描器有一段**常驻自检**（对每个禁语构造 `前缀+禁语+后缀`，要求扫描器命中），
所以它不是「空数组恒真」。

## 6. 常规验收（非 live）

| 命令 | 结果 |
| --- | --- |
| `cd apps/ui && npm test` | `Test Files 3 passed \| 1 skipped (4)`；`Tests 29 passed \| 1 skipped (30)`（live 默认 skip，不是通过） |
| `cd apps/ui && npx tsc --noEmit` | exit 0 |
| `bash xraytun-next/scripts/guard.sh` | `违规=0 警告=0 / GUARD PASSED` |

## 7. 我对 ui 实现的独立判断（哪些是真的、哪些还没证据）

**有证据为真（我亲自注入/真跑过）**

1. `stats == null` → 「未采样」，且整页无 `0 B`；真实 0 采样时显示真值 `0`（A1 正反两例）。
2. 四个 `phase` 的 `phase-label` 与 `phase-code` 逐字一致；`connecting` 时连接按钮 disabled。
3. `last_error` 的 `code`+`message` 原样渲染，徽章不被错误块替换。
4. 字节数/延迟可溯源：改注入值界面跟着变（A4）；真 StatsService 的 131362 字节被渲染成
   `128.3 KiB（131362 字节）`（live）。
5. 禁语扫描：整树文本无「重试/自动恢复/已切换/已保护」等；扫描器本身可失败。
6. 能力宣告门控（lead 的规则 B）：`[]` → 无 stats 卡/无 probe/无订阅区；
   `subscriptions` 无 `subscription_fetch` → 只读区在、拉取入口全无；有 `subscription_fetch`
   → 添加/刷新入口出现；`tun_mode` 有/无 → `run-mode-select` 有/无；`hello=null` → 全部不渲染。
7. 导航恰好 4 项，页面正文不含 TUN/拓扑/地球等未实现入口。
8. 空态「暂无节点」「暂无日志」逐字存在。
9. live：真 AF_UNIX → 真事件流 `connecting → connected` → 真 UI 渲染 → 真 SOCKS 65536 字节
   → 真 stats 字节 → 经 UI 断开回「未连接」。

**还没证据的（不写成「通过」）**

1. **`stats` 的长期正确性**：竞态已修复，5/5 复验通过（§4.1）；「界面如实显示未采样」
   的**渲染逻辑**与**数据链路**现在都有证据。**但仍限定**：5 次都在同一台机器、同一天、
   同一 xray/daemon 二进制、同一环回服务端；跨机器/跨网络/长时运行没有证据。
2. **Tauri 生产传输**（`src/transport/tauri.ts`）：本轮无真机/Tauri 环境，一条都没跑过；
   live 用的是 `unixSocket.ts`。ui 的界面逻辑因此是「在真实事件源上验证过的」，
   但「在打包应用里也成立」没有证据。
3. **daemon 未宣告 `probe`/`tun_mode`/`subscription_fetch` 的真实运行时**：能力门控是用
   注入的 hello 验证的（真 daemon 确实只宣告 `proxy_mode/stats/probe/subscriptions`，
   与注入路径一致，但「未宣告时真实运行时入口消失」我只在注入路径上断言过）。
4. **React `act(...)` 警告**：live 里有两处，来自真实事件在测试轮询间隙到达。它不影响断言，
   但我没有消除它，也没证明它在真机浏览器里对应任何问题。
5. **旧版 UX 遗产的回归**：`IA.md`/`INTERACTION.md` 里引用的旧仓库教训（如「两个断开按钮」）
   只在文档层面映射，没有为旧版行为做逐条对照测试。
6. **daemon 的就绪修复本身**：我用 5 连跑证明了「我这条路径上竞态不再出现」，
   但**没有**独立复跑 backend-3 的 `e2e_real_xray` 多轮断言，也没有对
   `required_addrs` 的单测做变异/反向验证——那是 backend-3 的验收面，不是我的。

## 8. 结论

* 本任务 D 的活体验收：**真跑过、有真字节证据、exit=0**（第 2 节）。
* 本任务 E 要求「没跑成也要如实写」：跑成了；同时也如实写出**两次失败尝试**与一次
  **真实竞态缺陷**（第 3/4 节，历史保留）。
* 独立发现的竞态：**已修复，独立 5 连跑 5/5 通过**（§4.1）。
* truthfulness 的 5 条断言：**每条都做过反向验证并确实变红**（第 5 节）。
