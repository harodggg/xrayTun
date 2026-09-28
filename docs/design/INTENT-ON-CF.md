# 意图过滤搬到 Cloudflare：方案与评估（0.9.2 提案）

> 作者：Lead · 状态：**提案，未实施**
> 触发：用户提议「把域名清洗放到 CF 上，用 Workers AI / 大模型直接去做」
> 本文回答两件事：**怎么设计**、**是不是更好**（含"不好"的部分）。

---

## 0. 一页结论

| 问题 | 结论 |
| --- | --- |
| 判决（问大模型）该不该搬到 CF？ | **该搬，而且这是本轮最大的一块收益** —— 但收益主要不来自"换模型"，来自**全局共享缓存 + 服务端可迭代 + 客户端不再存 key** |
| 域名清洗该不该放 CF？ | **该做，但清洗不能是第一跳**。清洗要**两层都做、同一套规范与夹具**：App 本地先清洗 → 命中本地缓存就**零联网**；没命中才带规范化域名去 CF |
| 该不该换成 Workers AI？ | **先评测再决定**。仓库已有离线评测夹具（`crates/xt-intent/src/eval.rs` + PRD P1.5 门禁）。Workers AI 的开放模型在这类「广告/追踪意图」判断上的**校准质量未知**，没跑过评测就切 = 拿用户的误杀率做实验 |
| 会更好吗？ | **会，但有前提**：① 本地缓存/预算/fail-open 必须保留；② 共享缓存**不落明文域名**；③ 换模型必须过离线评测；④ 服务端的提示词/模型版本必须回传给 App，否则本地缓存指纹会错 |

**一句话**：把「判决层」搬到 CF 是**架构上正确**的一步（共享缓存 + 无 key + 服务端迭代），
把「模型」换成 Workers AI 是**另一个独立决定**，必须被评测门禁卡住，
而「域名清洗」是两层的共同底座、不是某一层的专属职责。

---

## 1. 现状事实（可复核）

| 事实 | 位置 |
| --- | --- |
| 四层漏斗：L0 静态名单（0 成本）/ **L1 域名意图（大模型）** / L2 流量形状（本地加权）/ L3 MITM（opt-in） | `docs/design/INTENT-FILTER.md` §2 |
| **计费单位 = 一条新域名**，不是一条连接；同域名第 2..n 条连接零成本 | 同上 §7.3 |
| 判决缓存：`domain → Verdict + 证据 + 过期时间`，落盘原子写 + 版本号；Allow 30d / Block 90d / Deferred 1h | `crates/xt-intent/src/cache.rs`、`engine.rs:31-37` |
| 缓存指纹 = 模型 id + 网关 baseURL + **问题措辞版本** + 阈值 → 任一变化**整库作废** | `cache.rs:40`、`engine.rs:90` |
| 提示词写死在客户端（`question.rs`），改一句 ⇒ `QUESTIONS_REVISION +1` ⇒ **整库作废 + 必须发版** | `cache.rs:26`、`question.rs` |
| 网关**可插拔**：typesafe(Jev) / zen(免key) / openrouter / vercel | `gateway.rs`、`INTENT-FILTER.md` §7.6 |
| 预算：每小时/每天上限（默认 ≤200/天），超额只放行 + 审计记 `budget_exhausted` | `engine.rs` `Budget` |
| 审计：每次判决写一行 JSONL，界面要能展示**模型的原话与分数**（不许黑盒） | `INTENT-FILTER.md` §7.4 |
| 失败一律 **fail-open**（网关错 → 放行 + requeue），缓存坏 → 当空库 | `cache.rs`、`engine.rs:356-358` |
| **刚加的能力**（0.9.1）：父域（eTLD+1）继承、`cache_misses`/`cache_inherited` 计数、网关失败 60s 冷却 | `rules.rs:246+`、`cache.rs`、`engine.rs` |
| 仓库**已有 CF Worker**：`xraytun-incident-collector`（Worker + R2），同 zone 路由 | `infra/incident-collector/` |
| ⚠️ **`*.workers.dev` 在中国大陆经常不可达**，所以既有 Worker 走 `xraytun.top/api/incident` 同 zone 路由（Workers Route 优先于 Pages） | `infra/incident-collector/wrangler.toml` 头部注释 |
| 既有 Worker 的隐私口径：不存客户端 IP（限流用**加盐哈希**做键、只在内存）、不采集请求日志、R2 只存 zip 与 manifest | `infra/incident-collector/PRIVACY.md`、`worker.mjs` 头部 |

**这最后两条是本方案的现成底座**：路由方式、秘密管理（`wrangler secret put`）、
「不落 IP、只落加盐哈希」的隐私习惯、以及 `node --test` 的无依赖测试风格，都可以直接复用。

---

## 2. 建议架构：三层，各自的职责互不重叠

```
┌───────────────────────── App（macOS，本地）─────────────────────────┐
│ 观测（ConnectionRecord）→ 清洗①（规范化/去端口/eTLD+1）             │
│        │                                                            │
│        ├─ 本地缓存命中？ ──是──▶ 直接用（**零联网**，离线可用）      │
│        │         ▲                                                  │
│        └─否──▶ 本地预算闸（≤N/天）──▶ POST /api/intent               │
│                                              │                      │
│  写 Xray 路由（block/allow）◀── 判决 + 证据 + 版本 ──┘              │
│  本地审计 JSONL（模型原话与分数）                                   │
└──────────────────────────────────────┬──────────────────────────────┘
                                       │  HTTPS（同 zone：xraytun.top/api/intent）
┌──────────────────────────────────────▼──────────────────────────────┐
│ CF Worker（判决层）                                                  │
│  ① 鉴权（App 持有的轻量 token，不等于模型 key）                      │
│  ② 清洗②：与 App 同一套规范（去端口/小写/去尾点/eTLD+1/IP 字面量拒绝）│
│  ③ 全局共享缓存（KV）：**键 = HMAC(规范化域名)**，不存明文            │
│  ④ 限速 + 全局预算（防单机跑飞，也防被刷）                           │
│  ⑤ 模型调用（可插拔：Workers AI / Jev / 其他）                        │
│  ⑥ 返回：verdict + 分数 + **模型原话** + model_id + questions_rev     │
└──────────────────────────────────────┬──────────────────────────────┘
                                       │
                          ┌────────────▼────────────┐
                          │ Workers AI / Jev / 其他  │
                          └─────────────────────────┘
```

**关键设计点**

1. **清洗两层同规范**：`canonical(host)` 的**规范与夹具只写一份**（Rust + JS 两实现，
   同一套测试向量）。App 先用它命中本地缓存；Worker 用**同一个函数**决定共享缓存键。
   两边不一致会产生两种病：本地命中不了（徒增请求）、或共享键分裂（缓存命中率虚低）。
2. **共享缓存键用 HMAC**：`key = HMAC_SHA256(secret, canonical_host)[0..16]`。
   好处：Worker 与被盗的 KV 导出**都无法枚举用户问过哪些域名**（与既有 incident
   collector 的加盐哈希同一思路）。代价：无法做「按域名查」的运维查询（可接受）。
3. **服务端版本必须回传**：响应里带 `model_id` + `questions_rev` + `canonical_rev`。
   App 把它们**并进本地缓存指纹**（既有指纹机制正好能接）→ 服务端改了提示词，
   本地缓存会自动作废，不需要发版，也不会复用旧模型的判决。
4. **模型可插拔**：Worker 内部对「Workers AI / Jev / 其他」用同一个接口；
   线上按**流量比例或灰度**切换，出问题一键切回。**不要把 Worker 和某个模型绑死。**
5. **App 侧仍然 fail-open + 本地预算 + 本地审计**：三件事都不上移（见 §3）。

---

## 3. 什么搬、什么**绝对不搬**

| 对象 | 决定 | 理由 |
| --- | --- | --- |
| 大模型调用 | **搬** | 客户端不再持 key；服务端可换模型/改提示词 |
| 域名规范化（eTLD+1、去端口…） | **两层都做**（同一规范） | 单放 CF ⇒ 每次多一跳；单放本地 ⇒ 共享键分裂 |
| 全局判决缓存 | **搬**（新建） | 热门域名全 user 判一次；这是最大的成本/延迟杠杆 |
| 本地判决缓存 | **留** | 零联网、离线可用、CF 挂掉仍能工作 |
| 预算 | **两边都有** | 本地闸防“客户端跑飞”；服务端闸防“被刷/总账失控” |
| 审计（模型原话/分数/为什么拦） | **留**（服务端只回结构化证据） | 界面要能申诉；黑盒不可接受。**服务端不得只返回一个布尔** |
| 写 Xray 路由 / 生成配置 | **留** | 这是 App 的本地职责与安全边界 |
| L2 流量形状、L3 MITM | **留** | 本地特征与本地 TLS 拆分 |
| `fail-open` 语义 | **留** | 网络/视频不可用时的兜底；上游挂了不能导致"拦一切" |
| 用户自选网关（openrouter/vercel/自带 key） | **留**（可选项） | 不剥夺用户用自己的 key 的自由；CF 路径只是**默认/推荐** |

---

## 4. 协议草案

```jsonc
// POST https://xraytun.top/api/intent
// Authorization: Bearer <app-intent-token>   ← App 持有；不是模型 key
{
  "canonical_rev": 1,                 // 清洗规范版本（两边必须一致）
  "domains": ["ads.example-cdn.com"], // 一次 ≤ 8 个；App 已按 eTLD+1 归并
  "context": {                        // 只放"首次出现时真的拿得到"的字段
    "first_seen_unix": 1790000000,
    "ports": [443],
    "network": "tcp",
    "inbound": "tun",
    "seen_count": 3
  },
  "want": ["endpoint_kind", "ads_intent", "risk_of_breakage"]
}

// 200
{
  "canonical_rev": 1,
  "model_id": "cf/@cf/meta/llama-… | jev-latest",
  "questions_rev": 7,                 // 服务端提示词版本 → App 并进本地指纹
  "source": "workers-ai | jev | shared-cache",
  "verdicts": [{
    "domain": "ads.example-cdn.com",
    "canonical": "example-cdn.com",   // 回传清洗结果，App 可复核
    "verdict": "block",               // block | allow | deferred
    "ads_intent": 0.93,
    "risk_of_breakage": 0.11,
    "category": "ad_or_monetization",
    "model_words": "…模型原话…",       // 必须回传：界面要能展示与申诉
    "evidence": { "cache": "hit|miss", "agreement": 3 }
  }]
}
```

**硬约束（照抄既有 §7.6 的教训）**
* 一次一个域名一个判定，**批内互相独立**；批只是省往返，不是让模型互相参考。
* 客户端**不得**因为响应缺字段而猜 —— 缺 `model_words` / 分数 ⇒ 按 `deferred` 处理并如实说。
* `questions_rev`/`model_id`/`canonical_rev` **必须**进本地指纹（否则复用旧模型判决）。
* 鉴权失败 / 404 / 5xx / 超时 ⇒ **fail-open**（放行 + requeue），与今天一致。

---

## 5. 缓存与隐私（这一步最容易做错）

**缓存分层**

| 层 | 键 | 存什么 | 命中效果 |
| --- | --- | --- | --- |
| App 本地 | 规范化域名（含 eTLD+1 继承） | verdict + 证据 + 指纹 | 零联网；离线可用 |
| CF 共享 KV | `HMAC(规范化域名)` | verdict + 分数 + 证据 + 模型/提示词版本 | 热门域名**全 user 一次** |
| （可选）AI Gateway 响应缓存 | 请求体指纹 | 原始响应 | 省一次模型调用 |

**隐私口径（必须写进 UI，不能只写文档）**
* 判决要么走**用户自带的网关**（今天的行为），要么走**本项目自己的 Worker**；
  两条路都要在界面上写清楚「哪些服务会看到你访问的域名」。
* Worker **不落明文域名**（HMAC 键）、**不落客户端 IP**（沿用 incident collector 的加盐哈希限流）、
  **不开请求日志观测**（或只留聚合指标）。
* 共享缓存意味着**判决是跨用户共用的**：必须在 UI/文档说明，
  并提供「只用自己的本地缓存 / 只用自带网关」的开关。
* ⚠️ **共享缓存的中毒面**：一条错判会波及所有用户。缓解：
  * 只有**多个源/多次判定一致**的结论才进共享缓存（沿用 0.9.1 的「多数一致」口径）；
  * 共享缓存条目带**吊销表**（维护者可一键撤销某个键）；
  * `Block` 类共享结论的 TTL 明显短于本地 `Block`（本地 90d，共享建议 ≤ 7d）。

---

## 6. 分阶段落地（每阶段独立有用、可回滚）

| 阶段 | 做什么 | 验收判据 |
| --- | --- | --- |
| **P0 · 清洗规范与度量**（不碰 CF） | 把 `canonical(host)` 规范与测试向量写成**两份实现（Rust/JS）共享的夹具**；用 0.9.1 新加的 `cache_hits/misses` 量出**今天的本地命中率** | 两份实现跑同一套夹具结果逐条相同；给出今日命中率基线 |
| **P1 · Worker 代理 + 共享缓存**（模型仍用 Jev） | 起 `xraytun-intent` Worker：鉴权、清洗、HMAC 共享 KV、限速、**代理到既有 Jev**、AI Gateway 观测；App 加 `intent_endpoint`（默认关，显式开启） | ① 同一域名第二个用户命中共享缓存 ② p95 拖尾延迟 ③ App 关掉 Worker → 行为与今天完全一致（fail-open 回归） ④ Worker 里 grep 不到明文域名 |
| **P2 · Workers AI 评测与灰度** | 用 `eval.rs` 的离线夹具对 Workers AI 候选模型跑精确率/每千连接误杀数；达标才灰度（比例可调、一键回退） | 达到 PRD §10 的门禁；不达标就**不切**，继续用 Jev |
| **P3 · 服务端提示词迭代** | 提示词/模型版本服务端可发；`questions_rev` 回传进本地指纹 | 改一句提示词 ⇒ 本地旧判决自动作废、**不需要发版**；服务端灰度可控 |
| **P4 ·（可选）反馈回路** | 用户申诉 → 吊销共享键 + 长期修正 | 申诉入口可用；吊销生效可复验 |

**回滚**：任一阶段出问题 ⇒ 关掉 App 的 `intent_endpoint`（回落到自选网关/纯本地），
Worker 侧可 `wrangler deployments rollback`。**默认关闭**，符合本项目「危险能力 opt-in」的习惯。

---

## 7. 风险与失败模式（先说清，再动手）

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| **模型质量下降** | 误杀上升（拦掉不是广告的站） | P2 离线评测门禁；不达标不切；灰度 + 一键回退 |
| **Workers AI 结构化输出不稳** | 解析失败 → 全部 deferred | 严格 schema 校验 + 失败即 `deferred`（既有 `SchemaInvalid` 路径）；提示词与解析都要有对拍测试 |
| **CF 不可达（大陆网络）** | 新域名判不了 | 本地缓存 + fail-open；`xraytun.top` 同 zone 路由（已验证可行的方式） |
| **共享缓存中毒** | 错判扩散到所有用户 | 多数一致才入共享库；短 TTL；吊销表；`Block` 尤其保守 |
| **隐私面变大**（运营方能看域名） | 信任问题 | 不落明文（HMAC）、不落 IP、不开日志、UI 显式披露 + 可选关闭 |
| **客户端被仿冒刷 Worker** | 账单与滥用 | App token + 服务端限速/预算/配额；token 可轮换 |
| **契约漂移（Rust/JS 清洗不一致）** | 缓存键分裂、命中率虚低 | P0 的共享夹具 + CI 双向对拍 |
| **把 fail-open 也搬上去** | 上游一挂就"拦一切" | **明确不搬**：fail-open 与本地预算永远在 App |

---

## 8. 已核实的 CF 事实（官方文档，2026-09 抓取）

### 8.1 Workers AI 定价与额度 【官方】

来源：[Workers AI Pricing](https://developers.cloudflare.com/workers-ai/platform/pricing/)（Last updated 2026-09-17）

* 计价单位是 **Neuron**：**$0.011 / 1,000 Neurons**。
* 免费额度 **10,000 Neurons/天**（Free 与 Paid 计划相同），**每天 00:00 UTC 重置**；
  超出免费额度的用量**需要 Workers Paid**（$5/月起）才能继续。
* ⚠️ **超出任一限额后，后续请求直接报错**（"further operations will fail with an error"）
  ⇒ Worker 必须把它当成一种**明确失败**处理（defer + 通告），不能当成"模型说不是广告"。
* ⚠️ 部分模型（`@cf/moonshotai/kimi-k2.6/2.7`、`@cf/zai-org/glm-5.2/5.3`、
  `@cf/deepseek-ai/deepseek-v4-*`）**必须绑付费**。
* 个别模型有 **cached input 折扣**（例：`deepseek-v4-flash` 缓存输入 $0.014/M）。

**本用例的成本估算**（假设每次判定 ≈ 1,000 输入 token + 80 输出 token：

| 候选模型 | 输入/输出 $per M | Neurons/次判定 | 每次判定 | **免费额度可支撑** |
| --- | --- | --- | --- | --- |
| `@cf/ibm-granite/granite-4.0-h-micro`（**不支持 JSON 模式**） | $0.017 / $0.112 | ≈ 2.4 | ≈ **$0.000026** | ≈ **4,250 次/天** |
| `@cf/meta/llama-3.1-8b-instruct`（**支持 JSON 模式**） | $0.282 / $0.827 | ≈ 31.6 | ≈ **$0.00035** | ≈ **316 次/天** |
| `@cf/qwen/qwen3-30b-a3b-fp8`（不支持 JSON 模式，但便宜） | $0.051 / $0.335 | ≈ 5.0 | ≈ **$0.000055** | ≈ **1,990 次/天** |
| `@cf/openai/gpt-oss-20b`（不支持 JSON 模式） | $0.200 / $0.300 | ≈ 20.5 | ≈ **$0.00023** | ≈ **490 次/天** |

**对照产品自身的预算**：App 默认 **≤200 次/天/台**（`settings.rs` 实测 `per_day = 200`）。
⇒ 用**便宜的 8B 级模型**时，**免费额度就够一台机器用**（316 > 200，granite 更是 4,250 > 200）。

⚠️ **但这是"一台机器"的口径**：10,000 Neurons/天是**账号级**的。
如果 Worker 服务 N 台机器，免费额度被 N 台共享（全局共享缓存能大幅降低每台的增量，
但**没有共享缓存之前**不能假设免费）。⇒ 这正是 §6 把「全局共享缓存」放在 P1 的原因。

### 8.2 结构化输出：JSON Mode 【官方】

来源：[Workers AI JSON Mode](https://developers.cloudflare.com/workers-ai/features/json-mode/)（Last updated 2026-09-14）

* 兼容 OpenAI 写法：请求里带 `response_format: { type: "json_schema", json_schema: {...} }`。
* **支持 JSON Mode 的模型是一张短名单**（截至抓取时）：
  `@cf/meta/llama-3.3-70b-instruct-fp8-fast`、`@cf/meta/llama-3.1-8b-instruct`、
  `@cf/meta/llama-3-8b-instruct`、`@cf/deepseek-ai/deepseek-r1-distill-qwen-32b`、
  `@hf/nousresearch/hermes-2-pro-mistral-7b`、`@hf/thebloke/deepseek-coder-6.7b-instruct-awq`。
  → **便宜又支持 JSON 的只有 `llama-3.1-8b-instruct` / `llama-3-8b-instruct`**。
* ⚠️ **官方明说不能保证满足给的 schema**：极端情况下会返回错误 `JSON Mode couldn't be met`，
  **必须处理**。JSON Mode 也不支持流式。
* ⇒ 设计含义：**必须有"schema 不合法 ⇒ deferred"的路径**（仓库里已有
  `Verdict::Deferred(SchemaInvalid)` + 10 分钟 TTL，正好接上），
  并且要把「便宜模型 + 提示词约束 JSON + 本地严格解析」作为**不用 JSON Mode 的备选**。

### 8.3 数据与隐私 【官方】

来源：[Workers AI Data usage](https://developers.cloudflare.com/workers-ai/platform/data-usage/)（Last updated 2026-04-21）

原文关键句（逐条）：

* **不训练**：Cloudflare does not use your Customer Content to train any AI models made available
  on Workers AI or improve any Cloudflare or third-party services, and would not do so unless
  it received your explicit consent.
* **不跨客户**：Cloudflare does not make your Customer Content available to any other Cloudflare customer.
* 内容归你所有；**仅当你同时使用 R2/KV/DO/Vectorize 等存储**时，内容才可能被存储。

⇒ 对本方案的含义：**这比"把域名发给一个来路不明的第三方 LLM 网关"在条款上更清楚、更可控**。
但注意两点：
1. **"不训练"不等于"看不见"** —— 域名在 Worker 内存里必然可见；
   **只有"不落明文"（HMAC 键）与"不开请求日志"能把可见面缩到最小**。
2. 我们**主动使用 KV 存判决** ⇒ 按官方口径这属于"你选择存储"，**必须自己负责内容**
   （所以 §5 要求 KV 里只放 HMAC 键 + verdict，不放原始域名）。

### 8.4 AI Gateway 缓存与观测 【官方】

来源：[AI Gateway caching](https://developers.cloudflare.com/ai-gateway/features/caching/)（Last updated 2026-08-27）

* 默认**关闭**；可按请求用 `cf-aig-cache-ttl` / `cf-aig-cache-key` / `cf-aig-skip-cache` 控制。
* 默认缓存键 = provider + endpoint + model + **认证头** + **完整请求体**（SHA-256）⇒ 默认是**精确匹配**；
  可用 `cf-aig-cache-key` 换成自定义键。
* **TTL 范围：最小 60 秒，最大 1 个月**；响应头 `cf-aig-cache-status: HIT|MISS`。
* ⚠️ **缓存是易失的**：两个相同请求同时到达时，第二个可能仍打回源（**没有单飞**）。
* ⚠️ **还没有语义缓存**（官方说计划中）。

⇒ **关键结论（决定架构）**：本项目的 `Block` TTL 是 **90 天**、`Allow` 是 **30 天**，
**超过 AI Gateway 缓存 1 个月的上限** ⇒
**判决缓存必须自己用 KV 实现，不能靠 AI Gateway 缓存。**
AI Gateway 的正确用途是**观测 / 限速 / 多供应商 fallback / 统一计费**，
以及"完全相同的请求"这一层的顺带缓存。

### 8.5 仍待核实

* Workers / KV / D1 / Durable Objects 的免费额度与 CPU 时间上限（本方案用 KV，量很小）。
* **中国大陆访问 `workers.dev` vs 自定义 zone 路由的官方口径**：官方文档没有查到明确说法；
  本项目既有经验是「`*.workers.dev` 大陆经常不可达、同 zone 路由可达」
  （`infra/incident-collector/wrangler.toml` 头部实测记录）→ **沿用同 zone 路由**。
* 「今天每台机器每天新增域名数」的真实分布 ⇒ **P0 必须先量**，它决定共享缓存能省多少。

---

## 9. 结论：会更好吗？

**会，但要按条件成立才叫"更好"。** 拆成三个独立判断：

| 判断 | 结论 | 依据 |
| --- | --- | --- |
| 判决层搬到 CF Worker | **是，收益最大** | ① App 不再持模型 key ② **全局共享缓存**（热门域名全 user 一次）③ 提示词/模型**服务端可迭代**（今天改一句要升 `QUESTIONS_REVISION` ⇒ 整库作废 + 发版）④ AI Gateway 给观测/限速/fallback |
| 域名清洗 | **是，但两层都做** | 单放 CF ⇒ 每次多一跳；单放本地 ⇒ 共享键分裂。规范与夹具只有一份 |
| 模型换 Workers AI | **先评测** | JSON Mode 只有短名单且**不保证 schema**；开放模型在"广告意图"上的校准未知。仓库已有离线评测夹具 ⇒ 过门禁才切 |
| 成本 | **现在这一档基本免费** | 便宜 8B 模型免费额度 ≈316 次/天 > App 默认 200 次/天/台；但**账号级**共享，多机要算总账 |
| 隐私 | **条款上更好，但责任转移** | CF 明文承诺**不训练、不跨客户**；同时"你用了 KV"意味着**你要负责内容** ⇒ 只存 HMAC 键 |

**什么时候不该做**：如果目标是"零运营负担"，那么自建 Worker = 多一个要维护、
要监控、要防刷、要发版的线上服务。**只有在下面三件事都认账时才值得**：
1. 愿意维护一个线上判决服务（含密钥轮换、限速、回滚）；
2. 接受"共享缓存"带来的跨用户影响（错判会扩散，需要吊销与短 TTL）；
3. 愿意先花时间跑模型评测，而不是直接信 Workers AI。

**最小可行版本（如果只做一件事）**：先做 **P0 + P1 的"代理模式"**
（Worker 只做清洗 + 共享 KV 缓存 + 代理到**现有 Jev**），**先不换模型**。
这一步就能拿到 ①无 key ②全局命中率 ③服务端迭代 ④可观测，
而把"换模型"这个高风险决定留到评测之后。

