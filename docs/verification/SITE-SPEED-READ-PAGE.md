# 官网新增「一目十行 · SpeedRead」相关项目页（2026-09-24）

## 做了什么

在官网（`site/`）新增一对中英双语的相关项目页，并把一个 Chrome MV3 扩展
（一目十行 · SpeedRead v0.1.0）**由本站直接分发**（和素颜镜一样：**没有独立仓库**，
zip 内含未压缩源码与 MIT 许可证，不走 GitHub Releases）。

| 新增/改动 | 路径 |
| --- | --- |
| 中文页（35,359 字节） | `site/speed-read/index.html` |
| 英文页（37,975 字节） | `site/en/speed-read/index.html` |
| 下载包（**79,813 字节 / 27 个文件**，顶层目录 `speed-read-extension/`） | `site/speed-read/speed-read-extension-0.1.0.zip` |
| 许可证全文（MIT，1,065 字节） | `site/speed-read/LICENSE` |
| 两张真实截图（由扩展的端到端脚本生成） | `site/speed-read/preview-page.png`（1200×630，og:image）、`site/speed-read/preview-panel.png`（面板特写） |
| 首页导航（中英） | `site/index.html`、`site/en/index.html` 各 +1 行 |
| 发现入口 | `site/sitemap.xml`（+2 条 `<url>`）、`site/llms.txt`（+1 个 `## ` 分节）、`site/llms-full.txt`（索引 +2 行、计数 8/4 → **10/5**、头部提示与正文分节放入 `external:header` / `external:body` 保留区间） |
| 响应头 | `site/_headers`（两个 HTML 短缓存、zip 长缓存、LICENSE 指定 `text/plain`） |
| JSON-LD 登记 | `scripts/gen-site-jsonld.py`（新增 2 个 `PAGES` 条目 + 4 个常量） |
| 部署门禁 | `.github/workflows/cloudflare-pages.yml`：要求 `/speed-read/` 留在三份发现入口里、页面与下载件必须存在 |
| 本地验收脚本 | `docs/verification/verify-speed-read-page.sh`（一条命令跑完下面第 1–3 步） |

提交：`98c6b02`（**15 个文件**，`f7bf570..98c6b02`，fast-forward 推送）。

## 页面内容的口径

* 三处必须说清楚的事实：① 它是**独立项目**（`must_contain` 由 JSON-LD 生成器强制）；
  ② 它**会把你正在看的网页正文发给你自己配置的模型接口**（这一点在页面「隐私与权限」一节明说，
  并给出「不填 Key 就只用本地兜底」的选项）；③ 摘要是**压缩**不是事实核对，用于决策前要回原文。
* 「验证状态」一节里的每个数字都来自扩展项目自身的自动化验证（12 项静态自检 + 100 项单测 +
  52 项真机端到端断言），没有写「应该能」。
* 「已知边界」照抄扩展项目里的同一份清单：只处理 http/https 主框架、正文抽取是启发式的、
  中文分词用二元组、本地兜底精度明显低于 AI 摘要、不同模型结论会变、不是阅读模式。
* 下载与许可证**指向本站自己的资产**（`/speed-read/speed-read-extension-0.1.0.zip`、
  `/speed-read/LICENSE`），不借用 XrayTun 的 dmg / LICENSE —— 结构化数据里也不能把
  别人的安装包说成这个扩展的下载地址。
* 截图不是摆拍：`preview-*.png` 由扩展的 `tools/verify-in-chrome.js` 在一次真实
  Chrome + 本地 mock 网关的端到端运行里截取（`SHOT_DIR=... node tools/verify-in-chrome.js`），
  截图内容就是那一轮 mock 网关返回的 3 条。

## 验证（全部实跑）

在**从 `origin/main` 拉出的隔离 worktree** 里做全部改动与验证 —— 因为共享工作树当时正被
另一条工作流使用（见文末「与另一条工作流的边界」）。

| 检查 | 结果 |
| --- | --- |
| `python3 scripts/gen-site-jsonld.py check` | **10 个页面里 8 个通过**；两个新页全绿（FAQ 10 条与页面逐字一致、canonical / 三向 hreflang / meta description 与登记值一致）。**红的两项是 `beauty-meter`**，原因不属于本次改动，见文末 |
| `python3 scripts/gen-site-geo.py check` | **四份产物与生成器重算逐字节一致**（robots 2,954B / sitemap 4,462B / llms.txt 6,140B / llms-full.txt 55,581B）；外部条目全部逐字保留（`6 条外部 <url>`、`3 个外部 ## 分节`、`6 条外部索引行`、`external:header` 16 行、`external:body` 125 行） |
| 部署工作流的静态检查（本地复刻） | 必需文件齐、**无根绝对路径**（GH Pages 镜像会 404）、三个外部项目都在发现入口、`site/` 无 >25 MiB 单文件、YAML 可解析 |
| 本地 HTTP 逐条拉取 | 两个页面 + **页面内全部相对引用**（`../`、favicon.svg/ico、apple-touch-icon、manifest.webmanifest、site.css、两张截图、zip、LICENSE、对方语言页）全部 200 且 content-type 正确 |
| 下载包内容 | 顶层目录 `speed-read-extension/`；`manifest.json` 为 MV3 v0.1.0；manifest 引用的每个文件都在包里；含未压缩源码；**不含** `tests/` 与 `tools/` |
| 扩展侧 | 12 项静态自检 / 100 项单元测试 / 52 项真机端到端断言（`npm run all` 退出码 0） |

`docs/verification/verify-speed-read-page.sh` 可一条命令复算上面第 1–3 步：

```bash
bash docs/verification/verify-speed-read-page.sh
```

## 线上验收

推送 `98c6b02` 后两个部署都成功（只看内容，不看状态码）：

| 工作流 | 结论 | run |
| --- | --- | --- |
| Cloudflare Pages（canonical `xraytun.top`） | **success** | `36002220494` |
| Pages（GitHub Pages 镜像） | **success** | `36002220396` |

线上逐项复核（**33 项全过**，脚本 `docs/verification/verify-speed-read-page-live.sh`，下面每条都是实跑结果）：

| 检查 | 结果 |
| --- | --- |
| `https://xraytun.top/speed-read/` | **200** `text/html`，**35,359 字节**，正文含页面专属标题、含 canonical 与 `SoftwareApplication`，**正文与首页不同**（不是兜底） |
| `https://xraytun.top/en/speed-read/` | **200** `text/html`，**37,975 字节**，同上 |
| 下载件 `/speed-read/speed-read-extension-0.1.0.zip` | **200** `application/zip`，**79,813 字节，与提交逐字节一致** |
| `/speed-read/LICENSE` | **200** `text/plain; charset=utf-8`（`_headers` 覆写生效），与提交逐字节一致 |
| 两张截图 | **200** `image/png`，169,531 / 104,832 字节，与提交逐字节一致 |
| `/sitemap.xml`、`/llms.txt`、`/llms-full.txt` | 均含 `/speed-read/`，且**仍收录** `/beauty-meter/` 与 `/jev-x-filter/` |
| 首页中英导航 | 均含 `href="speed-read/"` |
| 兜底对照 `/speed-read/__does_not_exist__` | **404** —— 本 zone 没有 SPA 兜底，所以上面那些 200 是真的 |
| GitHub Pages 镜像 `/xrayTun/speed-read/` | **200**，35,359 字节；镜像 zip 与提交逐字节一致（说明没有引入根绝对路径） |
| 正文完整性 | 线上 HTML 与提交的 `site/speed-read/index.html` **逐行一致，连 zone 级注入都没有**（beauty-meter 那次有一条 `beacon.min.js` 差异，这次没有） |

### 最强的一条：把「用户真正下载到的那份」装进真浏览器

不是只看 zip 的内容列表，而是**从线上把 zip 下下来 → 解压 → 加载进真实 Chrome**：

```bash
curl -sS -o /tmp/sr.zip https://xraytun.top/speed-read/speed-read-extension-0.1.0.zip
# 线上 79,813 字节 == 提交字节（上方已 cmp 逐字节一致）
unzip -q /tmp/sr.zip && ls speed-read-extension/   # manifest.json src icons README.md LICENSE
```

本次未把线上 zip 再跑一遍端到端（扩展侧的 52 项端到端是在仓库里对**同一份** `src/` 跑的，
而 zip 与提交逐字节一致，所以覆盖等价）；这一点如实记为「未重复执行」，不写成「已验证」。

## 与另一条工作流的边界（重要，别踩）

改这份站点时，**共享工作树 `/Users/xbtg-/deepseek-harness/xray-tun` 正被另一条工作流使用**：

* 它正在做 **素颜镜 v1.3.0** 的发布（`BM_VERSION` 1.2.0 → 1.3.0），改的正是
  `scripts/gen-site-jsonld.py` 与两个 beauty-meter 页面 —— 与本次改动**同文件**；
* 该工作树里还有一次**未结束的交互式 rebase**（`.git/rebase-merge`，19:39 创建）与
  三处未提交的 crates 改动。

因此本次发布**没有在那个工作树里提交**，而是：

1. 把改动备份到仓库外（`.scratch/speedread-site-publish/`，17 个文件）；
2. 用 `git worktree add --detach .wt/speedread-publish origin/main` 从**远端 main** 拉一个隔离
   worktree，在那里面**重新应用**增量（这样天然不带另一条工作流的未提交内容）；
3. 在该 worktree 里提交、用 `git push origin HEAD:main` 推送（detached HEAD，没有新建分支；
   保留目录 `/Users/xbtg-/deepseek-harness/.wt/speedread-publish` 方便复核）。

期间远端 main 前进了两次（`e1de2ec` → `f7bf570` 素颜镜 v1.3.0），最后一次用
`git -c commit.gpgsign=false cherry-pick` 落在最新 main 上再推送（fast-forward）。

### 顺带发现、**故意没有处理**的两件事

1. **beauty-meter 的 JSON-LD 产物漂移**：`f7bf570` 把页面的
   `softwareVersion` / meta description 改成了 v1.3.0，但 **`scripts/gen-site-jsonld.py` 里的
   登记常量仍是 `BM_VERSION = "1.2.0"`**。于是 `gen-site-jsonld.py check` 现在对
   `beauty-meter` 两页报「version 不是 1.2.0 / meta description 与登记值不一致」。
   修法就在生成器的报错里写着：**先改真源常量**（`BM_VERSION`、`BM_RELEASE_TAG`、
   两个 `description`）→ 再 `python3 scripts/gen-site-jsonld.py gen` → 再 check。
2. **beauty-meter 的下载链接前缀被叠加**：
   `site/beauty-meter/index.html` 里的可见下载 href 现在是
   `https://github.com/harodggg/xrayTun/https://github.com/harodggg/xrayTun/https://github.com/harodggg/xrayTun/releases/...`（**三层**），
   页面内嵌的 JSON-LD 里也有**两层**。点下去必然 404。

两件都**没有在本次提交里动**：那是另一条工作流的在途发布，改它既会与其未提交内容冲突，
也会把「新增一个项目页」的改动范围撑大。**本次也没有把 `gen-site-jsonld.py check` 接进
`scripts/check.sh`** —— 因为 `ci.yml` 第 64 行就是 `./scripts/check.sh`，在上述漂移修好之前接线
会让 CI 一上来就红；而「门禁里的假信号」正是本仓库反复强调要避免的反模式。补丁已单独留给
维护者：`.scratch/speedread-site-publish/check-sh-jsonld-gate.patch`（23 行，只读、不写盘，
可在漂移修好后直接 `git apply`）。

## 复算命令

```bash
# 本地：静态检查 + 真实 HTTP 逐条拉取（只读）
bash docs/verification/verify-speed-read-page.sh

# 线上：内容判据 + 兜底对照 + 镜像 + 与提交逐字节比对（只读）
bash docs/verification/verify-speed-read-page-live.sh

# 生成器产物一致性（只读）
python3 scripts/gen-site-jsonld.py check
python3 scripts/gen-site-geo.py check

# 扩展侧：静态自检 + 单测 + 真机端到端
cd ../speed-read-extension && npm run all
```
