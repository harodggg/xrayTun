# 官网内容与信息架构（CONTENT）

> **任务**：task-16　**作者**：product-manager
> **唯一写入文件**：本文件（`docs/site/CONTENT.md`）。没有改源码，没有再改 `docs/product/PRD.md` 之外的任何文件。
> **上游**：`docs/site/INTERACTION.md`（task-18，锚点 id 与安装路径口径以它为准）、
> `docs/site/VISUAL.md`（task-17，长什么样归 art-designer）。
> **本文件只交付「写什么」**：可直接抄进 HTML 的中英文文案 + 信息架构决策 + 结构化数据内容。
> **基线**：`HEAD = e7ed509`；版本事实取自 `gh release view v0.8.28`（2026-09-20T16:03:44Z，`isDraft: false`）。

---

## 0. 事实核对表（每条都能追到来源）

**先说清楚：这一节是给以后的维护者看的。凡是要写进官网的断言，都必须能在下表里找到一行；
找不到的，就不许写。**

| # | 断言（写进官网的样子） | 来源（可复核） |
|---|---|---|
| F1 | 当前版本 **v0.8.28**，发布日期 **2026-09-20** | `gh release view v0.8.28` → `tagName` / `publishedAt` |
| F2 | dmg **47,145,126 字节（45.0 MiB）**，文件名 `XrayTun_0.8.28_x86_64_arm64.dmg` | `gh release view v0.8.28` assets[].size/name |
| F3 | zip **42,647,148 字节（40.7 MiB）**，文件名 `XrayTun_0.8.28_x86_64_arm64.zip` | 同上 |
| F4 | 另有 `SHA256SUMS.txt`（200 字节） | 同上 |
| F5 | 仅 **macOS 13.0 或更高** | `apps/desktop/tauri.conf.json` → `bundle.macOS.minimumSystemVersion: "13.0"` |
| F6 | 通用包，**arm64 与 x86_64 原生支持**；App / helper / 核心三者都是 universal | `.github/workflows/release.yml:118-125`（`lipo -archs` 断言三种可执行文件） |
| F7 | **包内自带 Xray 核心**（`Contents/Resources/xray`）与 `geoip.dat` / `geosite.dat`，用户**不需要**自备核心 | `tauri.conf.json` → `bundle.resources` 三项；`docs/site/INTERACTION.md` §2.0 真机 `lipo`/`ls` 实测 |
| F8 | 包内核心为 **Xray-core v26.9.9**（构建脚本默认值；本应用要求 ≥ 26.1.31） | `scripts/fetch-xray.sh:10` `VERSION="${XRAY_VERSION:-v26.9.9}"`；release.yml 未覆盖该变量；`crates/xt-core` 的 `MIN_CORE_VERSION_NATIVE_TUN = 26.1.31`（`docs/03` §1.1） |
| F9 | 包是 **ad-hoc 签名、未公证**（`Signature=adhoc`、`TeamIdentifier=not set`、`spctl` rejected、无公证 ticket） | `docs/site/INTERACTION.md` §2.0 真机实测；`.github/workflows/release.yml:171` 自述；`scripts/package-macos.sh:203` `codesign --sign -` |
| F10 | **下载后一定会被 Gatekeeper 拦**，必须按安装步骤放行 | 同 F9 |
| F11 | 正确放行命令是 **`xattr -d com.apple.quarantine`**（**没有** `-r`） | `docs/site/INTERACTION.md` §2.0 真机实测（`xattr -dr` → `option -r not recognized`） |
| F12 | **macOS 15 及以后不能再用「右键 → 打开」绕过**，要经「系统设置 → 隐私与安全性 → 仍要打开」 | Apple Developer News 2024-08-06（原文见 INTERACTION §2.2）；本机 macOS 26.6.2 |
| F13 | TUN 使用的是 **Xray-core 原生 `tun` 入站（内置 gVisor 协议栈）**，不是 tun2socks 之类旁路进程 | `docs/03-xray-integration.md` §1（上游源码核实） |
| F14 | 路由用 `0.0.0.0/1` + `128.0.0.0/1` 拆分，而不是替换默认路由 | `README.md` §核心问题；`docs/02` §路由策略 |
| F15 | 三种模式：**直连 / 系统代理 / TUN** | `crates/xt-core/src/model.rs` `ProxyMode` |
| F16 | 「系统代理」模式**只提供本机 SOCKS5(10808)/HTTP(10809) 入站，不修改 macOS 系统代理设置**（截至 v0.8.28） | 全仓库无 `networksetup -setwebproxy` 调用（`grep -rn "setwebproxy"` 只命中一处注释）；helper 协议无代理请求（`xt-proto` `Request` 枚举只有 TUN 相关）；`docs/07 §2.2` 未完成项第 6 条 |
| F17 | 分流：4 个预设 + 自定义规则；自定义规则**追加在预设之后** | `model.rs` `RoutingPreset`；`Routing.tsx:74-79` |
| F18 | 支持 **geoip / geosite 匹配**（`geoip:cn` / `geosite:cn` 等） | `docs/07 §2.1`、`docs/04` |
| F19 | 规则**顺序敏感**（自上而下取第一条命中） | `Routing.tsx:45-49`；`docs/04` |
| F20 | 订阅支持 **4 种格式**：Xray JSON / Clash-Mihomo YAML / base64 链接列表 / 明文链接列表；混入不支持的链接不会整体失败 | `crates/xt-core/src/subscription/mod.rs` 头部表；`Subscriptions.tsx` 页面说明 |
| F21 | 节点协议：**vmess / vless / trojan / shadowsocks / socks / http**；**不支持 ShadowsocksR（`ssr://`）** | `model.rs:44-96`；`Nodes.tsx` 粘贴说明 |
| F22 | DNS 有 **4 种策略**（全部走代理 / 按规则分流【默认】/ 全部本地 / 自定义服务器） | `model.rs` `DnsHandling`（`:491-510`） |
| F23 | Fake-IP 可选，**默认关闭** | 设置页 Fake-IP 卡；`model.rs` |
| F24 | 自动启动：**开机自启**（`SMAppService` 登录项） | `docs/02 §6.5`；`apps/desktop/src/login_item.rs` |
| F25 | 自动恢复：开机自动重连最多 **24 次 × 5 秒（约 2 分钟）**；隧道看门狗 **每 10 秒**真实探测、**连续 2 次失败**自动重建；重建失败**退回直连**；睡眠唤醒后只等 1 次失败 | `apps/desktop/src/commands/core.rs:766/954/956`（重连）、`:402-500`（看门狗）、`:886`（阈值）、`:516`（退回直连） |
| F26 | 崩溃/断电安全：改网络前先落盘会话快照，helper 启动时先回滚残留；退出应用会还原网络配置 | `README.md` §核心问题；`docs/07 §R3`；`lib.rs` 启动回滚路径 |
| F27 | 自更新：从 GitHub Release 下载 zip → 比对 `SHA256SUMS.txt` → 替换应用并重启；**只校验 SHA256、没有签名校验** | `apps/desktop/src/commands/snapshot.rs:502/535`；`docs/07 §5.1(c)` |
| F28 | 流量计数**跨核心重启续接**（重启不会把累计值归零），并如实显示「重启过 N 次」 | `crates/xt-core/src/xray/stats.rs` `MonotonicCounter`（`:357-390`）；`types.ts` `counter_resets` |
| F29 | `dns-out`（UDP 出站）与 `api`（本机回环）的**字节计数器恒为 0，是上游统计盲区**；界面用**连接数**表示活跃度 | `docs/ui/topology/README.md` §v0.8.24；实测 4769 / 5374 条连接 |
| F30 | 「最近连接」的域名是**时序配对**结果（可能不准，约一半连接配得到），界面标注 `*` 与配对时延 | `docs/ui/topology/CONNECTIONS.md` §2/§3；`CHANGELOG.md` 0.8.28 |
| F31 | 官网必须写清的两条诚实边界：**没有分连接字节数与持续时间**（上游没有） | `CONNECTIONS.md` §2「拿不到」 |
| F32 | 命令进程以**普通用户**运行；只有建 utun / 装路由 / 改 DNS 在一个 root helper 里，且 helper 只接受来自本应用的、通过代码签名校验的连接 | `README.md` 三条设计决定；`docs/06` 对端授权 |
| F33 | 首次使用需在设置里**安装特权 helper**（要一次管理员密码） | `.github/workflows/release.yml:167`；`Settings.tsx:426` |
| F34 | 日志/诊断报告在**后端脱敏**：订阅 URL 只留 host、UUID 替换成 `<uuid>` | `docs/05 §3.5`；`apps/desktop/src/commands/diagnostics.rs` |
| F35 | 日志里**不会出现完整订阅 URL**（含 token） | `Subscriptions.tsx` 页面说明；`docs/05 §3.3` |
| F36 | 地球仪的位置查询会把被查的 IP 发给**第三方**（`ipwho.is` 与 `ip-api.com`） | `crates/xt-core/src/geo_lookup.rs`；`docs/05 §3.8` |
| F37 | 仓库**已附带 `LICENSE`（MIT，版权 harodggg 2026）**，与 `Cargo.toml:15` 的 `license = "MIT"` 名实相符（2026-09-20 补上，commit `12c523d`） | `LICENSE` 文件存在；`Cargo.toml:15` |
| F38 | 分发的 Xray-core 采用 **MPL-2.0**，以独立进程调用、不构成衍生作品 | `docs/07 §4.1` |
| F39 | 仅 macOS：**没有 Windows / Linux / 移动端** | `tauri.conf.json` bundle 只产出 macOS 包；`README.md` 打包脚本 |
| F40 | 界面语言目前只有**中文**；规则可视化编辑、节点分组、浅色主题**未实现** | `docs/05 §9 尚未实现` |

**明确不许写的话（本项目的红线，与 `docs/product/PRD.md` 同一套口径）**：

* 不许写「自动配置 macOS 系统代理」——`系统代理` 模式不写系统代理设置（F16）。
* 不许写「每条连接用了多少流量 / 持续多久」——上游没有（F31）。
* **XrayTun 自身是 MIT**（仓库有 `LICENSE`，F37）——不许再写「许可证未声明」；但**不许把随包分发的 Xray-core 说成 MIT**，它是 MPL-2.0（F38）。
* 不许写「已签名 / 已公证 / 安装无提示」——恰恰相反（F9/F10）。
* 不许写「加速 / 解锁流媒体 / 免费节点 / 突破封锁」这类**本产品不提供也不承诺**的效果。
* 不许写「不需要管理员权限」——首次装 helper 要一次管理员密码（F33）。
* 不许写「支持 Windows / Linux / iOS / Android」——没有（F39）。
* 不许写具体延迟数字或速度承诺（没有任何基准数据）。

---

## 1. 信息架构决策

### 1.1 结论

**一个中文单页（`/`）+ 一个英文单页（`/en/`），页内 7 个锚点。不做多页，不做双语 SPA。**

锚点与顺序（**id 沿用 `docs/site/INTERACTION.md` §1.1，不要改**）：

| 顺序 | id | 段落 | 为什么在这个位置 |
|---|---|---|---|
| 1 | `#what` | 一句话是什么（H1 + 事实条） | GEO：实体定义是 AI 摘要最先抽取的部分，必须在最前 |
| 2 | `#why` | 它解决什么问题（用户处境） | 承接定义，回答「我为什么要看下去」；也决定 `#features` 的读法 |
| 3 | `#features` | 核心能力（事实）+ 设计取舍 + 不做什么 | 事实密度最高的部分，供 FAQ 与结构化数据交叉引用 |
| 4 | `#download` | 下载（版本/大小/系统要求/不需要自备核心） | 用户来的首要目的，必须在上半页可达 |
| 5 | `#install` | 安装（含未公证说明，**分版本两条路径**） | 下载之后立刻要给，否则用户卡在 Gatekeeper |
| 6 | `#faq` | 常见问题（完整问答句） | 用户带着安装/能力疑问来；同时供 `FAQPage` 结构化数据 |
| 7 | `#links` | 仓库 / 文档 / 更新记录 / llms.txt | 溯源链，放最后 |

### 1.2 三个决策与理由

**决策 1 · 「与众不同」和「诚实边界」不单独开锚点，合并进 `#features`。**
理由：锚点 id 已被 task-19 固定为 7 个，新增会破坏实现契约；而这两块的内容本质是**能力的事实补充**
（设计取舍 = 为什么这么做；不做什么 = 能力的边界）。合并后 `#features` 内部用三个小标题分层，
对 AI 抽取没有损失，因为每段仍能独立成义。

**决策 2 · 「运行要求与依赖」放在 `#download` 而不是 `#what`。**
理由：`#what` 要保持一句话定义的纯度（AI 摘要）；而「需要什么才能跑」是**决定按不按下下载按钮**的信息，
必须在下载那一屏可见。其中最重要的一条是「**不需要自备 Xray 核心**」（F7）——不写会让用户去找核心、
撞上第二个坑。

**决策 3 · 版本数字写死在 HTML 里，JS 只提示「有新版」。**
理由与 `INTERACTION.md` INT-4-1/4-2 一致：AI 爬虫与禁用 JS 的用户只能看到原始 HTML，
而资产文件名带版本号、链接会随版本失效。所以静态写死 + JS 增强 + 失败静默。
**本文件 §2/§3 里的所有版本数字都是 v0.8.28 的值，发新版时必须整体更新**
（可见文本 / JSON-LD `softwareVersion` / `llms.txt` 三处保持一致，见 INT-4-4）。

---

## 2. 中文站文案（可直接抄进 HTML）

### 2.0 `<head>` 建议

```html
<html lang="zh-Hans">
<title>XrayTun — macOS 上的 Xray 图形客户端（原生 TUN 模式）</title>
<meta name="description" content="XrayTun 是面向 macOS 13 及以上的 Xray 图形客户端，使用 Xray-core 原生 TUN 入站接管系统流量，支持 vmess / vless / trojan / shadowsocks 节点、四种订阅格式、geoip/geosite 分流与 Fake-IP。当前版本 v0.8.28，通用包（Apple Silicon + Intel），包内自带 Xray 核心。">
<link rel="canonical" href="https://xraytun.top/">
<link rel="alternate" hreflang="zh-Hans" href="https://xraytun.top/">
<link rel="alternate" hreflang="en" href="https://xraytun.top/en/">
<link rel="alternate" hreflang="x-default" href="https://xraytun.top/">
```

> 域名/路径以**线上实测**为准（`curl -I`）：canonical 已落地为 `https://xraytun.top/`（中文页）。
> GitHub Pages 的 project 站点子路径只作**镜像**，不再作为 canonical（`INTERACTION.md` INT-0-3/0-4：
> canonical/hreflang/sitemap 必须是绝对 URL，且大小写要按 `curl -I` 的结果写）。

### 2.1 `#what` — 一句话是什么

**H1**
> XrayTun：macOS 上的 Xray 图形客户端，用原生 TUN 模式接管系统流量

**定义段（独立成义，不依赖上下文）**
> XrayTun 是一个面向 macOS 的 Xray 图形客户端。它调用 Xray-core 的原生 `tun` 入站
> （内置 gVisor 协议栈）创建 utun 网卡，用 `0.0.0.0/1` 与 `128.0.0.0/1` 两条路由拆分默认路由，
> 并接管系统 DNS，从而让整台 Mac 的流量按规则走代理。只有「建网卡、装路由、改 DNS」
> 这三件事在特权 helper（root 守护进程）里执行，Xray 核心本身以普通用户身份运行。

**事实条（首屏可见，别做成 JS 渲染的卡片）**

| 项 | 值 |
|---|---|
| 当前版本 | v0.8.28（2026-09-20 发布） |
| 系统要求 | macOS 13.0 或更高 |
| 处理器 | Apple Silicon 与 Intel，通用包（arm64 + x86_64） |
| 安装包 | dmg 45.0 MiB（47,145,126 字节） |
| Xray 核心 | **包内自带**（构建使用 Xray-core v26.9.9），不需要另外安装 |
| 价格/授权 | 源码公开在 GitHub，以 **MIT** 许可发布（仓库有 `LICENSE`，见 `#links`） |

### 2.2 `#why` — 它解决什么问题

> **macOS 的「系统代理」只覆盖遵守代理设置的应用。** 系统代理是一个 HTTP/HTTPS/SOCKS 设置项，
> 很多应用（尤其是自带网络栈的应用、命令行工具、游戏）不会读它。XrayTun 的 TUN 模式在
> 网络层接管默认路由，因此不需要每个应用单独支持代理。
>
> **改系统网络配置是有风险的操作。** TUN 模式要创建网卡、改路由、改 DNS；如果中途崩溃或断电，
> 机器可能就断网了。XrayTun 在动手之前先把「我要开始改了」写进磁盘快照，每改一步增量更新，
> 启动时如果发现上次留下没回滚的会话就立刻回滚；退出应用时也会还原网络配置。
>
> **「连上了」不等于「能用」。** 节点可能握手成功却转发不了流量。XrayTun 的看门狗每 10 秒
> 经隧道发一次真实请求，连续 2 次失败就自动重建隧道；重建失败时退回直连，而不是把你留在断网状态。
>
> **分流规则是否命中，通常只能猜。** XrayTun 内置一个判定器：输入域名或 IP，用运行中的真实规则
> 与 geoip/geosite 数据算出它命中哪条规则、从哪个出口出去。

### 2.3 `#features` — 核心能力（事实）

> 下面每一条都能在 GitHub 仓库的文档里逐条核对（见 `#links`）。这一节同时也说明
> **本应用刻意不做的事情**，因为知道边界比看功能清单更能帮你判断它是否适合你。

**协议与订阅**

* 节点协议支持 **vmess、vless、trojan、shadowsocks、socks、http**。
* **不支持 ShadowsocksR（`ssr://`）**：解析到 `ssr://` 会明确报错，不会静默忽略。
* 订阅支持 **4 种格式**，自动嗅探：Xray JSON 配置、Clash / Mihomo YAML、整体 base64 的链接列表、明文链接列表。
* 订阅正文里混入一两条不支持的链接（例如 `hysteria2://`）不会导致整个订阅失败，只跳过并记入日志。

**分流**

* 内置 **4 个分流预设**：全局代理、绕过大陆（默认）、白名单代理、全部直连。
* 支持 **自定义规则**；自定义规则**追加在预设之后**执行，不会插到预设前面。
* 规则**顺序敏感**：Xray 自上而下取第一条命中的规则，所以「广告拦截」必须排在「大陆直连」之前。
* 支持 **geoip / geosite 匹配**（如 `geoip:cn`、`geosite:cn`）；`geoip.dat` 与 `geosite.dat` 随包附带。
* 规则的界面编辑**目前没有**：自定义规则以只读列表展示，需要直接编辑设置文件。

**DNS**

* **4 种 DNS 策略**：全部走代理解析、按规则分流解析（默认）、全部本地直连解析、自定义服务器。
* 支持 **Fake-IP**，默认关闭。
* 提供 DNS 解析器探测与国内外分组对比（两组走的是不同路径，因此分开测量、分开显示）。

**接管方式与权限**

* 三种模式：**直连 / 系统代理 / TUN**。
* TUN 模式使用 **Xray-core 原生 `tun` 入站**（内置 gVisor 协议栈），不依赖 tun2socks 等旁路进程，UDP 与 QUIC 由同一协议栈处理。
* 路由用 `0.0.0.0/1` 与 `128.0.0.0/1` 拆分而不是替换默认路由：最坏情况是「一部分流量走错路」，而不是「完全没有默认路由」。
* **系统代理模式不会修改 macOS 的系统代理设置**（截至 v0.8.28）。它只在本机启动 SOCKS5（127.0.0.1:10808）与 HTTP（127.0.0.1:10809）入站，需要你自己把应用指向这两个端口；要自动接管请用 TUN 模式。
* 权限最小化：建 utun、装路由、改 DNS 由特权 helper 完成，**Xray 核心以普通用户身份运行**；helper 只接受来自本应用、通过代码签名校验的连接。
* 首次使用需要在设置里安装特权 helper（会要求一次管理员密码）。

**可靠性与更新**

* **开机自启**：通过 macOS 的 `SMAppService` 登录项注册。
* **自动恢复**：开机后若上次是连接状态，会在后台最多重试 24 次 × 5 秒（约 2 分钟）；隧道看门狗每 10 秒探测一次，连续 2 次失败自动重建，重建失败退回直连。换 Wi-Fi 与合盖唤醒属于同一套自愈逻辑。
* **崩溃安全**：改网络配置前先落盘会话快照，每步增量更新；helper 启动时先回滚遗留会话；退出应用会还原网络配置。
* **自更新**：检查 GitHub Release → 下载 zip → 比对 `SHA256SUMS.txt` → 替换应用并重启。
  **这里要如实说明**：只校验 SHA256，**没有签名校验**，因此它能防「下载损坏」，防不了「上游被换掉」。
* **流量计数跨核心重启续接**：核心重启会让 Xray 的累计计数器归零，XrayTun 把归零前的量接上并如实显示「核心重启过 N 次」，而不是把流量显示成突然清零。

**观测（这一块是本项目的设计取舍）**

* **拓扑页**：按运行中的真实配置画出入口 → 规则链 → 出口的结构关系。
  连线表达的是**配置上的结构关系**，不是「某条流量实际走了哪条线」——Xray 只有按入口、按出口的聚合计数器，没有逐条规则的计数器。
* **地球仪**：画出本机公网出口到出口节点的大圆航线，两端标出城市。
  位置查询来自第三方（`ipwho.is` 与 `ip-api.com`），查询会把被查的 IP 发给这些服务；大陆轮廓是 2° 分辨率的粗略示意，不是导航级海岸线。
* **最近连接**：从 Xray 访问日志里取每条连接的时间、来源、目标、`[入站 → 出站]`、域名，点一条就在拓扑上高亮它经过的路径。
  域名来自日志的**时序配对**（`sniffed` 行与 `accepted` 行按时间就近配对），**可能配错**：界面用 `*` 标注并显示精确到微秒的配对时延；约一半连接本来就没有域名。
* **日志与诊断**：等级筛选、来源区分、一键生成诊断报告（在后端脱敏：订阅 URL 只保留 host，UUID 替换为 `<uuid>`）。

**本应用刻意不做（也不打算假装能做）**

* **每条连接用了多少字节、持续多久**：Xray 的统计服务只有聚合计数器，访问日志只记录连接建立、不记录结束。界面里没有这两个数字。
* **`dns-out` 与 `api` 的字节数**：Xray 不统计 UDP 出站流量与本机回环流量，这两个出口的字节计数器恒为 0。XrayTun 把它们单独列为「内部通道」，改用**连接数**表示活跃度；而 `block` 出口的 0 是真的 0。
* **「当前活跃连接数」**：连接从列表里消失不等于已关闭（可能只是被滚动缓冲区挤掉）。
* 没有 Windows / Linux / 移动端版本；界面目前只有中文；没有规则可视化编辑、节点分组、浅色主题。

### 2.4 `#download` — 下载

**主按钮**
> 下载 macOS 版 v0.8.28 · dmg · 45.0 MiB

**真实资产 URL（必须写死在 HTML 的 `href` 里）**

| 文件 | 链接 | 大小 |
|---|---|---|
| dmg（主） | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/XrayTun_0.8.28_x86_64_arm64.dmg` | 47,145,126 字节（45.0 MiB） |
| zip（备用） | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/XrayTun_0.8.28_x86_64_arm64.zip` | 42,647,148 字节（40.7 MiB） |
| 校验和 | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/SHA256SUMS.txt` | 200 字节 |
| 所有版本 | `https://github.com/harodggg/xrayTun/releases/latest` | — |

**运行要求**

* macOS **13.0 或更高**。
* Apple Silicon 与 Intel 都原生支持（同一个通用包）。
* **不需要另外安装 Xray 核心**：包内已包含 Xray-core、`geoip.dat`、`geosite.dat`。
* 需要一个可用的 Xray 节点或订阅链接（本应用不自带节点，也不提供节点服务）。
* 首次使用需要一次管理员密码（安装特权 helper）。

**校验下载文件（可选，防止下载被截断）**

```bash
# 与 release 里的 SHA256SUMS.txt 对比
grep XrayTun_0.8.28_x86_64_arm64.dmg SHA256SUMS.txt
shasum -a 256 XrayTun_0.8.28_x86_64_arm64.dmg
```

> 两行的哈希应当一致。注意 `SHA256SUMS.txt` 由 `shasum -a 256 ./*` 生成，行里带 `./` 前缀，
> 所以用 `grep` 取行再肉眼比对，比直接 `shasum -c` 更稳。

### 2.5 `#install` — 安装（**必须分版本给两条路径**）

> 这一节是整站最重要的部分：**下载下来的包一定会被 macOS 拦下，这不是文件损坏。**
> 这一段必须出现在原始 HTML 里（不能只靠 JS 渲染），并且不能把「右键 → 打开」当成唯一解法。

**第 1 步：拖进「应用程序」**
> 打开下载到的 dmg，把 `XrayTun.app` 拖进「应用程序」文件夹。

**第 2 步：首次打开会被拦住（预期行为，不是包坏了）**
> 你会看到类似「无法验证开发者」或「Apple 无法检查其是否包含恶意软件」的提示。
> 原因是 XrayTun 的安装包是 **ad-hoc 签名、未公证**的：项目仓库里没有 Apple 的
> Developer ID 证书（那需要付费开发者账号），所以 macOS 无法用常规方式验证它。
> 校验结果就是「被拒绝」——这是**预期**的，不是下载出错。

**第 3 步：按你的 macOS 版本放行（二选一 + 一条通用命令）**

> **macOS 15 及更高版本（含 macOS 26）**：双击 XrayTun 被拦下后，打开
> **系统设置 → 隐私与安全性**，在「安全性」一栏找到「已阻止使用『XrayTun』，因为来自身份不明的开发者」，
> 点「**仍要打开**」，用 Touch ID 或密码确认，然后**再双击一次**应用。
> —— 从 macOS 15 开始，Apple 已移除「按 Control 点按图标绕过 Gatekeeper」的做法。
>
> **macOS 14 及更低版本**：在「访达」里按 Control 点按（或右键）`XrayTun.app` 图标 → 选「打开」→
> 在弹窗里再点一次「打开」。
>
> **不想点图形界面 / 上面两步都不成功**：打开「终端」，执行下面这条命令，然后正常双击应用。
>
> ```bash
> xattr -d com.apple.quarantine /Applications/XrayTun.app
> ```
>
> 这条命令只删除系统给下载文件打的「来自互联网」标记。**注意是 `-d`，不是 `-dr`**：
> 现代 macOS 的 `xattr` 没有 `-r` 选项，写成 `-dr` 会报 `option -r not recognized`。

**第 4 步：在应用里安装特权 helper（TUN 模式需要）**
> 打开 XrayTun → 设置 → 特权助手 → 「安装」。这一步会要求一次管理员密码。
> 只有「创建 utun 网卡、安装路由、修改 DNS」这三件事由 helper 执行，Xray 核心本身以你的普通用户身份运行。

**第 5 步：确认装好了**
> 在「设置 → 环境自检」里，helper 应显示「已就绪」；把顶栏的模式切到 **TUN 模式**，
> 添加订阅或节点后点「连接」，能正常上网即安装成功。

**如果不想要了**
> 退出 XrayTun 会还原网络配置；卸载前先退出应用并卸载 helper（helper 的卸载请求会先回滚网络配置再删文件），
> 避免留下指向 utun 的路由或 DNS。

### 2.6 `#faq` — 常见问题

> 每条的**问题就是小标题**、**答案首句就是结论**（同时作为 `FAQPage` 结构化数据的内容，逐字一致）。

**XrayTun 需要我另外安装 Xray 核心吗？**
> 不需要。安装包内已包含 Xray-core 以及 `geoip.dat`、`geosite.dat`，位于应用的
> `Contents/Resources/` 目录，**不需要你另外安装**；你只需要准备节点或订阅链接。

**XrayTun 支持哪些 macOS 版本和处理器？**
> 支持 macOS 13.0 或更高版本，Apple Silicon（arm64）与 Intel（x86_64）都原生支持，
> 两者共用同一个通用安装包。目前只有 macOS 版本，没有 Windows、Linux 或移动端版本。

**打开时提示「无法验证开发者」，怎么办？**
> 这是未公证应用的预期提示，不是安装出错。macOS 15 及以后请走
> 「系统设置 → 隐私与安全性 → 仍要打开」；macOS 14 及更低版本可以右键（Control 点按）应用选「打开」；
> 也可以直接在终端执行 `xattr -d com.apple.quarantine /Applications/XrayTun.app` 后正常打开。

**提示「XrayTun.app 已损坏，无法打开」，是真的损坏了吗？**
> 不是。ad-hoc 签名且未公证的应用会被 Gatekeeper 判定为「被拒绝」，系统用的话术与「损坏」类似。
> 先执行 `xattr -d com.apple.quarantine /Applications/XrayTun.app` 再打开；如果仍然打不开，
> 用 `SHA256SUMS.txt` 校验你下载的文件是否完整（见下载区）。

**为什么不用 Apple 的开发者签名与公证？**
> 因为签名与公证需要付费的 Apple 开发者账号，项目目前没有 Developer ID 证书。
> 代价就是每次首次安装都要手动放行一次——这是已知且公开的取舍，见仓库的 Release 说明。

**为什么一定要安装特权 helper？**
> 因为 TUN 模式必须创建 utun 网卡、修改路由表和系统 DNS，这三件事需要 root 权限。
> helper 是一个只做这三件事的 root 守护进程，且只接受来自 XrayTun 本应用、通过代码签名校验的连接；
> Xray 核心本身仍然以你的普通用户身份运行。

**「系统代理」模式会自动设置 macOS 的系统代理吗？**
> 不会（截至 v0.8.28）。系统代理模式只在本机启动 SOCKS5（127.0.0.1:10808）与 HTTP（127.0.0.1:10809）
> 入站，需要你手动把应用或系统代理指向这两个端口。要让整机流量自动按规则走，请使用 TUN 模式。

**支持哪些节点协议和订阅格式？**
> 节点协议支持 vmess、vless、trojan、shadowsocks、socks、http；
> 订阅支持 Xray JSON、Clash / Mihomo YAML、base64 链接列表、明文链接列表四种格式，会自动识别。

**支持 ShadowsocksR（`ssr://`）吗？**
> 不支持。Xray-core 不提供 `ssr://` 支持，粘贴 `ssr://` 链接时 XrayTun 会明确报错，而不是静默忽略。

**为什么有的出口流量显示 0 B？**
> 因为那是 Xray 统计接口的盲区，不是「没流量」。Xray 不统计 UDP 出站流量（`dns-out`）
> 与本机回环流量（`api`），这两个出口的字节计数器恒为 0；XrayTun 把它们单独列为「内部通道」，
> 改用连接数表示活跃度。而 `block`（拦截）出口的 0 是真的 0，因为连接被拒绝，本来就没有字节。

**「最近连接」里的域名准确吗？**
> 是近似值。Xray 的访问日志里，连接建立行（`accepted`）不带连接 ID，域名出现在另一行（`sniffed`），
> 所以只能按时间就近配对，并发时可能配错。界面用 `*` 标注配对来的域名，并显示这条的配对时延（微秒），
> 由你判断。另外约一半连接本来就没有域名（IP 直连与内部通道没有 `sniffed` 行），这是正常状态。

**一条连接用了多少流量、持续了多久？**
> 看不到，因为 Xray 没有提供这两个数据。统计服务只有按入口、按出口的聚合计数器，
> 访问日志也只记录连接建立、不记录结束。XrayTun 不会用推测值填补这个空缺。

**崩溃或断电会不会把系统网络配置改坏？**
> 会留下残留，但能被自动清掉——这正是本项目投入最多的地方。XrayTun 在修改网络之前先把会话快照落盘，
> 每改一步增量更新；helper 每次启动的第一件事就是回滚磁盘上遗留的会话；正常退出应用时也会还原网络配置。
> 另外如果系统 DNS 还指向隧道内的哨兵地址而隧道已经不在了，界面会明确告诉你原因并给出修复命令。

**换 Wi-Fi、合盖唤醒之后需要手动点「连接」吗？**
> 不需要。看门狗每 10 秒经隧道发一次真实请求，连续 2 次失败就自动重建隧道；开机时若上次是连接状态，
> 会在后台最多重试约 2 分钟，因此开机时 Wi-Fi 还没就绪也能自动连上。重建失败时会退回直连以保证你能上网。
> 从 v0.8.27 起，界面会显示「正在自动恢复（第 N 次）」，你能看到自愈在进行；**v0.8.27 之前**的版本
> 只显示「已连接／未连接」，所以那段时间状态会短暂变成「未连接」——功能一直是自动的，
> 只是更早的界面没把过程画出来。
>
> （按版本锚定而不是「当前版本」：写成「当前版本」会在下一个版本发布后变成假话；
> **锚在「v0.8.27 起 / v0.8.27 之前」这种边界上则永远准确**，不依赖任何人记得改官网。
> v0.8.28 同步：原文只写了旧版的限制（「v0.8.28 及更早不显示进度」），会让读者以为现在也不显示 ——
> 现在把**当前行为**（自 v0.8.27 起显示自愈进度）与历史边界一起写出来。）

**自动更新安全吗？**
> 自动更新会从项目的 GitHub Release 下载压缩包，并用 release 里的 `SHA256SUMS.txt` 校验完整性，
> 校验不通过会拒绝安装。需要如实说明的是：这只校验校验和，**没有签名校验**，
> 因此它能防「下载损坏」，但防不了「上游被替换」。真正的签名 + 公证需要 Developer ID 证书。

**XrayTun 的许可证是什么？**
> 源码在 GitHub 上公开，并以 **MIT** 许可发布：仓库里有 `LICENSE` 文件，
> `Cargo.toml` 也声明 `license = "MIT"`（许可证文件见 `#links`）。
> 随包分发的 Xray-core 采用 MPL-2.0 许可（以独立进程调用，不构成衍生作品），
> 其许可证见 Xray-core 官方仓库；`geoip.dat` / `geosite.dat` 随其上游项目发布。

### 2.7 `#links` — 链接

| 内容 | 绝对链接 |
|---|---|
| 源码仓库 | `https://github.com/harodggg/xrayTun` |
| 全部 Release | `https://github.com/harodggg/xrayTun/releases` |
| 更新记录 | `https://github.com/harodggg/xrayTun/blob/main/CHANGELOG.md` |
| 设计文档 | `https://github.com/harodggg/xrayTun/tree/main/docs` |
| 安装与 TUN 权限说明 | `https://github.com/harodggg/xrayTun/blob/main/docs/02-tun-and-privileges.md` |
| 分流与 DNS 说明 | `https://github.com/harodggg/xrayTun/blob/main/docs/04-routing-and-dns.md` |
| 给 AI 的站点摘要 | `https://xraytun.top/llms.txt`（task-21 产出） |

**页脚许可证声明（中文）**
> 本页内容与 XrayTun 源码公开在 GitHub。XrayTun 以 **MIT** 许可发布
> （仓库有 `LICENSE`，`Cargo.toml` 亦声明 `license = "MIT"`）。随包分发的 Xray-core 为 MPL-2.0，
> 其许可证随 Xray-core 项目分发；`geoip.dat` / `geosite.dat` 随其上游项目发布。

---

## 3. English site copy (equivalent, not a partial translation)

> 这一段是 `/en/` 的完整正文。**与中文版逐段等价**：同样的 7 段、同样的事实、同样的边界。
> 锚点 id 与中文页相同（`#what` `#why` `#features` `#download` `#install` `#faq` `#links`）。

### 3.0 `<head>`

```html
<html lang="en">
<title>XrayTun — A macOS GUI client for Xray with native TUN mode</title>
<meta name="description" content="XrayTun is an Xray GUI client for macOS 13 and later. It uses Xray-core's native TUN inbound to take over system traffic, supports vmess / vless / trojan / shadowsocks nodes, four subscription formats, geoip/geosite routing and Fake-IP. Current version v0.8.28, universal build (Apple Silicon + Intel), Xray core included.">
<link rel="canonical" href="https://xraytun.top/en/">
<link rel="alternate" hreflang="zh-Hans" href="https://xraytun.top/">
<link rel="alternate" hreflang="en" href="https://xraytun.top/en/">
<link rel="alternate" hreflang="x-default" href="https://xraytun.top/">
```

### 3.1 `#what`

**H1**
> XrayTun: an Xray GUI client for macOS that takes over system traffic with native TUN mode

**Definition**
> XrayTun is an Xray GUI client for macOS. It uses Xray-core's native `tun` inbound (with a built-in
> gVisor network stack) to create a utun interface, splits the default route with `0.0.0.0/1` and
> `128.0.0.0/1`, and takes over system DNS so that traffic on the Mac follows routing rules.
> Only three operations run with root privileges inside a privileged helper — creating the utun
> interface, installing routes and changing DNS. The Xray core itself runs as your normal user.

**Facts**

| Item | Value |
|---|---|
| Current version | v0.8.28 (released 2026-09-20) |
| Requirements | macOS 13.0 or later |
| CPU | Apple Silicon and Intel, one universal build (arm64 + x86_64) |
| Download | dmg, 45.0 MiB (47,145,126 bytes) |
| Xray core | **Included in the app** (Xray-core v26.9.9 in this build); you do not install Xray yourself |
| License | Source is public on GitHub, released under **MIT** (a `LICENSE` file is in the repository; see `#links`) |

### 3.2 `#why`

> **macOS system proxy settings only cover apps that honour them.** The system proxy is an
> HTTP/HTTPS/SOCKS setting, and many apps — especially ones with their own network stack, command
> line tools and games — never read it. XrayTun's TUN mode takes over the default route at the
> network layer, so individual apps do not need proxy support.
>
> **Changing system network configuration is a risky operation.** TUN mode creates an interface,
> changes routes and changes DNS. If the process crashes or the machine loses power mid-way, the Mac
> can be left without working networking. XrayTun writes a session snapshot to disk before touching
> anything, updates it incrementally after each step, and rolls back a leftover session on the next
> start. Quitting the app restores the network configuration as well.
>
> **"Connected" does not mean "working".** A node may complete a handshake but fail to forward
> traffic. XrayTun's watchdog sends a real request through the tunnel every 10 seconds and rebuilds
> the tunnel after two consecutive failures. If rebuilding fails, XrayTun falls back to direct
> connection instead of leaving you offline.
>
> **Routing decisions are usually guesswork.** XrayTun includes a rule checker: enter a domain or IP
> and it evaluates the live rules together with geoip/geosite data to tell you which rule matched and
> which outbound the traffic uses.

### 3.3 `#features`

> Every statement below can be checked against the documents in the GitHub repository (see `#links`).
> This section also states what XrayTun deliberately does not do, because knowing the limits matters
> more than reading a feature list.

**Protocols and subscriptions**

* Node protocols: **vmess, vless, trojan, shadowsocks, socks, http**.
* **ShadowsocksR (`ssr://`) is not supported**: parsing an `ssr://` link produces an explicit error rather than being silently ignored.
* Subscriptions: **four formats**, auto-detected — Xray JSON config, Clash / Mihomo YAML, base64-encoded URI list, plain URI list.
* One unsupported link (for example `hysteria2://`) inside a subscription does not fail the whole subscription; it is skipped and logged.

**Routing**

* **Four routing presets**: global proxy, bypass mainland China (default), whitelist proxy, direct for everything.
* **Custom rules** are supported and run **after** the presets, never before them.
* Rule **order matters**: Xray takes the first matching rule from top to bottom, so ad blocking must be listed before mainland-China direct routing.
* **geoip / geosite matching** is supported (for example `geoip:cn`, `geosite:cn`); `geoip.dat` and `geosite.dat` ship with the app.
* There is **no visual rule editor** yet: custom rules are shown read-only and must be edited in the settings file.

**DNS**

* **Four DNS strategies**: resolve everything through the proxy, split by rule (default), resolve everything locally, or use custom servers.
* **Fake-IP** is available and off by default.
* DNS resolver probing compares domestic and foreign resolvers separately, because the two groups use different network paths.

**Traffic capture and privileges**

* Three modes: **direct, system proxy and TUN**.
* TUN mode uses **Xray-core's native `tun` inbound** (built-in gVisor stack). There is no tun2socks sidecar process, and UDP/QUIC are handled by the same stack.
* Routes use a `0.0.0.0/1` + `128.0.0.0/1` split instead of replacing the default route: the worst case is some traffic taking the wrong path, not losing the default route entirely.
* **System proxy mode does not modify macOS system proxy settings** (as of v0.8.28). It only starts local SOCKS5 (127.0.0.1:10808) and HTTP (127.0.0.1:10809) inbounds; you point your apps at those ports yourself. Use TUN mode to capture traffic automatically.
* Least privilege: creating the utun interface, installing routes and changing DNS are done by a privileged helper, while the **Xray core runs as your normal user**. The helper only accepts connections from the app that pass code-signature validation.
* The privileged helper must be installed once from the app's settings, which asks for an administrator password.

**Reliability and updates**

* **Launch at login** through the macOS `SMAppService` login item.
* **Automatic recovery**: if the app was connected when it last exited, it retries in the background up to 24 times with 5-second gaps (about 2 minutes); the tunnel watchdog probes every 10 seconds and rebuilds after two consecutive failures, falling back to direct connection if rebuilding fails. Switching Wi-Fi networks and waking from sleep use the same logic.
* **Crash safety**: a session snapshot is written to disk before network changes, updated incrementally, rolled back by the helper on next start, and reverted when the app quits.
* **Self-update**: check GitHub releases, download the zip, compare against `SHA256SUMS.txt`, replace the app and restart.
  To be explicit: XrayTun only verifies the SHA256 checksum. There is **no signature verification**, so this protects against corrupted downloads, not against a compromised upstream.
* **Traffic counters survive core restarts**: restarting the Xray core resets its counters, and XrayTun carries the previous totals forward and honestly reports "the core restarted N times" instead of showing traffic dropping to zero.

**Visibility features (a deliberate design trade-off)**

* **Topology page**: draws the structure of inbounds → rule chain → outbounds from the live configuration.
  The lines describe **configured structure**, not which line a particular flow actually took: Xray only exposes per-inbound and per-outbound aggregate counters, with no per-rule counters.
* **Globe page**: draws the great-circle route from your public egress to the exit node and labels both ends with a city.
  Location lookup uses third parties (`ipwho.is` and `ip-api.com`), and the queried IP addresses are sent to those services. The continent outlines are a rough 2° raster, not navigational coastlines.
* **Recent connections**: lists time, source, target, `[inbound → outbound]` and domain from the Xray access log; selecting a row highlights that path on the topology.
  Domains come from **time-ordered pairing** of `sniffed` lines with `accepted` lines and **can be wrong**: the UI marks paired domains with `*` and shows the pairing delta in microseconds. About half of all connections have no domain at all.
* **Logs and diagnostics**: level filters, source column, and a one-click diagnostics report that is redacted on the backend (subscription URLs keep only the host, UUIDs become `<uuid>`).

**What XrayTun deliberately does not do**

* **Per-connection byte counts and durations**: Xray's stats service only has aggregate counters, and the access log records connection setup but not teardown. These two numbers are not shown anywhere.
* **Byte counters for `dns-out` and `api`**: Xray does not count UDP outbound traffic or loopback traffic, so those counters stay at 0. XrayTun groups them separately as "internal channels" and uses connection counts instead; the `block` outbound really does carry 0 bytes.
* **"Active connection count"**: a connection disappearing from the list does not mean it closed; it may just have been evicted from the ring buffer.
* No Windows, Linux or mobile builds; the UI is currently Chinese only; no visual rule editor, node grouping or light theme.

### 3.4 `#download`

**Primary button**
> Download for macOS v0.8.28 · dmg · 45.0 MiB

| File | Link | Size |
|---|---|---|
| dmg (primary) | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/XrayTun_0.8.28_x86_64_arm64.dmg` | 47,145,126 bytes (45.0 MiB) |
| zip (alternative) | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/XrayTun_0.8.28_x86_64_arm64.zip` | 42,647,148 bytes (40.7 MiB) |
| Checksums | `https://github.com/harodggg/xrayTun/releases/download/v0.8.28/SHA256SUMS.txt` | 200 bytes |
| All releases | `https://github.com/harodggg/xrayTun/releases/latest` | — |

**Requirements**

* macOS **13.0 or later**.
* Apple Silicon and Intel are both supported natively (same universal build).
* **No separate Xray installation is needed**: the Xray-core binary, `geoip.dat` and `geosite.dat` are inside the app bundle.
* You need your own Xray node or subscription URL. XrayTun does not ship nodes and does not provide a node service.
* One administrator password is required once, to install the privileged helper.

**Verify the download (optional)**

```bash
grep XrayTun_0.8.28_x86_64_arm64.dmg SHA256SUMS.txt
shasum -a 256 XrayTun_0.8.28_x86_64_arm64.dmg
```

> The two hashes should match. `SHA256SUMS.txt` is generated with `shasum -a 256 ./*`, so each line
> starts with `./`; extracting the line with `grep` and comparing it manually is more reliable than
> running `shasum -c` on the file.

### 3.5 `#install`

> This is the most important section on the site: **the downloaded app will be blocked by macOS, and
> that does not mean the file is corrupted.** This content must be present in the raw HTML, and
> "right-click → Open" must not be presented as the only solution.

**Step 1 — Drag the app into Applications**
> Open the downloaded dmg and drag `XrayTun.app` into your Applications folder.

**Step 2 — The first launch is blocked (expected, not a broken download)**
> You may see a message such as "cannot verify the developer" or "Apple cannot check it for malicious
> software". XrayTun is **ad-hoc signed and not notarized**: the project has no Apple Developer ID
> certificate, which requires a paid account, so macOS cannot verify the app the usual way. The
> verification result is "rejected" — this is expected, not a download error.

**Step 3 — Allow it, depending on your macOS version**

> **macOS 15 and later (including macOS 26)**: after the blocked launch, open
> **System Settings → Privacy & Security**, find "XrayTun was blocked from use because it is not from
> an identified developer" in the Security section, click **Open Anyway**, confirm with Touch ID or
> your password, then double-click the app again.
> Starting with macOS 15, Apple removed the Control-click workaround for Gatekeeper.
>
> **macOS 14 and earlier**: in Finder, Control-click (or right-click) `XrayTun.app` and choose
> **Open**, then click **Open** again in the dialog.
>
> **Prefer the terminal, or the steps above did not work**: open Terminal and run the command below,
> then double-click the app normally.
>
> ```bash
> xattr -d com.apple.quarantine /Applications/XrayTun.app
> ```
>
> This only removes the "downloaded from the internet" quarantine flag. Note it is `-d`, **not**
> `-dr`: current versions of `xattr` do not have a `-r` option, and `-dr` fails with
> `option -r not recognized`.

**Step 4 — Install the privileged helper inside the app (TUN mode needs it)**
> Open XrayTun, go to Settings → Privileged helper → Install. This asks for an administrator password
> once. Only interface creation, route installation and DNS changes run inside the helper; the Xray
> core still runs as your normal user.

**Step 5 — Confirm the installation worked**
> In Settings → Environment check, the helper should show "ready". Switch the mode in the top bar to
> **TUN**, add a subscription or node, click Connect, and normal browsing means you are done.

**Removing it later**
> Quitting XrayTun restores the network configuration. Before deleting the app, quit it and uninstall
> the helper (the helper's uninstall request rolls back the network configuration before removing
> itself) so no routes or DNS settings pointing at the utun interface are left behind.

### 3.6 `#faq`

**Do I need to install Xray separately?**
> No. The app bundle already contains the Xray-core binary plus `geoip.dat` and `geosite.dat` under
> `Contents/Resources/`. You only need a node or a subscription URL.

**Which macOS versions and CPUs are supported?**
> macOS 13.0 or later, on both Apple Silicon (arm64) and Intel (x86_64), from one universal build.
> There is no Windows, Linux or mobile version.

**The app says "cannot verify the developer". What should I do?**
> That is the expected prompt for a non-notarized app, not an installation failure. On macOS 15 and
> later, use System Settings → Privacy & Security → Open Anyway. On macOS 14 and earlier, right-click
> (Control-click) the app and choose Open. Alternatively run
> `xattr -d com.apple.quarantine /Applications/XrayTun.app` in Terminal and open the app again.

**It says "XrayTun.app is damaged and can't be opened" — is the file really damaged?**
> No. Gatekeeper rejects ad-hoc signed, non-notarized apps, and the wording resembles a corruption
> message. Run `xattr -d com.apple.quarantine /Applications/XrayTun.app` first; if it still fails,
> verify your download against `SHA256SUMS.txt` (see the download section).

**Why is the app not signed with a Developer ID and notarized?**
> Because signing and notarization require a paid Apple developer account, and the project has no
> Developer ID certificate. The trade-off is a manual approval on first launch, and it is documented
> publicly in the release notes.

**Why does XrayTun need to install a privileged helper?**
> Because creating the utun interface, changing the routing table and changing system DNS all require
> root. The helper is a root daemon that does only those three things and only accepts connections
> from the XrayTun app that pass code-signature validation. The Xray core itself keeps running as your
> normal user.

**Does "system proxy" mode configure the macOS system proxy automatically?**
> No (as of v0.8.28). System proxy mode only starts local SOCKS5 (127.0.0.1:10808) and HTTP
> (127.0.0.1:10809) inbounds; you point your apps or system proxy settings at those ports yourself.
> To capture all traffic automatically, use TUN mode.

**Which node protocols and subscription formats are supported?**
> Node protocols: vmess, vless, trojan, shadowsocks, socks and http. Subscriptions: Xray JSON,
> Clash / Mihomo YAML, base64-encoded URI lists and plain URI lists, auto-detected.

**Is ShadowsocksR (`ssr://`) supported?**
> No. Xray-core does not support `ssr://`, so pasting an `ssr://` link produces an explicit error
> instead of being silently ignored.

**Why do some outbounds show 0 B of traffic?**
> Because that is a blind spot in Xray's statistics, not an absence of traffic. Xray does not count
> UDP outbound traffic (`dns-out`) or loopback traffic (`api`), so those byte counters stay at 0.
> XrayTun lists them separately as "internal channels" and uses connection counts instead. The `block`
> outbound really is 0 bytes, because connections are rejected before any data flows.

**How accurate are the domains in "recent connections"?**
> They are approximate. In Xray's access log, the connection setup line (`accepted`) carries no
> connection ID, while the domain appears on a separate `sniffed` line, so domains can only be paired
> by time, and concurrent connections can be paired incorrectly. The UI marks paired domains with `*`
> and shows the pairing delta in microseconds so you can judge. About half of all connections have no
> domain at all, which is normal for IP-based and internal traffic.

**How much data did one connection transfer, and how long did it last?**
> Not available, because Xray does not provide either number. The stats service only exposes
> per-inbound and per-outbound aggregate counters, and the access log records setup but not teardown.
> XrayTun does not fill the gap with estimates.

**Can a crash or power loss break my network configuration?**
> It can leave residue, and XrayTun is built to clean it up: a session snapshot is written before any
> network change and updated incrementally, the helper rolls back any leftover session on start, and
> quitting the app restores the configuration. If system DNS still points at the in-tunnel sentinel
> address while the tunnel is gone, the app states the cause and gives you the exact fix command.

**Do I have to reconnect manually after switching Wi-Fi or waking from sleep?**
> No. The watchdog sends a real request through the tunnel every 10 seconds and rebuilds after two
> consecutive failures; after login, if the app was connected when it last exited, it retries in the
> background for about two minutes so it can come up before Wi-Fi is ready. If rebuilding fails it
> falls back to direct connection.
> Since v0.8.27 the UI shows **"recovering (attempt N)"**, so you can watch the recovery happen;
> **before v0.8.27** it only showed "connected" or "disconnected", so you may briefly see
> "disconnected" while it recovers in the background.
>
> (Anchored to the version rather than "the current version": "the current version" would become
> false at the next release, whereas anchoring on the v0.8.27 boundary stays accurate forever.
> v0.8.28 sync: the original text only stated the old limitation, which made it read as if the
> progress indicator did not exist today — it now states the current behaviour plus the boundary.)

**Is the auto-updater secure?**
> The updater downloads the zip from the project's GitHub release and verifies it against
> `SHA256SUMS.txt`, refusing to install if the checksum does not match. To be explicit: there is
> **no signature verification**, so it protects against corrupted downloads, not against a
> compromised upstream. Real signing and notarization require a Developer ID certificate.

**What licence does XrayTun use?**
> The source is public on GitHub and released under the **MIT** licence: the repository contains a
> `LICENSE` file and `Cargo.toml` declares `license = "MIT"` (see `#links`). The bundled Xray-core is
> licensed under MPL-2.0 (invoked as a separate process, not a derivative work); see the Xray-core
> repository for its licence, and `geoip.dat` / `geosite.dat` ship with their upstream project.

### 3.7 `#links` and footer

| Item | Absolute link |
|---|---|
| Source repository | `https://github.com/harodggg/xrayTun` |
| All releases | `https://github.com/harodggg/xrayTun/releases` |
| Changelog | `https://github.com/harodggg/xrayTun/blob/main/CHANGELOG.md` |
| Documentation | `https://github.com/harodggg/xrayTun/tree/main/docs` |
| TUN and privileges | `https://github.com/harodggg/xrayTun/blob/main/docs/02-tun-and-privileges.md` |
| Routing and DNS | `https://github.com/harodggg/xrayTun/blob/main/docs/04-routing-and-dns.md` |
| AI summary for this site | `https://xraytun.top/llms.txt` (produced by task-21) |

**Footer licence statement (English)**
> The site content and the XrayTun source are public on GitHub. XrayTun is released under the
> **MIT** licence (the repository contains a `LICENSE` file and `Cargo.toml` declares
> `license = "MIT"`). The bundled Xray-core is licensed under MPL-2.0, and its licence ships with the
> Xray-core project; `geoip.dat` and `geosite.dat` ship with their upstream project.

---

## 4. 结构化数据（JSON-LD）

**挂载位置**：`<head>` 内两个 `<script type="application/ld+json">`（中文页与英文页各一份，
`FAQPage` 的问答与页面可见正文**逐字一致**）。

```json
{
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  "name": "XrayTun",
  "applicationCategory": "UtilitiesApplication",
  "operatingSystem": "macOS 13.0 or later",
  "softwareVersion": "0.8.28",
  "datePublished": "2026-09-20",
  "downloadUrl": "https://github.com/harodggg/xrayTun/releases/download/v0.8.28/XrayTun_0.8.28_x86_64_arm64.dmg",
  "fileSize": "47145126",
  "softwareRequirements": "macOS 13.0 or later; Apple Silicon or Intel; Xray node or subscription required",
  "offers": { "@type": "Offer", "price": "0", "priceCurrency": "CNY" },
  "url": "https://xraytun.top/",
  "sameAs": ["https://github.com/harodggg/xrayTun"]
}
```

```json
{
  "@context": "https://schema.org",
  "@type": "FAQPage",
  "mainEntity": [
    { "@type": "Question", "name": "XrayTun 需要我另外安装 Xray 核心吗？",
      "acceptedAnswer": { "@type": "Answer", "text": "不需要。安装包内已包含 Xray-core 以及 geoip.dat、geosite.dat，位于应用的 Contents/Resources/ 目录。你只需要准备节点或订阅链接。" } },
    { "@type": "Question", "name": "打开时提示「无法验证开发者」，怎么办？",
      "acceptedAnswer": { "@type": "Answer", "text": "这是未公证应用的预期提示，不是安装出错。macOS 15 及以后请走「系统设置 → 隐私与安全性 → 仍要打开」；macOS 14 及更低版本可以右键（Control 点按）应用选「打开」；也可以执行 xattr -d com.apple.quarantine /Applications/XrayTun.app 后正常打开。" } },
    { "@type": "Question", "name": "「系统代理」模式会自动设置 macOS 的系统代理吗？",
      "acceptedAnswer": { "@type": "Answer", "text": "不会（截至 v0.8.28）。系统代理模式只在本机启动 SOCKS5（127.0.0.1:10808）与 HTTP（127.0.0.1:10809）入站，需要你手动指向这两个端口；要让整机流量自动按规则走，请使用 TUN 模式。" } },
    { "@type": "Question", "name": "为什么有的出口流量显示 0 B？",
      "acceptedAnswer": { "@type": "Answer", "text": "因为那是 Xray 统计接口的盲区：Xray 不统计 UDP 出站流量（dns-out）与本机回环流量（api），这两个出口的字节计数器恒为 0，XrayTun 改用连接数表示活跃度；block 出口的 0 是真的 0。" } },
    { "@type": "Question", "name": "一条连接用了多少流量、持续了多久？",
      "acceptedAnswer": { "@type": "Answer", "text": "看不到，因为 Xray 没有提供这两个数据：统计服务只有按入口、按出口的聚合计数器，访问日志只记录连接建立、不记录结束。XrayTun 不会用推测值填补这个空缺。" } }
  ]
}
```

> `FAQPage` 只列 5 条是**内容决策**：这 5 条是「最容易被误信、且 AI 最容易合成错误结论」的问答；
> 页面正文仍给全部 16 条。若 task-21 希望结构化数据覆盖全部问答，可直接扩 `mainEntity` 数组，
> 但**必须与可见正文逐字一致**（INT-4-4 的一致性要求）。

---

## 5. GEO 自检清单（写完后逐条核对）

| # | 要求 | 在哪满足 |
|---|---|---|
| G1 | 事实密度高（不用形容词替代事实） | `#what` 事实条、`#features` 全部条目都带具体名词/数字；全站未使用「强大 / 高效 / 稳定」这类词 |
| G2 | 每段独立成义（无「如上所述」「它」） | 中英各段均以完整主谓句开头；`#features` 每条形如「XrayTun 支持…」；`#why` 每段自足 |
| G3 | FAQ 是完整问答句 | §2.6 / §3.6 每条问题即小标题、答案首句即结论 |
| G4 | 给数字 | 版本 v0.8.28、发布日期、dmg 47,145,126 B、zip 42,647,148 B、macOS 13.0+、重连 24×5s、看门狗 10s/2 次、核心 v26.9.9 |
| G5 | 依赖说清（是否需自备核心） | `#what` 事实条 + `#download` 运行要求 + FAQ 第 1 条：**包内自带，不需要自备** |
| G6 | 中英双语且等价 | §2 与 §3 段落一一对应（7 段 + 17 条 FAQ）；`/` 与 `/en/` 各为完整单页 |
| G7 | 关键内容在原始 HTML 里 | `INTERACTION.md` §6.1 的 8 条在 §2/§3 中都以普通正文给出，无「JS 渲染后出现」的依赖 |
| G8 | 下载区给真实资产 URL | §2.4 / §3.4 用具体 tag 的绝对资产 URL + 校验和 + `releases/latest` |
| G9 | 安装说明不出现唯一解「右键 → 打开」 | §2.5 / §3.5 分版本给「系统设置 → 仍要打开」「右键打开」「`xattr -d`」三条，且明确 `-d` 不是 `-dr` |
| G10 | 许可证表述必须与事实一致 | §2.6/§3.6（FAQ）、§2.7/§3.7（页脚）、`#what` 事实条、JSON-LD `license`：**XrayTun 是 MIT（仓库有 LICENSE，2026-09-20 起）**，随包 Xray-core 是 MPL-2.0（F37/F38） |

---

## 6. 最容易误写的三处事实（**给 lead 的简报重点**）

1. **安装步骤：照抄仓库现有的 Release Notes 会让用户装不上。**
   `release.yml:163` 与 `README.md:106` 写的是「右键 →「打开」」+ `xattr -dr`。
   实测：macOS 15 起 Apple 已移除 Control-点击绕过（本机 macOS 26.6.2 属受影响范围）；
   现代 `xattr` **没有 `-r`**，`xattr -dr` 直接报 `option -r not recognized`。
   官网必须写**分版本两条路径** + `xattr -d`（无 `r`）。这也是唯一一处「照抄我们自己的文档就装不上」。
2. **「系统代理」模式不写系统代理设置。** 很容易顺手写成「自动配置系统代理」——
   实测全仓库没有任何 `networksetup -setwebproxy` 调用（只有 DNS 相关），helper 协议里也没有代理请求。
   官网只能写「提供本机 SOCKS5/HTTP 入站，需要你自己指向」。
3. **许可证要写对（2026-09-20 起已变）。** 仓库**已有 `LICENSE`（MIT，版权 harodggg 2026）**，
   `Cargo.toml:15` 的 `license = "MIT"` 名实相符 —— 所以官网**应当**如实写「源码以 MIT 发布」；
   不许再写「许可证未声明」。唯一仍要小心的是**随包分发的 Xray-core 是 MPL-2.0**（F38），
   不要把它说成 MIT。

**其余容易写错的点（同样会误导用户）**：

* 「需要自备 Xray 核心」——错，包内自带（F7）。
* 「不需要管理员权限」——错，首次装 helper 要一次管理员密码（F33）。
* 「支持 Windows / Linux / 手机」——错（F39）。
* 任何关于速度、延迟、解锁流媒体的承诺——没有数据，不许写。
* 「界面支持英文」——错，目前只有中文界面（F40）；官网有英文版说的是**网站**，不是应用界面。

---

## 7. 待补 / 未确认

1. **域名与仓库元数据**：**已落地** —— 站点 canonical 为 `https://xraytun.top/`
   （中英两个首页与 `/wasm/`、`/en/wasm/` 各自 canonical + 三向 hreflang，
   `x-default` 指中文页）。2026-09-20 已从**线上** `curl` 核对：4 页 canonical 与
   `sitemap.xml` 的 4 个 `<loc>` 全部为 `xraytun.top`。
   GitHub Pages 的 project 站点子路径仅作镜像（canonical 指向 apex）。
2. **`llms.txt`（task-21）**：本文件提到它，但内容由 task-21 产出。要求：与本站 `#what`/`#features`/`#faq` 的
   事实**逐字不冲突**，版本号三处一致（INT-4-4）。
3. **截图与界面语言**：`#install` 第 2 步建议配 dmg 窗口截图；应用界面是中文，
   英文页若要放界面截图，需要在图注里说明「应用界面目前为中文」。截图由 task-19/21 处理，
   本文件不提供图片。
4. **包内核心版本**：F8 的 v26.9.9 取自 `scripts/fetch-xray.sh` 的默认值，且 release.yml 未覆盖 `XRAY_VERSION`；
   若以后 CI 用别的版本构建，官网这一条要跟着改（建议由构建时注入，而不是手抄）。
5. **未确认**：我没有在真机图形会话里走完 Gatekeeper 的 GUI 弹窗（`INTERACTION.md` §10 已声明同一限制），
   所以 §2.5 里对弹窗**措辞**的描述是「类似这样的提示」，不是逐字引用。
6. **`fileSize` 单位**：JSON-LD 的 `fileSize` 用字节数（47,145,126）而不是 "45.0 MiB"，
   因为 schema.org 的 `fileSize` 是文本字段且解析器对单位处理不一致；可见文本两处都写。
