# panic 策略：`[profile.release] panic = "abort"` 保留（2026-09-25 / v0.8.39）

> **结论（TL;DR）**：**保留 `panic = "abort"`，本次不改 `Cargo.toml`。**
> 理由不是「abort 更好」，而是三条可复算的事实叠加：
> ① 0.8.38 的启动崩溃不再需要靠 abort 才「能被看见」：`.build(...).expect(...)`
> 那一环已被 `task-1` 改成「落盘 + `exit(1)`」；而**更可疑的真凶**是 `setup`
> 里的裸 `tokio::spawn`（无 runtime context ⇒ panic ⇒ SIGABRT），见 §1.1，
> 已由另一条工作流修复；
> ② 上游 Tauri 2.11.5 **没有任何命令级 `catch_unwind`**，所以 unwind 换不来
> 「单条命令 panic 不炸整个 App」；
> ③ Cargo **禁止** `[profile.release.package.*] panic = ...`（实测报错），
> 而「只让 App unwind、helper 仍 abort」需要新增自定义 profile +
> 改 `scripts/package-macos.sh`（**超出本卡允许改的文件**）。
> **代价**：放弃「tokio 后台任务 panic 被 tokio 隔离」这一条真实收益（详见 §4）。
> **证据强度**：`★★★` = 本文件里有原始输出/可复算命令 · `★★` = 仓库内留档 · `★` = 仅推断，**未验证**。

---

## §1 事故链（0.8.38）与本卡在链条上的位置

```
用户双击
  └─ setup 闭包返回 Err（或 Builder::build 其它 Err）
       └─ Tauri 包成 Error::Setup，Builder::build 返回 Err
            └─ 我们自己的 .build(...).expect("Tauri 应用启动失败")   ← panic
                 └─ [profile.release] panic = "abort" ⇒ SIGABRT
                      └─ 崩溃报告只有 `abort() called`（hook 还没装的前一版）
```

| 环节 | 证据 | 强度 |
|---|---|---|
| `setup` Err 会被包成 `Error::Setup` 从 `build()` 冒出来 | 本地上游源码 `tauri-2.11.5/src/app.rs:2530-2531`：`if let Some(setup) = app.setup.take() { (setup)(app).map_err(\|e\| crate::Error::Setup(e.into()))?; }` | `★★★` |
| 上游承认「setup 失败会 panic」且仍未修 | [tauri-apps/tauri#12815](https://github.com/tauri-apps/tauri/issues/12815)（**open**，milestone 3.0）：「Currently, `App::run` as well as `App::run_return` will panic if the setup hook provided in `Builder::setup` fails.」 | `★★★`（已抓原文） |
| 我们那一环 `.expect(...)` | commit `eef3d10` 之前 `apps/desktop/src/lib.rs:225` | `★★★` |
| **这一环已被改写**：`build()` 的 Err 现在落盘 `logs/startup.log` + `exit(1)`，不再 panic | commit `eef3d10`；`docs/verification` 本文件 §2 | `★★★` |

**关键推论**：事故里 `panic = "abort"` 的作用是**把 panic 放大成不可读的信号**。
`task-1` 把 panic 从「启动失败」这条路径上拿掉之后，剩下的问题是：
**还要不要为未预见的 panic 保留 abort 这个放大器的失败语义？** §3/§4 回答它。

### 1.1 补记（同日，另一条工作流的发现）：还有第二条启动 panic，很可能才是 0.8.38 的真凶

上面画的是「setup 返回 Err」那条链。同一天在 `apps/desktop/src/lib.rs` 的 `setup`
闭包里发现了**第二条、独立**的 panic 源，而它更贴合 0.8.38 的现场
（「双击启动即 abort、连一条 Err 都没有」）：

```rust
// 修复前（截至 task-1 的提交 eef3d10，树里仍是这个写法）
tokio::spawn(async move { /* 意图判定节拍 */ });
```

`setup` 回调体**不在 tokio runtime context 里**（Tauri 用自己的全局 runtime
handle 驱动 setup 与插件初始化，见 `tauri-2.11.5/src/async_runtime.rs` 模块文档
与 `static RUNTIME: OnceLock<GlobalRuntime>` / `pub fn spawn`，`:29`、`:103-114`），
所以裸 `tokio::spawn` 会 panic：
`there is no reactor running, must be called from the context of a Tokio 1.x runtime`。
`panic = "abort"` ⇒ **SIGABRT**。正确写法是同文件下方 `bootstrap` 一直在用的
`tauri::async_runtime::spawn`。

* 本文写作时，工作区里那处**已由另一条工作流**改成 `tauri::async_runtime::spawn`
  （当时未提交）—— 本补记只记录事实与判据，**不冒领那个修复**。
* 其余 `tokio::spawn` 站点（`commands/core.rs:290`、`commands/globe.rs:210,214`、
  `traffic.rs:53`）都在 **`async fn` / async 命令体内**（Tauri 经
  `async_runtime::spawn` 执行），context 存在 ⇒ 不受影响。
  复算：`grep -rn 'tokio::spawn' apps/desktop/src`，逐个看所在函数是不是 `async fn`。

**这暴露了 §6 判据的一个盲区**：裸 `tokio::spawn`（无 runtime）是**运行时语义**错误，
不是 panic 家族**调用**，逐行扫描抓不到。它需要的判据是
「`setup` 闭包体内不许出现 `tokio::spawn`」这种源级守卫，或把启动路径上所有
spawn 统一走 `tauri::async_runtime::spawn`。**本文件不声称已覆盖这一类。**

---

## §2 事实核对（每条都给出可复算命令）

### F1 `setup` 失败 → `build()` Err → 我们原来的 `.expect` panic。`★★★`

```bash
T=$(ls -d "$CARGO_HOME"/registry/src/*/tauri-2.11.5 | head -1)
sed -n '2528,2532p' "$T/src/app.rs"
```

```
  if let Some(setup) = app.setup.take() {
    (setup)(app).map_err(|e| crate::Error::Setup(e.into()))?;
  }
```

⇒ **`setup` 闭包「任何失败都不得返回 Err」是必须的**，不是风格偏好。
本仓库现状（`task-1` 守卫测试钉住）：`setup` 内没有 `?`、没有 `return Err(`，
以 `Ok(())` 结束。

### F2 Tauri 2.11.5 **没有**命令级 `catch_unwind`。`★★★`

```bash
T=$(ls -d "$CARGO_HOME"/registry/src/*/tauri-2.11.5 | head -1)
grep -rn 'catch_unwind' "$T"; echo "exit=$?"
```

实测输出：**无匹配，退出码 1**（整棵 `tauri-2.11.5` 源码树里一次都没有）。

⇒ **unwind 不提供「命令 panic 就地转成 Err 返回前端」**。谁想要这个语义，
只能自己做 `catch_unwind` 包装；`panic = "abort"` 与 `"unwind"` 都**没有**它。

> 补充（异步命令的落点，`★★★`）：async 命令的响应走
> `tauri-2.11.5/src/ipc/mod.rs:329` / `:375` 的 `crate::async_runtime::spawn(...)`，
> 而该函数就是 `tokio::spawn`（`tauri-2.11.5/src/async_runtime.rs:103-114`）。
> tokio 会**接住任务里的 panic**（`JoinHandle` 返回 `JoinError::is_panic()`，
> 见 [tokio `JoinHandle` 文档](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html)：
> "Because panics in the spawned task are caught by Tokio"）⇒ 这条路径下
> `unwind` 确实能让**进程活着**，`abort` 会让整个 App 死。这是 §4 里那条真实代价。
> **同步命令**在 IPC 处理线程上**内联执行**（生成的 wrapper 里没有 `spawn`：
> `grep -n spawn tauri-macros-2.6.3/src/command/*.rs` 无输出），panic 会沿着
> FFI 回调边界走 —— 那种情况下 unwind 是否也 abort，**本次没做真机复现（`★`）**。

### F3 `panic` **不允许**写在 package 级 profile 覆盖里。`★★★`

```bash
mkdir -p /tmp/panic-probe/a/src /tmp/panic-probe/b/src
printf '[package]\nname="a"\nversion="0.1.0"\nedition="2021"\n' > /tmp/panic-probe/a/Cargo.toml
printf '[package]\nname="b"\nversion="0.1.0"\nedition="2021"\n' > /tmp/panic-probe/b/Cargo.toml
printf 'fn main(){}\n' > /tmp/panic-probe/a/src/main.rs
printf 'fn main(){}\n' > /tmp/panic-probe/b/src/main.rs
cat > /tmp/panic-probe/Cargo.toml <<'EOF'
[workspace]
resolver = "2"
members = ["a", "b"]
[profile.release]
panic = "abort"
[profile.release.package.a]
panic = "unwind"
EOF
cd /tmp/panic-probe && cargo metadata --format-version 1 >/dev/null
```

实测输出：

```
error: failed to parse manifest at `/tmp/panic-probe/Cargo.toml`
Caused by:
  `panic` may not be specified in a `package` profile
```

⇒ **「App 用 unwind、helper 仍 abort」写不进 `[profile.release]` 段**。

### F4 自定义 profile 能表达它，但要改构建脚本（超范围）。`★★★`

```bash
cat > /tmp/panic-probe/Cargo.toml <<'EOF'
[workspace]
resolver = "2"
members = ["a", "b"]
[profile.release]
panic = "unwind"
[profile.release-helper]
inherits = "release"
panic = "abort"
EOF
cd /tmp/panic-probe && cargo build --profile release-helper   # 通过
```

可行，但 helper 的产物路径会变成 `target/release-helper/…`；而发版脚本固定用
`--release` 构建整个 workspace（`scripts/package-macos.sh:130`
`cargo build --release --workspace`），要让 helper 用自定义 profile 就得改脚本，
**而 `task-6` 的允许改动只有 `Cargo.toml` 的 `[profile.release]` 段 + 本文件**。
⇒ 这条路不是「技术上不可能」，是「本卡范围内不可交付」。已登记为重新评估项（§7）。

### F5 panic hook 在 **abort 之前**一定执行 —— 两条策略都保留证据。`★★★`

隔离探针（`[profile.release] panic = "abort"`，hook 里落盘一个文件后 `panic!`）：

```bash
# 源码：std::panic::set_hook(Box::new(|info| fs::write(path, format!("HOOK RAN {loc}")))); panic!("probe panic")
./probe_abort /tmp/hook-abort.txt; echo $?
```

实测：

```
hook_file: HOOK RAN src/main.rs:7        ← hook 真的跑了，文件写出来了
exit_code=134                            ← 128 + 6 = SIGABRT
```

`unwind` 版同一探针：hook 同样落盘，退出码 `101`（不是信号）。

⇒ **`panic = "abort"` 不会吞掉 `apps/desktop/src/lib.rs` 的 panic hook**；
`ptrace`/崩溃报告里没有可读消息的问题是 hook 缺失造成的，不是 abort 造成的。
**hook 必须保留**（`task-6` 要求 4）：它是两条策略下唯一稳定的「文件:行号」来源。

### F6 `cargo test --release` **不受** release profile 的 `panic = "abort"` 影响。`★★★`

隔离探针（release profile `panic = "abort"`，测试里 `catch_unwind(|| panic!())`）：

```bash
cargo test --release
```

实测：测试**通过**（`1 passed`）—— Cargo 对 test/bench 的 harness 强制 unwind
（否则测试框架无法报告失败）。⇒ 用 `cargo test --release -p xraytun-desktop`
做发行档验证是**有效**的，不会被 abort 干扰。

**本仓库实测（`panic = "abort"`，即现状）**：

```
$ cargo test --release -p xraytun-desktop
test result: ok. 300 passed; 0 failed; 5 ignored   （unittests，305 run）
test result: ok. 8 passed; 0 failed                （tests/type_contract）
exit=0
```

### F7 体积与启动时间的影响

* **App 二进制体积差**：**已测**（§3）—— `abort` 10,891,232 B → `unwind`
  13,437,456 B，**+2,546,224 B（+23.38%）**。
* **启动时间**：**没测**。理由：unwind 不改变启动路径上的工作量（没有异常发生就
  没有 unwind 代价），要测出差异得做带误差棒的多轮冷启动采样，本次没有做；
  不编数字。

---

## §3 体积实测（同一份源码，只换 `panic`）

命令（**不修改 `Cargo.toml`**，用 `--config` 只影响这一次构建）：

```bash
export CARGO_HOME=<...> CARGO_TARGET_DIR=<独立 target>
cargo build --release -p xraytun-desktop
stat -f%z "$CARGO_TARGET_DIR/release/xraytun-desktop"          # abort（当前 Cargo.toml）
cargo build --release --config 'profile.release.panic="unwind"' -p xraytun-desktop
stat -f%z "$CARGO_TARGET_DIR/release/xraytun-desktop"          # unwind
```

<!-- SIZE-TABLE-START -->
| 构建 | 二进制 | 字节 | MiB | 相对 abort |
|---|---|---|---|---|
| `panic = "abort"`（现状） | `target/release/xraytun-desktop` | **10,891,232** | 10.39 | — |
| `panic = "unwind"` | `target/release/xraytun-desktop` | **13,437,456** | 12.81 | **+2,546,224 B（+23.38%）** |
| 参考：隔离探针（trivial bin，只落盘 hook） | `probe_abort` → `probe_unwind` | 421,888 → 422,688 | 0.40 | +800 B（+0.19%） |
<!-- SIZE-TABLE-END -->

**两次构建的原始尾部**（`lto = "thin"`、`codegen-units = 1`、`strip = true`，
同一份工作区源码，仅 `panic` 不同）：

```
# abort（当前 Cargo.toml）      Finished `release` profile [optimized] target(s) in 8m 08s
# --config panic="unwind"       Finished `release` profile [optimized] target(s) in 5m 43s
```

⇒ **App 上 unwind 的代价是 +2.43 MiB / +23.4%**（不是探针那种 0.19% 量级）——
薄 LTO + 单 codegen-unit 下 unwind 的落地垫被内联进大量调用点，探针**严重低估**了它。
这**不是**本决策的主理由（主理由是 §4 的 F1/F2/F3），但它让「顺手改成 unwind」这件事
从「几乎免费」变成「要拿 2.4 MiB 换一条未经真机验证的收益」。

> 探针一栏只是**量级参考**（一个几乎不依赖任何 crate 的 bin），不是 App 的结论；
> App 那两行才是本仓库自己的数字。

---

## §4 决策：保留 `abort`（不动 `Cargo.toml`）

| | A. 保留 `panic = "abort"`（**采纳**） | B. 改为 `panic = "unwind"`（不采纳） |
|---|---|---|
| App 的启动失败语义 | `task-1` 已改成「落盘 + exit(1)」；abort 不再参与 | 同左（这条已与 panic 策略无关） |
| 未预见 panic（后台 tokio 任务） | 整个 App 退出（hook 留下位置） | tokio 接住，App 活着（F2） |
| 未预见 panic（同步命令 / FFI 边界） | SIGABRT | 大概率仍 abort / 行为未验证（F2 `★`）——**不是确定的收益** |
| 单条命令 panic → 前端 Err | 不支持（Tauri 无 `catch_unwind`，F2） | 同样不支持（F2） |
| helper（root 守护进程） | 保持 abort：路由/DNS 改到一半宁可 fail-stop | **也会变 unwind**（package 级覆盖被禁，F3），与「只对 App」的初衷相反 |
| 发版范围 | 本卡零改动 | 要么全 workspace 一起变，要么新增自定义 profile + 改 `scripts/package-macos.sh`（超本卡范围，F4） |
| 体积 | App 二进制最小（10.39 MiB） | **+2.43 MiB / +23.38%**（§3 实测：13,437,456 B） |
| 诊断证据 | hook 仍先跑并落盘（F5） | 同左 |

**为什么采纳 A（逐条对应上面的证据）**

1. **事故根因已在代码层消除**（F1 + `task-1`）：启动失败不再经过 panic，
   `abort` 不再是「可读错误 → `abort() called`」的转换器。
2. **B 承诺的主要收益有一半是空的**（F2）：Tauri 不捕获命令 panic，
   `unwind` 不带来越权之外的命令级恢复；真正拿到的只有「tokio 任务 panic 被隔离」。
3. **B 在允许的改动范围内无法只对 App 生效**（F3/F4）：package 级 `panic`
   被 Cargo 直接拒绝；自定义 profile 需要动发版脚本。若强行让
   `[profile.release]` 全局 unwind，**root helper 也变成 unwind** —— 对一个
   正在改路由/DNS 的 root 进程，半途 panic 后继续运行比 fail-stop 更危险。
4. **A 的代价被如实记录**：tokio 后台任务（`bootstrap` / `version_check` /
   intent 节拍 / DNS 探测 / 重连）里若有未预见 panic，App 会整体退出；
   缓解手段是 `task-1` 的源级守卫 + 下次启动的遗留回滚（`lib.rs::bootstrap`）。

**A 的代价（不回避）**

* 后台任务的一次意外 panic = 用户看到 App 闪退。当前的理由是：这些任务的
  panic 目前全部来自「已被 `task-1` 消灭的写法」；等 F2 里的同步/异步边界
  在真机上验清、或上游提供命令级隔离后，可以重新评估（§7）。
* 体积比 unwind 小 **2.43 MiB（23.4%）**（§3 实测），这只是顺带，不是决策依据。

---

## §5 与 panic hook 的协调（要求 4）

* **hook 保留，不加任何 cfg 条件**：F5 证明 `abort` 下 hook 先执行、落盘成功。
  hook 是两条策略下唯一稳定的「`文件:行:列` + 消息 + backtrace」来源。
* **两个文件、两种成因，不合并**：
  * `logs/panic.log`：panic（hook 追加写，带 backtrace）；
  * `logs/startup.log`：`Builder::build` 返回 Err（`task-1` 覆盖写，不是 panic）。
  合并会让「这是 panic 还是启动失败」在下一次事故里更难判。
* `strip = true` 仍会剥符号 ⇒ backtrace 只有地址；hook 里优先保证的
  **位置字符串**不依赖符号，继续有效。

---

## §6 强制机制（这句话怎么保证不被写回去）

`apps/desktop/src/lib.rs` 的测试
`production_source_has_no_panic_family_calls`（`task-1`，commit `eef3d10`）：

* 递归扫 `apps/desktop/src/**`，按 `#[cfg(test)]` **逐项**跳过测试代码
  （不能按第一个 `#[cfg(test)]` 截断 —— `version_check.rs:221`、`supervisor.rs:377`、
  `commands/incident.rs:30` 这些测试专用项夹在生产代码中间）；
* 断言生产代码里没有 `.unwrap()` / `.unwrap_unchecked()` / `.expect(` /
  `panic!` / `unreachable!` / `todo!` / `unimplemented!` / `std::process::abort`；
* **同时断言扫到 > 5000 行生产代码**（防空转假绿），并用两条负例/正例测试
  证明判据有牙且不误伤（`panic_family_guard_catches_a_planted_unwrap`、
  `panic_family_guard_allows_non_panicking_fallbacks`）。

复算：

```bash
export CARGO_HOME=<...> CARGO_TARGET_DIR=<独立 target>
cargo test -p xraytun-desktop production_source_has_no_panic_family_calls -- --nocapture
```

### 已知盲区（如实记，禁止当成已闭环）

1. **切片/索引越界不在这条判据里**（判据是逐行文本，抓的是 panic 家族**调用**：
   索引 `a[i]`、切片 `&s[i..]`、整数除法在文本上没有可抓的特征）。
   本次只把 `commands/diagnostics.rs` 里两处切片改成 `str::get`（`char_at`），
   其余切片位置的安全前提写在各自注释里。
2. **宏展开**看不到：第三方 crate 内部的 panic、`unwrap` 的重新导出、
   自定义宏展开出的 panic 都不在扫描面内。
3. **测试段识别的启发式**：按 `#[cfg(test)]` 的缩进与收尾 `}` 跳过；
   若将来出现没有花括号、也不是 `use …;` 的 `#[cfg(test)]` 项，
   可能多跳/少跳。负例测试与「扫到 > 5000 行」的断言是它的护栏。
4. **不扫依赖、不扫 helper**：本卡范围是 App 侧
   （`apps/desktop/src/**`），`crates/xt-helper`、`crates/xt-tun` 的策略另卡。
5. **抓不到「运行时语义」型 panic**：`setup` 里的裸 `tokio::spawn`
   （无 runtime context ⇒ panic）就是一类 —— 它不是 panic 家族调用，
   行扫描看不见（§1.1）。这类要靠专门的源级守卫（例如「`setup` 闭包内不许
   `tokio::spawn`」）或统一走 `tauri::async_runtime::spawn`。

---

## §7 重新评估的触发条件

任一命中就重开这张卡：

1. 上游落地 [tauri#12815](https://github.com/tauri-apps/tauri/issues/12815)
   （`setup` 失败优雅返回）或提供命令级 panic 隔离；
2. 再出一次崩溃，且**位置落在 tokio 后台任务里**（unwind 本可以让 App 活着）；
3. Cargo 允许 `panic` 做 package 级覆盖（那时「只对 App」是零脚本改动）；
4. 实测在同步命令 / FFI 边界上，`unwind` 与 `abort` 的失败语义**确有一致差异**
   （F2 的 `★` 被升级为已验证事实）。

---

## §8 复算清单（一次跑完）

```bash
export CARGO_HOME=<...> CARGO_TARGET_DIR=<**自己的** target dir>   # 共享 target 会互相污染

# ① 本卡的两道门禁
cargo test -p xraytun-desktop                 # 期望：0 failed；测试数不少于基线（295 run）
cargo clippy -p xraytun-desktop --all-targets -- -D warnings   # 期望：exit 0

# ② 生产无 panic 家族调用（含负例）
cargo test -p xraytun-desktop production_source -- --nocapture
cargo test -p xraytun-desktop panic_family_guard -- --nocapture

# ③ 发行档验证（test profile 强制 unwind，见 F6）
cargo test --release -p xraytun-desktop

# ④ 上游事实（F1/F2）
T=$(ls -d "$CARGO_HOME"/registry/src/*/tauri-2.11.5 | head -1)
sed -n '2528,2532p' "$T/src/app.rs"
grep -rn 'catch_unwind' "$T"; echo "exit=$?"   # 期望：无输出、exit=1
```
