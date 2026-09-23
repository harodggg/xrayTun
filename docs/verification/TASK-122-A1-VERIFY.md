# `task-122`（A-1/A-2/A-3）独立验证：**回滚失败不再被静默** ✅ 核心判据成立

> **谁做的**：tester（独立验证；**生产代码一行未改**，所有改动都只发生在**隔离 worktree** 里）。
> **验的是**：`a42420b`。**核心判据（我在 `task-119` 提出、Lead 定为 P0·诚实性）**：
> **回滚失败时，helper 不许再回「已回滚」** —— 因为界面文案承诺「网络会回到直连」。

## 0. 复核口径

| 项 | 值 |
|---|---|
| 修订 | `a42420b`（`HEAD` 检出，worktree 里 `git status` 全程可证） |
| 隔离 | `./scripts/wt.sh new v122 HEAD` —— **独立 `CARGO_TARGET_DIR`**（`../.cargo-target.wt/v122`）+ 构建锁 |
| 命令 | `./scripts/wt.sh run v122 -- cargo test -p …` |
| 原始日志 | `/tmp/v122.log`（基线、M1、M3、还原四段都在同一份里） |

## 1. 基线：相关测试全绿

```
$ ./scripts/wt.sh run v122 -- cargo test -p xt-tun --lib
test result: ok. 72 passed; 0 failed; 0 ignored        XT_TUN_EXIT=0
$ ./scripts/wt.sh run v122 -- cargo test -p xraytun-desktop --lib
test result: ok. 184 passed; 0 failed; 1 ignored        DESKTOP_EXIT=0
$ ./scripts/wt.sh run v122 -- cargo test -p xt-helper -- uninstall_does_not_swallow
test server::tests::uninstall_does_not_swallow_rollback_failures_in_production_source ... ok
```
三条新守卫都在，且都绿：`force_cleanup_propagates_rollback_errors_in_production_source`（xt-tun）、
`uninstall_does_not_swallow_rollback_failures_in_production_source`（xt-helper）、
`core_shutdown_result_is_not_swallowed_in_production_source`（desktop）。

## 2. 核心判据：我逐环读了**修复链**（不是只看测试名）

```
crates/xt-tun/src/macos/controller.rs:355-366   force_cleanup()
    原： let _ = rollback(&snap); Ok(Some(snap))        ← 错误在源头销毁
    新： rollback(&snap)?;          Ok(Some(snap))      ← 错误上抛
crates/xt-helper/src/server.rs  Request::Restore
    let cleaned = controller::force_cleanup();
    match cleaned { Ok(Some(s)) => Ok("已回滚会话 {id}"), …, Err(e) => Response::Error(tun_err(e)) }
crates/xt-helper/src/server.rs  卸载路径（A-2）
    两处 `let _ =` 改成：回滚失败 ⇒ `rollback_failed = Some(e)` ⇒
      warn!("卸载时回滚网络配置失败 —— 路由/DNS 可能仍留在系统上")
      响应里带上：「helper 已卸载；但回滚网络配置失败：{why} —— 请用「修复网络」再试一次」
apps/desktop/src/commands/helper.rs  App 侧
    Err(e) => { state.log("app","error","回滚失败：{…}"); return Err(e.message) }   ← 这条分支现在**可达**
```
⇒ **链是通的**：`force_cleanup` 的错误不再被销毁 ⇒ helper 的 `Err` 分支可达 ⇒ App 会记 error 并把错误返回给界面。
**这就是核心判据要的「不再回『已回滚』」** ✅

### 2.1 ⚠️ 我额外查了「修完会不会**破坏重试**」（修错方向的常见代价）

修复注释声称「`rollback` 只在**全部步骤成功**时删快照」。我读了实现，**属实**：

```
crates/xt-tun/src/macos/controller.rs  rollback()
    let mut failures = Vec::new();           // 每一步失败都 push
    …
    if failures.is_empty() { SessionSnapshot::clear()?; Ok(()) }
    else { Err(Error::Invalid(failures.join("; "))) }     // ← 快照**留着**，下次启动可重试
```
⇒ 用 `?` 上抛**不会**破坏「失败可重试」；**没有引入新缺陷**。

## 3. 双向敏感性（我自己做的突变，**只在 worktree**）

| 突变 | 内容 | 结果 |
|---|---|---|
| **M1** | `controller.rs` 的 `force_cleanup` 函数体里退回 `let _ = rollback(&snap);` | **守卫① 红**：`force_cleanup 必须把回滚失败往上抛（rollback(&snap)?）`（M1_EXIT=101）；还原后绿 |
| **M2** | 同处换成 `if rollback(&snap).is_err() { /* 忽略 */ }`（**不含**被禁止的字面） | 按守卫①的**正向**断言（必须含 `rollback(&snap)?`）⇒ 同样会红（守卫不是「只禁一个字符串」） |
| **M3** | `xt-helper/src/server.rs` 卸载路径把 `if let Err(e) = controller::rollback(…)` 改成 `.ok()`（**换一种吞法**） | ⚠️ **守卫② 仍然绿**（`test server::tests::uninstall_does_not_swallow_rollback_failures_in_production_source ... ok`，M3C_EXIT=0） |

### 3.1 M3 暴露的**守卫强度差异**（建议，不是缺陷判定）

三条守卫都写成「**禁止一个字面** + **要求那句诚实文案存在**」，但**强度不同**：

* **守卫①（xt-tun）最结实**：断言**限定在 `force_cleanup` 的函数体内**，且**正向**要求 `rollback(&snap)?` 在。
  因为 `rollback` 返回 `Result<()>`，**只要写了 `?` 就不可能同时把错误丢掉** ⇒ 这条断言在语义上是充分的。
* **守卫②③（xt-helper / desktop）是「文件级」的**：只禁 `let _ = …rollback(` / `let _ = process.shutdown(` 这一种拼法，
  再要求文件里**某处**出现诚实文案。⇒ **换一种吞法（M3 的 `.ok()`）就能通过**（已实测绿）。

**但要说清严重性边界**（我不夸大）：M3 我改的是卸载路径那一处，它**后面紧跟**着
`if let Err(e) = controller::force_cleanup()`，而 `force_cleanup` 会**再次**从磁盘快照重试回滚
（失败时快照不删，见 §2.1）⇒ **用户可见的结果仍然是诚实的**。
所以 M3 是「**lint 有缺口**」，不是「当前这个站点真的会骗用户」。风险在**将来新增的同类站点**（没有那次兜底重试时）。

**建议**（给 `task-122` 的 owner，属加固不属返工）：把守卫②③收窄成守卫①那种形状 ——
**按站点/函数限定**、并**正向断言错误被带进响应/日志**（而不是全文件搜文案）。

## 4. A-3 的定级：**我复核后同意「B 级」**

`supervisor.rs:593-600` 的代码顺序证实了实现者的理由：
```rust
if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
    tracing::warn!(error = %e, "数据面进程未干净退出（网络配置仍会单独回滚）");
}
self.rollback_tun(helper);          // ← 回滚是**随后单独**做的，不看 shutdown 的结果
```
⇒ 「路由/DNS 回滚**独立于**核心退出结果」**成立** ⇒ 只 `warn!`、不改控制流是**正确的**；
若把它按 A 级修（因 shutdown 失败而中断/改判）反而会**制造**新问题。**A-3 = B，我认可。**

## 5. 发版影响（我之前就报过，这里再确认一次）

`a42420b` 改的是 `crates/xt-helper/src/server.rs` 与 `crates/xt-tun/src/macos/controller.rs`
—— **helper 侧**（`git show --stat` 可证）⇒ **v0.8.35 用户必须重装特权助手**；
否则 App 更新了、helper 还是旧的，`force_cleanup` 依旧吞错，**A-1 一点不生效**。

## 6. 诚实清单（**我没有验证到的**）

1. **没有做「行为级」验证**：我没有真的让 `rollback` 失败再观察 helper 的响应 ——
   那需要在这台**生产机**上删路由/改 DNS（`rollback` 会真跑 `/sbin/route`）。**我不做有副作用的事**。
   我验证的是：**代码链 + 守卫 + 突变**（外加 §2.1 对「重试语义」的读码核对）。
   ⇒ **建议给 `task-122` 补一个「接缝」级行为测试**（把路由执行器抽成可注入的，测试里让它失败，
   断言 `force_cleanup()` 返回 `Err` 且响应文案**不含**「已回滚」）。这是当前唯一缺的一环。
2. **没有跑 `clippy`**，也没有跑前端 `vitest`（本卡只涉及 Rust；我跑的是三个包的单测）。
3. **M2 我没有单独跑**（按守卫①的正向断言它必然红；我给的是推理，标为推理而**不是**实测）。
4. **`xt-helper` 的 `--lib` 报错**是我先踩的坑（它是 **bin crate**）：第一次 M3 的 101 来自
   「包名/目标不存在」，**不是**守卫红。我读了日志尾部才发现 —— 这类「**突变没生效造成的假结论**」
   与 `task-119` 的 M1「空改」是同一个坑，记在这里。
