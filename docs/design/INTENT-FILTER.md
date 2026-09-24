# 意图过滤 · 把 Jev 判定接进 TUN 透明代理

> 状态：**设计（Phase 0）**。本文只承诺能被证据支持的东西；凡是不确定的写法都标了
> `⚠ 待核实` 并进 §13 清单。实现按 §10 分期推进，每期都有自己的「可运行证据」。
>
> 输入诉求（用户原话）：*"可以把意图加入透明代理。在 tcp/udp 层直接把广告拦截了。
> 这应该能够过滤所有的广告。取决于 Jev 的准确程度。思考怎么加入 xraytun。"*
>
> 本文的第 1 节就是对这句话里**三个需要修正的前提**的回答。第 2 节起是怎么做。

---

## 0. 一页结论

| 问题 | 结论 |
|---|---|
| 能在 TCP/UDP 层拦广告吗 | **能，但只在域名/端点这一层**。这一层能干掉绝大多数**第三方**广告与追踪端点 —— 现有 `preset-ads`（`geosite:category-ads-all`）已经在做，本项目要做的是把它从「静态表」升级成「会判新域名的引擎」 |
| 能过滤**所有**广告吗 | **不能**。同域广告（X 时间线与回复里的推广帖、YouTube 前贴片、B 站推荐位）的字节来自主站自己，L3/L4 看不到 URL 与 body，拦域名等于把主站一起拦掉 |
| 那就没有办法了？ | 有。**可选 MITM**（本地根证书拆 TLS）能到内容层，但代价明确：只对用户点名的一小撮域名拆、证书固定类 app 天然不受影响（不在名单里）、隐私面最大且必须显式同意。同域广告只有这一条路 |
| Jev 在这里的价值是什么 | **不是替代域名表**。域名表覆盖已知广告域；Jev 用来判**没进表的新域名**（零日广告域、换域名的投放端），以及在 MITM 之后判 **URL / JSON 条目**的意图。它能花多少钱、花在哪里，由 §7 的预算与缓存决定 |
| "取决于 Jev 的准确程度" 怎么落地 | **先评测，再上线**。§10 的 P1.5 是一套离线评测夹具：用本机真实连接日志 + 已知名单做标注集，量出「精确率 / 每千条连接的误杀数」，达不到 §10 判据就不允许生成 block 规则 |
| 会动到系统网络配置吗 | **不会**。过滤只往 Xray 配置里加 `blackhole` 路由规则 —— 不新增系统路由、不碰 DNS 备份、不进 helper 快照。**唯一例外是可选 MITM 的根证书**，它走 helper 的既有快照机制（§8.1） |

---

## 1. 先修正三个前提（事实层）

### 1.1 "在 tcp/udp 层直接拦截" —— 能看到的和看不到的

TUN 数据面能拿到的字段（本项目的 `ConnectionRecord` 已经全部解析出来了）：

```text
network (tcp/udp) · target_host (多是 IP，偶尔是域名) · target_port
inbound_tag (tun/socks/http/api) · outbound_tag (node-*/direct/block/dns-out)
domain（由 `sniffed domain` 时序配对而来，标注了 domain_paired 与时间差）
ts_ms
```

再加上嗅探层能拿到的：

| 能看到 | 前提 |
|---|---|
| TLS ClientHello 的 **SNI** | 非 ECH；`sniffing.destOverride` 含 `tls` |
| HTTP 的 **Host 头** | 明文 HTTP（今天很少） |
| QUIC Initial 里的 **SNI** | `destOverride` 含 `quic` |
| 明文 DNS 的**查询名** | 被 `internal-dns-hijack` 送到 `dns-out`（项目已有） |
| 包的**大小与时序** | 永远 |

看不到的（这是硬边界，不是实现不足）：

* **TLS 里的 URL / Header / Body**。占了今天流量的 ~95%。
* **HTTP/3 里的任何内容**。QUIC 是加密的，不解就是不看见。
* **DoH / DoT 的查询内容**（查询本身是 HTTPS）。浏览器开着 DoH 时，DNS 层的拦截就失效 ——
  但 SNI 与 IP 仍在，所以退化而不是失能。
* **ECH 启用后的 SNI**（ClientHello 里只剩一个公共名）。退化到 IP 层。⚠ 待核实（§13-3）

**所以：** 「在 tcp/udp 层拦广告」= 拦**端点**（域名 / IP / 端口 / 协议），
不是拦「页面上的广告位」。这两件事经常被混为一谈，而它们的成功率差一个数量级。

### 1.2 "应该能够过滤所有广告" —— 这个前提是错的，而且错在最显眼的地方

按广告字节的来源分两类：

| 类型 | 例子 | 域名层能拦吗 |
|---|---|---|
| **第三方投放 / 追踪端点** | `doubleclick.net`、`googlesyndication.com`、各家 DSP、统计 beacon、`/collect` 心跳 | **能**。这正是 AdGuard / Pi-hole / `geosite:category-ads-all` 的领域，实测拦截率很高 |
| **同域内嵌** | X 时间线里的推广帖、YouTube 前贴片、B 站推荐位、各家 App 的信息流广告 | **不能**。字节来自主站域（`x.com`、`googlevideo.com`、主站 CDN），拦它等于拦主站 |

第二类**恰好是本项目起源那个扩展（`jev-x-filter`）在浏览器里做的事**：它看得见页面里的
DOM 与正文，所以能判「这条帖子是推广」。透明代理看得见全机器，但看不见内容。
两者不是替代关系，是互补（§12.3 的可选反馈回路把两者接起来）。

要在透明代理里拦第二类，**只有 MITM 一条路**（§8），并且要求：
拆了 TLS 还得看得懂那个站的接口形状（X 的 timeline JSON、YouTube 的 player 响应），
每个站一套规则 —— 这是「可选、按域名 opt-in」的真正原因。

### 1.3 "取决于 Jev 的准确程度" —— 对，所以先量它

Jev 的准确率不能靠感觉。本设计把「量的能力」当成 P1 的一部分：

* 标注集来源：已确认的广告域（`geosite:category-ads-all` ∩ 本机日志）作正样本；
  常年正常使用的域名（`geosite:cn` 里的主站、系统域名）作负样本；
* 只统计**本机真实出现过的**域名，因为 Jev 要判的就是这些；
* 指标：**精确率（block 判决中真正是广告的比例）** 与 **误杀率（每 1000 条连接的 FP）**，
  而不是召回率 —— 漏拦一个广告用户几乎无感，误杀一个正常站点用户会立刻关掉功能。

达标线（写死的判据，见 §10-P1.5）：holdout 精确率 ≥ 0.95 且 FP ≤ 1/1000 连接，
否则不生成 block 规则，只留在审计里。**这条判据先于功能存在。**

---

## 2. 四层漏斗：每一层加一次钱、加一次风险

```
                          ┌─ L0 静态名单        0 成本 0 请求     ──▶ block
  一条连接 ──▶ 有没有域名 ─┼─ L1 域名意图(Jev)    1 次 noul/新域名  ──▶ block / allow
                          ├─ L2 流量形状        0 成本 0 请求     ──▶ block / 只是加分
                          └─ L3 MITM 内容(opt-in) 拆 TLS，见 §8    ──▶ block / 剔除条目
```

| 层 | 输入 | 判据 | 误杀代价 | 落点 |
|---|---|---|---|---|
| **L0 静态** | 域名 / IP | `geosite:category-ads-all`（已有 `preset-ads`）+ 用户导入名单 | 低（名单是社区维护的） | `routing::preset_rules`（不动） |
| **L1 域名意图** | 域名串 + 首次出现时**真的拿得到**的上下文：端口、网络层、入站 tag、同一会话里出现过几次 | Jev：`noul` 广告/追踪概率 + `choice` 类别 | **中**（模型可能把新服务判成广告） | 本文核心：`xt-intent` |
| **L2 流量形状** | 只被单一主域引用、周期性心跳、无用户交互、只有一个字节级固定的请求 | 本地规则，**只加分不单独定罪** | 低 | `xt-intent::shape` |
| **L3 MITM 内容** | 请求 URL/Header；可选响应体片段 | Jev 判 URL 意图；或按 JSON 指针剔除条目 | **高**（要拆 TLS） | §8，opt-in |

设计原则：**任何一层都只能「放行」或「提交证据」，只有 L0/L1 能单独定罪。**
L2 永远只是加权。这条约束让「流量形状」这类容易过拟合的特征无法单独误杀。

> ⚠️ **上下文里有两项拿不到，不许假装有**（上游核实结论，见 §15）：
> * **进程名**：Xray 的路由字段是 `process`（`processName` 是废弃的键），而它在
>   **稳定版 26.3.27 的 macOS 上根本不工作**（`find_process_others.go` 的 build tag
>   是 `!windows && !linux`，matcher 恒返回 false，darwin 实现只在预发布版里有）。
> * **"上一跳页面"**：没有 referer。所以"只被某个主域引用"这类形状特征**算不出来**，
>   `xt-intent::shape` 只用了时间与端口维度的特征。

---

## 3. 三个"不做"的决定（与项目既有约束一致）

### 3.1 不 patch Xray

* Xray-core 是 **MPL-2.0 文件级 copyleft**，改一个文件就要公开那一个文件（`docs/03 §7.2` 已记录）；
* 更实际的是**跟随成本**：上游改路由/嗅探的实现，我们的补丁就要重做一次。
* 判定逻辑放核心外的代价只有一个：**规则变化原本要重启核心**（本项目本来就是重启，`docs/03 §6`）。
  但上游核实发现：`RoutingService` 暴露了 **`AddRule` / `RemoveRule` / `ListRule`**
  （`xray.app.router.command`），而本项目**已经**把 `RoutingService` 放进了 `api.services`
  （`config.rs::build_api`）—— 也就是说**意图规则的增删可以完全不重启核心、不断任何连接**。
  两个尖角必须写进实现：
  1. `AddRule{shouldAppend:false}` 会把**整份规则表替换掉** ⇒ 只能 `append`，删除按 `ruleTag` 逐条来；
  2. `domainStrategy` **只在 `Router.Init` 里读**，`AddRule` 改不了它 ⇒ 本功能不碰它
     （我们只需要 `IPIfNonMatch` 不变）。

  代价：要手写两条 protobuf 消息（`AddRuleRequest` / `RemoveRuleRequest`），
  与既有 `stats.rs` 手写 h2c gRPC 的做法一致（`docs/03 §6` 的取舍不变）。
  **分期决定**：P2 先用「重启」把链路打通（简单、已有大量测试），P2.5 换成 `AddRule`，
  并用「切换时连接不中断」作为验收判据。

### 3.2 不在数据面等模型

第一次见到某域名时**必须立刻放行**，同时在后台问 Jev；判决落到缓存，**下一次**才生效。
理由是数据的：一次 Jev 往返（几百 ms）压在 TCP 握手上，用户看到的是「网站很慢」，
而广告在首访本来就已经加载完了 —— 也就是说「等模型」既毁了体验又没多拦住东西。

这条决定要写进 UI 文案：**"第一次访问可能看到广告，第二次不会再出现"**。
把它说清楚，否则会被当成 bug。

### 3.3 不做全局 MITM

只对用户点名的域名集合拆 TLS。名单之外的流量（银行、iCloud、系统更新、证书固定的 app）
完全不碰，因此**不存在"MITM 把某个 app 搞崩"的全局故障模式**，只存在"名单里某个域名拆坏了"的局部故障。

---

## 4. 架构与数据流

```text
  Xray 核心 stdout（唯一日志单点，commands/core.rs:240 那个转发任务）
        │  accepted/sniffed 行
        ▼
  RecentConnections::observe()                      ← 已有：出口计数 + 连接记录
        │  ConnectionRecord（新增一个返回记录的入口，见 §5-6）
        ▼
  xt-intent::Observer                               ← 新：去重、窗口聚合、形状特征
        │  CandidateFlow { domain, port, network, process?, first_seen, shape }
        ▼
  xt-intent::Engine
        ├─ Cache（持久，判决 + TTL）───────────────┐
        ├─ Budget（按窗口计费）                    │
        ├─ Gateway（Jev HTTPS 客户端）             │
        └─ Audit（append-only JSONL）              │
        │                                          │
        ▼  Verdict { Block | Allow | Unknown } ◀───┘
  xt-intent::rules::materialize()
        │  Vec<RoutingRule>（复用既有 IR！）
        ▼
  去抖 / 只在集合变化时触发
        │
        ▼
  CoreConfigInput.rules（预设 + 意图 + 自定义，按 §6 排序）
        │
        ▼
  build_pretty → 原子写 runtime/config.json → 重启核心（既有机制）
```

关键设计：**意图判决的表达形式就是 `Vec<RoutingRule>`**，不是新概念。
`CoreConfigInput.rules: &[RoutingRule]` 已经是"调用方合并好的规则"，所以：

* `xt-core` **不需要依赖 `xt-intent`**（不会成环）；
* 既有单测、`route_explain`、Topology 页全部继续工作 —— 界面上的规则链里会多出
  `intent-block-*` / `intent-allow-*` 两带，用户能在拓扑图上**看到**意图规则真的生效了。

---

## 5. 精确接入点（文件 · 函数 · 改什么）

| # | 位置 | 改动 | 备注 |
|---|---|---|---|
| 1 | `Cargo.toml`（workspace） | `members += "crates/xt-intent"`；新依赖见 §5.1 | 版本 0.9.0（新功能，次版本 +1） |
| 2 | `crates/xt-intent/`（新） | `question/gateway/verdict/cache/budget/audit/shape/rules/engine/observer` | 平台无关、纯逻辑，可完整单测 |
| 3 | `crates/xt-core/src/model.rs` | `AppSettings` 加 `#[serde(default)] pub intent: IntentSettings`；`Default`；`migrate()`（老配置 → 默认关闭，无需迁移动作）；`SETTINGS_VERSION` 视情况 +1 | 默认**关闭** |
| 4 | `crates/xt-core/src/xray/config.rs` | 新增 `merge_rules_with_intent(s, intent_allow, intent_block)`；`merge_rules()` 变成它的零意图包装（既有调用点零改动）；`preset_rules()` **顺序不变** | 顺序由 7 条单测钉住 |
| 4b | ~~`RoutingRule` 加 `source` 字段~~ | **决定不加**：意图规则是**构建期算出来的**，永远不进用户的 `settings.json`，所以 `RuleSource` 只会多一个永不落盘的字段。排序改由 `merge_rules_with_intent` 显式表达（三个切片 → 一个有序表），比给 IR 加标记更难写错 | 见 §6 |
| 5 | `crates/xt-core/src/xray/config.rs` | **不改** `build_routing`（规则由调用方合并）；在 `config.rs::tests` 加一条"意图规则必须在 `preset-cn-domain` 之前"的断言 | 见 §6 的三条顺序理由 |
| 6 | `crates/xt-core/src/xray/access_log.rs` | 新增 `observe_with_record(&mut self, line) -> ObservedLine`（`{ outbound: String, record: Option<ConnectionRecord> }`），`observe()` 变成它的薄封装 | 既有调用点与测试零改动 |
| 7 | `apps/desktop/src/commands/core.rs`（~252） | 在 `i.connections.observe(&event.line)` 旁挂 `i.intent.observe(&event.line)`；日志转发任务是**天然单点**，与现有设计一致 | 不要另起日志 tail |
| 8 | `apps/desktop/src/state.rs` | `IntentRuntime { engine, pending_verdicts, last_applied_hash, debounce }`；一个 tokio 任务做"去抖 → 合并 → 重启" | 与既有 supervisor 重启路径共用，不另写一套 |
| 9 | `apps/desktop/src/commands/settings.rs` | 持久化 `IntentSettings`；校验（阈值范围、预算 ≥ 0、名单去重） | |
| 10 | `apps/ui/src/pages/Intent.tsx`（新）+ `App.tsx` 导航 + `Settings.tsx` 入口 | 开关 / 演练模式 / 预算 / 阈值 / 名单 / 审计 / "为什么这个域名被拦" | 复用 `Topology`/`Logs` 的既有组件风格 |
| 11 | `crates/xt-proto/src/lib.rs` | **仅 MITM 期**：`Request::InstallTrustAnchor { pem, sha256 }`、`RemoveTrustAnchor { fingerprint }`、`TrustStatus` | `PROTOCOL_VERSION +1`，helper 拒绝版本不等（既有机制） |
| 12 | `crates/xt-tun/` + `crates/xt-helper/` | **仅 MITM 期**：Keychain 信任锚的安装/移除，并**进快照**（`SnapshotKind::TrustAnchor`） | §8.1 |

### 5.1 依赖决定（先写清楚，免得实现时随手指一个）

现状：workspace **没有 HTTP 客户端**（`docs/03 §6` 明确为了不引 tonic/prost 而手写了
protobuf）。所以 Jev 客户端要新引一个 HTTPS 客户端，三个选项：

| 选项 | 体积/代价 | 取舍 |
|---|---|---|
| A. 手写 HTTP/1.1 POST over `rustls` + `webpki-roots` | 新增 `rustls`（已在缓存里）；HTTP/1.1 解析用 `httparse`（已缓存的版本存在）+ `sha2` | **与项目风格最一致**（他们已经为两条 protobuf 消息手写过 h2c）。Jev 只有一个 POST 端点，手写面很小 |
| B. `ureq`（blocking + rustls） | 一棵中等依赖树 | 省事；但 blocking 客户端要塞进 tokio 任务（`spawn_blocking`） |
| C. `reqwest` | 大（hyper + tower + rustls + …） | 与 §3.1/§6 的既有取舍冲突，不推荐 |

**决定：按 A 做**，已实现（`crates/xt-intent/src/transport.rs` + `src/jev.rs`）。
实测代价（`cargo tree -p xt-intent`）：

| 指标 | 实测 |
|---|---|
| 直接依赖 | **7** 个（含 `rustls`、`webpki-roots`） |
| 传递依赖总数 | **86** 个 |
| 手写的部分 | HTTP/1.1 请求组包 + 响应解析（`Content-Length` / `chunked` / 读到 EOF）+ 退避重试，合计约 **450 行含 28 条单测** |

`rustls` 必须 `default-features = false`：默认会拉 `aws-lc-rs`（要 cmake/nasm），
我们只要 `ring` 这一条纯 cc 后端。

⚠️ **没量过的不要写**：方案 C（`reqwest`）到底多几棵树，本轮**没有实测**，
所以这里不写数字。真要换的时候用同一台机器跑一次 `cargo tree` 再比。

**已实测的真实链路**（`#[ignore]` 的在线用例，手动跑）：
`cargo test -p xt-intent --lib live_zen -- --ignored --nocapture` ⇒
TLS 握手 + HTTP 请求 + 类型化错误体解析全部正常，免密钥档当前返回
`HTTP 429 Rate limit exceeded. Please try again later.`（5.53s，含两次退避重试）。
也就是说**传输层已验证可用**；缺的只是一次拿到真答案的机会（需要 TypeSafe Key，
或免密钥档恢复额度）。

---

## 6. 规则形状与顺序（这是最容易搞错的地方）

`docs/04 §3` 已经写了三条顺序理由（private 最先 / ads 早于 cn / cn-IP 晚于 cn-domain）。
意图规则插进来后，完整顺序是：

```text
[0] internal-dns-hijack     port 53 → dns-out
[1] internal-api            inboundTag api → api
[2] preset-private          私有地址直连（永远最先，否则路由器/NAS 不可达）
[3] intent-allow-*          ← 新：人工/Jev 判定的放行（必须先于 block，才能纠正误杀）
[4] intent-block-*          ← 新：Jev 判定的广告/追踪端点
[5] preset-ads              geosite:category-ads-all → block
[6] preset-cn-domain        geosite:cn → direct
[7] preset-cn-ip            geoip:cn → direct
[8..] 自定义规则
[n] internal-fallback
```

生成出来的**真实**规律（形状与既有 `compile()` 输出一致）：

```jsonc
// intent-block-<hash>：拦一个 Jev 判定的广告域，TCP+UDP 都拦
{ "type": "field",
  "domain": ["full:adsrv-7f3.example"],
  "inboundTag": ["tun", "socks", "http"],     // 只作用于本地流量，不含 mitm 回连
  "outboundTag": "block",
  "ruleTag": "intent-block-adsrv-7f3.example" }

// intent-allow-<hash>：纠正误杀，必须排在 block 之前
{ "type": "field",
  "domain": ["full:cdn.main-site.example"],
  "outboundTag": "direct",                    // 或留空 → 落到后续规则
  "ruleTag": "intent-allow-cdn.main-site.example" }

// QUIC 处理（仅 MITM 期）：只对 opt-in 集合拦 UDP/443，逼回 TCP
{ "type": "field", "domain": ["full:news.example"],
  "network": "udp", "port": "443", "inboundTag": ["tun"],
  "outboundTag": "block", "ruleTag": "intent-quic-fallback-news.example" }
```

三条**新增**的顺序理由（每条都对应一个失败模式）：

| 顺序 | 反过来会怎样 |
|---|---|
| `intent-allow` 必须早于 `intent-block` | 用户点"这个拦错了"之后什么都不会发生 —— 最伤信任的一类 bug |
| `intent-*` 必须早于 `preset-cn-domain` | 和 ads 那条同理：`geosite:cn` 里有一批投放域名，先命中直连就等于意图判定白花钱 |
| `intent-*` 的 `inboundTag` 必须显式列举 | 不写就包含了 MITM 组件的上游回连，形成"拆包 → 再拆包"的自环（§8.3） |

### 6.1 两条必须显式写进配置的前提（否则规则会静默失效）

| 前提 | 不满足会怎样 |
|---|---|
| `sniffing.destOverride` 必须**包含**我们想按域名匹配的协议（`tls` / `http` / `quic`） | `destOverride` 是一道**闸门**而不是过滤器：不在列表里的协议，嗅探到的域名**根本不会**写进 destination，`domain` 规则**静默不再命中**。当前实现是 `s.dns.sniffing \|\| s.fakedns.enabled` 时才开嗅探（`config.rs:347`），所以要保证意图过滤开启时嗅探是开的 |
| 每个入站**必须有 tag** | 稳定版 Xray 的入站没有默认 tag；没有 tag 的入站永远匹配不上 `inboundTag` |

另外两条与 MITM 无关但同样重要的事实：

* **`ext:` 外部规则集只吃 `GeoSiteList`/`GeoIPList` protobuf，没有 `.srs`**（见 §15）；
* `blackhole` 在稳定版只支持 `response.type: "none"|"http"`，`custom` 是预发布版才有的。
  实测语义：`none` = TCP 连接成功后立刻干净 EOF（UDP 完全不回，静默丢弃）；
  `http` = 回一段 **97 字节、只有 LF 换行**的 `HTTP/1.1 403`，1 秒后 RST。
  **对 HTTPS 流量，`http` 形态浏览器看到的是一次 TLS 失败，不是一个 403 页面** ——
  所以 UI 文案不能说"会返回一个提示页"。

`ruleTag` 唯一性：复用 `config.rs` 的 `uniquify_tags()`（v0.8.37 的 P0 修复）。
意图规则的 tag 里带域名，重复的唯一来源是"同一域名既 allow 又 block" —— 由
`materialize()` 直接消解（allow 优先），并写测试钉住。

---

## 7. 判决模型：问什么、花多少钱、怎么落盘

### 7.1 三值判决 + 一个闸门

```rust
enum Verdict { Block { score: f32, category: Category }, Allow, Unknown }
```

只有 `Block` 且 `score ≥ block_threshold`（默认 0.85，可调）才生成规则。
`Unknown` / 超预算 / 网关不可达 → **放行**（fail-open），并记审计。

### 7.2 问题设计（对照 `jev-x-filter` 的四问）

`jev-x-filter` 那套问法（`src/sw/classifier.js`）是**内容级**的，判的是"这条帖子像不像
机器人化的成人诱饵"。域名级要重新设计，但沿用两条硬约束：问**原型**而不是关键词；
instructions 用英文写（上游说英文最稳）。草案：

| QID | 类型 | 用途 | instructions 草案（英文） |
|---|---|---|---|
| `endpoint_kind` | `choice` | 类别 | `Classify the hostname of a network endpoint the device connected to. Options: ad_or_monetization / tracker_or_analytics / cdn_or_infra / api_or_service / human_site / unknown. Judge from the name itself and the context lines. Do not assume a hostname is an ad only because it sounds promotional.` |
| `ads_intent` | `noul` | 广告概率 | `This endpoint looks like advertising, retargeting, tracking or monetization infrastructure rather than a site or API the user intentionally uses. Third-party ad exchanges, bidder endpoints, fingerprinting and telemetry beacons count. A site's own CDN, its own API and its own analytics-on-first-party-domain do NOT count.` |
| `risk_of_breakage` | `noul` | 误杀代价 | `Blocking this endpoint would break a page or app the user is actively using (for example it is a CDN, an auth endpoint, or a payment endpoint). If unsure, answer high.` |

闸门：`endpoint_kind ∈ {ad_or_monetization, tracker_or_analytics}` **且** `ads_intent ≥ 0.85`
**且** `risk_of_breakage ≤ 0.3`。三条都要满足才 block —— 第三条是给"误杀"专门留的刹车，
它让模型有机会说"我知道它像广告，但拦了会坏"。

阈值都要**用 §10-P1.5 的离线夹具**校准，不许凭感觉定。注意 `noul` 的语义是**「是」的概率**，
本身**没有**独立的 confidence 字段（`types.d.ts:58-62`）—— 所以 `ads_intent` 直接用概率比阈值，
不要去找 confidence。`choice` 有 `confidence` 与 `probabilities`，可以做二级闸门（例如
`probabilities.ad_or_monetization ≥ 0.7`）。相对量级参考：`jev-x-filter` 在成人内容上用的
`blockNoul` 是 0.9 级别、`baitEscalate` 0.75、`baitReview` 0.65（`settings.js:63-89`）。

### 7.3 成本、缓存、预算

* **计费单位**：一条**新域名**（不是一条连接）。同一域名的第 2..n 条连接**零成本**。
* **缓存**：`domain → Verdict + 证据 + 过期时间`，持久化到 app 数据目录（原子写 + 版本号）。
  `Allow` 的 TTL 长（默认 30 天），`Block` 的 TTL 长（默认 90 天，且带证据可复查），
  `Unknown` 的 TTL 短（默认 1 天，让它有机会被重判）。
* **缓存 key 必须包含模型与网关**。`jev-x-filter` 的指纹（`pipeline.js:93-115`）把
  阈值/动作/范围/白名单/预算都算了进去，但**漏了 `api.model` / `baseURL` / `preset`** ——
  也就是说换了模型之后，旧判决会被继续复用。**这个坑我们不要抄**：指纹 = 模型 id +
  网关 baseURL + 问题的 instructions 哈希 + 阈值。改动任一项即整体失效。
* **预算**：每小时/每天请求上限（默认 ≤ 200/天）。超额 → 只放行 + 审计记 `budget_exhausted`。
  参考量级：`jev-x-filter` 用的 30 次/分钟、800 次/天（`settings.js:147-160`），
  对桌面代理偏松；真实的「每天新域名数」⚠ 待用本机日志实测（§13-8）。
* **批量**：一次请求可以带多个问题（服务端并行答，`vendor README.md:129`），但**多个域名能不能
  塞进一个请求**取决于把域名做成 `state` 还是做成问题 —— 安全做法是**一个域名一个请求**
  （一个 `state` 对应一组问题），靠缓存与限速控制成本，绝不并发轰网关。
* **限速与重试**：串行 + 滑动窗口限速；重试沿用已被验证的默认值（最多 3 次尝试、
  退避 500ms→5000ms 带 25% 抖动、尊重 `Retry-After` 但上限 60s、
  可重试状态码 `{408,429,500,502,503,504,529}`、每次尝试独立超时 12s）。
  这些数值来自 vendor 客户端的实测默认（`retry.js:1-11`、`client.js:25`），**不要自己重编一套**。

### 7.4 审计与"为什么被拦"

每次判决写一行 JSONL：`ts / domain / verdict / score / category / questions_hash / gateway_model /
tokens_or_cost / cache_hit / applied(bool) / rule_tag`。
UI 的"为什么这个域名被拦"直接读它，**必须能展示模型的原话与分数** ——
不能给用户一个无法申诉的黑盒。

### 7.5 演练模式默认开

沿用 `jev-x-filter` 的产品经验（`0.2.x` 的默认值）：默认只记录"本该拦谁"，
不生成 block 规则。用户看过几天审计、确认没有误杀，再关掉演练。
**默认值就是产品态度**：拉黑是不可逆的，先证明再武装。

### 7.6 协议事实（已核实，来源：`jev-x-filter` v0.2.1 内置的 `jev-systemone@0.1.1`）

这一小节是 Rust 客户端要照抄的**协议**（不是产品策略）。

**请求**

```jsonc
POST {baseURL}/v1/systemone
Accept: application/json
Authorization: Bearer <key>        // 仅当有 key；Zen 免费档可无 key
Content-Type: application/json

{
  "state": "<纯文本，≤4000 字符>",   // 也可以传 JSON，但上游只在字符串上验证过
  "questions": {                    // 非空 map，key 是我们自己起的 id
    "endpoint_kind": { "type": "choice", "instructions": "...", "criteria": { "ad_or_monetization": "...", "tracker_or_analytics": "...", "cdn_or_infra": "...", "api_or_service": "...", "human_site": "...", "unknown": null } },
    "ads_intent":     { "type": "noul",   "instructions": "...", "criteria": { "true": "...", "false": "..." } },
    "risk_of_breakage": { "type": "noul", "instructions": "..." }
  },
  "model": "jev-latest"
}
```

硬约束：`choice` 选项 ≤ 255 且非空；`score` 级别 2–10；`state` 必须存在；
**问题 id 不会发给模型**（完整问题必须写在 `instructions` 里，英文），
所以 id 只用于本地映射答案。网关预设与默认模型：`typesafe` → `api.typesafe.ai` + `jev-latest`（需 key）；
`zen` → `opencode.ai/zen` + `jev-1.13-free`（**免密钥**，适合首次跑通）；另有 openrouter / vercel。

**响应**

```jsonc
{
  "model": "jev-latest",
  "answers": {
    "endpoint_kind": { "type": "choice", "choice": "ad_or_monetization", "confidence": 0.93,
                       "probabilities": { "ad_or_monetization": 0.93, "tracker_or_analytics": 0.04, "unknown": 0.03 } },
    "ads_intent":    { "type": "noul", "noul": 0.97 },
    "risk_of_breakage": { "type": "noul", "noul": 0.08 }
  },
  "usage": { "input_tokens": 120, "output_tokens": 40 },
  "cost": "0"
}
```

**读取答案时必须防御性解析**（照抄 `jev-x-filter` 的教训，但比它更严）：

| 情况 | `jev-x-filter` 的做法 | 我们的做法 |
|---|---|---|
| 期望的 id 缺失 | `!answers[id]` → 整条判 `schema_invalid`，归零 | 同：**缺任何一个期望答案 → 整次判决作废（Unknown）** |
| 值是 `{}` | **会通过**存在性检查，然后静默取默认 0 | **不通过**：要求答案对象里有合法的 `noul`/`choice` 字段 |
| `choice` 标签不认识 | 原样接受（测试里 `'nope'` 存活） | 必须落在我们枚举内，否则 Unknown |
| `noul` 是 NaN | `typeof number` → clamp 成 0 | 显式 `is_finite` 检查 → Unknown |
| HTTP 200 但 body 不是 JSON | 抛错 | 抛错 → Unknown + 审计 `schema_invalid` |

**错误映射**：401 认证、422 校验、429 限流、529 过载，其余非 2xx 通用错误；
消息取 `body.error` / `body.error.message` / `body.message`，截断 500 字符。
本地预算与 HTTP 限流是**两套独立机制**：预算耗尽不该表现为"网关坏了"，
审计里要能区分 `budget_exhausted` 与 `rate_limited`。

**成本口径**：按**调用次数**计，不按 token（`jev-x-filter` 从不读 `usage`/`cost`）。
一次「单问预检」约是「四问」的 1/4（实测口径 90/12 tokens vs 220/40）。

---

## 8. MITM 阶段（可选，按域名 opt-in）

### 8.1 信任锚：必须进快照

这是整个功能里**唯一**会改系统状态的部分，所以按项目的最高标准做：

* helper 收到 `InstallTrustAnchor { pem, sha256 }` → 校验 sha256 → 写入
  `/Library/Application Support/XrayTun/ca/`（目录 0700、私钥 0600、**私钥永不出 helper**）→
  `security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain <pem>`；
* **动手前落快照**（`SnapshotKind::TrustAnchor { fingerprint, existed_before: bool }`）；
* 移除 = 按指纹删除信任锚 + 删文件；卸载 helper 时**先移除信任锚再删自己**（与既有卸载顺序一致）；
* GUI 启动时若发现 `stale_session` 含 TrustAnchor → 先回滚（既有机制，零新代码路径）；
* 签发叶子证书用本地 CA，**每个域名一张、有效期短**（如 7 天）；
* UI 上每次安装都要一张明确的同意页：**"本机将信任一个由本应用生成的根证书。
  卸载时会移除。名单外的流量不会被拆解。"**

### 8.2 只广告 `http/1.1`（关键降复杂度决定）

MITM 组件的服务端 ALPN **只广告 `http/1.1`**，不广告 `h2`。于是：

* 被拦截的连接一律退化成 HTTP/1.1 —— 我们只需要解析 h1 请求头 + `Content-Length`/chunked；
* **不需要引入 h2 终止**（那是 MITM 里最重的一块：帧、流、优先级、流控）；
* 代价要如实写进文案：被 opt-in 的域名上**失去 HTTP/2 多路复用**，可能与直连表现略有差异；
* WebSocket（`Upgrade: websocket`）**直接盲转发**，不解析。

例外：如果某站点只支持 h2（极少）或在 h1 下行为不同，该域名应能一键移出名单 —— UI 必须给这个出口。

### 8.3 上游回连与防自环

```text
浏览器 ──TUN──▶ Xray ──(规则: domain∈名单 且 inboundTag=tun)──▶ mitm-out
                                                                   │ freedom + settings.redirect
                                                                   │   → 127.0.0.1:PORT（TCP/UDP 都支持，已实测）
                                                                   ▼
                                                          MITM 组件（本进程内 tokio 服务）
                                                                   │ 从 SNI/Host 还原目标 → 解密 → 判定 → 重新加密
                                                                   ▼
                                                          127.0.0.1:10808（Xray 既有 SOCKS 入站）
                                                                   │ inboundTag=socks ≠ 名单规则 → 不再进 mitm-out
                                                                   ▼
                                                                 node-*/direct
```

**steering primitive 选 `freedom` + `settings.redirect`，不是 `http` outbound。** 理由（都已实测）：

* `http` outbound 是 `CONNECT host:port`，**只支持 TCP**（`proxy/http/client.go`：UDP 直接报错）；
  `freedom.redirect` 的 `DestinationOverride` **TCP/UDP 都走**（`proxy/freedom/freedom.go:96-108`）。
* 两者都**不传递原始目标**：freedom 的 `proxyProtocol` 只带客户端源地址，
  `redirect` 过去的目的地址是重定向后的地址。所以 **MITM 必须自己从 SNI / Host 头还原目标** ——
  这是设计上的硬约束，不是实现细节。
* `redirect` 不需要为原域名做 DNS 解析（省一次查询）。

四点必须做对：

1. steering 规则**必须带 `inboundTag`**，且**只列 `tun`（以及系统代理模式下的 `socks`/`http`）**；
   否则 MITM 自己的回连会再次命中同一条规则 → 自环。
   已核实：一条规则里 `inboundTag` 与 `domain` 是**与**关系，入站 tag 是**精确相等**匹配
   （tag 为空时 matcher 恒 false）。
2. MITM 走**既有 SOCKS 入站**回连，不自己 dial 外网 —— 回环地址不进 TUN，天然不绕回；
3. 该 SOCKS 入站已存在（`docs/03 §3.2` 已开 UDP），**不新增监听端口**；
4. 为回连单独准备一个**有 tag 的**回连入站（如 `mitm-upstream`），
   并把它的**排除规则放在名单规则之前**，作为第二道保险。

### 8.4 QUIC

拆不了 QUIC（加密且不透明）：Initial 包能解出的只有**外层 SNI**与 ALPN，
请求路径与内容全在 1-RTT/0-RTT 里，`routeOnly:false` 时还可能把 QUIC 目标改写成外层 SNI。

**好消息（上游实测）**：本项目已经**天然**让浏览器学不到 h3 —— DNS 被
`internal-dns-hijack` 送进 `dns-out`，而内核 DNS 模块对非 A/AAAA（含 HTTPS/SVCB type 65）
返回**0 答案、约 0ms**。没有 HTTPS RR，Chrome/Firefox 就不会启用 h3，自动走 TCP。
**所以"逼回 TCP"在多数场景下是既成事实，不需要额外动作。**

**坏消息**：任何自带 DoH/DoT 的客户端（浏览器安全 DNS）绕过 `port:53`，
照样能学到 h3；`Alt-Svc` 也能在 TCP 上把 h3 广告出来。

所以兜底手段仍要留：只对 **opt-in 名单内的域名**加一条
`network: udp, port: "443", inboundTag: ["tun"] → block`（§6 的 `intent-quic-fallback-*`）。
代价要如实写：`blackhole type:"none"` 对 UDP **完全不回包**，回退要等一次超时（几百 ms 到数秒），
用户观感是"第一次连接有点卡"。不做全局。

### 8.5 判定点与动作

| 判定点 | 输入 | 动作 | 成本 |
|---|---|---|---|
| 请求行 | `host + path`（**query 值脱敏**：只留键名） | `block_request`（返回 204 或空 body）/ 放行 | 每新 (host,path 模板) 一次，缓存 |
| 请求头 | `Referer` / `Sec-Fetch-*` / `X-Requested-With` | 只作 L2 形状加分 | 0 |
| 响应体（可选） | JSON 片段 / HTML 片段，**带大小上限**（默认 ≤ 64 KiB） | `strip_json`（按 JSON 指针删条目）/ 放行 | 高，必须单独开关 |

**明确不做**：改写 HTML DOM、注入/替换脚本、把正文送往网关缓存、修改响应语义
（例如重写 `Content-Length` 之外的字段）。理由：DOM 重写是重工程（等于在里面写一个 AdGuard），
而"删掉一个 JSON 数组元素"这种窄口径动作已经能覆盖 X 这类站点的同域广告。

### 8.6 剥离后的 JSON 一致性

`strip_json` 必须重算 `Content-Length`；chunked 响应要么先缓冲（带上限）要么放弃裁剪 ——
**不许出现"内容剪了但长度没改"** 这种会把客户端搞崩的 bug。这是必测项。

---

## 9. 隐私（必须写在 UI 上，不能只写在文档里）

| 模式 | 外发什么 | 适用 |
|---|---|---|
| `local_only` | **什么都不外发**。只用 L0 静态名单 + L2 形状 | 默认；也是"我不想把浏览记录给任何人"的答案 |
| `domain_judgement` | 域名串 + 极少量上下文（端口、协议、进程名、是否首见） | 想要零日广告拦截的用户 |
| `content_judgement` | 上面 + opt-in 域名的 URL（query 值脱敏）/ 可选正文片段 | MITM 用户 |

必做项：
* 三种模式的差异要在开关旁**逐条列出外发字段**，不是一句"会发送数据"；
* 每个新域名外发前**过一次本地过滤**：内网、`*.local`、用户白名单、已知系统域一律不问；
* 审计里显示"这条判决外发了什么"；
* 一键"清空缓存 + 关功能"，以及**卸载时删根证书**。

---

## 10. 分期与验收判据

判据分两类：**机制**（能被测试钉住的）与**量**（需要真实数据才能说的）。
不允许用"量"代替"机制"，也不允许只做机制就宣称有效。

| 期 | 交付 | 可运行证据（验收） |
|---|---|---|
| **P0** | 本文 | 文档评审；§13 待核实项清零或明确标注 |
| **P1** | `xt-intent`：问题构造 / 网关客户端 / 缓存 / 预算 / 审计 / 规则物化 / 观测器 | `cargo test -p xt-intent`：伪造网关（trait 注入，**测试内零网络**）覆盖 判决闸门三条件、缓存命中不再请求、预算耗尽 fail-open、TTL 过期重判、allow 优先于 block、ruleTag 唯一、离线时 fail-open；`materialize()` 输出与 §6 的 JSON 逐字比对 |
| **P1.5** | **离线评测夹具**：本机连接日志 → 候选域名 → 标注集 → 跑 Jev → 报告 | 一份可复跑的报告（脚本 + 输出）：精确率、FP/1000 连接、按类别拆分；**判据：holdout 精确率 ≥ 0.95 且 FP ≤ 1/1000，否则不进入 P2 的默认开启路径** |
| **P2** | 接入设置/配置/状态机；`AppSettings.intent`；去抖重启 | `cargo test` 全绿；生成配置跑 `xray run -test -c` exit 0（真实核心）；断言 intent 规则排在 `preset-cn-domain` 之前；演练模式下**断言配置里没有 block 规则**；打开后核心重启**只在集合变化时发生**（计数器断言） |
| **P3** | UI 页（开关/演练/预算/阈值/名单/审计/解释） | UI 单测（既有 tsx 测试风格）+ 演练模式跑一天的真实审计截图 |
| **P4** | MITM：`xt-proto` 新请求、helper 装/卸信任锚 + 快照、MITM 服务、steering 规则 | `cargo test -p xt-tun -p xt-helper`；CA 安装→**杀掉 helper**→重启→自动回滚（快照测试）；ALPN 只 h1 的断言；防自环：`TestRoute` 断言 socks 入站的回连不命中名单规则；`strip_json` 的 `Content-Length` 一致性测试；一个真实站点开关前后对比（广告请求数下降） |
| **P5（可选）** | 反馈回路：`jev-x-filter` 扩展 → `127.0.0.1` 本地端点 → 本机级拦截（§12.3） | 扩展侧不改隐私边界（仅 localhost）；端点只接受回环来源 + token |

---

## 11. 风险与失败模式

| 风险 | 检测 | 缓解 |
|---|---|---|
| **误杀主站**（把 CDN/主站 API 判成广告） | 审计里"被拦域名的后续连接失败率"；用户申诉 | 演练模式默认开；`risk_of_breakage` 问；allow 带永远优先；一键加入白名单 |
| **首访见广告** | —（这是设计，不是 bug） | UI 文案写清；对高频广告域预热（可选：每天预判一次 Top-N 未知名单） |
| **重启抖动** | 重启计数 + 每次重启的原因 | 去抖（默认 5 min）；只在 block/allow **集合哈希变化**时重启；重启前归档"本次为什么变" |
| **ECH 隐藏 SNI** | 命中率下降（配对域名比例本身就是可观测指标，`PairingStats` 已有） | 退化到 IP 规则 + 形状；文档如实说明 |
| **浏览器 DoH 绕过 DNS 层** | 同上 | 主要靠 SNI；不试图拦 DoH 端点（那会把浏览器整坏） |
| **Jev 网关不可用 / 配额** | 预算计数 + 错误率 | fail-open；缓存仍生效；本地名单兜底；绝不让判定失败变成断网 |
| **MITM 私钥/CA 成为攻击面** | 文件权限断言（0600/0700） | 私钥永不出 helper 目录；短效叶子证书；一键卸载；卸载顺序先删信任锚 |
| **拆包的站点行为异常** | 该域名下的错误率 | 只 h1；WebSocket 盲转发；一键移出名单；默认只 opt-in 用户点名的域名 |
| **嗅探本身的开销** | 首字节延迟（本机可测） | 已实测：能识别的 HTTP/TLS ClientHello 约 **2ms** 完成路由，**识别不了的首包要等最多约 200ms** 的拆包缓冲。所以意图过滤开启时，嗅探只给需要按域名匹配的入站开，并保留"关掉嗅探"的开关 |
| **ECH 让 SNI 失效** | 域名配对率下降（`PairingStats` 已有） | 见 §15：把 `routeOnly` 改成 `true` 可避免"用外层 SNI 重解析并改连"这条更糟的路径；除此之外只能退化到 IP/端口层 |
| **与其它 VPN/代理冲突** | 既有 R4（`docs/07`） | 本功能**不加系统路由**，只在 Xray 内部 blackhole，冲突面不扩大 |

---

## 12. 与既有项目的关系

### 12.1 与 `preset-ads` 的关系

`preset-ads` 是 L0，保留不动。意图过滤是它**上面**的一层：名单命中的不走模型（省钱），
名单没命中的才问。这条"先查表后问模型"的次序本身就是成本设计，不是实现细节。

### 12.2 与 `jev-x-filter`（浏览器扩展）的关系

| | `jev-x-filter` | 本文（XrayTun） |
|---|---|---|
| 视野 | 页面 DOM（x.com） | 全机器连接 |
| 判定对象 | 帖子正文/显示名/图片 | 域名、URL、JSON 条目 |
| 动作 | 隐藏 + 静音/拉黑账号 | blackhole / 剔除条目 |
| 覆盖 | 一个站 | 所有 app |
| 同域广告 | **能** | 只有 MITM 才能 |

两者是互补而非替代。共享的是**方法论**（类型化问法、闸门、缓存、预算、演练、审计），
不是代码。

### 12.3 可选反馈回路（P5，最"有意思"的一块）

扩展在页面里**已经知道**什么被隐藏了，而 `PerformanceResourceTiming` 能给出那条广告
实际来自哪些主机。回路：

```text
扩展判定"这条是广告" ──▶ 提取资源主机 ──▶ POST http://127.0.0.1:<port>/intent/report（仅回环 + token）
                                                      │
                                        XrayTun 把它变成 L1 的**高置信度先验**
                                        （比模型判域名更准：有页面上下文）
                                                      │
                                                      ▼
                                        本机级拦截：所有 app 都不再连这些主机
```

这条回路把"看得见内容的那一侧"和"管得了全机器的那一侧"接起来，而且是本设计里
唯一能**零模型成本**提高准确率的信号。它的边界也要写死：只走回环、只接受扩展的签名/token、
只上报主机名（不上报正文/图片）。

---

## 13. 待核实清单（live）

> 每一条都要有"证据落点"（文件行号或上游 URL），核完把结论写回正文并删除本行。

| # | 待核实 | 为什么重要 | 状态 |
|---|---|---|---|
| 1 | Jev 网关的精确请求/响应形状、每请求问题数上限、`choice` 选项上限、超时与重试、计费口径 | §7.2/§7.3 直接依赖 | ✅ 已核实 → §7.6（每请求问题数上限**代码里没有**，服务端限制不可从仓库得知；按"一域名一请求"规避） |
| 2 | Xray 路由规则字段全集；`processName` 在 darwin 是否可用、是否要 root | §2 的 L1 上下文里有"进程"，若不可用要改写问题 | ✅ 已核实 → `processName` **已是废弃键**（真键是 `process`），且稳定版 26.3.27 在 macOS 上**不支持**它；darwin 实现只在预发布版。⇒ 问题里**不问**进程 |
| 3 | ECH 启用后 `tls` 嗅探的行为 | §11 的退化路径 | ✅ 已核实：嗅探只读扩展 `0x00`，对 ECH（`0xfe0d`）**无感知**，拿到的是**外层 SNI**；QUIC 同理。⇒ §11 的退化路径成立 |
| 4 | outbound `http` 协议是否就是"向上游 HTTP 代理发 CONNECT"，字段是什么 | §8.3 数据面 | ✅ 已核实（是 CONNECT，**TCP-only**）⇒ §8.3 改用 `freedom.redirect`（TCP+UDP） |
| 5 | `inboundTag` 与 `domain` 是否 **AND**；`inboundTag` 能否排除 MITM 回连 | §8.3 防自环 | ✅ 已核实：是 **AND**，精确相等匹配，**入站必须有 tag** ⇒ §8.3 的四点把这条写死 |
| 6 | `blackhole` 的 `response` 语义与客户端观测（RST/超时/伪造响应） | §6 拦截体验 | ✅ 已核实：稳定版只有 `none`/`http`；`none` = 干净 EOF（UDP 静默丢弃），`http` = 97 字节 LF-only 403 再 RST。⇒ §6.1 已写，UI 文案据此改 |
| 7 | 是否存在 DNS 层 reject 语法（`dns.rules` 或 hosts→哨兵） | §2 的"fail fast"备选 | ✅ 已核实：**没有 `dns.rules`**（写了会被静默忽略）；但 `dns.hosts {"domain:x": "#3"}` 能返回 **NXDOMAIN**。⇒ §15-3 列为可选的快速失败层 |
| 8 | 本机日志里"每天新出现的域名"真实量级 | §7.3 预算默认值 | ⏳ 待用 `access_log` 语料实测（P1.5 一起做） |
| 9 | `ext:` 外部规则集文件是否可用、能否运行期生成 | §6 可能不需要重启 | ✅ 已核实：可用，但格式是 **`GeoSiteList`/`GeoIPList` protobuf**（**没有 `.srs`**），且在稳定版是**配置构建期读取、无 watcher** ⇒ 改文件仍需重启或 `AddRule` |
| 10 | 路由规则能否运行期增删（`RoutingService` 的 RPC 清单） | §3.1 的"重启"结论 | ✅ 已核实：**能**（`AddRule`/`RemoveRule`/`ListRule`），本项目已启用该服务 ⇒ §3.1 改成两条路，P2.5 走热加 |
| 11 | 依赖树体积：手写 HTTP/1.1 vs `ureq` vs `reqwest` | §5.1 | ⏳ 实现时给 `cargo tree` 数据 |
| 12 | 把 `sniffing.routeOnly` 从 `false` 改成 `true` 的影响面（含 Fake-IP 与 ECH） | 见 §15-1，可能是一条独立于本功能的修复 | ⏳ 需要单独验证，**不在本功能里顺手改** |

---

## 14. 名词与判据速查

* **block 判决**：`endpoint_kind ∈ {ad_or_monetization, tracker_or_analytics}` ∧ `ads_intent ≥ 0.85`
  ∧ `risk_of_breakage ≤ 0.3`（三者取 AND）
* **误杀率**：每 1000 条连接的 false-positive 条数（分母是连接，不是域名 —— 因为用户感受到的是"网坏了"）
* **首访放行**：任何未命中缓存的域名在判决回来之前**必须**放行
* **fail-open**：网关不可用 / 预算耗尽 / 解析失败 → 放行 + 审计
* **集合哈希**：block ∪ allow 域名集合的稳定哈希；只有它变化才触发核心重启

---

## 15. 本次核实推翻的**既有**说法（三条，都不是本功能引入的）

这三条是设计过程中读上游源码 + 对稳定版二进制实测发现的，**与本功能无关地存在于现有代码/文档里**。
单独列出来，是因为它们各自都是一个"静默失效"型的问题：配置合法、没有报错、行为却和文档相反。

### 15.1 `sniffing.routeOnly: false` 的代价（**建议单独评估，不在本功能里顺手改**）

* 现状：`crates/xt-core/src/xray/config.rs:347-352` 固定 `routeOnly: false`，
  文档（`docs/04 §5`）给的理由是"用嗅探出的域名重新解析并据此连接，也是 `fakedns` 生效所必需的"。
* 实测：`routeOnly: false` 会把 destination **替换**成嗅探到的域名并重新解析/拨号。
  后果有三个：① 用**外层 SNI**（ECH 的封面名）去重解析，把 ECH 打坏
  （上游 issue #1211 的现象与此机制一致）；② 多一次本不需要的 DNS 往返；
  ③ 对"目标被正确嗅探但重新解析到别处"的情况，连接目标与路由判断可能不一致。
* 反过来：`routeOnly: true` 时嗅探名只用于路由，实际仍拨原 IP。
  上游实现里**Fake-IP 地址会被特判**（`isFakeIP` 时忽略 `routeOnly` 并完整设置 destination），
  所以 `fakedns` 并不因此失效 —— 文档里"fakedns 生效所必需"这一句至少是不精确的。
* 结论：这是一条**独立的行为修复**，会改变核心的分流/拨号路径，必须单独验证、单独发布，
  不能混在意图过滤里。本设计只把它标出来（§13-12）。

### 15.2 `ext:` 只吃 protobuf，`docs/…` 里的 `.srs` 是错的

* `crates/xt-core/src/routing/mod.rs:79` 与 `:82` 的文档注释写着
  `ext:custom.srs` / `ext:cn.srs`。**Xray 里根本没有 `.srs` 解析器**（稳定版与预发布版都没有）。
  `ext:` 读的是 `GeoSiteList` / `GeoIPList` **二进制 protobuf**（就是 `geosite.dat` 的格式）。
* 影响：任何人照这两行注释写一条 `ext:xxx.srs` 规则，得到的是"文件加载失败"或规则静默不命中。
* 已处理：本次把这两处注释改成 `.dat` 并写清格式（见提交）。

### 15.3 `docs/04 §6.8` 的"空 NOERROR"实测是 **RCODE=5 (REFUSED)**

* 文档（`docs/04-routing-and-dns.md:423,428,774`）说：不配 `dns-out` 的 `settings` 时，
  非 A/AAAA 查询得到的是"空 NOERROR"。
* 实测（隔离 `dokodemo-door → dns-out`，稳定版 26.3.27）：
  `type=HTTPS` 与 `type=TXT` 返回 **rcode=5 REFUSED、0 答案、约 0ms**。
  代码路径是 `nonIPQuery` 默认 `"reject"` → `rejectNonIPQuery` 用 `RCodeRefused`。
* 实际效果一样（客户端拿到瞬时空答案、不启用 h3），所以**不是故障**；
  但文档描述错了，且它正好是本功能 §8.4"浏览器天然学不到 h3"这条推论的基础 —— 结论不变，措辞要改。

> 三处的共同点：**配置合法、无报错、行为与文字相反**。这正是本设计处处要求
> "把拿不到的东西写成 `None`、把不确定的东西写成待核实"的原因。

### 15.4 `ruleTag` 唯一性的检查是**新核心才有的**（本机实测）

同一份重复 `ruleTag` 的配置，两个核心的行为完全不同：

| 核心 | `run -test -c` | 真实 `run -c` |
|---|---|---|
| **26.9.9**（`apps/desktop/binaries/xray`，本项目目标版本） | exit 0 | **拒载**，`duplicate ruleTag <tag>` |
| 26.3.27（较旧的 stable，如 `.scratch/xray-server/xray`） | exit 0 | **接受并正常启动** |

三条后果：

1. **验收必须用对核心**。`crates/xt-core/tests/rule_tag_uniqueness.rs` 的负对照
   （`assert_ne!(code, 0)`）在 26.3.27 上会**假红**，在 26.9.9 上才是对的。
   凡是要跑"真实核心验收"的地方，先确认拿到的是哪一版；本设计新增的
   `crates/xt-intent/tests/real_core.rs` 会把核心版本打进失败信息。
2. **`-test` 不等于"核心会加载它"**。在 26.3.27 上 `-test` 连路由器的唯一性检查
   都不走（真实启动也不走）。所以"`-test` exit 0"只能证明**配置可解析**，
   不能证明核心愿意起 —— 这条判据要写在验收档里，别写成"核心接受"。
3. 客户端侧的唯一性守卫（`merge_rules` / `merge_rules_with_intent` 里的
   `uniquify_tags`）**仍然是必要的**：它不依赖核心版本，是这个失败模式的真正防线。

---

## 16. 实施进度（滚动更新）

> 只写**已经跑出来**的证据（命令 + 观察到的数字）。没跑的一律写"未开始"。

| 阶段 | 状态 | 证据（可复跑） |
|---|---|---|
| P0 设计 | ✅ 完成 | 本文（含 §15 的三条既有说法纠正、§7.6 的 Jev 协议事实） |
| P1 `xt-intent` crate | ✅ 完成 | `cargo test -p xt-intent` ⇒ **94 passed**（问题构造 / 严格答案解析 / 三条件闸门 / 缓存指纹与淘汰 / 滑动窗口预算 / JSONL 审计与轮转 / 形状加分上限 / 候选过滤 / 规则物化 / 引擎编排） |
| P1 真实核心验收 | ✅ 完成 | `cargo test -p xt-intent --test real_core` ⇒ **5 passed**：意图配置被真实核心接受（`exit 0`）、人为重复 `ruleTag` 被拒且**指名冲突**、规则位置（私有 → 放行 → 拦截 → `preset-ads` → `preset-cn-domain`）逐条断言、空意图层配置**逐字节不变** |
| P2a 设置 + 规则排序 | ✅ 完成 | `cargo test -p xt-core --lib` ⇒ **252 passed**（含 7 条意图规则顺序测试 + 8 条设置测试）；`cargo test -p xraytun-desktop --test type_contract` ⇒ **6 passed**；`apps/ui`：`vitest run` ⇒ **366 passed**，`tsc --noEmit` ⇒ clean |
| P1 传输层（提前做，P2b 的前置） | ✅ 完成 | `cargo test -p xt-intent` ⇒ **127 passed**（新增 `transport` 的 14 条 + `jev` 网关的 14 条：URL/请求组包/头注入防护/`Content-Length`/`chunked`/三种状态映射/退避与 `Retry-After`/抖动上界/不重试的 4xx）；在线用例见上 |
| P2b-1 桌面接线（**只观察/判定/审计**） | ✅ 完成 | `cargo test --workspace` ⇒ **770 passed**（含 `apps/desktop/src/intent.rs` 的 21 条）；`clippy --workspace --all-targets -D warnings` 干净；UI `vitest` 366 passed |
| P2b-2 规则下发（注入 `merge_rules_with_intent` + 重启/热加） | ⏳ 未开始 | 这一版**一条规则都不生成**：`IntentSummary.block_rules` 恒为 0，且引擎在非演练模式下会明说"本版不下发规则" |
| P2c 免重启热加规则 | ⏳ 未开始 | 目标：用 `RoutingService.AddRule/RemoveRule` 代替重启（§3.1）；验收判据是"切换拦截集合时已建立的连接不断" |
| P1.5 离线评测夹具 | ⏳ 未开始 | 目标：用本机 `access_log` 语料 + `geosite:category-ads-all` 标注，量出 holdout 精确率与 FP/1000 连接；**达不到 §10 的判据就不允许默认开启** |
| P3 UI | ⏳ 未开始 | 目标：开关 / 演练 / 预算 / 阈值 / 白名单 / 审计 / "为什么被拦" |
| P4 MITM | ⏳ 未开始 | 目标：helper 装信任锚（进快照）、`freedom.redirect` 引导、ALPN 只 h1、`strip_json` 的 `Content-Length` 一致性 |

### P2b-1 的刻意边界（写清楚，免得被当成"已经能拦广告了"）

* 桌面端只接了**观察 → 判定 → 缓存 → 审计**这条链：`commands/core.rs` 的日志单点
  多挂一个 `observe_with_record`，后台 10 秒节拍跑 `classify_pending`，缓存每 6 拍落盘一次。
* **不下发路由规则、不重启核心、不改任何系统网络配置。** 所以这一版即使打开功能，
  也不可能影响用户的网。界面摘要里的 `block_rules` 恒为 0，非演练模式下引擎会在说明里
  明确写"本版只观察不拦截"。
* 密钥：本切片只支持免密钥的 Zen 预设。需要密钥的预设会以**可读原因**被拒绝
  （`config_from_settings` 里），而不是发一个必然 401 的请求。Keychain 读写是独立一步。

### 已落地的文件

* `crates/xt-intent/`（新 crate：`question` / `answer` / `verdict` / `cache` / `budget` / `audit` / `shape` / `observer` / `gateway`（trait + 脚本化假实现）/ `transport`（手写 HTTP/1.1 over rustls）/ `jev`（协议 + 重试退避）/ `rules` / `engine` + `tests/real_core.rs`）
* `crates/xt-core/src/model.rs`：`IntentSettings` / `IntentThresholds` / `IntentPreset` / `IntentAllowOverride`（默认关闭 + 演练）
* `crates/xt-core/src/xray/config.rs`：`merge_rules_with_intent()`（`merge_rules()` 变成零意图包装）
* `crates/xt-core/src/routing/mod.rs`：修掉 `.srs` 的错误注释（§15.2）
* `apps/ui/src/types.ts` + `previewSnapshot.ts`：设置形状与预览快照同步（合同测试要求）
* `apps/desktop/src/intent.rs`：桌面运行态（重建式 `follow_settings` / `observe` / `tick` / `summary`）
* `apps/desktop/src/{lib,state}.rs` + `commands/core.rs` + `commands/settings.rs`：接线
* `crates/xt-core/src/xray/access_log.rs`：新增 `observe_with_record()`（`observe()` 变成薄封装，
  两边的计数与配对行为有对照测试钉住）

### 一条仍然悬着、且**必须先量**的东西

Jev 在**域名级**问题上的精确率/误杀率，本设计一次都没有假设过。P1.5 的评测夹具
是把它变成数字的唯一途径；在这个数字出来之前，功能默认关闭、即使打开也是演练模式。
