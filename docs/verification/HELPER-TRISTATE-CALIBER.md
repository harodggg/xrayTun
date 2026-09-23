# helper 三态口径：脚本 vs 产品（`task-171`）

> 缺陷来自**第一份真实用户现场包** `INC-20260923-123641-af29`：
> 包内 `manifest.versions.helper.check.state = **Mismatch**`（installed 0.8.35 / bundled 0.8.36），
> 但两个二进制**都报 `(protocol 1)`** ⇒ 按产品口径应当是 **Match**。
> 后果：`helper-mismatch` 信号被**误触发**，现场包与我们对外说的「本版不必重装」**自相矛盾**。

## 1. 权威在哪（**不要在脚本里发明规则**）

| 位置 | 规则 |
|---|---|
| `apps/desktop/src/commands/helper.rs:139-147`（`helper_versions_are_compatible`） | 两边都读到时：**协议号相等 ⇒ 兼容**；协议号**任一边读不到** ⇒ 退回**包版本相等** |
| `apps/desktop/src/commands/helper.rs:176-203`（`classify_helper_versions`） | 两边都读到才比；**任一边读不到 ⇒ `Unreadable`**（「不许猜成不一致」） |
| `apps/desktop/src/state.rs:578-584` | 「协议号相等 ⇒ **界面不该提示**」 |

**权威是 Rust**；脚本侧只能是**同构实现**（本卡把它收进一个共享模块，见 §2）。

## 2. 修法

1. **新增 `scripts/helper_tristate.py`**：`parse_probe()` / `classify()` / `classify_from_outputs()` + `--self-test`。
   `incident-bundle.sh` 与 `triage-incident.py` **都 import 它** ⇒ 两处不可能再各写一份口径。
2. **`incident-bundle.sh`**：`helper_version()`（只取包版本）→ `classify_from_outputs()`；
   manifest 的 `helper.check` 在**保持向后兼容字段**（`state` / `installed` / `bundled`，Match 时还有 `version`）之外，
   新增 `criterion`（`protocol` 或退化路径）、`installed_protocol` / `bundled_protocol`、`state_by_product_rule`。
3. **`triage-incident.py` 的 `helper-mismatch` 信号**：**不照抄 manifest 的旧字段**，而是
   * 新格式（有 `criterion`/`*_protocol`）⇒ 按产品口径**复算**；
   * **老格式**（只有包版本）⇒ 产品口径是协议号 ⇒ **无法从本包判定**：标 `state_by_product_rule = unknown`
     并给说明（**不算命中**、`near_miss=0.7`），说明里点名历史真例。
4. **修掉那句假注释**：`incident-bundle.sh` 头部原写「与产品自己判三态用的是**同一条口径**」——
   在原实现下这是**假的**；现在改成准确描述（协议号优先 + 退化路径 + `Unreadable`）并指向权威行号。

## 3. 测试（全部是脚本自测，不跑 cargo）

```bash
python3 scripts/helper_tristate.py --self-test       # 四类用例 + 双向敏感性
bash    scripts/incident-bundle.sh --self-test       # 脱敏 fixture + ↑ 的三态自测
python3 scripts/triage-incident.py --self-test       # 8 条 signature + ↑ 的额外断言 + 逐条敏感性
```

**原始输出（关键行）**：

```
=== helper 三态：四类用例（与 helper.rs 同构）===
  ✓ 包版本不同 + 协议号相同 ⇒ Match           ← 本卡的核心用例（旧口径会判 Mismatch）
  ✓ 协议号不同 ⇒ Mismatch
  ✓ 协议号读不到 + 包版本相同 ⇒ Match          ← 退化路径
  ✓ 协议号读不到 + 包版本不同 ⇒ Mismatch        ← 退化路径
  ✓ 两边都读不到 ⇒ Unreadable
  ✓ 一边读不到 ⇒ Unreadable
  ✓ 判据字段：协议号可用时标 protocol
=== 双向敏感性（改坏判据/解析 ⇒ 上面必须红）===
  ✓ 改回包版本判据 ⇒ 用例1 变成 Mismatch（原断言会红）
  ✓ 协议号解析改坏 ⇒ 用例1 变成 Mismatch（原断言会红）
  ✓ （对照）解析改坏后 版本相同 仍 Match
helper_tristate self-test：**全部通过**

# triage 自测里的新增断言
=== helper 三态（协议号口径）额外断言 ===
  ✓ 老格式 manifest（只有包版本）⇒ **不判成不一致**，标为无法判定: got='unknown'
  ✓ 老格式：判据里必须点出「产品口径是协议号」
  ✓ 老格式：说明里必须写明「旧脚本的包版本判据 ≠ 不兼容」
  ✓ 协议号相同 + 包版本不同 ⇒ **不命中**（产品口径 Match）
```

三个自测的退出码：`helper_tristate` **0** / `incident-bundle.sh --self-test` **0** / `triage-incident.py --self-test` **0**；
`grep -c '✗'` 在 triage 输出里 = **0**。

## 4. 真实现场包回归（端到端）

对同一份 `INC-20260923-123641-af29` 重跑分诊（脚本前后对比）：

```
修前 signature=multiple  命中 4：probe-false-negative / **helper-mismatch** / loopback-hole / `tun-iface-einval`
修后 signature=multiple  命中 3：probe-false-negative / loopback-hole / `tun-iface-einval`（**helper-mismatch 不再命中**）
  helper-mismatch: hit=False
    state_by_product_rule = unknown
    判据 = 老格式：manifest 只有包版本；产品口径是**协议号** ⇒ 无法从本包判定
    说明 = manifest 里的 Mismatch 是**旧脚本的包版本判据**，不等于不兼容。真例：…两个二进制都报 `(protocol 1)`，按产品口径应为 Match。要判定请对两个二进制跑 `<binary> version`。
```

（修后输出另存为 `docs/incidents/INC-20260923-123641-af29/incident.after-caliber-fix.json`；
原 `incident.json` **保持原样** —— 那是「当时的工具这么说」的记录，不追改。）

## 5. 诚实清单（这条修复**测不到**什么）

* **老包无法自动判定**：老 manifest 只有包版本，本流程**不会**去读用户机器上的两个二进制（现场包是离线的）
  ⇒ 我改成 `unknown` + 说明，**不猜**；要判定必须有人在本机跑 `<binary> version`。
* **同构 ≠ 同源** → **已由 `task-173` 修**：Python 与 Rust 现在读**同一份夹具**
  `scripts/fixtures/helper-version-cases.json`（Rust 是权威，夹具是共同真源），见 §6.2。
* **自测不在门禁里** → **已由 `task-173` 修**：`scripts/check.sh` 新增一步跑三条自测，任一非 0 ⇒ 门禁红，见 §6.1。
* **历史字段保留原文**：我们不改写已上传的用户包（`manifest.json` 仍是旧写法），
  只在**分诊侧**如实标注；所以「现场包自己写着 Mismatch」这件事在旧包里会继续存在。
* 本次时间线：缺陷由 `task-170` 的现场包暴露 → `task-171` 修复（本文件）；`task-170` 的
  `SUMMARY.md` §4 已同步指向本修复。

---

## 6. `task-173`：把「靠人守」变成机制

### 6.1 门禁新增一步（`scripts/check.sh`）

位置：**「CSS token 定义性」之后、「前端构建」之前**（纯脚本、**不依赖 cargo**、秒级）。
内容（原文）：

```bash
step "现场包脚本自测（helper 三态 / 脱敏 / 分诊；不依赖 cargo）"
python3 scripts/helper_tristate.py --self-test
bash scripts/incident-bundle.sh --self-test
python3 scripts/triage-incident.py --self-test
```

* `check.sh` 是 `set -euo pipefail` ⇒ **任一非 0 直接门禁红**，且**没有任何 `|| true`**（不许吞错）；
* **不动 build-lock 判据**、不动退出码语义（全绿 0 / 有失败非 0）、不动锁的获取与释放；
* `bash -n scripts/check.sh` 通过。

**实测（完整 `check.sh`）**：

```
$ ./scripts/wt.sh run t173 -- env BUILD_LOCK_HELD_BY_US=1 ./scripts/check.sh
…
==============================================================
  现场包脚本自测（helper 三态 / 脱敏 / 分诊；不依赖 cargo）
==============================================================
=== 共享夹具：helper-version-cases.json（8 条；Rust 侧读同一份）===
  ✓ [夹具] real-shape: 包版本不同但协议号相同 ⇒ Match（引发 task-171 的真实形态） ⇒ state …
…（8 条夹具 × state/reason + 三条自测全部 ✓；`grep -c '✗'` = **0**）
✓ 与 CI 相同的全部检查通过
CHECK_EXIT=0
```

（本次完整运行在隔离 worktree `t173`（**独立 target**）上执行，含 release 构建；`t173` 的基修订是
`96df259` + 本卡的 5 个文件 —— 与 `main` 的差异只是别人后来的提交。）

### 6.2 跨语言共同真源（Python ↔ Rust）

**夹具**：`scripts/fixtures/helper-version-cases.json`，每条
`{name, installed_out, bundled_out, expect_state, expect_reason}`，共 **8 条**，含**引发 `task-171` 的真实形态**
（`0.8.35 (protocol 1)` vs `0.8.36 (protocol 1)` ⇒ **Match**）、协议号不同但**包版本相同**的对抗用例、
协议号读不到的退化（包版本同/不同）、任一边读不到、以及「输出不是我们的形状」。

* **权威**：`apps/desktop/src/commands/helper.rs`（`:139-147` + `:176-203`）；
* **Python**：`helper_tristate.py --self-test` **改为读夹具**（不再内联一份）；
* **Rust**：`apps/desktop/src/commands/helper.rs` 的测试 `helper_version_cases_fixture_matches_authoritative_rule`
  **读同一份文件**（`read_to_string` + `CARGO_MANIFEST_DIR/../../scripts/fixtures/...`；
  **文件缺失即失败**，不会静默跳过）；门禁的 `cargo test --workspace` 里能看到它 ok。

### 6.3 双向敏感性（改坏 ⇒ 两边都红，原始输出见下）

| 手法 | Python 自测 | Rust 测试（权威） | 门禁 |
|---|---|---|---|
| **改坏夹具第 1 条 `expect_state`**（Match → Mismatch） | **`PY_EXIT=1`**：`✗ [夹具] real-shape… ⇒ state (got='Match', want='Mismatch')` | **`0 passed; 1 failed`**：`panicked at apps/desktop/src/commands/helper.rs:365: assertion left == right failed: 夹具用例 state 不符：real-shape…` | **`CHECK_RED_EXIT=1`**（那一步打印同一条 ✗ 后被 `set -e` 中止） |
| Python 判据改回「包版本相等」（`task-171` 修掉的那条） | **`EXIT=1`**：第 1 条 + 对抗用例（协议不同但版本相同）同时红 | — | — |
| 协议号解析改坏（永远读不到） | **`EXIT=1`**：第 1 条退化后变 Mismatch | — | — |

还原校验：改坏后 `cp` 回备份，主树与 worktree 夹具 sha256 都是 `fa52dea4…`（逐字节一致）。

### 6.4 操作规则（**改判据必须同时改夹具**）

1. **判据是权威（Rust）；夹具是共同真源**。任何「改判据」的动作必须**同一个提交**里改夹具：
   * 新增分支 ⇒ 夹具加一条（至少一条**真实形态**用例）；
   * 改语义 ⇒ 夹具里受影响的 `expect_state`/`expect_reason` 一起改，**Python 与 Rust 一起重跑**；
2. 只改夹具（不加实现）⇒ 两边**必须同时红**（这就是「夹具有效」的证明）；
3. 只改实现（不动夹具）⇒ 若行为变了，两边**至少一边红**；若两边都绿，说明**该行为没有夹具覆盖** ⇒ 补夹具；
4. 跑门禁即包含这三条自测（§6.1）⇒ 「判据存在但不被执行」这个形状已被关掉。

### 6.5 诚实清单（本卡**测不到**什么）

* **夹具只覆盖「解析 + 分类」**，不覆盖**执行二进制**（权限被拒、二进制不存在、非零退出码）——
  那些走 `out()` 的失败路径在 `incident-bundle.sh` 里，不在夹具里。
* 夹具不覆盖 `version` 输出的**多行/前后空白/其它子命令**（Rust 侧 `parse_helper_probe` 只看第一行，
  这些形状差异会落到「读不到 ⇒ Unreadable」，但没有专门的夹具用例）。
* **CI 上没有 macOS helper 二进制 ⇒ 这条自测照样跑**：三条自测**都不执行任何 helper 二进制**
  （夹具驱动），所以 Linux/macOS CI 都能跑；**真正需要二进制的是 App 运行时**（`helper_version_check`），
  不是这条自测。⇒ 反面：这条自测**不能**代替「真机上 App 与 helper 的版本判定」端到端验证。
* 那一步依赖 `python3` 与 `bash`（CI 两者都有）；**不依赖 cargo**，所以它跑在门禁很早的位置。
