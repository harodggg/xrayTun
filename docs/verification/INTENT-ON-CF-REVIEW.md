# 对抗性评审：`docs/design/INTENT-ON-CF.md`（意图过滤搬到 Cloudflare）

> 评审者：verifier · 任务：task-15 · 被评对象：`docs/design/INTENT-ON-CF.md` @ `2aacf57`（含 Lead 自纠的 qwen 行）
> 只读评审。**我没有 CF 账号、没部署过 Worker、没跑过 Workers AI 推理** ⇒ 凡属"实际行为"的结论一律标 **未验证**。
> 每条结论给 命令/URL + 原始片段。官方页抓取时间：2026-09-28（页面标注 Last updated 见各节）。

---

## 0. 最严重的 3 条 + 最小修正

| # | 问题 | 严重度 | 最小修正 |
| --- | --- | --- | --- |
| **S1** | **§9「成本：现在这一档基本免费」与 §8.1 自己的"账号级"警告矛盾**：10,000 Neurons/天是**账号级**；App 默认 200 次/天/台、36B 级 JSON 模型 31.62 neurons/次 ⇒ 免费额度一天只够 **≈316 次判定**，**第 2 台机器就会超**（316/200≈1.6 台）。而"共享缓存能把每台增量降下来"恰恰是**尚未测量**的 P0 结论。§9 把"未测量的东西"写成了结论。 | 高（会让人按"免费"做预算决策） | 把 §9 成本行改成条件句：`免费额度可支撑 N 台 = 316/N（无共享缓存）；共享缓存后的实际值必须等 P0 的命中率测量`；并在 §9 补一句指回 §8.1 的账号级口径 |
| **S2** | **"客户端不再持 key"作为收益不成立**：§3/§5 明确保留"用户自选网关（openrouter/vercel/自带 key）"，而只要这条 fallback 存在，**App 里必须继续留 key 处理与网关调用路径**（CF 挂掉时它就是唯一可用路径）。所以收益只能是"**默认路径**不需要用户 key"，不是"客户端不再持 key"。§0/§9 的措辞把两者混为一谈。 | 高（是"最大收益"的一半论据） | 把 §0/§9 的"客户端不再持 key"改为"**默认路径**不再需要用户 key；fallback 仍需保留网关与 key 处理，且必须保证 fallback 不需要运营方在场" |
| **S3** | **§1 的 fail-open 代码引用是错的**：`engine.rs:356-358` 实际是 `classify_pending` 里的 **cache lookup**（注释 + `match self.cache.lookup`），不是 fail-open。真正的 fail-open 在 `engine.rs:11`（模块文档「fail-open 是默认路径」）与 `:331/:389-390/:405-406/:435-436` 的 `observer.requeue`、`:442` 的 `GatewayUnavailable`。**用错引用会让读者以为 fail-open 是缓存分支的行为。** | 中（引用级，但影响理解正确性） | 引用改为 `engine.rs:11` + `:331/389/405/435` + `:442`；`cache.rs`（缓存坏=空库）保留 |

其余 6 项核验（成本算术、免费额度、JSON 名单、AI Gateway TTL、数据条款、其余代码引用）见 §2–§7：**成本表在 Lead 自纠后与官方逐格一致**；JSON/缓存/条款三项**基本成立但有精度瑕疵**；代码引用有 **4 处错/偏**。

---

## 1. 复核方法

* 官方页（直接抓取，非搜索摘要）：
  * Pricing `https://developers.cloudflare.com/workers-ai/platform/pricing/`（Last updated Sep 17, 2026）
  * JSON Mode `https://developers.cloudflare.com/workers-ai/features/json-mode/`（Sep 14, 2026）
  * Data usage `https://developers.cloudflare.com/workers-ai/platform/data-usage/`（Apr 21, 2026）
  * Limits `https://developers.cloudflare.com/workers-ai/platform/limits/`（Sep 17, 2026）
  * AI Gateway caching `https://developers.cloudflare.com/ai-gateway/features/caching/`（Aug 27, 2026）
* 代码：`grep -n` / `sed -n`，见各节命令。
* **未验证**（结构上无法在本机验证）：Worker 冷启动/尾延迟、Workers AI 实际推理质量与校准、KV 一致性与实测命中率、AI Gateway 缓存真实命中、`xraytun.top` 在真实大陆网络下的可达性、账单在真实滥用下的表现。

---

## 2. #1 成本算术复核（修正后的值）

官方口径（pricing 页原文）：
> Workers AI is included in both the Free and Paid Workers plans and is priced at **$0.011 per 1,000 Neurons**.
> Our free allocation allows anyone to use a total of **10,000 Neurons per day at no charge**. … All limits reset daily at 00:00 UTC. If you exceed any one of the above limits, further operations will fail with an error.
> The Price in Tokens column is equivalent to the Price in Neurons column.

我的算式（`Neurons/次 = 1000×in_neuron_per_M/1e6 + 80×out_neuron_per_M/1e6`；`$ = Neurons×0.011/1000`；`次/天 = 10000/Neurons`）：

| 模型 | 官方 neurons/M（in/out） | 我算 Neurons/次 | 我算 $/次 | 免费额度/天 | 文档（修正后） | 一致？ |
| --- | --- | --- | --- | --- | --- | --- |
| `@cf/ibm-granite/granite-4.0-h-micro` | 1542 / 10158 | **2.3546** | **$0.0000259** | **4246.9** | 2.35 / $0.000026 / 4,250 | ✅（文档 4,250 是上取整，精确 4,247） |
| `@cf/meta/llama-3.1-8b-instruct` | 25608 / 75147 | **31.6198** | **$0.0003478** | **316.3** | 31.6 / $0.00035 / 316 | ✅ |
| `@cf/qwen/qwen3-30b-a3b-fp8` | 4625 / 30475 | **7.0630** | **$0.0000777** | **1415.8** | 7.06 / $0.000078 / 1,416 | ✅（Lead 自纠行，现已正确） |
| `@cf/openai/gpt-oss-20b` | 18182 / 27273 | **20.3638** | **$0.0002240** | **491.1** | 20.4 / $0.00022 / 491 | ✅（文档 20.4 是 20.36 的粗舍入） |

**交叉校验**（用"Price in Tokens"列独立再算一遍，两列必须相等）：

```
granite-4.0-h-micro     $tok=0.000026 $neu=0.000026
llama-3.1-8b-instruct   $tok=0.000348 $neu=0.000348
qwen3-30b-a3b-fp8       $tok=0.000078 $neu=0.000078
gpt-oss-20b             $tok=0.000224 $neu=0.000224
```

⇒ **Lead 修正后的 4 行全部与官方一致**（我的精确值如上；差异只在 0.01 级舍入）。

### 2.1 但这张表**选样**有偏，会低估"便宜档"（事实性遗漏）

同页更便宜、且与本案同构的候选（官方 neuron rate 直算）：

| 模型 | Neurons/次 | $/次 | 免费额度/天 | 是否在 JSON Mode 名单 |
| --- | --- | --- | --- | --- |
| `@cf/meta/llama-3.2-1b-instruct` | **3.92** | $0.000043 | **2,553** | ❌ 不在 |
| `@cf/meta/llama-3.1-8b-instruct-fp8-fast` | **6.91** | $0.000076 | **1,447** | ❌ 不在 |
| `@cf/qwen/qwen3-30b-a3b-fp8` | 7.06 | $0.000078 | 1,416 | ❌ 不在（文档已列） |
| `@cf/zai-org/glm-4.7-flash` | 8.41 | $0.000093 | 1,189 | ❌ 不在 |
| `@cf/meta/llama-3.2-3b-instruct` | 7.06 | $0.000078 | 1,416 | ❌ 不在 |
| `@cf/meta/llama-3.1-8b-instruct-fp8` | 15.87 | $0.000175 | 630 | ❌ 不在 |
| `@cf/meta/llama-3.1-8b-instruct-awq` | 13.10 | $0.000144 | 764 | ❌ 不在 |

**含义**：文档表只列了 4 个模型，读者会以为"便宜的非 JSON 档 ≈ 5–7 neurons"；
实际上 **`llama-3.1-8b-instruct-fp8-fast` 只要 6.91**（比文档里的 qwen 行还便宜，且是 8B），
而**最便宜的 LLM 是 `llama-3.2-1b`（3.92）**。
建议：表里补一行"同族 fp8/awq 变体更便宜，但**不在 JSON Mode 名单**"，
否则 §8.2 的"便宜又支持 JSON 的只有 8b"与 §8.1 的选样会共同造成"没有中间档"的错觉。

---

## 3. #2 免费额度与 App 预算（账号级共享）

**App 默认值复核**（文档引用位置不准）：
```
$ grep -rn "per_day" crates/xt-intent/src/
crates/xt-intent/src/engine.rs:78:            per_day: 200,          <-- 真正的默认在这里
crates/xt-intent/src/settings.rs:60:        per_day: settings.per_day,
crates/xt-intent/src/settings.rs:202:        assert_eq!(c.per_day, 200);
```
⇒ 文档 §8.1 写「`settings.rs` 实测 `per_day = 200`」**位置不准**：默认值在 **`crates/xt-intent/src/engine.rs:78`**（`per_minute: 10` 在 `:77`），`settings.rs` 只是映射与断言。数值 200 正确。

**"账号级共享"文档写了吗？写了**：
> 文档 §8.1 末段：「⚠️ **但这是"一台机器"的口径**：10,000 Neurons/天是**账号级**的。如果 Worker 服务 N 台机器，免费额度被 N 台共享…**没有共享缓存之前**不能假设免费。」

⇒ 任务书假设的"文档漏掉账号级"**不成立**（文档 :236-238 已写）。
**但 §9 的成本行没有带这个限定**（"现在这一档基本免费"），与 §8.1 自相矛盾 —— 这是 S1 的出处。

**算术**（用 llama-3.1-8b 的 31.62 neurons/次）：
| 机器数 N | 每台 200 次/天需要 | 免费额度 10,000 能支撑 | 结论 |
| --- | --- | --- | --- |
| 1 | 6,324 neurons | 10,000 | 不超（剩 3,676） |
| 2 | 12,648 neurons | 10,000 | **超 26%** |
| 5 | 31,620 neurons | 10,000 | 超 216% |
| 10 | 63,240 neurons | 10,000 | 超 532% |

⇒ **不用共享缓存时，"免费"只对 1 台成立**；第 2 台起就必须开 Workers Paid（官方：超出后请求直接报错）。共享缓存能把"新域名才花神经元"的比例降下来 —— 但**这个比例没人测过**（文档自己在 §8.5 列为待核实）。

---

## 4. #3 JSON Mode 名单与"只有 8B"推论

官方 JSON Mode 页「Supported Models」原文列表（6 个）：
```
@cf/meta/llama-3.3-70b-instruct-fp8-fast
@cf/meta/llama-3-8b-instruct
@cf/meta/llama-3.1-8b-instruct
@hf/nousresearch/hermes-2-pro-mistral-7b
@hf/thebloke/deepseek-coder-6.7b-instruct-awq
@cf/deepseek-ai/deepseek-r1-distill-qwen-32b
```
⇒ 文档 §8.2 的 6 个名字**与官方完全一致** ✅（含大小写/路径）。"不保证满足 schema → `JSON Mode couldn't be met` 必须处理"、"不支持流式"也都在官方页 ✅。

**"便宜又支持 JSON 的只有 llama-3.1-8b / llama-3-8b"**：
* 在**有公布价格的** JSON 名单模型里成立：llama-3.1-8b=31.62、llama-3-8b=31.62、llama-3.3-70b=43.05、deepseek-r1-distill-qwen-32b=80.67。
* **`@hf/nousresearch/hermes-2-pro-mistral-7b` 与 `@hf/thebloke/deepseek-coder-6.7b-instruct-awq` 在当前 LLM pricing 表里查不到**（该表共 40 个 LLM，无这两个 ID）⇒ **这两个的价格/是否仍可服务 = 未验证**。严格说，推论应写成"**在有官方定价的 JSON 模型里**只有…"。
* **口径提醒**：JSON Mode ≠ 结构化输出的唯一路径。文档 :254 自己给了备选（"便宜模型 + 提示词约束 JSON + 本地严格解析"），而这条路下最便宜的 8B 级是 `llama-3.1-8b-instruct-fp8-fast`（**6.91**，是 JSON 版 8B 的 1/4.6 成本）—— 值得作为 P2 的一个评测分支，否则"要么贵 4.6 倍、要么换个陌生模型"是伪二选一。

---

## 5. #4 AI Gateway 缓存 TTL vs 本项目 TTL

官方 caching 页原文：
> The minimum TTL is **60 seconds** and the maximum TTL is **one month**.
> Caching is **disabled by default**. …
> AI Gateway constructs the cache key by concatenating … **Provider** … **Endpoint** … **Model** … **Provider authentication header** … **Full request body**. … **exact match** …
> **Cache in AI Gateway is volatile.** If two identical requests are sent simultaneously, the first request may not cache in time for the second request to use it …
> We plan on adding **semantic search** for caching in the future …

本项目 TTL（`grep -n "TTL_SECS" crates/xt-intent/src/engine.rs`）：
```
32:pub const BLOCK_TTL_SECS: u64 = 90 * 24 * 60 * 60;   // 90 天
33:pub const ALLOW_TTL_SECS: u64 = 30 * 24 * 60 * 60;   // 30 天
35:pub const DEFERRED_TTL_SECS: u64 = 60 * 60;          // 1 小时
38:pub const SCHEMA_FAILURE_TTL_SECS: u64 = 10 * 60;    // 10 分钟
```
⇒ **文档 §8.4 的结论成立**：官方上限 1 个月 < 本项目 90 天，**判决缓存必须自建（KV）**，AI Gateway 缓存只能当"完全相同请求"的顺带一层。文档引用的 `engine.rs:31-37` 覆盖到了（精确值是 `:32/:33/:35`，`:31` 是注释行、`:37` 是空行 —— 区间偏松但不误导）。
**补充（文档未提）**：KV 也有自己的 TTL/一致性语义（KV 是最终一致），所以"自建缓存"不能假设"删除立即全球生效"——见 §8 风险 #3。

---

## 6. #5 数据条款引用保真度

官方 data-usage 页原文（逐条）：
```
- You own, and are responsible for, all of your Customer Content.
- Cloudflare does not make your Customer Content available to any other Cloudflare customer.
- Cloudflare does not use your Customer Content to (1) train any AI models made available on
  Workers AI or (2) improve any Cloudflare or third-party services, and would not do so unless
  we received your explicit consent.
- Your Customer Content for Workers AI may be stored by Cloudflare if you specifically use a
  storage service (e.g., R2, KV, DO, Vectorize, etc.) in conjunction with Workers AI.
```

| 文档 §8.3 | 官方 | 判定 |
| --- | --- | --- |
| "不跨客户"句 | 逐字一致 | ✅ 原文 |
| "不训练"句 | 官方分 (1)/(2) 两条，且是 "unless **we** received"；文档合并为一句并写成 "unless **it** received" | ⚠️ **转述**（意义不变，但文档把它标成"原文关键句（逐条）"） |
| "内容归你所有；仅当你同时使用 R2/KV/DO/Vectorize 等存储时，内容才可能被存储" | 官方两句：own/responsible + "may be stored … if you specifically use a storage service" | ⚠️ 准确转述（漏了 "and are responsible for" 的责任前缀，但文档 §8.3 后文补了责任） |

⇒ 结论：**条款结论（不训练、不跨客户、用了 KV 则自己负责）成立**；但"逐条原文"里有 1 条是转述、"it/we" 被改了。
建议：要么贴官方原文并附 URL 行号，要么把标题从"原文关键句"改成"要点转述"。

---

## 7. #6 §1 表里的 `文件:行` 逐条核对

命令：`sed -n` / `grep -n`（均在当前工作区）。

| 文档 §1 断言 | 实际 | 判定 |
| --- | --- | --- |
| `cache.rs:26 QUESTIONS_REVISION` | `crates/xt-intent/src/cache.rs:25: pub const QUESTIONS_REVISION: u32 = 1;` | ❌ **偏 1 行**（正确 `:25`；`:26` 是空行） |
| `engine.rs:31-37` TTL | `:32 BLOCK`、`:33 ALLOW`、`:35 DEFERRED` | ⚠️ 区间偏松（精确 `:32-35`） |
| `cache.rs:40` 指纹 | `crates/xt-intent/src/cache.rs:40: pub fn fingerprint(...)` | ✅ 精确 |
| `engine.rs:90 thresholds_repr` | `engine.rs:90: pub fn thresholds_repr(&self) -> String` | ✅ 精确 |
| `gateway.rs`、`INTENT-FILTER.md §7.6` 作为"网关可插拔（typesafe/zen/openrouter/vercel）"的位置 | `gateway.rs` 里只有 HTTP 客户端与测试串（`grep -rn "openrouter\|vercel" crates/xt-intent/src/gateway.rs` → 0 命中）；预设清单在 `apps/ui/src/types.ts:531`（`IntentPreset = "typesafe" \| "zen" \| "openrouter" \| "vercel" \| "custom"`）与 `crates/xt-core/src/model.rs:751-763`（OpenRouter/Vercel 的 base URL），UI 文案在 `apps/ui/src/pages/Intent.tsx:82-83` | ❌ **文件错**（应引 types.ts / xt-core model.rs / Intent.tsx） |
| `engine.rs Budget`、默认 ≤200/天 | `engine.rs:78: per_day: 200`（`per_minute: 10` 在 `:77`）；`budget.rs` 是 `Budget` 实现 | ⚠️ 数值对；精确位置是 `engine.rs:78`（非 `settings.rs`） |
| `cache.rs`、`engine.rs:356-358` fail-open | `:356-358` 是 `classify_pending` 的 cache lookup 注释与 `match self.cache.lookup`；真正的 fail-open：`engine.rs:11`（文档「fail-open 是默认路径」）、`:331/:389-390/:405-406/:435-436`（`observer.requeue`）、`:442`（`Deferred(GatewayUnavailable)`） | ❌ **引用错**（S3） |
| 父域继承 `rules.rs:246+` | `rules.rs:246: pub fn normalize`，继承在 `:278+`（`registrable_domain`）与 `cache.rs` 的 `INHERIT_TTL_CAP_SECS`（7 天） | ✅（区间正确） |
| `question.rs` / `eval.rs` 存在 | 两者都在 `crates/xt-intent/src/`（17 个模块） | ✅ |
| `infra/incident-collector/` 同 zone 路由 / 不落 IP / 加盐哈希 / 不开观测 / R2 只存 zip+manifest | `wrangler.toml:12-16`（大陆 `*.workers.dev` 常不可达、同 zone、Workers Route 优先于 Pages）；`PRIVACY.md:23`（IP 不入 R2、salt 内存、重启即失）；`:27`（`[observability] enabled = false`）；`worker.mjs` 头部 | ✅ 全部属实 |

---

## 8. #7 为什么**不该**搬（尽量强的反方）

> 这一节刻意站"保持现状（App 直连可插拔网关 + 每机本地缓存）"，不替搬方案补理由。

**8.1 这是新增一个"线上服务"，不是新增一段代码。**
现成参照物：`infra/incident-collector/`（**一个**远更简单的"收上传包"端点）已经是
**10 个文件 / 2048 行**：`worker.mjs` 376、测试 402、`deploy-check.sh` 365、`verify.sh` 147、`smoke.sh` 144、`wrangler.toml` 58、`PRIVACY.md` 75、README 290。
intent Worker 要做的事**严格更多**：App token 鉴权与轮换、HMAC 键、KV 读写、限速/预算、模型供应商客户端、schema 校验、版本协商（model_id/questions_rev/canonical_rev）、吊销表、以及"共享缓存中毒"的止血操。
⇒ 现状的运维面是 **0 个自有线上服务**；搬到 CF 后至少 **1 个**（且它承载默认路径）。文档把这件事写成 §9 的"前提①愿意维护"，**成本却没有任何量级估计**。

**8.2 共享缓存把"单机故障"变成"全局故障"。**
今天每台机器自己的 `cache.rs` 坏了：**只影响这一台**，而且 `cache.rs` 已有"缓存坏 → 当空库"的 fail-open。
共享 KV 里一条错判：**所有用户**同时拿到同一个错误路由，直到 TTL 到期或有人吊销。
文档的缓解（多数一致才入共享、短 TTL、吊销表）每一条都**新增一个写路径 + 一个管理动作**：
"多数一致"要求在线聚合（或引入 DO/队列）；"吊销表"要求一个带鉴权的管理端点 + 审计。**攻击面与运维面同步变大**，文档没有算这笔账。

**8.3 隐私责任转移是真的。**
今天"域名 → 第三方 LLM 网关"是**用户自己选的服务**；搬到 CF 后，**运营方成为默认路径上的可见方**。
HMAC 键解决的是"KV 里能不能枚举域名"，**不解决流量分析**：谁的请求、什么时候、多少个键、键的重复模式，
仍然能刻画"这段时间哪些新域名被问过"——而"新域名"正是浏览行为里最有信息量的部分。
现有 incident collector 是**用户手动、opt-in、低频**的上传；intent 判决是**自动、按新域名、高频**的遥测，**性质不同**，不能因为前者的隐私习惯"已经验证过"就顺推后者。文档 §5 已意识到（"责任转移"），但把它列成"条款上更好"的脚注，权重偏低。

**8.4 可用性不是"无损降级"。**
"CF 挂掉 → 本地缓存 + fail-open"只覆盖"缓存里有 / 不拦"两种情况；**新域名在 CF 挂掉时等于没判决**。
而且**用户自选网关要能兜底，App 就必须保留 key 与网关调用路径**（见 S2）——也就是说
"多一个服务"并没有换掉"客户端持 key"，而是两者并存 ⇒ 复杂度只增不减。
文档 §6 说"关掉 `intent_endpoint` → 回落到自选网关/纯本地"，说明它自己也承认这条路径必须一直可用。

**8.5 用户自由被"默认值"侵蚀。**
即使保留 openrouter/vercel/自带 key，**默认与推荐变成运营方 Worker** 之后：
① UI/文档要多一套隐私披露；② 出问题的第一联系人变成运营方；③ 用户想验证"我的域名到底发给了谁"要跨两个信任域。
"可选"在事实层面会被"默认"压过 —— 这是产品决策，不是技术细节。

**8.6 反方能接受的最小让步。**
如果最终一定要做，赞成"**只做 P0（清洗规范 + 命中率度量），不建服务**"：P0 不碰 CF、无运维面，却能回答"共享缓存到底值不值"——而这正是 §8.1 成本表和 §9"收益最大"都依赖的未知量。

---

## 9. #8 文档漏掉的风险（每条：影响 + 措辞可用的官方依据 + 缓解）

| # | 风险 | 影响 | 官方依据 | 缓解 |
| --- | --- | --- | --- | --- |
| R1 | **Workers AI 限流** | Text Generation **300 请求/分钟**（账号级、按 task type）；**需付费的模型**只有 **20 RPM（标准计费）/ 50 RPM（预付费 AI Gateway credits）**。多机+无共享缓存时，几百台机器的自然尖峰就能撞到 300 RPM，表现为"部分用户开始 deferred" | limits 页：「Text Generation — 300 requests per minute …」「Paid models … 20 requests per minute」 | 服务端 per-token 限速 + 全局 Neuron 日预算硬停 + 共享缓存；把 429/限流当**明确失败**（defer），不要当"不是广告" |
| R2 | **KV 最终一致** | 吊销/纠错**不会立即全球生效**；中毒条目在传播窗口内继续被命中；"一键吊销"的语义要打折 | （KV 一致性属 Workers KV 文档；本次未抓取 ⇒ **未验证**，仅作为设计约束提出） | 吊销用强一致载体（DO）或"吊销名单 + 短 TTL + 版本号"；把吊销传播延迟写成明示 SLA |
| R3 | **域名=浏览画像（流量分析）** | 即使 HMAC，请求的时间/频率/键重复模式仍能推断"哪些新域名、什么时候被访问"；默认路径下运营方可见 | data-usage 页只承诺"不训练/不跨客户"，**没有**承诺不可见；HMAC 不改变网络可见性 | 批量 + 抖动 + 客户端不送精确时间戳；分桶上报；UI 明示"运营方能看到你访问了**新域名这一事件**" |
| R4 | **被刷账单 / 放大攻击** | 公开 Worker + 模型调用 = 花别人的钱打自己的模型；免费额度爆掉后（Paid）按 $0.011/1k Neurons 记账 | pricing 页：「you will be charged at $0.011 / 1,000 Neurons for any usage above the free allocation」 | App token + 每 token 分钟/日配额 + 全局硬预算 + 告警；token 轮换；把"额度耗尽导致请求失败"当成**全站降级事件**而不是单机事件 |
| R5 | **提示注入（域名即输入）** | 攻击者可注册/构造形如指令的域名，让模型产出"block/allow"错误判决，甚至诱导它输出越界内容 | JSON Mode 页：「Workers AI can't guarantee that the model responds according to the requested JSON Schema」 | 域名只作**数据**、严格字符白名单（IDNA 规范化后再校验）、schema 校验失败即 deferred、模型输出永不可直接变成路由（必须过本地 verdict 逻辑） |
| R6 | **`xraytun.top` 被墙的影响面放大** | 现有同 zone 路由（`infra/incident-collector/wrangler.toml:12-16`：大陆 `*.workers.dev` 常不可达、所以走自有域名）一旦被针对，**默认判决路径与上传端点同时不可用** | 仓库内既有实测注释（非官方） | 明确"fallback 必须不需要运营方"（→ 见 S2）；为判决与上传用不同子域/不同 zone 以限制相关性；把 CN 可达性作为 P1 的门槛指标 |
| R7 | **合规含义** | 从"用户自带网关"变成"运营方集中处理与浏览相关的信号"：可能落入 GDPR/CCPA 的 controller/processor 判断、需要 DPA/子处理者披露/留存与删除机制；若用户在中国大陆，还叠加 PIPL 的跨境与最小化要求 | 官方数据页只约束 CF 与你的关系，**不替你解决**你对用户的责任 | 默认关闭（文档已做）；写清留存=TTL、不落明文/不落 IP、可选自建网关；发布子处理者清单；把"能否行使删除权"做成可验证操作 |
| R8 | **模型下线/漂移** | 固定 `model_id` 可能在 CF 侧被弃用；同名模型版本更新也会改变判定分布（文档已把 `model_id` 进指纹，但**没有"模型消失"的处置**） | pricing 页模型清单会变（本次抓取已与文档 §8.1 的付费清单不完全一致，见 §10） | 多模型 fallback + 启动自检 + `model_id` 变更触发本地指纹作废（已有机制，补"模型不存在"分支） |
| R9 | **冷启动/尾延迟** | Worker + KV 冷读 + 模型推理叠加；今天 App 直连网关是一次外部调用，搬完后是"App→CF→模型"，多一跳且多一个排队点 | 本次无法实测 ⇒ **未验证** | P1 验收里已有 "p95 拖尾延迟"，补一个**绝对预算**（如 p95 ≤ X ms）与"超过即不上线"的判据 |
| R10 | **Rust/JS 清洗漂移** | 文档已识别（P0 共享夹具），但**这是新引入的跨语言契约**：今天只有 Rust 一份 | — | P0 的双向对拍是正确做法；补一条"夹具进 CI 且任一实现改动必须同时改夹具"的硬门禁 |

---

## 10. #9 结论校准：§9「会，但要按条件成立」

**我的判断：方向正确，但整体偏乐观；乐观点不在"要不要评测"（那部分反而很克制），而在三处把"未测量的收益"写成了结论。**

1. **成本行过于乐观**（S1）：§9 说"现在这一档基本免费"，而 §8.1 自己写了账号级、N 台共享。按默认 200 次/天/台 + 31.62 neurons/次，**第 2 台机器就超免费额度**。要么改写为条件句，要么把 §9 的成本行删掉、只保留 §8.1。
2. **"最大的一块收益"缺少测量**：文档 §8.5 把"今天每台每天新增域名数"列为待核实，§8.1 也承认"共享缓存前不能假设免费"——**收益的大小完全取决于这个未测量分布**。在 P0 拿到命中率之前，把共享缓存称为"最大收益"是**结论先于证据**。P0 一旦测出"新域名极少"，收益可能主要来自"服务端迭代 + 无 key"，而不是缓存。
3. **运维代价被低估**：§9 前提①"愿意维护一个线上判决服务"是一句抽象承诺；对照 `infra/incident-collector` 的 2048 行/10 文件，intent Worker 只会更大。且由于必须保留用户自选网关的 fallback，**"无 key"只对默认路径成立**（S2），复杂度是"新增服务"而不是"替换服务"。

**偏保守/正确的地方**：把"换模型"单列为独立决定、坚持离线评测门禁（文档 §0/§9）、坚持本地缓存/预算/fail-open 不上移（§3）、坚持不落明文域名（§5）——这几条我认为是对的，且恰好是反方最担心的那几点被"条件"挡住了。

**我给的一句话版**：
> **"该做 P0，但 P1 的立项不该以'免费/共享缓存收益最大'为前提，而应以'P0 测出的新域名分布 + p95 延迟 + 一个可验证的吊销机制'为前提；在 P0 出数之前，保持现状（App 直连 + 本地缓存）是更稳的默认。"**

补充两条与官方清单相关的小修正：
* 需付费模型清单，官方原文是 `kimi-k2.6`、`kimi-k2.7-code`、`glm-5.2`、`glm-5.3`、`glm-5.3-flash`、`deepseek-v4-flash-0731`、`deepseek-v4-pro-0813`；文档写成 "`kimi-k2.6/2.7`、`glm-5.2/5.3`、`deepseek-v4-*`" —— 不存在 "kimi-k2.7"，且漏了 `glm-5.3-flash`。
* "超出免费额度需要 Workers Paid（$5/月起）"：pricing 页只说"need to sign up for the Workers Paid plan"，**$5/月是 Workers Paid 的价格，不在该页**（我未在本页找到该数字）⇒ 建议标注为外部事实或补 URL。

---

## 11. 结论一览

| 待核项 | 结论 |
| --- | --- |
| #1 成本表（修正后） | ✅ **逐格与官方一致**（我的精确值：2.3546 / 31.6198 / 7.0630 / 20.3638 neurons；差异仅 0.01 级舍入）；⚠️ 表**选样偏贵**，漏了 3.92/6.91 的更便宜档 |
| #2 免费额度够一台机器 | ✅ 单台成立；❌ **多台不成立**（默认 200/天/台时第 2 台即超）；文档 §8.1 已写账号级，但 §9 没带限定 |
| #3 JSON Mode 名单 | ✅ 6 个名字与官方一致；⚠️ 其中 2 个在 pricing 表无价 ⇒ "便宜只有 8B"应加"有公布定价的"限定 |
| #4 AI Gateway TTL | ✅ 官方 min 60s / max 1 month；本项目 Block 90d / Allow 30d ⇒ **结论成立**（精确行号 `engine.rs:32-35`） |
| #5 数据条款引用 | ⚠️ 3 条里 1 条是转述且改了 "we→it"；不跨客户句逐字一致 |
| #6 §1 代码引用 | 1 处精确错误（fail-open 引用）、1 处偏 1 行（QS_REVISION）、1 处文件错（网关预设）、1 处位置不准（per_day）、1 处区间偏松 |
| #7 反方 | 见 §8（6 条） |
| #8 漏掉的风险 | 见 §9（R1–R10） |
| #9 结论校准 | **偏乐观**，三处需收紧；方向（先 P0/先评测）正确 |
