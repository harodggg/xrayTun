# 03 · Xray 集成

本文档记录与 Xray-core 的具体契约。**所有版本相关的事实都读取了上游源码核实**
（通过 codeload 拉取 tag 归档 + 读取 `proxy/tun/*`、`infra/conf/*`、
`app/*/command/*.proto`），并标注了核实方式。

---

## 1. 最重要的一个事实：Xray 有原生 TUN 入站

这一点改变了整个项目的架构。

```
"protocol": "tun"，内置基于 gVisor 的用户态 TCP/IP 协议栈
（proxy/tun/stack_gvisor.go，无 build tag，全平台编译）
```

所以本项目**不需要** tun2socks / hev-socks5-tunnel 之类的旁路进程。
数据面就在 Xray 进程里，UDP / QUIC / ICMP 的支持由 gVisor 协议栈统一提供。

### 1.1 版本演进（逐个 tag 核对过）

| 版本 | 日期 | macOS 上的能力 |
|---|---|---|
| < v26.1.18 | — | 没有 `proxy/tun/`，`"protocol": "tun"` 直接启动失败 |
| v26.1.13 | 2026-01-13 | 有 `proxy/tun/`，但**没有** `tun_darwin.go`（只有 Win/Linux/Android） |
| v26.1.18 | 2026-01-18 | 首次出现 `tun_darwin.go`：只建卡 + 设 MTU，**不配地址、不装路由** |
| **v26.1.31** | 2026-01-26 | 补齐地址/路由编程，并开始支持 `XRAY_TUN_FD` |
| v26.9.9 | 2026-09-08 | 当前最新 |

**因此版本下限是 `>= 26.1.31`**（常量
`xt_core::xray::MIN_CORE_VERSION_NATIVE_TUN`），实践上建议 `>= 26.9.x`。

在 v26.1.18 ~ v26.1.30 这一段，TUN 入站能建卡但不会配置网络 ——
用户必须自己跑 `ifconfig` / `route`。如果我们的代码把这一区间当成「支持」，
表现会是「核心起来了、网卡也在、但完全不通」，很难定位。
所以启动前会显式比对版本，不满足就拒绝启动并给出明确提示。

### 1.2 配置字段（`infra/conf/tun.go`）

```go
type TunConfig struct {
    Name                   string   `json:"name"`
    Desc                   string   `json:"desc"`
    MTU                    uint32   `json:"mtu"`
    Gateway                []string `json:"gateway"`
    DNS                    []string `json:"dns"`
    UserLevel              uint32   `json:"userLevel"`
    AutoSystemRoutingTable []string `json:"autoSystemRoutingTable"`
    AutoOutboundsInterface *string  `json:"autoOutboundsInterface"`
}
```

| 字段 | 默认 | 我们的用法 |
|---|---|---|
| `name` | 随机 `utunN`（10..1024） | 留空，让内核挑，避免与用户已有的 utun 冲突 |
| `mtu` | 1500 | 透传用户设置 |
| `gateway` | `169.254.10.1/30` | 设为 `198.18.0.1/15`。**macOS 只取第一个 IPv4 前缀** |
| `dns` | — | **Windows 专用，macOS 上被忽略**。不要指望它 |
| `autoSystemRoutingTable` | 空 | **始终留空**，路由由 helper 负责（理由见 1.3） |
| `autoOutboundsInterface` | `null` | 填入物理接口名（如 `en0`），用于防路由环 |

两个容易误解的点：

* **`gateway` 里的主机位是有意义的。** `169.254.10.1/30` 的含义是
  「把 `.1` 作为点对点对端地址，本机用 `.2`」。这也正是为什么
  `xt_proto::Cidr` **不**在构造时归一化主机位（见 §4 的那个 bug）。
* **`autoOutboundsInterface` 不是 DNS 相关配置。** 它注册一个全局 dialer
  controller，让核心**所有**出站 socket 绑定到指定网卡；在 darwin 上走
  `IP_BOUND_IF` / `IPV6_BOUND_IF`。

### 1.3 为什么不用 `autoSystemRoutingTable`

因为它会和 helper 的路由管理**冲突**。

Xray 的实现是：`Start()` 时用 `AF_ROUTE` socket 写入 `RTM_ADD`，
`Close()` 时倒序删除。看起来很完整。但如果同时存在两套：
helper 的快照记的是自己装的路由，Xray 崩溃时走它的 `Close()` 删自己的 ——
一旦有一方没跑到清理逻辑，就会留下**没人认领的路由**，
而用户看到的现象是断网，且没有任何线索指向是谁留下的。

所以规则是：**谁持有快照，谁负责路由。** 那个角色是 helper。

顺带一提，Xray 对「默认路由」做了保护性展开：
`0.0.0.0/0` 会被拆成 `1.0.0.0/8, 2.0.0.0/7, 4.0.0.0/6, ...` 八条。
思路和我们的 `/1` 拆分一致（都不动原默认路由），只是粒度不同。
我们用自己的实现，是为了让快照能精确记录每一条装了什么。

---

## 2. Fake-IP：Xray 原生就有

一个常见误解：Fake-IP 是 sing-box 独有的。**不是。**

| | sing-box | Xray |
|---|---|---|
| 地址池配置 | `inbounds[].type: tun` + `fakeip` | 顶层 `fakedns` 段 |
| 生效方式 | `dns.rules[].action: fakeip` | `sniffing.destOverride: ["fakedns"]` 或 `["fakedns+others"]` |

`fakedns+others` 的语义是：先尝试把假地址还原成域名，不是假地址时
再走常规的 SNI / Host 嗅探 —— 与 sing-box 的 fakeip + sniffing 组合等价。

### 2.1 它解决什么问题

客户端在拿到域名解析结果**之前**就可能把连接发出来了，此时只能看到 IP，
无法按域名分流。Fake-IP 让 DNS 立刻返回一个假地址，内核再把假地址还原成域名，
于是域名分流对「直接用 IP 发起连接」的程序也能生效。

### 2.2 为什么默认关闭

Fake-IP 会污染本机 DNS 缓存。隧道路径关闭后的一段时间内可能出现
「网络无法访问」，需要等缓存过期或手动 `sudo killall -HUP mDNSResponder`。

上游文档自己也警告了这一点。所以我们把它做成显式开关，并在 UI 上
把这段警告和开关放在一起（见 `apps/ui/src/pages/Settings.tsx`）。

### 2.3 实现要点

启用时必须做两件**顺序敏感**的事：

1. 在 `dns.servers` 的**最前面**插入 `{"address": "fakedns"}` ——
   放在后面就永远轮不到它，域名分流静默失效；
2. 在所有入站的 `sniffing.destOverride` 里改用 `["fakedns+others"]`。

对应测试：`config::tests::fakedns_pool_and_first_dns_server_when_enabled`。

---

## 3. 配置生成的固定骨架

```jsonc
{
  "log":       { "loglevel": "warning", "access": "", "error": "" },
  "api":       { "tag": "api", "services": ["HandlerService", "LoggerService",
                                            "StatsService", "RoutingService"] },
  "dns":       { "hosts": {...}, "servers": [...], "queryStrategy": "UseIP" },
  "fakedns":   { "ipPool": "198.18.0.0/16", "poolSize": 65535 },   // 可选
  "inbounds":  [ socks, http, api, tun? ],
  "outbounds": [ node-*, direct, block, dns-out, api ],
  "routing":   { "domainStrategy": "IPIfNonMatch", "rules": [...] },
  "policy":    { "levels": {...}, "system": {...} },
  "stats":     {}
}
```

### 3.1 DNS 劫持规则必须排第一

```jsonc
{ "type": "field", "port": "53", "outboundTag": "dns-out", "ruleTag": "internal-dns-hijack" }
```

如果它排在用户规则之后，任何一条 catch-all（甚至用户自己写的
`port: "53" → direct`）都会先命中，DNS 就再也到不了 `dns-out`。
测试 `dns_hijack_is_the_first_rule` 钉住了这个位置。

`dns-out` 是 `"protocol": "dns"` 的出站：被路由到它的 DNS 查询由内核
DNS 模块直接应答。它只处理经典明文 DNS（UDP/TCP），**不支持 DoH/DoT**——
但那没关系，因为 DoH 的流量本身就是 HTTPS，会正常走代理。

### 3.2 SOCKS 入站必须开 UDP

```jsonc
"socks": { "auth": "noauth", "udp": true, "userLevel": 0 }
```

TUN 模式下的 UDP 中继完全依赖 SOCKS5 的 UDP ASSOCIATE。
不开的话 QUIC 会静默失败（浏览器自动回退到 TCP，用户察觉不到），
DNS 直连查询也一样。表现为「能用但某些网站很慢」。

### 3.3 API 的安全考虑

管理 API 绑定 `127.0.0.1:10085`，并额外加一条路由规则把 `inboundTag: ["api"]`
送到 `api` 出站。

* 端口 10085 **不是 Xray 的默认值**（Xray 没有默认端口，`listen` 缺省为空），
  它只是官方文档与社区约定的示例值。我们沿用它是为了让用户查文档时不会困惑。
* API 只监听回环。它没有认证 —— 任何本机进程都能连上它并查询统计。
  这在单用户桌面上是可接受的，但**不要在多人共用的机器上把它暴露到 0.0.0.0**。

---

## 3.4 延迟探测：两个指标，不是一个数

延迟探测起一个**独立的核心实例**（每节点一个 SOCKS 入站 + 一条专属路由规则），
逐个节点探测。这里曾经只报一个数，而那个数**不是延迟**。

### 一个数混了两段路

原来的做法是：经节点请求 `http://cp.cloudflare.com/generate_204`，量首字节时间。
这个数包含

```
本地 → 服务器   +   服务器 → Cloudflare → 回来
```

后一段取决于**服务器离最近的 CF PoP 有多远**，和节点好坏无关。实测（香港节点）：

| 量法 | 中位数 |
|---|---|
| 本地 → 服务器（纯 TCP 握手） | **59 ms** |
| 经节点 → Cloudflare（原来报的数） | **196 ms** |

**136 ms 是后一段，占报出数字的 70%。**

### 为什么这不只是精度问题

它会让排序**反过来**。假设服务器→CF 这段是 137ms：

```
香港节点   你→HK  59ms + HK→CF 137ms = 196ms
美国节点   你→US 150ms + US→CF  20ms = 170ms   ← 报出来"更快"
```

离用户近 3 倍的节点被报成更慢。**因为掺进了「服务器离 Cloudflare 有多远」。**

### 现在拆成两个

| 指标 | 怎么测 | 回答什么 |
|---|---|---|
| **延迟** `server_rtt_ms` | 到 `server:port` 的 TCP 握手，3 次取**中位数** | 我离这台服务器多远 |
| **可用性** `available` | 经节点到 `generate_204`，只取成功/失败 | 这个节点到底能不能用 |

三点设计取舍：

* **取中位数而不是单次**：实测同一路径单次采样有 1.3 倍抖动
  （172 / 191 / 224ms），两个节点只差十几毫秒时单点值分不出来。
  首次探测还可能撞上冷 DNS 缓存（探测配置里没有 `dns` 段，靶点域名
  要走系统解析器绕回主核心的 DNS 模块，再经 DoH 走节点）。
* **RTT 不经过核心**：直接对服务器发 TCP 握手。所以即使探针核心启动失败，
  延迟这一项依然有值 —— 这两件事本来无关（核心起不来是本地问题，
  不代表服务器远或近）。
* **可用性不以 RTT 为准**：墙下的 RST 注入会让 TCP 握手又快又失败，
  伪造的 SYN-ACK 会让它又快又"成功"。只有真取一次数据才算数。
  实测 `:41529` 返回 `ECONNREFUSED`，我们报 `延迟 = —`、`可用性 = 不可用`，
  而不是报一个"5ms 很快"。

### 并发度从 8 降到 4

同一个核心上并发探测会互相抢 CPU（REALITY 握手是 CPU 密集的）和上行。
实测：

```
串行 1   中位 192ms  (185–208)
并发 8   中位 221ms  (182–250)
```

**+15%，而且节点越多每个数字越差** —— 纯属自伤。已降到 4。

## 4. 一个被单元测试抓到的模型 bug：CIDR 归一化

`Cidr` 最初的实现在构造时就清零主机位，理由是「`route(8)` 对 `10.1.2.3/8`
的行为依赖实现细节，明确归一化更稳妥」。

这个理由对**路由目标**成立，但对**接口地址**是错的：

```
198.18.0.1/15  ──归一化──▶  198.18.0.0/15
      ▲                            ▲
   接口自己的地址            这是网络地址
```

把网络地址配到网卡上，以及把 `198.18.0.0/15` 当作 Xray 的 `tun.gateway`
（期望的语义是「`.1` 是地址，`.2` 是本机」，见 §1.2），都是错的。

修复方式是把「归一化」从构造器里挪到显式方法：

| 用途 | 方法 |
|---|---|
| 接口地址 / `tun.gateway` | 直接用 `cidr.addr`（保留主机位） |
| 路由目标 | 先调 `cidr.network()` 再交给 `route(8)` |

测试 `cidr_preserves_host_bits` 与 `network_clears_host_bits` 钉住了两种语义。

**教训**：当同一个类型被用在两种语义不同的位置时，隐式的「规范化」
一定会在一侧产生静默错误。让它显式，编译器就会帮你在每个调用点提醒一次。

---

## 5. 版本下限与退路

`XRAY_TUN_FD` 在 macOS 上生效，但**上游文档只为 iOS / Android / Linux 承诺
这条路径**，darwin 分支共享代码是 Go build tag（`//go:build darwin`
同时覆盖 `GOOS=darwin` 与 `GOOS=ios`）带来的副作用。

这意味着一个上游重构（例如把 iOS 分支拆成独立的 build tag）可能
在某次升级后让 fd 交付失效。

### 5.1 我们已经做的准备

* `DatapathPlan` 有三种模式（见 [02](02-tun-and-privileges.md#24-三种数据面模式)），
  `SpawnDatapath { use_helper_fd: false }` 是完全公开的上游路径：
  让 Xray 自己建卡、自己配地址、自己装路由，helper 只负责 DNS 与快照。
* 启动时把 `core version` 写进日志与诊断报告，版本问题一眼可查。

### 5.2 检测与降级策略（待实现）

```
启动 TUN
  ├─ 用 HandoffFd 拉起核心
  ├─ 等 SOCKS 端口（10s）
  │    成功 → CommitRoutes → 完成
  │    失败 → 检查核心日志里是否出现 fd 相关错误
  │           是 → 记一条诊断信息，自动用 SpawnDatapath{use_helper_fd:false} 重试一次
  │           否 → 按普通启动失败处理
```

这一步目前只在文档里，代码里留了 `DatapathPlan` 的第三种模式作为接入点。
见 [07-roadmap-and-risks.md](07-roadmap-and-risks.md)。

---

## 6. 为什么不用 gRPC 热更新

Xray 提供完整的 gRPC 管理 API（`HandlerService.AddInbound` /
`AlterInbound`、`StatsService.QueryStats`、`ObservatoryService` 等），
可以在不重启的情况下改配置、读统计。

我们**没有**用它，理由：

* **收益有限。** 桌面客户端切换节点/规则的频率是分钟级，Xray 冷启动
  100~300ms，用户感知不到。
* **成本明确。** 用 gRPC 就要引入 `tonic` + `prost` + `protoc`（或用
  `prost-build` 的手写替代），而 `command.proto` 有一整条 import 依赖链
  （`app/proxyman/config.proto`、`common/protocol/*`、`core/config.proto`…）。
  这些定义与内核版本强耦合 —— 内核升级改了字段，我们的构建就会断。
  对一个「跟着上游跑」的客户端来说这是持续的维护负担。
* **可复现性更好。** 配置完整落盘后，用户可以
  `xray run -c ~/.../runtime/config.json` 直接复现问题。
  热更新模式下没有这样一个「现场」。

### 6.1 什么时候应该改主意

如果要做下面任何一件事，gRPC 就从「可选」变成「必需」：

| 需求 | 需要的能力 |
|---|---|
| 界面上显示真实的分节点上下行流量 | `StatsService.QueryStats` |
| 免重启切换节点（毫秒级） | `HandlerService.AlterOutbound` 或 balancer |
| 用内核自带的主动健康检查 | `ObservatoryService.GetOutboundStatus` |
| 实时看「当前有哪些连接」 | `StatsService.GetStatsOnline` + `RoutingService.SubscribeRoutingStats` |

### 6.2 接入时的注意事项（已核实）

如果将来接入，下面这些是**读上游 proto 得到的准确信息**，
网上流传的不少版本是错的：

| 服务 | 完整包名 |
|---|---|
| `HandlerService` | `xray.app.proxyman.command` |
| `StatsService` | `xray.app.stats.command` |
| `LoggerService` | ⚠️ **`xray.app.log.command`**（不是 `xray.app.logger.command`） |
| `RoutingService` | `xray.app.router.command` |
| `ObservatoryService` | ⚠️ **`xray.core.app.observatory.command`**（注意 `core` 段） |

另外两条容易踩的：

* **v1.8.12 起有「简单模式」**：`api` 对象里直接给 `listen` 就会自己起监听，
  不需要再配 `api` 入站和 `api` 路由规则。我们当前用的是兼容性更好的旧形式。
* **`burstObservatory` 不是「按需触发」的。** 它是随机周期的
  （每个 `interval × sampling` 周期内为每个 outbound 随机挑一个时刻发一次探测），
  `interval` 最小 10s —— 也就是说**最快也要几秒才能拿到结果**，
  不适合做「用户点一下立刻出延迟」的功能。
  我们的延迟探针用「临时核心 + 每节点独立 SOCKS 端口」正是因为这个限制
  （见 [01](01-architecture.md#43-延迟探测)）。
  `ObservatoryService` 只暴露 `GetOutboundStatus` 查询，没有「立即探测」的 RPC。

---

## 7. 发布产物与许可证

### 7.1 产物内容

macOS arm64 的官方产物是 `Xray-macos-arm64-v8a.zip`，解压后是：

```
xray            ← 唯一的可执行文件
geoip.dat       ← 路由规则集（geoip:cn 等依赖它）
geosite.dat     ← 域名规则集（geosite:cn 等依赖它）
README.md
LICENSE
```

**不是「单个二进制」。** `geoip.dat` / `geosite.dat` 是必需的 ——
配置里的 `geoip:cn` / `geosite:cn` 会在运行期去读它们，
缺失时核心的行为是「规则不命中」，也就是静默地不分流。

**已实现的做法**：启动核心时通过环境变量 `XRAY_LOCATION_ASSET`
把 `.dat` 所在目录告诉它，而不是复制文件或依赖工作目录。

```rust
// apps/desktop/src/supervisor.rs
if let Some(dir) = core_path.parent() {
    if dir.join("geoip.dat").is_file() || dir.join("geosite.dat").is_file() {
        envs.push(("XRAY_LOCATION_ASSET", dir.display().to_string()));
    } else {
        tracing::warn!(/* 明确警告：规则将不会命中 */);
    }
}
```

选这个方案而不是「把 `.dat` 复制到 runtime 目录」：

* 复制要在核心版本变化时重新复制，多一处状态需要同步；
* `.dat` 合计约 28 MB，两份拷贝是浪费；
* 环境变量是 Xray 官方支持的定位方式，语义明确。

**但目录里没有 `.dat` 时会打一条 `warn`** —— 因为这种失败是静默的，
必须让它在日志里可见。

### 7.2 许可证

| 组件 | 许可证 | 注意事项 |
|---|---|---|
| Xray-core | **MPL-2.0** | 以独立进程调用，不链接、不修改，不构成衍生作品。分发时需附带其 LICENSE |
| `XTLS/libXray` | MIT | 本项目未使用（那是 gomobile 绑定） |
| 本项目代码 | MIT | — |
| React / Vite / Tauri | MIT / Apache-2.0 | 见 `apps/ui/package.json` 与 `Cargo.lock` |

**注意**：MPL-2.0 是「文件级 copyleft」。只要我们不修改 Xray 的源文件、
只是分发官方二进制，就没有额外义务。如果我们将来要 patch Xray 并重新编译，
被修改的文件必须以 MPL-2.0 公开。

---

## 8. 参考

* `crates/xt-core/src/xray/config.rs` —— 配置生成与版本常量
* `crates/xt-core/src/xray/process.rs` —— 进程生命周期
* `crates/xt-core/src/xray/probe.rs` —— 延迟探针
* `crates/xt-core/src/subscription/` —— 四种订阅格式的解析
