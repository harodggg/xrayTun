# v0.8.31 发布验收（独立验证 · task-76）

> **角色**：发版的**独立验证者**。`ops` 执行发布（task-74 两阶段），我负责**不信它的自述，自己跑一遍**。
> 本文件里每一条结论都带「**我跑的命令**」与「**原始输出**」；凡是我没亲自跑出来的，一律进 §6 并标注来源。
> 不接受转述、不接受截图。
>
> **纪律**：本卡只写本文件（脚本与下载都放 `/tmp`）；**未改** `site/**`、`Cargo.toml`、`apps/**`、`scripts/**`、`CHANGELOG.md`；
> **未跑** `check.sh`（改为用 `sed` 逐条复现它的 6 条断言 —— 验的是**同一条事实**，不是同一条命令）。

## 结论（一句话版）

**v0.8.31 发布通过我的独立验证**：三个资产我都**自己下载、自己算哈希与字节数**（dmg `22c640d1…`/47,179,066 B、
zip `10b278f3…`/42,681,220 B，与 `SHA256SUMS.txt` 两行 **MATCH**）；包内 app `codesign --verify --strict` **EXIT=0**
（adhoc，与页面声明一致）、Info.plist 版本 **0.8.31**、隔离属性 **0**；`app_update_check` 整条链路 **EXIT=0**（我这边 **0 次** exit 28）；
线上 `/` 与 `/en/` 与提交 `400d15c` **逐字节相同**，页面印的精确字节数**等于我自己下载算出的值**，3 条 pinned 直链全 **200**。
过程中我抓到并促成了 **1 处真缺陷**（提交 1 页面上仍展示 0.8.30 的 45.0/40.7 MiB）、纠正了 **3 处自述口径**，
并点出 **2 个门禁盲区**（`check.sh` 不覆盖 wasm 两页、也不覆盖 `Cargo.lock`）+ **1 个平台陷阱**（CF 软 404 让状态码失效）。
**明确没验的**：真机安装、自动更新最后一步（替换+重启）、公证/Gatekeeper 真实体验、用户浏览器下载路径 —— 见 §5。

## 0. 修订与口径（先说清「我量的是哪个对象、哪一瞬间」）

| 对象 | 修订 | 状态 |
|---|---|---|
| v0.8.31 冻结修订 | `035e083` | lead 在此跑过完整门禁（我按纪律**未重跑** `check.sh`） |
| **提交 1**（版本 bump + 官网「正在发布」） | **`0e5fcf881bcc01f2cd9dcbd35e0df20a4bd54475`**（短 `0e5fcf8`，**父 `e99da22`**） | ✅ 已落地；**工作区 clean（脏文件数 0）**，所以我下面量到的就是提交内容本身 |
| **tag `v0.8.31`** | `1c108d39…`（annotated）→ **指向 `0e5fcf8`** | ✅ Release workflow **completed / success**（18m56s） |
| **资产（3 个：dmg / zip / SHA256SUMS）** | 构建产物，非修订 | ✅ 已发布（`publishedAt 2026-09-21T10:28:33Z`），见 §3.1 |
| **提交 2**（站点填真实字节数 + `PUBLISHED=True` + pinned 直链） | **`400d15c`**（**在 tag 之后**） | ✅ 已部署，见 §3.4 / §3.5 |
| 线上站点 | — | ✅ 此刻 `/` 与 `/en/` 与提交 `400d15c` **逐字节相同** |

**复现入口**：本节的每一条都由一个只读脚本一次跑出 —— `bash /tmp/verify-commit1.sh`（输出 `/tmp/verify-commit1.out`，104 行）。
它**不写仓库任何文件**：两个生成器只在 `/tmp` 副本里跑。

> ⚠️ **一处我核出的自述偏差（先说，因为它正是本卡的纪律）**：
> `ops` 报「提交 1 已落地：父 `035e083`，18 文件 +681/−119」。我实测：
> * `git show --shortstat 0e5fcf8` → **17 files changed, 248 insertions(+), 119 deletions(-)**；
> * `git show 0e5fcf8^` → **`e99da22`**（不是 `035e083`）；
> * `git diff --shortstat 035e083 0e5fcf8` → **18 files, 681 insertions**。
>
> 原因：`035e083..0e5fcf8` 这个**区间**里还有**我自己的 3 个文档提交**（`aca7a80`/`8fc6a05`/`e99da22`），
> 而 `docs/verification/RELEASE-v0.8.31.md` 恰好是 **+433 行**（`git diff --numstat 035e083 0e5fcf8 -- <该文件>` = `433 0`）。
> 验算：**248 + 433 = 681**，**17 + 1 = 18** —— 完全对上。
> ⇒ `ops` 量的是「从 `035e083` 到 HEAD 的区间」，却写成了「提交 1」。**提交 1 的内容没问题，是「量的是哪个对象」写错了**，
> 而这正是 task-66 我栽过的同一个坑（88 行 vs 190–192 行）。我按要求**只报告**。

---

## 1. 提交 1 的验证（修订 `0e5fcf8`，工作区 clean）

### 1.1 版本号：`check.sh` 那 6 条断言 + 其余来源 + `Cargo.lock`

**我跑的命令**（**没用** `check.sh`，逐条自己复现）：

```bash
want="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
sed -n 's/^XRAYTUN_VERSION *= *"\([^"]*\)".*/\1/p' scripts/gen-site-jsonld.py | head -1
sed -n 's/^VERSION *= *"\([^"]*\)".*/\1/p'          scripts/gen-site-geo.py | head -1
sed -n 's/.*PAGE_VERSION *= *"\([^"]*\)".*/\1/p'    site/assets/site.js | head -1
sed -n 's/^SITE_VERSION *= *"\([^"]*\)".*/\1/p'     scripts/gen-site-images.py | head -1
sed -n 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' site/index.html | head -1
sed -n 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' site/en/index.html | head -1
grep -n '"version"' apps/desktop/tauri.conf.json apps/ui/package.json; grep -n '^version' Cargo.toml
grep -A1 -E '^name = "(xraytun-desktop|xt-core|xt-helper|xt-proto|xt-tun)"' Cargo.lock | grep version
CARGO_HOME=/tmp/xt-cargo-home cargo metadata --locked --offline --format-version 1 >/dev/null; echo $?
```

**原始输出**：

```
want (Cargo.toml [workspace.package]) = 0.8.31
  gen-site-jsonld.py XRAYTUN_VERSION   0.8.31     ← 断言 1
  gen-site-geo.py VERSION              0.8.31     ← 断言 2
  site.js PAGE_VERSION                 0.8.31     ← 断言 3
  gen-site-images.py SITE_VERSION      0.8.31     ← 断言 4
  site/index.html dmg 文件名           0.8.31     ← 断言 5
  site/en/index.html dmg 文件名        0.8.31     ← 断言 6
  Cargo.toml:12:version = "0.8.31"
  tauri.conf.json:4:  "version": "0.8.31"
  package.json:4:     "version": "0.8.31"
  Cargo.lock: xraytun-desktop / xt-core / xt-helper / xt-proto / xt-tun = 0.8.31（5 个）
  Cargo.lock 里 "0.8.30" 计数 = 0
  cargo metadata --locked --offline EXIT = 0
```

| 检查项 | 读数 | 结论 | 与执行者自述一致？ |
|---|---|---|---|
| 6 条断言（1–6） | 全部 `0.8.31` | ✅ | 一致 |
| `Cargo.toml`（真源） | `0.8.31` | ✅ | 一致 |
| `tauri.conf.json` / `package.json` | `0.8.31` / `0.8.31` | ✅ | 一致 |
| `Cargo.lock`（**不在 6 条断言内**） | 5 个 crate 全 `0.8.31`、`0.8.30` = 0、`--locked` **EXIT=0** | ✅ | 一致 |

> **口径备注（保留我在过程中看到的中间态）**：`17:5x`（提交 1 之前）我曾读到 `Cargo.lock` 仍是 `0.8.30`×5，
> 且 `cargo metadata --locked --offline` **EXIT=101**（`cannot update the lock file … because --locked was passed`）；
> 我在**一致**修订 `c62a4ae` 上跑同一条命令是 **EXIT=0**（对照）⇒ 证明那次失败确实由 bump 引起。
> `ops` 随后重生成。**这两条是不同瞬间的两个对象**，我分别记录，不当缺陷。

### 1.2 「正在发布」态：pinned 链接与状态码

**我跑的命令 / 原始输出**（脚本 §5）：

```
site/** 里 "releases/download" 出现   = 0 条          ← 一个 pinned 直链都没有
https://github.com/harodggg/xrayTun/releases                                        -> 200
https://github.com/harodggg/xrayTun/releases/latest                                 -> 302 → .../releases/tag/v0.8.30
https://github.com/harodggg/xrayTun/releases/download/v0.8.31/XrayTun_0.8.31_x86_64_arm64.dmg -> 404
gh release view v0.8.31 -> release not found
```

| 检查项 | 读数 | 结论 |
|---|---|---|
| 页面里有没有 pinned 直链？ | `releases/download` = **0** | ✅ 本阶段不会把用户点到 404 |
| 那条 pinned URL（**预期失败项**） | **404** | ✅ **本阶段预期**；页面**没有**指向它，故自洽 |
| `releases`（页面实际指向） | **200** | ✅ |
| `releases/latest` | **302 → `tag/v0.8.30`** | ✅ 事实正确：0.8.31 还没发布 |
| 资产是否存在 | `release not found` | ✅ 与「正在发布」一致 |

### 1.3 陈旧字节数：我抓到的**真缺陷** → `ops` 已修 → 我复验通过

**抓到的时刻（提交 1 之前，已写进 `aca7a80`）**：`*_BYTES/*_MIB` 常量确已清空，但页面仍在**展示**上一版数字：

```
site/index.html:250,254,463 ×3   site/en/index.html:251,255,493 ×3   site/llms-full.txt ×4   —— 共 10 处
GitHub API: v0.8.30 dmg = 47154951 bytes = 45.0 MiB；zip = 42659730 bytes = 40.7 MiB  ← 与页面数字逐位吻合
gh release view v0.8.31 = not found；本地 find 无任何 0.8.31 产物 ⇒ 0.8.31 的字节数此刻不可能被测过
项目自己的规则：gen-site-geo.py 头部「提交 1 … 不沿用上一版的字节数（那是错的）」；site/llms.txt:26「不在这里预先写死字节数或直链」
```

**修订 `0e5fcf8` 上我重跑的读数**（脚本 §3）：

```
grep -rn '45\.0 MiB|40\.7 MiB' site/            → 0 条
grep -rn '47,154,951' site/ → 0 条；'42,659,730' → 0 条
site/index.html:250,463  >下载 macOS 版 v0.8.31 · dmg（大小见 Releases）</a>
site/index.html:254      >备用 .zip（大小见 Releases）</a>
site/en/index.html:251,493 >Download for macOS v0.8.31 · dmg (size per Releases page)</a>
site/en/index.html:255   >Alternative .zip (size per Releases page)</a
site/en/index.html:258   v0.8.31 publishing now · … · size per Releases page
```

| 检查项 | 读数 | 结论 |
|---|---|---|
| 陈旧 MiB（10 处） | **0 条** | ✅ 已修 |
| 上一版原始字节数（`47,154,951` / `42,659,730`） | **0 / 0** | ✅ 已修 |
| 新文案 | 「（大小见 Releases）」/「(size per Releases page)」 | ✅ 不再声称一个测不到的数字 |

⇒ **这是本卡抓到并闭环的一处真缺陷**（`ops` 已归因到根：0.8.30 的 phase-2 脚本只换文案、没清尺寸后缀）。
`check.sh` 的 6 条断言**不会**发现它 —— 它们只比版本号字符串，不比字节数。

### 1.4 ⚠️ 已知盲区 1：`check.sh` 对 **wasm 两页零覆盖**

**我跑的命令 / 原始输出**（脚本 §9）：

```
grep -c 'wasm' scripts/check.sh  → 0 条匹配        ← 6 条断言完全不覆盖这两页
site/wasm/index.html:217       XrayTun 的当前版本是另一个序列（<strong>v0.8.31</strong>，见首页）。
site/en/wasm/index.html:224    (<strong>v0.8.31</strong>, see the <a href="../../">home page</a>).
wasm 页自身版本 0.7.0 在 zh 页出现 21 次（xray-wasm 自己的版本，不该跟 XrayTun 变）
```

| 检查项 | 读数 | 结论 |
|---|---|---|
| wasm 两页的 XrayTun 版本 | 都是 `v0.8.31`（**手工核**） | ✅ 本次正确 |
| 自动断言覆盖 | `grep wasm scripts/check.sh` → **0** | ❌ **盲区仍在**：下次漏改无人守 |

### 1.5 `site/**` 里 0.8.30 残留 = 0，以及 0.8.31 的 68 次（**逐项对象说明**）

**我跑的命令 / 原始输出**（脚本 §4；基线用 `git grep` 在 `035e083` 复算）：

```
0e5fcf8: grep -ro '0\.8\.30' site/ | wc -l              → 0
035e083: git grep -o '0\.8\.30' 035e083 -- site/ | wc -l → 122
   逐文件（035e083）：index 28 / en 28 / llms-full 23 / llms 4 / site.js 1 / wasm 1 / en-wasm 1
0e5fcf8: grep -ro '0\.8\.31' site/ | wc -l              → 68
   逐文件：llms-full 21 / index 20 / en 20 / llms 3 / wasm 1 / en-wasm 1 / site.js 1
```

| 文件 | `0.8.30`@`035e083` | `0.8.31`@`0e5fcf8` |
|---|---|---|
| `site/index.html` | 28 | 20 |
| `site/en/index.html` | 28 | 20 |
| `site/llms-full.txt` | 23 | 21 |
| `site/llms.txt` | 4 | 3 |
| `site/assets/site.js` | 1 | 1 |
| `site/wasm/index.html` | 1 | 1 |
| `site/en/wasm/index.html` | 1 | 1 |
| **合计** | **122** | **68** |

* `ops` 报的基线 **122** 我用 `git grep` **复算完全吻合**（逐项 28/28/23/4/1/1/1）✅
* **0.8.30 残留 = 0** ✅（「每项都变 0」在这个阶段是**预期**）
* `ops` 对「122 → 68，差 54」的解释是「提交 1 撤掉了所有 pinned URL 与 og-image 文件名，每条 URL 含两次版本号」。
  **我把它当数字核了一遍，发现分解不出来**：

```
035e083 上：含 "releases/download" 的行里，0.8.30 出现 = 65 次（共 30 条 pinned URL）
035e083 上：含 "og-image" 的行里，0.8.30 出现          = 4 次
65 + 4 = 69 ≠ 54
```

  ⇒ 我据此**否决了 `ops` 最初那句分解**（65+4=69≠54）。`ops` 随后**自己更正**为「按行分类、两态同口径」，
  我**又把更正版核了一遍，逐项吻合**：

```
035e083（0.8.30）  pinned 行 65 + og 行 4 + 其余 53 = 122
0e5fcf8（0.8.31）  pinned 行  0 + og 行 4 + 其余 64 =  68
差额：            −65         +0        +11   = −54   ✅
```

  ⇒ **正确分解**：撤掉 **pinned 行内 65 处**（本阶段 pinned 直链全部删除）、**og 行 −0**（旧图换成 `0.8.31` 新文件名，仍有 4 处）、
  **其余新增 11 处**（新文案/JSON-LD/描述里新写的 `v0.8.31`），净差 **−54**。
  **两次都记在这里**：我对**旧说法**的质疑成立，对**更正版**的复核也成立 —— 与 §0 那处「18 文件/+681」是同一条纪律（数字必须说清量的是哪个对象）。

### 1.6 CHANGELOG：**曾停在 0.8.30** → `ops` 已补 → 我复验通过

**抓到的时刻（已写进 `8fc6a05`）**：`CHANGELOG.md` 顶部是 `## 0.8.30`，提交 1 未动它
（`grep -c '0.8.31' CHANGELOG.md` = **0**），而站点把用户指向它 ——
`site/index.html:58` JSON-LD `"releaseNotes"`、`:746` 「更新记录」链接、`site/en/index.html:59,793`、
`site/llms.txt:46`、`site/llms-full.txt:303,736`；历史上 `72b21fb`(0.8.30)/`73351a9`(0.8.29)/`29cb6f8`(0.8.28) 每次都改了。

**修订 `0e5fcf8` 上的复验**（脚本 §10）：

```
顶部条目：## 0.8.31
grep -c '0\.8\.31' CHANGELOG.md = 1
```

| 检查项 | 读数 | 结论 |
|---|---|---|
| 顶部条目 | `## 0.8.31` | ✅ 已补 |
| `grep -c "0.8.31"` | **1** | ✅ 通过 |

> **计数口径说明**（回答 `ops` 的提问）：我**不要求**正文复读版本号。我量的是「顶部条目存在」，
> `grep -c ≥ 1` 只是最省事的代理；历史上 0.8.30 条目正文同样不复读。真正要防的是「点进去还是上一版」，
> 那由**顶部条目标题**决定，**不需要为凑数加版本号**。

### 1.7 OG 图：旧图已被**改名**替换，不是「留在目录里的死文件」

`ops` 删/换了 `site/og-image-0.8.30{,-en}.png`。我实测（`git show --stat 0e5fcf8`）：

```
site/{og-image-0.8.30.png => og-image-0.8.31.png}       | Bin 59001 -> 58984 bytes
site/{og-image-en-0.8.30.png => og-image-en-0.8.31.png} | Bin 42278 -> 42263 bytes
当前 site/ 里只剩 4 个 og 图：0.8.31(zh/en) + wasm-0.7.0(zh/en)
仓库内对 "og-image-0.8.30" 的引用 = 0（除本文件旧版的一句自述，现已改）
```

⇒ 旧图**是被 rename 换掉的**（`git show --diff-filter=D` 为空 ⇒ 不是删除记录），
新图 `og-image-0.8.31.png` / `og-image-en-0.8.31.png` **存在**且被 `site/index.html:37,44` / `site/en/index.html:38` 正确引用 ✅。
**我上一版文档里「旧 og-image-0.8.30.png 仍在目录里但无人引用（死文件）」这句话已过期**，此处更正。

---

## 2. 决定性验证：两个生成器「重跑一次，产物与仓库逐字节相同」

这是本卡最有价值的一类证据 —— 不是「我 grep 到 0.8.31 了」，而是**把生成器在副本里重跑**，
把产物与仓库**逐字节** `diff -q`。若 `ops` 漏跑 `gen`（本项目栽过一次的坑），产物就会与仓库不一致。

**我跑的命令**（仓库只读；生成器只在 `/tmp` 副本里跑）：

```bash
# JSON-LD
cp scripts/gen-site-jsonld.py /tmp/jsonld-test/scripts/ && cp -R site /tmp/jsonld-test/site
cd /tmp/jsonld-test && python3 scripts/gen-site-jsonld.py gen
diff -q <repo>/site/<page> /tmp/jsonld-test/site/<page>
# GEO（注意：gen-site-geo.py 没有 check 模式，跑它就是写文件 —— 所以必须在副本里跑）
cp scripts/gen-site-geo.py /tmp/geo-test/scripts/ && cp -R site /tmp/geo-test/site
cd /tmp/geo-test && python3 scripts/gen-site-geo.py
diff -q <repo>/site/<f> /tmp/geo-test/site/<f>
```

**原始输出**（脚本 §7、§8）：

```
〔JSON-LD〕副本 gen 后：
  IDENTICAL site/index.html      IDENTICAL site/en/index.html
  IDENTICAL site/wasm/index.html IDENTICAL site/en/wasm/index.html
仓库上跑默认 check（只读）：4 个页面全部 ✓，结论: 全部通过
  （可见文本 13554 / 27306 / 7617 / 14400 字；FAQ 16/16/5/5 条与页面逐条一致）

〔GEO 产物〕副本 gen-site-geo.py 后：
  IDENTICAL site/llms.txt        IDENTICAL site/llms-full.txt
  IDENTICAL site/robots.txt      IDENTICAL site/sitemap.xml
```

**我直接读出来的 JSON-LD 值**（不是从脚本推的，脚本 §6）：

```
site/index.html:54   "softwareVersion": "0.8.31"
site/index.html:56   "downloadUrl": "https://github.com/harodggg/xrayTun/releases"   ← 非 pinned
site/index.html:57   "installUrl":  "https://github.com/harodggg/xrayTun/releases"
site/en/index.html:55 "softwareVersion": "0.8.31"
site/en/index.html:57 "downloadUrl": "https://github.com/harodggg/xrayTun/releases"
```

| 检查项 | 读数 | 结论 |
|---|---|---|
| `softwareVersion` | `"0.8.31"` | ✅ 不是上一版残留 |
| `downloadUrl` / `installUrl` | Releases 页（**非 pinned**） | ✅ 与 `PUBLISHED=False` 一致（若此时是 pinned，我会在 §1.2 看到 404 且页面指向它） |
| JSON-LD 产物 vs 仓库 | **4/4 IDENTICAL** | ✅ `gen` **没有**漏跑 |
| GEO 产物 vs 仓库 | **4/4 IDENTICAL** | ✅ llms/robots/sitemap 与页面**机制上等价**（`llms-full.txt` 里那 4 处 MiB 也随页面消失） |
| `PUBLISHED` | `gen-site-jsonld.py:42` = `False`；`gen-site-geo.py:37` = `False` | ✅ |

---

## 3. 资产出现后的验证（**已完成** —— v0.8.31 已发布）

### 3.0 被测对象（先说清量的是哪个）

| 项 | 值（我跑出来的） |
|---|---|
| tag `v0.8.31` | `1c108d39d5d1675ffba48abdd205d7f89af9563f`（annotated），**指向提交 `0e5fcf8`** |
| 提交 2（站点填真实数据） | `400d15c`，**在 tag 之后**（tag 只钉代码提交） |
| GitHub Release | `publishedAt=2026-09-21T10:28:33Z`、`isDraft=false`、`isPrerelease=false` |
| Release workflow | `gh run list --workflow=release.yml` → **completed / success**（18m56s） |
| 我下载到的 | `/tmp/xt-release-v0.8.31/`（`gh release download v0.8.31 --repo harodggg/xrayTun`） |
| 我跑的命令 | `bash /tmp/verify-release.sh` → 原始输出 `/tmp/verify-release.out` |

### 3.1 三个资产：**我自己下载、我自己算**（页面上的数字一个都没抄）

**命令**：

```bash
gh release download v0.8.31 --repo harodggg/xrayTun -D /tmp/xt-release-v0.8.31
shasum -a 256 *          # 我自己算
for f in *; do stat -f "%z  %N" "$f"; done
cat SHA256SUMS.txt
```

**原始输出**：

```
--- 我下载到 /tmp 的（这也是我量到的字节数）---
200        SHA256SUMS.txt
47179066   XrayTun_0.8.31_x86_64_arm64.dmg
42681220   XrayTun_0.8.31_x86_64_arm64.zip

--- 我自己算的 sha256 ---
7d5832dd716741abf6e7eecb01852e7cfb123f0d088713ca77e5df9f5b4bff19  SHA256SUMS.txt
22c640d117ac39840fd8ef5955f250ca0b59d208101f7597560767c2bee77652  XrayTun_0.8.31_x86_64_arm64.dmg
10b278f350e23de87b51601d2fd0fb9ee9f33f2b48f04a5690c5f85e5552edb3  XrayTun_0.8.31_x86_64_arm64.zip

--- SHA256SUMS.txt 原文（注意 ./ 前缀，这正是历史上踩过的坑）---
22c640d117ac39840fd8ef5955f250ca0b59d208101f7597560767c2bee77652  ./XrayTun_0.8.31_x86_64_arm64.dmg
10b278f350e23de87b51601d2fd0fb9ee9f33f2b48f04a5690c5f85e5552edb3  ./XrayTun_0.8.31_x86_64_arm64.zip

--- 逐行比对 ---
  MATCH   XrayTun_0.8.31_x86_64_arm64.dmg
  MATCH   XrayTun_0.8.31_x86_64_arm64.zip
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| 资产个数 | **3**（dmg / zip / SHA256SUMS） | ✅ |
| dmg 字节数 | **47,179,066** | ✅ 我算的 |
| zip 字节数 | **42,681,220** | ✅ 我算的 |
| `SHA256SUMS.txt` 字节数 | **200** | ✅ 我算的 |
| 我自己算的 sha256 vs 文件里写的 | dmg **MATCH**、zip **MATCH** | ✅ **不是抄 GitHub 页面** |
| 我自己算的 sha256 vs `gh` API 的 `digest` | 三个都相同（`22c640d1…` / `10b278f3…` / `7d5832dd…`） | ✅ 两条独立取数一致 |

### 3.2 dmg 完整性：`codesign` 退出码 / 挂载 / xattr

**命令与原始输出**：

```
{ dmg 本体 }
codesign --verify --strict --verbose=2 XrayTun_0.8.31_x86_64_arm64.dmg
  → "code object is not signed at all"        DMG codesign EXIT = 1

{ 挂载 }
hdiutil attach -nobrowse -readonly -mountpoint /tmp/xt-mnt-v0.8.31 <dmg>   → EXIT = 0
  /dev/disk6s2 Apple_HFS /private/tmp/xt-mnt-v0.8.31
  内容：Applications -> /Applications（软链） + XrayTun.app

{ 包内 app }
codesign --verify --strict --verbose=2 /tmp/xt-mnt-v0.8.31/XrayTun.app
  --prepared:…/xraytun-helper   --validated:…/xraytun-helper
  "valid on disk" + "satisfies its Designated Requirement"
  APP codesign --verify --strict EXIT = 0            ← 关键退出码

{ 签名详情 }
Identifier=com.xraytun.desktop   Format=app bundle with Mach-O universal (x86_64 arm64)
CodeDirectory v=20400 flags=0x2(adhoc)   Signature=adhoc        ← 无 Developer ID / 无 TeamIdentifier

{ 包内版本（我读的 Info.plist）}
CFBundleShortVersionString = 0.8.31
CFBundleVersion            = 0.8.31

{ xattr }
带 com.apple.quarantine 的文件数 = 0     ← 历史基线 13 → 应为 0，符合
带任意 xattr 的文件数            = 0
（挂载点内 find -type f 共 8 个文件）

{ 下载到 /tmp 的三个文件自身的 xattr }
SHA256SUMS.txt / .dmg / .zip → 都只有 com.apple.provenance（.dmg 另有 com.apple.diskimages.recentcksum）
→ 没有任何 com.apple.quarantine
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `.dmg` 本体 `codesign` | **EXIT=1**，`code object is not signed at all` | ⚠️ **dmg 未签名**（如实记录；见下方口径） |
| 包内 `XrayTun.app` `codesign --verify --strict` | **EXIT=0**（valid on disk / satisfies its Designated Requirement） | ✅ |
| 签名类型 | **adhoc**（`flags=0x2(adhoc)`、无 TeamIdentifier） | ✅ **与页面声明一致**（页面明写「没有签名校验」） |
| 包内版本号（我读 Info.plist） | `0.8.31` / `0.8.31` | ✅ |
| 隔离属性计数 | **0**（挂载点内 8 个文件里 0 个带 quarantine） | ✅ 符合「应为 0」 |
| 挂载内容 | `Applications` 软链 + `XrayTun.app` | ✅ 可拖拽安装的结构 |

> **口径（这里最容易含糊过去）**：`com.apple.quarantine` 是**下载器**加的，不是包做的。
> 我是用 `gh release download`（curl 路线）取的，所以三个文件**本来就不会**带 quarantine ——
> **我的 0 证明的是「包内没有烘焙进隔离/FinderInfo 属性」**（这是打包要保证的事）；
> 它**不证明**「用户用浏览器下载后不带隔离属性」——浏览器下载**会**由 macOS 自己加上。
> 用户路径要看到的是「首次打开需要一次 `xattr -d` 或右键打开」，这与页面 FAQ 的说法一致。

### 3.3 `app_update_check`：整条链路（除最后替换）走通 —— **第一次就成功，exit 28 次数 = 0**

**命令**：`CARGO_HOME=/tmp/xt-cargo-home CARGO_TARGET_DIR=/tmp/xt-target /tmp/xt-target/debug/examples/app_update_check`

**原始输出（节选）**：

```
=== 1) 查最新版 …===
=== 2) 下载（每 200ms 报进度）=== 10% … 100%  ✓ 下载完成
=== 3) 校验（这一步以前一直失败）===
  校验文件原文：
    22c640d1…  ./XrayTun_0.8.31_x86_64_arm64.dmg
    10b278f3…  ./XrayTun_0.8.31_x86_64_arm64.zip
  期望 10b278f350e23de87b51601d2fd0fb9ee9f33f2b48f04a5690c5f85e5552edb3
  实际 10b278f350e23de87b51601d2fd0fb9ee9f33f2b48f04a5690c5f85e5552edb3
  ✓ 校验通过
=== 4) 解压并核对包内版本 ===
  包内版本 0.8.31，release 声称 0.8.31    ✓ 一致

✓ 整条自更新链路（除最后的替换）全部走通
app_update_check EXIT = 0
尝试次数 = 1 ; exit 28（超时）次数 = 0
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `app_update_check` 退出码 | **0** | ✅ |
| **我遇到的 exit 28（超时）次数** | **0**（第 1 次尝试即成功；我的重试循环最多 3 次，只跑了 1 次） | ✅ 与上一版「第一次 exit 28」不同 —— 但那是**上一版**的对象，两次不矛盾 |
| 包内版本 vs release 声称 | `0.8.31` vs `0.8.31` | ✅ |
| SHA256 校验 | 期望 = 实际 = `10b278f3…` | ✅（与我在 §3.1 自己算的**同一个值**） |

### 3.4 线上站点：提交 2 已部署，且**逐字节**等于提交

**命令与原始输出**：

```
curl -s https://xraytun.top/     → 43980 bytes
   0.8.31 出现 35 次   0.8.30 出现 0 次   「正在发布」出现 0 次
   canonical = https://xraytun.top/
   hreflang: zh-Hans → https://xraytun.top/ ; en → https://xraytun.top/en/ ; x-default → https://xraytun.top/
curl -s https://xraytun.top/en/  → 45083 bytes
   0.8.31 出现 35 次   0.8.30 出现 0 次   canonical = https://xraytun.top/en/

git show 400d15c:site/index.html    vs 线上 /      → IDENTICAL（逐字节）
git show 400d15c:site/en/index.html vs 线上 /en/   → IDENTICAL（逐字节）

线上页面里的精确字节数：42,681,220 与 47,179,066     ← 与我在 §3.1 自己下载算出的**完全相同**
旧的 0.8.30 精确字节数（47,154,951 / 42,659,730）：出现 0 次
pinned 直链（3 条，全部指向 v0.8.31）：
  releases/download/v0.8.31/XrayTun_0.8.31_x86_64_arm64.dmg
  releases/download/v0.8.31/XrayTun_0.8.31_x86_64_arm64.zip
  releases/download/v0.8.31/SHA256SUMS.txt
这 3 条我逐个 curl：全部 **200**
页面上 MiB 显示：45.0 MiB ×4、40.7 MiB ×2
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| 线上 `0.8.31` 次数 | `/` **35**、`/en/` **35** | ✅ |
| 线上 `0.8.30` 次数 | `/` **0**、`/en/` **0** | ✅ |
| 线上「正在发布」 | `/` **0**、`/en/` **0** | ✅ 已切换到「已发布」态 |
| canonical | `/`→`https://xraytun.top/`；`/en/`→`https://xraytun.top/en/` | ✅ |
| 三向 hreflang | `zh-Hans` / `en` / `x-default` 齐全且指向正确 | ✅ `/en/` 与 `/` 等价互换 |
| **线上页面 vs 提交 `400d15c`** | **逐字节 IDENTICAL** | ✅ 线上就是我验收过的那一份 |
| 页面上印的精确字节数 | `47,179,066` / `42,681,220` | ✅ **等于我自己下载算出的值**，不是上一版的 47,154,951 / 42,659,730 |
| pinned 直链可达性 | 3 条全部 **200** | ✅（这正是历史上「pinned 直链 404」那类事故的反面） |

> **一处必须点明的「同一个数字、不同对象」**：MiB 显示仍是 **45.0 / 40.7**，与 0.8.30 时**一模一样** ——
> 但精确字节数**变了**（47,154,951→**47,179,066**、42,659,730→**42,681,220**）。
> 若只看 MiB，会以为「没更新」；**判据必须是精确字节数或哈希**。这正是我在 §1.3 抓那个缺陷时用的同一条纪律。

### 3.5 提交 2 自身的复核（站点侧，我独立交叉核对 `ops` 填的数）

**命令与原始输出**：

```
git show 400d15c:scripts/gen-site-jsonld.py | grep '^PUBLISHED'   → PUBLISHED = True
git show 400d15c:scripts/gen-site-geo.py    | grep '^PUBLISHED'   → PUBLISHED = True
git show 400d15c:scripts/gen-site-geo.py | grep 'BYTES\|MIB':
   DMG_BYTES, DMG_MIB = "47,179,066", "45.0"
   ZIP_BYTES, ZIP_MIB = "42,681,220", "40.7"
   SHA_BYTES = "200"
6 条版本断言在 400d15c 上重跑 → 全部 0.8.31
site/** 里 0.8.31 计数 = 122 ；0.8.30 = 0
JSON-LD（400d15c）：softwareVersion "0.8.31"；downloadUrl/installUrl =
   .../releases/download/v0.8.31/XrayTun_0.8.31_x86_64_arm64.dmg   ← 已 pinned（且该 URL curl = 200）
```

**决定性验证（在 `400d15c` 的干净 worktree 副本里重跑两个生成器）**：

```
IDENTICAL site/index.html      IDENTICAL site/en/index.html
IDENTICAL site/wasm/index.html IDENTICAL site/en/wasm/index.html
IDENTICAL site/llms.txt        IDENTICAL site/llms-full.txt
IDENTICAL site/robots.txt      IDENTICAL site/sitemap.xml
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `PUBLISHED` | `True` ×2 | ✅ |
| `ops` 填的**精确字节数** | `47,179,066` / `42,681,220` / `200` | ✅ **与我自己下载算出的逐位相同**（§3.1） |
| `ops` 填的 MiB | `45.0` / `40.7` | ✅ 是**新**字节数的正确取整（不是沿用旧值） |
| 6 条断言 | 全部 `0.8.31` | ✅ |
| `site/**` 计数 | `0.8.31` = **122**、`0.8.30` = **0** | ✅ 如 lead 预告：填回 pinned 直链后**从 68 回升到 122**（**注意：这是提交 2 的数字，「68」是提交 1 的数字**） |
| JSON-LD `downloadUrl` | 已 pinned，且 `curl` = **200** | ✅ 没有「pinned 到 404 的资产」 |
| 两个生成器产物 vs 仓库 | **8/8 IDENTICAL** | ✅ 提交 2 也**没有**漏跑 `gen` |

### 3.6 ⚠️ 线上站点的一个**会骗人的行为**：CF Pages 的 SPA 兜底（我独立复现）

`ops` 提示：`xraytun.top` 上**不存在的路径返回 200 + 首页 HTML**。我自己复现了三条 URL：

```
https://xraytun.top/                                bytes=43696  sha256(前16)=1e12653843e9f902  content-type=text/html
https://xraytun.top/og-image-0.8.30.png             bytes=43696  sha256(前16)=1e12653843e9f902  content-type=text/html
https://xraytun.top/this-path-does-not-exist-xyz    bytes=43696  sha256(前16)=1e12653843e9f902  content-type=text/html
```

⇒ **三条 URL 的 body 完全相同**（包括那个「图片」路径！），`content-type` 都是 `text/html`。
**结论：在 `xraytun.top` 上不能用状态码判断资产是否存在** —— 连 `curl -sI` 的 200 也不能。
本卡后续凡是「某个资源在不在线上」的判断，我一律用 **`content-type` + body sha256** 双证据，
并标注它**是既有行为**（不是本次发布引入），任何打错的路径都是 **200 软 404**。

### 3.7 `www.xraytun.top`：**200，无跳转**（已知未生效，未修）

```
HTTP/2 200          ← 不是 301，且**没有 Location 头**
content-type: text/html; charset=utf-8
server: cloudflare
apex sha256(前16)=a02e24a38f9d5dc5     www sha256(前16)=a02e24a38f9d5dc5   → SAME content
```

⇒ `www` 直接 **200** 并服务与 apex **完全相同**的内容（sha256 相同），**没有发生 301** ——
与「CF 不支持域级 `_redirects`，需在 CF 控制台建 Redirect Rule」的已知状态一致。我**没有**做任何修改尝试。

---

## 4. 执行者自述与我的实测**不一致**之处

| # | 执行者自述 | 我的实测 | 判定 |
|---|---|---|---|
| 1 | 「提交 1：**父 `035e083`**，**18 文件 +681/−119**」 | 真父 = **`e99da22`**；`git show --shortstat 0e5fcf8` = **17 files, +248/−119**。`035e083..0e5fcf8` 区间 = 18 文件/+681，因为**区间里还有我 3 个文档提交**（该 doc 恰好 +433 行；248+433=681、17+1=18） | ⚠️ **量错了对象**（提交内容无误，是统计口径写成「提交 1」） |
| 2 | 「陈旧字节数已清空/不再展示」 | 我抓到的那一刻：常量清了、但**页面 10 处仍在展示 0.8.30 的真实 MiB**（§1.3）；已修，我重跑 = **0 条** | ⚠️ **曾不一致 → 已复验通过** |
| 3 | 「122 → 68，差 54，因为撤掉 pinned URL 与 og-image 文件名」（初版） | `0.8.30`@`035e083`：pinned 行 **65**、og 行 **4**；**65+4=69 ≠ 54** ⇒ 初版分解不成立。`ops` 已更正为「按行两态同口径」，我复核**吻合**：`65+4+53=122` → `0+4+64=68`，Δ **−65/+0/+11 = −54** ✅ | ⚠️→✅ **初次不一致，更正后逐项吻合** |
| 4 | 「`CHANGELOG` 已写」 | 我抓到的那一刻还是 `0.8.30`；复验：顶部 `## 0.8.31`、`grep -c` = 1 | ⚠️ **曾不一致 → 已复验通过** |
| 5 | 「已跑 `gen-site-jsonld.py`」 | 副本 `gen` 产物与仓库 **4/4 逐字节 IDENTICAL** | ✅ 一致（机制级验证，不是看它怎么说） |
| 6 | 「已跑 `gen-site-geo.py`」 | 副本产物与仓库 **4/4 逐字节 IDENTICAL** | ✅ 一致 |
| 7 | 「版本号 bump 到 0.8.31」 | 6 条断言 + `Cargo.toml`/`tauri.conf`/`package.json`/`Cargo.lock` 全 `0.8.31`；`--locked` EXIT=0 | ✅ 一致 |
| 8 | 「`PUBLISHED=False`」 | 两个脚本都是 `False` | ✅ 一致 |
| 9 | 「pinned 直链 0 / 无 404 风险」 | `releases/download` = 0；pinned URL `curl -sI` = 404（预期，且页面不指向它） | ✅ 一致 |
| 10 | 「`site/**` 内 `0.8.30` = 0」 | **0** | ✅ 一致 |
| 11 | 「删了旧 og-image」 | 是 **rename** 换掉的（`git show --stat` 显示 `{og-image-0.8.30.png => og-image-0.8.31.png}`），仓库内对旧名的引用 = 0 | ✅ 一致（我上一版文档的措辞已更正） |
| 12 | 「wasm 盲区 / `Cargo.lock` 盲区」 | 我独立确认：`grep wasm scripts/check.sh` = 0；6 条断言不含 `Cargo.lock` | ✅ 一致 |
| 13 | 「3 个资产已发布」 | `gh release view` = 3 个资产、`state=uploaded`、`isDraft=false`；我**自己下载**了三个并自己算哈希 | ✅ 一致 |
| 14 | 「dmg / zip 的字节数」（页面与 `gh` 元数据） | 我**自己 `stat`**：dmg **47,179,066**、zip **42,681,220**、SHA256SUMS **200** —— 与页面、与 `gh api` digest 三方一致 | ✅ 一致 |
| 15 | 「`SHA256SUMS.txt` 可用」 | 我**自己 `shasum -a 256`**：dmg/zip 两行 **MATCH**（文件里带 `./` 前缀也没挡住） | ✅ 一致 |
| 16 | 「`app_update_check` 通过」 | 我跑**真实链路**：下载 → 校验（期望=实际=`10b278f3…`）→ 解压 → 包内 0.8.31，**EXIT=0**；**我遇到的 exit 28 次数 = 0**（第 1 次尝试即成功） | ✅ 一致（且我额外记录了**我自己的**超时次数这个独立对象） |
| 17 | 「站点已填真实字节数」 | 线上页面印的精确字节 = `47,179,066` / `42,681,220` —— **与我下载算出的逐位相同**；旧的 `47,154,951`/`42,659,730` = **0 次** | ✅ 一致 |
| 18 | 「`PUBLISHED=True` + pinned 直链」 | `400d15c` 上两个开关都 `True`；3 条 pinned 直链 `curl` 全部 **200**；JSON-LD `downloadUrl` 已 pinned 且可达 | ✅ 一致 |
| 19 | 「线上版本号计数回升」 | 我实测：`/` 与 `/en/` 各 **0.8.31 ×35、0.8.30 ×0**；`site/**` 内 `0.8.31` = **122**（提交 1 是 68） | ✅ 一致（**注明：122 是提交 2、68 是提交 1，两个对象**） |
| 20 | 「线上已部署」 | `/` 与 `git show 400d15c:site/index.html` **逐字节 IDENTICAL**；`/en/` 同样 | ✅ 一致（比计数更强） |

**我查过、且一致的其他项**：`releases`=200 / `releases/latest`=302→v0.8.30 / `gh release view`=not found（提交 1 时）；
wasm 两页手工 v0.8.31；OG 图存在且被引用；`Cargo.lock` 5 个 crate 0.8.31、`--locked` EXIT=0；
提交 2 的两个生成器产物 **8/8 逐字节 IDENTICAL**；`www` 200 无跳转。

---

## 5. 我**无法**验证的（如实列出，不含糊）

1. **真机安装**：把 dmg 拖进 `/Applications`、走一遍 Gatekeeper / 右键打开的**真实**用户路径。
   我做了 `hdiutil attach` + 读了包内结构（`Applications` 软链 + `XrayTun.app`）与 Info.plist，
   但**没有**在真机上双击安装、也没有以普通用户身份打开过它。
2. **自动更新的最后一步（替换 + 重启）**：`app_update_check` 把「查版 → 下载 zip → 校验 SHA256SUMS →
   解压 → 核对包内版本」整条链路跑通了（EXIT=0），但**故意不做替换**；
   「替换 `/Applications` 里的 app 并重启」这一段**我没有验**。
3. **公证 / Developer ID**：本项目是 **adhoc** 签名。我实测包内 app 的 `codesign --verify --strict` **EXIT=0**，
   但那只能说明**包内签名自洽**，**推不出**「用户不会看到 Gatekeeper 警告」。
4. **`.dmg` 本体未签名**（我实测 `codesign` EXIT=1、`code object is not signed at all`）：
   这**符合**页面「没有签名校验」的声明，但我**不能**据此判断「用户双击时会不会被拦」。
5. **用户浏览器下载后的隔离属性**：我实测包内 quarantine = **0**，但我的取件路径是 `gh release download`（curl 路线），
   **浏览器下载会由 macOS 自己加 quarantine** —— 这条用户路径我**验不了**（见 §3.2 的口径说明）。
6. **CI 在冻结修订上是真的通过**：`Release` workflow 我实测 **completed/success**，但 `CI` 那条我当时读到的是
   `completed success`（同一 push），而 `check.sh` 我按纪律**没重跑**；「本机 `check.sh` 绿」只有 lead 的引用。
7. **`www` 的 301 最终生效**：需用户在 CF 控制台建 Redirect Rule，**我无法在仓库侧验证**（此刻仍是 200 直出）。
8. **Cloudflare Pages 的部署事件**：我看不到部署动作，只能靠 `curl` 取到「部署后的内容」这一事实
   （我用「线上 ⇄ 提交逐字节相同」来替代部署事件本身）。
9. **StoreKit / 更新 UI 的交互闭环**：只验了后端链路，界面上的「检查更新」按钮点击流转**没验**。

## 6. 诚实清单：哪些是我实测、哪些是引用

**我自己跑出来的（可复现）**：§1.1 六条断言 + 其余版本源 + `Cargo.lock`/`--locked`（含中间态对照）；
§1.2 的 `grep` + `curl -sI` 状态码 + `gh release view`；§1.3 的 10 处陈旧字节数、GitHub API 的 0.8.30 真实字节数、本地无产物、修复后复验；
§1.4 的 wasm 手工读数与 `check.sh` 零覆盖；§1.5 的 122 复算与 68 逐文件计数 + 对 `ops` 解释的算术核验；
§1.6 CHANGELOG 复验；§1.7 的 og-image rename 认定；§2 两个生成器的逐字节比对与 JSON-LD 直读；
§0 对 `ops` 18 文件/+681 的口径核验；
**§3 全部由我自己下载/自己算**：三个资产的 sha256 与字节数（并与 `gh api` digest 交叉核对）、`SHA256SUMS.txt` 逐行 MATCH、
dmg/app 的 `codesign` 退出码与 adhoc 签名详情、Info.plist 版本、挂载点内与下载文件自身的 xattr 计数、
`app_update_check` 整条链路与我自己的 exit 28 次数、线上 `/` 与 `/en/` 的计数/canonical/hreflang、与提交 `400d15c` 的**逐字节**比对、
线上精确字节数与我下载值的一致性、3 条 pinned 直链的状态码、提交 2 的两个生成器 8/8 逐字节比对、
CF SPA 软 404 三条 URL 的 body 哈希、`www` 状态码与内容哈希。

**我只能引用的（标注来源与不可验证性）**：
* lead 在 `035e083` 上跑过「完整门禁 → `✓ 与 CI 相同的全部检查通过`，exit 0」——**引用 lead 的话，我未重跑**。
* `ops` 的「提交 1 / 提交 2」流程与根因归因（0.8.30 phase-2 脚本只换文案）——**引用**；我用实测核它的**后果**。
* 「历史基线：13 个文件带隔离属性」——**引用**卡内描述；我实测的是 **0**（§3.2），但**取件路径不同**（见 §3.2 口径）。
* 「上一版 `app_update_check` 第一次 exit 28」——**引用**卡内描述；**我遇到的次数是 0**（§3.3），两者是不同对象。
* `Release` / `CI` workflow 的结论来自 `gh run list`（**平台读数，不是我的实测**），我只把它当「引用」。

---

## 附：本次用到的产物

* 一次性复现脚本（只读）：`/tmp/verify-commit1.sh` → 输出 `/tmp/verify-commit1.out`（提交 1，104 行）
* 第二阶段复现脚本（只写 /tmp）：`/tmp/verify-release.sh` → 输出 `/tmp/verify-release.out`
* 我自己下载的资产：`/tmp/xt-release-v0.8.31/`（dmg / zip / SHA256SUMS.txt，**我算的哈希见 §3.1**）
* 提交 2 的干净 worktree：`/tmp/xt-c2`（`400d15c`）；生成器副本重跑：`/tmp/c2-gen/`
* 副本 JSON-LD 验证：`/tmp/jsonld-test/`；副本 GEO 验证：`/tmp/geo-test/`
* 线上页面原文：`/tmp/live2-https___xraytun_top__.html`、`/tmp/live2-https___xraytun_top_en__.html`
* 本文件是**唯一**被本卡写入的仓库文件（`docs/verification/RELEASE-v0.8.31.md`）
