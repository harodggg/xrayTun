# v0.8.35 发布验收（两阶段；**本版需要重装特权助手**）

> **状态：提交 1 阶段（发布进行中）** —— 本文件在提交 1 里就写出来，是因为上一版
> 出过一次「`CHANGELOG.md` 引用了本文件、而本文件不存在」（`RELEASE-v0.8.34.md` 的
> **发现 A**：tag 不可重写 ⇒ 那是唯一解）。先落档再做后续，避免同一个坑第二次。
> tag 之后的内容（tag/release 资产/pinned/线上验收/`--until` 口径的 net-metrics 基线）
> 在**提交 2** 里补齐，每项都会标明来源与命令。

## 0. 本篇的口径与限制

* 所有数字都是**时点值**（日志与站点内容都在变），每条都带命令或原始输出；
* 「我实测」= ops（本机）跑的；「Lead 实测」= Lead 跑的；不把两者混起来；
* 未验证的部分写在 §8 的诚实清单里，**不拿推断当实测**。

## 1. 两阶段与冻结候选

| 项 | 值 |
|---|---|
| 冻结候选（**Lead 跑门禁那一版**） | `3c9896a3a91005264c1eeaae2a0e92c38bc7a0d4`（门禁 13:55–14:03，见 §5） |
| **提交 1 的父提交**（HEAD 在我工作中**移动过**，见 §1.1） | `4397904c4c116d6bb8ef908c7502addbadf1b3fa` |
| **提交 1** | `181f9ca52f9b86ff41d4deb8b06b30ea31d0264e`（已推，`local = origin/main`） |
| 冻结窗口内后续落地（Lead 授权，见 §1.2） | `0a5f9965457c531d1e583be3b5417a578b1fa052`（`task-124` delta-3） |
| **tag 目标**（= 措辞同步提交） | `3754374d6cf8cccb9e53911abf7cd5078cb0432f` |
| **annotated tag `v0.8.35`** | tag 对象 `7c7bb2800def07f32284485c305ad79afce242a0`，tagger epoch **1790146088**（2026-09-23 14:48:08 +0800） |
| `git rev-list --left-right --count origin/main...HEAD`（提交 1 前） | `0 0` |
| `v0.8.34` 设计上的上一版 tag | `v0.8.34`（`tag v0.8.34^{commit}` = `6aa3b5e`） |
| `git rev-list --count v0.8.34..HEAD` | **63**（时点 2026-09-23 14:07，HEAD=3c9896a）→ **64**（含 4397904）→ **65**（含 0a5f996） |
| 工作树（提交 1 前） | 干净（`git status --short` 空） |

> 卡面写「未发布提交数 38」是**建卡时的时点值**；`task-124/121/128/130/127` 落地后
> 实测为 **63**（含上一版 tag 之后的「提交 2」`442d65d` —— 它不在 `v0.8.34` 里）。
>
> **tag 将打在「tag 前最后一个提交」上**（= 本文件的措辞同步提交，父提交 `0a5f996`），
> 而不是提交 1 本身 —— 因为 delta-3 是 P0 隐私修复，**必须进这个版本**。

### 1.2 冻结窗口内落地的 delta-3（**Lead 授权、单一写者**）

`task-124` 的第三种泄漏形态（**域名紧贴字母** `Xray<域名>`）由 tester 在 `4397904` 里发现，
由 backend-dev 在 **`0a5f996`** 修掉：`match_at` 的**左边界要求整个去掉**（右边仍挡字母/数字），
新增 `domain_glued_to_letters_is_redacted`（两种来源）与
`node_domain_is_redacted_inside_longer_hostnames_too`。

**选择是「修代码」而不是「把界面改成三种」**，理由与代价一并记录：
`xnode-example.xyz`（只是**包含**节点域名的另一个域名）现在也会被抹 ⇒
原本「别误伤相似域名」的断言**主动反过来**，**宁可多抹一个相似域名，也不漏一个真实服务器地址**。
修完「覆盖不到的形态」**仍是两类**（base64 载荷 / 不以 `HOME` 开头但带用户名的路径），
`Logs.tsx` 那句陈述**一个字符未动** ⇒ CHANGELOG 里同口径的那句**不需要改**（已逐字核对）。

### 1.3 tag（**已在门禁绿之后打**）

```
$ git cat-file -p v0.8.35 | head -5
object 3754374d6cf8cccb9e53911abf7cd5078cb0432f      ← tag 目标 == 门禁那一版
type commit
tag v0.8.35
tagger harodggg <haroldtiansheng@gmail.com> 1790146088 +0800

$ git rev-parse 'v0.8.35^{commit}'
3754374d6cf8cccb9e53911abf7cd5078cb0432f

$ git ls-remote --tags origin v0.8.35
7c7bb2800def07f32284485c305ad79afce242a0	refs/tags/v0.8.35
```

tag 注解（`git tag -l --format='%(contents)' v0.8.35` 可读全文）里写明：
**本版需要重装特权助手**（附 `-- crates/xt-helper crates/xt-tun` 的实测 stat）、四组用户可见改动、
以及门禁那一行的实测（§5）。打 tag 前确认**无并发 cargo**（`pgrep -fl "cargo|rustc"`）。

**注意**：`release.yml` 生成的 **Release 正文来自 CI 里的静态 `NOTES.md` 模板**，
**不含**版本特有的用户动作（实测 `v0.8.34` 的 body 逐字就是模板，1669 字符）⇒
本版按 Lead 裁决 (b) **发布后手工 `gh release edit` 插一段**，口径见 §9。

### 1.1 ⚠️ 发布过程中 HEAD 移动了，并因此产生一次**线上回归**（已实测）

**事实链（全部带时点）**：

1. 14:07:26 —— 我取改前基线，`HEAD = 3c9896a`（§2 的表就是这一版的树）；
2. 14:09:06 —— **`4397904`**（tester 的 `task-145` 第二轮验证文档）落在 `3c9896a` 之上并被推上
   `origin/main`；**HEAD 因此在发布进行中移动**。它相对 `3c9896a` 的全部差异只有三处：
   ```
   A  docs/verification/TASK-124-PRIVACY-VERIFY-R2.md
   D  site/og-image-0.8.34.png
   D  site/og-image-en-0.8.34.png
   ```
3. 那两条 `D` **是我 `git rm` 暂存的删除**（旧版本的 og 卡片，清理意图是对的），
   被对方**一次未按路径限定的 `git commit` 连带提交**进了它的文档提交 ——
   **切引用（在我的提交 1 里）与删旧卡片（被提前提交）被拆到了两个提交**；
4. 该 push 触及 `site/**` ⇒ 站点部署被触发；**实测（`curl`，时点 2026-09-23 14:10 +0800）**：
   ```
   https://xraytun.top/og-image-0.8.34.png      → HTTP 404
   https://xraytun.top/og-image-en-0.8.34.png   → HTTP 404
   https://xraytun.top/                        → HTTP 200，43980 字节（= 改版前的 index.html）
   其 og:image / twitter:image = https://xraytun.top/og-image-0.8.34.png   ← 指向已 404 的文件
   ```
   ⇒ **线上社交卡片图在那一刻起是坏的**（分享预览拿不到图）；HTML 与其它资产均正常。

**处置**：提交 1 把引用切到 `og-image-0.8.35.png` 并**同时**放入新图 ⇒ push 后部署即恢复一致
（提交 2 会补上部署后的 `curl` 复核：新图 200、旧引用消失）。

**两条机制教训（写下来，不靠下次注意）**：

* **切引用与删旧产物必须同一个提交**：任何「先把旧产物删掉、随后再切引用」的窗口
  都会在窗口内让线上指向 404 —— 这次窗口只有几分钟，但**确实发生了**；
* **共享工作区里暂存 ≠ 安全**：`git rm`（或任何 `git add`）之后**不要停在「已暂存、未提交」**的状态上，
  否则别人的一次不限路径 `git commit` 会把你的暂存一起交出去（本条正是这么发生的）。
  ⇒ 我这次的教训是：**先算完再一次性 add+commit 同一个提交**，删除也放在那一次里。

## 2. 改前基线（**提交 1 之前**的工作树）

**口径**：仓库 `HEAD = 3c9896a` 的工作树；`size` 由 `stat -f %z` 读出（**字节**）；
`mtime` 由 `stat -f '%Sm' -t '%Y-%m-%d %H:%M:%S'` 读出；**时点 = 2026-09-23 14:07:26 +0800**。

| 文件 | 字节 | mtime |
|---|---|---|
| `Cargo.toml` | 1570 | 2026-09-22 14:18:07 |
| `Cargo.lock` | 119448 | 2026-09-22 14:18:07 |
| `apps/desktop/tauri.conf.json` | 1816 | 2026-09-23 12:38:39 |
| `apps/ui/package.json` | 753 | 2026-09-22 14:18:07 |
| `scripts/gen-site-jsonld.py` | 18723 | 2026-09-22 14:46:55 |
| `scripts/gen-site-geo.py` | 23345 | 2026-09-22 14:46:55 |
| `scripts/gen-site-images.py` | 11933 | 2026-09-22 14:18:07 |
| `site/assets/site.js` | 4878 | 2026-09-22 14:18:07 |
| `site/index.html` | 43980 | 2026-09-22 14:46:55 |
| `site/en/index.html` | 45083 | 2026-09-22 14:46:55 |
| `site/wasm/index.html` | 25471 | 2026-09-22 14:46:55 |
| `site/en/wasm/index.html` | 27200 | 2026-09-22 14:46:55 |
| `site/llms.txt` | 5026 | 2026-09-22 14:46:56 |
| `site/llms-full.txt` | 63437 | 2026-09-22 14:46:56 |
| `CHANGELOG.md` | 80880 | 2026-09-22 14:47:23 |

已发布版本的 og 卡片（**提交 1 里删掉，删除前先证明引用为 0**，见 §4）：

```
58997  site/og-image-0.8.34.png
42267  site/og-image-en-0.8.34.png
61414  site/og-image-wasm-0.7.0.png      （wasm 序列，未动）
49195  site/og-image-wasm-en-0.7.0.png   （wasm 序列，未动）
```

磁盘（**原始行**，`df -k /Users/xbtg-`，时点同上）：

```
/dev/disk3s5   482797652 431915056  16735044    97% 4920255 167350440    3%   /System/Volumes/Data
```

**口径更正（重要）**：该看的是 **Data 卷**这一行的 `avail`；`df -h /` 给的是**只读系统卷**
（`45%`），`/System/Volumes/Update/SFR/mnt1` 又是另一个 `44%` —— 卡面早先引用的
「16 GiB（44%）」正是后两者之一，**会低估风险**。本文件一律以 `df -k /Users/xbtg-` 为准。

## 3. 版本号 8 处 + `Cargo.lock`（提交 1 实测）

替换脚本：`/tmp/bump-0835.py`（**先全部校验、再统一落盘**：42 条 pattern 每条都要
命中预期次数，任一条不符就整体退出、**一个字都不落盘**）。

⚠️ **机制自己先被干跑抓到一次**：脚本第一版把「组 C（已发布 → 正在发布）」的 pattern 也拿
**原始文本**校验，而它们依赖组 A/B 先生效 ⇒ 干跑报 8 条「命中 0」，**未落盘**。
改成**顺序模拟校验**后 42 条全过。这正是上一版写下这条机制的原因（v0.8.33 出过
「页面显示 0.8.33 文件名 + 0.8.32 字节数」的中间态，根因是管道吞掉退出码）。

| 位置 | 改后值 | 实测命令 |
|---|---|---|
| `Cargo.toml:12` `[workspace.package]` | `0.8.35` | `grep -n '0\.8\.35' Cargo.toml` |
| `apps/desktop/tauri.conf.json:4` | `0.8.35` | 同上 |
| `apps/ui/package.json:4` | `0.8.35` | 同上 |
| `scripts/gen-site-jsonld.py:37` `XRAYTUN_VERSION` | `0.8.35` | 同上 |
| `scripts/gen-site-geo.py:22` `VERSION` | `0.8.35` | 同上 |
| `scripts/gen-site-images.py:49` `SITE_VERSION`（+ 两张卡 chips） | `0.8.35` | 同上（chips 2 处） |
| `site/assets/site.js:25` `PAGE_VERSION` | `0.8.35` | 同上 |
| `site/{,en/}index.html` 下载文件名 | `XrayTun_0.8.35_x86_64_arm64.dmg` | `grep -o 'XrayTun_[0-9.]*_x86_64_arm64\.dmg'` |
| `Cargo.lock` | `0.8.35` ×5（`xraytun-desktop`/`xt-core`/`xt-helper`/`xt-proto`/`xt-tun`） | `grep -B1 'version = "0.8.35"' Cargo.lock` |

**手工核（`check.sh` 的 6 条站点断言不覆盖）**：`site/wasm/index.html:217`
与 `site/en/wasm/index.html:224` 的 `<strong>v0.8.35</strong>`。

## 4. 站点 = 「正在发布」态（此刻资产还不存在）

三条硬判据（**都为 0 才是对的**）：

| 判据 | 实测（提交 1 工作树） |
|---|---|
| `site/**` 里 pinned 直链 `releases/download/v0.8.35` | **0**（此刻必然 404 ⇒ 不给） |
| 上一版**真实**字节数 `47,243,124` / `42,742,652` | **0**（留着 = 站点带着错数字上线） |
| `site/**` 里 `0.8.34` 残留 | **0** |

页面上的实际文案（节选，逐条可 grep）：

```
>下载 macOS 版 v0.8.35 · dmg（大小见 Releases）
>备用 .zip（大小见 Releases）
v0.8.35 正在发布 · 仅 macOS · 通用包（arm64 + x86_64）· 大小以 Releases 页面为准
真实资产（v0.8.35）：**正在发布**，资产发布后本表填入**真实字节数**
site/llms.txt: - **v0.8.35 正在发布**：资产发布后在本页给出直链与**真实字节数**；
```

结构化数据（`PUBLISHED = False`）：`softwareVersion = 0.8.35`，
`downloadUrl`/`installUrl` = `https://github.com/harodggg/xrayTun/releases`（**不是** pinned）。

**og 卡片**：新生成 `site/og-image-0.8.35.png`（59,389 B）、`site/og-image-en-0.8.35.png`（42,634 B）。
旧的两张 `og-image{,-en}-0.8.34.png` **已在 `4397904` 里删除**（删除前我证明过「在本版工作树里引用为
`grep -rn 'og-image-0\.8\.34' site | wc -l` = **0**」）—— 但那次删除与引用切换**不在同一个提交**，
后果见 §1.1（线上图片 404 窗口），本提交把它补齐。

**生成器重跑（全部 exit 0）**：

```
python3 scripts/gen-site-jsonld.py gen     → 4 个页面写入；随后 check 全部通过
python3 scripts/gen-site-geo.py            → robots.txt / sitemap.xml / llms.txt / llms-full.txt
/usr/local/bin/python3.10 scripts/gen-site-images.py → 8 个产物（含新 og）
```

**站点部署静态检查本地复刻**（`pages.yml` 与 `cloudflare-pages.yml` 里那几段，逐字跑）：
必需文件 8/8 存在、无根绝对路径（`href|src="/…"`）、CSS 无根绝对 `url(/)`、
`en/index.html` 资源带 `../` ⇒ **fail=0**。

## 5. 门禁

**Lead 实测（tag 目标 `3754374`，**就是本版发版的那一版**）**：
`BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build` ⇒ **`GATE_EXIT=0`**，
窗口 **14:46:34–14:47:02**（构建锁持有 **28s**；增量，代码未变）。原始日志
`/tmp/lead-gate-v0835-tag.log` 里的关键行：

```
  🔒 已获取构建锁：pid=15850 命令=scripts/check.sh --no-release-build 开始=2026-09-23 14:46:34 +0800
1870: Test Files  28 passed (28)
1871:      Tests  296 passed | 1 todo (297)
2562:✓ 与 CI 相同的全部检查通过
2563:  🔓 已释放构建锁：pid=15850 持有 28s
```
**`Errors` 行计数 = 0**（`grep -c 'Errors' /tmp/lead-gate-v0835-tag.log` → `0`）。

**另有第一次门禁（`0a5f996`，同一批代码、但没有我那笔文档同步）**：`GATE_EXIT=0`，
窗口 14:26:37–14:46:24，构建锁 **1187s**。两次都绿；`0a5f996 → 3754374` 的树差是**纯文档两文件**
（Lead 已 `git diff --name-status` 核过，并确认 `check.sh` 不读这两个文件）——
但**放行依据是第二次那次在 tag 目标上跑的结果**，不是推论。

⚠️ **看前端那一步的口径**：`Test Files 28 passed / Tests 296 passed` **不等于绿** ——
曾经**同样的计数**那一次同时打了 `Errors 1 error`（unhandled error），`GATE_EXIT=1`。
**计数相同、结论相反** ⇒ **必须同时看 `Errors` 行与退出码。**

**各卡实测（不是同一时点，`0a5f996` 那次）**：`cargo test --workspace` `TEST_EXIT=0`
（desktop **217** / type_contract 6 / xt-core 227 / helper 14 / xt_proto 22 / xt_tun 72）；
`clippy --workspace --all-targets -D warnings` exit 0；`npx tsc --noEmit` exit 0；
`npx vitest run` **28 files / 296 passed + 1 todo，退出码 0、无 `Errors` 行**。

## 6. 本版特有的核对项：`.app` 里真的有三个脚本（**产物级**）

Tauri resources 现在应含三个脚本（`incident-bundle.sh` / `triage-incident.py` / `net-metrics.py`），
预期落在 `.app/Contents/Resources/scripts/`（`tauri.conf.json` 里的映射已核：
`"../../scripts/<name>": "scripts/<name>"` ×3）。口径（Lead 已采纳）：

1. **看产物不看源码**：**证据取自发布资产 zip 里的 `.app`**（用户拿到的就是它；
   CI 在 `$CARGO_TARGET_DIR/universal-apple-darwin/release/bundle/macos/XrayTun.app` 打包，
   本机不做本地 `tauri build` —— 既省磁盘，也比中间产物更贴近事实）。
   解包用 `ditto -x -k`（保签名/扩展属性），**不比对大小，只比 `shasum -a 256`**；
2. `codesign --verify --strict` 通过（新增 resource 会改变被签名内容，包内未声明文件会被判「已损坏」）；
3. **反向断言**：故意把校验路径写错 ⇒ 必须**报红**，否则这条自检等于没有；
4. 另外断言 `tauri.conf.json` 声明的 `scripts/*` **恰好三个**（同时防「声明了没进包」与「进了包没声明」）。

**工具**：`/tmp/verify-app-bundle-resources.sh`（本次先写在 `/tmp`，避免往冻结窗口叠东西；
计划在**提交 2** 落到 `docs/verification/`）。`--self-test`（离线假 bundle）实测 **`pass=4 fail=0`**：
T1 内容一致⇒绿、T2 内容不符⇒红、T3 缺文件⇒红、T4 **路径写错**⇒红。

> ⚠️ **工具自己的诚实一条**：这个自测**第一版是假的** —— T1 把三个脚本**全**拷进假 bundle，
> 于是 T3「缺文件」其实文件还在 ⇒ 那条自测**恒绿**（工具自己的假信号，同族）。
> 修法是 T3 先 `rm -f`，之后 4/0。**它还没在真产物上跑过** —— 现在只是「口径已定 + 自测过」。

> 状态：**已在发布资产上跑过（见 §11）** —— 含真产物的 5/0 与负对照 2/2。

## 7. 清单（**全部完成**）

- [x] 提交 1 的完整 hash = `181f9ca52f9b86ff41d4deb8b06b30ea31d0264e`
- [x] 门禁在 **tag 目标 `3754374`** 上：`GATE_EXIT=0`，14:46:34–14:47:02，`Tests 296 passed | 1 todo`、
      **`Errors` 行 0 条**（§5；另有 `0a5f996` 与 `3c9896a` 两次背景记录）
- [x] annotated tag `v0.8.35`：tag 对象 `7c7bb28…` → commit `3754374`，tagger epoch `1790146088`（§1.3）
- [x] Release workflow：run `35828488568` **success**，`isDraft=false`、`isPrerelease=false`、
      **3 个资产**（§10，字节数与 SHA256 三方一致）
- [x] §11 `.app` 产物级核对（证据 = 发布资产 zip）：声明 3 个脚本 + 三份 sha256 一致 + `codesign --verify --strict` ⇒ **5/0**，
      负对照 **2/2（红）**
- [x] 提交 2 = `35bbd2fbd5b9053cb09e7309aec5664ec6b29a06`：`PUBLISHED=True` + 真实字节数 + pinned + 生成器重跑
- [x] `VER=0.8.35 PREV=0.8.34 ./scripts/verify-live-site.sh` ⇒ **全部通过（0 条 warning）**，退出码 0；
      `--self-test` ⇒ 四例全部符合预期（§12）
- [x] `net-metrics.py --until "2026-09-23 14:48:08"`（= tagger epoch）基线（§13，带口径与时点）
- [x] 线上复核：og 图 200 且与仓库同字节、`0.8.34` 残留 0、pinned 30 条、`正在发布` 0、`cmp` 与仓库逐字节相同（§12）
- [x] Release Notes 补「本版需要重新安装特权助手」（**手工，§9**；机制 = `task-147`）

## 8. 诚实清单（**发布后仍然成立的部分**）

* **真机安装 / 重启 / Gatekeeper / 重装特权助手未在本环境验证** —— 本环境不具备
  「下载包 → 替换 → 打开」的完整链路；**Release Notes 里那句「需要重装助手」的依据是
  helper 侧源码 diff + 依赖关系（`xt-helper` 依赖 `xt-tun`），不是真机实测**；
* 本环境**不在中国大陆**，GFW 行为无法复现；
* 「用户装上新版之后的 after 数字」**现在不存在**，本篇不预填、不推测（§13 全是**本次发版前**的基线）；
* §2 的磁盘数字是**时点值**：发版过程中它一直在变（最低到过 ~12.8 GiB）；
* §5 有**三次**门禁记录，别混：`3c9896a`（13:55–14:03，**版本 bump 之前**，485s）、
  `0a5f996`（14:26–14:46，1187s）、**`3754374`（14:46:34–14:47:02，28s，tag 目标）**；
  **放行依据是第三次**（在 tag 目标上跑的），前两次只作背景；
* §1.1 的线上 og 404 → 修复：**先后两次实测**（14:12:34 与 15:57:15，均 200 且与仓库同字节）；
  但**仍无法证明「给第三方爬虫的社交卡片**这个具体消费者**拿到的就是新图**」——
  我只验了 URL 与字节，没验任何平台的抓取缓存；
* §6/§11 的校验**只覆盖三个脚本是否进包且与仓库同字节** + 签名有效；
  **没有**在真机上执行 `incident-bundle.sh` / 上传到端点（那属于功能验证，不在本环境做）；
* dmg 的 SHA256 一致性是「GitHub digest ↔ 发布资产里 `SHA256SUMS.txt` 那一行」**两方互证**，
  **我没有下载那 47 MB 复算**（zip 是下载后自己算的）—— 两者证据强度不同，别混着引；
* Release Notes 的补写是**手工**（§9.1 有 before/after 与 diff）；**这一步不是机制**，
  机制化在 `task-147` —— 在那之前，别的版本仍可能静默退化成通用模板；
* 站点两阶段发布：提交 1 阶段官网如实写「正在发布」（已实测），提交 2 之后为已发布态（§12）——
  **这不是缺陷，是设计**。

## 9. Release Notes 的版本特有动作（**本次手工，`task-147` 会做成机制**）

**背景（实测）**：`release.yml` 的 Release 正文来自 CI 里生成的静态 `NOTES.md`
（第 171–226 行 → `gh release create --title … --notes-file NOTES.md`）。
实测 `v0.8.34` 的 release body **逐字就是那个模板**（1669 字符），
里面只有通用第 3 步「点『安装 helper』」，**不含「重新安装」** ——
而本卡要求 **Release Notes 必须写明「请重新安装特权助手」**。

**Lead 裁决：选 (b)** —— tag 已经打在**已验证的 `3754374`** 上，为模板里一句话再叠一次提交
（把刚拿到的绿作废、重开冻结窗口）不值得；`gh release edit` 的结果**可读回复核**，
属于「可验证的手工」。**这一步不是机制**，已开 `task-147`
（`scripts/bump-release.py` + 让 `release.yml` 从**单一来源**取「版本特有用户动作」，**缺了必须失败**）。

执行口径（4 步，before/after 都要留原文）：

1. `gh release view v0.8.35 --json body --jq '.body' > /tmp/v0835-notes-before.md`（**读回 CI 生成的真实 body**）；
2. 只在**前部插入**一段「用户动作」，**不改动** CI 生成部分任何一行；
3. `gh release edit v0.8.35 --notes-file /tmp/v0835-notes-after.md`；
4. 再 `gh release view v0.8.35 --json body` **读回并 grep 复核**（`重新安装` 命中 ≥1，且 CI 段仍在）。

> 状态：**已执行（Lead 裁决 (b)，本次手工）** —— 4 步的原始输出如下。

### 9.1 执行与复核（原始数据）

**步骤 1（before 证据 = CI 生成的真实 body）**
```
$ gh release view v0.8.35 --json body --jq '.body' > /tmp/v0835-notes-before.md
before: 2890 字节 / 56 行 / sha256 15ffd9cbc98e65fca8baf0f27d82c0f067f5a32f7a71e52e7f5b3f2dbebd5deb
头 1 行：## 安装
```
（与 §1.3 的实测一致：正文逐字就是 `release.yml` 里的静态 `NOTES.md` 模板。）

**步骤 2（拼接：我的 action 段在前，CI body 原样在后）**
```
$ cat /tmp/v0835-release-notes-action.md /tmp/v0835-notes-before.md > /tmp/v0835-notes-after.md
after : 3938 字节 / 73 行 / sha256 99e8c85d4dfa45f7612cf763fe6f3da4590693ae7623714ca69517890e43744d
插入量 = 1048 字节（恰好等于 action 段自身的字节数）⇒ 没有改写 CI 段

$ tail -c 2890 /tmp/v0835-notes-after.md | cmp - /tmp/v0835-notes-before.md
  ✓ cmp 完全相同（**CI 生成部分零改动**）
```

**步骤 3（写回）**
```
$ gh release edit v0.8.35 --notes-file /tmp/v0835-notes-after.md
https://github.com/harodggg/xrayTun/releases/tag/v0.8.35      （exit 0）
```

**步骤 4（再从 GitHub 读回并复核）**
```
$ gh release view v0.8.35 --json body --jq '.body' > /tmp/v0835-notes-readback.md
readback: **3939** 字节 / 74 行 / sha256 6556b2649b60c6c78aeaf1993524bed996737902b0513a26f0c5b963d51d38c3

$ diff /tmp/v0835-notes-after.md /tmp/v0835-notes-readback.md
73a74
>                                          ← 只有**一个多出来的空行**（GitHub 补的尾换行）
$ grep -c '重新安装' /tmp/v0835-notes-readback.md   → **2**
$ grep -c '^## 安装' /tmp/v0835-notes-readback.md   → 1（CI 段仍在）
$ grep -c 'server.rs' /tmp/v0835-notes-readback.md  → 1（依据行在）
```
**诚实口径**：读回 **3939** vs 写入 3938，差的 **1 字节**是 GitHub 在正文末尾补的换行
（`diff` 只报 `73a74 >`（空行）；`cmp -l` 无差异输出、`cmp` 报 `EOF on` 说明前者是后者的前缀）
⇒ **内容等同，不是丢字**。这 1 字节差异如实记下，**别写成「逐字节一致」**。

### 9.2 这一步不是机制（已开 `task-147`）

`release.yml` 里的 `NOTES.md` **不随版本变化**，所以「本版要重装助手」这种**版本特有的用户动作**
只能人工补 —— 本次是「**可验证的手工**」（前后 body + `cmp` + `diff` + `grep` 全在上面）。
`task-147` 要把它做成机制：`scripts/bump-release.py`（干跑 + 失败零落盘 + 顺序依赖）
+ `release.yml` 从**单一来源**取版本特有动作，**缺了必须失败**（不许静默退化成通用模板）。

## 10. 真实资产（**三方一致**）

来源：`gh release view v0.8.35 --json isDraft,isPrerelease,publishedAt,assets`（**不手抄**）：

```
tagName v0.8.35   isDraft=false   isPrerelease=false   publishedAt=2026-09-23T07:17:15Z
SHA256SUMS.txt                    200 B        sha256:3c40bf4c259efd473f7e942258decf5ea1e8e99b54af39b820ec247626214fa5
XrayTun_0.8.35_x86_64_arm64.dmg   47,431,435 B → **45.2 MiB**   sha256:c17c9529805eacec124f6d953c7918e02a40554d323239b4a7e08f39c2b4004b
XrayTun_0.8.35_x86_64_arm64.zip   42,919,784 B → **40.9 MiB**   sha256:0f1000e76e566c57d2577e7cc9addbd92f31071bf899b3fc365833fc6d5e7fcb
```

**我自己下载后复算（`gh release download` + `shasum -a 256`）**：

| 三方 | SHA256SUMS.txt | dmg | zip |
|---|---|---|---|
| GitHub 的 `assets[].digest` | `3c40bf4c…4fa5` | `c17c9529…004b` | `0f1000e7…7fcb` |
| 我本地 `shasum -a 256` | `3c40bf4c…4fa5` ✓ | （未下载，见下） | `0f1000e7…7fcb` ✓ |
| 发布资产内 `SHA256SUMS.txt` 原文 | — | `c17c9529…004b` ✓ | `0f1000e7…7fcb` ✓ |

⇒ **三方逐字节一致**。dmg 的 47 MB **我没有下载**：它的一致性通过
「GitHub digest == 发布资产里 `SHA256SUMS.txt` 的那一行」交叉确认（**如实说明：这是两方一致，
不是下载后复算**，与 zip 的两条路径不同）。MiB 口径 = **ROUND_HALF_UP 一位小数**
（独立复核：tester 算出 45.23 / 40.93 ⇒ 45.2 / 40.9，与本文件一致）。

**与上一版对照（确认不是沿用旧值）**：dmg 47,243,124 → **47,431,435**（+188,311）、
zip 42,742,652 → **42,919,784**（+177,132）。

## 11. `.app` 里真的有三个脚本（**产物级，用发布资产**）

证据来源 = **发布资产 zip**（用户拿到的那个），`ditto -x -k` 解包（保签名）。
脚本**已进仓库**：`docs/verification/verify-app-bundle-resources.sh`（`--self-test` **pass=4 fail=0**；
它同时支持放在 `scripts/` 与 `docs/verification/` 两种落点，`ROOT` 多候选探测）。
**真产物原始输出**（下面这次是用仓库内那份跑的）：

```
$ /tmp/verify-app-bundle-resources.sh /tmp/v0835-app/XrayTun.app
  ✓ tauri.conf.json 声明的 scripts/* 恰好是 3 个：incident-bundle.sh net-metrics.py triage-incident.py
  ✓ scripts/incident-bundle.sh == scripts/incident-bundle.sh（sha256 7662c2193a9e6bf7…）
  ✓ scripts/triage-incident.py == scripts/triage-incident.py（sha256 6969e38b5355bab4…）
  ✓ scripts/net-metrics.py == scripts/net-metrics.py（sha256 e45318da088cb16f…）
  实际 Contents/Resources/scripts/ 内容：
    -rwxr-xr-x@ 1 xbtg- staff 36701 9月 23 14:48 incident-bundle.sh
    -rw-r--r--@ 1 xbtg- staff 49932 9月 23 14:48 net-metrics.py
    -rw-r--r--@ 1 xbtg- staff 45288 9月 23 14:48 triage-incident.py
  ✓ codesign --verify --strict 通过
pass=5 fail=0
```
（mtime `14:48` 是 CI 打包时刻；**我自己第一版文档里把这三行写成了 `15:17`（解包目录自身的 mtime），
已按 `ls -l` 原始输出更正** —— 记在这里，因为「同一件事写了两遍、其中一遍是我想当然」正是本项目的记录重点。）

**反向断言（真产物上的负对照）**：把期望路径故意改成 `scripts/net-metrics-NOPE.py` ⇒
```
  ✗ 声明与预期不符 —— 声明=[…net-metrics.py…] 预期=[…net-metrics-NOPE.py…]
    仓库里没有 scripts/net-metrics-NOPE.py
  ✗ scripts/net-metrics-NOPE.py 与 scripts/net-metrics-NOPE.py 不一致
pass=2 fail=2      NEG_EXIT=1
```
⇒ 检查**不是空的**。

⚠️ **工具自己又踩了一次同族坑（如实记）**：第一版跑真产物时在
`ok "$inner == $rel（sha256 …）"` 崩了 —— **全角括号紧跟变量**被 bash 吞进变量名
（`rel（sha256: unbound variable`，`check.sh` 注释里专门记过这个坑）。已改成 `${rel}`；
**自测 4/0 与上面 5/0 都是修后的结果**。

## 12. 线上验收（`verify-live-site.sh`）

```
$ VER=0.8.35 PREV=0.8.34 ./scripts/verify-live-site.sh
总结：全部通过（0 条 warning）        LIVE_EXIT=0
[0] 首页 = 43980 bytes，sha256 1a2eb4fa…     ← 与仓库 site/index.html **逐字节相同**（cmp 相同）
[1] 随机不存在路径 → HTTP 404（真 404；未观察到 SPA 兜底）
[2] og-image-0.8.35.png 200/image/png/59389 · og-image-en-0.8.35.png 200/image/png/42634（与仓库同字节）
    robots.txt 200/7215 · sitemap.xml 200/1787 · llms.txt / llms-full.txt 均为真文件
[3] （页面体/链接项）全部通过
[4] www.xraytun.top → 301 https://xraytun.top/（这条**曾经未生效**，现在好了）
[5] releases/latest → https://github.com/harodggg/xrayTun/releases/tag/v0.8.35 200
[6] GitHub Pages 镜像对不存在路径是真 404（与 apex 的 404 一致，兜底只发生在 apex）

$ ./scripts/verify-live-site.sh --self-test
self-test：四例全部符合预期（异常样例 ✗ / 缓存残留只 WARN / 真 404 正常）   SELFTEST_EXIT=0
```

**提交 2 落地后的独立复核（我自己的 curl，时点 2026-09-23 15:57:15 +0800）**：

```
zh 页 43980 B sha256 1a2eb4fa…  pinned v0.8.35 = 8（6 条 HTML + JSON-LD 2）  「正在发布」= 0  0.8.34 = 0
     真实字节数：47,431,435 ×1 · 42,919,784 ×1 · 45.2 MiB ×4 · 40.9 MiB ×2
en 页 45083 B sha256 d7719cb5…  pinned = 8   publishing = 0   0.8.34 = 0   「47,431,435 bytes」×1
cmp：zh 线上 == 仓库 ✓   en 线上 == 仓库 ✓
og 图仍 200 且与仓库同字节（59389 / 42634）—— 今天那次 404 窗口**没有回来**
```
（页脚「全部 Release」链接**故意保留**指向 Releases 页：每页 1 条。）

> **时点衔接（避免「同一件事两个结论」）**：tester 的 `task-133` 复核
> （`docs/verification/WAVE-VERIFY-v0.8.35-RELEASE.md`，7b7cd7 那一笔）
> 在 **15:54:15** 记录站点「仍是提交 1 状态（正在发布 ×2）」——那是**提交 2 部署之前**；
> 本节的复核在 **15:57:15**（Pages 与 Cloudflare Pages 两个部署都已 success，均针对 `35bbd2f`）。
> **两个观察都对，只是时点不同** ⇒ 引用时必须带时点。

## 13. 本版对照基线（`net-metrics.py`，**带口径**）

```
$ python3 scripts/net-metrics.py --until "2026-09-23 14:48:08"
```
右端 = **打 tag 时刻**（tagger epoch `1790146088`，`git cat-file -p v0.8.35` 可复算），
**口径**：全文件扫描（读 528,292 行 → 528,292 个对象、多对象行 0）、
去重键 `(ts_unix, message)` 先见者胜、跨文件 `app.1.jsonl → app.jsonl`、
丢弃重复 **40,740 条**（窗口内 40,739）、窗口内命中 **450,745 条**、
选择内容指纹 `sha256:6879fa28a0cecf28c5e72736f5602bc137c582a6df5f13328c09dd28b0945939`。
**日志在增长 ⇒ 这些是时点值，引用请连口径与时点一起引。**

| 指标 | 值（同一窗口） |
|---|---|
| `replace destination with tcp:[240e…` | **0**（v0.8.34 修掉的 v6 改写，仍为 0） |
| `failed to open connection` | 13 |
| 有改写的连接（按 session id 归并） | 3,250（全 v4：失败 2 = **0.1%**） |
| 探针连接 / 成功 / 失败·有 failed 行 | 910 / 904 / 6 |
| `cp.cloudflare.com:80` 成功耗时 | 中位 669.8ms、p90 1350.1、p95 2134.8、**p99 7375.1**、max 857065.7（n=904） |
| `>6s 才成功`的条数 | **10**（会被 6s 判据误杀 —— 这正是 v0.8.34 把超时放宽到 10s 的依据） |
| `www.baidu.com:80` | 连接 0（已在 v0.8.34 从探针目标里移除） |
| 「已作废」/「隧道已自动恢复」 | **3 / 1** |
| `core 启动` | 6 次（20:25:51、20:27:38、20:52:48、21:41:15、10:42:21、14:31:50） |
| 快照标识 | `app.1.jsonl=11,149,100B/mtime 22:14:42`；`app.jsonl=94,860,354B/mtime 15:37:38` |

完整输出：`/tmp/v0835-netmetrics.txt`（48 行）。
