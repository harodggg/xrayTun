# 交付一个 Mac 上能装能用的 XrayTun.app —— 分阶段计划

> 用户目标（2026-09-30 裁决）：**直接做完整 XrayTun.app**。
> 本文件是那份工作的路线图：每阶段的产出、判据、以及"这一步能在哪儿验证"。
>
> 贯穿全篇的一条纪律（沿用本仓库规矩）：**没有证据 ≠ 干净**。
> 本机是 Linux，macOS 的东西**编译不了也跑不了**，所以每个阶段都必须写清
> "证据来自哪里"：Linux CI / macOS CI（GitHub `macos-14`）/ 用户真机。

## 0. 现状（起点，别美化）

| 项 | 现状 |
| --- | --- |
| `xraytun-next` 的能力 | **只有 proxy(SOCKS) 模式**：连接 / 断开 / 切节点 / 真实流量统计 + 无头 CLI |
| macOS 相关代码 | **零**（无 utun、无 launchd、无 helper、无 SecCode、无 Tauri 壳） |
| 已发布的 Mac 包 | 只有**老代码库**的 `XrayTun_0.9.2_x86_64_arm64.dmg/.zip`（真机行为仍未验证） |
| 本机（Linux）能验证的 | 全部非 macOS 逻辑（已有 128 个测试 + 真 xray 端到端） |
| macOS CI 能验证的 | 能否构建、单测/端到端能否过、universal 是否真的 universal、CLI 冒烟 |
| **只有用户真机**能验证的 | TUN 是否真的接管流量、helper 安装与回滚、菜单栏交互、Gatekeeper/公证 |

## 1. 为什么不能"直接出 .app"

一个能用的 XrayTun.app 需要四块，缺一不可，且它们**都不在**当前重写里：

1. **TUN 数据面**：建 utun、配地址、装路由、改 DNS、快照回滚；
2. **特权 helper**：只有它能改系统网络；launchd 安装、对端 SecCode 校验、封闭指令集；
3. **控制面接入 TUN**：两阶段启动（`TunUp` → 等就绪 → `CommitRoutes`），状态机里
   `ConnectPhase::CommittingRoutes` 已经预留；
4. **壳与分发**：Tauri 2 壳 + 现有 React 界面 → `.app` → dmg → 签名 + 公证。

前两块是"把老实现按新架构重写"（老仓库自己写的 MIT 代码，可复用其设计而不是复制其结构），
第三块是接线，第四块是打包。**没有捷径能跳过它们**；跳过的结果就是一个装得上、但连不上流量的壳。

## 2. 阶段与判据

### S0 · macOS 产线打通（现在这一阶段）

* **产出**：`.github/workflows/xraytun-next-release.yml` 的 `macos-core` job；macOS universal
  命令行包（`xt-daemon` + `xt-cli`）。
* **判据**（全部在 `macos-14` 上真跑，命令与输出进 CI 日志）：
  1. 真 mac 上 `cargo clippy --all-targets -D warnings` 干净；
  2. 真 mac 上 `cargo test --workspace` 全绿（含真 xray 端到端：真字节、真 StatsService）；
  3. 两个架构分别构建后 `lipo -archs` 显示 `x86_64 arm64`；
  4. 起真 daemon 做 `hello` / `status` 冒烟；
  5. 产出 `xraytun-next-<ver>-macos-universal.tar.gz` + sha256。
* **不产出**：.app、TUN、helper。**不许**在这阶段对外说"Mac 版做好了"。

### S1 · macOS 平台层（`xt-macos-net`）

* **产出**：新 crate，只做四件事——建/拆 utun、配地址、加/删路由、设置/还原 DNS；
  以及"改动前落快照、崩溃后按快照还原"。
* **设计约束**（沿用老仓库已验证的结论）：外部命令一律绝对路径 + argv，永不经过 shell；
  权限边界封闭；所有对系统网络的改动都要能在快照里回放。
* **判据**：macOS CI 上编译 + 单元测试；能在 CI runner 上做**受控真实验**：
  建一个 utun、配一个**测试用前缀**的路由、然后拆掉（**绝不碰默认路由** ——
  那会把 runner 自己的网络打断）；DNS 只读不改。
* **风险**：CI runner 上做真实网络改动有打断 job 的风险 → 所以只做"不碰默认路由"的受控实验。

### S2 · 特权 helper（`xt-helperd`）

* **产出**：root 守护进程 + launchd plist + 安装/卸载（走一次用户授权）+ 对端校验 + 封闭指令集。
* **判据**：macOS CI 上能编译、协议层单测（帧、指令集封闭性、拒绝未知指令）；
  安装/回滚的**真实验必须由用户真机做**（CI runner 上装 launchd daemon 不可靠也不合适）。
* **诚实标注**：这一阶段完成时，能力是"能在真机上装"，而不是"已验证能装"。

### S3 · 控制面接 TUN（proxy ↔ tun 两条路径）

* **产出**：daemon 的 TUN 流程：`Connect{ mode: tun }` → 生成配置 → helper.TunUp →
  TakeTunFd → spawn xray（`XRAY_TUN_FD`）→ 等就绪 → helper.CommitRoutes → `Connected`；
  断开按相反顺序。失败一律落到 `Disconnected + last_error`（无回落）。
* **判据**：macOS CI 上跑**不依赖真实 TUN** 的状态机/协议测试；真 TUN 行为由用户真机验收。
* **能力宣告**：只有 S3 完成且真机验收通过，才允许宣告 `Capability::TunMode`；在那之前界面不出现 TUN 入口。

### S4 · 壳与界面（Tauri 2 + 现有 React）

* **产出**：Tauri 2 壳（菜单栏 + 主窗口）、把 `apps/ui` 的传输层换成 Tauri 通道、
  TUN 开关（按 `capabilities` 门控）、打包 `.app`、CI 出 dmg/zip。
* **判据**：macOS CI 上 `tauri build` 成功、产物是 universal（`lipo -archs`）、
  `codesign --verify --strict`（自签）通过；界面行为仍由 ux 的诚实性测试守。
* **参考**：老仓库 `release.yml` 已经跑通过 universal `.app` + dmg + 严格校验，步骤可复用
  （rustup 两个 target → lipo 核心 → tauri build → 校验 universal → codesign --verify --strict）。

### S5 · 签名与公证（需要用户提供凭据）

* **产出**：Developer ID 签名 + `notarytool` 公证 + staple；Gatekeeper 双击不再拦。
* **前置**：Apple Developer 账号（证书 + App-specific password 或 API Key）。
  **本机没有这些凭据**，必须由用户提供（放 `.secrets/`，永不入库）。
* **未提供时的降级**：发**未公证**的包，并在说明里给出 `xattr -dr com.apple.quarantine`
  的解除方式 + 明确写"未经公证"。**不许**把未公证的包说成"可安装无提示"。

### S6 · 真机验收（只有用户能做）

* **判据**（逐条打勾才算完成）：
  1. 换新 App → 连接/切节点/断开；
  2. TUN 模式真的接管流量（换 IP 前后对比）；
  3. 失败时如实报错、不自动重连；
  4. 断开/退出后系统网络恢复（DNS、路由不留残余）；
  5. helper 安装与卸载各一次，卸载后无残留。

## 3. 每个阶段"谁能验证什么"（防止把没验证说成可用）

| 阶段 | Linux CI | macOS CI | 用户真机 |
| --- | --- | --- | --- |
| S0 macOS 命令行核心 | — | ✅ 构建/测试/冒烟/universal | 可选（跑 CLI） |
| S1 平台层 | ❌ 编译都不过 | ✅ 编译 + 受控路由实验 | ✅ 真 TUN 行为 |
| S2 helper | ❌ | ✅ 编译 + 协议单测 | ✅ 安装/回滚/卸载 |
| S3 TUN 接线 | ❌ | ✅ 状态机/协议测试 | ✅ 端到端接管 |
| S4 壳与打包 | ❌ | ✅ tauri build + 产物校验 | ✅ 交互 |
| S5 签名公证 | ❌ | ✅ 有凭据时 | ✅ 双击安装 |
| S6 验收 | ❌ | ❌ | ✅（唯一） |

## 4. 现在缺什么（要用户给的）

1. **Apple Developer 凭据**（S5 才需要；现在不阻塞）：Developer ID Application 证书 +
   notarytool 凭据（App-specific password 或 API Key），放 `.secrets/`。
2. **一台真 Mac 做验收**（S6；每阶段的"真机结论"都从那里来）。
3. 一个决定：**要不要复用老仓库 macOS 层的设计**（不是照抄代码）。我的建议是要 ——
   那部分踩过的坑（fd 交付、两阶段启动、快照回滚的边界条件）比重新推一遍便宜得多，
   而且它是同一个作者、同一个许可证。
