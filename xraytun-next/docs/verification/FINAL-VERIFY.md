# 终检记录（lead）· 本文件只写「跑过什么、结果是什么」

> 规则：**没跑过的不写在这里**；跑过的写命令、退出码、关键输出。
> 各角色自己的证据在 `docs/verification/B3-E2E-EVIDENCE.md`、`docs/ux/LIVE-EVIDENCE.md`、
> `docs/product/ACCEPTANCE.md`；本文件是**集成后由 lead 在冻结树上重跑**的那一遍。

## 0. 冻结的时间点与树

| 项 | 值 |
| --- | --- |
| 时间 | 2026-09-29 10:2x UTC |
| 所有 writer | 全部 inactive（无人再改文件）后才开始终检 |
| 规模 | 13 crates / Rust src 8030 行 / Rust 测试 1860 行；UI src 2838 行 / UI 测试 962 行 / CSS 1187 行；文档 2904 行 |
| 工具链 | rustup/cargo 1.98.1（本机装的，不再依赖 GCP Cloud Run）；真 xray `26.3.27 linux/amd64` |

## 1. 一条命令跑完全部验收

```bash
cd /Users/xbtg-/deepseek-harness/xraytun-next
CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-targets/lead bash scripts/verify.sh
```

结果（`/tmp/verify-final2.log`，`verify_exit=0`）：

```
PASS       guard（五条不变量的机器判据）
PASS       clippy（--workspace --all-targets -D warnings）
PASS       cargo test（全 workspace）
PASS       E2E：真 xray 进程 + 真字节（e2e_real_xray）
PASS       UI：tsc --noEmit
PASS       UI：vitest
PASS       UI：vite build
全部通过
```

明细：`cargo test --workspace` → **128 passed / 0 failed**（35 个 test binary，含集成测试）；
UI → **29 passed / 1 skipped**（跳过的 1 条是 `XT_LIVE=1` 门控的活体验收，见 §3）；
`vite build` → 45 modules，CSS 16.91 kB。

## 2. guard 不只是「通过」，还验证过它能失败

* 干净树：`bash scripts/guard.sh` → `GUARD PASSED`（crates=31 / ui=17 / 违规=0 / 警告=0，11s）。
* **反向验证**：临时注入 `crates/xt-contract/src/__guard_probe.rs`（内容 `pub fn retry() {}`）
  → `GUARD FAILED` 并指出该行；删除后再次 `PASSED`。守卫不是永远为真的断言。
* 守卫自身修过两次缺陷（都记录在案）：① 块注释里的中文词被误判 → 改为 token 化扫描；
  ② 多趟替换把 `", "` 误当字符串 → 改为单次 token 化，且**保留行号**。

契约层另有 `crates/xt-contract/tests/wire.rs`（8 passed）：每个 Request/Response/Event/Frame
变体做 JSON round-trip；断言集合应答的具名键；并有一条**活证据**证明
「内部 tag + 单字段包序列」会被 serde 拒绝（这正是 frontend 发现的真缺陷）。

## 3. 独立验收（不是实现者自述）

| 谁 | 做了什么 | 结果 |
| --- | --- | --- |
| ux（独立） | 真 daemon + 真 xray + 真 AF_UNIX + 真 SOCKS，驱动真 UI 渲染；`XT_LIVE=1` 连跑 5 次 | 5/5 exit 0：每次真收 65536 B、每次 StatsService `downlink=131362`（`sampled_at_ms` 各不相同，非缓存）、界面显示 `128.3 KiB（131362 字节）` |
| ux（独立） | truthfulness 的 5 条断言各做一次 `XT_TEST_MUTATE` 反向验证 | 5 条全部真的变红（A4 第一版恒真，被 ux 自己抓出来改掉） |
| backend-3 | 真 xray 环回 E2E：connect→SOCKS→stats→switch(新 pid)→再传→disconnect(pid 消失)→probe TTFB | 3 轮每轮 stats 非 None |
| backend-2 | 真 daemon + `xt-cli` 联调；`xray run -test` 三种生成配置 | 全 `Configuration OK.`；未知节点 `not_found`；`--timeout-ms 1` 得到真实 `io` 超时而不是挂死 |

## 4. lead 在集成阶段做的一处代码修复（写在这里，因为它改变了行为）

**现象**：冻结前的 `verify.sh` 出现过失败，E2E 断言「Connected 之后立刻 Status 必须有真实采样」
间歇失败（实测一批 8 次里失败 2 次；独立 E2E 单跑也曾失败）。

**根因（两条，都是"发布状态的时机"问题，不是重试能解决的）**：

1. `flow.rs` 先 `commit(CoreReady)` 发布 **Connected**，之后才 `StatsClient::connect(...)`。
   而 UI 的采样是**消费者驱动**的：它看到 Connected 就会问 Status。
   → 存在一个"状态说已连接、统计还没装好"的窗口，窗口内 `stats=None`，界面会显示「未采样」。
2. 反过来，h2 握手成功 ≠ gRPC 服务已能应答。仅把 api 端口纳入 TCP 就绪判定，
   仍可能「连上了但第一次查询失败」。

**修法（两条都遵守 I1/I2：无 sleep、无重试、无回落）**：

* 顺序改成 **先装好统计链路，再发布 Connected**（顺序即语义）；
* 把「能真的查到一次计数」并入就绪判定：失败就等核心的**下一条日志**再试
  （事件驱动；核心被查询时会打日志，循环由真实事件推进），用 `timeout_at` 兜住
  「核心一声不吭」的情况（失败上限，不是等待手段）；窗口内始终不通 → 如实发 Notice
  + 保持「未采样」，**不**用 0 顶替。

**回归证据**：修后 `e2e_real_xray` **连续 20 次全绿**（`PASS=20 FAIL=0`），
此前同一命令的一批 8 次里失败 2 次。诚实地说：**这条竞态是先出现 2/8 才被彻底定位的**，
中间一次"只修顺序"的版本仍有 2/8 失败，所以我按上面的第二条把它一并堵住了。

> 该改动落在 `crates/xt-daemon/src/flow.rs`（原属 backend-3）。所有 writer 停止后由 lead 改，
> 并重跑了 clippy / workspace tests / E2E / guard / UI 全套。

## 5. **没有验证**的东西（不许在别处被写成"可用"）

| 项 | 为什么没有证据 | 解锁条件 |
| --- | --- | --- |
| macOS TUN + 特权 helper | 本轮**根本没实现**（不在范围内）；开发机是 Linux | 有 macOS 真机 + helper + 真机验收 |
| macOS 上的真实运行 | 只有 `aarch64-apple-darwin` 交叉 `check` 的**目标可用性**，没有跑过任何 macOS 二进制 | 真机 |
| Tauri 生产传输（`tauri.ts`） | 无 Tauri/macOS 环境；活体验收走的是 `unixSocket.ts`；Rust 侧没有 `xt_daemon_request` 这个命令 | 打包应用 + 真机 |
| 订阅远端拉取（http/https） | 本轮不做（不宣告 `subscription_fetch`） | 决定承担 TLS 客户端依赖面之后 |
| probe 的默认联网靶点（cloudflare generate_204） | 只对**环回**靶点真跑过（避免测试依赖外网） | 允许测试联网时 |
| 界面的观感 / 对比度 | 对比度是公式实算，没有浏览器渲染采样；没有截图 | 真机 + 截图 |
| 非 Tauri 浏览器环境下的 unixSocket 通道 | 已知会以 `transportError(io)` 失败（代码里如实标注），没把它当"可用" | —（这是设计事实，不是缺陷） |

## 6. 已知的、记录在案但未处理的小事

* live 测试里有 2 处 React `act(...)` 警告（测试仪器问题，不影响断言，ux 已如实记录）。
* `xt-cli` 的 `subscriptions` 是只读真实现；`add/refresh` 固定 exit 2 并说明能力未宣告。
* 本工作区已作为 `xraytun-next/` 子树提交到同一仓库的分支 `xraytun-next-v1`（见 §7.5），
  所以证据既在文件里、也在提交信息里；旧仓库 `xray-tun/`（v0.9.2）全程**一行未改**。

## 7.5 发布通道（Linux 核心二进制）

**为什么要单开一条流水线**：仓库根的 `release.yml` 产的是 macOS 应用（dmg/zip + 公证 + 站点），
跑 `macos-14`、几十分钟；这条分支交付的是 linux-x86_64 的 `xt-daemon` / `xt-cli`，跑 ubuntu、几分钟。
混在一条里会互相阻塞。流水线文件：`.github/workflows/xraytun-next-release.yml`。

**为什么不用本机 PAT 发**：本机那只 PAT 对 Releases 没有权限（实测 403
`Resource not accessible by personal access token`），而仓库自带的 workflow token 有
`contents: write` —— 这也正是根 `release.yml` 一直在用的通道。**二进制因此不进 git**：
产物走 Release 资产、源码走 git 历史，两边由 sha256 对上（根 `.gitignore` 一贯不让大体积
第三方产物入库）。

**怎么发**：

```bash
git tag -a xraytun-next-v1.0.0 -m "..." <commit>
git push origin xraytun-next-v1.0.0        # 推 tag 即触发：守卫 → clippy → 测试(真 xray) → 构建 → 发布
```

**线上资产**（`https://github.com/harodggg/xrayTun/releases/tag/xraytun-next-v1.0.0`，预发布）：

| 资产 | 字节 | sha256 |
| --- | --- | --- |
| `xraytun-next-1.0.0-linux-x86_64.tar.gz` | 2,826,693 | `30dd0da73e8d4141b47b8ebe20b55d2f42a577a13e1ffd97a896bcd109644de9` |
| `SHA256SUMS.txt` | 105 | — |

**发布产物本身也真跑过**（不是"CI 绿了就算"）：把资产下载回来、`sha256sum -c` 通过，
解出来的 `xt-daemon` / `xt-cli` 再跑一遍完整冒烟 →
connect 71 ms、经 SOCKS 收 65536 字节且 sha256 与源站逐字节相同、
`stats uplink=96 downlink=131492`（真实 StatsService）、断开后 pid 消失。
同一份清单也贴在该 Release 的说明里。

## 7.6 CI 抓到过两个"本机能过、换台机器就过不了"的问题（都已修）

| # | 现象 | 根因 | 修法 |
| --- | --- | --- | --- |
| 1 | `xt-xrayconf` 3 条真核心验收测试在 CI 直接红 | 同一个东西两个环境变量名：端到端测试用 `XT_XRAY_BIN`，契约验收测试用 `XRAY_BIN`；且两处都把**开发机绝对路径**当默认值写进了公开仓库 | 统一为 `XT_XRAY_BIN`（旧名 `XRAY_BIN` 继续认）→ `PATH` 里的 `xray`；删掉所有硬编码路径；**找不到仍失败、不静默跳过** |
| 2 | `xt-datapath` 3 条用例报 `Text file busy (os error 26)` | libtest 每用例一线程：线程 A 写夹具脚本（fd 可写）时线程 B `fork()`，子进程继承了那个可写 fd，A 随后 exec 该文件即被拒。"写临时文件→改名"治不了（inode 没变） | 给"会 spawn 进程"的用例加进程内串行锁（`FIXTURE_LOCK`），夹具创建函数把锁作为返回值交出去；配 `#[allow(clippy::await_holding_lock)]` 并写明为什么异步锁不管用 |

两次都是 CI 先发现、人后理解 —— 这就是"真跑一遍"的价值：它们都能编译、本机也都能过。

## 8. 怎么复核（任何人）

```bash
export RUSTUP_HOME=/Users/xbtg-/deepseek-harness/.rustup
export CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo
export PATH="$CARGO_HOME/bin:$PATH"

cd /Users/xbtg-/deepseek-harness/xraytun-next
bash scripts/verify.sh                 # 全量（含真 xray E2E 与 UI）
bash scripts/verify.sh --fast          # 只跑 guard + clippy + 单测

# 活体验收（真 daemon + 真 UI）
cargo build -p xt-daemon
cd apps/ui && XT_LIVE=1 XT_SOCKET=/tmp/xraytun-live.sock npx vitest run tests/acceptance/live-daemon.test.ts
```
