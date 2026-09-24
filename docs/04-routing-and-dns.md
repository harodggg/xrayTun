# 04 · 分流与 DNS

---

## 1. 为什么需要一层中间表示

用户的心智模型是「什么流量 → 怎么走」：

> 大陆的网站别走代理；广告域名直接拦掉；这个游戏走日本节点。

而 Xray 的规则模型是「一组条件的**合取** + 一个 `outboundTag`」：

```jsonc
{ "type": "field", "domain": ["a.com"], "port": "443", "outboundTag": "proxy" }
```

两者形状不同，直接让用户写 Xray 规则会有三个问题：

1. **OR 必须手动展开。** 「a.com 或 b.com 走代理」要写两条规则，
   而用户会自然地写成一条。
2. **顺序敏感且没有提示。** Xray 自上而下取第一条命中。把「广告拦截」
   放在「大陆直连」之后，广告规则就永远不会命中 —— 而配置本身完全合法，
   没有任何报错。
3. **内建规则集的名字不直观。** `geosite:geolocation-!cn` 这种写法
   要求用户先去读文档。

所以 `xt-core` 里有一层 IR（`routing::RoutingRule`），
再编译成 Xray 规则。这一层是纯函数，可以完整单测 —— 而「分流不对」
是代理工具里最难排查的一类问题，能单测就一定要单测。

---

## 2. IR 的形状

```rust
pub struct RoutingRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub when: MatchCondition,   // 字段之间是 AND
    pub then: RuleAction,       // Proxy / Direct / Block
}

pub struct MatchCondition {
    pub domains: Vec<String>,       // 支持 geosite:cn / full: / regexp: / ext:
    pub ip: Vec<String>,            // 支持 geoip:cn / ext: / CIDR
    pub ports: Vec<PortMatcher>,
    pub source_ip: Vec<Cidr>,
    pub inbound_tags: Vec<String>,  // socks / http / tun / api
    pub network: Network,           // Both / Tcp / Udp
    pub process_names: Vec<String>, // macOS 支持
    pub protocols: Vec<String>,     // 嗅探结果：http / tls / quic / bittorrent
}
```

`MatchCondition::is_empty()` 全为真时，编译出的规则**没有任何条件字段**，
也就是 Xray 的 catch-all。这很容易写错，所以：

> 一条 catch-all 规则会吃掉它后面的所有规则。
> `compile()` 不做任何隐式重排 —— 想让它兜底就自己放到最后。

测试 `catch_all_rule_has_no_conditions` 断言这种规则编译后只有
`type` / `outboundTag` / `ruleTag` 三个字段。

---

## 3. 预设的顺序（不能乱）

```rust
preset_rules(RoutingPreset::BypassMainland) = [
    "preset-private",     // 私有与保留地址直连   domains: geosite:private, ip: geoip:private
    "preset-ads",         // 拦截常见广告域名     domains: geosite:category-ads-all
    "preset-cn-domain",   // 大陆域名直连         domains: geosite:cn
    "preset-cn-ip",       // 大陆 IP 直连         ip: geoip:cn
]
```

三条顺序理由，每一条都有对应的失败模式：

| 顺序 | 如果反过来会怎样 |
|---|---|
| private 必须最先 | 访问路由器 / NAS / 打印机会被塞进隧道，很可能连不上 |
| ads 必须早于 cn | `geosite:cn` 里包含不少广告域名，直连规则会先命中，拦截完全失效 |
| cn-ip 必须晚于 cn-domain | `geoip:cn` 会命中一些「域名是国外的、但 CDN 节点在国内」的站，先按 IP 直连会导致本该走代理的站走了直连 |

`bypass_mainland_orders_ads_before_cn` 只钉住了第二条 ——
它是最容易被后来者「整理排序」时打乱的一条。

编译后，`build()` 还会在用户规则**之前**插入两条内部规则、
**之后**追加一条兜底：

```
[0] internal-dns-hijack     port 53 → dns-out
[1] internal-api            inboundTag api → api
[2..n] 用户规则（预设 + 自定义，按上面顺序）
[n+1] internal-fallback     network tcp,udp → 当前选中节点
```

---

## 4. 预设有四档

| 预设 | 规则 | 适用场景 |
|---|---|---|
| `bypass_mainland` | 见上 | 日常（默认） |
| `global_proxy` | 空（全部落到 fallback） | 需要全部走代理 |
| `whitelist_proxy` | private 直连 + `geosite:geolocation-!cn` 走代理 + 兜底直连 | 只想代理特定站点 |
| `direct_all` | 一条 catch-all 直连 | 排查「是代理的问题还是网络本身的问题」 |

`direct_all` 这个预设看起来多余，实际很常用：它让用户能不关代理、
不改配置地确认「断网到底是代理造成的还是网络本身的问题」。

**自定义规则追加在预设之后**，也就是优先级低于预设。
这一点在 UI 上明确写出来了 —— 否则用户会写一条
「example.com 走代理」然后困惑为什么没生效（因为它先被 `geosite:cn` 命中了）。
需要更高优先级时的正确做法是把预设改成 `global_proxy` 或 `direct_all`。

---

## 5. 域名分流的两种实现路径

Xray 没有 sing-box 那种「一边解析一边建映射」的 DNS 引擎，
域名分流的实现取决于**能不能拿到域名**：

| 场景 | 拿得到域名吗 | 依赖 |
|---|---|---|
| 浏览器访问 HTTPS | ✅ | 嗅探 TLS SNI |
| HTTP 请求 | ✅ | 嗅探 Host 头 |
| 程序直接用 IP 连接 | ❌ | 只能靠 **Fake-IP** 才能还原出域名 |
| DNS 查询本身 | ✅ | 路由规则直接匹配 `port: 53` |

所以「域名分流不生效」通常不是规则写错了，而是这个连接**从一开始就没有域名信息**。
Fake-IP 是唯一的解法（见 [03](03-xray-integration.md#2-fake-ipxray-原生就有)），
而它默认关闭 —— 这是本项目里最需要向用户解释清楚的一个取舍。

`sniffing.routeOnly` 的取值也与此相关：我们固定用 `false`，
含义是「用嗅探出的域名**重新解析并据此连接**」，而不是「只用它做路由决策、
连接仍用原 IP」。前者对分流更准确（连接目标与路由判断一致），
也是 `fakedns` 生效所必需的。

---

## 6. DNS 的四种策略

```rust
pub enum DnsHandling { Proxy, SplitByRule, Direct, Custom }
```

**默认是 `SplitByRule`。** 0.1.0 的默认是 `Proxy`，0.2.0 改掉了 ——
原因见 §6.1 与 §6.2 末尾的两次实测。旧设置由 `AppSettings::migrate`
一次性迁移过来（0.1.0 写下的 settings.json 里 `proxy` 会被改成
`split_by_rule`，并在日志里说明改了什么）。

### 6.1 `Proxy` —— 全部走代理解析

```jsonc
"servers": ["https://1.1.1.1/dns-query", "https://8.8.8.8/dns-query"]
```

抗污染，解析结果与出口位置一致（访问 Google 会拿到就近的海外节点）。

**但实测代价很大**（0.2.0 把它从默认值上撤下来的原因）：

```console
$ curl -w '%{time_total}' https://1.1.1.1/dns-query?name=api.deepseek.com   # 经节点
0.517s / 0.469s / 0.438s

$ # 同一时刻，国内 DNS 直连
223.5.5.5 → 0.001s
```

每个查询 ~450ms，国内 DNS 只要 **1ms** —— 相差约 450 倍。更要命的是
**它把「域名能不能解析」绑在了「节点快不快」上**：节点一忙或一抖，
查询就超过内核的 DNS 超时，日志里开始刷

```
[Error] app/dns: failed to retrieve response for api.deepseek.com.
        > Post "https://1.1.1.1/dns-query": context canceled
```

而用户看到的是「什么都打不开」—— 一个和 DNS 毫无字面关联的现象。

> `dns.google` 这种**域名形式**的 DoH 端点也不再用作默认值：
> 它的主机名本身要先被解析一次，而解析它用的还是这套 DNS，属于自举依赖。

### 6.2 `SplitByRule` —— 按规则分流解析（默认）

```jsonc
"servers": [
  { "address": "https://1.1.1.1/dns-query",
    "domains": ["geosite:geolocation-!cn"] },
  { "address": "223.5.5.5",
    "domains": ["geosite:cn"] },
  { "address": "223.5.5.5" },                        // 兜底
  { "address": "https://1.1.1.1/dns-query" }         // 兜底
]
```

关键在于 `domains` 字段让**内核按域名选解析器**：
大陆域名用国内 DNS 直连解析（拿到就近 CDN），其余走远端加密 DNS。

#### 为什么**没有** `expectIPs`

早期这里给两个解析器都写了 `expectIPs`（大陆域名要求返回 `geoip:cn`
的地址，反之亦然），本意是挡掉明显不可信的答案 —— 也就是「防投毒」。
实测它造成的伤害远大于收益：

```
UDP:223.5.5.5:53 got answer: api.deepseek.com. TypeA -> [3.173.21.63], rtt: 1.19ms
failed to lookup ip for domain api.deepseek.com at server UDP:223.5.5.5:53
  > features/dns: empty response
DOH//1.1.1.1 querying: api.deepseek.com.
```

国内的解析器 **1.2ms 就给出了正确答案**，只因为 `3.173.21.63`（AWS）
不在 `geoip:cn` 里就被判为「空响应」丢弃，然后**串行回退**到 DoH。
一次查询从 1ms 变成 450ms 起步。

而「国内域名解析到海外 IP」是常态：Apple、DeepSeek 这类都在用海外云。
`expectIPs` 等于在惩罚这种正常情况。回退链再叠上节点抖动，就会演变成
§6.1 里那种满屏 `context canceled`。

去掉之后重测，同一个核心、同一份数据：

```
empty response  次数: 0
context canceled 次数: 0
api.deepseek.com      → UDP:223.5.5.5:53
weatherkit.apple.com  → UDP:223.5.5.5:53
```

> 教训：`expectIPs` 看起来是「多一道校验更安全」，实际是把
> **「解析结果不符合我的预期」和「解析失败」当成了同一件事**。
> 而前者在 CDN 时代是常态。校验机制一旦会**丢弃正确结果**，
> 它的代价就是串行回退，而这恰恰会触发它本想避免的超时。

#### 同一条 `domains` 下的候选必须全部写进去

`servers` 的回退是按「先命中 `domains` 的，再其余的」分两层。所以**同一条
`domains` 下有几个候选，就决定了这一层有几个备选**。

早期实现里每个列表只写第一个（`first_or`），于是这一层永远只有一个备选。
真实后果（用户日志，v0.5.0）：

```
[Error] app/dns: failed to retrieve response for alive.github.com.
        > Post "https://1.0.0.1/dns-query": context deadline exceeded
```

`1.0.0.1` 超时之后，回退链上**再没有别的国外解析器**了 —— 下一个是国内
解析器。被墙域名被国内解析器接着答出来，拿到的就是被污染 / 错误的 IP。

隔离实例实测（把一个国外候选指向必然超时的 `192.0.2.1`，开 `dnsLog`）：

```
只取第一个     DOH//192.0.2.1  → []  4.0s
              UDP:223.5.5.5:53 → [162.159.140.229]  33ms   ← 国内顶上了

全部写进去     DOH//192.0.2.1  → []  4.0s
              DOH//1.1.1.1     → []  4.0s                   ← 仍是国外
              UDP:223.5.5.5:53 → [162.159.140.229]  33ms   ← 最后才轮到国内
```

（那次 `1.1.1.1` 也超时，是因为隔离实例的出口受本机现有隧道影响，
**延迟数字不可信，但顺序是可信的**。）

这也让「解析器自动选优」（§6.7）的排序真正有意义：第 2..n 位不再是白排的，
而是回退时真会用到的备选。

### 6.3 `Direct` / `Custom`

`Direct` 全部用本地解析器；`Custom` 把两组列表拼起来交给用户自己负责。
两者都用于排障或特殊网络环境。

### 6.4 `disableCache` 与 `queryStrategy`

* `queryStrategy`: `UseIP` / `UseIPv4` / `UseIPv6`。默认 `UseIP`
  （双栈时按系统偏好）。纯 IPv6 环境下需要改。
* `disableCache`: 默认 `false`。诊断「DNS 结果是不是被缓存住了」时临时打开。

### 6.5 DNS 劫持规则必须限定在哨兵地址上

```jsonc
// 错误写法：匹配任何来源、任何目的的 53 端口
{ "port": "53", "outboundTag": "dns-out" }

// 正确写法：只劫持发往哨兵的查询
{ "ip": ["198.18.0.2"], "port": "53", "outboundTag": "dns-out" }
```

宽泛的写法会把**内核自己的上游解析**一起吞掉：

```
from DNS accepted udp:223.5.5.5:53 [dns-module -> dns-out]
```

内核查国内域名时用 223.5.5.5:53，这个 UDP 连接经 dispatcher 派发后撞上
`port: 53` 规则，被塞回 `dns-out` —— 也就是回到 DNS 模块自己。在 TUN 模式下
它还会按默认路由掉回 utun、再次进入 tun 入站，形成「自己喂自己」的循环。

后果是**国内解析这条腿永远出不了本机**：国内域名查不到 → 回退到走节点的
DoH → 所有解析都压在节点上 → 节点一慢就是满屏 `context deadline exceeded`。

限定成哨兵地址后，内核自己的 `223.5.5.5:53` 不再被劫持，落到 `preset-cn-ip`
→ `direct` 出站（已绑定物理网卡）→ **逃出隧道**。实测：

```
修复前: [dns-module -> dns-out]
修复后: [dns-module -> direct]
```

> 教训：**给「客户端流量」写的规则，必须限定来源或目的，否则迟早会套住
> 中间件自己的流量。** 这里 DNS 模块、DoH 客户端都是「假装成客户端的中间件」，
> 它们的包和自己的入站长得一模一样。
>
> 另一个教训是验证方式：0.2.0 的分流是在**系统代理模式**下验证的 —— 没有 tun，
> 裸 UDP 包正常从 en0 出去，一切正常。是「TUN 接管 + 宽泛端口规则」两个条件
> 凑齐才暴露。**改 DNS 路径的验证必须在 TUN 模式下做。**

### 6.6 刻意不加 `localhost` 兜底

`localhost` 在 Xray 里表示「用操作系统的解析器」。而 TUN 模式下我们把
系统 DNS 指向了隧道内的哨兵地址（`198.18.0.2`）—— 于是这个兜底会变成：

```
内核 DNS 模块 → 系统解析器 → 哨兵地址 → 进隧道 → 又回到内核 DNS 模块
```

一个自指的死循环。实测症状：

```
lookup xxx on 198.18.0.2:53: dial udp 198.18.0.2:53: connect: network is unreachable
```

去掉它没有损失：上面两组显式解析器各自都是完整可用的，
不需要再兜一层必然会绕回自己的东西。

### 6.7 解析器优选：两组必须走两条不同的路径

`auto_select`（默认开）会在启动时探测候选池，把最快的排到列表第一位 ——
而 `build_dns` 在分流模式下对每个列表**只取第一个**元素（`first_or`），
所以「排第一」就等于「换掉正在用的那台」。

候选池分两组，**测量路径不同**，因为它们在配置里的用法本来就不同：

| 组 | 谁在用 | 怎么测 | 为什么必须这么测 |
|---|---|---|---|
| 国内 | `geosite:cn` → `direct_servers[0]` | 明文 UDP，`IP_BOUND_IF` 绑物理网卡直连 | 它本来就是直连用的 |
| 国外 | `geosite:geolocation-!cn` → `remote_servers[0]` | DoH（RFC 8484），经本地 SOCKS 入站 | 直连**根本连不上** |

国外那组经节点测不是近似、也不是偷懒，那就是它的真实成本。本机实测：

```
直连  https://1.1.1.1/dns-query   → 8 秒超时，连不上
经节点 https://1.1.1.1/dns-query   → 0.255s   （3 次中位）
经节点 https://94.140.14.14/dns-query → 0.234s
经节点 https://8.8.8.8/dns-query   → 0.321s
经节点 https://9.9.9.9/dns-query   → 0.390s   （抖动大，有一次 1.39s）
```

国外这组**必须串行测**（`FOREIGN_CONCURRENCY = 1`）。它们共享同一个节点，
并发测等于在测**节点的排队**，而不是解析器的远近 —— 和国内组「不绑网卡就
测到核心排队」是同一类错误，而且后果更重：**名次会变**。实测同一批候选：

```
并发 4： AdGuard 450ms  Cloudflare备 306ms  Cloudflare 277ms  Google 697ms  Quad9 898ms
串行：  AdGuard 234ms  Cloudflare备 250ms  Cloudflare 255ms  Google 321ms  Quad9 390ms
```

并发那组的名次被压成了「谁先抢到节点」，AdGuard 从第 1 掉到第 3。代价是
国外组总耗时变成 5 台 × 4 次请求 ≈ 4–5 秒 —— 它跑在启动时的后台任务里，值得。

因此**节点未连接时国外那组标成「未探测」**，而不是硬走直连测一遍、
再把必然的超时谎报成「这台解析器不通」。界面也分两块显示 ——
把两条路径的数字放进同一张表，会被误读成同一把尺子量出来的。

国内那组绑 `IP_BOUND_IF` 仍然是必须的：不绑的话查询会经 TUN → 核心的
gVisor 栈 → 解析器，并发时测到的是**核心排队**。实测同一台阿里 DNS，
绑 en0 是 32ms，不绑（并发 6）是 155ms，而且会把快的排到后面。

「答得对不对」的多数派投票**按组分开做**。跨组混投会把正常答案判成异常：
国外解析器经节点出去，看到的 CDN 边缘和国内直连本来就可能不是同一批地址
（实测 `example.com` 两边这次恰好都是 Cloudflare 的 `104.20.23.154` /
`172.66.147.243`，但那是运气，不是保证）。另外国外那组走的是加密 DoH，
**基本不可能被抢答或投毒**，所以这一组真正要比的只是「经这个节点谁快、谁通」。

自动排序**只调池内项的相对顺序**，用户手填的服务器保留在列表后面 ——
那是用户明确想要的东西，探测器没资格替他丢掉。反过来，池内项如果这次
没测通，会被移出列表，避免一台已经不可用的解析器继续占着位置。

探测在**两个时机**跑：

| 时机 | 国内组 | 国外组 |
|---|---|---|
| 启动时（后台） | 测得通 | 必然「未探测」—— 节点还没连 |
| **连上之后（后台）** | 重测 | 重测，这一轮才测得到 |

连上后那次不阻塞连接（国外组串行，要 5–8 秒），也**不重启核心**：DNS 配置
只在生成配置时被读取，所以结果对**下一次连接**生效。为了几毫秒的解析器差异
把刚建好的 TUN 拆掉重建，不划算。

手动验证用（只读不写，不碰设置、不改系统网络）：

```bash
cargo run -p xraytun-desktop --example dns_probe              # 节点已连
cargo run -p xraytun-desktop --example dns_probe -- --no-socks # 模拟未连
```

### 6.8 `proxy/dns: rejected type ... query` 不是故障，别去"修"它

调试等级下日志里会刷：

```
[Info] proxy/dns: rejected type TypeHTTPS query for domain x.com.
[Info] proxy/dns: rejected type TypePTR query for domain 22.0.168.192.in-addr.arpa.
[Info] proxy/dns: rejected type TypeSVCB query for domain _dns.resolver.arpa.
```

官方文档写得很清楚：内置 DNS **只支持 A / AAAA**（CNAME 会追到 A/AAAA 为止），
其余查询类型交给 DNS 出站决定「丢弃还是透传」。

**我们刻意不给 `dns-out` 加 `settings`**，于是走内核默认行为：立刻回一个
**0 答案**的响应。用一个隔离实例（`dokodemo-door` 收 DNS → `dns-out`，不碰 TUN、
不改系统网络）实测三种配法：

| `dns-out` 的配置 | TYPE65 的响应 | 内核日志 | 结论 |
|---|---|---|---|
| **不配（当前）** | `NOERROR, ANSWER: 0`，**1ms** | `rejected type` ×1 | ✅ 客户端立刻回退去问 A |

> ⚠️ **两处复测的口径不一致，别把这句话写强。** 2026-09 用同一手法在
> **稳定版 26.3.27** 上复测，TYPE65 与 TXT 拿到的是 **`rcode=5 REFUSED`、0 答案、约 0ms**
> （不是 NOERROR）。代码路径能对上：`dns-out` 不配 `settings` 时 `nonIPQuery` 取默认
> `"reject"`，而 `rejectNonIPQuery` 用的是 `dnsmessage.RCodeRefused`
> （`proxy/dns/dns.go`）。
>
> **对用户行为的影响是一样的**（客户端拿到瞬时空答案 → 不启用 h3 → 回退问 A/AAAA），
> 所以结论不变；但"空 NOERROR"这个说法在 26.3.27 上**不成立**。
> 谁再动这一段，请用一个可复现的探测脚本把 rcode 打出来，别只写文字。
| `nonIPQuery: "drop"` | 不回包，客户端**等到超时** | 无 | ❌ 更卡；且该字段已 deprecated |
| `rules` + `direct` 到 `223.5.5.5` | `NOERROR, ANSWER: 0` | 无 | ❌ 1ms 变一次真实上游往返，且 223.5.5.5 同样不提供 HTTPS RR |

也就是说这条日志代表的是「这个类型我们不处理，你问 A 吧」——**正确且最快**。
`nonIPQuery` 还会让内核在启动时打一条 `This feature ... is deprecated` 警告。

它出现在界面「错误」页签里，是**我们自己的问题**，与 DNS 无关：
`classify_log` 原先纯按关键字判级（消息里含 `rejected` / `failed` 就算错误），
完全无视内核写在行首的 `[Info]`。已改为**先信 `[Level]` 标记**，没有标记才退回
关键字。同类噪音还有 `[Info] ... write: broken pipe`（浏览器提前断开 keep-alive
连接），也是正常 churn。

---

## 7. TUN 模式下的端到端 DNS 路径

```
应用 → 系统解析器 (198.18.0.2)
         │
         │ 目标在 198.18.0.0/15，被 0.0.0.0/1 送进 utun4
         ▼
      utun4 ──(gVisor 协议栈)──▶ Xray 的路由决策
                                    │
                                    │ rule[0]: port 53 → dns-out
                                    ▼
                              内核 DNS 模块
                                    │
                          ┌─────────┴─────────┐
                     本地解析器            远端 DoH
                    (223.5.5.5)      (https://1.1.1.1/dns-query)
```

这条路径有三个值得注意的点：

1. **全程不需要 root 占用 53 端口。** 哨兵地址让查询自然流进隧道，
   比「用 pf 把 53 重定向到本机」简单得多，也不会和用户已有的
   DNS 服务（如 AdGuard Home、dnsmasq）抢端口。
2. **`dns-out` 同时服务 UDP 与 TCP 查询。** DNS 响应超过 512 字节时
   客户端会改用 TCP，规则里的 `port: 53` 不限协议，两者都能命中。
3. **Flush 缓存是必要的。** 切换 DNS 策略后 `mDNSResponder` 可能还在用
   旧结果。helper 在改完 DNS 后会执行
   `dscacheutil -flushcache` + `killall -HUP mDNSResponder`（尽力而为，失败不阻断）。

---

## 8. 出站绑定：最干净的防环手段

配置生成时会给 TUN 入站填上 `autoOutboundsInterface`：

```jsonc
"settings": {
  "autoOutboundsInterface": "en0"   // 物理网卡
}
```

它的作用是注册一个全局 dialer controller，让核心**所有**出站 socket
绑定到该接口。在 macOS 上 Xray 用 `IP_BOUND_IF` / `IPV6_BOUND_IF` 实现 ——
包在路由决策**之前**就确定了出口网卡，所以「连代理服务器」的流量
根本不会进入隧道。

这比「加一条 host 路由让代理服务器 IP 走物理网关」更可靠，
因为它不依赖 IP 地址（代理服务器用域名时 IP 会变，host 路由需要跟着更新）。

### 8.1 一个真实的事故：只留一种手段，然后就失效了

上面这句话最初写成「两条我们都会做」，但**代码里 `bypass_hosts` 是空的** ——
只留了 `autoOutboundsInterface` 这一条。结果 TUN 模式一开就炸：

```
dial tcp 203.0.113.10:8443: connect: network is unreachable
```

隧道建起来了、路由表看起来也对、但核心连不上自己的服务器，
日志里只有一句 `network is unreachable`。

**根因**（读 `proxy/tun/handler.go` 与 `tun_darwin.go` 确认）：

```go
// IP_BOUND_IF 会把路由查找**限定在该接口上**
unix.SetsockoptInt(fd, unix.IPPROTO_IP, unix.IP_BOUND_IF, iface.Index)
```

路由表当时是这样：

```
0.0.0.0/1    → utun4     ← 匹配 203.0.113.10，但挂在 utun 上
128.0.0.0/1  → utun4
default      → 192.168.0.1 via en0
```

绑定到 en0 之后，查找 203.0.113.10 时最具体的匹配虽然在，**却不在 en0 上**，
被限定的查找直接失败 → `ENETUNREACH`。

**教训不是「IP_BOUND_IF 不可靠」**，而是：

> 当两条独立的保险里有一条**没法验证它到底有没有生效**时，
> 就不能把另一条撤掉。`IP_BOUND_IF` 是核心内部行为，我们从外面看不见它
> 绑没绑上；而 host 路由是我们自己装的，`route -n get` 一查就知道。

事后补的两条：

1. **`bypass_hosts` 必须有服务器 IP**（以及物理网关）。解析节点域名 →
   安装 `203.0.113.10/32 → 192.168.0.1`，排在最前面。
   有了它，无论 scoped 还是 unscoped 查找，最具体的匹配都落在 en0 上。
2. **启动时做差分探测**（`supervisor::start`）：

```
提交路由前：TCP 连一次 node.ip:node.port     → 失败就中止（服务器本身不通）
提交路由后：TCP 再连一次同一个目标            → 失败就立刻回滚
```

两次探测目标相同，中间**唯一变化的只有「默认路由被接管」** ——
于是「前通后不通」必然指向路由问题。没有这个检查，防环失效的后果是
「显示已连接、但什么都打不开」，用户完全无从下手。

**但两条我们仍然都会做**：路由侧（自己装的，可验证）与核心侧
（`IP_BOUND_IF`，不可验证）失效条件不重叠，两个都要有。
区别是现在**不把可验证的那条撤掉**。

### 8.2 `direct` 出站的第二个坑：绑定了但没生效

修完 8.1 之后，代理服务器通了，但**规则里判给「直连」的流量全挂**：

```
proxy/freedom: failed to open connection to tcp:www.gstatic.com:443
  > dial tcp 142.250.197.35:443: connect: network is unreachable
```

（`www.gstatic.com` 走直连不是 bug —— `geosite:cn` 本来就收录了
`gstatic.com` / `apple.com` 这类「国内可直连」的域名。实测确认：
`google.com` / `github.com` / `wikipedia.org` 都正确走了节点。）

直连出站要把 socket 绑到物理网卡才能逃出隧道。Xray 的 tun 入站会通过
`autoOutboundsInterface` 注册一个**全局** dialer controller 做这件事，
源码上看起来完全正确：

```go
// system_dialer.go:129 —— TCP/UDP 两条路径都应用
for _, ctl := range Controllers { ctl(network, address, c) }
```

**但它没有作用到 `freedom` 的 socket 上。**

#### 怎么确认「`IP_BOUND_IF` 本身没问题」

这个问题的难点是：要判断「绑定没生效」还是「绑定生效了但内核不买账」。
造一个「更具体路由挂在别的接口上」的条件需要 root 改路由表 ——
但**现成的对照就有一个**：`127.0.0.0/8` 是挂在 `lo0` 上的路由。

```c
setsockopt(fd, IPPROTO_IP, IP_BOUND_IF, &ifindex, sizeof(ifindex));
connect(fd, /* 1.1.1.1:443 */);
```

```console
裸 socket              → 超时（包发出去了）
IP_BOUND_IF=en0        → 超时（和裸 socket 一样）
IP_BOUND_IF=lo0        → ✗ Network is unreachable (51)
```

**绑到有默认路由的 en0 = 和裸 socket 一样；绑到没有路由的 lo0 = ENETUNREACH。**
所以 `IP_BOUND_IF` 确实生效、确实按接口过滤路由查找 ——
那么绑到 en0 就应该能逃出 `/1` 路由。既然实际没逃出去，
结论就是**这个绑定根本没被应用到 freedom 的 socket 上**。

#### 修法：逐出站显式写 `sockopt.interface`

不再依赖那个全局 controller，直接在生成的配置里给 `direct` 出站写死：

```jsonc
{
  "tag": "direct",
  "protocol": "freedom",
  "settings": {},
  "streamSettings": {
    "sockopt": { "domainStrategy": "UseIP", "interface": "en0" }
  }
}
```

这条路走 `applyOutboundSocketOptions`，**不依赖任何全局状态**，
是确定性的。`autoOutboundsInterface` 保留作为第二道保险
（两者指向同一接口，谁生效都一样）。

> **教训**：当一个机制的生效路径跨越了「我们的代码 → 库的全局状态 →
> 内核」三层时，它的失效是**静默的**。能把它变成一句显式的配置，
> 就不要依赖中间层的全局副作用。

留空时的行为：Xray 在 `autoSystemRoutingTable` 非空且未指定接口时
会强制填 `"auto"`，由它自己从路由表里推断。我们因为不设
`autoSystemRoutingTable`，留空就是真的不绑定 —— 所以
**应用层会在探测到物理出口后自动填入**，用户不需要关心。

### 8.3 第三个坑：`-ifscope` 的默认路由**必须带网关**

8.2 修好之后，代理服务器通了、`direct` 出站也显式绑定了 en0，但**国内站点
依然打不开** —— 而且症状极具迷惑性：隧道「已连接」，外网站点正常，只有国内
站点超时。

核心日志暴露了真相：

```
（修好前）发起出站拨号 : 949     ← 全都在重复拨 1.1.1.1:443
（修好后）发起出站拨号 : 35
```

949 次拨号不是「流量成环」，而是 **DoH 查询一直没结果 → 无限重试**：
`direct` 出站明明绑到了 en0，包却一个都没出去。

#### 根因

接管路由时我们除了装 `/1`，还会装一条**接口作用域**的默认路由，
本意是给「已绑定 en0 的 socket」留一条逃生通道（`RTF_IFSCOPE` 允许
同一目的地按接口各存一条）：

```
（错误写法）
default  link#14  UCScFl  en0  !      ← 没有网关，是链路级路由
```

`scoped` 只说了「从 en0 出去」，**没说下一跳是谁**。于是内核对每个目的
地址都去做一次 ARP —— 而 `1.1.1.1` 显然不在本网段，不会有 ARP 响应。
包发不出去、也不报错，只是静静地消失。对上层表现为「超时」。

正确写法是把网关一起带上：

```
（正确写法）
default  192.168.0.1  UGScIg  en0     ← I = IFSCOPE，有下一跳
```

```console
$ route -n get default
  gateway: 192.168.0.1
  interface: en0
  flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,IFSCOPE,GLOBAL>
```

#### 为什么之前的验证没抓到

`route -n get default` 在两种写法下**都**返回 `interface: en0` ——
没有网关的那条，`gateway:` 字段会退化成 `link#14`。只要检查里
不专门看 `gateway:` 是不是一个真实地址，就会以为路由是对的。

对应到代码，`RouteVia::ScopedInterface` 现在**强制携带网关**，
`plan.rs` 在探测不到 IPv4 网关时**跳过这条路由并告警**，
而不是降级成一条没有下一跳的链路路由：

```rust
// xt-tun/src/plan.rs
RouteVia::ScopedInterface { name, gateway }   // gateway 必填
```

> **教训**：`-ifscope` 只回答「从哪张网卡出」，不回答「下一跳是谁」。
> 路由条目里少一个字段不会报错，只会让包**静默消失**。
> 这类故障必须靠「看计数器」而不是「看路由表长得对不对」来发现 ——
> 所以冒烟测试现在会统计出站拨号次数，并对「只有节点出站、
> 没有直连出站」单独告警。

#### 三道防线的最终形态

| 层 | 机制 | 失效时可见吗 |
|---|---|---|
| 路由 | `server/32 → 物理网关` host 路由 | ✅ `route -n get` 可查 |
| 路由 | `default → <网关> -ifscope <物理网卡>` | ✅ `netstat -rn` 看得到网关 |
| 核心 | 每个出站显式 `sockopt.interface` | ❌ 只能靠日志与拨号计数 |

### 8.4 换网会让这三道防线**同时失效**

上面三样全都是**按连接那一刻的物理出口**写死的：网关进路由、网卡进
`sockopt.interface`、核心的 DoH 长连接建在那条路径上。所以换网
（换 WiFi / 插网线 / 开热点 / 路由器重发 DHCP）之后：

```
旧网关的路由   → 指向一个已经不在的下一跳，包静默消失
旧网卡的绑定   → 那张网卡已经不是出口（或名字变了）
旧 DoH 连接    → 被从脚下抽走
```

内核不会因此报任何错（丢掉的路由条目不会报错，只会让包消失）。
日志里能看到的唯一线索是：

```
app/dns: failed to retrieve response for query.ess.apple.com.
  > Post "https://1.1.1.1/dns-query": io: read/write on closed pipe
```

**三种尾巴的含义完全不同，必须分开看**：

| 日志尾巴 | 含义 | 是谁的错 | 处置 |
|---|---|---|---|
| `context deadline exceeded` | 解析器在超时时间内没答 | 节点/解析器（网络抖动） | 等，或换解析器 |
| `io: read/write on closed pipe` | 连接**被人从脚下抽走** | 隧道那层的生命周期 | **断开重连** |
| `context canceled` | **请求方自己放弃了** | 谁问的谁放弃（不是解析器） | 一般不用管 |
| `unexpected EOF` | 连接**建起来了，但应答读到一半被截断** | 链路中间有人掐断（节点/中间设备） | 偶发不用管；**频繁**说明那条链路在重置长连接 |

四种尾巴对应四种完全不同的问题。只看到 `context deadline exceeded` 就以为是
"DNS 坏了"，和看到 `unexpected EOF` 就去换解析器，都是找错方向。

`context canceled` 值得单独说：它不是解析失败，而是「问了之后又不要了」。
两种常见触发：

* 应用/浏览器在答案回来之前就关掉了连接（用户切走了、页面关了）；
* **核心正在停机或重载配置** —— 此时所有在途查询会被一起取消，
  于是日志里会**一次性刷出一片** `context canceled`。

第二种是生命周期造成的，和我们自己的「停止 / 重连」直接对应。判断方法很简单：
看有没有一条对应的会话重建（helper 的 `status` 里会话 id 会变）。
如果一大批 `canceled` 出现在你刚点过重连的前后，那就是它，不用管。

应用现在会在连接后每 5 秒比一次「网卡 + 网关」，变了就报一句
`物理出口已变化（旧 → 新），隧道不再有效，请断开后重新连接`。

**刻意不自动重连**：拆掉再重建 TUN 是全项目最危险的动作，而换网时网络
常会抖几下，自动重连会跟着来回拆建 —— 风险大于收益。网络稳定后手动重连
才真的有效。

---

## 9. 测试覆盖

| 测试 | 钉住的结论 |
|---|---|
| `compile_skips_disabled_and_resolves_selected_tag` | 禁用规则被跳过；`Proxy{outbound:None}` 被替换为当前节点 tag |
| `catch_all_rule_has_no_conditions` | 无条件规则只有 3 个字段 |
| `bypass_mainland_orders_ads_before_cn` | 预设顺序 |
| `port_matcher_matches` | `80,443` / `1000-2000` 两种写法的解析 |
| `dns_hijack_is_the_first_rule` | DNS 劫持排在规则数组第 0 位 |
| `dns_split_by_rule_uses_domain_scoped_servers` | 分流解析生成了大陆 / 非大陆两条带 `domains` 的解析器 |
| `split_dns_servers_have_no_expect_ips` | 分流解析器**不带** `expectIPs`（它会丢弃正确结果，见 §6.2） |
| `default_dns_mode_is_split_by_rule` | 默认 DNS 模式是按规则分流 |
| `migrate_moves_proxy_dns_to_split_by_rule` | 0.1.0 的 `proxy` 设置会被迁移，且报告改了什么 |
| `migrate_leaves_explicit_direct_dns_alone` | 迁移只动旧默认值，不改用户显式选择 |
| `bypass_mainland_proxies_google_between_ads_and_cn` | Google 规则必须夹在广告拦截与大陆直连之间 |
| `native_tun_inbound_is_emitted_for_tun_profile` | TUN 入站字段与 `/1` 拆分路由 |
| `fakedns_pool_and_first_dns_server_when_enabled` | Fake-IP 在 DNS 列表第一位 + `destOverride` 生效 |
| `unsupported_type_is_named_in_the_warning` | 被跳过的节点告警里带**实际收到的 `type`**（防静默丢弃） |
| `transport_matches_kind` | 国内候选走明文直连、国外候选走 DoH 经节点（写反了会得到一组看起来正常、实际无意义的数字，见 §6.7） |
| `pool_has_enough_foreign_candidates` | 国外候选 ≥ 3，否则多数派投票这一半判据等于没有 |
| `pool_is_clean` | 候选池只放 IP 或 **IP 形式的** DoH 端点（域名会引入自举依赖） |
| `foreign_group_is_not_probed_without_node` | 节点未连接时国外组标「未探测」，**不谎报「不通」** |
| `curl_doh_args_carry_proxy_and_timeout` | DoH 探测确实带上 `--socks5-hostname` 与 `--max-time` |
| `merge_ranked_keeps_user_servers_after_probed_ones` | 自动排序保留用户手填的解析器（只调池内项顺序） |
| `merge_ranked_is_scoped_to_its_own_kind` | 国内组只动 `direct_servers`、国外组只动 `remote_servers` |
| `foreign_group_is_probed_serially` | 国外组串行探测（并发会测到节点排队并**改变名次**，见 §6.7） |
| `dns_outbound_has_no_settings_on_purpose` | `dns-out` **刻意不配** `settings`：非 A/AAAA 的快速空 NOERROR 比任何显式配置都好（见 §6.8） |
| `split_dns_keeps_every_candidate_as_same_tier_fallback` | 同一条 `domains` 下的候选**全部**进配置，国外超时后仍回退到国外（见 §6.2） |
| `split_dns_falls_back_when_lists_are_empty` | 列表被清空时仍写出硬编码兜底 |
| `egress_change_is_detected_by_interface_or_gateway` | 换网（换网卡或换网关）必须被识别出来（见 §8.4） |

### 9.1 冒烟测试：唯一会真改系统网络的测试

单元测试证明不了「回滚之后路由表真的回到原样」—— 那需要真的改一次再改回来。
所以有一个端到端测试，跑的是 App 点「连接」时的**同一份** `Supervisor::start`：

```bash
cargo run -p xraytun-desktop --example tun_smoke -- --dry-run          # 只看计划，不改动
cargo run -p xraytun-desktop --example tun_smoke -- --yes-i-understand
```

除了逐项比对改动前后的默认路由 / 接管路由 / DNS，它还检查三件
**只有真跑才暴露得出来**的事：

| 检查 | 不检查会怎样 |
|---|---|
| `curl` 外网站点并打印出口 IP 与地区 | 「已连接」，但其实是直连出去的 |
| `curl` **国内**站点（§8.2 / §8.3 的回归项） | 国内打不开，而只看外网却是通的 |
| 接口作用域默认路由的**网关字段**（§8.3） | 网关是 `link#N` 时包静默消失，`route -n get default` 查不出来 |
| 出站拨号次数、「只有节点出站没有直连出站」告警 | 路由成环只表现为「很慢」，没有任何报错 |

---

## 9.1 判定「这个地址会走哪条规则」

界面要回答「这个网站为什么走代理/直连」，就必须能算出某条规则是否命中。
这带来两件事：读 `geosite.dat` / `geoip.dat`，以及**与真实核心对拍**。

### 9.1.1 数据文件格式

两个文件都是 protobuf（字段号是**实测**出来的，不是照抄）：

```text
GeoSiteList { repeated GeoSite entry = 1 }
GeoSite     { string country_code = 1; repeated Domain domain = 2 }
Domain      { Type type = 1; string value = 2 }
            Type: Keyword=0  Regex=1  Domain=2  Full=3

GeoIP       { string country_code = 1; repeated CIDR cidr = 2 }
CIDR        { bytes ip = 1; uint32 prefix = 2 }
```

两个容易踩的点：

* **类别名是大写**（文件里是 `CN`），而规则里写的是 `geosite:cn`。
  比较时统一转大写。
* **`geoip` 的地址是原始字节**（4 或 16 字节），不是字符串 ——
  按字符串解会得到乱码并静默不命中。
* 类型分布实测：`Domain` 五十万条、`Full` 七千多条、`Regex` **仅 371 条**
  （0.07%）。所以自带一个只支持实际出现写法的极简正则即可，
  **编译不了的写法明确不命中**，不按别的语义悄悄匹配。

内存上只保留**被规则引用的类别**（当前 6 个 geosite + 2 个 geoip），
其余流式跳过 —— 峰值内存与文件大小无关。

### 9.1.2 匹配语义

| 类型 | 语义 | 易错点 |
|---|---|---|
| `Domain` | 后缀匹配 | **必须落在标签边界**：`google.com` 命中 `www.google.com`，不命中 `notgoogle.com` |
| `Full` | 精确相等 | 不匹配子域 |
| `Keyword` | 子串出现 | —— |
| `Regex` | 正则 | 只支持数据里实际出现的写法 |

域名比较**两边都转小写**：数据里确实存在大写条目，只转一边会「明明在列表里却匹配不上」。

### 9.1.3 一条实测出来的规则语义（容易想当然）

一条规则**同时**写了 `domain` 与 `ip` 时，如果目标只有 IP、没有域名，
这条规则**不命中**。

我一度按「缺少输入的条件就跳过」实现，于是私网地址 `192.168.1.1` 被判成直连
（`preset-private` 的 `geoip:private` 命中）。用真实核心跑同一批查询后发现
它实际把 `192.168.1.1` 送去了**兜底代理** —— 即**缺输入的条件视为不满足**。

判定器已按实测改正，依据写在代码注释里（否则下一个人会像我一样「顺手改回去」）。

### 9.1.4 怎么验证：与真实核心对拍

自己实现规则匹配、再自己写测试，只能证明「与自己的理解一致」。
所以 `scripts/compare-route.py` 拿**用户原样的规则**造一份带 `access.log`
的探针配置，起真实 `xray`，用 `--socks5-hostname` 逐个域名/IP 发请求，
从日志的 `[socks -> 出站]` 读出核心的选择，再与判定器逐条比对。

实测 10/10 一致（baidu/qq→direct、google/gmail→节点、doubleclick→block、
223.5.5.5→direct、私网与 8.8.8.8→兜底节点）。

**这个对拍就是 9.1.3 那条语义的来源。** 改动判定逻辑后请重跑它。

命令行工具：`cargo run -p xt-core --example route_explain -- <config.json> <数据目录> -- <域名...>`

---

## 10. 参考

* `crates/xt-core/src/routing.rs` —— IR 与编译
* `crates/xt-core/src/xray/config.rs` —— DNS 段与入站生成
* `crates/xt-core/src/model.rs` —— `DnsSettings` / `FakeDnsSettings`
* `crates/xt-core/src/routing/geo.rs` —— geosite/geoip 读取与匹配
* `crates/xt-core/src/routing/explain.rs` —— 路由判定
* `scripts/compare-route.py` —— 与真实核心对拍
* `crates/xt-core/src/routing/mod.rs` —— IR 与编译（含 `geo` 子模块）
