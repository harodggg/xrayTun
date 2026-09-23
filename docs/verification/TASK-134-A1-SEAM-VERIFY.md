# task-157 · 独立验证 `task-134`（A-1 行为级接缝测试是否真的有牙）

> 冻结对象：**`96d7c70`**（`test(xt-tun/helper): A-1 行为级接缝测试（注入失败执行器）+ 守卫②③收窄成「按站点正向断言」`）
> 验证人：`tester`（**不采信**实现者的测试结论与突变结论；本文所有「有牙/绕过」结论都由我在隔离 worktree 里重做）
> 隔离：worktree `a1seam`（`96d7c70`，detached）+ **独立 target** `.cargo-target.wt/a1seam`（`scripts/wt.sh`）
> 判定（一句话）：**接缝确实只在测试里 ✓；行为级测试有牙 ✓（含「不是只要 Err 就算过」的反证）；成功路径逐字未变 ✓；守卫②③ 抓住 M3 与 4 类常规吞法 ✓ —— 但守卫仍是文本/模糊判据，我实测到 4 种「保留受检文本但运行时吞掉」的写法仍绿，其中 3 种在 helper 卸载站点且没有任何测试兜住。按「守卫未到位不进 v0.8.36」的口径，请你裁决（§7）。**

---

## §0 三分口径

| 类别 | 内容 |
|---|---|
| **我量到的** | worktree 基线的真实退出码与计数；8 个源码突变 + 1 个测试弱化突变跑出的逐条 pass/fail；全 crate 15/15 绿；非测试 IR 与生产二进制里的符号计数；nm 正/负对照 |
| **我读码得到的** | `run` 的 `#[cfg(test)]` 块与 `real_run` 直调；守卫①②③的判据实现（`contains` / 400B 窗口）；`fn uninstall` 唯一调用点；文案字面量字节比对；hunk 归属 |
| **我推断的** | release profile 下与 dev 同样无接缝（依据：`cfg(test)` 由测试构建决定，与 profile 无关；我**没有**为 release 单独出 IR）；真机「删路由失败」时的用户可见行为（`替换执行器 ≠ 真机`，见 §8） |

---

## §1 冻结对象、隔离与基线

```
$ df -g /Users/xbtg- | tail -1
/dev/disk3s5  460  397  31  93%            # avail 31 GiB
$ ./scripts/wt.sh new a1seam 96d7c70
✓ worktree: /var/folders/…/T//xraytun-wt/a1seam （ref=96d7c70）
✓ 它的 target dir（**独立**）: …/xraytun-tun/../.cargo-target.wt/a1seam
```

被验文件在 `96d7c70` 的 sha256（事后复核 `git diff 96d7c70..HEAD -- <这些路径>` = **空**，即 main 前移后这些文件仍未被动过）：

```
ae847475d2818e5dddd963542116a7e6da892c78679af89c080cd914b7a96363  crates/xt-tun/src/macos/mod.rs
4b4e31647aef2f606984b3ede8e3964bd645cdbc16a8db1fe787285b8f71c521  crates/xt-tun/src/macos/controller.rs
79e4b2610be9adf44f38e8ee1022852ac8c520e58a2bf57cbabb39f5b65ced21  crates/xt-tun/src/macos/snapshot.rs
2d49821ebf1afb6624f65fa30af13ab128eed4befa99d11bbd1dbe6067960bbb  crates/xt-helper/src/server.rs
2b4c19ce54f9dbea153686860757801014c8b2251f8bb2d8ac3446dde986ac93  apps/desktop/src/supervisor.rs
```

**基线（我跑的，唯一一次 workspace 量）：**

```
$ ./scripts/wt.sh run a1seam -- bash -c 'cargo test --workspace; echo "WS_TEST_EXIT=$?"; \
    cargo clippy --workspace --all-targets -- -D warnings; echo "CLIPPY_EXIT=$?"'
……
11 个测试二进制：560 passed; 0 failed; 6 ignored
WS_TEST_EXIT=0
CLIPPY_EXIT=0
```

（由日志聚合：`test result:` 行 11 条，全部 `ok`，failed 合计 0，`warning:` 出现 **0** 次。）

---

## §2 item 1 · 接缝真的只在测试里（三重独立证据）

### 2.1 逐行读（源码）

* `crates/xt-tun/src/macos/mod.rs:49` `pub(crate) fn run(...)`，其内 `:50` 起是 `#[cfg(test)] { if let Some(exec) = current_test_executor() { return exec(program, args); } }`，然后 `:56` `real_run(program, args)`。
* `real_run`（`:16`）**只被 `run` 调用一次**（全仓 `grep -rn "real_run"` 命中：定义 1 处、文档 1 处、调用 1 处）。
* 接缝符号的定义全部带 `#[cfg(test)]`：`mod.rs` 的 `TestExecutor`（:63）、`TEST_EXECUTOR`（:66）、`current_test_executor`（:72）、`with_executor`（:80）；`snapshot.rs` 的 `TEST_SNAPSHOT_ROOT`（:36）、`with_test_root`（:44）。
* 接缝的**调用点全部在 `controller.rs` 的测试模块内**（`:536-595`；该文件 `mod tests` 起于 :382 之前），生产段（`#[cfg(test)]` 之前）**零引用**。

### 2.2 产物级（生产构建的 LLVM IR）

```
$ cargo rustc -p xt-tun --lib -- --emit=llvm-ir     # 非测试构建
NONTEST_IR=…/debug/deps/xt_tun-86d57ee9e0f0fa3d.ll
  real_run               次数=17        ← 生产函数在
  with_executor          次数=0
  current_test_executor  次数=0
  TEST_EXECUTOR          次数=0
  with_test_root         次数=0
  TEST_SNAPSHOT_ROOT     次数=0
  snapshot_dir           次数=7         ← 生产函数在

run 的函数体（IR，`define internal void @_RNvNtCsc3PSHi7J5Fq_6xt_tun5macos3run`）：
  ; call xt_tun::macos::real_run
  call void @_RNvNtCsc3PSHi7J5Fq_6xt_tun5macos8real_run(...), !dbg !54944
  ret void, !dbg !54945
```

⇒ **生产构建里的 `run` 就是「直接尾调 `real_run` + 返回」**：没有分支、没有 thread-local 读取、没有替身类型。零额外开销、无注入入口 —— 这不是「读起来像」，而是产物里就是这么编出来的。

### 2.3 同一工具的正/负对照（nm，防止「符号不在」只是工具看不见）

```
阳性对照（测试二进制 xt_tun-958d7187e6f092d0）:
  with_executor          nm 符号数=10
  current_test_executor  nm 符号数=4
  with_test_root         nm 符号数=17
阴性对照（同一个 nm、生产二进制 debug/xraytun-helper）:
  with_executor          nm 符号数=0
  current_test_executor  nm 符号数=0
  TEST_EXECUTOR          nm 符号数=0
  with_test_root         nm 符号数=0
  TEST_SNAPSHOT_ROOT     nm 符号数=0
```

### 2.4 判定

**通过**：接缝整个在 `#[cfg(test)]`；生产路径是 `run → real_run` 的直调（IR 级别证实）；注入入口在非测试构建里**不存在**（源码 + IR + 符号三层一致）。

---

## §3 item 2 · 行为级断言有牙（4 个突变，隔离 worktree 内）

每条突变用「精确锚点 + 断言锚点唯一」应用（避免空突变假绿），跑完立即还原；**每条都核对了「测试真的跑了」**（`2 passed` / `1 passed; 1 failed`，不接受 `0 passed; 0 failed`）。

| # | 突变 | 守卫②（源码判据） | 行为测试 | 原始结果 |
|---|---|---|---|---|
| **c_a** | `force_cleanup` 退回旧写法 `let _ = rollback(&snap); Ok(Some(snap))` | **FAILED** | **FAILED** | `test result: FAILED. 0 passed; 2 failed; 71 filtered out` |
| **c_b** | `force_cleanup`：`rollback(&snap)` 失败时**删掉快照**再返回 `Err` | ok | **FAILED** | `FAILED. 1 passed; 1 failed` ← 恰好是「快照仍在盘上」那条断言开火 |
| **c_bw** | c_b **+ 把新测试弱化成只断言 `is_err()`**（不查快照） | ok | **ok（假绿）** | `test result: ok. 2 passed; 0 failed` |
| **c_c** | 把注入的第 2 步改成**成功**（`if false`） | ok | **FAILED** | `FAILED. 1 passed; 1 failed` |
| **c_g1** | 顺带：保留 `rollback(&snap)?` 字面量但**先吞掉**失败（`return Ok(Some(snap))`） | ok | **FAILED** | `FAILED. 1 passed; 1 failed` |

**结论**：
* c_a 证明「旧写法会被抓」——**守卫②与行为测试双双变红**；
* c_b + c_bw 是本卡最关键的一对：**同一处生产缺陷下，「只看 `is_err()`」的测试是绿的，而原测试因「快照仍在盘上」这条断言变红** ⇒ 该断言确实是承重的，原测试**不是「只要 Err 就算过」**；
* c_c 证明它测的是「**第 2 步失败**」而非「任何失败」；
* c_g1 说明守卫②的**文本**判据本身可绕，但**行为测试兜住了**（这是与 helper 侧最大的差别，见 §4）。

---

## §4 item 3 · 守卫②③收窄后：M3 与其它吞法（逐条结论，含 4 个仍绿）

### 4.1 helper 卸载站点（守卫③，`server.rs`，判据 = 三个 `contains`）

| # | 突变（卸载处 `if let Err(e) = controller::rollback(&session.snapshot)` …） | 守卫③ | 格式化行为测试 | 整个 xt-helper crate | 结论 |
|---|---|---|---|---|---|
| **h_v1** | `let _ = controller::rollback(…).ok();`（= 实现者写的 M3 负例） | **FAILED** | ok | — | ✅ 抓住 |
| **h_v2** | `if let Err(_) = controller::rollback(…) {}` | **FAILED** | ok | — | ✅ 抓住 |
| **h_v3** | `let _e = controller::rollback(…); drop(_e);` | **FAILED** | ok | — | ✅ 抓住 |
| **h_v4** | `match controller::rollback(…) { Err(_) => {} Ok(_) => {} }` | **FAILED** | ok | — | ✅ 抓住 |
| **h_v8** | 格式化函数无视参数（`match None::<&str>`） | ok（绿） | **FAILED** | — | ⚠️ 守卫漏，但**行为测试兜住** |
| **h_v5** | 响应前插 `rollback_failed = None;`（三个受检字面量全保留） | **ok（绿）** | ok | **15 passed; 0 failed** | ❌ **绕过** |
| **h_v6** | 第二处 `if let Err(e) = controller::force_cleanup()` 的 body 换成 `let _ = e;` | **ok（绿）** | ok | **15 passed; 0 failed** | ❌ **绕过** |
| **h_v7** | 诱饵调用 + 实际用 `None`：`{ let _decoy = uninstall_response_message(rollback_failed.as_deref()); let _ = _decoy; Some(uninstall_response_message(None)) }` | **ok（绿）** | ok | **15 passed; 0 failed** | ❌ **绕过** |

三个绕过变体**都没有任何编译诊断**（我单独跑 `cargo test -p xt-helper --no-run` 过滤 `^warning|^error`：**输出为空**）。也就是说：`-D warnings` 也拦不住它们。

**为什么没有测试兜住**（读码）：`fn uninstall`（`server.rs:677`）的唯一调用点是 `:336` 的请求分发；**没有任何测试驱动这个站点**（测试只有两类：读源码文本的守卫 + 直接调 `uninstall_response_message` 的行为测试）。所以只要「三个字面量还在」，运行时怎么吞都没人管。

**后果分级**：
* **h_v5 / h_v7 = A 级同症状**：用户可见的**响应文案**会变成「helper 已卸载」（少了「但回滚网络配置失败：…」）—— 这正是 A-1/A-2 要消灭的「用户据此得出错误结论」；
* **h_v6 = A 级（少报一处失败）**：第一处站点仍如实上报，但 `force_cleanup` 那一处失败不再进响应文案；
* h_v8 = 被 `uninstall_message_is_honest_when_rollback_fails` 兜住（守卫漏、行为兜）。

### 4.2 supervisor 核心退出站点（守卫①，`supervisor.rs`，判据 = 「前 60B 有 `if let Err(e) = ` + 后 400B 窗口内有 `tracing::warn!`/`errors.push(`/`return Err(`」）

| # | 突变 | 守卫① | 结论 |
|---|---|---|---|
| **s_ok** | `process.shutdown(CORE_SHUTDOWN_GRACE).await.ok();`（实现者的负例） | **FAILED** | ✅ 抓住 |
| **s_win** | 本站点 body 换成 `let _ = e;`（**保留** `if let Err(e) = `，靠 400B 窗内邻近的 `return Err(` 充数） | **ok（绿）** | ❌ **绕过** |

*s_win 的后果是 B 级*：外层仍会返回 `核心未在预期时间内就绪`（用户知道出了事），但**「数据面进程未干净退出」这条留痕没了** —— A-3 的口径是「至少留痕」，所以这是留痕丢失。它的成因很明确：**「400B 窗口内出现 handled」是模糊判据，可被同一段里别的语句满足**。

### 4.3 小结（要报 Lead）

* 收窄确实解决了原来的 lint 缺口：`.ok()` / `if let Err(_)` / `let _e; drop(_e);` / `match Err(_)` **全部变红**（这些是「现实写法」）；
* 但守卫是**文本/窗口判据**，凡是「保留受检文本 + 运行时吞掉」的写法都还能过。我实测到 **4 种**：h_v5、h_v6、h_v7（helper 卸载站点，**无任何测试兜住**）、s_win（supervisor 窗口充数）；另一类 h_v8/c_g1 被行为测试兜住；
* ⇒ 按你「若有一种仍能绕过，直接报我（不修完不进 v0.8.36）」的口径，本条需要你裁决。我的建议见 §7。

---

## §5 item 4 · 成功路径逐字未变

**判据（我是怎么判定「形状未变」的）**：① `git diff --name-only 96d7c70^..96d7c70` 只有 5 个文件；② 逐 hunk 归属；③ 生产段**字面量集合**比对（不是看 diff 摘要）；④ 类型清单位置（`xt-proto`）是否被碰。

```
$ git diff --stat 96d7c70^..96d7c70
 apps/desktop/src/supervisor.rs        |  86 +++++-      ← hunk 全部在 mod tests 内（起始行 2143+）
 crates/xt-helper/src/server.rs        | 111 ++++++----
 crates/xt-tun/src/macos/controller.rs | 118 ++++++++++++  ← 全部是 #[cfg(test)] 新增测试
 crates/xt-tun/src/macos/mod.rs        |  65 +++-
 crates/xt-tun/src/macos/snapshot.rs   |  36 +++-
$ git diff --stat 96d7c70^..96d7c70 -- crates/xt-proto      # 空 ⇒ IPC 类型未动
```

* **成功文案逐字比对**（旧生产段 vs 新生产段，字面量提取后比较）：
  * `"helper 已卸载；但回滚网络配置失败：{why} —— 请用「修复网络」再试一次"` —— **两版完全相同**；
  * `"helper 已卸载"` —— 两版相同（仅 `.into()` → `.to_string()`，运行时结果一致）；
  * 其它成功/失败文案（`"已回滚会话 {}"`、`"已接管默认路由（共 {n} 条）"`、`"卸载时回滚网络配置失败 —— 路由/DNS 可能仍留在系统上"`）在两版生产段都在且未改。
* **IPC 形状**：`crates/xt-proto` 本提交零改动；响应仍是 `Response::Ok { message: Some(String) }`（唯一变化是 message 的**产生方式**从内联 `match` 变成调用纯函数）。
* **App 侧调用链**：`supervisor.rs` 的两个 hunk 都在 `mod tests`（2143/2159/2167/2195），生产段一行未动；其余文件不涉及 App 调用链。
* 唯一**生产行为**变化就一处：卸载响应文案的组装从内联 match 抽成 `uninstall_response_message(...)`（**输出逐字相同**）—— 目的是让两条文案路径可以行为级测（这正是 §4 里 h_v8 能被行为测试抓住的原因）。

---

## §6 复现命令（可粘贴；全部在 worktree 内）

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
df -g /Users/xbtg- | tail -1                     # 先看余量（本次 31 GiB）
./scripts/wt.sh new a1seam 96d7c70               # 独立 worktree + 独立 target
./scripts/wt.sh run a1seam -- bash -c 'cargo test --workspace; echo WS_TEST_EXIT=$?; \
  cargo clippy --workspace --all-targets -- -D warnings; echo CLIPPY_EXIT=$?'   # 基线

# 接缝的产物级证据（item 1）
eval "$(./scripts/wt.sh env a1seam)"; cd "$(./scripts/wt.sh path a1seam)"
cargo rustc -p xt-tun --lib -- --emit=llvm-ir            # 非测试 IR
grep -c with_executor "$CARGO_TARGET_DIR"/debug/deps/*.ll # ⇒ 0
cargo build -p xt-helper && nm -C "$CARGO_TARGET_DIR"/debug/xraytun-helper | grep -c with_executor  # ⇒ 0
cargo test -p xt-tun --lib --no-run                      # 测试二进制（阳性对照）
nm -C "$CARGO_TARGET_DIR"/debug/deps/xt_tun-* | grep -c with_executor  # ⇒ >0

# 突变（item 2/3）：见 /tmp/t157-run.sh、/tmp/t157-run3.sh、/tmp/t157-run5.sh、/tmp/t157mut.py
#   每个突变：python 精确锚点替换（断言唯一）→ 跑指定测试 → git checkout -- <file> 还原 → 核对 git status 为空
./scripts/wt.sh rm a1seam                        # 收工：连它的 target dir 一起删
```

---

## §7 结论与建议（供 Lead 裁决）

**通过项**：

1. **接缝只在测试里** —— 生产 `run` 在 IR 里就是 `call @real_run; ret void`；接缝符号在非测试 IR 与生产二进制里均为 0（同一工具的正/负对照成立）；
2. **行为级断言有牙** —— 退回旧写法 ⇒ 守卫+行为双红；「返回 Err 但快照被删」⇒ 行为红，而**弱化成只看 `is_err()` 则假绿**（证明断言承重）；注入第 2 步改成功 ⇒ 红；
3. **M3 与 4 类常规吞法全部被守卫抓住**；
4. **成功路径逐字未变**（文案字节相同、IPC 类型未动、App 调用链未动、supervisor hunk 全在测试内）；
5. 基线 `cargo test --workspace` = **560 passed / 0 failed / 6 ignored，退出码 0**；`clippy --workspace --all-targets -D warnings` = **退出码 0**。

**要你裁决的（守卫未到位）**：

* **helper 卸载站点（守卫③）**：h_v5 / h_v6 / h_v7 三种写法仍绿，**整个 xt-helper 测试全绿、无编译诊断**，且其中两种会让**用户可见的响应**说谎（A 级同症状）。根因是「守卫是 `contains` 文本判据 + 卸载站点没有行为测试」。
* **supervisor 站点（守卫①）**：`s_win`（本站点吞掉、靠 400B 窗内邻近 `return Err(` 充数）仍绿 ⇒ 判据的「窗口内 handled」不指向本站点。后果 B 级（丢留痕）。
* 守卫②（controller）**同样可被文本绕过**，但**新行为测试兜住了** ⇒ 这条我认为已闭环，不需要动。

**建议（二选一，倾向 a）**：
* **(a) 给卸载站点补站点级行为测试**：把「回滚结果 → 响应文案」的决策做成可注入/纯函数（例如 `fn uninstall_outcome(rollback: Result<(), String>, force: Result<(), String>) -> (Option<String>, Response)`）并行为测试；站点只做「取值 + 调用」。这样 h_v5/h_v6/h_v7 都会在**行为层**变红，文本守卫退化成第二道保险。
* **(b) 判据结构化**：把 `contains`/400B 窗口换成对**语句级**结构的检查（例如解析出「该站点自己的 handler 块」再要求块内 handled），或直接依赖 `must_use` 类型/`#[deny]` 让编译器拦住吞法。

---

## §8 诚实清单（替身覆盖 ≠ 真机验证）

* **本卡没有在真机上做任何「删路由失败」**：全部测试都用**注入执行器**替换外部命令、用**临时快照根**（`with_test_root`）替换 `/Library/Application Support/XrayTun`（后者需要 root）。因此我验证的是「*如果*某一步失败，上层是否如实上报」，**不是**「真机上某一步确实会失败、以及真机的错误文案在界面上长什么样」。
* 接缝的产物级证据针对本机 **dev profile + `--test`** 两个配置；release profile 我**没有**单独出 IR（推断：`cfg(test)` 由测试构建决定、与 profile 无关，属推断不属实测）。
* 我**没有**装/卸助手、没有连接/断开、没有改路由或 DNS（本卡边界，且这台是生产机）。
* 突变只在 worktree `a1seam` 内进行，**main 源码一行未改**；每次突变都核对目标文件还原后 `git status` 为空（最后一条：`FINAL git status: []`）。
* 报告中的地址均为文档保留段（`203.0.113.0/24`、`198.51.100.0/24`、`utun9`）与本地构造的 fixture，**不含任何真实节点地址/UUID**。
