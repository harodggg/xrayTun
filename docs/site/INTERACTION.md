# xray-tun 官网交互规范（INTERACTION）

> 本文是 **`site/` 官网的交互规格**（`task-19` 的实现输入）。文案归 `product-manager`（`docs/site/CONTENT.md`），
> 视觉归 `art-designer`（`docs/site/VISUAL.md`）；本文只管**怎么用**：导航、下载到装好的每一步、语言切换的 URL 策略、
> 状态与降级、可访问性、以及**关键内容必须在 HTML 里**这条 GEO 底线。
>
> **前提**：`site/` 目录**尚不存在**（本文写作时 `ls site` = No such file or directory）。所以本文全部是**规格**，
> 不是对既有页面的描述。凡属「现在是什么行为」的陈述，指的都是**仓库里已有的产物**（Release、README、CI）。
>
> **证据三档**，正文逐条标注：
> * **【实测·真机】**：在本机 macOS 26.6.2 上对**真实发布的 dmg** 做的操作（`gh release download v0.8.26` → 只读挂载 → `codesign` / `spctl` / `stapler` / `xattr` / `lipo`）；
> * **【实测·浏览器】**：headless Chrome 153 对我自己写的静态原型（`/tmp/site-proto/index.html`，禁 JS 加载）的探针结果；
> * **【代码/文档】**：仓库文件的行号引用。
>
> 没有第四条。我没有真机点过 Gatekeeper 的 GUI 弹窗（那需要图形会话并会改动系统），凡是弹窗**措辞**都标为「预期文案」。

---

## 0. 站点形态的一个硬前提：这是 project 站点，站内链接在子路径下

* 仓库实测：`gh repo view` → `url = https://github.com/harodggg/xrayTun`，**description 与 homepage 都是空字符串**。
* GitHub Pages 的 **project 站点**地址形如 `https://harodggg.github.io/<repo>/`（由 `task-20` 部署）。
* **规格**
  * **INT-0-1**：站内所有**相对**链接与资源引用必须能在子路径下工作。写法：站内用**相对路径**（`./en/`、`../`、`assets/site.css`），
    **不要**用 `/en/`、`/assets/...` 这种站根绝对路径（部署到子路径后会 404）。
    反面教材：本文原型里为了让 `file://` 打开也能跳，写的是 `/xrayTun/en/`（【实测·浏览器】`hreflang` 输出 `https://harodggg.github.io/xrayTun/en/`）；真实站点应改为相对路径或完整绝对 URL。
  * **INT-0-2**：**不要用 `<base href>`** 来解决路径问题。它会把页内锚点（`#download`）也改成相对 base 解析，
    在子路径 + 反向代理场景下极易静默失效。
  * **INT-0-3**：`sitemap.xml` / `llms.txt` / `llms-full.txt` / `og:url` / `canonical` / `hreflang` 里的 URL
    **必须是绝对 URL**（爬虫不会替我们补域名）。
  * **INT-0-4**：部署后**实测**大小写：GitHub Pages 的路径是否保留仓库名大小写（`xrayTun` vs `xraytun`）必须以
    `curl -I` 的真实结果为准，`canonical` / `hreflang` / `sitemap` 三处必须与实测一致。**不要凭记忆写死**。

---

## 1. 导航：单页锚点 + `/en/`，不做多页

**规格：单页（`/`）+ 一份英文副本（`/en/`），页内用锚点导航。**

理由（每条都可验证）：

1. 内容量小：官网只需要「是什么 / 解决什么问题 / 能力 / 下载安装 / FAQ / 链接」六段（`docs/site/CONTENT.md` 的 IA）。
   拆多页会让每页只有一两段，反而稀释 AI 抽取时的上下文密度。
2. **GEO**：单页 + 稳定锚点让 AI 抓一次就能拿到全部事实；
   但**锚点 URL 只能带一个 `#fragment`**，AI 引用锚点页时正文就是整页，因此每段必须能**独立成义**（`CONTENT.md` 已要求）。
3. 英文必须**独立路径**（见 §3），所以是「两个单页」而不是「一个双语 SPA」。

### 1.1 锚点结构（固定 id，实现按此命名）

| id | 段落 | 为什么需要它 |
|---|---|---|
| `#what` | 一句话是什么 | AI 摘要最常抽取的实体定义 |
| `#why` | 解决什么问题 | 用户处境，不是功能清单 |
| `#features` | 核心能力（事实） | 功能事实点，供 FAQ/结构化数据交叉引用 |
| `#download` | 下载 | 主 CTA，必须首屏可见入口 |
| `#install` | 安装（**含未公证说明**） | 本官网最重要的锚点，FAQ 与 llms.txt 都要直接指向它 |
| `#faq` | 常见问题 | `FAQPage` JSON-LD 必须与它逐条对应 |
| `#links` | 仓库 / 文档 / 更新记录 | 绝对外链，AI 用于溯源 |

* **INT-1-1**：导航项用**可读文本**（「下载」「安装」「常见问题」），**禁止**「点击这里」「更多」。
* **INT-1-2**：下载按钮**出现两次**（首屏 + `#download` 段），两处 `href` 指向**同一个真实资产 URL**（见 INT-2-2），
  不做 `#download` 内部跳转假装是按钮。
* **INT-1-3**：`#install` 必须在导航里，因为这是本产品的**最高摩擦点**（见 §2）。

### 1.2 移动端导航

* **INT-1-4（≤640px）**：不引入 JS 汉堡菜单。做法：导航条**横向滚动**放 4-5 个锚点链接 + 语言链接，
  或用原生 `<details>` 做折叠（零 JS、键盘可达、屏幕阅读器可读）。
  理由：JS 汉堡菜单会让「导航项」只存在于 JS 之后，**对禁用 JS 的爬虫等于不存在**；而它只有 5 个链接，不需要折叠。
* **INT-1-5**：语言链接在移动端**不得被折叠进菜单**（英文用户与英文 AI 爬虫都要一眼可点、可抓）。

---

## 2. 下载到装好（本官网最重要的路径）

### 2.0 先看事实：这个包一定会被 Gatekeeper 拦

**【实测·真机】**（`gh release download v0.8.26` → `hdiutil attach -readonly` → 对 `XrayTun.app` 直接跑）：

```
$ codesign -dv --verbose=2 XrayTun.app
CodeDirectory v=20400 size=59884 flags=0x2(adhoc) hashes=1865+3 location=embedded
Signature=adhoc
Identifier=com.xraytun.desktop
TeamIdentifier=not set
Format=app bundle with Mach-O universal (x86_64 arm64)

$ spctl --assess --type execute -vvv XrayTun.app
XrayTun.app: rejected

$ xcrun stapler validate XrayTun.app
XrayTun.app does not have a ticket stapled to it.
```

→ **ad-hoc 签名、无 Team ID、无公证 ticket、Gatekeeper 评估为 rejected**。
这与 `release.yml:171` 的自述一致：「这个包是 **ad-hoc 签名、未公证**的，因为仓库里没有 Developer ID 证书。」
**任何从浏览器下载的用户都会遇到拦截**，官网必须把这条路走完。

**另一个必须先修正的事实（本文件新增，实测）**：项目现在给的命令**在当前 macOS 上不工作**。

```
$ xattr -dr com.apple.quarantine /Applications/XrayTun.app
option -r not recognized            ← 本机 macOS 26.6.2 【实测·真机】
```

出处：`release.yml:165` 与 `README.md:106` 都写 `xattr -dr com.apple.quarantine …`。
现代 macOS 的 `xattr` **没有 `-r`**（`xattr -h` 只列 `-s -l -z -p -w -d -c`）。
正确写法【实测·真机，已跑通】：

```
$ xattr -d com.apple.quarantine /Applications/XrayTun.app     # 删掉隔离标记
$ xattr -l /Applications/XrayTun.app                          # 只剩 FinderInfo，说明已清除
```

**这意味着官网不能照抄 Release Notes**，否则用户会在这一步失败并以为官网在骗人。

**第三个事实（实测）**：包是 **universal**，且**核心随包附带**——用户**不需要**自己装 Xray：

```
$ lipo -archs XrayTun.app/Contents/MacOS/xraytun-desktop   → x86_64 arm64
$ lipo -archs XrayTun.app/Contents/MacOS/xraytun-helper    → x86_64 arm64
$ lipo -archs XrayTun.app/Contents/Resources/xray          → x86_64 arm64
$ ls XrayTun.app/Contents/Resources/                        → geoip.dat geosite.dat xray XrayTun.icns
$ PlistBuddy -c "Print :CFBundleShortVersionString" Info.plist → 0.8.26
```

dmg 布局【实测·真机】：`Applications -> /Applications` 符号链接 + `XrayTun.app` → 标准的「拖进应用程序」。

### 2.1 逐步规格（官网要让人不查搜索引擎就装好）

下面每一行都要**同时出现在 HTML 里**（不是 JS 渲染后才有；验收方法见 §6）。
UI 文案建议用 `product-manager` 的定稿；这里给的是**步骤与分支**。

| 步 | 用户看到 / 做什么 | 官网必须提供的信息 | 备注（依据） |
|---|---|---|---|
| 1 | 点「下载 dmg」 | 按钮旁边写**版本号 + 大小 + 文件名**：`v0.8.26 · 47.1 MB（44.9 MiB）· XrayTun_0.8.26_x86_64_arm64.dmg`，并给 zip 备用链接与 SHA256 入口 | 【实测·真机】`gh release view v0.8.26`：dmg = 47,128,987 B、zip = 42,636,964 B、`SHA256SUMS.txt` 200 B、`isDraft: false`、`publishedAt: 2026-09-20` |
| 2 | 打开 dmg，把 `XrayTun.app` 拖进「应用程序」 | 一句「把 XrayTun 拖进「应用程序」文件夹」，配一张 dmg 窗口截图 | dmg 里就是 `Applications` 软链 + app（实测） |
| 3 | 在「应用程序」里双击 XrayTun | **预告会被拦**：一段醒目但**不惊慌**的说明 + 「这不是 dmg 坏了」 | 见 §2.2 弹窗文案 |
| 4 | 系统弹窗拦截 | 给出**两条路径**：图形路径（系统设置 → 隐私与安全性 → 仍要打开）与终端路径（`xattr -d …`），并说明**哪条适合谁** | Apple 已废除 Control-click 绕过（见 §2.2），所以图形路径必须写成新版 |
| 5 | 再打开一次 | 提示可能需要再点一次「打开」确认 | 预期文案，未在真机 GUI 走完 |
| 6 | App 打开了，但 TUN 还不能用 | 写清「首次需要在设置里安装特权 helper，会要一次管理员密码」 | `release.yml:167` 原文；helper 存在且 universal（实测） |
| 7 | 装好了 | 给一个**成功判据**：「设置里 helper 显示『已就绪』，顶栏切到 TUN 模式后能连上」 | 应用内的实际文案：`Dashboard.tsx:204`「已就绪」 |

* **INT-2-1**：第 4 步的两条路径**必须并列**，不能只给图形路径 —— 因为图形路径的分支最多（系统版本差异），
  终端命令只有一条且**已验证**（`xattr -d`）。
* **INT-2-2**：下载 URL 用**具体 tag 的资产 URL**（`…/releases/download/v0.8.26/XrayTun_0.8.26_x86_64_arm64.dmg`），
  **不要**用 `/releases/latest/download/…` 当唯一入口：后者在资产名随版本变化时会 404（本项目的文件名里**带版本号**）。
  可以额外给一个「最新版页面」链接指向 `…/releases/latest`，但按钮本身要指向真实资产。
* **INT-2-3**：`SHA256SUMS.txt` 的校验步骤给**可复制命令**，并与文件名一一对应。
  【实测·真机】资产 `SHA256SUMS.txt` 存在（200 B）；dmg 的 `digest` 由 GitHub API 给出 `sha256:dc370640…`。
  官网**不要**手抄 sha256（会随版本变），要么链接到 `SHA256SUMS.txt`，要么由构建时注入并同时更新静态 HTML。
* **INT-2-4**：第 6 步必须写「**不需要**另外安装 Xray 核心」——实测包内已含 `Contents/Resources/xray`。
  这条是**减少用户放弃率**的关键（否则用户会去找核心，撞上第二个坑）。

### 2.2 拦截弹窗：必须在官网里「预告」的两种画面

**依据（官方，已核实原文）**——Apple Developer News，2024-08-06：

> In macOS Sequoia, users will no longer be able to Control-click to override Gatekeeper when opening software that isn't signed correctly or notarized. They'll need to visit System Settings > Privacy & Security to review security information for software before allowing it to run.
> — <https://developer.apple.com/news/?id=saqachfa>

* **INT-2-5（**高**）**：官网**禁止**出现「右键点按 → 打开」作为唯一解法。
  该做法在 macOS 15（Sequoia）及以后**已被移除**；本机是 **macOS 26.6.2**【实测·真机】，属于受影响范围。
  项目现在的说明（`release.yml:163`、`README.md:103`）正是「右键 →『打开』」，**必须与官网一起改**（见 §8 的报 lead 项）。
* **INT-2-6**：官网要给出**版本分叉**说明，而不是二选一：
  * **macOS 15 及以上**（含 26）：图形路径 = 双击被拦 → 「系统设置 → 隐私与安全性」→ 在**「安全性」**一栏看到
    「已阻止使用「XrayTun」，因为来自身份不明的开发者」→ 点「**仍要打开**」→ 用 Touch ID / 密码确认 → 再双击应用。
  * **macOS 14 及以下**：右键（或 Control-点按）应用图标 →「打开」→ 在弹窗里再点「打开」。
  * **不想点 GUI**：终端 `xattr -d com.apple.quarantine /Applications/XrayTun.app`（**已验证可用**；不要写 `-dr`）。
* **INT-2-7**：弹窗**措辞**由系统给，官网引用时写「你可能会看到类似这样的提示」并给出**关键词**，
  不要假装逐字一致（不同小版本 + 系统语言措辞不同）。**我未在真机 GUI 走完这条路径**，故此处只给规格。
* **INT-2-8**：`#install` 段落里给一个 **FAQ 三连**（同时作为 `FAQPage` JSON-LD 的内容，逐字一致）：
  1. 「打开时提示『无法验证开发者』/『Apple 无法检查是否包含恶意软件』怎么办？」→ 结论句 + 两条路径；
  2. 「提示『XrayTun.app 已损坏，无法打开』是真的损坏吗？」→ 不是。ad-hoc 签名 + 未公证，
     系统用同一类话术拒绝；先执行 `xattr -d com.apple.quarantine`，仍不行则校验 SHA256（见 `## 校验`）。
     **依据**：实测 `spctl` = rejected、`stapler validate` = 无 ticket、`Signature=adhoc`、`TeamIdentifier=not set`；
     非严格 `codesign --verify` 反而**通过**（`valid on disk` / `satisfies its Designated Requirement`）。
  3. 「为什么不用 Developer ID 签名/公证？」→ 需要付费开发者账号（`release.yml:172` 自述）。
* **INT-2-9**：**不依赖颜色**：这三条 FAQ 的答案首句必须是**结论**（「不是损坏」「不需要自己装核心」），
  不能靠红色警示块的颜色传达严重性（见 §5）。

### 2.3 一个必须报备的实测发现（不属于交互，但会影响安装成功率）

**【实测·真机】** 发布的 app 里，`Contents/MacOS/xraytun-helper` 带一个 `com.apple.FinderInfo` 扩展属性
（从 dmg 里**复制出来仍然存在**，说明是产物自身带的，不是挂载引入）。后果：

```
$ codesign --verify --verbose=1 XrayTun.app     # ← CI 用的就是这条（release.yml:128）
XrayTun.app: valid on disk
XrayTun.app: satisfies its Designated Requirement

$ codesign --verify --strict --verbose=2 XrayTun.app
XrayTun.app: resource fork, Finder information, or similar detritus not allowed
In subcomponent: XrayTun.app/Contents/MacOS/xraytun-helper
file with invalid attached data: Disallowed xattr com.apple.FinderInfo found on …/xraytun-helper
```

* 影响：**不影响 ad-hoc 启动**（用户按 §2.1 走完就能打开），但①如果以后要公证，这个 xattr 会让**公证直接失败**；
  ②「已损坏」类提示的概率会升高，而官网没法替用户判断，只能给出 `xattr -d` 兜底。**建议报 lead 让 ops/打包侧在 CI 里 `xattr -c` 清掉再打包。**

---

## 3. 语言切换与 URL 策略（GEO 结论）

### 3.1 结论：`/` 与 `/en/` 两个**真实路径**，不用 `?lang=en`

**规格 INT-3-1**：语言版本是**两个真实、稳定、可独立访问的 URL**：

```
https://<pages-host>/            ← 简体中文（x-default）
https://<pages-host>/en/         ← English
```

**理由（分三层，前两层有依据，第三层是我的推理，已标注）**：

1. **【文档】** Google Search Central 的《Localized Versions of your Pages》要求为每种语言提供**不同的 URL**，
   并用 `hreflang`（含 `x-default`）在页面间互相声明语言/地区版本：
   <https://developers.google.com/search/docs/specialty/international/localized-versions>。
   该文档同时是 Googlebot 抓取多语言版本的入口。
2. **【文档+官方公告】** AI/搜索爬虫**不一定执行 JS**（本项目自己就要求「关键内容在 HTML 里」，见 §6）。
   如果语言切换是 `?lang=en` + JS 渲染，那么 `/` 的 HTML 里只有中文，
   英文内容对**不执行 JS 的抓取方**根本不可见；换成 `/en/index.html`，英文正文就在 HTML 里，无需执行任何脚本。
3. **【我的推理，需在 task-21 实测确认】** 同一路径的不同 query 参数通常被搜索引擎**规范化到主 URL**，
   而 AI 引用需要一个**稳定的、能被单独引用与分享的 URL**；`/en/` 这种路径天然满足，
   `?lang=en` 则可能只被记成 `/` 的变体。→ 因此**路径优于 query**。

* **INT-3-2**：切换控件必须是**真实的 `<a href>`**，**不是** `<button onclick=…>`：
  ```html
  <a href="./en/" hreflang="en" lang="en">English</a>   <!-- 在 / 上 -->
  <a href="../" hreflang="zh-CN" lang="zh-CN">中文</a>   <!-- 在 /en/ 上 -->
  ```
  理由：`<a>` 才是爬虫可跟的链接，也是「语言之间有真实关系」的证据；`<button>` 对爬虫等于不存在。
* **INT-3-3**：`<html lang>` 必须与页面语言一致：`/` 用 `lang="zh-CN"`，`/en/` 用 `lang="en"`。
  理由：屏幕阅读器用 `lang` 选发音规则；AI 用它判断语言。**不允许**用 JS 动态改 `lang`（首屏必须正确）。
* **INT-3-4**：`hreflang` 三件套（两页都要写，**互相回指 + 一个 `x-default`**）：
  ```html
  <link rel="canonical" href="https://<host>/">                <!-- /en/ 里指向 /en/ -->
  <link rel="alternate" hreflang="zh-CN" href="https://<host>/">
  <link rel="alternate" hreflang="en"    href="https://<host>/en/">
  <link rel="alternate" hreflang="x-default" href="https://<host>/">
  ```
  **【实测·浏览器】** 这套标签在我的原型里能被 DOM 正确解析（探针输出 3 条 `hreflang`，指向正确 URL）。
* **INT-3-5**：**不做自动跳转**。不要根据 `navigator.language` 或 localStorage 把 `/` 重定向到 `/en/`。
  理由：①爬虫会被跳转误导（把中文页当成重定向）；②用户分享 `/` 时，别人看到的语言应该由 URL 决定；
  ③自动跳转 + 无 JS 会直接失效，属于「关键行为依赖 JS」。
* **INT-3-6**：**记忆策略**：用 `localStorage` 只做**一件事**——用户下次访问 `/` 时，在语言链接旁加一句
  「上次你用的是 English」（可点击），**不自动跳转**。无 JS 时这句不出现，页面仍完整可用。
* **INT-3-7**：`sitemap.xml` 同时列 `/` 与 `/en/`；`llms.txt` / `llms-full.txt` 用**绝对 URL**分别指向两版内容。
* **INT-3-8（GEO 验收）**：`curl -s <host>/en/ | grep -c 'xray\|XrayTun'` 必须 > 0 ——
  即英文正文出现在**原始 HTTP 响应**里，而不是靠 JS 渲染。

---

## 4. 状态：下载按钮与版本信息

### 4.1 版本信息：静态写进 HTML + JS 增强 + 明确降级（推荐方案 C）

| 方案 | 失效模式 | 对 GEO 的影响 |
|---|---|---|
| **A. 纯静态写死** | 发新版后忘记改 → 网站长期显示旧版本号与**过期下载链接**（本项目的资产名带版本号，链接会 404）；且 AI 会引用过期版本号 | 正文里永远有版本号（好），但**可能是错的**（坏） |
| **B. 纯运行时取 GitHub API** | 未认证 API 限流（每 IP 每小时 60 次）、离线、公司网络拦截 api.github.com、仓库改名/转私有 → **版本区空白**；禁 JS 用户与 AI 爬虫看到的是空 | **关键内容不在 HTML 里**（违反 §6），AI 抽不到版本号 → 直接违反本任务的核心目标 |
| **C. 静态写进 HTML + JS 只提示「有新版」（推荐）** | JS 失败时退化成方案 A（仍完整可用）；只有「有新版」这句话会缺失 | 版本号在 HTML 里可被引用；JS 只是增强 |

* **INT-4-1（采用 C）**：
  * HTML 里写死**当前**版本号、发布日期、文件名、大小、下载 URL（绝对）；
  * JS 拉 `https://api.github.com/repos/harodggg/xrayTun/releases/latest`，**只做一件事**：
    若 `tag_name` 与 HTML 里的版本不同，在下载区插入一条 `role="status"` 的提示
    「仓库已有 vX.Y.Z（本页显示的 v0.8.26 仍是可下载的）」；
  * **不修改**下载链接指向（避免把用户带去未经验证的资产），只提示。
* **INT-4-2（降级）**：JS 失败（限流/离线/CORS）时**静默保留静态内容**——
  不显示错误、不显示空白、不隐藏下载按钮。理由：官网的存在意义是「让人下载」，JS 只是锦上添花。
  **【实测·浏览器】** 我的原型正是这个结构：禁 JS 加载后 H1、下载链接、5 步安装说明、FAQ 全部在 DOM 里
  （探针：`hasH1: 1`、`dmgLink: https://github.com/…/XrayTun_0.8.26_x86_64_arm64.dmg`、`installInDom: true`、`skipLink: true`）。
* **INT-4-3（限流对策）**：若一定要在页面上实时取（不推荐），必须 (a) 设 3-5 秒超时；
  (b) 结果只用于增强；(c) 失败时**不**用 `aria-live` 播报错误（用户没做错任何事）。
* **INT-4-4**：`SoftwareApplication.softwareVersion` 与页面可见版本号**必须一致**，
  由 `task-19` 在构建时从同一个来源注入（同一个字符串出现在 3 处：可见文本、JSON-LD、`llms.txt`）。
  理由：不一致会被 AI 当成矛盾事实，降低引用可信度。

### 4.2 下载按钮的状态

| 态 | 呈现 | 规格 |
|---|---|---|
| **L 加载** | 不需要。按钮从一开始就是可点的 `<a>`（HTML 里就有 `href`） | **INT-4-5**：禁止「JS 加载完才把按钮变成可用」——这是纯静态站，链接不需要等 |
| **E 空/取不到版本** | 显示静态版本（INT-4-2） | INT-4-2 |
| **D 正常** | 主按钮：`下载 macOS 版（v0.8.26 · 44.9 MiB）`；次链接：zip、SHA256、Release 页 | INT-2-1/2-2 |
| **P 有新版** | 静态版本 + 一行 `role="status"`「仓库已有 vX.Y.Z」 | INT-4-1 |
| **F JS 失败** | 与 D 完全一样（静态内容），只是没有新版本提示 | INT-4-2 |

* **INT-4-6**：点下载后**新开标签**（`target="_blank" rel="noopener"`）还是同页？
  规格：**同页下载**（`<a href download>` 语义交给浏览器），不要 `target="_blank"` ——
  且**必须在下载链接附近立刻能看到 §2 的安装步骤**，否则用户下完就卡在 Gatekeeper。
  GitHub 资产会 302 到 CDN，`download` 属性能否生效不保证；**不要依赖 `download`**，把它当提示。

---

## 5. 可访问性

* **INT-5-1（skip link，必做）**：`<body>` 第一个可聚焦元素是
  `<a class="skip-link" href="#main">跳到主内容</a>`，`<main id="main" tabindex="-1">`。
  理由：导航条在每个页面最前，键盘/读屏用户每次都要重走一遍。
  **【实测·浏览器】** 原型里 `skipLink: true`，且它确实是 DOM 里第一个可聚焦项。
* **INT-5-2（焦点可见）**：`:focus-visible { outline: 2px solid <accent>; outline-offset: 2px }`，
  **禁止** `outline: none` 而不给替代。深色主题下尤其重要（本项目应用内就吃过这个亏：`styles.css:290` 的 `outline: none`）。
* **INT-5-3（键盘顺序）**：Tab 顺序应为：skip link → 导航锚点 → 语言链接 → 下载主按钮 → 安装步骤里的复制命令按钮 → FAQ。
  **不引入**需要键盘额外操作的自定义控件（`<details>` 是原生可键盘的，可用）。
* **INT-5-4（`lang`）**：见 INT-3-3。**中英混排时**，英文片段（如 `XrayTun`、`macOS`）不必逐字包 `lang="en"`，
  但**英文页面里的中文专有名词**（如果出现）要包 `<span lang="zh-CN">`。
* **INT-5-5（屏幕阅读器）**：
  * 一个 `<h1>`，层级 `<h2>`/`<h3>` 不跳级；
  * 每个 `<a>` 有可读文本（禁止「这里」）；
  * 下载按钮的**可访问名**要含版本：`下载 macOS 版 XrayTun v0.8.26（dmg，44.9 MiB）`；
  * 安装步骤用 `<ol>`（有序语义），代码块用 `<pre><code>`，命令带**复制按钮**时该按钮要有 `aria-label="复制命令：xattr -d …"`；
  * 「有新版」提示用 `role="status"`（礼貌播报，不打断）。
* **INT-5-6（不依赖颜色）**：
  * 「未公证」提示**不能**只靠黄色背景：必须有文字（「未公证，首次打开需手动放行」）+ 图标（`⚠`），
    且标题里就出现结论词；
  * 成功/失败分支（「仍不行？」）用**文字标签**（「如果还是打不开」），不要用红/绿点；
  * 下载按钮的主/次层级用**文字与位置**区分（主按钮写「下载 macOS 版」，次链接写「zip 备用 / SHA256 校验」），
    颜色只是补充。
* **INT-5-7（图片）**：应用截图必须有 `alt`，且 `alt` 要写**功能**而不是「截图」：
  例如 `alt="XrayTun 拓扑页：入口、规则链与出口的流向图，每辆车代表累计流量"`。
  纯装饰图用 `alt=""`（不要省略）。
* **INT-5-8（移动端触控）**：可点区域 ≥ 44×44 CSS px；语言链接与下载按钮在 375px 宽下不换行截断。

---

## 6. 关键内容必须在 HTML 里（GEO 底线）

### 6.1 必须在**原始 HTTP 响应**里的内容（逐项清单）

* **INT-6-1**：以下内容**不得**依赖 JS 才能出现：
  1. `<h1>` 的一句话定义（含「macOS」「Xray」「TUN」三个关键词）；
  2. **仅支持 macOS**、支持 arm64 + x86_64 通用包；
  3. 当前版本号 + 发布日期 + 文件名 + 大小；
  4. **dmg 下载链接（真实 `href`）** + zip 链接；
  5. **未公证说明 + 完整安装步骤**（含 `xattr -d com.apple.quarantine` 那条命令）；
  6. 「不需要另外安装 Xray 核心」这条；
  7. FAQ 的问答全文（与 JSON-LD 逐字一致）；
  8. 仓库/文档/更新记录三个外链。
  理由：AI 爬虫与禁用 JS 的用户都只能看到 HTML；这几条正是**被引用频率最高的事实点**。
* **INT-6-2**：JS 只允许做（白名单）：
  ①「有新版」提示（INT-4-1）；②语言偏好记忆提示（INT-3-6）；③复制按钮（无 JS 时降级为可手动选中的 `<code>`）。
  **白名单之外**的任何渲染都视为违规。
* **INT-6-3**：`<noscript>` **不是**解决方案——把内容放进 `<noscript>` 会让有 JS 的人类用户看不到；
  正确做法是**内容默认在 HTML 里，JS 只做增量**。

### 6.2 验收方法（可复现，已跑通）

```bash
# 1) 禁 JS 加载，截图 + 取正文（headless Chrome + CDP）
#    脚本：/tmp/interaction-review/cdp2.mjs（场景字段 jsOff:true 会调
#    Emulation.setScriptExecutionDisabled）
node /tmp/interaction-review/cdp2.mjs <scenario.json>

# 2) 只取原始 HTML（不执行任何脚本）
curl -s "$URL" | grep -c 'xattr -d com.apple.quarantine'   # 期望 ≥1
curl -s "$URL" | grep -c 'XrayTun_0\.'                     # 期望 ≥1（下载链接在 HTML 里）
```

**【实测·浏览器】** 用上面的方法对原型（`file:///tmp/site-proto/index.html`）跑过一次，
**禁用脚本后**探针输出：

```json
{"text":"跳到主内容 下载 安装 常见问题 English XrayTun：macOS 上的 Xray 图形客户端，支持 TUN 模式
 仅支持 macOS（arm64 + x86_64 通用包）。当前版本 v0.8.26，dmg 44.9 MiB。 … 安装（未公证，首次打开会被系统拦下）
 打开 dmg，把 XrayTun.app 拖进「应用程序」。 在「应用程序」里双击 XrayTun：macOS 会提示无法验证开发者。
 打开「系统设置 → 隐私与安全性」… 或者终端执行：xattr -dr com.apple.quarantine /Applications/XrayTun.app …",
 "dmgLink":"https://github.com/harodggg/xrayTun/releases/download/v0.8.26/XrayTun_0.8.26_x86_64_arm64.dmg",
 "installInDom":true,"hasH1":1,"skipLink":true}
```

（截图：`/tmp/interaction-review/shots/site-js-off.png` 与 `site-js-on.png` —— 两张正文一致。）
**注意**：原型里我故意抄了 `xattr -dr`（错误命令）来对照，正式站点必须改为 `xattr -d`（§2.0）。

---

## 7. 与 GEO 有关的其他交互细节

* **INT-7-1**：语言切换控件放在**页头右上角**，两种语言都在 HTML 里可点、可抓（不是 JS 下拉）。
* **INT-7-2**：**不要**用「自动弹出语言选择遮罩」。理由：干扰式弹窗会影响页面体验评估，
  且遮罩在禁 JS 时可能永久挡住内容。
* **INT-7-3**：页脚放仓库、文档、更新记录（`CHANGELOG`）与 `llms.txt` 的**绝对链接**；
  AI 抓取方走完首页后需要能顺着链接拿到文档。
* **INT-7-4**：任何**外链**（GitHub / 文档）用 `rel="noopener"` 且带可读文本；
  对**非官方/不受控**外链可加 `rel="nofollow"`，但仓库/文档/Release 这三个是官方源，**不要**加 `nofollow`（会削弱溯源链）。
* **INT-7-5**：不要用 `aria-hidden` 或 CSS 隐藏来「美化」重复内容：
  如果同一事实（如版本号）出现两次，两处都保留可读。

---

## 8. 验收清单（交给 task-19 / task-21）

1. **禁 JS**：`Emulation.setScriptExecutionDisabled` 下截图 + 取 `document.body.innerText`；
   §6.1 的 8 条内容**全部**在正文里（逐条勾选）。
2. **原始 HTML**：`curl -s "$URL"` 里能 grep 到版本号、dmg 链接、`xattr -d com.apple.quarantine` 字样。
3. **语言**：`/` 与 `/en/` 均返回 200；两页互相 `hreflang` 回指 + 一个 `x-default`；
   各自 `canonical` 指向自己；`lang` 属性正确；`/en/` 的**原始 HTML** 含英文正文（INT-3-8）。
4. **下载**：按钮 `href` 指向**存在的资产**（`curl -I` 200/302）；页面另给 zip、SHA256、Release 页三个链接。
5. **安装文案**：不出现「右键 →『打开』」作为唯一解法（INT-2-5）；`xattr` 命令是 **`-d`**，不是 `-dr`。
6. **版本一致性**：可见文本 / `SoftwareApplication.softwareVersion` / `llms.txt` 三处相同（INT-4-4）。
7. **可访问性**：skip link 是第一个可聚焦元素；`:focus-visible` 可见；Tab 顺序符合 INT-5-3；
   一个 `<h1>`；所有 `<a>` 有可读文本；截图有 `alt`。
8. **子路径**：把站点部署在**子目录**下（`python3 -m http.server` + 子目录模拟，或用 Pages 真地址）后，
   站内链接与 `assets/` 资源**全部**可用（INT-0-1）；`#install` 锚点跳转正常（`<base>` 会破坏它，INT-0-2）。
9. **移动端**：375px 宽下首屏能看到一句话定义 + 下载按钮；导航不依赖 JS 展开。

---

## 9. 必须报 lead 的两处事实冲突（我在本文里不能自行改别人的文件）

* **CONFLICT-1**：`release.yml:163` 与 `README.md:103` 的安装说明是「右键 →『打开』」，
  在 macOS 15+ **已失效**（Apple 官方公告，§2.2），且 `xattr -dr` 这条命令在本机 macOS 26.6.2 **直接报错**
  （`option -r not recognized`，【实测·真机】）。
  → 建议 lead 让 `ops` 改 Release Notes、`frontend-dev`/文档所有者改 README，统一为 §2.2 的两条路径 + `xattr -d`。
  **这是本轮唯一一处「项目自己的说明会导致用户装不上」的事实错误。**
* **CONFLICT-2**：发布的 app 里 `xraytun-helper` 带 `com.apple.FinderInfo` xattr，
  导致 `codesign --verify --strict` **失败**（CI 用的非严格 `--verify` 通过，所以没被发现）。
  现在不影响 ad-hoc 启动，但会**让以后的公证必然失败**。建议在打包流程里加 `xattr -c`（§2.3）。

---

## 10. 未验证 / 不确定（诚实清单）

1. **Gatekeeper GUI 弹窗**：我**没有**在真机图形会话里走完「双击 → 系统设置 → 仍要打开」，
   所以 §2.2 的弹窗措辞是「预期文案」；已核实的只是**策略**（Apple 官方公告）、
   **产物状态**（`Signature=adhoc`、`TeamIdentifier=not set`、`spctl` rejected、无公证 ticket）与
   **命令可用性**（`xattr -d` 可执行、`xattr -dr` 报错）。
2. **`?lang=en` 与 `/en/` 的索引差异**：我只引用了 Google 官方文档的「不同 URL + hreflang」要求；
   「query 参数变体会被规范化」这一层是我的推理，**未做 A/B 抓取实验**。task-21 可用
   `curl` + Search Console（若可访问）做一次真实验证。
3. **GitHub API 未认证限流**：60 次/小时/IP 是通用事实，我**没有**在本机跑满 60 次去触发 429。
   建议 task-19 实现时用超时 + 降级，不必真的压测。
4. **Pages 子路径的大小写**：未部署，无法实测；见 INT-0-4。
5. **文件大小换算**：47,128,987 B = 44.9 MiB = 47.1 MB；42,636,964 B = 40.7 MiB = 42.6 MB（按 1 MiB=1048576 计算）。
   官网呈现方式由 `art-designer`/`product-manager` 定，但**必须与 `gh release view` 的字节数一致**。
6. **版本号会过期**：本文所有版本数据取自 `gh release view v0.8.26`（2026-09-20T07:29:52Z，`isDraft: false`）。
   发新版后需同步更新 §2.1、§4 中的数字。
7. **中英内容等价性**：属 `CONTENT.md`（task-16）与 task-21 的验收；本文只规定 URL 与交互，不校验文案翻译完整度。
