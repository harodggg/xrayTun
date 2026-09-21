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
 */

import { useEffect, useState } from "react";
import { api } from "../ipc";
import { useStore } from "../store";
import {
  PRESET_LABEL,
  ruleActionLabel,
  type MatchCondition,
  type RoutingPreset,
} from "../types";

/** 预设的说明。顺序即内置规则的执行顺序，不要随意调换（见下方说明）。 */
const PRESETS: Array<{ id: RoutingPreset; desc: string }> = [
  { id: "bypass_mainland", desc: "大陆域名与 IP 直连，其余走代理。日常使用推荐。" },
  { id: "global_proxy", desc: "所有流量走代理，不做任何分流。" },
  { id: "whitelist_proxy", desc: "只有规则列表内的流量走代理，其余直连。" },
  { id: "direct_all", desc: "全部直连。用于排查「是代理的问题还是网络本身的问题」。" },
];

export default function Routing() {
  const { snapshot, busy, run } = useStore();

  /**
   * 「保存了但还没生效」的那次改动。
   *
   * # 为什么需要这个状态（本页此前是**静默空操作**）
   *
   * Xray **没有配置热重载**：规则只在核心**启动时**读取（全仓 grep 无 reload/SIGHUP/
   * 文件监视；`crates/xt-core/src/update.rs` 之外的启动路径见 `commands/core.rs` 的
   * `start_core`）。而 `save_settings` **只持久化 + 对齐登录项，不重启核心**。
   * 所以「已连接时改分流预设」以前是：界面没有任何变化、也没有任何提示，
   * 而规则其实没生效 —— 正是用户抱怨过的「改了设置，什么都没发生」。
   *
   * 这里记下保存时刻，用它和**后端给的** `runtime.started_at_unix` 比：
   * 只要核心是在这次保存**之前**启动的，它跑的就是当时生成的那份配置。
   * 两个量都来自快照/后端，所以提示不会变成编造。
   */
  const [pendingSave, setPendingSave] = useState<{ preset: RoutingPreset; savedAt: number } | null>(
    null,
  );
  const startedAt = snapshot?.runtime.started_at_unix ?? null;

  // 核心**重新启动**过（新配置已生成）→ 提示自动消失。
  // 这条同时覆盖「用户在顶栏自己重连」：那种情况下「需要重连」已经是假话了，
  // 不能继续显示（红线：不许声称一件不需要做的事）。
  useEffect(() => {
    if (pendingSave && startedAt !== null && startedAt >= pendingSave.savedAt) setPendingSave(null);
  }, [startedAt, pendingSave]);

  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { settings } = snapshot;
  const running = snapshot.runtime.running;

  const setPreset = (preset: RoutingPreset) =>
    void run("preset", async () => {
      const savedAt = Math.floor(Date.now() / 1000);
      const next = await api.saveSettings({ ...settings, routing_preset: preset });
      // 只在**确实有隧道在跑**时才记「待重连」：没连接就没有可重连的东西，
      // 那时候新预设会在下次连接时自然生效（task-54 踩过「未运行却隐式启核」的坑）。
      setPendingSave(next.runtime.running ? { preset, savedAt } : null);
      return next;
    });

  /**
   * 立即重连 = **先停再起**。
   *
   * 不能只调 `api.start()`：`start_core` 对「已在运行」是**空操作**
   * （源码注释原文：「已经在跑就当作成功，不要报错」），
   * 光调它不会重新生成配置，等于按钮点了没用。
   */
  const reconnect = () =>
    void run("reconnect", async () => {
      await api.stop();
      return api.start();
    });

  // 提示成立的**全部条件**（都可从快照核实）：
  //   ① 用户刚刚在本页保存过预设；② 核心此刻在跑；③ 它的启动时间早于那次保存。
  const needsReconnect =
    pendingSave !== null && running && startedAt !== null && startedAt < pendingSave.savedAt;

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

        {needsReconnect && (
          <div className="banner banner--warn" role="status" style={{ marginTop: 12 }}>
            <span>⚠︎</span>
            <div>
              <strong>分流规则已保存，但还没有生效。</strong>
              Xray <strong>不支持配置热重载</strong> —— 规则只在核心<strong>启动时</strong>读取，
              而这个核心是在你保存<strong>之前</strong>启动的，跑的仍是当时生成的那份配置
              （已保存为「{PRESET_LABEL[pendingSave!.preset]}」）。
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
          自定义规则会<strong>追加在预设之后</strong>执行，优先级低于预设。需要更高优先级时，
          请先把预设改成「全局代理」或「全部直连」，再写自己的规则。
        </p>

        {settings.custom_rules.length === 0 ? (
          <div className="note">
            还没有自定义规则。界面编辑规则是下一步计划；当前可以直接改
            <span className="mono"> settings.json </span>
            里的 <span className="mono">custom_rules</span> 字段。
          </div>
        ) : (
          <div className="list">
            {settings.custom_rules.map((rule) => (
              <div
                key={rule.id}
                className={`list__row${rule.enabled ? "" : " is-disabled"}`}
                style={{ cursor: "default" }}
              >
                <div className="list__main">
                  <div className="list__name">{rule.name}</div>
                  <div className="list__meta">
                    {describeMatch(rule.when)} → {ruleActionLabel(rule.then)}
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
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
          <dd>{settings.custom_rules.length} 条</dd>
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
