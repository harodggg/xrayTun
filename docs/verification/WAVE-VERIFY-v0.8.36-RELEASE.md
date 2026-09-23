# v0.8.36 独立发布验收（冻结对象 `c543515` = tag `v0.8.36`）

> 验收人：`tester`（独立复算，不采信实现者/Lead 的转述数字）
> 绑定：**tag `v0.8.36` → `c543515`**（提交 1）；提交 2 = `03cc02f`（站点真实资产数据）
> 隔离：worktree `a836`（`scripts/wt.sh`，独立 target `.cargo-target.wt/a836`）
> 本轮**没有任何写操作**：POST 0 次、无安装/卸载、无路由/DNS 改动；只做只读查询、下载、curl、cargo（本地）
> 证据强度：`★★★` 我亲跑且留档 · `★★` 仓库/团队留档但我未亲跑 · `★` 转述，无留档

---

## §0 三分口径

| 类别 | 内容 |
|---|---|
| **我量到的** | 独立门禁的退出码与全部计数（含 `Errors` 行）；三个 release helper 的 sha256 与 `version` 输出；5 个守卫新形状变体的逐条红/绿；版本声明扫描；资产/线上（§6） |
| **读码得到的** | helper 侧生产段（剔除 `#[cfg(test)]`）逐条改动与等价性核对；协议号与 IPC 形状；App 的助手兼容判据；守卫判据实现；锁机制 |
| **推断的** | `cargo clean -p` 后 release 产物的可比性；`unused_mut` 会随 `n1` 一起告警（本轮**未捕获**该 warning）；真机「删路由失败」的用户可见行为（未做） |

---

## §1 冻结对象与版本一致性

| 项 | 值（我自己 `git rev-parse` / `git ls-remote` / `grep`） |
|---|---|
| tag `v0.8.36` 目标 | 标注 tag 对象 `bf13b9c8…` → `target_type=commit` → **`c5435154f67bd1c1e9513e583664bf379a1bdaa9`**（GitHub API 解引用，见 §6.1；不是只信 tag 名） |
| 提交 2 | `03cc02f`（站点填真实资产数据） |
| 提交 1 相对 `941f71d` | 只动版本号/CHANGELOG/`docs/release-notes/v0.8.36.md`/site/生成器；`git diff 941f71d..c543515 -- crates apps` = **仅 2 行版本号**（`apps/desktop/tauri.conf.json`、`apps/ui/package.json`） |
| 提交 2 相对提交 1 | `git diff c543515..03cc02f -- crates apps` = **空** ⇒ 我验的代码就是被 tag 的代码 |
| 版本声明扫描 | `Cargo.toml [workspace.package]`、`tauri.conf.json`、`apps/ui/package.json`、`site/assets/site.js`、`gen-site-jsonld.py`、`gen-site-geo.py`、`gen-site-images.py` **全部 0.8.36**；`site/index.html` 与 `site/en/index.html` 的下载文件名均为 `XrayTun_0.8.36_x86_64_arm64.dmg`；无残留 `0.8.35` 声明 |
| 站点↔Cargo 一致性 | 由 `check.sh` 的站点一致性步在我**独立门禁**里判定：6 条全 ✓（`✓ 站点声明的版本与 Cargo.toml 一致：0.8.36`） |

---

## §2 item 1 · helper 侧生产行逐条读 ⇒「是否需要重装助手」

### 2.1 方法（可复现）

```bash
# 生产段 = 文件开头到 `#[cfg(test)]\nmod tests` 之前；两版分别提取后再 diff
python3 /tmp/prod_diff.py     # 见本报告 §7 复现清单
```

| 文件 | 生产段行数 `v0.8.35 → 941f71d` | 改动性质 |
|---|---|---|
| `crates/xt-helper/src/server.rs` | 785 → 820 | **真实改动**：卸载站点抽成 `uninstall_outcome`（见 2.2） |
| `crates/xt-tun/src/macos/mod.rs` | 49 → 104 | `run` 拆出 `real_run` + `#[cfg(test)]` 接缝（生产只多一层直调） |
| `crates/xt-tun/src/macos/snapshot.rs` | 138 → 170 | 新增 `snapshot_dir()` 间接层；**生产返回值仍是 `/Library/Application Support/XrayTun`** |
| `crates/xt-tun/src/macos/controller.rs` | 373 → 373 | **生产段逐字节相同**（+118 行全是 `#[cfg(test)]` 测试） |
| `crates/xt-proto` | — | **零改动**（见 §3） |

helper 侧改动只来自两个提交：`96d7c70`（task-134）与 `941f71d`（task-160）。

### 2.2 `uninstall_outcome` 抽取的等价性核对（逐项）

| 核对项 | 旧代码 | 新代码 | 判定 |
|---|---|---|---|
| **失败优先序** | `rollback_failed = Some(..)`，`force` 只 `get_or_insert_with`（先到先得） | `rollback.err().or(force.err())` | **相同**（rollback 优先，force 仅在没有 rollback 失败时补位） |
| **锁与 `force_cleanup` 顺序** | `if let Ok(mut guard)` 块结束（guard drop）→ 再 `force_cleanup()` | `let memory = match self.state.lock() { Ok(mut guard) => … }`，guard 是 match 臂内临时量，臂结束即释放 → 再 `force_cleanup()`（`server.rs:684-698`） | **相同**（回滚在锁内、`force_cleanup` 在锁外） |
| **锁中毒** | `Err(_)` ⇒ 跳过内存回滚、不记失败 | `Err(_) => Ok(())` | **相同**（不回滚、不谎报） |
| **`force_cleanup` 是否仍无条件调用** | 是 | 是 | **相同** |
| **成功/失败文案** | 内联 `match`，两条字面量 | `uninstall_response_message(why)`，**两条字面量逐字节相同** | **相同** |
| **响应形状** | `Response::Ok { message: Some(String) }` | 同（返回 `uninstall_outcome` 给的 `response`） | **相同** |
| **warn 留痕条件** | `if let Some(why) = &rollback_failed` | 同 | **相同** |

### 2.3 产物级（我自己的三向比较，`cargo clean` 后各构建一次 release）

```
v0.8.35 @3754374  sha256=fec3b66d06e49d9ddad0a3fb9d29515ad754767bbf5b1c308021dc6335603451 size=1744748  version: xraytun-helper 0.8.35 (protocol 1)
941f71d  @941f71d  sha256=f6df188946092394e998bd7fb2078635a1a80b8f39957279c232f3ee82d545ed size=1744748  version: xraytun-helper 0.8.35 (protocol 1)
c543515  @c543515  sha256=19c7de293b6a59036c4f7f285fabb2b1a0d21cbe0e78851707b62e1669c374c8 size=1744756  version: xraytun-helper 0.8.36 (protocol 1)
v0835 vs code_only      : **不同**（首个差异在第 217 字节）
code_only vs tagtarget  : **不同**（首个差异在第 1657 字节）
```

**读法（不要把两者混为一谈）**：
* 即使**版本号相同**（`v0.8.35` vs `941f71d`），helper 的 release 产物**也变了** ⇒ task-134/160 的重构确实改变构建产物（函数形状/内联布局），**但产物变 ≠ 行为变**：§2.2 逐项核对的是行为等价；
* `941f71d → c543515` 的差异里**至少包含版本串**（`version` 子命令打印 `env!("CARGO_PKG_VERSION")`）。

### 2.4 判定

**本版不需要新的重装动作**（我独立同意 `docs/release-notes/v0.8.36.md` 的口径）：
1. 协议号未变（§3）⇒ App 判 `Match`，**界面不提示**、不做任何自动安装；
2. helper 侧**没有新的行为修复**（§2.2 全部等价，§2.3 的字节差异来自重构而非语义）；
3. **但**：**没为 v0.8.35 重装过助手的人仍需补装一次** —— v0.8.35 的「回滚失败不再被静默」修复只在助手侧，那次的重装不是可选项。这一点 release notes 写了，我核了，**同意**。

---

## §3 item 2 · 协议号与 IPC 形状

```bash
$ git diff --stat v0.8.35..941f71d -- crates/xt-proto        # 空
$ git diff --stat 941f71d..c543515 -- crates/xt-proto        # 空
```

* `crates/xt-proto/src/lib.rs:32: pub const PROTOCOL_VERSION: u32 = 1;` —— **未变**。
* `crates/xt-helper/src/main.rs` 在区间内**零改动** ⇒ `version` 子命令输出格式不变；实际产物验证：`v0.8.35` 与 `c543515` 两份 helper 的 `version` 输出都是 `… (protocol 1)`（§2.3 原文）⇒ **已装的旧 helper 自报协议 1，包内新 helper 也报协议 1**。
* App 的兼容判据就是**协议号相等**：`apps/desktop/src/commands/helper.rs:139-147`（`installed.protocol == bundled.protocol`，读不到才退回包版本相等），三态分类在 `:176-203`；`apps/desktop/src/state.rs:578-584` 明确「**协议号相等 ⇒ 界面不该提示**」；提示与「重新安装助手」入口只在 `Mismatch` 出现，且 `apps/ui/src/pages/Settings.tsx:929-949` 写明**绝不静默自动重装**。
* **IPC 形状**：`xt-proto` 零改动 ⇒ `Request`/`Response` 枚举与常量形状未变（不是「diff 摘要为空」这一条，我另外读了常量与枚举所在文件、并核了 helper 的 `version`/握手路径未改）。

---

## §4 item 3 · 独立门禁（绑定 `c543515`）

命令（**不二次拿锁**，否则内层 `check.sh` 会等 3600s）：
```bash
./scripts/wt.sh run a836 -- env BUILD_LOCK_HELD_BY_US=1 ./scripts/check.sh --no-release-build
```

```
🔒 已获取构建锁：pid=1736 … 开始=2026-09-23 18:23:12 +0800   ← 全程只获取 1 次
 Test Files  36 passed (36)
      Tests  357 passed | 1 todo (358)
      Errors 行：**无**
  ✓ 站点声明的版本与 Cargo.toml 一致：0.8.36
  应用 UI：1 个样式表定义 23 个 token；268 处 var() 引用（其中 2 处带兜底）
  11 个 Rust 测试二进制：571 passed / 0 failed / 6 ignored（全部 `ok`）
✓ 与 CI 相同的全部检查通过
GATE2_EXIT=0                    ← 退出码
🔓 已释放构建锁：pid=1736 持有 797s
```

* 我另外核了日志里的 2 处 `error:` —— 都是**测试名**（`test error::tests::… ok`），不是失败。
* **与 Lead 在 `c543515` 上的门禁对照**（两个独立 run，不是同一次）：两者 `GATE_EXIT=0`；前端同为 36 files / 357 passed（+1 todo）；`Errors` 行都为空。我方是独立 worktree + 独立 target ⇒ 结论不共享构建产物。
* 释放路径也对：内层 `check.sh` 的 trap 看到 owner pid（1736）不是自己（1758）**没有删锁**，留给外层释放（原始输出：`⚠️ 锁的 owner pid=1736 不是本进程（1758）——不删，留给它的主人`）。

---

## §5 item 5 · 守卫新形状变体（`mut` / 诱饵 / 别名 / 不可达 warn）

环境：worktree `a836` @ `c543515`；每次 cargo 走 `wt.sh run a836 -- …`；每个变体跑完 `git checkout --` 还原（收尾 `git status` 空）。

| 变体 | 改法 | 守卫 | 行为测试 | 结论 |
|---|---|---|---|---|
| **n1** 绑定改 `mut` | `let (mut rollback_failed, response) = uninstall_outcome(memory, force);` | **红** | 绿 | ⚠️ **假红**：守卫对无害的 `mut` 过敏（`unused_mut` 本身也是告警，但本轮未捕获该 warning） |
| **n2** **硬编码诱饵响应** | 保留受检三条件（绑定行在、无 `uninstall_response_message(`、末尾 `response`），但把 `response` 影子成 `Response::Ok { message: Some("helper 已卸载") }` | **绿** | 全绿 | ❌ **仍能绕过**（无编译诊断；用户可见响应被改成成功文案） |
| **n3** 纯函数说谎 | `uninstall_outcome`: `let why = rollback.err();`（丢掉 force 那一路） | 绿 | **红** + `warning: unused variable: force` | ✅ 被行为测试兜住 —— 这正是 (a) 的价值 |
| **n5** 别名改绑定名 | 绑定成 `response_alias` 再 `let response = response_alias;` | **红** | 绿 | ⚠️ **假红**：守卫对绑定名/形状过敏 |
| **s3** supervisor 块内**不可达** warn | 站点块里 `let _ = e;` + `if false { tracing::warn!("（不可达）"); }` | **绿** | — | ❌ **仍能绕过**（B 级：release 下该处静默丢留痕） |

原始输出（节选，全文见 `/tmp/t836-probes.log`）：
```
##### n2 : cargo test -p xt-helper uninstall #####
  test server::tests::uninstall_site_cannot_swallow_rollbacks_in_production_source ... ok
  test result: ok. 3 passed; 0 failed; 0 ignored; 13 filtered out
##### s3 : … core_shutdown_result_is_not_swallowed_in_production_source #####
  test supervisor::tests::core_shutdown_result_is_not_swallowed_in_production_source ... ok
  test result: ok. 1 passed; 0 failed; 0 ignored; 231 filtered out
```

**判定（已报 Lead）**：**n2 与 s3 仍绿**。
* `n2` 就是「先在别处造好完整响应再直接 return」那一类 —— **站点级块判据对它不可判定**（站点仍能自造 `Response::Ok`）。真闭环只能上**站点级行为级测试**（给 `uninstall` 注入 `memory`/`force` 两个结果，断言**最终 Response 的文案**），文本/块判据只能拦低成本写法。
* `s3` 的根因是块判据只要求「块内出现 `tracing::warn!`」，**不检查可达性与是否带错误** ⇒ 建议要求 warn 绑定错误（`%e`）或把判定交回更小的可测单元。
* **不阻塞本版**（当前行为已验为诚实）；两条建议进 task-160 收尾或新卡。

---

## §6 item 4 · 资产 / 线上复核

**时点**：`2026-09-23T10:49:12Z → 10:52:09Z`（本地 18:49–18:52）。所有数字都由我自己查/自己下载复算，未引用转述。

### 6.1 tag 与 release

```
$ gh api repos/harodggg/xrayTun/git/ref/tags/v0.8.36 --jq '.object.type + " " + .object.sha'
tag bf13b9c81b8867afaf8cb82e1f37ab5b53226c73
$ gh api repos/harodggg/xrayTun/git/tags/bf13b9c8… --jq '…'
tag=v0.8.36  target_type=commit  target_sha=c5435154f67bd1c1e9513e583664bf379a1bdaa9
$ gh release view v0.8.36 --json isDraft,tagName
isDraft: False | tagName: v0.8.36
```
⇒ 标注 tag 解引用后的 commit = **`c543515`**，与 §1 的冻结对象一致（不是「只信 tag 名」）。

### 6.2 资产三方对照（GitHub digest ↔ 我下载自算 ↔ `SHA256SUMS.txt`）

| 资产 | 字节（下载实测） | GitHub digest | 我自算 | `SHA256SUMS.txt` | 三方一致 |
|---|---|---|---|---|---|
| `XrayTun_0.8.36_x86_64_arm64.dmg` | 47,446,724 | `70ca6a22e03634f6763c21be3e31a6932fb5a2cbd94ff780c23f2e3a9a5ef8d7` | 同左 | 同左 | **✓** |
| `XrayTun_0.8.36_x86_64_arm64.zip` | 42,936,627 | `2f41f076291ad4554784199913d7cc13ec53c90809ed5fa0b65c9a6211ef51fe` | 同左 | 同左 | **✓** |
| `SHA256SUMS.txt` | 200 | `30508e3741bd7c4191b3d49b0090ed0604a0d1aba4e9edbe8465dcc96dc0c13b` | 同左 | （自身无自条目） | **✓**（digest↔自算） |

`SHA256SUMS.txt` 原文（`cat -e`，`$` = LF 行尾；名字带 `./` 前缀）：
```
70ca6a22e03634f6763c21be3e31a6932fb5a2cbd94ff780c23f2e3a9a5ef8d7  ./XrayTun_0.8.36_x86_64_arm64.dmg$
2f41f076291ad4554784199913d7cc13ec53c90809ed5fa0b65c9a6211ef51fe  ./XrayTun_0.8.36_x86_64_arm64.zip$
```

### 6.3 pinned 直链（`curl -sIL`）

| 链接 | 结果 |
|---|---|
| `…/releases/download/v0.8.36/XrayTun_0.8.36_x86_64_arm64.dmg` | `HTTP/2 302`（本跳 `content-length: 0`）→ `HTTP/2 200`，`content-type: application/octet-stream`，**`content-length: 47446724`** |
| `…/releases/download/v0.8.36/XrayTun_0.8.36_x86_64_arm64.zip` | `HTTP/2 302` → `HTTP/2 200`，**`content-length: 42936627`** |

⇒ 与 §6.2 下载实测字节**逐一相等**。（302 的 `location` 是带签名的临时资产 URL，不入报告。）

### 6.4 站点（`https://xraytun.top/`）

```
HTTP=200 bytes=43980
  0\.8\.36  命中=35      0\.8\.35  命中=0      正在发布  命中=0
  pinned 文件名出现次数=11（我的计数口径：XrayTun_0.8.36_x86_64_arm64.{dmg,zip} 的出现次数）
  页面含千分位字节数：47,446,724 ✓（dmg）  42,936,627 ✓（zip）  200 ✓（SHA256SUMS.txt）
```
* **我验时站点已经是「提交 2 的真实资产态」**（`正在发布` = 0，`0.8.35` = 0）⇒ 本版**不需要**再写「提交 2 落地后复验」；时点已记（`10:49–10:52Z`）。
* 站点声明字节与我自算字节一致（千分位口径）；`0.8.36` 命中 35 次，无旧版本残留。

### 6.5 OG 图

| 文件 | HTTP | 类型 | 字节 | 线上 sha256 | 仓库文件 sha256 | 一致 |
|---|---|---|---|---|---|---|
| `og-image-0.8.36.png` | 200 | image/png | 59,013 | `68dc820e…15485a` | `68dc820e…15485a` | **✓** |
| `og-image-en-0.8.36.png` | 200 | image/png | 42,294 | `e18bdec8…780f5d` | `e18bdec8…780f5d` | **✓** |

### 6.6 `verify-live-site.sh --self-test`

```
self-test：四例全部符合预期（异常样例 ✗ / 缓存残留只 WARN / 真 404 正常）
SELFTEST_EXIT=0
```

### 6.7 Release 正文 ↔ `docs/release-notes/v0.8.36.md`（**不写「整体逐字节一致」**）

```
$ gh release view v0.8.36 --json body --jq .body > /tmp/t836-body.txt   # 6033 B
$ wc -c docs/release-notes/v0.8.36.md                                   # 3137 B
body 以 notes 原样开头: True
余下长度: 2896 | 余下前 160 字节: b'\n---\n\n## 安装\n\n1. 下载 `.dmg` 或 `.zip`，把 `XrayTun.app` 拖进「应用程序」…
```

**准确结论（逐字，不凑整）**：
* 正文**前 3,137 字节与仓库文件逐字节相同**（`body.startswith(notes)` = True，含两文件各自的结尾 LF）；
* 其后 **2,896 字节**是 **CI 模板的通用段**（`\n---\n\n## 安装` …「安装 / 校验」等）；
* 即 **正文 = 版本特有段（仓库文件） + 模板追加**；**不存在**「缺了版本特有段」「重复两次」「截断」这三种失败形态；
* 也**没有**出现 Lead 提醒的「只差一个尾换行」那种情况 —— 差异是**模板追加**，不是尾换行（我按实测写，不按预期写）。

### 6.8 我自己的一次假信号（留档，符合 `GUARD-FALSE-GREEN-PATTERNS` §2 R7）

第一版三方对照脚本把 `SHA256SUMS.txt` 里的 **`./` 前缀当成文件名的一部分**（键变成了 `./XrayTun_…`），又因为 **macOS 的 `cat -A` 不可用**那一节看起来是空的，于是打印了 `三方一致=False`。
修正后（`lstrip('./')` / `pathlib.Path(name).name` + `cat -e`）结论是 **True / True / True**。
⇒ 又一次「**工具/脚本自己的假信号**」：判据没错、数据没错，**读取与规范化错了**。这正是本报告 §7 与规范里要求「工具级信号必须有反向用例」的原因。

---

## §7 附录 A · 构建锁：`per-target-dir` ⇒ 隔离 worktree 与主树**不互斥**

* 判据是**纯 pgrep**，不看锁归属、也不看 target dir：
  `scripts/build-lock.sh:150-152` `_build_lock_foreign_cargo() { pgrep -fl '[c]argo|[r]ustc' | grep -v "build-lock.sh" | grep -v "verify-build-lock" || true; }`
  调用点在**拿到锁之后立刻**：`:228`（`_build_lock_warn_foreign || return $?`）。
* 锁目录由 `CARGO_TARGET_DIR` 推出（`_build_lock_dir`）⇒ 主树 `.cargo-target.lock.d`、我的 worktree `.cargo-target.wt/a836.lock.d` 是**两把不同的锁**。
  ⇒ **`wt.sh run` 包一层并不能让两边互斥**；strict 门禁只要在 acquire 那一刻看到任何 cargo/rustc 就会 `GATE_EXIT=75`（**75 = 环境问题，不是代码失败**）。
* 本轮真实代价：Lead 在 `c543515` 上跑 `BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build` 时，我的 cargo 正在跑 ⇒ 空跑一次（原文见 Lead 留档；根因即上面两行代码）。
* 正确用法：① **时间上错开**（本轮已这么做）；② 想容忍并发就用 `BUILD_LOCK_FOREIGN_WAIT=<秒>`（`:180-191` 会**可见地等**未持锁 cargo 结束）；③ 已开 `task-162`（strict 只对「同一 target dir 的未持锁 cargo」失败，跨 target dir 降级为 warning）。
* 顺带一条使用坑：`env BUILD_LOCK_HELD_BY_US=1` 必须加在 `wt.sh run --` **之后**的命令上（让内层 `check.sh` 跳过二次拿锁）；**加在外层会让外层跳过拿锁**（`build-lock.sh:208-210` 把它当「重入」）。

---

## §8 附录 B · Release notes 口径核对（没写强也没写弱）

`docs/release-notes/v0.8.36.md` 头部原文（我读的仓库文件）：
> 本版不需要新的重装动作（但请对一下你上次装的是哪一版）／**本版没有新的助手侧行为修复** —— 若你已为 v0.8.35 重装过助手，**不必**再装；**若还没为 v0.8.35 装过，请重装一次**（v0.8.35 的「回滚失败不再被静默」修复在助手侧）。／`crates/xt-proto` **一行未改** ⇒ 协议号不变 ⇒ 设置页**不会**报「助手版本不匹配」。

* 「协议号不变 ⇒ 不会报不匹配」—— 与 §3 一致（[`PROTOCOL_VERSION = 1`](crates/xt-proto/src/lib.rs) + 判据读码）✓
* 「没有新的助手侧行为修复」—— 与 §2.2 的逐项等价核对一致；**没有写弱**：它没有（也不该）声称「产物逐字节相同」，而 §2.3 证明**产物字节确实会变** ✓
* 「没为 v0.8.35 装过的请重装一次」—— 与「v0.8.35 的修复只在助手侧、且 App 判据是协议号、协议号那一版也没升」一致：**这句必须写**，否则那部分用户永远用旧 helper ✓
* 结论：口径**既没写强也没写弱**。

---

## §9 诚实清单

* **没有在真机上做「删路由失败」**：本卡不连接/断开、不改路由/DNS、不装/卸助手；§2.2 是**读码 + 产物级**证据，§5 的变体都在 worktree 内跑。
* **`n1` 的 `unused_mut`**：我只测到守卫变红，**没有捕获** `unused_mut` 告警原文 ⇒ 标为**推断**。
* §2.3 的 release 产物是**本机 dev 工具链**构建的，不是 CI 打的 universal 资产 ⇒ 它回答的是「源码变化是否改变构建产物」，**不等于**「已发布的 dmg/zip 内 helper 与 v0.8.35 的不同」。
* §4 的门禁是**本机隔离 worktree** 的独立 run，与 CI runner 是不同环境（CI 的结论我只作为对照，不作为我的证据）。
* §6 的线上数字带**时点**（`2026-09-23T10:49–10:52Z`）：我验时站点已是提交 2 的真实资产态，因此本版**不需要**「提交 2 后复验」这条待办；但**线上状态会变**，这份记录只对它被观测的那一刻负责。
* **我自己犯过一次假信号**（§6.8）：三方对照脚本误把 `./` 当文件名 + macOS `cat -A` 不可用 ⇒ 打印了假的 `三方一致=False`；已修正并复算为 **True/True/True**。这条**留档在报告里**而不是删掉，理由见规范 §2 R7。
* 站点 `pinned 条数=11` 是**出现次数**（不是唯一链接数、也不是下载按钮数），不同口径不可直接与历史报告的数字比较。
* 未覆盖：真机 Gatekeeper/公证验证、helper 真机重装、真机 Tauri GUI 行为、发布 workflow 内部步骤（只读其产物）。
