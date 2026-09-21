# 反证验证：尝试推翻本轮每一项修复（task-66）

> 角色：**反证者**。不采信作者的报告与作者的测试，自己构造场景去打。
> 每条都写「**我构造的攻击场景**」+ 原始输出/数字；**「已确认正常」不算交付**。
> 本卡**未修改**任何已有测试或源码：攻击在 `/tmp` 副本里做，仓库只新增本文件与
> `apps/ui/src/waveAcceptance.test.tsx`。

## 0. 口径（按 lead 的纪律修正）

| 口径 | 命令 | 结果 | 基线修订 |
|---|---|---|---|
| **验收（隔离 HEAD worktree）** | `git worktree add -f /tmp/xt-tester HEAD` → 在该 worktree 跑 `npx vitest run` | **140 passed + 1 todo（12 files）** | `bdeee46` |
| 隔离 HEAD + 本次新增测试文件 | 把 `waveAcceptance.test.tsx` 拷进该 worktree 再跑 | **145 passed + 1 failed + 1 todo（13 files）**（那 1 条红 = 下面目标 5 的缺口） | `bdeee46` |
| 共享工作区（**只能当「此刻实时状态」**） | 在工作区跑该文件 | **6 passed** | 工作区（`ipc.ts` 有未提交改动） |

> 我上一条报的「155 passed」确实是带着别人在途文件的**实时状态**，不是验收口径 ——
> 隔离 worktree 上是 **140 + 1 todo**，与 lead 一致。此表以后者为验收数字。
>
> **补记（口径随提交变化）**：`140 + 1 todo` 是 **task-66 提交前**的口径。task-66 自己新增了
> 6 条测试（含那条**为自愈可见性缺口写的验收断言**），所以在 task-66 之后的 HEAD 上口径变为
> **152 条、其中 1 条红** —— 那条红正是 `【要求】probe_failures ≥ 1 时界面必须有可见信号`。
> interaction-designer 的 task-68 落地后它才转绿。⇒ **一张卡写下的红基线，成了下一张卡的验收标准**；
> 这也说明「验收数字必须与修订号绑定」，否则同一句话在两个提交上含义不同。

> **最新读数（修订 `5223a01`，隔离 worktree `/tmp/xt-t2`，本文件写定时）**：
> UI `vitest run` = **188 passed + 1 todo（16 files）、0 failed**；
> Rust `cargo test -p xraytun-desktop --lib` = **121 passed / 0 failed / 1 ignored**。
> 其中我 task-66 写下的那条**红**验收断言
> `【要求】probe_failures ≥ 1 时界面必须有可见信号` **已转绿** ——
> 即 task-68（`5223a01`）的修复在**提交修订**上被我独立复验通过：
> **一张卡写下的红基线，被下一张卡关掉了**（这一步不是我采信作者结论，是我自己在那条修订上重跑出来的）。

---

## 1. 结果总表

| # | 目标 | 我构造的攻击场景 | 结果 | 关键数字 |
|---|---|---|---|---|
| 1 | task-55 日志跳动 | ①头部序号不变（过滤/搜索）时故意改动可见内容位置；②真裁剪；③跟随开着时裁剪；④**锚点那一行自己也被裁掉** | **未能攻破**（①③④断言全绿；②补偿正确），④留下一个**残余缺口** | ①scrollTop 500→500；②500→**460**（补了 -40）；③500→500；④500→500（本帧无基准） |
| 2 | task-54 模式切换卡顿 | 只拿到**无节点/失败路径条件下的读数**：注入闭包的调用序列 + 门禁并行化实测（见 §3.3） | **部分**：逻辑与并行上界**已实测**；**「用户感知的耗时」仍未测**（需真核心 + 真节点） | 门禁两次 150ms 探测**实测 152ms**（串行需 ≥300ms）；未运行态 `steps=[]`、stop/start **零调用** |
| 3 | task-61 192 行重复 | 删掉重复块 → 真浏览器**逐元素比较计算样式**（`.chain*` 8 行 × 1280/900 × 20 个属性） | **未能攻破**（差异 0）；重复结构我复测为 **两段 86+106=192 行、第二份 0 行独有**（与 lead 一致，**我上一版只覆盖了第一段**）；该重复在 `bdeee46`/`ac98a68` 存在，**已在 `1a1c8a3` 删除** | 元素数 `.chain`=1、`.chain__row`=8…；**diff=0**；`styles.css` 2577→2399 行（删重复提交 `1a1c8a3`） |
| 4 | task-63 取色缓存 | 车辆数量 **13→10→13**，让同名 key `mixed#3..5` 以**新元素**回来（缓存里的旧元素已脱挂） | **攻破「去掉 `isConnected` 回退」的变体**（HEAD 实现防住） | 无回退：**6 辆车 20 秒 fill 恒为 `#64748b`**（红）；HEAD：绿 |
| 5 | task-60 恢复期可见性 | 用后端会发的字段序列 `probe_failures 0→1→2→recovering→recovered/direct_fallback` 走一遍 | **在 HEAD `bdeee46` 上攻破**（16 秒窗口界面**完全沉默**）；该缺口的 `degraded` 修复**已随 `5223a01` 提交**，我在隔离 worktree 上复验「要求断言」**已转绿** | HEAD：`failures=1/2 → text=null`（红）；`5223a01`：`"隧道探测失败 N 次 · 整机断网时先点「断开」"`（绿） |

---

## 2. 我攻破的

### 2.1 目标 5：HEAD 上「整机断网但界面一切正常」的 16 秒窗口 —— **成立**

**攻击场景**：后端在 `recovery.begin` **之前**就会推 `probe_failures`（`core.rs` 的
`sync_probe_failures`）；看门狗 = 10s 间隔 + 6s 超时 + 连续 2 次 → 首次失败 ≈16s、
开始重建 ≈32s。我把这条字段序列喂给 `recoveryView`，看界面**每个阶段**到底显示什么。

隔离 HEAD `bdeee46` 的原始输出（我新增的 `waveAcceptance.test.tsx`）：

```
[恢复期文案] 平时（running） → phase=idle text=null button=disconnect
[恢复期文案] 首次探测失败（t≈16s，后端会推 failures=1） → phase=idle text=null
[恢复期文案] 第二次失败、即将重建（failures=2） → phase=idle text=null
[恢复期文案] 正在重建 → phase=recovering text="正在自动恢复（第 1 次）"
[恢复期文案] 重建失败、退回直连（此时 running=false） → phase=failed text="自动恢复失败（第 1 次），已退回直连 …"
× 【要求】probe_failures ≥ 1 时界面必须有可见信号
  → 这些阶段界面是沉默的（用户整机断网却看到「一切正常」）: expected [ 'probe_failures=1', …(1) ] to deeply equal []
```

**后果**：故障最初的 ~16 秒里，后端已经判定「探不通」，而界面与健康时**逐字相同**；
用户此时会去查路由器/运营商/节点，**不会想到「先断开」**——而「断开」正是能立刻回滚
系统网络配置的动作（`core.rs` 的 `stop_core` 日志「网络配置已回滚」）。这与用户报的
「必须手动断开才恢复」是同一个认知缺口。

**重要**：工作区此刻已有一份**未提交**的修复（`apps/ui/src/ipc.ts` 新增 `degraded` 相位 +
`hint`），我的同一条断言在那里**通过**：

```
[可见] probe_failures=1 → "隧道探测失败 1 次 · 整机断网时先点「断开」"
[可见] probe_failures=2 → "隧道探测失败 2 次 · 整机断网时先点「断开」"
✓ src/waveAcceptance.test.tsx (6 tests)
```

⇒ **缺口在 HEAD 口径上成立、在实时工作区已被覆盖**。发布前必须确认这份改动**落地**
（它是 task-60 的收尾，尚未提交）。

### 2.2 目标 4：取色缓存的 `isConnected` 回退 —— **去掉它会真的写错元素**

**攻击场景（在 /tmp 副本里）**：挂载 13 辆车 → 跑一帧让缓存建立 → 把数量降到 10
（`mixed#3..5` 被卸载）→ 再升回 13（同名 key 以**新元素**回来）→ 跑 20 秒，记录每辆车的
`fill` 集合。两条分支只在**一个条件**上不同：`if (!rect || !rect.isConnected)` vs `if (!rect)`。

| 变体 | 结果 | 原始输出 |
|---|---|---|
| HEAD（有 `isConnected`） | **绿** | `1 passed` |
| 去掉 `isConnected` | **红** | `这些车 20 秒里 fill 恒为初始灰：#64748b …: expected [ 'mixed#0', 'mixed#1', …(4) ] to deeply equal []`；`Tests 1 failed` |

⇒ 结论：**缓存确实引入了「同名 key → 新元素」的写错风险，回退条件是承重的**（不是装饰）。
同时这也说明 task-63 的重构**有必要**加这条回退；`querySelector` 每帧每车的老实现天然免疫。

### 2.3 目标 1 的**残余缺口**（不是拒绝接受，是如实记录）

攻击场景 ④：跟随关闭 + **锚点那一行自己正好是最旧的一行、这一帧被裁掉**。
`usePreserveReadingPosition` 的锚点是「渲染列表的最后一行」；它一旦脱挂，本帧没有可用基准
→ **不补偿**（我的断言：scrollTop 保持 500，不平移）。代码在下一帧会重新锚定，
但**这一帧内容确实上移了**（用户正在读的位置会跳一下）。
`jsdom` 无布局，我**无法**量化真实像素；要用真浏览器 + 真实滚动容器才能确认用户可见程度。
最小复现思路：过滤到某个**已经不再产生新行**的等级（例如只保留旧的 `debug`），
跟随关闭，等缓冲满后继续追加 → 观察那一帧的阅读位置。

---

## 3. 我试了但没能攻破的（§3.3 = 目标 2 只拿到「无节点条件读数」，**不是**攻破）

### 3.1 目标 1：日志跳动（task-55）

我试的四个场景（都在我新增的文件里，断言全绿）：

| 场景 | 我做了什么 | 期望 | 实测 |
|---|---|---|---|
| A 过滤/搜索引起的重渲染 | `headKey` 不变（= 未过滤缓冲的第一条）+ 可见内容整体上移 40px | 不许动 | `scrollTop 500 → 500` ✅ |
| B 真裁剪 | `headKey` 1→2 + 内容上移 40px | 必须补回 | `500 → 460`（补了 -40，锚点回到原位）✅ |
| C 跟随开着 | `enabled=false` + 裁剪 | 不补偿 | `500 → 500` ✅ |
| D 锚点被裁掉 | 锚点行 `remove()` + 裁剪 | 不崩、本帧不补 | `500 → 500`（残余缺口，见 2.3） |

另外**代码层面**我核过两处容易漏的接线：
* `Logs.tsx` 传给 `usePreserveReadingPosition` 的是 **`logs[0]?.seq`（未过滤的缓冲头）**
  —— 所以过滤/搜索**不可能**被误当成裁剪（这正是场景 A 防的）；
* `key={line.seq}` + `data-log-seq` 都在（后者是锚点定位用）；`contentRevision` 是
  `${filtered.length}:${lastVisibleSeq}`（不是长度，避免满员后跟随静默失效）。

**我没能构造出来的**：真浏览器里「1500 行 + 持续流入 + 跟随关闭」的端到端漂移量。
预览桥接只有 `tail_logs` 一次性 mock、**没有日志流**，而注入 `listen` 回调需要伪造 Tauri
事件通道（我判断收益/成本不划算，未做）。⇒ 这条的端到端结论**未验证**。

### 3.2 目标 3：192 行重复（**我上一版的测量只覆盖了一半 —— 这里更正**）

**我复测的重复结构**（在冻结文件上逐行比对：`git show bdeee46:apps/ui/src/styles.css`，
2577 行；`ac98a68` 的同一文件逐字节相同）：

| 段 | 我的边界（1-based） | art-designer 的边界 | 行数 | 逐行相同 |
|---|---|---|---|---|
| A | `2114–2199` == `2344–2429` | `2116–2201` == `2346–2431` | 86 / 86 | **True** |
| B | `2240–2345` == `2430–2535` | `2242–2345` == `2432–2535` | 106 / 104 | **True** |
| 合计 | **192** | **190** | **190–192** | 第二份 **0 行独有**（在两份测法下都是 0） |

**准确表述是「190–192 行逐行相同，边界有歧义」**：两段边界上都有**共有的 `}` / 空行**，
所以最长公共子串的**起点可以前后挪 2 行** —— 我和 art-designer 取的是**等价边界**，
**两个数字都不算错**（我 86+106=192，他 86+104=190）。

**我上一版为什么写成 88 行**：我用的是「从 `2344` 对 `2114` 求**最长共同前缀**」，得到 88 行
——它把 A 段（86）后面**恰好也能对上的 2 行边界**也算进去了，于是我把一个**残缺测量**
当成了「整段重复的长度」。正确做法是把第二份 192 行**逐行**去第一份里找归属：
它是**两段不同偏移**的重复（A 偏移 +230、B 偏移 +190），不是一段连续复制。
⇒ **我的 88 行是「只找到两个重复块中的第一个」**（结论「有重复」对，**幅度不完整**）；
art-designer 的 190 与我的 192 是**同一事实的两个等价边界**。
（教训与 task-49/task-63 同族：**测量边界必须写清，否则残缺测量会伪装成「更正」**。）

**去重状态**：该重复在 `bdeee46` / `ac98a68` 上**存在**；**已在 `1a1c8a3`
（`fix(css): 加一道防「未定义 token」的检查 + 删 190 行重复 + …`）删除** ——
`styles.css` 从 **2577 → 2399 行**（我复测当前 HEAD `1a1c8a3` 已无 `2344–2535` 区间，且
工作区与 HEAD 一致）。⇒ 我上一版写的「**仍存在 / 没落地**」**已过时**，此处更正。
（我复测过 `git show 1a1c8a3:apps/ui/src/styles.css`：2399 行、已无 `2344–2535` 这段区间。）
（提交信息说「删 190 行」、我量到第二份 192 行、净减 178 行 —— 三者是**不同口径**
（毛删 vs 第二份行数 vs 净变化，且同一次提交还加了 token 检查/reduced-motion/.row--between），
不作为矛盾结论。）

**我的证伪实验（保留，且与 lead 的规则集合比对互补）**：在 `/tmp` 里删掉第二份后：

| 检查 | 结果 |
|---|---|
| 两份的级联上下文 | 都在**花括号深度 1**（同一层，不在 `@media` 内） |
| 两份之间是否有规则提到这些 class | **没有**（142 行间隔里 `.chain*`/`.verdict*` 出现 0 次） |
| 真浏览器计算样式 diff（`?preview=1&state=connected&view=topology`；`.chain`/`.chain__row`/`__idx`/`__tag`/`__conds`/`__arrow`/`__out`；1280 与 900 两视口；20 个属性） | **差异 0 条**；元素数完全一致（`.chain`=1、其余各 8） |

**两条证据的互补关系（缺一不可）**：
* lead 的**规则集合比对**（`cf33d62` 旧文件 vs 当时去重后：1112 → 1102 条，消失的 10 条
  全部且仅是授权删除的 `@media (max-width: 880px)`）证明「**没有独有内容被丢**」，
  但它**天然忽略重复** → 证明不了「重复是否真被删掉」；
* 我的**计算样式比对**证明「**删掉在渲染面上无差异**」，但它**不覆盖未渲染的规则**
  （`.verdict*` 在预览里没渲染，只有静态结论）。
⇒ 集合比对 + 渲染比对合起来才覆盖「不丢内容」与「删了也没变化」两面。

**我没能覆盖的**：`.verdict*` 未渲染；只测了拓扑页两个视口，未测路由/设置页与其它主题；
去重已落地，但我**没有**在真浏览器里对 **`1a1c8a3` 的最终文件**重跑一次同样的计算样式
比对（预算用尽）—— 这属于发布前验收该补的一项。

---

### 3.3 目标 2：模式切换 —— **无节点 / 失败路径条件下的读数**（隔离 HEAD `5223a01`）

> **先把标签立死**：下面每一条都是「**无节点 / 失败路径条件下**」的读数 ——
> 探测、stop、start **全是测试注入的闭包**，没有真核心、没有真节点。
> 它们能证明「**逻辑与调用序列**」以及「门禁并行化把串行上界压掉了」，
> **不能**当「模式切换实测耗时」或「用户感知的卡顿」引用（后者见 §5.1，**仍未测**）。

**为什么非要有真节点**：`apply_mode_switch` 的真实耗时 ≈ `core::start_core` 的真实耗时，
而后者的绝大部分是「核心进程就绪」（上限 `CORE_READY_TIMEOUT = 10s`）
加「探测目标可达」（`REGION_PROBE_TIMEOUT = 4s` / `PRE_COMMIT_PROBE_TIMEOUT_SECS = 6s`，
两次探测并行后最坏 ≤6s）。没有真核心二进制 + 真节点，这些数只能是**常量上界**，不是测量值。

#### 3.3.1 这次真正读到的

| 读数 | 测试（`5223a01`） | 值 | 含义 |
|---|---|---|---|
| 门禁探测**确实并行** | `supervisor::tests::gate_probes_run_in_parallel_not_sequentially`（注入两次 150ms 探测） | **152 ms** | 串行必须 ≥300ms；实测 ≈150ms + 开销 ⇒ **并行成立** |
| 未运行态切模式**完全不碰核心** | `mode_switch_when_idle_never_touches_the_core`、`..._when_idle_to_direct_is_also_inert` | `steps=[]`、stop/start **零调用** | 「没连接时点模式」这条路径**结构上不可能**卡在核心上 |
| 运行中切模式**只有必要动作** | `..._while_running_restarts_the_core`（stop+start）、`..._while_running_to_direct_only_stops`（只 stop）、`..._does_not_start_when_stop_failed`（stop 失败**不得** start） | 调用序列断言全绿 | 不会出现「stop 失败还硬 start」的二连击 |
| 门禁的**反例面**（不是只探一边） | 8 条 `gate_*`：境内黑洞、境外代理死、空码当超时、未配目标、探测全过但 commit 失败、失败文案不预判原因 | 全绿 | 门禁不是摆设；「境外通就接管」这种写法会被这几条打红 |
| 同一修订的**整套**后端 | `cargo test -p xraytun-desktop --lib` | **121 passed / 0 failed / 1 ignored**（1.44s） | 唯一 ignored 的是 `commands::globe::tests::real_lookup_returns_a_plausible_location`，标注「需要网络」 |

`152 ms` 的取法：在 `/tmp` **副本**的该测试里临时加一行
`println!("GATE_PARALLEL_ELAPSED_MS={}", elapsed.as_millis())` 再 `--nocapture` 读出；
**被测逻辑一行未改，仓库文件未动**。

#### 3.3.2 有真机的人怎么读数（埋点地图；行号 = 修订 `5223a01`）

1. `XRAYTUN_LOG=info` 启动。每次连接会打 5 条 `stage=... ms=...`「启动阶段耗时」：
   `tun_up_and_fd`（`apps/desktop/src/supervisor.rs:478`）、`wait_for_port`（`:508`）、
   `commit_routes`（`:559`）、`pre_commit_gate`（`:568`，文案已写明「含两次探测，已并行」）、
   `stop_core`（`:641`，额外带 `failures=`）。
2. 切模式再打一条**总账**：`apps/desktop/src/commands/settings.rs:202` 的
   `tracing::info!(mode, was_running, steps = ?steps, elapsed_ms, "模式切换完成（逐阶段耗时见各阶段的 tracing 日志）")`；
   并且**同一句会写进 app 日志**（`已切换为「X」模式（已重启核心/已停止核心）；耗时 N ms`），
   用户排障直接看得到 —— 这也是 task-54 的交付面之一。
3. 所以「切模式卡不卡」的判法 = 读 `elapsed_ms`，再用 `stage` 把时间拆到具体阶段；
   `was_running=false` 时应为 `steps=[]` 且 `elapsed_ms`≈0（只改偏好 + 存快照）。

#### 3.3.3 环境坑（给别人省时间）

* 主工作区直接 `cargo test` 被沙箱拦：`~/.cargo` 只读
  （`failed to open '.../bit-set-0.8.0.crate': Operation not permitted (os error 1)`）。
  可复现的绕过：`CARGO_HOME=/tmp/xt-cargo-home CARGO_TARGET_DIR=/tmp/xt-target`；
  **首次全量编译 8m07s**，之后复用同一 target 只重编本工作区 crate。
* 隔离 worktree **缺未入库的资源**，tauri 构建脚本会直接 fail：
  `resource path 'binaries/xray' doesn't exist`，修完又报 `binaries/geosite.dat`。
  需要把主工作区的 `apps/desktop/binaries/{xray,geoip.dat,geosite.dat,LICENSE-xray}`、
  `apps/ui/dist`、`apps/ui/node_modules` 链进来。
  ⚠️ **坑**：`binaries/` 是**已入库目录**（有 `.gitkeep`/`README.md`），
  `ln -s <真实binaries目录> binaries` 不会替换它，而是**在它里面再套一层同名软链**
  （`binaries/binaries`），报错**依旧是**「resource path doesn't exist」——必须**逐文件**软链。

---

## 4. 我发现但没在本卡修的

1. **去重已在 `1a1c8a3` 落地**（`styles.css` 2577→2399 行）—— 我上一版写的「88 行重复仍在
   HEAD、没落地」**已过时**（更正见 §3.2）。遗留未验：最终文件没有重跑真浏览器计算样式比对。
2. **`degraded` 修复尚未提交**（`apps/ui/src/ipc.ts` 工作区改动）—— 目标 5 的缺口
   在 HEAD 口径成立，发布前必须确认它落地。
3. **HEAD→工作区的一个行为变化**：`direct_fallback` 的 `failed` 相位现在带
   `&& !running` 条件（注释解释：手动重连后 `last_outcome` 不会被重置，否则会显示
   「已退回直连」而实际在走代理）。这条**改变了 HEAD 行为**，我按真实状态
   （退回直连后 `running=false`）复测两者一致（都显示「已退回直连」）；但它属于别人
   在途的改动，我不改，仅报备。
4. **共享工作区此刻 `tsc --noEmit` 是红的（11 条），但那全是别人在途的改动**：
   同一修订 `5223a01` 的**隔离 worktree 里 `tsc --noEmit` 退出码 0（完全干净）**；
   11 条全部落在工作区被改动的 `App.tsx` / `Dashboard.tsx`（以及被它连带的**已提交**文件
   `recovery.test.ts`：`connectControl` / `runButtonDisabled` 正在被重命名）。
   ⇒ 发布前必须**以提交的修订**再跑一次，别拿工作区的实时红去判断谁没收拾干净。
   （我自己的那条 TS6133 噪音已在 `5fff566` 清掉，与这 11 条无关。）

---

## 5. 我测不到的（诚实清单）

1. **目标 2 的「用户感知耗时」没测**（§3.3 给的只是**无节点/失败路径条件下的读数**：
   注入闭包的调用序列 + 门禁并行化实测 152ms，**不是**「模式切换实测耗时」）。
   要拿真耗时需要 **cargo 编译 + 真核心二进制 + 真节点**，未运行态还要能真正 start。
   **好消息**：读数的埋点已经齐了（§3.3.2 的 `elapsed_ms` + 5 个 `stage`），
   有节点的人按 §3.3.2 跑一遍即可，不需要我再改代码。
   ⚠️ 常量（`CORE_READY_TIMEOUT=10s` / `CORE_SHUTDOWN_GRACE=3s` / `REGION_PROBE_TIMEOUT=4s` /
   `PRE_COMMIT_PROBE_TIMEOUT_SECS=6s`）是**上界**，不许当读数引用。
2. 目标 1 的**端到端漂移**未测（预览无日志流，见 3.1）。
3. 目标 3 只测了拓扑页的两个视口；**没测**路由页/设置页/暗色主题/其它缩放。
4. 目标 4 的对比在 **jsdom** 里做的（无布局），但「fill 是否写进 DOM」不依赖布局，
   所以结论可信；未在真浏览器复测同一场景。
5. 目标 5 是**状态机层面**的复现：我证明「按后端会发的字段序列走，界面文案是 null」，
   **没有**在真机上跑一次真实的断网+重建；`probe_failures` 的实际推送时序以 `core.rs` 常量为准。
6. 所有前端数字来自 headless Chrome / jsdom；真机 WKWebView 未测。

## 附：原始产物

* 反证测试（新增，仓库内）：`apps/ui/src/waveAcceptance.test.tsx`（6 条）
* 取色缓存攻击（/tmp）：`/tmp/ac-guard`（HEAD）与 `/tmp/ac-noguard`（去掉 `isConnected`）
* CSS 去重实验（/tmp）：`/tmp/css-orig`、`/tmp/css-dedup`、`/tmp/css-diff.mjs`、`/tmp/css-*.json`
* 隔离 worktree：`/tmp/xt-tester`（detached `bdeee46`）
* 目标 2 的隔离 worktree：`/tmp/xt-t2`（detached `5223a01`，只软链了资源/依赖，**源码未改**）
* 目标 2 的后端测试输出：`/tmp/t2-tests.log`（121 passed / 0 failed / 1 ignored）
* Rust 依赖缓存（绕沙箱只读 `~/.cargo`）：`CARGO_HOME=/tmp/xt-cargo-home`、`CARGO_TARGET_DIR=/tmp/xt-target`
