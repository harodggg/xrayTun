# ruleTag 唯一性：仓库内的真实核心验收（`task-169`）

> 由来：`task-166`（提交 `c25a0d5`，报告 `TASK-165-RULETAG-VERIFY.md`）用真实核心跑通了这条验收，
> 但**探针只活在 worktree / `/tmp` 里 = 等于不存在**。本页 + `crates/xt-core/tests/rule_tag_uniqueness.rs`
> 把它收进仓库，让下一个人一条命令就能重跑。
> 被验行为：`ac82111`（`task-165`）——自定义规则 id 与预设/内部规则 tag 同名时，Xray 会在
> `app/router` 阶段拒绝启动（用户报的原文：`duplicate ruleTag preset-private`）。

---

## 1. 落点与理由

**`crates/xt-core/tests/rule_tag_uniqueness.rs`**（集成测试，385 行）。

* 选这里而不是 `examples/`：判据需要**断言**（唯一性、顺序、条数、两次一致、核心退出码、负对照），
  集成测试能直接给**退出码**，且能被 `cargo test` 一次跑全；`examples/` 只能打印、不产生红/绿。
* 不放在 `#[cfg(test)] mod tests` 里：那要改生产文件；本卡明确**不改生产代码**。
* 测试**不需要改任何生产代码**（用的是 `merge_rules` / `build_pretty` / `validate_config` 这些已有公开 API）。

## 2. 判据分层（**别写成「撤掉第一层真实核心就得红」**）

* **最终产物唯一性由第二层保证**：`build_routing` 在**最终 `rules` 数组**上做唯一化，因此它看得见
  **三处来源** —— 预设规则、自定义规则、以及 App 自己追加的三条内部规则
  （`internal-dns-hijack` / `internal-api` / `internal-fallback`）。
* **第一层（`merge_rules`）另有一组单测直接断言其输出**：它保证「预设 + 自定义」这一段的 id 唯一，
  是**契约层**；但撤掉它，最终配置**仍然唯一**、真实核心**仍然 exit 0**。

`task-166` 的按层突变实测（每行一句，原始证据见 `TASK-165-RULETAG-VERIFY.md` §5）：

| 突变 | 我的判据（最终配置唯一 / 真实核心） | 仓库单测 | 结论 |
|---|---|---|---|
| `m1` 只撤第一层（`merge_rules`） | 全唯一 / **全 exit 0** | **1 条红**（`user_rule_shape_with_bypass_mainland_is_uniquified_without_losing_rules`） | 第一层对最终产物**冗余**，是契约/单测防线 |
| `m2` 只撤第二层（最终数组） | 三个 `internal-*` **unique=false / exit 23** | **2 条红** | 第二层是那三个入口的**唯一**兜底 |
| `m3` 两层都撤 | 用户形态 4 个 id ×2、**exit 23 `duplicate ruleTag preset-private`（与用户逐字一致）** | **4 条红** | 「去掉唯一化必须红」由 `m3` 成立 |

## 3. 覆盖范围（与 `task-166` 的 22 份配置的映射）

**不需要核心（默认跑，CI 上也跑）**：

| 用例 | 覆盖 |
|---|---|
| `rule_tags_are_unique_for_every_preset_and_collision_shape` | **19 个 case** = 5 预设 ×（无 / 部分 / 全部同 id）+ `custom` 分支内部两条同 id + 三个 `internal-*` 入口；每个 case 断言**唯一 + 顺序不变 + 条数不变** |
| `user_shape_is_uniquified_without_losing_rules` | 用户真实形态（5 条 id × `bypass_mainland`）⇒ **13 条**、全唯一、**5 条自定义一条不丢** |
| `same_input_builds_byte_identical_configs` | 两次构建**逐字节相同**（后缀稳定） |

**需要真实核心（`--ignored`）**：

| 用例 | 覆盖 |
|---|---|
| `real_core_accepts_generated_configs_and_rejects_duplicates` | 6 份 App 构建路径产出的配置（用户形态 + `custom` 内部重复 + 三个 `internal-*` + 当前预设对照）必须 `exit 0`；**外加一条手搓的重复配置作为负对照**，必须被拒且错误**指名** `duplicate ruleTag preset-private` |

> **合计映射**：19（矩阵）+ 2（用户形态、当前预设：分别由 `user_shape_*` 与 `#[ignore]` 里那份对照覆盖）
> = **21**，与 `task-166` 的矩阵一致；再加手搓负对照 = **22 份配置**。

## 4. 怎么跑（`--ignored` 示例）

```bash
# 1) 不需要核心的部分（CI 默认）
cargo test -p xt-core --test rule_tag_uniqueness

# 2) 真实核心验收：核心在仓库默认位置（apps/desktop/binaries/xray）
cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture

# 3) 核心在别处
XT_CORE=/abs/path/to/xray cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture
```

**核心不存在时的行为**：`#[ignore]` 用例先解析核心（`XT_CORE` → 否则 `<repo>/apps/desktop/binaries/xray`）；
解析不到可执行文件时 `eprintln!` 一行说明并**直接返回（测试算通过）** —— 目的是**让没有核心的 CI 依然绿**。
代价见 §6：**CI 上这条等于没跑**。

## 5. 实测原始输出（`task-169` 本次）

⏳ **待窗口结束重跑后填最终数字**（v0.8.37 冻结窗口期间禁止编译；下面是从 `task-166` 与 `task-169` 冻结前的运行里已经拿到的部分）。

```
$ cargo test -p xt-core --test rule_tag_uniqueness          # 不需要核心（CI 默认）
running 4 tests
test real_core_accepts_generated_configs_and_rejects_duplicates ... ignored, 需要真实核心二进制（XT_CORE 或 apps/desktop/binaries/xray）
test user_shape_is_uniquified_without_losing_rules ... ok
test same_input_builds_byte_identical_configs ... ok
test rule_tags_are_unique_for_every_preset_and_collision_shape ... ok
test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s
EXIT=0

# 模拟「CI 上没有核心」：XT_CORE 指到不存在的路径 ⇒ 必须 SKIP 且绿
$ XT_CORE=/nonexistent/xray cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture
SKIP：没找到真实核心（XT_CORE 未设，且 apps/desktop/binaries/xray 不存在）—— 这条用例在**没有核心的 CI 上等于没跑**，真实核心验收必须在本地/发布前手动跑。
test real_core_accepts_generated_configs_and_rejects_duplicates ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 3 filtered out; finished in 0.00s
EXIT=0

# 真实核心（worktree 里的 apps/desktop/binaries/xray 符号链接 → 仓库核心）
$ cargo test -p xt-core --test rule_tag_uniqueness -- --ignored --nocapture
test real_core_accepts_generated_configs_and_rejects_duplicates ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 3 filtered out; finished in 2.52s
EXIT=0        ← 6 份配置被核心接受（exit 0）+ 手搓负对照被拒且错误指名 duplicate ruleTag preset-private

$ cargo test -p xt-core --lib
test result: ok. 233 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 10.57s
EXIT=0

$ cargo clippy -p xt-core --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.36s
EXIT=0
```

## 6. 诚实清单（这条测试**测不到**什么）

* **App 的 supervisor 接线路径未覆盖**：这里直接调 `merge_rules` + `build_pretty` + `validate_config`，
  **不是**「点界面 → 切预设 → 启动核心」那条路（真机 UI 需要 GUI 与真实网络状态）。
* **核心不存在时等于没跑**（CI 常态）：所以它**不能**替代发布前的真实核心验收 ——
  发布门禁里必须显式跑一次 `--ignored` 并**确认它没有打印 SKIP**。
* **只验「配置能被核心接受」**，不验路由**行为**（哪条规则命中）：后者需要真机流量。
* 顺序判据是「剥离 `#n` 后缀后逐位对应 + 条数相等」，不是 AST 级。
* 它固定用 `nodes: &[]`、`selected: None`（与 `task-166` 同做法）⇒ 与节点/端口无关；
  拒启发生在 `app/router` 阶段，所以这对本判据无害，但**不等于**复现了用户的完整配置。

## 7. 本次附带发现的工具假信号（写进 `GUARD-FALSE-GREEN-PATTERNS.md` R7 ⑤）

`scripts/build-lock.sh:150-152` 的「未持锁 cargo」判据是 `pgrep -fl '[c]argo|[r]ustc'` ——
**它匹配的是任意进程的完整命令行文本**，所以：

* 一个**描述** cargo 命令的 heredoc/脚本（我在后台作业里写的 `bash -c cat > /tmp/t169-run.sh <<'SCRIPT' … cargo test …`）
  会被算成「并发构建」；
* 一个正在 `pgrep cargo` 的 watcher 同样中招。

代价：v0.8.36 与 v0.8.37 的冻结门禁各被**打成 `GATE_EXIT=75` 空跑一次**（75 = 环境问题，不是代码失败）。
⇒ 修法建议（已转 `task-162`）：strict 判据不要读 `pgrep -f` 的全命令行，改为按**锁目录 / 父进程链 / pid 归属**判定；
**我自己的操作纪律**：冻结窗口内不挂任何命令行含 `cargo`/`rustc` 字样的后台作业，跑 cargo 一律**先落脚本文件、再 `bash /tmp/x.sh` 启动**。

另附一条我自己的 bug（留档，不当成产品缺陷）：本测试第一版把矩阵 case 数写成 `assert_eq!(cases, 22)`，
实际矩阵是 **19**（另 2 个由「用户形态」「当前预设」覆盖）⇒ 那条**我自己的断言**把测试打红；
修正为 19 并写明与 21/22 的映射。**教训与规范同源**：断言里的数字必须与它真正覆盖的集合对应
（"我数错了" 与 "实现错了" 在测试输出里长得一样）。
