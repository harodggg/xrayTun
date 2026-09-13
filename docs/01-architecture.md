# 01 · 架构

## 1. 为什么是两个进程

macOS 上创建 `utun` 必须具备 root 权限（内核控制 `com.apple.net.utun_control` 带
`CTL_FLAG_PRIVILEGED`，非 root 的 `connect()` 直接 `EPERM`）。
但把 Electron/Tauri 这种体积的进程跑在 root 下是不可接受的：
它要解析网络数据、渲染 HTML、加载 npm 依赖树，攻击面比一个只做网络配置的
小守护进程大两个数量级。

于是只有两个选择：

| 方案 | 结论 |
|---|---|
| **A. 特权 helper 守护进程** | ✅ 采用。root 侧代码约 1200 行，不含任何协议解析 |
| B. `NEPacketTunnelProvider`（NetworkExtension） | ✗ 需要向 Apple 申请受限 entitlement + 付费开发者账号；且官方文档只为 iOS 说明了内存上限，macOS 上虽更宽松，但把代理内核放进扩展进程仍不划算 |

方案 A 的代价是**需要一次管理员授权来安装**，以及要自己处理「helper 崩溃后
网络配置留在半残状态」这个问题 —— 后者正是本项目里快照机制存在的原因。

## 2. 进程模型

```
┌─ XrayTun.app（用户 uid） ────────────────────────────────────┐
│                                                             │
│  WebView (React)                                            │
│      │  invoke / listen                                     │
│      ▼                                                      │
│  Tauri commands  ──▶ AppState { inner, supervisor, helper } │
│      │                                                      │
│      │  xt-core                                             │
│      ├─ Store            读写 ~/Library/Application Support │
│      ├─ subscription::*  订阅解析（4 种格式）                 │
│      ├─ xray::config     生成 Xray JSON                     │
│      ├─ xray::process    拉起/停止 xray 子进程               │
│      └─ xray::probe      临时核心 + 每节点独立 SOCKS 端口     │
│                                                             │
│  xray（子进程，普通用户）                                     │
│      ├─ SOCKS/HTTP 入站（127.0.0.1）                         │
│      ├─ tun 入站 ← 通过 XRAY_TUN_FD 拿到 helper 交付的 fd     │
│      └─ 出站 socket 绑定物理网卡（IP_BOUND_IF）               │
└──────────────────────┬──────────────────────────────────────┘
                       │ AF_UNIX SOCK_STREAM
                       │ /var/run/com.xraytun.helper.sock
                       │ 0660 root:admin + SecCode 校验
┌──────────────────────▼──────────────────────────────────────┐
│ com.xraytun.helper（root）                                   │
│                                                             │
│  socket 服务端（每连接一线程）                                │
│      ├─ peer::authorize   getpeereid + audit token + SecCode │
│      └─ 请求分发：TunUp / TakeTunFd / CommitRoutes / TunDown │
│                                                             │
│  xt-tun::controller                                         │
│      ├─ utun::create      PF_SYSTEM ioctl + connect          │
│      ├─ netif::configure  ifconfig（绝对路径 + argv）         │
│      ├─ route::add/del    route(8)                          │
│      ├─ dns::backup/set   networksetup                      │
│      └─ snapshot          每次变更后落盘                       │
└─────────────────────────────────────────────────────────────┘
```

### 2.1 职责边界（这是架构的核心约束）

**helper 不认识「代理」这个概念。** 它接受的指令集合是封闭的：

* 建一个 utun，给它这些地址、这个 MTU；
* 装这几条路由；
* 把这些 IP 设为这几个网络服务的 DNS；
* 把 utun 的 fd 交出去；
* 按快照回滚。

它**不**接受：任意命令、任意路径、任意文件写入。
数据面可执行文件的路径必须落在白名单目录内（`/Library/PrivilegedHelperTools`、
`/Library/Application Support/XrayTun`），且所有外部命令都用绝对路径 + argv 数组调用，
永不经过 shell。

这条边界的效果是：**即使 GUI 进程被完全攻破，攻击者能获得的也只是「改本机网络配置」，
而不是「以 root 执行任意代码」。**

### 2.2 为什么数据面不放在 helper 里

直觉上「helper 既然已经是 root，顺手把 xray 也拉起来」更简单。我们没这么做，
因为那意味着**整个代理内核以 root 解析来自互联网的不可信数据**
（VMess/VLESS 握手、TLS 记录、gVisor 协议栈的包处理）。

现在的做法是利用 Xray 的一个能力：当环境变量 `XRAY_TUN_FD` 存在时，
darwin 的 TUN 入站会直接使用这个 fd，并**跳过**自己的建卡与地址/路由配置
（源码里 `ownsFd = false`，`setup()` 直接返回）。

于是分工变成：

* helper：建卡、配地址、装路由、改 DNS —— 这些都是「配置」，不接触流量；
* xray（普通用户）：所有流量处理。

代价是这依赖一条上游文档只承诺给 iOS/Android 的代码路径（详见
[03-xray-integration.md](03-xray-integration.md#5-版本下限与退路)），
所以协议里保留了 `SpawnDatapath { use_helper_fd: false }` 作为完全公开路径的退路。

## 3. 模块边界

```
xt-proto   ── 被所有 crate 依赖。只有类型 + 传输层，无业务逻辑
   ▲
   ├── xt-core   与平台无关。不知道系统网络、不知道 UI
   │      └─ 可以被单独拿去做 CLI / 服务端
   └── xt-tun    macOS 系统调用。不知道 Xray、不知道订阅
          └─ 只做「建卡/配路由/改DNS/快照」四件事
                 ▲
                 └── xt-helper   = xt-tun + socket 服务端 + 对端授权
```

**没有反向依赖。** `xt-core` 不依赖 `xt-tun`，`xt-tun` 不依赖 `xt-core`。
这带来两个实际好处：

1. `xt-core` 的全部逻辑（订阅解析、配置生成）可以在 Linux CI 上跑测试；
2. 「谁有权改系统网络」这个问题在类型层面就一目了然 —— 只有 `xt-tun` 会调
   `ifconfig`/`route`/`networksetup`。

## 4. 数据流

### 4.1 启动 TUN（两阶段）

```
UI 点「连接」
  │
  ├─ 1. 探测物理出口        route -n get default  →  en0 / 192.168.1.1
  │     （必须在建 utun 之前，否则拿到的是上一次的 utun）
  │
  ├─ 2. 解析核心 & 校验版本  xray version → 26.9.9，>= 26.1.31 ✓
  │
  ├─ 3. 生成配置 & 落盘      ~/.../runtime/config.json
  │     xray run -c <文件> -test  ← 静态自检，把非法配置挡在进程启动之前
  │
  ├─ 4. helper.TunUp(defer_default_routes = true)
  │       建 utun4 → 配 198.18.0.1/15 → 装 bypass 路由（内网直连 + 网关 host 路由）
  │       落盘快照（state = BringingUp, pending_routes 已记录）
  │     ✗ 此刻流量仍然走原路，DNS 未改 —— 没有任何副作用暴露给用户
  │
  ├─ 5. helper.TakeTunFd(session)
  │       Response::TunFd 帧 + SCM_RIGHTS 消息（两步，时序由协议规定）
  │
  ├─ 6. 清除 fd 的 FD_CLOEXEC → spawn xray（带 XRAY_TUN_FD）
  │       核心自己不做任何系统网络配置，只用这个 fd 收发 IP 包
  │
  ├─ 7. 等 SOCKS 端口可连（唯一与版本无关的就绪信号）
  │
  └─ 8. helper.CommitRoutes
          装 0.0.0.0/1 + 128.0.0.0/1 → 写系统 DNS → state = Up
          ✗ 从这一刻起流量才真正被接管
```

**第 4 步与第 8 步的分离是刻意的。** 如果一次性做完，从「路由已接管」
到「数据面就绪」之间有几百毫秒的窗口：流量被送进一个还没人读的 utun，
同时 DNS 已经指向隧道内的哨兵地址。用户看到的现象是
「一连上所有网页都打不开」，而且因为它转瞬即逝，极难复现和定位。

### 4.2 停止

```
UI 点「断开」
  │
  ├─ 1. SIGTERM xray（等 3s，超时 SIGKILL）
  │     必须最先做：它还持有 utun fd，不先停掉接口不会消失
  │
  ├─ 2. helper.TunDown(session_id)
  │       state = TearingDown → 还原 DNS → 倒序删路由 → 删快照
  │
  └─ 3. helper 关闭自己持有的 utun fd → 接口消失
```

### 4.3 延迟探测

探测**不复用**正在运行的核心 —— Xray 的 SOCKS 入站无法按请求选择出站。
做法是临时启动一个独立核心实例，为每个节点开一个 SOCKS 端口：

```jsonc
// 探针专用配置（与用户正在用的实例完全隔离）
"inbounds": [
  { "tag": "probe-0", "port": 21000, "protocol": "socks", "udp": false },
  { "tag": "probe-1", "port": 21001, "protocol": "socks", "udp": false }
],
"routing": { "rules": [
  { "inboundTag": ["probe-0"], "outboundTag": "node-<id0>" },
  { "inboundTag": ["probe-1"], "outboundTag": "node-<id1>" }
]}
```

探针自己做最小 SOCKS5 握手（30 行，不引第三方库），连上后发一个
`GET http://cp.cloudflare.com/generate_204`，测量**首字节时间（TTFB）**。

不用 ICMP ping：ICMP 延迟和「通过代理访问一个网站要多久」几乎没有相关性。
TTFB 包含了 TLS 握手、代理转发、目标站响应全过程，才是用户真正感知的量。

## 5. 状态机

### 5.1 核心运行时（`CoreRuntime`）

```
        ┌─────────┐  start_proxy   ┌──────────┐
        │  Idle   │───────────────▶│ Starting │
        └─────────┘                └────┬─────┘
             ▲                          │
             │ stop_proxy          ┌────▼─────┐
             │                     │ Running  │
        ┌────┴─────┐               └────┬─────┘
        │ Stopping │◀───────────────────┘
        └──────────┘
```

TUN 模式下 `Running` 还带一个子状态：

```
routes_committed = false   ← 隧道已建，流量未接管（不应持续超过 1 秒）
routes_committed = true    ← 完全生效
```

UI **必须**显示前者：用户看到「已连接」但其实没生效是最容易引发不信任的体验。

### 5.2 会话快照（helper 侧，落盘）

```
（不存在）──TunUp──▶ BringingUp ──CommitRoutes──▶ Up
                         │                          │
                         │ 崩溃                      │ TunDown
                         ▼                          ▼
                    is_stale() = true          TearingDown → 删除快照
                         │
                    下次启动 restore_stale() 自动回滚
```

`is_stale()` 的判定包含一条容易漏掉的条件：**即使 `state == Up`，
只要 `pending_routes` 非空，也算未完成** —— 那说明两阶段启动在
`TunUp` 与 `CommitRoutes` 之间中断了。

## 6. 并发模型

| 位置 | 模型 | 理由 |
|---|---|---|
| helper socket | 每连接一个线程 | 消息频率极低（分钟级），线程模型最简单；而且「一问一答、不并发」正是 fd 传递时序正确性的前提 |
| Tauri commands | async（tokio） | 命令里要 await 进程启动与端口等待 |
| `AppState.inner` | `std::sync::Mutex` | 只覆盖纯内存操作，绝不跨 `.await` 持有 |
| `AppState.supervisor` | `tokio::sync::Mutex` | 启动流程本身是异步的，`std::sync::MutexGuard` 不是 `Send` |

一条硬规则：**`inner` 锁绝不跨 await 持有。** 耗时动作（起进程、改网络、跑探针）
都在锁外做，做完再短暂加锁写回结果。违反这条会让 UI 在启动核心时整体卡住。

锁中毒的处理是「恢复」而不是「放弃」：状态都是普通结构体，不存在被破坏的不变量；
如果直接放弃，用户就得重启 App。

## 7. 已知的架构级取舍

| 取舍 | 代价 | 为什么接受 |
|---|---|---|
| 配置变更走重启 | 切换节点有 ~300ms 停顿 | 省掉 `protoc` + tonic 代码生成；换节点是分钟级操作 |
| helper 用 launchd daemon 而非 `SMAppService` | 安装要输管理员密码 | 不依赖代码签名，开发期可跑。发行版必须切换（见 07） |
| fd 交付依赖 `XRAY_TUN_FD` | 上游只为 iOS/Android 承诺该路径 | 收益（内核不以 root 运行）远大于风险，且有完全公开的退路 |
| 规则中间表示 ≠ Xray 规则 | 需要一层编译 | 用户心智是「什么流量→怎么走」，Xray 是「条件合取+出站」，形状不同 |
| 不用 sqlite | 节点多了以后查询是线性扫描 | 数据量在几百条量级，而纯 JSON 让用户能直接打开排查 |
