# task-14 独立复验：启动 SIGABRT 修复 + 门禁守卫（tester，发现者不自证）

**复验对象**：`c5c7942`（backend-1：`setup` 裸 `tokio::spawn` → `tauri::async_runtime::spawn` + 门禁守卫 + `smoke-app-startup.sh`），
证据文档 `docs/verification/TASK-14-STARTUP-PANIC.md`。
**复验时的 HEAD**：静态扫描跑在 `079d199` 与 `df791e4`（两次都跑；`c5c7942` 均是祖先）；运行时烟测的构建来自 `079d199`。

## 结论

| # | 判据 | 判定 | 证据 |
|---|------|------|------|
| 1 | 生产代码里没有裸 `tokio::spawn` | ✅ **独立复验通过** | 我**自己写的**扫描器：`apps/desktop/src/**/*.rs` 12,615 行生产代码（去 `//` 注释、跳过 `#[cfg(test)]` 模块），命中 **0**；`tauri::async_runtime::spawn` 34 处 |
| 2 | 守卫的两条测试真的跑且真的绿 | ✅ **通过** | 我独立跑 `cargo test -p xraytun-desktop --lib naked_tokio_spawn` → `2 passed; 0 failed`，两条 `... ok`（`/tmp/t14-guard.log`） |
| 3 | 「过滤器匹配 0 条 = 假绿」被 `check.sh` 挡住 | ✅ **通过** | 我用假过滤器跑出 `running 0 tests` / exit 0（`/tmp/t14-zero.log`），再把 `check.sh` 的两条 `grep '... ok'` 断言套上去 → 零匹配那条**断言失败**，真跑那条通过 |
| 4 | 旧代码**必须变红**（运行时负例） | ✅ **3/3 红** | 我在 task-9 构建的**修复前** App（sha `24a63cd4…`）：`--mode direct` **3/3 `Abort trap: 6`（exit=134）**，隔离目录 `logs/panic.log` 3/3 写 `apps/desktop/src/lib.rs:178:17` + `there is no reactor running…`；`smoke-app-startup.sh` **exit=1** |
| 5 | 修复后**不再崩**（运行时正例） | ✅ **3/3 绿** | 我自己在 `079d199` 重打的 App（sha `efeeaab7…`）：`--mode direct` **3/3 存活 10s、有启动证据、NEW_IPS=0、真实 panic.log 未增长、网络未变**，脚本 **exit=0**；backend-1 的产物同样 **3/3 绿** |
| 6 | `--mode open` 在不安全 settings 下必须拒绝（安全门） | ✅ **通过** | 真实 settings 仍是 `mode=tun / was_connected=True / auto_reconnect=True` → 脚本 **exit=75**、**没有拉起任何进程**、路由/panic.log/IPS 全不变 |
| 7 | **忠实 `open -a` 3 次**（`NEW_IPS=0` + 真实 panic.log 字节不增长 + 路由同基线） | ❌ **未验证 —— 沙箱写权限** | 见下节。**我没有用别的路径凑绿** |

**一句话**：静态守卫与「旧红新绿」都独立复现了；**唯一没拿到的是忠实双击路径 `open` 的 3 次**，因为它必须先改真实 `settings.json`，而我在沙箱里改不了 —— 按 Lead 指示如实记为未验证。

---

## 为什么忠实 `open` 没跑（不是偷懒，是三条路都被硬墙挡住）

复验前实测：

```bash
# ① 真实 settings 确实不安全（App 会自动重连接管默认路由）
python3 -c "import json,os;s=json.load(open(os.path.expanduser('~/Library/Application Support/com.xraytun.desktop/settings.json')));print(s['mode'],s['was_connected'],s['auto_reconnect'])"
# → tun True True

# ② 直接改真实 settings.json：被沙箱拒
python3 -c "open(os.path.expanduser('~/Library/Application Support/com.xraytun.desktop/settings.json'),'a').close()"
# → PermissionError: [Errno 1] Operation not permitted

# ③ 连父目录都写不了（所以也不能"临时挪走 settings.json"）
touch ~/Library/Application\ Support/.tester-probe
# → touch: Operation not permitted

# ④ 也不能用会话环境变量绕过（这样 open 就会读到隔离目录）
launchctl setenv XRAYTUN_TESTER_PROBE 1
# → Not privileged to set domain environment.
```

而 `open` 阶段**不受隔离保护**（`open --env XRAYTUN_DATA_DIR` 传不进去，这是我在 task-9 实测、写进 `UX-FINDINGS-REVERIFY.md` 附录 A 的）⇒ 用真实 settings 跑 `open`，修复后的 App 会真的启动并自动重连、接管默认路由。**按「绝不能把用户网弄断」，我不跑。**

**谁做什么才能拿到这一条**（任选其一，之后我可以立刻补跑）：
1. 有人在有写权限处执行：备份 → `settings.json` 的 `was_connected` 置 `false`（或 `mode` 置 `"direct"`）→ 跑 `open` 3 次 → 还原；
2. 或授权在「无人使用网络、可接受接管」的机器上跑；
3. 或把该文件/父目录的写权限放给本会话（我这边是 `workspace-write`，无法自行扩权）。

命令（第 1 条就绪后我原样执行）：

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
XRAYTUN_SMOKE_APP=/Users/xbtg-/deepseek-harness/.cargo-target.wt/tester/release/bundle/macos/XrayTun.app \
XRAYTUN_SMOKE_MODE=open ./scripts/check.sh
```

---

## 基线（不动它，也请别人别把它算到本次复验头上）

- 真实 `~/Library/Application Support/com.xraytun.desktop/logs/panic.log`：复验**前 = 复验后 = 3488 B / 6 条**
  - 1–3 条 `[unix=1790322588/591/594] … lib.rs:178:17` ← **我**在 task-9 的 3/3 复现（15:49）
  - 4–6 条 `[unix=1790325946/958/970] … lib.rs:140:17` ← **不是我**（16:45–16:46，别人跑的；我 task-9 之后到 16:58 前没写过真实目录）
- `~/Library/Logs/DiagnosticReports` 里 `xraytun-desktop-*.ips` 计数变化：
  `14`（16:58，我跑负例前）→ **负例那次脚本自己统计的 IPS 增量是 0** → `17`（18:08，我跑正例前）→ `17`（正例后，`NEW_IPS=0`）。
  新增的 3 份文件时间戳是 **17:17:02–17:17:03**（`…171702.ips` / `…171702.000.ips` / `…171703.ips`），与我 16:58 的负例**时间对不上**，
  可能是我那 3 次崩溃的延迟落盘，也可能是别人 17:17 跑出来的 —— **所以我不声称这 3 份是我的**；我的负例证据用**自包含**的隔离目录 panic.log 与 exit=134。
- 网络基线（全程未变）：默认路由 `192.168.0.1 en0`、DNS `114.114.114.114`、`utun=36`。

---

## 我跑了什么（全部可复算）

**A. 独立静态扫描**（不依赖 backend-1 的测试实现）：见本文件 §结论 第 1 行的内联 python；
在 `079d199` 与 `df791e4` 各跑一次，均 `scanned=12615 / naked_tokio_spawn=0`。

**B. 独立跑守卫测试 + 反假绿验证**：

```bash
CARGO_TARGET_DIR=…/tester cargo test -p xraytun-desktop --lib naked_tokio_spawn
# test tests::naked_tokio_spawn_guard_catches_the_0_8_38_pattern ... ok
# test tests::production_never_calls_naked_tokio_spawn ... ok
# test result: ok. 2 passed; 0 failed; 306 filtered out
CARGO_TARGET_DIR=…/tester cargo test -p xraytun-desktop --lib no_such_test_zzz   # → running 0 tests, exit 0
# 把 check.sh:333-339 的两条 `grep '... ok'` 断言套到上面两份日志：
#   guard 日志 → 断言通过（check.sh 继续）
#   零匹配日志 → 断言失败（check.sh 会 exit 1）
```

**C. `--mode open` 安全门**（脚本先检查、拒绝启动）：

```bash
./scripts/smoke-app-startup.sh --app …/backend1-release/…/XrayTun.app --mode open --runs 3
# ✗ 拒绝跑 open 模式：真实 settings 是 unsafe:mode=tun:was_connected=True
#   … 三个安全出路（任选其一）…
# exit=75；pgrep 无进程；route/panic/ips 全不变
```

**D. 运行时负例（旧构建必须红）**：把 task-9 的修复前 `.app` 先拷到 `/tmp/pre-fix-XrayTun.app`（防止被新构建覆盖），

```bash
./scripts/smoke-app-startup.sh --app /tmp/pre-fix-XrayTun.app --mode direct --runs 3 --alive 10
# run 1/3: ✗ 进程在 5s 内退出（exit=134）  ✗ 隔离目录里出现 panic
# run 2/3: ✗ exit=134    run 3/3: ✗ exit=134      → exit=1
# 隔离 panic.log（3/3）：PANIC apps/desktop/src/lib.rs:178:17 / there is no reactor running…
```

**E. 运行时正例（修复后必须绿）**：我自己在 `079d199` 用
`XRAYTUN_TEAM_ID=UNSET-REFUSE-PRIVILEGED-OPS ./scripts/package-macos.sh`（新门禁要求显式注入；**哨兵**是文档里的 fail-closed 选项，测试构建里 TUN/装 helper/信任锚不可用，正好不会碰系统）重打 App，`PKG14B_EXIT=0`，二进制 sha `efeeaab7…`：

```bash
./scripts/smoke-app-startup.sh --app …/tester/release/bundle/macos/XrayTun.app --mode direct --runs 3 --alive 10
# 3/3：进程存活 10s ⇒ 未崩溃；✓ 活着且有启动证据；NEW_IPS=0；真实 panic.log 未增长；网络未变
# exit=0；真实 panic.log 前后均 3488 B；route 未变
```

同一命令对 backend-1 的产物（`…/backend1-release/…`）也 **3/3 绿、exit=0**。
修复后那次运行 stdout 里能看到 `使用 XRAYTUN_DATA_DIR 覆盖数据目录`（这行来自 `setup` 里的 `Store::with_default_root()`）——说明 `setup` 已经跑过去了、没在旧 `tokio::spawn` 处 panic。

**F. 全程网络检查**：每次烟测由脚本自带前后对照；我另外每次记录 `netstat -rn | default`、DNS、`utun` 数、真实 panic.log 字节 —— **每次都不变**。

## 我没跑 / 没证据的

1. **忠实 `open -a`/`open -n` 3 次 —— 未验证**（沙箱写权限，见上节）。这是本卡唯一缺口。
2. **没有在可接管网络的机器/条件下验证**，所以「修复后双击路径不崩」目前只有 `direct` 模式的证据 + 静态守卫，**没有**忠实双击证据。
3. **没有验证哨兵构建下 App 的完整功能**（我刻意为安全用了 `UNSET-REFUSE-PRIVILEGED-OPS`，TUN/helper/信任锚按设计不可用）——本卡只验「启动不崩」。
4. **没有独立验证 backend-1 手写的那次「临时改回 `tokio::spawn` → 红」**（我只是读了他的记录）；我复验的是**测试内自带的负例**（`naked_tokio_spawn_guard_catches_the_0_8_38_pattern`，它在内存里把锚点换回旧写法并断言命中 1 处）与**运行时负例**（旧 App 3/3 红）。
5. **`check.sh` 整跑没做**（本卡只跑守卫那一步与烟测；门禁整跑结论在 `docs/verification/TEST-REPORT-0.8.39.md`）。
6. **`.ips` 归因不完整**：17:17 那 3 份新报告我无法证明是我的负例（时间对不上），故不声称（见基线节）。
7. 守卫的**扫描边界**：只覆盖 `apps/desktop/src/**`（不含 `crates/**`）。这是合理取舍（`crates/**` 的 `tokio::spawn` 都在 `async` 上下文），但**边界本身没有测试**；另外 `without_line_comment` 不处理 `/* … */` 块注释，理论上块注释里的 `tokio::spawn(` 会误报（假红，不是假绿）。

---

复验人：`tester`（不写产品代码）。本文件只新建这一个文件；未 `git add -A`、未 push。
