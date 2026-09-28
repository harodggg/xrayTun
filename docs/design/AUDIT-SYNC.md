# 审计自动同步：契约与设计（0.9.2）

> 需求原话：「你都做吧，能每天定时上传吗，而不是用我自己上传」
> ⇒ 要做两件事：(1) 离线审计（读 `intent-audit.jsonl` 出报告）；(2) 把审计**每天自动**送出去，
> 不需要用户手动导出/上传。

本文是**实现契约**。Rust / Worker / UI 三边都按本文写，不许各写各的。

---

## 0. 一页结论

| 决定 | 内容 | 为什么 |
| --- | --- | --- |
| 端到端加密 | 上传的是**密文**：设备生成密钥，ChaCha20-Poly1305，服务器**永远拿不到明文** | 域名 = 浏览记录。服务端可读就意味着我们成为数据处理者。R2 里只放密文，威胁模型立刻简单一个数量级 |
| 传输 | 全部走 `POST`，同 zone 路由 `xraytun.top/api/audit`（**不是** `*.workers.dev`） | 复用既有 `incident-collector` 的结论：mainland 经常打不开 `*.workers.dev`（见 `infra/incident-collector/wrangler.toml` 的注释）|
| 独立 Worker | 新建 `infra/audit-collector/`，**不动** `incident-collector` | 两者的数据类别、保留期、接口（本服务要 revoke/list）都不同；不让现场包管道承担被改坏的风险 |
| 调度 | **进程内定时**（App 运行时每 30 分钟检查一次）+ **按天补齐** | 不装第二个常驻组件。审计按天分桶且上传幂等 ⇒ 关机期间的天数在下次启动时自动补齐 |
| 不装 launchd | 明确**不做** LaunchAgent | 见 §6 |
| 默认 | **默认关闭**；关闭时**零网络请求**；偏好文件坏掉按**关闭**处理（fail-closed） | 开启即代表数据离开设备，必须是显式同意，且可撤回。一个读不懂的配置绝不能被解读成"用户同意上传" |
| 幂等 | R2 key = `audit/<device>/<day>.json`，重复上传**覆盖同一 key** | 重试永远不可能产生重复数据 |
| 只传完整天 | 只传 `day < 今天(UTC)` | 否则同一天会被反复上传、且内容是半截的 |

---

## 1. 数据流

```
Mac（设备）                                          Cloudflare
┌───────────────────────────────┐                   ┌──────────────────────────┐
│ intent-audit.jsonl(.1)        │                   │ Worker: /api/audit       │
│   ↓ 按 UTC 天分桶              │                   │  · 校验信封形状（不解密）│
│ 明文 bundle（v1，含 host）     │  密文 envelope    │  · 限速 + 大小上限       │
│   ↓ ring AEAD 加密（设备密钥） │ ────────────────▶ │   ↓                      │
│ envelope（只有密文+元数据）    │  POST /api/audit  │ R2: xraytun-audit        │
└───────────────────────────────┘                   │  audit/<device>/<day>.json│
        ▲                                           └──────────────────────────┘
        │ 离线分析：把密文拉回来，用同一把密钥解密
        └── 报告：新域/天、Token、缓存命中率、L0 归纳候选、阈值校准候选
```

**关键**：分析在**用户自己的机器**上做（拿到密文 + 密钥才能解）。服务端不提供图表，
也不具备解密能力 —— 这不是缺陷，是这条管线的前提（见 §7 取舍）。

---

## 2. 明文 bundle（v1）

`intent-audit.jsonl` 里 `ts_unix` 落在同一天（UTC）的行 → 一个 bundle。

```json
{
  "v": 1,
  "kind": "xraytun.intent.audit.day",
  "day": "2026-09-24",
  "device": "3f2a91c4d0be7715",
  "app": { "name": "XrayTun", "version": "0.9.2" },
  "counts": { "rows": 123, "block": 41, "allow": 77, "deferred": 5,
              "cache_hit": 88, "applied": 39 },
  "rows": [ /* AuditRecord，按 ts_unix 升序 */ ]
}
```

规则（**每条都要有单测**）：

- `rows` 就是 `xt_intent::audit::AuditRecord` 的序列化，**字段名与语义一个字都不改**
  （历史文件要能一直读）。
- **`context_sent` 一律剥掉**，无论设备上是否开了「记录外发内容」。
  那是用户的页面内容，不因为开了同步就自动外发。⇒ 这是硬规则，必须有测试。
- `day` 由 `ts_unix` 转 **UTC** 日；用的日期算法与出处写在实现里（Howard Hinnant
  `civil_from_days`），并有已知日期测试（1970-01-01 / 2024-02-29 / 2026-01-01）。
- 空天（0 行）**不产生 bundle**（不上传空文件）。
- 同一 `day` 重复构建必须是**逐字节相同**的（确定性；否则"重传覆盖"就没有意义）。⇒ 测试。

## 3. 密文信封（v1，线上唯一的格式）

```json
{
  "v": 1,
  "alg": "chacha20poly1305",
  "device": "3f2a91c4d0be7715",
  "day": "2026-09-24",
  "rows": 123,
  "bytes": 45678,
  "nonce": "<24 hex = 12 字节>",
  "ct": "<hex，密文+16 字节 tag>"
}
```

- 算法：`ring::aead::CHACHA20_POLY1305`（`ring` 已在依赖树里，**不引入新依赖**）。
- nonce：每次加密由 `ring::rand::SystemRandom` 生成 12 字节随机数（同一密钥下不重复）。
- **AAD（附加认证数据）**=`xraytun-audit-v1|<device>|<day>|<rows>`。
  ⇒ 服务器/中间人改 `day`、`device`、`rows` 任何一个，解密**必然失败**。⇒ 必须有防篡改测试。
- **hex 而不是 base64**：不引新依赖、实现不可能错、利于人眼看。代价是体积 ×2 —— 一天几百行
  ≈ 100 KB 明文 → 200 KB 信封，远在 10 MiB 上限内。**这个取舍是故意的。**
- 明文长度的估算字段 `bytes` 只用于人读，不参与认证（服务端不可信，不据此做任何决定）。

## 4. 线上接口（全部 `POST`）

为什么全 POST：`crates/xt-intent/src/transport.rs` 只提供 `post`（刻意的：一个把域名发出去的功能
不该顺手长出通用 HTTP 客户端）。所以 list/revoke 也做成 POST 路径，**不改传输层**。

Base：`https://xraytun.top/api/audit`（Worker `xraytun-audit-collector`，R2 桶 `xraytun-audit`，
secret `AUDIT_TOKEN`）。

| 路径 | 请求 | 成功响应 | 失败 |
| --- | --- | --- | --- |
| `POST /api/audit` | body = 信封 JSON；头 `Authorization: Bearer <token>` | `200 {"ok":true,"key":"audit/<device>/<day>.json","replaced":<bool>}` | `400` 形状不对 / `401` token 不对 / `413` >10 MiB / `429` 限速 |
| `POST /api/audit/list` | `{"device":"<16hex>"}` | `200 {"ok":true,"items":[{"day","rows","bytes","key","uploaded_unix"}]}` | `400`/`401`/`429` |
| `POST /api/audit/revoke` | `{"device":"<16hex>"}` | `200 {"ok":true,"deleted":<n>}` | `400`/`401`/`429` |

实现口径（与 `infra/audit-collector/src/worker.mjs` 一致）：路径对但方法不是 `POST` ⇒ **405**；
路径不对 ⇒ **404**；`AUDIT_TOKEN` 没配 ⇒ **一切操作 401（fail closed）**，不是"放行"。
`device` 会被**两道**正则挡住：入口校验 + `auditKey()` 拼 key 前的 self-guard（防前缀穿越）。
R2 `list` 分页到底，`truncated=true` 却没给 cursor、或 cursor 不前进 ⇒ **抛错 500**，
绝不把不完整的结果当成完整。

⚠️ **两个线上事实（部署后实测，不是猜测）**

1. **请求必须带 `User-Agent`。** 空 UA 会被 Cloudflare 的 Browser Integrity Check 在**到达 Worker
   之前**挡掉：`403` + body 是 `error code: 1010`（不是我们的 401）。客户端固定发
   `xraytun-audit-sync/<version>`，而且 `transport::build_request` 在调用方没给 UA 时也会补一个
   默认 UA —— 但"哪天有人把那行删了"的后果是一个与 Worker 毫无关系的 403，所以写进契约。
2. **`wrangler secret put` 有几十秒传播延迟。** 轮换当天实测：改完立刻请求仍被**旧** token 接受，
   约 45 秒后才切换。⇒「轮换后立刻测出 401 没生效」不能当成轮换失败，等一分钟再断言。

服务端校验（**只验形状，绝不解密**）：

- `v == 1`、`alg == "chacha20poly1305"`；
- `device` 匹配 `^[a-f0-9]{16}$`、`day` 匹配 `^\d{4}-\d{2}-\d{2}$`、
  `nonce` 匹配 `^[a-f0-9]{24}$`、`ct` 匹配 `^[a-f0-9]+$`；
- **`device` 必须过正则才允许拼进 R2 key**（防前缀穿越：`../`、空串、全通配都不行）；
- 请求体上限 10 MiB，**先看 `Content-Length` 再读**；
- 响应**绝不回显 `ct`**；
- `[observability] enabled = false`：不采集请求日志；不存 IP。
- 限速：沿用 `infra/incident-collector` 的加盐哈希做法（具体算法以那边实现为准，别自己发明）；
  若那边是 isolate 内存态，就照抄并在 README 里写明"尽力而为、非授权判据"。

R2 侧：key `audit/<device>/<day>.json`；**retention 用 R2 lifecycle 规则**（建议保留 400 天，
让"跨年趋势"有意义），lifecycle 的 gcloud 式 CLI 命令写进 `README.md`，别只写在提交信息里。

## 5. 设备侧：密钥、状态、调度

### 5.1 密钥与 token 放哪 —— **落盘为 0600 文件**（与原稿不同，这里是修正）

原稿写的是 Keychain。**动手时发现本仓库还没有 Keychain 实现**：
`apps/desktop/src/intent.rs` 的模块头写明 Jev 的 API Key 也"还没落地"，
`store.rs` 只有"引用形式"的约定。与其为了让文档好看而假装有 Keychain，不如先把值放数据目录：

| 文件（都在数据目录，0600） | 内容 |
| --- | --- |
| `intent-audit-sync.json` | 进度：设备 id + 已上传到哪一天（**与 CLI 共用同一份**）|
| `audit-sync.json` | 偏好：`enabled` / `base_url` |
| `audit-sync.key` | 加密密钥（32 字节 hex，首次开启时生成）|
| `audit-sync.token` | 上传 token |

**为什么这个取舍可以接受**：审计文件 `intent-audit.jsonl` 本身就是**明文域名**，与密钥文件
同目录同权限（0600）—— 同一个用户身份本来就读得到两者，所以密钥落盘**没有扩大暴露面**。
**代价写清楚**：备份/迁移要连这几个文件一起带走；密钥丢了，已上传的密文再也解不开
（本机审计文件仍是第一副本）。Keychain 是后续可以单独做的一步，不阻塞这条链路。

共用进度文件是**故意**的：App 与 CLI 谁先跑，都不会把对方已经传过的天重传一遍
（R2 key 由 device+day 决定，重复也只是覆盖）。

⚠️ **诚实的代价**：密钥丢了 ⇒ 已上传的密文永久读不出（只能撤回删除）。本机的
`intent-audit.jsonl` 始终是第一副本，所以不会因此丢历史。

### 5.2 状态文件

`intent-audit-sync.json`（与 `settings.json` 同级）：

```json
{ "v": 1, "device": "3f2a91c4d0be7715", "last_uploaded_day": "2026-09-23",
  "last_attempt_unix": 0, "last_ok_unix": 0, "last_error": null,
  "consecutive_failures": 0, "skipped_days": [] }
```

`device` 是首次开启时随机生成的 8 字节 hex（**不是**硬件指纹、不含账号信息）。
它出现在密文信封的**明文头**里 —— 因为它必须能被服务端用来分组、列出与撤回。

### 5.3 调度语义（纯函数，必须可单测）

1. `pending_days` = {审计文件里出现过的天} 中满足 `last_uploaded_day < day < 今天(UTC)` 的天，
   升序，**最多 31 天**；超出的最老天记进 `skipped_days`（明说跳过，而不是无限重试）。
2. 首次检查在启动后 **60 秒 ±30 秒**（抖动，避免和启动抢资源）。
3. 之后每 **30 分钟**检查一次；无待传天数时**什么都不做**（不写状态、不发请求）。
4. 失败退避：`30min × 2^(consecutive_failures-1)`，上限 6 小时。
5. 逐天**顺序**上传（不并发）：一天一个请求，失败就停，下轮从这里继续。
6. 上传成功：`last_uploaded_day = day`，`last_ok_unix = now`，失败计数清零。
7. **幂等**：因为 key 含 `day`，同一天重传只会覆盖。

### 5.4 关闭时必须是零请求

`enabled == false` ⇒ 调度器**不构造也不发送任何请求**（测试用假 Transport 断言 `calls == 0`）。
关闭也不删已上传的数据；要删用 `revoke`。

## 6. 明确不做的（以及为什么）

| 不做 | 为什么 |
| --- | --- |
| launchd LaunchAgent | 多一个**常驻安装物**：plist 安装/卸载、TCC、升级时的迁移与残留，全都得管。而收益只是"精确在某个时刻上传"。审计按天幂等 ⇒ **启动时补齐**已经完全覆盖需求 |
| 服务端明文存储 | 等于把每个用户的浏览记录收进我们手里。不做，见 §7 |
| 服务端聚合/仪表盘 | E2E 的直接后果。要图表就在本机跑 `intent_audit report` |
| 上传 `context_sent` | 用户的页面内容，与"技术审计"无关 |
| 上传 settings / 订阅 / 节点 / 定位缓存 | 其它管线的事，别混进来 |

## 7. 取舍与诚实边界

- **加密带来的确定损失**：服务端不能出图、不能跨设备聚合（除非你在某台机器上拿密钥解密）。
  换来的是"我们不必声明自己是数据处理者"。**如果以后你真想要服务端图表，那是一次显式的
  重新决定**，不是在这里偷偷放宽。
- **服务端仍能看到的元数据**：设备随机 id、日期、行数、密文长度、上传时刻、来源 IP（CF 层面，
  我们**不存**）。这些**足以做流量模式分析**，文案必须承认，不能说"完全匿名"。
- **端到端加密的一个直接后果（必须承认）**：服务端**无法校验密文内容** ——
  它只能验形状。所以**拿到 token 的人可以覆盖某一天的对象**（污染/删除那一天的数据）。
  这是 E2E 的代价，不是实现缺陷：要能校验内容就得能读内容。
  缓解：token 是用户自己的 secret；R2 对象按天分键（污染一天不会波及别的天）；
  `list` 会返回每天的行数与大小 ⇒ **不一致看得出来**。
- **`list` 会顺手做惰性过期**：Worker 在列出时按对象 `uploaded` 年龄剔除 >`RETENTION_DAYS`（400 天）
  的对象（拿不到 `uploaded` 就不判）。主机制仍是 R2 lifecycle 规则；这一层只是"lifecycle 忘了配"时的兜底。
- **本文件写下的都是设计**。落地状态与验证证据见 §9、以及 `docs/verification/`（不含未验证的断言）。

## 8. 用户可见的三件事（UI 必须有，不许有假按钮）

1. **开关**（默认关）+ 一句"开启后每台设备每天上传一次，内容是密文"。
2. **「将要上传的内容」预览**：选一天，显示行数与**明文 JSON**（本机数据，不涉及网络）。
3. **状态区**：设备 id（可复制）、最后成功时间、待传天数、最后错误、
   「立即同步一次」「撤回全部已上传」（revoke 前必须二次确认）。

状态文案的规矩（沿用仓库既有要求）：**没有证据就不许说成功**。
`last_ok_unix` 为空时显示"从未成功上传过"，而不是"正常"。

## 9. 落地与验证

| 步骤 | 产出 | 怎么算验过 | 本环境能不能验 |
| --- | --- | --- | --- |
| A 契约 | 本文 | 评审 | ✅ |
| B Rust 内核 | `crates/xt-intent/src/audit_report.rs`、`audit_sync.rs` | 单测 + Cloud Run `cargo test -p xt-intent` | ✅（远端跑） |
| C CLI | `crates/xt-intent/examples/intent_audit.rs` | 同上（example 一起编译） | ✅ |
| D Worker | `infra/audit-collector/` | `node --test`（本地可跑）+ **部署需用户 CF 凭据** | 部分：测试 ✅ / 部署 ❌ |
| E 桌面接线 | 定时任务 + 5 条命令 | 编译 + 单测（Cloud Run / CI macos-14） | 部分：编译 ✅ / 真机跑 ❌ |
| F UI | 设置页开关 + Intent 页状态区 | `npm test` + `npm run build` | ✅ |
| G 发布 | release notes | `scripts/check.sh` | ✅ |

**明确无法在这里验证的**（不许写成已验证）：
- Worker 未部署（本机没有 CF 凭据）⇒ 端到端"真的上传到 R2"没跑过；
- 没有 macOS 机器 ⇒ 定时器在真实 App 生命周期里的行为（休眠、退出、升级）没跑过；
- 因此 §4 的线上契约与 §5.3 的调度语义，今天的证据只到"单测 + 编译"。
