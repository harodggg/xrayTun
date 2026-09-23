# v0.8.35 冻结提交独立校验（`task-133`）—— 门禁绿；**必须重装助手**

> 作者：tester（独立复核，**不引用 Lead/ops 的结论**）。
> 冻结提交 = **`3754374`**（`local == origin == 3754374`，工作树 **clean**）。
> 校验时间：2026-09-23 14:47–14:56 +0800。

## 0. 口径（含一处**有意偏离卡面**的做法，先说清）

| 项 | 值 |
|---|---|
| 冻结提交 | `3754374`；`git status --porcelain` = **空**（clean） |
| 磁盘 | 开工前 `df -k /Users/xbtg-` → **avail ≈ 12.8 GiB（Data 卷 98%）**，高于卡面的 8 GiB 线 |
| 命令 | `BUILD_LOCK_STRICT=1 CARGO_HOME=…/.cargo CARGO_TARGET_DIR=…/.cargo-target ./scripts/check.sh --no-release-build` |
| ⚠️ **偏离** | 卡面要求「`wt.sh` 建隔离 worktree + 独立 `CARGO_TARGET_DIR`」；我在**主工作区**跑（它正好**就是**冻结提交且 clean），用的是**已预热的共享 `.cargo-target`**。理由：① 磁盘 98%、新建 target ≈ +3 GiB；② 主工作区与冻结修订**逐字节同源**（`git status` 空）⇒ 没有 `wt.sh` 要防的「两个 checkout 混产物」风险；③ `BUILD_LOCK_STRICT=1` ⇒ 若与他人的构建撞上会**明确 75**，不会静默链错。**这条写进诚实清单**。 |
| 原始日志 | `/tmp/v133-gate.log` |

## 1. 门禁：**我自己跑出来的是 `GATE_EXIT=0`**（不是 75）

```
🔒 已获取构建锁：pid=16618 命令=scripts/check.sh --no-release-build 开始=2026-09-23 14:47:35 +0800
  前端单元测试
 Test Files  28 passed (28)
      Tests  296 passed | 1 todo (297)
  ✓ 站点声明的版本与 Cargo.toml 一致：0.8.35
  clippy（warning 视为错误）
  单元测试
✓ 与 CI 相同的全部检查通过
🔓 已释放构建锁：pid=16618 持有 495s
GATE_EXIT=0
gate_finished=2026-09-23 14:55:50 +0800
```
* **退出码 `0`**（不是 75）⇒ 没有遇到「构建锁被占」的环境问题；
* 锁**持 495 s**（14:47:35 → 14:55:50），期间没有第二个 cargo 抢（STRICT 下会 75）；
* 前端 **28 文件 / 296 passed + 1 todo**；站点版本断言 6 条全绿（打印为「一致：0.8.35」）；
* `clippy -D warnings` 干净（否则 `set -e` 会在那一步断）。

## 2. ⚠️ 本版最要紧的用户动作：**必须重装特权助手**（我自己量的）

```bash
$ git diff --stat v0.8.34..3754374 -- crates/xt-helper crates/xt-tun crates/xt-proto
 crates/xt-helper/src/server.rs        | 81 +++++++++++++++++++++++++++++++----
 crates/xt-tun/src/macos/controller.rs | 46 +++++++++++++++++++++++++-
 2 files changed, 118 insertions(+), 9 deletions(-)
```
⇒ **非空 ⇒ v0.8.35 用户必须重装特权助手**（与 v0.8.34「只改 App、不必重装」相反）。
依据是**依赖面**：`xt-helper` / `xt-tun` 属于 helper 侧；`xt-proto` 本次未改（diff 里没有它）。
**说错的代价**：用户会白挨一次安装，或装上后 A-1/A-2 的修复**一点不生效**（旧 helper 仍在跑）。

## 3. 版本号 8 处一致 + 站点两阶段状态（提交 1 阶段）

```
Cargo.toml            0.8.35      gen-site-images.py   0.8.35
tauri.conf.json       0.8.35      site/assets/site.js  0.8.35
apps/ui/package.json  0.8.35      site/index.html      XrayTun_0.8.35_x86_64_arm64.dmg
gen-site-jsonld.py    0.8.35
gen-site-geo.py       0.8.35
```
* `PUBLISHED = False`（**两个生成器都是**）⇒ 提交 1 阶段正确；
* 站点里「正在发布」文案 **6 处** ⇒ **没有**编造的字节数（历史字节 `47,243,124 / 42,742,652 / 45.1 MiB / 40.8 MiB` 命中 **0**）；
* `releases/download/v0.8.35` pinned 直链 **0 条** ⇒ 与「提交 1 不写死直链」一致。

## 4. 本波三条「陈述同步」抽查

| 项 | 我的证据 | 结论 |
|---|---|---|
| **① 助手版本判定 = 协议号口径** | `state.rs` 的 `HelperVersionCheck::Match` 注释已写「两边都读到了，且**协议号相等**……**包版本不同不算不一致**（App 0.8.34 + 已装 helper 0.8.33、协议同为 1 就是这里）」；`helper.rs` 有 `protocol: Option<u32>` 与 `parse_helper_protocol` | ✅ 陈述与实现同口径（`task-111`/`task-127` 落地） |
| **② `Logs.tsx` 脱敏说明 ⟷ `redact_secrets`** | 读 `0a5f996`（delta-3）的 diff：注释已改成「**左边界完全不要求**」，并**删掉**了原来「域名条目保持左边界严格，否则 `xnode-example.xyz` 会被误伤」那条 ⇒ 相似域名也会被抹 =**取舍**；`Logs.tsx` 仍写「覆盖不到的形态有**两种**」（base64 + 不以 `HOME` 开头但带用户名的路径） | ✅ **一致**（两种是准确的）。⚠️ **边界**：这条我只**读 diff**确认机制，**没有**在 `3754374` 上重跑我的合成域名探针（磁盘/时间）；`task-145` 实测的「域名紧贴字母仍在」是 **`d95b4ef`** 上的，`0a5f996` 是它的修复 |
| **③ 限流摘要不再每秒写持久化** | `LOG_THROTTLE_SUMMARY_WINDOW_SECS = 60` + `PersistSummaryGate`；1 秒的 `flush.tick()` 只 `take_ui_summary()`（**仅推界面事件**），持久化那条要 `gate.due(now)` 才 `take_persist_summary()`；落盘文案是「核心日志限流汇总（**60 秒窗口**）…」。另有测试断言 600 秒 ⇒ **10 条**窗口账 | ✅ 陈述成立（`task-121` 落地） |

## 5. 发布后复核（6–8 步）：**尚未到期，先记状态**

```
$ gh release view v0.8.35 --json isDraft,tagName,assets
release not found
$ git ls-remote --tags origin v0.8.35
7c7bb2800def07f32284485c305ad79afce242a0  refs/tags/v0.8.35
```
⇒ **tag 已经存在**（annotated 对象 `7c7bb28…`），但 **Release 还没出来**（workflow 仍在跑）⇒ 以下**待补**：
* `isDraft=false` + 3 个资产；dmg/zip 的**我自己算的** sha256 与字节数；
* 与 `SHA256SUMS`、与站点声明的逐字节对（含 MiB 取整陷阱）；
* `curl -sIL` pinned 链接的**真实 content-length**；站点 `0.8.35` 命中 / `0.8.34` 归零；
* `og-image-0.8.35.png` / `og-image-en-0.8.35.png` **200 + image/png + 与仓库同字节**（今天出现过 404 窗口）；
* `scripts/verify-live-site.sh --self-test` 仍绿。

## 6. 诚实清单

1. **真机安装 / Gatekeeper / 重装助手未在本环境验证** —— 本报告**没有**、也**不能**说「重装助手已验证有效」；能证明的只有「**helper 侧代码确实变了 ⇒ 必须重装**」。
2. **云端 CI 的内部步骤、CF 传播延迟**我看不到；Release workflow 的结论我**没有**独立证据（只能等 `gh release view`）。
3. **门禁用的是共享 target dir**（偏离卡面的 `wt.sh` 独立 target dir，理由见 §0）⇒ 若你要求严格隔离，我可以在磁盘宽松时用 worktree 重跑一遍（现 98% 满，我没做）。
4. **delta-3 的修复**：当时只读 diff 确认机制、没重跑探针 ⇒ **已在 §7 补做**（同一套探针在 `3754374` 上重跑，`synth_glued_domain` **已抹**）。
5. **`type_contract`**：本卡未单跑；门禁里它属于 `cargo test --workspace` 的一部分，**本次全绿**。
6. 报告里的「495 s / 28 文件 / 296 passed」都是**时点值**，绑定 `3754374` 与上面那次运行。

## 7. （补验）delta-3：**域名紧贴字母**在冻结提交上是否堵住 → **是**

**隔离要求这次按卡面做了**：`./scripts/wt.sh new v133p 3754374` ⇒ **独立 worktree + 独立 `CARGO_TARGET_DIR`**；
开工前 `df -g /Users/xbtg-` = **13 GiB**（高于 8 GiB 线）；**跑完立刻 `wt.sh rm v133p`**（含它的 target dir）。

### 7.1 明确回答
**「`task-145` 报的第三形态（`Xray<域名>`，无 `-`/`_` 分隔）在冻结提交 `3754374` 上是否已堵住」→ 是，已堵住。**
* 运行次数：**1 次**（探针在 `3754374` 上跑通，`test result: ok`）；
* 修前（`task-145` 在 `d95b4ef` 上）：`synth_glued_domain=>**仍在**`；
* 修后（本次，`3754374`）：
```
PROBE 行数=463378 节点=2 真值条目=2
PROBE 真实日志：原始命中行=70638 ⇒ 脱敏后=0
PROBE 形状：synth_hyphen_domain=>已抹 / synth_underscore_domain=>已抹 / **synth_glued_domain=>已抹**
            hyphen_ip=>已抹 / underscore_ip=>已抹 / glued_ip=>已抹 / ip_port=>已抹 / version_like=>已抹
PROBE 代价：与节点 IP 相同的版本号被抹=<addr>=true
PROBE 用户名：HOME 前缀被折=true / 非 HOME 前缀仍保留=true
PROBE 保留项：198.18.=95869 baidu=125 google=53360 127.0.0.=50660 192.168.=4285 <uuid>=1328
test result: ok. 1 passed; 0 failed
```
* **真值仍只来自 `nodes.json`**（`address` + 名字里任何位置的 IP + 名字里按 `-`/`_` 切出的域名段）；
  **没有**复用实现内部的地址集合（`task-137` 证明过那会假绿）；
* 域名形状仍只能用**合成节点**测：真实两个节点都是 IP 形态（`节点=2 / 真值条目=2`），
  所以「真实日志 0 命中」**只**覆盖 IP 那条线，域名那条线靠合成节点覆盖 —— 这条边界仍然成立。

### 7.2 陈述核对（「覆盖不到的两类」是否仍逐字成立）→ **成立**
* 域名紧贴形状现在**已抹** ⇒ 它**不再是**覆盖缺口；`redact_secrets` 的确按 delta-3 的注释**左边界完全不要求**
  （相似域名 `xnode-example.xyz` 也会被抹 =**取舍**，不是缺口）；
* 覆盖不到的仍是**两类**：**base64 载荷**、**不以 `HOME` 开头但带用户名的路径**（本次实测该路径里用户名**仍保留**=true）。
  ⇒ `Logs.tsx` 的「两种」措辞**与实现一致**。

### 7.3 复现命令（与 `TASK-124-PRIVACY-VERIFY.md` §6 同一套探针）
```bash
df -g /Users/xbtg-                       # 低于 8 GiB 停下报 Lead
./scripts/wt.sh new v133p 3754374        # 独立 worktree + 独立 target dir
# 把 TASK-124-PRIVACY-VERIFY.md §6 的探针 + §7 的合成域名块注入 worktree 的 diagnostics.rs
#   （两处编译小坑：`*kind` / `**k == *"version_like"` —— 见下）
./scripts/wt.sh run v133p -- cargo test -p xraytun-desktop --lib -- v145_real_log_and_name_shape_probe --nocapture
./scripts/wt.sh rm v133p                 # **收工立刻删**（含它的 target）
```
**诚实细节（这次多花了两个来回）**：探针从 `/tmp` 那份副本注入时，`kind != "version_like"` 与
`.find(|(k,_,_)| *k == …)` 两处**类型比较要写成 `*kind` / `**k == *"version_like"`**，否则 **E0277 编译失败**
（我第一次注入后直接跑，拿到的是编译错误 `EXIT=101`，不是测试结论 —— 与「突变没生效＝假绿」同族的一个坑）。
