# v6 数据面静态审计 · `task-143` 第一阶段

> **状态**：**只读 + 静态**。本页**不含**任何真机网络操作（没改路由 / DNS、没连接或断开 App、没装卸 helper）。
> 每条结论都标了 `[代码证据]`（可复算的 file:line 推演）/ `[推断]` / `[需真机验证]`。
> 第二阶段（真机）**未执行**，方案见 §6，等 Lead 批准。

## 0. 结论速览

| 问题 | 结论 |
|---|---|
| `default_route_v6` / `DEFAULT_TUN_NETWORK_V6` 是死代码还是被移除？ | **初始提交起就是死代码**（`757d2e3` 引入，之后没有任何 commit 改动过出现次数）`[代码证据]` |
| v6 节点的 host 路由拿到什么 `RouteVia`？ | `RouteVia::Interface { name: "en0" }`（**没有网关**）`[代码证据]` —— 由 `bypass_via` 的 match 唯一决定（`plan.rs:270-277`） |
| 这意味着 v6 节点受影响吗？ | **很可能受影响**，而且影响面**包含默认的 `passthrough`**（不只是 `override`）`[推断]`；**本机无法实测**（无全局 v6） |
| `Ipv6Mode::Override` 在干活吗？ | **很可能没有**：v6 捕获路由装进了 utun，但 utun 上**没有任何 v6 地址**，核心 tun 配置里也只有 v4 `gateway` `[代码证据]`；报文能否进 gVisor 栈**静态判不出** `[需真机验证]` |
| `Ipv6Mode::Disabled` | 在 `build_plan` 里与 `Passthrough` **同分支**（逐字节同样的处理），UI 侧已由 task-142 删掉选项 `[代码证据]` |
| 本机能否做端到端 v6 验证？ | **不能**：`en0` 只有 `fe80::`，无全局 v6 默认路由，`route -n get -inet6 2001:4860:4860::8888` → `not in table` `[本机只读实测]` |

**给 Lead 的一句话答复：v6 节点是否真的受影响 —— 「无法判定（本机测不了）；静态推断是『很可能受影响，且是 host 路由本身造成的，不是缺旁路』」，所以我建议先做 §6 的 A0/B1（成本极低、可做完就定案），再决定 §7 的修法。**

## 1. 历史：死代码，不是「曾被有意移除」

```bash
$ git log --oneline -S 'default_route_v6()' --all
dfa516f docs(product): A11「禁用 IPv6」/ A12「关闭嗅探」决策备忘（task-141）
757d2e3 初始提交：macOS TUN 代理工具（Xray 原生 TUN + 特权 helper）

$ git log --oneline -S 'DEFAULT_TUN_NETWORK_V6' --all
dfa516f …（同上，只是文档提到它）
757d2e3 初始提交…
```

* `-S` 统计的是**每提交中该字符串出现次数的变化**：两个符号都只在初始提交 `757d2e3` 出现「0 → 1」（**定义**），此后**一次都没变多**（即从未被调用/使用）；`dfa516f` 那次变化来自文档正文提到名字。
* 因此：**不是「曾被调用后移除」**，而是**从第一天起就是死代码**；历史里没有留下任何「为什么留/为什么删」的理由。
* 全仓调用点（`[代码证据]`）：
  * `default_route_v6()`：只有定义 `crates/xt-tun/src/macos/route.rs:44-48`；
  * `DEFAULT_TUN_NETWORK_V6`：只有定义 `crates/xt-tun/src/plan.rs:279-280`。
* 顺带两处**放置不对称**（`[代码证据]`）：
  * v4 兄弟 `DEFAULT_TUN_NETWORK_V4` 在 `crates/xt-proto/src/lib.rs:584`，被 `model.rs:646` 用作 `tun.network` 默认值；
  * v6 兄弟却在 `xt-tun/src/plan.rs`（**不是** `xt-proto`），且 `xt-proto` 也没有它的兄弟常量给别的调用方用 —— 所以「没人用」不只是没接线，连**位置都放错了**。

## 2. 逐跳推演：v6 节点的 host 路由拿到什么

Q：`bypass_hosts` 里的 v6 地址，在当前实现下会装成什么路由？

| # | 步骤 | 证据 |
|---|---|---|
| 1 | App 启动 TUN：探测物理出口用的是 **IPv4** `default_route()` | `apps/desktop/src/supervisor.rs:507` → `route.rs:39-42`（`route -n get default`） |
| 2 | 解析节点地址：`resolve_host()` 返回**全部** A/AAAA（v4 排前） | `crates/xt-core/src/net.rs:16-38`（`:29` 排序 `ip.is_ipv6()` 优先 false ⇒ v4 先） |
| 3 | App 把 server 地址 + 物理网关塞进 `bypass_hosts`；`addresses` **只给 v4** | `supervisor.rs:902-924`、`:942-944`（`tun.network_cidr()`，默认 `198.18.0.1/15`） |
| 4 | 请求里**不带**网关字段（`TunUpRequest` 没有 uplink），helper **自己重新探测**——而且同样是 v4 | `crates/xt-tun/src/macos/controller.rs:48-53`（`route::default_route()`） |
| 5 | helper `build_plan` 对每个 bypass host 调 `bypass_via(physical, host)` | `crates/xt-tun/src/plan.rs:167-178`（`PlannedRoute::bypass(.., critical=true)`） |
| 6 | **决定性一行**：`bypass_via` 的 match 只有 `(V4, Some(V4))` 与 `(V6, Some(V6))` 两个网关分支；`physical.gateway` 只可能是 v4/None ⇒ v6 主机落进 `_ => Ok(RouteVia::Interface { name: physical.interface.clone() })` | `crates/xt-tun/src/plan.rs:270-277` |
| 7 | `build_args` 把 `Interface` 展开为 `route -n add -inet6 -host <v6> -interface en0` —— **没有网关参数** | `crates/xt-tun/src/macos/route.rs:157-213`（`:166-168` 加 `-inet6`，`:186-190` `-interface`） |
| 8 | 这条 `/128` 比原来那条全局 v6 默认路由**更具体**，因此**胜出**；且它是 `critical` 路由（装不上 ⇒ 整次建立失败） | `plan.rs:177`；路由优先级是内核语义 `[推断]` |

⇒ **结论 `[代码证据]` + `[推断]`**：v6 服务器地址会被装成一条**无网关的 on-link `/128`**。
本项目自己对这个形状写过两次「会坏」：

* `route.rs:196-197`：「**网关必须给**。只写 `-interface` 会得到一条「目标在本地链路」的纯接口路由，内核会去对目标 IP 发 ARP，包根本出不去。」
* `plan.rs:212-213`：「只用 `-interface` 会得到一条「目标在本地链路」的纯接口路由，包发不出去。没有网关就干脆不加 —— 加了反而更糟。」
* 并且**有 v4 的回归测试**钉着这个故障：`route.rs:288-304` `scoped_interface_route_carries_gateway_and_ifscope`。

区别在于：这两处结论是在讲**作用域默认路由**（v4），而 **v6 的主机旁路**照样在制造同一个形状 —— 只是没人拦。

### 2.1 两个具体的用户症状路径（`[推断]`）

* **症状 A（v6-only 节点，或核心选了 v6）**：核心要连的服务器地址正好是那条 `/128` ⇒ 包发不出去 ⇒ App 的端到端门禁失败 ⇒ 报「**接管默认路由之前就联系不上代理服务器 ……**」并回滚（`supervisor.rs:626-634` 那条错误）。
* **症状 B（双栈域名节点）**：门禁探的是 `server_addrs.first()`（v4 优先）⇒ **能过**；随后核心建连（Go Happy Eyeballs，`crates/xt-core/src/xray/config.rs:507` 的注释自己记着「**TCP 仍然优先 IPv6**」）先试 v6、失败后**预期**回退 v4 ⇒ 可能只是**多 ~300ms 建连延迟**。回退是否真的发生、延迟多大，静态判不出 ⇒ `[需真机验证]`。

> 与 PM 备忘的关系：`docs/design/DECISION-A11-A12.md` §1.3 已把这个 `[推断]` 登记为「今天用 v6 节点 + `passthrough` 的用户可能同样受影响」。本页把它做实到「**第 6 步那一行**决定 `Interface`」，并补上症状 A/B 两条路径。

## 3. `Ipv6Mode::Override` 的静态检查 (a)/(b)

设置语义（`xt-proto/src/lib.rs:329-341`）：`Passthrough`（默认，不动 v6 路由）/ `Override`（同样用 `::/1`+`8000::/1` 接管）/ `Disabled`（枚举自称「显式禁用」，但实现见下）。

**(a) 帮助器/核心侧有没有把 v6 报文送进 gVisor 栈的路径？**

| 环节 | 现状 | 证据 |
|---|---|---|
| 捕获路由进 utun | **有**：`Override` 会加 `::/1` + `8000::/1`，`RouteKind::DefaultCapture`、critical | `plan.rs:227-244`；`controller.rs:130-152` |
| utun 的 v6 地址 | **没有**：helper 只配置 `plan.addresses`，而请求里只有 v4 隧道网段 | `controller.rs:113-116`；`supervisor.rs:942-944`；`netif.rs:27-66`（函数**本身支持** `inet6`，但没人传 v6 进来） |
| `DEFAULT_TUN_NETWORK_V6` | **无人使用**（§1）⇒ utun 上只会有 macOS 自动给的 `fe80::` link-local（本机 utun0–utun5 都是这样，§4） | `plan.rs:279-280` |
| 核心 tun 配置 | `gateway` 只有 v4；`autoSystemRoutingTable` **是空的**（App 传 `auto_routes=false`，故意的：路由由 helper 负责） | `config.rs:96-123`、`:402-407`；`apps/desktop/src/state.rs:666-684` |
| fd 交接 | helper 把 utun fd 交给 Xray（`HandoffFd`）；Xray 收到 `XRAY_TUN_FD` 时 **`ownsFd=false`，跳过自己那套地址/路由配置** | `supervisor.rs:960`、`:1007-1009`；`crates/xt-tun/src/macos/fdpass.rs:17-20` |

⇒ `[代码证据]`：**整条链路上没有任何一处给 v6 配地址**。

**(b) 若无，`Override` 是不是也没在干活？**

* 静态能确定的只有：捕获路由**装得进去**（helper 侧生效），但**没有地址基础**；
* 报文能不能被 Xray 的 gVisor 栈接受/转发（netstack 是否需要本端 v6 地址、是否有 promiscuous 处理）**静态判不出来** ⇒ `[需真机验证]`；
* 一个**可推断的附带后果** `[推断]`：应用发起 v6 连接时，出口接口（utun）只有 link-local，源地址选择多半直接失败 ⇒ 用户观感是「v6 立即不通」，而不是「被代理后超时」。这与「真黑洞」在**观感**上相近、**机制不同**（一个包被丢，一个包根本没发出去）—— 要区分只能真机看 `tcpdump -ni utunN ip6`。

**(c) `Ipv6Mode::Disabled` 现状 `[代码证据]`**

* `plan.rs:238` 把它与 `Passthrough` 放在**同一个分支**：`Ipv6Mode::Passthrough | Ipv6Mode::Disabled => { /* 不动 v6 默认路由 */ }` ⇒ **行为逐字节相同**；
* 全仓再无第二处读它（`grep -rn "Ipv6Mode::Disabled" --include=*.rs` ⇒ 仅 `plan.rs:238`），与 PM 备忘 A11-F3/F7 的发现一致；
* UI 侧 task-142 已删掉「禁用 IPv6」选项、把已存值 `disabled` 按 `passthrough` 显示（`apps/ui/src/pages/Settings.tsx:320-326`、`:663-696`）⇒ 这一支现在是**历史遗留**（旧 settings 反序列化仍会读到它）。

## 4. 本机只读读数（2026-09-23，**未做任何改动**）

```bash
$ /sbin/route -n get -inet6 default
route: writing to routing socket: not in table      # stderr；stdout 为空
EXIT=0                                              # ⚠️ route(8) 这个失败**不改退出码**

$ /sbin/route -n get -inet6 2001:4860:4860::8888    # 公共 v6 目标
route: writing to routing socket: not in table
EXIT=0

$ /sbin/ifconfig en0 | grep inet6
	inet6 fe80::cbd:3447:2830:a284%en0 prefixlen 64 secured scopeid 0xe   # 只有 link-local

$ /usr/sbin/netstat -rn -f inet6 | grep -c '^default'
6
$ /usr/sbin/netstat -rn -f inet6 | awk '$1=="default"{print $2, $3, $4}'
fe80::%utun0 UGcIg utun0
fe80::%utun1 UGcIg utun1
…（utun0–utun5，共 6 条，全部是 **interface-scoped** 的 `I` 标志）
```

三条对本卡有用的**本机事实**：

1. **本机没有可用的全局 IPv6**：`en0` 只有 link-local，没有全局 v6 默认路由，公共 v6 目标 `not in table` ⇒ **端到端 v6 验证在本机做不了**（不管是 v6 节点还是 `override`）。
2. 仅有的 6 条 v6 default 都是 `fe80::%utunN … I(ifscope)`（别的工具/VPN 留下的 utun）⇒ `default_route_v6()` 用的 `route -n get -inet6 default` **在本机只会解析失败**：`run()` 成功（退出码 0）但 stdout 为空 ⇒ `parse_route_get("")` → `None` ⇒ 返回 `None`。**即使今天就把这个死函数接上，本机也拿不到 v6 网关** —— 这属于「需要兜底」而不是「接上就好」。
3. 这些 utun 上 macOS 自动给了 `fe80::…` link-local ⇒ 我们的 utun 大概也会有（**`[推断]`**，因为现有 utun 不是本 App 建的）；它决定「v6 源地址选择会不会立即失败」。

## 5. 影响面小结

| 组合 | 静态推断的后果 | 置信度 |
|---|---|---|
| v6-only 节点 + `passthrough`（默认） | 服务器 `/128` 变成 on-link ⇒ 核心连不上 ⇒ 门禁失败 + 回滚（症状 A） | `[推断]`（机制由 `[代码证据]` 锁定：§2 第 6 步） |
| 双栈域名节点 + `passthrough` | 门禁走 v4 能过；核心可能先试 v6 ⇒ 多一次失败/延迟，预期回退 v4（症状 B） | `[推断]`，回退行为 `[需真机验证]` |
| `override`（任何节点） | v6 捕获进 utun 但**没有 v6 地址基础** ⇒ v6 很可能不通（立即失败或丢包） | `[需真机验证]` |
| `disabled` | 与 `passthrough` 完全相同（枚举自称的「禁用」没有实现） | `[代码证据]` |
| v4-only 节点（绝大多数用户） | **不受影响**（v6 地址根本不在 `bypass_hosts` 里） | `[代码证据]` |

## 6. 最小真机验证方案（**未执行**，等 Lead 批准）

分三组：**A = 零系统影响**、**B = 需要 TUN 起来（会短暂动路由）**、**C = 需要 v6 出口（本机做不了）**。

### A0. 单元级（不需要 root、不需要真机、零影响）——**建议第一个做**
* 给 `crates/xt-tun/src/plan.rs` 的测试补一条 **v6 host** 用例（今天没有：`grep` 显示测试里的 bypass host 只有 `203.0.113.7`）：
  `request.bypass_hosts = ["2001:db8::1"]`、`physical.gateway = Some("192.168.1.1".parse())` ⇒ 断言 `via == RouteVia::Interface{ "en0" }`。
* **判据**：断言通过 ⇒ §2 的 `[推断]` 升级为**可断言事实**（「当前代码确实给 v6 主机装无网关路由」）。
* 反向用例：给 `physical.gateway = Some("fe80::1")` ⇒ 今天会得到 `Gateway{fe80::1}`（但**生产路径永远给不出 v6 网关**，因为没人调 `default_route_v6()`）—— 这条同时说明「修法不是改 match，而是先让 physical 拿得到 v6 网关」。
* 成本：一次 cargo（**冻结窗口内先报 Lead**）。

### A1. 只读确认（已做）
§4 的三条命令 + 结论「本机无全局 v6」。**判据**：`en0` 无全局 v6 且公共 v6 目标 `not in table` ⇒ C 组在本机不可能做，B 组只能验**路由形状**。

### B1. 路由形状验证（**需要 Lead + 用户批准，用户在场**）
* 前置：确认 App 未连接；记录基线 `sudo route -n get -inet6 <node>`（以及 `netstat -rn -f inet6 | wc -l`）。
* 动作：在 App 里把节点地址临时改成 **v6 字面量**（用 `2001:db8::1` 这种文档地址即可 —— 我们要看的是**路由形状**，不是连通性）。
* 期望 `[推断 预期]`：
  * helper 快照里出现 `<node>/128`、`via = Interface{en0}`；
  * `netstat -rn -f inet6 | grep <node>` ⇒ `UHL en0`（**没有 U/G 网关标志**）；
  * `route -n get -inet6 <node>` ⇒ `interface: en0`，**没有 `gateway:` 行**。
* **判据**：`route -n get -inet6 <node>` 输出里 **有没有 `gateway:` 行**。
  * 没有 ⇒ §2 的推演成立（这一步就是「修前证据」）。
  * 有 ⇒ 我的第 6 步推演错了，必须重做（`[推断]` 被实测推翻，也是有效结论）。
* 用户影响：TUN 建立会在端到端门禁处失败并**自动回滚**（症状 A 本身就是这个）；捕获路由是**延迟提交**的（`defer_default_routes`），所以失败时不会有「流量已进隧道但数据面没起来」的窗口。**可能短暂影响网络（秒级）**，必须在用户在场时做。
* 回滚：App「断开」或退出（helper 侧 `TunDown` + 快照恢复，task-85 已实现）；随后 `sudo route -n get -inet6 <node>` 复核无残留。

### B2. `override` 的「捕获是否生效」半验证（**需要批准**）
* 用**可用的 v4 节点**连接 + `ipv6 = override`。
* 期望：`netstat -rn -f inet6 | grep -E '^(::/1|8000::/1)'` 出现两条指向 `utunN` 的捕获路由；`ifconfig utunN` 只有 `fe80::…`、**没有**全局 v6 地址。
* **判据**：捕获路由在（helper 生效）+ utun 无 v6 地址（没有地址基础）。
* **这条只能证伪「捕获没生效」，不能证伪「报文没进 gVisor 栈」** —— 后者要 C1。

### C1. 端到端 v6（**本机做不到**）
* 需要一台**有全局 v6 出口**的机器（或用户网络）：v6-only 节点 + `passthrough` 做真实连接。
* 判据：`curl --socks5-hostname 127.0.0.1:<socks> -6 https://<v6-only-target>` 成功 **且** 核心日志里有 v6 dial。
* 若要验 `override`「到底通不通」：`sudo tcpdump -ni utunN ip6`（看 v6 包有没有进来）+ 核心 access log 里 v6 目标有没有命中规则。
* 用户影响：同上（需要用户在场）。

## 7. 修复方案建议（供 Lead 决策；**本卡不实施**）

**P0（低风险、与 task-142 同风格；不改协议、不改 helper 的对外行为）**
1. `PhysicalUplink` 改成**按族**保存网关：新增 `gateway_v4` / `gateway_v6`（`#[serde(default)]`——**快照落盘在用户机器上**，见 `crates/xt-tun/src/macos/snapshot.rs:77` 与 `PhysicalUplink` 的 `Serialize/Deserialize`，加字段必须能读回 v0.8.36 的旧快照）。
2. helper 探测 `default_route_v6()`（**终于用上这个死函数**）填 `gateway_v6`；探测不到时接受 `None`（§4 第 2 条：本机式的「只有 ifscope v6 default」会拿到 `None`，需要兜底或明确接受不可用）。
3. `bypass_via` 改「**同族网关优先**」：`(V6, Some(gw_v6)) => Gateway{gw_v6}`；**没有同族网关时不要静默退化**：对**全局** v6 主机地址要 `warn!`（critical 路由被装成 on-link 等于伪造一条黑洞路由），并考虑**失败闭合**（宁可让建立失败并给出可操作文案，也别装一条注定发不出去的路由）。`fe80::/10` 这类**链路本地**网段仍应保持 `Interface`（`plan.rs:522-523` 的既有断言不许改）。
4. 单测：v6 host × {有 v6 网关 → `Gateway`，无 v6 网关 → 明确行为（warn/失败/Interface，按第 3 条的决定）}；再加一条「v4 主机路由不许因为这次改动退化」。
5. 文案/日志：出现「v6 旁路拿不到网关」时必须留痕（本项目最忌静默退化）。

**P1（产品决策，先做 A0/B1/B2 再定）**
* `override`：要么补全（utun 配 `DEFAULT_TUN_NETWORK_V6`、核心 tun 配置带 v6 地址、并实测 gVisor 收 v6），要么**和 `disabled` 一样删掉选项**（task-142 的先例：把「没有对应实现」的选项删掉，比留着一个错误信念好）。
* `DEFAULT_TUN_NETWORK_V6`：挪到 `xt-proto`（与 V4 对称）或删除。

## 8. 诚实清单（静态推不出来的部分）

1. **本机没有全局 IPv6** ⇒ **端到端 v6 行为（含 `override` 是否真把 v6 送进核心）在本机无法验证**；需要 v6 环境。
2. 本页**没有执行任何真机网络操作** ⇒ §2/§3 是 `[代码证据]` + `[推断]`；「on-link ⇒ 包发不出去」在 **v6** 上仍是 `[推断]`（v4 有本项目自己的实测与回归测试，v6 只是同语义外推）。
3. 「核心（Go）v6 失败后是否回退 v4、延迟多少」静态判不出 ⇒ `[需真机验证]`。
4. 「gVisor 是否接受/转发没有本端地址的 v6 报文」需要读核心源码或真机 ⇒ `[需真机验证]`。
5. `tun.network` 被手改成 v6 CIDR 的路径（`network_cidr()` 只 `parse()`、不校验族；`netif` 支持 `inet6`）**完全未测试**，本页只登记，不算结论。
6. §6 的 B 组会在用户生产机上**短暂动路由**且**需要用户在场**；A 组零影响；C 组本机做不了。
7. 我没有验证「别的工具留下的 6 条 ifscope v6 default（utun0–utun5）」是否会影响 B 组的判据 —— 真机方案执行前要先记录基线。

## 9. 本卡不做

* 不实现「禁用 IPv6」（`task-141` 已判走「删选项」= `task-142`，已完成）；
* 不改 `xt-proto` 协议号、不改 helper 侧行为（本页只是审计 + 方案）；
* 不执行 §6 的任何真机步骤（等 Lead 批准）。
