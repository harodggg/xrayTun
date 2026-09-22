# v0.8.33 发布验收（独立验证）

> 角色：**独立验证者**。`lead`/`ops` 给的数字只当「待我核对的转述」——下面每条都是**我自己跑出来的原始输出**。
> 被测对象（我实测）：
> * tag `v0.8.33`，附注对象 **`bb0a9d88e4eea6046da16f04f9eecce3dc832824`** → commit **`4aee1d883b761015a43cea547be9d8646d1c56f6`**
> * 站点**提交 2** = **`bfab1c26`**（`docs(site): v0.8.33 已发布 —— 填真实资产数据`）
> * `gh release view --repo harodggg/xrayTun`：`isDraft=false`、`isPrerelease=false`、`publishedAt=2026-09-22T04:42:42Z`、**3 个资产**
> * Release workflow `35686354597`：`status=completed conclusion=success headSha=4aee1d88…`

---

## 1. 三个资产：我自己下载、我自己算（三方对照）

### 1.1 ⚠️ 先说**我这边的一次下载失败**（如实记，因为它是判据的一部分）

第 1 次用 `gh release download` 拿到的文件是**截断的**，日志里有网络错误：

```
read tcp 192.168.0.28:60275->20.205.243.168:443: read: operation timed out     （api.github.com GraphQL）
read tcp 198.18.0.1:62264->185.199.111.133:443: read: connection reset by peer （资产 CDN，源地址是隧道网段）
落盘：dmg 31,343,616 B（应为 47,230,254）、zip 31,017,344 B（应为 42,726,199）
```

**我是靠「先比字节数」发现它坏了的** —— 大小对不上就根本不用去算哈希。
第 2 次改用 `curl -L --retry 8 --retry-all-errors -C -`，**一次成功**（dmg 93.7s、zip 116.6s，均 http=200）。
⇒ 我的失败计数：**1 次失败（截断）+ 1 次成功**，`exit 28` **0** 次、`exit 56` **0** 次（我改用 curl 重试后没再撞上）。
`ops` 报它那边 curl 失败 2 次（1 次 exit 28 只收 4,781,025 B、1 次 exit 56）—— **那是它的对象，与我的次数分开记**。

### 1.2 我自己算的结果

```
$ shasum -a 256 ./*
bd2736c5cc5aed765256d169ee9eb42b9547a8dfe2e78ded43a536ca97b7281e  ./SHA256SUMS.txt
cc989f5e8413d86fd28da8ccd654bf888c593038e618836d6977bbf5866b3d07  ./XrayTun_0.8.33_x86_64_arm64.dmg
cda7cd6e97be7326c1c5709b0d29fc3ca03a3e4b8a0f38f16c642ef21bf9e191  ./XrayTun_0.8.33_x86_64_arm64.zip

$ stat -f "%z %N" ./*
200        SHA256SUMS.txt
47230254   XrayTun_0.8.33_x86_64_arm64.dmg
42726199   XrayTun_0.8.33_x86_64_arm64.zip

$ 逐行比对 SHA256SUMS.txt
  MATCH   XrayTun_0.8.33_x86_64_arm64.dmg
  MATCH   XrayTun_0.8.33_x86_64_arm64.zip

$ gh api repos/harodggg/xrayTun/releases/tags/v0.8.33 的 digest
XrayTun_0.8.33_x86_64_arm64.dmg  size=47230254  digest=sha256:cc989f5e…6b3d07
XrayTun_0.8.33_x86_64_arm64.zip  size=42726199  digest=sha256:cda7cd6e…bf9e191
SHA256SUMS.txt                   size=200       digest=sha256:bd2736c5…b7281e

$ 廉价确认远端总长（range 请求）
XrayTun_0.8.33_…dmg  content-range: bytes 0-0/47230254
XrayTun_0.8.33_…zip  content-range: bytes 0-0/42726199
```

| 检查项 | 我实测 | 与 lead/ops 转述一致？ |
|---|---|---|
| dmg 字节 / sha256 | **47,230,254** / `cc989f5e8413d86fd28da8ccd654bf888c593038e618836d6977bbf5866b3d07` | ✅ 逐位一致 |
| zip 字节 / sha256 | **42,726,199** / `cda7cd6e97be7326c1c5709b0d29fc3ca03a3e4b8a0f38f16c642ef21bf9e191` | ✅ 逐位一致 |
| `SHA256SUMS.txt` | **200** / `bd2736c5cc5aed765256d169ee9eb42b9547a8dfe2e78ded43a536ca97b7281e` | ✅ 逐位一致 |
| 我的 sha vs 文件里写的 | dmg/zip **双 MATCH** | ✅ |
| 我的 sha vs `gh api` digest | 三个全同 | ✅ **三方一致** |
| range 请求的远端总长 | `47230254` / `42726199` | ✅ 与落盘一致 |

⇒ **你的数字与我的不符这种事没有发生**；我不需要「以我的为准」改任何数。

## 2. 包内完整性

```
$ codesign --verify --strict XrayTun_0.8.33_x86_64_arm64.dmg
"code object is not signed at all"                                EXIT = 1   ← 容器未签名（**预期**）

$ hdiutil attach -nobrowse -readonly -mountpoint …                EXIT = 0
  内容：Applications -> /Applications（软链）+ XrayTun.app

$ codesign --verify --strict XrayTun.app
  "valid on disk" + "satisfies its Designated Requirement"         EXIT = 0
  Identifier=com.xraytun.desktop   Format=Mach-O universal (x86_64 arm64)
  CodeDirectory flags=0x2(adhoc)   Signature=adhoc   TeamIdentifier=not set

$ PlistBuddy Info.plist
  CFBundleShortVersionString = 0.8.33        CFBundleVersion = 0.8.33

xattr：挂载点内 8 个文件 → 带 com.apple.quarantine = 0 ；带任意 xattr = 0

$ app_update_check
  最新版 0.8.33  发布于 2026-09-22T04:42:42Z
  ✓ 校验通过        包内版本 0.8.33，release 声称 0.8.33    ✓ 一致
  ✓ 整条自更新链路（除最后的替换）全部走通          EXIT = 0
  尝试次数 = 1 ; exit 28 = 0 次 ; exit 56 = 0 次
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| `.dmg` 容器 `codesign` | **EXIT=1**（`not signed at all`） | ✅ 预期（打包脚本只签 App） |
| 包内 `XrayTun.app` | **EXIT=0**（valid on disk / satisfies its Designated Requirement） | ✅ |
| 签名类型 | **adhoc**、`TeamIdentifier=not set`、universal | ✅ 与页面「没有签名校验」一致 |
| 包内版本 | `0.8.33` / `0.8.33` | ✅ |
| quarantine / 任意 xattr | **0 / 0** | ✅ 符合历史基线 0 |
| `app_update_check` | **EXIT=0（第 1 次尝试即成功）**、包内 0.8.33、校验通过 | ✅ |

## 3. ★ 本版新增行为之一：站点上的字节数**不能是上一版的**

`ops` 本轮出过一次事故（bump 脚本输出管给 `tail`、吞掉 exit 1 ⇒ 页面一度出现「0.8.33 文件名 + **0.8.32 的真实字节数**」）。
我在**提交 2 `bfab1c2`** 上逐个查：

```
$ git grep -o "<数字>" bfab1c2 -- site/ | wc -l        # 旧版字节数（**必须全 0**）
  47,204,857（0.8.32 dmg） = 0 次
  42,704,806（0.8.32 zip） = 0 次
  47,179,066（0.8.31 dmg） = 0 次
  42,681,220（0.8.31 zip） = 0 次

$ 新版字节数
  47,230,254（0.8.33 dmg） = 5 次        42,726,199（0.8.33 zip） = 5 次

$ 线上首页印的精确字节数（我 curl 的原样）
  42,726,199   47,230,254                ← **与我自己下载实测的字节数逐位相同**
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| 0.8.32 的字节数（`47,204,857` / `42,704,806`） | `site/**` 里 **0 / 0** | ✅ 事故形态不存在 |
| 0.8.31 的字节数（`47,179,066` / `42,681,220`） | `site/**` 里 **0 / 0** | ✅ 更早的形态也不存在 |
| 0.8.33 的新字节数 | **5 / 5** 次 | ✅ 已写入 |
| 线上印的字节 vs 我下载实测 | `47,230,254` / `42,726,199` **逐位相同** | ✅ |

## 4. ★ 本版新增行为之二：`www.xraytun.top` 301 且**保留路径与查询串**

```
/                  → 301  Location: https://xraytun.top/
/en/               → 301  Location: https://xraytun.top/en/
/wasm/             → 301  Location: https://xraytun.top/wasm/
/en/wasm/          → 301  Location: https://xraytun.top/en/wasm/
/llms.txt          → 301  Location: https://xraytun.top/llms.txt
/index.html?x=1    → 301  Location: https://xraytun.top/index.html?x=1    ← 查询串保留
/foo/bar?q=2       → 301  Location: https://xraytun.top/foo/bar?q=2       ← 任意未知路径同样保留
apex：/ 200 、/en/ 200 、/wasm/ 200 、/llms.txt 200                        ← 未受影响
```

| 检查项 | 我实测 | 结论 |
|---|---|---|
| 5 条验收路径 + 2 条额外 | **全 301，路径与查询串原样保留** | ✅ |
| apex 是否受影响 | 4 条路径仍 **200** | ✅ |

## 5. 线上站点

```
$ curl -s https://xraytun.top/     → 43980 B   0.8.33 ×35   0.8.32 ×0   「正在发布」×0
$ curl -s https://xraytun.top/en/  → 45083 B   0.8.33 ×35   0.8.32 ×0
  canonical: / → https://xraytun.top/    /en/ → https://xraytun.top/en/

$ cmp <线上 /> <git show bfab1c2:site/index.html>        → **IDENTICAL（逐字节）**
$ cmp <线上 /en/> <git show bfab1c2:site/en/index.html>  → **IDENTICAL（逐字节）**

$ VER=0.8.33 PREV=0.8.32 bash scripts/verify-live-site.sh   → **EXIT=0，全部通过**
  §1 未观察到 SPA 兜底（未知路径 = 真 404）
  §2 真资产 6/6（og 图 image/png、robots/llms text/plain、sitemap application/xml）
  §2b 上一版 og 图 = **观察项（image/png 缓存残留），不阻断** ← 这正是 task 里那次改造的分支
  §3 四个页面 0.8.33×35（wasm×1）/ 0.8.32×0、canonical/hreflang/og:url 正确
  §5 releases/latest → tag/v0.8.33
```

> 口径：与 tag `4aee1d8`（**提交 1**，`正在发布` 态）比会 DIFFERS —— 线上是**提交 2 `bfab1c2`**。
> 与 v0.8.31/v0.8.32 两次一样，**别拿 tag 比站点**。

## 6. 执行者自述 vs 我的实测

| # | 自述 | 我的实测 | 判定 |
|---|---|---|---|
| 1 | dmg 47,230,254 / `cc989f5e…` | 我自己 stat/shasum = **同** | ✅ |
| 2 | zip 42,726,199 / `cda7cd6e…` | **同** | ✅ |
| 3 | SHA 200 B / `bd2736c5…` | **同** | ✅ |
| 4 | `isDraft: false`、3 资产 | `gh release view --repo …` = `isDraft=false`、资产 3 个、size 三者与我实测一致 | ✅ |
| 5 | tag `v0.8.33` → `4aee1d8`（对象 `bb0a9d88`） | `git rev-parse v0.8.33` = `bb0a9d88…`、`v0.8.33^{}` = `4aee1d88…`、类型 `tag` | ✅ |
| 6 | Release workflow success | `gh run view 35686354597` = `completed/success`，`headSha=4aee1d88…` | ✅ |
| 7 | 站点填了**本轮**真实字节 | `site/**`：新值 5/5 次、旧三版**全 0**；线上印的 = 我下载实测 | ✅ |
| 8 | `www` 301 且保留路径 | 7 条 URL 全 301 且路径/查询保留；apex 200 | ✅ |
| 9 | 线上与提交 2 一致 | `/` 与 `/en/` **逐字节 IDENTICAL** | ✅ |

**无不一致之处**（数字逐位相符）。

## 7. 我**无法**验证的

1. **真机安装/自动替换**：只做了 `hdiutil attach` + 读包内结构；更新流程的**最后一步（替换 `/Applications` 并重启）** `app_update_check` 故意不做；
2. **公证 / Developer ID**：adhoc 签名，`codesign --verify --strict` EXIT=0 只说明**包内签名自洽**；
3. **用户浏览器下载路径的 quarantine**：我是 `curl`/`gh` 取件，浏览器下载**会**由 macOS 自己加隔离属性；
4. **helper 侧修复是否真机生效**：`019ab73`（回滚恢复被顶掉的路由）在 **helper 侧** ⇒ 需用户重装助手；
   而且 `task-85` 只保证「今后不再产生空洞」，**已被旧版弄坏的机器不会自愈**（本机 `127/8 → lo0` 仍缺）。
   这条要等用户更新到 0.8.33 + 重装助手 + 重连后，我按 `LOOPBACK-ROUTE-BASELINE.md` 的 route-layer 判据采 after；
5. **CF 侧 purge / 缓存策略**：只能在边缘观察返回了什么；
6. **本轮 CI 的最终结论**：我只核了 Release workflow；其余 CI 未在我的读数里逐一确认。

## 附：原始产物

* 资产与我算的哈希：`/tmp/xt-release-v0.8.33/`（**重下后的完整文件**；截断的那份已删除）
* 发布核验原始输出：`/tmp/v833.out`（含**失败的那次**记录）＋ `/tmp/auc33-1.log`
* 双判脚本输出：`/tmp/vls-33.out`
* 线上页面原文：`/tmp/l33i`（`/`）、`/tmp/l33e`（`/en/`）
* 本文件是本次验收**唯一**写入的仓库文件。
