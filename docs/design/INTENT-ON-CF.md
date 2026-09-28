# 意图过滤搬到 Cloudflare：方案与评估（0.9.2 提案）

> 作者：Lead · 状态：**提案，未实施**
> 触发：用户提议「把域名清洗放到 CF 上，用 Workers AI / 大模型直接去做」
> 本文回答两件事：**怎么设计**、**是不是更好**（含"不好"的部分）。

---

## 0. 一页结论

| 问题 | 结论 |
| --- | --- |
| 判决（问大模型）该不该搬到 CF？ | **方向正确**，但收益大小**取决于一件还没量的事**：每台机器每天新增多少域名（§8.5）。可确定的三条收益是：**默认路径不再需要用户 key**（fallback 仍要 key，见下）、服务端可迭代提示词/模型、AI Gateway 可观测 |
| 域名清洗该不该放 CF？ | **该做，但清洗不能是第一跳**。清洗要**两层都做、同一套规范与夹具**：App 本地先清洗 → 命中本地缓存就**零联网**；没命中才带规范化域名去 CF |
| 该不该换成 Workers AI？ | **先评测再决定**。仓库已有离线评测夹具（`crates/xt-intent/src/eval.rs` + PRD P1.5 门禁）。Workers AI 的开放模型在这类「广告/追踪意图」判断上的**校准质量未知**，没跑过评测就切 = 拿用户的误杀率做实验 |
| 会更好吗？ | **可能更好，但不是在今天**。三个前提：① 本地缓存/预算/fail-open 必须保留；② 共享缓存**不落明文域名**；③ 换模型必须过离线评测；④ 服务端版本必须回传进本地指纹。⚠️ **在 P0 量出新域名分布之前，保持现状（App 直连 + 本地缓存）是更稳的默认** |
| 现在这一档免费吗？ | **单台机器**基本免费（≈316 次/天的额度 > 默认 200 次/天/台）；⚠️ **但 10,000 Neurons/天是账号级** —— 按默认预算，**第 2 台机器就超**（见 §8.1 的算式）。共享缓存能省多少 = 未测 |

**一句话**：把「判决层」搬到 CF 是**架构上正确**的一步（共享缓存 + 服务端迭代 + 默认路径无 key），
把「模型」换成 Workers AI 是**另一个独立决定**，必须被评测门禁卡住，
而「域名清洗」是两层的共同底座、不是某一层的专属职责。
**顺序**：先做 P0（清洗规范 + 量出今日命中率与新域名分布）→ 再决定 P1 是否立项。

> ⚠️ **两个被独立评审纠正的说法（原稿写错过，留痕）**：
> 1. 原稿写「客户端**不再存 key**」——只要保留"用户自选网关"，CF 挂掉时它就是唯一路径，
>    **App 必须继续持 key 与网关调用**。正确说法是「**默认路径**不再需要用户 key」。
> 2. 原稿把「共享缓存」称为**最大的一块收益** —— 那依赖一个尚未测量的分布，属**结论先于证据**。

---

## 1. 现状事实（可复核）

| 事实 | 位置 |
| --- | --- |
| 四层漏斗：L0 静态名单（0 成本）/ **L1 域名意图（大模型）** / L2 流量形状（本地加权）/ L3 MITM（opt-in） | `docs/design/INTENT-FILTER.md` §2 |
| **计费单位 = 一条新域名**，不是一条连接；同域名第 2..n 条连接零成本 | 同上 §7.3 |
| 判决缓存：`domain → Verdict + 证据 + 过期时间`，落盘原子写 + 版本号；Allow 30d / Block 90d / Deferred 1h | `crates/xt-intent/src/cache.rs`、`engine.rs:32-35`（`:32 Block 90d` / `:33 Allow 30d` / `:35 Deferred 1h`） |
| 缓存指纹 = 模型 id + 网关 baseURL + **问题措辞版本** + 阈值 → 任一变化**整库作废** | `cache.rs:40`、`engine.rs:90` |
| 提示词写死在客户端（`question.rs`），改一句 ⇒ `QUESTIONS_REVISION +1` ⇒ **整库作废 + 必须发版** | `cache.rs:25`（`pub const QUESTIONS_REVISION`）、`question.rs` |
| 网关**可插拔**：typesafe(Jev) / zen(免key) / openrouter / vercel / custom | 预设清单在 `apps/ui/src/types.ts:531`（`IntentPreset`）与 `crates/xt-core/src/model.rs:740-763`（各预设 base URL）；UI 文案 `apps/ui/src/pages/Intent.tsx:82-83`；协议见 `INTENT-FILTER.md` §7.6。⚠️ `gateway.rs` 里只有 HTTP 客户端，**没有**预设表 |
| 预算：每小时/每天上限（默认 `per_minute: 10`、`per_day: 200`），超额只放行 + 审计记 `budget_exhausted` | 默认值在 `crates/xt-intent/src/engine.rs:77-78`；配置映射 `settings.rs:60`；`Budget` 实现在 `budget.rs` |
| 审计：每次判决写一行 JSONL，界面要能展示**模型的原话与分数**（不许黑盒） | `INTENT-FILTER.md` §7.4 |
| 失败一律 **fail-open**（网关错 → 放行 + requeue），缓存坏 → 当空库 | fail-open 在 `engine.rs:11`（模块文档「fail-open 是默认路径」）+ `:331/:389-390/:405-406/:435-436`（`observer.requeue`）+ `:442`（`Deferred(GatewayUnavailable)`）；缓存坏=空库在 `cache.rs`。⚠️ `engine.rs:356-358` **不是** fail-open，那是 `classify_pending` 的 cache lookup |
| **刚加的能力**（0.9.1）：父域（eTLD+1）继承、`cache_misses`/`cache_inherited` 计数、网关失败 60s 冷却 | `rules.rs:246`（`normalize`）、`rules.rs:278+`（`registrable_domain`）、`cache.rs`（`INHERIT_TTL_CAP_SECS`=7 天、`inherited_from`）、`engine.rs`（`cache_misses`/`cache_inherited`/`cooldown_skipped`） |
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
  超出免费额度的用量**需要升级到 Workers Paid 计划**才能继续。
  （⚠️ 原稿这里写了"$5/月起" —— **该数字不在 pricing 页上**，已删；引用价格需另给出处。）
* ⚠️ **超出任一限额后，后续请求直接报错**（"further operations will fail with an error"）
  ⇒ Worker 必须把它当成一种**明确失败**处理（defer + 通告），不能当成"模型说不是广告"。
* ⚠️ 部分模型**必须绑付费**，官方 pricing 页的完整清单是：
  `@cf/moonshotai/kimi-k2.6`、`@cf/moonshotai/kimi-k2.7-code`、`@cf/zai-org/glm-5.2`、
  `@cf/zai-org/glm-5.3`、`@cf/zai-org/glm-5.3-flash`、`@cf/deepseek-ai/deepseek-v4-flash-0731`、
  `@cf/deepseek-ai/deepseek-v4-pro-0813`。
  （⚠️ 原稿简写成"kimi-k2.6/2.7" —— **不存在 `kimi-k2.7`**，且漏了 `glm-5.3-flash`，已按原文改全。）
* 个别模型有 **cached input 折扣**（例：`deepseek-v4-flash` 缓存输入 $0.014/M）。

### 8.1a Neuron 是什么（先定义，否则下面的表看不懂）

官方定义（[Workers AI Pricing](https://developers.cloudflare.com/workers-ai/platform/pricing/) 原文）：
**Neurons are our way of measuring AI outputs across different models, representing the GPU
compute needed to perform your request.** 定价页同时给 `Price in Tokens` 与 `Price in Neurons`
两列，并写明**两列等价** —— 也就是说它是"按 token 折算、以 GPU 算力为单位计费"的中间量。

三条对本方案有直接影响的推论：

1. **免费额度以 Neuron 计**（10,000 Neurons/天，账号级）⇒ "免费能跑多少次"**必须按具体模型算**。
2. **换算是逐模型的**：官方没有"1 token = N neurons"的通用公式，只在表里逐模型列
   ⇒ **比价必须看 Neurons 列**，只看 token 单价会选错。
3. **输出比输入贵得多**（生成是逐 token 串行的）：例 `llama-3.1-8b-instruct` =
   25,608 neurons/M 输入 vs **75,147 neurons/M 输出**（2.9×）。
   ⇒ **我们的输出是结构化 JSON，越短越省**：1,000 in + 80 out 里，那 80 个输出 token 占 19% 的费用；
   若把"模型原话/解释"也塞进 JSON、输出涨到 300 token，输出占比升到 47%。
   （但"模型原话"是**必须回传**的审计字段 —— 这是"省钱"与"可申诉"的取舍，别为了省钱把它删掉。）

⚠️ **诚实的边界**：官方只说 Neuron "represent the GPU compute needed"，
**没有公布**"1 Neuron = 多少 FLOPs / GPU 秒"这类物理换算 ⇒ 它对我们是一个**黑箱计费单位**，
**能用来比价与算额度，不能用来推导这个模型实际跑了多久 GPU**。
本文件引用的所有 Neuron 数字都直接取自官方表格，没有自行换算。

---

**本用例的成本估算**（假设每次判定 ≈ 1,000 输入 token + 80 输出 token；
Neurons/次 = 1000×in_per_M/1e6 + 80×out_per_M/1e6；每次 $ = Neurons × $0.011/1000
⇒ **Neurons/次 与「每次 $」是同一个数除以 1000**，下面两列互为校验）：

| 候选模型 | 输入/输出 $per M | Neurons/次判定 | 每次判定 | **免费额度可支撑** |
| --- | --- | --- | --- | --- |
| `@cf/ibm-granite/granite-4.0-h-micro`（**不支持 JSON 模式**） | $0.017 / $0.112 | ≈ **2.35** | ≈ **$0.000026** | ≈ **4,250 次/天** |
| `@cf/meta/llama-3.1-8b-instruct`（**支持 JSON 模式**） | $0.282 / $0.827 | ≈ **31.6** | ≈ **$0.00035** | ≈ **316 次/天** |
| `@cf/qwen/qwen3-30b-a3b-fp8`（不支持 JSON 模式，但便宜） | $0.051 / $0.335 | ≈ **7.06** | ≈ **$0.000078** | ≈ **1,416 次/天** |
| `@cf/openai/gpt-oss-20b`（不支持 JSON 模式） | $0.200 / $0.300 | ≈ **20.4** | ≈ **$0.00022** | ≈ **491 次/天** |
| `@cf/meta/llama-3.1-8b-instruct-fp8-fast`（**不支持 JSON**，同族更便宜） | $0.045 / $0.384 | ≈ **6.91** | ≈ **$0.000076** | ≈ **1,447 次/天** |
| `@cf/meta/llama-3.2-1b-instruct`（**不支持 JSON**，全表最便宜） | $0.027 / $0.201 | ≈ **3.92** | ≈ **$0.000043** | ≈ **2,553 次/天** |

> ⚠️ **本表第一版我把 `qwen3-30b-a3b-fp8` 那一行算错了**（写成 5.0 neurons / $0.000055 / 1,990 次，
> 实际是 7.06 / $0.000078 / 1,416 —— 输出 token 的神经元没算进去）。已按官方表逐格重算修正。
> 复核方式：`Neurons/次 == 每次判定$(×1000)`，两列对不上就是抄错了。

**对照产品自身的预算**：App 默认 **≤200 次/天/台**（`settings.rs` 实测 `per_day = 200`）。
⇒ 用**便宜的 8B 级模型**时，**免费额度就够一台机器用**（316 > 200，granite 更是 4,250 > 200）。

⚠️ **但这是"一台机器"的口径**：10,000 Neurons/天是**账号级**的。
按默认预算 200 次/天/台 与 **JSON 可用的 8B（31.62 neurons/次）** 算：

```
单台：200 × 31.62 = 6,324 neurons/天   → 免费额度内（余 37%）
两台：12,648                            → 超 26%
五台：31,620                            → 超 216%
可支撑台数（无共享缓存） ≈ 10,000 / (200 × 31.62) ≈ 1.58 台
```

⇒ **第 2 台机器就超免费额度**。换便宜模型或改走"提示词约束 JSON"能把这条线推后
（granite 2.35 neurons ⇒ ≈21 台；llama-3.2-1b 3.92 ⇒ ≈12 台），
而**共享缓存能省多少取决于一个尚未测量的分布**（§8.5）。
这正是 §6 把「全局共享缓存」放在 P1 的原因，也是 §9 不再写"基本免费"的原因。

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
* ⚠️ 两个限定（独立评审指出）：
  1. 「便宜又支持 JSON 的只有 8B」**仅限"有公布定价的"模型** —— 名单里的
     `@hf/nousresearch/hermes-2-pro-mistral-7b` 与 `@hf/thebloke/deepseek-coder-6.7b-instruct-awq`
     在当前 pricing 表里**查不到价**（标未验证）。
  2. **JSON Mode 不是唯一路径**：走"提示词约束 JSON"时，同族
     `llama-3.1-8b-instruct-fp8-fast` 只要 **6.91 neurons**（是 JSON 版 8B 的 1/4.6）。
     ⇒ P2 评测应**同时评两条路**（JSON Mode vs 提示词约束），
     否则会变成"要么贵 4.6 倍、要么换个陌生模型"的伪二选一。

### 8.3 数据与隐私 【官方】

来源：[Workers AI Data usage](https://developers.cloudflare.com/workers-ai/platform/data-usage/)（Last updated 2026-04-21）

原文关键句（逐条；⚠️ 下面前两句是**要点转述**，逐字原文见上方链接——独立评审指出原稿把转述标成了「原文」）：

* **不训练**（官方分 (1) 训练模型 / (2) 改进服务 两条）：Cloudflare does not use your Customer Content
  to (1) train any AI models made available on Workers AI or (2) improve any Cloudflare or third-party
  services, and would not do so unless **we** received your explicit consent.
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

### 8.4b 速率限制（官方 Limits 页，Lead 自查）

来源：[Workers AI Limits](https://developers.cloudflare.com/workers-ai/platform/limits/)（2026-09-17）

* **Text Generation：300 请求/分钟**（除非该模型要求 Workers Paid）。
* 要求付费的模型：**标准计费 20 RPM / 预付费 AI Gateway credits 50 RPM**。
* 超出任何限额后「further operations will fail with an error」⇒ **既是天然熔断，也是全站降级**
  （Worker 必须把它当明确失败：defer + 上报，不能当成"模型说不是广告"）。
* 对我们的规模：默认 200 次/天/台 ⇒ 单机远低于 300 RPM，**瓶颈是免费 Neuron 额度**（§8.1）。

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
| 成本 | ⚠️ **只能说"单台机器这一档基本免费"，不能说"基本免费"** | 官方额度是**账号级** 10,000 Neurons/天；按默认 200 次/天/台 + JSON 可用的 8B（31.62 neurons/次）⇒ **第 2 台就超**（§8.1 算式）。可支撑台数 ≈ `316 / N`（N=每台每天次数÷200）。**共享缓存能省多少 = 未测（P0）** |
| 隐私 | **条款上更好，但责任转移** | CF 明文承诺**不训练、不跨客户**；同时"你用了 KV"意味着**你要负责内容** ⇒ 只存 HMAC 键 |

**什么时候不该做**：如果目标是"零运营负担"，那么自建 Worker = 多一个要维护、
要监控、要防刷、要发版的线上服务。**只有在下面三件事都认账时才值得**：
1. 愿意维护一个线上判决服务（含密钥轮换、限速、回滚）；
2. 接受"共享缓存"带来的跨用户影响（错判会扩散，需要吊销与短 TTL）；
3. 愿意先花时间跑模型评测，而不是直接信 Workers AI。

**最小可行版本（如果只做一件事）**：先做 **P0 + P1 的"代理模式"**
（Worker 只做清洗 + 共享 KV 缓存 + 代理到**现有 Jev**），**先不换模型**。
这一步就能拿到 ①**默认路径**无 key（fallback 仍要 key）②全局命中率 ③服务端迭代 ④可观测，
而把"换模型"这个高风险决定留到评测之后。

### 9.1 独立评审的校准（**本稿因此被下调**）

`docs/verification/INTENT-ON-CF-REVIEW.md` 对我这份方案做了对抗性评审，结论是
**方向正确但整体偏乐观**，并给出三条必须收紧的地方（我已按它改了 §0/§8.1/§8.2/§9）：

| 编号 | 评审指出的问题 | 我的处理 |
| --- | --- | --- |
| S1 | §9「成本基本免费」与 §8.1 自己的「账号级」警告矛盾（第 2 台就超） | §9 成本行改成条件句 `316/N`；§8.1 补了台数算式 |
| S2 | 「客户端不再持 key」**不成立** —— 保留自选网关 fallback ⇒ App 仍要处理 key | §0 明确改成「**默认路径**不再需要用户 key；fallback 仍保留 key 处理」 |
| S3 | §1 的 fail-open 代码引用**是错的**（`:356-358` 其实是 cache lookup） | 已改引 `engine.rs:11` + `:331/:389/:405/:435` + `:442` |

评审还纠正了 4 处代码引用（`cache.rs:26→:25`；网关预设不在 `gateway.rs` 而在
`types.ts:531`/`xt-core/model.rs:740-763`；`per_day` 默认在 `engine.rs:78`；TTL 精确区间 `:32-35`），
以及 2 处引文/清单错误（付费模型清单漏 `glm-5.3-flash`、误写 `kimi-k2.7`；pricing 页没有"$5/月"）。
**这些都已在上面的章节里修正。**

评审的**一句话版**（我认同）：**先做 P0，P1 立项以「P0 测出的新域名分布 + p95 延迟 +
一个可验证的吊销机制」为前提；在 P0 出数之前，保持现状（App 直连 + 本地缓存）是更稳的默认。**

---

## 10. 反方论证与漏掉的风险（来自独立评审，摘要）

完整版见 `docs/verification/INTENT-ON-CF-REVIEW.md`。这里只留最该记住的：

**反方（为什么"不搬"可能是对的）**

1. **新增一个线上服务的真实代价**：既有 `infra/incident-collector` 已经 **10 个文件 / 2048 行**
   （Worker + R2 + 隐私文档 + 加盐哈希限流 + 扫描 + 测试 + 部署/冒烟/校验脚本）。
   判决 Worker 只会更大，而且**因为要保留自选网关 fallback，"无 key"只对默认路径成立** ——
   复杂度是**新增**一套而不是**替换**一套。
2. **爆炸半径**：单机缓存错了只影响一台；共享缓存错了影响所有用户。
3. **隐私是转移而不是消失**：HMAC 只挡住"事后枚举"，**挡不住运营方在流量上做模式分析**
   （某段时间哪些键被问过、频率多高）。
4. **可用性**：CF 路径挂掉时，fallback 必须是"完全不需要运营方在场"的本地路径。
5. **自由**：默认路径侵蚀用户自选网关的自由（默认值比选项更有力量）。

**评审补的漏项（本稿原本没写）**

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| Workers AI 速率限制（官方：Text Generation 300 RPM；付费模型 20/50 RPM） | 高峰被限流 → 全部 defer | 本地缓存 + 预算 + 退避；把它当明确失败而非"不是广告" |
| 超额后**直接报错** | 全站降级（不是慢慢变慢） | 同上；并把 Neuron 用量做成可观测告警 |
| Worker 冷启动 / 首字节延迟 | 首次判决变慢 | 预热 + 本地缓存兜底；P1 要量 p95 |
| KV 最终一致性 | 刚写的判决可能读不到（重复花钱） | 进程内单飞 + 接受偶发重复；**官方一致性口径未核实** |
| 域名 = 浏览画像 | 集中到一处更敏感 | 只存 HMAC、不落 IP、不开请求日志、显式同意 |
| 被刷导致账单 | 直接花钱 | App token + 服务端限速/配额 + 额度告警 |
| 提示注入（域名里塞指令） | 模型判错 | 域名**只作为数据**、不拼进指令；严格 schema + 本地校验 |
| `xraytun.top` 被墙 | 判决不可用 | 本地缓存 + fail-open；不把拦截能力绑在这条路上 |
| 集中判决的合规含义 | 成为"数据处理者" | 写清隐私政策；只存必要字段；可关可退 |

**评审结论（我接受）**：这份方案**方向对、顺序对**（先 P0、先评测、本地兜底不上移），
但**不能拿"免费/共享缓存收益最大"当立项理由** —— 那两件事都还没量。


---

## 11. 附：Vectorize（向量数据库）在本方案里有用吗？

**结论：本轮不用。不是因为它贵 —— 恰恰相反，它几乎免费 —— 而是因为它是「找相似」的工具，
而我们的判决缓存是「是不是同一个域」的精确问题。用相似性共享判决，正是会把好域名误拦的地方。**

### 11.1 它是干什么的

官方定位：为**语义相似检索**而生 —— RAG、语义搜索、推荐、按内容聚类
（[What is a vector database](https://developers.cloudflare.com/vectorize/reference/what-is-a-vector-database/)）。
它的单位是**文档 / 文本块**，不是主机名。

计费与额度（[官方 Pricing](https://developers.cloudflare.com/vectorize/platform/pricing/)，GA）：

- 按 **queried vector dimensions** 与 **stored vector dimensions** 计费，不按 CPU / 索引数计费。
- 官方公式：`(queried vectors + stored vectors) × dimensions`（**注意**：stored vectors 会被计入 queried
  dimensions 这一项 —— 我按官方公式原样引用，不为它做超出文档的解释）。
- Workers **Free** 含 **30M queried dims/月 + 5M stored dims/月**；上限：1 个索引最多 20,000,000 条向量、
  1536 维、单条 metadata 10 KiB（[官方 Limits](https://developers.cloudflare.com/vectorize/platform/limits/)）。

按这个规模算我们的账（假设 5,000 个域 × 768 维、每天 200 次查询）：
stored = 3.84M ≤ 5M ✓；queried = `(6000 + 5000) × 768` ≈ 8.4M ≤ 30M ✓。
**⇒ 落在 Free 额度内，钱不是反对理由。**

### 11.2 为什么它不适合判决缓存（五条，按重要性排序）

1. **键是精确的，不是相似的。** `canonical(host)` 是确定性函数，精确查表 O(1)、**可解释、可单测**。
   向量检索返回"最像的 K 个"，你还得自己定阈值 —— 等于引入一个**会出错的判断**，而错误的形式是"拦了一个从没被判过的域"。
2. **相似 ≠ 同一实体，而这里恰好是对抗者能利用的地方。**
   `google-analytics.com` / `google-analitics.com` / `google-analytics.cn` 在向量空间里彼此很近，
   但一个是统计服务、另一个可能是攻击者注册的。**用 A 的判决覆盖"像 A"的域 = 把一次错误放大成一片。**
   我们的政策是 fail-open、宁可 Deferred 也不给错判决，这条与"用相似性共享判决"直接冲突。
3. **主机名没有语义。** bge 这类模型在 1–20 个字符的纯 ASCII 主机名上学到的主要是**子词统计噪声**，
   不是含义：`ads.doubleclick.net` 与 `analytics.example.com` 不会因为"都是广告"而在语义空间靠近。
   向量的价值来自**自然语言内容**，而主机名不是内容。
4. **我们的"相似"需求已被确定性手段解决。** `canonical()` + 父域继承
   （`crates/xt-intent/src/cache.rs`）是一条**可解释的树**：能对用户说"因为它的父域 `example.com` 已经判过"。
   向量检索给不出这种解释，而「为什么拦我」是这个产品**必须**能回答的问题（审计 JSONL 的初衷）。
5. **若为了它引入内容 embedding，代价比钱大得多。** 那需要把网页 HTML/正文送出设备 ——
   直接摧毁 §5 与产品文案里刚承诺的隐私口径（**只出一个 canonical 域的 HMAC**）。
   为一栏"可能更准"的猜测，把隐私基线从"一个域名"改成"网页内容"，不划算。

补充：CF 并没有给 AI Gateway 提供语义缓存（§8.4 已核实其缓存是**精确匹配**、TTL 最长 1 个月）。
所以"用 Vectorize 给判决做语义缓存"这件事，是要**我们自己拿它搭一个相似性缓存** ——
也就是上面第 2 条那个危险动作，而不是"打开一个官方开关"。

### 11.3 它真正可能有用的地方（都不在在线判定路径上）

1. **离线聚类审计语料 —— 有价值，可考虑。** 把 `audit.jsonl` 里**模型的 reason 文本**（不是域名）聚类，
   找出"这个月 400 个新域其实都是同一类我们没写规则的东西"，然后**写进 L0 静态规则**：
   把逐域 LLM 调用变成永久零成本规则 —— **直接减少 Neuron 消耗与隐私暴露**。
   这是批处理、跑在你自己机器上（本地 embedding + SQLite 即可），**不需要给运行时加一个向量库依赖**。
2. **内容侧（若有一天 L3 从"域名"升级到"页面意图"）**：那时 embedding 才有意义。
   但那是一次隐私与架构的**重新决定**，不属于本轮（§3 已明确 L2/L3 不搬）。
3. **需要"像不像"信号时**（typo-squat / DGA / 抢注）：用**本地确定性字符串相似度**
   （n-gram Jaccard / 编辑距离）比向量更好 —— 可解释、可单测、不出网、Rust 里几十行就能写、
   且能直接进 L0 且**免费**。

### 11.4 一句话答复

向量数据库是"帮我找出最像的**文档**"的工具；我们的判决缓存问的是"这是不是同一个**域**"。
**相似性共享判决会把好域名误拦**，所以：判决缓存继续用精确 KV（本地 + 可选共享，键 = HMAC(canonical domain)）；
真要用向量，就用在**离线**审计语料聚类上 —— 那是帮你**少花钱、少暴露**，而不是改变在线判定。

### 11.5 「离线审计」具体指什么

一句话：**把设备上已经落盘的记录拿下来，在「不联网、不改变运行行为」的前提下做统计与归纳，
用结论去改规则 / 改阈值 / 换模型 —— 而不是让在线路径即兴决定。**

它不是新概念，也不全是待办：**一半已经建好了。**

**已经存在的（可复核）**

1. **离线评测 CLI**：`crates/xt-intent/examples/eval_domains.rs` + `crates/xt-intent/src/eval.rs`
   - 输入本机 **Xray 连接日志**（`app.jsonl`），先算 **L0 静态名单的覆盖率**（不联网、不问模型）；
   - 加 `--live` 才对**有界样本**（`--limit`）真问网关，产出 `precision` / `recall` /
     `false_positives_per_1000_connections` 与一个 `Gate` 结论，另有 `render_diagnosis`
     专门回答"模型为什么没给出可用答案"（配额？schema？还是判不动？）；
   - 隐私上有硬约束：报告只有聚合数字，域名列表必须 `--dump-unknown <路径>` **显式**导出，
     且文件头就写着"应该导到仓库之外 —— 那里面是这台机器的浏览记录"。
2. **判决审计 `intent-audit.jsonl`**：每条判决一行结构化记录 —— `ts_unix`/`host`/`outcome`/`reason`/
   `category`/三个概率/`effective_min`/`applied`(是否真生效)/`cache_hit`/`model`/`usage`，
   `context_sent` **默认连 key 都不写**。今天就有一个消费者：界面 `intent_audit` 取最近 200 条。

**两张数据源回答的是不同问题**

| 数据源 | 回答什么 | 今天有工具吗 |
| --- | --- | --- |
| Xray 连接日志 `app.jsonl` | 我们**根本没看过**哪些域？L0 覆盖率多少？误拦率每千次连接多少？ | ✅ `eval_domains` |
| 判决审计 `intent-audit.jsonl` | 钱花在**哪个域/哪个模型**上？哪些判决**从未生效**？缓存命中率？阈值是否偏保守？ | ❌ 只有界面看最近 200 条 |

**值得做的离线分析（按省钱/省风险排序）**

1. **L0 规则归纳**（最省钱）：筛出高频、同 `category`、`outcome=block` 的域 → 固化为静态规则
   ⇒ 以后这些域**不再问模型**，直接省 Neuron、也少一次外发。
2. **阈值校准**：`risk_of_breakage` 高但仍被 `block`、或用户随后手动放行的记录 → 用真实数据调阈值，
   而不是拍脑袋。这正是 `eval_domains` 里 `--ads-min` 那个旋钮要回答的问题。
3. **成本账**：把 `usage` 汇总成"每个新域多少次调用、多少 token、折多少 Neuron"，以及
   **新域名/天的分布**（均值 + p95）—— 这恰好就是 §6 P0 要量、也是"上不上 CF"唯一能立项的两个数。
4. **`applied=false` 的占比**：演练模式下判决不生效，离线能算出"如果真按判决执行会拦掉多少"，
   这是上线前的安全阀。
5. **reason 文本聚类**（§11.3 第 1 条）：发现"一大堆新域其实是同一类我们没写规则的东西"。

**为什么必须是离线**

- 在线路径只该做**最少**的判断 —— 每加一个在线启发式，就多一个会误拦用户的现场决策；
- 而"规则该怎么改"是**应该有数据支撑的决策**，不是在线即兴；
- 离线天然满足隐私口径：**数据不出设备**，不需要给运行时加任何新依赖（第 5 条聚类在本地跑 embedding 即可）；
- 结论可复核：同一份语料重跑应得到同一份报告（`eval_domains` 就是按这个标准写的）。

**今天缺什么（以及缺得不多）**

缺一个**读 `intent-audit.jsonl` 的离线聚合**（上表第 2 行）：现在只有界面看最近 200 条，
没有按域/按模型/按 category 的汇总，也没有规则归纳。
工作量小 —— 一个 CLI（可挂在 `eval_domains` 旁边）+ 一批 `node --test` / Rust 单测风格的断言，
**不联网、不加服务、不进在线路径**。想上 CF 之前先把这个跑通，比先写 Worker 更划算。
