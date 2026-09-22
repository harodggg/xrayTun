# v0.8.32 发布验收（独立验证）

> 角色：**独立验证者**。`lead`/`ops` 给的数字只能当「待我核对的转述」——
> 下面每条都是**我自己跑出来的原始输出**。判据照 v0.8.31 那一套。
>
> 被测对象（我实测）：
> * tag `v0.8.32` → **`485d7e1366408ed0200f9dd1c2ff642e91020d36`**（对象 `a3fa218`，与 lead 报的一致）
> * 站点「提交 2」= **`3f974de`**（`docs(site): v0.8.32 已发布 —— 填真实资产数据`）
> * `gh release view v0.8.32` → `isDraft=false`、`isPrerelease=false`、`publishedAt=2026-09-21T12:51:19Z`、**3 个资产**
> * 本机 `HEAD` = `ff1fc9c`（lead 的现场留档提交）

---

## 1. 三个资产：我自己下载、我自己算（**三方对照**）

**命令**：`gh release download v0.8.32 --repo harodggg/xrayTun -D /tmp/xt-release-v0.8.32` + `shasum -a 256` + `stat -f %z`

**原始输出**：

```
-- 我下载到的文件 --
200        SHA256SUMS.txt
47204857   XrayTun_0.8.32_x86_64_arm64.dmg
42704806   XrayTun_0.8.32_x86_64_arm64.zip

-- 我自己算的 sha256 --
936ec5c3a625651c34ec27930cd8239116fc408c2784b3c82bd803df2fcdd990  SHA256SUMS.txt
efa87c0161e2b04748777d09d60987a900636922f11ab27035345126a6b2808a  XrayTun_0.8.32_x86_64_arm64.dmg
811b23df048bfa3a4c7cc2df155546c447b127d206566ce95b81e98d2a18ef67  XrayTun_0.8.32_x86_64_arm64.zip

-- SHA256SUMS.txt 原文（带 ./ 前缀）--
efa87c01…  ./XrayTun_0.8.32_x86_64_arm64.dmg
811b23df…  ./XrayTun_0.8.32_x86_64_arm64.zip

-- 逐行比对 --
  MATCH   XrayTun_0.8.32_x86_64_arm64.dmg
  MATCH   XrayTun_0.8.32_x86_64_arm64.zip

-- gh api 的 digest（第三方）--
SHA256SUMS.txt                   size=200       digest=sha256:936ec5c3a625651c34ec27930cd8239116fc408c2784b3c82bd803df2fcdd990
XrayTun_0.8.32_…dmg              size=47204857  digest=sha256:efa87c0161e2b04748777d09d60987a900636922f11ab27035345126a6b2808a
XrayTun_0.8.32_…zip              size=42704806  digest=sha256:811b23df048bfa3a4c7cc2df155546c447b127d206566ce95b81e98d2a18ef67
```

| 检查项 | 我实测 | 与 lead 转述一致？ |
|---|---|---|
| dmg 字节 / sha256 | **47,204,857** / `efa87c0161e2b047…a6b2808a` | ✅ 逐位一致 |
| zip sha256 | `811b23df048bfa3a…8d2a18ef67` | ✅ 一致 |
| `SHA256SUMS.txt` 字节 | **200** | ✅ 一致 |
| 我的 sha vs 文件里写的 | dmg/zip **双 MATCH** | ✅ |
| 我的 sha vs `gh api` digest | 三个全同 | ✅ **三方一致** |

## 2. 包内完整性

```
$ codesign --verify --strict XrayTun_0.8.32_x86_64_arm64.dmg
"code object is not signed at all"                     EXIT = 1      ← .dmg 容器未签名（**预期**）

$ hdiutil attach -nobrowse -readonly …                 EXIT = 0
  内容：Applications -> /Applications（软链） + XrayTun.app

$ codesign --verify --strict XrayTun.app
  "valid on disk" + "satisfies its Designated Requirement"      EXIT = 0
  Identifier=com.xraytun.desktop   Format=Mach-O universal (x86_64 arm64)
  CodeDirectory flags=0x2(adhoc)   Signature=adhoc   TeamIdentifier=not set

$ PlistBuddy Info.plist
  CFBundleShortVersionString = 0.8.32      CFBundleVersion = 0.8.32

xattr：挂载点内 8 个文件 → 带 com.apple.quarantine = 0 ；带任意 xattr = 0
下载到 /tmp 的三个文件自身：只有 com.apple.provenance（.dmg 另有 com.apple.diskimages.recentcksum）
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `.dmg` 容器 `codesign` | **EXIT=1**（`not signed at all`） | ✅ **预期**（打包脚本只签 App） |
| 包内 `XrayTun.app` | **EXIT=0**（valid on disk / satisfies its Designated Requirement） | ✅ |
| 签名类型 | **adhoc**，`TeamIdentifier=not set` | ✅ 与页面「没有签名校验」一致 |
| 包内版本 | `0.8.32` / `0.8.32` | ✅ |
| quarantine / 任意 xattr | **0 / 0** | ✅ 符合历史基线 0 |

## 3. `app_update_check`（真实链路）

```
=== 1) 查最新版 ===   === 2) 下载 === 10%…100% ✓
=== 3) 校验 ===
  期望 811b23df048bfa3a4c7cc2df155546c447b127d206566ce95b81e98d2a18ef67
  实际 811b23df048bfa3a4c7cc2df155546c447b127d206566ce95b81e98d2a18ef67   ✓ 校验通过
=== 4) 解压并核对包内版本 ===
  包内版本 0.8.32，release 声称 0.8.32    ✓ 一致
✓ 整条自更新链路（除最后的替换）全部走通
app_update_check EXIT = 0        尝试次数 = 1 ; exit 28（超时）次数 = 0
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| 退出码 | **0** | ✅ |
| 包内版本 | **0.8.32** | ✅ |
| 校验和 | 期望 = 实际 = `811b23df…`（与我在 §1 自己算的**同一个值**） | ✅ |
| **我遇到的 exit 28** | **0 次**（第 1 次尝试即成功） | ✅（这是**我**的对象；lead/ops 遇到几次是另一个对象） |

## 4. 线上站点

```
$ curl -s https://xraytun.top/      → 43980 bytes   0.8.32 ×35   0.8.31 ×0   0.8.30 ×0   「正在发布」×0
$ curl -s https://xraytun.top/en/   → 45083 bytes   0.8.32 ×35   0.8.31 ×0   0.8.30 ×0
  canonical: /        → https://xraytun.top/          og:url 同
  canonical: /en/     → https://xraytun.top/en/       og:url 同
  hreflang 三向：zh-Hans / en / x-default 齐全（wasm 两页用自己的 pair）

$ diff <线上 /> <git show 3f974de:site/index.html>      → **IDENTICAL（逐字节）**
$ diff <线上 /en/> <git show 3f974de:site/en/index.html> → **IDENTICAL（逐字节）**

站点印的精确字节数：47,204,857 与 42,704,806
我在 §1 自己 stat 的：47,204,857 与 42,704,806        ← **完全一致**
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `0.8.32` 次数 | `/` **35**、`/en/` **35** | ✅ |
| `0.8.31` / `0.8.30` / 「正在发布」 | **0 / 0 / 0** | ✅ 没有残留、也没有停在发布态 |
| canonical | `/`→apex、`/en/`→apex`/en/`（og:url 同） | ✅ |
| 线上 vs 站点提交 `3f974de` | **逐字节 IDENTICAL**（两个页面） | ✅ 比计数更强 |
| **站点印的字节 vs 我实测** | `47,204,857` / `42,704,806` **逐位相同** | ✅ |
| `releases/latest` | → `tag/v0.8.32` | ✅ |

> ⚠️ 口径：与 `485d7e1`（**tag** 指向的提交）比会 DIFFERS —— 因为那是**提交 1** 的站点
> （`正在发布`×2、`0.8.32`×20）；线上已经是**提交 2 `3f974de`**。**别拿 tag 比站点**。

## 5. ★ 本轮两个新行为（重点）

### 5.1 未知路径现在是**真 404**（双判，不用裸状态码）

```
首页兜底指纹：/ → 200 · 43980 B · sha256 3c720dce2d5d785c…

路径                              裸码  content-type                  字节    sha256(前16)
/this-path-does-not-exist-xyz     404   text/html; charset=utf-8       9183   6db7d5598f0705ac
/foo/bar/baz                      404   text/html; charset=utf-8       9183   6db7d5598f0705ac
/old.html                         404   text/html; charset=utf-8       9183   6db7d5598f0705ac
/og-image-0.8.32.png              200   image/png                    59376   （真资产）
```

* 三条未知路径 **404**，body 是同一个 **9183 B / `6db7d559…`** 的 `404.html`（不是首页、不是软 404）；
* 我起了 `scripts/verify-live-site.sh` 独立复核：**§1「未观察到 SPA 兜底」**、
  §6 镜像同路径也是 404 ⇒ **修好了**（对比 v0.8.31 时同路径是 `200 + text/html + body==首页`）。

### 5.2 `www.xraytun.top` 的 301 已生效，且**保留路径与查询串**

```
/               → 301  Location: https://xraytun.top/
/en/            → 301  Location: https://xraytun.top/en/
/wasm/          → 301  Location: https://xraytun.top/wasm/
/en/wasm/       → 301  Location: https://xraytun.top/en/wasm/
/llms.txt       → 301  Location: https://xraytun.top/llms.txt
/index.html?x=1 → 301  Location: https://xraytun.top/index.html?x=1     ← **查询串也保留**
apex：/ 200、/en/ 200、/wasm/ 200、/llms.txt 200                        ← 未受影响
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| www `/`、`/en/`、`/wasm/`、`/en/wasm/`、`/llms.txt` | **全 301 且路径原样保留** | ✅ 与 task-27 的验收要求一致 |
| 查询串 | `/index.html?x=1` → 保留 `?x=1` | ✅ 额外核过 |
| apex 是否受影响 | 4 条路径仍 **200** | ✅ |

### 5.3 ⚠️ 但「删除 ≠ 不可达」：边缘缓存仍会返回**已删除的旧图**（本轮新发现）

`_headers` 里 `/og-image*.png → max-age=31536000, immutable`，于是**从仓库删掉的旧图仍在 CF 边缘缓存里**：

```
/og-image-0.8.31.png      → 200 · image/png · 58984 B · sha e87db07a8ec44b38… · cf-cache-status: HIT
/og-image-en-0.8.31.png   → 200 · image/png · 42263 B（同上机制）
/og-image-0.8.30.png      → 200 · text/html · 43696 B · sha 1e12653843e9f902…（更早那次留下的「软 404 HTML」被当图片缓存）
```

对照仓库：`485d7e1` 已把 `og-image-0.8.31.png` **改名**成 `og-image-0.8.32.png`（58984→**59376** B）、
`og-image-en-0.8.31.png` 删除并新增 `og-image-en-0.8.32.png`；当前页面只引用 `0.8.32` 那两张。

⇒ **结论**：站点**内容**没有问题（页面只引用新图）；但**用状态码/URL 可达性判断「旧资产是否还在」在 `og-image*.png` 这一类路径上不可靠**
—— 缓存的 `max-age=31536000, immutable` 让被删的文件在边缘继续 200 一年，直到 CF purge 或过期。
这是**部署/缓存层**的观察，不是本次发布内容的缺陷；**要清掉需要在 CF 侧 purge**（仓库没有该权限）。
我的 `scripts/verify-live-site.sh` 因此报了 2 处 ✗（§2b 的严格判据），退出码 1 —— 我按「观察项」记录，**不当作发布阻断**。

## 6. 执行者自述 vs 我的实测

| # | 自述 | 我的实测 | 判定 |
|---|---|---|---|
| 1 | Release workflow success、tag → `485d7e1` | `git rev-parse v0.8.32^{}` = `485d7e1366408ed0200f9dd1c2ff642e91020d36` | ✅ |
| 2 | dmg 47,204,857 / `efa87c01…` | 我自己 stat/shasum = **同** | ✅ |
| 3 | zip `811b23df…` | 我自己 shasum = **同** | ✅ |
| 4 | SHA256SUMS 200 B | 我自己 stat = **200** | ✅ |
| 5 | 站点印的字节 47,204,857 / 42,704,806 | 线上页面逐位读到 **同两个数**，且等于我下载实测 | ✅ |
| 6 | 未知路径真 404 | 三条路径 **404** + `404.html` 体（9183 B） | ✅ |
| 7 | www 301 已生效 | 6 条 URL **全 301 且保留路径/查询** | ✅ |
| 8 | `isDraft=false`、3 个资产 | `gh release view --repo …` = `isDraft=false`、`assets|length` = **3**，size 三者与我实测一致 | ✅ |

**无不一致之处。** 唯一我要额外点出的是 §5.3 那条边缘缓存残留（自述里没提，我也没把它算成缺陷）。

## 7. 我**无法**验证的

1. **真机安装**（拖 dmg 进 `/Applications`、Gatekeeper 的真实体验）—— 只做了 `hdiutil attach` + 读包内结构；
2. **自动更新的最后一步**（替换 `/Applications` 里的 App 并重启）—— `app_update_check` **故意不做替换**；
3. **公证 / Developer ID**：本项目是 adhoc 签名，`codesign --verify --strict` EXIT=0 只说明**包内签名自洽**，
   推不出「用户不会看到 Gatekeeper 警告」；
4. **用户浏览器下载路径的 quarantine**：我的取件是 `gh`（curl 路线），浏览器下载**会**由 macOS 自己加隔离属性；
5. **helper 侧修复是否真的在真机生效** —— 这是本轮最要紧的未验项：本机特权 helper 仍是 **9月13** 的构建
   （`/Library/Application Support/com.xraytun.helper/com.xraytun.helper`，6,976,288 B，mtime 9月13 20:37），
   而 App 包已是新的。**需要用户更新到 0.8.32 并重装助手、重连后**，我才能只读采集路由表验证
   `127 → 网关` 是否消失（lead 已安排，我的 before 基线在 `/tmp/diag-final.txt`）。
6. **CF 侧 purge / 缓存策略**：我只能在边缘观察「返回了什么」，看不到 purge；
7. **GitHub Pages 镜像的部署时机**：只把它当对照，不当失败判据。

## 附：原始产物

* 资产下载与我算的哈希：`/tmp/xt-release-v0.8.32/`
* 发布核验原始输出：`/tmp/v83.out`（本文件 §1–§3 的来源）
* 双判脚本输出：`/tmp/vls-32.out`（§5 的独立复核）
* 线上页面原文：`/tmp/live32-__`（`/`）、`/tmp/live32-_en__`（`/en/`）
* 本文件是本次验收**唯一**写入的仓库文件。
