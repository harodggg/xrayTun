# 上线端点的独立隐私 / 权限审计（`task-135`，**非破坏性**）

> 审计对象：`https://xraytun.top/api/incident`。对照文件：`infra/incident-collector/PRIVACY.md`。
> 作者：tester（**独立于实现者**）。**每一节都按「我量到的 / 读码得到的 / 我推断的」三分类标注。**
>
> ⚠️ **已单独先报 Lead 的一条不符项**：`PRIVACY.md` §3 的**第一道**保留期保证（R2 lifecycle `expire after 30 days`）
> **没有配置** —— 见 §4。其余审计项全部通过。

## 0. 方法、边界与非破坏性承诺

| 项 | 值 |
|---|---|
| 令牌 | **从未打印、从未写进任何文件**。本文一律写 `<token>`。只读的 CF API 调用用它做 `Authorization: Bearer`，命令里不 `set -x`、不打印响应头 |
| 正确令牌路径 | **没有走**（会碰真实对象；按卡面留给 `task-123` 的端到端验收） |
| 破坏性操作 | **没有 DELETE 任何对象、没有写 R2、没有改 `infra/**`** |
| **POST 用量** | **2 次**（卡面上限 2）：① dirty 包（**计入**限流）② `content-type` 不在白名单（按代码在**限流之前**返回，不计数）。**没有把限流打满** |
| 其余探测 | 全是 `GET` / `OPTIONS` / `PUT` / `DELETE`（`DELETE` 全部用**错令牌**，返回 401，不产生副作用）。这些路径**不经过** `checkRateLimit`（限流只在 `handleUpload` 里） |
| 时间 | 2026-09-23（`cf-ray: a3f687…-ORD`，见 §5 原始响应头） |

---

## 1. 鉴权边界 —— **我量到的**：无令牌与错令牌 **全部 401**，没有一个 200

```bash
$ curl -s -o /dev/null -w "%{http_code}\n" https://xraytun.top/api/incident/INC-19700101-000000-dead/blob
401          # 无 token
$ curl -s -o /dev/null -w "%{http_code}\n" -H 'X-Auth-Token: WRONG-TOKEN-FOR-AUDIT' …/blob
401          # 错 token（**不是正确的那个**）
$ curl -s -o /dev/null -w "%{http_code}\n" -X DELETE …/INC-19700101-000000-dead
401          # 无 token
$ curl -s -o /dev/null -w "%{http_code}\n" -X DELETE -H 'X-Auth-Token: WRONG-TOKEN-FOR-AUDIT' …/INC-…
401          # 错 token
```
响应体一律 `{"error":"unauthorized","message":"需要 X-Auth-Token"}`，**不区分**「令牌错」与「没令牌」（不给穷举者额外信息）。
**读码得到的**顺序与之一致：`handleBlob` / `handleDelete` **先** `authorized()` 再查 manifest ⇒ 即使 id 不存在也先 401（不会用它当「存在性探测」）。
**我推断的**：令牌比较用的是模块里的 `constantTimeEqual`（源码 `:51`，`authorized` 在 `:112`）——**我没有做时序测量**，所以「常量时间」我只标**读码**，不声称实测。

## 2. 信息不泄露

### 2.1 上传被拒时**不回显密钥** —— **我量到的** ✅
```bash
$ curl -X POST --data-binary @dirty.zip -H 'Content-Type: application/zip' https://xraytun.top/api/incident
HTTP=422
{"error":"secret_detected",
 "message":"包内疑似含密钥/订阅信息，已拒收（命中只给类型与位置，不回显内容）。请在本机脱敏后重传。",
 "hits":[{"type":"node_url","file":"leak.txt","line":1},
         {"type":"uri_secret_param","file":"leak.txt","line":1}],
 "hits_truncated":false,"scanned_files":2,"scanned_bytes":130}
```
* 包里放的是 `vless://11111111-2222-…@node.example.com:443?pbk=SECRETPBK`；
* `grep -c 'SECRETPBK\|11111111-2222-3333-4444-555555555555\|pbk='` 对响应体 ⇒ **0**；
* 逐字段检查 `hits[]` 的键：**只有 `file` / `line` / `type`**，没有 `value` / `secret` / `match` / `raw` / `text` 之类。
⇒ 报告本身**不是**泄漏源（我自己的 `--privacy-check` 与这里的口径一致）。

### 2.2 被拒的上传**不留对象** —— **我量到的** ✅
```bash
$ GET /accounts/<acct>/r2/buckets/xraytun-incidents/objects      # 只读
{"success":true, …}   →  对象数 = 0
```
⇒ 422 之后桶里**仍然 0 个对象**（与「失败不落盘」的实现一致：`put()` 只在扫描通过之后才发生）。

### 2.3 未知 id 的措辞 —— **我量到的**
```json
GET /api/incident/INC-19700101-000000-dead  → 404 {"error":"not_found","message":"没有这个 id"}
```
**读码得到的**另一种 404：`handleManifest` 命中 `expired()` 时返回 `{"error":"expired","message":"已过保留期（30 天）"}`，
并**顺手删掉** zip 与 manifest。
**我推断的（观察项，低危）**：因此**知道确切 id** 的人可以区分「从未存在」与「存在但已过期」——这是一个**存在性残余信号**。
**边界要说清**：id = `INC-<UTC 时间戳>-<2 字节随机>`，穷举需要撞中「精确到秒的时间 + 4 位 hex」，
且它**不返回任何内容**；而 `PRIVACY.md` §7 已明说 manifest 是公开的（知道 id 就能读元数据）。
⇒ 我把它列为**观察项（B 级）**，**不**算隐私不符；若要抹平，把「过期」也回 `not_found` 即可。

## 3. 限流语义 —— **读码得到的**（外加一次真实 422 与一次真实 415）

源码位置：`infra/incident-collector/src/worker.mjs`
```js
DEFAULTS = { RETENTION_DAYS: 30, RATE_LIMIT_MAX: 5, RATE_LIMIT_WINDOW_SECONDS: 3600, … }
:157   const key = await rateKey(clientIp(request));        // 维度 = IP（CF-Connecting-IP，回退到其它头）
:141   rateKey(ip) = SHA-256(`${salt}:${ip}`).slice(0,16)    // salt 每 isolate 随机（:130-138）
:34    const rateBuckets = new Map();                        // **纯内存**，重启即消失
handleUpload 的顺序：① 声明大小(413) → ② content-type 白名单(415) → ③ **checkRateLimit(429)** → ④ 读体/魔数 → ⑤ 扫描(422)
```
* **维度与阈值**：按 IP，**5 次 / 3600 秒**；超限 ⇒ **429**，响应带 `retry_after_seconds` 与 `retry-after` 头（`:214`）。
* ⚠️ **`422` 计入限流**（限流在第 ③ 步、扫描在第 ⑤ 步）⇒ **用户最多连续重传 5 次**（含因密钥被拒的那些），
  第 6 次起 429、要等 `retry_after` 秒。**真实上限 = 5 次/小时/IP**（不论那 5 次是成功、415(魔数不符) 还是 422）。
* 另一侧：**声明超限(413)** 与 **content-type 不在白名单(415)** 在限流**之前**返回 ⇒ **不计入**额度
  —— 我实测的那次 415 就属于这一类（`Content-Type: text/plain` → 415，未消耗额度）。
  ⚠️ 这是**读码 + 由代码顺序推出的**；要**实测**计数变化需要第 3 次 POST，**我不烧**那个额度（留给 `task-130` 的真实上传）。
* **不持久化 IP**：`rateBuckets` 是模块级 `Map`；全文件只有两处 `put()`（manifest、blob），键里都不含 IP（`:279`、`:282`）；
  唯一的 `console.*` 是 `console.error('incident-endpoint error:', e.message)`（`:372`，不含 IP、不含令牌）。

## 4. 保留期 —— ⚠️ **不符：`PRIVACY.md` 说的「两道保证」今天只有一道**

> **一句话**：**这不是「数据对外泄露」，是「承诺与实现不符」** —— 桶在用户自己的 CF 账号下、访问受令牌控制；
> 但 `PRIVACY.md` §3 说「30 天，两道保证」，实际只剩第二道，用户会据此得出**错误结论**。

**读码得到的**（一致的部分）：
```
wrangler.toml:  RETENTION_DAYS = "30"     [observability] enabled = false
worker.mjs:     retention_days 写进 manifest；expired() 用 RETENTION_DAYS 判；读到超期 ⇒ 删 zip+manifest 并 404
PRIVACY.md §3:  「30 天，两道保证：1. R2 lifecycle 规则（expire after 30 days）；2. 端点内的惰性过期」
```

**我量到的**（**不符**）：
```bash
$ GET /accounts/<acct>/r2/buckets/xraytun-incidents/lifecycle      # 只读
{"success":true,
 "result":{"rules":[{"id":"Default Multipart Abort Rule","enabled":true,"conditions":{},
                     "abortMultipartUploadsTransition":{"condition":{"type":"Age","maxAge":604800}}}]}}
$ GET /accounts/<acct>/r2/buckets/xraytun-incidents
{"result":{"name":"xraytun-incidents","creation_date":"2026-09-22T11:11:00.488Z","location":"APAC",…}}   # 无 lifecycle 字段
```
⇒ 桶上**只有**「未完成分片上传 7 天后中止」这条默认规则，**没有任何按 Age 删除对象的规则**。
**第一条保证不存在**；只剩**惰性过期**，而它**只在有人读那个 id 时**才触发删除。

**严重性（我不夸大）**：这不是「数据对外泄露」（桶在用户自己的 CF 账号下、访问受令牌控制），
而是**承诺与实现不符**：`PRIVACY.md` 说「30 天后自动删除」，实际是「**30 天后读不到**；
**没人读过的对象会一直留在 R2**」。用户读到那句话会得出**错误结论** —— 这正是本项目最忌的那类。

**为什么一直没被发现**（对 `task-119` 是一个真实新实例）：
`infra/incident-collector/README.md` 里那条命令**以 `|| true` 结尾**：
```bash
npx wrangler r2 bucket lifecycle add xraytun-incidents --expire-days 30 --prefix "" || true
```
⇒ 加规则失败**被静默吞掉**，部署看起来成功。

**建议**（`infra/**` 归 ops，我没有动）：① 真加上规则（控制台或 API）；② 去掉那条 `|| true`，失败要留痕；
③ 把 `GET …/lifecycle` 的「必须存在 Age→Delete 30 天」断言写进 `deploy-check.sh`（我就是这样发现的）。

### 4.1 修完后的**只读复核命令**（交给 ops 复用；这条命令不改任何东西）

```bash
# 只读：确认桶上存在「Age(30 天) → 删除对象」的规则
# 令牌只从本地文件读、不进任何文件与报告；<acct> 由第一条命令推出
TOK="$(cat "$HOME/.cf-incident-token")"
ACCT="$(curl -s -H "Authorization: Bearer $TOK" \
        https://api.cloudflare.com/client/v4/accounts \
        | python3 -c "import json,sys;print(json.load(sys.stdin)['result'][0]['id'])")"

curl -s -H "Authorization: Bearer $TOK" \
  "https://api.cloudflare.com/client/v4/accounts/$ACCT/r2/buckets/xraytun-incidents/lifecycle" \
| python3 -m json.tool
```
**判据（写死，别靠印象）**：
* **修好之前**（我 2026-09-23 实测的原文，供对照）：
  `rules` 里**只有** `"Default Multipart Abort Rule"`（`abortMultipartUploadsTransition.maxAge = 604800`）⇒ **不符**；
* **修好之后**：`rules` 里除上面那条外，还要有一条 **对象过期**规则 ——
  形如 `deleteObjectsTransition.condition.type == "Age"` 且 `maxAge == 2592000`（= 30 天）。
  **字段名可能因 API 版本而异**（`deleteObjectsTransition` / `expiration`），所以判据看**语义**：
  「有一条按 Age 删除对象的规则，且天数 = 30」。
* 若输出里**仍然只有** multipart 那条 ⇒ **规则没加上**，不要因为「控制台点了」就认为成功。

### 4.2 ✅ 复核结果（2026-09-23 11:53:15 +0800）—— **已相符**

> **这是独立复核**：ops 修（`22a9473`）、我用 §4.1 那条命令与**写死的判据**自己量。**下面的输出是我自己取的**，不是引用他的。

```bash
$ TOK="$(cat "$HOME/.cf-incident-token")"
$ ACCT="$(curl -s -H "Authorization: Bearer $TOK" https://api.cloudflare.com/client/v4/accounts \
          | python3 -c "import json,sys;print(json.load(sys.stdin)['result'][0]['id'])")"
$ curl -s -H "Authorization: Bearer $TOK" \
    "https://api.cloudflare.com/client/v4/accounts/$ACCT/r2/buckets/xraytun-incidents/lifecycle" | python3 -m json.tool
{
    "success": true,
    "errors": [],
    "messages": [],
    "result": {
        "rules": [
            {
                "id": "Default Multipart Abort Rule",
                "enabled": true,
                "conditions": {},
                "abortMultipartUploadsTransition": { "condition": { "type": "Age", "maxAge": 604800 } }
            },
            {
                "id": "expire-30-days",
                "enabled": true,
                "conditions": {},
                "deleteObjectsTransition": { "condition": { "type": "Age", "maxAge": 2592000 } }
            }
        ]
    }
}
```

按 §4.1 的判据逐条核对（我自己的判定，不引用他人结论）：

| 判据 | 结果 |
|---|---|
| ① **默认 multipart 规则仍在**（没有被顶掉） | ✅ `Default Multipart Abort Rule`，`enabled = true`，`maxAge = 604800`（7 天） |
| ② 多出一条 **Age→Delete** 的对象过期规则 | ✅ `expire-30-days`，`enabled = true`，`deleteObjectsTransition.condition = {type: Age, maxAge: **2592000**}` |
| ③ **`maxAge == 2592000`（= 30 天）且 `enabled`** | ✅ 2592000 / 30 = 86400 s = 恰好 30 天 |
| **判定** | **相符** —— §4 那条不符项**已闭合** |

**根因**（ops 查明，我记录但不作为我的验证依据）：`wrangler r2 bucket lifecycle add <bucket> [name] [prefix]` 是**位置参数**，
旧 README 写的 `--prefix ""` 是**未知开关** ⇒ 命令必然失败 ⇒ 又被行尾的 `|| true` 抹平。
⇒ 这正是**「吞掉错误」把隐私承诺吃掉**的完整链条，已由 `task-136` 处置（含把该断言做进 `deploy-check.sh`）。

**这一条复核**能证明什么、不能证明什么（承接 §7.2）：
* ✅ 能证明：**存储层规则现在存在且启用**；
* ❌ 不能证明：30 天后对象**真的会被删**（lifecycle 是异步的，本环境无法观测）——
  发布说明若引用「30 天自动删除」，措辞仍应按 §7.2 的边界写。

## 5. CORS / 方法 / 响应头 —— **我量到的**

| 探测 | 结果 |
|---|---|
| `OPTIONS /api/incident` | **405** `method_not_allowed`「不支持 OPTIONS」 |
| `GET /api/incident`（裸路径） | **404** `not_found`「没有这个路由」+ 列出**允许的四个路由** |
| `PUT /api/incident` | **405**「不支持 PUT」 |
| `GET /api/incident/not-an-id` | **400** `bad_id`「id 形如 INC-YYYYMMDD-HHMMSS-<4hex>」 |
| 带 `Origin: https://evil.example` 的请求 | 响应里**没有** `Access-Control-Allow-Origin`（也没有任何 `Access-Control-*`） |

⇒ **没有开启 CORS**：浏览器跨源页面**读不到**响应，也**不能**代为发起写操作
（App 用的是原生 HTTP 客户端，不需要 CORS）。**后果说明（不是缺陷）**：将来若做 Web 端上传，需要显式加 CORS；
现在**不加是对的**（公开端点 + 无 CORS = 少一条滥用面）。
响应头里另有 `cache-control: no-store`、`x-robots-tag: noindex`、`server: cloudflare`、`cf-ray`、`nel`/`report-to`（CF 平台自带）。
⚠️ `report-to`/`nel` 是 **Cloudflare 平台**注入的，指向 `a.nel.cloudflare.com` —— 这属于**平台侧**遥测，不在 worker 控制范围内。

## 6. 与 `PRIVACY.md` 的逐条对照

| `PRIVACY.md` 的承诺 | 结论 | 依据 |
|---|---|---|
| 只存 zip + manifest；manifest 只有元数据 | **相符** | 读码（两处 `put()`）+ 量到（拒收后 0 对象；`hits` 无值字段） |
| **不存客户端 IP** | **相符（读码）** | `rateBuckets` 内存 Map、`put()` 不含 IP、manifest 字段清单无 IP；⚠️ CF 平台侧无法验证 |
| 不记 User-Agent / 设备指纹 | **相符（读码）** | 全文件未读 UA；manifest 只有可选的 `client_self_declared` |
| 不记订阅 URL / UUID | **相符** | 端点只做扫描拒收；**我自己实测** 422 且不回显 |
| manifest 不含日志正文 | **相符（读码 + 量到）** | `handleManifest` 只读 manifest key |
| 不开请求日志（observability） | **相符** | `wrangler.toml [observability] enabled = false` |
| **30 天：① R2 lifecycle ② 惰性过期** | ⚠️ **不符** | §4：lifecycle **没有** Age→Delete 规则；只剩惰性过期 |
| 原始 zip 只有持令牌者可读 | **相符** | 量到：无/错令牌 401；常量时间比较为**读码** |
| `DELETE` 带令牌可删 zip+manifest | **相符（读码）** | `handleDelete` 两个 key 都删（**我没有用正确令牌实测**，留给 `task-123`） |
| 限流 best-effort（每 isolate 一份内存） | **相符** | 读码；实测 422 与 415 的行为符合顺序 |
| 没有静默上传 | **不在本次范围**（客户端行为，`task-130`） | —— |

## 7. 诚实清单（**我没有测到的**）

> 前两条**单列成小节**：这类「我们测不到什么」比通过项更重要 ——
> 它们决定了这份审计**能**保证什么、**不能**保证什么。

### 7.1 边界一：**Cloudflare 边缘日志 / IP 留存 —— 无法从外部证明**

worker 侧我证明了「不把 IP 写进持久层」（读码：`rateBuckets` 是内存 Map；两处 `put()` 都不含 IP；
manifest 字段里没有 IP；`[observability] enabled = false`）。**但**：
* 请求是走 Cloudflare 边缘的，边缘**自身**的日志/遥测是否留存 IP，**我无法从外部证明**
  （响应里就有平台注入的 `nel` / `report-to`，指向 `a.nel.cloudflare.com` —— 那是 CF 的 NEL 端点）；
* `PRIVACY.md` 的措辞是「**我们**不把 IP 写进 R2」——**这条我验证了**；
  但如果读者把它理解成「Cloudflare 完全不留 IP」，那是**超出该承诺**的推断。
* ⇒ 结论：**「不写进我们的持久层」= 已证实；「平台上完全没有任何 IP 记录」= 无法证实**。

### 7.2 边界二：**lifecycle 的异步删除 —— 无法在本环境证明**

即使规则存在，我也**无法**从外部证明「30 天后对象真的消失」：
R2 的对象过期是**存储层异步执行**的，验证它要么等 30 天、要么看 CF 侧的删除事件/审计日志（本环境都做不到）。
⇒ 本次能证明的是**规则存在/不存在**（§4 量到了「不存在」）；**规则是否被真正执行**属于**测不到**的部分。
修完后的复核（§4.1）同样只能证明「规则在」，**不能**证明「30 天后一定删」——这一条要写进发布说明的话，请照此措辞。

### 7.3 其余未测项

1. **正确令牌的两条路径**（`GET blob` 200、`DELETE` 200）**没测** —— 卡面要求留给 `task-123`；
   因此「删除真的把两个 key 都删掉」我只能给**读码**，不是实测。
2. **限流计数**：我只能读码 + 用 1 次真实 422 验证「被限流的维度是 IP、阈值 5」；
   **没有**实测「第 6 次才 429」（那要打满额度，会挡住 `task-130` 的真实上传）。
3. **时序**：`constantTimeEqual` 的「常量时间」是读码结论，**没有做时序测量**。
4. **`expired` 措辞**：本机没有超过 30 天的对象 ⇒ 该分支的响应是**读码**，不是实测。
5. **>10 MB（413）与「魔数不符」（415）两条**：前者按卡面「不测带宽」，后者只走了 content-type 那条（不消耗额度）。
6. **`422` 计入限流**是按**代码顺序**得到的（限流在扫描之前）；我**没有**用「连续 5 次 422 再第 6 次」去实测，
   因为那会把额度打满、挡住 `task-130` 的真实上传。

## 8. 原始证据（本机）

| 文件 | 内容 |
|---|---|
| `/tmp/c1.out` | POST dirty 的 422 原始响应体 |
| `/tmp/c2.out` | POST `text/plain` 的 415 原始响应体 |
| `/tmp/b1.out`…`/tmp/b5.out` | 未知 id 404、`blob`/`DELETE` 的 401 原始响应体 |
| `/tmp/a1.out`…`/tmp/a4.out` | `OPTIONS` / 裸路径 `GET` / `PUT` / 非法 id 的原始响应体 |

**结论**：
1. ⚠️ **一条不符（本次审计的主要产出）**：`PRIVACY.md` §3 的 R2 lifecycle「30 天自动删除」**没有配置**（§4）。
   **分寸**：**不是对外泄露**，而是**承诺与实现不符**（用户会据此得出错误结论）。
   复核命令见 **§4.1**（只读，供 ops 修完后复用；判据写死在里面）。
2. 其余审计项**全部通过**：上传被拒时**不回显密钥、不留对象**（都实测）；`blob`/`DELETE` 无/错令牌一律 **401**；
   无 CORS（跨源读不到）；`OPTIONS` 405、裸路径 404、`PUT` 405、非法 id 400；
   限流 = 5 次/小时/IP（`422` 也计入，而 415-by-type / 413-by-declared 不计入）。
3. **两条测不到的边界**单列在 §7.1（CF 边缘日志/IP 留存）与 §7.2（lifecycle 异步删除）——
   引用本报告时请**连它们一起引**，否则会高估这份审计能保证的东西。
