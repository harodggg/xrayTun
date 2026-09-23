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
* **同构 ≠ 同源**：脚本是 Python、权威是 Rust。**没有**任何自动化断言「两者行为一致」——
  本卡的保障是「四类用例 + 双向敏感性」，不是「跨语言契约测试」。⇒ 建议（另开卡）：
  要么加一条契约测试，要么让脚本直接吃 Rust 侧同一份 fixture 期望值。
* **自测不在门禁里**：我 grep 了 `scripts/check.sh`，它**没有**调用这两个脚本的 `--self-test`
  ⇒ 这些用例目前只在人手动跑时生效。建议把三条自测挂进 `check.sh`（另开卡）。
* **历史字段保留原文**：我们不改写已上传的用户包（`manifest.json` 仍是旧写法），
  只在**分诊侧**如实标注；所以「现场包自己写着 Mismatch」这件事在旧包里会继续存在。
* 本次时间线：缺陷由 `task-170` 的现场包暴露 → `task-171` 修复（本文件）；`task-170` 的
  `SUMMARY.md` §4 已同步指向本修复。
