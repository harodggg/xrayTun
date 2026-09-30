# b1 · 传输与状态基石（xt-ipc / xt-bus / xt-state）

## 为什么 IPC 用长度前缀 JSON，而不是 gRPC

对端只有本机 UI 与本机 CLI：没有跨语言 SDK、没有跨机部署、没有 schema 演进需求。
gRPC 会把「谁能说这份协议」变成构建期问题（proto 编译器、代码生成、兼容规则）。
长度前缀 + JSON 让帧能被 `nc`、日志、用户直接看懂 —— 可排查性优先于编解码性能。
4 字节大端长度、上限 1 MiB、超限即 `invalid_request`、**不分片**：分片会把「一帧」
变成需要状态机的字节流，出错时谁也说不清丢的是哪一半。

## 为什么把状态机做成纯函数

「界面显示已连接」必须能被机器证明。纯 `apply(&State, Signal, now_ms)` 让整张转换矩阵
在没有进程、没有 socket、没有时钟的情况下被穷举测试：合法边逐条断言结果状态与动作，
非法边一律 `conflict`。`State -> ConnectionView` 只有一条方向，视图不能由别处拼出来，
所以「进程早退了但界面还写着已连接」这种假话没有入口。
`connected_since_ms` 只来自传入的真实时钟（CoreReady 的 `at_ms` 或路由提交时的 `now_ms`）；失败只有一条出口：`Disconnected + last_error`。

## 为什么意图要显式进入状态机（`begin_connect` / `begin_switch`）

信号描述的是「外面真实发生了什么」（配置就绪 / 核心启动 / 可连 / 提交路由 / 退出），
它不携带 `node_id`/`mode`。若让裸 `ConfigReady` 触发 `Disconnected -> Connecting`，
状态机只能猜节点，等于凭空多出一条没有意图的路径。因此意图单独绑定一次：
`begin_connect` 只从 `Disconnected` 接受，`begin_switch` 只从 `Connected` 接受。
切节点的动作是 `[StopCore, PrepareConfig, ...]`，路径里**没有**「失败回到旧节点」的边：
切过去失败就停在 `Disconnected + last_error`（无回落）。

## 为什么事件走总线而不是回调

回调把「谁在等什么」写死在发布点，且发布线程要等每个消费者。daemon 里同一个状态
要同时送达 IPC 连接、CLI 与本地订阅者，回调签名最终会膨胀成万能参数包，一个慢消费者
就能拖住控制面。broadcast 的语义是「慢的人丢自己的帧，发布者永不阻塞」；但丢帧不静默：
IPC 客户端按 `Event.seq` 检查连续性，跳号会生成一条**真实的** Notice（不伪造缺失帧）。
`watch` 只保留最新连接视图：界面要的是「现在是什么」，不是历史每一帧。

## 一条实现级硬要求：`Connection::next_frame` 必须取消安全

daemon 用单任务 `select!` 同时收请求与推事件；事件先就绪时 `next_frame` 的 future 被 drop。
所以读侧只用取消安全的 `read`，已读字节立刻进入连接自己的缓冲区，半个帧永远不会被
当成长度头（回归测试：发半个帧 → 取消一次 → 补齐 → 仍解出完整一帧）。
