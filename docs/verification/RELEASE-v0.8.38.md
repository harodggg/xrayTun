# v0.8.38 **停发**与站点回退记录（2026-09-24）

> **结论**：v0.8.38 **没有发布**。`c37eb90`（提交 1）曾把 phase-1 状态推上 `main` 并部署到线上；
> 用户裁决「暂缓发布」后，站点与版本号已回退到 **v0.8.37 真实发布态**（回退提交 `af582bf`）。
> **没有任何 tag / Release 指向 v0.8.38**；`docs/release-notes/v0.8.38.md` 保留为**草稿**。
> 卡：`task-182`（本回退）；被停的发行卡：`task-178`。

## 1. 时间线（本地时间；每条都有哈希）

| 时刻 | 事件 | 哈希 |
|---|---|---|
| 09-23 22:24 | v0.8.37 **提交 2**（真实字节 + `PUBLISHED=True` + pinned）⇒ 当时线上的状态 | `4b20e06` |
| 09-24 14:56 | v0.8.38 **提交 1**（版本号 8 处 + `PUBLISHED=False` + 站点「正在发布」+ CHANGELOG + notes） | `c37eb90` |
| 09-24 15:17 | 另一条工作流补回 sitemap 里的 Jev 两条 URL + 加「发现入口」部署门禁 | `fc8a471` |
| 09-24 15:26 | `task-179`：A21「本机 · IP」后端判据 + A20 归属只认 tag（App 侧） | `c45a12c` |
| 09-24 15:35 | 另一条工作流：intent 桌面接线第一切片（987 行） | `e0ebe0d` |
| 09-24 15:50 | `task-181`：UI 侧接线（A21 不可信不写「本机」、A20 未验证不显示数字） | `7deedb9` |
| 09-24 ~15:4x | **用户裁决：v0.8.38 暂不发**（等 Jev / intent 工作流收口） | Lead 消息 |
| 09-24 15:58 | **本回退**：站点 + 版本号回 v0.8.37 真实发布态，Jev 条目插回 | `af582bf` |

## 2. 为什么要回退：提交 1 在线上留下了用户可见的退化

`c37eb90` 是两阶段发布的**提交 1**：它把版本号推到 `0.8.38`、把 `PUBLISHED` 置 `False`，
并按设计**撤掉全部 pinned 直链**（换成 Releases 页面链接）—— 这在新版本资产**尚未存在**时是**正确**的，
前提是几十分钟内就会打 tag、跑提交 2。**停发**让这个「正确的中间态」停在线上：

```
https://xraytun.top/ → 0.8.38 命中 20 次、「正在发布」2 次、pinned releases/download/v0.8* = 0 条
```

⇒ 站点说「v0.8.38 正在发布」，**而直接下载链接被摘掉了**、Release 又不存在：
访客拿不到任何本版资产，也回不到上一版的直链。**这是对用户可见的退化，不能停在线上。**

## 3. 回退的目标态与依据（**口径**）

**目标态 = `4b20e06`（v0.8.37 提交 2）**，即 v0.8.37 真正发布时线上的状态。

> ⚠️ 口径澄清：`task-182` 卡面写「恢复到 tag `v0.8.37` 的发布态语义」。实测 **tag `v0.8.37` → `06d8f45`（提交 1）**，
> 那棵树里 `PUBLISHED=False`、字节数为空（两阶段设计的固有形态）。**真实发布态在 tag 之后的提交 2 `4b20e06`**，
> 本回退取的是它 —— 「站点说的字节数 = Release 里真实存在的资产」才成立。

**回退后逐字节核对**（`git diff 4b20e06 -- <下列文件>` 为空）：

```
site/index.html  site/en/index.html  site/assets/site.js
site/wasm/index.html  site/en/wasm/index.html
apps/desktop/tauri.conf.json  apps/ui/package.json
site/og-image-0.8.37.png  site/og-image-en-0.8.37.png
```

## 4. 回退内容（18 个文件，`af582bf`）

* **站点页面**：`site/index.html`、`site/en/index.html`、`site/assets/site.js`、两个 `wasm/index.html`
  取自 `4b20e06`；再在两条 `index.html` 的 `site-nav` 里补回 Jev 入口
  （`<a href="jev-x-filter/">信息过滤器</a>` / `Info Filter`，与回退前 `main` 逐字一致）；
* **og 卡片**：`og-image{,-en}-0.8.37.png` 取自 `4b20e06` 的 blob；删除 `…-0.8.38.png` 两张；
* **生成器常量**：`VERSION` / `XRAYTUN_VERSION` / `SITE_VERSION = "0.8.37"`、`PUBLISHED = True`、
  `DMG_BYTES,DMG_MIB = 47,469,092 / 45.3`、`ZIP_BYTES,ZIP_MIB = 42,954,264 / 41.0`、`SHA_BYTES = 200`；
* **版本号**：`Cargo.toml`（`[workspace.package]`）、`Cargo.lock`（6 个 workspace 包：`xraytun-desktop` /
  `xt-core` / `xt-tun` / `xt-helper` / `xt-proto` / `xt-intent`）、`tauri.conf.json`、`ui/package.json` → `0.8.37`；
* **CHANGELOG**：v0.8.38 段标题改 `## 未发布（v0.8.38，等本工作流收口后发）`，并加一段「本节尚未发布」说明；
* **`docs/release-notes/v0.8.38.md`**：**保留为草稿**（内容未动）。

## 5. Jev 条目：生成器会抹掉（机制缺口）

`gen-site-geo.py` 的页面清单只有本产品 **4 页** ⇒ **每次重跑都会抹掉**另一条工作流手写的
Jev 收录（v0.8.38 提交 1 已抹过一次，他们补了两次：`7e445ca`、`fc8a471`）。

本次按卡面要求：**跑完生成器后，按当前 `main` 的内容逐字插回**（同一次提交）：

* `site/sitemap.xml`：两条 `<url>`（中/英，各含三向 hreflang）；
* `site/llms.txt`：「相关项目：信息过滤器 · Jev」分节 + 维护提示；
* `site/llms-full.txt`：头部「另一个仓库」提示、索引两条、正文「相关项目」整节。

| 文件 | 回退前（`HEAD`） | 跑生成器后 | 插回后（本提交） |
|---|---|---|---|
| `site/sitemap.xml` | 8 | **0** | **8** |
| `site/llms.txt` | 3 | **0** | **3** |
| `site/llms-full.txt` | 3 | **0** | **3** |

**判据（逐行口径，卡面 revision 3）**：`grep 'jev-x-filter/'` 的行数 + 逐行比对，三份**全部为空 diff**：

```
$ diff <(git show HEAD:site/llms.txt | grep 'jev-x-filter/') <(grep 'jev-x-filter/' site/llms.txt)   → 空
（sitemap.xml 8 行、llms.txt 3 行、llms-full.txt 3 行，三份 diff 均空）
```

> **口径差异（记一笔）**：还有一种 `grep -c 'jev-x-filter'`（**不带斜杠**）会数到 `8 / 4 / 4`
> —— 多出来的是不该带斜杠的那几行（例如 llms.txt 里的仓库地址 `.../jev-x-filter`）。
> 卡面上一版把 `8 / 4 / 4` 当成目标，是**口径不同**，不是数字错：两种口径下
> 本提交的结果都**与 `HEAD` 逐项相同**（8/3/3 与 8/4/4）。

（**部署门禁**用 `grep -q 'jev-x-filter/'`，三文件分别 8 / 3 / 3，**全部非 0** ⇒ 通过。）

**根治**：`task-180`（让 `gen-site-geo.py` 收录 Jev 页，或保留手写块）。

## 6. 验收（本地，落地前实测原文）

```
$ for f in site/sitemap.xml site/llms.txt site/llms-full.txt; do
    diff <(git show HEAD:$f | grep 'jev-x-filter/') <(grep 'jev-x-filter/' $f)
  done
（三份均无输出 ⇒ 逐行相同；行数 8 / 3 / 3）

$ grep -rc '0\.8\.38' site | awk -F: '{s+=$2} END{print s+0}'          → 0
$ grep -rc '正在发布' site | awk -F: '{s+=$2} END{print s+0}'           → 0
$ grep -ro 'releases/download/v0\.8\.37' site | wc -l                   → 30

site/index.html       → dmg 47,469,092 字节（45.3 MiB）/ zip 42,954,264 字节（41.0 MiB）/ 校验和 200 字节
```

**复刻 `check.sh` 的「站点版本一致性」6 条断言**（`want = Cargo.toml [workspace.package] version = 0.8.37`）：

```
scripts/gen-site-jsonld.py      XRAYTUN_VERSION  0.8.37  ✓
scripts/gen-site-geo.py         VERSION          0.8.37  ✓
site/assets/site.js             PAGE_VERSION     0.8.37  ✓
scripts/gen-site-images.py      SITE_VERSION     0.8.37  ✓
site/index.html                 下载文件名        0.8.37  ✓
site/en/index.html              下载文件名        0.8.37  ✓
```

```
$ python3 scripts/gen-site-jsonld.py gen && python3 scripts/gen-site-jsonld.py check
✓ 6 个页面（index / en / wasm / en-wasm / jev-x-filter / en-jev-x-filter）JSON.parse ok、
  FAQPage 逐条一致、canonical/hreflang ok   → 结论: 全部通过
```

**未在本地做的**：`check.sh` 全量（按 Lead 指示不在主树跑，由 Lead 在隔离 worktree 上跑）；
线上 `curl` 验收（等部署完成后由 `docs/verification/LIVE-SITE-CHECK.md` 的口径做）。

## 7. 待办（给下一次真发 v0.8.38 时）

1. **`docs/release-notes/v0.8.38.md` 已过时**：它写于 `c37eb90`，此后 `main` 又落了
   `task-179`（后端 A20/A21，`c45a12c`）与 `task-181`（UI 接线，`7deedb9`）。
   真发版前**必须先更新 CHANGELOG 段与 notes**（否则「发布说明 vs 出厂内容」不一致 ——
   这是本项目反复出问题的地方）；
2. 站点回 `0.8.37` 后**版本号 8 处 + `Cargo.lock` 已一起回**，下次发版要走完整的两阶段
   （`bump-release.py phase1` → 门禁 → tag → `phase2`）；
3. `task-180`：生成器抹掉 Jev 的根治；
4. `docs/verification/UNSIGNED-COMMITS.md` 的会话计数（当时 6 个）已过期：本次 `af582bf` 也是未签名提交。

## 8. 诚实清单

* **站点日期回到了 v0.8.37 的 2026-09-23**（`index.html` 页脚「页面最后更新」与 `sitemap.xml` 的 `lastmod`）。
  本次回退动作发生在 09-24，但站点描述的是 **v0.8.37 的发布事实**（内容逐字节等于 `4b20e06`），
  因此未把日期改成 09-24 —— 这是**有意选择**，不是遗漏；
* `site/llms.txt` 的维护提示里原有「（`v0.8.38` 那次就删掉过本节）」一处**版本号被改写**为
  「（最近一次发版提交就删掉过本节）」：为了让 `site/**` 里 `0.8.38` 命中为 **0**（卡面验收条款），
  语义不变，是**对另一条工作流手写文字的一处最小改写**。
  该行**不含** `jev-x-filter/`，因此不影响卡面 revision 3 的逐行不变量（那份 diff 为空是实测的）；
* og 卡片是**从 `4b20e06` 取的已发布 blob**，不是本次重跑 `gen-site-images.py` 的产物
  （避免 Pillow 版本差异引入无谓的字节变化）；`site/index.html` / `site/en/index.html` 的
  `og:image` 引用与文件名一致；
* **tag 一个都没打**：`v0.8.38` 本地与远端都不存在（`git tag -l v0.8.38` = 0）；
* 线上是否已经回到 v0.8.37 取决于部署（Cloudflare Pages / GH Pages），**本文档不预填线上结论**；
* 本环境不在中国大陆、也没有真机安装流程，站点/资产相关的真机行为未验证；
* 本条回退**只动 `site/**` + 版本号 + CHANGELOG + 本文件**；另一条工作流的
  `crates/xt-intent/**`、`apps/desktop/**`、`apps/ui/**` 一行未碰。

## 9. 线上复核（**由 Lead 独立做，不采信实现者自测**）

推送后部署：

```
Pages            35972954562  91f3d7a  completed success
Cloudflare Pages 35972954412  91f3d7a  completed success
```

用**仓库自己的**线上验收脚本（不是临时 curl；它的判据是 content-type + body sha256 双判，
专门防「apex 的 SPA 兜底把不存在的东西报成 200」这个假绿）：

```
$ VER=0.8.37 PREV=0.8.36 ./scripts/verify-live-site.sh
...
总结：全部通过（2 条 warning）
EXIT=0
```

原始数字（`https://xraytun.top/`，44036 bytes，sha256 `68b1ed00ee56805216a3f40a1e79ba1fd0fa0403690d9c3796a6d2259723251f`）：

| 观察 | 值 |
|---|---|
| 站点版本字面 | `0.8.37` × **35**、`0.8.38` × **0**、`0.8.36` × 0 |
| 「正在发布」 | **0** |
| pinned 直链 | `releases/download/v0.8.37` × **8**（首页）／`site/**` 全量 **30** |
| 真实字节数 | `47,469,092` 与 `42,954,264` 字面量在场 |
| `releases/latest` 最终跳向 | `.../tag/v0.8.37`（200） |
| 随机不存在路径 | **404**（本次未观察到 SPA 兜底；对照 GitHub Pages 镜像同样 404） |
| Jev 侧 | `/jev-x-filter/` 200 · `/en/jev-x-filter/` 200 · `sitemap.xml` `jev-x-filter/` **8** · `llms.txt` **3** · `llms-full.txt` **3** |

2 条 warning 都是**观察项、不阻断**：v0.8.36 的中文/英文 OG 图（`59013 B` / `42294 B`）已从仓库删除，
但仍留在边缘缓存（immutable）里 —— 这是缓存残留的**合法旧资产**，不是「软 404」。要清需在 CF 侧 purge。

**口径更正（Lead 自己的错，记在这里）**：本文件 §6 的逐行不变量用 `grep 'jev-x-filter/'`（**带斜杠**）⇒ `8 / 3 / 3`；
而任务卡上写的 `8 / 4 / 4` 用的是**不带斜杠**的口径。两者**都与 `HEAD` 逐项相同**，
但当时把 `8 / 4 / 4` 当成唯一目标，属于**把一种口径的计数写成了通用判据** ——
已改为「与 `HEAD` 逐行 diff 为空」，这条不依赖任何计数口径。

## 10. 停发期间 `main` 上落地的进展与「可发布状态」（Lead 汇总，2026-09-24）

### 10.1 门禁证据（Lead 在隔离 worktree 上跑，命令都是 `BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build`）

| 时点（`main`） | 结果 | 这一步新覆盖了什么 |
|---|---|---|
| `fc8a471` | `GATE_EXIT=0` | 站点修复 + Jev 收录回补 |
| `c45a12c` | `GATE_EXIT=0` | A20/A21 后端字段（`task-179`） |
| `c61c311` | `GATE_EXIT=0` | 回退态 + `task-184`/`task-185`；**首次含 `gen-site-geo.py check`** |
| `0568a3a` | `GATE_EXIT=0` | `task-187`（`wt.sh` 锚主工作区）；自测案子 [6] 在门禁里真跑 |
| **`2f23d22`** | **`GATE_EXIT=0`** | **`task-183`（A10 修正）** + 并行工作流的 `real_core_dataplane`（2 条真核心数据面断言） |

`2f23d22` 的原始数字（15 个测试二进制，`FAILED` 字样 **0** 次）：

```
xraytun-desktop 259 passed / 0 failed / 5 ignored     type_contract 6/0
xt-core         254 / 0 / 2                           rule_tag_uniqueness 3 / 0
xt-helper        17 / 0                               xt-intent 139 / 0
real_core         5 / 0                               real_core_dataplane 2 / 0
xt-proto         22 / 0                               xt-tun  79 / 0
doc-tests         全 0 failed
✓ 与 CI 相同的全部检查通过
```

其中 `task-183` 的新回归都在 `2f23d22` 上真绿，例如
`availability_without_socket_but_installed_reports_not_running`、
`availability_without_socket_and_artifacts_missing_reports_not_installed`、
`availability_without_socket_and_unreadable_artifacts_reports_unknown`、
`humanize_not_running_is_true_for_both_causes`、
`commands::diagnostics::…::helper_diagnostics_line_reports_state_and_both_disk_facts`。

### 10.2 这段时间真落地的（都在 `origin/main`）

* **A20/A21 真话**（`task-179` + `task-181`）→ 独立验证 `task-184`：UI 探针 8/8、Rust 执行级断言、
  **四条突变全部变红**（两条 UI + 两条 Rust），跨语言字段一致性从「读码」升级为**执行级断言**；
* **生成器不再抹掉别人的条目**（`task-180`）：归属保留（sitemap/llms.txt）+ 标记区间（llms-full.txt）+
  **保不住就非零拒绝** + `check` 模式接进门禁；自测 **19/0 → 21/0**（含版本 bump 不误拦、两种 grep 口径一起打印）；
* **A10 修正**（`task-183`，详见 `docs/design/task-120-statement-audit.md` §五）：
  `!socket_present` **不等于**「没装」（socket 由守护进程启动时 bind、退出时删）⇒ 判据换成**安装产物**三分支
  （`NotRunning` / `NotInstalled` / `Unknown`），并一次改掉四处同源假话（`availability` / `humanize(NotRunning)` /
  启动日志 / 诊断文本）；实现者 6 条突变 + 验证者 3 条突变全红；
* **工具链**（`task-185` + `task-187`）：`wt.sh` 默认位置从**会被系统清理**的 `${TMPDIR}` 锚到主工作区
  （事故实况见 §9/`docs/verification/WORKTREE-TARGET-DIR.md`），并修掉「在 worktree 里运行会解析成 `.wt/.wt`」；
  位置守卫自测**接进 `check.sh`**（`pass=16/0`，含嵌套断言与反向敏感性）；
* **并行工作流**（同作者的另一条线）：Jev / 素颜镜项目页、`crates/xt-intent` 引擎与
  `real_core_dataplane` 真实核心验收 —— **不属于本产品的用户可见改动**，不要写进本版 CHANGELOG。

### 10.3 真发 `v0.8.38` 之前必须做的

1. **`docs/release-notes/v0.8.38.md` 与 `CHANGELOG.md` 的 v0.8.38 段已过时**（写于 `c37eb90`，
   此后 A20/A21/A10、生成器机制、`wt.sh` 都不在其中）⇒ 先按**实际出厂内容**重写，再走两阶段；
2. 在**当时的 tip** 上重跑一次门禁（上表每个时点都是独立凭据，别拿旧时点当现在还成立）；
3. 「**是否需要重装助手**」这条要按当时 tip 重新核一次
   （`git diff v0.8.37..<冻结> -- crates/xt-helper crates/xt-tun crates/xt-proto`）：
   本次 `task-183` 只改 `apps/desktop/**`（已复核：`crates/**` 零改动）；但并行工作流在动 `crates/xt-core`、`crates/xt-intent`。

### 10.4 诚实清单（本节）

* 门禁跑的**都是已经过去的 tip**（`main` 上的并行工作流提交很密集）；请把上表当**时点凭据**，不是「此刻 HEAD 一定绿」；
* 任务板上仍有更早的卡显示 `in_progress`（`task-136` / `task-168` / `task-172` / `task-174` / `task-178` 等），
  **本节没有核对它们的真实完成情况** —— 它们是否已完成、是否需要关掉，得单独过一遍；
* 仍未做真机验证的：`task-172`（scoped 默认路由假设，需用户在场）、`task-183` 的两条真实 helper 路径
  （全新机器 / 装了但没跑）、`task-184` 的真机 WKWebView；
* `v0.8.38` **tag 依然不存在**（本地与远端都没有），本文件描述的是一份**停发记录 + 可发布状态**，不是已发布事实。
