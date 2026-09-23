# 订正 · `task-163` 提交信息里对「接缝」的一处不实描述

> **为什么单独写一页**：`ee96609` 的提交信息（已在 `origin/main`）里有一句不实陈述。
> 本项目自 v0.8.35 起定的规矩是「**不重写已推送的共享历史**」（不 `--amend`、不 force-push），
> 所以订正放在文档里，而不是改历史。ops 已在 `CHANGELOG.md` / `RELEASE-v0.8.37.md` 同步改正。
> 由 Lead 裁为「以事实为准的更正」（ops 读 diff 时发现）。

## 1. 不实的那句 vs 准确的说法

| | 陈述 |
|---|---|
| ❌ 提交信息原文 | 「新增接缝（`uninstall_with` / `TestExecutor` / `with_test_root`）**只在 `#[cfg(test)]` 下存在**」 |
| ✅ 准确说法 | **两类东西必须分开说**：<br>① **生产路径上的新函数（行为不变的抽取）**：`Helper::uninstall_with(..)`、`uninstall_outcome(..)`、`rollback_memory_session(..)`、`remove_system_traces(..)` —— **没有 `cfg(test)`，生产构建里就在**，`fn uninstall` 现在就是一行委托给它；<br>② **只有测试构建存在的注入入口与钩子**：`xt-tun` 的 `TestExecutor` / `with_executor`、`snapshot::with_test_root`，以及 `supervisor.rs` / `server.rs` 里带 `#[cfg(test)]` 的那几个钩子。 |

## 2. 逐项对照（`ee96609` 之后的 `crates/xt-helper/src/server.rs`）

| 项 | 是否生产代码 | 位置 |
|---|---|---|
| `fn uninstall(&self) -> Response` | 生产：**一行委托**（三个闭包参数） | `server.rs:677-690` |
| `fn uninstall_with<R, F, C>(..)` | **生产**（泛型闭包参数就是接缝本体；无 `cfg(test)`） | `server.rs:725-745` |
| `fn uninstall_outcome(..)` | 生产：纯函数 | `server.rs:761-772` |
| `fn uninstall_response_message(..)` | 生产：纯函数 | `server.rs:774-783` |
| `fn rollback_memory_session(&self)` | 生产 | `server.rs:693-704` |
| `fn remove_system_traces(&self)` | 生产 | `server.rs:707-718` |
| `TestExecutor` / `with_executor`（`xt-tun`） | **仅测试构建**（`#[cfg(test)]`） | `crates/xt-tun/src/macos/mod.rs` |
| `snapshot::with_test_root`（`xt-tun`） | **仅测试构建**（`#[cfg(test)]`） | `crates/xt-tun/src/macos/snapshot.rs` |

「一行委托」不改变权限与协议：IPC 的 `Request::Uninstall` / `Response::Ok` 形状与文案字面量逐字节未变。

## 3. 行为等价的依据（逐行核对，**ops 独立核对过一次**）

结论：**行为等价** ⇒ 助手侧不需要重装（v0.8.35 的重装口径不变）。依据分四条：

1. **求值顺序**：新写法 `let (rollback_failed, response) = uninstall_outcome(session_rollback(), force_cleanup());`
   —— Rust 的**函数实参从左到右求值**（语言保证）⇒ `rollback_memory_session()` 先、`force_cleanup()` 后，
   与旧写法「先 `let memory = …lock/take/rollback…`，再 `let force = force_cleanup()`」**同序**（`server.rs:736` vs 旧 `:684-699`）。
2. **文案字面量**：`uninstall_response_message` 的两条分支与 `{why}` 插值位置逐字节未动（`server.rs:778-783`）。
3. **`warn!` 时机与条件**：两版都是「`rollback_failed` 为 `Some` 时 `tracing::warn!(error = %why, "卸载时回滚网络配置失败 —— 路由/DNS 可能仍留在系统上")`」，
   且位置都在「算出响应之后、清理系统痕迹之前」（新 `:737-742`，旧 `:700-705`）。
4. **副作用顺序**：`remove_traces(self)`（`server.rs:707-718`）与旧站点内联的四步完全一致且同序：
   `launchctl bootout system/<label>` → 删 `HELPER_PLIST_PATH` → 删 socket → 删 `HELPER_INSTALLED_PATH`（最后删自己）。

> 标注：第 1 条是语言保证 + 我按 `git show ee96609^:crates/xt-helper/src/server.rs` 与提交后版本的逐行对照；
> 第 2–4 条 ops 已**独立逐行核对**（我引用其结论，并在此注明「由 ops 独立核对」）。
> `uninstall_outcome` 的语义（第一处失败优先、但任何一处失败都不许丢）由行为测试 `uninstall_outcome_never_hides_a_rollback_failure`
> 与站点级行为测试 `uninstall_site_response_is_honest_under_injected_failures` 钉住（`crates/xt-helper/src/server.rs` 测试模块）。

## 4. 教训（写进方法：描述「接缝」时）

> **「生产路径上的新函数（行为不变的抽取）」与「只有测试构建存在的注入入口」必须分开说。**
> 混成一句，就会写出「只在 `#[cfg(test)]` 下存在」这种不实陈述 ——
> 而这类陈述会被下一个读代码的人**当成规格**（本项目反复记的那一族：陈述比事实更强/更窄）。
