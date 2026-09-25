# 0.8.39 独立测试报告（task-9 / tester）

**一句话结论**：CI 门禁 `scripts/check.sh` 最终跑出 **exit 0 全绿**，5 个真核用例 **5/5 通过**（`--nocapture` 复核确认**没有走 SKIP 分支**），`vitest` **42 文件 / 447 通过**，`package-macos.sh` **exit 0** 且包内校验全绿。

**但同时**：0.8.38 的启动 `SIGABRT` **复现成功** —— 用新构建（含 panic hook）走 `open -a`（即用户双击那一条路）**3/3 崩溃**，hook **3/3 在真机 panic.log 里留下了文件:行号**：`apps/desktop/src/lib.rs:178:17`，消息 `there is no reactor running, must be called from the context of a Tokio 1.x runtime`。根因是 Tauri `setup` 回调里调了 `tokio::spawn`（该写法在 **0.8.38 的 tip 里也存在**，`lib.rs:87`）。

> ⚠️ **最重要的一条**：门禁绿（run3，HEAD `46e52cf`）的时候，这个启动 panic **还在源码里**（`lib.rs:178` 仍是 `tokio::spawn`）。门禁里**没有任何一步会启动 App**，所以它抓不到这类“一启动就 abort”。→ 详见 [§3.7](#37-门禁绿--app-起不来门禁盲区)。

---

## 0. 结论 + 证据速查

| # | 结论 | 证据 |
|---|------|------|
| C1 | `./scripts/check.sh` 最终 **exit 0（全绿）** | `/tmp/tester-check3.log:4749` `✓ 与 CI 相同的全部检查通过`；`:4752` `CHECK_EXIT=0` |
| C2 | 前两次 check.sh 的红**都不是产品缺陷**：R1 = 磁盘写满（ENOSPC），R2 = 队友未提交的 `crates/xt-helper/src/peer.rs` 编译错 | `/tmp/tester-check.log:4673-4697`；`/tmp/tester-check2.log:3568,3588` |
| C3 | 5 个真核用例全绿，且**确实跑了核心**（无 SKIP） | 各 `/tmp/tester-<name>.log`；`--nocapture` 版 `/tmp/tester-<name>-nocap.log` |
| C4 | `vitest run` 全绿：42 文件 / 447 passed / 1 todo | `/tmp/tester-vitest.log` `Test Files 42 passed` |
| C5 | App 打包 `package-macos.sh` **exit 0**（.app/dmg/zip + 包内 items/codesign --strict 全绿） | `/tmp/tester-package.log:367-379,422` |
| C6 | **启动 SIGABRT 复现成功**：`open -a` 3/3 崩，3/3 写 panic.log | `~/Library/…/logs/panic.log`；`~/Library/Logs/DiagnosticReports/xraytun-desktop-2026-09-25-155028*.ips` |
| C7 | hook 写出的位置可复算：`apps/desktop/src/lib.rs:178:17` = `tokio::spawn`（Tauri setup 无 tokio reactor context） | panic.log 原文 + §3.5 的 `git show` 三处对照 |
| C8 | 该写法**在 0.8.38 里就有**（`git show 3376830:apps/desktop/src/lib.rs` → `:87`） | §3.5 |
| C9 | 全程**没有动系统网络**：默认路由始终 `192.168.0.1 en0`，DNS 始终 `114.114.114.114`，无 utun 默认路由、无残留进程 | §3.6 |
| C10 | 工作区**当前的未提交改动**已把它改成 `tauri::async_runtime::spawn`（`lib.rs:184`）——但**我没有构建/启动验证这个修复** | `apps/desktop/src/lib.rs:178-184`（工作区，未提交） |

---

## 1. 环境、修订与边界（先说清楚，免得把“时点结论”当“revision 结论”）

- 仓库：`/Users/xbtg-/deepseek-harness/xray-tun`，分支 `main`。
- 隔离编译目录（按 Lead 要求，避免与别人抢 target）：
  `CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo`，
  `CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target.wt/tester`，
  `npm_config_cache=/Users/xbtg-/deepseek-harness/.npm-cache`。
- **测试期间仓库一直在被队友写**。HEAD 变化：
  `0e09214`（开始时，工作区干净）→ `71700ba` → `4f0433e` → `58e4151` → `46e52cf`（run3 结束时）。
  因此本报告**没有**“某个单一 revision 的全绿”；每条结论都标了时点。
- `check.sh` 本身也被改过（ops 的任务），我跑的三次对应不同 sha256：
  - R1: `fc647291…`（=`0e09214` 版本）
  - R2: `26028fa0…`（=`71700ba` 版本）
  - R3: `cf63ec06…`（工作区版本；R3 结束后它又被改成了 `63e664b2…`）
- **沙箱边界**：我的 shell 不能写仓库外。实测：
  `touch ~/Library/Application\ Support/com.xraytun.desktop/.tester-write-probe` →
  `Operation not permitted`。所以“用真实用户数据目录启动 App”这条路我走不了（见 §4），
  崩溃复现改用 `XRAYTUN_DATA_DIR` 覆盖（`crates/xt-core/src/store.rs:41-51` 支持的正当入口）。
  注意：`open -a` 启动的进程由 launchd 拉起，**不受**我这层沙箱限制（这也是它写到了真实数据目录的原因）。
- 磁盘事件：R1 跑到最后一步时磁盘 100%（剩 158 MiB），release 构建 ENOSPC。我删掉了**本会话之前遗留**的两个 target 缓存
  `.cargo-target.wt/intent-p4`(9.0G) 与 `.cargo-target.wt/leadgate182`(6.5G)（最后写入 01:10 / 00:12，本会话 14:47 才开始；已确认无进程占用），
  并删掉我自己 ENOSPC 的半成品 `…/tester/release`。**没动** `.cargo-target`（有 `cargo run` 在用）和 `.cargo-target.wt/integration`（14:36 仍在写）。

---

## 2. 跑过的

### 2.1 `./scripts/check.sh`（三次，全量 CI 门禁）

命令（每次相同）：

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo \
CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target.wt/tester \
npm_config_cache=/Users/xbtg-/deepseek-harness/.npm-cache \
./scripts/check.sh
```

| 次 | 起始 HEAD | 结果 | 归因（不是产品缺陷） |
|----|-----------|------|----------------------|
| R1 | `0e09214`（干净） | **exit 101** | 前 12 步全过（含 clippy、`cargo test --workspace`、前端），**最后 release 构建** ENOSPC |
| R2 | `71700ba` | **exit 101**（33 秒） | `clippy` 红：队友 backend-2 **未提交**的 `crates/xt-helper/src/peer.rs` |
| R3 | `58e4151` | **✅ exit 0** | `✓ 与 CI 相同的全部检查通过` |

R1 的失败原文（环境，不是代码）：

```
error: failed to build archive at `…/tester/release/deps/libobjc2_app_kit-…rlib`: No space left on device (os error 28)
rustc-LLVM ERROR: IO failure on output stream: No space left on device
```

R2 的失败原文（队友在途代码，文件在跑的时候是 ` M` 未提交状态）：

```
error[E0277]: `peer::AuditToken` doesn't implement `std::fmt::Debug`
    --> crates/xt-helper/src/peer.rs:548:35   (audit_token(-1).expect_err(...))
error: function `SecCodeCopySelf` is never used
    --> crates/xt-helper/src/peer.rs:98:8
```

R3 的单元测试合计：**961 passed / 0 failed / 10 ignored**（24 个 `test result:` 行全 `ok`），release 构建也编过。
R3 还包含：`vitest` 42 文件通过、`tsc -p apps/ui/tsconfig.json` 干净、CSS token、`helper_tristate/incident-bundle/triage` 自测、
`verify-wt-dir-root.sh`、站点版本/GEO/JSON-LD 一致性 —— 全部通过（见 `/tmp/tester-check3.log`）。

### 2.2 五个真核用例（`XT_CORE` 指向仓库里那份 xray）

命令（逐条，`XT_CORE=/Users/xbtg-/deepseek-harness/xray-tun/apps/desktop/binaries/xray`）：

```bash
cargo test -p xt-core   --test mitm_steering_live
cargo test -p xt-core   --test mitm_tls_live
cargo test -p xt-intent --test real_core_dataplane
cargo test -p xt-intent --test real_core_udp
cargo test -p xt-intent --test real_core_tun
```

| 用例 | exit | 结果行 | 关键输出（`--nocapture`） |
|------|------|--------|----------------------------|
| `mitm_steering_live` | 0 | `1 passed; 0 failed` | `核心已就绪：socks=57615 mitm=57620 upstream=57618` |
| `mitm_tls_live` | 0 | `1 passed; 0 failed` | `① TLS 终结 + 204 + 源站 0 次 ✓ ② 经 mitm-upstream 回连（无自环）✓ ③ 非 opt-in 未经过 MITM ✓` |
| `real_core_dataplane` | 0 | `2 passed; 0 failed` | `真实核心已就绪：socks=127.0.0.1:57669` |
| `real_core_udp` | 0 | `1 passed; 0 failed` | `① 对照组 UDP 往返成功 ✓ ② 实验组收到 blackhole 403 ✓` |
| `real_core_tun` | 0 | `1 passed; 0 failed` | `① 纯 TUN ✓ ② TUN+拦截规则 ✓ ③ TUN+MITM 引导 steer 覆盖 tun ✓` |

**为什么还要补一次 `-- --nocapture`**：这些用例找不到核心时会 `eprintln!("SKIP…")` 然后**正常返回**，
`test result` 同样会写 `1 passed` —— 只看退出码会把“没跑”当“通过”。
5 个用例的 `--nocapture` 日志里 **`SKIP` 出现 0 次**，且都打印了“核心已就绪”/“核心自检通过”，确认是真跑了 `apps/desktop/binaries/xray`。
时点：HEAD `4f0433e`（测试循环结束时 `git rev-parse HEAD`）。

### 2.3 `apps/ui` 的 vitest（独立于 check.sh 再跑一次）

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun/apps/ui && ./node_modules/.bin/vitest run
```

```
 Test Files  42 passed (42)
      Tests  447 passed | 1 todo (448)
   Duration  14.27s
VITEST_EXIT=0
```

（R1 时是 41 文件 / 418 passed，因为期间 frontend 队友加了用例 —— 说明树在动。）

### 2.4 `./scripts/package-macos.sh`（打 App，供崩溃复现用）

```bash
CARGO_HOME=… CARGO_TARGET_DIR=…/tester npm_config_cache=… ./scripts/package-macos.sh
```

**`PKG_EXIT=0`**，构建时修订 `HEAD=4f0433e` **+ 9 个未提交改动**（含 `apps/desktop/src/lib.rs`）。
包内校验全绿：

```
  ✓ 核心(33M) ✓ geoip.dat(16M) ✓ geosite.dat(10M) ✓ helper(2.1M) ✓ Info.plist ✓ 主程序(10M)
  ✓ 已 ad-hoc 签名   ✓ 已生成 dmg   ✓ 镜像根目录里有 XrayTun.app   ✓ 有 /Applications 快捷方式
  ✓ 镜像内 App 的 codesign --strict 校验通过   ✓ 已生成 zip
```

产物：`/Users/xbtg-/deepseek-harness/.cargo-target.wt/tester/release/bundle/macos/XrayTun.app`（`CFBundleShortVersionString=0.8.39`）。

---

## 3. 启动 SIGABRT：复现尝试（**复现成功**）

### 3.1 现场既有记录（不是我制造的证据，但可复算）

`~/Library/Logs/DiagnosticReports/` 里今天 13:26–14:31 有 **7 份** `xraytun-desktop-*.ips`：

```
xraytun-desktop-2026-09-25-132654.ips / 132716.ips / 132716.000.ips / 132727.ips
xraytun-desktop-2026-09-25-143007.ips / 143147.ips / 143147.000.ips
```

每份都是：`app_version 0.8.38`、`exception.type=EXC_CRASH`、`signal=SIGABRT`、`termination.indicator="Abort trap: 6"`、
`asi={"libsystem_c.dylib":["abort() called"]}`；faulting thread 顶部为 `__pthread_kill → pthread_kill → abort`，**符号被 strip 掉，没有位置**。
（这就是 0.8.38 没有 hook 时的“没有任何证据”。）

### 3.2 `open -a`（≈ 用户双击）3 次：**3/3 崩溃**

```bash
open -n --env XRAYTUN_DATA_DIR=/Users/xbtg-/deepseek-harness/.tester-appdata \
     --env RUST_BACKTRACE=1 -a /…/tester/release/bundle/macos/XrayTun.app
```

结果：每次进程都在 ~1 秒内消失，`open_exit=0`；随后在 `DiagnosticReports` 里出现 **v0.8.39** 的
`xraytun-desktop-2026-09-25-155028.ips`、`…155028.000.ips`、`…155028.0002.ips`（三份，`SIGABRT / Abort trap: 6`，
`parentProc=launchd` —— 即这三次 `open`）。
**panic hook 每次都写了日志**，位置与消息见 §3.4。

### 3.3 直接 `exec` 3 次：**没崩，但“静默挂起”**（诚实记录）

```bash
RUST_BACKTRACE=1 XRAYTUN_DATA_DIR=…/.tester-appdata XRAYTUN_LOG=debug \
  /…/XrayTun.app/Contents/MacOS/xraytun-desktop
```

| 尝试 | 结果 |
|------|------|
| 脚本内的 direct-1/2/3 | 存活满 30 s 被我 `SIGTERM`（exit 143），**stdout/stderr 0 字节**，无 panic.log、无 .ips |
| 追加 direct-4/5/6 | 存活满 10 s 被我 `SIGKILL`（exit 137），**依旧 0 字节输出**，无 panic.log、无 .ips |
| 追加探针 direct-probe（同一命令、同一 env） | **崩了**：`Abort trap: 6`，`.ips` `…-155035.ips`（v0.8.39, SIGABRT, `parentProc=bash`），panic.log 437 B |

即：直接 exec 在本沙箱下一共 7 次尝试，只有 **1 次**真正走到启动并崩，其余 **6 次**在**打出任何一行日志之前**就停住、
必须被外部杀掉。我没能查清这个挂起的机制（见 §5）。所以：**“直接 exec 不复现”不是证据；`open -a` 才是忠实的路径，它 3/3 复现。**

### 3.4 panic hook 的第一次实战：真机文件里有什么

`open` 三次运行之后，**真实用户数据目录**：

```
~/Library/Application Support/com.xraytun.desktop/logs/panic.log   (1311 B, 3 条追加)
```

每条内容（三条除时间戳外完全相同）：

```
[unix=1790322588] PANIC apps/desktop/src/lib.rs:178:17
there is no reactor running, must be called from the context of a Tokio 1.x runtime
backtrace:
   0: __mh_execute_header
   …（全部 __mh_execute_header）
```

对照 direct-probe（走 `XRAYTUN_DATA_DIR` 覆盖）：

```
/…/.tester-appdata/logs/panic.log  (437 B)
[unix=1790322635] PANIC apps/desktop/src/lib.rs:178:17
there is no reactor running, must be called from the context of a Tokio 1.x runtime
此进程的 stderr 里同时有 "thread 'main' (695724) panicked at apps/desktop/src/lib.rs:178:17: …" +
"stack backtrace:"（RUST_BACKTRACE=1）
```

**结论**：hook 按设计工作 —— 文件:行号 + 消息一定落盘；backtrace 因为 `strip = true` 只有地址（文件里退化成 `__mh_execute_header`），
这跟 `e8aa9b3` 提交里写的“诚实边界”一致。
**注意**：`open --env XRAYTUN_DATA_DIR=…` **没有传进去**（证据：日志写在**真实**数据目录，而 `.tester-appdata/logs/` 在 open 阶段始终为空）。
换句话说，`open` 路径下 App 用的是用户的真实 `settings.json`（`mode=tun`、`was_connected=true`）—— 危险正是从这里来的（见 §3.6）。

### 3.5 根因链（可复算）

1. panic 位置 `apps/desktop/src/lib.rs:178:17`，结合当前工作区源码：

```bash
sed -n '176,179p' apps/desktop/src/lib.rs
# 176:  // 判定节拍：与看门狗一样 10 秒一跳。没有引擎时它什么都不做。
# 177:  let tick_handle = handle.clone();
# 178:  tokio::spawn(async move {          ← 178:17 就是这里的 `tokio::spawn`
```

2. 消息 `there is no reactor running, must be called from the context of a Tokio 1.x runtime`
   是 `tokio::spawn` 在**没有进入 tokio runtime context** 的线程上被调用时的标准 panic —— Tauri 的 `setup` 回调不在 tokio runtime context 里。
3. 这段代码**不是新引入的**，0.8.38 里就有：

```bash
git show 3376830:apps/desktop/src/lib.rs | grep -n -B2 -A1 "tokio::spawn"   # → :87  tokio::spawn(async move {
git show 0e09214:apps/desktop/src/lib.rs | grep -n    "tokio::spawn"         # → :140
git show 4f0433e:apps/desktop/src/lib.rs | grep -n    "tokio::spawn"         # → :140（打包那次用的版本）
git show 46e52cf:apps/desktop/src/lib.rs | grep -n    "tokio::spawn"         # → :178（run3 全绿时的 HEAD，仍在）
```

4. `if let Some(settings) = settings` 这里的 `Option` 只是 `state.with(...)` 的锁返回，**恒为 `Some`**
   （`apps/desktop/src/state.rs:186,380`：`settings: AppSettings`，不是 `Option<AppSettings>`），
   也就是说**每次启动都会走到 `tokio::spawn` 那一行** → 与 `open -a` 3/3、direct-probe 1/1 的观测一致。
5. 3.1 的 0.8.38 `SIGABRT` 与本次 panic **只差“有没有 hook”**：0.8.38 里同一行代码存在，只是崩溃时没有任何位置信息。

### 3.6 网络安全性观察（用户明确要求：不许把网弄断）

每次启动前/中/后都采了默认路由与 DNS：

- 基线：`default 192.168.0.1 en0`；`nameserver[0] 114.114.114.114`。
- 全部 10 次启动尝试：`route_changed=0`（脚本每 0.5 s 采样一次默认路由）。
- 结束后：`default 192.168.0.1 en0`、DNS `114.114.114.114`、`netstat -rn | grep -c utun` = **0**、无 `xraytun-desktop` / `binaries/xray` 残留进程。
- 之所以没被接管，除了全崩在 setup 之前，我还做了三重保护：独立数据目录 + `mode=direct` + `was_connected=false`。
  （`apps/desktop/src/commands/core.rs:591-602` 的 `should_auto_reconnect` 要求 `was_connected && auto_reconnect && mode != Direct`。）
- **副作用（诚实披露）**：`open` 路径下 hook 往**真实**数据目录追加了 3 条 panic.log（1311 B）。这是本报告要求的证据，我没有删除。

### 3.7 门禁绿 ≠ App 起得来（门禁盲区）

- run3 全绿时的 HEAD `46e52cf`：`git show 46e52cf:apps/desktop/src/lib.rs | grep -n "tokio::spawn"` → **`:178`**。
- 也就是说：**check.sh 绿 + App 一启动就 abort** 可以同时成立。`check.sh` 的每一步（clippy / `cargo test --workspace` / release 构建）
  都只**编译**和跑**库测试**，没有任何一步会启动打包出来的 App。
- 工作区**现在**（未提交）已经把它改成正确写法：

```bash
sed -n '178,184p' apps/desktop/src/lib.rs
# 178: // ⚠️ 必须走 `tauri::async_runtime::spawn`：Tauri 的 `setup` 回调**不在
# 179: // tokio runtime context 里**，`tokio::spawn` 会直接 panic
# 184: tauri::async_runtime::spawn(async move {
```

  注释里写的 panic 原因与我 3.5 复现的完全一致 —— 但**这是别人的在途改动，我没有构建/启动验证它**（见 §5）。

---

## 4. 没跑的（明确写清楚，不许当成“通过”）

1. **所有 `#[ignore]` 用例都没跑**（run3 里合计 `10 ignored`）。已知清单：
   `crates/xt-core/tests/rule_tag_uniqueness.rs`（`:324` 需要真实核心）、
   `crates/xt-core/src/xray/config.rs`、`crates/xt-core/src/xray/access_log.rs`（需要真实核心日志）、
   `crates/xt-tun/src/macos/trust.rs`（**需要 root 与真实钥匙串**）、
   `crates/xt-intent/src/jev.rs`（需要外网）、
   `apps/desktop/src/commands/{core,diagnostics,globe,incident}.rs` 的手动证据工具（需要真实数据/会真的上传）。
2. **安装版 `/Applications/XrayTun.app`（0.8.38）的真实双击没跑**。原因：真实 `settings.json` 是
   `mode=tun` + `was_connected=true` + `auto_reconnect=true`，一启动就会自动重连并接管默认路由；用户明确要求不许把网弄断。
3. **“在真实用户数据目录下直接启动”没跑**。我的 shell 沙箱对 `~/Library/Application Support/com.xraytun.desktop/` 的写操作是
   `Operation not permitted`，跑起来也不是真实条件（App 会写不进去）。所以崩溃复现走的是 `XRAYTUN_DATA_DIR` 覆盖 + `open`（后者绕过了沙箱）。
4. **修复（`tauri::async_runtime::spawn`）没有先构建、后启动地验证过**。我只做了源码层面的读取（§3.7）。
5. **没有在隔离 CI runner（GitHub Actions）上跑**；run3 是本机、且树在动。
6. **Windows / Linux 不适用**（该仓库 CI 与代码都明确只支持 macOS）。
7. **没有跑性能/长时间稳定性测试**；没有对 `real_core_tun` 做真实的系统 TUN 数据面流量测试
   （该用例是核心 `-test` 级自检 + 配置校验，不是真发包）。
8. **没有测“非 default 目标架构”**：本次打包是本机架构（Homebrew rust，非 universal），未跑 `XRAYTUN_TARGET=universal-apple-darwin`。

---

## 5. 我没能验证的

1. **“0.8.38 的 SIGABRT 与这次 `tokio::spawn` panic 是同一根因”只有间接证据**：同一行代码在 0.8.38 存在（`:87`）+ 都是启动即 `SIGABRT` + 新构建同点复现。
   0.8.38 崩溃当时**没有 hook**，`.ips` 也没有符号，所以严格说：**不能证明**它就是这一条 panic。
2. **直接 exec 6/7 静默挂起的原因未查明**：进程在打出第一行日志之前就停住，无输出、无 panic、无 crash report；
   我怀疑与 DSH 文件沙箱/窗口服务有关，但**没有证据**，只能记成“现象 + 未查明”。
3. **修复的有效性未验证**（未用修好的工作区重新 `package-macos.sh` + `open -a`）。
4. **没有单一 revision 的绿**：三次 check.sh 期间 HEAD 从 `0e09214` 走到 `46e52cf`，`check.sh` 自身也被改了两次；
   run3 的绿对应 `HEAD_BEFORE=58e4151` / `HEAD_AFTER=46e52cf` + 当时的未提交改动。
5. **`open --env` 环境变量没有传进去**这一点我只观测到“结果写在真实数据目录”，没有去查 `open(1)` 的实现/版本文档；
   因此“为什么没传进去”我没验证。
6. **hook 在 `open` 场景只验证了“写进去了”**，没有验证多进程并发追加是否会交错（本次 3 条恰好完整）。
7. **`cargo test --workspace` 里各 target 的 961 passed 我只做了汇总**，没有逐条人工核对每个 test 名字对应的行为；测试是否有效依赖上游用例自身的断言质量。

---

## 附：本次运行留下的可复算材料

| 路径 | 内容 |
|------|------|
| `/tmp/tester-check.log` / `check2` / `check3.log` | 三次 check.sh 全量日志（含 `CHECK_EXIT`） |
| `/tmp/tester-<name>.log`、`-nocap.log` | 5 个真核用例的普通/`--nocapture` 输出 |
| `/tmp/tester-vitest.log` | vitest 独立运行 |
| `/tmp/tester-package.log` | 打包日志（含 `PKG_EXIT=0` 与逐项 ✓） |
| `/tmp/tester-crash-repro.log` + `/tmp/tester-crash/` | 6 次启动尝试的逐次结果与 stdout/stderr |
| `/tmp/tester-direct-probe.out` | 直接 exec 崩那次完整输出（含 panic 原文） |
| `~/Library/Application Support/com.xraytun.desktop/logs/panic.log` | **新 hook 的实战产物**（3 条，1311 B） |
| `~/Library/Logs/DiagnosticReports/xraytun-desktop-2026-09-25-15*.ips` | 新构建 4 份 SIGABRT（3 份 `parentProc=launchd` + 1 份 `parentProc=bash`） |

报告人：`tester`（独立测试，不写产品代码）。本报告只新建 `docs/verification/TEST-REPORT-0.8.39.md` 一个文件；未 `git add -A`、未 push。
