# xraytun-next

xraytun 的**平行重写**。旧版 `../xray-tun`（v0.9.2，已发布）保持冻结，本目录是 v1 架构的落地。

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
