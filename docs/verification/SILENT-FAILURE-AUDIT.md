# 静默失败 / 吞错审计（task-119 **第一阶段：只审计、只报告**）

> **谁做的**：tester（独立审计，**一行代码都没改**）。
> **为什么**：同一个病灶一天出现四次（`xattr -dr … 2>/dev/null || true`、`store.rs` 的 `.ok()` 丢行、
> `check.sh` 被并发构建弄红、helper 版本假报不匹配）⇒ 「**吞掉错误**」与「**陈述比事实强**」是同一个病的两种表现。
> 本报告要把它变成**可勾选的清单**，而不是等它下次咬人。

## 0. 方法与范围（先说清「我扫了什么、没扫什么」）

```bash
# 范围：apps/desktop/src/**、crates/**、scripts/**、apps/ui/src/**、infra/incident-collector/src/**
# 形状（正则）：`let _ =` / `.ok()` / `unwrap_or_default()` / `Err(_) => {}` / `2>/dev/null` / `if let Ok` / `catch {`
$ for p in 'let _ =' '\.ok()' 'unwrap_or_default()' 'Err\(_\) *=> *\{\}' '2>/dev/null' 'if let Ok' 'catch *\{'; do …count…; done
  let _ =            159
  .ok()              178
  unwrap_or_default() 71
  Err(_) => {}         1
  2>/dev/null        118
  if let Ok           25
  catch {              4
```

**排除规则**（否则清单会被噪音淹没，而噪音会让真条目被忽略）：

1. **`#[cfg(test)]` 测试代码里的吞错不算**（测试里 `.ok()` 是断言辅助）——本报告只收**生产路径**；
   ⚠️ **上面的原始计数是「含测试」的**（例如 `update.rs` 生产区里 `let _ =` 只有 2 处，而全文件有 17 处），
   所以「159 / 178 / 71 …」是**候选规模**，**不是缺陷数**；
2. `Option` 上的 `.ok()?` / `.ok()` 用于**向上传播**（`route.rs:46`、`:90`、`controller.rs:50`）**不算吞**——
   它把「读不到」交给调用方，调用方能表达；
3. `apps/ui/**` 里的 `.catch {}` 属于「界面兜底」，本报告只在**它导致错误结论**时收录（本轮 0 条）；
4. `infra/**`（ops 的端点）不在本卡范围。

**下面逐条给的是「生产路径里会造成错误结论」的**；纯噪音型（`rm -f … 2>/dev/null || true` 之类）
按**类别**归到 §4 并**写明保留理由**——卡面要求「C 级也要给理由」，不是要求把 500 处逐行抄一遍。

## 1. A 级（**必须修**：吞掉的失败会让用户/我们得出错误结论）

### A-1 ⚠️「修复网络」会把**回滚失败**的**错误通道销毁**，helper 仍然回「已回滚会话 …」

```
crates/xt-tun/src/macos/controller.rs:353-360
    pub fn force_cleanup() -> Result<Option<SessionSnapshot>> {
        let Some(snap) = SessionSnapshot::load()? else { return Ok(None); };
        tracing::warn!(session = %snap.session_id, "强制清理遗留会话");
        let _ = rollback(&snap);              // ← 回滚失败在这里被丢掉
        Ok(Some(snap))                        // ← 永远 Ok
    }
crates/xt-helper/src/server.rs:318-329      （Request::Restore 分支）
    let cleaned = controller::force_cleanup();
    let resp = match cleaned {
        Ok(Some(s)) => Response::Ok { message: "已回滚会话 {id}" },   // ← 回滚失败也走这里
        …
        Err(e) => Response::Error(tun_err(e)),                        // ← 这条**不可达**
    };
apps/desktop/src/commands/helper.rs:47-63   （App 侧）
    Ok(_) => state.log("app","info","已请求 helper 回滚遗留会话"),
    Err(e) => { state.log("app","error", format!("回滚失败：{}", e.message)); return Err(e.message) }
apps/ui/src/pages/Settings.tsx:805-815      （按钮与确认文案）
    label: 修复网络（回滚遗留配置）
    question: 「修复网络会回滚 helper 装的路由与 DNS，并拆掉当前正在生效的那条隧道…网络会回到直连」
```

* **吞掉的是什么失败**：`rollback(&snap)` 的失败（删路由/还原 DNS/恢复被顶掉的 on-link 路由，任一步失败）。
* **失败时用户会看到什么**：**「已回滚会话 …」+ 界面一切正常** —— 而路由/DNS 可能仍留在系统上。
  这正好落在 A 级判据上：用户得出**「已回滚」这个错误结论**（也正是本项目「删除 ≠ 恢复」那一族）。
* **有没有第二条线索**：`force_cleanup` 前有一行 `tracing::warn!("强制清理遗留会话")`，
  但**回滚失败本身没有任何日志**；App 侧那条 `Err` 分支因为错误被销毁**永远不触发**。
* **建议（第二阶段，不在此卡）**：让 `force_cleanup()` **把回滚结果带出来**（`Result` 直接传播，或返回
  `(Option<Snapshot>, Option<Error>)`），`server.rs` 在失败时回 `Response::Error`/带 `partial` 标记，
  App **落日志并让界面如实说「回滚未完成，路由可能仍在」**。测试要断言「信息可见」而不是「不 panic」。
* **跨卡**：与 `task-118`（链式/防环）**不重叠**；这条是「回滚失败不可见」，不是拓扑语义。

### A-2 ⚠️ 卸载/退出路径上，回滚失败**完全静默**（连日志都没有）

```
crates/xt-helper/src/server.rs:655-660
        if let Ok(mut guard) = self.state.lock() {
            if let Some(session) = guard.session.take() {
                let _ = controller::rollback(&session.snapshot);   // ← 吞
            }
        }
        let _ = controller::force_cleanup();                       // ← 吞（且它自己还吞一层，见 A-1）
```

* **吞掉的是什么失败**：卸载/退出前最后一次「把系统改回去」的失败。
* **失败时用户会看到什么**：卸载流程「成功」结束 —— 而路由/DNS 可能留在机器上；
  用户随后看到的是「已卸载」，但网络配置没恢复（**错误结论「已清理」**）。
* **第二条线索**：**没有**（这里连 `tracing::warn!` 都没有；而且接下来就 `bootout`，进程要走了）。
* **建议**：卸载前把结果写进日志（helper 自己的日志/文件），并在响应里带上「回滚未完成」；
  若判断「卸载后无法补救」，至少**留下可查的痕迹**，不要静默。
* **诚实边界**：我没验证 helper 的日志到底落到哪个文件（不在本卡范围），二级线索引出「有/没有」以读码为准。

### A-3（候选）`process.shutdown()` 失败被吞 —— 需要上层确认后才能定级

```
apps/desktop/src/supervisor.rs:594, 618, 666, 671, 682
    let _ = process.shutdown(CORE_SHUTDOWN_GRACE).await;
```

* **风险**：核心**没被停掉**而 App 认为「已停止」⇒ 界面说「未连接」，而数据面可能还在转发。
  这是「用户以为直连、实际仍走隧道」类结论错误 ⇒ **按判据是 A**。
* **为什么标「候选」**：我**没有**追出这 5 处之后路由/DNS 的回滚是否**独立**于这个 await
  （若路由回滚另有一条不依赖它的路径，后果就降到 B）。**这条交你定级**（见 §5 诚实清单）。
* **建议**：至少 `tracing::warn!` + 一个状态位；测试断言「shutdown 失败 ⇒ 状态/日志可见」。

## 2. B 级（失败不可见，但**没有**造成错误结论）

| # | 位置 | 形状 | 吞掉什么 | 用户看到什么 | 第二条线索 | 建议 |
|---|---|---|---|---|---|---|
| B-1 | `crates/xt-core/src/update.rs:1047-1052` `InstalledMeta::load` | `.ok().and_then(…ok()).unwrap_or_default()` | 读/解析 `meta.json` 失败 | 元信息当作**空**（「没装过」）⇒ 可能触发重装或显示「未安装」 | 无 | 区分「文件不存在」与「文件坏了」；后者至少 `warn` |
| B-2 | `scripts/check.sh:88` 共享 target dir 判据 | `[ "$(cd A 2>/dev/null && pwd -P)" = "$(cd B 2>/dev/null && pwd -P)" ]` | 两个 `cd` 都失败 ⇒ 两边都是**空串** ⇒ **判为相等** | 打印「正在 linked worktree 且共用主 target dir」的**假警告**；`WT_STRICT=1` 时**以 75 假失败** | 有（后续自己会打印路径，但路径可能为空） | 两侧都要求非空；`cd` 失败要显式报「目标目录不存在」而不是「相等」 |
| B-3 | `scripts/build-lock.sh:233, 272` | `rm -rf "$dir" 2>/dev/null \|\| true` | 删锁目录失败 | 下次构建被**陈旧锁**挡住；用户以为「上一次没释放」 | 有（锁目录里有 owner 文件） | 失败要打印一行；否则「锁机制」自己会变成新的假红源 |
| B-4 | `crates/xt-core/src/update.rs:198-206` `run()` | `let _ = timeout;` | **timeout 参数被忽略**（注释承认依赖各工具自己的超时） | 调用方以为有超时保护 | 部分（curl 有 `--max-time`） | 要么真实现，要么**删掉参数**并在每个调用点注明——「签名在撒谎」正是本项目最忌讳的形态 |
| B-5 | `apps/desktop/src/supervisor.rs:495` | `xray::core_version(&core_path).await.unwrap_or_default()` | 读核心版本失败 | 显示**空版本**（不是错版本，但仍是不完整陈述） | 无 | 返回 `Option<String>`，界面显示「版本未知」而不是空串 |

## 3. 已被**手工抓到过**的四条：我逐条核对了「是否真的修干净」

| 实例 | 现状 | 证据 |
|---|---|---|
| ① `update.rs` 的 `xattr -dr … 2>/dev/null \|\| true` | **已修** | `update.rs:648-686`：不再有 `2>/dev/null`；**退出码与 stderr 原样写进日志**；并且**读回残留数**再判：「已确认全部清除」/「读回命令退出码非 0，请人工确认」/「**仍有 N 个文件带 quarantine**」。这三个分支都在 `quarantine_cleanup_block` 的生成脚本里 |
| ② `store.rs` 的 `from_str(l).ok()` 静默丢行（task-107/110） | **已修** | `store.rs:237-321`：改用 `serde_json::Deserializer::from_str(line).into_iter::<T>()`（一行多对象）；统计 `malformed_lines` + 保留坏行样本；`>0` 时 `tracing::warn!`；`tail_logs` 现在委托给 `tail_logs_with_stats(limit).0`（`:229-231`） |
| ③ `check.sh` 被并发构建弄红（task-109） | **已修成机制** | `scripts/build-lock.sh` 存在，`check.sh:82-100` 有共享 target dir 判据（但判据本身有 B-2 的问题） |
| ④ `helper version` 假报不匹配（task-111） | **尚未修** | `helper.rs` 的 `classify_helper_versions` 仍按**版本串相等**判 Mismatch；工作区里 `apps/desktop/src/commands/helper.rs` 有未提交改动（backend-dev 在做）⇒ **由 `task-111` 覆盖，本报告不重复开工作** |

## 4. C 级（**可保留**，但必须写明理由）

### 4.1 探测/清理路径的 `2>/dev/null || true`（`scripts/**`，共 118 处里的绝大多数）

代表性例子与理由：

| 例子 | 为什么保留 |
|---|---|
| `build-lock.sh:130` `kill -0 "$pid" 2>/dev/null`、`:151` `pgrep -fl …` | **探测**：失败 = 「进程不在」，语义上**就是**答案，不是错误 |
| `build-lock.sh:212/223/224/233/272` `mkdir -p` / `rm -f` / `rm -rf … 2>/dev/null` | **清理**：失败不影响正确性；**但见 B-3**（删锁失败要留痕） |
| `diagnose-network-drop.sh:143/186/188/190/191/315/325/333/403/442/445` | 诊断脚本的**尽力采集**：某条命令不可用时**照实空缺**（脚本的设计就是「能采到多少采多少」），且输出里会明显缺段 |
| `incident-bundle.sh:211` `pgrep … \| head -1 \|\| true` | 「App 没在跑」是**正常状态**（窗口退让并**显式标注 `degraded`**），不是失败 |
| `incident-bundle.sh:423/426/429` `netstat/ifconfig 2>/dev/null … \|\| true` | 同上；采集失败时对应段落为空，**不会造成错误结论**（`triage` 对缺文件记 `unavailable` 而不是 `not hit`） |
| `check.sh:82-83` `git rev-parse … 2>/dev/null \|\| true` | 结果被 `[ -n "$_git_common" ]` 守卫（见 B-2 的边界） |

### 4.2 其它有正当理由的吞

| 位置 | 为什么保留 |
|---|---|
| `crates/xt-tun/src/macos/dns.rs:187-188` `let _ = run_ok(DSCACHEUTIL …)` / `killall -HUP mDNSResponder` | **刷新缓存**：失败最多让旧缓存多活一会儿，TTL 会兜底；不会产生「已恢复」这类结论（**生产代码**，`mod tests` 从 `:191` 起） |
| `crates/xt-tun/src/macos/snapshot.rs:130` `let _ = set_permissions(0o600)` | 权限收紧失败**不影响数据正确性**（文件已 0600 创建为主路径）；严格说值得 `warn`（本卡不改）。**生产代码**，`mod tests` 从 `:140` 起 |
| `crates/xt-core/src/update.rs:433` `let _ = std::fs::remove_file(dest)`（`download_with_progress` 开头） | 目的是「从干净状态开始」；失败会被**下游更强的检查**挡住（同一文件 `:1083` 的流程是「下载 → **校验摘要** → 解压到暂存 → 结构校验 → 跑 `xray version` → 才落位」，`:851` 还专门有一条测试说「坏文件会被校验挡住」）⇒ **不会产生错误结论**。**代价**：排查时会表现为「校验失败」而不是「清理失败」，多绕一步 ⇒ 若要提升可诊性，可降为 B |
| `crates/xt-core/src/update.rs:674` `grep -c … \|\| true` | `grep -c` 在计数为 **0** 时返回 **1**（不是失败）⇒ 这里 `\|\| true` 是**正确处理**，判据是**读回的计数**而不是退出码（同一函数里已写清） |

> ⚠️ **我自己先写错了两条并当场删掉**：初稿把 `update.rs` 里成片的 `let _ = remove_dir_all(&dir)`
> 与 `macos/fdpass.rs:71` 列为 C 级候选 —— 复核模块边界后发现**它们全在 `#[cfg(test)] mod tests` 里**
> （`update.rs` 的 `mod tests` 从 `:784` 起 ⇒ 生产区里 `let _ =` **只有 `:204` 与 `:433` 两处**；
> `fdpass.rs` 的 `mod tests` 从 `:41` 起）。按 §0 的排除规则它们**根本不该进清单**。
> 这条留在这里，是因为「审计清单自己也会吞掉上下文」—— 少了模块边界这一步，清单就会把测试代码当成生产缺陷。

### 4.3 `Err(_) => {}`（全仓仅 1 处）

`crates/xt-tun/src/macos/utun.rs:270` 的 `Err(_) => true` —— 不是「吞」，是**默认分支**（把「读不到」当作某侧的结论），
之所以列出来是因为**它属于「读不到就猜」的形状**，而本项目对「读不到」有明确纪律（`task-84`：读不到不许猜成不一致）。
**判定：C（需读上下文确认语义）** —— 我没有读它的完整语义，**交你或交给实现者**，不硬判。

## 5. 诚实清单（**我判断不了 / 没做的**）

1. **A-3（`process.shutdown`）我定不了级**：需要知道「路由/DNS 回滚是否独立于这个 await」。
   我没有追完 `supervisor.rs` 的整条 teardown 链，**不硬判**。
2. **`utun.rs:270` 的 `Err(_) => true`** 我只看了形状，没有读语义，**定不了级**。
3. **未逐行审计 500+ 个候选点**：我按 §0 的排除规则聚焦「会造成错误结论」的生产路径，
   其余按**类别**给 C 级与理由；**这不是「其余都安全」的结论**。
4. **没跑任何 cargo/vitest**（本卡只审计、不改代码，也不需要构建）。
5. **没有真机复现 A-1/A-2 的失败**：这两条是**读码链**（controller → server → App → UI 文案）得出的，
   要在真机上触发「rollback 失败」需要人为制造权限/路由失败，**本卡不做有副作用的操作**。
6. **界面侧（`apps/ui/**`）只抽查了与 A-1 直接相关的确认文案**，没有做全量「陈述 vs 实现」排查——那是 `task-120`。
7. **`infra/**`（ops 的端点）不在本卡范围**，未审计。

## 6. 给 Lead 的下一步建议（**我一行都没改**）

1. **先修 A-1**（错误通道被销毁这一条最有价值：修它同时让 A-2 的 `force_cleanup` 部分受益）；
2. **A-2** 与 A-1 同源，建议**同一张卡**做；
3. **A-3 先定级**（你或原实现者补一句「回滚是否独立」即可），再决定要不要动；
4. B-2（`check.sh` 的假警告/假 75）建议**尽快**——它是**门禁**里的假信号，正好是我们最怕的那类；
5. B-3/B-4/B-5 可并入下一次清理；C 级**不必动**（除你要求「任何吞都要留痕」时把 4.2 那一族降级）。
