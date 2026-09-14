# 更新记录

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
