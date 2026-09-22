# 现场分诊：multiple

## 口径头（引用任何命中都要带上这一段）

* 工具：`triage-incident/1（2026-09-22）`
* 包：`inc-real2.zip`；`manifest.json` sha256 = `23f5221fa4162fe5348ce4257185b4b30e3166164de89cfe4fbb6d82af640ed8`
* 窗口：2026-09-22 18:46:04 → 2026-09-22 19:08:50（来源：**退让口径**：最近一次核心启动（ps 取不到 App 启动时刻 ⇒ 窗口不是 App 启动至今）；degraded=True）
* 版本：App 0.8.34 / 核心 26.9.9 / helper 三态 Match
* 阈值：{"tun_einval_per_min": 1.0, "watchdog_window_secs": 60, "write_interleave_fixed_version": "0.8.34"}
* 包内文件（bytes/sha256）：
  * `manifest.json` 3156 B `23f5221fa4162fe5…`
  * `metrics.json` 2808 B `234a4af6fc43f09c…`
  * `events.jsonl` 424130 B `a57bac65ad5ec6e2…`
  * `core-tail.txt` 2097298 B `f17bc7096742c89d…`
  * `network.txt` 3952 B `149c462718cb030b…`
  * `README.txt` 2087 B `00dc2802b1a34481…`

## 判定

**命中 2 条**：`loopback-hole`, `tun-iface-einval`

## 逐条判据

| signature | 结果 | 关键数字 | 判据 |
|---|---|---|---|
| `v6-rewrite` | 未命中 | {"replace_v6_lines": 0, "v6_only_failed": 0, "mixed_failed": 0} | v6 改写行数 > 0 且（v6-only 失败 + 混合失败）> 0 |
| `watchdog-false-positive` | 未命中 | {"已作废": 0} | 有「已作废」且其 ±60s 内仍有转发证据 |
| `probe-false-negative` | 未命中 | {"探针连接": 131, "成功": 131, "失败·有 failed 行": 0, "失败·无结局行": 0, "轮数": 130} | 探针「有 failed 行」或「无结局行」> 0 |
| `log-read-loss` | 未命中 | {"截断·残缺行": 0, "非 JSON 行": 0, "空行": 0} | 读统计里「截断·残缺」或「非 JSON」> 0（脚本侧损失；App 内部读侧统计不在包里） |
| `log-write-interleave` | 未命中 | {"多对象行": 0, "App 版本": "0.8.34", "修复版本": "0.8.34"} | 多对象行 > 0 **且** App 版本 ≥ 0.8.34（修复前出现属历史数据，不算回归） |
| `helper-mismatch` | 未命中 | {"版本检查三态": "Match", "installed": null, "bundled": null} | manifest 的 helper 版本三态 == Mismatch（读不到时是 Unreadable，**不算命中**） |
| `loopback-hole` | **命中** | {"interface": "utun6", "shape": "interface=utun6"} | `route -n get 127.0.0.2` 的 interface ≠ lo0（或路由不存在） |
| `tun-iface-einval` | **命中** | {"次数": 5, "跨度分钟": 1.8, "每分钟": 2.778, "每分钟阈值": 1.0} | `falied to set interface` 每分钟计数 ≥ 阈值（阈值来源见脚本常量注释） |

## 原始行（能自己看，不用信我）

* `loopback-hole`：route -n get 127.0.0.2      （判据：interface 必须是 lo0；不是 ⇒ loopback 空洞）
* `loopback-hole`：   route to: 127.0.0.2
* `loopback-hole`：destination: default
* `loopback-hole`：       mask: 128.0.0.0
* `loopback-hole`：  interface: utun6
* `tun-iface-einval`：{"ts_unix":1790075093,"source":"core","level":"info","message":"2026/09/22 19:04:53.900477 [Info] proxy/tun: [tun] falied to set interface > invalid argument"}
* `tun-iface-einval`：{"ts_unix":1790075094,"source":"app","level":"warn","message":"核心日志已限流：最近有 13 条未实时显示（原文已完整写入日志文件；刷新日志页可看到最近 2000 条）。示例格式：<ts> <ts> [Info] proxy/tun: [tun] falie
* `tun-iface-einval`：{"ts_unix":1790075095,"source":"core","level":"info","message":"2026/09/22 19:04:55.912395 [Info] proxy/tun: [tun] falied to set interface > invalid argument"}

## 边界（本流程不能证明什么）

* 命中是**症状**不是根因（例：`v6-rewrite` 只说明「有 v6 改写且有失败」）。
* 包里没有的东西看不到：真机 WKWebView、GFW 侧行为、App 内部读侧统计。
* `unknown` 只表示「这些谓词都没命中」，**不表示没有问题**。
