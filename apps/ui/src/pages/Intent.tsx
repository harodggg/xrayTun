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
import { nextSteps } from "../failure";
import { useStore } from "../store";
import type {
  AppSettings,
  IntentAllowAction,
  IntentAuditRecord,
  IntentExplain,
  IntentPreset,
  IntentSummary,
  MitmSettings,
  MitmStatus,
} from "../types";

/**
 * 「现在会真的拦吗」这一行的**唯一判据**（task-23 D3）。
 *
 * # 它防的错误信念
 *
 * 旧写法只看 `block_rules > 0` 就答「会：当前有 N 条拦截规则」。而
 * `summary.block_rules` 是 `intent.rs::rules()` 的**当前应该生效的集合**
 * （重启 App 后从缓存里加载也会 > 0），与「核心有没有按它跑」无关：
 * 判决变了但还没点「应用（会重连一次）」时，`rules_pending_apply === true`，
 * 同屏另外两处正写着「尚未下发到核心」—— 用户会相信广告已经被拦了。
 *
 * 所以「有规则但没下发」必须是**独立一态**，且**不许**答「会」。
 *
 * 抽成导出纯函数：它是这一页最容易再犯的错，值得单测直接钉（见
 * `intentHonesty.test.tsx`）。
 */
export function intentVerdictLine(
  intent: Pick<AppSettings["intent"], "enabled" | "drill">,
  summary: IntentSummary | null,
  summaryUnknown: string,
): string {
  if (!intent.enabled) return "不会：功能开关没开";
  if (intent.drill) return "不会：演练模式只记录本来该拦谁，不下发任何拦截规则";
  if (summary === null) return `读不到：${summaryUnknown}`;
  if (summary.block_rules <= 0) {
    return "暂时不会：当前 0 条拦截规则（规则要核心重连后才生效）";
  }
  if (summary.rules_pending_apply) {
    return (
      `还不会：${summary.block_rules} 条拦截规则已就绪，但还没下发到核心` +
      " —— 点下面的「应用（会重连一次）」才生效"
    );
  }
  return `会：核心已加载 ${summary.block_rules} 条拦截规则`;
}

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

// ---------------------------------------------------------------------------
// MITM 的三道闸门（内容级判定的全部前提）
// ---------------------------------------------------------------------------

/**
 * 闸门状态只有三种，`mark` 就是**证据强度**：
 *
 * * `✓` 已确证通过；
 * * `✗` 已确证没过；
 * * `？` **读不到** —— 后端没有把这一项做成结构化字段，界面不许猜。
 *
 * 「根证书已装」这一道特别容易撒谎：`ca_fingerprint` 只说明**本会话生成过**
 * 一张证书，不说明它被系统信任（`mitm.rs` 的 `trusted` 来自
 * `ca_is_trusted(existing_fingerprint())`，**没有**出现在 `MitmStatus` 里）。
 * 所以只有两种证据能证明它过了：① 代理在跑（`mitm_apply` 在证书不被信任时
 * **拒绝启动**代理）；② 后端没给 `note`（`note` 为 null 只在「开着 + 名单非空 +
 * 已信任 + 代理在跑」这一支成立）。其余一律 `？`。
 */
export interface MitmGate {
  id: "switch-list" | "ca" | "proxy";
  mark: "✓" | "✗" | "？";
  title: string;
  detail: string;
  /** 这一道没过时可以立刻做的事；`null` = 不需要动作。 */
  next: string | null;
}

export function mitmGates(settings: MitmSettings, status: MitmStatus | null): MitmGate[] {
  const domains = settings.domains;
  const listDetail = !settings.enabled
    ? "开关没开（下面「启用 MITM」还没勾）"
    : domains.length === 0
      ? "开关开着，但名单是空的 —— 空名单不会拆任何域名"
      : `${domains.length} 个域名：${domains.slice(0, 3).join("、")}${
          domains.length > 3 ? "…" : ""
        }`;
  const switchList: MitmGate = {
    id: "switch-list",
    mark: settings.enabled && domains.length > 0 ? "✓" : "✗",
    title: "开关 + 名单",
    detail: listDetail,
    next: !settings.enabled
      ? "勾上「启用 MITM」"
      : domains.length === 0
        ? "在「只拆这些域名」里至少写一个域名"
        : null,
  };

  let ca: MitmGate;
  if (status === null) {
    ca = {
      id: "ca",
      mark: "？",
      title: "根证书已装",
      detail: "读不到 MITM 状态 —— 不知道证书装了没有（上面的读取错误是唯一线索）",
      next: "先排除上面的读取错误；点「装入根证书」可确保本会话的证书已装",
    };
  } else if (status.ca_fingerprint === null) {
    ca = {
      id: "ca",
      mark: "✗",
      title: "根证书已装",
      detail: "本会话还没生成过根证书（指纹为空）",
      next: "点「装入根证书」—— 这是唯一会改系统钥匙串的动作",
    };
  } else if (status.running) {
    ca = {
      id: "ca",
      mark: "✓",
      title: "根证书已装",
      detail: `已装（证书不被信任时后端不会启动这个代理）· 指纹 ${status.ca_fingerprint}`,
      next: null,
    };
  } else if (status.note === null) {
    ca = {
      id: "ca",
      mark: "✓",
      title: "根证书已装",
      detail: `已装 · 指纹 ${status.ca_fingerprint}`,
      next: null,
    };
  } else {
    ca = {
      id: "ca",
      mark: "？",
      title: "根证书已装",
      detail: `读不到：本会话已生成证书（指纹 ${status.ca_fingerprint}），但后端没有单独给「是否已装」这一项；上面那句是后端的原话`,
      next: "如果上面那句说证书没装，点「装入根证书」",
    };
  }

  const proxy: MitmGate =
    status === null
      ? {
          id: "proxy",
          mark: "？",
          title: "代理在跑",
          detail: "读不到 MITM 状态 —— 不知道代理在不在跑",
          next: "先排除上面的读取错误，再点「应用（起/停代理）」",
        }
      : status.running
        ? {
            id: "proxy",
            mark: "✓",
            title: "代理在跑",
            detail: `在跑：127.0.0.1:${status.listen_port}`,
            next: null,
          }
        : {
            id: "proxy",
            mark: "✗",
            title: "代理在跑",
            detail: "没在跑（没有进程在监听那个端口）",
            next: "点「应用（起/停代理）」",
          };

  return [switchList, ca, proxy];
}

/**
 * 三道闸门的**一句话总结**。
 *
 * 核心事实：**只要有一道没过，就一个域名的 TLS 都不会被拆**（`mitm_apply` 在
 * 设置不活跃或证书不被信任时直接停代理）。所以「差一道」不是「差不多能用」。
 *
 * 第二件同样重要的事：**三道闸门只管「本机准备好了没有」，不管「核心有没有在按
 * 它跑」**。所以三道全过时也必须把核心那一侧说出来（U1）：`core_steering === null`
 * ＝核心没在跑＝引导规则现在不在任何核心里＝**此刻没有域名被拆 TLS**。
 */
export function mitmGateSummary(gates: MitmGate[], status: MitmStatus | null): string {
  const passed = gates.filter((g) => g.mark === "✓").length;
  const failed = gates.filter((g) => g.mark === "✗").length;
  const unknown = gates.filter((g) => g.mark === "？").length;
  if (failed === gates.length) {
    return "三道闸门都没过 —— MITM 默认全关（开关关、名单空、证书没装）：此刻一个域名的 TLS 都不会被拆。";
  }
  if (failed > 0) {
    return `过了 ${passed} 道，还差 ${failed} 道 —— 只要有一道没过，一个域名的 TLS 都不会被拆。`;
  }
  if (unknown > 0) {
    return `过了 ${passed} 道，还有 ${unknown} 道读不到 —— 无法判断此刻是否在拆 TLS。`;
  }
  // 三道全过：还差「核心那一侧」，而它**不属于**这三道闸门。
  if (status === null) return "三道闸门全过，但读不到核心那一侧的状态。";
  if (status.core_steering === null) {
    return "三道闸门全过 —— 但核心没在跑，引导规则现在不在任何核心里：此刻没有域名被拆 TLS（先连接核心）。";
  }
  if (status.core_restart_required) {
    return "三道闸门全过 —— 但核心正在用旧配置，要重连一次核心才会真的生效。";
  }
  if (status.core_steering) {
    return "三道闸门全过，且核心已带上引导规则：名单里的域名会被拆 TLS。";
  }
  return "三道闸门全过 —— 但核心这次启动没带引导规则（证书或名单是在它启动之后才满足的，重连一次即可）。";
}

/**
 * 概率显示：拿不到值一律显示「—」。
 *
 * ⚠️ 判据必须是 `typeof === "number" && Number.isFinite`，**不能**只判 `=== null`：
 * 后端 `Option` 字段一旦省略 key（历史审计文件、旧版本写下的行），JS 拿到的是
 * `undefined`，`.toFixed()` 会抛 `undefined is not an object` ⇒ React 卸载整棵树
 * ⇒ **界面黑屏**。后端已改成始终发显式 `null`（`xt-intent/src/audit.rs`），
 * 但磁盘上的老行永远是缺 key 的，所以这里必须有。
 */
function fmtProb(v: number | null | undefined): string {
  return typeof v === "number" && Number.isFinite(v) ? v.toFixed(2) : "—";
}

export default function Intent() {
  const { snapshot, runVoid } = useStore();
  const settings = snapshot?.settings ?? null;

  const [summary, setSummary] = useState<IntentSummary | null>(null);
  /**
   * 意图引擎状态**读失败**（区别于「还没读回来」）。
   *
   * 没有它就会出现这一页最严重的一种假陈述：`intentStatus()` 挂了 ⇒ `summary`
   * 是 null ⇒ 界面照样写「生效的规则 拦截 0 条 / 引擎未运行 / 缓存 0 条」——
   * 把一次 IPC 故障说成了**三项确定事实**（而且都是「没在拦/没在跑」这种
   * 让人以为功能是坏的结论）。
   */
  const [statusFailed, setStatusFailed] = useState(false);
  const [audit, setAudit] = useState<IntentAuditRecord[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [explain, setExplain] = useState<IntentExplain | null>(null);
  const [explainHost, setExplainHost] = useState<string | null>(null);
  const [mitm, setMitm] = useState<MitmStatus | null>(null);
  /**
   * MITM 状态读失败的原因。以前这里是 `catch { setMitm(null); }` —— **静默**。
   * 于是「读不到」在界面上长得和「没在跑」一模一样（见 `mitmErr` 的消费处）。
   */
  const [mitmErr, setMitmErr] = useState<string | null>(null);
  /** 编辑中的名单文本。`null` = 没在编辑（显示设置里的值）。 */
  const [domainsText, setDomainsText] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSummary(await api.intentStatus());
      setStatusFailed(false);
      setErr(null);
    } catch (e) {
      setStatusFailed(true);
      setErr(errorText(e));
    }
    // MITM 的状态单独取：它的失败**不该**把意图那半页也变成错误态，
    // 但**必须**留下痕迹 —— 否则「读不到」会被渲染成「没在跑 / 还没生成过」。
    try {
      setMitm(await api.mitmStatus());
      setMitmErr(null);
    } catch (e) {
      setMitm(null);
      setMitmErr(errorText(e));
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
  const gates = mitmGates(m, mitm);
  const gateLine = mitmGateSummary(gates, mitm);
  const errSteps = err ? nextSteps(err) : [];
  /**
   * `summary` 缺席时的说法。**必须区分两种缺席**：读失败（不知道）与还没读回来。
   * 两者都**不许**退化成「0 条 / 未运行」。
   */
  const summaryUnknown = statusFailed ? "读不到（见上面的错误）" : "读取中…";

  return (
    <div className="page">
      <h1>意图过滤</h1>
      <p className="note">
        用 Jev 的类型化判定判断<strong>端点</strong>是不是投放/追踪基础设施，然后把它交给 Xray 的
        blackhole。域名层拦不住同域广告（X 时间线里的推广帖、YouTube 前贴片）——
        要碰内容只有 MITM 一条路，见下面那一节：它默认全关，且<strong>只对你点名的域名</strong>生效。
      </p>

      {err && (
        // task-23 D2：页面级失败也必须是 live region（读屏用户才会被告知）。
        <div className="banner banner--error" role="alert">
          <div>
            {err}
            {errSteps.length > 0 && (
              <div className="banner__steps">下一步：{errSteps.join("；")}</div>
            )}
          </div>
        </div>
      )}

      {/* ---- 状态 ---- */}
      <section className="card">
        <h2>现在是什么状态</h2>
        <div className="kv">
          <span>开关</span>
          <strong>{s.enabled ? "已开启" : "未开启"}</strong>
        </div>
        <div className="kv">
          <span>演练模式</span>
          {/* U13：开关关掉时不该再说「会生成拦截规则」—— 那时什么都没生成。 */}
          <strong>
            {!s.enabled
              ? "未启用（开关打开后才谈得上）"
              : s.drill
                ? "开（只记录，不下发规则）"
                : "关（会生成拦截规则）"}
          </strong>
        </div>
        {/* 这一个行回答用户唯一真正关心的问题：「现在到底拦不拦？」 */}
        <div className="kv">
          <span>现在会真的拦吗</span>
          <strong>{intentVerdictLine(s, summary, summaryUnknown)}</strong>
        </div>
        <div className="kv">
          <span>引擎</span>
          <strong>
            {summary ? (summary.active ? `${summary.model} · ${summary.gateway}` : "未运行") : summaryUnknown}
          </strong>
        </div>
        <div className="kv">
          {/*
            U5：这里给的是**当前规则集合**（`intent.rs::rules()` = 现在「应该」
            生效的那一份），不是「核心已经加载的规则」。标题写「生效的规则」会和
            同屏的「还没下发给核心」当场互相否定。所以在待下发时把这件事写出来。
          */}
          <span>当前规则集合</span>
          <strong>
            {summary
              ? `拦截 ${summary.block_rules} 条 · 放行 ${summary.allow_rules} 条${
                  summary.rules_pending_apply ? "（尚未下发到核心）" : ""
                }`
              : summaryUnknown}
          </strong>
        </div>
        <div className="kv">
          <span>判决缓存</span>
          <strong>
            {summary
              ? `${summary.cache_len} 条（其中判为拦截 ${summary.blocked} 个域名）`
              : summaryUnknown}
          </strong>
        </div>
        <div className="kv">
          <span>问过网关</span>
          <strong>
            {summary
              ? `${summary.gateway_calls} 次 · 失败 ${summary.gateway_errors} 次 · 缓存命中 ${summary.cache_hits} 次`
              : summaryUnknown}
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
        {/*
          U6：「闸门」在本页下方指的是 MITM 的三道前提（开关+名单 / 证书 / 代理），
          而这里讲的是三个**判定阈值**。同一个词指两件事会让用户拿滑块去修 MITM。
        */}
        <h2>命中判据（三条全满足才拦）</h2>
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
          要做这件事只能在本机终结 TLS —— 代价与边界都写在下面，默认<strong>全关</strong>：
          开关关着、名单是空的、根证书没装，你不动它，它一个域名都不会碰。
        </p>
        <p className="note note--warn">
          这是整个功能里<strong>唯一会改系统状态</strong>的部分：会把一张本地根证书装进
          系统钥匙串，并<strong>只对你点名的域名</strong>拆 TLS。本版每次启动重新生成一张，
          正常退出时（或下次启动回滚过期会话时）会撤掉。装之前请想清楚：被拆的域名，
          其内容在本机是明文可见的。
          {/*
            U9：「退出会撤掉」不是无条件事实 —— helper 忙的时候这一步会被**跳过**
            （`tray.rs:186` 的 `Err(_) => warn!("helper 正忙，跳过退出前回滚…")`），
            留到下次启动修复。这句话是用户决定装不装证书的唯一凭据，所以要说全。
          */}
          <br />
          注意：如果退出时特权助手没有应答，撤证书这一步会被<strong>跳过</strong>，
          留到下次启动自动回滚过期会话时才做。
        </p>

        {mitm?.note && <div className="banner banner--muted">{mitm.note}</div>}

        {/*
         * MITM 状态读失败以前是**静默**的（`catch {}`），于是「读不到」被下面的
         * 状态表渲染成「代理没在跑 / 证书还没生成过」—— 一次 IPC 故障看起来像两项
         * 确定事实。这里把原因和下一步都摆出来。
         */}
        {mitmErr && (
          <div className="banner banner--error" role="alert">
            <div>
              读不到 MITM 状态（下面的「代理 / 根证书 / 生效」现在都显示「读不到」）：
              {mitmErr}
              <div className="banner__steps">
                下一步：{nextSteps(mitmErr).join("；")}
              </div>
            </div>
          </div>
        )}

        {/* ---- 三道闸门：一眼看懂到底走到哪一步 ---- */}
        <h3>三道闸门（全过才会真的拆 TLS）</h3>
        <div className="note">
          {gateLine}
        </div>
        <ul className="list">
          {gates.map((g) => (
            <li key={g.id}>
              <strong>
                {g.mark} {g.title}
              </strong>
              <span> —— {g.detail}</span>
              {g.next && <div className="note">下一步：{g.next}</div>}
            </li>
          ))}
        </ul>

        <div className="kv">
          <span>开关</span>
          <strong>{m.enabled ? "已开启" : "未开启"}</strong>
          <span>名单</span>
          <strong>{m.domains.length === 0 ? "空（不会拆任何域名）" : `${m.domains.length} 个域名`}</strong>
          <span>代理</span>
          <strong>
            {mitm === null
              ? "读不到"
              : mitm.running
                ? `在跑（127.0.0.1:${mitm.listen_port}）`
                : "没在跑"}
          </strong>
          <span>根证书指纹</span>
          <strong>
            {mitm === null ? "读不到" : mitm.ca_fingerprint ? <code>{mitm.ca_fingerprint}</code> : "还没生成过"}
          </strong>
          <span>生效</span>
          <strong>
            {/*
              U1：「核心没在跑」必须自己一态。
              `core_restart_required` 在 `core_steering === null`（核心从没启动过 /
              已经停了）时后端直接给 false（`mitm.rs:252-255`），而 `mitm.active`
              只等于「开关开 + 名单非空」、**与核心无关**（`model.rs:1134-1136`）。
              所以旧的三态写法会在「装了证书 + 起了代理 + 从没点过连接」时写出
              「引导规则已随核心生效」—— 把「没发生」说成「已发生」。
            */}
            {mitm === null
              ? "读不到（MITM 状态读取失败，不知道引导规则生效了没有）"
              : mitm.core_steering === null
                ? "核心没在跑 —— 引导规则现在不在任何核心里（先连接核心）"
                : mitm.core_restart_required
                  ? "核心正在用旧配置：要重连一次核心才会下发引导规则（顶栏点「断开」再点「连接」）"
                  : mitm.core_steering
                    ? "引导规则已随核心生效"
                    : "本次核心没带引导规则（证书或名单是在它启动之后才满足的，重连一次即可）"}
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
            // U7：`mitm_ca_install` 装完信任锚之后会**自己调一次 `mitm_apply`**
            // （`commands/mitm.rs:86`），所以「应用」不是装完之后必须的第二步。
            title="会把根证书装进系统钥匙串，并顺手启动本地代理（你只剩「重连核心」这一步）"
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
            // U10：`mitm_ca_remove` 撤锚之后**必定停掉本地代理**
            // （`commands/mitm.rs:114-117`）；不说的话用户会以为代理还在跑。
            title="会同时停掉本地代理，并让核心下次重连时不再带引导规则"
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
          顺序是<strong>装证书（会顺手起代理） → 重连核心</strong>：引导规则挂在核心的
          出站/入站上，没法热加，所以最后那一步必须重连一次（和上面意图规则的「应用」一样）。
          「应用（起/停代理）」是<strong>另一件事</strong>——它只起停本地代理，不会重连核心。
          证书没装时<strong>不会</strong>下发引导规则 —— 否则那几个域名的 HTTPS 会撞上一张
          没人信的证书，那就不是过滤而是把网站搞坏。
        </p>
      </section>

      {/* ---- 审计 ---- */}
      <section className="card">
        <h2>审计（最近 200 条）</h2>
        <p className="note">
          每一条判决都可复查：结论、原因、分数、<strong>这条判决会不会构成一条拦截规则</strong>。
          {/*
            U2：这一列（`applied`）的判据是 `!drill && verdict.is_block()`
            （`crates/xt-intent/src/engine.rs:444-446`），它**只**说明「判定那一刻这条
            判决构成规则」，与「规则有没有被下发到正在跑的核心」**完全无关**。
            原来的页面注释说「applied=false 可能是还没下发」—— 那是反的：
            后端永远不会因为「还没下发」把 applied 置 false。列名「生效」会让用户
            以为这些域名当下正在被黑洞掉。
          */}
          拦截判决里的「会」= 它会成为一条拦截规则；演练模式下一律是
          <strong>不会</strong>（只记录）。<strong>「会」不代表规则已经下发到核心</strong>
          —— 规则只在核心启动时下发，所以还要看上面的「应用（会重连一次）」。
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
                {/* U2：原来的表头是「生效」——它会读成「正在被拦」。 */}
                <th>会生成规则</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {audit.map((row, i) => (
                <tr key={`${row.ts_unix}-${row.host}-${i}`}>
                  <td className="mono">{fmtTime(row.ts_unix)}</td>
                  <td className="mono">{row.host}</td>
                  <td>
                    {/*
                      U2 + U11：演练模式下的「拦截」原来是一个**绿色**徽章，
                      而绿色在这套配色里 = 已完成/受保护（`topbarStatus.ts:34-43`）。
                      它旁边那一列还写着「否」—— 同屏两句话互相否定。
                      现在：没构成规则时用中性徽章，并明说「本该拦截（演练）」。
                    */}
                    {row.outcome === "block" && !row.applied ? (
                      <span className="badge badge--unknown">本该拦截（演练）</span>
                    ) : (
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
                    )}
                  </td>
                  <td>{row.reason ?? "—"}</td>
                  <td className="mono">
                    {fmtProb(row.ads_intent)}
                  </td>
                  {/* 「没有构成规则」有两种完全不同的原因，不许混成一句。 */}
                  <td>
                    {row.applied
                      ? "会"
                      : row.outcome === "block"
                        ? "不会（演练模式）"
                        : "不会（不是拦截判决）"}
                  </td>
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
                    {fmtProb(explain.verdict.ads_intent)} /{" "}
                    {fmtProb(explain.verdict.risk_of_breakage)}
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
