/**
 * 意图过滤页。
 *
 * # 这一页要回答的三个问题
 *
 * 1. **现在在拦吗？** —— `draft` / `enabled` / `rules_pending_apply` 三者分开显示。
 *    把它们混成一句"已开启"是这一页最容易犯的错：演练模式也是"已开启"，
 *    但一条规则都不下发。
 * 2. **它判了什么？** —— 审计表（每条都带结论、原因、分数、**是否真的生效**）。
 *    判决必须可申诉，所以"为什么"能点开看。
 * 3. **规则什么时候生效？** —— 规则在**核心启动时**才下发，所以有一个显式的
 *    「应用（会重连一次）」按钮。**绝不自动重连**：那会打断用户所有连接。
 * 4. **内容级那一条路（MITM）现在到底走到哪一步了？** —— 三道闸门（开关+名单 /
 *    根证书已装 / 代理在跑）各自显示，而不是一句"已开启"。引导规则挂在出站/入站上，
 *    **没法热加**，所以"证书刚装好"与"核心已经按它跑"之间隔着一次重连：
 *    这一步由 `core_restart_required` 显式说出来。
 *
 * # 本版刻意不做的事
 *
 * * 不显示"拦截了多少广告"这种好看但没依据的数字 —— 我们只有
 *   "判为拦截的域名数"和"生效的规则条数"，就只显示这两个；
 * * 密钥（`api_key_ref`）只读展示：Keychain 读写还没实现，做成输入框会骗人
 *   ——用户以为填进去就能用。需要密钥的预设会被后端明确拒绝并给出"改用 Zen"的提示。
 */
import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorText } from "../ipc";
import { useStore } from "../store";
import type {
  AppSettings,
  IntentAllowAction,
  IntentAuditRecord,
  IntentExplain,
  IntentPreset,
  IntentSummary,
  MitmStatus,
} from "../types";

const PRESET_LABEL: Record<IntentPreset, string> = {
  typesafe: "TypeSafe 官方（需密钥）",
  zen: "OpenCode Zen（免密钥）",
  openrouter: "OpenRouter（需密钥）",
  vercel: "Vercel AI Gateway（需密钥）",
  custom: "自建网关",
};

const OUTCOME_LABEL: Record<IntentAuditRecord["outcome"], string> = {
  block: "拦截",
  allow: "放行",
  deferred: "拿不到答案",
};

function fmtTime(unix: number): string {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export default function Intent() {
  const { snapshot, runVoid } = useStore();
  const settings = snapshot?.settings ?? null;

  const [summary, setSummary] = useState<IntentSummary | null>(null);
  const [audit, setAudit] = useState<IntentAuditRecord[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [explain, setExplain] = useState<IntentExplain | null>(null);
  const [explainHost, setExplainHost] = useState<string | null>(null);
  const [mitm, setMitm] = useState<MitmStatus | null>(null);
  /** 编辑中的名单文本。`null` = 没在编辑（显示设置里的值）。 */
  const [domainsText, setDomainsText] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSummary(await api.intentStatus());
      setErr(null);
    } catch (e) {
      setErr(errorText(e));
    }
    // MITM 的状态单独取：它的失败**不该**把意图那半页也变成错误态。
    try {
      setMitm(await api.mitmStatus());
    } catch {
      setMitm(null);
    }
  }, []);

  const loadAudit = useCallback(async () => {
    try {
      const rows = await api.intentAudit(200);
      // 文件是 append-only（旧在前），界面要**最近的在最上面**。
      setAudit([...rows].reverse());
    } catch (e) {
      setErr(errorText(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    void loadAudit();
    // 5 秒一跳：判定节拍是 10 秒，界面比它快一点才不会显得"卡住"。
    const t = setInterval(() => {
      void refresh();
      void loadAudit();
    }, 5000);
    return () => clearInterval(t);
  }, [refresh, loadAudit]);

  /** 写设置（合并式：只改 intent 子树，不动其它字段）。 */
  const patchIntent = useCallback(
    (patch: Partial<AppSettings["intent"]>, name: string) => {
      if (!settings) return;
      const next: AppSettings = { ...settings, intent: { ...settings.intent, ...patch } };
      return runVoid(name, async () => {
        await api.saveSettings(next);
        await refresh();
      });
    },
    [settings, runVoid, refresh],
  );

  /** 写设置（合并式：只改 mitm 子树）。 */
  const patchMitm = useCallback(
    (patch: Partial<AppSettings["mitm"]>, name: string) => {
      if (!settings) return;
      const next: AppSettings = { ...settings, mitm: { ...settings.mitm, ...patch } };
      return runVoid(name, async () => {
        await api.saveSettings(next);
        await refresh();
      });
    },
    [settings, runVoid, refresh],
  );

  /** 名单文本框 → 数组：按行/空白/逗号切，去空。 */
  const parseDomains = (text: string): string[] =>
    text
      .split(/[\s,]+/)
      .map((t) => t.trim())
      .filter(Boolean);

  const allow = useCallback(
    (host: string, action: IntentAllowAction) => {
      return runVoid(`放行 ${host}`, async () => {
        setSummary(await api.intentAllow(host, action));
        await loadAudit();
      });
    },
    [runVoid, loadAudit],
  );

  const blockers = useMemo(() => {
    if (!settings) return ["设置还没加载出来"];
    const s = settings.intent;
    const out: string[] = [];
    if (!s.enabled) out.push("意图过滤未开启");
    if (s.enabled && s.preset !== "zen" && !s.api_key_ref.trim()) {
      out.push("当前预设需要 Jev API Key，但还没配 —— 想零密钥试水请改用 Zen 预设");
    }
    if (s.enabled && s.preset === "custom" && !s.custom_base_url.startsWith("https://")) {
      out.push("自建网关需要一个 https:// 地址");
    }
    if (s.enabled && !s.drill) {
      out.push("演练模式已关闭：判决会生成拦截规则（规则在核心启动时生效）");
    }
    return out;
  }, [settings]);

  if (!settings) {
    return (
      <div className="page">
        <p className="empty">等待设置…</p>
      </div>
    );
  }
  const s = settings.intent;
  const m = settings.mitm;

  return (
    <div className="page">
      <h1>意图过滤</h1>
      <p className="note">
        用 Jev 的类型化判定判断<strong>端点</strong>是不是投放/追踪基础设施，然后把它交给 Xray 的
        blackhole。域名层拦不住同域广告（X 时间线里的推广帖、YouTube 前贴片）——
        要碰内容只有 MITM 一条路，见下面那一节：它默认全关，且<strong>只对你点名的域名</strong>生效。
      </p>

      {err && <div className="banner banner--error">{err}</div>}

      {/* ---- 状态 ---- */}
      <section className="card">
        <h2>现在是什么状态</h2>
        <div className="kv">
          <span>开关</span>
          <strong>{s.enabled ? "已开启" : "未开启"}</strong>
        </div>
        <div className="kv">
          <span>演练模式</span>
          <strong>{s.drill ? "开（只记录，不下发规则）" : "关（会生成拦截规则）"}</strong>
        </div>
        <div className="kv">
          <span>引擎</span>
          <strong>
            {summary?.active ? `${summary.model} · ${summary.gateway}` : "未运行"}
          </strong>
        </div>
        <div className="kv">
          <span>生效的规则</span>
          <strong>
            拦截 {summary?.block_rules ?? 0} 条 · 放行 {summary?.allow_rules ?? 0} 条
          </strong>
        </div>
        <div className="kv">
          <span>判决缓存</span>
          <strong>
            {summary?.cache_len ?? 0} 条（其中判为拦截 {summary?.blocked ?? 0} 个域名）
          </strong>
        </div>
        <div className="kv">
          <span>问过网关</span>
          <strong>
            {summary?.gateway_calls ?? 0} 次 · 失败 {summary?.gateway_errors ?? 0} 次 · 缓存命中{" "}
            {summary?.cache_hits ?? 0} 次
          </strong>
        </div>
        {summary?.note && <p className="note">{summary.note}</p>}

        {summary?.rules_pending_apply && (
          <div className="banner banner--warn">
            判决变了但还没下发给核心（规则在核心启动时才生效）。
            <button
              className="btn btn--primary"
              onClick={() =>
                void runVoid("应用意图规则", async () => {
                  setSummary(await api.intentApply());
                })
              }
            >
              应用（会重连一次）
            </button>
          </div>
        )}
      </section>

      {/* ---- 为什么不能用 ---- */}
      {blockers.length > 0 && (
        <section className="card">
          <h2>要让它真的能拦，还差什么</h2>
          <ul className="list">
            {blockers.map((b) => (
              <li key={b}>{b}</li>
            ))}
          </ul>
        </section>
      )}

      {/* ---- 基本设置 ---- */}
      <section className="card">
        <h2>接入</h2>
        <label className="row">
          <input
            type="checkbox"
            checked={s.enabled}
            onChange={(e) => void patchIntent({ enabled: e.target.checked }, "开关意图过滤")}
          />
          启用意图过滤
        </label>
        <label className="row">
          <input
            type="checkbox"
            checked={s.drill}
            onChange={(e) => void patchIntent({ drill: e.target.checked }, "切换演练模式")}
          />
          演练模式（只记录本该拦谁，不生成拦截规则）
        </label>

        <div className="field">
          <label htmlFor="intent-preset">网关预设</label>
          <select
            id="intent-preset"
            value={s.preset}
            onChange={(e) =>
              void patchIntent({ preset: e.target.value as IntentPreset }, "切换 Jev 网关预设")
            }
          >
            {(Object.keys(PRESET_LABEL) as IntentPreset[]).map((p) => (
              <option key={p} value={p}>
                {PRESET_LABEL[p]}
              </option>
            ))}
          </select>
        </div>

        {s.preset === "custom" && (
          <div className="field">
            <label htmlFor="intent-base">自建网关地址（https://）</label>
            <input
              id="intent-base"
              value={s.custom_base_url}
              placeholder="https://gw.example"
              onChange={(e) => void patchIntent({ custom_base_url: e.target.value }, "改网关地址")}
            />
          </div>
        )}

        <div className="field">
          <label htmlFor="intent-model">模型（留空用预设默认值）</label>
          <input
            id="intent-model"
            value={s.model}
            placeholder="例如 jev-latest"
            onChange={(e) => void patchIntent({ model: e.target.value }, "改 Jev 模型")}
          />
        </div>

        <p className="note">
          <strong>密钥不在这里填。</strong>
          {s.api_key_ref
            ? `当前引用：${s.api_key_ref}（Keychain 读写还没实现，所以它暂时读不出内容）`
            : "Keychain 读写还没实现；需要密钥的预设会被明确拒绝，零密钥可先用 Zen 预设。"}
        </p>
      </section>

      {/* ---- 阈值 ---- */}
      <section className="card">
        <h2>闸门（三条件全满足才拦）</h2>
        <p className="note">
          误杀与漏拦的代价不对称：漏一个广告用户无感，误杀一个正常站点用户会立刻关掉功能。
          所以第三条不是装饰 —— 它让模型有机会说"我知道它像广告，但拦了会坏"。
        </p>
        <div className="field">
          <label htmlFor="ads-min">广告概率下限 ads_intent_min（当前 {s.thresholds.ads_intent_min}）</label>
          <input
            id="ads-min"
            type="range"
            min={0.5}
            max={1}
            step={0.01}
            value={s.thresholds.ads_intent_min}
            onChange={(e) =>
              void patchIntent(
                { thresholds: { ...s.thresholds, ads_intent_min: Number(e.target.value) } },
                "改意图阈值",
              )
            }
          />
        </div>
        <div className="field">
          <label htmlFor="risk-max">
            误杀刹车 risk_of_breakage_max（当前 {s.thresholds.risk_of_breakage_max}）
          </label>
          <input
            id="risk-max"
            type="range"
            min={0}
            max={0.9}
            step={0.01}
            value={s.thresholds.risk_of_breakage_max}
            onChange={(e) =>
              void patchIntent(
                { thresholds: { ...s.thresholds, risk_of_breakage_max: Number(e.target.value) } },
                "改误杀刹车",
              )
            }
          />
        </div>
        <div className="field">
          <label htmlFor="conf-min">
            类别置信度下限 choice_confidence_min（当前 {s.thresholds.choice_confidence_min}）
          </label>
          <input
            id="conf-min"
            type="range"
            min={0}
            max={0.99}
            step={0.01}
            value={s.thresholds.choice_confidence_min}
            onChange={(e) =>
              void patchIntent(
                { thresholds: { ...s.thresholds, choice_confidence_min: Number(e.target.value) } },
                "改类别置信度",
              )
            }
          />
        </div>
      </section>

      {/* ---- 预算 ---- */}
      <section className="card">
        <h2>预算</h2>
        <p className="note">
          按<strong>新域名</strong>计费，不是按连接：同一域名第二万条连接也是 0 成本。
          本机实测 1.6 万条连接只出现 259 个域名，所以默认值余量很大。
        </p>
        <div className="field">
          <label htmlFor="per-min">每分钟上限</label>
          <input
            id="per-min"
            type="number"
            min={0}
            value={s.per_minute}
            onChange={(e) => void patchIntent({ per_minute: Number(e.target.value) }, "改每分钟上限")}
          />
        </div>
        <div className="field">
          <label htmlFor="per-day">每天上限</label>
          <input
            id="per-day"
            type="number"
            min={0}
            value={s.per_day}
            onChange={(e) => void patchIntent({ per_day: Number(e.target.value) }, "改每天上限")}
          />
        </div>
        <button
          className="btn"
          onClick={() =>
            void runVoid("清空意图判决缓存", async () => {
              await api.intentClearCache();
              await refresh();
            })
          }
        >
          清空判决缓存
        </button>
      </section>

      {/* ---- MITM（内容级判定，可选）---- */}
      <section className="card">
        <h2>MITM（内容级判定，可选）</h2>
        <p className="note">
          域名层拦不住同域广告（时间线里的推广条目、同一站点接口里的推广位）。
          要做这件事只能在本机终结 TLS —— 代价与边界都写在下面，默认<strong>全关</strong>。
        </p>
        <p className="note note--warn">
          这是整个功能里<strong>唯一会改系统状态</strong>的部分：会把一张本地根证书装进
          系统钥匙串，并<strong>只对你点名的域名</strong>拆 TLS。本版每次启动重新生成一张，
          退出（或下次启动的过期会话回滚）会撤掉。装之前请想清楚：被拆的域名，其内容
          在本机是明文可见的。
        </p>

        {mitm?.note && <div className="banner banner--muted">{mitm.note}</div>}

        <div className="kv">
          <span>开关</span>
          <strong>{m.enabled ? "已开启" : "未开启"}</strong>
          <span>名单</span>
          <strong>{m.domains.length === 0 ? "空（不会拆任何域名）" : `${m.domains.length} 个域名`}</strong>
          <span>代理</span>
          <strong>{mitm?.running ? `在跑（127.0.0.1:${mitm.listen_port}）` : "没在跑"}</strong>
          <span>根证书指纹</span>
          <strong>{mitm?.ca_fingerprint ? <code>{mitm.ca_fingerprint}</code> : "还没生成过"}</strong>
          <span>生效</span>
          <strong>
            {mitm?.core_restart_required
              ? "要重连一次核心才会下发引导规则"
              : mitm?.active
                ? "引导规则已随核心生效"
                : "不生效（见上面的原因）"}
          </strong>
        </div>

        {mitm?.stats && (
          <p className="note">
            这一轮代理的账：接受 {mitm.stats.accepted} · 阻断 {mitm.stats.blocked} · 放行{" "}
            {mitm.stats.passed} · 裁剪生效 {mitm.stats.body_rewritten} · 裁剪未生效{" "}
            {mitm.stats.body_rewrite_declined} · 失败 {mitm.stats.failed}
            {mitm.stats.websocket_refused > 0 && (
              <>
                {" "}
                · WebSocket 被拒 {mitm.stats.websocket_refused}（本版不支持，别把这类端点放进名单）
              </>
            )}
          </p>
        )}

        <div className="field">
          <label htmlFor="mitm-enabled">启用 MITM</label>
          <input
            id="mitm-enabled"
            type="checkbox"
            checked={m.enabled}
            onChange={(e) => void patchMitm({ enabled: e.target.checked }, "改 MITM 开关")}
          />
        </div>

        <div className="field">
          <label htmlFor="mitm-domains">只拆这些域名（每行一个）</label>
          <textarea
            id="mitm-domains"
            rows={4}
            value={domainsText ?? m.domains.join("\n")}
            onChange={(e) => setDomainsText(e.target.value)}
            onBlur={() => {
              if (domainsText !== null) {
                void patchMitm({ domains: parseDomains(domainsText) }, "改 MITM 域名名单");
                setDomainsText(null);
              }
            }}
          />
        </div>

        <div className="field">
          <label htmlFor="mitm-quic">顺带拦掉这些域名的 UDP/443（QUIC 拆不了，逼它回退 TCP）</label>
          <input
            id="mitm-quic"
            type="checkbox"
            checked={m.block_quic}
            onChange={(e) => void patchMitm({ block_quic: e.target.checked }, "改 QUIC 兜底")}
          />
        </div>

        <div className="field">
          <label htmlFor="mitm-strip">响应体裁剪（删掉 JSON 里某个布尔字段为 true 的条目）</label>
          <input
            id="mitm-strip"
            type="checkbox"
            checked={m.body_strip !== null}
            onChange={(e) =>
              void patchMitm(
                { body_strip: e.target.checked ? { pointer: "", field: "" } : null },
                "改响应体裁剪开关",
              )
            }
          />
        </div>
        {m.body_strip && (
          <>
            <div className="field">
              <label htmlFor="mitm-strip-pointer">数组的 JSON 指针（例如 /data/items）</label>
              <input
                id="mitm-strip-pointer"
                value={m.body_strip.pointer}
                onChange={(e) =>
                  void patchMitm(
                    { body_strip: { ...m.body_strip!, pointer: e.target.value } },
                    "改裁剪指针",
                  )
                }
              />
            </div>
            <div className="field">
              <label htmlFor="mitm-strip-field">为 true 就删的字段名（例如 promoted）</label>
              <input
                id="mitm-strip-field"
                value={m.body_strip.field}
                onChange={(e) =>
                  void patchMitm(
                    { body_strip: { ...m.body_strip!, field: e.target.value } },
                    "改裁剪字段",
                  )
                }
              />
            </div>
            <p className="note">
              两项都填了才会生效；没填等于没配。本版<strong>只支持这一种</strong>动作
              （不做 HTML 重写、不注入脚本），而且改不动时一律原样转发。
            </p>
          </>
        )}

        <div className="row">
          <button
            className="btn"
            onClick={() =>
              void runVoid("装入本地根证书", async () => {
                setMitm(await api.mitmInstallCa());
              })
            }
          >
            装入根证书
          </button>
          <button
            className="btn"
            onClick={() =>
              void runVoid("撤掉本地根证书", async () => {
                setMitm(await api.mitmRemoveCa());
              })
            }
          >
            撤掉根证书
          </button>
          <button
            className="btn"
            onClick={() =>
              void runVoid("应用 MITM 设置", async () => {
                setMitm(await api.mitmApply());
              })
            }
          >
            应用（起/停代理）
          </button>
        </div>
        <p className="note">
          顺序是<strong>装证书 → 应用 → 重连核心</strong>：引导规则挂在核心的出站/入站上，
          没法热加，所以最后那一步必须重连一次（和上面意图规则的「应用」一样）。
          证书没装时<strong>不会</strong>下发引导规则 —— 否则那几个域名的 HTTPS 会撞上一张
          没人信的证书，那就不是过滤而是把网站搞坏。
        </p>
      </section>

      {/* ---- 审计 ---- */}
      <section className="card">
        <h2>审计（最近 200 条）</h2>
        <p className="note">
          每一条判决都可复查：结论、原因、分数、<strong>是否真的变成了规则</strong>。
          `applied=false` 表示当时在演练模式或还没下发。
        </p>
        {audit.length === 0 ? (
          <p className="empty">还没有判决记录。</p>
        ) : (
          <table className="list">
            <thead>
              <tr>
                <th>时间</th>
                <th>域名</th>
                <th>结论</th>
                <th>原因</th>
                <th>广告概率</th>
                <th>生效</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {audit.map((row, i) => (
                <tr key={`${row.ts_unix}-${row.host}-${i}`}>
                  <td className="mono">{fmtTime(row.ts_unix)}</td>
                  <td className="mono">{row.host}</td>
                  <td>
                    <span
                      className={
                        row.outcome === "block"
                          ? "badge badge--ok"
                          : row.outcome === "allow"
                            ? "badge"
                            : "badge badge--unknown"
                      }
                    >
                      {OUTCOME_LABEL[row.outcome] ?? row.outcome}
                    </span>
                  </td>
                  <td>{row.reason ?? "—"}</td>
                  <td className="mono">
                    {row.ads_intent === null ? "—" : row.ads_intent.toFixed(2)}
                  </td>
                  <td>{row.applied ? "是" : "否"}</td>
                  <td>
                    <button
                      className="btn btn--ghost"
                      onClick={() => {
                        setExplainHost(row.host);
                        void api
                          .intentExplain(row.host)
                          .then(setExplain)
                          .catch((e) => setErr(errorText(e)));
                      }}
                    >
                      为什么
                    </button>
                    <button className="btn btn--ghost" onClick={() => void allow(row.host, "direct")}>
                      放行（直连）
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {explain && (
          <div className="conn-detail">
            <h3>{explainHost} 的判决</h3>
            <div className="kv">
              <span>结论</span>
              <strong>{explain.verdict.verdict}</strong>
            </div>
            {"reason" in explain.verdict && (
              <div className="kv">
                <span>原因</span>
                <strong>{explain.verdict.reason}</strong>
              </div>
            )}
            {explain.verdict.verdict === "block" && (
              <>
                <div className="kv">
                  <span>类别</span>
                  <strong>{explain.verdict.category}</strong>
                </div>
                <div className="kv">
                  <span>广告概率 / 误杀风险</span>
                  <strong>
                    {explain.verdict.ads_intent.toFixed(2)} /{" "}
                    {explain.verdict.risk_of_breakage.toFixed(2)}
                  </strong>
                </div>
                <div className="kv">
                  <span>本次生效阈值</span>
                  <strong>{explain.verdict.effective_min.toFixed(2)}</strong>
                </div>
              </>
            )}
            <div className="kv">
              <span>判决时刻 / 命中次数</span>
              <strong>
                {fmtTime(explain.decided_at_unix)} · {explain.hits}
              </strong>
            </div>
            <button className="btn" onClick={() => setExplain(null)}>
              关闭
            </button>
          </div>
        )}
      </section>
    </div>
  );
}
