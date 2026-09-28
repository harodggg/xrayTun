# 审计自动同步：验收证据（0.9.2）

> 需求原话：「你都做吧，能每天定时上传吗，而不是用我自己上传」
> 契约：`docs/design/AUDIT-SYNC.md`（三边都按它写）。
>
> 本文只写**能指向证据**的结论。跑过的命令与真实数字都列出来；没跑过的一律进 §4。

---

## 0. 结论（一句话）

**代码层面：三边（Rust 内核 + CLI / 桌面定时 + 命令 / Worker / 界面）全部落地，
Linux 聚焦检查 6 步全绿、macOS CI 绿、界面 675 项测试绿、Worker 38 项测试绿。**
**但"每天真的传上去"这件事还没被端到端验证过** —— Worker 没部署（本机没有 CF 凭据）。
要它真的开始工作，需要一次部署 + 一次配置，见 §5。

---

## 1. 代码提交

| 提交 | 内容 |
| --- | --- |
| `8af96bc` | **本次验证的对象**（界面 + 5 处编译/测试修复） |
| `ee1b17e` | Worker `infra/audit-collector/` + 契约补实现口径 |
| `a4492b0` | 桌面运行态 + 7 条命令 + 进程内定时 |
| `ba17588` | Rust 内核（离线报告 + 加密同步）+ CLI |
| `db79837`/`5a04442`/`e8c5b67` | 契约与 §11.5（离线审计）等文档 |

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

1. **Worker 未部署**：本机没有 CF 凭据 ⇒「密文真的写进 R2」「路由真的注册」**没跑过**。
2. **没有 macOS 机器** ⇒ 定时器在真实 App 生命周期里的行为（休眠、退出、升级、多实例）
   **没跑过**；"启动 60 秒后 / 每 30 分钟一次 / 按天补齐"目前只有单测与代码级证据。
3. **R2 lifecycle 命令**没在真 R2 上执行过（参数与读回复核步骤写在 Worker README §3.2）。
4. **限流是多 isolate 尽力而为**，不是全局精确限流、也**不是授权判据**（授权是 token）。
5. **服务端无法校验密文内容**（E2E 的直接后果）：拿到 token 的人可以覆盖某一天的对象（污染）。
   缓解：token 是用户自己的 secret、按天分键、`list` 会返回每天的行数与大小 ⇒ 不一致看得出来。
6. 界面上所有文案都是按冻结契约写的，**"真的连上 Worker 之后的观感"没看过**；
   浏览器预览兜底（`?audit=ready|pending|error`）有单测实跑。

---

## 5. 要让它真的开始每天上传，还差一步（需要你的 CF 凭据）

1. 部署 Worker（`infra/audit-collector/README.md` §3 有逐条命令）：
   ```bash
   npx wrangler deploy --config infra/audit-collector/wrangler.toml
   npx wrangler secret put AUDIT_TOKEN --config infra/audit-collector/wrangler.toml
   ```
2. 按 README §3.2 给 R2 桶 `xraytun-audit` 加 400 天 lifecycle，并**读回复核**。
3. 按 README §3.4 做部署后自测（路由真的注册 + `POST` 真的 200）。
4. 本机：装上这一版 App → 设置 → 系统与助手 → **审计同步** → 打开开关、填 token。
   开关只改意图；**token 与密钥齐备前，界面会说"还差什么"，而不会有任何请求发出去**。

或者完全不部署，用命令行先看数据长什么样（**不联网**）：

```bash
cargo run -p xt-intent --example intent_audit -- report
cargo run -p xt-intent --example intent_audit -- bundle --day 2026-09-27
```
