# xraytun-next v1.0.0 · macOS universal 核心（proxy 模式）

这个包里是 **xraytun-next**（xraytun 的平行重写）的 macOS **命令行核心**：控制面 daemon 与
无头客户端，`universal` 二进制（Apple Silicon 与 Intel 同一份）。

**它不是 XrayTun.app**：这一版还没有 TUN、没有菜单栏界面。完整的 `.app` 在后续阶段交付
（见 `docs/design/MACOS-APP-PLAN.md`）。这里能用的能力是 proxy（SOCKS）模式：
连接 / 断开 / 切节点 / 真实流量统计。

## 包里有什么

```
bin/xt-daemon   控制面：状态机 / 设置 / 订阅解析 / 拉起 xray / 真实统计
bin/xt-cli      无头客户端：与图形界面走完全相同的契约（AF_UNIX + 长度前缀 JSON 帧）
LICENSE         MIT
```

## 运行需要什么

* macOS 13+；`universal`（arm64 + x86_64）。
* **xray 核心要你自己准备**：本包**不附带** xray（第三方发行物、几十 MB，本仓库一贯不入库）。
  从 XTLS/Xray-core 取 macOS 版（`Xray-macos-arm64-v8a.zip` 或 `Xray-macos-64.zip`），
  或直接把两个架构用 `lipo` 合成 universal。实测 `26.3.27`。
* 节点来源是本机**订阅原文文件**（`--subscription-file`）。远端拉取（http/https）本轮没做。

## 首次运行：Gatekeeper

这个包**没有做 Apple 公证**（需要开发者账号与凭据）。从浏览器下载后 macOS 会加隔离标记，
命令行程序会被拦。解除方式：

```bash
xattr -dr com.apple.quarantine ./bin/xt-daemon ./bin/xt-cli
```

（命令行工具通常不弹窗；如果被拦，上面这条就是解法。真正发行时应当走签名 + 公证。）

## 怎么跑（复制即用）

```bash
mkdir -p /tmp/xt/state
printf 'vless://<uuid>@<host>:<port>?encryption=none&type=tcp&security=none#node1\n' \
  > /tmp/xt/state/subscription.txt

./bin/xt-daemon --socket /tmp/xt/daemon.sock --state-dir /tmp/xt/state \
  --xray /path/to/xray --log-level info

# 另一个终端
./bin/xt-cli --socket /tmp/xt/daemon.sock hello
./bin/xt-cli --socket /tmp/xt/daemon.sock nodes
./bin/xt-cli --socket /tmp/xt/daemon.sock connect <node-id> --timeout-ms 15000
./bin/xt-cli --socket /tmp/xt/daemon.sock status      # stats 是真实 StatsService 采样
./bin/xt-cli --socket /tmp/xt/daemon.sock disconnect
```

默认 SOCKS 监听 `127.0.0.1:1080`，可用
`xt-cli patch-settings --socks-listen 127.0.0.1:11080` 改。

## 这个版本的边界（如实说）

| 能用的 | 不能用的 |
| --- | --- |
| proxy 模式：连接 / 断开 / 切节点 / 真实流量统计 | TUN（接管全机流量）、特权 helper、菜单栏界面 |
| 真进程、真字节、真事件驱动（无 sleep / 无轮询） | 远端订阅拉取、签名与公证 |
| 失败如实上报（无回落、不自动重连、不重试） | — |

## 校验

```bash
shasum -a 256 -c SHA256SUMS.txt 2>/dev/null || shasum -a 256 -c ../SHA256SUMS-macos.txt
```
