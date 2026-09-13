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

### 6.1 `Proxy` —— 全部走代理解析

```jsonc
"servers": ["https://1.1.1.1/dns-query", "https://dns.google/dns-query", "localhost"]
```

抗污染，解析结果与出口位置一致（访问 Google 会拿到就近的海外节点）。
缺点是国内 CDN 会解析到很远的地方 —— 看国内视频可能变慢。

### 6.2 `SplitByRule` —— 按规则分流解析

```jsonc
"servers": [
  { "address": "https://1.1.1.1/dns-query",
    "domains": ["geosite:geolocation-!cn"], "expectIPs": ["geoip:!cn"] },
  { "address": "223.5.5.5",
    "domains": ["geosite:cn"], "expectIPs": ["geoip:cn"] },
  { "address": "223.5.5.5" },                        // 兜底
  { "address": "https://1.1.1.1/dns-query" },        // 兜底
  "localhost"
]
```

关键在于 `domains` 字段让**内核按域名选解析器**：
大陆域名用国内 DNS 直连解析（拿到就近 CDN），其余走远端加密 DNS。

`expectIPs` 是第二道校验：如果解析结果不落在期望的 IP 段内，
就认为这次解析不可信（可能是 DNS 投毒），改用后面的解析器重试。

### 6.3 `Direct` / `Custom`

`Direct` 全部用本地解析器；`Custom` 把两组列表拼起来交给用户自己负责。
两者都用于排障或特殊网络环境。

### 6.4 `disableCache` 与 `queryStrategy`

* `queryStrategy`: `UseIP` / `UseIPv4` / `UseIPv6`。默认 `UseIP`
  （双栈时按系统偏好）。纯 IPv6 环境下需要改。
* `disableCache`: 默认 `false`。诊断「DNS 结果是不是被缓存住了」时临时打开。

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
| `native_tun_inbound_is_emitted_for_tun_profile` | TUN 入站字段与 `/1` 拆分路由 |
| `fakedns_pool_and_first_dns_server_when_enabled` | Fake-IP 在 DNS 列表第一位 + `destOverride` 生效 |
| `unsupported_type_is_named_in_the_warning` | 被跳过的节点告警里带**实际收到的 `type`**（防静默丢弃） |

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

## 10. 参考

* `crates/xt-core/src/routing.rs` —— IR 与编译
* `crates/xt-core/src/xray/config.rs` —— DNS 段与入站生成
* `crates/xt-core/src/model.rs` —— `DnsSettings` / `FakeDnsSettings`
