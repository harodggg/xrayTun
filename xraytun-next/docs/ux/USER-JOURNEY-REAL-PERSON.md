# 真人走查：刚下载完 App 的普通 Mac 用户，能用吗？

> 作者：lead（含独立「真人模拟体验官」复核，见 §5）。
> 对象：已发布 **xraytun-next v1.2.0**（tag `xraytun-next-v1.2.0`）。
> 人物：会用微信、会复制粘贴、知道「机场/订阅」这个词，但**不懂 daemon / helper / socket / TUN**。
> 目标只有一句：**我要能翻墙** —— 具体动作是「导入节点 → 选节点 → 点开始」。

## 0. 一句话结论（也是我的检讨）

**这一版对一个刚下载的真人来说：走不通。** 三条主路径断了两条半：**没有地方能导入节点**、
**启动必然失败（TUN 需要的特权 helper 从来没有安装机制）**。

我上一轮把「TUN 通路已宣告」当成了「产品可用」，并据此发布了 v1.2.0。这是**用工程视角验收**：
我验的是「字段对不对、测试绿不绿、CI 通不通」，而真人要的是「我的机场订阅怎么进去、按下开始会发生什么」。
功能清单 ≠ 用户路径。这条错误应记在本文件与 release notes 里，不该被含糊过去。

**独立的「真人模拟体验官」（另一个上下文的 agent，只从代码/真机截图出发、不带我的结论）得出同一结论**，
并且多找出**三件我漏掉的事**：helper 的信任策略会 fail-closed（死路 5）、包里的 `xray` 从没被指给 daemon
（死路 6），以及一条排序警告：**只修「导入」仍然翻不了墙**。它的三条发现我已逐行核实（见各条证据）。

## 1. 真人走查（逐步）

| # | 他会做什么 | 屏幕上实际出现什么（逐字） | 结果 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | 下载 dmg → 拖进 `/Applications` → 双击 | Gatekeeper 拦下（**未公证**）：需要他自己执行 `xattr -dr com.apple.quarantine` | ⚠️ | `docs/release-notes/v1.2.0.md`（诚实声明）；`docs/design/MACOS-APP-PLAN.md` S5 未做 |
| 2 | 打开 App，看第一屏 | 顶栏「未连接」；仪表板大按钮写着 **「添加订阅」**，下面一行 **「节点目录为空 —— 启动前先添加订阅」**；流量区「未采样」 | ✅ 引导是对的 | `apps/ui/src/components/startButton.ts`（`nodeCount === 0` 分支）、`apps/ui/src/pages/Dashboard.tsx` |
| 3 | 照提示按下「添加订阅」 | 跳到**设置页**的「订阅」区，看到 **「暂无订阅」** + 「订阅列表来自 daemon 的真实记录。」——**没有任何输入框、没有按钮、没有「+」** | ⛔ **死路** | `Settings.tsx:16-17,33-34,167-171`：整块「添加 / 刷新」入口由 `capability = subscription_fetch` 门控，而 daemon **永不宣告**它（`crates/xt-daemon/src/lib.rs:323-328`） |
| 4 | 不甘心，去「节点」页看看 | 「暂无节点」+「节点目录为空。本页不会填充示例节点，也不会给出无法兑现的操作。」——**也没有导入入口** | ⛔ 死路 | `apps/ui/src/pages/Nodes.tsx` 空态；该页只有探测 / 切换 / 选择 |
| 5 | 假设他**莫名**有了节点（比如开发者手工放了订阅文件），按「启动」 | daemon 受理 `Connect{tun}` → 去连特权助手 socket → 失败：**`连接 helper socket /var/run/xraytun-helper.sock 失败：No such file or directory (os error 2)`**；仪表板显示「上次运行出错」+ `helper_unavailable` | ⛔ 死路 | `Dashboard.tsx`（Connect 恒发 `tun`）→ `crates/xt-daemon/src/flow.rs::connect_tun_flow` → `crates/xt-daemon/src/helper.rs:87-93` |
| 6 | 想自己解决「助手没装」 | 界面上没有任何「安装 / 授权 / 重装助手」的入口；全仓也**没有** launchd / plist / 安装逻辑 | ⛔ 死路 | 全仓搜 `launchd`/`plist`/`SMAppService`：只命中 `docs/design/MACOS-APP-PLAN.md`（**计划的 S2 阶段，未实现**；该文件 S 表里 S2 helper 的真机验收列就是 ❌） |

**对照他以前用的 v0.9.2**：老版订阅页有「备注名（可留空）」「`https://example.com/api/v1/client/subscribe?token=…`」
「**添加并拉取**」三个可见控件（`../xray-tun/apps/ui/src/pages/Subscriptions.tsx:56-81`），节点页有「手动添加」，
设置页有「重装助手」（`../xray-tun/apps/ui/src/App.tsx:207`）。
⇒ 新版的问题**不是「功能少」，而是主路径断了**。

## 2. 死路清单

### 死路 1：节点进不来（UI 里没有任何导入入口）
- **他看到什么**：「暂无订阅」+「订阅列表来自 daemon 的真实记录。」
- **他会怎么说**：「我订阅链接往哪贴？」
- **根因**：`AddSubscription` / `RefreshSubscription` 在 daemon 里直接回 `Unsupported`
  （`crates/xt-daemon/src/lib.rs:753-758`），`SubscriptionFetch` 不宣告；UI 的添加入口被 `subscription_fetch` 门控
  ⇒ 入口根本不存在。当前唯一的节点来源是**本地文件**：`--subscription-file`，缺省 `<state-dir>/subscription.txt`
  （`crates/xt-daemon/src/main.rs:33-34`）；文件不存在时 daemon **静默返回空目录**（`lib.rs:576-586`），
  界面上不解释「为什么没有节点」，也不告诉用户要放哪个文件。
- **最少需要什么**：一个真人能用的导入入口 —— ① 粘贴订阅 URL 并拉取；② 粘贴分享链接/导入文本。

### 死路 2：点「启动」必然失败（helper 从未安装）
- **他看到什么**：`helper_unavailable` + 「连接 helper socket /var/run/xraytun-helper.sock 失败：No such file or directory」。
- **他会怎么说**：「什么是 helper socket？」
- **根因**：TUN 需要 root 守护进程 `xt-helperd` 在 `/var/run/xraytun-helper.sock` 上服务；
  发布包**只把 `xt-helperd` 放进 `.app/Contents/Resources`**（release 工作流），**没有任何安装/启动它的代码**
  （全仓无 launchd/plist）。`docs/design/MACOS-APP-PLAN.md` S2 的既定做法就是
  「root 守护进程 + launchd plist + 安装/卸载（走一次用户授权）+ 对端校验」——这一阶段没做。
- **最少需要什么**：helper 安装链路（安装 → 用户授权 → launchd 启动 → 卸载/回滚），以及界面上的一条引导。

### 死路 3：错误话术不是给人看的
- **他会说什么**：「`helper_unavailable` 是啥？socket 在哪？」
- **根因**：`ErrorBox` 忠实显示 daemon 的 `code` + `message`（这符合本仓库「不翻译、不美化」的规矩），
  但**没有给「下一步」**：装助手 / 换节点 / 看日志这些动作，老版是有的（`failure.ts` 的 `nextSteps`/`failureActions`）。
- **最少需要什么**：失败时给**用户动作**（尤其「安装特权助手」），而不是只报码。

### 死路 5：helper 就算被手动跑起来，也会**拒绝一切特权操作**（fail-closed）
- **他会说什么**：「我明明让它跑起来了，它凭什么不干活？」
- **根因（已核实）**：`crates/xt-helperd/src/trust.rs:165` 把「已安装组件」写死为
  `/Applications/XrayTun.app/Contents/MacOS/xt-daemon`（注释自己写着「**待真机**：最终路径要与 S3/S4 的
  `.app` 打包结果对齐」），而发布包把二进制放在 **`Contents/Resources/`**
  （`apps/desktop/tauri.conf.json` 的 `resources`；发布工作流也按 `Contents/Resources/*` 验签）。
  同时发布工作流只注入 `APPLE_TEAM_ID`（第 348 行，公证用），**没有**注入编译期的 `XRAYTUN_TEAM_ID`
  ⇒ `TrustPolicy::policy_for(None, 不存在的路径)` 落到 `RefuseService`（`trust.rs:186-210`）。
- **最少需要什么**：安装位置与信任判据必须**对齐**（要么把 daemon 放到 helper 期望的位置，要么改常量到
  实装位置），并在发行时注入 Team ID（或明确走 cdhash 绑定那条路）。

### 死路 6：包里的 `xray` **从没被指给** daemon（helper 修好后立刻会炸）
- **他会说什么**：「xray 不是你们自己打包带进来的吗？」
- **根因（已核实）**：包内有 `Contents/Resources/xray`，但 daemon 的查找顺序是
  `--xray` > `XT_XRAY_BIN` > `PATH` 里的 `xray`（`crates/xt-daemon/src/main.rs:80-85`），
  而壳拉起 daemon 时**只传 `--socket` 与可选的 `--state-dir`**（`apps/desktop/src/daemon_launch.rs:117-118`，
  它的注释写着「`--xray` / `XT_XRAY_BIN` 由它自己从环境读，壳不替它决定」——但 GUI 启动的 daemon 环境里
  并没有这个变量）⇒ `validate_config` 报 `datapath_unavailable`「xray 二进制不存在或不是文件：xray」
  （`crates/xt-datapath/src/lib.rs:217-223`）。
- **最少需要什么**：App 必须把包里那份 `xray` 指给 daemon（`--xray` 或 `XT_XRAY_BIN` 指向 bundle 路径）。

## 3. 需求分档（按「用户为什么需要」分，不按技术）

### 没有它就用不了（P0）
1. **导入机场订阅 URL 并拉取**：绝大多数用户手上只有一个订阅链接。要能「粘贴 → 一键拉取 → 节点出现」。
2. **手动导入节点**（粘贴分享链接 `vless://` / `vmess://` / `ss://`，或导入订阅原文）：机场外的人只有单条链接；
   也是「订阅拉取失败」时的兜底**入口**（不是自动回落，是让用户自己再走一条明确的路）。
3. **helper 安装引导**：TUN 是这一版唯一模式 ⇒ 不装 helper = App 永远连不上。
   用户需要看到「为什么需要它 / 会不会改坏系统 / 怎么卸载」。

### 没有它很难用（P1）
4. **失败时给下一步动作**（装助手 / 换节点 / 看日志）。
5. **订阅状态可读**：这个订阅什么时候拉的、拉到了几个节点、上次失败原因（`SubscriptionView` 已有字段，界面要显示）。
6. **节点可用性一眼可读**：延迟/不可用（已有 `ProbeResult`，但需要用户点一下才探测；至少要把「未探测」和「不可用」分清楚）。

### 锦上添花（P2）
7. 扫码/二维码导入；8. 订阅分组与备注名；9. 托盘快捷连接；10. 更新检查。

## 4. 最小可用路径（v1.2.1）与成本

| 步 | 要做的 | 成本 / 依赖决策 |
| --- | --- | --- |
| A | **粘贴分享链接导入**（`xt-subs::parse_links` 已存在）→ 新区块 + daemon 加一条「导入文本」请求 + 落盘 + 刷新目录 | **零新增依赖**；UI + daemon 各一小块；可当天做完 |
| B | **订阅 URL 拉取**：daemon 侧实现 `AddSubscription` / `RefreshSubscription` + 宣告 `SubscriptionFetch` + UI 解除门控 | **原计划：新增 HTTPS 依赖**（当时工作区一个 TLS/HTTP 客户端都没有）⇒ **2026-10-16 最终决定：用 `/usr/bin/curl`（绝对路径 + argv），零新增依赖** —— 这是老版 v0.9.2 对同一功能的结论，见 §4.1 |
| C | **helper 安装**：`xt-helperd` 安装（拷贝到固定位置 + launchd plist + 用户授权）+ 卸载/回滚 + 界面引导 | 最大的一块；`MACOS-APP-PLAN` S2；**只有真机能验**（CI 装 launchd 不可靠，设计文档已写明） |

### 第 1 轮进展（2026-10-01 · commit `4951a2e` + `ea45303` · CI 全绿）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| **A 导入入口** | ✅ **已打通** | 节点页新增导入区（粘贴订阅原文 / 分享链接，由 `capability = node_import` 门控）；daemon `ImportNodes`：`xt-subs` 四格式自动嗅探 → 按节点 id 合并去重 → `<state-dir>/imported-nodes.json` 原子落盘 → 启动装载。Rust e2e 新增 §4.5：CI（Linux 真 xray）上断言「2 个订阅节点 + 1 个手工导入 = 3，且来源为 `Manual`」；两条新单测（合并去重 / 磁盘形状往返）在 CI 里 ok。UI 新增 7 条验收断言；本机 `vitest` 79 passed / `tsc` 0 错 / `vite build` 成功 / `guard.sh` PASSED |
| **D6 包里的 xray** | ✅ 已修 | `apps/desktop` 新增 `resolve_xray()`，把 `--xray <bundle>/Resources/xray` 传给 daemon（GUI 进程 PATH 里没有 xray，打包后必然 `datapath_unavailable`） |
| **D5 helper 信任路径** | ✅ 路径部分已修 | `trust.rs` 的 `INSTALLED_APP_BINARY` 改为 `Contents/Resources/xt-daemon`（与 `tauri.conf.json` 的资源布局、发布工作流验签路径一致）。**Team ID 注入仍未做**（要 Developer ID，属 S5） |
| **C helper 安装链路** | ⛔ 未做（下一轮） | 没有它 TUN 仍然连不上；只有真机能验 |
| **B 订阅 URL 拉取** | ⛔ 未做 | 需先选 HTTP/TLS 依赖 |
| **D4 失败时给下一步** | ⛔ 未做 | 现在失败仍只给「查看日志 / 重新连接」 |

### 第 2 轮进展（2026-10-01 · commit `fce5b94` · CI 全绿 `38881300652`）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| **C helper 安装链路** | ✅ **代码完成**（真机待验） | 壳新增 `apps/desktop/src/helper_install.rs`：launchd plist + 安装/卸载/重启脚本（方式 B：`/Library/PrivilegedHelperTools` + `/Library/LaunchDaemons` + **一次管理员授权**）；四个命令 `helper_status` / `install_helper` / `uninstall_helper` / `restart_helper`；设置页**第一张卡**「特权助手（TUN 需要）」说清为什么需要、只认识固定指令集、卸载会删干净 |
| **D4 失败给下一步** | ✅ | 仪表板的「上次运行出错」在 `code = helper_unavailable` 时给出「去安装特权助手」→ 设置页那张卡 |
| CI 补的洞 | ✅ | macos-app 新增 `cargo test -p xraytun-desktop --lib`：**壳里的纯函数（那两段会以 root 执行的脚本文本）以前没有任何 CI 跑过**。现在 5 条单测在 CI 上 `5 passed; 0 failed`（plist 逐字含 socket/RunAtLoad/KeepAlive；安装脚本 5 项要点；卸载删三样；无未替换占位符；AppleScript 转义） |
| **B 订阅 URL 拉取** | ⛔ 未做 | 需要新增 HTTPS/TLS 依赖（工作区现在一个 TLS 客户端都没有），等选型 |
| **真机验收（S6）** | ⏳ **只有你能做** | 授权弹窗、`launchctl bootstrap`、装完能不能真的接管网络 —— CI 上装 launchd daemon 不可靠，设计文档也这么写 |

**现在的口径**：三条主路径在**代码层面**都通了（导入节点 → 选节点 → 点开始；没装助手时界面直接引导安装）。
但「真人能用」这句话**还没有真机证据** —— 需要你真机装一次、过一次授权弹窗、连一次、再卸一次。
在那之前我不会说「能翻墙了」。

**进度口径（别自我美化）**：一个真人现在能做到「导入节点 → 选节点」，但按下「启动」**仍然会撞上「helper 没装」**。
所以这一轮把「界面里根本没有入口」变成了「有入口、能进来」，**离「能翻墙」还差 C 那一步**。

**顺序建议（已被体验官复核修正）**：
**A（导入入口）→ D6（把包里的 xray 指给 daemon）→ D5（helper 信任对齐）→ C（helper 安装链路）→ D4（失败时给下一步）→ B（订阅 URL 拉取）。**

- **先 A**：它是唯一能在**界面层立刻**把「死路」变成「有内容」的改动（不动依赖、可本地验），
  但**必须同时说清**：只做 A 用户**仍然翻不了墙**。
- **D6 / D5 是两颗已核实的定时炸弹**，不修的话 C 做完也连不上；它们改动都很小（一个传参、一个常量+注入），
  应该排在 C 前面或与 C 同批。
- **C 才是「能不能翻墙」的关键**，且**只有真机能验**（CI 上装 launchd daemon 不可靠，设计文档已写明）——
  不能因为「CI 装不了」就把这一步无限期推迟。
- **B 最主流但依赖最重**：要新增 HTTPS 依赖（工作区现在一个 TLS 客户端都没有）。

### 第 3 轮进展（2026-10-16 · commit `be0beb6` · CI 全绿 `38885737929`）

| 项 | 状态 | 证据 |
| --- | --- | --- |
| **B 订阅 URL 拉取** | ✅ **代码完成**（2026-10-16 改变方案：不引依赖） | daemon 新增 `crates/xt-daemon/src/subs.rs`：`fetch_body` 用 **`/usr/bin/curl`**（绝对路径 + argv，永不经过 shell；`--location` 限制重定向协议、`--proto-redir =https,http`、`--compressed`、`--max-time 20`、正文上限 5 MiB）；`validate_url` 只认 `http(s)` 且有主机名；**日志/错误里只出现 `scheme://host/…`**（`redact_url`/`redact_text`），完整 URL 只落在状态文件 `<state-dir>/subscriptions.json`（**权限 0600**，原子 tmp+rename）。`lib.rs` 新增 `add_subscription` / `refresh_subscription` / `record_subscription_error`，并**宣告** `Capability::SubscriptionFetch`。 |
| 订阅的失败口径 | ✅ 不静默 | 添加失败 = 原样上报（`invalid_request`/`io`），**不存一条空订阅**；刷新失败 = 既写进该条的 `last_error`（下次打开界面还看得到），又原样上报（这一次点击立刻有反馈）。`refresh_subscription("local")` 明确报 `invalid_request`：本地文件订阅每次启动由 `--subscription-file` 重读，不归「刷新」管。 |
| UI：添加 / 刷新入口 | ✅ | 设置页订阅区在 `subscription_fetch` 宣告后渲染「添加订阅」输入框与每条订阅的「刷新」按钮（`Settings.tsx:317`/`:376`，仍按能力门控 —— 没宣告就不渲染）。 |
| CLI 等价入口 | ✅ | `xt-cli add-subscription <url>` / `refresh-subscription <id>` 从「固定 exit 2 说不支持」改成**真连 daemon 发请求**；并新增 `import-nodes <原文>`（传 `-` 表示从 stdin 读）。缺 `--socket` 仍是用法错误（exit 2），连不上是 io 错误（exit 1）。 |
| CI（唯一能编译 Rust 的地方） | ✅ **全绿** | 分支 `xraytun-next-v1` 推送 `be0beb6` → run `38885737929`：`workflow-lint` / `linux-core` / `macos-core` / `macos-app` 四个 job 全部 success（含 `cargo clippy -D warnings`、`cargo test --workspace` —— 三条新订阅 e2e 在 Linux 与 macOS 上真跑过）。 |
| 本机可验的证据 | ✅ | 新增 UI 验收 4 条 `apps/ui/tests/acceptance/subscription.test.tsx`（能力门控 / 完整 URL 逐字送到 daemon / 刷新送该条 id / 失败可见且输入不清空）；全量 `npx vitest run` = **92 passed · 1 skipped**，`npx tsc --noEmit` 0 错，`bash scripts/guard.sh` PASSED。Rust 的编译与 daemon e2e **只能由 CI 判定**（本机禁 cargo）。 |

**真人走查又发现的三处「看得见但不告诉你去哪」的毛病（已在同一轮修掉）**：

1. **有节点、但还没选节点**时，大启动按钮是灰的、只写「未选择节点」——真人就卡在这一屏。
   现在按钮变成可点的**「选择节点」**（点了跳节点页），说明逐字写清「在节点页点某一行的『选为当前节点』」。
2. **订阅添加失败的错误提示**原本渲染在设置页**顶部**，而订阅卡片在页面**最下面**，中间隔着好几张卡 —— 用户看不到自己在卡片里点出来的错误。
   现在错误渲染在**订阅卡片内**（同一 testid，仍只有一处）。
3. 节点页在 `subscription_fetch` 已宣告时，导入区明确写着「手里只有订阅链接？到『设置 → 订阅』填 URL」——
   两种「把节点弄进来」的路（粘贴原文 / 订阅链接）现在都在屏幕上说得出来。

**这一轮的口径（别自我美化）**：订阅这条路在**代码与本机 UI 验收**层面通了，但
（a）Rust 侧结论要等 CI 跑完才算数；（b）真机那一步（授权弹窗、`utun` 接管、装/卸 helper）**仍然只有你能验**。
所以「能翻墙了」这句话我仍然不说。**CI 全绿只证明「代码能编译、用例在真机上跑得通」，不证明「TUN 能接管流量」。**

## 5. 真人模拟体验官（独立复核，2026-10-01）

角色：fresh-context agent，按「刚下载完 App 的普通 Mac 用户」代入，只从 v1.2.0 源码（逐字引用）、
`../xraytun-next-real-machine/` 的真机截图/OCR、老版 v0.9.2 源码与发布工作流出发；未安装、未运行、未改文件。

- **结论与本节一致**：导入入口在界面上不存在、启动必然失败、helper 无人安装。
- **它多找出的三件事**：死路 5、死路 6，以及「只修导入仍然翻不了墙」的排序警告 —— 已并入 §2/§4。
- **它的关键对照**：老版 v0.9.2 有「订阅」一级导航 + 「添加并拉取」按钮 + 「安装 helper」入口；
  而**新版把模式砍到只剩 TUN**，于是 helper 从「TUN 的可选项」变成了**整个 App 的硬前提**——
  而这个硬前提所依赖的安装链路，仓库里一行都没有。
- **它明确标注的不确定项**：手里唯一真机截图是 **v1.1.3**（能力行没有 `tun_mode`），
  v1.2.0 首屏逐字是**从代码推导**的；Gatekeeper 真实弹窗、helper 在发行包里的实际策略分支、
  GUI 环境下 PATH 里有没有 `xray`，它都无法验证 —— 这些属于「只能真机确认」。
- **它的裁决**：如果只能先做一件事 → 先做**导入**（让屏幕上出现内容）；但真正的关键工程是 **helper 安装链路**，
  不能因为 CI 装不了就无限期推迟。

## 6. 待裁决
1. **先做哪条**：A（手动导入，零依赖） / B（订阅 URL 拉取，需选 TLS 依赖） / C（helper 安装，只有真机能验）？
2. ~~若做 B：选哪个 HTTP 客户端（`reqwest(rustls)` / `ureq(rustls)` / hyper+rustls）？~~
   **2026-10-16 已决**：都不选 —— 用系统自带的 `/usr/bin/curl`（绝对路径 + argv），零新增依赖，
   与老版 v0.9.2 对同一功能的选择一致（代价是不能复用连接，对「一天拉几次订阅」无所谓）。
3. 未公证（S5）与 helper 安装都要用户配合真机；是否接受「helper 安装只在本机验证、CI 不覆盖」？
