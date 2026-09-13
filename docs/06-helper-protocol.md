# 06 · helper 协议与对端授权

---

## 1. 威胁模型

helper 以 root 运行，它的 Unix socket 就是一条**提权通道**。

假想的攻击者是**同机上的非特权进程** —— 例如用户从网上复制了一段脚本运行，
或者某个第三方程序被投毒。它想做的事是「借助 helper 获得 root 能力」。

我们需要保证的是：

> 即使攻击者能连上 socket、能构造任意请求，它能获得的最大能力
> 也只是「修改本机网络配置」，而不是「以 root 执行任意代码」。

---

## 2. 四层防御

| 层 | 机制 | 挡住什么 |
|---|---|---|
| 1. 文件系统 | socket `root:admin 0660` | 非 admin 组用户（内核在 `connect()` 就拒绝） |
| 2. 内核身份 | `getpeereid` + `LOCAL_PEERTOKEN` | 拿到真实 uid 与不可伪造的 audit token |
| 3. 代码签名 | `SecCodeCheckValidity` + audit token | admin 组内但不是我们签名的进程 |
| 4. 接口封闭 | 有限请求集 + 路径白名单 + 无 shell | 即使前三层全破，也只能改网络配置 |

### 2.1 为什么第 3 层用 audit token 而不是 pid

用 pid 做校验存在 **TOCTOU**：`SecCodeCopyGuestWithAttributes(kSecGuestAttributePid)`
按 pid 反查进程，而在「取到 pid」到「校验签名」之间，该 pid 可能被回收
并分配给另一个进程（本机进程创建是高频操作，这个窗口是真实存在的）。

**audit token** 是内核在 `connect()` 时给连接打上的不可伪造标识，
`getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN)` 取到 32 字节，
交给 `SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit)` 使用。
它描述的是「发起这次连接的那个进程」，不存在复用问题。

pid 仍然会取，但**只用于日志**。

### 2.2 要求串

```text
anchor apple generic
  and identifier "com.xraytun.desktop"
  and certificate leaf[subject.OU] = "<TEAM_ID>"
```

三条联合起来的效果是「只有我们签名的 App 能连上」。Team ID 通过
编译期环境变量 `XRAYTUN_TEAM_ID` 注入：

```bash
XRAYTUN_TEAM_ID=ABCDE12345 cargo build --release -p xt-helper
```

未注入时会退化成开发策略并**每次都打一条 warn 日志**：

```
构建时未设置 XRAYTUN_TEAM_ID，helper 将使用开发期策略（仅校验 socket 权限位）
```

这是刻意的：一个静默降级的授权检查比没有检查更危险，因为它会让人以为
「已经保护好了」。日志噪音是让人尽快发现配置错误的成本。

`PeerPolicy::build_env_policy_never_silently_trusts_everything` 测试确保
两种情况都有明确定义，不存在「什么都没配但看起来很安全」的中间态。

---

## 3. 传输层

### 3.1 一个被实测推翻的设计

最初选择了 `AF_UNIX` + **`SOCK_SEQPACKET`**。理由很充分：

* 报文边界天然保留 → 不需要长度前缀分帧 → 没有粘包/半包这一类 bug；
* `SCM_RIGHTS` 附带的 fd 一定和它所属的那条消息一起到达 → 不会错位。

**但 macOS 不支持它。**

```console
$ cargo test -p xt-proto transport
thread 'transport::tests::json_message_roundtrips' panicked:
  socketpair 失败: Protocol not supported (os error 43)
```

`EPROTONOSUPPORT (43)`。`AF_UNIX` 上的 `SOCK_SEQPACKET` 是 **Linux 特有**的，
Darwin 没有实现。

这个结论现在被钉在测试里：

```rust
#[test]
fn seqpacket_is_not_supported_on_macos() {
    // 谁要是又想「优化」回 SOCK_SEQPACKET，这个测试会立刻拦住他。
    ...
    assert_eq!(err.raw_os_error(), Some(libc::EPROTONOSUPPORT));
}
```

### 3.2 最终方案

`SOCK_STREAM` + `u32` 大端长度前缀分帧，fd 走「紧跟在帧后面的独立
1 字节 `SCM_RIGHTS` 消息」。

正确性依赖一条**明确的协议约束**：

> 连接是严格的一问一答，同一时刻只有一条在途消息。
> 客户端发出 `TakeTunFd` 后，必须先读完 `Response::TunFd` 帧，再读 fd。

因为双方严格同步，fd 消息不可能与任何其它帧交错。这不是「碰巧能跑」，
而是协议规定的时序。helper 每条连接一个线程也保证了一点。

`fd_follows_its_frame_in_order` 测试验证了三件事：fd 与它的帧配对、
下一条普通消息不带 fd、用收到的 fd 能读到正确的数据。

### 3.3 帧格式

```
┌──────────────┬────────────────────────────┐
│ u32 BE 长度   │ UTF-8 JSON 负载             │
└──────────────┴────────────────────────────┘
   4 字节           长度由前 4 字节给出，上限 64 KiB
```

选 JSON 而不是 protobuf：helper 的接口面很小、每秒消息数极低（分钟级），
可读性与可调试性远比编码效率重要。`MAX_MESSAGE = 64 KiB` 足够容纳
任何合理请求，超出即视为异常连接并断开。

### 3.4 fd 传递的两个 macOS 细节

**(a) 没有 `MSG_CMSG_CLOEXEC`。** 那是 Linux 的。macOS 上接收方必须自己
`fcntl(F_SETFD, FD_CLOEXEC)`，否则 fd 会泄漏给之后 `exec` 出去的所有子进程。
`recv_fd_raw` 做了这件事。

**(b) 但正因为库设了 CLOEXEC，spawn 数据面之前必须清掉它。**
否则核心继承不到 fd，症状是「helper 说交付成功了，但核心说没有 TUN 设备」。
见 `apps/desktop/src/supervisor.rs::clear_cloexec`。

另有一个必须显式检查的错误：`MSG_CTRUNC`。
控制消息被截断意味着 fd 没拿到，静默成功会变成极难排查的间歇性故障。

---

## 4. 请求集

```rust
pub enum Request {
    Hello { client_version, protocol, client_name },
    Status,
    TunUp(Box<TunUpRequest>),
    TakeTunFd { session_id },
    CommitRoutes { session_id },
    TunDown { session_id },
    Restore,
    Stats { session_id },
    Uninstall,
    Shutdown,
}
```

### 4.1 一次完整的 TUN 建立

```
C → Hello { protocol: 1 }
S → Hello(HelloInfo { helper_version, binary_sha256, tun_active, stale_session })

C → TunUp(TunUpRequest { session_id, mtu, addresses, routes, dns,
                         datapath: HandoffFd, defer_default_routes: true })
S → Ok { "接口 utun4（3 条 bypass 路由）已就绪，等待 CommitRoutes 接管默认路由" }
      ⚠ 此刻流量仍走原路，DNS 未改 —— 没有任何副作用暴露给用户

C → TakeTunFd { session_id }
S → TunFd(TunFdInfo { interface: "utun4", mtu: 1500, header_len: 4 })
S → [SCM_RIGHTS] utun fd

   （客户端在此之间拉起数据面并等待其就绪）

C → CommitRoutes { session_id }
S → Ok { "已接管默认路由（共 7 条）" }
```

### 4.2 为什么是两阶段

见 [01](01-architecture.md#41-启动-tun两阶段)。简言之：
把「流量被接管」与「数据面可用」之间的窗口压到零。

`defer_default_routes` 同时控制两件事：`0.0.0.0/1` 这类接管路由的安装，
**以及 DNS 的切换** —— 后者常被忽略，但 DNS 提前切到哨兵地址同样会导致
「所有网页都打不开」。

### 4.3 幂等与陈旧请求

* `session_id` 由客户端生成（用启动时刻的纳秒，天然单调，出问题时能从日志时间反查到是哪次启动）。
* `CommitRoutes` 在 `pending_routes` 已空时返回成功而不是错误 ——
  客户端重试时不该看到失败。
* `TunDown` 的 `session_id` 不匹配时**拒绝执行**，防止 GUI 的陈旧请求
  误拆掉后来建立的新会话。这种情况下会话被放回原处。
* `TunUp` 在已有活跃会话时直接拒绝：两个并发的 TUN 会话会互相踩路由，
  而「谁该负责回滚」会变得无法判定。

---

## 5. 输入校验（特权边界的护栏）

`xt-tun/src/validate.rs`。虽然用 argv 数组而不是 shell 已经杜绝了
`; rm -rf /` 这类注入，但仍需防止：

| 校验 | 防止 |
|---|---|
| `validate_interface_name` | 接口名以 `-` 开头 → 参数注入（`--help` 之类）；长度 > 15（`IFNAMSIZ-1`） |
| `validate_service_name` | 控制字符、引号、反引号、管道、`&`、`;` → 污染 `networksetup` 输出解析 |
| `validate_executable_path` | 路径穿越（先做**纯词法**规范化再比前缀，避免 TOCTOU）、相对路径、白名单外路径 |
| `validate_cidr_text` | CIDR 里混入非 `[0-9a-f:./]` 字符 |

白名单常量：

```rust
const ALLOWED_EXEC_ROOTS: &[&str] = &[
    "/Library/PrivilegedHelperTools",
    "/Library/Application Support/XrayTun",
];
```

`exec_whitelist_does_not_include_writable_user_dirs` 测试断言白名单里
不出现 `/tmp`、`/Users`、`/var/tmp` —— 这是安全不变式，不是可以「顺手放宽」的配置。

---

## 6. 会话快照

`/Library/Application Support/XrayTun/helper-session.json`，权限 0600。

```jsonc
{
  "session_id": "s1757...",
  "state": "bringing_up",          // bringing_up | up | tearing_down
  "interface": "utun4",
  "datapath_pid": 12345,
  "physical": { "interface": "en0", "gateway": "192.168.1.1", "service": "Wi-Fi" },
  "installed_routes": [ { "destination": "203.0.113.7/32", "via": {...} } ],
  "pending_routes":   [ { "destination": "0.0.0.0/1",     "via": {...} } ],
  "dns_backups": [ { "service": "Wi-Fi", "servers": ["1.1.1.1"] } ]
}
```

三个设计点：

**(a) 增量落盘。** 每装一条路由、每改一个服务的 DNS 都 `save()` 一次。
原子写（先写 `.tmp` 再 `rename`），避免断电留下半截 JSON。

**(b) `pending_routes` 也要落盘。** 两阶段启动期间崩溃时，我们**无法确定**
那些接管路由到底装上了没有。删除一条不存在的路由是安全的空操作
（`route::delete` 显式忽略 `not in table`），所以回滚时对 `installed_routes`
和 `pending_routes` 一并尝试删除 —— 「宁可多删一次」是这里唯一正确的策略。

**(c) `is_stale()` 包含「`state == Up` 但 `pending_routes` 非空」。**
这一条容易漏：它表示两阶段启动在中间中断了，会话看起来是好的但没生效。

---

## 6.5 一个真实的故障：陈旧 socket

**现象**（用户实际遇到）：

```
已安装但连不上：无法连接 helper（/var/run/com.xraytun.helper.sock）：
Connection refused (os error 61)
```

`ECONNREFUSED` 的确切含义是：**socket 文件存在，但没有进程在监听**。
它是一个独立的第三种状态，和「没装」「没授权」完全不同 ——
而最初的实现把它们混成了一句话。

**根因**：helper 退出时**不删除 socket 文件**。于是
`launchctl bootout` 杀掉旧实例之后、新实例 `bind()` 之前，
文件还在而监听者已经没了。这个窗口在「安装 → 卸载 → 再安装」
以及「重启 helper」时都会出现。

**修复**（四处，缺一不可）：

| # | 修复 | 位置 |
|---|---|---|
| 1 | helper 退出时 `remove_file(socket)`（`serve()` 返回路径 + 优雅退出路径） | `xt-helper/src/{server,main}.rs` |
| 2 | 客户端把 `ECONNREFUSED` / `ENOENT` / `EACCES` 归类成不同状态 | `helper_client.rs::classify` |
| 3 | 客户端首次连接**重试 8 次 × 200ms**，覆盖启动竞态 | `helper_client.rs::connect_fresh_with_retry` |
| 4 | 安装/重启脚本在 `bootstrap` 之后**轮询 `state = running`** 才返回成功 | `helper_install.rs` |

另有第 5 项：UI 在「已安装但没运行」状态下多一个**「重启 helper」**按钮 ——
修复它只需 `launchctl kickstart -k`，让用户为此去敲终端是不合理的。

### 教训：布尔量不够表达状态

最初 `HelperAvailability` 只有 `socket_present: bool` 和 `reachable: bool`。
两个布尔量能表达 4 种组合，但真实状态有 6 种，而且**每一种对应的用户动作都不同**：

| 状态 | 用户该做什么 |
|---|---|
| `NotInstalled` | 点「安装 helper」+ 输密码 |
| `NotRunning` | 点「重启 helper」（**不需要**输密码之外的任何东西） |
| `NotPermitted` | 换管理员账号 |
| `NeedsApproval` | 去系统设置里打开开关 |
| `Ready` | 什么都不用做 |

把 `NotRunning` 显示成「尚未安装或未授权」，用户就会去做两件**完全无用**的事。
状态机里省掉的那两个枚举值，最终会以「用户困惑」的形式付出代价。

## 7. 启动与关闭路径

### 7.1 启动：先回滚，再服务

```rust
pub fn recover_from_crash(&self) {
    match controller::restore_stale() { ... }
}
```

这一行是「helper 被 `kill -9` 之后用户不会永久断网」的**唯一**保障。
它必须在 `serve()` 之前执行。

### 7.2 关闭：同步回滚

helper 屏蔽 `SIGTERM`/`SIGINT`，用一个专用线程 `sigwait` 同步等待：

```rust
unsafe {
    libc::sigemptyset(&mut set);
    libc::sigaddset(&mut set, libc::SIGTERM);
    libc::sigaddset(&mut set, libc::SIGINT);
    libc::sigwait(&set, &mut sig);
}
graceful_shutdown();
```

**为什么不用 `signal()` 注册回调？** 因为回调运行在信号上下文里，
不能安全地调用 `route` / `networksetup`（会 malloc、会阻塞）。
`sigwait` 把信号变成普通的同步等待，之后就能随便做事了。

信号掩码必须在创建**任何其它线程之前**设置，这样新线程会继承它。
`sigwait` 要求信号处于屏蔽状态。

### 7.3 GUI 侧退出

托盘菜单的「退出」走 `shutdown_and_exit`，它是**完全同步**的：

1. 让 helper 按磁盘快照回滚（一次本机 IPC，~1ms）；
2. `SIGTERM` 核心进程；
3. `app.exit(0)`。

刻意不走 `Supervisor::stop`（异步），因为：

* 随后立刻 `app.exit(0)`，任何没被 poll 到的 async 清理都不会执行，
  用户就留在「路由指向一个已消失的 utun」的状态 —— 也就是彻底断网；
* `tauri::async_runtime::block_on` 在事件回调线程上可能死锁；
* 先 spawn 清理再退出，则清理与退出是竞态，等于没清理。

即使这一步也失败，helper 下次启动会读快照再回滚一次。
**这就是快照要落盘的原因。**

---

## 8. 安装与卸载

当前实现的是「写 `/Library/LaunchDaemons` + `/Library/PrivilegedHelperTools`」
这条路（方式 B），因为它不依赖代码签名，开发期就能跑通。

| | 方式 A：`SMAppService.daemon` | 方式 B：launchd plist |
|---|---|---|
| 系统版本 | macOS 13+ | 任意 |
| 需要密码 | ❌ 不需要 | ✅ 一次 |
| 用户操作 | 需要在「系统设置 → 通用 → 登录项与扩展 → 后台允许」中启用 | 无 |
| 代码签名 | **必须**（app 与 daemon 同 Team ID） | 不强制 |
| 布局要求 | plist 必须在 `Contents/Library/LaunchDaemons/<Label>.plist` | 自定义 |

**发行版必须切到方式 A。** 方式 B 用
`osascript ... with administrator privileges` 弹密码框 —— 这个模式虽然被
同类工具长期使用，但本质上是「让用户把管理员密码交给一个能跑任意脚本的通道」，
且在 App Store 分发中完全不可行。

不过即使走方式 B，我们也把 helper 二进制从一开始就放在
`Contents/MacOS/xraytun-helper` —— 这正是方式 A 的布局约定，
将来切换不需要动打包脚本。

### 8.1 plist 的几个选择

```xml
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>StandardErrorPath</key><string>/Library/Logs/XrayTun/helper.log</string>
```

* `KeepAlive` **不配 `SuccessfulExit`**：正常退出（例如响应 `Shutdown` 请求）
  后也希望能按需回来。
* **不加 `ProcessType: Background`**：那会让 launchd 抑制它的 CPU 配额，
  而建 utun / 改路由是交互式路径，被限速会表现为「点连接要等好几秒」。
* 日志写 `/Library/Logs/XrayTun/`：launchd 以 root 运行，
  写用户目录会因为权限失败，而且排障时找不到。

---

## 9. 必须补的测试

类型检查通过 **不等于** 运行时正确。下面这些只能在真实环境验证：

| 测试 | 为什么必须做 |
|---|---|
| **签名校验的正例与反例** | `Security.framework` 的 FFI 只保证类型层面正确；`kSecGuestAttributeAudit`、`SecRequirementCreateWithString` 的语义必须在真实签名的构建上验证。用一个未签名的测试进程连接，断言被拒 |
| socket 权限位 | 断言 `stat` 结果是 `0660 root:admin` |
| 跨进程 fd 传递 | 真实 helper + 真实客户端，断言收到的 fd 能读到 utun 的 IP 包 |
| `MSG_CTRUNC` 路径 | 用超长控制消息触发，断言显式报错而不是静默成功 |
| 崩溃恢复 | `kill -9` helper 后重启，断言遗留路由与 DNS 被清理 |
| 协议版本不匹配 | 用 `protocol: 999` 握手，断言返回 `ProtocolMismatch` |
| 重复 `TunUp` | 断言第二次被拒且第一次的会话不受影响 |

`sigwait` 的实现还有一个只能集成测的点：**信号掩码的继承顺序**。
如果将来有人在 `install_signal_handlers()` 之前创建了线程，
那些线程不会屏蔽信号，SIGTERM 可能被默认处理（直接终止进程）而不是
走到我们的清理逻辑。这个 bug 的表现是「有时候退出后网络没恢复」，
属于最难查的一类。

---

## 10. 参考

* `crates/xt-proto/src/lib.rs` —— 协议类型
* `crates/xt-proto/src/transport.rs` —— 分帧、fd 传递、对端凭据
* `crates/xt-helper/src/peer.rs` —— 授权与签名校验
* `crates/xt-helper/src/server.rs` —— 服务端与请求分发
* `crates/xt-tun/src/validate.rs` —— 输入校验白名单
* `crates/xt-tun/src/macos/snapshot.rs` —— 会话快照
* `apps/desktop/src/helper_install.rs` —— 安装/卸载脚本与 plist
