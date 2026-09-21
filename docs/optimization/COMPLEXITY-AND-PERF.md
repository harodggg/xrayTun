# 复杂度热点 + 流畅程度审计（task-56）

> 口径：**每条候选都给「现状证据（文件:行 / 实测数字）→ 更简单的替代 → 等价性怎么验证」**。
> 没有证据的条目一律进「我测不到的」。**本卡只读**，未改任何源码/测试。
> 排序公式：`(用户可感知收益 × 置信度) ÷ 风险`，不按发现顺序。

---

## 0. 方法与范围（先声明我量了什么、没量什么）

* 静态：Python 脚本扫全部 `crates/**` 与 `apps/desktop/src/**`（76 个 `.rs`、29,430 行）
  与 `apps/ui/src/**`（45 个 `.ts/.tsx`、12,553 行）：函数行数/分支数、`clone/to_string` 密度、
  跨文件重复字面量、调用点计数（脚本 `/tmp/scan-funcs.py`）。
* 动态：真浏览器（headless Chrome + `git archive HEAD` 快照 + `vite --host 127.0.0.1`）
  在**拓扑页**插桩统计每帧 DOM/几何调用与耗时（`/tmp/hotpath-measure.mjs`、`/tmp/hotpath2.mjs`）。
* **没跑任何 cargo**：`backend-dev` 正在 task-54/38/46、`frontend-dev` 在 task-55 编译，
  且卡里禁止 `check.sh`。⇒ 本文所有**后端结论是代码读数**，不是运行实测（每条已标注）。
* 已被别人认领、**本文不重复提**：`start_core` 门禁串行 12s（=`backend-dev` task-54）、
  日志 key 含下标重建 1500 行（=`frontend-dev` task-55）、`xattr -dr` 静默失败（=task-46）。
  本文只在需要时**引用**它们。

---

## 1. 排序表

| # | 候选 | 位置 | 现状（证据） | 更简单的替代 | 等价性怎么验证 | 收益 | 风险 |
|---|---|---|---|---|---|---|---|
| **1** | **回滚结果被丢弃 + 状态先清后判** —— 「已退回直连」可能是假的 | `commands/core.rs:206-240`（`:223-225` 先清状态、`:226-232` 后判 result）；`:530`（`let _ = stop_core(...)`）；`:539-540`（无条件写「网络可用」） | 回滚失败时：`running/pid/tun_session` 已被清空 → 界面显示「未连接」，而路由/DNS 可能仍指向隧道；看门狗 `return`（`:543`）后无人重试。task-39 已把这条链路钉死 | 用**显式枚举** `StopOutcome::{RolledBack, RollbackFailed(reason)}` 取代「先清字段再 `match result`」；失败时保留 `tun_session` 并把原因写进 notice，**不再断言网络可用** | 现有 `state.rs` 16 条状态机测试 + `snapshot.rs` 3 条；**新增**：注入恒失败的 helper → 断言 (a) `Err`、(b) `tun_session` 未被清、(c) notice 不含「网络可用」；**happy path 快照 JSON 字节级 diff 不变** | 用户可感知（不再被假恢复误导） | **中**：与 task-54 同区（`supervisor.rs`/`commands`），须与 `backend-dev` 协调 |
| **2** | **`build_snapshot` 每次调用 spawn 核心 2 次** | `commands/snapshot.rs:32`（spawn#1）、`:34` → `:567`（spawn#2）、`:620`（`Command::new(&path).arg("version")`） | `update_status` **只有 1 个调用者**（全仓 grep：`snapshot.rs:34`）；`build_snapshot` **25 个调用点**；同一次快照里同一个二进制被 `version` 探测两次，外加 `InstalledMeta::load` 每次读盘（`:566`） | `build_snapshot` 把已算好的 `CoreAvailability` 传进 `update_status(app, state, &core)`，内部只用 `core.version`；顺带把 `InstalledMeta` 也在同一层读一次 | 把纯逻辑抽成 `fn update_status_with(core_version: Option<String>, meta: InstalledMeta, current: &str, prev: UpdateStatus) -> UpdateStatus`：**这是无 AppHandle 的纯函数**，可直接单测；再断言 `u.core_version == core.version`；重构前后同状态 `serde_json::to_string(snapshot)` 字节级 diff | 每次 UI 操作少 1 次进程 spawn（25 个命令全受益）；顺带让这段**可测** | 低（单调用者、纯参数传递） |
| **3** | **动画每帧 `g.querySelector("rect")` × 车数** | `topology/Flow.tsx:503`（每车每帧一次）；`:385`/`:493`/`:496` 是同一循环 | **实测（当前 HEAD、11 辆车、60fps、6s）**：`querySelector` **662/s = 11.03/帧**；`setAttribute` 690/s = 11.5/帧（含 task-51 的 140ms 淡入）；`querySelectorAll` 60/s = 1/帧 | 在测量 effect 里缓存 `rectByKey`（或 `g.firstElementChild`），与既有 `spansByKey` 同样的做法 | 现有 **25 条**拓扑测试（含 task-36 身份/去程护栏、task-37 的 `data-arc` 落点护栏、回绕瞬时落位/淡入断言）全绿 → 位置不变；**注意：这 25 条里没有任何 `fill` 断言**（`grep -c fill topologyAnimation.test.ts` = 0）→ 本次重构应**顺手补一条 fill 断言**，否则「缓存 rect 引用」这类颜色的回归无人接住 | 省 662 次 DOM 查询/s（量级小） | 低 |
| **4** | **看门狗「判定→重建→回退」不可测**（142 行闭包，`tauri::async_runtime::spawn` 内） | `commands/core.rs:388-546` | task-39 §6：这条链路**没有任何测试**；`state.rs` 只测状态迁移函数，不测调用方 | 把循环体抽成可注入依赖的 `async fn watchdog_once(deps) -> Action`（deps：探测函数 + stop/start + 事件推送），保留 `spawn` 只做 IO | 抽取后：现有 `should_rebuild_tunnel`/`watchdog_should_watch` 单测不变 + **新增**「探测连续失败 → 重建被调用一次」「重建失败 → 回退被调用一次」「用户关断 → 不重建」三条假实现测试 | 收益是**可测性**（能挡住 #1、也更利于 task-54 重构） | 中（纯搬迁，但 142 行） |
| **5** | dev-only 大块：`preview.ts:66 installPreviewBridge` 260 行/35 分支、`previewProbe.ts:440 installTopologyProbe` 206 行/39 分支 | 两文件 | `installTopologyProbe` 的 `nearestOnPath` 每次调用 200 点粗采样 + 12 次细化；只在 `?preview=1` 生效 | 不重构（见 §3）；仅可把 dev 采样降到 64 点 | 探针输出与现在同数量级即可（它只服务开发） | dev 体验 | 低 |
| **6** | 长函数（**仅记录，不建议动**）：`supervisor.rs:374 start` 222 行/26 分支；`lib.rs:176 bootstrap` 153/24；`commands/core.rs:35 start_core` 170/12；`subscription/xray_json.rs:38 convert` 125/30 | 见左 | 见下 §3 第 1 条：这些的复杂度**换来的是回滚正确性/可测性** | — | — | — | 高 |

---

## 2. 顺带用数字否掉的两个假设（避免后人白干）

* **「拓扑每帧重算几何」不成立。** 实测 `getTotalLength` 只有 **5 次/秒（0.08/帧）**——
  几何分段（`spansByKey`）与路径长度是**每次测量 effect 才重算**，不是每帧。
* **「每帧 getPointAtLength ×车数」是真的，但代价很小**：**11 次/帧（恰好 1 次/车）= 660/s，合计 36.1 ms/秒**
  （≈单核 3.6%）；6 秒内 **0 个 longtask**。栈归属 400/400 全部来自 `step`。
  测量注意：另一版脚本在收尾时自己调了一次 `__topologyProbe()`，读到的是 **45.36/帧** —— 那是**我探针的开销**，不是动画的；**以栈归属的 11/帧 为准**。
  ⇒ 不值得为此手搓路径采样（见 §3 第 2 条）。

---

## 3. 「不该简化的」（看过并判定保持原样）

> 搜过的范围：`crates/{xt-core,xt-tun,xt-helper,xt-proto}`、`apps/desktop/src/**`（全部 `fn`，
> 1,150 个 Rust 函数；>100 行 16 个）、`apps/ui/src/**`（110 个 TS 函数/组件；>100 行 12 个）、
> 以及跨文件重复字面量扫描（`10808` 16 文件、`networksetup` 9 文件、`generate_204` 3 文件等）。

1. **`supervisor.rs:374 start` 的两阶段启动与提交前后两道检查**（222 行 / 26 分支）。
   复杂度换来的是「默认路由接管前后各验一次可达性 + 每一步失败都回滚」。
   注释（`:303-313`）明说第二次检查是防**路由环**的关键。**顺序本身就是正确性**，不要合并/提取。
2. **`getPointAtLength` 采样不要换掉**（`Flow.tsx:441` 附近）。
   实测 3.6% 单核、0 长任务；而「自己按折线采样」正是本项目 B1（guide≠可见线）与
   `span=total` 一类回归的温床 —— 收益小、风险大。
3. **`tunnel_probe` 用 `--socks5-hostname`**（`commands/core.rs:354`）：域名交给**节点**解析，
   这样一次探测同时覆盖「能不能转发」与「节点侧能不能解析」。改成本地解析会**改变语义**
   （且与 DNS 哨兵场景强相关）。
4. **`events.rs:51 runtime_changed` 的 `RuntimePayload`**（只有 runtime+traffic，不是全量快照）。
   注释（`:61-64`）解释了为什么下载进度要另开小事件：快照组装要读文件/问核心版本。
   这是**已经做对了的优化**，不要「统一成一个事件」。
5. **`TunUpOps` trait**（`supervisor.rs:398`）：注解写明「抽成 trait 的唯一目的是让自愈链路可测」。
6. **`MonitorGuard` + 按 pid 去重**（`commands/core.rs:390-403`）：防多个看门狗互相打架，
   有单测（`monitor_guard_deduplicates_per_pid`）。
7. **`slept_for`（单调 vs 墙上时钟差）**（`:313-315`）：不引系统 API 检测「睡过了」，
   唤醒场景把恢复从 ~30s 压到 ~10s；有单测。
8. **`was_connected`（意图）与 `runtime.running`（观测）分离**：这是**刻意**的
   （`:438-447` 注释：核心自己死时 `running=false`，看它会导致看门狗退出、彻底卡死）。
   task-39 的根因是「自动路径**没清意图**」，不是「不该有意图」——**不要合并这两个概念**。
9. **helper 会话快照 + 启动 `restore_stale`**（`xt-helper/src/server.rs:94-106`、
   `xt-tun/src/macos/controller.rs:301-316`）：崩溃安全的唯一依靠，`rollback` 的「尽力而为 +
   失败保快照」也是刻意的。
10. **`default_socks_port()` 已集中**（`model.rs:780`）。其它 16 个文件里的 `10808` 全是
    测试/示例/文档字面量 —— **不要为了「去重」把测试里的期望值也换成常量**，
    那会让「默认值变了没人知道」这件事失去独立断言。
11. **`data-arc`**（`Flow.tsx:496`）：它只表征「意图」，task-49 证明它抓不到滑行漂移；
    但它是回绕/身份护栏的落点，**删掉会让那两条护栏失去依据**（要补的是「渲染落点」指标，不是删它）。

---

## 4. 我测不到的（诚实清单）

1. **没跑 cargo / 没跑 check.sh**（同事在编译；卡也禁止）⇒ 第 1、2、4 条的时间收益**未经实测**，
   只有代码路径与调用点计数；「少一次进程 spawn」的实际毫秒数**不知道**。
2. **没有真机 Tauri 环境**：`build_snapshot` 的 spawn/读盘耗时、25 个命令真实调用频率、
   `runtime_changed` 真实载荷字节数都未测。前端 IPC 载荷只看到字段形状（`AppSnapshot` 含
   settings/subscriptions/nodes/runtime/latency/traffic/…），**没有字节数**。
3. 动画数字来自 **headless Chrome + `?preview=1`**（11 辆车、1280×900、假后端）；
   真机 WKWebView 的合成/刷新时序不同；`47.5 ms/s` 里含我 wrapper 的两次 `performance.now()` 开销，
   且 headless 的 GPU 路径与真机不同 —— **只可用于「相对比较」**。
4. **单调用者抽象扫描无效**：我的文本计数脚本得出 0 个，明显是方法不对（没有做 AST/作用域分析），
   所以本文**没有**基于它下任何结论。
5. 第 5 条（dev-only 大块）只做了行数/分支统计，**没测**探针的实际开销。
6. 视觉/CSS/交互/信息架构类复杂度不在本卡（task-57/58/59）。
7. 「回滚失败」的**真实发生率**未知 —— 我只能证明代码允许它被静默（task-39），
   不能证明用户本次断网就是它导致的。

---

## 附：原始产物

* 静态扫描：`/tmp/scan-funcs.py`（函数行数/分支/clone 密度）
* 每帧插桩：`/tmp/hotpath-measure.mjs`（DOM/几何调用计数）、`/tmp/hotpath2.mjs`（栈归属 + 耗时）
* 被测快照：`git archive HEAD`；**当前 HEAD `d39d75f`，`Flow.tsx` sha256 `c522e644be0926bb…`**。
  审计期间 task-51 落地（回绕改为「瞬时落位 + 140ms 淡入」），所以同一指标我在**两个修订**上都量过。
* 每帧读数（当前 HEAD、11 辆车、4–6 秒）：`getPointAtLength = 11/帧`（660/s，**栈归属全部是 `step`**，36.1 ms/s）；
  `querySelector = 11.03/帧`、`setAttribute = 11.5/帧`、`querySelectorAll = 1/帧`、`getTotalLength = 0.08/帧`；`longTasks = 0`；`guideLens = [2325, 2268, 2325]`
* 早期修订（`Flow.tsx 17e9a832200a4b41…`）：同一指标 11/帧、47.5 ms/s —— **结构相同，只有成本差异**（含机器噪声）。
