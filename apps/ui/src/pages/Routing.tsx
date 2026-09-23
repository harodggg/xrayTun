/**
 * 规则页。
 *
 * # 这一版改了什么
 *
 * 1. **预设变成真正的单选控件。** 原来四行都是普通列表行，靠一个绿点表示
 *    「当前是这条」—— 看起来像在陈列信息，而不是在让你做选择。现在用
 *    radio + 明确的选中态，点一下会切换设置这件事变得可预期。
 * 2. **三张卡片 -> 三段带标题的内容。** 去掉装饰性边框，用留白与字阶分组
 *    （与仪表盘、设置页同一套语言）。
 * 3. **「配置落盘位置」默认折叠。** 它是排障时才需要的信息（配置路径、
 *    复现命令），不该和「分流怎么走」抢注意力。
 * 4. 复现命令做成可复制的代码块 —— 原样是一条会折行的长文本，实际没法直接用。
 *
 * # task-117：把「按节点分流」做成界面能力（原来是「请自己改 JSON」）
 *
 * 数据模型一直是支持的：`RuleAction::Proxy { outbound: Option<String> }`
 * （`crates/xt-core/src/routing/mod.rs`）里 `Some(tag)` 会**直接**变成 Xray 的
 * `outboundTag`；而 `xray/config.rs` 的 `build_outbounds` 给**每个节点**都建了出站，
 * tag 是 `node-<id>`（`model.rs` 的 `Node::outbound_tag()`）。
 * ⇒「Gemini 走美国、其余走香港」在配置层完全可行，**缺的只是入口**。
 *
 * 所以这一版补上：**规则编辑器**（增 / 删 / 改 / 上下移动）、**指定出站选择器**
 * （写 `{"kind":"proxy","outbound":"node-<id>"}`）、以及**两条必须说清的提示**：
 *
 * * **顺序即优先级**：Xray 自上而下取**第一条命中**，所以界面上把序号摆出来，
 *   并提供上下移动 —— 顺序不是装饰。
 * * **预设会遮蔽自定义规则**：`xray/config.rs` 的 `merge_rules` 是
 *   「预设在前、自定义在后」（`routing_preset == custom` 时才只用自定义）。
 *   于是**保留预设时，自定义规则排在预设之后**，对预设已命中的域名（如
 *   `geosite:google`）**永远不会命中** —— 用户今天正是差点掉进这个坑，
 *   所以界面必须显式警告（见下方 `presetShadowsCustom`）。
 */

import { useEffect, useState } from "react";
import { api } from "../ipc";
import { useStore } from "../store";
import {
  PRESET_LABEL,
  ruleActionLabel,
  type MatchCondition,
  type Node,
  type RoutingPreset,
  type RoutingRule,
  type RuleAction,
} from "../types";

/** 预设的说明。顺序即内置规则的执行顺序，不要随意调换（见下方说明）。 */
const PRESETS: Array<{ id: RoutingPreset; desc: string }> = [
  { id: "bypass_mainland", desc: "大陆域名与 IP 直连，其余走代理。日常使用推荐。" },
  { id: "global_proxy", desc: "所有流量走代理，不做任何分流。" },
  { id: "whitelist_proxy", desc: "只有规则列表内的流量走代理，其余直连。" },
  { id: "direct_all", desc: "全部直连。用于排查「是代理的问题还是网络本身的问题」。" },
  { id: "custom", desc: "只用下面你自己写的规则（内置的私有地址/广告/大陆直连都不再生效）。" },
];

/** 动作选择器里的四档。前三档是既有语义，第四档（指定节点）是 task-117 的核心。 */
type ActionChoice = "direct" | "block" | "proxy_current" | "proxy_node";

function choiceOf(action: RuleAction): ActionChoice {
  if (action.kind === "direct") return "direct";
  if (action.kind === "block") return "block";
  return action.outbound ? "proxy_node" : "proxy_current";
}

/**
 * App **自己**给规则 id 用的前缀（task-167）。
 *
 * ⚠️ 这是**约定**，不是「预设 id 清单」：`crates/xt-core/src/routing/mod.rs` 里所有预设规则的
 * id 都以 `preset-` 开头（`preset-private` / `preset-ads` / `preset-cn-domain` /
 * `preset-cn-ip` / `preset-proxy-google` / `preset-proxy-list` / `preset-fallback-direct` /
 * `preset-direct-all`），`crates/xt-core/src/xray/config.rs` 里所有内部规则的 tag 都以
 * `internal-` 开头（`internal-api` / `internal-fallback` / `internal-dns-hijack`）。
 * 具体**某个预设**里有哪些 id 由后端决定，前端不抄 —— 所以这里只用来提示「可能撞」，
 * 真正「确实撞了」的判据是运行中配置里的 `#N` 后缀标记（见 `duplicatedRuleIds`）。
 */
export const APP_RULE_ID_PREFIXES = ["preset-", "internal-"];

/**
 * 运行中配置里**已经撞过**的 rule id。
 *
 * 后端（`task-165`）的修法是「首次出现保持原样，重复的确定性加 `#2`/`#3`」⇒
 * 只要 tag 形如 `<base>#<n>`，就说明 `<base>` 这次配置里出现了不止一次。
 * 这是**唯一**能证明「确实重名」的真源，不需要前端知道任何预设 id。
 */
export function duplicatedRuleIds(tags: string[]): string[] {
  const out = new Set<string>();
  for (const t of tags) {
    const m = /^(.*)#\d+$/.exec(t);
    if (m && m[1]) out.add(m[1]);
  }
  return [...out];
}

/** 自定义规则里「用了 App 自有前缀」的 id：**可能**与预设/内部规则同名。 */
export function appPrefixedIds(rules: RoutingRule[]): string[] {
  return rules
    .filter((r) => APP_RULE_ID_PREFIXES.some((p) => r.id.startsWith(p)))
    .map((r) => r.id);
}

/** 出站 tag 必须与 Rust 侧一致：`model.rs` 的 `Node::outbound_tag()` = `node-<id>`。 */
export function outboundTagOf(nodeId: string): string {
  return `node-${nodeId}`;
}

/** 把界面上的多行/逗号分隔文本解析成数组（域名、IP 共用）。 */
export function parseList(text: string): string[] {
  return text
    .split(/[,\n]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** 新建一条规则：**默认「代理（当前选中的节点）」**，也就是最不容易出错的那档。 */
export function newRule(seq: number): RoutingRule {
  return {
    id: `rule-${seq}-${Math.random().toString(36).slice(2, 8)}`,
    name: "新规则",
    enabled: true,
    when: {
      domains: [],
      ip: [],
      ports: [],
      source_ip: [],
      inbound_tags: [],
      network: "both",
      process_names: [],
      protocols: [],
    },
    then: { kind: "proxy", outbound: null },
  };
}

/**
 * 上下移动一条规则 —— **顺序就是优先级**，所以这是本页最要紧的一个操作。
 * 返回新数组（不原地改）；越界时原样返回。
 */
export function moveRule(rules: RoutingRule[], index: number, delta: -1 | 1): RoutingRule[] {
  const target = index + delta;
  if (index < 0 || index >= rules.length || target < 0 || target >= rules.length) return rules;
  const next = rules.slice();
  const [item] = next.splice(index, 1);
  next.splice(target, 0, item!);
  return next;
}

/** 规则指向的出站是否还在节点列表里；返回指向的 tag（失效时）或 null（正常/不适用）。 */
export function brokenOutbound(rule: RoutingRule, validTags: Set<string>): string | null {
  if (rule.then.kind !== "proxy" || !rule.then.outbound) return null;
  return validTags.has(rule.then.outbound) ? null : rule.then.outbound;
}

/**
 * 动作的可读文案。
 *
 * `types.ts` 的 `ruleActionLabel` 对指定出站会显示 **tag**（`代理(node-xxx)`）——
 * 对用户没意义，所以这里把 tag 映射回**节点名**；映射不到就如实说「已失效」，
 * 不猜一个名字出来。（`types.ts` 不在本卡写入范围，所以就地包一层。）
 */
export function actionText(action: RuleAction, nodes: Node[]): string {
  if (action.kind !== "proxy") return ruleActionLabel(action);
  if (!action.outbound) return "代理（当前选中的节点）";
  const node = nodes.find((n) => outboundTagOf(n.id) === action.outbound);
  return node ? `代理（${node.name}）` : `代理（已失效：${action.outbound}）`;
}

export default function Routing() {
  const { snapshot, busy, run } = useStore();

  /**
   * 「保存了但还没生效」的那次改动（task-70）。
   *
   * Xray **没有配置热重载**：规则只在核心启动时读取，而 `save_settings` 不重启核心。
   * 所以「已连接时改预设/改规则」必须说出来 + 给一键重连。
   *
   * 这里记 `label` 而不是 preset：规则变更和预设变更**走的是同一条提示**，
   * 但「已保存为「X」」里的 X 应当分别是预设名 / 「自定义规则」。
   */
  const [pendingSave, setPendingSave] = useState<{ label: string; savedAt: number } | null>(null);
  /** 规则草稿：`null` = 未改过，直接用快照里的。 */
  const [draft, setDraft] = useState<RoutingRule[] | null>(null);
  /** 正在展开编辑的规则 id（同一时刻只展开一条，避免页面变成一张大表单）。 */
  const [editing, setEditing] = useState<string | null>(null);
  const [seq, setSeq] = useState(1);

  const startedAt = snapshot?.runtime.started_at_unix ?? null;

  /**
   * 运行中配置的 ruleTag 列表（task-167）。`null` = 读不到（核心没在跑 / 命令缺席）——
   * 那就**只**能靠前缀提示，并且要如实说自己没确认。失败了不打扰用户（这一页本来
   * 就有「核心没在跑时读不到拓扑」的语义），但也**不假装**读过。
   */
  const [topoTags, setTopoTags] = useState<string[] | null>(null);
  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const t = await api.routingTopology();
        if (alive) setTopoTags(t.rule.map((r) => r.tag));
      } catch {
        if (alive) setTopoTags(null);
      }
    };
    void load();
    return () => {
      alive = false;
    };
  }, []);

  // 核心**重新启动**过（新配置已生成）→ 提示自动消失。
  // 这条同时覆盖「用户在顶栏自己重连」：那种情况下「需要重连」已经是假话了。
  useEffect(() => {
    if (pendingSave && startedAt !== null && startedAt >= pendingSave.savedAt) setPendingSave(null);
  }, [startedAt, pendingSave]);

  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { settings, nodes, latency } = snapshot;
  const running = snapshot.runtime.running;
  const rules = draft ?? settings.custom_rules;
  const dirty = draft !== null;

  const validTags = new Set(nodes.map((n) => outboundTagOf(n.id)));

  /**
   * 「保留预设 + 有自定义规则」⇒ 自定义规则**排在预设之后**，对预设已命中的域名
   * （`geosite:google` 这类）永远不命中。这不是猜测，是 `xray/config.rs`
   * `merge_rules()` 的合成顺序：`preset_rules(preset)` 在前，`custom_rules` 追加在后；
   * 只有 `routing_preset == custom` 时才只用自定义规则。
   */
  const presetShadowsCustom = settings.routing_preset !== "custom" && rules.length > 0;

  const setPreset = (preset: RoutingPreset) =>
    void run("preset", async () => {
      const savedAt = Math.floor(Date.now() / 1000);
      const next = await api.saveSettings({ ...settings, routing_preset: preset });
      // 只在**确实有隧道在跑**时才记「待重连」：没连接就没有可重连的东西。
      setPendingSave(next.runtime.running ? { label: PRESET_LABEL[preset], savedAt } : null);
      return next;
    });

  /** 保存规则草稿：走与预设**同一条**保存通路（`save_settings`，整份设置回传）。 */
  const saveRules = () =>
    void run("save-rules", async () => {
      const savedAt = Math.floor(Date.now() / 1000);
      const next = await api.saveSettings({ ...settings, custom_rules: rules });
      setDraft(null);
      setPendingSave(next.runtime.running ? { label: "自定义规则", savedAt } : null);
      return next;
    });

  const patchRule = (id: string, change: Partial<RoutingRule>) => {
    setDraft(rules.map((r) => (r.id === id ? { ...r, ...change } : r)));
  };
  const patchWhen = (id: string, change: Partial<MatchCondition>) => {
    setDraft(rules.map((r) => (r.id === id ? { ...r, when: { ...r.when, ...change } } : r)));
  };
  const addRule = () => {
    const rule = newRule(seq);
    setSeq(seq + 1);
    setDraft([...rules, rule]);
    setEditing(rule.id);
  };
  const removeRule = (id: string) => {
    setDraft(rules.filter((r) => r.id !== id));
    if (editing === id) setEditing(null);
  };

  /**
   * 立即重连 = **先停再起**（task-70）。
   *
   * 不能只调 `api.start()`：`start_core` 对「已在运行」是**空操作**，
   * 光调它不会重新生成配置，等于按钮点了没用。
   */
  const reconnect = () =>
    void run("reconnect", async () => {
      await api.stop();
      return api.start();
    });

  // 提示成立的**全部条件**（都可从快照核实）：保存过、核心在跑、且启动于保存之前。
  const needsReconnect =
    pendingSave !== null && running && startedAt !== null && startedAt < pendingSave.savedAt;

  /**
   * task-167 的两组结论：
   * * `collidingIds` —— **确实**会撞：运行中配置里出现过 `<id>#n`，而这个 id 又是你自己的规则
   *   （所以那对重名里有一条属于你）；
   * * `suspiciousIds` —— 只是**可能**撞：id 用了 App 自己的前缀，但当前配置里还没看到重复标记。
   *   用户真实遇到的场景正好落在这里（预设本来是「自定义」⇒ 配置里只有你那几条，看不出重复）。
   */
  const runningDuplicates = duplicatedRuleIds(topoTags ?? []);
  const collidingIds = runningDuplicates.filter((id) => rules.some((r) => r.id === id));
  const suspiciousIds = appPrefixedIds(rules).filter((id) => !collidingIds.includes(id));

  return (
    <div className="page">
      <section className="page__sec">
        <h2 className="page__title">分流预设</h2>
        <p className="page__desc">
          Xray 的规则是「一组条件的合取 + 一个出站」，且<strong>顺序敏感</strong>
          （自上而下取第一条命中）。因此内置预设的顺序是固定的：私有地址直连 → 广告拦截 →
          大陆域名 → 大陆 IP → 兜底。把「广告拦截」放到「大陆直连」之后，广告规则就永远不会命中。
        </p>

        <div className="radio-list" role="radiogroup" aria-label="分流预设">
          {PRESETS.map((p) => {
            const active = settings.routing_preset === p.id;
            return (
              <label key={p.id} className={`radio-row${active ? " is-active" : ""}`}>
                <input
                  type="radio"
                  name="routing-preset"
                  checked={active}
                  disabled={busy !== null}
                  onChange={() => setPreset(p.id)}
                />
                <span className="radio-row__body">
                  <span className="radio-row__name">{PRESET_LABEL[p.id]}</span>
                  <span className="radio-row__desc">{p.desc}</span>
                </span>
                {active && <span className="radio-row__tag">当前</span>}
              </label>
            );
          })}
        </div>

        {/* task-167：**确实**会撞（运行中配置里已经有 `<id>#n`）—— 指名 + 说清后果。
            后端会加后缀保证**能启动**，所以这里是告警而不是拦；措辞不许夸大。 */}
        {collidingIds.length > 0 && (
          <div className="banner banner--warn" role="alert" style={{ marginTop: 12 }}>
            <span>⚠︎</span>
            <div>
              <strong>你的自定义规则与预设/内部规则重名了：</strong>
              <span className="mono"> {collidingIds.join("、")}</span>。
              这会让配置里出现两条同名 <span className="mono">ruleTag</span>
              （Xray 那条 <span className="mono">duplicate ruleTag</span> 启动失败就是它）。
              后端会给重复的加后缀（<span className="mono">#2</span>/<span className="mono">#3</span>）
              保证<strong>能启动</strong> —— 但两条同名规则<strong>都会生效</strong>，且
              <strong>预设那条在前</strong>（Xray 取第一条命中）。
              想去掉歧义：把自定义规则的 id 改成不重名的（例如
              <span className="mono"> mine-private</span>），或把预设设为「自定义」。
            </div>
          </div>
        )}

        {/* **可能**会撞：id 用了 App 自己的前缀，而当前运行中的配置里还没看到重复标记。
            这一态在「预设本来是自定义」时正是用户踩到的场景，所以必须提；但措辞是条件句，
            不假装已经确认。 */}
        {suspiciousIds.length > 0 && (
          <div className="banner banner--info" role="status" style={{ marginTop: 12 }}>
            <span>ℹ︎</span>
            <div>
              这些自定义规则的 id 用了 App 自己给预设/内部规则用的前缀：
              <span className="mono"> {suspiciousIds.join("、")}</span>。
              如果<strong>目标预设</strong>里也存在同名规则，就会出现两条同名
              <span className="mono"> ruleTag</span> —— 后端会加后缀保证<strong>能启动</strong>，
              但两条都会生效、<strong>预设在前</strong>。
              {topoTags === null
                ? "（读不到运行中的配置，所以无法确认当前是否已经撞上。）"
                : "（当前运行中的配置里没有发现重复标记。）"}
            </div>
          </div>
        )}

        {presetShadowsCustom && (
          <div className="banner banner--warn" role="alert" style={{ marginTop: 12 }}>
            <span>⚠︎</span>
            <div>
              <strong>你现在写的规则排在预设之后，对预设已命中的域名不会生效。</strong>
              当前预设是「{PRESET_LABEL[settings.routing_preset]}」，合成顺序是
              <strong>预设在前、自定义规则在后</strong>（Xray 取第一条命中）。
              例如预设里已有 <span className="mono">geosite:google → 走代理</span> 这条，
              所以你在这里写的「Google 域名 → 走某个美国节点」<strong>永远不会命中</strong>。
              <div style={{ marginTop: 6 }}>
                要让自定义规则优先，请把上面的预设改成「<strong>自定义</strong>」——
                但注意：那会<strong>同时去掉内置的「私有地址直连 / 广告拦截 / 大陆直连」</strong>，
                这些需要你自己写成规则（顺序也要自己排）。
              </div>
            </div>
          </div>
        )}

        {needsReconnect && (
          <div className="banner banner--warn" role="status" style={{ marginTop: 12 }}>
            <span>⚠︎</span>
            <div>
              <strong>已保存，但还没有生效。</strong>
              Xray <strong>不支持配置热重载</strong> —— 规则只在核心<strong>启动时</strong>读取，
              而这个核心是在你保存<strong>之前</strong>启动的，跑的仍是当时生成的那份配置
              （已保存为「{pendingSave!.label}」）。
              要让新规则生效需要重新连接；重连会短暂中断流量。
              <div style={{ marginTop: 8 }}>
                <button className="btn btn--primary" disabled={busy !== null} onClick={reconnect}>
                  {busy === "reconnect" ? "正在重连…" : "立即重连"}
                </button>
              </div>
            </div>
          </div>
        )}
      </section>

      <section className="page__sec">
        <h2 className="page__title">自定义规则</h2>
        <p className="page__desc">
          规则<strong>自上而下取第一条命中</strong> —— <strong>顺序就是优先级</strong>，
          用每行右侧的 ↑ ↓ 调整。「代理（指定节点）」可以让某类流量走某个具体节点
          （例如 Gemini 走美国、其余境外走香港）。
        </p>

        {rules.length === 0 ? (
          <div className="note">
            还没有自定义规则。点下面的「新增规则」开始；例如域名填
            <span className="mono"> gemini.google.com, aistudio.google.com </span>
            ，动作选「代理（指定节点）」再挑一个美国节点。
          </div>
        ) : (
          <div className="list">
            {rules.map((rule, i) => {
              const broken = brokenOutbound(rule, validTags);
              const open = editing === rule.id;
              const choice = choiceOf(rule.then);
              return (
                <div
                  key={rule.id}
                  className={`list__row${rule.enabled ? "" : " is-disabled"}`}
                  style={{ cursor: "default", display: "block" }}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    {/* 序号 = 优先级。摆在最左边，让「谁先命中」一眼可见。 */}
                    <span className="badge" title="优先级（自上而下取第一条命中）">
                      {i + 1}
                    </span>
                    <div className="list__main">
                      <div className="list__name">
                        {rule.name}
                        {!rule.enabled && <span className="list__meta">（已停用）</span>}
                        {collidingIds.includes(rule.id) ? (
                          <span className="badge badge--slow" title="与预设/内部规则同名的 id">
                            与预设重名
                          </span>
                        ) : suspiciousIds.includes(rule.id) ? (
                          <span className="badge badge--unknown" title="id 用了 App 自己的前缀，可能与预设同名">
                            id 可能重名
                          </span>
                        ) : null}
                      </div>
                      <div className="list__meta">
                        {describeMatch(rule.when)} → {actionText(rule.then, nodes)}
                      </div>
                    </div>
                    <button
                      className="btn btn--ghost"
                      title="上移（提高优先级）"
                      disabled={i === 0}
                      onClick={() => setDraft(moveRule(rules, i, -1))}
                    >
                      ↑
                    </button>
                    <button
                      className="btn btn--ghost"
                      title="下移（降低优先级）"
                      disabled={i === rules.length - 1}
                      onClick={() => setDraft(moveRule(rules, i, 1))}
                    >
                      ↓
                    </button>
                    <button
                      className="btn btn--ghost"
                      onClick={() => setEditing(open ? null : rule.id)}
                    >
                      {open ? "收起" : "编辑"}
                    </button>
                    <button className="btn btn--ghost" onClick={() => removeRule(rule.id)}>
                      删除
                    </button>
                  </div>

                  {broken && (
                    <div className="banner banner--warn" role="alert" style={{ marginTop: 8 }}>
                      <span>⚠︎</span>
                      <div>
                        这条规则指向的出站 <span className="mono">{broken}</span>
                        <strong>已经不在节点列表里</strong>（节点被删除或改名了），
                        所以它不会按预期生效 —— 请重新选择一个节点，或改成「代理（当前选中的节点）」。
                      </div>
                    </div>
                  )}

                  {open && (
                    <div style={{ marginTop: 10, display: "grid", gap: 10 }}>
                      <label className="row" style={{ gap: 8, fontSize: 12 }}>
                        <input
                          type="checkbox"
                          checked={rule.enabled}
                          onChange={(e) => patchRule(rule.id, { enabled: e.target.checked })}
                        />
                        启用这条规则
                      </label>

                      <div className="field">
                        <label>规则名</label>
                        <input
                          type="text"
                          value={rule.name}
                          onChange={(e) => patchRule(rule.id, { name: e.target.value })}
                        />
                      </div>

                      <div className="field">
                        <label>域名</label>
                        <input
                          type="text"
                          value={rule.when.domains.join(", ")}
                          placeholder="gemini.google.com, geosite:google"
                          onChange={(e) =>
                            patchWhen(rule.id, { domains: parseList(e.target.value) })
                          }
                        />
                        <div className="field__hint">
                          多个用逗号分隔；支持 Xray 的 <span className="mono">geosite:</span> / 前缀 / 关键字写法。
                        </div>
                      </div>

                      <div className="field">
                        <label>IP / CIDR</label>
                        <input
                          type="text"
                          value={rule.when.ip.join(", ")}
                          placeholder="geoip:cn, 10.0.0.0/8"
                          onChange={(e) => patchWhen(rule.id, { ip: parseList(e.target.value) })}
                        />
                      </div>

                      <div className="field">
                        <label>动作</label>
                        {/* `<label>` 没有 htmlFor/id，**不会**自动关联到控件 —— 加上 aria-label
                            才真的有可访问名（也是测试唯一能稳定定位它的方式）。 */}
                        <select
                          aria-label="动作"
                          value={choice}
                          onChange={(e) => {
                            const v = e.target.value as ActionChoice;
                            if (v === "direct") patchRule(rule.id, { then: { kind: "direct" } });
                            else if (v === "block") patchRule(rule.id, { then: { kind: "block" } });
                            else if (v === "proxy_current")
                              patchRule(rule.id, { then: { kind: "proxy", outbound: null } });
                            else
                              patchRule(rule.id, {
                                then: { kind: "proxy", outbound: nodes[0] ? outboundTagOf(nodes[0].id) : null },
                              });
                          }}
                        >
                          <option value="proxy_current">代理（当前选中的节点）</option>
                          <option value="proxy_node">代理（指定节点）</option>
                          <option value="direct">直连</option>
                          <option value="block">拦截</option>
                        </select>
                      </div>

                      {choice === "proxy_node" && (
                        <div className="field">
                          <label>指定节点</label>
                          <select
                            aria-label="指定节点"
                            value={rule.then.kind === "proxy" ? (rule.then.outbound ?? "") : ""}
                            onChange={(e) =>
                              patchRule(rule.id, {
                                then: { kind: "proxy", outbound: e.target.value || null },
                              })
                            }
                          >
                            {/* 节点名里通常带地区（「香港 · REALITY 01」）——那**就是**模型里的名字，
                                界面不额外推断「国家」（`Node` 没有 country 字段，猜一个就是编造）。
                                延迟取现成的测速结果 `snapshot.latency`。 */}
                            {nodes.map((n) => {
                              const tag = outboundTagOf(n.id);
                              const rtt = latency[n.id]?.server_rtt_ms;
                              return (
                                <option key={n.id} value={tag}>
                                  {n.name}
                                  {typeof rtt === "number" ? ` · ${rtt}ms` : " · 未测速"}
                                </option>
                              );
                            })}
                          </select>
                          <div className="field__hint">
                            落盘为 <span className="mono">{`{"kind":"proxy","outbound":"node-<id>"}`}</span>；
                            出站 tag 由核心按 <span className="mono">node-&lt;节点 id&gt;</span> 生成。
                          </div>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}

        <div className="row row--wrap" style={{ marginTop: 12 }}>
          <button className="btn" disabled={busy !== null} onClick={addRule}>
            新增规则
          </button>
          <button className="btn btn--primary" disabled={!dirty || busy !== null} onClick={saveRules}>
            {busy === "save-rules" ? "正在保存…" : "保存规则"}
          </button>
          {dirty && <span className="field__hint">有未保存的改动。</span>}
          {dirty && (
            <button className="btn btn--ghost" disabled={busy !== null} onClick={() => setDraft(null)}>
              放弃改动
            </button>
          )}
        </div>
      </section>

      <details className="page__details">
        <summary>配置落盘位置与复现命令</summary>
        <p className="page__desc">
          生成的 Xray 配置会完整写到磁盘，可以直接拿来单跑排障。
          这是「配置变更走重启」这个取舍带来的额外好处：现场总是可复现的。
        </p>
        <dl className="kv">
          <dt>配置文件</dt>
          <dd className="mono">{snapshot.runtime.config_path ?? "（核心未运行，配置尚未生成）"}</dd>
          <dt>复现命令</dt>
          <dd>
            <code className="code-block">
              {snapshot.core.path ?? "xray"} run -c{" "}
              {snapshot.runtime.config_path ?? "<配置路径>"}
            </code>
          </dd>
          <dt>当前预设</dt>
          <dd>{PRESET_LABEL[settings.routing_preset]}</dd>
          <dt>自定义规则</dt>
          <dd>{rules.length} 条</dd>
        </dl>
      </details>
    </div>
  );
}

function describeMatch(when: MatchCondition): string {
  const parts: string[] = [];
  if (when.domains.length) {
    parts.push(`域名 ${when.domains.slice(0, 3).join(", ")}${when.domains.length > 3 ? "…" : ""}`);
  }
  if (when.ip.length) {
    parts.push(`IP ${when.ip.slice(0, 3).join(", ")}${when.ip.length > 3 ? "…" : ""}`);
  }
  if (when.network !== "both") parts.push(when.network.toUpperCase());
  if (when.process_names.length) parts.push(`进程 ${when.process_names.join(", ")}`);
  return parts.length ? parts.join(" · ") : "匹配全部";
}
