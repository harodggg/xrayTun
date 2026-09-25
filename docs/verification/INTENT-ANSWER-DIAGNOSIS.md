# 意图过滤 · 无标注桶「原始答案」诊断（task-13）

> 问题：两次实测都是**无标注桶 0 拦**（免密钥 0/96、TypeSafe 付费 0/149，限速、`unanswered=0`），
> 阈值全放开仍 0/48。但我们只统计了**最终 verdict**，看不到模型实际答了什么 ——
> 于是「模型判不动」与「我们的闸门/解析把它的答案丢了」分不开。
>
> 这份文档把**模型的原始答案**取回来，给出可判定的结论。
>
> 工具：`crates/xt-intent/examples/eval_domains.rs`（诊断逻辑在 `crates/xt-intent/src/eval.rs`，
> 新增 11 条单测）。时间：2026-09-25。
> **隐私**：原始答案文件（含域名）只在仓库外 `/tmp`；本文只有聚合数字。

---

## 0. 先决发现：**审计通道本来就答不了这个问题**

这是这次诊断的第一个结论，先说，因为它能省掉后人重复这条路：

`AuditRecord` 里那四个最要紧的字段 —— `category` / `ads_intent` / `risk_of_breakage` /
`choice_confidence` —— 只在 **`Block`** 时才有值，因为 `IntentEngine::record()` 是从
`verdict.block_evidence()` 取的（`crates/xt-intent/src/engine.rs`，`record()` 一带）。
`Allow` / `Deferred` 判决写进审计时这四个字段**全是 `None`**。

推论：**"0 拦"这件事本身就保证审计里看不到任何原始答案**。如果这次照 task 的字面
"优先复用审计通道"，会得到"模型什么都没答"的假象 —— 而实际上它每次都答了。
（顺带：`Verdict::reason_str()` 也不带 `Deferred` 是哪个 id 出的问题，所以连
"缺哪一个字段"都复不出来。）

因此本次在**网关层**加了录制：`--raw-answers <path>` 落 `GatewayResponse.answers` 的
**原样 JSON**（每个 id 的 `type` / `choice` / `noul` / `confidence` / `probabilities`），
`--audit-dir` 仍然开着只作旁证（`outcome`/`reason` 与录制一致，可交叉对账）。

---

## 1. 怎么复跑（命令与语料）

语料是**本机真实连接日志**（在仓库外，只读）：

| 项 | 实测 |
|---|---|
| 读入日志行 | 166,672 |
| 连接行（分母） | 13,845（配不到域名 10,530，76.0%） |
| 观测到的域名 | 239 |
| ├ 强正样本（命中 `category-ads-all`） | 14 |
| ├ 强负样本（apple/microsoft/google/gov-us） | 76 |
| └ **无标注（本次要判的那批）** | **149** |
| 本次实际问出 | 100（按连接数取前 100，`--limit 100`） |

两个档位、同一份语料、同一组**产品默认阈值**（`ads_intent_min=0.85`、
`risk_of_breakage_max=0.3`、`choice_confidence_min=0.5`）、同一条限速（`--sleep-ms 1200`）：

```bash
CORPUS="$HOME/Library/Application Support/com.xraytun.desktop/logs"

# ① 免密钥档（OpenCode Zen）
cargo run -q -p xt-intent --example eval_domains -- \
  --corpus "$CORPUS/app.jsonl" --corpus "$CORPUS/app.1.jsonl" \
  --geosite-dir apps/desktop/binaries --report /tmp/intent-diag-free.md \
  --live --base-url https://opencode.ai/zen --model jev-1.13-free \
  --only-unknown --limit 100 --sleep-ms 1200 \
  --audit-dir /tmp/intent-diag-free-audit \
  --raw-answers /tmp/intent-raw-free.jsonl \
  --answers-summary /tmp/intent-diag-free-summary.md

# ② TypeSafe 付费档（产品默认预设）。Key **只从环境变量读**，不落任何文件：
JEV_API_KEY="$(cat /tmp/jev_api_key)" cargo run -q -p xt-intent --example eval_domains -- \
  --corpus "$CORPUS/app.jsonl" --corpus "$CORPUS/app.1.jsonl" \
  --geosite-dir apps/desktop/binaries --report /tmp/intent-diag-typesafe.md \
  --live --base-url https://api.typesafe.ai --model jev-latest \
  --only-unknown --limit 100 --sleep-ms 1200 \
  --audit-dir /tmp/intent-diag-typesafe-audit \
  --raw-answers /tmp/intent-raw-typesafe.jsonl \
  --answers-summary /tmp/intent-diag-typesafe-summary.md

# ③ **不联网、不复跑模型**地复核任何一份原始答案（阈值可用 --ads-min/--risk-max/--choice-min 改）
cargo run -q -p xt-intent --example eval_domains -- \
  --summarize /tmp/intent-raw-typesafe.jsonl \
  --answers-summary /tmp/intent-diag-typesafe-summary.md
```

`--only-unknown` 把整份预算给无标注桶（不加它会先喂正负样本，那是量精确率/误杀率的跑法）。
`--raw-answers` 录的是原始答案；`--answers-summary` 是本文用的聚合表（不含域名）。
渲染器有一条单测直接断言**输出里不出现任何域名**（`the_diagnosis_contains_no_hostnames`）。

---

## 2. 判定（三分）

| 档位 / 模型 | 问到（拿到答案） | **(a)** 高置信"不是广告" | **(b)** 给了广告类别、被闸门挡下 | **(c)** 低置信 / 丢字段 | 未拿到答案 | 真拦到 |
|---|---:|---:|---:|---:|---:|---:|
| **TypeSafe** `jev-latest`（产品默认预设） | **100 / 100** | **91** | **0** | **9** | 0 | 0 |
| **Zen** `jev-1.13-free`（免密钥） | 67 / 100 | **59** | **0** | **8** | **33**（全部 HTTP 429） | 0 |

* 第 4 列 `unanswered` 在 Zen 档是 **33 条 HTTP 429**（`status`），所以 Zen 档的真实样本是 67，
  不是 100 —— 这 33 条**既不算 (a)/(b)/(c)，也不算"模型判不动"**。
* 两个档位的 **(b) 都是 0**。
* 两个档位的 **(c)** 全部是 `non_ad_low_confidence`：**答案解析得好好的**（`type` 全对、
  零缺失），类别是"不是广告"，只是自报置信度 < 0.5。

### 2.1 为什么这不是 (b)（我们的闸门）

* **类别计数里 `ad_or_monetization` / `tracker_or_analytics` 出现 0 次**（见 §3.1）——
  模型的答案里根本没有"这是广告"这个类别，所以阈值/白名单/刹车怎么调都不可能产生 block。
* **反事实**（把风险刹车关掉，`risk_of_breakage_max=1.0`，再走一遍闸门）：
  TypeSafe **100/100 下一条挡它的是 `category_not_blockable`**；Zen 67/67 同样是类别闸门。
  也就是说"被 risk 挡下"只是闸门链的**第一条**在报，真正决定它不构成 block 的是类别。
* 旁证：之前"阈值全放开（0.2/0.9/0.05）仍 0/48"与这里一致。

### 2.2 为什么这不是 (c)（解析/问法丢了答案）

* **零解析损失**：`endpoint_kind` / `ads_intent` / `risk_of_breakage` 的 `type` 分别是
  `choice` / `noul` / `noul`（TypeSafe 100/100、Zen 67/67），期望 id **一个都没缺**。
* `noul` 没有以 `choice` 形状回、`choice` 也没有出现白名单外的词 —— 这两类"读不出答案"的
  形状**一条都没有**。
* (c) 里的 9 / 8 条是"类别说不是广告 + 自报置信度 < 0.5"：模型在**犹豫**，
  但它给出的信息我们**读到了**。它们被判进 (c) 是按 task-13 的定义（低置信也算 (c)），
  不代表"我们丢了它的答案"。若按"答案是否被读懂"这个更窄的口径，这 9/8 条同样是"读懂了"。

---

## 3. 原始分布（聚合）

### 3.1 类别计数（模型实际选的 `endpoint_kind`）

| 类别 | TypeSafe | Zen |
|---|---:|---:|
| `api_or_service` | 78 | 48 |
| `cdn_or_infra` | 16 | 13 |
| `human_site` | 4 | 5 |
| `unknown` | 2 | 1 |
| `ad_or_monetization` | **0** | **0** |
| `tracker_or_analytics` | **0** | **0** |
| （没有 choice 字段：网关失败） | 0 | 33 |

### 3.2 `ads_intent` 分桶（「是广告」的概率）

| 桶 | TypeSafe | Zen |
|---|---:|---:|
| [0, 0.1) | 10 | 6 |
| [0.1, 0.3) | 69 | 46 |
| [0.3, 0.5) | 15 | 10 |
| [0.5, 0.7) | 6 | 5 |
| [0.7, 0.85) | **0** | **0** |
| [0.85, 1.0] | **0** | **0** |
| 缺（网关失败） | 0 | 33 |

**TypeSafe 的最大值 0.64；Zen 的最大值 0.63。** 没有任何一条到过 0.7。
产品默认阈值 0.85、之前"全放开"实验的 0.2 —— 分数这一项在放开到 0.2 时会有人过线，
但类别那一项（§3.1）仍然全是"不是广告"，所以仍然 0 block。

### 3.3 `risk_of_breakage` 分桶（拦了会不会坏）

| 桶 | TypeSafe | Zen |
|---|---:|---:|
| [0, 0.3] 不触刹车 | **0** | **0** |
| (0.3, 0.5] | 3 | 2 |
| (0.5, 0.8] | 92 | 64 |
| (0.8, 1.0] | 5 | 1 |
| 缺（网关失败） | 0 | 33 |

两个档位都把"拦了会坏"的风险报得很高（全部 > 0.3），所以闸门链的第一条**每次**都在报
`breakage_risk_too_high`。这是 §2.1 那个反事实存在的理由：只看这一条会误以为
"风险阈值放开就有 block"。

### 3.4 `choice_confidence` 分桶（模型自报的置信度）

| 桶 | TypeSafe | Zen |
|---|---:|---:|
| [0, 0.5) 过不了置信度闸门 | 9 | 8 |
| [0.5, 0.7) | 14 | 13 |
| [0.7, 0.9) | 26 | 18 |
| [0.9, 1.0] | 51 | 28 |
| 缺（网关失败） | 0 | 33 |

### 3.5 引擎实际挡下它的原因（+ 反事实）

| 原因 | TypeSafe | Zen |
|---|---:|---:|
| `breakage_risk_too_high`（风险刹车） | 100 | 67 |
| `status`（HTTP 429，没拿到答案） | 0 | 33 |
| 反事实：关掉刹车后 → `category_not_blockable` | 100 | 67 |

---

## 4. 判定：(a) —— **模型明确说"不是广告"，0 拦不是我们的闸门/解析造成的**

* TypeSafe（产品默认预设）：拿到答案 100 条，**(a)=91、(b)=0、(c)=9（全部是低置信，不是丢字段）**。
* Zen（免密钥）：拿到答案 67 条，**(a)=59、(b)=0、(c)=8**（另 33 条 HTTP 429 没量到）。
* 两个档位都是：类别里 **0 条**广告/追踪、`ads_intent` 最大值 0.63~0.64、
  `risk_of_breakage` 全部 > 0.3。**没有任何一条"我们本可以拦、但被阈值/白名单挡下"**（(b)=0），
  也没有任何一条"答案被解析丢"（§2.2）。

结论：**0 拦是模型的回答，不是我们丢了它的答案。** 这与"两次独立运行 0/96"、
"阈值全放开 0/48"不矛盾 —— 现在知道那 0 不是被闸门挡的，而是模型对这些真实域名
（绝大多数是 API/CDN 基础设施）给出的 `endpoint_kind` 就是"接口/服务/CDN"、
`ads_intent` 只有 0.1~0.6。

### 4.1 (b)/(c) 的最小修法 —— **不适用**

因为 (b)=0、(c) 里没有一条是解析损失，**没有"最小修法"可交**：没有任何闸门参数、
白名单或解析改动能把这些样本变成 block。（(c) 的 9/8 条是"模型犹豫"，
把 `choice_confidence_min` 降到 0.3 会把它们从 (c) 挪进 (a)/(b)，但它们的类别仍然
不是广告，`ads_intent` 仍然 < 0.7，**block 数仍然是 0**。）

如果还要救这个功能，唯一可能改变答案的入口是**问法/提示词或换模型**（`category`/`ads_intent`
是模型给的），而不是阈值 —— 而那属于 product 的决策，需要**改完重新量**。
本次**没有改动任何产品默认值、没有动 `apps/**`、没有改判断行为**。

---

## 5. 我没能验证的

* **Zen 档的 33 条 HTTP 429**：那 33 个域名在 Zen 档没有测到（不是"判不动"，是"没问到"）。
* 无标注桶共 **149** 个，本次按连接数取了前 **100** 个；剩下 49 个没问。
* **"模型判错了"这件事本身**无法用本方法验证：无标注桶没有 ground truth，
  这里只能说"模型没有给出广告类别/高概率"，不能说"这些域名其实都是广告"。
  要验证后者需要人工抽查（`--dump-unknown` 导出，只在仓库外）。
* 语料是**正在写入**的本机日志，换一天跑数字会变。
* 只覆盖 `endpoint_kind`/`ads_intent`/`risk_of_breakage` 三条问题的答案；
  MITM 内容级判定的行为不在本次范围内。
