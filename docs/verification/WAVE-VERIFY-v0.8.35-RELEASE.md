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

## 8. （补）发布后复核 6–8 步（**我的下载与我的 curl**）

### 8.1 资产：**三方逐字节一致**（`gh api` digest / 我下载后自算 / `SHA256SUMS.txt`）

```
$ gh release view v0.8.35 --json isDraft,isPrerelease,tagName,publishedAt,assets --jq '…'
draft=false pre=false tag=v0.8.35 at=2026-09-23T07:17:15Z
SHA256SUMS.txt                  200       uploaded  sha256:3c40bf4c259efd473f7e942258decf5ea1e8e99b54af39b820ec247626214fa5
XrayTun_0.8.35_x86_64_arm64.dmg 47431435  uploaded  sha256:c17c9529805eacec124f6d953c7918e02a40554d323239b4a7e08f39c2b4004b
XrayTun_0.8.35_x86_64_arm64.zip 42919784  uploaded  sha256:0f1000e76e566c57d2577e7cc9addbd92f31071bf899b3fc365833fc6d5e7fcb

$ gh release download v0.8.35 --dir /tmp/v133rel && shasum -a 256 *.dmg *.zip SHA256SUMS.txt
c17c9529…004b  XrayTun_0.8.35_x86_64_arm64.dmg      （47,431,435 bytes，我自己 wc -c）
0f1000e7…7fcb  XrayTun_0.8.35_x86_64_arm64.zip      （42,919,784 bytes）
3c40bf4c…4fa5  SHA256SUMS.txt                        （200 bytes）

$ cat SHA256SUMS.txt
c17c9529…004b  ./XrayTun_0.8.35_x86_64_arm64.dmg
0f1000e7…7fcb  ./XrayTun_0.8.35_x86_64_arm64.zip
```
⇒ `isDraft=false` + **3 个资产**；**我下载后算的 sha256/字节 = `gh` 的 digest = `SHA256SUMS.txt` 里的两行** ✓。
MiB 口径：47,431,435 B = **45.23 MiB**（应为 45.2）、42,919,784 B = **40.93 MiB**（应为 40.9）—— 提交 2 若写别的数即为不符（见 8.2）。

### 8.2 站点：**时点 = 2026-09-23 15:54:15 +0800，仍在「正在发布」阶段（提交 2 未落地）**

```
$ curl -s https://xraytun.top/ | …
0.8.35 命中 = 20        0.8.34 命中 = 0        「正在发布」= 2
声明字节 47,431,435 = 0   42,919,784 = 0      45.2 MiB = 0   40.9 MiB = 0   旧值 47,243,124 = 0
pinned releases/download/v0.8.35 直链 = 0 条
```
⇒ **如实记录**：站点此刻**仍是提交 1 的状态**（「正在发布」、**没有**字节声明、**没有** pinned 直链）。
**这不是失败，也不是通过** —— **提交 2 落地后必须复验**：`PUBLISHED=True`、pinned 三条、字节与上表逐字节一致、
`45.2 / 40.9 MiB`（用上面两个 B 值自己算）、以及「0.8.34 归零」。
（`curl -sIL` 的 pinned 真实 content-length **现在无处可验**：直链还没出现 ⇒ 记「未到期」，不记通过。）

### 8.3 OG 图（单独确认）：**200 + `image/png` + 与仓库同字节** ✅

```
og-image-0.8.35.png     HTTP/2 200  content-type: image/png  content-length: 59389
  线上 sha256 = 2501310cd9d914b60305e0f88955a7cb66afc83b7b0e6dc66aa84005821304d7
  仓库(3754374) sha256 = 2501310cd9d914b60305e0f88955a7cb66afc83b7b0e6dc66aa84005821304d7   ← 一致
og-image-en-0.8.35.png  HTTP/2 200  content-type: image/png  content-length: 42634
  线上 sha256 = 70951bd9e04d0c1d4ac273f3807fff50ec22760f96b7fbdd4a023eb9fcba245d
  仓库(3754374) sha256 = 70951bd9e04d0c1d4ac273f3807fff50ec22760f96b7fbdd4a023eb9fcba245d   ← 一致
```
（这两张是 ops 提交流程里换版本号产出的新图；`d95b4ef` 之前那两张 0.8.34 的已被替换 —— 与 `task-133` §4② 的
「相似域名被抹是取舍」无关，属另一条线。）

### 8.4 `verify-live-site.sh --self-test`：**四例全部符合预期** ✅

```
self-test：四例全部符合预期（异常样例 ✗ / 缓存残留只 WARN / 真 404 正常）
  ✓ 被检对象：真 404（不存在）→ 判定码 = 0（期望 0；0=已消失 2=观察项(不阻断) 1=异常）✓ 符合预期
```
⇒ 软 404 判据仍然有效。

### 8.5 本节的诚实清单（新增）
1. **站点状态的时点必须连在一起引**：上面的 8.2 是 **15:54:15**；ops 正在做提交 2，**落地后需复验**（我已写好判据）。
2. **CF 传播延迟**我看不到：即使提交 2 落地，线上生效时间我无法从本机判定（只能重复 `curl` 看变化）。
3. **真机安装 / Gatekeeper / 重装助手仍未验证**（同 §6.1）：本次只核到「资产完好、hash 自洽、OG 图字节一致」。
4. 资产是我**真的下载后自算**的（dmg 47 MB + zip 43 MB），不是引用 `gh` 的 digest —— 但**没有**解包/挂载 dmg 做功能验证。

### 8.6 ✅ 提交 2 落地后的**复验**（时点 2026-09-23 15:55:15 +0800）—— 8.2 的「未到期」现已闭合

`35bbd2f`（提交 2：`PUBLISHED=True` + pinned + 真实资产数据 + 三个生成器重跑）落地后我重跑同一组检查：

```
0.8.35 命中 = 35      0.8.34 命中 = 0      「正在发布」 = 0
47,431,435 = 1        42,919,784 = 1       45.2 MiB = 4       40.9 MiB = 2
旧值 47,243,124 = 0   旧值 42,742,652 = 0
pinned v0.8.35 直链 = 3 条（dmg / zip / SHA256SUMS.txt，页面共出现 8 次）

$ curl -sIL <pinned dmg>
HTTP/2 302  →  HTTP/2 200   content-type: application/octet-stream   content-length: 47431435
```
* **字节与资产逐字节一致**：`content-length: 47431435` = 我在 8.1 里自己 `wc -c` 出来的 dmg 字节数；
* **`45.2 / 40.9 MiB` 的换算我自己核了**：47,431,435 / 1,048,576 = **45.23**、42,919,784 / 1,048,576 = **40.93** ⇒ 站点写的一位小数正确（取整陷阱没踩）；
* `0.8.34` 在首页**归零** ✓；`PUBLISHED` 阶段文案（「正在发布」）归零 ✓；
* ⇒ 卡面 6–8 步**全部完成**（8.2 当时只是「时点未到期」，不是失败）。

**仍未做的两件（不在本卡能否完成的范围内）**：① 真机安装/Gatekeeper/重装助手；② CF 传播延迟的独立判定（我只能看到 15:55:15 这一时点已经生效，不能证明「何时开始生效」）。
