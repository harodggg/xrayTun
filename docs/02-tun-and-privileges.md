# 02 · TUN 实现与权限模型

本文档覆盖本项目风险最集中的部分：如何在 macOS 上创建并接管一条隧道，
以及为此付出的权限代价。

所有涉及系统行为的事实都**经过实测或读取系统头文件核实**，
不是凭记忆写的。标注 `[实测]` 的结论附带对应的单元测试。

---

## 1. utun 是怎么创建出来的

macOS 没有 Linux 那样的 `/dev/net/tun`。要拿到一个 utun，必须走**内核控制**
（kernel control）这条路：

```c
fd = socket(PF_SYSTEM, SOCK_DGRAM, SYSPROTO_CONTROL);

struct ctl_info info = {0};
strcpy(info.ctl_name, "com.apple.net.utun_control");
ioctl(fd, CTLIOCGINFO, &info);              // ① 用名字换控制 id

struct sockaddr_ctl addr = {
    .sc_len      = sizeof(addr),
    .sc_family   = AF_SYSTEM,
    .ss_sysaddr  = AF_SYS_CONTROL,
    .sc_id       = info.ctl_id,
    .sc_unit     = unit + 1,                // ② 0 = 让内核挑，N+1 = utunN
};
connect(fd, (struct sockaddr *)&addr, sizeof(addr));

getsockopt(fd, SYSPROTO_CONTROL, UTUN_OPT_IFNAME, buf, &len);  // ③ 问内核要接口名
```

对应实现：`crates/xt-tun/src/macos/utun.rs`。

### 1.1 三个必须知道的坑

**(a) `CTLIOCGINFO` 的值不能靠推导。** `[实测]`

按 BSD 惯例，`_IOW('N', 3, struct ctl_info)` 应该是
`IOC_IN (0x80000000) | (100 << 16) | ('N' << 8) | 3` = `0x80644e03`。
**但 Darwin 实际用的是 `IOC_INOUT`**，真实值是 `0xc0644e03`：

```console
$ cc -o /tmp/x - <<'EOF'
#include <stdio.h>
#include <sys/kern_control.h>
int main(void){ printf("0x%08lx\n", (unsigned long)CTLIOCGINFO); }
EOF
$ /tmp/x
0xc0644e03
```

用错的症状非常具有误导性：`ioctl` 返回 `ENOTSUP (45)` 而不是预期中的
`EPERM`。看到 `ENOTSUP` 的第一反应通常是「内核不支持这个操作」，
而不是「我的 ioctl 请求码写错了」。

测试 `utun::tests::ctliocginfo_matches_system_header` 同时钉住了这个值和
`sizeof(struct ctl_info) == 100`（`u32 ctl_id` + `char ctl_name[96]`），
因为结构体布局是推导这个请求码的前提。

**(b) 每个包前面有 4 字节地址族头。** `[实测，见 utun.rs 的读写实现]`

`read()` 拿到的前 4 字节是大端 `u32` 的 `AF_INET (2)` / `AF_INET6 (30)`，
真正的 IP 包从第 5 字节开始；`write()` 时也必须自己补上这 4 字节。
忘了这件事的典型症状是「隧道起来了、路由也对、但完全不通」。

`UtunDevice::read_packet` / `write_packet` 封装了这个细节。

**(c) 需要 root。** 内核控制带 `CTL_FLAG_PRIVILEGED`，非 root 的 `connect()`
直接 `EPERM`。这正是必须有特权 helper 的原因。

单元测试 `create_without_root_fails_with_permission_error` 在非 root 环境下
断言失败发生在 `connect(utun)` 而不是 `ioctl`：如果 `ioctl` 就失败了，
说明坑 (a) 又回来了。

---

## 2. 谁持有 fd —— 权限模型的核心

utun 的生命周期**绑定在 fd 上**：所有 fd 关闭时接口消失。
于是「谁持有 fd」就等于「谁控制这条隧道」，也决定了数据面以什么权限运行。

```
            建卡 + 配地址 + 装路由 + 改 DNS          收发 IP 包
            （必须 root）                          （不需要 root）
                    │                                   │
  helper ───────────┘                                   │
     │                                                  │
     │  SCM_RIGHTS 交付 fd                               │
     ▼                                                  ▼
  xray（普通用户）───────────────────────────────────────┘
```

### 2.1 关键事实：内核只在 `connect()` 那一刻查权限

拿到 fd 之后，**任何进程**都能在它上面收发 IP 包，内核不再做任何检查。
所以「传 fd」不是授权边界 —— 谁拿到 fd 谁就控制整条隧道。

这意味着两件事：

1. 交付通道本身必须是可信的（我们用的是 root:admin 0660 的 socket +
   audit token 代码签名校验，见 [06](06-helper-protocol.md)）；
2. **接口/地址/路由/DNS 的配置仍然需要 root**，fd 只解决数据面。

### 2.2 与 Xray 的契约

Xray 在 darwin 上的 `NewTun` 会先检查环境变量：

```go
fdStr := platform.NewEnvFlag(platform.TunFdKey).GetValue(func() string { return "" })
if fdStr != "" {
    fd, _ := strconv.Atoi(fdStr)
    return &DarwinTun{tunFile: os.NewFile(uintptr(fd), "utun"), ownsFd: false, ...}, nil
}
// 否则自己建卡
```

`ownsFd = false` 的直接后果是 `Start()` **直接返回**、`setup()` **被跳过** ——
Xray 不会设 MTU、不会配地址、不会装路由。

**这不是缺陷，而是分工的接口。** 它明确地把「配置网络」的责任交给了
发 fd 的一方（我们的 helper）。反过来看，如果我们一边让 helper 配好，
一边又让 Xray 用 `autoSystemRoutingTable` 再配一遍，就会出现
「谁负责删」的歧义 —— 而删漏的后果是用户永久断网。

因此配置生成时 **`autoSystemRoutingTable` 始终为空**，
只保留 `autoOutboundsInterface`（那是防环用的，见 [04](04-routing-and-dns.md)）。

### 2.3 fd 传递的实现细节

`xt_proto::transport` 里的 `recv_fd_raw` / `send_fd_raw`。

* macOS **没有 `MSG_CMSG_CLOEXEC`**（那是 Linux 的），接收方必须自己
  `fcntl(F_SETFD, FD_CLOEXEC)`，否则 fd 会泄漏给之后 `exec` 出去的所有子进程。
  实现里做了这件事。
* **但正因为库帮我们设了 CLOEXEC，supervisor 在 spawn xray 之前必须显式清掉它**
  （`supervisor::clear_cloexec`），否则核心继承不到 fd，表现为
  「helper 说交付成功了，但核心说没有 TUN 设备」。
* `MSG_CTRUNC` 必须显式检查。控制消息被截断意味着 fd 没拿到，
  静默成功会变成极难排查的间歇性故障。

### 2.4 三种数据面模式

协议里的 `DatapathPlan` 表达了三种分工，按推荐程度排序：

| 模式 | 谁建 utun | 谁跑数据面 | 数据面权限 | 说明 |
|---|---|---|---|---|
| `HandoffFd` | helper | GUI 拉起的 xray | 普通用户 | ✅ 默认。最小权限 |
| `SpawnDatapath { use_helper_fd: true }` | helper | helper 拉起的子进程 | **root** | 兼容路径 |
| `SpawnDatapath { use_helper_fd: false }` | 子进程自己 | 子进程自己 | **root** | 完全公开的上游路径，最保守的退路 |

---

## 3. 路由接管策略

### 3.1 用 `/1` 拆分，而不是删掉默认路由

```bash
route -n add -net 0.0.0.0/1   -interface utun4
route -n add -net 128.0.0.0/1 -interface utun4
```

`0.0.0.0/1` 覆盖 `0.0.0.0`–`127.255.255.255`，`128.0.0.0/1` 覆盖
`128.0.0.0`–`255.255.255.255`，两者合起来覆盖整个 IPv4 空间，
且都比默认路由的 `/0` **更具体**，因此总是胜出。

相比「删掉默认路由再指向 utun」：

| | `/1` 拆分 | 替换默认路由 |
|---|---|---|
| 原默认路由 | 完好无损 | 被删除，必须自己记住原值 |
| 回滚 | 删两条即可 | 必须精确还原原来的 gateway/interface |
| 崩溃后 | 接口消失时内核自动清理挂在它上面的路由 | 可能永久失去默认路由 → **彻底断网** |
| 部分失败 | 最坏情况是「一半流量进隧道」 | 最坏情况是断网 |

IPv6 同理，用 `::/1` + `8000::/1`（仅在 `Ipv6Mode::Override` 时）。

### 3.2 顺序：先绕过，后接管

```
1. 建 utun、配地址
2. 装 bypass 路由（代理服务器 IP → 物理网关；内网网段 → 物理出口）
3. 装 0.0.0.0/1 + 128.0.0.0/1
4. 改 DNS
```

**第 2 步必须早于第 3 步。** 反过来的话，中间会有一个窗口：
默认流量已经进隧道，但隧道里的数据还要再连代理服务器 →
代理服务器本身又被送进隧道 → **路由环**。现象是连接完全打不通，
而且因为它是瞬时的，用 `netstat -rn` 抓不到现场。

`plan::tests::proxy_host_route_precedes_default_capture` 钉住了这个顺序。

### 3.3 防环：两条独立的保险

| 手段 | 位置 | 原理 |
|---|---|---|
| `autoOutboundsInterface` | Xray 出站 socket | `IP_BOUND_IF` 把 socket 绑到物理网卡，包在路由决策**之前**就确定了出口 |
| host 路由 | 系统路由表 | 给代理服务器 IP 单独一条更具体的路由指向物理网关 |

只依赖一种是不负责任的：`IP_BOUND_IF` 在接口探测失败时会静默不绑定；
而 host 路由在「代理服务器使用域名 + 解析结果变化」时会失效。
两条一起上，任何一条生效就不会成环。

---

## 4. DNS 接管

### 4.1 为什么必须显式处理

macOS 的 DNS 是**按网络服务（network service）**配置的，不是按接口：

```
en0  ──▶  "Wi-Fi"
en7  ──▶  "USB 10/100/1000 LAN"
```

所以改 DNS 之前必须先做一次「设备名 → 服务名」的映射
（`networksetup -listnetworkserviceorder`）。

### 4.2 最容易毁掉口碑的坑

`networksetup -setdnsservers` 写进去的值**会一直留着**，即使隧道已经拆了。
用户会看到「代理关了但所有网站都打不开」—— 这是这类工具最经典的差评来源。

因此本项目的规则是：

> **改之前一定先备份，回滚时一定还原，且备份要落盘。**

还原时用 `Empty` 而不是「写回原来的 IP」：`networksetup -getdnsservers`
无法区分「用户手动设过 DNS」和「系统默认（DHCP 下发）」，而 `Empty`
正是「交还给 DHCP」的语义。原本就有手动配置的情况，我们会把原值一起存进
快照并在回滚时写回。

### 4.3 哨兵 DNS

TUN 模式写入系统的不是真实解析器，而是**隧道网段内的一个不可路由地址**
（默认 `198.18.0.2`）：

```
应用发起 DNS 查询 → 系统把它发给 198.18.0.2
                  → 目标在 198.18.0.0/15（被 0.0.0.0/1 送进隧道）
                  → 从 utun 出来 → Xray
                  → 路由规则 `port: 53 → dns-out`
                  → 内核 DNS 模块应答
```

好处是**不需要 root 占用 53 端口**，也不依赖 `pf` 做重定向。
坏处是这个地址必须真的落在隧道网段内，否则查询会走物理网卡直接失败 ——
所以 `TunSettings::network_cidr()` 的解析结果会被校验。

### 4.4 DNS 也要延迟到 CommitRoutes

和默认路由同理：如果 `TunUp` 阶段就把 DNS 切到哨兵地址，而数据面还没起来，
所有域名解析会立刻失败。所以 `apply()` 里 DNS 的写入受 `defer_default_routes`
控制，与 `/1` 路由一起在 `CommitRoutes` 阶段生效。

---

## 5. 网络服务名解析的一个真实 bug

`networksetup -listnetworkserviceorder` 的输出长这样：

```
An asterisk (*) denotes that a network service is disabled.
(1) Wi-Fi
(Hardware Port: Wi-Fi, Device: en0)

(2) Thunderbolt Bridge
(Hardware Port: Thunderbolt Bridge, Device: bridge0)
```

解析时踩了两个坑，都是被单元测试抓出来的：

1. **`(Hardware Port: ...)` 也以 `(` 开头。** 如果先做 `(N) 服务名` 的匹配，
   设备行会被误认成服务名，于是永远匹配不到设备。
2. **`Device:` 后面跟着一个空格。** 直接对 `Device:` 之后的部分做
   `take_while(非空格)` 会立刻停在空格上，返回空串。

两个 bug 都不会 panic，只会让「查不到网络服务」——
表现为「TUN 模式起来了但 DNS 没改」。见
`xt-tun/src/macos/dns.rs` 的 `parse_service_order` / `extract_device`。

---

## 6. 为什么不用 pfctl

macOS 没有 Linux 的 policy routing：没有 `ip rule`、没有 fwmark、
没有基于 uid/cgroup 的路由选择。最接近的是 per-socket 的
`IP_BOUND_IF`（需要程序配合，做不到系统级）。

理论上 `pfctl` 能做 `rdr` / `route-to`，但：

* macOS 自带的是较老的 OpenBSD pf；
* 对**本地生成**流量的 `rdr` 支持很脆，通常要配 `route-to` / `reply-to` 才生效；
* anchor 是单设备的，与「多个 VPN/代理工具共存」天然冲突。

业界的 macOS TUN 客户端基本都不用它。我们也不用。

如果将来需要「按进程分流」，正确的方向是 Xray 的
`processName` 规则字段（本项目在 `MatchCondition` 里已经预留），
而不是 pf。

---

## 7. 必须补的集成测试

单元测试覆盖了解析、顺序、回滚逻辑，但**下面这些只能在真实 root 环境验证**：

| 测试 | 内容 |
|---|---|
| utun 生命周期 | 建卡 → `ifconfig` 能看到地址与 MTU → 关闭全部 fd → 接口消失 |
| 路由回滚 | 装 4 条路由后模拟中途失败，断言路由表回到初始状态 |
| DNS 回滚 | 改 DNS → 回滚 → `networksetup -getdnsservers` 与备份一致 |
| 崩溃恢复 | `kill -9` helper → 重启 → 断言遗留路由与 DNS 被清理 |
| 真实数据面 | 建隧道 → 跑 Xray → `curl` 通过隧道成功 |
| 权限拒绝 | 非 admin 组用户连接 socket → 断言被拒 |
| 签名校验 | 未签名/异签名进程连接 → 断言被拒 |

建议做法：在 CI 里用一台 macOS runner 的 `sudo` 执行，且**每个测试都在
独立的网络命名空间之外、用可回滚的断言**（测试结束时无条件还原）。

---

## 8. 参考

* `crates/xt-tun/src/macos/utun.rs` —— utun 创建与包读写
* `crates/xt-tun/src/macos/route.rs` —— 路由增删与顺序
* `crates/xt-tun/src/macos/dns.rs` —— DNS 备份/修改/还原
* `crates/xt-tun/src/macos/controller.rs` —— 两阶段启动与回滚
* `crates/xt-tun/src/macos/snapshot.rs` —— 崩溃可恢复的快照
* `crates/xt-tun/src/plan.rs` —— 变更计划的纯函数计算（最容易单测的部分）
