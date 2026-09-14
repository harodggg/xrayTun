# 更新记录

## 0.5.2

### 修复：国外解析器超时后，会**回退到国内解析器**去解析被墙域名

`servers` 的回退分两层：「先命中 `domains` 的，再其余的」。所以**同一条
`domains` 下有几个候选，就决定了这一层有几个备选**。而早期实现对每个列表
只写第一个（`first_or`），这一层永远只有一个备选。

真实后果（用户日志）：

```
[Error] app/dns: failed to retrieve response for alive.github.com.
        > Post "https://1.0.0.1/dns-query": context deadline exceeded
```

`1.0.0.1` 一超时，回退链上下一个就是**国内解析器** —— 被墙域名被国内解析器
接着答出来，拿到被污染 / 错误的 IP。

**修法**：把同一条 `domains` 下的候选**按顺序全部写进配置**。隔离实例实测
（把一个国外候选指向必然超时的 `192.0.2.1`，开 `dnsLog`）：

| 结构 | 第二台被查的是谁 |
|---|---|
| 只取第一个 | `UDP:223.5.5.5:53` ← **国内** |
| 全部写进去 | `DOH//1.1.1.1` ← **仍是国外** |

这也顺带让 0.5.0 的「解析器自动选优」真正有意义了：`remote_servers` 里
第 2..n 位过去是白排的（根本不进配置），现在是回退时真会用到的备选。
自动选优的排序因此变成了一个**同层优先级链**，而不只是「换掉第一台」。

## 0.5.1

### 修复：发布流程的打包步骤**从来没成功过**（universal 包根本没出来）

`package-macos.sh` 里有这么一句：

```bash
cargo build --release --workspace $TARGET_FLAG   # TARGET_FLAG="--target universal-apple-darwin"
```

**`universal-apple-darwin` 不是 rustc 的 target。** 它只是 Tauri CLI 的伪
target（Tauri 内部把它展开成「分别编 x86_64 与 aarch64，再 lipo」）。
直接交给 cargo 必失败：

```
error: could not find specification for target "universal-apple-darwin"
```

实测 rustc 1.98：`rustc --print target-list` 里没有它。

后果是发版在「打包（universal）」这一步就死了，**Release 从来没有被创建过**；
之前你拿到的 dmg 全是我在本机用默认宿主架构打的（文件名里的 `_x86_64` 就是
这个意思）。报错藏在编译日志中段，看起来像普通编译失败，所以一直没被发现。

**修法**：universal 时不再把伪 target 交给 cargo，而是照 Tauri 的做法自己做 ——
分别编 `x86_64-apple-darwin` 与 `aarch64-apple-darwin`，再 `lipo` 合成 helper
（helper 是我们自己的 crate，不在 Tauri 的构建范围内，Tauri 只管 .app 里的主程序）。

顺带加了两个东西，让这类问题**下次自己暴露**：

- 其它显式 target 会先过一遍 `rustc --print target-list` 预检，不认识就立刻
  给出可读报错并列出本机已装 std，而不是让它变成编译日志中段一句 cargo 错误。
- `tauri build` 之后断言 .app 确实在预期位置；否则**把实际找到的位置全部列出来**
  （universal 的产物路径依赖 Tauri 的内部布局，猜错时这一步能自己交代清楚）。

> 为什么本地测不出 universal：本机是 Homebrew 的 x86_64 Rust，只有
> `x86_64-apple-darwin` 的 std，编 aarch64 会 `E0463`（找不到 `std`）。
> 这条路只能在 CI 上验证。**所以「发版必须真的跑一次 CI 并确认产物存在」是
> 发版的一部分，不能只看本地打出来的包。**

### 修复：「错误」页签里全是内核的**正常**信息

**症状**：日志页的「错误」页签被这些刷满（调试等级下尤其明显）——

```
[Info] proxy/dns: rejected type TypeHTTPS query for domain x.com.
[Info] proxy/dns: rejected type TypePTR query for domain 22.0.168.192.in-addr.arpa.
[Info] ... app/proxyman/outbound: ... write tcp 127.0.0.1:10808->...: write: broken pipe
```

**根因**：`classify_log` **纯按关键字判级**，完全无视内核写在行首的 `[Level]`：

```rust
if lower.contains("failed") || lower.contains("error") || lower.contains("rejected") {
    "error"
}
```

于是消息里带 `rejected` / `failed` 的 `[Info]` 行全被升级成「错误」。更糟的是
它**把真正的错误淹掉了**：错误页签里全是这两类噪音，用户翻不到真的；而且只要
消息里出现 `failed`，连 `[Debug]` 行都会被算成错误。

**修法**：先信内核自己写的 `[Error]` / `[Warning]` / `[Info]` / `[Debug]` 标记，
没有标记（核心启动横幅、裸 stderr）才退回关键字判断。

副作用：`[Warning] failed to dial` 从「错误」变成「警告」。这是对的 —— 内核说
是 Warning 就是 Warning，不该由我们按消息里的词去升级它。

### 顺带查清：`rejected type ... query` 不是故障，别去"修"它

官方文档写明内置 DNS **只支持 A / AAAA**，其余类型交给 DNS 出站决定丢弃还是
透传。用一个**隔离实例**（`dokodemo-door` 收 DNS → `dns-out`，不碰 TUN、不改
系统网络）实测三种配法：

| `dns-out` 的配置 | TYPE65 的响应 | 结论 |
|---|---|---|
| **不配（当前）** | `NOERROR, ANSWER: 0`，**1ms** | ✅ 客户端立刻回退去问 A |
| `nonIPQuery: "drop"` | 不回包，客户端**等到超时** | ❌ 更卡；该字段还已被内核标为 deprecated |
| `rules` + `direct` 到 `223.5.5.5` | `NOERROR, ANSWER: 0` | ❌ 1ms 变一次真实上游往返，仍拿不到 HTTPS RR |

所以**刻意保持 `dns-out` 无 `settings`**，并加测试钉住这个决定
（`dns_outbound_has_no_settings_on_purpose`）。详见 docs/04 §6.8。

> **差点被 `dig` 骗**：macOS 的 `dig x.com HTTPS` 打印了一个 IP，看着像 HTTPS RR
> 查询成功了。改用数字类型 `TYPE65` 重测，真实响应是 `ANSWER: 0` —— 那个 IP 是
> dig 自己按别的类型答的。**校验查询类型要用数字，别信助记符。**

## 0.5.0

### 国外 DNS 也参与探测，并且和国内分开显示

**问题**：0.4.0 的解析器优选只挑**国内**解析器，而且只用**直连**测；
`remote_servers`（`geosite:geolocation-!cn` 用的那组）从头到尾是硬编码的
`https://1.1.1.1/dns-query` + `https://8.8.8.8/dns-query`，从来没被验证过。
界面上国内、国外混在一张表里，看起来像同一把尺子量出来的数字。

**关键在于两组不能用同一条路径测**：

| 组 | 谁在用 | 怎么测 |
|---|---|---|
| 国内 | `geosite:cn` → `direct_servers[0]` | 明文 UDP，绑物理网卡直连 |
| 国外 | `geosite:geolocation-!cn` → `remote_servers[0]` | DoH，**经节点**（本地 SOCKS 入站） |

直连测国外 DNS 是**没有意义**的：本机实测直连 `https://1.1.1.1/dns-query`
用 8 秒超时都拿不到连接，而经节点 0.23–0.39 秒就有答案。经节点不是近似 ——
它在分流规则里本来就是经节点用的，那就是它的真实成本。

**因此节点未连接时国外那一组标成「未探测」**，而不是硬走直连测一遍、
再把必然的超时谎报成「这台解析器不通」。

**改动**：

- 候选池拆成两组：14 台国内明文 + 5 台国外 DoH（AdGuard / Cloudflare ×2 /
  Google / Quad9，全部是 **IP 形式**的端点以避开自举依赖）。
- 新增 DoH 探测路径：`curl --socks5-hostname` POST 一个
  `application/dns-message` 报文，响应体就是明文 DNS 的线格式，
  直接复用已有的 `parse_a_records`，不引入新的 DoH 客户端。
- 「答得对不对」的多数派投票**改成按组分开做**：国外解析器经节点出去，
  看到的 CDN 边缘和国内直连本来就可能不是同一批地址，跨组混投会误伤。
- 国外组**串行探测**。它们共享同一个节点，并发测等于测节点的排队而不是
  解析器的远近 —— 和国内组「不绑网卡就测到核心排队」是同一类错误，
  而且后果更重：**名次会变**。实测 AdGuard 并发 4 是 450ms、排名第 3，
  串行是 267ms、排名第 1。
- 两组各自排序、各自写回：国内组写 `direct_servers`，国外组写 `remote_servers`。
- **连上之后自动重探一次**。启动时探的那一次节点还没连上，国外组必然是
  「未探测」；不补这一次，用户就得自己点「立即检测」，等于功能默认不生效。
  这一轮不阻塞连接、也不重启核心 —— DNS 配置只在生成配置时被读取，
  所以结果对**下一次连接**生效。
- 新增 `apps/desktop/examples/dns_probe.rs`：跑的就是 App 里同一份
  `probe_pool`，只读不写，用来看两组各自测出什么。

### 实测（活隧道，n=3 取中位）

```
国内组 14/14 可用   28–160 ms
国外组  5/5 可用    AdGuard 267 < Cloudflare备 283 < Cloudflare 299
                    < Google 354 < Quad9 389   （经节点，串行）
```

`--no-socks`（模拟未连接）时国外组 5 台全部标「未探测」、延迟为空，
退出码 0 —— **「未探测」不是「不通」**。
- 自动排序**只调池内项的相对顺序**，用户手填的服务器保留在后面。
- 界面分两块显示，各带自己的「当前首选」和失败原因。

## 0.4.1

### 修复：界面什么都加载不出来（0.3.0 引入，0.4.0 加重）

**症状**：进程活着、连得上 helper、日志里一句错都没有，**但界面什么都不显示**。
没有任何崩溃报告，也没有报错。

**根因**：`build_snapshot` 在已经持有状态锁的闭包内部，又调用了会**再次加锁**
的函数：

```rust
state.with(|inner| AppSnapshot {
    ...
    update: update_status(app, state),       // ← 内部还要 state.with()
    dns: state.with(|i| i.dns.clone())...,   // ← 同样重入
})
```

`AppState::with` 用的是 `std::sync::Mutex`（**非递归**）。同一线程重复加锁
会**永久等待自己** —— 于是 `build_snapshot` 永不返回，而它被每个命令调用，
界面自然什么都拿不到。

第一处是 0.3.0 加 `update_status` 时引入的，第二处是 0.4.0 加 `dns` 时又添的。

**修法**：把需要加锁/做 IO 的东西全部挪到闭包**外面**算完再传进去
（这几项本来也要读文件、起进程问核心版本，不该握着状态锁做）。

**同时加了一道守卫**：`AppState::with` 现在检测重入并 **panic**，把
「静默挂起」变成一句能读的报错。配两条回归测试：重入必须 panic、
以及 panic 之后守卫要复位（不能连坐后续调用）。

**为什么发布前没发现**：我的验证一直停在「纯函数有测试」和「二进制能构建」，
**从来没有真的启动过一次应用、确认界面能加载**。0.3.0 和 0.4.0 都是这样出去的。
这条教训比 bug 本身更重要：一个 App 的验收标准必须是「它能启动并显示出来」。

## 0.4.0

### 新增：DNS 解析器候选池 + 启动时自动选优

候选池从 3 台扩到 **16 台**（13 台国内 + 3 台国外明文），每一项都在本机
实测过可用性与延迟。

**只按延迟选是不行的。** 实测：

```text
223.5.5.5   直连 31ms
8.8.8.8     直连 69ms     ← 中国到 Google DNS 不可能这么快
8.8.8.8     经节点 200ms
```

69ms 那个说明**有中间设备在 53 端口抢答**：任何 UDP DNS 查询它都回一个
自己的答案。抢答的解析器延迟一定漂亮，但答案可能是错的（投毒、广告跳转）。

所以「最好用」判两件事：**能不能通、多快**（直接 UDP 查询取中位数），
以及**答得对不对**（所有候选查同一个域名，答案与多数派不一致的标记出来）。

### 探测方法上踩的三个坑（都实测过）

**坑 1：核心自己的 DNS 缓存会伪造出漂亮数字。**
第一版用固定域名，`114.114.115.115` 报出 **1ms** —— 公网 DNS 不可能 1ms，
那是核心的 DNS 缓存答的（TUN 模式下所有 :53 都被接管）。

**坑 2：不绑网卡时测到的是核心排队。**
查询经 TUN → 核心的 gVisor 栈 → 解析器，并发 6 路时同一台阿里 DNS
报 **155ms**，绑定 en0 直连只有 **32ms** —— 差 5 倍，而且会把快的排到后面。
现在探测 socket 会绑到物理网卡（与核心 `direct` 出站同一机制）。

**坑 3：用「不存在的随机域名」测延迟是错的工具。**
本意是「任何缓存都答不上来」，但不存在的域名会逼解析器做**完整递归**，
于是测到的是「递归一次多久」而不是「到解析器多快」。同一台阿里 DNS：
随机名 142ms，常见域名 30ms。改成**轮换一批极常见域名**（解析器有缓存，
测到的就是网络 RTT），而本机缓存由绑网卡解决。

**坑 4：参照域名不能选重 CDN 的。**
第一版用 `www.microsoft.com` 做「答得对不对」的比对，结果电信的两台解析器
被误判成可疑 —— 因为 CDN 域名不同解析器**合法地**返回不同地址。
改用 `example.com`（IANA 保留，单条稳定 A 记录）。

已知局限（写在代码注释里）：这个比对**只能发现「对普通域名乱答」**，
发现不了「对被墙域名投毒」—— 后者恰恰是多数派都错、少数派才对，
多数派投票在这种场景下会把正确答案标记成异常。对分流设计影响不大：
被墙域名本来就全部走 DoH。

### 什么时候跑

启动时**后台**跑一次（约 10 秒，不阻塞窗口显示），结果在下一次连接时生效
（配置是那时生成的）。设置里也可以「立即检测」。
远端 DoH 不参与排序 —— 它的耗时由节点主导，换哪台差别很小。

## 0.3.0

### 新增：节点二维码导出

「节点」页每个节点多了「二维码」按钮，弹出二维码 + 分享链接，可复制。

链接由 `xt_core::subscription::share` 生成，是 `uri.rs` 里那些解析器的**逆运算**。
正确性靠**往返测试**保证：`parse(export(node)) == node`，每种协议一条
（vless/vmess/trojan/ss/socks/http，含 REALITY、WS、gRPC、后量子 encryption）。
只断言「生成的字符串长得对」是没有意义的 —— 那只能证明我按自己的理解写了两遍。

两处刻意的设计：

* **原始链接优先**。从链接导入的节点保留着原文，导出时直接用它 ——
  重新拼一遍可能丢掉我们不认识的参数。
* **丢字段要报出来**。分享链接的表达能力比内部模型窄（比如 mux、
  Shadowsocks 的 UOT 就没有标准位置），这些会以警告列在二维码下方。
  静默丢弃是这类功能最容易犯的错。

已用真实节点验证：往返一致、零丢失。

### 新增：界面显示版本

侧边栏底部现在同时显示**客户端版本与核心版本** —— 分开列是必要的，
升级客户端不等于升级核心，而核心版本决定支不支持原生 TUN。

### 新增：核心与 geo 数据自动更新

「设置 → 核心与数据更新」：显示三个版本，可检查更新、安装、回退。

**两条独立通道**：核心（`XTLS/Xray-core`，几个月一次）与 geo 数据
（`Loyalsoldier/v2ray-rules-dat`，**每天更新**）。合成一个按钮会让人
以为必须一起升级。

#### 三个关键决定

**1. 更新不写进 `.app` 包。**

包里是 ad-hoc 签名的，改 `Contents/Resources/` 会让签名失效。所以更新落在
用户数据目录的 `core/` 下，由核心解析优先命中它。于是**回退 = 删掉那个目录**，
不需要备份、不需要版本管理，而且永远可用。

**2. 不用 GitHub 的 `releases/latest`。**

实测它返回 `v26.3.27`，而当时最新是 `v26.9.9` —— 因为 XTLS 把新版本
**全部标成 prerelease**，`latest` 会跳过。照它写，一升级就把用户从
26.9.9 **降到** 26.3.27。改为拉列表自己按数值逐段比较版本号。
是否 prerelease 如实标在界面上。

**3. 全部验证完才落到生效位置。**

下载 → 校验摘要 → 解压到暂存 → **结构校验** → **跑一次 `xray version`**
→ 才改名进托管目录。任何一步失败都不留半套文件。

最后那条比摘要更贴近实际：摘要能证明「下载对了」，但证明不了「架构对」——
把 arm64 装到 x86_64 上摘要照样能对上，只有执行才会暴露。

geo 文件额外做了 protobuf 结构校验，挡的是**截断的下载**。因为缺 geo 数据的
表现是 `geoip:cn` **静默不命中**，日志里没有错误 —— 宁可在这里拒收。

#### 一个容易踩的坑

geo 与核心是两份独立更新，所以「托管目录里有新核心、但没有 geo 文件」是
**正常状态**。如果这时直接把 `XRAY_LOCATION_ASSET` 指向托管目录，规则会
静默失效。所以现在按「**哪个目录真的有 geo 文件**」来选，而不是按
「哪个目录提供了核心」。

#### 刻意没做全自动安装

自动**检查**做了（联网几秒，不改变任何东西），但安装需要点一下。
换核心必然中断一次连接，而且理论上可能让原本能用的配置失效 ——
这件事应该发生在一个用户看得见、能回退的地方。

## 0.2.2

### 延迟探测拆成两个指标（原来那个数不是延迟）

原来「延迟」是**经节点**请求 `http://cp.cloudflare.com/generate_204` 的首字节时间。
这个数包含两段路，而只有第一段是节点的属性：

```
本地 → 服务器   +   服务器 → Cloudflare → 回来
```

后一段取决于服务器离最近的 CF PoP 有多远。实测（香港节点，同一次会话）：

| 量法 | 中位数 |
|---|---|
| 本地 → 服务器（纯 TCP 握手） | **59 ms** |
| 经节点 → Cloudflare（原来报的） | **196 ms** |

**136 ms 是后一段，占报出数字的 70%。**

这会让排序**反过来**：如果服务器→CF 是 137ms，那么
「你→HK 59ms + HK→CF 137ms = 196ms」会慢于
「你→US 150ms + US→CF 20ms = 170ms」—— 离你近 3 倍的节点被报成更慢。

现在拆成：

* **延迟** = 到 `server:port` 的 TCP 握手 RTT，**3 次取中位数**。不含任何
  第三方路程，也不依赖靶点位置。
* **可用性** = 经节点到靶点能否取到东西，**只取成功/失败**。

配套的三处修正：

* **并发 8 → 4**。实测同一个核心上并发 8 条探测会把中位延迟从 192ms 抬到
  221ms（+15%），离散度也变大 —— 节点越多每个数字越差，纯属自伤。
* **超时重试一次再判失败**。冷 DNS 缓存（探测配置里没有 `dns` 段，靶点
  域名要走系统解析器绕回主核心的 DNS 模块）会让首次请求明显更慢，
  一次超时就标「失败」会把能用的节点误报成不可用。
* **RTT 不经过核心**：直接对服务器发 TCP 握手，所以即使探针核心启动失败，
  延迟这一项依然有值 —— 这两件事本来无关。

顺带修掉 `User-Agent: XrayTun/0.1` 的硬编码（现在取真实版本号）。

## 0.2.1

### 修复：TUN 模式下国内 DNS 完全失效（0.2.0 的回归）

0.2.0 把 DNS 改成「按规则分流」之后，TUN 模式下的国内解析**从来没出过本机**：
日志里全是

```
[Error] app/dns: failed to retrieve response for wx.qlogo.cn.
        > Post "https://1.1.1.1/dns-query": context deadline exceeded
```

注意 `wx.qlogo.cn` 是**国内域名**，本该由 `223.5.5.5` 直连解析，却在走 DoH。

根因是 DNS 劫持规则只写了 `"port": "53"` —— 匹配「任何来源、任何目的的
53 端口流量」，于是**内核自己的上游查询也被它吞了**：

```
from DNS accepted udp:223.5.5.5:53 [dns-module -> dns-out]     ← 修复前
```

链路是：内核要查 223.5.5.5:53（国内域名走国内解析器）→ 这个 UDP 连接经
dispatcher 派发 → 撞上 `port: 53` 规则 → 被塞回 `dns-out`，也就是回到 DNS
模块自己 → TUN 模式下它按默认路由掉回 utun，再次进入 tun 入站，如此反复。
结果是国内解析这条腿永远到不了网络，所有解析都回退到走节点的 DoH；
节点一慢就是满屏 `context deadline exceeded` 与 `record not found`。

**为什么 0.2.0 发布时没测出来**：验证是在系统代理模式下做的 —— 没有 tun，
裸 UDP 包正常从 en0 出去，国内解析 1.19ms 就回来了，看起来完全正常。
是「TUN 接管 + 宽泛的端口规则」两个条件凑齐才暴露的。

修法是把劫持限定在**哨兵地址**上（客户端 DNS 都发给它）：

```jsonc
{ "ip": ["198.18.0.2"], "port": "53", "outboundTag": "dns-out" }
```

各归各位之后：

| 流量 | 判给 | 结果 |
|---|---|---|
| 客户端的 DNS（发往哨兵） | `dns-out` | 照旧劫持进内核解析 |
| 内核自己的 `223.5.5.5:53` | `preset-cn-ip` → `direct` | **direct 绑定了 en0，逃出隧道** |
| 内核的 DoH `1.1.1.1:443` | `internal-fallback` → 节点 | 保持抗污染 |

用真实核心验证（TUN 档位配置，把 tun 入站换成 socks 以便免 root 观察）：

```
修复前: from DNS accepted udp:223.5.5.5:53 [dns-module -> dns-out]
修复后: from DNS accepted udp:223.5.5.5:53 [dns-module -> direct]
        UDP:223.5.5.5:53 got answer: myssl.com TypeA -> [182.242.214.100]
```

冒烟测试（`tun_smoke`）也补了断言：国内解析应答数必须 > 0，且不得出现
`[dns-module -> dns-out]`。这类「能上网但全是慢的」故障，只看网页打得开
是发现不了的。

## 0.2.0

### 修复：Google 系域名无法访问

`geosite:cn` 里收了约 130 个 Google 域名 —— `www.gstatic.com`、
`fonts.gstatic.com`、`g0-g3.gstatic.com`、`dl.google.com`、
`fonts.googleapis.com`、`update.googleapis.com`、`safebrowsing.googleapis.com`，
以及一整套 `pki.goog`（OCSP / CRL 证书吊销检查）。它们被当作「国内可达」，
实际早已被墙，于是被判去直连、连接超时。

症状很有迷惑性：`google.com`、`youtube.com` 正常（不在 CN 列表里，走兜底代理），
只有 `www.gstatic.com`、`dl.google.com` 这类挂掉 —— 看起来像「Google 有的能开、
有的不能开」。

修法是在「广告拦截」与「大陆直连」之间插入 `geosite:google` → 当前节点。
两侧顺序都是硬约束：放到大陆直连之后会被直连规则吃掉；放到广告拦截之前，
`google-analytics.com`、`doubleclick.net` 这些本该拦截的广告域名会被放去代理。

顺带排除一个看起来合理但无效的修法：用 `geosite:gfw`。CN 与 GFW 的交集只有
40 条，且**一条 Google 域名都没有**（实测）。

### 修复：DNS 频繁 `context canceled`

0.1.0 的默认 DNS 模式是「全部走代理解析」。实测每个查询经节点约 **450ms**，
而国内 DNS 直连只要 **1ms**。更要命的是它把「域名能不能解析」绑在了
「节点快不快」上 —— 节点一抖，查询就超过内核的 DNS 超时，日志里刷：

```
[Error] app/dns: failed to retrieve response for api.deepseek.com.
        > Post "https://1.1.1.1/dns-query": context canceled
```

而用户看到的是「什么都打不开」。

两处改动：

1. **默认改为按规则分流**：大陆域名走国内 DNS 直连，其余走远端 DoH。
   已有的旧设置会由 `AppSettings::migrate` 一次性迁移，并在日志里说明改了什么。
2. **去掉分流解析器上的 `expectIPs`**。它会把正确结果当成失败：国内的解析器
   1.2ms 返回了 `api.deepseek.com` 的地址，只因为该地址（AWS）不在 `geoip:cn`
   里就被判为「空响应」丢弃，然后串行回退到 DoH —— 一次查询从 1ms 变成
   450ms 起步。而「国内域名解析到海外 IP」在 CDN 时代是常态。

远端 DNS 默认值也从 `https://dns.google/dns-query` 换成 `https://8.8.8.8/dns-query`：
后者是域名形式的 DoH 端点，主机名要先被解析一次，而解析它用的还是这套 DNS，
属于自举依赖。

### 修复：⌘Q 退出会留下孤儿核心

`app.exit()` **不会运行析构函数**，所以 `XrayProcess` 上的 `kill_on_drop(true)`
在退出路径上根本不生效。而 ⌘Q 走的是 Tauri 默认退出流程，绕过托盘里的
「退出 XrayTun」菜单项。

后果：退出后路由和 DNS 留在系统上，核心进程还活着并继续占着
10808 / 10809 / 10085 —— **下一次点「连接」会直接因为端口被占而失败**。

已把清理逻辑提成幂等的 `tray::sync_cleanup`，托盘退出与
`RunEvent::ExitRequested` 都走它。

### 修复：退出/修复网络对「活着的会话」无效

`Request::Restore` 一度写成 `restore_stale()`，被 `is_stale()` 挡住 ——
而正常连接的会话恰恰是那个状态（`Up` + `pending_routes` 已清空）。
于是托盘「退出」和「修复网络」都对着一条活隧道回复「没有需要回滚的会话」，
路由和 DNS 全部留在系统上，用户点了修复也没用。

### 修复：启动事件发送空载荷

`bootstrap` 里是 `app.emit(RUNTIME_CHANGED, ())`，而前端会直接读
`payload.runtime` —— 在 webview 里抛 TypeError，运行时与流量都拿不到更新。

### 新增：开机自启动

「设置 → 其他 → 开机自启动」。用 `SMAppService`（macOS 13+ 的系统登录项），
而不是写 `~/Library/LaunchAgents` plist —— 后者会落在
「系统设置 → 通用 → 登录项与扩展 → **允许在后台**」里，而用户会去
「**登录时打开**」那个列表确认，找不到就认为功能没生效。

两个实现细节值得记下来：

* ObjC 选择子是 **`mainAppService`**，不是 `mainApp` —— 后者只是
  `NS_SWIFT_NAME` 给 Swift 用的名字，从 ObjC 发消息必须用前者，
  写错没有编译错误、只有运行时 unrecognized selector。
* 界面开关读的是**系统的 `status`**，不是回显 `settings.launch_at_login`。
  用户能直接在系统设置里删掉这一项，回显设置字段会显示「已开启」
  而实际不会自启。`RequiresApproval`（已登记但待批准）与 `Enabled` 分开呈现，
  并提供一键跳转到系统设置的按钮。

另加了排障入口（`SMAppService` 操作的是调用方所在的 bundle，
所以必须从 App 自己的二进制里跑）：

```bash
XrayTun.app/Contents/MacOS/xraytun-desktop --login-item status|enable|disable
```

### 新增：实时网速

* 从核心的 `StatsService` 读累计字节（gRPC over h2c，手写 protobuf）。
* 顶栏显示 `↓ 1.2 MB/s ↑ 34 KB/s`，菜单栏显示 `↓1.2M ↑34K`（空闲时不显示）。
* 设置里可关闭。
* 顺带修好了面板上「速率永远是 0」的问题 —— 那个字段此前从没有人写过。

### 新增：CI 与发版

* `ci.yml`：clippy（warning 视为错误）、228 个单元测试、前后端构建。仅 macOS。
* `release.yml`：打 `v*` tag 即出 **universal** 包并创建 GitHub Release。

### 新增：macOS 打包脚本

`scripts/package-macos.sh`。`tauri build` 单独跑出来的包是不能用的：
helper 不在 `Contents/MacOS/`（点安装时报「找不到 helper 二进制」），
`geoip.dat` / `geosite.dat` 未必与核心同级（`geoip:cn` **静默不命中**）。
脚本补齐这两件并逐项校验包内布局。

### 界面

顶栏底边改成绿色状态条（原来是 1px 的暗色分隔线，深色背景上几乎看不见）。
