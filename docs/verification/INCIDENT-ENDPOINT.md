# 现场包接收端点（task-114）：机制、边界与原始证据

> 交付物在 `infra/incident-collector/`（**故意不放 `site/`**：`main` 上的提交会触发 Pages 部署，
> 后端代码不能发布成静态站点）。本页记录：**做了什么、为什么这么做、验到了哪一步、验不到什么**。

## 1. 机制

```
POST   /api/incident             公开  ← 大小上限 + content-type 白名单 + zip 魔数 + 按 IP 限流
GET    /api/incident/<id>        公开  ← manifest（无日志正文、无 IP、无账号）
GET    /api/incident/<id>/blob   需 X-Auth-Token（常量时间比较）
DELETE /api/incident/<id>        需 X-Auth-Token（用户要求删除）
```

* **路由 = 路径而不是新子域**：`xraytun.top/api/incident/*`（同 zone 的 Workers Route 优先于 Pages；
  **不需要新 DNS**）。选它的决定性理由：**用户机器在中国大陆，`*.workers.dev` 经常不可达**，
  而 `xraytun.top` 已实测可达。
* **零依赖实现**（`src/worker.mjs`，只用 Web 标准 API + R2 绑定）；单测用 `node --test`，
  **不装任何 npm 包、不联网、不碰 CF 账号**。
* **保留 30 天两道保证**：R2 lifecycle（部署时设）+ 代码里的**惰性过期**（读到超期即删并 404）。
* **不存 IP**：限流的键 = `SHA-256(每次 isolate 启动随机 salt + IP)` 的前 16 hex，**只在内存**；
  manifest 里没有 `ip` / `client_ip` / `user_agent` 字段（用例钉住）。
* **不需要 CF 账号即可自测**：`verify.sh` 用内存 R2 替身覆盖每个分支。

## 2. 原始证据

### 2.1 本地自测（绿）

```
$ ./infra/incident-collector/verify.sh
  [1] 真源码：全部用例
    ℹ tests 16
    ℹ pass 16
    ℹ fail 0
  ✓ 真源码：fail=0（大小上限 / 类型白名单 / 乱序魔法 / 限流 / 鉴权 / 删除 / 保留期 / 路由 全覆盖）
  [2] 顺带自证：不存在的模块路径必须**报错**（防止「测了个空」）
  ✓ 模块缺失时报错（不是静默 0 个用例）
  pass=2 fail=0   ✓ 端点自测通过
```

16 个用例（每个分支一条，文件名 `test/worker.test.mjs`）：
上传 201/落盘两键、超出上限 413（且不落盘）、content-type 415、魔数 415、限流第 3 次 429 + `Retry-After`、
**manifest 不含 IP**、自述版本字段的格式门、manifest 公开可读、id 格式 400 / 不存在 404、
blob 无 token/错 token 401 / 对 token 200 + 字节一致、**未配 token 时 fail closed**、
删除未授权 401（且不动数据）/ 授权删除 200 后 404、保留期过期 404（并清两键）、
路由边界（POST 子路径 404 / GET 基路径 404 / PUT 405 / 尾斜杠可用 / 别的前缀 404）。

### 2.2 双向敏感性（拿掉鉴权 / 拿掉大小上限 ⇒ 必须红）

```
$ ./infra/incident-collector/verify.sh --sensitivity
  [A] 去掉「鉴权」⇒ 鉴权相关用例必须红，且其它用例仍须通过
    ✖ blob：不带 token ⇒ 401；错 token ⇒ 401；对 token ⇒ 200 + 原始字节
    ✖ blob：没配置 INCIDENT_TOKEN 时 ⇒ 一律 401（fail closed）
    ✖ 删除：不带 token ⇒ 401 且什么都没删；带 token ⇒ 200，随后 manifest 404
    ℹ tests 16 ｜ pass 13 ｜ fail 3
  ✓ 变体 A 变红且**只红该红的**（fail=3/tests=16）
  [B] 去掉「大小上限」⇒ 该用例必须红，且其它用例仍须通过
    ✖ 上传：超出大小上限 ⇒ 413
    ℹ tests 16 ｜ pass 15 ｜ fail 1
  ✓ 变体 B 变红且**只红该红的**（fail=1/tests=16）
  pass=2 fail=0   ✓ 双向敏感性成立
```

### 2.3 两次**我自己的假绿**（都当场修掉，留档）

1. **`sed` 把变体模式写坏** ⇒ 变体文件为空 ⇒ 16 条全红，而当时的脚本只要「有红」就判成立。
   **假绿：那次的红是「模块加载失败」，不是断言在验鉴权。**
   现在 `verify.sh` 会拒绝这种红：变体文件必须非空；**若全 16 条都红则判失败**
   （「整体崩了 ≠ 这条断言在验它」），并要求「不该受影响的用例仍然通过」。
2. **限流状态串味**：限流是模块级内存，第一版测试没在用例间复位 ⇒ 后面的用例拿到 429 而不是各自要验的状态码。
   现在测试里有 `test.beforeEach(() => worker.resetRateLimits())`。
   （顺带说：这两次都是**测试自身**制造的错误结论，与 build-lock 那次「敏感性模式下把 stale artifact 删了」同族。）

## 3. 部署前必须知道的两条硬约束（来自 lead 的账号只读探测）

* 账号里已有 8 个**别人的** Worker 与 `mymutlicloud` 桶 ⇒ 本目录**只创建/更新**
  **`xraytun-incident-collector`**（Worker）与 **`xraytun-incidents`**（R2 桶）；
  **没有任何 prune/delete 命令**；README 里写明「若输出里出现别的名字，说明命令写错了，停下来」。
* zone 上当前**没有** Workers Route ⇒ 与 Pages 暂不冲突；若 `wrangler deploy` 报
  「需要 hostname 的 DNS 记录」之类，**把原始报错交给 lead**，不要自行新建子域。

## 4. 诚实清单：**没有 CF 账号时我验证不到什么**

| 验不到 | 说明 |
|---|---|
| 真的部署 | 本卡**不做部署**（等 lead 用临时授权执行）；wrangler 版本行为、`routes` 是否需要 Zone 权限都未实测 |
| **Workers Route 与 Pages 的实际优先级** | 依据是 Cloudflare 文档（同 zone Workers Route 优先）；**本账号未实测** |
| **R2 lifecycle 真的会删** | 只写了命令与配置；未在真桶上观察过 30 天到期 |
| 真 `CF-Connecting-IP` | 本地/R2 替身里是我自己塞的头；真实请求由 CF 注入，语义一致但未实测 |
| `INCIDENT_TOKEN` secret 的读写 | 未在真账号上 `secret put`/读取 |
| wrangler 的 `r2 bucket lifecycle` 子命令名 | 不同 wrangler 版本可能不同（README 里给了控制台兜底路径）；**未实测** |
| 成本/配额 | Workers 请求数、R2 存储与操作数未评估（10 MiB × 少量包，量级很小，但**没算过**） |
| 限流的「跨 isolate」行为 | 本实现是 best-effort；真实并发下的有效上限未实测 |

## 5. 部署后要跑什么（由 lead 执行，原始输出回贴）

见 `infra/incident-collector/README.md` §3.4：上传 → manifest → blob 带/不带 token → 限流 6 连发 →
delete；每步期望码：`201 / 200 / 401 / 200 / 429 / 200 / 404`。

## 6. 与「不做静默上传」的关系

上传只由 App 里用户的一次明确点击触发（`task-115`）；打包脚本（`task-113`）在发送前会生成
`README.txt` 让用户看到包内清单。端点侧没有任何「自动上报」逻辑 —— 它只对请求作出响应。

## 7. 首次部署被 CF 拒（真实事故）与它的机制化

lead 用临时授权实际部署时，**Worker 被 Cloudflare 运行时拒绝**：

```
Uncaught Error: Disallowed operation called within global scope.
Asynchronous I/O (ex: fetch() or connect()), setting a timeout, and generating random values
are not allowed within global scope.
at …/src/worker.mjs:125:10                                    [code: 10021]
```
根因：限流盐写成模块顶层 `const RATE_SALT = (() => { crypto.getRandomValues(...) })()`。
**16 条 `node --test` 全绿也不可能抓到它** —— node 不执行 workerd 的全局作用域限制。
⇒ 修法：惰性初始化（`let rateSalt = null` + `getRateSalt()`，首次用到时才生成；**设计意图不变**，
盐仍只在本 isolate 内存里）。复核：账号里仍是 8 个脚本、zone 上 0 条路由 ⇒ **没有半成品状态**。

**机制化 = `smoke.sh`（真 workerd）**：

```
$ ./infra/incident-collector/smoke.sh
  （等 Ready：已就绪）
  HTTP 响应码：404
  响应体：{  "error": "not_found",  "message": "没有这个 id"}
  ✓ 真运行时起来了，并且用我们的 JSON 回话（404 not_found）——说明模块能加载、handler 能跑

$ ./infra/incident-collector/smoke.sh --sensitivity     # 把顶层取随机值的 bug 回退
  （等 Ready：未就绪）
  HTTP 响应码：000
  ✓ 敏感性成立：把「顶层取随机值」回退后，真运行时**起不来 / 不回话** ⇒ 这条冒烟测试抓得住它
（运行时日志里出现同一句 `Disallowed operation called within global scope` + `The Workers runtime failed to start`）
```

## 8. 服务端隐私拒收（最后防线，`POST` 落盘之前）

客户端打包脚本（`task-113`）负责脱敏，但 tester **当场抓到它自己三处漏**（JSON 形键值、
URI 的 `?query/#fragment`、`Authorization` 只吃掉 `Bearer`）。端点会把包**原样存 30 天**
⇒ 客户端漏一次，密钥就在云上躺一个月。所以服务端加一层**拒收**（两层互不替代）：

* 扫 zip 内文本（极简 zip 读取：EOCD + 中央目录 + 本地头；**store 与 deflate 都要**），命中即 **422 `secret_detected`**；
* 模式：`uuid_literal`（**排除 `<uuid>` / 全 x / 全 0 这类占位**）、`uri_secret_param`
  （`pbk=`/`sid=`/`spx=`/`token=`/`password=`/`uuid=`/`key=`/`secret=`…，值 ≥8 字符）、
  `private_key_pem`、`node_url`（`vless://` `vmess://` `ss://` `trojan://` …）、`json_secret_key`；
* **响应只给 `{type, file, line}`（最多 20 条，`hits_truncated` 标截断），绝不回显密钥原文**；
* **fail closed**：zip 读不成 / 条目解不开 / 扫描器抛异常 ⇒ **422 `scan_failed`**（不是放行）。

测试（`verify.sh` 现在 **23 条**）+ 敏感性变体 C：

```
$ ./infra/incident-collector/verify.sh
  ℹ tests 23 ｜ pass 23 ｜ fail 0        ✓

$ ./infra/incident-collector/verify.sh --sensitivity
  ✓ 变体 A（拿掉鉴权）      fail=3/23  只红：blob 无 token / delete 无 token / 未配 token fail-closed
  ✓ 变体 B（拿掉大小上限）  fail=1/23  只红：超出大小上限
  ✓ 变体 C（拿掉隐私扫描）  fail=7/23  只红：vless / JSON 键值 / URI 参数 / PEM / deflate / 限长 / fail-closed
  pass=3 fail=0
```

**又一次我自己的假绿（第三次，留档）**：变体一开始写在 `$TMP` 里，而 `worker.mjs` 现在有相对
import（`./secret-scan.mjs`）⇒ 变体加载失败、整份测试报「1 条红」，脚本一度把它当「全红=整体崩」拦下，
但换到 `src/` 旁边之前根本没有有效证据。**结论**：变体必须与源码同目录，且「全红」永远是可疑信号。

## 9. 环境事实（lead 实测，写进 README）

* `~/.npm` 被 root 污染 ⇒ `npx` EPERM：**不要 `sudo chown`**，用
  `npm_config_cache=/tmp/dsh-npm-cache` 绕开（`smoke.sh` 会先验可写性再决定）；
* wrangler 的日志目录也可能 EPERM ⇒ `WRANGLER_LOG_PATH=/tmp/dsh-wrangler-logs`；
* **wrangler 认 `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`**，不是 `CF_API_TOKEN` / `CF_ACCOUNT_ID`；
* 桶 `xraytun-incidents` **已经建好**：重复 `create` 会报错 ⇒ 先 `r2 bucket list` 确认。

## 10. 部署后自检（「本地绿 ≠ 运行时绿」的第三次）

`verify.sh`（23 个单测）与 `smoke.sh`（真 workerd）**都跑在本地，不经过 Workers Route** ⇒
它们**抓不到**「路由没注册」这类部署期错误。2026-09-23 连撞两次，两次都是**部署日志成功 + 端点 405**：

| 事故 | 根因 | 表现 |
|---|---|---|
| ①`env.routes` | `routes = [...]` 落在 `[vars]` 之后 ⇒ 被当成**环境变量** | 部署输出把它列进 Environment Variables；路由 0 条 ⇒ **405** |
| ②落进 `[[r2_buckets]]` | 改成放前面、但仍在 array-of-table 里 ⇒ wrangler 报 `Unexpected fields found in r2_buckets[0] field: "routes"` | 同上 |
| ③少了一条 pattern | 只注册 `…/api/incident/*`，**匹配不到 `/api/incident` 本身** | 上传入口落到 Pages ⇒ **405** |

修法（lead 已推 `c39e2e5` / `863901a`）：`routes` 放**文件最前（任何表头之前）**，并注册**两条** pattern。
**机制化 = `deploy-check.sh`**（跑在真实部署之后）：

```
$ ./infra/incident-collector/deploy-check.sh          # 不落盘，唯一必需的一步
  [3] 真请求：POST 入口必须由 Worker 处理（脏包 ⇒ 期望 422，且**不落盘**）
      POST https://xraytun.top/api/incident → HTTP 422
        { "error": "secret_detected", "hits": [ { "type": "node_url", "file": "nodes.txt", "line": 1 } ], … }
  ✓ HTTP 422 —— 请求确实进了 Worker（Pages 不会给 422），且脏包不会落盘
  ✓ 响应体是 secret_detected（命中信息只有 type/file/line）
  ✓ 响应体里没有密钥原文
  [1] 部署日志：缺 --deploy-log ⇒ ⏭（手动查：不得出现 env.routes / Unexpected fields）
  [2] 路由注册：缺 CLOUDFLARE_API_TOKEN ⇒ ⏭（脚本**不会**去读 ~/.cf-incident-token）
  pass=3 fail=0 skip=3   ✓ 部署后自检通过
```

**判据三条（缺一不可）**：① 部署日志无 `env.routes` / `Unexpected fields`；
② `GET /zones/<zone>/workers/routes` 能看到**两条** pattern；③ `POST {BASE}` 不是 405/404。
**「部署日志说成功 ≠ 端点能用」** —— 今天就是被这条咬的；`--upload` 可跑完整环回（带 `INCIDENT_TOKEN` 会自动删）。

## 11. 限流的真实行为（实测）

lead 部署后连续 POST **未触发限流**（每 isolate 各自计数、跨 colo 不共享）⇒
README 里「best-effort、**不是安全边界**」的口径被实测印证：
真正的门是「公开端点 + 大小/类型白名单 + **服务端隐私拒收** + 30 天保留」。

## 12. 我这边实测的一条残留

`deploy-check.sh` 与手工探活期间，我 `POST` 过一个 180 B 的干净包
（id `INC-20260923-031304-3c5b`）。它**不是**泄漏（内容是自造 fixture），30 天后自动过期；
要立刻清就用端点令牌 `DELETE /api/incident/INC-20260923-031304-3c5b`（**我没有端点令牌**，故未删）。

## 13. `deploy-check.sh` 自己的安全行为（tester 实测失误 → 机制）

tester 独立复跑时用 `curl -o /dev/null` 丢了 201 的响应体 ⇒ **拿不到 id ⇒ 删不掉那个测试包**，
只能等 30 天过期（lead 最后用 R2 API 逐对象清）。⇒ 工具不再允许这种「留下一个删不掉的对象」：

* **任何 POST 的响应体都会被解析 `id`**：取到 ⇒ 打印 `ID=…` + 追加到
  `${ID_FILE:-$TMPDIR/xraytun-incident-uploaded-ids.txt}` + 打印可复制的 `DELETE` 命令；
* `--upload` 且带 `INCIDENT_TOKEN` ⇒ 自动 DELETE 并复核 404；DELETE 失败 ⇒ 报「**残留**」并给出 id 与文件位置；
* **2xx 却没有 id** ⇒ 明确报「**无法清理**」并以非 0 退出（不静默）；
* `--self-test`（用**本地 mock 服务**，不碰 Cloudflare、不写 R2）覆盖两条分支：
  ```
  $ ./infra/incident-collector/deploy-check.sh --self-test
    带 id 的 201：id 被打印 ✓ / id 已落盘 ✓ / 自动 DELETE 真的发出去了 ✓ / 退出码 0 ✓
    缺 id 的 201：明确报「无法清理」✓ / 以非 0 退出（不静默）✓
    pass=6 fail=0   ✓ deploy-check 自测通过
  ```
  （自测里 mock 也按真端点的行为区分脏包：含 `vless://` ⇒ 422。）
