# 审计密文接收端点（Cloudflare Worker + R2）

给「意图判定审计」的**每天自动上传**提供接收端点：设备把当天的审计数据在**本机**加密成密文信封，
POST 上来，我们**原样存进 R2**。服务端**只验形状、绝不解密**（它没有、也永远不会有设备密钥）。

* 契约：[`docs/design/AUDIT-SYNC.md`](../../docs/design/AUDIT-SYNC.md) §3（信封）/ §4（接口）——**实现按它写，不许各写各的**。
* 目录：`infra/audit-collector/`。**故意不放在 `site/`** —— `main` 上的提交会触发
  Cloudflare Pages 部署，后端代码绝不能发布成静态站点。
* 代码：`src/worker.mjs`（**零依赖**，只用 Web 标准 API + R2 绑定）。
* 自测：`./infra/audit-collector/verify.sh`（静态检查 + `node --test`，不需要 Cloudflare 账号）。
* 隐私：见 [`PRIVACY.md`](./PRIVACY.md)（存什么/能看到什么/留多久/怎么撤回）。

> ⚠️ **部署是特权动作**：本仓库**不含**任何凭据，也没有自动部署。由维护者用**临时授权**手动执行，
> 用户可随时吊销 token（见「回滚 / 撤销」）。

## 1. 接口

Base：`https://xraytun.top/api/audit`（Worker `xraytun-audit-collector`，R2 桶 `xraytun-audit`，
secret `AUDIT_TOKEN`）。

| 方法 | 路径 | 请求 | 成功响应 | 失败 |
|---|---|---|---|---|
| `POST` | `/api/audit` | 密文信封 JSON；头 `Authorization: Bearer <AUDIT_TOKEN>` | `200 {"ok":true,"key":"audit/<device>/<day>.json","replaced":<bool>}` | `400` 形状不对 / `401` token 不对 / `413` >10 MiB / `429` 限速 |
| `POST` | `/api/audit/list` | `{"device":"<16hex>"}` | `200 {"ok":true,"items":[{"day","rows","bytes","key","uploaded_unix"}]}` | `400`/`401`/`413`/`429` |
| `POST` | `/api/audit/revoke` | `{"device":"<16hex>"}` | `200 {"ok":true,"deleted":<n>}` | `400`/`401`/`413`/`429` |

* **全部 POST**（契约 §4）：设备侧 `transport.rs` 只提供 `post`，所以读/删也走 POST，不改传输层。
* **鉴权头只有 `Authorization: Bearer <token>` 一个**：传 `X-Auth-Token`（incident-collector 用的那个）
  一律 **401**，与「完全没带令牌」表现一致。别把 401 当成「令牌不对」，先看头名。
* **错误响应体**与 incident-collector 同格式：`{"error":"<code>","message":"…", …可选字段}`。
  错误信息**只说不合法的字段名，不回显字段值**（尤其 `ct`）。
* 其它状态码：`404` 路径不对（路由不存在；端点外的路径也是 404）、`405` 路由对但方法不是 POST、
  `500` 内部错。
* **响应绝不回显 `ct` / `nonce`**：上传成功只有 `key` 与 `replaced`。

服务端校验（**只验形状，绝不解密**，契约 §4）：

| 字段 | 判据 |
|---|---|
| `v` | 必须 `=== 1` |
| `alg` | 必须 `=== "chacha20poly1305"` |
| `device` | `^[a-f0-9]{16}$`（**必须过这个正则才允许拼 R2 key**，防前缀穿越） |
| `day` | `^\d{4}-\d{2}-\d{2}$` |
| `nonce` | `^[a-f0-9]{24}$`（12 字节） |
| `ct` | `^[a-f0-9]+$`（非空小写 hex；密文 + 16 字节 tag） |
| `rows` / `bytes` | 非负整数（明文头里的元数据，**不参与认证**，服务端不据此做任何安全判断） |

* **大小**：**先看 `Content-Length`，超 10 MiB 直接 `413`（不去读 body）**；读出来再核一次实际大小。
* **`device` 没过正则 ⇒ 400，且绝不拼 key**：`../x`、空串、超长、大写 hex、带斜杠都被拒。
  另外 `auditKey()` / `devicePrefix()` 里还有第二道 self-guard（对非法输入直接抛错）—— 双保险。

### 1.1 R2 布局

```
xraytun-audit/
  audit/<device>/<day>.json      # 收到的**原始密文字节**（不重新序列化）
```

* key 里只有 `<device>` 与 `<day>`，两者都是已过正则的白名单形状 ⇒ 不存在前缀穿越；
* **幂等**：同一天重传覆盖同一个 key，`replaced` 报告是否命中已有对象（首次 `false`，重传 `true`）；
* `list` / `revoke` 都**分页列到底**（R2 一次最多回 1000 条）。若 binding 返回 `truncated` 却不给
  cursor、或 cursor 不前进，端点**抛错返回 500**，而不是返回一个「短了但不说」的结果。

## 2. 本地自测（不需要 Cloudflare 账号）

```bash
./infra/audit-collector/verify.sh                # 静态检查 + node --test 全绿
./infra/audit-collector/verify.sh --sensitivity  # 拿掉「鉴权」「device 正则」⇒ 对应用例必须变红（脚本才认）
./infra/audit-collector/deploy-check.sh          # 部署前静态自检（不部署、不联网）
./infra/audit-collector/smoke.sh                 # 真 workerd（wrangler dev --local）起一个请求
./infra/audit-collector/smoke.sh --sensitivity   # 把「顶层取随机值」回退 ⇒ 真运行时必须起不来
node --test                                      # 只跑测试（在 infra/audit-collector/ 下）
```

### 2.1 为什么还要 `smoke.sh`（**本地绿 ≠ 运行时绿**）

`node --test` 不执行 workerd 的运行限制，所以本地全绿也可能一部署就起不来。本目录有**两个**真实教训：

1. **顶层取随机值**：incident-collector 2026-09-22 首次部署被拒 ——
   `Disallowed operation called within global scope … [code: 10021]`（模块顶层调 `crypto.getRandomValues`
   生成限流盐）。本端点照抄了同样的**惰性盐**写法，`smoke.sh --sensitivity` 就是把这个 bug 回退回去，
   证明这条冒烟确实抓得住它。
2. **导出一个字符串常量**：本端点第一版写了 `export const ALG = 'chacha20poly1305'`。
   `node --test` 38 条全绿，但 workerd 起不来：
   `Incorrect type for map entry 'ALG': the provided value is not of type 'function or ExportedHandler'`
   —— 入口模块的每个命名导出都会被当成候选 handler。现在 `ALG` **不导出**，`smoke.sh` 已复现验证。
   （导出的正则 `DEVICE_RE` 等没问题：incident-collector 的 `ID_RE` 已在线验证。）

⇒ 涉及 Workers 运行时的改动，**必须**跑一次 `smoke.sh` 再提交。

可选的本地 HTTP 端到端（需要 `npx wrangler`，**本地模式、不碰账号**）：

```bash
npx wrangler dev --config infra/audit-collector/wrangler.toml --local \
  --var AUDIT_TOKEN:dev-token
# 另开一个终端（device 用 16 位小写 hex）：
curl -sS -X POST http://127.0.0.1:8787/api/audit \
  -H 'content-type: application/json' -H 'Authorization: Bearer dev-token' \
  -d '{"v":1,"alg":"chacha20poly1305","device":"3f2a91c4d0be7715","day":"2026-09-24","rows":1,"bytes":10,"nonce":"00112233445566778899aabb","ct":"deadbeef"}'
```

## 3. 部署（维护者执行；用户可随时吊销授权）

> **部署记录（2026-09-28）**：`xraytun-audit-collector` 已上线 ——
> version id `90526093-42e3-40c8-b35e-5c9eeddddf59`；两条路由经 CF API **读回确认**；
> 桶 `xraytun-audit` 已建，lifecycle（400 天）经 API 读回确认；
> `AUDIT_TOKEN` 已设为**用户自己的** token。
> 上线后做过一次真实客户端 → 真实 Worker 的端到端（密文落盘、R2 取回检查、撤回清理），
> 证据在 `docs/verification/AUDIT-SYNC-VERIFY.md`。
>
> ⚠️ **空 `User-Agent` 会被 CF 的 Browser Integrity Check 在到达 Worker 前挡掉
> （403 + `error code: 1010`）** —— curl 冒烟时请带上 `-A`。

> ### ⛔ 只会创建/更新这两个名字，别的一律不动
>
> * Worker：**`xraytun-audit-collector`**
> * R2 桶：**`xraytun-audit`**
>
> 账号里已有的其它 Worker 与别的桶**都不属于本项目**：本目录里**没有任何 `wrangler delete xraytun-…`
> / prune 命令**（「回滚」小节里的删除命令只能带 `--config infra/audit-collector/wrangler.toml`，
> 删的就是我们这一个 Worker）。
> **若你的输出里出现别的名字，说明命令写错了 —— 停下来，别继续。**

### 3.1 需要用户提供什么

| 项 | 说明 |
|---|---|
| **API Token** | **最小权限**：`Account > Workers Scripts > Edit`、`Account > R2 > Edit`、**`Zone > Workers Routes > Edit`**（路由要用）。若用户不愿给 Zone 权限，可改为**不配 `routes`**，再由用户自己在控制台加一次路由。 |
| **Account ID** | 非秘密。控制台右下角 / `wrangler whoami` 可看。 |
| **Zone** | `xraytun.top`（Pages 已在用；Worker 路由与它在同一个 zone）。 |

Token 的存放（**绝不写进仓库**）：

```bash
# 用户侧（0600，只放这一个 token）：
printf '%s' 'cf-xxxxxxxx' > ~/.cf-audit-token && chmod 600 ~/.cf-audit-token
# 维护者侧（从文件读，不进 shell history、不打印）；
# ⚠️ 变量名必须是 wrangler 认的那个：CLOUDFLARE_API_TOKEN
CLOUDFLARE_API_TOKEN="$(cat ~/.cf-audit-token)"
```

### 3.2 一次性准备

```bash
# R2 桶：**先 list 确认**（重复 create 会报错）
npx wrangler r2 bucket list | grep -F xraytun-audit || npx wrangler r2 bucket create xraytun-audit

# 端点令牌（**secret**，不进 wrangler.toml）：
npx wrangler secret put AUDIT_TOKEN --config infra/audit-collector/wrangler.toml
# ↑ 交互式粘贴一个随机字符串，例如：openssl rand -hex 24
#   设备侧：App 里「设置 → 系统与助手 → 审计同步」把 token 填进输入框，
#   它会以 0600 存到数据目录的 audit-sync.token（契约 §5.1：**不是 Keychain** ——
#   本仓库还没有 Keychain 实现，这一版与 intent-audit.jsonl 同级同权限存放）
#   命令行则用 --token-file 指向一个 0600 的文件。
```

**R2 lifecycle：保留 400 天后自动删除**（契约 §4；与 worker 侧的惰性过期互为兜底）。

⚠️ **`r2 bucket lifecycle add <bucket> [name] [prefix]` 里 name 与 prefix 是位置参数，
没有 `--prefix` 开关** —— incident-collector 当年就是写错这条命令、又被 `|| true` 抹平，
结果桶上**从来没有**删除规则、而文档却写着「自动删除」（2026-09-23 的真实事故）。
**不要加 `|| true`，也不要「先跳过」。**

```bash
# 正确形态（name 与 prefix 是位置参数；prefix 留空 = 整桶）
npx wrangler r2 bucket lifecycle add xraytun-audit expire-400-days "" --expire-days 400 --force

# ⚠️ **加完必须读回复核**（部署日志说成功 ≠ 规则真的在）
npx wrangler r2 bucket lifecycle list xraytun-audit      # 应看到 expire-400-days
# 或直接读 API：
#   GET /accounts/<acct>/r2/buckets/xraytun-audit/lifecycle
#   期望 rules 里有一条 deleteObjectsTransition.condition = {type: Age, maxAge: 34560000}，且 enabled=true
./infra/audit-collector/deploy-check.sh                  # 静态自检（含 lifecycle 命令是否写在 README 里的检查）
```

> 为什么建议 **400 天**：审计要能看「跨年趋势」；400 天 ≈ 13 个月，覆盖同比。这是**建议值**，
> 改了 `RETENTION_DAYS` 就要同步改 lifecycle，并重新读回复核（两处必须一致）。

### 3.3 部署

```bash
# ⚠️ wrangler 认的是 CLOUDFLARE_API_TOKEN / CLOUDFLARE_ACCOUNT_ID（**不是 CF_API_TOKEN / CF_ACCOUNT_ID**）
CLOUDFLARE_ACCOUNT_ID=<account-id> CLOUDFLARE_API_TOKEN="$(cat ~/.cf-audit-token)" \
  npx wrangler deploy --config infra/audit-collector/wrangler.toml
```

`wrangler.toml` 里的路由（**两条都要**，且必须写在**任何表头之前**）：

```toml
# ⚠️ 位置：必须在 `[[r2_buckets]]` / `[vars]` 等**任何表头之前**。
# 落在表里 ⇒ 被当成那张表的字段（wrangler 只报一句 `Unexpected fields …`）或 `vars.routes` 环境变量
# ⇒ **一条路由都不会注册**，而部署输出看起来是成功的。
routes = [
  { pattern = "xraytun.top/api/audit",   zone_name = "xraytun.top" },   # ← 上传入口本身（少了它 ⇒ 405）
  { pattern = "xraytun.top/api/audit/*", zone_name = "xraytun.top" }
]
```

incident-collector 在 2026-09-23 的两次真实事故都属于这一类（`env.routes` + 少注册裸路径那条 pattern），
两次的表现都是 **`POST https://xraytun.top/api/audit` → 405**（请求落到了 Pages），
**而部署日志是成功的** ⇒ 部署后必须实测（下面 §3.4）。

**为什么是路径而不是新子域**：用户的机器在中国大陆，`*.workers.dev` 经常不可达；
`xraytun.top` 已实测可达。同 zone 上的 **Workers Route 优先于 Pages**，因此不需要新 DNS。

### 3.4 部署后自测（**必做**：部署日志说成功 ≠ 端点能用）

```bash
./infra/audit-collector/deploy-check.sh    # 本机静态自检（不碰 Cloudflare、不部署）
```

**本机没有 CF 凭据 ⇒ 端到端没有跑过。** 部署后请至少手工复核这三条：

```bash
BASE=https://xraytun.top/api/audit
TOKEN=<AUDIT_TOKEN 的值>          # 端点令牌，不是 CF API Token
ENV='{"v":1,"alg":"chacha20poly1305","device":"3f2a91c4d0be7715","day":"2026-09-24","rows":1,"bytes":10,"nonce":"00112233445566778899aabb","ct":"deadbeef"}'

# 1) 路由注册（读回 CF API，需要 CLOUDFLARE_API_TOKEN）：
#    curl -sS -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
#      https://api.cloudflare.com/client/v4/zones/<zone>/workers/routes
#    应看到 xraytun.top/api/audit 与 xraytun.top/api/audit/* 两条

# 2) 不带 token ⇒ 401（Pages 不会给这个 JSON；405/404 就是路由没命中）
curl -sS -o /dev/null -w 'no-token=%{http_code}\n' -X POST "$BASE" -d "$ENV"

# 3) 上传 → list → revoke（完整环回）
curl -sS -X POST "$BASE" -H "Authorization: Bearer $TOKEN" -d "$ENV"          # 200 ok:true key:…
curl -sS -X POST "$BASE/list" -H "Authorization: Bearer $TOKEN" \
  -d '{"device":"3f2a91c4d0be7715"}'                                          # 200 items:[…]
curl -sS -X POST "$BASE/revoke" -H "Authorization: Bearer $TOKEN" \
  -d '{"device":"3f2a91c4d0be7715"}'                                          # 200 deleted:1
```

## 4. 回滚 / 撤销

```bash
# 1) 摘掉路由与 Worker（只删我们这一个；**R2 桶不会自动删**）
npx wrangler delete --config infra/audit-collector/wrangler.toml      # 交互确认，或加 --force
#    它删除的是 wrangler.toml 里 name = "xraytun-audit-collector" 那一个；
#    **别**用 `wrangler delete <别的名字>`（账号里其它 Worker 不属于本项目）。
# 2) 彻底清数据（仅当用户明确要求「全删」）
npx wrangler r2 bucket delete xraytun-audit                          # 桶非空时需先清空
# 3) 吊销 CF API Token（用户在自己的控制台：My Profile → API Tokens → Delete）
```

**吊销 CF token 后什么会失效**：一切 `wrangler` 操作（部署/删除/改 secret/lifecycle）。
**已经部署的 Worker 与桶里已有的密文不受影响**，会继续按配置运行与到期删除。
端点自己的 `revoke` 用的是 `AUDIT_TOKEN`（secret），与 CF API Token 是两回事。
**密钥丢了 ⇒ 已上传的密文永久读不出**（只能撤回删除）；本机的 `intent-audit.jsonl` 始终是第一副本。

## 5. 已知限制（诚实清单）

* **限速是 best-effort，而且明确「非授权判据」**：状态在每个 isolate 的内存里
  （键 = `SHA-256(salt + IP)` 的前 16 hex，**不落盘**；salt 每 isolate 随机、重启即失效），
  不同 colo/isolate 不共享、重启即清零。要强一致得上 Durable Objects 或 KV —— 本卡不做。
  ⇒ **限流只是减轻滥用，不是安全边界**；真正的门是 **token 鉴权** + 形状白名单 +
  大小上限 + 400 天保留 + `revoke`。
* **服务端不能验证密文本身**：`ct` 只检查「是不是非空小写 hex」。一个持有 token 的人可以传
  随机 hex 覆盖某天的对象（污染）。这是 E2E 的直接后果（服务端没有密钥），契约 §7 已知；
  缓解手段是 token 只发给自己的设备 + `list` 可核对 `rows/bytes`。
* **`rows` / `bytes` 不可信**：它们在明文头里，不参与 AAD 认证，服务端也不据此做决定
  （契约 §3：`bytes` 只是人读的估算）。真正的完整性由设备侧解密时的 AEAD 校验保证。
* **惰性过期只在 `list` 上生效**：`uploaded` 拿不到时（理论上不该发生）不判过期；
  **主要机制始终是 R2 lifecycle**，worker 侧只是兜底。`revoke` 与年龄无关，一律全删。
* **未接告警**：R2 用量/错误率要看 Cloudflare 控制台。
* **`smoke.sh` 需要 `npx`**（会拉 `wrangler@3`，只跑 `--local`，不碰账号）。本机 `~/.npm` 里有
  root 拥有的文件会让 npx 报 EPERM —— **不要 `sudo chown`**（那是用户的机器）；脚本已内置：
  验 `npm_config_cache` 可写性、不可写就换 `/tmp`，并设 `WRANGLER_LOG_PATH`。
* **本机未部署**：Worker 从未真正上过线，端到端「真的写进 R2」没有跑过（见
  `docs/design/AUDIT-SYNC.md` §9 D 行：测试 ✅ / 部署 ❌）。
