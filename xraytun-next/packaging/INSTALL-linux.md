# xraytun-next v1.0.0 · Linux x86_64 核心（proxy 模式）

这个包里是 **xraytun-next**（xraytun 的平行重写）在当前开发机上能构建、能真跑的产物：
控制面 daemon 与无头客户端。**它不是 macOS 应用**，也没有 TUN。

## 包里有什么

```
bin/xt-daemon   控制面：状态机 / 设置 / 订阅解析 / 拉起 xray / 真实统计
bin/xt-cli      无头客户端：与 UI 走完全相同的契约（AF_UNIX + 长度前缀 JSON 帧）
LICENSE         MIT
```

## 运行需要什么

* Linux x86_64（glibc）。
* **xray 核心要你自己准备一只**：这个包**不附带** xray 二进制
  （第三方发行物、几十 MB，本仓库一贯不入库）。实测版本 `Xray 26.3.27 linux/amd64`。
  用 `--xray /path/to/xray` 或环境变量 `XT_XRAY_BIN` 指定。
* 节点来源是本机**订阅原文文件**（`--subscription-file`）。
  远端拉取（http/https）本轮**没有实现**，daemon 也不会宣告这个能力。

## 怎么跑（复制即用）

```bash
# 1) 把订阅原文放进状态目录（base64 整包 / URI 列表 / Clash YAML 均可）
mkdir -p /tmp/xt/state
printf 'vless://<uuid>@<host>:<port>?encryption=none&type=tcp&security=none#node1\n' \
  > /tmp/xt/state/subscription.txt

# 2) 起控制面
./bin/xt-daemon \
  --socket /tmp/xt/daemon.sock \
  --state-dir /tmp/xt/state \
  --xray /path/to/xray \
  --log-level info

# 3) 另一个终端：用无头客户端驱动它（和 UI 完全同一份契约）
./bin/xt-cli --socket /tmp/xt/daemon.sock hello
./bin/xt-cli --socket /tmp/xt/daemon.sock nodes
./bin/xt-cli --socket /tmp/xt/daemon.sock connect <node-id> --timeout-ms 15000
./bin/xt-cli --socket /tmp/xt/daemon.sock status      # stats 是真实 StatsService 采样
./bin/xt-cli --socket /tmp/xt/daemon.sock disconnect
./bin/xt-cli --socket /tmp/xt/daemon.sock events      # 持续事件流（Ctrl-C 退出）
```

默认 SOCKS 监听 `127.0.0.1:1080`，可用
`xt-cli patch-settings --socks-listen 127.0.0.1:11080` 改。

## 这个版本的边界（如实说）

| 能用的 | 不能用的 |
| --- | --- |
| proxy 模式：连接 / 断开 / 切节点 / 真实流量统计 | macOS TUN（本轮**未实现**）、特权 helper |
| 真进程、真字节、真事件驱动（无 sleep / 无轮询） | 远端订阅拉取、Tauri 图形界面（本包不含） |
| 失败如实上报（无回落、无自动重连、不重试） | macOS 上的任何二进制（本包只有 linux-x86_64） |

* 切节点语义是「选择就用」：失败就停在失败并报真实原因，**不会**自动换下一个。
* 未采样时 `stats` 为 `null`，界面/CLI 会显示「未采样」——**不会**用 0 顶替。

## 校验

```bash
sha256sum -c SHA256SUMS.txt
```
