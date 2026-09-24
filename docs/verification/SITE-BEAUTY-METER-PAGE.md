# 官网新增「素颜镜 · 美颜程度检测」相关项目页（2026-09-24）

## 做了什么

在官网（`site/`）新增一对中英双语的相关项目页，并把一个 Chrome 扩展（素颜镜 · 美颜程度检测 v1.0.0）
**由本站直接分发**（zip 自带未压缩源码，不走 GitHub Releases —— 这个项目没有独立仓库）。

| 新增/改动 | 路径 |
| --- | --- |
| 中文页 | `site/beauty-meter/index.html` |
| 英文页 | `site/en/beauty-meter/index.html` |
| 下载包（74.2 KiB，25 个文件） | `site/beauty-meter/beauty-meter-extension-1.0.0.zip` |
| 许可证全文（MIT） | `site/beauty-meter/LICENSE` |
| 三张真实截图（由端到端脚本生成） | `site/beauty-meter/preview-{page,popup,options}.png` |
| 首页导航（中英） | `site/index.html`、`site/en/index.html` 各 +1 行 |
| 发现入口 | `site/sitemap.xml`（+2 条 `<url>`）、`site/llms.txt`（+1 个 `## ` 分节） |
| 响应头 | `site/_headers`（两个 HTML 短缓存、zip 长缓存、LICENSE 指定 `text/plain`） |
| JSON-LD 登记 | `scripts/gen-site-jsonld.py`（新增 2 个 `PAGES` 条目 + 4 个常量） |
| 部署门禁 | `.github/workflows/cloudflare-pages.yml`：要求 `beauty-meter/` 留在发现入口里、页面与下载件必须存在 |

## 页面内容的口径

* 三处必须说清楚的事实：① 它是**独立项目**（`must_contain` 由 JSON-LD 生成器强制）；
  ② **不联网、不上传图片、不带模型**；③ 分数是「疑似程度」，不是鉴定结论。
* 「验证状态」一节里的每个数字都来自项目自身的自动化验证（25 项单测 + 16 项真机端到端 +
  7 张真实原片标定 + 已知强度美颜阶梯 + 反例），没有写「应该能」。
* 「已知边界」照抄项目里的同一份清单：没有可信肤色时拒判或只给参考值、人脸占比 <4% 的远景被拒判、
  极深肤色可能走 partial、天生极白皙肤色带中等美白读数、几何形变不在范围内。
* 下载与许可证**指向本站自己的资产**（`/beauty-meter/beauty-meter-extension-1.0.0.zip`、
  `/beauty-meter/LICENSE`），不借用 XrayTun 的 dmg / LICENSE —— 结构化数据里也不能把
  别人的安装包说成这个扩展的下载地址。

## 验证（全部实跑）

| 检查 | 结果 |
| --- | --- |
| `python3 scripts/gen-site-jsonld.py check` | 8 个页面全部通过（新页含 4 块 JSON-LD：SoftwareApplication + FAQPage + WebSite + BreadcrumbList；FAQ 7 条与页面逐字一致；canonical / 三向 hreflang / meta description 与登记值一致） |
| 部署工作流的静态检查（本地复刻） | 必需文件齐、**无根绝对路径**（GH Pages 镜像会 404）、`jev-x-filter/` 与 `beauty-meter/` 均在发现入口 |
| 本地 HTTP 逐条拉取 | 两个新页面 + **页面内全部相对引用**（favicon / apple-touch-icon / manifest / site.css / 三张截图 / zip / 对方语言页）全部 200 且 content-type 正确；zip 75,943 字节与页面声明的 74.2 KiB 一致 |
| 下载包内容 | 顶层目录为 `beauty-meter-extension/`；`manifest.json` 为 MV3 v1.0.0；manifest 引用的每个文件都在包里；含未压缩源码 `src/common/beauty-engine.js`、`LICENSE`、`README.md` |
| `python3 -c "import yaml; yaml.safe_load(...)"` | 改动后的 `cloudflare-pages.yml` 解析通过 |
| 线上（部署后） | 见本文件「线上验收」一节 |

## 与另一条工作流的边界（重要，别踩）

第一次提交（`e79a735`）时，`site/llms-full.txt` 与 `scripts/gen-site-geo.py` 正被**另一条工作流**
（task-180：站点 GEO 生成器的「外部条目不许被静默抹掉」机制）改着且**未提交**，它的 `check`
当时还是红的。所以那次**没有动这两个文件**，发现入口只加在 `sitemap.xml` 与 `llms.txt`
（对方的机制按 `<loc>` / `## ` 分节逐字保留，实测一致）。

对方落地（`cb7c853`，其机制自测 `pass=21 fail=0`）之后，本页把 `llms-full.txt` 也补齐了：

* 索引表 +2 行（`- 中文：… → https://xraytun.top/beauty-meter/`、`- English: …`），
  计数行按对方口径从 `6 个，中英各 3` → **`8 个，中英各 4`**（4 个本产品页 + 4 条外部索引行）；
* 头部提示与正文分节放在对方的保留区间里（`external:header` / `external:body`），
  **不解析、不重排**；
* 复核：`python3 scripts/gen-site-geo.py check` 四份产物**逐字节一致**
  （`保留 4 条外部 <url>`、`保留 2 个外部 ## 分节`、`保留 4 条外部索引行`）；
* 对方的机制自测 `docs/verification/verify-gen-site-external.sh` 用 **beauty-meter 作第三个外部项目夹具**，
  我的改动后仍 `pass=21 fail=0`。

部署门禁随之收紧：`beauty-meter/` 现在要求在 **sitemap.xml / llms.txt / llms-full.txt 三个文件**里都被收录。

## 线上验收

推送 `e79a735` 后 Cloudflare Pages 部署成功（run 35974564225 / job `deploy` = success），
GitHub Pages 镜像同版本也成功。**只看内容，不看状态码**（apex 的 200 可能是兜底）：

| 检查 | 结果 |
| --- | --- |
| `https://xraytun.top/beauty-meter/` | 200 `text/html`，正文含页面专属标题，sha 与首页不同（不是兜底），含 canonical + JSON-LD |
| `https://xraytun.top/en/beauty-meter/` | 同上（英文页） |
| 下载件 `/beauty-meter/beauty-meter-extension-1.0.0.zip` | 200 `application/zip`，**75,943 字节**，与提交的 zip 逐字节一致 |
| `/beauty-meter/LICENSE` | 200 **`text/plain; charset=utf-8`**（`_headers` 覆写生效），正文是 MIT 全文 |
| 三张截图 | 200 `image/png`，字节数与提交一致 |
| `/sitemap.xml`、`/llms.txt`、首页中英导航 | 均含 `/beauty-meter/` 入口 |
| 正文完整性 | 线上 HTML 去掉 zone 级注入后与提交逐行一致（**唯一的差异是 Cloudflare Web Analytics 的 `beacon.min.js`**，zone 级注入，首页同样有，367 字节） |
| GitHub Pages 镜像（子路径 `/xrayTun/`） | 两个页面 200 且字节数与提交一致（31,205B / 32,393B），zip / PNG / LICENSE 也一致 —— 说明没有引入根绝对路径 |

对照组：`/beauty-meter/__does_not_exist__` 返回 **404**（本 zone 未开 SPA 兜底），因此上面的
「200 + content-type + 正文指纹」三重判据成立。

第二次部署（`770cd23`，补 `llms-full.txt`）后的复核：

| 检查 | 结果 |
| --- | --- |
| `/llms-full.txt` | 线上 **69,324 字节**，与提交**逐字节一致**；含 `收录页面（8 个，中英各 4）` 与新的相关项目分节 |
| `/llms.txt`、`/sitemap.xml` | 均含 `/beauty-meter/`（7,329B / 3,581B） |
| 两个页面 | canonical + JSON-LD 仍在，未回归 |

### 最强的一条：把「用户真正下载到的那份」装进真浏览器

不是只看 zip 的内容列表，而是**从线上把 zip 下下来 → 解压 → 用 CDP 加载进真实 Chrome 跑完整端到端**：

```
curl -sS -o bm.zip https://xraytun.top/beauty-meter/beauty-meter-extension-1.0.0.zip
unzip -q bm.zip
EXT_DIR=<解压目录> SHOT_DIR=<临时目录> node tools/verify-in-chrome.js
→ 通过 16 / 16
```

其中包含：后台跨域取图出报告、网页悬停角标、**同一张合成人像的原片 24 分 vs 美颜版 66 分**、
点击展开报告卡片、悬停开关生效、弹窗「分析当前页面最大的图片」、Service Worker 内引擎契约。
（`EXT_DIR` / `SHOT_DIR` 这两个覆盖点就是为「验证别人手上的副本」加的：默认不往用户的解压目录里写截图。）
