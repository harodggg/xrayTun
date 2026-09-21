# 线上站点验收：为什么「HTTP 200」不能用来判资产存在（task-79）

> 交付物：`scripts/verify-live-site.sh`（**只读、可反复跑**）+ 本说明。
> 触发它的教训来自 v0.8.31 发版验收：`https://xraytun.top/` 上**不存在的路径也返回 200 + 首页 HTML**
> （Cloudflare Pages 的 SPA 兜底），于是我们一直在用的「状态码 ⇒ 资产存在」判据在 apex 上**会给出假绿**。
> 本项目的做法是**教训变成机制**：这份脚本 + 下面这条敏感性验证就是那次教训的产物。

## 1. 判据（**双判，不看状态码**）

| 观察 | 判定 |
|---|---|
| `404` / `410` | **不存在**（正常） |
| `200` + content-type **与期望族不符**（例：要 `image/png` 却拿到 `text/html`） | **软 404 / 不存在**（不是「存在」） |
| `200` + `text/html` + body sha **== 首页兜底指纹** | **软 404 / 不存在** |
| `200` + 期望的 content-type + 不是兜底体 | **真存在** |

### ⚠️ 比卡面更强的一条（我实测到，所以写进了实现）

卡面假设「兜底体 == 当前首页」。**实际不是**：

```
当前首页              /                         200 text/html 43980 B sha a02e24a38f9d5dc5…
被删的 og-image-0.8.30.png                    200 text/html 43696 B sha 1e12653843e9f902…   ← **不等于首页**
随机路径 /foo/bar/baz2                        200 text/html 43980 B sha a02e24a38f9d5dc5…   ← 等于首页
```

`1e126538…` 是**上一版部署（提交 1 时期）的 index.html**，被 CF 边缘缓存住了 ——
那条路径命中了 `site/_headers` 里 `/og-image*.png → max-age=31536000, immutable`，
于是这份「软 404 HTML」被**当成图片缓存了一年**。

⇒ **只比「当前首页指纹」会把这个 URL 误判成「存在」**。所以脚本的判据顺序是
**先看 content-type 族**（图片路径拿到 `text/html` 本身就已判死），再用指纹辅助，
并且把遇到的**每一个**兜底指纹都收集起来（不假设只有一个）。

## 2. 敏感性验证（本卡存在的理由）：同一路径，两种判据给出相反结论

**修前现场（2026-09-21 19:09 CST，CF 兜底仍在生效）**，`/tmp/sens-evidence.txt` 原文：

```
### 裸判据（只看状态码）
$ curl -s -o /dev/null -w "%{http_code}" https://xraytun.top/og-image-0.8.30.png
  → 200                      ← 裸判据：**存在**
$ curl -s -o /dev/null -w "%{http_code}" https://xraytun.top/foo/bar/baz2
  → 200                      ← 裸判据：**存在**

### 双判据（content-type + body sha256）
$ https://xraytun.top/og-image-0.8.30.png
  HTTP 200 · content-type: text/html; charset=utf-8 · 43696 bytes · sha256 1e12653843e9f902
$ https://xraytun.top/foo/bar/baz2
  HTTP 200 · content-type: text/html; charset=utf-8 · 43980 bytes · sha256 a02e24a38f9d5dc5

### 首页指纹（基准）
https://xraytun.top/ → sha256 a02e24a38f9d5dc5 (43980 bytes)
```

**脚本自己的判据（同一时刻，同一路径，脚本 §2b 的原始输出）**：

```
[2b] 上一版的产出物**必须已经不存在** —— 这一节同时就是**敏感性验证**：
     裸状态码在 apex 上会被软 404 兜底骗成「存在(200)」，双判才是对的。
-- 上一版中文 OG 图：https://xraytun.top/og-image-0.8.30.png （期望：**不存在**）
      → HTTP 200 · content-type: text/html; charset=utf-8 · 43696 bytes · sha256 1e12653843e9f902…
     $ curl -s -o /dev/null -w '%{http_code}' https://xraytun.top/og-image-0.8.30.png   →  **200**
       裸状态码判据会说：「存在」；本脚本双判说：见下
  ✗ 上一版中文 OG 图：**软 404/不存在** —— 裸状态码 200（会被误判成「存在」），
     但 content-type 是 text/html（期望 png），body 43696 bytes / sha 1e12653843e9f902…
      （新的兜底指纹，且 != 当前首页 ⇒ 边缘缓存里的陈旧 index.html）
（上一版英文 OG 图同样：裸判据 200，双判 = 软 404/不存在）
```

| 判据 | `og-image-0.8.30.png`（确实已被删除） | `/foo/bar/baz2`（确实不存在） |
|---|---|---|
| **裸状态码** | 200 ⇒ 「存在」❌ | 200 ⇒ 「存在」❌ |
| **本脚本双判** | **软 404/不存在** ✅ | **软 404/不存在** ✅ |

⇒ **裸判据会被骗，本脚本不会被骗 —— 这就是这张卡要证明的事。**

> **诚实说明**：上面是**修前窗口**的现场。`ops` 的 task-77 要加 `site/404.html`；若它落地，
> 未知路径会变真 404，本节头两行就不再复现（那时敏感性验证只能靠这类**仍被边缘缓存**的旧路径，
> 或靠 `MIRROR` 对照）。脚本对**两种行为都正确**：真 404 判「不存在」，软 404 也判「不存在」。

## 3. 脚本怎么用

```bash
VER=0.8.31 PREV=0.8.30 ./scripts/verify-live-site.sh        # 版本号**不写死在脚本里**
./scripts/verify-live-site.sh --ver 0.8.31 --prev 0.8.30
ALLOW_STALE_LATEST=1 ./scripts/verify-live-site.sh          # 发布前演练：releases/latest 允许还没更新
SKIP_MIRROR=1 ./scripts/verify-live-site.sh                 # 跳过 GitHub Pages 镜像对照
```

* 覆盖：首页兜底指纹 → **软 404 兜底是否生效（显式 WARNING）** → 真资产必须真存在（`og-image-*.png`/`robots.txt`/`sitemap.xml`/`llms*.txt`）
  → **上一版产出物必须已消失（= 敏感性验证）** → 4 个页面的版本号计数（目标/上一版）+ canonical + hreflang（**wasm 两页是另一个 pair**）+ og:url
  → `www` 状态码与 `Location`（**只报不判失败**）→ `releases/latest` 的最终 tag → GitHub Pages 镜像对照。
* 每项都打印**实际命令与原始值**（不只有 `✓/✗`）。
* 退出码：`0` 全通过、`1` 发现问题、`2` 参数错误。
* **只读**：不写仓库、不动站点；临时文件都在 `mktemp` 目录。
* `bash -n scripts/verify-live-site.sh` 通过。

## 4. 本期实跑结果（2026-09-21 19:1x，修前窗口）

| 区块 | 结果 |
|---|---|
| [0] 首页指纹 | ✓ `43980 B / a02e24a38f9d5dc5…` |
| [1] 兜底是否生效 | **WARNING**：随机路径 200 + `text/html`，body == 当前首页 ⇒ **兜底正在生效** |
| [2] 真资产 | ✓ 6/6（`og-image*.png` = `image/png`；`robots.txt`/`llms*.txt` = `text/plain`；`sitemap.xml` = `application/xml`） |
| [2b] 上一版产出物应消失 | ✗ **2 处**（`og-image-0.8.30.png`、`og-image-en-0.8.30.png` 仍被边缘缓存成软 404 HTML）—— **修前窗口的预期结果，也是敏感性证据** |
| [3] 页面计数/canonical/hreflang/og:url | ✓ 4/4 页面；`0.8.31`×35（首页/en 首页）、×1（wasm 两页）；`0.8.30`×0 |
| [4] `www` | 本次为 **HTTP 301 → https://xraytun.top/**（**这条曾经未生效，现在好了**；本项只报不判失败） |
| [5] `releases/latest` | ✓ → `tag/v0.8.31` |
| [6] 镜像对照 | ✓ 镜像对不存在路径 **真 404**（与 apex 的 200 形成对照） |
| **总结** | `发现 2 处问题（另有 3 条 warning）`，退出码 **1** —— 这 2 处就是 §2 的两个软 404 |

> 脚本在我自己发现并修掉几处实现 bug 后才到这个状态（`$VAR：` 全角冒号会被 bash 吞进变量名、
> 首页与自己比指纹会自判软 404、`releases/latest` 的网络失败被误报成「指向错」）—— 都已在最终版里修掉。

## 5. 诚实清单

**这些检查依赖 `xraytun.top` 的**实际响应**，会随站点变化**：首页指纹、（因此）软 404 判定、
各页面版本号计数、`www` 状态码、`releases/latest` 的 tag。站点一改，这些数就变 —— 所以脚本**不写死版本号**，
且每次都打印原始值。

**对 GitHub Pages 镜像**不适用 / 需注意**：镜像**没有 SPA 兜底**，未知路径是真 404；
所以镜像上「状态码判据」是可用的 —— **本脚本的 `[6]` 恰恰用这个差异来证明「兜底只发生在 apex」**。
镜像也可能落后于 apex（部署时机不同），脚本只把它当**对照**、不作为失败判据。

**本脚本没有覆盖**（要别的办法）：
1. **真机安装**（拖 dmg 进 `/Applications`、Gatekeeper 真实体验）；
2. **自动更新的替换+重启**那一步（只有 `app_update_check` 那条链路可跑）；
3. **包的哈希/签名**（那是发版验收的 `codesign`/`shasum` 一摊，不是线上站点的事）；
4. **CDN 缓存本身的健康**（例如「某路径被缓存了多久」）—— 本脚本只能观察到「缓存里现在是什么」；
5. **站点内容是否正确/可读**（只比版本号出现次数与 canonical，不做语义检查）；
6. `/en/` 与 `/` 的**内容等价性**（只校验 canonical/hreflang，不比对正文）。

## 附

* 修前现场证据：`/tmp/sens-evidence.txt`（§2 的两种判据原文）
* 脚本完整输出：`/tmp/vls-run.out`
* 本说明与脚本是 task-79 的交付物；**未改** `site/**`、`scripts/check.sh`、`apps/**`
