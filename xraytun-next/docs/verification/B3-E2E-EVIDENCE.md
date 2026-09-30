# B3 · 真 xray 端到端证据（backend-3 / task-6 / task-9）

> 证据分级：下面每一条都是**本机真跑**的输出，命令与退出码原样保留。
> 被测路径上没有 mock：真 HTTP 源站、真 xray 服务端（环回 vless + freedom）、
> 真 xray 客户端进程、真 AF_UNIX 帧、真 StatsService 字节数。

## 0. 环境

| 项 | 值 |
| --- | --- |
| xray | `Xray 26.3.27 (Xray, Penetrates Everything.) d2758a0 (go1.26.1 linux/amd64)` |
| 二进制 | `/Users/xbtg-/deepseek-harness/.scratch/bin/xray`（可用 `XT_XRAY_BIN` 覆盖） |
| target | `CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-targets/backend-3` |
| 节点形态 | `vless://<uuid>@127.0.0.1:<port>?encryption=none&type=tcp&security=none#n1`（两个 uuid，同一个真 xray 服务端） |
| 服务端形态 | `vless` inbound（clients=2，decryption=none，tcp）+ `freedom` outbound，只监听 127.0.0.1 |

## 1. `cargo test -p xt-daemon --test e2e_real_xray -- --nocapture`

`exit=0`，`test result: ok. 1 passed; 0 failed; ... finished in 10.79s`。

关键原始输出（一次完整运行，`e2e_exit=0`）：

```
[e2e] 源站 http://127.0.0.1:22889（65536 字节固定内容）
[e2e] vless 服务端就绪于 127.0.0.1:22723
[e2e] daemon 1.0.0 pid=957401 能力=[ProxyMode, Stats, Probe, Subscriptions]
[e2e] 节点 dmxlc3N8MTI3LjAuMC4xfDIyNzIzfG4x / dmxlc3N8MTI3LjAuMC4xfDIyNzIzfG4y
[e2e] State stage=Connecting phase=Some(PreparingConfig) pid=None
[e2e] State stage=Connecting phase=Some(StartingCore) pid=None
[e2e][xt-daemon] 核心已启动 pid=957427
[e2e] State stage=Connecting phase=Some(AwaitingReady) pid=Some(957427)
[e2e] State stage=Connected pid=Some(957427)
[e2e] Connected pid=957427 version=Xray 26.3.27 (Xray, Penetrates Everything.) d2758a0 (go1.26.1 linux/amd64)
[e2e] SOCKS5 请求拿到 65536 字节
[e2e] stats uplink=79 downlink=131288 sampled_at_ms=1790674805286
[e2e] 切节点后 pid=957442，再次拿到 65536 字节
[e2e] 切换后 stats downlink=131288
[e2e] TailLogs 36 行，含核心输出
[e2e] Disconnected，pid=957442 已消失
[e2e] TTFB dmxlc3N8MTI3LjAuMC4xfDIyNzIzfG4x = 6ms
[e2e] TTFB dmxlc3N8MTI3LjAuMC4xfDIyNzIzfG4y = 5ms
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.79s
```

核心 xray 自己的日志（证明代理跳是真的）：`listening TCP on 127.0.0.1:10863`(socks) /
`10864`(api) / `dialing TCP to tcp:127.0.0.1:22723` / `proxy/vless/outbound: tunneling
request to tcp:127.0.0.1:22889 via 127.0.0.1:22723`。

### 断言与真实值

| 断言 | 真实观测 |
| --- | --- |
| connect 事件驱动就绪 | Connected 事件带 `pid` 与 `ready_at_ms`；就绪耗时 68–171ms |
| 真 SOCKS5 拿到全部字节 | 64 KiB 固定内容，逐字节 `assert_eq` 通过 |
| stats 是真实采样 | `uplink=79` / `downlink=131288`（socks 入站 + `node-<id>` 出口两路合计 ≥ 64 KiB） |
| sampled_at_ms 来自真实时钟 | epoch ms（> 1.7e12），非计数器 |
| 切节点换真进程 | `Connected pid=957427` → `SwitchNode` → `pid=957442`，旧 pid 的 `/proc/<pid>` 已不存在 |
| 切节点后仍能传 | 再次 `65536` 字节逐字节相等 |
| disconnect 后进程真的没了 | `/proc/<pid>` 不存在（pid 957442 已消失），`last_error` 为 `null` |
| TailLogs 是真日志 | 36 行里含 `target=xray.stdout/stderr` 的核心行 |
| probe 是真 TTFB | 6ms / 5ms（临时实例 + 每节点独立 socks 入站端口，靶点是本机源站） |
| 能力表 | `[ProxyMode, Stats, Probe, Subscriptions]`，不含 `TunMode` / `SubscriptionFetch` |

### 竞态回归（task-9，ux 独立发现）

api 入站与 socks 入站不是同一个就绪事件。修复后 `DatapathSpec.required_addrs` 把 api 也纳入
同一套事件驱动就绪判定，E2E 里追加连续 **3 轮 connect/disconnect，每轮都断言 stats 非 None**：

```
[e2e] 第 1 轮 pid=957469 uplink=79 downlink=131288
[e2e] 第 2 轮 pid=957485 uplink=79 downlink=131288
[e2e] 第 3 轮 pid=957503 uplink=79 downlink=131288
```

xt-datapath 单元回归：`ready_requires_every_required_addr_to_accept`（socks 通、api 未监听
→ 必须继续等到 deadline 并报 `DatapathUnavailable`，detail 列出未就绪地址）、
`ready_when_all_required_addrs_accept`（两个端口都可连 → 就绪）。

## 2. 有没有 sleep？

`grep -rn "sleep" crates/xt-daemon crates/xt-datapath crates/xt-stats crates/xt-probe` 的全部命中：

```
crates/xt-daemon/src/main.rs:5:            //! （等信号是事件，不是轮询，也没有 sleep）
crates/xt-daemon/tests/e2e_real_xray.rs:6: //! 无等待：所有等待都是「事件 + timeout 上限」
crates/xt-daemon/tests/e2e_real_xray.rs:295:  // 测试里不需要再写一遍「等端口」，更不需要 sleep。
crates/xt-datapath/Cargo.toml:7:          description = ".../停止，无 sleep"
crates/xt-datapath/src/lib.rs:256:        /// 等到核心**开始监听全部必需端口**：事件驱动，不轮询、不 sleep。
crates/xt-datapath/src/lib.rs:303:            _ = tokio::time::sleep_until(deadline_at) => break WaitOutcome::NotReady,
crates/xt-datapath/src/lib.rs:347:        /// 停止并 reap。SIGTERM → deadline → SIGKILL，全程没有 sleep。
crates/xt-datapath/src/lib.rs:831:        // `signal.pause()` 不用 sleep，且 SIGTERM 被显式忽略。
crates/xt-probe/src/lib.rs:200:              _ = tokio::time::sleep_until(deadline_at) => { ... }
```

**结论：关键路径无 sleep。** 仅有的两处代码是 `tokio::time::sleep_until(deadline)`，它们是
`select!` 里的**失败上限**分支（到点返回 `DatapathUnavailable` 并带未就绪地址），不是轮询周期；
其余命中都是注释/描述文字。E2E 与单元测试里的所有等待都是 `timeout(deadline, fut)` 包住一个
事件循环（`/proc` 探活、`/proc` 消失、事件到达）。

## 3. `xt-cli` 联调（真 daemon 二进制 + 真 xray + 真 curl 经 SOCKS5）

命令（完整脚本 `/tmp/xt-b3-joint.sh`，`exit=0`）：

```bash
cargo build -p xt-daemon -p xt-cli
<target>/debug/xt-daemon --socket $D/daemon.sock --state-dir $D --xray .scratch/bin/xray --log-level info &
<target>/debug/xt-cli --socket $D/daemon.sock hello|nodes|connect <id>|status|switch <id2>|probe <id1> <id2>|tail-logs 6|get-settings|patch-settings --log-level debug|get-settings|disconnect|status
curl --socks5 127.0.0.1:$SOCKS http://127.0.0.1:$ORIGIN/payload.bin -o out.bin
```

原始输出摘要：

```
capabilities: proxy_mode, stats, probe, subscriptions
connect n1  → stage: connected | pid: 957286 | stats: 未采样
status      → stats: up=0 down=0 @1790674793260          # 真实样本：还没流量，不是「未采样」
curl        → 接收字节: 65536 | BYTES-MATCH: ok
status      → stats: up=103 down=131492 @1790674793376   # 真字节数
switch n2   → stage: connected | pid: 957321 | stats: 未采样   # 新会话尚未采样
curl        → BYTES-MATCH: ok
status      → stats: up=103 down=131492 @1790674793524
probe       → 29 ms / 16 ms
tail-logs 6 → 6 行真实 xray 输出（target=xray.stdout）
get/patch   → log_level: info → debug（真落盘）
disconnect  → stage: disconnected | pid: 未运行 | stats: 未采样
进程证据     → NO-XRAY-PROCESS: ok（daemon 的 xray 子进程已消失）
SIGTERM     → daemon 已退出
```

## 4. 静态检查

```
$ cargo clippy -p xt-datapath -p xt-stats -p xt-probe -p xt-daemon --all-targets -- -D warnings
    Finished `dev` profile ...            # exit=0，无警告
$ bash xraytun-next/scripts/guard.sh
crates=31 个 Rust 文件, ui=17 个 TS 文件
违规=0 警告=0
GUARD PASSED                              # exit=0
```

### 4.1 收尾时发现并修掉的测试夹具缺陷（诚实记录）

最终验收时发现 `cargo test -p xt-datapath` **会永久挂住**（不是慢，是 `start()` 里的
`xray version` 探测永远不返回）：假核心脚本对任何参数都长驻。这是测试夹具缺陷，但它暴露了
一个真实的健壮性缺口。修复：

1. 夹具：假脚本先处理 `version` 并退出；脚本改为「写临时文件 → chmod → 原子改名」，
   消除多线程下 fork/exec 撞 `ETXTBSY (Text file busy)` 的 flake。
2. 代码：`read_version` 与 `validate_config` 都加失败上限（10s / 20s），超时 →
   `DatapathUnavailable`（不是 `ConfigInvalid`），`start()` 不会因为一个不认参数的二进制挂死。
3. `CoreExitedEarly` 的 detail：进程退出与读取任务收行是两个事件，加一次有上限的输出
   drain（500ms），保证错误里一定带真实最后几行；SIGKILL 测试改成等核心打印
   `term-ignored` 事件，不再赌信号处理函数是否已装好。

连跑 3 次结果：`xt-datapath 11 passed` / `xt-stats 6 passed` / `xt-probe 14 passed`，
0 failed，无挂起。

## 5. 未验证 / 边界

* **TUN 模式未实现**：`Connect{mode: tun}` 返回 `Unsupported`；能力表不宣告 `TunMode`。
  本轮任何「TUN 已工作」的说法都是假话。
* **远端订阅拉取未实现**：`AddSubscription` / `RefreshSubscription` 返回 `Unsupported`；
  节点只来自 `--subscription-file`（默认 `<state-dir>/subscription.txt`）。
* E2E 的探测靶点是本机源站（离线可跑）。默认联网靶点 `http://cp.cloudflare.com/generate_204`
  未在本次证据中真跑；daemon 只把它当 URL 使用，链路与环回靶点相同。
* `/proc/<pid>` 探活在 Linux 上成立；E2E 按契约只在本机 Linux 运行（macOS 需换 `kill(pid,0)`）。
