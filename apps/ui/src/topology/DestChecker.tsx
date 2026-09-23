/**
 * 目的地判定：输入域名或 IP，用**真实规则 + 真实 geosite/geoip 数据**算出
 * 它会走哪条规则。这是拓扑页里唯一能确定「某个东西走哪条路」的能力。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14 步骤 C，纯搬迁、行为不变）。
 */

import { useMemo, useState } from "react";

import { api, errorText } from "../ipc";
import type { RouteExplanation } from "../types";

/**
 * 目的地判定：输入域名或 IP，用真实规则 + 真实 geosite/geoip 数据算出
 * 它会走哪条规则。这是这一页里**唯一能确定「某个东西走哪条路」**的能力。
 */
export function DestChecker({ geoAvailable }: { geoAvailable: boolean }) {
  const [dest, setDest] = useState("");
  const [result, setResult] = useState<RouteExplanation | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const run = async () => {
    if (!dest.trim()) return;
    setBusy(true);
    setErr(null);
    try {
      setResult(await api.explainDest(dest.trim()));
    } catch (e) {
      setErr(errorText(e));
      setResult(null);
    } finally {
      setBusy(false);
    }
  };

  /**
   * 判定结论（task-155）。
   *
   * 原来用 `result.rule_tag` 判「有没有命中」—— 而「命中」的**权威判据是 `rule_index`**
   * （`crates/xt-core/src/routing/explain.rs:274`：命中时 `rule_index: Some(idx)`）。
   * `rule_tag` 来自 `rule.tag`（`Option<String>`），**可以为空**：那时旧代码会把
   * 「命中了第 N 条规则」说成「**未命中任何规则 → 使用第一条出站**」——
   * 而屏幕上那个 `outbound` 其实是**那条命中规则**的出站，不是兜底，
   * 用户会据此得出错误结论（`explain.rs:283-287` 的未命中分支里 `outbound` 是空串）。
   * 所以两种情形**分开呈现**。
   */
  const verdict = useMemo(() => {
    if (!result) return null;
    if (result.rule_index === null) {
      return "未命中任何规则 → 使用第一条出站";
    }
    if (result.rule_tag) {
      return `命中规则「${result.rule_tag}」→ 出站 ${result.outbound || "（默认）"}`;
    }
    return `命中第 ${result.rule_index + 1} 条规则（后端没有给出规则名）→ 出站 ${
      result.outbound || "（默认）"
    }`;
  }, [result]);

  return (
    <section className="page__sec">
      <h2 className="page__title">某个地址会走哪条路</h2>
      <p className="page__desc">
        输入域名或 IP，用真实的规则与 geosite/geoip 数据判定它命中哪条规则。
        {geoAvailable ? (
          <>
            {" "}
            这条结论是确定的（已与真实核心对拍过）—— 但只在<strong>按 443/tcp 求值</strong>这个前提下：
            判定器固定用 <span className="mono">port: 443, network: tcp</span>
            （`commands/topology.rs:456-469`），而真实规则是端口/网络/入站一起做
            AND 匹配。所以只按端口或只按 udp 命中的规则（例如 `198.18.0.2:53 → dns-out`
            这类内部规则）不在这里的结论里。
          </>
        ) : (
          <> 当前数据目录里没有 geosite.dat / geoip.dat，域名规则无法判定。</>
        )}
      </p>
      <div className="row row--wrap">
        <input
          type="text"
          placeholder="例如 www.google.com 或 223.5.5.5"
          value={dest}
          onChange={(e) => setDest(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void run();
          }}
          style={{ flex: 1, minWidth: 220, maxWidth: 360 }}
        />
        <button className="btn btn--primary" disabled={busy || !dest.trim()} onClick={() => void run()}>
          {busy ? <span className="spin" /> : null}
          判定
        </button>
      </div>
      {err && <div className="banner banner--error"><span>✕</span><div>{err}</div></div>}
      {result && (
        <div className="verdict">
          <div className="verdict__head">{verdict}</div>
          <ul className="verdict__reasons">
            {result.reasons.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
          {result.undecidable.length > 0 && (
            <div className="verdict__unknown">
              无法判定：{result.undecidable.join("；")}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
