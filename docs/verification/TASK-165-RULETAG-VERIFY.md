# task-166 · `duplicate ruleTag` P0 独立验证（冻结对象 `ac82111`，`task-165`）

> 验收人：`tester`（独立复现，不采信实现者与 Lead 的转述数字）
> 被验修复：**`ac82111`**（`fix(core): ruleTag 唯一化 —— 修「自定义规则与预设/内部规则同名 ⇒ 核心拒启」`）
> 隔离：worktree `rt166`（`scripts/wt.sh`，独立 target `.cargo-target.wt/rt166`）；cargo 全部走 `./scripts/wt.sh run rt166 -- …`
> **真实核心**：`apps/desktop/binaries/xray` = `Xray 26.9.9 (Xray, Penetrates Everything.) 52a412d (go1.27.1 darwin/arm64)`
> 判定：**P0 已闭环（修后全绿）**，且我修前发现的**第三类来源（与内部规则 tag 撞名）已被修复覆盖**；附 1 条口径更正与诚实清单。
> 本轮**没有任何写操作**：未连接/断开、未改路由/DNS、未装/卸助手；用户数据**只读**。

---

## §0 三类证据（本报告的读法）

| 类别 | 内容 |
|---|---|
| **真实核心实测** | 用仓库里的核心二进制对**App 自己构建路径产出的配置**跑 `run -test -c`，看**退出码与逐字输出** |
| **单测** | `cargo test -p xt-core --lib`（我自己的 run，不是实现者的报告） |
| **读码** | `merge_rules` / `build_routing` / `validate_config` / 守卫与文案组装 |

**我的判据（独立于实现者）**：① 生成配置里 `routing.rules[].ruleTag` **全唯一**（我自己的探针从**最终配置 JSON**里数，不读实现内部结构）；② **真实核心自检的退出码**（0 = 通过，23 = 拒启）。两者都不依赖实现者的测试。

---

## §1 冻结对象与用户数据（只读）

| 项 | 值 |
|---|---|
| 被验提交 | `ac82111`（`origin/main` 同值，工作树干净） |
| 修前基线提交 | `9019e38`（我用来做 item 1 复现） |
| 用户数据 | `~/Library/Application Support/com.xraytun.desktop/settings.json`，**3686 B**，sha256 **`e7fd98ba18394af80676888b9a99f2b169ea5d54f97683a0fb698ad3e8a5d775`**（修前=修后，见 §4.6） |
| 用户 `routing_preset` | `global_proxy`（**这就是它之前能启动的原因**） |
| 用户 `custom_rules`（5 条，**只记 id**） | `preset-private`、`preset-ads`、`google-to-us`、`preset-cn-domain`、`preset-cn-ip` |
| 用户 `log_level` | `debug`（这条影响自检失败文案的体积，见 §6） |

> 报告只出现**规则 id 与条数**；节点地址/UUID 一概不出现（我生成的临时配置用 `nodes: &[]`，磁盘上不含任何用户节点数据）。

---

## §2 item 1 · 修前复现（真实核心，逐字一致）

**方法**：在 `rt166` 里加一个**临时集成测试**（`crates/xt-core/tests/ruletag_probe.rs`，未提交），它**调用 App 自己的代码路径** —— `xt_core::xray::merge_rules` + `build_pretty(CoreConfigInput{ settings, nodes:&[], selected:None, rules, profile:LocalProxy, physical_interface:None })` —— 把用户那份规则 + 预设 `bypass_mainland` 渲染成配置写到 `/tmp/ruletag/`，再用真实核心自检。

```
$ xray run -test -c /tmp/ruletag/user-bypass.json
Failed to start: main: failed to create server > app/router: duplicate ruleTag preset-private
EXIT=23
```
**与用户报的原文逐字一致。** 生成配置共 **13 条**规则，重复清单 = `preset-private` ×2、`preset-ads` ×2、`preset-cn-domain` ×2、`preset-cn-ip` ×2（第 5 个预设 id `preset-proxy-google` 没撞，因为用户那条叫 `google-to-us`）。

**两个对照（证明「我的调用 + 工具」可信，而不是「怎么跑都红」）**：
1. 同一份用户在**当前预设** `global_proxy` 下生成的配置 ⇒ `Configuration OK.`，**exit 0**（解释了它当时能启动）；
2. 我手搓一个最小配置，故意让两条规则都叫 `preset-private` ⇒ **同一个错误**、**exit 23**（工具负对照）。

**修前覆盖矩阵**（我的探针逐 case 生成 + 判别）：

| 预设 | 无同 id | 部分同 id | 全部同 id |
|---|---|---|---|
| `global_proxy` | 唯一 | 唯一 | 唯一 |
| `bypass_mainland` | 唯一 | **`preset-private`×2** | **4 个 id ×2** |
| `whitelist_proxy` | 唯一 | **`preset-private`×2** | **`preset-private`×2**（该预设 id 只有它） |
| `direct_all` | 唯一 | 唯一 | 唯一（**该预设无预设规则** ⇒ 「全部同 id」退化） |
| `custom` | 唯一 | 唯一 | 唯一 |
| `custom` 分支**内部两条同 id** | — | — | **`preset-private`×2**（真实核心也拒，exit 23） |

---

## §3 修前额外发现 · **第三类来源：与 App 内部规则 tag 撞名**（已报 Lead，并已被修复覆盖）

`build_routing` 自己会追加 3 条内部规则，它们的 `ruleTag` 是 **`internal-dns-hijack` / `internal-api` / `internal-fallback`**（`config.rs:615/627/638`）—— 这三条**不在 `merge_rules` 的输出里**。把用户自定义规则的 id 改成这三个值，**修前**逐条实测：

```
自定义 id = internal-api          → exit 23  Failed to start: … duplicate ruleTag internal-api
自定义 id = internal-fallback     → exit 23  … duplicate ruleTag internal-fallback
自定义 id = internal-dns-hijack   → exit 23  … duplicate ruleTag internal-dns-hijack
```
⇒ 若唯一化只做在 `merge_rules` 上，这三个入口**仍会拒启**。Lead 据此收紧了 `task-165` 的范围；**修后这三个入口全部 exit 0**（§4.4）。

---

## §4 修后（`ac82111`）逐项

### 4.1 读码：唯一化落在**两层**

* 第一层 `merge_rules`（`config.rs:687+`）：两个分支（`Custom` / 预设+自定义）都对 `id` 列表调用 `uniquify_tags`，重复者按**首次原样、之后 `#2`/`#3`…** 确定性加后缀，且**只改 `id`（= `ruleTag`），不删规则、不合并规则**（注释说明了为什么不「同名即同规则」）。
* 第二层 `build_routing` 末尾（`:652`）`uniquify_rule_tag_values(&mut compiled)`：作用在**最终 `rules` 数组**上，因此**看得见全部三处来源**（预设、自定义、3 条内部规则）—— 这正是 §3 那三个入口的兜底。

### 4.2 修后矩阵（22 个 case，全部 `unique=true` 且 `order_ok=true`）

* 用户形态 `bypass_mainland`：**13 条**（与修前**同数**）、全唯一、顺序不变（我的 `order_ok` 判据 = 生成的 tag 与「`internal-dns-hijack`、`internal-api`、预设…、自定义…、`internal-fallback`」**逐位对应**，允许 `#n` 后缀）；
* 5 预设 ×（无/部分/全部）= 15 case、`custom` 内部两条同 id、`internal-*` 三个入口：**全部唯一**。

### 4.3 真实核心 sweep（修后）

```
$ for f in /tmp/ruletag/*.json; do xray run -test -c "$f"; done
21 份由 App 构建路径生成的配置 ⇒ 全部 exit=0 / Configuration OK.
（含 user-bypass、bypass_mainland-all、custom-internal-dup、internal-internal-api/fallback/dns-hijack …）
手搓的 min-dup.json（我自己的重复配置，不属于修复路径）⇒ 仍 exit=23 duplicate ruleTag preset-private
```
最后那条**很重要**：它证明这次 sweep **不是「怎么跑都绿」**（若全绿是假的，它也会绿）。

### 4.4 三个内部 tag 入口（专测）

```
internal-internal-api          exit=0  Configuration OK.
internal-internal-fallback     exit=0  Configuration OK.
internal-internal-dns-hijack   exit=0  Configuration OK.
```
⇒ 我发现的第三类来源**确实被第二层覆盖**。

### 4.5 幂等（同样输入两次，tag 稳定）

同样输入跑**两遍**，22 份生成配置**逐字节相同**（`shasum -a 256` 逐份比对，`diff` 为空）⇒ 后缀命名稳定、可复现，没有引入时间/随机因素。

### 4.6 用户数据未被修改

```
settings.json sha256 修前 = e7fd98ba18394af80676888b9a99f2b169ea5d54f97683a0fb698ad3e8a5d775
settings.json sha256 修后 = e7fd98ba18394af80676888b9a99f2b169ea5d54f97683a0fb698ad3e8a5d775
（size 3686 B 不变；`runtime/config.json` 只读跑过一次自检做对照）
```

### 4.7 `xt-core` 单测与 clippy（我自己的 run）

```
$ cargo test -p xt-core --lib
test result: ok. 233 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 8.12s

$ cargo clippy -p xt-core --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 17.81s      CLIPPY_EXIT=0
```

---

## §5 item 4 · 反例有牙（我自己写的三个突变，**按层**给结论）

突变只在 worktree `rt166`，每次跑完 `git checkout -- .` 还原（收尾 `status` 只剩我的临时探针目录）。

| 突变 | 我判据 A：最终配置 `ruleTag` 唯一 | 我判据 B：真实核心 | 仓库自带 xt-core 单测 | 结论 |
|---|---|---|---|---|
| **m1** 只去掉**第一层**（`merge_rules`） | **全部唯一**（22 case 无重复） | **全部 exit 0** | **1 failed**：`user_rule_shape_with_bypass_mainland_is_uniquified_without_losing_rules` | ⚠️ 第一层对**最终产物**是**冗余**的（第二层已兜住）；它是一层**契约/单测防线**。⇒ 「去掉唯一化 ⇒ 真实核心必须红」这条**只有在第二层也被去掉时才成立**（见 m3） |
| **m2** 只去掉**第二层**（最终数组） | `internal-*` 三个 case **unique=false**（各 ×2）；预设/自定义仍唯一 | `internal-*` **exit 23**（`duplicate ruleTag internal-api` 等）；其余 exit 0 | **2 failed**：`custom_rule_colliding_with_an_internal_tag_keeps_its_routing_semantics`、`generated_config_rule_tags_are_unique_for_every_preset_and_collision_shape` | ✅ 第二层正是 §3 那类入口的**唯一**兜底 |
| **m3** **两层都去掉** | 全面复现修前：`user-bypass` 4 个 id ×2、`custom-internal-dup`、`internal-*` 全重复 | `user-bypass` **exit 23 `duplicate ruleTag preset-private`**（**与用户原文逐字一致**）；6/6 目标配置全 exit 23 | **4 failed** | ✅ **反例有牙成立**（卡面要求的「去掉唯一化 ⇒ 必须红」由 m3 满足） |

**我要明确写在卡面上的口径建议**：如果验收判据写成「去掉第一层就必须让真实核心变红」，那是**写强了** —— 实测第一层撤掉后真实核心仍 exit 0（第二层兜底）。正确的表述是：**最终产物唯一性由第二层保证；第一层另有一组单测直接断言 `merge_rules` 的输出**（m1 下它变红）。

---

## §6 item 5 · 自检文案（修前 vs 修后）

**修前**（`9019e38`，我调**真代码路径** `xt_core::xray::validate_config` 后按 `supervisor.rs:534` 组合）：
```
APP_MESSAGE 字符数=3468 行数=35
首行=生成的配置未通过核心自检：Xray 配置非法: Xray 26.9.9 …（版本横幅）
末行=Failed to start: main: failed to create server > app/router: duplicate ruleTag preset-private
是否含「下一步/请/怎么办」字样 = false
```
⇒ **一整段原始日志**（35 行），可操作的那句在最后一行，**没有指名冲突、没有下一步** —— 按 Lead 的要求已报。

**修后**（`ac82111`，`config_self_check_message`；用我手搓的 `min-dup.json` 触发）：
```
APP_MSG 字符数=522 行数=9
规则标识（ruleTag）重复 —— 核心会因此拒绝启动：
  · `preset-private` 出现 2 次（自定义）
App 已自动为**后出现**的那条加后缀（`#2`、`#3`…）保证唯一 —— 这**不改变路由行为**（`ruleTag` 只用于日志排障）；若你不想让两条都生效，请在「路由」页删掉其中一条。

核心原始输出（共 4 行，只保留末尾 4 行）：
… （末行）Failed to start: … duplicate ruleTag preset-private
```
⇒ **结论 + 指名 tag + 次数 + 来源（自定义）+ 下一步**，再附**截断后的**日志尾部（`SELF_CHECK_TAIL_LINES = 8`）✔ 满足 Lead 钉的验收项。

**一条口径更正（关于「3445 字节 vs 382 字节」）**：backend-dev 把这差异归因于「两种调用形式输出量级不同」。我的证据指向**另一个主因 —— 配置里的 `log.loglevel`**：用户是 `debug` ⇒ 生成配置 `log.loglevel=debug` ⇒ 失败输出 **35 行 / 3445 B**（含 10 行 `[Debug]`）；而 `min-dup.json` 我写的是 `warning` ⇒ **4 行 / 382 B 量级**，修后文案里也自报「共 4 行」。截断是按**行数**（末尾 8 行），与字节数无关。两种说法都可能同时成立，但**只看字节数会把「用户 log level」这个主因漏掉**。

---

## §7 复现命令

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
df -g /Users/xbtg- | tail -1
./scripts/wt.sh new rt166 ac82111            # 或 new 后 git checkout ac82111
# 探针（临时文件，见附录；放在 worktree 的 crates/xt-core/tests/ruletag_probe.rs）
./scripts/wt.sh run rt166 -- env XT_USER_DIR="$HOME/Library/Application Support/com.xraytun.desktop" \
  cargo test -p xt-core --test ruletag_probe ruletag_matrix -- --nocapture
CORE=./apps/desktop/binaries/xray
$CORE run -test -c /tmp/ruletag/user-bypass.json      # 修后 exit 0 / Configuration OK.
$CORE run -test -c /tmp/ruletag/internal-internal-api.json
# App 层文案（镜像 supervisor 的组合）
./scripts/wt.sh run rt166 -- env XT_USER_DIR="…" XT_CORE="$PWD/$CORE" XT_DUP_CFG=/tmp/ruletag/min-dup.json \
  cargo test -p xt-core --test ruletag_probe app_message_post_fix -- --nocapture
./scripts/wt.sh rm rt166                     # 收工：连独立 target 一起删
```

**探针要点**（完整临时文件随 worktree 删除；以下为其全部关键逻辑）：
1. `user_settings()`：把用户 `settings.json` 反序列化成 `AppSettings`（**只读**）；
2. 逐 case 设 `routing_preset` / `custom_rules` ⇒ `merge_rules` + `build_pretty(CoreConfigInput{ nodes:&[], selected:None, profile:LocalProxy, .. })` ⇒ 写 `/tmp/ruletag/<case>.json`；
3. 从**最终配置 JSON** 里取 `routing.rules[].ruleTag`，算重复、算 `order_ok`（期望序列 = `internal-dns-hijack`,`internal-api`,`预设…`,`自定义…`,`internal-fallback`，允许 `#n` 后缀）；
4. `app_message_post_fix`：调 `xt_core::xray::validate_config`（真核心）拿到错误，再按 `supervisor.rs` 的方式 `config_self_check_message(settings, config, err)` 打印 App 真正给用户的文案。

---

## §8 诚实清单

* **没有**在真机上走「界面里切预设 → 点启动」这条 UI 路径（那要驱动 GUI + 拨动真实网络状态，本卡边界不允许）。我用的是 **App 自己的配置构建代码路径 + 真实核心自检**，两者合起来覆盖「配置非法 ⇒ 核心拒启」这一环；**UI 到 `merge_rules` 之间**的 GUI 层未验。
* 临时配置用 `nodes: &[]`：拒启发生在 `app/router` 阶段，**与节点无关**；但这不等于我复现了用户那份**完整**配置（他的 TUN/节点/端口部分没有参与）。真实核心的对照（用户当前 config `Configuration OK.`、手搓重复配置 exit 23）用来证明我的调用方式可信。
* **顺序判据**是「剥离 `#n` 后缀后逐位对应 + 条数相等」，不是 AST 级；`routeTag` 的后缀命名规则我按实测（`#2`/`#3`）记录，未去读它的实现细节。
* `cargo clippy -p xt-core --all-targets -- -D warnings`（我自己的 run）：**`CLIPPY_EXIT=0`**（第一次前台调用超时被杀、无残留进程、锁已释放；后台重跑 17.81s 通过）。**workspace** 级测试与 clippy 属**修复卡自己的门禁**范围，我没跑；我只对自己的判据负责：`xt-core` 单测 233 passed / 0 failed。
* 突变只在 worktree；`m1` 的 `unused`/`dead_code` 类告警我没有专门收集（我只记录单测红点与核心退出码）。
* 我**没有**修改任何 `main` 源码；报告与探针都在 worktree / `/tmp`；本卡没有 POST、没有网络写操作。
