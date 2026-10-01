# apps/desktop —— xraytun-next 桌面壳（Tauri 2）

> **未验证声明**：本机（Linux）没有 macOS、没有 Tauri 工具链，本轮没有跑过
> `cargo check`、没有跑过 `tauri build`、没有做过任何一次真实的
> `invoke` / `listen` 往返。这个目录是「按契约形状写完的壳」，**不构成可用声明**。
> 真机验收（S6）见 `docs/design/MACOS-APP-PLAN.md`。

## 它在整个系统里的位置

```
XrayTun Next.app（这个 crate，普通用户）
  ├── WebView：承载 apps/ui（只渲染 + 采集输入）
  ├── tray.rs          菜单栏：显示窗口 / 退出
  ├── daemon_launch.rs 拉起 xt-daemon（占位，真机 S6 对齐）
  └── bridge.rs        UI 请求 → daemon；daemon 事件 → UI
xt-daemon（独立二进制，普通用户进程；不是本 crate 的一部分）
  └── 全部业务：settings / subs / nodes / lifecycle / datapath / stats / probe
```

壳只认识 `xt-contract`（帧类型）与 `xt-ipc`（AF_UNIX 客户端）。业务规则一律不进壳。

## 文件职责

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 一行入口，调 `xraytun_desktop_lib::run()` |
| `src/lib.rs` | 模块文档（薄壳边界 + 桥线上形状 + 未验证声明）、`run()`、建主窗口、窗口关闭即隐藏 |
| `src/bridge.rs` | 桥命令 `xt_daemon_request`、懒连接、事件转发任务、错误形状 |
| `src/daemon_launch.rs` | socket 路径解析、daemon 二进制解析、单次探测、绝对路径 + argv spawn、日志重定向 |
| `src/tray.rs` | 菜单栏图标与菜单（显示窗口 / 退出） |
| `tauri.conf.json` | 窗口（`create:false`，由 Rust 建以便注入 socket）、`withGlobalTauri`、CSP、bundle |
| `capabilities/default.json` | 主窗口最小权限集 |
| `icons/` | 应用与托盘图标（从老仓库 `apps/desktop/icons/` 复制） |
| `binaries/README.md` | `xt-daemon` 打包接线说明（**当前未接线**） |
| `build.rs` | `tauri_build::build()` |

## 桥：逐字对齐 `apps/ui/src/transport/tauri.ts`

UI 侧（已存在，不改）：

```ts
invoke('xt_daemon_request', { socketPath, id, request })   // request: {op: ...}
listen('xt_daemon_event', event => ...)                    // payload: { seq, event }
```

Rust 侧实现：

* 命令：`#[tauri::command] async fn xt_daemon_request(app, socket_path, id, request)`。
  Tauri 2 把 Rust 的 `socket_path` 映射成 JS 的 `socketPath`（camelCase 是默认行为）。
* **返回值是 `Outcome`，不是裸 `Response`**：
  `{"status":"ok","response":{...}}` 或 `{"status":"error","error":{code,message,detail?}}`。
  UI 明确按 `outcome.status` 分流；任何失败都必须**解析成功**地返回 `Outcome::Error`，
  而不是让 `invoke` reject。
  > 任务书正文说「返回值是 daemon 的一帧 Response」，与 UI 代码不一致。
  > 以代码为准 —— UI 自己要求「读它、精确对齐」。这一条已在交付报告里标注。
* 请求反序列化：`serde_json::from_value::<xt_contract::protocol::Request>(request)`；
  失败返回 `invalid_request`（附 `ui_request_id`）。
* 代理：`xt_ipc::Client::request(Request) -> Result<Response, ErrorBody>`。
* 事件：后台任务 `client.events()` 收到 `Event` 后 `app.emit("xt_daemon_event", {seq, event})`。

### `seq` 的诚实说明（重要）

UI 按 `seq` 从 1 开始做连续性检查，但 `xt_ipc::Client::events()` 交出的是
`Event`（真实序号 `Frame::Event.seq` 在 xt-ipc 内部被消费，用于它自己的跳号检测，
并在跳号时补一条本地 `Notice` 事件），**没有对外暴露**。所以壳维护一个从 1 开始的
单调计数器。它描述的是「桥 → WebView 这一跳」的帧序号；daemon → 桥 这一跳的丢帧由
xt-ipc 检测并以 Notice 事件如实呈现。

已知局限：WebView 整页重载后 UI 的 `expectedSeq` 归 1，而壳的计数器不归零，UI 会判
「seq 跳号」进入致命错误。彻底修法是让 `xt-ipc::Client::events()` 暴露
`(EventSeq, Event)` —— 属于 xt-ipc 的接口变更，不在本任务范围。

## daemon 拉起（占位）

* socket 默认 `/tmp/xraytun-daemon.sock`（`XT_SOCKET` 可覆盖）。壳**通过
  `initialization_script` 注入** `window.__XT_SOCKET__`，所以两边的路径一定是同一个值。
  UI 自己的兜底 `/run/xraytun/daemon.sock` 不会被用到（macOS 上也没有 `/run`）。
* 二进制顺序：`XT_DAEMON_BIN` → `<exe>/../Resources/xt-daemon` →
  `<exe>/xt-daemon`（开发形态）。
* 先做**一次**非阻塞连接探测：已有 daemon 就复用，不再拉第二个（daemon 的
  `Server::bind` 会删掉别人的 socket 文件，盲目 spawn 会踢掉正在服务的实例）。
* `tokio::process::Command`，绝对路径 + argv，**永不经过 shell**；stdin=null，
  stdout/stderr 追加到 `~/Library/Logs/XrayTun/xt-daemon.log`（`XT_DAEMON_LOG` 可覆盖）。
* spawn 失败**不阻断**启动：界面会在第一条请求上如实报「连不上」。
* 生命周期是占位：壳不持有 `Child`、不 `kill_on_drop`，退出后 daemon 继续运行。

## 为什么 `apps/desktop` 不挂进根工作空间

根工作空间的门禁是 Linux 上 `cargo test --workspace` / `cargo clippy --all-targets`；
Tauri 依赖系统 GUI 库，在 Linux CI 上会直接编译失败。所以本 crate 用空的
`[workspace]` 表自成工作空间，根 `Cargo.toml` 里 `exclude = ["apps/desktop"]` 明示意图。
代价：本目录有自己的 `Cargo.lock`。

## 构建 / 验证（都要在 macOS 上）

```bash
cd apps/ui && npm install && npm run build      # 先出 dist/（frontendDist 指向它）
cd ../../apps/desktop
cargo check                                      # 只查 Rust 侧
cargo clippy --all-targets -- -D warnings
npm run tauri build   # 需要 tauri-cli；产物是 .app + .dmg
```

开发态（一个终端起 Vite，另一个起壳）：

```bash
cd apps/ui && npm run dev
cd apps/desktop && XT_DAEMON_BIN=/abs/path/xt-daemon npm run tauri dev
```

## 待真机对齐的清单

1. `bundle.resources` 加 `xt-daemon` 映射（见 `binaries/README.md`）。
2. daemon 的退出生命周期：退出壳时是否要停掉 daemon（当前不停）。
3. 单实例竞态（探测与 spawn 之间的 TOCTOU）。
4. `window.__XT_SOCKET__` 注入的时序在真实 WKWebView 上复核。
5. `tauri.conf.json` 的 CSP 在真实界面下复核（内联样式/资源加载）。
6. 事件 `seq` 计数器与「页面重载」的交互（见上）。
7. 代码签名/公证所需的 entitlements：**当前故意为空**。spawn 一个独立可执行文件
   不需要 `disable-library-validation`（那只约束 `dlopen` 进来的库），所以不加
   —— 最小权限面（不变量 I5）。真机若发现 WKWebView 或子进程需要额外 entitlement，
   再按证据加，不预先放宽。
