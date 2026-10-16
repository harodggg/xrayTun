# xraytun-next

xraytun 的**平行重写**。旧版 `../xray-tun`（v0.9.2，已发布）保持冻结，本目录是 v1 架构的落地。

## 给想装它的人（macOS）

发布包在 [Releases](https://github.com/harodggg/xrayTun/releases)（`xraytun-next-v*`，预发布）。

**它没有 Apple 开发者签名（未公证）**，所以第一次双击会被 Gatekeeper 拦，弹「无法打开，
因为 Apple 无法检查其是否包含恶意软件」。那是系统对「没公证」的默认拦截，不是文件损坏。三种打开方式：

1. 在「应用程序」里按住 **Control** 点（或右键）XrayTun.app →「打开」→ 再点一次「打开」；
2. 或「系统设置 → 隐私与安全性」→ 找到 XrayTun →「仍要打开」；
3. 或终端：`xattr -dr com.apple.quarantine /Applications/XrayTun.app`

打开后的最短路径：**节点页**粘订阅原文/分享链接导入（或**设置 → 订阅**填 URL）→ 节点页
「选为当前节点」→ **仪表板**按「启动」（第一次会让你装特权助手，需一次管理员授权）。
同一份说明也在 DMG 里的 `首次打开请看这里.txt`；完整版见
[`docs/install/FIRST-LAUNCH.txt`](docs/install/FIRST-LAUNCH.txt)。

> 口径：**产品面只做 TUN**（用户 2026-10-16 裁决，放弃 SOCKS 代理）—— 界面上没有模式选择器，
> 设置页那一栏是 xray 的本机 API 地址，不是 SOCKS 入口；CLI 的 `connect` 也只发 `tun`。
> daemon 内部那条 SOCKS 数据面**保留**，但它只服务 CI 的端到端验证（TUN 要 root/助手，CI 跑不了），
> 不是产品能力。
>
> 口径：TUN 的**真机接管**（授权弹窗、utun、路由与 DNS）仍未验收；本 README 的"状态"一节
> 只写有证据的事，不说「能翻墙了」。

先读顺序：

1. [`docs/architecture/00-CONTRACT-FREEZE.md`](docs/architecture/00-CONTRACT-FREEZE.md) —— 契约、五条不变量、文件所有权（动手前必读）
2. [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) —— **为什么**是现在这个形状（逐节推导 + 图）
3. [`crates/xt-contract/`](crates/xt-contract) —— 跨边界词汇表的唯一事实源（代码）
4. [`docs/verification/`](docs/verification) —— 验证证据；本 README 的"状态"一节只写有证据的事

## 结构

```
crates/
  xt-contract   类型 / 错误 / 事件 / 帧（无 IO，无 unsafe）
  xt-state      连接生命周期状态机（纯函数）
  xt-bus        进程内事件总线
  xt-settings   设置（单文件 + 原子替换）
  xt-subs       订阅原文解析（纯函数）
  xt-xrayconf   Xray 配置生成（纯函数）
  xt-ipc        AF_UNIX 长度前缀 JSON 帧传输
  xt-nodes      节点目录 + 选择（不回落）
  xt-stats      真实字节数（StatsService）
  xt-probe      真实 TTFB
  xt-datapath   xray 子进程生命周期（事件驱动就绪）
  xt-daemon     组合根：控制面 daemon
  xt-cli        无头客户端（与 UI 同一契约）
apps/ui         React 界面（只渲染 + 采集输入）
scripts/guard.sh 不变量的机器判据
```

## 环境

```bash
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export PATH="$CARGO_HOME/bin:$PATH"
```

端到端测试需要**真 xray 二进制**（不附带、也不写死路径）：

```bash
export XT_XRAY_BIN=/path/to/xray      # 或者把 xray 放进 PATH
```

实测版本 `Xray 26.3.27 linux/amd64`。脚本与测试遵守同一条查找顺序：
`XT_XRAY_BIN`（旧名 `XRAY_BIN`）→ `PATH` 里的 `xray`；**找不到就失败，不会静默跳过**
（"跳过"会让"端到端通过"这句话失去依据）。

## 状态（2026-09-29，冻结树）

一条命令跑完所有验收：

```bash
bash scripts/verify.sh          # 全量（含真 xray 端到端与 UI）
bash scripts/verify.sh --fast   # 只跑 guard + clippy + 单测
```

最近一次结果：**7/7 全绿** —— guard / clippy / cargo test（128 passed）/
真 xray E2E（3 轮连接·真实字节·真实 stats）/ UI tsc / UI vitest（29 passed）/ UI build。

**已验证**：proxy 模式的连接·断开·切节点·真实流量统计；真进程、真字节、真事件驱动；
界面只绑定契约字段（未采样 ≠ 0）；五条不变量的机器判据。
**未验证**（不许写成可用）：macOS TUN 与特权 helper、macOS 真机、Tauri 生产通道、
订阅远端拉取、probe 的联网靶点、界面观感。完整清单见
[`docs/verification/FINAL-VERIFY.md`](docs/verification/FINAL-VERIFY.md) §5。

## 验证

```bash
bash scripts/guard.sh                                   # 五条不变量的机器判据
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p xt-daemon --test e2e_real_xray -- --nocapture   # 真进程 / 真字节

cd apps/ui && npm test && npx tsc --noEmit && npm run build
```

> 未验证的东西不写在这里。当前已验证/未验证的清单见 `docs/verification/FINAL-VERIFY.md`。
