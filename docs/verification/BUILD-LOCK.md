# 构建锁：`check.sh` 与并发 cargo 共用 `CARGO_TARGET_DIR` 的**假红**，以及它的机制

> 一句话：**`scripts/check.sh` 现在会先拿一把锁**（`scripts/build-lock.sh` 实现）。
> 拿不到就**打印谁持有**并等待，超过 `BUILD_LOCK_WAIT`（默认 3600s）**明确失败（退出码 75）**，
> 绝不「等超时后继续跑」。手写 cargo 用 `./scripts/build-lock.sh run -- <命令>` 走同一把锁。

## 1. 由来：一次**看起来像产品缺陷**的假红

2026-09-22 14:46:19，tester 在修订 `6aa3b5e`（= `v0.8.34^{commit}`）上独立跑
`./scripts/check.sh --no-release-build`，得到 **exit 1**：

* **逐项全绿**：前端 21 文件 `208 passed | 1 todo`、`tsc`、CSS token、前端构建、
  站点版本一致性 6/6、`clippy` 零 warning、六套单测 `172 / 6 / 226 / 13 / 22 / 71`；
* **只有最后一步红**：`Doc-tests xraytun_desktop_lib` →
  `error[E0463]: can't find crate for xt_proto / tauri / xt_core / tokio / tracing_subscriber`，
  **而那条 rustdoc 命令行确实传了这些 `--extern`**。

**根因证据**：同一时刻 `pgrep` 显示**另一个人正在同一个 `CARGO_TARGET_DIR` 上跑 `cargo test`**；
等它的文件锁释放后单独跑那一步 ⇒ `cargo test --workspace --doc` **EXIT 0**。
**权威对照**：同一套检查在隔离 runner 上全绿 —— CI `35696466193` / `35694470992` / `35693699077`。

⇒ **产物没问题，是并发把门禁弄红了。** 而 `check.sh` 是本项目**唯一的发版前门禁**，
「门禁失败」与「产品坏了」必须能区分 —— 所以这条教训不能只是纪律（「别并行跑」），得变成机制。

## 2. 机制：锁在哪、怎么实现

| 项 | 做法 |
|---|---|
| 原语 | **原子 `mkdir` 锁目录**（macOS 没有 `flock(1)`），不引入任何新依赖 |
| 位置 | `${CARGO_TARGET_DIR}.lock.d` —— 挂在**被保护的那个 target dir 旁边**；用不同 `CARGO_TARGET_DIR` 的人天然互不阻塞（语义正确：他们本来就不冲突） |
| 内容 | 目录里的 `owner` 文件：`PID / PPID / CMD / STARTED_EPOCH / STARTED_ISO / HOST` |
| 获取 | 打印一行 `🔒 已获取构建锁：pid=… 命令=… 开始=…` |
| 释放 | `trap … EXIT INT TERM HUP`，打印 `🔓 已释放构建锁：pid=… 持有 Ns`；**只删自己持有的锁**（owner pid 必须等于自己） |
| 拿不到 | 打印持有者（pid / 命令 / 开始时间 / 已持有时长）+ 每 5 秒重复一行；超过 `BUILD_LOCK_WAIT` ⇒ **退出码 75** 并再次打印持有者。**不会静默等待，也不会无锁继续** |

### 死锁自救（三种 stale 判定，都会打印 `stale`）

1. 持有者 pid **已不存在**（`kill -0` 失败）⇒ 接管；
2. 锁目录存在但 `owner` 缺失且持续 **>30s**（区分「写入前的极短竞态」与「残骸」）⇒ 接管；
3. 持有时间 > `BUILD_LOCK_STALE_MAX`（默认 **14400s / 4 小时**；正常一次门禁是 15–30 分钟）⇒ 警告并接管。

## 3. 用法

```bash
./scripts/check.sh --no-release-build         # 门禁自己会拿锁（无需额外操作）

# 手写 cargo 请走同一把锁（尤其是「只跑一套单测」这种短命令）：
./scripts/build-lock.sh run -- cargo test -p xt-core --lib
./scripts/build-lock.sh status                # 谁持有？持有多久？
./scripts/build-lock.sh hold --seconds 5 --label demo   # 验证/演示用

# 验证这把锁真的在挡并发（红/绿都跑得出来）：
./scripts/verify-build-lock.sh                # 期望：pass=12 fail=0（锁生效）
./scripts/verify-build-lock.sh --sensitivity  # 期望：出现「预期的红」⇒ 验证真的在验锁
```

环境变量：

| 变量 | 默认 | 含义 |
|---|---|---|
| `BUILD_LOCK_WAIT` | `3600` | 拿不到锁时最长等待秒数；超时 ⇒ 退出码 75 |
| `BUILD_LOCK_STALE_MAX` | `14400` | 超过这么久的持有视为 stale（挂死/被 kill -9） |
| `BUILD_LOCK_DIR` | `${CARGO_TARGET_DIR}.lock.d` | 直接指定锁目录 |
| `BUILD_LOCK_STRICT` | `0` | `1` ⇒ 探测到**与我们同一个 target dir**（或拿不到 target dir，保守）的未持锁 cargo/rustc 时直接失败（75）；**跨 target dir 只提示**（见 §4.2） |
| `BUILD_LOCK_FOREIGN_WAIT` | `0` | `>0` ⇒ **可视地等同一 target dir 的**未持锁编译进程结束（每 5 秒一行、带已等秒数），等不到仍 75；**不等**别的 target dir（见 §4.2） |
| `BUILD_LOCK_DISABLE` | — | **只用于敏感性验证**：跳过锁并打印醒目警告（默认不开） |

## 4. ⚠️ 诚实的边界：这把锁**挡不住裸 `cargo`**，而且**一把锁只管一个 target dir**

那次假红的对手是**裸 `cargo test`**（两个人各跑一条，都没走这把锁）。
⇒ 锁只能串行化**愿意用锁的**进程。对裸 cargo，机制至少做到**说话**。

### 4.1 per-target-dir ⇒ **隔离 worktree 与主树不互斥**（这是刻意的）

锁目录 = `${CARGO_TARGET_DIR}.lock.d`。所以主树（`.cargo-target`）与隔离 worktree
（`scripts/wt.sh` 给的 `.cargo-target.wt/<名字>`）**各持各的锁，同时编译不互相阻塞** ——
那是 `task-112` 刻意建立的隔离，两者产物身份互不影响。
**推论**：任何「系统上有一个 cargo 在跑就拦」的判据都必然误报，见 §4.2。

### 4.2 strict 的**精确语义**（`task-162` 收窄）

`BUILD_LOCK_STRICT=1` **只对「与我们同一个 target dir（或拿不到 target dir）的未持锁编译进程」失败（75）**；
**跨 target dir 的并发只提示**，并在提示里打印对方的 target dir。

判定顺序（证据越弱，结论越保守）：

| # | 证据 | 结论 |
|---|---|---|
| 1 | 命令行里的 `CARGO_TARGET_DIR=<dir>` / `--target-dir <dir>` / rustc 的 `--out-dir <dir>`（按 `<target>/debug|release` 折算） | 明确 |
| 2 | `ps eww -p <pid>` 里的 `CARGO_TARGET_DIR=<dir>`（**有些环境禁止 `ps`**） | 明确 |
| 3 | `lsof -p <pid>` 里形如 `<target>/debug|release/…` 的**打开文件** ⇒ 反推 target dir（**不依赖 `ps`、也不依赖环境变量**） | 明确 |
| 4 | 以上都没有，但**可执行体确实是** `cargo`/`rustc` | `unknown` ⇒ **保守当作同一 target dir**（仍然 75） |
| 5 | **可执行体不是**编译进程（`bash`/`sh`/`env`/`python3`…，只是**命令行里含** `cargo` 字样） | **不计入**（只提示） |

两条真实事故（都是**旧**判据造成的，且都已进本文件的验证用例）：

* **v0.8.36**：隔离 worktree 里 tester 在编译 ⇒ 主树发版门禁**空跑 75**（10 秒退出，没跑任何检查）；
* **v0.8.37**：`pgrep -f` 匹配的是**命令行文本** —— 一个「内容里写着 `cargo test …` 的 heredoc
  包装进程」被当成并发构建 ⇒ 再次 75。

* 硬失败：`BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build`（发版前推荐；消息会点名
  「**与我们同一个 target dir**」以及数量，不会与「产品坏了」混淆）；
* **可见等待**：`BUILD_LOCK_FOREIGN_WAIT=<秒>` —— **只等**同一 target dir（含 `unknown`）的进程，
  每 5 秒打一行（带已等秒数）；等不到仍然 75；**不等**别的 target dir
  （等一个与我们无关的构建没有意义，也正是一次「假红」的来源）；
* **把裸 cargo 变成走锁的**才是根治：请用 `./scripts/build-lock.sh run -- …`。

## 5. 验证（原始输出见 `/tmp/ops-lock-verify-*.log`，命令可复跑）

```
$ ./scripts/verify-build-lock.sh
  [1] 一个进程持锁时，第二个必须等待并打印持有者  → 打印「等待构建锁」+ 持有者 pid，退出码 75
  [2] 持锁时再启动一个真的 check.sh              → 退 75，且**没有越过锁**（未出现第一步）
  [3] 两个 CLI 排队：执行区间不许重叠              → 时序文件显示 start/end/start/end（串行）
  [4] stale 锁必须能自救（假持有者 pid 已死）      → 打印 stale 并接管，命令正常执行
  [5] strict 判据只对**同一个 target dir** 失败（task-162）→ 5a–5e 五种情形（见下）
  [6] 结束后锁必须已释放（无残留）                 → 锁目录已清理
  pass=21 fail=0   ✓ 构建锁验证通过

$ ./scripts/verify-build-lock.sh --sensitivity     # 拿掉机制（副本去掉 acquire / 突变判据）
  ✗（敏感性/预期的红）第二个进程确实一起跑了
  ✗（敏感性/预期的红）去掉锁后 check.sh 越过锁开始跑
  ✗（敏感性/预期的红）去掉锁后两个 CLI 的区间重叠
  ✗ 5a 跨 target dir 被判 75（期望 0）        ← MUT-B：分类恒为 same（= 不看 target dir）
  ✗ 5c 只靠 lsof 证据时被判 75（期望 0）      ← 同上
  ✗ 5d 包装进程被判 75（期望 0）              ← MUT-A：可执行体判定恒真（= 回到匹配命令行文本）
  pass=8 fail=8   ✓ 敏感性成立：出现 8 条红 ⇒ 验证真的在验锁
```

### 5.1 `[5]` 的五种情形（`task-162` 的双向敏感性）

| 用例 | 夹具（注入 `BUILD_LOCK_PROC_TABLE` / `BUILD_LOCK_ENV_TABLE` / `BUILD_LOCK_LSOF_TABLE`） | 期望 |
|---|---|---|
| 5a | 真 cargo + `CARGO_TARGET_DIR=/tmp/some-other-target` | **不** 75，提示里点名「别的 target dir」 |
| 5b | 真 cargo + `CARGO_TARGET_DIR=<我们的 target dir>` | **75**，报错点名「同一 target dir」（`task-109` 的安全属性） |
| 5c | 真 cargo + **只有 lsof 证据**（`…/debug/deps/…`），没有环境变量 | **不** 75 |
| 5d | `/bin/bash -c '… cargo test …'`（命令行含 `cargo`，可执行体是 bash） | **不** 75，并说明「不是编译进程 ⇒ 不计入」 |
| 5e | 真 cargo，**任何证据都拿不到** | **75**（保守当作同一 target dir） |

> 这些夹具走的是脚本里**明确登记的测试缝**（`BUILD_LOCK_PROC_TABLE` / `BUILD_LOCK_ENV_TABLE` /
> `BUILD_LOCK_LSOF_TABLE`，只在 `scripts/verify-build-lock.sh` 里设置）；生产路径不设它们。
> 用缝而不是真起进程，是因为「造一个别的 target dir 的真 cargo」既慢又不可控（还要动真 target dir）。

> 敏感性模式下，**去掉锁的 `check.sh` 一旦越过锁点就被 kill** —— 既证明「锁没了就真会一起跑」，
> 又不真的在生产机上并发编译。

### 5.3 诚实清单（`task-162` 的判据**覆盖不到**的）

* **拿不到对方的 target dir 时，我们保守地当它「与我们同一个 target dir」**（仍然 75）。
  这不是缺陷而是取舍：**安全属性（同一 target dir 的并发不许静默通过）优先于「少一次误报」**。
  能少误报的证据依次是：命令行 → `ps eww` 环境 → `lsof` 打开文件；三条都拿不到才会保守 75
  （真构建几乎总能在 `lsof` 里看到 `<target>/debug|release/…`）。
* **`ps` 在某些受限环境会被禁止**（本机 ops 沙箱就是：`/bin/ps: Operation not permitted`）⇒ 第 2 条证据
  会直接跳过；**这也正是要加 `lsof` 那条证据的原因**。脚本对 `ps` 不可用是**静默降级到下一证据**，
  不会失败；
* **`lsof` 也可能被禁止/超时**：拿不到就同样降级；
* **「同一 tag / 同一 target dir」这类身份判断只看本机进程表**：如果对方的 cargo 起在一个**我们看不见的
  环境**（别的容器/VM、别的用户且 `lsof` 无权限），我们只能保守判 75；
* **测试缝**（`BUILD_LOCK_PROC_TABLE` / `BUILD_LOCK_ENV_TABLE` / `BUILD_LOCK_LSOF_TABLE`）是**生产
  代码里的注入点**：它们只在 `scripts/verify-build-lock.sh` 中被设置；如果有人误在生产里设置，
  就会把「进程表」换成文件内容 —— 这是**已知的、登记的**代价（换来的是判据可离线、可双向敏感性验证）；
* 判据**不检查锁归属**（谁持有锁）：它只看「有编译进程没走锁 + 是否同一个 target dir」。
  真正的串行化仍然只对**愿意走锁的**进程生效（§4 第一段）。

### 5.2 完整门禁（干净修订 + 无并发）

在**隔离 worktree**（`git worktree add --detach /tmp/wt-lock109 6144d3c`，再放进本卡的 4 个文件；
主工作区里当时有别人的 `crates/xt-core/src/store.rs` 在途改动，所以不在主工作区跑）上，
**先等到 `pgrep -f "cargo|rustc"` 为空**才开始：

```
$ CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target ./scripts/check.sh --no-release-build
  🔒 已获取构建锁：pid=80300 命令=scripts/check.sh --no-release-build 开始=2026-09-22 15:25:25 +0800
  （本次**没有**出现「检测到没有持锁的 cargo」警告 ⇒ 确实无并发）
  ✓ 站点声明的版本与 Cargo.toml 一致：0.8.34
  Test Files  21 passed (21) ｜ Tests  208 passed | 1 todo (209)
  Rust：172 / 6 / 223 / 13 / 22 / 71（另 4 套 doc-test 0）
  （--no-release-build：跳过 release 构建，发版流程里由 package-macos.sh 代劳）

  ✓ 与 CI 相同的全部检查通过
  🔓 已释放构建锁：pid=80300 持有 21s
CHECK_LOCK2_EXIT=0
```

另外记录一次**带并发**的运行（同一 worktree，但没有先等空闲）：开跑时 `pgrep` 有 **76 个**未持锁的
cargo/rustc（另一位成员 `task-107` 的敏感性实验），日志里出现了那条警告；那次也 **exit 0**，
但它**不能**当作「无并发」的证据 —— 所以上面那次才是本卡的验收依据。

## 6. 发版前的门禁怎么跑（**固化为流程**）

```bash
BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build
```

**为什么加这个变量**：门禁唯一的职责是回答「这份代码能不能发」。14:46 那次假红说明，
在**有未持锁的 cargo 并发**时，`check.sh` 可能给出**看起来像产品缺陷**的红
（`Doc-tests` → `E0463: can't find crate`）。加 `BUILD_LOCK_STRICT=1` 后，同样的情形会**明确失败**：

```
  ⚠️  检测到 N 个**没有持锁**的 cargo/rustc 进程 —— 本锁挡不住它们（它们不会看到这把锁）：
       <pid> <命令>（折行、截断 140 字符、最多列 5 条）
  ✗ BUILD_LOCK_STRICT=1：检测到未持锁的 cargo ⇒ 明确失败（退出码 75），不带着未知并发去跑门禁
```

**失败长什么样、怎么读**：

| 退出码 | 含义 | 该怎么办 |
|---|---|---|
| `0` | 全绿 | 可以发 |
| `75` | **环境问题：拿不到锁，或检测到未持锁的 cargo**（`EX_TEMPFAIL`） | **不要**当成产品缺陷；等对方结束，或请对方改用 `./scripts/build-lock.sh run -- <命令>`，然后重跑 |
| 其它非 0 | 检查失败（前端/站点/clippy/测试…） | 按日志修代码 |

`75` 与「检查失败」分开，是为了让「门禁失败」和「环境问题」可区分 —— 这正是这张卡存在的理由。

### 6.1 worktree 产物身份：三态判据（2026-09-23 修）

`check.sh` 在 **linked worktree** 里还会判「`CARGO_TARGET_DIR` 是否指向主工作区」（产物身份，防链到旧 rlib）。
判据是**三态**，只有**两侧都成功取到真实路径**才比较：

| 情形 | 行为 |
|---|---|
| 两侧都能解析、且**相同** | ⚠️ 共享警告；`WT_STRICT=1` ⇒ **75** |
| 两侧都能解析、且**不同** | 不打印任何东西（正常） |
| **任一取不到**（全新 checkout 还没建 target dir、或 `CARGO_TARGET_DIR` 指向不存在的路径） | ℹ️ **「无法判定」**：打印两边的值与「取不到」，写明**这是环境问题、不是代码失败**；普通模式**继续跑**，`WT_STRICT=1` ⇒ **75** |

**⚠️ 已知摩擦（不是 bug）**：**全新 checkout 上主 target dir 还不存在** ⇒ `WT_STRICT=1` 会给一次 75。
先跑一次构建（或先用普通模式跑一次门禁）让 target dir 出现即可。
（修的是旧形态：两个 `cd` 都失败时两侧空串 ⇒ 判为相等 ⇒ **假警告 + 假 75**；验证见 `scripts/verify-worktree-guard.sh`。）

**它在 CI 上是无害的 no-op**：`ci.yml` / `release.yml` 跑在**隔离 runner** 上，没有并发 cargo；
不设这个变量也完全正常（脚本末尾会打一行提示，提醒发版的人该用它）。`release.yml` 的实质语义**未改**。

**其它两个档**（按需）：

```bash
BUILD_LOCK_FOREIGN_WAIT=1800 ./scripts/check.sh --no-release-build   # 可视地等未持锁的 cargo 结束（最多 30 分钟）
./scripts/build-lock.sh run -- cargo test -p xt-core --lib           # 手写 cargo 也走同一把锁（根治办法）
```

> **优先级**：`BUILD_LOCK_STRICT=1` **先判**（检测到就立刻失败 75）；两个都设时「等待」那一档不会生效。
> 想**等一个干净窗口**就用 `BUILD_LOCK_FOREIGN_WAIT`（别带 STRICT）；发版前要**不冒险**就用 STRICT
> （task-112 实测：带 STRICT 时立刻 75，去掉后等到 `pgrep` 为空再跑 = exit 0）。
