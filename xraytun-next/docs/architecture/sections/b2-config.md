# B2 · 配置与目录域（xt-settings / xt-subs / xt-xrayconf / xt-nodes / xt-cli）

## 为什么设置用单文件 JSON，而不是数据库
设置全部内容就三个字段（`socks_listen`、`selected_node`、`log_level`）。数据库只能换来一个要迁移的 schema、一个打不开的文件、一个排查时的黑盒。两个代价已解决：
* **原子替换**：同目录写 `*.tmp` 再 `rename`（同文件系统内原子），核心/daemon 读到的要么是旧内容、要么是新内容，不会是半截 JSON。
* **坏文件如实报错**：只有「文件不存在」才是首次运行用默认值；文件存在但读不动（权限/非 UTF-8/JSON 非法/字段类型不对）一律返回具体 `ErrorBody`。静默重置会吃掉用户的设置，还会让界面显示用户从没选过的状态 —— 正是 I3 禁止的假话。断言：`corrupt_file_is_reported_and_never_reset_to_defaults`。
选择记忆与其它设置同文件：它们同生共死，拆开只会多一种不一致状态。

## 为什么 NodeId 要稳定可读，而不是哈希
`NodeId = base64url(protocol|host|port|name)`（无填充）。哈希（sha256 前 16 字节）同样稳定，但排查时只能反查数据库才知道 `9f2a…` 是哪台服务器；base64 解码后是 `vless|a.example.com|443|东京 01`，看一眼就够。**可排查性 > 不可读性**。三条约束由构造保证：
* **稳定**：输入只取原始节点字段，不含解析顺序/订阅来源/时间；同一台服务器在任何刷新、任何格式下都是同一个 id（断言 IPv6 在分享链接与 Clash YAML 下同 id）。
* **不含凭据**：uuid/密码/订阅 token 不参与派生，id 可安全进日志与 tag。
* **主机大小写归一**：`A.Example.com` 与 `a.example.com` 同 id。
稳定 id 是「切节点 = 换 tag」的前提：tag 是 `node-<id>`，统计、路由规则、选择记忆三者因此指向同一个对象。刷新后旧 id 消失时返回 `NotFound`，不自动改选。

## 为什么配置生成是纯函数
`xt_xrayconf::generate(&ConfigInputs) -> Result<String, ErrorBody>` 不读文件、不碰进程、不查时钟：同样输入必然同样输出。
1. **可断言**：socks → `node-<id>` 路由、outbound tag 与 NodeId 一一对应，能逐条断言且不依赖进程。
2. **可验证**：产物直接交给真核心 `xray run -test -c`，证明「Xray 认这个形状」，而不只是「我们以为自己写对了」。
3. **切节点无中间态**：配置是纯函数产物，切节点就重新生成整份配置再重启核心；不做热更新，也就没有「配置里说的」与「核心正在跑的」不一致的窗口。`ConfigInputs` 里没有任何运行时状态，从签名上堵死了状态依赖。
`api_listen` 由 daemon 按「socks 端口 + 1」计算并传入，生成器校验两者一致且都监听回环：用户少改一个端口就会得到一个连不上的核心，而 API 暴露到局域网等于把无鉴权的统计接口交出去。
