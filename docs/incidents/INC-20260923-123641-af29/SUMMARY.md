# 现场分诊：`multiple`（INC-20260923-123641-af29）

## 口径头（引用任何命中都要带上这一段）

* 工具：`triage-incident/1（2026-09-22）`（我实际跑的命令与原始输出见 `evidence/triage-raw.md`）
* 包：`INC-20260923-123641-af29`；`manifest.json` sha256 = `a89fe7e9353feba3ecb4296850b315492c65a656be5a64c67b886786ae6cd6a3`（3186 B）
* zip：`505fb4e8e3da5a856e169e5398ec5c78e561dbe22a88c9a925874c23191fc266`，**128,770 B**（我自己下载复算 = 服务端声明）
* 窗口：**2026-09-23 20:27:52 → 20:36:29**（来源：**退让口径**：最近一次核心启动；`degraded=true`）
* 版本：App **0.8.36** / 核心 **26.9.9** / helper 三态 **Mismatch**（⚠️ 这条**口径有误**，见 §4）
* 系统：macOS 26.6.2 / arm64 / kernel 25.6.0；mode = `tun`，log_level = `debug`
* **命中 4 条**：`probe-false-negative`、`helper-mismatch`、`loopback-hole`、`tun-iface-einval`

## 1. 现象（包里能看到什么）

用户这份包**没有**用户自己写的症状描述（bundle 只有采集内容）⇒ 我只能按信号与证据说话：
窗口内（约 9 分钟）核心侧持续报 **`proxy/tun: [tun] falied to set interface > invalid argument`**（**437 行**），
`app/dns` 有 **6 条** DoH 请求失败，`events.jsonl` 有 **10 条** `proxy/tun: connection was refused`，
App 自己的探针出现「**境内 全灭 1/1、境外 0/2 死**」，且 `route -n get 127.0.0.2` 落到 **utun6**（回环空洞）。
窗口被标记为 **degraded**：它不是「App 启动至今」，只是「最近一次核心启动之后」。

## 2. 证据（每条都能自己看；切片在 `evidence/`）

| # | 现象 | 证据（原件位置 / 我说到的行） | 我自己的独立复核 |
|---|---|---|---|
| E1 | **回环空洞仍在** | `network.txt` 第 15–20 行：`route -n get 127.0.0.2` → `route to: 127.0.0.2` / `destination: default` / `mask: 128.0.0.0` / **`interface: utun6`**（判据要求 `lo0`） | `grep -n -A4 '127.0.0.2' x/network.txt` 原样命中；切片 `evidence/loopback-route.txt` |
| E2 | **tun 接口设置失败（EINVAL）刷屏** | `core-tail.txt` 中 `falied to set interface` **437 行**（首 20:35:39.368605、末 20:36:31.608720，跨度 52 s ⇒ **504.2 次/分**，阈值 1.0/分） | 我自己 `grep -c` = 437；`events.jsonl` 里为 **0** ⇒ 这 437 行只出现在核心尾部日志 |
| E3 | **探针假阴性（自愈没触发）** | `events.jsonl:22`（app/warn）：`本轮只有 1/3 个探针目标失败（整侧不通：境内 全灭1/1、境外 0/2 死 —— http://223.5.5.5/ → 000）⇒ 未到 2 个的门槛，只记账、不重建` | 该行原样；切片 `evidence/events-probe-false-negative.txt` |
| E4 | **DoH 失败** | `events.jsonl` **6 行** `app/dns: failed to retrieve response … Post "https://94.140.14.14/dns-query"`（另见 `9.9.9.9`） | `grep -c 'dns-query'` = 6（core-tail 里 0）；切片取前 2 行 |
| E5 | **proxy/tun 连接被拒** | `events.jsonl` **10 行** `proxy/tun: connection was refused`（另有 core-tail 1 行） | `grep -c`（events=10 / core-tail=1）；切片取前 2 行 |
| E6 | **helper 三态被判 Mismatch** | `manifest.json` `versions.helper.check = {state: Mismatch, installed: 0.8.35, bundled: 0.8.36}`，且 source 写着「与产品同一口径」 | ⚠️ **我独立复核后认定这条判定是错的**（见 §4） |
| E7 | 未命中的三条（**不代表正常**） | `v6-rewrite`：`metrics.task97.v6_rewrite_lines = 0`；`log-write-interleave`：`multi_object_lines = 0`（App 0.8.36 ≥ 修复版 0.8.34）；`log-read-loss`：`truncated_lines = 0 / non_json_lines = 0`（**App 内部读侧统计不在包里**，这里只是脚本侧） | 我读了 `incident.json` 的 `signatures` 逐条 `evidence` 数字 |
| E8 | 分诊之外、值得记的一条：**IPv4 连接失败率高** | `metrics.task97.classes.v4_only = {connections: 169, failed: 79, pct: 46.7}`；`failed_open_lines: 112` | 从 `metrics.json` 原样读出（`evidence/metrics-keycounts.json`） |

## 3. 判定（triage 的机器结论）

```
signature = multiple
hit: probe-false-negative (near_miss 0.9) / helper-mismatch (1.0) / loopback-hole (1.0) / tun-iface-einval (0.5)
not hit: v6-rewrite / watchdog-false-positive / log-read-loss / log-write-interleave
```

## 4. ⚠️ 一条**口径缺陷**（我独立复核后确认）—— **已修：`task-171`**

（修复与敏感性证据：`docs/verification/HELPER-TRISTATE-CALIBER.md`；本包是暴露它的现场。）

**修后回归**：对同一份包重跑分诊 ⇒ 命中从 **4 条降到 3 条**，`helper-mismatch` 不再命中，
输出另存为 `incident.after-caliber-fix.json`（原 `incident.json` **保持原样** = 「当时工具这么说」的记录）。

* 现场包说 **Mismatch**，判据是 **包版本相等**（installed 0.8.35 ≠ bundled 0.8.36），而 source 字段还写着
  「与产品同一口径」。
* **产品**的判据是 **协议号相等**（`apps/desktop/src/commands/helper.rs:139-147`：`installed.protocol == bundled.protocol`；
  `state.rs:578-584`：「协议号相等 ⇒ 界面不该提示」）。
* 我**只读执行**了两个二进制的 `version`：
  ```
  /Library/PrivilegedHelperTools/com.xraytun.helper version        → xraytun-helper 0.8.35 (protocol 1)
  /Applications/XrayTun.app/Contents/MacOS/xraytun-helper version   → xraytun-helper 0.8.36 (protocol 1)
  ```
  ⇒ **协议号相同 ⇒ 按产品口径应为 `Match`**。
* 后果：这份 manifest 与「设置页不会提示重装」的产品行为**自相矛盾**，而且 `helper-mismatch` 这个 signature
  被**误触发**（分诊噪音），发布说明也容易被它带偏。⇒ 已报 Lead（`task-171`/卡面写作 `task-172`）。

## 5. 结论与不确定项

**能说的**：
1. 窗口内（20:27:52→20:36:29，degraded）用户机器处于**降级状态**：核心持续吐 tun 接口 EINVAL（504/分）、
   App 探针出现「整侧不通」但未达自愈阈值、DoH 与 proxy/tun 连接均有失败、回环地址被路由到 utun6。
   ⚠️ **分寸**：E2 的 `falied to set interface` 是**已知高频现象**；团队此前已用证据**排除它作为断网主因**
   （同一批证据里 **261 次该错误之后紧跟 `connection opened`**）。**该排除结论我是引用，未在本卡复核。**
   另：`task-106`（探针集合两份真源、国内只有 1 个目标）由 backend-dev 接手，本包是**现场证据**。
2. **回环空洞（F-1）仍在**：`127.0.0.2` 走 utun6 而不是 `lo0` —— 与 `docs/incidents/OPEN-FINDINGS.md` 的 F-1 是同一现象，
   本包是它在真实用户机器上的**又一次现场证据**。
3. **`helper-mismatch` 命中是口径造成的**，不是真的不兼容（§4）。

**我不能定的（诚实清单）**：
* **用户到底遇到什么**：包里没有用户自述；我**没有**复现他的网络环境，也**没有**在他的机器上做任何操作（只读）。
* **DoH 失败是「本机→目标」还是「目标侧」**：包里只有客户端错误文本，没有服务端视角 ⇒ 定不了。
* **`falied to set interface` 的根因**：只知道它在刷屏（437 行 / 52 s），根因需要真机复现 + 系统日志/接口状态；
  本包**不能**证明它导致了用户的症状（命中是症状不是根因）。
* **`core-tail.txt` 被截断**：窗口内共 40,659 行 / 8,837,432 B，**只保留尾部 9,629 行 / 2,097,388 B** ⇒
  这里的 437 次 EINVAL 与「504/分」**只代表被保留的那一段**，不能当全天口径。
* **窗口 degraded**：20:27:52 不是 App 启动时刻 ⇒ 不能拿它说「本次启动至今都这样」。
* 探针口径差异：App 自己说「1/3 个目标失败」，而 `metrics.probes` 只列了 **2 个目标**（`cp.cloudflare.com:80`、`www.baidu.com:80`）
  ⇒ 两处口径不一致，我**没有**断定哪边更准（属 `net-metrics.py` 的口径问题，另记）。
* `events.jsonl` 只有 25 行（core/error 15、app/warn 9、core/warn 1）——**它不是全量事件**，是按规则筛过、去重、有上限的。
