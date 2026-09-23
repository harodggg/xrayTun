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

**Lead 实测（提交 1 的父提交 `3c9896a`，即版本号 bump 之前）**：
`BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build` ⇒ **`GATE_EXIT=0`**，
窗口 **13:55:38–14:03:43**，构建锁持有 **485s**，末尾 `✓ 与 CI 相同的全部检查通过`。

⚠️ **那一次不能替代本版的门禁**：①它是**版本号 bump 之前**的树；②`3c9896a` 之后又落了
`4397904`（+1 文档 / −2 og）与 `0a5f996`（delta-3 改的是 `diagnostics.rs`）。
⇒ **Lead 在 tag 前的最终 HEAD 上重跑**，原始输出与退出码在提交 2 补进本文件。

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

> 状态：**待 tag 后的发布资产**才能做（时点：提交 1 阶段，尚无 `.app`）。原始输出在提交 2 补。

## 7. 提交 2 待补清单（不预填、不推测）

- [x] 提交 1 的完整 hash = `181f9ca52f9b86ff41d4deb8b06b30ea31d0264e`
- [ ] `BUILD_LOCK_STRICT=1` 在 **tag 前最终 HEAD** 上的门禁原始输出与退出码（Lead 跑；
      含前端那一步的 **`Errors` 行**）
- [ ] annotated tag `v0.8.35` 的 `git cat-file -p` 原文 + tagger epoch
- [ ] Release workflow 结论：`isDraft == false` + **3 个资产**的真实字节数与 SHA256
- [ ] §6 的 `.app` 产物级核对原始输出（含反向断言；证据 = 发布资产 zip）
- [ ] `PUBLISHED = True` + 真实字节数 + pinned 链接 + 重跑生成器
- [ ] `VER=0.8.35 PREV=0.8.34 scripts/verify-live-site.sh` 原始输出 + `--self-test`
- [ ] `python3 scripts/net-metrics.py --until <打 tag 时刻>` 的基线（**带口径**）
- [ ] **线上复核**：新 og 图 200（提交 1 已部署，见 §1.1）、`0.8.34` 残留 = 0、pinned 出现且与资产同字节

## 8. 诚实清单（**本篇已生效**的部分）

* **真机安装 / 重启 / Gatekeeper / 重装特权助手未在本环境验证** —— 本环境不具备
  「下载包 → 替换 → 打开」的完整链路；
* 本环境**不在中国大陆**，GFW 行为无法复现；
* 「用户装上新版之后的 after 数字」**现在不存在**，本篇不预填、不推测；
* §2 的磁盘数字是**时点值**：Lead 之后腾过一部分空间，**发版过程中它还在变**；
* §5 的绿是**版本号 bump 之前**那一版的（`3c9896a`），且它之后又落了 `4397904` 与 `0a5f996`
  ⇒ **不能用它替代 tag 前那次门禁**；
* §1.1 里「线上 og:image 404」是**实测**（curl 原始状态码），修复后的复核**我已在发送报告前
  实测过一次**（时点 2026-09-23 14:12:34：`og-image-0.8.35.png` 200 / 59389 B、
  `og-image-en-0.8.35.png` 200 / 42634 B，与仓库逐字节一致），提交 2 会再复核一次；
* §6 的三脚本核对在本阶段**还没做**（没有产物），现在只是「口径已定好 + 自测过」，不是「已验证」；
* 站点两阶段发布下，**官网此刻如实写「正在发布」**，没有字节数、没有 pinned 直链 ——
  这不是缺陷，是设计。
