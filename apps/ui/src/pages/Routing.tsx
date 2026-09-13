import { api } from "../ipc";
import { useStore } from "../store";
import {
  PRESET_LABEL,
  ruleActionLabel,
  type MatchCondition,
  type RoutingPreset,
} from "../types";

export default function Routing() {
  const { snapshot, busy, run } = useStore();
  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { settings } = snapshot;

  const setPreset = (preset: RoutingPreset) =>
    void run("preset", () => api.saveSettings({ ...settings, routing_preset: preset }));

  const presets: Array<{ id: RoutingPreset; desc: string }> = [
    { id: "bypass_mainland", desc: "大陆域名与 IP 直连，其余走代理。日常使用推荐。" },
    { id: "global_proxy", desc: "所有流量走代理，不做任何分流。" },
    { id: "whitelist_proxy", desc: "只有规则列表内的流量走代理，其余直连。" },
    { id: "direct_all", desc: "全部直连。用于排查「是代理的问题还是网络本身的问题」。" },
  ];

  return (
    <>
      <div className="card">
        <h2 className="card__title">分流预设</h2>
        <p className="card__desc">
          Xray 的规则是「一组条件的合取 + 一个出站」，且<strong>顺序敏感</strong>（自上而下取第一条命中）。
          因此内置预设的顺序是固定的：私有地址直连 → 广告拦截 → 大陆域名 → 大陆 IP → 兜底。
          把「广告拦截」放到「大陆直连」之后，广告规则就永远不会命中。
        </p>
        <div className="list">
          {presets.map((p) => (
            <div
              key={p.id}
              className={`list__row${settings.routing_preset === p.id ? " is-selected" : ""}`}
              onClick={() => (busy ? undefined : setPreset(p.id))}
            >
              <span className={`dot${settings.routing_preset === p.id ? " dot--on" : ""}`} />
              <div className="list__main">
                <div className="list__name">{PRESET_LABEL[p.id]}</div>
                <div className="list__meta">{p.desc}</div>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="card">
        <h2 className="card__title">自定义规则</h2>
        <p className="card__desc">
          自定义规则会<strong>追加在预设之后</strong>执行，优先级低于预设。需要更高优先级时，
          请先把预设改成「全局代理」或「全部直连」，再写自己的规则。
        </p>

        {settings.custom_rules.length === 0 ? (
          <div className="empty" style={{ padding: "20px 0" }}>
            还没有自定义规则。
            <br />
            <span style={{ fontSize: 11 }}>
              界面编辑规则是下一步计划；当前可以直接改
              <span className="mono">
                {" "}
                ~/Library/Application Support/com.xraytun.desktop/settings.json{" "}
              </span>
              里的 <span className="mono">custom_rules</span> 字段。
            </span>
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
      </div>

      <div className="card">
        <h2 className="card__title">配置落盘位置</h2>
        <p className="card__desc">
          生成的 Xray 配置会完整写到磁盘，可以直接拿来单跑排障。
          这是「配置变更走重启」这个取舍带来的额外好处：现场总是可复现的。
        </p>
        <dl className="kv">
          <dt>配置文件</dt>
          <dd className="mono">
            {snapshot.runtime.config_path ?? "（核心未运行，配置尚未生成）"}
          </dd>
          <dt>复现命令</dt>
          <dd className="mono">
            {snapshot.core.path ?? "xray"} run -c{" "}
            {snapshot.runtime.config_path ?? "<配置路径>"}
          </dd>
          <dt>当前预设</dt>
          <dd>{PRESET_LABEL[settings.routing_preset]}</dd>
          <dt>自定义规则</dt>
          <dd>{settings.custom_rules.length} 条</dd>
        </dl>
      </div>
    </>
  );
}

function describeMatch(when: MatchCondition): string {
  const parts: string[] = [];
  if (when.domains.length) {
    parts.push(
      `域名 ${when.domains.slice(0, 3).join(", ")}${when.domains.length > 3 ? "…" : ""}`,
    );
  }
  if (when.ip.length) {
    parts.push(`IP ${when.ip.slice(0, 3).join(", ")}${when.ip.length > 3 ? "…" : ""}`);
  }
  if (when.network !== "both") parts.push(when.network.toUpperCase());
  if (when.process_names.length) parts.push(`进程 ${when.process_names.join(", ")}`);
  return parts.length ? parts.join(" · ") : "匹配全部";
}
