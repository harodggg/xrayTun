# 现场包接收端点（Cloudflare Worker + R2）

给「一键现场包」（用户在 App 里点一下，本机脱敏打包）提供接收端点：
**上传 → 公开 manifest → 取原始 zip（需 token）→ 删除（需 token）**。

* 目录：`infra/incident-collector/`。**故意不放在 `site/`** —— `main` 上的提交会触发
  Cloudflare Pages 部署，后端代码绝不能发布成静态站点。
* 代码：`src/worker.mjs`（**零依赖**，只用 Web 标准 API + R2 绑定）。
* 自测：`./infra/incident-collector/verify.sh`（16 个用例）+ `--sensitivity`（双向敏感性）。
* 隐私：见 [`PRIVACY.md`](./PRIVACY.md)（采集什么/不采集什么/留多久/怎么删）。

> ⚠️ **部署是特权动作**：本仓库**不含**任何凭据，也没有自动部署。由维护者用**临时授权**手动执行，
> 用户可随时吊销 token（见「回滚 / 撤销」）。

## 1. 接口

路由前缀 `BASE_PATH`（默认 `/api/incident`，部署在 `https://xraytun.top`）：

| 方法 | 路径 | 鉴权 | 说明 |
|---|---|---|---|
| `POST` | `/api/incident` | 无（公开） | 上传 zip（≤10 MiB，默认）。返回 `201 {id, sha256, received_at, bytes}` |
| `GET` | `/api/incident/<id>` | 无 | 返回 manifest（**公开安全**：不含日志正文、不含 IP） |
| `GET` | `/api/incident/<id>/blob` | `X-Auth-Token` | 原始 zip |
| `DELETE` | `/api/incident/<id>` | `X-Auth-Token` | 删除该 id 的 zip + manifest（用于「用户要求删除」） |

* `id` = `INC-YYYYMMDD-HHMMSS-<4hex>`（UTC 时间 + 2 字节随机；**不含任何用户标识**）；
* 上传的**公开性**用三道门兜住：**大小上限**（`MAX_BYTES`，默认 10 MiB）、
  **content-type 白名单 + zip 魔数**（不认自述，`PK\x03\x04` 之类）、
  **按 IP 限流**（`RATE_LIMIT_MAX`/窗口，默认 5 次/小时）；
* 其它状态码：`400` id 格式错、`401` 缺/错 token、`404` 不存在/已过期/路径不对、
  `405` 方法不对、`413` 太大、`415` 类型不对、**`422` 隐私拒收/扫描失败**、
  `429` 限流（带 `Retry-After`）、`500` 内部错。
* **`422`（最后防线）**：落盘前会扫包内文本，命中疑似密钥（UUID 字面量 / `pbk=` 等 URI 参数 /
  `-----BEGIN … PRIVATE KEY-----` / `vless://` 等节点 URL / JSON 形键值）就**拒收**，
  响应只给「类型 + 文件 + 行号」（`hits`，最多 20 条，`hits_truncated` 标截断），**绝不回显密钥原文**；
  **扫描器自身出错也拒收（fail closed）**，对应 `error: "scan_failed"`。

manifest 形状：

```json
{
  "schema_version": 1,
  "id": "INC-20260922-120000-3f2a",
  "sha256": "…64 hex…",
  "bytes": 12345,
  "content_type": "application/zip",
  "received_at": "2026-09-22T12:00:00.000Z",
  "retention_days": 30,
  "client_self_declared": "XrayTun/0.8.35"   // 仅当客户端显式发 X-XrayTun-Client 且格式合法
}
```

## 2. 本地自测（不需要 Cloudflare 账号）

```bash
./infra/incident-collector/verify.sh                # 16/16 通过
./infra/incident-collector/verify.sh --sensitivity  # 拿掉鉴权/大小上限 ⇒ 对应用例变红（脚本才认）
```

可选的本地 HTTP 端到端（需要 `npx wrangler`，**本地模式、不碰账号**）：

```bash
npx wrangler dev --config infra/incident-collector/wrangler.toml --local \
  --var INCIDENT_TOKEN:dev-token
# 另开一个终端：
curl -sS -X POST http://127.0.0.1:8787/api/incident \
  -H 'content-type: application/zip' -H 'X-Auth-Token: dev-token' \
  --data-binary @/tmp/small.zip
```

## 3. 部署（维护者执行；用户可随时吊销授权）

> ### ⛔ 只会创建/更新这两个名字，别的一律不动
>
> * Worker：**`xraytun-incident-collector`**
> * R2 桶：**`xraytun-incidents`**
>
> 账号里已有的其它 Worker（`eth-arb-scout` / `vless` / `hello-world-*` / …）与别的桶
> （例如 `mymutlicloud`）**都不属于本项目**：本目录里**没有任何 `wrangler delete` / prune 命令**，
> 部署命令只碰上面两个名字。
> **若你的输出里出现别的名字，说明命令写错了 —— 停下来，别继续。**
> 需要「全删」时只在用户明确要求下删 `xraytun-incidents` 与我们那个 Worker（见 §4）。

### 3.1 需要用户提供什么

| 项 | 说明 |
|---|---|
| **API Token** | **最小权限**：`Account > Workers Scripts > Edit`、`Account > R2 > Edit`、**`Zone > Workers Routes > Edit`**（路由要用）。若用户不愿给 Zone 权限，可改为**不给路由**：先只部署 Worker（用 `--no-bundle` 之外的默认流程不变，但不配 `routes`），再由用户自己在控制台加一次路由。 |
| **Account ID** | 非秘密。控制台右下角 / `wrangler whoami` 可看。 |
| **Zone** | `xraytun.top`（Pages 已在用；Worker 路由与它在同一个 zone）。 |

Token 的存放（**绝不写进仓库**）：

```bash
# 用户侧（0600，只放这一个 token）：
printf '%s' 'cf-xxxxxxxx' > ~/.cf-incident-token && chmod 600 ~/.cf-incident-token
# 维护者侧（从文件读，不进 shell history、不打印）；
# ⚠️ 变量名必须是 wrangler 认的那个：CLOUDFLARE_API_TOKEN
CLOUDFLARE_API_TOKEN="$(cat ~/.cf-incident-token)"
```

### 3.2 一次性准备

```bash
# R2 桶：**先 list 确认**（本项目已经建过 xraytun-incidents；重复 create 会报错）
npx wrangler r2 bucket list | grep -F xraytun-incidents || npx wrangler r2 bucket create xraytun-incidents

# 端点令牌（**secret**，不进 wrangler.toml）：
npx wrangler secret put INCIDENT_TOKEN --config infra/incident-collector/wrangler.toml
# ↑ 交互式粘贴一个随机字符串，例如：openssl rand -hex 24
```

**R2 lifecycle：保留 30 天后自动删除**（与代码里的惰性过期互为兜底）：

```bash
# wrangler 版本不同，子命令名可能是 r2 bucket lifecycle；亦可在控制台
# R2 → 桶 → Settings → Object lifecycle rules 里加一条：
#   prefix: ""  expire after: 30 days
npx wrangler r2 bucket lifecycle add xraytun-incidents --expire-days 30 --prefix "" || true
```

### 3.3 部署

```bash
# ⚠️ wrangler 认的是 CLOUDFLARE_API_TOKEN / CLOUDFLARE_ACCOUNT_ID（**不是 CF_API_TOKEN / CF_ACCOUNT_ID** —— 实测无效）
CLOUDFLARE_ACCOUNT_ID=<account-id> CLOUDFLARE_API_TOKEN="$(cat ~/.cf-incident-token)" \
  npx wrangler deploy --config infra/incident-collector/wrangler.toml
```

`wrangler.toml` 里的路由（**两条都要**，且必须写在**任何表头之前**）：

```toml
# ⚠️ 位置：必须在 `[[r2_buckets]]` / `[vars]` 等**任何表头之前**。
# 落在表里 ⇒ 被当成那张表的字段（wrangler 只报一句 `Unexpected fields …`）或 `vars.routes` 环境变量
# ⇒ **一条路由都不会注册**，而部署输出看起来是成功的。
routes = [
  { pattern = "xraytun.top/api/incident",   zone_name = "xraytun.top" },   # ← 上传入口本身（少了它 ⇒ 405）
  { pattern = "xraytun.top/api/incident/*", zone_name = "xraytun.top" }
]
```

2026-09-23 的两次真实事故都属于这一类（`env.routes` + 少注册裸路径那条 pattern），
两次的表现都是 **`POST https://xraytun.top/api/incident` → 405**（请求落到了 Pages），
**而部署日志是成功的** ⇒ 见下面的「部署后自检」。

**为什么是路径而不是新子域**：用户的机器在中国大陆，`*.workers.dev` 经常不可达；
`xraytun.top` 已实测可达。同 zone 上的 **Workers Route 优先于 Pages**，因此不需要新 DNS。
若部署时报「路由需要 Zone 权限 / hostname 需要 DNS 记录」，**停手报 lead**，
备选是 `incident.xraytun.top`（同 zone、走 CF 代理）。

### 3.3.1 部署后自检（**必做**：部署日志说成功 ≠ 端点能用）

```bash
# 不落盘探活（默认）：脏包 ⇒ 期望 422（Pages 不会给 422）
./infra/incident-collector/deploy-check.sh

# 带部署日志一起查（日志里不得出现 env.routes / Unexpected fields）
./infra/incident-collector/deploy-check.sh --deploy-log /tmp/wrangler-deploy.log

# 读回路由注册（需要 CF API token；**不要**把它写进仓库）
CLOUDFLARE_API_TOKEN="$(cat ~/.cf-incident-token)" ./infra/incident-collector/deploy-check.sh

# 完整环回（会建一条记录；带 INCIDENT_TOKEN 会自动删掉）
INCIDENT_TOKEN=<端点令牌> ./infra/incident-collector/deploy-check.sh --upload
```

三条判据（缺一不可）：
1. 部署日志里**没有** `env.routes`、**没有** `Unexpected fields`；
2. `GET /zones/<zone>/workers/routes` 能看到**两条** pattern（`…/api/incident` 与 `…/api/incident/*`）；
3. `POST {BASE}` **不是 405/404**（脏包 ⇒ 422 就证明请求进了 Worker）。

### 3.4 部署后自测（原样可复制）

```bash
BASE=https://xraytun.top/api/incident
TOKEN="$(cat ~/.cf-incident-token)"        # 这里用的是**端点**令牌，不是 CF 的 API Token
printf 'PK\003\004hello' > /tmp/small.zip  # 最小合法 zip 头（仅用于连通性验证）

# 1) 上传
RESP="$(curl -sS -X POST "$BASE" -H 'content-type: application/zip' \
        -H 'X-XrayTun-Client: XrayTun/e2e-test' --data-binary @/tmp/small.zip)"
echo "$RESP"
ID="$(printf '%s' "$RESP" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p')"
echo "ID=$ID"

# 2) manifest（公开）
curl -sS "$BASE/$ID"

# 3) blob：不带 token 必须 401；带 token 必须 200
curl -sS -o /dev/null -w 'no-token=%{http_code}\n' "$BASE/$ID/blob"
curl -sS -o /dev/null -w 'token=%{http_code}\n' -H "X-Auth-Token: $TOKEN" "$BASE/$ID/blob"

# 4) 限流（默认 5/小时；第 6 次应 429）—— 用**不同** IP 测不到，这里只演示形状
for i in 1 2 3 4 5 6; do
  curl -sS -o /dev/null -w "req$i=%{http_code}\n" -X POST "$BASE" \
    -H 'content-type: application/zip' --data-binary @/tmp/small.zip
done

# 5) 删除（带 token）→ 之后 manifest 必须 404
curl -sS -X DELETE -H "X-Auth-Token: $TOKEN" "$BASE/$ID"
curl -sS -o /dev/null -w 'after-delete=%{http_code}\n' "$BASE/$ID"
```

## 3.5 已知环境问题（本机实测）

1. **`~/.npm` 里有 root 拥有的文件 ⇒ `npx` 报 EPERM**：
   ```
   npm error Your cache folder contains root-owned files …
   npm error   sudo chown -R 501:20 "/Users/xbtg-/.npm"
   ```
   **不要照它去 `sudo chown`**（那是用户的机器）。绕开方式（`smoke.sh` 已内置）：
   ```bash
   npm_config_cache=/tmp/dsh-npm-cache npx --yes wrangler@3 --version
   ```
   `smoke.sh` 会**先验可写性**：环境里的 `npm_config_cache`（可能就是那个坏的）不可写时自动换到临时目录。
2. **wrangler 的日志目录也可能 EPERM**（`~/Library/Preferences/.wrangler/…`）：
   `WRANGLER_LOG_PATH=/tmp/dsh-wrangler-logs` 可绕开。
3. **wrangler 认的环境变量名**：`CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`
   （`CF_API_TOKEN` / `CF_ACCOUNT_ID` **不会被采用** —— 实测两套都设了才跑通）。

## 3.6 workerd 冒烟（本地，不需要账号）

```bash
./infra/incident-collector/verify.sh                 # 23 个用例（含隐私拒收）
./infra/incident-collector/verify.sh --sensitivity    # 3 个变体（鉴权/大小上限/隐私扫描）都必须变红
./infra/incident-collector/smoke.sh                  # 真 workerd 起一个请求（抓运行时限制）
./infra/incident-collector/smoke.sh --sensitivity     # 把「顶层取随机值」的 bug 回退 ⇒ 必须红
```

**为什么必须有 `smoke.sh`**：2026-09-22 首次部署被 CF 拒 ——
`Uncaught Error: Disallowed operation called within global scope … [code: 10021]`
（模块顶层调 `crypto.getRandomValues` 生成限流盐）。
**`node --test` 不执行 workerd 的这条限制**，所以 16 条本地用例全绿也抓不到它。
⇒ **本地绿 ≠ 运行时绿**：涉及 Workers 运行时的改动，必须跑一次 `smoke.sh`。

## 4. 回滚 / 撤销

```bash
# 1) 摘掉路由与 Worker（只删我们这一个；**R2 桶不会自动删**）
npx wrangler delete --config infra/incident-collector/wrangler.toml      # 交互确认，或加 --force
#    它删除的是 wrangler.toml 里 name = "xraytun-incident-collector" 那一个；
#    **别**用 `wrangler delete <别的名字>`（账号里其它 Worker 不属于本项目）。
# 2) 彻底清数据（仅当用户明确要求「全删」）
npx wrangler r2 bucket delete xraytun-incidents                          # 桶非空时需先清空
# 3) 吊销 CF API Token（用户在自己的控制台：My Profile → API Tokens → Delete）
```

**吊销 token 后什么会失效**：一切 `wrangler` 操作（部署/删除/改 secret/lifecycle）。
**已经部署的 Worker 与桶里已有的数据不受影响**，会继续按配置运行与到期删除 ——
所以「用户要彻底停止」= 先 `wrangler delete` + `r2 bucket delete`，再吊销 token。
端点自己的 `DELETE /api/incident/<id>` 用的是 `INCIDENT_TOKEN`（secret），
与 CF API Token 是两回事：吊销 CF token 不影响它。

## 5. 已知限制（诚实清单）

* **限流是 best-effort**：状态在每个 isolate 的内存里（键 = 加盐哈希的 IP，**不落盘**），
  不同 colo/isolate 不共享、重启即清零。要强一致得上 Durable Objects 或 KV —— 本卡不做。
  **实测也印证了**：部署后连续 POST 未触发限流（每 isolate 各自计数）。
  ⇒ **限流不能被当作安全边界**，真正的门是「公开端点 + 大小/类型白名单 + **服务端隐私拒收** + 30 天保留」。
* **`--local` 的 `wrangler dev` 用不了真 R2 的 region/一致性语义**，只适合验路由与状态码。
* 端点的**读/删**只有一把 token（没有多用户、没有审计日志、没有按 id 的授权）。
* 未做 CORS：上传由 App（非浏览器）发起；若将来要从网页上传，需要单独加 CORS 白名单。
* 未接告警：R2 用量/错误率要看 Cloudflare 控制台。
