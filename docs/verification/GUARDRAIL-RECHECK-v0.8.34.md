# 护栏隔离复核：v0.8.34 里这两条护栏**真的存在、且真的会红**

> **谁做的**：tester（独立于实现者）。**为什么做**：本项目多次出现「**测试对真实故障敏感度为 0**」
> —— 那比没有测试更糟（看着有护栏，其实没有）。所以这次不看「测试名」，而是**把实现改坏**，
> 要求护栏**真的变红**。

## 0. 复核口径（先钉死，再谈结论）

| 项 | 值 |
|---|---|
| 复核修订 | **tag `v0.8.34` = commit `6aa3b5e`**（`git -C <wt> rev-parse HEAD`） |
| 隔离方式 | `./scripts/wt.sh new recheck834 v0.8.34` —— **worktree 自带独立 `CARGO_TARGET_DIR`**（`../.cargo-target.wt/recheck834`），并走构建锁 |
| 为什么必须隔离 | 共享 target dir 会把「两个 checkout 的同名同版本 crate」混在一起 ⇒ 回退/敏感性实验**链错 rlib**，得到「改了也不红」的假结论（`docs/verification/WORKTREE-TARGET-DIR.md`） |
| 命令 | `./scripts/wt.sh run recheck834 -- cargo test -p xraytun-desktop --lib [-- <filter>]` |
| 环境 | macOS 26.6.2 / arm64 / cargo 1.98.0 |
| 原始日志 | `/tmp/recheck834-desktop.log`（全量 172 项）、`/tmp/sens-m1b.log`、`/tmp/sens-m2.log`、`/tmp/sens-m3.log`、`/tmp/sens-baseline.log` |

**两条护栏各自是「哪个版本带出去的」**（用 `git show <rev>:<path> | grep -c` 逐个核）：

| 护栏 | 引入提交 | v0.8.33 | **v0.8.34** |
|---|---|---|---|
| `admitted + accounted == total`（界面限流的对账红线） | `1b58f8b`（task-91 A） | **0 处** | **1 处** ✓ 本版**新带出** |
| `FailureExit::ALL` / `SwitchEnd`（退场点清单） | `d3b463c`（task-75 ①③） | 有 | 有 ✓ |

## 1. 基线：两条护栏在 v0.8.34 的隔离构建上**全绿**

```
$ ./scripts/wt.sh run recheck834 -- cargo test -p xraytun-desktop --lib
test commands::core::tests::throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate ... ok
test commands::core::tests::every_failure_exit_still_invalidates_intent_in_production_source ... ok
test commands::nodes::tests::switch_end_intent_decision_is_pinned ... ok
test commands::nodes::tests::failed_switch_drops_intent_on_disk ... ok
test result: ok. 172 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1.43s
```

## 2. 护栏 A：`admitted + accounted == total`（红线原文：「每一行都要有着落」）

**被复核的断言**（`apps/desktop/src/commands/core.rs`，测试 `throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate`）：
用实测速率搭一个洪流（**31 行/秒、两种形状、600 秒**），要求
`放行数 + 记账省略数 == 18600`，并且放行速率 ≤ 0.5 行/秒。

### 2.1 第一次「改坏」是**空改**（必须记下来）

```rust
// M1（第一次）：只给「形状被判掉」的行记账
- self.suppressed += 1;
+ self.suppressed += u64::from(!shape_ok);
```
→ `test result: ok. 1 passed` —— **没红**。
**原因不是护栏弱，而是这个改法在这条 fixture 上不改变行为**：fixture 只有 **2 种形状**，
「形状通过但被全局配额挡下」这一支**一次都不会发生**（5 行/秒的配额永远用不完两种形状）。
⇒ **「改坏代码」≠「改坏行为」**；敏感性实验必须确认改完**真的走了不同的分支**。

### 2.2 改成真的会丢账 ⇒ 护栏**红**（这才是结论）

```rust
// M1'：摘要每次最多报 5 条（一种真实的「静默截断」实现）
- self.suppressed += 1;
+ self.suppressed = (self.suppressed + 1).min(5);
```

```
test commands::core::tests::throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate ... FAILED
thread '…throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate' panicked at apps/desktop/src/commands/core.rs:3453:9:
assertion `left == right` failed: **每一行都要有着落**：放行的 + 明确记账省略的 = 全部（不许静默丢弃）
  left: 3240
 right: 18600
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 172 filtered out
EXIT=101
```

⇒ **护栏 A 是真的**：它挡住的正是「界面比事实弱」（少报了 15360 行）。

## 3. 护栏 B：`SwitchEnd` / `settle_switch` / `FailureExit`

被复核的**三处**：
1. `nodes.rs::switch_end_keeps_intent` —— 结局 → 要不要作废「自动重连」意图；
2. `nodes.rs::settle_switch` —— 切换路径上**唯一**碰意图的地方；
3. `core.rs::every_failure_exit_still_invalidates_intent_in_production_source` —— **源码级**守卫：
   每个 `FailureExit` 变体在生产源码里必须**恰好出现一次**（注释里提到不算）。

### 3.1 M2：把「失败也作废」改回「失败不作废」（task-75 修掉的那个 bug）

```rust
// M2（nodes.rs）
- SwitchEnd::NoTunnel(_) => false,
+ SwitchEnd::NoTunnel(_) => true, // 切换失败也不作废意图
```

```
test commands::nodes::tests::switch_end_intent_decision_is_pinned ... FAILED
thread '…switch_end_intent_decision_is_pinned' panicked at apps/desktop/src/commands/nodes.rs:561:13:
隧道是断的 —— 必须作废意图（否则下次启动拿坏节点再接管一次网络）

test commands::nodes::tests::failed_switch_drops_intent_on_disk ... FAILED
thread '…failed_switch_drops_intent_on_disk' panicked at apps/desktop/src/commands/nodes.rs:605:17:
切换失败（NodeSwitchNoFallback）后盘上必须是 false —— 否则下次启动会用这个坏节点自动重连

test commands::nodes::tests::successful_switch_keeps_intent_on_disk ... ok
test result: FAILED. 12 passed; 2 failed; 0 ignored; 0 measured; 159 filtered out
EXIT=101
```

⇒ **护栏 B-1/B-2 是真的，而且是对照组**：同一改动下「成功路径仍保留意图」那条**依旧绿**
—— 说明红的是**失败路径**这个自由度，不是「所有 switch 测试一起红」。

### 3.2 M3：把一处生产调用点的变体换掉 ⇒ **源码级守卫**红

```rust
// M3（core.rs 生产源码）
- invalidate_after_failure(&state, FailureExit::WatchdogRebuild);
+ invalidate_after_failure(&state, FailureExit::NetworkWatchRebuild);
```

```
test commands::core::tests::every_failure_exit_still_invalidates_intent_in_production_source ... FAILED
assertion `left == right` failed: 退场点 `FailureExit::WatchdogRebuild` 在**去掉注释后的**生产源码里
应当恰好出现一次（那一处作废调用），现在出现 0 次。删掉它、或把它注释掉，都等于这处作废失效
—— 那正是本测试要防的回归。
  left: 0
 right: 1
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 172 filtered out
EXIT=101
```

### 3.3 还原后基线**重新变绿**（双向都记）

```
$ git -C <wt> checkout -- apps/desktop/src/commands/core.rs apps/desktop/src/commands/nodes.rs
$ git -C <wt> status --porcelain          # 0 个改动
$ ./scripts/wt.sh run recheck834 -- cargo test -p xraytun-desktop --lib -- <4 条测试名>
test …throttle_cuts_a_realistic_flood_to_a_bounded_ui_rate ... ok
test …switch_end_intent_decision_is_pinned ... ok
test …failed_switch_drops_intent_on_disk ... ok
test …every_failure_exit_still_invalidates_intent_in_production_source ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 169 filtered out
EXIT=0
```

## 4. 结论

| 护栏 | 在 v0.8.34 里存在？ | 会红？ | 证据 |
|---|---|---|---|
| `admitted + accounted == total` | **是**（本版新带出） | **会**（`每一行都要有着落`，3240 vs 18600） | §2.2 |
| `SwitchEnd` 失败必须作废意图 + 盘上为 false | **是**（v0.8.33 起） | **会**（2 条红，成功路径仍绿） | §3.1 |
| `FailureExit` 源码级「恰好一次」守卫 | **是** | **会**（0 次即红） | §3.2 |

**三条都是真的护栏，不是空壳。**

## 5. 诚实清单（这次**没**做到的）

1. **这不是系统的变异测试**：三个变异都是我手写的、针对**单个条件**的；没有做「覆盖率驱动的变异扫描」，
   也不能推出「其余 169 条测试都有效」。
2. **M1 第一次是空改**（§2.1）：我把它留在报告里，因为它正好说明「改了代码」不等于「改了行为」——
   若不检查分支是否可达，敏感性实验会得出**相反**的结论。
3. **只跑 desktop 一个 crate 的相关测试**：`xt-core` / `xt-tun` 的护栏这次没有复核。
4. **只证明「证据存在」**：护栏红说明测试在拦这件事，**不**说明产品在真机上的行为；
   UI 侧限流的实际体感、真机切换节点的体验都**未验证**。
5. 复核用的是 **worktree（tag 检出）+ 独立 target dir**；主工作区在同一时间被其他人的改动占用，
   **不影响**本报告（每次构建用的都是 worktree 里的源码，`git -C <wt> status` 可证）。
