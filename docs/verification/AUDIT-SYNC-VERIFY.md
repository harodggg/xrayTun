# 审计自动同步：验收证据（0.9.2）

> 需求原话：「你都做吧，能每天定时上传吗，而不是用我自己上传」
> 契约：`docs/design/AUDIT-SYNC.md`（三边都按它写）。
>
> 本文只写**能指向证据**的结论。跑过的命令与真实数字都列出来；没跑过的一律进 §4。

---

## 0. 结论（一句话）

**代码 + 线上端点都通了。** 四边（Rust 内核 / CLI / 桌面定时 + 命令 / Worker / 界面）全部落地；
Linux 聚焦检查 6 步全绿、macOS CI 绿、界面 675 项测试绿、Worker 38 项测试绿；
**Worker 已于 2026-09-28 部署上线，并用真实客户端跑过一次端到端**：
密文落进 R2、从 R2 取回的对象**没有任何明文残留**、撤回把测试数据清干净。

**仍未验证的只剩一件**：没有 macOS 机器 ⇒ 定时器在真实 App 生命周期（休眠 / 退出 / 升级）
里的行为没跑过。要让它真的开始传，见 §5（两步，其中一步是**吊销你贴出来的那个 CF token**）。

---

## 1. 代码提交

| 提交 | 内容 |
| --- | --- |
| `8af96bc` | **本次验证的对象**（界面 + 5 处编译/测试修复） |
| `ee1b17e` | Worker `infra/audit-collector/` + 契约补实现口径 |
| `a4492b0` | 桌面运行态 + 7 条命令 + 进程内定时 |
| `ba17588` | Rust 内核（离线报告 + 加密同步）+ CLI |
| `db79837`/`5a04442`/`e8c5b67` | 契约与 §11.5（离线审计）等文档 |

> `8af96bc` **之后**的提交都只改文档（本文件、契约的线上事实、Worker README 的部署记录），
> 不再动代码 —— 所以代码层面的验证对象始终是 `8af96bc`。

> 中间的 `a4492b0` … `7298043` 在 CI 上是**红**的（我自己写出来的编译/测试问题，见 §3）。
> 绿的是最终提交 `8af96bc` 及其后代（文档提交）。

---

## 2. 跑过的检查（命令 + 真实结果）

### 2.1 macOS CI（**这是本项目的真闸门**）

`scripts/check.sh` 在 `macos-14` 上跑（含真实 xray 二进制的 `real_core*` 集成测试、
release 构建、界面测试、类型契约）。

| 提交 | 结论 | 证据 |
| --- | --- | --- |
| `8af96bc` | **success** | GitHub Actions run `36394798055`（CI #376） |

> 对照：`a4492b0` / `d44c069` / `7298043` 都是 `failure`，`e7ee67a` 也是 `failure`
> —— 那正是 §3 里那批问题的现场；修完之后才绿。

### 2.2 远端 Linux 聚焦检查（Cloud Run Job `xt-verify-intent`）

runid `20260928T080920Z-816270`，commit `8af96bc`，**6 步全 rc=0、exit 0**：

| step | rc | 真实数字 |
| --- | --- | --- |
| patch | 0 | ui/dist 占位 + 三个占位二进制 + 删 objc2 行 + 覆盖 login_item.rs |
| clippy_intent | 0 | `cargo clippy -p xt-intent --all-targets -- -D warnings` |
| test_intent | 0 | `cargo test -p xt-intent --lib` → **230 passed / 0 failed / 1 ignored** |
| clippy_desktop | 0 | `cargo clippy -p xraytun-desktop --all-targets -- -D warnings` |
| test_desktop | 0 | `cargo test -p xraytun-desktop --lib` → **410 passed / 0 failed / 5 ignored** |
| type_contract | 0 | **8 passed**（命令集 57 = 57 双向相等，含本次新增的 7 条） |

### 2.3 界面（本机）

| 命令 | 结果 |
| --- | --- |
| `cd apps/ui && npm test` | **Test Files 69 passed**；**Tests 675 passed \| 1 todo**；exit 0 |
| `cd apps/ui && npm run build` | 成功（`tsc --noEmit` + vite build；`index-8vAT2jJ3.js` 326.63 kB） |

### 2.4 Worker（本机）

| 命令 | 结果 |
| --- | --- |
| `cd infra/audit-collector && node --test` | **tests 38 / pass 38 / fail 0** |
| `bash verify.sh` | **pass=25 fail=0** |
| `bash verify.sh --sensitivity` | pass=2 fail=0（拿掉鉴权 → 3 条红；拿掉 device 正则 → 4 条红） |
| `bash deploy-check.sh` | pass=19 fail=0 |
| `bash smoke.sh`（真 workerd） | 绿（401 JSON） |

我另外**逐条核过**的安全点（不是转述子 agent 的话）：
`AUDIT_TOKEN` 没配 ⇒ 一切操作 **401（fail closed）**；
device **两道**正则（入口 + `auditKey()` self-guard，防 `../` 前缀穿越）；
R2 `list` 分页到底、`truncated` 却没 cursor 或 cursor 不前进 ⇒ 抛错 500；
先看 `Content-Length` 再读 body；响应**不回显 `ct`**；`routes` 在任何表头之前。

### 2.5 线上端点（2026-09-28 部署 + 真实请求实测）

| 项 | 结果 |
| --- | --- |
| Worker | `xraytun-audit-collector` 已部署；version id `90526093-42e3-40c8-b35e-5c9eeddddf59` |
| 路由 | `xraytun.top/api/audit` 与 `xraytun.top/api/audit/*` —— **用 CF API 读回确认**（不是看部署日志说成功） |
| 桶 + 保留期 | `xraytun-audit` 已建；400 天 lifecycle **读回复核**：`maxAge=34560000, enabled=true, prefix=""` |
| 鉴权 | `AUDIT_TOKEN` 已设；**没设 token 时一律 401（fail closed 实测）** |
| 收单实测 9 项 | 无 token→401；错 token→401；正确→200 且 key 正好是 `audit/<device>/<day>.json`；同日重传→`replaced:true` 且 key 不变（幂等）；`device` 前缀穿越→400；`ct` 非 hex→400；list→元数据正确；revoke→`deleted:1`；revoke 后 list 为空 |
| 真实客户端 E2E | GCP Cloud Run runid `20260928T083233Z-818629`（3 步全 rc=0）：`intent_audit bundle` → `upload` → `list`。上传 1 天（2026-09-27）、1 个请求；服务端清单显示 `audit/5d607edaed17b57c/2026-09-27.json`，527 字节、1 行 |
| 组包时的隐私断言 | 明文 bundle 里 day=2026-09-27、1 行、754 字节（pretty）；`E2E-PRIVACY-CANARY` 命中 **0**、`context_sent` key 命中 **0** ⇒ 本地开了"记录外发内容"也不会跟着传 |
| 从 R2 取回检查 | 合法信封（v=1、alg=chacha20poly1305、rows=1、nonce 24 hex、ct 1086 hex = 543 字节 = 527 明文 + 16 tag）；明文残留检查 `e2e-canary.example` / `E2E-PRIVACY-CANARY` / `context_sent` / `ads_intent` / `jev-1.13-free` **全部 0 命中** |
| token 轮换 | 一次性 token 用完即换：旧 token→**401**、新 token→**200**（⚠️ secret 传播约 45 秒，别立刻断言失败） |
| 清理 | 测试产生的 3 个对象**已全部 revoke**，桶里只留你自己的数据（现在为空） |

另外两条线上事实（已写进契约 §4 与 Worker README）：

* **空 `User-Agent` 会被 CF 的 Browser Integrity Check 挡在 Worker 之前**：`403` + `error code: 1010`。
  我们客户端固定发 `xraytun-audit-sync/<version>`，实测能到 Worker（拿到的是 Worker 的 401）。
* 部署只创建/更新了 `xraytun-audit-collector` 与 `xraytun-audit` 两个资源；
  账号里其它 8 个 Worker（`eth-arb-scout` / `vless` / `xraytun-incident-collector` …）与
  另外两个桶（`mymutlicloud` / `xraytun-incidents`）**一律没动**。

---

## 3. 被实验推翻的判断（这个仓库的文化：写下来）

1. **我自己的 p95 测试夹具写错了。** 第一版用 `h{i}.example`，同名域在更早的天就出现过
   ⇒ "新域/天"恒为 1、p95 恒为 1。远端真跑出 `left: 1, right: 19` 才发现。
   现在夹具带天（`d{d}h{i}`）并断言 1..20 与均值 10.5。
2. **`ring::rand::SystemRandom::new()` 不返回 `Result`**（会失败的是 `fill`）——编译期抓到。
3. **`xt-helper` 在 Linux 上本来就编不过**（`LOCAL_PEERTOKEN` / `getpeereid` / framework 链接）：
   昨天的全量探针 `20260927T203115Z-714494` 里同样红。⇒ 所以**"全量 probe 绿"在本环境不是可用判据**，
   本轮用的是"打好 patch 的聚焦 step"。
4. **占位 xray 二进制会让 `real_core*` 集成测试 panic（而不是 skip）**：
   `core_binary()` 找到那个空的可执行文件 → `Command::new` 起不来 → `.expect(...)` panic。
   所以容器里只跑 `--lib`；**真实核心的验收交给 macOS CI**（它带真二进制）。
5. **我第一次远端运行用了自己猜的 SHA**（`72980435…`），克隆必然失败 —— 自己发现后杀掉重跑。
6. 子 agent 抓到的真运行时坑：入口模块的命名导出会被 workerd 当候选 handler
   （`'ALG' is not of type function or ExportedHandler`）⇒ **`node --test` 全绿但 workerd 起不来**，
   所以另有 `smoke.sh` 用真 workerd 冒烟。已写进 Worker README §2.1。

---

## 4. **没有**验证的（不许当成已验证）

1. **没有 macOS 机器** ⇒ 定时器在真实 App 生命周期里的行为（休眠、退出、升级、多实例）
   **没跑过**；"启动 60 秒后 / 每 30 分钟一次 / 按天补齐"目前只有单测与代码级证据。
   （`xraytun.top` 本身在大陆的可达性由既有的 `/api/incident` 长期使用佐证，但**我在这里没法实测大陆网络**。）
2. **限流是多 isolate 尽力而为**，不是全局精确限流、也**不是授权判据**（授权是 token）。
3. **服务端无法校验密文内容**（E2E 的直接后果）：拿到 token 的人可以覆盖某一天的对象（污染）。
   缓解：token 是用户自己的 secret、按天分键、`list` 会返回每天的行数与大小 ⇒ 不一致看得出来。
4. 界面上所有文案都按契约写了，但**"真机上连上端点之后的观感"没看过**；
   浏览器预览兜底（`?audit=ready|pending|error`）有单测实跑。
5. **`real_core*` 集成测试**在 Linux 容器里没跑（缺真实 xray 二进制）；由 macOS CI 覆盖。

## 5. 现在让它真的开始每天上传：你要做两件事

### 5.1 把上传 token 填进 App

token 文件在 **`<workspace>/.secrets/audit-upload-token`**（0600，不在仓库里）。

1. 装这一版 App（或直接用 CLI 验证）：

   ```bash
   cargo run -p xt-intent --example intent_audit -- report          # 先看数据长什么样（不联网）
   cargo run -p xt-intent --example intent_audit -- list \
     --state ~/.xraytun-audit-sync.json --token-file .secrets/audit-upload-token
   ```
2. App：设置 → 系统与助手 → **审计同步** → 打开开关 + 粘贴 token。
   （开关只改意图；token 与密钥齐备前，界面会说"还差什么"，而不会有任何请求发出去。）

### 5.2 ⚠️ 吊销你在聊天里贴出来的那个 CF API token

它已经**明文出现在对话里**，而它对这个账号有 Workers / R2 / Zone 的读写权限。
去 Cloudflare Dashboard → My Profile → API Tokens → **Roll/Delete** 掉它，需要时再建一个新的。

它现在只存在两处：你发出的那条消息，以及本机 `.secrets/cf-audit-token`（0600、不在仓库里；
本仓库是公开仓库，我已确认 `.secrets/` 不会被提交）。

**部署用的上传 token（`AUDIT_TOKEN`）是另一回事** —— 那是我新生成的随机值，
只存在 Worker secret 与本机 `.secrets/audit-upload-token` 里，不随上面那个 token 一起失效。

### 5.3 想撤回这条链路

* App 里「撤回全部已上传」⇒ 服务端删掉本设备的全部密文；
* 关掉开关 ⇒ 不再有任何请求；
* 彻底不要了：删 Worker + 桶即可（README 里给了资源边界，别误删别的）。
