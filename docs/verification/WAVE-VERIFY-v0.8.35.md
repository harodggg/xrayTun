# v0.8.35 波次独立验证（tester 自己量，不看自述）

> 规矩：**自己量、给原始输出、假绿比红危险**（`task-119` 的 M1「空改」是最好的例子）。
> 需要隔离就用 `scripts/wt.sh`（worktree 自带独立 `CARGO_TARGET_DIR` + 构建锁）。

## 1. `task-111` 助手版本判据改成「协议兼容性」

### 1.1 我读到的判据（不是转述卡面）

```
apps/desktop/src/commands/helper.rs
  :139-146  helper_versions_are_compatible(installed, bundled)
              match (installed.protocol, bundled.protocol) {
                  (Some(a), Some(b)) => a == b,                        ← **协议相等**
                  _ => installed.version == bundled.version,           ← 协议读不到时退回「包版本相等」（保守）
              }
  :101-105  parse_helper_protocol(line) —— 从 `(protocol N)` 取数字，取不到就是 None（**不猜**）
  :181      classify: 两边都读到 且 helper_versions_are_compatible ⇒ Match
```
依据（实现者写在注释里，我核对了行号存在）：协议号的唯一来源是 `crates/xt-proto/src/lib.rs` 的
`PROTOCOL_VERSION`；握手本来就用它当兼容键（`helper_client.rs` 发、helper 回），这里只是把同一把尺子用到版本检查上。

### 1.2 基线：**隔离 worktree 上全绿**

```
$ ./scripts/wt.sh new v111 HEAD          # worktree 自带独立 target dir
$ ./scripts/wt.sh run v111 -- cargo test -p xraytun-desktop --lib -- helper
test commands::helper::tests::same_protocol_different_package_version_is_match ... ok   ← 本卡核心
test commands::helper::tests::different_protocol_is_still_mismatch ... ok              ← 安全属性
test commands::helper::tests::missing_protocol_falls_back_to_conservative_version_compare ... ok
test commands::helper::tests::unreadable_version_is_not_reported_as_mismatch ... ok    ← task-84 反例
test commands::helper::tests::version_line_parsing_is_strict ... ok
…（共 24 个 helper 相关测试）
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 160 filtered out; finished in 1.43s
EXIT=0
```

### 1.3 双向敏感性：把判据**退回旧的「包版本相等」** ⇒ 必须红

只在 worktree 里改一行（生产代码一行没动）：
```rust
-        (Some(a), Some(b)) => a == b,
+        (Some(_a), Some(_b)) => installed.version == bundled.version, // MUTANT
```
```
test commands::helper::tests::same_protocol_different_package_version_is_match ... FAILED
thread '…same_protocol_different_package_version_is_match' panicked at helper.rs:331:9:
assertion `left == right` failed: 协议相同 ⇒ 一致；报**已安装**那份的版本（那才是实际在跑的助手）

test commands::helper::tests::different_protocol_is_still_mismatch ... FAILED
test result: FAILED. 0 passed; 2 failed; 182 filtered out
EXIT=101
```
**关键读法**：这个突变**同时打坏两个方向** —— ①「协议同、包版本不同」被误报成 Mismatch（本卡要修的误报）；
②「协议不同但版本串相同」被误判成 Match（**丢掉安全属性**）。两条测试各挡住一个方向 ⇒ 判据是**双向**被钉住的。

还原后：
```
$ git -C <wt> checkout -- apps/desktop/src/commands/helper.rs     # 0 个改动
test …same_protocol_different_package_version_is_match ... ok
test …different_protocol_is_still_mismatch ... ok
test result: ok. 2 passed; 0 failed    EXIT=0
```

### 1.4 ⚠️ 我发现的**新问题**：注释仍在说「版本一致 / 版本不同」（**陈述与实现不符**）

`d27239c` **只改了 `helper.rs`**（`--stat`：1 file changed），**没有同步**这两处**面向读者的**注释：

```
apps/desktop/src/state.rs         Match { version }    /// 两边都读到了，且**版本一致** → 界面不该提示
                                  Mismatch { … }       /// 两边都读到了、**版本不同** → 提示 + 重装入口
apps/ui/src/types.ts              「…所以「装的」与「包里带的」**不一致**时，helper 侧那一部分修复就没生效」
```
按现在的实现，**Match 可以是版本不同**（协议同），**Mismatch 也可以是版本相同**（协议不同）⇒ 这两处注释**已经错了**。
**这正是本项目反复记的那类缺陷**（「改了实现、漏改陈述」）。危害等级 **B**（不影响行为，但会把下一个读代码的人带偏 —— 而 `task-111` 卡面本身就是因为「陈述与实现不符」才开的）。
**建议**（`task-111` 的 owner，不是我）：把注释改成「协议相同 ⇒ 一致；协议不同（或读不到且包版本不同）⇒ 不匹配」，
并同步 `types.ts` 的措辞。**我没有替他们改**（写入范围不在我）。

### 1.5 诚实清单（`task-111`）

1. **真机界面表现未验证**：要复现原误报需要「App 0.8.34 + 已装 helper 0.8.33」，而本机已装的是 **0.8.34 = Match**
   ⇒ 要么降级安装旧 helper（**特权 + 有副作用，我不做**），要么 Tauri WebView 截图（**本环境无法自动化**）。
   我验证到的是：**判据的三态**（单测 + 我这边的突变敏感性）与**界面消费三态的测试**（`helperVersionNotice.test.tsx`：`match` 不给重装入口、`unreadable` 不猜成一致）。
2. 我**没有**跑 `cargo clippy`（只跑了 helper 相关的单测）；也没跑前端 `vitest`。
3. 突变只改了一个条件（判据），**不是**系统变异测试。

## 2. `task-114` 事故上报端点（**线上真部署**）—— 我独立复跑

端点：`https://xraytun.top/api/incident`。我只发**必要的**请求（原因见 §2.2）。

| # | 请求 | 结果 | 判读 |
|---|---|---|---|
| 1 | `POST /api/incident`，包内含 `vless://<uuid>@…?pbk=SECRETPBK` | **HTTP 422** `{"error":"secret_detected","hits":[{"type":"node_url","file":"leak.txt","line":1},{"type":"uri_sec…` | **fail closed 生效**；且 `grep -c 'SECRETPBK\|11111111-2222'` 响应 = **0** ⇒ **不回显密钥**（报告本身不是泄漏源）✓ |
| 2 | `GET /api/incident/INC-19700101-000000-dead` | **404** `{"error":"not_found"}` | ✓ |
| 3 | `GET /api/incident/<同上>/blob`（无 token） | **401** `{"error":"unauthorized","message":"需要 X-Auth-Token"}` | 鉴权在取正文之前 ✓ |
| 4 | `DELETE /api/incident/<同上>`（无 token） | **401** | 未授权不得删除 ✓ |
| 5 | `POST /api/incident/`（**带尾斜杠**） | **201** | 路由前缀修复生效（`/api/incident` 与 `/api/incident/*` 都覆盖）✓ |

### 2.1 ⚠️ 我自己的操作失误（如实报，不藏）

第 5 步我用 `-o /dev/null` 丢掉响应 ⇒ **没有记下那个上传 id**，因此**无法删除它**：
它是个 **226 字节的干净测试包**（只含一行 `{"note":"clean bundle for endpoint verification"}`），
会由 R2 生命周期在 **30 天**后自动过期。**这是我的失误**（正确做法是先把 id 存下来）。
⇒ 也说明 `verify` 流程应当**强制打印/保存 id**，否则「可删除」这个承诺在测试路径上就落空了。

> **后续（Lead 已把它变成机制）**：这个包已由 Lead 用 R2 API 逐对象清掉（桶里 0 个对象）；
> 并且「**任何会 POST 的路径都必须保存 id；拿不到 id 要明确报『无法清理』**」已作为要求发给 `deploy-check.sh` 的 owner。
> 「测试不该制造它声称要避免的危害」这条判断，Lead 已认可并要求**以后都这样**。

### 2.2 我**故意没测**的项（并说明理由）

1. **429 限流**：会消耗本 IP 的每小时额度（5 次），**可能挡住用户真实的故障上传** ——
   信息价值（确认限流存在）**小于**这个代价，且 **429 已由 Lead 在一次端到端里验证过**
   （并已认可「测试不该制造它声称要避免的危害」这条判断）。**我不烧这个额度。**
   我这次共用了 **2 次 POST**（1 次 422 + 1 次 201）。
2. **>10 MB 的大小上限（413）**：要传 10 MB 才测得到，**带宽与流量不值当**，且 Lead 已在自己的序列里覆盖。
3. **带 token 的 `GET blob` / `DELETE`**：**我没有 token**（那是 Lead/owner 的），所以这两个「正常路径」我**验证不到**。
4. **不采集 IP 到 R2**、**30 天保留**这两条**声明**：我只能验证响应里没有 IP 回显，
   **无法**从外部证明服务端没把 IP 写进持久层（需要看 R2/Worker 侧，即 `infra/**`，不在我范围）。

## 3. 本波其余项的现状（我会按顺序继续）

| 卡 | 状态 | 我下一步 |
|---|---|---|
| `task-117` 路由编辑器 | 已提交（`cb4461c` / `7450cd4`） | 验「顺序即优先级真的进了 `runtime/config.json`」+ 预设遮蔽警告 + UI 测试是否行为级 |
| `task-120` 界面陈述审计 | frontend-dev 仍在修（已 9 条） | 等它声明做完再验（**不验移动靶**） |
| `task-122` A-1/A-2（helper 侧） | 待 backend-dev 落地 | 到时验**核心判据**：回滚失败时**不许**再回「已回滚」 |
| `task-110/107/105` 日志读侧/轮转 | 已提交 | 复核「新旧文件混读不再丢 / 不再重叠」 |

## 4. 证据文件（本机）

| 文件 | 内容 |
|---|---|
| `/tmp/v111.log` | `task-111` 隔离基线（24 passed，EXIT=0） |
| `/tmp/v111-mut.log` | 突变后（2 FAILED，EXIT=101） |
| `/tmp/v111-revert.log` | 还原后（2 passed，EXIT=0） |
| `/tmp/ictest/*.out` | 线上端点的原始响应体（422 / 404 / 401 / 401） |
