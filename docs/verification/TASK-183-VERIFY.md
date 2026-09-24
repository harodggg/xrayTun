# task-183（A10 修正版）独立验证：「没看到 socket」≠「没装」

* **被验对象**：`2f23d22`（`fix(helper): 「没 socket」≠「没装」—— 判据改为安装产物（task-183，A10 修正）`），
  改动 4 个文件 `apps/desktop/src/{helper_client,lib,state.rs,commands/diagnostics}.rs`（+430/−34）。
* **验证者**：`ops`（与实现者 `backend-dev` 不是同一人；**用例我自己写**，没抄他报告里的用例）。
* **验证环境**：隔离 worktree `/Users/xbtg-/deepseek-harness/.wt/t186v`（`2f23d22`），
  独立 target dir `.cargo-target.wt/t186v`；主树**一个字没碰**；收工已删（§5）。
* **本文档的三类标注**：**【量到的】**= 我实际跑出来的原始数字；**【读码】**= 读源码得到的；
  **【推断】**= 我据此推断、但**没有**在本环境验证的。

---

## 1. 【量到的】结论

| 项 | 结果 |
|---|---|
| 实现者的本卡回归（10 条） | 全部 **ok** |
| **我自己写的**真值表 T1–T9（追加在副本里，**不进 main**） | 9/9 **ok** |
| 基线（`cargo test -p xraytun-desktop --lib`，含我的 9 条） | **268 passed / 0 failed / 5 ignored**（EXIT=0） |
| 三条突变（我构造的 M1/M2/M3） | **3/3 变红**，失败原文见 §1.2；每次按 sha256 还原后复跑 **268/0** |
| 诊断文本 | **已无**「已安装=<socket 存在性>」这一说法（§1.4） |
| 三态契约（Rust 枚举 × UI 类型 × 文案表 × 按钮） | **逐项对得上**，`not_running` 有标签且「重启 helper」挂在它上（§1.5） |

> 未被本验证覆盖：真机「全新机器」「装了但没跑」两条路径、`Unreadable` 的真实触发场景、
> 以及 workspace 级测试 —— 见 §3 与 §4。

---

## 1.1 【量到的】真值表 T1–T9

我在 worktree 的 `apps/desktop/src/helper_client.rs` **末尾追加**了一个 `mod a10_verify_independent`
（副本里 876 行 vs 原 719 行；**只在 worktree，主树干净**）。九条各自独立构造：

| # | 输入 | 断言 | 结果 |
|---|---|---|---|
| **T1** | `socket_present=false` + `InstallArtifacts::Present`（**核心反例**） | `state == NotRunning`；`error` **不含「尚未安装」**；含「重启 helper」 | **ok** |
| T2 | `false` + `Absent` | `state == NotInstalled`；文案含「先安装特权助手」 | ok |
| T3 | `false` + `Unreadable("plist: permission denied (os error 13)")` | `state == Unknown`；`error` 含「无法判断」**且带原始原因** | ok |
| T4 | `true`（socket 标志在、连不上） | **不得** `NotInstalled`（保留原基线语义） | ok |
| **T5** | `humanize(NotRunning)` 与 `NOT_RUNNING_HINT` 两处文案 | 都**不得**出现「socket 文件存在」；都要解释 socket 生命周期；都要给「重启 helper」 | ok |
| T6 | `classify_install_artifacts` 四种组合 | `Absent / Present / Present / Unreadable`（含「一个读不出 + 另一个在 ⇒ Present」） | ok |
| T7 | 真磁盘三态（临时目录 + `/dev/null/a10-child`） | 存在⇒`Present`、都不在⇒`Absent`、**ENOTDIR⇒`Unreadable`**（不是「不存在」） | ok |
| T8 | `helper_startup_log` 三态 | `NotRunning` 日志**不含**「尚未安装」；`NotInstalled` 含；`Unknown` 含「无法判断」 | ok |
| **T9** | 诊断行（注入 `Present`） | **不得**出现「已安装=true/false」；必须含 `状态=not_running` / `socket存在=false` / `安装产物=存在` | ok |

命令与原文（节选）：

```bash
./scripts/wt.sh run t186v -- cargo test -p xraytun-desktop --lib
#   test result: ok. 268 passed; 0 failed; 5 ignored; ... EXIT=0
#   test helper_client::a10_verify_independent::t1_installed_but_not_running_is_never_reported_as_not_installed ... ok
#   …（T2–T9 同）…
```

> 被忽略的 5 条与本次无关（1 条需要网络、4 条是「手动证据工具」，例如
> `real_lookup_returns_a_plausible_location`、`real_upload_acceptance`）—— 口径照实列出，不混进通过数。

## 1.2 【量到的】三条突变（我自己构造，与实现者的 m1–m6 不是同一批）

每条：打突变 → 跑**全部**库测试 → 记录 → **按 sha256 还原**再复核。突变只动 worktree 副本。

**M1「改回 `Unknown`」**（把 `InstallArtifacts::Present` 那一支的 state 改回 `Unknown`）：
基线 sha256 `75941a9c…` → 突变后 `cff7ea20…`；`test result: FAILED. 266 passed; 2 failed`：

```
thread 'helper_client::a10_verify_independent::t1_...' panicked at helper_client.rs:749:
  assertion `left == right` failed: state=Unknown
    left: Unknown
   right: NotRunning
thread 'helper_client::tests::availability_without_socket_but_installed_reports_not_running' panicked at helper_client.rs:518:
  assertion `left == right` failed
    left: Unknown
   right: NotRunning
```

**M2「改回被推翻的原处方（`!socket_present ⇒ 无条件 NotInstalled`）」**：
sha256 `503104cb…`；`FAILED. 264 passed; 4 failed`：

```
thread 'helper_client::a10_verify_independent::t1_...' panicked at helper_client.rs:752:
  left: NotInstalled   right: NotRunning
thread 'helper_client::a10_verify_independent::t3_unreadable_is_unknown_and_keeps_the_reason' panicked at helper_client.rs:774:
  left: NotInstalled   right: Unknown
thread 'helper_client::tests::availability_without_socket_and_unreadable_artifacts_reports_unknown' panicked at helper_client.rs:545:
  left: NotInstalled   right: Unknown
thread 'helper_client::tests::availability_without_socket_but_installed_reports_not_running' panicked at helper_client.rs:521:
  left: NotInstalled   right: NotRunning
```

**M3「产物判据取反」**（`classify_install_artifacts` 的 `Present ⇄ Absent`）：
sha256 `2e225735…`；`FAILED. 264 passed; 4 failed`：

```
thread 'helper_client::a10_verify_independent::t6_classify_install_artifacts_covers_all_combinations' panicked at helper_client.rs:805:
  left: Present   right: Absent
thread 'helper_client::a10_verify_independent::t7_real_filesystem_is_three_way' panicked at helper_client.rs:822:
  left: Absent    right: Present
thread 'helper_client::tests::classify_install_artifacts_is_three_way' panicked at helper_client.rs:552:
  left: Absent    right: Present
thread 'helper_client::tests::probe_install_artifacts_at_reads_the_filesystem_three_ways' panicked at helper_client.rs:572:
  assertion `left == right` failed: 有一个在就算装过     left: Absent   right: Present
```

**还原证据**（每条突变后）：

```
还原：sha256(path)=75941a9cd8c14eab… sha256(base)=75941a9cd8c14eab… ✓ 相同
收尾复跑：test result: ok. 268 passed; 0 failed; 5 ignored   EXIT=0
```

## 1.3 【量到的 + 推断】突变覆盖分析（这条比「红了」更有用）

1. **M2 会漏过 T2**：在「无条件 NotInstalled」下，T2（全新机器）**仍然绿** ——
   因为那一格本来就该是 `NotInstalled`。⇒ **只有 T1（装了没跑）能抓住被推翻的原处方**，
   这正是卡面把 T1 定为核心反例的理由；我据此确认「只测全新机器」不足以防回归。
2. **M3 只能由「更下层」的用例抓住**：注入缝在 `probe_install_artifacts()` **内部**短路，
   **不经过** `classify_install_artifacts` ⇒ 我的 T1/T2、实现者的 availability 三条在 M3 下**全绿**，
   红的是纯函数/真磁盘那两条（我的 T6/T7 + 他的 `classify_…`/`probe_…_at`）。
   ⇒ **两层都要有**：缝测「分支逻辑」，纯函数/真磁盘测「归属判定」。
3. **缝没有串台**（【推断】）：每次突变只红**直接相关**的那几条，**没有**成片红 ⇒
   `thread_local!` + `Drop` 还原在并行测试线程下没有把注入值泄漏给别的用例
   （若泄漏，`Present` 会落到每个调 `availability(false)` 的用例上）。

## 1.4 【量到的】诊断文本：改前 / 改后

```text
# 改前（2f23d22^ 的 commands/diagnostics.rs:203-205）
"helper: 已安装={} 可连接={} 版本={:?} 隧道活跃={}\n"
  ← 那个 {} 喂的是 snap.helper.socket_present ⇒ 把「socket 在不在」说成「已安装」

# 改后（2f23d22 的 helper_diagnostics_line）
"helper: 状态={} socket存在={} 安装产物={} 可连接={} 版本={:?} 隧道活跃={}\n"
  ← 状态=三态 slug；socket存在= 与 安装产物= **两个独立磁盘事实**（后者来自 probe_install_artifacts()）
```

【量到的】实测样例（注入 `Present` + `socket_present=false` + `state=NotRunning`）：

```
helper: 状态=not_running socket存在=false 安装产物=存在 可连接=false 版本=None 隧道活跃=false
```

全仓复核（【读码】）：`apps/desktop/src` 里已无「把 socket 存在性写成已安装」的措辞；
`grep -rn "已安装=" apps/desktop/src` 在诊断行里已不含该形态。

## 1.5 【量到的】三态契约（Rust × UI）

| Rust `HelperState`（serde `snake_case`） | UI 类型联合 `types.ts:322-328` | `HELPER_STATE_LABEL`（`Settings.tsx:187-194`） | 交互 |
|---|---|---|---|
| `Ready` | `"ready"` | 已就绪 | — |
| `NotInstalled` | `"not_installed"` | 未安装 | 「**安装** helper」（`:989` 只在 `not_installed` 时是这个文案） |
| `NotRunning` | `"not_running"` | 已安装但进程未运行 | 「**重启 helper**」按钮（`:991` 挂在 `not_running` 上） |
| `NotPermitted` | `"not_permitted"` | 权限不足 | — |
| `NeedsApproval` | `"needs_approval"` | 等待系统批准 | — |
| `Unknown` | `"unknown"` | 状态未知 | — |

⇒ **UI 不需要改**（与卡面一致）：三态里的 `not_running` 既有标签也有对应按钮；
`state` 由后端给出，UI 不自行推断。**没有发现 UI 侧遗漏。**

## 2. 【读码】得到的事实（不是我从运行里量的）

* `availability()` 的 `!socket_present` 分支已是三分支：`Present ⇒ NotRunning + NOT_RUNNING_HINT`、
  `Absent ⇒ NotInstalled + humanize(NotInstalled)`、`Unreadable ⇒ Unknown + 原始原因`；
  `socket_present == true` 时仍走 `ensure_connected()` ⇒ `classify(&msg)`。
* `classify()` **已去掉 `socket_present` 参数**，`ENOENT` 无条件 `NotRunning`（我预审指出的
  「早返回之后不可达的那一支」确实整体消失了，不是加注释）。
* 缝是 `#[cfg(test)] thread_local! TEST_INSTALL_ARTIFACTS` + `with_install_artifacts(fake, f)`
  （`Drop` 还原，含 panic）；另有**不靠缝**的 `probe_install_artifacts_at(plist, binary)` 真读文件系统。
* `humanize(NotRunning)` 与 `NOT_RUNNING_HINT` 两处文案我都逐字看过：**都不再**出现
  「socket 文件存在」，都写明「socket 由守护进程启动时创建、退出时删除」并指向「重启 helper」。
* 启动日志抽成纯函数 `helper_startup_log(&HelperAvailability)`（可测），与 UI 同判据。

## 3. 【推断】未在本环境验证（我这边的诚实边界）

* **真机两条路径未验证**：本机**装过且正在跑** helper —— 我读到的磁盘事实是
  `/Library/LaunchDaemons/com.xraytun.helper.plist`（777 B，root:wheel）、
  `/Library/PrivilegedHelperTools/com.xraytun.helper`（4,180,672 B，root:wheel）、
  `/var/run/com.xraytun.helper.sock`（存在，16:45）⇒ 本机永远走「socket 存在」那一支，
  **新分支一次都没被真实环境走到**。要验它需要：一台从未装过的机器（`Absent`），
  以及手工停掉守护进程（`Present`）—— 我**没有**做（对生产机做安装/卸载是卡面红线）。
* **`Unreadable` 的真实触发**：单测走的是 `ENOTDIR`（`/dev/null/child`）与注入的字符串；
  「plist 被改成 root-only 之类导致 EACCES」这条真实场景**没有复现**。
* **`metadata` 的 TOCTOU**：`probe_install_artifacts()` 与实际使用之间，产物可能被并发卸载；
  我**没有**验证这种竞态（【推断】：最坏是短暂显示 `NotRunning`，下次刷新即修正）。
* **注入缝只在 `cfg(test)`**：【读码】成立，但我**没有**去反汇编/审计发布产物确认它真的不在
  release 二进制里（项目已有「测试缝不进生产」的先例与惯例）。

## 4. 对实现者诚实清单的复核（交叉确认）

| 他的条目 | 我的判断 |
|---|---|
| 真机「全新机器 / 装了但没跑」两条路径未验证 | **同意，且我补了「为什么」**：本机三件产物全在（§3），新分支在本机走不到 |
| `Unreadable` 的真实场景未在真机复现，单测用 ENOTDIR 走同一代码路径 | **同意**（我的 T7 也走的是 ENOTDIR，不构成对他的加强） |
| 反向敏感性 6 条、每次还原 + sha256 | **同意其存在**：我独立重做了 3 条（M1–M3，构造与他不同），同样全红、同样按 sha256 还原 |
| 基线数字「779 passed / 0 failed / 9 ignored（workspace）」 | 我没有重跑 workspace（Lead 的门禁已在 `2f23d22` 上跑过 `check.sh`）；我只在 **lib 层**独立得到 **268/0/5**（含我的 9 条） |

**我认为他说轻了的一条（我的补充）**：`classify()` **仍然是「在错误文本里找 errno 片段」的匹配**
（`"os error 2"` / `"Connection refused"` / `"Permission denied"` / `"后台允许"`）。
本卡把**错误的那条推断**（socket ⇒ 装没装）删掉了，但这一层的**脆弱性没有变**：
它依赖 `xt_proto::transport` 把 `io::Error` 的 Display 拼进消息这一实现细节。按本项目的证据分层，
这是 **L3（文本匹配）**边界，建议后续卡在 `HelperError` 上加结构化 `errno`，而不是继续匹配文本。
（他报告里没有明写这一条 ⇒ 我在这里替它补上，**不是**说他隐瞒：改动本身没有引入新的文本匹配。）

## 5. 【量到的】复现命令（全部可重跑）

```bash
# 1) 建验证 worktree（默认已锚主工作区，task-187）
./scripts/wt.sh new t186v 2f23d22
# 2) 把独立用例追加进副本（只在 worktree；`a10_verify_independent.rs` 见 .prep-v0838/）
cat /path/to/a10_verify_independent.rs >> .wt/t186v/apps/desktop/src/helper_client.rs
# 3) 基线
./scripts/wt.sh run t186v -- cargo test -p xraytun-desktop --lib        # 268 passed; 0 failed; 5 ignored
# 4) 三条突变（脚本会打突变 → 跑 → 打印失败原文 → 按 sha256 还原）
bash run-mutations.sh                                                   # m1 266/2、m2 264/4、m3 264/4，全部 EXPLICIT FAILED
# 5) 收工
./scripts/wt.sh rm t186v                                                # 删 worktree + 它的 target dir（不 prune 别人的）
```

本验证的辅助文件（**不在仓库里**，放 `.prep-v0838/`）：
`a10_verify_independent.rs`（我的 T1–T9）、`a10_mutate.py`（M1–M3 + 还原）、`run-mutations.sh`、
`t186v-helper_client.baseline.rs`（基线快照，sha256 `75941a9cd8c14eab…`）。
**收工已删 worktree `t186v` 与 `.cargo-target.wt/t186v`；别人的 worktree 条目一个没动。**
