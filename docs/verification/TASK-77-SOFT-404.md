# task-77：CF Pages 未知路径返回 200 + index.html（软 404）—— 取证、判定与处置

> 结论一句话：**站点没有任何前端路径路由，也没有任何页面依赖这个兜底**；
> 兜底是「CF Pages 没有顶层 `404.html` 时的默认行为」这一**纯副作用** ⇒ 加 `site/404.html` 让未知路径返回**真 404**。

状态标记：`实测` = 我用命令跑出来的原始输出；`读码` = 读仓库源码得出的结论；`推断` = 未直接实测。

---

## 1. 修前现场（**不可复制的现场**，务必留档）

现场时间：2026-09-21（`site/404.html` 部署**之前**）。判据 = **content-type + body sha256 双判**，
**不用裸状态码** —— 状态码恰恰是会被这个兜底骗到的东西。完整原始输出在
`/tmp/ops-404-probe-before.txt`（脚本 `/tmp/ops-404-probe.sh`），关键读数如下（`实测`）：

```
apex https://xraytun.top        首页 / body sha256 = a02e24a38f9d5dc5 （43980 B）
  路径                             状态  content-type                    字节     body sha256(前16)
  /                               200  text/html                        43980   a02e24a38f9d5dc5
  /en/                            200  text/html                        45083   6cc1dd39d7fbfed8
  /wasm/                          200  text/html                        25471   c7806ade85bbb9eb
  /en/wasm/                       200  text/html                        27200   7692442981fd2374
  /assets/site.css                200  text/css                         10707   b7f9ffadf0af4b30
  /og-image-0.8.31.png            200  image/png                        58984   e87db07a8ec44b38
  /manifest.webmanifest           200  application/manifest+json          757   e66b8924dfb89163
  /robots.txt                     200  text/plain                        7004   0d76be21828ad01a
  /sitemap.xml                    200  application/xml                   1787   8f2ecf603d980c3a
  /llms.txt                       200  text/plain                        5026   7b4b3e7e80e0f6e2
  /llms-full.txt                  200  text/plain                       63437   f9f790ac36a34b45

  ---- 以下全是「软 404」：状态 200、content-type text/html、body 与首页逐字节相同 ----
  /og-image-0.8.30.png            200  text/html                        43696   1e12653843e9f902  ← 见下方注解
  /this-path-does-not-exist-xyz   200  text/html                        43980   a02e24a38f9d5dc5
  /foo/bar/baz                    200  text/html                        43980   a02e24a38f9d5dc5
  /old.html                       200  text/html                        43980   a02e24a38f9d5dc5
  /_redirects                     200  text/html                        43980   a02e24a38f9d5dc5
  /_headers                       200  text/html                        43980   a02e24a38f9d5dc5

镜像 https://harodggg.github.io/xrayTun   同一批路径 = 真 404（9379 B，GitHub 默认 404 页，sha b620507312c5e975）
```

两条注解（`实测` + `推断`）：

1. `/_redirects`、`/_headers` 返回 200 + 首页，与仓库里 `site/_redirects` 的注释一致
   （那两份 CF 配置文件不会被当静态资源提供）。
2. `/og-image-0.8.30.png` 的 body 是 **43696 B / sha `1e12653843e9f902`**，**不是**当前首页
   （43980 B / `a02e24a3`）—— 它是**提交 1 那次部署的 index.html** 被 CF 边缘缓存住的那一份：
   该路径命中 `site/_headers` 的 `/og-image*.png → max-age=31536000, immutable`，
   于是「软 404 的 HTML」被当成图片**缓存了一年**。
   **推断**：加 `404.html` 后，这个**具体 URL** 仍可能从边缘缓存返回 200 + HTML，直到缓存过期或被 purge
   （我们的 token 没有 purge 权限）；而**从未被请求过**的未知路径应当立刻变真 404。
   部署后实测两条都记录（见 §4）。

## 2. 关键问题：这个兜底被有意依赖吗？—— **没有**

### 2.1 读码结论（`读码`）

| 检查项 | 结果 |
|---|---|
| `site/assets/site.js`（站点唯一的脚本，108 行） | 只读 `document.documentElement.lang` 与 `localStorage`，可选地 fetch GitHub API 取最新 tag；**无 `location.pathname` / `location.search` / `location.hash` / `history.*` / `pushState` / `URLSearchParams` / 路由表** |
| 4 个页面（`/`、`/en/`、`/wasm/`、`/en/wasm/`）的内联脚本 | 只有 `application/ld+json` 数据块（不可执行），**没有**任何内联可执行脚本 |
| `<base>` 标签 | 全站 **0 处** |
| `site/_redirects` | **零规则**（文件里写明「故意不含任何规则」；SPA 兜底不是它配的） |
| 站点结构 | 4 个真实目录页，站内导航全是 `<a href>` + `#锚点` |

### 2.2 更强的证据（`实测`）：**没有任何链接需要兜底**

把 4 个页面里的**站内相对链接**逐条解析到 `site/` 文件系统：

```
四个页面里的站内相对链接共 46 条；解析后在 site/ 里找不到对应文件/目录的：0 条
⇒ 所有站内链接都指向真实存在的文件/目录（不需要任何兜底路由）
```

这条把结论从「我没找到路由代码」升级为「**即使有路由，也没有任何链接依赖它**」。

⇒ 兜底是**纯副作用**，按 task-77 第一节的判据，应当让未知路径返回**真 404**。

## 3. 处置

* 新增 `site/404.html`（自包含：样式内联、图标 data URI、**不写任何版本号**、`noindex`）。
  三条约束与原因写在文件头部注释里 —— 其中最重要的一条：404 页会被**任意深度**的路径命中，
  所以**不能引用相对路径资源**，站内链接的根由页面内脚本探测（CF 根 `/`；GH Pages 镜像 `/xrayTun/`）。
* `site/robots.txt` 的 CF 托管段字节数注释改为**不再陈述成当前事实**（改的是生成器
  `scripts/gen-site-geo.py` 那一行，再重跑生成器 —— 只改产物会被下次生成静默覆盖）。

### 3.1 第一版被**两个部署 workflow 拒绝**（真实事件，留档）

第一版 `404.html` 的站内链接写成了**根绝对路径**（`href="/"`、`href="/en/"`、`href="/wasm/"`），
想让「禁 JS 时在 canonical 站上也是对的」。结果 `0dccb3d` 推上去后
**Cloudflare Pages 与 GitHub Pages 两个部署都失败**（run `35595485637` / `35595485638`），
失败原因是仓库自己的静态检查 —— 它比我的设计更权威：

```
$ grep -rInE '(href|src)="/([^/]|$)' site --include='*.html'      # pages.yml / cloudflare-pages.yml 同一条
site/404.html:152:  <a class="btn btn--primary" data-site-link="" href="/">回到首页</a>
site/404.html:153:  <a class="btn btn--secondary" data-site-link="en/" href="/en/" …>English home</a>
site/404.html:158:  <li><a data-site-link="wasm/" href="/wasm/">xray-wasm…</a></li>
```

那条检查存在的理由正是**镜像站跑在 `/xrayTun/` 子路径下，根绝对路径会指到站点外面**
（`pages.yml` 的注释里写着）。所以**不是绕过检查，而是我的写法违反了一条真实约束**。
修正：`href` 改为**相对值**（`./`、`en/`、`wasm/`），仍由脚本按探测到的根改写；
无 JS 时的降级如实写进了文件头注释（单段未知路径正确；多段路径落回上一级）。

事故记录（不放任它变成空白）：这次红是**我的新文件引入的**，两次部署都失败，
窗口内 `xraytun.top` 仍是上一版内容（软 404 依然存在）；修正后重推才真正生效。

## 4. 部署后实测

部署：`8f943d5`（Cloudflare Pages run `35598158663` **success**、Pages run `35598158679` **success**）。
判据同上：**content-type + body sha256**。原始输出 `/tmp/ops-404-probe-after.txt`。

### 4.1 同一批路径：修前 → 修后（`实测`）

| 路径 | 修前（apex） | 修后（apex） | 修后（镜像） |
|---|---|---|---|
| `/` | 200 text/html `a02e24a3…` | 200 text/html `a02e24a3…`（**未变**） | 200（同） |
| `/en/` | 200 `6cc1dd39…` | 200 `6cc1dd39…`（**未变**） | 200（同） |
| `/wasm/`、`/en/wasm/` | 200 | 200（未变） | — |
| `/assets/site.css` | 200 text/css `b7f9ffad…` | 200 `b7f9ffad…`（**未变**） | — |
| `/og-image-0.8.31.png` | 200 image/png `e87db07a…` | 200 `e87db07a…`（**未变**） | 200（同） |
| `/manifest.webmanifest` | 200 manifest+json | 200（未变） | — |
| `/sitemap.xml`、`/llms.txt`、`/llms-full.txt` | 200 | 200（sha 全部未变） | — |
| `/robots.txt` | 200 text/plain 7004 B | 200 text/plain **7215 B**（注释加了 2 行） | — |
| `/this-path-does-not-exist-xyz` | 200 text/html **= 首页** | **404** `6db7d559…`（9183 B） | **404** 同一份 |
| `/foo/bar/baz` | 200 text/html **= 首页** | **404** `6db7d559…` | **404** 同一份 |
| `/old.html` | 200 text/html **= 首页** | **404** `6db7d559…` | 404 |
| `/_redirects`、`/_headers` | 200 text/html **= 首页** | **404** `6db7d559…` | 404 |
| `/og-image-0.8.30.png` | 200 text/html `1e12653843e9f902`（43696 B） | **仍 200 / 同一 sha** ← 见 4.3 | 404 |

* 线上 404 页 body 与仓库 `site/404.html` **逐字节相同**（9183 B，`cmp` 通过）。
* **所有真实页面与静态资源 sha256 与修前完全一致** ⇒ 这次改动**没有碰到任何正常内容**。
* 深链锚点：`/#faq`、`/en/#faq` 均 200（fragment 不发往服务器）。
* `/404.html` 直接访问：apex **308 → `/404`**（CF Pages 会剥掉 `.html` 后缀）→ 200，
  内容仍是本页；镜像直接 200。

### 4.2 真浏览器验证根路径探测（`实测`）

用 CDP 在 headless Chrome 里打开**线上**的 404 页（不是本地副本），读运行时 `href`：

```
✓ apex 单段   /this-path-does-not-exist-xyz   → ["/","/en/","/wasm/"]
✓ apex 深层   /foo/bar/baz                    → ["/","/en/","/wasm/"]
✓ 镜像 单段   /xrayTun/this-path-…            → ["/xrayTun/","/xrayTun/en/","/xrayTun/wasm/"]
✓ 镜像 深层   /xrayTun/foo/bar/baz            → ["/xrayTun/","/xrayTun/en/","/xrayTun/wasm/"]
  （四例的 title/h1 与 `robots=noindex, follow` 均正确；无页面错误日志）
```

⇒ 探测在**真实响应**上成立（镜像与 canonical 都命中，深层路径也对）。

### 4.3 残留：一个 URL 仍返回「200 + HTML」（**如实报告，不是已修好**）

`https://xraytun.top/og-image-0.8.30.png` 修后**仍然 200 + text/html**。机制已实测清楚：

```
$ curl -sSI https://xraytun.top/og-image-0.8.30.png
HTTP/2 200
cache-control: public, max-age=31536000, immutable
age: 7323                       ← 约 2 小时前被缓存
cf-cache-status: HIT            ← 命中**边缘缓存**

$ curl -sSI 'https://xraytun.top/og-image-0.8.30.png?bust=1'
HTTP/2 404
cache-control: no-store
cf-cache-status: BYPASS         ← 绕过缓存打到源站 = **源站已经是 404**
```

⇒ 源站行为**已经正确**（未知路径 404）；这一个 URL 是**部署前的软 404 HTML 被当成图片缓存了一年**
（`site/_headers` 的 `/og-image*.png → immutable`）。清掉它需要在 Cloudflare 层面
**purge 缓存**（或等 immutable 过期）—— 仓库里没有 CF 缓存清理权限，**本卡不假装它已归零**。

影响面（`实测`）：同一批里只有**它**残留 —— `/old.html`、`/foo/bar/baz`、`/_redirects`、
`/_headers` 都已变真 404（它们的响应没有 immutable 规则，缓存可重新验证）。

## 5. 验证与诚实清单

* **勘误**：上一个提交（`c1d74f9`）的提交信息标题写了「18 条路径读数」，实际是 **23 条**
  （apex **17** 条 + 镜像 **6** 条，见 `/tmp/ops-404-probe-before.txt`）。提交信息已推送，
  **不改写历史**，在此更正 —— 本文件里的表格本身就是准确的那一份。
* **两个部署 workflow 的静态检查**（必需文件 / `href|src="/…"` / CSS `url(/…)` /
  `en/index.html` 不用 `assets/`）已**逐字复刻**在本地跑过一遍 → 全部 ✓，
  再推的（见 §3.1：第一版就是被它拦下的）。
* `check.sh` 的 6 条站点版本断言：在**隔离 worktree**（HEAD + 本卡改动，不含他人在途改动）里跑
  完整 `./scripts/check.sh --no-release-build` → **exit 0**，其中「站点版本一致性」步骤全绿
  （前端 197 passed + 1 todo；Rust 全部通过；CSS token 检查绿）。
  为什么用 worktree：共享工作区里 `apps/ui/src/pages/Settings.tsx` 有他人未提交的在途改动，
  在共享工作区跑会把那份 WIP 一起编译/测试 —— 那是别人的提交面，不该由我引入变量。
* `404.html` 的根路径探测脚本：用 `node` + `vm` 把页面里**那段真实脚本**抽出来跑了 5 个场景
  （CF 根 `/unknown`、CF 深层 `/foo/bar/baz`、镜像 `/xrayTun/unknown`、镜像深层 `/xrayTun/a/b/c`、
  探不到 manifest → 回退 `/`），全部得到期望的链接前缀。
* `scripts/gen-site-geo.py` 改完**重跑生成器两次**，`site/robots.txt` 的 sha256 前后相同（幂等），
  且 `git status` 里**只多出这一处产物差异**（没有第二处）。
* §1 的资料来自 `xraytun.top` 与 GitHub Pages 镜像的**实测**；§2.1 是**读码**；§2.2 是**实测**；
  §1 注解 2 的「旧图 URL 可能仍被边缘缓存」是**推断**，部署后以实测替换。
* CF 部署传播延迟：部署后我会**分两次**（刚上完 / 等一分钟）重复探测，若两次不一致会如实写出，
  不把「传播中的中间态」当成最终结论。
