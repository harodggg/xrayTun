# v0.8.34 发布验收（tag `v0.8.34`，急救版）

> **谁写的**：tester（**独立于实现者**）。凡「我实测」都是我在本机跑的；凡引别人的数都标了来源。
> **它为什么必须存在**：`CHANGELOG.md` 有 **三处**引用本文件（提交 1 两处 + 提交 2 一处），
> 而 tag `v0.8.34` **不可重写** ⇒ 把这份档真写出来是那些引用的**唯一解**。

## 0. 结论（一句话）

**发布成立**：门禁 exit 0、Release workflow `success`、`isDraft=false`、3 个资产三方一致（我 / ops / Lead 各自独立），
站点两阶段发布完成且线上与 `442d65d` 逐字节一致。**两个 P0 都在 tag 内。**
发布过程中发现并已修掉 **2 处文档缺陷**（见 §5.3），另有 **3 条观察项**（§7，均不阻断）。

| 项 | 值 |
|---|---|
| tag | `v0.8.34`（annotated） |
| tag 对象 | `5e73b6853c302589960893feb241f8247ad0018c` |
| **tag 指向的提交** | **`6aa3b5e7743a58afc52691e2a4394b91de0b3c94`** |
| tagger epoch | `1790058092` = **2026-09-22 14:21:32 +0800** |
| 发布提交 2 | `442d65d5a8212f060dbe837a9e56244caa3d3df8`（site/** + CHANGELOG） |
| 本地 HEAD == `origin/main` | `442d65d`（`git rev-list --left-right --count HEAD...origin/main` = `0  0`） |
| Release workflow | run `35694476730` → `completed` / `success`；`publishedAt` = `2026-09-22T06:45:31Z` |
| 门禁 | `./scripts/check.sh --no-release-build` → **exit 0**（**Lead 在冻结修订 `dd95fdb` 上跑的**；我的独立复现见 §2 —— 六套单测/前端/站点全绿，唯一红的是**一步并发 doctest**，隔离重跑即绿） |

## 1. 修订身份（artifact identity）——**每条都能自己复算**

```bash
$ git cat-file -p v0.8.34 | head -4
object 6aa3b5e7743a58afc52691e2a4394b91de0b3c94
type commit
tag v0.8.34
tagger harodggg <haroldtiansheng@gmail.com> 1790058092 +0800

$ git rev-parse 'v0.8.34^{commit}'      # → 6aa3b5e7743a58afc52691e2a4394b91de0b3c94
$ date -r 1790058092 '+%Y-%m-%d %H:%M:%S %z'   # → 2026-09-22 14:21:32 +0800
$ git log -1 --format='%cd' 6aa3b5e --date=iso  # commit 时间 14:19:51（比 tag 早 101 s）
```

**版本三处 + `Cargo.lock` 在 tag 上全部是 `0.8.34`**（我逐个 `git show v0.8.34:<路径>` 读出）：
`Cargo.toml` / `apps/desktop/tauri.conf.json` / `apps/ui/package.json` / `Cargo.lock`（`xraytun-desktop`）。

**两个 P0 确实在 tag 内**（`git merge-base --is-ancestor <c> v0.8.34` 全为真）：

| 提交 | 内容 |
|---|---|
| `41cf1ea` | 看门狗别再把**还活着**的隧道拆掉（task-98 P0） |
| `dd95fdb` | 写侧：一行两个 JSON 对象（task-104 P0 的配套修复，见 §5.3） |
| `d490935` | `scripts/net-metrics.py` + 本文档的指标基线（口径头 v2 的前身） |

## 2. 门禁：`./scripts/check.sh --no-release-build`（本机原始输出）

**运行口径（必须连这些一起看）**：

```
命令   : ./scripts/check.sh --no-release-build
起始   : 2026-09-22 14:46:19 +0800
修订   : HEAD = 6aa3b5e（== v0.8.34^{commit}）
工作区 : `git status --porcelain` = 仅 ` M CHANGELOG.md`（另一人的文档编辑，check.sh 不读 CHANGELOG）
环境   : cargo 1.98.0 / node v26.9.0 / npm 11.19.1 / python3 3.12.3
落盘   : /tmp/check-v0834-6aa3b5e.log（全文）
```

⚠️ **运行期间 ops 推了提交 2（`442d65d`，14:47:40，只改 `site/**` + `CHANGELOG.md`）**。
其中唯一与 `site/**` 耦合的一步是「站点版本一致性」——**我在最终修订 `442d65d` 上把该步逐字重跑过一遍，同样全绿**（§2.5）。
cargo 各步与 `site/**`、`CHANGELOG.md` 无关。

### 2.1 前端单元测试

```
 Test Files  21 passed (21)
      Tests  208 passed | 1 todo (209)
   Start at  14:46:20
   Duration  10.23s
```

### 2.2 TypeScript 类型检查 / 2.3 CSS token / 2.4 前端构建

```
（tsc）仅一条 npm warn：install-scripts .npmrc allow-scripts 被 --allow-scripts 覆盖 —— 非错误
（css token）应用 UI：1 个样式表定义 23 个 token；268 处 var() 引用（其中 2 处带兜底）
             官网：1 个样式表定义 41 个 token；87 处 var() 引用（其中 0 处带兜底）
             ✓ 所有无兜底的 var() 引用，都能在本 bundle 内找到定义
（构建）✓ 63 modules transformed；dist/assets/index-DbexVoHD.js 245.89 kB │ gzip: 85.09 kB
```

### 2.5 站点版本一致性（`site/**` ↔ `Cargo.toml`）

门禁里的那一步（起始修订上）：

```
  ✓ gen-site-jsonld.py XRAYTUN_VERSION 0.8.34
  ✓ gen-site-geo.py VERSION        0.8.34
  ✓ site.js PAGE_VERSION           0.8.34
  ✓ gen-site-images.py SITE_VERSION 0.8.34
  ✓ site/index.html 下载文件名 0.8.34
  ✓ site/en/index.html 下载文件名 0.8.34
  ✓ 站点声明的版本与 Cargo.toml 一致：0.8.34
```

**在最终修订 `442d65d` 上逐字重跑同样的代码**（`sed -n '121,155p' scripts/check.sh` 原样执行，未改写）：**exit 0，七行同上。**

### 2.6 clippy / `cargo test --workspace`

**clippy（`-D warnings`）**：`Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 5m 04s` —— **零 warning**
（`-D warnings` 下有一个 warning 就会失败）。

**`cargo test --workspace`** 逐套结果（本机原始输出）：

| 测试目标 | 结果 |
|---|---|
| `xraytun_desktop_lib`（`unittests src/lib.rs`） | **172 passed**; 0 failed; 1 ignored |
| `xraytun_desktop`（`unittests src/main.rs`） | 0 passed; 0 failed |
| `tests/type_contract.rs` | **6 passed**; 0 failed |
| `xt_core`（`unittests src/lib.rs`） | **226 passed**; 0 failed; 1 ignored |
| `xraytun_helper` | **13 passed**; 0 failed |
| `xt_proto` | **22 passed**; 0 failed |
| `xt_tun` | **71 passed**; 0 failed |
| **`Doc-tests xraytun_desktop_lib`** | ✗ **失败（`E0463`）** ← 见 §2.7 |
| `Doc-tests xt_core / xt_proto / xt_tun` | 未执行（上一步失败即中止） |

⇒ 整条门禁 **`EXIT=1`**：红的**只有最后那一步 rustdoc doctest**，前面每一步都是绿的。

### 2.7 ⚠️ 那一步为什么红 —— **并发构建造成的假红**，不是产品缺陷

**现象**（原始日志 `/tmp/check-v0834-6aa3b5e.log:1218-1255`）：`rustdoc` 报
`error[E0463]: can't find crate for xt_proto / tauri / xt_core / tracing_subscriber / tokio`，
而**同一条 rustdoc 命令行上确实传了** `--extern xt_proto=…rlib --extern tauri=…rlib --extern tokio=…rlib …`。

**同一时刻、同一个 `CARGO_TARGET_DIR` 上还有另一个 cargo 在编译**：

```
$ pgrep -fl cargo
64401 cargo test -p xt-core --lib store
64399 bash -c … CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target cargo test -p xt-core --lib store …
```

这正是本项目的**已知纪律**：「`scripts/check.sh` 与并发的 `cargo test` 会抢构建锁，**不要并行跑**」。

**隔离复验**（用与 `check.sh` **相同**的环境变量，只跑那一步）：

```
$ CARGO_HOME=…/.cargo CARGO_TARGET_DIR=…/.cargo-target cargo test --workspace --doc
    Finished `test` profile [unoptimized + debuginfo] target(s) in 5m 02s
   Doc-tests xraytun_desktop_lib : test result: ok. 0 passed; 0 failed
   Doc-tests xt_core             : test result: ok. 0 passed; 0 failed
   Doc-tests xt_proto            : test result: ok. 0 passed; 0 failed
   Doc-tests xt_tun              : test result: ok. 0 passed; 0 failed
EXIT=0        （2026-09-22 15:02:46）
```

**诚实边界**：复验开始时**仍有**另一个 cargo（backend-dev 的 `task-107` 灵敏度实验，同一 target dir），
我的进程先 `Blocking waiting for file lock on build directory`、**拿到锁之后独占跑完** ⇒ 它是「锁序列化后的干净跑」。
**我没有把并发竞争的精确机制证明到行级**（只能说：红/绿两次的差别只有「是否有另一个 cargo 同时在同一 target dir 上」这一项）。

### 2.8 计数差异（**可逐项对上**）：我量到 `xt-core 226`，发布说明写 `223`

发布说明的计数是 Lead 在**冻结修订 `dd95fdb`** 上跑出来的；我这次跑的工作区**带着别人未提交的改动**
（`crates/xt-core/src/store.rs`，`task-107` 进行中，`+232 / −7`）。差值**恰好是那 3 条新测试**：

```bash
$ git diff -- crates/xt-core/src/store.rs | grep -E '^\+\s+fn [a-z_]+\(\) \{'
+    fn a_line_with_two_objects_yields_both_records() {
+    fn a_malformed_line_is_counted_and_later_lines_still_read() {
+    fn tail_logs_stats_document_early_break_and_truncation() {
```

⇒ **223 + 3 = 226**；**其余六套（172 / 6 / 13 / 22 / 71）与发布说明逐项相同**。
⇒ 也正因如此：**我这次跑的不能当作「发布修订的门禁」** —— 发布修订的门禁是 Lead 那次（exit 0）。
我这次的角色是**独立复现**：clippy 干净、六套单测全绿、前端 21 文件 208 passed、站点一致性 6/6 绿，
**唯一红的是一步与并发构建有关的 rustdoc doctest，隔离后同样绿**。

### 2.9 本节的结论（**不夸大**）

1. **发布门禁成立**（Lead 在冻结修订上的 exit 0 + 我这次各项独立复现）；
2. 我这次**没有**在「干净且无人并发」的条件下从头跑完一遍 `check.sh`（共享 `CARGO_TARGET_DIR` 就是并发的现场）
   —— 这条**如实写在 §8**，不拿「重跑一步绿了」冒充「整条重跑绿了」。

## 3. 资产与完整性：**三方独立结果一致**

| 资产 | 大小（字节） | SHA256 |
|---|---|---|
| `XrayTun_0.8.34_x86_64_arm64.dmg` | **47,243,124** | `e8b82a05934cdf8034f85469ade11886eca5b4e3f6b5b900153f76fa934007df` |
| `XrayTun_0.8.34_x86_64_arm64.zip` | **42,742,652** | `d76cc4e958ce4cfc4227a9677060b59aaedbc852fa793648e898f724d1f721dc` |
| `SHA256SUMS.txt` | 200 | `b781772bf3657800fa79dd9f58fc2c08ba6b81b70ff4ab4cc6a209af282b93d3` |

**三处来源**：
1. **GitHub 自己算的**：`gh api repos/harodggg/xrayTun/releases/tags/v0.8.34 --jq '.assets[] | "\(.name) \(.size) \(.digest)"'`；
2. **ops 下载后自算**：`gh release download … && shasum -a 256 *.dmg *.zip SHA256SUMS.txt`；
3. **发布说明里贴的** `SHA256SUMS.txt` 原文（两行，与上面 dmg/zip 一致）。

**且不是旧资产被复用**：v0.8.33 的 dmg/zip 是 `47,230,254 / 42,726,199`，与本版不同。
`gh release view v0.8.34 --json isDraft,isPrerelease` → `false / false`。

## 4. 站点两阶段发布（提交 1 = 「正在发布」 → 提交 2 = 真值）

**提交 1 阶段（我在 14:38 独立只读抽查）**：
```bash
$ grep -rnoE 'https://github\.com/[^"'"'"' ]*/releases/download/[^"'"'"' ]*' site --include=*.html
（0 命中）      # 与「提交 1 不给 pinned 直链」一致
```

**提交 2 之后（`442d65d`，我在本机复核 + ops 的线上输出）**：

| 检查 | 值 | 来源 |
|---|---|---|
| `PUBLISHED`（两个生成器） | `True` / `True` | 我本机 `grep '^PUBLISHED'` |
| 声明字节 | `47,243,124` / `42,742,652` / `SHA_BYTES 200` | 我本机 `grep '^DMG_BYTES\|^ZIP_BYTES\|^SHA_BYTES'` |
| pinned 链接 | **30** 条 `releases/download/v0.8.34` | 我本机 `grep -rho … \| wc -l` |
| 「正在发布」残留 | **0** | 我本机 grep |
| 三版历史字节（6 个数）残留 | **0** | 我本机 grep |
| 线上首页声明字节 | `47,243,124` ×1、`42,742,652` ×1、`45.1 MiB` ×4、`40.8 MiB` ×2 | Lead 独立核对 |
| 线上 pinned dmg | `HTTP/2 200`，`content-length: 47243124` | Lead 独立核对 |
| 线上 ≡ 修订 | `cmp` 线上 `/index.html`、`/en/index.html`、`llms.txt`、`llms-full.txt`、`sitemap.xml` vs `442d65d:site/…` → 全部 IDENTICAL | ops |
| `verify-live-site.sh` | exit 0，「全部通过（2 条 warning）」；含 404 无 SPA 兜底、`www` 301、`releases/latest` → `v0.8.34`、镜像 404 对照 | ops（本脚本是我写的，见 §8 诚实清单） |

## 5. 发布说明里的数字：我逐条复算

### 5.1 自愈那一段（口径 = `--until 14:21:32`）

复算命令：`python3 scripts/net-metrics.py --until 14:21:32`。**窗口右端不是估的**：取自 tag 的 tagger epoch（§1）。

| 发布说明里的数 | 我复算 | 一致 |
|---|---|---|
| 窗口内 **305,742** 条 | 305742 | ✓ |
| 「已作废」**7** 次 | 7 | ✓ |
| 空档（口径A）`48694 / 2657 / 691 / 2610 / 20 / 50 / 23` s | 同 | ✓ |
| 「隧道已自动恢复」**3** 行 | 3 | ✓ |

**这三行（3 次自愈）为什么重要**：它们**全部落在「一行两个 JSON 对象」的行上** ⇒ 旧的逐行解析器
会把它们**整行丢掉**（显示成 0 行）。我用 `raw_decode` 循环独立复算：全文件 **4 行**多对象行、
会丢 **4** 个对象；3 条互不相同的自愈记录 `ts_unix` = `1790051357 / 1790054534 / 1790054858`
= **12:29:17 / 13:22:14 / 13:27:38**。口径与证据见 `docs/verification/NET-METRICS.md`。

### 5.2 缺陷在「打 tag 前后仍在触发」的直接证据

`14:16:41`、`14:19:09` 两次「已作废」就发生在打 tag（14:21:32）前后。ops 侧的只读网况记录同期：

```
[09-22 14:15:32] iface=en0 gw=192.168.0.1 dns=198.18.0.2      xray=up   | 国内=000 国外=204 [⚠️]
[09-22 14:16:58] iface=en0 gw=192.168.0.1 dns=114.114.114.114 xray=down | 国内=200 国外=000 [⚠️]
[09-22 14:18:30] iface=en0 gw=192.168.0.1 dns=198.18.0.2      xray=up   | 国内=000 国外=204 [⚠️]
[09-22 14:19:55] iface=en0 gw=192.168.0.1 dns=198.18.0.2      xray=up   | 国内=000 国外=204 [⚠️]
```
（★ 这四行是 **ops 的原始记录**（`/tmp/netmon/netmon.log:159-162`），**不是我跑的**；它与我复算的
「14:16:41 已作废 → 14:17:31 core 启动」「14:19:09 已作废 → 14:19:32 core 启动」在时间上对得上。）

### 5.3 发布过程中发现并处置的 **2 处文档缺陷**（这类问题正是本项目的常客）

| # | 缺陷 | 我的证据 | 处置 |
|---|---|---|---|
| **A** | `CHANGELOG.md` **三处**引用 `docs/verification/RELEASE-v0.8.34.md`，而该文件**不存在**（`git log --all --diff-filter=A -- <path>` 空、`find` 空） | 提交 1 的 `835ca2f` 里已有两处引用；tag 不可重写 | **本文件即其解**（由我写） |
| **B** | **同一文档、同一口径、两个数**：上半段写「已作废 **7** 次」，`已知边界` 段仍写 **5** 次（且措辞是已被取代的「全部记录口径」） | `git show 835ca2f:CHANGELOG.md \| grep -n 已作废` → 40 行「5 次」、129 行「5 次」 | ops 在**提交 2**（`442d65d`）统一为同一窗口表述；我已复核 |

## 6. 「这一版**不需要重装**特权助手」的四条依据（我复核，全部成立）

```bash
$ sed -n '/\[dependencies\]/,/^\[/p' crates/xt-helper/Cargo.toml
xt-proto / xt-tun / serde / serde_json / thiserror / tracing(+subscriber) / clap / libc   ← 无 xt-core
$ grep -rn '^xt-core' --include=Cargo.toml .
./Cargo.toml:61:xt-core = { path = "crates/xt-core" }          ← workspace 定义
./apps/desktop/Cargo.toml:19:xt-core.workspace = true
$ git diff --stat v0.8.33..v0.8.34 -- crates/
 crates/xt-core/src/net.rs | 160 ++++++++++-
 crates/xt-core/src/store.rs | 117 +++++++-
 crates/xt-core/src/xray/config.rs | 133 ++++++++++-
$ git diff --stat v0.8.33..v0.8.34 -- crates/xt-tun crates/xt-proto
（空）
```

⇒ 本版改动**全在 App 侧**（`xt-core` 由 App 使用；helper 不依赖它，且 `xt-tun`/`xt-proto` **零改动**）——
「更新 App 即生效、不必重装助手」**成立**。（对照：v0.8.33 含 `xt-tun` 改动，那时必须重装。）

## 7. 观察项（**不阻断发布**，但必须写下来）

1. **旧 OG 图仍返回 200**：`og-image-0.8.33.png` → `200 image/png 59008 B`。
   这与「新图 `og-image-0.8.34.png` 也是 200」并不矛盾 —— **CDN 对不可变路径的缓存**（`immutable`）会把旧图继续供应一段时间。
   **这是观察项，不是失败**：判定失败的判据应是「**新**图 404/字节不符」，而新图 200 且字节与生成物一致。
2. **历史产物仍在 CDN 上**：三版历史字节已从页面里清干净（0 处），但**旧的 dmg/zip 资产本身**在 GitHub Release 上本来就会保留 ——
   页面不再指向它们即为正确行为。
3. **`docs/verification/RELEASE-v0.8.34.md` 在 tag `6aa3b5e` 里并不存在**（写于其后）：
   tag 的 `CHANGELOG` 因此**在 tag 这个快照上仍是一条悬挂引用**。tag 不可重写 ⇒ 只能在这里说明：
   **该引用在 `main`（`442d65d` 之后）由本文件兑现，在 tag 快照上不成立。**

## 8. 诚实清单：**我这次没有验证的**

1. **我没有重新下载那 47 MB 的 dmg/zip**：§3 的 SHA256 来自 **GitHub API 的 digest**、**ops 的下载自算**、
   以及 `SHA256SUMS.txt` 原文，**三方一致**，但**不是我在本机算的**。
2. **真机安装 / 首次打开 / Gatekeeper / 重启后路由是否恢复**：**未验证**（本环境不具备；需用户在真机走一遍）。
3. **GFW 行为无法复现**：本机不在中国大陆，「国内直连 v6 改写」的**实际效果**只能等用户侧 after 数据。
4. **after 数字现在不存在**：v0.8.34 装上后的对照**不预填、不推测**。
5. **`verify-live-site.sh` 是 tests 侧写的脚本**（我写的），§4 里它的输出是 **ops 跑的**；
   本文件的其它站点项（`PUBLISHED`、字节常量、pinned 数、0 残留）是**我在本机独立 grep 的**。
6. **netmon 那四行是 ops 的原始记录**，我只做了「与自愈事件的时间对齐」这一项推断，**没有复跑 netmon**。
7. **`check.sh` 的 6 条站点版本断言不覆盖 `Cargo.lock`、也不覆盖 `site/{,en/}wasm/index.html`**：
   `Cargo.lock` 的 0.8.34 是我单独 `git show` 读的；wasm 两页**未逐字核**。
8. **§2 门禁跑在工作区而不是 pristine checkout**：起始修订 `6aa3b5e`，期间 (a) `site/**` 被提交 2 更新
   —— 与站点耦合的那一步我已在最终修订上逐字重跑（§2.5）；(b) 另一人的 `task-107` 未提交改动进了 `xt-core`
   —— 因此我量到 `xt-core 226` 而不是发布说明的 `223`（差值恰好 3 条新测试，§2.8）；
   (c) 并发的 `cargo test -p xt-core` 与我的门禁共用同一个 `CARGO_TARGET_DIR`，导致 doctest 一步假红（§2.7）。
   **我没有在「干净修订 + 无并发」的条件下从头跑完整条 `check.sh`** —— 这是本文件**最大的口径折中**，写在这里而不是藏起来。
9. **`publishedAt`、资产字节/SHA、线上 HTTP/`cmp` 结果** 来自 Lead 与 ops 两方（互相独立，且与 GitHub API 的 digest 一致）；
   我**自己**只复核了本机可复算的部分：修订/tag/版本三处、站点常量与 pinned 数、§5.1 的指标、§6 的四条依赖命令。
10. **§2.7 的机制没有证明到行级**：我只能证明「红的那次有另一个 cargo 在同一 target dir，绿的那次没有并发竞态」，
    不能指出是哪一个文件被替换。

## 9. 复现命令清单（照抄即可）

```bash
# 修订身份
git cat-file -p v0.8.34 | head -4
git rev-parse 'v0.8.34^{commit}'; date -r 1790058092 '+%Y-%m-%d %H:%M:%S %z'
git show v0.8.34:Cargo.toml | grep -m1 '^version'
git show v0.8.34:apps/desktop/tauri.conf.json | grep -m1 '"version"'
git show v0.8.34:apps/ui/package.json | grep -m1 '"version"'

# 门禁
./scripts/check.sh --no-release-build

# 资产
gh release view v0.8.34 --json isDraft,isPrerelease,assets
gh api repos/harodggg/xrayTun/releases/tags/v0.8.34 --jq '.assets[] | "\(.name) \(.size) \(.digest)"'

# 站点
grep -h '^PUBLISHED' scripts/gen-site-geo.py scripts/gen-site-jsonld.py
grep -h '^DMG_BYTES\|^ZIP_BYTES\|^SHA_BYTES' scripts/gen-site-geo.py
grep -rho 'releases/download/v0\.8\.34' site/ | wc -l
grep -rho '正在发布\|publishing now' site/ | wc -l

# 「不需要重装助手」的四条
sed -n '/\[dependencies\]/,/^\[/p' crates/xt-helper/Cargo.toml
grep -rn '^xt-core' --include=Cargo.toml .
git diff --stat v0.8.33..v0.8.34 -- crates/
git diff --stat v0.8.33..v0.8.34 -- crates/xt-tun crates/xt-proto

# 指标（口径头 v2；§5.1）
python3 scripts/net-metrics.py --until 14:21:32
```
