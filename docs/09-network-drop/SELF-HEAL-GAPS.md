# 网络断开 · 自愈机制覆盖矩阵与盲区（task-39）

> 症状（用户原话）：**「断开或退出应用后网络就恢复了」** + **「国外可以，国内直接断掉」**。
> 本文只读代码 + 现有测试，**不改源码、不改测试**。每条结论给 `文件:行号`；
> 「代码里读到的」与「我推断的」分开写。所有行号基于本轮工作区版本。

---

## 0. 结论：用户为什么必须手动断开？

**一句话可证伪的解释：**

> 自动路径里**唯一**能停止这条机器的开关是 `settings.was_connected`；
> 而**只有用户手动 `stop_proxy` 会把它清成 `false`**。
> 看门狗与自动重连都以它为门槛，所以「重建了但根本走不通」的隧道会被判定为
> **恢复成功**并再次接管默认路由 —— 这个环只能由手动断开打断。

拆成四步（每步都可单独证伪）：

1. **重建的「成功」判据里没有任何一次真实代理请求。**
   `commands/core.rs:509-526`：
   ```rust
   if stop_core(&handle, &state).await.is_ok()
       && start_core(&handle, &state).await.is_ok() {
       i.runtime.recovery.succeeded(...);
       state.log("app", "info", format!("隧道已自动恢复（第 {attempt} 次自动重建）"));
       ...
       return;   // start_core 会 spawn 新的看门狗
   }
   ```
   `start_core` 的全部判定是：helper 建 TUN（`supervisor.rs:257-262`）、拉起核心（`supervisor.rs:293-294`）、
   **`wait_for_port` 只 connect 127.0.0.1:port**（`crates/xt-core/src/xray/process.rs:203-215`）、
   提交前直连 TCP 到节点（`supervisor.rs:316`）、`CommitRoutes`（`supervisor.rs:328`）、
   提交后再直连 TCP 到节点（`supervisor.rs:337`）。
   **一步都不经过 SOCKS，也不发 HTTP。** 所以「TCP 通但 REALITY/TLS 被墙 / 数据面不通」
   的隧道会被判为**恢复成功**，并写日志「隧道已自动恢复」。
2. **`start_core` 成功后会再 spawn 一整套监控 → 新看门狗**（`commands/core.rs:85`、`:120`、`:390-403`）。
   新看门狗 32 秒后发现探测又失败（见 §1），于是**再重建、再宣告成功**。
   每一圈里网络只在 `stop_core` 回滚的那两三秒是通的（`CORE_SHUTDOWN_GRACE = 3s`，`supervisor.rs:43`）。
   ⇒ 表现就是「一直显示已连接/正在自动恢复，但整机一直上不了网」。
3. **唯一能停住这个环的是 `was_connected=false`**：
   看门狗门槛 `watchdog_should_watch(wants, …)`（`commands/core.rs:294-300`，调用点 `:458-460`）
   和探测回来后的意图复查（`:479-483`）都只认它；
   而自动重建/切换节点这些「过程」**刻意不清**它，`stop_proxy` 的注释把这点写死了：
   `commands/core.rs:18-33`「**用户主动停止 —— 这是唯一会清掉「该连着」的地方**」。
4. **手动断开还多做两件事**，自动退回直连一件都不做（见 §3 逐行对比）：
   `stop_core(...).await?`（错误冒到 UI）+ `was_connected=false` + 持久化 + `runtime=default()`。

**两种读法都能被这条解释覆盖，而且都能用现场日志区分（关键取证）：**

| 读法 | 机制 | 判别日志 |
|---|---|---|
| A：隧道**整体**走不通（境外也走不通） | 看门狗判定失败 → 重建 → `start_core` 的直连检查仍全过 → 宣告「已自动恢复」→ 循环 | 离线期间出现 `隧道已自动恢复（第 N 次自动重建）` |
| B：隧道**能走境外**、只有境内直连被黑洞 | 探测目标是**境外** 204（`crates/xt-core/src/xray/probe.rs:46` `http://cp.cloudflare.com/generate_204`）→ **探测一直成功 → 自愈根本不触发** | 离线期间出现 `隧道连通性检查通过（HTTP 204）` |

读法 B 里看门狗**一次重建都不会发起**（`tunnel_is_dead` 恒为 false），所以「必须手动断开」
也不奇怪：自动路径从未启动。**我无法从代码判断用户当时属于 A 还是 B** —— 需要那一行日志。

---

## 1. 时间线：从「接管默认路由」到「自愈真正动作」多少秒

| 时刻 | 事件 | 依据 |
|---|---|---|
| t=0 | `CommitRoutes` 接管默认路由，流量进隧道 | `supervisor.rs:328`（`deferred_commit` 分支；lead 引用的 `:319` 是同一分支里的提交前直连检查） |
| t≈2s | 连通性检查睡醒后发第一次**经 SOCKS**的真实请求（超时 10s） | `commands/core.rs:569`、`:593`（`tunnel_probe(port, 10)`） |
| t≈12s | 连通性检查的结论最早可能出来 | 2s + 10s |
| t=10s | 看门狗第一次探测（间隔 10s，curl `--max-time 6`） | `commands/core.rs:412`、`:462`、`:342-365` |
| t≈16s | 探测失败 #1 | 10 + 6 |
| t≈32s | 探测失败 #2 → **满足重建条件开始重建** | `FAILURES_BEFORE_REBUILD = 2`（`:923`）、`should_rebuild_tunnel`（`:333-335`） |
| t≈32s+ | 重建：`stop_core`（3s 宽限 + 回滚）→ `start_core`（等端口≤10s + 两次直连 TCP≤4s） | `supervisor.rs:43`、`:40`、`:49`、`:314-346` |
| 重建若失败 | 走「退回直连」，结束看门狗 | `commands/core.rs:530-543` |

* **完全断网的最短窗口 ≈ 32 秒**才是自愈的第一次动作；唤醒场景可缩短到 ≈16s
  （`slept_for > SLEEP_THRESHOLD(30s)` 时把失败计数直接拉满，`commands/core.rs:918`、`:426`、`:469-472`）。
* 看门狗**不带指数退避、也不带次数上限**，但它一轮只重建一次；成功则换新看门狗再来一轮（§0.2）。
* 连通性检查（t≈12s）是**最早的**真实路径探测，但它在「没有上一个可用节点」时**只提示、不回滚**
  （`commands/core.rs:611-625`，`if let Some(back) = fallback { … }` 之前就已经把隧道留在接管状态）
  —— 这是当前就存在的「把用户留在断网」分支，见 §5/S2。

---

## 2. 「退回直连」那一步逐行

`commands/core.rs:528-543`：

```rust
// 重建也失败：退回直连。用户至少能上网 …
let _ = stop_core(&handle, &state).await;                 // ← 530：错误被丢弃
state.with(|i| {
    i.push_log("app", "error", "自动重建失败，已退回直连：网络可用，但流量不再走代理");
    i.runtime.recovery.fell_back_to_direct(now);
    i.last_notice = Some("自动恢复失败，已退回直连：网络可用，但流量不再走代理。可在节点页重新连接");
});
events::runtime_changed(&handle, &state);
return;                                                   // ← 543：看门狗任务结束，不再重试
```

三个硬伤：

1. **`let _ =` 丢弃回滚结果（`:530`）**。而 `stop_core` 在拿到结果**之前**就把状态清干净了
   （`commands/core.rs:223-225` 先 `running=false / pid=None / tun_session=None`，
   `:226-232` 之后才看 `result` 并只在日志里写「停止过程中出错」）。
   ⇒ **路由/DNS 没回滚成功时，界面照样显示「未连接」，而机器还在断网**。
2. **用户提示会谎报**：无论回滚是否成功，`:535`、`:539-540` 都写「**网络可用**，但流量不再走代理」。
   用户据此认为「已经好了」，于是不会去断开。
3. **没有下一次**：`:543` 直接 `return`，看门狗任务结束；同一会话里没有任何东西会再试
   （`spawn_network_watch` 以 `runtime.running && pid` 为门槛，`commands/core.rs:688-693`，此刻已经退出）。

回滚失败的兜底**只有三条，都在 GUI 之外或要用户动作**：

* helper 侧快照仍在磁盘，**helper 下次启动**时 `recover_from_crash()` → `controller::restore_stale()`
  （`crates/xt-helper/src/server.rs:94-106`、`crates/xt-tun/src/macos/controller.rs:301-316`）；
  注意 `restore_stale` 只在 `snap.is_stale()` 为真时动作，且**helper 一直在跑就永远不会触发**。
* 「一键修复网络」`force_cleanup()`（`controller.rs:319`）—— 需要用户点。
* 退出 App：utun fd 关闭、接口消失，挂在它上面的路由随即失效（**这就是「退出应用网络就恢复」**的机制）。

回滚本身是**尽力而为、聚合错误**的：`controller::rollback`（`controller.rs:241-278`）
先按备份倒序还原 DNS，再倒序删 `installed_routes + pending_routes`，
全部成功才 `SessionSnapshot::clear()`，否则保留快照下次重试。GUI 侧 `rollback_tun` 只记日志
（`supervisor.rs:475-479`：「TUN 回滚失败，helper 侧快照已保留，下次启动会重试」）。

---

## 3. 手动断开 vs 自动退回直连：逐行对比

| | **手动** `stop_proxy`（用户点「断开」/退出） | **自动** fallback（看门狗重建失败） |
|---|---|---|
| 代码位置 | `commands/core.rs:17-33` | `commands/core.rs:530-543` |
| `stop_core` 结果 | `stop_core(&app, &state).await?` —— 失败直接返回给 UI | `let _ = …` —— **丢弃** |
| `was_connected` | `= false` + `persist_settings`（`:23-29`） | **保持 `true`** |
| `runtime` | `runtime = CoreRuntime::default()`（`:30-32`） | 由 `stop_core` 清 `running/pid/tun_session`；`recovery` 记 `DirectFallback` 结局 |
| 之后谁会再动网络 | **没有**：看门狗门槛、自动重连都以 `was_connected` 为前提 | 意图仍是「该连着」→ **下次启动**的自动重连（`RECONNECT_ATTEMPTS=24 × 5s = 120s`，`:991-993`、`:774-863`）会再试；UI 的「连接」按钮也在 |

**差异就是根因**：手动路径把**意图**清掉，自动路径只动了**状态**。
自愈的所有「还要不要守/还要不要连」判断都读意图（`:294-300`、`:445-447`、`:479-483`），
所以只有手动断开能让这台机器**停止再接管**。

---

## 4. 覆盖矩阵

机制缩写：**M1** 提交前检查 · **M2** 连通性检查（经 SOCKS，t≈12s） · **M3** 看门狗（10s/6s/连续 2 次） ·
**M4** 重建成功判据（`stop_core && start_core`） · **M5** 退回直连 · **M6** 开机自动重连（24×5s） ·
**M7** 换网检测（只报错） · **M8** 唤醒加速（1 次失败即重建） · **M9** helper 崩溃恢复（快照 restore_stale） ·
**M10** 节点回退（`last_good_node`） · **M11** UI 恢复状态

图例：✅ 覆盖 · 🟡 部分覆盖 · ❌ 不覆盖 · ⚫ 不适用

| 断连场景 | M1 | M2 | M3 | M4 | M5 | M6 | M7 | M8 | M9 | M10 | M11 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **S1 数据面整体死**（TCP 通、TLS/REALITY 被墙或节点转发不了） | ❌ | ✅ | ✅ | ❌ | 🟡 | 🟡 | ⚫ | ✅ | ⚫ | 🟡 | ✅ |
| **S2 仅境内/部分流量黑洞**（境外通、探测目标通） | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ⚫ | ❌ | ⚫ | ❌ | ❌ |
| **S3 DNS 指向已消失的隧道** | 🟡 | ✅ | ✅ | 🟡 | 🟡 | 🟡 | ⚫ | ✅ | 🟡 | 🟡 | 🟡 |
| **S4 helper 不在/被杀** | ❌ | ✅ | ✅ | ❌ | ❌ | ❌ | ⚫ | ✅ | ✅ | 🟡 | 🟡 |
| **S5 回滚失败**（helper 报错/超时） | ⚫ | ⚫ | ⚫ | ⚫ | ❌ | ⚫ | ⚫ | ⚫ | 🟡 | ⚫ | ✅ |
| **S6 换 Wi-Fi / 合盖唤醒** | ⚫ | ✅ | ✅ | 🟡 | 🟡 | 🟡 | 🟡 | ✅ | ⚫ | 🟡 | ✅ |
| **S7 开机时 Wi-Fi 未就绪** | ⚫ | ⚫ | ⚫ | ⚫ | ⚫ | 🟡 | ⚫ | ⚫ | ⚫ | ⚫ | ✅ |
| **S8 看门狗重建 vs 用户手动断开（并发）** | ⚫ | ⚫ | 🟡 | ⚫ | ⚫ | ⚫ | ⚫ | ⚫ | ⚫ | ⚫ | 🟡 |
| **S9 反复失败后彻底放弃** | ⚫ | ⚫ | 🟡 | 🟡 | ❌ | 🟡 | ⚫ | ⚫ | ⚫ | ⚫ | ✅ |

---

## 5. 「❌/🟡」每一格：最小复现 + 后果

> 复现手段分两类：**(R)** 真机/联网复现；**(U)** 单元/集成可造（但当前没有测试，见 §6）。
> 我不在本文里跑真机复现（会改网络配置），所以每条都注明是「代码推断」还是「已由代码结构确定」。

### S1-M1/M4 —— 提交前检查与重建判据都不看真实路径（**根因所在**）
* **复现（U）**：把节点的 TCP 端口放在一个「三次握手成功但拒绝转发」的假服务后面，
  或在真机上用一个被墙的 REALITY 节点：`wait_for_port`（`process.rs:203`）与
  `tcp_reachable`（`supervisor.rs:95/316/337`）**全部通过**，`CommitRoutes` 照常提交。
* **后果**：整机流量进黑洞。自愈 32s 后才可能反应；重建还会把它判成「已恢复」（§0）。
* **证据等级**：代码确定（两个检查的实现就是这么写的）。

### S1-M5 —— 退回直连可能「假成功」
* **复现（R）**：连续两次探测失败后，让重建的 `start_core` 也失败（例如把节点地址改成不可达），
  同时让 helper 在 `TunDown` 时返回错误（kill helper 或让它超时）。
* **后果**：`let _ = stop_core` 吞掉错误 → 日志/提示说「已退回直连、网络可用」，
  而路由/DNS 仍指向隧道；`return` 之后无人重试 ⇒ **用户必须手动断开或退出 App**。
* **证据等级**：代码确定（`:530`、`:223-232`、`:539-540`、`:543`）。

### S2（整行）—— 「境外通、境内断」这种**部分**黑洞完全不在自愈视野内
* **复现（R）**：在能访问 `cp.cloudflare.com` 但境内目标被黑洞的环境（正是用户症状）下连接。
* **后果**：`tunnel_is_dead` 只看一个**境外** 204（`probe.rs:46`），恒判「活着」→
  **M3/M4/M5 一步都不启动**；M2 也会「通过」。用户唯一出路是手动断开。
* **补充**：M7（换网检测）只打印「请断开后重新连接」并 `return`（`commands/core.rs:700-708`），
  **不重建**（与 `spawn_monitors` 上方注释「变了就重建」不一致 —— 实际重建靠看门狗兜）。
* **证据等级**：代码确定（探测目标与判据）；「用户是否属于这一读法」需要 §0 的那行日志。

### S3 —— DNS 哨兵：自愈能启动，但**重建**可能卡在解析上
* 探测侧**不依赖本机 DNS**：`curl --socks5-hostname`（`commands/core.rs:354`）把域名交给节点解析
  ⇒ 看门狗照常工作。**这一条是"覆盖"的**。
* 但重建侧要**本机解析节点地址**：`resolve_server_addrs` → `xt_core::net::resolve_host`
  → `(host,0).to_socket_addrs()`（`supervisor.rs:72-76`、`crates/xt-core/src/net.rs:16-37`）。
  如果上一次会话把 DNS 改成了隧道内哨兵且回滚失败，解析失败 →
  `build_tun_request` 报「无法确定代理服务器的 IP…」（`supervisor.rs:537-541`）→ `start_core` 失败 →
  走 S1-M5 的假成功路径。
* **复现（U）**：造一个「DNS 备份还原失败」的 helper 假实现，让 `resolve_host` 返回空。
* **证据等级**：代码确定（解析走系统 resolver；DNS 还原失败只记日志）。

### S4 —— helper 不在/被杀
* 依赖 helper 的步骤：`stop_core` 的回滚（`supervisor.rs:483` `Request::TunDown`）、
  `start_core` 的建 TUN（`supervisor.rs:257-262`）。探测（走核心）不依赖 helper。
* **后果**：helper 死 → 回滚不可能 → 看门狗只能「重建失败 → 假退回直连 → return」；
  联网能力要靠 helper **重启**（`recover_from_crash`）或 App 退出/一键修复。
* **复现（U）**：注入一个 `TunDown` 恒失败的 `TunUpOps` 假实现，断言 `stop()` 返回 Err；
  当前**没有**这样的测试（见 §6）。
* **证据等级**：代码确定。

### S5 —— 回滚失败没有任何进程内兜底
* 见 §2。**没有**「回滚失败就重试 N 次」「回滚失败就不许显示未连接」「回滚失败就退出 App 让它自然恢复」
  这类逻辑。`stop_core` 甚至会先清状态再报错。
* **证据等级**：代码确定。

### S6 —— 换网/唤醒
* 覆盖：看门狗 32s、唤醒后 16s。换网检测只提示不重建（`:700-708`）。
* **不覆盖**：换网导致的「旧网关路由 + 新网关」组合如果恰好让**境外**探测仍成功（S2 读法），
  仍然没有任何机制会发现境内流量坏了。

### S7 —— 开机时 Wi-Fi 未就绪
* `start_core` 失败 → 自动重连 24×5s=120s（`:774-863`）；用尽后只写日志/notice「请手动连接」
  （`:850-858`），**没有退回直连这一步**（此时通常也没接管路由，所以危害小）。
* **不覆盖**：>120s 才恢复网络（例如 Wi-Fi 5 分钟后才起来）时，重连已经停止，用户看到的是「未连接」，
  得自己点 —— 但与「断网」不同，不属高危。

### S8 —— 并发：看门狗重建 vs 用户手动断开
* 看门狗在探测前后各查一次意图（`:458-460`、`:479-483`），**能覆盖**「探测期间用户点了关闭」这一竞态。
* **不覆盖**：用户点「连接/切换节点」与看门狗重建同时发生时，`start_core` 的幂等守卫
  （`if supervisor.is_running()` 早退，`core.rs:56-86`）会把它当成「已经成功」，但**接管路由的是对方那次**
  —— 这条我没有找到测试，也没有现场数据，标为**未验证**。

### S9 —— 次数上限
* 看门狗：**无上限**，但一轮只重建一次；成功 → 新看门狗 → 再循环（§0.2）。
* 自动重连：24 次 ×5s。用尽后 `last_notice` 提示手动连接（`:850-858`）。
* **「彻底放弃、把用户留在断网」的路径有两条**：① 连通性检查在没有 `last_good_node` 时不回滚（`:611-625`）；
  ② 退回直连时 `let _ = stop_core` 失败（`:530`）且看门狗 `return`（`:543`）。

---

## 6. 现有测试没覆盖的分支（对照 `#[test]` 清单）

已覆盖（纯函数/状态机层面）：`tunnel_is_dead`、`should_rebuild_tunnel`、`watchdog_should_watch`、
`slept_for`、`should_auto_reconnect`、`monitor_guard_deduplicates_per_pid`
（`commands/core.rs:1021-1240`）；恢复**状态机**的成功/失败两个结局
（`state.rs:772-855`）；helper 的会话冲突自愈（supervisor 的 `session_conflict_is_healed_by_restore_and_one_retry`
等，`supervisor.rs:1124+`）；快照陈旧判定（`xt-tun/src/macos/snapshot.rs:172-215`）。

**没有测试的分支（按价值排序）：**

1. **「接管了默认路由、但真实路径不通」这个致命组合没有任何测试** ——
   也正是它没有被实现拦住。任何测试都无法让 `CommitRoutes` 失败，因为代码里没有这道门。
2. `spawn_connectivity_check`（`commands/core.rs:561-639`）**整个函数没有测试**；
   它的「没有 `last_good_node` → 只提示不回滚」分支（`:611-625`）是现成的「留在断网」路径。
3. 看门狗的**重建→退回直连**链路（`:509-543`）没有测试；`state.rs` 只测了状态迁移函数，
   没测「谁调用它、失败怎么办」。整个 `spawn_tunnel_watchdog` 是 `tauri::async_runtime::spawn` 里的
   闭包（`:407`），以 `AppHandle` 为输入 —— **当前结构下不可测**（这本身是个可测性缺陷）。
4. `rollback_tun_inner` 的 `TunDown` 失败分支（`supervisor.rs:481-488`）没有测试；
   现有假实现只覆盖「会话冲突 → Restore → 重试成功」。
5. `controller::rollback` 的部分失败聚合（`controller.rs:241-278`，DNS 还原失败/路由删除失败同时发生）
   没有测试（`controller.rs` 的测试只覆盖 `resolve_via`、参数校验、utun 名解析）。
6. helper `recover_from_crash` 的失败分支（`server.rs:94-106`，只 `tracing::error!`）没有测试。
7. `dns::restore` 失败（`dns.rs:168-179`）没有测试。

---

## 7. 如果只能加一个护栏，加哪个

> **把「经 SOCKS 的真实可用性探测（至少两个目标：一个境外 + 一个境内 IP）」变成
> `CommitRoutes` 之前的强制门槛 —— 失败就回滚并报错，绝不接管默认路由。**

具体位置：`apps/desktop/src/supervisor.rs` 的 `deferred_commit` 分支，
在 `:316` 的直连检查之后、`:328` 的 `Request::CommitRoutes` **之前**，
调用已有的 `tunnel_probe(port, 6)`（`commands/core.rs:342-365`；它已经用 `--socks5-hostname`，
不需要本机 DNS），要求返回 `204`；否则复用 `:330-332` 的回滚 + 报错路径。

为什么是它，而不是别的：

1. **它是唯一「在伤害发生之前」的一步。** 其余机制（M2/M3/M5）都只能在整机已经断网**之后**反应：
   最早 12s（且不回滚），看门狗 32s，并且重建的成功判据不含真实路径 → 会被判成「已恢复」并**再次接管**（§0）。
   在提交前拦住 = 断网时长为 0。
2. **两种读法都覆盖**：境外目标拦住 S1；**必须再加一个境内 IP 目标**才拦得住 S2（用户症状的读法）。
   两个都是 IP 字面量，避免 DNS 依赖（DNS 哨兵场景下也能测）。
3. **改动面最小、与现有风格一致**：`tunnel_probe` 已经存在且已被看门狗/连通性检查使用；
   只是把调用点前移到「提交前」，并把它从「事后探测」升格为「前置条件」。
4. **代价明确且可接受**：连接时多一次 HTTP 请求（~1-3s，已经有了 10s 的 `wait_for_port` 窗口）；
   探测目标可用固定 IP、失败重试 1 次以抗抖动。

**为什么不选其它候选（只列最诱人的两个）：**

* 「重建后先经 SOCKS 验证再宣告成功」—— 也能打断 §0 的环，但**用户仍要先经历 32s 断网**，
  且如果验证失败还要再回滚一次（多一次抖动）；它应该作为**第二道**，不该是唯一那一道。
* 「`let _ = stop_core` 改成把错误写进提示」—— 只是让谎报变诚实，**不产生联网能力**；
  属于顺带修（一行），但仍必须配合 §2 的「回滚失败要有人重试」。

---

## 8. 诚实清单（我没验证 / 需要现场数据）

1. **用户当时属于读法 A 还是 B 我没有现场数据。** 判别只需一行日志（§0 表格）：
   `隧道已自动恢复（第 N 次自动重建）` vs `隧道连通性检查通过（HTTP 204）`。
2. **我没有在真机上复现**「整机断网」（会改本机网络配置，超出只读范围）。本文所有机制结论
   来自代码阅读，已尽量给到行号；标注为「代码确定」的条目不依赖运行。
3. **`Egress::now()` 在断网时的行为未验证**（`commands/core.rs:694-696` 有「查不到默认路由是暂时的」分支）；
   如果黑洞期间它一直返回同一值，换网检测不会误报——这只是推断。
4. **S8 的「用户点连接 vs 看门狗重建」竞态**没有测试也没有现场数据，标为未验证。
5. **helper 被杀后 fd 的归属**：core.rs（handoff_fd 模式）拿着 dup 出来的 fd，
   「App 退出即恢复」是我按 `supervisor.rs:489-492` 关闭 fd + 内核语义推断的，未实测。
6. **重建/回滚的实测耗时**（3s 宽限 + 10s 等端口 + 两次 4s 直连）是上限，不是实测值；
   现场日志时间戳可以校准 §1 的表。
7. 本文**没有**覆盖「为什么境内会断而境外不断」的网络层解释（那是 task-38/40 的范围）。

---

## 附：行号索引（便于复核）

| 主题 | 位置 |
|---|---|
| 探测（经 SOCKS、境外 204） | `commands/core.rs:337-365`；目标 `crates/xt-core/src/xray/probe.rs:46` |
| 探测判死 | `commands/core.rs:265-267` |
| 看门狗循环/阈值/唤醒加速 | `commands/core.rs:405-546`、`:918`、`:923`、`:426`、`:469-472` |
| 重建（stop→start）与「成功」判据 | `commands/core.rs:509-526` |
| 退回直连（丢错误 + return） | `commands/core.rs:528-543` |
| `stop_core` 先清状态再看结果 | `commands/core.rs:206-240`（尤其 `:223-232`） |
| 手动断开（唯一清意图处） | `commands/core.rs:17-33` |
| 连通性检查（无 last_good 只提示） | `commands/core.rs:561-639`（`:611-625`） |
| 自动重连 24×5s 与放弃 | `commands/core.rs:774-863`、`:991-993` |
| 换网检测只报错 | `commands/core.rs:667-710` |
| 提交前检查 1：`wait_for_port` | `crates/xt-core/src/xray/process.rs:203-215` |
| 提交前检查 2 / 提交后检查：直连 TCP | `supervisor.rs:88-97`、`:314-348` |
| `CommitRoutes` | `supervisor.rs:328` |
| TUN 回滚（尽力而为） | `supervisor.rs:370-390`、`:474-495` |
| 快照回滚实现 | `crates/xt-tun/src/macos/controller.rs:240-316` |
| helper 启动回滚 | `crates/xt-helper/src/server.rs:94-106` |
| DNS 还原 | `crates/xt-tun/src/macos/dns.rs:160-180` |
| 节点地址解析（依赖系统 DNS） | `supervisor.rs:72-76`、`crates/xt-core/src/net.rs:16-37` |
