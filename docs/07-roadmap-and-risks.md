# 07 · 风险、未完成项与路线图

---

## 1. 风险登记表

按「发生概率 × 影响」排序。每一条都写清楚**检测手段**，
因为无法检测的风险等于不知道它是否存在。

### R1 · `XRAY_TUN_FD` 路径被上游移除（中 / 高）

**是什么。** 我们依赖 darwin 的 TUN 入站在 `XRAY_TUN_FD` 存在时使用外部 fd
并跳过自己的地址/路由配置。这条路径在源码里是 `//go:build darwin` 分支
（Go 的 darwin tag 同时覆盖 `GOOS=darwin` 与 `GOOS=ios`），
但**上游文档只为 iOS / Android / Linux 承诺**了它。

一个上游重构（例如把 iOS 分支拆成独立 build tag）就会让它在 macOS 上失效。

**影响。** TUN 模式完全不可用 —— 症状是核心报「没有 TUN 设备」或
「无法创建 utun」，而 helper 那边显示一切正常。

**检测。**
* 启动日志里核心版本 + TUN 建立结果；
* 集成测试：建隧道后断言能通过隧道 `curl` 成功（见 [02 §7](02-tun-and-privileges.md#7-必须补的集成测试)）。

**缓解。** 协议里已经有 `DatapathPlan::SpawnDatapath { use_helper_fd: false }`
—— 让 Xray 自己建卡、自己配置，走完全公开的路径。
自动降级逻辑见 §3 路线图第 1 项。

---

### R2 · helper 安装被用户拒绝 / 未批准（高 / 中）

**是什么。** 用户没有完成管理员授权，或者在「系统设置 → 通用 → 登录项与扩展
→ 后台允许」里关掉了 helper。

**影响。** TUN 模式不可用（系统代理模式不受影响）。

**检测。**
* `HelperClient::availability` 区分三种情况：未安装 / 已安装但连不上 / 已就绪；
* 错误信息里包含「后台允许」等关键词时置 `needs_approval` 标志。

**缓解。** UI 上把「未安装」和「已安装但无法连接」显示成不同的文案与指引
（见 [05 §4](05-ui-spec.md#4-提示条把中间态说清楚)）。

---

### R3 · 崩溃后网络配置残留（中 / 高）

**是什么。** helper 或 GUI 在修改网络配置的中途被 `kill -9` / 断电。

**影响。** 用户断网，且不知道原因。

**缓解（这是本项目投入最多的地方）。**
* 快照在动手前落盘，之后**增量**更新；
* `pending_routes` 也落盘，覆盖「不确定装没装」的情况；
* helper 启动第一件事就是 `restore_stale()`；
* GUI 启动时检查 `stale_session` 并主动请求回滚；
* 托盘提供「修复网络」入口；
* 用 `/1` 拆分而不是替换默认路由 —— 最坏情况只是「一半流量走错路」，
  而不是「完全没有默认路由」。

**残余风险。** 如果 helper 二进制本身被删除（卸载脚本跑到一半），
磁盘上的快照就再也没有人去处理。缓解：`Uninstall` 请求里先回滚再删文件，
且卸载脚本的第一步是 `launchctl bootout`（先停服务，避免 launchd 反复重启
一个不存在的程序）。

---

### R3.5 · 防路由环失效（中 / 高）

**是什么。** TUN 接管默认路由后，代理服务器自身的流量也被送进隧道。

**已经发生过一次**，详见 [04 §8.1](04-routing-and-dns.md#81-一个真实的事故只留一种手段然后就失效了)。
根因是只依赖了核心侧的 `IP_BOUND_IF`，而它会与 `/1` 接管路由冲突并返回
`ENETUNREACH`。

**现在的防护（四层）：**
1. 服务器 IP 的 host 路由走物理网关，且**最先安装**；
2. `direct` 出站显式写 `streamSettings.sockopt.interface`（见
   [04 §8.2](04-routing-and-dns.md#82-direct-出站的第二个坑绑定了但没生效)）；
3. 核心侧 `autoOutboundsInterface` 作为第三道保险；
4. **启动时的差分探测** —— 接管路由前后各连一次服务器，前通后不通就立刻回滚。

**残余风险。** 节点用域名时，若 DNS 在运行期解析到新的 IP，
新 IP 没有 host 路由。缓解：`IP_BOUND_IF` 覆盖这种情况；
且 Xray 的路由监视器在自建模式下会更新接口。

### R4 · 与其它 VPN / 代理工具冲突（中 / 中）

**是什么。** 用户同时运行 Tailscale、WireGuard、Clash 等。

**影响。** 路由表被多方修改。`/1` 拆分的一个副作用是：
另一个工具如果也装了 `/1`，后装的会覆盖先装的（同前缀只有一个胜出）。

**检测。** 目前**没有**检测。这是一个已知缺口。

**缓解（计划中）。** 启动前检查路由表里是否已存在指向其它 utun 的 `/1` 路由，
有就在 UI 上警告；退出时只删自己装的、且删除失败不报致命错误。

---

### R5 · DNS 被第三方接管（中 / 低）

**是什么。** 用户的网络里有 AdGuard Home / dnsmasq / 企业 DNS，
或者装了会修改 DNS 的安全软件。

**影响。** helper 备份的是「当前值」，可能与用户以为的值不同；
回滚后用户会发现 DNS 变成了他不认识的设置。

**缓解。** 备份原值并在回滚时原样写回（而不是一律 `Empty`）。
快照里保存了完整的 `DnsBackup`，所以还原是精确的。

---

### R6 · 界面编辑规则造成优先级误解（高 / 低）

**是什么。** 自定义规则追加在预设之后，但用户可能以为自己的规则优先。

**影响。** 规则「不生效」，用户困惑。

**缓解。** 目前**不做**规则的界面编辑，只在页面上明确写出优先级关系
并给出直接改 JSON 的路径。做一个不能正确表达优先级的编辑器会更糟。

---

### R7 · 依赖 `/usr/bin/curl` 拉订阅（低 / 低）

**是什么。** 订阅拉取用系统 `curl` 而不是 HTTP 客户端库。

**为什么这么做。** macOS 自带 curl，支持 HTTPS（走系统信任链）、gzip、
重定向，零依赖。对「一天更新几次订阅」的频率，不能复用连接完全无所谓。

**风险。** 理论上用户环境里 curl 被替换/损坏。检测：拉取失败时错误信息里
包含 curl 的 stderr，一眼可辨。

---

## 2. 未完成项

按「不做会怎样」排序。

| # | 项 | 影响 | 工作量 |
|---|---|---|---|
| 1 | ~~`geoip.dat` / `geosite.dat` 的部署~~ **已完成**（`XRAY_LOCATION_ASSET`） | — | — |
| 2 | 自动降级到 `SpawnDatapath{use_helper_fd:false}` | R1 发生时 TUN 直接不可用 | 中 |
| 3 | 签名校验的集成测试 | `Security.framework` FFI 未经验证 | 中 |
| 4 | ~~utun / 路由 / DNS 的 root 集成测试~~ **代码已写**（`apps/desktop/examples/tun_smoke.rs`），待实际运行 | — |
| 5 | 流量统计的定时采样推送 | `TrafficSample::rx_rate` 当前恒为 0 | 小 |
| 6 | 系统代理模式的实际生效 | `ProxyMode::SystemProxy` 目前只生成 SOCKS/HTTP 入站，**没有改系统代理设置**（实测不需要 root，见 §2.2） | 小 |
| 7 | ~~开机自启~~ | 已在 0.2.0 实现（`SMAppService`，见 docs/02 §6.5） | — |
| 8 | 规则的界面编辑 | 见 R6 | 大 |
| 9 | 连接列表 | 需要 gRPC | 大 |
| 10 | 切换到 `SMAppService` | 发行版必需 | 中 |

### 2.1 关于第 1 项

这是最容易被忽略、影响却最直接的一项。

Xray 的 `geoip:cn` / `geosite:cn` 会在运行期去读工作目录下的
`geoip.dat` / `geosite.dat`。`XrayProcess::spawn` 把工作目录设成配置文件
所在目录（`~/Library/Application Support/com.xraytun.desktop/runtime/`），
而这两个文件在 app bundle 的 Resources 里。

**缺失时的行为是「规则不命中」，也就是静默地不分流** ——
用户会看到「绕过大陆」预设完全没起作用，而日志里没有任何错误。

修法有两种：
1. 复制 `.dat` 到 runtime 目录（首次启动时，或核心版本变化时）；
2. 通过命令行参数指定（`xray run -c <config> -confdir <dir>` 之类）。

推荐第 1 种：它让 runtime 目录成为完整的现场，便于排障时原样复现。

### 2.2 关于第 6 项

`ProxyMode::SystemProxy` 当前的行为是：启动核心并提供本机 SOCKS/HTTP 入站，
但**没有**调用 `networksetup -setwebproxy` 等去真的设置系统代理。

这意味着在系统代理模式下，用户需要手动把浏览器/系统代理指向
`127.0.0.1:10809`。这是一个明显的功能缺口。

**实测结论（2026-09，macOS 26 / M 系列）：`networksetup -setwebproxy` 作为普通用户
即可生效，不需要 root。** 这推翻了本文档早先的假设，也把这一项从「中等工作量」
降到了「小」。

```console
$ networksetup -setwebproxy Wi-Fi 127.0.0.1 10809   # 非 root
$ networksetup -getwebproxy Wi-Fi
Enabled: Yes  Server: 127.0.0.1  Port: 10809        # 确实改了
```

原因：系统代理是**按网络服务的用户级设置**，不是像 utun 那样的特权操作。
这也意味着它和路由/DNS 的性质不同 —— 后者是系统级、需要 root，前者不是。

### 因此的架构选择

**放在 GUI 侧，但沿用同一套快照纪律。**

```
GUI 改系统代理前：
  1. 备份三个服务（HTTP / HTTPS / SOCKS）的 Enable + Server + Port
  2. 原子写 ~/Library/Application Support/<id>/proxy-snapshot.json
  3. 逐个写入
  4. 启动时若发现快照是 stale → 先还原再提供服务
```

不放进 helper 的理由：helper 的整个设计前提是「必须 root 才能做的事」。
把一件不需要 root 的事塞进去，只会扩大它的接口面（更多请求类型、
更多快照字段、更多可能出错的地方），而收益为零。

但要保证纪律一致：**GUI 侧的快照同样要落盘、同样要在启动时检查 stale**，
否则就会出现「GUI 崩溃 → 系统代理指向一个已经不存在的端口 → 全网不通」。

### 一个真实事故（值得记下来）

开发过程中为了「测试 networksetup 是否需要 root」，直接执行了
`networksetup -setwebproxy` 而**没有先备份原值**，把用户 Wi-Fi 的 HTTP 代理
从 `127.0.0.1:7897` 改成了 `127.0.0.1:10809`（那个端口当时没有服务在听）。

这恰好违反了 [02-tun-and-privileges.md](02-tun-and-privileges.md#42-最容易毁掉口碑的坑)
里写的第一条原则。教训：

* 想确认一个命令是否需要权限，应该用**只读**命令（`-getwebproxy`）去推断，
  而不是执行写命令看它报不报错；
* 任何写操作前先记录原值 —— 哪怕只是「测一下」。

---

## 3. 路线图

### 阶段 1 · 让 TUN 真正可用（当前）

- [x] utun 创建 + 地址配置
- [x] 路由接管（`/1` 拆分 + bypass 顺序）
- [x] DNS 备份 / 修改 / 还原
- [x] 两阶段启动（避免黑洞窗口）
- [x] 会话快照 + 崩溃恢复
- [x] 特权 helper（协议 + 授权 + 安装）
- [x] Xray 配置生成（原生 TUN / Fake-IP / 四种订阅格式）
- [x] 延迟探针
- [x] Tauri 外壳 + React 界面
- [ ] `.dat` 文件部署 ← **下一步**
- [ ] 系统代理模式真正生效
- [ ] 流量采样定时推送

### 阶段 2 · 可靠性

- [ ] root 集成测试套件（见 [02 §7](02-tun-and-privileges.md#7-必须补的集成测试)）
- [ ] 签名校验的集成测试
- [ ] R1 的自动降级
- [ ] R4 的冲突检测
- [ ] 网络变化监听（切换 Wi-Fi / 插网线时重新探测物理出口并重建隧道）

阶段 2 的每一项都对应一个**已经写进文档但还没被代码覆盖**的失败模式。
在它们完成之前，这个项目适合自己用和内部测试，不适合推荐给普通用户。

### 阶段 3 · 发行就绪

- [ ] 切换到 `SMAppService`（需要真实的 Developer ID）
- [ ] 代码签名 + 公证
- [ ] 自动更新（`tauri-plugin-updater`）
- [ ] 崩溃上报（可选，需要明确告知用户）

### 阶段 4 · 功能完善

- [ ] 规则可视化编辑（先解决 R6 的优先级表达问题）
- [ ] 接入 gRPC：分节点流量、连接列表、免重启切换
- [ ] 节点分组与订阅自动更新
- [ ] 浅色主题 / 国际化

---

## 4. 第三方依赖与许可证

### 4.1 运行时

| 组件 | 许可证 | 分发方式 | 义务 |
|---|---|---|---|
| **Xray-core** | **MPL-2.0** | 独立进程调用，不链接 | 分发时附带其 LICENSE。若修改其源文件，被修改的文件需以 MPL-2.0 公开 |
| `geoip.dat` / `geosite.dat` | 随 Xray 发布 | 数据文件 | 同上 |
| `XTLS/libXray` | MIT | **未使用** | — |

**关于 MPL-2.0 的一个常见误解**：它常被误认为「比 MIT 严格得多」。
实际上 MPL-2.0 是**文件级** copyleft —— 只要不修改 Xray 的源文件、
仅以独立进程方式分发官方二进制，就没有额外义务。
真正需要注意的只有一条：不要静态链接它，也不要改它的源码后闭源分发。

### 4.2 构建期（Rust）

见 `Cargo.lock`。主要项：

| crate | 许可证 |
|---|---|
| `tauri` / `tauri-build` | MIT / Apache-2.0 |
| `tokio` | MIT |
| `serde` / `serde_json` | MIT / Apache-2.0 |
| `serde_yaml` | MIT / Apache-2.0（上游已停止维护，见下） |
| `base64` | MIT / Apache-2.0 |
| `url` | MIT / Apache-2.0 |
| `libc` | MIT / Apache-2.0 |
| `clap` | MIT / Apache-2.0 |
| `tracing` / `tracing-subscriber` | MIT |

**`serde_yaml` 已停止维护**（crate 描述里直接写着 `deprecated`）。
它只用于 Clash / Mihomo 订阅的解析，输入是我们控制不了的第三方数据 ——
也就是说这是个真实的攻击面。

替代方案（按推荐度）：
1. `serde_yaml_ng`（活跃维护的 fork，API 兼容）
2. `saphyr` / `yaml-rust2`
3. 自己写一个只支持 `proxies:` 列表的极简解析器

**建议在阶段 2 处理。** 目前的风险可接受（YAML 解析器的内存安全问题
需要构造特定输入，且订阅来源通常是用户自己选的机场），但不应该长期留着。

### 4.3 构建期（前端）

见 `apps/ui/package.json`。React / Vite / TypeScript / `@tauri-apps/api`
均为 MIT 或 Apache-2.0。

---

## 5. 安全审计建议

如果要把这个项目交给第三方审计，优先看这几处：

| 优先级 | 位置 | 关注点 |
|---|---|---|
| 高 | `crates/xt-helper/src/peer.rs` | `Security.framework` FFI 是否正确；要求串是否可被绕过 |
| 高 | `crates/xt-tun/src/validate.rs` | 白名单是否可被绕过（尤其是路径穿越的词法规范化） |
| 高 | `crates/xt-helper/src/server.rs` | `spawn_datapath` 的 `pre_exec` 是否引入了不安全状态 |
| 中 | `crates/xt-proto/src/transport.rs` | `SCM_RIGHTS` 与长度前缀的边界处理；`CMSG_*` 指针运算 |
| 中 | `crates/xt-tun/src/macos/utun.rs` | `ioctl` / `connect` 的裸 FFI 与缓冲区大小 |
| 中 | `crates/xt-tun/src/macos/snapshot.rs` | 快照文件的权限与完整性（当前没有签名/校验和） |
| 低 | `crates/xt-core/src/subscription/` | 解析不可信输入时的资源消耗（当前没有长度上限） |

### 5.1 两个已知的、可接受的风险

**(a) 快照文件没有完整性校验。** 它位于
`/Library/Application Support/XrayTun/`，只有 root 可写（目录 root:wheel 0755，
文件 0600）。能改它的进程已经是 root，不需要绕过我们。所以不加 HMAC。

**(b) 订阅解析没有输入长度上限。** 一个恶意订阅可以返回巨大的响应体。
`curl` 有超时但没有大小限制。实践中用户只会添加自己信任的订阅，
且这是**用户主动发起**的操作。建议加一个 8 MiB 上限（阶段 2）。

**(c) 客户端自更新只校验 sha256，没有签名。** 更新时比对 release 里的
`SHA256SUMS.txt`，并核对包内版本号与 release 声称的一致。这能防「下载损坏」
和「拿到名字对、内容错的包」，但**防不了「上游被换掉」** —— 校验和与被校验的
文件来自同一个 release，同源校验不构成信任根。真正的做法是签名 + 公钥内置，
前提是有 Developer ID 证书（见 `docs/02` 关于签名的说明）。在那之前，界面和
CHANGELOG 都如实写明这一点，不假装验过了。

顺带：客户端仓库**已改成公开**，所以自更新匿名可用，token 变成可选。
但仍建议填 —— 匿名配额只有 60 次/小时且**按 IP** 算，而请求多是经节点出去的，
等于和整台节点的用户共用，别人刷满就会报限流。填 token 可到 5000 次/小时。

与之配套：**403（限流）和 404（不存在/私有）必须给出不同的指引**。
两者都表现为「检查更新失败」，但处置完全不同 —— 前者等一会儿或填 token，
后者要去开权限。「拿不到更新」与「没有更新」也必须分开报，否则用户会以为
自己已经是最新版。

---

## 6. 发版流程（约定）

**打 tag 即发布，不需要再问。** 版本号改好、`./scripts/check.sh` 全绿之后：

```bash
git tag -a v0.6.1 -m "…" && git push origin v0.6.1
```

`release.yml` 会构建 universal 包（App / helper / 核心三者都是 x86_64 + arm64）、
跑一遍 `check.sh`、校验产物、创建 Release 并上传 `.dmg`、`.zip`、`SHA256SUMS.txt`。

发版前必须做的两件事：

1. **`./scripts/check.sh` 全绿**（CI 跑的是同一个脚本，见 `scripts/check.sh` 开头）；
2. **确认 CI 真的出了产物** —— 不要只看本地打出来的包。`gh release view v<版本>`
   应该能看到两个资产；下载回来 `lipo -archs` 确认三个可执行文件都是双架构。

> 这条是踩出来的：`universal-apple-darwin` 曾经被直接交给 `cargo build`，
> 而它只是 **Tauri CLI 的伪 target**，于是发版每次都死在「打包（universal）」，
> **Release 从来没被创建过**，而本地打的包看起来一切正常。

---

## 7. 参考

* [01-architecture.md](01-architecture.md) —— 整体设计
* [02-tun-and-privileges.md](02-tun-and-privileges.md) —— 系统集成与权限
* [03-xray-integration.md](03-xray-integration.md) —— 与 Xray 的契约
* [04-routing-and-dns.md](04-routing-and-dns.md) —— 分流与 DNS
* [05-ui-spec.md](05-ui-spec.md) —— 界面规格
* [06-helper-protocol.md](06-helper-protocol.md) —— 协议与授权
