# task-184 · 独立验证 A20/A21「真话」修复（`task-179` = `c45a12c` + `task-181` = `7deedb9`）

> 验收人：`tester`。**不采信实现者的自测**：UI 侧的结论来自**我自己写的探针**（渲染真实组件 + 注入两种
> `self_check` / 三种 `traffic`），不是读他们的用例。
> 绑定：`af582bf`（tip，含两个目标提交；`af582bf` 只动 site/版本号，见 §1）。
> 隔离：worktree `v179`（**已被环境清理，见 §0.2**）。
> 判定：**UI 侧（item 2/5 + 两条 UI 突变）我量到并通过；Rust 执行级（item 1 的反例、A20 核心断言、
> 跨语言自动断言、两条 Rust 突变）被环境事件打断，未复测 —— 只给出读码证据，不冒充已验证。**

---

## §0 口径与一次环境事件（先说清楚）

### 0.1 证据三分

| 类别 | 本次内容 |
|---|---|
| **我量到的** | 我的 UI 探针 8/8 绿；两条 UI 突变各自的红（4 failed / 2 failed，**我的与实现者的用例同时红**）；实现者 UI 用例 9/9 绿的对照 |
| **读码得到的** | Rust `SelfCheck::judge` 四态、`read_exit_traffic` 的「只认节点 tag」、`ExitTraffic::unattributed` 的占位语义、TS↔Rust 字段名逐字段对照、canvas 标记与 `originLabel` 同源 |
| **推断的** | 节点 tag 不在统计里时 `bytes=0 且 verified=true`（读码推出，**执行级未跑**） |

### 0.2 ⚠️ 环境事件：worktree 被 `TMPDIR` 清理

`scripts/wt.sh` 默认把 worktree 放在 `${TMPDIR:-/tmp}/xraytun-wt`（本机 = `/var/folders/…/T/`），
**整个目录在 16:25 前后被系统清理**：`git worktree list` 显示 `leadgate182 / t183 / v179` 全部 pruned。
⇒ 我的 `v179` 里**正在跑的 Rust 探针构建被打断**（依赖还在编译阶段），**Rust 侧结论未能产出**。
**target dir 幸存**（`.cargo-target.wt/v179`），可复用重建。
**未复测的部分已在 §4/§5 逐条标注**（不写成「已验证」）。
（附带发现：`wt.sh` 把 worktree 放进可清理目录这件事本身值得机制化 —— 建议默认改成仓库内 `.wt/`。）

---

## §1 绑定

| 项 | 值 |
|---|---|
| 被验提交 | `c45a12c`（backend，`apps/desktop/src/commands/globe.rs` +286−33）、`7deedb9`（frontend，6 文件 +382−24） |
| 验证时 tip | `af582bf`（`git show --stat` 显示它只动 `CHANGELOG.md`/`Cargo.toml`/`Cargo.lock`/`tauri.conf.json`/`package.json`/`gen-site-*.py`/`site/**` ⇒ 不含这两个提交的范围） |
| 隐私 | 报告里用的地址全是文档段/测试段（`203.0.113.0/24`、`198.51.100.0/24`）；探针 fixture 用 `TEST-ISP` 假数据 |

---

## §2 item 1（字段语义）——**读码**结论（执行级未复测）

### 2.1 `SelfCheck::judge(iface, origin)` 的真值表（`globe.rs`）

| 输入 | `trusted` | `ip` | `bound_interface` | `reason` |
|---|---|---|---|---|
| `(Some(en0), Some(loc))` | **true** | `Some(loc.ip)` | `Some("en0")` | `None` |
| `(Some(en0), None)` | **false** | `None` | `None` | `Some("绑定 en0 的查询没有成功（两个数据源都没返回或绑卡失败）⇒ 本机位置未验证")` |
| `(None, Some(loc))` | **false** | `Some(loc.ip)` | `None` | `Some("读不到物理默认路由 ⇒ 查询走的是系统默认路由；隧道开着时那就是节点出口，查到的不是本机")` |
| `(None, None)` | **false** | `None` | `None` | `Some("读不到物理默认路由，且两个数据源都没返回 ⇒ 本机位置未知")` |

* `trusted ⇔ (绑了网卡 ∧ 拿到位置)` 在**代码结构上**成立（唯一 `trusted:true` 分支同时满足两条件）；
* `trusted ⇔ reason.is_none()` 也成立（三个 false 分支**各带一条具体 reason**，唯一 true 分支 `reason: None`）；
* 三条 reason 都**点名了原因**（网卡名 / 系统默认路由 / 两个数据源都没返回），不是「失败」两个字的空话。

### 2.2 A20：`read_exit_traffic(stats, counters, node_tag)`

* `node_tag == None` ⇒ `ExitTraffic::unattributed("没有选中的节点 ⇒ 归不到任何 outbound（不挑「最大的」顶上）")`；
* `stats == None` ⇒ `unattributed("查统计失败（核心没在跑或 API 不可达）⇒ 归属未验证")`；
* 否则 `monotonic_traffic_by_tag(...)` 之后 **只取 `by_tag.get(node_tag)`**，注释原文：
  「**只认这个 tag**：别的出站（例如 direct）流量再大也不算它的」
  ⇒ **`direct` 比节点大时，报的就是节点 tag 的数字**（A20 的核心，**读码成立**）；
* `unattributed` 的 `bytes` 是**占位 0**、`ok=false`，且注释写明「**不用 0 表示『没查到』**」；
* 构造器交叉约束：`verified_for(tag)` ⇒ `tag=Some` + `verified=true`；`unattributed` ⇒ `tag=None` + `verified=false` + `is_node_outbound=false`
  ⇒ **`is_node_outbound=true` 而 `tag=None` 这种矛盾组合在构造器层面不可达**。

### 2.3 ⏳ 未复测

我原本在工作区里准备了 3 条执行级断言（真值表 4 例、`direct` 16e9 vs 节点 5,555 字节、未归属契约），
**构建被环境事件打断**，因此**不作为已验证结论**。

---

## §3 item 2 + item 5（UI 映射有牙 + 诚实边界文案）——**我实测**

**我的探针**：`apps/ui/src/testerProvenance.probe.test.tsx`（worktree 内，未提交）。
做法：`vi.mock("./ipc")` 注入 `api.globeData`，`render(<StoreProvider><Globe/></StoreProvider>)`，
**渲染真实组件**（不是只调字符串函数），断言全部落在 `document.body.textContent` 上；另加一层纯函数断言。

```
$ npx vitest run src/testerProvenance.probe.test.tsx
 ✓ src/testerProvenance.probe.test.tsx (8 tests) 305ms
 Test Files  1 passed (1)
      Tests  8 passed (8)                     ← 我的探针全绿
$ npx vitest run src/globeProvenance.test.tsx
      Tests  9 passed (9)                     ← 实现者用例（对照）
```

**逐条判据（我的断言）**：

| 卡面要求 | 我的断言 | 结果 |
|---|---|---|
| `trusted=false` ⇒ body 不含「本机 · 」 | `not.toContain("本机 · ")` + `not.toContain("本机出口")` + `toContain("未验证的出口")` + 后端 reason 必须出现 | ✅ 通过 |
| `trusted=true` 不许写成更强承诺 | 含「多网卡」；**不含** `唯一出口|唯一的出口|一定是这个 IP` | ✅ 通过 |
| `!verified` ⇒ 不渲染数字 | 不匹配 `/\d[\d,.]*\s*(B|KiB|MiB|GiB|TiB|KB|MB|GB)\b/`、不出现 `0 B`、必须含「归属未验证」+ 后端 reason | ✅ 通过 |
| 对照：`verified` ⇒ 数字出现 | 匹配字节单位 | ✅ 通过 |
| `verified && !is_node_outbound` ⇒ 不许说成节点出站 | 含 `direct`、**不含**「节点出站累计」、含「不是节点出站」 | ✅ 通过 |
| `verified && is_node_outbound` ⇒ 才可说节点出站；不许承诺覆盖全部 | 含「节点出站累计」；**不含** `全部流量|所有流量` | ✅ 通过 |
| `ip === null` 态 | `originLabel` ⇒「本机位置未知」 | ✅ 通过（纯函数层） |

**canvas 上的起始标记（如实说明替代证据）**：`Globe.tsx:707`
`data.self_check.trusted ? "本机 · " + route.from.ip : "未验证的出口 · " + route.from.ip`
—— 它**与 `originLabel` 同源**（同一个 `self_check.trusted`），但画在 canvas 上、**DOM 读不到**，
所以我**没有**对它断言（卡面也要求如实说明这一点）。替代证据 = 读码（同源字段）+ 我上面那条 DOM 断言。
**真机 WKWebView 未验**：证据是 jsdom 渲染 + 注入数据；它测不到的部分见 §6。

---

## §4 item 3（反向敏感性）——**2 条 UI 实测 ✓ / 2 条 Rust 未复测 ⏳**

| 突变 | 期望 | 实测 |
|---|---|---|
| **M1-UI**：`originLabel` 恒返回「本机出口」（trusted 恒真） | 我的 + 实现者的用例都要红 | ✅ **`Test Files 2 failed (2)` / `Tests 4 failed \| 13 passed (17)`** |
| **M3-UI**：把渲染条件 `{r.traffic.verified ? …}` 改成恒真（去掉数字渲染条件） | 同上 | ✅ **`Test Files 2 failed (2)` / `Tests 2 failed \| 15 passed (17)`**，两条红分别是<br>`task-181 · A20：归属决定数字挂谁名下 > verified === false ⇒ 不显示数字…`（实现者）<br>`task-184 探针 · A20 流量归属 > verified=false ⇒ 不渲染任何数字…`（我） |
| **M1-Rust**：`SelfCheck::judge` 里 `trusted` 恒真 | 我的 + 实现的 globe 断言都要红 | ⏳ **未复测**（构建被清理打断） |
| **M2-Rust**：`read_exit_traffic` 改回「取最大」当归属 | 同上 | ⏳ **未复测** |

两次 UI 突变都在**同一份 worktree 源码**上做，做完立刻 `cp` 还原：
`Globe.tsx` 的 sha256 还原前后**均为 `2149417071694a0c06568e59e16c959e9cd5d710bff0f2c875ebcdec23cbad09`**（逐字节一致）。
（实现者的提交信息里也自报过同一组 UI 敏感性 —— 我是**重做**，不是引用。）

---

## §5 item 4（跨语言字段一致）——读码 ✓ / 自动断言未复测 ⏳

**逐字段对照**（`apps/ui/src/types.ts` ↔ `globe.rs` 的 `Serialize` 结构体）：

| TS 接口 | 字段 | Rust 结构体 | 一致？ |
|---|---|---|---|
| `GlobeSelfCheck` | `ip` / `bound_interface` / `trusted` / `reason` | `SelfCheck` | ✅ 同名同序（读码） |
| `GlobeTrafficProvenance` | `tag` / `is_node_outbound` / `verified` / `reason` | `TrafficProvenance` | ✅ 同名同序（读码） |
| `GlobeRoute` | `from` / `to` / `bytes` / `traffic_ok` / `counter_resets` / `node_name` / `traffic` | `GlobeRoute` | ✅ 同名（读码） |
| `GlobeData` | `route` / `origin` / `error` / `self_check` | `GlobeData` | ✅ 同名（读码） |

* Rust 侧没有 `#[serde(rename)]`/`rename_all` 作用在这四个字段上（读码），所以 serde 输出的键就是字段名；
* **`preview.ts` 已补齐**这两个字段（`git show 7deedb9 -- apps/ui/src/preview.ts` 增了
  `traffic{tag,is_node_outbound,verified,reason}` + `self_check{ip,bound_interface,trusted,reason}`）
  ⇒ 预览（唯一能截图核对的路径）能看到两种降级文案；
* ⏳ 我原计划在 Rust 探针里**读 `types.ts`、解析接口字段、与 `serde_json::to_value()` 的键集合断言相等**
  （把「读码一致」升级为「自动断言一致」）——**未跑**（同 §0.2）。

---

## §6 诚实清单

1. **Rust 执行级结论缺失**：item 1 的反例、A20 的「`direct` 比节点大」执行级断言、跨语言自动断言、
   两条 Rust 突变 —— **全部未复测**，原因见 §0.2（worktree 被系统清理，构建中断）。报告里它们只以**读码**形式出现。
2. **真机 WKWebView 未验**：UI 结论来自 jsdom + 注入数据；它**测不到**真机 canvas 渲染结果、字体/布局、
   以及「用户实际看到的那一屏」。
3. **canvas 标记未断言**（只在 DOM 之外）：我验证的是「它与 `originLabel` 同源」这一读码事实。
4. **`ip === null` 态**在今天的后端路径里据实现者说不可达（我未复核其可达性）；我只验证了它的 UI 映射。
5. 我的 UI 探针 fixture 里 `GeoLocation` 必须带 `sources`（缺了会让 `<Fact>` 崩）——这是**我自己的**
   fixture bug（第一次跑 6 红），修好后 8/8；记在这里以免被误读成产品缺陷。
6. 主树里别人的在途改动我未碰（`site/**`、`Cargo.toml`、`CHANGELOG.md` 等）。

---

## §7 复现（含本次踩到的两个环境坑）

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
# ⚠️ 坑 1：wt.sh 默认把 worktree 放 ${TMPDIR}/xraytun-wt（可被系统清理）⇒ 显式指定仓库内目录
WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt ./scripts/wt.sh new v179 af582bf
WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt ./scripts/wt.sh run v179 -- <命令>
# ⚠️ 坑 2：裸 `cargo` 会去 ~/.cargo（沙箱下 Operation not permitted）⇒ 必须走 `wt.sh run`
#          （它导出 CARGO_HOME=<repo>/../.cargo 与独立 CARGO_TARGET_DIR）
# UI 探针（不需要 cargo）：把 testerProvenance.probe.test.tsx 放进 apps/ui/src/ 后
cd apps/ui && npx vitest run src/testerProvenance.probe.test.tsx
```
