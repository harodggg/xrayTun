# 08 · 失败模式总账

这份文档记录**真实发生过**的故障，按**根因**分类 —— 而不是按症状。
症状会重复出现（「网络断了」至少对应过五种不同的原因），根因才是可以防的。

每条都给出：症状 → 根因 → 现在的防线 → 钉住它的东西（测试 / 文档 / 代码位置）。

> **为什么要有这份文档**：这个项目里绝大多数 bug 都不是「算错了」，
> 而是「以为它对」。把这句话拆成可检查的类别，比记住十几个具体 bug 有用。

---

## A. 「以为它对了」——状态的来源与去向不一致

这一类占了一半以上。共同点：**代码里有一个值，而真正生效的是另一份**。

| 症状 | 根因 | 现在的防线 |
|---|---|---|
| 自更新报「校验文件里没有 X.zip」，而文件明明在里面 | `SHA256SUMS.txt` 由 `shasum -a 256 ./*` 生成，每行是 `./X.zip`；解析器拿整段路径比文件名 | 只比 **basename**（剥掉 `./`、目录、`*`） |
| 0.7.3 声称修好的自动重连没生效 | `was_connected = true` 只写了内存，没 `persist_settings`；而这个标记**全部用途**就是跨进程存活 | 写完立刻落盘，失败会 warn |
| 「关闭」没反应，再点却说「核心已经在运行」 | 快照里的 `running` 与 supervisor 的真实状态短暂不一致 | `start_core` 幂等；检查放在 **supervisor 锁内** |
| **连接按钮点了永远没反应**，核心其实早就不在了 | `is_running()` 用 `process.is_some()` —— 那只说明「**我手里有个 handle**」，核心自己退出后句柄还在 | 改成探真实存活（`has_exited()`）并回收陈旧句柄 |
| 发版永远失败在「打包（universal）」，Release 从未创建 | `universal-apple-darwin` 不是 rustc 的 target，是 **Tauri CLI 的伪 target** | 分别编两个真 target 再 `lipo`；预检 `rustc --print target-list` |
| CI 红了三个提交没人发现 | `npm --prefix apps/ui exec tsc` 不改 cwd，而 `tsc` 只在**当前目录**找 tsconfig | 加 `-p`；并把 CI 收敛成 `scripts/check.sh`，本地跑同一份 |
| Actions 缓存永远写不进也读不到 | `check.sh` 的 `npm_config_cache` 默认落在仓库**隔壁**，而 `actions/cache` 缓存的是工作区**里面** | 两个 workflow 的 `env` 里显式对齐 |

**共同教训**：写完一个值要问两句 ——
**它读到的是哪一份？**（内存 / 磁盘 / 快照）
**谁说了算？**（意图 / 观测 / 缓存）

**钉子**：
* `update::parse_sha256sum_for` —— `checksums_are_looked_up_by_file_name`
* `scripts/check.sh` 开头那段说明 + `docs/07` §6 发版流程

---

## B. 生命周期 —— 「拆掉」和「建起来」之间

这一类的共同点：**中间态没有被当成一等公民**。

| 症状 | 根因 | 现在的防线 |
|---|---|---|
| 崩溃后启动，网络配置没被回滚 | `Restore` 用了 `restore_stale()`（判据是「崩在半路」），而不是 `force_cleanup()` | `Restore` → `tear_down_live_session()` + `force_cleanup()` |
| 切换节点时「连环爆炸」，切到坏节点后彻底断网 | 先 `stop_core` 再 `start_core`，**后者失败就直接返回** —— 旧隧道已拆、新隧道没建 | 失败退回上一个**验证过**的节点并重连 |
| 用着用着网停了，日志末尾是 `Logger closing` | 自更新要先退出 app（核心优雅关闭）、替换、重启 —— 而重启后不连回来 | `was_connected` + `auto_reconnect` → 启动时 `reconnect_if_needed` |
| 核心已经死了，界面还显示「已连接」 | 日志转发任务 `while let Some(..) = rx.recv()` 结束就什么都不做 | 循环结束时把运行时标记为已停止并报错 |
| 点了「关闭」，几秒后它自己又连上了 | 看门狗的探测是异步的，结果回来时用户的意图已经变了 | 重建前先看 `was_connected`，false 就放弃 |
| ⌘Q 之后留下孤儿核心占着 10808/10809 | `app.exit()` 不跑析构，`kill_on_drop` 无效 | `sync_cleanup` + `RunEvent::ExitRequested` |
| 换网 / 熄屏唤醒后隧道死了，只能手动重连 | 路由/网卡绑定/长连接全指向旧出口，且**没有任何自愈** | 看门狗：10 秒真实探测，连续 2 次失败自动重建；重建失败**退回直连** |
| 核心崩了之后**彻底卡死**：按钮无效、也没人恢复 | 两个「看观测值」的判据一起坏：看门狗看 `runtime.running`（核心一死就被置 false → 看门狗退出），按钮看 `process.is_some()`（句柄还在 → 空转） | 看门狗改看**意图 + 代次**（`was_connected` + pid）；`is_running` 改探真实存活 |
| 起 TUN 时磁盘满，留下半套网络配置 | ——（**顺序是对的**：`snap.save()?` 在改路由/DNS **之前**，失败即中止） | 见 §E |

**共同教训**：任何「先拆后建」都必须回答**中间失败怎么办**。
答案只有两个：**回退**，或者**退回一个可用的降级状态**（宁可直连不可断网）。

**钉子**：
* `dns::restore` —— DHCP 情形走 `Empty`（`bind_to_interface_rejects_unknown_nic` 同级）
* `commands::network_moved` —— `egress_change_is_detected_by_interface_or_gateway`
* `commands::should_auto_reconnect` / `tunnel_is_dead` / `should_rebuild_tunnel` / `watchdog_should_watch`

---

## C. 测量 —— 量到的不是想量的

| 症状 | 根因 | 现在的防线 |
|---|---|---|
| 服务器延迟显示 **0ms**（远端不可能 0ms） | 隧道接管默认路由后，任何新 socket 的握手都被**本地协议栈在 TUN 那侧应答** | RTT 用 `TcpSocket` 在 `connect()` **之前**设 `IP_BOUND_IF` 绑物理网卡 |
| DNS 探测并发时测到的是「核心排队」而非解析器延迟 | 同上（查询进了隧道） | DNS 探测同样绑网卡 |
| 国外解析器名次随并发变化 | 五台共享同一个节点，并发等于测**节点的排队** | 国外组**串行**（`FOREIGN_CONCURRENCY = 1`） |
| 国内/国外混在一张表里，像同一把尺子量的 | 两组走的是**两条不同路径** | 分开测量、分开显示；组内单独多数派投票 |
| 正确结果被丢弃、查询退化到 450ms | `expectIPs` 把「不符合预期」当成「失败」 | 去掉 `expectIPs`；谁被 `domains` 选中就用谁的答案 |
| 一个域名在五台解析器之间轮流超时 | 每个列表只写第一个，**回退链上再没有同层备选** | 同一条 `domains` 下的候选**全部**写进配置 |

**共同教训**：测之前先问 **「这个数字是被什么路径量出来的」**。
隧道内测 RTT、不绑网卡测 DNS、并发测共享出口 —— 三种都是「量到了别的东西」。

**钉子**：
* `net::bind_to_interface_fd` —— `bind_to_interface_rejects_unknown_nic`
* `probe::foreign_group_is_probed_serially`、`checksums...`（同族）
* `dns_probe::foreign_group_is_not_probed_without_node`
* `config::split_dns_keeps_every_candidate_as_same_tier_fallback`

---

## D. 判读 —— 把不同的东西当成同一个

| 症状 | 根因 | 现在的防线 |
|---|---|---|
| 「错误」页签里全是内核的**正常**信息，真错误被淹 | `classify_log` 纯按关键字判级，无视内核写在行首的 `[Level]` | 先信 `[Error]/[Warning]/[Info]/[Debug]`，没标记才退回关键字 |
| 「限流了」和「仓库不存在」长得一模一样 | `curl -f` 把 403/404 压成同一句「退出码 22」 | 把 HTTP 状态码带出来分别翻译 |
| 绿色的「55 ms」紧挨着红色的「不可用」 | 延迟徽章按 RTT 上色，而 RTT **只表示距离，不代表能用** | 不可用时降级为中性色，tooltip 说清含义 |
| 把四种 DNS 故障当成同一种 | 尾巴不同、含义完全不同 | 见下表 |

**DNS 故障的四种尾巴**（细节见 [04](04-routing-and-dns.md) §8.4）：

| 尾巴 | 含义 | 处置 |
|---|---|---|
| `context deadline exceeded` | 解析器在超时内没答 | 等，或换解析器 |
| `io: read/write on closed pipe` | 连接被从脚下抽走（换网/拆隧道） | 断开重连 |
| `context canceled` | 请求方自己放弃了 | 一般不用管 |
| `unexpected EOF` | 连接建起来了，应答读到一半被截断 | 偶发不管；频繁说明链路在重置长连接 |

**共同教训**：**分类器的输入必须包含「谁说的」**。
内核说了 `[Info]` 就不要再按消息里的词去升级它；HTTP 说了 403 就不要当成 404。

---

## E. 环境 —— 代码之外的

| 症状 | 根因 | 处置 |
|---|---|---|
| 「断网」时有时无，代码路径查不出问题 | **磁盘 100% 满**（构建产物 `/.cargo-target` 占了 32G） | 我清掉了；`brew cleanup` 还能再放 6.9G |
| `brew` 什么都装不了 | macOS 26 只有 Xcode、没有 CLT，Homebrew 7 拒绝工作 | `xcode-select --install` |
| 自更新必须填 token | 客户端仓库是**私有**的，匿名一律 404 | 已改公开，token 设置项已删 |
| 本地只能打出单一架构的包 | 本机 rust 是 Homebrew 的 x86_64，只有宿主架构的 std | 通用包只能在 CI 出（`docs/07` §6） |

**共同教训**：**先量环境，再读代码。** 这一条我犯过：花了几轮读 DNS 代码路径，
而真正的原因是磁盘满了。

---

## 怎么用这份文档

* 遇到**新**故障：先在上面的表里找**同类根因**，而不是找同一个症状。
* 加一条防线时：顺手在「钉子」那一列补上测试或文档位置。
* 改完之后问自己：**这次的错误属于哪一类？** 如果四类都不是，说明这一页该扩了。
