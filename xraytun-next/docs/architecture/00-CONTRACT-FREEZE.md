# 00 · 契约冻结（v1）—— xraytun-next 所有人先读这一页

> 冻结的意思是：**接口先定，实现后写**。三个后端、一个前端并行开工的前提是
> 他们不用互相等。这一页 + `crates/xt-contract/` 的代码就是那份「不用等」的东西。
>
> 唯一事实源是 **代码**：`crates/xt-contract/src/{error,model,protocol}.rs`。
> 这一页解释**为什么**是那个形状；两者不一致时以代码为准，并立刻回来改这一页。
>
> 冻结范围：`xt-contract` 的公开类型、`xt-ipc` 的帧格式、crate 之间的依赖方向、
> 文件所有权（§9）。**动冻结项前必须先发消息给 lead**，由 lead 改这里再改代码。

## 0. 五条不可协商的不变量

| # | 不变量 | 判据（可被机器检查） |
| --- | --- | --- |
| I1 | **没有等待**：不许 `sleep`、不许轮询、不许固定延时用来同步 | `scripts/guard.sh` 全仓扫描；就绪只由「事件 + 一次非阻塞尝试」判定 |
| I2 | **没有回落**：一条意图只有一条路径；失败就是如实上报的终态 | `ErrorCode` 是封闭枚举且无 `Retry/Fallback/Degraded`；guard 扫 `fallback`/`retry`/`backoff` 标识符 |
| I3 | **没有假话**：显示项必须能追到真实字段；未知就是 `None`/「未采样」 | 契约里没有「默认 0 字节」「默认延迟」这类字段；UI 守卫测试逐条断言溯源 |
| I4 | **单向依赖**：底层不知道上层；UI 不认识 Rust 内部类型 | `cargo tree` + crate 分层表（§2） |
| I5 | **权限最小面**：只有 helper 改系统网络，且它只认识封闭指令集 | helper 本轮**不实现**（§8），因此任何「TUN 已工作」的说法都是假话 |

## 1. 进程图

```
┌────────────────────────────┐
│ UI（React + Vite）          │  只渲染 + 采集输入；不含业务规则
│ apps/ui                     │
└─────────────┬──────────────┘
              │ AF_UNIX · 长度前缀 JSON 帧（xt-ipc）
              │ /run/xraytun/daemon.sock
┌─────────────▼──────────────────────────────────────────────┐
│ xraytund（控制面 daemon，普通用户，无特权）                    │
│  settings │ subs │ nodes │ lifecycle(xt-state) │ datapath    │
│  stats    │ probe │ bus   │ ipc-server                       │
└──────┬──────────────────────────────────────────┬───────────┘
       │ spawn / stop / kill（父子进程）             │ 封闭指令集（本轮未实现）
┌──────▼───────────────┐                  ┌───────▼──────────────────┐
│ xray（数据面，上游）    │                  │ xraytun-helperd（root）    │
│  真实进程、真实字节数    │                  │  utun/route/DNS/快照       │
└──────────────────────┘                  └──────────────────────────┘
```

**为什么 daemon 是独立进程**（而不是像 v0.9.2 那样把逻辑放在 Tauri 进程里）：

1. UI 崩溃/重载不牵连隧道；2. 业务逻辑可以**脱离 WebView**在 CI 里跑真实验证；
3. CLI 与 UI 走**同一份契约**，所以「UI 能做的，无头也能做」，反之亦然；
4. 崩溃被限制在一个进程里，错误归属清楚。

代价：多一次 IPC 往返、多一个进程要管。**只有当收益大于代价才引入进程边界** ——
本轮只有这一处（UI ↔ daemon）。不引入网络微服务：这些「服务」之间没有独立伸缩与
独立部署的需求，跨 TCP 只会带来端口冲突、延迟和打包复杂度。

## 2. crate 分层与依赖方向

| 层 | crate | 职责 | 允许依赖 |
| --- | --- | --- | --- |
| L0 | `xt-contract` | 类型 / 错误 / 事件 / 帧 | serde、thiserror |
| L1 | `xt-state` | 连接生命周期状态机（纯函数） | L0 |
| L1 | `xt-bus` | 进程内事件总线（watch/broadcast） | L0、tokio(sync) |
| L1 | `xt-settings` | 设置读写（单文件 + 原子替换） | L0 |
| L1 | `xt-subs` | 订阅解析（4 种格式，纯函数） | L0、base64/url/yaml |
| L1 | `xt-xrayconf` | Xray JSON 生成（纯函数） | L0、serde_json |
| L2 | `xt-ipc` | AF_UNIX 帧传输，client/server | L0、tokio(net,io) |
| L2 | `xt-nodes` | 节点目录 + 选择（无回落） | L0、L1 |
| L2 | `xt-stats` | StatsService 真实字节计数（h2c） | L0、h2/http |
| L2 | `xt-probe` | 真实 TTFB 探测 | L0、L1 |
| L2 | `xt-datapath` | xray 进程生命周期 + 事件驱动就绪（socks + required_addrs 全部接受才算就绪） | L0 |
| L3 | `xt-daemon` | 组合根：装配服务、对外提供 IPC | 全部 |
| L3 | `xt-cli` | 无头客户端（端到端入口） | L0、xt-ipc |

`cargo tree --workspace` 必须**没有环**。新增 crate 前先改这张表。

### 2.1 跨 crate 调用面（冻结，避免三线各写一版）

```rust
// xt-subs
pub struct ParsedNode { pub id: NodeId, pub name: String, pub protocol: String, pub endpoint: String, pub outbound: serde_json::Value }

// xt-xrayconf（L1，只依赖 xt-contract —— 所以它不认识 ParsedNode）
pub struct OutboundSpec { pub node_id: NodeId, pub outbound: serde_json::Value }
pub struct ConfigInputs { pub listen_socks: SocketAddr, pub api_listen: SocketAddr, pub selected: OutboundSpec, pub log_level: LogLevel }
pub fn generate(input: &ConfigInputs) -> Result<String, ErrorBody>;
/// 探测用：临时实例，每个节点一个独立 socks 入站端口；返回 (配置, 端口列表)
pub fn generate_probe(outbounds: &[OutboundSpec], base_port: u16) -> Result<(String, Vec<u16>), ErrorBody>;

// xt-nodes（同时依赖 xt-subs 与 xt-xrayconf，所以转换放在这里）
impl Catalog { pub fn outbound_spec(&self, id: &NodeId) -> Result<xt_xrayconf::OutboundSpec, ErrorBody>; }

// xt-datapath
pub struct DatapathSpec {
    pub xray_bin: PathBuf,
    pub config_path: PathBuf,
    pub socks_addr: SocketAddr,
    /// 除 socks 之外**必须一起就绪**的端口（当前是 StatsService 的 api 入站）。
    /// 为什么不是"就绪后再去连"：api 与 socks 不是同一个就绪事件，
    /// 就绪后一次性 connect 会撞 refused，整段会话显示"未采样"（真事故，见 ux 的 LIVE-EVIDENCE §4）。
    pub required_addrs: Vec<SocketAddr>,
    pub log_level: LogLevel,
}
pub async fn start(spec: &DatapathSpec) -> Result<RunningDatapath, ErrorBody>;

// xt-stats / xt-probe
impl StatsClient { pub async fn connect(api_addr: SocketAddr) -> Result<StatsClient, ErrorBody>; pub async fn sample(&self) -> Result<StatsView, ErrorBody>; }
```

daemon 负责算 `api_listen = socks 端口 + 1`（对用户不暴露成设置项）。

## 3.1 能力宣告（新增的硬规则）

`DaemonHello.capabilities` 是**唯一**的能力事实来源：

* 界面**不得渲染**未在 `capabilities` 里宣告的入口（这是 I3 的机制化：未实现的能力 = 看不见，
  而不是一个按不动的灰按钮）。
* daemon 收到未宣告能力的请求时返回 `ErrorCode::Unsupported`。
  这个错误码**只能**用于"本版本不提供该能力"，**绝不能**用来包装"我试了但失败了"
  （那必须用具体失败码：config_invalid / core_exited_early / io …），否则它就成了新的兜底。
* 本轮能力现状：proxy 模式 + stats = 要宣告；probe = 实现成功才宣告；
  `Subscriptions`（**本地订阅原文解析**）= 要宣告；
  `SubscriptionFetch`（http/https 拉取）与 `TunMode` = 本轮不宣告。
  `AddSubscription` / `RefreshSubscription` 属于拉取语义 → 返回 `Unsupported`。
* 能力粒度必须与**用户可做的动作**对齐：`Subscriptions` 与 `SubscriptionFetch` 之所以分开，
  是因为合成一个会让界面只剩两个坏选择——显示一个按不动的"刷新"（假控件），
  或者把本地解析也一起藏掉。

## 3. 协议

以 `crates/xt-contract/src/protocol.rs` 为准。三条容易踩的：

* 一条新连接的第一个请求**必须**是 `hello`；`protocol_version != PROTOCOL_VERSION`
  一律 `invalid_request` 并关闭连接（不做兼容）。
* `connect`/`disconnect`/`switch_node`/`probe_nodes` 只返回 `Accepted`；
  终态**一定**以 `Event` 到达（成功或失败都会到达，不允许石沉大海）。
* `Event` 帧的 `seq` 从 1 连续自增；客户端发现跳号要如实报告（别静默）。

## 4. 状态机（`xt-state`）

内部状态机与 wire 形状**不必同形**（内部服务于正确性，wire 服务于消费者），
但只允许一个方向转换 `state → ConnectionView`，并且必须有穷举测试。

```
Disconnected ──connect──▶ Connecting{PreparingConfig→StartingCore→AwaitingReady}
                                   │(tun) → CommittingRoutes
                                   ▼
                              Connected ──disconnect──▶ Disconnecting ──▶ Disconnected
                                   │
                                   └─ 数据面提前退出/失败 ──▶ Disconnected + last_error（不重试、不切下一个节点）
```

不变量：
* `stage == Connected` ⟺ 数据面**已被证明可连**（proxy：SOCKS 端口接受连接）。
  界面显示「已连接」当且仅当它成立。
* 进入 `Connected` 时 `connected_since_ms` 必须来自真实时钟，不是计数器。
* 任何失败路径都必须落到 `Disconnected` 或 `Disconnecting`，且带 `last_error`。

## 5. 无等待（I1）细则

**禁止**：`tokio::time::sleep`、`std::thread::sleep`、`Instant::now()` 循环轮询、
`setInterval`/`setTimeout`（UI 动画除外）、任何「先等 X 毫秒再检查」的写法。

**允许**：
* `tokio::time::timeout(deadline, fut)` —— 只能作为**失败上限**，超时必须产生
  一个具体的 `ErrorBody`（例如 `CoreExitedEarly`），并且**不得**用来循环重试。
* 子进程的 stdout/stderr 逐行事件：核心每输出一行就触发一次「试连」判断
  （v0.9.2 已验证过的 `wait_ready` 思路）。
* `tokio::select!` 等事件源。

自检：`bash scripts/guard.sh`。

## 6. 无回落（I2）细则

* 节点切换 = 「选择就用」。切过去失败就停在失败，报真实原因，**不**自动换下一个节点。
* 启动失败不自动重连、不重试、不降级到直连。
* 不写「兼容旧版」分支；版本不符就拒绝。
* 允许的**回滚**只有一种：崩溃/退出时把已经改掉的本机状态还原（helper 快照）——
  那是清理，不是「换一条路成功」。本轮 helper 未实现，因此这条只是设计约束。

## 7. 真实数据（I3）细则

* UI 不许有 `preview*` / `mock*` / 假 fixture 作为**运行时**数据源。
  测试里构造 fixture 允许，但必须由契约类型构造，且必须标注为测试数据。
* 统计只有两个来源：真实 `StatsService` 采样、真实进程事件。没有第三条。
  采样未成功 → `stats: null` → 界面显示「未采样」，**不显示 0**。
* 版本号、pid、启动耗时、TTFB、字节数全部来自真实观测。

## 8. 本轮纵切面（要交付并验证的东西）

**范围**：`proxy` 模式下的 连接 / 断开 / 切节点 / 真实流量统计 + UI 真实绑定。

**明确不做（并因此不作任何声明）**：macOS TUN、特权 helper、订阅之外的账号体系、
审计上传、地理可视化。旧仓库 `xray-tun/` 一行都不改。

**端到端验收（本机 Linux，真实进程、真实字节）**：

```
# 真 xray（已下载）：/Users/xbtg-/deepseek-harness/.scratch/bin/xray  (Xray 26.3.27 linux/amd64)
bash scripts/guard.sh                 # I1/I2/I3 的机器判据
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xt-cli -- --socket <sock> connect <node>     # 真实事件驱动，无 sleep
# E2E 测试内部：本地 HTTP 源站 + 真 xray（vless 本地服务端，环回，无外网）
#   → 真实 SOCKS 请求 → 真实 StatsService 字节数 → 切节点 → 再请求 → 断开
```

E2E 必须是**真实进程**：本机 HTTP 源站（真 socket，真字节）、真 xray 二进制、
真 AF_UNIX 传输。允许「为了不依赖外网」把服务端也放在环回，**不允许**在
被测路径上放 mock。

## 9. 文件所有权（写之前先看这里；越界即返工）

| 所有者 | 独占写入路径 |
| --- | --- |
| lead | `Cargo.toml`、`crates/xt-contract/**`、`scripts/guard.sh`、`docs/architecture/ARCHITECTURE.md`、`docs/verification/**` |
| backend-1 | `crates/xt-bus/**`、`crates/xt-state/**`、`crates/xt-ipc/**` |
| backend-2 | `crates/xt-settings/**`、`crates/xt-subs/**`、`crates/xt-xrayconf/**`、`crates/xt-nodes/**`、`crates/xt-cli/**` |
| backend-3 | `crates/xt-datapath/**`、`crates/xt-stats/**`、`crates/xt-probe/**`、`crates/xt-daemon/**` |
| frontend | `apps/ui/**`（除 `src/components/**`、`src/pages/**`、`src/App.tsx`、`src/main.tsx`、`tests/**`） |
| ui | `apps/ui/src/components/**`、`apps/ui/src/pages/**`、`apps/ui/src/App.tsx`、`apps/ui/src/main.tsx` |
| design | `apps/ui/src/styles.css`、`docs/design/**` |
| ux | `apps/ui/tests/**`、`docs/ux/**` |
| product | `docs/product/**` |

共享只读：`crates/xt-contract/**`（要改 → 找 lead）。
需要别人改动的，发消息说明「哪个文件、什么形状、为什么」，不要自己动手。

## 10. 环境与协作硬规则

* 工具链（每个 shell 都要先 export，工作目录是 fresh shell）：
  ```
  export RUSTUP_HOME=/Users/xbtg-/deepseek-harness/.rustup
  export CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo
  export PATH="$CARGO_HOME/bin:$PATH"
  ```
* **并行编译不能共用 target 目录**（会互相等锁 = 浪费）：每人用自己的
  `export CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-targets/<你的名字>`。
* 不改 `xray-tun/`（v0.9.2 是已发布产品，冻结）；不执行任何 `git push`；
  不写密钥进任何文件。
* 证据分级：没跑过就不写「通过」；跑过就把命令和输出摘要贴在
  `docs/verification/` 或自己的报告里。**没有证据 ≠ 干净。**
* **改契约的人（含 lead）必须先本机编译验证再广播变更**。已经发生过一次：
  给 `ErrorCode` 加变体却漏了 `as_str()` 的 match 分支，全队 build 立刻失败。
  契约变更 = 一次 `cargo clippy -p xt-contract --all-targets -- -D warnings`。
* 注释写「为什么」，不写「是什么」。中文注释，代码标识符英文。
