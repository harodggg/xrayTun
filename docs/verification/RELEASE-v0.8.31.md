# v0.8.31 发布验收（独立验证 · task-76）

> **角色**：发版的**独立验证者**。`ops` 执行发布（task-74 两阶段），我负责**不信它的自述，自己跑一遍**。
> 本文件里每一条结论都带「**我跑的命令**」与「**原始输出**」；凡是我没亲自跑出来的，一律进 §6 并标注来源。
> 不接受转述、不接受截图。
>
> **纪律**：本卡只写本文件（与 `/tmp` 下的临时脚本）；**未改** `site/**`、`Cargo.toml`、`apps/**`、`scripts/**`；
> **未跑** `check.sh`（改用定向命令 + 自己复现它的断言）。

## 0. 修订与口径（先说清「我量的是哪个对象」）

| 对象 | 修订/状态 | 说明 |
|---|---|---|
| v0.8.31 冻结修订 | `035e083` | lead 在此跑过完整门禁（我按纪律**未重跑** `check.sh`） |
| **提交 1**（版本 bump + 官网「正在发布」） | **工作区，`git log -1` 仍为 `035e083`，尚未提交** | 下面 §1 的所有读数都取自**验证时刻的工作区**，时间见各条 |
| 提交 2 / 资产 / 线上 | 尚未发生（`gh release view v0.8.31` → `release not found`） | §3 待做 |

> ⚠️ 因为提交 1 **当时还没提交**，我在 `17:5x` 抓到过一处「`Cargo.lock` 仍是 0.8.30、`cargo metadata --locked`
> exit 101」的中间态，`ops` 随后自行重生成（现已 0.8.31 ×5、`--locked` exit 0）。⇒ 这属于**时间差**，不是缺陷，
> 但它正好说明：**任何一条读数都必须绑定「哪一瞬间、哪个对象」**。等提交 1 落成 commit 后我会重跑一遍并改绑修订号。

---

## 1. 提交 1 的验证：版本号 + 「正在发布」态

### 1.1 版本号六处 + Cargo.lock

**我跑的命令**（逐条 `sed`/`grep`，**没用** `check.sh`）：

```bash
want="$(sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml | sed -n 's/^version *= *"\([^"]*\)".*/\1/p' | head -1)"
sed -n 's/^XRAYTUN_VERSION *= *"\([^"]*\)".*/\1/p' scripts/gen-site-jsonld.py | head -1
sed -n 's/^VERSION *= *"\([^"]*\)".*/\1/p'          scripts/gen-site-geo.py | head -1
sed -n 's/.*PAGE_VERSION *= *"\([^"]*\)".*/\1/p'    site/assets/site.js | head -1
sed -n 's/^SITE_VERSION *= *"\([^"]*\)".*/\1/p'     scripts/gen-site-images.py | head -1
sed -n 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' site/index.html | head -1
sed -n 's/.*XrayTun_\([0-9.]*\)_x86_64_arm64\.dmg.*/\1/p' site/en/index.html | head -1
grep -n "0\.8\.3[0-9]" Cargo.toml apps/desktop/tauri.conf.json apps/ui/package.json Cargo.lock
```

**原始输出**：

```
want(Cargo.toml workspace)=0.8.31
--- 1 gen-site-jsonld.py ---  0.8.31
--- 2 gen-site-geo.py ---     0.8.31
--- 3 site/assets/site.js --- 0.8.31
--- 4 gen-site-images.py ---  0.8.31
--- 5 site/index.html dmg --- 0.8.31  (3 条下载链接)
--- 6 site/en/index.html dmg - 0.8.31  (3 条下载链接)
Cargo.toml:12:version = "0.8.31"
apps/desktop/tauri.conf.json:4:  "version": "0.8.31",
apps/ui/package.json:4:  "version": "0.8.31",
```

| 检查项 | 原始输出 | 结论 | 与执行者自述一致？ |
|---|---|---|---|
| `Cargo.toml`（真源） | `version = "0.8.31"` | ✅ | 一致 |
| `apps/desktop/tauri.conf.json` | `"version": "0.8.31"` | ✅ | 一致 |
| `apps/ui/package.json` | `"version": "0.8.31"` | ✅ | 一致 |
| `gen-site-jsonld.py` `XRAYTUN_VERSION` | `0.8.31` | ✅ | 一致 |
| `gen-site-geo.py` `VERSION` | `0.8.31` | ✅ | 一致 |
| `gen-site-images.py` `SITE_VERSION` | `0.8.31` | ✅ | 一致 |
| `site/assets/site.js` `PAGE_VERSION` | `0.8.31` | ✅ | 一致 |
| `site/index.html` 下载文件名 | `0.8.31` ×3 | ✅ | 一致 |
| `site/en/index.html` 下载文件名 | `0.8.31` ×3 | ✅ | 一致 |
| `Cargo.lock`（**不在 check.sh 的 6 条里**） | `xraytun-desktop`/`xt-core`/`xt-helper`/`xt-proto`/`xt-tun` = **0.8.31 ×5**；`0.8.30` 计数 **0**；`cargo metadata --locked --offline` → **EXIT=0** | ✅（见 §0 的中间态说明） | 一致 |

> `check.sh` 的 6 条断言的**精确范围**（我读了 `scripts/check.sh:140-143`）：
> `gen-site-jsonld.py:XRAYTUN_VERSION`、`gen-site-geo.py:VERSION`、`site.js:PAGE_VERSION`、
> `gen-site-images.py:SITE_VERSION`、`site/index.html` 与 `site/en/index.html` 的 dmg 文件名。
> ⇒ **它不覆盖 `Cargo.lock`，也不覆盖 wasm 两页**（见 §1.4）。

### 1.2 「正在发布」态：pinned 链接与状态码

**我跑的命令**：

```bash
grep -rn "releases/download" site/ | wc -l
grep -rn "releases/download/v0\.8\.31" site/ | wc -l
for u in ...; do curl -sI -o /tmp/h.txt -w "%{http_code}" "$u"; done
gh release view v0.8.31
```

**原始输出**：

```
site/** 里 "releases/download" 出现次数        = 0
site/** 里 "releases/download/v0.8.31" 次数     = 0
https://github.com/harodggg/xrayTun/releases                                              -> 200
https://github.com/harodggg/xrayTun/releases/latest                                       -> 302 location: .../releases/tag/v0.8.30
https://github.com/harodggg/xrayTun/releases/download/v0.8.31/XrayTun_0.8.31_x86_64_arm64.dmg -> 404
gh release view v0.8.31 -> release not found
```

| 检查项 | 原始输出 | 结论 |
|---|---|---|
| 页面里有**任何** pinned 直链？ | `releases/download` 出现 **0** 次 | ✅ 一个都没有 ⇒ 本阶段不会点进 404 |
| 会 404 的那条 URL（预期失败项，如实列出） | pinned dmg URL → **404** | ✅ **本阶段预期**；且**页面没有指向它**，所以自洽 |
| `releases`（页面实际指向的链接） | **200** | ✅ |
| `releases/latest` | **302 → `releases/tag/v0.8.30`** | ✅ 事实正确：0.8.31 还没发布，latest 仍是 0.8.30 |
| 资产是否已存在 | `gh release view v0.8.31` → `release not found` | ✅ 与「正在发布」一致 |

### 1.3 ⚠️ **发现一处不一致：仍在展示上一版（0.8.30）的真实字节数**

`ops` 报的是「陈旧字节数已清空」。**生成器常量确实清空了，但页面与 `llms-full.txt` 仍在展示上一版的数字。**

**我跑的命令**：

```bash
grep -rn "45\.0 MiB\|40\.7 MiB" site/
curl -s "https://api.github.com/repos/harodggg/xrayTun/releases/tags/v0.8.30" | python3 -c "..."
find . -name "*.dmg" -o -name "XrayTun*.zip" | grep -v node_modules
grep -n "^DMG_BYTES\|^ZIP_BYTES" scripts/gen-site-geo.py
```

**原始输出**：

```
site/index.html:250:  >下载 macOS 版 v0.8.31 · dmg · 45.0 MiB</a>
site/index.html:254:  >备用 .zip · 40.7 MiB</a>
site/index.html:463:  >下载 macOS 版 v0.8.31 · dmg · 45.0 MiB</a>
site/en/index.html:251:  >Download for macOS v0.8.31 · dmg · 45.0 MiB</a>
site/en/index.html:255:  >Alternative .zip · 40.7 MiB</a>
site/en/index.html:493:  >Download for macOS v0.8.31 · dmg · 45.0 MiB</a>
site/llms-full.txt:39: [下载 macOS 版 v0.8.31 · dmg · 45.0 MiB](...releases) [备用 .zip · 40.7 MiB](...)
site/llms-full.txt:147,473,581: 同上（en 两条）
（共 10 处：index×3 / en×3 / llms-full×4）

v0.8.30 真实资产（GitHub API）：
  XrayTun_0.8.30_x86_64_arm64.dmg: 47154951 bytes = 45.0 MiB
  XrayTun_0.8.30_x86_64_arm64.zip: 42659730 bytes = 40.7 MiB

本地 0.8.31 构建产物：find 结果为空（没有任何 .dmg/.zip）
scripts/gen-site-geo.py: DMG_BYTES, DMG_MIB = "", ""   ← 常量已清空 ✅
```

**为什么这是不一致（证据自己闭环，不依赖外部知识）**：

1. 页面上的 **45.0 / 40.7 MiB 与 v0.8.30 的真实资产字节数逐位吻合**（45.0 = 47154951 B，40.7 = 42659730 B）。
2. `gh release view v0.8.31` = `release not found`、本地**无任何 0.8.31 产物** ⇒ 0.8.31 的字节数此刻**不可能被测过**。
3. 项目**自己**的规则禁止这种写法：
   * `scripts/gen-site-geo.py` 文件头 30–31 行：「提交 1 … **不沿用上一版的字节数（那是错的）**」；
   * `site/index.html:468`：「真实资产（v0.8.31）：**正在发布**，资产发布后本表填入**真实字节数**」（下载表本身确实已按此写，✅）；
   * `site/llms.txt:26`：「**不在这里预先写死字节数或直链 —— 发布前它们还不存在**」——
     而 `llms-full.txt:39` 恰恰写死了字节数。
4. `site/index.html:257` 的 meta 还写着「大小以 Releases 页面为准」，与按钮上印着的具体 MiB 自相矛盾。

**结论**：这是「**正在发布**态页面上的一处陈旧数字**」，属 `site/**`，我**只报告不改**（已发给 `ops`，建议去掉 CTA 文案里的 `· 45.0 MiB` / `· 40.7 MiB` 并重跑 `gen-site-geo.py`）。
`check.sh` 的 6 条断言论**不会**发现它（它们只比对版本号字符串，不比对字节数）——这是本卡的第二个盲区。

### 1.4 ⚠️ 已知盲区复核：`check.sh` 对 **wasm 两页零覆盖**

**我跑的命令**：

```bash
grep -n "wasm" scripts/check.sh          # ← 空
grep -n "0\.8\." site/wasm/index.html site/en/wasm/index.html
```

**原始输出**：

```
(grep wasm scripts/check.sh → 无任何匹配)
site/wasm/index.html:217:  XrayTun 的当前版本是另一个序列（<strong>v0.8.31</strong>，见<a href="../">首页</a>）。
site/en/wasm/index.html:224: (<strong>v0.8.31</strong>, see the <a href="../../">home page</a>).
```

| 检查项 | 手工读数 | 结论 |
|---|---|---|
| wasm 两页的 XrayTun 版本引用 | zh `v0.8.31`、en `v0.8.31` | ✅ **本次是对的** |
| wasm 页自身的版本（xray-wasm） | `0.7.0`（共 20+ 处） | ✅ 不该跟着 XrayTun 变 |
| 是否被自动断言覆盖 | `grep wasm scripts/check.sh` → **空**；6 条断言里没有 wasm | ❌ **盲区仍在** |

> **单独点出**：`check.sh` 的 6 条断言**完全不覆盖** `site/wasm/index.html` 与 `site/en/wasm/index.html`。
> 这两页里唯一跟随 XrayTun 发版的字符串就是上面那两行 `v0.8.31`；**下次发版没人守它**，
> 漏改不会有任何门禁报警。本次是我**手工**核出来的。

### 1.5 `site/**` 里 0.8.30 残留计数（逐项对照 ops 基线 122）

**我跑的命令**（工作区 + 用 `git grep` 在 `035e083` 上复算基线）：

```bash
grep -ro "0\.8\.30" site/ | wc -l
grep -rc "0\.8\.30" site/ | grep -v ":0$"
git grep -c "0\.8\.30" HEAD -- site/
git grep -o "0\.8\.30" HEAD -- site/ | wc -l
```

**原始输出**：

```
〔工作区，提交 1 后〕site/** 里 "0.8.30" 总数 = 0（逐文件全部 0）

〔HEAD 035e083，复算 ops 的基线〕
HEAD:site/assets/site.js:1        HEAD:site/en/index.html:28
HEAD:site/en/wasm/index.html:1    HEAD:site/index.html:28
HEAD:site/llms-full.txt:23        HEAD:site/llms.txt:4
HEAD:site/wasm/index.html:1
总数 = 122
```

| 文件 | ops 报的基线 | 我在 `035e083` 复算 | 提交 1 后 | 说明 |
|---|---|---|---|---|
| `site/index.html` | 28 | **28** | 0 | 全清 |
| `site/en/index.html` | 28 | **28** | 0 | 全清 |
| `site/llms-full.txt` | 23 | **23** | 0 | 全清（**但见 §1.3：字节数没清**） |
| `site/llms.txt` | 4 | **4** | 0 | 全清 |
| `site/assets/site.js` | 1 | **1** | 0 | 全清 |
| `site/wasm/index.html` | 1 | **1** | 0 | 全清 |
| `site/en/wasm/index.html` | 1 | **1** | 0 | 全清 |
| **合计** | **122** | **122 ✅ 完全吻合** | **0** | 「任何一处变 0」在此**是预期**，逐项列出 |

### 1.6 ⚠️ 待跟进的第二处：`CHANGELOG.md` 仍停在 0.8.30

**我跑的命令**：

```bash
head -3 CHANGELOG.md
git status --porcelain CHANGELOG.md
git log --oneline -3 -- CHANGELOG.md
grep -rc "0\.8\.31" CHANGELOG.md
grep -rn "CHANGELOG" site/*.html site/en/*.html site/llms*.txt
```

**原始输出**：

```
# 更新记录
## 0.8.30            ← 最新条目，没有 0.8.31
git status CHANGELOG.md → 空（提交 1 未改它）
72b21fb v0.8.30：拓扑回流/蓝车漂移 + …（历史上每次发版都改它）
73351a9 v0.8.29：接管默认路由前的端到端门禁…
29cb6f8 v0.8.28：会话泄漏自动清理并重试一次…
CHANGELOG.md 里 "0.8.31" 出现 0 次

站点指向它（用户在页面上看得到）：
site/index.html:58   "releaseNotes": ".../blob/main/CHANGELOG.md"
site/index.html:746  更新记录：<a href=".../CHANGELOG.md">CHANGELOG.md</a>
site/en/index.html:59,793 同上（英文）
site/llms.txt:46 / llms-full.txt:303,736 同上
```

**为什么值得盯**：站点 **JSON-LD 的 `releaseNotes` 与页面上「更新记录」链接都指向 `CHANGELOG.md`**。
提交 1 之后站点已写「v0.8.31」，但点进去的最新条目还是 **0.8.30**。
**若提交 2 也不补**，这次发版就会「官网说 0.8.31、更新记录说 0.8.30」。
（`ops` 可能本来就打算放在提交 2 —— 我**不断言**它漏了，只把它列为**待复核项**：
提交 2 落地后我会重新 `grep -c '0\.8\.31' CHANGELOG.md`，仍为 0 就是缺陷。）

---

## 2. JSON-LD 的决定性验证（本卡最容易悄悄漏掉的一处）

`ops` 报「已跑 `gen`」。我不采信，做了**机制级**验证：把脚本与站点复制到 `/tmp`，在副本里跑
**`gen`**，再把产物与仓库里的页面**逐字节**比对。若 `ops` 漏跑 `gen`（本项目栽过一次的那种），
产物就会与仓库页面不一致（JSON-LD 停在旧版本）。

**我跑的命令**（**副本**里跑，仓库只读）：

```bash
python3 scripts/gen-site-jsonld.py                 # A) 仓库上跑默认 check（只读）
cp scripts/gen-site-jsonld.py /tmp/jsonld-test/scripts/ && cp -R site /tmp/jsonld-test/site
cd /tmp/jsonld-test && python3 scripts/gen-site-jsonld.py gen     # B) 副本里强制重写
diff -q /仓库/site/<page> /tmp/jsonld-test/site/<page>            # 逐字节比对
```

**原始输出**：

```
〔A〕✓ index.html: JSON.parse ok · SoftwareApplication ok · FAQPage 16 条与页面逐条一致 · canonical/hreflang ok · 可见文本 13545 字
    ✓ en/index.html: … 27264 字
    ✓ wasm/index.html: SoftwareSourceCode ok · FAQPage 5 条 … 7617 字
    ✓ en/wasm/index.html: … 14400 字
    结论: 全部通过        CHECK_EXIT=0

〔B〕副本 gen 后：4 个页面全部「全部通过」，GEN_EXIT=0
    IDENTICAL  site/index.html
    IDENTICAL  site/en/index.html
    IDENTICAL  site/wasm/index.html
    IDENTICAL  site/en/wasm/index.html
```

**我直接读出来的 JSON-LD 值**（不是从脚本推的）：

```
site/index.html:54:  "softwareVersion": "0.8.31",
site/index.html:56:  "downloadUrl": "https://github.com/harodggg/xrayTun/releases"
site/en/index.html:55: "softwareVersion": "0.8.31",
site/en/index.html:57: "downloadUrl": "https://github.com/harodggg/xrayTun/releases"
```

**结论**：
* `gen` 的产物与仓库页面**逐字节相同** ⇒ 页面里的 JSON-LD **就是**当前生成器在该开关下的输出，
  **不存在漏跑 `gen` 导致的旧版本残留**（本次 `ops` 没有重犯那个错误）。
* `softwareVersion = "0.8.31"` ✅（不是 0.8.30 残留）。
* `downloadUrl` = Releases 页面（**非 pinned**）—— 与 `PUBLISHED=False` **一致**；
  若此刻写成 pinned 直链，我会在上面 §1.2 的 `curl` 里看到 404 且页面指向它。
* `PUBLISHED = False`（`scripts/gen-site-jsonld.py:42`、`scripts/gen-site-geo.py:37`）✅。
* 顺带核过页面资源：新 OG 图 `og-image-0.8.31.png` / `og-image-en-0.8.31.png` **存在**且被正确引用
  （`site/index.html:37,44`、`site/en/index.html:38`）；旧 `og-image-0.8.30.png` 仍在目录里但**无人引用**（死文件，非缺陷）。

---

## 3. 资产出现后的验证（**待做** —— `gh release view v0.8.31` 目前 `release not found`）

以下每条都要「我跑的命令 + 原始输出」，现在还没有对象可测，**先列清单**：

- [ ] `gh release view v0.8.31` 显示 **3 个资产**（dmg / zip / SHA256SUMS）
- [ ] **自己下载** dmg 与 `SHA256SUMS.txt` 到 `/tmp`，**自己 `shasum -a 256`**，与文件里的值比对；
      给出**我自己算出的字节数**（并与 §1.3 的旧值对比，确认站点数字被更新成真实值）
- [ ] `codesign --verify --strict` 的**退出码**
- [ ] `xattr -l`：历史基线 13 个文件带隔离属性 → 应为 **0**
- [ ] `app_update_check`：包内版本号（应 0.8.31）与 SHA256；**如实记录遇到几次 exit 28 超时**（上一版第一次超时过）
- [ ] 线上站点 `https://xraytun.top/`：`0.8.31` / `0.8.30` / 「正在发布」各自出现次数；canonical；`/en/` 是否等价
- [ ] `www.xraytun.top`：原始状态码（**已知未生效**，不修）
- [ ] **复核 §1.6**：提交 2 后 `grep -c "0\.8\.31" CHANGELOG.md` 是否已非 0（若仍为 0 = 官网说 0.8.31、更新记录说 0.8.30）
- [ ] 复核 §1.3 的 10 处 MiB：提交 2 时应变成 **0.8.31 的真实字节数**（而不是继续沿用 45.0 / 40.7）

**已提前测的对照基线（此刻线上还是旧版本，因为提交 1 还没部署）**：

```
curl -s https://xraytun.top/ → 43987 bytes
  0.8.31  出现 0 次
  0.8.30  出现 35 次
  正在发布 出现 0 次
canonical: <link rel="canonical" href="https://xraytun.top/" />
```

**`www.xraytun.top`（已知未生效，只报状态码，不修）**：

```
HTTP/2 200          ← 不是 301，且**没有 Location 头**
server: cloudflare
apex sha256(前16)=02958cc437382b91 bytes=43987
www  sha256(前16)=02958cc437382b91 bytes=43987   → SAME content
```

⇒ `www` 直接返回 **200** 并服务与 apex **完全相同**的内容（sha256 相同），**没有发生 301 跳转** ——
与「CF 不支持域级 `_redirects`，需在 CF 控制台建 Redirect Rule」的已知状态一致。我**没有**做任何修改尝试。

---

## 4. 执行者自述与我的实测**不一致**之处

| # | 执行者自述 | 我的实测 | 判定 |
|---|---|---|---|
| 1 | 「陈旧字节数已清空/不再展示」 | `*_BYTES/*_MIB` 常量**确实**清空，但**页面 10 处仍在展示 0.8.30 的真实 MiB**（§1.3） | ⚠️ **不一致**（部分正确）——已报 `ops` |
| 2 | 「已跑 `gen-site-jsonld.py`」 | 副本里 `gen` 产物与仓库页面**逐字节相同**，`softwareVersion=0.8.31`、`downloadUrl` 非 pinned | ✅ 一致（我做了机制级验证，不是看它说了什么） |
| 3 | 「122 处 0.8.30 残留已清」 | `git grep` 在 `035e083` 复算 = **122**（28/28/23/4/1/1/1），提交 1 后 = **0** | ✅ 一致 |
| 4 | 「版本号已 bump 到 0.8.31」 | 9 处全部 0.8.31（含 `Cargo.lock` ×5、`--locked` exit 0） | ✅ 一致 |
| 5 | 「已置 `PUBLISHED=False`」 | `gen-site-jsonld.py:42` / `gen-site-geo.py:37` 均为 `False` | ✅ 一致 |
| 6 | （未声称）pinned 直链 | `site/**` 里 `releases/download` = **0**；pinned dmg URL `curl -sI` = **404**（预期，且页面不指向它） | ✅ 自洽 |

**我查过、且一致的其他项**：`site/**` 里 `0.8.30` 计数归零（逐文件）；wasm 两页手工版本；OG 图存在且被引用；
`releases`=200 / `releases/latest`=302→v0.8.30 / `gh release view v0.8.31`=not found；`www` 200 无跳转。

---

## 5. 我**无法**验证的（如实列出，不含糊）

1. **真机安装**：把 dmg 拖进 `/Applications`、走一遍 Gatekeeper / `xattr -d com.apple.quarantine` 的真实体验。
   我只有命令行工具，**没有**在真机上双击安装过。
2. **真机自动更新全流程**：从旧版 → 检查更新 → 下载 zip → SHA256 校验 → 替换 → 重启。
   §3 的 `app_update_check` 只能验「包内版本号与哈希」这类静态事实，**验不了** UI 里的升级闭环。
3. **公证 / Developer ID**：本项目是 ad-hoc 签名（页面自己写明「没有签名校验」）。
   `codesign --verify --strict` 只能说明**包内签名自洽**，**不能**推出「用户不会看到 Gatekeeper 警告」。
4. **dmg 挂载后的目录内容**（是否含 `Applications` 快捷方式、是否真的能拖拽安装）——需要 `hdiutil attach` + 人工看。
5. **release.yml 的真实执行**：我按纪律**没跑** `check.sh`，也没触发 CI。
   「CI 会绿」是**引用**，不是我的实测。
6. **`www` 的 301 最终生效**：需要用户在 CF 控制台建 Redirect Rule，**我无法在仓库侧验证**。
7. **线上站点部署**：Cloudflare Pages 何时把提交 1/2 部署上线，我**看不到**部署事件；只能靠 `curl` 反复取事实。

## 6. 诚实清单：哪些是我实测、哪些是引用

**我自己跑出来的（可复现）**：§1.1 全部 9 处版本号 + `Cargo.lock`/`--locked`；§1.2 的 `grep` + `curl -sI` 状态码 + `gh release view`；
§1.3 的 10 处陈旧字节数 + GitHub API 的 0.8.30 真实字节数 + 本地无产物；§1.4 的 wasm 手工读数与 `check.sh` 零覆盖；
§1.5 的 122 逐项复算；§2 的 `gen` 逐字节比对与 `softwareVersion`/`downloadUrl` 直读；
§3 的线上基线计数与 `www` 状态码/内容哈希。

**我只能引用的（标注来源与不可验证性）**：
* lead 在 `035e083` 上跑过「完整门禁 → `✓ 与 CI 相同的全部检查通过`，exit 0」——**引用 lead 的话，我未重跑**（纪律要求不跑 `check.sh`）。
* `ops` 的「提交 1 / 提交 2」流程描述来自 task-74 与 `scripts/gen-site-geo.py` 的注释——**引用**，我用实测去核它的**后果**。
* 「历史基线：13 个文件带隔离属性」——**引用**卡内描述，等资产出现后我会用 `xattr -l` 自己数。
* 「上一版 `app_update_check` 第一次 exit 28 后重试成功」——**引用**卡内描述，`ops` 遇到的次数与我遇到的次数是**两个不同的对象**，我会分别记录。

---

## 附：本次用到的产物

* 副本 JSON-LD 验证：`/tmp/jsonld-test/`（`gen` 产物与仓库逐字节比对）
* 线上首页原文：`/tmp/live.html`
* 本文件是**唯一**被本卡写入的仓库文件（`docs/verification/RELEASE-v0.8.31.md`）
