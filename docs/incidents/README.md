# `docs/incidents/` —— 现场包与自动分诊（约定）

> 目标（用户的话）：「出错时**自动把现场送到能修它的人手里**，然后自动修、出下一个版本。」
> 本目录管的是**前两段**：**采集（脱敏、自锚定）** 与 **分诊（信号表）**。
> 上传端点与 App 侧按钮是别的卡（`task-114` / `task-115`）；**本流程不联网、不上传**。

## 1. 一次完整的流程

```
[用户点一下 / 命令行]  scripts/incident-bundle.sh
        → 本机脱敏，产出 zip（含 README.txt 让用户先看）
        → scripts/triage-incident.py  自动分诊 → incident.json + SUMMARY.md
        → 人（维护者）按 signature 去改 → 写一条会失败的测试 → 修 → 门禁 → 发版
        → 发版后用 scripts/net-metrics.py 的**同一口径**做「改前 vs 改后」对照
```

```bash
# 采集（只读；默认产出 ~/Desktop/xraytun-incident-<UTC>.zip）
./scripts/incident-bundle.sh
./scripts/incident-bundle.sh --out /tmp/i.zip --since 17:12:00 --keep-dir

# 分诊（只读；zip 或解开的目录都行）
python3 scripts/triage-incident.py --bundle /tmp/i.zip --json-out incident.json --md-out SUMMARY.md
python3 scripts/triage-incident.py --self-test     # 8 条 signature 的 fixture + 双向敏感性
```

两个脚本都有 `--self-test`：**没有自测的采集/分诊工具比没有更糟**（算错了会把人带偏）。

## 2. `incident-bundle.sh` 产出的包

| 文件 | 内容 | 关键口径 |
|---|---|---|
| `manifest.json` | **自锚定**：App 版本 / 核心版本 / helper 版本与**三态** / macOS 与架构 / mode / log_level / 采集起止 / **每个文件的 sha256** / 脱敏说明 / 截断事实 | App 版本**只**从 `XrayTun.app/Contents/Info.plist` 读 —— **核心横幅是核心版本，推不出 App 版本**（今天为此绕过一大圈） |
| `metrics.json` | `net-metrics.py --json --since <窗口起点> --until <now>` 的原始输出 | **自带口径头**（切/解/匹配/单位 + 选择内容指纹）。脚本缺失或失败时**不留空**：写一个 `error` 对象并说明原因 |
| `events.jsonl` | `source=app` 的行 + `level∈{error,warn}` 的行；去重、按 `ts_unix` 排序、有上限 | 让「自愈/重建/作废」这类事件**直接可读**，不用人肉 grep |
| `core-tail.txt` | 核心日志尾部（默认 2 MiB 上限） | **截断必须说出来**：文件**首行**写「已截断，尾部 k 行（共 N 行）」；stdout 同时再说一次 |
| `network.txt` | `route -n get default` / `route -n get 127.0.0.2` / `netstat -rn -f inet`（过滤）/ `ifconfig`（utun）/ `scutil --dns` | 只读；**不做任何网络修改** |
| `README.txt` | 包里有什么、脱敏做掉了什么、**没采集**什么、截断与上限 | 给用户**上传前**自己看 |

**窗口口径**：默认 = **本次 App 进程启动 → 现在**（`ps -o lstart=`）。
取不到就**退让**（最近一次核心启动 / 最近 30 分钟），并在 **stdout + manifest + README 三处**
显式标注 `degraded: true` —— **退让可以说，装作没退让不行**。

**脱敏（全在本机完成）**：

| 规则 | 结果 |
|---|---|
| 任何 URI | 只留 `scheme://host[:port]`（`vless://<uuid>@host:443?pbk=…` → `vless://host:443`） |
| UUID（含 32 位十六进制） | `<uuid>` |
| `password/passwd/pwd/token/secret/uuid/api_key/private_key/auth/psk` 的值 | `<redacted>` |
| `Authorization` / `Proxy-Authorization` | 整行余下部分 → `<redacted>` |
| **保留** | 域名与 IP（含节点 IP）、错误消息结构、路由与 DNS 结构（否则没法定位） |

脱敏**可测**：`incident-bundle.sh --self-test` 用一条含**真值**（编的，但形状是真的）的假日志，
断言产物里**不出现**原值，**同时**断言「该留的还留着」（host、节点 IP），并给出「把脱敏换成空操作 ⇒ 断言失效」的反向对照。

## 3. `triage-incident.py`：信号表（**字段 ↔ 判据**）

| signature | 判据（可测谓词） | 读哪个字段 | 历史实例 |
|---|---|---|---|
| `v6-rewrite` | v6 改写行数 > 0 **且**（v6-only 失败 + 混合失败）> 0 | `metrics.json` → `task97` | task-97 |
| `watchdog-false-positive` | 有「已作废」**且**其 ±60s 内仍有 `tunneling request`/`connection opened` | `events.jsonl` + `core-tail.txt` | task-95 |
| `probe-false-negative` | 探针「有 failed 行」或「无结局行」> 0 | `metrics.json` → `probes` | task-100 |
| `log-read-loss` | 「截断·残缺」或「非 JSON」行 > 0（**脚本侧**损失） | `metrics.json` → `stats` | task-107 的可见面 |
| `log-write-interleave` | 多对象行 > 0 **且** App 版本 ≥ `0.8.34`（修复前属历史数据） | `core-tail.txt` + `manifest.json` | task-104 |
| `helper-mismatch` | 版本三态 == `Mismatch`（`Unreadable` **不算命中**） | `manifest.json` → `versions.helper.check` | task-111 |
| `loopback-hole` | `route -n get 127.0.0.2` 的 interface ≠ `lo0`（或路由不存在） | `network.txt` | T3 |
| `tun-iface-einval` | `falied to set interface`（核心自己的拼写）**每分钟 ≥ 1** | `core-tail.txt` + 行内 `ts_unix` | task-93 |
| `unknown` | 以上都不命中 | —— | —— |

* **阈值都是常量**，写在 `triage-incident.py` 顶部并注明来源；它们出现在输出的**口径头**里。
* **`unknown` 不许猜**：输出「**最像的三条**」+ 各自的原始行，并明说「没命中 ≠ 没问题」。
* **「没数据」≠「数据说没有」**：缺 `metrics.json` 时那条 signature 记为 `unavailable`，不是 `not hit`。
* **双向敏感性**：每条 signature 一份**正 fixture**（必须命中）+ 一份**边界 fixture**（必须不命中）；
  再把谓词**改坏一个条件** ⇒ 边界 fixture **必须**变成命中（即原断言会红）。`--self-test` 会逐条打印。

## 4. 入库约定（**现场留可引用原件**）

```
docs/incidents/<INC-ID>/            INC-ID = INC-YYYYMMDD-HHMMSS-<4hex>（不含任何用户标识）
  manifest.json                     采集原件（自锚定；含本机绝对路径 —— 需要外发时先自己看一遍）
  incident.json                     triage 的机器读输出（含口径头）
  SUMMARY.md                        triage 的人读摘要（现象/证据/判定/边界）
  evidence/*                        脱敏后的**最小证据切片**（只贴判据用到的那几行）
  evidence/sha256.txt               原件的指纹（zip 与每个文件）—— 完整原件**不入库**
  README.md                         这一份是怎么产生的、原件在哪、哪些没入库
```

* ⚠️ **完整 bundle 不入库**（可能含隐私）。入库的是**脱敏后的最小切片 + 原件 sha256**，
  让后来人能核对「原件长什么样」，而不是把 100 MB 日志塞进仓库。
* `INC-ID` 里的 `<4hex>` 用 **manifest 的 sha256 前 4 位** ⇒ ID 与内容绑定（同一份包只会有一个 ID）。

## 5. 本流程**不能**证明什么（诚实清单）

1. **命中是症状，不是根因**：`v6-rewrite` 只说明「窗口里有 v6 改写、且这些连接里有失败」——
   它**不**证明「v6 改写导致了用户的问题」。
2. **只看包里有的东西**：真机 WKWebView 的交互、GFW 侧行为、App 内部的读侧统计，包里一概没有。
3. **`unknown` 不代表正常**：它只说明「这些谓词都没命中」。
4. **窗口可能已退让**（`degraded=true`）：那不是「App 启动至今」，引用时必须带上。
5. **脱敏是规则化的**：它挡住的是**已知形状**的凭据；一条没见过的凭据形状可能漏过去 ——
   所以 `README.txt` 明确请用户在**上传前**自己看一眼。
6. **不做因果、不做统计推断**：阈值是「该去看看」的提示，不是判决。
