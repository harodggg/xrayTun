#!/usr/bin/env python3
"""helper 三态判定（**与产品同构**；供 `incident-bundle.sh` 与 `triage-incident.py` 共用一条口径）。

# 权威在哪（**不要**在这里发明规则）

真实权威是 Rust：`apps/desktop/src/commands/helper.rs`
* `helper_versions_are_compatible`（`:139-147`）：两边都读到时，**先比协议号**（相等 ⇒ 兼容），
  协议号**任一边读不到** ⇒ 退回「包版本相等」；
* `classify_helper_versions`（`:176-203`）：两边都读到才比；**任一边读不到 ⇒ `Unreadable`**
  （「不许猜成不一致」）。

本模块是**同构实现**：`python3 scripts/helper_tristate.py --self-test` 用四类用例 + 双向敏感性钉住它。

# 为什么存在

`incident-bundle.sh` 曾经用「**包版本相等**」判三态，而产品用「**协议号相等**」⇒ 同一件事两份真源：
真实现场包 `INC-20260923-123641-af29` 里写着 `Mismatch`（0.8.35 vs 0.8.36），
但两个二进制都报 `(protocol 1)` ⇒ 按产品口径应当是 `Match`，于是 `helper-mismatch` 信号被误触发。
细节与敏感性证据：`docs/verification/HELPER-TRISTATE-CALIBER.md`。
"""

import pathlib
import re
import sys

NAME = "xraytun-helper"
PROTOCOL_RE = re.compile(r"\(protocol\s+(\d+)\)")


def parse_probe(text):
    """解析 `<binary> version` 的输出，例如 `xraytun-helper 0.8.35 (protocol 1)`。

    返回 `{"version": "0.8.35", "protocol": 1}`；`protocol` 读不到时为 `None`。
    输出不是这个形状（老版本没有该子命令、被改过、空）⇒ 返回 `None`（= 读不到）。
    """
    if not text:
        return None
    parts = text.split()
    if len(parts) < 2 or parts[0] != NAME or not parts[1][:1].isdigit():
        return None
    m = PROTOCOL_RE.search(text)
    return {"version": parts[1], "protocol": int(m.group(1)) if m else None}


def classify(installed, bundled):
    """三态判定（与 `helper.rs` 同构）。

    `installed` / `bundled` 是 [`parse_probe`] 的结果（或 `None` = 读不到）。
    返回的 dict 里：
    * `state`：`Match` / `Mismatch` / `Unreadable`；
    * `criterion`：本次用的是 `protocol` 还是退化的 `package_version_fallback`；
    * `state_by_product_rule`：与 `state` 相同（**给旧包留的**：triage 复算时用它区别于旧字段）。
    """
    if not installed or not bundled:
        return {
            "state": "Unreadable",
            "criterion": "unreadable",
            "installed": (installed or {}).get("version"),
            "bundled": (bundled or {}).get("version"),
            "installed_protocol": (installed or {}).get("protocol"),
            "bundled_protocol": (bundled or {}).get("protocol"),
            "state_by_product_rule": "Unreadable",
            "reason": "两边都读到才能比 —— 读不到不许猜成不一致",
        }
    ip, bp = installed.get("protocol"), bundled.get("protocol")
    if ip is not None and bp is not None:
        criterion = "protocol"
        state = "Match" if ip == bp else "Mismatch"
    else:
        criterion = "package_version_fallback（协议号任一边读不到）"
        state = "Match" if installed["version"] == bundled["version"] else "Mismatch"
    out = {
        "state": state,
        "criterion": criterion,
        "installed": installed["version"],
        "bundled": bundled["version"],
        "installed_protocol": ip,
        "bundled_protocol": bp,
        "state_by_product_rule": state,
    }
    if state == "Match":
        # 向后兼容：旧格式在 Match 时写的是 {"state","version"}
        out["version"] = installed["version"]
    return out


def classify_from_outputs(installed_text, bundled_text):
    """便捷入口：直接吃两份 `<binary> version` 的原始输出。"""
    return classify(parse_probe(installed_text), parse_probe(bundled_text))


def _ck(fails, name, got, want):
    ok = got == want
    print(("  ✓ " if ok else "  ✗ ") + f"{name}  (got={got!r}, want={want!r})")
    if not ok:
        fails.append(name)


FIXTURE_ENV = "HELPER_TRISTATE_FIXTURE"


def fixture_path():
    """共享夹具：Python 与 Rust（权威）读**同一份**。可用环境变量覆盖（测试/敏感性用）。"""
    import os

    override = os.environ.get(FIXTURE_ENV)
    if override:
        return pathlib.Path(override)
    return pathlib.Path(__file__).resolve().parent / "fixtures" / "helper-version-cases.json"


def _expected_reason(state, ip, bp):
    """夹具里的 `expect_reason` 口径（Rust 侧测试用同一套映射，见 helper.rs 的 task-173 用例）。"""
    if state == "Unreadable":
        return "unreadable"
    if ip is not None and bp is not None:
        return "protocol_equal" if state == "Match" else "protocol_differ"
    return "fallback_equal" if state == "Match" else "fallback_differ"


def _run_fixture_cases(fails):
    import json

    path = fixture_path()
    if not path.is_file():
        print(f"  ✗ 读不到共享夹具 {path} ⇒ 夹具是共同真源，缺了必须红")
        fails.append("fixture-missing")
        return 0
    cases = json.loads(path.read_text(encoding="utf-8"))
    print(f"=== 共享夹具：{path.name}（{len(cases)} 条；Rust 侧读同一份）===")
    for c in cases:
        name = c["name"]
        inst, bund = parse_probe(c.get("installed_out", "")), parse_probe(c.get("bundled_out", ""))
        verdict = classify(inst, bund)
        got = verdict["state"]
        reason = _expected_reason(got, (inst or {}).get("protocol"), (bund or {}).get("protocol"))
        _ck(fails, f"[夹具] {name} ⇒ state", got, c["expect_state"])
        _ck(fails, f"[夹具] {name} ⇒ reason", reason, c["expect_reason"])
    return len(cases)


def self_test():
    fails = []
    n = _run_fixture_cases(fails)
    print(f"（以上 {n} 条来自共享夹具；本文件不再内联一份）")

    print("=== 双向敏感性（改坏判据/解析 ⇒ 上面必须红）===")
    p1 = "xraytun-helper 0.8.35 (protocol 1)"
    p2 = "xraytun-helper 0.8.36 (protocol 1)"

    # (a) 把判据改回「包版本相等」（旧脚本的写法）
    def old_rule(i_text, b_text):
        i, b = parse_probe(i_text), parse_probe(b_text)
        if i and b and i["version"] == b["version"]:
            return "Match"
        return "Mismatch"

    _ck(fails, "改回包版本判据 ⇒ 夹具第 1 条会变 Mismatch（原断言会红）", old_rule(p1, p2), "Mismatch")
    # (b) 把协议号解析改坏（永远读不到）⇒ 第 1 条退化成包版本比较 ⇒ 同样红
    def broken_parse(text):
        p = parse_probe(text)
        if p:
            p["protocol"] = None
        return p

    _ck(fails, "协议号解析改坏 ⇒ 夹具第 1 条会变 Mismatch（原断言会红）",
        classify(broken_parse(p1), broken_parse(p2))["state"], "Mismatch")
    _ck(fails, "（对照）解析改坏后 版本相同 仍 Match",
        classify(broken_parse("xraytun-helper 0.8.35"), broken_parse("xraytun-helper 0.8.35"))["state"], "Match")

    print()
    if fails:
        print(f"helper_tristate self-test：**失败**（{len(fails)} 项）：{fails}")
        return 1
    print("helper_tristate self-test：**全部通过**（读共享夹具 + 双向敏感性）")
    return 0


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    print(__doc__)
    sys.exit(0)
