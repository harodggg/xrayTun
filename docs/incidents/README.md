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

## 4. 上传前的隐私闸（`--privacy-check`）——**fail closed**

脱敏在**客户端**做（`incident-bundle.sh`），而上传端点会把包**原样**存起来
⇒ **漏一次，密钥就在云上躺很久**。task-113 的真实教训就是脱敏自己漏了三处
（JSON 形键值 `"password":"x"`、URI 的 `?query`（`pbk=SECRET` 就在里面）、`Authorization` 只吃掉 `Bearer`）。
所以上传前再过一道**可测**的闸：

```bash
python3 scripts/triage-incident.py --privacy-check /tmp/xraytun-incident-XXXX.zip
python3 scripts/triage-incident.py --privacy-check <包> --privacy-json   # 给流水线/服务端用
```

* **命中 ⇒ 非 0 退出**（`1`；路径不存在 `2`）—— **fail closed**，不是「提示一下」；
* 输出 `文件:行号:类型` + **值只显示前 4 字符与长度**：**报告本身不许成为泄漏源**；
* 扫**包内所有文本文件**；zip 与解开的目录都支持；只读、不联网。

命中的类型（每条都是可测谓词，逐条敏感性见 `--self-test`）：

| 类型 | 抓什么 |
|---|---|
| `uuid` / `uuid-32hex` | 带连字符的 UUID、32 位十六进制（**排除**我们自己的 `<uuid>` 占位） |
| `uri-secret-param` | `?pbk=`/`&sid=`/`token=`/`password=`/`secret=`/`key=`/`spx=`… 的值 |
| `credential-field-json` | JSON 形键值，**含转义形**（`\"password\":\"x\"` —— 核心日志的 message 里就是这种） |
| `subscription-url` | 订阅路径（`/subscribe`、`/api/vN/client/subscribe`、`/link/…`） |
| `proxy-url-with-credentials` | `vless://`/`vmess://`/`ss://`/`trojan://` 且带 userinfo |
| `vmess-base64` | `vmess://<base64>`（**内容不可读** —— 见 §6 诚实清单） |
| `private-key-block` | `-----BEGIN … PRIVATE KEY-----` |
| `bearer-token` / `authorization-header` | `Bearer <token>`、`Authorization:` 的值（`<redacted>` 除外） |
| `email` | 邮箱形（**精确**白名单 `example.com/org/net`、`localhost`、`xraytun.top`；**子域照报**） |

**与 `task-114` 的服务端拒收互不替代**：这一层是上传前自检，那一层是最后防线。

**真包实测（2026-09-22 19:08 与 2026-09-23 10:43 各一份）：两份都 `✓ 未发现疑似密钥模式`（exit 0）**
—— 即当时的客户端脱敏在这些规则下站得住。**这不等于「一定没有」**（见 §6）。

## 5. 入库约定（**现场留可引用原件**）

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
* `INC-ID` 里的 `<4hex>` 由服务端**随机**生成（`infra/incident-collector/src/worker.mjs:64-73` 的
  `crypto.getRandomValues`）——**不含用户标识，也不绑定内容**：
  * 它**不是** manifest 的 sha256 前缀；
  * **同一份包上传两次会得到两个不同的 ID** ⇒ 操作时（去重/认领）**不要**把它当内容指纹。
  ⚠️ **留档：下面这句原文是假的（2026-09-23 更正，文档跟代码走）**——
  「`<4hex>` 用 manifest 的 sha256 前 4 位 ⇒ ID 与内容绑定（同一份包只会有一个 ID）」。
  实测反例：`INC-20260923-123641-af29` 的后缀 `af29` ≠ 其 manifest sha256 前 4 位 `a89f`
  （上一份 `…-23f5` **恰好**相符，所以过去没暴露）。
  若将来要做「内容绑定」，那是**端点侧**改动（需重新部署 `infra/**`）——本仓库文档只记录事实，不改端点。

### 已入库的现场

| INC-ID | 怎么来的 | 窗口 | 命中 signature | 备注 |
|---|---|---|---|---|
| `INC-20260922-190335-23f5` | 本机手动跑 `incident-bundle.sh`（流程首条真实记录） | 2026-09-22 18:46:04 → 19:08:50（degraded） | 见该目录 `SUMMARY.md` | App 0.8.34 / helper **Match** |
| `INC-20260923-123641-af29` | **App「报告问题」真实上传（第一份）** | 2026-09-23 20:27:52 → 20:36:29（degraded） | `probe-false-negative`、`helper-mismatch`、`loopback-hole`、`tun-iface-einval` | App 0.8.36；其中 `helper-mismatch` 是**口径误判**（协议号相同应为 Match，见该目录 §4） |

## 6. 本流程**不能**证明什么（诚实清单）

1. **命中是症状，不是根因**：`v6-rewrite` 只说明「窗口里有 v6 改写、且这些连接里有失败」——
   它**不**证明「v6 改写导致了用户的问题」。
2. **只看包里有的东西**：真机 WKWebView 的交互、GFW 侧行为、App 内部的读侧统计，包里一概没有。
3. **`unknown` 不代表正常**：它只说明「这些谓词都没命中」。
4. **窗口可能已退让**（`degraded=true`）：那不是「App 启动至今」，引用时必须带上。
5. **脱敏是规则化的**：它挡住的是**已知形状**的凭据；一条没见过的凭据形状可能漏过去 ——
   所以 `README.txt` 明确请用户在**上传前**自己看一眼。
6. **不做因果、不做统计推断**：阈值是「该去看看」的提示，不是判决。

### 6.1 隐私闸**测不到**什么（正则会误报/漏报，至少这四个具体形态）

1. **base64 里裹着的密钥**：`vmess://<base64>` 只报「**这里有一个不可读的载荷**」——
   我们**不**解码、也不假装知道里面有没有 UUID/口令。同理：任何被 base64 编码后的
   `password=…`、私钥、订阅 URL，本条规则一概**读不出来**。
2. **二进制/压缩容器里的密钥**：只扫**文本**。zip 里若嵌了二进制附件、或日志中出现压缩过的字节流，
   扫描不做解压、不做熵分析 ⇒ **漏报**。
3. **自造形状的令牌**：例如某个上游用「大写字母 + 数字 + 下划线、不带任何关键字」的裸密钥，
   本闸**没有**「高熵长串」这一类判据（故意不加：它会把 commit hash、Xray 横幅里的构建号、
   各种 ID 全部报出来，闸门一有噪音就会被绕过）⇒ **漏报**。
4. **误报方向同样是代价**：白名单只有 5 个域，所以文档里写 `foo@sub.example.com` 会**被报出**；
   `uuid-32hex` 会命中任何 32 位十六进制串（例如某些设备 ID / 构建哈希）。
   这是**故意**的 fail-closed 取舍：宁可让人多看一眼，也不放过。**但**过度误报会让人把闸当噪音 ——
   所以每次**扩大**规则都要跑 `--self-test` 的干净 fixture。

⇒ 结论：**它是上传前的自检，不是安全保证**。服务端那一层（`task-114` 的拒收）是最后防线，
两层**互不替代**；而真正的保证只有一条：**不要把凭据写进日志**。
