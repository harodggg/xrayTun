# XrayTun

基于 **Xray-core 原生 TUN 入站** 的 macOS 代理客户端。Tauri 2 + Rust 外壳，React 界面，
特权操作全部收敛到一个只做网络配置的 root 守护进程。

```
┌──────────────────────────────────────────────────────────────┐
│  XrayTun.app                          （普通用户身份运行）      │
│                                                              │
│   React UI ──IPC──▶ Tauri commands ──▶ xt-core               │
│                                        ├─ 订阅解析             │
│                                        ├─ 节点模型             │
│                                        ├─ Xray 配置生成        │
│                                        └─ 核心进程 / 延迟探针   │
│                                                              │
│                         xt-proto::transport （Unix socket）   │
└────────────────────────────────┬─────────────────────────────┘
                                 │ /var/run/com.xraytun.helper.sock
                                 │ 0660 root:admin + 代码签名校验
┌────────────────────────────────▼─────────────────────────────┐
│  com.xraytun.helper                    （root LaunchDaemon）  │
│                                                              │
│   xt-tun                                                     │
│    ├─ 建 utun（PF_SYSTEM ioctl + connect）                    │
│    ├─ 配地址 / 装路由（0.0.0.0/1 + 128.0.0.0/1）               │
│    ├─ 备份 / 修改 / 还原系统 DNS                               │
│    └─ 会话快照（落盘，崩溃可回滚）                              │
└──────────────────────────────────────────────────────────────┘
```

## 它解决的核心问题

代理工具最难的不是「连上服务器」，而是**在网络配置被改动之后，保证任何时刻都能完整还原**。
本项目把这一点当成第一约束：

* 建 utun 之前先落一笔「我要开始动手了」的快照；
* 每装一条路由、每改一个服务的 DNS，都增量落盘一次；
* 回滚时**先还原 DNS 再删路由**，且倒序删除、单项失败不中断；
* 即使 helper 被 `kill -9`，下次启动的第一件事就是读快照回滚。

## 三条与常见做法不同的设计决定

| 决定 | 常见做法 | 本项目 | 理由 |
|---|---|---|---|
| TUN 数据面 | 外挂 tun2socks / hev-socks5-tunnel | **Xray 原生 `tun` 入站**（内置 gVisor） | 少一个进程、少一份配置、UDP/QUIC 支持更完整。要求核心 `>= 26.1.31` |
| 数据面权限 | 整个核心以 root 运行 | **核心以普通用户运行**，utun fd 由 helper 通过 `SCM_RIGHTS` 交付 | 只有「建卡 + 改路由」这一小段逻辑在 root 下 |
| 配置热更新 | gRPC `HandlerService` 动态改 inbound | **重启核心** | 桌面端切换频率是分钟级，冷启动 100~300ms 无感；换来零 `protoc` 依赖 |

## 目录

```
crates/
  xt-proto/    helper 线协议 + SEQPACKET/SCM_RIGHTS 传输层 + 共享领域类型
  xt-core/     订阅解析、Xray 配置生成、核心进程、延迟探针   （与平台无关）
  xt-tun/      utun / 路由 / DNS / 快照回滚 / 输入校验        （macOS）
  xt-helper/   root 守护进程（socket 服务端 + 对端授权）
apps/
  desktop/     Tauri 2 外壳：命令层、状态机、托盘
  ui/          React + TypeScript 界面
docs/          设计文档（先读 docs/01-architecture.md）
```

## 快速开始（开发）

```bash
# 1) 准备 Xray 核心（>= 26.1.31，macOS 上原生 TUN 的完整可用版本）
./scripts/fetch-xray.sh

# 2) 编译
cargo build                      # 或 cargo build --release
(cd apps/ui && npm install && npm run build)

# 3) 跑测试（227 个单元测试，不需要 root）
cargo test

# 4) 只测 helper 的协议层（不需要 root，TUN 功能不可用）
sudo cargo run -p xt-helper -- run --socket /tmp/xraytun-helper.sock
cargo run -p xt-helper -- status --socket /tmp/xraytun-helper.sock

# 5) 起完整应用
npm --prefix apps/ui run dev     # 另开一个终端
cargo run -p xraytun-desktop
```

## 安装（普通用户）

从 [Releases](https://github.com/harodggg/xrayTun/releases) 下载 `XrayTun_<版本>_<架构>.dmg`
（或同名 `.zip`），把 `XrayTun.app` 拖进「应用程序」。

**不需要自己安装 Xray 核心**：核心与规则数据随包附带，开箱即用 ——
`Contents/Resources/{xray, geoip.dat, geosite.dat}`。不用另外下载 Xray，
也不用配 `XRAY_LOCATION_ASSET`。

首次打开会被 Gatekeeper 拦下（这个包是 **ad-hoc 签名、未公证**的）。
**放行方式按 macOS 版本分**：

* **macOS 15 及以上**：右键「打开」**已被 Apple 移除**（2024-08-06 公告）。
  先双击一次让它被拦下，再打开「系统设置 → 隐私与安全性」，在「安全性」区域找到
  关于 XrayTun 的提示，点「仍要打开」，然后确认。
* **macOS 14 及更早**：在「应用程序」里右键（或 Control-点击）`XrayTun.app` →「打开」，
  在弹窗里再点一次「打开」。
* **终端（两个版本都适用）**：

  ```bash
  # 逐文件清掉这个 App 的 quarantine
  find /Applications/XrayTun.app -exec xattr -d com.apple.quarantine {} + 2>/dev/null
  ```

  ⚠️ 实测（macOS 26.6.2 / build 25G83）：**`xattr` 有两个不同实现，行为不一样** ——
  Apple 的 `/usr/bin/xattr`（Mach-O）与 PATH 上可能先命中的 Python `xattr` 包（脚本）：

  ```text
  $ which -a xattr
  /Library/Frameworks/Python.framework/Versions/3.12/bin/xattr   ← Python 的 xattr 包
  /usr/local/bin/xattr                                            ← 同一个包
  /usr/bin/xattr                                                  ← Apple 的
  $ xattr -dr com.apple.quarantine /tmp/xtest          → exit 64「option -r not recognized」
  $ /usr/bin/xattr -dr com.apple.quarantine /tmp/xtest → exit 0，标记真的被删掉
  ```

  `/usr/bin/xattr` 的 usage 是 `xattr [-l] [-r] [-s] [-v] [-x] file …`（**支持 `-r`**）。
  也就是说，**`-r` 的支持随实现与版本而异**：PATH 上先命中的那个没有；本机 Apple 的有；
  更早的 macOS 里 Apple 的也没有。所以：

  * **一律写绝对路径 `/usr/bin/xattr`** —— 否则可能命中 Python 的包；
  * **不要依赖递归开关 `-r`** —— 需要递归就用上面那条 `find … -exec … +`；
  * 命令**不要 `2>/dev/null`** —— 它会把「本来就没有该属性」和**真正的失败**一起吞掉，
    失败要留痕（上面示例里保留它只是为了少刷屏，自己执行时建议去掉）。
  * `xattr -d com.apple.quarantine /Applications/XrayTun.app`（只给 bundle 根路径）
    **只会清掉根上那一个**：实测 13 个带 quarantine 的文件里还剩 **12 个**，
    所以要像上面那样逐文件清。`xattr -c` 同理（对目录不递归）。

TUN 模式还需要装一次特权 helper：侧栏「设置」→「特权助手（helper）」→
「安装 helper」（会弹一次管理员密码）。**系统代理模式不需要它。**

## 打包 macOS 发行版

```bash
./scripts/package-macos.sh
```

产出 `XrayTun.app`、`XrayTun_<版本>_<架构>.dmg` 与同名 `.zip`
（都在 `.cargo-target/release/bundle/` 下）。

脚本除了调 `tauri build`，还补了两件 Tauri 不会做的事：

* **把 helper 放进 `Contents/MacOS/`** —— `helper_binary_path` 只在自己
  可执行文件的同级目录找它，而 `bundle.resources` 放不进 `Contents/MacOS/`。
  少这一步的表现是点「安装 helper」时报「找不到 helper 二进制」。
* **校验 geoip.dat / geosite.dat 与核心同级** —— `XRAY_LOCATION_ASSET`
  取的是核心二进制的父目录。放错位置不会报错，只会让 `geoip:cn` /
  `geosite:cn` 规则**静默不命中**，「绕过大陆」预设看起来完全没生效。

包是 **ad-hoc 签名**、未公证的：用户首次打开会被 Gatekeeper 拦，
放行方式见上面的 [安装（普通用户）](#安装普通用户)。

正式分发需要付费开发者账号，用 Developer ID 重签并 `notarytool` 公证。

## CI 与发版

| 工作流 | 触发 | 做什么 |
|---|---|---|
| `ci.yml` | push 到 main / PR | clippy（warning 视为错误）、227 个单元测试、前后端构建 |
| `release.yml` | 打 tag `v*` | 出 **universal** 包并创建 GitHub Release |

发版只需要推一个 tag：

```bash
git tag v0.2.0 && git push origin v0.2.0
```

为什么发版必须交给 CI，而不是本地跑上面那个脚本：本机是 Homebrew 装的
rust，没有 rustup、加不了 target，所以**本地只能出主机架构的包**（实测是
x86_64 的 App + arm64 的核心）；而且核心上游按架构分发，要出通用包得分别
下载再 lipo 合成。CI 上这两件事都是确定的。发布用的 `contents: write`
令牌也由 CI 提供，本地没有。

## 更新记录

见 [CHANGELOG.md](CHANGELOG.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [01-architecture.md](docs/01-architecture.md) | 进程模型、模块边界、数据流、状态机 |
| [02-tun-and-privileges.md](docs/02-tun-and-privileges.md) | utun 的创建、路由策略、DNS 接管、权限模型取舍 |
| [03-xray-integration.md](docs/03-xray-integration.md) | 原生 TUN 入站、Fake-IP、配置生成、版本下限、gRPC 升级路径 |
| [04-routing-and-dns.md](docs/04-routing-and-dns.md) | 分流规则模型、DNS 四种策略、防环手段 |
| [05-ui-spec.md](docs/05-ui-spec.md) | 界面结构、各页面职责、状态呈现约定 |
| [06-helper-protocol.md](docs/06-helper-protocol.md) | 线协议、传输层选型、对端授权、必须补的测试 |
| [07-roadmap-and-risks.md](docs/07-roadmap-and-risks.md) | 已知风险、未完成项、许可证、下一步 |

## 许可证

本项目**源码**采用 **MIT**，正文见 [`LICENSE`](LICENSE)。

发布产物（dmg / zip）里**随包分发**的第三方二进制**各有其许可、不受 MIT 覆盖**：

* **Xray-core** —— MPL-2.0（以独立进程调用，不构成衍生作品）
* `geoip.dat` / `geosite.dat` —— 随 Xray-core 发布
* 界面依赖见 `apps/ui/package.json`

完整说明见 [`THIRD-PARTY.md`](THIRD-PARTY.md)；`docs/07-roadmap-and-risks.md` 里有第三方清单与注意事项。

> **为什么第三方声明不在 `LICENSE` 里**：GitHub 的许可证识别只看标准模板，
> 在 `LICENSE` 里附加段落会让整个仓库被识别成 `Other` 而不是 MIT。
> 拆文件之后 `LICENSE` 是纯 MIT 正文，第三方声明也完整保留。
