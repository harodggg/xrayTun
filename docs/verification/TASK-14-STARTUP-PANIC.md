# task-14：`setup` 里裸 `tokio::spawn` → 双击即 SIGABRT（验证 + 门禁盲区）

> **结论**：根因已定位并由 tester 稳定复现（真机 panic.log），修复是
> `apps/desktop/src/lib.rs` 的 `setup` 闭包里把裸 `tokio::spawn` 换成
> `tauri::async_runtime::spawn`；同族站点一并统一。
> **门禁盲区补上了**：`check.sh` 新增一步静态守卫（`naked_tokio_spawn` 两条测试），
> 并给出「旧代码上必红」的突变反证（退出码 101，位置指向 `lib.rs:184`）。
> **没做到的（如实说）**：本沙箱**无法安全执行忠实的 `open -a` 3 次验证** —— 原因见 §4，
> 需要 tester / 用户按 §4 的前置条件跑。

---

## §1 根因（tester 的现场证据 + 上游源码）

真机 panic.log（`open -a` 3/3，由 `e8aa9b3` 装的 hook 写出）：

```
[unix=…] PANIC apps/desktop/src/lib.rs:178:17
there is no reactor running, must be called from the context of a Tokio 1.x runtime
```

- `apps/desktop/src/lib.rs:178` 当时是 `tokio::spawn(async move {`（判定节拍任务）。
- Tauri 的 `setup` 回调**不在 tokio runtime context 里**；裸 `tokio::spawn` 需要
  「当前线程已进入某个 tokio runtime」，否则就是上面那句 panic。
- `[profile.release] panic = "abort"` ⇒ 这个 panic 直接变成 **SIGABRT**（双击即崩）。
- 该代码**不是新引入的**：`git show 3376830:apps/desktop/src/lib.rs | grep -n 'tokio::spawn'`
  → `:87`（0.8.38 tip）。⇒ 与用户报的那次崩溃同源。
- 0.8.38 没有 hook、二进制 `strip = true`，所以当时崩溃报告只有 `abort() called`、没有位置。

**为什么 `if let Some(settings)` 一定走进那一行**：`state.with(...)` 返回的 `Option`
恒为 `Some`（`settings` 字段不是 `Option<AppSettings>`），所以**每次启动**都会到
`tokio::spawn`；这与 `open -a` 3/3、direct-probe 1/1 的观测一致。

## §2 修复（`apps/desktop/src/**` 的同类一起统一）

| 文件:处 | 原来 | 现在 | 为什么 |
|---|---|---|---|
| `lib.rs`（`setup` 内，判定节拍） | `tokio::spawn` | **`tauri::async_runtime::spawn`** | 同步上下文，没有 runtime context ⇒ 直接 panic（根因） |
| `commands/core.rs`（日志转发） | `tokio::spawn` | `tauri::async_runtime::spawn` | 现状在 `async fn` 里能跑，但**写法会被人抄到同步上下文**（0.8.38 就是这么来的） |
| `commands/globe.rs` ×2（并发查询） | `tokio::spawn` | `tauri::async_runtime::spawn` | 同上；Tauri 的 `JoinHandle` 也实现 `Future`，`.await` 用法不变 |
| `traffic.rs`（流量采样） | `tokio::spawn` | `tauri::async_runtime::spawn` | 旧注释说「Tauri 的 JoinHandle 拿不到 `abort()`」**不成立**：它有 `abort()`；且本函数是 pub 同步函数，谁都能从同步上下文调 |

复算「生产里不再有裸 `tokio::spawn`」：

```bash
grep -rn 'tokio::spawn' apps/desktop/src --include=*.rs
# 只剩注释行（说明为什么不能用）
```

`tokio::time::sleep/interval` 保留不动：它们都写在 `tauri::async_runtime::spawn(async move {…})`
或 `async fn` 体内，**被 poll 时一定有 runtime context**；`tokio::task::spawn_blocking` 同理。

## §3 门禁盲区与新增守卫（最重要的产出）

**为什么原来抓不到**：`scripts/check.sh` 的每一步（clippy / `cargo test --workspace` /
release 构建）**都不会启动 App** ⇒「编译得过 + 单测全绿」与「一启动就 abort」
可以同时成立（run3 全绿时的 HEAD `46e52cf` 里 `lib.rs:178` 仍是 `tokio::spawn`）。

新增两步：

1. **静态守卫（每次都跑）**：`check.sh` 的「App 启动路径守卫」调用
   `cargo test -p xraytun-desktop --lib naked_tokio_spawn`，并**额外断言两条测试
   各自的 ok 行都出现**（过滤到 0 条测试时退出码是 0 —— 本仓库 R4 记过这个假绿坑）。
   判据本体在 `apps/desktop/src/lib.rs`：
   * `production_never_calls_naked_tokio_spawn`：扫 `apps/desktop/src/**` 的
     **生产代码**（逐项跳过 `#[cfg(test)]`），一个裸 `tokio::spawn(` 都不许有；
     同时断言扫到 >5000 行生产代码、且确实看到 `tauri::async_runtime::spawn`
     （防空转假绿）。
   * `naked_tokio_spawn_guard_catches_the_0_8_38_pattern`：负例 —— 把 0.8.38 的写法
     放回源码文本，判据必须命中。
2. **真启动烟测（可选，`XRAYTUN_SMOKE_APP=<path>` 时跑）**：
   `scripts/smoke-app-startup.sh`。安全边界与判据见脚本头部；要点：
   * `--mode open` ≈ 双击，但**先检查真实 `settings.json`**：若
     `was_connected=true` 且 `mode != direct`（会自动重连接管路由）⇒ **拒绝运行（75）**；
   * `--mode direct`：`XRAYTUN_DATA_DIR` 隔离目录 + `mode=direct` + `was_connected=false`，
     安全；但受限终端里可能**静默挂起**（见 §4）⇒ 记 75，不记绿；
   * 判据：`NEW_IPS=0`（`~/Library/Logs/DiagnosticReports/xraytun-desktop-*.ips` 不新增）
     + 真实 `panic.log` 字节不增长 + 默认路由/utun 与基线相同 + 活着 + 有启动证据。

### 反证：门禁新步骤在旧代码上**必红**（本次实测）

```bash
cp apps/desktop/src/lib.rs /tmp/b1-lib.rs.orig     # 备份
python3 -c "… 把第一处 tauri::async_runtime::spawn 换回 tokio::spawn …"
cargo test -p xraytun-desktop --lib naked_tokio_spawn
cp /tmp/b1-lib.rs.orig apps/desktop/src/lib.rs     # 还原（sha 校验一致）
cargo test -p xraytun-desktop --lib naked_tokio_spawn
```

实测输出（节选）：

```
thread 'tests::production_never_calls_naked_tokio_spawn' panicked at …/lib.rs:901:
生产代码里不许有裸 `tokio::spawn`：同步上下文（Tauri `setup` 回调）会 panic
`there is no reactor running…` ⇒ release 下 SIGABRT ⇒ 双击即崩。…
/Users/xbtg-/deepseek-harness/xray-tun/apps/desktop/src/lib.rs:184: tokio::spawn(async move {
test result: FAILED. 0 passed; 2 failed; …
exit=101
```

还原后：`orig_sha == restored_sha`，两条测试 `ok`，`exit=0`。
⇒ **`check.sh` 新增的那一步在旧代码/回归写法上会红**，不是摆设。

## §4 忠实 `open -a` 3 次验证：本沙箱**做不到**（原因与前置条件）

`open` 路径的 env **传不进去**（tester 实测 `open -n --env XRAYTUN_DATA_DIR=…` 无效，
panic.log 仍落真实目录）⇒ `open` 阶段**不受隔离保护**，用的是
`~/Library/Application Support/com.xraytun.desktop/settings.json`。本机真实设置是：

```
mode=tun  was_connected=true  auto_reconnect=true
```

修复**前**它崩在 `setup`（来不及接管网络，tester 实测 `route_changed=0`）；
修复**后** App 会真的启动 ⇒ `should_auto_reconnect` 为真 ⇒ **会去自动重连、接管默认路由**。
而本沙箱里：

* 写真实数据目录被拒（`touch …: Operation not permitted`）；
* `launchctl setenv` 被拒（`Not privileged to set domain environment`）；
* `open --env` 实测无效（tester）。

⇒ 我**没有执行** `open -a`（用户明确要求「绝不能改用户系统网络」）。烟测脚本
在这种情况下**主动拒绝**（退出码 75）：

```
$ ./scripts/smoke-app-startup.sh --app …/XrayTun.app --mode open --runs 3
✗ 拒绝跑 open 模式：真实 settings 是 unsafe:mode=tun:was_connected=True
  …三个安全出路…
exit=75
```

**要拿到「`open -a` 3/3 不崩、panic.log 不增长、NEW_IPS=0」这条证据，需要一个前置条件**
（任一即可，都在脚本提示里）：

1. 备份后把真实 `settings.json` 的 `was_connected` 置 `false`（或 `mode` 置 `direct`），
   跑完还原；
2. 在无人使用网络、且可接受接管的机器上跑；
3. 或由用户在有写权限的终端里自己跑（脚本会读设置决定是否放行）。

### 也试过但**不采用**的第三条路（如实记）

「临时 wrapper `.app`：脚本设好 env 再 exec 真实二进制 + `open -n wrapper`」——
实测**修复前的 tester 构建在它下面也不崩**：进程活着、`stdout.log` 0 字节、
无 `panic.log`、无新 `.ips`（与 `direct` 的静默挂起同形）。它**不复现双击路径的崩溃**，
拿它当判据会给出**假绿**，所以脚本不提供这个模式，只保留 `open`（忠实）与 `direct`（安全但有环境噪声）。

### `--mode direct` 在本沙箱的结果（记录，不当作产品的错）

```
进程存活 6s ⇒ 已收尾（未崩溃）
⚠ 活着但**没有任何启动证据** ⇒ 可能静默挂起（不是通过）
exit=75
```

这与 tester 的观察一致（受限终端里 direct exec 6/7 次在打任何日志前停住）。
⇒ **本沙箱里 direct 既不能证伪也不能证实**，这正是脚本要区分
「产品失败（1）」与「无法判定（75）」的原因。

## §5 本次实际跑过的门禁

```bash
export CARGO_HOME=… CARGO_TARGET_DIR=…/backend1        # 自己的 target dir
cargo test -p xraytun-desktop
cargo clippy -p xraytun-desktop --all-targets -- -D warnings
```

| 命令 | 结果 |
|---|---|
| `cargo test -p xraytun-desktop` | **308 run：303 passed / 0 failed / 5 ignored**，另有 `tests/type_contract` 8/8 |
| `cargo clippy -p xraytun-desktop --all-targets -- -D warnings` | **exit 0** |
| 门禁新步骤（`cargo test … --lib naked_tokio_spawn`） | 绿；突变反证见 §3（exit 101） |
| `./scripts/package-macos.sh` | App 出包成功（`.cargo-target.wt/backend1-release/release/bundle/macos/XrayTun.app`） |

## §6 还欠什么（交 tester / Lead）

1. **忠实的 `open -a` 3 次**：按 §4 的前置条件跑；
   `XRAYTUN_SMOKE_APP=<path> XRAYTUN_SMOKE_MODE=open ./scripts/check.sh` 会把这一步接进全门禁。
   反例（修复前构建）应给 `NEW_IPS=+3`、真实 panic.log 追加 3 条 —— tester 在 task-9 已
   留档 `open -a` 3/3 崩（真机 panic.log 3 条：1311 B）。
2. **独立复验**：按 Lead 要求，发现者不自证 —— 请 tester 独立跑
   `--mode open`（在安全前置条件下）与本文件的突变反证。

## §7 独立复验（tester，commit `b435a49`，报告 `TASK-14-INDEPENDENT-VERIFY.md`）

tester 用**自己写的扫描器**（不复用本文的实现）复验，结论：

* 静态：`apps/desktop/src` 生产代码 **12,615 行 → 裸 `tokio::spawn` 命中 0**、
  `tauri::async_runtime::spawn` 34 处；
* 守卫：`cargo test … --lib naked_tokio_spawn` → 2 passed；并确认「过滤器匹配 0 条 =
  退出码 0」的假绿会被 `check.sh` 的两条 grep 断言拦住；
* **运行时负例（旧必须红）**：修复前 App `--mode direct` → **3/3 `Abort trap: 6`（exit=134）**，
  隔离 panic.log 3/3 `lib.rs:178:17` + `there is no reactor running…`，smoke **exit=1**；
* **运行时正例（新必须绿）**：新构建 `--mode direct` → **3/3 绿、exit=0**
  （存活 10s、有启动证据、`NEW_IPS=0`、真实 panic.log 未增长、网络未变），
  本卡产出的 App 同样 3/3 绿；
* `--mode open` 的安全门：独立确认不安全 settings 下 **exit=75 且未拉起进程**，
  路由 / panic.log / IPS 不变；
* **唯一缺口**：忠实的 `open -a` 双方都跑不了（沙箱拒写真实数据目录与父目录、
  `launchctl setenv` 无权限）⇒ 记「因沙箱写权限无法完成，未验证」，
  **没有**用 wrapper/direct 凑成 open 的绿。

### tester 指出的两条边界与处置

| 边界 | 处置 |
|---|---|
| 守卫只覆盖 `apps/desktop/src/**`（不含 `crates/**`） | 保持 —— 本卡范围如此；边界已写进本文件 §3 与守卫注释 |
| `without_line_comment` 不处理 `/* */` ⇒ 块注释里的调用会**假红** | **已修**：改为 `strip_comments`（保留换行 ⇒ 行号不变；支持嵌套块注释；跳过字符串字面量，避免 `"http://…"` 把同行后续代码吞成假绿），新增用例 `comments_and_strings_do_not_trip_or_hide_the_guards`（旧实现下必红） |

> 假红不是假绿，但它会让守卫在「注释里提到被禁写法」时误报；修完本次重跑门禁：
> `cargo test -p xraytun-desktop` **309 run / 304 passed / 0 failed / 5 ignored**
> + 8 条 `tests/type_contract`，`cargo clippy … -D warnings` **exit 0**。
